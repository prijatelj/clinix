//! `shared` (the former `shareck`, generalized 2→N): N-way shared-package
//! comparison across several envs' runtime closures.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::env::{Context, Targets, resolve};
use crate::error::{ClinixError, Result};

use super::human_bytes;

/// N-way shared-package comparison across several envs (the former `shareck`,
/// generalized from 2 to N). Instantiates each env's `shell.nix` and compares the
/// runtime closures: the global shared core, each env's unique set, a pairwise
/// shared-percentage matrix, and the on-disk cost of each set. Shared paths are
/// content-addressed (cost disk once), so a high shared count is the good case.
pub fn shared(targets: Targets, context: &Context) -> Result<()> {
	let mut sets: Vec<(String, BTreeSet<PathBuf>)> = Vec::new();
	for name in &targets.names {
		let env = resolve(&context.config, Some(name))?;
		let shell_nix = env.root.join("shell.nix");
		if !shell_nix.is_file() {
			return Err(ClinixError::Resolve(format!(
				"no shell.nix at {} (run: clinix env init)",
				env.root.display()
			)));
		}
		let drv = crate::nix::instantiate(&shell_nix)?;
		let closure: BTreeSet<PathBuf> = crate::nix::closure(&drv)?
			.into_iter()
			.map(PathBuf::from)
			.collect();
		sets.push((name.clone(), closure));
	}
	let report = analyze_sharing(&sets);

	println!("== counts");
	for (name, total) in &report.totals {
		println!("  {name:<16} {total} paths");
	}
	println!(
		"  {:<16} {} paths  (cost disk once)",
		"shared (all)", report.global_shared
	);
	for (name, uniq) in &report.unique {
		println!("  unique to {name:<6} {uniq} paths");
	}

	if report.matrix.len() > 1 {
		println!("\n== pairwise shared (% of row env's closure)");
		for (a, b, pct) in &report.matrix {
			println!("  {a:<16} ∩ {b:<16} {pct}%");
		}
	}

	println!("\n== disk (hardlink-aware)");
	println!(
		"  {:<20} {}",
		"shared (counted once)",
		human_bytes(disk_of(&report.shared_paths))
	);
	for (name, paths) in &report.unique_paths {
		println!("  unique to {name:<10} {}", human_bytes(disk_of(paths)));
	}
	Ok(())
}

/// The pure set-arithmetic of `shared`, separated from the nix I/O so it is
/// unit-testable on synthetic closures.
struct SharingReport {
	totals: Vec<(String, usize)>,
	global_shared: usize,
	unique: Vec<(String, usize)>,
	/// Row-relative: `(a, b, |a∩b|·100/|a|)`.
	matrix: Vec<(String, String, usize)>,
	shared_paths: Vec<PathBuf>,
	unique_paths: Vec<(String, Vec<PathBuf>)>,
}

fn analyze_sharing(sets: &[(String, BTreeSet<PathBuf>)]) -> SharingReport {
	let totals = sets.iter().map(|(n, s)| (n.clone(), s.len())).collect();

	// Global shared = fold intersection across every set.
	let global: BTreeSet<PathBuf> = match sets.split_first() {
		Some(((_, first), rest)) => rest.iter().fold(first.clone(), |acc, (_, s)| {
			acc.intersection(s).cloned().collect()
		}),
		None => BTreeSet::new(),
	};

	// Per env: paths in it but in no other set.
	let mut unique = Vec::new();
	let mut unique_paths = Vec::new();
	for (i, (name, set)) in sets.iter().enumerate() {
		let others: BTreeSet<&PathBuf> = sets
			.iter()
			.enumerate()
			.filter(|(j, _)| *j != i)
			.flat_map(|(_, (_, s))| s.iter())
			.collect();
		let only: Vec<PathBuf> = set.iter().filter(|p| !others.contains(p)).cloned().collect();
		unique.push((name.clone(), only.len()));
		unique_paths.push((name.clone(), only));
	}

	// Pairwise, row-relative percentage.
	let mut matrix = Vec::new();
	for (i, (a, sa)) in sets.iter().enumerate() {
		for (j, (b, sb)) in sets.iter().enumerate() {
			if i == j {
				continue;
			}
			let inter = sa.intersection(sb).count();
			let pct = if sa.is_empty() { 0 } else { inter * 100 / sa.len() };
			matrix.push((a.clone(), b.clone(), pct));
		}
	}

	SharingReport {
		totals,
		global_shared: global.len(),
		unique,
		matrix,
		shared_paths: global.into_iter().collect(),
		unique_paths,
	}
}

/// Hardlink-aware on-disk size of a set of store paths (`disk.rs` dedups by
/// `(device, inode)` — the store is heavily hardlinked).
fn disk_of(paths: &[PathBuf]) -> u64 {
	let as_str: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
	crate::disk::total_bytes(&as_str)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn set(paths: &[&str]) -> BTreeSet<PathBuf> {
		paths.iter().map(PathBuf::from).collect()
	}

	#[test]
	fn analyze_sharing_computes_global_core_and_uniques() {
		let sets = vec![
			("a".to_string(), set(&["/s/common", "/s/a1", "/s/a2"])),
			("b".to_string(), set(&["/s/common", "/s/b1"])),
			("c".to_string(), set(&["/s/common", "/s/a1", "/s/c1"])),
		];
		let r = analyze_sharing(&sets);
		assert_eq!(r.global_shared, 1); // only /s/common is in all three
		// a's uniques: a2 only (a1 is also in c, common is shared).
		assert_eq!(r.unique[0], ("a".to_string(), 1));
		assert_eq!(r.unique[1], ("b".to_string(), 1)); // b1
		// a∩b = {common} = 1 of a's 3 → 33%.
		let ab = r.matrix.iter().find(|(x, y, _)| x == "a" && y == "b").unwrap();
		assert_eq!(ab.2, 33);
	}

	#[test]
	fn analyze_sharing_identical_sets_are_fully_shared() {
		let s = set(&["/s/x", "/s/y"]);
		let sets = vec![("a".to_string(), s.clone()), ("b".to_string(), s)];
		let r = analyze_sharing(&sets);
		assert_eq!(r.global_shared, 2);
		assert_eq!(r.unique[0].1, 0);
		assert_eq!(r.matrix[0].2, 100); // a∩b = all of a
	}
}
