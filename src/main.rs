//! clinix binary entry point — a thin wrapper over the `clinix` library crate.

use std::process::ExitCode;

use clap::Parser;

use clinix::cli::Cli;

fn main() -> ExitCode {
    match Cli::parse().dispatch() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("clinix: error: {e}");
            ExitCode::FAILURE
        }
    }
}
