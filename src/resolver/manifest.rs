//! Manifest loading: `pyproject.toml` and `package.json` → declarations.
//!
//! Every dependency list that the language toolchain resolves is read:
//! - Python: `[project] dependencies`, `[project] optional-dependencies.*`,
//!   and `[dependency-groups].*` (PEP 735);
//! - npm: `dependencies`, `devDependencies`, `optionalDependencies`, and
//!   `peerDependencies`.
//!
//! Loading also performs the declaration-form validation (forbidden Git /
//! artifact URL / local path references) so an invalid manifest fails before
//! any index access.

// The resolver error enum carries rich context strings for actionable
// messages and flows through `anyhow` at the CLI boundary, where its
// stack size is not performance-relevant.
#![allow(clippy::result_large_err)]
use std::path::Path;

use super::declaration::{parse_npm, parse_python, Declaration};
use super::error::ResolverError;

/// Which ecosystem a manifest belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecosystem {
    Python,
    Npm,
}

impl Ecosystem {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Npm => "npm",
        }
    }

    /// Accepted major-only declaration forms, for error guidance.
    pub fn accepted_forms(self) -> &'static str {
        match self {
            Self::Python => super::declaration::PYTHON_ACCEPTED_FORMS,
            Self::Npm => super::declaration::NPM_ACCEPTED_FORMS,
        }
    }
}

/// A loaded manifest and its parsed declarations.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub path: std::path::PathBuf,
    pub ecosystem: Ecosystem,
    pub declarations: Vec<Declaration>,
}

/// Load and validate a `pyproject.toml` or `package.json` manifest.
///
/// The file must be named `pyproject.toml` or `package.json`; the language
/// follows from the manifest kind.
pub fn load_manifest(path: &Path) -> Result<Manifest, ResolverError> {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    match file_name {
        "pyproject.toml" => load_python_manifest(path),
        "package.json" => load_npm_manifest(path),
        other => Err(ResolverError::InvalidManifest {
            path: path.display().to_string(),
            reason: format!(
                "`{other}` is not a supported manifest; expected pyproject.toml or package.json"
            ),
        }),
    }
}

fn read_to_string(path: &Path) -> Result<String, ResolverError> {
    std::fs::read_to_string(path).map_err(|e| ResolverError::InvalidManifest {
        path: path.display().to_string(),
        reason: format!("failed to read: {e}"),
    })
}

fn load_python_manifest(path: &Path) -> Result<Manifest, ResolverError> {
    let content = read_to_string(path)?;
    let doc: toml::Value = content
        .parse()
        .map_err(|e| ResolverError::InvalidManifest {
            path: path.display().to_string(),
            reason: format!("failed to parse TOML: {e}"),
        })?;

    let mut declarations = Vec::new();

    if let Some(deps) = doc
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_array())
    {
        for entry in deps {
            let req = entry
                .as_str()
                .ok_or_else(|| type_error(path, "project.dependencies"))?;
            declarations.push(parse_python(req, "pyproject.toml [project].dependencies")?);
        }
    }

    if let Some(extras) = doc
        .get("project")
        .and_then(|p| p.get("optional-dependencies"))
        .and_then(|d| d.as_table())
    {
        for (extra, list) in extras {
            let list = list.as_array().ok_or_else(|| {
                type_error(path, &format!("[project].optional-dependencies.{extra}"))
            })?;
            for entry in list {
                let req = entry.as_str().ok_or_else(|| {
                    type_error(path, &format!("[project].optional-dependencies.{extra}"))
                })?;
                declarations.push(parse_python(
                    req,
                    &format!("pyproject.toml [project].optional-dependencies.{extra}"),
                )?);
            }
        }
    }

    if let Some(groups) = doc.get("dependency-groups").and_then(|d| d.as_table()) {
        for (group, list) in groups {
            let list = list
                .as_array()
                .ok_or_else(|| type_error(path, &format!("[dependency-groups].{group}")))?;
            for entry in list {
                let req = entry
                    .as_str()
                    .ok_or_else(|| type_error(path, &format!("[dependency-groups].{group}")))?;
                declarations.push(parse_python(
                    req,
                    &format!("pyproject.toml [dependency-groups].{group}"),
                )?);
            }
        }
    }

    Ok(Manifest {
        path: path.to_path_buf(),
        ecosystem: Ecosystem::Python,
        declarations,
    })
}

fn type_error(path: &Path, section: &str) -> ResolverError {
    ResolverError::InvalidManifest {
        path: path.display().to_string(),
        reason: format!("`{section}` must be a list of dependency strings"),
    }
}

fn load_npm_manifest(path: &Path) -> Result<Manifest, ResolverError> {
    let content = read_to_string(path)?;
    let doc: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| ResolverError::InvalidManifest {
            path: path.display().to_string(),
            reason: format!("failed to parse JSON: {e}"),
        })?;

    let mut declarations = Vec::new();
    for section in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        let Some(map) = doc.get(section).and_then(|v| v.as_object()) else {
            continue;
        };
        for (name, value) in map {
            let value = value
                .as_str()
                .ok_or_else(|| ResolverError::InvalidManifest {
                    path: path.display().to_string(),
                    reason: format!("`{section}.{name}` must be a version-range string"),
                })?;
            declarations.push(parse_npm(name, value, &format!("package.json {section}"))?);
        }
    }

    Ok(Manifest {
        path: path.to_path_buf(),
        ecosystem: Ecosystem::Npm,
        declarations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::declaration::Spec;
    use crate::resolver::error::NotMajorOnlyReason;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jumbo-manifest-ut-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn python_manifest_sections_are_all_read() {
        let dir = temp_dir("sections");
        let path = dir.join("pyproject.toml");
        std::fs::write(
            &path,
            r#"
[project]
name = "consumer"
dependencies = [
    "juntai-fuse-api[http]@2",
    "numpy>=1.26",
]

[project.optional-dependencies]
extra = ["demo-alpha>=2,<3"]

[dependency-groups]
dev = ["pytest>=8"]
"#,
        )
        .expect("write manifest");

        let manifest = load_manifest(&path).expect("load");
        assert_eq!(manifest.ecosystem, Ecosystem::Python);
        assert_eq!(manifest.declarations.len(), 4);
        let names: Vec<&str> = manifest
            .declarations
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["juntai-fuse-api", "numpy", "demo-alpha", "pytest"]
        );
        assert_eq!(manifest.declarations[0].spec, Spec::Major(2));
        assert!(manifest.declarations[1].spec.major().is_none()); // third-party
        assert_eq!(manifest.declarations[2].spec.major(), Some(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_manifest_rejects_git_and_wheel_urls() {
        let dir = temp_dir("urls");
        for (tag, dep) in [
            ("git", r#""demo @ git+https://github.com/org/repo.git""#),
            (
                "wheel",
                r#""demo @ https://github.com/org/repo/releases/download/v1/demo-1.0.0-py3-none-any.whl""#,
            ),
            (
                "bare-git",
                r#""git+https://github.com/org/repo.git#egg=demo""#,
            ),
        ] {
            let case_dir = dir.join(tag);
            std::fs::create_dir_all(&case_dir).expect("create case dir");
            let path = case_dir.join("pyproject.toml");
            std::fs::write(&path, format!("[project]\ndependencies = [{dep}]\n"))
                .expect("write manifest");
            let err = load_manifest(&path).unwrap_err();
            assert!(
                err.to_string().contains("forbidden"),
                "{tag}: expected forbidden reference, got: {err}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn npm_manifest_sections_are_all_read() {
        let dir = temp_dir("npm");
        let path = dir.join("package.json");
        std::fs::write(
            &path,
            r#"{
  "name": "consumer",
  "dependencies": {
    "@juntai/demo-kit": "^1",
    "lodash": "^4.17.21"
  },
  "devDependencies": {
    "@zephytiju/vangu-constructs": "1.x"
  },
  "peerDependencies": {
    "react": "*"
  }
}"#,
        )
        .expect("write manifest");

        let manifest = load_manifest(&path).expect("load");
        assert_eq!(manifest.ecosystem, Ecosystem::Npm);
        assert_eq!(manifest.declarations.len(), 4);
        assert_eq!(manifest.declarations[0].name, "@juntai/demo-kit");
        assert_eq!(manifest.declarations[0].spec.major(), Some(1));
        assert_eq!(manifest.declarations[1].name, "lodash");
        assert_eq!(manifest.declarations[2].name, "@zephytiju/vangu-constructs");
        assert_eq!(manifest.declarations[2].spec.major(), Some(1));
        // Third-party wildcard passes through as Other (Unsupported).
        assert_eq!(
            manifest.declarations[3].spec.not_major_only_reason(),
            NotMajorOnlyReason::Unsupported { text: "*".into() }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn npm_manifest_rejects_git_shorthand_and_url() {
        let dir = temp_dir("npm-reject");
        for (tag, name, value) in [
            ("git", "@juntai/kit", "git+https://github.com/org/kit.git"),
            (
                "tarball",
                "@juntai/kit",
                "https://github.com/org/kit/-/kit-1.0.0.tgz",
            ),
            ("file", "@juntai/kit", "file:../kit"),
            ("shorthand", "org/kit", "*"),
        ] {
            let case_dir = dir.join(tag);
            std::fs::create_dir_all(&case_dir).expect("create case dir");
            let path = case_dir.join("package.json");
            std::fs::write(
                &path,
                format!(r#"{{"dependencies": {{"{name}": "{value}"}}}}"#),
            )
            .expect("write manifest");
            let err = load_manifest(&path).unwrap_err();
            assert!(
                err.to_string().contains("forbidden"),
                "{tag}: expected forbidden reference, got: {err}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsupported_manifest_names_are_rejected() {
        let dir = temp_dir("unsupported");
        let path = dir.join("requirements.txt");
        std::fs::write(&path, "numpy\n").expect("write");
        let err = load_manifest(&path).unwrap_err();
        assert!(err.to_string().contains("not a supported manifest"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
