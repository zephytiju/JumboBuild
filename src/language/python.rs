use anyhow::{Context, Result};
use std::path::Path;

use super::LanguageSupport;
use crate::utils::runner::run_steps;
use crate::workspace::metadata::RepoInfo;

/// Python language support.
pub struct PythonSupport;

impl LanguageSupport for PythonSupport {
    fn name(&self) -> &str {
        "python"
    }

    fn detect(&self, repo_path: &Path) -> bool {
        repo_path.join("pyproject.toml").exists()
    }

    fn sync_workspace(&self, workspace_root: &Path, repo: &RepoInfo, local: bool) -> Result<()> {
        let workspace_toml_path = workspace_root.join("pyproject.toml");

        // Only sync if workspace has a root pyproject.toml
        if !workspace_toml_path.exists() {
            return Ok(());
        }

        let content = std::fs::read_to_string(&workspace_toml_path)
            .with_context(|| format!("Failed to read {}", workspace_toml_path.display()))?;

        let mut doc: toml::Table = content.parse::<toml::Table>()
            .with_context(|| "Failed to parse root pyproject.toml")?;

        // Ensure [tool.uv.sources] exists
        let tool = doc
            .entry("tool")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let tool_table = tool.as_table_mut().context("[tool] is not a table")?;

        let uv = tool_table
            .entry("uv")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let uv_table = uv.as_table_mut().context("[tool.uv] is not a table")?;

        let sources = uv_table
            .entry("sources")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let sources_table = sources.as_table_mut().context("[tool.uv.sources] is not a table")?;

        // Read the repo's pyproject.toml to get the package name
        let repo_toml_path = workspace_root.join(&repo.path).join("pyproject.toml");
        if repo_toml_path.exists() {
            let repo_content = std::fs::read_to_string(&repo_toml_path)?;
            let repo_doc: toml::Table = repo_content.parse::<toml::Table>()?;

            if let Some(project) = repo_doc.get("project").and_then(|v| v.as_table()) {
                if let Some(pkg_name) = project.get("name").and_then(|v| v.as_str()) {
                    if local {
                        // Set as workspace member (local source)
                        let mut source_table = toml::Table::new();
                        source_table.insert(
                            "workspace".to_string(),
                            toml::Value::Boolean(true),
                        );
                        sources_table.insert(
                            pkg_name.to_string(),
                            toml::Value::Table(source_table),
                        );
                    } else if let Some(remote) = &repo.remote {
                        // Set as git remote source
                        let mut source_table = toml::Table::new();
                        let mut git_table = toml::Table::new();
                        git_table.insert("url".to_string(), toml::Value::String(remote.clone()));
                        source_table.insert("git".to_string(), toml::Value::Table(git_table));
                        sources_table.insert(
                            pkg_name.to_string(),
                            toml::Value::Table(source_table),
                        );
                    }
                }
            }
        }

        // Write back
        let new_content = toml::to_string_pretty(&doc)
            .context("Failed to serialize pyproject.toml")?;
        std::fs::write(&workspace_toml_path, new_content)?;

        Ok(())
    }

    fn build(&self, workspace_root: &Path, repo_path: &Path) -> Result<()> {
        // uv lock/sync run at workspace root
        run_steps(
            &[
                ("uv lock --upgrade", "Updating lockfile"),
                ("uv sync", "Syncing environment metadata"),
            ],
            workspace_root,
        )?;
        // uv build runs in the repo directory
        run_steps(
            &[("uv build", "Running python build")],
            repo_path,
        )
    }

    fn test(&self, workspace_root: &Path, repo_path: &Path) -> Result<()> {
        run_steps(
            &[
                ("uv lock --upgrade", "Updating lockfile"),
                ("uv sync", "Syncing environment metadata"),
            ],
            workspace_root,
        )?;
        run_steps(
            &[
                ("uv build", "Running python build"),
                ("pytest -v", "Executing rigorous testing suite"),
            ],
            repo_path,
        )
    }

    fn format(&self, workspace_root: &Path, repo_path: &Path) -> Result<()> {
        run_steps(
            &[
                ("uv lock --upgrade", "Updating lockfile"),
                ("uv sync", "Syncing environment metadata"),
            ],
            workspace_root,
        )?;
        run_steps(
            &[
                ("uv build", "Running python build"),
                ("ruff format .", "Structuring formats"),
                ("ruff check --fix .", "Applying automated code lint fixes"),
            ],
            repo_path,
        )
    }

    fn release(&self, workspace_root: &Path, repo_path: &Path) -> Result<()> {
        run_steps(
            &[
                ("uv lock --upgrade", "Updating lockfile"),
                ("uv sync", "Syncing environment metadata"),
            ],
            workspace_root,
        )?;
        run_steps(
            &[
                ("uv build", "Running python build"),
                ("pytest -v", "Executing rigorous testing suite"),
                ("ruff check .", "Validating strict rule compliance checks"),
            ],
            repo_path,
        )
    }
}
