//! **Composition**: turning a list of resolved targets into one instantiable nix
//! expression, and entering it.
//!
//! The pipeline `shell`, `run`, and `export` all share, and the single place the
//! rules about *combining* environments live:
//!
//! 1. resolve each token to a [`Node`] ([`crate::env::resolve_node`]);
//! 2. decide the pin — a self-contained member's own `flake.lock`, else the seed
//!    pin — and reject a union whose members disagree;
//! 3. assemble the expression ([`imports_frag`] over
//!    [`crate::env::nix_expr::ComposeSpec`]);
//! 4. GC-root it as a new version when the derivation changed ([`mint_current`]),
//!    or skip rooting entirely for an ephemeral ad-hoc `-p` shell;
//! 5. enter it via classic `nix-shell` ([`launch`]).
//!
//! Split out of [`super::env`] because composition is a distinct concern from
//! *resolution* (name → directory) and from verb dispatch. Everything a
//! composition needs about the request — including the ad-hoc `-p` package set —
//! arrives on [`Context`], so no caller threads it through.

use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use crate::env::config::Config;
use crate::env::registry;
// Resolution lives next door; composition consumes it (see the module docs).
use super::env::{
	Context, Env, Node, env_label, env_shell_file, file_label, named_pins, nixpkgs_config_path,
	node_label, resolve, resolve_node, seed_lock,
};
use crate::error::{ClinixError, Result};

/// A resolved composition: the single nix file to instantiate (an env's own
/// shell, a `*.nix` file, or a written seed-stack compose file), a human `label`,
/// and the GC-root path keyed for it. Produced by [`compose_nodes`] and consumed
/// by both `launch` (enter it) and `export` (serialize/image it).
pub(crate) struct Composition {
	pub shell_file: PathBuf,
	pub label: String,
	pub root: PathBuf,
	/// The `flake.lock` pinning this composition's nixpkgs, when discoverable (a
	/// registry env's own lock, a file target's adjacent lock, or the seed pin for
	/// a stack). `export docker` needs it to pin `dockerTools`.
	pub lock: Option<PathBuf>,
	/// True when ad-hoc `-p` packages made this composition **ephemeral**: it is
	/// entered unrooted (like plain `nix-shell`), so `nix-collect-garbage` reclaims
	/// it. Register it with `env new <name> --from <envs…> -p <pkgs…>` to keep it.
	pub ephemeral: bool,
}

/// A directory-backed env's own `flake.lock`, if present.
fn env_lock(env: &Env) -> Option<PathBuf> {
	let lock = env.root.join("flake.lock");
	lock.is_file().then_some(lock)
}

/// Resolve a name list to a single instantiable [`Composition`] — the one place
/// the target precedence + seed-stack composition live, shared by `launch` and
/// `export`. Empty names ⇒ the cwd project. No nix eval or entry here; a stack
/// writes its compose expression to `state/compose/`. A union may mix a
/// self-contained env (project/registry/file) with seeds **only when all such
/// envs share a byte-identical `flake.lock`** (the shared-pin union); the general
/// multi-pin case stays deferred.
pub(crate) fn compose_nodes(ctx: &Context, names: &[String]) -> Result<Composition> {
	let cfg = &ctx.config;
	let settings = crate::env::config::Settings::load(&cfg.config_dir)?;
	let catalog = crate::env::seeds::Catalog::build(&settings.env.seeds);
	for w in &catalog.warnings {
		eprintln!("clinix: {w}");
	}

	// Ad-hoc `-p` packages are part of the composition *request*, which `Context`
	// already carries — so they are read here rather than threaded through every
	// caller. A caller that composes without them simply never set them.
	let packages: &[String] = &ctx.options.packages;
	// They must be spliced into a generated `mkShell`, so they force the compose
	// path below, including the 0-node and 1-node shapes that otherwise hand back
	// an env's own `shell.nix` untouched.
	let adhoc = !packages.is_empty();

	// Empty ⇒ the cwd project. With `-p` and no names this is instead the bare
	// `nix-shell -p` shape: no imports at all, pinned by whatever lock is active
	// here — the cwd project's if there is one, else the seed pin — so
	// `clinix -p tokei` works in any directory.
	if names.is_empty() {
		if adhoc {
			let lock = match resolve(cfg, None).ok().as_ref().and_then(env_lock) {
				Some(l) => l,
				None => seed_lock(cfg, &settings)?,
			};
			return adhoc_composition(cfg, &settings, &[], &[], lock, packages);
		}
		let env = resolve(cfg, None)?;
		return Ok(Composition {
			shell_file: env_shell_file(&env.root)?,
			label: env_label(&env),
			root: registry::root_path(cfg, &env),
			lock: env_lock(&env),
			ephemeral: false,
		});
	}

	let nodes: Vec<Node> = names
		.iter()
		.map(|n| resolve_node(cfg, &catalog, n))
		.collect::<Result<_>>()?;

	// A single self-contained node: an env's own shell, or an explicit `*.nix` file.
	// Skipped under `-p`, which needs a generated wrapper to carry the packages.
	if !adhoc {
		match nodes.as_slice() {
			[Node::Env(env)] => {
				return Ok(Composition {
					shell_file: env_shell_file(&env.root)?,
					label: env_label(env),
					root: registry::root_path(cfg, env),
					lock: env_lock(env),
					ephemeral: false,
				});
			}
			[Node::File { path }] => {
				let lock = path
					.parent()
					.map(|d| d.join("flake.lock"))
					.filter(|l| l.is_file());
				return Ok(Composition {
					shell_file: path.clone(),
					label: file_label(path),
					root: registry::file_root(cfg, path),
					lock,
					ephemeral: false,
				});
			}
			_ => {}
		}
	}

	// Otherwise a **union** of 2+ nodes. Seeds inject the shared `pkgs`;
	// self-contained nodes (project/registry env, `*.nix` file) read their own
	// `flake.lock` — permitted **only when all such locks are byte-identical** (a
	// shared pin), else the multi-pin case stays deferred.
	let mut items: Vec<UnionItem> = Vec::with_capacity(nodes.len());
	for node in &nodes {
		items.push(union_item(node)?);
	}

	// The shared pin: every self-contained node's lock must be identical; else the
	// seeds compose against the configured seed pin (an all-seeds union).
	let sc_locks: Vec<&PathBuf> = items.iter().filter_map(|i| i.lock.as_ref()).collect();
	let lock = match sc_locks.first() {
		Some(first) => {
			let first_bytes = std::fs::read(first)?;
			for other in &sc_locks[1..] {
				if std::fs::read(other)? != first_bytes {
					return Err(crate::error::unimplemented(
						"composing envs with differing flake.locks",
						"only a shared flake.lock is supported at this time — every env in a \
						 union must share one pin",
					));
				}
			}
			(*first).clone()
		}
		None => seed_lock(cfg, &settings)?,
	};

	// Order: lexical by default; `-o` preserves the given order.
	if !ctx.options.ordered {
		items.sort_by(|a, b| a.label.cmp(&b.label));
	}
	let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
	let members: Vec<Member> = items
		.iter()
		.map(|i| Member {
			path: i.shell.clone(),
			pkgs: i.inject,
			pins: i.wants_pins,
		})
		.collect();
	adhoc_composition(cfg, &settings, &labels, &members, lock, packages)
}

/// Write a generated compose expression and describe it as a [`Composition`]. The
/// single writer for every composed shape: a plain union (`packages` empty) and an
/// ad-hoc `-p` layer (which may have **no** imports at all — the `nix-shell -p`
/// case). Ad-hoc compositions are **ephemeral**: entered unrooted, so GC reclaims
/// them.
///
/// The compose file's name carries a hash of the package set, so distinct `-p`
/// invocations over the same envs do not overwrite each other's expression.
fn adhoc_composition(
	cfg: &Config,
	settings: &crate::env::config::Settings,
	labels: &[String],
	members: &[Member],
	lock: PathBuf,
	packages: &[String],
) -> Result<Composition> {
	let label = if labels.is_empty() {
		packages.join(" ")
	} else {
		labels.join(" ")
	};
	let nixpkgs_config = nixpkgs_config_path(settings)?;
	// Alternate pins are resolved (and lazily locked) **only** when a member asked
	// for them, so a declared-but-unused `[env.pins]` costs nothing — not even the
	// one-time network lock of a `Ref` pin.
	let pins = if members.iter().any(|m| m.pins) {
		let declared = named_pins(cfg, settings)?;
		if declared.is_empty() {
			eprintln!(
				"clinix: warning: `{label}` asks for `pins` but [env.pins] declares none — \
				 `pins.<name>` will fail to evaluate"
			);
		}
		declared
	} else {
		Vec::new()
	};
	let expr = crate::env::nix_expr::ComposeSpec {
		lock_ref: &crate::env::nix_expr::nix_str(&lock),
		config_frag: &crate::env::nix_expr::nixpkgs_config_frag(nixpkgs_config.as_deref()),
		name: &label.replace(' ', "-"),
		imports: &imports_frag(members),
		label: &label,
		packages,
		pins: &pins,
	}
	.render();

	// Key: the stack key, plus `-p<hash>` when ad-hoc so the two never collide.
	let base = if labels.is_empty() {
		"stack".to_string()
	} else {
		stack_key(labels.iter().map(String::as_str))
	};
	let key = if packages.is_empty() {
		base
	} else {
		format!("{base}-p{:08x}", package_hash(packages))
	};

	let compose_dir = registry::compose_dir(cfg);
	std::fs::create_dir_all(&compose_dir)?;
	let compose_file = compose_dir.join(format!("{key}.nix"));
	std::fs::write(&compose_file, expr)?;
	Ok(Composition {
		shell_file: compose_file,
		label,
		root: registry::roots_dir(cfg).join(&key),
		lock: Some(lock),
		ephemeral: !packages.is_empty(),
	})
}

/// A stable 32-bit FNV-1a hash of the package set, order-independent (the set is
/// sorted first) — the compose-file name's `-p<hash>` suffix. Hand-rolled rather
/// than `DefaultHasher` so the same `-p` set always names the same file, across
/// toolchain versions.
fn package_hash(packages: &[String]) -> u32 {
	let mut sorted: Vec<&str> = packages.iter().map(String::as_str).collect();
	sorted.sort_unstable();
	let mut hash: u32 = 0x811c_9dc5;
	for name in sorted {
		for b in name.as_bytes() {
			hash ^= u32::from(*b);
			hash = hash.wrapping_mul(0x0100_0193);
		}
		hash ^= u32::from(b'\0');
		hash = hash.wrapping_mul(0x0100_0193);
	}
	hash
}

/// One node's contribution to a union: the shell file to import, whether to inject
/// the shared `pkgs` (seeds do; self-contained shells read their own lock), and its
/// own `flake.lock` for the shared-pin check (self-contained only).
struct UnionItem {
	label: String,
	shell: PathBuf,
	inject: bool,
	/// The member asked for the declared `[env.pins]` set (seeds only).
	wants_pins: bool,
	lock: Option<PathBuf>,
}

fn union_item(node: &Node) -> Result<UnionItem> {
	Ok(match node {
		Node::Seed {
			name,
			path,
			wants_pins,
		} => UnionItem {
			label: name.clone(),
			shell: path.clone(),
			inject: true,
			wants_pins: *wants_pins,
			lock: None,
		},
		Node::Env(env) => {
			let lock = env_lock(env).ok_or_else(|| {
				ClinixError::Config(format!(
					"`{}` has no flake.lock — it cannot join a shared-flake.lock union",
					env_label(env)
				))
			})?;
			UnionItem {
				label: env_label(env),
				shell: env_shell_file(&env.root)?,
				inject: false,
				wants_pins: false,
				lock: Some(lock),
			}
		}
		Node::File { path } => {
			let lock = path
				.parent()
				.map(|d| d.join("flake.lock"))
				.filter(|l| l.is_file())
				.ok_or_else(|| {
					ClinixError::Config(format!(
						"`{}` has no adjacent flake.lock — it cannot join a shared-flake.lock union",
						path.display()
					))
				})?;
			UnionItem {
				label: file_label(path),
				shell: path.clone(),
				inject: false,
				wants_pins: false,
				lock: Some(lock),
			}
		}
	})
}

/// Shared launcher for `shell`/`run`: [`compose_nodes`] the names, GC-root, and
/// enter via classic `nix-shell` (`--run <command>` when non-interactive).
pub(crate) fn launch(
	ctx: &Context,
	names: &[String],
	pure: bool,
	command: Option<&str>,
	version: registry::VersionSelect,
) -> Result<ExitStatus> {
	let comp = compose_nodes(ctx, names)?;
	let drv = match version {
		// An ad-hoc `-p` composition is ephemeral by design: evaluate and enter it
		// unrooted, exactly as plain `nix-shell -p` does, so it mints no version and
		// `nix-collect-garbage` reclaims it once the shell exits.
		_ if comp.ephemeral => crate::nix::instantiate(&comp.shell_file)?,
		// Current: mint a new version iff the derivation changed, else reuse.
		registry::VersionSelect::Current => mint_current(ctx, &comp)?,
		// A prior/exact version: enter its stored `.drv` directly — no eval, no mint,
		// no prune (offline-capable, since the recipe + closure are already rooted).
		sel => {
			let vpath = registry::resolve_version(&comp.root, sel)?;
			std::fs::read_link(&vpath)?.to_string_lossy().into_owned()
		}
	};
	match command {
		// Interactive shell: hand the terminal off by *replacing* this process
		// (like `~/dev_env/shell`'s `exec nix-shell`), so no clinix node lingers
		// between the parent shell and the nix-shell. A lingering clinix process
		// otherwise hides the nix-shell layer from process-tree tools such as
		// `clonetty`. On success this does not return (see [`crate::nix::nix_shell_exec`]).
		//
		// A future post-shell cleanup hook would attach here: swap this arm back to
		// `nix::nix_shell(&drv, pure, None)` (fork + wait) and run the hook on the
		// returned `ExitStatus`. Kept out until such a hook has a defined purpose,
		// rather than shipping an unreachable stub.
		None => crate::nix::nix_shell_exec(&drv, pure),
		// Non-interactive `run`: fork + wait so `execution::run` can mirror the
		// command's exit code. No terminal is handed off, so there is no
		// process-tree concern and nothing to gain from `exec` here.
		Some(c) => crate::nix::nix_shell(&drv, pure, Some(c)),
	}
}

/// Root the current composition as a new version **iff its derivation differs** from
/// the current version (else reuse — idempotent re-entry does no work), then prune to
/// `keep_n_prior_roots + 1`. Returns the `.drv` to enter. Each minted version is a
/// `.drv` root (`registry::version_path`) plus its `inputDerivation` `.rt` sibling —
/// the complete build closure, retained regardless of `keep-outputs`.
fn mint_current(ctx: &Context, comp: &Composition) -> Result<String> {
	let settings = crate::env::config::Settings::load(&ctx.config.config_dir)?;
	let base = &comp.root;
	let base_key = base
		.file_name()
		.map(|n| n.to_string_lossy().into_owned())
		.unwrap_or_default();

	// Rooting may be disabled for this kind (negative project/stack retention): then
	// just evaluate and enter unrooted (plain `nix-shell`, GC-collectible).
	let Some(keep_total) = settings.env.root_retention(&base_key) else {
		return crate::nix::instantiate(&comp.shell_file);
	};

	// Reuse the current version when the derivation is unchanged.
	let versions = registry::list_versions(base)?;
	if let Some(cur) = versions.last() {
		let new_drv = crate::nix::instantiate(&comp.shell_file)?;
		if std::fs::read_link(&cur.drv_root)
			.map(|t| t == Path::new(&new_drv))
			.unwrap_or(false)
		{
			return Ok(new_drv);
		}
	}

	// Mint the next version (a real replacement, or the first entry).
	let vpath = registry::version_path(base, registry::next_seq(base)?);
	let drv = crate::nix::instantiate_rooted(&comp.shell_file, &vpath)?;
	if let Err(e) = crate::nix::root_input_closure(&comp.shell_file, &registry::rt_root(&vpath)) {
		eprintln!(
			"clinix: warning: could not root `{}`'s full closure ({e}); \
			 packages may be collected unless `keep-outputs = true`",
			comp.label
		);
	}
	registry::prune_versions(base, keep_total)?;
	Ok(drv)
}

/// The `stack-<labels>` key that composing `names` would produce (sorted unless
/// `ordered`), computed **offline** — label resolution only, no lock/eval/compose
/// write. Lets `clean --union` / `clean -o` target the exact stack root a
/// `shell`/`run` composition created.
pub(crate) fn resolve_stack_key(
	cfg: &Config,
	catalog: &crate::env::seeds::Catalog,
	names: &[String],
	ordered: bool,
) -> Result<String> {
	let mut labels: Vec<String> = names
		.iter()
		.map(|n| resolve_node(cfg, catalog, n).map(|node| node_label(&node)))
		.collect::<Result<_>>()?;
	if !ordered {
		labels.sort();
	}
	Ok(stack_key(labels.iter().map(String::as_str)))
}

/// The `stack-<labels>` GC-root key for a seed/union composition: each label
/// slugged and joined by `-`. One source of truth shared by [`compose_nodes`] (all
/// members) and [`node_root`] (a single seed → `stack-<label>`, since the join of
/// one element is itself).
fn stack_key<'a>(labels: impl Iterator<Item = &'a str>) -> String {
	format!(
		"stack-{}",
		labels
			.map(|l| crate::env::naming::slug(l, false, false, None))
			.collect::<Vec<_>>()
			.join("-")
	)
}

/// The GC-root path a single target *would* have been rooted under, computed with
/// **no side effects** (no compose-file write, no lazy locking, no network) — so
/// `clean` can locate an env/seed/file/project root offline. Mirrors
/// [`compose_nodes`]' single-target keying exactly: a registry/project env →
/// [`registry::root_path`], a `*.nix` file → [`registry::file_root`], a single seed
/// → `stack-<label>` (the union branch's key for one member).
pub(crate) fn node_root(
	cfg: &Config,
	catalog: &crate::env::seeds::Catalog,
	name: &str,
) -> Result<PathBuf> {
	Ok(match resolve_node(cfg, catalog, name)? {
		Node::Env(env) => registry::root_path(cfg, &env),
		Node::File { path } => registry::file_root(cfg, &path),
		Node::Seed { name, .. } => {
			registry::roots_dir(cfg).join(stack_key(std::iter::once(name.as_str())))
		}
	})
}

/// One member of a generated composition: the shell file to import, plus which of
/// the prelude's bindings to hand it.
///
/// Named fields rather than a tuple because what a member receives is now two
/// independent decisions, and a `(PathBuf, bool, bool)` at the call site says
/// nothing about which bool is which.
pub(crate) struct Member {
	pub path: PathBuf,
	/// Pass the stack's `pkgs` — a **seed** fragment (`{ pkgs }:`) takes it; a
	/// **self-contained** env shell computes its own from its own (byte-identical,
	/// shared) `flake.lock` and takes `{ }`, which is what makes mixing a project
	/// with seeds coherent under the shared-`flake.lock` union.
	pub pkgs: bool,
	/// Also pass the declared `pins` set — only for a seed whose argument pattern
	/// accepts it ([`crate::env::seeds::Seed::wants_pins`]).
	pub pins: bool,
}

/// The newline-joined `(import <file> { … })` lines for `@IMPORTS@`, in composition
/// order. Pure — the `*_frag` counterpart to [`crate::env::nix_expr::packages_frag`]
/// and [`crate::env::nix_expr::pins_frag`], so each slot of the generated file has
/// exactly one builder.
fn imports_frag(members: &[Member]) -> String {
	members
		.iter()
		.map(|m| {
			let args = match (m.pkgs, m.pins) {
				(true, true) => "{ inherit pkgs pins; }",
				(true, false) => "{ inherit pkgs; }",
				(false, true) => "{ inherit pins; }",
				(false, false) => "{ }",
			};
			format!(
				"    (import {} {args})",
				crate::env::nix_expr::nix_str(&m.path)
			)
		})
		.collect::<Vec<_>>()
		.join("\n")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn stack_key_joins_slugged_labels_and_a_single_label_is_itself() {
		// One source of truth for the union key and node_root's single-seed key: the
		// join of one element equals that element, so a lone seed keys `stack-<label>`.
		assert_eq!(stack_key(std::iter::once("rust")), "stack-rust");
		assert_eq!(
			stack_key(["rust", "claude"].into_iter()),
			"stack-rust-claude"
		);
	}

	/// The union shape: a seed (takes the shared `pkgs`) and a self-contained project
	/// shell (reads its own shared lock → takes `{ }`).
	fn union_members() -> Vec<Member> {
		vec![
			Member {
				path: PathBuf::from("/s/rust.nix"),
				pkgs: true,
				pins: false,
			},
			Member {
				path: PathBuf::from("/proj/shell.nix"),
				pkgs: false,
				pins: false,
			},
		]
	}

	/// Render as [`adhoc_composition`] does, without its filesystem side effects.
	fn render(members: &[Member], label: &str, config: Option<&Path>, packages: &[String]) -> String {
		crate::env::nix_expr::ComposeSpec {
			lock_ref: &crate::env::nix_expr::nix_str(Path::new("/cfg/flake.lock")),
			config_frag: &crate::env::nix_expr::nixpkgs_config_frag(config),
			name: &label.replace(' ', "-"),
			imports: &imports_frag(members),
			label,
			packages,
			pins: &[],
		}
		.render()
	}

	#[test]
	fn compose_expr_unions_fragments_in_order_against_one_pin() {
		let members = union_members();
		let expr = render(&members, "rust proj", None, &[]);
		assert!(expr.contains("pkgs = pkgsOf \"/cfg/flake.lock\";"));
		assert!(expr.contains("import \"/s/rust.nix\" { inherit pkgs; }"));
		assert!(
			expr.contains("import \"/proj/shell.nix\" { }"),
			"self-contained: no inject"
		);
		// composition order preserved (caller sorts lexically or honors -o).
		assert!(expr.find("/s/rust.nix").unwrap() < expr.find("/proj/shell.nix").unwrap());
		assert!(expr.contains("inputsFrom"));
		assert!(
			expr.contains("name = \"rust-proj\";"),
			"sanitized store name"
		);
		assert!(
			!expr.contains("config = import"),
			"no nixpkgs config by default"
		);

		// A nixpkgs config file is applied to the nixpkgs import (allowUnfree etc.).
		let cfg = PathBuf::from("/cfg/nixpkgs-config.nix");
		let expr2 = render(&members, "rust proj", Some(&cfg), &[]);
		assert!(expr2.contains("config = import \"/cfg/nixpkgs-config.nix\";"));

		// Ad-hoc `-p` packages ride along in the composed shell's own list.
		let expr3 = render(&members, "rust proj", None, &["tokei".to_string()]);
		assert!(expr3.contains("packages = with pkgs; [ tokei ];"));
	}

	#[test]
	fn imports_frag_passes_pins_only_to_the_member_that_asked() {
		// A seed that opted in (`{ pkgs, pins }:`) beside one that did not: the
		// opt-out keeps the exact argument it got before `[env.pins]` existed, which
		// is what makes the feature backward compatible with seeds on disk.
		let members = vec![
			Member {
				path: PathBuf::from("/s/claude-latest.nix"),
				pkgs: true,
				pins: true,
			},
			Member {
				path: PathBuf::from("/s/rust.nix"),
				pkgs: true,
				pins: false,
			},
		];
		let frag = imports_frag(&members);
		assert!(frag.contains("(import \"/s/claude-latest.nix\" { inherit pkgs pins; })"));
		assert!(frag.contains("(import \"/s/rust.nix\" { inherit pkgs; })"));
	}

	#[test]
	fn a_composition_binds_pins_once_for_every_declared_pin() {
		let pins = [(
			"unstable".to_string(),
			PathBuf::from("/c/locks/unstable.lock"),
		)];
		let expr = crate::env::nix_expr::ComposeSpec {
			lock_ref: "\"/cfg/flake.lock\"",
			name: "claude-latest",
			imports: &imports_frag(&[Member {
				path: PathBuf::from("/s/claude-latest.nix"),
				pkgs: true,
				pins: true,
			}]),
			label: "claude-latest",
			pins: &pins,
			..Default::default()
		}
		.render();
		// Both pins come from the one `pkgsOf` reader, and the stack's pin is still
		// the lock the shared-pin check settled on.
		assert!(expr.contains("pkgs = pkgsOf \"/cfg/flake.lock\";"));
		assert!(expr.contains("unstable = pkgsOf \"/c/locks/unstable.lock\";"));
		assert_eq!(expr.matches("fetchTarball").count(), 1);
	}
}
