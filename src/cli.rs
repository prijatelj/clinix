//! Command Line Interface: command tree and dispatch.
//!
//! ## Grammar:
//! - top level commands: sys, env, info, config, completions, man
//! - Verb-first: `clinix env <verb> [names…] [args]`.
//! - Sugar (the common case): a bare name list `clinix <names…>` is captured by
//!   clap's `external_subcommand` and routed to `env shell <names…>`. This form
//!   is *terminal* — no verb may follow the names — which is why it is
//!   unambiguous despite the variadic name list.
//! 	- `clinix` with no args prints help — it does **not** enter the cwd
//!			project, unlike `clinix env shell`. Use `clinix .` at top level.
//!
//! Global flags (`-o`, `-r`, `-v`) are declared once here and read by whichever
//! command needs them, so the sugar form can still pass the common options that
//! `external_subcommand` cannot itself parse.

use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};

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

	/// Print a shell completion script to stdout (bash, zsh, fish, elvish,
	/// powershell). Install it, or in nix `installShellCompletion`.
	Completions(CompletionsArgs),

	/// Write the man page (roff) — so `man clinix` works. To a directory on
	/// MANPATH, or stdout.
	Man(ManArgs),

	/// Bare name list → `env shell <names…>` (see module docs). The first token
	/// is the "subcommand name" clap could not match, so it is folded back in.
	#[command(external_subcommand)]
	Shell(Vec<String>),
}

/// `clinix completions <shell>`.
#[derive(clap::Args, Debug)]
pub struct CompletionsArgs {
	/// Shell to emit a completion script for.
	pub shell: clap_complete::Shell,
}

/// `clinix man [dir]`.
#[derive(clap::Args, Debug)]
pub struct ManArgs {
	/// Output directory for `clinix.1` (install onto MANPATH, e.g.
	/// `~/.local/share/man/man1`). Omit to print the roff to stdout.
	pub dir: Option<PathBuf>,
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
	/// Show where clinix keeps its state on disk (registry envs, GC roots, the
	/// generated compose files, and the seed nixpkgs pin).
	State,
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
					println!(
						"{}",
						context.config.config_dir.join("config.toml").display()
					);
					Ok(())
				}
				ConfigVerb::State => {
					use crate::env::registry;
					let cfg = &context.config;
					println!("state:    {}", cfg.state_dir.display());
					println!(
						"  envs:    {}   registry envs (env new / import)",
						registry::envs_dir(cfg).display()
					);
					println!(
						"  roots:   {}   GC roots (entered envs)",
						registry::roots_dir(cfg).display()
					);
					println!(
						"  compose: {}   generated seed-stack compose files",
						registry::compose_dir(cfg).display()
					);
					println!(
						"pin lock: {}   seed nixpkgs pin (generated; lives in config)",
						cfg.config_dir.join("flake.lock").display()
					);
					Ok(())
				}
			},
			Some(Command::Completions(a)) => {
				let mut cmd = Cli::command();
				clap_complete::generate(a.shell, &mut cmd, "clinix", &mut std::io::stdout());
				Ok(())
			}
			Some(Command::Man(a)) => {
				let man = clap_mangen::Man::new(Cli::command());
				match a.dir {
					Some(dir) => {
						std::fs::create_dir_all(&dir)?;
						let path = dir.join("clinix.1");
						let mut buf = Vec::new();
						man.render(&mut buf)?;
						std::fs::write(&path, buf)?;
						eprintln!("clinix: wrote {}", path.display());
						Ok(())
					}
					None => {
						man.render(&mut std::io::stdout())?;
						Ok(())
					}
				}
			}
			Some(Command::Shell(names)) => Shell {
				names,
				pure: false,
				prior: None,
				root_version: None,
			}
			.run(&context),
			// Bare `clinix` (or only global flags) → print help, rather than
			// silently entering the cwd project. Enter the project explicitly with
			// `clinix .` or `clinix env shell`.
			None => {
				Cli::command().print_help()?;
				Ok(())
			}
		}
	}
}
