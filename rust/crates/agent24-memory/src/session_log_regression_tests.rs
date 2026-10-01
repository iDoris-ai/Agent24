//! Regression coverage and a narrow query-row probe for `SessionLog`.
#![allow(clippy::expect_used)]

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::{
    KvStore,
    event::{EventLog, EventStore, MemEvent, Origin, Scope, Trust},
    session::CanonicalSession,
};
use agent24_models::Msg;
use sqlx::{Connection, sqlite::SqliteConnectOptions};
use tokio::sync::Notify;

use super::{ImportOutcome, SessionLog};

/// Test hook called after a session-log read. The first observation can be
/// held at a barrier so another SQLite connection can exercise lock ordering.
pub(super) struct Probe {
    rows: AtomicUsize,
    gated: AtomicBool,
    hit: Notify,
    release: Notify,
}

impl Probe {
    pub(super) fn new() -> Self {
        Self {
            rows: AtomicUsize::new(0),
            gated: AtomicBool::new(false),
            hit: Notify::new(),
            release: Notify::new(),
        }
    }

    fn ungated() -> Self {
        Self {
            rows: AtomicUsize::new(0),
            gated: AtomicBool::new(true),
            hit: Notify::new(),
            release: Notify::new(),
        }
    }

    pub(super) async fn observe(&self, rows: usize) {
        self.rows.fetch_add(rows, Ordering::SeqCst);
        if !self.gated.swap(true, Ordering::SeqCst) {
            self.hit.notify_one();
            self.release.notified().await;
        }
    }

    async fn wait_hit(&self) {
        self.hit.notified().await;
    }

    fn release(&self) {
        self.release.notify_one();
    }

    fn rows(&self) -> usize {
        self.rows.load(Ordering::SeqCst)
    }

    fn reset_rows(&self) {
        self.rows.store(0, Ordering::SeqCst);
    }
}

fn origin(trust: Trust) -> Origin {
    Origin {
        source: "regression".into(),
        trust,
    }
}

fn assistant(value: &str) -> Msg {
    Msg::assistant(Some(value.into()), vec![])
}

fn instrument(log: &mut SessionLog) -> Arc<Probe> {
    let probe = Arc::new(Probe::new());
    log.probe = Some(probe.clone());
    probe
}

async fn append(
    log: &SessionLog,
    owner: &str,
    session: &str,
    turn: u64,
) -> crate::Result<super::TurnIds> {
    log.append_turn(
        owner,
        session,
        turn,
        &Msg::user("same input"),
        origin(Trust::UserSaid),
        &assistant("answer"),
        origin(Trust::Model),
    )
    .await
}

async fn wait_for_probe(probe: &Probe) {
    tokio::time::timeout(std::time::Duration::from_secs(5), probe.wait_hit())
        .await
        .expect("the operation reached its first read barrier");
}

async fn concurrent_writer_case(operation: &'static str) -> crate::Result<()> {
    let dir = tempfile::tempdir().expect("temporary database directory");
    let path = dir.path().join("session-log.sqlite");
    let first = KvStore::open(&path).await?;
    let second = KvStore::open(&path).await?;
    let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&first.pool)
        .await?;
    assert_eq!(journal.to_ascii_lowercase(), "wal");

    let session_a = format!("a-{operation}");
    let session_b = format!("b-{operation}");
    let mut log_a = first.session_log();
    let probe = instrument(&mut log_a);
    let mut legacy_a = CanonicalSession::new(session_a.clone());
    legacy_a.recent.push(Msg::user("legacy content"));
    let task_session_a = session_a.clone();
    let task = tokio::spawn(async move {
        match operation {
            "append" => append(&log_a, "a", &task_session_a, 0).await.map(|_| ()),
            "import" => log_a.import_legacy("a", &legacy_a).await.map(|_| ()),
            "summary" => log_a
                .append_summary("a", &task_session_a, "s", 0)
                .await
                .map(|_| ()),
            _ => unreachable!(),
        }
    });
    wait_for_probe(&probe).await;
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(false)
        .busy_timeout(std::time::Duration::ZERO);
    let mut conn = sqlx::SqliteConnection::connect_with(&opts).await?;
    let lock_attempt = sqlx::query("BEGIN IMMEDIATE").execute(&mut conn).await;
    let busy = match lock_attempt {
        Ok(_) => {
            let event = MemEvent::new(
                format!("b-{operation}"),
                Scope::owner("b").with_session(&session_b),
                "message",
                serde_json::to_value(Msg::user("writer"))?,
                origin(Trust::UserSaid),
            );
            EventLog::append_tx(&mut conn, &event).await?;
            sqlx::query("COMMIT").execute(&mut conn).await?;
            conn.close().await?;
            false
        }
        Err(error) => error
            .as_database_error()
            .and_then(|db| db.code())
            .is_some_and(|code| code == "5" || code == "SQLITE_BUSY"),
    };
    probe.release();
    let a_result = task.await.expect("session-log task did not panic");
    assert!(
        a_result.is_ok(),
        "A should commit after its read barrier: {a_result:?}"
    );
    assert!(
        busy,
        "{operation} must hold the writer lock before its first read"
    );
    let a_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM mem_events WHERE scope_owner='a' AND scope_session=?",
    )
    .bind(&session_a)
    .fetch_one(&first.pool)
    .await?;
    let expected_a = match operation {
        "append" | "import" => 2,
        "summary" => 1,
        _ => unreachable!(),
    };
    assert_eq!(a_rows, expected_a, "{operation} result must be persisted");

    let unrelated = second.session_log();
    let outcome = match operation {
        "append" => append(&unrelated, "b", &session_b, 0).await.map(|_| ()),
        "import" => {
            let mut legacy_b = CanonicalSession::new(session_b.clone());
            legacy_b.recent.push(Msg::user("writer"));
            unrelated.import_legacy("b", &legacy_b).await.map(|_| ())
        }
        "summary" => unrelated
            .append_summary("b", &session_b, "s", 0)
            .await
            .map(|_| ()),
        _ => unreachable!(),
    };
    outcome?;
    let b_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM mem_events WHERE scope_owner='b' AND scope_session=?",
    )
    .bind(&session_b)
    .fetch_one(&second.pool)
    .await?;
    let expected_b = if operation == "summary" { 1 } else { 2 };
    assert_eq!(
        b_rows, expected_b,
        "B must persist its independent operation"
    );
    Ok(())
}

#[tokio::test]
async fn append_takes_immediate_lock_before_reading() -> crate::Result<()> {
    concurrent_writer_case("append").await
}

#[tokio::test]
async fn import_takes_immediate_lock_before_reading() -> crate::Result<()> {
    concurrent_writer_case("import").await
}

#[tokio::test]
async fn summary_takes_immediate_lock_before_reading() -> crate::Result<()> {
    concurrent_writer_case("summary").await
}

#[tokio::test]
async fn covered_history_reads_only_summary_tail_and_targeted_rows() -> crate::Result<()> {
    let kv = KvStore::open_memory().await?;
    let scope = Scope::owner("o").with_session("s");
    let mut conn = kv.pool.acquire().await?;
    for index in 0..2_000_u64 {
        let event = MemEvent::new(
            format!("covered-{index}"),
            scope.clone(),
            "message",
            serde_json::to_value(Msg::user("x"))?,
            origin(Trust::UserSaid),
        );
        EventLog::append_tx(&mut conn, &event).await?;
    }
    drop(conn);
    let mut log = kv.session_log();
    let covered = sqlx::query_scalar::<_, i64>(
        "SELECT MAX(seq) FROM mem_events WHERE scope_owner='o' AND scope_session='s'",
    )
    .fetch_one(&kv.pool)
    .await?;
    log.append_summary("o", "s", "covered", covered).await?;
    for (id, message) in [
        ("tail-u", Msg::user("tail")),
        ("tail-a", assistant("answer")),
    ] {
        kv.events()
            .append(&MemEvent::new(
                id,
                scope.clone(),
                "message",
                serde_json::to_value(message)?,
                origin(Trust::UserSaid),
            ))
            .await?;
    }
    let probe = Arc::new(Probe::ungated());
    log.probe = Some(probe.clone());

    probe.reset_rows();
    let view = log.load_view("o", "s").await?;
    assert_eq!(view.tail.len(), 2);
    assert_eq!(
        probe.rows(),
        3,
        "one summary row and two uncovered tail rows"
    );

    probe.reset_rows();
    append(&log, "o", "s", 3_000).await?;
    assert_eq!(probe.rows(), 0, "a new turn lookup reads no prior bodies");
    probe.reset_rows();
    append(&log, "o", "s", 3_000).await?;
    assert_eq!(
        probe.rows(),
        2,
        "retry lookup reads only the identified turn"
    );

    probe.reset_rows();
    let legacy = CanonicalSession::new("s");
    assert!(matches!(
        log.import_legacy("o", &legacy).await?,
        ImportOutcome::AlreadyImported
    ));
    assert_eq!(probe.rows(), 1, "import check reads only the EXISTS result");
    Ok(())
}
