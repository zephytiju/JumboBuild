pub mod build;
pub mod dedup;
pub mod fingerprint;
pub mod lock;
pub mod pin;
pub mod promote;
pub mod reproduce;
pub mod resolve;
pub mod workspace;

use clap::{Parser, Subcommand};
use clap_complete::Shell;

/// Jumbo Build - Juntai internal unified build tool
#[derive(Parser)]
#[command(name = "jumbo", version, about = "Juntai internal unified build tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run the build pipeline (default when no subcommand is given)
    Build(build::BuildArgs),
    /// Run tests (= build test)
    Test,
    /// Run formatting (= build format)
    Format,
    /// Run release pipeline (= build release)
    Release,
    /// Clean build artifacts (= build clean)
    Clean,
    /// Manage Jumbo workspace
    #[command(alias = "ws")]
    Workspace(workspace::WorkspaceArgs),
    /// Resolve internal dependencies by declared major against the Jumbo index
    Resolve(resolve::ResolveArgs),
    /// Generate the language lock with jumbo-injected internal sources
    Lock(lock::LockArgs),
    /// Compute sha256(own commit + canonical extract) of the generated lock
    Fingerprint(fingerprint::FingerprintArgs),
    /// Decide build-or-reuse against the index by fingerprint; optionally
    /// materialize recorded artifacts (pull, verify, ingest)
    Dedup(dedup::DedupArgs),
    /// Compute the auto-promotion version bump decision (publish-on-bump
    /// contract for executors)
    Promote(promote::PromoteArgs),
    /// Emit a deployment pin manifest for one promoted build (by buildId,
    /// commit, or latest of major)
    #[command(
        long_about = "Emit a deployment pin manifest (`jumbo.deployment-pin/v1`) for one \
promoted build of one package — the fields a downstream Pulumi program flows \
into the existing vangu Selection and PackageLock path.\n\n\
Exactly one selector is required: --by-build-id (the earliest record whose \
recorded or derived `bootstrap-…` buildId matches), --by-commit (the newest \
record promoted from that commit — a dependency refresh on an unchanged \
commit appends patch records, the newest is the one a deployment of that \
commit pins), or --latest-of-major (the newest record of the major — exactly \
what dependency resolution resolves). Recency is record order, never wall \
clock.\n\n\
The manifest carries buildId (and buildIdSource), commit, version, the exact \
digest-pinned imageRef when the record published an imageDigest, the \
artifact URL + SHA-256, the fingerprint, and the record's index location \
for audit. On stdout as pretty JSON; exit codes: 0 on success, 1 on any \
failure (the typed error on stderr, e.g. PIN_IMAGE_REQUIRED under \
--require-image), 2 on command-line usage errors.\n\n\
See docs/pinning.md for the authoritative field mapping and \
docs/member-onboarding.md for the walkthrough."
    )]
    Pin(pin::PinArgs),
    /// Reproduce the pinned build a buildId refers to (alias: jumbo build
    /// --pinned <BUILD_ID>)
    #[command(
        long_about = "Reproduce the pinned build a buildId refers to — the same run as \
`jumbo build --pinned <BUILD_ID>`.\n\n\
The index record is the lock — no repository lock file is consulted: the \
record is resolved by buildId (recorded, or the documented `bootstrap-…` \
derivation for imported records), the fingerprint \
sha256(commit + canonical extract) is recomputed from the recorded inputs \
and must equal the recorded one (a mismatch aborts before anything is \
fetched or written), every internal dependency of the recorded closure is \
materialized at its exact recorded version under <out>/deps/<slug>/, and \
the own artifact is materialized under <out>/dist/ — reproducing the \
recorded SHA-256.\n\n\
Artifacts come from the network (the validated github.com-only fetch layer, \
identical locally and in CI) or from a local cache directory \
(--artifact-dir / JUMBO_ARTIFACT_DIR) whose bytes are still verified against \
the recorded digest — the cache is a transport, not a trust anchor.\n\n\
Exit codes: 0 on a digest-verified reproduction (the report JSON on stdout), \
1 on any failure (the typed error on stderr), 2 on command-line usage \
errors.\n\n\
See docs/pinning.md for the contract and docs/member-onboarding.md for the \
walkthrough."
    )]
    Reproduce(reproduce::ReproduceArgs),
    /// Generate shell completion scripts
    Completions(CompletionsArgs),
}

#[derive(Parser)]
pub struct CompletionsArgs {
    /// Target shell
    pub shell: Shell,
}
