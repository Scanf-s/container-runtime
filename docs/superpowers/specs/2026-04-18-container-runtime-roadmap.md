# Container Runtime — Learning Roadmap

**Date:** 2026-04-18
**Status:** Approved

## Project Goal

Build a minimal `container-runtime run <rootfs> <cmd>` binary in Rust that launches an isolated Linux process. The project is for **self-study of virtualization concepts**, not production use. The learning style is concept-first and iterative: each stage pairs one kernel concept with a small implementation that exercises it.

After completing all five stages, the project *may* be extended toward OCI Runtime Spec compliance; that decision is deferred until Stage 5 is done.

## Target Stack

- **Language:** Rust (edition 2024), using the `nix` crate for safe syscall wrappers throughout.
- **Dev environment:** privileged Docker container running Debian slim + Rust toolchain, with the repo bind-mounted from the macOS host. All building and testing happens inside the container.
- **Target rootfs:** Alpine minirootfs tarball (`alpine-minirootfs-*.tar.gz`), fetched via `scripts/fetch-rootfs.sh`.

## Stages

Each stage gets its own design spec + implementation plan + implementation cycle. Completion of one stage is the gate to brainstorming the next.

| Stage | Concept | Deliverable | Success signal |
|---|---|---|---|
| **1** | Filesystem isolation | `chroot(2)` → escape demo → `unshare(CLONE_NEWNS)` + `pivot_root(2)` + `/proc` mount | Container sees only the alpine rootfs; mounts inside don't leak to host |
| **2** | Process isolation | PID namespace (`CLONE_NEWPID`), proper `/proc` remount, re-exec "init" pattern | `ps` inside shows PID 1 for the launched command |
| **3** | Other namespaces | UTS (hostname), IPC, Network (loopback only) | `hostname` inside is independent; `ip link` shows only `lo` |
| **4** | Resource limits | cgroups v2 — CPU weight, memory max, pids max | Container OOMs at memory limit; CPU-bound task throttles |
| **5** | Privilege drop | User namespace, drop capabilities | Container runs as UID 0 inside but UID 1000 on host; dangerous caps dropped |

## What's explicitly out of scope (for now)

- OCI Runtime Spec compliance (`config.json`, lifecycle commands `create`/`start`/`kill`/`delete`)
- OCI Image Spec handling (image pulling, layer extraction, registry protocol)
- Docker-compatible CLI surface
- Multi-container state management / container lists
- Networking beyond loopback (bridges, veth pairs, CNI)

These can be revisited after Stage 5.

## Current state

- `src/main.rs` contains an unrelated CPU-usage printer from earlier experimentation. It will be replaced during Stage 1.
- `Cargo.toml` depends on `sysinfo`; this will be removed and replaced with `nix`, `clap`, `anyhow` in Stage 1.
- `README.md` mentions chroot-based filesystem isolation; it will be updated as stages land.
