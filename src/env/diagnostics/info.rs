//! `info`: summarize a single env — its nixpkgs pin and every package's
//! **resolved version**.

use clap::Args;

use crate::env::project::{LockTarget, Project};
use crate::env::{Context, RunCmd, resolve};
use crate::error::Result;

use super::{kind_str, nixpkgs_pin};

/// Summarize an env: its nixpkgs pin and every package's **resolved version**.
/// Versions are computed on demand from the pinned nixpkgs via classic
/// `nix-instantiate --eval` — clinix owns no version-list file (plan decision 4 /
/// `version-pinning.md` D2). `--json` emits the machine-readable form.
#[derive(Args, Debug)]
#[command(
	after_help = "See `clinix env list` for the environment registry (registered envs + seeds)."
)]
pub struct Info {
	/// Env to summarize (name, path, or `.`), or a lockfile by path; defaults to
	/// the cwd project. A lockfile reports its pinned inputs only.
	#[arg(conflicts_with = "lock")]
	pub name: Option<String>,
	/// A named lock from `[env.locks]` (or `default`): report its inputs.
	#[arg(long, value_name = "NAME")]
	pub lock: Option<String>,
	/// Emit machine-readable JSON instead of a table.
	#[arg(long)]
	pub json: bool,
}
impl RunCmd for Info {
	fn run(self, context: &Context) -> Result<()> {
		// A lockfile has no packages to resolve — report its inputs.
		if let Some(name) = &self.lock {
			let path = crate::env::env::lock_by_name(&context.config, name)?;
			return print_lockfile(LockTarget::file(&path)?, self.json);
		}
		if let Some(name) = self.name.as_deref()
			&& std::path::Path::new(name).is_file()
		{
			return print_lockfile(LockTarget::resolve(&context.config, name)?, self.json);
		}
		// `Project::load` names the case when there is no lock (see `LockSource`).
		let project = Project::load(resolve(&context.config, self.name.as_deref())?)?;
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

/// `info` on a bare lockfile: its path and every pinned input (the `sys info`
/// table), or `{ lock, inputs }` JSON.
fn print_lockfile(target: LockTarget, json: bool) -> Result<()> {
	let LockTarget::File { path, lock } = target else {
		unreachable!("an existing non-dir path resolves to a lockfile or errors");
	};
	if json {
		let inputs: Vec<_> = crate::relock::root_input_names(&lock)
			.into_iter()
			.filter_map(|name| {
				let node = lock.nodes.get(&name)?;
				Some(serde_json::json!({
					"name": name,
					"rev": node.locked.as_ref().and_then(|l| l.rev()),
					"tracking": node.original.as_ref().and_then(|o| o.git_ref()),
				}))
			})
			.collect();
		let out = serde_json::json!({ "lock": path.display().to_string(), "inputs": inputs });
		println!("{}", serde_json::to_string_pretty(&out)?);
	} else {
		println!("lock: {}\n", path.display());
		crate::relock::show_pins(&lock);
	}
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
