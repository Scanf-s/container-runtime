use anyhow::{bail, Context, Result};
use nix::sched::{unshare, CloneFlags};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{fork, ForkResult};
use std::process::ExitCode;

use crate::cli::RunArgs;
use crate::container;

pub fn run(args: RunArgs) -> Result<ExitCode> {
    // Make sure the rootfs exists before we fork.
    if !args.rootfs.is_dir() {
        // Exit early with a clear error message via the bail! macro.
        bail!("rootfs {:?} does not exist or is not a directory", args.rootfs);
    }

    // fork() is marked unsafe in `nix` because it cannot guarantee memory
    // safety across the parent/child split — we acknowledge that here.
    match unsafe { fork() }.context("fork failed")? {
        // Parent process
        ForkResult::Parent { child } => {
            // Wait for the child to finish and inspect its status.
            let status = waitpid(child, None).context("waitpid failed")?;
            match status {

                // Child exited normally — forward its exit code.
                WaitStatus::Exited(_, code) => Ok(ExitCode::from(code as u8)),

                // Child was killed by a signal (ctrl+c, kill, ...) —
                // return 128 + signal number (Linux convention).
                WaitStatus::Signaled(_, sig, _) => Ok(ExitCode::from(128u8 + sig as u8)),
                other => bail!("unexpected wait status: {:?}", other),
            }
        }

        // Child setup process
        ForkResult::Child => {
            if let Err(e) = setup_child(args) {
                eprintln!("container-runtime: setup_child failed: {e:#}");
                std::process::exit(127);
            }
            unreachable!();
        }
    }
}

fn setup_child(args: RunArgs) -> Result<()> {

    // Create new namespace for process isolation
    unshare(CloneFlags::CLONE_NEWPID).context("unshare(CLONE_NEWPID)")?;

    // Create new child process (container process)
    match unsafe { fork() }.context("fork (setup) failed")? {
        // Parent process
        ForkResult::Parent { child } => {
            let status = waitpid(child, None).context("waitpid(child) failed")?;
            let code = match status {
                WaitStatus::Exited(_, c) => c as i32,
                WaitStatus::Signaled(_, sig, _) => 128 + sig as i32,
                other => bail!("unexpected wait status for init: {:?}", other),
            };
            std::process::exit(code);
        }

        // Grand child process (the container environment)
        // Grand child process has the new PID namespace inside because of unshare(CLONE_NEWPID)
        ForkResult::Child => {

            // Run the child. If it fails, exit immediately with code 127
            // instead of returning into parent-side logic.
            if let Err(e) = child_main(args) {
                eprintln!("container-runtime: child_main failed: {e:#}");
                std::process::exit(127);
            }
            unreachable!();
        }
    }
}

fn child_main(args: RunArgs) -> Result<()> {
    // Isolate the container's filesystem from the host using pivot_root.
    container::isolate_fs_pivot(&args.rootfs)?;

    // Replace the current process image with the target command via execvp.
    container::exec_cmd(&args.cmd, &args.args)?;

    unreachable!();
}
