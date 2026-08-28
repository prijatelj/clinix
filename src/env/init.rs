//! `init`: scaffold a **project** env (`Kind::Project`) in a directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::Args;

use crate::env::{Context, Pkg, RunCmd};
use crate::error::{ClinixError, Result, unimplemented};
use crate::model::lock::{FlakeLock, InputRef, Node, Source};

/// Scaffold a **project** env (`Kind::Project`) in a directory. A project is
/// identified by its directory (not a name — `resolve` finds it by path/cwd, and
/// `new` owns named registry envs), so `init` takes only an optional `path`.
///
/// Default output is the primary pair — `shell.nix` (the vanilla `nix-shell`
/// entry point) + `flake.lock` (its version-controlled pin, which clinix owns
/// and writes classically, no `nix flake`). `--flake` additionally emits a
/// `flake.nix` that *wraps* `shell.nix`, so `nix develop`/flake users consume
/// the very same `flake.lock`.
#[derive(Args, Debug)]
pub struct Init {
	/// Directory to scaffold the project in (default: the current directory).
	pub path: Option<PathBuf>,
	/// Seed packages (`-p ripgrep -p nodejs`) written into the `packages` list.
	/// Versioned specs (`nodejs=20.11`) need package-level pinning (phase 4) and
	/// are rejected until then.
	#[arg(short = 'p', long = "pkg")]
	pub packages: Vec<Pkg>,
	/// nixpkgs ref to pin: a branch/tag (default `nixos-26.05`, the latest
	/// stable) or a 40-hex rev (frozen). clinix resolves and locks it classically
	/// (`git ls-remote` + `nix-prefetch-url`).
	#[arg(long = "nixpkgs", default_value = "nixos-26.05")]
	pub nixpkgs: String,
	/// Also emit `flake.nix` (wraps `shell.nix`) for `nix develop`/flake users.
	/// Off by default — `shell.nix` + `flake.lock` is the primary pair.
	#[arg(long = "flake")]
	pub flake: bool,
	/// Embed the lock inside `shell.nix` (one self-contained file; no separate
	/// `flake.lock`). Vanilla `nix-shell` only — mutually exclusive with `--flake`.
	#[arg(long = "single-file", conflicts_with = "flake")]
	pub single_file: bool,
	/// Adopt an existing spec (`pyproject.toml`, `Cargo.toml`, `shell.nix`, …).
	#[arg(long = "from")]
	pub from: Option<PathBuf>,
}
impl RunCmd for Init {
	/// Resolve + build `flake.lock` classically, then write `shell.nix` +
	/// `flake.lock` (and `flake.nix` under `--flake`). Never clobbers existing
	/// project files, and does the network resolution *before* any write so a
	/// failure leaves the directory untouched.
	fn run(self, _context: &Context) -> Result<()> {
		// `--from` adopt (ADR-2 state detection) and package-level version pins
		// are later slices; reject rather than silently drop their semantics.
		if self.from.is_some() {
			return Err(unimplemented(
				"env init --from",
				"plan phase 3+: adopt existing spec (ADR-2)",
			));
		}
		if self.packages.iter().any(|p| p.version.is_some()) {
			return Err(unimplemented(
				"env init with a versioned package",
				"plan phase 4: package-level version pinning",
			));
		}

		let root = match self.path {
			Some(ref p) => p.clone(),
			None => std::env::current_dir()?,
		};
		std::fs::create_dir_all(&root)?;

		// Clobber-check every target up front, before the network resolution, so
		// a pre-existing file aborts with nothing done.
		let shell_nix = root.join("shell.nix");
		let flake_lock = root.join("flake.lock");
		let flake_nix = root.join("flake.nix");
		ensure_absent(&shell_nix, &root)?;
		if !self.single_file {
			ensure_absent(&flake_lock, &root)?;
		}
		if self.flake {
			ensure_absent(&flake_nix, &root)?;
		}

		// Resolve nixpkgs and build the lock (network); nothing written on error.
		let lock = build_nixpkgs_lock(&self.nixpkgs)?;

		if self.single_file {
			// One self-contained file: the lock is embedded in shell.nix, no
			// flake.lock (like `--flake` adds a file, this removes one).
			std::fs::write(
				&shell_nix,
				render_shell_nix(&self.packages, Some(&lock.to_json())),
			)?;
		} else {
			std::fs::write(&shell_nix, render_shell_nix(&self.packages, None))?;
			std::fs::write(&flake_lock, lock.to_json())?;
		}
		if self.flake {
			std::fs::write(&flake_nix, render_flake_nix(&self.nixpkgs))?;
		}

		println!(
			"clinix: initialized project env at {} (nixpkgs/{})",
			root.display(),
			self.nixpkgs
		);
		if self.single_file {
			println!("clinix: single self-contained shell.nix (lock embedded; no flake.lock)");
		}
		if self.flake {
			println!("clinix: wrote flake.nix wrapping shell.nix (flake users: `nix develop`)");
		}
		Ok(())
	}
}

/// Base project templates, baked into the binary (a distributable can't `cp`
/// sibling files). `include_str!` keeps them as ordinary, lintable `.nix`; the
/// larger `shells/` seed catalog can move to `include_dir` in phase 5.
const SHELL_NIX_TEMPLATE: &str = include_str!("../../templates/project/shell.nix");
const FLAKE_NIX_TEMPLATE: &str = include_str!("../../templates/project/flake.nix");
/// The anchor line in `shell.nix`'s `packages = with pkgs; [ … ]` list.
const PACKAGES_PLACEHOLDER: &str = "    # project dependencies go here";

/// Fill the `packages` list with the seed package names (or leave the
/// placeholder comment when there are none), and — for `--single-file` — embed
/// the lock in place of the `./flake.lock` read. init *creates* the file from a
/// known template, so this string splice is sufficient — span-preserving
/// `rnix-parser` editing (phase 5, `add`) is only needed for existing,
/// hand-edited files.
fn render_shell_nix(packages: &[Pkg], embedded_lock: Option<&str>) -> String {
	let mut shell = if packages.is_empty() {
		SHELL_NIX_TEMPLATE.to_string()
	} else {
		let list = packages
			.iter()
			.map(|p| format!("    {}", p.name))
			.collect::<Vec<_>>()
			.join("\n");
		SHELL_NIX_TEMPLATE.replacen(PACKAGES_PLACEHOLDER, &list, 1)
	};
	if let Some(lock_json) = embedded_lock {
		shell = shell.replacen(
			"builtins.readFile ./flake.lock",
			&embed_lock_source(lock_json),
			1,
		);
	}
	shell
}

/// The single-file lock source: the `flake.lock` JSON as a nix `''…''`
/// here-string (identical bytes), escaping the two sequences nix interprets
/// (`''` and `${`). Replaces `builtins.readFile ./flake.lock` so the env needs no
/// separate `flake.lock`; [`crate::env::project`] reverses this to read it back.
fn embed_lock_source(lock_json: &str) -> String {
	let escaped = lock_json.replace("''", "'''").replace("${", "''${");
	format!("''\n{escaped}''")
}

/// Retarget the `flake.nix` template's nixpkgs URL to the chosen ref (the
/// template names `nixos-unstable` so it is valid Nix standalone; this is the
/// same one-literal swap `pin`'s `freeze` uses). The template already wraps
/// `shell.nix`, so flake users get the same lock.
fn render_flake_nix(nixpkgs_ref: &str) -> String {
	FLAKE_NIX_TEMPLATE.replace(
		"github:NixOS/nixpkgs/nixos-unstable",
		&format!("github:NixOS/nixpkgs/{nixpkgs_ref}"),
	)
}

/// **Never-clobber**: an existing target file is an error, not an overwrite
/// (this is not an adopt).
fn ensure_absent(path: &Path, root: &Path) -> Result<()> {
	if path.exists() {
		return Err(ClinixError::AmbiguousInit {
			path: root.display().to_string(),
			detail: format!(
				"`{}` already exists; refusing to overwrite (this is not an adopt)",
				path.file_name().unwrap().to_string_lossy()
			),
		});
	}
	Ok(())
}

/// Resolve the nixpkgs ref and assemble a classic `flake.lock` for it — clinix
/// owns this state; no `nix flake`. A 40-hex `nixpkgs_ref` is recorded as a
/// frozen `original.rev`; anything else is a tracked `original.ref` resolved via
/// `git ls-remote`. The single node is a github source
/// (`{narHash, owner, repo, rev, type}`), mirroring `pin init` + `pin update`.
fn build_nixpkgs_lock(nixpkgs_ref: &str) -> Result<FlakeLock> {
	const OWNER: &str = "NixOS";
	const REPO: &str = "nixpkgs";

	// A 40-hex ref is a frozen rev (hash it directly); anything else is a tracked
	// branch/tag (resolve it first). `resolve_github`/`Source::github_*` are the
	// same helpers `update` uses.
	let frozen = is_rev(nixpkgs_ref);
	let (rev, nar_hash) = if frozen {
		let nar_hash = crate::nix::github_tarball_narhash(OWNER, REPO, nixpkgs_ref)?;
		(nixpkgs_ref.to_string(), nar_hash)
	} else {
		let (rev, nar_hash) = crate::nix::resolve_github(OWNER, REPO, nixpkgs_ref)?;
		(rev.as_str().to_string(), nar_hash)
	};

	let nixpkgs_node = Node {
		locked: Some(Source::github_locked(OWNER, REPO, &rev, nar_hash.as_str())),
		original: Some(if frozen {
			Source::github_rev(OWNER, REPO, &rev)
		} else {
			Source::github_ref(OWNER, REPO, nixpkgs_ref)
		}),
		..Node::default()
	};
	let mut root_inputs = BTreeMap::new();
	root_inputs.insert(
		"nixpkgs".to_string(),
		InputRef::Direct("nixpkgs".to_string()),
	);
	let root_node = Node {
		inputs: root_inputs,
		..Node::default()
	};

	let mut nodes = BTreeMap::new();
	nodes.insert("nixpkgs".to_string(), nixpkgs_node);
	nodes.insert("root".to_string(), root_node);

	Ok(FlakeLock {
		nodes,
		root: "root".to_string(),
		version: 7,
	})
}

/// A 40-char lowercase-hex string — the shape Nix records as `original.rev`
/// (frozen). Matches `pin`'s `is_rev`.
fn is_rev(s: &str) -> bool {
	s.len() == 40
		&& s.bytes()
			.all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// Private-helper unit tests live inline (hybrid layout): integration tests in
// `tests/` can only reach `pub` API, so these stay beside the code they cover.
#[cfg(test)]
mod tests {
	use super::*;

	fn pkg(name: &str) -> Pkg {
		Pkg {
			name: name.to_string(),
			version: None,
		}
	}

	#[test]
	fn render_shell_nix_splices_packages() {
		let shell = render_shell_nix(&[pkg("ripgrep"), pkg("nodejs")], None);
		assert!(
			shell.contains("    ripgrep\n    nodejs"),
			"packages spliced"
		);
		assert!(
			!shell.contains("# project dependencies go here"),
			"placeholder replaced"
		);
	}

	#[test]
	fn render_shell_nix_empty_keeps_placeholder() {
		assert!(render_shell_nix(&[], None).contains("# project dependencies go here"));
	}

	#[test]
	fn render_shell_nix_single_file_embeds_lock() {
		let lock = r#"{ "nodes": {}, "root": "root", "version": 7 }"#;
		let shell = render_shell_nix(&[], Some(lock));
		// The lock is embedded as a here-string; the file read is gone.
		assert!(
			!shell.contains("readFile ./flake.lock"),
			"file read removed"
		);
		assert!(
			shell.contains("builtins.fromJSON (''"),
			"embedded here-string"
		);
		assert!(shell.contains(r#""version": 7"#), "lock JSON present");
	}

	#[test]
	fn render_flake_nix_retargets_the_ref() {
		let flake = render_flake_nix("nixos-26.05");
		assert!(flake.contains("github:NixOS/nixpkgs/nixos-26.05"));
		assert!(!flake.contains("nixos-unstable"));
	}

	#[test]
	fn ensure_absent_errors_on_existing_and_leaves_it() {
		let dir = tempfile::tempdir().unwrap();
		let f = dir.path().join("shell.nix");
		std::fs::write(&f, "hand-written").unwrap();
		let err = ensure_absent(&f, dir.path()).unwrap_err();
		assert!(matches!(err, ClinixError::AmbiguousInit { .. }));
		assert_eq!(std::fs::read_to_string(&f).unwrap(), "hand-written");
		// Absent path is fine.
		assert!(ensure_absent(&dir.path().join("flake.lock"), dir.path()).is_ok());
	}

	#[test]
	fn is_rev_matches_40_hex_only() {
		assert!(is_rev("062346a6d85bc4b49dfaa61c986e9c5be21217d1"));
		assert!(!is_rev("nixos-26.05"));
		assert!(!is_rev("ABC")); // too short / uppercase
	}

	// Network + classic-nix E2E: resolves nixos-26.05 and checks the built lock
	// round-trips through the model and has a github nixpkgs node. Gated so
	// offline/CI runs pass; run with `cargo test -- --ignored`.
	#[test]
	#[ignore = "requires network + git/nix-prefetch-url/nix-hash"]
	fn build_nixpkgs_lock_resolves_and_roundtrips() {
		let lock = build_nixpkgs_lock("nixos-26.05").expect("resolve+build");
		let nixpkgs = &lock.nodes["nixpkgs"];
		let locked = nixpkgs.locked.as_ref().unwrap();
		assert_eq!(locked.source_type(), Some("github"));
		assert_eq!(locked.owner(), Some("NixOS"));
		assert_eq!(locked.repo(), Some("nixpkgs"));
		assert!(locked.rev().is_some_and(|r| r.len() == 40));
		assert!(locked.nar_hash().is_some_and(|h| h.starts_with("sha256-")));
		// original tracks the branch ref.
		assert_eq!(
			nixpkgs.original.as_ref().unwrap().git_ref(),
			Some("nixos-26.05")
		);
		// Serializes to a well-formed, re-parseable lock.
		let text = lock.to_json();
		assert_eq!(FlakeLock::from_json(&text).unwrap(), lock);
	}
}
