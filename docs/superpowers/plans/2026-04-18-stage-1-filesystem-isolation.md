# Stage 1 — Filesystem Isolation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a minimal `container-runtime run <rootfs> <cmd> [args...]` binary in Rust that isolates its child's filesystem view, first via `chroot(2)` and finally via `unshare(CLONE_NEWNS)` + `pivot_root(2)`. Along the way, demonstrate (via an integration test) that `chroot` alone can be escaped.

**Architecture:** A thin `main.rs` parses CLI, `runtime.rs` owns the `fork()`+`waitpid()` split, and `container.rs` holds the child-side isolation work (chroot in Commit 1.1; `unshare`+`pivot_root`+`/proc` in Commit 1.3). An integration test under `tests/chroot_escape.rs` demonstrates chroot's limitations between those commits.

**Tech Stack:** Rust edition 2024; `nix 0.29` (safe syscall wrappers); `clap 4` with `derive` (CLI); `anyhow` (error context). All building, running, and testing happens inside a privileged Debian-based Docker dev container; host is macOS.

**Parent spec:** [`docs/superpowers/specs/2026-04-18-stage-1-filesystem-isolation-design.md`](../specs/2026-04-18-stage-1-filesystem-isolation-design.md)

---

## File Structure

| Path | Create / Modify | Purpose |
|---|---|---|
| `Dockerfile.dev` | Create | Privileged Linux dev container image |
| `.gitignore` | Create | Ignore `/target`, `/rootfs` |
| `scripts/fetch-rootfs.sh` | Create | Idempotent Alpine minirootfs fetcher |
| `Cargo.toml` | Modify | Drop `sysinfo`; add `nix`, `clap`, `anyhow` |
| `src/main.rs` | Replace | Thin entry: parse CLI, dispatch |
| `src/cli.rs` | Create | `clap`-derived `Cli`, `Command::Run(RunArgs)` |
| `src/runtime.rs` | Create | `fork()`/`waitpid()` parent/child split |
| `src/container.rs` | Create | Child-side isolation primitives + `execvp` |
| `tests/chroot_escape.rs` | Create (Task 6) | Integration test proving chroot is escapable |

Each module has one responsibility. `container.rs` is the file that grows across Stages 2–5; `runtime.rs` will grow when PID namespace forces the re-exec pattern in Stage 2.

---

## Task 1: Dev environment (Dockerfile, fetch script, .gitignore)

**Files:**
- Create: `Dockerfile.dev`
- Create: `scripts/fetch-rootfs.sh`
- Create: `.gitignore`

- [x] **Step 1.1: Create `.gitignore`**

Create `.gitignore` with:

```gitignore
/target
/rootfs
```

- [x] **Step 1.2: Create `Dockerfile.dev`**

Create `Dockerfile.dev` with:

```dockerfile
FROM rust:1-slim-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
      curl ca-certificates iproute2 procps util-linux \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /workspace
```

- [x] **Step 1.3: Create `scripts/fetch-rootfs.sh`**

Create `scripts/fetch-rootfs.sh` with:

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOTFS_DIR="${1:-./rootfs}"
VERSION="3.20.3"
ARCH="$(uname -m)"
URL="https://dl-cdn.alpinelinux.org/alpine/v${VERSION%.*}/releases/${ARCH}/alpine-minirootfs-${VERSION}-${ARCH}.tar.gz"

if [ -d "$ROOTFS_DIR" ] && [ -n "$(ls -A "$ROOTFS_DIR" 2>/dev/null)" ]; then
  echo "rootfs exists at $ROOTFS_DIR — skipping"
  exit 0
fi

mkdir -p "$ROOTFS_DIR"
echo "fetching $URL"
curl -fsSL "$URL" | tar -xz -C "$ROOTFS_DIR"
echo "extracted alpine-minirootfs to $ROOTFS_DIR"
```

Then make it executable:

```bash
chmod +x scripts/fetch-rootfs.sh
```

- [x] **Step 1.4: Build the dev image and fetch the rootfs**

On the macOS host, run:

```bash
docker build -f Dockerfile.dev -t crt-dev .
docker run --rm -it --privileged -v "$PWD:/workspace" crt-dev bash -c './scripts/fetch-rootfs.sh && ls rootfs'
```

Expected: `bin dev etc home lib media mnt opt proc root run sbin srv sys tmp usr var` printed, confirming the rootfs is in place.

- [x] **Step 1.5: Commit**

```bash
git add .gitignore Dockerfile.dev scripts/fetch-rootfs.sh
git commit -m "chore: add privileged Docker dev env and alpine rootfs fetcher"
```

---

## Task 2: Swap Cargo dependencies

**Files:**
- Modify: `Cargo.toml`

- [x] **Step 2.1: Replace `Cargo.toml` contents**

Open `Cargo.toml` and replace its entire contents with:

```toml
[package]
name = "container-runtime"
version = "0.1.0"
edition = "2024"

[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
nix = { version = "0.29", features = ["fs", "mount", "process", "sched", "user"] }
```

- [x] **Step 2.2: Verify the dependencies resolve**

From inside the dev container (see Task 1, Step 1.4 for how to start it):

```bash
cargo check
```

Expected: compilation fails with errors about the old `src/main.rs` referencing `sysinfo` — that is fine; Task 3 replaces `main.rs`. The dependencies themselves must resolve without error.

If you see an error like `no matching package found for sysinfo` from Cargo.lock, run `rm Cargo.lock && cargo check` to regenerate it.

- [x] **Step 2.3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: swap sysinfo for nix/clap/anyhow"
```

---

## Task 3: CLI module

**Files:**
- Create: `src/cli.rs`

- [ ] **Step 3.1: Create `src/cli.rs`**

Create `src/cli.rs` with:

```rust
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "container-runtime", version, about = "A toy container runtime for learning")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run a command inside an isolated rootfs.
    Run(RunArgs),
}

#[derive(Parser, Debug)]
pub struct RunArgs {
    /// Path to the rootfs directory (e.g. ./rootfs).
    pub rootfs: PathBuf,

    /// Command to execute inside the container.
    pub cmd: String,

    /// Arguments to pass to the command.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}
```

(Commit happens at end of Task 6, after the whole Commit 1.1 slice compiles and runs.)

---

## Task 4: Container module (chroot version)

**Files:**
- Create: `src/container.rs`

- [x] **Step 4.1: Create `src/container.rs`**

Create `src/container.rs` with:

```rust
use anyhow::{Context, Result};
use nix::unistd::{chdir, chroot, execvp};
use std::ffi::CString;
use std::path::Path;

/// Restrict the process's view of the filesystem to `rootfs` using chroot(2).
///
/// NOTE: This is intentionally insufficient for real isolation; it does not
/// create a mount namespace, and the process retains enough capability to
/// escape via open file descriptors. Stage 1 upgrades this to pivot_root.
pub fn isolate_fs_chroot(rootfs: &Path) -> Result<()> {
    chroot(rootfs).with_context(|| format!("chroot({:?})", rootfs))?;
    chdir("/").context("chdir(\"/\") after chroot")?;
    Ok(())
}

/// Replace the current process with `cmd` + `args` via execvp(3).
/// Returns only on failure.
pub fn exec_cmd(cmd: &str, args: &[String]) -> Result<()> {
    let c_cmd = CString::new(cmd).context("cmd contains a nul byte")?;

    let mut c_args: Vec<CString> = Vec::with_capacity(args.len() + 1);
    c_args.push(c_cmd.clone()); // argv[0]
    for a in args {
        c_args.push(CString::new(a.as_str()).context("arg contains a nul byte")?);
    }

    execvp(&c_cmd, &c_args).with_context(|| format!("execvp({:?})", cmd))?;
    unreachable!("execvp returns only on failure");
}
```

---

## Task 5: Runtime module (fork/waitpid split)

**Files:**
- Create: `src/runtime.rs`

- [x] **Step 5.1: Create `src/runtime.rs`**

Create `src/runtime.rs` with:

```rust
use anyhow::{bail, Context, Result};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{fork, ForkResult};
use std::process::ExitCode;

use crate::cli::RunArgs;
use crate::container;

pub fn run(args: RunArgs) -> Result<ExitCode> {
    // Precheck in the parent so a bad rootfs doesn't cost us a fork.
    if !args.rootfs.is_dir() {
        bail!("rootfs {:?} does not exist or is not a directory", args.rootfs);
    }

    // SAFETY: the child calls execvp (or exits) before returning to any Rust
    // code that could observe duplicated resources. No destructors need to
    // run across the fork boundary.
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
            // Any error path in the child must _not_ return to the parent.
            if let Err(e) = child_main(args) {
                eprintln!("container-runtime: child failed: {e:#}");
                std::process::exit(127);
            }
            unreachable!("child_main either execs or exits");
        }
    }
}

fn child_main(args: RunArgs) -> Result<()> {
    container::isolate_fs_chroot(&args.rootfs)?;
    container::exec_cmd(&args.cmd, &args.args)?;
    unreachable!();
}
```

---

## Task 6: Wire up `main.rs` and verify Commit 1.1

**Files:**
- Replace: `src/main.rs`

- [ ] **Step 6.1: Replace `src/main.rs`**

Replace the entire contents of `src/main.rs` with:

```rust
use anyhow::Result;
use clap::Parser;
use std::process::ExitCode;

mod cli;
mod container;
mod runtime;

fn main() -> Result<ExitCode> {
    let cli = cli::Cli::parse();
    match cli.command {
        cli::Command::Run(args) => runtime::run(args),
    }
}
```

- [x] **Step 6.2: Build inside the dev container**

From inside the privileged dev container (bind-mount `$PWD:/workspace`), run:

```bash
cargo build
```

Expected: `Finished \`dev\` profile [unoptimized + debuginfo] target(s)` with no errors.

- [x] **Step 6.3: Run the runtime against alpine**

Still inside the dev container:

```bash
cargo run -- run ./rootfs /bin/sh -c 'cat /etc/os-release; echo PID=$$; ls /'
```

Expected output (exact strings matter — they confirm isolation is in effect):

- `NAME="Alpine Linux"` somewhere in the output
- A line like `PID=1234` with an arbitrary number (no PID namespace yet — that's Stage 2)
- Directory listing showing alpine's layout: `bin dev etc home lib media mnt opt proc root run sbin srv sys tmp usr var` (NOT your dev container's layout)

If you instead see `NAME="Debian GNU/Linux"` or your dev container's `/workspace` directory, chroot did not take effect — revisit `isolate_fs_chroot`.

- [x] **Step 6.4: Commit (this is Commit 1.1 of the spec)**

```bash
git add src/cli.rs src/container.rs src/runtime.rs src/main.rs
git commit -m "feat: chroot-only 'run' subcommand (Stage 1, Commit 1.1)

Implements container-runtime run <rootfs> <cmd> [args...] using fork +
chroot + execvp. This is deliberately insufficient for real isolation:
no mount namespace, and chroot can be escaped (demonstrated in the next
commit). See docs/superpowers/specs/2026-04-18-stage-1-*.md."
```

---

## Task 7: Chroot escape integration test (Commit 1.2)

**Files:**
- Create: `tests/chroot_escape.rs`

- [ ] **Step 7.1: Write the failing (well, passing-via-escape) integration test**

Create `tests/chroot_escape.rs` with:

```rust
//! Demonstrates that chroot(2) alone is not a security boundary.
//!
//! The test:
//!   1. Opens a file descriptor to the host `/` BEFORE calling chroot.
//!   2. Calls chroot(<rootfs>) so absolute paths now resolve inside rootfs.
//!   3. Uses fchdir(fd) to jump back to the host root via the open fd.
//!   4. Walks up with `chdir("..")` until reaching the real filesystem root.
//!   5. Reads a file known to exist on the dev-container host but NOT in the
//!      alpine rootfs, proving we escaped.
//!
//! Marked `#[ignore]` because it requires CAP_SYS_CHROOT and a populated
//! ./rootfs directory. Run with:
//!
//!     cargo test --test chroot_escape -- --ignored
//!
//! Expected result in Stage 1 Commit 1.2: PASSES (escape succeeds).
//! After Stage 2+ introduces PID namespaces and re-exec, this test still
//! demonstrates the underlying chroot weakness — it does not test our
//! runtime binary, which uses pivot_root as of Commit 1.3.

use std::fs;
use std::os::fd::AsRawFd;
use std::path::Path;

#[test]
#[ignore = "requires CAP_SYS_CHROOT and ./rootfs; run with --ignored"]
fn chroot_can_be_escaped_via_retained_fd() {
    let rootfs = Path::new("./rootfs");
    assert!(
        rootfs.is_dir(),
        "rootfs not found at {:?}; run scripts/fetch-rootfs.sh first",
        rootfs
    );

    // Sentinel present on the Debian-based dev container host, absent from alpine rootfs.
    let sentinel_abs = "/etc/debian_version";
    assert!(
        Path::new(sentinel_abs).exists(),
        "host sentinel {sentinel_abs} not found — this test expects the Debian-based dev container"
    );

    // Step 1: open fd to host root BEFORE chroot.
    let host_root = fs::File::open("/").expect("open host /");

    // Step 2: chroot into the rootfs.
    nix::unistd::chroot(rootfs).expect("chroot failed");
    nix::unistd::chdir("/").expect("chdir / inside chroot");

    // Sanity: after chroot, the sentinel must NOT be reachable via its absolute path.
    assert!(
        !Path::new(sentinel_abs).exists(),
        "sentinel reachable via absolute path — chroot did not take effect"
    );

    // Step 3: fchdir back to the retained host-root fd.
    nix::unistd::fchdir(host_root.as_raw_fd()).expect("fchdir to host root fd");

    // Step 4: climb up until we reach the actual filesystem root.
    // 64 iterations is overkill — a normal dev container root is 1–2 levels deep.
    for _ in 0..64 {
        nix::unistd::chdir("..").expect("chdir ..");
    }

    // Step 5: read the sentinel via RELATIVE path (leading '/' would still resolve
    // inside the chroot). This works because we are now CWD'd at the real host root.
    let relative = sentinel_abs.trim_start_matches('/');
    let contents = fs::read_to_string(relative)
        .expect("reading host sentinel via escape failed — chroot was NOT escaped");

    assert!(
        !contents.is_empty(),
        "host sentinel was readable but empty — unexpected"
    );
}
```

- [ ] **Step 7.2: Run the test and confirm the escape succeeds**

From inside the dev container:

```bash
cargo test --test chroot_escape -- --ignored
```

Expected: `test chroot_can_be_escaped_via_retained_fd ... ok` — the test *passes*, which means the escape succeeded. That is the whole point: chroot is not a security boundary.

If the test fails, read the failure message carefully. A common cause is the dev container not being `--privileged` (so `chroot` itself fails with `EPERM`).

- [ ] **Step 7.3: Commit (this is Commit 1.2 of the spec)**

```bash
git add tests/chroot_escape.rs
git commit -m "test: demonstrate chroot escape via retained fd (Stage 1, Commit 1.2)

chroot(2) does not rewrite open file descriptors. By opening an fd to /
before chroot and then using fchdir() + chdir('..'), a process inside
the chroot can return to the host filesystem. This makes the case for
mount namespaces + pivot_root, which Commit 1.3 implements."
```

---

## Task 8: Upgrade to `unshare(CLONE_NEWNS)` + `pivot_root` (Commit 1.3)

**Files:**
- Modify: `src/container.rs`
- Modify: `src/runtime.rs`

- [ ] **Step 8.1: Add `isolate_fs_pivot` to `src/container.rs`**

Append the following to `src/container.rs` (keep `isolate_fs_chroot` and `exec_cmd` for reference and for the Stage 2 tests later — if you want it deleted now, see Step 8.5):

```rust
use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::sched::{unshare, CloneFlags};
use nix::unistd::pivot_root;
use std::fs;

/// Full Stage 1 filesystem isolation: new mount namespace, pivot_root into the
/// provided rootfs, detach the old root, and mount a fresh /proc.
pub fn isolate_fs_pivot(rootfs: &Path) -> Result<()> {
    // 1. New mount namespace for this process.
    unshare(CloneFlags::CLONE_NEWNS).context("unshare(CLONE_NEWNS)")?;

    // 2. Make "/" recursively private so any mount we perform below does not
    //    propagate back to the host's mount namespace.
    mount::<str, _, str, str>(
        None,
        "/",
        None,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None,
    )
    .context("mount / MS_REC|MS_PRIVATE")?;

    // 3. pivot_root(2) requires new_root to be a mount point distinct from
    //    its parent. Bind-mount the rootfs onto itself to satisfy that.
    mount::<_, _, str, str>(
        Some(rootfs),
        rootfs,
        None,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None,
    )
    .with_context(|| format!("bind-mount {:?} onto itself", rootfs))?;

    // 4. Create a directory inside the new root to receive the old root.
    let old_root = rootfs.join(".old_root");
    fs::create_dir_all(&old_root).with_context(|| format!("create_dir_all {:?}", old_root))?;

    // 5. Pivot.
    pivot_root(rootfs, old_root.as_path())
        .with_context(|| format!("pivot_root({:?}, {:?})", rootfs, old_root))?;

    // 6. Chdir into the new root so relative paths below are sane.
    chdir("/").context("chdir(\"/\") after pivot_root")?;

    // 7. Detach the old root (it is still visible at /.old_root) and remove
    //    the stub directory. MNT_DETACH because files may still be held open.
    umount2("/.old_root", MntFlags::MNT_DETACH).context("umount2(/.old_root)")?;
    fs::remove_dir("/.old_root").context("remove_dir(/.old_root)")?;

    // 8. Mount a fresh /proc inside the container. Needed by ps, top,
    //    /proc/self/*, and almost anything interactive.
    fs::create_dir_all("/proc").context("create /proc")?;
    mount::<_, _, _, str>(
        Some("proc"),
        "/proc",
        Some("proc"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
        None,
    )
    .context("mount /proc")?;

    Ok(())
}
```

- [ ] **Step 8.2: Switch `runtime::child_main` to call `isolate_fs_pivot`**

In `src/runtime.rs`, replace the body of `child_main` so it calls the new function:

```rust
fn child_main(args: RunArgs) -> Result<()> {
    container::isolate_fs_pivot(&args.rootfs)?;
    container::exec_cmd(&args.cmd, &args.args)?;
    unreachable!();
}
```

Leave `isolate_fs_chroot` in `container.rs` — it's dead code now but is educational in the diff history and will be deleted (or repurposed) in a later stage. If you prefer a clean tree now, see Step 8.5.

- [ ] **Step 8.3: Build and run**

From inside the privileged dev container:

```bash
cargo build
cargo run -- run ./rootfs /bin/sh
```

Expected: an interactive Alpine shell prompt (`/ #`). If you get `unshare: Operation not permitted`, the dev container is not `--privileged` — fix that before continuing.

- [ ] **Step 8.4: Verify mount isolation manually**

Inside the shell spawned by the runtime:

```sh
mount
# Expect to see only a handful of entries: the rootfs bind-mount and /proc.
# You should NOT see /workspace, /var/lib/docker, or any host mounts.

mount -t tmpfs tmpfs /tmp
mount | grep tmpfs
# Expect the new tmpfs entry.
```

Now open a *second* shell on the host (`docker exec -it <container> bash` into the same dev container):

```bash
mount | grep tmpfs
# Expect the tmpfs from the inner shell to NOT appear here.
# If it does, mount propagation was not correctly blocked — review the MS_PRIVATE step.
```

Exit the inner shell (`exit`), and confirm the runtime process exits cleanly.

- [ ] **Step 8.5 (optional cleanup): Remove dead `isolate_fs_chroot`**

If you want a clean tree — delete `isolate_fs_chroot` from `src/container.rs`. Git history preserves the Commit 1.1 version. Skip this step if you want the function to stay for reference.

- [ ] **Step 8.6: Re-run the chroot_escape test unchanged**

```bash
cargo test --test chroot_escape -- --ignored
```

Expected: the test still *passes* — it is a standalone demonstration of the `chroot(2)` syscall's limitations and does not exercise your runtime binary. What changed is that your runtime no longer uses `chroot` as its primary isolation mechanism, so the attack class demonstrated by this test no longer applies to containers launched by `container-runtime run`.

Leaving the test in place is deliberate: it is documentation-as-code for *why* the pivot_root work was necessary.

- [ ] **Step 8.7: Commit (this is Commit 1.3 of the spec)**

```bash
git add src/container.rs src/runtime.rs
git commit -m "feat: pivot_root + mount namespace isolation (Stage 1, Commit 1.3)

Replaces chroot-based isolation with unshare(CLONE_NEWNS) + pivot_root +
a dedicated /proc mount. Mounts performed inside the container no longer
propagate to the host's mount namespace, closing the filesystem-isolation
gap demonstrated by the chroot_escape test."
```

---

## Task 9: Final verification & README touch-up

**Files:**
- Modify: `README.md`

- [ ] **Step 9.1: Smoke-test the end-state**

Inside the privileged dev container:

```bash
cargo run -- run ./rootfs /bin/sh -c 'echo inside; ls /; mount | wc -l'
```

Expected: prints `inside`, then alpine's top-level dirs, then a small number (typically 2) for the mount count — only the rootfs and `/proc`. Exits cleanly with status 0.

- [ ] **Step 9.2: Update `README.md`**

Open `README.md` and replace its contents with:

```markdown
# Container Runtime

A toy container runtime implementation in Rust, built for self-study of Linux
virtualization primitives. Not intended for production use.

## Stage 1 (current): Filesystem isolation

The `run` subcommand launches a command with its filesystem view restricted
to a provided rootfs, using a new mount namespace + `pivot_root(2)`.

```sh
./scripts/fetch-rootfs.sh
cargo run -- run ./rootfs /bin/sh
```

Requires `CAP_SYS_ADMIN` (available inside a `--privileged` Docker container —
see `Dockerfile.dev`).

## Development

```sh
docker build -f Dockerfile.dev -t crt-dev .
docker run --rm -it --privileged -v "$PWD:/workspace" crt-dev bash
# inside:
./scripts/fetch-rootfs.sh
cargo build
cargo test -- --ignored   # integration tests need privileges
```

## Roadmap

See [`docs/superpowers/specs/2026-04-18-container-runtime-roadmap.md`](docs/superpowers/specs/2026-04-18-container-runtime-roadmap.md).
```

- [ ] **Step 9.3: Commit**

```bash
git add README.md
git commit -m "docs: update README for Stage 1 completion"
```

---

## Definition of Done

All of the following must be true:

- `cargo build` succeeds inside the dev container.
- `cargo run -- run ./rootfs /bin/sh` drops into an Alpine shell whose `mount` output contains only the rootfs bind-mount and `/proc`.
- Mounts created inside the container are invisible to a sibling shell on the host dev container.
- `cargo test --test chroot_escape -- --ignored` passes (the escape test continues to demonstrate chroot's weakness).
- Git log shows three substantive commits matching Commits 1.1, 1.2, 1.3 from the spec, plus setup/cleanup commits around them.
