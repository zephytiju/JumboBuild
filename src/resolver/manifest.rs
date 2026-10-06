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
    let content = read_to_string(path)?;
    load_manifest_content(path, &content)
}

/// Parse declared inputs without mutating a temporarily injected manifest.
pub fn load_manifest_content(path: &Path, content: &str) -> Result<Manifest, ResolverError> {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    match file_name {
        "pyproject.toml" => load_python_manifest(path, content),
        "package.json" => load_npm_manifest(path, content),
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

fn load_python_manifest(path: &Path, content: &str) -> Result<Manifest, ResolverError> {
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
        for group in groups.keys() {
            for entry in expand_group(groups, group, &mut Vec::new(), path)? {
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

/// Expand a PEP 735 dependency group into its entry list, resolving
/// `{ include-group = "name" }` inline-table references recursively.
///
/// Included groups are substituted in place, so `[dependency-groups].dev`
/// with `[{ include-group = "test" }, { include-group = "lint" }, "build>=1"]`
/// yields test's and lint's entries followed by `build>=1` — the same
/// materialization `uv` performs. Cycles are an error; an inline table that
/// is not exactly one `include-group` string key is an error (PEP 735 allows
/// no other keys).
fn expand_group<'a>(
    groups: &'a toml::map::Map<String, toml::Value>,
    group: &str,
    stack: &mut Vec<String>,
    path: &Path,
) -> Result<Vec<&'a toml::Value>, ResolverError> {
    if stack.iter().any(|seen| seen == group) {
        let chain = stack.join(" -> ");
        return Err(ResolverError::InvalidManifest {
            path: path.display().to_string(),
            reason: format!(
                "[dependency-groups].{group} participates in an include-group cycle ({chain} -> {group})"
            ),
        });
    }
    let list = groups
        .get(group)
        .and_then(|value| value.as_array())
        .ok_or_else(|| type_error(path, &format!("[dependency-groups].{group}")))?;
    let mut entries: Vec<&toml::Value> = Vec::new();
    stack.push(group.to_string());
    for entry in list {
        if let Some(table) = entry.as_table() {
            let included = match (table.len(), table.get("include-group")) {
                (1, Some(value)) => value.as_str(),
                _ => None,
            };
            let Some(included) = included else {
                return Err(ResolverError::InvalidManifest {
                    path: path.display().to_string(),
                    reason: format!(
                        "[dependency-groups].{group}: inline tables must be exactly `{{ include-group = \"name\" }}` (PEP 735)"
                    ),
                });
            };
            entries.extend(expand_group(groups, included, stack, path)?);
        } else {
            entries.push(entry);
        }
    }
    stack.pop();
    Ok(entries)
}

fn type_error(path: &Path, section: &str) -> ResolverError {
    ResolverError::InvalidManifest {
        path: path.display().to_string(),
        reason: format!("`{section}` must be a list of dependency strings"),
    }
}

fn load_npm_manifest(path: &Path, content: &str) -> Result<Manifest, ResolverError> {
    let doc: serde_json::Value =
        serde_json::from_str(content).map_err(|e| ResolverError::InvalidManifest {
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
    fn python_dependency_groups_expand_include_group() {
        let dir = temp_dir("include-group");
        let path = dir.join("pyproject.toml");
        std::fs::write(
            &path,
            r#"
[project]
name = "consumer"
dependencies = ["juntai-iam==2.*"]

[dependency-groups]
test = ["pytest>=8", "httpx>=0.28"]
lint = ["ruff==0.15.2"]
dev = [
  { include-group = "test" },
  { include-group = "lint" },
  "build>=1.3.0",
]
"#,
        )
        .expect("write manifest");

        let manifest = load_manifest(&path).expect("load with include-group");
        // The loader reads every group; `dev`'s include-group entries are
        // substituted in place, so the declaration SET is the union of every
        // group's expanded entries (duplicates across groups are harmless:
        // the same declaration parsed twice resolves identically).
        let mut names: Vec<&str> = manifest
            .declarations
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names,
            vec!["build", "httpx", "juntai-iam", "pytest", "ruff"]
        );
        // And the raw (unsorted) declaration list contains every group's
        // entries with dev fully expanded — 8 = 1 project + (2 test + 1 lint)
        // + (2 test + 1 lint + 1 build via dev's expansion).
        assert_eq!(manifest.declarations.len(), 8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_dependency_groups_reject_cycles() {
        let dir = temp_dir("cycle");
        let path = dir.join("pyproject.toml");
        std::fs::write(
            &path,
            r#"
[dependency-groups]
a = [{ include-group = "b" }]
b = [{ include-group = "a" }]
"#,
        )
        .expect("write manifest");
        let err = load_manifest(&path).unwrap_err();
        assert!(
            err.to_string().contains("include-group cycle"),
            "expected cycle error, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_dependency_groups_reject_foreign_inline_tables() {
        let dir = temp_dir("foreign-inline");
        let path = dir.join("pyproject.toml");
        std::fs::write(
            &path,
            r#"
[dependency-groups]
dev = [{ include-group = "test", platform = "linux" }]
test = ["pytest>=8"]
"#,
        )
        .expect("write manifest");
        let err = load_manifest(&path).unwrap_err();
        assert!(
            err.to_string().contains("inline tables must be exactly"),
            "expected inline-table shape error, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_dependency_groups_reject_unknown_include_target() {
        let dir = temp_dir("unknown-include");
        let path = dir.join("pyproject.toml");
        std::fs::write(
            &path,
            r#"
[dependency-groups]
dev = [{ include-group = "missing" }]
"#,
        )
        .expect("write manifest");
        let err = load_manifest(&path).unwrap_err();
        assert!(
            err.to_string().contains("[dependency-groups].missing"),
            "expected missing-group error naming the target, got: {err}"
        );
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
