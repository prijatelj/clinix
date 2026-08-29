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

/// Shared launcher for `shell`/`run`: resolve a **single** env, GC-root its
/// `shell.nix` ([`crate::nix::instantiate_rooted`]) under the env's keyed root
/// ([`registry::root_path`]), and enter it via classic `nix-shell` —
/// interactive, or `--run <command>` for `run`. Multi-env composition/union over
/// a name stack, and the eval cache, are phase 5.
pub(crate) fn launch(
	cfg: &Config,
	names: &[String],
	pure: bool,
	command: Option<&str>,
) -> Result<ExitStatus> {
	if names.len() > 1 {
		return Err(crate::error::unimplemented(
			"env shell/run with multiple envs",
			"plan phase 5: compose/union of a name stack",
		));
	}
	let env = resolve(cfg, names.first().map(String::as_str))?;
	let shell_nix = env.root.join("shell.nix");
	if !shell_nix.is_file() {
		return Err(ClinixError::Resolve(format!(
			"no shell.nix at {} (run: clinix env init)",
			env.root.display()
		)));
	}
	let root = registry::root_path(cfg, &env);
	let drv = crate::nix::instantiate_rooted(&shell_nix, &root)?;
	crate::nix::nix_shell(&drv, pure, command)
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
