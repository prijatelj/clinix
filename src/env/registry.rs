//! The env **registry**: named, reusable envs under `state/envs/<name>` and their
//! GC-root symlinks under `state/roots/`. Per the plan (§Config, state & the env
//! registry) the **filesystem *is* the index** — there is no database, so `list`
//! is a `readdir`, `rename` is a `mv`, and there is nothing to keep in sync.
//!
//! This module also owns **GC-root path keying**, the rename-correctness fix:
//! registry envs key their root by **name** (`env-<name>`, so a rename is `mv` +
//! one symlink rename, O(1)), while projects key by **path slug** (`proj-<slug>`,
//! since a project has no registry name). A path-slug key for a registry env
//! would orphan the old root on rename.
//!
//! Pure filesystem work (no nix, no network) — richly testable without a nix
//! toolchain (an L3 layer, `notes/clinix/design/testing.md`).

use std::fs;
use std::io;
use std::path::PathBuf;

use crate::env::config::Config;
use crate::env::{Env, Kind};
use crate::error::{ClinixError, Result};

/// Where registry envs live: `state/envs/`.
pub fn envs_dir(cfg: &Config) -> PathBuf {
	cfg.state_dir.join("envs")
}

/// Where GC-root symlinks live: `state/roots/`.
pub fn roots_dir(cfg: &Config) -> PathBuf {
	cfg.state_dir.join("roots")
}

/// Where generated seed-stack compose expressions live: `state/compose/`.
pub fn compose_dir(cfg: &Config) -> PathBuf {
	cfg.state_dir.join("compose")
}

/// A registry env's root directory: `state/envs/<name>`.
pub fn env_root(cfg: &Config, name: &str) -> PathBuf {
	envs_dir(cfg).join(name)
}

/// The GC-root file name for an env — the rename-correctness key. Registry envs
/// are keyed by name (`env-<name>`), projects by their path slug (`proj-<slug>`,
/// `/`→`_`). See the module docs.
pub fn root_key(env: &Env) -> String {
	match (env.kind, env.name.as_deref()) {
		(Kind::Registry, Some(name)) => format!("env-{name}"),
		_ => {
			let slug = env.root.to_string_lossy().replace('/', "_");
			format!("proj-{}", slug.trim_start_matches('_'))
		}
	}
}

/// The full GC-root path for an env: `state/roots/<key>`. One indirect root per
/// env; re-entering after an `update` overwrites it, unrooting the previous drv.
pub fn root_path(cfg: &Config, env: &Env) -> PathBuf {
	roots_dir(cfg).join(root_key(env))
}

/// Enumerate registry env names (sorted). An absent `envs/` dir means "no
/// registry yet" → an empty list, not an error (graceful: `list` works before the
/// first env is created).
pub fn list_names(cfg: &Config) -> Result<Vec<String>> {
	let dir = envs_dir(cfg);
	let read = match fs::read_dir(&dir) {
		Ok(read) => read,
		Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
		Err(e) => return Err(e.into()),
	};
	let mut names = Vec::new();
	for entry in read {
		let entry = entry?;
		if entry.file_type()?.is_dir() {
			names.push(entry.file_name().to_string_lossy().into_owned());
		}
	}
	names.sort();
	Ok(names)
}

/// Rename a registry env: atomic `mv` of `envs/<old>` → `envs/<new>`, plus a
/// rename of its GC root (`env-<old>` → `env-<new>`) if one exists. `<old>` must
/// be an existing registry env; `<new>` must be a valid, unused registry name.
pub fn rename(cfg: &Config, old: &str, new: &str) -> Result<()> {
	validate_name(new)?;
	let src = env_root(cfg, old);
	if !src.is_dir() {
		return Err(ClinixError::UnknownEnv(old.to_string()));
	}
	let dst = env_root(cfg, new);
	if dst.exists() {
		return Err(ClinixError::EnvExists(new.to_string()));
	}
	fs::rename(&src, &dst)?;

	// Keep the GC root aligned with the new name (env-<name> keying). Absent root
	// = the env was never entered; nothing to move.
	let roots = roots_dir(cfg);
	let old_root = roots.join(format!("env-{old}"));
	if old_root.symlink_metadata().is_ok() {
		fs::rename(old_root, roots.join(format!("env-{new}")))?;
	}
	Ok(())
}

/// Release an env's GC root so `nix-collect-garbage` can reap its store paths.
/// Returns whether a root was present (so callers can report accurately). Removes
/// the symlink only — the env's `shell.nix`/`flake.lock` are untouched.
pub fn clean(cfg: &Config, env: &Env) -> Result<bool> {
	let path = root_path(cfg, env);
	match fs::remove_file(&path) {
		Ok(()) => Ok(true),
		Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
		Err(e) => Err(e.into()),
	}
}

/// A registry name must be a single path component: no separators (which
/// [`crate::env::resolve`] reads as a project path), no `.`/`..`, non-empty. This
/// keeps names disjoint from project paths and safe as a directory name.
pub fn validate_name(name: &str) -> Result<()> {
	let invalid = |detail| ClinixError::InvalidEnvName {
		name: name.to_string(),
		detail,
	};
	if name.is_empty() {
		return Err(invalid("name is empty"));
	}
	if name == "." || name == ".." {
		return Err(invalid("`.`/`..` are reserved"));
	}
	if name.contains('/') || name.contains('\\') {
		return Err(invalid("name may not contain a path separator"));
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::PathBuf;

	fn env(name: Option<&str>, kind: Kind, root: &str) -> Env {
		Env {
			name: name.map(str::to_string),
			root: PathBuf::from(root),
			kind,
		}
	}

	#[test]
	fn registry_env_root_key_is_name_based() {
		let e = env(Some("python"), Kind::Registry, "/state/envs/python");
		assert_eq!(root_key(&e), "env-python");
	}

	#[test]
	fn project_root_key_is_path_slug() {
		let e = env(None, Kind::Project, "/home/u/proj");
		assert_eq!(root_key(&e), "proj-home_u_proj");
	}

	#[test]
	fn rename_key_only_changes_the_name_component() {
		// The correctness invariant: a registry rename moves one root symlink,
		// because the key tracks the name, not the (changed) path.
		let before = env(Some("old"), Kind::Registry, "/state/envs/old");
		let after = env(Some("new"), Kind::Registry, "/state/envs/new");
		assert_eq!(root_key(&before), "env-old");
		assert_eq!(root_key(&after), "env-new");
	}

	#[test]
	fn validate_name_rejects_separators_and_dots() {
		assert!(validate_name("python").is_ok());
		assert!(validate_name("").is_err());
		assert!(validate_name(".").is_err());
		assert!(validate_name("..").is_err());
		assert!(validate_name("a/b").is_err());
	}
}
