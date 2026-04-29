# Container Runtime

A container runtime implementation in Rust.

This repository contains a general-purpose container runtime written in Rust. It is not intended for production use — it is purely a personal study project.

## Concept

### 0. Basic System Calls

To implement a container runtime, we first need to understand a handful of basic system calls.

#### Clone

The `clone()` system call creates a new child process.  
While similar to `fork()`, `clone()` accepts flags such as `CLONE_NEWPID`, `CLONE_NEWNET`, and `CLONE_NEWNS`.  
These flags cause the child to be created inside a new namespace, isolated from the parent's system resources.

#### Unshare

The `unshare()` system call disassociates parts of the calling process's execution context (its namespaces).  
Unlike `clone()`, which creates a new process, `unshare()` lets the current process detach from one of its existing namespaces (for example, the mount namespace) and move into a new, isolated one.

<img width="1440" height="1200" alt="image" src="https://github.com/user-attachments/assets/7574defd-58a8-4aea-83fc-5ebe1a6f247c" />

#### Setns

The `setns()` system call attaches the calling process to an existing namespace.  
This is what powers commands like `docker exec`, which inject a new process (such as `/bin/bash`) into a namespace that belongs to an already-running container.

#### Execve

The `execve()` system call replaces the current process's memory image with a new program.  
Once namespace setup and filesystem isolation are done, `execve` overwrites the process's memory with the target container application (e.g. `/bin/sh`) and hands execution control over to it.

#### Mount / Unmount

These system calls attach or detach a filesystem to or from the directory tree.  
For example, we can mount a dedicated `/proc` inside the container's filesystem, or use a bind mount to expose a specific host directory to the container.

#### Pivot_root

This system call swaps the current root mount with a new one and moves the old root filesystem to a designated path.  
After the pivot, the process effectively loses access to the host's filesystem, which significantly improves isolation. The typical steps are:

1. Call `unshare`: create a new mount namespace so subsequent mount changes don't leak back to the host.
2. Prepare the new root: designate a specific directory (e.g. `/rootfs`) and make sure it is a mount point.
3. Call `pivot_root`: set `/rootfs` as the new root and move the original root into a subdirectory beneath it (e.g. `/rootfs/old_root`).
4. Unmount the old root: run `umount -l` (lazy unmount) on that subdirectory to fully detach the host's filesystem from the container's view.
5. Change directory: call `chdir("/")` so the working directory follows the new root.

<img width="1440" height="800" alt="image" src="https://github.com/user-attachments/assets/8240d345-3b26-4266-8137-498e78293051" />

### 1. Filesystem Isolation

I decided to start with filesystem isolation. Filesystem isolation means that the container sees its own filesystem as the root of its environment — the host's filesystem is no longer visible from inside.

To achieve this, we need to leverage system calls that relocate and isolate the container's root directory. There are several ways to do it, and I'm going to implement two variants.

#### Chroot-based filesystem isolation

This strategy performs filesystem isolation using the `chroot` system call.  
`chroot` changes the apparent root directory of the calling process (and its children) to a given path. Once applied, the process treats that path as its filesystem root — paths like `/` and `/etc` are resolved relative to it.

The implementation is as follows.

**1. Call `fork()` to create the container child process.**
```rust
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

        // Child process (the container environment)
        ForkResult::Child => {

            // Run the child. If it fails, exit immediately with code 127
            // instead of returning into parent-side logic.
            if let Err(e) = child_main(args) {
                eprintln!("container-runtime: child failed: {e:#}");
                std::process::exit(127);
            }
            unreachable!("child_main either execs or exits");
        }
    }
}
```

**2. In the child process, call `chroot` to change its root directory to the given path.**
```rust
/// Restrict the process's view of the filesystem to `rootfs` using the `chroot` system call.
pub fn isolate_fs_chroot(rootfs: &Path) -> Result<()> {
    // Change the current process's root directory.
    chroot(rootfs).with_context(|| format!("chroot {:?}", rootfs))?;

    // Reset the working directory so it matches the new root.
    chdir("/").context("chdir(\"/\") after chroot")?;

    Ok(())
}
```

**3. After `chroot`, call `execvp` to replace the child's memory image with the target program (the container's entrypoint).**
```rust
/// Replace the current process with `cmd` + `args` via execvp.
/// Returns only on failure.
pub fn exec_cmd(cmd: &str, args: &[String]) -> Result<()> {
    // Convert the command into a nul-safe CString.
    let c_cmd = CString::new(cmd).context("cmd contains a nul byte")?;

    // Convert the arguments into nul-safe CStrings.
    let mut c_args: Vec<CString> = Vec::with_capacity(args.len() + 1);
    c_args.push(c_cmd.clone()); // argv[0] — conventionally the program name
    for arg in args {
        c_args.push(CString::new(arg.as_str()).context("arg contains a nul byte")?);
    }

    // Replace the current process image with the target program.
    execvp(&c_cmd, &c_args).with_context(|| format!("exec {:?}", cmd))?;

    unreachable!("failed to execute: {} {}", cmd, args.join(" "));
}
```

**Result**

As shown in the output below, the container's filesystem is successfully isolated from the host's.
```bash
root@0a77470d7094:/app# cargo run -- run ./rootfs /bin/sh -c 'cat /etc/os-release'
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.03s
Running `target/debug/container-runtime run ./rootfs /bin/sh -c 'cat /etc/os-release'`
NAME="Alpine Linux"
ID=alpine
VERSION_ID=3.20.3
PRETTY_NAME="Alpine Linux v3.20"
HOME_URL="https://alpinelinux.org/"
BUG_REPORT_URL="https://gitlab.alpinelinux.org/alpine/aports/-/issues"
root@0a77470d7094:/app#
```

#### chroot jailbreak

Modern container systems don't rely on `chroot` for filesystem isolation, because the root directory created by `chroot` can be escaped back to the host filesystem.
`chroot` only changes the starting point from which the process resolves absolute paths — it does not restrict anything else (open file descriptors, the current working directory, capabilities, namespaces, ...).

Let's walk through the classic `chroot` + `chdir` jailbreak scenario.

<img width="661" height="433" alt="Screenshot 2026-04-24 at 7 51 29 PM" src="https://github.com/user-attachments/assets/2364e00e-26d7-4d6e-8841-cc204f19def6" />

1. Inside the current chroot, create a new directory to chroot into again (e.g. `./escape`).  

2. Call `chroot("./escape")`.  

3. The process's apparent root is now `/rootfs/escape`, but its CWD is still `/rootfs` — which sits *outside* the new root.  

4. Repeatedly call `chdir("..")`.  

5. Because the CWD is already outside the new root, the kernel does not clamp `..` at the root boundary, so we keep climbing until we reach the host's real root directory.

6. Jailbreak success  

#### pivot_root filesystem isolation

Because of the security problem above, modern container runtimes use the `pivot_root` system call to isolate the filesystem in a more secure way.

Unlike `chroot`, which only moves the starting point of absolute path resolution, `pivot_root` swaps the kernel's idea of the root mount itself, and when combined with a new mount namespace, makes mounts inside the container invisible to the host (and vice versa). The old root is not simply discarded. It is *relocated* into a subdirectory, which we then unmount to cut the last connection to the host filesystem.

The full sequence is:

<img width="481" height="645" alt="Screenshot 2026-04-24 at 7 57 37 PM" src="https://github.com/user-attachments/assets/d1e95aea-1f89-4a34-a9b9-bc9bcb708e85" />

1. `unshare(CLONE_NEWNS)`: create a new mount namespace.
2. `mount("/", MS_PRIVATE | MS_REC)`: stop mount events from propagating.
3. Bind-mount the rootfs onto itself: satisfy `pivot_root`'s mount-point requirement.
4. Create a stub directory (`rootfs/.old`) to receive the old root.
5. `pivot_root(rootfs, rootfs/.old)`: swap the root mount.
6. `chdir("/")`: reset the working directory into the new root.
7. `umount2("/.old", MNT_DETACH)` + `remove_dir("/.old")`: detach and clean up.
8. Mount a fresh `/proc` inside the container.

Each step deserves a closer look.

##### 1. Create a new mount namespace

```rust
unshare(CloneFlags::CLONE_NEWNS).context("unshare(CLONE_NEWNS)")?;
```

`unshare(CLONE_NEWNS)` detaches this process's mount namespace from the host's. After this call, mount/unmount operations performed by this process can be confined to a separate view of the mount table.

Namespace separation alone is not sufficient, however. Linux's default mount propagation mode is `MS_SHARED`, which means mount events can still cross namespace boundaries. That is what the next step addresses.

##### 2. Make "/" recursively private

```rust
mount::<str, _, str, str>(
    None,
    "/",
    None,
    MsFlags::MS_REC | MsFlags::MS_PRIVATE,
    None,
).context("mount / MS_REC|MS_PRIVATE")?;
```

This call does not create a new mount — it modifies the *propagation* property of the existing `/` mount, recursively applying `MS_PRIVATE` to every submount. Afterwards, any mount performed inside the container stays inside the container, and host-side mounts do not leak in.

The `mount()` syscall is overloaded: when `source` and `fstype` are both `None`, it is interpreted as "modify an existing mount" instead of "attach a new filesystem". The flag bits decide the exact semantics.

##### 3. Bind-mount the rootfs onto itself

```rust
mount::<_, _, str, str>(
    Some(rootfs),
    rootfs,
    None,
    MsFlags::MS_BIND | MsFlags::MS_REC,
    None,
).with_context(|| format!("bind mount {:?} onto itself", rootfs))?;
```

`pivot_root` has a strict requirement: the new root must be a *mount point* distinct from its parent mount. A plain directory like `./rootfs` sitting on top of an existing mount does not qualify. Bind-mounting it onto itself creates an independent mount-table entry for the same files, which satisfies `pivot_root`.

`MS_REC` is added defensively — if there happen to be any submounts beneath the rootfs, we want them preserved in the new view.

##### 4. Prepare a place for the old root

```rust
let old_root = rootfs.join(".old");
fs::create_dir_all(&old_root)
    .with_context(|| format!("create_dir_all {:?}", old_root))?;
```

`pivot_root(new_root, put_old)` does not throw away the old root — it *relocates* it into `put_old`. The kernel also requires `put_old` to be a subdirectory of `new_root`. We create `rootfs/.old` for that purpose.

##### 5. Swap the root

```rust
pivot_root(rootfs, old_root.as_path())
    .with_context(|| format!("pivot_root({:?}, {:?})", rootfs, old_root))?;
```

After this call:

- `/` resolves to what was `./rootfs`.
- The previous host root is now visible at `/.old`.

The container can still reach the host filesystem via `/.old/...`, so isolation is not complete until we detach it in step 7.

##### 6. Reset the working directory

```rust
chdir("/").context("chdir(\"/\") after pivot_root")?;
```

`pivot_root` changes the root mount but leaves the process's current working directory pointing at wherever it was before. After the pivot, that CWD now lives inside `/.old` (the relocated old root). If we skip this step, any relative path used by the child process would resolve back into the host filesystem — the same failure mode as the classic chroot escape.

##### 7. Detach and remove the old root

```rust
umount2("/.old", MntFlags::MNT_DETACH).context("umount2(/.old)")?;
fs::remove_dir("/.old").context("remove_dir(/.old)")?;
```

`MNT_DETACH` is a *lazy* unmount: it disconnects `/.old` from the mount tree immediately, but defers the actual teardown until every process that still has files open on it has closed them. A plain `umount` would likely fail with `EBUSY` because the runtime binary and its shared libraries were loaded from the host filesystem.

After the unmount, `/.old` is an empty directory. We use `remove_dir` (not `remove_dir_all`) so that if something went wrong and the directory still contained host files, the call would fail safely instead of deleting them.

##### 8. Mount a fresh procfs

```rust
fs::create_dir_all("/proc").context("create /proc")?;
mount::<_, _, _, str>(
    Some("proc"),
    "/proc",
    Some("proc"),
    MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
    None,
).context("mount /proc")?;
```

Detaching the old root in step 7 also severed the container's view of `/proc`. Without it, `ps`, `top`, reads under `/proc/self/*`, and many shell built-ins stop working.

A few details are worth unpacking.

**`/proc` is not a regular directory.** It is a *virtual filesystem* synthesised by the kernel. Files like `/proc/cpuinfo` or `/proc/self/status` do not exist on any disk; the procfs driver fabricates their contents on each read. That is why `fs::create_dir_all("/proc")` alone is not sufficient — it creates an empty mount point, but no driver is attached to it.

**How does the kernel know to invoke procfs when something opens `/proc/...`?** The Linux VFS (Virtual Filesystem Switch) maintains a mount table that maps paths to filesystem drivers. When a process makes a path-based syscall, VFS walks the path and switches to the driver registered at whichever mount point it hits:

```
path prefix   →  driver
─────────────────────────
/             →  ext4
/proc         →  procfs
/sys          →  sysfs
/tmp          →  tmpfs
```

`mount()` is the act of inserting a row into that table. From that moment on, any access under the mount point is transparently routed to the driver — no user-level code needs to be aware of the dispatch.

**Why is `source` set to the literal string `"proc"`?** For an on-disk filesystem (`ext4`, `xfs`, ...), `source` points to a block device that the driver reads. For a virtual filesystem, there is no such device. The kernel does not use `source` at all. The string only shows up as a label in `mount` output and `/proc/self/mountinfo`. By convention, the source is set to the filesystem's type name (`proc`, `tmpfs`, `sysfs`), so `mount` output reads naturally as `proc on /proc type proc`. Tools that parse mount output rely on this convention.

**`MS_NOSUID | MS_NODEV | MS_NOEXEC`** are standard container hardening flags: they disable set-uid/set-gid binaries, device nodes, and executable files under `/proc`. Nothing in a healthy procfs needs any of them, so denying them closes a small attack surface.

**Result**

After all eight steps, the container's mount table contains only what we intentionally placed there:

```bash
/ # mount
/run/host_mark/Users on / type fakeowner (rw,nosuid,nodev,relatime,fakeowner)
proc on /proc type proc (rw,nosuid,nodev,noexec,relatime)
```

Mounts created inside the container do not leak to the host, and each new container launch starts from a fresh mount namespace:

```bash
# Container A
/ # mount -t tmpfs tmpfs /tmp
/ # mount | grep tmpfs
tmpfs on /tmp type tmpfs (rw,relatime)

# Container B (fresh launch)
/ # mount | grep tmpfs
/ #
```

Compared with the chroot escape demonstrated earlier, the retained-fd trick no longer works.
Because the detached old root is not reachable even via open file descriptors inherited from before the pivot.

```bash
cargo run -- run ./rootfs /bin/sh -c 'echo inside; ls /; mount | wc -l'
```

```text
root@6b7298085cd6:/app# cargo run -- run ./rootfs /bin/sh -c 'echo inside; ls /; mount | wc -l'
   Compiling container-runtime v0.1.0 (/app)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.67s
     Running `target/debug/container-runtime run ./rootfs /bin/sh -c 'echo inside; ls /; mount | wc -l'`
inside
bin    dev    etc    home   lib    media  mnt    opt    proc   root   run    sbin   srv    sys    tmp    usr    var
2 (only root and /proc are mounted in new child process)
root@6b7298085cd6:/app# 
```

**Existing problem**

As we can see, we completed filesystem isolation but we didn't isolated the PID namespace.
If we run the container runtime and run `ps` command inside of it, we still can see the host's process.
```bash
PID   USER     TIME  COMMAND
    1 root      0:02 /bin/sh -c echo Container started trap "exit 0" 15  exec "$@" while sleep 1 & wait $!; do :; done -
   28 root      0:00 /bin/sh -c echo "New container started. Keep-alive process started." ; export VSCODE_REMOTE_CONTAINERS_SESSION=b96b2018-3691-4519-ad46-fadb534b58c31777019931519 ; /bin/sh
   34 root      0:00 /bin/sh
   40 root      0:00 /bin/sh
  214 root      0:00 /bin/sh
  216 root      0:00 sh /root/.vscode-server/bin/560a9dba96f961efea7b1612916f89e5d5d4d679/bin/code-server --log debug --force-disable-user-env --server-data-dir /root/.vscode-server --use-host-proxy --telemetry-level all --accept-server-license-terms --host 127.0.0.1 --port 0 --connection-token-
  230 root      0:23 /vscode/vscode-server/bin/linux-arm64/560a9dba96f961efea7b1612916f89e5d5d4d679/node /vscode/vscode-server/bin/linux-arm64/560a9dba96f961efea7b1612916f89e5d5d4d679/out/server-main.js --log debug --force-disable-user-env --server-data-dir /root/.vscode-server --use-host-proxy 
  243 root      0:00 /root/.vscode-server/bin/560a9dba96f961efea7b1612916f89e5d5d4d679/node /tmp/vscode-remote-containers-server-0aa04ef3-e653-42bd-b918-3889fca26696.js
  272 root      0:03 /root/.vscode-server/bin/560a9dba96f961efea7b1612916f89e5d5d4d679/node -e      const net = require('net');     const fs = require('fs');     process.stdin.pause();     const client = net.createConnection({ host: '127.0.0.1', port: 43011 }, () => {      console.error('Connect
  294 root      0:14 /root/.vscode-server/bin/560a9dba96f961efea7b1612916f89e5d5d4d679/node -e      const net = require('net');     const fs = require('fs');     process.stdin.pause();     const client = net.createConnection({ host: '127.0.0.1', port: 43011 }, () => {      console.error('Connect
  314 root      0:04 /vscode/vscode-server/bin/linux-arm64/560a9dba96f961efea7b1612916f89e5d5d4d679/node /vscode/vscode-server/bin/linux-arm64/560a9dba96f961efea7b1612916f89e5d5d4d679/out/bootstrap-fork --type=fileWatcher
  326 root      2:31 /vscode/vscode-server/bin/linux-arm64/560a9dba96f961efea7b1612916f89e5d5d4d679/node --dns-result-order=ipv4first /vscode/vscode-server/bin/linux-arm64/560a9dba96f961efea7b1612916f89e5d5d4d679/out/bootstrap-fork --type=extensionHost --transformURIs --useHostProxy=true
  383 root      0:05 /vscode/vscode-server/bin/linux-arm64/560a9dba96f961efea7b1612916f89e5d5d4d679/node /vscode/vscode-server/bin/linux-arm64/560a9dba96f961efea7b1612916f89e5d5d4d679/out/bootstrap-fork --type=ptyHost --logsPath /root/.vscode-server/data/logs/20260424T083904
  414 root      0:01 /vscode/vscode-server/bin/linux-arm64/560a9dba96f961efea7b1612916f89e5d5d4d679/node /vscode/vscode-server/bin/linux-arm64/560a9dba96f961efea7b1612916f89e5d5d4d679/extensions/json-language-features/server/dist/node/jsonServerMain --node-ipc --clientProcessId=326
  552 root      3:57 /root/.vscode-server/extensions/rust-lang.rust-analyzer-0.3.2870-linux-arm64/server/rust-analyzer
  855 root      0:00 /usr/local/rustup/toolchains/1.95.0-aarch64-unknown-linux-gnu/libexec/rust-analyzer-proc-macro-srv
 3101 root      0:04 /vscode/vscode-server/bin/linux-arm64/560a9dba96f961efea7b1612916f89e5d5d4d679/node /vscode/vscode-server/bin/linux-arm64/560a9dba96f961efea7b1612916f89e5d5d4d679/extensions/markdown-language-features/dist/serverWorkerMain --node-ipc --clientProcessId=326
58211 root      0:00 /bin/bash --init-file /vscode/vscode-server/bin/linux-arm64/560a9dba96f961efea7b1612916f89e5d5d4d679/out/vs/workbench/contrib/terminal/common/scripts/shellIntegration-bash.sh
59413 root      0:00 target/debug/container-runtime run ./rootfs /bin/sh
59423 root      0:00 /bin/sh
59432 root      0:00 /bin/sh -c "/vscode/vscode-server/bin/linux-arm64/560a9dba96f961efea7b1612916f89e5d5d4d679/out/vs/base/node/cpuUsage.sh" 58211 59413 59423
59433 root      0:00 {cpuUsage.sh} /bin/bash /vscode/vscode-server/bin/linux-arm64/560a9dba96f961efea7b1612916f89e5d5d4d679/out/vs/base/node/cpuUsage.sh 58211 59413 59423
59438 root      0:00 sleep 1
59439 root      0:00 sleep 1
59440 root      0:00 ps
```

---

### 2. Process Isolation

Filesystem isolation alone is not enough. As the `ps` output at the end of the previous section shows, a process running inside the container can still see every process on the host. 
The first process in a container has a different PID on the host computer (like 59423). Because of this, it cannot always be PID 1, which is the normal rule for Linux start-up processes.

To fix both problems, we add a PID namespace.

#### Why `unshare(CLONE_NEWPID)` alone is not enough

Unlike `CLONE_NEWNS` (mount namespace), which moves the calling process into a new namespace immediately, 
but `CLONE_NEWPID` does *not* move the caller. 
It only declares that the caller's *future children* will be members of a new PID namespace. 
The first such child becomes PID 1 inside it.

This means we must `fork()` once *after* the `unshare`. The new child (not the caller) is the one that lives inside the new namespace and becomes its PID 1.

#### The double-fork pattern

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
- **Mount namespace stays in the filesystem code.** The `unshare(CLONE_NEWNS)` call from the previous section stays inside `isolate_fs_pivot`, not here. PID and mount namespaces have different semantics. `CLONE_NEWNS` moves the caller immediately, `CLONE_NEWPID` does not. So each is set up where it makes sense for that namespace, rather than combining them together.
- **Exit-status forwarding.** The setup process forwards the init's exit status back up by calling `std::process::exit(code)`. The host parent's `waitpid` then sees that as a normal `Exited(_, code)` status, so the user gets the same exit code they would have gotten in a single-fork world.

#### Why `/proc` just works

The fresh procfs mount from the previous filesystem isolation section is enough to make `ps` inside the container show only namespace-local processes. So no new mount code is needed. 
The Linux kernel ties each procfs *mount instance* to the PID namespace of the process that mounted it. 
Because the init process mounts `/proc` *after* being forked into the new PID namespace, that mount instance is bound to the new namespace and reflects only its members.
If we mounted `/proc` from the setup process (which is still in the host PID namespace), `ps` inside the container would continue to leak host PIDs.

#### Why we have to keep the setup layer and even in Docker and ContainerD?

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

Compared with the previous section's `ps` dump, which leaked dozens of host processes, the container now sees only its own processes. 
The first shell is PID 1, and `/proc` reflects the new namespace's view.

### 3. Hardware resource limitation

#### Cgroup 해야하는 이유

#### /sys/fs/cgroup 가상 파일 시스템 탐색

다음과 깉이 cgroup 내부에 존재하는 파일들을 확인할 수 있다.
cgroup.procs
cgroup.threads
memory.stat
cpu.stat
docker
kubepods
```text
root@6b7298085cd6:/sys/fs/cgroup# ls
cgroup.controllers  cgroup.max.descendants  cgroup.procs  cgroup.subtree_control  cpu.pressure  cpu.stat.local         cpuset.cpus.isolated   docker       io.stat   memory.pressure  memory.stat             podruntime    restricted
cgroup.max.depth    cgroup.pressure         cgroup.stat   cgroup.threads          cpu.stat      cpuset.cpus.effective  cpuset.mems.effective  io.pressure  kubepods  memory.reclaim   memory.zswap.writeback  procd-paused
```

docker cgroup을 한번 살펴보자
```text
root@6b7298085cd6:/sys/fs/cgroup# cd docker/
root@6b7298085cd6:/sys/fs/cgroup/docker# cat cpu.max
max 100000
root@6b7298085cd6:/sys/fs/cgroup/docker# ls
6b72980...  cgroup.pressure         cpu.stat                         cpuset.mems.effective                                             hugetlb.2MB.events         hugetlb.32MB.numa_stat     io.max               memory.oom.group     memory.zswap.current
6f20202...  cgroup.procs            cpu.stat.local                   ddcca065....  hugetlb.2MB.events.local   hugetlb.32MB.rsvd.current  io.pressure          memory.peak          memory.zswap.max
buildkit                                                          cgroup.stat             cpu.weight                       hugetlb.1GB.current                                               hugetlb.2MB.max            hugetlb.32MB.rsvd.max      io.stat              memory.pressure      memory.zswap.writeback
buildx                                                            cgroup.subtree_control  cpu.weight.nice                  hugetlb.1GB.events                                                hugetlb.2MB.numa_stat      hugetlb.64KB.current       memory.current       memory.reclaim       pids.current
cgroup.controllers                                                cgroup.threads          cpuset.cpus                      hugetlb.1GB.events.local                                          hugetlb.2MB.rsvd.current   hugetlb.64KB.events        memory.events        memory.stat          pids.events
cgroup.events                                                     cgroup.type             cpuset.cpus.effective            hugetlb.1GB.max                                                   hugetlb.2MB.rsvd.max       hugetlb.64KB.events.local  memory.events.local  memory.swap.current  pids.events.local
cgroup.freeze                                                     cpu.idle                cpuset.cpus.exclusive            hugetlb.1GB.numa_stat                                             hugetlb.32MB.current       hugetlb.64KB.max           memory.high          memory.swap.events   pids.max
cgroup.kill                                                       cpu.max                 cpuset.cpus.exclusive.effective  hugetlb.1GB.rsvd.current                                          hugetlb.32MB.events        hugetlb.64KB.numa_stat     memory.low           memory.swap.high     pids.peak
cgroup.max.depth                                                  cpu.max.burst           cpuset.cpus.partition            hugetlb.1GB.rsvd.max                                              hugetlb.32MB.events.local  hugetlb.64KB.rsvd.current  memory.max           memory.swap.max      rdma.current
cgroup.max.descendants                                            cpu.pressure            cpuset.mems                      hugetlb.2MB.current                                               hugetlb.32MB.max           hugetlb.64KB.rsvd.max      memory.min           memory.swap.peak     rdma.max
```
docker도 마찬가지로 cgroup을 사용하여 docker 프로세스 자체의 리소스 관리와 docker process에서 생성되는 새로운 컨테이너에 대한 리소스 제한도 수행하고 있음을 알수있다,
실제로 더 자세히 살펴보면 (6b729...) 컨테이너는 현재 container_runtime의 개발 환경

```bash
root@6b7298085cd6:/sys/fs/cgroup/docker# cd 6b7298.../
root@6b7298085cd6:/sys/fs/cgroup/docker/6b7298...# ls
cgroup.controllers      cgroup.procs            cpu.max.burst    cpuset.cpus.effective            hugetlb.1GB.events        hugetlb.2MB.events        hugetlb.32MB.events        hugetlb.64KB.events        io.pressure          memory.max        memory.swap.current   memory.zswap.writeback  rdma.max
cgroup.events           cgroup.stat             cpu.pressure     cpuset.cpus.exclusive            hugetlb.1GB.events.local  hugetlb.2MB.events.local  hugetlb.32MB.events.local  hugetlb.64KB.events.local  io.stat              memory.min        memory.swap.events    pids.current
cgroup.freeze           cgroup.subtree_control  cpu.stat         cpuset.cpus.exclusive.effective  hugetlb.1GB.max           hugetlb.2MB.max           hugetlb.32MB.max           hugetlb.64KB.max           memory.current       memory.oom.group  memory.swap.high      pids.events
cgroup.kill             cgroup.threads          cpu.stat.local   cpuset.cpus.partition            hugetlb.1GB.numa_stat     hugetlb.2MB.numa_stat     hugetlb.32MB.numa_stat     hugetlb.64KB.numa_stat     memory.events        memory.peak       memory.swap.max       pids.events.local
cgroup.max.depth        cgroup.type             cpu.weight       cpuset.mems                      hugetlb.1GB.rsvd.current  hugetlb.2MB.rsvd.current  hugetlb.32MB.rsvd.current  hugetlb.64KB.rsvd.current  memory.events.local  memory.pressure   memory.swap.peak      pids.max
cgroup.max.descendants  cpu.idle                cpu.weight.nice  cpuset.mems.effective            hugetlb.1GB.rsvd.max      hugetlb.2MB.rsvd.max      hugetlb.32MB.rsvd.max      hugetlb.64KB.rsvd.max      memory.high          memory.reclaim    memory.zswap.current  pids.peak
cgroup.pressure         cpu.max                 cpuset.cpus      hugetlb.1GB.current              hugetlb.2MB.current       hugetlb.32MB.current      hugetlb.64KB.current       io.max                     memory.low           memory.stat       memory.zswap.max      rdma.current
root@6b7298085cd6:/sys/fs/cgroup/docker/6b7298...# cat memory.max
max
root@6b7298085cd6:/sys/fs/cgroup/docker/6b7298...# cat memory.current
1626386432
root@6b7298085cd6:/sys/fs/cgroup/docker/6b7298...# cat cpu.max       
max 100000
root@6b7298085cd6:/sys/fs/cgroup/docker/6b7298...# cat cpu.idle
0
root@6b7298085cd6:/sys/fs/cgroup/docker/6b7298...# 
```
위와 같이 현재 사용중인 컨테이너에 대해서도 리소스가 제한된 상태를 알수있다
