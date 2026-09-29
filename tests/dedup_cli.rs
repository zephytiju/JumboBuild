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
        .env_remove("JUMBO_REPO_MAP")
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
    // demo-beta published no artifact: the fallback fetches its real tree
    // (repo-mapped) from the cache, keyed by the record's commit.
    write_tarball_fixture(
        &cache.join("0123456789abcdef0123456789abcdef01234567"),
        "demo-beta-01234567",
        &real_python_project("demo-beta", "1.4.2"),
    );
    let repo_map = root.join("repo-map.json");
    fs::write(
        &repo_map,
        "{\n  \"demo-beta\": \"https://github.com/acme/demo-beta\"\n}\n",
    )
    .expect("write repo map");
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
                "--repo-map",
                &repo_map.display().to_string(),
            ],
            &project,
        )
    };
    let out = run();
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    let deps = &json["dependencies"];
    let materialized = deps["materialized"].as_array().expect("materialized");
    assert_eq!(materialized.len(), 2, "{materialized:?}");
    assert_eq!(materialized[0]["package"], "demo-alpha");
    assert_eq!(
        materialized[0]["path"],
        "deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl"
    );
    assert_eq!(materialized[0]["sha256"], DEMO_ALPHA_WHEEL_SHA);
    // demo-beta published no artifact: its real source materialized.
    assert_eq!(materialized[1]["package"], "demo-beta");
    assert_eq!(materialized[1]["mode"], "source");
    assert_eq!(deps["keptSourceOverlays"], serde_json::json!(["demo-beta"]));

    // The wheel replaced the synthetic source-overlay project, at J3's
    // stable deps/<slug> coordinate; the manifest points uv at the wheel.
    let wheel = project.join("deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl");
    assert_eq!(fs::read(&wheel).expect("wheel"), DEMO_ALPHA_WHEEL);
    assert!(!project.join("deps/demo-alpha/pyproject.toml").exists());
    let beta_manifest =
        fs::read_to_string(project.join("deps/demo-beta/pyproject.toml")).expect("real source");
    assert!(
        beta_manifest.contains("name = \"demo-beta\""),
        "{beta_manifest}"
    );
    assert!(!beta_manifest.contains("jumbo-injected internal source"));
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

    // The materialization marker records both decisions.
    let marker: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project.join("deps/.jumbo-artifacts.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(marker["format"], "jumbo-artifact-materialization/1");
    assert_eq!(marker["artifacts"][0]["package"], "demo-alpha");
    assert_eq!(marker["artifacts"][1]["package"], "demo-beta");
    assert_eq!(marker["artifacts"][1]["mode"], "source");
    assert_eq!(
        marker["artifacts"][1]["url"],
        "https://codeload.github.com/acme/demo-beta/tar.gz/0123456789abcdef0123456789abcdef01234567"
    );

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

// ---------------------------------------------------------------------------
// Dependency source fallback (dead artifact URLs) — offline via a PATH
// shim over `curl`, the transport the validated github.com-only fetch
// layer drives. No network access, no credentials.
// ---------------------------------------------------------------------------

/// A PATH shim over `curl`: answers jumbo's artifact-download hops
/// (`--url`, `--dump-header`, `--output`) from fixture data. The four
/// `FAKE_CURL_OK_URL_<SLOT>` slots (A–D) answer 200 serving the bytes at
/// the matching `FAKE_CURL_OK_BODY_<SLOT>`; every other URL answers
/// `FAKE_CURL_STATUS` (404, 410, 500, ...). Never touches the network.
fn write_curl_shim(root: &Path) -> PathBuf {
    let shim_dir = root.join("curl-shim");
    fs::create_dir_all(&shim_dir).expect("shim dir");
    let script = shim_dir.join("curl");
    fs::write(
        &script,
        r#"#!/bin/sh
# jumbo offline test transport: answers jumbo's curl invocations from
# fixture data; never touches the network.
url=""
headers=""
output=""
while [ $# -gt 0 ]; do
  case "$1" in
    --url) url="$2"; shift 2 ;;
    --dump-header) headers="$2"; shift 2 ;;
    --output) output="$2"; shift 2 ;;
    *) shift ;;
  esac
done
respond () {
  status="$1"
  body_file="$2"
  printf 'HTTP/1.1 %s x\r\ncontent-length: 0\r\n\r\n' "$status" > "$headers"
  if [ -n "$body_file" ]; then
    cat "$body_file" > "$output"
  elif [ -n "$output" ]; then
    : > "$output"
  fi
  echo "$status"
  exit 0
}
for slot in A B C D; do
  eval "ok_url=\$FAKE_CURL_OK_URL_$slot"
  eval "ok_body=\$FAKE_CURL_OK_BODY_$slot"
  if [ -n "$ok_url" ] && [ "$url" = "$ok_url" ]; then
    respond 200 "$ok_body"
  fi
done
if [ -n "$FAKE_CURL_STATUS_URL" ] && [ "$url" = "$FAKE_CURL_STATUS_URL" ]; then
  respond "${FAKE_CURL_STATUS_CODE:-404}" ""
fi
respond "${FAKE_CURL_STATUS:-404}" ""
"#,
    )
    .expect("write curl shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755));
    }
    // The credential stub: no token unless the test opts in via
    // FAKE_GH_AUTH — a run under this transport must never see ambient
    // `gh auth` credentials, so the fetch layer's behavior is
    // deterministic with and without the private-release fallback.
    let gh = shim_dir.join("gh");
    fs::write(
        &gh,
        r#"#!/bin/sh
# jumbo offline test credential stub: deterministic, never ambient.
if [ -n "$FAKE_GH_AUTH" ]; then
  echo "jumbo-test-token"
  exit 0
fi
echo "gh: no auth in the offline test transport" >&2
exit 1
"#,
    )
    .expect("write gh stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755));
    }
    shim_dir
}

/// Run the jumbo binary with the shim directory prepended to PATH (the
/// injectable transport) plus extra environment variables.
fn run_jumbo_with_transport(
    args: &[&str],
    cwd: &Path,
    transport: &Path,
    envs: &[(&str, Option<&str>)],
) -> Output {
    let mut cmd = Command::new(jumbo_bin());
    cmd.args(args)
        .current_dir(cwd)
        .env_remove("JUMBO_INDEX_PATH")
        .env_remove("JUMBO_INDEX_URL")
        .env_remove("JUMBO_ARTIFACT_DIR")
        .env_remove("JUMBO_REPO_MAP")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .env(
            "PATH",
            format!(
                "{}:{}",
                transport.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    for (key, value) in envs {
        match value {
            Some(value) => {
                cmd.env(key, value);
            }
            None => {
                cmd.env_remove(key);
            }
        }
    }
    cmd.output().expect("run jumbo")
}

/// Build a gzipped tarball fixture shaped like a codeload GitHub tarball:
/// one leading `<root>/` directory wrapping the given files. The same
/// codecs the unpack path uses, so the fixture is representative.
fn write_tarball_fixture(path: &Path, root: &str, files: &[(String, String)]) {
    let file = fs::File::create(path).expect("create tarball fixture");
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(enc);
    for (name, content) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, format!("{root}/{name}"), content.as_bytes())
            .unwrap_or_else(|e| panic!("append {name}: {e}"));
    }
    builder
        .into_inner()
        .expect("finish tar")
        .finish()
        .expect("flush gz");
}

/// A real (buildable) python project fixture for a package, deliberately
/// at a version that differs from the index record's — the record's
/// version semantics hold, the tree's own version stands.
fn real_python_project(package: &str, version: &str) -> Vec<(String, String)> {
    let module: String = package.replace(['-', '_', '.'], "");
    vec![
        (
            "pyproject.toml".to_string(),
            format!(
                "[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n\n[project]\nname = \"{package}\"\nversion = \"{version}\"\n",
            ),
        ),
        (
            format!("src/{module}/__init__.py"),
            format!("\"\"\"{package} — real repository source.\"\"\"\n__version__ = \"{version}\"\n"),
        ),
    ]
}

const DEMO_GAMMA_WHEEL_URL: &str =
    "https://github.com/acme/demo-gamma/releases/download/v1.0.0/demo_gamma-1.0.0-py3-none-any.whl";
const DEMO_GAMMA_WHEEL: &[u8] = b"demo-gamma 1.0.0 wheel bytes (fixture artifact)\n";
const DEMO_GAMMA_WHEEL_SHA: &str =
    "f996b9da3f648c52f96fca584e95b5ee8bfc75f9da6e4cffef4705f9fb8e7984";
const DEMO_GAMMA_COMMIT: &str = "f00dcafe0123456789abcdef0123456789abcdef";
/// The codeload tarball URL of demo-gamma's real repository tree at the
/// recorded commit — the coordinate parsed from its (dead) artifactUrl.
const DEMO_GAMMA_CODELOAD_URL: &str =
    "https://codeload.github.com/acme/demo-gamma/tar.gz/f00dcafe0123456789abcdef0123456789abcdef";
const DEMO_DELTA_WHEEL_URL: &str =
    "https://github.com/acme/demo-delta/releases/download/v1.2.0/demo_delta-1.2.0-py3-none-any.whl";
const DEMO_DELTA_CODELOAD_URL: &str =
    "https://codeload.github.com/acme/demo-delta/tar.gz/0123456789abcdef0123456789abcdef01234567";

/// The consumer declares three internal dependencies: `demo-alpha` (live
/// artifact), `demo-gamma` (artifact URL the transport answers
/// 404/410 for), and `demo-delta` (artifact URL with a null sha256).
const PYPROJECT_FALLBACK: &str = "[project]\nname = \"consumer\"\ndependencies = [\n    \"demo-alpha@2\",\n    \"demo-gamma@1\",\n    \"demo-delta@1\",\n    \"numpy>=1.26\",\n]\n";

const UV_LOCK_FALLBACK: &str = r#"version = 1
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
name = "demo-gamma"
version = "1.0.0"
source = { directory = "deps/demo-gamma" }

[[package]]
name = "demo-delta"
version = "1.2.0"
source = { directory = "deps/demo-delta" }

[[package]]
name = "numpy"
version = "1.26.4"
source = { registry = "https://pypi.org/simple" }
sdist = { url = "https://files.pythonhosted.org/numpy-1.26.4.tar.gz", hash = "sha256:aaaa1111aaaa", size = 1 }
"#;

/// An index where `demo-gamma`'s record carries a dead artifact URL (the
/// transport answers 404/410) and `demo-delta`'s record has a URL but no
/// `artifactSha256`.
fn fallback_index(root: &Path) -> PathBuf {
    let index_dir = root.join("index");
    fs::create_dir_all(&index_dir).expect("index dir");
    let write = |slug: &str, line: String| {
        fs::write(index_dir.join(format!("{slug}.jsonl")), line + "\n").expect("write jsonl");
    };
    write(
        "demo-alpha",
        record_json(
            "demo-alpha",
            "2.4.0",
            Some(&"e".repeat(64)),
            Some(DEMO_ALPHA_WHEEL_URL),
            Some(DEMO_ALPHA_WHEEL_SHA),
        ),
    );
    write(
        "demo-gamma",
        serde_json::json!({
            "package": "demo-gamma",
            "major": 1,
            "version": "1.0.0",
            "commit": DEMO_GAMMA_COMMIT,
            "fingerprint": Some("e".repeat(64)),
            "canonicalExtract": null,
            "artifactUrl": DEMO_GAMMA_WHEEL_URL,
            "artifactSha256": DEMO_GAMMA_WHEEL_SHA,
            "imageDigest": null,
            "buildId": "demo-gamma-1.0.0-001",
            "pipelineRun": Some("circleci/run-52"),
            "executor": "circleci",
            "timestamp": "2026-09-20T00:00:00Z",
        })
        .to_string(),
    );
    write(
        "demo-delta",
        serde_json::json!({
            "package": "demo-delta",
            "major": 1,
            "version": "1.2.0",
            "commit": "0123456789abcdef0123456789abcdef01234567",
            "fingerprint": Some("e".repeat(64)),
            "canonicalExtract": null,
            "artifactUrl": DEMO_DELTA_WHEEL_URL,
            "artifactSha256": null,
            "imageDigest": null,
            "buildId": "demo-delta-1.2.0-001",
            "pipelineRun": Some("circleci/run-52"),
            "executor": "circleci",
            "timestamp": "2026-09-20T00:00:00Z",
        })
        .to_string(),
    );
    index_dir
}

/// A committed fixture project declaring all three fallback deps.
fn fallback_project(tag: &str) -> PathBuf {
    committed_repo(
        tag,
        &[
            ("pyproject.toml", PYPROJECT_FALLBACK),
            ("uv.lock", UV_LOCK_FALLBACK),
        ],
    )
}

#[test]
fn dead_dependency_artifacts_fall_back_to_real_source() {
    for gone_status in ["404", "410"] {
        let root = temp_workspace("fallback");
        let index_dir = fallback_index(&root);
        let alpha_body = root.join("alpha.whl");
        fs::write(&alpha_body, DEMO_ALPHA_WHEEL).expect("alpha bytes");
        let alpha_body = alpha_body.display().to_string();
        // Codeload tarball fixtures: demo-gamma's real tree at the recorded
        // commit (version 1.0.1 — the record's version semantics hold, the
        // tree's own version stands) and demo-delta's (version 2.0.0).
        let gamma_tarball = root.join("demo-gamma-tree.tar.gz");
        write_tarball_fixture(
            &gamma_tarball,
            "demo-gamma-f00dcafe",
            &real_python_project("demo-gamma", "1.0.1"),
        );
        let delta_tarball = root.join("demo-delta-tree.tar.gz");
        write_tarball_fixture(
            &delta_tarball,
            "demo-delta-01234567",
            &real_python_project("demo-delta", "2.0.0"),
        );
        let (gamma_tarball, delta_tarball) = (
            gamma_tarball.display().to_string(),
            delta_tarball.display().to_string(),
        );
        let shim = write_curl_shim(&root);
        let project = fallback_project("fallback");
        let index_arg = index_dir.display().to_string();

        let out = run_jumbo_with_transport(
            &[
                "dedup",
                "--manifest",
                "pyproject.toml",
                "--index",
                &index_arg,
                "--deps",
            ],
            &project,
            &shim,
            &[
                ("FAKE_CURL_OK_URL_A", Some(DEMO_ALPHA_WHEEL_URL)),
                ("FAKE_CURL_OK_BODY_A", Some(alpha_body.as_str())),
                ("FAKE_CURL_OK_URL_B", Some(DEMO_GAMMA_CODELOAD_URL)),
                ("FAKE_CURL_OK_BODY_B", Some(gamma_tarball.as_str())),
                ("FAKE_CURL_OK_URL_C", Some(DEMO_DELTA_CODELOAD_URL)),
                ("FAKE_CURL_OK_BODY_C", Some(delta_tarball.as_str())),
                ("FAKE_CURL_STATUS", Some(gone_status)),
            ],
        );
        assert!(
            out.status.success(),
            "HTTP {gone_status} must fall back, stderr: {}",
            stderr_of(&out)
        );
        let json = stdout_json(&out);
        let deps = &json["dependencies"];
        let materialized = deps["materialized"].as_array().expect("materialized");
        assert_eq!(materialized.len(), 3, "{materialized:?}");

        let by_package = |name: &str| {
            materialized
                .iter()
                .find(|m| m["package"] == name)
                .unwrap_or_else(|| panic!("no entry for {name}: {materialized:?}"))
                .clone()
        };
        // The live artifact materialized as an artifact.
        let alpha = by_package("demo-alpha");
        assert_eq!(alpha["mode"], "artifact");
        assert_eq!(alpha["url"], DEMO_ALPHA_WHEEL_URL);
        assert_eq!(alpha["sha256"], DEMO_ALPHA_WHEEL_SHA);
        assert_eq!(
            alpha["path"],
            "deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl"
        );
        // The dead artifact fell back to the real repository source at the
        // recorded commit, with the reason and the tarball provenance URL.
        let gamma = by_package("demo-gamma");
        assert_eq!(gamma["mode"], "source");
        assert_eq!(gamma["url"], DEMO_GAMMA_CODELOAD_URL);
        assert_eq!(gamma["sha256"], serde_json::Value::Null);
        assert_eq!(gamma["path"], "deps/demo-gamma");
        assert_eq!(gamma["commit"], DEMO_GAMMA_COMMIT);
        let reason = gamma["reason"].as_str().expect("reason");
        assert!(reason.contains(&format!("HTTP {gone_status}")), "{reason}");
        assert!(reason.contains(DEMO_GAMMA_WHEEL_URL), "{reason}");
        assert!(reason.contains("real repository source"), "{reason}");
        // The digest-less record fell back too.
        let delta = by_package("demo-delta");
        assert_eq!(delta["mode"], "source");
        assert_eq!(delta["url"], DEMO_DELTA_CODELOAD_URL);
        assert!(delta["reason"].as_str().unwrap().contains("artifactSha256"));
        // Both fallback deps report standing source materializations.
        assert_eq!(
            deps["keptSourceOverlays"],
            serde_json::json!(["demo-delta", "demo-gamma"])
        );

        // On disk: the artifact replaced alpha's overlay; gamma and delta
        // hold the REAL repository trees — the minimal lock stubs (which
        // are never buildable) were replaced.
        assert!(project
            .join("deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl")
            .is_file());
        assert!(!project.join("deps/demo-alpha/pyproject.toml").exists());
        for (name, version, module) in [
            ("demo-gamma", "1.0.1", "demogamma"),
            ("demo-delta", "2.0.0", "demodelta"),
        ] {
            let overlay = fs::read_to_string(project.join(format!("deps/{name}/pyproject.toml")))
                .expect("real source manifest");
            assert!(overlay.contains(&format!("name = \"{name}\"")), "{overlay}");
            assert!(
                overlay.contains(&format!("version = \"{version}\"")),
                "the tree's own version stands: {overlay}"
            );
            assert!(
                overlay.contains("hatchling.build"),
                "a real build-backend, not the minimal stub: {overlay}"
            );
            assert!(
                !overlay.contains("jumbo-injected internal source"),
                "the minimal stub must be gone: {overlay}"
            );
            assert!(
                project
                    .join(format!("deps/{name}/src/{module}/__init__.py"))
                    .is_file(),
                "the real tree's files are unpacked"
            );
        }

        // The manifest: alpha points at the wheel; the fallbacks keep the
        // lock injection's overlay-directory references — now backed by
        // real, buildable projects.
        let manifest = fs::read_to_string(project.join("pyproject.toml")).unwrap();
        let doc: toml::Table = manifest.parse().expect("parse manifest");
        assert_eq!(
            doc["tool"]["uv"]["sources"]["demo-alpha"]["path"]
                .as_str()
                .unwrap(),
            "deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl"
        );
        assert_eq!(
            doc["tool"]["uv"]["sources"]["demo-gamma"]["path"]
                .as_str()
                .unwrap(),
            "deps/demo-gamma"
        );
        assert_eq!(
            doc["tool"]["uv"]["sources"]["demo-delta"]["path"]
                .as_str()
                .unwrap(),
            "deps/demo-delta"
        );

        // The marker records every decision with its mode and provenance.
        let marker: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(project.join("deps/.jumbo-artifacts.json")).unwrap(),
        )
        .unwrap();
        let gamma_entry = marker["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["package"] == "demo-gamma")
            .expect("gamma marker entry");
        assert_eq!(gamma_entry["mode"], "source");
        assert_eq!(gamma_entry["url"], DEMO_GAMMA_CODELOAD_URL);
        assert_eq!(gamma_entry["sha256"], serde_json::Value::Null);

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&project);
    }
}

#[test]
fn artifact_to_source_transition_removes_the_stale_artifact() {
    let root = temp_workspace("transition");
    let index_dir = fallback_index(&root);
    let alpha_body = root.join("alpha.whl");
    fs::write(&alpha_body, DEMO_ALPHA_WHEEL).expect("alpha bytes");
    let alpha_body = alpha_body.display().to_string();
    let gamma_body_path = root.join("gamma.whl");
    fs::write(&gamma_body_path, DEMO_GAMMA_WHEEL).expect("gamma bytes");
    let gamma_body = gamma_body_path.display().to_string();
    let gamma_tarball = root.join("demo-gamma-tree.tar.gz");
    write_tarball_fixture(
        &gamma_tarball,
        "demo-gamma-f00dcafe",
        &real_python_project("demo-gamma", "1.0.1"),
    );
    let gamma_tarball = gamma_tarball.display().to_string();
    let delta_tarball = root.join("demo-delta-tree.tar.gz");
    write_tarball_fixture(
        &delta_tarball,
        "demo-delta-01234567",
        &real_python_project("demo-delta", "2.0.0"),
    );
    let delta_tarball = delta_tarball.display().to_string();
    let shim = write_curl_shim(&root);
    let project = fallback_project("transition");
    let index_arg = index_dir.display().to_string();
    let run = |gamma_served: bool| {
        let mut envs: Vec<(&str, Option<&str>)> = vec![
            ("FAKE_CURL_OK_URL_A", Some(DEMO_ALPHA_WHEEL_URL)),
            ("FAKE_CURL_OK_BODY_A", Some(alpha_body.as_str())),
            ("FAKE_CURL_OK_URL_C", Some(DEMO_GAMMA_CODELOAD_URL)),
            ("FAKE_CURL_OK_BODY_C", Some(gamma_tarball.as_str())),
            ("FAKE_CURL_OK_URL_D", Some(DEMO_DELTA_CODELOAD_URL)),
            ("FAKE_CURL_OK_BODY_D", Some(delta_tarball.as_str())),
            ("FAKE_CURL_STATUS", Some("404")),
        ];
        if gamma_served {
            envs.push(("FAKE_CURL_OK_URL_B", Some(DEMO_GAMMA_WHEEL_URL)));
            envs.push(("FAKE_CURL_OK_BODY_B", Some(gamma_body.as_str())));
        } else {
            envs.push(("FAKE_CURL_OK_URL_B", None));
        }
        run_jumbo_with_transport(
            &[
                "dedup",
                "--manifest",
                "pyproject.toml",
                "--index",
                &index_arg,
                "--deps",
            ],
            &project,
            &shim,
            &envs,
        )
    };

    // Run 1: both artifacts live — gamma materializes as an artifact.
    let out = run(true);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let gamma_wheel = project.join("deps/demo-gamma/demo_gamma-1.0.0-py3-none-any.whl");
    assert!(gamma_wheel.is_file());
    assert!(!project.join("deps/demo-gamma/pyproject.toml").exists());

    // Run 2: gamma's release asset is gone — the build proceeds on the
    // real repository source at the recorded commit, and the stale
    // artifact must not linger.
    let out = run(false);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    let gamma = json["dependencies"]["materialized"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["package"] == "demo-gamma")
        .unwrap()
        .clone();
    assert_eq!(gamma["mode"], "source");
    assert!(gamma["reason"].as_str().unwrap().contains("HTTP 404"));
    assert_eq!(gamma["url"], DEMO_GAMMA_CODELOAD_URL);
    assert_eq!(
        json["dependencies"]["keptSourceOverlays"],
        serde_json::json!(["demo-delta", "demo-gamma"])
    );
    assert!(!gamma_wheel.exists(), "the stale artifact must be removed");
    let overlay =
        fs::read_to_string(project.join("deps/demo-gamma/pyproject.toml")).expect("overlay");
    assert!(overlay.contains("name = \"demo-gamma\""), "{overlay}");
    assert!(
        !overlay.contains("jumbo-injected internal source"),
        "{overlay}"
    );
    assert!(project
        .join("deps/demo-gamma/src/demogamma/__init__.py")
        .is_file());
    let doc: toml::Table = fs::read_to_string(project.join("pyproject.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        doc["tool"]["uv"]["sources"]["demo-gamma"]["path"]
            .as_str()
            .unwrap(),
        "deps/demo-gamma"
    );
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&project);
}

/// Re-runs are idempotent: once the real source materialization stands at
/// the recorded commit with the recorded provenance, the fetch is skipped
/// — proven by a second run whose transport would answer any codeload
/// request with 404 (which would abort).
#[test]
fn standing_source_materialization_skips_the_refetch() {
    let root = temp_workspace("idem");
    let index_dir = fallback_index(&root);
    let gamma_tarball = root.join("demo-gamma-tree.tar.gz");
    write_tarball_fixture(
        &gamma_tarball,
        "demo-gamma-f00dcafe",
        &real_python_project("demo-gamma", "1.0.1"),
    );
    let gamma_tarball = gamma_tarball.display().to_string();
    let delta_tarball = root.join("demo-delta-tree.tar.gz");
    write_tarball_fixture(
        &delta_tarball,
        "demo-delta-01234567",
        &real_python_project("demo-delta", "2.0.0"),
    );
    let delta_tarball = delta_tarball.display().to_string();
    let shim = write_curl_shim(&root);
    let project = fallback_project("idem");
    let index_arg = index_dir.display().to_string();

    // Run 1: alpha's artifact and both codeload tarballs are served;
    // gamma and delta fall back and their real trees materialize.
    let alpha_body = root.join("alpha.whl");
    fs::write(&alpha_body, DEMO_ALPHA_WHEEL).expect("alpha bytes");
    let alpha_body = alpha_body.display().to_string();
    let out = run_jumbo_with_transport(
        &[
            "dedup",
            "--manifest",
            "pyproject.toml",
            "--index",
            &index_arg,
            "--deps",
        ],
        &project,
        &shim,
        &[
            ("FAKE_CURL_OK_URL_A", Some(DEMO_ALPHA_WHEEL_URL)),
            ("FAKE_CURL_OK_BODY_A", Some(alpha_body.as_str())),
            ("FAKE_CURL_OK_URL_B", Some(DEMO_GAMMA_CODELOAD_URL)),
            ("FAKE_CURL_OK_BODY_B", Some(gamma_tarball.as_str())),
            ("FAKE_CURL_OK_URL_C", Some(DEMO_DELTA_CODELOAD_URL)),
            ("FAKE_CURL_OK_BODY_C", Some(delta_tarball.as_str())),
            ("FAKE_CURL_STATUS", Some("404")),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let manifest_once = fs::read_to_string(project.join("pyproject.toml")).unwrap();
    let gamma_once = fs::read_to_string(project.join("deps/demo-gamma/pyproject.toml")).unwrap();
    let marker_once = fs::read_to_string(project.join("deps/.jumbo-artifacts.json")).unwrap();

    // Run 2: no OK slot serves a codeload URL anymore — any codeload fetch
    // would answer 404 and abort. Success proves the fetch was skipped.
    let out = run_jumbo_with_transport(
        &[
            "dedup",
            "--manifest",
            "pyproject.toml",
            "--index",
            &index_arg,
            "--deps",
        ],
        &project,
        &shim,
        &[
            ("FAKE_CURL_OK_URL_A", Some(DEMO_ALPHA_WHEEL_URL)),
            ("FAKE_CURL_OK_BODY_A", Some(alpha_body.as_str())),
            ("FAKE_CURL_OK_URL_B", None),
            ("FAKE_CURL_OK_URL_C", None),
            ("FAKE_CURL_STATUS", Some("404")),
        ],
    );
    assert!(
        out.status.success(),
        "a standing source materialization must skip the refetch, stderr: {}",
        stderr_of(&out)
    );
    assert_eq!(
        fs::read_to_string(project.join("pyproject.toml")).unwrap(),
        manifest_once,
        "second materialization must be byte-identical"
    );
    assert_eq!(
        fs::read_to_string(project.join("deps/demo-gamma/pyproject.toml")).unwrap(),
        gamma_once,
        "the standing real source must be untouched"
    );
    assert_eq!(
        fs::read_to_string(project.join("deps/.jumbo-artifacts.json")).unwrap(),
        marker_once,
        "the marker must be stable"
    );
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&project);
}

#[test]
fn server_errors_do_not_fall_back() {
    let root = temp_workspace("fivexx");
    let index_dir = fallback_index(&root);
    let shim = write_curl_shim(&root);
    let project = fallback_project("fivexx");
    let index_arg = index_dir.display().to_string();

    let out = run_jumbo_with_transport(
        &[
            "dedup",
            "--manifest",
            "pyproject.toml",
            "--index",
            &index_arg,
            "--deps",
        ],
        &project,
        &shim,
        &[
            ("FAKE_CURL_OK_URL_A", None),
            ("FAKE_CURL_OK_URL_B", None),
            ("FAKE_CURL_STATUS", Some("500")),
        ],
    );
    assert!(
        !out.status.success(),
        "a 5xx is a real outage and must abort, not fall back"
    );
    let stderr = stderr_of(&out);
    assert!(stderr.contains("HTTP 500"), "stderr: {stderr}");
    // Nothing was mutated by the materializer: the manifest keeps the
    // lock injection's source-overlay reference, no artifact was placed,
    // no marker was written.
    let manifest = fs::read_to_string(project.join("pyproject.toml")).unwrap();
    assert!(
        !manifest.contains("demo_alpha-2.4.0-py3-none-any.whl"),
        "manifest must not reference an artifact that could not be fetched: {manifest}"
    );
    let doc: toml::Table = manifest.parse().expect("parse manifest");
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
    let _ = fs::remove_dir_all(&project);
}

/// A 5xx on the codeload tarball fetch aborts too: the fallback fetch is
/// part of the build, and leaving the unbuildable stub standing would
/// just move the failure into uv/npm.
#[test]
fn codeload_server_errors_abort_the_fallback() {
    let root = temp_workspace("codeload5xx");
    let index_dir = fallback_index(&root);
    let shim = write_curl_shim(&root);
    let project = fallback_project("codeload5xx");
    let index_arg = index_dir.display().to_string();
    let alpha_body = root.join("alpha.whl");
    fs::write(&alpha_body, DEMO_ALPHA_WHEEL).expect("alpha bytes");
    let alpha_body = alpha_body.display().to_string();

    // Alpha's artifact is served; gamma's dead asset answers a definitive
    // 404 (falling back), and the codeload tarball fetch then answers 500.
    let out = run_jumbo_with_transport(
        &[
            "dedup",
            "--manifest",
            "pyproject.toml",
            "--index",
            &index_arg,
            "--deps",
        ],
        &project,
        &shim,
        &[
            ("FAKE_CURL_OK_URL_A", Some(DEMO_ALPHA_WHEEL_URL)),
            ("FAKE_CURL_OK_BODY_A", Some(alpha_body.as_str())),
            ("FAKE_CURL_STATUS_URL", Some(DEMO_GAMMA_WHEEL_URL)),
            ("FAKE_CURL_STATUS_CODE", Some("404")),
            ("FAKE_CURL_STATUS", Some("500")),
        ],
    );
    assert!(
        !out.status.success(),
        "a 5xx on the source fallback fetch must abort"
    );
    let stderr = stderr_of(&out);
    assert!(stderr.contains("HTTP 500"), "stderr: {stderr}");
    assert!(stderr.contains("real source"), "stderr: {stderr}");
    // The stub was not replaced and no marker was written.
    let overlay = fs::read_to_string(project.join("deps/demo-gamma/pyproject.toml")).expect("stub");
    assert!(
        overlay.contains("jumbo-injected internal source"),
        "{overlay}"
    );
    assert!(!project.join("deps/.jumbo-artifacts.json").exists());
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&project);
}

// ---------------------------------------------------------------------------
// Private-repository release assets — the diagnosed jumbo-publish defect:
// a PRIVATE member's recorded github.com release-download URL answers 404
// even WITH an Authorization header (the public control 302s), so the
// fetch layer resolves the asset through the authenticated api.github.com
// asset route. Offline via the same curl shim: the tags lookup and the
// asset endpoint are fixture-mapped URLs; the credential comes from the
// shim's `gh` stub (FAKE_GH_AUTH), never from the ambient environment.
// ---------------------------------------------------------------------------

/// The api.github.com URLs the fetch layer builds from the recorded
/// demo-gamma release-download URL (owner/repo/tag parsed from it).
const DEMO_GAMMA_TAGS_URL: &str =
    "https://api.github.com/repos/acme/demo-gamma/releases/tags/v1.0.0";
const DEMO_GAMMA_ASSET_API_URL: &str =
    "https://api.github.com/repos/acme/demo-gamma/releases/assets/424242";

/// The release JSON the api.github.com tags lookup answers for the
/// private demo-gamma release: the wheel, by exact name, at asset id
/// 424242.
fn private_release_json(with_wheel: bool) -> String {
    let mut assets = vec![serde_json::json!({ "id": 4241, "name": "SHA256SUMS" })];
    if with_wheel {
        assets.push(serde_json::json!({
            "id": 424242,
            "name": "demo_gamma-1.0.0-py3-none-any.whl"
        }));
    }
    serde_json::json!({ "id": 1001, "tag_name": "v1.0.0", "assets": assets }).to_string()
}

/// A committed single-dependency consumer (demo-gamma@1 only) plus an
/// index carrying demo-gamma's record: a github.com release-download URL
/// (dead — this is a PRIVATE repository's re-pull), the recorded commit,
/// and the recorded sha256 of the fixture wheel.
fn private_asset_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = temp_workspace(tag);
    let index_dir = root.join("index");
    fs::create_dir_all(&index_dir).expect("index dir");
    fs::write(
        index_dir.join("demo-gamma.jsonl"),
        serde_json::json!({
            "package": "demo-gamma",
            "major": 1,
            "version": "1.0.0",
            "commit": DEMO_GAMMA_COMMIT,
            "fingerprint": Some("e".repeat(64)),
            "canonicalExtract": null,
            "artifactUrl": DEMO_GAMMA_WHEEL_URL,
            "artifactSha256": DEMO_GAMMA_WHEEL_SHA,
            "imageDigest": null,
            "buildId": "demo-gamma-1.0.0-001",
            "pipelineRun": Some("circleci/run-52"),
            "executor": "circleci",
            "timestamp": "2026-09-20T00:00:00Z",
        })
        .to_string()
            + "\n",
    )
    .expect("write record");
    let project = committed_repo(
        tag,
        &[
            (
                "pyproject.toml",
                "[project]\nname = \"consumer\"\ndependencies = [\"demo-gamma@1\"]\n",
            ),
            (
                "uv.lock",
                "version = 1\n\n[[package]]\nname = \"consumer\"\nversion = \"0.1.0\"\nsource = { editable = \".\" }\n\n[[package]]\nname = \"demo-gamma\"\nversion = \"1.0.0\"\nsource = { directory = \"deps/demo-gamma\" }\n",
            ),
        ],
    );
    (root, project)
}

/// A PRIVATE release asset resolves through the api.github.com route: the
/// recorded URL answers 404 (what GitHub answers for private assets even
/// with a token), the tags lookup returns the release, the asset is
/// matched by exact name and downloaded by id — and the bytes are
/// sha256-verified against the record exactly as a public pull's are.
#[test]
fn private_release_assets_resolve_via_the_api_route() {
    let (root, project) = private_asset_fixture("private-ok");
    let wheel_body = root.join("gamma.whl");
    fs::write(&wheel_body, DEMO_GAMMA_WHEEL).expect("wheel bytes");
    let wheel_body = wheel_body.display().to_string();
    let release_json = root.join("release.json");
    fs::write(&release_json, private_release_json(true)).expect("release json");
    let (release_json, shim) = (release_json.display().to_string(), write_curl_shim(&root));

    let out = run_jumbo_with_transport(
        &[
            "dedup",
            "--manifest",
            "pyproject.toml",
            "--index",
            &root.join("index").display().to_string(),
            "--deps",
        ],
        &project,
        &shim,
        &[
            ("FAKE_GH_AUTH", Some("1")),
            // The recorded URL 404s — the private-repository answer.
            ("FAKE_CURL_STATUS_URL", Some(DEMO_GAMMA_WHEEL_URL)),
            ("FAKE_CURL_STATUS_CODE", Some("404")),
            // The API route: release lookup 200 + the asset by id 200.
            ("FAKE_CURL_OK_URL_A", Some(DEMO_GAMMA_TAGS_URL)),
            ("FAKE_CURL_OK_BODY_A", Some(release_json.as_str())),
            ("FAKE_CURL_OK_URL_B", Some(DEMO_GAMMA_ASSET_API_URL)),
            ("FAKE_CURL_OK_BODY_B", Some(wheel_body.as_str())),
        ],
    );
    assert!(
        out.status.success(),
        "the private asset must resolve via the api.github.com route, stderr: {}",
        stderr_of(&out)
    );
    let materialized = &stdout_json(&out)["dependencies"]["materialized"];
    let gamma = materialized
        .as_array()
        .expect("materialized")
        .iter()
        .find(|m| m["package"] == "demo-gamma")
        .expect("gamma entry")
        .clone();
    // An artifact-mode materialization: the recorded URL stands (the
    // index record's provenance), the bytes verified against the
    // recorded digest.
    assert_eq!(gamma["mode"], "artifact", "{gamma}");
    assert_eq!(gamma["url"], DEMO_GAMMA_WHEEL_URL, "{gamma}");
    assert_eq!(gamma["sha256"], DEMO_GAMMA_WHEEL_SHA, "{gamma}");
    assert_eq!(
        gamma["path"], "deps/demo-gamma/demo_gamma-1.0.0-py3-none-any.whl",
        "{gamma}"
    );
    assert!(project
        .join("deps/demo-gamma/demo_gamma-1.0.0-py3-none-any.whl")
        .is_file());
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&project);
}

/// When the api.github.com route confirms the asset is gone (404 from the
/// release lookup), the definitive-absence behavior stands: the
/// dependency falls back to the real repository source at the recorded
/// commit, exactly as a token-less 404 always has.
#[test]
fn api_confirmed_gone_still_falls_back_to_real_source() {
    let (root, project) = private_asset_fixture("private-gone");
    let gamma_tarball = root.join("demo-gamma-tree.tar.gz");
    write_tarball_fixture(
        &gamma_tarball,
        "demo-gamma-f00dcafe",
        &real_python_project("demo-gamma", "1.0.1"),
    );
    let gamma_tarball = gamma_tarball.display().to_string();
    let shim = write_curl_shim(&root);

    let out = run_jumbo_with_transport(
        &[
            "dedup",
            "--manifest",
            "pyproject.toml",
            "--index",
            &root.join("index").display().to_string(),
            "--deps",
        ],
        &project,
        &shim,
        &[
            ("FAKE_GH_AUTH", Some("1")),
            ("FAKE_CURL_STATUS_URL", Some(DEMO_GAMMA_WHEEL_URL)),
            ("FAKE_CURL_STATUS_CODE", Some("404")),
            // The tags lookup answers 404: release (or visibility) gone.
            // Every unmapped URL — the tags URL included — 404s.
            ("FAKE_CURL_STATUS", Some("404")),
            ("FAKE_CURL_OK_URL_A", Some(DEMO_GAMMA_CODELOAD_URL)),
            ("FAKE_CURL_OK_BODY_A", Some(gamma_tarball.as_str())),
        ],
    );
    assert!(
        out.status.success(),
        "an api-confirmed absence must still fall back to source, stderr: {}",
        stderr_of(&out)
    );
    let materialized = &stdout_json(&out)["dependencies"]["materialized"];
    let gamma = materialized
        .as_array()
        .expect("materialized")
        .iter()
        .find(|m| m["package"] == "demo-gamma")
        .expect("gamma entry")
        .clone();
    assert_eq!(gamma["mode"], "source", "{gamma}");
    assert_eq!(gamma["url"], DEMO_GAMMA_CODELOAD_URL, "{gamma}");
    let reason = gamma["reason"].as_str().expect("reason");
    assert!(reason.contains("HTTP 404"), "{reason}");
    assert!(reason.contains(DEMO_GAMMA_WHEEL_URL), "{reason}");
    assert!(reason.contains("real repository source"), "{reason}");
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&project);
}

/// An api.github.com outage (5xx on the release lookup) is a transient
/// failure, not an absence: the run aborts instead of silently
/// degrading to the source fallback, and nothing is mutated.
#[test]
fn api_outage_on_the_private_route_aborts() {
    let (root, project) = private_asset_fixture("private-5xx");
    let shim = write_curl_shim(&root);

    let out = run_jumbo_with_transport(
        &[
            "dedup",
            "--manifest",
            "pyproject.toml",
            "--index",
            &root.join("index").display().to_string(),
            "--deps",
        ],
        &project,
        &shim,
        &[
            ("FAKE_GH_AUTH", Some("1")),
            // The recorded URL 404s; the tags lookup then answers 500.
            ("FAKE_CURL_STATUS_URL", Some(DEMO_GAMMA_WHEEL_URL)),
            ("FAKE_CURL_STATUS_CODE", Some("404")),
            ("FAKE_CURL_STATUS", Some("500")),
        ],
    );
    assert!(
        !out.status.success(),
        "an api outage must abort, not fall back"
    );
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("the private-release asset lookup via api.github.com answered HTTP 500"),
        "stderr: {stderr}"
    );
    // The stub was not replaced and no marker was written.
    let overlay = fs::read_to_string(project.join("deps/demo-gamma/pyproject.toml")).expect("stub");
    assert!(
        overlay.contains("jumbo-injected internal source"),
        "{overlay}"
    );
    assert!(!project.join("deps/.jumbo-artifacts.json").exists());
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&project);
}

/// A fetched tree whose manifest name does not match the record aborts
/// the fallback: the coordinate or the repository is wrong.
#[test]
fn source_name_mismatch_aborts_the_fallback() {
    let root = temp_workspace("namemismatch");
    let index_dir = fallback_index(&root);
    let alpha_body = root.join("alpha.whl");
    fs::write(&alpha_body, DEMO_ALPHA_WHEEL).expect("alpha bytes");
    let alpha_body = alpha_body.display().to_string();
    let wrong_tree = root.join("wrong-tree.tar.gz");
    write_tarball_fixture(
        &wrong_tree,
        "demo-gamma-f00dcafe",
        &real_python_project("other-package", "1.0.0"),
    );
    let wrong_tree = wrong_tree.display().to_string();
    let delta_tarball = root.join("demo-delta-tree.tar.gz");
    write_tarball_fixture(
        &delta_tarball,
        "demo-delta-01234567",
        &real_python_project("demo-delta", "2.0.0"),
    );
    let delta_tarball = delta_tarball.display().to_string();
    let shim = write_curl_shim(&root);
    let project = fallback_project("namemismatch");
    let index_arg = index_dir.display().to_string();

    let out = run_jumbo_with_transport(
        &[
            "dedup",
            "--manifest",
            "pyproject.toml",
            "--index",
            &index_arg,
            "--deps",
        ],
        &project,
        &shim,
        &[
            ("FAKE_CURL_OK_URL_A", Some(DEMO_ALPHA_WHEEL_URL)),
            ("FAKE_CURL_OK_BODY_A", Some(alpha_body.as_str())),
            ("FAKE_CURL_OK_URL_B", Some(DEMO_GAMMA_CODELOAD_URL)),
            ("FAKE_CURL_OK_BODY_B", Some(wrong_tree.as_str())),
            ("FAKE_CURL_OK_URL_C", Some(DEMO_DELTA_CODELOAD_URL)),
            ("FAKE_CURL_OK_BODY_C", Some(delta_tarball.as_str())),
            ("FAKE_CURL_STATUS", Some("404")),
        ],
    );
    assert!(
        !out.status.success(),
        "a name mismatch must abort, not materialize a wrong tree"
    );
    let stderr = stderr_of(&out);
    assert!(stderr.contains("does not match"), "stderr: {stderr}");
    assert!(stderr.contains("other-package"), "stderr: {stderr}");
    assert!(stderr.contains("demo-gamma"), "stderr: {stderr}");
    // The stub was not replaced and no marker was written.
    let overlay = fs::read_to_string(project.join("deps/demo-gamma/pyproject.toml")).expect("stub");
    assert!(
        overlay.contains("jumbo-injected internal source"),
        "{overlay}"
    );
    assert!(!project.join("deps/.jumbo-artifacts.json").exists());
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&project);
}

/// A dependency whose repository cannot be resolved (no github.com
/// artifactUrl, no repo map) aborts with a typed error naming the package
/// and both resolution options.
#[test]
fn unresolvable_repository_is_a_typed_error() {
    let root = temp_workspace("unresolvable");
    let index_dir = fixture_index(&root, None); // demo-beta: no artifactUrl
    let alpha_body = root.join("alpha.whl");
    fs::write(&alpha_body, DEMO_ALPHA_WHEEL).expect("alpha bytes");
    let alpha_body = alpha_body.display().to_string();
    let shim = write_curl_shim(&root);
    let project = committed_repo(
        "unresolvable",
        &[
            ("pyproject.toml", PYPROJECT_WITH_BETA),
            ("uv.lock", UV_LOCK_WITH_BETA),
        ],
    );

    let out = run_jumbo_with_transport(
        &[
            "dedup",
            "--manifest",
            "pyproject.toml",
            "--index",
            &index_dir.display().to_string(),
            "--deps",
        ],
        &project,
        &shim,
        &[
            ("FAKE_CURL_OK_URL_A", Some(DEMO_ALPHA_WHEEL_URL)),
            ("FAKE_CURL_OK_BODY_A", Some(alpha_body.as_str())),
            ("FAKE_CURL_STATUS", Some("404")),
        ],
    );
    assert!(
        !out.status.success(),
        "an unresolvable repository must abort, not keep an unbuildable stub"
    );
    let stderr = stderr_of(&out);
    assert!(stderr.contains("cannot resolve the repository"), "{stderr}");
    assert!(stderr.contains("demo-beta"), "{stderr}");
    assert!(stderr.contains("--repo-map"), "{stderr}");
    assert!(stderr.contains("JUMBO_REPO_MAP"), "{stderr}");
    assert!(stderr.contains("artifactUrl"), "{stderr}");
    // Nothing was mutated.
    assert!(!project.join("deps/.jumbo-artifacts.json").exists());
    let _ = fs::remove_dir_all(&root);
}

/// A repo map resolves dependencies whose records carry no github.com
/// artifactUrl — here demo-beta (a null-artifact bootstrap-style record),
/// with the tarball served from the artifact cache (the offline
/// transport) under the commit-keyed codeload file name.
#[test]
fn repo_map_resolves_dependencies_without_github_urls() {
    let root = temp_workspace("repomap");
    let index_dir = fixture_index(&root, None);
    let cache = root.join("cache");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::write(
        cache.join("demo_alpha-2.4.0-py3-none-any.whl"),
        DEMO_ALPHA_WHEEL,
    )
    .expect("wheel fixture");
    // The demo-beta record's commit keys the codeload tarball in the cache.
    let beta_commit = "0123456789abcdef0123456789abcdef01234567";
    write_tarball_fixture(
        &cache.join(beta_commit),
        "demo-beta-01234567",
        &real_python_project("demo-beta", "1.4.2"),
    );
    let repo_map = root.join("repo-map.json");
    fs::write(
        &repo_map,
        "{\n  \"demo-beta\": \"https://github.com/acme/demo-beta\"\n}\n",
    )
    .expect("write repo map");
    let project = committed_repo(
        "repomap",
        &[
            ("pyproject.toml", PYPROJECT_WITH_BETA),
            ("uv.lock", UV_LOCK_WITH_BETA),
        ],
    );

    let out = Command::new(jumbo_bin())
        .args([
            "dedup",
            "--manifest",
            "pyproject.toml",
            "--index",
            &index_dir.display().to_string(),
            "--deps",
            "--artifact-dir",
            &cache.display().to_string(),
        ])
        .current_dir(&project)
        .env("JUMBO_REPO_MAP", &repo_map) // the env form of --repo-map
        .env_remove("JUMBO_INDEX_PATH")
        .env_remove("JUMBO_INDEX_URL")
        .env_remove("JUMBO_ARTIFACT_DIR")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .output()
        .expect("run jumbo");
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    let deps = &json["dependencies"];
    let materialized = deps["materialized"].as_array().expect("materialized");
    assert_eq!(materialized.len(), 2, "{materialized:?}");
    let beta = materialized
        .iter()
        .find(|m| m["package"] == "demo-beta")
        .expect("beta entry");
    assert_eq!(beta["mode"], "source");
    assert_eq!(
        beta["url"],
        format!("https://codeload.github.com/acme/demo-beta/tar.gz/{beta_commit}")
    );
    assert!(beta["reason"]
        .as_str()
        .expect("reason")
        .contains("no artifactUrl"));
    assert_eq!(deps["keptSourceOverlays"], serde_json::json!(["demo-beta"]));

    // The real tree stands at the overlay coordinate; the stub is gone.
    let overlay =
        fs::read_to_string(project.join("deps/demo-beta/pyproject.toml")).expect("real source");
    assert!(overlay.contains("name = \"demo-beta\""), "{overlay}");
    assert!(overlay.contains("version = \"1.4.2\""), "{overlay}");
    assert!(
        !overlay.contains("jumbo-injected internal source"),
        "{overlay}"
    );
    assert!(project
        .join("deps/demo-beta/src/demobeta/__init__.py")
        .is_file());
    // alpha still materialized as an artifact.
    assert!(project
        .join("deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl")
        .is_file());
    let _ = fs::remove_dir_all(&root);
}
