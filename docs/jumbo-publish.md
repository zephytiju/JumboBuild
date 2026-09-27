# jumbo-publish: releasing a public repository to jumbo through GitHub Actions

Public repositories release to jumbo through one reusable workflow that lives
in the JumboBuild repository. Jumbo publication is executor-agnostic — the
release steps (`jumbo lock` → `jumbo fingerprint` → `jumbo dedup` →
`jumbo promote`, then build/publish on a bump) are jumbo behaviors defined
once by the [Jumbo Build & Versioning Standard]; the executor only runs the
same jumbo commands with a token. A public repository therefore carries **no
per-repo release workflow logic**: it calls the workflow, inherits the org
secrets, and nothing else.

- Workflow: [`.github/workflows/jumbo-publish.yml`](../.github/workflows/jumbo-publish.yml)
- Member forwarder template: [`templates/member-jumbo-publish.yml`](../templates/member-jumbo-publish.yml)
- Index append implementation: [`scripts/jumbo_index_append.py`](../scripts/jumbo_index_append.py)
  (unit tests: [`scripts/tests/test_jumbo_index_append.py`](../scripts/tests/test_jumbo_index_append.py),
  run in CI by [`index-append.yml`](../.github/workflows/index-append.yml) — including the
  concurrent two-writer race that must converge to exactly one record and one artifact)

## The caller

Add this to a workflow in the public repository (for example
`.github/workflows/jumbo-release.yml` triggered on `push` to the default
branch and on `workflow_dispatch`):

```yaml
jobs:
  jumbo-publish:
    uses: zephytiju/JumboBuild/.github/workflows/jumbo-publish.yml@<ref>
    secrets: inherit
    with:
      jumbobuild-ref: <ref>
```

`<ref>` is the JumboBuild ref to release with — a tag or a full commit SHA.
It appears twice on purpose: the `uses:` line picks the workflow (release
logic) version, and `jumbobuild-ref` pins the JumboBuild ref the workflow
builds the `jumbo` binary from. Keep the two identical (the forwarder
template carries a single `<ref>` placeholder used in both places), so a
pinned `<ref>` also pins the release toolchain.

**Why the explicit input**: under `workflow_call`, GitHub exposes the
called workflow only the caller's context — `GITHUB_WORKFLOW_REF` names the
*caller's* workflow file and ref, not this workflow's. The called workflow
cannot read the ref its `uses:` line was pinned at without OIDC token
introspection (`job_workflow_ref` / `job_workflow_sha` claims), which would
force every caller to grant `id-token: write` and the workflow to fetch and
parse a runtime OIDC token. The explicit input is the cleaner contract: no
new caller permissions, no token plumbing, and the resolution is
deterministically testable. The tradeoff is that the pin is stated twice;
if the two ever disagree, the release logic runs at the `uses:` ref while
jumbo builds from `jumbobuild-ref`. Callers that omit `jumbobuild-ref` fail
fast at coordinate resolution with an error naming the input (the workflow
never silently trusts caller context), and direct runs on the JumboBuild
repository itself (`workflow_dispatch`) still fall back to the ref the
workflow was dispatched at.

Reusable workflows cannot elevate permissions, so the calling job must also
grant the release write it needs (GitHub Releases on the caller repository):

```yaml
    permissions:
      contents: write
```

A complete caller workflow is committed as the **member forwarder template**
at [`templates/member-jumbo-publish.yml`](../templates/member-jumbo-publish.yml).
It lives outside JumboBuild's own `.github/workflows/` so GitHub Actions never
picks it up in this repository. To adopt it:

1. Copy the template into your repository as
   `.github/workflows/jumbo-publish.yml` (the file name is yours to choose;
   only the location is fixed).
2. Replace both `<ref>` occurrences with the JumboBuild tag or full commit
   SHA you release with (see the pinning note above) — the `uses:` line and
   the `jumbobuild-ref` input.
3. Nothing else — no further inputs to wire, no release steps to add. The
   forwarder
   triggers on `push` to your default branch and on `workflow_dispatch`
   (which is how Mahout-orchestrated re-releases dispatch it), inherits the
   org secrets, and grants `contents: write` for the GitHub Release on your
   repository.

The template, in full:

```yaml
name: jumbo publish

on:
  push:
    branches: [main]
  workflow_dispatch:

jobs:
  jumbo-publish:
    uses: zephytiju/JumboBuild/.github/workflows/jumbo-publish.yml@<ref>
    secrets: inherit
    # The same JumboBuild ref the uses: line pins — the reusable workflow
    # builds the jumbo binary from exactly this ref.
    with:
      jumbobuild-ref: <ref>
    # Reusable workflows cannot elevate permissions; the GitHub Release and
    # the tag it creates live on THIS repository, so the calling job grants
    # contents: write. No other permission is needed.
    permissions:
      contents: write
```

## Inputs

| Input | Type | Default | Meaning |
| --- | --- | --- | --- |
| `commit` | string | `""` | Commit SHA of the caller repository to release; empty means the triggering commit. `workflow_dispatch` uses it to re-release a specific commit. |
| `jumbobuild-ref` | string | `""` | JumboBuild ref (tag, branch, or full SHA) to build the `jumbo` binary from — pin the same ref the `uses:` line uses. **Required when the workflow is called with `workflow_call`** (the called workflow cannot see the `uses:` ref; omitting it fails fast). Empty falls back to the ref this workflow lives at, which direct runs on JumboBuild itself use. |
| `ecosystem` | string | `auto` | `python`, `npm`, or `auto` (detected from the package manifest by jumbo). |
| `publish-to-public-registry` | boolean | `false` | Opt-in publication to PyPI/npm for external consumers, always driven by the jumbo-computed version. |
| `publish-image` | boolean | `false` | Opt-in for **image-producing packages**: build the service image (`Dockerfile` at the repository root) from the same release commit, push it to GHCR, verify the pushed digest against the registry, and record it as the record's `imageDigest`. See [Image-producing packages](#image-producing-packages). |
| `index-repository` | string | `zephytiju/JumboIndex` | The index repository to resolve against and append to. |

Workflow outputs (for downstream jobs): `package`, `version`, `bump`
(`none|minor|patch|bootstrap`), `published` (`true` when artifacts were
published and the index appended), `reused` (`true` when a fingerprint hit
skipped the build), and `image-digest` (the verified `sha256:...` GHCR digest
when `publish-image` was enabled and the image was pushed; empty otherwise).

## Required secrets

All secrets are org-level and flow in with `secrets: inherit`. None of them
may ever appear as a literal in any repository file.

The workflow resolves index/artifact authentication at run time, in this
order:

1. **Preferred — the org CI GitHub App pair.** When `JUNTAI_CI_APP_ID` and
   `JUNTAI_CI_APP_PRIVATE_KEY` are both present, the workflow mints a
   short-lived **installation token** at run time
   (`actions/create-github-app-token`, pinned to a full commit SHA). The
   token is downgraded to exactly `contents: read+write`, scoped to the two
   repositories the executor touches (`zephytiju/JumboBuild` and the index
   repository), and revoked when the job ends. It authenticates the
   JumboBuild checkout, the JumboIndex fetch, **and** the index append. No
   long-lived credential is stored anywhere; the private key flows only from
   its secret into the minting action — never echoed, logged, or written to
   a file.
2. **Backward-compatible fallback — the static token secrets.** When the App
   pair is absent, the workflow falls back to the two static secrets below,
   exactly as it did before the App path existed. If only one half of the
   App pair is set, the run warns and uses the fallback.
3. **Neither path available** — the workflow fails fast with an actionable
   `::error` naming both options, before any build work. The caller's
   default `GITHUB_TOKEN` cannot read another repository, so continuing
   would only fail later at the index clone (the failure mode this
   resolution replaces).

| Secret | Required | Used for |
| --- | --- | --- |
| `JUNTAI_CI_APP_ID` + `JUNTAI_CI_APP_PRIVATE_KEY` | preferred (one of the two auth paths must exist) | The org CI GitHub App pair. Mints a short-lived installation token (`contents: read+write`, scoped to `zephytiju/JumboBuild` + the index repository, revoked at job end) used for the JumboBuild checkout, the index fetch, and the index append. The private key is consumed only as the minting action's `private-key` input. |
| `JUNTAI_GITHUB_ARTIFACT_TOKEN` | static fallback | When the App pair is absent: a token that can **read** the private `zephytiju/JumboBuild` (the workflow/JumboBuild checkout) and **clone** the private `zephytiju/JumboIndex`. |
| `JUNTAI_INDEX_TOKEN` | static fallback (on publish) | When the App pair is absent: a token with `contents: write` on `zephytiju/JumboIndex` — pushes the index append. Passed to `scripts/jumbo_index_append.py` through the `JUMBO_INDEX_TOKEN` environment variable only. |
| `PYPI_TOKEN` | only with `publish-to-public-registry: true` and a Python package | Trusted publishing token for `uv publish`. Consumed via the `UV_PUBLISH_TOKEN` env var. |
| `NPM_TOKEN` | only with `publish-to-public-registry: true` and an npm package | Automation token for `npm publish`. Consumed via the `NODE_AUTH_TOKEN` env var. |

## Required repository and App permissions

- The caller repository must be able to call the workflow: JumboBuild is
  private, so the caller must be in the same GitHub organization
  (`zephytiju`) — public Meridian-family repositories qualify.
- The calling job grants `permissions: contents: write` (the GitHub Release
  on the caller repository). Image-producing packages additionally grant
  `packages: write` (the GHCR push with the caller's `GITHUB_TOKEN`) — only
  with `publish-image: true`. No other permission is requested.
- **App path (preferred)**: the org CI GitHub App must be installed on
  `zephytiju/JumboBuild` and `zephytiju/JumboIndex` with repository
  permission `Contents: Read and write` — that is all the executor needs,
  and the minted installation token is downgraded to exactly it (scoped to
  those two repositories, revoked when the job ends).
- **Static fallback**: the token behind `JUNTAI_GITHUB_ARTIFACT_TOKEN` needs
  read access to `zephytiju/JumboBuild` and `zephytiju/JumboIndex`; the
  token behind `JUNTAI_INDEX_TOKEN` needs write (push) access to
  `zephytiju/JumboIndex`. Index branch protection should keep requiring the
  index validator and forbidding force-pushes (this also holds for the App
  path — the minted token pushes the append the same way).

## What the workflow does

| Step | Command / action | Notes |
| --- | --- | --- |
| Resolve index/artifact auth | presence checks on the secrets | App pair present → mint an installation token; else the static `JUNTAI_GITHUB_ARTIFACT_TOKEN` / `JUNTAI_INDEX_TOKEN` fallback; neither → actionable `::error` before any build work. |
| Mint installation token | `actions/create-github-app-token@<full SHA>` | App path only: short-lived token downgraded to `contents: read+write`, scoped to `zephytiju/JumboBuild` + the index repository, revoked when the job ends; the private key flows only from its secret into the action input. |
| Checkout caller repo | `actions/checkout@v6` at the release commit | Clean, attributable tree — `jumbo promote` refuses a dirty tree. |
| Build jumbo | `actions/checkout@v6` + `cargo build --release --locked` | From the JumboBuild ref pinned by `jumbobuild-ref` (direct runs on JumboBuild fall back to the ref the workflow was dispatched at — never from `GITHUB_WORKFLOW_REF`, which names the caller's workflow under `workflow_call`); cargo build cached with `Swatinem/rust-cache@v2`. |
| Fetch the index | `gh repo clone zephytiju/JumboIndex` | Authenticated with the minted installation token or the static artifact token. `JUMBO_INDEX_PATH` points every later jumbo command at this clone. |
| Lock | `jumbo lock` | Resolves internal deps from the index, generates `uv.lock` / `package-lock.json`. |
| Fingerprint | `jumbo fingerprint` | `sha256(own commit + canonical extract)`; informational evidence for the run log. |
| Dedup | `jumbo dedup` | **Fingerprint hit ⇒ `jumbo dedup --materialize` pulls the recorded artifact (exact URL, SHA-256 verified) into `dist/` and the run ends — no build, no publish, no append.** Same skip-build rule as CircleCI. |
| Promote | `jumbo promote` | Decision JSON; `publishRequired` is true exactly when a new version was computed. |
| Build on bump | `uv build` / `npm pack` | The manifest keeps declaring only the major; the build's version is set from the decision's `publish.version` before building. |
| Checksums + Release | `sha256sum` → `gh release create v<version>` on the **caller** repository | Assets + `SHA256SUMS`; notes carry the version, bump, commit, fingerprint, executor, and run URL. |
| Service image (opt-in) | `docker/build-push-action@v6` → `scripts/verify_image_digest.sh` | Only with `publish-image: true`: build the `Dockerfile` at the repository root from the same release commit, push to `ghcr.io/<owner>/<repo>:v<version>`, then verify the pushed digest against the registry **before** anything is recorded — a digest mismatch aborts the build with no index append (standard §3.7). |
| Index append | `scripts/jumbo_index_append.py --push` | JumboIndex append protocol: canonical one-line record, serialized fast-forward push, fetch-and-retry on non-FF (bounded backoff), validator-gated, no history rewrites ever. Authenticated with the minted installation token or the static `JUNTAI_INDEX_TOKEN`. `executor` is `jumbo-publish-github-actions`. |
| Public registry (opt-in) | `uv publish` / `npm publish` | Gated by `publish-to-public-registry` (default **off**); version always the jumbo-computed one; tokens only from caller secrets. |

## The index record from this executor

The appended record has exactly the JumboIndex record shape (the CircleCI
executor appends the same fields): `package`, `major`, `version`, `commit`,
`fingerprint`, `canonicalExtract` from the promotion decision's `publish`
block, plus the executor-filled `artifactUrl` (the release asset URL of the
primary artifact), `artifactSha256`, `imageDigest` (the verified GHCR digest
when `publish-image` was enabled — else `null`; see
[Image-producing packages](#image-producing-packages)), `buildId`
(`<package>-<version>-gha<run id>`), `pipelineRun` (the caller's Actions run
URL), `executor` (`jumbo-publish-github-actions`), and `timestamp`. The
index covers public and private packages uniformly, so consumers resolve
both through the same major-based rule.

## Image-producing packages

A package that produces a service image opts in with `publish-image: true`
and grants `packages: write` on the calling job. The contract for that path
(the standard's §3.4/§3.6/§3.7 requirements, executor form):

```yaml
jobs:
  jumbo-publish:
    uses: zephytiju/JumboBuild/.github/workflows/jumbo-publish.yml@<ref>
    secrets: inherit
    with:
      jumbobuild-ref: <ref>
      publish-image: true
    permissions:
      contents: write
      packages: write   # only needed with publish-image: true (GHCR push)
```

- **Registry**: service images live on GHCR only (`ghcr.io/<owner>/<repo>:v<version>`,
  lowercase as GHCR requires). The digest verification refuses to inspect any
  reference whose registry is not exactly `ghcr.io` — no other registry,
  localhost/loopback, or private address is ever contacted for images.
- **Same release commit**: the image is built from the caller repository's
  checkout at the release commit — the exact commit the language artifacts,
  the release tag, and the index record refer to.
- **Credentials**: the push authenticates with the caller's own `GITHUB_TOKEN`
  (`packages: write`); no image-specific secret exists. Digests are public
  values; no credential literal appears anywhere.
- **Provenance before recording**: after the push, the workflow verifies the
  registry-observed digest equals the digest the push reported
  (`scripts/verify_image_digest.sh`). The verified digest is the only value
  ever written to the record's `imageDigest` field — the field deployment
  pinning consumes (§3.6). A mismatch or malformed digest aborts the build
  **before** the index append, so no record can carry unverified provenance
  (§3.7 failure behavior). The GitHub Release may exist unrecorded after an
  abort; resolution only ever goes through the index, so nothing consumes it.
- **Dedup**: a fingerprint hit skips the build, and with it the image leg —
  reuse pulls the recorded language artifact and never re-publishes an image.
- **CI evidence**: `.github/workflows/jumbo-publish-image-check.yml`
  exercises this exact path against the real registry on every change to it —
  build → push → digest verification (positive control) → §3.7 mismatch and
  malformed-digest and non-GHCR-host negative controls → capture of the
  verified digest into a record that passes the executor-side validation.
  The fixture record is uploaded as run evidence and is deliberately **not**
  appended to the authoritative JumboIndex: records are append-only, and a
  fixture package is not a real promoted internal package.

## Local verification (no real publications)

The same steps can be run locally against a local JumboIndex clone — this is
how the workflow was verified end to end without pushing to the real index
or creating real releases:

```sh
cargo build --release                       # the jumbo the workflow builds
export JUMBO_INDEX_PATH=/path/to/JumboIndex # a local clone; never pushed
cd /path/to/public-repo-copy                # a throwaway copy, never pushed
jumbo lock && jumbo fingerprint && jumbo dedup && jumbo promote
uv version <nextVersion> && uv build --out-dir dist   # on publishRequired
python3 /path/to/JumboBuild/scripts/jumbo_index_append.py \
  --index-dir "$JUMBO_INDEX_PATH" --record-file record.json   # NO --push
```

Appending without `--push` writes and validates the append on the local
clone only. Omitting the flag is the offline/dry-run mode; the real workflow
passes `--push` with the minted installation token (App pair) or
`JUMBO_INDEX_TOKEN` from the caller's secrets (static fallback).

## Boundary notes

- No per-repo build logic: if a repository needs release steps beyond
  calling this workflow, that is a design change — stop and return to the
  design first (the workflow's step list mirrors the standard's release
  steps exactly).
- No CircleCI-side behavior is implemented or altered here.
- The manifest's own `version` declares only the major; the workflow never
  commits a version back to the caller repository — the computed version
  lives in the release tag, the artifacts, the index record, and (opt-in)
  the public registry.
- Python packages must carry a static `[project].version` (the workflow
  sets it for the build via `uv version`); dynamic-version builds are out
  of scope until someone needs them in the design.

[Jumbo Build & Versioning Standard]: https://qcnwge0wy4s0.feishu.cn/wiki/FqYXwyVr7iWcuEkGYTbccwsEned
