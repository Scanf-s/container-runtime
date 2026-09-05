# Process namespaces

[Learning notes](README.md) · [Getting started](../README.md)

Filesystem isolation alone is not enough. As the `ps` output at the end of the [filesystem isolation notes](filesystems.md) shows, a process running inside the container can still see every process on the host. 
The first process in a container has a different PID on the host computer (like 59423). Because of this, it cannot always be PID 1, which is the normal rule for Linux start-up processes.

To fix both problems, we add a PID namespace.

## Why `unshare(CLONE_NEWPID)` alone is not enough

Unlike `CLONE_NEWNS` (mount namespace), which moves the calling process into a new namespace immediately, 
but `CLONE_NEWPID` does *not* move the caller. 
It only declares that the caller's *future children* will be members of a new PID namespace. 
The first such child becomes PID 1 inside it.

This means we must `fork()` once *after* the `unshare`. The new child (not the caller) is the one that lives inside the new namespace and becomes its PID 1.

## The double-fork pattern

The runtime ends up with three layers of process.

```
host runtime -> fork -> setup process -> fork -> init (PID 1 in new ns) -> exec -> user command
```

Why two forks instead of one? 

Functionally a single fork is enough. The host runtime could call `unshare(CLONE_NEWPID)` itself and fork once, and the child would still become PID 1.  

But We keep an intermediate "setup" process because production runtimes put real work in that intermediate layer.
And the structure makes it natural to extend later (cgroup, user namespace isolation).

The implementation in `src/runtime.rs`:

```rust
pub fn run(args: RunArgs) -> Result<ExitCode> {
    if !args.rootfs.is_dir() {
        bail!("rootfs {:?} does not exist or is not a directory", args.rootfs);
    }

    // First fork: the child becomes the "setup" process that will
    // establish the new PID namespace and launch the container's PID 1.
    match unsafe { fork() }.context("fork failed")? {
        ForkResult::Parent { child } => {
            let status = waitpid(child, None).context("waitpid failed")?;
            match status {
                WaitStatus::Exited(_, code) => Ok(ExitCode::from(code as u8)),
                WaitStatus::Signaled(_, sig, _) => Ok(ExitCode::from(128u8 + sig as u8)),
                other => bail!("unexpected wait status: {:?}", other),
            }
        }
        ForkResult::Child => {
            // SETUP Process (aka Container-Shim)
            if let Err(e) = setup_child(args) {
                eprintln!("container-runtime: setup_child failed: {e:#}");
                std::process::exit(127);
            }
            unreachable!();
        }
    }
}

fn setup_child(args: RunArgs) -> Result<()> {
    // unshare(CLONE_NEWPID) does NOT move us into the new namespace.
    // It only causes our future children to be members of it.
    // So we must fork again, and that grandchild is PID 1.
    unshare(CloneFlags::CLONE_NEWPID).context("unshare(CLONE_NEWPID)")?;

    match unsafe { fork() }.context("fork (setup) failed")? {
        ForkResult::Parent { child } => {
            let status = waitpid(child, None).context("waitpid(child) failed")?;
            let code = match status {
                WaitStatus::Exited(_, c) => c as i32,
                WaitStatus::Signaled(_, sig, _) => 128 + sig as i32,
                other => bail!("unexpected wait status for init: {:?}", other),
            };
            std::process::exit(code);
        }
        ForkResult::Child => {
            if let Err(e) = child_main(args) {
                eprintln!("container-runtime: child_main failed: {e:#}");
                std::process::exit(127);
            }
            unreachable!();
        }
    }
}

fn child_main(args: RunArgs) -> Result<()> {
    container::isolate_fs_pivot(&args.rootfs)?;
    container::exec_cmd(&args.cmd, &args.args)?;
    unreachable!();
}
```

Three observations are worth calling out.

- **Where the `unshare` lives.** `unshare(CLONE_NEWPID)` sits in `setup_child`, between the two forks. Putting it in `run()` (before the first fork) would also work, but then every subsequent `fork()` performed by `run()` would land its child in the new PID namespace. That is fine for this code, but the `run()` function's responsibility ever grows additionally.
- **Mount namespace stays in the filesystem code.** The `unshare(CLONE_NEWNS)` call from the [filesystem isolation notes](filesystems.md) stays inside `isolate_fs_pivot`, not here. PID and mount namespaces have different semantics. `CLONE_NEWNS` moves the caller immediately, `CLONE_NEWPID` does not. So each is set up where it makes sense for that namespace, rather than combining them together.
- **Exit-status forwarding.** The setup process forwards the init's exit status back up by calling `std::process::exit(code)`. The host parent's `waitpid` then sees that as a normal `Exited(_, code)` status, so the user gets the same exit code they would have gotten in a single-fork world.

## Why `/proc` just works

The fresh procfs mount from the previous filesystem isolation section is enough to make `ps` inside the container show only namespace-local processes. So no new mount code is needed. 
The Linux kernel ties each procfs *mount instance* to the PID namespace of the process that mounted it. 
Because the init process mounts `/proc` *after* being forked into the new PID namespace, that mount instance is bound to the new namespace and reflects only its members.
If we mounted `/proc` from the setup process (which is still in the host PID namespace), `ps` inside the container would continue to leak host PIDs.

## Why we have to keep the setup layer and even in Docker and ContainerD?

Since a single `unshare + fork` would suffice, why introduce a setup process at all?

There are two reasons.

**1. Real runtimes do non-trivial work between fork and exec.**

The sequence between "init has been forked into the new namespace" and "init has called `execve` into the user's command" is the only place certain setup tasks can happen.

- **Cgroup registration.** Resource limits (memory, CPU, ...) are enforced by writing the init process's host PID into a cgroup file on the host's `/sys/fs/cgroup/...`. This must happen before the init starts allocating, otherwise allocations made during early setup escape the limit.
- **User namespace mapping.** Writing `/proc/<init_pid>/uid_map` and `gid_map` is the step that maps the container's `root` to a non-root host user. The kernel requires these files to be written from *outside* the namespace (i.e. by the setup process), and the init must wait for that write before doing any uid-sensitive operation. This implies a synchronization barrier (typically a pipe) between setup and init.
- **Capability and security pre-configuration.** Some hardening steps need to be applied with knowledge of both the host PID and the in-namespace state, again requiring coordination across the namespace boundary.

A single-fork design forces the host to do all of this itself. The setup layer gives each container a dedicated process that is responsible for exactly one container's setup, which keeps the host clean and makes per-container failures isolated.

**2. The setup layer is the seed of a "shim" process.**

In production runtimes (containerd, Docker) there is a long-lived process called a *shim* (`containerd-shim`) that sits between the container manager and the container itself.

- Stays alive for as long as the container does, even if the container manager (`containerd`) is restarted for an update — preventing the container's init from being orphaned to host system's PID 1 and losing its exit status.
- Receives signals and forwards them to the container's init, so that a `Ctrl+C` to the manager does not propagate to every container the manager owns.
- Holds the container's exit code until the manager comes back to read it.

A shim is necessary because in Linux, the *parent* of a process is the one responsible for reaping it (`waitpid`) and for routing signals to it. If the manager were the direct parent of every container's init, then restarting the manager would orphan every container to host PID 1, and a single `Ctrl+C` to the manager would tear down every container at once. The shim absorbs both responsibilities on a per-container basis.

Our `setup_child` is the seed of that idea. Right now it is a thin wrapper that just unshares the PID namespace and waits, but the structure is already in place so that future stages (cgroups, user namespaces, signal forwarding) can plug in without reshaping the runtime.

**Result**

```bash
root@6b7298085cd6:/app# cargo run -- run ./rootfs /bin/sh
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.04s
     Running `target/debug/container-runtime run ./rootfs /bin/sh`
/ # ps -a
PID   USER     TIME  COMMAND
    1 root      0:00 /bin/sh
    2 root      0:00 ps -a
/ # ls -al
total 0
drwxr-xr-x   19 root     root           608 Apr 25 06:45 .
drwxr-xr-x   19 root     root           608 Apr 25 06:45 ..
drwxr-xr-x   84 root     root          2688 Sep  6  2024 bin
drwxr-xr-x    2 root     root            64 Sep  6  2024 dev
drwxr-xr-x   37 root     root          1184 Sep  6  2024 etc
drwxr-xr-x    2 root     root            64 Sep  6  2024 home
drwxr-xr-x   13 root     root           416 Sep  6  2024 lib
drwxr-xr-x    5 root     root           160 Sep  6  2024 media
drwxr-xr-x    2 root     root            64 Sep  6  2024 mnt
drwxr-xr-x    2 root     root            64 Sep  6  2024 opt
dr-xr-xr-x  256 root     root             0 Apr 25 06:45 proc
drwx------    3 root     root            96 Apr 24 10:11 root
drwxr-xr-x    2 root     root            64 Sep  6  2024 run
drwxr-xr-x   63 root     root          2016 Sep  6  2024 sbin
drwxr-xr-x    2 root     root            64 Sep  6  2024 srv
drwxr-xr-x    2 root     root            64 Sep  6  2024 sys
drwxrwxrwt    2 root     root            64 Sep  6  2024 tmp
drwxr-xr-x    7 root     root           224 Sep  6  2024 usr
drwxr-xr-x   13 root     root           416 Sep  6  2024 var
/ # mount | wc -l
2
/ # mount
/run/host_mark/Users on / type fakeowner (rw,nosuid,nodev,relatime,fakeowner)
proc on /proc type proc (rw,nosuid,nodev,noexec,relatime)
/ # ps -a
PID   USER     TIME  COMMAND
    1 root      0:00 /bin/sh
    7 root      0:00 ps -a
/ # exit
root@6b7298085cd6:/app#  
```

Compared with the [filesystem isolation notes](filesystems.md)' `ps` dump, which leaked dozens of host processes, the container now sees only its own processes. 
The first shell is PID 1, and `/proc` reflects the new namespace's view.
