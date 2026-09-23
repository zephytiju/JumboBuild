//! CLI integration tests for `jumbo dedup`: the build-or-reuse decision
//! against the index by fingerprint and artifact materialization.
//!
//! Every test runs the built `jumbo` binary against local fixture indexes
//! and local fixture artifacts (the `--artifact-dir` cache — the same
//! verified-digest path CI asset caches use) inside throwaway Git
//! repositories — no network access, no credentials, no language tooling
//! required. A PATH shim over `uv`/`npm` proves the reuse path performs
//! zero source rebuilds.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn jumbo_bin() -> &'static str {
    env!("CARGO_BIN_EXE_jumbo")
}

fn temp_workspace(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "jumbo-dedup-cli-{tag}-{seq}-{}-{}",
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
        .env_remove("JUMBO_ARTIFACT_DIR")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .output()
        .expect("run jumbo")
}

fn stdout_json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).expect("stdout is valid JSON")
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// The consumer's lock: root `consumer`, the internal `demo-alpha` as a
/// jumbo-injected source coordinate, and one registry dependency.
const UV_LOCK: &str = r#"version = 1
requires-python = ">=3.12"

[[package]]
name = "consumer"
version = "0.1.0"
source = { editable = "." }

[[package]]
name = "demo-alpha"
version = "2.4.0"
source = { directory = "deps/demo-alpha" }

[[package]]
name = "numpy"
version = "1.26.4"
source = { registry = "https://pypi.org/simple" }
sdist = { url = "https://files.pythonhosted.org/numpy-1.26.4.tar.gz", hash = "sha256:aaaa1111aaaa", size = 1 }
"#;

const PYPROJECT: &str = "[project]\nname = \"consumer\"\ndependencies = [\n    \"demo-alpha@2\",\n    \"numpy>=1.26\",\n]\n";

/// The consumer's lock when it also depends on the artifact-less
/// `demo-beta@1` — the shape the `--deps` projects commit.
const UV_LOCK_WITH_BETA: &str = r#"version = 1
requires-python = ">=3.12"

[[package]]
name = "consumer"
version = "0.1.0"
source = { editable = "." }

[[package]]
name = "demo-alpha"
version = "2.4.0"
source = { directory = "deps/demo-alpha" }

[[package]]
name = "demo-beta"
version = "1.0.0"
source = { directory = "deps/demo-beta" }

[[package]]
name = "numpy"
version = "1.26.4"
source = { registry = "https://pypi.org/simple" }
sdist = { url = "https://files.pythonhosted.org/numpy-1.26.4.tar.gz", hash = "sha256:aaaa1111aaaa", size = 1 }
"#;

const PYPROJECT_WITH_BETA: &str = "[project]\nname = \"consumer\"\ndependencies = [\n    \"demo-alpha@2\",\n    \"demo-beta@1\",\n    \"numpy>=1.26\",\n]\n";

const NPM_LOCK: &str = r#"{
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

const CONSUMER_WHEEL: &[u8] = b"consumer 0.1.0 wheel bytes (fixture artifact)\n";
const CONSUMER_WHEEL_SHA: &str = "9e57c250a17576e34ae374e277b54c7c393b45fb0b338fdecac4f5198dc80163";
const DEMO_ALPHA_WHEEL: &[u8] = b"demo-alpha 2.4.0 wheel bytes (fixture artifact)\n";
const DEMO_ALPHA_WHEEL_SHA: &str =
    "80ba8d910e6b168622c3131d56d264af0bd2d81fac511cecbb79a827e8c8dfe8";
const WRONG_BYTES: &[u8] = b"these are the wrong bytes\n";

const CONSUMER_WHEEL_URL: &str =
    "https://github.com/acme/consumer/releases/download/v0.1.0/consumer-0.1.0-py3-none-any.whl";
const DEMO_ALPHA_WHEEL_URL: &str =
    "https://github.com/acme/demo-alpha/releases/download/v2.4.0/demo_alpha-2.4.0-py3-none-any.whl";

/// One index record line.
fn record_json(
    package: &str,
    version: &str,
    fingerprint: Option<&str>,
    artifact_url: Option<&str>,
    artifact_sha256: Option<&str>,
) -> String {
    serde_json::json!({
        "package": package,
        "major": version.split('.').next().and_then(|m| m.parse::<u64>().ok()).unwrap_or(0),
        "version": version,
        "commit": "0123456789abcdef0123456789abcdef01234567",
        "fingerprint": fingerprint,
        "canonicalExtract": null,
        "artifactUrl": artifact_url,
        "artifactSha256": artifact_sha256,
        "imageDigest": null,
        "buildId": format!("{package}-{version}-001"),
        "pipelineRun": Some("circleci/run-42"),
        "executor": "circleci",
        "timestamp": "2026-09-20T00:00:00Z",
    })
    .to_string()
}

/// A fixture index with `consumer`, `demo-alpha` (with artifact), and
/// `demo-beta` (bootstrap, no artifact) records; returns the index dir.
fn fixture_index(root: &Path, consumer: Option<(&str, Option<&str>, Option<&str>)>) -> PathBuf {
    let index_dir = root.join("index");
    fs::create_dir_all(&index_dir).expect("create index dir");
    let (consumer_fp, consumer_url, consumer_sha) = match consumer {
        Some((fp, url, sha)) => (Some(fp), url, sha),
        None => (None, None, None),
    };
    fs::write(
        index_dir.join("consumer.jsonl"),
        record_json("consumer", "0.1.0", consumer_fp, consumer_url, consumer_sha) + "\n",
    )
    .expect("write consumer");
    fs::write(
        index_dir.join("demo-alpha.jsonl"),
        record_json(
            "demo-alpha",
            "2.4.0",
            Some(&"e".repeat(64)),
            Some(DEMO_ALPHA_WHEEL_URL),
            Some(DEMO_ALPHA_WHEEL_SHA),
        ) + "\n",
    )
    .expect("write demo-alpha");
    fs::write(
        index_dir.join("demo-beta.jsonl"),
        record_json("demo-beta", "1.0.0", None, None, None) + "\n",
    )
    .expect("write demo-beta");
    index_dir
}

/// The computed fingerprint of the consumer's committed lock.
fn computed_fingerprint(repo: &Path) -> String {
    let out = run_jumbo(&["fingerprint", "--lock", "uv.lock"], repo);
    assert!(
        out.status.success(),
        "fingerprint failed: {}",
        stderr_of(&out)
    );
    stdout_json(&out)["fingerprint"]
        .as_str()
        .expect("fingerprint string")
        .to_string()
}

#[test]
fn decision_reuse_on_hit_build_on_miss_and_null_never_matches() {
    let root = temp_workspace("decision");
    let repo = committed_repo(
        "decision",
        &[("uv.lock", UV_LOCK), ("pyproject.toml", PYPROJECT)],
    );
    let fingerprint = computed_fingerprint(&repo);

    // Hit: a record of `consumer` carries the identical fingerprint.
    let hit_index = fixture_index(&root, Some((&fingerprint, None, None)));
    let out = run_jumbo(
        &[
            "dedup",
            "--lock",
            "uv.lock",
            "--index",
            &hit_index.display().to_string(),
        ],
        &repo,
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["package"], "consumer"); // derived from the lock root
    assert_eq!(json["ecosystem"], "python");
    assert_eq!(json["fingerprint"], fingerprint.as_str());
    assert_eq!(json["duplicate"], true);
    assert_eq!(json["action"], "reuse");
    assert_eq!(json["recordsSearched"], 1);
    assert_eq!(json["matchedRecord"]["record"]["version"], "0.1.0");
    assert_eq!(json["matchedRecord"]["recordLine"], 1);

    // Miss: a different recorded fingerprint means build from source.
    let other = "f".repeat(64);
    let miss_index = fixture_index(&root, Some((other.as_str(), None, None)));
    let out = run_jumbo(
        &[
            "dedup",
            "--lock",
            "uv.lock",
            "--index",
            &miss_index.display().to_string(),
        ],
        &repo,
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["duplicate"], false);
    assert_eq!(json["action"], "build");
    assert!(json["matchedRecord"].is_null());

    // Null fingerprints (bootstrap records) never match.
    let null_index = fixture_index(&root, None);
    let out = run_jumbo(
        &[
            "dedup",
            "--lock",
            "uv.lock",
            "--index",
            &null_index.display().to_string(),
        ],
        &repo,
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["duplicate"], false);
    assert_eq!(json["action"], "build");

    // An explicit --package override searching a different history.
    let out = run_jumbo(
        &[
            "dedup",
            "--lock",
            "uv.lock",
            "--package",
            "demo-alpha",
            "--index",
            &hit_index.display().to_string(),
        ],
        &repo,
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["package"], "demo-alpha");
    assert_eq!(
        json["duplicate"], false,
        "consumer's fp must not hit demo-alpha"
    );
    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&root));
}

#[test]
fn materialize_pulls_the_recorded_artifact_into_dist() {
    let root = temp_workspace("self");
    let repo = committed_repo(
        "self",
        &[("uv.lock", UV_LOCK), ("pyproject.toml", PYPROJECT)],
    );
    let fingerprint = computed_fingerprint(&repo);
    let index_dir = fixture_index(
        &root,
        Some((
            &fingerprint,
            Some(CONSUMER_WHEEL_URL),
            Some(CONSUMER_WHEEL_SHA),
        )),
    );
    let cache = root.join("cache");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::write(
        cache.join("consumer-0.1.0-py3-none-any.whl"),
        CONSUMER_WHEEL,
    )
    .expect("wheel fixture");

    let args = |index: &Path| -> Vec<String> {
        vec![
            "dedup".into(),
            "--lock".into(),
            "uv.lock".into(),
            "--index".into(),
            index.display().to_string(),
            "--materialize".into(),
            "--artifact-dir".into(),
            cache.display().to_string(),
        ]
    };
    let run_with = |argv: &[String]| {
        let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        run_jumbo(&refs, &repo)
    };
    let argv = args(&index_dir);
    let out = run_with(&argv);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["duplicate"], true);
    assert_eq!(json["action"], "reuse");
    assert_eq!(
        json["materialized"]["path"],
        "dist/consumer-0.1.0-py3-none-any.whl"
    );
    assert_eq!(json["materialized"]["sha256"], CONSUMER_WHEEL_SHA);
    assert_eq!(json["materialized"]["url"], CONSUMER_WHEEL_URL);
    assert_eq!(json["materialized"]["buildId"], "consumer-0.1.0-001");

    // The pulled artifact is bit-for-bit the recorded bytes.
    let pulled = repo.join("dist/consumer-0.1.0-py3-none-any.whl");
    assert_eq!(fs::read(&pulled).expect("pulled wheel"), CONSUMER_WHEEL);

    // Repeat run: identical decision, identical bytes (idempotent reuse).
    let out = run_with(&argv);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_json(&out)["materialized"]["sha256"],
        CONSUMER_WHEEL_SHA
    );
    assert_eq!(
        fs::read(&pulled).expect("pulled wheel again"),
        CONSUMER_WHEEL
    );

    // A miss with --materialize is not an error: build from source.
    let other = "f".repeat(64);
    let miss_index = fixture_index(&root, Some((other.as_str(), None, None)));
    let clean = committed_repo(
        "self-miss",
        &[("uv.lock", UV_LOCK), ("pyproject.toml", PYPROJECT)],
    );
    let mut miss_argv = args(&miss_index);
    miss_argv.pop(); // the artifact-dir value
    miss_argv.pop(); // the artifact-dir flag: nothing is pulled on a miss
    let refs: Vec<&str> = miss_argv.iter().map(|s| s.as_str()).collect();
    let out = run_jumbo(&refs, &clean);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(stdout_json(&out)["action"], "build");
    assert!(!clean.join("dist").exists());
    let _ = (
        fs::remove_dir_all(&repo),
        fs::remove_dir_all(&root),
        fs::remove_dir_all(&clean),
    );
}

#[test]
fn digest_mismatch_aborts_and_nothing_is_materialized() {
    let root = temp_workspace("mismatch");
    let repo = committed_repo(
        "mismatch",
        &[("uv.lock", UV_LOCK), ("pyproject.toml", PYPROJECT)],
    );
    let fingerprint = computed_fingerprint(&repo);
    // The record carries the digest of the real wheel; the cache holds
    // different bytes.
    let index_dir = fixture_index(
        &root,
        Some((
            &fingerprint,
            Some(CONSUMER_WHEEL_URL),
            Some(CONSUMER_WHEEL_SHA),
        )),
    );
    let cache = root.join("cache");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::write(cache.join("consumer-0.1.0-py3-none-any.whl"), WRONG_BYTES).expect("wrong bytes");

    let out = run_jumbo(
        &[
            "dedup",
            "--lock",
            "uv.lock",
            "--index",
            &index_dir.display().to_string(),
            "--materialize",
            "--artifact-dir",
            &cache.display().to_string(),
        ],
        &repo,
    );
    assert!(!out.status.success(), "a digest mismatch must abort");
    let stderr = stderr_of(&out);
    assert!(stderr.contains("digest mismatch"), "stderr: {stderr}");
    assert!(stderr.contains(CONSUMER_WHEEL_SHA), "stderr: {stderr}");
    assert!(
        stderr.contains("ee32a6b544dcf50ee1aa45557e5e87ffc0ce2e72b60103c89de8f830cb9d5164"),
        "stderr: {stderr}"
    );
    // Never proceed on unverifiable bytes: nothing was materialized.
    assert!(!repo.join("dist").exists());
    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&root));
}

#[test]
fn missing_recorded_sha256_aborts() {
    let root = temp_workspace("noseha");
    let repo = committed_repo(
        "noseha",
        &[("uv.lock", UV_LOCK), ("pyproject.toml", PYPROJECT)],
    );
    let fingerprint = computed_fingerprint(&repo);
    let index_dir = fixture_index(&root, Some((&fingerprint, Some(CONSUMER_WHEEL_URL), None)));
    let cache = root.join("cache");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::write(
        cache.join("consumer-0.1.0-py3-none-any.whl"),
        CONSUMER_WHEEL,
    )
    .expect("wheel fixture");

    let out = run_jumbo(
        &[
            "dedup",
            "--lock",
            "uv.lock",
            "--index",
            &index_dir.display().to_string(),
            "--materialize",
            "--artifact-dir",
            &cache.display().to_string(),
        ],
        &repo,
    );
    assert!(!out.status.success(), "a missing sha256 must abort");
    let stderr = stderr_of(&out);
    assert!(stderr.contains("no artifactSha256"), "stderr: {stderr}");
    assert!(
        stderr.contains("never proceed on unverifiable bytes"),
        "stderr: {stderr}"
    );
    assert!(!repo.join("dist").exists());
    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&root));
}

#[test]
fn unsafe_artifact_urls_are_rejected_before_any_download() {
    let root = temp_workspace("urls");
    let repo = committed_repo(
        "urls",
        &[("uv.lock", UV_LOCK), ("pyproject.toml", PYPROJECT)],
    );
    let fingerprint = computed_fingerprint(&repo);
    for bad in [
        "http://github.com/acme/c/releases/download/v0.1.0/c.whl",
        "https://evil.com/c.whl",
        "https://github.com.attacker.io/c.whl",
        "https://localhost/c.whl",
        "https://127.0.0.1/c.whl",
        "https://169.254.169.254/latest/meta-data",
        "https://user:secret@github.com/acme/c/releases/download/v0.1.0/c.whl",
        "https://github.com:8443/acme/c/releases/download/v0.1.0/c.whl",
        "ftp://github.com/c.whl",
    ] {
        let index_dir = fixture_index(
            &root,
            Some((&fingerprint, Some(bad), Some(CONSUMER_WHEEL_SHA))),
        );
        let out = run_jumbo(
            &[
                "dedup",
                "--lock",
                "uv.lock",
                "--index",
                &index_dir.display().to_string(),
                "--materialize",
            ],
            &repo,
        );
        assert!(!out.status.success(), "`{bad}` must be rejected");
        let stderr = stderr_of(&out);
        assert!(stderr.contains("not allowed"), "`{bad}`: {stderr}");
        assert!(!repo.join("dist").exists(), "`{bad}`: nothing materialized");
    }

    // A hit whose record published no artifact is a typed error, too.
    let no_artifact = fixture_index(&root, Some((&fingerprint, None, None)));
    let out = run_jumbo(
        &[
            "dedup",
            "--lock",
            "uv.lock",
            "--index",
            &no_artifact.display().to_string(),
            "--materialize",
        ],
        &repo,
    );
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("no artifactUrl"),
        "stderr: {}",
        stderr_of(&out)
    );
    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&root));
}

#[test]
fn python_dependency_artifact_replaces_the_source_overlay() {
    let root = temp_workspace("pydeps");
    let index_dir = fixture_index(&root, None);
    let cache = root.join("cache");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::write(
        cache.join("demo_alpha-2.4.0-py3-none-any.whl"),
        DEMO_ALPHA_WHEEL,
    )
    .expect("wheel fixture");
    let project = committed_repo(
        "pydeps",
        &[
            ("pyproject.toml", PYPROJECT_WITH_BETA),
            ("uv.lock", UV_LOCK_WITH_BETA),
        ],
    );

    let run = || {
        run_jumbo(
            &[
                "dedup",
                "--manifest",
                "pyproject.toml",
                "--index",
                &index_dir.display().to_string(),
                "--deps",
                "--artifact-dir",
                &cache.display().to_string(),
            ],
            &project,
        )
    };
    let out = run();
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    let deps = &json["dependencies"];
    let materialized = deps["materialized"].as_array().expect("materialized");
    assert_eq!(materialized.len(), 1);
    assert_eq!(materialized[0]["package"], "demo-alpha");
    assert_eq!(
        materialized[0]["path"],
        "deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl"
    );
    assert_eq!(materialized[0]["sha256"], DEMO_ALPHA_WHEEL_SHA);
    // demo-beta published no artifact: its source overlay is kept.
    assert_eq!(deps["keptSourceOverlays"], serde_json::json!(["demo-beta"]));

    // The wheel replaced the synthetic source-overlay project, at J3's
    // stable deps/<slug> coordinate; the manifest points uv at the wheel.
    let wheel = project.join("deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl");
    assert_eq!(fs::read(&wheel).expect("wheel"), DEMO_ALPHA_WHEEL);
    assert!(!project.join("deps/demo-alpha/pyproject.toml").exists());
    assert!(project.join("deps/demo-beta/pyproject.toml").exists());
    let manifest = fs::read_to_string(project.join("pyproject.toml")).unwrap();
    let doc: toml::Table = manifest.parse().expect("parse manifest");
    assert_eq!(
        doc["tool"]["uv"]["sources"]["demo-alpha"]["path"]
            .as_str()
            .unwrap(),
        "deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl"
    );
    let dep_strings: Vec<String> = doc["project"]["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d.as_str().map(str::to_string))
        .collect();
    assert!(dep_strings.contains(&"demo-alpha==2.4.0".to_string()));
    assert!(dep_strings.contains(&"demo-beta==1.0.0".to_string()));

    // The materialization marker records the pull.
    let marker: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project.join("deps/.jumbo-artifacts.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(marker["format"], "jumbo-artifact-materialization/1");
    assert_eq!(marker["artifacts"][0]["package"], "demo-alpha");

    // Idempotent: a second run produces the same manifest bytes.
    let once = manifest;
    let out = run();
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(
        fs::read_to_string(project.join("pyproject.toml")).unwrap(),
        once,
        "second materialization must be byte-identical"
    );
    let _ = fs::remove_dir_all(&root);
}

const PACKAGE_JSON: &str = r#"{
  "name": "consumer",
  "dependencies": {
    "@juntai/demo-kit": "^1",
    "lodash": "^4.17.21"
  }
}
"#;

const DEMO_KIT_TGZ: &[u8] = b"juntai-demo-kit 1.2.0 tarball bytes (fixture artifact)\n";
const DEMO_KIT_TGZ_SHA: &str = "30fc2b28d74b80d738f2e606f16356591ef44a46aed6b257dcaae6b9b4da0cdc";

#[test]
fn npm_dependency_artifact_enters_via_file_sources() {
    let root = temp_workspace("npmdeps");
    let index_dir = root.join("index");
    fs::create_dir_all(&index_dir).expect("index dir");
    fs::write(
        index_dir.join("juntai-demo-kit.jsonl"),
        record_json(
            "@juntai/demo-kit",
            "1.2.0",
            Some(&"e".repeat(64)),
            Some("https://github.com/acme/demo-kit/releases/download/v1.2.0/juntai-demo-kit-1.2.0.tgz"),
            Some(DEMO_KIT_TGZ_SHA),
        ) + "\n",
    )
    .expect("write demo-kit");
    let cache = root.join("cache");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::write(cache.join("juntai-demo-kit-1.2.0.tgz"), DEMO_KIT_TGZ).expect("tgz fixture");
    let project = committed_repo(
        "npmdeps",
        &[
            ("package.json", PACKAGE_JSON),
            ("package-lock.json", NPM_LOCK),
        ],
    );

    let run = || {
        run_jumbo(
            &[
                "dedup",
                "--manifest",
                "package.json",
                "--index",
                &index_dir.display().to_string(),
                "--deps",
                "--artifact-dir",
                &cache.display().to_string(),
            ],
            &project,
        )
    };
    let out = run();
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["ecosystem"], "npm");
    let materialized = json["dependencies"]["materialized"]
        .as_array()
        .expect("materialized");
    assert_eq!(materialized.len(), 1);
    assert_eq!(materialized[0]["package"], "@juntai/demo-kit");
    assert_eq!(
        materialized[0]["path"],
        "deps/juntai-demo-kit/juntai-demo-kit-1.2.0.tgz"
    );

    // The tarball replaced the source overlay; the manifest references it
    // via the file: protocol — no registry was contacted.
    let tarball = project.join("deps/juntai-demo-kit/juntai-demo-kit-1.2.0.tgz");
    assert_eq!(fs::read(&tarball).expect("tarball"), DEMO_KIT_TGZ);
    assert!(!project.join("deps/juntai-demo-kit/package.json").exists());
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(project.join("package.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["dependencies"]["@juntai/demo-kit"]
            .as_str()
            .unwrap(),
        "file:deps/juntai-demo-kit/juntai-demo-kit-1.2.0.tgz"
    );
    assert_eq!(
        manifest["dependencies"]["lodash"].as_str().unwrap(),
        "^4.17.21"
    );

    // Idempotent.
    let once = fs::read_to_string(project.join("package.json")).unwrap();
    let out = run();
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(
        fs::read_to_string(project.join("package.json")).unwrap(),
        once,
        "second materialization must be byte-identical"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn dependency_digest_mismatch_aborts_before_any_mutation() {
    let root = temp_workspace("depmismatch");
    let index_dir = fixture_index(&root, None);
    let cache = root.join("cache");
    fs::create_dir_all(&cache).expect("cache dir");
    // The cache holds bytes that do not match the recorded digest.
    fs::write(cache.join("demo_alpha-2.4.0-py3-none-any.whl"), WRONG_BYTES).expect("wrong bytes");
    let project = committed_repo(
        "depmismatch",
        &[
            ("pyproject.toml", PYPROJECT_WITH_BETA),
            ("uv.lock", UV_LOCK_WITH_BETA),
        ],
    );

    let out = run_jumbo(
        &[
            "dedup",
            "--manifest",
            "pyproject.toml",
            "--index",
            &index_dir.display().to_string(),
            "--deps",
            "--artifact-dir",
            &cache.display().to_string(),
        ],
        &project,
    );
    assert!(!out.status.success(), "a digest mismatch must abort");
    let stderr = stderr_of(&out);
    assert!(stderr.contains("digest mismatch"), "stderr: {stderr}");
    // Nothing was mutated by the materializer: the manifest still points
    // at the source overlay, no artifact was placed, no marker written.
    let now = fs::read_to_string(project.join("pyproject.toml")).unwrap();
    assert!(
        !now.contains("demo_alpha-2.4.0-py3-none-any.whl"),
        "manifest must not reference an unverifiable artifact: {now}"
    );
    let doc: toml::Table = now.parse().expect("parse manifest");
    assert_eq!(
        doc["tool"]["uv"]["sources"]["demo-alpha"]["path"]
            .as_str()
            .unwrap(),
        "deps/demo-alpha",
        "the source overlay reference must be untouched"
    );
    assert!(!project
        .join("deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl")
        .exists());
    assert!(!project.join("deps/.jumbo-artifacts.json").exists());
    let _ = fs::remove_dir_all(&root);
}

/// The flagship repeat-run property: on a fingerprint hit the second
/// build consumes the recorded artifacts with **zero source rebuilds** —
/// proven by a PATH shim that records any `uv`/`npm` invocation.
#[test]
fn repeat_run_performs_zero_source_rebuilds() {
    let root = temp_workspace("repeat");
    let repo = committed_repo(
        "repeat",
        &[("uv.lock", UV_LOCK), ("pyproject.toml", PYPROJECT)],
    );
    let fingerprint = computed_fingerprint(&repo);
    // The pipeline of the first build appended this record: same commit,
    // same fingerprint, published wheel.
    let index_dir = fixture_index(
        &root,
        Some((
            &fingerprint,
            Some(CONSUMER_WHEEL_URL),
            Some(CONSUMER_WHEEL_SHA),
        )),
    );
    let cache = root.join("cache");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::write(
        cache.join("consumer-0.1.0-py3-none-any.whl"),
        CONSUMER_WHEEL,
    )
    .expect("wheel fixture");
    fs::write(
        cache.join("demo_alpha-2.4.0-py3-none-any.whl"),
        DEMO_ALPHA_WHEEL,
    )
    .expect("dep wheel fixture");

    // The PATH shim: any uv/npm invocation is recorded as a source rebuild.
    let shim = root.join("shim");
    fs::create_dir_all(&shim).expect("shim dir");
    let log = root.join("tool-invocations.log");
    for tool in ["uv", "npm"] {
        let script = shim.join(tool);
        fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"{tool} $*\" >> {:?}\nexit 1\n",
                log.display().to_string()
            ),
        )
        .expect("write shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755));
        }
    }

    let run_dedup = || {
        Command::new(jumbo_bin())
            .args([
                "dedup",
                "--lock",
                "uv.lock",
                "--index",
                &index_dir.display().to_string(),
                "--materialize",
                "--deps",
            ])
            .current_dir(&repo)
            .env("JUMBO_ARTIFACT_DIR", &cache) // env parity with CI caches
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    shim.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("JUMBO_INDEX_PATH")
            .env_remove("JUMBO_INDEX_URL")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_TOKEN")
            .output()
            .expect("run jumbo dedup")
    };

    let out = run_dedup();
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["duplicate"], true);
    assert_eq!(json["action"], "reuse");
    assert_eq!(
        json["materialized"]["path"],
        "dist/consumer-0.1.0-py3-none-any.whl"
    );
    let deps = json["dependencies"]["materialized"].as_array().unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(
        deps[0]["path"],
        "deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl"
    );

    // Zero source rebuilds: no language tool was invoked at all.
    assert!(
        !log.exists(),
        "reuse must not rebuild from source; invocations: {}",
        fs::read_to_string(&log).unwrap_or_default()
    );

    // The second run consumes exactly the same bytes.
    let dist_before = fs::read(repo.join("dist/consumer-0.1.0-py3-none-any.whl")).unwrap();
    let dep_before =
        fs::read(repo.join("deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl")).unwrap();
    let out = run_dedup();
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(
        fs::read(repo.join("dist/consumer-0.1.0-py3-none-any.whl")).unwrap(),
        dist_before
    );
    assert_eq!(
        fs::read(repo.join("deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl")).unwrap(),
        dep_before
    );
    assert!(
        !log.exists(),
        "the repeated run must not rebuild from source either; invocations: {}",
        fs::read_to_string(&log).unwrap_or_default()
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn dedup_argument_validation() {
    let repo = committed_repo("args", &[("uv.lock", UV_LOCK)]);

    // --lock and --manifest cannot be combined.
    let out = run_jumbo(
        &["dedup", "--lock", "uv.lock", "--manifest", "pyproject.toml"],
        &repo,
    );
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("cannot be combined"));

    // Missing lock file.
    let out = run_jumbo(&["dedup", "--lock", "missing.lock"], &repo);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("not found"));

    // --deps requires a manifest (none exists in this repo).
    let out = run_jumbo(&["dedup", "--lock", "uv.lock", "--deps"], &repo);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("--deps requires a manifest"));

    // A lock whose root name cannot be derived needs --package.
    let anonymous = committed_repo(
        "anon",
        &[(
            "uv.lock",
            "version = 1\n\n[[package]]\nname = \"demo-alpha\"\nversion = \"2.4.0\"\nsource = { directory = \"deps/demo-alpha\" }\n",
        )],
    );
    let out = run_jumbo(&["dedup", "--lock", "uv.lock"], &anonymous);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("--package"),
        "stderr: {}",
        stderr_of(&out)
    );
    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&anonymous));
}
