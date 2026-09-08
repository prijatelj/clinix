//! `info`: summarize a single env — its nixpkgs pin and every package's
//! **resolved version**.

use clap::Args;

use crate::env::project::Project;
use crate::env::{Context, RunCmd, resolve};
use crate::error::{ClinixError, Result};

use super::{kind_str, nixpkgs_pin};

/// Summarize an env: its nixpkgs pin and every package's **resolved version**.
/// Versions are computed on demand from the pinned nixpkgs via classic
/// `nix-instantiate --eval` — clinix owns no version-list file (plan decision 4 /
/// `version-pinning.md` D2). `--json` emits the machine-readable form.
#[derive(Args, Debug)]
#[command(after_help = "See `clinix env list` for the environment registry (registered envs + seeds).")]
pub struct Info {
	/// Env to summarize (name, path, or `.`); defaults to the cwd project.
	pub name: Option<String>,
	/// Emit machine-readable JSON instead of a table.
	#[arg(long)]
	pub json: bool,
}
impl RunCmd for Info {
	fn run(self, context: &Context) -> Result<()> {
		let env = resolve(&context.config, self.name.as_deref())?;
		// A resolved directory with neither a `shell.nix` nor a `flake.lock` is not
		// a clinix environment — surface that plainly instead of the raw io error
		// `Project::load` would otherwise raise reading the absent `shell.nix`.
		if !env.root.join("shell.nix").is_file() && !env.root.join("flake.lock").is_file() {
			return Err(ClinixError::Resolve(format!(
				"no valid shell.nix in {} — this directory is not a clinix environment. \
				 See `clinix env info --help` for more information.",
				env.root.display()
			)));
		}
		let project = Project::load(env)?;
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

fn print_table(project: &Project, packages: &[ResolvedPkg]) {
	println!(
		"env: {} ({})",
		project.env.root.display(),
		kind_str(project.env.kind)
	);
	if let Some((track, rev)) = nixpkgs_pin(project) {
		println!("nixpkgs: {track} @ {:.9}", rev);
	}
	println!();
	let width = packages
		.iter()
		.map(|p| p.name.len())
		.max()
		.unwrap_or(7)
		.max(7);
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
