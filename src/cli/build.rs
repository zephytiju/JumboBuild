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
}

#[derive(Subcommand)]
pub enum BuildAction {
    /// Run tests
    Test,
    /// Run formatting
    Format,
    /// Run release pipeline
    Release,
    /// Clean build artifacts for all languages
    Clean,
}

/// Detect which registered repository the current working directory belongs to.
///
/// Walks up from the current directory and checks whether it falls under any
/// registered repository path. Returns the matching `RepoInfo` on success.
fn detect_current_repo(workspace_root: &Path, metadata: &JumboToml) -> Result<RepoInfo> {
    let cwd = std::env::current_dir()?;
    let cwd_canonical = cwd
        .canonicalize()
        .unwrap_or_else(|_| cwd.clone());

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
/// Detects the current project from the working directory and runs only
/// against that single repository.
pub fn execute(action: Option<BuildAction>) -> Result<()> {
    let workspace_root = ensure_in_workspace()?;
    let metadata = JumboToml::load(&workspace_root)?;
    let registry = get_registry();

    let repo = detect_current_repo(&workspace_root, &metadata)?;
    let repo_path: PathBuf = workspace_root.join(&repo.path);

    let lang = detect_language(&registry, &repo_path).ok_or_else(|| {
        anyhow::anyhow!(
            "No language support detected for project '{}'",
            repo.name
        )
    })?;

    println!(
        "\n{} Building {} project: {}",
        "➔".blue().bold(),
        lang.name(),
        repo.name
    );

    let result = match &action {
        Some(BuildAction::Clean) => lang.clean(&repo_path),
        None | Some(BuildAction::Test) if matches!(action, None) => {
            lang.build(&workspace_root, &repo_path)
        }
        Some(BuildAction::Test) => lang.test(&workspace_root, &repo_path),
        Some(BuildAction::Format) => lang.format(&workspace_root, &repo_path),
        Some(BuildAction::Release) => lang.release(&workspace_root, &repo_path),
        _ => lang.build(&workspace_root, &repo_path),
    };

    if let Err(e) = result {
        eprintln!("  {} {} failed: {}", "✗".red(), repo.name, e);
        finalize_build(false);
    }

    finalize_build(true);
}

/// Execute test command (shortcut for build test).
pub fn execute_test() -> Result<()> {
    execute(Some(BuildAction::Test))
}

/// Execute format command (shortcut for build format).
pub fn execute_format() -> Result<()> {
    execute(Some(BuildAction::Format))
}

/// Execute release command (shortcut for build release).
pub fn execute_release() -> Result<()> {
    execute(Some(BuildAction::Release))
}

/// Execute clean command (shortcut for build clean).
pub fn execute_clean() -> Result<()> {
    execute(Some(BuildAction::Clean))
}
