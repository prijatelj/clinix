//! Typed models parsed from clinix's sources of truth (plan phase 2).
//!
//! [`lock`] models `flake.lock` (versions); [`newtypes`] holds validated
//! wrappers (`Rev`, `NarHash`) used at the boundary where a command needs a
//! well-formed revision or hash. `pyproject`/`clinix_env` land in later phases.

pub mod lock;
pub mod newtypes;
