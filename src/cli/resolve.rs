use anyhow::{bail, Result};
use clap::Args;
use std::path::PathBuf;

use crate::resolver;

/// Resolve internal dependencies by declared major against the Jumbo index
#[derive(Args)]
pub struct ResolveArgs {
    /// Declaration to resolve, e.g. `juntai-fuse-api[http]@2` or `@juntai/pkg@^1`
    #[arg(value_name = "DECLARATION")]
    pub declaration: Option<String>,

    /// Manifest to validate and resolve (pyproject.toml or package.json)
    #[arg(short, long, value_name = "PATH")]
    pub manifest: Option<PathBuf>,

    /// Jumbo index location: a local clone path or an https://github.com URL
    /// (default: JUMBO_INDEX_PATH, then JUMBO_INDEX_URL, then the JumboIndex repository)
    #[arg(short, long, value_name = "PATH_OR_URL")]
    pub index: Option<String>,

    /// Validate declaration forms only; do not look up index records
    #[arg(long)]
    pub check: bool,
}

pub fn execute(args: ResolveArgs) -> Result<()> {
    match (&args.declaration, &args.manifest) {
        (Some(_), Some(_)) => bail!("DECLARATION and --manifest cannot be combined"),
        (None, None) => {
            bail!("pass a DECLARATION (e.g. pkg@2) or --manifest <pyproject.toml|package.json>")
        }
        (Some(_), None) if args.check => {
            bail!("--check applies to --manifest, not to a single DECLARATION")
        }
        (Some(declaration), None) => {
            let source = resolver::resolve_source(args.index.as_deref())?;
            let index = resolver::Index::load(&source).map_err(|e| {
                anyhow::anyhow!(e).context(format!("index source: {}", source.describe()))
            })?;
            let resolution = resolver::resolve_declaration(&index, declaration)?;
            println!("{}", serde_json::to_string_pretty(&resolution)?);
            Ok(())
        }
        (None, Some(path)) => {
            let source = resolver::resolve_source(args.index.as_deref())?;
            let index = resolver::Index::load(&source).map_err(|e| {
                anyhow::anyhow!(e).context(format!("index source: {}", source.describe()))
            })?;
            if args.check {
                let report = resolver::check_manifest(path, &index)?;
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                let report = resolver::resolve_manifest(path, &index)?;
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
            Ok(())
        }
    }
}
