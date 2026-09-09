//! The env **registry**: named, reusable envs under `state/envs/<name>` and their
//! GC-root symlinks under `state/roots/`. Per the plan (§Config, state & the env
//! registry) the **filesystem *is* the index** — there is no database, so `list`
//! is a `readdir`, `rename` is a `mv`, and there is nothing to keep in sync.
//!
//! **Versioned root pairs per env.** Each entry that changes the derivation mints a
//! new version `roots/<key>@<seq>` (the `.drv` root → eval/source graph) plus its
//! [`rt_root`] sibling `roots/<key>@<seq>.rt` (the `inputDerivation` output → the
//! complete build closure, retained regardless of `keep-outputs`; see
//! [`crate::nix::root_input_closure`]). The highest `seq` is "current"; older
//! versions are **priors**, kept up to `keep_n_prior_roots` and pruned beyond that
//! ([`prune_versions`]). Because both roots are kept per version, a prior stays
//! enterable **offline** from its stored `.drv`. [`release_root`] drops every
//! version; [`rename`] re-keys them; nothing is ever moved to preserve a root
//! (indirect roots are path-bound), so priors are simply un-pruned prior entries.
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

/// The **base** GC-root path for an env: `state/roots/<key>`. This is the *identity*
/// path; the actual roots are **versioned** siblings `state/roots/<key>@<seq>` (+ a
/// `.rt` sibling per version). See [`version_path`] and [`list_versions`].
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

/// The **`.rt` sibling** of a `.drv` root (`…@<seq>` → `…@<seq>.rt`): the indirect
/// root on that version's `inputDerivation` output. Appends `.rt` to the file name
/// (not `with_extension`, which would clobber a `.`-containing path slug).
pub fn rt_root(root: &std::path::Path) -> PathBuf {
	let mut name = root.as_os_str().to_os_string();
	name.push(".rt");
	PathBuf::from(name)
}

/// The versioned `.drv` root path for `seq`: `state/roots/<key>@<seq>`. Each entry
/// that changes the derivation mints the next `seq`; the highest `seq` is "current".
/// Both the `.drv` root here and its [`rt_root`] sibling are kept per version, so a
/// prior version's recipe **and** built closure survive (offline-enterable).
pub fn version_path(base: &std::path::Path, seq: u64) -> PathBuf {
	let mut name = base.as_os_str().to_os_string();
	name.push(format!("@{seq}"));
	PathBuf::from(name)
}

/// One root version: its `seq` id and the versioned `.drv` root path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootVersion {
	pub seq: u64,
	pub drv_root: PathBuf,
}

/// How to select a root version to enter (see `crate::env::launch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionSelect {
	/// The current version (mint a new one if the derivation changed).
	Current,
	/// The Nth prior version — `1` = the version immediately before current.
	Prior(usize),
	/// An exact version by its `seq` id.
	Exact(u64),
}

/// Enumerate a base key's root versions, **sorted ascending by `seq`** (so `.last()`
/// is current). Matches `roots/<key>@<digits>` (never the `.rt` siblings). An absent
/// roots dir ⇒ empty (graceful). One source of truth for launch/clean/rename/list.
pub fn list_versions(base: &std::path::Path) -> Result<Vec<RootVersion>> {
	let (dir, prefix) = match (base.parent(), base.file_name()) {
		(Some(d), Some(k)) => (d.to_path_buf(), format!("{}@", k.to_string_lossy())),
		_ => return Ok(Vec::new()),
	};
	let read = match fs::read_dir(&dir) {
		Ok(r) => r,
		Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
		Err(e) => return Err(e.into()),
	};
	let mut versions = Vec::new();
	for entry in read {
		let entry = entry?;
		let name = entry.file_name();
		let name = name.to_string_lossy();
		// `<key>@<digits>` only — the remainder after the prefix must be all digits,
		// which excludes the `.rt` siblings (`<key>@<seq>.rt`).
		if let Some(rest) = name.strip_prefix(&prefix)
			&& let Ok(seq) = rest.parse::<u64>()
		{
			versions.push(RootVersion {
				seq,
				drv_root: dir.join(name.as_ref()),
			});
		}
	}
	versions.sort_by_key(|v| v.seq);
	Ok(versions)
}

/// Every distinct root **base key** present under `roots/` (sorted, de-duplicated) —
/// e.g. `env-python`, `proj-home_u_x`, `stack-rust-claude`. Strips the `.rt` sibling
/// suffix and the `@<seq>` version suffix from each entry. Powers the global `env
/// roots` listing and `clean`'s bulk selectors. An absent roots dir ⇒ empty.
pub fn list_root_keys(cfg: &Config) -> Result<Vec<String>> {
	let dir = roots_dir(cfg);
	let read = match fs::read_dir(&dir) {
		Ok(r) => r,
		Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
		Err(e) => return Err(e.into()),
	};
	let mut keys = std::collections::BTreeSet::new();
	for entry in read {
		let name = entry?.file_name().to_string_lossy().into_owned();
		// `<key>@<seq>[.rt]` → `<key>`: drop a trailing `.rt`, then a trailing
		// `@<digits>` (only when the suffix after the last `@` is all digits).
		let stem = name.strip_suffix(".rt").unwrap_or(&name);
		let base = match stem.rsplit_once('@') {
			Some((head, seq)) if !seq.is_empty() && seq.bytes().all(|b| b.is_ascii_digit()) => head,
			_ => stem,
		};
		keys.insert(base.to_string());
	}
	Ok(keys.into_iter().collect())
}

/// The next version `seq` for a base key: one past the current max, or `1`.
pub fn next_seq(base: &std::path::Path) -> Result<u64> {
	Ok(list_versions(base)?.last().map(|v| v.seq + 1).unwrap_or(1))
}

/// Resolve a [`VersionSelect`] to a concrete version's `.drv` root path (for entry),
/// erroring clearly if it names a version that does not exist.
pub fn resolve_version(base: &std::path::Path, sel: VersionSelect) -> Result<PathBuf> {
	let mut versions = list_versions(base)?;
	if versions.is_empty() {
		return Err(ClinixError::Config(format!(
			"no root versions for `{}` — enter it once first",
			base.display()
		)));
	}
	match sel {
		VersionSelect::Current => Ok(versions.pop().unwrap().drv_root),
		VersionSelect::Prior(n) => {
			// desc: index 0 = current, n = the Nth prior.
			versions.reverse();
			versions.get(n).map(|v| v.drv_root.clone()).ok_or_else(|| {
				ClinixError::Config(format!(
					"no prior {n} — {} version(s) exist (0 = current)",
					versions.len()
				))
			})
		}
		VersionSelect::Exact(seq) => versions
			.iter()
			.find(|v| v.seq == seq)
			.map(|v| v.drv_root.clone())
			.ok_or_else(|| ClinixError::Config(format!("no root version @{seq}"))),
	}
}

/// Release one version's `.drv` + `.rt` pair by its `seq` id. Returns whether it was
/// present (so `clean` can report a no-op accurately).
pub fn release_version(base: &std::path::Path, seq: u64) -> Result<bool> {
	let drv = version_path(base, seq);
	let mut released = false;
	for p in [drv.clone(), rt_root(&drv)] {
		match fs::remove_file(&p) {
			Ok(()) => released = true,
			Err(e) if e.kind() == io::ErrorKind::NotFound => {}
			Err(e) => return Err(e.into()),
		}
	}
	Ok(released)
}

/// Release the `n` **oldest** versions (lowest `seq`) of a base key, returning the
/// removed `seq`s (ascending). `n` beyond the count releases them all.
pub fn release_oldest(base: &std::path::Path, n: usize) -> Result<Vec<u64>> {
	let versions = list_versions(base)?;
	let take = n.min(versions.len());
	let mut removed = Vec::with_capacity(take);
	for v in &versions[..take] {
		let _ = fs::remove_file(&v.drv_root);
		let _ = fs::remove_file(rt_root(&v.drv_root));
		removed.push(v.seq);
	}
	Ok(removed)
}

/// Prune a base key's versions to the newest `keep`, removing older versions'
/// `.drv` + `.rt` roots (releasing their closures). `keep` = `keep_n_prior_roots + 1`.
pub fn prune_versions(base: &std::path::Path, keep: usize) -> Result<()> {
	let versions = list_versions(base)?;
	if versions.len() <= keep {
		return Ok(());
	}
	for v in &versions[..versions.len() - keep] {
		let _ = fs::remove_file(&v.drv_root);
		let _ = fs::remove_file(rt_root(&v.drv_root));
	}
	Ok(())
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

	// Re-key every versioned root (and its `.rt`) to the new name so `clean <new>`
	// finds them. Indirect roots are path-bound, so a moved link is re-registered by
	// the next entry; retention across a rename is best-effort (re-enter to re-root).
	let old_base = roots_dir(cfg).join(format!("env-{old}"));
	let new_base = roots_dir(cfg).join(format!("env-{new}"));
	for v in list_versions(&old_base)? {
		let new_drv = version_path(&new_base, v.seq);
		fs::rename(&v.drv_root, &new_drv)?;
		let old_rt = rt_root(&v.drv_root);
		if old_rt.symlink_metadata().is_ok() {
			fs::rename(old_rt, rt_root(&new_drv))?;
		}
	}
	Ok(())
}

/// Release a GC root **and its `.rt` sibling** so `nix-collect-garbage` can reap the
/// env's store paths. Returns whether *either* was present (so callers can report a
/// no-op accurately). Removes the symlinks only — the env's `shell.nix`/`flake.lock`
/// are untouched.
pub fn release_root(base: &std::path::Path) -> Result<bool> {
	let mut released = false;
	for v in list_versions(base)? {
		for p in [v.drv_root.clone(), rt_root(&v.drv_root)] {
			match fs::remove_file(&p) {
				Ok(()) => released = true,
				Err(e) if e.kind() == io::ErrorKind::NotFound => {}
				Err(e) => return Err(e.into()),
			}
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

	/// Write a fake version pair (`<base>@<seq>` drv + its `.rt`) for tests.
	fn touch_version(base: &Path, seq: u64) {
		std::fs::write(version_path(base, seq), format!("drv{seq}")).unwrap();
		std::fs::write(rt_root(&version_path(base, seq)), "rt").unwrap();
	}

	#[test]
	fn release_root_removes_all_versions_and_is_a_noop_when_empty() {
		let dir = tempfile::tempdir().unwrap();
		let base = dir.path().join("env-x");
		touch_version(&base, 1);
		touch_version(&base, 2);

		assert!(release_root(&base).unwrap(), "removes every version pair");
		assert!(list_versions(&base).unwrap().is_empty());
		assert!(!release_root(&base).unwrap(), "second release is a no-op");
	}

	#[test]
	fn list_versions_is_sorted_excludes_rt_and_next_seq_advances() {
		let dir = tempfile::tempdir().unwrap();
		let base = dir.path().join("proj-a");
		for seq in [3u64, 1, 2] {
			std::fs::write(version_path(&base, seq), "drv").unwrap();
		}
		// A `.rt` sibling must not be counted as its own version.
		std::fs::write(rt_root(&version_path(&base, 3)), "rt").unwrap();

		let seqs: Vec<u64> = list_versions(&base).unwrap().iter().map(|v| v.seq).collect();
		assert_eq!(seqs, vec![1, 2, 3], "sorted ascending; `.rt` excluded");
		assert_eq!(next_seq(&base).unwrap(), 4);
	}

	#[test]
	fn prune_keeps_newest_and_resolve_selects_versions() {
		let dir = tempfile::tempdir().unwrap();
		let base = dir.path().join("env-p");
		for seq in 1..=4u64 {
			touch_version(&base, seq);
		}
		// keep = current + 1 prior → newest two survive, older `.drv` + `.rt` pruned.
		prune_versions(&base, 2).unwrap();
		let seqs: Vec<u64> = list_versions(&base).unwrap().iter().map(|v| v.seq).collect();
		assert_eq!(seqs, vec![3, 4]);
		assert!(!rt_root(&version_path(&base, 1)).exists(), "pruned .rt gone too");

		assert_eq!(
			resolve_version(&base, VersionSelect::Current).unwrap(),
			version_path(&base, 4)
		);
		assert_eq!(
			resolve_version(&base, VersionSelect::Prior(1)).unwrap(),
			version_path(&base, 3),
			"prior 1 = the version before current"
		);
		assert_eq!(
			resolve_version(&base, VersionSelect::Exact(3)).unwrap(),
			version_path(&base, 3)
		);
		assert!(resolve_version(&base, VersionSelect::Prior(9)).is_err());
		assert!(resolve_version(&base, VersionSelect::Exact(99)).is_err());
	}

	#[test]
	fn next_seq_is_one_for_a_fresh_key() {
		let dir = tempfile::tempdir().unwrap();
		assert_eq!(next_seq(&dir.path().join("env-fresh")).unwrap(), 1);
	}

	#[test]
	fn list_root_keys_strips_version_and_rt_suffixes_and_dedups() {
		let dir = tempfile::tempdir().unwrap();
		let cfg = Config {
			config_dir: dir.path().join("c"),
			state_dir: dir.path().to_path_buf(),
		};
		let roots = roots_dir(&cfg);
		std::fs::create_dir_all(&roots).unwrap();
		for f in [
			"env-py@1",
			"env-py@1.rt",
			"env-py@2",
			"env-py@2.rt",
			"proj-a_b@1",
			"stack-x-y@3",
			"stack-x-y@3.rt",
			"file-c@1",
		] {
			std::fs::write(roots.join(f), "").unwrap();
		}
		assert_eq!(
			list_root_keys(&cfg).unwrap(),
			vec!["env-py", "file-c", "proj-a_b", "stack-x-y"], // sorted, de-duplicated
		);
	}

	#[test]
	fn release_version_removes_one_pair_by_seq() {
		let dir = tempfile::tempdir().unwrap();
		let base = dir.path().join("env-v");
		touch_version(&base, 1);
		touch_version(&base, 2);

		assert!(release_version(&base, 1).unwrap());
		assert!(!rt_root(&version_path(&base, 1)).exists(), "its .rt goes too");
		let seqs: Vec<u64> = list_versions(&base).unwrap().iter().map(|v| v.seq).collect();
		assert_eq!(seqs, vec![2], "only the targeted version is gone");
		assert!(!release_version(&base, 9).unwrap(), "absent seq → false");
	}

	#[test]
	fn release_oldest_removes_the_n_lowest_seqs() {
		let dir = tempfile::tempdir().unwrap();
		let base = dir.path().join("env-o");
		for seq in 1..=4u64 {
			touch_version(&base, seq);
		}
		assert_eq!(release_oldest(&base, 2).unwrap(), vec![1, 2]);
		let seqs: Vec<u64> = list_versions(&base).unwrap().iter().map(|v| v.seq).collect();
		assert_eq!(seqs, vec![3, 4]);
		// `n` beyond the count releases all remaining, reporting only what existed.
		assert_eq!(release_oldest(&base, 10).unwrap(), vec![3, 4]);
		assert!(list_versions(&base).unwrap().is_empty());
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
