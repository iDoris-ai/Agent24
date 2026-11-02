//! Docked startup (CORE-5 D6/D7, 5f-3). Reached only from `server::serve`
//! behind `migration_coordinator::ENABLED` (false), and from tests.
//!
//! A docked daemon keeps the singleton lock and keeps serving
//! [`docked_router`] (health + three diagnostics routes, gate and auth
//! unchanged). It starts no business task: no store, scheduler, module,
//! chat or sweep. It publishes `daemon.json` with `startup_state="docked"`
//! only after its listener is bound, and sends `type="docked"` on the ready
//! pipe, never `type="ready"`. It never returns a migration error, so the
//! process stays up for the service manager (`SuccessfulExit=false`) and
//! exits cleanly on SIGTERM/SIGINT (the signal handlers are `serve`'s).

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use agent24_protocol::HealthMigrations;
use agent24_protocol::state_file::{AuthMode, DaemonState};

use crate::capabilities::CapabilityStore;
use crate::capabilities::first_party::FirstPartyAuthority;
use crate::docked_router::{
    DockedState, MigrationDiagnostics, SnapshotDiagnostics, SpaceDiagnostics, docked_router,
};
use crate::migration_guard::atomic::RealOps;
use crate::migration_guard::dock::{
    DockCode, DockReason, DockedHealth, MigrationsState, RollbackState,
};
use crate::migration_guard::manifest::{BinaryIdentity, sha256_file};
use crate::migration_guard::startup::{Docking, decide};
use crate::server::Shutdown;

/// Where the single `docked` line goes.
pub enum ReadyOut {
    /// Service / legacy: stdout.
    Stdout,
    /// Host child: the private ready pipe.
    Host(crate::host_bootstrap::ReadyWriter),
    #[cfg(test)]
    Capture(tokio::sync::mpsc::UnboundedSender<serde_json::Value>),
}

pub struct Launch<'a> {
    pub data_dir: &'a Path,
    pub port: u16,
    pub auth_mode: AuthMode,
    /// Whether the enrolment endpoint is opened (service and host-child).
    pub enroll: bool,
    pub generation: String,
    pub capabilities: Option<CapabilityStore>,
    /// Host-child only: goes on the private pipe, never into `daemon.json`.
    pub product_host_token: Option<String>,
    pub ready: ReadyOut,
    pub shutdown: Shutdown,
    /// The singleton lock. Owned here so it lives exactly as long as the
    /// docked process serves.
    pub lock: Option<File>,
}

/// The identity of this binary, hashed lazily (only when a marker is written).
fn current_binary() -> BinaryIdentity {
    let sha256 = std::env::current_exe()
        .and_then(|p| sha256_file(&p))
        .map_or_else(|_| "unavailable".to_owned(), |(_, h)| h);
    BinaryIdentity {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        sha256,
    }
}

/// `enabled` is `migration_coordinator::ENABLED`. When false this is a pure
/// no-op: nothing is read, replayed or written, so startup is identical to
/// the pre-5f-3 daemon.
pub async fn pre_start(enabled: bool, data_dir: &Path) -> Option<Docking> {
    if !enabled {
        return None;
    }
    let dir = data_dir.to_path_buf();
    // Hashing and copying whole databases: off the async threads.
    match tokio::task::spawn_blocking(move || decide(&RealOps, &dir, &current_binary)).await {
        Ok(docking) => docking,
        Err(err) => {
            tracing::error!("migration guard check panicked: {err}");
            None
        }
    }
}

/// The state the docked router serves, from the real marker and manifest.
pub fn docked_state(
    d: &Docking,
    auth_mode: AuthMode,
    capabilities: Option<CapabilityStore>,
) -> DockedState {
    let m = &d.marker;
    let verified = matches!(
        m.rollback,
        RollbackState::Restored | RollbackState::NotNeeded
    ) && m.observed_migrations.is_some();
    let reason = DockReason {
        code: m.error_code,
        phase: m.phase,
        rollback: m.rollback,
        retry_allowed: false,
        migrations: if verified {
            MigrationsState::Verified
        } else {
            MigrationsState::Unknown
        },
    };
    let observed = m.observed_migrations.map_or(
        HealthMigrations {
            store: 0,
            memory: 0,
        },
        |o| HealthMigrations {
            store: o.store.version.unwrap_or(0),
            memory: o.memory.version.unwrap_or(0),
        },
    );
    let snapshots = match (&m.snapshot_id, &d.manifest) {
        (Some(id), Some(man)) => SnapshotDiagnostics::from_manifest(id, man, m.rollback),
        _ => SnapshotDiagnostics {
            snapshot_id: m.snapshot_id.clone(),
            restore: m.rollback,
            files: Vec::new(),
        },
    };
    let space_code = matches!(
        m.error_code,
        DockCode::SpaceInsufficient | DockCode::SpaceBudgetUnknown
    );
    DockedState {
        health: DockedHealth::new(
            reason,
            env!("CARGO_PKG_VERSION").to_owned(),
            "rust".to_owned(),
            agent24_protocol::PROTOCOL_VERSION,
            observed,
        ),
        migration: MigrationDiagnostics::from_marker(m),
        snapshots,
        space: SpaceDiagnostics {
            available_bytes: None,
            budget_bytes: None,
            failures: if space_code {
                vec![m.error_code]
            } else {
                vec![]
            },
        },
        capabilities,
        auth_mode,
    }
}

/// The only line a docked daemon sends. `type` is `docked`, never `ready`.
pub fn docked_line(
    port: u16,
    auth_mode: AuthMode,
    generation: &str,
    enroll_endpoint: Option<&str>,
    code: DockCode,
    host_token: Option<&str>,
) -> serde_json::Value {
    let mut line = serde_json::json!({
        "type": "docked",
        "port": port,
        "auth_mode": if auth_mode.is_capabilities() { "capabilities" } else { "legacy_single_token" },
        "generation": generation,
        "version": env!("CARGO_PKG_VERSION"),
        "enroll_endpoint": enroll_endpoint,
        "reason": code,
    });
    if let Some(token) = host_token {
        line["product_host_token"] = serde_json::json!(token);
    }
    line
}

/// Serve the docked daemon until shutdown. `Err` only for a failed start
/// (bind, enrolment bind, service-mode publish); never a migration error.
pub async fn serve_docked(mut l: Launch<'_>, docking: Docking) -> std::io::Result<()> {
    let _lock = l.lock.take(); // held until this function returns
    let reason_code = docking.marker.error_code;
    let state = docked_state(&docking, l.auth_mode, l.capabilities.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", l.port)).await?;
    let port = listener.local_addr()?.port();
    let router = crate::server::guard_host_origin(docked_router(state), port);
    let pid = std::process::id();

    let mut endpoint = if l.enroll {
        Some(crate::enroll_endpoint::bind(l.data_dir)?)
    } else {
        None
    };
    let enroll_path = endpoint
        .as_ref()
        .map(|e| e.path().to_string_lossy().into_owned());
    // Published only now, after the listener is bound. No secret: a legacy
    // record needs a non-empty token field, so it gets a random one that the
    // docked router never accepts.
    let record = match l.auth_mode {
        AuthMode::Capabilities => {
            DaemonState::new_capabilities(port, pid, env!("CARGO_PKG_VERSION"), &l.generation)
        }
        AuthMode::LegacySingleToken => DaemonState::new_legacy(
            port,
            crate::server::generate_token(),
            pid,
            env!("CARGO_PKG_VERSION"),
            &l.generation,
        ),
    }
    .map_err(std::io::Error::other)?;
    let record = match &enroll_path {
        Some(p) => record.with_enroll_endpoint(p.clone()),
        None => record,
    }
    .docked();
    agent24_protocol::state_file::write_in(l.data_dir, &record)?;
    let cleanup = || {
        agent24_protocol::state_file::remove_if_owner_in(l.data_dir, pid, Some(&l.generation));
    };

    if let (Some(e), Some(store)) = (endpoint.as_mut(), l.capabilities.clone()) {
        e.serve(
            l.shutdown.token().clone(),
            l.generation.clone(),
            port,
            Arc::new(FirstPartyAuthority::new(store)),
        );
    }
    if !l.shutdown.commit_ready() {
        cleanup();
        return Ok(());
    }
    let line = docked_line(
        port,
        l.auth_mode,
        &l.generation,
        enroll_path.as_deref(),
        reason_code,
        l.product_host_token.as_deref(),
    );
    let sent = match &mut l.ready {
        ReadyOut::Stdout => {
            println!("{line}");
            Ok(())
        }
        ReadyOut::Host(w) => w.send(&line).await,
        #[cfg(test)]
        ReadyOut::Capture(tx) => tx.send(line).map_err(std::io::Error::other),
    };
    if let Err(err) = sent {
        cleanup();
        return Err(err);
    }
    crate::server::READY_SENT.store(true, std::sync::atomic::Ordering::SeqCst);

    let cancel = l.shutdown.token().clone();
    let graceful = cancel.clone();
    let server = axum::serve(listener, router)
        .with_graceful_shutdown(async move { graceful.cancelled().await });
    let result = tokio::select! {
        r = server => r,
        () = async {
            cancel.cancelled().await;
            tokio::time::sleep_until(l.shutdown.deadline()).await;
        } => Ok(()),
    };
    // A server that ended on its own (accept error) still starts the ordered
    // shutdown; a cancelled one was already requested by whoever cancelled.
    if !cancel.is_cancelled() {
        l.shutdown.request();
    }
    cleanup();
    result
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::migration_guard::dock::{
        DbMark, DockPhase, MigrationMarker, PairMark, marker_path, write_marker,
    };
    use crate::migration_guard::manifest::FORMAT_VERSION;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_util::sync::CancellationToken;

    fn docking(code: DockCode, rollback: RollbackState) -> Docking {
        let mark = DbMark {
            existed: true,
            version: Some(7),
        };
        Docking {
            marker: MigrationMarker {
                format_version: FORMAT_VERSION,
                attempt_id: "a1".into(),
                snapshot_id: None,
                created_at: "2026-11-02T00:00:00Z".into(),
                failed_binary: BinaryIdentity {
                    version: "0.6.0".into(),
                    sha256: "h".into(),
                },
                previous_binary: None,
                database: None,
                migration_version: None,
                phase: DockPhase::Migrating,
                error_code: code,
                error_summary: code.summary().into(),
                rollback,
                before_migrations: PairMark {
                    store: mark,
                    memory: mark,
                },
                observed_migrations: Some(PairMark {
                    store: mark,
                    memory: mark,
                }),
            },
            manifest: None,
        }
    }

    async fn get(port: u16, path: &str) -> String {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let req =
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    }

    #[test]
    fn docked_line_is_never_ready() {
        let l = docked_line(
            1,
            AuthMode::Capabilities,
            "g",
            None,
            DockCode::Interrupted,
            None,
        );
        assert_eq!(l["type"], "docked");
        assert!(l.get("product_host_token").is_none());
    }

    #[test]
    fn state_comes_from_the_marker() {
        let s = docked_state(
            &docking(DockCode::MigrationFailed, RollbackState::Restored),
            AuthMode::Capabilities,
            None,
        );
        assert_eq!(s.health.migrations.store, 7);
        assert_eq!(s.health.reason.migrations, MigrationsState::Verified);
        let s = docked_state(
            &docking(DockCode::RollbackFailed, RollbackState::Failed),
            AuthMode::Capabilities,
            None,
        );
        assert_eq!(s.health.migrations.store, 0, "unknown hides the numbers");
        assert_eq!(s.health.reason.migrations, MigrationsState::Unknown);
    }

    /// ENABLED=false: nothing is read, replayed or written, even with a
    /// marker on disk; enabled, the same dir docks.
    #[tokio::test]
    async fn disabled_pre_start_is_a_no_op() {
        let td = tempfile::tempdir().unwrap();
        let d = docking(DockCode::Interrupted, RollbackState::NotNeeded);
        write_marker(&RealOps, td.path(), &d.marker).unwrap();
        let before = std::fs::read(marker_path(td.path())).unwrap();
        assert!(pre_start(false, td.path()).await.is_none());
        assert!(pre_start(true, td.path()).await.is_some());
        assert_eq!(std::fs::read(marker_path(td.path())).unwrap(), before);
    }

    /// The hook in `serve` is behind ENABLED, after the lock and the signal
    /// handlers, before the first business start.
    #[test]
    fn serve_hook_sits_between_the_lock_and_the_store() {
        let src = include_str!("server.rs");
        let serve = &src[src.find("pub async fn serve(").unwrap()..];
        let at = |needle: &str| serve.find(needle).unwrap_or_else(|| panic!("{needle}"));
        let hook = at("docked_startup::pre_start(");
        assert!(at("signal(SignalKind::terminate())") < hook);
        assert!(at("try_acquire_singleton()") < hook);
        assert!(hook < at("Store::open(&dir.join"));
        assert!(hook < at("spawn_startup_snapshot"));
        assert!(serve[hook..hook + 120].contains("migration_coordinator::ENABLED"));
    }

    /// T13: docked serve publishes `docked`, sends `docked`, serves only the
    /// docked routes, holds the lock, opens no database, and exits cleanly
    /// when cancelled.
    #[tokio::test]
    async fn docked_serve_end_to_end() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().to_path_buf();
        let lock = agent24_protocol::state_file::try_acquire_singleton_in(&dir)
            .unwrap()
            .unwrap();
        let cancel = CancellationToken::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let generation = "gen-t13".to_owned();
        let launch = Launch {
            data_dir: &dir,
            port: 0,
            auth_mode: AuthMode::Capabilities,
            enroll: false,
            generation: generation.clone(),
            capabilities: Some(CapabilityStore::new(generation.clone())),
            product_host_token: None,
            ready: ReadyOut::Capture(tx),
            shutdown: Shutdown::new(cancel.clone()),
            lock: Some(lock),
        };
        let dk = docking(DockCode::MigrationFailed, RollbackState::Restored);
        let handle = {
            let dir2 = dir.clone();
            tokio::spawn(async move {
                let l = Launch {
                    data_dir: &dir2,
                    ..launch
                };
                serve_docked(l, dk).await
            })
        };
        let line = rx.recv().await.unwrap();
        assert_eq!(line["type"], "docked");
        let port = u16::try_from(line["port"].as_u64().unwrap()).unwrap();

        let rec: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("daemon.json")).unwrap()).unwrap();
        assert_eq!(rec["startup_state"], "docked");
        assert_eq!(rec["generation"], "gen-t13");
        assert!(rec.get("token").is_none());

        assert!(
            get(port, "/api/v1/health")
                .await
                .starts_with("HTTP/1.1 503")
        );
        assert!(get(port, "/api/v1/runs").await.starts_with("HTTP/1.1 404"));
        assert!(
            get(port, "/api/v1/events")
                .await
                .starts_with("HTTP/1.1 404")
        );
        assert!(!dir.join("agent24.db").exists() && !dir.join("memory.db").exists());
        assert!(
            agent24_protocol::state_file::try_acquire_singleton_in(&dir)
                .unwrap()
                .is_none(),
            "a second instance cannot take the lock while docked"
        );

        cancel.cancel();
        handle.await.unwrap().unwrap();
        assert!(!dir.join("daemon.json").exists(), "own record removed");
        assert!(
            agent24_protocol::state_file::try_acquire_singleton_in(&dir)
                .unwrap()
                .is_some(),
            "lock released only at exit"
        );
    }

    /// Q3: with the enrolment channel on, the endpoint is bound, published in
    /// `daemon.json` and on the docked line.
    #[tokio::test]
    async fn docked_opens_the_enrolment_endpoint_when_asked() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().to_path_buf();
        let cancel = CancellationToken::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let l = Launch {
            data_dir: &dir,
            port: 0,
            auth_mode: AuthMode::Capabilities,
            enroll: true,
            generation: "g".into(),
            capabilities: Some(CapabilityStore::new(String::from("g"))),
            product_host_token: None,
            ready: ReadyOut::Capture(tx),
            shutdown: Shutdown::new(cancel.clone()),
            lock: None,
        };
        let d2 = dir.clone();
        let h = tokio::spawn(async move {
            serve_docked(
                Launch { data_dir: &d2, ..l },
                docking(DockCode::Interrupted, RollbackState::NotNeeded),
            )
            .await
        });
        let line = rx.recv().await.unwrap();
        let sock = line["enroll_endpoint"].as_str().unwrap().to_owned();
        assert!(std::path::Path::new(&sock).exists());
        let rec: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("daemon.json")).unwrap()).unwrap();
        assert_eq!(rec["enroll_endpoint"], sock);
        cancel.cancel();
        h.await.unwrap().unwrap();
        assert!(!std::path::Path::new(&sock).exists());
    }

    /// Cleanup must not delete another generation's record.
    #[test]
    fn cleanup_keeps_another_generation() {
        let td = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let other = DaemonState::new_capabilities(1, pid, "v", "other-gen").unwrap();
        agent24_protocol::state_file::write_in(td.path(), &other).unwrap();
        agent24_protocol::state_file::remove_if_owner_in(td.path(), pid, Some("mine"));
        assert!(td.path().join("daemon.json").exists());
        agent24_protocol::state_file::remove_if_owner_in(td.path(), pid, Some("other-gen"));
        assert!(!td.path().join("daemon.json").exists());
    }
}
