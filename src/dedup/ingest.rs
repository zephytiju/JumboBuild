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
//! the record carries no `artifactUrl`, or it has no `artifactSha256` (the
//! bytes would be unverifiable).
//!
//! Source materialization fetches the dependency's **real repository tree
//! at the recorded commit** — never the minimal lock stub, which exists for
//! resolution only and is not buildable — from GitHub's tarball host
//! (`https://codeload.github.com/<owner>/<repo>/tar.gz/<commit>`, through
//! the same validated https layer, authenticated with the run's available
//! credentials exactly like every other fetch: a PRIVATE repository
//! answers 404 to an anonymous codeload request, so the executor must pass
//! a token that can read member repositories — the org CI app installation
//! token — through `GITHUB_TOKEN`/`GH_TOKEN`; see
//! [`super::fetch::source_tarball_gone_reason`] for the split-by-credential
//! failure guidance). The tree unpacks into `deps/<slug>/`
//! (leading directory stripped), replacing any standing minimal stub so
//! exactly one materialization exists, and the downstream uv/npm build
//! consumes it as the ordinary workspace member / `file:` source. The
//! repository coordinate is resolved by (a) parsing owner/repo from the
//! record's `artifactUrl` when it is a github.com URL (dead asset URLs
//! still carry it), else (b) a repo map (`--repo-map` / `JUMBO_REPO_MAP`,
//! package name to https clone URL); without either, a typed error names
//! the package and both options.
//!
//! The unpacked project must carry the record's package name (normalized;
//! the version may differ — the record's version semantics hold), or the
//! run aborts with a typed error. Tarballs have no recorded sha256, so the
//! materialization marker and the dedup JSON record the provenance
//! (tarball URL + commit) instead. Re-runs are idempotent: when
//! `deps/<slug>/` already holds the source materialization of the same
//! commit and provenance, the fetch is skipped.
//!
//! Transient failures (5xx, network errors, curl failures) and integrity
//! failures (digest mismatch, malformed digest) **do not** fall back: they
//! abort so real outages and tampering stay visible — and a fallback fetch
//! that itself fails (unresolvable repository, dead tarball URL, name
//! mismatch) aborts too, because leaving the unbuildable stub standing
//! would just move the failure into the build. The own-record artifact and
//! pinned reproduction never fall back either — a reuse or reproduction
//! that cannot produce the recorded bytes is a failure, not a degradation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::error::MaterializeError;
use super::fetch::{
    codeload_tarball_url, download_artifact, is_full_commit_sha, parse_github_repo,
    validate_artifact_url, ArtifactUrl, RepoCoordinate,
};
use crate::fingerprint::lockgen::manifest_is_stub;
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
    /// The dependency's real repository tree at the recorded commit was
    /// fetched (codeload tarball, provenance URL recorded) into
    /// `deps/<slug>/`, because the recorded artifact could not be used —
    /// see [`super::ingest`] for the fallback rules.
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
    /// Exact URL the artifact came from; on the source fallback, the
    /// provenance codeload tarball URL of the fetched repository tree
    /// (tarballs carry no recorded sha256, so url+commit are the
    /// provenance). Null on fallback markers written before that held.
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

/// A repo map: package name to https clone URL (`{"pkg":
/// "https://github.com/owner/repo"}`), passed via `--repo-map` or
/// `JUMBO_REPO_MAP`. The delivery platform derives it from its catalog.
///
/// Lookup tries the exact name, then the PEP 503 normalized form, then the
/// file-name slug, so a record's normalized name meets a catalog's exact
/// one (and vice versa).
#[derive(Debug, Clone)]
pub struct RepoMap {
    path: PathBuf,
    entries: BTreeMap<String, String>,
    /// Slug/normalized index over the keys, so a record's normalized name
    /// meets a catalog's exact one (and vice versa).
    aliases: BTreeMap<String, String>,
}

impl RepoMap {
    /// Load and parse a repo map file (a JSON object of package name to
    /// https clone URL).
    pub fn load(path: &Path) -> Result<Self, MaterializeError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| MaterializeError::InvalidRepoMap {
                path: path.display().to_string(),
                reason: format!("failed to read: {e}"),
            })?;
        let value: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| MaterializeError::InvalidRepoMap {
                path: path.display().to_string(),
                reason: format!("failed to parse JSON: {e}"),
            })?;
        let Some(object) = value.as_object() else {
            return Err(MaterializeError::InvalidRepoMap {
                path: path.display().to_string(),
                reason: "the top level must be a JSON object of package name to clone URL"
                    .to_string(),
            });
        };
        let mut entries = BTreeMap::new();
        for (name, url) in object {
            let Some(url) = url.as_str() else {
                return Err(MaterializeError::InvalidRepoMap {
                    path: path.display().to_string(),
                    reason: format!("the entry for `{name}` is not a string"),
                });
            };
            if parse_github_repo(url).is_none() {
                return Err(MaterializeError::InvalidRepoMap {
                    path: path.display().to_string(),
                    reason: format!(
                        "the entry for `{name}` is not an https github.com clone URL: {url}"
                    ),
                });
            }
            entries.insert(name.clone(), url.to_string());
        }
        let aliases = entries
            .keys()
            .map(|name| {
                let alias = crate::resolver::index::package_slug(
                    &crate::resolver::index::normalize_python_name(name),
                );
                (alias, name.clone())
            })
            .collect();
        Ok(Self {
            path: path.to_path_buf(),
            entries,
            aliases,
        })
    }

    /// The display path of the map (for error contexts).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The clone URL recorded for `package`, if any: by exact name, then
    /// by normalized/slugged form (either side of the lookup).
    fn lookup(&self, package: &str) -> Option<&str> {
        if let Some(url) = self.entries.get(package) {
            return Some(url.as_str());
        }
        let alias = crate::resolver::index::package_slug(
            &crate::resolver::index::normalize_python_name(package),
        );
        self.aliases
            .get(&alias)
            .and_then(|name| self.entries.get(name))
            .map(String::as_str)
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
/// recorded artifact cannot be used" — the signal to fall back to
/// materializing the dependency's real source instead of failing the
/// build — and the human-readable reason to record for it.
///
/// Falls back only on:
/// - [`MaterializeError::ArtifactGone`] (the download answered 404/410:
///   the release asset no longer exists at the recorded URL);
/// - [`MaterializeError::NoArtifact`] (the record published no artifact);
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
             exists at this URL; the real repository source at the recorded commit is fetched \
             instead"
        )),
        MaterializeError::NoArtifact { package, .. } => Some(format!(
            "the record for {package} has no artifactUrl: there is no artifact to pull; the real \
             repository source at the recorded commit is fetched instead"
        )),
        MaterializeError::MissingSha256 { package, .. } => Some(format!(
            "the record for {package} has no artifactSha256: the artifact bytes would be \
             unverifiable; the real repository source at the recorded commit is fetched instead"
        )),
        _ => None,
    }
}

/// Resolve the repository coordinate of a dependency's record: owner/repo
/// parsed from the record's `artifactUrl` when it is an https github.com
/// URL (dead asset URLs still carry it), else the repo map's clone URL for
/// the package. Without either, a typed error names the package and both
/// resolution options.
fn resolve_repo_coordinate(
    record: &IndexRecord,
    repo_map: Option<&RepoMap>,
) -> Result<RepoCoordinate, MaterializeError> {
    let unresolvable = |reason: String| MaterializeError::UnresolvableRepository {
        package: record.package.clone(),
        version: record.version.clone(),
        reason,
    };
    if let Some(raw) = record.artifact_url.as_deref() {
        if let Some(coordinate) = parse_github_repo(raw) {
            return Ok(coordinate);
        }
    }
    if let Some(map) = repo_map {
        if let Some(clone_url) = map.lookup(&record.package) {
            return parse_github_repo(clone_url).ok_or_else(|| {
                unresolvable(format!(
                    "the repo map {} maps it to `{clone_url}`, which is not an https \
                     github.com clone URL",
                    map.path().display()
                ))
            });
        }
        return Err(unresolvable(format!(
            "its artifactUrl is not an https github.com URL and the repo map {} has no entry \
             for `{}`",
            map.path().display(),
            record.package
        )));
    }
    Err(unresolvable(match record.artifact_url.as_deref() {
        Some(raw) => format!(
            "its artifactUrl `{raw}` is not an https github.com URL and no repo map was provided \
             (--repo-map <PATH> / JUMBO_REPO_MAP)"
        ),
        None => "it has no artifactUrl to parse an owner/repo from and no repo map was provided \
                (--repo-map <PATH> / JUMBO_REPO_MAP)"
            .to_string(),
    }))
}

/// The codeload provenance URL of a record's real repository tree: the
/// coordinate resolved, the commit validated as a full sha, and the URL
/// passed through the egress validation.
fn source_provenance_url(
    record: &IndexRecord,
    repo_map: Option<&RepoMap>,
) -> Result<String, MaterializeError> {
    let coordinate = resolve_repo_coordinate(record, repo_map)?;
    if !is_full_commit_sha(&record.commit) {
        return Err(MaterializeError::UnresolvableRepository {
            package: record.package.clone(),
            version: record.version.clone(),
            reason: format!(
                "the recorded commit `{}` is not a full 40-hex sha, so no repository tree can \
                 be fetched at it",
                record.commit
            ),
        });
    }
    Ok(codeload_tarball_url(&coordinate, &record.commit)?.url)
}

/// Whether `deps/<slug>/` already holds this record's source
/// materialization: a previous marker entry in source mode at the same
/// commit and provenance URL, plus a standing manifest that is not a
/// minimal lock stub (the lock regeneration that precedes materialization
/// re-writes stubs, so a clobbered stub means the real source has to be
/// fetched again).
fn source_materialization_stands(
    project_dir: &Path,
    overlay: &str,
    record: &IndexRecord,
    provenance_url: &str,
    ecosystem: Ecosystem,
    previous: &[MaterializedArtifact],
) -> bool {
    let marked = previous.iter().any(|a| {
        a.package == record.package
            && a.mode == MaterializationMode::Source
            && a.commit == record.commit
            && a.url.as_deref() == Some(provenance_url)
    });
    if !marked {
        return false;
    }
    let manifest_name = match ecosystem {
        Ecosystem::Python => "pyproject.toml",
        Ecosystem::Npm => "package.json",
    };
    match std::fs::read_to_string(project_dir.join(overlay).join(manifest_name)) {
        Ok(content) => !manifest_is_stub(&content),
        Err(_) => false,
    }
}

/// Whether a link target escapes the destination when resolved from the
/// entry's (already stripped) relative path.
fn link_target_escapes(entry_relative: &Path, target: &Path, root_relative: bool) -> bool {
    let mut depth: Vec<std::ffi::OsString> = if root_relative {
        Vec::new()
    } else {
        entry_relative
            .parent()
            .map(|p| {
                p.components()
                    .filter_map(|c| match c {
                        std::path::Component::Normal(c) => Some(c.to_os_string()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    for component in target.components() {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => return true,
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if depth.pop().is_none() {
                    return true;
                }
            }
            std::path::Component::Normal(c) => depth.push(c.to_os_string()),
        }
    }
    false
}

/// Unpack a GitHub tarball into `dest`, stripping the leading directory
/// (`<repo>-<ref>/`) every GitHub tarball carries.
///
/// Two passes over the staged file: first a validation pass (exactly one
/// shared leading directory; no link target that resolves outside the
/// unpacked tree), then the tar crate's own `unpack` — which additionally
/// refuses entry paths escaping the destination — into a sibling staging
/// directory whose single root is renamed into place.
fn unpack_tarball_strip_one(
    tarball: &Path,
    dest: &Path,
    package: &str,
) -> Result<(), MaterializeError> {
    let failure = |reason: String| MaterializeError::SourceTarball {
        package: package.to_string(),
        url: tarball.display().to_string(),
        reason,
    };
    // Pass 1: shape validation.
    let mut root_name: Option<std::ffi::OsString> = None;
    {
        let file = std::fs::File::open(tarball)
            .map_err(|e| failure(format!("failed to open the staged tarball: {e}")))?;
        let gz = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(gz);
        let entries = archive
            .entries()
            .map_err(|e| failure(format!("failed to read the tarball: {e}")))?;
        for entry in entries {
            let entry =
                entry.map_err(|e| failure(format!("failed to read a tarball entry: {e}")))?;
            let entry_type = entry.header().entry_type();
            // PAX/GNU metadata entries — every GitHub codeload tarball
            // carries a `pax_global_header` entry — are archive
            // metadata, not members. Counting one as a leading directory
            // rejected every GitHub source fallback ("the tarball has
            // more than one leading directory (`pax_global_header`,
            // `<repo>-<ref>`)"; MeridianConfigArtifactPlugin run
            // 37261477754); the single-root contract is about MEMBERS.
            if matches!(entry_type.as_byte(), b'g' | b'x' | b'L' | b'K') {
                continue;
            }
            let path = entry
                .path()
                .map_err(|e| failure(format!("failed to read an entry path: {e}")))?
                .to_path_buf();
            let mut components = path
                .components()
                .filter(|c| !matches!(c, std::path::Component::CurDir));
            let Some(root) = components.next() else {
                continue;
            };
            let root = root.as_os_str().to_os_string();
            match &root_name {
                None => root_name = Some(root),
                Some(existing) if *existing == root => {}
                Some(existing) => {
                    return Err(failure(format!(
                        "the tarball has more than one leading directory (`{}`, `{}`); only \
                         single-root GitHub tarballs are accepted",
                        existing.to_string_lossy(),
                        root.to_string_lossy()
                    )))
                }
            }
            let relative: PathBuf = components.collect();
            if relative.as_os_str().is_empty() {
                continue; // the root directory entry itself
            }
            if matches!(entry_type, tar::EntryType::Symlink | tar::EntryType::Link) {
                if let Some(target) = entry.link_name().ok().flatten() {
                    // A hardlink target is archive-root relative; a symlink
                    // target is relative to the entry's own directory.
                    let root_relative = entry_type == tar::EntryType::Link;
                    if link_target_escapes(&relative, &target, root_relative) {
                        return Err(failure(format!(
                            "entry `{}` links to `{}`, which escapes the unpack destination",
                            relative.display(),
                            target.display()
                        )));
                    }
                }
            }
        }
    }
    let Some(root_name) = root_name else {
        return Err(failure(
            "the tarball is empty: no repository tree to unpack".to_string(),
        ));
    };
    // Pass 2: unpack via the tar crate (its own traversal guards apply),
    // then hoist the single root directory into place.
    let file = std::fs::File::open(tarball)
        .map_err(|e| failure(format!("failed to open the staged tarball: {e}")))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    let parent = dest.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|e| failure(format!("failed to create the staging tree: {e}")))?;
    let tmp = parent.join(format!(
        ".{}-unpack-{}",
        dest.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "source".to_string()),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    archive
        .unpack(&tmp)
        .map_err(|e| failure(format!("failed to unpack the tarball: {e}")))?;
    let unpacked_root = tmp.join(&root_name);
    if !unpacked_root.is_dir() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(failure(format!(
            "the tarball's leading directory `{}` was not unpacked",
            root_name.to_string_lossy()
        )));
    }
    let result = (|| -> std::io::Result<()> {
        if dest.exists() {
            std::fs::remove_dir_all(dest)?;
        }
        std::fs::rename(&unpacked_root, dest)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result.map_err(|e| failure(format!("failed to place the unpacked tree: {e}")))
}

/// Sanity-check the unpacked project: its manifest must carry the record's
/// package name (normalized). The version may differ — the record's
/// version semantics hold; the name may not.
fn verify_source_project_name(
    dir: &Path,
    package: &str,
    ecosystem: Ecosystem,
) -> Result<(), MaterializeError> {
    let mismatch = |reason: String| MaterializeError::SourceNameMismatch {
        package: package.to_string(),
        path: dir.display().to_string(),
        reason,
    };
    match ecosystem {
        Ecosystem::Python => {
            let manifest = dir.join("pyproject.toml");
            let content = std::fs::read_to_string(&manifest).map_err(|e| {
                mismatch(format!(
                    "no pyproject.toml at the repository root (failed to read: {e})"
                ))
            })?;
            let doc: toml::Table = content
                .parse()
                .map_err(|e| mismatch(format!("its pyproject.toml is not valid TOML: {e}")))?;
            let name = doc
                .get("project")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .ok_or_else(|| mismatch("its pyproject.toml has no [project].name".into()))?;
            let expected = crate::resolver::index::normalize_python_name(package);
            let found = crate::resolver::index::normalize_python_name(name);
            if found != expected {
                return Err(mismatch(format!(
                    "its manifest name `{name}` does not match the record's package `{package}`"
                )));
            }
        }
        Ecosystem::Npm => {
            let manifest = dir.join("package.json");
            let content = std::fs::read_to_string(&manifest).map_err(|e| {
                mismatch(format!(
                    "no package.json at the repository root (failed to read: {e})"
                ))
            })?;
            let doc: serde_json::Value = content
                .parse()
                .map_err(|e| mismatch(format!("its package.json is not valid JSON: {e}")))?;
            let name = doc
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| mismatch("its package.json has no name".into()))?;
            if name != package {
                return Err(mismatch(format!(
                    "its manifest name `{name}` does not match the record's package `{package}`"
                )));
            }
        }
    }
    Ok(())
}

/// Recursively copy a directory (symlinks preserved on unix).
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    for entry in walkdir::WalkDir::new(from) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(from).unwrap_or(entry.path());
        let target = to.join(relative);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(target)?;
        } else if entry.file_type().is_symlink() {
            let link = std::fs::read_link(entry.path())?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(link, &target)?;
            #[cfg(not(unix))]
            std::fs::copy(entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Move a directory, falling back to a recursive copy when the staging
/// area and the project live on different filesystems.
fn move_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy_dir(from, to)?;
            std::fs::remove_dir_all(from)
        }
    }
}

/// One staged fallback dependency: the fallback reason, the provenance
/// codeload URL, and the staged (unpacked, name-verified) tree — `None`
/// when the same materialization already stands and the fetch was skipped.
struct StagedFallback<'a> {
    dep: &'a ResolvedDependency,
    reason: String,
    provenance_url: String,
    staged_tree: Option<PathBuf>,
}

/// Download the dependency's real repository tree at the recorded commit
/// into `staged_tree` (unpacked, leading directory stripped, name
/// verified). Nothing outside staging is touched.
fn fetch_source_tree(
    record: &IndexRecord,
    ecosystem: Ecosystem,
    provider: &ArtifactProvider,
    staging_dir: &Path,
    repo_map: Option<&RepoMap>,
    staged_tree: &Path,
) -> Result<(), MaterializeError> {
    let coordinate = resolve_repo_coordinate(record, repo_map)?;
    let url = codeload_tarball_url(&coordinate, &record.commit)?;
    let failure = |reason: String| MaterializeError::SourceTarball {
        package: record.package.clone(),
        url: url.url.clone(),
        reason,
    };
    provider
        .stage(&url, staging_dir)
        .map_err(|e| match e {
            MaterializeError::ArtifactGone { status, .. } => {
                failure(super::fetch::source_tarball_gone_reason(
                    status,
                    super::fetch::github_token_present(),
                ))
            }
            MaterializeError::ArtifactDownload { reason, .. } => failure(reason),
            other => other,
        })
        .and_then(|tarball| unpack_tarball_strip_one(&tarball, staged_tree, &record.package))?;
    verify_source_project_name(staged_tree, &record.package, ecosystem)
}

/// Ingest the recorded artifacts of a manifest's internal dependencies,
/// replacing each dependency's source overlay with the pulled artifact.
///
/// Every candidate artifact is staged and sha256-verified — and every
/// fallback source tree is fetched, unpacked, and name-verified — before
/// any mutation, so a failure aborts the whole run with the tree
/// untouched.
///
/// A dependency whose recorded artifact **cannot be used** — the download
/// answered a definitive 404/410, the record published no artifact, or it
/// has no `artifactSha256` (see [`source_fallback_reason`]) — falls back
/// to **source materialization**: the dependency's real repository tree at
/// the recorded commit is fetched from codeload.github.com and unpacked
/// into `deps/<slug>/`, replacing the minimal lock stub (which exists for
/// resolution only and is never buildable), and the returned entry carries
/// `mode: "source"` plus the reason and the provenance URL. When the same
/// source materialization already stands, the fetch is skipped. Transient
/// and integrity failures — of the artifact pull or of the fallback fetch
/// itself — still abort with nothing mutated.
pub fn materialize_dependency_artifacts(
    manifest: &Path,
    resolution: &[ResolvedDependency],
    ecosystem: Ecosystem,
    provider: &ArtifactProvider,
    staging_dir: &Path,
    repo_map: Option<&RepoMap>,
) -> Result<Vec<MaterializedArtifact>, MaterializeError> {
    let project_dir = manifest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    // The previous materialization marker drives the fallback cleanup and
    // the idempotence check: a dependency that switches from a pulled
    // artifact to real source must not leave the stale artifact file
    // behind, and a standing source materialization skips its re-fetch.
    let previous: Vec<MaterializedArtifact> = load_artifacts_marker(&project_dir)
        .map(|m| m.map(|m| m.artifacts).unwrap_or_default())
        .unwrap_or_default();

    // Phase 1: stage and verify every candidate — pulled artifacts and
    // fallback source trees alike. Definitive absence degrades to the
    // real-source fetch; everything else aborts with nothing mutated.
    let mut candidates: Vec<(&ResolvedDependency, ArtifactUrl, String, PathBuf)> = Vec::new();
    let mut fallbacks: Vec<StagedFallback> = Vec::new();
    for dep in resolution {
        match stage_and_verify(&dep.record, provider, staging_dir) {
            Ok((url, sha256, staged)) => {
                ArtifactKind::of(&url, ecosystem)?;
                candidates.push((dep, url, sha256, staged));
            }
            Err(err) => {
                let Some(reason) = source_fallback_reason(&err) else {
                    return Err(err);
                };
                let provenance_url = source_provenance_url(&dep.record, repo_map)?;
                let slug = crate::resolver::index::package_slug(&dep.name);
                let overlay = format!("{INJECTED_DIR}/{slug}");
                let staged_tree = if source_materialization_stands(
                    &project_dir,
                    &overlay,
                    &dep.record,
                    &provenance_url,
                    ecosystem,
                    &previous,
                ) {
                    None // the same materialization stands; skip the fetch
                } else {
                    let staged_tree = staging_dir.join("source").join(&slug);
                    fetch_source_tree(
                        &dep.record,
                        ecosystem,
                        provider,
                        staging_dir,
                        repo_map,
                        &staged_tree,
                    )?;
                    Some(staged_tree)
                };
                fallbacks.push(StagedFallback {
                    dep,
                    reason,
                    provenance_url,
                    staged_tree,
                });
            }
        }
    }

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

    // Phase 2b: source fallback — the dependency's real repository tree at
    // the recorded commit becomes its materialization. A previously pulled
    // artifact for the same package is removed, the standing minimal stub
    // is replaced (exactly one materialization exists), and the manifest
    // reference the lock generation wrote (`deps/<slug>` /
    // `file:deps/<slug>`) keeps pointing at the directory — now holding a
    // real, buildable project.
    for prep in &fallbacks {
        let dep = prep.dep;
        let slug = crate::resolver::index::package_slug(&dep.name);
        let overlay = format!("{INJECTED_DIR}/{slug}");
        for prior in previous.iter().filter(|a| a.package == dep.name) {
            if prior.mode == MaterializationMode::Artifact {
                let stale = project_dir.join(&prior.path);
                // Best-effort: the manifest points at the overlay
                // directory, so a leftover file is inert even if it cannot
                // be removed.
                let _ = std::fs::remove_file(&stale);
            }
        }
        if let Some(staged_tree) = &prep.staged_tree {
            let target = project_dir.join(&overlay);
            if target.exists() {
                std::fs::remove_dir_all(&target).map_err(|e| MaterializeError::Ingestion {
                    package: dep.name.clone(),
                    target: target.display().to_string(),
                    reason: format!(
                        "failed to remove the standing minimal stub before placing the \
                             real source: {e}"
                    ),
                })?;
            }
            move_dir(staged_tree, &target).map_err(|e| MaterializeError::Ingestion {
                package: dep.name.clone(),
                target: target.display().to_string(),
                reason: format!("failed to place the fetched source: {e}"),
            })?;
        }
        materialized.push(MaterializedArtifact {
            package: dep.name.clone(),
            version: dep.record.version.clone(),
            commit: dep.record.commit.clone(),
            build_id: dep.record.build_id.clone(),
            mode: MaterializationMode::Source,
            reason: Some(prep.reason.clone()),
            url: Some(prep.provenance_url.clone()),
            sha256: None,
            path: overlay.clone(),
            source_overlay_path: Some(overlay),
            rewritten: None,
        });
    }

    // Preserve the marker picture for dependencies this run did not touch.
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
            assert!(reason.contains("real repository source"), "{reason}");
        }
        // A record without an artifact and a record without a digest both
        // degrade to the real-source fetch.
        let reason = source_fallback_reason(&E::NoArtifact {
            package: "demo-alpha".into(),
            version: "2.4.0".into(),
        })
        .expect("a null artifactUrl must fall back");
        assert!(reason.contains("no artifactUrl"), "{reason}");
        assert!(reason.contains("real repository source"), "{reason}");
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
        // Source-mode entries serialize with the provenance url (the
        // codeload tarball the tree came from) and no sha256.
        let mut source_entry = marker.artifacts[0].clone();
        source_entry.mode = MaterializationMode::Source;
        source_entry.reason = Some("artifact download answered HTTP 404".into());
        source_entry.url = Some(
            "https://codeload.github.com/acme/pkg/tar.gz/0123456789abcdef0123456789abcdef01234567"
                .into(),
        );
        source_entry.sha256 = None;
        let value = serde_json::to_value(&source_entry).expect("serialize");
        assert_eq!(value["mode"], "source");
        assert_eq!(
            value["url"],
            "https://codeload.github.com/acme/pkg/tar.gz/0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(value["sha256"], serde_json::Value::Null);
        assert!(value["reason"].as_str().unwrap().contains("404"));
        // Markers written by the pre-provenance fallback (a null url)
        // still deserialize.
        let pre_provenance = r#"{
  "format": "jumbo-artifact-materialization/1",
  "artifacts": [
    {
      "package": "demo-gamma",
      "version": "1.0.0",
      "commit": "f00dcafe0123456789abcdef0123456789abcdef0",
      "mode": "source",
      "reason": "artifact download answered HTTP 404",
      "url": null,
      "sha256": null,
      "path": "deps/demo-gamma",
      "sourceOverlayPath": "deps/demo-gamma",
      "rewritten": null
    }
  ]
}"#;
        let marker: ArtifactsMarker =
            serde_json::from_str(pre_provenance).expect("parse pre-provenance marker");
        assert_eq!(marker.artifacts[0].mode, MaterializationMode::Source);
        assert_eq!(marker.artifacts[0].url, None);
    }

    #[test]
    fn repo_map_loads_and_looks_up_by_every_name_form() {
        let dir = temp_dir("repomap");
        let map_path = dir.join("repo-map.json");
        std::fs::write(
            &map_path,
            r#"{
  "demo-alpha": "https://github.com/acme/demo-alpha",
  "@juntai/demo-kit": "https://github.com/acme/demo-kit.git",
  "Meridian_Storage.Semantics": "https://github.com/juntai/meridian-storage"
}
"#,
        )
        .expect("write map");
        let map = RepoMap::load(&map_path).expect("load");
        // Exact, normalized (PEP 503), and slug lookups all resolve — in
        // either direction between the record's name and the map's key.
        assert_eq!(
            map.lookup("demo-alpha"),
            Some("https://github.com/acme/demo-alpha")
        );
        assert_eq!(
            map.lookup("Demo_Alpha"),
            Some("https://github.com/acme/demo-alpha")
        );
        assert_eq!(
            map.lookup("@juntai/demo-kit"),
            Some("https://github.com/acme/demo-kit.git")
        );
        assert_eq!(
            map.lookup("juntai-demo-kit"),
            Some("https://github.com/acme/demo-kit.git")
        );
        assert_eq!(
            map.lookup("meridian-storage-semantics"),
            Some("https://github.com/juntai/meridian-storage")
        );
        assert_eq!(
            map.lookup("Meridian_Storage.Semantics"),
            Some("https://github.com/juntai/meridian-storage")
        );
        assert_eq!(map.lookup("unknown-package"), None);
        assert_eq!(map.path(), map_path.as_path());

        // Bad maps are typed errors: not an object, non-string entry,
        // non-github URL value.
        std::fs::write(dir.join("array.json"), "[]").expect("array");
        let err = RepoMap::load(&dir.join("array.json")).unwrap_err();
        assert!(err.to_string().contains("top level must be a JSON object"));
        std::fs::write(dir.join("bad-url.json"), r#"{"x": "https://evil.com/y"}"#).expect("bad");
        let err = RepoMap::load(&dir.join("bad-url.json")).unwrap_err();
        assert!(err
            .to_string()
            .contains("not an https github.com clone URL"));
        let err = RepoMap::load(&dir.join("missing.json")).unwrap_err();
        assert!(err.to_string().contains("failed to read"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn source_name_check_normalizes_python_but_not_npm() {
        let dir = temp_dir("namecheck");
        let python = dir.join("py");
        std::fs::create_dir_all(python.join("src")).expect("dirs");
        std::fs::write(
            python.join("pyproject.toml"),
            "[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n\n[project]\nname = \"Demo_Gamma\"\nversion = \"9.9.9\"\n",
        )
        .expect("manifest");
        // Normalized match; version may differ.
        verify_source_project_name(&python, "demo-gamma", Ecosystem::Python).expect("python ok");
        let err = verify_source_project_name(&python, "demo-omega", Ecosystem::Python).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");

        let npm = dir.join("npm");
        std::fs::create_dir_all(&npm).expect("dirs");
        std::fs::write(
            npm.join("package.json"),
            r#"{ "name": "@acme/demo-kit", "version": "3.0.0" }"#,
        )
        .expect("manifest");
        verify_source_project_name(&npm, "@acme/demo-kit", Ecosystem::Npm).expect("npm ok");
        let err = verify_source_project_name(&npm, "demo-kit", Ecosystem::Npm).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");

        // No manifest at all is a typed mismatch.
        let err =
            verify_source_project_name(&dir.join("empty"), "x", Ecosystem::Python).unwrap_err();
        assert!(err.to_string().contains("no pyproject.toml"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
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

    #[test]
    fn github_pax_global_header_does_not_break_the_single_root_contract() {
        // Every GitHub codeload tarball carries a pax_global_header
        // metadata entry. The single-root validation must treat PAX/GNU
        // metadata as non-members — counting one as a second leading
        // directory rejected every GitHub source fallback
        // (MeridianConfigArtifactPlugin run 37261477754).
        let dir = temp_dir("pax-tarball");
        let tarball = dir.join("source.tar.gz");
        let file = std::fs::File::create(&tarball).unwrap();
        let gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(gz);
        let root = "MeridianObjectCommon-e520402f778378c50d3d6dd9181da2bfb4c3e469";

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::new(b'g'));
        header.set_size(0);
        header.set_path("pax_global_header").unwrap();
        header.set_cksum();
        builder.append(&header, std::io::empty()).unwrap();

        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Directory);
        header.set_path(format!("{root}/")).unwrap();
        header.set_cksum();
        builder.append(&header, std::io::empty()).unwrap();

        let mut header = tar::Header::new_gnu();
        header.set_size(5);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_path(format!("{root}/pyproject.toml")).unwrap();
        header.set_cksum();
        builder
            .append(&header, std::io::Cursor::new(b"hello"))
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();

        let dest = dir.join("unpacked");
        unpack_tarball_strip_one(&tarball, &dest, "demo-alpha")
            .expect("the pax_global_header entry must not count as a leading directory");
        assert_eq!(
            std::fs::read_to_string(dest.join("pyproject.toml")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn two_real_roots_are_still_rejected() {
        let dir = temp_dir("two-roots");
        let tarball = dir.join("source.tar.gz");
        let file = std::fs::File::create(&tarball).unwrap();
        let gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(gz);
        for root in ["repo-a-abc", "repo-b-def"] {
            let mut header = tar::Header::new_gnu();
            header.set_size(5);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_path(format!("{root}/file.txt")).unwrap();
            header.set_cksum();
            builder
                .append(&header, std::io::Cursor::new(b"hello"))
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();

        let err = unpack_tarball_strip_one(&tarball, &dir.join("dest"), "demo-alpha")
            .expect_err("two real roots must stay rejected");
        assert!(
            err.to_string().contains("more than one leading directory"),
            "unexpected error: {err}"
        );
    }
}
