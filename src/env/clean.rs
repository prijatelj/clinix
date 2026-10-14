//! `clean`: release GC roots so their store paths can be collected. Supports
//! per-name release, version-targeting (`--root-version`/`--oldest`), composed-union
//! targeting (`--union`, or the global `-o`), and bulk selection (`--projects`,
//! `--stacks`, `--match <glob>`).

use std::path::PathBuf;

use clap::Args;

use crate::env::config::Settings;
use crate::env::seeds::Catalog;
use crate::env::{Context, node_root, registry, resolve_stack_key};
use crate::error::{ClinixError, Result};

/// `clean` arguments. Target selection is one of: **per-name** (default — each name
/// released independently), **union** (`--union`, or the global `-o`; the one
/// `stack-<…>` key those names compose), or **bulk** (`--projects`/`--stacks` by kind,
/// `--match <glob>` by key). Release scope defaults to *all versions* of each target;
/// `--root-version <SEQ>`/`--oldest <N>` narrow it (single target only).
#[derive(Args, Debug)]
pub struct Clean {
	/// Env names to release (per-name), or the members of a `--union`/`-o` stack.
	pub names: Vec<String>,
	/// Release only the exact root version with this id (single target only).
	#[arg(long, conflicts_with = "oldest")]
	pub root_version: Option<u64>,
	/// Release only the `N` oldest versions, keeping the newer ones (single target only).
	#[arg(long, conflicts_with = "root_version")]
	pub oldest: Option<usize>,
	/// Treat `names` as one composed union and release that `stack-<…>` root (ordered
	/// under the global `-o`, else sorted) — the root a `shell`/`run` composition made.
	#[arg(long)]
	pub union: bool,
	/// Bulk: release **all** project (`proj-*`) roots.
	#[arg(long)]
	pub projects: bool,
	/// Bulk: release **all** stack/union (`stack-*`) roots.
	#[arg(long)]
	pub stacks: bool,
	/// Bulk: release every root whose key matches this `*`-glob (e.g. `*-rust*`).
	#[arg(long = "match")]
	pub match_glob: Option<String>,
	/// Show what would be released without deleting anything (a preview).
	#[arg(long)]
	pub dry_run: bool,
}

/// Release GC roots. Only the root symlinks are touched — an env's
/// `shell.nix`/`flake.lock` are untouched, and an absent target is a reported no-op.
pub fn clean(args: Clean, context: &Context) -> Result<()> {
	let cfg = &context.config;
	let settings = Settings::load(&cfg.config_dir)?;
	let catalog = Catalog::build(&settings.env.seeds);
	for w in &catalog.warnings {
		eprintln!("clinix: {w}");
	}

	let ordered = context.options.ordered; // global `-o`
	let bulk = args.projects || args.stacks || args.match_glob.is_some();
	let union_mode = args.union || (ordered && !args.names.is_empty());
	let version_targeted = args.root_version.is_some() || args.oldest.is_some();

	// Resolve the set of target base keys (label, base path), tracking unresolved
	// per-name lookups so a typo is reported but does not abort the rest.
	let mut unresolved: Vec<String> = Vec::new();
	let targets: Vec<(String, PathBuf)> = if bulk {
		if union_mode || !args.names.is_empty() {
			return Err(ClinixError::Config(
				"bulk selectors (--projects/--stacks/--match) take no names and are not combined \
				 with --union/-o"
					.to_string(),
			));
		}
		select_bulk(cfg, &args)?
	} else if union_mode {
		if args.names.is_empty() {
			return Err(ClinixError::Config(
				"--union / -o needs env names to compose into a stack".to_string(),
			));
		}
		let key = resolve_stack_key(cfg, &catalog, &args.names, ordered)?;
		vec![(key.clone(), registry::roots_dir(cfg).join(key))]
	} else {
		if args.names.is_empty() {
			return Err(ClinixError::Config(
				"specify env names, --union/-o, or a bulk selector (--projects/--stacks/--match)"
					.to_string(),
			));
		}
		let mut v = Vec::new();
		for name in &args.names {
			match node_root(cfg, &catalog, name) {
				Ok(base) => v.push((name.clone(), base)),
				Err(e) => {
					eprintln!("clinix: `{name}` — {e}");
					unresolved.push(name.clone());
				}
			}
		}
		v
	};

	if version_targeted && targets.len() != 1 {
		return Err(ClinixError::Config(
			"`--root-version` / `--oldest` target a single env's versions — narrow to one target"
				.to_string(),
		));
	}

	for (label, base) in &targets {
		release_one(label, base, &args)?;
	}
	if targets.is_empty() && unresolved.is_empty() {
		println!("clinix: no matching roots");
	}

	// Generated compose expressions outlive the roots they were written for — an
	// ephemeral `-p` shell never roots one at all. Sweep the orphans here, after
	// the releases above, so a root just released takes its compose file with it.
	sweep_compose(cfg, args.dry_run)?;

	if unresolved.is_empty() {
		Ok(())
	} else {
		Err(ClinixError::UnknownEnv(unresolved.join(", ")))
	}
}

/// Delete generated compose expressions in `state/compose/` that nothing uses: a
/// `<key>.nix` is **orphaned** when no GC root for `<key>` remains — neither a
/// version `roots/<key>@<seq>` nor a bare `roots/<key>` (the unversioned shape
/// clinix wrote before root versioning; still a live indirect root, so it counts
/// as in-use).
///
/// Two ways one becomes orphaned: the union's roots were just released (so the
/// expression is dead with them), or it was never rooted in the first place — the
/// ephemeral `-p` case, whose file is a pure cache. A registered env is unaffected:
/// `state/envs/<name>/shell.nix` is its own file and is never written here.
///
/// Deleting one is always safe — the next entry of that composition rewrites it.
/// Best-effort and silent when there is nothing to do; a read error is not worth
/// failing a `clean` over.
fn sweep_compose(cfg: &crate::env::config::Config, dry_run: bool) -> Result<()> {
	let dir = registry::compose_dir(cfg);
	let read = match std::fs::read_dir(&dir) {
		Ok(r) => r,
		Err(_) => return Ok(()), // no compose dir yet
	};
	let mut swept = 0usize;
	for entry in read {
		let Ok(entry) = entry else { continue };
		let path = entry.path();
		if path.extension().is_none_or(|e| e != "nix") {
			continue;
		}
		let Some(key) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
			continue;
		};
		// Still rooted ⇒ still in use; leave it alone. Checked both ways: a
		// versioned root, or a bare unversioned one.
		let root_base = registry::roots_dir(cfg).join(&key);
		let rooted = !registry::list_versions(&root_base)?.is_empty()
			|| root_base.symlink_metadata().is_ok();
		if rooted {
			continue;
		}
		if dry_run {
			println!("clinix: would sweep unused compose file `{key}.nix`");
		} else if std::fs::remove_file(&path).is_ok() {
			swept += 1;
		}
	}
	if swept > 0 {
		println!("clinix: swept {swept} unused compose file(s)");
	}
	Ok(())
}

/// Enumerate the bulk-selected base keys, filtered by kind (`--projects`/`--stacks`)
/// and/or key glob (`--match`).
fn select_bulk(cfg: &crate::env::config::Config, args: &Clean) -> Result<Vec<(String, PathBuf)>> {
	let mut keys = registry::list_root_keys(cfg)?;
	if args.projects || args.stacks {
		keys.retain(|k| {
			(args.projects && k.starts_with("proj-")) || (args.stacks && k.starts_with("stack-"))
		});
	}
	if let Some(glob) = &args.match_glob {
		keys.retain(|k| glob_match(glob.as_bytes(), k.as_bytes()));
	}
	Ok(keys
		.into_iter()
		.map(|k| (k.clone(), registry::roots_dir(cfg).join(k)))
		.collect())
}

/// Apply the release scope (exact version / oldest N / all) to one target base.
/// With `--dry-run`, report what *would* be released without deleting anything.
fn release_one(label: &str, base: &std::path::Path, args: &Clean) -> Result<()> {
	if args.dry_run {
		let versions = registry::list_versions(base)?;
		if let Some(seq) = args.root_version {
			if versions.iter().any(|v| v.seq == seq) {
				println!("clinix: would release `{label}` root version @{seq}");
			} else {
				println!("clinix: `{label}` has no root version @{seq}");
			}
		} else if let Some(n) = args.oldest {
			let ids: Vec<String> = versions.iter().take(n).map(|v| format!("@{}", v.seq)).collect();
			if ids.is_empty() {
				println!("clinix: `{label}` had no versions to release");
			} else {
				println!(
					"clinix: would release {} oldest version(s) of `{label}`: {}",
					ids.len(),
					ids.join(" ")
				);
			}
		} else if versions.is_empty() {
			println!("clinix: `{label}` had no GC root (nothing to release)");
		} else {
			println!(
				"clinix: would release all {} version(s) of `{label}`",
				versions.len()
			);
		}
		return Ok(());
	}
	if let Some(seq) = args.root_version {
		if registry::release_version(base, seq)? {
			println!("clinix: released `{label}` root version @{seq}");
		} else {
			println!("clinix: `{label}` has no root version @{seq}");
		}
	} else if let Some(n) = args.oldest {
		let removed = registry::release_oldest(base, n)?;
		if removed.is_empty() {
			println!("clinix: `{label}` had no versions to release");
		} else {
			let ids: Vec<String> = removed.iter().map(|s| format!("@{s}")).collect();
			println!(
				"clinix: released {} oldest version(s) of `{label}`: {}",
				removed.len(),
				ids.join(" ")
			);
		}
	} else if registry::release_root(base)? {
		println!("clinix: released GC root(s) for `{label}`");
	} else {
		println!("clinix: `{label}` had no GC root (nothing to release)");
	}
	Ok(())
}

/// Minimal `*`-wildcard glob match (only `*` is special; matches any run, including
/// empty). Iterative with backtracking — no regex dependency. `*-rust*` matches any
/// key containing `-rust`.
fn glob_match(pat: &[u8], s: &[u8]) -> bool {
	let (mut p, mut i) = (0usize, 0usize);
	let (mut star, mut mark) = (None, 0usize);
	while i < s.len() {
		if p < pat.len() && pat[p] == b'*' {
			star = Some(p);
			mark = i;
			p += 1;
		} else if p < pat.len() && pat[p] == s[i] {
			p += 1;
			i += 1;
		} else if let Some(sp) = star {
			p = sp + 1;
			mark += 1;
			i = mark;
		} else {
			return false;
		}
	}
	while p < pat.len() && pat[p] == b'*' {
		p += 1;
	}
	p == pat.len()
}

#[cfg(test)]
mod tests {
	use super::glob_match;

	fn m(pat: &str, s: &str) -> bool {
		glob_match(pat.as_bytes(), s.as_bytes())
	}

	#[test]
	fn glob_star_matches_substring_and_anchors() {
		assert!(m("*rust*", "proj-home_u_rust-app"), "substring rust");
		assert!(m("*-rust*", "stack-rust-claude"), "contains -rust");
		assert!(!m("*-rust*", "proj-home_u_rust"), "has _rust, not -rust");
		assert!(!m("*-rust*", "env-python"));
		assert!(m("env-*", "env-python"));
		assert!(!m("env-*", "proj-python"));
		assert!(m("*", "anything"));
		assert!(m("stack-rust-claude", "stack-rust-claude")); // exact, no `*`
		assert!(!m("stack-rust", "stack-rust-claude")); // exact must be full
	}
}
