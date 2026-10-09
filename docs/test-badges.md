# Shared test-status badges

JumboBuild owns measurement boundaries, SVG rendering and publication. A trusted
scheduler in `zephytiju/JumboIndex` calls `.github/workflows/test-badges.yml`,
which reads completed source runs through GitHub's API. Consumers keep only
README links and the standard verification forwarder. They do not commit badge
files, run a renderer, send badge artifacts, or receive index write credentials.
No package release is necessary to refresh status.

## Stable interface

Files live outside the immutable package index:

```text
badges/<owner>/<repository>/<encoded-default-branch>/tests.svg
badges/<owner>/<repository>/<encoded-default-branch>/last-run.svg
badges/<owner>/<repository>/<encoded-default-branch>/status.json
```

Preserve owner/repository spelling. The branch is one component, percent-encoded
with UTF-8 and no safe reserved characters (`release/a` becomes `release%2Fa`).
When forming a URL for that literal filename, encode the percent sign as well:
`release%252Fa`. Both initial consumers use `main`, requiring no encoding.

```markdown
[![Tests](https://raw.githubusercontent.com/zephytiju/JumboIndex/main/badges/zephytiju/JuntaiFuseAPI/main/tests.svg)](https://github.com/zephytiju/JuntaiFuseAPI/actions/workflows/jumbo-publish.yml)
```

Substitute `JuntaiObservabilityTools` for the other initial consumer. The link
leads to the consumer's workflow; `status.json` identifies the exact attempt.

**Visibility:** JumboIndex was checked during implementation and is private.
GitHub README image proxy requests cannot reliably authenticate to private raw
files; these stable URLs therefore do not currently promise a visible inline
image. Do not embed PATs, signed download URLs or credentials in a README/SVG,
and do not change repository visibility as part of adoption. Authenticated
readers can inspect the files in JumboIndex and use the linked Actions run. A
separately approved public serving arrangement would be needed for public image
rendering. This implementation does not create one.

## What a badge means

The reusable verification workflow exposes named `Jumbo tests (python)` and
`Jumbo tests (npm)` steps. The producer checks the exact reusable workflow SHA
in GitHub's `referenced_workflows` metadata, the configured workflow filename,
source repository, push event, default branch, and verification job identity.
It fetches jobs for the exact run attempt, with pagination. It never reads logs,
artifacts or consumer files, and never executes consumer code.

| Outcome | Evidence |
| --- | --- |
| `passed` | Exactly one test command succeeded and the verification job succeeded. |
| `failed` | A test command failed. |
| `error` | Verification failed/cancelled or did not finish successfully, including setup, dependency, or build failures. |
| `not-run` | Verification succeeded but no test command ran (for example pytest absent). |
| `unknown` | Producer SHA is unapproved, or the verification job/measurement is missing or ambiguous. |

A successful workflow alone is never evidence of passing tests. Runner command
success measures the configured test command's exit status; it does not assert
a test count or coverage. Missing pytest remains build-only verification; pytest
collection errors and its no-tests exit status remain failures. All failures
retain their original Actions conclusions: collection happens later in another
repository and cannot make a source run green. Only completed default-branch
push verification is eligible; PRs, dispatch-based releases and feature branches
cannot update the stable default-branch path.

`tests.svg` is required. `last-run.svg` is a measured completion timestamp.
Reserved optional companions are `coverage.svg`, `duration.svg`, `warnings.svg`,
`skipped.svg` and `xfailed.svg`; this version emits none of them because the
metadata does not measure them. Do not infer numerical results from a successful
exit code. SVGs are standalone XML, escape labels in text/attribute contexts,
and include accessible title/label text without scripts, external assets or
links to authenticated resources.

## Machine-readable record

`status.json` is a mutable `jumbo.test-status/v1` object alongside the SVGs:

| Field | Meaning |
| --- | --- |
| `repository`, `branch` | GitHub source owner/name and actual default branch. |
| `commit`, `workflow` | Full source SHA and configured forwarder filename. |
| `runId`, `runAttempt`, `runUrl` | Positive numeric run/attempt and exact GitHub attempt URL. |
| `completedAt`, `completedAtSource` | RFC 3339 last job completion (`job.completed_at`); for startup failures without jobs, the completed run's API update time (`run.updated_at`), explicitly distinguished from an exact job completion. |
| `producerCommit` | Approved full JumboBuild SHA for the collector and verification contract. |
| `outcome`, `reason` | Enum above and human-readable explanation. |
| `measurements` | Test step name, `conclusion`, `started_at`, `completed_at` (API values, nullable timestamps). Empty if no test command was measured or producer was unapproved. |

Unknown legacy runs are recorded as unverified, never passing. When updating the
producer, pin consumer verification and JumboIndex to the same approved SHA and
trigger a fresh default-branch verification; publication intentionally does not
reinterpret a previously recorded run/attempt.

## Write authorization and ordering

Only the JumboIndex `main` schedule/manual-dispatch job may write. The reusable
publisher additionally checks the repository, ref and event before any secrets
or checkout. Its short-lived source-reader token has `contents:read` and
`actions:read` on the three named source/tool repositories. The fallback
`JUNTAI_GITHUB_ARTIFACT_TOKEN` must have those read permissions. An App installed
without Actions read permission must be updated before use. The index writer is
JumboIndex's own `GITHUB_TOKEN` with `contents:write`, held only in this trusted
job. Nothing is added to PR verification's credentials or permissions.

The source allowlist lives in JumboIndex `config/test-badges.json`; extending it
also requires adding the repository to the publisher's read-token scope. Only
trusted default-branch maintainers can change the allowlist, workflow pins or
consumer verification scripts; their reviewed code defines each test command.
The publisher does not claim to defend against a malicious source maintainer.

The newest completed push is chosen by numeric run ID, then attempt. A stored
`(runId, runAttempt)` can only increase. Equal and older outcomes are no-ops,
regardless of completion timestamps. Publication creates a disposable linked
worktree from the fetched index tip and commits only `badges/`. Pushes are
fast-forward only; on a race with another badge writer or package append, fetch
the new tip, re-check every existing status record, re-render, and retry (five
attempts maximum). The caller checkout is never reset. A failed API fetch or
malformed metadata aborts collection before any publication. Existing images
then remain the last completed recorded result; `last-run` exposes their age.

`index/*.jsonl` remains append-only and immutable; this protocol never reads,
rewrites or appends package records and is independent of package promotion.
Normal branch protection must permit the trusted bot's fast-forward commits
(or provide the repository's approved bot exception); there is no force push or
branch-protection bypass. No images are seeded with invented passing results.

## Validation and rollout

Run the offline producer suite:

```sh
python3 -m unittest discover -s scripts/tests -p 'test_jumbo_test_badges.py' -v
cargo test --locked --test jumbo_verify_workflow
```

The suite covers outcomes, rejected PR/dispatch/feature runs, exact-attempt reads,
pagination, escaping, branch paths, symlink refusal, monotonic updates and a real
local Git race preserving package history. Index append CI includes this suite.

1. Review/merge the JumboBuild producer change, without a package release.
2. Review the separate JumboIndex scheduler/configuration change, pinned to the
   approved producer SHA. Configure source read access and index bot push access.
3. Update the two consumers' verification workflow pins to that SHA through
   their existing cleanup changes. Do not modify their release pin merely for
   badges. Remove local badges and use the stable README URLs.
4. After fresh default-branch push verification, dispatch the JumboIndex badge
   workflow or wait for its 15-minute schedule. Inspect `status.json` and the
   linked run attempt. Private-image rendering remains limited as stated above.

The implementation PRs do not publish badges, merge changes, release/delete
packages, or alter historical index records.
