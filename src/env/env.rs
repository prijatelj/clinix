//! Shared `env` primitives: the verb dispatch ([`Cmd`]), the [`RunCmd`] trait,
//! [`Context`], env resolution ([`resolve`]/[`Env`]/[`Kind`]), the generic
//! env-name selectors, and the `rename` verb. Each concrete verb lives in its
//! own sibling submodule; this file dispatches to them.

use std::path::{Path, PathBuf};
use std::process::ExitStatus;

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

/// Composition options read from the global flags (so the `external_subcommand`
/// bare-name sugar can still set them). See [`crate::cli`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ShellOptions {
	/// Preserve the given order instead of sorting names lexically.
	pub ordered: bool,
	/// In a project, compose only the runtime env (skip dev tools).
	pub runtime: bool,
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
enum Node {
	Env(Env),
	File { path: PathBuf },
	Seed { name: String, path: PathBuf },
}

/// A resolved composition: the single nix file to instantiate (an env's own
/// shell, a `*.nix` file, or a written seed-stack compose file), a human `label`,
/// and the GC-root path keyed for it. Produced by [`compose_nodes`] and consumed
/// by both `launch` (enter it) and `export` (serialize/image it).
pub(crate) struct Composition {
	pub shell_file: PathBuf,
	pub label: String,
	pub root: PathBuf,
	/// The `flake.lock` pinning this composition's nixpkgs, when discoverable (a
	/// registry env's own lock, a file target's adjacent lock, or the seed pin for
	/// a stack). `export docker` needs it to pin `dockerTools`.
	pub lock: Option<PathBuf>,
}

/// A directory-backed env's own `flake.lock`, if present.
fn env_lock(env: &Env) -> Option<PathBuf> {
	let lock = env.root.join("flake.lock");
	lock.is_file().then_some(lock)
}

/// Resolve a name list to a single instantiable [`Composition`] — the one place
/// the target precedence + seed-stack composition live, shared by `launch` and
/// `export`. Empty names ⇒ the cwd project. No nix eval or entry here; a stack
/// writes its compose expression to `state/compose/`. A union may mix a
/// self-contained env (project/registry/file) with seeds **only when all such
/// envs share a byte-identical `flake.lock`** (the shared-pin union); the general
/// multi-pin case stays deferred.
pub(crate) fn compose_nodes(ctx: &Context, names: &[String]) -> Result<Composition> {
	let cfg = &ctx.config;
	let settings = crate::env::config::Settings::load(&cfg.config_dir)?;
	let catalog = crate::env::seeds::Catalog::build(&settings.env.seeds);
	for w in &catalog.warnings {
		eprintln!("clinix: {w}");
	}

	// Empty ⇒ the cwd project.
	if names.is_empty() {
		let env = resolve(cfg, None)?;
		return Ok(Composition {
			shell_file: env_shell_file(&env.root)?,
			label: env_label(&env),
			root: registry::root_path(cfg, &env),
			lock: env_lock(&env),
		});
	}

	let nodes: Vec<Node> = names
		.iter()
		.map(|n| resolve_node(cfg, &catalog, n))
		.collect::<Result<_>>()?;

	// A single self-contained node: an env's own shell, or an explicit `*.nix` file.
	match nodes.as_slice() {
		[Node::Env(env)] => {
			return Ok(Composition {
				shell_file: env_shell_file(&env.root)?,
				label: env_label(env),
				root: registry::root_path(cfg, env),
				lock: env_lock(env),
			});
		}
		[Node::File { path }] => {
			let lock = path
				.parent()
				.map(|d| d.join("flake.lock"))
				.filter(|l| l.is_file());
			return Ok(Composition {
				shell_file: path.clone(),
				label: file_label(path),
				root: registry::file_root(cfg, path),
				lock,
			});
		}
		_ => {}
	}

	// Otherwise a **union** of 2+ nodes. Seeds inject the shared `pkgs`;
	// self-contained nodes (project/registry env, `*.nix` file) read their own
	// `flake.lock` — permitted **only when all such locks are byte-identical** (a
	// shared pin), else the multi-pin case stays deferred.
	let mut items: Vec<UnionItem> = Vec::with_capacity(nodes.len());
	for node in &nodes {
		items.push(union_item(node)?);
	}

	// The shared pin: every self-contained node's lock must be identical; else the
	// seeds compose against the configured seed pin (an all-seeds union).
	let sc_locks: Vec<&PathBuf> = items.iter().filter_map(|i| i.lock.as_ref()).collect();
	let lock = match sc_locks.first() {
		Some(first) => {
			let first_bytes = std::fs::read(first)?;
			for other in &sc_locks[1..] {
				if std::fs::read(other)? != first_bytes {
					return Err(crate::error::unimplemented(
						"composing envs with differing flake.locks",
						"only a shared flake.lock is supported at this time — every env in a \
						 union must share one pin",
					));
				}
			}
			(*first).clone()
		}
		None => seed_lock(cfg, &settings)?,
	};

	// Order: lexical by default; `-o` preserves the given order.
	if !ctx.options.ordered {
		items.sort_by(|a, b| a.label.cmp(&b.label));
	}
	let label = items
		.iter()
		.map(|i| i.label.as_str())
		.collect::<Vec<_>>()
		.join(" ");
	let imports: Vec<(PathBuf, bool)> = items.iter().map(|i| (i.shell.clone(), i.inject)).collect();

	let nixpkgs_config = nixpkgs_config_path(&settings)?;
	let expr = crate::env::seeds::compose_expr(&lock, &imports, &label, nixpkgs_config.as_deref());
	let key = stack_key(items.iter().map(|i| i.label.as_str()));
	let compose_dir = registry::compose_dir(cfg);
	std::fs::create_dir_all(&compose_dir)?;
	let compose_file = compose_dir.join(format!("{key}.nix"));
	std::fs::write(&compose_file, expr)?;
	Ok(Composition {
		shell_file: compose_file,
		label,
		root: registry::roots_dir(cfg).join(&key),
		lock: Some(lock),
	})
}

/// One node's contribution to a union: the shell file to import, whether to inject
/// the shared `pkgs` (seeds do; self-contained shells read their own lock), and its
/// own `flake.lock` for the shared-pin check (self-contained only).
struct UnionItem {
	label: String,
	shell: PathBuf,
	inject: bool,
	lock: Option<PathBuf>,
}

fn union_item(node: &Node) -> Result<UnionItem> {
	Ok(match node {
		Node::Seed { name, path } => UnionItem {
			label: name.clone(),
			shell: path.clone(),
			inject: true,
			lock: None,
		},
		Node::Env(env) => {
			let lock = env_lock(env).ok_or_else(|| {
				ClinixError::Config(format!(
					"`{}` has no flake.lock — it cannot join a shared-flake.lock union",
					env_label(env)
				))
			})?;
			UnionItem {
				label: env_label(env),
				shell: env_shell_file(&env.root)?,
				inject: false,
				lock: Some(lock),
			}
		}
		Node::File { path } => {
			let lock = path
				.parent()
				.map(|d| d.join("flake.lock"))
				.filter(|l| l.is_file())
				.ok_or_else(|| {
					ClinixError::Config(format!(
						"`{}` has no adjacent flake.lock — it cannot join a shared-flake.lock union",
						path.display()
					))
				})?;
			UnionItem {
				label: file_label(path),
				shell: path.clone(),
				inject: false,
				lock: Some(lock),
			}
		}
	})
}

/// Shared launcher for `shell`/`run`: [`compose_nodes`] the names, GC-root, and
/// enter via classic `nix-shell` (`--run <command>` when non-interactive).
pub(crate) fn launch(
	ctx: &Context,
	names: &[String],
	pure: bool,
	command: Option<&str>,
) -> Result<ExitStatus> {
	let comp = compose_nodes(ctx, names)?;
	let drv = crate::nix::instantiate_rooted(&comp.shell_file, &comp.root)?;
	// The retention guarantee: root the complete build closure via `inputDerivation`
	// (independent of `keep-outputs`). Non-fatal if the shell isn't an mkDerivation —
	// the `.drv` root still stands, so we warn and enter anyway.
	let rt = registry::rt_root(&comp.root);
	if let Err(e) = crate::nix::root_input_closure(&comp.shell_file, &rt) {
		eprintln!(
			"clinix: warning: could not root `{}`'s full closure ({e}); \
			 packages may be collected unless `keep-outputs = true`",
			comp.label
		);
	}
	crate::nix::nix_shell(&drv, pure, command)
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
fn file_label(path: &Path) -> String {
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
fn resolve_node(cfg: &Config, catalog: &crate::env::seeds::Catalog, name: &str) -> Result<Node> {
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
				})
			}
			None => Err(ClinixError::UnknownEnv(name.to_string())),
		},
		Err(e) => Err(e),
	}
}

/// The `stack-<labels>` GC-root key for a seed/union composition: each label
/// slugged and joined by `-`. One source of truth shared by [`compose_nodes`] (all
/// members) and [`node_root`] (a single seed → `stack-<label>`, since the join of
/// one element is itself).
fn stack_key<'a>(labels: impl Iterator<Item = &'a str>) -> String {
	format!(
		"stack-{}",
		labels
			.map(|l| crate::env::naming::slug(l, false, false, None))
			.collect::<Vec<_>>()
			.join("-")
	)
}

/// The GC-root path a single target *would* have been rooted under, computed with
/// **no side effects** (no compose-file write, no lazy locking, no network) — so
/// `clean` can locate an env/seed/file/project root offline. Mirrors
/// [`compose_nodes`]' single-target keying exactly: a registry/project env →
/// [`registry::root_path`], a `*.nix` file → [`registry::file_root`], a single seed
/// → `stack-<label>` (the union branch's key for one member).
pub(crate) fn node_root(
	cfg: &Config,
	catalog: &crate::env::seeds::Catalog,
	name: &str,
) -> Result<PathBuf> {
	Ok(match resolve_node(cfg, catalog, name)? {
		Node::Env(env) => registry::root_path(cfg, &env),
		Node::File { path } => registry::file_root(cfg, &path),
		Node::Seed { name, .. } => registry::roots_dir(cfg).join(stack_key(std::iter::once(name.as_str()))),
	})
}

/// The lockfile providing the seed catalog's nixpkgs pin: a configured
/// `flake_lock` path, or a ref clinix lazily locks into `<config>/flake.lock`
/// (default `nixos-26.05`). Never a channel (the design's §0).
pub(crate) fn seed_lock(cfg: &Config, settings: &crate::env::config::Settings) -> Result<PathBuf> {
	use crate::env::config::{NixpkgsPin, expand_tilde};
	match &settings.env.nixpkgs {
		Some(NixpkgsPin::FlakeLock { flake_lock }) => {
			let p = expand_tilde(flake_lock);
			if p.is_file() {
				Ok(p)
			} else {
				Err(ClinixError::Config(format!(
					"[env].nixpkgs.flake_lock not found: {}",
					p.display()
				)))
			}
		}
		Some(NixpkgsPin::Ref(r)) => ensure_seed_lock(cfg, r),
		None => ensure_seed_lock(cfg, "nixos-26.05"),
	}
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

/// Lazily create `<config>/flake.lock` pinning nixpkgs to `nixpkgs_ref` (network,
/// once), reusing `init`'s classic lock builder. Committable — the user's pin.
fn ensure_seed_lock(cfg: &Config, nixpkgs_ref: &str) -> Result<PathBuf> {
	let lock = cfg.config_dir.join("flake.lock");
	if lock.is_file() {
		return Ok(lock);
	}
	// Silent for now (no `--quiet` on launch); the same first-run lock could show
	// progress if wanted — see `crate::progress`.
	let built =
		crate::env::init::build_nixpkgs_lock(nixpkgs_ref, &crate::progress::Progress::silent())?;
	std::fs::create_dir_all(&cfg.config_dir)?;
	std::fs::write(&lock, built.to_json())?;
	Ok(lock)
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
	/// Create a new **registry** env by merging existing envs (`--from A B C`).
	New(New),
	/// Rename a registered env.
	Rename(Rename),
	/// Add packages to an env's `shell.nix`.
	Add(Pkgs),
	/// Remove packages from an env's `shell.nix`.
	Remove(Pkgs),
	/// Manage `flake.lock` inputs: `flake <env> add|rm|freeze|unfreeze`.
	Flake(Flake),
	/// Update unpinned packages to the latest the baseline provides.
	Update(Update),
	/// Import an env from another format (shell.nix / flake / .deb / OCI / …).
	Import(Import),
	/// Export an env to another format.
	Export(Export),
	/// Release one or more envs' GC roots so their store paths can be collected.
	Clean(Targets),

	// -- execution: instantiate and enter/run a composition -------------------
	/// Enter an interactive shell for the composed env(s).
	Shell(Shell),
	/// Run a command inside the composed env(s), non-interactively.
	Run(Run),

	// -- diagnostics: read-only reports over resolved envs --------------------
	/// List registered envs.
	List,
	/// Summarize a single env (nixpkgs pin + resolved package versions).
	Info(Info),
	/// Dependency/closure report for an env.
	Deps(Deps),
	/// N-way shared-package comparison across several envs.
	Shared(Targets),
	/// Environment/PATH audit (the former `envcheck`).
	Check(OptionalTarget),

	/// Bare name list under `env` → `shell <names…>` (parity with the top-level
	/// `clinix <names…>` sugar), so `clinix env rust claude` composes those envs.
	/// The unmatched first token is folded back in. A name equal to a verb above
	/// is taken as that verb — use `env shell <name>` to enter such an env.
	#[command(external_subcommand)]
	Compose(Vec<String>),
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

			Shell(a) => a.run(context),
			Run(a) => a.run(context),

			Import(a) => a.run(context),
			Export(a) => a.run(context),

			// Diagnostic information
			List => diagnostics::list(context),
			Info(a) => a.run(context),
			Deps(a) => a.run(context),
			Shared(a) => diagnostics::shared(a, context),
			Check(a) => diagnostics::check(a, context),
			Clean(a) => clean::clean(a, context),

			// Bare-name sugar → the launcher (interactive shell). Fully qualified
			// because `use Cmd::*` shadows the `Shell` struct with the `Shell` variant.
			Compose(names) => super::execution::Shell { names, pure: false }.run(context),
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
  management   init new rename add remove flake update import export clean
  execution    shell run
  diagnostics  list info deps shared check")]
pub struct EnvArgs {
	#[command(subcommand)]
	pub cmd: Cmd,
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
	fn stack_key_joins_slugged_labels_and_a_single_label_is_itself() {
		// One source of truth for the union key and node_root's single-seed key: the
		// join of one element equals that element, so a lone seed keys `stack-<label>`.
		assert_eq!(stack_key(std::iter::once("rust")), "stack-rust");
		assert_eq!(stack_key(["rust", "claude"].into_iter()), "stack-rust-claude");
	}

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
