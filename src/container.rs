use std::path::Path;
use anyhow::{Context, Result};
use std::ffi::CString;
use nix::unistd::{chroot, chdir, execvp};

/// Restrict the process's view of the filesystem to `rootfs` using `chroot` system call
pub fn isolate_fs_chroot(rootfs: &Path) -> Result<()> {
    // Change current process's root directory
    chroot(rootfs).with_context(|| format!("chroot {:?}", rootfs))?;

    // Change current process's working directory
    chdir("/").context("chdir(\"/\") after chroot")?;

    // Return success result
    Ok(())
}


/// Replace the current process with `cmd` + `args` via execvp
/// Returns only on failure.
pub fn exec_cmd(cmd: &str, args: &[String]) -> Result<()> {
    // Parse command into null-safe CString struct
    let c_cmd = CString::new(cmd).context("cmd contains a nul byte")?;

    // Parse arguments into null-safe CString vector
    let mut c_args: Vec<CString> = Vec::with_capacity(args.len() + 1);
    c_args.push(c_cmd.clone()); // Subcommand
    for arg in args {
        c_args.push(CString::new(arg.as_str()).context("arg contains a nul byte")?);
    }

    // Run command in the separated memory
    execvp(&c_cmd, &c_args).with_context(|| format!("exec {:?}", cmd))?;

    unreachable!("failed to execute: {} {}", cmd, args.join(" "));
}