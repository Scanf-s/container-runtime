# User namespaces and UID/GID mapping

[Learning notes](README.md) · [Getting started](../README.md)

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
