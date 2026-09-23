pub mod node;
pub mod python;

use anyhow::Result;
use std::path::Path;

use crate::workspace::metadata::RepoInfo;

/// Trait for language-specific build support.
/// Implement this trait to add support for a new language.
pub trait LanguageSupport: Send + Sync {
    /// Human-readable name of the language.
    fn name(&self) -> &str;

    /// Detect whether the given repository path contains a project of this language.
    fn detect(&self, repo_path: &Path) -> bool;

    /// Sync workspace configuration for ALL repositories of this language at once.
    /// This allows generating complete config files (e.g., pyproject.toml) in one pass.
    fn sync_workspace(&self, workspace_root: &Path, repos: &[RepoInfo]) -> Result<()>;

    /// Run the build pipeline for this language.
    fn build(&self, workspace_root: &Path, repo_path: &Path) -> Result<()>;

    /// Run tests for this language.
    fn test(&self, workspace_root: &Path, repo_path: &Path) -> Result<()>;

    /// Run formatting for this language.
    fn format(&self, workspace_root: &Path, repo_path: &Path) -> Result<()>;

    /// Run release pipeline for this language.
    fn release(&self, workspace_root: &Path, repo_path: &Path) -> Result<()>;

    /// Clean build artifacts for this language in the given repository.
    fn clean(&self, repo_path: &Path) -> Result<()>;
}

/// Build the language support registry.
/// To add a new language, add it here.
///
/// Detection order matters: the first backend whose `detect()` matches wins,
/// so a repository carrying both a `pyproject.toml` and a `package.json`
/// builds as Python (existing Python projects may add a package.json for
/// tooling) and pure Node/TypeScript repositories build as Node.
pub fn get_registry() -> Vec<Box<dyn LanguageSupport>> {
    vec![Box::new(python::PythonSupport), Box::new(node::NodeSupport)]
}

/// Detect the language of a repository by trying all registered language supports.
pub fn detect_language<'a>(
    registry: &'a [Box<dyn LanguageSupport>],
    repo_path: &Path,
) -> Option<&'a dyn LanguageSupport> {
    registry
        .iter()
        .find(|lang| lang.detect(repo_path))
        .map(|l| l.as_ref())
}
