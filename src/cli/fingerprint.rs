use anyhow::{bail, Result};
use clap::Args;
use std::path::PathBuf;

use crate::fingerprint;
use crate::resolver;

/// Compute sha256(own commit + canonical extract) of the generated language lock
#[derive(Args)]
pub struct FingerprintArgs {
    /// Manifest whose lock is generated and fingerprinted (pyproject.toml or
    /// package.json); default: the manifest in the current directory
    #[arg(short, long, value_name = "PATH")]
    pub manifest: Option<PathBuf>,

    /// Fingerprint an existing lock file (uv.lock or package-lock.json)
    /// instead of generating one; a pure-local query that never promotes
    #[arg(short, long, value_name = "PATH")]
    pub lock: Option<PathBuf>,

    /// Jumbo index location: a local clone path or an https://github.com URL
    /// (default: JUMBO_INDEX_PATH, then JUMBO_INDEX_URL, then the JumboIndex repository)
    #[arg(short, long, value_name = "PATH_OR_URL")]
    pub index: Option<String>,

    /// Promotion mode: refuse on a dirty working tree (pipelines only;
    /// pure-local queries never promote)
    #[arg(long)]
    pub promote: bool,
}

pub fn execute(args: FingerprintArgs) -> Result<()> {
    let report = match (&args.lock, &args.manifest) {
        (Some(_), Some(_)) => bail!("--lock and --manifest cannot be combined"),
        (Some(lock), None) => {
            if !lock.exists() {
                bail!("lock file not found: {}", lock.display());
            }
            fingerprint::fingerprint_lock_file(lock, args.promote).map_err(|e| {
                anyhow::anyhow!(e).context(format!("fingerprinting {}", lock.display()))
            })?
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
            let source = resolver::resolve_source(args.index.as_deref())?;
            let index = resolver::Index::load(&source).map_err(|e| {
                anyhow::anyhow!(e).context(format!("index source: {}", source.describe()))
            })?;
            let (_generation, report) =
                fingerprint::fingerprint_manifest(&manifest, &index, args.promote, true).map_err(
                    |e| {
                        anyhow::anyhow!(e).context(format!("fingerprinting {}", manifest.display()))
                    },
                )?;
            report
        }
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
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
