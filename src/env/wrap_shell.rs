//! `flake <target> wrap-shell`: generate a `flake.nix` that wraps an existing
//! `shell.nix`, so `nix develop`/flake users can consume a shell that was written
//! for classic `nix-shell`. The point is a frictionless transition: a directory
//! that only had a `shell.nix` gains a flake front door without a second source of
//! truth for the nixpkgs pin.
//!
//! Three cases, keyed by how the wrapped `shell.nix` receives its arguments
//! ([`ShellClass`]). For a clinix env the class is known (clinix-native) and the
//! existing lock is authoritative; for a foreign shell the class is auto-detected
//! from the shell's formal arguments (`builtins.functionArgs`), overridable with
//! `--arg-style`.
//!
//! Lock handling is orthogonal to the arg class and is what keeps this honest:
//! - an env with a `flake.lock` → **reuse it, never clobber** (informed by it);
//! - a shell-only env (lock embedded in `shell.nix`) → **expand** into the three
//!   files, materializing `flake.lock` and de-embedding `shell.nix`, same env;
//! - a foreign conventional shell → **pin nixpkgs fresh** and write `flake.lock`;
//! - a foreign impure shell → no input, no lock (`nix develop --impure`).

use std::path::Path;

use clap::Args;

use crate::env::project::Project;
use crate::env::{Context, Env, resolve};
use crate::error::{ClinixError, Result};
use crate::model::lock::FlakeLock;
use crate::progress::Progress;

/// The clinix-native `flake.nix` (passes `inherit sources system; pkgs = …`),
/// reused verbatim from `init --flake` and retargeted to the env's pinned ref.
const FLAKE_NATIVE_TEMPLATE: &str = include_str!("../../templates/project/flake.nix");
/// The conventional wrapper (`pkgs = import nixpkgs { inherit system; }`).
const FLAKE_CONVENTIONAL_TEMPLATE: &str =
	include_str!("../../templates/project/flake-wrap-conventional.nix");
/// The impure wrapper (`import ./shell.nix { }`, entered `nix develop --impure`).
const FLAKE_IMPURE_TEMPLATE: &str = include_str!("../../templates/project/flake-wrap-impure.nix");
/// The nixpkgs URL literal both github templates ship with, swapped to the actual
/// pinned ref by [`retarget`]. Valid Nix standalone (same convention as `init`).
const TEMPLATE_URL: &str = "github:NixOS/nixpkgs/nixos-unstable";

/// How the generated `flake.nix` supplies arguments to the wrapped `shell.nix`,
/// determined by the shell's formal arguments (auto-detected, overridable with
/// `--arg-style`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ShellClass {
	/// A clinix-shaped shell: takes `sources`, `system`, and `pkgs` (all with
	/// defaults — the "one file, three entry points" form `clinix env init`
	/// writes). The flake passes `inherit sources system; pkgs = import nixpkgs
	/// {…}`, so `nix develop` reuses the very same lock and never instantiates a
	/// second nixpkgs. Only valid when a lock already exists (env or embedded).
	ClinixNative,
	/// A conventional community shell: takes a `pkgs` argument (e.g.
	/// `{ pkgs ? import <nixpkgs> {} }:`). The flake pins nixpkgs and passes
	/// `pkgs = import nixpkgs { inherit system; }`, making it reproducible under
	/// pure `nix develop` without reaching for `<nixpkgs>`/channels.
	Conventional,
	/// An opaque or channel-bound shell: no injectable `pkgs` (no formal
	/// arguments, a positional argument, or a top-level `import <nixpkgs>`). It
	/// cannot be wrapped purely, so the flake does `import ./shell.nix { }` and
	/// must be entered with `nix develop --impure` — reproducibility is lost.
	/// Requires the explicit `--impure` opt-in.
	Impure,
}

/// `flake <target> wrap-shell` — emit a `flake.nix` around the target's `shell.nix`.
#[derive(Args, Debug)]
pub struct WrapShell {
	/// Override the auto-detected argument style of the wrapped `shell.nix`.
	#[arg(long = "arg-style", value_enum)]
	pub arg_style: Option<ShellClass>,
	/// nixpkgs ref to pin for a *foreign* conventional shell that has no existing
	/// lock (branch/tag, default `nixos-26.05`, or a 40-hex rev). Ignored when the
	/// target already has a lock — clinix reuses that pin rather than re-resolving.
	#[arg(long = "nixpkgs", default_value = "nixos-26.05")]
	pub nixpkgs: String,
	/// Permit generating an impure wrapper (`nix develop --impure`) for a shell
	/// that cannot be wrapped purely. Required for the `impure` class; without it
	/// such a shell is refused with guidance rather than silently made unreproducible.
	#[arg(long = "impure")]
	pub impure: bool,
	/// Overwrite an existing `flake.nix`.
	#[arg(long = "force")]
	pub force: bool,
}

/// Resolve the target, then branch on whether it is a clinix env (authoritative
/// lock present) or a foreign shell. Network (fresh pin) only in the foreign
/// conventional case; everything else is offline.
pub fn wrap_shell(name: &str, args: WrapShell, ctx: &Context) -> Result<()> {
	let env = resolve(&ctx.config, Some(name))?;
	let shell_nix = env.require_shell_nix()?;
	let flake_nix = env.root.join("flake.nix");
	if flake_nix.exists() && !args.force {
		return Err(ClinixError::AmbiguousInit {
			path: env.root.display().to_string(),
			detail: "`flake.nix` already exists; refusing to overwrite (pass --force)".into(),
		});
	}

	// A clinix env loads its lock (from flake.lock or the shell-only embed); that
	// success is what makes it clinix-native and the lock authoritative. A load
	// failure means a foreign shell with no clinix lock to reuse.
	match Project::load(env.clone()) {
		Ok(project) => wrap_clinix_env(&env, &shell_nix, &flake_nix, project, &args),
		Err(_) => wrap_foreign(&env, &shell_nix, &flake_nix, &args),
	}
}

/// A clinix env: the shell is clinix-native and the pin comes from the loaded lock.
/// Two sub-cases — reuse an on-disk `flake.lock` (never clobbered) or expand a
/// shell-only env into the three files. All content is computed before any write
/// so a failure leaves the directory untouched.
fn wrap_clinix_env(
	env: &Env,
	shell_nix: &Path,
	flake_nix: &Path,
	project: Project,
	args: &WrapShell,
) -> Result<()> {
	if let Some(style) = args.arg_style
		&& style != ShellClass::ClinixNative
	{
		return Err(ClinixError::Resolve(
			"target is a clinix env (a lock is present) — it is clinix-native; drop \
			 --arg-style (or pass `clinix-native`)"
				.into(),
		));
	}

	let url = nixpkgs_url(&project.lock)?;
	let flake_src = retarget(FLAKE_NATIVE_TEMPLATE, &url);
	let flake_lock = env.root.join("flake.lock");

	if flake_lock.exists() {
		// Requirement: an env with shell.nix + flake.lock gains a flake.nix that is
		// *informed by* the existing lock (its pinned ref) and shell.nix (clinix-
		// native arg passing), and the flake.lock is left byte-for-byte untouched.
		std::fs::write(flake_nix, flake_src)?;
		println!(
			"clinix: wrote flake.nix wrapping shell.nix at {} (existing flake.lock reused, untouched)",
			env.root.display()
		);
	} else {
		// Single-file env: the lock is embedded in shell.nix. Expand into the three
		// files that work together — materialize flake.lock from the same lock and
		// de-embed shell.nix so it reads ./flake.lock — preserving the exact env.
		let shell_src = std::fs::read_to_string(shell_nix)?;
		let de_embedded = de_embed_shell(&shell_src)?;
		std::fs::write(&flake_lock, project.lock.to_json())?;
		super::pkgs::write_atomic(shell_nix, &de_embedded)?;
		std::fs::write(flake_nix, flake_src)?;
		println!(
			"clinix: expanded shell-only shell.nix at {} into shell.nix + flake.lock + flake.nix \
			 (same environment)",
			env.root.display()
		);
	}
	Ok(())
}

/// A foreign shell (no clinix lock): detect the arg class (or take `--arg-style`),
/// then pin fresh (conventional) or emit an impure wrapper.
fn wrap_foreign(env: &Env, shell_nix: &Path, flake_nix: &Path, args: &WrapShell) -> Result<()> {
	let class = match args.arg_style {
		Some(ShellClass::ClinixNative) => {
			return Err(ClinixError::Resolve(
				"target has no clinix lock, so --arg-style clinix-native does not apply — it needs a \
				 flake.lock or a shell-only embedded lock"
					.into(),
			));
		}
		Some(style) => style,
		None => detect_class(shell_nix)?,
	};

	match class {
		ShellClass::Conventional => {
			let flake_lock = env.root.join("flake.lock");
			if flake_lock.exists() && !args.force {
				return Err(ClinixError::AmbiguousInit {
					path: env.root.display().to_string(),
					detail: "`flake.lock` already exists; refusing to overwrite (pass --force)"
						.into(),
				});
			}
			// Fresh classic pin (network) before any write, so a resolve failure leaves
			// the directory untouched. Two steps tracked, one frozen — matches `init`.
			let steps = if crate::env::init::is_rev(&args.nixpkgs) {
				1
			} else {
				2
			};
			let progress = Progress::new(steps);
			let lock = crate::env::init::build_nixpkgs_lock(&args.nixpkgs, &progress)?;
			let url = nixpkgs_url(&lock)?;
			let flake_src = retarget(FLAKE_CONVENTIONAL_TEMPLATE, &url);
			std::fs::write(&flake_lock, lock.to_json())?;
			std::fs::write(flake_nix, flake_src)?;
			println!(
				"clinix: wrote flake.nix + flake.lock wrapping shell.nix at {} (nixpkgs/{}, `nix develop`)",
				env.root.display(),
				args.nixpkgs
			);
		}
		ShellClass::Impure => {
			if !args.impure {
				return Err(ClinixError::Resolve(
					"shell.nix cannot be wrapped purely: it exposes no injectable `pkgs` (no formal \
					 arguments, a positional argument, or a top-level `import <nixpkgs>`). Re-run with \
					 --impure to generate an impure wrapper (entered via `nix develop --impure`, not \
					 reproducible), or convert shell.nix to a `{ pkgs ? ... }:` function."
						.into(),
				));
			}
			std::fs::write(flake_nix, FLAKE_IMPURE_TEMPLATE)?;
			println!(
				"clinix: wrote IMPURE flake.nix wrapping shell.nix at {} — enter with `nix develop \
				 --impure` (not reproducible; see the file header)",
				env.root.display()
			);
		}
		// wrap_foreign never receives ClinixNative (rejected above / not detected).
		ShellClass::ClinixNative => unreachable!("clinix-native handled in wrap_clinix_env"),
	}
	Ok(())
}

/// Detect a foreign shell's arg class from its formal arguments via classic
/// `builtins.functionArgs` (reusing [`crate::nix::eval_json`], the single Nix
/// boundary). `isFunction` guards a non-function `shell.nix` (→ `[]` → impure); an
/// eval failure (e.g. a top-level `import <nixpkgs>` with no channel) is likewise
/// treated as opaque/impure rather than aborting.
fn detect_class(shell_nix: &Path) -> Result<ShellClass> {
	let expr = format!(
		"let f = import {}; in if builtins.isFunction f \
		 then builtins.attrNames (builtins.functionArgs f) else []",
		super::nix_expr::nix_str(shell_nix)
	);
	let arg_names: Vec<String> = match crate::nix::eval_json(&expr) {
		Ok(v) => serde_json::from_value(v).unwrap_or_default(),
		Err(_) => Vec::new(),
	};
	Ok(classify_args(&arg_names))
}

/// **Pure.** A shell exposing a `pkgs` argument can be wrapped purely
/// (conventional); anything else must run impurely.
fn classify_args(arg_names: &[String]) -> ShellClass {
	if arg_names.iter().any(|a| a == "pkgs") {
		ShellClass::Conventional
	} else {
		ShellClass::Impure
	}
}

/// Swap the template's placeholder nixpkgs URL for the real pinned one (exactly the
/// occurrences present; the templates carry it once).
fn retarget(template: &str, nixpkgs_url: &str) -> String {
	template.replace(TEMPLATE_URL, nixpkgs_url)
}

/// The `inputs.nixpkgs.url` a `flake.nix` must declare to reuse this lock's pin
/// without re-resolving: `github:<owner>/<repo>/<ref-or-rev>` from the `nixpkgs`
/// node's `original` (falling back to `locked` for owner/repo/rev). Errors if the
/// lock has no github `nixpkgs` node — flake-wrap only supports a github nixpkgs.
fn nixpkgs_url(lock: &FlakeLock) -> Result<String> {
	let node = lock
		.nodes
		.get("nixpkgs")
		.ok_or_else(|| ClinixError::Resolve("lock has no `nixpkgs` node to wrap".into()))?;
	let src = node
		.original
		.as_ref()
		.or(node.locked.as_ref())
		.ok_or_else(|| {
			ClinixError::Resolve("nixpkgs node has neither `original` nor `locked`".into())
		})?;
	if src.source_type() != Some("github") {
		return Err(ClinixError::Resolve(format!(
			"nixpkgs input is `{}`, not github — flake wrap-shell supports a github nixpkgs pin",
			src.source_type().unwrap_or("?")
		)));
	}
	let owner = src
		.owner()
		.ok_or_else(|| ClinixError::Resolve("nixpkgs original: missing owner".into()))?;
	let repo = src
		.repo()
		.ok_or_else(|| ClinixError::Resolve("nixpkgs original: missing repo".into()))?;
	// A tracked input carries a ref; a frozen one carries a rev. Either is a valid
	// flake URL fragment and yields the matching `original` when Nix re-parses it.
	let ref_or_rev = src
		.git_ref()
		.or_else(|| src.rev())
		.ok_or_else(|| ClinixError::Resolve("nixpkgs original: neither ref nor rev".into()))?;
	Ok(format!("github:{owner}/{repo}/{ref_or_rev}"))
}

/// Reverse `init --shell-only`'s embed: replace the `builtins.fromJSON (''<json>'')`
/// here-string with `builtins.fromJSON (builtins.readFile ./flake.lock)`, so the
/// expanded `shell.nix` reads its sibling `flake.lock` again. Mirrors the locator in
/// [`crate::env::project`] (the lock JSON contains no `''`, so the naive close works).
fn de_embed_shell(shell_src: &str) -> Result<String> {
	const PREFIX: &str = "builtins.fromJSON (";
	const OPEN: &str = "builtins.fromJSON (''";
	const READ: &str = "builtins.readFile ./flake.lock";
	let open_at = shell_src.find(OPEN).ok_or_else(|| {
		ClinixError::Resolve(
			"shell.nix is not a shell-only env (no embedded lock to expand)".into(),
		)
	})?;
	// init embedded the lock by swapping only `builtins.readFile ./flake.lock` for a
	// `''<json>''` here-string, leaving `builtins.fromJSON (` and its `)` in place.
	// So reverse exactly that span: from the here-string's opening `''` (right after
	// `builtins.fromJSON (`) to its closing `''`, keeping the surrounding call intact.
	let hs_open = open_at + PREFIX.len();
	let after_open = hs_open + 2;
	let close_rel = shell_src[after_open..]
		.find("''")
		.ok_or_else(|| ClinixError::Resolve("shell.nix: unterminated embedded lock".into()))?;
	let hs_close = after_open + close_rel + 2;
	let mut out = String::with_capacity(shell_src.len());
	out.push_str(&shell_src[..hs_open]);
	out.push_str(READ);
	out.push_str(&shell_src[hs_close..]);
	Ok(out)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::lock::{FlakeLock, InputRef, Node, Source};
	use std::collections::BTreeMap;

	fn lock_with(nixpkgs: Node) -> FlakeLock {
		let mut root_inputs = BTreeMap::new();
		root_inputs.insert(
			"nixpkgs".to_string(),
			InputRef::Direct("nixpkgs".to_string()),
		);
		let mut nodes = BTreeMap::new();
		nodes.insert("nixpkgs".to_string(), nixpkgs);
		nodes.insert(
			"root".to_string(),
			Node {
				inputs: root_inputs,
				..Node::default()
			},
		);
		FlakeLock {
			nodes,
			root: "root".to_string(),
			version: 7,
		}
	}

	#[test]
	fn nixpkgs_url_uses_the_tracked_ref() {
		let lock = lock_with(Node {
			locked: Some(Source::github_locked(
				"NixOS", "nixpkgs", "abc123", "sha256-x",
			)),
			original: Some(Source::github_ref("NixOS", "nixpkgs", "nixos-26.05")),
			..Node::default()
		});
		assert_eq!(
			nixpkgs_url(&lock).unwrap(),
			"github:NixOS/nixpkgs/nixos-26.05"
		);
	}

	#[test]
	fn nixpkgs_url_uses_the_rev_when_frozen() {
		let lock = lock_with(Node {
			locked: Some(Source::github_locked(
				"NixOS", "nixpkgs", "deadbeef", "sha256-x",
			)),
			original: Some(Source::github_rev("NixOS", "nixpkgs", "deadbeef")),
			..Node::default()
		});
		assert_eq!(nixpkgs_url(&lock).unwrap(), "github:NixOS/nixpkgs/deadbeef");
	}

	#[test]
	fn retarget_swaps_the_native_template_url() {
		let out = retarget(FLAKE_NATIVE_TEMPLATE, "github:NixOS/nixpkgs/nixos-26.05");
		assert!(out.contains("github:NixOS/nixpkgs/nixos-26.05"));
		assert!(!out.contains(TEMPLATE_URL));
		// The native wrapper passes clinix's sources/system/pkgs trio.
		assert!(out.contains("inherit sources system"));
	}

	#[test]
	fn conventional_template_injects_pkgs() {
		let out = retarget(
			FLAKE_CONVENTIONAL_TEMPLATE,
			"github:NixOS/nixpkgs/nixos-26.05",
		);
		assert!(out.contains("github:NixOS/nixpkgs/nixos-26.05"));
		assert!(out.contains("import ./shell.nix { pkgs = import nixpkgs { inherit system; }; }"));
	}

	#[test]
	fn impure_template_is_input_free_and_warns() {
		assert!(!FLAKE_IMPURE_TEMPLATE.contains("inputs"));
		assert!(FLAKE_IMPURE_TEMPLATE.contains("nix develop --impure"));
		assert!(FLAKE_IMPURE_TEMPLATE.contains("import ./shell.nix { }"));
	}

	#[test]
	fn classify_prefers_conventional_when_pkgs_present() {
		assert_eq!(
			classify_args(&["pkgs".into(), "system".into()]),
			ShellClass::Conventional
		);
		// A clinix-shaped shell (sources/system/pkgs) is also injectable via pkgs.
		assert_eq!(
			classify_args(&["sources".into(), "system".into(), "pkgs".into()]),
			ShellClass::Conventional
		);
		// No injectable pkgs → impure.
		assert_eq!(classify_args(&[]), ShellClass::Impure);
		assert_eq!(classify_args(&["config".into()]), ShellClass::Impure);
	}

	#[test]
	fn de_embed_restores_the_flake_lock_read() {
		// The shell-only shape init writes: `builtins.fromJSON (''<json>'')`.
		let embedded = "let lock = builtins.fromJSON (''\n{\"version\":7}''); in lock\n";
		let out = de_embed_shell(embedded).unwrap();
		assert_eq!(
			out,
			"let lock = builtins.fromJSON (builtins.readFile ./flake.lock); in lock\n"
		);
	}

	#[test]
	fn de_embed_errors_when_not_shell_only() {
		assert!(de_embed_shell("builtins.fromJSON (builtins.readFile ./flake.lock)").is_err());
	}
}
