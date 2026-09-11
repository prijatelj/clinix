//! The single git/Nix shell-out boundary — **classic tooling only**.
//!
//! clinix deliberately avoids `nix flake` and the experimental `nix` CLI
//! (`nix eval`, `nix flake lock`, …): the `dev_env` prototype (`pin`) resolves
//! and locks with portable classic tools, and clinix maintains that property
//! (works across CppNix/Lix/Snix, no experimental-features gate). `flake.lock`
//! is just JSON that classic Nix reads via `shell.nix`'s hand-written reader.
//!
//! - **ref → rev**: `git ls-remote` (branch first, then peeled tag).
//! - **rev → narHash**: `nix-prefetch-url --unpack` + `nix-hash --to-sri`, the
//!   NAR hash of the unpacked tree that `builtins.fetchTarball`'s `sha256`
//!   verifies.
//!
//! (The `git`-type `add` path in a later phase needs `builtins.fetchGit`, which
//! `pin` runs via `nix … eval`; that lone `nix-command` use will be flagged when
//! it lands. The github path used by `init` is fully classic.)

use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Output};

use serde_json::Value;

use crate::error::{ClinixError, Result};
use crate::model::newtypes::{NarHash, Rev};

/// Run a prepared command, mapping a nonzero exit to [`ClinixError::Nix`].
fn run(mut cmd: Command) -> Result<Output> {
	let rendered = format!("{cmd:?}");
	let out = cmd.output()?; // io::Error → ClinixError::Io
	if out.status.success() {
		Ok(out)
	} else {
		Err(ClinixError::Nix {
			cmd: rendered,
			status: out.status.to_string(),
			stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
		})
	}
}

fn stdout_string(out: &Output) -> String {
	String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The rev (first column) of the `git ls-remote` line whose ref (second column)
/// equals `want`.
fn pick_ref(text: &str, want: &str) -> Option<String> {
	text.lines().find_map(|line| {
		let mut cols = line.split_whitespace();
		let rev = cols.next()?;
		let name = cols.next()?;
		(name == want).then(|| rev.to_string())
	})
}

/// Resolve a github ref (branch or tag) to its commit revision, mirroring
/// `pin`'s resolver: `refs/heads/<ref>` first, then `refs/tags/<ref>` preferring
/// the peeled (`^{}`) commit over the tag object.
pub fn resolve_github_ref(owner: &str, repo: &str, git_ref: &str) -> Result<Rev> {
	let url = format!("https://github.com/{owner}/{repo}.git");

	let mut heads = Command::new("git");
	heads.args(["ls-remote", "--heads", &url, git_ref]);
	if let Some(rev) = pick_ref(
		&stdout_string(&run(heads)?),
		&format!("refs/heads/{git_ref}"),
	) {
		return rev.parse();
	}

	let mut tags = Command::new("git");
	tags.args([
		"ls-remote",
		"--tags",
		&url,
		git_ref,
		&format!("{git_ref}^{{}}"),
	]);
	let lines = stdout_string(&run(tags)?);
	let peeled = pick_ref(&lines, &format!("refs/tags/{git_ref}^{{}}"));
	let plain = pick_ref(&lines, &format!("refs/tags/{git_ref}"));
	match peeled.or(plain) {
		Some(rev) => rev.parse(),
		None => Err(ClinixError::Resolve(format!(
			"no branch or tag `{git_ref}` at {owner}/{repo}"
		))),
	}
}

/// The NAR hash (SRI) of a github tarball's unpacked tree — exactly what
/// `builtins.fetchTarball`'s `sha256` verifies. `nix-prefetch-url --unpack` then
/// `nix-hash --to-sri`, as `pin` does.
pub fn github_tarball_narhash(owner: &str, repo: &str, rev: &str) -> Result<NarHash> {
	let url = format!("https://github.com/{owner}/{repo}/archive/{rev}.tar.gz");
	let mut prefetch = Command::new("nix-prefetch-url");
	prefetch.args(["--unpack", &url]);
	let base32 = stdout_string(&run(prefetch)?).trim().to_string();
	if base32.is_empty() {
		return Err(ClinixError::Resolve(format!(
			"nix-prefetch-url returned no hash for {owner}/{repo}@{rev}"
		)));
	}
	to_sri(&base32)
}

/// Convert a base32 sha256 to its SRI form (`sha256-…`) via `nix-hash --to-sri`.
fn to_sri(base32: &str) -> Result<NarHash> {
	let mut cmd = Command::new("nix-hash");
	cmd.args(["--to-sri", "--type", "sha256", base32]);
	stdout_string(&run(cmd)?).trim().parse()
}

/// Fully resolve a tracked github ref to `(rev, narHash)` — `git ls-remote` then
/// prefetch. Shared by `init` (initial lock) and `update` (re-lock).
pub fn resolve_github(owner: &str, repo: &str, git_ref: &str) -> Result<(Rev, NarHash)> {
	let rev = resolve_github_ref(owner, repo, git_ref)?;
	let nar_hash = github_tarball_narhash(owner, repo, rev.as_str())?;
	Ok((rev, nar_hash))
}

/// Resolve a **git** input to `(rev, narHash)` via `nix-prefetch-git`, which both
/// resolves the ref and hashes the checkout in one call. `git_ref` is a branch/tag
/// (`None` = the remote's default branch). Used by `pin add git`.
pub fn resolve_git(url: &str, git_ref: Option<&str>) -> Result<(Rev, NarHash)> {
	let mut cmd = Command::new("nix-prefetch-git");
	cmd.args(["--url", url, "--quiet"]);
	if let Some(r) = git_ref {
		cmd.args(["--rev", r]);
	}
	let out = run(cmd)?;
	let json: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| {
		ClinixError::Resolve(format!("nix-prefetch-git gave no JSON for {url}: {e}"))
	})?;
	let rev: Rev = json
		.get("rev")
		.and_then(|v| v.as_str())
		.ok_or_else(|| ClinixError::Resolve(format!("nix-prefetch-git: no rev for {url}")))?
		.parse()?;
	// Prefer an SRI `hash` (recent nix-prefetch-git); else convert the base32 `sha256`.
	let nar_hash = match json.get("hash").and_then(|v| v.as_str()) {
		Some(h) if h.starts_with("sha256-") => h.parse()?,
		_ => {
			let sha = json.get("sha256").and_then(|v| v.as_str()).ok_or_else(|| {
				ClinixError::Resolve(format!("nix-prefetch-git: no sha256 for {url}"))
			})?;
			to_sri(sha)?
		}
	};
	Ok((rev, nar_hash))
}

/// GC-root a nix file's derivation and return its `.drv` store path:
/// `nix-instantiate <file> --add-root <root> --indirect`. Rooting is why a shell
/// entered through clinix survives `nix-collect-garbage` (plain `nix-shell` does
/// not root; keep the outputs too with `keep-outputs = true` in nix.conf).
pub fn instantiate_rooted(nix_file: &Path, root: &Path) -> Result<String> {
	if let Some(parent) = root.parent() {
		std::fs::create_dir_all(parent)?;
	}
	let mut cmd = Command::new("nix-instantiate");
	cmd.arg(nix_file)
		.arg("--add-root")
		.arg(root)
		.arg("--indirect");
	Ok(stdout_string(&run(cmd)?).trim().to_string())
}

/// GC-root an env's **complete realized build closure** via `mkShell`'s hidden
/// `inputDerivation` attribute ([nixpkgs#95536]): a derivation whose *runtime*
/// dependencies are the shell's *build-time* dependencies, so realizing and rooting
/// its output keeps `stdenv`, `bash`, the setup hooks, and every `buildInput` (with
/// their transitive closures) alive — **independent of `keep-outputs`**. This is the
/// retention guarantee; [`instantiate_rooted`]'s `.drv` root only covers the
/// eval/source graph (offline re-eval), not the built outputs.
///
/// Two classic steps: `nix-instantiate --expr '(import <shell> {}).inputDerivation'`
/// for the drv, then `nix-store --realise <drv> --add-root <root> --indirect` to
/// build it and register its output as an indirect root. Errors if the shell is not
/// an `mkDerivation`/`mkShell` (no `inputDerivation`) — callers treat that as a
/// non-fatal "no complete root" and keep the `.drv` root.
///
/// [nixpkgs#95536]: https://github.com/NixOS/nixpkgs/pull/95536
pub fn root_input_closure(shell_nix: &Path, root: &Path) -> Result<()> {
	if let Some(parent) = root.parent() {
		std::fs::create_dir_all(parent)?;
	}
	let expr = format!(
		"(import {} {{}}).inputDerivation",
		crate::env::nix_expr::nix_str(shell_nix)
	);
	let mut inst = Command::new("nix-instantiate");
	inst.arg("--expr").arg(&expr);
	let drv = stdout_string(&run(inst)?).trim().to_string();

	let mut real = Command::new("nix-store");
	real.arg("--realise")
		.arg(&drv)
		.arg("--add-root")
		.arg(root)
		.arg("--indirect");
	run(real)?;
	Ok(())
}

/// Enter `nix-shell <drv>`, inheriting stdio: interactive, unless `command` is
/// given (then `--run <command>`); `--pure` for a pure shell. Returns the child
/// exit status (this is the one nix invocation that does not capture output).
pub fn nix_shell(drv: &str, pure: bool, command: Option<&str>) -> Result<ExitStatus> {
	let mut cmd = Command::new("nix-shell");
	cmd.arg(drv);
	if pure {
		cmd.arg("--pure");
	}
	if let Some(c) = command {
		cmd.arg("--run").arg(c);
	}
	Ok(cmd.status()?)
}

/// Enter `nix-shell <drv>` interactively **by replacing this process image**
/// (`execvp`), so clinix does not linger as a parent of the nix-shell.
///
/// This keeps the process tree the same shape as `~/dev_env/shell`'s
/// `exec nix-shell`: the parent shell's direct child is the nix-shell's bash, with
/// no intervening clinix node. Tools that reconstruct a terminal's shell stack from
/// `/proc` (e.g. `clonetty`) walk that tree and treat every process between the
/// terminal and the leaf shell as a shell level; a lingering non-shell clinix
/// process otherwise breaks that descent and hides the nix-shell layer. `--pure`
/// for a pure shell.
///
/// On success `execvp` does not return (the image is replaced); a returned value is
/// therefore always the `Err` case — the `exec` itself failed (e.g. `nix-shell` not
/// on `PATH`). The `Ok(ExitStatus)` arm is unreachable and exists only to share a
/// signature with [`nix_shell`] so callers can select between them uniformly.
pub fn nix_shell_exec(drv: &str, pure: bool) -> Result<ExitStatus> {
	let mut cmd = Command::new("nix-shell");
	cmd.arg(drv);
	if pure {
		cmd.arg("--pure");
	}
	Err(cmd.exec().into()) // io::Error → ClinixError::Io; only reached if exec fails
}

/// Evaluate a nix expression to JSON with **classic** `nix-instantiate --eval
/// --strict --json` (not the experimental `nix eval`). `--strict` forces deep
/// evaluation so lists/attrsets serialize instead of printing thunks. Used by
/// `info` to read resolved package versions from the pinned nixpkgs (ADR-1).
pub fn eval_json(expr: &str) -> Result<Value> {
	let mut cmd = Command::new("nix-instantiate");
	cmd.args(["--eval", "--strict", "--json", "--expr", expr]);
	Ok(serde_json::from_slice(&run(cmd)?.stdout)?)
}

/// Instantiate a nix file to its `.drv` store path (no GC root):
/// `nix-instantiate <file>`. Used by `deps` to analyze an env's derivation.
pub fn instantiate(nix_file: &Path) -> Result<String> {
	let mut cmd = Command::new("nix-instantiate");
	cmd.arg(nix_file);
	Ok(stdout_string(&run(cmd)?).trim().to_string())
}

/// The closure of a derivation — every store path reachable, including built
/// outputs: `nix-store --query --requisites --include-outputs <drv>`.
pub fn closure(drv: &str) -> Result<Vec<String>> {
	let mut cmd = Command::new("nix-store");
	cmd.args(["--query", "--requisites", "--include-outputs", drv]);
	Ok(stdout_string(&run(cmd)?)
		.lines()
		.map(str::to_string)
		.collect())
}

/// GC roots referencing a derivation: `nix-store --query --roots <drv>`.
pub fn gc_roots(drv: &str) -> Result<Vec<String>> {
	let mut cmd = Command::new("nix-store");
	cmd.args(["--query", "--roots", drv]);
	Ok(stdout_string(&run(cmd)?)
		.lines()
		.map(str::to_string)
		.collect())
}

/// A package in an env and the store path of its (intended) output.
#[derive(Debug, serde::Deserialize)]
pub struct PackagePath {
	pub name: String,
	pub path: String,
}

/// Evaluate an env's package output paths — the `buildInputs ++ nativeBuildInputs`
/// of its `shell.nix`. `toString` yields each derivation's output path **without
/// building** it (classic eval, ADR-1). Shared by `deps` and `export closure`.
pub fn package_paths(shell_nix: &Path) -> Result<Vec<PackagePath>> {
	let expr = format!(
		"let s = import {} {{}}; ins = (s.nativeBuildInputs or []) ++ (s.buildInputs or []); \
		 in map (p: {{ name = p.name or \"?\"; path = toString p; }}) ins",
		shell_nix.display()
	);
	Ok(serde_json::from_value(eval_json(&expr)?)?)
}

/// Of `paths`, those **not** valid in the local store (not built/substituted):
/// `nix-store --check-validity --print-invalid`. Empty ⇒ everything is present.
/// `--print-invalid` reports rather than failing, so a nonzero exit is a real
/// error, not "some are missing".
pub fn invalid_paths(paths: &[String]) -> Result<Vec<String>> {
	if paths.is_empty() {
		return Ok(Vec::new());
	}
	let mut cmd = Command::new("nix-store");
	cmd.args(["--check-validity", "--print-invalid"])
		.args(paths);
	Ok(stdout_string(&run(cmd)?)
		.lines()
		.filter(|l| !l.is_empty())
		.map(str::to_string)
		.collect())
}

/// Runtime requisites of already-realized output paths: `nix-store -qR <paths>`
/// (no `--include-outputs`, since these are outputs). This is the full set that
/// `--export` must be handed — `--export` never auto-adds references.
pub fn requisites(paths: &[String]) -> Result<Vec<String>> {
	let mut cmd = Command::new("nix-store");
	cmd.args(["--query", "--requisites"]).args(paths);
	Ok(stdout_string(&run(cmd)?)
		.lines()
		.map(str::to_string)
		.collect())
}

/// Serialize a set of store paths (a complete closure) into one archive:
/// `nix-store --export <paths> > out_file`. The inverse of [`import_closure`].
pub fn export_paths(paths: &[String], out_file: &Path) -> Result<()> {
	let file = File::create(out_file)?; // io::Error → ClinixError::Io
	let mut cmd = Command::new("nix-store");
	cmd.arg("--export").args(paths).stdout(file);
	let status = cmd.status()?;
	if status.success() {
		Ok(())
	} else {
		Err(ClinixError::Nix {
			cmd: "nix-store --export".into(),
			status: status.to_string(),
			stderr: String::new(),
		})
	}
}

/// Import a serialized closure archive into the local store (verifies NAR
/// hashes): `nix-store --import < archive`. The inverse of [`export_paths`].
/// `run` captures stderr, so a signature/trust refusal (a multi-user store
/// rejecting unsigned paths) reaches the caller intact.
pub fn import_closure(archive: &Path) -> Result<()> {
	let file = File::open(archive)?; // io::Error → ClinixError::Io
	let mut cmd = Command::new("nix-store");
	cmd.arg("--import").stdin(file);
	run(cmd)?;
	Ok(())
}

/// The local system tuple (`builtins.currentSystem`, e.g. `x86_64-linux`) — the
/// arch guard for `import`.
pub fn current_system() -> Result<String> {
	Ok(eval_json("builtins.currentSystem")?
		.as_str()
		.unwrap_or_default()
		.to_string())
}
