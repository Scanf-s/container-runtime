# Container Runtime

Simple container runtime implementation in Rust.

This repository contains a study-purpose container runtime written in Rust.
It isolates the filesystem, processes, network, user namespace, and hardware resources.

## Requirements

This project only runs on Linux. It uses namespaces, cgroup v2, `pivot_root`, and privileged `mount` calls.

- A Linux host with cgroup v2 mounted in unified mode. Run `stat -fc %T /sys/fs/cgroup`, and the output must be `cgroup2fs`. Debian 11 and later, and Ubuntu 22.04 and later, use cgroup v2 by default.
- Root privileges. The runtime writes to `/sys/fs/cgroup`, and it also calls `mount` and `pivot_root`.
- A Rust toolchain that supports edition 2024, so Rust 1.85 or newer. The project was last checked with Rust 1.98.1.
- `curl` and `tar`, because `scripts/fetch-rootfs.sh` downloads and extracts an Alpine minirootfs.

## How to run

### run directly on the host

This is the normal path on a native Debian or Ubuntu machine.

1. Fetch the rootfs. The script skips the download if `./rootfs` already has contents.

```bash
./scripts/fetch-rootfs.sh ./rootfs
```

2. Build the binary as your normal user.

```bash
cargo build --release
```

3. Run the binary with `sudo`.

```bash
sudo ./target/release/container-runtime run ./rootfs \
  --cpus 1.0 --mem 536870912 --pids 1024 \
  --uid "$(id -u)" --gid "$(id -g)" /bin/sh
```

Two notes about this command:

- Avoid `sudo cargo run` when you installed Rust with `rustup`. The root PATH usually does not include `~/.cargo/bin`, so the command fails. Build first, then run the produced binary with `sudo`.
- `--uid` and `--gid` set the host user that container root maps to. `$(id -u)` and `$(id -g)` map it to yourself, so files created inside the container stay writable for you.

### Development commands

The Makefile only holds formatting and linting targets.

```bash
make fmt    # cargo fmt
make lint   # cargo clippy --all-features
```

## Verify the isolation

Run these commands inside the container shell.

```bash
ip link # isolated network namespace (only lo - loopback device occurs)
```
<img width="490" height="61" alt="스크린샷 2026-07-11 235123" src="https://github.com/user-attachments/assets/7958bf35-77a5-4b23-b5fa-5fe71b771fe7" />

```bash
mount   # isolated rootfs and procfs
ls -al
```
<img width="495" height="379" alt="스크린샷 2026-07-11 235128" src="https://github.com/user-attachments/assets/2382666b-0ec4-4f8e-b806-caa73d5fcc5c" />

```bash
ps -a   # only container-local processes
```
<img width="235" height="76" alt="스크린샷 2026-07-11 235133" src="https://github.com/user-attachments/assets/a182b541-9cc1-48db-947c-e5378a03d177" />

```bash
id      # uid=0(root), gid=0(root)
```
<img width="360" height="39" alt="스크린샷 2026-07-11 235138" src="https://github.com/user-attachments/assets/647989a8-13c2-4b4a-b7f2-010876acfd3e" />

## Concept

See the detailed document in [DOCS.md](./DOCS.md).
