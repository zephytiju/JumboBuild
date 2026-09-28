#!/usr/bin/env bash
# Materialize the recorded artifacts of the manifest's internal dependencies
# on the jumbo-publish build path (Jumbo Build & Versioning Standard, §3.4
# "Artifact Storage and Materialization"; engine side: src/dedup/ingest.rs).
#
# The executor's build (miss) path runs this after `jumbo promote` and
# BEFORE any build tool (uv version / uv build / npm version / npm pack):
# every internal dependency's resolution-only stub under deps/<slug>/ —
# written by `jumbo lock` to resolve and fingerprint, never buildable — is
# replaced with the dependency's recorded release artifact, downloaded by
# exact URL and SHA-256 verified, or (only when the recorded artifact
# cannot be used: a definitive 404/410, no artifactUrl, no artifactSha256)
# with the dependency's real repository source at the recorded commit. The
# manifest reference is repointed at the materialized file, so a real wheel
# sits at deps/<slug>/*.whl before uv/npm ever run — on any uv/npm tool
# version. Without this step the build fails exactly the way the first
# dependent-repo onboarding did: uv invokes the build backend on the stub
# and hatchling aborts with "Unable to determine which files to ship".
#
# Usage (from the caller repository root, with jumbo on PATH):
#
#   jumbo_publish_materialize_deps.sh <python|npm>
#
# Environment:
#
#   JUMBO_OUT      required; directory for the dedup JSON (the executor
#                  sets it to the run's jumbo state directory)
#   GITHUB_OUTPUT  optional; receives materialized=<count>
#   JUMBO_ARTIFACT_DIR / JUMBO_REPO_MAP  passed through to jumbo (the CLI
#                  artifact cache for offline runs / the source-fallback
#                  repo map); no credential is read or stored here —
#                  artifact downloads go through jumbo's validated
#                  github.com-only layer.
#
# Failures abort the step before the build: a materialized entry that does
# not exist on disk in the mode it declares, or a missing materialization
# marker, is a hard error — stubs must never reach a build silently.

set -euo pipefail

ecosystem="${1:?usage: jumbo_publish_materialize_deps.sh <python|npm>}"
case "$ecosystem" in
  python) manifest="pyproject.toml" ;;
  npm) manifest="package.json" ;;
  *)
    echo "::error title=jumbo deps::unsupported ecosystem: ${ecosystem}" >&2
    exit 1
    ;;
esac
out_dir="${JUMBO_OUT:?JUMBO_OUT must point at the jumbo state directory of this run}"
if [[ ! -f "$manifest" ]]; then
  echo "::error title=jumbo deps::no ${manifest} in $(pwd)" >&2
  exit 1
fi

# --materialize is inert on this path (here the dedup decision is a miss —
# the reuse path pulls its own recorded artifact in its own step). The
# command is exactly the ingestion invocation the engine documents: wheels
# land at deps/<slug>/*.whl and [tool.uv.sources] (or the npm file: value)
# is repointed at them.
jumbo dedup --materialize --deps --manifest "$manifest" | sed -n '/^{/,$p' > "$out_dir/deps-materialized.json"
cat "$out_dir/deps-materialized.json"

count="$(jq -r '.dependencies.materialized | length' "$out_dir/deps-materialized.json")"

# Pre-build guard: what the build is about to consume must never be a stub
# again. Every materialized entry must exist on disk in the mode it
# declares (a verified artifact file, or the fetched source directory).
jq -r '.dependencies.materialized[] | [.mode, .path] | @tsv' "$out_dir/deps-materialized.json" |
  while IFS=$'\t' read -r mode path; do
    case "$mode" in
      artifact)
        if [[ ! -f "$path" ]]; then
          echo "::error title=jumbo deps::the materialized artifact is missing: ${path}" >&2
          exit 1
        fi
        ;;
      source)
        if [[ ! -d "$path" ]]; then
          echo "::error title=jumbo deps::the materialized source is missing: ${path}" >&2
          exit 1
        fi
        ;;
      *)
        echo "::error title=jumbo deps::unknown materialization mode: ${mode}" >&2
        exit 1
        ;;
    esac
  done

if [[ "$count" -gt 0 ]]; then
  if [[ ! -f deps/.jumbo-artifacts.json ]]; then
    echo "::error title=jumbo deps::the materialization marker deps/.jumbo-artifacts.json is missing" >&2
    exit 1
  fi
  ls -l deps/*/
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    echo "materialized=${count}" >> "$GITHUB_OUTPUT"
  fi
  echo "::notice title=jumbo deps::${count} internal dependency artifact(s) materialized under deps/ before the build"
else
  echo "no internal dependencies to materialize (nothing under deps/ is build input)"
fi
