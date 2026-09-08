//! The `env` scope: one unified surface over the former dev/user/run scopes.
//!
//! An environment is identified by a **name** that [`resolve`] maps to a **root
//! directory** holding `shell.nix` + `flake.lock` (the single source of truth,
//! per the locked design). Two population sources — a **registry** of named,
//! composable tool envs under the state dir, and **project** directories
//! resolved by path/cwd — are what the dev+run+user merge collapses to: they
//! differ only in resolution, not in command surface.
//!
//! Layout: [`env`](self) holds the shared primitives (verb dispatch [`Cmd`], the
//! [`RunCmd`] trait, [`Context`], [`resolve`], the env-name selectors, and
//! `rename`); every concrete verb lives in its own sibling submodule and this
//! module wires them together.

mod clean;
pub mod config;
mod diagnostics;
mod env;
mod execution;
mod export;
mod import;
mod init;
pub mod naming;
mod new;
mod nix_edit;
pub mod nix_expr;
mod pkgs;
pub mod project;
pub mod registry;
pub mod seeds;

pub use diagnostics::{Deps, Info, InfoVerb, context_report};
pub use env::{
	Cmd, Context, Env, EnvArgs, Kind, OptionalTarget, Rename, RunCmd, ShellOptions, Target,
	Targets, resolve,
};
pub(crate) use env::{compose_nodes, launch};
pub use execution::{Run, Shell};
pub use export::{Export, ExportTarget};
pub use import::Import;
pub use init::Init;
pub use new::New;
pub use pkgs::{Flake, Pkg, Pkgs, Update};
