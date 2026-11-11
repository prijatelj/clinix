//! The shared working object every `env` command acts on.
//!
//! Interface catalog (plan): each subcommand is "read `flake.lock` → resolve →
//! act" over one `struct Project`. [`crate::env::resolve`] answers *where* an env
//! is ([`Env`]); `Project` is *what* was loaded from there — the root, where its
//! lock lives ([`LockSource`]), and the parsed lock — so command logic operates on
//! typed state, not paths.
//!
//! The lock verbs (`update`/`outdated`/`verify`/`info`/`flake …`) also accept a
//! **bare lockfile** by path — a lock no `shell.nix` owns, such as one under
//! `<locks>/`. [`LockTarget`] is that wider target.

use std::fs;
use std::path::{Path, PathBuf};

use crate::env::Env;
use crate::env::config::Config;
use crate::error::{ClinixError, Result, unimplemented};
use crate::model::lock::FlakeLock;

/// Where a directory-backed env keeps its lock — the one classification every
/// lock verb reports from, so each case gets its own accurate message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockSource {
	/// A sibling `flake.lock` file (the default two-file form).
	File(PathBuf),
	/// The lock embedded in this `shell.nix` (the `--shell-only` form).
	Embedded(PathBuf),
	/// This `shell.nix` exists but carries no lock in either form — it evaluates
	/// against whatever nixpkgs it imports itself (e.g. `<nixpkgs>`).
	NoLock(PathBuf),
	/// This directory holds neither a `shell.nix` nor a `flake.lock`.
	NoShell(PathBuf),
}

/// The marker `init --shell-only` writes around the embedded lock JSON.
const EMBED_OPEN: &str = "builtins.fromJSON (''";

impl LockSource {
	/// Classify `env`'s lock. Reads `shell.nix` only when there is no `flake.lock`.
	pub fn of(env: &Env) -> Result<Self> {
		let flake_lock = env.root.join("flake.lock");
		if flake_lock.is_file() {
			return Ok(Self::File(flake_lock));
		}
		let shell_nix = env.root.join("shell.nix");
		if !shell_nix.is_file() {
			return Ok(Self::NoShell(env.root.clone()));
		}
		Ok(if fs::read_to_string(&shell_nix)?.contains(EMBED_OPEN) {
			Self::Embedded(shell_nix)
		} else {
			Self::NoLock(shell_nix)
		})
	}

	/// Parse the lock, or the case-specific error for a source that has none.
	pub fn read(&self) -> Result<FlakeLock> {
		match self {
			Self::File(path) => FlakeLock::from_json(&fs::read_to_string(path)?),
			Self::Embedded(shell_nix) => {
				FlakeLock::from_json(&extract_embedded_lock(&fs::read_to_string(shell_nix)?)?)
			}
			Self::NoLock(_) | Self::NoShell(_) => Err(self.missing()),
		}
	}

	/// The writable lockfile, or why this source cannot be written. Lock-mutating
	/// verbs call this **before** any network work, so a refusal costs nothing.
	pub fn writable(&self, verb: &str) -> Result<&Path> {
		match self {
			Self::File(path) => Ok(path),
			// Writing a `flake.lock` beside an embedded lock would create a second,
			// drifting source of truth.
			Self::Embedded(_) => Err(unimplemented(
				format!("{verb} on a shell-only env"),
				"the lock is embedded in shell.nix; re-embedding is a later slice — \
				 `clinix env flake <env> wrap-shell` expands it into shell.nix + flake.lock",
			)),
			Self::NoLock(_) | Self::NoShell(_) => Err(self.missing()),
		}
	}

	/// The error for a source with no lock at all ([`Self::NoLock`]/[`Self::NoShell`]).
	fn missing(&self) -> ClinixError {
		ClinixError::Target(match self {
			Self::NoLock(shell_nix) => format!(
				"{} has no lock — no sibling flake.lock and none embedded, so it \
				 evaluates against whatever nixpkgs it imports. Lock it by rewriting it \
				 to the clinix-native form (`clinix env init` writes one), or \
				 `clinix env flake <dir> wrap-shell` (locks `nix develop` only)",
				shell_nix.display()
			),
			Self::NoShell(dir) => format!(
				"no shell.nix or flake.lock in {} — not a clinix env{}",
				dir.display(),
				lockfile_hint(dir)
			),
			Self::File(p) | Self::Embedded(p) => format!("{} has a lock", p.display()),
		})
	}
}

/// For a directory with no env in it: name any lockfiles it holds, since a lock
/// directory (e.g. `<locks>/`) is the likeliest reason to have pointed here.
fn lockfile_hint(dir: &Path) -> String {
	let is_lock =
		|p: &Path| fs::read_to_string(p).is_ok_and(|text| FlakeLock::from_json(&text).is_ok());
	let mut locks: Vec<String> = fs::read_dir(dir)
		.into_iter()
		.flatten()
		.flatten()
		.map(|e| e.path())
		.filter(|p| p.is_file() && is_lock(p))
		.filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
		.collect();
	if locks.is_empty() {
		return String::new();
	}
	locks.sort();
	format!(
		". It holds lockfile(s) {} — target one by path (e.g. `{}`)",
		locks.join(", "),
		dir.join(&locks[0]).display()
	)
}

/// Write a lock as canonical JSON. `fs::write` follows a symlink to its target,
/// so a symlinked lock (e.g. one shared with a system config) is updated in place.
fn write_lock(path: &Path, lock: &FlakeLock) -> Result<()> {
	fs::write(path, lock.to_json())?;
	Ok(())
}

/// A resolved env loaded into memory: its [`Env`], where its lock lives, and the
/// parsed lock.
#[derive(Debug, Clone)]
pub struct Project {
	pub env: Env,
	pub source: LockSource,
	pub lock: FlakeLock,
}

impl Project {
	/// Load an env's lock into a typed model, from either `flake.lock` (the
	/// default two-file form) or — when there is no `flake.lock` — the lock
	/// embedded in `shell.nix` (the shell-only form). Both hold the identical
	/// `flake.lock` JSON. Any other [`LockSource`] is its own descriptive error.
	pub fn load(env: Env) -> Result<Self> {
		let source = LockSource::of(&env)?;
		let lock = source.read()?;
		Ok(Project { env, source, lock })
	}

	/// Write this env's (possibly edited) lock back to its `flake.lock`. Refuses a
	/// shell-only env (see [`LockSource::writable`]); mutating verbs check that
	/// first, before any network work.
	pub fn save_lock(&self) -> Result<()> {
		write_lock(self.source.writable("writing the lock")?, &self.lock)
	}
}

/// What a lock verb acts on: an env's lock, or a bare lockfile named by path.
#[derive(Debug, Clone)]
pub enum LockTarget {
	Env(Project),
	/// A lockfile no `shell.nix` owns — nothing to reconcile it with.
	File {
		path: PathBuf,
		lock: FlakeLock,
	},
}

impl LockTarget {
	/// Resolve a lock verb's token. A token naming an existing **file** is a bare
	/// lockfile (identified by content, so `flake.lock`, `unstable.lock`, and
	/// `x.json` all work); a `*.nix` file is refused (its directory is the env).
	/// Anything else resolves as an env via [`crate::env::resolve`].
	pub fn resolve(cfg: &Config, token: &str) -> Result<Self> {
		let path = Path::new(token);
		if path.is_file() {
			if token.ends_with(".nix") {
				return Err(ClinixError::Target(format!(
					"{token} is a nix file, not a lock — target its directory (the env) \
					 or a lockfile"
				)));
			}
			let path = fs::canonicalize(path)?;
			let lock = FlakeLock::from_json(&fs::read_to_string(&path)?).map_err(|e| {
				ClinixError::Target(format!("{} is not a flake lock: {e}", path.display()))
			})?;
			return Ok(Self::File { path, lock });
		}
		Ok(Self::Env(Project::load(crate::env::resolve(
			cfg,
			Some(token),
		)?)?))
	}

	pub fn lock(&self) -> &FlakeLock {
		match self {
			Self::Env(project) => &project.lock,
			Self::File { lock, .. } => lock,
		}
	}

	pub fn lock_mut(&mut self) -> &mut FlakeLock {
		match self {
			Self::Env(project) => &mut project.lock,
			Self::File { lock, .. } => lock,
		}
	}

	/// The owning env, if any — a bare lockfile has no `flake.nix` to reconcile.
	pub fn env(&self) -> Option<&Env> {
		match self {
			Self::Env(project) => Some(&project.env),
			Self::File { .. } => None,
		}
	}

	/// Guard a mutating verb (see [`LockSource::writable`]); a bare lockfile is
	/// always writable.
	pub fn require_writable(&self, verb: &str) -> Result<()> {
		match self {
			Self::Env(project) => project.source.writable(verb).map(|_| ()),
			Self::File { .. } => Ok(()),
		}
	}

	pub fn save(&self) -> Result<()> {
		match self {
			Self::Env(project) => project.save_lock(),
			Self::File { path, lock } => write_lock(path, lock),
		}
	}
}

/// Extract the `flake.lock` JSON embedded in a shell-only `shell.nix` — the
/// `builtins.fromJSON ''<json>''` here-string clinix writes for `init
/// --shell-only` — reversing the nix indented-string escapes.
fn extract_embedded_lock(shell_nix: &str) -> Result<String> {
	let start = shell_nix
		.find(EMBED_OPEN)
		.map(|i| i + EMBED_OPEN.len())
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

	/// A shell-only `shell.nix` as `init --shell-only` writes it: the lock JSON
	/// embedded via `builtins.fromJSON (''<json>'')`, with no sibling `flake.lock`.
	fn shell_only_shell_nix(lock_json: &str) -> String {
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
		let shell = shell_only_shell_nix(LOCK_JSON);
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
			shell_only_shell_nix(LOCK_JSON),
		)
		.unwrap();
		// Deliberately no flake.lock: `load` must fall back to the embedded lock.
		let project = Project::load(env_at(dir.path())).unwrap();
		assert!(project.lock.nodes.contains_key("nixpkgs"));
	}

	#[test]
	fn lock_source_classifies_every_case() {
		let dir = tempfile::tempdir().unwrap();
		let env = env_at(dir.path());
		assert_eq!(
			LockSource::of(&env).unwrap(),
			LockSource::NoShell(dir.path().to_path_buf())
		);

		let shell = dir.path().join("shell.nix");
		fs::write(
			&shell,
			"{ pkgs ? import <nixpkgs> { } }: pkgs.mkShell { }\n",
		)
		.unwrap();
		assert_eq!(
			LockSource::of(&env).unwrap(),
			LockSource::NoLock(shell.clone())
		);

		fs::write(&shell, shell_only_shell_nix(LOCK_JSON)).unwrap();
		assert_eq!(LockSource::of(&env).unwrap(), LockSource::Embedded(shell));

		let flake_lock = dir.path().join("flake.lock");
		fs::write(&flake_lock, LOCK_JSON).unwrap();
		assert_eq!(LockSource::of(&env).unwrap(), LockSource::File(flake_lock));
	}

	#[test]
	fn only_a_file_source_is_writable() {
		let p = std::path::PathBuf::from("/x");
		assert!(LockSource::File(p.clone()).writable("v").is_ok());
		for source in [
			LockSource::Embedded(p.clone()),
			LockSource::NoLock(p.clone()),
			LockSource::NoShell(p),
		] {
			assert!(source.writable("v").is_err());
		}
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
