use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

use crate::dedup::ArtifactProvider;
use crate::pinning::{self, ReproduceOptions};
use crate::resolver;

/// Reproduce the pinned build a buildId refers to (`jumbo build --pinned`):
/// resolve the record, verify the recomputed fingerprint against the
/// recorded one, and materialize the recorded closure and artifact
/// (sha256-enforced)
#[derive(Args)]
pub struct ReproduceArgs {
    /// The buildId to reproduce (recorded, or the documented bootstrap
    /// derivation)
    #[arg(value_name = "BUILD_ID")]
    pub build_id: String,

    /// Constrain the buildId search to one package (default: the whole index)
    #[arg(short, long, value_name = "NAME")]
    pub package: Option<String>,

    /// Jumbo index location: a local clone path or an https://github.com URL
    /// (default: JUMBO_INDEX_PATH, then JUMBO_INDEX_URL, then the JumboIndex repository)
    #[arg(short, long, value_name = "PATH_OR_URL")]
    pub index: Option<String>,

    /// Resolve artifacts by exact file name from a local directory (a CI
    /// asset cache or offline fixture directory) instead of downloading;
    /// the recorded SHA-256 is still enforced
    /// (default: JUMBO_ARTIFACT_DIR when set, otherwise download)
    #[arg(long, value_name = "DIR")]
    pub artifact_dir: Option<PathBuf>,

    /// Where the reproduction outputs land (default: ./reproduced)
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,
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
        "jumbo-reproduce-stage-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir).expect("create staging dir");
    dir
}

pub fn execute(args: ReproduceArgs) -> Result<()> {
    let source = resolver::resolve_source(args.index.as_deref())?;
    let index = resolver::Index::load(&source)?;
    let options = ReproduceOptions {
        package: args.package.clone(),
        out_dir: args
            .out
            .clone()
            .unwrap_or_else(|| PathBuf::from("reproduced")),
    };
    let provider = artifact_provider(args.artifact_dir.as_ref());
    let staging = staging_dir();
    // The typed pinning error is the primary message: it already names the
    // package, buildId, and remediation — a wrapping context would only
    // bury it under "Caused by".
    let result = pinning::reproduce(&index, &args.build_id, &options, &provider, &staging);
    let _ = std::fs::remove_dir_all(&staging);
    let report = result?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
