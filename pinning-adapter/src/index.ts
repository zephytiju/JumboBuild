/**
 * Reference adapter: jumbo deployment-pin manifest → vangu
 * Selection / PackageLock fields
 * (Jumbo Build & Versioning Standard, §3.6 Deployment Pinning Contract).
 *
 * A downstream Pulumi program resolves a build by buildId or commit from
 * its index record and pins the image digest through the existing vangu
 * Selection and PackageLock path. This adapter is the reference mapping:
 * it reads the JSON emitted by `jumbo pin` and produces exactly the fields
 * that path expects — the selection contract's `buildId`, and the exact
 * digest-pinned image string validated by the SAME rule the IaC applies.
 *
 * The package is consumed by path (never published to npm):
 *
 * ```jsonc
 * // in the Pulumi program's package.json
 * "dependencies": { "@juntai/jumbo-pinning-adapter": "file:../JumboBuild/pinning-adapter" }
 * ```
 *
 * Zero runtime dependencies; pure functions only. The exact-image rule is
 * byte-identical to `LatticeDeployment/src/selection.ts` (`exactImage`) —
 * parity is pinned by tests on both sides.
 */

/** The pin manifest contract identifier emitted by `jumbo pin`. */
export const PIN_CONTRACT = "jumbo.deployment-pin/v1";

/**
 * The exact-image validation the vangu Selection enforces.
 *
 * Copied verbatim from `LatticeDeployment/src/selection.ts`
 * (`exactImage`): one or more non-whitespace, non-`@` characters, then
 * `@sha256:` and exactly 64 lowercase hex characters, then end. Tests
 * pin parity with the same vectors the Rust side (`jumbo pin`) checks.
 */
export const EXACT_IMAGE_RE: RegExp = /^[^\s@]+@sha256:[a-f0-9]{64}$/;

/** Error codes — the leading token of every message, as the IaC does. */
export const PIN_IMAGE_REQUIRED = "PIN_IMAGE_REQUIRED";
export const PIN_CONTRACT_MISMATCH = "PIN_CONTRACT_MISMATCH";
export const EXACT_IMAGE_REQUIRED = "EXACT_IMAGE_REQUIRED";

/** The artifact coordinates of a pin, when the record published one. */
export interface PinArtifact {
  readonly url: string;
  readonly sha256: string | null;
}

/** Where in the index the pinned record lives. */
export interface PinRecordRef {
  readonly indexFile: string;
  readonly recordLine: number;
}

/**
 * The `jumbo.deployment-pin/v1` manifest emitted by
 * `jumbo pin <package> [--by-build-id X | --by-commit SHA | --latest-of-major M]`.
 * Field mapping is authoritative in JumboBuild's `docs/pinning.md`.
 */
export interface PinManifest {
  readonly contract: typeof PIN_CONTRACT;
  readonly package: string;
  readonly major: number;
  readonly version: string;
  /** Pinning and reproduction key; derived (`bootstrap-…`) for records imported with a null buildId. */
  readonly buildId: string;
  readonly buildIdSource: "record" | "derived";
  readonly commit: string;
  /** `<image-name>@sha256:<64hex>` when the record published an imageDigest; null otherwise. */
  readonly imageRef: string | null;
  /** The raw image digest from the record (`sha256:<64hex>`), when published. */
  readonly imageDigest: string | null;
  readonly artifact: PinArtifact | null;
  readonly fingerprint: string | null;
  readonly recordRef: PinRecordRef;
  readonly selector: string;
  readonly timestamp: string;
}

/**
 * The Selection-contract fields that come from the index record.
 *
 * The full `lattice.deployment-selection/v1` document carries
 * deployment-owned identity (tenant, application, transport, resources);
 * this is the additive pinning part — `buildId` flows into the selection
 * contract exactly as `Selection.buildId`, and the pinned inputs
 * (`version`, `commit`, `fingerprint`) ride along for the lock and audit
 * outputs. Additive only: existing Selection semantics are unchanged.
 */
export interface SelectionPinFields {
  readonly contract: "lattice.deployment-selection/v1";
  readonly buildId: string;
  readonly version: string;
  readonly commit: string;
  readonly fingerprint: string | null;
}

/**
 * The PackageLock image fields that come from the index record.
 *
 * `runtimeImage` is the exact digest-pinned image string — validated by
 * {@link EXACT_IMAGE_RE}, the same regex the IaC's `validateLock` applies
 * to `lock.runtimeImage`. `runtimeImageSourceDigest` maps from the
 * record's `imageDigest` (the digest of the image the runtime runs, the
 * source of the contribution digest the lock compares against).
 */
export interface PackageLockImageFields {
  readonly version: string;
  readonly runtimeImage: string;
  readonly runtimeImageSourceDigest: string;
}

function fail(code: string, detail: string): never {
  throw new Error(`${code}: ${detail}`);
}

/**
 * Validate an image reference with the IaC's exact-image rule — the same
 * behavior as `exactImage` in `LatticeDeployment/src/selection.ts`.
 */
export function exactImage(image: string): void {
  if (!EXACT_IMAGE_RE.test(image)) {
    fail(EXACT_IMAGE_REQUIRED, `image reference \`${image}\` does not match ${EXACT_IMAGE_RE.source}`);
  }
}

/** Parse and validate a pin manifest document (`jumbo pin` stdout). */
export function readPinManifest(bytes: string): PinManifest {
  let value: unknown;
  try {
    value = JSON.parse(bytes);
  } catch (e) {
    fail(PIN_CONTRACT_MISMATCH, `not valid JSON: ${(e as Error).message}`);
  }
  const manifest = value as Partial<PinManifest>;
  if (
    manifest === null ||
    typeof manifest !== "object" ||
    manifest.contract !== PIN_CONTRACT
  ) {
    fail(
      PIN_CONTRACT_MISMATCH,
      `expected contract \`${PIN_CONTRACT}\`, got \`${
        manifest === null || typeof manifest !== "object" ? String(value) : String(manifest.contract)
      }\``,
    );
  }
  for (const field of [
    "package",
    "version",
    "buildId",
    "commit",
    "selector",
    "timestamp",
  ] as const) {
    if (typeof manifest[field] !== "string" || manifest[field]!.length === 0) {
      fail(PIN_CONTRACT_MISMATCH, `field \`${field}\` must be a non-empty string`);
    }
  }
  if (manifest.buildIdSource !== "record" && manifest.buildIdSource !== "derived") {
    fail(PIN_CONTRACT_MISMATCH, `buildIdSource must be \`record\` or \`derived\``);
  }
  return manifest as PinManifest;
}

/**
 * Map a pin manifest to the Selection-contract fields the deployment
 * pins: `buildId` into the selection contract, the pinned-input identity
 * riding along (additive; no change to Selection semantics).
 */
export function selectionPinFields(manifest: PinManifest): SelectionPinFields {
  return {
    contract: "lattice.deployment-selection/v1",
    buildId: manifest.buildId,
    version: manifest.version,
    commit: manifest.commit,
    fingerprint: manifest.fingerprint,
  };
}

/**
 * Map a pin manifest to the PackageLock image fields: the exact
 * digest-pinned `runtimeImage` (validated by the IaC's exact-image rule)
 * and its source digest.
 *
 * Errors with {@link PIN_IMAGE_REQUIRED} when the pinned record published
 * no image — a deployment that pins a service image must pin a record
 * that produced one (pass `--require-image` to `jumbo pin` to fail there
 * instead).
 */
export function packageLockImageFields(
  manifest: PinManifest,
): PackageLockImageFields {
  if (manifest.imageRef === null || manifest.imageRef.length === 0) {
    fail(
      PIN_IMAGE_REQUIRED,
      `\`${manifest.package}\` ${manifest.version} (buildId \`${manifest.buildId}\`) has no imageRef — the record's build published no service image`,
    );
  }
  exactImage(manifest.imageRef);
  if (manifest.imageDigest === null || manifest.imageDigest.length === 0) {
    fail(PIN_IMAGE_REQUIRED, "imageRef is present but imageDigest is missing");
  }
  return {
    version: manifest.version,
    runtimeImage: manifest.imageRef,
    runtimeImageSourceDigest: manifest.imageDigest,
  };
}
