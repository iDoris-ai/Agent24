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
//! listener is closed and its path removed at once. A later `connect` finds
//! nothing to connect to; one that raced the first into the listen backlog
//! DID connect, but is never served — closing the listener ends it (EOF or a
//! reset). What holds is "exactly one connection is served", not "a second
//! `connect()` fails" — the latter no pathname socket can promise (review of
//! ME3-SUP slice 2, round 1). That is how "the callback connection IS the
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
    /// This process already took over this socket directory: a second
    /// `create` would empty it under the listeners the first one made.
    AlreadyCreated(PathBuf),
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
            Self::AlreadyCreated(p) => write!(
                f,
                "{} is still taken over by this process; drop that directory and its \
                 listeners first",
                p.display()
            ),
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
    /// The last generation number handed out. Numbers come from here and
    /// nowhere else, so a number — and with it a socket path — is never used
    /// twice by this directory.
    last: std::sync::atomic::AtomicU64,
    /// This directory's entry in [`TAKEN_OVER`], shared with every listener
    /// made from it.
    claim: std::sync::Arc<Claim>,
}

/// The socket directories this process has taken over. A second `create` for
/// one of them would `remove_dir_all` it under listeners the first already
/// made (PR-Daemon review of #179, L2).
static TAKEN_OVER: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// A directory's entry in [`TAKEN_OVER`], held by its [`CallbackDir`] and by
/// every [`CallbackListener`] made from it, and removed when the last of
/// them goes: the directory can then be taken over again in this process,
/// and not while anything that emptying it would break is still alive
/// (FU-59).
#[derive(Debug)]
struct Claim(PathBuf);

impl Drop for Claim {
    fn drop(&mut self) {
        TAKEN_OVER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|p| p != &self.0);
    }
}

impl CallbackDir {
    /// Create (or take over) `<state>/run/<this pid>/`.
    ///
    /// The steps go by path, not by a held directory handle, so they rest on
    /// `state` itself being stable — the daemon's own directory, which another
    /// user cannot rename or replace. Within that, `run/` and the pid directory
    /// are checked here (review of ME3-SUP slice 2, round 1, F8).
    ///
    /// Once at a time per state directory per process: the pid directory is
    /// emptied here, so a second call while the first directory — or any
    /// listener made from it — still lives would take the sockets of
    /// generations already listening away. Once all of them are dropped, it
    /// can be created again (FU-59).
    ///
    /// # Errors
    ///
    /// [`EndpointError::UnsafeDirectory`] if `run/` or the pid directory is not
    /// this user's alone; [`EndpointError::AlreadyCreated`] while a
    /// directory for the same path, or a listener made from one, still
    /// lives; [`EndpointError::Io`] if it cannot be made.
    pub fn create(state: &Path) -> Result<Self, EndpointError> {
        let run = state.join("run");
        private_dir(&run)?;
        let path = run.canonicalize()?.join(std::process::id().to_string());
        {
            let mut taken = TAKEN_OVER
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if taken.contains(&path) {
                return Err(EndpointError::AlreadyCreated(path));
            }
            taken.push(path.clone());
        }
        let claim = std::sync::Arc::new(Claim(path.clone()));
        // Held only by a take-over that succeeds: one that fails made no
        // listener, so nothing it could empty is in use, and a retry once the
        // cause is fixed must not be refused (review of ME3-SUP slice 3a) —
        // the claim is dropped with the error.
        Self::take_over(&path)?;
        Ok(Self {
            path,
            last: std::sync::atomic::AtomicU64::new(0),
            claim,
        })
    }

    /// Empty a stale pid directory of ours, then (re)make it exactly `0700`.
    fn take_over(path: &Path) -> Result<(), EndpointError> {
        if std::fs::symlink_metadata(path).is_ok() {
            // Normalised and checked before it is emptied — the same rule as
            // `run/` (the first version refused an ours-but-0755 pid directory
            // that `run/` would have normalised; review of ME3-SUP slice 2,
            // round 2): only a directory that is this user's is ours to clear.
            private_dir(path)?;
            std::fs::remove_dir_all(path)?;
        }
        private_dir(path)
    }

    /// The directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Listen for the next generation's callback connection, at
    /// `<dir>/<n>.sock` with a number this directory has never handed out.
    /// Must be called within a Tokio runtime.
    ///
    /// The number is chosen here, not by the caller: taking one as a parameter
    /// let the same number be listened on again after its first connection had
    /// been served, and the second connection was served too — "one
    /// connection per generation" then held only while callers remembered not
    /// to reuse numbers (PR-Daemon review of #179, L1). Now it holds per
    /// directory by construction.
    ///
    /// # Errors
    ///
    /// [`EndpointError::PathTooLong`], or the OS refusing to bind.
    pub fn listen_next(&self) -> Result<CallbackListener, EndpointError> {
        let n = self.last.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        self.listen_at(n)
    }

    /// Listen at `<dir>/<n>.sock`. Private: a caller-chosen number is what
    /// [`CallbackDir::listen_next`] exists to rule out.
    fn listen_at(&self, n: u64) -> Result<CallbackListener, EndpointError> {
        let path = self.path.join(format!("{n}.sock"));
        if path.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(EndpointError::PathTooLong(path));
        }
        // Nothing is removed first. The directory was emptied when this
        // process took it over and numbers are never handed out twice, so a node at this
        // path is not a stale file but someone's live socket — removing it (as
        // the first version did) took a working listener's address away
        // (review of ME3-SUP slice 2, round 1, F5). An address in use fails.
        use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
        let listener = tokio::net::UnixListener::bind(&path)?;
        // Record the node at once and hand it to a `CallbackListener`, so any
        // failure below drops it and removes the path — a socket left behind
        // would make the next bind of this name fail for good (review of
        // ME3-SUP slice 2, round 2).
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                return Err(e.into());
            }
        };
        let bound = CallbackListener {
            listener,
            node: (meta.dev(), meta.ino()),
            path,
            _claim: self.claim.clone(),
        };
        if !meta.file_type().is_socket() {
            return Err(EndpointError::UnsafeDirectory {
                path: bound.path.clone(),
                why: "the bound path is not a socket".to_owned(),
            });
        }
        // The socket node itself `0700` too, not the umask's `0755`. By path:
        // this rests, like everything here, on nobody else of this user
        // swapping entries in a `0700` directory under us (SPEC §0 does not
        // defend against a hostile process of the same user).
        std::fs::set_permissions(&bound.path, std::fs::Permissions::from_mode(0o700))?;
        Ok(bound)
    }
}

/// Create `path` (and parents) `0700` if it is missing; set it to exactly
/// `0700` if it is a real directory of ours with any other mode (looser, or
/// stricter — a `0600` directory gains the owner's search bit); then check it.
fn private_dir(path: &Path) -> Result<(), EndpointError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir()
        && meta.uid() == rustix::process::geteuid().as_raw()
        && meta.mode() & 0o7777 != 0o700
    {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    check_private(path)
}

/// Remove the socket directories of daemons that are gone (FU-56): `run/<pid>/`
/// for every pid that no longer exists, when it is this user's real directory.
/// A live pid's directory is kept — it may be another daemon's — and so is one
/// whose pid cannot be asked about (`EPERM`: not this user's process). Nothing
/// is followed through a symlink, and `run/` itself must pass the same check as
/// when it is created, or nothing is touched. Returns what was removed. Called
/// once at daemon start, before [`CallbackDir::create`].
///
/// **Assumes one daemon per state directory at a time** (agent24d holds a
/// singleton lock for its state directory, and an ephemeral daemon has a root
/// of its own). Between "that pid is gone" and the removal, a reused pid's new
/// daemon could take the directory over in the same state directory only if
/// two daemons shared it — which that lock rules out. Without it, this would
/// need a per-directory lock (review of SUP-4, round 1).
pub fn remove_stale(state: &Path) -> Vec<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let run = state.join("run");
    if check_private(&run).is_err() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&run) else {
        return Vec::new();
    };
    let me = rustix::process::geteuid().as_raw();
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        if u32::try_from(pid).is_ok_and(|p| p == std::process::id()) {
            continue;
        }
        let Some(pid) = rustix::process::Pid::from_raw(pid) else {
            continue;
        };
        if rustix::process::test_kill_process(pid) != Err(rustix::io::Errno::SRCH) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() && meta.uid() == me && std::fs::remove_dir_all(&path).is_ok() {
            removed.push(path);
        }
    }
    removed
}

/// A real directory, this user's, mode exactly `0700` (SPEC §1: the kernel
/// listens on a path it chose, `0700`). The first version refused only group-
/// or other-WRITABLE, which let `0755` through (review of ME3-SUP slice 2,
/// round 1, F6).
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
    // All twelve mode bits: "exactly 0700" admits no sticky, setuid or setgid
    // bit either (review of ME3-SUP slice 2, round 3).
    if meta.mode() & 0o7777 != 0o700 {
        return Err(unsafe_dir(format!(
            "mode {:04o}, not 0700",
            meta.mode() & 0o7777
        )));
    }
    Ok(())
}

/// One generation's listening socket. When it is dropped — after
/// [`CallbackListener::accept_one`], or unused — its path is removed if it
/// still names this listener's node (see `node`).
#[derive(Debug)]
pub struct CallbackListener {
    listener: tokio::net::UnixListener,
    path: PathBuf,
    /// The socket node this listener bound (device, inode): `Drop` removes the
    /// path only if it still names this node when looked at. Best effort, not
    /// atomic — the look and the removal are two calls, and an inode number
    /// can be reused — so it guards against the ordinary case (a later
    /// listener bound at the same name), not against someone of this user
    /// replacing entries in the directory concurrently (review of ME3-SUP
    /// slice 2, round 2).
    node: (u64, u64),
    /// Its directory's take-over, kept while this listener lives. Dropped
    /// after `Drop` has removed the socket (fields drop after the body).
    _claim: std::sync::Arc<Claim>,
}

impl CallbackListener {
    /// The path to hand to the child in `A24_CALLBACK_SOCK`.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Take the first connection that arrives before `deadline`, then stop
    /// listening: the listener is closed and its path removed before this
    /// returns, whatever it returns. Exactly one connection is served: a later
    /// `connect` finds nothing, and one that raced into the backlog is ended
    /// unserved when the listener closes.
    ///
    /// **Pass [`handshake`] the same `deadline`.** A connection can be
    /// accepted just after it (`timeout_at` polls the accept first); that is
    /// harmless only because the handshake that follows is bounded by the same
    /// instant and fails at once.
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
        use std::os::unix::fs::MetadataExt;
        // Only our own node: if the path now names something else, it is not
        // ours to remove.
        if std::fs::symlink_metadata(&self.path).is_ok_and(|m| (m.dev(), m.ino()) == self.node) {
            let _ = std::fs::remove_file(&self.path);
        }
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

/// Write the success line within `min(deadline, now + WRITE_TIMEOUT)`, and
/// call it written only if the clock still says the deadline has not passed.
///
/// The biased select puts the timer first, but it can only prefer a timer that
/// is ready WHEN it is polled: a thread preempted between polling the timer
/// (not yet due) and polling the write (now done) would come back past the
/// deadline with a finished write, and report success. So the clock is read
/// again once the write is done (review of ME3-SUP slice 2, round 3).
async fn answer<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    line: &[u8],
    deadline: tokio::time::Instant,
) -> Result<(), HandshakeFailed> {
    answer_within(writer, line, deadline, crate::rpc::WRITE_TIMEOUT).await
}

/// [`answer`] with the write's own bound as a parameter, so a test can make it
/// shorter than the deadline.
async fn answer_within<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    line: &[u8],
    deadline: tokio::time::Instant,
    write_timeout: Duration,
) -> Result<(), HandshakeFailed> {
    let until = deadline.min(tokio::time::Instant::now() + write_timeout);
    let timed_out = || {
        if until >= deadline {
            HandshakeFailed::Timeout
        } else {
            HandshakeFailed::Write(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the handshake answer was not taken",
            ))
        }
    };
    let written = tokio::select! {
        biased;
        () = tokio::time::sleep_until(until) => return Err(timed_out()),
        written = async {
            writer.write_all(line).await?;
            writer.flush().await
        } => written,
    };
    written.map_err(HandshakeFailed::Write)?;
    // Against `until`, the earlier of the two bounds — not the deadline alone:
    // the write's own bound has the same preemption hole (review of ME3-SUP
    // slice 2, round 3′).
    if tokio::time::Instant::now() >= until {
        return Err(timed_out());
    }
    Ok(())
}

/// Run the handshake on a fresh callback connection: read the first frame
/// before `deadline`, check it against `expect`, and answer — the result on
/// success, or the refusal and then a disconnect (SPEC §3: *"握手期（首帧）任何
/// 协议或语义失败一律断连"*).
///
/// A module can receive a complete success line and still be counted as having
/// missed its startup deadline — when the answer finished past it — and see
/// the connection close. That is the kernel failing closed at the boundary,
/// not a protocol error (D1: the connection ending ends the generation).
///
/// Ready (`Generation::ready`) is the CALLER's step, taken after this returns
/// `Ok`: that is after the success line was written, so a module that never
/// received the agreed version is not counted ready.
///
/// `deadline` bounds the WHOLE handshake — reading the first frame and
/// writing the answer — so "no successful `initialize` within the startup
/// timeout counts as a crash" holds by this function's own bound (the first
/// version let the answer take another ten seconds after it; review of
/// ME3-SUP slice 2, round 1, F2).
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
    // `timeout_at` polls the wrapped future before its timer, so a frame that
    // was already buffered would win over a deadline that had already passed
    // (a starved task resuming late). The deadline goes FIRST in a biased
    // select instead, here and for the answer (review of ME3-SUP slice 2,
    // round 2).
    let frame = tokio::select! {
        biased;
        () = tokio::time::sleep_until(deadline) => return Err(HandshakeFailed::Timeout),
        read = crate::rpc::read_frame_async(&mut reader) => read.map_err(HandshakeFailed::Frame)?,
    };
    let verdict = initialize::accept(&frame, expect);
    // Checking up to a megabyte is synchronous work; it must not carry a
    // handshake past its deadline into success.
    if tokio::time::Instant::now() >= deadline {
        return Err(HandshakeFailed::Timeout);
    }
    match verdict {
        Ok(accepted) => {
            let line = initialize::success_line(&accepted);
            answer(&mut writer, &line, deadline).await?;
            Ok(Handshaken {
                reader,
                writer,
                accepted,
            })
        }
        Err(refusal) => {
            let line = initialize::error_line(initialize::id_of(&frame).as_deref(), &refusal);
            let until = deadline.min(tokio::time::Instant::now() + REFUSAL_WRITE_TIMEOUT);
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(until) => {}
                _ = async {
                    writer.write_all(&line).await?;
                    writer.flush().await
                } => {}
            }
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
        let listener = dir.listen_next().unwrap();
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
        let listener = dir.listen_next().unwrap();
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
        let listener = dir.listen_next().unwrap();
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
        let listener = dir.listen_next().unwrap();
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

    /// One connection per generation: once the first is taken, a later
    /// `connect` is refused — the listener is closed and its path gone. The
    /// first connecting is the control.
    #[tokio::test]
    async fn after_the_first_connection_a_second_is_refused() {
        let s = state();
        let dir = CallbackDir::create(s.path()).unwrap();
        let listener = dir.listen_next().unwrap();
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
        let listener = dir.listen_next().unwrap();
        let path = listener.path().to_owned();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
        let err = listener.accept_one(deadline).await.expect_err("nobody");
        assert!(matches!(err, EndpointError::Timeout), "{err}");
        assert!(!path.exists());
    }

    /// The socket directory must be a real directory: a `run/` that is a
    /// symlink — to anywhere — is refused, not followed (a looser directory
    /// that IS ours is tightened instead: see
    /// `the_socket_and_its_directories_are_exactly_0700`). A fresh one is the
    /// control.
    #[test]
    fn a_socket_directory_that_is_a_symlink_is_refused() {
        let s = state();
        CallbackDir::create(s.path()).expect("control: a fresh directory");

        let s2 = state();
        let elsewhere = s2.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, s2.path().join("run")).unwrap();
        let err = CallbackDir::create(s2.path()).expect_err("a symlinked run/");
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
        let err = dir.listen_next().expect_err("too long");
        assert!(matches!(err, EndpointError::PathTooLong(_)), "{err}");
        // Control: a short state directory works.
        CallbackDir::create(s.path())
            .unwrap()
            .listen_next()
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

    /// Two clients that both connect before the kernel accepts: exactly one is
    /// served; the other did connect (it sat in the backlog) but is never
    /// served — closing the listener ends it. The literal claim is "one is
    /// served", not "the second connect fails" (review of ME3-SUP slice 2,
    /// round 1, F7).
    #[tokio::test]
    async fn of_two_early_connections_exactly_one_is_served() {
        let s = state();
        let dir = CallbackDir::create(s.path()).unwrap();
        let listener = dir.listen_next().unwrap();
        let path = listener.path().to_owned();
        let a = UnixStream::connect(&path).await.expect("first connect");
        let b = UnixStream::connect(&path)
            .await
            .expect("second connect, into the backlog");
        let served = listener.accept_one(soon()).await.expect("one accepted");
        // Whichever was not accepted sees its connection end, without a byte.
        let mut ends = 0;
        for mut c in [a, b] {
            let mut buf = [0u8; 1];
            match tokio::time::timeout(Duration::from_millis(500), c.read(&mut buf)).await {
                Ok(Ok(0) | Err(_)) => ends += 1, // EOF or reset: never served
                Ok(Ok(_)) => panic!("a byte from a connection nobody wrote to"),
                Err(_) => {} // still open: the one being served
            }
        }
        assert_eq!(
            ends, 1,
            "exactly one connection should have been ended unserved"
        );
        drop(served);
    }

    /// The socket node and both directories are exactly `0700`; an existing
    /// `run/` that is ours but looser is tightened, not refused or left.
    #[tokio::test]
    async fn the_socket_and_its_directories_are_exactly_0700() {
        use std::os::unix::fs::PermissionsExt;
        let s = state();
        std::fs::create_dir(s.path().join("run")).unwrap();
        std::fs::set_permissions(s.path().join("run"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let dir = CallbackDir::create(s.path()).expect("ours, so tightened");
        let listener = dir.listen_next().unwrap();
        for p in [
            s.path().join("run"),
            dir.path().to_owned(),
            listener.path().to_owned(),
        ] {
            let mode = std::fs::symlink_metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{} is {mode:o}", p.display());
        }
    }

    /// A listener removes its path only while the path is still its own
    /// socket: a second listener bound at the same name after the first's
    /// path was taken away keeps its address when the first is dropped.
    #[tokio::test]
    async fn a_dropped_listener_does_not_remove_someone_elses_socket() {
        let s = state();
        let dir = CallbackDir::create(s.path()).unwrap();
        let first = dir.listen_at(1).unwrap();
        let path = first.path().to_owned();
        // The same name, bound again: refused while the first holds it.
        assert!(
            dir.listen_at(1).is_err(),
            "an address in use was taken over"
        );
        std::fs::remove_file(&path).unwrap();
        let second = dir.listen_at(1).expect("the name is free again");
        drop(first);
        assert!(
            path.exists(),
            "dropping the first listener removed the second's socket"
        );
        drop(second);
        assert!(!path.exists(), "control: the second removes its own");
    }

    /// A deadline that has already passed wins even when the first frame is
    /// sitting in the buffer and the answer would be written at once: no
    /// success after the deadline. (`timeout_at` polls its future first, and
    /// returned success here; review of ME3-SUP slice 2, round 2.)
    #[tokio::test]
    async fn a_passed_deadline_wins_over_a_frame_already_waiting() {
        let s = state();
        let dir = CallbackDir::create(s.path()).unwrap();
        let listener = dir.listen_next().unwrap();
        let mut module = UnixStream::connect(listener.path()).await.unwrap();
        module
            .write_all(initialize_frame("late", "s3cret").as_bytes())
            .await
            .unwrap();
        let stream = listener.accept_one(soon()).await.unwrap();
        // Give the frame time to land in the kernel's buffer.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let passed = tokio::time::Instant::now();
        tokio::time::sleep(Duration::from_millis(5)).await;
        let err = handshake(stream, &expectation(), passed)
            .await
            .expect_err("a handshake past its deadline succeeded");
        assert!(matches!(err, HandshakeFailed::Timeout), "{err}");
    }

    /// A stale pid directory of ours — left, say, by a crashed daemon that had
    /// this pid — is normalised and emptied like `run/`, not refused.
    #[tokio::test]
    async fn a_stale_pid_directory_of_ours_is_taken_over() {
        use std::os::unix::fs::PermissionsExt;
        let s = state();
        let stale = s.path().join("run").join(std::process::id().to_string());
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("leftover.sock"), b"").unwrap();
        std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o755)).unwrap();
        let dir = CallbackDir::create(s.path()).expect("ours, so taken over");
        assert!(
            !stale.join("leftover.sock").exists(),
            "the stale directory was not emptied"
        );
        let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    /// A writer whose write completes only after the deadline has passed —
    /// by blocking the thread in `poll_write`, which is what a preempted
    /// thread looks like to the select: the timer was polled (not yet due),
    /// then the write, which is ready by the time it returns.
    struct LateWriter {
        until: std::time::Instant,
    }
    impl tokio::io::AsyncWrite for LateWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            if let Some(left) = self.until.checked_duration_since(std::time::Instant::now()) {
                std::thread::sleep(left);
            }
            std::task::Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// An answer whose write finishes after the deadline is a timeout, not a
    /// success: the select saw the timer not yet due, and the write done only
    /// once the deadline had gone (review of ME3-SUP slice 2, round 3). The
    /// same writer finishing in time is the control.
    #[tokio::test]
    async fn an_answer_finished_past_the_deadline_is_not_a_success() {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(50);
        let mut late = LateWriter {
            until: std::time::Instant::now() + Duration::from_millis(100),
        };
        let err = answer(&mut late, b"x\n", deadline)
            .await
            .expect_err("past the deadline");
        assert!(matches!(err, HandshakeFailed::Timeout), "{err}");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut prompt = LateWriter {
            until: std::time::Instant::now(),
        };
        answer(&mut prompt, b"x\n", deadline)
            .await
            .expect("control: in time");
    }

    /// The same for the write's own bound when it is the earlier one: a write
    /// that finishes past it — though well inside the deadline — is a write
    /// that took too long, not a success.
    #[tokio::test]
    async fn an_answer_finished_past_its_write_bound_is_not_a_success() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut late = LateWriter {
            until: std::time::Instant::now() + Duration::from_millis(100),
        };
        let err = answer_within(&mut late, b"x\n", deadline, Duration::from_millis(50))
            .await
            .expect_err("past the write bound");
        assert!(
            matches!(&err, HandshakeFailed::Write(e) if e.kind() == std::io::ErrorKind::TimedOut),
            "{err}"
        );
    }

    /// "Exactly 0700" includes the special bits: an otherwise-0700 directory
    /// of ours with the sticky bit is normalised, not accepted as it is.
    #[tokio::test]
    async fn a_directory_with_a_special_bit_is_normalised_to_exactly_0700() {
        use std::os::unix::fs::PermissionsExt;
        let s = state();
        std::fs::create_dir(s.path().join("run")).unwrap();
        std::fs::set_permissions(
            s.path().join("run"),
            std::fs::Permissions::from_mode(0o1700),
        )
        .unwrap();
        CallbackDir::create(s.path()).expect("ours, so normalised");
        let mode = std::fs::metadata(s.path().join("run"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(mode, 0o700, "{mode:o}");
    }

    /// Every listener gets a number the directory has not handed out before:
    /// the sequence "listen, serve one, listen on that number again, serve a
    /// second" cannot be written any more (PR-Daemon review of #179, L1).
    #[tokio::test]
    async fn every_listener_gets_a_path_never_used_before() {
        let s = state();
        let dir = CallbackDir::create(s.path()).unwrap();
        let paths: Vec<PathBuf> = (0..5)
            .map(|_| dir.listen_next().unwrap().path().to_owned())
            .collect();
        let unique: std::collections::BTreeSet<&PathBuf> = paths.iter().collect();
        assert_eq!(unique.len(), paths.len(), "{paths:?}");
    }

    /// FU-59: once its directory and every listener made from it are gone,
    /// the directory can be taken over again in this process — and not while
    /// a listener still lives, whose socket the take-over would empty away.
    #[tokio::test]
    async fn a_socket_directory_is_released_by_the_last_thing_using_it() {
        let s = state();
        drop(CallbackDir::create(s.path()).unwrap());
        let dir = CallbackDir::create(s.path()).expect("a directory nothing uses any more");
        let listener = dir.listen_next().unwrap();
        drop(dir);
        let err = CallbackDir::create(s.path()).expect_err("a listener still lives");
        assert!(matches!(err, EndpointError::AlreadyCreated(_)), "{err}");
        assert!(
            listener.path().exists(),
            "the live listener's socket was removed"
        );
        drop(listener);
        CallbackDir::create(s.path()).expect("released by the last listener");
    }

    /// A second take-over of the same directory is refused — it would empty
    /// the directory under a listener the first one made — and that listener's
    /// socket is still there afterwards (PR-Daemon review of #179, L2).
    #[tokio::test]
    async fn a_socket_directory_is_taken_over_once() {
        let s = state();
        let dir = CallbackDir::create(s.path()).unwrap();
        let listener = dir.listen_next().unwrap();
        let err = CallbackDir::create(s.path()).expect_err("a second take-over");
        assert!(matches!(err, EndpointError::AlreadyCreated(_)), "{err}");
        assert!(
            listener.path().exists(),
            "the first listener's socket was removed"
        );
    }

    /// Whatever the module sends right behind its `initialize` — pipelined in
    /// the same write — is still there to read after the handshake: the reader
    /// handed back keeps what it buffered past the first line. SUP-3 serves
    /// the connection from that reader.
    #[tokio::test]
    async fn bytes_sent_right_after_initialize_survive_the_handshake() {
        let s = state();
        let dir = CallbackDir::create(s.path()).unwrap();
        let listener = dir.listen_next().unwrap();
        let mut module = UnixStream::connect(listener.path()).await.unwrap();
        let next = r#"{"jsonrpc":"2.0","id":"2","method":"t/next","params":{}}"#;
        module
            .write_all(format!("{}{next}\n", initialize_frame("1", "s3cret")).as_bytes())
            .await
            .unwrap();
        let stream = listener.accept_one(soon()).await.unwrap();
        let mut done = handshake(stream, &expectation(), soon())
            .await
            .expect("handshake");
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), done.reader.read_line(&mut line))
            .await
            .expect("the pipelined line was lost")
            .unwrap();
        assert_eq!(line.trim_end(), next);
    }

    /// A take-over that fails does not hold the directory: once the cause is
    /// fixed, the next `create` succeeds (review of ME3-SUP slice 3a).
    #[tokio::test]
    async fn a_failed_take_over_can_be_retried() {
        use std::os::unix::fs::PermissionsExt;
        let s = state();
        let run = s.path().join("run");
        std::fs::create_dir(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
        // A file where the pid directory goes: `private_dir` refuses it.
        let pid_path = run
            .canonicalize()
            .unwrap()
            .join(std::process::id().to_string());
        std::fs::write(&pid_path, b"").unwrap();
        CallbackDir::create(s.path()).expect_err("a file is not a directory");
        std::fs::remove_file(&pid_path).unwrap();
        let dir = CallbackDir::create(s.path()).expect("the retry was refused");
        assert!(dir.listen_next().is_ok());
    }

    /// FU-56: at daemon start, the socket directories of daemons that are gone
    /// are removed — and one whose pid is alive is kept (it may be another
    /// daemon's; here a live child of this test stands in for it), as is this
    /// process's own.
    #[test]
    fn stale_socket_directories_are_removed_and_live_ones_kept() {
        let s = state();
        let run = s.path().join("run");
        private_dir(&run).unwrap();
        // A pid that is gone: a child that has already been waited for.
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let gone_pid = gone.id();
        gone.wait().unwrap();
        let mut alive = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        for pid in [gone_pid, alive.id(), std::process::id()] {
            std::fs::create_dir(run.join(pid.to_string())).unwrap();
        }
        let removed = remove_stale(s.path());
        assert_eq!(removed, vec![run.join(gone_pid.to_string())]);
        assert!(!run.join(gone_pid.to_string()).exists());
        assert!(
            run.join(alive.id().to_string()).exists(),
            "a live pid's directory was removed"
        );
        assert!(run.join(std::process::id().to_string()).exists());
        alive.kill().unwrap();
        alive.wait().unwrap();
    }
}
