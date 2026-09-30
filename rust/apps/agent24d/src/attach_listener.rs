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
use agent24_os_proto::initialize::{
    HandshakeError, accept_attached, error_line, id_of, success_line,
};
use agent24_os_proto::rpc::{Limits, read_frame_async};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::sync::CancellationToken;

use crate::attach_registry::AttachRegistry;

/// §4.3: "握手期（从 accept 起 5 s 内，含内核写完应答）任何失败 → 内核写一行
/// 错误后断开".
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(5);

/// Ensure `path` exists (created `0700` if missing) and is safe to trust as
/// the attach socket's parent directory. Review M3: mirrors A1's own
/// `agent24_os_proto::endpoint::check_private` (duplicated here rather than
/// exported from that crate, which A3-1 was mid-revision on as this landed)
/// — a REAL directory (`symlink_metadata`, so a symlink to a directory is
/// caught, not followed), owned by this process's euid, mode EXACTLY
/// `0700`. The FIRST version only fixed up an existing directory's mode
/// (`chmod` it to `0700` if it was, say, `0755`) and never even looked at
/// ownership or symlink-ness — silently trusting (and quietly tightening the
/// permissions of) a directory an attacker, or another user on a shared
/// host, put there first is exactly the tampering this check exists to
/// catch, so an existing directory that fails any of these checks is
/// REFUSED, not normalized.
///
/// # Errors
///
/// A human-readable reason; the caller does not bind and the attach
/// listener degrades (same posture as a package whose supervisor host
/// failed to start, `crate::domain::mount_package`'s own `degraded` path).
fn ensure_private_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
    if !meta.is_dir() {
        return Err(format!(
            "{} is not a directory (or is a symlink to one) — refusing to trust it for the \
             attach socket",
            path.display()
        ));
    }
    let me = rustix::process::geteuid().as_raw();
    if meta.uid() != me {
        return Err(format!(
            "{} is owned by uid {}, not this process's euid {me} — refusing to trust it",
            path.display(),
            meta.uid()
        ));
    }
    if meta.mode() & 0o7777 != 0o700 {
        return Err(format!(
            "{} has mode {:04o}, not exactly 0700 — refusing to trust it",
            path.display(),
            meta.mode() & 0o7777
        ));
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
        ensure_private_dir(parent)?;
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
                 long; move it to a shorter path",
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

    // Review (Codex A3 follow-up, §5.5): checked BEFORE `accept_attached` at
    // all, and independently of `name` — once the registry is closed for
    // shutdown, `AttachRegistry::expectation` returns `None` for EVERY name
    // (its map was drained by `revoke_all`), which `accept_attached` cannot
    // tell apart from "no such module" and would answer `auth_failed`. That
    // is the wrong answer for a module that was legitimately registered and
    // just lost a race with shutdown — see `AttachRegistry::is_closed`'s own
    // doc, and `HandshakeError::ShuttingDown`'s, for why this is a distinct
    // wire error rather than reusing `auth_failed`.
    if registry.is_closed() {
        let line = error_line(id_of(&frame).as_deref(), &HandshakeError::ShuttingDown);
        let _ = write_within(&mut writer, &line, deadline).await;
        return;
    }

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
        // Review M2 ①: a registry closed for shutdown gets NO wire frame at
        // all — see `CommitRefused::Closed`'s own doc for why `auth_failed`
        // would be the wrong (and permanently reconnect-stopping) answer
        // here. The module just sees the connection end, same as any other
        // EOF.
        Err(crate::attach_registry::CommitRefused::Closed) => return,
        Err(crate::attach_registry::CommitRefused::Handshake(refusal)) => {
            let line = error_line(Some(&claim.accepted.id), &refusal);
            let _ = write_within(&mut writer, &line, deadline).await;
            return;
        }
    };

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
    let (calls, serve) =
        serve_attached(reader, writer, methods, Limits::default(), connection_stop);

    // C1 (pre-release): the handshake success line is now enqueued through
    // `calls` (`KernelCalls::enqueue_raw`) INSTEAD OF being written directly
    // to `writer` here, and — critically — BEFORE `attach_kernel_calls`
    // installs `calls` into the registry, not after. The old order (write
    // the success line to the raw connection, THEN install `calls`) left a
    // window where the module already has its handshake result but
    // `POST /api/v1/os/{name}/commands/*` still gets a spurious
    // `503 module_not_ready`, because the registry has no `KernelCalls` for
    // this generation yet — confirmed flaky on `main@77b05cb` and PR #541's
    // CI (`a3_3_host_commands_blackbox`'s
    // `c5_a_dropped_connection_after_the_frame_was_sent_is_502_connection_lost`),
    // and the likely cause of AgentEar's real-machine E2E S6 `speak` 503 on
    // its first round. Enqueuing first closes it: nothing else has a handle
    // on `calls` until the very next line installs it, so `line` cannot help
    // but be message #1 on `calls`' own outbound queue — see
    // `KernelCalls::enqueue_raw`'s own doc for the full argument.
    let line = success_line(&claim.accepted);
    if calls.enqueue_raw(line).is_err() {
        // The outbound queue is already gone (or, impossibly for a queue
        // this fresh, momentarily full) — fail closed, same rule
        // `agent24_os_proto::endpoint::handshake` documents for A1: a module
        // that never received the agreed version must not be admitted.
        // `calls` (and so `serve`'s background tasks) is simply dropped
        // here, never installed and never awaited.
        registry.release(&claim.module, &generation);
        return;
    }
    // A3-3 (`docs/design/A3-ATTACHED-MODULE.md` §6.2): hand the registry this
    // connection's `KernelCalls` so `POST /api/v1/os/{name}/commands/{command}`
    // (`crate::attach_commands`) can reach it. Strictly after `commit`
    // installed `generation` as `last_generation` above — see
    // `AttachRegistry::attach_kernel_calls`'s own doc for why a rotation that
    // lands in between makes this a harmless no-op rather than a race.
    registry.attach_kernel_calls(&claim.module, &generation, calls);
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    fn tmp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "a24-attach-listener-test-{n}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn a_missing_directory_is_created_0700() {
        let dir = tmp_dir();
        assert!(ensure_private_dir(&dir).is_ok());
        let meta = std::fs::symlink_metadata(&dir).unwrap();
        assert_eq!(meta.permissions().mode() & 0o7777, 0o700);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_existing_0700_directory_of_ours_passes() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(ensure_private_dir(&dir).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Review M3's own example: an existing `0755` directory must be
    /// REFUSED, not silently tightened to `0700` — the first version of this
    /// function did the latter, which is exactly the "quietly trust
    /// whatever is already there" gap the review caught.
    #[test]
    fn an_existing_0755_directory_is_refused_not_silently_fixed() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = ensure_private_dir(&dir).expect_err("0755 must be refused");
        assert!(err.contains("0755") || err.contains("mode"), "{err}");
        // Control: it really was left at 0755, not fixed up as a side effect
        // of the (refused) attempt.
        let meta = std::fs::symlink_metadata(&dir).unwrap();
        assert_eq!(meta.permissions().mode() & 0o7777, 0o755);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Review M3: a symlink standing in for the directory (even one that
    /// points at a real `0700` directory) must be refused — `check_private`'s
    /// whole point is that `symlink_metadata` (never followed) is what tells
    /// a real directory apart from one a same-uid attacker swapped for a
    /// symlink after the fact.
    #[test]
    fn a_symlink_standing_in_for_the_directory_is_refused() {
        let real_dir = tmp_dir();
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::set_permissions(&real_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = tmp_dir();
        std::os::unix::fs::symlink(&real_dir, &link).unwrap();
        let err = ensure_private_dir(&link).expect_err("a symlink must be refused");
        assert!(
            err.contains("symlink") || err.contains("not a directory"),
            "{err}"
        );
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_dir_all(&real_dir).unwrap();
    }

    // ── Review (Codex A3 follow-up, §5.5): a handshake racing shutdown ──

    /// A `RunTrigger` that never fires — this test only needs a `Scheduler`
    /// to exist so `AttachDeps` can be built without wiring a real one. Same
    /// shape as `crate::attach_registry::tests::NoopTrigger` (duplicated
    /// rather than shared: that one lives in a private `mod tests` in a
    /// different file).
    struct NoopTrigger;

    #[async_trait::async_trait]
    impl agent24_scheduler::RunTrigger for NoopTrigger {
        async fn trigger(
            &self,
            _invocation: &agent24_scheduler::ScheduleInvocation,
        ) -> agent24_scheduler::FireOutcome {
            agent24_scheduler::FireOutcome::Deferred {
                reason: agent24_scheduler::DeferReason::MountPending,
            }
        }
    }

    async fn test_attach_deps() -> crate::attach_registry::AttachDeps {
        let store = agent24_store::Store::open_memory().await.unwrap();
        crate::attach_registry::AttachDeps {
            scheduler: agent24_scheduler::Scheduler::new(
                store.clone(),
                std::sync::Arc::new(NoopTrigger),
                std::sync::Arc::new(|_| {}),
            ),
            models: None,
            approval_broker: crate::module_approval_broker::ModuleApprovalBroker::new(
                store,
                crate::events::EventsHub::default(),
            ),
            events: crate::events::EventsHub::default(),
        }
    }

    /// Review (Codex A3 follow-up): before this fix, `handle_connection`
    /// handed `registry.expectation(name)` straight to `accept_attached`
    /// with no way to tell "no such module" apart from "the registry is
    /// closed for shutdown" — `revoke_all()` drains the registry's entire
    /// map, so a handshake for a name that WAS legitimately registered a
    /// moment ago gets exactly the same `None` a truly unknown name would,
    /// and `accept_attached` answers both with `auth_failed`. A real
    /// AgentEar reading `auth_failed` stops reconnecting and demands a fresh
    /// pairing (§5.6's own table) — the wrong response to "the daemon is
    /// just restarting".
    ///
    /// This drives the REAL `handle_connection` over a real (in-process)
    /// Unix socket pair, with a REAL registered module (minted through
    /// `crate::attached::register`, so `resp.token` is a real plaintext
    /// token whose hash genuinely matches what the registry holds) — not a
    /// mock of either. `revoke_all()` runs before the connection is ever
    /// handled, simulating shutdown having won the race.
    #[tokio::test]
    async fn a_handshake_racing_shutdown_gets_unavailable_not_auth_failed() {
        use tokio::io::AsyncBufReadExt;

        let registry = crate::attach_registry::AttachRegistry::new(test_attach_deps().await);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("attached.json");
        let yaml = "name: agentear\nversion: \"1\"\nroute_namespace: /api/v1/agentear\n\
                     event_module: agentear\ndata_dir: ~/.agent24/os/agentear/\n\
                     impl_kind: attached_process\nkernel_capabilities: [events]\n";
        let outcome = crate::attached::register(
            &path,
            yaml,
            false,
            |_| false,
            |change| {
                registry.on_change(change);
            },
        )
        .unwrap();
        let crate::attached::RegisterOutcome::Created(resp) = outcome else {
            panic!("expected Created");
        };

        // The daemon decided to shut down before this (otherwise perfectly
        // legitimate) connection was handled.
        registry.revoke_all();

        let (kernel_side, module_side) = UnixStream::pair().unwrap();
        let mut frame = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": "1",
            "method": "initialize",
            "params": {
                "protocol_versions": {"min": 1, "max": 1},
                "module": "agentear",
                "manifest_digest": resp.manifest_digest,
                "auth_token": resp.token,
                "capabilities": ["events"],
            }
        }))
        .unwrap();
        frame.push(b'\n');
        let mut module_side = module_side;
        module_side.write_all(&frame).await.unwrap();

        handle_connection(kernel_side, &registry, CancellationToken::new()).await;

        let mut reader = BufReader::new(module_side);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(
            v["error"]["data"]["kind"], "unavailable",
            "a handshake that lost the race with shutdown must get `unavailable`, not \
             `auth_failed` — the presented token was perfectly valid: {v}"
        );
        assert_ne!(v["error"]["data"]["kind"], "auth_failed");
    }
}
