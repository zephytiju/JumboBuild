//! Jumbo resolver core.
//!
//! Resolution semantics (Jumbo Build & Versioning Standard, §2.2):
//! - declarations reduce to a major version (see [`declaration`]);
//! - resolution = the **newest index record** whose major matches the
//!   declaration, newest by record order (the last matching line), never by
//!   wall clock;
//! - an internal dependency with **no index record** is an absorption error
//!   naming the package and the absorption step;
//! - internal = packages resolvable through the index (a record exists) plus
//!   the internal npm scopes `@juntai/*` and legacy `@zephytiju/*` and any
//!   dependency declared with the jumbo `name@MAJOR` syntax; third-party
//!   dependencies pass through untouched for the normal language tooling.

// The resolver error enum carries rich context strings for actionable
// messages and flows through `anyhow` at the CLI boundary, where its
// stack size is not performance-relevant.
#![allow(clippy::result_large_err)]
pub mod declaration;
pub mod error;
pub mod index;
pub mod manifest;

use std::path::Path;

use serde::Serialize;

use declaration::{Declaration, Spec};
use error::ResolverError;
use index::{DEFAULT_INDEX_URL, INDEX_PATH_ENV, INDEX_URL_ENV};
use manifest::{load_manifest, Ecosystem, Manifest};

pub use index::{Index, IndexRecord, IndexSource};

/// Resolve the index location: `--index` flag, then `JUMBO_INDEX_PATH`
/// (local clone), then `JUMBO_INDEX_URL`, then the default repository URL.
pub fn resolve_source(flag: Option<&str>) -> Result<IndexSource, ResolverError> {
    if let Some(spec) = flag {
        return IndexSource::from_spec(spec);
    }
    if let Ok(path) = std::env::var(INDEX_PATH_ENV) {
        if !path.trim().is_empty() {
            return IndexSource::from_spec(&path);
        }
    }
    if let Ok(url) = std::env::var(INDEX_URL_ENV) {
        if !url.trim().is_empty() {
            return IndexSource::from_spec(&url);
        }
    }
    IndexSource::from_spec(DEFAULT_INDEX_URL)
}

/// A resolved internal dependency: declaration plus the chosen index record.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedDependency {
    pub name: String,
    pub declared_major: u64,
    pub declaration: String,
    pub extras: Vec<String>,
    pub marker: Option<String>,
    pub location: String,
    /// The index record that satisfies the declaration.
    pub record: IndexRecord,
    /// 1-based line of the record in the package's `.jsonl` file.
    pub record_line: usize,
    /// The `.jsonl` index file the record was read from.
    pub index_file: String,
}

/// A third-party dependency, passed through untouched.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalDependency {
    pub name: String,
    pub declaration: String,
    pub location: String,
}

/// Full resolution report for one manifest.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestResolution {
    pub manifest: String,
    pub ecosystem: &'static str,
    pub internal: Vec<ResolvedDependency>,
    pub external: Vec<ExternalDependency>,
}

/// Form-validation report for one manifest (`--check`): no records required.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestCheckReport {
    pub manifest: String,
    pub ecosystem: &'static str,
    pub internal: Vec<CheckedDependency>,
    pub external: Vec<ExternalDependency>,
}

/// An internal dependency whose declaration form passed validation.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckedDependency {
    pub name: String,
    pub declared_major: u64,
    pub declaration: String,
    pub location: String,
}

/// Result of resolving a single declaration string.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SingleResolution {
    pub name: String,
    pub declared_major: u64,
    pub record: IndexRecord,
    pub record_line: usize,
    /// The `.jsonl` index file the record was read from.
    pub index_file: String,
}

/// Whether a declaration names an internal package for its ecosystem.
///
/// Internal = present in the index, or provably internal by construction:
/// the internal npm scopes, or the jumbo `name@MAJOR` syntax (which only
/// exists for jumbo-resolved packages).
fn is_internal(ecosystem: Ecosystem, decl: &Declaration, index: &Index) -> bool {
    match ecosystem {
        Ecosystem::Python => matches!(decl.spec, Spec::Major(_)) || index.contains(&decl.name),
        Ecosystem::Npm => {
            declaration::is_npm_internal_name(&decl.name) || index.contains(&decl.name)
        }
    }
}

/// Validate and resolve every dependency of a manifest against the index.
pub fn resolve_manifest(path: &Path, index: &Index) -> Result<ManifestResolution, ResolverError> {
    let manifest: Manifest = load_manifest(path)?;
    let mut internal = Vec::new();
    let mut external = Vec::new();

    for decl in &manifest.declarations {
        if !is_internal(manifest.ecosystem, decl, index) {
            external.push(ExternalDependency {
                name: decl.name.clone(),
                declaration: decl.raw.clone(),
                location: decl.location.clone(),
            });
            continue;
        }
        let Some(major) = decl.spec.major() else {
            return Err(ResolverError::NotMajorOnly {
                name: decl.name.clone(),
                raw: decl.raw.clone(),
                location: decl.location.clone(),
                reason: decl.spec.not_major_only_reason(),
                ecosystem: manifest.ecosystem.as_str(),
                accepted: manifest.ecosystem.accepted_forms(),
            });
        };
        match index.newest_of_major(&decl.name, major) {
            Some((record_line, record)) => {
                let index_file = index
                    .records(&decl.name)
                    .map(|records| records.file.clone())
                    .unwrap_or_default();
                internal.push(ResolvedDependency {
                    name: decl.name.clone(),
                    declared_major: major,
                    declaration: decl.raw.clone(),
                    extras: decl.extras.clone(),
                    marker: decl.marker.clone(),
                    location: decl.location.clone(),
                    record: record.clone(),
                    record_line,
                    index_file,
                })
            }
            None => return Err(missing_record_error(index, decl, major)),
        }
    }

    Ok(ManifestResolution {
        manifest: manifest.path.display().to_string(),
        ecosystem: manifest.ecosystem.as_str(),
        internal,
        external,
    })
}

/// Validate declaration forms only (no record lookups, no absorption errors).
pub fn check_manifest(path: &Path, index: &Index) -> Result<ManifestCheckReport, ResolverError> {
    let manifest = load_manifest(path)?;
    let mut internal = Vec::new();
    let mut external = Vec::new();

    for decl in &manifest.declarations {
        if !is_internal(manifest.ecosystem, decl, index) {
            external.push(ExternalDependency {
                name: decl.name.clone(),
                declaration: decl.raw.clone(),
                location: decl.location.clone(),
            });
            continue;
        }
        let Some(major) = decl.spec.major() else {
            return Err(ResolverError::NotMajorOnly {
                name: decl.name.clone(),
                raw: decl.raw.clone(),
                location: decl.location.clone(),
                reason: decl.spec.not_major_only_reason(),
                ecosystem: manifest.ecosystem.as_str(),
                accepted: manifest.ecosystem.accepted_forms(),
            });
        };
        internal.push(CheckedDependency {
            name: decl.name.clone(),
            declared_major: major,
            declaration: decl.raw.clone(),
            location: decl.location.clone(),
        });
    }

    Ok(ManifestCheckReport {
        manifest: manifest.path.display().to_string(),
        ecosystem: manifest.ecosystem.as_str(),
        internal,
        external,
    })
}

/// Resolve one declaration string (CLI form), e.g. `juntai-fuse-api[http]@2`
/// or `@juntai/demo-kit@^1`.
pub fn resolve_declaration(index: &Index, input: &str) -> Result<SingleResolution, ResolverError> {
    let decl = parse_declaration_string(input)?;
    let Some(major) = decl.spec.major() else {
        return Err(ResolverError::NotMajorOnly {
            name: decl.name.clone(),
            raw: decl.raw.clone(),
            location: "<declaration>".to_string(),
            reason: decl.spec.not_major_only_reason(),
            ecosystem: if decl.name.starts_with('@') {
                Ecosystem::Npm.as_str()
            } else {
                Ecosystem::Python.as_str()
            },
            accepted: if decl.name.starts_with('@') {
                Ecosystem::Npm.accepted_forms()
            } else {
                Ecosystem::Python.accepted_forms()
            },
        });
    };
    match index.newest_of_major(&decl.name, major) {
        Some((record_line, record)) => {
            let index_file = index
                .records(&decl.name)
                .map(|records| records.file.clone())
                .unwrap_or_default();
            Ok(SingleResolution {
                name: decl.name.clone(),
                declared_major: major,
                record: record.clone(),
                record_line,
                index_file,
            })
        }
        None => Err(missing_record_error(index, &decl, major)),
    }
}

/// Parse a standalone declaration string, inferring the ecosystem grammar:
/// `@`-prefixed names use the npm grammar; everything else uses the Python
/// grammar, falling back to the npm grammar only for npm-only range
/// operators (`pkg@^1`, `pkg@~2`) that the Python grammar cannot express.
fn parse_declaration_string(input: &str) -> Result<Declaration, ResolverError> {
    let input_trim = input.trim();
    if let Some(body) = input_trim.strip_prefix('@') {
        // @scope/name@range — the range separator is the '@' after the name.
        let (name, range) = match body.split_once('@') {
            Some((name, range)) => (format!("@{name}"), range),
            None => (input_trim.to_string(), ""),
        };
        return declaration::parse_npm(&name, range, "<declaration>");
    }
    match declaration::parse_python(input_trim, "<declaration>") {
        Ok(decl) => Ok(decl),
        Err(ResolverError::InvalidDeclaration { .. }) => match input_trim.split_once('@') {
            Some((name, range)) if range.starts_with('^') || range.starts_with('~') => {
                declaration::parse_npm(name, range, "<declaration>")
            }
            _ => declaration::parse_python(input_trim, "<declaration>"),
        },
        Err(other) => Err(other),
    }
}

/// Build the right error for a missing index lookup: absorption when the
/// package has no index record at all; otherwise a major-availability error.
fn missing_record_error(index: &Index, decl: &Declaration, major: u64) -> ResolverError {
    if index.contains(&decl.name) {
        let majors = index.majors(&decl.name);
        let available = if majors.is_empty() {
            "none recorded".to_string()
        } else {
            majors
                .iter()
                .map(|m| m.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        ResolverError::NoRecordForMajor {
            name: decl.name.clone(),
            major,
            available,
        }
    } else if matches!(decl.spec, Spec::Major(_)) || declaration::is_npm_internal_name(&decl.name) {
        ResolverError::Absorption {
            name: decl.name.clone(),
            major,
            location: decl.location.clone(),
        }
    } else {
        ResolverError::NotInternalPackage {
            name: decl.name.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::index::IndexRecord;

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

    fn fixture_index(tag: &str) -> (std::path::PathBuf, Index) {
        let dir = std::env::temp_dir().join(format!(
            "jumbo-resolver-ut-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let index_dir = dir.join("index");
        std::fs::create_dir_all(&index_dir).expect("create index dir");
        let write = |slug: &str, lines: &[String]| {
            std::fs::write(
                index_dir.join(format!("{slug}.jsonl")),
                lines.join("\n") + "\n",
            )
            .expect("write jsonl");
        };
        write(
            "demo-alpha",
            &[
                serde_json::to_string(&record("demo-alpha", 2, "2.0.0")).unwrap(),
                serde_json::to_string(&record("demo-alpha", 1, "1.9.0")).unwrap(),
                serde_json::to_string(&record("demo-alpha", 2, "2.4.0")).unwrap(),
            ],
        );
        write(
            "juntai-demo-kit",
            &[serde_json::to_string(&record("@juntai/demo-kit", 1, "1.2.0")).unwrap()],
        );
        let index = Index::load(&IndexSource::Local(index_dir.clone())).expect("load index");
        (dir, index)
    }

    #[test]
    fn resolve_declaration_takes_newest_record_of_major() {
        let (_dir, index) = fixture_index("single");
        let res = resolve_declaration(&index, "demo-alpha@2").expect("resolve");
        assert_eq!(res.name, "demo-alpha");
        assert_eq!(res.declared_major, 2);
        assert_eq!(res.record.version, "2.4.0");
        assert_eq!(res.record_line, 3);

        // Collapsing-range form resolves identically.
        let res = resolve_declaration(&index, "demo-alpha>=2,<3").expect("resolve");
        assert_eq!(res.record.version, "2.4.0");

        // Scoped npm form.
        let res = resolve_declaration(&index, "@juntai/demo-kit@^1").expect("resolve");
        assert_eq!(res.name, "@juntai/demo-kit");
        assert_eq!(res.record.version, "1.2.0");
    }

    #[test]
    fn resolve_declaration_missing_major_and_absorption() {
        let (_dir, index) = fixture_index("missing");
        let err = resolve_declaration(&index, "demo-alpha@3").unwrap_err();
        assert!(err.to_string().contains("no major-3 record"), "got: {err}");
        assert!(err.to_string().contains("1, 2"));

        let err = resolve_declaration(&index, "@juntai/not-absorbed@^1").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("absorption error"), "got: {msg}");
        assert!(msg.contains("@juntai/not-absorbed"));
        assert!(msg.contains("JumboIndex"), "must point at the index: {msg}");
        assert!(
            msg.contains("jumbo-publish"),
            "must name the absorption step: {msg}"
        );

        let err = resolve_declaration(&index, "some-pkg@2").unwrap_err();
        assert!(err.to_string().contains("absorption error"), "got: {err}");

        // A non-internal range declaration for an unknown package is not an
        // absorption error.
        let err = resolve_declaration(&index, "unknown-third-party>=1,<2").unwrap_err();
        assert!(err.to_string().contains("not internal"), "got: {err}");
    }

    #[test]
    fn resolve_declaration_rejects_non_major_forms() {
        let (_dir, index) = fixture_index("reject");
        let err = resolve_declaration(&index, "demo-alpha@2.1").unwrap_err();
        assert!(err.to_string().contains("is not a major"), "got: {err}");
        let err = resolve_declaration(&index, "demo-alpha").unwrap_err();
        assert!(err.to_string().contains("not major-only"), "got: {err}");
        let err =
            resolve_declaration(&index, "demo-alpha @ git+https://github.com/o/r.git").unwrap_err();
        assert!(err.to_string().contains("forbidden"), "got: {err}");
    }

    #[test]
    fn resolve_manifest_python_end_to_end() {
        let (dir, index) = fixture_index("manifest");
        let path = dir.join("pyproject.toml");
        std::fs::write(
            &path,
            r#"
[project]
name = "consumer"
dependencies = [
    "demo-alpha[http]@2",
    "numpy>=1.26",
]
"#,
        )
        .expect("write manifest");

        let report = resolve_manifest(&path, &index).expect("resolve manifest");
        assert_eq!(report.ecosystem, "python");
        assert_eq!(report.internal.len(), 1);
        assert_eq!(report.internal[0].name, "demo-alpha");
        assert_eq!(report.internal[0].record.version, "2.4.0");
        assert_eq!(report.internal[0].extras, vec!["http"]);
        assert_eq!(report.external.len(), 1);
        assert_eq!(report.external[0].name, "numpy");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_manifest_python_internal_range_resolves() {
        let (dir, index) = fixture_index("range");
        let path = dir.join("pyproject.toml");
        std::fs::write(&path, "[project]\ndependencies = [\"demo-alpha==2.*\"]\n")
            .expect("write manifest");
        let report = resolve_manifest(&path, &index).expect("resolve manifest");
        assert_eq!(report.internal[0].record.version, "2.4.0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_manifest_rejects_internal_violations() {
        let (dir, index) = fixture_index("violations");
        let write = |tag: &str, body: &str| {
            let case_dir = dir.join(tag);
            std::fs::create_dir_all(&case_dir).expect("create case dir");
            let path = case_dir.join("pyproject.toml");
            std::fs::write(&path, body).expect("write");
            path
        };

        // Multi-major internal range.
        let path = write(
            "multi",
            "[project]\ndependencies = [\"demo-alpha>=1,<3\"]\n",
        );
        let err = resolve_manifest(&path, &index).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not major-only"), "got: {msg}");
        assert!(msg.contains("spans multiple majors"), "got: {msg}");

        // Un-absorbed internal package (jumbo syntax, no index record).
        let path = write("absorb", "[project]\ndependencies = [\"missing-pkg@1\"]\n");
        let err = resolve_manifest(&path, &index).unwrap_err();
        assert!(err.to_string().contains("absorption error"), "got: {err}");

        // Git URL in optional-dependencies.
        let path = write(
            "giturl",
            "[project.optional-dependencies]\nextra = [\"demo @ git+https://github.com/org/repo.git\"]\n",
        );
        let err = resolve_manifest(&path, &index).unwrap_err();
        assert!(err.to_string().contains("forbidden"), "got: {err}");

        // Wheel URL in dependency-groups.
        let path = write(
            "wheel",
            "[dependency-groups]\ndev = [\"demo @ https://example.org/demo-1.0.0-py3-none-any.whl\"]\n",
        );
        let err = resolve_manifest(&path, &index).unwrap_err();
        assert!(err.to_string().contains("forbidden"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_manifest_npm_end_to_end() {
        let (dir, index) = fixture_index("npm");
        let path = dir.join("package.json");
        std::fs::write(
            &path,
            r#"{
  "name": "consumer",
  "dependencies": {
    "@juntai/demo-kit": "^1",
    "lodash": "^4.17.21"
  }
}"#,
        )
        .expect("write manifest");

        let report = resolve_manifest(&path, &index).expect("resolve manifest");
        assert_eq!(report.ecosystem, "npm");
        assert_eq!(report.internal.len(), 1);
        assert_eq!(report.internal[0].name, "@juntai/demo-kit");
        assert_eq!(report.internal[0].record.version, "1.2.0");
        assert_eq!(report.external[0].name, "lodash");

        // Un-absorbed scoped internal package.
        std::fs::write(&path, r#"{"dependencies": {"@juntai/ghost-kit": "2.x"}}"#)
            .expect("write manifest");
        let err = resolve_manifest(&path, &index).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("absorption error"), "got: {msg}");
        assert!(msg.contains("@juntai/ghost-kit"));

        // Multi-major internal range.
        std::fs::write(&path, r#"{"dependencies": {"@juntai/demo-kit": ">=1,<3"}}"#)
            .expect("write manifest");
        let err = resolve_manifest(&path, &index).unwrap_err();
        assert!(
            err.to_string().contains("spans multiple majors"),
            "got: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_manifest_validates_forms_without_records() {
        let (dir, index) = fixture_index("check");
        let path = dir.join("pyproject.toml");
        std::fs::write(
            &path,
            "[project]\ndependencies = [\"missing-pkg@1\", \"numpy>=1.26\"]\n",
        )
        .expect("write manifest");
        // `missing-pkg@1` has no index record, but its FORM is valid → check passes.
        let report = check_manifest(&path, &index).expect("check");
        assert_eq!(report.internal.len(), 1);
        assert_eq!(report.internal[0].name, "missing-pkg");
        assert_eq!(report.internal[0].declared_major, 1);
        assert_eq!(report.external[0].name, "numpy");

        // A bad form still fails check (indexed package with a bad range).
        std::fs::write(&path, "[project]\ndependencies = [\"demo-alpha>=1,<9\"]\n")
            .expect("write manifest");
        assert!(check_manifest(&path, &index).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
