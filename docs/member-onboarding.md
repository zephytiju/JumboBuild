# Member onboarding: declaring, building, and pinning with jumbo

This is the walkthrough for a **service owner** bringing a repository into
the Jumbo build system (the [Jumbo Build & Versioning Standard], §5.1
service-owner interface). Onboarding is **two touches** — a forwarder file
and a pipeline listing — and after that you never touch versions again: the
pipeline owns minor and patch, you own the major and the source.

Live references this document matches (proven members you can read today):

| Member repository | Forwarder at | First/representative release | Index record |
| --- | --- | --- | --- |
| [zephytiju/MeridianCore] | main, commit `75b7066` | `meridian-storage-core@1.3.0` (tag `v1.3.0`) | JumboIndex commit `e5434ee` |
| [zephytiju/MeridianSemantics] | main, commit `fe0997c` | `meridian-storage-semantics@2.2.0` (tag `v2.2.0`) | JumboIndex commit `566fbaf` |
| [zephytiju/JuntaiPythonPackage] | main, commit `92466a9` | `juntai-python-package@1.0.0` (bootstrap cut, tag `v1.0.0`) | JumboIndex commit `7cc0d03` |

Related reading:

- Executor contract (what the reusable workflow runs): [docs/jumbo-publish.md](./jumbo-publish.md)
- Deployment pinning contract and reproduction: [docs/pinning.md](./pinning.md)
- Generated command reference: [docs/cli-reference.md](./cli-reference.md)

## 1. The service-owner interface (standard §5.1)

Three rules, and they are the whole contract:

1. **Declare internal dependencies major-only.** The declaration says which
   major you accept; resolution takes the newest promoted build of that
   major (newest by record order, never wall clock).
2. **Never edit minor or patch.** The pipeline owns those segments: a
   promoted rebuild of your own source bumps the minor, a dependency-only
   refresh bumps the patch. A manifest that pins `==1.2.3` is rejected.
3. **Run the same jumbo commands locally that CI runs.** There is no
   special CI mode — the executor runs the same `jumbo lock` /
   `jumbo fingerprint` / `jumbo dedup` / `jumbo promote` sequence you can
   run in your checkout (§5 below). Only authentication differs.

A breaking change is a manual **major** bump in your manifest (resets
minor/patch to 0; the first build of a major is `M.0.0`). Consumers stay on
the old major until they declare the new one — nothing breaks underneath
them, because resolution never crosses a major.

### What a declaration looks like

| Manifest | Accepted (internal, major-only) | Rejected |
| --- | --- | --- |
| `pyproject.toml` | `meridian-storage-core@1`, `pkg==2.*`, `pkg~=2.0`, `pkg>=2,<3` | Git URLs, direct wheel/tarball URLs, exact pins, ranges not bounding one major |
| `package.json` (`@juntai/*`) | `"1"`, `"1.x"`, `"^1"`, `"~1"`, `">=1,<2"` | Git/tarball URLs, `user/repo` shorthands, `file:` paths, exact pins, `"*"` |

No manifest ever carries a GitHub URL for an internal package, and no
repository commits a lock file: `jumbo lock` generates it, and **the index
record is the lock** (§8 below).

## 2. Onboarding is two touches

### Touch 1 — the forwarder file

Copy [`templates/member-jumbo-publish.yml`](../templates/member-jumbo-publish.yml)
into your repository as `.github/workflows/jumbo.yml` and replace **every**
`<ref>` occurrence with the zephytiju/JumboBuild ref you verify and release
with (a tag or a full commit SHA — the members pin a full SHA). The `uses:`
lines pick the workflow versions; the `jumbobuild-ref` inputs pin the ref
jumbo is built from. Under `workflow_call` GitHub does not expose the
`uses:` ref to the called workflow, so the input is the authoritative pin —
keep them identical.

The forwarder is the **single workflow file** a member repository carries,
with two modes:

- **PR-mode** (`pull_request` + `push` to the default branch) calls the
  reusable [jumbo-verify](./jumbo-verify.md) workflow: `jumbo lock` →
  materialize the recorded dependency artifacts → build → test, read-only.
  This replaces any standalone per-PR verify workflow the repository
  carried; repository-specific checks (lock-presence gates, secret scans,
  boundary checks) move into the repository's own test surface (an npm
  `verify` script or a pytest test), where the verify pipeline runs them.
- **Dispatch-mode** (`workflow_dispatch`) calls the reusable
  [jumbo-publish](./jumbo-publish.md) workflow with the **release-commit
  passthrough** (`commit: ${{ inputs.commit }}`): plain dispatches release
  the triggering commit, and a dispatch with an explicit `commit` releases
  that exact existing commit — the re-release path a fingerprint-equal
  dedup re-run uses.

That is the entire file. It carries **no release and no verify logic**:
every release step
(lock → fingerprint → dedup → promote → publish on bump → index append →
opt-in public registry) is owned by the reusable
[`.github/workflows/jumbo-publish.yml`](../.github/workflows/jumbo-publish.yml)
in this repository, and every verify step by
[`.github/workflows/jumbo-verify.yml`](../.github/workflows/jumbo-verify.yml).
If your repository needs workflow steps beyond calling these two workflows,
that is a design change — stop and return to the standard
first.

### Touch 2 — the pipeline listing

Get the repository covered by a jumbo pipeline: register it in the Mahout
pipeline definitions so its `jumbo publish` workflow is dispatched and
ordered against its dependencies. This is the **absorption step** — until a
repository is covered, its consumers hit the absorption error (§6). Where a
repository is configured for auto promotion, the pipeline publishes its
package whenever the build requires a version bump; there is no
manually-released internal package mode. Service pipelines that deploy
Pulumi resources publish artifacts too, not only build-only pipelines.

## 3. Manifest requirements

- **Python** (`pyproject.toml`): a static `[project].version` declaring only
  the major (the workflow sets the real build version via `uv version`
  before building; dynamic-version builds are not supported yet). The
  `[project].name` is the package name every record and declaration uses.
- **Node** (`package.json`): internal dependencies under the `@juntai/*`
  scope (legacy `@zephytiju/*` accepted) with major-only ranges.
- Do **not** commit `uv.lock` / `package-lock.json`; do not add release
  scripts. `jumbo release` (local) stays a release-readiness check — it
  never publishes.

## 4. What the first release does (the bootstrap cut)

The first push to your default branch runs the executor end to end:

1. `jumbo lock` resolves your major-only declarations against the index and
   generates the language lock with internal sources injected.
2. `jumbo fingerprint` computes `sha256(commit + canonical extract)`.
3. `jumbo dedup` finds no record of your package → build path.
4. `jumbo promote` computes the **bootstrap** decision: version `M.0.0`.
5. The workflow materializes recorded dependency artifacts, builds, and
   publishes the wheel/tarball plus `SHA256SUMS` as a GitHub Release
   (`vM.0.0`) on **your** repository.
6. `scripts/jumbo_index_append.py` appends the record to JumboIndex:
   `buildId <package>-<version>-gha<run id>`, your commit, the fingerprint,
   the canonical extract, the artifact URL and SHA-256 (and `imageDigest`
   with `publish-image: true`).

Live example: `juntai-python-package@1.0.0` — the bootstrap cut of
[zephytiju/JuntaiPythonPackage] (forwarder at `92466a9`, tag `v1.0.0`,
record appended by JumboIndex commit `7cc0d03`, buildId
`juntai-python-package-1.0.0-gha36497683368`).

Every later push re-runs the same sequence: a fingerprint hit skips the
build entirely (the recorded artifact is re-pulled instead, SHA-256
verified); a source change promotes a **minor**; a dependency-closure
change on an unchanged commit promotes a **patch**. Fingerprint-equal
re-runs converge on the same artifact — concurrent coverage of the same
repository cannot fork it.

## 5. Local parity: the same commands, your checkout

```sh
git clone https://github.com/zephytiju/JumboBuild.git && cd JumboBuild && ./install.sh
git clone https://github.com/zephytiju/JumboIndex.git   # a read-only consumer clone
export JUMBO_INDEX_PATH=/path/to/JumboIndex

cd /path/to/your-repo
jumbo lock          # resolve majors against the index, generate the lock
jumbo fingerprint   # sha256(commit + canonical extract) — a pure-local query
jumbo dedup         # build-or-reuse decision; --materialize pulls the recorded artifact
jumbo promote       # the version bump decision the executor publishes on
```

Identical inputs produce identical decisions locally and in CI — same
resolution, same fingerprint, same promote JSON. Two boundaries keep local
runs honest:

- **Promotion is pipeline-only.** `jumbo fingerprint --promote` (and every
  promotion-mode operation) refuses a dirty working tree, and only a
  pipeline run appends records. Local builds never promote.
- **Credentials enter by name, from the environment.** A local re-pull from
  a private repository needs a token in `GH_TOKEN` (or `--artifact-dir` /
  `JUMBO_ARTIFACT_DIR` pointing at a local cache of release assets — the
  recorded SHA-256 is still enforced). Never write a token into any file.

## 6. Absorption error remediation

An internal dependency with no index record fails resolution — fast, with
the package named and the step spelled out:

```
$ jumbo resolve some-unabsorbed-package@1
Error: absorption error: internal dependency `some-unabsorbed-package` (declared major 1)
from <declaration> has no record in the Jumbo index (https://github.com/zephytiju/JumboIndex).
  Absorption step: bring the package's repository into the Jumbo build system — cover it
  with a pipeline so every promoted build appends an index record. The dependency cannot
  be consumed until its repository is absorbed (Jumbo Build & Versioning Standard,
  Resolution Semantics).
```

Remediation is §2 of this document, applied **to the dependency's
repository** (not yours): add the forwarder file and get the repository
listed in a pipeline. Once its first build appends a record, the error
disappears — no manifest change is needed on the consumer side. If you hit
this error you are the consumer; the fix belongs to the package owner.
There is no override, no fallback registry, and no direct URL escape hatch:
the standard rejects Git URLs and wheel URLs precisely so that resolution
stays inside the index.

## 7. Third-party refresh policy

Third-party ranges (`numpy>=1.26`, `^4.17`) are re-resolved by the lock
step, and the canonical extract captures the resolved versions — so a
third-party update **within its declared range** changes the fingerprint
and promotes a patch bump, exactly like an internal dependency change. The
policy knob controls when that re-resolution happens:

| Policy | Meaning | When to choose it |
| --- | --- | --- |
| `run` (default) | every pipeline run re-resolves declared ranges | default: freshest resolutions, third-party updates promote promptly |
| `schedule:<interval>` — `24h`, `30m`, `7d`, `2w` | re-resolve only when the cadence has elapsed since the package's newest index record; in between the current resolution stands (extract unchanged, no patch churn) | bound the build-minute cost of freshness waves across many repositories |
| `schedule:<cron>` | the cron is recorded verbatim; the cadence is honored by the executor's pipeline schedule | align refresh waves with an off-peak window |

Set it per invocation with `jumbo lock --refresh <policy>` / `jumbo promote
--refresh <policy>`, or for every command in an environment with
`JUMBO_REFRESH`. The executor's default is `run`; a scheduled cadence is a
per-repository decision, not a global one.

## 8. Pin and reproduce: what your consumers (and you) get

Every promoted build is addressable and reproducible by its `buildId`
(`<package>-<version>-gha<run id>` for executor-appended records;
`bootstrap-<12 hex>` derived ids for imported records — pin works, but a
bootstrap record predates the fingerprint engine and cannot be reproduced;
the typed error says so).

```sh
# The deployment pin manifest (buildId, commit, imageRef, artifact, fingerprint)
jumbo pin meridian-storage-core --by-build-id meridian-storage-core-1.3.0-gha36483044691
jumbo pin meridian-storage-core --by-commit 75b706626b44b0d0821df667e81e84f505df2c8b
jumbo pin meridian-storage-core --latest-of-major 1

# Reproduce a past build exactly from its record — the record is the lock,
# no repository lock file is consulted
jumbo build --pinned meridian-storage-core-1.3.0-gha36483044691
jumbo reproduce  meridian-storage-core-1.3.0-gha36483044691   # the same command
```

The reproduction resolves the record, recomputes
`sha256(commit + canonical extract)` and aborts on any mismatch **before**
fetching anything, materializes every internal dependency of the recorded
closure at its **exact recorded version** under `reproduced/deps/<slug>/`,
and lands the own artifact under `reproduced/dist/` — **producing the
recorded SHA-256**. Artifacts come from the github.com-only fetch layer
(identical locally and in CI) or from `--artifact-dir` / `JUMBO_ARTIFACT_DIR`
(a cache is a transport, not a trust anchor — cached bytes are verified too).

Proven live leg (fresh clone, no credentials — the release is public):

```text
$ jumbo build --pinned meridian-storage-core-1.3.0-gha36483044691
{ "contract": "jumbo.pinned-reproduction/1", "package": "meridian-storage-core",
  "version": "1.3.0", "fingerprintMatch": true, ... }
$ shasum -a 256 reproduced/dist/meridian_storage_core-1.3.0-py3-none-any.whl
f71fccba77e0e22678554df676b87a396c98246a7c162dcc1e90e53048795777
```

…byte-identical to the digest recorded in the index for `v1.3.0`. The
closure path is proven the same way: reproducing
`meridian-storage-semantics-2.2.0-gha36496161507` pulls
`meridian-storage-core@1.3.0` (the exact recorded version, digest
`f71fccba…` verified) into `deps/meridian-storage-core/`.

Exit codes: `0` digest-verified success (report JSON on stdout), `1` typed
failure (error on stderr), `2` usage error. Output is deterministic: two
runs of the same record print byte-identical JSON.

## 9. Secrets and permissions (what to configure, never what to paste)

| Secret | Required | Used for |
| --- | --- | --- |
| `JUNTAI_CI_APP_ID` + `JUNTAI_CI_APP_PRIVATE_KEY` | preferred auth (one of the two auth paths must exist) | Org CI GitHub App pair; the executor mints a short-lived installation token (`contents: read+write`, scoped to JumboBuild + the index repository, revoked at job end) |
| `JUNTAI_GITHUB_ARTIFACT_TOKEN` | static fallback | Read on JumboBuild + JumboIndex (checkouts, index fetch, private-asset re-pull) |
| `JUNTAI_INDEX_TOKEN` | static fallback (on publish) | `contents: write` on JumboIndex — the index append |
| `PYPI_TOKEN` / `NPM_TOKEN` | only with `publish-to-public-registry: true` | Opt-in public registry publication, always at the jumbo-computed version |

Flow them with `secrets: inherit`; the calling job grants
`permissions: contents: write` (the GitHub Release on your repository) and
`packages: write` (declared by the executor, exercised only with
`publish-image: true`). Tokens are consumed via secrets and environment
only — never as literals in any file. Full details:
[docs/jumbo-publish.md](./jumbo-publish.md).

## 10. Error quick reference

| Error | Meaning | Fix |
| --- | --- | --- |
| `absorption error: internal dependency … has no record` | a declared internal package is not covered by a pipeline | onboard the dependency's repository (§2); nothing to change in your manifest |
| declaration rejected (`git+https`, wheel URL, exact pin, …) | the manifest carries a forbidden internal-dependency form | declare major-only (§1) |
| `jumbo promote` refuses: dirty working tree | promotion guard | commit; only clean, attributable trees promote inside a pipeline |
| auth fails fast naming both options | neither the App pair nor the static secrets exist | configure the secrets (§9) |
| `PIN_FINGERPRINT_MISMATCH` | a record does not reproduce its stated inputs | do not consume it; report the standards violation |
| `closure incomplete for …` | a recorded dependency version vanished from the index | append-only violation; report it |
| `buildId … cannot be reproduced` | bootstrap record (predates the fingerprint engine) | pin it, or reproduce the first promoted record instead |
| `PIN_IMAGE_REQUIRED` | `--require-image` and the record has no `imageDigest` | pin a record that produced an image, or deploy artifact-only |
| digest mismatch on fetch | artifact bytes ≠ recorded SHA-256 | abort is correct; investigate the asset, never bypass |

[Jumbo Build & Versioning Standard]: https://qcnwge0wy4s0.feishu.cn/wiki/FqYXwyVr7iWcuEkGYTbccwsEned
[zephytiju/MeridianCore]: https://github.com/zephytiju/MeridianCore
[zephytiju/MeridianSemantics]: https://github.com/zephytiju/MeridianSemantics
[zephytiju/JuntaiPythonPackage]: https://github.com/zephytiju/JuntaiPythonPackage
