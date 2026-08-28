//! `run`: run a command inside a composed env, non-interactively.

use clap::Args;

use crate::env::{Context, RunCmd, launch};
use crate::error::{ClinixError, Result};

#[derive(Args, Debug)]
pub struct Run {
	/// Envs to compose, as a stack — same rules as [`super::shell::Shell`]
	/// (`.` = cwd project, empty ⇒ `[.]`).
	pub names: Vec<String>,
	/// The command (and its args) to run inside the env; after `--`.
	#[arg(last = true, required = true)]
	pub command: Vec<String>,
}
impl RunCmd for Run {
	/// GC-root the env's `shell.nix` and run `command` inside it via
	/// `nix-shell --run`, non-interactively. A nonzero command exit fails clinix.
	fn run(self, _context: &Context) -> Result<()> {
		let command = shell_join(&self.command);
		let status = launch(&self.names, false, Some(&command))?;
		if status.success() {
			Ok(())
		} else {
			Err(ClinixError::Nix {
				cmd: format!("nix-shell --run {command}"),
				status: status.to_string(),
				stderr: String::new(),
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
		assert_eq!(shell_join(&["cargo".into(), "build".into()]), "'cargo' 'build'");
		// A single quote inside an arg is escaped, not left to break the string.
		assert_eq!(shell_join(&["it's".into()]), r"'it'\''s'");
	}
}
