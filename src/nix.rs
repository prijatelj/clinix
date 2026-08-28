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

use std::process::{Command, Output};

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
    if let Some(rev) = pick_ref(&stdout_string(&run(heads)?), &format!("refs/heads/{git_ref}")) {
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
