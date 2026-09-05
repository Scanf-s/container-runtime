# Basic system calls

[Learning notes](README.md) · [Getting started](../README.md)

To implement a container runtime, we first need to understand a handful of basic system calls.

## Clone

The `clone()` system call creates a new child process.  
While similar to `fork()`, `clone()` accepts flags such as `CLONE_NEWPID`, `CLONE_NEWNET`, and `CLONE_NEWNS`.  
These flags cause the child to be created inside a new namespace, isolated from the parent's system resources.

## Unshare

The `unshare()` system call disassociates parts of the calling process's execution context (its namespaces).  
Unlike `clone()`, which creates a new process, `unshare()` lets the current process detach from one of its existing namespaces (for example, the mount namespace) and move into a new, isolated one.

<img width="1440" height="1200" alt="image" src="https://github.com/user-attachments/assets/7574defd-58a8-4aea-83fc-5ebe1a6f247c" />

## Setns

The `setns()` system call attaches the calling process to an existing namespace.  
This is what powers commands like `docker exec`, which inject a new process (such as `/bin/bash`) into a namespace that belongs to an already-running container.

## Execve

The `execve()` system call replaces the current process's memory image with a new program.  
Once namespace setup and filesystem isolation are done, `execve` overwrites the process's memory with the target container application (e.g. `/bin/sh`) and hands execution control over to it.

## Mount / Unmount

These system calls attach or detach a filesystem to or from the directory tree.  
For example, we can mount a dedicated `/proc` inside the container's filesystem, or use a bind mount to expose a specific host directory to the container.

## Pivot_root

This system call swaps the current root mount with a new one and moves the old root filesystem to a designated path.  
After the pivot, the process effectively loses access to the host's filesystem, which significantly improves isolation. The typical steps are:

1. Call `unshare`: create a new mount namespace so subsequent mount changes don't leak back to the host.
2. Prepare the new root: designate a specific directory (e.g. `/rootfs`) and make sure it is a mount point.
3. Call `pivot_root`: set `/rootfs` as the new root and move the original root into a subdirectory beneath it (e.g. `/rootfs/old_root`).
4. Unmount the old root: run `umount -l` (lazy unmount) on that subdirectory to fully detach the host's filesystem from the container's view.
5. Change directory: call `chdir("/")` so the working directory follows the new root.

<img width="1440" height="800" alt="image" src="https://github.com/user-attachments/assets/8240d345-3b26-4266-8137-498e78293051" />
