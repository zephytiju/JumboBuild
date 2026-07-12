use anyhow::{Context, Result};
use serde_json::json;
use std::path::Path;

use super::metadata::JumboToml;

/// Generate or update a VSCode .code-workspace file based on workspace metadata.
pub fn generate_vscode_workspace(workspace_root: &Path, metadata: &JumboToml) -> Result<()> {
    let name = &metadata.workspace.name;
    let file_name = format!("{}.code-workspace", name);
    let file_path = workspace_root.join(&file_name);

    // Root folder entry for the entire workspace
    let mut folders = vec![json!({
        "name": "ALL REPOSITORIES (ROOT)",
        "path": "."
    })];

    // Add each repository as a folder
    for repo in &metadata.workspace.repositories {
        folders.push(json!({
            "path": repo.path
        }));
    }

    // Build settings from IDE config
    let settings = match &metadata.workspace.ide {
        Some(ide) => json!({
            "git.autoRepositoryDetection": ide.git_auto_repo_detection,
            "git.repositoryScanMaxDepth": ide.git_repo_scan_max_depth
        }),
        None => json!({}),
    };

    let workspace_json = json!({
        "folders": folders,
        "settings": settings
    });

    let content = serde_json::to_string_pretty(&workspace_json)
        .context("Failed to serialize VSCode workspace")?;
    std::fs::write(&file_path, content)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    println!("  Generated VSCode workspace: {}", file_name);
    Ok(())
}
