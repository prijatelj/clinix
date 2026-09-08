//! L3 — CLI tests that need no nix: grammar, `--help`, argument validation, and
//! error/exit paths. Runs on every `cargo test`. See
//! `notes/clinix/design/testing.md` §7 for the catalog these port.

mod common;

use common::Project;
use predicates::prelude::*;

// ---- help / grammar ---------------------------------------------------------

#[test]
fn bare_clinix_prints_help_not_the_cwd_project() {
	// `clinix` with no args shows help (does not silently enter `.`).
	Project::new()
		.clinix(&[])
		.assert()
		.success()
		.stdout(predicate::str::contains("Usage").and(predicate::str::contains("clinix")));
}

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

// ---- config: template generation & path -------------------------------------

#[test]
fn config_example_prints_a_fillable_template() {
	// Minimal by default; --full is the extensive annotated form.
	Project::new()
		.clinix(&["config", "example"])
		.assert()
		.success()
		.stdout(predicate::str::contains("[env.seeds]"));
	Project::new()
		.clinix(&["config", "example", "--full"])
		.assert()
		.success()
		.stdout(predicate::str::contains("shorthand").and(predicate::str::contains("nixpkgs")));
}

#[test]
fn config_path_prints_the_resolved_config_file() {
	Project::new()
		.clinix(&["config", "path"])
		.assert()
		.success()
		.stdout(predicate::str::contains("clinix").and(predicate::str::contains("config.toml")));
}

#[test]
fn config_state_lists_the_state_locations() {
	Project::new()
		.clinix(&["config", "state"])
		.assert()
		.success()
		.stdout(
			predicate::str::contains("envs")
				.and(predicate::str::contains("roots"))
				.and(predicate::str::contains("compose")),
		);
}

// ---- completions & man (generated from the clap definition) -----------------

#[test]
fn completions_prints_a_shell_script() {
	Project::new()
		.clinix(&["completions", "bash"])
		.assert()
		.success()
		.stdout(predicate::str::contains("clinix"));
	// an unknown shell is rejected by clap's value parser.
	Project::new()
		.clinix(&["completions", "notashell"])
		.assert()
		.failure();
}

#[test]
fn man_prints_roff_to_stdout_and_writes_to_a_dir() {
	// stdout form: a roff man page (`.TH` header + the tool's about line).
	Project::new()
		.clinix(&["man"])
		.assert()
		.success()
		.stdout(predicate::str::contains(".TH").and(predicate::str::contains("Manage Nix")));
	// directory form: writes clinix.1 for install onto MANPATH.
	let p = Project::new();
	p.clinix(&["man", p.path().to_str().unwrap()])
		.assert()
		.success();
	assert!(p.file("clinix.1").exists(), "clinix.1 written to the dir");
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
fn run_with_an_unknown_name_in_a_stack_errors() {
	// Multi-name stacks now compose (seeds); an unknown name is a clear error.
	Project::new()
		.clinix(&["env", "run", ".", "other", "--", "true"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("not registered"));
}

#[test]
fn env_bare_name_routes_to_shell() {
	// `clinix env <name>` (no verb) routes to the launcher via the env-level
	// external_subcommand, so an unknown name is an env error — not a clap
	// "unrecognized subcommand".
	Project::new()
		.clinix(&["env", "ghost"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("not registered"));
}

#[test]
fn new_from_materializes_seeds_into_a_portable_registry_env() {
	let p = Project::new();
	p.seed("tool", "{ pkgs }: pkgs.mkShell { }\n");
	// Pre-create the config nixpkgs lock so `new` needs no network.
	std::fs::create_dir_all(p.config_dir()).unwrap();
	std::fs::write(p.config_dir().join("flake.lock"), common::MINIMAL_LOCK).unwrap();

	p.clinix(&["env", "new", "mytools", "--from", "tool"])
		.assert()
		.success()
		.stdout(predicate::str::contains("created registry env"));

	// Self-contained + portable: local seed copy + local lock, no source paths.
	let env = p.env_dir("mytools");
	assert!(env.join("seeds/tool.nix").exists(), "seed copied in");
	assert!(env.join("flake.lock").exists(), "pin lock copied in");
	let shell = std::fs::read_to_string(env.join("shell.nix")).unwrap();
	assert!(shell.contains("import ./seeds/tool.nix"), "unions the local copy");
	assert!(shell.contains("builtins.readFile ./flake.lock"), "reads the local lock");
}

#[test]
fn new_from_a_non_seed_name_is_rejected() {
	let p = Project::new();
	std::fs::create_dir_all(p.config_dir()).unwrap();
	std::fs::write(p.config_dir().join("flake.lock"), common::MINIMAL_LOCK).unwrap();
	// No seeds configured → "ghost" resolves to nothing.
	p.clinix(&["env", "new", "x", "--from", "ghost"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("is not one"));
}

// ---- new: register a single shell (dir/file source) — no nix needed ----------

/// A clinix-style shell that reads its own `./flake.lock` (so lock detection fires).
const SHELL_READS_LOCK: &str =
	"{ sources ? (builtins.fromJSON (builtins.readFile ./flake.lock)), pkgs ? import <nixpkgs> {} }: pkgs.mkShell { }\n";

/// Write a source project dir (`<name>/shell.nix` + `flake.lock`) under the cwd.
fn write_source_project(p: &Project, name: &str, lock: &str) {
	let dir = p.file(name);
	std::fs::create_dir_all(&dir).unwrap();
	std::fs::write(dir.join("shell.nix"), SHELL_READS_LOCK).unwrap();
	std::fs::write(dir.join("flake.lock"), lock).unwrap();
}

#[test]
fn new_register_non_copy_references_source_and_establishes_pin() {
	let p = Project::new();
	write_source_project(&p, "proj", common::MINIMAL_LOCK);
	let abs = std::fs::canonicalize(p.file("proj/shell.nix")).unwrap();
	p.clinix(&["env", "new", "myenv", "--from", "proj"])
		.assert()
		.success()
		.stdout(predicate::str::contains("referenced"));
	// Non-copy: the wrapper imports the source's absolute path and injects pkgs.
	let wrapper = std::fs::read_to_string(p.env_dir("myenv").join("shell.nix")).unwrap();
	assert!(wrapper.contains(&format!("import \"{}\" {{ inherit pkgs; }}", abs.display())));
	assert!(wrapper.contains("builtins.readFile ./flake.lock"));
	// The source's lock is established as the namespace pin.
	assert!(p.env_dir("myenv").join("flake.lock").is_file());
	assert!(!p.env_dir("myenv").join("source.nix").exists(), "non-copy: no body copy");
}

#[test]
fn new_register_copy_copies_the_shell_body() {
	let p = Project::new();
	write_source_project(&p, "proj", common::MINIMAL_LOCK);
	p.clinix(&["env", "new", "myenv", "--from", "proj", "--copy"])
		.assert()
		.success()
		.stdout(predicate::str::contains("copied"));
	assert!(p.env_dir("myenv").join("source.nix").is_file(), "copy: body is copied in");
	let wrapper = std::fs::read_to_string(p.env_dir("myenv").join("shell.nix")).unwrap();
	assert!(wrapper.contains("import ./source.nix { inherit pkgs; }"));
}

#[test]
fn new_register_member_creates_empty_namespace_and_shares_the_pin() {
	let p = Project::new();
	std::fs::write(p.file("dev.nix"), SHELL_READS_LOCK).unwrap();
	std::fs::write(p.file("flake.lock"), common::MINIMAL_LOCK).unwrap();
	p.clinix(&["env", "new", "proj:dev", "--from", "./dev.nix"])
		.assert()
		.success()
		.stderr(predicate::str::contains("created empty namespace"));
	// Member lives in a subdir and reads the SHARED pin at ../flake.lock.
	let member = p.env_dir("proj").join("dev").join("shell.nix");
	assert!(member.is_file());
	assert!(
		std::fs::read_to_string(&member).unwrap().contains("builtins.readFile ../flake.lock")
	);
	assert!(p.env_dir("proj").join("flake.lock").is_file(), "shared pin at the namespace root");
}

#[test]
fn new_register_unpinned_source_warns() {
	let p = Project::new();
	// A shell that reads no flake.lock.
	std::fs::write(p.file("bare.nix"), "{ pkgs }: pkgs.mkShell { }\n").unwrap();
	p.clinix(&["env", "new", "myenv", "--from", "./bare.nix"])
		.assert()
		.success()
		.stderr(predicate::str::contains("unpinned"));
	let wrapper = std::fs::read_to_string(p.env_dir("myenv").join("shell.nix")).unwrap();
	assert!(wrapper.contains("import ") && wrapper.contains("{ }"), "imported as-is");
	assert!(!p.env_dir("myenv").join("flake.lock").exists(), "no pin established");
}

#[test]
fn new_register_lock_conflict_errors_then_overwrite_repins() {
	let p = Project::new();
	// A second, distinct lock (different rev) for the conflict.
	let other_lock = common::MINIMAL_LOCK.replace("abc1234567890def", "fff0000000000000");
	write_source_project(&p, "a", common::MINIMAL_LOCK);
	write_source_project(&p, "b", &other_lock);
	// First member establishes the namespace pin.
	p.clinix(&["env", "new", "proj:one", "--from", "a"]).assert().success();
	// Second member with a different lock → conflict error (no flag).
	p.clinix(&["env", "new", "proj:two", "--from", "b"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("differs from namespace").and(predicate::str::contains("--overwrite")));
	// --overwrite repins the namespace to b's lock.
	p.clinix(&["env", "new", "proj:two", "--from", "b", "--overwrite"]).assert().success();
	let pin = std::fs::read_to_string(p.env_dir("proj").join("flake.lock")).unwrap();
	assert!(pin.contains("fff0000000000000"), "namespace repinned to b's lock");
}

#[test]
fn new_warns_when_the_name_shadows_a_seed() {
	let p = Project::new();
	p.seed("rust", "{ pkgs }: pkgs.mkShell { }\n"); // a seed named `rust`
	write_source_project(&p, "proj", common::MINIMAL_LOCK);
	// Registering an env `rust` shadows the seed in resolution → warn (not error).
	p.clinix(&["env", "new", "rust", "--from", "proj"])
		.assert()
		.success()
		.stderr(predicate::str::contains("shadows seed `rust`"));
}

#[test]
fn run_mixing_a_project_env_into_a_seed_stack_is_deferred() {
	// A stack must be all seeds for now (self-contained composition deferred).
	// `.` (cwd project) + a seed hits that boundary before any nix call.
	let p = Project::new();
	p.seed("dev", "{ pkgs }: pkgs.mkShell { }\n");
	p.clinix(&["env", "run", ".", "dev", "--", "true"])
		.assert()
		.failure()
		.stderr(predicate::str::contains(
			"composing a self-contained env/file into a union",
		));
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
fn shell_file_target_missing_errors_clearly() {
	// A `*.nix` target that does not exist is a File-shape error, not a
	// fall-through to a registry/seed lookup (stops before any nix call).
	Project::new()
		.clinix(&["env", "shell", "nope.nix"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("no such shell file: nope.nix"));
}

#[test]
fn member_target_resolves_to_the_member_subdir() {
	// `namespace:member` resolves to `envs/<namespace>/<member>`. With the member
	// dir present but no shell file, the launcher errors *at that path* — proving
	// resolution reached the member subdir, all without invoking nix.
	let p = Project::new();
	std::fs::create_dir_all(p.env_dir("proj").join("dev")).unwrap();
	p.clinix(&["env", "shell", "proj:dev"])
		.assert()
		.failure()
		.stderr(
			predicate::str::contains("no shell.nix or default.nix")
				.and(predicate::str::contains("envs/proj/dev")),
		);
}

#[test]
fn member_target_unknown_reports_unknown_env() {
	// No such registry member and no such seed → unknown env (nix-free).
	Project::new()
		.clinix(&["env", "shell", "ghost:dev"])
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
fn list_includes_seeds_from_the_catalog() {
	// `env list` shows both registry envs and in-place seeds.
	let p = Project::new();
	p.seed("codex", "{ pkgs }: pkgs.mkShell { }\n");
	p.seed("pi", "{ pkgs }: pkgs.mkShell { }\n");
	p.clinix(&["env", "list"])
		.assert()
		.success()
		.stdout(
			predicate::str::contains("seeds:")
				.and(predicate::str::contains("codex"))
				.and(predicate::str::contains("pi")),
		);
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

// ---- check / info (contextual reporting; offline PATH+lock audit) -----------

#[test]
fn check_on_a_scaffold_reports_shell_lock_and_path_sections() {
	let p = Project::new();
	std::fs::write(p.file("shell.nix"), "pkgs.mkShell { packages = with pkgs; [ ]; }\n").unwrap();
	std::fs::write(p.file("flake.lock"), FLAKE_LOCK).unwrap();
	p.clinix(&["env", "check"])
		.assert()
		.success()
		.stdout(predicate::str::contains("== shell"))
		.stdout(predicate::str::contains("== pinned sources"))
		.stdout(predicate::str::contains("nixos-26.05")) // tracking from the lock
		.stdout(predicate::str::contains("== PATH"));
}

#[test]
fn check_without_a_lock_still_audits_path() {
	let p = Project::new();
	std::fs::write(p.file("shell.nix"), "pkgs.mkShell { }\n").unwrap();
	p.clinix(&["env", "check"])
		.assert()
		.success()
		.stdout(predicate::str::contains("no readable flake.lock"))
		.stdout(predicate::str::contains("== PATH"));
}

#[test]
fn info_summary_reports_the_active_shell_from_the_environment() {
	// Bare `clinix info` describes the active nix-shell it is run inside, read
	// from the exported derivation env — independent of clinix. Every branch
	// (nix-shell / develop / neither) mentions "shell".
	let p = Project::new();
	p.clinix(&["info"])
		.env("IN_NIX_SHELL", "impure")
		.env(
			"buildInputs",
			"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-ripgrep-15.1.0",
		)
		.assert()
		.success()
		.stdout(predicate::str::contains("active: in a nix-shell"))
		.stdout(predicate::str::contains("ripgrep-15.1.0")); // provided pkg from $buildInputs
}

#[test]
fn info_summary_outside_a_shell_falls_back_to_the_cwd_project() {
	// No IN_NIX_SHELL and no store on PATH → not in a shell; with no project
	// files present, say so plainly.
	let p = Project::new();
	p.clinix(&["info"])
		.env_remove("IN_NIX_SHELL")
		.env_remove("NIX_BUILD_TOP")
		.env("PATH", "/usr/bin:/bin")
		.assert()
		.success()
		.stdout(predicate::str::contains("not in a nix shell"));
}

#[test]
fn info_check_delegates_to_env_check() {
	// `clinix info check` == `env check` on the active (cwd) env.
	let p = Project::new();
	std::fs::write(p.file("shell.nix"), "pkgs.mkShell { }\n").unwrap();
	p.clinix(&["info", "check"])
		.assert()
		.success()
		.stdout(predicate::str::contains("== shell"))
		.stdout(predicate::str::contains("== PATH"));
}

#[test]
fn shared_with_unknown_env_reports_not_registered() {
	Project::new()
		.clinix(&["env", "shared", "ghost", "phantom"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("not registered"));
}

// ---- export / import closure (offline error paths) --------------------------

#[test]
fn export_closure_without_a_shell_nix_errors() {
	let p = Project::new();
	p.clinix(&["env", "export", "closure", "."])
		.assert()
		.failure()
		.stderr(predicate::str::contains("no shell.nix"));
}

#[test]
fn export_closure_requires_at_least_one_name() {
	// Target-first grammar: names are required (clap error, before any nix).
	Project::new()
		.clinix(&["env", "export", "closure"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("required").or(predicate::str::contains("<NAMES>")));
}

#[test]
fn import_to_an_existing_registry_name_is_refused() {
	// The name-collision check runs before any file/nix work, so this stays
	// nix-free (the closure/shell.nix paths need not even exist).
	let p = Project::new();
	p.seed_registry_env("taken", "pkgs.mkShell { }\n");
	p.clinix(&[
		"env",
		"import",
		"taken",
		"whatever.closure",
		"--shell-nix",
		"whatever/shell.nix",
	])
	.assert()
	.failure()
	.stderr(predicate::str::contains("already exists"));
}

#[test]
fn import_with_a_missing_closure_errors() {
	// Name is free → the next pre-check (closure archive exists) fires, pre-nix.
	let p = Project::new();
	p.clinix(&[
		"env",
		"import",
		"restored",
		"nope.closure",
		"--shell-nix",
		"whatever/shell.nix",
	])
	.assert()
	.failure()
	.stderr(predicate::str::contains("closure archive not found"));
}

#[test]
fn import_with_a_missing_shell_nix_errors() {
	// A real closure file passes its check; the missing shell.nix is next, pre-nix.
	let p = Project::new();
	std::fs::write(p.file("env.closure"), b"not really a closure").unwrap();
	p.clinix(&[
		"env",
		"import",
		"restored",
		p.file("env.closure").to_str().unwrap(),
		"--shell-nix",
		"absent/shell.nix",
	])
	.assert()
	.failure()
	.stderr(predicate::str::contains("shell.nix not found"));
}
