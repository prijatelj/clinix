//! L4 — end-to-end tests against **real nix** (init → `nix-shell` → info/deps/add),
//! writing down the manual verifications done during development so they become
//! permanent regression guards. See `notes/clinix/design/testing.md`.
//!
//! Gated: every test is `#[ignore]` *and* `have_nix()`-guarded, so `cargo test`
//! stays fast and nix-free; run these with `cargo test -- --ignored`
//! (`just test-e2e`). They need network (first run) + nix. Assertions check
//! **presence/shape**, never volatile versions (nixpkgs branches drift).

mod common;

use common::{Project, have_nix, nix_shell_run};
use predicates::prelude::*;

#[test]
#[ignore = "needs nix + network"]
fn init_default_scaffolds_and_runs_under_nix_shell() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", ".", "-p", "ripgrep"])
		.assert()
		.success();
	assert!(p.file("shell.nix").exists() && p.file("flake.lock").exists());
	assert!(!p.file("flake.nix").exists());

	let out = nix_shell_run(&p.file("shell.nix"), "rg --version");
	assert!(out.status.success());
	assert!(String::from_utf8_lossy(&out.stdout).contains("ripgrep"));
}

#[test]
#[ignore = "needs nix + network"]
fn init_single_file_is_self_contained_and_runs() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", ".", "--single-file", "-p", "ripgrep"])
		.assert()
		.success();
	assert!(p.file("shell.nix").exists());
	assert!(!p.file("flake.lock").exists(), "single-file: no flake.lock");

	let out = nix_shell_run(&p.file("shell.nix"), "rg --version");
	assert!(out.status.success());
	assert!(String::from_utf8_lossy(&out.stdout).contains("ripgrep"));
}

#[test]
#[ignore = "needs nix + network"]
fn init_flake_wraps_shell_nix() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", ".", "--flake"]).assert().success();
	let flake = std::fs::read_to_string(p.file("flake.nix")).unwrap();
	assert!(flake.contains("import ./shell.nix"));
}

#[test]
#[ignore = "needs nix + network"]
fn info_reports_pin_and_resolved_versions() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", ".", "-p", "ripgrep"])
		.assert()
		.success();
	p.clinix(&["env", "info", "."])
		.assert()
		.success()
		.stdout(predicate::str::contains("nixpkgs:").and(predicate::str::contains("ripgrep")));
	// --json lists the package (with some version) in a `packages` map.
	p.clinix(&["env", "info", ".", "--json"])
		.assert()
		.success()
		.stdout(predicate::str::contains("\"packages\"").and(predicate::str::contains("ripgrep")));
}

#[test]
#[ignore = "needs nix + network"]
fn deps_reports_derivation_closure_and_packages() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", ".", "-p", "ripgrep"])
		.assert()
		.success();
	p.clinix(&["env", "deps", "."]).assert().success().stdout(
		predicate::str::contains("derivation:")
			.and(predicate::str::contains("closure:"))
			.and(predicate::str::contains("ripgrep")),
	);
}

#[test]
#[ignore = "needs nix + network"]
fn add_edits_shell_nix_and_it_still_runs() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", ".", "-p", "ripgrep"])
		.assert()
		.success();
	p.clinix(&["env", "add", ".", "jq"]).assert().success();
	// The rnix-spliced shell.nix is still valid and the new package resolves.
	let out = nix_shell_run(&p.file("shell.nix"), "jq --version");
	assert!(out.status.success());
	assert!(String::from_utf8_lossy(&out.stdout).contains("jq"));
}
