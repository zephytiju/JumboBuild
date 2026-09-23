//! Jumbo version arithmetic: `major.minor.patch` parsing and the bump
//! operations of the auto-promotion engine (Jumbo Build & Versioning
//! Standard, §2.1).
//!
//! The developer owns the major (a manual manifest bump on a breaking
//! change, resetting minor and patch to 0 — the first build of a major is
//! `M.0.0`); the pipeline owns minor (own-source change) and patch
//! (dependency-closure change only). Arithmetic therefore never crosses a
//! major: a minor bump of `2.9.9` is `2.10.0`, not `3.0.0`; the next major
//! exists only when the manifest declares it and the index has no record
//! of it (bootstrap).

use std::fmt;
use std::str::FromStr;

use serde::Serialize;

use super::error::PromotionError;

/// A parsed `major.minor.patch` version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct JumboVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl JumboVersion {
    /// The first build of a major: `M.0.0`.
    pub fn bootstrap(major: u64) -> Self {
        Self {
            major,
            minor: 0,
            patch: 0,
        }
    }

    /// The next minor: minor + 1, patch reset (own-source change).
    pub fn next_minor(self) -> Self {
        Self {
            major: self.major,
            minor: self.minor + 1,
            patch: 0,
        }
    }

    /// The next patch: patch + 1 (dependency-closure change only).
    pub fn next_patch(self) -> Self {
        Self {
            major: self.major,
            minor: self.minor,
            patch: self.patch + 1,
        }
    }
}

impl fmt::Display for JumboVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for JumboVersion {
    type Err = PromotionError;

    /// Parse exactly `major.minor.patch` (three non-negative integers).
    /// Anything else — `2`, `2.4`, `v2.4.0`, `2.4.x`, pre-release tags — is
    /// rejected: the jumbo version space has no other shapes.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || PromotionError::InvalidVersion {
            value: s.to_string(),
            reason: "expected exactly `major.minor.patch` (three non-negative integers)".into(),
        };
        let mut parts = s.trim().split('.');
        let (Some(major), Some(minor), Some(patch), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid());
        };
        let digits = |v: &str| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit());
        if !digits(major) || !digits(minor) || !digits(patch) {
            return Err(invalid());
        }
        Ok(Self {
            major: major.parse().map_err(|_| invalid())?,
            minor: minor.parse().map_err(|_| invalid())?,
            patch: patch.parse().map_err(|_| invalid())?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_display_round_trip() {
        for text in ["0.0.0", "2.4.0", "1.0.10", "12.34.56"] {
            let v: JumboVersion = text.parse().expect("parse");
            assert_eq!(v.to_string(), text);
        }
    }

    #[test]
    fn malformed_versions_are_rejected() {
        for bad in [
            "",
            " ",
            "2",
            "2.4",
            "2.4.0.1",
            "v2.4.0",
            "2.4.x",
            "-1.0.0",
            "2.-4.0",
            "2.4.-0",
            "a.b.c",
            "2.4.0-rc1",
            "2 .4.0",
        ] {
            let err = JumboVersion::from_str(bad).unwrap_err();
            assert!(
                err.to_string().contains("invalid version"),
                "`{bad}` should be rejected, got: {err}"
            );
        }
        // Leading zeros are accepted (u64 semantics: 02 == 2).
        assert_eq!(
            "02.4.0".parse::<JumboVersion>().unwrap().to_string(),
            "2.4.0"
        );
    }

    #[test]
    fn minor_arithmetic_never_crosses_a_major() {
        // The boundary case: 2.9.9 + minor = 2.10.0, NOT 3.0.0.
        let v: JumboVersion = "2.9.9".parse().unwrap();
        assert_eq!(v.next_minor().to_string(), "2.10.0");
        let v: JumboVersion = "9.999.999".parse().unwrap();
        assert_eq!(v.next_minor().to_string(), "9.1000.0");
        let v: JumboVersion = "0.0.0".parse().unwrap();
        assert_eq!(v.next_minor().to_string(), "0.1.0");
        // Minor resets patch.
        let v: JumboVersion = "2.4.7".parse().unwrap();
        assert_eq!(v.next_minor().to_string(), "2.5.0");
    }

    #[test]
    fn patch_arithmetic_carries_within_the_minor() {
        let v: JumboVersion = "2.4.9".parse().unwrap();
        assert_eq!(v.next_patch().to_string(), "2.4.10");
        let v: JumboVersion = "1.0.99".parse().unwrap();
        assert_eq!(v.next_patch().to_string(), "1.0.100");
        let v: JumboVersion = "0.0.0".parse().unwrap();
        assert_eq!(v.next_patch().to_string(), "0.0.1");
        // Patch never touches minor or major.
        let v: JumboVersion = "3.19.255".parse().unwrap();
        assert_eq!(v.next_patch().to_string(), "3.19.256");
    }

    #[test]
    fn bootstrap_resets_minor_and_patch() {
        assert_eq!(JumboVersion::bootstrap(3).to_string(), "3.0.0");
        assert_eq!(JumboVersion::bootstrap(0).to_string(), "0.0.0");
        // A bootstrap of the same major after any history starts at M.0.0
        // regardless of what other majors recorded.
        assert_ne!(
            JumboVersion::bootstrap(2),
            "2.4.0".parse::<JumboVersion>().unwrap().next_minor()
        );
    }

    #[test]
    fn ordering_is_numeric_not_lexicographic() {
        let a: JumboVersion = "2.10.0".parse().unwrap();
        let b: JumboVersion = "2.9.9".parse().unwrap();
        assert!(b < a, "2.9.9 must order below 2.10.0");
    }
}
