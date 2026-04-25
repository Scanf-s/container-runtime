# Stage 2 — Process Isolation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add PID namespace isolation so that the container's first process appears as PID 1 inside the container, while `/proc` reflects only processes that live in the new namespace. Achieved via `unshare(CLONE_NEWPID)` followed by a second `fork()` — the grandchild is PID 1 in the new namespace.

**Architecture:** `src/runtime.rs` grows a two-layer fork. The existing child path becomes a *setup* process that calls `unshare(CLONE_NEWPID)` and forks once more; the grandchild is PID 1 inside the new PID namespace and runs the Stage 1 filesystem isolation + `execvp`. `src/container.rs` is unchanged — because the container's init process already calls `isolate_fs_pivot` *inside* the new PID namespace, the existing `/proc` mount automatically shows the namespace-local view.

**Design note — double-fork vs. re-exec.** The roadmap mentions a "re-exec init pattern"; that phrasing is inherited from Go-based runtimes (runc, etc.) where the Go scheduler spawns threads at startup and `unshare(CLONE_NEWPID)` therefore fails in the main process. Those runtimes work around it by `execve`-ing a minimal init binary before any threads exist. Rust's standard binary does not start background threads implicitly, so a plain `unshare + fork` sequence is sufficient and noticeably simpler. We use the double-fork form and document this deviation so the roadmap entry is not misread as a requirement.

**Tech Stack:** Rust edition 2024; `nix 0.29` (already present, no new features needed); the existing privileged Docker dev container invoked via `make dev-shell`. Integration tests use `std::process::Command` + `env!("CARGO_BIN_EXE_container-runtime")`.

**Parent roadmap:** [`docs/superpowers/specs/2026-04-18-container-runtime-roadmap.md`](../specs/2026-04-18-container-runtime-roadmap.md) (Stage 2 row).

---

## File Structure

| Path | Create / Modify | Purpose |
|---|---|---|
| `src/runtime.rs` | Modify | Add `setup_main` + `init_main` layers; `unshare(CLONE_NEWPID)` between them |
| `tests/pid_namespace.rs` | Create | Integration tests: PID 1 + `/proc` namespace filter |
| `README.md` | Modify | Add `### 2. Process Isolation` section |

`src/container.rs`, `src/main.rs`, `src/cli.rs`, `Cargo.toml` are all unchanged in this stage.

---

## Task 1: Write failing integration tests (red)

**Files:**
- Create: `tests/pid_namespace.rs`

These tests are written first so we can confirm, before touching `runtime.rs`, that they genuinely fail on the current Stage 1 code (the container's shell has a host PID, and `/proc` lists host processes). TDD-wise this is the "red" step.

- [ ] **Step 1.1: Create `tests/pid_namespace.rs`**

Create the file with exactly this content:

```rust
//! Stage 2 integration tests — PID namespace isolation.
//!
//! Both tests spawn the compiled `container-runtime` binary, run a shell
//! command inside the container, and inspect its output. They require
//! CAP_SYS_ADMIN (to unshare the PID namespace) and an extracted alpine
//! rootfs at `./rootfs`. Run them from the project root:
//!
//!     cargo test --test pid_namespace -- --ignored
//!
//! Marked `#[ignore]` so they do not run by default — the host environment
//! will almost always lack the required privileges.

use std::process::Command;

#[test]
#[ignore = "requires CAP_SYS_ADMIN and ./rootfs"]
fn container_first_process_is_pid_1() {
    let out = Command::new(env!("CARGO_BIN_EXE_container-runtime"))
        .args(["run", "./rootfs", "/bin/sh", "-c", "echo $$"])
        .output()
        .expect("failed to spawn container-runtime");

    assert!(
        out.status.success(),
        "runtime exited with status {:?}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let pid = String::from_utf8(out.stdout).expect("stdout not utf-8");
    assert_eq!(
        pid.trim(),
        "1",
        "expected the container's first shell to be PID 1, got {:?}",
        pid
    );
}

#[test]
#[ignore = "requires CAP_SYS_ADMIN and ./rootfs"]
fn container_proc_reflects_namespace_view() {
    // Inside a container with PID namespace isolation, /proc must list
    // only processes from the new namespace. On the host, /proc typically
    // contains dozens to hundreds of numeric entries.
    let out = Command::new(env!("CARGO_BIN_EXE_container-runtime"))
        .args([
            "run",
            "./rootfs",
            "/bin/sh",
            "-c",
            "ls /proc | grep -E '^[0-9]+$' | wc -l",
        ])
        .output()
        .expect("failed to spawn container-runtime");

    assert!(out.status.success(), "runtime exited: {:?}", out.status);

    let n: usize = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("expected a number from wc -l");

    assert!(
        n <= 10,
        "expected a small number of processes in container /proc (namespace view), got {}",
        n
    );
}
```

- [ ] **Step 1.2: Run the tests and confirm they FAIL on Stage 1 code**

From inside the privileged dev container, at the project root:

```bash
cargo test --test pid_namespace -- --ignored
```

Expected: **both tests FAIL.** Typical failure messages:

- `container_first_process_is_pid_1` — assertion message like `expected the container's first shell to be PID 1, got "1234\n"` (some arbitrary host PID).
- `container_proc_reflects_namespace_view` — assertion message like `expected a small number of processes ..., got 250` (or however many host processes exist).

If either test **passes** here, something is wrong with the test (not the runtime). Inspect before proceeding — passing tests against Stage 1 code means they are not actually exercising PID-namespace behaviour.

- [ ] **Step 1.3: Commit the failing tests**

```bash
git add tests/pid_namespace.rs
git commit -m "test: add failing PID namespace integration tests (Stage 2)

Both tests spawn the runtime and assert on its container's PID view.
They currently fail because Stage 1 does not unshare CLONE_NEWPID;
Stage 2's runtime change will make them pass."
```

Committing a known-failing test is deliberate: it documents the precise success criteria for Stage 2 in the git history.

---

## Task 2: Add PID namespace via double-fork in `runtime.rs`

**Files:**
- Modify: `src/runtime.rs` (complete rewrite — see below)

- [ ] **Step 2.1: Replace the entire contents of `src/runtime.rs`**

```rust
use anyhow::{bail, Context, Result};
use nix::sched::{unshare, CloneFlags};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{fork, ForkResult};
use std::process::ExitCode;

use crate::cli::RunArgs;
use crate::container;

pub fn run(args: RunArgs) -> Result<ExitCode> {
    // Make sure the rootfs exists before we fork.
    if !args.rootfs.is_dir() {
        bail!("rootfs {:?} does not exist or is not a directory", args.rootfs);
    }

    // First fork: the child becomes the "setup" process that will establish
    // the new PID namespace and launch the container's PID 1.
    //
    // SAFETY: between fork and exec the child does not call back into any
    // Rust code that relies on resources duplicated across the fork boundary
    // (no destructors need to run, no shared threads, etc.).
    match unsafe { fork() }.context("fork (setup) failed")? {
        ForkResult::Parent { child } => {
            let status = waitpid(child, None).context("waitpid(setup) failed")?;
            match status {
                WaitStatus::Exited(_, code) => Ok(ExitCode::from(code as u8)),
                WaitStatus::Signaled(_, sig, _) => Ok(ExitCode::from(128u8 + sig as u8)),
                other => bail!("unexpected wait status for setup: {:?}", other),
            }
        }
        ForkResult::Child => {
            if let Err(e) = setup_main(args) {
                eprintln!("container-runtime: setup failed: {e:#}");
                std::process::exit(127);
            }
            unreachable!("setup_main either waits on init or exits");
        }
    }
}

/// Setup process. Not PID 1 yet — its job is to create a new PID namespace
/// and fork once more so the grandchild is PID 1 inside that namespace.
///
/// `unshare(CLONE_NEWPID)` does NOT move the caller into the new namespace;
/// it only makes the caller's future children members of it. So we must
/// fork again after the unshare.
fn setup_main(args: RunArgs) -> Result<()> {
    unshare(CloneFlags::CLONE_NEWPID).context("unshare(CLONE_NEWPID)")?;

    match unsafe { fork() }.context("fork (init) failed")? {
        ForkResult::Parent { child: init } => {
            // Wait for the container's PID 1 and forward its exit status.
            let status = waitpid(init, None).context("waitpid(init) failed")?;
            let code = match status {
                WaitStatus::Exited(_, c) => c as i32,
                WaitStatus::Signaled(_, sig, _) => 128 + sig as i32,
                other => bail!("unexpected wait status for init: {:?}", other),
            };
            std::process::exit(code);
        }
        ForkResult::Child => {
            init_main(args)?;
            unreachable!();
        }
    }
}

/// The container's PID 1 inside the new PID namespace. Completes the Stage 1
/// filesystem isolation (which also mounts a fresh `/proc` — now bound to the
/// new PID namespace because this process is inside it) and execs the user
/// command.
fn init_main(args: RunArgs) -> Result<()> {
    container::isolate_fs_pivot(&args.rootfs)?;
    container::exec_cmd(&args.cmd, &args.args)?;
    unreachable!();
}
```

- [ ] **Step 2.2: Build inside the dev container**

```bash
cargo build
```

Expected: `Finished \`dev\` profile [unoptimized + debuginfo] target(s)` with no errors or new warnings. The `isolate_fs_chroot` `dead_code` path is silenced by the existing `#[allow(dead_code)]` attribute on that function.

- [ ] **Step 2.3: Manual smoke test**

Inside the privileged dev container:

```bash
cargo run -- run ./rootfs /bin/sh
```

Inside the resulting alpine shell, check:

```sh
/ # echo $$
1
/ # ps -o pid,comm
  PID COMMAND
    1 /bin/sh
    ...  (a handful of entries, not hundreds)
```

If `echo $$` prints `1` and `ps` shows `/bin/sh` as PID 1, the PID namespace is working. Exit the shell (`exit`) and verify the runtime returns cleanly with exit status 0.

- [ ] **Step 2.4: Run the Stage 2 integration tests — expect GREEN**

```bash
cargo test --test pid_namespace -- --ignored
```

Expected: both tests pass.

```
running 2 tests
test container_first_process_is_pid_1 ... ok
test container_proc_reflects_namespace_view ... ok
```

If they still fail, inspect the error output. Most likely causes:
- Forgot the `unshare(CLONE_NEWPID)` call → `echo $$` still prints a host PID.
- `unshare` is invoked in `init_main` instead of `setup_main` → the ns is created for *init's* children, not for init itself. The first fork after unshare must be the one that creates PID 1.
- `./rootfs` missing → rerun `make rootfs`.

- [ ] **Step 2.5: Verify the Stage 1 filesystem tests still pass**

The pivot_root + mount-namespace work is unchanged; run the manual checks from Stage 1 Task 8.4 once to confirm we have not regressed:

```bash
cargo run -- run ./rootfs /bin/sh -c 'mount | wc -l; mount'
```

Expected: `2` followed by the two lines (rootfs bind-mount + `/proc`). No host mounts leaking in.

- [ ] **Step 2.6: Commit (Commit 2.1)**

```bash
git add src/runtime.rs
git commit -m "feat: PID namespace + init re-exec pattern (Stage 2, Commit 2.1)

Add a setup process between the host-side runtime and the container's
PID 1. The setup process unshares CLONE_NEWPID and forks; the grandchild
is PID 1 inside the new namespace and runs the Stage 1 filesystem
isolation + execvp. /proc, mounted by the grandchild, inherits the new
PID namespace so 'ps' inside the container shows only namespace-local
processes."
```

---

## Task 3: Document Stage 2 in `README.md`

**Files:**
- Modify: `README.md`

- [ ] **Step 3.1: Add a new `### 2. Process Isolation` section**

Open `README.md` and add the following section **immediately after** the existing `#### pivot_root filesystem isolation` section (i.e., appended to the end of `### 1. Filesystem Isolation`'s contents):

````markdown
### 2. Process Isolation

Stage 1 isolated the container's filesystem, but from a process-visibility standpoint the container is still part of the host: `ps` inside the container would still see every process on the host, and the container's first process has whatever host PID the kernel happened to assign it. Stage 2 fixes that with the PID namespace.

#### Why `unshare(CLONE_NEWPID)` alone is not enough

`CLONE_NEWPID` tells the kernel to create a new PID namespace, but it does not move the calling process into that namespace. Instead, the caller's *future children* are the first members of the new namespace. The first such child becomes PID 1 inside it — the container's "init" process.

This is a subtle but important detail: we cannot simply call `unshare(CLONE_NEWPID)` and then continue as PID 1. We must fork once more after the `unshare`.

The resulting structure is a two-layer fork:

```
host parent (runs main / waitpid)
  │ fork()
  └── setup process  (still in host PID ns)
        │ unshare(CLONE_NEWPID)     -- future children go to the new PID ns
        │ fork()
        └── init process  (PID 1 inside the new PID ns)
              │ isolate_fs_pivot(rootfs)  -- mount ns, pivot_root, /proc mount
              │ execvp(cmd, args)
              └── user command
```

The setup process exists only to create the PID namespace and wait on the init process; it forwards the init's exit status back up so the original caller sees a normal exit code.

#### Why `/proc` just works

The fresh procfs mount from Stage 1 is enough for Stage 2 to behave correctly — no new mount code is needed. The Linux kernel ties each procfs mount instance to the PID namespace of the process that mounted it. Because the init process mounts `/proc` *after* being forked into the new PID namespace, the procfs instance in the container shows only that namespace's processes — not the host's.

If we mounted `/proc` from the setup process instead (which is still in the host PID namespace), `ps` inside the container would continue to leak host PIDs.

#### Known limitations (documented, not addressed here)

These are real concerns for a production runtime but are orthogonal to the PID-namespace mechanism itself, so they are deliberately deferred:

- **Zombie reaping.** PID 1 in a namespace inherits the responsibility of reaping orphaned children. The current init process just `execvp`s into the user's command, so zombies created inside the container rely on the user's shell to reap them. An interactive `/bin/sh` is usually fine; longer-running workloads would want a dedicated init (e.g. `tini`).
- **Signal forwarding.** Signals sent directly to the host-side `container-runtime` process are not forwarded to the container's init. A production runtime installs signal handlers in the setup process and forwards them to the init process.
- **Exit-status fidelity.** If the container's init is killed by a signal, the setup process converts that into an `exit(128 + signum)` code. The original caller cannot distinguish that from a normal exit with code `128 + signum`.

**Result**

```bash
/ # echo $$
1
/ # ps -o pid,comm
  PID COMMAND
    1 /bin/sh
    2 ps
/ # ls /proc | grep -E '^[0-9]+$' | wc -l
2
```

Compare with Stage 1 alone (no PID namespace), where `echo $$` would print an arbitrary host PID such as `1234`, and `ls /proc` would list every process on the host.
````

- [ ] **Step 3.2: Commit (Commit 2.2)**

```bash
git add README.md
git commit -m "docs: add Stage 2 Process Isolation section to README

Explains why CLONE_NEWPID alone cannot make the caller PID 1, the
double-fork setup-process + init-process pattern, and why the Stage 1
/proc mount continues to do the right thing once the mounter lives in
the new PID namespace. Also records known limitations (zombie reaping,
signal forwarding, exit-status fidelity) as future work."
```

---

## Task 4: Final sanity check

**Files:** none changed.

- [ ] **Step 4.1: Rerun everything from a clean build**

```bash
cargo clean
cargo build
cargo test --test pid_namespace -- --ignored
cargo run -- run ./rootfs /bin/sh -c 'echo PID=$$ ; mount | wc -l'
```

Expected:
- `cargo build`: clean, no warnings.
- `cargo test`: two tests pass.
- `cargo run`: prints `PID=1` and `2` (rootfs bind + `/proc`), exits 0.

If all three succeed, Stage 2 is done.

---

## Definition of Done

All of the following must be true:

- `cargo build` succeeds cleanly inside the dev container, with no new warnings.
- `cargo test --test pid_namespace -- --ignored` passes both `container_first_process_is_pid_1` and `container_proc_reflects_namespace_view`.
- `cargo run -- run ./rootfs /bin/sh` drops into a shell where `echo $$` prints `1` and `ls /proc` shows only a handful of numeric directories.
- Stage 1 behaviour is unchanged: inside that shell, `mount` still lists only the rootfs bind-mount and `/proc`, and mounts performed inside the container do not leak to the host.
- Git log shows the Stage 2 commits (tests first as red, runtime change as green, README last), matching the TDD narrative.
