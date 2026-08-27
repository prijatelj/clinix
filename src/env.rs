/// Environments managed by the CLI with general Commands & their Args
use std::path::{Path, PathBuf};

// pub enum Scope { Sys, User, Dev, Run }


pub struct Pkg {
	name: String,
	version: Option<String>, // TODO Look up how rust cargo typically represents these.
}

/// Environment Identifier
pub struct Id {
	//pub scope: Scope,
	pub root: PathBuf,
	pub name: Option<String>
}

// The ONLY per-scope-divergent code. Phase 1: shallow + pure — no nix, no lock read.
//   Run  → cwd project;  Dev/User → state dir shells/<name>;  Sys → system config path.
//   User resolves to its pointed dev-shell (the "user = dev + login" collapse).
pub fn resolve(
	//scope: Scope,
	name: Option<&str>,
	cwd: &Path,
) -> Result<Env> {

}

mod args {
	#[derive(clap::Args)]
	pub struct Init {
		name: Option<String>,
		//channel: Option<String>, // TODO We do not want to support channel use! We use system.nix!

		packages: Vec<Pkg>, // -p
		from: Option<PathBuf>, // --from
	}

	#[derive(clap::Args)]
	pub struct Pkg { packages: Vec<Pkg> }

	#[derive(clap::Args)]
	pub struct Pin {
		packages: Vec<Pkg>,
		all: bool, // --all
	}

	#[derive(clap::Args)]
	pub struct Update {
		/// If names is empty, then all.
		names: Vec<String>,
	}

	#[derive(clap::Args)]
	pub struct Import { source: PathBuf }

	#[derive(clap::Args)]
	pub struct Rename {
		old: String,
		new: String,
	}

	#[derive(clap::Args)]
	pub struct Name { name: String }

	#[derive(clap::Args)]
	pub struct Docker { target: EnvId }

	#[derive(clap::Args)]
	pub struct Closure { out_dir: PathBuf }

	#[derive(clap::Args)]
	pub struct Deps {
		target: EnvId,
		size: bool, // TODO Why a bool? What does size mean here?
	}

	#[derive(clap::Args)]
	pub struct EnvAudit {
		target: Option<EnvId>,
		tools: Vec<String>,
	}

	#[derive(clap::Args)]
	pub struct Share { targets: Vec<EnvId> }

	#[derive(clap::Args)]
	pub struct Link { packages: Vec<Pkg> }

	#[derive(clap::Args)]
	pub struct Adopt { path: PathBuf }

	//pub struct Info { #[command(subcommand)] pub cmd: InfoCmd }
}

mod cmd {
	pub enum Export {
		Docker(args::Docker),
		Closure(args::Closure),
	}

	#[derive(Subcommand)]
	pub enum Env {
			Init(args::Init),
			Rename(args::Rename),
			Add(args::Pkg),
			Remove(args::Pkg),
			Pin(args::Pin),
			Unpin(args::Pin),
			Update(args::Update),
			Import(args::Import),
			Export {
				#[command(subcommand)]
				target: cmd::Export
			},
			Info {
				#[command(subcommand)]
				target: Info
			},
	}
	pub enum Info {
		Deps(args::Deps),
		Env(args::EnvAudit),
		Share(args::Share),
		// security analysis with at leaast vulnix and other tools from Kali Linux.
	}
}
