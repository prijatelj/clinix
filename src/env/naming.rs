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

#[cfg(test)]
mod tests {
	use super::*;

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
