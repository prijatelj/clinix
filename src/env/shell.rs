//! `shell`: enter an interactive composed env — the launcher.

use clap::Args;

use crate::env::{Context, RunCmd};
use crate::error::{Result, unimplemented};

#[derive(Args, Debug)]
pub struct Shell {
	/// Envs to compose, as a stack. Empty ⇒ `[.]` (the cwd project). The token
	/// `.` denotes the cwd project and may appear anywhere in the stack (usually
	/// first); every other name resolves via [`crate::env::resolve`].
	pub names: Vec<String>,
	/// Enter a pure shell (`nix-shell --pure`).
	#[arg(long)]
	pub pure: bool,
}
impl RunCmd for Shell {
	/// The launcher. Reached by `env shell`, the bare-name sugar, and the no-arg
	/// cwd case. Empty `names` ⇒ `[.]`; the `.` token composes the cwd project as
	/// an ordinary node anywhere in the stack. `base` is prepended, and the stack
	/// is lexically sorted unless `options.ordered`.
	fn run(self, _context: &Context) -> Result<()> {
		Err(unimplemented(
			"env shell",
			"plan phase 5: compose-dev/compose + GC-rooted enter",
		))
	}
}
