use crate::cgroups::CgroupSettings;
use anyhow::{Context, Result, bail};
use clap::Parser;
use std::num::NonZeroU64;
use std::path::PathBuf;

#[derive(Parser, Debug)]
pub struct RunArgs {
    /// Path to the rootfs directory (e.g. ./rootfs).
    pub rootfs: PathBuf,

    /// Number of CPUs (e.g. 0.5 for half a core, 2.0 for two cores).
    #[arg(long, default_value_t = 1.0)]
    pub cpus: f64,

    /// Memory limit (bytes)
    #[arg(long, default_value_t = 512 * 1024 * 1024)]
    pub mem: u64,

    /// Maximum number of tasks (processes and threads).
    #[arg(long, default_value_t = 1024)]
    pub pids: u64,

    /// Host UID to map with container's root user
    #[arg(long, default_value_t = 0)]
    pub uid: u32,

    /// Host GID to map with container's root user
    #[arg(long, default_value_t = 0)]
    pub gid: u32,

    /// Command to execute inside the container.
    pub cmd: String,

    /// Arguments to pass to the command.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

impl RunArgs {
    pub fn validate(&self) -> Result<CgroupSettings> {
        if !self.rootfs.is_dir() {
            bail!(
                "rootfs {:?} does not exist or is not a directory",
                self.rootfs
            );
        }

        if !self.cpus.is_finite() || self.cpus <= 0.0 {
            bail!("--cpus must be finite and positive but got {}", self.cpus);
        }

        let host_cpus = num_cpus::get() as f64;
        if self.cpus > host_cpus {
            bail!(
                "--cpus must be less than or equal to `max_cpus`. you have only {} cpus available",
                host_cpus
            );
        }

        let cpu_quota_us = (self.cpus * CgroupSettings::CPU_PERIOD_US as f64) as u64;
        let cpu_quota_us =
            NonZeroU64::new(cpu_quota_us).context("--cpus is too small for a nonzero CPU quota")?;

        Ok(CgroupSettings::new(cpu_quota_us, self.mem, self.pids))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_cpu_limits() {
        let mut args = RunArgs {
            rootfs: PathBuf::from("/"),
            cpus: 1.0,
            mem: 1,
            pids: 1,
            uid: 0,
            gid: 0,
            cmd: String::from("true"),
            args: Vec::new(),
        };

        assert!(args.validate().is_ok());
        for cpus in [f64::NAN, f64::INFINITY, 0.0, -1.0, 0.000001] {
            args.cpus = cpus;
            assert!(args.validate().is_err(), "accepted {cpus}");
        }
    }
}
