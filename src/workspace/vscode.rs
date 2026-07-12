use anyhow::{Context, Result};
use serde_json::json;
use std::path::Path;

use super::metadata::JumboToml;

/// Generate or update a VSCode .code-workspace file based on workspace metadata.
pub fn generate_vscode_workspace(workspace_root: &Path, metadata: &JumboToml) -> Result<()> {
    let name = &metadata.workspace.name;
    let file_name = format!("{}.code-workspace", name);
    let file_path = workspace_root.join(&file_name);

    let folders: Vec<_> = metadata
        .workspace
        .repositories
        .iter()
        .map(|repo| {
            json!({
                "path": repo.path
            })
        })
        .collect();

    let workspace_json = json!({
        "folders": folders,
        "settings": {}
    });

    let content = serde_json::to_string_pretty(&workspace_json)
        .context("Failed to serialize VSCode workspace")?;
    std::fs::write(&file_path, content)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    println!("  Generated VSCode workspace: {}", file_name);
    Ok(())
}
