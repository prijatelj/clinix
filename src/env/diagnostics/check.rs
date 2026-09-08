//! `check` (the former `envcheck`): shell + lock + PATH-shadow audit — answers
//! "is the tool I run the one I pinned, or did it leak from the host?".

use crate::env::project::Project;
use crate::env::{Context, OptionalTarget, resolve};
use crate::error::Result;

use super::{ShellKind, audit_path, detect_shell, kind_str};

/// Environment/PATH audit (the former `envcheck`); defaults to the cwd project.
/// Answers "is the tool I run the one I pinned, or did it leak from the host?" —
/// the failure mode on non-NixOS where `/usr/bin` sits on `PATH` and shadows a
/// store binary. Offline: inspects env vars, `$PATH`, and the env's `flake.lock`;
/// no nix invocation.
pub fn check(target: OptionalTarget, context: &Context) -> Result<()> {
	let env = resolve(&context.config, target.name.as_deref())?;

	println!("== shell");
	match detect_shell() {
		ShellKind::NixShell(v) => println!("  kind        nix-shell (IN_NIX_SHELL={v})"),
		ShellKind::Develop => println!("  kind        nix develop / store-provided PATH"),
		ShellKind::NotNix => println!("  kind        NOT a nix shell — nothing below is pinned"),
	}
	println!(
		"  derivation  {}",
		std::env::var("name").unwrap_or_else(|_| "<unset>".into())
	);
	println!("  env         {} ({})", env.root.display(), kind_str(env.kind));

	println!("\n== pinned sources (flake.lock)");
	match Project::load(env.clone()) {
		Ok(project) => print_pinned_sources(&project),
		Err(_) => println!("  -           no readable flake.lock (run: clinix env init)"),
	}

	println!("\n== PATH");
	let path = std::env::var("PATH").unwrap_or_default();
	let audit = audit_path(&path);
	println!("  store entries  {}", audit.store);
	println!("  host entries   {}", audit.host);
	if audit.store == 0 {
		println!("  note           nothing from the store on PATH — not in a dev shell");
	} else if audit.shadowing.is_empty() {
		println!("  note           all host dirs come after the store entries — none can shadow");
	} else {
		println!("  WARNING        host dirs BEFORE a store dir — these shadow pinned tools:");
		for dir in &audit.shadowing {
			println!("                   {dir}");
		}
	}

	println!("\n== store");
	println!(
		"  NIX_STORE   {}",
		std::env::var("NIX_STORE").unwrap_or_else(|_| "<unset>".into())
	);
	Ok(())
}

/// The `flake.lock` root inputs as a `NAME / TYPE / REV / TRACKING` table (the
/// `envcheck` lock section). `TRACKING` is `frozen` for a rev-pinned `original`,
/// else the tracked branch/tag ref.
fn print_pinned_sources(project: &Project) {
	let Some(root) = project.lock.root_node() else {
		println!("  -           lock has no root node");
		return;
	};
	if root.inputs.is_empty() {
		println!("  -           no inputs");
		return;
	}
	println!("  {:<12} {:<8} {:<10} TRACKING", "NAME", "TYPE", "REV");
	for (name, input) in &root.inputs {
		let node = input.node_key().and_then(|k| project.lock.nodes.get(k));
		let (ty, rev, track) = match node.and_then(|n| n.locked.as_ref()) {
			Some(locked) => {
				let ty = locked.source_type().unwrap_or("-").to_string();
				let rev = locked
					.rev()
					.map(|r| r.chars().take(9).collect())
					.unwrap_or_else(|| "-".to_string());
				let track = node
					.and_then(|n| n.original.as_ref())
					.map(|o| {
						if o.rev().is_some() {
							"frozen".to_string()
						} else {
							o.git_ref().unwrap_or("-").to_string()
						}
					})
					.unwrap_or_else(|| "-".to_string());
				(ty, rev, track)
			}
			None => ("-".to_string(), "-".to_string(), "-".to_string()),
		};
		println!("  {name:<12} {ty:<8} {rev:<10} {track}");
	}
}
