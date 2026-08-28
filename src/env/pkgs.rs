//! Packages: the [`Pkg`] spec (`name[=version]`) plus the package-mutating verbs
//! — `add`/`remove` (`shell.nix` splice), `pin`/`unpin` (`flake.lock`), and
//! `update`.

use clap::Args;

use crate::env::project::Project;
use crate::env::{Context, Env, RunCmd, resolve};
use crate::error::{ClinixError, Result, unimplemented};
use crate::model::lock::{FlakeLock, InputRef, Source};

/// A package with an optional pinned version, parsed from `name[=version]`.
#[derive(Debug, Clone)]
pub struct Pkg {
	pub name: String,
	pub version: Option<String>,
}

impl std::str::FromStr for Pkg {
	type Err = ClinixError;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		if s.is_empty() {
			return Err(ClinixError::InvalidPackage(s.to_string()));
		}
		match s.split_once('=') {
			Some((name, ver)) if !name.is_empty() && !ver.is_empty() => Ok(Pkg {
				name: name.to_string(),
				version: Some(ver.to_string()),
			}),
			Some(_) => Err(ClinixError::InvalidPackage(s.to_string())),
			None => Ok(Pkg {
				name: s.to_string(),
				version: None,
			}),
		}
	}
}

/// A set of packages for a target environment (shared by `add`/`remove`).
#[derive(Args, Debug)]
pub struct Pkgs {
	/// Target env name.
	pub name: String,
	/// Packages to add/remove (`name` or `name=version`).
	#[arg(required = true)]
	pub packages: Vec<Pkg>,
	/// After the edit, sort the `packages` list lexically (a reformatting pass;
	/// in-list comments are not preserved).
	#[arg(long)]
	pub sort: bool,
}

/// Package pin/unpin selection (shared by `pin`/`unpin`).
#[derive(Args, Debug)]
pub struct Pin {
	/// Target env name.
	pub name: String,
	/// Packages to pin/unpin. Empty with `--all` operates on the whole closure.
	pub packages: Vec<Pkg>,
	/// Freeze/unfreeze the whole env: pin every input at its current rev
	/// (closure-equivalent full pin), or (`unpin`) resume tracking.
	#[arg(long)]
	pub all: bool,
	/// For `unpin --all`: the branch/tag to track again. Required for now
	/// (clinix does not yet remember the pre-freeze ref — that arrives with
	/// `clinixEnv`).
	#[arg(long)]
	pub branch: Option<String>,
}

#[derive(Args, Debug)]
pub struct Update {
	/// Target env name.
	pub name: String,
	/// Packages to update; empty = all unpinned packages.
	pub packages: Vec<String>,
}
impl RunCmd for Update {
	/// Re-resolve the env's tracked `flake.lock` inputs to their latest revs (the
	/// classic `pin update`). With no package args, updates every root input that
	/// tracks a branch/tag; frozen (`original.rev`) and non-github inputs are left
	/// as-is. Writes a byte-compatible lock only if something advanced.
	fn run(self, _context: &Context) -> Result<()> {
		if !self.packages.is_empty() {
			return Err(unimplemented(
				"env update <pkg>",
				"plan phase 4: per-package version update",
			));
		}

		let env = resolve(Some(&self.name))?;
		// A single-file env keeps its lock embedded in shell.nix; writing a
		// flake.lock here would create a second, drifting source of truth.
		if !env.root.join("flake.lock").exists() {
			return Err(unimplemented(
				"env update on a single-file env",
				"the lock is embedded in shell.nix; re-embedding on update is a later slice",
			));
		}
		let mut project = Project::load(env)?;

		// The root's direct inputs are the update set (matches `pin`; `follows`
		// edges have no node of their own).
		let root = project.lock.root.clone();
		let targets: Vec<String> = match project.lock.nodes.get(&root) {
			Some(node) => node
				.inputs
				.values()
				.filter_map(|edge| match edge {
					InputRef::Direct(name) => Some(name.clone()),
					InputRef::Follows(_) => None,
				})
				.collect(),
			None => Vec::new(),
		};

		let mut changed = 0;
		for name in targets {
			let Some(node) = project.lock.nodes.get(&name) else {
				continue;
			};
			let Some(original) = &node.original else {
				continue;
			};
			if original.source_type() != Some("github") {
				continue; // git/tarball re-lock is a later phase
			}
			let Some(git_ref) = original.git_ref().map(str::to_string) else {
				continue; // frozen at a rev — nothing to advance
			};
			let owner = required(original.owner(), &name, "owner")?;
			let repo = required(original.repo(), &name, "repo")?;
			let old_rev = node
				.locked
				.as_ref()
				.and_then(|l| l.rev())
				.map(str::to_string);

			let (rev, nar_hash) = crate::nix::resolve_github(&owner, &repo, &git_ref)?;
			if old_rev.as_deref() == Some(rev.as_str()) {
				println!("{name}: unchanged");
				continue;
			}
			project
				.lock
				.nodes
				.get_mut(&name)
				.expect("target exists")
				.locked = Some(Source::github_locked(
				&owner,
				&repo,
				rev.as_str(),
				nar_hash.as_str(),
			));
			println!(
				"{name}: {} -> {}",
				old_rev.as_deref().unwrap_or("none"),
				rev.as_str()
			);
			changed += 1;
		}

		if changed == 0 {
			println!("clinix: all inputs up to date");
		} else {
			std::fs::write(project.env.root.join("flake.lock"), project.lock.to_json())?;
		}
		Ok(())
	}
}

/// A required `original` field, or a [`ClinixError::Resolve`] naming what's missing.
fn required(value: Option<&str>, node: &str, field: &str) -> Result<String> {
	value
		.map(str::to_string)
		.ok_or_else(|| ClinixError::Resolve(format!("{node}: github input missing `{field}`")))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::lock::{FlakeLock, Node};
	use std::collections::BTreeMap;

	// Network + classic-nix E2E: pin an env to a deliberately stale nixos-26.05
	// rev, then `update` and assert it advanced. Gated; run with `-- --ignored`.
	#[test]
	#[ignore = "requires network + git/nix-prefetch-url/nix-hash"]
	fn update_advances_a_stale_nixpkgs_pin() {
		let stale = "2f5a153c270b70cb0f8c11f46d96d6d3bc39f4e3";
		let mut nodes = BTreeMap::new();
		nodes.insert(
			"nixpkgs".to_string(),
			Node {
				locked: Some(Source::github_locked(
					"NixOS",
					"nixpkgs",
					stale,
					"sha256-Yjv0WEg39KRYS0rBdTbu6Fc/or/ihAKk13W9sQ6VWd0=",
				)),
				original: Some(Source::github_ref("NixOS", "nixpkgs", "nixos-26.05")),
				..Node::default()
			},
		);
		let mut root_inputs = BTreeMap::new();
		root_inputs.insert(
			"nixpkgs".to_string(),
			InputRef::Direct("nixpkgs".to_string()),
		);
		nodes.insert(
			"root".to_string(),
			Node {
				inputs: root_inputs,
				..Node::default()
			},
		);
		let lock = FlakeLock {
			nodes,
			root: "root".to_string(),
			version: 7,
		};

		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("flake.lock"), lock.to_json()).unwrap();

		Update {
			name: dir.path().to_str().unwrap().to_string(),
			packages: vec![],
		}
		.run(&Context {
			options: Default::default(),
		})
		.unwrap();

		let out =
			FlakeLock::from_json(&std::fs::read_to_string(dir.path().join("flake.lock")).unwrap())
				.unwrap();
		let new_rev = out.nodes["nixpkgs"].locked.as_ref().unwrap().rev().unwrap();
		assert_ne!(new_rev, stale, "nixos-26.05 should have advanced");
		assert_eq!(new_rev.len(), 40);
	}

	/// A one-input lock: nixpkgs tracking `nixos-26.05` at a fixed rev.
	fn tracked_lock() -> FlakeLock {
		let mut nodes = BTreeMap::new();
		nodes.insert(
			"nixpkgs".to_string(),
			Node {
				locked: Some(Source::github_locked("NixOS", "nixpkgs", "abc123", "sha256-x")),
				original: Some(Source::github_ref("NixOS", "nixpkgs", "nixos-26.05")),
				..Node::default()
			},
		);
		let mut root_inputs = BTreeMap::new();
		root_inputs.insert(
			"nixpkgs".to_string(),
			InputRef::Direct("nixpkgs".to_string()),
		);
		nodes.insert(
			"root".to_string(),
			Node {
				inputs: root_inputs,
				..Node::default()
			},
		);
		FlakeLock {
			nodes,
			root: "root".to_string(),
			version: 7,
		}
	}

	#[test]
	fn freeze_then_unfreeze_roundtrips_standardly() {
		let mut lock = tracked_lock();

		let froze = freeze(&mut lock);
		assert_eq!(froze.len(), 1);
		let orig = lock.nodes["nixpkgs"].original.as_ref().unwrap();
		assert_eq!(orig.rev(), Some("abc123")); // pinned to the locked rev
		assert_eq!(orig.git_ref(), None); // standard: ref dropped

		assert!(freeze(&mut lock).is_empty(), "already frozen");

		let unfroze = unfreeze(&mut lock, "nixos-26.05");
		assert_eq!(unfroze.len(), 1);
		let orig = lock.nodes["nixpkgs"].original.as_ref().unwrap();
		assert_eq!(orig.git_ref(), Some("nixos-26.05"));
		assert_eq!(orig.rev(), None);
	}

	#[test]
	fn swap_once_requires_exactly_one_occurrence() {
		assert_eq!(swap_once("a b", "a", "X").as_deref(), Some("X b"));
		assert_eq!(swap_once("a b a", "a", "X"), None); // twice
		assert_eq!(swap_once("b", "a", "X"), None); // zero
	}
}

/// Add packages to an env's `shell.nix` `packages` list (offline `rnix` splice;
/// versions stay implicit in the pinned rev — use `pin` for `pkg=ver`).
pub fn add(args: Pkgs, _context: &Context) -> Result<()> {
	let (shell_nix, names) = prepare(&args)?;
	let src = std::fs::read_to_string(&shell_nix)?;
	let edit = super::nix_edit::add_packages(&src, &names)?;
	finish(
		&shell_nix,
		&src,
		edit,
		args.sort,
		"added",
		"already present",
	)
}

/// Remove packages from an env's `shell.nix` `packages` list (offline splice).
pub fn remove(args: Pkgs, _context: &Context) -> Result<()> {
	let (shell_nix, names) = prepare(&args)?;
	let src = std::fs::read_to_string(&shell_nix)?;
	let edit = super::nix_edit::remove_packages(&src, &names)?;
	finish(&shell_nix, &src, edit, args.sort, "removed", "not present")
}

/// Apply the optional `--sort` reformat, write atomically only if the file
/// actually changed, and print the grouped report.
fn finish(
	shell_nix: &std::path::Path,
	original: &str,
	edit: super::nix_edit::Edit,
	sort: bool,
	changed_label: &str,
	skipped_label: &str,
) -> Result<()> {
	let source = if sort {
		super::nix_edit::sort_packages(&edit.source)?
	} else {
		edit.source.clone()
	};
	if source != original {
		write_atomic(shell_nix, &source)?;
	}
	report(changed_label, skipped_label, &edit);
	Ok(())
}

/// Resolve the env, require its `shell.nix`, reject versioned specs (versions are
/// `pin`'s job), and return `(shell.nix path, bare package names)`.
fn prepare(args: &Pkgs) -> Result<(std::path::PathBuf, Vec<String>)> {
	if args.packages.iter().any(|p| p.version.is_some()) {
		return Err(unimplemented(
			"env add/remove with a versioned package",
			"names only; use `pin <env> <pkg>=<ver>` for versions (phase 4)",
		));
	}
	let env = resolve(Some(&args.name))?;
	let shell_nix = env.root.join("shell.nix");
	if !shell_nix.is_file() {
		return Err(ClinixError::ShellNix(format!(
			"no shell.nix at {} (run: clinix env init)",
			env.root.display()
		)));
	}
	let names = args.packages.iter().map(|p| p.name.clone()).collect();
	Ok((shell_nix, names))
}

/// Write atomically (same-dir temp + rename) so a partial write can't corrupt a
/// hand-edited `shell.nix`.
fn write_atomic(path: &std::path::Path, content: &str) -> Result<()> {
	let name = path.file_name().unwrap().to_string_lossy();
	let tmp = path.with_file_name(format!(".{name}.clinix-tmp"));
	std::fs::write(&tmp, content)?;
	std::fs::rename(&tmp, path)?;
	Ok(())
}

/// Grouped, tab-indented report:
/// `added:\n\tripgrep\n\nalready present:\n\tjq`.
fn report(changed_label: &str, skipped_label: &str, edit: &super::nix_edit::Edit) {
	if !edit.changed.is_empty() {
		println!("{changed_label}:");
		for name in &edit.changed {
			println!("\t{name}");
		}
	}
	if !edit.skipped.is_empty() {
		if !edit.changed.is_empty() {
			println!();
		}
		println!("{skipped_label}:");
		for name in &edit.skipped {
			println!("\t{name}");
		}
	}
}

/// Freeze the whole env: pin every tracked input at its current rev (`--all`).
/// Ports `pin freeze` — a standard `flake.lock` edit (`original`: drop `ref`, add
/// `rev`), plus the `flake.nix` URL swap for `--flake` envs. Offline (copies
/// `locked.rev`; no resolve). Per-package pinning (`pin <pkg>[=<ver>]`) is phase 4.
pub fn pin(args: Pin, _context: &Context) -> Result<()> {
	require_scope(&args, "env pin")?;
	let mut project = Project::load(resolve(Some(&args.name))?)?;
	let flake_lock = require_flake_lock(&project.env, "env pin")?;

	let changes = freeze(&mut project.lock);
	if changes.is_empty() {
		println!("clinix: nothing to freeze (all inputs already pinned)");
		return Ok(());
	}
	// For a `--flake` env, keep `flake.nix` consistent (else `nix flake` re-locks).
	let kept = reconcile_flake_nix(&mut project.lock, &project.env, &changes, Freeze)?;
	if kept.is_empty() {
		return Ok(());
	}
	std::fs::write(&flake_lock, project.lock.to_json())?;
	println!("frozen:");
	for c in &kept {
		println!("\t{} @ {:.9}", c.name, c.to);
	}
	Ok(())
}

/// Unfreeze the whole env: resume tracking `--branch <ref>` (`unpin --all`).
/// Ports `pin unfreeze -b`. Requires `--branch` for now (no remembered ref yet).
pub fn unpin(args: Pin, _context: &Context) -> Result<()> {
	require_scope(&args, "env unpin")?;
	let Some(branch) = args.branch.as_deref() else {
		return Err(ClinixError::Resolve(
			"unpin needs `--branch <ref>` (clinix does not yet remember the pre-freeze branch; \
			 that arrives with clinixEnv)"
				.into(),
		));
	};
	let mut project = Project::load(resolve(Some(&args.name))?)?;
	let flake_lock = require_flake_lock(&project.env, "env unpin")?;

	let changes = unfreeze(&mut project.lock, branch);
	if changes.is_empty() {
		println!("clinix: nothing to unfreeze (no frozen inputs)");
		return Ok(());
	}
	let kept = reconcile_flake_nix(&mut project.lock, &project.env, &changes, Unfreeze)?;
	if kept.is_empty() {
		return Ok(());
	}
	std::fs::write(&flake_lock, project.lock.to_json())?;
	println!("tracking {branch}:");
	for c in &kept {
		println!("\t{}", c.name);
	}
	Ok(())
}

/// Require `--all` (per-package pinning is phase 4).
fn require_scope(args: &Pin, verb: &str) -> Result<()> {
	if !args.packages.is_empty() {
		return Err(unimplemented(
			verb,
			"per-package pinning is phase 4 (version index); use `--all` to freeze the whole env",
		));
	}
	if !args.all {
		return Err(ClinixError::Resolve(format!(
			"{verb}: give `--all` to freeze/unfreeze the whole env (per-package is phase 4)"
		)));
	}
	Ok(())
}

/// The env's `flake.lock` path, or a guarded error for a single-file env (its
/// lock is embedded in `shell.nix`; editing a `flake.lock` here would drift).
fn require_flake_lock(env: &Env, verb: &str) -> Result<std::path::PathBuf> {
	let path = env.root.join("flake.lock");
	if path.exists() {
		Ok(path)
	} else {
		Err(unimplemented(
			verb,
			"single-file env: the lock is embedded in shell.nix; re-embedding is a later slice",
		))
	}
}

/// A frozen/unfrozen input, for `flake.nix` reconciliation and reporting.
/// `from`/`to` are the URL ref/rev being swapped (freeze: ref→rev; unfreeze:
/// rev→branch).
#[derive(Clone)]
struct Change {
	name: String,
	owner: String,
	repo: String,
	from: String,
	to: String,
}

/// **Pure.** Freeze every tracked github input (`original.ref`, no `rev`) at its
/// `locked.rev`: rewrite `original` to `{owner, repo, rev, type}` (standard —
/// drops the ref). Returns the changes.
fn freeze(lock: &mut FlakeLock) -> Vec<Change> {
	let mut changes = Vec::new();
	for name in input_names(lock) {
		let node = &lock.nodes[&name];
		let (Some(original), Some(locked)) = (&node.original, &node.locked) else {
			continue;
		};
		if original.source_type() != Some("github") || original.rev().is_some() {
			continue; // not github, or already frozen
		}
		let (Some(owner), Some(repo), Some(git_ref), Some(rev)) = (
			original.owner(),
			original.repo(),
			original.git_ref(),
			locked.rev(),
		) else {
			continue;
		};
		let change = Change {
			name: name.clone(),
			owner: owner.into(),
			repo: repo.into(),
			from: git_ref.into(),
			to: rev.into(),
		};
		lock.nodes.get_mut(&name).unwrap().original =
			Some(Source::github_rev(&change.owner, &change.repo, &change.to));
		changes.push(change);
	}
	changes
}

/// **Pure.** Unfreeze every frozen github input (`original.rev`, no `ref`) back
/// to tracking `branch`: rewrite `original` to `{owner, ref, repo, type}`.
fn unfreeze(lock: &mut FlakeLock, branch: &str) -> Vec<Change> {
	let mut changes = Vec::new();
	for name in input_names(lock) {
		let node = &lock.nodes[&name];
		let Some(original) = &node.original else {
			continue;
		};
		if original.source_type() != Some("github") || original.git_ref().is_some() {
			continue; // not github, or not frozen (still tracks a ref)
		}
		let (Some(owner), Some(repo), Some(rev)) =
			(original.owner(), original.repo(), original.rev())
		else {
			continue;
		};
		let change = Change {
			name: name.clone(),
			owner: owner.into(),
			repo: repo.into(),
			from: rev.into(),
			to: branch.into(),
		};
		lock.nodes.get_mut(&name).unwrap().original =
			Some(Source::github_ref(&change.owner, &change.repo, branch));
		changes.push(change);
	}
	changes
}

fn input_names(lock: &FlakeLock) -> Vec<String> {
	lock.nodes
		.keys()
		.filter(|n| **n != lock.root)
		.cloned()
		.collect()
}

#[derive(Clone, Copy)]
enum FlakeOp {
	Freeze,
	Unfreeze,
}
use FlakeOp::{Freeze, Unfreeze};

/// For a `--flake` env, swap each input's `github:owner/repo/<from>` →
/// `.../<to>` in `flake.nix` (exactly once, like `pin`'s `rewrite_url`). An input
/// whose URL can't be updated is **reverted** in the lock and warned about, so
/// `flake.lock` and `flake.nix` stay consistent. Returns the changes actually
/// kept. A default env (no `flake.nix`) keeps everything.
fn reconcile_flake_nix(
	lock: &mut FlakeLock,
	env: &Env,
	changes: &[Change],
	op: FlakeOp,
) -> Result<Vec<Change>> {
	let flake_path = env.root.join("flake.nix");
	let Ok(mut flake_src) = std::fs::read_to_string(&flake_path) else {
		return Ok(changes.to_vec()); // default env: nothing to reconcile
	};

	let mut kept = Vec::new();
	for c in changes {
		let url = |r: &str| format!("github:{}/{}/{}", c.owner, c.repo, r);
		let (old, new) = (url(&c.from), url(&c.to));
		match swap_once(&flake_src, &old, &new) {
			Some(swapped) => {
				flake_src = swapped;
				kept.push(c.clone());
			}
			None => {
				// Revert the lock edit so the two stay consistent; tell the user.
				let reverted = match op {
					Freeze => Source::github_ref(&c.owner, &c.repo, &c.from),
					Unfreeze => Source::github_rev(&c.owner, &c.repo, &c.from),
				};
				lock.nodes.get_mut(&c.name).unwrap().original = Some(reverted);
				eprintln!(
					"clinix: warning: `{old}` not found exactly once in flake.nix; \
					 left `{}` unchanged — set its input url to `{new}` and re-run",
					c.name
				);
			}
		}
	}
	if !kept.is_empty() {
		std::fs::write(&flake_path, flake_src)?;
	}
	Ok(kept)
}

/// Replace `old` with `new` iff `old` occurs exactly once (else `None`).
fn swap_once(src: &str, old: &str, new: &str) -> Option<String> {
	(src.matches(old).count() == 1).then(|| src.replacen(old, new, 1))
}
