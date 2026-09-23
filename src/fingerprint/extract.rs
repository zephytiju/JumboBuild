//! The canonical extract: the sorted, tool-independent view of a generated
//! language lock (Jumbo Build & Versioning Standard, §2.3).
//!
//! Raw lock bytes are never hashed. Tool-version formatting changes, key
//! ordering, and machine-specific paths would alter a raw-byte hash without
//! any input change. The canonical extract reduces a lock to what the build
//! materializes — package name, resolved version, integrity digest or
//! internal source coordinate — and serializes it deterministically:
//!
//! - shape: `jumbo-canonical-extract/1`, matching the JumboIndex record
//!   schema (`canonicalExtract` field) so an index record round-trips the
//!   exact closure it was fingerprinted from;
//! - entries sorted by (name, version, source, digest, path) with exact
//!   duplicates removed — lock order, key order, and formatting never
//!   matter;
//! - Python names PEP 503-normalized; npm names kept verbatim;
//! - injected internal sources carry the stable relative coordinate
//!   `deps/<slug>` regardless of how the tool wrote the path;
//! - digests are the lock's own integrity strings (uv `sha256:…` sdist
//!   hash, npm `sha512-…` integrity), never lock formatting.
//!
//! A formatting-only lock change produces the same extract and therefore
//! no fingerprint change; any real resolution change produces a different
//! extract and therefore a rebuild.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

use super::error::FingerprintError;
use crate::resolver::index::{normalize_python_name, package_slug};

/// Canonical-extract format identifier (JumboIndex record schema).
pub const EXTRACT_FORMAT: &str = "jumbo-canonical-extract/1";

/// Where a lock entry resolved from. The wire values are the exact enum
/// strings of the JumboIndex `canonicalExtract.source` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntrySource {
    /// Internal package injected from a Jumbo index record (`deps/<slug>`).
    Index,
    /// Third-party Python distribution resolved from a package index.
    PyPI,
    /// Third-party npm package resolved from a registry.
    Npm,
    /// Local source that is not an index coordinate (e.g. a uv workspace
    /// member outside `deps/`).
    Path,
}

/// One resolved package in the canonical extract.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExtractEntry {
    /// Language-native, normalized package name.
    pub name: String,
    /// Resolved version, exactly as materialized.
    pub version: String,
    /// Where the entry resolved from.
    pub source: EntrySource,
    /// Integrity digest for registry entries; null for internal coordinates.
    pub digest: Option<String>,
    /// Stable relative path for injected sources; null otherwise.
    pub path: Option<String>,
}

/// The canonical extract of one generated lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalExtract {
    /// Always [`EXTRACT_FORMAT`].
    pub format: String,
    /// Sorted, deduplicated entries.
    pub entries: Vec<ExtractEntry>,
}

impl CanonicalExtract {
    /// Build an extract, sorting and deduplicating entries into canonical
    /// order: (name, version, source, digest, path), nulls first.
    pub fn new(mut entries: Vec<ExtractEntry>) -> Self {
        entries.sort();
        entries.dedup();
        Self {
            format: EXTRACT_FORMAT.to_string(),
            entries,
        }
    }

    /// The canonical JSON serialization: compact, struct field order
    /// (`format`, `entries`; then `name`, `version`, `source`, `digest`,
    /// `path`), sorted entries. This exact byte string is the fingerprint
    /// preimage component and what an index record stores.
    pub fn canonical_json(&self) -> String {
        serde_json::to_string(self).expect("canonical extract serializes")
    }
}

/// Compute `sha256(own commit + canonical extract)`.
///
/// Preimage, byte-exact: `<40-hex lowercase commit> "\n" <canonical JSON>`.
/// An equal fingerprint means the same commit was already built with the
/// same full resolution.
pub fn compute_fingerprint(
    commit: &str,
    extract: &CanonicalExtract,
) -> Result<String, FingerprintError> {
    let trimmed = commit.trim();
    let valid = trimmed.len() == 40
        && trimmed
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !valid {
        return Err(FingerprintError::InvalidCommit {
            value: commit.to_string(),
        });
    }
    let mut hasher = Sha256::new();
    hasher.update(trimmed.as_bytes());
    hasher.update(b"\n");
    hasher.update(extract.canonical_json().as_bytes());
    let digest = hasher.finalize();
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// Normalize a local path written by a lock tool into its stable relative
/// POSIX form.
///
/// - strips any leading `./` sequence and converts `\` to `/`;
/// - `None` for the project root (`.` / empty) — the own package is not
///   part of its dependency closure;
/// - rejects absolute paths: a machine-specific path in a lock would make
///   the extract non-deterministic, which is a lock-generation bug.
fn normalize_local_path(raw: &str, origin: &str) -> Result<Option<String>, FingerprintError> {
    let mut p = raw.trim().replace('\\', "/");
    while let Some(stripped) = p.strip_prefix("./") {
        p = stripped.to_string();
    }
    if p.is_empty() || p == "." {
        return Ok(None);
    }
    if p.starts_with('/') {
        return Err(FingerprintError::MachineSpecificPath {
            path: raw.to_string(),
            origin: origin.to_string(),
        });
    }
    Ok(Some(p))
}

/// Classify a local (non-registry) source path.
///
/// A path whose parent directory component is `deps` is a jumbo-injected
/// index source and is rewritten to the stable coordinate `deps/<basename>`
/// — even when the tool wrote it relative to a workspace root. Anything
/// else is a plain local path entry.
fn classify_local_path(p: &str) -> (EntrySource, String) {
    let mut parts = p.split('/');
    let _ = parts.next_back();
    let parent = parts.next_back();
    let base = p.rsplit('/').next().unwrap_or(p);
    match parent {
        Some("deps") => (EntrySource::Index, format!("deps/{base}")),
        _ => (EntrySource::Path, p.to_string()),
    }
}

/// Extract the canonical view of a `uv.lock` (TOML) or `package-lock.json`.
pub fn extract_lock(content: &str, file_name: &str) -> Result<CanonicalExtract, FingerprintError> {
    match file_name {
        "uv.lock" => extract_uv_lock(content),
        "package-lock.json" => extract_npm_lock(content),
        other => Err(FingerprintError::InvalidLock {
            path: other.to_string(),
            reason: "expected a uv.lock or package-lock.json lock file".into(),
        }),
    }
}

/// Parse `uv.lock` into the canonical extract.
///
/// Read fields per `[[package]]`: `name`, `version`, `source` (registry /
/// directory / editable / path / virtual / git / url), `sdist.hash`, and
/// `wheels.[].hash`. The project root (`editable = "."` / `virtual = "."`)
/// is excluded: the own package is represented by the own commit in the
/// fingerprint, and including its version would couple the extract to the
/// promotion decision.
pub fn extract_uv_lock(content: &str) -> Result<CanonicalExtract, FingerprintError> {
    let doc: toml::Table = content.parse().map_err(|e| FingerprintError::InvalidLock {
        path: "uv.lock".into(),
        reason: format!("failed to parse TOML: {e}"),
    })?;
    let packages = doc
        .get("package")
        .and_then(|p| p.as_array())
        .ok_or_else(|| FingerprintError::InvalidLock {
            path: "uv.lock".into(),
            reason: "no [[package]] entries found".into(),
        })?;

    let mut entries = Vec::new();
    for package in packages {
        let name = package
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FingerprintError::InvalidLock {
                path: "uv.lock".into(),
                reason: "[[package]] without a name".into(),
            })?;
        let version = package
            .get("version")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FingerprintError::InvalidLock {
                path: "uv.lock".into(),
                reason: format!("package `{name}` without a version"),
            })?;
        let source = package.get("source").and_then(|v| v.as_table());
        let origin = format!("uv.lock package `{name}`");

        let (entry_source, digest, path) = match source {
            None => {
                // Bare entries appear for virtual roots of some uv
                // versions; a bare real package would be ambiguous.
                (EntrySource::Path, None, None)
            }
            Some(table) => {
                if table.get("registry").is_some() {
                    let digest = uv_digest(package);
                    (EntrySource::PyPI, digest, None)
                } else if let Some(local) = ["directory", "editable", "path"]
                    .iter()
                    .find_map(|k| table.get(*k).and_then(|v| v.as_str()))
                {
                    match normalize_local_path(local, &origin)? {
                        None => continue, // the own project root
                        Some(p) => {
                            let (source, stable) = classify_local_path(&p);
                            (source, None, Some(stable))
                        }
                    }
                } else if table.contains_key("virtual") {
                    continue; // virtual workspace root — not a dependency
                } else if table.contains_key("git") || table.contains_key("url") {
                    return Err(FingerprintError::ForbiddenLockReference {
                        name: name.to_string(),
                        origin,
                    });
                } else {
                    (EntrySource::Path, None, None)
                }
            }
        };

        entries.push(ExtractEntry {
            name: normalize_python_name(name),
            version: version.to_string(),
            source: entry_source,
            digest,
            path,
        });
    }
    Ok(CanonicalExtract::new(entries))
}

/// The integrity digest of a uv package: the sdist hash when present,
/// otherwise the sorted unique wheel hashes joined with `,`. Both are
/// fixed per released version, so the choice is stable across uv
/// formatting versions.
fn uv_digest(package: &toml::Value) -> Option<String> {
    if let Some(hash) = package
        .get("sdist")
        .and_then(|s| s.get("hash"))
        .and_then(|h| h.as_str())
    {
        return Some(hash.to_string());
    }
    let mut hashes: Vec<String> = package
        .get("wheels")
        .and_then(|w| w.as_array())
        .map(|wheels| {
            wheels
                .iter()
                .filter_map(|w| w.get("hash").and_then(|h| h.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if hashes.is_empty() {
        None
    } else {
        hashes.sort();
        hashes.dedup();
        Some(hashes.join(","))
    }
}

/// Parse `package-lock.json` into the canonical extract.
///
/// `packages` (lockfileVersion 2 and 3) is authoritative when present;
/// a version-1 lock falls back to its `dependencies` tree. The `""` root
/// entry is excluded (the own package), `file:`-injected sources carry
/// their stable `deps/<slug>` coordinate, registry entries carry their
/// integrity string.
pub fn extract_npm_lock(content: &str) -> Result<CanonicalExtract, FingerprintError> {
    let doc: Json = serde_json::from_str(content).map_err(|e| FingerprintError::InvalidLock {
        path: "package-lock.json".into(),
        reason: format!("failed to parse JSON: {e}"),
    })?;
    let packages = doc.get("packages").and_then(|p| p.as_object());
    match packages {
        Some(packages) => extract_npm_packages(packages),
        None => {
            let dependencies = doc.get("dependencies").and_then(|d| d.as_object());
            match dependencies {
                Some(dependencies) => {
                    let mut entries = Vec::new();
                    walk_npm_v1(dependencies, "", &mut entries)?;
                    Ok(CanonicalExtract::new(entries))
                }
                None => Err(FingerprintError::InvalidLock {
                    path: "package-lock.json".into(),
                    reason: "neither `packages` nor `dependencies` found".into(),
                }),
            }
        }
    }
}

/// Read a lockfileVersion 2/3 `packages` map.
fn extract_npm_packages(
    packages: &serde_json::Map<String, Json>,
) -> Result<CanonicalExtract, FingerprintError> {
    let mut entries = Vec::new();
    for (key, value) in packages {
        if key.is_empty() {
            continue; // the root package — represented by the own commit
        }
        let origin = format!("package-lock.json entry `{key}`");

        if key.starts_with("node_modules/") {
            let name = npm_name_from_modules_key(key);
            // Link entries carry no version; the linked target (a `deps/…`
            // key) provides it and yields the same canonical entry, so the
            // link itself contributes nothing new.
            if value.get("link").and_then(|l| l.as_bool()).unwrap_or(false)
                || value.get("version").is_none()
            {
                continue;
            }
            let version = value
                .get("version")
                .and_then(|v| v.as_str())
                .ok_or_else(|| FingerprintError::InvalidLock {
                    path: "package-lock.json".into(),
                    reason: format!("entry `{key}` without a version"),
                })?;
            let (source, digest, path) = classify_npm_resolved(
                value.get("resolved"),
                value.get("integrity"),
                &name,
                &origin,
            )?;
            entries.push(ExtractEntry {
                name,
                version: version.to_string(),
                source,
                digest,
                path,
            });
        } else {
            // Bare path keys: jumbo-injected sources (`deps/<slug>`) and
            // other file locations.
            let name = value
                .get("name")
                .and_then(|n| n.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    key.trim_end_matches('/')
                        .rsplit('/')
                        .next()
                        .unwrap_or(key)
                        .to_string()
                });
            let version = value
                .get("version")
                .and_then(|v| v.as_str())
                .ok_or_else(|| FingerprintError::InvalidLock {
                    path: "package-lock.json".into(),
                    reason: format!("entry `{key}` without a version"),
                })?;
            let normalized = normalize_local_path(key, &origin)?;
            let (source, stable) = normalized
                .map(|p| classify_local_path(&p))
                .unwrap_or((EntrySource::Path, String::new()));
            entries.push(ExtractEntry {
                name,
                version: version.to_string(),
                source,
                digest: None,
                path: if stable.is_empty() {
                    None
                } else {
                    Some(stable)
                },
            });
        }
    }
    Ok(CanonicalExtract::new(entries))
}

/// Walk a lockfileVersion 1 `dependencies` tree.
///
/// v1 nests scoped packages inside a scope container that carries no
/// `version` of its own (`"@juntai" → "demo-kit"`); flat names also occur,
/// so both shapes are handled. Nested `dependencies` of a real entry are
/// separate packages and never inherit the parent's scope prefix.
fn walk_npm_v1(
    dependencies: &serde_json::Map<String, Json>,
    scope_prefix: &str,
    entries: &mut Vec<ExtractEntry>,
) -> Result<(), FingerprintError> {
    for (key, value) in dependencies {
        let object = value.as_object();
        // v1 nests scoped packages inside a scope container that carries
        // no `version` of its own.
        let is_scope_container =
            key.starts_with('@') && object.map(|o| !o.contains_key("version")).unwrap_or(false);
        if is_scope_container {
            let prefix = format!("@{}/", key.trim_start_matches('@'));
            walk_npm_v1(object.unwrap_or(&serde_json::Map::new()), &prefix, entries)?;
            continue;
        }
        let name = format!("{scope_prefix}{key}");
        let origin = format!("package-lock.json entry `{name}`");
        let object = object.ok_or_else(|| FingerprintError::InvalidLock {
            path: "package-lock.json".into(),
            reason: format!("entry `{name}` is not an object"),
        })?;
        let version = object
            .get("version")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FingerprintError::InvalidLock {
                path: "package-lock.json".into(),
                reason: format!("entry `{name}` without a version"),
            })?;
        let (source, digest, path) = classify_npm_resolved(
            object.get("resolved"),
            object.get("integrity"),
            &name,
            &origin,
        )?;
        entries.push(ExtractEntry {
            name,
            version: version.to_string(),
            source,
            digest,
            path,
        });
        if let Some(children) = object.get("dependencies").and_then(|d| d.as_object()) {
            walk_npm_v1(children, "", entries)?;
        }
    }
    Ok(())
}

/// The package name for a `node_modules/…` key: scoped names keep their
/// scope; nested installs take the innermost package.
fn npm_name_from_modules_key(key: &str) -> String {
    match key.rfind("node_modules/") {
        Some(idx) => key[idx + "node_modules/".len()..].to_string(),
        None => key.to_string(),
    }
}

/// Classify a `resolved` (+ `integrity`) pair of an npm entry.
fn classify_npm_resolved(
    resolved: Option<&Json>,
    integrity: Option<&Json>,
    name: &str,
    origin: &str,
) -> Result<(EntrySource, Option<String>, Option<String>), FingerprintError> {
    let digest = integrity.and_then(|i| i.as_str()).map(str::to_string);
    let Some(resolved) = resolved.and_then(|r| r.as_str()) else {
        // Bundled dependencies have no resolved URL; they are registry
        // artifacts of their parent, already covered by its integrity.
        return Ok((EntrySource::Npm, digest, None));
    };
    let lower = resolved.to_ascii_lowercase();
    if lower.starts_with("git+") || lower.starts_with("git:") {
        return Err(FingerprintError::ForbiddenLockReference {
            name: name.to_string(),
            origin: origin.to_string(),
        });
    }
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Ok((EntrySource::Npm, digest, None));
    }
    // file: prefix or a bare relative path — a local source.
    let raw = resolved.strip_prefix("file:").unwrap_or(resolved);
    match normalize_local_path(raw, origin)? {
        None => Ok((EntrySource::Npm, digest, None)),
        Some(p) => {
            let (source, stable) = classify_local_path(&p);
            Ok((source, None, Some(stable)))
        }
    }
}

/// Convenience: the stable relative path jumbo injects an internal source
/// at, for a package name (`deps/<slug>`).
pub fn injected_source_path(name: &str) -> String {
    format!("deps/{}", package_slug(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        name: &str,
        version: &str,
        source: EntrySource,
        digest: Option<&str>,
        path: Option<&str>,
    ) -> ExtractEntry {
        ExtractEntry {
            name: name.to_string(),
            version: version.to_string(),
            source,
            digest: digest.map(str::to_string),
            path: path.map(str::to_string),
        }
    }

    const UV_LOCK_A: &str = r#"
version = 1
requires-python = ">=3.12"

[[package]]
name = "consumer"
version = "0.1.0"
source = { editable = "." }
dependencies = [
    { name = "demo-alpha" },
    { name = "numpy" },
]

[[package]]
name = "numpy"
version = "1.26.4"
source = { registry = "https://pypi.org/simple" }
sdist = { url = "https://files.pythonhosted.org/packages/numpy-1.26.4.tar.gz", hash = "sha256:aaaa1111", size = 15784360 }
wheels = [
    { url = "https://files.pythonhosted.org/packages/numpy-1.26.4-cp312.whl", hash = "sha256:bbbb2222", size = 15795404 },
    { url = "https://files.pythonhosted.org/packages/numpy-1.26.4-cp311.whl", hash = "sha256:cccc3333", size = 15801108 },
]

[[package]]
name = "demo-alpha"
version = "2.4.0"
source = { directory = "deps/demo-alpha" }
"#;

    /// Formatting-only variant of `UV_LOCK_A`: same resolution, different
    /// package order, different array/key formatting, cosmetic extra keys.
    const UV_LOCK_B: &str = r#"
version = 1
requires-python = ">=3.12"
manifest-version = "2"   # cosmetic, newer uv metadata

[[package]]
name = "demo-alpha"
version = "2.4.0"
source = { directory = "deps/demo-alpha" }

[[package]]
name = "consumer"
version = "0.1.0"
source = { editable = "." }

[[package]]
name = "numpy"
version = "1.26.4"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { url = "https://files.pythonhosted.org/packages/numpy-1.26.4-cp311.whl", hash = "sha256:cccc3333", size = 15801108 },
    { url = "https://files.pythonhosted.org/packages/numpy-1.26.4-cp312.whl", hash = "sha256:bbbb2222", size = 15795404 }
]
sdist = { hash = "sha256:aaaa1111", url = "https://files.pythonhosted.org/packages/numpy-1.26.4.tar.gz", size = 15784360 }
"#;

    #[test]
    fn uv_lock_extracts_index_pypi_and_root() {
        let extract = extract_uv_lock(UV_LOCK_A).expect("extract");
        assert_eq!(extract.format, EXTRACT_FORMAT);
        assert_eq!(
            extract.entries,
            vec![
                entry(
                    "demo-alpha",
                    "2.4.0",
                    EntrySource::Index,
                    None,
                    Some("deps/demo-alpha")
                ),
                entry(
                    "numpy",
                    "1.26.4",
                    EntrySource::PyPI,
                    Some("sha256:aaaa1111"),
                    None
                ),
            ]
        );
        // Root project excluded; npm-style names untouched here.
    }

    #[test]
    fn uv_lock_formatting_only_change_yields_identical_extract() {
        let a = extract_uv_lock(UV_LOCK_A).expect("extract a");
        let b = extract_uv_lock(UV_LOCK_B).expect("extract b");
        assert_eq!(a, b);
        assert_eq!(a.canonical_json(), b.canonical_json());
    }

    #[test]
    fn uv_lock_wheel_only_digest_is_sorted_and_stable() {
        let lock = r#"
[[package]]
name = "wheel-only"
version = "0.3.0"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { url = "w1", hash = "sha256:zzzz", size = 1 },
    { url = "w2", hash = "sha256:aaaa", size = 2 },
]
"#;
        let one = extract_uv_lock(lock).expect("extract");
        let shuffled = lock
            .replace("w1", "tmp")
            .replace("w2", "w1")
            .replace("tmp", "w2");
        let two = extract_uv_lock(&shuffled).expect("extract shuffled");
        assert_eq!(
            one.entries[0].digest.as_deref(),
            Some("sha256:aaaa,sha256:zzzz")
        );
        assert_eq!(one, two);
    }

    #[test]
    fn uv_lock_normalizes_names_and_stabilizes_workspace_relative_paths() {
        let lock = r#"
[[package]]
name = "Demo_Alpha"
version = "2.4.0"
source = { directory = "projects/consumer/deps/demo-alpha" }

[[package]]
name = "member"
version = "0.0.1"
source = { directory = "../shared-member" }
"#;
        let extract = extract_uv_lock(lock).expect("extract");
        assert_eq!(
            extract.entries,
            vec![
                entry(
                    "demo-alpha",
                    "2.4.0",
                    EntrySource::Index,
                    None,
                    Some("deps/demo-alpha")
                ),
                entry(
                    "member",
                    "0.0.1",
                    EntrySource::Path,
                    None,
                    Some("../shared-member")
                ),
            ]
        );
    }

    #[test]
    fn uv_lock_rejects_git_and_absolute_paths() {
        let git = r#"
[[package]]
name = "sneaky"
version = "1.0.0"
source = { git = "https://github.com/org/repo?rev=abc" }
"#;
        let err = extract_uv_lock(git).unwrap_err();
        assert!(err.to_string().contains("forbidden"), "got: {err}");

        let absolute = r#"
[[package]]
name = "local-pinned"
version = "1.0.0"
source = { path = "/Users/someone/deps/local-pinned" }
"#;
        let err = extract_uv_lock(absolute).unwrap_err();
        assert!(err.to_string().contains("machine-specific"), "got: {err}");
    }

    const NPM_LOCK_V3: &str = r#"{
  "name": "consumer",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "requires": true,
  "packages": {
    "": { "name": "consumer", "version": "1.0.0", "dependencies": { "@juntai/demo-kit": "^1", "lodash": "^4.17.21" } },
    "node_modules/@juntai/demo-kit": { "resolved": "deps/juntai-demo-kit", "link": true },
    "deps/juntai-demo-kit": { "name": "@juntai/demo-kit", "version": "1.2.0", "extraneous": false },
    "node_modules/lodash": {
      "version": "4.17.21",
      "resolved": "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz",
      "integrity": "sha512-v2yUIIQQAL05+013r3Ju1cHpIkB8Kf3GwPQmZCuUZZFeEhQqXcHA5ivoZ5S2srBIjPlFaaF500LglEuZHAkYuA=="
    }
  }
}"#;

    /// lockfileVersion 2 writing the same resolution: link/target entries
    /// in a different order, plus the legacy `dependencies` mirror that
    /// v2 emits and v3 omits.
    const NPM_LOCK_V2: &str = r#"{
  "name": "consumer",
  "version": "1.0.0",
  "lockfileVersion": 2,
  "requires": true,
  "packages": {
    "node_modules/lodash": {
      "version": "4.17.21",
      "resolved": "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz",
      "integrity": "sha512-v2yUIIQQAL05+013r3Ju1cHpIkB8Kf3GwPQmZCuUZZFeEhQqXcHA5ivoZ5S2srBIjPlFaaF500LglEuZHAkYuA=="
    },
    "deps/juntai-demo-kit": { "name": "@juntai/demo-kit", "version": "1.2.0" },
    "node_modules/@juntai/demo-kit": { "resolved": "deps/juntai-demo-kit", "link": true },
    "": { "name": "consumer", "version": "1.0.0" }
  },
  "dependencies": {
    "@juntai/demo-kit": { "version": "1.2.0", "resolved": "deps/juntai-demo-kit", "link": true },
    "lodash": {
      "version": "4.17.21",
      "resolved": "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz",
      "integrity": "sha512-v2yUIIQQAL05+013r3Ju1cHpIkB8Kf3GwPQmZCuUZZFeEhQqXcHA5ivoZ5S2srBIjPlFaaF500LglEuZHAkYuA=="
    }
  }
}"#;

    #[test]
    fn npm_lock_extracts_link_target_registry_and_root_excluded() {
        let extract = extract_npm_lock(NPM_LOCK_V3).expect("extract");
        assert_eq!(
            extract.entries,
            vec![
                entry(
                    "@juntai/demo-kit",
                    "1.2.0",
                    EntrySource::Index,
                    None,
                    Some("deps/juntai-demo-kit")
                ),
                entry(
                    "lodash",
                    "4.17.21",
                    EntrySource::Npm,
                    Some("sha512-v2yUIIQQAL05+013r3Ju1cHpIkB8Kf3GwPQmZCuUZZFeEhQqXcHA5ivoZ5S2srBIjPlFaaF500LglEuZHAkYuA=="),
                    None
                ),
            ]
        );
    }

    #[test]
    fn npm_lock_v2_and_v3_yield_identical_extracts() {
        let v3 = extract_npm_lock(NPM_LOCK_V3).expect("extract v3");
        let v2 = extract_npm_lock(NPM_LOCK_V2).expect("extract v2");
        assert_eq!(v3, v2);
        assert_eq!(v3.canonical_json(), v2.canonical_json());
    }

    #[test]
    fn npm_lock_v1_tree_is_supported() {
        let v1 = r#"{
  "lockfileVersion": 1,
  "dependencies": {
    "@juntai": { "demo-kit": { "version": "1.2.0", "resolved": "deps/juntai-demo-kit" } },
    "lodash": {
      "version": "4.17.21",
      "resolved": "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz",
      "integrity": "sha512-v2yUIIQQAL05+013r3Ju1cHpIkB8Kf3GwPQmZCuUZZFeEhQqXcHA5ivoZ5S2srBIjPlFaaF500LglEuZHAkYuA=="
    }
  }
}"#;
        let extract = extract_npm_lock(v1).expect("extract");
        assert_eq!(
            extract.entries,
            vec![
                entry("@juntai/demo-kit", "1.2.0", EntrySource::Index, None, Some("deps/juntai-demo-kit")),
                entry("lodash", "4.17.21", EntrySource::Npm, Some("sha512-v2yUIIQQAL05+013r3Ju1cHpIkB8Kf3GwPQmZCuUZZFeEhQqXcHA5ivoZ5S2srBIjPlFaaF500LglEuZHAkYuA=="), None),
            ]
        );
    }

    #[test]
    fn npm_lock_rejects_git_resolved() {
        let git = r#"{
  "lockfileVersion": 3,
  "packages": {
    "node_modules/sneaky": { "version": "1.0.0", "resolved": "git+https://github.com/org/repo.git#abc" }
  }
}"#;
        let err = extract_npm_lock(git).unwrap_err();
        assert!(err.to_string().contains("forbidden"), "got: {err}");
    }

    #[test]
    fn canonical_json_is_compact_sorted_and_stable() {
        let a = CanonicalExtract::new(vec![
            entry("z-pkg", "1.0.0", EntrySource::Npm, None, None),
            entry("a-pkg", "1.0.0", EntrySource::Npm, None, None),
            entry("a-pkg", "1.0.0", EntrySource::Npm, None, None),
        ]);
        assert_eq!(a.entries.len(), 2); // deduplicated
        assert_eq!(
            a.canonical_json(),
            r#"{"format":"jumbo-canonical-extract/1","entries":[{"name":"a-pkg","version":"1.0.0","source":"npm","digest":null,"path":null},{"name":"z-pkg","version":"1.0.0","source":"npm","digest":null,"path":null}]}"#
        );
        // Byte-for-byte reproducible.
        assert_eq!(
            a.canonical_json(),
            CanonicalExtract::new(vec![
                entry("a-pkg", "1.0.0", EntrySource::Npm, None, None),
                entry("z-pkg", "1.0.0", EntrySource::Npm, None, None),
            ])
            .canonical_json()
        );
    }

    #[test]
    fn fingerprint_preimage_is_commit_plus_canonical_extract() {
        let extract = CanonicalExtract::new(vec![entry(
            "demo-alpha",
            "2.4.0",
            EntrySource::Index,
            None,
            Some("deps/demo-alpha"),
        )]);
        let fp = compute_fingerprint("0123456789abcdef0123456789abcdef01234567", &extract)
            .expect("fingerprint");
        // Independently computed expectation of sha256("<commit>\n<json>").
        let mut hasher = Sha256::new();
        hasher.update(b"0123456789abcdef0123456789abcdef01234567\n");
        hasher.update(extract.canonical_json().as_bytes());
        let expected: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(fp, expected);
        assert_eq!(fp.len(), 64);
    }

    #[test]
    fn fingerprint_stability_and_sensitivity() {
        let extract = CanonicalExtract::new(vec![entry(
            "demo-alpha",
            "2.4.0",
            EntrySource::Index,
            None,
            Some("deps/demo-alpha"),
        )]);
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let one = compute_fingerprint(commit, &extract).expect("fp");
        // Stable: same inputs, same value.
        assert_eq!(one, compute_fingerprint(commit, &extract).unwrap());
        // A formatting-only change (entry order) changes nothing.
        let reordered = CanonicalExtract::new(extract.entries.clone().into_iter().rev().collect());
        assert_eq!(one, compute_fingerprint(commit, &reordered).unwrap());
        // A real resolution change changes the fingerprint.
        let bumped = CanonicalExtract::new(vec![entry(
            "demo-alpha",
            "2.5.0",
            EntrySource::Index,
            None,
            Some("deps/demo-alpha"),
        )]);
        assert_ne!(one, compute_fingerprint(commit, &bumped).unwrap());
        // A different own commit changes the fingerprint.
        assert_ne!(
            one,
            compute_fingerprint("ffffffffffffffffffffffffffffffffffffffff", &extract).unwrap()
        );
        // Uppercase or malformed commits are rejected.
        assert!(
            compute_fingerprint("ABCDEF0123456789abcdef0123456789abcdef012", &extract).is_err()
        );
        assert!(compute_fingerprint("short", &extract).is_err());
    }

    #[test]
    fn injected_source_path_uses_stable_slug() {
        assert_eq!(injected_source_path("demo-alpha"), "deps/demo-alpha");
        assert_eq!(
            injected_source_path("@juntai/demo-kit"),
            "deps/juntai-demo-kit"
        );
    }
}
