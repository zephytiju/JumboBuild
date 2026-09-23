# Deployment Pinning Contract

Every promoted index record **is** the build record a deployment pins
(Jumbo Build & Versioning Standard, §3.6). This document is the
authoritative field mapping from a JumboIndex record to a deployment pin,
the machine contract of the pin manifest, the reference adapter that feeds
the existing vangu Selection/PackageLock path, and the pinned-reproduction
contract. The mapping is **additive**: the deployed IaC keeps its exact
semantics — the pin only supplies field values.

## 1. Index record → pin manifest

`jumbo pin <package> [--by-build-id X | --by-commit SHA | --latest-of-major M]
[--index PATH_OR_URL] [--image-name NAME] [--require-image]` resolves one
record of one package and emits a `jumbo.deployment-pin/v1` manifest on
stdout (pretty JSON) — ready for a Pulumi program to consume.

| Index record field | Pin manifest field | Rule |
| --- | --- | --- |
| `package` | `package` | verbatim |
| `major` | `major` | verbatim |
| `version` | `version` | verbatim |
| `buildId` | `buildId` + `buildIdSource: "record"` | the recorded value when non-null |
| *(null `buildId`)* | `buildId` + `buildIdSource: "derived"` | the bootstrap derivation below |
| `commit` | `commit` | verbatim (40-hex) |
| `imageDigest` | `imageRef` | `<image-name>@<imageDigest>` **only when the digest is present**; construction below |
| `imageDigest` | `imageDigest` | verbatim (`sha256:<64hex>`), the PackageLock source-digest mapping |
| `artifactUrl`, `artifactSha256` | `artifact {url, sha256}` | the object is emitted when `artifactUrl` is present; `sha256` may be null in records that published none |
| `fingerprint` | `fingerprint` | verbatim; null in bootstrap records |
| *(file location)* | `recordRef {indexFile, recordLine}` | the `.jsonl` file and 1-based line the record was read from — the audit pointer |
| — | `contract` | always `jumbo.deployment-pin/v1` |
| — | `selector` | which selector resolved the record |
| `timestamp` | `timestamp` | informational; **record order, not wall clock, defines recency** |

### buildId: recorded value and the bootstrap derivation

`buildId` is the pinning and reproduction key and is unique per promoted
build. Records appended by a pipeline always carry one. Bootstrap records
(imported before the fingerprint engine) have a null `buildId`; for those
the manifest carries a deterministic derivation:

```
derived buildId = "bootstrap-" + first 12 hex of sha256(
    package + "\n" + version + "\n" + commit + "\n" + (fingerprint or "")
)
```

The identity tuple (package, version, commit, fingerprint) is unique per
promoted build — auto-promotion never reuses a version — so the derived
id is stable across machines and runs, and distinct for every record.
`buildIdSource` tells the consumer which form it got. Both forms are
first-class keys: `jumbo pin --by-build-id` and `jumbo reproduce` accept a
derived id exactly like a recorded one.

### imageRef: the exact digest-pinned image string

The index records the image **digest** (`imageDigest`,
`sha256:<64hex>`); the reference **name** is deployment-owned. The
manifest emits `imageRef = <name>@<digest>` only when the record
published a digest, with the name resolved as:

1. `--image-name NAME` — an explicit deployment decision, or
2. the GHCR convention derived from the record's artifact URL
   repository: `https://github.com/<owner>/<repo>/releases/...` →
   `ghcr.io/<owner>/<repo>` (lowercased) — the artifact and the service
   image publish from the same package repository.

The constructed reference is validated against the exact-image rule the
IaC already enforces (`^[^\s@]+@sha256:[a-f0-9]{64}$`, see §3) before it
is emitted. When no name is derivable (no digest, or no
`--image-name` and no artifact URL), `imageRef` is null — an
artifact-only build.

**When the deployment needs an image and the record has a null
`imageDigest`, the pin fails loudly**, not silently:

```
$ jumbo pin demo-alpha --by-build-id demo-2.4.1-001 --require-image
Error: PIN_IMAGE_REQUIRED: `demo-alpha` 2.4.1 (buildId `demo-2.4.1-001`) has
no imageDigest — the record's build published no service image; pin a record
that produced an image or drop --require-image for an artifact-only deployment
```

### Selection rules

Exactly one selector is required (the CLI enforces exclusivity):

- `--by-build-id X` — the earliest record in append order whose recorded
  or derived buildId matches (mirrors the dedup decision's
  earliest-match determinism; buildIds are unique per promoted build by
  contract).
- `--by-commit SHA` — the **newest** record promoted from that commit. A
  dependency refresh on an unchanged commit appends patch records
  (§2.1 of the standard); the newest of the commit is the one a
  deployment of that commit pins.
- `--latest-of-major M` — the newest record of the major, exactly what
  dependency resolution resolves (`jumbo resolve`).

An un-absorbed package, an unknown buildId, an unknown commit, and a
major with no records are each typed errors naming the package and the
remediation.

## 2. Pin manifest → vangu Selection / PackageLock (reference adapter)

`pinning-adapter/` is a dependency-free TypeScript package (under the
`@juntai` scope, **not** published; consumed by path) implementing the
reference mapping a downstream Pulumi program uses:

```jsonc
// the Pulumi program's package.json
"dependencies": { "@juntai/jumbo-pinning-adapter": "file:../JumboBuild/pinning-adapter" }
```

```ts
import { readPinManifest, selectionPinFields, packageLockImageFields }
  from "@juntai/jumbo-pinning-adapter";

const manifest = readPinManifest(await run("jumbo", ["pin", pkg, "--by-build-id", id]));

// into the existing Selection path (additive — buildId in the selection contract)
const selection = selectionPinFields(manifest);
//   { contract: "lattice.deployment-selection/v1", buildId, version, commit, fingerprint }

// into the existing PackageLock path (exact digest-pinned image)
const images = packageLockImageFields(manifest);
//   { version, runtimeImage, runtimeImageSourceDigest }
```

| Adapter output | Consumed by | Source |
| --- | --- | --- |
| `contract: "lattice.deployment-selection/v1"` | `Selection.contract` | fixed literal |
| `buildId` | `Selection.buildId` (and `Selection.composition.build_id` parity) | manifest `buildId` |
| `version`, `commit`, `fingerprint` | deployment lock and audit outputs | manifest fields verbatim |
| `runtimeImage` | `PackageLock.runtimeImage` | manifest `imageRef` |
| `runtimeImageSourceDigest` | `PackageLock.runtimeImageSourceDigest` | manifest `imageDigest` |

Nothing else about the Selection or PackageLock changes: the deployment
still owns tenant/application identity, transport, resources, lifecycle
images, and policy. A pinned record with no image makes
`packageLockImageFields` throw `PIN_IMAGE_REQUIRED` — the same failure
`jumbo pin --require-image` produces on the CLI side.

### Exact-image validation parity

The adapter's `EXACT_IMAGE_RE` is copied verbatim from the IaC source
(`LatticeDeployment/src/selection.ts`, `exactImage`):

```
^[^\s@]+@sha256:[a-f0-9]{64}$
```

The `jumbo pin` CLI validates every emitted `imageRef` against the same
rule (implemented as a pure function in `src/pinning/mod.rs`,
`matches_exact_image`), and both sides run the identical accept/reject
vectors in their tests (`pinning-adapter/tests/adapter.test.ts`,
`src/pinning/mod.rs`), pinning three-way parity: CLI ↔ adapter ↔ IaC.

## 3. Pinned reproduction

```
jumbo build --pinned <buildId> [--index ...] [--artifact-dir DIR] [--out DIR]
jumbo reproduce  <buildId> [--package NAME] [--index ...] [--artifact-dir DIR] [--out DIR]
```

Both forms are the same command. Reproducing a past build resolves
exactly from its index record — *the record is the lock* (§2.4 of the
standard):

1. **Resolve** the record by buildId (recorded or derived; searched
   across the index, or within `--package`). An unknown buildId is a
   typed error.
2. **Refuse bootstrap records**: a null `fingerprint` or
   `canonicalExtract` cannot be reproduced; the error says the record
   predates the fingerprint engine and points at the first promoted
   record.
3. **Verify the fingerprint**: recompute
   `sha256(commit + canonical extract)` from the recorded inputs and
   require equality with the recorded fingerprint. A mismatch aborts
   with `PIN_FINGERPRINT_MISMATCH` **before anything is fetched or
   written** — the record is not a faithful lock of its stated inputs.
4. **Materialize the recorded closure**: every internal (`source:
   "index"`) entry of the recorded canonical extract resolves to that
   dependency's record at the **exact recorded version** — never the
   newest of the major — and its artifact is pulled by exact URL through
   the validated github.com-only fetch layer with the recorded SHA-256
   enforced, landing under `<out>/deps/<slug>/<file>`. Dependencies
   whose records published no artifact keep their `deps/<slug>` source
   coordinates (the same rule as `jumbo dedup --deps`) and are reported
   as `sourceOverlayDependencies`. A closure entry whose exact version
   is missing from the index is a typed `closure incomplete` violation —
   append-only indexes keep every old record addressable.
5. **Materialize the own artifact** the same way into `<out>/dist/` —
   the reproduction **produces the recorded digest**.

Artifacts come from the network (the github.com-only layer, identical
locally and in CI) or from a local cache directory
(`--artifact-dir` / `JUMBO_ARTIFACT_DIR`) whose bytes are still verified
against the recorded SHA-256 — the cache is a transport, not a trust
anchor. Third-party registry entries of the extract are covered by the
fingerprint check; no public registry is contacted. The output is a
`jumbo.pinned-reproduction/1` report JSON:

```jsonc
{
  "contract": "jumbo.pinned-reproduction/1",
  "package": "consumer",
  "buildId": "consumer-2.4.0-001",
  "recordedFingerprint": "…64hex…",
  "recomputedFingerprint": "…64hex…",   // equal, else the run aborted
  "fingerprintMatch": true,
  "materializedDependencies": [ /* {package, version, buildId, url, sha256, path} */ ],
  "sourceOverlayDependencies": [],
  "artifact": { "package": "consumer", "sha256": "…64hex…", "path": "dist/consumer-2.4.0-py3-none-any.whl" },
  "outDir": "reproduced"
}
```

Later deployments re-pin against the record associated with that
deployment: resolve its buildId again (records are immutable and stay
addressable forever) and re-run the same flow.

## 4. Error taxonomy

| Code / error | When | Remediation |
| --- | --- | --- |
| `PIN_IMAGE_REQUIRED` | `--require-image` set (or the adapter asked for image fields) and the record has no `imageDigest` | pin a record that produced an image, or deploy artifact-only |
| `EXACT_IMAGE_REQUIRED` | an image reference fails `^[^\s@]+@sha256:[a-f0-9]{64}$` | fix `--image-name` (no whitespace or `@`) |
| `invalid imageDigest` | the record's `imageDigest` is not `sha256:<64hex>` | correct the record at the source (schema violation) |
| `PIN_FINGERPRINT_MISMATCH` | recomputed ≠ recorded fingerprint during reproduction | re-pin the newest record; report the standards violation |
| `closure incomplete` | an internal closure entry's exact version has no index record | report the history violation |
| `cannot be reproduced` | bootstrap record (null fingerprint/extract) | reproduce from the first promoted record |
| `buildId not found` / `commit not found` / `no major-N record` / `no index records` | selection misses | check the buildId/commit, list majors, absorb the package |

## 5. Example

```
$ jumbo pin consumer --by-build-id consumer-2.4.0-001
{
  "contract": "jumbo.deployment-pin/v1",
  "package": "consumer",
  "major": 2,
  "version": "2.4.0",
  "buildId": "consumer-2.4.0-001",
  "buildIdSource": "record",
  "commit": "fedcba9876543210fedcba9876543210fedcba98",
  "imageRef": "ghcr.io/acme/consumer@sha256:dddd…64hex…",
  "imageDigest": "sha256:dddd…64hex…",
  "artifact": {
    "url": "https://github.com/acme/consumer/releases/download/v2.4.0/consumer-2.4.0-py3-none-any.whl",
    "sha256": "eeee…64hex…"
  },
  "fingerprint": "cccc…64hex…",
  "recordRef": { "indexFile": "index/consumer.jsonl", "recordLine": 2 },
  "selector": "buildId consumer-2.4.0-001",
  "timestamp": "2026-09-11T00:00:00Z"
}
```

Contract tests: `tests/pin_cli.rs` and `tests/reproduce_cli.rs` (Rust,
offline, fixture indexes + artifact-dir caches) and
`pinning-adapter/tests/adapter.test.ts` (`node --test`, zero
dependencies).
