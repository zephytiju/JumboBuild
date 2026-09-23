use anyhow::Result;
use clap::{CommandFactory, Parser};
use clap_complete::generate;
use clap_complete::CompleteEnv;
use jumbo_build::cli;

use cli::{build, Cli, Commands};

fn main() -> Result<()> {
    // Dynamic shell completion: activated via `COMPLETE=<shell> jumbo`.
    // When the env var is set, this generates the registration script or
    // returns completion candidates and exits. Otherwise it is a no-op.
    CompleteEnv::with_factory(Cli::command).complete();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Build(args)) => build::execute(&args)?,
        Some(Commands::Test) => build::execute_test()?,
        Some(Commands::Format) => build::execute_format()?,
        Some(Commands::Release) => build::execute_release()?,
        Some(Commands::Clean) => build::execute_clean()?,
        Some(Commands::Workspace(args)) => cli::workspace::execute(args)?,
        Some(Commands::Resolve(args)) => cli::resolve::execute(args)?,
        Some(Commands::Lock(args)) => cli::lock::execute(args)?,
        Some(Commands::Fingerprint(args)) => cli::fingerprint::execute(args)?,
        Some(Commands::Dedup(args)) => cli::dedup::execute(args)?,
        Some(Commands::Promote(args)) => cli::promote::execute(args)?,
        Some(Commands::Pin(args)) => cli::pin::execute(args)?,
        Some(Commands::Reproduce(args)) => cli::reproduce::execute(args)?,
        Some(Commands::Completions(args)) => {
            let mut cmd = Cli::command();
            generate(args.shell, &mut cmd, "jumbo", &mut std::io::stdout());
        }
        None => {
            // Default: run build (an unpinned build of the current project)
            build::execute(&cli::build::BuildArgs {
                action: None,
                pinned: None,
                index: None,
                artifact_dir: None,
                out: None,
            })?;
        }
    }

    Ok(())
}
