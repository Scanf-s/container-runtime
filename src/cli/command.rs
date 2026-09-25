use clap::{Parser, Subcommand};

use super::RunArgs;

/// Top-level parsed CLI command.
#[derive(Parser, Debug)]
#[command(
    name = "container-runtime",
    version,
    about = "A toy container runtime for learning"
)]
pub struct Cli {
    /// The subcommand to dispatch (run, ...).
    #[command(subcommand)]
    pub command: Command,
}

/// Available subcommands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run a command inside an isolated rootfs.
    Run(RunArgs),
}
