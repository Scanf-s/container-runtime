# OCI direction

The Open Container Initiative (OCI) publishes three related specifications:

- **Runtime:** how to configure, start, and manage a container from a local filesystem bundle.
- **Image:** how to package a container filesystem and its metadata.
- **Distribution:** how to transfer images to and from a registry.

This project is a runtime, so the [OCI Runtime Specification](https://github.com/opencontainers/runtime-spec/tree/v1.3.0) is the first relevant target.
An OCI bundle is a directory containing a `config.json` file and the root filesystem.

The JSON describes the process, environment, mounts, namespaces, and resource settings. 

See the [bundle format](https://github.com/opencontainers/runtime-spec/blob/v1.3.0/bundle.md) and [configuration fields](https://github.com/opencontainers/runtime-spec/blob/v1.3.0/config.md).

## First milestone

I'm going to add a command that accepts a bundle directory and runs the process described by its `config.json`.

1. Resolve `root.path` relative to the bundle and read `process.args`, `process.env`, and `process.cwd`.
2. Translate the supported Linux namespace and cgroup settings into the runtime's existing setup code.
3. Reject unsupported settings with a clear error.
4. Prepare the sample job (application) and run it without passing its command and rootfs path as separate CLI arguments.

After that, implement the standard lifecycle operations (`create`, `start`, `state`, `kill`, and `delete`),
described by the [runtime lifecycle](https://github.com/opencontainers/runtime-spec/blob/v1.3.0/runtime.md),

then use [OCI runtime-tools](https://github.com/opencontainers/runtime-tools) to validate bundles and find lifecycle gaps. 
Image pulling and registry support can come later because they belong to the other two specifications.
