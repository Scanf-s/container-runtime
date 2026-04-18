use anyhow::Result;
use clap::Parser;
use std::process::ExitCode;

mod cli;
mod container;
mod runtime;

fn main() -> Result<ExitCode> {
    // Parse cli command
    let cli = cli::Cli::parse();
    
    match cli.command {
        // Create new container and run command
        cli::Command::Run(args) => runtime::run(args),
    }
}