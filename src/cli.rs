use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Entire parsed command struct
#[derive(Parser, Debug)]
#[command(name = "container-runtime", version, about = "A toy container runtime for learning")]
pub struct Cli {

    /// Subcommand (run, ...)
    #[command(subcommand)]
    pub command: Command,

}

/// Subcommand struct
#[derive(Subcommand, Debug)]
pub enum Command {

    /// Subcommands to run inside an isolated rootfs.
    Run(RunArgs),

}

#[derive(Parser, Debug)]
pub struct RunArgs {
    /// Path to the rootfs directory (e.g. ./rootfs).
    pub rootfs: PathBuf,

    /// Command to execute inside the container.
    pub cmd: String,

    /// Arguments to pass to the command.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}