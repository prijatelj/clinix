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

use super::export::Export;
use super::import::Import;
use super::info::{Deps, Info};
use super::init::Init;
use super::new::New;
use super::pkgs::{self, Pin, Pkgs, Update};
use super::run::Run;
use super::shell::Shell;
use super::{clean, info};

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

/// Shared launcher for `shell`/`run`. Resolves the name stack, then either enters
/// a single project/registry env's `shell.nix`, or **composes a stack of seed
/// fragments** against the configured nixpkgs pin (lexical order, or the given
/// order under `-o`) — the `~/dev_env` `compose-dev.nix` behavior. GC-roots the
/// result and enters it via classic `nix-shell`. Composing a self-contained env
/// (project/registry) into a multi-node stack is deferred with self-contained
/// seeds, so a stack must currently be all seeds.
pub(crate) fn launch(
	ctx: &Context,
	names: &[String],
	pure: bool,
	command: Option<&str>,
) -> Result<ExitStatus> {
	let cfg = &ctx.config;
	let settings = crate::env::config::Settings::load(&cfg.config_dir)?;
	let catalog = crate::env::seeds::Catalog::build(&settings.env.seeds);
	for w in &catalog.warnings {
		eprintln!("clinix: {w}");
	}

	// Empty stack ⇒ the cwd project.
	if names.is_empty() {
		return launch_env(cfg, &resolve(cfg, None)?, pure, command);
	}

	let nodes: Vec<Node> = names
		.iter()
		.map(|n| resolve_node(cfg, &catalog, n))
		.collect::<Result<_>>()?;

	// A single self-contained node enters directly (no composition): a
	// project/registry env's own shell, or an explicit `*.nix` file.
	match nodes.as_slice() {
		[Node::Env(env)] => return launch_env(cfg, env, pure, command),
		[Node::File { path }] => return launch_file(cfg, path, pure, command),
		_ => {}
	}

	// Otherwise a stack: v1 composes seeds only. A self-contained node (env or
	// file) in a multi-node stack needs self-contained composition (deferred).
	let mut seeds: Vec<(String, PathBuf)> = Vec::with_capacity(nodes.len());
	for node in &nodes {
		match node {
			Node::Seed { name, path } => seeds.push((name.clone(), path.clone())),
			Node::Env(_) | Node::File { .. } => {
				return Err(crate::error::unimplemented(
					"composing a self-contained env/file into a stack",
					"seeds-only stacks for now; self-contained seeds are deferred",
				));
			}
		}
	}

	// Order: lexical by default; `-o` preserves the given order.
	if !ctx.options.ordered {
		seeds.sort_by(|a, b| a.0.cmp(&b.0));
	}
	let label = seeds
		.iter()
		.map(|(n, _)| n.as_str())
		.collect::<Vec<_>>()
		.join(" ");
	let paths: Vec<PathBuf> = seeds.iter().map(|(_, p)| p.clone()).collect();

	// Compose against the configured nixpkgs pin, root by the ordered stack, enter.
	let lock = seed_lock(cfg, &settings)?;
	let nixpkgs_config = nixpkgs_config_path(&settings)?;
	let expr = crate::env::seeds::compose_expr(&lock, &paths, &label, nixpkgs_config.as_deref());
	let key = format!(
		"stack-{}",
		seeds.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join("-")
	);
	let compose_dir = registry::compose_dir(cfg);
	std::fs::create_dir_all(&compose_dir)?;
	let compose_file = compose_dir.join(format!("{key}.nix"));
	std::fs::write(&compose_file, expr)?;
	let root = registry::roots_dir(cfg).join(&key);
	let drv = crate::nix::instantiate_rooted(&compose_file, &root)?;
	crate::nix::nix_shell(&drv, pure, command)
}

/// Enter a single env's own shell (project/registry), GC-rooted by its key. The
/// entry file is `shell.nix`, else `default.nix` — the same fallback `nix-shell`
/// itself uses.
fn launch_env(cfg: &Config, env: &Env, pure: bool, command: Option<&str>) -> Result<ExitStatus> {
	let shell_file = env_shell_file(&env.root)?;
	let root = registry::root_path(cfg, env);
	let drv = crate::nix::instantiate_rooted(&shell_file, &root)?;
	crate::nix::nix_shell(&drv, pure, command)
}

/// Run an explicit `*.nix` **File** target directly (`clinix env shell dev.nix`),
/// GC-rooted by a path slug (like a project — the file has no registry name). The
/// file is a standalone shell expression; it is entered as-is, not composed.
fn launch_file(cfg: &Config, file: &Path, pure: bool, command: Option<&str>) -> Result<ExitStatus> {
	let slug = file.to_string_lossy().replace('/', "_");
	let root = registry::roots_dir(cfg).join(format!("file-{}", slug.trim_start_matches('_')));
	let drv = crate::nix::instantiate_rooted(file, &root)?;
	crate::nix::nix_shell(&drv, pure, command)
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
pub(crate) fn nixpkgs_config_path(settings: &crate::env::config::Settings) -> Result<Option<PathBuf>> {
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
#[derive(Subcommand, Debug)]
pub enum Cmd {
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
	/// Pin package versions in `flake.lock` (`--all` = closure freeze).
	Pin(Pin),
	/// Unpin packages back to baseline tracking (`--all` = unfreeze).
	Unpin(Pin),
	/// Update unpinned packages to the latest the baseline provides.
	Update(Update),

	/// Enter an interactive shell for the composed env(s).
	Shell(Shell),
	/// Run a command inside the composed env(s), non-interactively.
	Run(Run),

	/// Import an env from another format (shell.nix / flake / .deb / OCI / …).
	Import(Import),
	/// Export an env to another format.
	Export(Export),

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
	/// Release an env's GC root so its store paths can be collected.
	Clean(Target),

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
			Pin(a) => pkgs::pin(a, context),
			Unpin(a) => pkgs::unpin(a, context),
			Update(a) => a.run(context),

			Shell(a) => a.run(context),
			Run(a) => a.run(context),

			Import(a) => a.run(context),
			Export(a) => a.run(context),

			// Diagnostic information
			List => info::list(context),
			Info(a) => a.run(context),
			Deps(a) => a.run(context),
			Shared(a) => info::shared(a, context),
			Check(a) => info::check(a, context),
			Clean(a) => clean::clean(a, context),

			// Bare-name sugar → the launcher (interactive shell).
			Compose(names) => super::shell::Shell { names, pure: false }.run(context),
		}
	}
}

/// The arguments for every command.
#[derive(Args, Debug)]
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
