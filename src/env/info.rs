//! Diagnostics: `info` (resolved-version summary, implemented) plus `list` /
//! `deps` / `shared` / `check` (stubs). Read-only reports over resolved envs; the
//! env-name selectors live in [`crate::env`].

use clap::Args;

use crate::env::project::Project;
use crate::env::{Context, Kind, OptionalTarget, RunCmd, Target, Targets, resolve};
use crate::error::{Result, unimplemented};

/// Summarize an env: its nixpkgs pin and every package's **resolved version**.
/// Versions are computed on demand from the pinned nixpkgs via classic
/// `nix-instantiate --eval` — clinix owns no version-list file (plan decision 4 /
/// `version-pinning.md` D2). `--json` emits the machine-readable form.
#[derive(Args, Debug)]
pub struct Info {
	/// Env to summarize (name, path, or `.`); defaults to the cwd project.
	pub name: Option<String>,
	/// Emit machine-readable JSON instead of a table.
	#[arg(long)]
	pub json: bool,
}
impl RunCmd for Info {
	fn run(self, _context: &Context) -> Result<()> {
		let project = Project::load(resolve(self.name.as_deref())?)?;
		let packages = resolved_packages(&project)?;
		if self.json {
			print_json(&project, &packages)
		} else {
			print_table(&project, &packages);
			Ok(())
		}
	}
}

#[derive(serde::Deserialize)]
struct ResolvedPkg {
	name: String,
	version: String,
}

/// Evaluate the env's `shell.nix` and read each package's `{pname, version}` from
/// its `nativeBuildInputs` (where `mkShell` places `packages`). Classic eval — no
/// experimental `nix`; the versions come from the nixpkgs rev `flake.lock` pins.
fn resolved_packages(project: &Project) -> Result<Vec<ResolvedPkg>> {
	let shell_nix = project.env.root.join("shell.nix");
	let expr = format!(
		"let s = import {} {{}}; in map (p: {{ name = p.pname or p.name or \"\"; \
		 version = p.version or \"\"; }}) (s.nativeBuildInputs or s.buildInputs or [])",
		shell_nix.display()
	);
	Ok(serde_json::from_value(crate::nix::eval_json(&expr)?)?)
}

/// The nixpkgs input's `(tracked-ref-or-"(frozen)", rev)` from `flake.lock`.
fn nixpkgs_pin(project: &Project) -> Option<(String, String)> {
	let node = project.lock.nodes.get("nixpkgs")?;
	let rev = node.locked.as_ref()?.rev()?.to_string();
	let track = node
		.original
		.as_ref()
		.and_then(|o| o.git_ref())
		.map(str::to_string)
		.unwrap_or_else(|| "(frozen)".to_string());
	Some((track, rev))
}

fn print_table(project: &Project, packages: &[ResolvedPkg]) {
	let kind = match project.env.kind {
		Kind::Project => "project",
		Kind::Registry => "registry",
	};
	println!("env: {} ({kind})", project.env.root.display());
	if let Some((track, rev)) = nixpkgs_pin(project) {
		println!("nixpkgs: {track} @ {:.9}", rev);
	}
	println!();
	let width = packages.iter().map(|p| p.name.len()).max().unwrap_or(7).max(7);
	println!("{:<width$}  VERSION", "PACKAGE");
	for p in packages {
		println!("{:<width$}  {}", p.name, p.version);
	}
}

fn print_json(project: &Project, packages: &[ResolvedPkg]) -> Result<()> {
	let versions: std::collections::BTreeMap<&str, &str> = packages
		.iter()
		.map(|p| (p.name.as_str(), p.version.as_str()))
		.collect();
	let out = serde_json::json!({
		"root": project.env.root.display().to_string(),
		"nixpkgs": nixpkgs_pin(project).map(|(_, rev)| rev),
		"packages": versions,
	});
	println!("{}", serde_json::to_string_pretty(&out)?);
	Ok(())
}

/// List registered envs.
pub fn list(_context: &Context) -> Result<()> {
	Err(unimplemented("env list", "plan phase 5: enumerate registry"))
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_eval_json_into_resolved_packages() {
		let json = r#"[{"name":"ripgrep","version":"15.1.0"},{"name":"jq","version":"1.8.2"}]"#;
		let value: serde_json::Value = serde_json::from_str(json).unwrap();
		let pkgs: Vec<ResolvedPkg> = serde_json::from_value(value).unwrap();
		assert_eq!(pkgs.len(), 2);
		assert_eq!(pkgs[0].name, "ripgrep");
		assert_eq!(pkgs[1].version, "1.8.2");
	}
}
