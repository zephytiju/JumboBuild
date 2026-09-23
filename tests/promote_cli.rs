//! CLI integration tests for `jumbo promote`: the auto-promotion version
//! bump decision (bootstrap / minor / patch / none), the publish-on-bump
//! contract, and the third-party refresh policy knob.
//!
//! Every test runs the built `jumbo` binary against local fixture indexes
//! inside throwaway committed Git repositories — no network access, no
//! credentials, no language tooling required (a PATH shim over `uv` proves
//! the refresh gate switches the lock tool's `--upgrade` step on and off).
//!
//! Same-commit scenarios keep ONE repository and rewrite only the lock
//! file between the recorded build and the current one: generated lock
//! files are exempt from the promotion clean-tree guard, and the own
//! commit — not the lock bytes — is the fingerprint's own-source input.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn jumbo_bin() -> &'static str {
    env!("CARGO_BIN_EXE_jumbo")
}

fn temp_workspace(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "jumbo-promote-cli-{tag}-{seq}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
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
        .env_remove("JUMBO_REFRESH")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .output()
        .expect("run jumbo")
}

fn stdout_json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).expect("stdout is valid JSON")
}

/// Parse the JSON document after any progress-banner lines (commands that
/// run a tool step print `➔ …` banners before their report).
fn stdout_json_after_banners(out: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let start = stdout
        .lines()
        .position(|line| line.trim_start().starts_with('{'))
        .expect("stdout contains a JSON document");
    let document: String = stdout.lines().skip(start).collect::<Vec<_>>().join("\n");
    serde_json::from_str(&document).expect("stdout JSON is valid")
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// The consumer's manifest: own version 2.4.0 (declared major 2).
const PYPROJECT_M2: &str = "[project]\nname = \"consumer\"\nversion = \"2.4.0\"\ndependencies = [\n    \"demo-alpha@2\",\n    \"numpy>=1.26\",\n]\n";

/// Same package, manifest bumped to major 3 by the developer.
const PYPROJECT_M3: &str = "[project]\nname = \"consumer\"\nversion = \"3.0.0\"\ndependencies = [\n    \"demo-alpha@2\",\n    \"numpy>=1.26\",\n]\n";

/// The consumer's lock: root `consumer`, the internal `demo-alpha` as a
/// jumbo-injected source coordinate, and `numpy 1.26.4` from the registry.
const UV_LOCK_A: &str = r#"version = 1
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

/// Formatting-only variant of `UV_LOCK_A`: same resolution, different
/// package order and key formatting (a newer tool version rewrote it).
const UV_LOCK_A_REFORMATTED: &str = r#"version = 1
requires-python = ">=3.12"
manifest-version = "2"

[[package]]
name = "numpy"
version = "1.26.4"
source = { registry = "https://pypi.org/simple" }
sdist = { hash = "sha256:aaaa1111aaaa", url = "https://files.pythonhosted.org/numpy-1.26.4.tar.gz", size = 1 }

[[package]]
name = "demo-alpha"
version = "2.4.0"
source = { directory = "deps/demo-alpha" }

[[package]]
name = "consumer"
version = "0.1.0"
source = { editable = "." }
"#;

/// A third-party in-range update: same commit, `numpy` moved to 1.27.0
/// within its declared `>=1.26` range — extract change, patch bump.
const UV_LOCK_B: &str = r#"version = 1
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
version = "1.27.0"
source = { registry = "https://pypi.org/simple" }
sdist = { url = "https://files.pythonhosted.org/numpy-1.27.0.tar.gz", hash = "sha256:bbbb2222bbbb", size = 1 }
"#;

const OTHER_COMMIT: &str = "1111111111111111111111111111111111111111";

/// One index record line with full control over the promotion-relevant
/// fields.
fn record_json(
    package: &str,
    major: u64,
    version: &str,
    commit: &str,
    fingerprint: Option<&str>,
    canonical_extract: Option<serde_json::Value>,
    timestamp: &str,
) -> String {
    serde_json::json!({
        "package": package,
        "major": major,
        "version": version,
        "commit": commit,
        "fingerprint": fingerprint,
        "canonicalExtract": canonical_extract,
        "artifactUrl": null,
        "artifactSha256": null,
        "imageDigest": null,
        "buildId": format!("{package}-{version}-001"),
        "pipelineRun": Some("circleci/run-42"),
        "executor": "circleci",
        "timestamp": timestamp,
    })
    .to_string()
}

/// Write a fixture index containing one `consumer` record in its own
/// directory (each fixture gets a fresh directory so later fixtures never
/// shadow earlier ones).
fn fixture_index(root: &Path, name: &str, record: &str) -> PathBuf {
    let index_dir = root.join(format!("{name}-index"));
    fs::create_dir_all(&index_dir).expect("create index dir");
    fs::write(index_dir.join("consumer.jsonl"), format!("{record}\n")).expect("write consumer");
    index_dir
}

/// Run `jumbo fingerprint --lock` inside a repo and return its JSON
/// (commit, fingerprint, canonicalExtract of the current lock).
fn fingerprint_report(repo: &Path, lock: &str) -> serde_json::Value {
    let out = run_jumbo(&["fingerprint", "--lock", lock], repo);
    assert!(
        out.status.success(),
        "fingerprint failed: {}",
        stderr_of(&out)
    );
    stdout_json(&out)
}

/// The current time as RFC 3339, shifted back by `seconds`.
fn rfc3339_ago(seconds: u64) -> String {
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(seconds);
    let (days, secs) = (unix / 86_400, unix % 86_400);
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60
    )
}

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Run `jumbo promote` against a fixture index; returns the raw output.
fn promote(repo: &Path, index: &Path, manifest: &str, extra: &[&str]) -> Output {
    let mut args: Vec<&str> = vec!["promote", "--manifest", manifest, "--index"];
    let index = index.display().to_string();
    args.push(&index);
    args.extend_from_slice(extra);
    run_jumbo(&args, repo)
}

/// A committed consumer repo (declared major 2) with the given lock body.
fn consumer_repo(tag: &str, pyproject: &str, lock: &str) -> PathBuf {
    committed_repo(tag, &[("pyproject.toml", pyproject), ("uv.lock", lock)])
}

#[test]
fn bootstrap_first_build_of_a_new_major() {
    let root = temp_workspace("bootstrap");
    // The index knows major 2 of `consumer`; the manifest declares major 3.
    let repo = consumer_repo("bootstrap", PYPROJECT_M3, UV_LOCK_A);
    let index_dir = fixture_index(
        &root,
        "main",
        &record_json(
            "consumer",
            2,
            "2.9.0",
            OTHER_COMMIT,
            Some(&"e".repeat(64)),
            None,
            "2026-01-01T00:00:00Z",
        ),
    );

    let out = promote(&repo, &index_dir, "pyproject.toml", &[]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    let identity = fingerprint_report(&repo, "uv.lock");
    assert_eq!(json["package"], "consumer");
    assert_eq!(json["ecosystem"], "python");
    assert_eq!(json["declaredMajor"], 3);
    assert_eq!(json["currentVersion"], serde_json::json!(null));
    assert_eq!(json["nextVersion"], "3.0.0");
    assert_eq!(json["bump"], "bootstrap");
    assert_eq!(json["publishRequired"], true);
    assert_eq!(json["sourceChange"], true);
    assert_eq!(json["dependencyChange"], false);
    assert_eq!(json["duplicate"], false);
    assert_eq!(json["publish"]["version"], "3.0.0");
    assert_eq!(json["publish"]["major"], 3);
    assert_eq!(json["publish"]["commit"], identity["commit"]);
    assert_eq!(json["publish"]["fingerprint"], identity["fingerprint"]);
    assert_eq!(
        json["publish"]["canonicalExtract"],
        identity["canonicalExtract"]
    );
    // Default refresh policy: re-resolve every run.
    assert_eq!(json["refresh"]["mode"], "run");
    assert_eq!(json["refresh"]["upgrade"], true);
    assert_eq!(
        json["lockToolRan"], false,
        "the committed lock was consumed"
    );
    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&root));
}

#[test]
fn minor_on_own_source_change() {
    let root = temp_workspace("minor");
    let repo = consumer_repo("minor", PYPROJECT_M2, UV_LOCK_A);
    // The newest record of major 2 was built from a different commit.
    let index_dir = fixture_index(
        &root,
        "main",
        &record_json(
            "consumer",
            2,
            "2.4.0",
            OTHER_COMMIT,
            Some(&"e".repeat(64)),
            None,
            "2026-01-01T00:00:00Z",
        ),
    );

    let out = promote(&repo, &index_dir, "pyproject.toml", &[]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["bump"], "minor");
    assert_eq!(json["currentVersion"], "2.4.0");
    assert_eq!(json["nextVersion"], "2.5.0");
    assert_eq!(json["publishRequired"], true);
    assert_eq!(json["sourceChange"], true);
    assert_eq!(json["dependencyChange"], false);
    assert!(json["reason"].as_str().unwrap().contains("own-source"));
    assert_eq!(json["publish"]["version"], "2.5.0");
    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&root));
}

#[test]
fn patch_on_third_party_in_range_update() {
    let root = temp_workspace("patch");
    // One repository, one commit: the recorded resolution pinned numpy
    // 1.26.4; the current resolution pulled 1.27.0 within its declared
    // range. The lock rewrite keeps the own commit (locks are exempt from
    // the promotion guard), so the only change is the dependency closure.
    let repo = consumer_repo("patch", PYPROJECT_M2, UV_LOCK_A);
    let recorded = fingerprint_report(&repo, "uv.lock");
    fs::write(repo.join("uv.lock"), UV_LOCK_B).expect("rewrite lock");
    let current = fingerprint_report(&repo, "uv.lock");
    assert_ne!(
        recorded["fingerprint"], current["fingerprint"],
        "the in-range update must change the extract (and fingerprint)"
    );
    assert_eq!(
        recorded["commit"], current["commit"],
        "the own commit is unchanged"
    );

    let index_dir = fixture_index(
        &root,
        "main",
        &record_json(
            "consumer",
            2,
            "2.4.0",
            recorded["commit"].as_str().unwrap(),
            Some(recorded["fingerprint"].as_str().unwrap()),
            Some(recorded["canonicalExtract"].clone()),
            "2026-01-01T00:00:00Z",
        ),
    );

    let out = promote(&repo, &index_dir, "pyproject.toml", &[]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["bump"], "patch");
    assert_eq!(json["currentVersion"], "2.4.0");
    assert_eq!(json["nextVersion"], "2.4.1");
    assert_eq!(json["publishRequired"], true);
    assert_eq!(json["sourceChange"], false);
    assert_eq!(json["dependencyChange"], true);
    assert_eq!(json["duplicate"], false);
    assert!(json["reason"]
        .as_str()
        .unwrap()
        .contains("dependency-closure"));
    assert_eq!(json["publish"]["fingerprint"], current["fingerprint"]);
    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&root));
}

#[test]
fn none_on_duplicate_fingerprint() {
    let root = temp_workspace("duplicate");
    let repo = consumer_repo("duplicate", PYPROJECT_M2, UV_LOCK_A);
    let identity = fingerprint_report(&repo, "uv.lock");
    // The pipeline of the first build appended this record: same commit,
    // same fingerprint.
    let index_dir = fixture_index(
        &root,
        "main",
        &record_json(
            "consumer",
            2,
            "2.4.0",
            identity["commit"].as_str().unwrap(),
            Some(identity["fingerprint"].as_str().unwrap()),
            Some(identity["canonicalExtract"].clone()),
            "2026-01-01T00:00:00Z",
        ),
    );

    let out = promote(&repo, &index_dir, "pyproject.toml", &[]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["bump"], "none");
    assert_eq!(json["duplicate"], true);
    assert_eq!(json["publishRequired"], false);
    assert_eq!(json["sourceChange"], false);
    assert_eq!(json["dependencyChange"], false);
    assert_eq!(json["nextVersion"], "2.4.0", "reuse the recorded version");
    assert_eq!(json["matchedRecord"]["record"]["version"], "2.4.0");
    assert_eq!(json["matchedRecord"]["recordLine"], 1);
    assert!(json["reason"]
        .as_str()
        .unwrap()
        .contains("duplicate fingerprint"));
    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&root));
}

#[test]
fn none_on_formatting_only_lock_change() {
    let root = temp_workspace("formatting");
    // One repository, one commit: the lock was rewritten by a newer tool
    // version with different formatting — the canonical extract (and
    // therefore the fingerprint) is identical.
    let repo = consumer_repo("formatting", PYPROJECT_M2, UV_LOCK_A);
    let recorded = fingerprint_report(&repo, "uv.lock");
    fs::write(repo.join("uv.lock"), UV_LOCK_A_REFORMATTED).expect("rewrite lock");
    let current = fingerprint_report(&repo, "uv.lock");
    assert_eq!(
        recorded["fingerprint"], current["fingerprint"],
        "formatting-only changes must not move the fingerprint"
    );

    // (a) Via the duplicate-fingerprint hit: the record carries the
    //     computed fingerprint.
    let hit_index = fixture_index(
        &root,
        "hit",
        &record_json(
            "consumer",
            2,
            "2.4.0",
            recorded["commit"].as_str().unwrap(),
            Some(recorded["fingerprint"].as_str().unwrap()),
            Some(recorded["canonicalExtract"].clone()),
            "2026-01-01T00:00:00Z",
        ),
    );
    let out = promote(&repo, &hit_index, "pyproject.toml", &[]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["bump"], "none");
    assert_eq!(json["duplicate"], true);
    assert_eq!(json["publishRequired"], false);

    // (b) Via extract equality alone: the record's fingerprint is null
    //     (predates fingerprinting), the extract still matches.
    let null_fp_index = fixture_index(
        &root,
        "nullfp",
        &record_json(
            "consumer",
            2,
            "2.4.0",
            recorded["commit"].as_str().unwrap(),
            None,
            Some(recorded["canonicalExtract"].clone()),
            "2026-01-01T00:00:00Z",
        ),
    );
    let out = promote(&repo, &null_fp_index, "pyproject.toml", &[]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["bump"], "none");
    assert_eq!(json["duplicate"], false, "null fingerprints never match");
    assert_eq!(json["publishRequired"], false);
    assert_eq!(json["nextVersion"], "2.4.0");
    assert!(json["reason"]
        .as_str()
        .unwrap()
        .contains("identical canonical extract"));
    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&root));
}

const PACKAGE_JSON: &str = r#"{
  "name": "consumer",
  "version": "1.2.0",
  "dependencies": {
    "@juntai/demo-kit": "^1",
    "lodash": "^4.17.21"
  }
}
"#;

const NPM_LOCK: &str = r#"{
  "name": "consumer",
  "version": "1.2.0",
  "lockfileVersion": 3,
  "requires": true,
  "packages": {
    "": { "name": "consumer", "version": "1.2.0" },
    "node_modules/@juntai/demo-kit": { "resolved": "deps/juntai-demo-kit", "link": true },
    "deps/juntai-demo-kit": { "name": "@juntai/demo-kit", "version": "1.2.0" },
    "node_modules/lodash": {
      "version": "4.17.21",
      "resolved": "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz",
      "integrity": "sha512-v2yUIIQQAL05+013r3Ju1cHpIkB8Kf3GwPQmZCuUZZFeEhQqXcHA5ivoZ5S2srBIjPlFaaF500LglEuZHAkYuA=="
    }
  }
}"#;

#[test]
fn npm_ecosystem_promotes_minor_on_source_change() {
    let root = temp_workspace("npm");
    let repo = committed_repo(
        "npm",
        &[
            ("package.json", PACKAGE_JSON),
            ("package-lock.json", NPM_LOCK),
        ],
    );
    let identity = fingerprint_report(&repo, "package-lock.json");
    let index_dir = fixture_index(
        &root,
        "main",
        &record_json(
            "consumer",
            1,
            "1.2.0",
            OTHER_COMMIT,
            Some(&"e".repeat(64)),
            None,
            "2026-01-01T00:00:00Z",
        ),
    );

    let out = promote(&repo, &index_dir, "package.json", &[]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["ecosystem"], "npm");
    assert_eq!(json["declaredMajor"], 1);
    assert_eq!(json["bump"], "minor");
    assert_eq!(json["currentVersion"], "1.2.0");
    assert_eq!(json["nextVersion"], "1.3.0");
    assert_eq!(json["publishRequired"], true);
    assert_eq!(json["publish"]["commit"], identity["commit"]);
    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&root));
}

#[test]
fn promote_honors_the_lock_flag_with_a_sibling_manifest() {
    let root = temp_workspace("lockflag");
    let repo = consumer_repo("lockflag", PYPROJECT_M2, UV_LOCK_A);
    let index_dir = fixture_index(
        &root,
        "main",
        &record_json(
            "consumer",
            2,
            "2.4.0",
            OTHER_COMMIT,
            Some(&"e".repeat(64)),
            None,
            "2026-01-01T00:00:00Z",
        ),
    );
    let index = index_dir.display().to_string();
    let out = run_jumbo(&["promote", "--lock", "uv.lock", "--index", &index], &repo);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["package"], "consumer");
    assert_eq!(json["bump"], "minor");
    assert_eq!(json["nextVersion"], "2.5.0");
    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&root));
}

#[test]
fn refresh_knob_gates_and_is_recorded() {
    let root = temp_workspace("refresh");
    let repo = consumer_repo("refresh", PYPROJECT_M2, UV_LOCK_A);

    // A record appended three days ago: a 7d cadence has not elapsed, so
    // third-party re-resolution is suppressed this run.
    let fresh_index = fixture_index(
        &root,
        "fresh",
        &record_json(
            "consumer",
            2,
            "2.4.0",
            OTHER_COMMIT,
            Some(&"e".repeat(64)),
            None,
            &rfc3339_ago(3 * 86_400),
        ),
    );
    let out = promote(
        &repo,
        &fresh_index,
        "pyproject.toml",
        &["--refresh", "schedule:7d"],
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["refresh"]["policy"], "schedule:7d");
    assert_eq!(json["refresh"]["mode"], "schedule");
    assert_eq!(json["refresh"]["schedule"], "7d");
    assert_eq!(json["refresh"]["scheduleKind"], "interval");
    assert_eq!(json["refresh"]["due"], false);
    assert_eq!(json["refresh"]["upgrade"], false);
    // The bump decision itself is untouched by the knob.
    assert_eq!(json["bump"], "minor");
    assert_eq!(json["publishRequired"], true);

    // Thirty days ago: the cadence elapsed, re-resolution is due.
    let stale_index = fixture_index(
        &root,
        "stale",
        &record_json(
            "consumer",
            2,
            "2.4.0",
            OTHER_COMMIT,
            Some(&"e".repeat(64)),
            None,
            &rfc3339_ago(30 * 86_400),
        ),
    );
    let out = promote(
        &repo,
        &stale_index,
        "pyproject.toml",
        &["--refresh", "schedule:7d"],
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["refresh"]["due"], true);
    assert_eq!(json["refresh"]["upgrade"], true);

    // run mode upgrades regardless of the record's age.
    let out = promote(&repo, &fresh_index, "pyproject.toml", &["--refresh", "run"]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["refresh"]["mode"], "run");
    assert_eq!(json["refresh"]["upgrade"], true);

    // A cron cadence is recorded verbatim; the cadence itself is honored
    // by the executor's pipeline schedule, so the run re-resolves.
    let out = promote(
        &repo,
        &fresh_index,
        "pyproject.toml",
        &["--refresh", "schedule:0 3 * * *"],
    );
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let json = stdout_json(&out);
    assert_eq!(json["refresh"]["schedule"], "0 3 * * *");
    assert_eq!(json["refresh"]["scheduleKind"], "cron");
    assert_eq!(json["refresh"]["upgrade"], true);

    // JUMBO_REFRESH provides the default when no flag is passed.
    let index = fresh_index.display().to_string();
    let out = Command::new(jumbo_bin())
        .args(["promote", "--manifest", "pyproject.toml", "--index", &index])
        .current_dir(&repo)
        .env("JUMBO_REFRESH", "schedule:7d")
        .env_remove("JUMBO_INDEX_PATH")
        .env_remove("JUMBO_INDEX_URL")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .output()
        .expect("run jumbo promote");
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(stdout_json(&out)["refresh"]["upgrade"], false);

    // An explicit flag wins over the environment.
    let out = Command::new(jumbo_bin())
        .args([
            "promote",
            "--manifest",
            "pyproject.toml",
            "--index",
            &index,
            "--refresh",
            "run",
        ])
        .current_dir(&repo)
        .env("JUMBO_REFRESH", "schedule:7d")
        .env_remove("JUMBO_INDEX_PATH")
        .env_remove("JUMBO_INDEX_URL")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .output()
        .expect("run jumbo promote");
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(stdout_json(&out)["refresh"]["upgrade"], true);

    // Malformed policies are actionable errors.
    let out = promote(
        &repo,
        &fresh_index,
        "pyproject.toml",
        &["--refresh", "sometimes"],
    );
    assert!(!out.status.success());
    let stderr = stderr_of(&out);
    assert!(stderr.contains("invalid refresh policy"), "{stderr}");
    assert!(stderr.contains("run"), "{stderr}");
    assert!(stderr.contains("schedule:"), "{stderr}");

    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&root));
}

#[test]
fn refresh_gate_switches_the_lock_tool_upgrade_step() {
    let root = temp_workspace("gate");
    // The consumer project depends on the internal demo-alpha@2, so
    // `jumbo lock` injects its source overlay before running the tool.
    let repo = temp_workspace("gate-repo");
    fs::write(repo.join("pyproject.toml"), PYPROJECT_M2).expect("manifest");
    run_git(&repo, &["init", "-q", "--initial-branch=main"]);
    run_git(&repo, &["config", "user.email", "jumbo@test.invalid"]);
    run_git(&repo, &["config", "user.name", "Jumbo Test"]);
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "fixture"]);

    let index_dir = root.join("index");
    fs::create_dir_all(&index_dir).expect("index dir");
    fs::write(
        index_dir.join("demo-alpha.jsonl"),
        record_json(
            "demo-alpha",
            2,
            "2.4.0",
            OTHER_COMMIT,
            None,
            None,
            "2026-01-01T00:00:00Z",
        ) + "\n",
    )
    .expect("write demo-alpha");
    fs::write(
        index_dir.join("consumer.jsonl"),
        record_json(
            "consumer",
            2,
            "2.4.0",
            OTHER_COMMIT,
            None,
            None,
            &rfc3339_ago(3 * 86_400), // three days old
        ) + "\n",
    )
    .expect("write consumer");

    // The uv shim records its invocation; jumbo lock must not need a real
    // toolchain for the gate to be observable.
    let shim = root.join("shim");
    fs::create_dir_all(&shim).expect("shim dir");
    let log = root.join("tool-invocations.log");
    let script = shim.join("uv");
    fs::write(
        &script,
        format!("#!/bin/sh\necho \"uv $*\" >> {:?}\nexit 0\n", log.display()),
    )
    .expect("write shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755));
    }

    let run_lock = |refresh: &str| {
        let index = index_dir.display().to_string();
        Command::new(jumbo_bin())
            .args([
                "lock",
                "--manifest",
                "pyproject.toml",
                "--index",
                &index,
                "--refresh",
                refresh,
            ])
            .current_dir(&repo)
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
            .env_remove("JUMBO_REFRESH")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_TOKEN")
            .output()
            .expect("run jumbo lock")
    };

    // Default (run): the tool re-resolves third-party ranges.
    let out = run_lock("run");
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let report = stdout_json_after_banners(&out);
    assert_eq!(report["refresh"]["upgrade"], true);
    assert_eq!(
        fs::read_to_string(&log).unwrap_or_default(),
        "uv lock --upgrade\n",
        "run mode must pass --upgrade"
    );

    // schedule:7d with a three-day-old record: the upgrade step is
    // suppressed and the current resolution is reused.
    fs::remove_file(&log).ok();
    let out = run_lock("schedule:7d");
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let report = stdout_json_after_banners(&out);
    assert_eq!(report["refresh"]["upgrade"], false);
    assert_eq!(
        fs::read_to_string(&log).unwrap_or_default(),
        "uv lock\n",
        "the suppressed gate must drop --upgrade"
    );

    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&root));
}

#[test]
fn promote_refuses_a_dirty_working_tree() {
    let root = temp_workspace("dirty");
    let repo = committed_repo(
        "dirty",
        &[
            ("pyproject.toml", PYPROJECT_M2),
            ("uv.lock", UV_LOCK_A),
            ("src.py", "print(\"hello\")\n"),
        ],
    );
    let index_dir = fixture_index(
        &root,
        "main",
        &record_json(
            "consumer",
            2,
            "2.4.0",
            OTHER_COMMIT,
            Some(&"e".repeat(64)),
            None,
            "2026-01-01T00:00:00Z",
        ),
    );

    // Clean tree promotes.
    let out = promote(&repo, &index_dir, "pyproject.toml", &[]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));

    // A modified source file refuses: local builds on dirty trees never
    // promote.
    fs::write(repo.join("src.py"), "print(\"dirty\")\n").expect("edit source");
    let out = promote(&repo, &index_dir, "pyproject.toml", &[]);
    assert!(!out.status.success(), "a dirty tree must refuse promotion");
    let stderr = stderr_of(&out);
    assert!(stderr.contains("promotion refused"), "{stderr}");
    assert!(stderr.contains("src.py"), "{stderr}");
    let _ = (fs::remove_dir_all(&repo), fs::remove_dir_all(&root));
}

#[test]
fn promote_argument_validation() {
    let root = temp_workspace("args");
    let repo = consumer_repo("args", PYPROJECT_M2, UV_LOCK_A);
    let index_dir = fixture_index(
        &root,
        "main",
        &record_json(
            "consumer",
            2,
            "2.4.0",
            OTHER_COMMIT,
            Some(&"e".repeat(64)),
            None,
            "2026-01-01T00:00:00Z",
        ),
    );
    let index = index_dir.display().to_string();

    // --lock and --manifest cannot be combined.
    let out = run_jumbo(
        &[
            "promote",
            "--lock",
            "uv.lock",
            "--manifest",
            "pyproject.toml",
        ],
        &repo,
    );
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("cannot be combined"));

    // A missing manifest version has no declared major.
    let unversioned = committed_repo(
        "unversioned",
        &[
            ("pyproject.toml", "[project]\nname = \"consumer\"\n"),
            ("uv.lock", UV_LOCK_A),
        ],
    );
    let out = run_jumbo(
        &["promote", "--manifest", "pyproject.toml", "--index", &index],
        &unversioned,
    );
    assert!(!out.status.success());
    let stderr = stderr_of(&out);
    assert!(stderr.contains("[project].version"), "{stderr}");

    // A malformed manifest version is rejected with guidance.
    let malformed = committed_repo(
        "malformed",
        &[
            (
                "pyproject.toml",
                "[project]\nname = \"consumer\"\nversion = \"2\"\n",
            ),
            ("uv.lock", UV_LOCK_A),
        ],
    );
    let out = run_jumbo(
        &["promote", "--manifest", "pyproject.toml", "--index", &index],
        &malformed,
    );
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("major.minor.patch"),
        "stderr: {}",
        stderr_of(&out)
    );

    // --lock without a sibling manifest cannot resolve the declared major.
    let orphan = committed_repo("orphan", &[("uv.lock", UV_LOCK_A)]);
    let out = run_jumbo(
        &["promote", "--lock", "uv.lock", "--index", &index],
        &orphan,
    );
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("declared major"),
        "stderr: {}",
        stderr_of(&out)
    );

    let _ = (
        fs::remove_dir_all(&repo),
        fs::remove_dir_all(&unversioned),
        fs::remove_dir_all(&malformed),
        fs::remove_dir_all(&orphan),
        fs::remove_dir_all(&root),
    );
}
