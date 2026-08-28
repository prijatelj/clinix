//! Diagnostics: `list`, `info`, `deps`, `shared`, `check`. Read-only reports over
//! resolved envs; the env-name selectors live in [`crate::env`].

use crate::env::{Context, OptionalTarget, Target, Targets};
use crate::error::{Result, unimplemented};

/// List registered envs.
pub fn list(_context: &Context) -> Result<()> {
	Err(unimplemented("env list", "plan phase 5: enumerate registry"))
}

/// Summarize a single env (packages, pins, diagnostics).
pub fn info(_target: Target, _context: &Context) -> Result<()> {
	Err(unimplemented("env info", "plan phase 6: single-env summary"))
}

/// Dependency/closure report for an env.
pub fn deps(_target: Target, _context: &Context) -> Result<()> {
	Err(unimplemented("env deps", "plan phase 6: closure report"))
}

/// N-way shared-package comparison across several envs.
pub fn shared(_targets: Targets, _context: &Context) -> Result<()> {
	Err(unimplemented(
		"env shared",
		"plan phase 6: N-way shared-set comparison",
	))
}

/// Environment/PATH audit (the former `envcheck`); defaults to the cwd project.
pub fn check(_target: OptionalTarget, _context: &Context) -> Result<()> {
	Err(unimplemented(
		"env check",
		"plan phase 6: env/PATH audit (envcheck)",
	))
}
