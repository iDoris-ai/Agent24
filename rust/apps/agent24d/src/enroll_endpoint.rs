//! CORE-2e — the enrolment endpoint `<data_dir>/enroll.sock`
//! (`docs/agent/CORE-2-ENROLL-DESIGN.md` D5, D8).
//!
//! This slice binds and owns the socket only. Every request is answered with
//! `enroll_unavailable` and the connection closed: no token, no identity
//! check — those are CORE-2c/2b. The holder of the singleton lock creates the
//! socket, and removes only the node it created.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

pub const SOCKET_NAME: &str = "enroll.sock";
/// One JSON line per frame, at most this many bytes (D5).
const MAX_FRAME: u64 = 16 * 1024;
const FRAME_DEADLINE: Duration = Duration::from_secs(5);
/// Connections served at once; further ones are closed unanswered so a slow
/// or hostile peer cannot pile up tasks or exhaust descriptors.
const MAX_CONCURRENT: usize = 8;
const UNAVAILABLE: &[u8] =
    b"{\"error\":\"enroll_unavailable\",\"reason\":\"2c_not_implemented\"}\n";

/// A bound endpoint. Dropping it unlinks the socket node this instance
/// created, and only that node (device + inode must still match).
#[cfg(unix)]
pub struct EnrollEndpoint {
    path: PathBuf,
    identity: (u64, u64),
    listener: Option<tokio::net::UnixListener>,
}

#[cfg(unix)]
impl EnrollEndpoint {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Start accepting. The task ends when `stop` fires.
    pub fn serve(&mut self, stop: CancellationToken) {
        let Some(listener) = self.listener.take() else {
            return;
        };
        let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT));
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => break,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => {
                            let Ok(permit) = permits.clone().try_acquire_owned() else {
                                continue; // over the cap: drop = close
                            };
                            let stop = stop.clone();
                            tokio::spawn(async move {
                                let _permit = permit;
                                tokio::select! {
                                    () = stop.cancelled() => {}
                                    () = answer(stream) => {}
                                }
                            });
                        }
                        Err(err) => {
                            // e.g. EMFILE: back off instead of spinning.
                            tracing::warn!("enroll endpoint accept failed: {err}");
                            tokio::select! {
                                () = stop.cancelled() => break,
                                () = tokio::time::sleep(Duration::from_millis(200)) => {}
                            }
                        }
                    },
                }
            }
        });
    }
}

#[cfg(unix)]
impl Drop for EnrollEndpoint {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if let Ok(meta) = std::fs::symlink_metadata(&self.path)
            && (meta.dev(), meta.ino()) == self.identity
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Read one bounded frame, reply `enroll_unavailable`, close. Oversize,
/// timeout and early EOF close silently.
#[cfg(unix)]
async fn answer(stream: tokio::net::UnixStream) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let (read, mut write) = stream.into_split();
    let mut frame = Vec::new();
    let mut reader = BufReader::new(read.take(MAX_FRAME));
    let got = tokio::time::timeout(FRAME_DEADLINE, reader.read_until(b'\n', &mut frame)).await;
    if matches!(got, Ok(Ok(n)) if n > 0) && frame.ends_with(b"\n") {
        let _ = tokio::time::timeout(FRAME_DEADLINE, write.write_all(UNAVAILABLE)).await;
    }
    let _ = write.shutdown().await;
}

/// Verify `dir` is a real directory of ours and make it 0700. Refuses a
/// symlink or a directory owned by someone else. The directory is opened once
/// (`O_NOFOLLOW | O_DIRECTORY`) and checked and chmod-ed through that fd, so
/// nothing can be swapped in between the check and the chmod.
#[cfg(unix)]
fn private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(dir)
        .map_err(|err| {
            io::Error::other(format!(
                "{} is not a plain directory (or is a symlink): {err}",
                dir.display()
            ))
        })?;
    let meta = handle.metadata()?;
    if meta.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::other(format!(
            "{} is not owned by this user",
            dir.display()
        )));
    }
    handle.set_permissions(std::fs::Permissions::from_mode(0o700))
}

/// Bind `<dir>/enroll.sock`. The caller must hold the singleton lock: a node
/// already there is then either a corpse (connect refused: unlinked) or a live
/// foreign listener / non-socket (refused, never overwritten).
#[cfg(unix)]
pub fn bind(dir: &Path) -> io::Result<EnrollEndpoint> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    private_dir(dir)?;
    let path = dir.join(SOCKET_NAME);
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if !meta.file_type().is_socket() {
            return Err(io::Error::other(format!(
                "{} exists and is not a socket",
                path.display()
            )));
        }
        // Only a definite "nobody is listening" proves a corpse. Any other
        // outcome (accepted, would-block, permission...) leaves it alone.
        match std::os::unix::net::UnixStream::connect(&path) {
            Err(err) if err.kind() == io::ErrorKind::ConnectionRefused => {
                std::fs::remove_file(&path)?;
            }
            Err(err) => {
                return Err(io::Error::other(format!(
                    "{} cannot be proven stale ({err}); not removing it",
                    path.display()
                )));
            }
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "{} is already accepting connections",
                    path.display()
                )));
            }
        }
    }
    let listener = tokio::net::UnixListener::bind(&path)?;
    // From here Drop owns the node, so a failure below still cleans up.
    let identity = std::fs::symlink_metadata(&path).map(|m| (m.dev(), m.ino()))?;
    let endpoint = EnrollEndpoint {
        path: path.clone(),
        identity,
        listener: Some(listener),
    };
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(endpoint)
}

#[cfg(not(unix))]
pub struct EnrollEndpoint;

#[cfg(not(unix))]
impl EnrollEndpoint {
    pub fn path(&self) -> &Path {
        Path::new("")
    }
    pub fn serve(&mut self, _stop: CancellationToken) {}
}

#[cfg(not(unix))]
pub fn bind(_dir: &Path) -> io::Result<EnrollEndpoint> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the enrolment endpoint requires Unix sockets",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_loose_directory_of_ours_is_tightened_to_0700() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("d");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o700);
    }

    #[test]
    fn a_symlinked_directory_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(private_dir(&link).is_err());
    }
}
