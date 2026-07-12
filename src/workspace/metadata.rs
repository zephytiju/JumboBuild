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
        let content = toml::to_string_pretty(self)
            .context("Failed to serialize workspace metadata")?;
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
        self.workspace.repositories.iter_mut().find(|r| r.name == name)
    }
}
