//! CLI contract tests for pinned reproduction: `jumbo build --pinned
//! <buildId>` and `jumbo reproduce <buildId>` (Jumbo Build & Versioning
//! Standard, §2.4 — the record is the lock).
//!
//! Every test runs the built `jumbo` binary against a local fixture index
//! and a local artifact cache (the `--artifact-dir` transport — the
//! recorded SHA-256 is still enforced) — no network access, no
//! credentials. The fixtures cover the fingerprint-match path, the
//! mismatch abort, and the bootstrap-record refusal.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest as _, Sha256};

fn jumbo_bin() -> &'static str {
    env!("CARGO_BIN_EXE_jumbo")
}

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "jumbo-repro-cli-{tag}-{seq}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The fingerprint preimage the standard defines:
/// `<40-hex commit> "\n" <canonical extract JSON>` under sha256.
fn fingerprint_of(commit: &str, extract_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(commit.as_bytes());
    hasher.update(b"\n");
    hasher.update(extract_json.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
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

/// A fixture whose recorded fingerprint, artifact digests, and artifact
/// bytes all agree — the reproducible case — unless `tampered_fingerprint`
/// replaces the recorded value.
fn fixture(tag: &str, tampered_fingerprint: bool) -> (PathBuf, PathBuf) {
    let dir = temp_dir(tag);
    let cache = dir.join("cache");
    fs::create_dir_all(&cache).expect("cache dir");

    let dep_wheel = b"demo wheel bytes";
    let own_wheel = b"consumer wheel bytes";
    fs::write(cache.join("demo_alpha-2.4.0-py3-none-any.whl"), dep_wheel).expect("dep wheel");
    fs::write(cache.join("consumer-2.4.0-py3-none-any.whl"), own_wheel).expect("own wheel");

    let own_commit = "fedcba9876543210fedcba9876543210fedcba98";
    let extract = serde_json::json!({
        "format": "jumbo-canonical-extract/1",
        "entries": [
            {"name": "demo-alpha", "version": "2.4.0", "source": "index",
             "digest": null, "path": "deps/demo-alpha"},
            {"name": "numpy", "version": "1.26.4", "source": "pypi",
             "digest": "sha256:aaaa1111aaaa", "path": null}
        ]
    });
    // The fingerprint preimage is the canonical serialization of the typed
    // CanonicalExtract: compact, struct field order (format, entries;
    // name, version, source, digest, path), sorted entries — spelled out
    // here byte-exactly because serde_json::Value would reorder keys.
    let canonical_json = concat!(
        r#"{"format":"jumbo-canonical-extract/1","entries":"#,
        r#"[{"name":"demo-alpha","version":"2.4.0","source":"index","digest":null,"path":"deps/demo-alpha"},"#,
        r#"{"name":"numpy","version":"1.26.4","source":"pypi","digest":"sha256:aaaa1111aaaa","path":null}]}"#
    );
    let fingerprint = if tampered_fingerprint {
        "f".repeat(64)
    } else {
        fingerprint_of(own_commit, canonical_json)
    };

    let dep = serde_json::json!({
        "package": "demo-alpha", "major": 2, "version": "2.4.0",
        "commit": "0123456789abcdef0123456789abcdef01234567",
        "fingerprint": null, "canonicalExtract": null,
        "artifactUrl": "https://github.com/acme/demo-alpha/releases/download/v2.4.0/demo_alpha-2.4.0-py3-none-any.whl",
        "artifactSha256": sha256_hex(dep_wheel),
        "imageDigest": null, "buildId": "demo-2.4.0-001",
        "pipelineRun": "https://circleci.com/gh/acme/demo-alpha/42",
        "executor": "circleci", "timestamp": "2026-09-10T00:00:00Z"
    });
    let own = serde_json::json!({
        "package": "consumer", "major": 2, "version": "2.4.0",
        "commit": own_commit,
        "fingerprint": fingerprint, "canonicalExtract": extract,
        "artifactUrl": "https://github.com/acme/consumer/releases/download/v2.4.0/consumer-2.4.0-py3-none-any.whl",
        "artifactSha256": sha256_hex(own_wheel),
        "imageDigest": format!("sha256:{}", "d".repeat(64)),
        "buildId": "consumer-2.4.0-001",
        "pipelineRun": "https://circleci.com/gh/acme/consumer/7",
        "executor": "circleci", "timestamp": "2026-09-11T00:00:00Z"
    });

    let index_dir = dir.join("index");
    fs::create_dir_all(&index_dir).expect("index dir");
    fs::write(
        index_dir.join("demo-alpha.jsonl"),
        serde_json::to_string(&dep).expect("dep json") + "\n",
    )
    .expect("write dep jsonl");
    fs::write(
        index_dir.join("consumer.jsonl"),
        serde_json::to_string(&own).expect("own json") + "\n",
    )
    .expect("write own jsonl");
    (dir, cache)
}

#[test]
fn build_pinned_reproduces_the_recorded_digests() {
    let (dir, cache) = fixture("ok", false);
    let out = run_jumbo(
        &[
            "build",
            "--pinned",
            "consumer-2.4.0-001",
            "--index",
            dir.join("index").to_str().unwrap(),
            "--artifact-dir",
            cache.to_str().unwrap(),
            "--out",
            dir.join("reproduced").to_str().unwrap(),
        ],
        &dir,
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let report = stdout_json(&out);

    assert_eq!(report["contract"], "jumbo.pinned-reproduction/1");
    assert_eq!(report["package"], "consumer");
    assert_eq!(report["buildId"], "consumer-2.4.0-001");
    assert_eq!(
        report["recordedFingerprint"],
        report["recomputedFingerprint"]
    );
    assert_eq!(report["fingerprintMatch"], true);

    // The own artifact reproduced the recorded digest.
    let artifact = &report["artifact"];
    assert_eq!(artifact["sha256"], sha256_hex(b"consumer wheel bytes"));
    assert_eq!(artifact["path"], "dist/consumer-2.4.0-py3-none-any.whl");
    let placed = dir
        .join("reproduced")
        .join("dist")
        .join("consumer-2.4.0-py3-none-any.whl");
    assert!(placed.is_file(), "artifact placed");
    assert_eq!(
        sha256_hex(&fs::read(&placed).expect("read placed")),
        artifact["sha256"].as_str().expect("digest")
    );

    // The internal closure dependency materialized at its exact recorded
    // version, sha256-enforced.
    let deps = report["materializedDependencies"].as_array().expect("deps");
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0]["package"], "demo-alpha");
    assert_eq!(deps[0]["version"], "2.4.0");
    assert_eq!(
        deps[0]["path"],
        "deps/demo-alpha/demo_alpha-2.4.0-py3-none-any.whl"
    );
    assert_eq!(deps[0]["sha256"], sha256_hex(b"demo wheel bytes"));
    assert!(dir
        .join("reproduced")
        .join("deps/demo-alpha")
        .join("demo_alpha-2.4.0-py3-none-any.whl")
        .is_file());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn reproduce_subcommand_matches_build_pinned() {
    let (dir, cache) = fixture("subcmd", false);
    let out = run_jumbo(
        &[
            "reproduce",
            "consumer-2.4.0-001",
            "--index",
            dir.join("index").to_str().unwrap(),
            "--artifact-dir",
            cache.to_str().unwrap(),
            "--out",
            dir.join("reproduced").to_str().unwrap(),
        ],
        &dir,
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let report = stdout_json(&out);
    assert_eq!(report["contract"], "jumbo.pinned-reproduction/1");
    assert_eq!(report["fingerprintMatch"], true);
    assert!(report["artifact"].is_object());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn fingerprint_mismatch_aborts_before_any_output() {
    let (dir, cache) = fixture("mismatch", true);
    let out_dir = dir.join("reproduced");
    let out = run_jumbo(
        &[
            "reproduce",
            "consumer-2.4.0-001",
            "--index",
            dir.join("index").to_str().unwrap(),
            "--artifact-dir",
            cache.to_str().unwrap(),
            "--out",
            out_dir.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(!out.status.success(), "must fail");
    let err = stderr_of(&out);
    assert!(err.contains("PIN_FINGERPRINT_MISMATCH"), "got: {err}");
    assert!(
        err.contains(&"f".repeat(64)),
        "must name the recorded value: {err}"
    );
    // Aborted before materializing anything.
    assert!(!out_dir.exists(), "no outputs may exist after an abort");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn tampered_artifact_bytes_abort_with_a_digest_mismatch() {
    let (dir, cache) = fixture("tampered", false);
    // Corrupt the cached dependency wheel: the recorded digest must fail.
    fs::write(
        cache.join("demo_alpha-2.4.0-py3-none-any.whl"),
        b"tampered bytes",
    )
    .expect("tamper");
    let out = run_jumbo(
        &[
            "reproduce",
            "consumer-2.4.0-001",
            "--index",
            dir.join("index").to_str().unwrap(),
            "--artifact-dir",
            cache.to_str().unwrap(),
            "--out",
            dir.join("reproduced").to_str().unwrap(),
        ],
        &dir,
    );
    assert!(!out.status.success(), "must fail");
    let err = stderr_of(&out);
    assert!(err.contains("digest mismatch"), "got: {err}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn bootstrap_record_and_unknown_build_id_error_clearly() {
    let (dir, cache) = fixture("bootstrap", false);
    let index_dir = dir.join("index");
    let legacy = serde_json::json!({
        "package": "legacy-pkg", "major": 1, "version": "1.0.0",
        "commit": "0123456789abcdef0123456789abcdef01234567",
        "fingerprint": null, "canonicalExtract": null,
        "artifactUrl": null, "artifactSha256": null,
        "imageDigest": null, "buildId": null, "pipelineRun": null,
        "executor": "bootstrap", "timestamp": "2026-09-01T00:00:00Z"
    });
    fs::write(
        index_dir.join("legacy-pkg.jsonl"),
        serde_json::to_string(&legacy).expect("json") + "\n",
    )
    .expect("write legacy jsonl");

    // A bootstrap record cannot be reproduced — pin it first to learn its
    // derived buildId, then ask for a reproduction.
    let out = run_jumbo(
        &[
            "pin",
            "legacy-pkg",
            "--latest-of-major",
            "1",
            "--index",
            index_dir.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let derived = stdout_json(&out)["buildId"]
        .as_str()
        .expect("id")
        .to_string();

    let out = run_jumbo(
        &[
            "reproduce",
            &derived,
            "--index",
            index_dir.to_str().unwrap(),
            "--artifact-dir",
            cache.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(!out.status.success(), "must fail");
    let err = stderr_of(&out);
    assert!(err.contains("cannot be reproduced"), "got: {err}");
    assert!(err.contains("null fingerprint"), "got: {err}");

    // An unknown buildId is a clean typed error.
    let out = run_jumbo(
        &[
            "reproduce",
            "no-such-build",
            "--index",
            index_dir.to_str().unwrap(),
        ],
        &dir,
    );
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("not found"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn closure_dep_without_artifact_keeps_its_source_coordinate() {
    let (dir, cache) = fixture("overlay", false);
    // demo-gamma is in the closure but publishes no artifact: its
    // deps/<slug> source coordinate stands (the `jumbo dedup --deps` rule).
    let gamma = serde_json::json!({
        "package": "demo-gamma", "major": 1, "version": "1.0.0",
        "commit": "1111111111111111111111111111111111111111",
        "fingerprint": null, "canonicalExtract": null,
        "artifactUrl": null, "artifactSha256": null,
        "imageDigest": null, "buildId": "gamma-1.0.0-001",
        "pipelineRun": null, "executor": "circleci",
        "timestamp": "2026-09-09T00:00:00Z"
    });
    let index_dir = dir.join("index");
    fs::write(
        index_dir.join("demo-gamma.jsonl"),
        serde_json::to_string(&gamma).expect("json") + "\n",
    )
    .expect("write gamma jsonl");

    // Extend consumer's extract with demo-gamma and recompute the
    // fingerprint over the new closure.
    let own_commit = "fedcba9876543210fedcba9876543210fedcba98";
    let extract = serde_json::json!({
        "format": "jumbo-canonical-extract/1",
        "entries": [
            {"name": "demo-alpha", "version": "2.4.0", "source": "index",
             "digest": null, "path": "deps/demo-alpha"},
            {"name": "demo-gamma", "version": "1.0.0", "source": "index",
             "digest": null, "path": "deps/demo-gamma"},
            {"name": "numpy", "version": "1.26.4", "source": "pypi",
             "digest": "sha256:aaaa1111aaaa", "path": null}
        ]
    });
    let canonical_json = concat!(
        r#"{"format":"jumbo-canonical-extract/1","entries":"#,
        r#"[{"name":"demo-alpha","version":"2.4.0","source":"index","digest":null,"path":"deps/demo-alpha"},"#,
        r#"{"name":"demo-gamma","version":"1.0.0","source":"index","digest":null,"path":"deps/demo-gamma"},"#,
        r#"{"name":"numpy","version":"1.26.4","source":"pypi","digest":"sha256:aaaa1111aaaa","path":null}]}"#
    );
    let own = serde_json::json!({
        "package": "consumer", "major": 2, "version": "2.5.0",
        "commit": own_commit,
        "fingerprint": fingerprint_of(own_commit, canonical_json),
        "canonicalExtract": extract,
        "artifactUrl": null, "artifactSha256": null,
        "imageDigest": null, "buildId": "consumer-2.5.0-001",
        "pipelineRun": null, "executor": "circleci",
        "timestamp": "2026-09-12T00:00:00Z"
    });
    fs::write(
        index_dir.join("consumer.jsonl"),
        serde_json::to_string(&own).expect("json") + "\n",
    )
    .expect("rewrite own jsonl");

    let out = run_jumbo(
        &[
            "reproduce",
            "consumer-2.5.0-001",
            "--index",
            index_dir.to_str().unwrap(),
            "--artifact-dir",
            cache.to_str().unwrap(),
            "--out",
            dir.join("reproduced").to_str().unwrap(),
        ],
        &dir,
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let report = stdout_json(&out);
    assert_eq!(report["fingerprintMatch"], true);
    let deps = report["materializedDependencies"].as_array().expect("deps");
    assert_eq!(deps.len(), 1, "only demo-alpha has an artifact");
    let overlays = report["sourceOverlayDependencies"]
        .as_array()
        .expect("overlays");
    assert_eq!(overlays.len(), 1);
    assert_eq!(overlays[0], "demo-gamma");
    assert!(
        report["artifact"].is_null(),
        "record published no own artifact"
    );
    let _ = fs::remove_dir_all(&dir);
}
