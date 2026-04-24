use std::fs;
use std::path::Path;
use anyhow::{Context, Result};
use nix::mount::{MntFlags, MsFlags, mount, umount2};
use std::ffi::CString;
use nix::unistd::{chdir, chroot, execvp, pivot_root};
use nix::sched::{unshare, CloneFlags};

/// Restrict the process's view of the filesystem to `rootfs` using the `chroot` system call.
/// Kept for reference; superseded by `isolate_fs_pivot`. See README for the escape demo.
#[allow(dead_code)]
pub fn isolate_fs_chroot(rootfs: &Path) -> Result<()> {
    // Change the current process's root directory.
    chroot(rootfs).with_context(|| format!("chroot {:?}", rootfs))?;

    // Reset the working directory so it matches the new root.
    chdir("/").context("chdir(\"/\") after chroot")?;

    Ok(())
}

/// Full filesystem isolation: new mount namespace, pivot_root into `rootfs`,
/// detach the old root, and mount a fresh `/proc`.
/// See README ("pivot_root filesystem isolation") for the rationale behind each step.
pub fn isolate_fs_pivot(rootfs: &Path) -> Result<()> {
    // New mount namespace for this process.
    unshare(CloneFlags::CLONE_NEWNS).context("unshare(CLONE_NEWNS)")?;

    // Make "/" recursively private so mounts do not propagate to the host.
    mount::<str, _, str, str>(
        None,
        "/",
        None,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None,
    )
    .context("mount / MS_REC|MS_PRIVATE")?;

    // Bind rootfs onto itself — pivot_root requires new_root to be a mount point distinct from its parent.
    mount::<_, _, str, str>(
        Some(rootfs),
        rootfs,
        None,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None,
    )
    .with_context(|| format!("bind mount {:?} onto itself", rootfs))?;

    // Directory inside the new root to receive the old root.
    let old_root = rootfs.join(".old");
    fs::create_dir_all(&old_root)
        .with_context(|| format!("create_dir_all {:?}", old_root))?;

    // Swap root: rootfs becomes /, previous root is relocated into /.old.
    pivot_root(rootfs, old_root.as_path())
        .with_context(|| format!("pivot_root({:?}, {:?})", rootfs, old_root))?;

    // Reset CWD to the new root (pivot_root does not touch the current dir).
    chdir("/").context("chdir(\"/\") after pivot_root")?;

    // Detach the old root lazily (files may still be held open), then remove the now-empty stub. 
    // Must be remove_dir (not remove_dir_all).
    umount2("/.old", MntFlags::MNT_DETACH).context("umount2(/.old)")?;
    fs::remove_dir("/.old").context("remove_dir(/.old)")?;

    // Mount a fresh procfs (detaching the old root removed the host /proc).
    fs::create_dir_all("/proc").context("create /proc")?;
    mount::<_, _, _, str>(
        Some("proc"),
        "/proc",
        Some("proc"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
        None,
    )
    .context("mount /proc")?;

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
