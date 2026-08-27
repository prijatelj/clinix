/// The verb subcommands used by each parent command.

/// Each environment command's shared verb subcommands
#[derive(Subcommand)]
enum EnvCmd {
	Init(..),
	Rename(RenameArgs),
	Info(InfoArgs),
	Add(PkgArgs),
	Remove(PkgArgs),
	Pin(PinArgs),
	Unpin(PinArgs),
	Update(UpdateArgs),
	Import(..),
	Export(..),
}


pub fn init  (env: &mut Env, a: &InitArgs)   -> Result<()> { Err(nyi("init",   "plan §3")) }
pub fn add   (env: &mut Env, a: &PkgArgs)    -> Result<()> { Err(nyi("add",    "plan §3")) }
pub fn remove(env: &mut Env, a: &PkgArgs)    -> Result<()> { … }
pub fn pin   (env: &mut Env, a: &PinArgs)    -> Result<()> { … }   // --all ⇒ closure freeze
pub fn unpin (env: &mut Env, a: &PinArgs)    -> Result<()> { … }
pub fn update(env: &mut Env, a: &UpdateArgs) -> Result<()> { … }
pub fn import(env: &mut Env, a: &ImportArgs) -> Result<()> { … }
pub fn rename(env: &mut Env, a: &RenameArgs) -> Result<()> { … }   // relabel
pub fn set_default(scope: Scope, a: &NameArgs) -> Result<()> { … } // switch active

pub fn export_docker (env: &mut Env, a: &DockerArgs)  -> Result<()> { … }
pub fn export_closure(env: &mut Env, a: &ClosureArgs) -> Result<()> { … }

// info is cross-scope: resolves its own targets, not a single Env
pub fn info_deps (a: &DepsArgs)     -> Result<()> { … }
pub fn info_env  (a: &EnvAuditArgs) -> Result<()> { … }
pub fn info_share(a: &ShareArgs)    -> Result<()> { … }

// dev launcher (None-subcommand case)
pub fn launch(env: &Env, /* launcher flags */) -> Result<()> { … }
