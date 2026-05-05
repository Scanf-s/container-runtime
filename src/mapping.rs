use nix::unistd::{Gid, Pid, Uid};
use anyhow::{Context, Result};

pub struct Mapping {
    process_id: Pid,
    host_uid: Uid,
    host_gid: Gid,
}

impl Mapping {

    pub fn new(process_id: Pid, host_uid: Uid, host_gid: Gid) -> Mapping {
        Mapping {process_id: process_id, host_uid: host_uid, host_gid: host_gid }
    }

    pub fn map(&self) -> Result<()> {

        // UidMap
        std::fs::write(
            format!("/proc/{}/uid_map", self.process_id.as_raw()),
            format!("0 {} 1\n", self.host_uid.as_raw())
        ).with_context(|| format!("write /proc/{}/uid_map", self.process_id.as_raw()))?;

        // SetGroups
        std::fs::write(
            format!("/proc/{}/setgroups", self.process_id.as_raw()),
            "deny\n"
        ).with_context(|| format!("write /proc/{}/setgroups", self.process_id.as_raw()))?;

        // Gidmap
        std::fs::write(
            format!("/proc/{}/gid_map", self.process_id.as_raw()),
            format!("0 {} 1\n", self.host_gid.as_raw())
        ).with_context(|| format!("write /proc/{}/gid_map", self.process_id.as_raw()))?;

        Ok(())
    }
}