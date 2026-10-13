//! The `sys` scope: manage a NixOS system as a declarative `system.nix`
//! configuration (no nix-channels). Distinct from `env` because no
//! environment/project manager touches the OS level, and its posture is
//! declarative-config with the strongest rollback in the corpus (system
//! generations).
//!
//! `update`/`info` are implemented now because they are pure `flake.lock`
//! operations — the same re-lock engine `env` uses ([`crate::relock`]), pointed
//! at a `system.nix` config directory instead of an env. A `system.nix` config is
//! just a directory holding a `flake.lock` (read by `system.nix`'s inlined
//! reader) plus the NixOS modules; advancing the pin needs neither `shell.nix`
//! nor the experimental `nix flake` CLI. `init`/`switch`/`rollback` (which drive
//! `nixos-rebuild`) remain `NotYetImplemented` (plan phase 8).

use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::error::{ClinixError, Result, unimplemented};
use crate::model::lock::FlakeLock;

#[derive(Args, Debug)]
pub struct SysArgs {
	#[command(subcommand)]
	pub cmd: SysCmd,
}

#[derive(Subcommand, Debug)]
pub enum SysCmd {
	/// Initialize a `system.nix` configuration.
	Init,
	/// Build and switch to a new system generation.
	Switch,
	/// Roll back to the previous system generation.
	Rollback,
	/// Advance the pinned inputs in `flake.lock` to the latest commit of the
	/// branch each tracks — the latest stable nixpkgs while tracking a release
	/// branch (`nixos-XX.YY`). Frozen (rev-pinned) and non-github inputs are left
	/// as-is. Writes a byte-compatible lock only if something advanced.
	Update(SysUpdateArgs),
	/// Show the pinned inputs (`flake.lock`): node name, type, locked rev, and
	/// what each tracks.
	Info(SysInfoArgs),
}

/// `clinix sys update [dir] [--dry-run]`.
#[derive(Args, Debug)]
pub struct SysUpdateArgs {
	/// The system config directory holding `flake.lock` (default: cwd).
	pub dir: Option<PathBuf>,
	/// Resolve and report the pending advances without prefetching hashes or
	/// writing the lock (a preview).
	#[arg(long)]
	pub dry_run: bool,
}

/// `clinix sys info [dir]`.
#[derive(Args, Debug)]
pub struct SysInfoArgs {
	/// The system config directory holding `flake.lock` (default: cwd).
	pub dir: Option<PathBuf>,
}

pub fn dispatch(args: SysArgs) -> Result<()> {
	match args.cmd {
		SysCmd::Init => Err(unimplemented(
			"sys init",
			"plan phase 8: scaffold system.nix",
		)),
		SysCmd::Switch => Err(unimplemented("sys switch", "plan phase 8")),
		SysCmd::Rollback => Err(unimplemented("sys rollback", "plan phase 8")),
		SysCmd::Update(a) => update(a),
		SysCmd::Info(a) => info(a),
	}
}

/// Resolve the target `flake.lock` path from an optional dir (default: cwd) and
/// load it, failing with a clear message when the directory has no lock.
fn load_lock(dir: Option<PathBuf>) -> Result<(PathBuf, FlakeLock)> {
	let dir = match dir {
		Some(d) => d,
		None => std::env::current_dir()?,
	};
	let lock_path = dir.join("flake.lock");
	if !lock_path.exists() {
		return Err(ClinixError::Config(format!(
			"no flake.lock in {} — a system.nix config directory must contain the \
			 flake.lock its reader consumes",
			dir.display()
		)));
	}
	let lock = FlakeLock::from_json(&std::fs::read_to_string(&lock_path)?)?;
	Ok((lock_path, lock))
}

/// `sys update` — re-lock the system config's tracked inputs via the shared
/// engine ([`crate::relock::update_tracked`]), writing the lock only when
/// something advanced (and never under `--dry-run`).
fn update(args: SysUpdateArgs) -> Result<()> {
	let (lock_path, mut lock) = load_lock(args.dir)?;
	let changed = crate::relock::update_tracked(&mut lock, args.dry_run)?;
	if !args.dry_run && changed > 0 {
		std::fs::write(&lock_path, lock.to_json())?;
	}
	Ok(())
}

/// `sys info` — print the system config's pinned inputs (read-only).
fn info(args: SysInfoArgs) -> Result<()> {
	let (_lock_path, lock) = load_lock(args.dir)?;
	crate::relock::show_pins(&lock);
	Ok(())
}
