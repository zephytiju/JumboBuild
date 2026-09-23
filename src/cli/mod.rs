pub mod build;
pub mod fingerprint;
pub mod lock;
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
    /// Generate shell completion scripts
    Completions(CompletionsArgs),
}

#[derive(Parser)]
pub struct CompletionsArgs {
    /// Target shell
    pub shell: Shell,
}
