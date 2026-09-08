//! Execution: the two verbs that instantiate a composition and launch it —
//! `shell` (enter it interactively) and `run` (run a command inside it). Both go
//! through the shared [`crate::env::launch`]; they differ only in whether a
//! `--run` command is passed.

mod run;
mod shell;

pub use run::Run;
pub use shell::Shell;
