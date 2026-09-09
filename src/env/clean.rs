//! `clean`: release envs' GC roots so their store paths can be collected.

use clap::Args;

use crate::env::config::Settings;
use crate::env::seeds::Catalog;
use crate::env::{Context, node_root, registry};
use crate::error::{ClinixError, Result};

/// `clean` arguments: which envs to release, and optionally *which versions*.
///
/// With no version flag, **all** versions of each named env are released. The version
/// flags target a **single** env's version history (they require exactly one name):
/// `--root-version <SEQ>` releases one exact version (the id shown by `env roots`),
/// `--oldest <N>` releases the N oldest versions (keeping the newer ones).
#[derive(Args, Debug)]
pub struct Clean {
	/// Envs whose GC roots to release, each resolved independently (a registry/project
	/// env, a `*.nix` file, or a single seed). Not a `stack-a_b_c` union.
	#[arg(required = true)]
	pub names: Vec<String>,
	/// Release only the exact root version with this id (see `env roots <name>`).
	/// Requires exactly one name.
	#[arg(long, conflicts_with = "oldest")]
	pub root_version: Option<u64>,
	/// Release only the `N` **oldest** versions of the env, keeping the newer ones.
	/// Requires exactly one name.
	#[arg(long, conflicts_with = "root_version")]
	pub oldest: Option<usize>,
}

/// Release GC roots so `nix-collect-garbage` can reap the store paths. Each name is
/// resolved **independently** — exactly as a single-name `shell <name>` would key its
/// root. Whole-env (no flag) releases every version; `--root-version`/`--oldest` do
/// version-targeted removal on a single env. Only the root symlinks are touched — the
/// env's `shell.nix`/`flake.lock` are untouched. Unknown names are reported per-name;
/// if any failed to resolve, the command exits with an error after processing the rest.
pub fn clean(args: Clean, context: &Context) -> Result<()> {
	let cfg = &context.config;
	let settings = Settings::load(&cfg.config_dir)?;
	let catalog = Catalog::build(&settings.env.seeds);
	for w in &catalog.warnings {
		eprintln!("clinix: {w}");
	}

	let version_targeted = args.root_version.is_some() || args.oldest.is_some();
	if version_targeted && args.names.len() != 1 {
		return Err(ClinixError::Config(
			"`--root-version` / `--oldest` target a single env's versions — pass exactly one name"
				.to_string(),
		));
	}

	let mut unresolved: Vec<String> = Vec::new();
	for name in &args.names {
		let base = match node_root(cfg, &catalog, name) {
			Ok(base) => base,
			Err(e) => {
				eprintln!("clinix: `{name}` — {e}");
				unresolved.push(name.clone());
				continue;
			}
		};

		if let Some(seq) = args.root_version {
			if registry::release_version(&base, seq)? {
				println!("clinix: released `{name}` root version @{seq}");
			} else {
				println!("clinix: `{name}` has no root version @{seq}");
			}
		} else if let Some(n) = args.oldest {
			let removed = registry::release_oldest(&base, n)?;
			if removed.is_empty() {
				println!("clinix: `{name}` had no versions to release");
			} else {
				let ids: Vec<String> = removed.iter().map(|s| format!("@{s}")).collect();
				println!(
					"clinix: released {} oldest version(s) of `{name}`: {}",
					removed.len(),
					ids.join(" ")
				);
			}
		} else if registry::release_root(&base)? {
			println!("clinix: released GC root(s) for `{name}`");
		} else {
			println!("clinix: `{name}` had no GC root (nothing to release)");
		}
	}

	if unresolved.is_empty() {
		Ok(())
	} else {
		Err(ClinixError::UnknownEnv(unresolved.join(", ")))
	}
}
