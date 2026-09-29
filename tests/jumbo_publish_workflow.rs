//! Offline contract checks over the reusable `jumbo-publish` workflow —
//! no network, no credentials, no Actions run required.
//!
//! The first check pins the dedup re-pull contract this executor carries:
//! the "Pull the recorded artifact" step (the fingerprint-hit re-pull)
//! must export the caller's own `GH_TOKEN`, because on a PRIVATE
//! repository the recorded `github.com/<o>/<r>/releases/download/...` URL
//! answers 404 even with an Authorization header — private release assets
//! are served only through the authenticated api.github.com asset route,
//! which jumbo's fetch layer (`src/dedup/fetch.rs`) walks when the direct
//! URL 404s and a token is present. Without the export, every re-pull on
//! a private member repository fails exactly the way the promotion
//! validation observed (HTTP 404 on the recorded URL; the public control
//! 302s fine). The token enters only through the environment: never a
//! literal, never a log line, never a command line.

/// The step whose environment the re-pull contract lives in.
const REPULL_STEP: &str = "Pull the recorded artifact (fingerprint hit - build skipped)";

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
    // The export is context-expression only: no credential literal may
    // ever appear in the step.
    assert!(
        !block.to_lowercase().contains("ghp_") && !block.to_lowercase().contains("github_pat_"),
        "no credential literal may appear in the re-pull step"
    );
}

#[test]
fn no_other_step_gains_a_credential_through_this_contract() {
    // The re-pull token export is scoped to exactly one step. Every other
    // `env:` block in the workflow must not export GH_TOKEN (the other
    // steps authenticate through their own named secrets, checked by
    // review and by the workflows' own CI).
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
            ];
            assert!(
                allowed.contains(&owner),
                "unexpected GH_TOKEN export in step `{owner}` — scope the re-pull \
                 credential to the re-pull step only"
            );
        }
    }
}
