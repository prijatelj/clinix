//! `new`: create a **registry** env by materializing a stack of seeds
//! (`--from A B C`, order = merge order). See
//! `notes/clinix/design/seed-catalog-and-config.md` §6.
//!
//! The seed fragments are **copied** into `envs/<name>/seeds/` and wrapped by a
//! self-contained `shell.nix` (reads `./flake.lock`, imports the pinned nixpkgs,
//! unions the copies via `inputsFrom`), so the registry env is **portable** — it
//! does not depend on the user's source paths. Each source's path + content
//! `sha256` are recorded in a `clinixEnv` attribute (evaluable, no sidecar) so
//! `info` can warn when a source has **drifted** since materialization.

use std::path::{Path, PathBuf};

use clap::Args;
use sha2::{Digest, Sha256};

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
		let mut provenance: Vec<Provenance> = Vec::with_capacity(seeds.len());
		for (sname, spath) in &seeds {
			let content = std::fs::read(spath)?;
			std::fs::write(seeds_dir.join(format!("{sname}.nix")), &content)?;
			provenance.push(Provenance {
				name: sname.clone(),
				source: spath.clone(),
				sha256: sha256_hex(&content),
			});
		}
		std::fs::copy(&lock, dest.join("flake.lock"))?;
		let label = seeds
			.iter()
			.map(|(n, _)| n.as_str())
			.collect::<Vec<_>>()
			.join(" ");
		std::fs::write(dest.join("shell.nix"), render_shell_nix(&seeds, &provenance, &label))?;

		println!(
			"clinix: created registry env `{}` from {} seed(s): {label}",
			self.name,
			seeds.len()
		);
		println!("  enter: clinix env {}", self.name);
		Ok(())
	}
}

/// One materialized seed's provenance, recorded for drift detection.
struct Provenance {
	name: String,
	source: PathBuf,
	sha256: String,
}

/// The self-contained `shell.nix` for a materialized registry env. Reads
/// `./flake.lock`, imports the pinned nixpkgs, and unions the local seed copies.
/// A `clinixMeta` arg (default false) short-circuits to the `clinixEnv` record so
/// drift checks read it **without** forcing nixpkgs; `nix-shell` (meta off) gets
/// the composed shell.
fn render_shell_nix(seeds: &[(String, PathBuf)], provenance: &[Provenance], label: &str) -> String {
	let imports = seeds
		.iter()
		.map(|(n, _)| format!("      (import ./seeds/{n}.nix {{ inherit pkgs; }})"))
		.collect::<Vec<_>>()
		.join("\n");
	let prov = provenance
		.iter()
		.map(|p| {
			format!(
				"      {{ name = \"{}\"; source = \"{}\"; sha256 = \"{}\"; }}",
				p.name,
				p.source.display(),
				p.sha256
			)
		})
		.collect::<Vec<_>>()
		.join("\n");
	let sanitized = label.replace(' ', "-");
	SHELL_NIX_TEMPLATE
		.replace("@PROV@", &prov)
		.replace("@NAME@", &sanitized)
		.replace("@IMPORTS@", &imports)
		.replace("@LABEL@", label)
}

const SHELL_NIX_TEMPLATE: &str = r##"{ system ? builtins.currentSystem, clinixMeta ? false }:
let
  clinixEnv = {
    seeds = [
@PROV@
    ];
  };
in
if clinixMeta then clinixEnv
else
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

/// Lowercase hex `sha256` of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
	let digest = Sha256::digest(bytes);
	let mut s = String::with_capacity(64);
	for b in digest {
		use std::fmt::Write;
		let _ = write!(s, "{b:02x}");
	}
	s
}

/// Seed sources of a materialized env that have **drifted** — the current source
/// file's content no longer matches the recorded `sha256`, or is gone. Reads the
/// `clinixEnv.seeds` record via a cheap eval (`clinixMeta = true`, no nixpkgs).
/// Empty when the env is not seed-materialized or nothing drifted.
pub(crate) fn drifted_seeds(env_root: &Path) -> Result<Vec<String>> {
	let shell = env_root.join("shell.nix");
	// Gate on the materialized shape: a normal shell.nix has no `clinixMeta` arg
	// and would error if we passed one. This presence check keeps the drift eval
	// scoped to seed-materialized envs (a cheap read, not a semantic scrape).
	match std::fs::read_to_string(&shell) {
		Ok(src) if src.contains("clinixMeta") => {}
		_ => return Ok(Vec::new()),
	}
	let expr = format!(
		"(import {} {{ clinixMeta = true; }}).seeds or []",
		nix_path_lit(&shell)
	);
	let value = crate::nix::eval_json(&expr)?;
	let mut drifted = Vec::new();
	if let Some(items) = value.as_array() {
		for item in items {
			let source = item.get("source").and_then(|v| v.as_str()).unwrap_or("");
			let recorded = item.get("sha256").and_then(|v| v.as_str()).unwrap_or("");
			match std::fs::read(source) {
				Ok(bytes) if sha256_hex(&bytes) == recorded => {}
				Ok(_) => drifted.push(format!("{source} (changed since materialized)")),
				Err(_) => drifted.push(format!("{source} (source missing)")),
			}
		}
	}
	Ok(drifted)
}

/// A path as a nix path literal for an `import` expression (bare absolute path).
fn nix_path_lit(path: &Path) -> String {
	path.display().to_string()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn sha256_hex_is_stable_and_content_sensitive() {
		assert_eq!(sha256_hex(b""), // known SHA-256 of the empty input
			"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
		assert_ne!(sha256_hex(b"a"), sha256_hex(b"b"));
	}

	#[test]
	fn render_shell_nix_is_self_contained_and_records_provenance() {
		let seeds = vec![
			("rust".to_string(), PathBuf::from("/src/rust.nix")),
			("claude".to_string(), PathBuf::from("/src/claude.nix")),
		];
		let prov = vec![
			Provenance {
				name: "rust".into(),
				source: PathBuf::from("/src/rust.nix"),
				sha256: "aa".into(),
			},
			Provenance {
				name: "claude".into(),
				source: PathBuf::from("/src/claude.nix"),
				sha256: "bb".into(),
			},
		];
		let s = render_shell_nix(&seeds, &prov, "rust claude");
		// Self-contained: local lock + local seed copies, not the source paths.
		assert!(s.contains("builtins.readFile ./flake.lock"));
		assert!(s.contains("import ./seeds/rust.nix { inherit pkgs; }"));
		assert!(s.contains("import ./seeds/claude.nix { inherit pkgs; }"));
		// Provenance recorded (evaluable via clinixMeta).
		assert!(s.contains("clinixMeta"));
		assert!(s.contains("source = \"/src/rust.nix\"; sha256 = \"aa\";"));
		assert!(s.contains("name = \"rust-claude\";"));
	}
}
