//! The `sys` scope: manage a NixOS system as a declarative `system.nix`
//! configuration (no nix-channels). Distinct from `env` because no
//! environment/project manager touches the OS level, and its posture is
//! declarative-config with the strongest rollback in the corpus (system
//! generations). v0.1 scaffolds `init`; the rest returns `NotYetImplemented`.

use clap::{Args, Subcommand};

use crate::error::{Result, unimplemented};

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
		SysCmd::Init => Err(unimplemented(
			"sys init",
			"plan phase 8: scaffold system.nix",
		)),
		SysCmd::Switch => Err(unimplemented("sys switch", "plan phase 8")),
		SysCmd::Rollback => Err(unimplemented("sys rollback", "plan phase 8")),
		SysCmd::Update => Err(unimplemented("sys update", "plan phase 8")),
		SysCmd::Info => Err(unimplemented("sys info", "plan phase 8")),
	}
}
