use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use colored::Colorize;

use crate::language::{detect_language, get_registry};
use crate::utils::runner::finalize_build;
use crate::workspace::detection::ensure_in_workspace;
use crate::workspace::metadata::{JumboToml, RepoInfo};

#[derive(Args)]
pub struct BuildArgs {
    #[command(subcommand)]
    pub action: Option<BuildAction>,

    /// Reproduce the pinned build this buildId refers to instead of
    /// building the current project: resolve the record, verify the
    /// recomputed fingerprint against the recorded one, and materialize
    /// the recorded closure and artifact (sha256-enforced). The
    /// reproduction flags below apply only with --pinned
    #[arg(long, value_name = "BUILD_ID")]
    pub pinned: Option<String>,

    /// Jumbo index location for --pinned: a local clone path or an
    /// https://github.com URL
    /// (default: JUMBO_INDEX_PATH, then JUMBO_INDEX_URL, then the JumboIndex repository)
    #[arg(short, long, value_name = "PATH_OR_URL")]
    pub index: Option<String>,

    /// With --pinned: resolve artifacts by exact file name from a local
    /// directory instead of downloading; the recorded SHA-256 is still
    /// enforced (default: JUMBO_ARTIFACT_DIR when set, otherwise download)
    #[arg(long, value_name = "DIR")]
    pub artifact_dir: Option<PathBuf>,

    /// With --pinned: where the reproduction outputs land (default: ./reproduced)
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,
}

#[derive(Subcommand, Clone)]
pub enum BuildAction {
    /// Run tests
    Test,
    /// Run formatting
    Format,
    /// Run release pipeline
    Release,
    /// Clean build artifacts for the current project
    Clean,
}

/// Detect which registered repository the current working directory belongs to.
///
/// Walks up from the current directory and checks whether it falls under any
/// registered repository path. Returns the matching `RepoInfo` on success.
fn detect_current_repo(workspace_root: &Path, metadata: &JumboToml) -> Result<RepoInfo> {
    let cwd = std::env::current_dir()?;
    let cwd_canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());

    for repo in &metadata.workspace.repositories {
        let repo_path = workspace_root.join(&repo.path);
        let repo_canonical = repo_path
            .canonicalize()
            .unwrap_or_else(|_| repo_path.clone());

        if cwd_canonical.starts_with(&repo_canonical) {
            return Ok(repo.clone());
        }
    }

    bail!(
        "Not inside a registered project directory.\n\
         Current directory: {}\n\
         Run this command from within a project listed in jumbo.toml.",
        cwd.display()
    );
}

/// Execute the build command (default or with a specific action).
///
/// With `--pinned <BUILD_ID>` the command reproduces that build from its
/// index record (the `jumbo reproduce` path — no workspace required) and
/// returns; otherwise it detects the current project from the working
/// directory and runs only against that single repository.
pub fn execute(args: &BuildArgs) -> Result<()> {
    if let Some(build_id) = &args.pinned {
        return super::reproduce::execute(super::reproduce::ReproduceArgs {
            build_id: build_id.clone(),
            package: None,
            index: args.index.clone(),
            artifact_dir: args.artifact_dir.clone(),
            out: args.out.clone(),
        });
    }
    if args.index.is_some() || args.artifact_dir.is_some() || args.out.is_some() {
        bail!("--index, --artifact-dir, and --out apply only to `jumbo build --pinned <BUILD_ID>`");
    }
    let action = args.action.clone();
    let workspace_root = ensure_in_workspace()?;
    let metadata = JumboToml::load(&workspace_root)?;
    let registry = get_registry();

    let repo = detect_current_repo(&workspace_root, &metadata)?;
    let repo_path: PathBuf = workspace_root.join(&repo.path);

    let lang = detect_language(&registry, &repo_path).ok_or_else(|| {
        anyhow::anyhow!("No language support detected for project '{}'", repo.name)
    })?;

    println!(
        "\n{} Building {} project: {}",
        "➔".blue().bold(),
        lang.name(),
        repo.name
    );

    let result = (|| -> Result<()> {
        // Cleaning has no resolution or build side effects.
        if matches!(action, Some(BuildAction::Clean)) {
            return lang.clean(&repo_path);
        }
        let plan = crate::workspace::local::BuildPlan::new(&workspace_root, &repo_path)?;
        // Keep every reachable manifest overlay alive: uv resolves the whole
        // workspace and npm links transitively to the producer's installed tree.
        let mut guards = Vec::new();
        for &index in &plan.order {
            guards.push(plan.prepare(index)?);
        }
        crate::workspace::local::build_dependencies(&plan, &workspace_root)?;
        let result = match &action {
            Some(BuildAction::Test) => lang.test(&workspace_root, &repo_path),
            Some(BuildAction::Format) => lang.format(&workspace_root, &repo_path),
            Some(BuildAction::Release) => lang.release(&workspace_root, &repo_path),
            _ => lang.build(&workspace_root, &repo_path),
        };
        if result.is_ok() {
            plan.finish()?;
        }
        // Explicit restoration reports an I/O failure as a failed build.
        for guard in &guards {
            guard.restore()?;
        }
        result
    })();

    if let Err(e) = result {
        eprintln!("  {} {} failed: {}", "✗".red(), repo.name, e);
        finalize_build(false);
    }

    finalize_build(true);
}

/// Execute test command (shortcut for build test).
pub fn execute_test() -> Result<()> {
    execute(&BuildArgs {
        action: Some(BuildAction::Test),
        pinned: None,
        index: None,
        artifact_dir: None,
        out: None,
    })
}

/// Execute format command (shortcut for build format).
pub fn execute_format() -> Result<()> {
    execute(&BuildArgs {
        action: Some(BuildAction::Format),
        pinned: None,
        index: None,
        artifact_dir: None,
        out: None,
    })
}

/// Execute release command (shortcut for build release).
pub fn execute_release() -> Result<()> {
    execute(&BuildArgs {
        action: Some(BuildAction::Release),
        pinned: None,
        index: None,
        artifact_dir: None,
        out: None,
    })
}

/// Execute clean command (shortcut for build clean).
pub fn execute_clean() -> Result<()> {
    execute(&BuildArgs {
        action: Some(BuildAction::Clean),
        pinned: None,
        index: None,
        artifact_dir: None,
        out: None,
    })
}
