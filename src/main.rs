mod cli;
mod language;
mod utils;
mod workspace;

use anyhow::Result;
use clap::Parser;

use cli::{build, Cli, Commands};

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Build(args)) => build::execute(args.action)?,
        Some(Commands::Test) => build::execute_test()?,
        Some(Commands::Format) => build::execute_format()?,
        Some(Commands::Release) => build::execute_release()?,
        Some(Commands::Clear) => build::execute_clear()?,
        Some(Commands::Workspace(args)) => cli::workspace::execute(args)?,
        None => {
            // Default: run build
            build::execute(None)?;
        }
    }

    Ok(())
}
