//! Artifact verification and ingestion into the standard build
//! (Jumbo Build & Versioning Standard, §3.4 Artifact Storage and
//! Materialization).
//!
//! A record's artifact is downloaded by exact URL (the validated
//! github.com-only layer in [`super::fetch`]), verified against the
//! recorded `artifactSha256`, and only then ingested:
//!
//! - **Python wheels** enter the uv build as a direct wheel source in the
//!   injected-source overlay: the file lands at `deps/<slug>/<file>.whl`
//!   and `[tool.uv.sources] <name>.path` points at it — replacing the
//!   synthetic source-overlay project for that dependency.
//! - **Node tarballs** enter via the npm `file:` protocol: the tarball
//!   lands at `deps/<slug>/<file>.tgz` and the manifest entry becomes
//!   `file:deps/<slug>/<file>.tgz`.
//! - A **matched own-record artifact** (the reuse decision on the project
//!   about to build) lands in the project's `dist/` directory — the
//!   standard build output location — so the repeated run consumes the
//!   recorded bytes and performs zero source rebuilds.
//!
//! Unverifiable bytes never proceed: a malformed `artifactSha256`, or any
//! digest mismatch, aborts with a typed error before anything on disk is
//! mutated. All artifacts of a run are fetched and verified first
//! (staging), and the manifest/overlay rewrite happens only afterwards,
//! so a failure leaves the tree in its prior state.
//!
//! # Dependency source fallback
//!
//! The dependency ingestion path (`materialize_dependency_artifacts`, the
//! `--deps` half of `jumbo dedup --materialize`) degrades to **source
//! materialization** instead of failing the build when a dependency's
//! *recorded artifact cannot be used*: the download answered a definitive
//! 404/410 (the release asset is gone — [`MaterializeError::ArtifactGone`]),
//! or the record carries no `artifactSha256` (the bytes would be
//! unverifiable). The dependency then keeps the J3 lock path — the
//! `deps/<slug>/` source overlay at the recorded commit (name, version,
//! and `record.commit` provenance) that the lock generation materialized —
//! and downstream builds consume that coordinate identically to a
//! null-artifact record.
//!
//! Transient failures (5xx, network errors, curl failures) and integrity
//! failures (digest mismatch, malformed digest) **do not** fall back: they
//! abort so real outages and tampering stay visible. The own-record
//! artifact and pinned reproduction never fall back either — a reuse or
//! reproduction that cannot produce the recorded bytes is a failure, not a
//! degradation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::error::MaterializeError;
use super::fetch::{download_artifact, validate_artifact_url, ArtifactUrl};
use crate::resolver::index::IndexRecord;
use crate::resolver::manifest::Ecosystem;
use crate::resolver::ResolvedDependency;

/// Marker file recording the current artifact materialization (below `deps/`).
pub const ARTIFACTS_MARKER_FILE: &str = ".jumbo-artifacts.json";
/// Artifact-materialization marker format identifier.
pub const ARTIFACTS_MARKER_FORMAT: &str = "jumbo-artifact-materialization/1";
/// Directory ingested dependency artifacts live under, relative to the manifest.
pub const INJECTED_DIR: &str = "deps";
/// Default build-output directory for a pulled own-record artifact.
pub const DEFAULT_DIST_DIR: &str = "dist";

/// The kind of artifact a record published, derived from the asset name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    /// Python wheel (`*.whl`), ingested as a direct uv wheel source.
    Wheel,
    /// npm tarball (`*.tgz`), ingested as a `file:` source.
    Tarball,
}

impl ArtifactKind {
    fn of(url: &ArtifactUrl, ecosystem: Ecosystem) -> Result<Self, MaterializeError> {
        let name = url.file_name.to_ascii_lowercase();
        let wheel = name.ends_with(".whl");
        let tgz = name.ends_with(".tgz") || name.ends_with(".tar.gz");
        match (ecosystem, wheel, tgz) {
            (Ecosystem::Python, true, _) => Ok(ArtifactKind::Wheel),
            (Ecosystem::Npm, _, true) => Ok(ArtifactKind::Tarball),
            _ => Err(MaterializeError::UnsupportedArtifactKind {
                package: url.file_name.clone(),
                url: url.url.clone(),
            }),
        }
    }
}

/// How a dependency (or the own record) was materialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MaterializationMode {
    /// The recorded release asset was pulled by exact URL and
    /// sha256-verified.
    Artifact,
    /// The dependency's `deps/<slug>/` source overlay at the recorded
    /// commit stands (the J3 lock path), because the recorded artifact
    /// could not be used — see [`super::ingest`] for the fallback rules.
    Source,
}

impl Default for MaterializationMode {
    /// Markers written before the field existed only ever recorded pulled
    /// artifacts.
    fn default() -> Self {
        Self::Artifact
    }
}

/// One materialized dependency: either a pulled, verified, ingested
/// artifact ([`MaterializationMode::Artifact`]) or a fallback to the
/// dependency's source overlay ([`MaterializationMode::Source`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterializedArtifact {
    /// Language-native package name of the record.
    pub package: String,
    /// Record version.
    pub version: String,
    /// Record commit.
    pub commit: String,
    /// Record buildId, when present.
    #[serde(default)]
    pub build_id: Option<String>,
    /// How the dependency was materialized. Defaults to `artifact` when
    /// deserializing markers written before the field existed.
    #[serde(default)]
    pub mode: MaterializationMode,
    /// Why the source fallback was taken (`mode == "source"` only).
    #[serde(default)]
    pub reason: Option<String>,
    /// Exact URL the artifact came from (null on the source fallback).
    #[serde(default)]
    pub url: Option<String>,
    /// Verified sha256 (64 hex; null on the source fallback).
    #[serde(default)]
    pub sha256: Option<String>,
    /// Stable relative path the dependency materialized at
    /// (`deps/<slug>/<file>` for an artifact, `deps/<slug>` for a source
    /// overlay, or `dist/<file>` for the own record).
    pub path: String,
    /// The source-overlay path this artifact replaced (`deps/<slug>`),
    /// when it replaced one.
    #[serde(default)]
    pub source_overlay_path: Option<String>,
    /// The manifest value written for this artifact (npm `file:` value or
    /// the python `uv.sources` path). Null on the source fallback — the
    /// lock generation's overlay reference stands untouched.
    #[serde(default)]
    pub rewritten: Option<String>,
}

/// The on-disk materialization marker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactsMarker {
    pub format: String,
    pub artifacts: Vec<MaterializedArtifact>,
}

/// Where artifact bytes come from.
#[derive(Debug, Clone)]
pub enum ArtifactProvider {
    /// Download from the validated github.com-only layer (production;
    /// identical behavior locally and in CI).
    Remote,
    /// Resolve the asset by its exact file name inside a local directory
    /// (CI asset cache / offline runs). The recorded sha256 is still
    /// enforced — the cache is a transport, not a trust anchor.
    Cache(PathBuf),
}

impl ArtifactProvider {
    /// Stage the artifact for `record` into `staging_dir`, returning the
    /// staged file path. Bytes are not yet verified.
    fn stage(&self, url: &ArtifactUrl, staging_dir: &Path) -> Result<PathBuf, MaterializeError> {
        std::fs::create_dir_all(staging_dir).map_err(|e| MaterializeError::ArtifactDownload {
            url: url.url.clone(),
            reason: format!("failed to create the staging directory: {e}"),
        })?;
        let staged = staging_dir.join(&url.file_name);
        match self {
            Self::Remote => download_artifact(url, &staged),
            Self::Cache(dir) => {
                let cached = dir.join(&url.file_name);
                if !cached.is_file() {
                    return Err(MaterializeError::ArtifactDownload {
                        url: url.url.clone(),
                        reason: format!(
                            "asset `{}` not found in the artifact directory {}",
                            url.file_name,
                            dir.display()
                        ),
                    });
                }
                std::fs::copy(&cached, &staged).map(|_| ()).map_err(|e| {
                    MaterializeError::ArtifactDownload {
                        url: url.url.clone(),
                        reason: format!("failed to copy the cached asset: {e}"),
                    }
                })
            }
        }?;
        Ok(staged)
    }
}

/// sha256 of a file's bytes, lowercase hex.
pub fn sha256_of_file(path: &Path) -> Result<String, MaterializeError> {
    let bytes = std::fs::read(path).map_err(|e| MaterializeError::Ingestion {
        package: String::new(),
        target: path.display().to_string(),
        reason: format!("failed to read for hashing: {e}"),
    })?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// The recorded digest of a record, or a typed error when the artifact is
/// unverifiable (missing or malformed `artifactSha256`).
pub fn recorded_sha256(record: &IndexRecord, url: &str) -> Result<String, MaterializeError> {
    let Some(expected) = record.artifact_sha256.as_deref() else {
        return Err(MaterializeError::MissingSha256 {
            package: record.package.clone(),
            version: record.version.clone(),
            url: url.to_string(),
        });
    };
    let expected = expected.trim().to_ascii_lowercase();
    let valid = expected.len() == 64
        && expected
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !valid {
        return Err(MaterializeError::InvalidSha256 {
            package: record.package.clone(),
            url: url.to_string(),
            expected: record.artifact_sha256.clone().unwrap_or_default(),
        });
    }
    Ok(expected)
}

/// Verify a staged artifact against the recorded digest.
pub fn verify_sha256(file: &Path, expected: &str) -> Result<String, MaterializeError> {
    let actual = sha256_of_file(file)?;
    if !actual.eq_ignore_ascii_case(expected.trim()) {
        return Err(MaterializeError::DigestMismatch {
            url: file.display().to_string(),
            expected: expected.trim().to_string(),
            actual,
        });
    }
    Ok(actual)
}

/// Stage and verify the artifact of a record: validate the URL against the
/// egress policy, fetch the bytes through the provider, and check the
/// recorded sha256. Returns the validated URL, the verified digest, and
/// the staged file path. Nothing on disk outside staging is touched.
pub fn stage_and_verify(
    record: &IndexRecord,
    provider: &ArtifactProvider,
    staging_dir: &Path,
) -> Result<(ArtifactUrl, String, PathBuf), MaterializeError> {
    let Some(raw_url) = record.artifact_url.as_deref() else {
        return Err(MaterializeError::NoArtifact {
            package: record.package.clone(),
            version: record.version.clone(),
        });
    };
    let url = validate_artifact_url(raw_url)?;
    let expected = recorded_sha256(record, raw_url)?;
    let staged = provider.stage(&url, staging_dir)?;
    let actual = verify_sha256(&staged, &expected)?;
    Ok((url, actual, staged))
}

/// Pull the matched own-record artifact into the project's build-output
/// directory (`dist/` by default). The reuse decision's artifact is the
/// project's own build result; placing it at the standard output location
/// lets the repeated run consume the recorded bytes with zero source
/// rebuilds.
pub fn materialize_self_artifact(
    project_dir: &Path,
    dist_dir: &str,
    record: &IndexRecord,
    ecosystem: Ecosystem,
    provider: &ArtifactProvider,
    staging_dir: &Path,
) -> Result<MaterializedArtifact, MaterializeError> {
    let (url, sha256, staged) = stage_and_verify(record, provider, staging_dir)?;
    ArtifactKind::of(&url, ecosystem)?;
    let dist = project_dir.join(dist_dir);
    std::fs::create_dir_all(&dist).map_err(|e| MaterializeError::Ingestion {
        package: record.package.clone(),
        target: dist.display().to_string(),
        reason: format!("failed to create: {e}"),
    })?;
    let dest = dist.join(&url.file_name);
    std::fs::copy(&staged, &dest).map_err(|e| MaterializeError::Ingestion {
        package: record.package.clone(),
        target: dest.display().to_string(),
        reason: format!("failed to place the artifact: {e}"),
    })?;
    // The copy must be bit-for-bit identical — verify the placed bytes too.
    verify_sha256(&dest, &sha256)?;
    Ok(MaterializedArtifact {
        package: record.package.clone(),
        version: record.version.clone(),
        commit: record.commit.clone(),
        build_id: record.build_id.clone(),
        mode: MaterializationMode::Artifact,
        reason: None,
        url: Some(url.url.clone()),
        sha256: Some(sha256),
        path: format!("{}/{}", dist_dir, url.file_name),
        source_overlay_path: None,
        rewritten: None,
    })
}

/// Load the current artifacts marker, if one exists.
pub fn load_artifacts_marker(
    project_dir: &Path,
) -> Result<Option<ArtifactsMarker>, MaterializeError> {
    let path = project_dir.join(INJECTED_DIR).join(ARTIFACTS_MARKER_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path).map_err(|e| MaterializeError::InvalidMarker {
        path: path.display().to_string(),
        reason: format!("failed to read: {e}"),
    })?;
    let marker: ArtifactsMarker =
        serde_json::from_str(&content).map_err(|e| MaterializeError::InvalidMarker {
            path: path.display().to_string(),
            reason: format!("failed to parse: {e}"),
        })?;
    if marker.format != ARTIFACTS_MARKER_FORMAT {
        return Err(MaterializeError::InvalidMarker {
            path: path.display().to_string(),
            reason: format!("unknown format `{}`", marker.format),
        });
    }
    Ok(Some(marker))
}

/// Whether a dependency-artifact staging failure is a definitive "the
/// recorded artifact cannot be used" — the signal to fall back to the
/// dependency's source overlay instead of failing the build — and the
/// human-readable reason to record for it.
///
/// Falls back only on:
/// - [`MaterializeError::ArtifactGone`] (the download answered 404/410:
///   the release asset no longer exists at the recorded URL);
/// - [`MaterializeError::MissingSha256`] (the record carries no digest,
///   so the artifact bytes would be unverifiable).
///
/// Everything else — 5xx and network errors, digest mismatches, malformed
/// digests, disallowed or unsupported artifact URLs — returns `None` and
/// must abort: real outages and integrity violations have to stay visible.
pub fn source_fallback_reason(err: &MaterializeError) -> Option<String> {
    match err {
        MaterializeError::ArtifactGone { url, status } => Some(format!(
            "artifact download for {url} answered HTTP {status}: the recorded artifact no longer \
             exists at this URL; the source overlay at the recorded commit is used instead"
        )),
        MaterializeError::MissingSha256 { package, .. } => Some(format!(
            "the record for {package} has no artifactSha256: the artifact bytes would be \
             unverifiable; the source overlay at the recorded commit is used instead"
        )),
        _ => None,
    }
}

/// Ingest the recorded artifacts of a manifest's internal dependencies,
/// replacing each dependency's source overlay with the pulled artifact.
///
/// Every candidate artifact is staged and sha256-verified before any
/// mutation; dependencies whose records published no artifact keep their
/// source overlays (reported as `skipped`). The rewrite reuses J3's
/// `deps/<slug>` convention: the artifact lands at `deps/<slug>/<file>`
/// and the manifest reference for that dependency points at the artifact.
///
/// A dependency whose recorded artifact **cannot be used** — the download
/// answered a definitive 404/410, or the record has no `artifactSha256`
/// (see [`source_fallback_reason`]) — falls back to source materialization
/// exactly like a null-artifact record: the `deps/<slug>/` source overlay
/// at the recorded commit (written by the lock generation that precedes
/// `--deps` materialization) stands, the manifest keeps pointing at it,
/// and the returned entry carries `mode: "source"` plus the reason.
/// Transient and integrity failures still abort with nothing mutated.
pub fn materialize_dependency_artifacts(
    manifest: &Path,
    resolution: &[ResolvedDependency],
    ecosystem: Ecosystem,
    provider: &ArtifactProvider,
    staging_dir: &Path,
) -> Result<Vec<MaterializedArtifact>, MaterializeError> {
    let project_dir = manifest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    // Phase 1: stage and verify every candidate. Nothing is mutated on
    // failure, so an unverifiable artifact aborts the whole run with the
    // tree untouched. Definitive absence degrades to the source overlay.
    let mut candidates: Vec<(&ResolvedDependency, ArtifactUrl, String, PathBuf)> = Vec::new();
    let mut fallbacks: Vec<(&ResolvedDependency, String)> = Vec::new();
    let mut skipped: Vec<&ResolvedDependency> = Vec::new();
    for dep in resolution {
        if dep.record.artifact_url.is_none() {
            skipped.push(dep);
            continue;
        }
        match stage_and_verify(&dep.record, provider, staging_dir) {
            Ok((url, sha256, staged)) => {
                ArtifactKind::of(&url, ecosystem)?;
                candidates.push((dep, url, sha256, staged));
            }
            Err(err) => match source_fallback_reason(&err) {
                Some(reason) => fallbacks.push((dep, reason)),
                None => return Err(err),
            },
        }
    }

    // The previous materialization marker drives the fallback cleanup:
    // a dependency that switches from a pulled artifact to its source
    // overlay must not leave the stale artifact file behind.
    let previous: Vec<MaterializedArtifact> = load_artifacts_marker(&project_dir)
        .map(|m| m.map(|m| m.artifacts).unwrap_or_default())
        .unwrap_or_default();

    // Phase 2: ingest — replace each source overlay with its artifact and
    // rewrite the manifest reference.
    let mut materialized = Vec::new();
    for (dep, url, sha256, staged) in candidates {
        let slug = crate::resolver::index::package_slug(&dep.name);
        let overlay = project_dir.join(INJECTED_DIR).join(&slug);
        let dest = overlay.join(&url.file_name);
        // Replace the synthetic source-overlay project wholesale: the
        // artifact is now what this dependency materializes as.
        if overlay.exists() {
            std::fs::remove_dir_all(&overlay).map_err(|e| MaterializeError::Ingestion {
                package: dep.name.clone(),
                target: overlay.display().to_string(),
                reason: format!("failed to remove the previous source overlay: {e}"),
            })?;
        }
        std::fs::create_dir_all(&overlay).map_err(|e| MaterializeError::Ingestion {
            package: dep.name.clone(),
            target: overlay.display().to_string(),
            reason: format!("failed to create: {e}"),
        })?;
        std::fs::copy(&staged, &dest).map_err(|e| MaterializeError::Ingestion {
            package: dep.name.clone(),
            target: dest.display().to_string(),
            reason: format!("failed to place the artifact: {e}"),
        })?;
        verify_sha256(&dest, &sha256)?;

        let relative = format!("{INJECTED_DIR}/{}/{}", slug, url.file_name);
        let rewritten = match ecosystem {
            Ecosystem::Python => rewrite_python_source_path(manifest, &dep.name, &relative)?,
            Ecosystem::Npm => rewrite_npm_source_value(manifest, &dep.name, &relative)?,
        };
        materialized.push(MaterializedArtifact {
            package: dep.name.clone(),
            version: dep.record.version.clone(),
            commit: dep.record.commit.clone(),
            build_id: dep.record.build_id.clone(),
            mode: MaterializationMode::Artifact,
            reason: None,
            url: Some(url.url.clone()),
            sha256: Some(sha256),
            path: relative,
            source_overlay_path: Some(format!("{INJECTED_DIR}/{slug}")),
            rewritten: Some(rewritten),
        });
    }

    // Phase 2b: source fallback — the `deps/<slug>/` source overlay the
    // lock generation wrote stands as this dependency's materialization.
    // A previously pulled artifact for the same package is removed (the
    // marker below replaces its entry), and the manifest reference the
    // lock generation wrote (`deps/<slug>` / `file:deps/<slug>`) is left
    // exactly as `jumbo lock` produces it.
    for (dep, reason) in fallbacks {
        let slug = crate::resolver::index::package_slug(&dep.name);
        let overlay = format!("{INJECTED_DIR}/{slug}");
        for prior in previous.iter().filter(|a| a.package == dep.name) {
            if prior.mode == MaterializationMode::Artifact {
                let stale = project_dir.join(&prior.path);
                // Best-effort: the manifest already points at the overlay
                // directory, so a leftover file is inert even if it cannot
                // be removed.
                let _ = std::fs::remove_file(&stale);
            }
        }
        materialized.push(MaterializedArtifact {
            package: dep.name.clone(),
            version: dep.record.version.clone(),
            commit: dep.record.commit.clone(),
            build_id: dep.record.build_id.clone(),
            mode: MaterializationMode::Source,
            reason: Some(reason),
            url: None,
            sha256: None,
            path: overlay.clone(),
            source_overlay_path: Some(overlay),
            rewritten: None,
        });
    }

    // Preserve the skipped-dependency overlays in the marker so the next
    // run (and humans) can see the full picture.
    let mut artifacts = previous;
    artifacts.retain(|a| !materialized.iter().any(|m| m.package == a.package));
    artifacts.extend(materialized.iter().cloned());
    artifacts.sort_by(|a, b| a.path.cmp(&b.path));
    let marker = ArtifactsMarker {
        format: ARTIFACTS_MARKER_FORMAT.to_string(),
        artifacts,
    };
    let marker_path = project_dir.join(INJECTED_DIR).join(ARTIFACTS_MARKER_FILE);
    std::fs::create_dir_all(project_dir.join(INJECTED_DIR)).map_err(|e| {
        MaterializeError::Ingestion {
            package: String::new(),
            target: marker_path.display().to_string(),
            reason: format!("failed to create deps/: {e}"),
        }
    })?;
    std::fs::write(
        &marker_path,
        serde_json::to_string_pretty(&marker).map_err(|e| MaterializeError::InvalidMarker {
            path: marker_path.display().to_string(),
            reason: format!("failed to serialize: {e}"),
        })? + "\n",
    )
    .map_err(|e| MaterializeError::Ingestion {
        package: String::new(),
        target: marker_path.display().to_string(),
        reason: format!("failed to write: {e}"),
    })?;

    let _ = skipped; // reported by the caller through the resolution
    Ok(materialized)
}

/// Point `[tool.uv.sources] <name>.path` at the ingested artifact
/// (a direct wheel source). Returns the path value written.
fn rewrite_python_source_path(
    manifest: &Path,
    name: &str,
    artifact_path: &str,
) -> Result<String, MaterializeError> {
    let content = std::fs::read_to_string(manifest).map_err(|e| MaterializeError::Ingestion {
        package: name.to_string(),
        target: manifest.display().to_string(),
        reason: format!("failed to read: {e}"),
    })?;
    let mut doc: toml::Table = content.parse().map_err(|e| MaterializeError::Ingestion {
        package: name.to_string(),
        target: manifest.display().to_string(),
        reason: format!("failed to parse TOML: {e}"),
    })?;
    let sources_table = doc
        .entry("tool")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .and_then(|tool| {
            Some(
                tool.entry("uv")
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()?
                    .entry("sources")
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()?
                    .clone(),
            )
        })
        .ok_or_else(|| MaterializeError::Ingestion {
            package: name.to_string(),
            target: manifest.display().to_string(),
            reason: "[tool.uv.sources] is not a table".into(),
        })?;
    let mut sources_table = sources_table;
    let entry = sources_table
        .entry(name.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let entry_table = entry
        .as_table_mut()
        .ok_or_else(|| MaterializeError::Ingestion {
            package: name.to_string(),
            target: manifest.display().to_string(),
            reason: format!("[tool.uv.sources] {name} is not a table"),
        })?;
    entry_table.insert(
        "path".to_string(),
        toml::Value::String(artifact_path.to_string()),
    );
    // Keep the managed-entry list in sync so the lockgen restore can
    // always remove jumbo-owned entries.
    let tool = doc
        .get_mut("tool")
        .and_then(|t| t.as_table_mut())
        .expect("tool table");
    let jumbo = tool
        .entry("jumbo")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let jumbo_table = jumbo
        .as_table_mut()
        .ok_or_else(|| MaterializeError::Ingestion {
            package: name.to_string(),
            target: manifest.display().to_string(),
            reason: "[tool.jumbo] is not a table".into(),
        })?;
    let mut lock_sources: Vec<String> = jumbo_table
        .get("lock_sources")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if !lock_sources.iter().any(|s| s == name) {
        lock_sources.push(name.to_string());
        lock_sources.sort();
    }
    jumbo_table.insert(
        "lock_sources".to_string(),
        toml::Value::Array(lock_sources.into_iter().map(toml::Value::String).collect()),
    );
    let uv = doc
        .get_mut("tool")
        .and_then(|t| t.as_table_mut())
        .and_then(|t| t.get_mut("uv"))
        .and_then(|u| u.as_table_mut())
        .expect("uv table");
    uv.insert("sources".to_string(), toml::Value::Table(sources_table));

    let serialized = toml::to_string_pretty(&doc).map_err(|e| MaterializeError::Ingestion {
        package: name.to_string(),
        target: manifest.display().to_string(),
        reason: format!("failed to serialize: {e}"),
    })?;
    std::fs::write(manifest, &serialized).map_err(|e| MaterializeError::Ingestion {
        package: name.to_string(),
        target: manifest.display().to_string(),
        reason: format!("failed to write: {e}"),
    })?;
    Ok(artifact_path.to_string())
}

/// Point every npm dependency-section entry for `name` at the ingested
/// tarball via the `file:` protocol. Returns the value written.
fn rewrite_npm_source_value(
    manifest: &Path,
    name: &str,
    artifact_path: &str,
) -> Result<String, MaterializeError> {
    let content = std::fs::read_to_string(manifest).map_err(|e| MaterializeError::Ingestion {
        package: name.to_string(),
        target: manifest.display().to_string(),
        reason: format!("failed to read: {e}"),
    })?;
    let mut doc: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| MaterializeError::Ingestion {
            package: name.to_string(),
            target: manifest.display().to_string(),
            reason: format!("failed to parse JSON: {e}"),
        })?;
    let value = format!("file:{artifact_path}");
    for section in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        if let Some(map) = doc.get_mut(section).and_then(|v| v.as_object_mut()) {
            if let Some(slot) = map.get_mut(name) {
                *slot = serde_json::Value::String(value.clone());
            }
        }
    }
    let serialized = serde_json::to_string_pretty(&doc)
        .map(|s| s + "\n")
        .map_err(|e| MaterializeError::Ingestion {
            package: name.to_string(),
            target: manifest.display().to_string(),
            reason: format!("failed to serialize: {e}"),
        })?;
    std::fs::write(manifest, &serialized).map_err(|e| MaterializeError::Ingestion {
        package: name.to_string(),
        target: manifest.display().to_string(),
        reason: format!("failed to write: {e}"),
    })?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::index::IndexRecord;

    fn record(artifact_url: Option<&str>, sha: Option<&str>) -> IndexRecord {
        IndexRecord {
            package: "demo-alpha".to_string(),
            major: 2,
            version: "2.4.0".to_string(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
            fingerprint: None,
            canonical_extract: None,
            artifact_url: artifact_url.map(str::to_string),
            artifact_sha256: sha.map(str::to_string),
            image_digest: None,
            build_id: Some("demo-2.4.0-001".into()),
            pipeline_run: None,
            executor: Some("circleci".into()),
            timestamp: "2026-09-01T00:00:00Z".to_string(),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jumbo-ingest-ut-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    const URL: &str =
        "https://github.com/acme/pkg/releases/download/v2.4.0/demo_alpha-2.4.0-py3-none-any.whl";

    #[test]
    fn sha256_roundtrip_and_verify() {
        let dir = temp_dir("sha");
        let file = dir.join("a.whl");
        std::fs::write(&file, b"wheel bytes").expect("write");
        let digest = sha256_of_file(&file).expect("hash");
        assert_eq!(digest.len(), 64);
        assert_eq!(verify_sha256(&file, &digest).expect("verify"), digest);
        let err = verify_sha256(&file, &"0".repeat(64)).unwrap_err();
        assert!(err.to_string().contains("digest mismatch"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_or_malformed_sha256_is_unverifiable() {
        let err = recorded_sha256(&record(Some(URL), None), URL).unwrap_err();
        assert!(err.to_string().contains("no artifactSha256"), "got: {err}");
        let err = recorded_sha256(&record(Some(URL), Some("zz")), URL).unwrap_err();
        assert!(err.to_string().contains("not a valid 64-hex"), "got: {err}");
    }

    #[test]
    fn no_artifact_url_is_a_typed_error() {
        let dir = temp_dir("noart");
        let err = stage_and_verify(
            &record(None, Some(&"a".repeat(64))),
            &ArtifactProvider::Cache(dir.clone()),
            &dir,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no artifactUrl"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_definitive_absence_falls_back_to_source() {
        use super::MaterializeError as E;
        // 404 and 410: the recorded asset is gone -> source fallback.
        for status in [404u16, 410] {
            let reason = source_fallback_reason(&E::ArtifactGone {
                url: URL.to_string(),
                status,
            })
            .unwrap_or_else(|| panic!("HTTP {status} must fall back"));
            assert!(reason.contains(&format!("HTTP {status}")), "{reason}");
            assert!(reason.contains("no longer exists at this URL"), "{reason}");
            assert!(reason.contains("source overlay"), "{reason}");
        }
        // A record without a digest cannot be verified -> source fallback.
        let reason = source_fallback_reason(&E::MissingSha256 {
            package: "demo-alpha".into(),
            version: "2.4.0".into(),
            url: URL.into(),
        })
        .expect("null artifactSha256 must fall back");
        assert!(reason.contains("no artifactSha256"), "{reason}");
        assert!(reason.contains("unverifiable"), "{reason}");

        // Everything else aborts: 5xx, network errors, auth, digest
        // mismatches, malformed digests, egress-policy violations.
        for no_fallback in [
            E::ArtifactDownload {
                url: URL.into(),
                reason: "HTTP 500".into(),
            },
            E::ArtifactDownload {
                url: URL.into(),
                reason: "failed to run curl".into(),
            },
            E::ArtifactDownload {
                url: URL.into(),
                reason: "HTTP 403: authentication required or insufficient".into(),
            },
            E::DigestMismatch {
                url: URL.into(),
                expected: "0".repeat(64),
                actual: "1".repeat(64),
            },
            E::InvalidSha256 {
                package: "demo-alpha".into(),
                url: URL.into(),
                expected: "zz".into(),
            },
            E::UnsupportedArtifactUrl {
                url: URL.into(),
                reason: "host is not github.com".into(),
            },
            E::NoArtifact {
                package: "demo-alpha".into(),
                version: "2.4.0".into(),
            },
        ] {
            assert!(
                source_fallback_reason(&no_fallback).is_none(),
                "must abort, not fall back: {no_fallback}"
            );
        }
    }

    #[test]
    fn artifact_gone_reports_the_live_404_wording() {
        let err = MaterializeError::ArtifactGone {
            url: URL.to_string(),
            status: 404,
        };
        assert!(
            err.to_string()
                .contains("HTTP 404: the recorded artifact no longer exists at this URL"),
            "got: {err}"
        );
    }

    #[test]
    fn pre_fallback_markers_deserialize_as_artifact_mode() {
        // A marker written before the `mode` field existed (a pulled
        // artifact) must round-trip as an artifact-mode entry.
        let legacy = r#"{
  "format": "jumbo-artifact-materialization/1",
  "artifacts": [
    {
      "package": "demo-alpha",
      "version": "2.4.0",
      "commit": "0123456789abcdef0123456789abcdef01234567",
      "buildId": "demo-2.4.0-001",
      "url": "https://github.com/acme/pkg/releases/download/v2.4.0/demo_alpha-2.4.0-py3-none-any.whl",
      "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "path": "deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl",
      "sourceOverlayPath": "deps/demo-alpha",
      "rewritten": "deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl"
    }
  ]
}"#;
        let marker: ArtifactsMarker = serde_json::from_str(legacy).expect("parse legacy marker");
        assert_eq!(marker.artifacts.len(), 1);
        assert_eq!(marker.artifacts[0].mode, MaterializationMode::Artifact);
        assert_eq!(marker.artifacts[0].reason, None);
        assert_eq!(
            marker.artifacts[0].url.as_deref(),
            Some("https://github.com/acme/pkg/releases/download/v2.4.0/demo_alpha-2.4.0-py3-none-any.whl")
        );
        // Source-mode entries serialize with null url/sha256 and a reason.
        let mut source_entry = marker.artifacts[0].clone();
        source_entry.mode = MaterializationMode::Source;
        source_entry.reason = Some("artifact download answered HTTP 404".into());
        source_entry.url = None;
        source_entry.sha256 = None;
        let value = serde_json::to_value(&source_entry).expect("serialize");
        assert_eq!(value["mode"], "source");
        assert_eq!(value["url"], serde_json::Value::Null);
        assert_eq!(value["sha256"], serde_json::Value::Null);
        assert!(value["reason"].as_str().unwrap().contains("404"));
    }

    #[test]
    fn kind_must_match_ecosystem() {
        let url = validate_artifact_url(URL).expect("valid");
        assert_eq!(
            ArtifactKind::of(&url, Ecosystem::Python).expect("wheel"),
            ArtifactKind::Wheel
        );
        let tgz = validate_artifact_url(
            "https://github.com/acme/pkg/releases/download/v1.0.0/juntai-demo-kit-1.0.0.tgz",
        )
        .expect("valid");
        assert_eq!(
            ArtifactKind::of(&tgz, Ecosystem::Npm).expect("tarball"),
            ArtifactKind::Tarball
        );
        // Cross-ecosystem assets cannot be ingested.
        assert!(ArtifactKind::of(&tgz, Ecosystem::Python).is_err());
        assert!(ArtifactKind::of(&url, Ecosystem::Npm).is_err());
    }

    #[test]
    fn staged_bytes_are_verified_before_use() {
        let dir = temp_dir("stage");
        let cache = dir.join("cache");
        std::fs::create_dir_all(&cache).expect("cache");
        let bytes = b"demo wheel bytes";
        std::fs::write(cache.join("demo_alpha-2.4.0-py3-none-any.whl"), bytes).expect("asset");
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        let sha: String = digest.iter().map(|b| format!("{b:02x}")).collect();

        // Correct digest passes.
        let staging = dir.join("stage-ok");
        let (_url, verified, staged) = stage_and_verify(
            &record(Some(URL), Some(&sha)),
            &ArtifactProvider::Cache(cache.clone()),
            &staging,
        )
        .expect("stage+verify");
        assert_eq!(verified, sha);
        assert!(staged.is_file());

        // Wrong digest aborts.
        let err = stage_and_verify(
            &record(Some(URL), Some(&"b".repeat(64))),
            &ArtifactProvider::Cache(cache.clone()),
            &dir.join("stage-bad"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("digest mismatch"), "got: {err}");

        // Missing asset in the cache aborts.
        let err = stage_and_verify(
            &record(
                Some("https://github.com/acme/pkg/releases/download/v2.4.0/other-2.4.0.whl"),
                Some(&sha),
            ),
            &ArtifactProvider::Cache(cache.clone()),
            &dir.join("stage-miss"),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("not found in the artifact directory"),
            "got: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn npm_rewrite_replaces_file_values_by_name() {
        let dir = temp_dir("npmrw");
        let manifest = dir.join("package.json");
        std::fs::write(
            &manifest,
            r#"{
  "name": "consumer",
  "dependencies": {
    "@juntai/demo-kit": "file:deps/juntai-demo-kit",
    "lodash": "^4.17.21"
  }
}
"#,
        )
        .expect("write manifest");
        let value = rewrite_npm_source_value(
            &manifest,
            "@juntai/demo-kit",
            "deps/juntai-demo-kit/juntai-demo-kit-1.2.0.tgz",
        )
        .expect("rewrite");
        assert_eq!(value, "file:deps/juntai-demo-kit/juntai-demo-kit-1.2.0.tgz");
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(
            doc["dependencies"]["@juntai/demo-kit"].as_str().unwrap(),
            "file:deps/juntai-demo-kit/juntai-demo-kit-1.2.0.tgz"
        );
        assert_eq!(doc["dependencies"]["lodash"].as_str().unwrap(), "^4.17.21");
        // Idempotent: rewriting again yields the same value.
        rewrite_npm_source_value(
            &manifest,
            "@juntai/demo-kit",
            "deps/juntai-demo-kit/juntai-demo-kit-1.2.0.tgz",
        )
        .expect("rewrite again");
        let doc2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(doc, doc2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_rewrite_points_uv_sources_at_the_artifact() {
        let dir = temp_dir("pyrw");
        let manifest = dir.join("pyproject.toml");
        std::fs::write(
            &manifest,
            "[project]\nname = \"consumer\"\ndependencies = [\"demo-alpha==2.4.0\"]\n\n[tool.uv.sources]\ndemo-alpha = { path = \"deps/demo-alpha\" }\n\n[tool.jumbo]\nlock_sources = [\"demo-alpha\"]\n",
        )
        .expect("write manifest");
        let written = rewrite_python_source_path(
            &manifest,
            "demo-alpha",
            "deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl",
        )
        .expect("rewrite");
        assert_eq!(written, "deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl");
        let doc: toml::Table = std::fs::read_to_string(&manifest).unwrap().parse().unwrap();
        assert_eq!(
            doc["tool"]["uv"]["sources"]["demo-alpha"]["path"]
                .as_str()
                .unwrap(),
            "deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl"
        );
        assert_eq!(
            doc["project"]["dependencies"].as_array().unwrap()[0]
                .as_str()
                .unwrap(),
            "demo-alpha==2.4.0"
        );
        let lock_sources: Vec<String> = doc["tool"]["jumbo"]["lock_sources"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
        assert_eq!(lock_sources, vec!["demo-alpha"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
