//! Typed errors for the Jumbo fingerprint engine (lock generation,
//! canonical extract, fingerprint, promotion guard).

// The fingerprint error enum carries rich context strings for actionable
// messages and flows through `anyhow` at the CLI boundary, where its
// stack size is not performance-relevant.
#![allow(clippy::result_large_err)]

/// Errors produced by the fingerprint engine.
#[derive(Debug, thiserror::Error)]
pub enum FingerprintError {
    #[error("invalid lock file `{path}`: {reason}")]
    InvalidLock { path: String, reason: String },

    #[error(
        "forbidden reference for `{name}` in {origin}: a Git or direct-URL source.\n  \
         Jumbo rule: internal packages resolve from the Jumbo index and third-party packages \
         from their registries; a lock carrying a Git URL or direct artifact URL means a \
         declaration bypassed validation (Jumbo Build & Versioning Standard, Resolution Semantics)."
    )]
    ForbiddenLockReference { name: String, origin: String },

    #[error(
        "machine-specific path `{path}` in {origin}.\n  \
         Canonicalization requires stable relative paths; regenerate the lock with jumbo so \
         injected sources live under deps/ (Jumbo Build & Versioning Standard, Duplicate Detection)."
    )]
    MachineSpecificPath { path: String, origin: String },

    #[error("invalid own commit `{value}`: expected a full 40-hex-digit Git commit SHA")]
    InvalidCommit { value: String },

    #[error(
        "no Git repository found above `{start}`.\n  \
         The fingerprint is sha256(own commit + canonical extract); a project outside a \
         Git repository has no own commit."
    )]
    NotARepository { start: String },

    #[error(
        "promotion refused: the working tree of {origin} is not clean at commit {commit}.\n  \
         Offending paths (first {shown} of {total}):\n{paths}\n  \
         Jumbo rule: promotion happens only on clean commits inside a pipeline; local builds on \
         dirty working trees never promote. jumbo-generated output under deps/ and the generated \
         lock files are exempt; commit or stash everything else and re-run with --promote \
         (Jumbo Build & Versioning Standard, Duplicate Detection)."
    )]
    DirtyTree {
        origin: String,
        commit: String,
        paths: String,
        shown: usize,
        total: usize,
    },

    #[error("invalid jumbo injection marker `{path}`: {reason}")]
    InvalidMarker { path: String, reason: String },

    #[error("cannot generate the lock for `{manifest}`: {reason}")]
    LockGeneration { manifest: String, reason: String },
}

/// Render a list of dirty paths for the promotion error.
pub fn format_dirty_paths(paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| format!("    - {p}"))
        .collect::<Vec<_>>()
        .join("\n")
}
