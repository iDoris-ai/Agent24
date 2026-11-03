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
use crate::migration_guard::atomic::{FileOps, RealOps};
use crate::migration_guard::dock::{
    DockCode, DockReason, DockedHealth, MigrationsState, RollbackState,
};
use crate::migration_guard::manifest::{BinaryIdentity, sha256_file};
use crate::migration_guard::startup::{Docking, decide, fail_closed};
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

/// Whether the guard runs: `migration_coordinator::ENABLED`, which is false.
/// Only a debug test build (feature `test-enrollment`, a compile error in
/// release) may force it on, with `AGENT24_TEST_MIGRATION_GUARD=1`, so the
/// black-box tests can drive the real `serve` into the docked path.
pub fn guard_enabled() -> bool {
    #[cfg(feature = "test-enrollment")]
    if std::env::var("AGENT24_TEST_MIGRATION_GUARD").as_deref() == Ok("1") {
        return true;
    }
    crate::migration_coordinator::ENABLED
}

/// `enabled` is [`guard_enabled`]. When false this is a pure no-op: nothing
/// is read, replayed or written, so startup is identical to the pre-5f-3
/// daemon. When true it never answers "carry on" after a failure of its own.
pub async fn pre_start(enabled: bool, data_dir: &Path) -> Option<Docking> {
    pre_start_with(enabled, data_dir, RealOps).await
}

async fn pre_start_with<O: FileOps + Send + 'static>(
    enabled: bool,
    data_dir: &Path,
    ops: O,
) -> Option<Docking> {
    if !enabled {
        return None;
    }
    let dir = data_dir.to_path_buf();
    // Hashing and copying whole databases: off the async threads.
    match tokio::task::spawn_blocking(move || decide(&ops, &dir, &current_binary)).await {
        Ok(docking) => docking,
        // A panic or cancellation may have left a half-restored pair. Carrying
        // on would reach `Store::open` and auto-migrate it, so dock instead.
        // No marker is written: the state is unknown, not a recorded failure.
        Err(err) => {
            tracing::error!("migration guard check failed: {err}");
            Some(fail_closed(current_binary()))
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
) -> serde_json::Value {
    // No credential of any kind: today's shells log a non-`ready` line in
    // plain text. Diagnostics are reached by enrolling (5g teaches the shells).
    serde_json::json!({
        "type": "docked",
        "port": port,
        "auth_mode": if auth_mode.is_capabilities() { "capabilities" } else { "legacy_single_token" },
        "generation": generation,
        "version": env!("CARGO_PKG_VERSION"),
        "enroll_endpoint": enroll_endpoint,
        "reason": code,
    })
}

/// What a docked daemon enrols shells against: tokens reach only the docked
/// diagnostics (M4).
fn docked_authority(store: CapabilityStore) -> Arc<FirstPartyAuthority> {
    Arc::new(FirstPartyAuthority::new_docked(store))
}

/// Serve the docked daemon until shutdown. `Err` only for a failed start
/// (bind, enrolment bind, ready pipe); never a migration error. A
/// `daemon.json` that cannot be written is logged and the daemon keeps
/// serving: exiting would restart-loop under the service manager (D6/D7).
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
    if let Err(err) = agent24_protocol::state_file::write_in(l.data_dir, &record) {
        tracing::warn!("could not write docked daemon state file: {err}");
    }
    let cleanup = || {
        agent24_protocol::state_file::remove_if_owner_in(l.data_dir, pid, Some(&l.generation));
    };

    if let (Some(e), Some(store)) = (endpoint.as_mut(), l.capabilities.clone()) {
        e.serve(
            l.shutdown.token().clone(),
            l.generation.clone(),
            port,
            docked_authority(store),
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

    use std::io::Read;
    use std::path::PathBuf;

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

    struct Running {
        dir: PathBuf,
        cancel: CancellationToken,
        handle: tokio::task::JoinHandle<std::io::Result<()>>,
        _td: tempfile::TempDir,
    }

    fn launch(
        dir: &Path,
        cancel: &CancellationToken,
        enroll: bool,
        generation: &str,
        ready: ReadyOut,
        lock: Option<File>,
    ) -> Launch<'static> {
        // `Launch` borrows the data dir; tests leak a copy for the task's life.
        let dir: &'static Path = Box::leak(dir.to_path_buf().into_boxed_path());
        Launch {
            data_dir: dir,
            port: 0,
            auth_mode: AuthMode::Capabilities,
            enroll,
            generation: generation.to_owned(),
            capabilities: Some(CapabilityStore::new(generation.to_owned())),
            ready,
            shutdown: Shutdown::new(cancel.clone()),
            lock,
        }
    }

    async fn start_capture(
        enroll: bool,
        generation: &str,
        prepare: impl FnOnce(&Path),
    ) -> (Running, serde_json::Value) {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().to_path_buf();
        prepare(&dir);
        let cancel = CancellationToken::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let l = launch(
            &dir,
            &cancel,
            enroll,
            generation,
            ReadyOut::Capture(tx),
            None,
        );
        let handle = tokio::spawn(serve_docked(
            l,
            docking(DockCode::MigrationFailed, RollbackState::Restored),
        ));
        let line = rx.recv().await.unwrap();
        (
            Running {
                dir,
                cancel,
                handle,
                _td: td,
            },
            line,
        )
    }

    fn port_of(line: &serde_json::Value) -> u16 {
        u16::try_from(line["port"].as_u64().unwrap()).unwrap()
    }

    fn daemon_json(dir: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(dir.join("daemon.json")).unwrap()).unwrap()
    }

    #[test]
    fn docked_line_is_never_ready_and_never_carries_a_credential() {
        let l = docked_line(1, AuthMode::Capabilities, "g", None, DockCode::Interrupted);
        assert_eq!(l["type"], "docked");
        assert!(!l.to_string().contains("token"));
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
        assert!(!guard_enabled(), "production default is off");
    }

    struct PanickingOps;
    impl FileOps for PanickingOps {
        fn write_new_synced(&self, _: &Path, _: &[u8]) -> std::io::Result<()> {
            panic!("injected")
        }
        fn rename(&self, _: &Path, _: &Path) -> std::io::Result<()> {
            panic!("injected")
        }
        fn fsync_dir(&self, _: &Path) -> std::io::Result<()> {
            panic!("injected")
        }
        fn remove_file(&self, _: &Path) -> std::io::Result<()> {
            panic!("injected")
        }
    }

    /// H1: a panic inside replay must dock, never fall through to `Store::open`.
    #[tokio::test]
    async fn a_panic_in_replay_docks_instead_of_proceeding() {
        use crate::migration_guard::guard_log::{GuardLog, Phase, advance, persist};
        use crate::migration_guard::manifest::test_empty_snapshot;
        let td = tempfile::tempdir().unwrap();
        let snap = test_empty_snapshot(td.path(), "a1");
        let mut log = GuardLog::new(
            "a1".into(),
            BinaryIdentity {
                version: "1".into(),
                sha256: "x".into(),
            },
        );
        log.snapshot_id = Some(snap);
        persist(&RealOps, td.path(), &log).unwrap();
        for p in [Phase::Snapshotted, Phase::Migrating] {
            advance(&RealOps, td.path(), &mut log, p).unwrap();
        }
        // Migrating: replay must restore, which calls the panicking ops.
        let d = pre_start_with(true, td.path(), PanickingOps)
            .await
            .expect("never None after a failed check");
        assert_eq!(d.marker.error_code, DockCode::GuardInvalid);
        assert_eq!(d.marker.rollback, RollbackState::Failed);
        assert!(!marker_path(td.path()).exists(), "no marker is written");
        let s = docked_state(&d, AuthMode::Capabilities, None);
        assert_eq!(s.health.reason.migrations, MigrationsState::Unknown);
    }

    /// The hook sits behind the guard switch, after the lock and the signal
    /// handlers, and before every business start `serve` makes.
    #[test]
    fn serve_hook_precedes_every_business_start() {
        let src = include_str!("server.rs");
        let serve = &src[src.find("pub async fn serve(").unwrap()..];
        let at = |needle: &str| serve.find(needle).unwrap_or_else(|| panic!("{needle}"));
        let hook = at("docked_startup::pre_start(");
        assert!(serve[hook..hook + 120].contains("guard_enabled()"));
        assert!(at("signal(SignalKind::terminate())") < hook);
        assert!(at("try_acquire_singleton()") < hook);
        for business in [
            "Store::open(&dir.join",
            "spawn_startup_snapshot",
            "ManagedIdoris::new(",
            "host.restore().await",
            "open_memory_base(ephemeral)",
            "with_discovered(Vec::new()",
            "restore_pending_approvals",
            "sweep_orphan_runs",
            ".spawn_scan(",
            "TcpListener::bind",
            "build_router_with_modules(",
        ] {
            assert!(hook < at(business), "{business} must come after the hook");
        }
        // `guard_enabled` is the coordinator switch, nothing else.
        let g = &include_str!("docked_startup.rs");
        assert!(g.contains("    crate::migration_coordinator::ENABLED\n}"));
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
        let l = launch(
            &dir,
            &cancel,
            false,
            "gen-t13",
            ReadyOut::Capture(tx),
            Some(lock),
        );
        let handle = tokio::spawn(serve_docked(
            l,
            docking(DockCode::MigrationFailed, RollbackState::Restored),
        ));
        let line = rx.recv().await.unwrap();
        assert_eq!(line["type"], "docked");
        let port = port_of(&line);
        let rec = daemon_json(&dir);
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

    /// Host-child path: the docked line goes down the private pipe with no
    /// credential, and none is written to `daemon.json` either.
    #[tokio::test]
    async fn host_child_ready_path_carries_no_token() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().to_path_buf();
        let (mut rx, tx) = std::io::pipe().unwrap();
        let writer = crate::host_bootstrap::ReadyWriter::for_test(File::from(
            std::os::fd::OwnedFd::from(tx),
        ));
        let cancel = CancellationToken::new();
        let l = launch(&dir, &cancel, false, "g-host", ReadyOut::Host(writer), None);
        let handle = tokio::spawn(serve_docked(
            l,
            docking(DockCode::Interrupted, RollbackState::NotNeeded),
        ));
        let line = tokio::task::spawn_blocking(move || {
            let mut buf = Vec::new();
            let mut b = [0u8; 1];
            while rx.read(&mut b).unwrap() == 1 && b[0] != b'\n' {
                buf.push(b[0]);
            }
            String::from_utf8(buf).unwrap()
        })
        .await
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "docked");
        assert!(!line.contains("token"));
        assert!(
            !std::fs::read_to_string(dir.join("daemon.json"))
                .unwrap()
                .contains("token")
        );
        cancel.cancel();
        handle.await.unwrap().unwrap();
    }

    /// Q3: with the enrolment channel on, the endpoint is bound, published in
    /// `daemon.json` and on the docked line; off, neither.
    #[tokio::test]
    async fn enrolment_endpoint_follows_the_flag() {
        let (run, line) = start_capture(true, "g", |_| {}).await;
        let sock = line["enroll_endpoint"].as_str().unwrap().to_owned();
        assert!(Path::new(&sock).exists());
        assert_eq!(daemon_json(&run.dir)["enroll_endpoint"], sock);
        run.cancel.cancel();
        run.handle.await.unwrap().unwrap();
        assert!(!Path::new(&sock).exists());

        let (run, line) = start_capture(false, "g", |_| {}).await;
        assert!(line["enroll_endpoint"].is_null());
        assert!(daemon_json(&run.dir).get("enroll_endpoint").is_none());
        assert!(!run.dir.join("enroll.sock").exists());
        run.cancel.cancel();
        run.handle.await.unwrap().unwrap();
    }

    /// M1: a `daemon.json` that cannot be written is a warning, not an exit.
    #[tokio::test]
    async fn unwritable_discovery_file_keeps_serving() {
        let (run, line) = start_capture(false, "g", |d| {
            std::fs::create_dir(d.join("daemon.json")).unwrap()
        })
        .await;
        assert_eq!(line["type"], "docked");
        let port = port_of(&line);
        assert!(
            get(port, "/api/v1/health")
                .await
                .starts_with("HTTP/1.1 503")
        );
        run.cancel.cancel();
        run.handle.await.unwrap().unwrap();
    }

    /// Bind first, publish second: a port that cannot be bound leaves no
    /// `daemon.json` behind.
    #[tokio::test]
    async fn nothing_is_published_before_the_listener_is_bound() {
        let td = tempfile::tempdir().unwrap();
        let taken = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut l = launch(td.path(), &cancel, true, "g", ReadyOut::Capture(tx), None);
        l.port = taken.local_addr().unwrap().port();
        let r = serve_docked(l, docking(DockCode::Interrupted, RollbackState::NotNeeded)).await;
        assert!(r.is_err());
        assert!(!td.path().join("daemon.json").exists());
        assert!(!td.path().join("enroll.sock").exists());
        assert!(rx.try_recv().is_err(), "no docked line either");
    }

    /// Cleanup passes this daemon's generation: a same-pid record of another
    /// generation written meanwhile survives the exit.
    #[tokio::test]
    async fn exit_keeps_another_generations_record() {
        let (run, _) = start_capture(false, "mine", |_| {}).await;
        let other = DaemonState::new_capabilities(1, std::process::id(), "v", "other-gen").unwrap();
        agent24_protocol::state_file::write_in(&run.dir, &other).unwrap();
        run.cancel.cancel();
        run.handle.await.unwrap().unwrap();
        assert_eq!(daemon_json(&run.dir)["generation"], "other-gen");
    }

    /// Q3/M4: what the docked enrol path issues reaches only health and
    /// diagnostics, never a business route.
    #[test]
    fn docked_enrolment_issues_diagnostic_only_tokens() {
        use crate::capabilities::{Operation, Resource, unix_now};
        use crate::enroll_api::EnrollAuthority;
        let store = CapabilityStore::new(String::from("g"));
        let token = docked_authority(store.clone())
            .issue(&crate::capabilities::first_party::test_peer("g"))
            .unwrap();
        let ok = |op| {
            store
                .validate_bearer(&token.secret, op, &Resource::global(), unix_now())
                .is_ok()
        };
        assert!(ok(Operation::DiagnosticRead));
        assert!(!ok(Operation::ModelsRead));
    }

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
