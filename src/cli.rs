//! Top-level command tree and dispatch.
//!
//! Grammar (decided 2026-08-27):
//! - Canonical, verb-first: `clinix env <verb> [names…] [args]`.
//! - System: `clinix sys <verb> …`.
//! - Sugar (the common case): a bare name list `clinix <names…>` is captured by
//!   clap's `external_subcommand` and routed to `env shell <names…>`. This form
//!   is *terminal* — no verb may follow the names — which is why it is
//!   unambiguous despite the variadic name list.
//! - `clinix` with no args enters the current directory's project shell.
//!
//! Global flags (`-o`, `-r`, `-v`) are declared once here and read by whichever
//! command needs them, so the sugar form can still pass the common options that
//! `external_subcommand` cannot itself parse.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::env::config::{Config, Overrides};
use crate::env::{Context, EnvArgs, InfoVerb, RunCmd, Shell, ShellOptions, context_report};
use crate::error::Result;
use crate::sys::SysArgs;

/// `clinix [global flags] [command | names…]`
#[derive(Parser, Debug)]
#[command(name = "clinix", version, propagate_version = true)]
#[command(about = "Manage Nix environments (env) and a NixOS system (sys).")]
pub struct Cli {
	/// Increase verbosity (repeatable: `-v`, `-vv`).
	#[arg(short = 'v', long, global = true, action = clap::ArgAction::Count)]
	pub verbose: u8,

	/// Preserve the given order when composing envs instead of sorting them
	/// lexically. Applies to `env shell`/`run` and the bare-name sugar.
	#[arg(short = 'o', long, global = true)]
	pub ordered: bool,

	/// In a project, compose only the runtime env (skip the dev tools).
	#[arg(short = 'r', long, global = true)]
	pub runtime: bool,

	/// Override clinix's config dir (else `$CLINIX_CONFIG_DIR`, `$XDG_CONFIG_HOME`,
	/// `~/.config/clinix`). See [`crate::env::config`].
	#[arg(long, global = true, value_name = "DIR")]
	pub config: Option<PathBuf>,

	/// Override clinix's state dir (else `$CLINIX_STATE_DIR`, `$XDG_STATE_HOME`,
	/// `~/.local/state/clinix`). Holds the env registry and GC roots.
	#[arg(long = "state-dir", global = true, value_name = "DIR")]
	pub state_dir: Option<PathBuf>,

	#[command(subcommand)]
	pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
	/// Manage the NixOS system configuration (`system.nix`).
	Sys(SysArgs),

	/// Manage a Nix environment: init, shell, run, packages, pins, interop, info.
	Env(EnvArgs),

	/// Report the active environment (what shell you're in) and, with a subverb,
	/// audit or inspect it — the contextual counterpart to the explicit `env`
	/// verbs. `info check`/`info deps` act on the active (cwd) env.
	Info(InfoArgs),

	/// Show clinix's config file path, or print a template to fill in.
	Config(ConfigArgs),

	/// Bare name list → `env shell <names…>` (see module docs). The first token
	/// is the "subcommand name" clap could not match, so it is folded back in.
	#[command(external_subcommand)]
	Shell(Vec<String>),
}

/// `clinix config <example|path>`.
#[derive(clap::Args, Debug)]
pub struct ConfigArgs {
	#[command(subcommand)]
	pub verb: ConfigVerb,
}

#[derive(Subcommand, Debug)]
pub enum ConfigVerb {
	/// Print a config template to stdout — minimal by default, `--full` for the
	/// extensive annotated template with defaults. Redirect into your config file:
	/// `clinix config example --full > ~/.config/clinix/config.toml`.
	Example {
		/// Emit the extensive, fully-annotated template (every option + defaults).
		#[arg(long)]
		full: bool,
	},
	/// Print the resolved config file path clinix reads.
	Path,
}

/// `clinix info [check|deps]` — the optional subverb defaults to a summary of the
/// active environment.
#[derive(clap::Args, Debug)]
pub struct InfoArgs {
	#[command(subcommand)]
	pub verb: Option<InfoVerb>,
}

impl Cli {
	/// Route the parsed command to its handler. The two name-first entry points
	/// (`None` = cwd project, `Shell(..)` = bare names) both resolve to
	/// `env shell`, keeping a single launcher implementation.
	pub fn dispatch(self) -> Result<()> {
		let context = Context {
			options: ShellOptions {
				ordered: self.ordered,
				runtime: self.runtime,
			},
			config: Config::resolve(&Overrides {
				config_dir: self.config,
				state_dir: self.state_dir,
			}),
		};
		match self.command {
			Some(Command::Sys(args)) => crate::sys::dispatch(args),
			Some(Command::Env(c)) => c.cmd.run(&context),
			Some(Command::Info(a)) => context_report(a.verb, &context),
			Some(Command::Config(a)) => match a.verb {
				ConfigVerb::Example { full } => {
					let tmpl = if full {
						crate::env::config::FULL_TEMPLATE
					} else {
						crate::env::config::MINIMAL_TEMPLATE
					};
					print!("{tmpl}");
					Ok(())
				}
				ConfigVerb::Path => {
					println!("{}", context.config.config_dir.join("config.toml").display());
					Ok(())
				}
			},
			Some(Command::Shell(names)) => Shell { names, pure: false }.run(&context),
			None => Shell {
				names: vec![],
				pure: false,
			}
			.run(&context),
		}
	}
}
