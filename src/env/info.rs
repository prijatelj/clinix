//! Diagnostics: `info`/`deps`/`list`/`check`/`shared` — read-only reports over
//! resolved envs. The env-name selectors live in [`crate::env`]. `check` (PATH
//! audit) and `shared` (N-way closure compare) port the `envcheck`/`shareck`
//! prototype scripts; the top-level contextual [`context_report`] (`clinix info`)
//! reuses these verbs against the active (cwd) env.

use std::collections::BTreeSet;
use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::env::project::Project;
use crate::env::{Context, Kind, OptionalTarget, RunCmd, Targets, resolve};
use crate::error::{ClinixError, Result};

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
	fn run(self, context: &Context) -> Result<()> {
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

fn kind_str(kind: Kind) -> &'static str {
	match kind {
		Kind::Project => "project",
		Kind::Registry => "registry",
	}
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

/// List registered envs (`state/envs/<name>`), each enriched cheaply with its
/// nixpkgs pin read from `flake.lock`. The filesystem *is* the index
/// ([`crate::env::registry`]) — nothing to keep in sync. An empty registry prints a
/// hint instead of a table.
pub fn list(context: &Context) -> Result<()> {
	let names = crate::env::registry::list_names(&context.config)?;
	if names.is_empty() {
		println!("clinix: no registered envs (create one: clinix env new <name> --from …)");
		return Ok(());
	}
	let width = names.iter().map(String::len).max().unwrap_or(4).max(4);
	println!("{:<width$}  NIXPKGS", "NAME");
	for name in &names {
		let pin = registry_pin(context, name);
		println!("{name:<width$}  {pin}");
	}
	Ok(())
}

/// A registry env's nixpkgs pin as a short `ref @ rev9` string, or a reason it
/// could not be read (a malformed env should not abort the whole listing).
fn registry_pin(context: &Context, name: &str) -> String {
	let env = match resolve(&context.config, Some(name)) {
		Ok(env) => env,
		Err(_) => return "(unresolved)".to_string(),
	};
	match Project::load(env) {
		Ok(project) => match nixpkgs_pin(&project) {
			Some((track, rev)) => format!("{track} @ {:.9}", rev),
			None => "(no nixpkgs pin)".to_string(),
		},
		Err(_) => "(no flake.lock)".to_string(),
	}
}

/// Where an env's dependencies live on disk: its derivation, each package's store
/// path, the full closure (count, and `--size` for on-disk bytes), and the GC
/// roots holding it. All classic (`nix-instantiate`, `nix-store -qR`, `du`).
#[derive(Args, Debug)]
pub struct Deps {
	/// Env to analyze (name, path, or `.`); defaults to the cwd project.
	pub name: Option<String>,
	/// Also measure the closure's on-disk size (slower — stats every path).
	#[arg(long)]
	pub size: bool,
}
impl RunCmd for Deps {
	fn run(self, context: &Context) -> Result<()> {
		let env = resolve(&context.config, self.name.as_deref())?;
		let shell_nix = env.root.join("shell.nix");
		if !shell_nix.is_file() {
			return Err(ClinixError::Resolve(format!(
				"no shell.nix at {} (run: clinix env init)",
				env.root.display()
			)));
		}
		let drv = crate::nix::instantiate(&shell_nix)?;
		println!("env: {} ({})", env.root.display(), kind_str(env.kind));
		println!("derivation: {drv}");

		let packages = crate::nix::package_paths(&shell_nix)?;
		println!("\npackages ({}):", packages.len());
		for p in &packages {
			println!("  {}\n    {}", p.name, p.path);
		}

		let closure = crate::nix::closure(&drv)?;
		print!("\nclosure: {} store paths", closure.len());
		if self.size {
			println!(" — {}", human_bytes(crate::disk::total_bytes(&closure)));
		} else {
			println!("  (pass --size to measure on disk)");
		}

		let roots = crate::nix::gc_roots(&drv)?;
		println!("\ngc roots:");
		if roots.is_empty() {
			println!("  none — nix-collect-garbage will delete this env");
			println!(
				"  root it: clinix env shell {}",
				self.name.as_deref().unwrap_or(".")
			);
		} else {
			for r in &roots {
				println!("  {r}");
			}
		}
		Ok(())
	}
}

fn human_bytes(bytes: u64) -> String {
	const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
	let mut value = bytes as f64;
	let mut unit = 0;
	while value >= 1024.0 && unit < UNITS.len() - 1 {
		value /= 1024.0;
		unit += 1;
	}
	if unit == 0 {
		format!("{bytes} B")
	} else {
		format!("{value:.1} {}", UNITS[unit])
	}
}

// ---- check (envcheck): shell + lock + PATH-shadow audit ---------------------

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

/// How the current process relates to nix. `nix-shell` sets `IN_NIX_SHELL`;
/// `nix develop` does not, so fall back to a store-provided PATH signal.
enum ShellKind {
	NixShell(String),
	Develop,
	NotNix,
}

fn detect_shell() -> ShellKind {
	if let Ok(v) = std::env::var("IN_NIX_SHELL") {
		return ShellKind::NixShell(v);
	}
	let store_on_path = std::env::var("PATH").is_ok_and(|p| p.contains("/nix/store"));
	if std::env::var_os("NIX_BUILD_TOP").is_some() || store_on_path {
		ShellKind::Develop
	} else {
		ShellKind::NotNix
	}
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

/// The result of splitting `$PATH` into store vs host entries and finding those
/// host entries positioned to shadow a pinned store tool.
#[derive(Debug, PartialEq)]
struct PathAudit {
	store: usize,
	host: usize,
	/// Host dirs appearing before the last store dir — only these can shadow.
	shadowing: Vec<String>,
}

/// Classify `$PATH`: a host dir can shadow a pinned tool only when it appears
/// *before* a store dir, so the shadow set is the host entries left of the last
/// store entry. Pure `&str → PathAudit` (no env read) — unit-testable.
fn audit_path(path: &str) -> PathAudit {
	let entries: Vec<&str> = path.split(':').filter(|e| !e.is_empty()).collect();
	let is_store = |e: &str| e.starts_with("/nix/store");
	let store = entries.iter().filter(|e| is_store(e)).count();
	let host = entries.len() - store;
	let last_store = entries.iter().rposition(|e| is_store(e));
	let shadowing = match last_store {
		Some(last) => entries[..last]
			.iter()
			.filter(|e| !is_store(e))
			.map(|s| s.to_string())
			.collect(),
		None => Vec::new(),
	};
	PathAudit {
		store,
		host,
		shadowing,
	}
}

// ---- shared (shareck): N-way closure comparison -----------------------------

/// N-way shared-package comparison across several envs (the former `shareck`,
/// generalized from 2 to N). Instantiates each env's `shell.nix` and compares the
/// runtime closures: the global shared core, each env's unique set, a pairwise
/// shared-percentage matrix, and the on-disk cost of each set. Shared paths are
/// content-addressed (cost disk once), so a high shared count is the good case.
pub fn shared(targets: Targets, context: &Context) -> Result<()> {
	let mut sets: Vec<(String, BTreeSet<PathBuf>)> = Vec::new();
	for name in &targets.names {
		let env = resolve(&context.config, Some(name))?;
		let shell_nix = env.root.join("shell.nix");
		if !shell_nix.is_file() {
			return Err(ClinixError::Resolve(format!(
				"no shell.nix at {} (run: clinix env init)",
				env.root.display()
			)));
		}
		let drv = crate::nix::instantiate(&shell_nix)?;
		let closure: BTreeSet<PathBuf> = crate::nix::closure(&drv)?
			.into_iter()
			.map(PathBuf::from)
			.collect();
		sets.push((name.clone(), closure));
	}
	let report = analyze_sharing(&sets);

	println!("== counts");
	for (name, total) in &report.totals {
		println!("  {name:<16} {total} paths");
	}
	println!(
		"  {:<16} {} paths  (cost disk once)",
		"shared (all)", report.global_shared
	);
	for (name, uniq) in &report.unique {
		println!("  unique to {name:<6} {uniq} paths");
	}

	if report.matrix.len() > 1 {
		println!("\n== pairwise shared (% of row env's closure)");
		for (a, b, pct) in &report.matrix {
			println!("  {a:<16} ∩ {b:<16} {pct}%");
		}
	}

	println!("\n== disk (hardlink-aware)");
	println!(
		"  {:<20} {}",
		"shared (counted once)",
		human_bytes(disk_of(&report.shared_paths))
	);
	for (name, paths) in &report.unique_paths {
		println!("  unique to {name:<10} {}", human_bytes(disk_of(paths)));
	}
	Ok(())
}

/// The pure set-arithmetic of `shared`, separated from the nix I/O so it is
/// unit-testable on synthetic closures.
struct SharingReport {
	totals: Vec<(String, usize)>,
	global_shared: usize,
	unique: Vec<(String, usize)>,
	/// Row-relative: `(a, b, |a∩b|·100/|a|)`.
	matrix: Vec<(String, String, usize)>,
	shared_paths: Vec<PathBuf>,
	unique_paths: Vec<(String, Vec<PathBuf>)>,
}

fn analyze_sharing(sets: &[(String, BTreeSet<PathBuf>)]) -> SharingReport {
	let totals = sets.iter().map(|(n, s)| (n.clone(), s.len())).collect();

	// Global shared = fold intersection across every set.
	let global: BTreeSet<PathBuf> = match sets.split_first() {
		Some(((_, first), rest)) => rest.iter().fold(first.clone(), |acc, (_, s)| {
			acc.intersection(s).cloned().collect()
		}),
		None => BTreeSet::new(),
	};

	// Per env: paths in it but in no other set.
	let mut unique = Vec::new();
	let mut unique_paths = Vec::new();
	for (i, (name, set)) in sets.iter().enumerate() {
		let others: BTreeSet<&PathBuf> = sets
			.iter()
			.enumerate()
			.filter(|(j, _)| *j != i)
			.flat_map(|(_, (_, s))| s.iter())
			.collect();
		let only: Vec<PathBuf> = set.iter().filter(|p| !others.contains(p)).cloned().collect();
		unique.push((name.clone(), only.len()));
		unique_paths.push((name.clone(), only));
	}

	// Pairwise, row-relative percentage.
	let mut matrix = Vec::new();
	for (i, (a, sa)) in sets.iter().enumerate() {
		for (j, (b, sb)) in sets.iter().enumerate() {
			if i == j {
				continue;
			}
			let inter = sa.intersection(sb).count();
			let pct = if sa.is_empty() { 0 } else { inter * 100 / sa.len() };
			matrix.push((a.clone(), b.clone(), pct));
		}
	}

	SharingReport {
		totals,
		global_shared: global.len(),
		unique,
		matrix,
		shared_paths: global.into_iter().collect(),
		unique_paths,
	}
}

/// Hardlink-aware on-disk size of a set of store paths (`disk.rs` dedups by
/// `(device, inode)` — the store is heavily hardlinked).
fn disk_of(paths: &[PathBuf]) -> u64 {
	let as_str: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
	crate::disk::total_bytes(&as_str)
}

// ---- top-level `clinix info` (contextual front-end) -------------------------

/// The verb under the contextual top-level `clinix info` (default = summary).
#[derive(Subcommand, Debug)]
pub enum InfoVerb {
	/// Audit the active env's shell/lock/PATH (contextual `env check`).
	Check,
	/// Dependency/closure report for the active env (contextual `env deps`).
	Deps,
}

/// `clinix info [check|deps]` — the contextual entry point.
///
/// Bare `info` describes the **active nix-shell itself, read from the environment
/// nix exports** (`IN_NIX_SHELL`, `system`, `out`, and the provided packages in
/// `buildInputs`/`nativeBuildInputs`) — so it works for *any* nix-shell, whether
/// or not clinix launched it, with no nix call and no `flake.lock`. Outside a
/// shell it falls back to the cwd project. (It reports the shell's *contents*;
/// naming *which clinix registry env / composed stack* you entered is the
/// separate, deferred `CLINIX_ENV_STACK` launch marker.) `info check`/`info deps`
/// reuse the `env` verbs (write-once).
pub fn context_report(verb: Option<InfoVerb>, context: &Context) -> Result<()> {
	match verb {
		None => report_active(context),
		Some(InfoVerb::Check) => check(OptionalTarget { name: None }, context),
		Some(InfoVerb::Deps) => Deps {
			name: None,
			size: false,
		}
		.run(context),
	}
}

/// Summarize the active environment. In a nix-shell: report it from the exported
/// derivation environment ([`report_active_shell`]). Outside one: the cwd project
/// if present, else a clear "neither" note.
fn report_active(context: &Context) -> Result<()> {
	match detect_shell() {
		ShellKind::NotNix => {
			let cwd = std::env::current_dir()?;
			if cwd.join("shell.nix").exists() || cwd.join("flake.lock").exists() {
				println!("not in a nix shell — reporting the cwd project:\n");
				Info {
					name: None,
					json: false,
				}
				.run(context)
			} else {
				println!(
					"not in a nix shell, and no project (shell.nix/flake.lock) in the cwd"
				);
				Ok(())
			}
		}
		kind => {
			report_active_shell(&kind);
			Ok(())
		}
	}
}

/// Report the live nix-shell from the environment nix set for it — no nix call,
/// no `flake.lock`, works for any nix-shell. `$out`'s basename is the reliable
/// derivation label (`$name` can be shadowed by an inherited value); the provided
/// packages come from `buildInputs`/`nativeBuildInputs`.
fn report_active_shell(kind: &ShellKind) {
	match kind {
		ShellKind::NixShell(v) => println!("active: in a nix-shell (IN_NIX_SHELL={v})"),
		ShellKind::Develop => println!("active: in a nix develop / store-provided shell"),
		ShellKind::NotNix => return,
	}
	if let Ok(out) = std::env::var("out") {
		println!("  derivation  {}", store_label(&out));
	}
	println!(
		"  system      {}",
		std::env::var("system").unwrap_or_else(|_| "<unset>".into())
	);

	let pkgs = provided_packages();
	if pkgs.is_empty() {
		println!("  packages    (none exported in buildInputs/nativeBuildInputs)");
	} else {
		println!("  packages ({}):", pkgs.len());
		for p in &pkgs {
			println!("    {p}");
		}
	}

	let audit = audit_path(&std::env::var("PATH").unwrap_or_default());
	let shadow = if audit.shadowing.is_empty() {
		String::new()
	} else {
		format!(" — {} host dir(s) shadow pinned tools (clinix info check)", audit.shadowing.len())
	};
	println!(
		"  PATH        {} store / {} host entries{shadow}",
		audit.store, audit.host
	);
}

/// The packages the active nix-shell provides, from the derivation env vars nix
/// exports (`buildInputs` + `nativeBuildInputs` + `propagatedBuildInputs`) —
/// whitespace-separated store paths. Labeled by [`store_label`], order preserved,
/// de-duplicated. No nix call; works for any nix-shell.
fn provided_packages() -> Vec<String> {
	let mut seen = BTreeSet::new();
	let mut out = Vec::new();
	for var in ["buildInputs", "nativeBuildInputs", "propagatedBuildInputs"] {
		let Ok(val) = std::env::var(var) else { continue };
		for path in val.split_whitespace() {
			let label = store_label(path);
			if seen.insert(label.clone()) {
				out.push(label);
			}
		}
	}
	out
}

/// Strip a store path down to its readable `name-version` label: drop the
/// `/nix/store/` dir and the 32-char base32 hash prefix
/// (`/nix/store/<hash>-ripgrep-15.1.0` → `ripgrep-15.1.0`). A non-store or
/// unexpected path degrades to its basename. Pure — unit-tested.
fn store_label(path: &str) -> String {
	let base = path.rsplit('/').next().unwrap_or(path);
	match base.split_once('-') {
		Some((hash, rest))
			if hash.len() == 32 && hash.bytes().all(|b| b.is_ascii_alphanumeric()) =>
		{
			rest.to_string()
		}
		_ => base.to_string(),
	}
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

	// ---- PATH-shadow audit (envcheck core) ----

	#[test]
	fn path_audit_flags_only_host_dirs_before_a_store_dir() {
		// /usr/bin sits before a store dir → it can shadow a pinned tool.
		let a = audit_path("/usr/bin:/nix/store/abc-ripgrep/bin:/home/u/.local/bin");
		assert_eq!(a.store, 1);
		assert_eq!(a.host, 2);
		assert_eq!(a.shadowing, vec!["/usr/bin".to_string()]); // trailing host dir is safe
	}

	#[test]
	fn path_audit_no_store_means_not_in_a_shell() {
		let a = audit_path("/usr/bin:/bin");
		assert_eq!(a.store, 0);
		assert!(a.shadowing.is_empty()); // nothing to shadow
	}

	#[test]
	fn path_audit_all_host_after_store_cannot_shadow() {
		let a = audit_path("/nix/store/x/bin:/nix/store/y/bin:/usr/bin");
		assert_eq!((a.store, a.host), (2, 1));
		assert!(a.shadowing.is_empty());
	}

	// ---- N-way sharing set-math (shareck core) ----

	fn set(paths: &[&str]) -> BTreeSet<PathBuf> {
		paths.iter().map(PathBuf::from).collect()
	}

	#[test]
	fn analyze_sharing_computes_global_core_and_uniques() {
		let sets = vec![
			("a".to_string(), set(&["/s/common", "/s/a1", "/s/a2"])),
			("b".to_string(), set(&["/s/common", "/s/b1"])),
			("c".to_string(), set(&["/s/common", "/s/a1", "/s/c1"])),
		];
		let r = analyze_sharing(&sets);
		assert_eq!(r.global_shared, 1); // only /s/common is in all three
		// a's uniques: a2 only (a1 is also in c, common is shared).
		assert_eq!(r.unique[0], ("a".to_string(), 1));
		assert_eq!(r.unique[1], ("b".to_string(), 1)); // b1
		// a∩b = {common} = 1 of a's 3 → 33%.
		let ab = r.matrix.iter().find(|(x, y, _)| x == "a" && y == "b").unwrap();
		assert_eq!(ab.2, 33);
	}

	#[test]
	fn analyze_sharing_identical_sets_are_fully_shared() {
		let s = set(&["/s/x", "/s/y"]);
		let sets = vec![("a".to_string(), s.clone()), ("b".to_string(), s)];
		let r = analyze_sharing(&sets);
		assert_eq!(r.global_shared, 2);
		assert_eq!(r.unique[0].1, 0);
		assert_eq!(r.matrix[0].2, 100); // a∩b = all of a
	}

	// ---- store-path label (reading the active nix-shell's contents) ----

	#[test]
	fn store_label_strips_hash_prefix_to_name_version() {
		// 32-char base32 hash + `-` + name-version (as `$buildInputs` exports).
		let hash = "0123456789abcdfghijklmnpqrsvwxyz"; // 32 chars (nix base32 alphabet)
		assert_eq!(hash.len(), 32);
		assert_eq!(
			store_label(&format!("/nix/store/{hash}-ripgrep-15.1.0")),
			"ripgrep-15.1.0"
		);
		// An output suffix is part of the label.
		assert_eq!(
			store_label(&format!("/nix/store/{hash}-jq-1.8.2-dev")),
			"jq-1.8.2-dev"
		);
		// Non-store paths degrade to the basename.
		assert_eq!(store_label("/usr/bin/rg"), "rg");
	}
}
