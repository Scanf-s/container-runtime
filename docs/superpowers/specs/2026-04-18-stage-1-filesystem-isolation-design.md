# Stage 1 — Filesystem Isolation (Design)

**Date:** 2026-04-18
**Status:** Approved (pending user review)
**Parent roadmap:** [2026-04-18-container-runtime-roadmap.md](2026-04-18-container-runtime-roadmap.md)

## Goal

Produce a working `container-runtime run <rootfs> <cmd> [args...]` binary that launches `<cmd>` with its filesystem view restricted to `<rootfs>`. Along the way, demonstrate *why* `chroot(2)` alone is insufficient and evolve to the correct `unshare(CLONE_NEWNS)` + `pivot_root(2)` implementation.

This stage covers **only** filesystem isolation. PID, network, UTS, IPC isolation, cgroups, and user namespaces are deferred to later stages.

## Staged delivery

Stage 1 lands as three commits, each independently runnable so the learning is visible in the git history.

### Commit 1.1 — chroot-only

Minimal runnable version. Uses plain `fork` + `chroot` + `execve`.

```
main → cli::parse() → runtime::run(args)
  runtime::run:
    match fork() {
      Parent(child_pid) => waitpid(child_pid)
      Child             => container::isolate_fs_chroot(&rootfs)?;
                           container::exec_cmd(&cmd, &args)?;
    }

  container::isolate_fs_chroot(rootfs):
    chroot(rootfs)
    chdir("/")
```

At this point, `cargo run -- run ./rootfs /bin/sh` drops the user into an alpine shell whose `ls /` differs from the host.

### Commit 1.2 — chroot escape demonstration

No production code changes. Adds an integration test under `tests/chroot_escape.rs` that:
1. Opens a file descriptor to an ancestor directory of the rootfs *before* calling `chroot`.
2. Calls `chroot(rootfs)`.
3. Uses `fchdir(fd)` followed by repeated `chdir("..")` to climb back to the real root.
4. Reads a file known to exist only on the host (e.g., `/etc/os-release` from the dev container).

Test passes when it successfully reads the host file — proving the chroot was escaped. The commit message documents the underlying reason: `chroot` only changes the resolution base for `/`; it does not remove the process's ability to reference paths by open file descriptor or to traverse `..` relative to those fds.

This commit exists to make the motivation for mount namespaces and `pivot_root` concrete rather than hand-waved.

### Commit 1.3 — mount namespace + pivot_root

Replaces `container::isolate_fs_chroot` with `container::isolate_fs_pivot`:

```
container::isolate_fs_pivot(rootfs):
  unshare(CloneFlags::CLONE_NEWNS)                          # new mount namespace

  # Stop any mount we do from propagating back to the host's mount namespace.
  mount(None, "/", None, MsFlags::MS_REC | MsFlags::MS_PRIVATE, None)

  # pivot_root requires new_root to be a mount point distinct from its parent.
  # Bind-mount the rootfs onto itself to satisfy that precondition.
  mount(Some(rootfs), rootfs, None, MsFlags::MS_BIND | MsFlags::MS_REC, None)

  # Put the old root inside the new root so we can unmount it afterwards.
  let put_old = rootfs.join(".old_root")
  create_dir_all(put_old)
  pivot_root(rootfs, put_old)

  chdir("/")
  umount2("/.old_root", MntFlags::MNT_DETACH)
  remove_dir("/.old_root")

  # /proc will be needed by most commands (ps, top, self-introspection).
  create_dir_all("/proc")
  mount(Some("proc"), "/proc", Some("proc"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC, None)
```

The parent stays in the host mount namespace and only waits on the child. Cleanup of the mount namespace is automatic: when the child exits, the kernel tears down its namespace, and the bind-mount artifacts vanish with it.

After this commit, the escape test from 1.2 should fail (the host `/etc/os-release` is no longer reachable), and `mount` inside the container lists only entries we explicitly set up.

## Code layout

```
container-runtime/
├── Cargo.toml
├── Dockerfile.dev
├── scripts/
│   └── fetch-rootfs.sh
├── docs/superpowers/specs/
│   ├── 2026-04-18-container-runtime-roadmap.md
│   └── 2026-04-18-stage-1-filesystem-isolation-design.md
├── src/
│   ├── main.rs            # thin entry, delegates to cli + runtime
│   ├── cli.rs             # clap-derived RunArgs { rootfs: PathBuf, cmd: String, args: Vec<String> }
│   ├── runtime.rs         # fork/waitpid split
│   └── container.rs       # isolate_fs_chroot / isolate_fs_pivot / exec_cmd
└── tests/
    └── chroot_escape.rs   # lands in commit 1.2
```

The existing `src/main.rs` (CPU-usage printer) is removed in commit 1.1.

### Responsibilities

- **`main.rs`** — parse args, dispatch. No business logic.
- **`cli.rs`** — `clap` derive, one subcommand `run` with positional `<rootfs> <cmd> [args...]`. Returns a typed `RunArgs`.
- **`runtime.rs`** — owns the `fork()` split. Parent path: `waitpid` and propagate exit status. Child path: call into `container::isolate_fs_*` then `container::exec_cmd`. This is the module that will later grow the re-exec / clone(2) pattern in Stage 2.
- **`container.rs`** — all syscalls that run *inside the child after fork*. In Stage 1: `isolate_fs_chroot` (1.1), `isolate_fs_pivot` (1.3), `exec_cmd`. Subsequent stages add more functions here.

## Dependencies

**Add:**
- `nix` (with features for `mount`, `sched`, `unistd`, `sys`)
- `clap` (with `derive` feature)
- `anyhow` — error handling. Chosen over custom error enums to keep the learning code readable; container-runtime is not a library and doesn't need a stable error taxonomy.

**Remove:**
- `sysinfo` — leftover from the CPU-usage experiment.

## Dev environment

`Dockerfile.dev`:

```dockerfile
FROM rust:1-slim-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
      curl ca-certificates iproute2 procps util-linux \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /workspace
```

Dev loop:

```bash
docker build -f Dockerfile.dev -t crt-dev .
docker run --rm -it --privileged \
  -v "$PWD:/workspace" \
  crt-dev bash

# inside the dev container:
./scripts/fetch-rootfs.sh            # one-time
cargo build
cargo run -- run ./rootfs /bin/sh
cargo test
```

`--privileged` is used because `unshare(CLONE_NEWNS)`, `mount`, and `pivot_root` all require `CAP_SYS_ADMIN`. Tightening to specific capabilities is deliberately left to Stage 5 (privilege drop), where that is the subject being studied.

## Rootfs fetch script

`scripts/fetch-rootfs.sh` — idempotent, architecture-aware, no-op when rootfs already present.

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOTFS_DIR="${1:-./rootfs}"
VERSION="3.20.3"
ARCH="$(uname -m)"
URL="https://dl-cdn.alpinelinux.org/alpine/v${VERSION%.*}/releases/${ARCH}/alpine-minirootfs-${VERSION}-${ARCH}.tar.gz"

if [ -d "$ROOTFS_DIR" ] && [ -n "$(ls -A "$ROOTFS_DIR" 2>/dev/null)" ]; then
  echo "rootfs exists at $ROOTFS_DIR — skipping"; exit 0
fi
mkdir -p "$ROOTFS_DIR"
curl -fsSL "$URL" | tar -xz -C "$ROOTFS_DIR"
echo "extracted alpine-minirootfs to $ROOTFS_DIR"
```

`./rootfs/` is added to `.gitignore`.

## Error handling

- All fallible syscalls return `anyhow::Result<T>` with `.context("what we were trying to do")`.
- Child-side errors: after fork, if the child fails before `execve`, it prints the error to stderr and exits with code 127 (shell convention for "exec failed"). Parent translates child wait status into its own exit code.
- Preconditions checked up front in the parent, before fork: rootfs directory exists, is a directory, is not empty. Failing early avoids a fork+crash cycle during learning.

## Verification plan

After each commit, the author manually runs the checks below. A passing set is the gate to moving on.

**After Commit 1.1:**
```
$ cargo run -- run ./rootfs /bin/sh
/ # ls /                   # alpine layout (bin, etc, lib, …)
/ # cat /etc/os-release    # NAME="Alpine Linux"
# on host shell:
$ ps -ef | grep sh         # shows child as a normal process, no namespace
```

**After Commit 1.2:**
```
$ cargo test chroot_escape
# passes — i.e., escape succeeded, confirming chroot is not a boundary
```
The escape test requires `CAP_SYS_CHROOT` (available in the `--privileged` dev container) and an extracted rootfs at `./rootfs`. If the rootfs is absent, the test fails fast with a clear message rather than silently skipping.

**After Commit 1.3:**
```
$ cargo run -- run ./rootfs /bin/sh
/ # mount                  # only rootfs bind-mount + /proc; no host mounts
/ # mount -t tmpfs t /tmp  # succeeds
# on host (separate shell):
$ mount | grep tmpfs       # no new tmpfs — confirms mount isolation
$ cargo test chroot_escape # now FAILS — escape no longer works
```

## Out of scope for Stage 1

These are deliberately deferred, even though they will be familiar from container usage:

- PID namespace / `ps` showing PID 1 — Stage 2.
- `/dev`, `/sys`, `/dev/pts` mounts — revisit once PID and user namespaces exist (Stages 2, 5).
- Signal forwarding (SIGTERM to parent → container) — Stage 2.
- Any network, hostname, cgroup, or capability handling — Stages 3–5.
- `config.json` / OCI spec concepts — deferred past Stage 5.

## Open questions

None at design approval. Decisions already locked in:
- `nix` crate for syscalls (approved).
- Alpine minirootfs source (approved).
- Docker `--privileged` dev container (approved).
- Three-commit progression with escape demo in the middle (approved).
