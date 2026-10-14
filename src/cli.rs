//! Command Line Interface: command tree and dispatch.
//!
//! ## Grammar:
//! - top level commands: sys, env, info, config, completions, man
//! - Verb-first: `clinix env <verb> [names…] [args]`.
//! - Sugar (the common case): a bare name list `clinix <names…>` is `env shell
//!   <names…>`. It is **parsed by clap**, not hand-split: [`Shell`] is flattened
//!   into both [`Cli`] and [`EnvArgs`] under
//!   `args_conflicts_with_subcommands`, so a token list that matches no
//!   subcommand is parsed as `Shell`'s own arguments. A name equal to a verb is
//!   taken as that verb — write `env shell <name>` for such an env.
//! 	- `clinix` with no args prints help — it does **not** enter the cwd
//!			project, unlike `clinix env shell`. Use `clinix .` at top level.
//!
//! That flatten is why the sugar supports *every* `env shell` flag (`--pure`,
//! `--prior`, …) rather than only the ones a hand-written splitter knew about: a
//! flag is declared once, on `Shell`, and clap does the parsing at both levels.
//!
//! **Do not add `args_conflicts_with_subcommands`.** It reads like the right
//! setting for "names XOR a verb", but it means args XOR subcommands *literally*:
//! with it set, any global flag given before a verb (`clinix -o env shell …`)
//! suppresses subcommand matching, and the verb is swallowed as an env name. It is
//! unnecessary anyway — clap already prefers a matching subcommand over the
//! flattened positional.
//!
//! Global flags (`-o`, `-r`, `-v`, `-p`/`--pkg`) are declared once here. `-p` is
//! global rather than per verb because the ad-hoc package set is part of the
//! composition *request*, which every verb reads from [`Context`].

use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};

use crate::env::config::{Config, Overrides};
use crate::env::{
	Context, EnvArgs, InfoVerb, Pkg, PkgArgs, RunCmd, Shell, ShellOptions, context_report,
};
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
	/// **Currently inert**: implicit project composition (runtime ∪ dev shells) is
	/// deferred, so entering a project already composes only that project — there
	/// is nothing yet to opt out of. Union dev tools by naming them explicitly
	/// instead: `clinix python claude .`.
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

	/// Ad-hoc packages layered onto the composition, resolved from its own nixpkgs
	/// pin — the `nix-shell -p` shape. **Greedy**: every remaining bare token is a
	/// package, not an env. The resulting shell is ephemeral (entered unrooted);
	/// keep it with `env new <name> --from <envs…> -p <pkgs…>`.
	///
	/// Declared here, once, and `global` so it reaches `env shell`/`run`/`new` and
	/// can lead the bare-name sugar (`clinix -p tokei`). A trailing occurrence
	/// (`clinix claude -p tokei`) lands in the sugar's raw token list instead and is
	/// recovered by `Shell::from_sugar`; the two are merged.
	#[arg(short = 'p', long = "pkgs", global = true, num_args = 1.., value_name = "PKG")]
	pub pkgs: Vec<Pkg>,

	/// One ad-hoc package; repeatable, and bare tokens after it are still envs —
	/// `clinix claude --pkg tokei rust --pkg fastfetch python`.
	#[arg(long = "pkg", global = true, value_name = "PKG")]
	pub pkg: Vec<Pkg>,

	#[command(subcommand)]
	pub command: Option<Command>,

	/// The bare-name sugar: `clinix <names…> [shell flags]` ≡ `clinix env shell
	/// <names…>`. Parsed only when no subcommand matched.
	#[command(flatten)]
	pub shell: Shell,
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
	/// powershell).
	///
	/// clinix writes the script and nothing else — where it belongs depends on
	/// your shell and on whether the install is user-local, system-wide, or done
	/// by a package manager. The recipes for each are below.
	Completions(CompletionsArgs),

	/// Write the man page (roff) so `man clinix` works — to a `man1` directory,
	/// or stdout.
	///
	/// The user-local case needs care: `~/.local/share/man` is not universally on
	/// the man search path (a Nix-managed profile usually omits it), so the
	/// recipes below pair the write with making it findable.
	Man(ManArgs),
}

/// Where a generated completion script belongs, per shell. clinix only writes to
/// stdout — installing is the caller's choice (user-local, system-wide, or by a
/// package manager), so the recipes are documented rather than guessed at.
const COMPLETIONS_HELP: &str = r#"Install for one user:
  bash  clinix completions bash > ~/.local/share/bash-completion/completions/clinix
  zsh   clinix completions zsh  > ~/.local/share/zsh/site-functions/_clinix
        # the dir must be on $fpath BEFORE compinit runs, e.g. in ~/.zshrc:
        #   fpath=(~/.local/share/zsh/site-functions $fpath)
        #   autoload -Uz compinit && compinit
  fish  clinix completions fish > ~/.config/fish/completions/clinix.fish
  pwsh  clinix completions powershell >> $PROFILE

Install system-wide (what a distro package does):
  bash  /usr/share/bash-completion/completions/clinix
  zsh   /usr/share/zsh/site-functions/_clinix
  fish  /usr/share/fish/vendor_completions.d/clinix.fish

In a nix derivation, let installShellFiles place all three:
  nativeBuildInputs = [ installShellFiles ];
  postInstall = ''
    installShellCompletion --cmd clinix \
      --bash <($out/bin/clinix completions bash) \
      --zsh  <($out/bin/clinix completions zsh) \
      --fish <($out/bin/clinix completions fish)
  '';

A written script is a snapshot of THIS version's verbs and flags, so re-generate
it after upgrading clinix. To never have to, generate on shell startup instead:
  bash/zsh  source <(clinix completions bash)   # use `zsh` in ~/.zshrc
  fish      clinix completions fish | source"#;

/// `clinix completions <shell>`.
#[derive(clap::Args, Debug)]
#[command(after_long_help = COMPLETIONS_HELP)]
pub struct CompletionsArgs {
	/// Shell to emit a completion script for.
	pub shell: clap_complete::Shell,
}

/// How to get `man clinix` working. The user-local path is the fiddly one: a
/// `~/.local/share/man` is **not** universally on the man search path (notably
/// not under a Nix-managed profile), so the recipe pairs the write with the two
/// ways to make it findable instead of assuming it is picked up.
const MAN_HELP: &str = r#"Install for one user:
  clinix man ~/.local/share/man/man1          # writes clinix.1 there
  man clinix                                  # should now work

  If it does not, that dir is not on the man search path (`manpath` shows the
  real one — a Nix-managed profile usually omits it). Fix it either way:
    export MANPATH="$HOME/.local/share/man:$(manpath)"     # in your shell rc
  or add a line to ~/.manpath:
    MANDATORY_MANPATH /home/<you>/.local/share/man

Install system-wide (what a distro package does):
  clinix man /usr/share/man/man1

In a nix derivation:
  nativeBuildInputs = [ installShellFiles ];
  postInstall = ''
    $out/bin/clinix man .
    installManPage clinix.1
  '';

Without installing anything:
  clinix man | man -l -                       # preview the page
  clinix man > /tmp/clinix.1                  # keep the roff

Only the top-level page is generated; per-verb detail lives in
`clinix env <verb> --help`."#;

/// `clinix man [dir]`.
#[derive(clap::Args, Debug)]
#[command(after_long_help = MAN_HELP)]
pub struct ManArgs {
	/// Output directory for `clinix.1` — a `man1` dir on the man search path (see
	/// the recipes below). Omit to print the roff to stdout.
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
		// Validate the ad-hoc package specs once, here, so every consumer
		// (`env shell`/`run`/`new`, the sugar) reports a bad spec identically.
		let packages = PkgArgs {
			pkgs: self.pkgs,
			pkg: self.pkg,
		}
		.names()?;
		let context = Context {
			options: ShellOptions {
				ordered: self.ordered,
				runtime: self.runtime,
				packages,
			},
			config: {
				let cfg = Config::resolve(&Overrides {
					config_dir: self.config,
					state_dir: self.state_dir,
				});
				// `[env].registry` can relocate the registry root, so fold the settings
				// in once here rather than re-reading them at every path lookup.
				//
				// Best-effort on purpose: a malformed `config.toml` must not break
				// `clinix config example` or `--help` — the very commands you reach for
				// to fix it. Verbs that genuinely need the settings load them again and
				// surface the parse error there.
				match crate::env::config::Settings::load(&cfg.config_dir) {
					Ok(settings) => cfg.with_settings(&settings),
					Err(_) => cfg,
				}
			},
		};
		match self.command {
			Some(Command::Sys(args)) => crate::sys::dispatch(args),
			// `clinix env <verb> …`, or `clinix env <names…>` (the same sugar, one
			// level down). Bare `clinix env` prints the env help rather than
			// silently entering the cwd project.
			Some(Command::Env(c)) => match c.cmd {
				Some(cmd) => cmd.run(&context),
				None if c.shell.names.is_empty() && context.options.packages.is_empty() => {
					if let Some(env) = Cli::command().find_subcommand_mut("env") {
						env.print_help()?;
					}
					Ok(())
				}
				None => c.shell.run(&context),
			},
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
			// Bare `clinix` (or only global flags) → print help, rather than
			// silently entering the cwd project. Enter the project explicitly with
			// `clinix .` or `clinix env shell`.
			None if self.shell.names.is_empty() && context.options.packages.is_empty() => {
				Cli::command().print_help()?;
				Ok(())
			}
			// The sugar: names and/or `-p` with no subcommand → the launcher.
			None => self.shell.run(&context),
		}
	}
}
