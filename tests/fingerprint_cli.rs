//! CLI integration tests for `jumbo lock` and `jumbo fingerprint`.
//!
//! Every test runs the built `jumbo` binary against local fixture indexes
//! and lock files inside throwaway Git repositories — no network access,
//! no credentials, no language tooling required.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn jumbo_bin() -> &'static str {
    env!("CARGO_BIN_EXE_jumbo")
}

fn temp_workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "jumbo-fingerprint-cli-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp workspace");
    dir
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

/// A committed repository containing the given files.
fn committed_repo(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let repo = temp_workspace(tag);
    for (name, content) in files {
        let path = repo.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, content).expect("write fixture file");
    }
    run_git(&repo, &["init", "-q", "--initial-branch=main"]);
    run_git(&repo, &["config", "user.email", "jumbo@test.invalid"]);
    run_git(&repo, &["config", "user.name", "Jumbo Test"]);
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "fixture"]);
    repo
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

fn stdout_json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).expect("stdout is valid JSON")
}

/// Two uv.lock files writing the same resolution in different orders and
/// formatting — a formatting-only change across uv versions.
const UV_LOCK_ONE: &str = r#"version = 1
requires-python = ">=3.12"

[[package]]
name = "consumer"
version = "0.1.0"
source = { editable = "." }

[[package]]
name = "numpy"
version = "1.26.4"
source = { registry = "https://pypi.org/simple" }
sdist = { url = "https://files.pythonhosted.org/numpy-1.26.4.tar.gz", hash = "sha256:aaaa1111aaaa", size = 1 }

[[package]]
name = "demo-alpha"
version = "2.4.0"
source = { directory = "deps/demo-alpha" }
"#;

const UV_LOCK_TWO: &str = r#"version = 1
requires-python = ">=3.12"

[[package]]
name = "demo-alpha"
version = "2.4.0"
source = { directory = "deps/demo-alpha" }

[[package]]
name = "consumer"
version = "0.1.0"
source = { editable = "." }

[[package]]
version = "1.26.4"
name = "numpy"
source = { registry = "https://pypi.org/simple" }
sdist = { hash = "sha256:aaaa1111aaaa", url = "https://files.pythonhosted.org/numpy-1.26.4.tar.gz", size = 1 }
"#;

/// The same npm resolution as lockfileVersion 3 and 2 (the v2 file also
/// carries the legacy `dependencies` mirror and a different entry order).
const NPM_LOCK_V3: &str = r#"{
  "name": "consumer",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "requires": true,
  "packages": {
    "": { "name": "consumer", "version": "1.0.0" },
    "node_modules/@juntai/demo-kit": { "resolved": "deps/juntai-demo-kit", "link": true },
    "deps/juntai-demo-kit": { "name": "@juntai/demo-kit", "version": "1.2.0" },
    "node_modules/lodash": {
      "version": "4.17.21",
      "resolved": "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz",
      "integrity": "sha512-v2yUIIQQAL05+013r3Ju1cHpIkB8Kf3GwPQmZCuUZZFeEhQqXcHA5ivoZ5S2srBIjPlFaaF500LglEuZHAkYuA=="
    }
  }
}"#;

const NPM_LOCK_V2: &str = r#"{
  "name": "consumer",
  "version": "1.0.0",
  "lockfileVersion": 2,
  "requires": true,
  "packages": {
    "node_modules/lodash": {
      "version": "4.17.21",
      "resolved": "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz",
      "integrity": "sha512-v2yUIIQQAL05+013r3Ju1cHpIkB8Kf3GwPQmZCuUZZFeEhQqXcHA5ivoZ5S2srBIjPlFaaF500LglEuZHAkYuA=="
    },
    "deps/juntai-demo-kit": { "name": "@juntai/demo-kit", "version": "1.2.0" },
    "node_modules/@juntai/demo-kit": { "resolved": "deps/juntai-demo-kit", "link": true },
    "": { "name": "consumer", "version": "1.0.0" }
  },
  "dependencies": {
    "@juntai/demo-kit": { "version": "1.2.0", "resolved": "deps/juntai-demo-kit", "link": true },
    "lodash": {
      "version": "4.17.21",
      "resolved": "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz",
      "integrity": "sha512-v2yUIIQQAL05+013r3Ju1cHpIkB8Kf3GwPQmZCuUZZFeEhQqXcHA5ivoZ5S2srBIjPlFaaF500LglEuZHAkYuA=="
    }
  }
}"#;

fn fixture_index(root: &Path) -> PathBuf {
    let index_dir = root.join("index");
    fs::create_dir_all(&index_dir).expect("create index dir");
    fs::write(
        index_dir.join("demo-alpha.jsonl"),
        r#"{"package":"demo-alpha","major":2,"version":"2.4.0","commit":"0123456789abcdef0123456789abcdef01234567","fingerprint":null,"canonicalExtract":null,"artifactUrl":null,"artifactSha256":null,"imageDigest":null,"buildId":null,"pipelineRun":null,"executor":"bootstrap","timestamp":"2026-09-01T00:00:00Z"}
"#,
    )
    .expect("write demo-alpha");
    fs::write(
        index_dir.join("juntai-demo-kit.jsonl"),
        r#"{"package":"@juntai/demo-kit","major":1,"version":"1.2.0","commit":"fedcba9876543210fedcba9876543210fedcba98","fingerprint":null,"canonicalExtract":null,"artifactUrl":null,"artifactSha256":null,"imageDigest":null,"buildId":null,"pipelineRun":null,"executor":"bootstrap","timestamp":"2026-09-01T00:00:00Z"}
"#,
    )
    .expect("write juntai-demo-kit");
    index_dir
}

#[test]
fn formatting_only_uv_lock_change_produces_no_fingerprint_change() {
    let root = temp_workspace("uv-formatting");
    let _index_dir = fixture_index(&root);
    let repo = committed_repo(
        "uv-formatting",
        &[
            ("uv.lock", UV_LOCK_ONE),
            ("pyproject.toml", "[project]\nname = \"consumer\"\n"),
        ],
    );

    // Same own commit, first formatting of the resolution.
    let one = run_jumbo(&["fingerprint", "--lock", "uv.lock"], &repo);
    assert!(
        one.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&one.stderr)
    );

    // The same resolution written by a different uv formatting version:
    // package order, key order, and array layout all differ. Overwriting
    // the lock dirties the tree, but a pure-local query never promotes.
    fs::write(repo.join("uv.lock"), UV_LOCK_TWO).expect("write formatting variant");
    let two = run_jumbo(&["fingerprint", "--lock", "uv.lock"], &repo);
    assert!(two.status.success());

    let json_one = stdout_json(&one);
    let json_two = stdout_json(&two);
    assert_eq!(json_one["ecosystem"], "python");
    // Identical commit, extract, and fingerprint despite different lock bytes.
    assert_eq!(json_one["commit"], json_two["commit"]);
    assert_eq!(json_one["canonicalExtract"], json_two["canonicalExtract"]);
    assert_eq!(json_one["fingerprint"], json_two["fingerprint"]);
    let fp = json_one["fingerprint"].as_str().unwrap();
    assert_eq!(fp.len(), 64);
    assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));

    // The report shape matches the documented contract.
    assert!(json_one["commit"].as_str().unwrap().len() == 40);
    assert_eq!(json_one["promotion"], false);
    assert_eq!(
        json_one["canonicalExtract"]["format"],
        "jumbo-canonical-extract/1"
    );
    let entries = json_one["canonicalExtract"]["entries"].as_array().unwrap();
    assert!(entries.iter().any(|e| e["name"] == "demo-alpha"
        && e["source"] == "index"
        && e["path"] == "deps/demo-alpha"));
    assert!(entries.iter().any(|e| e["name"] == "numpy"
        && e["source"] == "pypi"
        && e["digest"] == "sha256:aaaa1111aaaa"));

    // A real resolution change (numpy 1.26.4 → 1.27.0) DOES change the
    // fingerprint for the same commit.
    fs::write(
        repo.join("uv.lock"),
        UV_LOCK_ONE.replace("1.26.4", "1.27.0"),
    )
    .expect("bump numpy");
    let bumped = run_jumbo(&["fingerprint", "--lock", "uv.lock"], &repo);
    assert!(bumped.status.success());
    assert_ne!(stdout_json(&bumped)["fingerprint"], json_one["fingerprint"]);
    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&root));
}

#[test]
fn npm_lock_v2_and_v3_yield_the_same_fingerprint() {
    let repo = committed_repo("npm-versions", &[("package-lock.json", NPM_LOCK_V3)]);

    let v3 = run_jumbo(&["fingerprint", "--lock", "package-lock.json"], &repo);
    assert!(
        v3.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&v3.stderr)
    );

    // The same resolution written by an older npm (lockfileVersion 2 with
    // the legacy `dependencies` mirror), at the same own commit.
    fs::write(repo.join("package-lock.json"), NPM_LOCK_V2).expect("write v2 lock");
    let v2 = run_jumbo(&["fingerprint", "--lock", "package-lock.json"], &repo);
    assert!(v2.status.success());

    let json_v3 = stdout_json(&v3);
    let json_v2 = stdout_json(&v2);
    assert_eq!(json_v3["ecosystem"], "npm");
    assert_eq!(json_v3["commit"], json_v2["commit"]);
    assert_eq!(json_v3["canonicalExtract"], json_v2["canonicalExtract"]);
    assert_eq!(json_v3["fingerprint"], json_v2["fingerprint"]);
    let _ = fs::remove_dir_all(&repo);
}

#[test]
fn promotion_refused_on_dirty_tree_and_allowed_when_clean() {
    let repo = committed_repo("promote", &[("uv.lock", UV_LOCK_ONE)]);

    // Clean tree: promotion mode succeeds.
    let clean = run_jumbo(&["fingerprint", "--lock", "uv.lock", "--promote"], &repo);
    assert!(
        clean.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&clean.stderr)
    );
    assert_eq!(stdout_json(&clean)["promotion"], true);

    // Dirty tracked file: promotion mode refuses before computing.
    fs::write(repo.join("source.py"), "dirty = True\n").expect("write dirty source");
    let dirty = run_jumbo(&["fingerprint", "--lock", "uv.lock", "--promote"], &repo);
    assert!(!dirty.status.success());
    let stderr = String::from_utf8_lossy(&dirty.stderr);
    assert!(stderr.contains("promotion refused"), "stderr: {stderr}");
    assert!(stderr.contains("source.py"), "stderr: {stderr}");
    assert!(
        stderr.contains("dirty working trees never promote"),
        "stderr: {stderr}"
    );
    // Nothing was printed as a report.
    assert!(dirty.stdout.is_empty());

    // The same dirty tree is fine for a pure-local query.
    let local = run_jumbo(&["fingerprint", "--lock", "uv.lock"], &repo);
    assert!(local.status.success());
    let json = stdout_json(&local);
    assert_eq!(json["promotion"], false);
    assert_eq!(json["treeClean"], false);
    assert!(json["dirtyPaths"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p == "source.py"));
    let _ = fs::remove_dir_all(&repo);
}

#[test]
fn lock_inject_only_materializes_stable_relative_paths() {
    let root = temp_workspace("lock-inject");
    let index_dir = fixture_index(&root);
    let project = root.join("consumer");
    fs::create_dir_all(&project).expect("create project");
    fs::write(
        project.join("pyproject.toml"),
        "[project]\nname = \"consumer\"\ndependencies = [\"demo-alpha@2\", \"numpy>=1.26\"]\n",
    )
    .expect("write manifest");

    let out = run_jumbo(
        &[
            "lock",
            "--manifest",
            "pyproject.toml",
            "--index",
            &index_dir.display().to_string(),
            "--inject-only",
        ],
        &project,
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = stdout_json(&out);
    assert_eq!(json["ecosystem"], "python");
    assert_eq!(json["toolRan"], false);
    assert!(
        json["lock"]
            .as_str()
            .unwrap()
            .ends_with("/consumer/uv.lock"),
        "lock path: {}",
        json["lock"]
    );
    let injected = json["injectedSources"].as_array().unwrap();
    assert_eq!(injected.len(), 1);
    assert_eq!(injected[0]["name"], "demo-alpha");
    assert_eq!(injected[0]["path"], "deps/demo-alpha");
    assert_eq!(injected[0]["version"], "2.4.0");

    // Stable relative paths on disk + marker + rewritten manifest.
    assert!(project.join("deps/demo-alpha/pyproject.toml").exists());
    assert!(project.join("deps/.jumbo-sources.json").exists());
    let manifest = fs::read_to_string(project.join("pyproject.toml")).unwrap();
    assert!(manifest.contains("\"demo-alpha==2.4.0\""));
    assert!(manifest.contains("path = \"deps/demo-alpha\""));

    // Idempotent: running again yields the same manifest bytes.
    let again = run_jumbo(
        &[
            "lock",
            "--manifest",
            "pyproject.toml",
            "--index",
            &index_dir.display().to_string(),
            "--inject-only",
        ],
        &project,
    );
    assert!(again.status.success());
    let manifest_again = fs::read_to_string(project.join("pyproject.toml")).unwrap();
    assert_eq!(manifest, manifest_again, "second run must be identical");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn lock_and_fingerprint_argument_validation() {
    let repo = committed_repo("args", &[("uv.lock", UV_LOCK_ONE)]);

    // --lock and --manifest cannot be combined.
    let out = run_jumbo(
        &[
            "fingerprint",
            "--lock",
            "uv.lock",
            "--manifest",
            "pyproject.toml",
        ],
        &repo,
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be combined"));

    // Missing lock file.
    let out = run_jumbo(&["fingerprint", "--lock", "missing.lock"], &repo);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not found"));

    // No manifest and no lock in an empty directory.
    let empty = temp_workspace("empty");
    let out = run_jumbo(&["fingerprint"], &empty);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--manifest"));
    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&empty));
}
