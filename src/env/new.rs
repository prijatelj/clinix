//! `new`: create a **registry** env. Two modes, chosen by `--from`:
//!
//! - **compose** (`--from a b c`, seed names): materialize a stack of seed
//!   fragments into a portable, frozen env (copies + shared lock + self-contained
//!   `shell.nix`). The `~/dev_env` composition, frozen.
//! - **register** (`--from <dir-or-file>`, a single shell): register an existing
//!   project/shell under a name or `namespace:member`. **non-copy** by default (a
//!   thin wrapper importing the source's absolute path), `--copy` to freeze. The
//!   wrapper reads the **namespace pin** (`./flake.lock`, or `../flake.lock` for a
//!   member) and injects `pkgs` into the source, so one lock governs a namespace.
//!
//! Lock coherence (register mode): the source `shell.nix` is parsed for a
//! `readFile …/flake.lock`. None → warn (unpinned, entered as-is). Present and the
//! namespace has no pin → establish it. Matches → reuse. Differs → error unless
//! `--overwrite` (repin the namespace) or `--use-prior` (conform to it). See
//! `notes/clinix/design/target-resolution.md` §3–4.

use std::path::{Path, PathBuf};

use clap::Args;
use rnix::SyntaxKind;

use crate::env::config::Settings;
use crate::env::seeds::{Catalog, Resolved};
use crate::env::{Context, RunCmd, registry};
use crate::error::{ClinixError, Result};

/// Create a new **registry** env. Distinct from [`super::init::Init`] (which
/// scaffolds a *project* in a directory): `new` writes a reusable, named env into
/// the registry. `--from` is either seed names (compose) or a single directory /
/// `*.nix` file (register). The name may be a bare name or `namespace:member`.
#[derive(Args, Debug)]
pub struct New {
	/// Name of the new env: a bare name, or `namespace:member` for a member subshell.
	pub name: String,
	/// Source(s): seed names to compose, or a single directory / `*.nix` file to
	/// register (in stack order for compose).
	#[arg(long = "from", num_args = 1.., required = true)]
	pub from: Vec<String>,
	/// Register mode: copy the source shell into the registry (a frozen snapshot)
	/// instead of referencing it in place.
	#[arg(long)]
	pub copy: bool,
	/// On a lock conflict, repin the namespace to this shell's lock (re-pins every
	/// member).
	#[arg(long, conflicts_with = "use_prior")]
	pub overwrite: bool,
	/// On a lock conflict, keep the namespace's existing pin (this shell conforms).
	#[arg(long = "use-prior")]
	pub use_prior: bool,
}

impl RunCmd for New {
	fn run(self, ctx: &Context) -> Result<()> {
		// Register mode iff `--from` is a single directory/file shell source;
		// otherwise the tokens are seed names to compose.
		if let [only] = self.from.as_slice() {
			if is_shell_source(only) {
				return self.register_shell(ctx, only.clone());
			}
		}
		self.compose_seeds(ctx)
	}
}

impl New {
	/// **compose** mode: materialize a stack of seeds into `envs/<name>/`. Bare name
	/// only (a `namespace:member` compose target is deferred).
	fn compose_seeds(&self, ctx: &Context) -> Result<()> {
		let cfg = &ctx.config;
		if self.name.contains(':') {
			return Err(crate::error::unimplemented(
				"composing seeds into a namespace:member",
				"register a single shell into a member, or compose into a bare name",
			));
		}
		registry::validate_name(&self.name)?;
		let dest = registry::env_root(cfg, &self.name);
		if dest.exists() {
			return Err(ClinixError::EnvExists(self.name.clone()));
		}

		let settings = Settings::load(&cfg.config_dir)?;
		let catalog = Catalog::build(&settings.env.seeds);
		let mut seeds: Vec<(String, PathBuf)> = Vec::with_capacity(self.from.len());
		for name in &self.from {
			match catalog.find(name) {
				Some(Resolved::One(s)) => seeds.push((s.name.clone(), s.path.clone())),
				Some(Resolved::Collision { chosen, .. }) => {
					seeds.push((chosen.name.clone(), chosen.path.clone()))
				}
				None => {
					return Err(ClinixError::Config(format!(
						"`new --from` names must be seeds (or a single dir/file to register); \
						 `{name}` is not one"
					)));
				}
			}
		}

		let lock = super::env::seed_lock(cfg, &settings)?;
		let seeds_dir = dest.join("seeds");
		std::fs::create_dir_all(&seeds_dir)?;
		for (sname, spath) in &seeds {
			std::fs::copy(spath, seeds_dir.join(format!("{sname}.nix")))?;
		}
		std::fs::copy(&lock, dest.join("flake.lock"))?;
		let with_config = match super::env::nixpkgs_config_path(&settings)? {
			Some(src) => {
				std::fs::copy(&src, dest.join("nixpkgs-config.nix"))?;
				true
			}
			None => false,
		};
		let label = seeds
			.iter()
			.map(|(n, _)| n.as_str())
			.collect::<Vec<_>>()
			.join(" ");
		std::fs::write(
			dest.join("shell.nix"),
			render_composed_shell(&seeds, &label, with_config),
		)?;

		super::env::warn_name_collision(cfg, &self.name);
		println!(
			"clinix: created registry env `{}` from {} seed(s): {label}",
			self.name,
			seeds.len()
		);
		println!("  enter: clinix env {}", self.name);
		Ok(())
	}

	/// **register** mode: register a single directory/file shell as `<name>` or
	/// `namespace:member`, with the lock-coherence rules.
	fn register_shell(&self, ctx: &Context, src_token: String) -> Result<()> {
		let cfg = &ctx.config;
		let target = Dest::parse(cfg, &self.name)?;
		if target.dir.exists() {
			return Err(ClinixError::EnvExists(self.name.clone()));
		}

		// Resolve the source shell file (a file directly, or a dir's shell/default).
		let src = Path::new(&src_token);
		let source_shell = if src.is_file() {
			std::fs::canonicalize(src)?
		} else if src.is_dir() {
			super::env::env_shell_file(&std::fs::canonicalize(src)?)?
		} else {
			return Err(ClinixError::Resolve(format!(
				"no such source shell: {src_token}"
			)));
		};
		let source_dir = source_shell.parent().unwrap_or(Path::new("."));
		let source_text = std::fs::read_to_string(&source_shell)?;
		let source_lock = source_flake_lock(&source_text, source_dir);

		// A member may create its namespace empty (+warn) so a dev shell can be
		// staged before the runtime shell is registered.
		if target.is_member && !target.namespace_dir.exists() {
			std::fs::create_dir_all(&target.namespace_dir)?;
			eprintln!(
				"clinix: warning: created empty namespace `{}` (no runtime shell yet) for member `{}`",
				target.top, self.name
			);
		}

		// Lock coherence → whether the wrapper injects pkgs from the namespace pin.
		let inject = self.resolve_lock(&target, source_lock.as_deref())?;

		std::fs::create_dir_all(&target.dir)?;
		let source_expr = if self.copy {
			let copied = target.dir.join("source.nix");
			std::fs::copy(&source_shell, &copied)?;
			"./source.nix".to_string()
		} else {
			nix_string(&source_shell)
		};
		let wrapper = if inject {
			render_wrapper_with_lock(target.lock_read, &source_expr, &target.top)
		} else {
			render_wrapper_no_lock(&source_expr)
		};
		std::fs::write(target.dir.join("shell.nix"), wrapper)?;

		super::env::warn_name_collision(cfg, &target.top);
		let mode = if self.copy { "copied" } else { "referenced" };
		println!(
			"clinix: registered `{}` ({mode} {})",
			self.name,
			source_shell.display()
		);
		println!("  enter: clinix env {}", self.name);
		Ok(())
	}

	/// Apply the lock-coherence rules for the source's detected lock against the
	/// namespace pin. Returns whether the wrapper should inject `pkgs` from the pin
	/// (true when a usable pin is present/established; false → unpinned, entered
	/// as-is with a warning).
	fn resolve_lock(&self, target: &Dest, source_lock: Option<&Path>) -> Result<bool> {
		let src_lock = match source_lock {
			Some(p) if p.is_file() => p,
			Some(missing) => {
				eprintln!(
					"clinix: warning: `{}` reads a flake.lock that was not found ({}) — \
					 registering unpinned; not guaranteed reproducible",
					self.name,
					missing.display()
				);
				return Ok(false);
			}
			None => {
				eprintln!(
					"clinix: warning: `{}` reads no flake.lock — registering unpinned; \
					 it depends on a flake/source not vendored (not guaranteed reproducible)",
					self.name
				);
				return Ok(false);
			}
		};

		let has_pin = target.lock_path.is_file();
		let matches = has_pin && files_equal(src_lock, &target.lock_path)?;
		match decide_lock(has_pin, matches, self.overwrite, self.use_prior) {
			LockAction::Establish | LockAction::Overwrite => {
				if let Some(parent) = target.lock_path.parent() {
					std::fs::create_dir_all(parent)?;
				}
				std::fs::copy(src_lock, &target.lock_path)?;
			}
			LockAction::Reuse | LockAction::UsePrior => {}
			LockAction::Conflict => {
				return Err(ClinixError::Config(format!(
					"the lock read by `{}` differs from namespace `{}`'s existing pin \
					 ({}). Re-run with --overwrite (repin the namespace to this shell's \
					 lock) or --use-prior (conform to the namespace pin)",
					self.name,
					target.top,
					target.lock_path.display()
				)));
			}
		}
		Ok(true)
	}
}

/// The registry destination a `new` target resolves to.
struct Dest {
	/// The top-level namespace/name (collision-warn + empty-namespace handling).
	top: String,
	/// Directory that receives the wrapper `shell.nix`.
	dir: PathBuf,
	/// The namespace pin lock path (shared across a namespace's members).
	lock_path: PathBuf,
	/// The relative `readFile` path the wrapper uses to reach the pin.
	lock_read: &'static str,
	/// The namespace directory (== `dir` for a bare name; the parent for a member).
	namespace_dir: PathBuf,
	is_member: bool,
}

impl Dest {
	fn parse(cfg: &crate::env::config::Config, name: &str) -> Result<Dest> {
		if let Some((ns, member)) = name.split_once(':') {
			registry::validate_name(ns)?;
			registry::validate_name(member)?;
			let namespace_dir = registry::env_root(cfg, ns);
			Ok(Dest {
				top: ns.to_string(),
				dir: namespace_dir.join(member),
				lock_path: namespace_dir.join("flake.lock"),
				lock_read: "../flake.lock",
				namespace_dir,
				is_member: true,
			})
		} else {
			registry::validate_name(name)?;
			let dir = registry::env_root(cfg, name);
			Ok(Dest {
				top: name.to_string(),
				lock_path: dir.join("flake.lock"),
				lock_read: "./flake.lock",
				namespace_dir: dir.clone(),
				dir,
				is_member: false,
			})
		}
	}
}

/// Whether a `--from` token denotes a shell source (dir/file) rather than a seed
/// name: a `*.nix` name, an existing file, a `/`-bearing path, or an existing dir
/// (incl. `.`).
fn is_shell_source(token: &str) -> bool {
	let p = Path::new(token);
	token.ends_with(".nix") || token.contains('/') || token == "." || p.is_file() || p.is_dir()
}

/// The lock-coherence decision for a source lock vs the namespace pin.
#[derive(Debug, PartialEq, Eq)]
enum LockAction {
	/// No pin yet → this shell's lock becomes it.
	Establish,
	/// Identical to the pin → keep the pin (dedup).
	Reuse,
	/// Differs, `--overwrite` → repin the namespace.
	Overwrite,
	/// Differs, `--use-prior` → keep the pin, this shell conforms.
	UsePrior,
	/// Differs, no flag → hard error.
	Conflict,
}

fn decide_lock(has_pin: bool, matches: bool, overwrite: bool, use_prior: bool) -> LockAction {
	if !has_pin {
		LockAction::Establish
	} else if matches {
		LockAction::Reuse
	} else if overwrite {
		LockAction::Overwrite
	} else if use_prior {
		LockAction::UsePrior
	} else {
		LockAction::Conflict
	}
}

/// Parse a shell source for a `readFile …/flake.lock` reference and resolve it
/// against the shell's directory. Returns the lock path if the shell reads one
/// (the "has a lock" test of the coherence rules) — a **syntactic** rnix scan for
/// a path literal ending in `flake.lock`, no eval.
fn source_flake_lock(src: &str, shell_dir: &Path) -> Option<PathBuf> {
	let parse = rnix::Root::parse(src);
	for node in parse.syntax().descendants() {
		if node.kind() == SyntaxKind::NODE_PATH {
			let text = node.text().to_string();
			let trimmed = text.trim();
			if trimmed.ends_with("flake.lock") {
				let p = Path::new(trimmed);
				return Some(if p.is_absolute() {
					p.to_path_buf()
				} else {
					shell_dir.join(p)
				});
			}
		}
	}
	None
}

/// Byte-identical comparison of two files (the current "match" test; a normalized
/// same-rev compare is a future refinement).
fn files_equal(a: &Path, b: &Path) -> Result<bool> {
	Ok(std::fs::read(a)? == std::fs::read(b)?)
}

/// A path as an escaped, double-quoted nix string literal (coerced to a path where
/// one is expected). Escapes `\`, `"`, and `${`.
fn nix_string(path: &Path) -> String {
	let s = path.to_string_lossy();
	let escaped = s
		.replace('\\', "\\\\")
		.replace('"', "\\\"")
		.replace("${", "\\${");
	format!("\"{escaped}\"")
}

/// The `pkgs`-injecting wrapper: reads the namespace pin and passes `pkgs` into the
/// source shell, so one lock governs the namespace. `@LOCK@` is a bare relative
/// path (`./flake.lock` or `../flake.lock`); `@SOURCE@` is `./source.nix` (copy) or
/// a quoted absolute path (non-copy).
fn render_wrapper_with_lock(lock_read: &str, source_expr: &str, name: &str) -> String {
	WRAPPER_WITH_LOCK
		.replace("@LOCK@", lock_read)
		.replace("@SOURCE@", source_expr)
		.replace("@NAME@", &name.replace(' ', "-"))
}

/// The unpinned wrapper: imports the source as-is (it supplies its own `pkgs`).
fn render_wrapper_no_lock(source_expr: &str) -> String {
	format!("import {source_expr} {{ }}\n")
}

const WRAPPER_WITH_LOCK: &str = r##"{ system ? builtins.currentSystem }:
let
  lock = builtins.fromJSON (builtins.readFile @LOCK@);
  fetch = node:
    let i = lock.nodes.${node}.locked; in
    if i.type == "github" then
      builtins.fetchTarball { url = "https://github.com/${i.owner}/${i.repo}/archive/${i.rev}.tar.gz"; sha256 = i.narHash; }
    else if i.type == "git" then
      (builtins.fetchGit { inherit (i) url rev; }).outPath
    else throw "clinix: unsupported input type '${i.type}'";
  sources = builtins.mapAttrs (_: fetch) lock.nodes.root.inputs;
  pkgs = import sources.nixpkgs { inherit system; };
in
import @SOURCE@ { inherit pkgs; }
"##;

/// The self-contained `shell.nix` for a **composed** seed env: reads
/// `./flake.lock`, imports the pinned nixpkgs, unions the local seed copies via
/// `inputsFrom`. Portable — no reference to the user's source paths.
fn render_composed_shell(seeds: &[(String, PathBuf)], label: &str, with_config: bool) -> String {
	let imports = seeds
		.iter()
		.map(|(n, _)| format!("    (import ./seeds/{n}.nix {{ inherit pkgs; }})"))
		.collect::<Vec<_>>()
		.join("\n");
	let sanitized = label.replace(' ', "-");
	let config = if with_config {
		" config = import ./nixpkgs-config.nix;"
	} else {
		""
	};
	COMPOSED_TEMPLATE
		.replace("@CONFIG@", config)
		.replace("@NAME@", &sanitized)
		.replace("@IMPORTS@", &imports)
		.replace("@LABEL@", label)
}

const COMPOSED_TEMPLATE: &str = r##"{ system ? builtins.currentSystem }:
let
  lock = builtins.fromJSON (builtins.readFile ./flake.lock);
  fetch = node:
    let i = lock.nodes.${node}.locked; in
    if i.type == "github" then
      builtins.fetchTarball { url = "https://github.com/${i.owner}/${i.repo}/archive/${i.rev}.tar.gz"; sha256 = i.narHash; }
    else if i.type == "git" then
      (builtins.fetchGit { inherit (i) url rev; }).outPath
    else throw "clinix: unsupported input type '${i.type}'";
  sources = builtins.mapAttrs (_: fetch) lock.nodes.root.inputs;
  pkgs = import sources.nixpkgs { inherit system;@CONFIG@ };
in
pkgs.mkShell {
  name = "@NAME@";
  inputsFrom = [
@IMPORTS@
  ];
  shellHook = "export name=${pkgs.lib.escapeShellArg ''@LABEL@''}\n";
}
"##;

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn decide_lock_covers_the_coherence_table() {
		// No pin → establish; identical → reuse (regardless of flags).
		assert_eq!(decide_lock(false, false, false, false), LockAction::Establish);
		assert_eq!(decide_lock(true, true, false, false), LockAction::Reuse);
		// Differs → conflict, unless a resolution flag is given.
		assert_eq!(decide_lock(true, false, false, false), LockAction::Conflict);
		assert_eq!(decide_lock(true, false, true, false), LockAction::Overwrite);
		assert_eq!(decide_lock(true, false, false, true), LockAction::UsePrior);
	}

	#[test]
	fn source_flake_lock_detects_a_lock_read() {
		let dir = Path::new("/env/x");
		// A clinix-style shell that reads ./flake.lock.
		let src = "{ sources ? (builtins.fromJSON (builtins.readFile ./flake.lock)) }: {}";
		assert_eq!(
			source_flake_lock(src, dir),
			Some(PathBuf::from("/env/x/flake.lock"))
		);
		// A shell that reads no lock → None (the unpinned-warn case).
		assert_eq!(source_flake_lock("{ pkgs }: pkgs.mkShell { }", dir), None);
	}

	#[test]
	fn wrapper_with_lock_injects_pkgs_from_the_pin() {
		// A member reads the shared `../flake.lock` and injects pkgs into the source.
		let w = render_wrapper_with_lock("../flake.lock", "\"/abs/dev.nix\"", "proj");
		assert!(w.contains("builtins.readFile ../flake.lock"));
		assert!(w.contains("import \"/abs/dev.nix\" { inherit pkgs; }"));
		// A runtime env reads its own `./flake.lock`; copy mode imports the copy.
		let w2 = render_wrapper_with_lock("./flake.lock", "./source.nix", "x");
		assert!(w2.contains("builtins.readFile ./flake.lock"));
		assert!(w2.contains("import ./source.nix { inherit pkgs; }"));
	}

	#[test]
	fn wrapper_no_lock_imports_as_is() {
		assert_eq!(
			render_wrapper_no_lock("\"/abs/dev.nix\""),
			"import \"/abs/dev.nix\" { }\n"
		);
	}

	#[test]
	fn render_composed_shell_is_self_contained_and_portable() {
		let seeds = vec![
			("rust".to_string(), PathBuf::from("/src/rust.nix")),
			("claude".to_string(), PathBuf::from("/src/claude.nix")),
		];
		let s = render_composed_shell(&seeds, "rust claude", false);
		assert!(s.contains("builtins.readFile ./flake.lock"));
		assert!(s.contains("import ./seeds/rust.nix { inherit pkgs; }"));
		assert!(!s.contains("/src/"), "must not reference source paths");
		assert!(s.contains("name = \"rust-claude\";"));

		let s2 = render_composed_shell(&seeds, "rust claude", true);
		assert!(s2.contains("config = import ./nixpkgs-config.nix;"));
	}

	#[test]
	fn is_shell_source_distinguishes_paths_from_seed_names() {
		assert!(is_shell_source("./dev.nix"));
		assert!(is_shell_source("dev.nix"));
		assert!(is_shell_source("."));
		assert!(is_shell_source("some/dir"));
		assert!(!is_shell_source("rust")); // a bare seed name
		assert!(!is_shell_source("claude"));
	}
}
