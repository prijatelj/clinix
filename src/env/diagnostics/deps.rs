//! `deps`: dependency/closure report for an env — its derivation, package store
//! paths, closure size, and GC roots.

use clap::Args;

use crate::env::{Context, RunCmd, resolve};
use crate::error::Result;

use super::{human_bytes, kind_str};

/// Where an env's dependencies live on disk: its derivation, each package's store
/// path, the full closure (count, and `--size` for on-disk bytes), and the GC
/// roots holding it. All classic (`nix-instantiate`, `nix-store -qR`, `du`).
#[derive(Args, Debug)]
pub struct Deps {
	/// Env to analyze (name, path, or `.`); defaults to the cwd project.
	pub name: Option<String>,
	/// Also measure the closure's on-disk size (slower — stats every path).
	#[arg(long)]
	pub size: bool,
	/// Emit machine-readable JSON instead of a table.
	#[arg(long)]
	pub json: bool,
}
impl RunCmd for Deps {
	fn run(self, context: &Context) -> Result<()> {
		let env = resolve(&context.config, self.name.as_deref())?;
		let shell_nix = env.require_shell_nix()?;
		let drv = crate::nix::instantiate(&shell_nix)?;

		if self.json {
			let packages = crate::nix::package_paths(&shell_nix)?;
			let closure = crate::nix::closure(&drv)?;
			let roots = crate::nix::gc_roots(&drv)?;
			let mut out = serde_json::json!({
				"env": env.root.to_string_lossy(),
				"kind": kind_str(env.kind),
				"derivation": drv,
				"packages": packages.iter().map(|p| serde_json::json!({ "name": p.name, "path": p.path })).collect::<Vec<_>>(),
				"closure_paths": closure.len(),
				"gc_roots": roots,
			});
			if self.size {
				out["closure_bytes"] = serde_json::json!(crate::disk::total_bytes(&closure));
			}
			println!("{}", serde_json::to_string_pretty(&out)?);
			return Ok(());
		}

		println!("env: {} ({})", env.root.display(), kind_str(env.kind));
		println!("derivation: {drv}");

		let packages = crate::nix::package_paths(&shell_nix)?;
		println!("\npackages ({}):", packages.len());
		for p in &packages {
			println!("  {}\n    {}", p.name, p.path);
		}

		let closure = crate::nix::closure(&drv)?;
		print!("\nclosure: {} store paths", closure.len());
		if self.size {
			println!(" — {}", human_bytes(crate::disk::total_bytes(&closure)));
		} else {
			println!("  (pass --size to measure on disk)");
		}

		let roots = crate::nix::gc_roots(&drv)?;
		println!("\ngc roots:");
		if roots.is_empty() {
			println!("  none — nix-collect-garbage will delete this env");
			println!(
				"  root it: clinix env shell {}",
				self.name.as_deref().unwrap_or(".")
			);
		} else {
			for r in &roots {
				println!("  {r}");
			}
		}
		Ok(())
	}
}
