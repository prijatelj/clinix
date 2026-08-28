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
