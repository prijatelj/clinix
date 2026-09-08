//! `deps`: dependency/closure report for an env — its derivation, package store
//! paths, closure size, and GC roots.

use clap::Args;

use crate::env::{Context, RunCmd, resolve};
use crate::error::{ClinixError, Result};

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
}
impl RunCmd for Deps {
	fn run(self, context: &Context) -> Result<()> {
		let env = resolve(&context.config, self.name.as_deref())?;
		let shell_nix = env.root.join("shell.nix");
		if !shell_nix.is_file() {
			return Err(ClinixError::Resolve(format!(
				"no shell.nix at {} (run: clinix env init)",
				env.root.display()
			)));
		}
		let drv = crate::nix::instantiate(&shell_nix)?;
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
