//! Typed errors for clinix. Every unimplemented leaf returns
//! [`ClinixError::Unimplemented`] so `--help` and the command tree stay
//! honest while the crate is built up incrementally.

/// Crate-wide result alias. The CLI boundary in `main.rs` maps this to an
/// [`std::process::ExitCode`]; `anyhow` is reserved for the outermost boundary.
pub type Result<T> = std::result::Result<T, ClinixError>;

#[derive(thiserror::Error, Debug)]
pub enum ClinixError {
    #[error("`{command}` is not yet implemented ({note})")]
    Unimplemented {
        command: String,
        note: &'static str,
    },

    /// A name that is neither a registered env nor a path to a project dir.
    #[error("environment `{0}` is not registered and is not a project directory")]
    UnknownEnv(String),

    /// `init` could not decide what to scaffold from the existing directory
    /// state. `detail` names exactly what is undetermined (user-facing).
    #[error("cannot initialize `{path}`: {detail}")]
    AmbiguousInit { path: String, detail: String },

    #[error("invalid package spec `{0}` — expected `name` or `name=version`")]
    InvalidPackage(String),
}

/// Construct a [`ClinixError::NotYetImplemented`] for `command`, tagged with a
/// short tracking `note` (e.g. the plan phase that will implement it).
pub fn unimplemented(command: impl Into<String>, note: &'static str) -> ClinixError {
    ClinixError::Unimplemented {
        command: command.into(),
        note,
    }
}
