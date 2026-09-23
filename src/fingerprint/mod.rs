//! Jumbo fingerprint engine: lock generation, canonical extract,
//! fingerprint computation, and the clean-commit promotion guard
//! (Jumbo Build & Versioning Standard, §2.3–§2.4).
//!
//! The fingerprint of a package build is
//! `sha256(own commit + canonical extract of the generated language lock)`.
//! Jumbo generates the lock (`uv.lock`, `package-lock.json`) by resolving
//! the manifest — internal dependencies injected from index records at
//! stable relative paths (`deps/<slug>`), third-party dependencies from
//! their declared ranges — so the lock is the final resolved input set.
//! Canonicalize before hashing; never hash raw lock bytes. The index
//! record stores the canonical extract alongside the fingerprint, so a
//! past build reproduces exactly from its record.
//!
//! Boundary: no dedup decision against index history, no artifact
//! download, no version bumping — those belong to later tasks.

pub mod error;
pub mod extract;
pub mod gitguard;
pub mod lockgen;

use std::path::Path;

use serde::Serialize;

pub use error::FingerprintError;
use extract::{compute_fingerprint, extract_lock, CanonicalExtract};
use gitguard::own_commit;
pub use gitguard::{ensure_clean_for_promotion, promotion_tree_state};
pub use lockgen::{generate_lock_inputs, LockGeneration};

/// The full fingerprint report printed by `jumbo fingerprint`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintReport {
    /// The manifest the lock was generated from (omitted when reading an
    /// existing lock directly).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    /// `python` or `npm`.
    pub ecosystem: &'static str,
    /// Full 40-hex own commit SHA.
    pub commit: String,
    /// The lock file the extract came from.
    pub lock: String,
    /// Whether this computation ran in promotion mode (clean-tree guard
    /// enforced). Pure-local queries never promote.
    pub promotion: bool,
    /// Working-tree state at computation time (informational unless
    /// `promotion` is true, where non-clean is a hard refusal).
    pub tree_clean: bool,
    /// Repo-root-relative paths making the tree dirty (empty when clean).
    pub dirty_paths: Vec<String>,
    /// The canonical extract — exactly what the index record stores.
    pub canonical_extract: CanonicalExtract,
    /// `sha256(own commit + canonical extract)`.
    pub fingerprint: String,
}

/// Compute the fingerprint from an existing lock file (pure-local query).
///
/// The ecosystem follows the lock file name; the own commit is the HEAD
/// of the repository containing the lock file. Tree state is reported
/// informationally; it is never enforced unless `promotion` is set, in
/// which case a non-clean tree is a hard refusal before anything is
/// computed.
pub fn fingerprint_lock_file(
    lock_path: &Path,
    promotion: bool,
) -> Result<FingerprintReport, FingerprintError> {
    let own = if promotion {
        ensure_clean_for_promotion(lock_path)?
    } else {
        own_commit(lock_path)?
    };
    let state = promotion_tree_state(lock_path)?;
    let file_name = lock_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let content =
        std::fs::read_to_string(lock_path).map_err(|e| FingerprintError::InvalidLock {
            path: lock_path.display().to_string(),
            reason: format!("failed to read: {e}"),
        })?;
    let extract = extract_lock(&content, file_name)?;
    let fingerprint = compute_fingerprint(&own.commit, &extract)?;
    Ok(FingerprintReport {
        manifest: None,
        ecosystem: ecosystem_of_lock(file_name),
        commit: own.commit,
        lock: lock_path.display().to_string(),
        promotion,
        tree_clean: state.clean,
        dirty_paths: state.offending,
        canonical_extract: extract,
        fingerprint,
    })
}

/// Generate the lock for a manifest, run the language lock tool, then
/// fingerprint the produced lock.
///
/// Promotion mode enforces the clean-tree guard before generation: the
/// fingerprint must be attributable to exactly one commit before any
/// pipeline consumes it for a publish decision. With `run_tool` false the
/// caller generates the lock separately (`jumbo lock --inject-only`).
pub fn fingerprint_manifest(
    manifest: &Path,
    index: &crate::resolver::Index,
    promotion: bool,
    run_tool: bool,
) -> Result<(LockGeneration, FingerprintReport), FingerprintError> {
    // Promotion mode refuses dirty trees before anything is generated.
    if promotion {
        ensure_clean_for_promotion(manifest)?;
    }
    let generation = generate_lock_inputs(manifest, index)?;
    if run_tool {
        let (command, description) = generation.lock_command();
        let working_dir = manifest
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        crate::utils::runner::run_steps(&[(command, description)], &working_dir).map_err(|e| {
            FingerprintError::LockGeneration {
                manifest: manifest.display().to_string(),
                reason: format!("language lock tool failed: {e}"),
            }
        })?;
    }
    let state = promotion_tree_state(manifest)?;
    if !generation.lock_path.exists() {
        return Err(FingerprintError::InvalidLock {
            path: generation.lock_path.display().to_string(),
            reason: "the language lock tool did not produce the lock file; ensure uv/npm is \
                     installed or run `jumbo lock` first"
                .into(),
        });
    }
    let mut report = fingerprint_lock_file(&generation.lock_path, false)?;
    report.manifest = Some(manifest.display().to_string());
    report.ecosystem = generation.ecosystem.as_str();
    report.promotion = promotion;
    report.tree_clean = state.clean;
    report.dirty_paths = state.offending;
    Ok((generation, report))
}
fn ecosystem_of_lock(file_name: &str) -> &'static str {
    match file_name {
        "uv.lock" => "python",
        _ => "npm",
    }
}
