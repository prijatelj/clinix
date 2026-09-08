//! `list`: enumerate what `clinix env <name>` can enter — registry envs and seeds.

use crate::env::project::Project;
use crate::env::{Context, resolve};
use crate::error::Result;

use super::nixpkgs_pin;

/// List what `clinix env <name>` can enter: **registry** envs
/// (`state/envs/<name>`, each enriched with its nixpkgs pin from `flake.lock`) and
/// **seeds** (the in-place `*.nix` fragments discovered from `[env.seeds].sources`).
/// The filesystem *is* the registry index; the catalog is rebuilt from config —
/// nothing to keep in sync. Empty of both prints a hint.
pub fn list(context: &Context) -> Result<()> {
	let names = crate::env::registry::list_names(&context.config)?;
	let settings = crate::env::config::Settings::load(&context.config.config_dir)?;
	let catalog = crate::env::seeds::Catalog::build(&settings.env.seeds);
	for w in &catalog.warnings {
		eprintln!("clinix: {w}");
	}

	if names.is_empty() && catalog.seeds.is_empty() {
		println!(
			"clinix: no registered envs or seeds\n  \
			 create a registry env: clinix env new <name> --from …\n  \
			 or add seed sources: [env.seeds] in {}",
			context.config.config_dir.join("config.toml").display()
		);
		return Ok(());
	}

	if !names.is_empty() {
		println!("registry envs:");
		let width = names.iter().map(String::len).max().unwrap_or(4).max(4);
		println!("  {:<width$}  NIXPKGS", "NAME");
		for name in &names {
			let pin = registry_pin(context, name);
			println!("  {name:<width$}  {pin}");
		}
	}

	if !catalog.seeds.is_empty() {
		if !names.is_empty() {
			println!();
		}
		println!("seeds:");
		let nw = catalog
			.seeds
			.iter()
			.map(|s| s.name.len())
			.max()
			.unwrap_or(4)
			.max(4);
		let nsw = catalog
			.seeds
			.iter()
			.map(|s| s.namespace.as_deref().map_or(0, str::len))
			.max()
			.unwrap_or(9)
			.max(9);
		println!("  {:<nw$}  {:<nsw$}  SOURCE", "NAME", "NAMESPACE");
		for seed in &catalog.seeds {
			let namespace = seed.namespace.as_deref().unwrap_or("");
			println!(
				"  {:<nw$}  {namespace:<nsw$}  {}",
				seed.name,
				seed.path.display()
			);
		}
	}
	Ok(())
}

/// A registry env's nixpkgs pin as a short `ref @ rev9` string, or a reason it
/// could not be read (a malformed env should not abort the whole listing).
fn registry_pin(context: &Context, name: &str) -> String {
	let env = match resolve(&context.config, Some(name)) {
		Ok(env) => env,
		Err(_) => return "(unresolved)".to_string(),
	};
	match Project::load(env) {
		Ok(project) => match nixpkgs_pin(&project) {
			Some((track, rev)) => format!("{track} @ {:.9}", rev),
			None => "(no nixpkgs pin)".to_string(),
		},
		Err(_) => "(no flake.lock)".to_string(),
	}
}
