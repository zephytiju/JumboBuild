use anyhow::{bail, Result};
use clap::Args;
use std::path::PathBuf;

use crate::fingerprint;
use crate::resolver;

/// Generate the language lock for a manifest, injecting internal
/// dependencies from the Jumbo index at stable relative paths
#[derive(Args)]
pub struct LockArgs {
    /// Manifest to lock (pyproject.toml or package.json); default: pyproject.toml
    /// or package.json in the current directory
    #[arg(short, long, value_name = "PATH")]
    pub manifest: Option<PathBuf>,

    /// Jumbo index location: a local clone path or an https://github.com URL
    /// (default: JUMBO_INDEX_PATH, then JUMBO_INDEX_URL, then the JumboIndex repository)
    #[arg(short, long, value_name = "PATH_OR_URL")]
    pub index: Option<String>,

    /// Materialize the injected sources and rewrite the manifest without
    /// running the language lock tool (uv/npm)
    #[arg(long)]
    pub inject_only: bool,
}

/// Locate the manifest: the given path, or the pyproject.toml/package.json
/// in the current directory.
fn resolve_manifest_path(manifest: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(path) = manifest {
        if !path.exists() {
            bail!("manifest not found: {}", path.display());
        }
        return Ok(path.clone());
    }
    for name in ["pyproject.toml", "package.json"] {
        let candidate = PathBuf::from(name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("no pyproject.toml or package.json in the current directory; pass --manifest <PATH>")
}

pub fn execute(args: LockArgs) -> Result<()> {
    let manifest = resolve_manifest_path(args.manifest.as_ref())?;
    let source = resolver::resolve_source(args.index.as_deref())?;
    let index = resolver::Index::load(&source)
        .map_err(|e| anyhow::anyhow!(e).context(format!("index source: {}", source.describe())))?;

    let generation = fingerprint::generate_lock_inputs(&manifest, &index).map_err(|e| {
        anyhow::anyhow!(e).context(format!("lock generation for {}", manifest.display()))
    })?;

    if !args.inject_only {
        let (command, description) = generation.lock_command();
        let working_dir = manifest
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        crate::utils::runner::run_steps(&[(command, description)], &working_dir)?;
    }

    let report = serde_json::json!({
        "manifest": generation.manifest.display().to_string(),
        "ecosystem": generation.ecosystem.as_str(),
        "lock": generation.lock_path.display().to_string(),
        "toolRan": !args.inject_only,
        "injectedSources": generation.injected.iter().map(|s| serde_json::json!({
            "name": s.name,
            "version": s.version,
            "path": s.path,
            "commit": s.commit,
            "declared": s.declared,
        })).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
