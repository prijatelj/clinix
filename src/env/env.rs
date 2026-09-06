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
/// pure name → root-dir resolution against the resolved [`Config`]:
/// - `Some(".")` or `None` → the cwd project ([`Kind::Project`]);
/// - `Some(name)` that is a path (contains `/` or is an existing dir) →
///   [`Kind::Project`] at that path;
/// - `Some(name)` otherwise → a registry env at `state/envs/<name>` if that dir
///   exists ([`Kind::Registry`]); else [`ClinixError::UnknownEnv`].
///
/// The registry *layout* (`envs/<name>`) lives in [`crate::env::registry`]; this only
/// resolves a name to a root. Enumeration is [`registry::list_names`].
pub fn resolve(cfg: &Config, name: Option<&str>) -> Result<Env> {
	match name {
		None | Some(".") => Ok(Env {
			name: None,
			root: std::env::current_dir()?,
			kind: Kind::Project,
		}),
		Some(n) => {
			let path = Path::new(n);
			if n.contains('/') || path.is_dir() {
				return Ok(Env {
					name: Some(n.to_string()),
					root: std::fs::canonicalize(path)?,
					kind: Kind::Project,
				});
			}
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

/// A resolved stack node: an env with its own `shell.nix` (project/registry), or
/// a seed fragment file from the catalog.
enum Node {
	Env(Env),
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

	// A single project/registry env: enter its own shell.nix.
	if let [Node::Env(env)] = nodes.as_slice() {
		return launch_env(cfg, env, pure, command);
	}

	// Otherwise a stack: v1 supports seeds only.
	let mut seeds: Vec<(String, PathBuf)> = Vec::with_capacity(nodes.len());
	for node in &nodes {
		match node {
			Node::Seed { name, path } => seeds.push((name.clone(), path.clone())),
			Node::Env(_) => {
				return Err(crate::error::unimplemented(
					"composing a project/registry env into a stack",
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
	let compose_dir = cfg.state_dir.join("compose");
	std::fs::create_dir_all(&compose_dir)?;
	let compose_file = compose_dir.join(format!("{key}.nix"));
	std::fs::write(&compose_file, expr)?;
	let root = registry::roots_dir(cfg).join(&key);
	let drv = crate::nix::instantiate_rooted(&compose_file, &root)?;
	crate::nix::nix_shell(&drv, pure, command)
}

/// Enter a single env's own `shell.nix` (project/registry), GC-rooted by its key.
fn launch_env(cfg: &Config, env: &Env, pure: bool, command: Option<&str>) -> Result<ExitStatus> {
	let shell_nix = env.root.join("shell.nix");
	if !shell_nix.is_file() {
		return Err(ClinixError::Resolve(format!(
			"no shell.nix at {} (run: clinix env init)",
			env.root.display()
		)));
	}
	let root = registry::root_path(cfg, env);
	let drv = crate::nix::instantiate_rooted(&shell_nix, &root)?;
	crate::nix::nix_shell(&drv, pure, command)
}

/// Map a name to a stack node: a project/registry env if [`resolve`] finds one,
/// else a seed from the catalog (`alias:name`, or a bare name — the highest-
/// precedence match, warning on a collision), else [`ClinixError::UnknownEnv`].
fn resolve_node(cfg: &Config, catalog: &crate::env::seeds::Catalog, name: &str) -> Result<Node> {
	use crate::env::seeds::Resolved;
	match resolve(cfg, Some(name)) {
		Ok(env) => Ok(Node::Env(env)),
		Err(ClinixError::UnknownEnv(_)) => match catalog.find(name) {
			Some(Resolved::One(seed)) => Ok(Node::Seed {
				name: seed.name.clone(),
				path: seed.path.clone(),
			}),
			Some(Resolved::Collision { chosen, others }) => {
				let alts = others
					.iter()
					.map(|s| format!("{}:{}", s.alias, s.name))
					.collect::<Vec<_>>()
					.join(", ");
				eprintln!(
					"clinix: seed `{name}` is ambiguous — using `{}:{}` (also: {alts}; qualify to pick another)",
					chosen.alias, chosen.name
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
	let built = crate::env::init::build_nixpkgs_lock(nixpkgs_ref)?;
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
