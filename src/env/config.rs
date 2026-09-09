//! Configuration & path resolution, resolvable **from anywhere**.
//!
//! Both roots — config and state — resolve by one precedence chain (plan §Config,
//! state & the env registry):
//!
//! > **CLI flag > direct env var (`CLINIX_*`) > XDG base var + `clinix` > default**
//!
//! Config is resolved **once** in [`crate::cli`] and carried in
//! [`crate::env::Context`], so every consumer reads the same values and the state
//! is explicit rather than a hidden global. The two roots follow the classic XDG
//! split (as cargo/git/uv do): config is user-authored and portable, state is
//! machine-generated and disposable.
//!
//! Path resolution is hand-rolled (~2 helpers) rather than pulling `etcetera`:
//! the logic is a small, well-understood XDG lookup already present in the crate,
//! and avoiding the dependency keeps the single-static-binary property. The plan
//! lists `etcetera` as an option; this is the deliberate minimal alternative.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{ClinixError, Result};

/// CLI-flag overrides for the two roots (from [`crate::cli::Cli`]). Each is the
/// highest-precedence source for its root when set.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
	/// `--config <dir>`: points directly at clinix's config dir.
	pub config_dir: Option<PathBuf>,
	/// `--state-dir <dir>`: points directly at clinix's state dir.
	pub state_dir: Option<PathBuf>,
}

/// Resolved configuration: the two clinix roots. Portable `config.toml` settings
/// (default nixpkgs channel, version-index provider, default env) are read lazily
/// when a verb first needs one; this slice (the registry) needs only the roots.
#[derive(Debug, Clone)]
pub struct Config {
	/// `$XDG_CONFIG_HOME/clinix` — portable, user-authored config.
	pub config_dir: PathBuf,
	/// `$XDG_STATE_HOME/clinix` — machine-generated, disposable state.
	pub state_dir: PathBuf,
}

impl Config {
	/// Resolve both roots by the precedence chain. Infallible: it only inspects
	/// env vars and joins paths (no I/O), so it never fails — directories are
	/// created lazily by the verbs that write into them.
	pub fn resolve(overrides: &Overrides) -> Self {
		Config {
			config_dir: resolve_root(
				overrides.config_dir.clone(),
				"CLINIX_CONFIG_DIR",
				"XDG_CONFIG_HOME",
				".config",
			),
			state_dir: resolve_root(
				overrides.state_dir.clone(),
				"CLINIX_STATE_DIR",
				"XDG_STATE_HOME",
				".local/state",
			),
		}
	}
}

/// Resolve one root: `flag` → `$CLINIX_*` (direct) → `$XDG_*`/clinix (base) →
/// `$HOME/<home_rel>/clinix` → a cwd-local fallback. A direct override
/// (`flag`/`CLINIX_*`) *is* the clinix dir; an XDG/HOME base gets `clinix`
/// appended. Empty env vars are ignored (treated as unset, per the XDG spec).
fn resolve_root(flag: Option<PathBuf>, direct_var: &str, xdg_var: &str, home_rel: &str) -> PathBuf {
	if let Some(dir) = flag {
		return dir;
	}
	if let Some(dir) = env_path(direct_var) {
		return dir;
	}
	if let Some(base) = env_path(xdg_var) {
		return base.join("clinix");
	}
	if let Some(home) = env_path("HOME") {
		return home.join(home_rel).join("clinix");
	}
	PathBuf::from(".clinix").join(home_rel)
}

/// A non-empty environment variable as a path, or `None` (unset or empty).
fn env_path(var: &str) -> Option<PathBuf> {
	std::env::var_os(var)
		.filter(|v| !v.is_empty())
		.map(PathBuf::from)
}

// ---- config.toml (portable settings, mirrors the CLI command tree) ----------
//
// See `notes/clinix/design/seed-catalog-and-config.md`. Loaded via
// [`Settings::load`], which resolves the `use` import chain: imported files are
// the base, the importing file overrides them, later `use` entries win over
// earlier, and cycles error. List fields (seed sources, ignores) **concatenate**
// with the higher-precedence file first, so a base config's sources are kept and
// collisions resolve in favor of the more-local file ("local overrides").

/// Parsed `config.toml` — portable, user-authored settings.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Settings {
	/// `use = [paths…]` — config files merged in as a base (this file overrides
	/// them). Chainable; resolved and cleared by [`Settings::load`].
	#[serde(rename = "use")]
	pub imports: Vec<PathBuf>,
	pub clinix: ClinixMeta,
	pub env: EnvSettings,
}

/// `[clinix]` — global meta.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClinixMeta {
	/// `clinix <name>` shorthand for `clinix env <name>` (default on). `None` = unset.
	pub shorthand: Option<bool>,
}

/// `[env]` — the environment scope.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EnvSettings {
	/// Override the registry (env store) location; default = `<state>/envs`.
	pub registry: Option<PathBuf>,
	/// The nixpkgs pin fragments build against (a ref clinix locks, or a lockfile).
	pub nixpkgs: Option<NixpkgsPin>,
	/// A nix file evaluating to a nixpkgs `config` attrset (e.g. an `allowUnfree`
	/// predicate), applied when building seeds. If unset, clinix passes no explicit
	/// config, so nixpkgs uses its default (`~/.config/nixpkgs/config.nix`).
	pub nixpkgs_config: Option<PathBuf>,
	pub seeds: SeedSettings,
	/// `[env.gc]` — GC-root retention policy.
	pub gc: GcSettings,
}

impl EnvSettings {
	/// How many prior root versions to keep on replacement (default `0` = release
	/// the outgoing version immediately). See [`GcSettings`].
	pub fn keep_n_prior_roots(&self) -> usize {
		self.gc.keep_n_prior_roots.unwrap_or(0)
	}
}

/// `[env.gc]` — garbage-collection root retention policy.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GcSettings {
	/// How many **prior root versions** to keep when a new version replaces the
	/// current one, *in addition* to the current version. `0` (the default) releases
	/// the outgoing version immediately, so `nix-collect-garbage` can reap its
	/// closure; a higher value keeps that many previous versions rooted (each is a
	/// `.drv` + `.rt` pair, so the recipe *and* the built packages survive — a prior
	/// stays enterable **offline** via `env shell --prior N`), pruning the oldest
	/// beyond the limit. Versions are minted only when the derivation actually
	/// changes (see `crate::env::launch`).
	pub keep_n_prior_roots: Option<usize>,
}

/// `[env.seeds]` — the seed catalog sources.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SeedSettings {
	/// Ordered sources: a directory (scanned for `*.nix`) or an exact file.
	pub sources: Vec<SeedSource>,
	/// gitignore-style globs suppressing "other files exist" warnings (also
	/// honored via a `.clinix_ignore` per source dir).
	pub ignore: Vec<String>,
	/// Default namespace for sources without their own (`{ path, namespace }`):
	/// `None` when unset **or** `false` (the two are the same — no namespace), else
	/// the validated string. Bare-name access is unaffected. See
	/// `notes/clinix/design/target-resolution.md` §5.
	#[serde(deserialize_with = "de_seed_namespace")]
	pub namespace: Option<String>,
}

/// Deserialize the seed default namespace when present: a validated string, or
/// `false` for none (identical to leaving it unset). Absence is handled by the
/// struct-level `#[serde(default)]` → `None`.
fn de_seed_namespace<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
	D: serde::Deserializer<'de>,
{
	use serde::de::Error;
	#[derive(Deserialize)]
	#[serde(untagged)]
	enum Repr {
		Name(String),
		Toggle(bool),
	}
	match Repr::deserialize(deserializer)? {
		Repr::Name(s) => {
			crate::env::naming::validate_namespace(&s).map_err(D::Error::custom)?;
			Ok(Some(s))
		}
		Repr::Toggle(false) => Ok(None),
		Repr::Toggle(true) => Err(D::Error::custom(
			"namespace = true is not valid; use a string or `false`",
		)),
	}
}

/// One seed source: a directory (scanned recursively for `*.nix`) or an exact
/// `*.nix` file. Deserializes from **either** a bare path string
/// (`"~/dev_env/shells"`) **or** the table form (`{ path = "…", namespace = "…" }`)
/// when an explicit namespace is wanted.
#[derive(Debug, Clone)]
pub struct SeedSource {
	pub path: PathBuf,
	/// Qualifier for the `namespace:name` selector; `None` when unset (explicit
	/// only — no derived default).
	pub namespace: Option<String>,
}

impl<'de> Deserialize<'de> for SeedSource {
	fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		// A bare string is the path; a table carries `path` (+ optional `namespace`).
		#[derive(Deserialize)]
		#[serde(untagged)]
		enum Repr {
			Path(PathBuf),
			Table {
				path: PathBuf,
				#[serde(default)]
				namespace: Option<String>,
			},
		}
		use serde::de::Error;
		Ok(match Repr::deserialize(deserializer)? {
			Repr::Path(path) => SeedSource {
				path,
				namespace: None,
			},
			Repr::Table { path, namespace } => {
				if let Some(ns) = &namespace {
					crate::env::naming::validate_namespace(ns).map_err(D::Error::custom)?;
				}
				SeedSource { path, namespace }
			}
		})
	}
}

/// The nixpkgs pin: a git **ref** clinix locks to a rev, or a path to an existing
/// **lockfile** (never a channel — the design's §0).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum NixpkgsPin {
	/// e.g. `nixos-26.05` — a branch/tag/rev clinix resolves and locks.
	Ref(String),
	/// `{ flake_lock = "path" }` — an existing lockfile is authoritative.
	FlakeLock { flake_lock: PathBuf },
}

/// A minimal `config.toml` scaffold — the common case (a seed source to fill in).
/// Printed by `clinix config example`.
pub const MINIMAL_TEMPLATE: &str = r#"# clinix config (TOML). Default location: ~/.config/clinix/config.toml
# Full annotated template with defaults: `clinix config example --full`

[env.seeds]
# Seed shells: a directory (scanned recursively for *.nix) or an exact *.nix file.
# A bare path string is enough; use { path = "...", namespace = "..." } for a namespace.
sources = [
  # "~/dev_env/shells",
]
"#;

/// The extensive `config.toml` template — every option, annotated, with defaults.
/// Printed by `clinix config example --full`. Kept in sync with [`Settings`] by
/// `config_templates_are_valid_settings` (it must parse).
pub const FULL_TEMPLATE: &str = r#"# clinix configuration (TOML)
#
# Location: $XDG_CONFIG_HOME/clinix/config.toml (default ~/.config/clinix/config.toml).
# Override the dir with --config or $CLINIX_CONFIG_DIR.
# Precedence for every setting: CLI flag > CLINIX_* env var > this file > built-in default.
# Values shown are clinix's defaults; commented lines are optional overrides/examples.

# Merge-import other config files as a base; this file overrides them. Chainable
# (a file that is only `use = [...]` acts as a redirect). Later entries win; cycles error.
# use = ["~/dots/clinix/base.toml"]

[clinix]
# `clinix <name>` shorthand for `clinix env <name>`. A name that collides with a
# top-level command (env, sys, info, ...) is NOT captured — use `clinix env <name>`.
shorthand = true

[env]
# Where the env registry (materialized envs + GC roots) lives.
# Default: $XDG_STATE_HOME/clinix (i.e. ~/.local/state/clinix).
# registry = "~/.local/state/clinix/envs"

# The nixpkgs pin seed fragments build against — a git ref clinix locks into
# <config>/flake.lock (never a nix-channel), or a path to an existing lockfile.
nixpkgs = "nixos-26.05"
# nixpkgs = { flake_lock = "~/dev_env/flake.lock" }

# A nix file evaluating to a nixpkgs `config` attrset applied when building seeds
# — e.g. an allowUnfree predicate (like dev_env's nixpkgs-config.nix) or CUDA.
# If unset, nixpkgs uses its default (~/.config/nixpkgs/config.nix).
# nixpkgs_config = "~/dev_env/nixpkgs-config.nix"

[env.gc]
# How many PRIOR root versions to keep when a new version replaces the current one,
# in addition to the current. 0 (default) releases the outgoing version immediately
# so `nix-collect-garbage` can reap it; a higher value keeps that many previous
# versions rooted (recipe + packages) for fast, OFFLINE switch-back via
# `clinix env shell <name> --prior N`. List versions with `clinix env roots <name>`.
# keep_n_prior_roots = 0

[env.seeds]
# Seed shells, read in place. Each source is a directory (scanned RECURSIVELY for
# *.nix, each a seed named by basename) or an exact *.nix file. A per-source
# `namespace` qualifies the `namespace:name` selector when a basename appears in
# more than one source. Enter with `clinix env rust claude` (lexical; -o keeps
# order), `clinix env <name>`, or the `clinix <name>` shorthand.
#
# Default namespace applied to sources without their own. Unset (or `false`) means
# no namespace — the two are the same. A custom string follows Rust-identifier
# rules: starts a-zA-Z, then [a-zA-Z0-9_-], `-`==`_`, no `:`. Bare-name access is
# unaffected either way.
# namespace = "seeds"
#
# Each source is either a bare path string, or a { path = "...", namespace = "..." }
# table when you want to override the default namespace for that source.
sources = [
  # "~/dev_env/shells",                                    # bare path — simplest
  # { path = "~/dots/clinix/seeds", namespace = "dots" },  # table form: set a namespace
  # "~/one-off/python.nix",                                # an exact file
]

# gitignore-ish globs that silence "other files exist" warnings (also honored via a
# .clinix_ignore in a source dir). A pattern with `/` matches the path relative to
# the source (lib/**, lib/*.nix); without `/` it matches the basename at any depth
# (_*.nix). `**` crosses directories; `*` and `?` do not.
ignore = []
"#;

impl Settings {
	/// Load `config.toml` from `config_dir`, resolving its `use` import chain. A
	/// missing top-level file yields all defaults.
	pub fn load(config_dir: &Path) -> Result<Settings> {
		load_config_file(&config_dir.join("config.toml"), &mut Vec::new())
	}

	/// Whether the `clinix <name>` shorthand is enabled (default: yes).
	pub fn shorthand(&self) -> bool {
		self.clinix.shorthand.unwrap_or(true)
	}

	/// Merge `other` (higher precedence) over `self`: scalars — `other` wins when
	/// set; lists — concatenate with `other` first (so `other` wins name
	/// collisions). `imports` is dropped (already resolved).
	fn merged_over(self, other: Settings) -> Settings {
		Settings {
			imports: Vec::new(),
			clinix: ClinixMeta {
				shorthand: other.clinix.shorthand.or(self.clinix.shorthand),
			},
			env: EnvSettings {
				registry: other.env.registry.or(self.env.registry),
				nixpkgs: other.env.nixpkgs.or(self.env.nixpkgs),
				nixpkgs_config: other.env.nixpkgs_config.or(self.env.nixpkgs_config),
				seeds: SeedSettings {
					sources: concat_first(other.env.seeds.sources, self.env.seeds.sources),
					ignore: concat_first(other.env.seeds.ignore, self.env.seeds.ignore),
					namespace: other.env.seeds.namespace.or(self.env.seeds.namespace),
				},
				gc: GcSettings {
					keep_n_prior_roots: other
						.env
						.gc
						.keep_n_prior_roots
						.or(self.env.gc.keep_n_prior_roots),
				},
			},
		}
	}
}

/// Concatenate `high` then `low` — higher precedence first.
fn concat_first<T>(high: Vec<T>, low: Vec<T>) -> Vec<T> {
	let mut v = high;
	v.extend(low);
	v
}

/// Load one config file and its `use` chain, merged. `stack` holds the canonical
/// paths being loaded, for cycle detection.
fn load_config_file(path: &Path, stack: &mut Vec<PathBuf>) -> Result<Settings> {
	if !path.exists() {
		// A missing top-level config is fine (all defaults). A missing *imported*
		// file is caught before recursion, with the importer named.
		return Ok(Settings::default());
	}
	let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
	if stack.contains(&canon) {
		return Err(ClinixError::Config(format!(
			"`use` import cycle involving {}",
			path.display()
		)));
	}
	let text = fs::read_to_string(path)
		.map_err(|e| ClinixError::Config(format!("{}: {e}", path.display())))?;
	let this: Settings = toml::from_str(&text)
		.map_err(|e| ClinixError::Config(format!("{}: {e}", path.display())))?;

	stack.push(canon);
	// Imports are the base, merged left→right (later `use` wins); the local file
	// then overrides all of them.
	let mut base = Settings::default();
	for import in &this.imports {
		let import_path = resolve_import(path, import);
		if !import_path.exists() {
			stack.pop();
			return Err(ClinixError::Config(format!(
				"{}: `use` target not found: {}",
				path.display(),
				import_path.display()
			)));
		}
		let imported = load_config_file(&import_path, stack)?;
		base = base.merged_over(imported); // later `use` wins over earlier
	}
	let local = Settings {
		imports: Vec::new(),
		..this
	};
	let merged = base.merged_over(local);
	stack.pop();
	Ok(merged)
}

/// Resolve a `use` target: `~` → `$HOME`; a relative path is relative to the
/// importing file's directory; absolute paths are used as-is.
fn resolve_import(from: &Path, target: &Path) -> PathBuf {
	let expanded = expand_tilde(target);
	if expanded.is_absolute() {
		expanded
	} else {
		from.parent()
			.unwrap_or_else(|| Path::new("."))
			.join(expanded)
	}
}

/// Expand a leading `~` to `$HOME`. Config paths are user-authored and commonly
/// use `~`, which TOML strings do not expand.
pub fn expand_tilde(path: &Path) -> PathBuf {
	if let Ok(rest) = path.strip_prefix("~") {
		if let Some(home) = std::env::var_os("HOME") {
			return PathBuf::from(home).join(rest);
		}
	}
	path.to_path_buf()
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::Path;

	// A CLI flag is the top of the precedence chain and needs no env, so this is
	// deterministic. The XDG-var and default layers mutate process-global env, so
	// they are exercised by the L3 integration tests (which set `XDG_STATE_HOME`
	// per test) instead — see `notes/clinix/design/testing.md` §2.
	#[test]
	fn flag_overrides_everything() {
		let cfg = Config::resolve(&Overrides {
			config_dir: Some(PathBuf::from("/tmp/explicit-config")),
			state_dir: Some(PathBuf::from("/tmp/explicit-state")),
		});
		assert_eq!(cfg.config_dir, Path::new("/tmp/explicit-config"));
		assert_eq!(cfg.state_dir, Path::new("/tmp/explicit-state"));
	}

	// ---- config.toml parsing / use-merge -----------------------------------

	fn write(dir: &Path, name: &str, body: &str) {
		std::fs::write(dir.join(name), body).unwrap();
	}

	#[test]
	fn settings_parse_reads_env_and_seeds() {
		let dir = tempfile::tempdir().unwrap();
		write(
			dir.path(),
			"config.toml",
			r#"
[clinix]
shorthand = false
[env]
nixpkgs = "nixos-26.05"
[env.seeds]
sources = [ { path = "~/dev_env/shells", namespace = "dev" }, { path = "/abs/one.nix" } ]
ignore = ["_*.nix"]
"#,
		);
		let s = Settings::load(dir.path()).unwrap();
		assert!(!s.shorthand());
		assert!(matches!(s.env.nixpkgs, Some(NixpkgsPin::Ref(ref r)) if r == "nixos-26.05"));
		assert_eq!(s.env.seeds.sources.len(), 2);
		assert_eq!(s.env.seeds.sources[0].namespace.as_deref(), Some("dev"));
		assert_eq!(s.env.seeds.ignore, vec!["_*.nix".to_string()]);
	}

	#[test]
	fn seed_source_accepts_bare_string_or_table() {
		let dir = tempfile::tempdir().unwrap();
		write(
			dir.path(),
			"config.toml",
			r#"
[env.seeds]
sources = [ "~/a/shells", { path = "~/b/seeds", namespace = "bee" }, "/abs/one.nix" ]
"#,
		);
		let s = Settings::load(dir.path()).unwrap();
		assert_eq!(s.env.seeds.sources.len(), 3);
		// bare string → path, no namespace.
		assert_eq!(s.env.seeds.sources[0].path, PathBuf::from("~/a/shells"));
		assert!(s.env.seeds.sources[0].namespace.is_none());
		// table → path + namespace.
		assert_eq!(s.env.seeds.sources[1].namespace.as_deref(), Some("bee"));
		// bare string works mid-array too.
		assert!(s.env.seeds.sources[2].namespace.is_none());
	}

	#[test]
	fn settings_missing_file_is_all_defaults() {
		let dir = tempfile::tempdir().unwrap();
		let s = Settings::load(dir.path()).unwrap();
		assert!(s.shorthand(), "shorthand defaults on");
		assert!(s.env.seeds.sources.is_empty());
		// The seed default namespace is unset (None) when nothing is configured.
		assert_eq!(s.env.seeds.namespace, None);
	}

	#[test]
	fn seed_default_namespace_unset_false_or_custom() {
		let load = |body: &str| {
			let dir = tempfile::tempdir().unwrap();
			write(dir.path(), "config.toml", body);
			Settings::load(dir.path())
		};
		// Absent and `false` are the same — None (no namespace).
		assert_eq!(
			load("[env.seeds]\nsources = []\n")
				.unwrap()
				.env
				.seeds
				.namespace,
			None
		);
		assert_eq!(
			load("[env.seeds]\nnamespace = false\n")
				.unwrap()
				.env
				.seeds
				.namespace,
			None
		);
		// A custom string is validated and used.
		assert_eq!(
			load("[env.seeds]\nnamespace = \"mine\"\n")
				.unwrap()
				.env
				.seeds
				.namespace
				.as_deref(),
			Some("mine")
		);
		// An invalid namespace (leading digit / `:`) is a load error.
		assert!(load("[env.seeds]\nnamespace = \"1bad\"\n").is_err());
		assert!(load("[env.seeds]\nnamespace = \"a:b\"\n").is_err());
	}

	#[test]
	fn use_merges_base_local_overrides_scalars_and_concats_lists_local_first() {
		let dir = tempfile::tempdir().unwrap();
		write(
			dir.path(),
			"base.toml",
			r#"
[clinix]
shorthand = true
[env]
nixpkgs = "nixos-26.05"
[env.seeds]
sources = [ { path = "/base/a" } ]
"#,
		);
		write(
			dir.path(),
			"config.toml",
			r#"
use = ["base.toml"]
[clinix]
shorthand = false
[env.seeds]
sources = [ { path = "/local/b" } ]
"#,
		);
		let s = Settings::load(dir.path()).unwrap();
		assert!(!s.shorthand(), "local scalar overrides base");
		assert!(
			matches!(s.env.nixpkgs, Some(NixpkgsPin::Ref(ref r)) if r == "nixos-26.05"),
			"unset-in-local scalar is inherited from base"
		);
		let paths: Vec<String> = s
			.env
			.seeds
			.sources
			.iter()
			.map(|x| x.path.to_string_lossy().into_owned())
			.collect();
		assert_eq!(paths, vec!["/local/b".to_string(), "/base/a".to_string()]);
	}

	#[test]
	fn use_merge_namespace_unset_and_false_both_inherit_base() {
		let dir = tempfile::tempdir().unwrap();
		write(
			dir.path(),
			"base.toml",
			"[env.seeds]\nnamespace = \"base\"\nsources = []\n",
		);
		// unset and `false` are the same (both None), so both inherit the base via
		// `.or()`. A local string is what overrides.
		for local in [
			"use = [\"base.toml\"]\n[env.seeds]\nsources = []\n",
			"use = [\"base.toml\"]\n[env.seeds]\nnamespace = false\nsources = []\n",
		] {
			write(dir.path(), "config.toml", local);
			assert_eq!(
				Settings::load(dir.path())
					.unwrap()
					.env
					.seeds
					.namespace
					.as_deref(),
				Some("base"),
				"None (unset/false) inherits base"
			);
		}
		write(
			dir.path(),
			"config.toml",
			"use = [\"base.toml\"]\n[env.seeds]\nnamespace = \"local\"\nsources = []\n",
		);
		assert_eq!(
			Settings::load(dir.path())
				.unwrap()
				.env
				.seeds
				.namespace
				.as_deref(),
			Some("local"),
			"a local string overrides base"
		);
	}

	#[test]
	fn use_cycle_is_an_error() {
		let dir = tempfile::tempdir().unwrap();
		write(dir.path(), "config.toml", "use = [\"a.toml\"]\n");
		write(dir.path(), "a.toml", "use = [\"config.toml\"]\n");
		assert!(matches!(
			Settings::load(dir.path()),
			Err(ClinixError::Config(_))
		));
	}

	#[test]
	fn use_missing_target_is_an_error() {
		let dir = tempfile::tempdir().unwrap();
		write(dir.path(), "config.toml", "use = [\"nope.toml\"]\n");
		assert!(Settings::load(dir.path()).is_err());
	}

	#[test]
	fn config_templates_are_valid_settings() {
		// Both templates must parse against the current schema (guards drift).
		let _: Settings = toml::from_str(MINIMAL_TEMPLATE).expect("minimal template parses");
		let full: Settings = toml::from_str(FULL_TEMPLATE).expect("full template parses");
		// The full template documents the real defaults.
		assert!(full.shorthand());
		assert!(matches!(full.env.nixpkgs, Some(NixpkgsPin::Ref(ref r)) if r == "nixos-26.05"));
		// Unedited templates are warning-free: no live sources.
		assert!(full.env.seeds.sources.is_empty());
	}

	#[test]
	fn expand_tilde_expands_leading_tilde_only() {
		if let Some(home) = std::env::var_os("HOME") {
			assert_eq!(
				expand_tilde(Path::new("~/x")),
				PathBuf::from(home).join("x")
			);
		}
		assert_eq!(expand_tilde(Path::new("/abs")), Path::new("/abs"));
		assert_eq!(expand_tilde(Path::new("rel/x")), Path::new("rel/x"));
	}
}
