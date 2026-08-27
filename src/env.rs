//! The `env` scope: one unified surface over the former dev/user/run scopes.
//!
//! An environment is identified by a **name** that [`resolve`] maps to a **root
//! directory** holding `shell.nix` + `flake.nix` + `flake.lock` (the single
//! source of truth, per the locked design). Two population sources — a
//! **registry** of named, composable tool envs under the state dir, and
//! **project** directories resolved by path/cwd — are what the dev+run+user
//! merge collapses to: they differ only in resolution, not in command surface.

use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::error::{Result, unimplemented};
use crate::pkg::Pkg;

/// An environment type resolved to its root directory
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
	/// A named, reusable, composable env under the clinix state dir.
	Registry,
	/// A project directory (given by path, or the cwd when no name is given).
	Project,
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
	/// Directory containing `shell.nix` / `flake.nix` / `flake.lock`.
	pub root: PathBuf,
	pub kind: Kind,
}

/// The one per-source-divergent seam (plan §resolve). Phase 1 is shallow and
/// pure — no nix eval, no lock read:
/// - `Some(name)` registered → [`Kind::Registry`] at `state/envs/<name>`;
/// - `Some(name)` that is a path → [`Kind::Project`] at that path;
/// - `None` → the cwd project.
pub fn resolve(name: Option<&str>) -> Result<Env> {
	let _ = name;
	Err(unimplemented("env resolve", "plan phase 5: state registry + cwd detection"))
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

/// The unified verb set. Every verb takes an env name (or several, for the
/// compositional ones: `shell`, `run`, `shared`).
#[derive(Subcommand, Debug)]
pub enum Cmd {
	/// Scaffold a new env, or adopt existing state (shell.nix / flake / uv / cargo).
	Init(Init),
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
	fn run(self, context: &Context) -> Result<()> {
		use Cmd::*;
		match self {
			// impl their own RunCmd
			Init(a) 	=> a.run(context),
			Shell(a)	=> a.run(context),
			Run(a)		=> a.run(context),
			Update(a)	=> a.run(context),
			Rename(a)	=> a.run(context),
			Import(a)	=> a.run(context),
			Export(a)	=> a.run(context),
			// Generic / shared arg shapes.
			Add(a) 		=> Err(unimplemented("env add", "plan phase 5: rnix-parser splice")),
			Remove(a)	=> Err(unimplemented("env remove", "plan phase 5: rnix-parser splice")),
			Pin(a) 		=> Err(unimplemented("env pin", "plan phase 3/4: flake.lock version pin")),
			Unpin(a)	=> Err(unimplemented("env unpin", "plan phase 3/4: flake.lock unpin")),
			List			=> Err(unimplemented("env list", "plan phase 5: enumerate registry")),
			Info(a)		=> Err(unimplemented("env info", "plan phase 6: single-env summary")),
			Deps(a)		=> Err(unimplemented("env deps", "plan phase 6: closure report")),
			Shared(a)	=> Err(unimplemented("env shared", "plan phase 6: N-way shared-set comparison")),
			Check(a)	=> Err(unimplemented("env check", "plan phase 6: env/PATH audit (envcheck)")),
			Clean(a)	=> Err(unimplemented("env clean", "plan phase 5: remove GC root")),
		}
	}
}

/// The arguments for every command.
#[derive(Args, Debug)]
pub struct EnvArgs {
	#[command(subcommand)]
	pub cmd: Cmd,
}

#[derive(Args, Debug)]
pub struct Init {
	/// Name of the env to create.
	pub name: String,
	/// Optional directory to initialize (defaults to the state registry for a
	/// named tool env, or cwd for a project).
	pub path: Option<PathBuf>,
	/// Seed packages (`-p ripgrep -p nodejs=20.11`); folds through `add`.
	#[arg(short = 'p', long = "pkg")]
	pub packages: Vec<Pkg>,
	/// Adopt an existing spec (`pyproject.toml`, `Cargo.toml`, `shell.nix`, …).
	#[arg(long = "from")]
	pub from: Option<PathBuf>,
}
impl RunCmd for Init {
	fn run(self, context: &Context) -> Result<()> {
		Err(unimplemented("env init", "plan phase 3/5: scaffold + state-detect (ADR-2)"))
	}
}

#[derive(Args, Debug)]
pub struct Shell {
	/// Env names to compose. Empty = the cwd project.
	pub names: Vec<String>,
	/// Add a dev env to the union (repeatable; project mode).
	#[arg(short = 'w', long = "with")]
	pub with: Vec<String>,
	/// Enter a pure shell (`nix-shell --pure`).
	#[arg(long)]
	pub pure: bool,
}
impl RunCmd for Shell {
	/// The launcher. Reached by `env shell`, the bare-name sugar, and the no-arg
	/// cwd case. `names` empty ⇒ the cwd project; otherwise compose the named envs
	/// (with `base` prepended, lexically sorted unless `opts.ordered`).
	fn run(self, context: &Context) -> Result<()> {
		Err(unimplemented("env shell", "plan phase 5: compose-dev/compose + GC-rooted enter"))
	}
}

// TODO enable the creation of a new named env from a stack of existing named envs.

#[derive(Args, Debug)]
pub struct Run {
	/// Env names to compose. Empty = the cwd project.
	pub names: Vec<String>,
	/// The command (and its args) to run inside the env; after `--`.
	#[arg(last = true, required = true)]
	pub command: Vec<String>,
}
impl RunCmd for Run {
	fn run(self, context: &Context) -> Result<()> {
		Err(unimplemented("env run", "plan phase 5: nix-shell --run"))
	}
}

/// A set of packages for a target environment
#[derive(Args, Debug)]
pub struct Pkgs {
	/// Target env name.
	pub name: String,
	/// Packages to add/remove (`name` or `name=version`).
	#[arg(required = true)]
	pub packages: Vec<Pkg>,
}

#[derive(Args, Debug)]
pub struct Pin {
	/// Target env name.
	pub name: String,
	/// Packages to pin/unpin. Empty with `--all` operates on the whole closure.
	pub packages: Vec<Pkg>,
	/// Freeze/unfreeze every package (closure-equivalent full pin).
	#[arg(long)]
	pub all: bool,
}

#[derive(Args, Debug)]
pub struct Update {
	/// Target env name.
	pub name: String,
	/// Packages to update; empty = all unpinned packages.
	pub packages: Vec<String>,
}
impl RunCmd for Update {
	fn run(self, context: &Context) -> Result<()> {
		Err(unimplemented("env update", "plan phase 3: flake.lock update"))
	}
}

#[derive(Args, Debug)]
pub struct Rename {
	pub old: String,
	pub new: String,
}
impl RunCmd for Rename {
	fn run(self, context: &Context) -> Result<()> {
    Err(unimplemented("env rename", "plan phase 5: registry relabel"))
	}
}

#[derive(Args, Debug)]
pub struct Import {
	/// Name for the imported env.
	pub name: String, // TODO could be optional if provided by source?
	/// Source file or reference to import from.
	pub source: PathBuf,
}
impl RunCmd for Import {
	fn run(self, context: &Context) -> Result<()> {
    Err(unimplemented("env import", "plan phase 7: import doctor (ADR-2/6)"))
	}
}

#[derive(Args, Debug)]
pub struct Export {
	/// Target env name.
	pub name: String,
	#[command(subcommand)]
	pub target: ExportTarget,
}
impl RunCmd for Export {
	fn run(self, context: &Context) -> Result<()> {
		Err(unimplemented("env export", "plan phase 7: docker/closure extension (ADR-6)"))
	}
}

#[derive(Subcommand, Debug)]
pub enum ExportTarget {
	/// Emit a reproducible OCI image (or a Dockerfile).
	Docker {
		/// Output path (defaults to a Dockerfile in the cwd).
		out: Option<PathBuf>,
	},
	/// Export the Nix closure to a directory.
	Closure { out_dir: PathBuf },
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
