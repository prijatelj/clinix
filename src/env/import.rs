//! `import`: register a clinix env from an exported closure + its `shell.nix`.
//!
//! The clinix-specific value here is **registering a `state/envs/<name>` registry
//! env** from the env definition, so the target machine can `clinix shell <name>`.
//! Populating the store from the `.closure` is *best-effort convenience*: it is
//! plain `nix-store --import`, which the user can (and on a multi-user store, from
//! an untrusted account, must) run themselves with vanilla nix under sudo. So a
//! store-import that nix refuses does **not** fail the command — the env is still
//! registered and clinix tells the user how to finish the store load.

use std::fs;
use std::path::PathBuf;

use clap::Args;

use crate::env::registry;
use crate::env::{Context, RunCmd};
use crate::error::{ClinixError, Result};

#[derive(Args, Debug)]
pub struct Import {
	/// Name to register the imported env under (in the registry).
	pub name: String,
	/// The `.closure` archive produced by `clinix env export … closure`.
	pub source: PathBuf,
	/// The env's `shell.nix` (carried over alongside the closure).
	#[arg(long)]
	pub shell_nix: PathBuf,
	/// The env's `flake.lock` (needed for a two-file env to resolve its pin).
	#[arg(long)]
	pub flake_lock: Option<PathBuf>,
}
impl RunCmd for Import {
	fn run(self, context: &Context) -> Result<()> {
		// 1. All pre-checks first — no nix, no writes, so failures are clean.
		registry::validate_name(&self.name)?;
		let dest = registry::env_root(&context.config, &self.name);
		if dest.exists() {
			return Err(ClinixError::EnvExists(self.name.clone()));
		}
		if !self.source.is_file() {
			return Err(ClinixError::Resolve(format!(
				"closure archive not found: {}",
				self.source.display()
			)));
		}
		if !self.shell_nix.is_file() {
			return Err(ClinixError::Resolve(format!(
				"shell.nix not found: {}",
				self.shell_nix.display()
			)));
		}
		if let Some(lock) = &self.flake_lock
			&& !lock.is_file()
		{
			return Err(ClinixError::Resolve(format!(
				"flake.lock not found: {}",
				lock.display()
			)));
		}

		// 2. Best-effort store population. A trust/signature refusal (multi-user
		//    store, untrusted user) is *not* fatal — the user finishes with vanilla
		//    nix under sudo; any other failure (e.g. a corrupt archive) aborts
		//    before we register a broken env.
		match crate::nix::import_closure(&self.source) {
			Ok(()) => println!("imported closure into the store"),
			Err(e) => {
				let msg = e.to_string();
				if msg.contains("signature") || msg.contains("trusted") {
					eprintln!(
						"clinix: warning: nix refused the closure import \
						 (multi-user store, untrusted user)."
					);
					eprintln!(
						"  the env is still registered; finish the store load with vanilla nix:"
					);
					eprintln!("    sudo nix-store --import < {}", self.source.display());
				} else {
					return Err(e);
				}
			}
		}

		// 3. Register the env (the guaranteed, clinix-specific action).
		fs::create_dir_all(&dest)?;
		fs::copy(&self.shell_nix, dest.join("shell.nix"))?;
		if let Some(lock) = &self.flake_lock {
			fs::copy(lock, dest.join("flake.lock"))?;
		}

		println!(
			"registered env `{}` — enter it: clinix shell {}",
			self.name, self.name
		);
		Ok(())
	}
}
