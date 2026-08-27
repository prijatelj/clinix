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

use crate::error::{Result, nyi};

// ---------------------------------------------------------------------------
// Resolved environment
// ---------------------------------------------------------------------------

/// How a name was resolved to its root directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvKind {
    /// A named, reusable, composable env under the clinix state dir.
    Registry,
    /// A project directory (given by path, or the cwd when no name is given).
    Project,
}

/// A resolved environment: a root dir plus how we found it.
#[derive(Debug, Clone)]
pub struct Env {
    /// `None` for an anonymous cwd project; otherwise the resolved name.
    pub name: Option<String>,
    /// Directory containing `shell.nix` / `flake.nix` / `flake.lock`.
    pub root: PathBuf,
    pub kind: EnvKind,
}

/// The one per-source-divergent seam (plan §resolve). Phase 1 is shallow and
/// pure — no nix eval, no lock read:
/// - `Some(name)` registered → [`EnvKind::Registry`] at `state/envs/<name>`;
/// - `Some(name)` that is a path → [`EnvKind::Project`] at that path;
/// - `None` → the cwd project.
pub fn resolve(name: Option<&str>) -> Result<Env> {
    let _ = name;
    Err(nyi("env resolve", "plan phase 5: state registry + cwd detection"))
}

// ---------------------------------------------------------------------------
// Package spec (`name` or `name=version`)
// ---------------------------------------------------------------------------

/// A package with an optional pinned version, parsed from `name[=version]`.
#[derive(Debug, Clone)]
pub struct Pkg {
    pub name: String,
    pub version: Option<String>,
}

impl std::str::FromStr for Pkg {
    type Err = crate::error::ClinixError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(crate::error::ClinixError::InvalidPackage(s.to_string()));
        }
        match s.split_once('=') {
            Some((name, ver)) if !name.is_empty() && !ver.is_empty() => Ok(Pkg {
                name: name.to_string(),
                version: Some(ver.to_string()),
            }),
            Some(_) => Err(crate::error::ClinixError::InvalidPackage(s.to_string())),
            None => Ok(Pkg {
                name: s.to_string(),
                version: None,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Options shared with the bare-name sugar
// ---------------------------------------------------------------------------

/// Composition options read from the global flags (so the `external_subcommand`
/// sugar can still set them). See [`crate::cli`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ShellOpts {
    /// Preserve the given order instead of sorting names lexically.
    pub ordered: bool,
    /// In a project, compose only the runtime env (skip dev tools).
    pub runtime: bool,
}

// ---------------------------------------------------------------------------
// Command tree
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct EnvArgs {
    #[command(subcommand)]
    pub cmd: EnvCmd,
}

/// The unified verb set. Every verb takes an env name (or several, for the
/// compositional ones: `shell`, `run`, `share`).
#[derive(Subcommand, Debug)]
pub enum EnvCmd {
    /// Scaffold a new env, or adopt existing state (shell.nix / flake / uv / cargo).
    Init(InitArgs),
    /// Enter an interactive shell for the composed env(s).
    Shell(ShellArgs),
    /// Run a command inside the composed env(s), non-interactively.
    Run(RunArgs),
    /// Add packages to an env's `shell.nix`.
    Add(PkgArgs),
    /// Remove packages from an env's `shell.nix`.
    Remove(PkgArgs),
    /// Pin package versions in `flake.lock` (`--all` = closure freeze).
    Pin(PinArgs),
    /// Unpin packages back to baseline tracking (`--all` = unfreeze).
    Unpin(PinArgs),
    /// Update unpinned packages to the latest the baseline provides.
    Update(UpdateArgs),
    /// Rename a registered env.
    Rename(RenameArgs),
    /// List registered envs.
    List,
    /// Import an env from another format (shell.nix / flake / .deb / OCI / …).
    Import(ImportArgs),
    /// Export an env to another format.
    Export(ExportArgs),
    /// Summarize a single env (packages, pins, diagnostics).
    Info(TargetArgs),
    /// Dependency/closure report for an env.
    Deps(TargetArgs),
    /// N-way shared-package comparison across several envs.
    Share(NamesArgs),
    /// Environment/PATH audit (the former `envcheck`).
    Check(OptTargetArgs),
    /// Release an env's GC root so its store paths can be collected.
    Clean(TargetArgs),
}

#[derive(Args, Debug)]
pub struct InitArgs {
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

#[derive(Args, Debug)]
pub struct ShellArgs {
    /// Env names to compose. Empty = the cwd project.
    pub names: Vec<String>,
    /// Add a dev env to the union (repeatable; project mode).
    #[arg(short = 'w', long = "with")]
    pub with: Vec<String>,
    /// Enter a pure shell (`nix-shell --pure`).
    #[arg(long)]
    pub pure: bool,
}

#[derive(Args, Debug)]
pub struct RunArgs {
    /// Env names to compose. Empty = the cwd project.
    pub names: Vec<String>,
    /// The command (and its args) to run inside the env; after `--`.
    #[arg(last = true, required = true)]
    pub command: Vec<String>,
}

#[derive(Args, Debug)]
pub struct PkgArgs {
    /// Target env name.
    pub name: String,
    /// Packages to add/remove (`name` or `name=version`).
    #[arg(required = true)]
    pub packages: Vec<Pkg>,
}

#[derive(Args, Debug)]
pub struct PinArgs {
    /// Target env name.
    pub name: String,
    /// Packages to pin/unpin. Empty with `--all` operates on the whole closure.
    pub packages: Vec<Pkg>,
    /// Freeze/unfreeze every package (closure-equivalent full pin).
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Target env name.
    pub name: String,
    /// Packages to update; empty = all unpinned packages.
    pub packages: Vec<String>,
}

#[derive(Args, Debug)]
pub struct RenameArgs {
    pub old: String,
    pub new: String,
}

#[derive(Args, Debug)]
pub struct ImportArgs {
    /// Name for the imported env.
    pub name: String,
    /// Source file or reference to import from.
    pub source: PathBuf,
}

#[derive(Args, Debug)]
pub struct ExportArgs {
    /// Target env name.
    pub name: String,
    #[command(subcommand)]
    pub target: ExportTarget,
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
pub struct TargetArgs {
    pub name: String,
}

/// A single optional env name (defaults to the cwd project).
#[derive(Args, Debug)]
pub struct OptTargetArgs {
    pub name: Option<String>,
}

/// One or more env names.
#[derive(Args, Debug)]
pub struct NamesArgs {
    #[arg(required = true)]
    pub names: Vec<String>,
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Route an `env` subcommand. `opts` carries the global composition flags so
/// `shell`/`run` see the same `-o`/`-r` as the bare-name sugar.
pub fn dispatch(args: EnvArgs, opts: ShellOpts) -> Result<()> {
    match args.cmd {
        EnvCmd::Init(a) => init(a),
        EnvCmd::Shell(a) => {
            // Fold the explicit `env shell` form through the same launcher as
            // the sugar; `--with`/`--pure` are not expressible in sugar form.
            let _ = (&a.with, a.pure);
            shell(a.names, opts)
        }
        EnvCmd::Run(a) => run(a, opts),
        EnvCmd::Add(a) => add(a),
        EnvCmd::Remove(a) => remove(a),
        EnvCmd::Pin(a) => pin(a),
        EnvCmd::Unpin(a) => unpin(a),
        EnvCmd::Update(a) => update(a),
        EnvCmd::Rename(a) => rename(a),
        EnvCmd::List => list(),
        EnvCmd::Import(a) => import(a),
        EnvCmd::Export(a) => export(a),
        EnvCmd::Info(a) => info(a),
        EnvCmd::Deps(a) => deps(a),
        EnvCmd::Share(a) => share(a),
        EnvCmd::Check(a) => check(a),
        EnvCmd::Clean(a) => clean(a),
    }
}

// ---------------------------------------------------------------------------
// Verb handlers (all NotYetImplemented; signatures are the real contract)
// ---------------------------------------------------------------------------

/// The launcher. Reached by `env shell`, the bare-name sugar, and the no-arg
/// cwd case. `names` empty ⇒ the cwd project; otherwise compose the named envs
/// (with `base` prepended, lexically sorted unless `opts.ordered`).
pub fn shell(names: Vec<String>, opts: ShellOpts) -> Result<()> {
    let _ = (names, opts);
    Err(nyi("env shell", "plan phase 5: compose-dev/compose + GC-rooted enter"))
}

fn run(a: RunArgs, opts: ShellOpts) -> Result<()> {
    let _ = (a, opts);
    Err(nyi("env run", "plan phase 5: nix-shell --run"))
}

fn init(a: InitArgs) -> Result<()> {
    let _ = a;
    Err(nyi("env init", "plan phase 3/5: scaffold + state-detect (ADR-2)"))
}

fn add(a: PkgArgs) -> Result<()> {
    let _ = a;
    Err(nyi("env add", "plan phase 5: rnix-parser splice"))
}

fn remove(a: PkgArgs) -> Result<()> {
    let _ = a;
    Err(nyi("env remove", "plan phase 5: rnix-parser splice"))
}

fn pin(a: PinArgs) -> Result<()> {
    let _ = a;
    Err(nyi("env pin", "plan phase 3/4: flake.lock version pin"))
}

fn unpin(a: PinArgs) -> Result<()> {
    let _ = a;
    Err(nyi("env unpin", "plan phase 3/4: flake.lock unpin"))
}

fn update(a: UpdateArgs) -> Result<()> {
    let _ = a;
    Err(nyi("env update", "plan phase 3: flake.lock update"))
}

fn rename(a: RenameArgs) -> Result<()> {
    let _ = a;
    Err(nyi("env rename", "plan phase 5: registry relabel"))
}

fn list() -> Result<()> {
    Err(nyi("env list", "plan phase 5: enumerate registry"))
}

fn import(a: ImportArgs) -> Result<()> {
    let _ = a;
    Err(nyi("env import", "plan phase 7: import doctor (ADR-2/6)"))
}

fn export(a: ExportArgs) -> Result<()> {
    let _ = a;
    Err(nyi("env export", "plan phase 7: docker/closure extension (ADR-6)"))
}

fn info(a: TargetArgs) -> Result<()> {
    let _ = a;
    Err(nyi("env info", "plan phase 6: single-env summary"))
}

fn deps(a: TargetArgs) -> Result<()> {
    let _ = a;
    Err(nyi("env deps", "plan phase 6: closure report"))
}

fn share(a: NamesArgs) -> Result<()> {
    let _ = a;
    Err(nyi("env share", "plan phase 6: N-way shared-set comparison"))
}

fn check(a: OptTargetArgs) -> Result<()> {
    let _ = a;
    Err(nyi("env check", "plan phase 6: env/PATH audit (envcheck)"))
}

fn clean(a: TargetArgs) -> Result<()> {
    let _ = a;
    Err(nyi("env clean", "plan phase 5: remove GC root"))
}
