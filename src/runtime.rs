use std::os::fd::AsRawFd;
use anyhow::{bail, Context, Result};
use nix::sched::{unshare, CloneFlags};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{ForkResult, Gid, Uid, fork, pipe, read, write, setgid, setuid};
use std::process::ExitCode;
use nix::sys::signal::{kill, Signal};
use crate::cgroup::Cgroup;
use crate::cli::RunArgs;
use crate::container;
use crate::mapping::Mapping;

const CPU_PERIOD_US: u64 = 100_000; // 100ms (cgroup default)

pub fn run(args: RunArgs) -> Result<ExitCode> {

    // PIPELINES
    let (c_read_fd, c_write_fd) = pipe()?;
    let (u_read_fd, u_write_fd) = pipe()?;
    let (m_read_fd, m_write_fd) = pipe()?;

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
            drop(c_read_fd);
            drop(u_write_fd);
            drop(m_read_fd);

            // Block the child until cgroup registration is complete, so the
            // grandchild (the actual workload) is born inside the cgroup.
            // pipe() is a FIFO buffer maintained by the kernel; it returns two
            // file descriptors: c_read_fd (for the reader) and c_write_fd (for the writer).
            if let Err(e) = new_cgroup.add_pid(child) {
                // if failed to create new group, kill child process before return an error
                let _ = kill(child, Signal::SIGKILL);
                let _ = waitpid(child, None);
                return Err(e).context("add_pid failed; child killed");
            }
            // signal the child that it can proceed
            if let Err(e) = write(&c_write_fd, &[1u8]) {
                let _ = kill(child, Signal::SIGKILL);
                let _ = waitpid(child, None);
                return Err(e).context("failed to write in cgroup pipe buffer; child killed");
            }
            drop(c_write_fd);

            // Wait until the child has created a new user namespace.
            let mut buf = [0u8; 1];
            if let Err(e) = read(u_read_fd.as_raw_fd(), &mut buf) {
                let _ = kill(child, Signal::SIGKILL);
                let _ = waitpid(child, None);
                return Err(e).context("child closed pipe before creating new user namespace; child killed");
            }
            drop(u_read_fd);

            // Map container root to the requested host UID/GID.
            let new_mapping: Mapping = Mapping::new(
                child,
                Uid::from_raw(args.uid),
                Gid::from_raw(args.gid),
            );
            if let Err(e) = new_mapping.map() {
                let _ = kill(child, Signal::SIGKILL);
                let _ = waitpid(child, None);
                return Err(e).context("user namespace mapping failed; child killed");
            }
            write(&m_write_fd, &[1u8])?; // signal the child that it can proceed
            drop(m_write_fd);

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
            drop(c_write_fd);
            drop(u_read_fd);
            drop(m_write_fd);
            
            // wait until parent finished to create new cgroup
            let mut buf = [0u8; 1];
            let check_cgroup = read(c_read_fd.as_raw_fd(), &mut buf);
            if check_cgroup != Ok(1) {
                eprintln!("parent closed cgroup step pipe before completing it");
                std::process::exit(127);
            }
            drop(c_read_fd);

            // flush buf to reuse in mapping step
            buf = [0u8; 1];

            // Create a new user namespace
            // setup_child is in new user namespace
            if let Err(e) = unshare(CloneFlags::CLONE_NEWUSER).context("unshare(CLONE_NEWUSER)") {
                eprintln!("failed to execute unshare(CLONE_NEWUSER): {e:#}");
                std::process::exit(127);
            }
            // signal to notify to the parent about finishing unshare(CLONE_NEWUSER)
            if let Err(e) = write(&u_write_fd, &[1u8]) {
                eprintln!("failed to write buffer in u_write_fd: {e:#}");
                std::process::exit(127);
            }
            drop(u_write_fd);
            
            // Wait until mapping step completed from the parent process
            let check_mapping = read(m_read_fd.as_raw_fd(), &mut buf);
            if check_mapping != Ok(1) {
                eprintln!("parent closed mapping pipe before completing uid/gid mapping");
                std::process::exit(127);
            }
            drop(m_read_fd);

            // Run setup_child
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
    // After calling unshare(CLONE_NEWPID), new child will be created with new PID namespace.
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

    // set uid and gid
    setgid(Gid::from_raw(0))?;
    setuid(Uid::from_raw(0))?;

    // Replace the current process image with the target command via execvp.
    container::exec_cmd(&args.cmd, &args.args)?;

    unreachable!();
}
