//! Own-commit discovery and the clean-commit promotion guard
//! (Jumbo Build & Versioning Standard, §2.3).
//!
//! Promotion happens only on clean commits inside a pipeline; local builds
//! on dirty working trees never promote. The guard separates operations
//! that could publish (promotion mode) from pure-local queries: a
//! pure-local `jumbo fingerprint` never inspects tree state, while
//! `--promote` refuses unless the working tree is attributable to exactly
//! one commit.
//!
//! Jumbo-generated output is exempt from "dirty":
//! - everything under `deps/` (injected internal sources and the
//!   `.jumbo-sources.json` marker);
//! - generated lock files (`uv.lock`, `package-lock.json`) — developers
//!   never maintain a repository lock file;
//! - the manifest itself, provided the only difference from the committed
//!   version is jumbo's recorded injection rewrite (verified by
//!   un-injecting both sides with the marker and comparing).
//!
//! Anything else — a modified source file, a stray untracked file — makes
//! the tree dirty and promotion is refused with the offending paths.

use std::path::Path;

use super::error::{format_dirty_paths, FingerprintError};
use super::lockgen::{load_marker, restore_npm_manifest, restore_python_manifest};

/// The repository containing `start` plus its full HEAD commit SHA.
#[derive(Debug, Clone)]
pub struct OwnCommit {
    pub commit: String,
    pub workdir: std::path::PathBuf,
}

/// Discover the git repository at or above `start` and read its HEAD
/// commit as full 40-hex SHA.
pub fn own_commit(start: &Path) -> Result<OwnCommit, FingerprintError> {
    let repo = git2::Repository::discover(start).map_err(|_| FingerprintError::NotARepository {
        start: start.display().to_string(),
    })?;
    let head = repo
        .head()
        .map_err(|_| FingerprintError::NotARepository {
            start: start.display().to_string(),
        })?
        .peel_to_commit()
        .map_err(|e| FingerprintError::NotARepository {
            start: format!("{} (unreadable HEAD: {e})", start.display()),
        })?;
    let commit = head.id().to_string();
    debug_assert_eq!(commit.len(), 40);
    let workdir =
        repo.workdir()
            .map(Path::to_path_buf)
            .ok_or_else(|| FingerprintError::NotARepository {
                start: format!("{} (bare repository)", start.display()),
            })?;
    Ok(OwnCommit { commit, workdir })
}

/// The promotion-relevant working-tree state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeState {
    /// True when every change is attributable to jumbo-generated output.
    pub clean: bool,
    /// Repo-root-relative paths that make the tree dirty (empty when clean).
    pub offending: Vec<String>,
}

/// Evaluate the working tree for promotion at `start` (any path inside
/// the repository). See the module docs for what counts as generated.
pub fn promotion_tree_state(start: &Path) -> Result<TreeState, FingerprintError> {
    let repo = git2::Repository::discover(start).map_err(|_| FingerprintError::NotARepository {
        start: start.display().to_string(),
    })?;
    let workdir = repo
        .workdir()
        .map(Path::to_path_buf)
        .ok_or_else(|| FingerprintError::NotARepository {
            start: format!("{} (bare repository)", start.display()),
        })?
        .to_string_lossy()
        .into_owned();
    let statuses = repo
        .statuses(None)
        .map_err(|e| FingerprintError::NotARepository {
            start: format!("{} (git status failed: {e})", start.display()),
        })?;

    // Manifests whose difference must be proven jumbo-only.
    let mut manifests: Vec<String> = Vec::new();
    let mut offending: Vec<String> = Vec::new();

    for entry in statuses.iter() {
        let Some(path) = entry.path().map(str::to_string) else {
            continue;
        };
        // deps/** — injected sources and the marker.
        if path.starts_with("deps/") || path == "deps" {
            continue;
        }
        // Generated lock files at any depth.
        let base = path.rsplit('/').next().unwrap_or(&path);
        if super::lockgen::LOCK_FILE_NAMES.contains(&base) {
            continue;
        }
        if path.ends_with("/pyproject.toml")
            || path == "pyproject.toml"
            || path.ends_with("/package.json")
            || path == "package.json"
        {
            manifests.push(path);
            continue;
        }
        offending.push(path);
    }

    for manifest in &manifests {
        verify_manifest_jumbo_only(&repo, manifest, &workdir, &mut offending);
    }

    offending.sort();
    offending.dedup();
    Ok(TreeState {
        clean: offending.is_empty(),
        offending,
    })
}

/// Enforce the promotion guard: refuse (typed error) when the tree is not
/// attributable to the HEAD commit; otherwise return the repository plus
/// its full commit SHA.
pub fn ensure_clean_for_promotion(start: &Path) -> Result<OwnCommit, FingerprintError> {
    let own = own_commit(start)?;
    let state = promotion_tree_state(start)?;
    if !state.clean {
        let shown = state.offending.len().min(10);
        return Err(FingerprintError::DirtyTree {
            origin: own.workdir.display().to_string(),
            commit: own.commit,
            paths: format_dirty_paths(&state.offending[..shown]),
            shown,
            total: state.offending.len(),
        });
    }
    Ok(own)
}

/// A changed manifest is acceptable only when un-injecting the current
/// marker from both the working-tree copy and the HEAD copy yields the
/// same bytes — i.e. jumbo's rewrite is the only difference.
fn verify_manifest_jumbo_only(
    repo: &git2::Repository,
    manifest: &str,
    workdir: &str,
    offending: &mut Vec<String>,
) {
    let marker = match load_marker(
        &std::path::PathBuf::from(workdir).join(
            std::path::Path::new(manifest)
                .parent()
                .unwrap_or(std::path::Path::new("")),
        ),
    ) {
        Ok(Some(marker)) => marker.sources,
        _ => Vec::new(),
    };

    let disk_path = std::path::Path::new(workdir).join(manifest);
    let Ok(disk) = std::fs::read_to_string(&disk_path) else {
        offending.push(format!("{manifest} (unreadable)"));
        return;
    };

    let head = head_file_content(repo, manifest);
    let Some(head) = head else {
        offending.push(format!("{manifest} (not committed at HEAD)"));
        return;
    };

    let is_python = manifest.ends_with("pyproject.toml");
    let normalize = |content: &str| -> String {
        if marker.is_empty() {
            return content.to_string();
        }
        if is_python {
            restore_python_manifest(content, &marker)
        } else {
            restore_npm_manifest(content, &marker)
        }
    };

    if normalize(&disk) != normalize(&head) {
        offending.push(format!(
            "{manifest} (differs from HEAD beyond jumbo injection)"
        ));
    }
}

/// Read a file's content as committed at HEAD, if tracked.
fn head_file_content(repo: &git2::Repository, path: &str) -> Option<String> {
    let head = repo.head().ok()?;
    let tree = head.peel_to_tree().ok()?;
    let entry = tree.get_path(std::path::Path::new(path)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    String::from_utf8(blob.content().to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::lockgen::generate_lock_inputs;
    use crate::resolver::index::{Index, IndexRecord, IndexSource};
    use std::process::Command;

    fn record(package: &str, major: u64, version: &str) -> IndexRecord {
        IndexRecord {
            package: package.to_string(),
            major,
            version: version.to_string(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
            fingerprint: None,
            canonical_extract: None,
            artifact_url: None,
            artifact_sha256: None,
            image_digest: None,
            build_id: None,
            pipeline_run: None,
            executor: Some("bootstrap".to_string()),
            timestamp: "2026-09-01T00:00:00Z".to_string(),
        }
    }

    fn fixture_index(tag: &str) -> (std::path::PathBuf, Index) {
        let dir = std::env::temp_dir().join(format!(
            "jumbo-guard-index-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let index_dir = dir.join("index");
        std::fs::create_dir_all(&index_dir).expect("create index dir");
        std::fs::write(
            index_dir.join("demo-alpha.jsonl"),
            serde_json::to_string(&record("demo-alpha", 2, "2.4.0")).unwrap() + "\n",
        )
        .expect("write demo-alpha");
        let index = Index::load(&IndexSource::Local(index_dir.clone())).expect("load index");
        (dir, index)
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("git starts");
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    }

    /// A committed project with a jumbo-declared internal dependency.
    fn committed_project(tag: &str) -> (std::path::PathBuf, Index) {
        let (_index_dir, index) = fixture_index(tag);
        let repo = std::env::temp_dir().join(format!(
            "jumbo-guard-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&repo).expect("create repo dir");
        run_git(&repo, &["init", "-q", "--initial-branch=main"]);
        run_git(&repo, &["config", "user.email", "jumbo@test.invalid"]);
        run_git(&repo, &["config", "user.name", "Jumbo Test"]);
        std::fs::write(
            repo.join("pyproject.toml"),
            "[project]\nname = \"consumer\"\ndependencies = [\"demo-alpha@2\", \"numpy>=1.26\"]\n",
        )
        .expect("write manifest");
        std::fs::write(repo.join("src.py"), "print(\"hello\")\n").expect("write source");
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "initial"]);
        (repo, index)
    }

    #[test]
    fn clean_tree_passes_and_dirty_source_refuses() {
        let (repo, _index) = committed_project("clean-dirty");
        let own = ensure_clean_for_promotion(&repo).expect("clean passes");
        assert_eq!(own.commit.len(), 40);

        // A modified tracked source file refuses promotion.
        std::fs::write(repo.join("src.py"), "print(\"dirty\")\n").expect("edit source");
        let err = ensure_clean_for_promotion(&repo).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("promotion refused"), "got: {msg}");
        assert!(msg.contains("src.py"), "got: {msg}");

        // A stray untracked file refuses promotion too.
        std::fs::write(
            repo.join("pyproject.toml"),
            "[project]\nname = \"consumer\"\ndependencies = [\"demo-alpha@2\", \"numpy>=1.26\"]\n",
        )
        .expect("restore manifest");
        std::fs::write(repo.join("stray.txt"), "untracked\n").expect("write stray");
        let err = ensure_clean_for_promotion(&repo).unwrap_err();
        assert!(err.to_string().contains("stray.txt"), "got: {err}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn jumbo_generated_output_does_not_dirty_promotion() {
        let (repo, index) = committed_project("generated");
        // jumbo lock injects: deps/ appears, the manifest is rewritten,
        // and a lock file is generated — none of it may dirty promotion.
        generate_lock_inputs(&repo.join("pyproject.toml"), &index).expect("generate");
        std::fs::write(
            repo.join("uv.lock"),
            "version = 1\n\n[[package]]\nname = \"demo-alpha\"\nversion = \"2.4.0\"\nsource = { directory = \"deps/demo-alpha\" }\n",
        )
        .expect("write generated lock");
        let state = promotion_tree_state(&repo).expect("state");
        assert!(state.clean, "offending: {:?}", state.offending);
        ensure_clean_for_promotion(&repo).expect("promotion allowed after jumbo lock");

        // A human edit on top of the jumbo rewrite still refuses.
        std::fs::write(
            repo.join("pyproject.toml"),
            "[project]\nname = \"consumer\"\ndependencies = [\"demo-alpha==2.4.0\", \"numpy>=1.27\"]\n\n[tool.uv.sources]\ndemo-alpha = { path = \"deps/demo-alpha\" }\n\n[tool.jumbo]\nlock_sources = [\"demo-alpha\"]\n",
        )
        .expect("human edit");
        let state = promotion_tree_state(&repo).expect("state");
        assert!(!state.clean, "human edit must dirty the tree");
        assert!(
            state
                .offending
                .iter()
                .any(|p| p.contains("beyond jumbo injection")),
            "offending: {:?}",
            state.offending
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn uncommitted_manifest_refuses_promotion() {
        let (repo, _index) = committed_project("uncommitted");
        std::fs::write(
            repo.join("pyproject.toml"),
            "[project]\nname = \"consumer\"\ndependencies = [\"demo-alpha@3\"]\n",
        )
        .expect("edit manifest without injection");
        let state = promotion_tree_state(&repo).expect("state");
        assert!(
            !state.clean,
            "an edited manifest without a jumbo marker is dirty"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn non_repository_has_no_own_commit() {
        let dir = std::env::temp_dir().join(format!(
            "jumbo-guard-norepo-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create dir");
        let err = own_commit(&dir).unwrap_err();
        assert!(err.to_string().contains("no Git repository"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
