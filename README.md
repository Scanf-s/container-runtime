# Container Runtime

Simple container runtime implementation in Rust.

This repository contains a study-purpose container runtime written in Rust.
It isolates the filesystem, processes, user namespace, and hardware resources.

## How to run

> Recommended: run this project inside the devcontainer.
> The runtime uses Linux `namespaces`, `cgroup v2`, `pivot_root`, and privileged `mount` operations.

### 1. Build Dev Image

```bash
make dev-image
```

### 2. Prepare Rootfs

```bash
make rootfs
```

### 3. Run Container Runtime

```bash
make dev-shell
cargo run -- run ./rootfs --cpus 1.0 --mem 536870912 --pids 1024 --uid 1000 --gid 1000 /bin/sh
```

Inside the container, check:

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
