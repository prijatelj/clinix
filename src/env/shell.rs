//! `shell`: enter an interactive composed env — the launcher.

use clap::Args;

use crate::env::{Context, RunCmd, launch};
use crate::error::Result;

#[derive(Args, Debug)]
pub struct Shell {
	/// Envs to compose, as a stack. Empty ⇒ `[.]` (the cwd project). The token
	/// `.` denotes the cwd project; every other name resolves via
	/// [`crate::env::resolve`]. (Multi-env composition is phase 5; today a single
	/// env or the cwd project is entered.)
	pub names: Vec<String>,
	/// Enter a pure shell (`nix-shell --pure`).
	#[arg(long)]
	pub pure: bool,
}
impl RunCmd for Shell {
	/// The launcher. Reached by `env shell`, the bare-name sugar, and the no-arg
	/// cwd case. GC-roots the env's `shell.nix` and enters it interactively.
	fn run(self, _context: &Context) -> Result<()> {
		launch(&self.names, self.pure, None)?;
		Ok(())
	}
}
