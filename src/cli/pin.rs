use anyhow::Result;
use clap::Args;

use crate::pinning::{self, PinOptions, PinSelector};
use crate::resolver;

/// Emit a deployment pin manifest (`jumbo.deployment-pin/v1`) for one
/// promoted build of one package — the fields a downstream Pulumi program
/// flows into the existing vangu Selection and PackageLock path
#[derive(Args)]
pub struct PinArgs {
    /// Package to pin (language-native name, e.g. juntai-fuse-api or
    /// @juntai/demo-kit)
    #[arg(value_name = "PACKAGE")]
    pub package: String,

    #[command(flatten)]
    pub selector: PinSelectorArgs,

    /// Jumbo index location: a local clone path or an https://github.com URL
    /// (default: JUMBO_INDEX_PATH, then JUMBO_INDEX_URL, then the JumboIndex repository)
    #[arg(short, long, value_name = "PATH_OR_URL")]
    pub index: Option<String>,

    /// Image name for `imageRef` (overrides the ghcr.io/<owner>/<repo>
    /// derivation from the record's artifact URL; the index records the
    /// digest, the deployment owns the reference name)
    #[arg(long, value_name = "NAME")]
    pub image_name: Option<String>,

    /// Fail when the record published no imageDigest (the deployment pins
    /// a service image)
    #[arg(long)]
    pub require_image: bool,
}

/// Exactly one selection mode (enforced by clap: required, exclusive).
#[derive(Args)]
#[group(required = true, multiple = false)]
pub struct PinSelectorArgs {
    /// Pin the record with this buildId (recorded, or the documented
    /// bootstrap derivation)
    #[arg(long, value_name = "BUILD_ID")]
    pub by_build_id: Option<String>,

    /// Pin the newest record promoted from this commit (full 40-hex SHA)
    #[arg(long, value_name = "SHA")]
    pub by_commit: Option<String>,

    /// Pin the newest record of the major — what dependency resolution
    /// resolves
    #[arg(long, value_name = "MAJOR")]
    pub latest_of_major: Option<u64>,
}

impl From<&PinSelectorArgs> for Option<PinSelector> {
    fn from(args: &PinSelectorArgs) -> Self {
        if let Some(id) = &args.by_build_id {
            Some(PinSelector::ByBuildId(id.clone()))
        } else if let Some(commit) = &args.by_commit {
            Some(PinSelector::ByCommit(commit.clone()))
        } else {
            args.latest_of_major.map(PinSelector::LatestOfMajor)
        }
    }
}

pub fn execute(args: PinArgs) -> Result<()> {
    let selector: Option<PinSelector> = (&args.selector).into();
    let selector = selector.expect("clap enforces exactly one selector");
    let source = resolver::resolve_source(args.index.as_deref())?;
    let index = resolver::Index::load(&source)
        .map_err(|e| anyhow::anyhow!(e).context(format!("index source: {}", source.describe())))?;
    let options = PinOptions {
        image_name: args.image_name.clone(),
        require_image: args.require_image,
    };
    let manifest = pinning::pin(&index, &args.package, &selector, &options)
        .map_err(|e| anyhow::anyhow!(e).context(format!("pinning {}", args.package)))?;
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    Ok(())
}
