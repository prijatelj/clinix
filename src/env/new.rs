//! `new`: create a **registry** env by materializing a stack of seeds
//! (`--from A B C`, order = merge order). See
//! `notes/clinix/design/seed-catalog-and-config.md` §6.
//!
//! The seed fragments are **copied** into `envs/<name>/seeds/` and wrapped by a
//! self-contained `shell.nix` (reads `./flake.lock`, imports the pinned nixpkgs,
//! unions the copies via `inputsFrom`), so the registry env is **portable** — a
//! frozen snapshot that does not depend on the user's source paths.
//!
//! No drift hashing: a materialized env is deliberately frozen, and Nix already
//! rebuilds on a seed *update* wherever the seed is read live — the `shell`/`run`
//! launcher imports seed *sources* directly, so re-running rebuilds on change.

use std::path::PathBuf;

use clap::Args;

use crate::env::config::Settings;
use crate::env::seeds::{Catalog, Resolved};
use crate::env::{Context, RunCmd, registry};
use crate::error::{ClinixError, Result};

/// Create a new **registry** env by merging seeds. Distinct from
/// [`super::init::Init`] (which scaffolds a *project* in a directory): `new`
/// writes a reusable, named env into the registry. `--from` order is the merge
/// order. Sources are **seed names** for now (registry-env merge and the cwd
/// token `.` are deferred with self-contained composition).
#[derive(Args, Debug)]
pub struct New {
	/// Name of the new registry env.
	pub name: String,
	/// Seeds to merge, in stack order (`--from A B C`).
	#[arg(long = "from", num_args = 1.., required = true)]
	pub from: Vec<String>,
}

impl RunCmd for New {
	fn run(self, ctx: &Context) -> Result<()> {
		let cfg = &ctx.config;
		registry::validate_name(&self.name)?;
		let dest = registry::env_root(cfg, &self.name);
		if dest.exists() {
			return Err(ClinixError::EnvExists(self.name.clone()));
		}

		// Resolve each --from name to a seed (order preserved = merge order).
		let settings = Settings::load(&cfg.config_dir)?;
		let catalog = Catalog::build(&settings.env.seeds);
		let mut seeds: Vec<(String, PathBuf)> = Vec::with_capacity(self.from.len());
		for name in &self.from {
			match catalog.find(name) {
				Some(Resolved::One(s)) => seeds.push((s.name.clone(), s.path.clone())),
				Some(Resolved::Collision { chosen, .. }) => {
					seeds.push((chosen.name.clone(), chosen.path.clone()))
				}
				None => {
					return Err(ClinixError::Config(format!(
						"`new --from` names must be seeds; `{name}` is not one \
						 (registry-env merge is deferred)"
					)));
				}
			}
		}

		// The nixpkgs pin the composed env builds against (network on first use).
		let lock = super::env::seed_lock(cfg, &settings)?;

		// Materialize envs/<name>/: seed copies + flake.lock + self-contained shell.nix.
		let seeds_dir = dest.join("seeds");
		std::fs::create_dir_all(&seeds_dir)?;
		for (sname, spath) in &seeds {
			std::fs::copy(spath, seeds_dir.join(format!("{sname}.nix")))?;
		}
		std::fs::copy(&lock, dest.join("flake.lock"))?;
		let label = seeds
			.iter()
			.map(|(n, _)| n.as_str())
			.collect::<Vec<_>>()
			.join(" ");
		std::fs::write(dest.join("shell.nix"), render_shell_nix(&seeds, &label))?;

		println!(
			"clinix: created registry env `{}` from {} seed(s): {label}",
			self.name,
			seeds.len()
		);
		println!("  enter: clinix env {}", self.name);
		Ok(())
	}
}

/// The self-contained `shell.nix` for a materialized registry env: reads
/// `./flake.lock`, imports the pinned nixpkgs, and unions the local seed copies
/// via `inputsFrom`. Portable — no reference to the user's source paths.
fn render_shell_nix(seeds: &[(String, PathBuf)], label: &str) -> String {
	let imports = seeds
		.iter()
		.map(|(n, _)| format!("    (import ./seeds/{n}.nix {{ inherit pkgs; }})"))
		.collect::<Vec<_>>()
		.join("\n");
	let sanitized = label.replace(' ', "-");
	SHELL_NIX_TEMPLATE
		.replace("@NAME@", &sanitized)
		.replace("@IMPORTS@", &imports)
		.replace("@LABEL@", label)
}

const SHELL_NIX_TEMPLATE: &str = r##"{ system ? builtins.currentSystem }:
let
  lock = builtins.fromJSON (builtins.readFile ./flake.lock);
  fetch = node:
    let i = lock.nodes.${node}.locked; in
    if i.type == "github" then
      builtins.fetchTarball { url = "https://github.com/${i.owner}/${i.repo}/archive/${i.rev}.tar.gz"; sha256 = i.narHash; }
    else if i.type == "git" then
      (builtins.fetchGit { inherit (i) url rev; }).outPath
    else throw "clinix: unsupported input type '${i.type}'";
  sources = builtins.mapAttrs (_: fetch) lock.nodes.root.inputs;
  pkgs = import sources.nixpkgs { inherit system; };
in
pkgs.mkShell {
  name = "@NAME@";
  inputsFrom = [
@IMPORTS@
  ];
  shellHook = "export name=${pkgs.lib.escapeShellArg ''@LABEL@''}\n";
}
"##;

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn render_shell_nix_is_self_contained_and_portable() {
		let seeds = vec![
			("rust".to_string(), PathBuf::from("/src/rust.nix")),
			("claude".to_string(), PathBuf::from("/src/claude.nix")),
		];
		let s = render_shell_nix(&seeds, "rust claude");
		// Portable: reads the local lock + local seed copies, not the source paths.
		assert!(s.contains("builtins.readFile ./flake.lock"));
		assert!(s.contains("import ./seeds/rust.nix { inherit pkgs; }"));
		assert!(s.contains("import ./seeds/claude.nix { inherit pkgs; }"));
		assert!(!s.contains("/src/"), "must not reference source paths");
		assert!(s.contains("name = \"rust-claude\";"));
	}
}
