//! `export`: emit an env to another format. **Closure** (this file) is core nix
//! interop — serialize an env's already-built store closure into **one archive**.
//! Bundling anything else (the `shell.nix`/`flake.lock`, which already sit in the
//! env dir) is deliberately left to the user: export does one thing, produce the
//! `.closure`. **Docker** stays the ADR-6 extension (phase 7).

use std::fs;
use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::env::{Context, Env, RunCmd, resolve};
use crate::error::{ClinixError, Result, unimplemented};

#[derive(Args, Debug)]
pub struct Export {
	/// Env to export (registry name, path, or `.`).
	pub name: String,
	#[command(subcommand)]
	pub target: ExportTarget,
}
impl RunCmd for Export {
	fn run(self, context: &Context) -> Result<()> {
		match self.target {
			ExportTarget::Closure { out, packages } => {
				export_closure(&self.name, out, packages, context)
			}
			ExportTarget::Docker { .. } => Err(unimplemented(
				"env export docker",
				"plan phase 7: docker extension (ADR-6)",
			)),
		}
	}
}

#[derive(Subcommand, Debug)]
pub enum ExportTarget {
	/// Emit a reproducible OCI image (or a Dockerfile). [ADR-6 extension, phase 7]
	Docker {
		/// Output path (defaults to a Dockerfile in the cwd).
		out: Option<PathBuf>,
	},
	/// Serialize the env's **already-built** Nix closure into one archive file.
	Closure {
		/// Output archive path (default `./<label>.closure`).
		out: Option<PathBuf>,
		/// Seed from the package outputs only (the *delta*), not the full env
		/// runtime closure. Smaller, but only enters a shell on a target that
		/// already provides the base (bash/stdenv) by hash.
		#[arg(long)]
		packages: bool,
	},
}

/// Serialize `name`'s closure to a single archive. Exports **only an already-built
/// env** (settled — no implicit heavy build): if any needed path is not in the
/// store it **hard-errors** and points at building first. There is no useful
/// "export an unbuilt env" — `nix-store --export` can only serialize valid paths.
///
/// Two seed modes (settled 2026-09-05; see plan "Closure export / import"):
/// - **default = the complete env runtime closure** — the shell derivation's
///   *realized inputs* (bash/stdenv/packages), so the closure can *enter* the
///   shell on any target. mkShell's own output does not carry its `buildInputs`,
///   so the seed is the realized OUTPUT paths of the drv's include-outputs closure.
/// - **`--packages` = the delta** — the package outputs' closure only. Smaller;
///   assumes the target already provides the base (bash/stdenv) by hash.
///
/// Only the `.closure` is written; the nixpkgs source is never folded in (it is an
/// eval-time input, not runtime — the user carries it, or enters via the `.drv`).
/// `import` on the far side takes this plus the env's `shell.nix`. Single primitive.
fn export_closure(
	name: &str,
	out: Option<PathBuf>,
	packages_only: bool,
	context: &Context,
) -> Result<()> {
	let env = resolve(&context.config, Some(name))?;
	let shell_nix = env.root.join("shell.nix");
	if !shell_nix.is_file() {
		return Err(ClinixError::Resolve(format!(
			"no shell.nix at {} (run: clinix env init)",
			env.root.display()
		)));
	}

	// Package count is reported in both modes.
	let pkgs = crate::nix::package_paths(&shell_nix)?;

	// Seed the closure. `--export` is handed a *complete* closure (it never adds
	// references); export refuses an unbuilt env (hard error → build first).
	let (closure, mode) = if packages_only {
		let paths: Vec<String> = pkgs.iter().map(|p| p.path.clone()).collect();
		ensure_built(name, &paths)?;
		(crate::nix::requisites(&paths)?, "packages")
	} else {
		// The env's realized inputs: the include-outputs closure of the shell drv,
		// minus the `.drv` recipes = the runtime output paths (already closed under
		// references, since they are requisites of the drv).
		let drv = crate::nix::instantiate(&shell_nix)?;
		let outputs: Vec<String> = crate::nix::closure(&drv)?
			.into_iter()
			.filter(|p| !p.ends_with(".drv"))
			.collect();
		ensure_built(name, &outputs)?;
		(outputs, "complete env")
	};

	let out = out.unwrap_or_else(|| PathBuf::from(format!("{}.closure", env_label(&env))));
	if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
		fs::create_dir_all(parent)?;
	}
	crate::nix::export_paths(&closure, &out)?;

	println!("exported env `{name}` closure ({mode}) → {}", out.display());
	println!("  {} store paths ({} packages)", closure.len(), pkgs.len());
	println!(
		"  load on the target with its shell.nix:\n\
		 \tclinix env import <name> {} --shell-nix {} [--flake-lock {}]",
		out.display(),
		env.root.join("shell.nix").display(),
		env.root.join("flake.lock").display()
	);
	Ok(())
}

/// Refuse to export an env whose needed paths are not realized — there is no
/// useful "export an unbuilt env" (nix can only serialize valid store paths).
fn ensure_built(name: &str, paths: &[String]) -> Result<()> {
	let invalid = crate::nix::invalid_paths(paths)?;
	if !invalid.is_empty() {
		return Err(ClinixError::Resolve(format!(
			"env `{name}` is not built — {} of {} store paths are not in the store.\n\
			 Build it first (e.g. `clinix run {name} -- true`), then re-export.",
			invalid.len(),
			paths.len()
		)));
	}
	Ok(())
}

/// A filesystem-safe label for a resolved env: the registry name when there is
/// one, else the resolved directory's basename (for a project/cwd env).
fn env_label(env: &Env) -> String {
	let raw = env
		.name
		.as_deref()
		.filter(|n| !n.contains('/') && *n != ".")
		.map(str::to_string)
		.or_else(|| {
			env.root
				.file_name()
				.map(|s| s.to_string_lossy().into_owned())
		})
		.unwrap_or_else(|| "env".to_string());
	let slug: String = raw
		.chars()
		.map(|c| {
			if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
				c
			} else {
				'_'
			}
		})
		.collect();
	let trimmed = slug.trim_matches('_');
	if trimmed.is_empty() {
		"env".to_string()
	} else {
		trimmed.to_string()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn env_label_slugs_registry_and_project_names() {
		let reg = Env {
			name: Some("python".into()),
			root: PathBuf::from("/state/envs/python"),
			kind: crate::env::Kind::Registry,
		};
		assert_eq!(env_label(&reg), "python");

		// A cwd/project env (name None) uses the dir basename.
		let proj = Env {
			name: None,
			root: PathBuf::from("/home/u/my.proj"),
			kind: crate::env::Kind::Project,
		};
		assert_eq!(env_label(&proj), "my_proj");
	}
}
