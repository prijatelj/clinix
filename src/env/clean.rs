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

	if unresolved.is_empty() {
		Ok(())
	} else {
		Err(ClinixError::UnknownEnv(unresolved.join(", ")))
	}
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
