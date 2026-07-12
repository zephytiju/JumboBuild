pub mod detection;
pub mod metadata;
pub mod vscode;

use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::language::{detect_language, get_registry};
use metadata::{IdeConfig, JumboToml, RepoInfo, WorkspaceConfig};

/// Create a new workspace at the given path.
/// If `import_existing` is true, scan the `projects/` folder for existing repositories.
pub fn create_workspace(path: &Path, name: &str, import_existing: bool) -> Result<()> {
    detection::ensure_not_in_workspace()?;

    let projects_dir = path.join("projects");
    std::fs::create_dir_all(&projects_dir)
        .with_context(|| format!("Failed to create projects directory at {}", projects_dir.display()))?;

    let mut metadata = JumboToml {
        workspace: WorkspaceConfig {
            name: name.to_string(),
            repositories: Vec::new(),
            ide: Some(IdeConfig { ide_type: "vscode".to_string(), ..Default::default() }),
        },
    };

    if import_existing && projects_dir.exists() {
        metadata = scan_projects_dir(path, metadata)?;
    }

    metadata.save(path)?;
    println!("{} Workspace '{}' created at {}", "✓".green().bold(), name, path.display());

    // Generate VSCode workspace file if IDE is vscode
    if let Some(ref ide) = metadata.workspace.ide {
        if ide.ide_type == "vscode" {
            vscode::generate_vscode_workspace(path, &metadata)?;
        }
    }

    Ok(())
}

/// Clone a repository into the workspace.
pub fn use_repository(workspace_root: &Path, repo_url: &str) -> Result<()> {
    let mut metadata = JumboToml::load(workspace_root)?;

    // Extract repo name from URL
    let repo_name = extract_repo_name(repo_url)?;
    let target_path = format!("projects/{}", repo_name);
    let abs_target = workspace_root.join(&target_path);

    if abs_target.exists() {
        bail!("Repository directory already exists: {}", abs_target.display());
    }

    println!("{} Cloning {} into {}...", "➔".blue().bold(), repo_url, target_path);
    let repo = git2::Repository::clone(repo_url, &abs_target)
        .with_context(|| format!("Failed to clone {}", repo_url))?;

    // Get remote URL
    let remote_url = repo
        .find_remote("origin")
        .ok()
        .and_then(|r| r.url().map(String::from));

    let repo_info = RepoInfo {
        name: repo_name.clone(),
        path: target_path,
        remote: remote_url,
    };

    metadata.workspace.repositories.push(repo_info);
    metadata.save(workspace_root)?;

    println!("{} Repository '{}' added to workspace", "✓".green().bold(), repo_name);

    // Update VSCode workspace
    if let Some(ref ide) = metadata.workspace.ide {
        if ide.ide_type == "vscode" {
            vscode::generate_vscode_workspace(workspace_root, &metadata)?;
        }
    }

    Ok(())
}

/// Import an existing folder as a repository in the workspace.
pub fn import_project(workspace_root: &Path, project_path: &str) -> Result<()> {
    let mut metadata = JumboToml::load(workspace_root)?;

    let abs_path = workspace_root.join(project_path);
    if !abs_path.exists() {
        bail!("Path does not exist: {}", abs_path.display());
    }

    let name = abs_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(String::from)
        .context("Invalid project path")?;

    if metadata.find_repo(&name).is_some() {
        bail!("Repository '{}' is already registered in the workspace", name);
    }

    // Detect git remote if available
    let remote = git2::Repository::open(&abs_path)
        .ok()
        .and_then(|repo| {
            repo.find_remote("origin")
                .ok()
                .and_then(|r| r.url().map(String::from))
        });

    let repo_info = RepoInfo {
        name: name.clone(),
        path: project_path.to_string(),
        remote,
    };

    metadata.workspace.repositories.push(repo_info);
    metadata.save(workspace_root)?;

    println!("{} Project '{}' imported into workspace", "✓".green().bold(), name);

    // Update VSCode workspace
    if let Some(ref ide) = metadata.workspace.ide {
        if ide.ide_type == "vscode" {
            vscode::generate_vscode_workspace(workspace_root, &metadata)?;
        }
    }

    Ok(())
}

/// Sync workspace locally: detect languages, update pyproject.toml, etc.
pub fn sync_workspace(workspace_root: &Path, _local: bool) -> Result<()> {
    let mut metadata = JumboToml::load(workspace_root)?;

    // Scan projects directory for current repositories
    let projects_dir = workspace_root.join("projects");
    if !projects_dir.exists() {
        bail!("projects/ directory not found in workspace");
    }

    let current_repos = scan_projects_dir(workspace_root, metadata.clone())?;

    // Detect removed repos and update their config to use git remote
    let removed: Vec<_> = metadata
        .workspace
        .repositories
        .iter()
        .filter(|r| !current_repos.workspace.repositories.iter().any(|c| c.name == r.name))
        .cloned()
        .collect();

    for repo in &removed {
        if let Some(remote) = &repo.remote {
            println!(
                "  {} Repository '{}' removed from projects, switching to remote: {}",
                "!".yellow().bold(),
                repo.name,
                remote
            );
        }
    }

    // Update metadata with current state
    metadata.workspace.repositories = current_repos.workspace.repositories;

    // Run language-specific sync with all repos (each language filters its own)
    let registry = get_registry();
    for lang in &registry {
        // Collect repos that this language supports
        let matching_repos: Vec<_> = metadata
            .workspace
            .repositories
            .iter()
            .filter(|repo| {
                let repo_path = workspace_root.join(&repo.path);
                lang.detect(&repo_path)
            })
            .cloned()
            .collect();

        // Also include repos that are NOT detected by any language (for exclude lists)
        let all_repos = &metadata.workspace.repositories;

        if !matching_repos.is_empty() {
            println!("  Syncing {} projects", lang.name());
            if let Err(e) = lang.sync_workspace(workspace_root, all_repos, true) {
                eprintln!("  {} Failed to sync {}: {}", "✗".red(), lang.name(), e);
            }
        }
    }

    metadata.save(workspace_root)?;
    println!("{} Workspace synced", "✓".green().bold());

    // Update VSCode workspace
    if let Some(ref ide) = metadata.workspace.ide {
        if ide.ide_type == "vscode" {
            vscode::generate_vscode_workspace(workspace_root, &metadata)?;
        }
    }

    Ok(())
}

/// Watch the workspace and periodically sync.
pub fn watch_workspace(workspace_root: &Path, interval_secs: u64) -> Result<()> {
    println!(
        "{} Watching workspace (interval: {}s). Press Ctrl+C to stop.",
        "➔".blue().bold(),
        interval_secs
    );
    loop {
        if let Err(e) = sync_workspace(workspace_root, true) {
            eprintln!("{} Sync error: {}", "✗".red(), e);
        }
        thread::sleep(Duration::from_secs(interval_secs));
    }
}

/// Clean build artifacts for all repositories in the workspace.
pub fn clean_workspace(workspace_root: &Path) -> Result<()> {
    let metadata = JumboToml::load(workspace_root)?;
    let registry = get_registry();

    println!(
        "{} Cleaning build artifacts for all repositories...",
        "➔".blue().bold()
    );

    for repo in &metadata.workspace.repositories {
        let repo_path = workspace_root.join(&repo.path);
        if !repo_path.exists() {
            continue;
        }

        if let Some(lang) = detect_language(&registry, &repo_path) {
            println!("  Cleaning {} project: {}", lang.name(), repo.name);
            if let Err(e) = lang.clean(&repo_path) {
                eprintln!("  {} Failed to clean {}: {}", "✗".red(), repo.name, e);
            }
        }
    }

    println!("{} Workspace cleaned", "✓".green().bold());
    Ok(())
}

/// Scan the projects/ directory and build a metadata structure.
fn scan_projects_dir(workspace_root: &Path, mut metadata: JumboToml) -> Result<JumboToml> {
    let projects_dir = workspace_root.join("projects");
    if !projects_dir.exists() {
        return Ok(metadata);
    }

    let mut repos = Vec::new();
    for entry in std::fs::read_dir(&projects_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(String::from)
                .unwrap_or_default();

            if name.is_empty() {
                continue;
            }

            let rel_path = format!("projects/{}", name);

            // Preserve existing remote if already tracked
            let remote = metadata
                .find_repo(&name)
                .and_then(|r| r.remote.clone())
                .or_else(|| {
                    git2::Repository::open(&path)
                        .ok()
                        .and_then(|repo| {
                            repo.find_remote("origin")
                                .ok()
                                .and_then(|r| r.url().map(String::from))
                        })
                });

            repos.push(RepoInfo {
                name,
                path: rel_path,
                remote,
            });
        }
    }

    metadata.workspace.repositories = repos;
    Ok(metadata)
}

/// Extract repository name from a git URL.
fn extract_repo_name(url: &str) -> Result<String> {
    let name = url
        .rsplit('/')
        .next()
        .unwrap_or(url)
        .trim_end_matches(".git")
        .to_string();
    if name.is_empty() {
        bail!("Could not extract repository name from URL: {}", url);
    }
    Ok(name)
}
