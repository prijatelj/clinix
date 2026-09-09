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

	/// The clinix config root this sandbox pins (`$XDG_CONFIG_HOME/clinix`).
	pub fn config_dir(&self) -> PathBuf {
		self.config.path().join("clinix")
	}

	/// A registry env's directory: `state/clinix/envs/<name>`.
	pub fn env_dir(&self, name: &str) -> PathBuf {
		self.state().join("envs").join(name)
	}

	/// A registry env's GC-root **base** path (name-keyed): `state/clinix/roots/env-<name>`.
	/// The real roots are versioned siblings — see [`Self::env_root_version`].
	pub fn env_root_link(&self, name: &str) -> PathBuf {
		self.state().join("roots").join(format!("env-{name}"))
	}

	/// A registry env's versioned `.drv` GC-root path: `roots/env-<name>@<seq>`. Its
	/// package-closure sibling is the same path with `.rt` appended.
	pub fn env_root_version(&self, name: &str, seq: u64) -> PathBuf {
		self.state()
			.join("roots")
			.join(format!("env-{name}@{seq}"))
	}

	/// Configure a seed catalog: write `config.toml` pointing at a seeds dir and
	/// drop a `<name>.nix` fragment into it, so `clinix env <name>` resolves a
	/// seed. Repeated calls add more seeds (the config.toml is rewritten to the
	/// one seeds dir). Returns the seed file path.
	pub fn seed(&self, name: &str, body: &str) -> PathBuf {
		let cfg = self.config.path().join("clinix");
		let seeds = self.config.path().join("seeds");
		std::fs::create_dir_all(&cfg).unwrap();
		std::fs::create_dir_all(&seeds).unwrap();
		let file = seeds.join(format!("{name}.nix"));
		std::fs::write(&file, body).unwrap();
		std::fs::write(
			cfg.join("config.toml"),
			format!(
				"[env.seeds]\nsources = [ {{ path = \"{}\" }} ]\n",
				seeds.display()
			),
		)
		.unwrap();
		file
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

/// Whether `nix-store --import` of unsigned paths can succeed here. A multi-user
/// (daemon) store rejects unsigned paths from a non-root/untrusted user (the
/// prototype used sudo); a single-user store, or running as root, has no such
/// gate. Used to guard the closure-import round-trip in E2E.
pub fn can_import_store() -> bool {
	let multiuser = Path::new("/nix/var/nix/daemon-socket/socket").exists();
	let is_root = Command::new("id")
		.arg("-u")
		.output()
		.ok()
		.and_then(|o| String::from_utf8(o.stdout).ok())
		.is_some_and(|s| s.trim() == "0");
	!multiuser || is_root
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

/// A relocated **single-user** Nix store rooted in a tempdir
/// (`NIX_REMOTE=local?root=…`). A store the caller owns has single-user semantics,
/// so an **unsigned** closure imports without root or a trusted user — the way to
/// exercise the offline import/round-trip on a multi-user host (see the
/// `clinix-closure-import-trust` constraint). Set [`ChrootStore::remote`] as
/// `NIX_REMOTE` on the clinix/nix command whose store you want redirected.
///
/// Cleanup contract: imported store paths are read-only (mode 555), so `Drop`
/// runs `chmod -R u+w` **before** the inner `TempDir` removes them — otherwise
/// removal fails and leaks the closure (hundreds of MB).
pub struct ChrootStore {
	dir: TempDir,
}

impl ChrootStore {
	pub fn new() -> Self {
		Self {
			dir: TempDir::new().unwrap(),
		}
	}

	/// The `NIX_REMOTE` value that points nix at this store.
	pub fn remote(&self) -> String {
		format!("local?root={}", self.dir.path().display())
	}

	pub fn root(&self) -> &Path {
		self.dir.path()
	}
}

impl Default for ChrootStore {
	fn default() -> Self {
		Self::new()
	}
}

impl Drop for ChrootStore {
	fn drop(&mut self) {
		// Make read-only store paths writable so the inner TempDir can remove them
		// (its Drop runs after this and is what actually deletes the directory).
		let _ = Command::new("chmod")
			.args(["-R", "u+w"])
			.arg(self.dir.path())
			.status();
	}
}

/// The realized **runtime output paths** an env's `shell.nix` needs to be entered,
/// read from the ambient store: the include-outputs closure of the instantiated
/// shell derivation, minus the `.drv` recipes. This is the set an offline-usable
/// export must cover. L4 only (shells out to nix).
pub fn runtime_paths(shell_nix: &Path) -> Vec<String> {
	let drv = Command::new("nix-instantiate")
		.arg(shell_nix)
		.output()
		.expect("nix-instantiate should spawn");
	let drv = String::from_utf8(drv.stdout).unwrap().trim().to_string();
	let out = Command::new("nix-store")
		.args(["--query", "--requisites", "--include-outputs", &drv])
		.output()
		.expect("nix-store should spawn");
	String::from_utf8(out.stdout)
		.unwrap()
		.lines()
		.filter(|l| !l.ends_with(".drv"))
		.map(str::to_string)
		.collect()
}

/// How many of `paths` are **not** valid in the store named by `remote`
/// (`NIX_REMOTE`). Zero means the store contains the whole set — the offline
/// content-sufficiency check (pure store query: no network, build, or exec).
pub fn missing_in_store(remote: &str, paths: &[String]) -> usize {
	let out = Command::new("nix-store")
		.env("NIX_REMOTE", remote)
		.args(["--check-validity", "--print-invalid"])
		.args(paths)
		.output()
		.expect("nix-store should spawn");
	String::from_utf8(out.stdout)
		.unwrap()
		.lines()
		.filter(|l| !l.trim().is_empty())
		.count()
}
