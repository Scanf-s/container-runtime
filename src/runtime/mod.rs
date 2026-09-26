use crate::cgroups::Cgroup;
use crate::cli::RunArgs;
use crate::container;
use crate::filesystem::PivotRoot;
use crate::ipc::StartupChannels;
use crate::user::Mapping;
use anyhow::{Context, Result, bail};
use nix::sched::{CloneFlags, unshare};
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{ForkResult, Gid, Pid, Uid, fork, setgid, setuid};
use std::process::ExitCode;

pub struct Runtime {
    args: RunArgs,
    cgroup: Cgroup,
    channels: Option<StartupChannels>,
}

impl Runtime {
    pub fn new(args: RunArgs) -> Result<Self> {
        // validate passed argument from CLI
        let settings = args.validate()?;

        let cgroup = Cgroup::new()?;
        cgroup.configure(&settings)?;

        // ipc setup for the parent and the child process communication
        let channels = StartupChannels::new()?;

        Ok(Self {
            args,
            cgroup,
            channels: Some(channels),
        })
    }

    pub fn run(mut self) -> Result<ExitCode> {
        match unsafe { fork() }.context("fork failed")? {
            ForkResult::Parent { child } => self.run_parent(child),
            ForkResult::Child => self.run_setup_process(),
        }
    }

    fn run_parent(&mut self, child: Pid) -> Result<ExitCode> {
        if let Err(error) = self.setup_parent(child) {
            let _ = kill(child, Signal::SIGKILL);
            let _ = waitpid(child, None);
            return Err(error).context("container setup failed; child killed");
        }

        let status = waitpid(child, None).context("waitpid failed")?;
        Ok(ExitCode::from(exit_status(status)?))
    }

    fn setup_parent(&mut self, child: Pid) -> Result<()> {
        let channels = self
            .channels
            .take()
            .context("startup channels already used")?;
        let mut ipc = channels.for_parent();

        // The setup process waits until it belongs to the cgroup.
        self.cgroup
            .add_pid(child)
            .context("add setup child to cgroup")?;
        ipc.signal_cgroup_ready()?;

        // UID/GID maps can be written after the child creates its user namespace.
        ipc.wait_for_user_namespace()?;
        Mapping::new(
            child,
            Uid::from_raw(self.args.uid),
            Gid::from_raw(self.args.gid),
        )
        .map()
        .context("map container UID/GID")?;
        ipc.signal_mapping_ready()?;
        Ok(())
    }

    fn run_setup_process(&mut self) -> ! {
        if let Err(error) = self.setup_process() {
            eprintln!("container-runtime: setup process failed: {error:#}");
            std::process::exit(127);
        }
        unreachable!("setup process returned without exiting");
    }

    fn setup_process(&mut self) -> Result<()> {
        let channels = self
            .channels
            .take()
            .context("startup channels already used")?;
        let mut ipc = channels.for_child();
        ipc.wait_for_cgroup()?;

        // call unshare to isolate the user namespace
        unshare(CloneFlags::CLONE_NEWUSER).context("unshare(CLONE_NEWUSER)")?;
        ipc.signal_user_namespace_ready()?;

        ipc.wait_for_mapping()?;
        self.launch_init()
    }

    fn launch_init(&self) -> Result<()> {
        // The next fork places container init at PID 1 in the new PID namespace.
        unshare(CloneFlags::CLONE_NEWPID | CloneFlags::CLONE_NEWNET)
            .context("unshare(CLONE_NEWPID | CLONE_NEWNET)")?;

        match unsafe { fork() }.context("fork (setup) failed")? {
            ForkResult::Parent { child } => {
                let status = waitpid(child, None).context("waitpid(child) failed")?;
                std::process::exit(i32::from(exit_status(status)?));
            }
            ForkResult::Child => {
                if let Err(error) = self.run_init() {
                    eprintln!("container-runtime: container init failed: {error:#}");
                    std::process::exit(127);
                }
                unreachable!("container init returned without executing command");
            }
        }
    }

    fn run_init(&self) -> Result<()> {
        // file system isolation
        PivotRoot::new(self.args.rootfs.clone()).isolate_filesystem()?;
        setgid(Gid::from_raw(0))?;
        setuid(Uid::from_raw(0))?;
        container::exec_cmd(&self.args.cmd, &self.args.args)
    }
}

fn exit_status(status: WaitStatus) -> Result<u8> {
    match status {
        WaitStatus::Exited(_, code) => Ok(code as u8),
        WaitStatus::Signaled(_, signal, _) => Ok(128 + signal as u8),
        other => bail!("unexpected wait status: {other:?}"),
    }
}
