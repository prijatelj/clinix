//! Validated newtypes for `flake.lock` fields (plan phase 2).
//!
//! Lock storage stays a lossless map ([`super::lock::Source`]) to preserve the
//! byte-compatible-writer invariant; these types validate on the way *out*, when
//! a command actually needs a well-formed revision or hash. Constructing one is
//! the single point where a malformed value is rejected.

use std::fmt;
use std::str::FromStr;

use crate::error::ClinixError;

/// A git revision: 40- or 64-char **lowercase** hexadecimal (SHA-1 / SHA-256).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rev(String);

impl Rev {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_lower_hex(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl FromStr for Rev {
    type Err = ClinixError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if matches!(s.len(), 40 | 64) && is_lower_hex(s) {
            Ok(Rev(s.to_string()))
        } else {
            Err(ClinixError::InvalidRev(s.to_string()))
        }
    }
}

impl fmt::Display for Rev {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A nix content hash: an SRI form (`sha256-<base64>`) or the legacy prefixed
/// base32 form (`sha256:<base32>`). Only the algorithm and separator are
/// validated; the digest body is left to nix to reject on use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarHash(String);

impl NarHash {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for NarHash {
    type Err = ClinixError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let split = s.split_once(|c| c == '-' || c == ':');
        match split {
            Some((algo, body))
                if !body.is_empty()
                    && matches!(algo, "sha256" | "sha512" | "sha1" | "md5") =>
            {
                Ok(NarHash(s.to_string()))
            }
            _ => Err(ClinixError::InvalidNarHash(s.to_string())),
        }
    }
}

impl fmt::Display for NarHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
