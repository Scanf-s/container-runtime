//! Pipes used to synchronize the parent and setup child during startup.
//!
//! Parent -> child: cgroup ready. Child -> parent: user namespace ready.
//! Parent -> child: UID/GID mapping ready.

mod child_channels;
mod parent_channels;
mod startup_channels;

use child_channels::ChildChannels;
use parent_channels::ParentChannels;
pub(crate) use startup_channels::StartupChannels;

use anyhow::{Context, Result, bail};
use nix::errno::Errno;
use nix::unistd::{read, write};
use std::os::fd::{AsRawFd, OwnedFd};

const READY: u8 = 1;

fn send_ready(fd: &OwnedFd, stage: &str) -> Result<()> {
    loop {
        match write(fd, &[READY]) {
            Ok(1) => return Ok(()),
            Ok(count) => bail!("sent {count} bytes for {stage}; expected one"),
            // Retry if an OS signal interrupts the write() systemcall, before writing something
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(error).with_context(|| format!("signal {stage} ready")),
        }
    }
}

fn wait_ready(fd: &OwnedFd, stage: &str) -> Result<()> {
    let mut byte = [0u8; 1];
    loop {
        match read(fd.as_raw_fd(), &mut byte) {
            Ok(1) if byte[0] == READY => return Ok(()),
            Ok(1) => bail!("unexpected {stage} signal: {}", byte[0]),
            Ok(0) => bail!("peer closed {stage} pipe before signaling"),
            Ok(count) => bail!("read {count} bytes for {stage}; expected one"),
            // Retry if an OS signal interrupts the read() systemcall, before reading something
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(error).with_context(|| format!("wait for {stage}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::unistd::pipe;

    #[test]
    fn writer_closed_pipe_first_before_sending_something() {
        let (reader, writer) = pipe().unwrap();
        drop(writer);

        let error = wait_ready(&reader, "test").unwrap_err();
        assert!(error.to_string().contains("closed test pipe"));
    }

    #[test]
    fn wait_ready_receives_signal() {
        let (reader, writer) = pipe().unwrap();
        write(&writer, &[1]).unwrap();
        drop(writer);

        wait_ready(&reader, "test").unwrap();
    }

    #[test]
    fn wait_ready_rejects_unexpected_signal() {
        let (reader, writer) = pipe().unwrap();
        write(&writer, &[2]).unwrap();
        drop(writer);

        let error = wait_ready(&reader, "test").unwrap_err();
        assert!(error.to_string().contains("unexpected test signal"));
    }
}
