//! Build-or-reuse decision (Jumbo Build & Versioning Standard, §2.3).
//!
//! Given the computed input fingerprint of the project about to build,
//! query the index history for the same package name: if any record
//! carries an identical fingerprint, this build is a duplicate — reuse
//! that record's artifact instead of building from source. The decision
//! is a pure query over the in-memory index; downloading and ingesting
//! the artifact is the materializer's job ([`crate::dedup::ingest`]).
//!
//! Null fingerprints never match: bootstrap records were imported without
//! a computed fingerprint and can never prove input equality, so they are
//! skipped rather than trusted. When several records share the fingerprint
//! (a standards violation — the same inputs must produce one artifact),
//! the earliest record in append order is reported so the decision is
//! deterministic and idempotent.

use serde::Serialize;

use super::error::MaterializeError;
use crate::resolver::index::{Index, IndexRecord};

/// What the pipeline should do with the project about to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DedupAction {
    /// A record with an identical fingerprint exists: pull its artifact.
    Reuse,
    /// No record matches: build from source.
    Build,
}

/// The index record a duplicate decision matched.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchedRecord {
    /// The `.jsonl` index file the record was read from.
    pub index_file: String,
    /// 1-based line of the record in the package's `.jsonl` file.
    pub record_line: usize,
    /// The matched record itself.
    pub record: IndexRecord,
}

/// The build-or-reuse decision for one package fingerprint.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DedupDecision {
    /// Package name the history was searched for.
    pub package: String,
    /// The computed input fingerprint that was queried.
    pub fingerprint: String,
    /// Whether any record of the package carries the identical fingerprint.
    pub duplicate: bool,
    /// `reuse` when [`Self::duplicate`], otherwise `build`.
    pub action: DedupAction,
    /// The matched record (earliest in append order), when a duplicate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_record: Option<MatchedRecord>,
    /// How many records of the package were searched (including the null
    /// fingerprints of bootstrap records).
    pub records_searched: usize,
}

/// Decide build-or-reuse: search every record of `package` for an
/// identical fingerprint.
///
/// The query fingerprint must be a valid 64-hex sha256 (as produced by the
/// fingerprint engine); a malformed value is a caller bug and errors out
/// instead of silently reporting a miss. Comparison is ASCII
/// case-insensitive so records written with uppercase hex still match.
pub fn decide(
    package: &str,
    fingerprint: &str,
    index: &Index,
) -> Result<DedupDecision, MaterializeError> {
    let query = fingerprint.trim().to_ascii_lowercase();
    let valid = query.len() == 64
        && query
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !valid {
        return Err(MaterializeError::InvalidFingerprint {
            value: fingerprint.to_string(),
        });
    }

    let records = index.records(package);
    let entries = records.map(|r| r.entries.as_slice()).unwrap_or(&[]);
    let mut matched: Option<(usize, &IndexRecord)> = None;
    for (line, record) in entries {
        // Null fingerprints (bootstrap records) can never prove input
        // equality — they are skipped, never treated as hits.
        let Some(recorded) = record.fingerprint.as_deref() else {
            continue;
        };
        if recorded.trim().eq_ignore_ascii_case(&query) {
            matched = Some((*line, record));
            // Earliest append-order record with this fingerprint: the
            // canonical artifact for these inputs.
            break;
        }
    }

    let duplicate = matched.is_some();
    Ok(DedupDecision {
        package: package.to_string(),
        fingerprint: query,
        duplicate,
        action: if duplicate {
            DedupAction::Reuse
        } else {
            DedupAction::Build
        },
        matched_record: matched.map(|(record_line, record)| MatchedRecord {
            index_file: records.map(|r| r.file.clone()).unwrap_or_default(),
            record_line,
            record: record.clone(),
        }),
        records_searched: entries.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::index::IndexSource;
    use std::path::PathBuf;

    fn record(package: &str, fingerprint: Option<&str>) -> IndexRecord {
        IndexRecord {
            package: package.to_string(),
            major: 2,
            version: "2.4.0".to_string(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
            fingerprint: fingerprint.map(str::to_string),
            canonical_extract: None,
            artifact_url: Some(
                "https://github.com/acme/pkg/releases/download/v2.4.0/pkg-2.4.0-py3-none-any.whl"
                    .into(),
            ),
            artifact_sha256: Some("a".repeat(64)),
            image_digest: None,
            build_id: Some("acme-pkg-2.4.0-001".into()),
            pipeline_run: None,
            executor: Some("circleci".into()),
            timestamp: "2026-09-01T00:00:00Z".to_string(),
        }
    }

    fn fixture_index(records: &[IndexRecord]) -> (PathBuf, Index) {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "jumbo-dedup-ut-{seq}-{}-{}",
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

    const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn identical_fingerprint_in_any_record_position_is_a_hit() {
        let (dir, index) = fixture_index(&[
            record("demo-alpha", None),
            record("demo-alpha", Some(&"f".repeat(64))),
            record("demo-alpha", Some(FP)),
        ]);
        let decision = decide("demo-alpha", FP, &index).expect("decide");
        assert!(decision.duplicate);
        assert_eq!(decision.action, DedupAction::Reuse);
        assert_eq!(decision.records_searched, 3);
        let matched = decision.matched_record.expect("matched");
        assert_eq!(matched.record_line, 3);
        assert_eq!(matched.record.version, "2.4.0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn earliest_record_wins_when_fingerprint_repeats() {
        let (dir, index) = fixture_index(&[
            record("demo-alpha", Some(FP)),
            record("demo-alpha", Some(FP)),
        ]);
        let decision = decide("demo-alpha", FP, &index).expect("decide");
        assert_eq!(decision.matched_record.expect("matched").record_line, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn null_fingerprints_never_match() {
        let (dir, index) = fixture_index(&[record("demo-alpha", None), record("demo-alpha", None)]);
        let decision = decide("demo-alpha", FP, &index).expect("decide");
        assert!(!decision.duplicate);
        assert_eq!(decision.action, DedupAction::Build);
        assert!(decision.matched_record.is_none());
        assert_eq!(decision.records_searched, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_package_is_a_miss_not_an_error() {
        let (dir, index) = fixture_index(&[record("demo-alpha", Some(FP))]);
        let decision = decide("other-package", FP, &index).expect("decide");
        assert!(!decision.duplicate);
        assert_eq!(decision.action, DedupAction::Build);
        assert_eq!(decision.records_searched, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn case_insensitive_hex_and_whitespace_trimmed() {
        let upper = FP.to_ascii_uppercase();
        let (dir, index) = fixture_index(&[record("demo-alpha", Some(FP))]);
        let decision = decide("demo-alpha", &format!("  {upper}  "), &index).expect("decide");
        assert!(decision.duplicate);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_query_fingerprint_is_an_error() {
        let (dir, index) = fixture_index(&[record("demo-alpha", Some(FP))]);
        for bad in ["", "abc", &"g".repeat(64), &"a".repeat(63)] {
            let err = decide("demo-alpha", bad, &index).unwrap_err();
            assert!(
                err.to_string().contains("invalid fingerprint"),
                "got: {err}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
