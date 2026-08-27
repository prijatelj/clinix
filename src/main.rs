//! clinix — a single CLI over Nix environments.
//!
//! Two top-level subcommands: [`sys`](crate::sys) manages a NixOS system
//! (`system.nix`), and [`env`](crate::env) manages every non-system
//! environment (the former dev/user/run scopes, unified). A bare name list —
//! `clinix python rust` — is sugar for `clinix env shell python rust`.

mod cli;
mod env;
mod error;
mod sys;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.dispatch() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("clinix: error: {e}");
            ExitCode::FAILURE
        }
    }
}
