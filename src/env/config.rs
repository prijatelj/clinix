//! Configuration & path resolution, resolvable **from anywhere**.
//!
//! Both roots — config and state — resolve by one precedence chain (plan §Config,
//! state & the env registry):
//!
//! > **CLI flag > direct env var (`CLINIX_*`) > XDG base var + `clinix` > default**
//!
//! Config is resolved **once** in [`crate::cli`] and carried in
//! [`crate::env::Context`], so every consumer reads the same values and the state
//! is explicit rather than a hidden global. The two roots follow the classic XDG
//! split (as cargo/git/uv do): config is user-authored and portable, state is
//! machine-generated and disposable.
//!
//! Path resolution is hand-rolled (~2 helpers) rather than pulling `etcetera`:
//! the logic is a small, well-understood XDG lookup already present in the crate,
//! and avoiding the dependency keeps the single-static-binary property. The plan
//! lists `etcetera` as an option; this is the deliberate minimal alternative.

use std::path::PathBuf;

/// CLI-flag overrides for the two roots (from [`crate::cli::Cli`]). Each is the
/// highest-precedence source for its root when set.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
	/// `--config <dir>`: points directly at clinix's config dir.
	pub config_dir: Option<PathBuf>,
	/// `--state-dir <dir>`: points directly at clinix's state dir.
	pub state_dir: Option<PathBuf>,
}

/// Resolved configuration: the two clinix roots. Portable `config.toml` settings
/// (default nixpkgs channel, version-index provider, default env) are read lazily
/// when a verb first needs one; this slice (the registry) needs only the roots.
#[derive(Debug, Clone)]
pub struct Config {
	/// `$XDG_CONFIG_HOME/clinix` — portable, user-authored config.
	pub config_dir: PathBuf,
	/// `$XDG_STATE_HOME/clinix` — machine-generated, disposable state.
	pub state_dir: PathBuf,
}

impl Config {
	/// Resolve both roots by the precedence chain. Infallible: it only inspects
	/// env vars and joins paths (no I/O), so it never fails — directories are
	/// created lazily by the verbs that write into them.
	pub fn resolve(overrides: &Overrides) -> Self {
		Config {
			config_dir: resolve_root(
				overrides.config_dir.clone(),
				"CLINIX_CONFIG_DIR",
				"XDG_CONFIG_HOME",
				".config",
			),
			state_dir: resolve_root(
				overrides.state_dir.clone(),
				"CLINIX_STATE_DIR",
				"XDG_STATE_HOME",
				".local/state",
			),
		}
	}
}

/// Resolve one root: `flag` → `$CLINIX_*` (direct) → `$XDG_*`/clinix (base) →
/// `$HOME/<home_rel>/clinix` → a cwd-local fallback. A direct override
/// (`flag`/`CLINIX_*`) *is* the clinix dir; an XDG/HOME base gets `clinix`
/// appended. Empty env vars are ignored (treated as unset, per the XDG spec).
fn resolve_root(
	flag: Option<PathBuf>,
	direct_var: &str,
	xdg_var: &str,
	home_rel: &str,
) -> PathBuf {
	if let Some(dir) = flag {
		return dir;
	}
	if let Some(dir) = env_path(direct_var) {
		return dir;
	}
	if let Some(base) = env_path(xdg_var) {
		return base.join("clinix");
	}
	if let Some(home) = env_path("HOME") {
		return home.join(home_rel).join("clinix");
	}
	PathBuf::from(".clinix").join(home_rel)
}

/// A non-empty environment variable as a path, or `None` (unset or empty).
fn env_path(var: &str) -> Option<PathBuf> {
	std::env::var_os(var)
		.filter(|v| !v.is_empty())
		.map(PathBuf::from)
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::Path;

	// A CLI flag is the top of the precedence chain and needs no env, so this is
	// deterministic. The XDG-var and default layers mutate process-global env, so
	// they are exercised by the L3 integration tests (which set `XDG_STATE_HOME`
	// per test) instead — see `notes/clinix/design/testing.md` §2.
	#[test]
	fn flag_overrides_everything() {
		let cfg = Config::resolve(&Overrides {
			config_dir: Some(PathBuf::from("/tmp/explicit-config")),
			state_dir: Some(PathBuf::from("/tmp/explicit-state")),
		});
		assert_eq!(cfg.config_dir, Path::new("/tmp/explicit-config"));
		assert_eq!(cfg.state_dir, Path::new("/tmp/explicit-state"));
	}
}
