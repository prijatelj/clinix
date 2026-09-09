//! The env **registry**: named, reusable envs under `state/envs/<name>` and their
//! GC-root symlinks under `state/roots/`. Per the plan (§Config, state & the env
//! registry) the **filesystem *is* the index** — there is no database, so `list`
//! is a `readdir`, `rename` is a `mv`, and there is nothing to keep in sync.
//!
//! **Two indirect roots per entered env** ([`root_path`] + its [`rt_root`] sibling):
//! `roots/<key>` points at the env's `.drv` (its eval/source graph → offline
//! re-eval), while `roots/<key>.rt` points at the realized output of the shell's
//! `inputDerivation` (its *complete* build closure → the retention guarantee, which
//! holds regardless of `keep-outputs`; see [`crate::nix::root_input_closure`]).
//! [`release_root`] drops both; [`rename`] moves both.
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

/// The full GC-root path for an env: `state/roots/<key>` (the `.drv` root).
/// Re-entering after an `update` overwrites it, unrooting the previous drv.
pub fn root_path(cfg: &Config, env: &Env) -> PathBuf {
	roots_dir(cfg).join(root_key(env))
}

/// The `state/roots/<key>` GC-root path for a **File** target (`*.nix` run directly):
/// `file-<path-slug>` (`/`→`_`). Shared by the launcher and `clean` so both agree on
/// the key.
pub fn file_root(cfg: &Config, path: &std::path::Path) -> PathBuf {
	let slug = path.to_string_lossy().replace('/', "_");
	roots_dir(cfg).join(format!("file-{}", slug.trim_start_matches('_')))
}

/// The **`.rt` sibling** of a `.drv` root (`roots/<key>` → `roots/<key>.rt`): the
/// indirect root on the env's `inputDerivation` output. Appends `.rt` to the file
/// name (not `with_extension`, which would clobber a `.`-containing path slug).
pub fn rt_root(root: &std::path::Path) -> PathBuf {
	let mut name = root.as_os_str().to_os_string();
	name.push(".rt");
	PathBuf::from(name)
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

	// Keep both GC roots aligned with the new name (env-<name> keying). Absent root
	// = the env was never entered; nothing to move. The `.rt` sibling moves too.
	let roots = roots_dir(cfg);
	let old_drv = roots.join(format!("env-{old}"));
	let new_drv = roots.join(format!("env-{new}"));
	for (from, to) in [
		(old_drv.clone(), new_drv.clone()),
		(rt_root(&old_drv), rt_root(&new_drv)),
	] {
		if from.symlink_metadata().is_ok() {
			fs::rename(from, to)?;
		}
	}
	Ok(())
}

/// Release a GC root **and its `.rt` sibling** so `nix-collect-garbage` can reap the
/// env's store paths. Returns whether *either* was present (so callers can report a
/// no-op accurately). Removes the symlinks only — the env's `shell.nix`/`flake.lock`
/// are untouched.
pub fn release_root(root: &std::path::Path) -> Result<bool> {
	let mut released = false;
	for p in [root.to_path_buf(), rt_root(root)] {
		match fs::remove_file(&p) {
			Ok(()) => released = true,
			Err(e) if e.kind() == io::ErrorKind::NotFound => {}
			Err(e) => return Err(e.into()),
		}
	}
	Ok(released)
}

/// A registry name is a **namespace** (it can hold `namespace:member` subshells),
/// so it obeys the namespace naming rules (`crate::env::naming`): start `[a-zA-Z]`,
/// then `[a-zA-Z0-9_-]`, no `:` (the member separator), no path separators or
/// `.`/`..`. This keeps names disjoint from project paths and safe as a directory
/// name, and consistent with seed namespaces.
pub fn validate_name(name: &str) -> Result<()> {
	crate::env::naming::validate_namespace(name).map_err(|detail| ClinixError::InvalidEnvName {
		name: name.to_string(),
		detail,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::{Path, PathBuf};

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
	fn rt_root_appends_suffix_without_clobbering_a_dotted_slug() {
		// `with_extension` would turn `…my.proj` into `…my.rt`; we must keep the slug.
		assert_eq!(rt_root(Path::new("/s/roots/env-python")), Path::new("/s/roots/env-python.rt"));
		assert_eq!(
			rt_root(Path::new("/s/roots/proj-home_u_my.proj")),
			Path::new("/s/roots/proj-home_u_my.proj.rt")
		);
	}

	#[test]
	fn file_root_slugs_the_path() {
		let cfg = Config {
			config_dir: PathBuf::from("/c"),
			state_dir: PathBuf::from("/s"),
		};
		assert_eq!(
			file_root(&cfg, Path::new("/home/u/dev.nix")),
			PathBuf::from("/s/roots/file-home_u_dev.nix")
		);
	}

	#[test]
	fn release_root_removes_both_roots_and_is_a_noop_the_second_time() {
		let dir = tempfile::tempdir().unwrap();
		let root = dir.path().join("env-x");
		let rt = rt_root(&root);
		std::fs::write(&root, "").unwrap();
		std::fs::write(&rt, "").unwrap();

		assert!(release_root(&root).unwrap(), "first release removes the roots");
		assert!(!root.exists() && !rt.exists());
		assert!(!release_root(&root).unwrap(), "second release is a no-op");
	}

	#[test]
	fn release_root_reports_true_when_only_the_rt_sibling_is_present() {
		let dir = tempfile::tempdir().unwrap();
		let root = dir.path().join("env-y");
		std::fs::write(rt_root(&root), "").unwrap();
		assert!(release_root(&root).unwrap());
	}

	#[test]
	fn validate_name_enforces_namespace_rules() {
		assert!(validate_name("python").is_ok());
		assert!(validate_name("my-tool_2").is_ok());
		assert!(validate_name("").is_err());
		assert!(validate_name(".").is_err());
		assert!(validate_name("..").is_err());
		assert!(validate_name("a/b").is_err());
		assert!(validate_name("2fast").is_err(), "no leading digit");
		assert!(
			validate_name("ns:member").is_err(),
			"`:` is the member separator"
		);
	}
}
