//! Shared test harness (L3 CLI / L4 E2E), per `notes/clinix/design/testing.md`.
//!
//! A [`Project`] isolates *all* clinix state and config to per-test tempdirs
//! (leveraging the config/state "from anywhere" design), so tests can never see
//! or corrupt the real user environment. It's the first consumer of that design.

// Helpers are shared across test crates; not every crate uses every helper.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::Command as AssertCommand;
use tempfile::TempDir;

/// An isolated project sandbox: a working directory plus throwaway XDG state,
/// config, and HOME. Everything a clinix invocation reads or writes stays here.
pub struct Project {
	dir: TempDir,
	state: TempDir,
	config: TempDir,
	home: TempDir,
}

impl Project {
	pub fn new() -> Self {
		Self {
			dir: TempDir::new().unwrap(),
			state: TempDir::new().unwrap(),
			config: TempDir::new().unwrap(),
			home: TempDir::new().unwrap(),
		}
	}

	/// An `assert_cmd` command for the built `clinix`, with cwd = the project dir
	/// and XDG state/config/HOME pinned to this sandbox's tempdirs.
	pub fn clinix(&self, args: &[&str]) -> AssertCommand {
		let mut cmd = AssertCommand::cargo_bin("clinix").unwrap();
		cmd.current_dir(self.dir.path())
			.env("XDG_STATE_HOME", self.state.path())
			.env("XDG_CONFIG_HOME", self.config.path())
			.env("HOME", self.home.path())
			.args(args);
		cmd
	}

	pub fn path(&self) -> &Path {
		self.dir.path()
	}

	pub fn file(&self, name: &str) -> PathBuf {
		self.dir.path().join(name)
	}

	/// The clinix state root this sandbox pins (`$XDG_STATE_HOME/clinix`).
	pub fn state(&self) -> PathBuf {
		self.state.path().join("clinix")
	}

	/// A registry env's directory: `state/clinix/envs/<name>`.
	pub fn env_dir(&self, name: &str) -> PathBuf {
		self.state().join("envs").join(name)
	}

	/// A registry env's GC-root symlink path (name-keyed): `state/clinix/roots/env-<name>`.
	pub fn env_root_link(&self, name: &str) -> PathBuf {
		self.state().join("roots").join(format!("env-{name}"))
	}

	/// Materialize a registry env `<name>` with the given `shell.nix` body and a
	/// minimal `flake.lock`, so registry verbs (`list`/`rename`/`clean`) have
	/// something to act on without invoking nix. Returns the env directory.
	pub fn seed_registry_env(&self, name: &str, shell_nix: &str) -> PathBuf {
		let dir = self.env_dir(name);
		std::fs::create_dir_all(&dir).unwrap();
		std::fs::write(dir.join("shell.nix"), shell_nix).unwrap();
		std::fs::write(dir.join("flake.lock"), MINIMAL_LOCK).unwrap();
		dir
	}
}

/// A one-input `flake.lock` (nixpkgs tracking `nixos-26.05` at a fixed rev), used
/// to give seeded registry envs a readable pin for `list` enrichment.
pub const MINIMAL_LOCK: &str = r#"{
  "nodes": {
    "nixpkgs": {
      "locked": { "narHash": "sha256-x", "owner": "NixOS", "repo": "nixpkgs", "rev": "abc1234567890def", "type": "github" },
      "original": { "owner": "NixOS", "ref": "nixos-26.05", "repo": "nixpkgs", "type": "github" }
    },
    "root": { "inputs": { "nixpkgs": "nixpkgs" } }
  },
  "root": "root",
  "version": 7
}
"#;

impl Default for Project {
	fn default() -> Self {
		Self::new()
	}
}

/// True when the classic nix toolchain an L4 test needs is on PATH; L4 tests
/// skip (rather than fail) when it isn't.
pub fn have_nix() -> bool {
	[
		"nix-shell",
		"nix-instantiate",
		"nix-store",
		"nix-prefetch-url",
	]
	.iter()
	.all(|bin| on_path(bin))
}

fn on_path(bin: &str) -> bool {
	Command::new("sh")
		.arg("-c")
		.arg(format!("command -v {bin}"))
		.output()
		.is_ok_and(|o| o.status.success())
}

/// Run a command inside a scaffolded env via vanilla `nix-shell` (the thesis:
/// `shell.nix` + `flake.lock` runs under plain nix, no flakes). L4 only.
pub fn nix_shell_run(shell_nix: &Path, command: &str) -> std::process::Output {
	Command::new("nix-shell")
		.arg(shell_nix)
		.arg("--run")
		.arg(command)
		.output()
		.expect("nix-shell should spawn")
}
