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
	/// Enter the Nth **prior** root version instead of the current (`1` = the version
	/// immediately before current). Enters the stored recipe directly — offline-capable
	/// — and does not mint a new version. See `env roots <name>` to list versions.
	#[arg(long, conflicts_with = "root_version")]
	pub prior: Option<usize>,
	/// Enter an **exact** root version by its id (as shown by `env roots <name>`).
	#[arg(long = "root-version", conflicts_with = "prior")]
	pub root_version: Option<u64>,
}
impl RunCmd for Shell {
	/// The launcher. Reached by `env shell`, the bare-name sugar, and the no-arg
	/// cwd case. GC-roots the env's `shell.nix` and enters it interactively.
	fn run(self, context: &Context) -> Result<()> {
		let version = super::version_select(self.prior, self.root_version);
		launch(context, &self.names, self.pure, None, version)?;
		Ok(())
	}
}
