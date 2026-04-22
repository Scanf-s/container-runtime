use std::path::Path;
use anyhow::{Context, Result};
use std::ffi::CString;
use nix::unistd::{chroot, chdir, execvp};

/// Restrict the process's view of the filesystem to `rootfs` using the `chroot` system call.
pub fn isolate_fs_chroot(rootfs: &Path) -> Result<()> {
    // Change the current process's root directory.
    chroot(rootfs).with_context(|| format!("chroot {:?}", rootfs))?;

    // Reset the working directory so it matches the new root.
    chdir("/").context("chdir(\"/\") after chroot")?;

    Ok(())
}


/// Replace the current process with `cmd` + `args` via `execvp`.
/// Returns only on failure.
pub fn exec_cmd(cmd: &str, args: &[String]) -> Result<()> {
    // Convert the command name into a nul-safe CString.
    let c_cmd = CString::new(cmd).context("cmd contains a nul byte")?;

    // Convert the arguments into nul-safe CStrings.
    let mut c_args: Vec<CString> = Vec::with_capacity(args.len() + 1);
    c_args.push(c_cmd.clone()); // argv[0] — conventionally the program name
    for arg in args {
        c_args.push(CString::new(arg.as_str()).context("arg contains a nul byte")?);
    }

    // Replace the current process image with the target program.
    execvp(&c_cmd, &c_args).with_context(|| format!("exec {:?}", cmd))?;

    unreachable!("failed to execute: {} {}", cmd, args.join(" "));
}
