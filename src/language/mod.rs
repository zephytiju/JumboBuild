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

    /// Sync workspace configuration for this language (e.g., update pyproject.toml).
    fn sync_workspace(&self, workspace_root: &Path, repo: &RepoInfo, local: bool) -> Result<()>;

    /// Run the build pipeline for this language.
    fn build(&self, repo_path: &Path) -> Result<()>;

    /// Run tests for this language.
    fn test(&self, repo_path: &Path) -> Result<()>;

    /// Run formatting for this language.
    fn format(&self, repo_path: &Path) -> Result<()>;

    /// Run release pipeline for this language.
    fn release(&self, repo_path: &Path) -> Result<()>;
}

/// Build the language support registry.
/// To add a new language, add it here.
pub fn get_registry() -> Vec<Box<dyn LanguageSupport>> {
    vec![Box::new(python::PythonSupport)]
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
