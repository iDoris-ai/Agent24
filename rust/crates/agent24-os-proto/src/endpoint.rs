//! ME3-SUP slice 2 — the kernel's end of a module's callback connection: where
//! the socket lives, how exactly one connection is taken from it, and the
//! handshake on that connection.
//!
//! SPEC-ME3 §1 and §3: the kernel listens on a path it chose (`0700`), hands the
//! path to the child in `A24_CALLBACK_SOCK`, and the module connects and sends
//! `initialize` as its first message. Ready means that handshake succeeded;
//! anything else during it — a malformed frame, a wrong token, silence past the
//! deadline — disconnects, after saying why.
//!
//! # One connection per generation, by structure
//!
//! A [`CallbackListener`] belongs to one generation and is consumed by
//! [`CallbackListener::accept_one`]: the first connection is taken, and the
//! listener is closed and its path removed at once. A second `connect` finds
//! nothing to connect to. That is how "the callback connection IS the
//! generation's lifeline" (user decision D1: it breaking ends the generation,
//! and the same generation may not reconnect) is kept without a rule anyone
//! has to remember to check.
//!
//! # Not in this slice
//!
//! Choosing when to listen, spawning, and acting on the end of a connection —
//! the supervisor loop, ME3-SUP slice 3.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::frame::FrameError;
use crate::initialize::{self, Accepted, Expectation, HandshakeError};

/// The longest socket path accepted, in bytes. A Unix socket address holds 104
/// bytes on macOS (108 on Linux) including the terminating NUL; the smaller
/// bound is used everywhere so a path that works here works there.
pub const MAX_SOCKET_PATH: usize = 103;

/// How long the kernel gives a refusal line to be written before it
/// disconnects anyway. The line is a courtesy to the module's author; a module
/// that does not read must not hold the kernel's side open.
const REFUSAL_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// Why an endpoint could not be set up or used.
#[derive(Debug)]
pub enum EndpointError {
    /// The directory is not this user's alone: not a real directory, owned by
    /// someone else, or writable by group or others.
    UnsafeDirectory { path: PathBuf, why: String },
    /// A socket path would be longer than [`MAX_SOCKET_PATH`].
    PathTooLong(PathBuf),
    /// No connection arrived before the deadline.
    Timeout,
    /// The process that connected runs as another user.
    ForeignPeer { uid: u32 },
    /// The OS refused.
    Io(std::io::Error),
}

impl std::fmt::Display for EndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsafeDirectory { path, why } => {
                write!(f, "{} cannot hold callback sockets: {why}", path.display())
            }
            Self::PathTooLong(p) => write!(
                f,
                "the callback socket path {} is longer than {MAX_SOCKET_PATH} bytes",
                p.display()
            ),
            Self::Timeout => f.write_str("no callback connection before the deadline"),
            Self::ForeignPeer { uid } => {
                write!(
                    f,
                    "the callback connection came from uid {uid}, not this user"
                )
            }
            Self::Io(e) => write!(f, "callback endpoint: {e}"),
        }
    }
}

impl std::error::Error for EndpointError {}

impl From<std::io::Error> for EndpointError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// This daemon's directory of callback sockets: `<state>/run/<daemon pid>/`,
/// both levels `0700` and this user's.
///
/// Per daemon process, so two daemons on one state directory cannot remove each
/// other's sockets; and made afresh — a directory left by a crashed daemon that
/// happened to have the same pid is ours (checked) and emptied.
#[derive(Debug)]
pub struct CallbackDir {
    path: PathBuf,
}

impl CallbackDir {
    /// Create (or take over) `<state>/run/<this pid>/`.
    ///
    /// # Errors
    ///
    /// [`EndpointError::UnsafeDirectory`] if `run/` or the pid directory is not
    /// this user's alone; [`EndpointError::Io`] if it cannot be made.
    pub fn create(state: &Path) -> Result<Self, EndpointError> {
        let run = state.join("run");
        private_dir(&run)?;
        let path = run.join(std::process::id().to_string());
        if std::fs::symlink_metadata(&path).is_ok() {
            // Checked before it is emptied: only a directory that is already
            // this user's alone is ours to clear.
            check_private(&path)?;
            std::fs::remove_dir_all(&path)?;
        }
        private_dir(&path)?;
        Ok(Self { path })
    }

    /// The directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Listen for generation `n`'s callback connection at `<dir>/<n>.sock`.
    /// Must be called within a Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`EndpointError::PathTooLong`], or the OS refusing to bind.
    pub fn listen(&self, n: u64) -> Result<CallbackListener, EndpointError> {
        let path = self.path.join(format!("{n}.sock"));
        if path.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(EndpointError::PathTooLong(path));
        }
        // A socket file left by an earlier generation with the same number
        // (after a failed bind, say) would make the bind fail. The directory is
        // ours alone, so what is there is ours to remove.
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        let listener = tokio::net::UnixListener::bind(&path)?;
        Ok(CallbackListener { listener, path })
    }
}

/// Create `path` (and parents) `0700` if it is missing; then check it.
fn private_dir(path: &Path) -> Result<(), EndpointError> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
    {
        Ok(()) => check_private(path),
        Err(e) => Err(e.into()),
    }
}

/// A real directory, this user's, not writable by group or others.
fn check_private(path: &Path) -> Result<(), EndpointError> {
    use std::os::unix::fs::MetadataExt;
    let unsafe_dir = |why: String| EndpointError::UnsafeDirectory {
        path: path.to_owned(),
        why,
    };
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() {
        return Err(unsafe_dir(
            "not a directory (or a symlink to one)".to_owned(),
        ));
    }
    let me = rustix::process::geteuid().as_raw();
    if meta.uid() != me {
        return Err(unsafe_dir(format!("owned by uid {}, not {me}", meta.uid())));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(unsafe_dir(format!(
            "mode {:04o} lets others create or remove sockets in it",
            meta.mode() & 0o777
        )));
    }
    Ok(())
}

/// One generation's listening socket. Its path is removed when it is dropped —
/// after [`CallbackListener::accept_one`], or unused.
#[derive(Debug)]
pub struct CallbackListener {
    listener: tokio::net::UnixListener,
    path: PathBuf,
}

impl CallbackListener {
    /// The path to hand to the child in `A24_CALLBACK_SOCK`.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Take the first connection that arrives before `deadline`, then stop
    /// listening: the listener is closed and its path removed before this
    /// returns, whatever it returns, so a second connection is refused.
    ///
    /// # Errors
    ///
    /// [`EndpointError::Timeout`]; [`EndpointError::ForeignPeer`] if the
    /// process that connected runs as another user (the directory is `0700`,
    /// so this is a second line, not the first); or the OS failing.
    pub async fn accept_one(
        self,
        deadline: tokio::time::Instant,
    ) -> Result<UnixStream, EndpointError> {
        let accepted = tokio::time::timeout_at(deadline, self.listener.accept()).await;
        drop(self); // close and unlink before anything else can connect
        let (stream, _) = accepted.map_err(|_| EndpointError::Timeout)??;
        let uid = stream.peer_cred()?.uid();
        if uid != rustix::process::geteuid().as_raw() {
            return Err(EndpointError::ForeignPeer { uid });
        }
        Ok(stream)
    }
}

impl Drop for CallbackListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A connection whose handshake succeeded: the rest of the stream (the reader
/// keeps whatever it buffered past the first line), and what was agreed.
#[derive(Debug)]
pub struct Handshaken {
    pub reader: BufReader<OwnedReadHalf>,
    pub writer: OwnedWriteHalf,
    pub accepted: Accepted,
}

/// Why a handshake did not succeed. Every one of these ends the connection.
#[derive(Debug)]
pub enum HandshakeFailed {
    /// No complete first frame before the deadline.
    Timeout,
    /// The first frame could not be read: the peer closed, the line was too
    /// long (it is not answered — it cannot be parsed), or reading failed.
    Frame(FrameError),
    /// The first frame was read and refused; the refusal was written (best
    /// effort) before disconnecting.
    Refused(HandshakeError),
    /// The success line could not be written.
    Write(std::io::Error),
}

impl std::fmt::Display for HandshakeFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => f.write_str("no handshake before the startup deadline"),
            Self::Frame(e) => write!(f, "the handshake could not be read: {e:?}"),
            Self::Refused(e) => write!(f, "the handshake was refused: {e}"),
            Self::Write(e) => write!(f, "the handshake could not be answered: {e}"),
        }
    }
}

impl std::error::Error for HandshakeFailed {}

/// Run the handshake on a fresh callback connection: read the first frame
/// before `deadline`, check it against `expect`, and answer — the result on
/// success, or the refusal and then a disconnect (SPEC §3: *"握手期（首帧）任何
/// 协议或语义失败一律断连"*).
///
/// Ready (`Generation::ready`) is the CALLER's step, taken after this returns
/// `Ok`: that is after the success line was written, so a module that never
/// received the agreed version is not counted ready.
///
/// # Errors
///
/// [`HandshakeFailed`]; the connection is closed by the time it is returned.
pub async fn handshake(
    stream: UnixStream,
    expect: &Expectation,
    deadline: tokio::time::Instant,
) -> Result<Handshaken, HandshakeFailed> {
    let (read, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read);
    let frame =
        match tokio::time::timeout_at(deadline, crate::rpc::read_frame_async(&mut reader)).await {
            Err(_) => return Err(HandshakeFailed::Timeout),
            Ok(Err(e)) => return Err(HandshakeFailed::Frame(e)),
            Ok(Ok(frame)) => frame,
        };
    match initialize::accept(&frame, expect) {
        Ok(accepted) => {
            let line = initialize::success_line(&accepted);
            let write = async {
                writer.write_all(&line).await?;
                writer.flush().await
            };
            match tokio::time::timeout(crate::rpc::WRITE_TIMEOUT, write).await {
                Ok(Ok(())) => Ok(Handshaken {
                    reader,
                    writer,
                    accepted,
                }),
                Ok(Err(e)) => Err(HandshakeFailed::Write(e)),
                Err(_) => Err(HandshakeFailed::Write(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the handshake answer was not taken",
                ))),
            }
        }
        Err(refusal) => {
            let line = initialize::error_line(initialize::id_of(&frame).as_deref(), &refusal);
            let _ = tokio::time::timeout(REFUSAL_WRITE_TIMEOUT, async {
                writer.write_all(&line).await?;
                writer.flush().await
            })
            .await;
            Err(HandshakeFailed::Refused(refusal))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::initialize::Offer;
    use crate::version::VersionRange;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};

    fn expectation() -> Expectation {
        Expectation {
            module: "cos72".to_owned(),
            manifest_digest: "sha256:abc".to_owned(),
            auth_token: "s3cret".to_owned(),
            kernel_versions: VersionRange::new(1, 1).unwrap(),
            offer: Offer::none(),
        }
    }

    fn initialize_frame(id: &str, token: &str) -> String {
        format!(
            r#"{{"jsonrpc":"2.0","method":"initialize","id":"{id}","params":{{"protocol_versions":{{"min":1,"max":1}},"module":"cos72","manifest_digest":"sha256:abc","auth_token":"{token}","capabilities":[]}}}}"#
        ) + "\n"
    }

    /// A state directory with a short path: a socket address holds ~104 bytes,
    /// and a temp directory on macOS alone takes about half of that.
    fn state() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("a24")
            .tempdir_in("/tmp")
            .unwrap()
    }

    fn soon() -> tokio::time::Instant {
        tokio::time::Instant::now() + Duration::from_secs(30)
    }

    /// Connect as the module, send `frame`, and read the kernel's answer line
    /// and then whether the connection was closed after it.
    async fn module_says(path: PathBuf, frame: String) -> (String, bool) {
        let mut conn = UnixStream::connect(&path).await.expect("connect");
        conn.write_all(frame.as_bytes()).await.unwrap();
        let mut reader = BufReader::new(conn);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let mut rest = Vec::new();
        let closed = tokio::time::timeout(Duration::from_secs(5), reader.read_to_end(&mut rest))
            .await
            .is_ok_and(|r| r.is_ok());
        (line, closed)
    }

    /// The whole happy path, and its control for every refusal below: the
    /// kernel answers under the module's id with the agreed version, and the
    /// connection stays open for what follows.
    #[tokio::test]
    async fn a_correct_handshake_is_answered_and_the_connection_stays_open() {
        let s = state();
        let dir = CallbackDir::create(s.path()).unwrap();
        let listener = dir.listen(1).unwrap();
        let path = listener.path().to_owned();
        let module = tokio::spawn(async move {
            let mut conn = UnixStream::connect(&path).await.unwrap();
            conn.write_all(initialize_frame("hello", "s3cret").as_bytes())
                .await
                .unwrap();
            let mut reader = BufReader::new(conn);
            let mut line = String::new();
            // Bounded: without an answer this would wait for as long as the
            // kernel holds the connection — a hang, which a test run reports as
            // silence (the mutation that dropped the answer showed exactly that).
            tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line))
                .await
                .expect("no answer to the handshake within 10s")
                .unwrap();
            // Still open: nothing more arrives within a short wait.
            let mut more = [0u8; 1];
            let open = tokio::time::timeout(Duration::from_millis(300), reader.read(&mut more))
                .await
                .is_err();
            (line, open)
        });
        let stream = listener.accept_one(soon()).await.unwrap();
        let done = handshake(stream, &expectation(), soon())
            .await
            .expect("handshake");
        let (line, open) = module.await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], "hello");
        assert_eq!(v["result"]["protocol_version"], 1);
        assert_eq!(done.accepted.id, "hello");
        assert!(
            open,
            "the kernel closed a connection whose handshake succeeded"
        );
        drop(done);
    }

    /// A wrong token: refused with `-32000` + `auth_failed`, under the id the
    /// module sent, and then disconnected.
    #[tokio::test]
    async fn a_wrong_token_is_refused_by_kind_and_disconnected() {
        let s = state();
        let dir = CallbackDir::create(s.path()).unwrap();
        let listener = dir.listen(1).unwrap();
        let module = tokio::spawn(module_says(
            listener.path().to_owned(),
            initialize_frame("x", "wrong"),
        ));
        let stream = listener.accept_one(soon()).await.unwrap();
        let err = handshake(stream, &expectation(), soon())
            .await
            .expect_err("wrong token");
        assert!(
            matches!(err, HandshakeFailed::Refused(HandshakeError::AuthFailed)),
            "{err}"
        );
        let (line, closed) = module.await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            (v["id"].as_str(), v["error"]["code"].as_i64()),
            (Some("x"), Some(-32000))
        );
        assert_eq!(v["error"]["data"]["kind"], "auth_failed");
        assert!(
            closed,
            "the connection stayed open after a refused handshake"
        );
    }

    /// A first frame that is not `initialize`: `-32600`, then disconnected.
    #[tokio::test]
    async fn a_first_frame_that_is_not_initialize_is_refused_and_disconnected() {
        let s = state();
        let dir = CallbackDir::create(s.path()).unwrap();
        let listener = dir.listen(1).unwrap();
        let module = tokio::spawn(module_says(
            listener.path().to_owned(),
            r#"{"jsonrpc":"2.0","method":"ping","id":"p","params":{}}"#.to_owned() + "\n",
        ));
        let stream = listener.accept_one(soon()).await.unwrap();
        let err = handshake(stream, &expectation(), soon())
            .await
            .expect_err("not initialize");
        assert!(
            matches!(
                err,
                HandshakeFailed::Refused(HandshakeError::NotInitialize(_))
            ),
            "{err}"
        );
        let (line, closed) = module.await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["error"]["code"], -32600);
        assert!(closed);
    }

    /// Silence: no frame before the deadline is a timeout, not a hang.
    #[tokio::test]
    async fn no_first_frame_before_the_deadline_is_a_timeout() {
        let s = state();
        let dir = CallbackDir::create(s.path()).unwrap();
        let listener = dir.listen(1).unwrap();
        let path = listener.path().to_owned();
        let _module = tokio::spawn(async move {
            let _conn = UnixStream::connect(&path).await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let stream = listener.accept_one(soon()).await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
        let err = handshake(stream, &expectation(), deadline)
            .await
            .expect_err("silence");
        assert!(matches!(err, HandshakeFailed::Timeout), "{err}");
    }

    /// One connection per generation: once the first is taken, a second
    /// `connect` is refused — the listener is closed and its path gone. The
    /// first connecting is the control.
    #[tokio::test]
    async fn after_the_first_connection_a_second_is_refused() {
        let s = state();
        let dir = CallbackDir::create(s.path()).unwrap();
        let listener = dir.listen(1).unwrap();
        let path = listener.path().to_owned();
        let first = tokio::spawn({
            let path = path.clone();
            async move { UnixStream::connect(&path).await }
        });
        let _taken = listener
            .accept_one(soon())
            .await
            .expect("the first connection");
        assert!(
            first.await.unwrap().is_ok(),
            "control: the first connect succeeded"
        );
        assert!(!path.exists(), "the socket path is still there");
        assert!(
            UnixStream::connect(&path).await.is_err(),
            "a second connection was accepted"
        );
    }

    /// Nobody connecting: `accept_one` gives up at the deadline, and the path
    /// is removed then too.
    #[tokio::test]
    async fn no_connection_before_the_deadline_is_a_timeout_and_leaves_nothing() {
        let s = state();
        let dir = CallbackDir::create(s.path()).unwrap();
        let listener = dir.listen(7).unwrap();
        let path = listener.path().to_owned();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
        let err = listener.accept_one(deadline).await.expect_err("nobody");
        assert!(matches!(err, EndpointError::Timeout), "{err}");
        assert!(!path.exists());
    }

    /// The socket directory is this user's alone: a `run/` others can write is
    /// refused, not used; a fresh one is created `0700` (the control).
    #[test]
    fn a_socket_directory_others_can_write_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let s = state();
        let dir = CallbackDir::create(s.path()).expect("control: a fresh directory");
        let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);

        let s2 = state();
        std::fs::create_dir(s2.path().join("run")).unwrap();
        std::fs::set_permissions(
            s2.path().join("run"),
            std::fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        let err = CallbackDir::create(s2.path()).expect_err("a shared run/");
        assert!(
            matches!(err, EndpointError::UnsafeDirectory { .. }),
            "{err}"
        );
    }

    /// A socket path longer than an address holds is refused before the bind
    /// — which would otherwise fail, or on some systems truncate.
    #[tokio::test]
    async fn a_socket_path_too_long_for_an_address_is_refused() {
        let s = state();
        let deep = s.path().join("x".repeat(100));
        let dir = CallbackDir::create(&deep).unwrap();
        let err = dir.listen(1).expect_err("too long");
        assert!(matches!(err, EndpointError::PathTooLong(_)), "{err}");
        // Control: a short state directory works.
        CallbackDir::create(s.path())
            .unwrap()
            .listen(1)
            .expect("short enough");
    }

    /// A version mismatch says both ranges (SPEC §8).
    #[test]
    fn a_version_refusal_carries_both_ranges() {
        let err = HandshakeError::VersionMismatch(crate::version::VersionMismatch::NoOverlap {
            module: VersionRange::new(3, 4).unwrap(),
            kernel: VersionRange::new(1, 1).unwrap(),
        });
        let v: serde_json::Value =
            serde_json::from_slice(&initialize::error_line(Some("v"), &err)).unwrap();
        assert_eq!(v["error"]["data"]["kind"], "version_mismatch");
        assert_eq!(
            v["error"]["data"]["module"],
            serde_json::json!({"min": 3, "max": 4})
        );
        assert_eq!(
            v["error"]["data"]["kernel"],
            serde_json::json!({"min": 1, "max": 1})
        );
    }
}
