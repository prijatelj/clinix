//! The shared `flake.lock` re-lock engine — enumerate a lock's tracked inputs
//! and advance them to their latest upstream rev (the classic `pin update`).
//!
//! Pure orchestration over [`crate::model::lock::FlakeLock`] (the data) and the
//! [`crate::nix`] resolvers (`git ls-remote` → rev, `nix-prefetch-url --unpack`
//! → narHash). It is deliberately **lock-level**: it reads/writes only a
//! `FlakeLock` and never touches `shell.nix`, `flake.nix`, or `system.nix`, so
//! the same engine serves `env update`/`outdated`/`verify` (two-file envs) and
//! `sys update`/`info` (a `system.nix` config). `sys` and `env` are sibling
//! scopes; neither owns the re-lock logic, so it lives once here (write-once).
//!
//! Persistence is the caller's job: [`update_tracked`] mutates the lock in place
//! and returns how many inputs advanced; the caller writes the file (`env` via
//! `Project::save_lock`, `sys` via its config dir). This keeps the engine free of
//! any assumption about *where* the lock lives.

use crate::error::{ClinixError, Result};
use crate::model::lock::{FlakeLock, InputRef, Source};
use crate::model::newtypes::Rev;

/// A required `original` field, or a [`ClinixError::Resolve`] naming what's missing.
pub(crate) fn required(value: Option<&str>, node: &str, field: &str) -> Result<String> {
	value
		.map(str::to_string)
		.ok_or_else(|| ClinixError::Resolve(format!("{node}: github input missing `{field}`")))
}

/// First 9 chars of a rev for compact display (revs are 40-hex).
pub(crate) fn short(rev: &str) -> &str {
	&rev[..rev.len().min(9)]
}
/// Compact display of an optional locked rev (`none` when absent).
pub(crate) fn short_opt(rev: Option<&str>) -> &str {
	rev.map(short).unwrap_or("none")
}
pub(crate) fn short_rev(rev: &Rev) -> String {
	short(rev.as_str()).to_string()
}

/// The root's **direct** input node names (the `pin`/update set; `follows` edges
/// have no node of their own). Shared by `update`, `outdated`, `verify`, and the
/// `sys info` table.
fn root_input_names(lock: &FlakeLock) -> Vec<String> {
	match lock.nodes.get(&lock.root) {
		Some(node) => node
			.inputs
			.values()
			.filter_map(|edge| match edge {
				InputRef::Direct(name) => Some(name.clone()),
				InputRef::Follows(_) => None,
			})
			.collect(),
		None => Vec::new(),
	}
}

/// A root github input that **tracks a branch/tag** (the update/outdated set).
/// Frozen (rev-pinned) and non-github inputs are excluded.
pub(crate) struct GhTracking {
	pub name: String,
	pub owner: String,
	pub repo: String,
	pub git_ref: String,
	pub locked_rev: Option<String>,
}

pub(crate) fn tracking_github_inputs(lock: &FlakeLock) -> Result<Vec<GhTracking>> {
	let mut out = Vec::new();
	for name in root_input_names(lock) {
		let Some(node) = lock.nodes.get(&name) else {
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
		out.push(GhTracking {
			owner: required(original.owner(), &name, "owner")?,
			repo: required(original.repo(), &name, "repo")?,
			git_ref,
			locked_rev: node.locked.as_ref().and_then(|l| l.rev()).map(str::to_string),
			name,
		});
	}
	Ok(out)
}

/// A root github input with a **locked** source (the verify set): its recorded
/// `(rev, narHash)` to re-check against the upstream tarball.
pub(crate) struct GhLocked {
	pub name: String,
	pub owner: String,
	pub repo: String,
	pub rev: String,
	pub nar_hash: String,
}

pub(crate) fn locked_github_inputs(lock: &FlakeLock) -> Vec<GhLocked> {
	let mut out = Vec::new();
	for name in root_input_names(lock) {
		let Some(node) = lock.nodes.get(&name) else {
			continue;
		};
		let Some(locked) = &node.locked else { continue };
		if locked.source_type() != Some("github") {
			continue; // only github tarball hashes are re-checkable here
		}
		if let (Some(owner), Some(repo), Some(rev), Some(nar_hash)) = (
			locked.owner(),
			locked.repo(),
			locked.rev(),
			locked.nar_hash(),
		) {
			out.push(GhLocked {
				name,
				owner: owner.to_string(),
				repo: repo.to_string(),
				rev: rev.to_string(),
				nar_hash: nar_hash.to_string(),
			});
		}
	}
	out
}

/// Advance every tracked github input to its latest upstream rev — the classic
/// `pin update`. For each root input that tracks a branch/tag (frozen and
/// non-github inputs are left as-is): resolve the ref to its newest commit and,
/// unless unchanged, rewrite that node's `locked` **in place**. Prints a
/// per-input line and a summary; returns the number of inputs that advanced.
///
/// `dry_run` resolves the rev for the preview but skips the hash prefetch and
/// makes **no** mutation. This function never touches the filesystem — the caller
/// persists the (mutated) lock when `!dry_run && returned > 0`.
pub fn update_tracked(lock: &mut FlakeLock, dry_run: bool) -> Result<usize> {
	let mut changed = 0;
	for tgt in tracking_github_inputs(lock)? {
		let old = tgt.locked_rev.as_deref();
		if dry_run {
			// Preview: the rev is enough; skip the hash prefetch.
			let rev = crate::nix::resolve_github_ref(&tgt.owner, &tgt.repo, &tgt.git_ref)?;
			if old == Some(rev.as_str()) {
				println!("{}: unchanged", tgt.name);
			} else {
				println!("{}: {} -> {} (dry run)", tgt.name, short_opt(old), short_rev(&rev));
				changed += 1;
			}
			continue;
		}
		let (rev, nar_hash) = crate::nix::resolve_github(&tgt.owner, &tgt.repo, &tgt.git_ref)?;
		if old == Some(rev.as_str()) {
			println!("{}: unchanged", tgt.name);
			continue;
		}
		lock.nodes
			.get_mut(&tgt.name)
			.expect("target exists")
			.locked = Some(Source::github_locked(
			&tgt.owner,
			&tgt.repo,
			rev.as_str(),
			nar_hash.as_str(),
		));
		println!("{}: {} -> {}", tgt.name, short_opt(old), short_rev(&rev));
		changed += 1;
	}
	println!(
		"clinix: {}",
		match (dry_run, changed) {
			(_, 0) => "all inputs up to date".to_string(),
			(true, n) => format!("{n} input(s) would update (dry run — not written)"),
			(false, n) => format!("{n} input(s) updated"),
		}
	);
	Ok(changed)
}

/// Print a `pin show`-style table of the lock's root inputs — node name, source
/// `type`, short locked rev, and what it tracks (`original.ref`, or `frozen` when
/// pinned to a rev). Read-only; used by `sys info`.
pub fn show_pins(lock: &FlakeLock) {
	println!("NAME\tTYPE\tREV\tTRACKING");
	for name in root_input_names(lock) {
		let Some(node) = lock.nodes.get(&name) else {
			continue;
		};
		let typ = node
			.locked
			.as_ref()
			.or(node.original.as_ref())
			.and_then(|s| s.source_type())
			.unwrap_or("-");
		let rev = node
			.locked
			.as_ref()
			.and_then(|l| l.rev())
			.map(short)
			.unwrap_or("-");
		let tracking = node
			.original
			.as_ref()
			.and_then(|o| {
				o.git_ref()
					.map(str::to_string)
					.or_else(|| o.rev().map(|_| "frozen".to_string()))
			})
			.unwrap_or_else(|| "-".to_string());
		println!("{name}\t{typ}\t{rev}\t{tracking}");
	}
}
