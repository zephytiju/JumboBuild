#!/usr/bin/env bash
# Verify that the image digest observed in the registry matches the digest a
# build push reported. This is the Jumbo Build & Versioning Standard §3.7
# failure behavior for image provenance: "digest mismatch aborts the build".
#
# The jumbo-publish reusable workflow runs this immediately after pushing the
# service image to GHCR and BEFORE appending the index record, so a record can
# never carry an image digest that the registry does not confirm (the record's
# imageDigest field is what deployment pinning consumes, standard §3.6).
#
# Usage:
#   verify_image_digest.sh --image <registry/repo:tag> --expected <sha256:...>
#
# Exit codes:
#   0  the registry digest exists, is well-formed, and equals the expected one
#   1  mismatch, malformed digest, missing arguments, or registry failure
#
# Credentials never enter this script: registry access reuses the caller's
# already-authenticated docker session. Digests are public values; nothing
# secret is read, printed, or passed through.
set -euo pipefail

usage() {
  echo "usage: $0 --image <registry/repo:tag> --expected <sha256:hex>" >&2
  exit 1
}

image=""
expected=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --image)
      [[ $# -ge 2 ]] || usage
      image="$2"
      shift 2
      ;;
    --expected)
      [[ $# -ge 2 ]] || usage
      expected="$2"
      shift 2
      ;;
    *) usage ;;
  esac
done

abort() {
  # The ::error workflow command is only processed from stdout; the plain
  # text goes to stderr for humans reading the failing step.
  echo "::error title=jumbo image digest verification::${1}"
  echo "ABORT: ${1}" >&2
  exit 1
}

[[ -n "$image" && -n "$expected" ]] || usage

# Registry host validation before any registry contact: the standard names
# GHCR as the only image registry (§3.4), so the reference must be a ghcr.io
# path. Any other host — an arbitrary registry, localhost/loopback, a private
# or reserved address, or a bare repository path — is refused outright; this
# script never inspects anything it was not explicitly given for GHCR.
registry="${image%%/*}"
if [[ "$registry" != "ghcr.io" || "$image" == "$registry" ]]; then
  abort "image ${image} is not a ghcr.io reference; the standard allows service images on ghcr.io only"
fi

# Validate the expected digest before any registry contact: anything that is
# not a plain sha256:<64 hex> digest is a malformed provenance value, never a
# verification pass.
if [[ ! "$expected" =~ ^sha256:[0-9a-f]{64}$ ]]; then
  abort "expected digest is not a valid sha256 image digest (${expected}); refusing to record it"
fi

if ! inspect="$(docker buildx imagetools inspect "$image" 2>&1)"; then
  abort "registry inspection failed for ${image}: ${inspect}"
fi

observed="$(printf '%s\n' "$inspect" | sed -n 's/^Digest:[[:space:]]*//p' | head -n 1)"
if [[ -z "$observed" ]]; then
  abort "registry inspection returned no digest for ${image}"
fi
if [[ ! "$observed" =~ ^sha256:[0-9a-f]{64}$ ]]; then
  abort "registry digest for ${image} is malformed (${observed})"
fi

if [[ "$observed" != "$expected" ]]; then
  abort "digest mismatch for ${image}: registry has ${observed}, the build pushed ${expected} - aborting before index append (standard §3.7)"
fi

echo "digest verified for ${image}: ${observed}"
