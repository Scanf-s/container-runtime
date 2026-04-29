use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Top-level parsed CLI command.
#[derive(Parser, Debug)]
#[command(name = "container-runtime", version, about = "A toy container runtime for learning")]
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

#[derive(Parser, Debug)]
pub struct RunArgs {
    /// Path to the rootfs directory (e.g. ./rootfs).
    pub rootfs: PathBuf,

    /// Number of CPUs (e.g. 0.5 for half a core, 2.0 for two cores).
    #[arg(long, default_value_t = 1.0)]
    pub cpus: f64,

    /// Memory limit (bytes)
    #[arg(long, default_value_t = 512 * 1024 * 1024)]
    pub mem: u64,

    /// Maximum number of tasks (processes and threads).
    #[arg(long, default_value_t = 1024)]
    pub pids: u64,

    /// Command to execute inside the container.
    pub cmd: String,

    /// Arguments to pass to the command.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}
