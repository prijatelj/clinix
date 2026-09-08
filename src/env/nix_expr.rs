//! Shared fragments for the nix files clinix generates. One source of truth for
//! (1) escaping a host path into a nix string literal and (2) the `let`-prelude
//! every generated file opens with — read `flake.lock`, `fetch` each input, and
//! build `pkgs`. The seed-stack compose file ([`crate::env::seeds`]), the `new`
//! register/compose wrappers ([`crate::env::new`]), and the docker image base
//! ([`crate::ext::export_docker`]) all assemble their output from these, so the
//! lock-reader logic lives in exactly one place instead of four.
//!
//! Pure `&str`/`&Path` → `String` — no I/O — so callers stay unit-testable.

use std::path::Path;

/// A path as an escaped, double-quoted nix string literal (nix coerces it to a
/// path where one is expected, e.g. `readFile`/`import`). Escapes `\`, `"`, and
/// `${` (interpolation).
pub fn nix_str(path: &Path) -> String {
	let s = path.to_string_lossy();
	let escaped = s
		.replace('\\', "\\\\")
		.replace('"', "\\\"")
		.replace("${", "\\${");
	format!("\"{escaped}\"")
}

/// The ` config = import "<path>";` fragment for the nixpkgs import, or empty (so
/// nixpkgs uses its default `~/.config/nixpkgs/config.nix`). Goes in `@CONFIG@`.
pub fn nixpkgs_config_frag(config: Option<&Path>) -> String {
	match config {
		Some(p) => format!(" config = import {};", nix_str(p)),
		None => String::new(),
	}
}

/// The `let`-prelude shared by every clinix-generated nix file: the
/// `{ system ? … }:` header plus the `flake.lock` reader (`lock`/`fetch`/`sources`)
/// and the `pkgs` binding. It stops **before** `in`, so a caller appends any extra
/// `let` bindings (e.g. docker's `shell`/`envPackages`) and then its own
/// `in <body>`. Substituted by [`lock_prelude`]:
/// - `@LOCK@` — the lock reference placed after `readFile ` (a [`nix_str`] path, or
///   a bare relative literal like `./flake.lock`);
/// - `@CONFIG@` — the optional nixpkgs `config` fragment ([`nixpkgs_config_frag`]),
///   empty when none.
pub const LOCK_PRELUDE: &str = r##"{ system ? builtins.currentSystem }:
let
  lock = builtins.fromJSON (builtins.readFile @LOCK@);
  fetch = node:
    let i = lock.nodes.${node}.locked; in
    if i.type == "github" then
      builtins.fetchTarball { url = "https://github.com/${i.owner}/${i.repo}/archive/${i.rev}.tar.gz"; sha256 = i.narHash; }
    else if i.type == "git" then
      (builtins.fetchGit { inherit (i) url rev; }).outPath
    else throw "clinix: unsupported input type '${i.type}'";
  sources = builtins.mapAttrs (_: fetch) lock.nodes.root.inputs;
  pkgs = import sources.nixpkgs { inherit system;@CONFIG@ };
"##;

/// Fill [`LOCK_PRELUDE`]: `lock_ref` is the exact text after `readFile ` (already a
/// [`nix_str`] result or a bare path); `config_frag` is the `@CONFIG@` fragment
/// (`""` or a [`nixpkgs_config_frag`] string).
pub fn lock_prelude(lock_ref: &str, config_frag: &str) -> String {
	LOCK_PRELUDE
		.replace("@LOCK@", lock_ref)
		.replace("@CONFIG@", config_frag)
}

/// The `mkShell` tail shared by the seed-stack compose file and the `new` composed
/// env: unions the imported shells via `inputsFrom`, in composition order, and
/// exports a human `name`. Follows a [`lock_prelude`] (it opens with `in`).
/// `@NAME@` is the sanitized store name, `@IMPORTS@` the newline-joined
/// `(import … { … })` lines, `@LABEL@` the un-sanitized human label.
pub const MKSHELL_TAIL: &str = r##"in
pkgs.mkShell {
  name = "@NAME@";
  inputsFrom = [
@IMPORTS@
  ];
  shellHook = "export name=${pkgs.lib.escapeShellArg ''@LABEL@''}\n";
}
"##;

/// Assemble the `mkShell` composition: [`lock_prelude`] + [`MKSHELL_TAIL`] with the
/// name/imports/label filled. Shared by seed-stack compose and `new` compose (they
/// differ only in `lock_ref`/`config_frag` and how each builds `imports`).
pub fn compose_shell(
	lock_ref: &str,
	config_frag: &str,
	name: &str,
	imports: &str,
	label: &str,
) -> String {
	let tail = MKSHELL_TAIL
		.replace("@NAME@", name)
		.replace("@IMPORTS@", imports)
		.replace("@LABEL@", label);
	format!("{}{tail}", lock_prelude(lock_ref, config_frag))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn nix_str_escapes_backslash_quote_and_interpolation() {
		assert_eq!(nix_str(Path::new("/a/b")), "\"/a/b\"");
		assert_eq!(nix_str(Path::new("/a b/c")), "\"/a b/c\"");
		// `"`, `\`, and `${` are the three nix-string-significant sequences.
		assert_eq!(nix_str(Path::new("a${x}")), "\"a\\${x}\"");
	}

	#[test]
	fn config_frag_is_empty_or_an_import() {
		assert_eq!(nixpkgs_config_frag(None), "");
		assert_eq!(
			nixpkgs_config_frag(Some(Path::new("/c.nix"))),
			" config = import \"/c.nix\";"
		);
	}

	#[test]
	fn lock_prelude_substitutes_lock_and_config() {
		let p = lock_prelude("\"/cfg/flake.lock\"", " config = import \"/c.nix\";");
		assert!(p.contains("builtins.readFile \"/cfg/flake.lock\""));
		assert!(
			p.contains("import sources.nixpkgs { inherit system; config = import \"/c.nix\"; };")
		);
		// A bare relative lock literal (composed env) is placed verbatim.
		assert!(lock_prelude("./flake.lock", "").contains("builtins.readFile ./flake.lock"));
		// Empty config leaves a bare `{ inherit system; }`.
		assert!(
			lock_prelude("./flake.lock", "")
				.contains("import sources.nixpkgs { inherit system; };")
		);
	}

	#[test]
	fn compose_shell_wires_prelude_and_mkshell_tail() {
		let imports = "    (import ./seeds/rust.nix { inherit pkgs; })";
		let s = compose_shell("./flake.lock", "", "rust-claude", imports, "rust claude");
		assert!(s.contains("builtins.readFile ./flake.lock"));
		assert!(
			s.contains("inputsFrom = [\n    (import ./seeds/rust.nix { inherit pkgs; })\n  ];")
		);
		assert!(s.contains("name = \"rust-claude\";"));
		assert!(
			s.contains("''rust claude''"),
			"un-sanitized label in the shellHook"
		);
	}
}
