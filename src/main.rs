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
        Some(Commands::Build(args)) => build::execute(args.action)?,
        Some(Commands::Test) => build::execute_test()?,
        Some(Commands::Format) => build::execute_format()?,
        Some(Commands::Release) => build::execute_release()?,
        Some(Commands::Clean) => build::execute_clean()?,
        Some(Commands::Workspace(args)) => cli::workspace::execute(args)?,
        Some(Commands::Completions(args)) => {
            let mut cmd = Cli::command();
            generate(args.shell, &mut cmd, "jumbo", &mut std::io::stdout());
        }
        None => {
            // Default: run build
            build::execute(None)?;
        }
    }

    Ok(())
}
