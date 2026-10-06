//! Developer workspace acceptance: real npm/uv builds, remote fallback and
//! immutable-release guards. No organization credentials enter these fixtures.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture(PathBuf);
impl Fixture {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "jumbo-local-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(path.join("projects")).unwrap();
        Self(path)
    }
    fn project(&self, name: &str) -> PathBuf {
        self.0.join("projects").join(name)
    }
    fn initialize(&self) {
        let name = self.0.file_name().unwrap().to_str().unwrap();
        success(jumbo(
            &["workspace", "create", "--import", name],
            self.0.parent().unwrap(),
        ));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn jumbo(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jumbo"))
        .args(args)
        .current_dir(cwd)
        .env_remove("JUMBO_INDEX_PATH")
        .env_remove("JUMBO_INDEX_URL")
        .env_remove("JUMBO_ARTIFACT_DIR")
        .env_remove("JUMBO_REPO_MAP")
        .env(
            "UV_CACHE_DIR",
            std::env::var("UV_CACHE_DIR")
                .unwrap_or_else(|_| "/tmp/jumbo-local-uv-cache".to_owned()),
        )
        .env(
            "npm_config_cache",
            std::env::temp_dir().join("jumbo-local-npm-cache"),
        )
        .env("npm_config_audit", "false")
        .env("npm_config_fund", "false")
        .output()
        .unwrap()
}
fn success(out: Output) -> String {
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}
fn available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}
fn node(path: &Path, name: &str, deps: serde_json::Value, build: &str, private: bool) {
    fs::create_dir_all(path).unwrap();
    fs::write(path.join("package.json"), serde_json::to_string_pretty(&serde_json::json!({
        "name":name,"version":"1.2.0","private":private,"main":"dist/index.cjs",
        "dependencies":deps,"scripts":{"build":"node build.cjs","test":"node check.cjs","format:check":"node check.cjs"}
    })).unwrap()+"\n").unwrap();
    fs::write(
        path.join("build.cjs"),
        format!(
            "const fs = require('node:fs'); fs.mkdirSync('dist', {{recursive:true}}); {build}\n"
        ),
    )
    .unwrap();
    fs::write(
        path.join("check.cjs"),
        "require('node:assert/strict').equal(typeof require('./dist/index.cjs'), 'number');\n",
    )
    .unwrap();
}
fn git(path: &Path) {
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "jumbo@test.invalid"],
        &["config", "user.name", "Jumbo Test"],
        &["add", "."],
        &["commit", "-qm", "fixture"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap()
            .success());
    }
}

#[test]
fn node_chain_builds_local_public_private_and_changed_sources_in_order() {
    if !available("npm") {
        eprintln!("skip: npm unavailable");
        return;
    }
    let f = Fixture::new("node-chain");
    let a = f.project("folder-unrelated-to-package-a");
    let b = f.project("private-producer");
    let c = f.project("consumer");
    node(&a,"public-local-base",serde_json::json!({}),"fs.writeFileSync('dist/index.cjs', 'module.exports = ' + fs.readFileSync('value.txt','utf8'));",false);
    fs::write(a.join("value.txt"), "10").unwrap();
    node(
        &b,
        "@juntai/private-local",
        serde_json::json!({"public-local-base":"^1.1"}),
        "fs.writeFileSync('dist/index.cjs','module.exports = '+(require('public-local-base')+1));",
        true,
    );
    node(&c,"@outside-org/consumer",serde_json::json!({"@juntai/private-local":">=1 <2"}),"fs.writeFileSync('dist/index.cjs','module.exports = '+(require('@juntai/private-local')+1));",false);
    git(&a);
    git(&b);
    git(&c);
    for (path, url) in [
        (&a, "https://github.com/public-org/base.git"),
        (&b, "ssh://git@gitlab.example/private-org/base.git"),
        (&c, "https://forge.example/another-org/consumer.git"),
    ] {
        assert!(Command::new("git")
            .args(["remote", "add", "origin", url])
            .current_dir(path)
            .status()
            .unwrap()
            .success());
    }
    f.initialize();
    let manifests: Vec<_> = [&a, &b, &c]
        .into_iter()
        .map(|p| fs::read(p.join("package.json")).unwrap())
        .collect();
    // A local-only closure never contacts an index, even when a developer's
    // configured index is temporarily unavailable.
    let out = Command::new(env!("CARGO_BIN_EXE_jumbo"))
        .arg("release")
        .current_dir(&c)
        .env("JUMBO_INDEX_PATH", f.0.join("unavailable-index"))
        .env(
            "npm_config_cache",
            std::env::temp_dir().join("jumbo-local-npm-cache"),
        )
        .env("npm_config_audit", "false")
        .output()
        .unwrap();
    let stdout = success(out);
    assert!(
        stdout
            .find("Building local dependency public-local-base")
            .unwrap()
            < stdout
                .find("Building local dependency @juntai/private-local")
                .unwrap()
    );
    assert_eq!(
        fs::read_to_string(c.join("dist/index.cjs")).unwrap(),
        "module.exports = 12"
    );
    for (p, bytes) in [&a, &b, &c].into_iter().zip(&manifests) {
        assert_eq!(&fs::read(p.join("package.json")).unwrap(), bytes);
    }
    fs::write(a.join("value.txt"), "20").unwrap();
    success(jumbo(&["build"], &c));
    assert_eq!(
        fs::read_to_string(c.join("dist/index.cjs")).unwrap(),
        "module.exports = 22"
    );
    // A dependency's unchanged version/consumer commit cannot hide its changed
    // checkout as a reproducible published input.
    let out = jumbo(&["fingerprint", "--lock", "package-lock.json"], &c);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("local workspace inputs"));
}

#[test]
fn duplicate_incompatible_and_cyclic_local_packages_fail_before_building() {
    for (tag, other_name, other_version, other_deps, message) in [
        (
            "duplicate",
            "same-package",
            "1.0.0",
            serde_json::json!({}),
            "ambiguous local package",
        ),
        (
            "incompatible",
            "dependency",
            "2.0.0",
            serde_json::json!({}),
            "incompatible local package",
        ),
        (
            "cycle",
            "dependency",
            "1.0.0",
            serde_json::json!({"same-package":"1"}),
            "local dependency cycle",
        ),
    ] {
        let f = Fixture::new(tag);
        let a = f.project("a");
        let b = f.project("b");
        node(
            &a,
            "same-package",
            serde_json::json!({"dependency":"^1"}),
            "fs.writeFileSync('dist/index.cjs','module.exports = 1');",
            false,
        );
        node(
            &b,
            other_name,
            other_deps,
            "fs.writeFileSync('dist/index.cjs','module.exports = 1');",
            true,
        );
        let mut doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(b.join("package.json")).unwrap()).unwrap();
        doc["version"] = other_version.into();
        fs::write(b.join("package.json"), serde_json::to_string(&doc).unwrap()).unwrap();
        f.initialize();
        let before = fs::read(a.join("package.json")).unwrap();
        let out = jumbo(&["build"], &a);
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!a.join("dist").exists());
        assert_eq!(before, fs::read(a.join("package.json")).unwrap());
    }
}

#[test]
fn failing_build_restores_every_manifest_verbatim() {
    if !available("npm") {
        return;
    }
    let f = Fixture::new("failure");
    let a = f.project("a");
    let b = f.project("b");
    node(
        &a,
        "base",
        serde_json::json!({}),
        "process.exit(17);",
        false,
    );
    node(
        &b,
        "consumer",
        serde_json::json!({"base":"1"}),
        "fs.writeFileSync('dist/index.cjs','module.exports = 1');",
        true,
    );
    f.initialize();
    let originals = [
        fs::read(a.join("package.json")).unwrap(),
        fs::read(b.join("package.json")).unwrap(),
    ];
    assert!(!jumbo(&["build"], &b).status.success());
    assert_eq!(originals[0], fs::read(a.join("package.json")).unwrap());
    assert_eq!(originals[1], fs::read(b.join("package.json")).unwrap());
}

#[test]
fn absent_node_checkout_uses_verified_remote_index_artifact() {
    if !available("npm") {
        return;
    }
    let f = Fixture::new("remote");
    let a = f.project("a");
    let c = f.project("c");
    node(
        &a,
        "@juntai/remote-base",
        serde_json::json!({}),
        "fs.writeFileSync('dist/index.cjs','module.exports = 7');",
        true,
    );
    node(
        &c,
        "consumer",
        serde_json::json!({"@juntai/remote-base":"^1"}),
        "fs.writeFileSync('dist/index.cjs','module.exports = '+require('@juntai/remote-base'));",
        false,
    );
    f.initialize();
    success(jumbo(&["build"], &a));
    let cache = f.0.join("cache");
    fs::create_dir_all(&cache).unwrap();
    let pack = Command::new("npm")
        .args([
            "pack",
            "--ignore-scripts",
            "--json",
            "--pack-destination",
            cache.to_str().unwrap(),
        ])
        .current_dir(&a)
        .env(
            "npm_config_cache",
            std::env::temp_dir().join("jumbo-local-npm-cache"),
        )
        .output()
        .unwrap();
    assert!(
        pack.status.success(),
        "{}",
        String::from_utf8_lossy(&pack.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&pack.stdout).unwrap();
    let filename = doc[0]["filename"].as_str().unwrap();
    let sha = jumbo_build::dedup::sha256_of_file(&cache.join(filename)).unwrap();
    let index = f.0.join("index");
    fs::create_dir_all(&index).unwrap();
    fs::write(index.join("juntai-remote-base.jsonl"),serde_json::to_string(&serde_json::json!({
        "package":"@juntai/remote-base","major":1,"version":"1.2.0","commit":"0123456789abcdef0123456789abcdef01234567",
        "artifactUrl":format!("https://github.com/private-org/base/releases/download/v1.2.0/{filename}"),"artifactSha256":sha,"timestamp":"2026-10-06T00:00:00Z"
    })).unwrap()+"\n").unwrap();
    fs::remove_dir_all(&a).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_jumbo"))
        .arg("build")
        .current_dir(&c)
        .env("JUMBO_INDEX_PATH", &index)
        .env("JUMBO_ARTIFACT_DIR", &cache)
        .env(
            "npm_config_cache",
            std::env::temp_dir().join("jumbo-local-npm-cache"),
        )
        .env("npm_config_audit", "false")
        .output()
        .unwrap();
    success(out);
    assert_eq!(
        fs::read_to_string(c.join("dist/index.cjs")).unwrap(),
        "module.exports = 7"
    );
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(c.join("package.json")).unwrap()).unwrap();
    assert_eq!(original["dependencies"]["@juntai/remote-base"], "^1");
}

fn python(path: &Path, name: &str, deps: &str, value: &str) {
    fs::create_dir_all(path.join("src")).unwrap();
    let module = name.replace('-', "_").to_ascii_lowercase();
    fs::write(path.join("pyproject.toml"),format!("[project]\nname = \"{name}\"\nversion = \"1.2.0\"\ndependencies = [{deps}]\n[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n[tool.hatch.build.targets.wheel]\npackages = [\"src/{module}\"]\n")).unwrap();
    fs::create_dir_all(path.join(format!("src/{module}"))).unwrap();
    fs::write(path.join(format!("src/{module}/__init__.py")), value).unwrap();
}

#[test]
fn python_uv_workspace_uses_normalized_identity_and_changed_local_source() {
    if !available("uv") {
        assert!(
            std::env::var_os("JUMBO_REQUIRE_PYTHON_TOOLCHAIN").is_none(),
            "CI requires uv"
        );
        eprintln!("skip: uv unavailable");
        return;
    }
    let f = Fixture::new("python-chain");
    let a = f.project("unrelated-folder");
    let b = f.project("private-middle");
    let c = f.project("consumer");
    python(&a, "public-base", "", "VALUE = 10\n");
    python(
        &b,
        "private-middle",
        "\"Public_Base@1\"",
        "from public_base import VALUE as BASE\nVALUE = BASE + 1\n",
    );
    python(
        &c,
        "consumer",
        "\"private-middle>=1,<2\"",
        "from private_middle import VALUE as BASE\nVALUE = BASE + 1\n",
    );
    // An npm package with the same name does not shadow the Python package.
    node(
        &f.project("node-base"),
        "public-base",
        serde_json::json!({}),
        "fs.writeFileSync('dist/index.cjs','module.exports = 999');",
        false,
    );
    git(&a);
    git(&b);
    git(&c);
    for (p, url) in [
        (&a, "https://github.com/public-org/base.git"),
        (&b, "ssh://git@gitlab.example/private-org/base.git"),
    ] {
        assert!(Command::new("git")
            .args(["remote", "add", "origin", url])
            .current_dir(p)
            .status()
            .unwrap()
            .success());
    }
    f.initialize();
    let manifests: Vec<_> = [&a, &b, &c]
        .into_iter()
        .map(|p| fs::read(p.join("pyproject.toml")).unwrap())
        .collect();
    success(jumbo(&["build"], &c));
    let read = || {
        let out = Command::new(f.0.join(".venv/bin/python"))
            .args(["-c", "from consumer import VALUE; print(VALUE)"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    assert_eq!(read(), "12");
    fs::write(a.join("src/public_base/__init__.py"), "VALUE = 30\n").unwrap();
    success(jumbo(&["build"], &c));
    assert_eq!(read(), "32");
    for (p, bytes) in [&a, &b, &c].into_iter().zip(manifests) {
        assert_eq!(fs::read(p.join("pyproject.toml")).unwrap(), bytes);
    }
    let out = jumbo(
        &[
            "fingerprint",
            "--lock",
            f.0.join("uv.lock").to_str().unwrap(),
        ],
        &c,
    );
    assert!(!out.status.success()); // workspace roots have no own immutable commit
}

#[test]
fn published_lock_and_dirty_source_do_not_reuse_each_other() {
    let f = Fixture::new("provenance");
    let p = f.project("consumer");
    fs::create_dir_all(&p).unwrap();
    fs::write(
        p.join("package.json"),
        "{\"name\":\"consumer\",\"version\":\"1.0.0\"}\n",
    )
    .unwrap();
    fs::write(p.join("source.js"), "source\n").unwrap();
    git(&p);
    fs::write(p.join("package-lock.json"),r#"{"name":"consumer","lockfileVersion":3,"packages":{"":{"name":"consumer","version":"1.0.0"},"node_modules/base":{"name":"base","version":"1.0.0","resolved":"file:../base"}}}"#).unwrap();
    for args in [
        &["fingerprint", "--lock", "package-lock.json"][..],
        &["fingerprint", "--promote", "--lock", "package-lock.json"],
    ] {
        let out = jumbo(args, &p);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("local workspace inputs"));
    }
    fs::write(p.join("package-lock.json"),r#"{"name":"consumer","lockfileVersion":3,"packages":{"":{"name":"consumer","version":"1.0.0"}}}"#).unwrap();
    success(jumbo(
        &["fingerprint", "--promote", "--lock", "package-lock.json"],
        &p,
    ));
    let report: serde_json::Value = serde_json::from_str(&success(jumbo(
        &["fingerprint", "--lock", "package-lock.json"],
        &p,
    )))
    .unwrap();
    let index = f.0.join("index");
    fs::create_dir_all(&index).unwrap();
    fs::write(index.join("consumer.jsonl"),serde_json::to_string(&serde_json::json!({"package":"consumer","major":1,"version":"1.0.0","commit":report["commit"],"fingerprint":report["fingerprint"],"timestamp":"2026-10-06T00:00:00Z"})).unwrap()+"\n").unwrap();
    fs::write(p.join("source.js"), "dirty\n").unwrap();
    let out = jumbo(
        &[
            "dedup",
            "--lock",
            "package-lock.json",
            "--package",
            "consumer",
            "--index",
            index.to_str().unwrap(),
        ],
        &p,
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("dirty local inputs"));
}

#[test]
fn python_absent_checkout_retains_the_registered_remote_fallback() {
    if !available("uv") {
        assert!(std::env::var_os("JUMBO_REQUIRE_PYTHON_TOOLCHAIN").is_none());
        return;
    }
    let f = Fixture::new("python-remote");
    let a = f.project("producer");
    let c = f.project("consumer");
    python(&a, "remote-base", "", "VALUE = 47\n");
    python(
        &c,
        "consumer",
        "\"remote-base>=1,<2\"",
        "from remote_base import VALUE\n",
    );
    git(&a);
    git(&c);
    let remote = f.0.join("remote-base");
    assert!(Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            &format!("file://{}", remote.display())
        ])
        .current_dir(&a)
        .status()
        .unwrap()
        .success());
    f.initialize();
    fs::rename(&a, &remote).unwrap();
    success(jumbo(&["workspace", "sync"], &f.0));
    success(jumbo(&["build"], &c));
    let out = Command::new(f.0.join(".venv/bin/python"))
        .args(["-c", "from consumer import VALUE; print(VALUE)"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "47");
}

#[test]
fn python_normalized_ambiguity_and_version_mismatch_fail_before_uv() {
    for (tag, name, version, message) in [
        (
            "python-duplicate",
            "Public_Base",
            "1.0.0",
            "ambiguous local package",
        ),
        (
            "python-incompatible",
            "different",
            "2.0.0",
            "incompatible local package",
        ),
    ] {
        let f = Fixture::new(tag);
        let a = f.project("a");
        let b = f.project("b");
        python(&a, "public-base", "\"different>=1,<2\"", "VALUE = 1\n");
        python(&b, name, "", "VALUE = 2\n");
        let content = fs::read_to_string(b.join("pyproject.toml"))
            .unwrap()
            .replace("1.2.0", version);
        fs::write(b.join("pyproject.toml"), content).unwrap();
        f.initialize();
        let out = jumbo(&["build"], &a);
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!f.0.join(".venv").exists());
    }
}

#[test]
fn local_provenance_survives_inject_only_and_clears_after_a_real_index_lock() {
    if !available("npm") {
        return;
    }
    let f = Fixture::new("marker");
    let p = f.project("consumer");
    fs::create_dir_all(p.join("deps")).unwrap();
    fs::write(
        p.join("package.json"),
        "{\"name\":\"consumer\",\"version\":\"1.0.0\"}\n",
    )
    .unwrap();
    git(&p);
    // Even an old lock whose path happens to look like an index overlay
    // must remain ineligible when developer provenance is standing.
    fs::write(p.join("package-lock.json"),r#"{"name":"consumer","lockfileVersion":3,"packages":{"":{"name":"consumer","version":"1.0.0"},"deps/base":{"name":"base","version":"1.0.0"}}}"#).unwrap();
    let marker = p.join("deps/.jumbo-workspace-inputs.json");
    fs::write(&marker, "[]\n").unwrap();
    let index = f.0.join("index");
    fs::create_dir_all(&index).unwrap();
    fs::write(index.join("base.jsonl"),"{\"package\":\"base\",\"major\":1,\"version\":\"1.0.0\",\"commit\":\"0123456789abcdef0123456789abcdef01234567\",\"timestamp\":\"2026-10-06T00:00:00Z\"}\n").unwrap();
    success(jumbo(
        &["lock", "--inject-only", "--index", index.to_str().unwrap()],
        &p,
    ));
    assert!(marker.exists());
    let out = jumbo(&["fingerprint", "--lock", "package-lock.json"], &p);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("local workspace inputs"));
    success(jumbo(&["lock", "--index", index.to_str().unwrap()], &p));
    assert!(!marker.exists());
    success(jumbo(
        &["fingerprint", "--promote", "--lock", "package-lock.json"],
        &p,
    ));
}
