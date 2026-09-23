//! Errors of the auto-promotion version bump engine.

use thiserror::Error;

/// Something prevented computing a promotion decision.
#[derive(Debug, Error)]
pub enum PromotionError {
    /// A version string does not parse as `major.minor.patch`.
    #[error("invalid version `{value}`: {reason}")]
    InvalidVersion { value: String, reason: String },

    /// The manifest's own version is missing or not `major.minor.patch` —
    /// the developer owns the major in the manifest, so it must parse.
    #[error("manifest `{path}` does not declare a usable own version: {reason}")]
    ManifestVersion { path: String, reason: String },

    /// An index record's version does not parse as `major.minor.patch`.
    #[error("invalid version `{value}` in index record for `{package}` (major {major}): {reason}")]
    RecordVersion {
        package: String,
        major: u64,
        value: String,
        reason: String,
    },

    /// An index record's version disagrees with its own `major` field.
    #[error(
        "index record for `{package}` declares major {major} but version `{value}` has major \
         {version_major}; the index is inconsistent"
    )]
    RecordMajorMismatch {
        package: String,
        major: u64,
        value: String,
        version_major: u64,
    },

    /// The refresh policy knob could not be parsed.
    #[error(transparent)]
    Refresh(#[from] super::refresh::RefreshError),

    /// The duplicate-detection query underlying the decision failed.
    #[error(transparent)]
    Dedup(#[from] crate::dedup::MaterializeError),
}
