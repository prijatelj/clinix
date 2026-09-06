//! Typed errors for clinix. Every unimplemented leaf returns
//! [`ClinixError::Unimplemented`] so `--help` and the command tree stay
//! honest while the crate is built up incrementally.

/// Crate-wide result alias. The CLI boundary in `main.rs` maps this to an
/// [`std::process::ExitCode`]; `anyhow` is reserved for the outermost boundary.
pub type Result<T> = std::result::Result<T, ClinixError>;

#[derive(thiserror::Error, Debug)]
pub enum ClinixError {
	#[error("`{command}` is not yet implemented ({note})")]
	Unimplemented { command: String, note: &'static str },

	/// A name that is neither a registered env nor a path to a project dir.
	#[error("environment `{0}` is not registered and is not a project directory")]
	UnknownEnv(String),

	/// A registry env already exists at the requested name (e.g. `rename` target).
	#[error("environment `{0}` already exists")]
	EnvExists(String),

	/// A registry name that is not a valid single path component.
	#[error("invalid environment name `{name}`: {detail}")]
	InvalidEnvName { name: String, detail: &'static str },

	/// `init` could not decide what to scaffold from the existing directory
	/// state. `detail` names exactly what is undetermined (user-facing).
	#[error("cannot initialize `{path}`: {detail}")]
	AmbiguousInit { path: String, detail: String },

	#[error("invalid package spec `{0}` — expected `name` or `name=version`")]
	InvalidPackage(String),

	/// A malformed git revision (not 40/64-char lowercase hex).
	#[error("invalid git revision `{0}` — expected 40- or 64-char lowercase hex")]
	InvalidRev(String),

	/// A malformed nix content hash.
	#[error("invalid narHash `{0}` — expected `<algo>-<base64>` or `<algo>:<base32>`")]
	InvalidNarHash(String),

	/// `flake.lock` (or another JSON document) failed to parse/serialize.
	#[error("failed to parse flake.lock: {0}")]
	Lock(#[from] serde_json::Error),

	/// A filesystem operation failed.
	#[error("i/o error: {0}")]
	Io(#[from] std::io::Error),

	/// A `nix*`/`git` CLI invocation exited nonzero (the shell-out boundary).
	#[error("command failed ({status}): {cmd}\n{stderr}")]
	Nix {
		cmd: String,
		status: String,
		stderr: String,
	},

	/// A command run inside an env (`run … -- cmd`) exited nonzero. clinix mirrors
	/// the command's exit code (like `nix-shell --run`), so `run` is
	/// exit-transparent for scripts/CI. `main.rs` exits with `code` and prints no
	/// clinix error line — the command already reported its own failure.
	#[error("command exited with status {code}")]
	CommandFailed { code: i32 },

	/// A ref could not be resolved to a revision (command succeeded, no match).
	#[error("could not resolve: {0}")]
	Resolve(String),

	/// `config.toml` (or a file it `use`-imports) failed to load, parse, or formed
	/// an import cycle.
	#[error("config: {0}")]
	Config(String),

	/// Editing a `shell.nix` failed (parse error, or no `packages` list found).
	#[error("shell.nix: {0}")]
	ShellNix(String),
}

/// Construct a [`ClinixError::Unimplemented`] for `command`, tagged with a
/// short tracking `note` (e.g. the plan phase that will implement it).
pub fn unimplemented(command: impl Into<String>, note: &'static str) -> ClinixError {
	ClinixError::Unimplemented {
		command: command.into(),
		note,
	}
}
