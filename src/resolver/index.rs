//! The Jumbo index: reader side.
//!
//! The index is an append-only set of JSONL files, one per package, in the
//! JumboIndex repository (`index/<slug>.jsonl`, one record per line). The
//! last record of a package is its newest; within a major, resolution takes
//! the newest record by record order — never by wall-clock `timestamp`.
//!
//! Sources:
//! - a local clone (repository root, its `index/` directory, or a single
//!   `.jsonl` file), selected via `--index` or `JUMBO_INDEX_PATH`;
//! - an `https://github.com/<owner>/<repo>` URL, fetched read-only with
//!   `gh repo clone --depth 1` into a temporary directory. Only https URLs
//!   to github.com are accepted; no credentials are read or stored here —
//!   `gh` supplies authentication from its own environment.

// The resolver error enum carries rich context strings for actionable
// messages and flows through `anyhow` at the CLI boundary, where its
// stack size is not performance-relevant.
#![allow(clippy::result_large_err)]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::error::ResolverError;

/// Default index repository used when no flag or environment variable is set.
pub const DEFAULT_INDEX_URL: &str = "https://github.com/zephytiju/JumboIndex";
/// Environment variable holding a local index clone path.
pub const INDEX_PATH_ENV: &str = "JUMBO_INDEX_PATH";
/// Environment variable holding an index repository https URL.
pub const INDEX_URL_ENV: &str = "JUMBO_INDEX_URL";

/// One promoted build of one internal package (a single JSONL line).
///
/// Field names round-trip the index record schema exactly (camelCase).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct IndexRecord {
    pub package: String,
    pub major: u64,
    pub version: String,
    pub commit: String,
    /// `sha256(own commit + canonical extract)`; null only in bootstrap records.
    pub fingerprint: Option<String>,
    /// Sorted, tool-independent resolved closure; null only in bootstrap records.
    pub canonical_extract: Option<serde_json::Value>,
    /// Exact HTTPS URL of the artifact; null when the release published none.
    pub artifact_url: Option<String>,
    pub artifact_sha256: Option<String>,
    /// GHCR digest (`sha256:...`) when the build produces a service image.
    pub image_digest: Option<String>,
    /// Pinning and reproduction key (`jumbo build --pinned <buildId>`).
    pub build_id: Option<String>,
    pub pipeline_run: Option<String>,
    /// Who appended: `bootstrap`, `circleci`, `jumbo-publish-github-actions`, ...
    pub executor: Option<String>,
    pub timestamp: String,
}

/// File-name slug for a package: `@` removed, `/` replaced by `-`.
pub fn package_slug(name: &str) -> String {
    name.replace('@', "").replace('/', "-")
}

/// PEP 503 name normalization for Python distribution names.
pub fn normalize_python_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut pending_separator = false;
    for c in lower.chars() {
        if c == '-' || c == '_' || c == '.' {
            if !out.is_empty() {
                pending_separator = true;
            }
        } else {
            if pending_separator {
                out.push('-');
                pending_separator = false;
            }
            out.push(c);
        }
    }
    out
}

/// All records of one package, in file (append) order, with 1-based line numbers.
#[derive(Debug, Clone)]
pub struct PackageRecords {
    /// Display path of the `.jsonl` file the records were read from.
    pub file: String,
    /// (line number, record) pairs in record order; the last is the newest.
    pub entries: Vec<(usize, IndexRecord)>,
}

/// An in-memory snapshot of the index.
#[derive(Debug, Clone, Default)]
pub struct Index {
    packages: BTreeMap<String, PackageRecords>,
}

impl Index {
    /// Load the index from a local or GitHub source.
    pub fn load(source: &IndexSource) -> Result<Self, ResolverError> {
        match source {
            IndexSource::Local(path) => Self::load_local(path),
            IndexSource::GitHub { url, owner, repo } => {
                let temp = fetch_github(url, owner, repo)?;
                let loaded = Self::load_local(&temp);
                // The snapshot is fully in memory; drop the temporary clone.
                let _ = std::fs::remove_dir_all(&temp);
                loaded
            }
        }
    }

    fn load_local(path: &Path) -> Result<Self, ResolverError> {
        let files = discover_jsonl_files(path)?;
        if files.is_empty() {
            return Err(ResolverError::IndexUnavailable {
                origin: path.display().to_string(),
                reason: "no `index/` directory or `*.jsonl` package files found".into(),
            });
        }
        let mut packages = BTreeMap::new();
        for file in files {
            let content =
                std::fs::read_to_string(&file).map_err(|e| ResolverError::IndexUnavailable {
                    origin: path.display().to_string(),
                    reason: format!("failed to read {}: {e}", file.display()),
                })?;
            let mut entries = Vec::new();
            for (index, line) in content.lines().enumerate() {
                let line_no = index + 1;
                if line.trim().is_empty() {
                    continue;
                }
                let record: IndexRecord =
                    serde_json::from_str(line).map_err(|e| ResolverError::InvalidRecord {
                        file: file.display().to_string(),
                        line: line_no,
                        reason: e.to_string(),
                    })?;
                let stem = file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                if package_slug(&record.package) != stem {
                    return Err(ResolverError::InvalidRecord {
                        file: file.display().to_string(),
                        line: line_no,
                        reason: format!(
                            "record for package `{}` does not belong in `{stem}.jsonl`",
                            record.package
                        ),
                    });
                }
                entries.push((line_no, record));
            }
            let stem = file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            packages.insert(
                stem,
                PackageRecords {
                    file: file.display().to_string(),
                    entries,
                },
            );
        }
        Ok(Self { packages })
    }

    /// Whether any record exists for the package name.
    pub fn contains(&self, name: &str) -> bool {
        self.packages.contains_key(&package_slug(name))
    }

    /// All records of a package in record order, if the package is indexed.
    pub fn records(&self, name: &str) -> Option<&PackageRecords> {
        self.packages.get(&package_slug(name))
    }

    /// Every package's records, in file-sorted package order. Callers that
    /// know only a build key (pinned reproduction) search the whole index.
    pub fn iter(&self) -> impl Iterator<Item = &PackageRecords> {
        self.packages.values()
    }

    /// Distinct majors recorded for a package, ascending.
    pub fn majors(&self, name: &str) -> Vec<u64> {
        let mut majors: Vec<u64> = self
            .records(name)
            .map(|r| r.entries.iter().map(|(_, rec)| rec.major).collect())
            .unwrap_or_default();
        majors.sort_unstable();
        majors.dedup();
        majors
    }

    /// The newest record of a major: the last record, in file order, whose
    /// `major` matches. Bootstrap records are valid resolution targets.
    pub fn newest_of_major(&self, name: &str, major: u64) -> Option<(usize, &IndexRecord)> {
        self.records(name)?
            .entries
            .iter()
            .filter(|(_, rec)| rec.major == major)
            .map(|(line, rec)| (*line, rec))
            .next_back()
    }
}

/// Where the index is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexSource {
    /// A local clone: repository root, `index/` directory, or single `.jsonl` file.
    Local(PathBuf),
    /// A GitHub repository fetched read-only via `gh`.
    GitHub {
        url: String,
        owner: String,
        repo: String,
    },
}

impl IndexSource {
    /// Interpret a CLI/config value as an index source.
    ///
    /// `https://` values must point at github.com exactly (no userinfo, no
    /// port, no IP or localhost — the host is compared literally). Anything
    /// that is not an https github.com URL is treated as a local path.
    pub fn from_spec(spec: &str) -> Result<Self, ResolverError> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            return Err(ResolverError::IndexUnavailable {
                origin: spec.to_string(),
                reason: "empty index location".into(),
            });
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("https://") {
            let authority_end = rest.find('/').unwrap_or(rest.len());
            let authority = &rest[..authority_end];
            if authority != "github.com" {
                return Err(ResolverError::IndexUnavailable {
                    origin: spec.to_string(),
                    reason: format!(
                        "https index URLs must point at github.com exactly; `{authority}` is not allowed \
                         (no other hosts, localhost, or IP addresses)"
                    ),
                });
            }
            // The original string keeps its case for the owner/repo path.
            let path = &trimmed["https://".len()..];
            let path = &path[authority_end..];
            let mut segments = path.split('/').filter(|s| !s.is_empty());
            let owner = segments.next();
            let repo = segments.next();
            match (owner, repo) {
                (Some(owner), Some(repo)) if !owner.is_empty() && !repo.is_empty() => {
                    let repo = repo.strip_suffix(".git").unwrap_or(repo);
                    Ok(IndexSource::GitHub {
                        url: format!("https://github.com/{owner}/{repo}"),
                        owner: owner.to_string(),
                        repo: repo.to_string(),
                    })
                }
                _ => Err(ResolverError::IndexUnavailable {
                    origin: spec.to_string(),
                    reason: "GitHub index URLs need the form https://github.com/<owner>/<repo>"
                        .into(),
                }),
            }
        } else if lower.starts_with("http://")
            || lower.starts_with("git@")
            || lower.starts_with("git://")
            || lower.starts_with("ssh://")
            || lower.starts_with("git+")
        {
            Err(ResolverError::IndexUnavailable {
                origin: spec.to_string(),
                reason: "only https URLs to github.com (or a local clone path) are supported"
                    .into(),
            })
        } else {
            Ok(IndexSource::Local(PathBuf::from(trimmed)))
        }
    }

    /// Human-readable description for error messages and logs.
    pub fn describe(&self) -> String {
        match self {
            Self::Local(path) => path.display().to_string(),
            Self::GitHub { url, .. } => url.clone(),
        }
    }
}

/// Find the `.jsonl` files to read under a local path.
fn discover_jsonl_files(path: &Path) -> Result<Vec<PathBuf>, ResolverError> {
    if !path.exists() {
        return Err(ResolverError::IndexUnavailable {
            origin: path.display().to_string(),
            reason: "path does not exist; clone the Jumbo index or set JUMBO_INDEX_PATH".into(),
        });
    }
    if path.is_file() {
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            return Ok(vec![path.to_path_buf()]);
        }
        return Err(ResolverError::IndexUnavailable {
            origin: path.display().to_string(),
            reason: "a single-file index location must be a `.jsonl` file".into(),
        });
    }
    // Repository root (has index/) or the index directory itself.
    for candidate in [path.join("index"), path.to_path_buf()] {
        if candidate.is_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&candidate)
                .map_err(|e| ResolverError::IndexUnavailable {
                    origin: path.display().to_string(),
                    reason: format!("failed to list {}: {e}", candidate.display()),
                })?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
                .collect();
            if !files.is_empty() {
                files.sort();
                return Ok(files);
            }
        }
    }
    Ok(Vec::new())
}

/// Clone the index repository read-only via `gh` into a fresh temp directory.
///
/// `gh` supplies authentication from its own environment; this function never
/// reads, stores, or embeds credentials.
fn fetch_github(url: &str, owner: &str, repo: &str) -> Result<PathBuf, ResolverError> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dest = std::env::temp_dir().join(format!("jumbo-index-{}-{nanos}", std::process::id()));
    let output = Command::new("gh")
        .args([
            "repo",
            "clone",
            &format!("{owner}/{repo}"),
            &dest.display().to_string(),
            "--",
            "--depth",
            "1",
            "--quiet",
        ])
        .output();
    match output {
        Ok(out) if out.status.success() => Ok(dest),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let _ = std::fs::remove_dir_all(&dest);
            Err(ResolverError::IndexUnavailable {
                origin: url.to_string(),
                reason: format!("`gh repo clone {owner}/{repo}` failed: {stderr}"),
            })
        }
        Err(e) => Err(ResolverError::IndexUnavailable {
            origin: url.to_string(),
            reason: format!(
                "failed to run `gh` ({e}); install the GitHub CLI or set JUMBO_INDEX_PATH \
                 to a local index clone"
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn temp_index(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jumbo-index-ut-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("index")).expect("create temp index");
        dir
    }

    fn write_package(dir: &Path, lines: &[String]) {
        let file = dir.join("index").join("demo-alpha.jsonl");
        std::fs::write(file, lines.join("\n") + "\n").expect("write jsonl");
    }

    fn line(rec: &IndexRecord) -> String {
        serde_json::to_string(rec).unwrap()
    }

    #[test]
    fn slug_mapping_matches_index_naming() {
        assert_eq!(
            package_slug("@zephytiju/vangu-constructs"),
            "zephytiju-vangu-constructs"
        );
        assert_eq!(package_slug("juntai-fuse-api"), "juntai-fuse-api");
        assert_eq!(normalize_python_name("Juntai_Fuse.API"), "juntai-fuse-api");
        assert_eq!(normalize_python_name("demo"), "demo");
    }

    #[test]
    fn newest_of_major_follows_record_order_not_wall_clock() {
        let dir = temp_index("order");
        let mut older_wall_clock = record("demo-alpha", 2, "2.4.0");
        older_wall_clock.timestamp = "2026-03-01T00:00:00Z".into();
        let mut newer_wall_clock = record("demo-alpha", 2, "2.1.0");
        newer_wall_clock.timestamp = "2026-08-01T00:00:00Z".into();
        write_package(
            &dir,
            &[
                line(&record("demo-alpha", 2, "2.0.0")),
                line(&newer_wall_clock),
                line(&record("demo-alpha", 1, "1.9.0")),
                line(&older_wall_clock), // appended last → newest of major 2
            ],
        );

        let index = Index::load(&IndexSource::Local(dir.clone())).expect("load index");
        let (line_no, rec) = index.newest_of_major("demo-alpha", 2).expect("record");
        assert_eq!(rec.version, "2.4.0");
        assert_eq!(line_no, 4);

        let (line_no, rec) = index.newest_of_major("demo-alpha", 1).expect("record");
        assert_eq!(rec.version, "1.9.0");
        assert_eq!(line_no, 3);

        assert!(index.newest_of_major("demo-alpha", 3).is_none());
        assert!(index.contains("demo-alpha"));
        assert_eq!(index.majors("demo-alpha"), vec![1, 2]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bootstrap_records_with_null_fingerprint_resolve() {
        let dir = temp_index("bootstrap");
        let rec = record("demo-alpha", 2, "2.1.0"); // all optional fields null
        assert!(rec.fingerprint.is_none());
        write_package(&dir, &[line(&rec)]);
        let index = Index::load(&IndexSource::Local(dir.clone())).expect("load index");
        assert_eq!(
            index.newest_of_major("demo-alpha", 2).unwrap().1.version,
            "2.1.0"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_package_is_not_contained() {
        let dir = temp_index("missing");
        write_package(&dir, &[line(&record("demo-alpha", 2, "2.0.0"))]);
        let index = Index::load(&IndexSource::Local(dir.clone())).expect("load index");
        assert!(!index.contains("unknown-pkg"));
        assert!(index.newest_of_major("unknown-pkg", 1).is_none());
        assert!(index.majors("unknown-pkg").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_in_wrong_file_is_rejected() {
        let dir = temp_index("wrongfile");
        write_package(&dir, &[line(&record("other-package", 1, "1.0.0"))]);
        let err = Index::load(&IndexSource::Local(dir.clone())).unwrap_err();
        assert!(err.to_string().contains("does not belong"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_source_accepts_index_dir_and_repo_root_and_single_file() {
        let dir = temp_index("shapes");
        write_package(&dir, &[line(&record("demo-alpha", 1, "1.0.0"))]);
        let file = dir.join("index").join("demo-alpha.jsonl");
        for spec in [
            dir.join("index"), // index dir
            dir.clone(),       // repo root
            file.clone(),      // single file
        ] {
            let index = Index::load(&IndexSource::Local(spec)).expect("load index");
            assert!(index.contains("demo-alpha"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn source_spec_validates_github_https_urls() {
        assert!(matches!(
            IndexSource::from_spec("https://github.com/zephytiju/JumboIndex"),
            Ok(IndexSource::GitHub { .. })
        ));
        assert!(matches!(
            IndexSource::from_spec("https://github.com/zephytiju/JumboIndex.git"),
            Ok(IndexSource::GitHub { ref repo, .. }) if repo == "JumboIndex"
        ));
        assert!(matches!(
            IndexSource::from_spec("/tmp/some-clone"),
            Ok(IndexSource::Local(_))
        ));
        for bad in [
            "https://example.com/owner/repo",
            "https://localhost/owner/repo",
            "https://127.0.0.1/owner/repo",
            "https://github.com:8443/owner/repo",
            "https://user@github.com/owner/repo",
            "https://evil.github.com.attacker.io/owner/repo",
            "http://github.com/owner/repo",
            "git@github.com:zephytiju/JumboIndex.git",
            "git+https://github.com/owner/repo",
            "ssh://git@github.com/owner/repo",
            "https://github.com",
            "https://github.com/only-owner",
            "",
        ] {
            let err = IndexSource::from_spec(bad).unwrap_err();
            assert!(
                err.to_string().contains("cannot read the Jumbo index"),
                "spec `{bad}` should be rejected, got: {err}"
            );
        }
    }

    #[test]
    fn nonexistent_local_path_errors() {
        let err = Index::load(&IndexSource::Local(PathBuf::from(
            "/nonexistent/jumbo-index-path",
        )))
        .unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }
}
