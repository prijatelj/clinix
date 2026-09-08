//! clinix — a single CLI over Nix environments (library crate).
//!
//! Two top-level subcommands: [`sys`] manages a NixOS system (`system.nix`), and
//! [`env`] manages every non-system environment (the former dev/user/run scopes,
//! unified). A bare name list — `clinix python rust` — is sugar for
//! `clinix env shell python rust`.
//!
//! The binary (`main.rs`) is a thin wrapper; these modules are `pub` so
//! integration tests in `tests/` can exercise the public API.

pub mod cli;
pub mod disk;
pub mod env;
pub mod error;
pub mod ext;
pub mod model;
pub mod nix;
pub mod progress;
pub mod sys;
