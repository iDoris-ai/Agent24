//! Trusted stdio bootstrap for capability-mode daemons.
//!
//! The spawning host gives the child a private stdin pipe for its lifetime and
//! a private stdout pipe for the ready record. Capability mode refuses terminal
//! or file-backed stdio, sends the host bearer only through that ready pipe,
//! and treats stdin EOF as loss of the owning parent.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

pub struct ReadyWriter;

pub struct ParentLiveness;

/// Validate that stdio is a pair of IPC channels, not a terminal or a file.
pub fn open_stdio() -> io::Result<(ReadyWriter, ParentLiveness)> {
    let stdin = rustix::fs::fstat(rustix::stdio::stdin()).map_err(io::Error::from)?;
    let stdout = rustix::fs::fstat(rustix::stdio::stdout()).map_err(io::Error::from)?;
    if !private_channel(stdin.st_mode) || !private_channel(stdout.st_mode) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capability bootstrap requires private piped stdin and stdout",
        ));
    }
    Ok((ReadyWriter, ParentLiveness))
}

fn private_channel(mode: rustix::fs::RawMode) -> bool {
    matches!(
        rustix::fs::FileType::from_raw_mode(mode),
        rustix::fs::FileType::Fifo | rustix::fs::FileType::Socket
    )
}

impl ReadyWriter {
    pub async fn send(&mut self, ready: &serde_json::Value) -> io::Result<()> {
        let mut line = serde_json::to_vec(ready).map_err(io::Error::other)?;
        line.push(b'\n');
        let mut stdout = tokio::io::stdout();
        stdout.write_all(&line).await?;
        stdout.flush().await
    }
}

impl ParentLiveness {
    pub async fn closed(self) {
        wait_for_eof(tokio::io::stdin()).await;
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

    #[tokio::test]
    async fn parent_eof_ends_the_liveness_lease() {
        let (reader, writer) = tokio::io::duplex(64);
        let waiting = tokio::spawn(wait_for_eof(reader));
        drop(writer);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("parent EOF was not observed")
            .unwrap();
    }

    #[test]
    fn only_fifo_or_socket_is_a_private_channel() {
        assert!(private_channel(rustix::fs::FileType::Fifo.as_raw_mode()));
        assert!(private_channel(rustix::fs::FileType::Socket.as_raw_mode()));
        assert!(!private_channel(
            rustix::fs::FileType::RegularFile.as_raw_mode()
        ));
        assert!(!private_channel(
            rustix::fs::FileType::CharacterDevice.as_raw_mode()
        ));
    }
}
