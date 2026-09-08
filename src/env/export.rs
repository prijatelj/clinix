//! `export`: emit an env — or the **union** of several — to another format.
//! Grammar (2026-09-07): `export <target> [opts] <names…>`, target first, names
//! variadic, so `export closure rust python` exports the composed closure of both.
//!
//! **Closure** (this file) is core nix interop — serialize an env's already-built
//! store closure into **one archive**. **Docker** is the ADR-6 extension
//! (`ext::export_docker`). Both share the union composition ([`compose_nodes`]).

use std::fs;
use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::env::{Context, RunCmd, compose_nodes};
use crate::error::{ClinixError, Result};

#[derive(Args, Debug)]
pub struct Export {
	#[command(subcommand)]
	pub target: ExportTarget,
}
impl RunCmd for Export {
	fn run(self, context: &Context) -> Result<()> {
		match self.target {
			ExportTarget::Closure {
				names,
				out,
				packages,
			} => export_closure(&names, out, packages, context),
			ExportTarget::Docker {
				names,
				out,
				name,
				include,
				from,
				pull,
				build,
				load,
				latest_version,
			} => crate::ext::export_docker::run(
				&names,
				crate::ext::export_docker::Opts {
					out,
					name,
					include,
					from,
					pull,
					build,
					load,
					latest_version,
				},
				context,
			),
		}
	}
}

#[derive(Subcommand, Debug)]
pub enum ExportTarget {
	/// Serialize the env's (or union's) **already-built** Nix closure into one
	/// archive file.
	Closure {
		/// Env name(s) to export; several compose into a union.
		#[arg(required = true)]
		names: Vec<String>,
		/// Output archive path (default `./<label>.closure`).
		#[arg(long)]
		out: Option<PathBuf>,
		/// Seed from the package outputs only (the *delta*), not the full env
		/// runtime closure. Smaller, but only enters a shell on a target that
		/// already provides the base (bash/stdenv) by hash.
		#[arg(long)]
		packages: bool,
	},
	/// Emit a reproducible OCI image (or a Dockerfile) of the env (or union).
	/// [ADR-6 extension — `ext::export_docker`]
	Docker {
		/// Env name(s) to image; several compose into a union. (Omit with
		/// `--latest-version`.)
		names: Vec<String>,
		/// Output dir for the generated artifacts (default `./containers`).
		#[arg(long)]
		out: Option<PathBuf>,
		/// Image name (default: the composed env's label).
		#[arg(long)]
		name: Option<String>,
		/// Host path to bake into the image (repeatable).
		#[arg(long = "include")]
		include: Vec<PathBuf>,
		/// External base image to layer on — emitted as a digest-pinned Dockerfile.
		#[arg(long)]
		from: Option<String>,
		/// Resolve a `--from` digest via `docker pull` + inspect (heavy fallback).
		#[arg(long)]
		pull: bool,
		/// Build the image tarball (`nix-build` the streamer).
		#[arg(long)]
		build: bool,
		/// Build **and** `docker load` the image.
		#[arg(long)]
		load: bool,
		/// Resolver-only: print `<img>@sha256:…` for this image and exit (no env).
		#[arg(long = "latest-version")]
		latest_version: Option<String>,
	},
}

/// Serialize the closure of `names` (composed into a union) to a single archive.
/// Exports **only an already-built** env (settled — no implicit heavy build): if
/// any needed path is not in the store it **hard-errors** and points at building
/// first. `nix-store --export` can only serialize valid paths.
///
/// Two seed modes (settled 2026-09-05; see plan "Closure export / import"):
/// - **default = the complete env runtime closure** — the shell derivation's
///   *realized inputs* (bash/stdenv/packages), so the closure can *enter* the
///   shell on any target.
/// - **`--packages` = the delta** — the package outputs' closure only. Smaller;
///   assumes the target already provides the base (bash/stdenv) by hash.
fn export_closure(
	names: &[String],
	out: Option<PathBuf>,
	packages_only: bool,
	context: &Context,
) -> Result<()> {
	// Resolve/compose the names to one instantiable shell (union = compose path).
	let comp = compose_nodes(context, names)?;
	let shell_nix = &comp.shell_file;

	// Package count is reported in both modes.
	let pkgs = crate::nix::package_paths(shell_nix)?;
	let what = names.join(" ");

	// Seed the closure. `--export` is handed a *complete* closure (it never adds
	// references); export refuses an unbuilt env (hard error → build first).
	let (closure, mode) = if packages_only {
		let paths: Vec<String> = pkgs.iter().map(|p| p.path.clone()).collect();
		ensure_built(&what, &paths)?;
		(crate::nix::requisites(&paths)?, "packages")
	} else {
		// The env's realized inputs: the include-outputs closure of the shell drv,
		// minus the `.drv` recipes = the runtime output paths.
		let drv = crate::nix::instantiate(shell_nix)?;
		let outputs: Vec<String> = crate::nix::closure(&drv)?
			.into_iter()
			.filter(|p| !p.ends_with(".drv"))
			.collect();
		ensure_built(&what, &outputs)?;
		(outputs, "complete env")
	};

	let out = out.unwrap_or_else(|| {
		PathBuf::from(format!(
			"{}.closure",
			crate::env::naming::slug(&comp.label, false, false, Some("env"))
		))
	});
	if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
		fs::create_dir_all(parent)?;
	}
	crate::nix::export_paths(&closure, &out)?;

	println!("exported `{what}` closure ({mode}) → {}", out.display());
	println!("  {} store paths ({} packages)", closure.len(), pkgs.len());
	println!(
		"  load on the target with its shell.nix:\n\
		 \tclinix env import <name> {} --shell-nix {}",
		out.display(),
		shell_nix.display()
	);
	Ok(())
}

/// Refuse to export an env whose needed paths are not realized — there is no
/// useful "export an unbuilt env" (nix can only serialize valid store paths).
fn ensure_built(what: &str, paths: &[String]) -> Result<()> {
	let invalid = crate::nix::invalid_paths(paths)?;
	if !invalid.is_empty() {
		return Err(ClinixError::Resolve(format!(
			"`{what}` is not built — {} of {} store paths are not in the store.\n\
			 Build it first (e.g. `clinix run {what} -- true`), then re-export.",
			invalid.len(),
			paths.len()
		)));
	}
	Ok(())
}
