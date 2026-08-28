//! `clean`: release an env's GC root so its store paths can be collected.

use crate::env::{Context, Target};
use crate::error::{Result, unimplemented};

/// Release an env's GC root (removes `state/roots/<slug>`), letting
/// `nix-collect-garbage` reap its store paths.
pub fn clean(_target: Target, _context: &Context) -> Result<()> {
	Err(unimplemented("env clean", "plan phase 5: remove GC root"))
}
