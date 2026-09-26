# Fixture image for the jumbo-publish GHCR digest-provenance CI check
# (.github/workflows/jumbo-publish-image-check.yml). The image carries no
# payload: it exists so the same build → push → digest verification → index
# record path the reusable jumbo-publish workflow runs for image-producing
# packages is exercised against the real GHCR registry in CI.
FROM scratch

LABEL org.opencontainers.image.title="jumbo-publish image sample"
LABEL org.opencontainers.image.description="Fixture image exercising the jumbo-publish GHCR digest provenance path: build, push, verify the digest (standard 3.7), and capture it in a JumboIndex record shape."
