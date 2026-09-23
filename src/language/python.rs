use anyhow::{Context, Result};
use colored::Colorize;
use std::collections::BTreeMap;
use std::path::Path;
use walkdir::WalkDir;

use super::LanguageSupport;
use crate::utils::runner::run_steps;
use crate::workspace::metadata::RepoInfo;

/// Python language support.
pub struct PythonSupport;

const ACTIVATE_WORKSPACE_ENVIRONMENT: (&str, &str) =
    (". .venv/bin/activate", "Activating workspace environment");

impl LanguageSupport for PythonSupport {
    fn name(&self) -> &str {
        "python"
    }

    fn detect(&self, repo_path: &Path) -> bool {
        repo_path.join("pyproject.toml").exists()
    }

    fn sync_workspace(&self, workspace_root: &Path, repos: &[RepoInfo]) -> Result<()> {
        let workspace_toml_path = workspace_root.join("pyproject.toml");
        let content = std::fs::read_to_string(&workspace_toml_path)
            .with_context(|| format!("Failed to read {}", workspace_toml_path.display()))?;
        let mut doc: toml::Table = content
            .parse::<toml::Table>()
            .with_context(|| "Failed to parse root pyproject.toml")?;

        let previous_sources = managed_source_names(&doc);
        let mut generated_sources = BTreeMap::new();
        let mut member_paths = Vec::new();
        let mut exclude_paths = Vec::new();

        for repo in repos {
            let repo_path = workspace_root.join(&repo.path);
            let repo_toml_path = repo_path.join("pyproject.toml");
            let package_name = if repo_toml_path.exists() {
                let repo_content = std::fs::read_to_string(&repo_toml_path)?;
                let repo_doc: toml::Table = repo_content.parse::<toml::Table>()?;
                repo_doc
                    .get("project")
                    .and_then(|value| value.as_table())
                    .and_then(|project| project.get("name"))
                    .and_then(|name| name.as_str())
                    .map(str::to_owned)
            } else if repo.is_python_package() {
                // Absent Python repositories keep their recorded
                // distribution name as the git-source fallback. Repositories
                // recorded for another ecosystem (node) never enter the uv
                // workspace: they stay excluded when present and ignored
                // when absent.
                repo.package.clone()
            } else {
                None
            };

            if let Some(package_name) = package_name {
                let source = if repo_path.exists() {
                    member_paths.push(repo.path.clone());
                    let mut source = toml::Table::new();
                    source.insert("workspace".to_string(), toml::Value::Boolean(true));
                    Some(source)
                } else {
                    repo.remote.as_ref().map(|remote| {
                        let mut source = toml::Table::new();
                        source.insert("git".to_string(), toml::Value::String(remote.clone()));
                        source
                    })
                };

                if let Some(source) = source {
                    generated_sources.insert(package_name, toml::Value::Table(source));
                }
            } else if repo_path.exists() {
                exclude_paths.push(repo.path.clone());
            }
        }

        let tool = doc
            .entry("tool")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let tool_table = tool.as_table_mut().context("[tool] is not a table")?;
        let uv = tool_table
            .entry("uv")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let uv_table = uv.as_table_mut().context("[tool.uv] is not a table")?;
        let mut sources_table = uv_table
            .get("sources")
            .and_then(|value| value.as_table())
            .cloned()
            .unwrap_or_default();
        for package_name in previous_sources {
            sources_table.remove(&package_name);
        }
        for (package_name, source) in &generated_sources {
            sources_table.insert(package_name.clone(), source.clone());
        }
        uv_table.insert("sources".to_string(), toml::Value::Table(sources_table));

        let mut ws_table = toml::Table::new();
        ws_table.insert(
            "members".to_string(),
            toml::Value::Array(member_paths.into_iter().map(toml::Value::String).collect()),
        );
        if !exclude_paths.is_empty() {
            ws_table.insert(
                "exclude".to_string(),
                toml::Value::Array(exclude_paths.into_iter().map(toml::Value::String).collect()),
            );
        }
        uv_table.insert("workspace".to_string(), toml::Value::Table(ws_table));

        let jumbo = tool_table
            .entry("jumbo")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let jumbo_table = jumbo
            .as_table_mut()
            .context("[tool.jumbo] is not a table")?;
        jumbo_table.insert(
            "workspace_sources".to_string(),
            toml::Value::Array(
                generated_sources
                    .keys()
                    .cloned()
                    .map(toml::Value::String)
                    .collect(),
            ),
        );

        // Write back
        let new_content =
            toml::to_string_pretty(&doc).context("Failed to serialize pyproject.toml")?;
        std::fs::write(&workspace_toml_path, new_content)?;

        Ok(())
    }

    fn build(&self, workspace_root: &Path, repo_path: &Path) -> Result<()> {
        // Lockfiles and environments belong to each Python project, not the workspace root.
        run_steps(
            &[
                ("uv lock --upgrade", "Updating lockfile"),
                ("uv sync", "Syncing environment metadata"),
            ],
            repo_path,
        )?;
        run_steps(&[ACTIVATE_WORKSPACE_ENVIRONMENT], workspace_root)?;
        // uv build runs in the repo directory
        run_steps(&[("uv build", "Running python build")], repo_path)
    }

    fn test(&self, workspace_root: &Path, repo_path: &Path) -> Result<()> {
        run_steps(
            &[
                ("uv lock --upgrade", "Updating lockfile"),
                ("uv sync", "Syncing environment metadata"),
            ],
            repo_path,
        )?;
        run_steps(&[ACTIVATE_WORKSPACE_ENVIRONMENT], workspace_root)?;
        run_steps(
            &[
                ("uv build", "Running python build"),
                ("uv run pytest -v", "Executing rigorous testing suite"),
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
            repo_path,
        )?;
        run_steps(&[ACTIVATE_WORKSPACE_ENVIRONMENT], workspace_root)?;
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
            repo_path,
        )?;
        run_steps(&[ACTIVATE_WORKSPACE_ENVIRONMENT], workspace_root)?;
        run_steps(
            &[
                ("uv build", "Running python build"),
                ("uv run pytest -v", "Executing rigorous testing suite"),
                ("ruff check .", "Validating strict rule compliance checks"),
            ],
            repo_path,
        )
    }

    fn clean(&self, repo_path: &Path) -> Result<()> {
        let dir_patterns = &[
            "__pycache__",
            ".pytest_cache",
            "dist",
            "build",
            "htmlcov",
            ".mypy_cache",
            ".ruff_cache",
        ];
        let file_patterns = &[".coverage"];
        let glob_suffixes = &[".egg-info"];

        let mut cleaned = 0u32;

        // Remove matching directories (walk bottom-up to handle nested __pycache__)
        for entry in WalkDir::new(repo_path).into_iter().filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy();

            if entry.file_type().is_dir() {
                if (dir_patterns.contains(&name.as_ref())
                    || glob_suffixes.iter().any(|s| name.ends_with(s)))
                    && std::fs::remove_dir_all(entry.path()).is_ok()
                {
                    println!("    {} Removed {}", "-".dimmed(), entry.path().display());
                    cleaned += 1;
                }
            } else if entry.file_type().is_file()
                && file_patterns.contains(&name.as_ref())
                && std::fs::remove_file(entry.path()).is_ok()
            {
                println!("    {} Removed {}", "-".dimmed(), entry.path().display());
                cleaned += 1;
            }
        }

        if cleaned > 0 {
            println!(
                "  {} Cleaned {} Python artifact(s) in {}",
                "✓".green(),
                cleaned,
                repo_path.display()
            );
        }
        Ok(())
    }
}

fn managed_source_names(doc: &toml::Table) -> Vec<String> {
    doc.get("tool")
        .and_then(|value| value.as_table())
        .and_then(|tool| tool.get("jumbo"))
        .and_then(|value| value.as_table())
        .and_then(|jumbo| jumbo.get("workspace_sources"))
        .and_then(|value| value.as_array())
        .map(|sources| {
            sources
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::ACTIVATE_WORKSPACE_ENVIRONMENT;

    #[test]
    fn activation_command_runs_under_posix_sh() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!(
            "jumbo-python-activation-{}-{unique}",
            std::process::id()
        ));
        let activate_script = workspace.join(".venv/bin/activate");
        fs::create_dir_all(activate_script.parent().expect("activation parent exists"))
            .expect("create activation directory");
        fs::write(
            &activate_script,
            "VIRTUAL_ENV=workspace; export VIRTUAL_ENV\n",
        )
        .expect("write activation script");

        let status = Command::new("sh")
            .arg("-c")
            .arg(ACTIVATE_WORKSPACE_ENVIRONMENT.0)
            .current_dir(&workspace)
            .status()
            .expect("POSIX sh should start");

        fs::remove_dir_all(&workspace).expect("remove activation fixture");
        assert!(
            status.success(),
            "POSIX sh should source the activation script"
        );
    }
}
