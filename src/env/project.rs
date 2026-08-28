//! The shared working object every `env` command acts on.
//!
//! Interface catalog (plan): each subcommand is "read `flake.lock` → resolve →
//! act" over one `struct Project`. [`crate::env::resolve`] answers *where* an env
//! is ([`Env`]); `Project` is *what* was loaded from there — the root plus the
//! parsed lock — so command logic operates on typed state, not paths.

use std::fs;

use crate::env::Env;
use crate::error::Result;
use crate::model::lock::FlakeLock;

/// A resolved env loaded into memory: its [`Env`] plus the parsed `flake.lock`.
#[derive(Debug, Clone)]
pub struct Project {
	pub env: Env,
	pub lock: FlakeLock,
}

impl Project {
	/// Load an env's `flake.lock` (at `env.root/flake.lock`) into a typed model.
	pub fn load(env: Env) -> Result<Self> {
		let text = fs::read_to_string(env.root.join("flake.lock"))?; // io::Error → ClinixError::Io
		let lock = FlakeLock::from_json(&text)?;
		Ok(Project { env, lock })
	}
}
