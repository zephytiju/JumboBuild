//! Pinned reproduction (Jumbo Build & Versioning Standard, §2.4):
//! `jumbo build --pinned <buildId>` / `jumbo reproduce <buildId>`.
//!
//! Reproducing a past build resolves exactly from its index record — the
//! record is the lock. Given a buildId:
//!
//! 1. resolve the record (by recorded or derived bootstrap buildId);
//! 2. recompute `sha256(commit + canonical extract)` from the recorded
//!    inputs and require equality with the recorded fingerprint — a
//!    mismatch aborts with a typed error before anything is consumed;
//! 3. materialize the recorded closure: every internal entry of the
//!    recorded canonical extract resolves to the dependency's record at
//!    the **exact recorded version** (never the newest of the major), and
//!    its artifact is pulled through the J4 fetch/ingest layer — exact
//!    URL, recorded SHA-256 enforced;
//! 4. materialize the own artifact the same way, producing the recorded
//!    digest in the reproduction output directory.
//!
//! Bootstrap records (null fingerprint or canonicalExtract) predate the
//! fingerprint engine and cannot be reproduced — a typed error says so.
//! Third-party registry entries are covered by the fingerprint check over
//! the extract; no public registry is contacted.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{effective_build_id, resolve_by_build_id, PinningError};
use crate::dedup::{stage_and_verify, verify_sha256, ArtifactProvider, MaterializedArtifact};
use crate::fingerprint::extract::{compute_fingerprint, CanonicalExtract, EntrySource};
use crate::resolver::index::{Index, IndexRecord};

/// Contract identifier of the reproduction report.
pub const REPRODUCE_CONTRACT: &str = "jumbo.pinned-reproduction/1";

/// The report of a successful pinned reproduction.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReproduceReport {
    /// Always [`REPRODUCE_CONTRACT`].
    pub contract: String,
    pub package: String,
    /// The buildId the reproduction resolved.
    pub build_id: String,
    pub version: String,
    pub commit: String,
    /// The fingerprint recorded in the index record.
    pub recorded_fingerprint: String,
    /// The fingerprint recomputed from the recorded inputs.
    pub recomputed_fingerprint: String,
    /// Always true on success — a mismatch aborts before this report.
    pub fingerprint_match: bool,
    /// Internal dependencies materialized from their exact-version records
    /// (sha256-verified through the J4 layer).
    pub materialized_dependencies: Vec<MaterializedArtifact>,
    /// Internal dependencies whose records published no artifact and whose
    /// `deps/<slug>` source coordinates therefore stand.
    pub source_overlay_dependencies: Vec<String>,
    /// The own artifact, pulled and sha256-verified — reproducing the
    /// recorded digest. Null when the record published none.
    pub artifact: Option<MaterializedArtifact>,
    /// Where the reproduction outputs landed.
    pub out_dir: String,
}

/// Options of a reproduction run.
#[derive(Debug, Clone)]
pub struct ReproduceOptions {
    /// Constrain the buildId search to one package (else the whole index).
    pub package: Option<String>,
    /// Where outputs land (created when absent).
    pub out_dir: PathBuf,
}

impl Default for ReproduceOptions {
    fn default() -> Self {
        Self {
            package: None,
            out_dir: PathBuf::from("reproduced"),
        }
    }
}

/// The dependency records a canonical extract's internal entries resolve
/// to: exact-version lookup against the index, never newest-of-major.
fn closure_records<'a>(
    index: &'a Index,
    package: &str,
    version: &str,
    extract: &CanonicalExtract,
) -> Result<Vec<(String, &'a IndexRecord)>, PinningError> {
    let mut resolved = Vec::new();
    for entry in &extract.entries {
        if entry.source != EntrySource::Index {
            continue;
        }
        let records =
            index
                .records(&entry.name)
                .ok_or_else(|| PinningError::ClosureIncomplete {
                    package: package.to_string(),
                    version: version.to_string(),
                    dep: entry.name.clone(),
                    available: "none recorded".to_string(),
                })?;
        // The exact recorded version — old records stay addressable forever.
        let found = records
            .entries
            .iter()
            .find(|(_, rec)| rec.version == entry.version);
        let Some((_, record)) = found else {
            let available: Vec<String> = records
                .entries
                .iter()
                .map(|(_, rec)| rec.version.clone())
                .collect();
            return Err(PinningError::ClosureIncomplete {
                package: package.to_string(),
                version: version.to_string(),
                dep: format!("{}@{}", entry.name, entry.version),
                available: available.join(", "),
            });
        };
        resolved.push((entry.name.clone(), record));
    }
    Ok(resolved)
}

/// Place a staged, verified artifact at `<out>/<sub>/<file>` and re-verify
/// the placed bytes.
fn place_verified(
    staged: &Path,
    out: &Path,
    sub: &str,
    file_name: &str,
    sha256: &str,
) -> Result<PathBuf, PinningError> {
    let dest_dir = out.join(sub);
    std::fs::create_dir_all(&dest_dir).map_err(|e| {
        PinningError::Materialize(crate::dedup::MaterializeError::Ingestion {
            package: String::new(),
            target: dest_dir.display().to_string(),
            reason: format!("failed to create: {e}"),
        })
    })?;
    let dest = dest_dir.join(file_name);
    std::fs::copy(staged, &dest).map_err(|e| {
        PinningError::Materialize(crate::dedup::MaterializeError::Ingestion {
            package: String::new(),
            target: dest.display().to_string(),
            reason: format!("failed to place the artifact: {e}"),
        })
    })?;
    verify_sha256(&dest, sha256)?;
    Ok(dest)
}

/// Reproduce the build a buildId pins. See the module docs for the steps;
/// nothing is written outside `options.out_dir` and the staging directory,
/// and the fingerprint check runs before any bytes are fetched.
pub fn reproduce(
    index: &Index,
    build_id: &str,
    options: &ReproduceOptions,
    provider: &ArtifactProvider,
    staging_dir: &Path,
) -> Result<ReproduceReport, PinningError> {
    // 1. Resolve the record by buildId.
    let (package, _line, record) =
        resolve_by_build_id(index, build_id, options.package.as_deref())?;
    let (effective_id, _source) = effective_build_id(record);

    // 2. Bootstrap records cannot be reproduced.
    let unavailable = |reason: &str| PinningError::ReproduceUnavailable {
        package: package.clone(),
        build_id: effective_id.clone(),
        reason: reason.to_string(),
    };
    let Some(recorded_fingerprint) = record.fingerprint.as_deref() else {
        return Err(unavailable(
            "the record has a null fingerprint (imported before the fingerprint engine)",
        ));
    };
    let Some(extract_value) = record.canonical_extract.as_ref() else {
        return Err(unavailable(
            "the record has a null canonicalExtract (imported before the fingerprint engine)",
        ));
    };

    // 3. Recompute the fingerprint from the recorded inputs.
    let extract: CanonicalExtract = serde_json::from_value(extract_value.clone()).map_err(|e| {
        PinningError::InvalidExtract {
            package: package.clone(),
            version: record.version.clone(),
            reason: e.to_string(),
        }
    })?;
    let recomputed = compute_fingerprint(&record.commit, &extract)?;
    if !recomputed.eq_ignore_ascii_case(recorded_fingerprint.trim()) {
        return Err(PinningError::FingerprintMismatch {
            package,
            version: record.version.clone(),
            build_id: effective_id,
            recorded: recorded_fingerprint.trim().to_string(),
            recomputed,
        });
    }

    // 4. Resolve the closure to exact-version records before any fetch.
    let closure = closure_records(index, &package, &record.version, &extract)?;

    std::fs::create_dir_all(&options.out_dir).map_err(|e| {
        PinningError::Materialize(crate::dedup::MaterializeError::Ingestion {
            package: package.clone(),
            target: options.out_dir.display().to_string(),
            reason: format!("failed to create the output directory: {e}"),
        })
    })?;

    // 5. Materialize the closure: stage + verify every artifact-bearing
    //    dependency, then place under deps/<slug>/ (the J4 convention).
    let mut materialized_dependencies = Vec::new();
    let mut source_overlay_dependencies = Vec::new();
    for (name, dep_record) in &closure {
        match stage_and_verify(dep_record, provider, staging_dir) {
            Ok((url, sha256, staged)) => {
                let slug = crate::resolver::index::package_slug(name);
                let relative = format!("deps/{slug}/{}", url.file_name);
                let placed = place_verified(
                    &staged,
                    &options.out_dir,
                    &format!("deps/{slug}"),
                    &url.file_name,
                    &sha256,
                )?;
                debug_assert!(placed.is_file());
                materialized_dependencies.push(MaterializedArtifact {
                    package: name.clone(),
                    version: dep_record.version.clone(),
                    commit: dep_record.commit.clone(),
                    build_id: dep_record.build_id.clone(),
                    url: url.url.clone(),
                    sha256,
                    path: relative,
                    source_overlay_path: Some(format!("deps/{slug}")),
                    rewritten: None,
                });
            }
            Err(crate::dedup::MaterializeError::NoArtifact { .. }) => {
                // Records that published no artifact keep their source
                // coordinates — the same rule `jumbo dedup --deps` applies.
                source_overlay_dependencies.push(name.to_string());
            }
            Err(e) => return Err(e.into()),
        }
    }

    // 6. The own artifact: pull, verify, place under dist/ — the
    //    reproduction produces the recorded digest.
    let mut artifact = None;
    if record.artifact_url.is_some() {
        let (url, sha256, staged) = stage_and_verify(record, provider, staging_dir)?;
        let placed = place_verified(&staged, &options.out_dir, "dist", &url.file_name, &sha256)?;
        debug_assert!(placed.is_file());
        artifact = Some(MaterializedArtifact {
            package: package.clone(),
            version: record.version.clone(),
            commit: record.commit.clone(),
            build_id: record.build_id.clone(),
            url: url.url.clone(),
            sha256,
            path: format!("dist/{}", url.file_name),
            source_overlay_path: None,
            rewritten: None,
        });
    }

    Ok(ReproduceReport {
        contract: REPRODUCE_CONTRACT.to_string(),
        package,
        build_id: effective_id,
        version: record.version.clone(),
        commit: record.commit.clone(),
        recorded_fingerprint: recorded_fingerprint.trim().to_string(),
        recomputed_fingerprint: recomputed,
        fingerprint_match: true,
        materialized_dependencies,
        source_overlay_dependencies,
        artifact,
        out_dir: options.out_dir.display().to_string(),
    })
}

/// The index record a buildId reproduces from (for callers that only need
/// the lookup, e.g. `jumbo build --pinned` validation).
pub fn pinned_record<'a>(
    index: &'a Index,
    build_id: &str,
    package: Option<&str>,
) -> Result<(String, usize, &'a IndexRecord), PinningError> {
    resolve_by_build_id(index, build_id, package)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::IndexSource;
    use std::path::PathBuf;

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(bytes);
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn base_record(package: &str, major: u64, version: &str, commit: &str) -> IndexRecord {
        IndexRecord {
            package: package.to_string(),
            major,
            version: version.to_string(),
            commit: commit.to_string(),
            fingerprint: None,
            canonical_extract: None,
            artifact_url: None,
            artifact_sha256: None,
            image_digest: None,
            build_id: None,
            pipeline_run: None,
            executor: Some("circleci".to_string()),
            timestamp: "2026-09-10T00:00:00Z".to_string(),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jumbo-reproduce-ut-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// Build a fixture index: consumer 2.4.0 depends on demo-alpha 2.4.0
    /// (index source) and numpy 1.26.4 (pypi). The wheel bytes, their
    /// digests, and the recorded fingerprint all agree — unless `tamper`
    /// replaces the recorded fingerprint. Returns (dir, artifact cache).
    fn fixture(tag: &str, tamper: bool) -> (PathBuf, PathBuf) {
        let dir = temp_dir(tag);
        let cache = dir.join("cache");
        std::fs::create_dir_all(&cache).expect("cache");

        let wheel_bytes = b"demo wheel bytes";
        let own_bytes = b"consumer wheel bytes";
        std::fs::write(cache.join("demo_alpha-2.4.0-py3-none-any.whl"), wheel_bytes).expect("whl");
        std::fs::write(cache.join("consumer-2.4.0-py3-none-any.whl"), own_bytes).expect("whl");

        let dep_commit = "0123456789abcdef0123456789abcdef01234567";
        let own_commit = "fedcba9876543210fedcba9876543210fedcba98";

        let extract = CanonicalExtract::new(vec![
            crate::fingerprint::extract::ExtractEntry {
                name: "demo-alpha".into(),
                version: "2.4.0".into(),
                source: EntrySource::Index,
                digest: None,
                path: Some("deps/demo-alpha".into()),
            },
            crate::fingerprint::extract::ExtractEntry {
                name: "numpy".into(),
                version: "1.26.4".into(),
                source: EntrySource::PyPI,
                digest: Some("sha256:aaaa1111aaaa".into()),
                path: None,
            },
        ]);
        let fingerprint = compute_fingerprint(own_commit, &extract).expect("fingerprint");
        let fingerprint = if tamper { "f".repeat(64) } else { fingerprint };

        let mut dep = base_record("demo-alpha", 2, "2.4.0", dep_commit);
        dep.build_id = Some("demo-2.4.0-001".into());
        dep.artifact_url = Some(
            "https://github.com/acme/demo-alpha/releases/download/v2.4.0/demo_alpha-2.4.0-py3-none-any.whl"
                .into(),
        );
        dep.artifact_sha256 = Some(sha256_hex(wheel_bytes));

        let mut own = base_record("consumer", 2, "2.4.0", own_commit);
        own.build_id = Some("consumer-2.4.0-001".into());
        own.fingerprint = Some(fingerprint);
        own.canonical_extract = Some(serde_json::to_value(&extract).unwrap());
        own.artifact_url = Some(
            "https://github.com/acme/consumer/releases/download/v2.4.0/consumer-2.4.0-py3-none-any.whl"
                .into(),
        );
        own.artifact_sha256 = Some(sha256_hex(own_bytes));

        let index_dir = dir.join("index");
        std::fs::create_dir_all(&index_dir).expect("index dir");
        std::fs::write(
            index_dir.join("demo-alpha.jsonl"),
            serde_json::to_string(&dep).unwrap() + "\n",
        )
        .expect("dep jsonl");
        std::fs::write(
            index_dir.join("consumer.jsonl"),
            serde_json::to_string(&own).unwrap() + "\n",
        )
        .expect("own jsonl");

        (dir, cache)
    }

    #[test]
    fn pinned_record_resolves_by_build_id_across_packages() {
        let (dir, _cache) = fixture("lookup", false);
        let index = Index::load(&IndexSource::Local(dir.join("index"))).expect("load");
        let (package, _line, record) =
            pinned_record(&index, "consumer-2.4.0-001", None).expect("resolve");
        assert_eq!(package, "consumer");
        assert_eq!(record.version, "2.4.0");

        // Constrained lookup errors cleanly on the wrong package.
        let err = pinned_record(&index, "consumer-2.4.0-001", Some("demo-alpha")).unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {err}");

        let err = pinned_record(&index, "missing-id", None).unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {err}");
    }

    #[test]
    fn reproduce_verifies_fingerprint_and_materializes_closure() {
        let (dir, cache) = fixture("ok", false);
        let index = Index::load(&IndexSource::Local(dir.join("index"))).expect("load");
        let out = dir.join("reproduced");
        let staging = dir.join("staging");
        let options = ReproduceOptions {
            package: None,
            out_dir: out.clone(),
        };
        let report = reproduce(
            &index,
            "consumer-2.4.0-001",
            &options,
            &ArtifactProvider::Cache(cache.clone()),
            &staging,
        )
        .expect("reproduce");

        assert_eq!(report.contract, "jumbo.pinned-reproduction/1");
        assert!(report.fingerprint_match);
        assert_eq!(report.recomputed_fingerprint, report.recorded_fingerprint);
        // The own artifact reproduced the recorded digest.
        let own = report.artifact.as_ref().expect("own artifact");
        assert_eq!(own.sha256, sha256_hex(b"consumer wheel bytes"));
        assert_eq!(own.path, "dist/consumer-2.4.0-py3-none-any.whl");
        assert!(out
            .join("dist")
            .join("consumer-2.4.0-py3-none-any.whl")
            .is_file());
        // The closure dependency materialized at the exact recorded version.
        assert_eq!(report.materialized_dependencies.len(), 1);
        let dep = &report.materialized_dependencies[0];
        assert_eq!(dep.package, "demo-alpha");
        assert_eq!(dep.version, "2.4.0");
        assert_eq!(
            dep.path,
            "deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl"
        );
        assert!(out
            .join("deps/demo-alpha")
            .join("demo_alpha-2.4.0-py3-none-any.whl")
            .is_file());
        assert!(report.source_overlay_dependencies.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reproduce_aborts_on_fingerprint_mismatch() {
        let (dir, cache) = fixture("mismatch", true);
        let index = Index::load(&IndexSource::Local(dir.join("index"))).expect("load");
        let out = dir.join("reproduced");
        let options = ReproduceOptions {
            package: None,
            out_dir: out.clone(),
        };
        let err = reproduce(
            &index,
            "consumer-2.4.0-001",
            &options,
            &ArtifactProvider::Cache(cache),
            &dir.join("staging"),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("PIN_FINGERPRINT_MISMATCH"), "got: {msg}");
        assert!(
            msg.contains("ffffffff"),
            "must show the recorded value: {msg}"
        );
        // Aborted before any output was written.
        assert!(!out.join("dist").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bootstrap_records_cannot_reproduce() {
        let dir = temp_dir("bootstrap");
        let index_dir = dir.join("index");
        std::fs::create_dir_all(&index_dir).expect("index dir");
        let rec = base_record(
            "legacy-pkg",
            1,
            "1.0.0",
            "0123456789abcdef0123456789abcdef01234567",
        );
        std::fs::write(
            index_dir.join("legacy-pkg.jsonl"),
            serde_json::to_string(&rec).unwrap() + "\n",
        )
        .expect("jsonl");
        let index = Index::load(&IndexSource::Local(index_dir)).expect("load");
        let derived = crate::pinning::effective_build_id(&rec).0;

        let err = reproduce(
            &index,
            &derived,
            &ReproduceOptions::default(),
            &ArtifactProvider::Cache(dir.clone()),
            &dir.join("staging"),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("cannot be reproduced"),
            "got: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closure_must_resolve_the_exact_recorded_version() {
        let dir = temp_dir("closure");
        let index_dir = dir.join("index");
        std::fs::create_dir_all(&index_dir).expect("index dir");
        let own_commit = "fedcba9876543210fedcba9876543210fedcba98";
        // The extract records demo-alpha 2.4.0, but the index only has 2.5.0.
        let extract = CanonicalExtract::new(vec![crate::fingerprint::extract::ExtractEntry {
            name: "demo-alpha".into(),
            version: "2.4.0".into(),
            source: EntrySource::Index,
            digest: None,
            path: Some("deps/demo-alpha".into()),
        }]);
        let fingerprint = compute_fingerprint(own_commit, &extract).expect("fingerprint");
        let mut dep = base_record(
            "demo-alpha",
            2,
            "2.5.0",
            "0123456789abcdef0123456789abcdef01234567",
        );
        dep.build_id = Some("demo-2.5.0-001".into());
        let mut own = base_record("consumer", 2, "2.4.0", own_commit);
        own.build_id = Some("consumer-2.4.0-001".into());
        own.fingerprint = Some(fingerprint);
        own.canonical_extract = Some(serde_json::to_value(&extract).unwrap());
        std::fs::write(
            index_dir.join("demo-alpha.jsonl"),
            serde_json::to_string(&dep).unwrap() + "\n",
        )
        .expect("dep jsonl");
        std::fs::write(
            index_dir.join("consumer.jsonl"),
            serde_json::to_string(&own).unwrap() + "\n",
        )
        .expect("own jsonl");
        let index = Index::load(&IndexSource::Local(index_dir)).expect("load");

        let err = reproduce(
            &index,
            "consumer-2.4.0-001",
            &ReproduceOptions::default(),
            &ArtifactProvider::Cache(dir.clone()),
            &dir.join("staging"),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("closure incomplete"), "got: {msg}");
        assert!(msg.contains("demo-alpha@2.4.0"), "got: {msg}");
        assert!(msg.contains("2.5.0"), "must list available versions: {msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
