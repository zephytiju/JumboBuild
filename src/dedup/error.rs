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
        "artifact download failed for {url}: HTTP {status}: the recorded artifact no longer \
         exists at this URL.\n  \
         Only the dependency ingestion path may fall back to the source overlay on this error \
         (a release asset that is definitively gone); everywhere else it aborts like any other \
         download failure."
    )]
    ArtifactGone { url: String, status: u16 },

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

    #[error(
        "cannot resolve the repository of {package} {version}: {reason}\n  \
         The source fallback fetches the dependency's real repository tree at the recorded \
         commit, so it needs a repository coordinate. Either (a) the record's artifactUrl is \
         an https github.com URL — the owner/repo is parsed from it, which works even when \
         the asset itself is gone — or (b) pass a repo map mapping the package name to its \
         https github.com clone URL (--repo-map <PATH> or the JUMBO_REPO_MAP environment \
         variable, a JSON object of package name to clone URL)."
    )]
    UnresolvableRepository {
        package: String,
        version: String,
        reason: String,
    },

    #[error(
        "invalid repo map `{path}`: {reason}\n  \
         A repo map is a JSON object mapping package names to https github.com clone URLs."
    )]
    InvalidRepoMap { path: String, reason: String },

    #[error(
        "fetching the real source of {package} failed for {url}: {reason}\n  \
         The minimal lock stub is never buildable, so a source fallback that cannot fetch \
         the recorded tree aborts instead of leaving a broken materialization behind."
    )]
    SourceTarball {
        package: String,
        url: String,
        reason: String,
    },

    #[error(
        "the fetched source at {path} does not match the record for {package}: {reason}\n  \
         Jumbo rule: the repository tree fetched at the recorded commit must carry the \
         recorded package's manifest name (the version may differ — the record's version \
         semantics hold); a mismatch means the coordinate or the repository is wrong."
    )]
    SourceNameMismatch {
        package: String,
        path: String,
        reason: String,
    },
}
