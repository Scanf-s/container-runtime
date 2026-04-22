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

Because of above security problem, modern container runtime uses `pivot_root` system call to isolate filesystem more secure way.

