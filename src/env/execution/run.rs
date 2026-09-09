//! `run`: run a command inside a composed env, non-interactively.

use clap::Args;

use crate::env::{Context, RunCmd, launch};
use crate::error::{ClinixError, Result};

#[derive(Args, Debug)]
pub struct Run {
	/// Envs to compose, as a stack — same rules as [`super::shell::Shell`]
	/// (`.` = cwd project, empty ⇒ `[.]`).
	pub names: Vec<String>,
	/// Run inside the Nth **prior** root version instead of the current (`1` = the
	/// version immediately before current); enters the stored recipe directly
	/// (offline-capable) and mints nothing. See `env roots <name>`.
	#[arg(long, conflicts_with = "root_version")]
	pub prior: Option<usize>,
	/// Run inside an **exact** root version by its id (as shown by `env roots <name>`).
	#[arg(long = "root-version", conflicts_with = "prior")]
	pub root_version: Option<u64>,
	/// The command (and its args) to run inside the env; after `--`.
	#[arg(last = true, required = true)]
	pub command: Vec<String>,
}
impl RunCmd for Run {
	/// GC-root the env's `shell.nix` and run `command` inside it via
	/// `nix-shell --run`, non-interactively. clinix is **exit-transparent**: it
	/// mirrors the command's exit code (like `nix-shell --run` / the `~/dev_env`
	/// `exec nix-shell` prototype), so `run` composes in scripts and CI.
	fn run(self, context: &Context) -> Result<()> {
		let command = shell_join(&self.command);
		let version = super::version_select(self.prior, self.root_version);
		let status = launch(context, &self.names, false, Some(&command), version)?;
		if status.success() {
			Ok(())
		} else {
			// Propagate the exact code (128+signal collapses to 1 via `unwrap_or`).
			Err(ClinixError::CommandFailed {
				code: status.code().unwrap_or(1),
			})
		}
	}
}

/// POSIX-quote each arg into one `nix-shell --run` string, so args with spaces or
/// metacharacters survive the bash the `--run` string is executed by.
fn shell_join(args: &[String]) -> String {
	args.iter()
		.map(|a| format!("'{}'", a.replace('\'', r"'\''")))
		.collect::<Vec<_>>()
		.join(" ")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn shell_join_quotes_each_arg() {
		assert_eq!(
			shell_join(&["cargo".into(), "build".into()]),
			"'cargo' 'build'"
		);
		// A single quote inside an arg is escaped, not left to break the string.
		assert_eq!(shell_join(&["it's".into()]), r"'it'\''s'");
	}
}
