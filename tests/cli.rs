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

// ---- pin / unpin (offline flake.lock freeze/unfreeze) ------------------------

const FLAKE_LOCK: &str = r#"{
  "nodes": {
    "nixpkgs": {
      "locked": { "narHash": "sha256-x", "owner": "NixOS", "repo": "nixpkgs", "rev": "abc123", "type": "github" },
      "original": { "owner": "NixOS", "ref": "nixos-26.05", "repo": "nixpkgs", "type": "github" }
    },
    "root": { "inputs": { "nixpkgs": "nixpkgs" } }
  },
  "root": "root",
  "version": 7
}
"#;

#[test]
fn pin_all_freezes_to_a_standard_rev() {
	let p = Project::new();
	std::fs::write(p.file("flake.lock"), FLAKE_LOCK).unwrap();
	p.clinix(&["env", "pin", ".", "--all"])
		.assert()
		.success()
		.stdout(predicate::str::contains("frozen"));
	let lock = std::fs::read_to_string(p.file("flake.lock")).unwrap();
	// original is pinned to the rev and the branch ref is dropped (nix-standard).
	assert!(lock.contains("\"rev\": \"abc123\""));
	assert!(!lock.contains("nixos-26.05"), "{lock}");
}

#[test]
fn unpin_all_without_branch_is_a_clear_error() {
	Project::new()
		.clinix(&["env", "unpin", ".", "--all"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("--branch"));
}

#[test]
fn pin_requires_all_and_defers_per_package() {
	Project::new()
		.clinix(&["env", "pin", "."]) // neither --all nor packages
		.assert()
		.failure()
		.stderr(predicate::str::contains("--all"));
	Project::new()
		.clinix(&["env", "pin", ".", "ripgrep"]) // per-package
		.assert()
		.failure()
		.stderr(predicate::str::contains("phase 4"));
}

// ---- registry: list / rename / clean (offline filesystem ops) ---------------

const REG_SHELL_NIX: &str = "pkgs.mkShell { packages = with pkgs; [ ripgrep ]; }\n";

#[test]
fn list_on_empty_registry_hints_instead_of_erroring() {
	Project::new()
		.clinix(&["env", "list"])
		.assert()
		.success()
		.stdout(predicate::str::contains("no registered envs"));
}

#[test]
fn list_enumerates_registry_envs_sorted() {
	let p = Project::new();
	p.seed_registry_env("rust", REG_SHELL_NIX);
	p.seed_registry_env("python", REG_SHELL_NIX);
	p.clinix(&["env", "list"])
		.assert()
		.success()
		// Both names appear; nixpkgs pin is read from each flake.lock.
		.stdout(predicate::str::contains("python").and(predicate::str::contains("rust")))
		.stdout(predicate::str::contains("nixos-26.05"));
}

#[test]
fn rename_moves_the_env_dir_and_its_gc_root() {
	let p = Project::new();
	p.seed_registry_env("old", REG_SHELL_NIX);
	// Simulate an entered env: a name-keyed GC root exists.
	std::fs::create_dir_all(p.state().join("roots")).unwrap();
	std::fs::write(p.env_root_link("old"), "drv").unwrap();

	p.clinix(&["env", "rename", "old", "new"])
		.assert()
		.success()
		.stdout(predicate::str::contains("renamed"));

	assert!(!p.env_dir("old").exists(), "old env dir gone");
	assert!(p.env_dir("new").join("shell.nix").exists(), "new env dir present");
	// The rename-correctness invariant: the root followed the name.
	assert!(!p.env_root_link("old").exists(), "old root gone");
	assert!(p.env_root_link("new").exists(), "root re-keyed to new name");
}

#[test]
fn rename_to_existing_name_fails_and_leaves_source() {
	let p = Project::new();
	p.seed_registry_env("a", REG_SHELL_NIX);
	p.seed_registry_env("b", REG_SHELL_NIX);
	p.clinix(&["env", "rename", "a", "b"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("already exists"));
	assert!(p.env_dir("a").exists(), "source untouched on failure");
}

#[test]
fn rename_unknown_env_reports_not_registered() {
	Project::new()
		.clinix(&["env", "rename", "ghost", "new"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("not registered"));
}

#[test]
fn rename_to_invalid_name_is_rejected() {
	let p = Project::new();
	p.seed_registry_env("old", REG_SHELL_NIX);
	p.clinix(&["env", "rename", "old", "a/b"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("invalid environment name"));
}

#[test]
fn clean_removes_the_gc_root_and_is_idempotent() {
	let p = Project::new();
	p.seed_registry_env("web", REG_SHELL_NIX);
	std::fs::create_dir_all(p.state().join("roots")).unwrap();
	std::fs::write(p.env_root_link("web"), "drv").unwrap();

	p.clinix(&["env", "clean", "web"])
		.assert()
		.success()
		.stdout(predicate::str::contains("released GC root"));
	assert!(!p.env_root_link("web").exists(), "root removed");

	// A second clean is a no-op success, not an error.
	p.clinix(&["env", "clean", "web"])
		.assert()
		.success()
		.stdout(predicate::str::contains("no GC root"));
}
