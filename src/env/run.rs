//! `run`: run a command inside a composed env, non-interactively.

use clap::Args;

use crate::env::{Context, RunCmd};
use crate::error::{Result, unimplemented};

#[derive(Args, Debug)]
pub struct Run {
	/// Envs to compose, as a stack — same rules as [`super::shell::Shell`]
	/// (`.` = cwd project, empty ⇒ `[.]`).
	pub names: Vec<String>,
	/// The command (and its args) to run inside the env; after `--`.
	#[arg(last = true, required = true)]
	pub command: Vec<String>,
}
impl RunCmd for Run {
	fn run(self, _context: &Context) -> Result<()> {
		Err(unimplemented("env run", "plan phase 5: nix-shell --run"))
	}
}
