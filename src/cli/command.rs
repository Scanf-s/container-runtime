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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn run_args(args: &[&str]) -> RunArgs {
        let cli = Cli::try_parse_from(args).unwrap();
        let Command::Run(run) = cli.command;
        run
    }

    #[test]
    fn parses_run_with_default_limits_and_ids() {
        let args = run_args(&["container-runtime", "run", "./rootfs", "/bin/sh"]);
        assert_eq!(args.rootfs, PathBuf::from("./rootfs"));
        assert_eq!(args.cmd, "/bin/sh");
        assert!(args.args.is_empty());
        assert_eq!(args.cpus, 1.0);
        assert_eq!(args.mem, 512 * 1024 * 1024);
        assert_eq!(args.pids, 1024);
        assert_eq!(args.uid, 0);
        assert_eq!(args.gid, 0);
    }

    #[test]
    fn parses_limits_ids_and_hyphenated_command_arguments() {
        let args = run_args(&[
            "container-runtime",
            "run",
            "--cpus",
            "0.5",
            "--mem",
            "1048576",
            "--pids",
            "64",
            "--uid",
            "1000",
            "--gid",
            "1001",
            "./rootfs",
            "/bin/sh",
            "-c",
            "echo hello",
        ]);
        assert_eq!(args.cpus, 0.5);
        assert_eq!(args.mem, 1_048_576);
        assert_eq!(args.pids, 64);
        assert_eq!(args.uid, 1000);
        assert_eq!(args.gid, 1001);
        assert_eq!(args.args, ["-c", "echo hello"]);
    }

    #[test]
    fn rejects_missing_run_inputs_and_invalid_numeric_limits() {
        assert!(Cli::try_parse_from(["container-runtime"]).is_err());
        assert!(Cli::try_parse_from(["container-runtime", "run", "./rootfs"]).is_err());
        assert!(
            Cli::try_parse_from([
                "container-runtime",
                "run",
                "--mem",
                "invalid",
                "./rootfs",
                "/bin/sh",
            ])
            .is_err()
        );
    }
}
