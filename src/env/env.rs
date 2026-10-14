//! Shared `env` primitives: the verb dispatch ([`Cmd`]), the [`RunCmd`] trait,
//! [`Context`], env resolution ([`resolve`]/[`Env`]/[`Kind`]/[`resolve_node`]),
//! the generic env-name selectors, and the `rename` verb. Each concrete verb
//! lives in its own sibling submodule; this file dispatches to them.
//!
//! Resolution only — *combining* resolved targets into one instantiable
//! expression, and entering it, is [`super::compose`]. Keeping the two apart is
//! why a change to how environments compose no longer has to touch the file that
//! also owns `Context`, `Env`, and the verb table.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};

use crate::env::config::Config;
use crate::env::registry;
use crate::error::{ClinixError, Result};

use super::diagnostics::{Deps, Info};
use super::execution::{Run, Shell};
use super::export::Export;
use super::import::Import;
use super::init::Init;
use super::new::New;
use super::pkgs::{self, Flake, Pkgs, Update};
use super::{clean, diagnostics};

/// An environment type resolved to its root directory
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
	/// A named, reusable, composable env under the clinix state dir.
	Registry,
	/// A project directory (given by path, or the cwd when no name is given).
	Project,
}

/// Composition options read from the global flags, shared by every verb and by
/// the bare-name sugar. See [`crate::cli`].
#[derive(Debug, Clone, Default)]
pub struct ShellOptions {
	/// Preserve the given order instead of sorting names lexically.
	pub ordered: bool,
	/// In a project, compose only the runtime env (skip dev tools). Threaded from
	/// the CLI but **not yet read by any verb**: implicit project composition
	/// (`compose.nix` — runtime ∪ dev shells) is deferred, so a project already
	/// composes alone. Wire this up when that composition lands.
	pub runtime: bool,
	/// Ad-hoc `-p`/`--pkg` package attribute names, already validated once in
	/// [`crate::cli`] (see [`crate::env::PkgArgs::names`]).
	///
	/// They live here, on the shared request context, rather than on each verb's
	/// own args: the ad-hoc set is part of what a composition *is*, so
	/// [`compose_nodes`] reads it directly instead of every caller threading it
	/// through. Empty for every non-`-p` invocation.
	pub packages: Vec<String>,
}

/// Shared execution context: the resolved [`Config`] (config/state roots) plus
/// the composition options. Built once in [`crate::cli`] and threaded to every
/// verb, so path resolution is explicit rather than a hidden global.
pub struct Context {
	pub options: ShellOptions,
	pub config: Config,
}

pub trait RunCmd {
	fn run(self, context: &Context) -> Result<()>;
}

/// A resolved environment: a root dir plus how we found it.
#[derive(Debug, Clone)]
pub struct Env {
	/// `None` for an anonymous cwd project; otherwise the resolved name.
	pub name: Option<String>,
	/// Directory containing `shell.nix` / `flake.lock`.
	pub root: PathBuf,
	pub kind: Kind,
}

impl Env {
	/// The env's `shell.nix`, or a uniform "not a clinix env" error naming the
	/// directory and the `init` fix. For the verbs that must instantiate `shell.nix`
	/// specifically (`add`/`remove`, `deps`, `shared`). Distinct from
	/// [`env_shell_file`], which also accepts `default.nix` (the launcher's lookup).
	pub fn require_shell_nix(&self) -> Result<PathBuf> {
		let shell_nix = self.root.join("shell.nix");
		if shell_nix.is_file() {
			Ok(shell_nix)
		} else {
			Err(ClinixError::Resolve(format!(
				"no shell.nix at {} (run: clinix env init)",
				self.root.display()
			)))
		}
	}
}

/// The one per-source-divergent seam (plan §resolve). No nix eval, no lock read —
/// pure name → root-dir resolution against the resolved [`Config`]. Resolves the
/// *directory-backed* target shapes (the launcher's [`resolve_node`] handles the
/// `*.nix` **File** shape and the seed catalog before delegating here):
/// - `Some(".")` or `None` → the cwd project ([`Kind::Project`]);
/// - a **Dir** (`/`-bearing or an existing dir) → [`Kind::Project`] at that path;
/// - a **Member** `namespace:member` → the registry env's member subshell at
///   `envs/<namespace>/<member>` ([`Kind::Registry`]); absent → [`ClinixError::UnknownEnv`];
/// - a **bare name** → a registry env at `envs/<name>` if it exists
///   ([`Kind::Registry`]); else [`ClinixError::UnknownEnv`].
///
/// The registry *layout* (`envs/<name>`, members `envs/<name>/<member>`) lives in
/// [`crate::env::registry`]; this only resolves a name to a root. Enumeration is
/// [`registry::list_names`].
pub fn resolve(cfg: &Config, name: Option<&str>) -> Result<Env> {
	match name {
		None | Some(".") => Ok(Env {
			name: None,
			root: std::env::current_dir()?,
			kind: Kind::Project,
		}),
		Some(n) => {
			let path = Path::new(n);
			// Dir target: a `/`-bearing path or an existing dir → a project there.
			if n.contains('/') || path.is_dir() {
				return Ok(Env {
					name: Some(n.to_string()),
					root: std::fs::canonicalize(path)?,
					kind: Kind::Project,
				});
			}
			// Member target: `namespace:member` → a registry env's member subshell
			// at `envs/<namespace>/<member>` (members are subdirs sharing the parent
			// env's pin). If that dir is absent, fall through to `UnknownEnv` so the
			// caller can try the seed catalog (a seed `namespace:name`). Both halves
			// must be valid single components (guards against `..` traversal).
			if let Some((ns, member)) = n.split_once(':') {
				if registry::validate_name(ns).is_ok() && registry::validate_name(member).is_ok() {
					let root = registry::env_root(cfg, ns).join(member);
					if root.is_dir() {
						return Ok(Env {
							name: Some(n.to_string()),
							root,
							kind: Kind::Registry,
						});
					}
				}
				return Err(ClinixError::UnknownEnv(n.to_string()));
			}
			// Bare name: a registry env at `envs/<name>` (its default/runtime shell).
			let root = registry::env_root(cfg, n);
			if root.is_dir() {
				Ok(Env {
					name: Some(n.to_string()),
					root,
					kind: Kind::Registry,
				})
			} else {
				Err(ClinixError::UnknownEnv(n.to_string()))
			}
		}
	}
}

/// Warn (never error) when creating an env named `name` would collide, under the
/// namespace normalization (`-`≡`_`), with an existing **registry env** or a
/// **seed** — since `resolve_node` resolves a registry env before a seed, a new
/// registry env silently shadows a same-named seed. Best-effort: a config/registry
/// read error is ignored (creation should not fail on a diagnostic).
pub(crate) fn warn_name_collision(cfg: &Config, name: &str) {
	let key = crate::env::naming::namespace_key(name);
	if let Ok(names) = registry::list_names(cfg) {
		for existing in names.iter().filter(|e| e.as_str() != name) {
			if crate::env::naming::namespace_key(existing) == key {
				eprintln!(
					"clinix: warning: `{name}` normalizes to the same as existing registry env `{existing}` (`-`≡`_`)"
				);
			}
		}
	}
	if let Ok(settings) = crate::env::config::Settings::load(&cfg.config_dir) {
		let catalog = crate::env::seeds::Catalog::build(&settings.env.seeds);
		for seed in &catalog.seeds {
			if crate::env::naming::namespace_key(&seed.name) == key {
				eprintln!(
					"clinix: warning: `{name}` shadows seed `{}` — `clinix env {name}` will enter the new env, not the seed",
					seed.name
				);
			}
		}
	}
}

/// A resolved stack node: an env with its own `shell.nix` (project/registry), an
/// explicit `*.nix` **File** run directly, or a seed fragment from the catalog.
pub(crate) enum Node {
	Env(Env),
	File { path: PathBuf },
	Seed {
		name: String,
		path: PathBuf,
		/// The fragment asked for the declared `[env.pins]` set — carried from the
		/// catalog ([`crate::env::seeds::Seed::wants_pins`]) so composition does not
		/// re-parse the file.
		wants_pins: bool,
	},
}

/// A resolved env's human label: its registry name, else the resolved directory's
/// basename (a project/cwd env). Un-slugged — callers that need a filename slug it.
pub(crate) fn env_label(env: &Env) -> String {
	env.name
		.as_deref()
		.filter(|n| !n.contains('/') && *n != ".")
		.map(str::to_string)
		.or_else(|| {
			env.root
				.file_name()
				.map(|s| s.to_string_lossy().into_owned())
		})
		.unwrap_or_else(|| "env".to_string())
}

/// A `*.nix` file target's label: its file stem (`dev.nix` → `dev`).
pub(crate) fn file_label(path: &Path) -> String {
	path.file_stem()
		.map(|s| s.to_string_lossy().into_owned())
		.unwrap_or_else(|| "shell".to_string())
}

/// The entry file for a directory-backed env: `shell.nix` if present, else
/// `default.nix` (matching `nix-shell`'s own lookup), else a clear error.
pub(crate) fn env_shell_file(root: &Path) -> Result<PathBuf> {
	for name in ["shell.nix", "default.nix"] {
		let candidate = root.join(name);
		if candidate.is_file() {
			return Ok(candidate);
		}
	}
	Err(ClinixError::Resolve(format!(
		"no shell.nix or default.nix at {} (run: clinix env init)",
		root.display()
	)))
}

/// Map one token to a stack node, following the target precedence: an explicit
/// `*.nix` **File** (checked first, so `./dev.nix` runs the file, not
/// `./dev.nix/shell.nix`); else a directory-backed env via [`resolve`]
/// (cwd/Dir/Member/bare); else a seed from the catalog (`namespace:name`, or a
/// bare name — highest-precedence match, warning on a collision); else
/// [`ClinixError::UnknownEnv`].
pub(crate) fn resolve_node(
	cfg: &Config,
	catalog: &crate::env::seeds::Catalog,
	name: &str,
) -> Result<Node> {
	use crate::env::seeds::Resolved;
	// File target: a `*.nix` path (or any existing file). A `.nix` name that does
	// not exist is a clear error, not a fall-through to a registry/seed lookup.
	let path = Path::new(name);
	if name.ends_with(".nix") || path.is_file() {
		return if path.is_file() {
			Ok(Node::File {
				path: std::fs::canonicalize(path)?,
			})
		} else {
			Err(ClinixError::Resolve(format!("no such shell file: {name}")))
		};
	}
	match resolve(cfg, Some(name)) {
		Ok(env) => Ok(Node::Env(env)),
		Err(ClinixError::UnknownEnv(_)) => match catalog.find(name) {
			Some(Resolved::One(seed)) => Ok(Node::Seed {
				name: seed.name.clone(),
				path: seed.path.clone(),
				wants_pins: seed.wants_pins,
			}),
			Some(Resolved::Collision { chosen, others }) => {
				// Prefer the `namespace:name` selector; a seed without a namespace
				// has no such form, so point at its exact path (always works).
				let selector = |s: &crate::env::seeds::Seed| match &s.namespace {
					Some(ns) => format!("{ns}:{}", s.name),
					None => s.path.display().to_string(),
				};
				let alts = others
					.iter()
					.map(|s| selector(s))
					.collect::<Vec<_>>()
					.join(", ");
				eprintln!(
					"clinix: seed `{name}` is ambiguous — using `{}` (also: {alts}; qualify to pick another)",
					selector(chosen)
				);
				Ok(Node::Seed {
					name: chosen.name.clone(),
					path: chosen.path.clone(),
					wants_pins: chosen.wants_pins,
				})
			}
			None => Err(ClinixError::UnknownEnv(name.to_string())),
		},
		Err(e) => Err(e),
	}
}

/// A resolved node's composition label — a seed's name, a registry/project env's
/// [`env_label`], or a `*.nix` file's [`file_label`]. Shared by [`union_item`] (the
/// composed shell) and [`resolve_stack_key`] (the offline key) so both agree.
pub(crate) fn node_label(node: &Node) -> String {
	match node {
		Node::Env(env) => env_label(env),
		Node::File { path } => file_label(path),
		Node::Seed { name, .. } => name.clone(),
	}
}

/// The lockfile providing the seed catalog's nixpkgs pin: a configured
/// `flake_lock` path, or a ref clinix lazily locks into `<config>/flake.lock`
/// (default `nixos-26.05`). Never a channel (the design's §0).
pub(crate) fn seed_lock(cfg: &Config, settings: &crate::env::config::Settings) -> Result<PathBuf> {
	use crate::env::config::NixpkgsPin;
	match &settings.env.nixpkgs {
		Some(pin) => pin_lock(cfg, pin, None),
		// Unset ⇒ the latest stable, locked lazily like any other ref.
		None => pin_lock(cfg, &NixpkgsPin::Ref("nixos-26.05".to_string()), None),
	}
}

/// Resolve **any** nixpkgs pin — the stack pin (`[env].nixpkgs`, `name = None`) or a
/// named alternate (`[env.pins].<name>`) — to the lockfile it denotes. One function
/// so both kinds accept the identical two spellings and report errors identically:
///
/// - `{ flake_lock = "…" }` — an existing lock is authoritative, and must exist;
/// - `"<ref>"` — a branch/tag/rev clinix lazily locks **once** (network) into
///   `<config>/flake.lock` for the stack pin, or `<config>/locks/<name>.lock` for a
///   named one. An existing file at that path is adopted as-is, never overwritten,
///   so a lock you wrote yourself (or moved there) is picked up unchanged.
pub(crate) fn pin_lock(
	cfg: &Config,
	pin: &crate::env::config::NixpkgsPin,
	name: Option<&str>,
) -> Result<PathBuf> {
	use crate::env::config::{NixpkgsPin, expand_tilde};
	// Where a `Ref` gets lazily locked, and how to name this pin in an error.
	let (lazy_path, label) = match name {
		Some(n) => (cfg.config_dir.join("locks").join(format!("{n}.lock")), format!("[env.pins].{n}")),
		None => (cfg.config_dir.join("flake.lock"), "[env].nixpkgs".to_string()),
	};
	match pin {
		NixpkgsPin::FlakeLock { flake_lock } => {
			let p = expand_tilde(flake_lock);
			if p.is_file() {
				Ok(p)
			} else {
				Err(ClinixError::Config(format!(
					"{label}.flake_lock not found: {}",
					p.display()
				)))
			}
		}
		NixpkgsPin::Ref(r) => ensure_lock(r, &lazy_path),
	}
}

/// Every declared `[env.pins]` entry resolved to its lockfile, in config order.
/// Called only when a composition member actually asked for `pins`, so an unused
/// `[env.pins]` never triggers the lazy locking (and never touches the network).
pub(crate) fn named_pins(
	cfg: &Config,
	settings: &crate::env::config::Settings,
) -> Result<Vec<(String, PathBuf)>> {
	settings
		.env
		.pins
		.iter()
		.map(|(name, pin)| Ok((name.clone(), pin_lock(cfg, pin, Some(name))?)))
		.collect()
}

/// The optional nixpkgs `config` file (`[env].nixpkgs_config`, e.g. an
/// `allowUnfree` predicate), expanded and validated to exist. `None` when unset —
/// clinix then passes no explicit config, so nixpkgs uses its default
/// (`~/.config/nixpkgs/config.nix`).
pub(crate) fn nixpkgs_config_path(
	settings: &crate::env::config::Settings,
) -> Result<Option<PathBuf>> {
	match &settings.env.nixpkgs_config {
		Some(p) => {
			let path = crate::env::config::expand_tilde(p);
			if path.is_file() {
				Ok(Some(path))
			} else {
				Err(ClinixError::Config(format!(
					"[env].nixpkgs_config not found: {}",
					path.display()
				)))
			}
		}
		None => Ok(None),
	}
}

/// Lazily create `lock` pinning nixpkgs to `nixpkgs_ref` (network, **once**),
/// reusing `init`'s classic lock builder. Committable — the user's pin. An existing
/// file is returned untouched, which is what makes a hand-written or relocated lock
/// adoptable: declare the ref, drop the lock at the path, and clinix uses it.
fn ensure_lock(nixpkgs_ref: &str, lock: &Path) -> Result<PathBuf> {
	if lock.is_file() {
		return Ok(lock.to_path_buf());
	}
	// Silent for now (no `--quiet` on launch); the same first-run lock could show
	// progress if wanted — see `crate::progress`.
	let built =
		crate::env::init::build_nixpkgs_lock(nixpkgs_ref, &crate::progress::Progress::silent())?;
	if let Some(parent) = lock.parent() {
		std::fs::create_dir_all(parent)?;
	}
	std::fs::write(lock, built.to_json())?;
	Ok(lock.to_path_buf())
}

/// The unified verb set. Every verb takes an env name (or several, for the
/// compositional ones: `shell`, `run`, `shared`).
///
/// Declaration order is the help-listing order, so the verbs cluster by the three
/// groups named in `EnvArgs`'s `after_help` legend — **management**, then
/// **execution**, then **diagnostics** (clap 4 has no native per-group subcommand
/// headings; see clap issue #1553). This ordering is presentation only — every
/// verb is still invoked flat as `clinix env <verb>`.
#[derive(Subcommand, Debug)]
pub enum Cmd {
	// -- management: create, edit, and maintain envs --------------------------
	/// Scaffold a new **project** env in a directory (adopt via `--from` is a
	/// later slice; init never overwrites existing project files).
	Init(Init),
	/// Create a new **registry** env — two modes, chosen by `--from`: compose seed
	/// fragments (`--from a b c`) into a frozen, portable env, or register an
	/// existing project/shell (`--from <dir-or-file>`) under a name.
	New(New),
	/// Rename a registered env.
	Rename(Rename),
	/// Add packages to an env's `shell.nix`.
	Add(Pkgs),
	/// Remove packages from an env's `shell.nix`.
	Remove(Pkgs),
	/// Manage an env's `flake.lock` inputs and its flake front door:
	/// `flake <env> add|rm|freeze|unfreeze|wrap-shell`.
	Flake(Flake),
	/// Advance the env's tracked `flake.lock` inputs to the latest commit of the
	/// branch each tracks. Frozen (rev-pinned) and non-github inputs are left
	/// as-is; the lock is written only if something advanced.
	Update(Update),
	/// Report tracked inputs whose upstream rev is newer than the lock (no writes).
	Outdated(Target),
	/// Re-check every locked input still hashes correctly.
	Verify(Target),
	/// Register an env from an exported closure plus its `shell.nix` (the inverse
	/// of `export closure`). Other source formats (flake / .deb / OCI / …) are a
	/// later slice.
	Import(Import),
	/// Export an env to another format.
	Export(Export),
	/// Release one or more envs' GC roots so their store paths can be collected
	/// (whole-env, or version-targeted via `--root-version`/`--oldest`).
	Clean(clean::Clean),

	// -- execution: instantiate and enter/run a composition -------------------
	/// Enter an interactive shell for the composed env(s).
	Shell(Shell),
	/// Run a command inside the composed env(s), non-interactively.
	Run(Run),

	// -- diagnostics: read-only reports over resolved envs --------------------
	/// List everything `env <name>` can enter: registry envs (with their nixpkgs
	/// pin) and seeds (with their namespace and source path).
	List(diagnostics::List),
	/// Summarize a single env (nixpkgs pin + resolved package versions).
	Info(Info),
	/// Dependency/closure report for an env.
	Deps(Deps),
	/// N-way shared-package comparison across several envs.
	Shared(Targets),
	/// Environment/PATH audit (the former `envcheck`).
	Check(OptionalTarget),
	/// List GC-root versions — one env's (`roots <name>`) or, with no name, a grouped
	/// listing of every root family (`--size` for disk, `--json` for machine output).
	Roots(diagnostics::Roots),
}

impl RunCmd for Cmd {
	/// Pure dispatch. Verbs with a dedicated struct delegate to their own
	/// `RunCmd` impl; verbs that share an arg shape delegate to a free function
	/// in the owning submodule — so every verb's logic lives in its own file.
	fn run(self, context: &Context) -> Result<()> {
		use Cmd::*;
		match self {
			// Dedicated struct + `RunCmd` impl in the submodule.
			Init(a) => a.run(context),
			New(a) => a.run(context),
			Rename(a) => a.run(context),

			// Package management
			Add(a) => pkgs::add(a, context),
			Remove(a) => pkgs::remove(a, context),
			Flake(a) => pkgs::flake(a, context),
			Update(a) => a.run(context),
			Outdated(a) => pkgs::outdated(a, context),
			Verify(a) => pkgs::verify(a, context),

			Shell(a) => a.run(context),
			Run(a) => a.run(context),

			Import(a) => a.run(context),
			Export(a) => a.run(context),

			// Diagnostic information
			List(a) => diagnostics::list(a, context),
			Info(a) => a.run(context),
			Deps(a) => a.run(context),
			Shared(a) => diagnostics::shared(a, context),
			Check(a) => diagnostics::check(a, context),
			Roots(a) => diagnostics::roots(a, context),
			Clean(a) => clean::clean(a, context),
		}
	}
}

/// The arguments for every command.
///
/// The `after_help` legend groups the flat verb list into management/execution/
/// diagnostics — a legend rather than real subcommand headings because clap 4 has
/// no native per-group subcommand headings (issue #1553). The verbs are declared
/// in that same group order (see [`Cmd`]) so the "Commands" block and the legend
/// agree.
#[derive(Args, Debug)]
#[command(after_help = "\
Command groups:
  management   init new rename add remove flake update outdated verify import export clean
  execution    shell run
  diagnostics  list info deps shared check roots")]
pub struct EnvArgs {
	#[command(subcommand)]
	pub cmd: Option<Cmd>,
	/// `clinix env <names…>` ≡ `clinix env shell <names…>`. Parsed only when no
	/// verb matched.
	#[command(flatten)]
	pub shell: Shell,
}

/// Rename a registered env (registry relabel). Kept here in the shared file
/// because it is not owned by any single verb submodule.
#[derive(Args, Debug)]
pub struct Rename {
	pub old: String,
	pub new: String,
}

impl RunCmd for Rename {
	/// Relabel a registry env: `mv envs/<old> envs/<new>` plus the `env-<name>`
	/// GC-root rename ([`registry::rename`]), O(1) and rename-correct.
	fn run(self, context: &Context) -> Result<()> {
		registry::rename(&context.config, &self.old, &self.new)?;
		println!("clinix: renamed env `{}` → `{}`", self.old, self.new);
		Ok(())
	}
}

/// A single required env name.
#[derive(Args, Debug)]
pub struct Target {
	pub name: String,
}

/// A single optional env name (defaults to the cwd project).
#[derive(Args, Debug)]
pub struct OptionalTarget {
	pub name: Option<String>,
}

/// One or more env names.
#[derive(Args, Debug)]
pub struct Targets {
	#[arg(required = true)]
	pub names: Vec<String>,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn env_shell_file_prefers_shell_nix_then_default_nix() {
		let dir = tempfile::tempdir().unwrap();
		// Neither present → a clear error naming the directory.
		assert!(env_shell_file(dir.path()).is_err());
		// default.nix alone → used (nix-shell's fallback).
		std::fs::write(dir.path().join("default.nix"), "{}").unwrap();
		assert_eq!(
			env_shell_file(dir.path()).unwrap(),
			dir.path().join("default.nix")
		);
		// shell.nix takes precedence when both exist.
		std::fs::write(dir.path().join("shell.nix"), "{}").unwrap();
		assert_eq!(
			env_shell_file(dir.path()).unwrap(),
			dir.path().join("shell.nix")
		);
	}
}
