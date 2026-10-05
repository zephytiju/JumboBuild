//! Offline contract checks over the reusable `jumbo-publish` workflow —
//! no network, no credentials, no Actions run required.
//!
//! Three pinned contracts live here:
//!
//! 1. **The dedup re-pull credential.** The "Pull the recorded artifact"
//!    step (the fingerprint-hit re-pull) must export the caller's own
//!    `GH_TOKEN`, because on a PRIVATE repository the recorded
//!    `github.com/<o>/<r>/releases/download/...` URL answers 404 even
//!    with an Authorization header — private release assets are served
//!    only through the authenticated api.github.com asset route, which
//!    jumbo's fetch layer (`src/dedup/fetch.rs`) walks when the direct
//!    URL 404s and a token is present.
//!
//! 2. **The dependency-materialization credential.** The "Materialize the
//!    recorded dependency artifacts" step must export a `GH_TOKEN` that
//!    can read PRIVATE member repositories (the org CI app's org-wide
//!    read-only installation token, minted by its own step, preferred) —
//!    without it the dependency fetches run anonymous and both the
//!    recorded asset URLs and the codeload source-fallback URLs answer
//!    404 for private repositories even though the run holds valid
//!    credentials (evidence: PrismPipelineFluxboardMicroUI run
//!    36819957831 — anonymous codeload 404 for private
//!    zephytiju/PrismReact).
//!
//! 3. **The npm build-before-pack contract.** The npm branch of the
//!    "Build the artifacts" step must run the package's standard build
//!    (lock refresh → clean install → build script, the node.rs
//!    pipeline) BEFORE `npm pack`, so the packed tarball carries the
//!    built dist — packing the bare tree released source-only stubs
//!    (proof: @zephytiju/software-development-cicd-interfaces@1.0.0 =
//!    package.json + README only).
//!
//! 4. **The python clean-manifest restore contract.** The python
//!    branch of the "Build the artifacts" step must restore the
//!    developer's declared pyproject.toml (snapshotted before `jumbo
//!    lock` rewrote it) BEFORE `uv version --frozen` stamps the
//!    pipeline version and `uv build` packs the wheel — the exact pins
//!    jumbo lock writes for materialization must never reach the
//!    released wheel's Requires-Dist, where an exact internal pin can
//!    name an index-only version no consumer can resolve (proof:
//!    lattice-runtime-core 0.2.0 pinning
//!    juntai-documentation-capability==2.0.2). The version stamp is
//!    `--frozen`: a plain `uv version` refreshes uv.lock against the
//!    restored declarations, whose index-only internal ranges no
//!    public registry serves (proof: LatticeRuntimeCore publish run
//!    37366690541 — `No solution found` at JUMBO_VERSION=0.3.0). The
//!    full-file restore is the python twin of the npm branch's
//!    marker-based restore and the identical mechanism jumbo-verify
//!    runs after its `uv sync`.
//!
//! In every case the token enters only through the environment: never a
//! literal, never a log line, never a command line.

/// The steps whose environments carry pinned credentials.
const REPULL_STEP: &str = "Pull the recorded artifact (fingerprint hit - build skipped)";
const MATERIALIZE_STEP: &str = "Materialize the recorded dependency artifacts (build path)";
const DEPS_TOKEN_MINT_STEP: &str = "Mint the org-wide dependency read token";
const BUILD_STEP: &str = "Build the artifacts at the jumbo-computed version";
const SNAPSHOT_STEP: &str = "Snapshot the declared python manifest";

/// Extract one step's YAML block from the workflow text: from its
/// `- name:` line to the next sibling step (`- name:` at the same
/// indentation) or job key.
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
    let workflow = include_str!("../.github/workflows/jumbo-publish.yml");
    for literal in ["ghp_", "github_pat_", "ghs_", "gho_"] {
        assert!(
            !workflow.to_lowercase().contains(literal),
            "credential literal `{literal}` must never appear in jumbo-publish.yml"
        );
    }
}

#[test]
fn the_repull_step_exports_the_caller_github_token() {
    let workflow = include_str!("../.github/workflows/jumbo-publish.yml");
    let block = step_block(workflow, REPULL_STEP)
        .unwrap_or_else(|| panic!("step `{REPULL_STEP}` not found in jumbo-publish.yml"));
    assert!(
        block.contains("env:"),
        "the re-pull step must carry an env block:\n{block}"
    );
    assert!(
        block.contains("GH_TOKEN: ${{ github.token }}"),
        "the re-pull step must export GH_TOKEN from the caller's github.token \
         (private release assets resolve only via the authenticated api.github.com \
         asset route):\n{block}"
    );
}

#[test]
fn the_materialize_step_exports_the_dependency_read_token() {
    let workflow = include_str!("../.github/workflows/jumbo-publish.yml");
    let block = step_block(workflow, MATERIALIZE_STEP)
        .unwrap_or_else(|| panic!("step `{MATERIALIZE_STEP}` not found in jumbo-publish.yml"));
    // The credential chain: the org CI app's org-wide read token (the
    // mint step below), then the static artifact token, then the caller's
    // own GITHUB_TOKEN (public dependencies only). An anonymous
    // dependency fetch is the diagnosed defect — the chain must exist.
    assert!(
        block.contains("env:"),
        "the materialize step must carry an env block:\n{block}"
    );
    assert!(
        block.contains(
            "GH_TOKEN: ${{ steps.deps-token.outputs.token || secrets.JUNTAI_GITHUB_ARTIFACT_TOKEN || github.token }}"
        ),
        "the materialize step must export GH_TOKEN from the org-wide dependency \
         read token, falling back to the static artifact token and then the \
         caller's github.token:\n{block}"
    );
}

#[test]
fn the_dependency_read_token_is_org_wide_and_read_only() {
    let workflow = include_str!("../.github/workflows/jumbo-publish.yml");
    let block = step_block(workflow, DEPS_TOKEN_MINT_STEP)
        .unwrap_or_else(|| panic!("step `{DEPS_TOKEN_MINT_STEP}` not found in jumbo-publish.yml"));
    // App path only, same pinned action as the index/artifact mint.
    assert!(
        block.contains("if: steps.auth.outputs.mode == 'app'"),
        "the dependency token mint follows the resolved auth mode:\n{block}"
    );
    assert!(
        block.contains("actions/create-github-app-token@"),
        "the dependency token is a minted installation token:\n{block}"
    );
    // Read only — dependency fetches never need write.
    assert!(
        block.contains("permission-contents: read"),
        "the dependency token is downgraded to contents: read:\n{block}"
    );
    // And deliberately NOT repository-scoped: dependency materialization
    // reads arbitrary private member repositories of the organization,
    // which cannot be pre-listed. A `repositories:` line here would
    // reintroduce the anonymous-404 defect for every unlisted member.
    assert!(
        !block.contains("repositories:"),
        "the dependency token mint must stay org-wide (no repositories scoping):\n{block}"
    );
}

#[test]
fn the_npm_build_path_builds_before_packing() {
    let workflow = include_str!("../.github/workflows/jumbo-publish.yml");
    let block = step_block(workflow, BUILD_STEP)
        .unwrap_or_else(|| panic!("step `{BUILD_STEP}` not found in jumbo-publish.yml"));
    // The standard build (the node.rs pipeline) must run inside the npm
    // branch, before the pack, in pipeline order.
    let lock_refresh = block
        .find("npm install --package-lock-only --ignore-scripts")
        .unwrap_or_else(|| panic!("the npm branch must refresh the lock:\n{block}"));
    let install = block
        .find("npm ci")
        .unwrap_or_else(|| panic!("the npm branch must clean-install:\n{block}"));
    let build = block
        .find("npm run build")
        .unwrap_or_else(|| panic!("the npm branch must run the build script:\n{block}"));
    let pack = block
        .find("npm pack")
        .unwrap_or_else(|| panic!("the npm branch must pack:\n{block}"));
    assert!(
        lock_refresh < install && install < build && build < pack,
        "the standard build must complete before npm pack (lock refresh → clean \
         install → build script → pack);\n{block}"
    );
    // The build must be conditional on a configured build script (the
    // node.rs rule), not unconditional.
    assert!(
        block.contains("jq -e '.scripts.build' package.json"),
        "the build script runs only when one is configured:\n{block}"
    );
    // The pack lands in a staging directory outside the build output and
    // the tarball is moved into a RESET dist/ — the package's own build
    // usually writes dist/, so packing into it directly would mix build
    // outputs into the release assets (the built bytes travel inside the
    // tarball already).
    assert!(
        block.contains("npm pack --pack-destination \"$pack_dir\"")
            && block.contains("rm -rf dist")
            && block.contains("mv \"$pack_dir\"/*.tgz dist/"),
        "npm packs into a staging directory and dist/ is reset to exactly the \
         artifact:\n{block}"
    );
    // The npm build path also needs Node set up before the step runs.
    let setup = step_block(workflow, "Set up node (npm build path)")
        .unwrap_or_else(|| panic!("the npm build path needs a node setup step"));
    assert!(
        setup.contains("steps.identity.outputs.ecosystem == 'npm'")
            && setup.contains("steps.promote.outputs.publish_required == 'true'"),
        "the node setup step is gated to the npm build path:\n{setup}"
    );
}

/// The python build path restores the declared manifest (full file,
/// snapshotted before the injection) BEFORE `uv version` stamps the
/// pipeline version and `uv build` packs the wheel — the materialized
/// exact pins must never reach the released wheel's Requires-Dist.
#[test]
fn the_python_build_path_restores_the_declared_manifest_before_building() {
    let workflow = include_str!("../.github/workflows/jumbo-publish.yml");
    let block = step_block(workflow, BUILD_STEP)
        .unwrap_or_else(|| panic!("step `{BUILD_STEP}` not found in jumbo-publish.yml"));
    // The restore is the FULL declared file, not a string mapping: the
    // injection re-serializes the manifest and drops comments (SPDX
    // headers), and a string-level restore leaves the exact pins in
    // place whenever the rewritten form differs from the mapping's
    // expectation. (Same rationale as the jumbo-verify contract.)
    let restore = block
        .find("cp \"$RUNNER_TEMP/declared-pyproject.toml\" pyproject.toml")
        .unwrap_or_else(|| panic!("the python branch must restore the declared manifest verbatim:\n{block}"));
    let version = block
        .find("uv version --frozen \"$JUMBO_VERSION\"")
        .unwrap_or_else(|| panic!("the python branch must set the pipeline version without re-locking:\n{block}"));
    let build = block
        .find("uv build --out-dir dist")
        .unwrap_or_else(|| panic!("the python branch must run the standard build:\n{block}"));
    assert!(
        restore < version && version < build,
        "the python branch must run restore → uv version --frozen → uv build (the declared ranges — \
         not the materialized exact pins — reach the wheel metadata, the pipeline-owned \
         version lands on the clean file, and the version stamp never re-locks against the \
         restored declarations):\n{block}"
    );
}

/// The declared python manifest is snapshotted after the checkout and
/// BEFORE `jumbo lock` rewrites it; only the pristine file can be
/// restored on the build path.
#[test]
fn the_declared_python_manifest_is_snapshotted_before_the_lock() {
    let workflow = include_str!("../.github/workflows/jumbo-publish.yml");
    let snapshot = step_block(workflow, SNAPSHOT_STEP)
        .unwrap_or_else(|| panic!("step `{SNAPSHOT_STEP}` not found in jumbo-publish.yml"));
    assert!(
        snapshot.contains("declared-pyproject.toml"),
        "the snapshot step preserves the declared python manifest:\n{snapshot}"
    );
    // Ordering: the snapshot precedes the lock step that rewrites the
    // manifest (a snapshot taken after the injection would preserve the
    // exact pins and the restore would be a no-op).
    let snap_pos = workflow.find(SNAPSHOT_STEP).expect("snapshot step");
    let lock_pos = workflow
        .find("jumbo lock (resolve and generate the language lock)")
        .expect("lock step");
    assert!(
        snap_pos < lock_pos,
        "the snapshot precedes jumbo lock (the injection must never reach the snapshot)"
    );
}


#[test]
fn no_other_step_gains_a_credential_through_this_contract() {
    // The token exports are scoped to exactly the steps that need them.
    // Every other `env:` block in the workflow must not export GH_TOKEN
    // (the other steps authenticate through their own named secrets,
    // checked by review and by the workflows' own CI).
    let workflow = include_str!("../.github/workflows/jumbo-publish.yml");
    let mut current: Option<&str> = None;
    for line in workflow.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("- name: ") {
            current = rest.trim().split('\n').next();
        }
        if trimmed.starts_with("GH_TOKEN:") && current != Some(REPULL_STEP) {
            let owner = current.unwrap_or("<unknown step>");
            let allowed = [
                "Fetch the Jumbo index", // minted installation token / static artifact token
                "Publish the GitHub Release on the caller repository", // github.token for gh release create
                MATERIALIZE_STEP, // org-wide dependency read token / static artifact token / github.token
            ];
            assert!(
                allowed.contains(&owner),
                "unexpected GH_TOKEN export in step `{owner}` — scope credential exports to \
                 the steps whose contracts name them"
            );
        }
    }
}
