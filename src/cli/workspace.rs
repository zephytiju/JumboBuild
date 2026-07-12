use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};

use crate::workspace::{
    clean_workspace, create_workspace, import_all_projects, import_project, sync_workspace,
    use_repository, watch_workspace,
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
    /// Clean build artifacts for all repositories
    Clean,
}

#[derive(Args)]
pub struct CreateArgs {
    /// Workspace name — creates a new folder with this name at the current directory
    pub name: String,

    /// Import existing projects folder content (requires <name> folder to already exist)
    #[arg(short, long)]
    pub import: bool,
}

#[derive(Args)]
pub struct UseArgs {
    /// Git repository URL(s) to clone (can be specified multiple times)
    #[arg(short, long, required = true, num_args = 1..)]
    pub repository: Vec<String>,
}

#[derive(Args)]
pub struct ImportArgs {
    /// Name(s) of project(s) under projects/ to import.
    /// If omitted, all folders under projects/ are imported automatically.
    #[arg(short, long, num_args = 1..)]
    pub project: Vec<String>,
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
            let name = &create_args.name;
            let workspace_path = cwd.join(name);

            if create_args.import {
                // -i: verify <name> folder already exists
                if !workspace_path.exists() || !workspace_path.is_dir() {
                    bail!(
                        "Directory '{}' does not exist at {}",
                        name,
                        cwd.display()
                    );
                }
            } else {
                // Create new folder <name> at cwd
                std::fs::create_dir_all(&workspace_path).with_context(|| {
                    format!(
                        "Failed to create workspace directory at {}",
                        workspace_path.display()
                    )
                })?;
            }

            create_workspace(&workspace_path, name, create_args.import)?;
        }
        WorkspaceAction::Use(use_args) => {
            let workspace_root = ensure_in_workspace()?;
            for repo_url in &use_args.repository {
                use_repository(&workspace_root, repo_url)?;
            }
        }
        WorkspaceAction::Import(import_args) => {
            let workspace_root = ensure_in_workspace()?;
            if import_args.project.is_empty() {
                // No -p: auto-import all folders under projects/
                import_all_projects(&workspace_root)?;
            } else {
                for project_path in &import_args.project {
                    import_project(&workspace_root, project_path)?;
                }
            }
        }
        WorkspaceAction::Sync(sync_args) => {
            let workspace_root = ensure_in_workspace()?;
            sync_workspace(&workspace_root, sync_args.local)?;
        }
        WorkspaceAction::Watch(watch_args) => {
            let workspace_root = ensure_in_workspace()?;
            watch_workspace(&workspace_root, watch_args.interval)?;
        }
        WorkspaceAction::Clean => {
            let workspace_root = ensure_in_workspace()?;
            clean_workspace(&workspace_root)?;
        }
    }
    Ok(())
}
