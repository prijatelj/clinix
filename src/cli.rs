/// Nix focused structs and impl, e.g. packages, environments, etc.
use clinix::env::args::*;

/// The parent commands
mod cmd {
	pub enum SysCmd {
		#[command(flatten)]
		Env(EnvCmd),
		SetDefault(args::Name),
	}

	pub enum UserCmd {
			#[command(flatten)] Env(EnvCmd),
			SetDefault(args::Name),
			Link(args::Link),
			Adopt(args::Adopt),
			Unlink(args::Link),
			Status, // TODO Need to create this
	}

	pub enum InfoCmd {
		Deps(args::Deps),
		Env(args::EnvAudit),
		Share(args::Share),
	}

}

mod scopes {
	// reuse as-is
	pub struct args::Run { #[command(subcommand)] pub cmd: EnvCmd }        

	// reuse + launcher
	#[command(args_conflicts_with_subcommands = true)]
	pub struct args::Dev {                                                 
		// None ⇒ launcher
		#[command(subcommand)]
		pub cmd: Option<EnvCmd>,                  
		pub names: Vec<String>,
		#[arg(short='r', long)]
		runtime: bool,
		#[arg(short='w', long="with")]
		with: Vec<String>,
		#[arg(long)]
		list: bool,
		#[arg(long)]
		pure: bool,
		#[arg(long)]
		run: Option<String>,
		#[arg(short='c', long)]
		cache: bool,
	}

	pub struct args::Sys {
		#[command(subcommand)]
		pub cmd: SysCmd,
	}

	pub struct args::User {
		#[command(subcommand)]
		pub cmd: UserCmd,
	}

	pub struct args::Info { #[command(subcommand)] pub cmd: InfoCmd }
}

pub enum Command {
	Sys(args::Sys),
	User(args::User),
	Dev(args::Dev),
	Run(args::Run),
	Info(args::Info),
}

// name="clinix", version, propagate_version
#[derive(Parser)]                       
pub struct Cli {
		// -v; -V is version (clap default)
    #[arg(short='v', long, global=true, action=Count)] verbose: u8,
    #[command(subcommand)] pub command: Command,
}
