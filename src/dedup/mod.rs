//! Jumbo dedup decision and artifact materialization
//! (Jumbo Build & Versioning Standard, §2.3–§2.4, §3.4).
//!
//! The fingerprint of the project about to build is compared against the
//! index history for the same package name:
//! - **hit** — the build is a duplicate: reuse the record's artifact.
//!   `jumbo dedup --materialize` pulls the recorded release asset by exact
//!   URL through the validated github.com-only layer, verifies the
//!   recorded SHA-256, and places it in the project's `dist/` directory,
//!   so the repeated run consumes the recorded bytes with zero source
//!   rebuilds;
//! - **miss** — build from source.
//!
//! `jumbo dedup --deps` materializes the recorded artifacts of a
//! manifest's internal dependencies in place of their source overlays:
//! Python wheels enter the uv build as direct wheel sources under
//! `deps/<slug>/`, Node tarballs via `file:` sources — never a registry
//! protocol. Null fingerprints (bootstrap records) never match.
//!
//! Boundary: the decision and the pull only — no version bumping, no
//! publication, no index writes, no rebuild orchestration.

pub mod decision;
pub mod error;
pub mod fetch;
pub mod ingest;

pub use decision::{decide, DedupAction, DedupDecision, MatchedRecord};
pub use error::MaterializeError;
pub use fetch::{download_artifact, validate_artifact_url, ArtifactUrl};
pub use ingest::{
    load_artifacts_marker, materialize_dependency_artifacts, materialize_self_artifact,
    recorded_sha256, sha256_of_file, stage_and_verify, verify_sha256, ArtifactKind,
    ArtifactProvider, ArtifactsMarker, MaterializedArtifact, ARTIFACTS_MARKER_FORMAT,
    DEFAULT_DIST_DIR,
};
