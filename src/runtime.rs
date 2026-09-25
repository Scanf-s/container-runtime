use crate::cgroups::Cgroup;
use crate::cli::RunArgs;
use crate::container;
use crate::filesystem::PivotRoot;
use crate::ipc::StartupChannels;
use crate::mapping::Mapping;
use anyhow::{Context, Result, bail};
use nix::sched::{CloneFlags, unshare};
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{ForkResult, Gid, Uid, fork, setgid, setuid};
use std::process::ExitCode;

const CPU_PERIOD_US: u64 = 100_000; // 100ms (cgroup default)

pub fn run(args: RunArgs) -> Result<ExitCode> {
    // VALIDATE INPUT ARGUMENTS
    // Make sure the rootfs exists before we fork.
    if !args.rootfs.is_dir() {
        // Exit early with a clear error message via the bail! macro.
        bail!(
            "rootfs {:?} does not exist or is not a directory",
            args.rootfs
        );
    }
    if args.cpus <= 0.0 {
        bail!("--cpus must be positive but got {}", args.cpus);
    }
    let host_cpus = num_cpus::get() as f64;
    if args.cpus > host_cpus {
        bail!(
            "--cpus must be less than or equal to `max_cpus`. you have only {} cpus available",
            host_cpus
        );
    }

    // SETUP AVAILABLE RESOURCES
    // Create a new cgroup to restrict the child's resource usage.
    let new_cgroup: Cgroup = Cgroup::new()?;
    let cpu_quota_us = (args.cpus * CPU_PERIOD_US as f64) as u64;
    new_cgroup.set_cpu_max(cpu_quota_us, CPU_PERIOD_US)?;
    new_cgroup.set_memory_max(args.mem)?;
    new_cgroup.set_pids_max(args.pids)?;

    // Both sides of each channel must exist before fork.
    let channels = StartupChannels::new()?;

    // CREATE SETUP_CHILD PROCESS
    // fork() is marked unsafe in `nix` because it cannot guarantee memory
    // safety across the parent/child split — we acknowledge that here.
    match unsafe { fork() }.context("fork failed")? {
        // Parent process
        ForkResult::Parent { child } => {
            let mut ipc = channels.for_parent();
            let setup_result = (|| -> Result<()> {
                // The child waits for this signal before it can fork PID 1.
                new_cgroup
                    .add_pid(child)
                    .context("add setup child to cgroup")?;
                ipc.signal_cgroup_ready()?;

                // UID/GID maps can be written after the child creates its user namespace.
                ipc.wait_for_user_namespace()?;
                Mapping::new(child, Uid::from_raw(args.uid), Gid::from_raw(args.gid))
                    .map()
                    .context("map container UID/GID")?;
                ipc.signal_mapping_ready()?;
                Ok(())
            })();

            if let Err(error) = setup_result {
                let _ = kill(child, Signal::SIGKILL);
                let _ = waitpid(child, None);
                return Err(error).context("container setup failed; child killed");
            }

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
            let mut ipc = channels.for_child();
            let setup_result = (|| -> Result<()> {
                ipc.wait_for_cgroup()?;
                unshare(CloneFlags::CLONE_NEWUSER).context("unshare(CLONE_NEWUSER)")?;
                ipc.signal_user_namespace_ready()?;
                ipc.wait_for_mapping()?;
                setup_child(args)
            })();

            if let Err(error) = setup_result {
                eprintln!("container-runtime: setup_child failed: {error:#}");
                std::process::exit(127);
            }
            unreachable!();
        }
    }
}

fn setup_child(args: RunArgs) -> Result<()> {
    // Create a new PID namespace and new Network device namespace.
    // After calling unshare(CLONE_NEWPID | CLONE_NEWNET), new child will be created with new PID and network namespace.
    unshare(CloneFlags::CLONE_NEWPID | CloneFlags::CLONE_NEWNET)
        .context("unshare(CLONE_NEWPID | CLONE_NEWNET)")?;

    // Fork the actual container process (PID 1 inside the new namespace).
    match unsafe { fork() }.context("fork (setup) failed")? {
        // Parent process
        ForkResult::Parent { child } => {
            let status = waitpid(child, None).context("waitpid(child) failed")?;
            let code = match status {
                WaitStatus::Exited(_, c) => c,
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
    PivotRoot::new(args.rootfs.clone()).isolate_filesystem()?;

    // set uid and gid
    setgid(Gid::from_raw(0))?;
    setuid(Uid::from_raw(0))?;

    // Replace the current process image with the target command via execvp.
    container::exec_cmd(&args.cmd, &args.args)?;

    unreachable!();
}
