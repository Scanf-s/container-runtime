# A guide to implementing the OCI Runtime Specification

This guide explains how to extend the current learning project to support the OCI Runtime Specification.
It connects OCI concepts to the existing code and breaks the work into small steps. Each step includes a way to check the result.

**This document describes planned work as well as existing features.** See the [project README](../README.md) for commands that work today. The `run-bundle`, `create`, `start`, `state`, `kill`, and `delete` commands below are proposed interfaces. They still need to be implemented.

## 1. What does OCI define?

Container tools need shared rules so that different runtimes can use the same configuration. The Open Container Initiative (OCI) defines these rules in three specifications.

| Specification | What it defines | Place in this project |
| --- | --- | --- |
| Runtime Specification | How to run and manage a container from a prepared filesystem and configuration | First goal |
| Image Specification | How to store filesystem layers and image metadata | A separate future task |
| Distribution Specification | How to transfer images to and from a registry | A separate future task |

Downloading and unpacking an image is one task. Running a process inside that unpacked filesystem is another. This guide focuses on running the process. You can develop the runtime without adding image downloads first.

Use [OCI Runtime Specification v1.3.0](https://github.com/opencontainers/runtime-spec/tree/v1.3.0) as the reference version. A version tag gives us a fixed set of requirements. The `main` branch can change during development. In the specification, MUST means required, SHOULD means recommended, and MAY means allowed but optional.

**The first goal is to support bundles and the container lifecycle for a limited set of Linux features.** Reading `ociVersion` or parsing JSON does not mean that the runtime meets every OCI requirement.

## 2. What does the current code do?

The runtime currently follows this sequence:

```text
Parse the CLI run arguments
  → Create a cgroup and set resource limits
  → Create a setup process
  → Create a user namespace and let the parent map UIDs and GIDs
  → Create PID and network namespaces
  → Fork again to create the container init process
  → Prepare the mount namespace and rootfs, then call pivot_root
  → Set the container UID and GID to 0
  → Use exec to run the user program
  → Wait for the processes to exit and return their exit status
```

A namespace gives a process its own view of part of the system, such as process IDs or network interfaces. A cgroup groups processes so that Linux can control their resource use. The rootfs is the filesystem that the container sees as `/`.

`fork` creates a new process. `exec` replaces the current process with another program. In this guide, init means the process with PID 1 inside the container's PID namespace. The current runtime runs the user program directly as this process.

| Existing code | What we can reuse | What needs to change |
| --- | --- | --- |
| [`src/cli/run_args.rs`](../src/cli/run_args.rs) | CLI input and validation | Separate CLI arguments from execution settings |
| [`src/runtime/mod.rs`](../src/runtime/mod.rs) | Process creation and setup order | Separate creation from starting, and report the real init PID |
| [`src/ipc/`](../src/ipc/) | Communication between parent and child | Report readiness and errors, and wait for a start request |
| [`src/filesystem/pivot_root.rs`](../src/filesystem/pivot_root.rs) | Filesystem isolation and the `/proc` mount | Read mounts and rootfs options from configuration |
| [`src/user/mapping.rs`](../src/user/mapping.rs) | Map container ID 0 to one host ID | Support ID ranges and separate mapping from the execution user |
| [`src/cgroups/cgroup.rs`](../src/cgroups/cgroup.rs) | CPU, memory, and process limits in cgroup v2 | Support optional limits and manage paths and cleanup |
| [`src/container.rs`](../src/container.rs) | Execute the user program | Apply the requested environment and working directory |

`Runtime` currently owns `RunArgs` directly. The namespace types and `/proc` mount are also fixed in the code. To accept OCI input, the runtime needs configuration objects that describe these choices.

## 3. Understand the bundle

A bundle is the base directory where the runtime finds the configuration and locates the rootfs. Start with this layout:

```text
examples/oci-bundle/
├── config.json
└── rootfs/
    ├── bin/
    ├── etc/
    ├── proc/
    └── ...
```

The rootfs must contain the program and any libraries it needs. An empty directory is not enough. You can use the existing `scripts/fetch-rootfs.sh` script to prepare it.

When `root.path` is relative, resolve it **from the bundle directory**. The directory where you ran the CLI does not change this rule. An absolute path is also allowed, so the rootfs does not have to be inside the bundle. See the [bundle format](https://github.com/opencontainers/runtime-spec/blob/v1.3.0/bundle.md) and [configuration reference](https://github.com/opencontainers/runtime-spec/blob/v1.3.0/config.md).

The following example is a target for our first implementation. It assumes a learning environment where the runtime runs as root and maps container root to host root. It is not a complete security configuration for production use.

```json
{
  "ociVersion": "1.3.0",
  "root": { "path": "rootfs", "readonly": false },
  "process": {
    "terminal": false,
    "user": { "uid": 0, "gid": 0 },
    "args": ["/bin/sh", "-c", "echo OCI_STARTED; exec /bin/sleep 300"],
    "env": ["PATH=/bin:/usr/bin", "HOME=/"],
    "cwd": "/"
  },
  "mounts": [
    {
      "destination": "/proc",
      "type": "proc",
      "source": "proc",
      "options": ["nosuid", "nodev", "noexec"]
    }
  ],
  "linux": {
    "namespaces": [
      { "type": "user" },
      { "type": "pid" },
      { "type": "network" },
      { "type": "mount" }
    ],
    "uidMappings": [{ "containerID": 0, "hostID": 0, "size": 1 }],
    "gidMappings": [{ "containerID": 0, "hostID": 0, "size": 1 }],
    "resources": {
      "cpu": { "quota": 100000, "period": 100000 },
      "memory": { "limit": 536870912 },
      "pids": { "limit": 64 }
    }
  }
}
```

`process.args` is an array containing the program name and its arguments. The runtime must pass these entries as separate arguments. In this example, we explicitly run `/bin/sh`, so the shell reads the string after `-c`. Do not join ordinary argument arrays with spaces and pass them to a shell.

## 4. Step 1: Separate CLI input from execution settings

Start with a small change to the existing structure before adding OCI parsing.

```text
Existing CLI RunArgs ──────────┐
                              ├→ ContainerConfig → Runtime
OCI config.json → validation ─┘
```

`ContainerConfig` is the runtime's internal configuration. It should describe:

- The absolute rootfs path on the host.
- The program arguments, environment, and working directory inside the container.
- The container UID and GID, with separate mappings to host IDs.
- The namespaces and mounts to prepare.
- The resource limits to apply.

The following file layout is a suggestion. You do not need to move all existing code at once.

```text
src/config.rs             # Internal ContainerConfig and validation
src/oci/mod.rs            # Bundle loading and conversion to internal settings
src/oci/spec.rs           # Types for the OCI JSON format
src/runtime/state.rs      # State storage, added during lifecycle work
src/runtime/lifecycle.rs  # Lifecycle operations, added during lifecycle work
```

Implementation order:

1. Add a function that converts `RunArgs` into the internal configuration.
2. Change `Runtime::new` to accept this configuration instead of `RunArgs`.
3. Keep the CLI's default CPU, memory, and process limits in the CLI conversion step.
4. Check that the existing `run` command keeps its behavior and exit codes.

An OCI configuration may leave out resource limits. Applying the old CLI defaults in that case would change the requested behavior. Use `Option` or a similar type to represent an unspecified limit.

**Done when:** the existing README examples still work, and Runtime no longer depends on CLI argument types.

## 5. Step 2: Read a bundle and run its program

For this step, add a proposed `run-bundle <bundle>` command. Like the existing `run` command, it waits until the program finishes. This lets us test configuration handling before separating the lifecycle operations.

### Load and validate the configuration

In Rust, `serde` and `serde_json` can read JSON into data types. You can define a small set of types or evaluate a library with OCI types. Either way, successful parsing only tells you that the data can be read. You must also check whether this runtime can apply it.

```text
Convert the bundle path to an absolute path
  → Read config.json
  → Parse it into data types
  → Validate the version, requested features, and values
  → Build ContainerConfig
  → Call the existing execution code
```

Initial validation should cover these points:

- Check the accepted `ociVersion`. If this stage accepts only `1.3.0`, document that as a temporary limit.
- Check that the rootfs exists and is a directory.
- Require `process` and a nonempty `args` array when running a program. Later, treat `create` separately because OCI allows creation without a `process` entry.
- Check that `cwd` is an absolute path inside the container.
- Reject NUL characters in arguments and environment entries. NUL is the zero byte used to end strings in many system calls.
- Report unsupported terminal, mount, or namespace requests before starting setup.

The first version can accept only the namespace combination and proc mount shown in the example. When a request is outside that range, return a clear error. Include the field name, for example: `process.terminal=true is not supported yet`. This is especially important for security settings that the runtime cannot yet apply.

Unknown extension fields need a separate policy from known but unsupported features. Adding `deny_unknown_fields` to every object does not by itself provide full OCI compatibility.

### Apply the process settings

The current `execvp` call inherits the host environment. For OCI execution, use an interface such as `execve` that accepts an explicit environment. Treat a missing `process.env` as an empty environment. To keep the first step small, you can accept only absolute executable paths and document that limit. Add lookup through the container's PATH later.

After switching to the rootfs, change to `process.cwd`. Resolve the program and working directory inside the container filesystem. Finish setup that needs elevated permissions before switching to the requested user.

Keep these two settings separate:

| Setting | Meaning | Example |
| --- | --- | --- |
| `process.user.uid` | The UID used to run the program inside the container | `0` |
| `linux.uidMappings` | How container UID ranges map to host UID ranges | Container `0` maps to host `1000` |

The current `--uid 1000` option has the second meaning. Converting it directly to `process.user.uid=1000` would change its behavior.

Plan ID range mappings, supplementary groups, and `/proc/<pid>/setgroups` together. Supplementary groups are the user's group memberships beyond the main GID. The current code writes `deny` to `setgroups`, which restricts later group changes. Do not assume that `additionalGids` can always be applied after that step.

**Done when:** the sample bundle prints `OCI_STARTED` and runs with the requested working directory and user. It does not inherit unrelated host environment variables. Invalid input fails without leaving cgroups or child processes behind.

## 6. Step 3: Separate create from start

This step needs the largest change to the process structure. After `create`, the environment and init process exist, but the user program has not run. When `start` sends a request, init uses `exec` to run the program. The lifecycle states include `creating`, `created`, `running`, and `stopped`. See the [runtime lifecycle and operations](https://github.com/opencontainers/runtime-spec/blob/v1.3.0/runtime.md).

```text
Absent → creating → created → running → stopped → removed by delete
                       └→ exits before execution → stopped
```

The command names and options below are our proposed CLI design. OCI defines what the operations do, but it does not require a particular CLI syntax.

### Proposed process structure

One approach is to add a monitor process for each container. The monitor tracks program completion and updates the state. This is a design choice for this project, not an extra process required by OCI.

IPC means interprocess communication. It allows processes to exchange requests and results through pipes or sockets.

```text
create CLI
  └─ monitor (in the host PID namespace)
       └─ setup (prepares the user namespace and creates the PID namespace)
            └─ init (container PID 1, waiting for start)

start CLI ── control socket ──→ monitor ── IPC ──→ init calls exec
state CLI ───────────────────→ query state
kill CLI ────────────────────→ ask monitor to signal init
```

1. The CLI reserves the container ID and validates the configuration.
2. The monitor and setup process prepare the isolated environment using the existing communication steps.
3. Setup reports **init's host PID**, obtained from the second fork, to the monitor.
4. Once init has prepared the rootfs and other required settings, it sends READY and waits for a start request.
5. The monitor records `created` and reports success to the CLI. The CLI can exit while the monitor and init remain alive.
6. A separate `start` call sends a request. Init then executes the user program using the saved settings.
7. Setup calls `waitpid` to collect init's exit status and passes the result to the monitor. The monitor updates the state.

Setup failures before READY must also be reported through IPC. EOF means that no more data can be read from a pipe. If the connection closes before an explicit readiness message, treat that as a setup error.

The outer parent currently knows the setup PID. Using that PID as the init PID would make `state` and `kill` refer to the wrong process. Also, init is not a direct child of the monitor in this design. The monitor cannot normally collect its exit status with `waitpid`. Let setup do that, as shown above, or change the process structure.

### Report whether execution succeeded

Sending a start request does not prove that the program started. `exec` can fail because the file is missing or has no execute permission.

An initial implementation can use an error pipe with `CLOEXEC` set. This flag closes the file descriptor when `exec` succeeds. If `exec` fails, init writes the system error code, called errno, to the pipe.

However, a process crash can also close the pipe. EOF alone cannot explain every failure. Combine error messages with process exit tracking in the monitor. A program that exits immediately may already be `stopped` when the caller checks its state.

Save a copy of the validated configuration during `create`. Changes to the original `config.json` after creation must not change the settings used by `start`.

### Store state and define who cleans up resources

An initial implementation that runs as root could use this layout:

```text
/run/container-runtime/<id>/
├── state.json       # Information returned by state queries
├── config.json      # Configuration saved during create
├── internal.json    # Monitor PID, cgroup path, and other internal details
└── control.sock     # Unix socket for control requests
```

Only the runtime user should be able to change these files. Storage under `/run` keeps state between CLI calls, but it does not keep it across a reboot. Reject IDs containing path components such as `/` or `..`.

The `state` output includes `ociVersion`, `id`, `status`, and the absolute bundle path. On Linux, it also includes the real init PID for `created` and `running` containers. Build the public output separately from internal storage files.

Handle the following cases in this step:

- Reserve IDs with an operation such as atomic directory creation. If two callers request the same ID, only one should succeed.
- Use the monitor or a lock to handle conflicting requests one at a time. Examples include two `start` calls or a `start` call during deletion.
- Write updates to a temporary file, then rename it. Readers should never see half of a JSON document.
- Check process identity, not only its PID number. Linux can reuse PIDs. A pidfd, which is a file descriptor referring to a process, or a check of the process start time can help.
- Recover from old state files when the monitor dies. A state query should check whether the recorded process is still alive.
- Review `Cgroup::drop()`. A short CLI command must not own a cgroup that needs to remain after the command exits. Separate cleanup during failed creation from cleanup managed by the monitor and `delete` after successful creation.

### Define each command's responsibility

| Proposed command | Responsibility | Errors to check |
| --- | --- | --- |
| `create --bundle <path> <id>` | Prepare the environment, wait for readiness, and save state | Duplicate ID, invalid settings, setup failure |
| `start <id>` | Execute the user program from the created state | Missing ID, wrong state, exec failure |
| `state <id>` | Return the container state as JSON | Unknown container ID |
| `kill <id> <signal>` | Send the requested signal to init | Missing process, invalid signal |
| `delete <id>` | Remove resources and state when the container is no longer running | Container is still running |

Start with explicit termination followed by deletion. A force-delete option can be added later. If a cgroup is not empty, check for remaining processes. Report cleanup failures instead of returning success. Keep enough state to manage the container until cleanup has finished.

**Done when:** separate CLI processes can control the complete lifecycle, and tests prove that `create` alone never runs the user program.

## 7. Step 4: Extend Linux configuration support

Maintain a table of supported fields as the implementation grows. Mark each field as supported, partly supported, or unsupported, and link it to relevant tests. Use the [Linux configuration reference](https://github.com/opencontainers/runtime-spec/blob/v1.3.0/config-linux.md) for namespace, mapping, and resource requirements.

### Namespaces

Extend the initial fixed combination to handle three cases: creating a namespace, joining an existing one, and leaving a namespace type out of the configuration.

An entry with `path` requests an existing namespace. An entry without `path` requests a new one. Do not automatically create a namespace for an omitted type. Joining an existing namespace requires `setns`. Also account for the PID namespace behavior, which affects children created later.

The current code combines mount namespace creation with rootfs preparation. Separate these responsibilities. If a configuration shares the host mount namespace, the existing mount and `pivot_root` sequence could affect the host. Reject combinations that the runtime cannot safely support yet.

Creating a network namespace does not provide an external network connection. Virtual Ethernet devices, addresses, and routes need separate setup.

### Mounts and rootfs

Start with the example proc mount. Then add bind mounts, tmpfs, and a read-only rootfs. A bind mount exposes an existing file or directory at another path. Tmpfs provides a temporary filesystem backed by memory.

Separate mount options into system call flags and filesystem-specific data. Apply mounts in the order requested by the configuration.

Resolve mount destinations inside the container rootfs. For example, the container's `/proc` must not become the host's `/proc`. In Rust, `PathBuf::join` discards the first path when the second path is absolute. Therefore, `rootfs.join("/proc")` does not produce the intended destination.

Removing the leading `/` is only part of the solution. Path handling must also account for `..`, symbolic links, and paths that change while setup is running. Keep path resolution tied to the intended rootfs.

A read-only bind mount may require a remount or a separate mount attribute operation. Do not assume that adding one flag to the first call is enough. Test the result, including mounts below that path.

### Cgroup v2

Reuse the existing code that writes CPU quota and period, memory limits, and process limits. Extend the internal types to represent unspecified and unlimited values. The current `NonZeroU64` type and fixed CPU period cannot represent every request.

Require only the controllers needed by the requested limits. A controller is the part of cgroup support that manages a resource, such as memory. When adding `linux.cgroupsPath`, validate its path rules and permissions.

Do not apply the old CLI CPU-count check directly to OCI quota values. Quota and period describe a CPU time budget. CPU affinity, which controls which CPUs may run a process, is a separate setting.

## 8. Step 5: Add security settings and other process features

Add these features in small changes. If the runtime cannot apply a requested setting, return an error instead of continuing as if it worked.

| Feature | What it controls | Example check |
| --- | --- | --- |
| Capabilities | Separate permissions that divide up root's powers | Compare the requested sets with `/proc/self/status` |
| `noNewPrivileges` | Prevents gaining new privileges through exec | Check the setting and test that new privileges cannot be gained |
| Seccomp | Filters system calls | Check that selected calls succeed or fail as configured |
| Read-only and masked paths | Restrict writing to paths or hide their contents | Test access from inside the container |
| Hooks | Run external programs at lifecycle stages | Check order, state input, and cleanup after failure |
| Terminal | Manage a pseudo-terminal, or PTY, and its input and output | Test terminal connection and exit behavior |
| Rlimits and additional groups | Set process resource limits and group memberships | Inspect values inside the process |

Hooks run at different lifecycle stages. Their namespace and failure rules depend on the hook type. Do not implement all hooks as callbacks that run before the user program.

This table is a development guide, not a complete list of OCI requirements. Compare the implementation with all relevant requirements in the reference version before claiming full support.

## 9. Test each stage

Keep parsing unit tests separate from integration tests that use real Linux isolation. Namespace, mount, and cgroup tests change host state, so a dedicated Linux virtual machine is useful. The project's current root and cgroup v2 requirements still apply.

### Configuration tests

- Running from a different working directory still finds the same bundle rootfs.
- Environment variables defined only on the host do not appear in the container.
- The working directory, UID, and GID match the requested values inside the process.
- Invalid JSON, unsupported settings, and missing rootfs directories are rejected.
- Missing resource limits do not cause the runtime to apply the CLI defaults.

### Lifecycle tests

After implementation, manual testing can follow this sequence. Confirm the final option syntax and log location as part of the implementation.

```bash
sudo ./target/debug/container-runtime create --bundle ./examples/oci-bundle demo
sudo ./target/debug/container-runtime state demo
sudo ./target/debug/container-runtime start demo
sudo ./target/debug/container-runtime state demo
sudo ./target/debug/container-runtime kill demo KILL
# Check state until it reports stopped before running delete.
sudo ./target/debug/container-runtime state demo
sudo ./target/debug/container-runtime delete demo
```

Decide how the monitor handles standard output and standard error. It can keep suitable output connections open or redirect output to log files. When the `create` CLI exits, the user program must still have the intended output destination.

Automated tests should check the following:

1. After `create`, the state is `created` and `OCI_STARTED` has not been printed.
2. After `start`, the output appears and the long-running sample program has state `running`.
3. The recorded host PID belongs to PID 1 inside the container.
4. Duplicate `create`, repeated `start`, and `delete` while running return the expected errors.
5. Editing the original configuration after `create` does not change program execution.
6. A missing executable causes a reported start failure.
7. After termination, the state eventually becomes `stopped`. After deletion, a state query fails.
8. Failures at each setup stage leave no processes, mounts, or cgroups behind.

Container PID 1 has special signal behavior. Start with KILL, as shown above, to make the termination check simple. Test TERM separately with a program that has a signal handler. A short program may exit before a state query, so do not use it in a test that must observe `running`.

Once these tests work, evaluate [OCI runtime-tools](https://github.com/opencontainers/runtime-tools) for additional checks. Confirm which specification version and features the tool tests. Passing part of a test suite does not prove full OCI support.

## 10. Suggested work checklist

Use small changes so that each review and failure is easier to understand.

- [ ] Introduce `ContainerConfig` while keeping the existing CLI behavior.
- [ ] Add a bundle loader, support checks, and a sample configuration.
- [ ] Apply environment, working directory, and user settings through `run-bundle`.
- [ ] Add IPC messages for the init PID, readiness, and setup failures.
- [ ] Add the monitor and start wait, then separate `create` and `start`.
- [ ] Add state storage, `state`, `kill`, `delete`, and failure cleanup.
- [ ] Extend namespace, mount, ID mapping, and cgroup support.
- [ ] Add security settings, hooks, and terminal support in separate changes.
- [ ] Map specification requirements to tests and document the support status.

Treat each task as complete when its code, behavior checks, and support documentation are all updated.
