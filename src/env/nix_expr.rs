//! Shared fragments for the nix files clinix generates. One source of truth for
//! (1) escaping a host path into a nix string literal and (2) the `let`-prelude
//! every generated file opens with — read `flake.lock`, `fetch` each input, and
//! build `pkgs`. The seed-stack compose file ([`crate::env::seeds`]), the `new`
//! register/compose wrappers ([`crate::env::new`]), and the docker image base
//! ([`crate::ext::export_docker`]) all assemble their output from these, so the
//! lock-reader logic lives in exactly one place instead of four.
//!
//! Pure `&str`/`&Path` → `String` — no I/O — so callers stay unit-testable.

use std::path::{Path, PathBuf};

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
/// `{ system ? … }:` header plus the `flake.lock` reader (`fetch`/`sourcesOf`/
/// `pkgsOf`), the `pkgs` binding, and — when any member asked for them — the
/// named alternate pins as `pins`. It stops **before** `in`, so a caller appends
/// any extra `let` bindings (e.g. docker's `shell`/`envPackages`) and then its own
/// `in <body>`. Substituted by [`lock_prelude`]:
/// - `@LOCK@` — the lock reference placed after `readFile ` (a [`nix_str`] path, or
///   a bare relative literal like `./flake.lock`);
/// - `@CONFIG@` — the optional nixpkgs `config` fragment ([`nixpkgs_config_frag`]),
///   empty when none;
/// - `@PINS@` — the optional `pins = { … };` block ([`pins_frag`]), empty when no
///   member requested an alternate pin (so the generated file is byte-identical to
///   one without the feature).
///
/// The reader is **lock-parameterized** (`sourcesOf`/`pkgsOf` take the lockfile)
/// rather than closing over a single `lock` binding, because that is what lets one
/// generated file instantiate nixpkgs from more than one pin — the stack's pin for
/// `pkgs`, plus one per `[env.pins]` entry — from a single definition of the fetch
/// logic. `fetch` therefore takes a node's `locked` attrset, not its node name.
pub const LOCK_PRELUDE: &str = r##"{ system ? builtins.currentSystem }:
let
  fetch = i:
    if i.type == "github" then
      builtins.fetchTarball { url = "https://github.com/${i.owner}/${i.repo}/archive/${i.rev}.tar.gz"; sha256 = i.narHash; }
    else if i.type == "git" then
      (builtins.fetchGit { inherit (i) url rev; }).outPath
    else throw "clinix: unsupported input type '${i.type}'";
  sourcesOf = lockFile:
    let lock = builtins.fromJSON (builtins.readFile lockFile); in
    builtins.mapAttrs (_: node: fetch lock.nodes.${node}.locked) lock.nodes.root.inputs;
  pkgsOf = lockFile: import (sourcesOf lockFile).nixpkgs { inherit system;@CONFIG@ };
  pkgs = pkgsOf @LOCK@;
@PINS@"##;

/// Fill [`LOCK_PRELUDE`] with no alternate pins — the shape every generated file
/// had before `[env.pins]` existed. `lock_ref` is the exact text after `readFile `
/// (already a [`nix_str`] result or a bare path); `config_frag` is the `@CONFIG@`
/// fragment (`""` or a [`nixpkgs_config_frag`] string).
pub fn lock_prelude(lock_ref: &str, config_frag: &str) -> String {
	lock_prelude_with_pins(lock_ref, config_frag, &[])
}

/// Fill [`LOCK_PRELUDE`], additionally binding `pins` to one nixpkgs instance per
/// entry of `pins` (see [`pins_frag`]). Empty `pins` ⇒ identical to
/// [`lock_prelude`].
pub fn lock_prelude_with_pins(
	lock_ref: &str,
	config_frag: &str,
	pins: &[(String, PathBuf)],
) -> String {
	LOCK_PRELUDE
		.replace("@LOCK@", lock_ref)
		.replace("@CONFIG@", config_frag)
		.replace("@PINS@", &pins_frag(pins))
}

/// The `pins = { <name> = pkgsOf "<lock>"; … };` block for the declared alternate
/// nixpkgs pins, or **empty** when there are none. Goes in `@PINS@`.
///
/// Every declared pin is bound, not just the ones a seed names, because nix
/// attribute sets are lazy: an unreferenced entry is never forced, so it costs no
/// fetch and no evaluation. That keeps clinix out of the business of working out
/// which seed touches which pin.
pub fn pins_frag(pins: &[(String, PathBuf)]) -> String {
	if pins.is_empty() {
		return String::new();
	}
	let entries: String = pins
		.iter()
		.map(|(name, lock)| format!("    {name} = pkgsOf {};\n", nix_str(lock)))
		.collect();
	format!("  pins = {{\n{entries}  }};\n")
}

/// The `mkShell` tail shared by the seed-stack compose file and the `new` composed
/// env: unions the imported shells via `inputsFrom`, in composition order, and
/// exports a human `name`. Follows a [`lock_prelude`] (it opens with `in`).
/// `@NAME@` is the sanitized store name, `@IMPORTS@` the newline-joined
/// `(import … { … })` lines, `@LABEL@` the un-sanitized human label, and
/// `@PACKAGES@` the optional ad-hoc `packages` line ([`packages_frag`] — empty for
/// a plain composition, so its generated file is byte-identical to before `-p`).
pub const MKSHELL_TAIL: &str = r##"in
pkgs.mkShell {
  name = "@NAME@";
@PACKAGES@  inputsFrom = [
@IMPORTS@
  ];
  shellHook = "export name=${pkgs.lib.escapeShellArg ''@LABEL@''}\n";
}
"##;

/// The `  packages = with pkgs; [ … ];` line for ad-hoc `-p`/`--pkg` packages, or
/// **empty** when there are none. Goes in `@PACKAGES@`.
///
/// Names are spliced as written, so a nixpkgs **attribute path**
/// (`python313Packages.black`) works as well as a bare name — matching
/// `nix-shell -p`, whose arguments are "interpreted as attribute names inside the
/// Nix Packages collection". `with pkgs;` is what makes a bare name resolve.
pub fn packages_frag(packages: &[String]) -> String {
	if packages.is_empty() {
		String::new()
	} else {
		format!("  packages = with pkgs; [ {} ];\n", packages.join(" "))
	}
}

/// Everything the generated `mkShell` composition needs, as **named fields with
/// defaults** rather than a positional parameter list.
///
/// This shape is deliberate: the two callers (the seed-stack compose and `new`'s
/// materialized env) differ only in a couple of fields, and every past widening of
/// this assembler rippled out to both of them plus their callers' callers. With a
/// `Default` struct, a caller that does not care about a new field keeps compiling
/// unchanged — the field is set by the one caller that needs it.
///
/// Fields are the already-rendered fragments, because the two callers build them
/// differently: the seed stack uses quoted absolute paths ([`nix_str`]), while
/// `new` writes bare relative literals (`./flake.lock`, `./seeds/x.nix`) so the
/// materialized env stays portable.
#[derive(Debug, Default, Clone)]
pub struct ComposeSpec<'a> {
	/// The lock reference placed after `readFile ` — a [`nix_str`] result or a bare
	/// relative literal.
	pub lock_ref: &'a str,
	/// The nixpkgs `config` fragment ([`nixpkgs_config_frag`]), or `""` for none.
	pub config_frag: &'a str,
	/// The sanitized store name.
	pub name: &'a str,
	/// Newline-joined `(import … { … })` lines for `inputsFrom`.
	pub imports: &'a str,
	/// The un-sanitized human label, exported by the `shellHook`.
	pub label: &'a str,
	/// Ad-hoc package attribute names; empty ⇒ no `packages` line at all.
	pub packages: &'a [String],
	/// Declared alternate nixpkgs pins to bind as `pins` ([`pins_frag`]); empty ⇒ no
	/// `pins` binding at all.
	pub pins: &'a [(String, PathBuf)],
}

impl ComposeSpec<'_> {
	/// Assemble the composition: [`lock_prelude_with_pins`] + [`MKSHELL_TAIL`] with
	/// every slot filled.
	pub fn render(&self) -> String {
		let tail = MKSHELL_TAIL
			.replace("@NAME@", self.name)
			.replace("@IMPORTS@", self.imports)
			.replace("@LABEL@", self.label)
			.replace("@PACKAGES@", &packages_frag(self.packages));
		format!(
			"{}{tail}",
			lock_prelude_with_pins(self.lock_ref, self.config_frag, self.pins)
		)
	}
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
		assert!(p.contains("pkgs = pkgsOf \"/cfg/flake.lock\";"));
		assert!(p.contains(
			"pkgsOf = lockFile: import (sourcesOf lockFile).nixpkgs \
			 { inherit system; config = import \"/c.nix\"; };"
		));
		// A bare relative lock literal (composed env) is placed verbatim.
		assert!(lock_prelude("./flake.lock", "").contains("pkgs = pkgsOf ./flake.lock;"));
		// Empty config leaves a bare `{ inherit system; }`.
		assert!(
			lock_prelude("./flake.lock", "")
				.contains("import (sourcesOf lockFile).nixpkgs { inherit system; };")
		);
		// No pins requested ⇒ no `pins` binding at all, and the prelude still stops
		// before `in` so callers can append their own bindings.
		assert!(!lock_prelude("./flake.lock", "").contains("pins"));
		assert!(lock_prelude("./flake.lock", "").ends_with(";\n"));
	}

	#[test]
	fn pins_frag_is_empty_or_one_pkgs_of_per_declared_pin() {
		assert_eq!(pins_frag(&[]), "");
		let pins = [
			("unstable".to_string(), PathBuf::from("/c/locks/unstable.lock")),
			("old".to_string(), PathBuf::from("/c/locks/old.lock")),
		];
		assert_eq!(
			pins_frag(&pins),
			"  pins = {\n    unstable = pkgsOf \"/c/locks/unstable.lock\";\n    \
			 old = pkgsOf \"/c/locks/old.lock\";\n  };\n"
		);
	}

	#[test]
	fn lock_prelude_with_pins_binds_every_pin_through_the_one_reader() {
		let pins = [(
			"unstable".to_string(),
			PathBuf::from("/c/locks/unstable.lock"),
		)];
		let p = lock_prelude_with_pins("\"/cfg/flake.lock\"", "", &pins);
		// The stack's pin and the alternate pin both go through `pkgsOf` — the fetch
		// logic is defined exactly once no matter how many pins are in play.
		assert_eq!(p.matches("fetchTarball").count(), 1);
		assert!(p.contains("pkgs = pkgsOf \"/cfg/flake.lock\";"));
		assert!(p.contains("    unstable = pkgsOf \"/c/locks/unstable.lock\";"));
	}

	#[test]
	fn compose_shell_wires_prelude_and_mkshell_tail() {
		let imports = "    (import ./seeds/rust.nix { inherit pkgs; })";
		let s = ComposeSpec {
			lock_ref: "./flake.lock",
			name: "rust-claude",
			imports,
			label: "rust claude",
			..Default::default()
		}
		.render();
		assert!(s.contains("pkgs = pkgsOf ./flake.lock;"));
		assert!(
			s.contains("inputsFrom = [\n    (import ./seeds/rust.nix { inherit pkgs; })\n  ];")
		);
		assert!(s.contains("name = \"rust-claude\";"));
		assert!(
			s.contains("''rust claude''"),
			"un-sanitized label in the shellHook"
		);
		// No `-p` packages → no `packages` line at all (byte-identical to pre-`-p`).
		assert!(!s.contains("packages ="));
	}

	#[test]
	fn packages_frag_is_empty_or_a_with_pkgs_list() {
		assert_eq!(packages_frag(&[]), "");
		assert_eq!(
			packages_frag(&["tokei".to_string(), "fastfetch".to_string()]),
			"  packages = with pkgs; [ tokei fastfetch ];\n"
		);
		// An attribute path is spliced as written (resolved by `with pkgs;`).
		assert_eq!(
			packages_frag(&["python313Packages.black".to_string()]),
			"  packages = with pkgs; [ python313Packages.black ];\n"
		);
	}

	#[test]
	fn compose_shell_splices_adhoc_packages_before_inputs_from() {
		let s = ComposeSpec {
			lock_ref: "./flake.lock",
			name: "claude",
			imports: "    (import /s/claude.nix { inherit pkgs; })",
			label: "claude",
			packages: &["tokei".to_string()],
			..Default::default()
		}
		.render();
		assert!(s.contains("  packages = with pkgs; [ tokei ];\n  inputsFrom = ["));
	}

	#[test]
	fn compose_shell_with_only_packages_has_an_empty_inputs_from() {
		// The `nix-shell -p` case: no envs at all, just packages.
		let s = ComposeSpec {
			lock_ref: "\"/cfg/flake.lock\"",
			name: "pkgs",
			label: "pkgs",
			packages: &["tokei".to_string()],
			..Default::default()
		}
		.render();
		assert!(s.contains("packages = with pkgs; [ tokei ];"));
		assert!(s.contains("inputsFrom = [\n\n  ];"), "valid empty list");
	}
}
