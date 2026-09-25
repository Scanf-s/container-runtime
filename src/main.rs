use anyhow::Result;
use clap::Parser;
use std::process::ExitCode;

mod cgroups;
mod cli;
mod container;
mod filesystem;
mod mapping;
mod runtime;

#[cfg(target_os = "linux")]
fn main() -> Result<ExitCode> {
    // Parse the CLI arguments.
    let cli = cli::Cli::parse();

    match cli.command {
        // Create a new container and run the requested command inside it.
        cli::Command::Run(args) => runtime::run(args),
    }
}
