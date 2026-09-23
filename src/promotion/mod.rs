//! Jumbo auto-promotion version bump engine (Jumbo Build & Versioning
//! Standard, §2.1 Version Semantics, §2.3 Duplicate Detection, §3.5
//! Executors and the Publication Contract).
//!
//! Given a package, its manifest (the declared major — the developer owns
//! the major, bumping it manually on a breaking change), the computed
//! input fingerprint, and the index history for that package, `decide`
//! emits the promotion decision the executor consumes:
//!
//! - **duplicate fingerprint** (a J4 dedup hit: any record of the package
//!   carries the identical fingerprint) → `bump: none`,
//!   `publishRequired: false` — the inputs were already built, reuse the
//!   recorded artifact;
//! - **first build of a declared major** (no index record of the major) →
//!   `bump: bootstrap`, `M.0.0`;
//! - **own-source change** (the own commit differs from the newest
//!   record's commit of that major) → `bump: minor`;
//! - **same commit, changed canonical extract** → `bump: patch` (a
//!   third-party in-range update is an extract change, so it promotes a
//!   patch exactly like any other dependency-closure change); a bootstrap
//!   record without a canonical extract can never prove input equality, so
//!   a same-commit build against it promotes a patch as well;
//! - **same commit, identical extract** → `bump: none` (formatting-only
//!   lock changes canonicalize away; normally this is already the
//!   duplicate-fingerprint case — the equality fallback also covers
//!   records whose fingerprint field is null but whose extract is
//!   recorded).
//!
//! The decision derives **only** from the own commit, the canonical
//! extract, and the index history — never from wall clocks, artifacts, or
//! executor state. Minor/patch arithmetic never crosses a major: the next
//! major exists only when the manifest declares it and the index has no
//! record of it.
//!
//! Publish-on-bump contract: `publishRequired` is true exactly when a new
//! version was computed (bootstrap/minor/patch). Executors (CircleCI jobs
//! and the `jumbo-publish` GitHub Actions workflow) read that single field
//! and, when publishing, append an index record from the `publish` block —
//! `version`, `commit`, `fingerprint`, `canonicalExtract` — filling
//! `artifactUrl`, `artifactSha256`, `imageDigest`, `buildId`,
//! `pipelineRun`, `executor`, and `timestamp` after publishing. Jumbo
//! itself never publishes artifacts and never writes the index.
//!
//! Boundary: the decision (and the third-party refresh policy knob,
//! [`refresh`]) only — no artifact publication, no index writes, no
//! fingerprint changes.

pub mod error;
pub mod refresh;
pub mod version;

use serde::Serialize;
use std::str::FromStr;

pub use error::PromotionError;
pub use refresh::{evaluate, resolve_policy, RefreshGate, RefreshPolicy, REFRESH_ENV};
pub use version::JumboVersion;

use crate::dedup::{self, MatchedRecord};
use crate::fingerprint::extract::CanonicalExtract;
use crate::resolver::index::Index;

/// Which bump the promotion engine computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Bump {
    /// No new version: the inputs were already built (duplicate) or are
    /// unchanged. Nothing publishes; the executor reuses the record.
    None,
    /// Own-source change: minor + 1, patch reset.
    Minor,
    /// Dependency-closure change only (same commit): patch + 1.
    Patch,
    /// First build of the manifest-declared major: `M.0.0`.
    Bootstrap,
}

impl Bump {
    /// Whether a new version was computed — exactly the publish-on-bump
    /// condition executors act on.
    pub fn publishes(self) -> bool {
        !matches!(self, Bump::None)
    }
}

/// The inputs a promotion decision derives from. Nothing else may
/// influence the bump.
#[derive(Debug)]
pub struct PromotionInputs<'a> {
    /// Package name whose index history decides (language-native form).
    pub package: &'a str,
    /// `python` or `npm` (informational; echoed in the decision).
    pub ecosystem: &'a str,
    /// The major the package manifest declares (developer-owned).
    pub declared_major: u64,
    /// The full 40-hex own commit of the build about to promote.
    pub commit: &'a str,
    /// The canonical extract of the generated lock.
    pub extract: &'a CanonicalExtract,
    /// `sha256(own commit + canonical extract)`.
    pub fingerprint: &'a str,
}

/// The executor-facing record skeleton: everything needed to append an
/// index record after publishing (the executor fills artifact URL and
/// digest, image digest, buildId, pipeline run, executor, timestamp).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishContract {
    /// The version to publish (`nextVersion`).
    pub version: String,
    /// The major the record belongs to.
    pub major: u64,
    /// The full 40-hex own commit.
    pub commit: String,
    /// The input fingerprint.
    pub fingerprint: String,
    /// The canonical extract — exactly what the index record stores.
    pub canonical_extract: CanonicalExtract,
}

/// The promotion decision for one package build.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromotionDecision {
    /// Package name the decision was computed for.
    pub package: String,
    /// `python` or `npm`.
    pub ecosystem: String,
    /// The major the package manifest declares.
    pub declared_major: u64,
    /// Version of the newest index record of the declared major; null
    /// when the major has no records (a bootstrap).
    pub current_version: Option<String>,
    /// The version this build promotes. On `bump: none` this is the
    /// version to reuse: the matched duplicate record's version on a
    /// dedup hit, otherwise the newest record's version.
    pub next_version: String,
    /// Which bump was computed.
    pub bump: Bump,
    /// Human-readable, self-contained explanation.
    pub reason: String,
    /// Whether the own commit differs from the newest record's commit of
    /// the major (proven own-source change).
    pub source_change: bool,
    /// Whether the canonical extract differs from the newest record's
    /// extract (proven dependency-closure change).
    pub dependency_change: bool,
    /// Publish-on-bump: true exactly when `bump` is bootstrap/minor/patch.
    /// Executors read this single field.
    pub publish_required: bool,
    /// Whether the computed fingerprint hit the index (J4 dedup).
    pub duplicate: bool,
    /// The matched record when `duplicate` (the reuse target).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_record: Option<MatchedRecord>,
    /// The evaluated third-party refresh policy for this run.
    pub refresh: RefreshGate,
    /// Whether this command ran the language lock tool itself (false when
    /// it consumed an existing lock — the pipeline's `jumbo lock` step ran
    /// the gate instead).
    pub lock_tool_ran: bool,
    /// Executor-facing record skeleton for the index append after
    /// publishing.
    pub publish: PublishContract,
}

/// Compute the promotion decision (see the module docs for the rules).
pub fn decide(
    inputs: &PromotionInputs<'_>,
    index: &Index,
    refresh: RefreshGate,
    lock_tool_ran: bool,
) -> Result<PromotionDecision, PromotionError> {
    // `currentVersion` is defined by the newest record of the declared
    // major (null when the major has no records).
    let newest = index.newest_of_major(inputs.package, inputs.declared_major);
    let current_version = newest.map(|(_, record)| record.version.clone());

    // 1. Duplicate detection first (J4): an identical fingerprint anywhere
    //    in the package's history means the inputs were already built.
    let dedup_decision = dedup::decide(inputs.package, inputs.fingerprint, index)?;
    if dedup_decision.duplicate {
        let matched = dedup_decision
            .matched_record
            .clone()
            .expect("a duplicate has a matched record");
        return Ok(assemble(
            inputs,
            refresh,
            lock_tool_ran,
            current_version,
            Bump::None,
            matched.record.version.clone(),
            format!(
                "duplicate fingerprint: identical inputs (commit {}, canonical extract) are \
                 already recorded as version {} ({}:{}); reuse its artifact, no publish",
                short(inputs.commit),
                matched.record.version,
                matched.index_file,
                matched.record_line
            ),
            false,
            false,
            Some(matched),
        ));
    }

    // 2. No record of the declared major: the first build of the major
    //    bootstraps at M.0.0 (the developer's manifest bump).
    let Some((_line, newest)) = newest else {
        let next = JumboVersion::bootstrap(inputs.declared_major);
        return Ok(assemble(
            inputs,
            refresh,
            lock_tool_ran,
            None,
            Bump::Bootstrap,
            next.to_string(),
            format!(
                "first build of major {}: no index records of the major exist; the \
                 manifest-declared major bootstraps at {}; promoting {}",
                inputs.declared_major, next, next
            ),
            true,
            false,
            None,
        ));
    };

    let current = JumboVersion::from_str(&newest.version).map_err(|e| {
        let reason = match e {
            PromotionError::InvalidVersion { reason, .. } => reason,
            other => other.to_string(),
        };
        PromotionError::RecordVersion {
            package: inputs.package.to_string(),
            major: inputs.declared_major,
            value: newest.version.clone(),
            reason,
        }
    })?;
    if current.major != newest.major {
        return Err(PromotionError::RecordMajorMismatch {
            package: inputs.package.to_string(),
            major: newest.major,
            value: newest.version.clone(),
            version_major: current.major,
        });
    }

    // 3. Classify the change from the own commit and the canonical extract
    //    alone. An own-source change wins even when the closure moved with
    //    it; only a same-commit extract change is a patch.
    let source_change = !commits_equal(inputs.commit, &newest.commit);
    let record_extract = newest.canonical_extract.as_ref();
    let own_value = serde_json::to_value(inputs.extract).expect("extract serializes");
    let extract_equal = record_extract
        .map(|recorded| recorded == &own_value)
        .unwrap_or(false);

    let (bump, next, reason) = if source_change {
        let next = current.next_minor();
        (
            Bump::Minor,
            next,
            format!(
                "own-source change: commit {} differs from the newest record of major {} ({}, \
                 commit {}); promoting {}",
                short(inputs.commit),
                inputs.declared_major,
                current,
                short(&newest.commit),
                next
            ),
        )
    } else if extract_equal {
        (
            Bump::None,
            current,
            format!(
                "no input change: same commit {} and identical canonical extract as the newest \
                 record of major {} ({}); no publish",
                short(inputs.commit),
                inputs.declared_major,
                current
            ),
        )
    } else {
        let next = current.next_patch();
        let cause = if record_extract.is_none() {
            format!(
                "the newest record of major {} ({}) carries no canonical extract (a bootstrap \
                 record), so input equality cannot be proven",
                inputs.declared_major, current
            )
        } else {
            format!(
                "canonical extract differs from the newest record of major {} ({})",
                inputs.declared_major, current
            )
        };
        (
            Bump::Patch,
            next,
            format!(
                "dependency-closure change only: same commit {}, {}; promoting {}",
                short(inputs.commit),
                cause,
                next
            ),
        )
    };
    // A patch attributes the promotion to the dependency closure (a null
    // record extract is an unprovable, not an absent, closure change).
    let dependency_change =
        matches!(bump, Bump::Patch) || (record_extract.is_some() && !extract_equal);

    Ok(assemble(
        inputs,
        refresh,
        lock_tool_ran,
        current_version,
        bump,
        next.to_string(),
        reason,
        source_change,
        dependency_change,
        None,
    ))
}

/// Assemble the final decision struct from the classified outcome.
#[allow(clippy::too_many_arguments)]
fn assemble(
    inputs: &PromotionInputs<'_>,
    refresh: RefreshGate,
    lock_tool_ran: bool,
    current_version: Option<String>,
    bump: Bump,
    next_version: String,
    reason: String,
    source_change: bool,
    dependency_change: bool,
    matched: Option<MatchedRecord>,
) -> PromotionDecision {
    PromotionDecision {
        package: inputs.package.to_string(),
        ecosystem: inputs.ecosystem.to_string(),
        declared_major: inputs.declared_major,
        current_version,
        publish_required: bump.publishes(),
        duplicate: matched.is_some(),
        matched_record: matched,
        refresh,
        lock_tool_ran,
        publish: PublishContract {
            version: next_version.clone(),
            major: inputs.declared_major,
            commit: inputs.commit.to_string(),
            fingerprint: inputs.fingerprint.trim().to_ascii_lowercase(),
            canonical_extract: inputs.extract.clone(),
        },
        next_version,
        bump,
        reason,
        source_change,
        dependency_change,
    }
}

/// Compare two full commit SHAs (whitespace-trimmed, case-insensitive).
fn commits_equal(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// The first 8 hex characters of a commit, for readable reasons.
fn short(commit: &str) -> &str {
    &commit[..commit.len().min(8)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::extract::{compute_fingerprint, EntrySource, ExtractEntry};
    use crate::resolver::index::{IndexRecord, IndexSource};
    use std::path::PathBuf;

    const COMMIT_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const COMMIT_B: &str = "fedcba9876543210fedcba9876543210fedcba98";

    fn extract_of(entries: &[(&str, &str, EntrySource)]) -> CanonicalExtract {
        CanonicalExtract::new(
            entries
                .iter()
                .map(|(name, version, source)| ExtractEntry {
                    name: name.to_string(),
                    version: version.to_string(),
                    source: *source,
                    digest: None,
                    path: None,
                })
                .collect(),
        )
    }

    fn record(
        package: &str,
        major: u64,
        version: &str,
        commit: &str,
        fingerprint: Option<&str>,
        canonical_extract: Option<serde_json::Value>,
    ) -> IndexRecord {
        IndexRecord {
            package: package.to_string(),
            major,
            version: version.to_string(),
            commit: commit.to_string(),
            fingerprint: fingerprint.map(str::to_string),
            canonical_extract,
            artifact_url: None,
            artifact_sha256: None,
            image_digest: None,
            build_id: None,
            pipeline_run: None,
            executor: Some("bootstrap".into()),
            timestamp: "2026-09-01T00:00:00Z".to_string(),
        }
    }

    fn fixture_index(records: &[IndexRecord]) -> (PathBuf, Index) {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "jumbo-promote-ut-{seq}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("index")).expect("create index dir");
        let lines: Vec<String> = records
            .iter()
            .map(|r| serde_json::to_string(r).unwrap())
            .collect();
        std::fs::write(
            dir.join("index").join("demo-alpha.jsonl"),
            lines.join("\n") + "\n",
        )
        .expect("write jsonl");
        let index = Index::load(&IndexSource::Local(dir.join("index"))).expect("load index");
        (dir, index)
    }

    fn gate() -> RefreshGate {
        evaluate(&RefreshPolicy::Run, None, 0)
    }

    fn inputs<'a>(
        commit: &'a str,
        extract: &'a CanonicalExtract,
        fingerprint: &'a str,
    ) -> PromotionInputs<'a> {
        PromotionInputs {
            package: "demo-alpha",
            ecosystem: "python",
            declared_major: 2,
            commit,
            extract,
            fingerprint,
        }
    }

    #[test]
    fn bootstrap_when_major_has_no_records() {
        let extract = extract_of(&[]);
        let fingerprint = compute_fingerprint(COMMIT_A, &extract).unwrap();
        // Records exist — but only for major 1.
        let (dir, index) = fixture_index(&[record("demo-alpha", 1, "1.9.0", COMMIT_A, None, None)]);
        let mut i = inputs(COMMIT_A, &extract, &fingerprint);
        i.declared_major = 3;
        let decision = decide(&i, &index, gate(), false).expect("decide");
        assert_eq!(decision.bump, Bump::Bootstrap);
        assert_eq!(decision.current_version, None);
        assert_eq!(decision.next_version, "3.0.0");
        assert!(decision.publish_required);
        assert!(decision.source_change);
        assert!(!decision.dependency_change);
        assert!(!decision.duplicate);
        assert_eq!(decision.publish.version, "3.0.0");
        assert_eq!(decision.publish.major, 3);
        assert_eq!(decision.publish.commit, COMMIT_A);
        assert_eq!(decision.publish.fingerprint, fingerprint);
        assert_eq!(
            serde_json::to_value(&decision.publish.canonical_extract).unwrap(),
            serde_json::to_value(&extract).unwrap()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn minor_on_own_source_change_even_with_dependency_change() {
        // The newest record of major 2 was built from COMMIT_A + numpy 1.26.4.
        let old_extract = extract_of(&[("numpy", "1.26.4", EntrySource::PyPI)]);
        // This build: a different commit AND a different extract.
        let new_extract = extract_of(&[("numpy", "1.27.0", EntrySource::PyPI)]);
        let fingerprint = compute_fingerprint(COMMIT_B, &new_extract).unwrap();
        let old_fingerprint = compute_fingerprint(COMMIT_A, &old_extract).unwrap();
        let (dir, index) = fixture_index(&[record(
            "demo-alpha",
            2,
            "2.9.9",
            COMMIT_A,
            Some(&old_fingerprint),
            Some(serde_json::to_value(&old_extract).unwrap()),
        )]);
        let decision = decide(
            &inputs(COMMIT_B, &new_extract, &fingerprint),
            &index,
            gate(),
            false,
        )
        .expect("decide");
        assert_eq!(decision.bump, Bump::Minor);
        // Boundary arithmetic: 2.9.9 + minor = 2.10.0, never 3.0.0.
        assert_eq!(decision.next_version, "2.10.0");
        assert_eq!(decision.current_version.as_deref(), Some("2.9.9"));
        assert!(decision.publish_required);
        assert!(decision.source_change);
        assert!(decision.dependency_change, "the extract also moved");
        assert!(decision.reason.contains("own-source change"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn patch_on_third_party_in_range_update_same_commit() {
        // Same commit; the recorded closure pinned numpy 1.26.4, the
        // current resolution pulled 1.27.0 within its declared range.
        let old_extract = extract_of(&[("numpy", "1.26.4", EntrySource::PyPI)]);
        let new_extract = extract_of(&[("numpy", "1.27.0", EntrySource::PyPI)]);
        let fingerprint = compute_fingerprint(COMMIT_A, &new_extract).unwrap();
        let old_fingerprint = compute_fingerprint(COMMIT_A, &old_extract).unwrap();
        let (dir, index) = fixture_index(&[record(
            "demo-alpha",
            2,
            "2.4.9",
            COMMIT_A,
            Some(&old_fingerprint),
            Some(serde_json::to_value(&old_extract).unwrap()),
        )]);
        let decision = decide(
            &inputs(COMMIT_A, &new_extract, &fingerprint),
            &index,
            gate(),
            false,
        )
        .expect("decide");
        assert_eq!(decision.bump, Bump::Patch);
        assert_eq!(decision.next_version, "2.4.10");
        assert!(decision.publish_required);
        assert!(!decision.source_change);
        assert!(decision.dependency_change);
        assert!(decision.reason.contains("dependency-closure"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn patch_when_bootstrap_record_cannot_prove_equality() {
        // Same commit, but the newest record (a bootstrap import) carries
        // no canonical extract — equality is unprovable, so a patch
        // promotes rather than silently reusing unverifiable inputs.
        let extract = extract_of(&[("numpy", "1.26.4", EntrySource::PyPI)]);
        let fingerprint = compute_fingerprint(COMMIT_A, &extract).unwrap();
        let (dir, index) = fixture_index(&[record("demo-alpha", 2, "2.4.0", COMMIT_A, None, None)]);
        let decision = decide(
            &inputs(COMMIT_A, &extract, &fingerprint),
            &index,
            gate(),
            false,
        )
        .expect("decide");
        assert_eq!(decision.bump, Bump::Patch);
        assert_eq!(decision.next_version, "2.4.1");
        assert!(!decision.source_change);
        assert!(decision.dependency_change);
        assert!(decision.reason.contains("cannot be proven"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn none_on_duplicate_even_when_newest_record_moved_on() {
        // The exact inputs were already recorded (line 1); a later record
        // of the major moved on (line 2). The duplicate wins: no bump, no
        // publish, reuse the matched record's artifact.
        let extract = extract_of(&[("numpy", "1.26.4", EntrySource::PyPI)]);
        let fingerprint = compute_fingerprint(COMMIT_A, &extract).unwrap();
        let newer_extract = extract_of(&[("numpy", "1.27.0", EntrySource::PyPI)]);
        let (dir, index) = fixture_index(&[
            record(
                "demo-alpha",
                2,
                "2.4.0",
                COMMIT_A,
                Some(&fingerprint),
                Some(serde_json::to_value(&extract).unwrap()),
            ),
            record(
                "demo-alpha",
                2,
                "2.5.0",
                COMMIT_B,
                Some(&"e".repeat(64)),
                Some(serde_json::to_value(&newer_extract).unwrap()),
            ),
        ]);
        let decision = decide(
            &inputs(COMMIT_A, &extract, &fingerprint),
            &index,
            gate(),
            false,
        )
        .expect("decide");
        assert_eq!(decision.bump, Bump::None);
        assert!(decision.duplicate);
        assert!(!decision.publish_required);
        assert!(!decision.source_change);
        assert!(!decision.dependency_change);
        // nextVersion is the version to reuse; currentVersion follows the
        // newest record of the major by definition.
        assert_eq!(decision.next_version, "2.4.0");
        assert_eq!(decision.current_version.as_deref(), Some("2.5.0"));
        assert_eq!(decision.matched_record.as_ref().unwrap().record_line, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn none_on_formatting_only_extract_equality_without_a_recorded_fingerprint() {
        // Same commit, identical canonical extract — but the record's
        // fingerprint field is null (never a dedup hit). Equality of the
        // extract alone proves no input change: no bump. This is the path
        // a formatting-only lock change takes when the record predates
        // fingerprinting.
        let extract = extract_of(&[("numpy", "1.26.4", EntrySource::PyPI)]);
        let fingerprint = compute_fingerprint(COMMIT_A, &extract).unwrap();
        let (dir, index) = fixture_index(&[record(
            "demo-alpha",
            2,
            "2.4.0",
            COMMIT_A,
            None,
            Some(serde_json::to_value(&extract).unwrap()),
        )]);
        let decision = decide(
            &inputs(COMMIT_A, &extract, &fingerprint),
            &index,
            gate(),
            false,
        )
        .expect("decide");
        assert_eq!(decision.bump, Bump::None);
        assert!(!decision.publish_required);
        assert!(!decision.duplicate, "null fingerprints never match");
        assert_eq!(decision.next_version, "2.4.0");
        assert!(decision.reason.contains("identical canonical extract"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unparseable_and_mismatched_record_versions_are_errors() {
        let extract = extract_of(&[]);
        let fingerprint = compute_fingerprint(COMMIT_B, &extract).unwrap();
        let (dir, index) = fixture_index(&[record("demo-alpha", 2, "2.4", COMMIT_A, None, None)]);
        let err = decide(
            &inputs(COMMIT_B, &extract, &fingerprint),
            &index,
            gate(),
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid version"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);

        let (dir, index) = fixture_index(&[record("demo-alpha", 2, "3.0.0", COMMIT_A, None, None)]);
        let err = decide(
            &inputs(COMMIT_B, &extract, &fingerprint),
            &index,
            gate(),
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("inconsistent"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn uppercase_fingerprint_and_commit_compare_case_insensitively() {
        let extract = extract_of(&[]);
        let fingerprint = compute_fingerprint(COMMIT_A, &extract).unwrap();
        let (dir, index) = fixture_index(&[record(
            "demo-alpha",
            2,
            "2.4.0",
            &COMMIT_A.to_ascii_uppercase(),
            Some(&fingerprint.to_ascii_uppercase()),
            None,
        )]);
        let decision = decide(
            &inputs(COMMIT_A, &extract, &fingerprint),
            &index,
            gate(),
            false,
        )
        .expect("decide");
        assert_eq!(decision.bump, Bump::None);
        assert!(decision.duplicate);
        // The publish contract always lowercases the fingerprint.
        assert_eq!(decision.publish.fingerprint, fingerprint);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_gate_and_lock_tool_flag_are_recorded_verbatim() {
        let extract = extract_of(&[]);
        let fingerprint = compute_fingerprint(COMMIT_B, &extract).unwrap();
        let (dir, index) = fixture_index(&[record("demo-alpha", 2, "2.4.0", COMMIT_A, None, None)]);
        let policy = RefreshPolicy::parse("schedule:7d").unwrap();
        let refresh = evaluate(&policy, Some("2026-09-01T00:00:00Z"), 1_788_220_800);
        let decision = decide(
            &inputs(COMMIT_B, &extract, &fingerprint),
            &index,
            refresh,
            true,
        )
        .expect("decide");
        assert_eq!(decision.refresh.policy, "schedule:7d");
        assert!(!decision.refresh.upgrade);
        assert!(decision.lock_tool_ran);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_shape_matches_the_executor_contract() {
        let extract = extract_of(&[("numpy", "1.26.4", EntrySource::PyPI)]);
        let fingerprint = compute_fingerprint(COMMIT_B, &extract).unwrap();
        let (dir, index) = fixture_index(&[record("demo-alpha", 2, "2.4.0", COMMIT_A, None, None)]);
        let decision = decide(
            &inputs(COMMIT_B, &extract, &fingerprint),
            &index,
            gate(),
            false,
        )
        .expect("decide");
        let json = serde_json::to_value(&decision).expect("serialize");
        for key in [
            "package",
            "ecosystem",
            "declaredMajor",
            "currentVersion",
            "nextVersion",
            "bump",
            "reason",
            "sourceChange",
            "dependencyChange",
            "publishRequired",
            "duplicate",
            "refresh",
            "lockToolRan",
            "publish",
        ] {
            assert!(json.get(key).is_some(), "missing key `{key}` in {json}");
        }
        for key in [
            "version",
            "major",
            "commit",
            "fingerprint",
            "canonicalExtract",
        ] {
            assert!(
                json["publish"].get(key).is_some(),
                "missing publish key `{key}`"
            );
        }
        assert_eq!(json["bump"], "minor");
        assert_eq!(json["publishRequired"], true);
        assert_eq!(
            json["publish"]["canonicalExtract"]["format"],
            "jumbo-canonical-extract/1"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
