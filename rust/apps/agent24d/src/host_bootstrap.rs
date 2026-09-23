//! Trusted stdio bootstrap for capability-mode daemons.
//!
//! The trusted launcher must place access-controlled pipe or socket endpoints
//! on stdin and stdout: stdin lasts for the parent's lifetime and stdout carries
//! the ready record. This module verifies only that the inherited descriptors
//! are IPC channels, rejecting terminals and regular files. `fstat` cannot by
//! itself prove that a FIFO or socket is private; that remains the launcher's
//! placement contract. stdin EOF ends the owning-parent lease.

#![allow(dead_code)] // Staged until the capability startup layer consumes it.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

pub struct ReadyWriter(tokio::fs::File);

pub struct ParentLiveness(tokio::fs::File);

/// Duplicate validated stdio channels with close-on-exec so these values retain them safely.
pub fn open_stdio() -> io::Result<(ReadyWriter, ParentLiveness)> {
    let stdin = validated_channel(rustix::stdio::stdin())?;
    let stdout = validated_channel(rustix::stdio::stdout())?;
    Ok((
        ReadyWriter(tokio::fs::File::from_std(stdout)),
        ParentLiveness(tokio::fs::File::from_std(stdin)),
    ))
}

fn validated_channel(fd: impl rustix::fd::AsFd) -> io::Result<std::fs::File> {
    let fd = rustix::io::fcntl_dupfd_cloexec(fd, 3).map_err(io::Error::from)?;
    let stat = rustix::fs::fstat(&fd).map_err(io::Error::from)?;
    if !is_ipc_channel(stat.st_mode) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capability bootstrap requires FIFO or socket stdin and stdout",
        ));
    }
    Ok(fd.into())
}

fn is_ipc_channel(mode: rustix::fs::RawMode) -> bool {
    matches!(
        rustix::fs::FileType::from_raw_mode(mode),
        rustix::fs::FileType::Fifo | rustix::fs::FileType::Socket
    )
}

impl ReadyWriter {
    pub async fn send(&mut self, ready: &serde_json::Value) -> io::Result<()> {
        let mut line = serde_json::to_vec(ready).map_err(io::Error::other)?;
        line.push(b'\n');
        self.0.write_all(&line).await?;
        self.0.flush().await
    }
}

impl ParentLiveness {
    pub async fn closed(self) {
        wait_for_eof(self.0).await;
    }
}

async fn wait_for_eof(mut reader: impl AsyncRead + Unpin) {
    let mut discard = [0_u8; 64];
    loop {
        match reader.read(&mut discard).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn parent_eof_ends_the_liveness_lease() -> io::Result<()> {
        use std::os::unix::net::UnixStream as StdUnixStream;

        let (reader, writer) = StdUnixStream::pair()?;
        let reader = validated_channel(&reader)?;
        let liveness = ParentLiveness(tokio::fs::File::from_std(reader));
        let waiting = tokio::spawn(liveness.closed());
        drop(writer);
        let completed = tokio::time::timeout(std::time::Duration::from_secs(1), waiting).await;
        assert!(completed.is_ok(), "parent EOF was not observed");
        assert!(completed.is_ok_and(|result| result.is_ok()));
        Ok(())
    }

    #[test]
    fn only_fifo_or_socket_is_an_ipc_channel() {
        assert!(is_ipc_channel(rustix::fs::FileType::Fifo.as_raw_mode()));
        assert!(is_ipc_channel(rustix::fs::FileType::Socket.as_raw_mode()));
        assert!(!is_ipc_channel(
            rustix::fs::FileType::RegularFile.as_raw_mode()
        ));
        assert!(!is_ipc_channel(
            rustix::fs::FileType::CharacterDevice.as_raw_mode()
        ));
    }

    #[test]
    fn regular_file_is_rejected_as_a_bootstrap_channel() -> io::Result<()> {
        let regular_file = tempfile::tempfile()?;
        let error = match validated_channel(&regular_file) {
            Ok(_) => panic!("regular file was accepted as a bootstrap channel"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn validated_channel_is_close_on_exec_and_outlives_its_source() -> io::Result<()> {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream as StdUnixStream;

        let (source, mut peer) = StdUnixStream::pair()?;
        let mut duplicate = validated_channel(&source)?;
        drop(source);

        assert!(rustix::io::fcntl_getfd(&duplicate)?.contains(rustix::io::FdFlags::CLOEXEC));
        duplicate.write_all(b"ok")?;
        let mut received = [0; 2];
        peer.read_exact(&mut received)?;
        assert_eq!(received, *b"ok");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ready_writer_flushes_to_its_owned_channel() -> io::Result<()> {
        use std::os::unix::net::UnixStream as StdUnixStream;

        let (source, reader) = StdUnixStream::pair()?;
        let writer = validated_channel(&source)?;
        drop(source);
        let mut ready_writer = ReadyWriter(tokio::fs::File::from_std(writer));
        reader.set_nonblocking(true)?;
        let mut reader = tokio::net::UnixStream::from_std(reader)?;

        ready_writer
            .send(&serde_json::json!({ "ready": true }))
            .await?;
        let mut line = [0; 15];
        reader.read_exact(&mut line).await?;
        assert_eq!(&line, b"{\"ready\":true}\n");
        Ok(())
    }
}
