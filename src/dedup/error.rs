//! Typed errors for the Jumbo dedup decision and artifact materializer
//! (Jumbo Build & Versioning Standard, §2.3–§2.4 and §3.4).

// The materializer error enum carries rich context strings for actionable
// messages and flows through `anyhow` at the CLI boundary, where its
// stack size is not performance-relevant.
#![allow(clippy::result_large_err)]

/// Errors produced by the dedup decision and the materializer.
#[derive(Debug, thiserror::Error)]
pub enum MaterializeError {
    #[error(
        "invalid fingerprint `{value}`: expected 64 lowercase hex digits.\n  \
         The dedup decision compares sha256(own commit + canonical extract) values; \
         a malformed query fingerprint is a caller bug, never a miss."
    )]
    InvalidFingerprint { value: String },

    #[error(
        "recorded artifact URL is not allowed: {url}\n  \
         Reason: {reason}\n  \
         Jumbo rule: artifacts are pulled only from github.com and GitHub release-asset \
         hosts over https; localhost, loopback, private, reserved, and IP-literal hosts \
         are always rejected (Jumbo Build & Versioning Standard, Artifact Storage and \
         Materialization)."
    )]
    UnsupportedArtifactUrl { url: String, reason: String },

    #[error("artifact download failed for {url}: {reason}")]
    ArtifactDownload { url: String, reason: String },

    #[error(
        "unverifiable artifact for {package} {version} ({url}): the record has no \
         artifactSha256.\n  \
         Jumbo rule: never proceed on unverifiable bytes — a digest-less artifact is \
         aborted, not trusted (Jumbo Build & Versioning Standard, Artifact Storage and \
         Materialization)."
    )]
    MissingSha256 {
        package: String,
        version: String,
        url: String,
    },

    #[error(
        "unverifiable artifact for {package} ({url}): the recorded artifactSha256 is not a \
         valid 64-hex digest: {expected}"
    )]
    InvalidSha256 {
        package: String,
        url: String,
        expected: String,
    },

    #[error(
        "artifact digest mismatch for {url}.\n  \
         Recorded artifactSha256: {expected}\n  \
         Actual sha256 of downloaded bytes: {actual}\n  \
         Jumbo rule: a digest mismatch aborts the build; never proceed on bytes that do \
         not match the index record bit-for-bit (Jumbo Build & Versioning Standard, \
         Security and Reliability)."
    )]
    DigestMismatch {
        url: String,
        expected: String,
        actual: String,
    },

    #[error(
        "matched record for {package} {version} has no artifactUrl; there is nothing to \
         pull.\n  \
         The reuse decision stands, but materialization requires a record that published \
         a release asset."
    )]
    NoArtifact { package: String, version: String },

    #[error(
        "unsupported artifact kind for {package}: {url}\n  \
         Python packages materialize as wheels (*.whl); Node packages as npm tarballs \
         (*.tgz). Any other asset cannot be ingested into the standard build."
    )]
    UnsupportedArtifactKind { package: String, url: String },

    #[error("cannot materialize artifact for {package} into {target}: {reason}")]
    Ingestion {
        package: String,
        target: String,
        reason: String,
    },

    #[error("invalid artifact materialization marker `{path}`: {reason}")]
    InvalidMarker { path: String, reason: String },
}
