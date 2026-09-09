//! Execution: the two verbs that instantiate a composition and launch it —
//! `shell` (enter it interactively) and `run` (run a command inside it). Both go
//! through the shared [`crate::env::launch`]; they differ only in whether a
//! `--run` command is passed.

mod run;
mod shell;

pub use run::Run;
pub use shell::Shell;

use crate::env::registry::VersionSelect;

/// Map the mutually-exclusive `--prior N` / `--root-version SEQ` flags (shared by
/// `shell` and `run`) to a [`VersionSelect`]. Neither ⇒ [`VersionSelect::Current`].
pub(crate) fn version_select(prior: Option<usize>, exact: Option<u64>) -> VersionSelect {
	match (prior, exact) {
		(Some(n), _) => VersionSelect::Prior(n),
		(_, Some(seq)) => VersionSelect::Exact(seq),
		_ => VersionSelect::Current,
	}
}
