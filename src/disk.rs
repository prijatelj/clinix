//! On-disk size accounting — pure Rust, no `du`/`dust` shell-out.
//!
//! Replaces the `du -sc` the `deps` prototype used: the nix store is heavily
//! hardlinked (`nix-store --optimise` shares identical files across store paths
//! by inode), so a truthful closure size must count each `(device, inode)`
//! **once** — the same dedup dust does in `clean_inodes`. Walks with `walkdir`
//! and parallelizes across the closure's paths with `rayon`.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;

use rayon::prelude::*;
use walkdir::WalkDir;

/// Total on-disk size (bytes) of `paths`, **hardlink-aware**: each unique
/// `(device, inode)` is counted once, so files hardlinked across store paths
/// (common in the nix store) are not double-counted. `blocks() * 512` is the real
/// on-disk usage (matching `du`, not `du --apparent-size`). Unreadable entries
/// are skipped, like `du`.
pub fn total_bytes(paths: &[String]) -> u64 {
	// Walk each closure path in parallel, mapping every entry's inode → on-disk
	// bytes. A `HashMap` keyed by `(dev, ino)` dedups within a path; merging the
	// per-path maps dedups hardlinks that span paths.
	let per_path: Vec<HashMap<(u64, u64), u64>> = paths
		.par_iter()
		.map(|path| {
			let mut sizes = HashMap::new();
			for entry in WalkDir::new(path).into_iter().filter_map(Result::ok) {
				if let Ok(md) = entry.metadata() {
					sizes.insert((md.dev(), md.ino()), md.blocks() * 512);
				}
			}
			sizes
		})
		.collect();

	let mut seen: HashMap<(u64, u64), u64> = HashMap::new();
	for map in per_path {
		seen.extend(map);
	}
	seen.values().sum()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn counts_hardlinks_once() {
		let dir = tempfile::tempdir().unwrap();
		let a = dir.path().join("a");
		std::fs::write(&a, vec![0u8; 8192]).unwrap();
		let b = dir.path().join("b");
		std::fs::hard_link(&a, &b).unwrap(); // same inode as `a`

		let paths = vec![
			a.to_string_lossy().into_owned(),
			b.to_string_lossy().into_owned(),
		];
		let bytes = total_bytes(&paths);
		// One 8 KiB inode, counted once — not ~16 KiB (which double-counting gives).
		assert!(
			(8192..16384).contains(&bytes),
			"expected one 8 KiB file, got {bytes} bytes"
		);
	}
}
