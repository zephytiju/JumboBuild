//! The pin manifest: how an index record becomes a deployment pin
//! (Jumbo Build & Versioning Standard, §3.6 Deployment Pinning Contract).
//!
//! Every promoted index record **is** the build record a deployment pins.
//! This module resolves one record of one package — by buildId, by commit,
//! or as the latest of a major — and emits a `jumbo.deployment-pin/v1`
//! manifest: the exact fields a downstream Pulumi program needs to flow
//! into the existing vangu Selection and PackageLock path (buildId in the
//! selection contract, exact digest-pinned images) without changing its
//! semantics.
//!
//! Field mapping (authoritative in `docs/pinning.md`):
//! - `buildId` — the record's `buildId`; bootstrap records with a null
//!   `buildId` get a deterministic derived id (see [`effective_build_id`]);
//! - `imageRef` — `<image-name>@<imageDigest>` only when the record
//!   published an image digest; the image name comes from `--image-name`
//!   or the GHCR convention derived from the artifact URL's repository;
//! - `artifact` — the record's exact `artifactUrl` + `artifactSha256`;
//! - `fingerprint`, `commit`, `version` — verbatim from the record;
//! - `recordRef` — where in the index the record lives, for audit.
//!
//! Boundary: read-only. Pinning never writes the index, never contacts a
//! registry, and never deploys anything.

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::resolver::index::{Index, IndexRecord};

mod error;
pub mod reproduce;

pub use error::PinningError;
pub use reproduce::{reproduce, ReproduceOptions, ReproduceReport, REPRODUCE_CONTRACT};

/// Contract identifier of the pin manifest.
pub const PIN_CONTRACT: &str = "jumbo.deployment-pin/v1";
/// The exact-image validation the vangu Selection enforces. This string is
/// byte-identical to the regex in the IaC source
/// (`LatticeDeployment/src/selection.ts`, `exactImage`); the adapter in
/// `pinning-adapter/` carries the same literal, and both sides test parity.
pub const EXACT_IMAGE_RE: &str = r"^[^\s@]+@sha256:[a-f0-9]{64}$";

/// How one record was selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PinSelector {
    /// The record whose (recorded or derived) buildId matches.
    ByBuildId(String),
    /// The newest record promoted from this commit.
    ByCommit(String),
    /// The newest record of the major — what dependency resolution uses.
    LatestOfMajor(u64),
}

impl PinSelector {
    /// Human-readable selector description for the manifest.
    pub fn describe(&self) -> String {
        match self {
            Self::ByBuildId(id) => format!("buildId {id}"),
            Self::ByCommit(commit) => format!("commit {commit}"),
            Self::LatestOfMajor(major) => format!("latest of major {major}"),
        }
    }
}

/// Where the manifest's buildId came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BuildIdSource {
    /// The record's own `buildId` field.
    Record,
    /// Deterministically derived for a bootstrap record (null `buildId`).
    Derived,
}

/// The resolved artifact coordinates of a pin.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PinArtifact {
    /// Exact HTTPS URL of the language artifact (github.com-only egress).
    pub url: String,
    /// Recorded SHA-256 (64 hex); null in records that published none.
    pub sha256: Option<String>,
}

/// Where in the index the pinned record lives.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordRef {
    /// The `.jsonl` index file the record was read from.
    pub index_file: String,
    /// 1-based line of the record in the package's `.jsonl` file.
    pub record_line: usize,
}

/// The deployment pin manifest (`jumbo.deployment-pin/v1`): everything a
/// downstream Pulumi program needs to pin this exact build.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PinManifest {
    /// Always [`PIN_CONTRACT`].
    pub contract: String,
    /// Language-native package name of the record.
    pub package: String,
    /// Major segment of `version` (developer-owned).
    pub major: u64,
    /// Full promoted version.
    pub version: String,
    /// Pinning and reproduction key — the record's buildId, or the
    /// documented deterministic derivation for bootstrap records.
    pub build_id: String,
    /// `record` or `derived` — whether `build_id` came from the record or
    /// the bootstrap derivation.
    pub build_id_source: BuildIdSource,
    /// Full 40-hex own commit SHA of the pinned build.
    pub commit: String,
    /// `<image-name>@sha256:<64hex>` when the record published an image
    /// digest; null otherwise (an artifact-only build).
    pub image_ref: Option<String>,
    /// The raw image digest from the record (`sha256:<64hex>`), when
    /// published — the PackageLock `runtimeImageSourceDigest` mapping.
    pub image_digest: Option<String>,
    /// The artifact coordinates, when the record published an artifact.
    pub artifact: Option<PinArtifact>,
    /// `sha256(own commit + canonical extract)`; null in bootstrap records.
    pub fingerprint: Option<String>,
    /// Where the pinned record lives in the index.
    pub record_ref: RecordRef,
    /// Which selector resolved this record.
    pub selector: String,
    /// Record append timestamp (RFC 3339). Record order — not wall clock —
    /// is authoritative for recency.
    pub timestamp: String,
}

/// The effective buildId of a record: the recorded value when present,
/// otherwise the documented deterministic derivation.
///
/// Derivation for bootstrap records (null `buildId`):
/// `bootstrap-<first 12 hex of sha256(package \n version \n commit \n fingerprint-or-empty)>`.
/// The identity tuple (package, version, commit, fingerprint) is unique per
/// promoted build — auto-promotion never reuses a version — so the derived
/// id is stable across machines and runs, and distinct for every record.
pub fn effective_build_id(record: &IndexRecord) -> (String, BuildIdSource) {
    if let Some(id) = record
        .build_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return (id.to_string(), BuildIdSource::Record);
    }
    let mut hasher = Sha256::new();
    hasher.update(record.package.as_bytes());
    hasher.update(b"\n");
    hasher.update(record.version.as_bytes());
    hasher.update(b"\n");
    hasher.update(record.commit.as_bytes());
    hasher.update(b"\n");
    hasher.update(
        record
            .fingerprint
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .as_bytes(),
    );
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    (format!("bootstrap-{}", &hex[..12]), BuildIdSource::Derived)
}

/// Whether an image reference satisfies the exact-image validation the
/// vangu Selection enforces — semantics of [`EXACT_IMAGE_RE`], which the
/// downstream IaC applies to every `PackageLock` image.
///
/// Implemented by hand (no regex dependency): the name part is one or more
/// characters that are neither whitespace nor `@`, followed by exactly one
/// `@`, then `sha256:` and exactly 64 lowercase hex characters, then end.
pub fn matches_exact_image(image: &str) -> bool {
    let Some((name, digest)) = image.split_once('@') else {
        return false;
    };
    // After the separator only `sha256:<64hex>` is acceptable, and it can
    // never contain another `@`.
    if image.matches('@').count() != 1 {
        return false;
    }
    if name.is_empty() || name.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Validate an image reference with the IaC's exact-image rule.
pub fn exact_image(image: &str) -> Result<(), PinningError> {
    if matches_exact_image(image) {
        Ok(())
    } else {
        Err(PinningError::InvalidImageRef {
            value: image.to_string(),
        })
    }
}

/// Derive the default image name for a record's service image: the GHCR
/// convention `ghcr.io/<owner>/<repo>` taken from the record's artifact
/// URL repository (the artifact and the image are published from the same
/// package repository). Lowercase, as registry references require. Returns
/// `None` when the record carries no artifact URL to derive from.
pub fn derived_image_name(record: &IndexRecord) -> Option<String> {
    let url = record.artifact_url.as_deref()?;
    let rest = url.strip_prefix("https://")?;
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let path = &rest[authority_end..];
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    let owner = segments.next()?;
    let repo = segments.next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!(
        "ghcr.io/{}/{}",
        owner.to_ascii_lowercase(),
        repo.to_ascii_lowercase()
    ))
}

/// Validate a record's `imageDigest` (`sha256:<64 lowercase hex>`).
fn validate_image_digest(record: &IndexRecord) -> Result<&str, PinningError> {
    let Some(digest) = record.image_digest.as_deref() else {
        return Ok("");
    };
    let valid = digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    });
    if valid {
        Ok(digest)
    } else {
        Err(PinningError::InvalidImageDigest {
            package: record.package.clone(),
            version: record.version.clone(),
            value: digest.to_string(),
        })
    }
}

/// Resolve one record of `package` by the selector.
///
/// Selection rules (documented in `docs/pinning.md`):
/// - by buildId — the earliest record in append order whose recorded or
///   derived buildId matches (mirrors the dedup decision's earliest-match
///   determinism; buildIds are unique per promoted build by contract);
/// - by commit — the newest record promoted from the commit (a dependency
///   refresh on an unchanged commit appends patch records; the newest is
///   the one a deployment of that commit should pin);
/// - latest of major — the newest record of the major, exactly what
///   dependency resolution resolves.
pub fn resolve_record<'a>(
    index: &'a Index,
    package: &str,
    selector: &PinSelector,
) -> Result<(usize, &'a IndexRecord), PinningError> {
    let Some(records) = index.records(package) else {
        return Err(PinningError::PackageNotIndexed {
            package: package.to_string(),
        });
    };
    let entries = &records.entries;
    if entries.is_empty() {
        return Err(PinningError::PackageNotIndexed {
            package: package.to_string(),
        });
    }
    let found = match selector {
        PinSelector::ByBuildId(build_id) => {
            let needle = build_id.trim();
            entries
                .iter()
                .find(|(_, rec)| effective_build_id(rec).0 == needle)
        }
        PinSelector::ByCommit(commit) => {
            let needle = commit.trim().to_ascii_lowercase();
            let valid = needle.len() == 40
                && needle
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if !valid {
                return Err(PinningError::InvalidCommit {
                    value: commit.to_string(),
                });
            }
            entries
                .iter()
                .rfind(|(_, rec)| rec.commit.trim().eq_ignore_ascii_case(&needle))
        }
        PinSelector::LatestOfMajor(major) => entries.iter().rfind(|(_, rec)| rec.major == *major),
    };
    match found {
        Some((line, record)) => Ok((*line, record)),
        None => Err(match selector {
            PinSelector::ByBuildId(build_id) => PinningError::BuildIdNotFound {
                package: package.to_string(),
                build_id: build_id.clone(),
            },
            PinSelector::ByCommit(commit) => PinningError::CommitNotFound {
                package: package.to_string(),
                commit: commit.clone(),
            },
            PinSelector::LatestOfMajor(major) => PinningError::MajorNotRecorded {
                package: package.to_string(),
                major: *major,
                available: format_majors(index, package),
            },
        }),
    }
}

/// Comma-separated recorded majors ("1, 2"), or "none recorded".
fn format_majors(index: &Index, package: &str) -> String {
    let majors = index.majors(package);
    if majors.is_empty() {
        "none recorded".to_string()
    } else {
        majors
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Options controlling manifest construction.
#[derive(Debug, Clone, Default)]
pub struct PinOptions {
    /// Explicit image name for `imageRef` (overrides the GHCR derivation).
    pub image_name: Option<String>,
    /// Fail when the record published no image digest.
    pub require_image: bool,
}

/// Build the pin manifest for `package` under `selector`.
pub fn pin(
    index: &Index,
    package: &str,
    selector: &PinSelector,
    options: &PinOptions,
) -> Result<PinManifest, PinningError> {
    let (line, record) = resolve_record(index, package, selector)?;
    let index_file = index
        .records(package)
        .map(|records| records.file.clone())
        .unwrap_or_default();
    let (build_id, build_id_source) = effective_build_id(record);

    // imageRef: only when the record published a digest. The name is
    // deployment-owned (`--image-name`) or the GHCR convention derived from
    // the artifact URL's repository; with no derivable name the ref stays
    // null (documented) and --require-image names the gap.
    let image_digest = validate_image_digest(record)?;
    let mut image_ref = None;
    if !image_digest.is_empty() {
        let name = options
            .image_name
            .clone()
            .or_else(|| derived_image_name(record));
        if let Some(name) = name {
            let reference = format!("{name}@{image_digest}");
            exact_image(&reference)?;
            image_ref = Some(reference);
        }
    }
    if options.require_image && image_ref.is_none() {
        if image_digest.is_empty() {
            return Err(PinningError::ImageRequired {
                package: record.package.clone(),
                version: record.version.clone(),
                build_id,
            });
        }
        return Err(PinningError::InvalidImageRef {
            value: format!("<no image name>@{image_digest}"),
        });
    }

    Ok(PinManifest {
        contract: PIN_CONTRACT.to_string(),
        package: record.package.clone(),
        major: record.major,
        version: record.version.clone(),
        build_id,
        build_id_source,
        commit: record.commit.clone(),
        image_ref,
        image_digest: record.image_digest.clone(),
        artifact: record.artifact_url.as_deref().map(|url| PinArtifact {
            url: url.to_string(),
            sha256: record.artifact_sha256.clone(),
        }),
        fingerprint: record.fingerprint.clone(),
        record_ref: RecordRef {
            index_file,
            record_line: line,
        },
        selector: selector.describe(),
        timestamp: record.timestamp.clone(),
    })
}

/// Resolve the record a buildId points at, searching every package of the
/// index (in file-sorted package order, then record order; the first match
/// wins, mirroring the deterministic earliest-match rule). Used by pinned
/// reproduction, where only the buildId is known.
pub fn resolve_by_build_id<'a>(
    index: &'a Index,
    build_id: &str,
    package: Option<&str>,
) -> Result<(String, usize, &'a IndexRecord), PinningError> {
    let needle = build_id.trim();
    if let Some(package) = package {
        let (line, record) =
            resolve_record(index, package, &PinSelector::ByBuildId(needle.into()))?;
        return Ok((record.package.clone(), line, record));
    }
    for records in index.iter() {
        if let Some((line, record)) = records
            .entries
            .iter()
            .find(|(_, rec)| effective_build_id(rec).0 == needle)
        {
            return Ok((record.package.clone(), *line, record));
        }
    }
    Err(PinningError::BuildIdNotFound {
        package: package.unwrap_or("<any>").to_string(),
        build_id: build_id.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::index::IndexSource;
    use std::path::PathBuf;

    fn record(package: &str, major: u64, version: &str) -> IndexRecord {
        IndexRecord {
            package: package.to_string(),
            major,
            version: version.to_string(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
            fingerprint: None,
            canonical_extract: None,
            artifact_url: None,
            artifact_sha256: None,
            image_digest: None,
            build_id: None,
            pipeline_run: None,
            executor: Some("bootstrap".to_string()),
            timestamp: "2026-09-01T00:00:00Z".to_string(),
        }
    }

    fn temp_index(tag: &str, files: &[(&str, Vec<String>)]) -> (PathBuf, Index) {
        let dir = std::env::temp_dir().join(format!(
            "jumbo-pin-ut-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let index_dir = dir.join("index");
        std::fs::create_dir_all(&index_dir).expect("create index dir");
        for (slug, lines) in files {
            std::fs::write(
                index_dir.join(format!("{slug}.jsonl")),
                lines.join("\n") + "\n",
            )
            .expect("write jsonl");
        }
        let index = Index::load(&IndexSource::Local(index_dir.clone())).expect("load index");
        (dir, index)
    }

    #[test]
    fn exact_image_matches_the_iac_regex_semantics() {
        let digest = "sha256:".to_string() + &"a".repeat(64);
        for good in [
            format!("ghcr.io/acme/pkg@{digest}"),
            format!("localhost:5000/pkg@{digest}"),
            format!("pkg@{digest}"),
        ] {
            assert!(matches_exact_image(&good), "should match: {good}");
            assert!(exact_image(&good).is_ok());
        }
        for bad in [
            format!("ghcr.io/acme/pkg@{digest} "), // trailing space
            format!(" ghcr.io/acme/pkg@{digest}"), // leading space
            format!("ghcr.io/acme/pkg@sha256:{}", "A".repeat(64)), // uppercase
            format!("ghcr.io/acme/pkg@sha256:{}", "a".repeat(63)), // short
            format!("ghcr.io/acme/pkg@sha256:{}", "a".repeat(65)), // long
            format!("ghcr.io/acme/pkg@{}", "a".repeat(64)), // no sha256:
            format!("ghcr.io/acme@@{digest}"),     // @ in name
            format!("@{digest}"),                  // empty name
            String::from("ghcr.io/acme/pkg"),      // no digest
            String::new(),
        ] {
            assert!(!matches_exact_image(&bad), "should reject: `{bad}`");
            assert!(exact_image(&bad).is_err(), "should reject: `{bad}`");
        }
    }

    #[test]
    fn derived_build_id_is_deterministic_and_prefixed() {
        let mut a = record("demo-alpha", 2, "2.4.0");
        a.build_id = None;
        let (id1, source1) = effective_build_id(&a);
        let (id2, source2) = effective_build_id(&a);
        assert_eq!(id1, id2, "derivation must be deterministic");
        assert_eq!(source1, BuildIdSource::Derived);
        assert_eq!(source2, BuildIdSource::Derived);
        assert!(id1.starts_with("bootstrap-"), "got {id1}");
        assert_eq!(id1.len(), "bootstrap-".len() + 12);

        // Distinct identity tuples derive distinct ids; the recorded value
        // wins whenever present.
        let mut b = record("demo-alpha", 2, "2.5.0");
        b.build_id = None;
        assert_ne!(effective_build_id(&a).0, effective_build_id(&b).0);
        let mut c = record("demo-alpha", 2, "2.4.0");
        c.build_id = Some("demo-2.4.0-001".into());
        assert_eq!(effective_build_id(&c).0, "demo-2.4.0-001");
        assert_eq!(effective_build_id(&c).1, BuildIdSource::Record);
    }

    #[test]
    fn image_name_derives_from_the_artifact_repository() {
        let mut rec = record("demo-alpha", 2, "2.4.0");
        assert_eq!(derived_image_name(&rec), None);
        rec.artifact_url = Some(
            "https://github.com/AcmeCorp/DemoAlpha/releases/download/v2.4.0/demo_alpha-2.4.0.whl"
                .into(),
        );
        assert_eq!(
            derived_image_name(&rec).as_deref(),
            Some("ghcr.io/acmecorp/demoalpha")
        );
    }

    #[test]
    fn selectors_pick_records_by_the_documented_rules() {
        let mut r1 = record("demo-alpha", 2, "2.0.0");
        r1.build_id = Some("demo-2.0.0-001".into());
        let mut r2 = record("demo-alpha", 2, "2.4.0");
        r2.build_id = Some("demo-2.4.0-001".into());
        r2.image_digest = Some(format!("sha256:{}", "b".repeat(64)));
        r2.artifact_url = Some("https://github.com/acme/pkg/releases/download/v2.4.0/a.whl".into());
        let mut r3 = record("demo-alpha", 2, "2.4.1"); // patch on same commit
        r3.build_id = Some("demo-2.4.1-001".into());
        let bootstrap = record("demo-beta", 1, "1.0.0"); // null buildId

        let (dir, index) = temp_index(
            "selectors",
            &[
                (
                    "demo-alpha",
                    vec![
                        serde_json::to_string(&r1).unwrap(),
                        serde_json::to_string(&r2).unwrap(),
                        serde_json::to_string(&r3).unwrap(),
                    ],
                ),
                (
                    "demo-beta",
                    vec![serde_json::to_string(&bootstrap).unwrap()],
                ),
            ],
        );

        // by buildId → the exact record.
        let (line, rec) = resolve_record(
            &index,
            "demo-alpha",
            &PinSelector::ByBuildId("demo-2.4.0-001".into()),
        )
        .expect("resolve");
        assert_eq!((line, rec.version.as_str()), (2, "2.4.0"));

        // by commit → newest record of that commit (the patch refresh).
        let (line, rec) = resolve_record(
            &index,
            "demo-alpha",
            &PinSelector::ByCommit("0123456789abcdef0123456789abcdef01234567".into()),
        )
        .expect("resolve");
        assert_eq!((line, rec.version.as_str()), (3, "2.4.1"));

        // latest of major.
        let (line, rec) =
            resolve_record(&index, "demo-alpha", &PinSelector::LatestOfMajor(2)).expect("resolve");
        assert_eq!((line, rec.version.as_str()), (3, "2.4.1"));

        // a derived bootstrap buildId resolves too.
        let derived = effective_build_id(&bootstrap).0;
        let (_line, rec) =
            resolve_record(&index, "demo-beta", &PinSelector::ByBuildId(derived)).expect("resolve");
        assert_eq!(rec.version, "1.0.0");

        // typed errors.
        let err = resolve_record(&index, "demo-alpha", &PinSelector::ByBuildId("nope".into()))
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {err}");
        let err = resolve_record(&index, "demo-alpha", &PinSelector::ByCommit("dead".into()))
            .unwrap_err();
        assert!(err.to_string().contains("40-hex"), "got: {err}");
        let err = resolve_record(&index, "demo-alpha", &PinSelector::LatestOfMajor(7)).unwrap_err();
        assert!(err.to_string().contains("no major-7 record"), "got: {err}");
        let err =
            resolve_record(&index, "unknown-pkg", &PinSelector::LatestOfMajor(1)).unwrap_err();
        assert!(err.to_string().contains("no index records"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_maps_fields_and_enforces_image_rules() {
        let mut rec = record("demo-alpha", 2, "2.4.0");
        rec.build_id = Some("demo-2.4.0-001".into());
        rec.fingerprint = Some("c".repeat(64));
        rec.image_digest = Some(format!("sha256:{}", "d".repeat(64)));
        rec.artifact_url =
            Some("https://github.com/acme/pkg/releases/download/v2.4.0/a.whl".into());
        rec.artifact_sha256 = Some("e".repeat(64));

        let (dir, index) = temp_index(
            "manifest",
            &[("demo-alpha", vec![serde_json::to_string(&rec).unwrap()])],
        );

        let manifest = pin(
            &index,
            "demo-alpha",
            &PinSelector::ByBuildId("demo-2.4.0-001".into()),
            &PinOptions::default(),
        )
        .expect("pin");

        assert_eq!(manifest.contract, "jumbo.deployment-pin/v1");
        assert_eq!(manifest.build_id, "demo-2.4.0-001");
        assert_eq!(manifest.build_id_source, BuildIdSource::Record);
        assert_eq!(manifest.commit, rec.commit);
        assert_eq!(
            manifest.image_ref.as_deref(),
            Some(format!("ghcr.io/acme/pkg@sha256:{}", "d".repeat(64)).as_str())
        );
        assert_eq!(
            manifest.artifact.as_ref().unwrap().url,
            "https://github.com/acme/pkg/releases/download/v2.4.0/a.whl"
        );
        assert_eq!(
            manifest.artifact.as_ref().unwrap().sha256.as_deref(),
            Some(&"e".repeat(64)[..])
        );
        assert_eq!(manifest.fingerprint.as_deref(), Some(&"c".repeat(64)[..]));
        assert_eq!(manifest.record_ref.record_line, 1);
        assert!(manifest.record_ref.index_file.ends_with("demo-alpha.jsonl"));

        // An explicit --image-name overrides the derivation.
        let manifest = pin(
            &index,
            "demo-alpha",
            &PinSelector::ByBuildId("demo-2.4.0-001".into()),
            &PinOptions {
                image_name: Some("registry.internal/demo-alpha-svc".into()),
                require_image: false,
            },
        )
        .expect("pin");
        assert_eq!(
            manifest.image_ref.as_deref(),
            Some(format!("registry.internal/demo-alpha-svc@sha256:{}", "d".repeat(64)).as_str())
        );

        // --require-image on an imageless record → typed error.
        let mut plain = record("demo-beta", 1, "1.0.0");
        plain.build_id = Some("beta-1.0.0-001".into());
        let (dir2, index2) = temp_index(
            "noimage",
            &[("demo-beta", vec![serde_json::to_string(&plain).unwrap()])],
        );
        let err = pin(
            &index2,
            "demo-beta",
            &PinSelector::LatestOfMajor(1),
            &PinOptions {
                require_image: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("PIN_IMAGE_REQUIRED"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}
