//! Pinning error taxonomy (deployment pinning contract,
//! Jumbo Build & Versioning Standard, §3.6).
//!
//! Every failure a pin or a pinned reproduction can hit has a typed
//! variant with an actionable, self-contained message: the deployment
//! side must never see a bare string mismatch without the package,
//! buildId, and remediation step.

use thiserror::Error;

/// Everything that can go wrong while producing a pin manifest or
/// reproducing a pinned build.
#[derive(Debug, Error)]
pub enum PinningError {
    /// The package has no index records at all (not absorbed).
    #[error(
        "cannot pin `{package}`: the package has no index records; an un-absorbed internal \
            package cannot be pinned — absorb it into JumboIndex first (a pipeline appending \
            records via jumbo-publish)"
    )]
    PackageNotIndexed { package: String },

    /// No record of the package carries the requested buildId.
    #[error(
        "buildId `{build_id}` not found for `{package}`: no record (recorded or derived \
            bootstrap buildId) matches; list the history with `jumbo pin {package} \
            --latest-of-major <M>` — the manifest carries the package's buildIds — or \
            resolve the buildId from the deployment record"
    )]
    BuildIdNotFound { package: String, build_id: String },

    /// No record of any package carries the requested buildId (the
    /// whole-index search, no `--package` constraint).
    #[error(
        "buildId `{build_id}` not found in the index: no record of any package (recorded or \
            derived bootstrap buildId) matches; a buildId is carried by its index record and \
            by every `jumbo pin <package> --latest-of-major <M>` manifest of the package — \
            resolve it from the deployment record or the release notes"
    )]
    BuildIdNotFoundAnywhere { build_id: String },

    /// No record of the package carries the requested commit.
    #[error(
        "commit `{commit}` not found for `{package}`: no record of the package was promoted \
            from this commit"
    )]
    CommitNotFound { package: String, commit: String },

    /// The requested major has no records.
    #[error("no major-{major} record for `{package}` (recorded majors: {available})")]
    MajorNotRecorded {
        package: String,
        major: u64,
        available: String,
    },

    /// `--require-image` was set but the record published no image digest.
    #[error(
        "PIN_IMAGE_REQUIRED: `{package}` {version} (buildId `{build_id}`) has no imageDigest \
            — the record's build published no service image; pin a record that produced an image \
            or drop --require-image for an artifact-only deployment"
    )]
    ImageRequired {
        package: String,
        version: String,
        build_id: String,
    },

    /// The record's imageDigest is malformed.
    #[error(
        "invalid imageDigest for `{package}` {version}: `{value}` is not \
            `sha256:<64 lowercase hex>`; the record violates the index schema and must be \
            corrected at the source"
    )]
    InvalidImageDigest {
        package: String,
        version: String,
        value: String,
    },

    /// The constructed image reference fails the exact-image validation the
    /// downstream IaC applies (regex parity with the Selection contract).
    #[error(
        "EXACT_IMAGE_REQUIRED: image reference `{value}` does not match \
            `^[^\\s@]+@sha256:[a-f0-9]{{64}}$` — the same validation the vangu Selection \
            enforces; pass --image-name without whitespace or '@' characters"
    )]
    InvalidImageRef { value: String },

    /// A commit selector that is not a full 40-hex SHA.
    #[error("invalid commit `{value}`: --by-commit needs a full 40-hex commit SHA")]
    InvalidCommit { value: String },

    /// The record cannot be reproduced (bootstrap records).
    #[error(
        "buildId `{build_id}` of `{package}` cannot be reproduced: {reason}. Bootstrap \
            records predate the fingerprint engine; reproduce from the first promoted record \
            of the package instead"
    )]
    ReproduceUnavailable {
        package: String,
        build_id: String,
        reason: String,
    },

    /// The recomputed fingerprint differs from the recorded one.
    #[error(
        "PIN_FINGERPRINT_MISMATCH: reproduced `{package}` {version} (buildId `{build_id}`) \
            recomputed fingerprint {recomputed} but the record pins {recorded}; the record is \
            not a faithful lock of its stated inputs — aborting before anything is consumed. \
            Re-pin the newest record of the major or report the standards violation"
    )]
    FingerprintMismatch {
        package: String,
        version: String,
        build_id: String,
        recorded: String,
        recomputed: String,
    },

    /// An internal dependency of the recorded closure has no record at the
    /// exact recorded version.
    #[error(
        "closure incomplete for `{package}` {version}: the recorded closure resolves \
            `{dep}` at {version} but the index has no record of that exact version (available: \
            {available}); an append-only index must keep every old record addressable — report \
            the history violation"
    )]
    ClosureIncomplete {
        package: String,
        version: String,
        dep: String,
        available: String,
    },

    /// The recorded canonical extract does not round-trip the extract format.
    #[error("invalid canonicalExtract for `{package}` {version}: {reason}")]
    InvalidExtract {
        package: String,
        version: String,
        reason: String,
    },

    /// An artifact fetch/verification failure of the materializer (J4 layer).
    #[error(transparent)]
    Materialize(#[from] crate::dedup::MaterializeError),

    /// Fingerprint-engine failure while recomputing.
    #[error(transparent)]
    Fingerprint(#[from] crate::fingerprint::FingerprintError),
}
