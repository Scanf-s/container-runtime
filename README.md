# Container Runtime

Container Runtime implementation with Rust

This repository is for the general container runtime implementation using Rust language.
It is not for the production system, it is only for study purpose myself.

## Concept

### 0. Basic System Call

To implement container runtime, we need to understand the basic system call.

#### Clone

The clone() system call creates a new child process.  
While similar to fork(), clone() allows for specific flags such as `CLONE_NEWPID`, `CLONE_NEWNET`, and `CLONE_NEWNS`.  
These flags ensure that the child process is created within a new namespace, isolated from the parent's system resources.  

#### Unshare

The unshare() system call disassociates parts of the current process's execution context (namespace).  
Unlike clone(), which creates a new process, unshare() allows the calling process to detach itself from its current namespace (such as the mount namespace)  
and move into a new, isolated one.

#### Setns

The setns() system call associates the current process with an existing namespace.  
This is utilized in commands like docker exec, allowing a new process (like `/bin/bash`) to be injected into a currently running container.

#### Execve

The execve() system call replaces the current process's memory space with a new program.  
After the namespace and filesystem isolation tasks are complete, execve overwrites the process's memory with the target container application, such as `/bin/sh`,  
and transfers execution control to that application.

#### Mount/Unmount

This system call connects or disconnects specific filesystem into the directory tree.  
If we create container-only file system like `/proc` within the host filesystem,  
We can mount this directory into the container filesystem (bind mount).  

#### Pivot_root

This system call replaces the current root mount point with a new one and moves the old root filesystem to a designated path.  
By doing so, the process effectively loses access to the host's filesystem, enhancing security. The following steps outline how to use this system call.

1. Call `unshare`: Create a new mount namespace to isolate the mount points from the host.
2. Prepare the new root: Designate a specific directory (`/rootfs`) and ensure it is a mount point.
3. Call `pivot_root`: Set `/rootfs` as the new root and move the original root to a subdirectory within the new root (/rootfs/old_root).
4. Unmount the old root: Use umount -l on the subdirectory containing the old root to completely remove the host's filesystem from the container's view.
5. Change directory: Use chdir("/") to ensure the current working directory is updated to the new root.

### 1. Filesystem Isolation

