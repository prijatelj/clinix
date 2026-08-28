//! `import`: adopt an env from another format (shell.nix / flake / .deb / OCI).

use std::path::PathBuf;

use clap::Args;

use crate::env::{Context, RunCmd};
use crate::error::{Result, unimplemented};

#[derive(Args, Debug)]
pub struct Import {
	/// Name for the imported env.
	pub name: String, // TODO could be optional if provided by source?
	/// Source file or reference to import from.
	pub source: PathBuf,
}
impl RunCmd for Import {
	fn run(self, _context: &Context) -> Result<()> {
		Err(unimplemented(
			"env import",
			"plan phase 7: import doctor (ADR-2/6)",
		))
	}
}
