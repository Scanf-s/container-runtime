use std::os::fd::AsRawFd;
use anyhow::{bail, Context, Result};
use nix::sched::{unshare, CloneFlags};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{fork, ForkResult, pipe, write, read};
use std::process::ExitCode;
use nix::sys::signal::{kill, Signal};
use crate::cgroup::Cgroup;
use crate::cli::RunArgs;
use crate::container;

const CPU_PERIOD_US: u64 = 100_000; // 100ms (cgroup default)

pub fn run(args: RunArgs) -> Result<ExitCode> {
    // VALIDATE INPUT ARGUMENTS
    // Make sure the rootfs exists before we fork.
    if !args.rootfs.is_dir() {
        // Exit early with a clear error message via the bail! macro.
        bail!("rootfs {:?} does not exist or is not a directory", args.rootfs);
    }
    if args.cpus <= 0.0 {
        bail!("--cpus must be positive but got {}", args.cpus);
    }
    let host_cpus = num_cpus::get() as f64;
    if args.cpus > host_cpus {
        bail!("--cpus must be less than or equal to `max_cpus`. you have only {} cpus available", host_cpus);
    }

    // SETUP AVAILABLE RESOURCES
    // Create a new cgroup to restrict the child's resource usage.
    let new_cgroup: Cgroup = Cgroup::new()?;
    let (read_fd, write_fd) = pipe()?;
    let cpu_quota_us = (args.cpus * CPU_PERIOD_US as f64) as u64;
    new_cgroup.set_cpu_max(cpu_quota_us, CPU_PERIOD_US)?;
    new_cgroup.set_memory_max(args.mem)?;
    new_cgroup.set_pids_max(args.pids)?;

    // CREATE SETUP_CHILD PROCESS
    // fork() is marked unsafe in `nix` because it cannot guarantee memory
    // safety across the parent/child split — we acknowledge that here.
    match unsafe { fork() }.context("fork failed")? {
        // Parent process
        ForkResult::Parent { child } => {
            // Block the child until cgroup registration is complete, so the
            // grandchild (the actual workload) is born inside the cgroup.
            // pipe() is a FIFO buffer maintained by the kernel; it returns two
            // file descriptors: read_fd (for the reader) and write_fd (for the writer).
            drop(read_fd); // parent never reads from the pipe
            if let Err(e) = new_cgroup.add_pid(child) {
                // if failed to create new group, kill child process before return an error
                let _ = kill(child, Signal::SIGKILL);
                let _ = waitpid(child, None);
                return Err(e).context("add_pid failed; child killed");
            }
            write(&write_fd, &[1u8])?; // signal the child that it can proceed
            drop(write_fd);

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
            drop(write_fd); // child never writes to the pipe
            let mut buf = [0u8; 1];
            read(read_fd.as_raw_fd(), &mut buf)?; // wait for the parent's signal
            drop(read_fd);
            if let Err(e) = setup_child(args) {
                eprintln!("container-runtime: setup_child failed: {e:#}");
                std::process::exit(127);
            }
            unreachable!();
        }
    }
}

fn setup_child(args: RunArgs) -> Result<()> {

    // Create a new PID namespace for process isolation.
    unshare(CloneFlags::CLONE_NEWPID).context("unshare(CLONE_NEWPID)")?;

    // Fork the actual container process (PID 1 inside the new namespace).
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

        // Grandchild process (the container environment).
        // It runs inside the new PID namespace thanks to the earlier unshare(CLONE_NEWPID).
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
