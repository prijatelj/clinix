//! Span-preserving edits to a `shell.nix`'s `packages = with pkgs; [ … ];` list,
//! via `rnix` (a lossless Nix CST). We parse only to **locate** the list and its
//! element spans, then apply the change as a **string splice** on the original
//! source — so everything else (comments, formatting, hand edits, and a
//! single-file env's embedded-lock string) stays byte-identical.
//!
//! Pure `&str → String`: no nix eval, no I/O — richly unit-testable without nix.
//! Reused by `pkgs::{add, remove}`; `pin` will reuse the same locator later.

use rnix::{SyntaxKind, SyntaxNode, SyntaxToken};

use crate::error::{ClinixError, Result};

/// The outcome of an edit: the new source plus which names actually changed vs
/// were skipped (already present / not present), for grouped reporting.
#[derive(Debug)]
pub struct Edit {
	pub source: String,
	pub changed: Vec<String>,
	pub skipped: Vec<String>,
}

/// Append `names` to the `packages` list (idempotent: names already present are
/// skipped). Preserves the list's style — a new line per name for a multiline
/// list, space-separated for an inline one.
pub fn add_packages(src: &str, names: &[String]) -> Result<Edit> {
	let list = packages_list(src)?;
	let present: Vec<String> = elements(&list).iter().map(node_text).collect();

	let mut to_insert: Vec<String> = Vec::new();
	let mut changed = Vec::new();
	let mut skipped = Vec::new();
	for name in names {
		if present.contains(name) || to_insert.contains(name) {
			skipped.push(name.clone());
		} else {
			to_insert.push(name.clone());
			changed.push(name.clone());
		}
	}
	if to_insert.is_empty() {
		return Ok(Edit {
			source: src.to_string(),
			changed,
			skipped,
		});
	}

	let (multiline, indent) = list_style(&list);
	let rbrack = r_brack(&list)?;
	let at = leading_ws_start(rbrack.prev_token(), rbrack.text_range().start().into());
	let insertion: String = to_insert
		.iter()
		.map(|n| {
			if multiline {
				format!("\n{indent}{n}")
			} else {
				format!(" {n}")
			}
		})
		.collect();

	let mut source = String::with_capacity(src.len() + insertion.len());
	source.push_str(&src[..at]);
	source.push_str(&insertion);
	source.push_str(&src[at..]);
	Ok(Edit {
		source,
		changed,
		skipped,
	})
}

/// Remove `names` from the `packages` list (idempotent: names not present are
/// skipped). Deletes each matching element together with its leading whitespace
/// so no blank line or dangling separator is left behind.
pub fn remove_packages(src: &str, names: &[String]) -> Result<Edit> {
	let list = packages_list(src)?;

	let mut ranges: Vec<(usize, usize)> = Vec::new();
	let mut changed = Vec::new();
	for el in elements(&list) {
		let text = node_text(&el);
		if names.contains(&text) {
			let start = leading_ws_start(
				el.prev_sibling_or_token().and_then(|e| e.into_token()),
				usize::from(el.text_range().start()),
			);
			ranges.push((start, usize::from(el.text_range().end())));
			if !changed.contains(&text) {
				changed.push(text);
			}
		}
	}
	let skipped: Vec<String> = names
		.iter()
		.filter(|n| !changed.contains(n))
		.cloned()
		.collect();

	let mut source = src.to_string();
	// Apply right-to-left so earlier byte offsets stay valid.
	ranges.sort_by(|a, b| b.0.cmp(&a.0));
	for (start, end) in ranges {
		source.replace_range(start..end, "");
	}
	Ok(Edit {
		source,
		changed,
		skipped,
	})
}

/// Reorder the `packages` list lexically. Unlike `add`/`remove` this is a
/// *reformatting* op: element order is normalized and in-list comments are not
/// preserved (opt-in via `--sort`). Style (multiline indent / inline) is kept.
pub fn sort_packages(src: &str) -> Result<String> {
	let list = packages_list(src)?;
	let mut els: Vec<String> = elements(&list).iter().map(node_text).collect();
	els.sort();
	els.dedup();

	let l = l_brack(&list)?;
	let r = r_brack(&list)?;
	let (inner_start, inner_end) = (
		usize::from(l.text_range().end()),
		usize::from(r.text_range().start()),
	);

	let (multiline, indent) = list_style(&list);
	let inner = if multiline {
		let mut s = String::new();
		for e in &els {
			s.push('\n');
			s.push_str(&indent);
			s.push_str(e);
		}
		s.push('\n');
		s.push_str(&rbrack_indent(&r));
		s
	} else if els.is_empty() {
		" ".to_string()
	} else {
		format!(" {} ", els.join(" "))
	};

	let mut out = String::with_capacity(src.len());
	out.push_str(&src[..inner_start]);
	out.push_str(&inner);
	out.push_str(&src[inner_end..]);
	Ok(out)
}

// ---- rnix locating helpers --------------------------------------------------

/// Find the `packages` attribute's list node (handles `packages = [ … ]` and
/// `packages = with pkgs; [ … ]`).
fn packages_list(src: &str) -> Result<SyntaxNode> {
	let parse = rnix::Root::parse(src);
	if let Some(err) = parse.errors().first() {
		return Err(ClinixError::ShellNix(format!("parse error: {err}")));
	}
	parse
		.syntax()
		.descendants()
		.filter(|n| n.kind() == SyntaxKind::NODE_ATTRPATH_VALUE)
		.find(|av| {
			av.children()
				.find(|c| c.kind() == SyntaxKind::NODE_ATTRPATH)
				.is_some_and(|ap| ap.text().to_string().trim() == "packages")
		})
		.and_then(|av| av.descendants().find(|n| n.kind() == SyntaxKind::NODE_LIST))
		.ok_or_else(|| {
			ClinixError::ShellNix(
				"no `packages = with pkgs; [ … ];` list found (edit shell.nix by hand or re-init)"
					.into(),
			)
		})
}

/// The list's element nodes (the `[`/`]` and comments are tokens, so excluded).
fn elements(list: &SyntaxNode) -> Vec<SyntaxNode> {
	list.children().collect()
}

fn node_text(node: &SyntaxNode) -> String {
	node.text().to_string()
}

fn r_brack(list: &SyntaxNode) -> Result<SyntaxToken> {
	list.children_with_tokens()
		.filter_map(|e| e.into_token())
		.find(|t| t.kind() == SyntaxKind::TOKEN_R_BRACK)
		.ok_or_else(|| ClinixError::ShellNix("packages list has no closing `]`".into()))
}

fn l_brack(list: &SyntaxNode) -> Result<SyntaxToken> {
	list.children_with_tokens()
		.filter_map(|e| e.into_token())
		.find(|t| t.kind() == SyntaxKind::TOKEN_L_BRACK)
		.ok_or_else(|| ClinixError::ShellNix("packages list has no opening `[`".into()))
}

/// The indentation of the closing `]`'s line (so it stays aligned after a sort).
fn rbrack_indent(rbrack: &SyntaxToken) -> String {
	match rbrack.prev_token() {
		Some(t) if t.kind() == SyntaxKind::TOKEN_WHITESPACE => t
			.text()
			.rfind('\n')
			.map(|nl| t.text()[nl + 1..].to_string())
			.unwrap_or_default(),
		_ => String::new(),
	}
}

/// `(multiline, element-indent)` — read from the whitespace right after `[`.
fn list_style(list: &SyntaxNode) -> (bool, String) {
	let lbrack = list
		.children_with_tokens()
		.filter_map(|e| e.into_token())
		.find(|t| t.kind() == SyntaxKind::TOKEN_L_BRACK);
	if let Some(ws) = lbrack
		.and_then(|t| t.next_token())
		.filter(|t| t.kind() == SyntaxKind::TOKEN_WHITESPACE)
	{
		let text = ws.text();
		if let Some(nl) = text.rfind('\n') {
			return (true, text[nl + 1..].to_string());
		}
	}
	(false, String::new())
}

/// Byte offset to start an edit at: the start of `ws` when it's leading
/// whitespace, else `fallback`.
fn leading_ws_start(ws: Option<SyntaxToken>, fallback: usize) -> usize {
	match ws {
		Some(t) if t.kind() == SyntaxKind::TOKEN_WHITESPACE => usize::from(t.text_range().start()),
		_ => fallback,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const MULTILINE: &str = "\
pkgs.mkShell {
  packages = with pkgs; [
    ripgrep
    jq
  ];
}
";

	fn names(v: &[&str]) -> Vec<String> {
		v.iter().map(|s| s.to_string()).collect()
	}

	#[test]
	fn add_appends_to_multiline_list_preserving_indent() {
		let e = add_packages(MULTILINE, &names(&["nodejs"])).unwrap();
		assert_eq!(e.changed, names(&["nodejs"]));
		assert!(
			e.source.contains("    jq\n    nodejs\n  ];"),
			"\n{}",
			e.source
		);
	}

	#[test]
	fn add_is_idempotent() {
		let e = add_packages(MULTILINE, &names(&["jq", "nodejs"])).unwrap();
		assert_eq!(e.changed, names(&["nodejs"]));
		assert_eq!(e.skipped, names(&["jq"]));
		// jq not duplicated.
		assert_eq!(e.source.matches("jq").count(), 1);
	}

	#[test]
	fn add_to_inline_list_is_space_separated() {
		let src = "mkShell { packages = with pkgs; [ ripgrep jq ]; }";
		let e = add_packages(src, &names(&["nodejs"])).unwrap();
		assert_eq!(
			e.source,
			"mkShell { packages = with pkgs; [ ripgrep jq nodejs ]; }"
		);
	}

	#[test]
	fn remove_deletes_element_and_its_line() {
		let e = remove_packages(MULTILINE, &names(&["ripgrep"])).unwrap();
		assert_eq!(e.changed, names(&["ripgrep"]));
		assert!(!e.source.contains("ripgrep"));
		assert!(
			e.source.contains("  packages = with pkgs; [\n    jq\n  ];"),
			"\n{}",
			e.source
		);
	}

	#[test]
	fn remove_absent_is_skipped() {
		let e = remove_packages(MULTILINE, &names(&["nodejs"])).unwrap();
		assert!(e.changed.is_empty());
		assert_eq!(e.skipped, names(&["nodejs"]));
		assert_eq!(e.source, MULTILINE); // untouched
	}

	#[test]
	fn no_packages_list_errors() {
		let err = add_packages("pkgs.mkShell { }", &names(&["ripgrep"])).unwrap_err();
		assert!(matches!(err, ClinixError::ShellNix(_)));
	}

	#[test]
	fn single_file_embedded_lock_is_untouched() {
		// A single-file shell.nix: an embedded-lock string *and* a packages list.
		let src = "\
let lock = builtins.fromJSON ''
{ \"nodes\": { \"nixpkgs\": {} }, \"version\": 7 }
''; in
pkgs.mkShell {
  packages = with pkgs; [
    ripgrep
  ];
}
";
		let e = add_packages(src, &names(&["jq"])).unwrap();
		assert!(
			e.source.contains("    ripgrep\n    jq\n  ];"),
			"packages edited"
		);
		// The embedded lock JSON string is byte-for-byte intact.
		assert!(
			e.source
				.contains("{ \"nodes\": { \"nixpkgs\": {} }, \"version\": 7 }")
		);
	}

	#[test]
	fn sort_orders_multiline_list_and_keeps_indent() {
		let out = sort_packages(MULTILINE).unwrap();
		assert!(out.contains("    jq\n    ripgrep\n  ];"), "\n{out}");
	}

	#[test]
	fn sort_orders_inline_list() {
		let src = "mkShell { packages = with pkgs; [ ripgrep jq nodejs ]; }";
		assert_eq!(
			sort_packages(src).unwrap(),
			"mkShell { packages = with pkgs; [ jq nodejs ripgrep ]; }"
		);
	}
}
