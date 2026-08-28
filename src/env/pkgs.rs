//! Packages: the [`Pkg`] spec (`name[=version]`) plus the package-mutating verbs
//! — `add`/`remove` (`shell.nix` splice), `pin`/`unpin` (`flake.lock`), and
//! `update`.

use clap::Args;

use crate::env::{Context, RunCmd};
use crate::error::{ClinixError, Result, unimplemented};

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
	fn run(self, _context: &Context) -> Result<()> {
		Err(unimplemented("env update", "plan phase 3: flake.lock update"))
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
