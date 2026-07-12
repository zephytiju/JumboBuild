use anyhow::Result;
use clap::{Args, Subcommand};

use crate::workspace::{
    create_workspace, import_project, sync_workspace, use_repository, watch_workspace,
};
use crate::workspace::detection::ensure_in_workspace;

#[derive(Args)]
pub struct WorkspaceArgs {
    #[command(subcommand)]
    pub action: WorkspaceAction,
}

#[derive(Subcommand)]
pub enum WorkspaceAction {
    /// Create a new Jumbo workspace
    Create(CreateArgs),
    /// Clone a repository into the workspace
    Use(UseArgs),
    /// Import an existing local project into the workspace
    Import(ImportArgs),
    /// Sync workspace configuration with local repositories
    Sync(SyncArgs),
    /// Watch workspace and periodically sync
    Watch(WatchArgs),
}

#[derive(Args)]
pub struct CreateArgs {
    /// Import existing projects folder content
    #[arg(short, long)]
    pub import: bool,

    /// Workspace name (defaults to directory name)
    #[arg(short, long)]
    pub name: Option<String>,
}

#[derive(Args)]
pub struct UseArgs {
    /// Git repository URL to clone
    #[arg(short, long)]
    pub repository: String,
}

#[derive(Args)]
pub struct ImportArgs {
    /// Path to the project folder to import (relative to workspace root)
    #[arg(short, long)]
    pub project: String,
}

#[derive(Args)]
pub struct SyncArgs {
    /// Sync with local repository state
    #[arg(short, long)]
    pub local: bool,
}

#[derive(Args)]
pub struct WatchArgs {
    /// Sync interval in seconds
    #[arg(short, long, default_value = "30")]
    pub interval: u64,
}

/// Execute workspace commands.
pub fn execute(args: WorkspaceArgs) -> Result<()> {
    match args.action {
        WorkspaceAction::Create(create_args) => {
            let cwd = std::env::current_dir()?;
            let name = create_args
                .name
                .unwrap_or_else(|| {
                    cwd.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("workspace")
                        .to_string()
                });
            create_workspace(&cwd, &name, create_args.import)?;
        }
        WorkspaceAction::Use(use_args) => {
            let workspace_root = ensure_in_workspace()?;
            use_repository(&workspace_root, &use_args.repository)?;
        }
        WorkspaceAction::Import(import_args) => {
            let workspace_root = ensure_in_workspace()?;
            import_project(&workspace_root, &import_args.project)?;
        }
        WorkspaceAction::Sync(sync_args) => {
            let workspace_root = ensure_in_workspace()?;
            sync_workspace(&workspace_root, sync_args.local)?;
        }
        WorkspaceAction::Watch(watch_args) => {
            let workspace_root = ensure_in_workspace()?;
            watch_workspace(&workspace_root, watch_args.interval)?;
        }
    }
    Ok(())
}
