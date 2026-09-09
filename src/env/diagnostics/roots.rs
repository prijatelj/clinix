//! `roots`: list GC-root versions — for one env (`roots <name>`) or, with no name, a
//! global grouped listing of every root family (the only way to discover project /
//! file / stack roots, which have no registry entry).

use clap::Args;

use crate::env::config::{Config, Settings};
use crate::env::seeds::Catalog;
use crate::env::{Context, node_root, registry};
use crate::error::Result;

use super::human_bytes;

/// `env roots [name] [--size] [--json]`.
#[derive(Args, Debug)]
pub struct Roots {
	/// Env whose versions to list; omit for a global grouped listing of every root.
	pub name: Option<String>,
	/// Also measure each version's retained closure on disk (slower — shells to nix).
	#[arg(long)]
	pub size: bool,
	/// Emit machine-readable JSON instead of a table.
	#[arg(long)]
	pub json: bool,
}

/// The on-disk size of a version's retained closure (the `.rt` output's requisites),
/// hardlink-aware, when `--size` is set. `None` if the `.rt` root is absent/unresolved.
fn version_bytes(drv_root: &std::path::Path) -> Option<u64> {
	let rt = registry::rt_root(drv_root);
	let target = std::fs::read_link(&rt).ok()?;
	let paths = crate::nix::requisites(&[target.to_string_lossy().into_owned()]).ok()?;
	Some(crate::disk::total_bytes(&paths))
}

pub fn roots(args: Roots, context: &Context) -> Result<()> {
	let cfg = &context.config;
	let settings = Settings::load(&cfg.config_dir)?;
	match &args.name {
		Some(name) => {
			let catalog = Catalog::build(&settings.env.seeds);
			for w in &catalog.warnings {
				eprintln!("clinix: {w}");
			}
			roots_of(cfg, &catalog, name, &settings, &args)
		}
		None => roots_global(cfg, &args),
	}
}

/// One version's report row (shared by text + JSON).
fn version_row(v: &registry::RootVersion, size: bool) -> serde_json::Value {
	let drv = std::fs::read_link(&v.drv_root)
		.map(|p| super::store_label(&p.to_string_lossy()))
		.unwrap_or_else(|_| "(unresolved)".to_string());
	let has_pkgs = registry::rt_root(&v.drv_root).symlink_metadata().is_ok();
	let mut obj = serde_json::json!({ "seq": v.seq, "drv": drv, "pkgs": has_pkgs });
	if size {
		if let Some(b) = version_bytes(&v.drv_root) {
			obj["bytes"] = serde_json::json!(b);
		}
	}
	obj
}

/// The version history of a single env (current + retained priors).
fn roots_of(cfg: &Config, catalog: &Catalog, name: &str, settings: &Settings, args: &Roots) -> Result<()> {
	let base = node_root(cfg, catalog, name)?;
	let versions = registry::list_versions(&base)?;
	if args.json {
		let rows: Vec<_> = versions.iter().map(|v| version_row(v, args.size)).collect();
		println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "env": name, "versions": rows }))?);
		return Ok(());
	}
	if versions.is_empty() {
		println!("clinix: `{name}` has no root versions (never entered)");
		return Ok(());
	}
	let key = base.file_name().map(|n| n.to_string_lossy().into_owned());
	let policy = key
		.as_deref()
		.and_then(|k| settings.env.root_retention(k))
		.map(|total| format!("keep {total} version(s)"))
		.unwrap_or_else(|| "not rooted (retention off)".to_string());
	println!("root versions for `{name}` ({policy}):");
	for (i, v) in versions.iter().rev().enumerate() {
		let label = if i == 0 { "current".to_string() } else { format!("prior {i}") };
		let drv = std::fs::read_link(&v.drv_root)
			.map(|p| super::store_label(&p.to_string_lossy()))
			.unwrap_or_else(|_| "(unresolved)".to_string());
		let pkgs = if registry::rt_root(&v.drv_root).symlink_metadata().is_ok() { "+pkgs" } else { "drv-only" };
		let size = if args.size {
			version_bytes(&v.drv_root).map(|b| format!(" {}", human_bytes(b))).unwrap_or_default()
		} else {
			String::new()
		};
		println!("  @{:<5} {:<9} {:<8}{size} {drv}", v.seq, label, pkgs);
	}
	Ok(())
}

/// A grouped listing of every root family under `roots/`, by kind.
fn roots_global(cfg: &Config, args: &Roots) -> Result<()> {
	let keys = registry::list_root_keys(cfg)?;
	if args.json {
		let mut families = Vec::new();
		for key in &keys {
			let versions = registry::list_versions(&registry::roots_dir(cfg).join(key))?;
			let rows: Vec<_> = versions.iter().map(|v| version_row(v, args.size)).collect();
			families.push(serde_json::json!({ "key": key, "versions": rows }));
		}
		println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "roots": families }))?);
		return Ok(());
	}
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
			let size = if args.size {
				let total: u64 = versions.iter().filter_map(|v| version_bytes(&v.drv_root)).sum();
				format!("  {}", human_bytes(total))
			} else {
				String::new()
			};
			println!("  {key}  ({n} version{}){size}  {cur}", if n == 1 { "" } else { "s" });
		}
	}
	println!("\nrelease with: clinix env clean <name> | --projects | --stacks | --match '<glob>'");
	Ok(())
}
