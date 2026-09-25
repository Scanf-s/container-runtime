use anyhow::{Context, Result, bail};
use nix::mount::{MntFlags, MsFlags, mount, umount2};
use nix::sched::{CloneFlags, unshare};
use nix::unistd::{chdir, pivot_root};
use std::fs;
use std::path::PathBuf;

pub struct PivotRoot {
    pub rootfs: PathBuf,
}

impl PivotRoot {
    pub fn new(rootfs: PathBuf) -> Self {
        Self { rootfs }
    }

    /// Isolate the filesystem by pivoting into `rootfs`, then detach the old root.
    pub fn isolate_filesystem(&self) -> Result<()> {
        self.validate_rootfs()?;

        // Create new mount table for this new process
        self.set_private_mount_namespace()?;

        // Prepare a new mount point of rootfs to pass it on the pivot_root
        mount::<_, _, str, str>(
            Some(self.rootfs.as_os_str()),
            self.rootfs.as_os_str(),
            None,
            MsFlags::MS_BIND | MsFlags::MS_REC,
            None,
        )
        .with_context(|| format!("bind mount {:?} onto itself", self.rootfs.as_os_str()))?;

        // Mount necessary virtual filesystems
        self.mount_proc()?;
        // TODO: ...

        // Isolate filesystem with new configured mount table using pivot_root
        self.pivot_root()?;

        Ok(())
    }

    fn validate_rootfs(&self) -> Result<()> {
        if !self.rootfs.is_dir() {
            bail!(
                "rootfs {:?} does not exist or is not a directory",
                self.rootfs
            );
        }
        Ok(())
    }

    // Create new namespace and mount table for the new process
    fn set_private_mount_namespace(&self) -> Result<()> {
        unshare(CloneFlags::CLONE_NEWNS).context("unshare(CLONE_NEWNS)")?;
        mount::<str, _, str, str>(None, "/", None, MsFlags::MS_REC | MsFlags::MS_PRIVATE, None)
            .context("mount / MS_REC|MS_PRIVATE")?;
        Ok(())
    }

    // create and mount new proc virtual filesystem
    fn mount_proc(&self) -> Result<()> {
        let proc_path = self.rootfs.join("proc");
        fs::create_dir_all(&proc_path)
            .with_context(|| format!("create {}", proc_path.display()))?;
        mount::<_, _, _, str>(
            Some("proc"),
            proc_path.as_path(),
            Some("proc"),
            MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
            None,
        )
        .with_context(|| format!("mount proc on {:?}", proc_path))?;
        Ok(())
    }

    fn pivot_root(&self) -> Result<()> {
        // Prepare new directory to move the current root later here
        let old_root = self.rootfs.join(".old");
        fs::create_dir_all(&old_root).with_context(|| format!("create_dir_all {:?}", old_root))?;

        // rootfs becomes /, previous root is relocated into /.old
        pivot_root(self.rootfs.as_os_str(), old_root.as_path()).with_context(|| {
            format!("pivot_root({:?}, {:?})", self.rootfs.as_os_str(), old_root)
        })?;

        // Reset CWD to the new root
        chdir("/").context("chdir(\"/\") after pivot_root")?;

        // Umount the old root
        umount2("/.old", MntFlags::MNT_DETACH).context("umount2(/.old)")?;
        fs::remove_dir("/.old").context("remove_dir(/.old)")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::random;

    #[test]
    fn rejects_missing_rootfs_before_mount_operations() {
        let rootfs = std::env::temp_dir().join(format!("missing-rootfs-{:x}", random::<u64>()));
        let error = PivotRoot::new(rootfs.clone())
            .isolate_filesystem()
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not exist or is not a directory")
        );
        assert!(!rootfs.exists());
    }

    #[test]
    fn rejects_regular_file_as_rootfs() {
        let path = std::env::temp_dir().join(format!("rootfs-file-{:x}", random::<u64>()));
        fs::write(&path, "not a directory").unwrap();
        let error = PivotRoot::new(path.clone())
            .isolate_filesystem()
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not exist or is not a directory")
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn accepts_an_existing_directory_as_rootfs() {
        let path = std::env::temp_dir().join(format!("rootfs-dir-{:x}", random::<u64>()));
        fs::create_dir(&path).unwrap();
        assert!(PivotRoot::new(path.clone()).validate_rootfs().is_ok());
        fs::remove_dir(path).unwrap();
    }
}
