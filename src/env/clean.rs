//! `clean`: release envs' GC roots so their store paths can be collected.

use crate::env::config::Settings;
use crate::env::seeds::Catalog;
use crate::env::{Context, Targets, node_root, registry};
use crate::error::{ClinixError, Result};

/// Release each named target's GC roots (`state/roots/<key>` **and** its `.rt`
/// sibling), letting `nix-collect-garbage` reap the store paths. Each name is
/// resolved **independently** — exactly as a single-name `shell <name>` would key
/// its root — so `clean a b c` frees `a`, `b`, and `c` in turn (registry/project
/// envs, `*.nix` files, and single seeds alike). It does **not** touch a
/// `stack-a_b_c` union root; that is only created by launching that exact union.
///
/// The env's `shell.nix`/`flake.lock` are untouched — only the root symlinks are
/// removed. A no-op release (no root present) is reported, not an error. Unknown
/// names are reported per-name and collected; if any name failed to resolve, the
/// command exits with an error after processing the rest.
pub fn clean(targets: Targets, context: &Context) -> Result<()> {
	let cfg = &context.config;
	let settings = Settings::load(&cfg.config_dir)?;
	let catalog = Catalog::build(&settings.env.seeds);
	for w in &catalog.warnings {
		eprintln!("clinix: {w}");
	}

	let mut unresolved: Vec<String> = Vec::new();
	for name in &targets.names {
		let root = match node_root(cfg, &catalog, name) {
			Ok(root) => root,
			Err(e) => {
				eprintln!("clinix: `{name}` — {e}");
				unresolved.push(name.clone());
				continue;
			}
		};
		if registry::release_root(&root)? {
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
