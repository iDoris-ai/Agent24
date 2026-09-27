//! A3-2b (`docs/design/A3-ATTACHED-MODULE.md` §4, §5) — the persistent
//! `~/.agent24/attach/agent24d.sock` listener: unlike A1's per-generation,
//! accept-once callback socket (`agent24_os_proto::endpoint::CallbackListener`),
//! this ONE socket stays bound for the daemon's whole life and accepts as
//! many connections as arrive, each running its own independent handshake
//! (§4.1: "可多次 accept").

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent24_os_proto::attach_mux::serve_attached;
use agent24_os_proto::initialize::{accept_attached, error_line, id_of, success_line};
use agent24_os_proto::rpc::{Limits, read_frame_async};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::sync::CancellationToken;

use crate::attach_registry::AttachRegistry;

/// §4.3: "握手期（从 accept 起 5 s 内，含内核写完应答）任何失败 → 内核写一行
/// 错误后断开".
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(5);

/// Create `path`'s parent `0700` if missing (same rule
/// `agent24_os_proto::endpoint`'s private `private_dir` enforces for A1's
/// sockets — duplicated here rather than exported from that crate, which
/// A3-1 is mid-revision on as this lands).
fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
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
    Ok(())
}

/// Bind the attach socket, handling a stale node left by a daemon that is no
/// longer running (§5.6: "启动时若路径已是 socket 节点且连不上 → 视为残留，
/// unlink 后 bind；连得上 → 另一 daemon 在跑，附着监听降级并记录错误"), and a
/// path too long for `sockaddr_un` (§4.1: "路径过长时给出可操作报错" —
/// macOS's limit is 104 bytes including the NUL terminator).
async fn bind(path: &Path) -> Result<UnixListener, String> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    if std::fs::symlink_metadata(path).is_ok() {
        match UnixStream::connect(path).await {
            Ok(_probe) => {
                return Err(format!(
                    "{} is already accepting connections — another agent24d is running; this \
                     daemon's attach listener is disabled",
                    path.display()
                ));
            }
            Err(_not_reachable) => {
                // A residual node from a daemon that is gone (§5.6).
                std::fs::remove_file(path).map_err(|e| {
                    format!("cannot remove the stale socket {}: {e}", path.display())
                })?;
            }
        }
    }
    UnixListener::bind(path).map_err(|e| {
        if e.raw_os_error() == Some(libc::ENAMETOOLONG) {
            format!(
                "the attach socket path {} is too long for a Unix socket (macOS's limit is 104 \
                 bytes including the terminator) — this usually means $HOME itself is unusually \
                 long; move it or set A24_STATE_DIR to a shorter path",
                path.display()
            )
        } else {
            format!("cannot bind {}: {e}", path.display())
        }
    })
}

/// Run the listener until `stop` fires. Errors binding the socket are logged
/// and end this task WITHOUT taking the daemon down — an attach listener that
/// cannot start degrades attached-module support, exactly like a package
/// whose supervisor host failed to start degrades out-of-process packages
/// (`crate::domain::mount_package`'s own `degraded` path).
pub async fn run(registry: Arc<AttachRegistry>, path: PathBuf, stop: CancellationToken) {
    let listener = match bind(&path).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("attach listener not started: {e}");
            return;
        }
    };
    tracing::info!("attach listener bound at {}", path.display());
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _addr)) => {
                        let registry = Arc::clone(&registry);
                        let conn_stop = stop.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, &registry, conn_stop).await;
                        });
                    }
                    Err(e) => {
                        tracing::warn!("attach listener accept failed: {e}");
                    }
                }
            }
        }
    }
    // The socket path is a stable, daemon-lifetime resource (unlike A1's
    // per-generation node) — it is not unlinked here. A future start's
    // `bind` above is what reclaims a residual node, exactly like it does
    // after an ungraceful exit.
}

async fn handle_connection(stream: UnixStream, registry: &AttachRegistry, stop: CancellationToken) {
    // §4.1: same-UID only. The directory is already `0700`, so this is a
    // second line, not the first — same posture as
    // `CallbackListener::accept_one`.
    let peer_uid = match stream.peer_cred() {
        Ok(cred) => cred.uid(),
        Err(e) => {
            tracing::warn!("attach connection: could not read peer credentials: {e}");
            return;
        }
    };
    if peer_uid != rustix::process::geteuid().as_raw() {
        tracing::warn!("attach connection from foreign uid {peer_uid}: closing");
        return;
    }

    let deadline = tokio::time::Instant::now() + HANDSHAKE_DEADLINE;
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let frame = match tokio::time::timeout_at(deadline, read_frame_async(&mut reader)).await {
        Ok(Ok(frame)) => frame,
        Ok(Err(_frame_error)) => return,
        Err(_timeout) => {
            // Nothing coherent to answer with — the peer either sent
            // something unreadable or nothing at all before the deadline.
            return;
        }
    };

    let claim = match accept_attached(&frame, &|name| registry.expectation(name)) {
        Ok(claim) => claim,
        Err(refusal) => {
            let line = error_line(id_of(&frame).as_deref(), &refusal);
            let _ = write_within(&mut writer, &line, deadline).await;
            return;
        }
    };

    let (_number, generation, methods) = match registry.commit(&claim) {
        Ok(installed) => installed,
        Err(refusal) => {
            let line = error_line(Some(&claim.accepted.id), &refusal);
            let _ = write_within(&mut writer, &line, deadline).await;
            return;
        }
    };

    let line = success_line(&claim.accepted);
    if write_within(&mut writer, &line, deadline).await.is_err() {
        // The success line did not make it out in time — fail closed, same
        // rule `agent24_os_proto::endpoint::handshake` documents for A1: a
        // module that never received the agreed version must not be
        // admitted, whatever `commit` already installed.
        registry.release(&claim.module, &generation);
        return;
    }

    // §5.3: no drain for an attached generation — its own revocation (by
    // `DELETE`, `disable`, a rotation, or `revoke_all` at shutdown) is this
    // connection's only stop signal, ORed with the daemon-wide `stop` this
    // listener was handed (so a shutdown that has not yet reached
    // `revoke_all` still ends every open connection at the same deadline
    // everything else in `stopping` respects).
    let revoked_generation = Arc::clone(&generation);
    let connection_stop = async move {
        tokio::select! {
            () = revoked_generation.revoked() => {}
            () = stop.cancelled() => {}
        }
    };
    let (_calls, serve) =
        serve_attached(reader, writer, methods, Limits::default(), connection_stop);
    let _ended = serve.await;
    registry.release(&claim.module, &generation);
}

async fn write_within(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    line: &[u8],
    deadline: tokio::time::Instant,
) -> Result<(), ()> {
    let write = async {
        writer.write_all(line).await.map_err(|_| ())?;
        writer.flush().await.map_err(|_| ())
    };
    match tokio::time::timeout_at(deadline, write).await {
        Ok(result) => result,
        Err(_elapsed) => Err(()),
    }
}
