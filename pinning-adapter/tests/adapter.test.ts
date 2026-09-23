/**
 * Contract tests for @juntai/jumbo-pinning-adapter.
 *
 * Offline (node:test, zero dependencies). The exact-image vectors are the
 * SAME vectors JumboBuild's Rust tests apply (`src/pinning/mod.rs`,
 * `exact_image_matches_the_iac_regex_semantics`), pinning parity between
 * the adapter regex, the `jumbo pin` CLI, and the IaC source
 * (`LatticeDeployment/src/selection.ts`, `exactImage`).
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  EXACT_IMAGE_RE,
  PIN_CONTRACT,
  PIN_IMAGE_REQUIRED,
  exactImage,
  packageLockImageFields,
  readPinManifest,
  selectionPinFields,
} from "../src/index.ts";

/** A manifest byte-exact in shape to `jumbo pin` stdout (fixture parity). */
const PIN_MANIFEST_JSON = JSON.stringify(
  {
    contract: "jumbo.deployment-pin/v1",
    package: "consumer",
    major: 2,
    version: "2.4.0",
    buildId: "consumer-2.4.0-001",
    buildIdSource: "record",
    commit: "fedcba9876543210fedcba9876543210fedcba98",
    imageRef: `ghcr.io/acme/consumer@sha256:${"d".repeat(64)}`,
    imageDigest: `sha256:${"d".repeat(64)}`,
    artifact: {
      url: "https://github.com/acme/consumer/releases/download/v2.4.0/consumer-2.4.0-py3-none-any.whl",
      sha256: "e".repeat(64),
    },
    fingerprint: "c".repeat(64),
    recordRef: { indexFile: "index/consumer.jsonl", recordLine: 2 },
    selector: "buildId consumer-2.4.0-001",
    timestamp: "2026-09-11T00:00:00Z",
  },
  null,
  2,
);

const digest = `sha256:${"a".repeat(64)}`;

test("readPinManifest parses the jumbo pin contract", () => {
  const manifest = readPinManifest(PIN_MANIFEST_JSON);
  assert.equal(manifest.contract, PIN_CONTRACT);
  assert.equal(manifest.buildId, "consumer-2.4.0-001");
  assert.equal(manifest.buildIdSource, "record");
  assert.equal(manifest.artifact?.sha256, "e".repeat(64));
  assert.equal(manifest.recordRef.recordLine, 2);
});

test("readPinManifest rejects foreign contracts and malformed documents", () => {
  const cases: string[] = [
    JSON.stringify({ contract: "other.contract/v1" }),
    "not json",
    JSON.stringify({ contract: PIN_CONTRACT, buildId: "" }),
  ];
  for (const bytes of cases) {
    assert.throws(() => readPinManifest(bytes), /PIN_CONTRACT_MISMATCH/);
  }
});

test("selectionPinFields maps the selection contract fields incl. buildId", () => {
  const manifest = readPinManifest(PIN_MANIFEST_JSON);
  const selection = selectionPinFields(manifest);
  assert.equal(selection.contract, "lattice.deployment-selection/v1");
  assert.equal(selection.buildId, manifest.buildId);
  assert.equal(selection.version, manifest.version);
  assert.equal(selection.commit, manifest.commit);
  assert.equal(selection.fingerprint, manifest.fingerprint);
});

test("packageLockImageFields produces the exact digest-pinned image", () => {
  const manifest = readPinManifest(PIN_MANIFEST_JSON);
  const lock = packageLockImageFields(manifest);
  assert.equal(lock.version, "2.4.0");
  assert.equal(lock.runtimeImage, manifest.imageRef);
  assert.equal(lock.runtimeImageSourceDigest, manifest.imageDigest);
  // The produced string passes the IaC validation itself.
  exactImage(lock.runtimeImage);
});

test("packageLockImageFields errors when the record published no image", () => {
  const manifest = readPinManifest(
    JSON.stringify({
      ...JSON.parse(PIN_MANIFEST_JSON),
      imageRef: null,
      imageDigest: null,
    }),
  );
  assert.throws(() => packageLockImageFields(manifest), new RegExp(PIN_IMAGE_REQUIRED));
});

test("derived bootstrap buildIds map through unchanged", () => {
  const manifest = readPinManifest(
    JSON.stringify({
      ...JSON.parse(PIN_MANIFEST_JSON),
      buildId: "bootstrap-0123456789ab",
      buildIdSource: "derived",
      imageRef: null,
      imageDigest: null,
      fingerprint: null,
      artifact: null,
    }),
  );
  const selection = selectionPinFields(manifest);
  assert.equal(selection.buildId, "bootstrap-0123456789ab");
  assert.equal(selection.fingerprint, null);
  assert.throws(() => packageLockImageFields(manifest), new RegExp(PIN_IMAGE_REQUIRED));
});

/**
 * Regex parity with the IaC source. The literal below is copied from
 * `LatticeDeployment/src/selection.ts` a second time — proving the
 * adapter's copy equals the deployed rule character for character.
 */
const IAC_SOURCE_RE: RegExp = /^[^\s@]+@sha256:[a-f0-9]{64}$/;

test("EXACT_IMAGE_RE is byte-identical to the IaC source regex", () => {
  assert.equal(EXACT_IMAGE_RE.source, IAC_SOURCE_RE.source);
  assert.equal(EXACT_IMAGE_RE.flags, IAC_SOURCE_RE.flags);
});

test("exact-image vectors match the Rust CLI vectors", () => {
  const good: string[] = [
    `ghcr.io/acme/pkg@${digest}`,
    `localhost:5000/pkg@${digest}`,
    `pkg@${digest}`,
    // A tag before the digest still matches: `[^\s@]+` is any
    // whitespace-free, `@`-free name — exactly the IaC's rule.
    `ghcr.io/acme/pkg:v1@${digest}`,
  ];
  for (const image of good) {
    assert.ok(EXACT_IMAGE_RE.test(image), `should match: ${image}`);
    assert.doesNotThrow(() => exactImage(image));
  }

  const bad: string[] = [
    `ghcr.io/acme/pkg@${digest} `,
    ` ghcr.io/acme/pkg@${digest}`,
    `ghcr.io/acme/pkg@sha256:${"A".repeat(64)}`,
    `ghcr.io/acme/pkg@sha256:${"a".repeat(63)}`,
    `ghcr.io/acme/pkg@sha256:${"a".repeat(65)}`,
    `ghcr.io/acme/pkg@${"a".repeat(64)}`,
    `ghcr.io/acme@@${digest}`,
    `@${digest}`,
    `ghcr.io/acme/pkg`,
    ``,
  ];
  for (const image of bad) {
    assert.ok(!EXACT_IMAGE_RE.test(image), `should reject: \`${image}\``);
    assert.throws(() => exactImage(image), /EXACT_IMAGE_REQUIRED/);
  }
});
