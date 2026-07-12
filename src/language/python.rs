use anyhow::{Context, Result};
use colored::Colorize;
use std::path::Path;
use walkdir::WalkDir;

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

    fn sync_workspace(&self, workspace_root: &Path, repos: &[RepoInfo], local: bool) -> Result<()> {
        let workspace_toml_path = workspace_root.join("pyproject.toml");

        // Load existing root pyproject.toml or create a fresh one
        let mut doc: toml::Table = if workspace_toml_path.exists() {
            let content = std::fs::read_to_string(&workspace_toml_path)
                .with_context(|| format!("Failed to read {}", workspace_toml_path.display()))?;
            content
                .parse::<toml::Table>()
                .with_context(|| "Failed to parse root pyproject.toml")?
        } else {
            return Ok(());
        };

        // Collect info from each Python repo
        let mut pkg_names: Vec<String> = Vec::new();
        let mut member_paths: Vec<String> = Vec::new();

        for repo in repos {
            let repo_toml_path = workspace_root.join(&repo.path).join("pyproject.toml");
            if !repo_toml_path.exists() {
                continue;
            }
            let repo_content = std::fs::read_to_string(&repo_toml_path)?;
            let repo_doc: toml::Table = repo_content.parse::<toml::Table>()?;

            if let Some(project) = repo_doc.get("project").and_then(|v| v.as_table()) {
                if let Some(name) = project.get("name").and_then(|v| v.as_str()) {
                    pkg_names.push(name.to_string());
                    member_paths.push(repo.path.clone());
                }
            }
        }

        // --- [project] dependencies ---
        let project = doc
            .entry("project")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let project_table = project.as_table_mut().context("[project] is not a table")?;
        let deps: Vec<toml::Value> = pkg_names
            .iter()
            .map(|n| toml::Value::String(n.clone()))
            .collect();
        project_table.insert("dependencies".to_string(), toml::Value::Array(deps));

        // --- [tool.uv.sources] ---
        let tool = doc
            .entry("tool")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let tool_table = tool.as_table_mut().context("[tool] is not a table")?;

        let uv = tool_table
            .entry("uv")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let uv_table = uv.as_table_mut().context("[tool.uv] is not a table")?;

        // Generate sources for each Python package
        let mut sources_table = toml::Table::new();
        for repo in repos {
            let repo_toml_path = workspace_root.join(&repo.path).join("pyproject.toml");
            if !repo_toml_path.exists() {
                continue;
            }
            let repo_content = std::fs::read_to_string(&repo_toml_path)?;
            let repo_doc: toml::Table = repo_content.parse::<toml::Table>()?;

            if let Some(project) = repo_doc.get("project").and_then(|v| v.as_table()) {
                if let Some(pkg_name) = project.get("name").and_then(|v| v.as_str()) {
                    if local {
                        let mut source = toml::Table::new();
                        source.insert(
                            "workspace".to_string(),
                            toml::Value::Boolean(true),
                        );
                        sources_table.insert(
                            pkg_name.to_string(),
                            toml::Value::Table(source),
                        );
                    } else if let Some(remote) = &repo.remote {
                        let mut source = toml::Table::new();
                        let mut git_table = toml::Table::new();
                        git_table.insert(
                            "url".to_string(),
                            toml::Value::String(remote.clone()),
                        );
                        source.insert("git".to_string(), toml::Value::Table(git_table));
                        sources_table.insert(
                            pkg_name.to_string(),
                            toml::Value::Table(source),
                        );
                    }
                }
            }
        }
        uv_table.insert("sources".to_string(), toml::Value::Table(sources_table));

        // --- [tool.uv.workspace] members & exclude ---
        let mut ws_table = toml::Table::new();

        // members: explicit list of Python project paths
        let members: Vec<toml::Value> = member_paths
            .iter()
            .map(|p| toml::Value::String(p.clone()))
            .collect();
        ws_table.insert("members".to_string(), toml::Value::Array(members));

        // exclude: non-Python repo paths
        let exclude_paths: Vec<String> = repos
            .iter()
            .filter(|repo| {
                let repo_toml = workspace_root.join(&repo.path).join("pyproject.toml");
                !repo_toml.exists()
            })
            .map(|repo| repo.path.clone())
            .collect();
        if !exclude_paths.is_empty() {
            let exclude: Vec<toml::Value> = exclude_paths
                .iter()
                .map(|p| toml::Value::String(p.clone()))
                .collect();
            ws_table.insert("exclude".to_string(), toml::Value::Array(exclude));
        }

        uv_table.insert("workspace".to_string(), toml::Value::Table(ws_table));

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

    fn clean(&self, repo_path: &Path) -> Result<()> {
        let dir_patterns = &["__pycache__", ".pytest_cache", "dist", "build", "htmlcov", ".mypy_cache", ".ruff_cache"];
        let file_patterns = &[".coverage"];
        let glob_suffixes = &[".egg-info"];

        let mut cleaned = 0u32;

        // Remove matching directories (walk bottom-up to handle nested __pycache__)
        for entry in WalkDir::new(repo_path).into_iter().filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy();

            if entry.file_type().is_dir() {
                if dir_patterns.contains(&name.as_ref()) || glob_suffixes.iter().any(|s| name.ends_with(s)) {
                    if std::fs::remove_dir_all(entry.path()).is_ok() {
                        println!("    {} Removed {}", "-".dimmed(), entry.path().display());
                        cleaned += 1;
                    }
                }
            } else if entry.file_type().is_file() {
                if file_patterns.contains(&name.as_ref()) {
                    if std::fs::remove_file(entry.path()).is_ok() {
                        println!("    {} Removed {}", "-".dimmed(), entry.path().display());
                        cleaned += 1;
                    }
                }
            }
        }

        if cleaned > 0 {
            println!("  {} Cleaned {} Python artifact(s) in {}", "✓".green(), cleaned, repo_path.display());
        }
        Ok(())
    }
}
