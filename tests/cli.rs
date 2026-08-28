//! L3 — CLI tests that need no nix: grammar, `--help`, argument validation, and
//! error/exit paths. Runs on every `cargo test`. See
//! `notes/clinix/design/testing.md` §7 for the catalog these port.

mod common;

use common::Project;
use predicates::prelude::*;

// ---- help / grammar ---------------------------------------------------------

#[test]
fn help_and_subcommands_succeed() {
	Project::new().clinix(&["--help"]).assert().success();
	Project::new().clinix(&["env", "--help"]).assert().success();
	Project::new()
		.clinix(&["env", "init", "--help"])
		.assert()
		.success()
		.stdout(predicate::str::contains("[PATH]"));
}

// ---- init: argument validation & deferrals ----------------------------------

#[test]
fn init_single_file_and_flake_are_mutually_exclusive() {
	Project::new()
		.clinix(&["env", "init", "--single-file", "--flake"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn init_from_adopt_is_deferred() {
	Project::new()
		.clinix(&["env", "init", "--from", "pyproject.toml"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("env init --from"));
}

#[test]
fn init_versioned_package_is_deferred() {
	Project::new()
		.clinix(&["env", "init", "-p", "ripgrep=1.2.3"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("package-level version pinning"));
}

#[test]
fn init_refuses_to_clobber_existing_shell_nix() {
	let p = Project::new();
	std::fs::write(p.file("shell.nix"), "hand-written").unwrap();
	p.clinix(&["env", "init"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("already exists"));
	// The pre-existing file is left untouched, and nothing else was written.
	assert_eq!(
		std::fs::read_to_string(p.file("shell.nix")).unwrap(),
		"hand-written"
	);
	assert!(!p.file("flake.lock").exists());
}

// ---- update: deferrals & the single-file guard ------------------------------

#[test]
fn update_per_package_is_deferred() {
	Project::new()
		.clinix(&["env", "update", ".", "ripgrep"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("per-package"));
}

#[test]
fn update_on_single_file_env_is_guarded_and_writes_no_lock() {
	let p = Project::new();
	// A single-file env: shell.nix present, no flake.lock.
	std::fs::write(p.file("shell.nix"), "# lock embedded here").unwrap();
	p.clinix(&["env", "update", "."])
		.assert()
		.failure()
		.stderr(predicate::str::contains("single-file"));
	// The guard must not create a second, drifting flake.lock.
	assert!(!p.file("flake.lock").exists());
}

// ---- launcher & diagnostics: error paths ------------------------------------

#[test]
fn run_with_multiple_envs_is_deferred() {
	Project::new()
		.clinix(&["env", "run", ".", "other", "--", "true"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("multiple envs"));
}

#[test]
fn shell_unknown_env_reports_not_registered() {
	Project::new()
		.clinix(&["env", "shell", "definitely-not-an-env"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("not registered"));
}

#[test]
fn deps_without_a_shell_nix_errors_clearly() {
	Project::new()
		.clinix(&["env", "deps", "."])
		.assert()
		.failure()
		.stderr(predicate::str::contains("no shell.nix"));
}

// ---- add / remove (offline `shell.nix` edits — no nix needed) ----------------

const SHELL_NIX: &str = "\
pkgs.mkShell {
  packages = with pkgs; [
    ripgrep
  ];
}
";

#[test]
fn add_appends_and_is_idempotent() {
	let p = Project::new();
	std::fs::write(p.file("shell.nix"), SHELL_NIX).unwrap();
	p.clinix(&["env", "add", ".", "jq", "nodejs"])
		.assert()
		.success()
		.stdout(predicate::str::contains("added:").and(predicate::str::contains("nodejs")));
	let shell = std::fs::read_to_string(p.file("shell.nix")).unwrap();
	assert!(shell.contains("ripgrep") && shell.contains("jq") && shell.contains("nodejs"));

	p.clinix(&["env", "add", ".", "jq"])
		.assert()
		.success()
		.stdout(predicate::str::contains("already present"));
}

#[test]
fn remove_deletes_and_reports_absent() {
	let p = Project::new();
	std::fs::write(p.file("shell.nix"), SHELL_NIX).unwrap();
	p.clinix(&["env", "remove", ".", "ripgrep", "bogus"])
		.assert()
		.success()
		.stdout(predicate::str::contains("removed:").and(predicate::str::contains("not present")));
	assert!(
		!std::fs::read_to_string(p.file("shell.nix"))
			.unwrap()
			.contains("ripgrep")
	);
}

#[test]
fn add_versioned_package_is_rejected() {
	let p = Project::new();
	std::fs::write(p.file("shell.nix"), SHELL_NIX).unwrap();
	p.clinix(&["env", "add", ".", "ripgrep=1.2.3"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("versioned package"));
}

#[test]
fn add_without_a_shell_nix_errors() {
	Project::new()
		.clinix(&["env", "add", ".", "jq"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("no shell.nix"));
}

#[test]
fn add_with_sort_orders_the_list() {
	let p = Project::new();
	std::fs::write(p.file("shell.nix"), SHELL_NIX).unwrap(); // has ripgrep
	p.clinix(&["env", "add", ".", "jq", "--sort"])
		.assert()
		.success();
	// jq sorts before ripgrep.
	let shell = std::fs::read_to_string(p.file("shell.nix")).unwrap();
	assert!(shell.contains("    jq\n    ripgrep\n  ];"), "{shell}");
}
