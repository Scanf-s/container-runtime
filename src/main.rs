use anyhow::Result;
use clap::Parser;
use std::process::ExitCode;

mod cli;
mod container;
mod runtime;

fn main() -> Result<ExitCode> {
    // Parse the CLI arguments.
    let cli = cli::Cli::parse();

    match cli.command {
        // Create a new container and run the requested command inside it.
        cli::Command::Run(args) => runtime::run(args),
    }
}
