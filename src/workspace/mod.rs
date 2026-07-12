pub mod detection;
pub mod metadata;
pub mod vscode;

use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::language::{detect_language, get_registry};
use crate::utils::runner::run_steps;
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

    // Use RepoBuilder with credential callbacks so that SSH agent,
    // default SSH keys, and the system git credential helper are consulted.
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(|url, username_from_url, allowed_types| {
        // Try SSH agent first
        if allowed_types.contains(git2::CredentialType::SSH_KEY) {
            let user = username_from_url.unwrap_or("git");
            if let Ok(cred) = git2::Cred::ssh_key_from_agent(user) {
                return Ok(cred);
            }
        }
        // Try default SSH key (~/.ssh/id_rsa, etc.)
        if allowed_types.contains(git2::CredentialType::SSH_KEY) {
            let user = username_from_url.unwrap_or("git");
            let home = std::env::var("HOME").unwrap_or_default();
            let key_path = std::path::Path::new(&home).join(".ssh").join("id_rsa");
            if key_path.exists() {
                if let Ok(cred) = git2::Cred::ssh_key(user, None, &key_path, None) {
                    return Ok(cred);
                }
            }
            // Also try id_ed25519
            let ed_key_path = std::path::Path::new(&home).join(".ssh").join("id_ed25519");
            if ed_key_path.exists() {
                if let Ok(cred) = git2::Cred::ssh_key(user, None, &ed_key_path, None) {
                    return Ok(cred);
                }
            }
        }
        // Try username/password (for HTTPS with credential helpers)
        if allowed_types.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
            if let Ok(cred) = git2::Cred::credential_helper(
                &git2::Config::open_default().unwrap(),
                url,
                username_from_url,
            ) {
                return Ok(cred);
            }
        }
        // Fall back to default
        git2::Cred::default()
    });

    let mut fetch_opts = git2::FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);

    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(fetch_opts);

    let repo = builder.clone(repo_url, &abs_target)
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

/// Import a single project by name, looking it up under `projects/` in the workspace.
pub fn import_project(workspace_root: &Path, project_name: &str) -> Result<()> {
    let mut metadata = JumboToml::load(workspace_root)?;

    let rel_path = format!("projects/{}", project_name);
    let abs_path = workspace_root.join(&rel_path);
    if !abs_path.exists() {
        bail!("Project not found at {}", abs_path.display());
    }

    if metadata.find_repo(project_name).is_some() {
        bail!("Repository '{}' is already registered in the workspace", project_name);
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
        name: project_name.to_string(),
        path: rel_path,
        remote,
    };

    metadata.workspace.repositories.push(repo_info);
    metadata.save(workspace_root)?;

    println!("{} Project '{}' imported into workspace", "✓".green().bold(), project_name);

    // Update VSCode workspace
    if let Some(ref ide) = metadata.workspace.ide {
        if ide.ide_type == "vscode" {
            vscode::generate_vscode_workspace(workspace_root, &metadata)?;
        }
    }

    Ok(())
}

/// Import all folders under `projects/` that are not yet registered in the workspace.
pub fn import_all_projects(workspace_root: &Path) -> Result<()> {
    let mut metadata = JumboToml::load(workspace_root)?;

    let projects_dir = workspace_root.join("projects");
    if !projects_dir.exists() {
        bail!("projects/ directory not found in workspace");
    }

    let mut imported = 0u32;
    for entry in std::fs::read_dir(&projects_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from)
            .unwrap_or_default();

        if name.is_empty() {
            continue;
        }

        // Skip already registered repositories
        if metadata.find_repo(&name).is_some() {
            continue;
        }

        let rel_path = format!("projects/{}", name);

        // Detect git remote if available
        let remote = git2::Repository::open(&path)
            .ok()
            .and_then(|repo| {
                repo.find_remote("origin")
                    .ok()
                    .and_then(|r| r.url().map(String::from))
            });

        let repo_info = RepoInfo {
            name: name.clone(),
            path: rel_path,
            remote,
        };

        metadata.workspace.repositories.push(repo_info);
        println!("{} Project '{}' imported into workspace", "✓".green().bold(), name);
        imported += 1;
    }

    if imported == 0 {
        println!("{} No new projects to import", "➔".blue().bold());
    } else {
        metadata.save(workspace_root)?;

        // Update VSCode workspace
        if let Some(ref ide) = metadata.workspace.ide {
            if ide.ide_type == "vscode" {
                vscode::generate_vscode_workspace(workspace_root, &metadata)?;
            }
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
        "{} Cleaning build artifacts...",
        "➔".blue().bold()
    );

    // --- Workspace root level artifacts ---
    println!("  Cleaning workspace root...");
    let mut root_cleaned = 0u32;

    // Remove dist/ directory
    let dist_dir = workspace_root.join("dist");
    if dist_dir.exists() {
        if std::fs::remove_dir_all(&dist_dir).is_ok() {
            println!("    {} Removed {}", "-".dimmed(), dist_dir.display());
            root_cleaned += 1;
        }
    }

    // Remove .venv/ directory
    let venv_dir = workspace_root.join(".venv");
    if venv_dir.exists() {
        if std::fs::remove_dir_all(&venv_dir).is_ok() {
            println!("    {} Removed {}", "-".dimmed(), venv_dir.display());
            root_cleaned += 1;
        }
    }

    // Remove *.egg-info/ directories
    if let Ok(entries) = std::fs::read_dir(workspace_root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if entry.path().is_dir() && name_str.ends_with(".egg-info") {
                if std::fs::remove_dir_all(entry.path()).is_ok() {
                    println!("    {} Removed {}", "-".dimmed(), entry.path().display());
                    root_cleaned += 1;
                }
            }
        }
    }

    if root_cleaned == 0 {
        println!("    {} No workspace-level artifacts found", "-".dimmed());
    }

    // Rebuild environment if .venv was removed
    if !venv_dir.exists() && workspace_root.join("pyproject.toml").exists() {
        println!("  Rebuilding environment...");
        if let Err(e) = run_steps(
            &[("uv sync", "Syncing dependencies")],
            workspace_root,
        ) {
            eprintln!("  {} Failed to rebuild environment: {}", "✗".red(), e);
        }
    }

    // Remove any stale jumbo entry point from .venv/bin to avoid PATH conflicts
    let venv_jumbo = workspace_root.join(".venv").join("bin").join("jumbo");
    if venv_jumbo.exists() {
        let _ = std::fs::remove_file(&venv_jumbo);
    }
    let venv_jumbo_build = workspace_root.join(".venv").join("bin").join("jumbo-build");
    if venv_jumbo_build.exists() {
        let _ = std::fs::remove_file(&venv_jumbo_build);
    }

    // --- Per-repo language-specific artifacts ---
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
