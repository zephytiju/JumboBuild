//! Lock generation: materialize internal dependencies as jumbo-injected
//! sources at stable relative paths, then hand the manifest to the normal
//! language lock tool (Jumbo Build & Versioning Standard, §2.3).
//!
//! For one project (manifest directory) jumbo:
//! 1. resolves the manifest against the index (resolver core);
//! 2. writes every internal dependency as a minimal source project under
//!    `deps/<slug>/` carrying the index record's `name` and `version`
//!    (the commit is recorded in the marker for provenance);
//! 3. rewrites the manifest so internal declarations point at the injected
//!    sources — Python: `name[extras]==<version>` plus a
//!    `[tool.uv.sources]` path entry; npm: `"file:deps/<slug>"`;
//! 4. runs `uv lock --upgrade` / `npm install --package-lock-only` to
//!    produce `uv.lock` / `package-lock.json`.
//!
//! The rewrite is recorded in `deps/.jumbo-sources.json` and is fully
//! reversible and idempotent: every generation first restores the previous
//! injection, so `jumbo lock` twice in a row produces identical files.
//! Downloading release artifacts instead of materializing source overlays
//! is the materializer's scope, not this module's.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::FingerprintError;
use super::extract::injected_source_path;
use crate::resolver::manifest::Ecosystem;
use crate::resolver::{resolve_manifest, Index, ResolvedDependency};

/// Marker file recording the current injection (below `deps/`).
pub const MARKER_FILE: &str = ".jumbo-sources.json";
/// Injection marker format identifier.
pub const MARKER_FORMAT: &str = "jumbo-lock-injection/1";
/// Directory every injected source lives under, relative to the manifest.
pub const INJECTED_DIR: &str = "deps";
/// Generated lock file names (also the promotion-guard exemptions).
pub const LOCK_FILE_NAMES: [&str; 2] = ["uv.lock", "package-lock.json"];

/// One jumbo-injected internal source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InjectedSource {
    /// Language-native package name.
    pub name: String,
    /// Stable relative path (`deps/<slug>`).
    pub path: String,
    /// Resolved version from the index record.
    pub version: String,
    /// Index-record commit (provenance; not parsed by the extract).
    pub commit: String,
    /// Original declaration string, for restoring the manifest.
    pub declared: String,
    /// Manifest section the declaration was found in.
    pub location: String,
    /// Python extras (empty for npm).
    #[serde(default)]
    pub extras: Vec<String>,
    /// The exact dependency string written into the manifest.
    pub rewritten: String,
}

/// The on-disk injection marker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectionMarker {
    pub format: String,
    pub sources: Vec<InjectedSource>,
}

/// Result of generating the lock inputs for one manifest.
#[derive(Debug)]
pub struct LockGeneration {
    /// The manifest the lock is generated for.
    pub manifest: PathBuf,
    pub ecosystem: Ecosystem,
    /// Where the generated lock lands (`uv.lock` / `package-lock.json`).
    pub lock_path: PathBuf,
    /// The injected internal sources, sorted by path.
    pub injected: Vec<InjectedSource>,
    /// The resolved internal dependencies the injection was built from
    /// (the resolver ran against the declared forms, before rewriting).
    pub internal: Vec<ResolvedDependency>,
}

impl LockGeneration {
    /// The lock command under the third-party refresh policy
    /// (Jumbo Build & Versioning Standard, §2.3): `upgrade` re-resolves
    /// declared ranges (`uv lock --upgrade`); `upgrade == false` keeps the
    /// current resolution — `uv lock` without `--upgrade` reuses the
    /// existing pins (npm's lock-only install already prefers existing
    /// pins within the declared ranges whenever a lock exists, so its
    /// command is unchanged).
    pub fn lock_command_for(&self, upgrade: bool) -> (&'static str, &'static str) {
        match (self.ecosystem, upgrade) {
            (Ecosystem::Python, true) => (
                "uv lock --upgrade",
                "Generating uv.lock (third-party ranges re-resolved)",
            ),
            (Ecosystem::Python, false) => (
                "uv lock",
                "Generating uv.lock (third-party ranges reused per the refresh policy)",
            ),
            (Ecosystem::Npm, _) => (
                "npm install --package-lock-only --ignore-scripts",
                "Generating package-lock.json (third-party ranges per the refresh policy)",
            ),
        }
    }
}

/// Load the current injection marker, if one exists.
pub fn load_marker(project_dir: &Path) -> Result<Option<InjectionMarker>, FingerprintError> {
    let path = project_dir.join(INJECTED_DIR).join(MARKER_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path).map_err(|e| FingerprintError::InvalidMarker {
        path: path.display().to_string(),
        reason: format!("failed to read: {e}"),
    })?;
    let marker: InjectionMarker =
        serde_json::from_str(&content).map_err(|e| FingerprintError::InvalidMarker {
            path: path.display().to_string(),
            reason: format!("failed to parse: {e}"),
        })?;
    if marker.format != MARKER_FORMAT {
        return Err(FingerprintError::InvalidMarker {
            path: path.display().to_string(),
            reason: format!("unknown format `{}`", marker.format),
        });
    }
    Ok(Some(marker))
}

/// Generate the lock inputs for one manifest: inject internal sources at
/// stable relative paths and rewrite the manifest. Does not run the
/// language lock tool — the CLI layer does (see [`LockGeneration::lock_command`]).
///
/// Idempotent: any previous injection is restored before the manifest is
/// resolved, so two runs in a row produce identical files and the resolver
/// always sees the declared (major-only) forms, never the rewritten ones.
pub fn generate_lock_inputs(
    manifest: &Path,
    index: &Index,
) -> Result<LockGeneration, FingerprintError> {
    // `load_manifest` validates the manifest kind; recover the enum from
    // the file name (its own convention).
    let ecosystem = match manifest.file_name().and_then(|n| n.to_str()) {
        Some("pyproject.toml") => Ecosystem::Python,
        _ => Ecosystem::Npm,
    };
    let project_dir = manifest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let lock_name = match ecosystem {
        Ecosystem::Python => "uv.lock",
        Ecosystem::Npm => "package-lock.json",
    };

    // Restore any previous injection before resolving, so the resolver
    // validates the declared forms rather than the rewritten ones.
    let raw = std::fs::read_to_string(manifest).map_err(|e| FingerprintError::LockGeneration {
        manifest: manifest.display().to_string(),
        reason: format!("failed to read: {e}"),
    })?;
    let previous = load_marker(&project_dir)?;
    let restored = match (&previous, ecosystem) {
        (Some(marker), Ecosystem::Python) => restore_python_manifest(&raw, &marker.sources),
        (Some(marker), Ecosystem::Npm) => restore_npm_manifest(&raw, &marker.sources),
        _ => raw.clone(),
    };
    if restored != raw {
        std::fs::write(manifest, &restored).map_err(|e| FingerprintError::LockGeneration {
            manifest: manifest.display().to_string(),
            reason: format!("failed to restore the manifest before injection: {e}"),
        })?;
    }

    let resolution =
        resolve_manifest(manifest, index).map_err(|e| FingerprintError::LockGeneration {
            manifest: manifest.display().to_string(),
            reason: e.to_string(),
        })?;

    // Build the new injection from the resolution.
    let mut injected: Vec<InjectedSource> =
        resolution.internal.iter().map(injected_source).collect();
    injected.sort_by(|a, b| a.path.cmp(&b.path));

    let rewritten = match ecosystem {
        Ecosystem::Python => apply_python_injection(&restored, &injected)?,
        Ecosystem::Npm => apply_npm_injection(&restored, &injected)?,
    };

    // Materialize the injected source projects.
    let deps_dir = project_dir.join(INJECTED_DIR);
    std::fs::create_dir_all(&deps_dir).map_err(|e| FingerprintError::LockGeneration {
        manifest: manifest.display().to_string(),
        reason: format!("failed to create {}: {e}", deps_dir.display()),
    })?;
    for source in &injected {
        write_injected_source_project(&project_dir, source, ecosystem)?;
    }
    let marker = InjectionMarker {
        format: MARKER_FORMAT.to_string(),
        sources: injected.clone(),
    };
    let marker_path = deps_dir.join(MARKER_FILE);
    std::fs::write(
        &marker_path,
        serde_json::to_string_pretty(&marker).map_err(|e| FingerprintError::LockGeneration {
            manifest: manifest.display().to_string(),
            reason: format!("failed to serialize marker: {e}"),
        })? + "\n",
    )
    .map_err(|e| FingerprintError::LockGeneration {
        manifest: manifest.display().to_string(),
        reason: format!("failed to write {}: {e}", marker_path.display()),
    })?;

    // Write the rewritten manifest last: on disk, marker + sources always
    // precede the rewrite that references them.
    std::fs::write(manifest, &rewritten).map_err(|e| FingerprintError::LockGeneration {
        manifest: manifest.display().to_string(),
        reason: format!("failed to write rewritten manifest: {e}"),
    })?;

    Ok(LockGeneration {
        manifest: manifest.to_path_buf(),
        ecosystem,
        lock_path: project_dir.join(lock_name),
        injected,
        internal: resolution.internal,
    })
}

/// The injected-source record for one resolved internal dependency.
fn injected_source(dep: &ResolvedDependency) -> InjectedSource {
    let mut extras = dep.extras.clone();
    extras.sort();
    extras.dedup();
    let rewritten = python_rewritten_declaration(&dep.name, &dep.record.version, &extras);
    InjectedSource {
        name: dep.name.clone(),
        path: injected_source_path(&dep.name),
        version: dep.record.version.clone(),
        commit: dep.record.commit.clone(),
        declared: dep.declaration.clone(),
        location: dep.location.clone(),
        extras,
        rewritten,
    }
}

/// `name[a,b]==<version>`: the exact-pin + path-source form uv resolves.
/// Extras are normalized to sorted order so the rewrite is canonical.
fn python_rewritten_declaration(name: &str, version: &str, extras: &[String]) -> String {
    if extras.is_empty() {
        format!("{name}=={version}")
    } else {
        format!("{name}[{}]=={version}", extras.join(","))
    }
}

/// Apply the injection to a `pyproject.toml`: exact-pin every internal
/// declaration and add `[tool.uv.sources] <name> = { path = "deps/<slug>" }`.
fn apply_python_injection(
    content: &str,
    sources: &[InjectedSource],
) -> Result<String, FingerprintError> {
    let mut doc: toml::Table = content
        .parse()
        .map_err(|e| FingerprintError::LockGeneration {
            manifest: "pyproject.toml".into(),
            reason: format!("failed to parse TOML: {e}"),
        })?;

    for source in sources {
        rewrite_python_dependency_strings(&mut doc, source);
    }

    // [tool.uv.sources] — path entries for the injected sources.
    let tool = doc
        .entry("tool")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let tool_table = tool
        .as_table_mut()
        .ok_or_else(|| FingerprintError::LockGeneration {
            manifest: "pyproject.toml".into(),
            reason: "[tool] is not a table".into(),
        })?;
    let uv = tool_table
        .entry("uv")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let uv_table = uv
        .as_table_mut()
        .ok_or_else(|| FingerprintError::LockGeneration {
            manifest: "pyproject.toml".into(),
            reason: "[tool.uv] is not a table".into(),
        })?;
    let mut sources_table = uv_table
        .get("sources")
        .and_then(|v| v.as_table())
        .cloned()
        .unwrap_or_default();
    for source in sources {
        let mut entry = toml::Table::new();
        entry.insert("path".to_string(), toml::Value::String(source.path.clone()));
        sources_table.insert(source.name.clone(), toml::Value::Table(entry));
    }
    uv_table.insert("sources".to_string(), toml::Value::Table(sources_table));

    // [tool.jumbo] lock_sources — the same managed-entry convention the
    // workspace sync uses, so only jumbo-owned entries are touched.
    let jumbo = tool_table
        .entry("jumbo")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let jumbo_table = jumbo
        .as_table_mut()
        .ok_or_else(|| FingerprintError::LockGeneration {
            manifest: "pyproject.toml".into(),
            reason: "[tool.jumbo] is not a table".into(),
        })?;
    jumbo_table.insert(
        "lock_sources".to_string(),
        toml::Value::Array(
            sources
                .iter()
                .map(|s| toml::Value::String(s.name.clone()))
                .collect(),
        ),
    );

    toml::to_string_pretty(&doc).map_err(|e| FingerprintError::LockGeneration {
        manifest: "pyproject.toml".into(),
        reason: format!("failed to serialize: {e}"),
    })
}

/// Replace the declared string of `source` with its rewritten exact-pin in
/// every dependency list uv resolves: `[project].dependencies`,
/// `[project].optional-dependencies.*`, and `[dependency-groups].*`.
fn rewrite_python_dependency_strings(doc: &mut toml::Table, source: &InjectedSource) {
    let rewrite = |dep: &mut toml::Value| {
        if let Some(req) = dep.as_str() {
            if req.trim() == source.declared {
                *dep = toml::Value::String(source.rewritten.clone());
            }
        }
    };
    if let Some(deps) = doc
        .get_mut("project")
        .and_then(|p| p.as_table_mut())
        .and_then(|p| p.get_mut("dependencies"))
        .and_then(|d| d.as_array_mut())
    {
        for dep in deps.iter_mut() {
            rewrite(dep);
        }
    }
    if let Some(extras) = doc
        .get_mut("project")
        .and_then(|p| p.as_table_mut())
        .and_then(|p| p.get_mut("optional-dependencies"))
        .and_then(|d| d.as_table_mut())
    {
        for (_, list) in extras.iter_mut() {
            if let Some(deps) = list.as_array_mut() {
                for dep in deps.iter_mut() {
                    rewrite(dep);
                }
            }
        }
    }
    if let Some(groups) = doc
        .get_mut("dependency-groups")
        .and_then(|d| d.as_table_mut())
    {
        for (_, list) in groups.iter_mut() {
            if let Some(deps) = list.as_array_mut() {
                for dep in deps.iter_mut() {
                    rewrite(dep);
                }
            }
        }
    }
}

/// Reverse a previous python injection: restore declared strings, remove
/// the jumbo-managed `[tool.uv.sources]` entries and the marker list, and
/// re-serialize. Value-level and idempotent — applying it to an already
/// restored manifest is a no-op, which is what the promotion guard relies
/// on to compare the working tree against HEAD.
pub fn restore_python_manifest(content: &str, sources: &[InjectedSource]) -> String {
    let Ok(mut doc) = content.parse::<toml::Table>() else {
        return content.to_string();
    };
    for source in sources {
        restore_python_dependency_strings(&mut doc, source);
    }
    let managed: Vec<String> = doc
        .get("tool")
        .and_then(|t| t.get("jumbo"))
        .and_then(|j| j.get("lock_sources"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for name in &managed {
        if let Some(sources) = doc
            .get_mut("tool")
            .and_then(|t| t.as_table_mut())
            .and_then(|t| t.get_mut("uv"))
            .and_then(|u| u.as_table_mut())
            .and_then(|u| u.get_mut("sources"))
            .and_then(|s| s.as_table_mut())
        {
            sources.remove(name);
        }
    }
    if let Some(jumbo) = doc
        .get_mut("tool")
        .and_then(|t| t.as_table_mut())
        .and_then(|t| t.get_mut("jumbo"))
        .and_then(|j| j.as_table_mut())
    {
        jumbo.remove("lock_sources");
    }
    prune_empty_tables(&mut doc);
    toml::to_string_pretty(&doc).unwrap_or_else(|_| content.to_string())
}

/// Replace the rewritten exact-pin of `source` with its original declared
/// string in every dependency list uv resolves. Inverse of
/// [`rewrite_python_dependency_strings`].
fn restore_python_dependency_strings(doc: &mut toml::Table, source: &InjectedSource) {
    let rewrite = |dep: &mut toml::Value| {
        if let Some(req) = dep.as_str() {
            if req.trim() == source.rewritten {
                *dep = toml::Value::String(source.declared.clone());
            }
        }
    };
    if let Some(deps) = doc
        .get_mut("project")
        .and_then(|p| p.as_table_mut())
        .and_then(|p| p.get_mut("dependencies"))
        .and_then(|d| d.as_array_mut())
    {
        for dep in deps.iter_mut() {
            rewrite(dep);
        }
    }
    if let Some(extras) = doc
        .get_mut("project")
        .and_then(|p| p.as_table_mut())
        .and_then(|p| p.get_mut("optional-dependencies"))
        .and_then(|d| d.as_table_mut())
    {
        for (_, list) in extras.iter_mut() {
            if let Some(deps) = list.as_array_mut() {
                for dep in deps.iter_mut() {
                    rewrite(dep);
                }
            }
        }
    }
    if let Some(groups) = doc
        .get_mut("dependency-groups")
        .and_then(|d| d.as_table_mut())
    {
        for (_, list) in groups.iter_mut() {
            if let Some(deps) = list.as_array_mut() {
                for dep in deps.iter_mut() {
                    rewrite(dep);
                }
            }
        }
    }
}

/// Remove empty nested tables under `tool` (cosmetic restoration).
fn prune_empty_tables(doc: &mut toml::Table) {
    if let Some(tool) = doc.get_mut("tool").and_then(|t| t.as_table_mut()) {
        if let Some(uv) = tool.get_mut("uv").and_then(|u| u.as_table_mut()) {
            let empty_sources = uv
                .get("sources")
                .map(|s| s.as_table().map(|t| t.is_empty()).unwrap_or(false))
                .unwrap_or(false);
            if empty_sources {
                uv.remove("sources");
            }
            if uv.is_empty() {
                tool.remove("uv");
            }
        }
        if let Some(jumbo) = tool.get_mut("jumbo").and_then(|j| j.as_table_mut()) {
            if jumbo.is_empty() {
                tool.remove("jumbo");
            }
        }
        if tool.is_empty() {
            doc.remove("tool");
        }
    }
}

/// Apply the injection to a `package.json`: point internal declarations at
/// the injected sources with the `file:` protocol.
fn apply_npm_injection(
    content: &str,
    sources: &[InjectedSource],
) -> Result<String, FingerprintError> {
    let mut doc: serde_json::Value =
        serde_json::from_str(content).map_err(|e| FingerprintError::LockGeneration {
            manifest: "package.json".into(),
            reason: format!("failed to parse JSON: {e}"),
        })?;
    let by_name: std::collections::BTreeMap<&str, &InjectedSource> =
        sources.iter().map(|s| (s.name.as_str(), s)).collect();
    for section in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        let Some(map) = doc.get_mut(section).and_then(|v| v.as_object_mut()) else {
            continue;
        };
        for (name, value) in map.iter_mut() {
            if let Some(source) = by_name.get(name.as_str()) {
                if value.as_str() == Some(source.declared.as_str()) {
                    *value = serde_json::Value::String(format!("file:{}", source.path));
                }
            }
        }
    }
    serde_json::to_string_pretty(&doc)
        .map(|s| s + "\n")
        .map_err(|e| FingerprintError::LockGeneration {
            manifest: "package.json".into(),
            reason: format!("failed to serialize: {e}"),
        })
}

/// Reverse a previous npm injection: restore the declared ranges and
/// re-serialize. Value-level and idempotent, so the promotion guard can
/// normalize both the working-tree and HEAD copies through it.
///
/// Both jumbo-written forms are restored: the injected source overlay
/// (`file:deps/<slug>`) and a materialized record artifact
/// (`file:deps/<slug>/<file>.tgz`) — anything under the package's overlay
/// coordinate is jumbo-owned and reverts to the declared range.
pub fn restore_npm_manifest(content: &str, sources: &[InjectedSource]) -> String {
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(content) else {
        return content.to_string();
    };
    let by_name: std::collections::BTreeMap<&str, &InjectedSource> =
        sources.iter().map(|s| (s.name.as_str(), s)).collect();
    for section in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        let Some(map) = doc.get_mut(section).and_then(|v| v.as_object_mut()) else {
            continue;
        };
        for (name, value) in map.iter_mut() {
            if let Some(source) = by_name.get(name.as_str()) {
                if let Some(current) = value.as_str() {
                    let overlay_source = format!("file:{}", source.path);
                    let overlay_under = format!("file:{}/", source.path.trim_end_matches('/'));
                    if current == overlay_source || current.starts_with(&overlay_under) {
                        *value = serde_json::Value::String(source.declared.clone());
                    }
                }
            }
        }
    }
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| content.to_string())
}

/// Write the minimal source project for one injected internal dependency:
/// a real, buildable project carrying the index record's name and version.
fn write_injected_source_project(
    project_dir: &Path,
    source: &InjectedSource,
    ecosystem: Ecosystem,
) -> Result<(), FingerprintError> {
    let dir = project_dir.join(&source.path);
    std::fs::create_dir_all(&dir).map_err(|e| FingerprintError::LockGeneration {
        manifest: project_dir.display().to_string(),
        reason: format!("failed to create {}: {e}", dir.display()),
    })?;
    match ecosystem {
        Ecosystem::Python => {
            let extras = if source.extras.is_empty() {
                String::new()
            } else {
                let mut table = String::from("\n[project.optional-dependencies]\n");
                for extra in &source.extras {
                    table.push_str(&format!("{extra} = []\n"));
                }
                table
            };
            let content = format!(
                "# jumbo-injected internal source (index record {commit}).\n# Regenerated by `jumbo lock`; do not edit.\n\
                 [build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n\n\
                 [project]\nname = \"{name}\"\nversion = \"{version}\"{extras}",
                commit = source.commit,
                name = source.name,
                version = source.version,
                extras = extras,
            );
            std::fs::write(dir.join("pyproject.toml"), content).map_err(|e| {
                FingerprintError::LockGeneration {
                    manifest: project_dir.display().to_string(),
                    reason: format!(
                        "failed to write {}: {e}",
                        dir.join("pyproject.toml").display()
                    ),
                }
            })
        }
        Ecosystem::Npm => {
            let content = format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \
                 \"description\": \"jumbo-injected internal source (index record {commit}); regenerated by jumbo lock; do not edit\"\n}}\n",
                name = source.name,
                version = source.version,
                commit = source.commit,
            );
            std::fs::write(dir.join("package.json"), content).map_err(|e| {
                FingerprintError::LockGeneration {
                    manifest: project_dir.display().to_string(),
                    reason: format!(
                        "failed to write {}: {e}",
                        dir.join("package.json").display()
                    ),
                }
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::index::{Index, IndexRecord, IndexSource};

    fn record(package: &str, major: u64, version: &str, commit: &str) -> IndexRecord {
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
            executor: Some("bootstrap".to_string()),
            timestamp: "2026-09-01T00:00:00Z".to_string(),
        }
    }

    fn fixture_index(tag: &str) -> (std::path::PathBuf, Index) {
        let dir = std::env::temp_dir().join(format!(
            "jumbo-lockgen-ut-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let index_dir = dir.join("index");
        std::fs::create_dir_all(&index_dir).expect("create index dir");
        std::fs::write(
            index_dir.join("demo-alpha.jsonl"),
            serde_json::to_string(&record(
                "demo-alpha",
                2,
                "2.4.0",
                "0123456789abcdef0123456789abcdef01234567",
            ))
            .unwrap()
                + "\n",
        )
        .expect("write demo-alpha");
        std::fs::write(
            index_dir.join("juntai-demo-kit.jsonl"),
            serde_json::to_string(&record(
                "@juntai/demo-kit",
                1,
                "1.2.0",
                "fedcba9876543210fedcba9876543210fedcba98",
            ))
            .unwrap()
                + "\n",
        )
        .expect("write juntai-demo-kit");
        let index = Index::load(&IndexSource::Local(index_dir.clone())).expect("load index");
        (dir, index)
    }

    fn temp_project(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jumbo-lockgen-proj-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create project dir");
        dir
    }

    #[test]
    fn python_injection_uses_stable_relative_paths() {
        let (_index_dir, index) = fixture_index("py-stable");
        let project = temp_project("py-stable");
        let manifest = project.join("pyproject.toml");
        std::fs::write(
            &manifest,
            "[project]\nname = \"consumer\"\ndependencies = [\n    \"demo-alpha[http]@2\",\n    \"numpy>=1.26\",\n]\n",
        )
        .expect("write manifest");

        let generation = generate_lock_inputs(&manifest, &index).expect("generate");
        assert_eq!(generation.injected.len(), 1);
        let injected = &generation.injected[0];
        assert_eq!(injected.name, "demo-alpha");
        assert_eq!(injected.path, "deps/demo-alpha");
        assert_eq!(injected.version, "2.4.0");
        assert_eq!(injected.commit, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(injected.rewritten, "demo-alpha[http]==2.4.0");
        assert_eq!(generation.lock_path, project.join("uv.lock"));

        // The injected source project exists with the record identity.
        let injected_manifest = project.join("deps/demo-alpha/pyproject.toml");
        let content = std::fs::read_to_string(&injected_manifest).expect("read injected");
        assert!(content.contains("name = \"demo-alpha\""));
        assert!(content.contains("version = \"2.4.0\""));
        assert!(content.contains("0123456789abcdef0123456789abcdef01234567"));

        // The marker records the injection.
        let marker: InjectionMarker = serde_json::from_str(
            &std::fs::read_to_string(project.join("deps/.jumbo-sources.json"))
                .expect("read marker"),
        )
        .expect("parse marker");
        assert_eq!(marker.format, MARKER_FORMAT);
        assert_eq!(marker.sources.len(), 1);
        assert_eq!(marker.sources[0].declared, "demo-alpha[http]@2");

        // The manifest was rewritten: exact pin + path source.
        let rewritten = std::fs::read_to_string(&manifest).expect("read rewritten");
        let doc: toml::Table = rewritten.parse().expect("parse rewritten");
        let deps: Vec<String> = doc["project"]["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|d| d.as_str().map(str::to_string))
            .collect();
        assert!(deps.contains(&"demo-alpha[http]==2.4.0".to_string()));
        assert!(deps.contains(&"numpy>=1.26".to_string()));
        assert!(!deps.contains(&"demo-alpha[http]@2".to_string()));
        assert_eq!(
            doc["tool"]["uv"]["sources"]["demo-alpha"]["path"]
                .as_str()
                .unwrap(),
            "deps/demo-alpha"
        );
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn python_injection_is_idempotent_and_restorable() {
        let (_index_dir, index) = fixture_index("py-idem");
        let project = temp_project("py-idem");
        let manifest = project.join("pyproject.toml");
        let original =
            "[project]\nname = \"consumer\"\ndependencies = [\"demo-alpha@2\"]\n[tool.ruff]\nline-length = 100\n";
        std::fs::write(&manifest, original).expect("write manifest");

        let first = generate_lock_inputs(&manifest, &index).expect("first");
        let once = std::fs::read_to_string(&manifest).expect("read once");
        let second = generate_lock_inputs(&manifest, &index).expect("second");
        let twice = std::fs::read_to_string(&manifest).expect("read twice");
        assert_eq!(once, twice, "second generation must be byte-identical");
        assert_eq!(first.injected, second.injected);

        // Restoring reverses the injection at the value level and keeps
        // unrelated user configuration ([tool.ruff]).
        let restored = restore_python_manifest(&twice, &second.injected);
        let doc: toml::Table = restored.parse().expect("parse restored");
        let deps: Vec<String> = doc["project"]["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|d| d.as_str().map(str::to_string))
            .collect();
        assert_eq!(deps, vec!["demo-alpha@2"]);
        assert!(doc
            .get("tool")
            .unwrap()
            .get("ruff")
            .unwrap()
            .get("line-length")
            .is_some());
        assert!(doc
            .get("tool")
            .and_then(|t| t.get("uv"))
            .and_then(|u| u.get("sources"))
            .is_none());
        // Restoring an already-restored manifest is a no-op.
        assert_eq!(
            restore_python_manifest(&restored, &second.injected),
            restored
        );
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn npm_injection_uses_file_protocol_at_stable_paths() {
        let (_index_dir, index) = fixture_index("npm-stable");
        let project = temp_project("npm-stable");
        let manifest = project.join("package.json");
        std::fs::write(
            &manifest,
            r#"{
  "name": "consumer",
  "dependencies": {
    "@juntai/demo-kit": "^1",
    "lodash": "^4.17.21"
  }
}
"#,
        )
        .expect("write manifest");

        let generation = generate_lock_inputs(&manifest, &index).expect("generate");
        assert_eq!(generation.injected.len(), 1);
        assert_eq!(generation.injected[0].name, "@juntai/demo-kit");
        assert_eq!(generation.injected[0].path, "deps/juntai-demo-kit");
        assert_eq!(generation.lock_path, project.join("package-lock.json"));

        let injected_package = project.join("deps/juntai-demo-kit/package.json");
        let content = std::fs::read_to_string(&injected_package).expect("read injected");
        assert!(content.contains("\"name\": \"@juntai/demo-kit\""));
        assert!(content.contains("\"version\": \"1.2.0\""));
        assert!(content.contains("fedcba9876543210fedcba9876543210fedcba98"));

        let rewritten: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(
            rewritten["dependencies"]["@juntai/demo-kit"]
                .as_str()
                .unwrap(),
            "file:deps/juntai-demo-kit"
        );
        assert_eq!(
            rewritten["dependencies"]["lodash"].as_str().unwrap(),
            "^4.17.21"
        );

        // Idempotent.
        generate_lock_inputs(&manifest, &index).expect("second");
        let twice: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(rewritten, twice);

        // Restorable.
        let restored = restore_npm_manifest(
            &std::fs::read_to_string(&manifest).unwrap(),
            &generation.injected,
        );
        let restored_doc: serde_json::Value = serde_json::from_str(&restored).unwrap();
        assert_eq!(
            restored_doc["dependencies"]["@juntai/demo-kit"]
                .as_str()
                .unwrap(),
            "^1"
        );

        // A materialized record artifact (`file:deps/<slug>/<file>.tgz`,
        // written by the dedup materializer) restores to the declared
        // range as well — anything under the overlay coordinate is
        // jumbo-owned.
        let materialized = restored.replace(
            r#""@juntai/demo-kit": "^1""#,
            r#""@juntai/demo-kit": "file:deps/juntai-demo-kit/juntai-demo-kit-1.2.0.tgz""#,
        );
        let restored2 = restore_npm_manifest(&materialized, &generation.injected);
        let restored2_doc: serde_json::Value = serde_json::from_str(&restored2).unwrap();
        assert_eq!(
            restored2_doc["dependencies"]["@juntai/demo-kit"]
                .as_str()
                .unwrap(),
            "^1"
        );
        let _ = std::fs::remove_dir_all(&project);
    }
}
