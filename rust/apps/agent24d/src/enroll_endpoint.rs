//! CORE-2e — the enrolment endpoint `<data_dir>/enroll.sock`
//! (`docs/agent/CORE-2-ENROLL-DESIGN.md` D5, D8).
//!
//! Frames follow the hello/challenge order frozen in the slice §3.1.

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
// Frozen code-signing allowlist from design D5; filenames are not identities.
const TEAM_ID: &str = "HYAP444FN9";
const IDENTIFIERS: &[&str] = &["ai.auraai.agent24", "agent24", "agent24d"];

use crate::enroll_api::*;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

pub(crate) trait IdentityVerifier: Send + Sync {
    fn start_time(&self, pid: i32) -> io::Result<ProcStart>;
    fn verify(&self, pid: i32) -> Result<VerifiedIdentity, EnrollErrorCode>;
}

pub(crate) struct SystemIdentityVerifier;
impl IdentityVerifier for SystemIdentityVerifier {
    fn start_time(&self, pid: i32) -> io::Result<ProcStart> {
        agent24_os_proc::start_time(pid).map(|s| ProcStart {
            sec: s.sec,
            usec: s.usec,
        })
    }
    fn verify(&self, pid: i32) -> Result<VerifiedIdentity, EnrollErrorCode> {
        let identity = agent24_os_proc::verify_code_identity(pid, TEAM_ID, IDENTIFIERS)
            .map_err(|_| EnrollErrorCode::IdentityRejected)?;
        Ok(VerifiedIdentity {
            kind: IdentityKind::MacosSecCode,
            identifier: identity.identifier,
            team_id: identity.team_id,
            executable: identity.executable,
        })
    }
}

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
    pub fn serve(
        &mut self,
        stop: CancellationToken,
        generation: String,
        authority: Arc<dyn EnrollAuthority>,
    ) {
        self.serve_with_identity(
            stop,
            generation,
            authority,
            Arc::new(SystemIdentityVerifier),
        );
    }

    fn serve_with_identity(
        &mut self,
        stop: CancellationToken,
        generation: String,
        authority: Arc<dyn EnrollAuthority>,
        verifier: Arc<dyn IdentityVerifier>,
    ) {
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
                            let generation = generation.clone();
                            let authority = authority.clone();
                            let verifier = verifier.clone();
                            tokio::spawn(async move {
                                let permit = Arc::new(permit);
                                tokio::select! {
                                    () = stop.cancelled() => {}
                                    () = answer(stream, generation, authority, verifier, stop.clone(), permit) => {}
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

#[derive(serde::Deserialize)]
struct Frame {
    v: u32,
    op: String,
    generation: Option<String>,
    method: Option<String>,
    path: Option<String>,
    body_sha256: Option<String>,
    summary: Option<String>,
}

async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Frame>, EnrollErrorCode> {
    let mut bytes = Vec::new();
    let count = tokio::time::timeout(
        FRAME_DEADLINE,
        reader.take(MAX_FRAME + 1).read_until(b'\n', &mut bytes),
    )
    .await
    .map_err(|_| EnrollErrorCode::Timeout)?
    .map_err(|_| EnrollErrorCode::BadFrame)?;
    if count == 0 {
        return Ok(None);
    }
    if count as u64 > MAX_FRAME || !bytes.ends_with(b"\n") {
        return Err(EnrollErrorCode::BadFrame);
    }
    // Version precedes shape validation, including malformed unknown-version frames.
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| EnrollErrorCode::BadFrame)?;
    if value
        .get("v")
        .and_then(|v| v.as_u64())
        .is_some_and(|v| v != 1)
    {
        return Err(EnrollErrorCode::UnsupportedVersion);
    }
    let frame: Frame = serde_json::from_value(value).map_err(|_| EnrollErrorCode::BadFrame)?;
    if frame.v != 1 {
        return Err(EnrollErrorCode::UnsupportedVersion);
    }
    Ok(Some(frame))
}

async fn send<W: tokio::io::AsyncWrite + Unpin>(
    write: &mut W,
    value: serde_json::Value,
) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    tokio::time::timeout(FRAME_DEADLINE, write.write_all(&bytes)).await??;
    Ok(())
}
async fn send_error<W: tokio::io::AsyncWrite + Unpin>(write: &mut W, code: EnrollErrorCode) {
    tracing::info!(event = "enroll_rejected", code = code.as_code());
    let message = if code == EnrollErrorCode::NotElevatedOperation {
        "elevation for this operation is not available yet"
    } else {
        "Enrollment rejected"
    };
    let _ = send(
        write,
        serde_json::json!({"v":1,"op":"error","code":code.as_code(),"message":message}),
    )
    .await;
}
async fn enrolled<W: tokio::io::AsyncWrite + Unpin>(
    write: &mut W,
    token: &IssuedToken,
    peer: &EnrolledPeer,
) -> io::Result<()> {
    send(write, serde_json::json!({"v":1,"op":"enrolled","token":token.secret,"expires_in":token.expires_in,"tier":"daily","identity":{"kind":"macos_seccode","identifier":peer.identity.identifier}})).await
}
fn peer_allowed(uid: u32, pid: Option<i32>) -> Option<i32> {
    (uid == rustix::process::geteuid().as_raw())
        .then_some(pid)
        .flatten()
        .filter(|p| *p > 0)
}

/// Keep the connection permit in the blocking job too. If Security.framework
/// outlives its deadline, it cannot accumulate work beyond the connection cap.
#[cfg(unix)]
async fn inspect<T: Send + 'static>(
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    read: impl FnOnce() -> Result<T, EnrollErrorCode> + Send + 'static,
) -> Result<T, EnrollErrorCode> {
    tokio::time::timeout(
        FRAME_DEADLINE,
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            read()
        }),
    )
    .await
    .map_err(|_| EnrollErrorCode::Timeout)?
    .map_err(|_| EnrollErrorCode::Internal)?
}

#[cfg(unix)]
async fn recheck(
    verifier: Arc<dyn IdentityVerifier>,
    pid: i32,
    start: ProcStart,
    identity: VerifiedIdentity,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> Result<(), EnrollErrorCode> {
    inspect(permit, move || {
        if verifier.start_time(pid).ok() != Some(start)
            || verifier.verify(pid).ok().as_ref() != Some(&identity)
        {
            return Err(EnrollErrorCode::IdentityRejected);
        }
        Ok(())
    })
    .await
}

#[cfg(unix)]
async fn answer(
    stream: tokio::net::UnixStream,
    generation: String,
    authority: Arc<dyn EnrollAuthority>,
    verifier: Arc<dyn IdentityVerifier>,
    cancel: CancellationToken,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
) {
    let Ok(cred) = stream.peer_cred() else {
        return;
    };
    let Some(pid) = peer_allowed(cred.uid(), cred.pid()) else {
        return;
    };
    let initial = verifier.clone();
    let Ok(start) = inspect(permit.clone(), move || {
        initial
            .start_time(pid)
            .map_err(|_| EnrollErrorCode::IdentityRejected)
    })
    .await
    else {
        return;
    };
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let hello = match read_frame(&mut reader).await {
        Ok(Some(frame)) => frame,
        Ok(None) => return,
        Err(code) => {
            send_error(&mut write, code).await;
            return;
        }
    };
    if hello.op != "hello" {
        send_error(&mut write, EnrollErrorCode::BadFrame).await;
        return;
    }
    if hello.generation.as_deref() != Some(&generation) {
        send_error(&mut write, EnrollErrorCode::GenerationMismatch).await;
        return;
    }
    let conn_id = random_hex(8);
    if send(
        &mut write,
        serde_json::json!({"v":1,"op":"challenge","conn_id":conn_id,"nonce":random_hex(16)}),
    )
    .await
    .is_err()
    {
        return;
    }
    let first = verifier.clone();
    let identity = match inspect(permit.clone(), move || first.verify(pid)).await {
        Ok(identity) => identity,
        Err(code) => {
            send_error(&mut write, code).await;
            return;
        }
    };
    // Recheck both identity and birth time: exec does not change birth time.
    if let Err(code) = recheck(
        verifier.clone(),
        pid,
        start,
        identity.clone(),
        permit.clone(),
    )
    .await
    {
        send_error(&mut write, code).await;
        return;
    }
    let peer = EnrolledPeer {
        conn_id,
        uid: cred.uid(),
        pid,
        start,
        identity,
        generation,
    };
    let token = match authority.issue(&peer) {
        Ok(token) => token,
        Err(code) => {
            send_error(&mut write, code).await;
            return;
        }
    };
    if enrolled(&mut write, &token, &peer).await.is_err() {
        return;
    }
    let mut token_id = token.token_id;
    drop(token.secret);
    loop {
        // The original connection can stay idle; only a started frame has a
        // five-second deadline. EOF cancels outstanding presence confirmation.
        match reader.fill_buf().await {
            Ok([]) => return,
            Err(_) => return,
            _ => {}
        }
        let frame = match read_frame(&mut reader).await {
            Ok(Some(frame)) => frame,
            Ok(None) => return,
            Err(code) => {
                send_error(&mut write, code).await;
                return;
            }
        };
        let result = match frame.op.as_str() {
            "renew" => {
                match recheck(
                    verifier.clone(),
                    pid,
                    start,
                    peer.identity.clone(),
                    permit.clone(),
                )
                .await
                {
                    Ok(()) => authority.renew(&peer, &token_id),
                    Err(code) => Err(code),
                }
            }
            "elevate" => {
                let (Some(method), Some(path), Some(body_sha256)) =
                    (frame.method, frame.path, frame.body_sha256)
                else {
                    send_error(&mut write, EnrollErrorCode::BadFrame).await;
                    return;
                };
                let summary = frame.summary.unwrap_or_default();
                if summary.chars().count() > 200
                    || body_sha256.len() != 64
                    || !body_sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    send_error(&mut write, EnrollErrorCode::BadFrame).await;
                    return;
                }
                let (pending, receive) = tokio::sync::oneshot::channel();
                let connection_cancel = cancel.child_token();
                let future = authority.elevate(
                    &peer,
                    &token_id,
                    ElevateRequest {
                        method,
                        path,
                        body_sha256,
                        client_summary: summary,
                    },
                    pending,
                    connection_cancel.clone(),
                );
                tokio::pin!(future);
                // Poll both: elevate sends pending before awaiting the provider.
                let result = tokio::select! {
                    biased;
                    id = receive => {
                        if let Ok(id) = id
                            && send(&mut write, serde_json::json!({"v":1,"op":"elevation_pending","challenge_id":id})).await.is_err() { connection_cancel.cancel(); return; }
                        tokio::select! {
                            result = &mut future => result,
                            _ = reader.fill_buf() => { connection_cancel.cancel(); return; }
                        }
                    }
                    result = &mut future => result,
                };
                match result {
                    Ok(grant) => { if send(&mut write, serde_json::json!({"v":1,"op":"elevated","challenge_id":grant.challenge_id,"grant":grant.secret,"expires_in":grant.expires_in})).await.is_err() { return; } }
                    Err(code) => send_error(&mut write, code).await,
                }
                continue;
            }
            _ => Err(EnrollErrorCode::BadFrame),
        };
        match result {
            Ok(new) => {
                if enrolled(&mut write, &new, &peer).await.is_err() {
                    return;
                }
                token_id = new.token_id;
            }
            Err(code) => {
                send_error(&mut write, code).await;
                return;
            }
        }
    }
}

fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut value = vec![0; bytes];
    rand::rng().fill_bytes(&mut value);
    value.iter().map(|b| format!("{b:02x}")).collect()
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
        if meta.uid() != rustix::process::geteuid().as_raw() {
            return Err(io::Error::other(
                "existing enroll socket has a foreign owner",
            ));
        }
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
    pub fn serve(
        &mut self,
        _stop: CancellationToken,
        _generation: String,
        _authority: Arc<dyn EnrollAuthority>,
    ) {
    }
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

#[cfg(all(test, unix))]
mod handshake_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Verifier {
        reject: bool,
        changed_start: bool,
        changed_identity: bool,
        starts: AtomicUsize,
        identities: AtomicUsize,
    }
    impl IdentityVerifier for Verifier {
        fn start_time(&self, _pid: i32) -> io::Result<ProcStart> {
            let second = self.starts.fetch_add(1, Ordering::SeqCst) > 0;
            Ok(ProcStart {
                sec: if second && self.changed_start { 2 } else { 1 },
                usec: 0,
            })
        }
        fn verify(&self, _pid: i32) -> Result<VerifiedIdentity, EnrollErrorCode> {
            if self.reject {
                return Err(EnrollErrorCode::IdentityRejected);
            }
            let second = self.identities.fetch_add(1, Ordering::SeqCst) > 0;
            Ok(VerifiedIdentity {
                kind: IdentityKind::MacosSecCode,
                identifier: if second && self.changed_identity {
                    "other"
                } else {
                    "agent24"
                }
                .into(),
                team_id: TEAM_ID.into(),
                executable: "/agent24".into(),
            })
        }
    }
    #[derive(Default)]
    struct Authority(AtomicUsize);
    #[async_trait]
    impl EnrollAuthority for Authority {
        fn issue(&self, peer: &EnrolledPeer) -> Result<IssuedToken, EnrollErrorCode> {
            assert_eq!(peer.pid, std::process::id() as i32);
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(IssuedToken {
                token_id: "id".into(),
                secret: "test-token".into(),
                expires_in: 3600,
            })
        }
        fn renew(&self, peer: &EnrolledPeer, _id: &str) -> Result<IssuedToken, EnrollErrorCode> {
            self.issue(peer)
        }
        async fn elevate(
            &self,
            _peer: &EnrolledPeer,
            _id: &str,
            _request: ElevateRequest,
            _pending: tokio::sync::oneshot::Sender<String>,
            _cancel: CancellationToken,
        ) -> Result<ElevationGrant, EnrollErrorCode> {
            Err(EnrollErrorCode::PresenceUnavailable)
        }
    }
    async fn line(reader: &mut BufReader<tokio::net::UnixStream>) -> serde_json::Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }
    async fn connect(
        reject: bool,
        changed_start: bool,
        changed_identity: bool,
    ) -> (
        tempfile::TempDir,
        EnrollEndpoint,
        CancellationToken,
        Arc<Authority>,
        BufReader<tokio::net::UnixStream>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut endpoint = bind(dir.path()).unwrap();
        let stop = CancellationToken::new();
        let authority = Arc::new(Authority::default());
        endpoint.serve_with_identity(
            stop.clone(),
            "generation".into(),
            authority.clone(),
            Arc::new(Verifier {
                reject,
                changed_start,
                changed_identity,
                starts: AtomicUsize::new(0),
                identities: AtomicUsize::new(0),
            }),
        );
        let stream = tokio::net::UnixStream::connect(endpoint.path())
            .await
            .unwrap();
        (dir, endpoint, stop, authority, BufReader::new(stream))
    }
    #[tokio::test]
    async fn hello_challenge_enrolled_and_renew_use_original_connection() {
        let (_dir, _endpoint, stop, authority, mut client) = connect(false, false, false).await;
        client
            .get_mut()
            .write_all(b"{\"v\":1,\"op\":\"hello\",\"generation\":\"generation\",\"pid\":1}\n")
            .await
            .unwrap();
        assert_eq!(line(&mut client).await["op"], "challenge");
        assert_eq!(line(&mut client).await["token"], "test-token");
        client
            .get_mut()
            .write_all(b"{\"v\":1,\"op\":\"renew\"}\n")
            .await
            .unwrap();
        assert_eq!(line(&mut client).await["op"], "enrolled");
        assert_eq!(authority.0.load(Ordering::SeqCst), 2);
        stop.cancel();
    }
    #[tokio::test]
    async fn rejected_identity_or_changed_process_never_issues() {
        for (reject, changed_start, changed_identity) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let (_dir, _endpoint, stop, authority, mut client) =
                connect(reject, changed_start, changed_identity).await;
            client
                .get_mut()
                .write_all(b"{\"v\":1,\"op\":\"hello\",\"generation\":\"generation\"}\n")
                .await
                .unwrap();
            assert_eq!(line(&mut client).await["op"], "challenge");
            assert_eq!(line(&mut client).await["code"], "identity_rejected");
            assert_eq!(authority.0.load(Ordering::SeqCst), 0);
            stop.cancel();
        }
    }
    #[tokio::test]
    async fn wrong_generation_version_and_oversize_never_issue() {
        for (frame, code) in [
            (
                b"{\"v\":1,\"op\":\"hello\",\"generation\":\"old\"}\n".to_vec(),
                "generation_mismatch",
            ),
            (b"{\"v\":2}\n".to_vec(), "unsupported_version"),
            (vec![b'x'; MAX_FRAME as usize + 1], "bad_frame"),
        ] {
            let (_dir, _endpoint, stop, authority, mut client) = connect(false, false, false).await;
            client.get_mut().write_all(&frame).await.unwrap();
            assert_eq!(line(&mut client).await["code"], code);
            assert_eq!(authority.0.load(Ordering::SeqCst), 0);
            stop.cancel();
        }
    }
    struct KernelStartVerifier;
    impl IdentityVerifier for KernelStartVerifier {
        fn start_time(&self, pid: i32) -> io::Result<ProcStart> {
            SystemIdentityVerifier.start_time(pid)
        }
        fn verify(&self, _pid: i32) -> Result<VerifiedIdentity, EnrollErrorCode> {
            Ok(VerifiedIdentity {
                kind: IdentityKind::MacosSecCode,
                identifier: "agent24".into(),
                team_id: TEAM_ID.into(),
                executable: std::env::current_exe().unwrap(),
            })
        }
    }
    struct AllowPresence;
    #[async_trait]
    impl crate::capabilities::presence::PresenceProvider for AllowPresence {
        async fn confirm(
            &self,
            _req: crate::capabilities::presence::PresenceRequest,
            _cancel: CancellationToken,
        ) -> crate::capabilities::presence::PresenceOutcome {
            crate::capabilities::presence::PresenceOutcome::Allowed
        }
    }
    #[tokio::test]
    async fn real_authority_pending_precedes_result_and_disconnect_does_not_revoke() {
        let dir = tempfile::tempdir().unwrap();
        let mut endpoint = bind(dir.path()).unwrap();
        let store = crate::capabilities::CapabilityStore::new("g");
        let authority = Arc::new(crate::capabilities::first_party::FirstPartyAuthority::new(
            store.clone(),
        ));
        let stop = CancellationToken::new();
        endpoint.serve_with_identity(
            stop.clone(),
            "g".into(),
            authority.clone(),
            Arc::new(KernelStartVerifier),
        );
        let mut client = BufReader::new(
            tokio::net::UnixStream::connect(endpoint.path())
                .await
                .unwrap(),
        );
        client
            .get_mut()
            .write_all(b"{\"v\":1,\"op\":\"hello\",\"generation\":\"g\"}\n")
            .await
            .unwrap();
        assert_eq!(line(&mut client).await["op"], "challenge");
        let old = line(&mut client).await["token"]
            .as_str()
            .unwrap()
            .to_owned();
        // No rejected HTTP body has been cached yet: fail before pending/provider.
        let body = serde_json::to_vec(&serde_json::json!({"manifest":"name: agentear\nversion: '1'\nroute_namespace: /api/v1/agentear\nevent_module: agentear\ndata_dir: ~/.agent24/os/agentear/\nimpl_kind: attached_process\n"})).unwrap();
        let frame = serde_json::json!({"v":1,"op":"elevate","method":"POST","path":"/api/v1/attached","body_sha256":crate::capabilities::first_party::body_digest(&body),"summary":"ignored"});
        send(client.get_mut(), frame.clone()).await.unwrap();
        let rejected = line(&mut client).await;
        assert_eq!(rejected["code"], "not_elevated_operation");
        assert_eq!(
            rejected["message"],
            "elevation for this operation is not available yet"
        );
        let authorization = store
            .validate_bearer(
                &old,
                crate::capabilities::Operation::ModelsRead,
                &crate::capabilities::Resource::global(),
                crate::capabilities::unix_now(),
            )
            .unwrap();
        authority.remember_attached_request(&authorization.claims.capability_id, &body);
        send(client.get_mut(), frame.clone()).await.unwrap();
        assert_eq!(line(&mut client).await["op"], "elevation_pending");
        assert_eq!(line(&mut client).await["code"], "presence_unavailable");
        authority.set_provider(Some(Arc::new(AllowPresence)));
        send(client.get_mut(), frame).await.unwrap();
        let pending = line(&mut client).await;
        let elevated = line(&mut client).await;
        assert_eq!(pending["op"], "elevation_pending");
        assert_eq!(elevated["op"], "elevated");
        assert_eq!(pending["challenge_id"], elevated["challenge_id"]);
        client
            .get_mut()
            .write_all(b"{\"v\":1,\"op\":\"renew\"}\n")
            .await
            .unwrap();
        let renewed = line(&mut client).await["token"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            store
                .require_live_bearer(&old, crate::capabilities::unix_now())
                .is_err()
        );
        drop(client);
        tokio::task::yield_now().await;
        assert!(
            store
                .require_live_bearer(&renewed, crate::capabilities::unix_now())
                .is_ok()
        );
        stop.cancel();
    }

    struct SlowVerifier;
    impl IdentityVerifier for SlowVerifier {
        fn start_time(&self, pid: i32) -> io::Result<ProcStart> {
            SystemIdentityVerifier.start_time(pid)
        }
        fn verify(&self, _pid: i32) -> Result<VerifiedIdentity, EnrollErrorCode> {
            std::thread::sleep(FRAME_DEADLINE + Duration::from_millis(100));
            Err(EnrollErrorCode::IdentityRejected)
        }
    }
    #[tokio::test]
    async fn identity_verification_deadline_never_issues() {
        let dir = tempfile::tempdir().unwrap();
        let mut endpoint = bind(dir.path()).unwrap();
        let stop = CancellationToken::new();
        let authority = Arc::new(Authority::default());
        endpoint.serve_with_identity(
            stop.clone(),
            "g".into(),
            authority.clone(),
            Arc::new(SlowVerifier),
        );
        let mut client = BufReader::new(
            tokio::net::UnixStream::connect(endpoint.path())
                .await
                .unwrap(),
        );
        client
            .get_mut()
            .write_all(b"{\"v\":1,\"op\":\"hello\",\"generation\":\"g\"}\n")
            .await
            .unwrap();
        assert_eq!(line(&mut client).await["op"], "challenge");
        assert_eq!(line(&mut client).await["code"], "timeout");
        assert_eq!(authority.0.load(Ordering::SeqCst), 0);
        stop.cancel();
    }

    #[tokio::test]
    async fn handshake_timeout_never_issues() {
        let (_dir, _endpoint, stop, authority, mut client) = connect(false, false, false).await;
        assert_eq!(line(&mut client).await["code"], "timeout");
        assert_eq!(authority.0.load(Ordering::SeqCst), 0);
        stop.cancel();
    }
    #[test]
    fn missing_pid_and_foreign_uid_are_refused() {
        let uid = rustix::process::geteuid().as_raw();
        assert_eq!(peer_allowed(uid, None), None);
        assert_eq!(peer_allowed(uid.wrapping_add(1), Some(1)), None);
        assert_eq!(peer_allowed(uid, Some(-1)), None);
    }
}
