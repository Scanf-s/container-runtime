# Learning notes

These notes follow the runtime's development through incremental implementations and experiments. Code snippets and terminal output describe those stages; see the [project README](../README.md) for the current setup and run commands.

Suggested reading order:

1. [Basic system calls](system-calls.md): `clone`, `unshare`, `setns`, `execve`, mounts, and `pivot_root`.
2. [Filesystem isolation](filesystems.md): `chroot`, its limitations, mount namespaces, and `pivot_root`.
3. [Process namespaces](namespaces.md): PID namespaces, the double-fork pattern, and `/proc`.
4. [Resource limits with cgroups](cgroups.md): cgroup v2, CPU/memory/PID limits, synchronization, and cleanup.
5. [User namespaces and UID/GID mapping](user-mapping.md): mapping container root to a host user and coordinating setup.

For commands and example screenshots, see [Verify the isolation](verification.md).
