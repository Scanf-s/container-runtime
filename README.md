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
