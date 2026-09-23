//! CLI contract tests for `jumbo pin`: the deployment pinning manifest
//! (Jumbo Build & Versioning Standard, §3.6).
//!
//! Every test runs the built `jumbo` binary against a local fixture index
//! — no network access, no credentials, no language tooling. The fixture
//! covers recorded and derived (bootstrap) buildIds, commit selection,
//! latest-of-major selection, imageRef construction, and the
//! missing-imageDigest error path.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn jumbo_bin() -> &'static str {
    env!("CARGO_BIN_EXE_jumbo")
}

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "jumbo-pin-cli-{tag}-{seq}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Write a fixture index from `(slug, jsonl lines)` pairs; returns the
/// `index/` directory to pass as `--index`.
fn fixture_index(dir: &Path, files: &[(&str, &[String])]) -> PathBuf {
    let index_dir = dir.join("index");
    fs::create_dir_all(&index_dir).expect("create index dir");
    for (slug, lines) in files {
        fs::write(
            index_dir.join(format!("{slug}.jsonl")),
            lines.join("\n") + "\n",
        )
        .expect("write jsonl");
    }
    index_dir
}

fn record_json(mut fields: serde_json::Value) -> String {
    let mut base = serde_json::json!({
        "package": "demo-alpha",
        "major": 2,
        "version": "2.0.0",
        "commit": "0123456789abcdef0123456789abcdef01234567",
        "fingerprint": null,
        "canonicalExtract": null,
        "artifactUrl": null,
        "artifactSha256": null,
        "imageDigest": null,
        "buildId": null,
        "pipelineRun": null,
        "executor": "circleci",
        "timestamp": "2026-09-01T00:00:00Z"
    });
    let object = base.as_object_mut().expect("base object");
    for (key, value) in fields.as_object_mut().expect("fields object") {
        object.insert(key.clone(), value.take());
    }
    serde_json::to_string(&base).expect("serialize record")
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

/// The standard fixture: three records of demo-alpha major 2 (2.0.0,
/// 2.4.0 with image+artifact, 2.4.1 patch on the same commit) and one
/// bootstrap record of demo-beta with a null buildId.
fn standard_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let dir = temp_dir(tag);
    let extract = serde_json::json!({
        "format": "jumbo-canonical-extract/1",
        "entries": [
            {"name": "numpy", "version": "1.26.4", "source": "pypi",
             "digest": "sha256:aaaa1111aaaa", "path": null}
        ]
    });
    let lines: Vec<String> = vec![
        record_json(serde_json::json!({"version": "2.0.0", "buildId": "demo-2.0.0-001"})),
        record_json(serde_json::json!({
            "version": "2.4.0",
            "buildId": "demo-2.4.0-001",
            "fingerprint": "c".repeat(64),
            "canonicalExtract": extract,
            "artifactUrl": "https://github.com/acme/pkg/releases/download/v2.4.0/demo_alpha-2.4.0-py3-none-any.whl",
            "artifactSha256": "e".repeat(64),
            "imageDigest": format!("sha256:{}", "d".repeat(64)),
            "timestamp": "2026-09-02T00:00:00Z"
        })),
        record_json(serde_json::json!({
            "version": "2.4.1",
            "buildId": "demo-2.4.1-001",
            "timestamp": "2026-09-03T00:00:00Z"
        })),
    ];
    let beta: Vec<String> = vec![record_json(serde_json::json!({
        "package": "demo-beta",
        "major": 1,
        "version": "1.0.0",
        "executor": "bootstrap",
    }))];
    let index_dir = fixture_index(&dir, &[("demo-alpha", &lines), ("demo-beta", &beta)]);
    (dir, index_dir)
}

#[test]
fn pin_by_build_id_emits_the_contract_manifest() {
    let (dir, index_dir) = standard_fixture("byid");
    let out = run_jumbo(
        &[
            "pin",
            "demo-alpha",
            "--by-build-id",
            "demo-2.4.0-001",
            "--index",
            index_dir.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let manifest = stdout_json(&out);

    assert_eq!(manifest["contract"], "jumbo.deployment-pin/v1");
    assert_eq!(manifest["package"], "demo-alpha");
    assert_eq!(manifest["major"], 2);
    assert_eq!(manifest["version"], "2.4.0");
    assert_eq!(manifest["buildId"], "demo-2.4.0-001");
    assert_eq!(manifest["buildIdSource"], "record");
    assert_eq!(
        manifest["commit"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(
        manifest["imageRef"],
        format!("ghcr.io/acme/pkg@sha256:{}", "d".repeat(64))
    );
    assert_eq!(
        manifest["artifact"]["url"],
        "https://github.com/acme/pkg/releases/download/v2.4.0/demo_alpha-2.4.0-py3-none-any.whl"
    );
    assert_eq!(manifest["artifact"]["sha256"], "e".repeat(64));
    assert_eq!(manifest["fingerprint"], "c".repeat(64));
    assert_eq!(manifest["recordRef"]["recordLine"], 2);
    assert!(manifest["recordRef"]["indexFile"]
        .as_str()
        .expect("indexFile")
        .ends_with("demo-alpha.jsonl"));
    assert_eq!(manifest["selector"], "buildId demo-2.4.0-001");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn pin_by_commit_takes_the_newest_record_of_the_commit() {
    let (dir, index_dir) = standard_fixture("bycommit");
    // 2.4.0 and 2.4.1 share the commit; the newest (2.4.1) is pinned.
    let out = run_jumbo(
        &[
            "pin",
            "demo-alpha",
            "--by-commit",
            "0123456789abcdef0123456789abcdef01234567",
            "--index",
            index_dir.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let manifest = stdout_json(&out);
    assert_eq!(manifest["version"], "2.4.1");
    assert_eq!(manifest["recordRef"]["recordLine"], 3);
    assert_eq!(manifest["imageRef"], serde_json::Value::Null);
    assert_eq!(manifest["artifact"], serde_json::Value::Null);
    assert_eq!(
        manifest["selector"],
        "commit 0123456789abcdef0123456789abcdef01234567"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn pin_latest_of_major_matches_resolution() {
    let (dir, index_dir) = standard_fixture("latest");
    let out = run_jumbo(
        &[
            "pin",
            "demo-alpha",
            "--latest-of-major",
            "2",
            "--index",
            index_dir.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let manifest = stdout_json(&out);
    assert_eq!(manifest["version"], "2.4.1");
    assert_eq!(manifest["selector"], "latest of major 2");

    // A missing major errors with the recorded majors listed.
    let out = run_jumbo(
        &[
            "pin",
            "demo-alpha",
            "--latest-of-major",
            "7",
            "--index",
            index_dir.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("no major-7 record"), "got: {err}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn pin_bootstrap_record_derives_its_build_id() {
    let (dir, index_dir) = standard_fixture("bootstrap");
    // Pin the bootstrap record by latest-of-major and read the derived id.
    let out = run_jumbo(
        &[
            "pin",
            "demo-beta",
            "--latest-of-major",
            "1",
            "--index",
            index_dir.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let manifest = stdout_json(&out);
    assert_eq!(manifest["buildIdSource"], "derived");
    let derived = manifest["buildId"]
        .as_str()
        .expect("derived id")
        .to_string();
    assert!(derived.starts_with("bootstrap-"), "got {derived}");

    // The derived id is a first-class pinning key: resolving by it returns
    // the same record.
    let out = run_jumbo(
        &[
            "pin",
            "demo-beta",
            "--by-build-id",
            &derived,
            "--index",
            index_dir.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let again = stdout_json(&out);
    assert_eq!(again["buildId"], derived);
    assert_eq!(again["version"], "1.0.0");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn pin_image_rules_error_clearly() {
    let (dir, index_dir) = standard_fixture("images");

    // --require-image on an imageless record.
    let out = run_jumbo(
        &[
            "pin",
            "demo-alpha",
            "--by-build-id",
            "demo-2.4.1-001",
            "--index",
            index_dir.to_str().unwrap(),
            "--require-image",
        ],
        &dir,
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("PIN_IMAGE_REQUIRED"), "got: {err}");
    assert!(err.contains("no imageDigest"), "got: {err}");

    // An explicit --image-name flows into imageRef.
    let out = run_jumbo(
        &[
            "pin",
            "demo-alpha",
            "--by-build-id",
            "demo-2.4.0-001",
            "--index",
            index_dir.to_str().unwrap(),
            "--image-name",
            "registry.internal/demo-alpha-svc",
        ],
        &dir,
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let manifest = stdout_json(&out);
    assert_eq!(
        manifest["imageRef"],
        format!("registry.internal/demo-alpha-svc@sha256:{}", "d".repeat(64))
    );

    // A malformed recorded digest fails the schema check.
    let bad_dir = temp_dir("baddigest");
    let lines: Vec<String> = vec![record_json(serde_json::json!({
        "buildId": "demo-bad-001",
        "imageDigest": "sha256:NOT_HEX",
    }))];
    let bad_index = fixture_index(&bad_dir, &[("demo-alpha", &lines)]);
    let out = run_jumbo(
        &[
            "pin",
            "demo-alpha",
            "--by-build-id",
            "demo-bad-001",
            "--index",
            bad_index.to_str().unwrap(),
        ],
        &bad_dir,
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("invalid imageDigest"), "got: {err}");
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&bad_dir);
}

#[test]
fn pin_selector_errors_and_exclusivity() {
    let (dir, index_dir) = standard_fixture("errors");

    // Unknown buildId.
    let out = run_jumbo(
        &[
            "pin",
            "demo-alpha",
            "--by-build-id",
            "no-such-id",
            "--index",
            index_dir.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("not found"));

    // Unknown package.
    let out = run_jumbo(
        &[
            "pin",
            "ghost-pkg",
            "--latest-of-major",
            "1",
            "--index",
            index_dir.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("no index records"), "got: {err}");

    // Not exactly one selector.
    let out = run_jumbo(
        &["pin", "demo-alpha", "--index", index_dir.to_str().unwrap()],
        &dir,
    );
    assert!(!out.status.success());
    let out = run_jumbo(
        &[
            "pin",
            "demo-alpha",
            "--latest-of-major",
            "2",
            "--by-build-id",
            "demo-2.0.0-001",
            "--index",
            index_dir.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(!out.status.success());
    let _ = fs::remove_dir_all(&dir);
}
