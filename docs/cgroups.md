# Resource limits with cgroups

[Learning notes](README.md) · [Getting started](../README.md)

## Why cgroups?

Namespaces and `pivot_root` give us *visibility* isolation.
The container sees only its own processes, only its own filesystem. 
But they do not place any *quantitative* limit on what a container can consume. 
A process inside the container is still free to allocate every byte of host memory, fork until the host runs out of PIDs, or pin every CPU at 100%.

> Linux solves this with **control groups (cgroups)**. 

A cgroup is a kernel-managed group of processes for which one or more *controllers* (memory, cpu, pids, io, ...) account for usage and enforce limits. 

Namespaces and cgroups are complementary:
| Mechanism | Question it answers |
|---|---|
| Namespace | "What can the process *see*?" |
| cgroup    | "How much can the process *consume*?" |

A real container needs both. This section adds memory, CPU, and PID limits via cgroup v2.

## cgroup v1 vs v2

Two generations of the API exist. 
- v1 maintains a *separate* hierarchy per controller (`/sys/fs/cgroup/memory/...`, `/sys/fs/cgroup/cpu/...`), so a single process can simultaneously belong to different cgroups in different hierarchies. 
- v2 unifies everything into a single tree under `/sys/fs/cgroup/`, which makes membership and accounting easier to reason about. Modern distributions (Ubuntu 22.04+, Fedora 31+, recent Debian) are using v2 for its standard.

The two can be told apart by looking for the `cgroup.controllers` file at the root. it only exists in v2:
```bash
$ stat -fc %T /sys/fs/cgroup/
cgroup2fs       # v2
tmpfs           # v1 (legacy)
```

We target `v2` exclusively.

## Exploring `/sys/fs/cgroup`

Before writing any code, it's instructive to look at what is already there. 
The whole cgroup interface is exposed as a virtual filesystem. 
*Creating* a cgroup is a `mkdir`, *limiting* it is a `write` to a file, *registering a process* is another write. 
There are no special syscalls.

```bash
root@host:/sys/fs/cgroup# ls
cgroup.controllers       cpu.stat            kubepods.slice
cgroup.subtree_control   memory.stat         system.slice
cgroup.procs             pids.current        user.slice
cgroup.threads           docker              ...
```

A few of these directories are made by other software running on the same host:

- `system.slice/` and `user.slice/` are created by `systemd` to group system services and user sessions.
- `docker/` is created by the Docker daemon, with one subdirectory per container.
- `kubepods.slice/` is created by `kubelet`, with one subdirectory per Pod.

In other words, every container runtime on Linux(Docker, containerd, runc, podman, Kubernetes) works by adding directories under `/sys/fs/cgroup/`. 
The runtime we're building does the same thing.
The trick is that there are no new APIs to learn; the API *is* the filesystem.

Also, Let's inspect the Docker's cgroup (our dev container).

```bash
root@host:/sys/fs/cgroup/docker# cat cpu.max
max 100000
root@host:/sys/fs/cgroup/docker/6b7298085cd6.../memory.current
1626386432              # ~1.5 GiB currently used by this container
```

This `6b7298085cd6...` directory is the cgroup of my devcontainer.
Docker put its own work inside `/sys/fs/cgroup/docker/`.

## Key files in a cgroup directory

Once a cgroup directory exists, the kernel populates it with controller-specific files. The ones this runtime cares about:

| File | Type | Purpose |
|---|---|---|
| `cgroup.controllers` | read-only | Controllers available to this cgroup |
| `cgroup.subtree_control` | rw | Controllers delegated to children |
| `cgroup.procs` | rw | Process IDs that belong to this cgroup |
| `memory.max` | rw | Hard memory limit (bytes); OOM-kill on excess |
| `memory.current` | read-only | Current memory usage |
| `cpu.max` | rw | "&lt;quota&gt; &lt;period&gt;" — μs of CPU per period |
| `pids.max` | rw | Maximum number of tasks (processes/threads) |

A few subtleties worth highlighting:

- **`cgroup.controllers` vs `cgroup.subtree_control`**: The former lists controllers *available* to this cgroup. The latter lists controllers *delegated to its children*. A child only sees a controller if its parent enables it via `subtree_control`. Without that delegation, a fresh child cgroup will not even have files like `memory.max`.

- **`memory.current` vs `docker stats` difference.**: `memory.current` includes page cache, while `docker stats` and most monitoring tools subtract `inactive_file` ("working set"). On a freshly booted devcontainer, the difference is usually tens to hundreds of MiB.

- **`pids.max` counts threads.** Linux models a thread as a task with its own PID, so a JVM or Go workload with many threads can exhaust `pids.max` even with a single "process".

## Implementation

The runtime represents a container's cgroup as a Rust struct that creates the directory on construction and removes it on `Drop`:

```rust
// src/cgroup.rs
pub struct Cgroup {
    path: PathBuf,
}

impl Cgroup {
    pub fn new() -> Result<Self> {
        let cgroup_path: &Path = Path::new("/sys/fs/cgroup");
        let subtree_path: &Path = Path::new("/sys/fs/cgroup/cgroup.subtree_control");

        // 1. Verify cgroup v2 (only v2 has cgroup.controllers at the root).
        if !cgroup_path.join("cgroup.controllers").is_file() {
            bail!("cgroupfs is not v2 (or not mounted)");
        }

        // 2. cgroupfs is privileged. Fail early with a clear error.
        if !nix::unistd::geteuid().is_root() {
            bail!("cgroup operations require root privileges");
        }

        // 3. Make sure the parent has memory/cpu/pids in subtree_control.
        //    Otherwise the new child will lack the corresponding files.
        let controllers = fs::read_to_string(subtree_path)?;
        let active: Vec<&str> = controllers.split_whitespace().collect();
        let required = ["memory", "cpu", "pids"];
        let missing: Vec<&str> = required.iter()
            .filter(|c| !active.contains(c))
            .copied().collect();
        if !missing.is_empty() {
            let payload = missing.iter().map(|c| format!("+{c}"))
                .collect::<Vec<_>>().join(" ");
            fs::write(subtree_path, payload)
                .context("failed to enable controllers")?;
        }

        // 4. Create our own subdirectory with a random name.
        let id = format!("rust_container_{:x}", random::<u64>());
        let new_cgroup = cgroup_path.join(&id);
        fs::create_dir(&new_cgroup).context("create cgroup dir")?;

        // 5. Check delegated controllers.
        let delegated = fs::read_to_string(new_cgroup.join("cgroup.controllers"))?;
        let delegated: Vec<&str> = delegated.split_whitespace().collect();
        for c in &required {
            if !delegated.contains(c) {
                bail!("controller {c} not delegated to new cgroup");
            }
        }

        Ok(Cgroup { path: new_cgroup })
    }

    pub fn add_pid(&self, pid: nix::unistd::Pid) -> Result<()> {
        fs::write(self.path.join("cgroup.procs"), pid.to_string())
            .context("write cgroup.procs")?;
        Ok(())
    }

    pub fn set_memory_max(&self, bytes: u64) -> Result<()> {
        fs::write(self.path.join("memory.max"), bytes.to_string())
            .context("write memory.max")?;
        Ok(())
    }

    pub fn set_cpu_max(&self, quota_us: u64, period_us: u64) -> Result<()> {
        fs::write(self.path.join("cpu.max"), format!("{quota_us} {period_us}"))
            .context("write cpu.max")?;
        Ok(())
    }

    pub fn set_pid_max(&self, pids: u64) -> Result<()> {
        fs::write(self.path.join("pids.max"), pids.to_string())
            .context("write pids.max")?;
        Ok(())
    }
}

impl Drop for Cgroup {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}
```

A few design notes:

- **RAII for cleanup.** Without `Drop`: Every container run would leave a stale `rust_container_<hex>` directory under `/sys/fs/cgroup/`. With `Drop`, the directory is automatically removed when the parent's `Cgroup` value goes out of scope at the end of `run()`. The forked child also holds a copy of the struct, but the child path always exits via `std::process::exit`, which skips destructors. So cleanup happens exactly once, in the parent.

- **`cgroup.controllers` for the v2 check.**: Initial attempts checked `/sys/fs/cgroup` itself with `is_file()`, which always returns `false` because it's a directory. So i changed the logic by cheking the `cgroup.controllers` file is exists.

## The race condition between `fork` and `add_pid`

**Without synchronization**
| Time | Parent | Setup-child | Grandchild |
|---|---|---|---|
| t0 | issued `fork()` | started in host root cgroup | — |
| t1 | (still running) | `unshare -> fork` | born; **inherits host root cgroup** |
| t2 | (still running) | `waitpid` | `execvp` -> workload running, *no limit* |
| t3 | `add_pid(setup)` | (now in our cgroup) | still uncontrolled <<< **PROBLEM** |

The grandchild (the actual user workload) has already been forked and is running before the parent gets a chance to register it. Since the setup-child was still in the host root cgroup at the moment of grandchild's fork, the grandchild inherits the host root cgroup. By the time the parent migrates the setup-child, it is too late. The grandchild was forked from a not-yet-migrated process.

## Pipe-based parent/child synchronization

The fix is to make the setup-child wait until the parent has finished `add_pid` before doing its own `unshare`/`fork`. A pipe is the standard primitive for this:

```rust
// src/runtime.rs
let new_cgroup = Cgroup::new()?;
let (read_fd, write_fd) = pipe()?;       // Created BEFORE fork
new_cgroup.set_cpu_max(cpu_quota_us, CPU_PERIOD_US)?;
new_cgroup.set_memory_max(args.mem)?;
new_cgroup.set_pid_max(args.pids)?;

match unsafe { fork() }? {
    ForkResult::Parent { child } => {
        drop(read_fd);                    // parent never reads
        new_cgroup.add_pid(child)?;       // 1. register child
        write(&write_fd, &[1u8])?;        // 2. signal "you may proceed"
        drop(write_fd);
        let status = waitpid(child, None)?;
        // ...
    }
    ForkResult::Child => {
        drop(write_fd);                   // child never writes
        let mut buf = [0u8; 1];
        read(read_fd.as_raw_fd(), &mut buf)?;   // BLOCK until parent signals
        drop(read_fd);
        // From here on, this process is in the cgroup.
        // The grandchild we are about to fork will inherit cgroup membership.
        setup_child(args)?;
    }
}
```

Both file descriptors must be created *before* the fork so that both processes inherit them. After fork, each side closes the end it does not use (otherwise the EOF that the other side would observe never arrives, because someone is still holding the descriptor open).

> `pipe()` returns `(OwnedFd, OwnedFd)`.   
`OwnedFd` automatically closes its file descriptor when dropped.  
Using `drop(fd)` instead of `close(...)` lets `OwnedFd`'s destructor handle the close exactly once.

With this in place, the corrected timeline is:

| Time | Parent | Setup-child | Grandchild |
|---|---|---|---|
| t0 | `fork()` returns | blocked on `read()` | — |
| t1 | `add_pid(setup)` | blocked | — |
| t2 | `write(pipe, 1)` | unblocks; now in cgroup | — |
| t3 | `waitpid` | `unshare → fork` | born **inside cgroup** (inherited) |
| t4 | (still waiting) | `waitpid(grandchild)` | `execvp` → workload **constrained** |

The grandchild starts life inside the cgroup, so all its allocations are accounted, the OOM killer enforces `memory.max`, fork attempts respect `pids.max`, and the scheduler caps it at `cpu.max`.

## CLI integration

Limits are exposed as flags on `run`:

```rust
// src/cli.rs
#[derive(Parser, Debug)]
pub struct RunArgs {
    pub rootfs: PathBuf,

    /// Number of CPUs (e.g. 0.5 for half a core, 2.0 for two cores).
    #[arg(long, default_value_t = 1.0)]
    pub cpus: f64,

    /// Memory limit in bytes.
    #[arg(long, default_value_t = 512 * 1024 * 1024)]
    pub mem: u64,

    /// Maximum number of tasks (processes and threads).
    #[arg(long, default_value_t = 1024)]
    pub pids: u64,

    pub cmd: String,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}
```

The `--cpus` flag mirrors Docker's. Internally it is converted to the `cpu.max` model the kernel actually wants:

```rust
const CPU_PERIOD_US: u64 = 100_000;     // 100 ms — cgroup v2 default

// Validation
if args.cpus <= 0.0 {
    bail!("--cpus must be positive (got {})", args.cpus);
}
let host_cpus = num_cpus::get() as f64;
if args.cpus > host_cpus {
    bail!("--cpus ({}) exceeds the host's CPU count ({})", args.cpus, host_cpus);
}

// Conversion: --cpus 0.5 with a 100 ms period becomes 50_000 100_000.
let cpu_quota_us = (args.cpus * CPU_PERIOD_US as f64) as u64;
```

Keeping the user-facing input intuitive (`--cpus 0.5`) while the internal API mirrors the kernel's exact model (`set_cpu_max(quota_us, period_us)`) cleanly separates the two concerns.

## Trial and error: `EBUSY` on `cgroup.subtree_control`

The first attempt to run the integrated runtime failed not in our code but in `Cgroup::new`'s call to enable controllers:

```bash
$ cargo run -- run --mem 209715200 ./rootfs /bin/sh
Error: failed to enable controllers
Caused by:
    Device or resource busy (os error 16)
```

The diagnostic state was:

```bash
$ cat /sys/fs/cgroup/cgroup.subtree_control
                                            # empty: no controllers delegated
$ cat /sys/fs/cgroup/cgroup.controllers
cpuset cpu io memory hugetlb pids rdma      # but all are available

$ wc -l /sys/fs/cgroup/cgroup.procs
23 /sys/fs/cgroup/cgroup.procs              # 23 processes in this cgroup

$ mount | grep cgroup
cgroup on /sys/fs/cgroup type cgroup2 (rw,nosuid,nodev,...)
                                            # writable
```

The cause is cgroup v2's **"no internal process" rule**:

> A non-root cgroup may not simultaneously contain processes in its `cgroup.procs`, and delegate controllers to its children via `cgroup.subtree_control`.

The kernel enforces this to remove ambiguity in resource accounting between a parent's own processes and its child cgroups. The devcontainer the development was happening in *was* a non-root cgroup (Docker had assigned it a slice), it had 23 processes (the shell, `cargo`, the VS Code server), and `subtree_control` was empty. Writing `+memory` therefore failed with `EBUSY`.

Production runtimes solve this with the same pattern that systemd uses: move every existing process into a sibling sub-cgroup *before* enabling controllers in the parent. Concretely:

```text
Before:
/sys/fs/cgroup/
├── cgroup.procs                ← 23 PIDs
└── cgroup.subtree_control      ← empty

Migration:
mkdir /sys/fs/cgroup/init
move every PID from cgroup.procs → init/cgroup.procs
echo "+memory +cpu +pids" > /sys/fs/cgroup/cgroup.subtree_control

After:
/sys/fs/cgroup/
├── cgroup.procs                ← empty
├── cgroup.subtree_control      ← memory cpu pids
└── init/
    └── cgroup.procs            ← 23 PIDs
```

Because this development environment is itself a Docker container whose PID 1 and its children all live in a single cgroup, we do not implement the migration inside the runtime (it would have to move processes that don't belong to us, like the editor's language server). Instead, we automate it once at devcontainer startup.

`.devcontainer/cgroup_init.sh`:

```bash
#!/bin/bash
set -u
mkdir -p /sys/fs/cgroup/init

# Move every process out of the root cgroup.
# Repeat a few times to catch processes that spawn during migration.
for _ in 1 2 3 4 5; do
    for pid in $(cat /sys/fs/cgroup/cgroup.procs 2>/dev/null); do
        echo "$pid" > /sys/fs/cgroup/init/cgroup.procs 2>/dev/null || true
    done
done

remaining=$(wc -l < /sys/fs/cgroup/cgroup.procs)
if [ "$remaining" -gt 0 ]; then
    echo "warning: $remaining processes still in root cgroup:"
    cat /sys/fs/cgroup/cgroup.procs
fi

echo "+memory +cpu +pids" > /sys/fs/cgroup/cgroup.subtree_control \
    && echo "cgroup setup complete: $(cat /sys/fs/cgroup/cgroup.subtree_control)" \
    || echo "ERROR: failed to enable controllers"
```

`.devcontainer/devcontainer.json` wires it up plus passes the necessary Docker flags:

```json
{
  "runArgs": [
    "--privileged",
    "--cgroupns=private"
  ],
  "postStartCommand": ".devcontainer/cgroup_init.sh"
}
```

`--privileged` makes `/sys/fs/cgroup` writable, and `--cgroupns=private` gives the container its own cgroup namespace so it sees its assigned slice as `/`. Neither flag, by itself, enables controller delegation — only the migration script does that. Both flags require a full container *rebuild* (not just restart) to take effect, since they only apply at `docker create` time.

### A subtle pitfall: running the migration as a script

A first attempt to run the migration interactively went like this:

```bash
$ bash .devcontainer/cgroup_init.sh
.devcontainer/cgroup_init.sh: line 19: echo: write error: Device or resource busy
```

The script *looked* identical to the working one. But running it from an interactive shell still hit `EBUSY` on the final `subtree_control` write. Why?

The answer is in how the shell starts a script. When you type `bash some-script.sh` at a prompt, your interactive shell does **not** run the script's commands itself. It `fork()`s a *new* bash process, which then reads and executes the script's lines:

```
[Interactive shell — PID 1559]            ← waits at the prompt for the script to finish
        │
        └── fork() + exec(bash)
                │
                └── [Script bash — PID 4498]   ← actually runs cgroup_init.sh
```

Both processes are in the same cgroup at the start: PID 1559 was already there, and PID 4498 inherited cgroup membership from its parent at the moment of fork. So `cgroup.procs` now has 24 PIDs, not 23.

The interactive shell at PID 1559 just sits at the prompt waiting for the script to return. It is alive, it is in `cgroup.procs`, but it isn't doing anything — no command of its own to fork from, no opportunity to migrate itself. Meanwhile other long-lived processes in this devcontainer (vscode-server, language servers, file watchers) keep spawning short-lived helpers, and many of those helpers land in `cgroup.procs` as they appear. The script's migration loop tries to drain the list five times to absorb this churn, but PID 1559 sits there the whole time, and the kernel checks `cgroup.procs` *at the moment* `subtree_control` is written:

```
Script's view at this moment:
  cgroup.procs still contains: [1559, ...]   ← parent shell + maybe a few stragglers
  Script tries:  echo "+memory ..." > /sys/fs/cgroup/cgroup.subtree_control
  Kernel checks: is cgroup.procs empty?  No → EBUSY
```

The fix is to **stop using a script wrapper** and type the commands directly into the interactive shell:

```bash
# Type these directly at the prompt, NOT via "bash some-script.sh"
$ mkdir -p /sys/fs/cgroup/init
$ for pid in $(cat /sys/fs/cgroup/cgroup.procs); do
    echo "$pid" > /sys/fs/cgroup/init/cgroup.procs 2>/dev/null
  done
$ echo "+memory +cpu +pids" > /sys/fs/cgroup/cgroup.subtree_control
```

Now there is no second bash. The interactive shell *itself* expands `$(cat ...)`, and the list it gets back **includes its own PID (1559)**. The loop sends `echo 1559 > init/cgroup.procs`, which moves the shell. From that moment on, every subsequent command typed at this shell — including the next `echo` to `subtree_control` — runs inside `init/`, so the writer of `subtree_control` is no longer "internal" to the root cgroup. The "no internal process" rule is satisfied, and the write succeeds.

Why, then, does the very same script work fine as a `postStartCommand`? Because at devcontainer boot time, **there is no interactive shell yet**. The Docker container has just started; only its entrypoint and a handful of system processes exist, and `postStartCommand` is invoked by the devcontainer infrastructure rather than from a user shell. The bash that runs the script is itself one of the few processes in the cgroup, and the migration loop sees and moves all of them — including itself. There is no separate "parent shell sitting at a prompt" left behind to keep `cgroup.procs` non-empty.

In short:

| Scenario | What happens | Result |
|---|---|---|
| Script run from an interactive shell | Parent shell stays at the prompt, never migrated | `EBUSY` |
| Inline commands in an interactive shell | That very shell is in the migration list and moves itself | succeeds |
| Script run before any interactive shell exists (postStartCommand) | No parent shell exists in the first place | succeeds |

## Putting it all together

With the devcontainer prepared, the integrated startup sequence is:

```
run()
├── validate args (rootfs, cpus, mem, pids)
├── Cgroup::new()                     # creates /sys/fs/cgroup/rust_container_<hex>
├── pipe()                            # synchronization primitive
├── set_memory_max / set_cpu_max / set_pid_max
└── fork()
    ├── parent
    │   ├── add_pid(child)
    │   ├── write(pipe, 1)            # "child may proceed"
    │   ├── waitpid(child)
    │   └── Drop ⇒ rmdir cgroup
    └── setup_child (this is `child`)
        ├── read(pipe)                # blocks until parent signals
        ├── unshare(CLONE_NEWPID)
        └── fork()                    # grandchild inherits cgroup
            ├── parent (PID-namespace's reaper)
            └── grandchild (PID 1 in new namespace)
                ├── isolate_fs_pivot  # mount-ns + pivot_root + /proc
                └── execvp            # workload runs, fully constrained
```

## Verification

The point of building this is not "the code compiles" but "the kernel does what we asked". Each limit is verified by trying to exceed it.

### Memory limit

Run with a 200 MiB cap and have an `awk` process double a string until it OOMs

```bash
$ cargo run -- run --mem 209715200 ./rootfs /bin/sh
/ # awk 'BEGIN { s = "x"; while (1) s = s s }'
Killed
```

`Killed` is BusyBox's report that `SIGKILL` was delivered. 
The matching dmesg entry from the host confirms it was the cgroup OOM killer, and points at our cgroup by name

```text
oom-kill:constraint=CONSTRAINT_MEMCG, ...,
  oom_memcg=/docker/<devcontainer-id>/rust_container_1a0784d8c9d453cb,
  task_memcg=/docker/<devcontainer-id>/rust_container_1a0784d8c9d453cb,
  task=awk, pid=81708, uid=0
Memory cgroup out of memory: Killed process 81708 (awk)
  total-vm:1574596kB, anon-rss:191784kB, file-rss:120kB, shmem-rss:0kB,
  UID:0 pgtables:2480kB oom_score_adj:0
```

Three observations:

- **`CONSTRAINT_MEMCG`**: the OOM was triggered by the cgroup, not by host-wide memory pressure.
- **The path** `/docker/<devcontainer-id>/rust_container_1a0784d8c9d453cb` shows our cgroup as a grandchild of Docker's cgroup for this devcontainer. Our runtime really is just adding a directory under someone else's tree.
- **`anon-rss: 191784 kB ≈ 187 MiB`** is just under the 200 MiB limit. `awk` requested ~1.5 GiB of virtual memory but Linux only allocates physical pages on demand; the cgroup counts physical usage, so it triggered just before the next doubling would have crossed the limit.

### PID limit

Run with `--pids 30`. The shell itself takes 1 PID; `seq 1..100` started in a `for` loop with `sleep 100 &` will start failing at the 30th fork:

```bash
$ cargo run -- run --pids 30 ./rootfs /bin/sh
/ # for i in $(seq 1 100); do echo "process $i"; sleep 100 & done
process 1
...
process 29
/bin/sh: can't fork: Resource temporarily unavailable
```

`EAGAIN` ("Resource temporarily unavailable") is the standard `errno` for a `fork` rejected by the kernel. From the host:

```bash
$ cat /sys/fs/cgroup/rust_container_*/pids.max
30
$ cat /sys/fs/cgroup/rust_container_*/pids.current
30
$ cat /sys/fs/cgroup/rust_container_*/pids.events
max 1                                       # limit was reached once
```

The same mechanism makes the classic shell fork-bomb `:(){ :|:&};:` survivable: inside the container it caps at 30 processes; on a host without a similar cgroup, it would bring down the system.

### CPU limit

Run with `--cpus 0.5` (half a core), then start two infinite spin loops:

```bash
$ cargo run -- run --cpus 0.5 ./rootfs /bin/sh
/ # while true; do :; done &
/ # while true; do :; done &
```

From the host's `top`:

```text
  PID  USER      %CPU  COMMAND
 4582  root      25.2  sh        # spin loop #1
 4572  root      24.9  sh        # spin loop #2
                                 ────────
                                   50.1%  =  --cpus 0.5
```

Each loop wants 100% CPU. 
Together they would consume 200% on a multi-core host. 
The cgroup's `cpu.max = 50000 100000` (50 ms quota per 100 ms period) caps the *combined* usage of every process in the cgroup at 50% of one core, and the scheduler shares that fairly between the two.
Hence ~25% each. If only one loop were running, it would reach ~50% on its own. 
The limit is on the group, not on individual tasks.

### Drop cleanup

After the container exits, the cgroup directory is gone:

```bash
# while container is running
$ ls /sys/fs/cgroup/ | grep rust_container
rust_container_1a0784d8c9d453cb

# after the container exits
$ ls /sys/fs/cgroup/ | grep rust_container
                            # nothing — RAII Drop removed it
```

If `rmdir` ever ran while the cgroup still contained processes, it would fail with `EBUSY`. 
The fact that cleanup succeeds confirms two things
- the workload really did run inside our cgroup (so all PIDs left when the container died).
- the parent process (the only one whose `Drop` fires) is the only owner that performs the cleanup.
