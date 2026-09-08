//! L4 — end-to-end tests against **real nix** (init → `nix-shell` → info/deps/add),
//! writing down the manual verifications done during development so they become
//! permanent regression guards. See `notes/clinix/design/testing.md`.
//!
//! Gated: every test is `#[ignore]` *and* `have_nix()`-guarded, so `cargo test`
//! stays fast and nix-free; run these with `cargo test -- --ignored`
//! (`just test-e2e`). They need network (first run) + nix. Assertions check
//! **presence/shape**, never volatile versions (nixpkgs branches drift).

mod common;

use common::{
	ChrootStore, Project, can_import_store, have_nix, missing_in_store, nix_shell_run, runtime_paths,
};
use predicates::prelude::*;

#[test]
#[ignore = "needs nix + network"]
fn init_default_scaffolds_and_runs_under_nix_shell() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	// Default: numbered progress on stderr (resolve → hash → write), result on
	// stdout. A tracked ref → 3 steps.
	p.clinix(&["env", "init", ".", "-p", "ripgrep"])
		.assert()
		.success()
		.stderr(
			predicate::str::contains("[1/3] resolving nixpkgs/nixos-26.05")
				.and(predicate::str::contains("[3/3] writing project files")),
		)
		.stdout(predicate::str::contains("initialized project env"));
	assert!(p.file("shell.nix").exists() && p.file("flake.lock").exists());
	assert!(!p.file("flake.nix").exists());
	// The generated shell.nix documents the real launcher command, not `shell`.
	let shell = std::fs::read_to_string(p.file("shell.nix")).unwrap();
	assert!(shell.contains("clinix env shell  the launcher"));

	let out = nix_shell_run(&p.file("shell.nix"), "rg --version");
	assert!(out.status.success());
	assert!(String::from_utf8_lossy(&out.stdout).contains("ripgrep"));
}

#[test]
#[ignore = "needs nix + network"]
fn init_quiet_suppresses_progress() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	// `-q`: no progress on stderr; the stdout result summary is unaffected.
	p.clinix(&["env", "init", ".", "--quiet"])
		.assert()
		.success()
		.stderr(predicate::str::contains("[1/").not())
		.stdout(predicate::str::contains("initialized project env"));
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

#[test]
#[ignore = "needs nix + network"]
fn shared_compares_two_envs_closures() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	// Two project envs seeded with the same package → high sharing expected.
	p.clinix(&["env", "init", "./a", "-p", "ripgrep"])
		.assert()
		.success();
	p.clinix(&["env", "init", "./b", "-p", "ripgrep"])
		.assert()
		.success();
	p.clinix(&["env", "shared", "./a", "./b"])
		.assert()
		.success()
		.stdout(predicate::str::contains("== counts"))
		.stdout(predicate::str::contains("shared (all)"))
		.stdout(predicate::str::contains("== disk"));
}

/// Export writes a single `.closure` archive (nothing else — bundling is the
/// user's job). Always runnable: exporting reads the store, no write trust needed.
#[test]
#[ignore = "needs nix + network"]
fn closure_export_writes_a_nonempty_archive() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", ".", "-p", "ripgrep"])
		.assert()
		.success();
	// Realize the env so export has already-built store paths.
	p.clinix(&["env", "run", ".", "--", "true"])
		.assert()
		.success();

	let archive = p.path().join("env.closure");
	p.clinix(&["env", "export", "closure", "--out", archive.to_str().unwrap(), "."])
		.assert()
		.success()
		.stdout(predicate::str::contains("store paths"));
	assert!(archive.is_file(), "the .closure archive exists");
	assert!(
		archive.metadata().unwrap().len() > 0,
		"the .closure archive is non-empty"
	);
	// Export writes ONLY the archive — no shell.nix copy, no manifest.
	assert!(!p.path().join("clinix-closure.json").exists());
}

/// The full round-trip: export → import (store + register from the user-carried
/// shell.nix/flake.lock) → run the restored env. Guarded by `can_import_store()`
/// because a multi-user store refuses unsigned paths from a non-root user.
#[test]
#[ignore = "needs nix + network + importable store"]
fn closure_import_round_trips_into_the_registry() {
	if !have_nix() || !can_import_store() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", ".", "-p", "ripgrep"])
		.assert()
		.success();
	p.clinix(&["env", "run", ".", "--", "true"])
		.assert()
		.success();
	let archive = p.path().join("env.closure");
	p.clinix(&["env", "export", "closure", "--out", archive.to_str().unwrap(), "."])
		.assert()
		.success();

	// Import carries over the env's own shell.nix + flake.lock (as the user would).
	p.clinix(&[
		"env",
		"import",
		"restored",
		archive.to_str().unwrap(),
		"--shell-nix",
		p.file("shell.nix").to_str().unwrap(),
		"--flake-lock",
		p.file("flake.lock").to_str().unwrap(),
	])
	.assert()
	.success();
	assert!(p.env_dir("restored").join("shell.nix").exists());
	p.clinix(&["env", "run", "restored", "--", "rg", "--version"])
		.assert()
		.success();
}

/// The offline-transfer guarantee, tested against a **single-user chroot store**
/// so it runs on a multi-user host without root. The default (complete-env) export
/// must be **offline-sufficient** — every runtime output path present after import
/// (0 missing) — while `--packages` is the smaller *delta* that deliberately omits
/// the base (bash/stdenv). This is a pure store-content check: no network, build,
/// or execution, so nothing can leak in to fake a pass. (Guards Gap F: the import
/// half never runs on a multi-user daemon store.)
#[test]
#[ignore = "needs nix + network"]
fn closure_export_default_is_offline_sufficient_packages_is_the_delta() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", ".", "-p", "ripgrep"])
		.assert()
		.success();
	p.clinix(&["env", "run", ".", "--", "true"])
		.assert()
		.success();

	// Two exports from the same built env: default (complete) vs --packages (delta).
	let full = p.path().join("full.closure");
	let delta = p.path().join("delta.closure");
	p.clinix(&["env", "export", "closure", "--out", full.to_str().unwrap(), "."])
		.assert()
		.success();
	p.clinix(&["env", "export", "closure", "--out", delta.to_str().unwrap(), "--packages", "."])
		.assert()
		.success();
	// The delta is strictly smaller — it omits the base.
	let full_sz = std::fs::metadata(&full).unwrap().len();
	let delta_sz = std::fs::metadata(&delta).unwrap().len();
	assert!(delta_sz < full_sz, "packages-only ({delta_sz}) < complete env ({full_sz})");

	// The full runtime set required to enter the shell (from the ambient store).
	let req = runtime_paths(&p.file("shell.nix"));
	assert!(!req.is_empty(), "runtime path set should be non-empty");

	// Default (complete env) → import into a fresh single-user store → 0 missing.
	let s_full = ChrootStore::new();
	p.clinix(&[
		"env",
		"import",
		"from_full",
		full.to_str().unwrap(),
		"--shell-nix",
		p.file("shell.nix").to_str().unwrap(),
	])
	.env("NIX_REMOTE", s_full.remote())
	.assert()
	.success();
	assert_eq!(
		missing_in_store(&s_full.remote(), &req),
		0,
		"default (complete-env) export must be offline-sufficient — 0 runtime paths missing"
	);

	// --packages → import into another fresh store → base (bash/stdenv) is absent,
	// so runtime paths ARE missing. This documents the delta boundary (not a bug).
	let s_delta = ChrootStore::new();
	p.clinix(&[
		"env",
		"import",
		"from_delta",
		delta.to_str().unwrap(),
		"--shell-nix",
		p.file("shell.nix").to_str().unwrap(),
	])
	.env("NIX_REMOTE", s_delta.remote())
	.assert()
	.success();
	assert!(
		missing_in_store(&s_delta.remote(), &req) > 0,
		"packages-only export is a delta: the base is expected to be absent"
	);
}

/// The seed catalog composes user `{ pkgs }:` fragments against the lazily-locked
/// nixpkgs pin (the `~/dev_env` `compose-dev.nix` behavior), for both a single
/// seed and a stack. Verifies Slice 2/3: `clinix env <seed…>` resolves seeds from
/// config, unions them via `inputsFrom`, and runs.
#[test]
#[ignore = "needs nix + network"]
fn seed_catalog_composes_single_and_stacked_seeds() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.seed(
		"tool",
		"{ pkgs }: pkgs.mkShell { packages = with pkgs; [ ripgrep ]; }\n",
	);
	p.seed(
		"data",
		"{ pkgs }: pkgs.mkShell { packages = with pkgs; [ jq ]; }\n",
	);

	// Single seed: builds the config nixpkgs lock on first use, composes, runs.
	p.clinix(&["env", "run", "tool", "--", "rg", "--version"])
		.assert()
		.success();
	// Stacked seeds: both tools present in the one composed shell.
	p.clinix(&[
		"env",
		"run",
		"data",
		"tool",
		"--",
		"sh",
		"-c",
		"rg --version >/dev/null && jq --version >/dev/null",
	])
	.assert()
	.success();
}

/// `pin add github` resolves a flake input and machine-edits the lock; `pin rm`
/// removes it. Verifies the input add/remove round-trip against a real registry.
#[test]
#[ignore = "needs nix + network"]
fn flake_add_github_input_then_rm_round_trips() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", ".", "-p", "ripgrep"])
		.assert()
		.success();
	// A tiny, stable public repo.
	p.clinix(&[
		"env", "flake", ".", "add", "github", "numtide", "flake-utils", "-b", "main",
	])
	.assert()
	.success()
	.stdout(predicate::str::contains("added input `flake-utils`"));
	let lock = std::fs::read_to_string(p.file("flake.lock")).unwrap();
	assert!(lock.contains("\"repo\": \"flake-utils\""), "input added to the lock");
	// And remove it (pure edit).
	p.clinix(&["env", "flake", ".", "rm", "flake-utils"])
		.assert()
		.success();
	assert!(!std::fs::read_to_string(p.file("flake.lock")).unwrap().contains("flake-utils"));
}

/// The **shared-`flake.lock` project ∪ dev union**: the cwd project (its own
/// `shell.nix` + `flake.lock`) composed with a seed, which builds against the
/// project's pin. Verifies mixing a self-contained env with seeds when they share
/// a lock.
#[test]
#[ignore = "needs nix + network"]
fn shared_lock_union_of_project_and_seed_runs() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", ".", "-p", "ripgrep"])
		.assert()
		.success();
	p.seed(
		"extra",
		"{ pkgs }: pkgs.mkShell { packages = with pkgs; [ jq ]; }\n",
	);
	// `.` (project, reads its own lock) ∪ `extra` (seed, injected the shared pkgs).
	p.clinix(&[
		"env",
		"run",
		".",
		"extra",
		"--",
		"sh",
		"-c",
		"rg --version >/dev/null && jq --version >/dev/null",
	])
	.assert()
	.success();
}

/// The flipped grammar exports the **union** of several envs: `export closure tool
/// data` composes both seeds and serializes their combined closure.
#[test]
#[ignore = "needs nix + network"]
fn export_closure_of_a_union_of_seeds() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.seed(
		"tool",
		"{ pkgs }: pkgs.mkShell { packages = with pkgs; [ ripgrep ]; }\n",
	);
	p.seed(
		"data",
		"{ pkgs }: pkgs.mkShell { packages = with pkgs; [ jq ]; }\n",
	);
	// Build the composed union first (export refuses an unbuilt env).
	p.clinix(&["env", "run", "tool", "data", "--", "true"])
		.assert()
		.success();
	let archive = p.path().join("union.closure");
	p.clinix(&["env", "export", "closure", "--out", archive.to_str().unwrap(), "tool", "data"])
		.assert()
		.success()
		.stdout(
			predicate::str::contains("tool data").and(predicate::str::contains("store paths")),
		);
	assert!(archive.is_file() && archive.metadata().unwrap().len() > 0);
}

/// `export docker <env> --build` renders `docker-base.nix` and nix-builds it into
/// an image tarball (streamLayeredImage → tar; no docker daemon needed). Proves
/// the generated expression evaluates and the env's packages become the image.
#[test]
#[ignore = "needs nix + network"]
fn export_docker_nix_base_builds_an_image_tarball() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", ".", "-p", "ripgrep"])
		.assert()
		.success();
	let out = p.path().join("containers");
	p.clinix(&[
		"env",
		"export",
		"docker",
		".",
		"--name",
		"testimg",
		"--out",
		out.to_str().unwrap(),
		"--build",
	])
	.assert()
	.success()
	.stdout(predicate::str::contains("image tarball"));
	let tar = out.join("testimg.tar");
	assert!(tar.is_file() && tar.metadata().unwrap().len() > 0, "image tar produced");
}

/// `new --from` materializes a seed stack into a **portable** registry env
/// (copied fragments + local lock + a self-contained `shell.nix`), which then
/// enters and runs. Verifies Slice 4.
#[test]
#[ignore = "needs nix + network"]
fn new_from_materializes_a_portable_env_that_composes_and_runs() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.seed(
		"tool",
		"{ pkgs }: pkgs.mkShell { packages = with pkgs; [ ripgrep ]; }\n",
	);
	// Materialize (builds the config nixpkgs lock on first use), then enter + run.
	p.clinix(&["env", "new", "mytools", "--from", "tool"])
		.assert()
		.success();
	p.clinix(&["env", "run", "mytools", "--", "rg", "--version"])
		.assert()
		.success();
}

/// `run` is **exit-transparent** (mirrors the command's exact code, like the
/// `~/dev_env` `exec nix-shell` prototype) and **GC-roots** the env on entry, so
/// `nix-collect-garbage` cannot reap a shell you just entered. (Gaps A + B.)
#[test]
#[ignore = "needs nix + network"]
fn run_is_exit_transparent_and_roots_the_env_on_entry() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", ".", "-p", "ripgrep"])
		.assert()
		.success();

	// Success path: exit 0, and a real indirect GC root lands under the isolated
	// state dir (a `proj-<slug>` symlink), not a synthetic one.
	p.clinix(&["env", "run", ".", "--", "true"])
		.assert()
		.success();
	let roots = p.state().join("roots");
	let has_proj_root = std::fs::read_dir(&roots)
		.expect("roots dir should exist after entering an env")
		.filter_map(Result::ok)
		.any(|e| e.file_name().to_string_lossy().starts_with("proj-"));
	assert!(has_proj_root, "entering the env must create a proj-* GC root");

	// Exit-transparency: a nonzero command propagates its *exact* code, not a
	// blanket failure (false → 1; `exit 3` → 3).
	p.clinix(&["env", "run", ".", "--", "false"])
		.assert()
		.code(1);
	p.clinix(&["env", "run", ".", "--", "sh", "-c", "exit 3"])
		.assert()
		.code(3);
}

/// A **File target** — an explicit `*.nix` path — is entered directly (not
/// composed), so `clinix env run ./shell.nix -- …` runs that file's shell.
#[test]
#[ignore = "needs nix + network"]
fn shell_file_target_runs_the_file_directly() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", ".", "-p", "ripgrep"])
		.assert()
		.success();
	p.clinix(&["env", "run", "./shell.nix", "--", "rg", "--version"])
		.assert()
		.success()
		.stdout(predicate::str::contains("ripgrep"));
	// A file target GC-roots by a path slug (`file-*`), like a project.
	let has_file_root = std::fs::read_dir(p.state().join("roots"))
		.unwrap()
		.filter_map(Result::ok)
		.any(|e| e.file_name().to_string_lossy().starts_with("file-"));
	assert!(has_file_root, "a file target creates a file-* GC root");
}

/// `new … --from <dir>` (register, non-copy) wraps the referenced source: the
/// wrapper reads the established pin, injects `pkgs`, and imports the source — so
/// entering the registered env runs the source shell.
#[test]
#[ignore = "needs nix + network"]
fn new_register_non_copy_runs_the_referenced_shell() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", "src", "-p", "ripgrep"]).assert().success();
	p.clinix(&["env", "new", "tools", "--from", "src"])
		.assert()
		.success()
		.stdout(predicate::str::contains("referenced"));
	p.clinix(&["env", "run", "tools", "--", "rg", "--version"])
		.assert()
		.success()
		.stdout(predicate::str::contains("ripgrep"));
}

/// `new namespace:member --from <dir>` registers a member sharing the namespace
/// pin (`../flake.lock`); entering `namespace:member` runs it.
#[test]
#[ignore = "needs nix + network"]
fn new_register_member_runs_sharing_the_namespace_pin() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", "src", "-p", "ripgrep"]).assert().success();
	p.clinix(&["env", "new", "proj:dev", "--from", "src"])
		.assert()
		.success()
		.stderr(predicate::str::contains("created empty namespace"));
	p.clinix(&["env", "run", "proj:dev", "--", "rg", "--version"])
		.assert()
		.success()
		.stdout(predicate::str::contains("ripgrep"));
}

/// A directory env with no `shell.nix` falls back to `default.nix` (the same
/// lookup `nix-shell` performs), so `clinix env run .` still enters it.
#[test]
#[ignore = "needs nix + network"]
fn default_nix_fallback_is_entered() {
	if !have_nix() {
		return;
	}
	let p = Project::new();
	p.clinix(&["env", "init", ".", "-p", "ripgrep"])
		.assert()
		.success();
	std::fs::rename(p.file("shell.nix"), p.file("default.nix")).unwrap();
	p.clinix(&["env", "run", ".", "--", "rg", "--version"])
		.assert()
		.success()
		.stdout(predicate::str::contains("ripgrep"));
}
