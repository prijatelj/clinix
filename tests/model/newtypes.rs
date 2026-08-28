//! Validated newtypes: `Rev` / `NarHash` boundary validation (public API).

use clinix::model::newtypes::{NarHash, Rev};

#[test]
fn rev_accepts_sha1_and_sha256_lengths() {
	assert!("2f5a153c270b70cb0f8c11f46d96d6d3bc39f4e3".parse::<Rev>().is_ok());
	assert!("a".repeat(64).parse::<Rev>().is_ok());
}

#[test]
fn rev_rejects_wrong_length_case_and_nonhex() {
	assert!("abc".parse::<Rev>().is_err());
	assert!("2F5A153C270B70CB0F8C11F46D96D6D3BC39F4E3".parse::<Rev>().is_err()); // uppercase
	assert!("g".repeat(40).parse::<Rev>().is_err()); // non-hex
}

#[test]
fn narhash_accepts_sri_and_legacy() {
	assert!("sha256-Yjv0WEg39KRYS0rBdTbu6Fc/or/ihAKk13W9sQ6VWd0=".parse::<NarHash>().is_ok());
	assert!("sha256:0m5m5wr5f...".parse::<NarHash>().is_ok());
}

#[test]
fn narhash_rejects_unknown_algo_and_empty_body() {
	assert!("crc32-deadbeef".parse::<NarHash>().is_err());
	assert!("sha256-".parse::<NarHash>().is_err());
	assert!("nohash".parse::<NarHash>().is_err());
}
