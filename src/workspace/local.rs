//! Developer builds prefer compatible, registered local packages. Remote
//! coordinates and repository folder names never determine package identity.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::metadata::JumboToml;
use crate::fingerprint::lockgen::{load_marker, restore_npm_manifest, restore_python_manifest};
use crate::language::{get_registry, LanguageSupport};
use crate::resolver::declaration::{Declaration, Spec};
use crate::resolver::index::normalize_python_name;
use crate::resolver::manifest::{load_manifest_content, Ecosystem};

pub const LOCAL_INPUTS_MARKER: &str = ".jumbo-workspace-inputs.json";

const NODE_SECTIONS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];

#[derive(Debug)]
pub struct LocalProject {
    pub path: PathBuf,
    pub manifest: PathBuf,
    pub ecosystem: Ecosystem,
    pub name: String,
    version: Option<String>,
    declared: String,
    dependencies: Vec<Declaration>,
}

impl LocalProject {
    fn load(path: PathBuf) -> Result<Option<Self>> {
        let (ecosystem, manifest) = if path.join("pyproject.toml").is_file() {
            (Ecosystem::Python, path.join("pyproject.toml"))
        } else if path.join("package.json").is_file() {
            (Ecosystem::Npm, path.join("package.json"))
        } else {
            return Ok(None);
        };
        let raw = std::fs::read_to_string(&manifest)?;
        let declared = match load_marker(&path)? {
            Some(marker) => match ecosystem {
                Ecosystem::Python => restore_python_manifest(&raw, &marker.sources),
                Ecosystem::Npm => restore_npm_manifest(&raw, &marker.sources),
            },
            None => raw,
        };
        let (name, version) = match ecosystem {
            Ecosystem::Python => {
                let doc: toml::Value = declared.parse()?;
                let project = doc.get("project");
                (
                    project
                        .and_then(|p| p.get("name"))
                        .and_then(|v| v.as_str())
                        .map(normalize_python_name),
                    project
                        .and_then(|p| p.get("version"))
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                )
            }
            Ecosystem::Npm => {
                let doc: serde_json::Value = serde_json::from_str(&declared)?;
                (
                    doc.get("name").and_then(|v| v.as_str()).map(str::to_owned),
                    doc.get("version")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                )
            }
        };
        let Some(name) = name else { return Ok(None) };
        let dependencies = load_manifest_content(&manifest, &declared)?.declarations;
        Ok(Some(Self {
            path,
            manifest,
            ecosystem,
            name,
            version,
            declared,
            dependencies,
        }))
    }

    fn compatible(&self, dependency: &Declaration) -> Result<bool> {
        let version = self.version.as_deref().with_context(|| {
            format!(
                "local package {} has no static version in {}",
                self.name,
                self.manifest.display()
            )
        })?;
        match self.ecosystem {
            Ecosystem::Npm => {
                let version: node_semver::Version = version.parse()?;
                // The resolver also accepts comma-separated major ranges.
                let range: node_semver::Range =
                    dependency.raw.replace(',', " ").parse().with_context(|| {
                        format!(
                            "cannot verify local compatibility for {}: {}",
                            dependency.name, dependency.raw
                        )
                    })?;
                Ok(version.satisfies(&range))
            }
            Ecosystem::Python => {
                let version: pep440_rs::Version = version.parse()?;
                let requirement = dependency.raw.split(';').next().unwrap_or("").trim();
                let end = requirement
                    .find(|c: char| !(c.is_ascii_alphanumeric() || "._-".contains(c)))
                    .unwrap_or(requirement.len());
                let mut range = requirement[end..].trim();
                if range.starts_with('[') {
                    range = range
                        .split_once(']')
                        .context("unterminated Python extras")?
                        .1
                        .trim();
                }
                let spec = match dependency.spec {
                    Spec::Major(major) => format!(">={major},<{}", major + 1),
                    _ => range.trim_matches(['(', ')']).to_owned(),
                };
                let spec: pep440_rs::VersionSpecifiers = spec.parse()?;
                Ok(spec.contains(&version))
            }
        }
    }
}

/// A fully validated DAG. Nothing is rewritten or built until the entire
/// reachable closure has passed identity, version and cycle checks.
pub struct BuildPlan {
    projects: Vec<LocalProject>,
    edges: BTreeMap<usize, Vec<(Declaration, usize)>>,
    pub order: Vec<usize>,
}

impl BuildPlan {
    pub fn new(workspace_root: &Path, target: &Path) -> Result<Self> {
        let metadata = JumboToml::load(workspace_root)?;
        let mut projects = Vec::new();
        let mut identities: BTreeMap<(String, String), usize> = BTreeMap::new();
        let mut paths = BTreeSet::new();
        for repo in &metadata.workspace.repositories {
            let path = workspace_root.join(&repo.path);
            if !path.is_dir() {
                continue;
            } // absent checkout: ordinary remote resolution
            let path = path.canonicalize()?;
            if !paths.insert(path.clone()) {
                continue;
            }
            let Some(project) = LocalProject::load(path)? else {
                continue;
            };
            let key = (project.ecosystem.as_str().to_owned(), project.name.clone());
            if let Some(previous) = identities.insert(key, projects.len()) {
                let prior: &LocalProject = &projects[previous];
                bail!(
                    "ambiguous local package {} ({}): {} and {}",
                    project.name,
                    project.ecosystem.as_str(),
                    prior.path.display(),
                    project.path.display()
                );
            }
            projects.push(project);
        }
        let target = target.canonicalize()?;
        let root = projects
            .iter()
            .position(|p| p.path == target)
            .context("current project has no package identity in its manifest")?;
        let mut plan = Self {
            projects,
            edges: BTreeMap::new(),
            order: Vec::new(),
        };
        plan.visit(root, &identities, &mut Vec::new(), &mut BTreeSet::new())?;
        Ok(plan)
    }

    fn visit(
        &mut self,
        current: usize,
        identities: &BTreeMap<(String, String), usize>,
        stack: &mut Vec<usize>,
        done: &mut BTreeSet<usize>,
    ) -> Result<()> {
        if done.contains(&current) {
            return Ok(());
        }
        if stack.contains(&current) {
            let mut names: Vec<_> = stack
                .iter()
                .map(|i| self.projects[*i].name.as_str())
                .collect();
            names.push(&self.projects[current].name);
            bail!("local dependency cycle: {}", names.join(" -> "));
        }
        stack.push(current);
        let mut edges = Vec::new();
        for dependency in self.projects[current].dependencies.clone() {
            let key = (
                self.projects[current].ecosystem.as_str().to_owned(),
                dependency.name.clone(),
            );
            let Some(&local) = identities.get(&key) else {
                continue;
            };
            let producer = &self.projects[local];
            if !producer.compatible(&dependency)? {
                bail!("incompatible local package {} at {}: version {} does not satisfy {} required by {}",
                    producer.name, producer.path.display(), producer.version.as_deref().unwrap_or("unknown"),
                    dependency.raw, self.projects[current].name);
            }
            self.visit(local, identities, stack, done)?;
            edges.push((dependency, local));
        }
        self.edges.insert(current, edges);
        stack.pop();
        done.insert(current);
        self.order.push(current);
        Ok(())
    }

    /// Only a successfully rebuilt lock may retire previous local provenance.
    pub fn finish(&self) -> Result<()> {
        for &index in &self.order {
            if self.edges[&index].is_empty() {
                clear_local_provenance(&self.projects[index].path)?;
            }
        }
        Ok(())
    }

    pub fn project(&self, index: usize) -> &LocalProject {
        &self.projects[index]
    }

    /// Install the temporary developer inputs. A guard restores the exact
    /// previous manifest bytes on success, failure, or unwinding.
    pub fn prepare(&self, index: usize) -> Result<ManifestGuard> {
        let project = &self.projects[index];
        let guard = ManifestGuard::new(&project.manifest)?;
        let edges = &self.edges[&index];
        match project.ecosystem {
            Ecosystem::Npm => {
                let mut doc: serde_json::Value = serde_json::from_str(&project.declared)?;
                // Remove local declarations while resolving only the remaining
                // remote internal dependencies through the existing index path.
                for (dependency, _) in edges {
                    for section in NODE_SECTIONS {
                        if let Some(map) = doc.get_mut(section).and_then(|v| v.as_object_mut()) {
                            map.remove(&dependency.name);
                        }
                    }
                }
                std::fs::write(&project.manifest, serde_json::to_string_pretty(&doc)?)?;
                materialize_remote(&project.manifest, &project.path)?;
                doc = serde_json::from_str(&std::fs::read_to_string(&project.manifest)?)?;
                let original: serde_json::Value = serde_json::from_str(&project.declared)?;
                for (dependency, local) in edges {
                    let relative = relative_path(&project.path, &self.projects[*local].path)?;
                    for section in NODE_SECTIONS {
                        if original
                            .get(section)
                            .and_then(|v| v.get(&dependency.name))
                            .is_some()
                        {
                            let map = doc
                                .as_object_mut()
                                .context("package.json must be an object")?
                                .entry(section)
                                .or_insert_with(|| serde_json::json!({}));
                            map.as_object_mut()
                                .context("dependency section must be an object")?
                                .insert(
                                    dependency.name.clone(),
                                    serde_json::Value::String(format!("file:{relative}")),
                                );
                        }
                    }
                }
                std::fs::write(
                    &project.manifest,
                    serde_json::to_string_pretty(&doc)? + "\n",
                )?;
            }
            Ecosystem::Python => {
                // Reuse the generated uv workspace and its editable sources.
                // Child sources may carry old published overlays; the selected
                // local package explicitly overrides those with workspace=true.
                let mut doc: toml::Value = project.declared.parse()?;
                normalize_python_majors(&mut doc)?;
                if !edges.is_empty() {
                    let root = doc
                        .as_table_mut()
                        .context("pyproject.toml must be a table")?;
                    let tool = root
                        .entry("tool")
                        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                        .as_table_mut()
                        .context("tool must be a table")?;
                    let uv = tool
                        .entry("uv")
                        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                        .as_table_mut()
                        .context("tool.uv must be a table")?;
                    let sources = uv
                        .entry("sources")
                        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                        .as_table_mut()
                        .context("tool.uv.sources must be a table")?;
                    for (dependency, _) in edges {
                        let mut source = toml::Table::new();
                        source.insert("workspace".to_owned(), toml::Value::Boolean(true));
                        sources.insert(dependency.name.clone(), toml::Value::Table(source));
                    }
                }
                if !edges.is_empty() || doc != project.declared.parse::<toml::Value>()? {
                    std::fs::write(&project.manifest, toml::to_string_pretty(&doc)?)?;
                }
            }
        }
        if !edges.is_empty() {
            let provenance: Vec<_> = edges
                .iter()
                .map(|(dependency, index)| {
                    Ok(serde_json::json!({
                        "package": dependency.name,
                        "ecosystem": project.ecosystem.as_str(),
                        "path": relative_path(&project.path, &self.projects[*index].path)?,
                        "version": self.projects[*index].version,
                        "source": "local-checkout"
                    }))
                })
                .collect::<Result<_>>()?;
            let deps = project.path.join("deps");
            std::fs::create_dir_all(&deps)?;
            std::fs::write(
                deps.join(LOCAL_INPUTS_MARKER),
                serde_json::to_string_pretty(&provenance)? + "\n",
            )?;
        }
        Ok(guard)
    }
}

fn normalize_python_majors(value: &mut toml::Value) -> Result<()> {
    // Only dependency lists are visited: descriptions and arbitrary config
    // strings are never interpreted as requirements.
    fn list(value: &mut toml::Value) -> Result<()> {
        if let Some(entries) = value.as_array_mut() {
            for entry in entries {
                if let Some(raw) = entry.as_str() {
                    let declaration =
                        crate::resolver::declaration::parse_python(raw, "local workspace")?;
                    if let Spec::Major(major) = declaration.spec {
                        let extras = if declaration.extras.is_empty() {
                            String::new()
                        } else {
                            format!("[{}]", declaration.extras.join(","))
                        };
                        let marker = declaration
                            .marker
                            .map(|m| format!("; {m}"))
                            .unwrap_or_default();
                        *entry = toml::Value::String(format!(
                            "{}{extras}>={major},<{}{marker}",
                            declaration.name,
                            major + 1
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    if let Some(project) = value.get_mut("project") {
        if let Some(deps) = project.get_mut("dependencies") {
            list(deps)?;
        }
        if let Some(extras) = project
            .get_mut("optional-dependencies")
            .and_then(|v| v.as_table_mut())
        {
            for (_, deps) in extras.iter_mut() {
                list(deps)?;
            }
        }
    }
    if let Some(groups) = value
        .get_mut("dependency-groups")
        .and_then(|v| v.as_table_mut())
    {
        for (_, deps) in groups.iter_mut() {
            list(deps)?;
        }
    }
    Ok(())
}

pub fn clear_local_provenance(project: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(project.join("deps").join(LOCAL_INPUTS_MARKER)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn materialize_remote(manifest: &Path, project: &Path) -> Result<()> {
    let loaded = crate::resolver::manifest::load_manifest(manifest)?;
    let explicit_index = std::env::var("JUMBO_INDEX_PATH")
        .ok()
        .or_else(|| std::env::var("JUMBO_INDEX_URL").ok());
    let needs_index = !loaded.declarations.is_empty()
        && (explicit_index.is_some()
            || loaded
                .declarations
                .iter()
                .any(|d| d.name.starts_with("@juntai/") || d.name.starts_with("@zephytiju/")));
    if !needs_index {
        return Ok(());
    }
    let source = crate::resolver::resolve_source(explicit_index.as_deref())?;
    let index = crate::resolver::Index::load(&source)?;
    let generation = crate::fingerprint::generate_lock_inputs(manifest, &index)?;
    if generation.internal.is_empty() {
        return Ok(());
    }
    let provider = std::env::var("JUMBO_ARTIFACT_DIR")
        .ok()
        .map(|p| crate::dedup::ArtifactProvider::Cache(PathBuf::from(p)))
        .unwrap_or(crate::dedup::ArtifactProvider::Remote);
    let repo_map = std::env::var("JUMBO_REPO_MAP")
        .ok()
        .map(|p| crate::dedup::RepoMap::load(Path::new(&p)))
        .transpose()?;
    let staging = project.join("deps/.jumbo-workspace-stage");
    std::fs::create_dir_all(&staging)?;
    let result = crate::dedup::materialize_dependency_artifacts(
        manifest,
        &generation.internal,
        generation.ecosystem,
        &provider,
        &staging,
        repo_map.as_ref(),
    );
    let _ = std::fs::remove_dir_all(&staging);
    result?;
    Ok(())
}

fn relative_path(from: &Path, to: &Path) -> Result<String> {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut path = PathBuf::new();
    for _ in common..from.len() {
        path.push("..");
    }
    for component in &to[common..] {
        path.push(component.as_os_str());
    }
    path.to_str()
        .map(|p| p.replace('\\', "/"))
        .context("local package path is not UTF-8")
}

pub struct ManifestGuard {
    manifest: PathBuf,
    original: Vec<u8>,
}
impl ManifestGuard {
    fn new(manifest: &Path) -> Result<Self> {
        Ok(Self {
            manifest: manifest.to_owned(),
            original: std::fs::read(manifest)?,
        })
    }
    pub fn restore(&self) -> Result<()> {
        std::fs::write(&self.manifest, &self.original)
            .with_context(|| format!("restoring {}", self.manifest.display()))
    }
}
impl Drop for ManifestGuard {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!("{error:#}");
        }
    }
}

/// Build each dependency once using its own backend; the caller applies
/// test/format/release only to the requested project.
pub fn build_dependencies(plan: &BuildPlan, workspace_root: &Path) -> Result<()> {
    let registry = get_registry();
    for &index in plan.order.iter().take(plan.order.len().saturating_sub(1)) {
        let project = plan.project(index);
        let lang: &dyn LanguageSupport = registry
            .iter()
            .find(|l| {
                l.name()
                    == match project.ecosystem {
                        Ecosystem::Python => "python",
                        Ecosystem::Npm => "node",
                    }
            })
            .context("language backend unavailable")?
            .as_ref();
        println!(
            "  Building local dependency {} ({})",
            project.name,
            project.ecosystem.as_str()
        );
        lang.build(workspace_root, &project.path)?;
    }
    Ok(())
}
