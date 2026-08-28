//! `export`: emit an env to another format (Docker image / Nix closure).

use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::env::{Context, RunCmd};
use crate::error::{Result, unimplemented};

#[derive(Args, Debug)]
pub struct Export {
	/// Target env name.
	pub name: String,
	#[command(subcommand)]
	pub target: ExportTarget,
}
impl RunCmd for Export {
	fn run(self, _context: &Context) -> Result<()> {
		Err(unimplemented(
			"env export",
			"plan phase 7: docker/closure extension (ADR-6)",
		))
	}
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
