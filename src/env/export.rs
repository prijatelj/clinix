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
			ExportTarget::Closure { out } => export_closure(&self.name, out, context),
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
	},
}

/// Serialize `name`'s runtime closure to a single archive. Exports **only an
/// already-built env** (settled decision — no implicit heavy build): if any
/// package output is not in the store, it errors and points at building first.
///
/// Only the `.closure` is written. `import` on the far side takes it plus the
/// env's `shell.nix` (which the user carries over themselves — it is right there
/// in the env dir), so export stays a single-responsibility primitive.
fn export_closure(name: &str, out: Option<PathBuf>, context: &Context) -> Result<()> {
	let env = resolve(&context.config, Some(name))?;
	let shell_nix = env.root.join("shell.nix");
	if !shell_nix.is_file() {
		return Err(ClinixError::Resolve(format!(
			"no shell.nix at {} (run: clinix env init)",
			env.root.display()
		)));
	}

	// The env's package output paths (computed by eval, not built).
	let pkgs = crate::nix::package_paths(&shell_nix)?;
	let paths: Vec<String> = pkgs.iter().map(|p| p.path.clone()).collect();

	// Export only what is already realized.
	let invalid = crate::nix::invalid_paths(&paths)?;
	if !invalid.is_empty() {
		return Err(ClinixError::Resolve(format!(
			"env `{name}` is not built — {} of {} package paths are not in the store.\n\
			 Build it first (e.g. `clinix run {name} -- true`), then re-export.",
			invalid.len(),
			paths.len()
		)));
	}

	// `--export` must be handed the *complete* closure (it never adds references).
	let closure = crate::nix::requisites(&paths)?;

	let out = out.unwrap_or_else(|| PathBuf::from(format!("{}.closure", env_label(&env))));
	if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
		fs::create_dir_all(parent)?;
	}
	crate::nix::export_paths(&closure, &out)?;

	println!("exported env `{name}` closure → {}", out.display());
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
