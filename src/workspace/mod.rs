pub mod detection;
pub mod metadata;
pub mod vscode;

use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::io::{self, BufRead, Write};
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
    reconcile_workspace_config(path, &metadata, true)?;
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
        package: python_package_name(&abs_target)?,
    };

    metadata.workspace.repositories.push(repo_info);
    metadata.save(workspace_root)?;
    reconcile_workspace_config(workspace_root, &metadata, true)?;

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
        package: python_package_name(&abs_path)?,
    };

    metadata.workspace.repositories.push(repo_info);
    metadata.save(workspace_root)?;
    reconcile_workspace_config(workspace_root, &metadata, true)?;

    println!("{} Project '{}' imported into workspace", "✓".green().bold(), project_name);

    // Update VSCode workspace
    if let Some(ref ide) = metadata.workspace.ide {
        if ide.ide_type == "vscode" {
            vscode::generate_vscode_workspace(workspace_root, &metadata)?;
        }
    }

    Ok(())
}

/// Remove a project from the workspace: metadata, directory, and IDE configuration.
/// If the project has uncommitted git changes and `force` is false, prompt the user for confirmation.
pub fn remove_project(workspace_root: &Path, project_name: &str, force: bool) -> Result<()> {
    let mut metadata = JumboToml::load(workspace_root)?;

    // Verify the project is registered in the workspace
    let repo = metadata
        .find_repo(project_name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Project '{}' is not registered in the workspace", project_name))?;

    let abs_path = workspace_root.join(&repo.path);

    // Check for pending git changes
    let has_pending_changes = if abs_path.exists() {
        check_pending_git_changes(&abs_path)?
    } else {
        false
    };

    if has_pending_changes && !force {
        eprintln!(
            "{} Project '{}' has uncommitted changes:",
            "!".yellow().bold(),
            project_name
        );
        print_pending_changes_summary(&abs_path)?;
        eprint!("  Are you sure you want to remove it? [y/N] ");
        io::stderr().flush()?;

        let stdin = io::stdin();
        let mut answer = String::new();
        stdin.lock().read_line(&mut answer)?;
        let answer = answer.trim().to_lowercase();

        if answer != "y" && answer != "yes" {
            println!("{} Removal of '{}' cancelled", "➔".blue().bold(), project_name);
            return Ok(());
        }
    }

    // Remove the project directory from disk
    if abs_path.exists() {
        std::fs::remove_dir_all(&abs_path).with_context(|| {
            format!("Failed to remove project directory at {}", abs_path.display())
        })?;
        println!("{} Removed directory {}", "✓".green().bold(), abs_path.display());
    }

    // Remove from metadata
    metadata.workspace.repositories.retain(|r| r.name != project_name);
    metadata.save(workspace_root)?;
    println!(
        "{} Removed project '{}' from workspace metadata",
        "✓".green().bold(),
        project_name
    );

    reconcile_workspace_config(workspace_root, &metadata, true)?;

    // Regenerate VSCode workspace file
    if let Some(ref ide) = metadata.workspace.ide {
        if ide.ide_type == "vscode" {
            vscode::generate_vscode_workspace(workspace_root, &metadata)?;
        }
    }

    println!(
        "{} Project '{}' removed from workspace",
        "✓".green().bold(),
        project_name
    );
    Ok(())
}

/// Check whether a git repository has uncommitted changes (staged, unstaged, or untracked files).
fn check_pending_git_changes(repo_path: &Path) -> Result<bool> {
    let repo = match git2::Repository::open(repo_path) {
        Ok(r) => r,
        Err(_) => return Ok(false), // Not a git repo, treat as clean
    };

    let statuses = repo.statuses(None).with_context(|| {
        format!("Failed to read git status at {}", repo_path.display())
    })?;

    Ok(!statuses.is_empty())
}

/// Print a brief summary of pending git changes to stderr.
fn print_pending_changes_summary(repo_path: &Path) -> Result<()> {
    let repo = match git2::Repository::open(repo_path) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };

    let statuses = repo.statuses(None)?;
    let mut shown = 0u32;
    for entry in statuses.iter().take(10) {
        let status = entry.status();
        let path_str = entry.path().unwrap_or("?");
        let label = if status.is_index_new() || status.is_wt_new() {
            "new"
        } else if status.is_index_modified() || status.is_wt_modified() {
            "modified"
        } else if status.is_index_deleted() || status.is_wt_deleted() {
            "deleted"
        } else if status.is_index_renamed() {
            "renamed"
        } else {
            "changed"
        };
        eprintln!("    {} {} ({})", "-".dimmed(), path_str, label);
        shown += 1;
    }
    let total = statuses.len() as u32;
    if total > shown {
        eprintln!("    {} ... and {} more", "-".dimmed(), total - shown);
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
            package: python_package_name(&path)?,
        };

        metadata.workspace.repositories.push(repo_info);
        println!("{} Project '{}' imported into workspace", "✓".green().bold(), name);
        imported += 1;
    }

    if imported == 0 {
        println!("{} No new projects to import", "➔".blue().bold());
    }

    metadata.save(workspace_root)?;
    reconcile_workspace_config(workspace_root, &metadata, true)?;

    // Update VSCode workspace
    if let Some(ref ide) = metadata.workspace.ide {
        if ide.ide_type == "vscode" {
            vscode::generate_vscode_workspace(workspace_root, &metadata)?;
        }
    }

    Ok(())
}

/// Sync workspace locally: detect languages, update pyproject.toml, etc.
pub fn sync_workspace(workspace_root: &Path, local: bool) -> Result<()> {
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

    let mut repositories = current_repos.workspace.repositories;
    for repo in removed {
        if repo.remote.is_some() && repo.package.is_some() {
            repositories.push(repo);
        }
    }
    metadata.workspace.repositories = repositories;

    reconcile_workspace_config(workspace_root, &metadata, local)?;

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

/// Reconcile the root uv workspace configuration without changing package manifests.
fn reconcile_workspace_config(
    workspace_root: &Path,
    metadata: &JumboToml,
    local: bool,
) -> Result<()> {
    ensure_root_pyproject(workspace_root, &metadata.workspace.name)?;

    for lang in get_registry() {
        if lang.name() == "python" {
            lang.sync_workspace(workspace_root, &metadata.workspace.repositories, local)?;
        }
    }

    Ok(())
}

/// Create the smallest valid uv workspace root while preserving existing user configuration.
fn ensure_root_pyproject(workspace_root: &Path, workspace_name: &str) -> Result<()> {
    let path = workspace_root.join("pyproject.toml");
    let mut doc: toml::Table = if path.exists() {
        std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?
            .parse()
            .with_context(|| format!("Failed to parse {}", path.display()))?
    } else {
        toml::Table::new()
    };

    let project = doc
        .entry("project")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let project_table = project.as_table_mut().context("[project] is not a table")?;
    project_table
        .entry("name")
        .or_insert_with(|| toml::Value::String(workspace_project_name(workspace_name)));
    project_table
        .entry("version")
        .or_insert_with(|| toml::Value::String("0.0.0".to_string()));

    std::fs::write(
        path,
        toml::to_string_pretty(&doc).context("Failed to serialize root pyproject.toml")?,
    )?;
    Ok(())
}

fn workspace_project_name(workspace_name: &str) -> String {
    let normalized: String = workspace_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let normalized = normalized.trim_matches('-');
    if normalized.is_empty()
        || !normalized
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic())
    {
        "jumbo-workspace".to_string()
    } else {
        normalized.to_string()
    }
}

fn python_package_name(repo_path: &Path) -> Result<Option<String>> {
    let path = repo_path.join("pyproject.toml");
    if !path.exists() {
        return Ok(None);
    }

    let doc: toml::Table = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?
        .parse()
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(doc
        .get("project")
        .and_then(|value| value.as_table())
        .and_then(|project| project.get("name"))
        .and_then(|name| name.as_str())
        .map(str::to_owned))
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
                package: python_package_name(&path)?,
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

/// Shell completion helper: returns an `ArgValueCompleter` that lists
/// sub-directories under `projects/` in the current workspace.
/// Used by `import -p` and `remove -p` via `#[arg(add = "...")]`.
pub fn complete_project_names() -> clap_complete::engine::ArgValueCompleter {
    clap_complete::engine::ArgValueCompleter::new(project_name_completer)
}

fn project_name_completer(_current: &std::ffi::OsStr) -> Vec<clap_complete::engine::CompletionCandidate> {
    let Some(root) = detection::find_workspace_root().ok().flatten() else {
        return Vec::new();
    };
    let projects_dir = root.join("projects");
    if !projects_dir.exists() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&projects_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.is_empty() {
                None
            } else {
                Some(clap_complete::engine::CompletionCandidate::new(name))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempWorkspace(std::path::PathBuf);

    impl TempWorkspace {
        fn new() -> Self {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "jumbo-workspace-test-{}-{}",
                std::process::id(),
                suffix
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn metadata() -> JumboToml {
        JumboToml {
            workspace: WorkspaceConfig {
                name: "Example Workspace".to_string(),
                repositories: vec![RepoInfo {
                    name: "package-a-repo".to_string(),
                    path: "projects/package-a-repo".to_string(),
                    remote: Some("https://github.com/example/package-a-repo.git".to_string()),
                    package: Some("package-a".to_string()),
                }],
                ide: None,
            },
        }
    }

    fn root_doc(path: &Path) -> toml::Table {
        std::fs::read_to_string(path.join("pyproject.toml"))
            .unwrap()
            .parse()
            .unwrap()
    }

    #[test]
    fn reconciliation_creates_a_workspace_root_and_switches_sources() {
        let workspace = TempWorkspace::new();
        let project_dir = workspace.0.join("projects/package-a-repo");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("pyproject.toml"),
            "[project]\nname = \"package-a\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        std::fs::write(workspace.0.join("pyproject.toml"), "[tool.custom]\nkeep = true\n")
            .unwrap();

        let metadata = metadata();
        reconcile_workspace_config(&workspace.0, &metadata, true).unwrap();

        let doc = root_doc(&workspace.0);
        assert_eq!(doc["project"]["name"].as_str(), Some("example-workspace"));
        assert_eq!(doc["tool"]["custom"]["keep"].as_bool(), Some(true));
        assert_eq!(
            doc["tool"]["uv"]["workspace"]["members"][0].as_str(),
            Some("projects/package-a-repo")
        );
        assert_eq!(
            doc["tool"]["uv"]["sources"]["package-a"]["workspace"].as_bool(),
            Some(true)
        );

        std::fs::remove_dir_all(project_dir).unwrap();
        reconcile_workspace_config(&workspace.0, &metadata, true).unwrap();

        let doc = root_doc(&workspace.0);
        assert!(doc["tool"]["uv"]["workspace"]["members"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(
            doc["tool"]["uv"]["sources"]["package-a"]["git"].as_str(),
            Some("https://github.com/example/package-a-repo.git")
        );
    }

    #[test]
    fn workspace_project_names_are_valid_and_stable() {
        assert_eq!(workspace_project_name("123 workspace"), "jumbo-workspace");
        assert_eq!(workspace_project_name("My Workspace!"), "my-workspace");
    }
}
