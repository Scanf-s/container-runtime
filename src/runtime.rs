use anyhow::{bail, Context, Result};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{fork, ForkResult};
use std::process::ExitCode;

use crate::cli::RunArgs;
use crate::container;

pub fn run(args: RunArgs) -> Result<ExitCode> {
    // Check whether rootfs exists or not.
    if !args.rootfs.is_dir() {
        // We can exit this program with error message below using bail! macro
        bail!("rootfs {:?} does not exist or is not a directory", args.rootfs);
    }

    // When we call fork system call, we need to ensure that fork cannot guarantee the safety using unsafe keyword
    match unsafe { fork() }.context("fork failed")? {
        // If parent process
        ForkResult::Parent { child } => {
            // Check child process's status
            let status = waitpid(child, None).context("waitpid failed")?;
            match status {

                // If child exited (successfully exited)
                WaitStatus::Exited(_, code) => Ok(ExitCode::from(code as u8)),

                // If child signaled (ctrl+c, kill) -> Return with 128 + signal number (Linux convention)
                WaitStatus::Signaled(_, sig, _) => Ok(ExitCode::from(128u8 + sig as u8)),
                other => bail!("unexpected wait status: {:?}", other),
            }
        }

        // If child process (Container environment)
        ForkResult::Child => {

            // Run child (container) process
            // If child process returns an error, it should not propagate its error message to parent process
            if let Err(e) = child_main(args) {
                eprintln!("container-runtime: child failed: {e:#}");
                std::process::exit(127);
            }
            unreachable!("child_main either execs or exits");
        }
    }
}

fn child_main(args: RunArgs) -> Result<()> {
    // isolate container's filesystem from host's filesystem using chroot
    container::isolate_fs_chroot(&args.rootfs)?;

    // Run command in isolated memory environment using execvp system call
    container::exec_cmd(&args.cmd, &args.args)?;

    unreachable!();
}