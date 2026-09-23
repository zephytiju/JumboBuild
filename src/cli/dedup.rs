use anyhow::{bail, Context, Result};
use clap::Args;
use std::path::PathBuf;

use crate::dedup::{self, ArtifactProvider};
use crate::fingerprint::{self, FingerprintReport};
use crate::resolver;
use crate::resolver::manifest::Ecosystem;

/// Decide build-or-reuse against the Jumbo index by input fingerprint,
/// and optionally materialize the recorded artifacts
#[derive(Args)]
pub struct DedupArgs {
    /// Manifest of the project about to build (pyproject.toml or
    /// package.json). Its jumbo-generated lock (uv.lock /
    /// package-lock.json) is fingerprinted when present — refresh it with
    /// `jumbo lock`; one is generated only when absent
    #[arg(short, long, value_name = "PATH")]
    pub manifest: Option<PathBuf>,

    /// Fingerprint an existing lock file (uv.lock or package-lock.json)
    /// instead of using the manifest; a pure-local query that runs no tool
    #[arg(short, long, value_name = "PATH")]
    pub lock: Option<PathBuf>,

    /// Package name whose index history is searched (default: the manifest
    /// or lock root's own name)
    #[arg(short, long, value_name = "NAME")]
    pub package: Option<String>,

    /// Jumbo index location: a local clone path or an https://github.com URL
    /// (default: JUMBO_INDEX_PATH, then JUMBO_INDEX_URL, then the JumboIndex repository)
    #[arg(short, long, value_name = "PATH_OR_URL")]
    pub index: Option<String>,

    /// On a duplicate, pull the matched record's artifact (exact URL,
    /// SHA-256 verified) into the project's dist directory instead of
    /// leaving the rebuild to the pipeline
    #[arg(long)]
    pub materialize: bool,

    /// Materialize the recorded artifacts of the manifest's internal
    /// dependencies, replacing their source overlays (manifest: --manifest
    /// or pyproject.toml/package.json in the current directory)
    #[arg(long)]
    pub deps: bool,

    /// Build-output directory for a pulled own-record artifact (default: dist)
    #[arg(long, value_name = "DIR")]
    pub dist_dir: Option<String>,

    /// Resolve artifacts by exact file name from a local directory (a CI
    /// asset cache or offline fixture directory) instead of downloading;
    /// the recorded SHA-256 is still enforced
    /// (default: JUMBO_ARTIFACT_DIR when set, otherwise download)
    #[arg(long, value_name = "DIR")]
    pub artifact_dir: Option<PathBuf>,
}

/// The ecosystem of a fingerprint report.
fn ecosystem_of(report: &FingerprintReport) -> Ecosystem {
    match report.ecosystem {
        "python" => Ecosystem::Python,
        _ => Ecosystem::Npm,
    }
}

/// The own package name of a manifest: `[project].name` (PEP 503
/// normalized for index lookup) or package.json `name`.
fn manifest_package_name(manifest: &std::path::Path) -> Result<String> {
    let is_python = manifest
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "pyproject.toml");
    if is_python {
        let content = std::fs::read_to_string(manifest)
            .with_context(|| format!("reading {}", manifest.display()))?;
        let doc: toml::Table = content
            .parse()
            .with_context(|| format!("parsing {}", manifest.display()))?;
        let name = doc
            .get("project")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .with_context(|| format!("{} has no [project].name", manifest.display()))?;
        Ok(crate::resolver::index::normalize_python_name(name))
    } else {
        let content = std::fs::read_to_string(manifest)
            .with_context(|| format!("reading {}", manifest.display()))?;
        let doc: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("parsing {}", manifest.display()))?;
        doc.get("name")
            .and_then(|n| n.as_str())
            .map(str::to_string)
            .with_context(|| format!("{} has no name", manifest.display()))
    }
}

/// The own package name of a lock file: the root entry uv/npm record.
fn lock_package_name(lock: &std::path::Path) -> Result<Option<String>> {
    let file_name = lock
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let content =
        std::fs::read_to_string(lock).with_context(|| format!("reading {}", lock.display()))?;
    match file_name {
        "uv.lock" => {
            let doc: toml::Table = content
                .parse()
                .with_context(|| format!("parsing {}", lock.display()))?;
            // The root is the `editable = "."` (or `virtual = "."`) entry.
            for package in doc
                .get("package")
                .and_then(|p| p.as_array())
                .unwrap_or(&Vec::new())
            {
                let is_root = package
                    .get("source")
                    .and_then(|s| s.as_table())
                    .map(|s| {
                        s.get("editable")
                            .and_then(|e| e.as_str())
                            .or_else(|| s.get("virtual").and_then(|v| v.as_str()))
                            .is_some_and(|p| p.trim() == ".")
                    })
                    .unwrap_or(false);
                if is_root {
                    if let Some(name) = package.get("name").and_then(|n| n.as_str()) {
                        return Ok(Some(crate::resolver::index::normalize_python_name(name)));
                    }
                }
            }
            Ok(None)
        }
        "package-lock.json" => {
            let doc: serde_json::Value = serde_json::from_str(&content)?;
            Ok(doc.get("name").and_then(|n| n.as_str()).map(str::to_string))
        }
        _ => Ok(None),
    }
}

/// Where artifact bytes come from: an explicit `--artifact-dir`, the
/// `JUMBO_ARTIFACT_DIR` environment variable, or the network (the
/// validated github.com-only layer — identical behavior locally and in CI).
fn artifact_provider(dir: Option<&PathBuf>) -> ArtifactProvider {
    let dir = dir
        .cloned()
        .or_else(|| std::env::var("JUMBO_ARTIFACT_DIR").ok().map(PathBuf::from));
    match dir {
        Some(dir) => ArtifactProvider::Cache(dir),
        None => ArtifactProvider::Remote,
    }
}

/// A fresh staging directory for fetched artifacts.
fn staging_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "jumbo-dedup-stage-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir).expect("create staging dir");
    dir
}

pub fn execute(args: DedupArgs) -> Result<()> {
    if args.lock.is_some() && args.manifest.is_some() {
        bail!("--lock and --manifest cannot be combined");
    }
    // --deps works on a manifest: explicit, or detected in the cwd.
    let deps_manifest: Option<PathBuf> = if args.deps {
        match &args.manifest {
            Some(path) => {
                if !path.exists() {
                    bail!("manifest not found: {}", path.display());
                }
                Some(path.clone())
            }
            None => Some(detect_manifest().map_err(|_| {
                anyhow::anyhow!(
                    "--deps requires a manifest: pass --manifest <PATH> or run from a directory \
                     containing pyproject.toml or package.json"
                )
            })?),
        }
    } else {
        None
    };
    let source = resolver::resolve_source(args.index.as_deref())?;
    let index = resolver::Index::load(&source)
        .map_err(|e| anyhow::anyhow!(e).context(format!("index source: {}", source.describe())))?;

    // 1. Compute the input fingerprint of the project about to build.
    let (report, manifest_path) = match (&args.lock, &args.manifest) {
        (Some(lock), None) => {
            if !lock.exists() {
                bail!("lock file not found: {}", lock.display());
            }
            let report = fingerprint::fingerprint_lock_file(lock, false).map_err(|e| {
                anyhow::anyhow!(e).context(format!("fingerprinting {}", lock.display()))
            })?;
            (report, None)
        }
        (None, manifest) => {
            let manifest = match manifest {
                Some(path) => {
                    if !path.exists() {
                        bail!("manifest not found: {}", path.display());
                    }
                    path.clone()
                }
                None => detect_manifest()?,
            };
            // Prefer the jumbo-generated lock next to the manifest — the
            // pipeline generates it with `jumbo lock`, so the decision
            // reads exactly the resolved inputs (identically offline and
            // in CI). Only generate one when it is absent.
            let lock_name = match manifest.file_name().and_then(|n| n.to_str()) {
                Some("pyproject.toml") => "uv.lock",
                _ => "package-lock.json",
            };
            let existing = manifest
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.join(lock_name))
                .filter(|p| p.is_file())
                .unwrap_or_else(|| PathBuf::from(lock_name));
            let report = if existing.is_file() {
                fingerprint::fingerprint_lock_file(&existing, false).map_err(|e| {
                    anyhow::anyhow!(e).context(format!("fingerprinting {}", existing.display()))
                })?
            } else {
                let (_generation, report) = fingerprint::fingerprint_manifest(
                    &manifest, &index, false, true,
                )
                .map_err(|e| {
                    anyhow::anyhow!(e).context(format!("fingerprinting {}", manifest.display()))
                })?;
                report
            };
            (report, Some(manifest))
        }
        _ => unreachable!("both set was rejected above"),
    };

    // 2. The package name whose history is searched.
    let package = match (&args.package, &manifest_path, &args.lock) {
        (Some(name), _, _) => name.clone(),
        (None, Some(manifest), _) => manifest_package_name(manifest)?,
        (None, None, Some(lock)) => lock_package_name(lock)?.with_context(|| {
            format!(
                "could not derive the package name from {}; pass --package <NAME>",
                lock.display()
            )
        })?,
        _ => unreachable!(),
    };

    // 3. The build-or-reuse decision.
    let decision = dedup::decide(&package, &report.fingerprint, &index)
        .map_err(|e| anyhow::anyhow!(e).context("dedup decision"))?;

    // 4. Optional materialization.
    let mut materialized_self = None;
    let mut materialized_deps: Option<serde_json::Value> = None;
    if args.materialize || args.deps {
        let provider = artifact_provider(args.artifact_dir.as_ref());
        let staging = staging_dir();
        let result = (|| -> Result<()> {
            // A miss with --materialize is not an error: the decision says
            // build, and building from source is the pipeline's next step,
            // not the dedup command's.
            if args.materialize && decision.duplicate {
                let record = decision
                    .matched_record
                    .as_ref()
                    .expect("a duplicate has a matched record")
                    .record
                    .clone();
                let project_dir = project_dir_of(&report);
                let dist_dir = args
                    .dist_dir
                    .clone()
                    .unwrap_or_else(|| dedup::DEFAULT_DIST_DIR.to_string());
                let artifact = dedup::materialize_self_artifact(
                    &project_dir,
                    &dist_dir,
                    &record,
                    ecosystem_of(&report),
                    &provider,
                    &staging,
                )
                .map_err(|e| {
                    anyhow::anyhow!(e).context(format!("materializing the artifact of {package}"))
                })?;
                materialized_self = Some(serde_json::to_value(&artifact)?);
            }
            if args.deps {
                let manifest = deps_manifest
                    .as_ref()
                    .expect("--deps implies a manifest was resolved");
                // Source overlays first (J3's inject-only lock inputs), so
                // the artifact rewrite always applies to an injected
                // manifest. The generation carries the resolution taken
                // against the declared forms.
                let generation =
                    fingerprint::generate_lock_inputs(manifest, &index).map_err(|e| {
                        anyhow::anyhow!(e)
                            .context(format!("injecting lock inputs for {}", manifest.display()))
                    })?;
                let kept: Vec<String> = generation
                    .internal
                    .iter()
                    .filter(|d| d.record.artifact_url.is_none())
                    .map(|d| d.name.clone())
                    .collect();
                let materialized = dedup::materialize_dependency_artifacts(
                    manifest,
                    &generation.internal,
                    ecosystem_of(&report),
                    &provider,
                    &staging,
                )
                .map_err(|e| anyhow::anyhow!(e).context("materializing dependency artifacts"))?;
                materialized_deps = Some(serde_json::json!({
                "materialized": serde_json::to_value(&materialized)?,
                "keptSourceOverlays": kept,
                }));
            }
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&staging);
        result?;
    }

    // 5. The machine-readable decision for the pipeline (C3 contract):
    //    {duplicate, matchedRecord, action} plus materialization evidence.
    let mut json = serde_json::to_value(&decision)?;
    let object = json
        .as_object_mut()
        .expect("the decision serializes to an object");
    object.insert(
        "ecosystem".to_string(),
        serde_json::Value::String(report.ecosystem.to_string()),
    );
    object.insert(
        "indexSource".to_string(),
        serde_json::Value::String(source.describe()),
    );
    if let Some(materialized) = materialized_self {
        object.insert("materialized".to_string(), materialized);
    }
    if let Some(deps) = materialized_deps {
        object.insert("dependencies".to_string(), deps);
    }
    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}

/// The project directory of the fingerprinted project.
fn project_dir_of(report: &FingerprintReport) -> PathBuf {
    std::path::Path::new(&report.lock)
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Detect pyproject.toml or package.json in the current directory.
fn detect_manifest() -> Result<PathBuf> {
    for name in ["pyproject.toml", "package.json"] {
        let candidate = PathBuf::from(name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("no pyproject.toml or package.json in the current directory; pass --manifest <PATH> or --lock <PATH>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_package_name_reads_the_uv_root_entry() {
        let dir = std::env::temp_dir().join(format!(
            "jumbo-dedup-cli-ut-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(
            dir.join("uv.lock"),
            "[[package]]\nname = \"Demo_Alpha\"\nversion = \"2.4.0\"\nsource = { editable = \".\" }\n",
        )
        .expect("uv.lock");
        std::fs::write(
            dir.join("package-lock.json"),
            "{\"name\": \"@juntai/consumer\", \"lockfileVersion\": 3, \"packages\": {}}",
        )
        .expect("package-lock.json");
        assert_eq!(
            lock_package_name(&dir.join("uv.lock")).unwrap().as_deref(),
            Some("demo-alpha")
        );
        assert_eq!(
            lock_package_name(&dir.join("package-lock.json"))
                .unwrap()
                .as_deref(),
            Some("@juntai/consumer")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
