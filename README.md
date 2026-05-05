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
cargo run -- run ./rootfs --cpus 1.0 --mem 536870912 --pids 1024 --uid 1000 --gid 1000 /bin/sh
```

Inside the container, check:

```bash
id      # uid=0(root), gid=0(root)
ps -a   # only container-local processes
mount   # isolated rootfs and procfs
```

## Concept

See the detailed document in DOCS.md (DOCS.md).
