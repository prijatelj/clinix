//! Packages: the [`Pkg`] spec (`name[=version]`) plus the package-mutating verbs
//! — `add`/`remove` (`shell.nix` splice), `pin`/`unpin` (`flake.lock`), and
//! `update`.

use clap::Args;

use crate::env::project::Project;
use crate::env::{Context, RunCmd, resolve};
use crate::error::{ClinixError, Result, unimplemented};
use crate::model::lock::{InputRef, Source};

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
}

/// Package pin/unpin selection (shared by `pin`/`unpin`).
#[derive(Args, Debug)]
pub struct Pin {
	/// Target env name.
	pub name: String,
	/// Packages to pin/unpin. Empty with `--all` operates on the whole closure.
	pub packages: Vec<Pkg>,
	/// Freeze/unfreeze every package (closure-equivalent full pin).
	#[arg(long)]
	pub all: bool,
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

		let mut project = Project::load(resolve(Some(&self.name))?)?;

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
			let old_rev = node.locked.as_ref().and_then(|l| l.rev()).map(str::to_string);

			let (rev, nar_hash) = crate::nix::resolve_github(&owner, &repo, &git_ref)?;
			if old_rev.as_deref() == Some(rev.as_str()) {
				println!("{name}: unchanged");
				continue;
			}
			project.lock.nodes.get_mut(&name).expect("target exists").locked =
				Some(Source::github_locked(&owner, &repo, rev.as_str(), nar_hash.as_str()));
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
		root_inputs.insert("nixpkgs".to_string(), InputRef::Direct("nixpkgs".to_string()));
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

		let out = FlakeLock::from_json(
			&std::fs::read_to_string(dir.path().join("flake.lock")).unwrap(),
		)
		.unwrap();
		let new_rev = out.nodes["nixpkgs"].locked.as_ref().unwrap().rev().unwrap();
		assert_ne!(new_rev, stale, "nixos-26.05 should have advanced");
		assert_eq!(new_rev.len(), 40);
	}
}

/// Add packages to an env's `shell.nix` (rnix-parser splice).
pub fn add(_args: Pkgs, _context: &Context) -> Result<()> {
	Err(unimplemented("env add", "plan phase 5: rnix-parser splice"))
}

/// Remove packages from an env's `shell.nix` (rnix-parser splice).
pub fn remove(_args: Pkgs, _context: &Context) -> Result<()> {
	Err(unimplemented("env remove", "plan phase 5: rnix-parser splice"))
}

/// Pin package versions in `flake.lock` (`--all` = closure freeze).
pub fn pin(_args: Pin, _context: &Context) -> Result<()> {
	Err(unimplemented("env pin", "plan phase 3/4: flake.lock version pin"))
}

/// Unpin packages back to baseline tracking (`--all` = unfreeze).
pub fn unpin(_args: Pin, _context: &Context) -> Result<()> {
	Err(unimplemented("env unpin", "plan phase 3/4: flake.lock unpin"))
}
