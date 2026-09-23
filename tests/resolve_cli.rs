//! CLI integration tests for `jumbo resolve`.
//!
//! Every test runs the built `jumbo` binary against a local fixture index —
//! no network access, no credentials.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn jumbo_bin() -> &'static str {
    env!("CARGO_BIN_EXE_jumbo")
}

fn temp_workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "jumbo-resolve-cli-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp workspace");
    dir
}

/// One index record line with a plausible 40-hex commit.
fn record_line(package: &str, major: u64, version: &str, timestamp: &str) -> String {
    let seed: u64 = format!("{package}{major}{version}")
        .bytes()
        .fold(0, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
    let commit = format!("{seed:040x}");
    assert_eq!(commit.len(), 40);
    format!(
        r#"{{"package":"{package}","major":{major},"version":"{version}","commit":"{commit}","fingerprint":null,"canonicalExtract":null,"artifactUrl":null,"artifactSha256":null,"imageDigest":null,"buildId":null,"pipelineRun":null,"executor":"bootstrap","timestamp":"{timestamp}"}}"#
    )
}

/// Fixture index: `demo-alpha` with majors 1 and 2 (major-2 newest is the
/// last line even though an earlier line has a newer timestamp), an
/// `@juntai/demo-kit` npm package, and a bootstrap-pending `juntai-fuse-api`.
fn write_fixture_index(root: &Path) -> PathBuf {
    let index_dir = root.join("index");
    fs::create_dir_all(&index_dir).expect("create index dir");
    fs::write(
        index_dir.join("demo-alpha.jsonl"),
        [
            record_line("demo-alpha", 2, "2.0.0", "2026-01-01T00:00:00Z"),
            record_line("demo-alpha", 2, "2.1.0", "2026-08-01T00:00:00Z"), // newest wall clock
            record_line("demo-alpha", 1, "1.9.0", "2026-02-01T00:00:00Z"),
            record_line("demo-alpha", 2, "2.4.0", "2026-03-01T00:00:00Z"), // newest by order
        ]
        .join("\n")
            + "\n",
    )
    .expect("write demo-alpha");
    fs::write(
        index_dir.join("juntai-demo-kit.jsonl"),
        [record_line(
            "@juntai/demo-kit",
            1,
            "1.2.0",
            "2026-04-01T00:00:00Z",
        )]
        .join("\n")
            + "\n",
    )
    .expect("write juntai-demo-kit");
    fs::write(
        index_dir.join("zephytiju-legacy-kit.jsonl"),
        [record_line(
            "@zephytiju/legacy-kit",
            1,
            "1.0.4",
            "2026-04-02T00:00:00Z",
        )]
        .join("\n")
            + "\n",
    )
    .expect("write zephytiju-legacy-kit");
    fs::write(
        index_dir.join("juntai-fuse-api.jsonl"),
        [record_line(
            "juntai-fuse-api",
            2,
            "2.1.0",
            "2026-05-01T00:00:00Z",
        )]
        .join("\n")
            + "\n",
    )
    .expect("write juntai-fuse-api");
    index_dir
}

fn run_resolve(args: &[&str]) -> Output {
    Command::new(jumbo_bin())
        .args(args)
        .env_remove("JUMBO_INDEX_PATH")
        .env_remove("JUMBO_INDEX_URL")
        .output()
        .expect("run jumbo resolve")
}

fn stdout_json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).expect("stdout is valid JSON")
}

#[test]
fn resolve_declaration_returns_newest_record_of_declared_major() {
    let root = temp_workspace("single");
    let index_dir = write_fixture_index(&root);
    let out = run_resolve(&[
        "resolve",
        "demo-alpha@2",
        "--index",
        &index_dir.display().to_string(),
    ]);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = stdout_json(&out);
    assert_eq!(json["name"], "demo-alpha");
    assert_eq!(json["declaredMajor"], 2);
    assert_eq!(json["record"]["version"], "2.4.0"); // record order wins
    assert_eq!(json["record"]["major"], 2);
    assert_eq!(json["recordLine"], 4);
    assert_eq!(json["record"]["executor"], "bootstrap");

    // The collapsing range form resolves identically.
    let out = run_resolve(&[
        "resolve",
        "demo-alpha>=2,<3",
        "--index",
        &index_dir.display().to_string(),
    ]);
    assert!(out.status.success());
    assert_eq!(stdout_json(&out)["record"]["version"], "2.4.0");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resolve_declaration_supports_jumbo_python_syntax_and_npm_scope() {
    let root = temp_workspace("forms");
    let index_dir = write_fixture_index(&root);
    let index_arg = index_dir.display().to_string();

    let out = run_resolve(&["resolve", "juntai-fuse-api[http]@2", "--index", &index_arg]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_json(&out)["record"]["version"], "2.1.0");

    let out = run_resolve(&["resolve", "@juntai/demo-kit@^1", "--index", &index_arg]);
    assert!(out.status.success());
    assert_eq!(stdout_json(&out)["name"], "@juntai/demo-kit");
    assert_eq!(stdout_json(&out)["record"]["version"], "1.2.0");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resolve_declaration_missing_major_fails_with_available_majors() {
    let root = temp_workspace("major");
    let index_dir = write_fixture_index(&root);
    let out = run_resolve(&[
        "resolve",
        "demo-alpha@3",
        "--index",
        &index_dir.display().to_string(),
    ]);

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no major-3 record"), "stderr: {stderr}");
    assert!(stderr.contains("1, 2"), "stderr: {stderr}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn unabsorbed_internal_dependency_fails_with_absorption_error() {
    let root = temp_workspace("absorb");
    let index_dir = write_fixture_index(&root);
    let index_arg = index_dir.display().to_string();

    // Scoped npm internal package with no index record.
    let out = run_resolve(&["resolve", "@juntai/ghost-kit@^1", "--index", &index_arg]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("absorption error"), "stderr: {stderr}");
    assert!(stderr.contains("@juntai/ghost-kit"), "stderr: {stderr}");
    assert!(
        stderr.contains("https://github.com/zephytiju/JumboIndex"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("jumbo-publish"), "stderr: {stderr}");

    // Python jumbo-syntax declaration for an unabsorbed package.
    let out = run_resolve(&["resolve", "not-absorbed-pkg@1", "--index", &index_arg]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("absorption error"), "stderr: {stderr}");
    assert!(stderr.contains("not-absorbed-pkg"), "stderr: {stderr}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resolve_manifest_pyproject_resolves_and_validates() {
    let root = temp_workspace("pyproject");
    let index_dir = write_fixture_index(&root);
    let index_arg = index_dir.display().to_string();
    let manifest = root.join("pyproject.toml");

    // Happy path: internal by jumbo syntax + third-party pass-through.
    fs::write(
        &manifest,
        r#"
[project]
name = "consumer"
dependencies = [
    "juntai-fuse-api[http]@2",
    "numpy>=1.26",
]
"#,
    )
    .expect("write manifest");
    let out = run_resolve(&[
        "resolve",
        "--manifest",
        &manifest.display().to_string(),
        "--index",
        &index_arg,
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = stdout_json(&out);
    assert_eq!(json["ecosystem"], "python");
    assert_eq!(json["internal"].as_array().unwrap().len(), 1);
    assert_eq!(json["internal"][0]["name"], "juntai-fuse-api");
    assert_eq!(json["internal"][0]["record"]["version"], "2.1.0");
    assert_eq!(json["external"][0]["name"], "numpy");

    // git+https URL rejected.
    fs::write(
        &manifest,
        "[project]\ndependencies = [\"demo @ git+https://github.com/org/repo.git\"]\n",
    )
    .expect("write manifest");
    let out = run_resolve(&[
        "resolve",
        "--manifest",
        &manifest.display().to_string(),
        "--index",
        &index_arg,
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("forbidden"), "stderr: {stderr}");
    assert!(stderr.contains("Git URL"), "stderr: {stderr}");

    // Direct wheel URL rejected.
    fs::write(
        &manifest,
        "[project]\ndependencies = [\"demo @ https://example.org/demo-1.0.0-py3-none-any.whl\"]\n",
    )
    .expect("write manifest");
    let out = run_resolve(&[
        "resolve",
        "--manifest",
        &manifest.display().to_string(),
        "--index",
        &index_arg,
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("forbidden"));

    // Multi-major internal range rejected.
    fs::write(
        &manifest,
        "[project]\ndependencies = [\"demo-alpha>=1,<3\"]\n",
    )
    .expect("write manifest");
    let out = run_resolve(&[
        "resolve",
        "--manifest",
        &manifest.display().to_string(),
        "--index",
        &index_arg,
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not major-only"), "stderr: {stderr}");
    assert!(stderr.contains("spans multiple majors"), "stderr: {stderr}");

    // --check validates forms without failing on unabsorbed packages.
    fs::write(
        &manifest,
        "[project]\ndependencies = [\"unabsorbed-any-pkg@1\", \"numpy>=1.26\"]\n",
    )
    .expect("write manifest");
    let out = run_resolve(&[
        "resolve",
        "--manifest",
        &manifest.display().to_string(),
        "--index",
        &index_arg,
        "--check",
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = stdout_json(&out);
    assert_eq!(json["internal"][0]["name"], "unabsorbed-any-pkg");
    assert_eq!(json["internal"][0]["declaredMajor"], 1);
    assert_eq!(json["external"][0]["name"], "numpy");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resolve_manifest_package_json_resolves_and_validates() {
    let root = temp_workspace("npm");
    let index_dir = write_fixture_index(&root);
    let index_arg = index_dir.display().to_string();
    let manifest = root.join("package.json");

    // Happy path: internal scoped package (both ^1 and 1.x forms) + third-party.
    fs::write(
        &manifest,
        r#"{
  "name": "consumer",
  "dependencies": {
    "@juntai/demo-kit": "^1"
  },
  "devDependencies": {
    "@zephytiju/legacy-kit": "1.x",
    "typescript": "^5.5.0"
  }
}"#,
    )
    .expect("write manifest");
    let out = run_resolve(&[
        "resolve",
        "--manifest",
        &manifest.display().to_string(),
        "--index",
        &index_arg,
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = stdout_json(&out);
    assert_eq!(json["ecosystem"], "npm");
    assert_eq!(json["internal"].as_array().unwrap().len(), 2);
    assert_eq!(json["internal"][0]["name"], "@juntai/demo-kit");
    assert_eq!(json["internal"][0]["record"]["version"], "1.2.0");
    assert_eq!(json["internal"][1]["name"], "@zephytiju/legacy-kit");
    assert_eq!(json["internal"][1]["record"]["version"], "1.0.4");
    assert_eq!(json["external"][0]["name"], "typescript");

    // git+https URL rejected.
    fs::write(
        &manifest,
        r#"{"dependencies": {"@juntai/demo-kit": "git+https://github.com/org/kit.git"}}"#,
    )
    .expect("write manifest");
    let out = run_resolve(&[
        "resolve",
        "--manifest",
        &manifest.display().to_string(),
        "--index",
        &index_arg,
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("forbidden"));

    // Tarball URL rejected.
    fs::write(
        &manifest,
        r#"{"dependencies": {"@juntai/demo-kit": "https://github.com/org/kit/-/kit-1.0.0.tgz"}}"#,
    )
    .expect("write manifest");
    let out = run_resolve(&[
        "resolve",
        "--manifest",
        &manifest.display().to_string(),
        "--index",
        &index_arg,
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("forbidden"));

    // Multi-major internal range rejected.
    fs::write(
        &manifest,
        r#"{"dependencies": {"@juntai/demo-kit": ">=1,<3"}}"#,
    )
    .expect("write manifest");
    let out = run_resolve(&[
        "resolve",
        "--manifest",
        &manifest.display().to_string(),
        "--index",
        &index_arg,
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("spans multiple majors"));

    // Unabsorbed scoped internal package fails with the absorption error.
    fs::write(
        &manifest,
        r#"{"dependencies": {"@juntai/ghost-kit": "^2"}}"#,
    )
    .expect("write manifest");
    let out = run_resolve(&[
        "resolve",
        "--manifest",
        &manifest.display().to_string(),
        "--index",
        &index_arg,
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("absorption error"), "stderr: {stderr}");
    assert!(stderr.contains("@juntai/ghost-kit"), "stderr: {stderr}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn index_url_specs_are_rejected_before_any_network_access() {
    let root = temp_workspace("urlguard");
    let index_dir = write_fixture_index(&root);
    for bad in [
        "https://example.com/owner/repo",
        "https://localhost/owner/repo",
        "https://127.0.0.1/owner/repo",
        "http://github.com/zephytiju/JumboIndex",
        "git@github.com:zephytiju/JumboIndex.git",
    ] {
        let out = run_resolve(&["resolve", "demo-alpha@2", "--index", bad]);
        assert!(!out.status.success(), "`{bad}` should be rejected");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("cannot read the Jumbo index"),
            "`{bad}`: {stderr}"
        );
    }
    // A local path still works in the same position.
    let out = run_resolve(&[
        "resolve",
        "demo-alpha@2",
        "--index",
        &index_dir.display().to_string(),
    ]);
    assert!(out.status.success());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resolve_requires_declaration_or_manifest() {
    let out = run_resolve(&["resolve"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("DECLARATION"));

    let out = run_resolve(&["resolve", "pkg@1", "--manifest", "pyproject.toml"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be combined"));
}

#[test]
fn env_var_selects_local_index() {
    let root = temp_workspace("env");
    let index_dir = write_fixture_index(&root);
    let out = Command::new(jumbo_bin())
        .args(["resolve", "demo-alpha@2"])
        .env("JUMBO_INDEX_PATH", &index_dir)
        .env_remove("JUMBO_INDEX_URL")
        .output()
        .expect("run jumbo resolve");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_json(&out)["record"]["version"], "2.4.0");
    let _ = fs::remove_dir_all(&root);
}
