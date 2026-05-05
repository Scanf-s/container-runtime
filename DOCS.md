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

---

### 3. Hardware Resource Isolation with cgroups

#### Why cgroups?

Namespaces and `pivot_root` give us *visibility* isolation.
The container sees only its own processes, only its own filesystem. 
But they do not place any *quantitative* limit on what a container can consume. 
A process inside the container is still free to allocate every byte of host memory, fork until the host runs out of PIDs, or pin every CPU at 100%.

> Linux solves this with **control groups (cgroups)**. 

A cgroup is a kernel-managed group of processes for which one or more *controllers* (memory, cpu, pids, io, ...) account for usage and enforce limits. 

Namespaces and cgroups are complementary:
| Mechanism | Question it answers |
|---|---|
| Namespace | "What can the process *see*?" |
| cgroup    | "How much can the process *consume*?" |

A real container needs both. This section adds memory, CPU, and PID limits via cgroup v2.

#### cgroup v1 vs v2

Two generations of the API exist. 
- v1 maintains a *separate* hierarchy per controller (`/sys/fs/cgroup/memory/...`, `/sys/fs/cgroup/cpu/...`), so a single process can simultaneously belong to different cgroups in different hierarchies. 
- v2 unifies everything into a single tree under `/sys/fs/cgroup/`, which makes membership and accounting easier to reason about. Modern distributions (Ubuntu 22.04+, Fedora 31+, recent Debian) are using v2 for its standard.

The two can be told apart by looking for the `cgroup.controllers` file at the root. it only exists in v2:
```bash
$ stat -fc %T /sys/fs/cgroup/
cgroup2fs       # v2
tmpfs           # v1 (legacy)
```

We target `v2` exclusively.

#### Exploring `/sys/fs/cgroup`

Before writing any code, it's instructive to look at what is already there. 
The whole cgroup interface is exposed as a virtual filesystem. 
*Creating* a cgroup is a `mkdir`, *limiting* it is a `write` to a file, *registering a process* is another write. 
There are no special syscalls.

```bash
root@host:/sys/fs/cgroup# ls
cgroup.controllers       cpu.stat            kubepods.slice
cgroup.subtree_control   memory.stat         system.slice
cgroup.procs             pids.current        user.slice
cgroup.threads           docker              ...
```

A few of these directories are made by other software running on the same host:

- `system.slice/` and `user.slice/` are created by `systemd` to group system services and user sessions.
- `docker/` is created by the Docker daemon, with one subdirectory per container.
- `kubepods.slice/` is created by `kubelet`, with one subdirectory per Pod.

In other words, every container runtime on Linux(Docker, containerd, runc, podman, Kubernetes) works by adding directories under `/sys/fs/cgroup/`. 
The runtime we're building does the same thing.
The trick is that there are no new APIs to learn; the API *is* the filesystem.

Also, Let's inspect the Docker's cgroup (our dev container).

```bash
root@host:/sys/fs/cgroup/docker# cat cpu.max
max 100000
root@host:/sys/fs/cgroup/docker/6b7298085cd6.../memory.current
1626386432              # ~1.5 GiB currently used by this container
```

This `6b7298085cd6...` directory is the cgroup of my devcontainer.
Docker put its own work inside `/sys/fs/cgroup/docker/`.

#### Key files in a cgroup directory

Once a cgroup directory exists, the kernel populates it with controller-specific files. The ones this runtime cares about:

| File | Type | Purpose |
|---|---|---|
| `cgroup.controllers` | read-only | Controllers available to this cgroup |
| `cgroup.subtree_control` | rw | Controllers delegated to children |
| `cgroup.procs` | rw | Process IDs that belong to this cgroup |
| `memory.max` | rw | Hard memory limit (bytes); OOM-kill on excess |
| `memory.current` | read-only | Current memory usage |
| `cpu.max` | rw | "&lt;quota&gt; &lt;period&gt;" — μs of CPU per period |
| `pids.max` | rw | Maximum number of tasks (processes/threads) |

A few subtleties worth highlighting:

- **`cgroup.controllers` vs `cgroup.subtree_control`**: The former lists controllers *available* to this cgroup. The latter lists controllers *delegated to its children*. A child only sees a controller if its parent enables it via `subtree_control`. Without that delegation, a fresh child cgroup will not even have files like `memory.max`.

- **`memory.current` vs `docker stats` difference.**: `memory.current` includes page cache, while `docker stats` and most monitoring tools subtract `inactive_file` ("working set"). On a freshly booted devcontainer, the difference is usually tens to hundreds of MiB.

- **`pids.max` counts threads.** Linux models a thread as a task with its own PID, so a JVM or Go workload with many threads can exhaust `pids.max` even with a single "process".

#### Implementation

The runtime represents a container's cgroup as a Rust struct that creates the directory on construction and removes it on `Drop`:

```rust
// src/cgroup.rs
pub struct Cgroup {
    path: PathBuf,
}

impl Cgroup {
    pub fn new() -> Result<Self> {
        let cgroup_path: &Path = Path::new("/sys/fs/cgroup");
        let subtree_path: &Path = Path::new("/sys/fs/cgroup/cgroup.subtree_control");

        // 1. Verify cgroup v2 (only v2 has cgroup.controllers at the root).
        if !cgroup_path.join("cgroup.controllers").is_file() {
            bail!("cgroupfs is not v2 (or not mounted)");
        }

        // 2. cgroupfs is privileged. Fail early with a clear error.
        if !nix::unistd::geteuid().is_root() {
            bail!("cgroup operations require root privileges");
        }

        // 3. Make sure the parent has memory/cpu/pids in subtree_control.
        //    Otherwise the new child will lack the corresponding files.
        let controllers = fs::read_to_string(subtree_path)?;
        let active: Vec<&str> = controllers.split_whitespace().collect();
        let required = ["memory", "cpu", "pids"];
        let missing: Vec<&str> = required.iter()
            .filter(|c| !active.contains(c))
            .copied().collect();
        if !missing.is_empty() {
            let payload = missing.iter().map(|c| format!("+{c}"))
                .collect::<Vec<_>>().join(" ");
            fs::write(subtree_path, payload)
                .context("failed to enable controllers")?;
        }

        // 4. Create our own subdirectory with a random name.
        let id = format!("rust_container_{:x}", random::<u64>());
        let new_cgroup = cgroup_path.join(&id);
        fs::create_dir(&new_cgroup).context("create cgroup dir")?;

        // 5. Check delegated controllers.
        let delegated = fs::read_to_string(new_cgroup.join("cgroup.controllers"))?;
        let delegated: Vec<&str> = delegated.split_whitespace().collect();
        for c in &required {
            if !delegated.contains(c) {
                bail!("controller {c} not delegated to new cgroup");
            }
        }

        Ok(Cgroup { path: new_cgroup })
    }

    pub fn add_pid(&self, pid: nix::unistd::Pid) -> Result<()> {
        fs::write(self.path.join("cgroup.procs"), pid.to_string())
            .context("write cgroup.procs")?;
        Ok(())
    }

    pub fn set_memory_max(&self, bytes: u64) -> Result<()> {
        fs::write(self.path.join("memory.max"), bytes.to_string())
            .context("write memory.max")?;
        Ok(())
    }

    pub fn set_cpu_max(&self, quota_us: u64, period_us: u64) -> Result<()> {
        fs::write(self.path.join("cpu.max"), format!("{quota_us} {period_us}"))
            .context("write cpu.max")?;
        Ok(())
    }

    pub fn set_pid_max(&self, pids: u64) -> Result<()> {
        fs::write(self.path.join("pids.max"), pids.to_string())
            .context("write pids.max")?;
        Ok(())
    }
}

impl Drop for Cgroup {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}
```

A few design notes:

- **RAII for cleanup.** Without `Drop`: Every container run would leave a stale `rust_container_<hex>` directory under `/sys/fs/cgroup/`. With `Drop`, the directory is automatically removed when the parent's `Cgroup` value goes out of scope at the end of `run()`. The forked child also holds a copy of the struct, but the child path always exits via `std::process::exit`, which skips destructors. So cleanup happens exactly once, in the parent.

- **`cgroup.controllers` for the v2 check.**: Initial attempts checked `/sys/fs/cgroup` itself with `is_file()`, which always returns `false` because it's a directory. So i changed the logic by cheking the `cgroup.controllers` file is exists.

#### The race condition between `fork` and `add_pid`

**Without synchronization**
| Time | Parent | Setup-child | Grandchild |
|---|---|---|---|
| t0 | issued `fork()` | started in host root cgroup | — |
| t1 | (still running) | `unshare -> fork` | born; **inherits host root cgroup** |
| t2 | (still running) | `waitpid` | `execvp` -> workload running, *no limit* |
| t3 | `add_pid(setup)` | (now in our cgroup) | still uncontrolled <<< **PROBLEM** |

The grandchild (the actual user workload) has already been forked and is running before the parent gets a chance to register it. Since the setup-child was still in the host root cgroup at the moment of grandchild's fork, the grandchild inherits the host root cgroup. By the time the parent migrates the setup-child, it is too late. The grandchild was forked from a not-yet-migrated process.

#### Pipe-based parent/child synchronization

The fix is to make the setup-child wait until the parent has finished `add_pid` before doing its own `unshare`/`fork`. A pipe is the standard primitive for this:

```rust
// src/runtime.rs
let new_cgroup = Cgroup::new()?;
let (read_fd, write_fd) = pipe()?;       // Created BEFORE fork
new_cgroup.set_cpu_max(cpu_quota_us, CPU_PERIOD_US)?;
new_cgroup.set_memory_max(args.mem)?;
new_cgroup.set_pid_max(args.pids)?;

match unsafe { fork() }? {
    ForkResult::Parent { child } => {
        drop(read_fd);                    // parent never reads
        new_cgroup.add_pid(child)?;       // 1. register child
        write(&write_fd, &[1u8])?;        // 2. signal "you may proceed"
        drop(write_fd);
        let status = waitpid(child, None)?;
        // ...
    }
    ForkResult::Child => {
        drop(write_fd);                   // child never writes
        let mut buf = [0u8; 1];
        read(read_fd.as_raw_fd(), &mut buf)?;   // BLOCK until parent signals
        drop(read_fd);
        // From here on, this process is in the cgroup.
        // The grandchild we are about to fork will inherit cgroup membership.
        setup_child(args)?;
    }
}
```

Both file descriptors must be created *before* the fork so that both processes inherit them. After fork, each side closes the end it does not use (otherwise the EOF that the other side would observe never arrives, because someone is still holding the descriptor open).

> `pipe()` returns `(OwnedFd, OwnedFd)`.   
`OwnedFd` automatically closes its file descriptor when dropped.  
Using `drop(fd)` instead of `close(...)` lets `OwnedFd`'s destructor handle the close exactly once.

With this in place, the corrected timeline is:

| Time | Parent | Setup-child | Grandchild |
|---|---|---|---|
| t0 | `fork()` returns | blocked on `read()` | — |
| t1 | `add_pid(setup)` | blocked | — |
| t2 | `write(pipe, 1)` | unblocks; now in cgroup | — |
| t3 | `waitpid` | `unshare → fork` | born **inside cgroup** (inherited) |
| t4 | (still waiting) | `waitpid(grandchild)` | `execvp` → workload **constrained** |

The grandchild starts life inside the cgroup, so all its allocations are accounted, the OOM killer enforces `memory.max`, fork attempts respect `pids.max`, and the scheduler caps it at `cpu.max`.

#### CLI integration

Limits are exposed as flags on `run`:

```rust
// src/cli.rs
#[derive(Parser, Debug)]
pub struct RunArgs {
    pub rootfs: PathBuf,

    /// Number of CPUs (e.g. 0.5 for half a core, 2.0 for two cores).
    #[arg(long, default_value_t = 1.0)]
    pub cpus: f64,

    /// Memory limit in bytes.
    #[arg(long, default_value_t = 512 * 1024 * 1024)]
    pub mem: u64,

    /// Maximum number of tasks (processes and threads).
    #[arg(long, default_value_t = 1024)]
    pub pids: u64,

    pub cmd: String,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}
```

The `--cpus` flag mirrors Docker's. Internally it is converted to the `cpu.max` model the kernel actually wants:

```rust
const CPU_PERIOD_US: u64 = 100_000;     // 100 ms — cgroup v2 default

// Validation
if args.cpus <= 0.0 {
    bail!("--cpus must be positive (got {})", args.cpus);
}
let host_cpus = num_cpus::get() as f64;
if args.cpus > host_cpus {
    bail!("--cpus ({}) exceeds the host's CPU count ({})", args.cpus, host_cpus);
}

// Conversion: --cpus 0.5 with a 100 ms period becomes 50_000 100_000.
let cpu_quota_us = (args.cpus * CPU_PERIOD_US as f64) as u64;
```

Keeping the user-facing input intuitive (`--cpus 0.5`) while the internal API mirrors the kernel's exact model (`set_cpu_max(quota_us, period_us)`) cleanly separates the two concerns.

#### Trial and error: `EBUSY` on `cgroup.subtree_control`

The first attempt to run the integrated runtime failed not in our code but in `Cgroup::new`'s call to enable controllers:

```bash
$ cargo run -- run --mem 209715200 ./rootfs /bin/sh
Error: failed to enable controllers
Caused by:
    Device or resource busy (os error 16)
```

The diagnostic state was:

```bash
$ cat /sys/fs/cgroup/cgroup.subtree_control
                                            # empty: no controllers delegated
$ cat /sys/fs/cgroup/cgroup.controllers
cpuset cpu io memory hugetlb pids rdma      # but all are available

$ wc -l /sys/fs/cgroup/cgroup.procs
23 /sys/fs/cgroup/cgroup.procs              # 23 processes in this cgroup

$ mount | grep cgroup
cgroup on /sys/fs/cgroup type cgroup2 (rw,nosuid,nodev,...)
                                            # writable
```

The cause is cgroup v2's **"no internal process" rule**:

> A non-root cgroup may not simultaneously contain processes in its `cgroup.procs`, and delegate controllers to its children via `cgroup.subtree_control`.

The kernel enforces this to remove ambiguity in resource accounting between a parent's own processes and its child cgroups. The devcontainer the development was happening in *was* a non-root cgroup (Docker had assigned it a slice), it had 23 processes (the shell, `cargo`, the VS Code server), and `subtree_control` was empty. Writing `+memory` therefore failed with `EBUSY`.

Production runtimes solve this with the same pattern that systemd uses: move every existing process into a sibling sub-cgroup *before* enabling controllers in the parent. Concretely:

```text
Before:
/sys/fs/cgroup/
├── cgroup.procs                ← 23 PIDs
└── cgroup.subtree_control      ← empty

Migration:
mkdir /sys/fs/cgroup/init
move every PID from cgroup.procs → init/cgroup.procs
echo "+memory +cpu +pids" > /sys/fs/cgroup/cgroup.subtree_control

After:
/sys/fs/cgroup/
├── cgroup.procs                ← empty
├── cgroup.subtree_control      ← memory cpu pids
└── init/
    └── cgroup.procs            ← 23 PIDs
```

Because this development environment is itself a Docker container whose PID 1 and its children all live in a single cgroup, we do not implement the migration inside the runtime (it would have to move processes that don't belong to us, like the editor's language server). Instead, we automate it once at devcontainer startup.

`.devcontainer/cgroup_init.sh`:

```bash
#!/bin/bash
set -u
mkdir -p /sys/fs/cgroup/init

# Move every process out of the root cgroup.
# Repeat a few times to catch processes that spawn during migration.
for _ in 1 2 3 4 5; do
    for pid in $(cat /sys/fs/cgroup/cgroup.procs 2>/dev/null); do
        echo "$pid" > /sys/fs/cgroup/init/cgroup.procs 2>/dev/null || true
    done
done

remaining=$(wc -l < /sys/fs/cgroup/cgroup.procs)
if [ "$remaining" -gt 0 ]; then
    echo "warning: $remaining processes still in root cgroup:"
    cat /sys/fs/cgroup/cgroup.procs
fi

echo "+memory +cpu +pids" > /sys/fs/cgroup/cgroup.subtree_control \
    && echo "cgroup setup complete: $(cat /sys/fs/cgroup/cgroup.subtree_control)" \
    || echo "ERROR: failed to enable controllers"
```

`.devcontainer/devcontainer.json` wires it up plus passes the necessary Docker flags:

```json
{
  "runArgs": [
    "--privileged",
    "--cgroupns=private"
  ],
  "postStartCommand": ".devcontainer/cgroup_init.sh"
}
```

`--privileged` makes `/sys/fs/cgroup` writable, and `--cgroupns=private` gives the container its own cgroup namespace so it sees its assigned slice as `/`. Neither flag, by itself, enables controller delegation — only the migration script does that. Both flags require a full container *rebuild* (not just restart) to take effect, since they only apply at `docker create` time.

##### A subtle pitfall: running the migration as a script

A first attempt to run the migration interactively went like this:

```bash
$ bash .devcontainer/cgroup_init.sh
.devcontainer/cgroup_init.sh: line 19: echo: write error: Device or resource busy
```

The script *looked* identical to the working one. But running it from an interactive shell still hit `EBUSY` on the final `subtree_control` write. Why?

The answer is in how the shell starts a script. When you type `bash some-script.sh` at a prompt, your interactive shell does **not** run the script's commands itself. It `fork()`s a *new* bash process, which then reads and executes the script's lines:

```
[Interactive shell — PID 1559]            ← waits at the prompt for the script to finish
        │
        └── fork() + exec(bash)
                │
                └── [Script bash — PID 4498]   ← actually runs cgroup_init.sh
```

Both processes are in the same cgroup at the start: PID 1559 was already there, and PID 4498 inherited cgroup membership from its parent at the moment of fork. So `cgroup.procs` now has 24 PIDs, not 23.

The interactive shell at PID 1559 just sits at the prompt waiting for the script to return. It is alive, it is in `cgroup.procs`, but it isn't doing anything — no command of its own to fork from, no opportunity to migrate itself. Meanwhile other long-lived processes in this devcontainer (vscode-server, language servers, file watchers) keep spawning short-lived helpers, and many of those helpers land in `cgroup.procs` as they appear. The script's migration loop tries to drain the list five times to absorb this churn, but PID 1559 sits there the whole time, and the kernel checks `cgroup.procs` *at the moment* `subtree_control` is written:

```
Script's view at this moment:
  cgroup.procs still contains: [1559, ...]   ← parent shell + maybe a few stragglers
  Script tries:  echo "+memory ..." > /sys/fs/cgroup/cgroup.subtree_control
  Kernel checks: is cgroup.procs empty?  No → EBUSY
```

The fix is to **stop using a script wrapper** and type the commands directly into the interactive shell:

```bash
# Type these directly at the prompt, NOT via "bash some-script.sh"
$ mkdir -p /sys/fs/cgroup/init
$ for pid in $(cat /sys/fs/cgroup/cgroup.procs); do
    echo "$pid" > /sys/fs/cgroup/init/cgroup.procs 2>/dev/null
  done
$ echo "+memory +cpu +pids" > /sys/fs/cgroup/cgroup.subtree_control
```

Now there is no second bash. The interactive shell *itself* expands `$(cat ...)`, and the list it gets back **includes its own PID (1559)**. The loop sends `echo 1559 > init/cgroup.procs`, which moves the shell. From that moment on, every subsequent command typed at this shell — including the next `echo` to `subtree_control` — runs inside `init/`, so the writer of `subtree_control` is no longer "internal" to the root cgroup. The "no internal process" rule is satisfied, and the write succeeds.

Why, then, does the very same script work fine as a `postStartCommand`? Because at devcontainer boot time, **there is no interactive shell yet**. The Docker container has just started; only its entrypoint and a handful of system processes exist, and `postStartCommand` is invoked by the devcontainer infrastructure rather than from a user shell. The bash that runs the script is itself one of the few processes in the cgroup, and the migration loop sees and moves all of them — including itself. There is no separate "parent shell sitting at a prompt" left behind to keep `cgroup.procs` non-empty.

In short:

| Scenario | What happens | Result |
|---|---|---|
| Script run from an interactive shell | Parent shell stays at the prompt, never migrated | `EBUSY` |
| Inline commands in an interactive shell | That very shell is in the migration list and moves itself | succeeds |
| Script run before any interactive shell exists (postStartCommand) | No parent shell exists in the first place | succeeds |

#### Putting it all together

With the devcontainer prepared, the integrated startup sequence is:

```
run()
├── validate args (rootfs, cpus, mem, pids)
├── Cgroup::new()                     # creates /sys/fs/cgroup/rust_container_<hex>
├── pipe()                            # synchronization primitive
├── set_memory_max / set_cpu_max / set_pid_max
└── fork()
    ├── parent
    │   ├── add_pid(child)
    │   ├── write(pipe, 1)            # "child may proceed"
    │   ├── waitpid(child)
    │   └── Drop ⇒ rmdir cgroup
    └── setup_child (this is `child`)
        ├── read(pipe)                # blocks until parent signals
        ├── unshare(CLONE_NEWPID)
        └── fork()                    # grandchild inherits cgroup
            ├── parent (PID-namespace's reaper)
            └── grandchild (PID 1 in new namespace)
                ├── isolate_fs_pivot  # mount-ns + pivot_root + /proc
                └── execvp            # workload runs, fully constrained
```

#### Verification

The point of building this is not "the code compiles" but "the kernel does what we asked". Each limit is verified by trying to exceed it.

##### Memory limit

Run with a 200 MiB cap and have an `awk` process double a string until it OOMs

```bash
$ cargo run -- run --mem 209715200 ./rootfs /bin/sh
/ # awk 'BEGIN { s = "x"; while (1) s = s s }'
Killed
```

`Killed` is BusyBox's report that `SIGKILL` was delivered. 
The matching dmesg entry from the host confirms it was the cgroup OOM killer, and points at our cgroup by name

```text
oom-kill:constraint=CONSTRAINT_MEMCG, ...,
  oom_memcg=/docker/<devcontainer-id>/rust_container_1a0784d8c9d453cb,
  task_memcg=/docker/<devcontainer-id>/rust_container_1a0784d8c9d453cb,
  task=awk, pid=81708, uid=0
Memory cgroup out of memory: Killed process 81708 (awk)
  total-vm:1574596kB, anon-rss:191784kB, file-rss:120kB, shmem-rss:0kB,
  UID:0 pgtables:2480kB oom_score_adj:0
```

Three observations:

- **`CONSTRAINT_MEMCG`**: the OOM was triggered by the cgroup, not by host-wide memory pressure.
- **The path** `/docker/<devcontainer-id>/rust_container_1a0784d8c9d453cb` shows our cgroup as a grandchild of Docker's cgroup for this devcontainer. Our runtime really is just adding a directory under someone else's tree.
- **`anon-rss: 191784 kB ≈ 187 MiB`** is just under the 200 MiB limit. `awk` requested ~1.5 GiB of virtual memory but Linux only allocates physical pages on demand; the cgroup counts physical usage, so it triggered just before the next doubling would have crossed the limit.

##### PID limit

Run with `--pids 30`. The shell itself takes 1 PID; `seq 1..100` started in a `for` loop with `sleep 100 &` will start failing at the 30th fork:

```bash
$ cargo run -- run --pids 30 ./rootfs /bin/sh
/ # for i in $(seq 1 100); do echo "process $i"; sleep 100 & done
process 1
...
process 29
/bin/sh: can't fork: Resource temporarily unavailable
```

`EAGAIN` ("Resource temporarily unavailable") is the standard `errno` for a `fork` rejected by the kernel. From the host:

```bash
$ cat /sys/fs/cgroup/rust_container_*/pids.max
30
$ cat /sys/fs/cgroup/rust_container_*/pids.current
30
$ cat /sys/fs/cgroup/rust_container_*/pids.events
max 1                                       # limit was reached once
```

The same mechanism makes the classic shell fork-bomb `:(){ :|:&};:` survivable: inside the container it caps at 30 processes; on a host without a similar cgroup, it would bring down the system.

##### CPU limit

Run with `--cpus 0.5` (half a core), then start two infinite spin loops:

```bash
$ cargo run -- run --cpus 0.5 ./rootfs /bin/sh
/ # while true; do :; done &
/ # while true; do :; done &
```

From the host's `top`:

```text
  PID  USER      %CPU  COMMAND
 4582  root      25.2  sh        # spin loop #1
 4572  root      24.9  sh        # spin loop #2
                                 ────────
                                   50.1%  =  --cpus 0.5
```

Each loop wants 100% CPU. 
Together they would consume 200% on a multi-core host. 
The cgroup's `cpu.max = 50000 100000` (50 ms quota per 100 ms period) caps the *combined* usage of every process in the cgroup at 50% of one core, and the scheduler shares that fairly between the two.
Hence ~25% each. If only one loop were running, it would reach ~50% on its own. 
The limit is on the group, not on individual tasks.

##### Drop cleanup

After the container exits, the cgroup directory is gone:

```bash
# while container is running
$ ls /sys/fs/cgroup/ | grep rust_container
rust_container_1a0784d8c9d453cb

# after the container exits
$ ls /sys/fs/cgroup/ | grep rust_container
                            # nothing — RAII Drop removed it
```

If `rmdir` ever ran while the cgroup still contained processes, it would fail with `EBUSY`. 
The fact that cleanup succeeds confirms two things
- the workload really did run inside our cgroup (so all PIDs left when the container died).
- the parent process (the only one whose `Drop` fires) is the only owner that performs the cleanup.

### 4. New user namespace

Until now, we isolate the filesystem, PID namespace, and resources from the host process.
But the container runtime still has an important security problem.

If we run `whoami` inside the container:

```bash
cargo run -- run ./rootfs /bin/sh
whoami
```

we see

```bash
/ # whoami
root
```

This means the process is running as UID 0 inside the container.
In our current runtime, because we have not created a user namespace, this container root is also root from the host kernel's point of view.
If the container process somehow escapes the container environment, it may have real host-root privileges.  
User namespace isolation lets the process appear as root inside the container environment while mapping UID to an unprivileged UID on the host environment.

For example,
- Inside container: `UID 0`, username root.
- But in host environment: `UID 1000`, not privileged root account.

So `whoami` may still print `root` inside the container but that root account is no longer host root.

> To isolate container root from host root, we need a new user namespace.
> A user namespace isolates both user and group IDs.
> We can configure UID mappings with `uid_map` and GID mappings with `gid_map`.
> So UID/GID 0 inside the container can correspond to an unprivileged UID/GID on the host environment.

We can create a new user namespace with `unshare(CLONE_NEWUSER)`.
After that call, the setup process is no longer in the parent's user namespace. Any later children inherit this new user namespace.

However, `unshare(CLONE_NEWUSER)` alone is not enough.
The new namespace needs UID/GID mappings so the kernel can translate IDs between two views:

- **Inside**: the new user namespace used by the container process.
- **Outside**: the parent user namespace where the runtime process still lives.

If the kernel cannot translate a process's UID/GID into the current namespace, it displays the overflow ID `65534` (`nobody`). The kernel does not change every UID to `65534`; it uses `65534` only when an ID is unmapped and therefore cannot be represented in that namespace.

For example, if the mapping is:

```text
inside UID 0  ->  outside UID 1000
inside GID 0  ->  outside GID 1000
```

then the container process can appear as `root` inside the container while being treated as UID/GID `1000` outside the container.

The mapping file uses this format:
```text
inside_id outside_id count
```
`inside_id`: The UID/GID used inside the container user namespace.
`outside_id`: The UID/GID used in the parent user namespace.
`count`: Number of consecutive IDs to map.

The runtime writes these files for the setup process:

```text
/proc/<setup-pid>/uid_map
/proc/<setup-pid>/setgroups
/proc/<setup-pid>/gid_map
```

`setgroups` must be written before `gid_map`. This runtime writes `deny` there, which is the standard safe path for a single-ID GID mapping. After `setgroups` is denied, the process cannot call `setgroups(2)` inside that user namespace. That is why supplementary groups may still show as `65534(nobody)` even when the primary UID/GID are correctly mapped.

After the parent writes the mapping, the setup process switches to the mapped inside root identity:

```rust
setgid(Gid::from_raw(0))?;
setuid(Uid::from_raw(0))?;
```

This `0` is the **inside** UID/GID 0. With the mapping above, the same process is seen as UID/GID `1000` from the outside.

**Race condition and pipe**

User namespace setup needs a synchronization barrier, just like the cgroup setup.

The setup process is the one that calls `unshare(CLONE_NEWUSER)`, but the UID/GID mapping must be written by the parent process from outside that new user namespace. The setup process must not continue into PID/filesystem setup until the parent has finished writing the mapping.

The runtime therefore uses two extra pipes:

```text
userns_ready: setup process -> parent
mapping_done: parent -> setup process
```

The corrected sequence is:

```text
parent
├── add setup process to cgroup
├── signal cgroup_done
├── wait for userns_ready
├── write uid_map / setgroups / gid_map
├── signal mapping_done
└── waitpid(setup process)

setup process
├── wait for cgroup_done
├── unshare(CLONE_NEWUSER)
├── signal userns_ready
├── wait for mapping_done
├── setgid(0), setuid(0)
├── unshare(CLONE_NEWPID)
└── fork container init
```

Without the `mapping_done` barrier, the setup process could run mount, PID, or filesystem setup before its UID/GID are valid in the new user namespace.

**Experiment**

After the implementation, I checked the user namespace isolation by running the command below:
```bash
cargo run -- run ./rootfs --uid 1000 --gid 1000 /bin/sh
# and inside container, I also executed sleep 1000
```

Inside the container, the workload should see itself as root:

```bash
/ # id
uid=0(root) gid=0(root) groups=65534(nobody)
```

The supplementary group is `65534(nobody)` because `setgroups` is denied before `gid_map` is written. The important parts for this implementation are the primary UID and GID: both are `0` inside the container.

From the parent namespace, the actual workload should be visible as the mapped host UID/GID. The runtime and setup process may still appear as root because they are orchestration processes, but the executed container workload (`/bin/sh`, `sleep`) should appear as UID/GID `1000`.

```bash
root@db58918e2bfb:/app# ps -eo pid,ppid,user,uid,gid,comm,args | grep -E 'container-runtime|/bin/sh|sleep'
 2479  1283 root         0     0 container-runti target/debug/container-runtime run ./rootfs --uid 1000 --gid 1000 /bin/sh
 2489  2479 root         0     0 container-runti target/debug/container-runtime run ./rootfs --uid 1000 --gid 1000 /bin/sh
 2490  2489 1000      1000  1000 sh              /bin/sh
 2522  2490 1000      1000  1000 sleep           sleep 1000
root@db58918e2bfb:/app#
```

As we can see, the runtime/setup processes are still UID/GID `0`, but the actual workload processes are UID/GID `1000` from the outside. Inside the container, the same workload sees itself as UID/GID `0`.

This confirms the goal of user namespace isolation:

```text
inside container: UID/GID 0 (root)
outside runtime:  UID/GID 1000 (non-root)
```
