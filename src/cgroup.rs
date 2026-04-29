use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use rand::random;

pub struct Cgroup {
    path: PathBuf,
}

impl Cgroup {

    // Create a new cgroup for the container.
    pub fn new() -> Result<Self> {
        // controllers = resources
        let cgroup_path: &Path = Path::new("/sys/fs/cgroup");
        let subtree_path: &Path = Path::new("/sys/fs/cgroup/cgroup.subtree_control");

        // Check that cgroupfs is mounted as v2.
        let v2_marker = cgroup_path.join("cgroup.controllers");
        if !v2_marker.is_file() {
            bail!("cgroupfs is not v2 (or not mounted)");
        }

        // cgroup operations require root privileges.
        if !nix::unistd::geteuid().is_root() {
            bail!("cgroup operations require root privileges");
        }

        // Check whether subtree_control delegates the memory, cpu, and pids
        // controllers to children. If any are missing, enable them below.
        let controllers: String = fs::read_to_string(subtree_path)?;
        let active_controllers: Vec<&str> = controllers.split_whitespace().collect();
        let required_controllers: Vec<&str> = vec!["memory", "cpu", "pids"];
        let mut missing_controllers: Vec<&str> = Vec::new();
        for controller in &required_controllers {
            if !active_controllers.contains(controller) {
                missing_controllers.push(controller);
            }
        }

        // Enable any missing controllers on the parent cgroup.
        if !missing_controllers.is_empty() {
            let payload = missing_controllers.iter()
                .map(|c| format!("+{}", c))
                .collect::<Vec<_>>()
                .join(" ");
            fs::write(subtree_path, payload)
                .context("failed to enable controllers")?;
        }

        // Create a new cgroup subdirectory for this container.
        let id = format!("rust_container_{:x}", random::<u64>());
        let new_container_cgroup = cgroup_path.join(&id);
        fs::create_dir(&new_container_cgroup).context("create cgroup dir")?;

        // Verify the required controllers were delegated to the new cgroup.
        let delegated = fs::read_to_string(new_container_cgroup.join("cgroup.controllers"))
            .context("read new cgroup.controllers")?;
        let delegated: Vec<&str> = delegated.split_whitespace().collect();
        for c in &required_controllers {
            if !delegated.contains(c) {
                bail!("controller {c} not delegated to new cgroup (check parent's cgroup.subtree_control)");
            }
        }

        Ok(Cgroup { path: new_container_cgroup })
    }

    pub fn add_pid(&self, pid: nix::unistd::Pid) -> Result<()> {
        fs::write(self.path.join("cgroup.procs"), pid.to_string()).context("write cgroup.procs")?;
        Ok(())
    }

    pub fn set_memory_max(&self, bytes: u64) -> Result<()> {
        fs::write(self.path.join("memory.max"), bytes.to_string()).context("write memory.max")?;
        Ok(())
    }

    pub fn set_cpu_max(&self, quota_us: u64, period_us: u64) -> Result<()> {
        fs::write(self.path.join("cpu.max"), format!("{quota_us} {period_us}")).context("write cpu.max")?;
        Ok(())
    }

    pub fn set_pids_max(&self, pids: u64) -> Result<()> {
        fs::write(self.path.join("pids.max"), pids.to_string()).context("write pids.max")?;
        Ok(())
    }
}

// Remove the cgroup directory when the handle is dropped.
impl Drop for Cgroup {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}
