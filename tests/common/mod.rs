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
}

impl Default for Project {
	fn default() -> Self {
		Self::new()
	}
}

/// True when the classic nix toolchain an L4 test needs is on PATH; L4 tests
/// skip (rather than fail) when it isn't.
pub fn have_nix() -> bool {
	["nix-shell", "nix-instantiate", "nix-store", "nix-prefetch-url"]
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
