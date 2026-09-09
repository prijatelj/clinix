//! Diagnostics: `info`/`deps`/`list`/`check`/`shared` — read-only reports over
//! resolved envs, plus the contextual top-level `clinix info`. Each verb lives in
//! its own submodule (matching the crate's one-file-per-verb convention); this
//! module wires them together and holds the helpers they share.
//!
//! `check` (PATH audit) and `shared` (N-way closure compare) port the
//! `envcheck`/`shareck` prototype scripts; the top-level [`context_report`]
//! (`clinix info`) reuses these verbs against the active (cwd) env.

use std::collections::BTreeSet;

use clap::Subcommand;

use crate::env::project::Project;
use crate::env::{Context, Kind, OptionalTarget, RunCmd};
use crate::error::Result;

mod check;
mod deps;
mod info;
mod list;
mod roots;
mod shared;

pub use check::check;
pub use deps::Deps;
pub use info::Info;
pub use list::{List, list};
pub use roots::{Roots, roots};
pub use shared::shared;

// ---- helpers shared across the diagnostics verbs ----------------------------

/// The env's kind as a short label for the report headers.
fn kind_str(kind: Kind) -> &'static str {
	match kind {
		Kind::Project => "project",
		Kind::Registry => "registry",
	}
}

/// Human-readable bytes (binary units) for the closure/disk reports.
fn human_bytes(bytes: u64) -> String {
	const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
	let mut value = bytes as f64;
	let mut unit = 0;
	while value >= 1024.0 && unit < UNITS.len() - 1 {
		value /= 1024.0;
		unit += 1;
	}
	if unit == 0 {
		format!("{bytes} B")
	} else {
		format!("{value:.1} {}", UNITS[unit])
	}
}

/// The nixpkgs input's `(tracked-ref-or-"(frozen)", rev)` from `flake.lock`.
/// Shared by `info` (summary) and `list` (registry pin column).
fn nixpkgs_pin(project: &Project) -> Option<(String, String)> {
	let node = project.lock.nodes.get("nixpkgs")?;
	let rev = node.locked.as_ref()?.rev()?.to_string();
	let track = node
		.original
		.as_ref()
		.and_then(|o| o.git_ref())
		.map(str::to_string)
		.unwrap_or_else(|| "(frozen)".to_string());
	Some((track, rev))
}

// ---- shell detection + PATH audit (shared by `check` and `context_report`) ---

/// How the current process relates to nix. `nix-shell` sets `IN_NIX_SHELL`;
/// `nix develop` does not, so fall back to a store-provided PATH signal.
enum ShellKind {
	NixShell(String),
	Develop,
	NotNix,
}

fn detect_shell() -> ShellKind {
	if let Ok(v) = std::env::var("IN_NIX_SHELL") {
		return ShellKind::NixShell(v);
	}
	let store_on_path = std::env::var("PATH").is_ok_and(|p| p.contains("/nix/store"));
	if std::env::var_os("NIX_BUILD_TOP").is_some() || store_on_path {
		ShellKind::Develop
	} else {
		ShellKind::NotNix
	}
}

/// The result of splitting `$PATH` into store vs host entries and finding those
/// host entries positioned to shadow a pinned store tool.
#[derive(Debug, PartialEq)]
struct PathAudit {
	store: usize,
	host: usize,
	/// Host dirs appearing before the last store dir — only these can shadow.
	shadowing: Vec<String>,
}

/// Classify `$PATH`: a host dir can shadow a pinned tool only when it appears
/// *before* a store dir, so the shadow set is the host entries left of the last
/// store entry. Pure `&str → PathAudit` (no env read) — unit-testable.
fn audit_path(path: &str) -> PathAudit {
	let entries: Vec<&str> = path.split(':').filter(|e| !e.is_empty()).collect();
	let is_store = |e: &str| e.starts_with("/nix/store");
	let store = entries.iter().filter(|e| is_store(e)).count();
	let host = entries.len() - store;
	let last_store = entries.iter().rposition(|e| is_store(e));
	let shadowing = match last_store {
		Some(last) => entries[..last]
			.iter()
			.filter(|e| !is_store(e))
			.map(|s| s.to_string())
			.collect(),
		None => Vec::new(),
	};
	PathAudit {
		store,
		host,
		shadowing,
	}
}

/// Strip a store path down to its readable `name-version` label: drop the
/// `/nix/store/` dir and the 32-char base32 hash prefix
/// (`/nix/store/<hash>-ripgrep-15.1.0` → `ripgrep-15.1.0`). A non-store or
/// unexpected path degrades to its basename. Pure — unit-tested.
fn store_label(path: &str) -> String {
	let base = path.rsplit('/').next().unwrap_or(path);
	match base.split_once('-') {
		Some((hash, rest))
			if hash.len() == 32 && hash.bytes().all(|b| b.is_ascii_alphanumeric()) =>
		{
			rest.to_string()
		}
		_ => base.to_string(),
	}
}

// ---- top-level `clinix info` (contextual front-end) -------------------------

/// The verb under the contextual top-level `clinix info` (default = summary).
#[derive(Subcommand, Debug)]
pub enum InfoVerb {
	/// Audit the active env's shell/lock/PATH (contextual `env check`).
	Check,
	/// Dependency/closure report for the active env (contextual `env deps`).
	Deps,
}

/// `clinix info [check|deps]` — the contextual entry point.
///
/// Bare `info` describes the **active nix-shell itself, read from the environment
/// nix exports** (`IN_NIX_SHELL`, `system`, `out`, and the provided packages in
/// `buildInputs`/`nativeBuildInputs`) — so it works for *any* nix-shell, whether
/// or not clinix launched it, with no nix call and no `flake.lock`. Outside a
/// shell it falls back to the cwd project. (It reports the shell's *contents*;
/// naming *which clinix registry env / composed stack* you entered is the
/// separate, deferred `CLINIX_ENV_STACK` launch marker.) `info check`/`info deps`
/// reuse the `env` verbs (write-once).
pub fn context_report(verb: Option<InfoVerb>, context: &Context) -> Result<()> {
	match verb {
		None => report_active(context),
		Some(InfoVerb::Check) => check(OptionalTarget { name: None }, context),
		Some(InfoVerb::Deps) => Deps {
			name: None,
			size: false,
			json: false,
		}
		.run(context),
	}
}

/// Summarize the active environment. In a nix-shell: report it from the exported
/// derivation environment ([`report_active_shell`]). Outside one: the cwd project
/// if present, else a clear "neither" note.
fn report_active(context: &Context) -> Result<()> {
	match detect_shell() {
		ShellKind::NotNix => {
			let cwd = std::env::current_dir()?;
			if cwd.join("shell.nix").exists() || cwd.join("flake.lock").exists() {
				println!("not in a nix shell — reporting the cwd project:\n");
				Info {
					name: None,
					json: false,
				}
				.run(context)
			} else {
				println!("not in a nix shell, and no project (shell.nix/flake.lock) in the cwd");
				Ok(())
			}
		}
		kind => {
			report_active_shell(&kind);
			Ok(())
		}
	}
}

/// Report the live nix-shell from the environment nix set for it — no nix call,
/// no `flake.lock`, works for any nix-shell. `$out`'s basename is the reliable
/// derivation label (`$name` can be shadowed by an inherited value); the provided
/// packages come from `buildInputs`/`nativeBuildInputs`.
fn report_active_shell(kind: &ShellKind) {
	match kind {
		ShellKind::NixShell(v) => println!("active: in a nix-shell (IN_NIX_SHELL={v})"),
		ShellKind::Develop => println!("active: in a nix develop / store-provided shell"),
		ShellKind::NotNix => return,
	}
	if let Ok(out) = std::env::var("out") {
		println!("  derivation  {}", store_label(&out));
	}
	println!(
		"  system      {}",
		std::env::var("system").unwrap_or_else(|_| "<unset>".into())
	);

	let pkgs = provided_packages();
	if pkgs.is_empty() {
		println!("  packages    (none exported in buildInputs/nativeBuildInputs)");
	} else {
		println!("  packages ({}):", pkgs.len());
		for p in &pkgs {
			println!("    {p}");
		}
	}

	let audit = audit_path(&std::env::var("PATH").unwrap_or_default());
	let shadow = if audit.shadowing.is_empty() {
		String::new()
	} else {
		format!(
			" — {} host dir(s) shadow pinned tools (clinix info check)",
			audit.shadowing.len()
		)
	};
	println!(
		"  PATH        {} store / {} host entries{shadow}",
		audit.store, audit.host
	);
}

/// The packages the active nix-shell provides, from the derivation env vars nix
/// exports (`buildInputs` + `nativeBuildInputs` + `propagatedBuildInputs`) —
/// whitespace-separated store paths. Labeled by [`store_label`], order preserved,
/// de-duplicated. No nix call; works for any nix-shell.
fn provided_packages() -> Vec<String> {
	let mut seen = BTreeSet::new();
	let mut out = Vec::new();
	for var in ["buildInputs", "nativeBuildInputs", "propagatedBuildInputs"] {
		let Ok(val) = std::env::var(var) else {
			continue;
		};
		for path in val.split_whitespace() {
			let label = store_label(path);
			if seen.insert(label.clone()) {
				out.push(label);
			}
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	// ---- PATH-shadow audit (envcheck core) ----

	#[test]
	fn path_audit_flags_only_host_dirs_before_a_store_dir() {
		// /usr/bin sits before a store dir → it can shadow a pinned tool.
		let a = audit_path("/usr/bin:/nix/store/abc-ripgrep/bin:/home/u/.local/bin");
		assert_eq!(a.store, 1);
		assert_eq!(a.host, 2);
		assert_eq!(a.shadowing, vec!["/usr/bin".to_string()]); // trailing host dir is safe
	}

	#[test]
	fn path_audit_no_store_means_not_in_a_shell() {
		let a = audit_path("/usr/bin:/bin");
		assert_eq!(a.store, 0);
		assert!(a.shadowing.is_empty()); // nothing to shadow
	}

	#[test]
	fn path_audit_all_host_after_store_cannot_shadow() {
		let a = audit_path("/nix/store/x/bin:/nix/store/y/bin:/usr/bin");
		assert_eq!((a.store, a.host), (2, 1));
		assert!(a.shadowing.is_empty());
	}

	// ---- store-path label (reading the active nix-shell's contents) ----

	#[test]
	fn store_label_strips_hash_prefix_to_name_version() {
		// 32-char base32 hash + `-` + name-version (as `$buildInputs` exports).
		let hash = "0123456789abcdfghijklmnpqrsvwxyz"; // 32 chars (nix base32 alphabet)
		assert_eq!(hash.len(), 32);
		assert_eq!(
			store_label(&format!("/nix/store/{hash}-ripgrep-15.1.0")),
			"ripgrep-15.1.0"
		);
		// An output suffix is part of the label.
		assert_eq!(
			store_label(&format!("/nix/store/{hash}-jq-1.8.2-dev")),
			"jq-1.8.2-dev"
		);
		// Non-store paths degrade to the basename.
		assert_eq!(store_label("/usr/bin/rg"), "rg");
	}
}
