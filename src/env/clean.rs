//! `clean`: release an env's GC root so its store paths can be collected.

use crate::env::registry;
use crate::env::{Context, Target, resolve};
use crate::error::Result;

/// Release an env's GC root (removes `state/roots/<key>`), letting
/// `nix-collect-garbage` reap its store paths. The env's `shell.nix`/`flake.lock`
/// are untouched — only the root symlink is removed. Reports whether a root was
/// present (a no-op clean is not an error).
pub fn clean(target: Target, context: &Context) -> Result<()> {
	let env = resolve(&context.config, Some(&target.name))?;
	if registry::clean(&context.config, &env)? {
		println!("clinix: released GC root for `{}`", target.name);
	} else {
		println!("clinix: `{}` had no GC root (nothing to release)", target.name);
	}
	Ok(())
}
