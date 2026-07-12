use anyhow::Result;
use clap::{Args, Subcommand};
use colored::Colorize;

use crate::language::{detect_language, get_registry};
use crate::utils::runner::finalize_build;
use crate::workspace::detection::ensure_in_workspace;
use crate::workspace::metadata::JumboToml;

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

/// Execute the build command (default or with a specific action).
pub fn execute(action: Option<BuildAction>) -> Result<()> {
    let workspace_root = ensure_in_workspace()?;
    let metadata = JumboToml::load(&workspace_root)?;
    let registry = get_registry();

    let mut all_success = true;

    for repo in &metadata.workspace.repositories {
        let repo_path = workspace_root.join(&repo.path);
        if !repo_path.exists() {
            eprintln!(
                "  {} Skipping {}: directory not found ({})",
                "!".yellow().bold(),
                repo.name,
                repo.path
            );
            continue;
        }

        if let Some(lang) = detect_language(&registry, &repo_path) {
            println!(
                "\n{} Building {} project: {}",
                "➔".blue().bold(),
                lang.name(),
                repo.name
            );

            let result = match &action {
                Some(BuildAction::Clean) => {
                    lang.clean(&repo_path)
                }
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
                all_success = false;
            }
        } else {
            println!(
                "  {} Skipping {}: no language support detected",
                "-".dimmed(),
                repo.name
            );
        }
    }

    finalize_build(all_success);
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
