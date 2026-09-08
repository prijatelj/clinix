//! `export docker` (ADR-6 extension): a **general** clinix env → OCI image. By
//! default emits a committable `docker-base.nix` (a `dockerTools.streamLayeredImage`
//! whose `contents` are the composed env's own packages); `--build`/`--load`
//! realize it; `--from` layers on a **digest-pinned** external base via a
//! Dockerfile. No uv/Python specialization — the env's shell already provides its
//! tools. See `notes/clinix/design/export.md` §3.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::env::{Context, compose_nodes};
use crate::error::{ClinixError, Result};

/// `export docker` options (from the CLI variant).
#[derive(Debug, Default)]
pub struct Opts {
	/// Output dir for the artifacts (default `./containers`).
	pub out: Option<PathBuf>,
	/// Image name (default: the composed env's label, slugged).
	pub name: Option<String>,
	/// Host paths to bake into the image (repeatable).
	pub include: Vec<PathBuf>,
	/// External base image to layer on (a digest-pinned `Dockerfile` path).
	pub from: Option<String>,
	/// Resolve a `--from` digest via `docker pull` + inspect (heavy fallback).
	pub pull: bool,
	/// Build the image tarball (`nix-build` the streamer).
	pub build: bool,
	/// Build **and** `docker load` the image.
	pub load: bool,
	/// Resolver-only mode: print `<img>@sha256:…` for this image and exit.
	pub latest_version: Option<String>,
}

/// Run `export docker`. In `--latest-version` mode it just resolves + prints;
/// otherwise it composes `names`, writes the artifacts, and optionally builds.
pub fn run(names: &[String], opts: Opts, ctx: &Context) -> Result<()> {
	// Resolver-only mode — no env needed.
	if let Some(img) = &opts.latest_version {
		let pinned = crate::ext::oci::pin_reference(img, opts.pull)?;
		println!("{pinned}");
		return Ok(());
	}
	if names.is_empty() {
		return Err(ClinixError::Config(
			"give one or more env names to image, or `--latest-version <img>` to just resolve a digest"
				.to_string(),
		));
	}

	let comp = compose_nodes(ctx, names)?;
	let image = opts
		.name
		.clone()
		.unwrap_or_else(|| crate::env::naming::slug(&comp.label, true, true, Some("env")));
	let outdir = opts
		.out
		.clone()
		.unwrap_or_else(|| PathBuf::from("containers"));
	std::fs::create_dir_all(&outdir)?;

	match &opts.from {
		// Nix base: needs the env's pin for `dockerTools` + its packages.
		None => {
			let lock = comp.lock.as_ref().ok_or_else(|| {
				ClinixError::Config(format!(
					"`{}` has no discoverable flake.lock — docker export needs a pinned env",
					names.join(" ")
				))
			})?;
			nix_base(&comp.shell_file, lock, &image, &opts, &outdir)
		}
		// External base: pins the base + `--include` files (env packages not injected yet).
		Some(base) => from_base(base, &image, &opts, &outdir),
	}
}

/// The nix-base path: write `docker-base.nix` (a complete `streamLayeredImage`),
/// then `--build`/`--load` it.
fn nix_base(shell_file: &Path, lock: &Path, image: &str, opts: &Opts, outdir: &Path) -> Result<()> {
	let base_nix = outdir.join("docker-base.nix");
	std::fs::write(
		&base_nix,
		render_base_nix(lock, shell_file, image, &opts.include),
	)?;
	println!("wrote {} (image: {image}:latest)", base_nix.display());
	println!(
		"  build:  nix-build {} --no-out-link | docker load",
		base_nix.display()
	);

	if opts.build || opts.load {
		let streamer = nix_build(&base_nix)?;
		if opts.load {
			load_stream(&streamer)?;
			println!("loaded image `{image}:latest` via docker");
		} else {
			let tar = outdir.join(format!("{image}.tar"));
			stream_to(&streamer, &tar)?;
			println!("wrote image tarball → {}", tar.display());
		}
	}
	Ok(())
}

/// The `--from` path: resolve the base image to a digest and write a `Dockerfile`
/// `FROM <base>@sha256:…` + the `--include` files. Injecting the env's nix
/// packages onto an external base is the deferred nix-`fromImage` path (§3b), so
/// this scaffolds a reproducible base; note that clearly.
fn from_base(base: &str, image: &str, opts: &Opts, outdir: &Path) -> Result<()> {
	let pinned = crate::ext::oci::pin_reference(base, opts.pull)?;
	let dockerfile = outdir.join("Dockerfile");
	std::fs::write(&dockerfile, render_dockerfile(&pinned, &opts.include))?;
	println!("wrote {} (FROM {pinned})", dockerfile.display());
	println!(
		"  note: the env's Nix packages are not injected on an external base yet \
		 (nix `fromImage` is future) — this pins the base + your --include files."
	);
	if opts.build || opts.load {
		if !tool_present("docker") {
			return Err(ClinixError::Config(
				"`--build`/`--load` with `--from` needs docker".to_string(),
			));
		}
		let status = Command::new("docker")
			.args(["build", "-f"])
			.arg(&dockerfile)
			.args(["-t", &format!("{image}:latest"), "."])
			.status()?;
		if !status.success() {
			return Err(ClinixError::Config("docker build failed".to_string()));
		}
		println!("built image `{image}:latest`");
	}
	Ok(())
}

/// `nix-build <file> --no-out-link` → the streamer script's store path.
fn nix_build(file: &Path) -> Result<String> {
	let out = Command::new("nix-build")
		.arg(file)
		.arg("--no-out-link")
		.output()?;
	if !out.status.success() {
		return Err(ClinixError::Config(format!(
			"nix-build failed:\n{}",
			String::from_utf8_lossy(&out.stderr).trim()
		)));
	}
	Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run the streamer, piping its tar stream into `docker load`.
fn load_stream(streamer: &str) -> Result<()> {
	if !tool_present("docker") {
		return Err(ClinixError::Config("`--load` needs docker".to_string()));
	}
	let status = Command::new("sh")
		.arg("-c")
		.arg(format!("{streamer} | docker load"))
		.status()?;
	if !status.success() {
		return Err(ClinixError::Config("docker load failed".to_string()));
	}
	Ok(())
}

/// Run the streamer, writing its tar stream to `out`.
fn stream_to(streamer: &str, out: &Path) -> Result<()> {
	let status = Command::new("sh")
		.arg("-c")
		.arg(format!("{streamer} > {}", shell_quote(out)))
		.status()?;
	if !status.success() {
		return Err(ClinixError::Config(
			"streaming the image tarball failed".to_string(),
		));
	}
	Ok(())
}

/// Render `docker-base.nix`: read the env's pin for `dockerTools`, import the env
/// shell for its packages, and build a reproducible layered image. `--include`
/// files are copied in via `extraCommands` (nix has no "files in a shell", so an
/// image-only file is store-ified here — see §3).
fn render_base_nix(lock: &Path, shell_file: &Path, name: &str, include: &[PathBuf]) -> String {
	let includes: String = include
		.iter()
		.map(|p| {
			let base = p
				.file_name()
				.map(|s| s.to_string_lossy().into_owned())
				.unwrap_or_default();
			// A bare path literal interpolates to a store path (copies the file in).
			format!("    cp -a ${{{}}} ./{base}\n", p.display())
		})
		.collect();
	use crate::env::nix_expr;
	// Head comment + shared lock-prelude + the dockerTools tail. The prelude reads
	// the env's pin (`fetch`/`sources`/`pkgs`); the tail adds `shell`/`envPackages`.
	let prelude = nix_expr::lock_prelude(&nix_expr::nix_str(lock), "");
	format!("{BASE_NIX_HEAD}{prelude}{BASE_NIX_TAIL}")
		.replace("@SHELL@", &nix_expr::nix_str(shell_file))
		.replace("@NAME@", name)
		.replace("@INCLUDES@", &includes)
}

/// Render a `--from` `Dockerfile`: a digest-pinned base + `COPY` of the includes.
fn render_dockerfile(pinned_base: &str, include: &[PathBuf]) -> String {
	let mut df = format!(
		"# Generated by clinix — commit as a recorded artifact.\n\
		 # Base pinned to a digest for reproducibility (never a bare :tag).\n\
		 FROM {pinned_base}\nWORKDIR /workspace\n"
	);
	for p in include {
		let base = p
			.file_name()
			.map(|s| s.to_string_lossy().into_owned())
			.unwrap_or_default();
		df.push_str(&format!("COPY {base} ./{base}\n"));
	}
	df
}

/// The head comment of `docker-base.nix` (before the shared lock-prelude).
const BASE_NIX_HEAD: &str = r##"# docker-base.nix — a reproducible OCI image of the composed clinix env.
# Generated by clinix; commit it as a recorded artifact.
#
#   nix-build docker-base.nix --no-out-link | docker load   # -> @NAME@:latest
"##;

/// The `dockerTools` tail of `docker-base.nix` (after the shared lock-prelude): it
/// adds the `shell`/`envPackages` bindings and the `streamLayeredImage` body.
const BASE_NIX_TAIL: &str = r##"  shell = import @SHELL@ { };
  envPackages = (shell.nativeBuildInputs or [ ]) ++ (shell.buildInputs or [ ]);
in
pkgs.dockerTools.streamLayeredImage {
  name = "@NAME@";
  tag = "latest";
  # `created` defaults to the epoch -> reproducible. Never "now".
  contents = envPackages ++ (with pkgs; [ bashInteractive coreutils ]);
  config = {
    Cmd = [ "${pkgs.bashInteractive}/bin/bash" ];
    WorkingDir = "/workspace";
  };
  maxLayers = 100;
  extraCommands = ''
@INCLUDES@  '';
}
"##;

/// A minimal single-quote shell-quote for a path used in a `sh -c` redirect.
fn shell_quote(path: &Path) -> String {
	format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn tool_present(name: &str) -> bool {
	Command::new(name)
		.arg("--version")
		.output()
		.map(|o| o.status.success())
		.unwrap_or(false)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn render_base_nix_wires_lock_shell_and_includes() {
		let s = render_base_nix(
			Path::new("/e/flake.lock"),
			Path::new("/e/shell.nix"),
			"myimg",
			&[PathBuf::from("/data/model.bin")],
		);
		assert!(s.contains("builtins.readFile \"/e/flake.lock\""));
		assert!(s.contains("import \"/e/shell.nix\" { }"));
		assert!(s.contains("name = \"myimg\";"));
		assert!(s.contains("streamLayeredImage"));
		// The env's own packages become the image contents.
		assert!(s.contains("envPackages = (shell.nativeBuildInputs"));
		// --include copies the host file in via a store-path interpolation.
		assert!(s.contains("cp -a ${/data/model.bin} ./model.bin"));
	}

	#[test]
	fn render_base_nix_without_includes_has_empty_extra_commands() {
		let s = render_base_nix(
			Path::new("/e/flake.lock"),
			Path::new("/e/shell.nix"),
			"x",
			&[],
		);
		assert!(
			s.contains("extraCommands = ''\n  '';"),
			"empty include block"
		);
	}

	#[test]
	fn render_dockerfile_pins_base_and_copies_includes() {
		let df = render_dockerfile(
			"nvidia/cuda:12.4.1@sha256:abc",
			&[PathBuf::from("/data/model.bin")],
		);
		assert!(df.contains("FROM nvidia/cuda:12.4.1@sha256:abc"));
		assert!(df.contains("COPY model.bin ./model.bin"));
	}
}
