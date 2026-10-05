//! Offline contract checks over the reusable `jumbo-verify` workflow —
//! no network, no credentials, no Actions run required.
//!
//! The pinned contracts (the verification half of the member forwarder;
//! see docs/jumbo-verify.md and tests/jumbo_publish_workflow.rs for the
//! publication half):
//!
//! 1. **Verification is read-only.** No publish/index machinery may appear
//!    in the file: no `jumbo fingerprint` / `jumbo dedup` / `jumbo
//!    promote`, no `gh release`, no index append, no registry publication,
//!    and the workflow's own permissions are `contents: read`.
//!
//! 2. **The dependency-materialization credential.** The materialize step
//!    must export a `GH_TOKEN` that can read PRIVATE member repositories
//!    (the org CI app's org-wide read-only installation token, minted by
//!    its own step, preferred, falling back to the static artifact
//!    token) — the same defect class the publish workflow's test pins:
//!    anonymous dependency fetches answer 404 for private repositories.
//!
//! 3. **The tolerant private-key reader.** The org CI App private key is
//!    handed VERBATIM to the pinned `actions/create-github-app-token`
//!    action (which accepts raw PEM or base64-encoded PEM). No local
//!    openssl/base64 re-parsing of the secret may appear anywhere in the
//!    file — a hand-rolled decoder rejected the org secret the jumbo
//!    executors accept ("not valid PEM or base64-encoded PEM",
//!    JuntaiFuseAPI's removed decode step).
//!
//! 4. **The npm pipeline order.** Lock refresh → clean install → restore
//!    the clean dependency declarations → the project's scripts, with the
//!    test step guarding npm's generated placeholder (`echo "Error: no
//!    test specified" && exit 1`), exactly the member pattern the
//!    migrated repositories run (and the same front the jumbo-publish
//!    build path runs).
//!
//! 5. **The python pipeline order.** `uv sync --all-extras` → `uv build`
//!    → pytest gated on pytest actually being installed — mirroring
//!    jumbo's python test pipeline (`uv sync` → `uv build` → pytest).
//!
//! In every case the token enters only through the environment: never a
//! literal, never a log line, never a command line.

/// The steps whose environments or contents carry pinned contracts.
const MINT_STEP: &str = "Mint the org-wide dependency read token";
const MATERIALIZE_STEP: &str = "Materialize the recorded dependency artifacts";
const NPM_VERIFY_STEP: &str = "Verify (npm)";
const PYTHON_VERIFY_STEP: &str = "Verify (python)";

/// Extract one step's YAML block from the workflow text: from its
/// `- name:` line to the next sibling step (`- name:` at the same
/// indentation) or job key. (Same extractor as the publish workflow's
/// contract test.)
fn step_block(workflow: &str, step_name: &str) -> Option<String> {
    let needle = format!("- name: {step_name}");
    let lines: Vec<&str> = workflow.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.trim() == needle || line.trim_end().ends_with(&needle))
        .filter(|&idx| lines[idx].trim_start().starts_with("- name:"))?;
    let indent = lines[start].len() - lines[start].trim_start().len();
    let mut block = Vec::new();
    for line in &lines[start..] {
        if !block.is_empty() {
            let trimmed = line.trim_end();
            if trimmed.len() >= indent
                && trimmed.starts_with(&" ".repeat(indent))
                && !trimmed[indent..].starts_with(' ')
            {
                break; // the next sibling step or job key
            }
            if !trimmed.is_empty() && trimmed.len() < indent {
                break;
            }
        }
        block.push(*line);
    }
    Some(block.join("\n"))
}

/// No credential literal may appear anywhere in the workflow file.
#[test]
fn no_credential_literal_appears_in_the_workflow() {
    let workflow = include_str!("../.github/workflows/jumbo-verify.yml");
    for literal in ["ghp_", "github_pat_", "ghs_", "gho_"] {
        assert!(
            !workflow.to_lowercase().contains(literal),
            "credential literal `{literal}` must never appear in jumbo-verify.yml"
        );
    }
}

/// Verification never publishes, appends, or promotes.
#[test]
fn the_workflow_carries_no_publication_machinery() {
    let workflow = include_str!("../.github/workflows/jumbo-verify.yml");
    for forbidden in [
        "jumbo fingerprint",
        "jumbo dedup",
        "jumbo promote",
        "gh release create",
        "jumbo_index_append.py",
        "uv publish",
        "npm publish",
        "JUNTAI_INDEX_TOKEN",
    ] {
        assert!(
            !workflow.contains(forbidden),
            "`{forbidden}` must never appear in jumbo-verify.yml — verification is \
             read-only; publication belongs to jumbo-publish"
        );
    }
    // The workflow's own grant is read-only; reusable workflows cannot
    // elevate, so this is the ceiling for every verify call.
    assert!(
        workflow.contains("permissions:\n  contents: read"),
        "jumbo-verify must declare exactly contents: read"
    );
    // workflow_call-only: never dispatchable directly (JumboBuild is the
    // toolchain, not a member).
    assert!(
        !workflow.contains("workflow_dispatch:"),
        "jumbo-verify is workflow_call-only"
    );
}

/// The mint step is the tolerant reader: the org CI App private key is
/// passed verbatim to the pinned action — no local openssl/base64
/// re-parsing anywhere in the file (the JuntaiFuseAPI defect).
#[test]
fn the_private_key_flows_verbatim_into_the_pinned_minting_action() {
    let workflow = include_str!("../.github/workflows/jumbo-verify.yml");
    for forbidden in ["openssl pkey", "base64 --decode", "::add-mask::", "APP_PRIVATE_KEY:"] {
        assert!(
            !workflow.contains(forbidden),
            "`{forbidden}` must never appear in jumbo-verify.yml — a hand-rolled \
             secret decoder is exactly the defect class this workflow retires"
        );
    }
    let block = step_block(workflow, MINT_STEP)
        .unwrap_or_else(|| panic!("step `{MINT_STEP}` not found in jumbo-verify.yml"));
    assert!(
        block.contains("actions/create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1"),
        "the minting action must be the same full-SHA pin jumbo-publish uses:\n{block}"
    );
    assert!(
        block.contains("private-key: ${{ secrets.JUNTAI_CI_APP_PRIVATE_KEY }}"),
        "the private key flows verbatim from the secret into the action (the action \
         accepts raw or base64-encoded PEM):\n{block}"
    );
    // Read only — verification never needs write.
    assert!(
        block.contains("permission-contents: read"),
        "the minted token is downgraded to contents: read:\n{block}"
    );
    // And deliberately NOT repository-scoped: dependency materialization
    // reads arbitrary private member repositories of the organization,
    // which cannot be pre-listed (the anonymous-404 defect).
    assert!(
        !block.contains("repositories:"),
        "the token mint must stay org-wide (no repositories scoping):\n{block}"
    );
}

/// The materialize step exports the authenticated fetch credential.
#[test]
fn the_materialize_step_exports_the_dependency_read_token() {
    let workflow = include_str!("../.github/workflows/jumbo-verify.yml");
    let block = step_block(workflow, MATERIALIZE_STEP)
        .unwrap_or_else(|| panic!("step `{MATERIALIZE_STEP}` not found in jumbo-verify.yml"));
    assert!(
        block.contains("env:"),
        "the materialize step must carry an env block:\n{block}"
    );
    assert!(
        block.contains(
            "GH_TOKEN: ${{ steps.deps-token.outputs.token || secrets.JUNTAI_GITHUB_ARTIFACT_TOKEN }}"
        ),
        "the materialize step must export GH_TOKEN from the org-wide dependency \
         read token, falling back to the static artifact token:\n{block}"
    );
    // The identical materialization script the publish build path runs —
    // verification consumes exactly what a release build would.
    assert!(
        block.contains("jumbo_publish_materialize_deps.sh"),
        "the materialize step runs the publish path's script:\n{block}"
    );
}

/// The npm verify pipeline order and the placeholder-guarded test step.
#[test]
fn the_npm_pipeline_refreshes_installs_restores_then_verifies() {
    let workflow = include_str!("../.github/workflows/jumbo-verify.yml");
    let block = step_block(workflow, NPM_VERIFY_STEP)
        .unwrap_or_else(|| panic!("step `{NPM_VERIFY_STEP}` not found in jumbo-verify.yml"));
    let lock_refresh = block
        .find("npm install --package-lock-only --ignore-scripts")
        .unwrap_or_else(|| panic!("the npm pipeline must refresh the lock:\n{block}"));
    let install = block
        .find("npm ci")
        .unwrap_or_else(|| panic!("the npm pipeline must clean-install:\n{block}"));
    let restore = block
        .find("deps/.jumbo-sources.json")
        .unwrap_or_else(|| panic!("the npm pipeline must restore the clean declarations:\n{block}"));
    let verify = block
        .find("npm run verify --if-present")
        .unwrap_or_else(|| panic!("the npm pipeline must run the project's verify script:\n{block}"));
    let build = block
        .find("npm run build --if-present")
        .unwrap_or_else(|| panic!("the npm pipeline must run the build script:\n{block}"));
    assert!(
        lock_refresh < install && install < restore && restore < verify && verify < build,
        "the npm pipeline must run lock refresh → clean install → restore clean \
         declarations → project scripts:\n{block}"
    );
    // npm's generated placeholder ("Error: no test specified") must count
    // as absent — running it would fail every scriptless member.
    assert!(
        block.contains("\"no test specified\"") && block.contains("node --test"),
        "the test step guards npm's placeholder and falls back to the Node test \
         runner:\n{block}"
    );
}

/// The python verify pipeline: sync (all extras) → restore the clean
/// declarations → build → gated pytest.
#[test]
fn the_python_pipeline_syncs_restores_builds_then_tests() {
    let workflow = include_str!("../.github/workflows/jumbo-verify.yml");
    let block = step_block(workflow, PYTHON_VERIFY_STEP)
        .unwrap_or_else(|| panic!("step `{PYTHON_VERIFY_STEP}` not found in jumbo-verify.yml"));
    let sync = block
        .find("uv sync --all-extras")
        .unwrap_or_else(|| panic!("the python pipeline must sync every extra:\n{block}"));
    let restore = block
        .find(".jumbo-sources.json")
        .unwrap_or_else(|| panic!("the python pipeline must restore the clean declarations from the injection marker:\n{block}"));
    let build = block
        .find("uv build")
        .unwrap_or_else(|| panic!("the python pipeline must run the standard build:\n{block}"));
    let pytest = block
        .find("pytest")
        .unwrap_or_else(|| panic!("the python pipeline must run pytest:\n{block}"));
    assert!(
        sync < restore && restore < build && build < pytest,
        "the python pipeline must run uv sync → restore clean declarations → uv build → \
         pytest (jumbo's python test pipeline order, with the same restore the npm \
         branch applies — the manifest's rewritten exact pins must never reach anything \
         that packs or asserts it):\n{block}"
    );
    // The restore maps the marker's rewritten strings back to the declared
    // ones (never a blind delete — [tool.uv.sources] must survive for the
    // synced environment to keep resolving the materialized wheels).
    assert!(
        block.contains("source[\"rewritten\"]") && block.contains("source[\"declared\"]"),
        "the restore uses the injection marker's rewritten→declared mapping:\n{block}"
    );
    // `python -m pytest` (not bare pytest): the module form puts the
    // project root on sys.path, which member suites that import through
    // the tests package rely on (MeridianS3Adapter run 37266649309 —
    // "No module named 'tests'" under bare pytest).
    assert!(
        block.contains("python -m pytest"),
        "pytest runs in module form (the project root joins sys.path):\n{block}"
    );
    assert!(
        block.contains("import pytest"),
        "pytest is gated on being installed in the synced environment (build-only \
         members skip it with a notice, never fail):\n{block}"
    );
}

/// The token exports are scoped to exactly the steps that need them.
#[test]
fn no_other_step_gains_a_credential_through_this_contract() {
    let workflow = include_str!("../.github/workflows/jumbo-verify.yml");
    let mut current: Option<&str> = None;
    for line in workflow.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("- name: ") {
            current = rest.trim().split('\n').next();
        }
        if trimmed.starts_with("GH_TOKEN:") && current != Some(MATERIALIZE_STEP) {
            let owner = current.unwrap_or("<unknown step>");
            let allowed = [
                "Fetch the Jumbo index", // minted installation token / static artifact token
            ];
            assert!(
                allowed.contains(&owner),
                "unexpected GH_TOKEN export in step `{owner}` — scope credential exports to \
                 the steps whose contracts name them"
            );
        }
    }
}
