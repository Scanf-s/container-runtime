use super::{send_ready, wait_ready};
use anyhow::{Context, Result};
use std::os::fd::OwnedFd;

pub(crate) struct ChildChannels {
    cgroup_reader: Option<OwnedFd>,
    user_namespace_writer: Option<OwnedFd>,
    mapping_reader: Option<OwnedFd>,
}

impl ChildChannels {
    pub(super) fn new(
        cgroup_reader: OwnedFd,
        user_namespace_writer: OwnedFd,
        mapping_reader: OwnedFd,
    ) -> Self {
        Self {
            cgroup_reader: Some(cgroup_reader),
            user_namespace_writer: Some(user_namespace_writer),
            mapping_reader: Some(mapping_reader),
        }
    }

    // wait until the parent finishes its cgroup setting
    pub(crate) fn wait_for_cgroup(&mut self) -> Result<()> {
        let fd = self
            .cgroup_reader
            .take()
            .context("cgroup already checked")?;
        wait_ready(&fd, "cgroup")
    }

    // signal to the parent that user/network namespace isolation is done
    pub(crate) fn signal_user_namespace_ready(&mut self) -> Result<()> {
        let fd = self
            .user_namespace_writer
            .take()
            .context("user namespace already signaled")?;
        send_ready(&fd, "user namespace")
    }

    // wait until the parent finishes its uid/gid mapping task
    pub(crate) fn wait_for_mapping(&mut self) -> Result<()> {
        let fd = self
            .mapping_reader
            .take()
            .context("mapping already checked")?;
        wait_ready(&fd, "mapping")
    }
    
}
