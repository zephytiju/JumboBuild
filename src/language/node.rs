//! Node language support: npm/TypeScript projects detected by `package.json`.
//!
//! Command parity with the Python pipeline (README "Project commands"):
//!
//! | Command | Node pipeline |
//! | --- | --- |
//! | `jumbo` / `jumbo build` | `npm install --package-lock-only --ignore-scripts` → `npm ci` → `npm run build` (when a `build` script is configured) |
//! | `jumbo test` | build pipeline → `npm test` (when configured) or `node --test` (when Node test files exist) |
//! | `jumbo format` | build pipeline → `npm run format` (when configured) or `npx --no-install prettier --write .` (when Prettier is configured) |
//! | `jumbo release` | build pipeline → test step → `npm run format:check` or `npx --no-install prettier --check .` |
//! | `jumbo clean` | Remove `node_modules/`, build output, coverage, and cache artifacts |
//!
//! The lock step mirrors the fingerprint engine's tool choice exactly
//! (`npm install --package-lock-only --ignore-scripts`): the lock is
//! re-resolved from the declared ranges without ever executing lifecycle
//! scripts. `npm ci` then installs exactly what the refreshed lock pins, so
//! the installed tree always matches the lockfile version the ambient npm
//! writes (lockfileVersion 2/3 on npm ≥ 7; older v1 locks are re-resolved
//! and upgraded by the same step before anything installs from them).
//! Internal `@juntai/*` (standard) and legacy `@zephytiju/*` dependencies
//! are injected at `file:deps/<slug>` coordinates by `jumbo lock` and the
//! materializer; this pipeline only runs the plain toolchain against
//! whatever the manifest and lock say.

use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::collections::BTreeMap;
use std::path::Path;
use walkdir::WalkDir;

use super::LanguageSupport;
use crate::utils::runner::run_steps;
use crate::workspace::metadata::RepoInfo;

/// Node language support for npm and TypeScript projects.
pub struct NodeSupport;

/// Refresh the lock from the declared ranges without running lifecycle
/// scripts. Byte-identical to the fingerprint engine's lock command so the
/// workspace pipeline and `jumbo lock` produce the same `package-lock.json`.
const LOCK_REFRESH_STEP: (&str, &str) = (
    "npm install --package-lock-only --ignore-scripts",
    "Refreshing package-lock.json (third-party ranges re-resolved)",
);

/// Clean-install exactly the refreshed lock. The npm counterpart of
/// `uv sync`; installing always follows a fresh lock step, which is what
/// makes the pipeline lockfileVersion-agnostic.
const INSTALL_STEP: (&str, &str) = ("npm ci", "Installing dependencies from package-lock.json");

const BUILD_STEP: (&str, &str) = ("npm run build", "Running node build script");
const TEST_SCRIPT_STEP: (&str, &str) = ("npm test", "Executing test suite");
const NODE_RUNNER_TEST_STEP: (&str, &str) =
    ("node --test", "Executing test suite (Node test runner)");
const FORMAT_SCRIPT_STEP: (&str, &str) = ("npm run format", "Formatting sources");
const PRETTIER_WRITE_STEP: (&str, &str) = (
    "npx --no-install prettier --write .",
    "Formatting sources with Prettier",
);
const FORMAT_CHECK_SCRIPT_STEP: (&str, &str) = (
    "npm run format:check",
    "Validating strict formatting compliance checks",
);
const PRETTIER_CHECK_STEP: (&str, &str) = (
    "npx --no-install prettier --check .",
    "Checking Prettier formatting compliance",
);

/// Prettier configuration files recognized at the package root (Prettier's
/// own documented list, minus editor plugins).
const PRETTIER_CONFIG_FILES: &[&str] = &[
    ".prettierrc",
    ".prettierrc.json",
    ".prettierrc.json5",
    ".prettierrc.yaml",
    ".prettierrc.yml",
    ".prettierrc.js",
    ".prettierrc.cjs",
    ".prettierrc.mjs",
    ".prettierrc.toml",
    "prettier.config.js",
    "prettier.config.cjs",
    "prettier.config.mjs",
];

/// The parsed `package.json` facts the pipelines select on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeProject {
    /// `name` from package.json, when present.
    pub name: Option<String>,
    /// `scripts` entries.
    pub scripts: BTreeMap<String, String>,
    /// `engines.node` range, when declared.
    pub engines_node: Option<String>,
    /// Whether Prettier is configured (dependency, `prettier` key, or a
    /// Prettier configuration file).
    pub prettier_configured: bool,
}

impl NodeProject {
    /// Whether `scripts` defines the given command.
    pub fn has_script(&self, name: &str) -> bool {
        self.scripts.contains_key(name)
    }

    /// The effective `test` script: npm's generated placeholder
    /// (`echo "Error: no test specified" && exit 1`) counts as absent.
    pub fn test_script(&self) -> Option<&str> {
        self.scripts.get("test").and_then(|script| {
            if script.contains("no test specified") {
                None
            } else {
                Some(script.as_str())
            }
        })
    }

    /// The formatting step: the project's own `format` script when defined,
    /// otherwise Prettier directly when configured.
    pub fn format_step(&self) -> Option<(&'static str, &'static str)> {
        if self.has_script("format") {
            Some(FORMAT_SCRIPT_STEP)
        } else if self.prettier_configured {
            Some(PRETTIER_WRITE_STEP)
        } else {
            None
        }
    }

    /// The strict formatting-check step used by `jumbo release`.
    pub fn format_check_step(&self) -> Option<(&'static str, &'static str)> {
        if self.has_script("format:check") {
            Some(FORMAT_CHECK_SCRIPT_STEP)
        } else if self.prettier_configured {
            Some(PRETTIER_CHECK_STEP)
        } else {
            None
        }
    }

    /// The test step: `npm test` when configured, otherwise the built-in
    /// Node test runner when the project carries Node test files.
    pub fn test_step(&self, repo_path: &Path) -> Option<(&'static str, &'static str)> {
        if self.test_script().is_some() {
            Some(TEST_SCRIPT_STEP)
        } else if has_node_test_files(repo_path) {
            Some(NODE_RUNNER_TEST_STEP)
        } else {
            None
        }
    }
}

/// Read and parse the `package.json` of a Node project.
pub fn load_node_project(repo_path: &Path) -> Result<NodeProject> {
    let manifest = repo_path.join("package.json");
    let content = std::fs::read_to_string(&manifest)
        .with_context(|| format!("Failed to read {}", manifest.display()))?;
    let doc: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", manifest.display()))?;

    let scripts = doc
        .get("scripts")
        .and_then(|value| value.as_object())
        .map(|object| {
            object
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .as_str()
                        .map(|script| (name.clone(), script.to_string()))
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    let engines_node = doc
        .get("engines")
        .and_then(|value| value.get("node"))
        .and_then(|value| value.as_str())
        .map(str::to_owned);

    Ok(NodeProject {
        name: doc
            .get("name")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        scripts,
        engines_node,
        prettier_configured: prettier_configured(repo_path, &doc),
    })
}

/// Whether Prettier is configured for the project: a `prettier` entry in
/// any dependency section, a `prettier` key in package.json, or a Prettier
/// configuration file at the package root.
fn prettier_configured(repo_path: &Path, doc: &serde_json::Value) -> bool {
    for section in ["dependencies", "devDependencies", "optionalDependencies"] {
        if doc
            .get(section)
            .and_then(|value| value.as_object())
            .is_some_and(|map| map.contains_key("prettier"))
        {
            return true;
        }
    }
    doc.get("prettier").is_some()
        || PRETTIER_CONFIG_FILES
            .iter()
            .any(|file| repo_path.join(file).exists())
}

/// The shared front of every pipeline: refresh the lock, install exactly
/// from it, then run the project's `build` script when one is configured.
fn build_steps(project: &NodeProject) -> Vec<(&'static str, &'static str)> {
    let mut steps = vec![LOCK_REFRESH_STEP, INSTALL_STEP];
    if project.has_script("build") {
        steps.push(BUILD_STEP);
    }
    steps
}

impl NodeSupport {
    /// Load the project configuration and enforce `engines.node` before any
    /// pipeline step runs.
    fn prepare_pipeline(&self, repo_path: &Path) -> Result<NodeProject> {
        let project = load_node_project(repo_path)?;
        check_node_engines(repo_path, &project)?;
        if !project.has_script("build") {
            println!(
                "  {} No build script configured; install-only build",
                "-".dimmed()
            );
        }
        Ok(project)
    }
}

impl LanguageSupport for NodeSupport {
    fn name(&self) -> &str {
        "node"
    }

    fn detect(&self, repo_path: &Path) -> bool {
        repo_path.join("package.json").exists()
    }

    /// Node projects do not need workspace-root configuration: internal
    /// dependencies resolve through the Jumbo index (`jumbo lock` injects
    /// `file:deps/<slug>` sources; the materializer ingests release assets
    /// the same way), never through a shared npm workspace. A root npm
    /// workspace would centralize `node_modules` and change per-project
    /// install semantics, so none is generated. Node repositories are
    /// registered in `jumbo.toml` with their npm package identity
    /// (`@juntai/*` standard, `@zephytiju/*` legacy) and stay excluded
    /// from the uv workspace, which the Python backend owns.
    fn sync_workspace(&self, workspace_root: &Path, repos: &[RepoInfo]) -> Result<()> {
        let _ = (workspace_root, repos);
        Ok(())
    }

    fn build(&self, _workspace_root: &Path, repo_path: &Path) -> Result<()> {
        let project = self.prepare_pipeline(repo_path)?;
        run_steps(&build_steps(&project), repo_path)
    }

    fn test(&self, _workspace_root: &Path, repo_path: &Path) -> Result<()> {
        let project = self.prepare_pipeline(repo_path)?;
        let mut steps = build_steps(&project);
        match project.test_step(repo_path) {
            Some(step) => steps.push(step),
            None => println!(
                "  {} No test script and no Node test files; nothing to test",
                "-".dimmed()
            ),
        }
        run_steps(&steps, repo_path)
    }

    fn format(&self, _workspace_root: &Path, repo_path: &Path) -> Result<()> {
        let project = self.prepare_pipeline(repo_path)?;
        let mut steps = build_steps(&project);
        match project.format_step() {
            Some(step) => steps.push(step),
            None => println!(
                "  {} Prettier is not configured; formatting skipped",
                "-".dimmed()
            ),
        }
        run_steps(&steps, repo_path)
    }

    fn release(&self, _workspace_root: &Path, repo_path: &Path) -> Result<()> {
        let project = self.prepare_pipeline(repo_path)?;
        let mut steps = build_steps(&project);
        match project.test_step(repo_path) {
            Some(step) => steps.push(step),
            None => println!(
                "  {} No test script and no Node test files; nothing to test",
                "-".dimmed()
            ),
        }
        match project.format_check_step() {
            Some(step) => steps.push(step),
            None => println!(
                "  {} Prettier is not configured; strict formatting check skipped",
                "-".dimmed()
            ),
        }
        run_steps(&steps, repo_path)
    }

    fn clean(&self, repo_path: &Path) -> Result<()> {
        let dir_patterns = &[
            "node_modules",
            "dist",
            "build",
            "out",
            "coverage",
            ".nyc_output",
        ];
        let file_patterns = &[".eslintcache"];
        let glob_suffixes = &[".tsbuildinfo"];

        let mut cleaned = 0u32;
        let mut it = WalkDir::new(repo_path).into_iter();
        while let Some(Ok(entry)) = it.next() {
            let name = entry.file_name().to_string_lossy();

            if entry.file_type().is_dir() {
                if dir_patterns.contains(&name.as_ref())
                    || glob_suffixes.iter().any(|s| name.ends_with(s))
                {
                    if std::fs::remove_dir_all(entry.path()).is_ok() {
                        println!("    {} Removed {}", "-".dimmed(), entry.path().display());
                        cleaned += 1;
                    }
                    // Nothing left to walk inside a removed directory.
                    it.skip_current_dir();
                } else if name == ".git" || name == "deps" {
                    // Never descend into git metadata or jumbo-injected
                    // sources; they hold no build artifacts.
                    it.skip_current_dir();
                }
            } else if entry.file_type().is_file()
                && (file_patterns.contains(&name.as_ref())
                    || glob_suffixes.iter().any(|s| name.ends_with(s)))
                && std::fs::remove_file(entry.path()).is_ok()
            {
                println!("    {} Removed {}", "-".dimmed(), entry.path().display());
                cleaned += 1;
            }
        }

        if cleaned > 0 {
            println!(
                "  {} Cleaned {} Node artifact(s) in {}",
                "✓".green(),
                cleaned,
                repo_path.display()
            );
        }
        Ok(())
    }
}

/// Enforce `engines.node` before running any pipeline step. A range this
/// evaluator cannot parse is deferred to npm's own engines check (a note is
/// printed); a clearly unsatisfied range fails fast with both versions.
fn check_node_engines(repo_path: &Path, project: &NodeProject) -> Result<()> {
    let Some(range) = &project.engines_node else {
        return Ok(());
    };
    let runtime = runtime_node_version()?;
    match node_satisfies(range, runtime) {
        Some(true) => Ok(()),
        Some(false) => bail!(
            "Node {}.{}.{} does not satisfy engines.node `{}` required by {}",
            runtime.0,
            runtime.1,
            runtime.2,
            range,
            repo_path.join("package.json").display()
        ),
        None => {
            println!(
                "  {} engines.node `{}` not statically evaluated; deferring to npm",
                "-".dimmed(),
                range
            );
            Ok(())
        }
    }
}

/// The runtime Node version from `node --version` (`v24.14.0` → (24, 14, 0)).
fn runtime_node_version() -> Result<(u64, u64, u64)> {
    let output = std::process::Command::new("node")
        .arg("--version")
        .output()
        .context("Failed to run `node --version` while checking engines")?;
    if !output.status.success() {
        bail!("`node --version` failed while checking engines");
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_node_version(&raw)
        .ok_or_else(|| anyhow::anyhow!("Unrecognized `node --version` output: `{raw}`"))
}

/// Parse a Node version like `v24.14.0` or `24.14.0-nightly.1` into a
/// numeric triple (prerelease/build suffixes ignored).
fn parse_node_version(text: &str) -> Option<(u64, u64, u64)> {
    let text = text.trim().trim_start_matches('v');
    let text = text.split(['-', '+']).next()?.trim();
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let patch = parts.next().unwrap_or("0").parse().unwrap_or(0);
    Some((major, minor, patch))
}

/// A numeric version triple plus the number of significant parts, which
/// carries the wildcard semantics npm gives to short forms: `18` means
/// `18.x.x` (precision 1), `18.2` means `18.2.x` (precision 2), `18.2.1`
/// is exact (precision 3), and `*`/`x` is anything (precision 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    precision: usize,
}

impl Version {
    /// Parse `18`, `18.2`, `18.2.1`, `18.x`, `18.2.x`, `*`. Explicit and
    /// implicit (missing) `x` parts both lower the precision; prerelease
    /// and build suffixes are ignored.
    fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        if text.is_empty() || text == "*" || text == "x" || text == "X" {
            return Some(Version {
                major: 0,
                minor: 0,
                patch: 0,
                precision: 0,
            });
        }
        let text = text.split(['-', '+']).next()?.trim();
        let mut parts = text.split('.');
        let mut numbers = [0u64; 3];
        let mut precision = 0usize;
        for slot in numbers.iter_mut() {
            let part = match parts.next() {
                Some(part) => part.trim(),
                None => break,
            };
            if part == "x" || part == "X" || part == "*" {
                break;
            }
            *slot = part.parse().ok()?;
            precision += 1;
        }
        if parts.next().is_some() {
            return None;
        }
        Some(Version {
            major: numbers[0],
            minor: numbers[1],
            patch: numbers[2],
            precision,
        })
    }

    /// The numeric triple alone; `precision` never participates in
    /// ordering (it only selects range semantics).
    fn triple(self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }
}

/// Evaluate an `engines.node` range against a runtime version.
///
/// Supports the common forms: `*`, exact versions, `M`/`M.m` short forms,
/// `x`-ranges, `^`/`~` ranges, comparators (`>`, `>=`, `<`, `<=`, `=`), and
/// space-separated conjunctions plus `||` alternatives. Returns `None` for
/// anything else so callers can defer to npm instead of guessing.
pub fn node_satisfies(range: &str, runtime: (u64, u64, u64)) -> Option<bool> {
    let runtime = Version {
        major: runtime.0,
        minor: runtime.1,
        patch: runtime.2,
        precision: 3,
    };
    for alternative in range.split("||") {
        let alternative = alternative.trim();
        if alternative.is_empty() || alternative == "*" {
            return Some(true);
        }
        // Parse every comparator before evaluating any: a range with
        // unsupported syntax anywhere defers to npm as a whole instead of
        // answering from the prefix it happened to understand.
        let comparators: Vec<_> = alternative
            .split_whitespace()
            .map(parse_comparator)
            .collect::<Option<Vec<_>>>()?;
        if comparators
            .iter()
            .all(|(operator, bound)| eval_comparator(operator, *bound, runtime))
        {
            return Some(true);
        }
    }
    Some(false)
}

/// Split one comparator into its operator and version bound
/// (`>=18` → `(">=", 18.0.0)`).
fn parse_comparator(comparator: &str) -> Option<(&'static str, Version)> {
    let (operator, text) = comparator
        .strip_prefix(">=")
        .map(|rest| (">=", rest))
        .or_else(|| comparator.strip_prefix("<=").map(|rest| ("<=", rest)))
        .or_else(|| comparator.strip_prefix('<').map(|rest| ("<", rest)))
        .or_else(|| comparator.strip_prefix('>').map(|rest| (">", rest)))
        .or_else(|| comparator.strip_prefix('^').map(|rest| ("^", rest)))
        .or_else(|| comparator.strip_prefix('~').map(|rest| ("~", rest)))
        .or_else(|| comparator.strip_prefix('=').map(|rest| ("=", rest)))
        .unwrap_or(("", comparator));
    Some((operator, Version::parse(text)?))
}

/// Evaluate one parsed comparator against a runtime version. Short and
/// `x`-range bounds evaluate as ranges.
fn eval_comparator(operator: &str, bound: Version, runtime: Version) -> bool {
    match operator {
        // A bare or `=`-prefixed version: exact at full precision, a range
        // for short and `x` forms (`18` accepts all of major 18).
        "" | "=" => match bound.precision {
            0 => true,
            1 => runtime.major == bound.major,
            2 => runtime.major == bound.major && runtime.minor == bound.minor,
            _ => runtime.triple() == bound.triple(),
        },
        "^" => {
            if bound.precision == 0 {
                return true;
            }
            let upper = if bound.major > 0 {
                Version {
                    major: bound.major + 1,
                    minor: 0,
                    patch: 0,
                    precision: 3,
                }
            } else if bound.minor > 0 {
                Version {
                    major: 0,
                    minor: bound.minor + 1,
                    patch: 0,
                    precision: 3,
                }
            } else {
                Version {
                    major: 0,
                    minor: 0,
                    patch: bound.patch + 1,
                    precision: 3,
                }
            };
            runtime.triple() >= bound.triple() && runtime.triple() < upper.triple()
        }
        "~" => {
            if bound.precision == 0 {
                return true;
            }
            let upper = Version {
                major: bound.major,
                minor: bound.minor + 1,
                patch: 0,
                precision: 3,
            };
            runtime.triple() >= bound.triple() && runtime.triple() < upper.triple()
        }
        ">=" => runtime.triple() >= bound.triple(),
        ">" => runtime.triple() > bound.triple(),
        // `<=24.14` accepts all of `24.14.x`; `<=24.14.0` is exact.
        "<=" => match bound.precision {
            0 => true,
            1 => runtime.major == bound.major,
            2 => runtime.major == bound.major && runtime.minor <= bound.minor,
            _ => runtime.triple() <= bound.triple(),
        },
        "<" => runtime.triple() < bound.triple(),
        _ => false,
    }
}

/// Whether the project contains files the Node test runner would execute:
/// `*.test.js`/`*-test.js`/`*_test.js`/`test.js` (`.mjs`/`.cjs` included).
/// Used only to decide whether `node --test` can stand in for a missing
/// `test` script; projects with real suites configure `scripts.test`.
fn has_node_test_files(repo_path: &Path) -> bool {
    WalkDir::new(repo_path)
        .into_iter()
        .filter_entry(|entry| {
            !matches!(
                entry.file_name().to_str(),
                Some("node_modules") | Some(".git") | Some("deps") | Some("dist")
            )
        })
        .filter_map(|entry| entry.ok())
        .any(|entry| {
            entry.file_type().is_file()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(is_node_test_file_name)
        })
}

fn is_node_test_file_name(name: &str) -> bool {
    let Some(dot) = name.rfind('.') else {
        return false;
    };
    let (stem, extension) = name.split_at(dot);
    let extension = &extension[1..];
    if !matches!(extension, "js" | "mjs" | "cjs") {
        return false;
    }
    stem.ends_with(".test") || stem.ends_with("-test") || stem.ends_with("_test") || stem == "test"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::{detect_language, get_registry};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_project(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jumbo-node-ut-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create temp project");
        dir
    }

    fn write_project(tag: &str, package_json: &str) -> std::path::PathBuf {
        let dir = temp_project(tag);
        fs::write(dir.join("package.json"), package_json).expect("write package.json");
        dir
    }

    #[test]
    fn detect_requires_package_json() {
        let empty = temp_project("detect-empty");
        assert!(!NodeSupport.detect(&empty));

        let node = write_project("detect-node", r#"{"name": "kit"}"#);
        assert!(NodeSupport.detect(&node));

        let _ = (fs::remove_dir_all(&empty), fs::remove_dir_all(&node));
    }

    #[test]
    fn registry_detects_python_before_node() {
        let both = temp_project("detect-both");
        fs::write(both.join("package.json"), r#"{"name": "kit"}"#).unwrap();
        fs::write(both.join("pyproject.toml"), "[project]\nname = \"kit\"\n").unwrap();

        let registry = get_registry();
        let lang = detect_language(&registry, &both).expect("language detected");
        assert_eq!(lang.name(), "python");

        // The Node backend itself must stay reachable behind it.
        assert!(registry.iter().any(|lang| lang.name() == "node"));
        let _ = fs::remove_dir_all(&both);
    }

    #[test]
    fn project_configuration_is_parsed() {
        let dir = temp_project("parse");
        fs::write(
            dir.join("package.json"),
            r#"{
  "name": "@juntai/sample-kit",
  "engines": { "node": ">=18" },
  "devDependencies": { "prettier": "3.9.6" },
  "scripts": {
    "build": "tsc -p tsconfig.build.json",
    "test": "vitest run",
    "format": "prettier --write ."
  }
}"#,
        )
        .unwrap();

        let project = load_node_project(&dir).expect("load");
        assert_eq!(project.name.as_deref(), Some("@juntai/sample-kit"));
        assert_eq!(project.engines_node.as_deref(), Some(">=18"));
        assert!(project.prettier_configured);
        assert!(project.has_script("build"));
        assert_eq!(project.test_script(), Some("vitest run"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prettier_configuration_is_detected_from_files_and_key() {
        let with_file = temp_project("prettier-file");
        fs::write(with_file.join("package.json"), r#"{"name": "a"}"#).unwrap();
        fs::write(with_file.join(".prettierrc"), "{}\n").unwrap();
        assert!(load_node_project(&with_file).unwrap().prettier_configured);

        let with_key = temp_project("prettier-key");
        fs::write(
            with_key.join("package.json"),
            r#"{"name": "a", "prettier": {"printWidth": 100}}"#,
        )
        .unwrap();
        assert!(load_node_project(&with_key).unwrap().prettier_configured);

        let bare = temp_project("prettier-none");
        fs::write(bare.join("package.json"), r#"{"name": "a"}"#).unwrap();
        assert!(!load_node_project(&bare).unwrap().prettier_configured);

        let _ = (
            fs::remove_dir_all(&with_file),
            fs::remove_dir_all(&with_key),
            fs::remove_dir_all(&bare),
        );
    }

    #[test]
    fn npm_test_placeholder_counts_as_no_test_script() {
        let project = NodeProject {
            scripts: BTreeMap::from([(
                "test".to_string(),
                "echo \"Error: no test specified\" && exit 1".to_string(),
            )]),
            ..Default::default()
        };
        assert!(project.test_script().is_none());
    }

    #[test]
    fn build_steps_follow_the_documented_pipeline() {
        // Without a build script: lock refresh + clean install only.
        let plain = NodeProject::default();
        assert_eq!(build_steps(&plain), vec![LOCK_REFRESH_STEP, INSTALL_STEP]);

        // With one: the project's build script runs after the install.
        let with_build = NodeProject {
            scripts: BTreeMap::from([("build".to_string(), "tsc".to_string())]),
            ..Default::default()
        };
        assert_eq!(
            build_steps(&with_build),
            vec![LOCK_REFRESH_STEP, INSTALL_STEP, BUILD_STEP]
        );
    }

    #[test]
    fn test_step_prefers_script_then_node_runner() {
        let dir = temp_project("test-step");
        fs::write(dir.join("package.json"), r#"{"name": "a"}"#).unwrap();

        // No script, no test files: nothing to run.
        let plain = NodeProject::default();
        assert!(plain.test_step(&dir).is_none());

        // No script, but a Node test file exists: the built-in runner runs.
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/add.test.js"),
            "import test from 'node:test';\n",
        )
        .unwrap();
        assert_eq!(plain.test_step(&dir), Some(NODE_RUNNER_TEST_STEP));

        // A configured script always wins.
        let scripted = NodeProject {
            scripts: BTreeMap::from([("test".to_string(), "vitest run".to_string())]),
            ..Default::default()
        };
        assert_eq!(scripted.test_step(&dir), Some(TEST_SCRIPT_STEP));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn node_test_file_discovery_ignores_generated_trees() {
        let dir = temp_project("discovery");
        fs::write(dir.join("package.json"), r#"{"name": "a"}"#).unwrap();
        for skipped in ["node_modules", "deps", "dist", ".git"] {
            fs::create_dir_all(dir.join(skipped)).unwrap();
            fs::write(dir.join(skipped).join("stray.test.js"), "").unwrap();
        }
        assert!(!has_node_test_files(&dir));

        fs::write(dir.join("app.spec.ts"), "").unwrap();
        assert!(
            !has_node_test_files(&dir),
            ".spec.ts is not a Node runner file"
        );

        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(dir.join("lib/math.test.mjs"), "").unwrap();
        assert!(has_node_test_files(&dir));
        assert!(is_node_test_file_name("math-test.cjs"));
        assert!(is_node_test_file_name("math_test.js"));
        assert!(is_node_test_file_name("test.js"));
        assert!(!is_node_test_file_name("math.test.ts"));
        assert!(!is_node_test_file_name("math.test.txt"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn format_steps_select_script_prettier_or_nothing() {
        let none = NodeProject::default();
        assert!(none.format_step().is_none());
        assert!(none.format_check_step().is_none());

        let with_prettier = NodeProject {
            prettier_configured: true,
            ..Default::default()
        };
        assert_eq!(with_prettier.format_step(), Some(PRETTIER_WRITE_STEP));
        assert_eq!(with_prettier.format_check_step(), Some(PRETTIER_CHECK_STEP));

        let scripted = NodeProject {
            scripts: BTreeMap::from([
                ("format".to_string(), "prettier --write .".to_string()),
                ("format:check".to_string(), "prettier --check .".to_string()),
            ]),
            ..Default::default()
        };
        assert_eq!(scripted.format_step(), Some(FORMAT_SCRIPT_STEP));
        assert_eq!(scripted.format_check_step(), Some(FORMAT_CHECK_SCRIPT_STEP));
    }

    #[test]
    fn engines_ranges_evaluate() {
        let runtime = (24, 14, 0);
        for range in [
            "*",
            ">=18",
            ">=18.0.0",
            "^24",
            "~24.14",
            "24.x",
            "24",
            "24.14.0",
            "24.14.x",
            ">=22",
            ">=18 <25",
            ">=18 || >=20 <21",
            "^20 || ^24",
            "=24",
        ] {
            assert_eq!(
                node_satisfies(range, runtime),
                Some(true),
                "range {range} should accept 24.14.0"
            );
        }
        for range in [
            ">=25",
            "^18",
            "^25",
            "~23",
            "23.x",
            "23",
            "23.14.0",
            "24.14.1",
            ">=18 <24",
            ">=25 || <20",
            "~25",
        ] {
            assert_eq!(
                node_satisfies(range, runtime),
                Some(false),
                "range {range} should reject 24.14.0"
            );
        }
        // Node 18 with a modern floor, the VanguDeploymentConstructs case.
        assert_eq!(node_satisfies(">=22", (18, 20, 0)), Some(false));
        assert_eq!(node_satisfies(">=22", (22, 0, 0)), Some(true));
        // Zero-major caret ranges follow semver's special rule.
        assert_eq!(node_satisfies("^0.2.3", (0, 2, 9)), Some(true));
        assert_eq!(node_satisfies("^0.2.3", (0, 3, 0)), Some(false));
        // Syntax outside the documented subset defers to npm.
        for range in ["18 - 20", ">=18 <20 || 22.x - 24", "latest"] {
            assert_eq!(node_satisfies(range, runtime), None, "range {range}");
        }
    }

    #[test]
    fn node_versions_parse() {
        assert_eq!(parse_node_version("v24.14.0"), Some((24, 14, 0)));
        assert_eq!(parse_node_version("18"), Some((18, 0, 0)));
        assert_eq!(parse_node_version("v22.0.0-nightly.1"), Some((22, 0, 0)));
        assert_eq!(parse_node_version("not-a-version"), None);
    }

    #[test]
    fn clean_removes_node_artifacts_only() {
        let dir = temp_project("clean");
        fs::write(dir.join("package.json"), r#"{"name": "a"}"#).unwrap();
        for artifact in ["node_modules", "dist", "coverage", "out"] {
            fs::create_dir_all(dir.join(artifact)).unwrap();
            fs::write(dir.join(artifact).join("payload.js"), "").unwrap();
        }
        fs::write(dir.join("app.tsbuildinfo"), "").unwrap();
        fs::write(dir.join(".eslintcache"), "").unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/app.ts"), "export {};\n").unwrap();
        // jumbo-injected sources are regenerated by `jumbo lock`, never
        // cleaned as build output.
        fs::create_dir_all(dir.join("deps/juntai-kit")).unwrap();
        fs::write(dir.join("deps/juntai-kit/package.json"), "{}\n").unwrap();

        NodeSupport.clean(&dir).expect("clean");

        assert!(!dir.join("node_modules").exists());
        assert!(!dir.join("dist").exists());
        assert!(!dir.join("coverage").exists());
        assert!(!dir.join("out").exists());
        assert!(!dir.join("app.tsbuildinfo").exists());
        assert!(!dir.join(".eslintcache").exists());
        assert!(dir.join("src/app.ts").exists());
        assert!(dir.join("deps/juntai-kit/package.json").exists());
        assert!(dir.join("package.json").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_workspace_is_an_explicit_no_op() {
        NodeSupport
            .sync_workspace(Path::new("/nonexistent"), &[])
            .expect("node sync is a no-op");
    }

    #[test]
    fn engines_check_fails_fast_on_unsatisfied_range() {
        let dir = write_project(
            "engines-fail",
            r#"{ "name": "a", "engines": { "node": ">=99" } }"#,
        );
        let project = load_node_project(&dir).unwrap();
        let err = check_node_engines(&dir, &project).unwrap_err();
        assert!(err.to_string().contains("engines.node"), "{err}");
        assert!(err.to_string().contains(">=99"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }
}
