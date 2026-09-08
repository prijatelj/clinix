//! Namespace naming rules (Rust-identifier-like) and collision normalization.
//!
//! A **namespace** — a seed source's, the seed default, or a registry env name —
//! must start with an ASCII letter, then contain only letters, digits, `_`, or
//! `-`. `-` and `_` are the **same character for collision** (`my-ns` ≡ `my_ns`),
//! and `:` (the member separator) is forbidden. See
//! `notes/clinix/design/target-resolution.md` §5.
//!
//! Pure string predicates — no I/O — so trivially unit-testable.

/// Validate a namespace name against the Rust-identifier-like rules. Returns the
/// broken rule as a message (for a serde or CLI error), not a typed error, so both
/// config deserialization and runtime validation can wrap it.
pub fn validate_namespace(name: &str) -> Result<(), String> {
	let mut chars = name.chars();
	match chars.next() {
		None => return Err("namespace is empty".to_string()),
		Some(c) if c.is_ascii_alphabetic() => {}
		Some(c) => {
			return Err(format!(
				"namespace must start with a letter (a-zA-Z), got {c:?}"
			));
		}
	}
	for c in chars {
		if c == ':' {
			return Err("namespace may not contain ':' (the member separator)".to_string());
		}
		if !(c.is_ascii_alphanumeric() || c == '_' || c == '-') {
			return Err(format!(
				"namespace may only contain letters, digits, '_' or '-', got {c:?}"
			));
		}
	}
	Ok(())
}

/// The collision key for a namespace: `-` normalized to `_` (they are the same
/// character for collision). Case is significant (Rust-identifier semantics).
pub fn namespace_key(name: &str) -> String {
	name.replace('-', "_")
}

/// Map an arbitrary label to a filesystem/identifier-safe **slug**: characters
/// outside `[A-Za-z0-9_-]` (plus `.` when `dot`) become `_`, and `lowercase` folds
/// case (Docker image names must be lowercase). With `fallback = Some(s)` the
/// result is trimmed of surrounding `_` and replaced by `s` when it would be empty
/// (a whole filename/name); `fallback = None` returns the raw mapping unchanged (a
/// mid-key segment joined with others). Distinct from [`validate_namespace`], which
/// *rejects* an ill-formed name rather than rewriting it.
pub fn slug(label: &str, lowercase: bool, dot: bool, fallback: Option<&str>) -> String {
	let mapped: String = label
		.chars()
		.map(|c| {
			let c = if lowercase { c.to_ascii_lowercase() } else { c };
			if c.is_ascii_alphanumeric() || c == '-' || c == '_' || (dot && c == '.') {
				c
			} else {
				'_'
			}
		})
		.collect();
	match fallback {
		Some(f) => {
			let trimmed = mapped.trim_matches('_');
			if trimmed.is_empty() {
				f.to_string()
			} else {
				trimmed.to_string()
			}
		}
		None => mapped,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn slug_variants_cover_filename_image_and_key_segment() {
		// Filename slug (export closure): trim + "env" fallback, `.`→`_`.
		assert_eq!(slug("python", false, false, Some("env")), "python");
		assert_eq!(
			slug("rust claude", false, false, Some("env")),
			"rust_claude"
		);
		assert_eq!(slug("my.proj", false, false, Some("env")), "my_proj");
		assert_eq!(slug("///", false, false, Some("env")), "env");
		// Docker image name: lowercased, `.` kept.
		assert_eq!(slug("Rust Claude", true, true, Some("env")), "rust_claude");
		assert_eq!(slug("my.env-1", true, true, Some("env")), "my.env-1");
		assert_eq!(slug("///", true, true, Some("env")), "env");
		// Key segment (compose filename): raw mapping, no trim/fallback.
		assert_eq!(slug("a b", false, false, None), "a_b");
		assert_eq!(slug("_x_", false, false, None), "_x_");
	}

	#[test]
	fn validate_namespace_enforces_rust_identifier_shape() {
		assert!(validate_namespace("seeds").is_ok());
		assert!(validate_namespace("my_ns").is_ok());
		assert!(validate_namespace("my-ns").is_ok());
		assert!(validate_namespace("a1-b2_c3").is_ok());
		// Must start with a letter, be non-empty, no `:`, no other punctuation.
		assert!(validate_namespace("").is_err());
		assert!(validate_namespace("1ns").is_err()); // leading digit
		assert!(validate_namespace("_ns").is_err()); // leading underscore
		assert!(validate_namespace("ns:x").is_err()); // member separator
		assert!(validate_namespace("ns.x").is_err()); // stray punctuation
		assert!(validate_namespace("ns/x").is_err());
	}

	#[test]
	fn namespace_key_treats_dash_and_underscore_the_same() {
		assert_eq!(namespace_key("my-ns"), namespace_key("my_ns"));
		assert_eq!(namespace_key("a-b-c"), "a_b_c");
		// Case is significant.
		assert_ne!(namespace_key("Ns"), namespace_key("ns"));
	}
}
