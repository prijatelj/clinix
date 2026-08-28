//! `new`: create a **registry** env by merging existing envs (`--from A B C`).

use clap::Args;

use crate::env::{Context, RunCmd};
use crate::error::{Result, unimplemented};

/// Create a new **registry** env by merging existing envs. Distinct from
/// [`super::init::Init`], which scaffolds a **project** env in a directory: `new`
/// writes a reusable, named env into the state registry. The `--from` order is
/// the merge (stack) order. Sources are registry names; the cwd-project token
/// `.` is not valid here, since a registry env must be portable.
#[derive(Args, Debug)]
pub struct New {
	/// Name of the new registry env.
	pub name: String,
	/// Existing registry envs to merge, in stack order (`--from A B C`).
	#[arg(long = "from", num_args = 1.., required = true)]
	pub from: Vec<String>,
}
impl RunCmd for New {
	fn run(self, _context: &Context) -> Result<()> {
		Err(unimplemented(
			"env new",
			"plan phase 5: merge envs → registry item",
		))
	}
}
