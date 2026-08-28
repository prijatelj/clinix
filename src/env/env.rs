//! Shared `env` primitives: the verb dispatch ([`Cmd`]), the [`RunCmd`] trait,
//! [`Context`], env resolution ([`resolve`]/[`Env`]/[`Kind`]), the generic
//! env-name selectors, and the `rename` verb. Each concrete verb lives in its
//! own sibling submodule; this file dispatches to them.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};

use crate::error::{ClinixError, Result, unimplemented};

use super::export::Export;
use super::import::Import;
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

/// Shared execution context: state dir, verbosity, resolved options, etc.
pub struct Context {
	pub options: ShellOptions,
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
/// pure name → root-dir resolution:
/// - `Some(".")` or `None` → the cwd project ([`Kind::Project`]);
/// - `Some(name)` that is a path (contains `/` or is an existing dir) →
///   [`Kind::Project`] at that path;
/// - `Some(name)` otherwise → a registry env at `state/envs/<name>` if that dir
///   exists ([`Kind::Registry`]); else [`ClinixError::UnknownEnv`].
///
/// Full registry indexing/enumeration (listing, config default) lands with
/// `state.rs` in phase 5; this resolves the registry *path* by convention so
/// `init`/`shell` work against named envs now.
pub fn resolve(name: Option<&str>) -> Result<Env> {
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
			let root = registry_dir().join(n);
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

/// The clinix state root: `$XDG_STATE_HOME/clinix`, else `~/.local/state/clinix`.
/// A minimal stand-in until `state.rs` (phase 5) adopts `etcetera` + config.
fn state_dir() -> PathBuf {
	if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
		PathBuf::from(xdg).join("clinix")
	} else if let Some(home) = std::env::var_os("HOME") {
		PathBuf::from(home).join(".local/state/clinix")
	} else {
		PathBuf::from(".clinix-state")
	}
}

/// Where registry envs live: `state/envs/<name>` (plan §state layout).
fn registry_dir() -> PathBuf {
	state_dir().join("envs")
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
	/// Enter an interactive shell for the composed env(s).
	Shell(Shell),
	/// Run a command inside the composed env(s), non-interactively.
	Run(Run),
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
	/// Rename a registered env.
	Rename(Rename),
	/// List registered envs.
	List,
	/// Import an env from another format (shell.nix / flake / .deb / OCI / …).
	Import(Import),
	/// Export an env to another format.
	Export(Export),
	/// Summarize a single env (packages, pins, diagnostics).
	Info(Target),
	/// Dependency/closure report for an env.
	Deps(Target),
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
			Shell(a) => a.run(context),
			Run(a) => a.run(context),
			Update(a) => a.run(context),
			Rename(a) => a.run(context),
			Import(a) => a.run(context),
			Export(a) => a.run(context),
			// Shared arg shape → free fn in the submodule.
			Add(a) => pkgs::add(a, context),
			Remove(a) => pkgs::remove(a, context),
			Pin(a) => pkgs::pin(a, context),
			Unpin(a) => pkgs::unpin(a, context),
			List => info::list(context),
			Info(a) => info::info(a, context),
			Deps(a) => info::deps(a, context),
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
	fn run(self, _context: &Context) -> Result<()> {
		Err(unimplemented("env rename", "plan phase 5: registry relabel"))
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
