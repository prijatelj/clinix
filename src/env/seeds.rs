//! The **seed catalog**: the user's live `*.nix` shell fragments, read **in
//! place** from the directories/files declared in `config.toml`
//! (`[env.seeds].sources`). See `notes/clinix/design/seed-catalog-and-config.md`.
//!
//! A directory source contributes each top-level `*.nix` as a seed (named by
//! basename); a file source contributes one. Files are classified with a cheap
//! `rnix` syntactic parse: a valid shell fragment is cataloged, a `flake.nix`
//! shape or a parse error is skipped with a warning. Ignore globs
//! (`[env.seeds].ignore` + a per-dir `.clinix_ignore`) suppress the warnings.
//!
//! Pure filesystem + `rnix` parse — no nix eval — so richly unit-testable.

use std::path::{Path, PathBuf};

use rnix::{SyntaxKind, SyntaxNode};

use crate::env::config::{SeedSettings, SeedSource, expand_tilde};

/// One cataloged seed: its bare `name` (file stem), the source's `alias` if one
/// was **explicitly** set in config (`Some` → usable as the `alias:name` qualified
/// form; `None` → no alias, so disambiguate by exact path), and the `*.nix` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seed {
	pub name: String,
	pub alias: Option<String>,
	pub path: PathBuf,
}

/// How a candidate `*.nix` classifies under the shell-file gate.
#[derive(Debug, PartialEq, Eq)]
pub enum SeedKind {
	/// A usable shell fragment / `shell.nix` (cataloged).
	Shell,
	/// A `flake.nix` shape — detected and skipped (flake seeds are deferred).
	Flake,
	/// Fails to parse as nix — skipped.
	Invalid,
}

/// The result of resolving a name token against the catalog.
#[derive(Debug)]
pub enum Resolved<'a> {
	/// Exactly one seed matched.
	One(&'a Seed),
	/// A bare name matched several seeds; `chosen` is the highest-precedence one
	/// (first source wins), `others` are the shadowed matches (for a warning that
	/// names the `alias:name` form to select them).
	Collision { chosen: &'a Seed, others: Vec<&'a Seed> },
}

/// All seeds discovered across the configured sources, in precedence order
/// (highest first — earlier sources win bare-name collisions), plus any non-fatal
/// warnings gathered while scanning.
#[derive(Debug, Default)]
pub struct Catalog {
	pub seeds: Vec<Seed>,
	pub warnings: Vec<String>,
}

impl Catalog {
	/// Build the catalog from the configured seed sources.
	pub fn build(settings: &SeedSettings) -> Catalog {
		let mut cat = Catalog::default();
		for source in &settings.sources {
			cat.add_source(source, &settings.ignore);
		}
		cat
	}

	/// Resolve a name token: `alias:name` (exact), or a bare `name` (unique, else
	/// the highest-precedence match with the others reported as a collision).
	pub fn find(&self, token: &str) -> Option<Resolved<'_>> {
		if let Some((alias, name)) = token.split_once(':') {
			return self
				.seeds
				.iter()
				.find(|s| s.alias.as_deref() == Some(alias) && s.name == name)
				.map(Resolved::One);
		}
		let mut matches = self.seeds.iter().filter(|s| s.name == token);
		let first = matches.next()?;
		let others: Vec<&Seed> = matches.collect();
		if others.is_empty() {
			Some(Resolved::One(first))
		} else {
			Some(Resolved::Collision { chosen: first, others })
		}
	}

	fn add_source(&mut self, source: &SeedSource, config_ignore: &[String]) {
		let path = expand_tilde(&source.path);
		if path.is_dir() {
			// The alias is only what the user explicitly set — no derived default,
			// so an unaliased source's seeds carry `None` (empty in `env list`).
			let alias = source.alias.clone();
			let mut ignore = config_ignore.to_vec();
			ignore.extend(read_clinix_ignore(&path));
			// Recursively collect `*.nix` under the source, deterministically. A
			// seed is still named by basename (subdirs organize, not qualify);
			// same-basename files across subdirs collide (warned; use `alias:name`).
			let mut files: Vec<PathBuf> = walkdir::WalkDir::new(&path)
				.into_iter()
				.filter_map(std::result::Result::ok)
				.filter(|e| {
					e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "nix")
				})
				.map(walkdir::DirEntry::into_path)
				.collect();
			files.sort();
			for file in &files {
				let base = file.file_name().unwrap().to_string_lossy().into_owned();
				let rel = file
					.strip_prefix(&path)
					.unwrap_or(file)
					.to_string_lossy()
					.into_owned();
				if ignore.iter().any(|g| ignore_matches(g, &rel, &base)) {
					continue; // ignored — silent
				}
				self.add_file(file, &alias);
			}
		} else if path.is_file() {
			// An explicitly named file is not subject to ignore globs (the user
			// pointed at it directly).
			let alias = source.alias.clone();
			self.add_file(&path, &alias);
		} else {
			self.warnings
				.push(format!("seed source not found: {}", path.display()));
		}
	}

	fn add_file(&mut self, file: &Path, alias: &Option<String>) {
		let name = file.file_stem().unwrap().to_string_lossy().into_owned();
		let src = match std::fs::read_to_string(file) {
			Ok(s) => s,
			Err(e) => {
				self.warnings.push(format!("{}: {e}", file.display()));
				return;
			}
		};
		match classify(&src) {
			SeedKind::Shell => self.seeds.push(Seed {
				name,
				alias: alias.clone(),
				path: file.to_path_buf(),
			}),
			SeedKind::Flake => self.warnings.push(format!(
				"{}: looks like a flake.nix (flake seeds not supported yet) — skipped",
				file.display()
			)),
			SeedKind::Invalid => self
				.warnings
				.push(format!("{}: not valid nix — skipped", file.display())),
		}
	}
}

/// Classify a `*.nix` source: a parse error → [`SeedKind::Invalid`]; a top-level
/// attrset carrying an `outputs` attribute → [`SeedKind::Flake`]; otherwise a
/// usable [`SeedKind::Shell`]. Cheap: syntactic parse only, no eval.
pub fn classify(src: &str) -> SeedKind {
	let parse = rnix::Root::parse(src);
	if parse.errors().first().is_some() {
		return SeedKind::Invalid;
	}
	if looks_like_flake(&parse.syntax()) {
		SeedKind::Flake
	} else {
		SeedKind::Shell
	}
}

/// A flake shape = the top-level expression is an attrset with an `outputs`
/// attribute. A shell fragment's top expression is a lambda (`{ pkgs }: …`) or an
/// application (`pkgs.mkShell …`) / `let … in …`, never a bare attrset with
/// `outputs`.
fn looks_like_flake(root: &SyntaxNode) -> bool {
	let Some(top) = root.first_child() else {
		return false;
	};
	if top.kind() != SyntaxKind::NODE_ATTR_SET {
		return false;
	}
	top.children()
		.filter(|n| n.kind() == SyntaxKind::NODE_ATTRPATH_VALUE)
		.filter_map(|av| av.children().find(|c| c.kind() == SyntaxKind::NODE_ATTRPATH))
		.any(|p| p.text().to_string().trim() == "outputs")
}

/// The globs in a directory's `.clinix_ignore` (gitignore-lite: one glob per line,
/// blanks and `#` comments skipped). Missing file → none.
fn read_clinix_ignore(dir: &Path) -> Vec<String> {
	std::fs::read_to_string(dir.join(".clinix_ignore"))
		.map(|text| {
			text.lines()
				.map(str::trim)
				.filter(|l| !l.is_empty() && !l.starts_with('#'))
				.map(str::to_string)
				.collect()
		})
		.unwrap_or_default()
}

/// Does an ignore glob match a candidate under the source dir? gitignore-ish: a
/// pattern containing `/` matches the path **relative to the source** (`lib/**`,
/// `lib/*.nix`); a pattern without `/` matches the **basename at any depth**
/// (`_*.nix`).
fn ignore_matches(pattern: &str, rel_path: &str, basename: &str) -> bool {
	if pattern.contains('/') {
		glob_match(pattern.as_bytes(), rel_path.as_bytes())
	} else {
		glob_match(pattern.as_bytes(), basename.as_bytes())
	}
}

/// Wildcard match with gitignore-ish path semantics: `?` and `*` match any char
/// **except `/`**; `**` (optionally followed by `/`) matches any run **including
/// `/`**; other chars match literally. Recursive.
fn glob_match(pat: &[u8], s: &[u8]) -> bool {
	match pat.first() {
		None => s.is_empty(),
		Some(b'*') if pat.get(1) == Some(&b'*') => {
			// `**` (optionally `**/`) matches any run, crossing `/`.
			let rest = match pat.get(2) {
				Some(b'/') => &pat[3..],
				_ => &pat[2..],
			};
			(0..=s.len()).any(|i| glob_match(rest, &s[i..]))
		}
		Some(b'*') => {
			glob_match(&pat[1..], s)
				|| (s.first().is_some_and(|&c| c != b'/') && glob_match(pat, &s[1..]))
		}
		Some(b'?') => s.first().is_some_and(|&c| c != b'/') && glob_match(&pat[1..], &s[1..]),
		Some(&p) => s.first() == Some(&p) && glob_match(&pat[1..], &s[1..]),
	}
}

/// A nix compose expression that imports the pinned nixpkgs (read from `lock`) and
/// **unions** the `seeds` fragments via `inputsFrom` (the `compose-dev.nix`
/// mechanism). `seeds` are the fragment files **in composition order**; `label`
/// is the human name (e.g. `"claude rust"`). All seeds share the one pin, so there
/// is a single nixpkgs instance (fragments-only, no multi-pin — the design's §5).
pub fn compose_expr(
	lock: &Path,
	seeds: &[PathBuf],
	label: &str,
	nixpkgs_config: Option<&Path>,
) -> String {
	let imports = seeds
		.iter()
		.map(|p| format!("    (import {} {{ inherit pkgs; }})", nix_str(p)))
		.collect::<Vec<_>>()
		.join("\n");
	let sanitized = label.replace(' ', "-");
	COMPOSE_TEMPLATE
		.replace("@LOCK@", &nix_str(lock))
		.replace("@CONFIG@", &nixpkgs_config_frag(nixpkgs_config))
		.replace("@IMPORTS@", &imports)
		.replace("@NAME@", &sanitized)
		.replace("@LABEL@", label)
}

/// The ` config = import "<path>";` fragment for the nixpkgs import, or empty (so
/// nixpkgs uses its default `~/.config/nixpkgs/config.nix`).
pub(crate) fn nixpkgs_config_frag(config: Option<&Path>) -> String {
	match config {
		Some(p) => format!(" config = import {};", nix_str(p)),
		None => String::new(),
	}
}

/// The compose expression template. Paths are substituted as escaped nix strings
/// (coerced to paths by `readFile`/`import`); `@LABEL@` is the un-sanitised name.
const COMPOSE_TEMPLATE: &str = r##"{ system ? builtins.currentSystem }:
let
  lock = builtins.fromJSON (builtins.readFile @LOCK@);
  fetch = node:
    let i = lock.nodes.${node}.locked; in
    if i.type == "github" then
      builtins.fetchTarball { url = "https://github.com/${i.owner}/${i.repo}/archive/${i.rev}.tar.gz"; sha256 = i.narHash; }
    else if i.type == "git" then
      (builtins.fetchGit { inherit (i) url rev; }).outPath
    else throw "clinix compose: unsupported input type '${i.type}'";
  sources = builtins.mapAttrs (_: fetch) lock.nodes.root.inputs;
  pkgs = import sources.nixpkgs { inherit system;@CONFIG@ };
in
pkgs.mkShell {
  name = "@NAME@";
  inputsFrom = [
@IMPORTS@
  ];
  shellHook = "export name=${pkgs.lib.escapeShellArg ''@LABEL@''}\n";
}
"##;

/// A path as an escaped, double-quoted nix string literal (nix coerces it to a
/// path where one is expected). Escapes `\`, `"`, and `${` (interpolation).
fn nix_str(path: &Path) -> String {
	let s = path.to_string_lossy();
	let escaped = s
		.replace('\\', "\\\\")
		.replace('"', "\\\"")
		.replace("${", "\\${");
	format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
	use super::*;

	fn source(path: &Path, alias: Option<&str>) -> SeedSource {
		SeedSource {
			path: path.to_path_buf(),
			alias: alias.map(str::to_string),
		}
	}

	const FRAGMENT: &str = "{ pkgs }: pkgs.mkShell { packages = with pkgs; [ ripgrep ]; }\n";
	const FLAKE: &str =
		"{\n  inputs.nixpkgs.url = \"github:NixOS/nixpkgs\";\n  outputs = { self, nixpkgs }: { };\n}\n";

	#[test]
	fn classify_distinguishes_shell_flake_and_invalid() {
		assert_eq!(classify(FRAGMENT), SeedKind::Shell);
		assert_eq!(
			classify("let lock = builtins.fromJSON (''{}''); in pkgs.mkShell { }\n"),
			SeedKind::Shell
		);
		assert_eq!(classify(FLAKE), SeedKind::Flake);
		assert_eq!(classify("{ this is ( not valid"), SeedKind::Invalid);
	}

	#[test]
	fn unaliased_source_yields_no_alias() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("rust.nix"), FRAGMENT).unwrap();
		let settings = SeedSettings {
			sources: vec![source(dir.path(), None)], // no alias set in config
			ignore: vec![],
		};
		let cat = Catalog::build(&settings);
		assert_eq!(cat.seeds.len(), 1);
		assert_eq!(cat.seeds[0].alias, None, "no derived-basename default");
	}

	#[test]
	fn glob_matches_basename_and_path_patterns() {
		// basename patterns (no `/`).
		assert!(glob_match(b"_*.nix", b"_helper.nix"));
		assert!(!glob_match(b"_*.nix", b"claude.nix"));
		assert!(glob_match(b"*.nix", b"claude.nix"));
		assert!(glob_match(b"claude.???", b"claude.nix"));
		// path patterns: `**` crosses `/`, `*` does not.
		assert!(glob_match(b"lib/**", b"lib/helper.nix"));
		assert!(glob_match(b"lib/**", b"lib/sub/deep.nix"));
		assert!(glob_match(b"lib/*.nix", b"lib/a.nix"));
		assert!(!glob_match(b"lib/*.nix", b"lib/sub/a.nix"));
		assert!(!glob_match(b"*.nix", b"sub/a.nix"));
	}

	#[test]
	fn build_recursively_catalogs_shells_warns_and_honors_path_ignores() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("claude.nix"), FRAGMENT).unwrap();
		std::fs::write(dir.path().join("rust.nix"), FRAGMENT).unwrap();
		std::fs::write(dir.path().join("_helper.nix"), FRAGMENT).unwrap(); // ignored (basename)
		std::fs::write(dir.path().join("flake.nix"), FLAKE).unwrap(); // warned
		std::fs::write(dir.path().join("broken.nix"), "{ ( ").unwrap(); // warned
		// a nested seed (recursive scan) and a lib/ dir ignored by a path glob.
		std::fs::create_dir_all(dir.path().join("lang")).unwrap();
		std::fs::write(dir.path().join("lang/go.nix"), FRAGMENT).unwrap();
		std::fs::create_dir_all(dir.path().join("lib")).unwrap();
		std::fs::write(dir.path().join("lib/helper.nix"), FRAGMENT).unwrap(); // ignored (path)

		let settings = SeedSettings {
			sources: vec![source(dir.path(), Some("dev"))],
			ignore: vec!["_*.nix".to_string(), "lib/**".to_string()],
		};
		let cat = Catalog::build(&settings);

		let mut names: Vec<&str> = cat.seeds.iter().map(|s| s.name.as_str()).collect();
		names.sort();
		// `go` comes from the lang/ subdir; lib/ + _helper are ignored.
		assert_eq!(names, vec!["claude", "go", "rust"]);
		assert!(cat.seeds.iter().all(|s| s.alias.as_deref() == Some("dev")));
		// flake.nix + broken.nix warn; ignored files are silent.
		assert_eq!(cat.warnings.len(), 2, "warnings: {:?}", cat.warnings);
		assert!(cat.warnings.iter().any(|w| w.contains("flake")));
	}

	#[test]
	fn compose_expr_unions_fragments_in_order_against_one_pin() {
		let lock = PathBuf::from("/cfg/flake.lock");
		let seeds = vec![PathBuf::from("/s/rust.nix"), PathBuf::from("/s/claude.nix")];
		let expr = compose_expr(&lock, &seeds, "rust claude", None);
		assert!(expr.contains("builtins.readFile \"/cfg/flake.lock\""));
		assert!(expr.contains("import \"/s/rust.nix\" { inherit pkgs; }"));
		assert!(expr.contains("import \"/s/claude.nix\" { inherit pkgs; }"));
		// composition order preserved (caller sorts lexically or honors -o).
		assert!(expr.find("/s/rust.nix").unwrap() < expr.find("/s/claude.nix").unwrap());
		assert!(expr.contains("inputsFrom"));
		assert!(expr.contains("name = \"rust-claude\";"), "sanitized store name");
		assert!(expr.contains("rust claude"), "un-sanitized label in the hook");
		assert!(!expr.contains("config = import"), "no nixpkgs config by default");

		// A nixpkgs config file is applied to the nixpkgs import (allowUnfree etc.).
		let cfg = PathBuf::from("/cfg/nixpkgs-config.nix");
		let expr2 = compose_expr(&lock, &seeds, "rust claude", Some(&cfg));
		assert!(expr2.contains("config = import \"/cfg/nixpkgs-config.nix\";"));
	}

	#[test]
	fn find_resolves_bare_alias_and_collision() {
		let a = tempfile::tempdir().unwrap();
		let b = tempfile::tempdir().unwrap();
		std::fs::write(a.path().join("claude.nix"), FRAGMENT).unwrap();
		std::fs::write(a.path().join("rust.nix"), FRAGMENT).unwrap();
		std::fs::write(b.path().join("claude.nix"), FRAGMENT).unwrap();

		// Source `a` (alias "one") is higher precedence than `b` (alias "two").
		let settings = SeedSettings {
			sources: vec![source(a.path(), Some("one")), source(b.path(), Some("two"))],
			ignore: vec![],
		};
		let cat = Catalog::build(&settings);

		// Unique bare name.
		assert!(matches!(cat.find("rust"), Some(Resolved::One(s)) if s.name == "rust"));
		// Qualified form selects a specific source.
		assert!(
			matches!(cat.find("two:claude"), Some(Resolved::One(s)) if s.alias.as_deref() == Some("two"))
		);
		// Bare collision: highest-precedence source wins, the other is reported.
		match cat.find("claude") {
			Some(Resolved::Collision { chosen, others }) => {
				assert_eq!(chosen.alias.as_deref(), Some("one"));
				assert_eq!(others.len(), 1);
				assert_eq!(others[0].alias.as_deref(), Some("two"));
			}
			other => panic!("expected a collision, got {other:?}"),
		}
		// Unknown name.
		assert!(cat.find("nope").is_none());
	}
}
