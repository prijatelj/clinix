//! The `env` scope: one unified surface over the former dev/user/run scopes.
//!
//! An environment is identified by a **name** that [`resolve`] maps to a **root
//! directory** holding `shell.nix` + `flake.nix` + `flake.lock` (the single
//! source of truth, per the locked design). Two population sources — a
//! **registry** of named, composable tool envs under the state dir, and
//! **project** directories resolved by path/cwd — are what the dev+run+user
//! merge collapses to: they differ only in resolution, not in command surface.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use serde_json::Value;

use crate::error::{ClinixError, Result, unimplemented};
use crate::model::lock::{FlakeLock, InputRef, Node, Source};
use crate::pkg::Pkg;

/// An environment type resolved to its root directory
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
	/// A named, reusable, composable env under the clinix state dir.
	Registry,
	/// A project directory (given by path, or the cwd when no name is given).
	Project,
}

/// Composition options read from the global flags (so the `external_subcommand`
/// bare-name sugar can still set them). See [`crate::cli`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ShellOptions {
	/// Preserve the given order instead of sorting names lexically.
	pub ordered: bool,
	/// In a project, compose only the runtime env (skip dev tools).
	pub runtime: bool,
}

/// Shared execution context: state dir, verbosity, resolved options, etc.
pub struct Context {
	pub options: ShellOptions,
}

pub trait RunCmd {
	fn run(self, context: &Context) -> Result<()>;
}

/// A resolved environment: a root dir plus how we found it.
#[derive(Debug, Clone)]
pub struct Env {
	/// `None` for an anonymous cwd project; otherwise the resolved name.
	pub name: Option<String>,
	/// Directory containing `shell.nix` / `flake.nix` / `flake.lock`.
	pub root: PathBuf,
	pub kind: Kind,
}

/// The one per-source-divergent seam (plan §resolve). No nix eval, no lock read —
/// pure name → root-dir resolution:
/// - `Some(".")` or `None` → the cwd project ([`Kind::Project`]);
/// - `Some(name)` that is a path (contains `/` or is an existing dir) →
///   [`Kind::Project`] at that path;
/// - `Some(name)` otherwise → a registry env at `state/envs/<name>` if that dir
///   exists ([`Kind::Registry`]); else [`ClinixError::UnknownEnv`].
///
/// Full registry indexing/enumeration (listing, config default) lands with
/// `state.rs` in phase 5; this resolves the registry *path* by convention so
/// `init`/`shell` work against named envs now.
pub fn resolve(name: Option<&str>) -> Result<Env> {
	match name {
		None | Some(".") => Ok(Env {
			name: None,
			root: std::env::current_dir()?,
			kind: Kind::Project,
		}),
		Some(n) => {
			let path = Path::new(n);
			if n.contains('/') || path.is_dir() {
				return Ok(Env {
					name: Some(n.to_string()),
					root: std::fs::canonicalize(path)?,
					kind: Kind::Project,
				});
			}
			let root = registry_dir().join(n);
			if root.is_dir() {
				Ok(Env {
					name: Some(n.to_string()),
					root,
					kind: Kind::Registry,
				})
			} else {
				Err(ClinixError::UnknownEnv(n.to_string()))
			}
		}
	}
}

/// The clinix state root: `$XDG_STATE_HOME/clinix`, else `~/.local/state/clinix`.
/// A minimal stand-in until `state.rs` (phase 5) adopts `etcetera` + config.
fn state_dir() -> PathBuf {
	if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
		PathBuf::from(xdg).join("clinix")
	} else if let Some(home) = std::env::var_os("HOME") {
		PathBuf::from(home).join(".local/state/clinix")
	} else {
		PathBuf::from(".clinix-state")
	}
}

/// Where registry envs live: `state/envs/<name>` (plan §state layout).
fn registry_dir() -> PathBuf {
	state_dir().join("envs")
}

/// The unified verb set. Every verb takes an env name (or several, for the
/// compositional ones: `shell`, `run`, `shared`).
#[derive(Subcommand, Debug)]
pub enum Cmd {
	/// Scaffold a new **project** env in a directory (adopt via `--from` is a
	/// later slice; init never overwrites existing project files).
	Init(Init),
	/// Create a new **registry** env by merging existing envs (`--from A B C`).
	New(New),
	/// Enter an interactive shell for the composed env(s).
	Shell(Shell),
	/// Run a command inside the composed env(s), non-interactively.
	Run(Run),
	/// Add packages to an env's `shell.nix`.
	Add(Pkgs),
	/// Remove packages from an env's `shell.nix`.
	Remove(Pkgs),
	/// Pin package versions in `flake.lock` (`--all` = closure freeze).
	Pin(Pin),
	/// Unpin packages back to baseline tracking (`--all` = unfreeze).
	Unpin(Pin),
	/// Update unpinned packages to the latest the baseline provides.
	Update(Update),
	/// Rename a registered env.
	Rename(Rename),
	/// List registered envs.
	List,
	/// Import an env from another format (shell.nix / flake / .deb / OCI / …).
	Import(Import),
	/// Export an env to another format.
	Export(Export),
	/// Summarize a single env (packages, pins, diagnostics).
	Info(Target),
	/// Dependency/closure report for an env.
	Deps(Target),
	/// N-way shared-package comparison across several envs.
	Shared(Targets),
	/// Environment/PATH audit (the former `envcheck`).
	Check(OptionalTarget),
	/// Release an env's GC root so its store paths can be collected.
	Clean(Target),
}
impl RunCmd for Cmd {
	fn run(self, context: &Context) -> Result<()> {
		use Cmd::*;
		match self {
			// impl their own RunCmd
			Init(a) 	=> a.run(context),
			New(a)		=> a.run(context),
			Shell(a)	=> a.run(context),
			Run(a)		=> a.run(context),
			Update(a)	=> a.run(context),
			Rename(a)	=> a.run(context),
			Import(a)	=> a.run(context),
			Export(a)	=> a.run(context),
			// Generic / shared arg shapes.
			Add(a) 		=> Err(unimplemented("env add", "plan phase 5: rnix-parser splice")),
			Remove(a)	=> Err(unimplemented("env remove", "plan phase 5: rnix-parser splice")),
			Pin(a) 		=> Err(unimplemented("env pin", "plan phase 3/4: flake.lock version pin")),
			Unpin(a)	=> Err(unimplemented("env unpin", "plan phase 3/4: flake.lock unpin")),
			List			=> Err(unimplemented("env list", "plan phase 5: enumerate registry")),
			Info(a)		=> Err(unimplemented("env info", "plan phase 6: single-env summary")),
			Deps(a)		=> Err(unimplemented("env deps", "plan phase 6: closure report")),
			Shared(a)	=> Err(unimplemented("env shared", "plan phase 6: N-way shared-set comparison")),
			Check(a)	=> Err(unimplemented("env check", "plan phase 6: env/PATH audit (envcheck)")),
			Clean(a)	=> Err(unimplemented("env clean", "plan phase 5: remove GC root")),
		}
	}
}

/// The arguments for every command.
#[derive(Args, Debug)]
pub struct EnvArgs {
	#[command(subcommand)]
	pub cmd: Cmd,
}

/// Scaffold a **project** env (`Kind::Project`) in a directory. A project is
/// identified by its directory (not a name — `resolve` finds it by path/cwd, and
/// `new` owns named registry envs), so `init` takes only an optional `path`.
///
/// Default output is the primary pair — `shell.nix` (the vanilla `nix-shell`
/// entry point) + `flake.lock` (its version-controlled pin, which clinix owns
/// and writes classically, no `nix flake`). `--flake` additionally emits a
/// `flake.nix` that *wraps* `shell.nix`, so `nix develop`/flake users consume
/// the very same `flake.lock`.
#[derive(Args, Debug)]
pub struct Init {
	/// Directory to scaffold the project in (default: the current directory).
	pub path: Option<PathBuf>,
	/// Seed packages (`-p ripgrep -p nodejs`) written into the `packages` list.
	/// Versioned specs (`nodejs=20.11`) need package-level pinning (phase 4) and
	/// are rejected until then.
	#[arg(short = 'p', long = "pkg")]
	pub packages: Vec<Pkg>,
	/// nixpkgs ref to pin: a branch/tag (default `nixos-26.05`, the latest
	/// stable) or a 40-hex rev (frozen). clinix resolves and locks it classically
	/// (`git ls-remote` + `nix-prefetch-url`).
	#[arg(long = "nixpkgs", default_value = "nixos-26.05")]
	pub nixpkgs: String,
	/// Also emit `flake.nix` (wraps `shell.nix`) for `nix develop`/flake users.
	/// Off by default — `shell.nix` + `flake.lock` is the primary pair.
	#[arg(long = "flake")]
	pub flake: bool,
	/// Adopt an existing spec (`pyproject.toml`, `Cargo.toml`, `shell.nix`, …).
	#[arg(long = "from")]
	pub from: Option<PathBuf>,
}
impl RunCmd for Init {
	/// Resolve + build `flake.lock` classically, then write `shell.nix` +
	/// `flake.lock` (and `flake.nix` under `--flake`). Never clobbers existing
	/// project files, and does the network resolution *before* any write so a
	/// failure leaves the directory untouched.
	fn run(self, _context: &Context) -> Result<()> {
		// `--from` adopt (ADR-2 state detection) and package-level version pins
		// are later slices; reject rather than silently drop their semantics.
		if self.from.is_some() {
			return Err(unimplemented(
				"env init --from",
				"plan phase 3+: adopt existing spec (ADR-2)",
			));
		}
		if self.packages.iter().any(|p| p.version.is_some()) {
			return Err(unimplemented(
				"env init with a versioned package",
				"plan phase 4: package-level version pinning",
			));
		}

		let root = match self.path {
			Some(ref p) => p.clone(),
			None => std::env::current_dir()?,
		};
		std::fs::create_dir_all(&root)?;

		// Clobber-check every target up front, before the network resolution, so
		// a pre-existing file aborts with nothing done.
		let shell_nix = root.join("shell.nix");
		let flake_lock = root.join("flake.lock");
		let flake_nix = root.join("flake.nix");
		ensure_absent(&shell_nix, &root)?;
		ensure_absent(&flake_lock, &root)?;
		if self.flake {
			ensure_absent(&flake_nix, &root)?;
		}

		// Resolve nixpkgs and build the lock (network); nothing written on error.
		let lock = build_nixpkgs_lock(&self.nixpkgs)?;

		std::fs::write(&shell_nix, render_shell_nix(&self.packages))?;
		std::fs::write(&flake_lock, lock.to_json())?;
		if self.flake {
			std::fs::write(&flake_nix, render_flake_nix(&self.nixpkgs))?;
		}

		println!(
			"clinix: initialized project env at {} (nixpkgs/{})",
			root.display(),
			self.nixpkgs
		);
		if self.flake {
			println!("clinix: wrote flake.nix wrapping shell.nix (flake users: `nix develop`)");
		}
		Ok(())
	}
}

/// Base project templates, baked into the binary (a distributable can't `cp`
/// sibling files). `include_str!` keeps them as ordinary, lintable `.nix`; the
/// larger `shells/` seed catalog can move to `include_dir` in phase 5.
const SHELL_NIX_TEMPLATE: &str = include_str!("../templates/project/shell.nix");
const FLAKE_NIX_TEMPLATE: &str = include_str!("../templates/project/flake.nix");
/// The anchor line in `shell.nix`'s `packages = with pkgs; [ … ]` list.
const PACKAGES_PLACEHOLDER: &str = "    # project dependencies go here";

/// Fill the `packages` list with the seed package names (or leave the
/// placeholder comment when there are none). init *creates* the file from a
/// known template, so this string splice is sufficient — span-preserving
/// `rnix-parser` editing (phase 5, `add`) is only needed for existing,
/// hand-edited files.
fn render_shell_nix(packages: &[Pkg]) -> String {
	if packages.is_empty() {
		return SHELL_NIX_TEMPLATE.to_string();
	}
	let list = packages
		.iter()
		.map(|p| format!("    {}", p.name))
		.collect::<Vec<_>>()
		.join("\n");
	SHELL_NIX_TEMPLATE.replacen(PACKAGES_PLACEHOLDER, &list, 1)
}

/// Retarget the `flake.nix` template's nixpkgs URL to the chosen ref (the
/// template names `nixos-unstable` so it is valid Nix standalone; this is the
/// same one-literal swap `pin`'s `freeze` uses). The template already wraps
/// `shell.nix`, so flake users get the same lock.
fn render_flake_nix(nixpkgs_ref: &str) -> String {
	FLAKE_NIX_TEMPLATE.replace(
		"github:NixOS/nixpkgs/nixos-unstable",
		&format!("github:NixOS/nixpkgs/{nixpkgs_ref}"),
	)
}

/// **Never-clobber**: an existing target file is an error, not an overwrite
/// (this is not an adopt).
fn ensure_absent(path: &Path, root: &Path) -> Result<()> {
	if path.exists() {
		return Err(ClinixError::AmbiguousInit {
			path: root.display().to_string(),
			detail: format!(
				"`{}` already exists; refusing to overwrite (this is not an adopt)",
				path.file_name().unwrap().to_string_lossy()
			),
		});
	}
	Ok(())
}

/// Resolve the nixpkgs ref and assemble a classic `flake.lock` for it — clinix
/// owns this state; no `nix flake`. A 40-hex `nixpkgs_ref` is recorded as a
/// frozen `original.rev`; anything else is a tracked `original.ref` resolved via
/// `git ls-remote`. The single node is a github source
/// (`{narHash, owner, repo, rev, type}`), mirroring `pin init` + `pin update`.
fn build_nixpkgs_lock(nixpkgs_ref: &str) -> Result<FlakeLock> {
	const OWNER: &str = "NixOS";
	const REPO: &str = "nixpkgs";

	let frozen = is_rev(nixpkgs_ref);
	let rev = if frozen {
		nixpkgs_ref.to_string()
	} else {
		crate::nix::resolve_github_ref(OWNER, REPO, nixpkgs_ref)?
			.as_str()
			.to_string()
	};
	let narhash = crate::nix::github_tarball_narhash(OWNER, REPO, &rev)?;

	let locked = github_source(&[
		("narHash", narhash.as_str()),
		("owner", OWNER),
		("repo", REPO),
		("rev", &rev),
		("type", "github"),
	]);
	let original = if frozen {
		github_source(&[("owner", OWNER), ("repo", REPO), ("rev", &rev), ("type", "github")])
	} else {
		github_source(&[
			("owner", OWNER),
			("ref", nixpkgs_ref),
			("repo", REPO),
			("type", "github"),
		])
	};

	let nixpkgs_node = Node {
		locked: Some(locked),
		original: Some(original),
		..Node::default()
	};
	let mut root_inputs = BTreeMap::new();
	root_inputs.insert("nixpkgs".to_string(), InputRef::Direct("nixpkgs".to_string()));
	let root_node = Node {
		inputs: root_inputs,
		..Node::default()
	};

	let mut nodes = BTreeMap::new();
	nodes.insert("nixpkgs".to_string(), nixpkgs_node);
	nodes.insert("root".to_string(), root_node);

	Ok(FlakeLock {
		nodes,
		root: "root".to_string(),
		version: 7,
	})
}

/// Build a `Source` from ordered `(key, value)` string pairs.
fn github_source(pairs: &[(&str, &str)]) -> Source {
	Source(
		pairs
			.iter()
			.map(|(k, v)| (k.to_string(), Value::from(*v)))
			.collect(),
	)
}

/// A 40-char lowercase-hex string — the shape Nix records as `original.rev`
/// (frozen). Matches `pin`'s `is_rev`.
fn is_rev(s: &str) -> bool {
	s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Args, Debug)]
pub struct Shell {
	/// Envs to compose, as a stack. Empty ⇒ `[.]` (the cwd project). The token
	/// `.` denotes the cwd project and may appear anywhere in the stack (usually
	/// first); every other name resolves via [`resolve`].
	pub names: Vec<String>,
	/// Enter a pure shell (`nix-shell --pure`).
	#[arg(long)]
	pub pure: bool,
}
impl RunCmd for Shell {
	/// The launcher. Reached by `env shell`, the bare-name sugar, and the no-arg
	/// cwd case. Empty `names` ⇒ `[.]`; the `.` token composes the cwd project as
	/// an ordinary node anywhere in the stack. `base` is prepended, and the stack
	/// is lexically sorted unless `options.ordered`.
	fn run(self, context: &Context) -> Result<()> {
		Err(unimplemented("env shell", "plan phase 5: compose-dev/compose + GC-rooted enter"))
	}
}

/// Create a new **registry** env by merging existing envs. Distinct from
/// [`Init`], which scaffolds a **project** env in a directory: `new` writes a
/// reusable, named env into the state registry. The `--from` order is the merge
/// (stack) order. Sources are registry names; the cwd-project token `.` is not
/// valid here, since a registry env must be portable.
#[derive(Args, Debug)]
pub struct New {
	/// Name of the new registry env.
	pub name: String,
	/// Existing registry envs to merge, in stack order (`--from A B C`).
	#[arg(long = "from", num_args = 1.., required = true)]
	pub from: Vec<String>,
}
impl RunCmd for New {
	fn run(self, context: &Context) -> Result<()> {
		Err(unimplemented("env new", "plan phase 5: merge envs → registry item"))
	}
}

#[derive(Args, Debug)]
pub struct Run {
	/// Envs to compose, as a stack — same rules as [`Shell`] (`.` = cwd project,
	/// empty ⇒ `[.]`).
	pub names: Vec<String>,
	/// The command (and its args) to run inside the env; after `--`.
	#[arg(last = true, required = true)]
	pub command: Vec<String>,
}
impl RunCmd for Run {
	fn run(self, context: &Context) -> Result<()> {
		Err(unimplemented("env run", "plan phase 5: nix-shell --run"))
	}
}

/// A set of packages for a target environment
#[derive(Args, Debug)]
pub struct Pkgs {
	/// Target env name.
	pub name: String,
	/// Packages to add/remove (`name` or `name=version`).
	#[arg(required = true)]
	pub packages: Vec<Pkg>,
}

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
	fn run(self, context: &Context) -> Result<()> {
		Err(unimplemented("env update", "plan phase 3: flake.lock update"))
	}
}

#[derive(Args, Debug)]
pub struct Rename {
	pub old: String,
	pub new: String,
}
impl RunCmd for Rename {
	fn run(self, context: &Context) -> Result<()> {
		Err(unimplemented("env rename", "plan phase 5: registry relabel"))
	}
}

#[derive(Args, Debug)]
pub struct Import {
	/// Name for the imported env.
	pub name: String, // TODO could be optional if provided by source?
	/// Source file or reference to import from.
	pub source: PathBuf,
}
impl RunCmd for Import {
	fn run(self, context: &Context) -> Result<()> {
		Err(unimplemented("env import", "plan phase 7: import doctor (ADR-2/6)"))
	}
}

#[derive(Args, Debug)]
pub struct Export {
	/// Target env name.
	pub name: String,
	#[command(subcommand)]
	pub target: ExportTarget,
}
impl RunCmd for Export {
	fn run(self, context: &Context) -> Result<()> {
		Err(unimplemented("env export", "plan phase 7: docker/closure extension (ADR-6)"))
	}
}

#[derive(Subcommand, Debug)]
pub enum ExportTarget {
	/// Emit a reproducible OCI image (or a Dockerfile).
	Docker {
		/// Output path (defaults to a Dockerfile in the cwd).
		out: Option<PathBuf>,
	},
	/// Export the Nix closure to a directory.
	Closure { out_dir: PathBuf },
}

/// A single required env name.
#[derive(Args, Debug)]
pub struct Target {
	pub name: String,
}

/// A single optional env name (defaults to the cwd project).
#[derive(Args, Debug)]
pub struct OptionalTarget {
	pub name: Option<String>,
}

/// One or more env names.
#[derive(Args, Debug)]
pub struct Targets {
	#[arg(required = true)]
	pub names: Vec<String>,
}

#[cfg(test)]
mod tests {
	use super::*;

	fn pkg(name: &str) -> Pkg {
		Pkg {
			name: name.to_string(),
			version: None,
		}
	}

	#[test]
	fn render_shell_nix_splices_packages() {
		let shell = render_shell_nix(&[pkg("ripgrep"), pkg("nodejs")]);
		assert!(shell.contains("    ripgrep\n    nodejs"), "packages spliced");
		assert!(
			!shell.contains("# project dependencies go here"),
			"placeholder replaced"
		);
	}

	#[test]
	fn render_shell_nix_empty_keeps_placeholder() {
		assert!(render_shell_nix(&[]).contains("# project dependencies go here"));
	}

	#[test]
	fn render_flake_nix_retargets_the_ref() {
		let flake = render_flake_nix("nixos-26.05");
		assert!(flake.contains("github:NixOS/nixpkgs/nixos-26.05"));
		assert!(!flake.contains("nixos-unstable"));
	}

	#[test]
	fn ensure_absent_errors_on_existing_and_leaves_it() {
		let dir = tempfile::tempdir().unwrap();
		let f = dir.path().join("shell.nix");
		std::fs::write(&f, "hand-written").unwrap();
		let err = ensure_absent(&f, dir.path()).unwrap_err();
		assert!(matches!(err, ClinixError::AmbiguousInit { .. }));
		assert_eq!(std::fs::read_to_string(&f).unwrap(), "hand-written");
		// Absent path is fine.
		assert!(ensure_absent(&dir.path().join("flake.lock"), dir.path()).is_ok());
	}

	#[test]
	fn is_rev_matches_40_hex_only() {
		assert!(is_rev("062346a6d85bc4b49dfaa61c986e9c5be21217d1"));
		assert!(!is_rev("nixos-26.05"));
		assert!(!is_rev("ABC")); // too short / uppercase
	}

	// Network + classic-nix E2E: resolves nixos-26.05 and checks the built lock
	// round-trips through the model and has a github nixpkgs node. Gated so
	// offline/CI runs pass; run with `cargo test -- --ignored`.
	#[test]
	#[ignore = "requires network + git/nix-prefetch-url/nix-hash"]
	fn build_nixpkgs_lock_resolves_and_roundtrips() {
		let lock = build_nixpkgs_lock("nixos-26.05").expect("resolve+build");
		let nixpkgs = &lock.nodes["nixpkgs"];
		let locked = nixpkgs.locked.as_ref().unwrap();
		assert_eq!(locked.source_type(), Some("github"));
		assert_eq!(locked.owner(), Some("NixOS"));
		assert_eq!(locked.repo(), Some("nixpkgs"));
		assert!(locked.rev().is_some_and(|r| r.len() == 40));
		assert!(locked.nar_hash().is_some_and(|h| h.starts_with("sha256-")));
		// original tracks the branch ref.
		assert_eq!(nixpkgs.original.as_ref().unwrap().git_ref(), Some("nixos-26.05"));
		// Serializes to a well-formed, re-parseable lock.
		let text = lock.to_json();
		assert_eq!(FlakeLock::from_json(&text).unwrap(), lock);
	}
}
