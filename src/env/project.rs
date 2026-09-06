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

#[cfg(test)]
mod tests {
	use super::*;
	use crate::env::Kind;

	// A minimal one-input lock (nixpkgs tracking nixos-26.05), same shape as the
	// test fixtures elsewhere — parses without validating rev/hash strictly.
	const LOCK_JSON: &str = r#"{
  "nodes": {
    "nixpkgs": {
      "locked": { "narHash": "sha256-x", "owner": "NixOS", "repo": "nixpkgs", "rev": "abc123", "type": "github" },
      "original": { "owner": "NixOS", "ref": "nixos-26.05", "repo": "nixpkgs", "type": "github" }
    },
    "root": { "inputs": { "nixpkgs": "nixpkgs" } }
  },
  "root": "root",
  "version": 7
}"#;

	/// A single-file `shell.nix` as `init --single-file` writes it: the lock JSON
	/// embedded via `builtins.fromJSON (''<json>'')`, with no sibling `flake.lock`.
	fn single_file_shell_nix(lock_json: &str) -> String {
		format!("let lock = builtins.fromJSON (''{lock_json}''); in pkgs.mkShell {{ }}\n")
	}

	fn env_at(dir: &std::path::Path) -> Env {
		Env {
			name: None,
			root: dir.to_path_buf(),
			kind: Kind::Project,
		}
	}

	#[test]
	fn extract_embedded_lock_recovers_the_json() {
		let shell = single_file_shell_nix(LOCK_JSON);
		let json = extract_embedded_lock(&shell).unwrap();
		let lock = FlakeLock::from_json(&json).unwrap();
		assert!(lock.nodes.contains_key("nixpkgs"));
	}

	#[test]
	fn extract_embedded_lock_errors_when_there_is_no_here_string() {
		assert!(extract_embedded_lock("pkgs.mkShell { }\n").is_err());
	}

	#[test]
	fn load_reads_the_embedded_lock_when_no_flake_lock() {
		let dir = tempfile::tempdir().unwrap();
		fs::write(
			dir.path().join("shell.nix"),
			single_file_shell_nix(LOCK_JSON),
		)
		.unwrap();
		// Deliberately no flake.lock: `load` must fall back to the embedded lock.
		let project = Project::load(env_at(dir.path())).unwrap();
		assert!(project.lock.nodes.contains_key("nixpkgs"));
	}

	#[test]
	fn load_prefers_the_flake_lock_file_when_present() {
		let dir = tempfile::tempdir().unwrap();
		// shell.nix carries no embedded lock; the flake.lock file supplies it.
		fs::write(dir.path().join("shell.nix"), "pkgs.mkShell { }\n").unwrap();
		fs::write(dir.path().join("flake.lock"), LOCK_JSON).unwrap();
		let project = Project::load(env_at(dir.path())).unwrap();
		assert!(project.lock.nodes.contains_key("nixpkgs"));
	}
}
