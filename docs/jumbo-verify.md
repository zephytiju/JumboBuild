# jumbo-verify: verifying a member repository through GitHub Actions

Member verification runs the same jumbo pipeline the publication executor
runs — resolve the internal dependencies from the JumboIndex (`jumbo
lock`), materialize the recorded dependency artifacts (or source
fallbacks) under `deps/` with the manifest repointed at them, then build
and test — minus every publication-side step: **no fingerprint, no dedup,
no promote, no GitHub Release, no index append, no registry publication**
(Jumbo Build & Versioning Standard, §3.5 "Executors and the Publication
Contract"). A member repository therefore carries **no per-repo verify
workflow logic**: its jumbo forwarder triggers this reusable workflow on
`pull_request` (and on `push` to the default branch) alongside the
dispatch-only publication call, and nothing else.

- Workflow: [`.github/workflows/jumbo-verify.yml`](../.github/workflows/jumbo-verify.yml)
- Member forwarder template: [`templates/member-jumbo-publish.yml`](../templates/member-jumbo-publish.yml)
  (one file, two triggers: `pull_request`/`push` → jumbo-verify,
  `workflow_dispatch` → jumbo-publish)
- Publication counterpart: [docs/jumbo-publish.md](./jumbo-publish.md)
- Member onboarding walkthrough: [docs/member-onboarding.md](./member-onboarding.md)

## The caller

The member forwarder is the single workflow file a member repository
carries:

```yaml
name: jumbo

on:
  pull_request:
  push:
    branches: [main]
  workflow_dispatch:
    inputs:
      commit:
        description: "Commit SHA of this repository to release (default: the triggering commit)"
        type: string
        default: ""

jobs:
  # PR-mode: verify every pull request (and main) through jumbo.
  verify:
    if: github.event_name != 'workflow_dispatch'
    uses: zephytiju/JumboBuild/.github/workflows/jumbo-verify.yml@<ref>
    secrets: inherit
    with:
      jumbobuild-ref: <ref>
    permissions:
      contents: read

  # Dispatch-mode: release on demand (unchanged publication path).
  jumbo-publish:
    if: github.event_name == 'workflow_dispatch'
    uses: zephytiju/JumboBuild/.github/workflows/jumbo-publish.yml@<ref>
    secrets: inherit
    with:
      jumbobuild-ref: <ref>
      commit: ${{ inputs.commit }}
    permissions:
      contents: write
      packages: write
```

`<ref>` is the JumboBuild ref to verify/release with — a tag or a full
commit SHA, stated identically on the `uses:` line and the
`jumbobuild-ref` input (the same dual-pin contract
[docs/jumbo-publish.md](./jumbo-publish.md) documents: under
`workflow_call` GitHub does not expose the `uses:` ref to the called
workflow, so the input is the authoritative pin).

Inputs:

| input | default | meaning |
| --- | --- | --- |
| `commit` | PR head (else triggering commit) | exact caller commit to verify |
| `jumbobuild-ref` | *(required under `workflow_call`)* | JumboBuild ref to build the `jumbo` binary from |
| `ecosystem` | `auto` | `python` \| `npm` \| `auto` (detected: `pyproject.toml` → python, `package.json` → npm) |
| `manifest` | *(empty)* | forwarded to `jumbo lock --manifest`; for mixed npm+python repositories whose python side is not jumbo-lockable yet, pass `package.json` to verify the npm side (implies `ecosystem: npm`) |
| `index-repository` | `zephytiju/JumboIndex` | the Jumbo index repository |

## Secrets

`secrets: inherit` from the organization. The workflow resolves
authentication in this order (presence checks only; verification is
read-only and never reads `JUNTAI_INDEX_TOKEN`):

1. **Preferred** — the org CI GitHub App pair `JUNTAI_CI_APP_ID` +
   `JUNTAI_CI_APP_PRIVATE_KEY`: one short-lived **org-wide**
   `contents: read` installation token is minted for the JumboBuild
   checkout, the JumboIndex fetch, and dependency materialization
   (materialization fetches recorded release assets and source-fallback
   trees from arbitrary PRIVATE member repositories, so its credential
   cannot be pre-scoped). The minting action accepts the private key as
   raw PEM or base64-encoded PEM; the key material never leaves the
   action.
2. **Fallback** — the static secret `JUNTAI_GITHUB_ARTIFACT_TOKEN` (read
   access to zephytiju/JumboBuild and the index repository).
3. **Neither** — the run fails fast with an actionable error naming both
   options, before any build work.

## The pipeline

Per-event behavior:

- `pull_request` / `push` (the forwarder's verify job): check out the
  caller at the pull request head (push: the pushed commit), build `jumbo`
  from the pinned JumboBuild ref, fetch the private index, `jumbo lock`
  (internal dependencies resolve to their recorded JumboIndex builds;
  third-party from the public registries), materialize the recorded
  dependency artifacts under `deps/`, then run the ecosystem pipeline:

  - **npm**: `npm install --package-lock-only --ignore-scripts` →
    `npm ci` → restore the clean dependency declarations
    (`deps/.jumbo-sources.json` maps every jumbo-rewritten declaration
    back to the developer's declared range) → the project's
    `verify`/`typecheck` scripts when configured → the test step
    (`npm test` when a real script is configured — npm's generated
    placeholder counts as absent — else `node --test` when Node test
    files exist) → the `build` script when configured. This is the same
    front the jumbo-publish npm build path runs.
  - **python**: `uv sync --all-extras` (the materialized file: wheels
    resolve through `[tool.uv.sources]`; every declared extra installs so
    test dependencies are present) → `uv build` → `uv run pytest` when
    pytest is installed in the synced environment. This mirrors jumbo's
    python test pipeline (`uv sync` → `uv build` → pytest).

- `workflow_dispatch` (the forwarder's publish job): the unchanged
  [jumbo-publish](./jumbo-publish.md) publication path.

Repository-specific checks that used to live as steps in a standalone
verify workflow (lock-presence gates, secret scans, boundary checks)
belong in the repository's own test surface — an npm `verify` script or a
pytest test — so they run inside this pipeline and the repository still
carries exactly one workflow file.

## Reproducibility boundary

Byte-for-byte artifact reproducibility is `jumbo build --pinned`'s job
(the `jumbo reproduce` path), not CI's: verification proves the closure
resolves, builds, and passes tests at the pull request head. Release
evidence integrity (a released artifact matching its own recorded digest)
is enforced by the index records and the release manifests, never by
re-deriving bytes in a verify run.

## Direct runs

This workflow is `workflow_call`-only: it never runs directly on the
JumboBuild repository (JumboBuild is the toolchain, not a member).
