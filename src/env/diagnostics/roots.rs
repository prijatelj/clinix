//! `roots`: list GC-root versions — for one env (`roots <name>`) or, with no name, a
//! global grouped listing of every root family (the only way to discover project /
//! file / stack roots, which have no registry entry).

use crate::env::config::{Config, Settings};
use crate::env::seeds::Catalog;
use crate::env::{Context, OptionalTarget, node_root, registry};
use crate::error::Result;

/// `env roots [name]`. With a name: the version history of that env. Without: a
/// grouped listing of all root keys (registry / project / file / stack) with version
/// counts. Offline — `readdir` + `readlink`, no nix call.
pub fn roots(target: OptionalTarget, context: &Context) -> Result<()> {
	let cfg = &context.config;
	let settings = Settings::load(&cfg.config_dir)?;
	match target.name {
		Some(name) => {
			let catalog = Catalog::build(&settings.env.seeds);
			for w in &catalog.warnings {
				eprintln!("clinix: {w}");
			}
			roots_of(cfg, &catalog, &name, &settings)
		}
		None => roots_global(cfg),
	}
}

/// The version history of a single env (current + retained priors).
fn roots_of(cfg: &Config, catalog: &Catalog, name: &str, settings: &Settings) -> Result<()> {
	let base = node_root(cfg, catalog, name)?;
	let versions = registry::list_versions(&base)?;
	if versions.is_empty() {
		println!("clinix: `{name}` has no root versions (never entered)");
		return Ok(());
	}
	let key = base.file_name().map(|n| n.to_string_lossy().into_owned());
	let policy = key
		.as_deref()
		.and_then(|k| settings.env.root_retention(k))
		.map(|total| format!("keep {} version(s)", total))
		.unwrap_or_else(|| "not rooted (retention off)".to_string());
	println!("root versions for `{name}` ({policy}):");
	// Newest first: index 0 is current, index N is `--prior N`.
	for (i, v) in versions.iter().rev().enumerate() {
		let label = if i == 0 {
			"current".to_string()
		} else {
			format!("prior {i}")
		};
		let drv = std::fs::read_link(&v.drv_root)
			.map(|p| super::store_label(&p.to_string_lossy()))
			.unwrap_or_else(|_| "(unresolved)".to_string());
		let pkgs = if registry::rt_root(&v.drv_root).symlink_metadata().is_ok() {
			"+pkgs"
		} else {
			"drv-only"
		};
		println!("  @{:<5} {:<9} {:<8} {drv}", v.seq, label, pkgs);
	}
	Ok(())
}

/// A grouped listing of every root family under `roots/`, by kind. This is how
/// project / file / stack roots (which have no registry listing) are discovered.
fn roots_global(cfg: &Config) -> Result<()> {
	let keys = registry::list_root_keys(cfg)?;
	if keys.is_empty() {
		println!("clinix: no GC roots recorded (nothing entered yet)");
		return Ok(());
	}
	for (title, prefix) in [
		("registry envs (env-*)", "env-"),
		("projects (proj-*)", "proj-"),
		("files (file-*)", "file-"),
		("stacks / unions (stack-*)", "stack-"),
	] {
		let mut printed_header = false;
		for key in keys.iter().filter(|k| k.starts_with(prefix)) {
			if !printed_header {
				println!("{title}:");
				printed_header = true;
			}
			let base = registry::roots_dir(cfg).join(key);
			let versions = registry::list_versions(&base)?;
			let cur = versions
				.last()
				.and_then(|v| std::fs::read_link(&v.drv_root).ok())
				.map(|p| super::store_label(&p.to_string_lossy()))
				.unwrap_or_default();
			let n = versions.len();
			println!(
				"  {key}  ({n} version{})  {cur}",
				if n == 1 { "" } else { "s" }
			);
		}
	}
	println!("\nrelease with: clinix env clean <name> | --projects | --stacks | --match '<glob>'");
	Ok(())
}
