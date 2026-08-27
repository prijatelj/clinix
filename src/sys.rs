//! The `sys` scope: manage a NixOS system as a declarative `system.nix`
//! configuration (no nix-channels). Distinct from `env` because no
//! environment/project manager touches the OS level, and its posture is
//! declarative-config with the strongest rollback in the corpus (system
//! generations). v0.1 scaffolds `init`; the rest returns `NotYetImplemented`.

use clap::{Args, Subcommand};

use crate::error::{Result, nyi};

#[derive(Args, Debug)]
pub struct SysArgs {
    #[command(subcommand)]
    pub cmd: SysCmd,
}

#[derive(Subcommand, Debug)]
pub enum SysCmd {
    /// Initialize a `system.nix` configuration.
    Init,
    /// Build and switch to a new system generation.
    Switch,
    /// Roll back to the previous system generation.
    Rollback,
    /// Update the pinned inputs (`flake.lock`).
    Update,
    /// Show system generations / diagnostics.
    Info,
}

pub fn dispatch(args: SysArgs) -> Result<()> {
    match args.cmd {
        SysCmd::Init => Err(nyi("sys init", "plan phase 8: scaffold system.nix")),
        SysCmd::Switch => Err(nyi("sys switch", "plan phase 8")),
        SysCmd::Rollback => Err(nyi("sys rollback", "plan phase 8")),
        SysCmd::Update => Err(nyi("sys update", "plan phase 8")),
        SysCmd::Info => Err(nyi("sys info", "plan phase 8")),
    }
}
