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
  (unit tests: [`scripts/tests/test_jumbo_index_append.py`](../scripts/tests/test_jumbo_index_append.py))

## The two-line caller

Add this to a workflow in the public repository (for example
`.github/workflows/jumbo-release.yml` triggered on `push` to the default
branch and on `workflow_dispatch`):

```yaml
jobs:
  jumbo-publish:
    uses: zephytiju/JumboBuild/.github/workflows/jumbo-publish.yml@<ref>
    secrets: inherit
```

`<ref>` is the JumboBuild ref to release with — a tag or a full commit SHA.
The workflow builds the `jumbo` binary from exactly the JumboBuild commit
the workflow file was called at, so a pinned `<ref>` also pins the release
toolchain.

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
2. Replace `<ref>` with the JumboBuild tag or full commit SHA you release
   with (see the pinning note above).
3. Nothing else — no inputs to wire, no release steps to add. The forwarder
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
| `ecosystem` | string | `auto` | `python`, `npm`, or `auto` (detected from the package manifest by jumbo). |
| `publish-to-public-registry` | boolean | `false` | Opt-in publication to PyPI/npm for external consumers, always driven by the jumbo-computed version. |
| `index-repository` | string | `zephytiju/JumboIndex` | The index repository to resolve against and append to. |

Workflow outputs (for downstream jobs): `package`, `version`, `bump`
(`none|minor|patch|bootstrap`), `published` (`true` when artifacts were
published and the index appended), `reused` (`true` when a fingerprint hit
skipped the build).

## Required secrets

All secrets are org-level and flow in with `secrets: inherit`. None of them
may ever appear as a literal in any repository file.

| Secret | Required | Used for |
| --- | --- | --- |
| `JUNTAI_GITHUB_ARTIFACT_TOKEN` | yes | A GitHub App installation or user token that can **read** the private `zephytiju/JumboBuild` (the workflow/JumboBuild checkout) and **clone** the private `zephytiju/JumboIndex`. The caller's default `GITHUB_TOKEN` cannot cross repository boundaries; it is used as a fallback and works only if it can read both repositories. |
| `JUNTAI_INDEX_TOKEN` | yes (on publish) | A token with `contents: write` on `zephytiju/JumboIndex` — pushes the index append. Passed to `scripts/jumbo_index_append.py` through the `JUMBO_INDEX_TOKEN` environment variable only. |
| `PYPI_TOKEN` | only with `publish-to-public-registry: true` and a Python package | Trusted publishing token for `uv publish`. Consumed via the `UV_PUBLISH_TOKEN` env var. |
| `NPM_TOKEN` | only with `publish-to-public-registry: true` and an npm package | Automation token for `npm publish`. Consumed via the `NODE_AUTH_TOKEN` env var. |

## Required repository and App permissions

- The caller repository must be able to call the workflow: JumboBuild is
  private, so the caller must be in the same GitHub organization
  (`zephytiju`) — public Meridian-family repositories qualify.
- The calling job grants `permissions: contents: write` (the GitHub Release
  on the caller repository). No other permission is requested.
- The GitHub App (or user) behind `JUNTAI_GITHUB_ARTIFACT_TOKEN` needs read
  access to `zephytiju/JumboBuild` and `zephytiju/JumboIndex`.
- The GitHub App (or user) behind `JUNTAI_INDEX_TOKEN` needs write (push)
  access to `zephytiju/JumboIndex`; index branch protection should keep
  requiring the index validator and forbidding force-pushes.

## What the workflow does

| Step | Command / action | Notes |
| --- | --- | --- |
| Checkout caller repo | `actions/checkout@v6` at the release commit | Clean, attributable tree — `jumbo promote` refuses a dirty tree. |
| Build jumbo | `actions/checkout@v6` + `cargo build --release --locked` | From the JumboBuild commit the workflow lives at (`GITHUB_WORKFLOW_REF`), cargo build cached with `Swatinem/rust-cache@v2`. |
| Fetch the index | `gh repo clone zephytiju/JumboIndex` | `JUMBO_INDEX_PATH` points every later jumbo command at this clone. |
| Lock | `jumbo lock` | Resolves internal deps from the index, generates `uv.lock` / `package-lock.json`. |
| Fingerprint | `jumbo fingerprint` | `sha256(own commit + canonical extract)`; informational evidence for the run log. |
| Dedup | `jumbo dedup` | **Fingerprint hit ⇒ `jumbo dedup --materialize` pulls the recorded artifact (exact URL, SHA-256 verified) into `dist/` and the run ends — no build, no publish, no append.** Same skip-build rule as CircleCI. |
| Promote | `jumbo promote` | Decision JSON; `publishRequired` is true exactly when a new version was computed. |
| Build on bump | `uv build` / `npm pack` | The manifest keeps declaring only the major; the build's version is set from the decision's `publish.version` before building. |
| Checksums + Release | `sha256sum` → `gh release create v<version>` on the **caller** repository | Assets + `SHA256SUMS`; notes carry the version, bump, commit, fingerprint, executor, and run URL. |
| Index append | `scripts/jumbo_index_append.py --push` | JumboIndex append protocol: canonical one-line record, serialized fast-forward push, fetch-and-retry on non-FF (bounded backoff), validator-gated, no history rewrites ever. `executor` is `jumbo-publish-github-actions`. |
| Public registry (opt-in) | `uv publish` / `npm publish` | Gated by `publish-to-public-registry` (default **off**); version always the jumbo-computed one; tokens only from caller secrets. |

## The index record from this executor

The appended record has exactly the JumboIndex record shape (the CircleCI
executor appends the same fields): `package`, `major`, `version`, `commit`,
`fingerprint`, `canonicalExtract` from the promotion decision's `publish`
block, plus the executor-filled `artifactUrl` (the release asset URL of the
primary artifact), `artifactSha256`, `imageDigest` (`null` here — this
workflow ships language artifacts, not service images), `buildId`
(`<package>-<version>-gha<run id>`), `pipelineRun` (the caller's Actions run
URL), `executor` (`jumbo-publish-github-actions`), and `timestamp`. The
index covers public and private packages uniformly, so consumers resolve
both through the same major-based rule.

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
passes `--push` with `JUMBO_INDEX_TOKEN` from the caller's secrets.

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
