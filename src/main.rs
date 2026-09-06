//! clinix binary entry point — a thin wrapper over the `clinix` library crate.

use std::process::ExitCode;

use clap::Parser;

use clinix::cli::Cli;
use clinix::error::ClinixError;

fn main() -> ExitCode {
	match Cli::parse().dispatch() {
		Ok(()) => ExitCode::SUCCESS,
		// A command run inside an env (`run … -- cmd`) failed: mirror its exit code
		// and print no clinix error — the command already reported its own failure.
		Err(ClinixError::CommandFailed { code }) => ExitCode::from(code.clamp(1, 255) as u8),
		Err(e) => {
			eprintln!("clinix: error: {e}");
			ExitCode::FAILURE
		}
	}
}
