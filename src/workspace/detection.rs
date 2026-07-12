use anyhow::{bail, Result};
use std::path::PathBuf;

use super::metadata::METADATA_FILENAME;

/// Find the workspace root by searching upward for jumbo.toml.
pub fn find_workspace_root() -> Result<Option<PathBuf>> {
    let current = std::env::current_dir()?;
    let mut dir = Some(current.as_path());
    while let Some(d) = dir {
        if d.join(METADATA_FILENAME).exists() {
            return Ok(Some(d.to_path_buf()));
        }
        dir = d.parent();
    }
    Ok(None)
}

/// Ensure we are inside a workspace. Returns the workspace root path.
pub fn ensure_in_workspace() -> Result<PathBuf> {
    match find_workspace_root()? {
        Some(root) => Ok(root),
        None => bail!("Not inside a Jumbo workspace. Run 'jumbo workspace create' first."),
    }
}

/// Ensure we are NOT inside another workspace (for create command).
pub fn ensure_not_in_workspace() -> Result<()> {
    if let Some(root) = find_workspace_root()? {
        bail!(
            "Cannot create a workspace inside another workspace.\n\
             Existing workspace found at: {}",
            root.display()
        );
    }
    Ok(())
}
