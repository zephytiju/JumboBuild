//! CLI integration tests for Node language support.
//!
//! The pipeline tests run the built `jumbo` binary against a fixture npm
//! package inside a throwaway Jumbo workspace. The fixture has zero
//! third-party dependencies, so `npm install --package-lock-only` and
//! `npm ci` complete without network access; formatting runs through the
//! project's own `format`/`format:check` scripts for the same reason.
//! Tests that need Node/npm skip with a notice when the tools are
//! unavailable; the lock-parity and workspace-registration tests need
//! neither.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn jumbo_bin() -> &'static str {
    env!("CARGO_BIN_EXE_jumbo")
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "jumbo-node-cli-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn npm_available() -> bool {
    Command::new("npm")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn require_node_toolchain() -> bool {
    if node_available() && npm_available() {
        true
    } else {
        eprintln!("skipping: node/npm not available on PATH");
        false
    }
}

fn run_jumbo(args: &[&str], cwd: &Path) -> Output {
    Command::new(jumbo_bin())
        .args(args)
        .current_dir(cwd)
        .env_remove("JUMBO_INDEX_PATH")
        .env_remove("JUMBO_INDEX_URL")
        .output()
        .expect("run jumbo")
}

/// The fixture npm package: no third-party dependencies (offline installs),
/// a build script, and `engines.node` the local runtime satisfies.
fn write_fixture_package(dir: &Path, package_json: &str) {
    fs::create_dir_all(dir.join("src")).expect("create src");
    fs::write(dir.join("package.json"), package_json).expect("write package.json");
    fs::write(
        dir.join("src/add.js"),
        "export function add(a, b) {\n  return a + b;\n}\n",
    )
    .expect("write src/add.js");
    fs::write(
        dir.join("src/add.test.js"),
        "import test from 'node:test';\nimport assert from 'node:assert/strict';\nimport { add } from './add.js';\n\ntest('add sums two numbers', () => {\n  assert.equal(add(2, 3), 5);\n});\n",
    )
    .expect("write src/add.test.js");
}

const FIXTURE_PACKAGE_JSON: &str = r#"{
  "name": "@juntai/sample-kit",
  "version": "1.0.0",
  "private": true,
  "type": "module",
  "engines": { "node": ">=18" },
  "scripts": {
    "build": "mkdir -p dist && cp src/add.js dist/add.js"
  }
}
"#;

/// Create a Jumbo workspace whose root is a fresh temp directory and whose
/// only project is the fixture npm package under `projects/sample-kit`.
/// Returns `(workspace_root, project_dir)`.
fn fixture_workspace(tag: &str, package_json: &str) -> (PathBuf, PathBuf) {
    let root = temp_dir(tag);
    let project = root.join("projects/sample-kit");
    write_fixture_package(&project, package_json);

    // `workspace create --import <name>` initializes the existing directory
    // and registers everything under projects/.
    let parent = root.parent().unwrap().to_path_buf();
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .expect("workspace dir name");
    let out = run_jumbo(&["workspace", "create", "--import", name], &parent);
    assert!(
        out.status.success(),
        "workspace create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (root, project)
}

fn read_jumbo_toml(root: &Path) -> toml::Value {
    fs::read_to_string(root.join("jumbo.toml"))
        .expect("read jumbo.toml")
        .parse()
        .expect("parse jumbo.toml")
}

fn run_git_init(dir: &Path) {
    for args in [
        &["init", "-q", "--initial-branch=main"][..],
        &["config", "user.email", "jumbo@test.invalid"][..],
        &["config", "user.name", "Jumbo Test"][..],
        &["add", "."][..],
        &["commit", "-q", "-m", "fixture"][..],
    ] {
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
}

#[test]
fn workspace_registers_node_projects_with_npm_identity() {
    let (root, _project) = fixture_workspace("ws-register", FIXTURE_PACKAGE_JSON);

    let metadata = read_jumbo_toml(&root);
    let repos = metadata["workspace"]["repositories"].as_array().unwrap();
    let repo = repos
        .iter()
        .find(|repo| repo["name"].as_str() == Some("sample-kit"))
        .expect("sample-kit registered");
    assert_eq!(repo["package"].as_str(), Some("@juntai/sample-kit"));
    assert_eq!(repo["ecosystem"].as_str(), Some("node"));

    // The root uv workspace excludes the node project instead of listing
    // it as a member or a uv source.
    let pyproject: toml::Value = fs::read_to_string(root.join("pyproject.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let members = pyproject["tool"]["uv"]["workspace"]["members"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !members
            .iter()
            .any(|member| member.as_str() == Some("projects/sample-kit")),
        "node project must not be a uv workspace member"
    );
    let excluded = pyproject["tool"]["uv"]["workspace"]["exclude"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        excluded
            .iter()
            .any(|member| member.as_str() == Some("projects/sample-kit")),
        "node project should be excluded from the uv workspace"
    );
    assert!(pyproject["tool"]["uv"]["sources"]
        .as_table()
        .map(|sources| !sources.contains_key("@juntai/sample-kit"))
        .unwrap_or(true));

    // Sync is idempotent: a second pass leaves the generated files alone.
    let metadata_before = fs::read_to_string(root.join("jumbo.toml")).unwrap();
    let pyproject_before = fs::read_to_string(root.join("pyproject.toml")).unwrap();
    let out = run_jumbo(&["workspace", "sync"], &root);
    assert!(
        out.status.success(),
        "sync failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        metadata_before,
        fs::read_to_string(root.join("jumbo.toml")).unwrap()
    );
    assert_eq!(
        pyproject_before,
        fs::read_to_string(root.join("pyproject.toml")).unwrap()
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn build_pipeline_locks_installs_and_runs_the_build_script() {
    if !require_node_toolchain() {
        return;
    }
    let (_root, project) = fixture_workspace("build", FIXTURE_PACKAGE_JSON);

    let out = run_jumbo(&["build"], &project);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "jumbo build failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("Building node project"), "{stdout}");
    assert!(stdout.contains("BUILD SUCCESSFUL"), "{stdout}");

    // The documented pipeline ran: lock refreshed, deps installed, build
    // script executed.
    assert!(stdout.contains("Refreshing package-lock.json"), "{stdout}");
    assert!(
        stdout.contains("Installing dependencies from package-lock.json"),
        "{stdout}"
    );
    assert!(stdout.contains("Running node build script"), "{stdout}");

    let lock: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project.join("package-lock.json")).expect("lock generated"),
    )
    .expect("lock parses");
    assert!(
        lock["lockfileVersion"].as_u64().unwrap_or(0) >= 2,
        "npm 7+ lockfileVersion expected"
    );
    // With zero dependencies npm may skip creating an empty node_modules,
    // so the build-script output is the install-side evidence here.
    assert!(
        project.join("dist/add.js").exists(),
        "build script produced output"
    );
    let _ = fs::remove_dir_all(&project);
}

#[test]
fn test_pipeline_prefers_the_configured_script_and_falls_back_to_node_runner() {
    if !require_node_toolchain() {
        return;
    }

    // Configured script: `npm test` runs.
    let with_script = FIXTURE_PACKAGE_JSON.replace(
        "\"scripts\": {",
        "\"scripts\": {\n    \"test\": \"node --test src/add.test.js\",",
    );
    let (_root, project) = fixture_workspace("test-script", &with_script);
    let out = run_jumbo(&["test"], &project);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "jumbo test (script) failed:\n{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !stdout.contains("Node test runner"),
        "configured script must win over the built-in runner: {stdout}"
    );
    assert!(stdout.contains("Executing test suite"), "{stdout}");
    assert!(stdout.contains("BUILD SUCCESSFUL"), "{stdout}");
    let _ = fs::remove_dir_all(&project);

    // No script: the Node test runner discovers src/add.test.js.
    let (_root, project) = fixture_workspace("test-runner", FIXTURE_PACKAGE_JSON);
    let out = run_jumbo(&["test"], &project);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "jumbo test (node runner) failed:\n{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Node test runner"), "{stdout}");
    assert!(stdout.contains("BUILD SUCCESSFUL"), "{stdout}");
    let _ = fs::remove_dir_all(&project);
}

#[test]
fn format_and_release_run_the_configured_scripts() {
    if !require_node_toolchain() {
        return;
    }
    let with_format = FIXTURE_PACKAGE_JSON.replace(
        "\"scripts\": {",
        "\"scripts\": {\n    \"format\": \"echo formatted > formatted.marker\",\n    \"format:check\": \"test -f formatted.marker\",",
    );
    let (_root, project) = fixture_workspace("format", &with_format);

    // `jumbo format`: lock, install, build, then the project's format
    // script.
    let out = run_jumbo(&["format"], &project);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "jumbo format failed:\n{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Formatting sources"), "{stdout}");
    assert!(project.join("formatted.marker").exists());

    // `jumbo release`: build pipeline, tests, then the strict check.
    let out = run_jumbo(&["release"], &project);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "jumbo release failed:\n{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("Validating strict formatting compliance checks"),
        "{stdout}"
    );
    assert!(stdout.contains("BUILD SUCCESSFUL"), "{stdout}");
    let _ = fs::remove_dir_all(&project);
}

#[test]
fn pipelines_succeed_without_tests_or_formatter_and_clean_removes_artifacts() {
    if !require_node_toolchain() {
        return;
    }
    // A bare package: no scripts, no test files, no formatter.
    let bare = r#"{ "name": "@juntai/bare-kit", "version": "1.0.0", "private": true }"#;
    let (_root, project) = fixture_workspace("bare", bare);
    fs::remove_dir_all(project.join("src")).expect("remove src");

    for command in ["test", "format", "release"] {
        let out = run_jumbo(&[command], &project);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "jumbo {command} on a bare package failed:\n{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(stdout.contains("BUILD SUCCESSFUL"), "{stdout}");
        assert!(
            stdout.contains("No build script configured"),
            "{command}: {stdout}"
        );
    }

    // And a project-level clean removes the generated artifacts but leaves
    // the manifest and the jumbo-injected sources alone.
    let out = run_jumbo(&["clean"], &project);
    assert!(
        out.status.success(),
        "jumbo clean failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!project.join("node_modules").exists());
    assert!(project.join("package.json").exists());
    let _ = fs::remove_dir_all(&project);
}

#[test]
fn unsatisfied_engines_fail_the_pipeline_before_any_step() {
    if !node_available() {
        eprintln!("skipping: node not available on PATH");
        return;
    }
    let strict = r#"{
  "name": "@juntai/strict-kit",
  "version": "1.0.0",
  "private": true,
  "engines": { "node": ">=99" }
}"#;
    let (_root, project) = fixture_workspace("engines", strict);

    let out = run_jumbo(&["build"], &project);
    assert!(
        !out.status.success(),
        "engines violation must fail the build"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("engines.node"), "{stderr}");
    assert!(stderr.contains(">=99"), "{stderr}");
    assert!(
        !project.join("package-lock.json").exists(),
        "no step may run before the engines check"
    );
    let _ = fs::remove_dir_all(&project);
}

/// A local fixture index carrying one record per internal scope: the
/// standard `@juntai/*` scope and the legacy-accepted `@zephytiju/*` scope.
fn fixture_index(root: &Path) -> PathBuf {
    let index_dir = root.join("index");
    fs::create_dir_all(&index_dir).expect("create index dir");
    fs::write(
        index_dir.join("juntai-demo-kit.jsonl"),
        r#"{"package":"@juntai/demo-kit","major":1,"version":"1.2.0","commit":"fedcba9876543210fedcba9876543210fedcba98","fingerprint":null,"canonicalExtract":null,"artifactUrl":null,"artifactSha256":null,"imageDigest":null,"buildId":null,"pipelineRun":null,"executor":"bootstrap","timestamp":"2026-09-01T00:00:00Z"}
"#,
    )
    .expect("write @juntai record");
    fs::write(
        index_dir.join("zephytiju-vangu-constructs.jsonl"),
        r#"{"package":"@zephytiju/vangu-constructs","major":1,"version":"1.3.0","commit":"0123456789abcdef0123456789abcdef01234567","fingerprint":null,"canonicalExtract":null,"artifactUrl":null,"artifactSha256":null,"imageDigest":null,"buildId":null,"pipelineRun":null,"executor":"bootstrap","timestamp":"2026-09-01T00:00:00Z"}
"#,
    )
    .expect("write @zephytiju record");
    index_dir
}

#[test]
fn lock_injection_and_extract_cover_both_accepted_scopes() {
    // No Node toolchain needed: --inject-only never runs npm.
    let root = temp_dir("lock-scopes");
    let index_dir = fixture_index(&root);
    let project = root.join("consumer");
    fs::create_dir_all(&project).expect("create project");
    fs::write(
        project.join("package.json"),
        r#"{
  "name": "consumer",
  "dependencies": {
    "@juntai/demo-kit": "^1",
    "@zephytiju/vangu-constructs": "~1",
    "lodash": "^4.17.21"
  }
}
"#,
    )
    .expect("write manifest");

    let out = run_jumbo(
        &[
            "lock",
            "--manifest",
            "package.json",
            "--index",
            &index_dir.display().to_string(),
            "--inject-only",
        ],
        &project,
    );
    assert!(
        out.status.success(),
        "lock failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON report");
    assert_eq!(report["ecosystem"], "npm");
    let injected = report["injectedSources"].as_array().unwrap();
    assert_eq!(injected.len(), 2, "both scopes resolve: {injected:?}");

    // Both scopes materialize at stable deps/<slug> paths and the manifest
    // points at them through file: sources.
    assert!(project.join("deps/juntai-demo-kit/package.json").exists());
    assert!(project
        .join("deps/zephytiju-vangu-constructs/package.json")
        .exists());
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(project.join("package.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["dependencies"]["@juntai/demo-kit"].as_str(),
        Some("file:deps/juntai-demo-kit")
    );
    assert_eq!(
        manifest["dependencies"]["@zephytiju/vangu-constructs"].as_str(),
        Some("file:deps/zephytiju-vangu-constructs")
    );
    assert_eq!(
        manifest["dependencies"]["lodash"].as_str(),
        Some("^4.17.21"),
        "third-party ranges pass through untouched"
    );

    // A package-lock.json carrying both injected coordinates and a
    // registry entry fingerprints with both scopes classified as index
    // sources — the same extract contract the Python path uses.
    let lock = r#"{
  "name": "consumer",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "requires": true,
  "packages": {
    "": { "name": "consumer", "version": "1.0.0" },
    "node_modules/@juntai/demo-kit": { "resolved": "deps/juntai-demo-kit", "link": true },
    "deps/juntai-demo-kit": { "name": "@juntai/demo-kit", "version": "1.2.0" },
    "node_modules/@zephytiju/vangu-constructs": { "resolved": "deps/zephytiju-vangu-constructs", "link": true },
    "deps/zephytiju-vangu-constructs": { "name": "@zephytiju/vangu-constructs", "version": "1.3.0" },
    "node_modules/lodash": {
      "version": "4.17.21",
      "resolved": "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz",
      "integrity": "sha512-v2yUIIQQAL05+013r3Ju1cHpIkB8Kf3GwPQmZCuUZZFeEhQqXcHA5ivoZ5S2srBIjPlFaaF500LglEuZHAkYuA=="
    }
  }
}"#;
    let repo = root.join("locked-repo");
    fs::create_dir_all(&repo).expect("create locked repo");
    fs::write(repo.join("package-lock.json"), lock).expect("write lock");
    run_git_init(&repo);

    let out = run_jumbo(&["fingerprint", "--lock", "package-lock.json"], &repo);
    assert!(
        out.status.success(),
        "fingerprint failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["ecosystem"], "npm");
    let entries = report["canonicalExtract"]["entries"].as_array().unwrap();
    assert!(entries
        .iter()
        .any(|entry| entry["name"] == "@juntai/demo-kit"
            && entry["source"] == "index"
            && entry["path"] == "deps/juntai-demo-kit"));
    assert!(entries
        .iter()
        .any(|entry| entry["name"] == "@zephytiju/vangu-constructs"
            && entry["source"] == "index"
            && entry["path"] == "deps/zephytiju-vangu-constructs"));
    assert!(entries
        .iter()
        .any(|entry| entry["name"] == "lodash" && entry["source"] == "npm"));
    let _ = fs::remove_dir_all(&root);
}
