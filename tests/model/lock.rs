//! `flake.lock` model: byte-identical round-trip + classification (public API).

use clinix::model::lock::{FlakeLock, InputRef, LockedKind, Source};

// Real locks from this repo's fixture corpus (byte-for-byte). SIMPLE is a single
// github input; FOLLOWS exercises the string-vs-array `inputs` polymorphism and
// multiple github nodes; GENERATED is live `nix flake lock` output (has
// `lastModified`), so the round-trip is checked against real nix output too.
const SIMPLE: &str = include_str!("../fixtures/simple.lock");
const FOLLOWS: &str = include_str!("../fixtures/follows.lock");
const GENERATED: &str = include_str!("../fixtures/generated.lock");

fn assert_roundtrip(src: &str) {
	let lock = FlakeLock::from_json(src).expect("parse");
	assert_eq!(
		lock.to_json(),
		src,
		"parse → serialize must be byte-identical (interchangeable-writer invariant)"
	);
}

#[test]
fn simple_roundtrip_is_byte_identical() {
	assert_roundtrip(SIMPLE);
}

#[test]
fn follows_roundtrip_is_byte_identical() {
	assert_roundtrip(FOLLOWS);
}

#[test]
fn live_generated_roundtrip_is_byte_identical() {
	assert_roundtrip(GENERATED);
}

#[test]
fn parses_input_ref_polymorphism() {
	let lock = FlakeLock::from_json(FOLLOWS).unwrap();
	// root → direct string edge.
	assert_eq!(
		lock.root_node().unwrap().inputs.get("nixpkgs"),
		Some(&InputRef::Direct("nixpkgs".to_string()))
	);
	// treefmt-nix → a two-element follows path.
	assert_eq!(
		lock.nodes["treefmt-nix"].inputs.get("nixpkgs"),
		Some(&InputRef::Follows(vec![
			"vulnix".to_string(),
			"nixpkgs".to_string()
		]))
	);
}

#[test]
fn github_source_constructors_have_expected_shape() {
	let locked = Source::github_locked("NixOS", "nixpkgs", "abc123", "sha256-x");
	assert_eq!(locked.source_type(), Some("github"));
	assert_eq!(locked.owner(), Some("NixOS"));
	assert_eq!(locked.repo(), Some("nixpkgs"));
	assert_eq!(locked.rev(), Some("abc123"));
	assert_eq!(locked.nar_hash(), Some("sha256-x"));

	let tracked = Source::github_ref("NixOS", "nixpkgs", "nixos-26.05");
	assert_eq!(tracked.git_ref(), Some("nixos-26.05"));
	assert_eq!(tracked.rev(), None);

	let frozen = Source::github_rev("NixOS", "nixpkgs", "abc123");
	assert_eq!(frozen.rev(), Some("abc123"));
	assert_eq!(frozen.git_ref(), None);
}

#[test]
fn classifies_and_reads_github_source() {
	let lock = FlakeLock::from_json(SIMPLE).unwrap();
	let locked = lock.nodes["nixpkgs"].locked.as_ref().unwrap();
	assert_eq!(locked.kind(), LockedKind::Github);
	assert_eq!(locked.owner(), Some("NixOS"));
	assert_eq!(locked.repo(), Some("nixpkgs"));
	assert_eq!(locked.rev(), Some("2f5a153c270b70cb0f8c11f46d96d6d3bc39f4e3"));
	// The root node has no source.
	assert!(lock.root_node().unwrap().locked.is_none());
}
