use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const METADATA_FILENAME: &str = "jumbo.toml";

/// Top-level structure of jumbo.toml
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JumboToml {
    pub workspace: WorkspaceConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceConfig {
    pub name: String,
    #[serde(default)]
    pub repositories: Vec<RepoInfo>,
    #[serde(default)]
    pub ide: Option<IdeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoInfo {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub remote: Option<String>,
    /// Distribution or package name from the repository's project manifest,
    /// when known (`project.name` in pyproject.toml or `name` in package.json).
    #[serde(default)]
    pub package: Option<String>,
    /// Ecosystem of the recorded `package` name: [`ECOSYSTEM_PYTHON`] or
    /// [`ECOSYSTEM_NODE`]. Legacy metadata without the field defaults to
    /// Python, which is the only ecosystem older versions recorded.
    #[serde(default)]
    pub ecosystem: Option<String>,
}

/// The Python ecosystem identifier in `jumbo.toml` repository entries.
pub const ECOSYSTEM_PYTHON: &str = "python";
/// The Node ecosystem identifier in `jumbo.toml` repository entries.
pub const ECOSYSTEM_NODE: &str = "node";

impl RepoInfo {
    /// Whether the recorded `package` name is a Python distribution name.
    /// Legacy entries without an ecosystem are Python by construction.
    pub fn is_python_package(&self) -> bool {
        self.ecosystem
            .as_deref()
            .is_none_or(|ecosystem| ecosystem == ECOSYSTEM_PYTHON)
    }

    /// Whether the recorded `package` name is an npm package name.
    #[allow(dead_code)]
    pub fn is_node_package(&self) -> bool {
        self.ecosystem.as_deref() == Some(ECOSYSTEM_NODE)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IdeConfig {
    #[serde(rename = "type")]
    pub ide_type: String,
    /// Enable VSCode git auto repository detection
    #[serde(default = "default_true")]
    pub git_auto_repo_detection: bool,
    /// Max depth for VSCode git repository scanning
    #[serde(default = "default_scan_depth")]
    pub git_repo_scan_max_depth: u32,
}

fn default_true() -> bool {
    true
}

fn default_scan_depth() -> u32 {
    2
}

impl JumboToml {
    /// Load metadata from a jumbo.toml file at the given root path.
    pub fn load(workspace_root: &Path) -> Result<Self> {
        let file_path = workspace_root.join(METADATA_FILENAME);
        let content = std::fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read {}", file_path.display()))?;
        let parsed: JumboToml = toml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", file_path.display()))?;
        Ok(parsed)
    }

    /// Save metadata to jumbo.toml at the given root path.
    pub fn save(&self, workspace_root: &Path) -> Result<()> {
        let file_path = workspace_root.join(METADATA_FILENAME);
        let content =
            toml::to_string_pretty(self).context("Failed to serialize workspace metadata")?;
        std::fs::write(&file_path, content)
            .with_context(|| format!("Failed to write {}", file_path.display()))?;
        Ok(())
    }

    /// Get the absolute path for a repository within the workspace.
    #[allow(dead_code)]
    pub fn repo_abs_path(workspace_root: &Path, repo: &RepoInfo) -> PathBuf {
        workspace_root.join(&repo.path)
    }

    /// Find a repository by name.
    #[allow(dead_code)]
    pub fn find_repo(&self, name: &str) -> Option<&RepoInfo> {
        self.workspace.repositories.iter().find(|r| r.name == name)
    }

    /// Find a mutable repository by name.
    #[allow(dead_code)]
    pub fn find_repo_mut(&mut self, name: &str) -> Option<&mut RepoInfo> {
        self.workspace
            .repositories
            .iter_mut()
            .find(|r| r.name == name)
    }
}
