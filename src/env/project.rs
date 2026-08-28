//! The shared working object every `env` command acts on.
//!
//! Interface catalog (plan): each subcommand is "read `flake.lock` → resolve →
//! act" over one `struct Project`. [`crate::env::resolve`] answers *where* an env
//! is ([`Env`]); `Project` is *what* was loaded from there — the root plus the
//! parsed lock — so command logic operates on typed state, not paths.

use std::fs;

use crate::env::Env;
use crate::error::{ClinixError, Result};
use crate::model::lock::FlakeLock;

/// A resolved env loaded into memory: its [`Env`] plus the parsed lock.
#[derive(Debug, Clone)]
pub struct Project {
	pub env: Env,
	pub lock: FlakeLock,
}

impl Project {
	/// Load an env's lock into a typed model, from either `flake.lock` (the
	/// default two-file form) or — when there is no `flake.lock` — the lock
	/// embedded in `shell.nix` (the single-file form). Both hold the identical
	/// `flake.lock` JSON.
	pub fn load(env: Env) -> Result<Self> {
		let flake_lock = env.root.join("flake.lock");
		let text = if flake_lock.exists() {
			fs::read_to_string(flake_lock)? // io::Error → ClinixError::Io
		} else {
			extract_embedded_lock(&fs::read_to_string(env.root.join("shell.nix"))?)?
		};
		let lock = FlakeLock::from_json(&text)?;
		Ok(Project { env, lock })
	}
}

/// Extract the `flake.lock` JSON embedded in a single-file `shell.nix` — the
/// `builtins.fromJSON ''<json>''` here-string clinix writes for `init
/// --single-file` — reversing the nix indented-string escapes.
fn extract_embedded_lock(shell_nix: &str) -> Result<String> {
	const OPEN: &str = "builtins.fromJSON (''";
	let start = shell_nix
		.find(OPEN)
		.map(|i| i + OPEN.len())
		.ok_or_else(|| {
			ClinixError::Resolve("shell.nix has no flake.lock (file or embedded)".into())
		})?;
	let rest = &shell_nix[start..];
	let end = rest
		.find("''")
		.ok_or_else(|| ClinixError::Resolve("shell.nix: unterminated embedded lock".into()))?;
	Ok(rest[..end]
		.replace("''${", "${")
		.replace("'''", "''")
		.trim()
		.to_string())
}
