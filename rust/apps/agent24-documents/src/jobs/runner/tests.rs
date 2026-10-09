#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::sync::Notify;

use super::{Attempt, Claim, Mine, settle, spawn};
use crate::state::{AppState, Storage};

// ---- hooks the runner calls in tests ----

/// Injected database failures: (job id, point, times left).
static FAULTS: Mutex<Vec<(String, &'static str, u32)>> = Mutex::new(Vec::new());
/// Jobs whose settles fail for as long as they are listed.
static HOLD_SETTLE: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Jobs whose claim's COMMIT is made to fail for real (a deferred foreign
/// key violation), once.
static BAD_COMMIT: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Calls to `settle`, settles whose update ran, and finished supervisors.
static SETTLES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static SETTLED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static DONE: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Gates before a worker's claim: (job id, entered, release).
type Gate = (String, Arc<Notify>, Arc<Notify>);
static START_GATES: Mutex<Vec<Gate>> = Mutex::new(Vec::new());

pub(crate) fn fault(job_id: &str, point: &str) -> Result<(), sqlx::Error> {
    if point == "settle" {
        SETTLES.lock().unwrap().push(job_id.to_owned());
        if HOLD_SETTLE.lock().unwrap().iter().any(|j| j == job_id) {
            return Err(sqlx::Error::PoolTimedOut);
        }
    }
    let mut faults = FAULTS.lock().unwrap();
    match faults
        .iter_mut()
        .find(|(j, p, n)| j == job_id && *p == point && *n > 0)
    {
        Some(f) => {
            f.2 -= 1;
            Err(sqlx::Error::PoolTimedOut)
        }
        None => Ok(()),
    }
}

pub(crate) fn settle_done(job_id: &str) {
    SETTLED.lock().unwrap().push(job_id.to_owned());
}

pub(crate) fn worker_done(job_id: &str) {
    DONE.lock().unwrap().push(job_id.to_owned());
}

pub(crate) async fn before_start(job_id: &str) {
    let gate = {
        let mut gates = START_GATES.lock().unwrap();
        let at = gates.iter().position(|g| g.0 == job_id);
        at.map(|i| gates.remove(i))
    };
    if let Some((_, entered, release)) = gate {
        entered.notify_one();
        release.notified().await;
    }
}

pub(crate) async fn before_claim_commit(tx: &mut sqlx::SqliteConnection, job_id: &str) {
    let bad = {
        let mut bad = BAD_COMMIT.lock().unwrap();
        let at = bad.iter().position(|j| j == job_id);
        at.map(|i| bad.remove(i)).is_some()
    };
    if bad {
        // A row pointing at no document, with the check deferred to COMMIT.
        sqlx::query("PRAGMA defer_foreign_keys = ON")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
             VALUES ('doc_01K74Z3QJ8V5N2W9RTX6YB4MCD', 1,
                     'sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824', 1, 'x', 'import')",
        )
        .execute(&mut *tx)
        .await
        .unwrap();
    }
}

// ---- helpers ----

/// The state keeps the data dir's lock; tests use the storage.
struct Env {
    _dir: tempfile::TempDir,
    _state: AppState,
    storage: Arc<Storage>,
}

async fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    let storage = state.storage().await.unwrap();
    Env {
        _dir: dir,
        _state: state,
        storage,
    }
}

/// A queued job of a kind no real worker runs, at `attempt`.
async fn add_job(env: &Env, attempt: i64) -> String {
    let id = crate::id::new_id(crate::id::IdKind::Job).unwrap();
    sqlx::query(
        "INSERT INTO jobs (id, kind, status, origin, attempt, input)
         VALUES (?, 'probe', 'queued', '{\"kind\":\"page\"}', ?, '{\"n\":1}')",
    )
    .bind(&id)
    .bind(attempt)
    .execute(env.storage.db.pool())
    .await
    .unwrap();
    id
}

async fn exec(env: &Env, sql: &str) {
    sqlx::query(sql)
        .execute(env.storage.db.pool())
        .await
        .unwrap();
}

type Row = (String, i64, Option<String>);

async fn row(env: &Env, id: &str) -> Row {
    sqlx::query_as("SELECT status, attempt, error ->> '$.code' FROM jobs WHERE id = ?")
        .bind(id)
        .fetch_one(env.storage.db.pool())
        .await
        .unwrap()
}

async fn wait_for(list: &Mutex<Vec<String>>, id: &str, n: usize) {
    let start = Instant::now();
    while list.lock().unwrap().iter().filter(|j| *j == id).count() < n {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{id} never reached {n}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// The row once every worker started for it has finished. It must also
/// read back as a contract-valid job (`jobs::load` refuses one that is not).
async fn finished(env: &Env, id: &str, workers: usize) -> Row {
    wait_for(&DONE, id, workers).await;
    let job = crate::jobs::load(env.storage.db.pool(), id).await;
    assert!(matches!(job, Ok(Some(_))), "{id}: {job:?}");
    row(env, id).await
}

/// Ends the attempt succeeded, through the fenced commit.
async fn succeed(storage: &Storage, claim: &Claim) -> Result<(), sqlx::Error> {
    if let Some(commit) = claim.begin_commit(storage).await? {
        commit.succeed(None, None, None).await?;
    }
    Ok(())
}

type Work = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), sqlx::Error>> + Send>>;

/// Work that succeeds, counting its runs.
fn counted(runs: Arc<AtomicUsize>) -> impl FnOnce(Arc<Storage>, Claim) -> Work {
    move |storage, claim| {
        Box::pin(async move {
            runs.fetch_add(1, SeqCst);
            succeed(&storage, &claim).await
        })
    }
}

fn row_of(status: &str, attempt: i64, code: Option<&str>) -> Row {
    (status.into(), attempt, code.map(Into::into))
}

// ---- tests ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claimed_job_runs_its_work_with_its_input_and_commits_its_success() {
    let env = env().await;
    let id = add_job(&env, 1).await;
    let seen = Arc::new(Mutex::new(None));
    spawn(env.storage.clone(), id.clone(), 1, {
        let seen = seen.clone();
        move |storage, claim| async move {
            *seen.lock().unwrap() = Some((claim.input.clone(), claim.attempt.attempt));
            let mut commit = claim.begin_commit(&storage).await?.unwrap();
            sqlx::query("INSERT INTO oplog (op, origin) VALUES ('test', 'page')")
                .execute(commit.conn())
                .await?;
            commit
                .succeed(
                    None,
                    None,
                    Some(&json!({ "stage": "s", "done": 1, "total": 1, "unit": "u" })),
                )
                .await
        }
    });
    assert_eq!(finished(&env, &id, 1).await, row_of("succeeded", 1, None));
    assert_eq!(*seen.lock().unwrap(), Some((Some(json!({ "n": 1 })), 1)));
    let ops: i64 = sqlx::query_scalar("SELECT count(*) FROM oplog")
        .fetch_one(env.storage.db.pool())
        .await
        .unwrap();
    assert_eq!(ops, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_queued_job_at_the_given_attempt_is_claimed() {
    let env = env().await;
    let runs = Arc::new(AtomicUsize::new(0));
    let other_attempt = add_job(&env, 2).await;
    spawn(
        env.storage.clone(),
        other_attempt.clone(),
        1,
        counted(runs.clone()),
    );
    let cancelled = add_job(&env, 1).await;
    exec(
        &env,
        &format!("UPDATE jobs SET status = 'cancelled' WHERE id = '{cancelled}'"),
    )
    .await;
    spawn(
        env.storage.clone(),
        cancelled.clone(),
        1,
        counted(runs.clone()),
    );
    assert_eq!(
        finished(&env, &other_attempt, 1).await,
        row_of("queued", 2, None)
    );
    assert_eq!(finished(&env, &cancelled, 1).await.0, "cancelled");
    assert_eq!(runs.load(SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn however_the_work_ends_the_claimed_attempt_is_settled() {
    let env = env().await;
    let db_error = add_job(&env, 1).await;
    spawn(env.storage.clone(), db_error.clone(), 1, |_, _| async {
        Err(sqlx::Error::PoolTimedOut)
    });
    assert_eq!(
        finished(&env, &db_error, 1).await,
        row_of("failed", 1, Some("storage_unavailable"))
    );
    let panicked = add_job(&env, 1).await;
    spawn(env.storage.clone(), panicked.clone(), 1, |_, _| async {
        panic!("injected")
    });
    assert_eq!(finished(&env, &panicked, 1).await.0, "failed");
    // Returning without a result is no way to leave a job running.
    let early = add_job(&env, 1).await;
    spawn(env.storage.clone(), early.clone(), 1, |_, _| async {
        Ok(())
    });
    assert_eq!(
        finished(&env, &early, 1).await,
        row_of("failed", 1, Some("storage_unavailable"))
    );
    // A job being cancelled ends cancelled, however the work ends.
    for fails in [true, false] {
        let id = add_job(&env, 1).await;
        spawn(
            env.storage.clone(),
            id.clone(),
            1,
            move |storage, claim| async move {
                sqlx::query("UPDATE jobs SET status = 'cancelling' WHERE id = ?")
                    .bind(&claim.attempt.job_id)
                    .execute(storage.db.pool())
                    .await?;
                if fails {
                    Err(sqlx::Error::PoolTimedOut)
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(
            finished(&env, &id, 1).await,
            row_of("cancelled", 1, Some("cancelled")),
            "{fails}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commit_finds_a_cancel_or_a_newer_attempt_and_writes_nothing() {
    let env = env().await;
    for (name, change) in [
        (
            "cancel",
            "UPDATE jobs SET status = 'cancelling' WHERE id = '{id}'",
        ),
        (
            "newer attempt",
            "UPDATE jobs SET status = 'queued', attempt = 2 WHERE id = '{id}'",
        ),
    ] {
        let id = add_job(&env, 1).await;
        let (running, go) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        let committed = Arc::new(AtomicUsize::new(0));
        spawn(env.storage.clone(), id.clone(), 1, {
            let (running, go, committed) = (running.clone(), go.clone(), committed.clone());
            move |storage, claim| async move {
                running.notify_one();
                go.notified().await;
                if let Some(mut commit) = claim.begin_commit(&storage).await? {
                    sqlx::query("INSERT INTO oplog (op, origin) VALUES ('result', 'page')")
                        .execute(commit.conn())
                        .await?;
                    commit.succeed(None, None, None).await?;
                    committed.fetch_add(1, SeqCst);
                }
                Ok(())
            }
        });
        tokio::time::timeout(Duration::from_secs(5), running.notified())
            .await
            .unwrap();
        exec(&env, &change.replace("{id}", &id)).await;
        go.notify_one();
        let after = finished(&env, &id, 1).await;
        let want = if name == "cancel" {
            row_of("cancelled", 1, Some("cancelled"))
        } else {
            row_of("queued", 2, None)
        };
        assert_eq!(after, want, "{name}");
        assert_eq!(committed.load(SeqCst), 0, "{name}");
        let ops: i64 = sqlx::query_scalar("SELECT count(*) FROM oplog WHERE op = 'result'")
            .fetch_one(env.storage.db.pool())
            .await
            .unwrap();
        assert_eq!(ops, 0, "{name}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claim_that_fails_or_does_not_commit_runs_no_work_and_is_settled() {
    let env = env().await;
    let runs = Arc::new(AtomicUsize::new(0));
    let faulty = add_job(&env, 1).await;
    FAULTS.lock().unwrap().push((faulty.clone(), "claim", 1));
    spawn(
        env.storage.clone(),
        faulty.clone(),
        1,
        counted(runs.clone()),
    );
    assert_eq!(
        finished(&env, &faulty, 1).await,
        row_of("failed", 1, Some("storage_unavailable"))
    );
    // A real COMMIT failure: the claim is rolled back, and settled as unclaimed.
    let bad = add_job(&env, 1).await;
    BAD_COMMIT.lock().unwrap().push(bad.clone());
    spawn(env.storage.clone(), bad.clone(), 1, counted(runs.clone()));
    assert_eq!(
        finished(&env, &bad, 1).await,
        row_of("failed", 1, Some("storage_unavailable"))
    );
    let revisions: i64 = sqlx::query_scalar("SELECT count(*) FROM revisions")
        .fetch_one(env.storage.db.pool())
        .await
        .unwrap();
    assert_eq!(revisions, 0, "the failed commit was rolled back");
    assert_eq!(runs.load(SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settling_retries_until_the_database_answers() {
    let env = env().await;
    let id = add_job(&env, 1).await;
    FAULTS.lock().unwrap().push((id.clone(), "settle", 3));
    spawn(env.storage.clone(), id.clone(), 1, |_, _| async {
        Err(sqlx::Error::PoolTimedOut)
    });
    assert_eq!(finished(&env, &id, 1).await.0, "failed");
    assert_eq!(
        SETTLES.lock().unwrap().iter().filter(|j| **j == id).count(),
        4
    );
}

#[tokio::test]
async fn settling_is_fenced_by_attempt_and_by_claim() {
    let env = env().await;
    let id = add_job(&env, 2).await;
    let at = |attempt, mine| Attempt {
        job_id: id.clone(),
        attempt,
        mine,
    };
    // Attempt 1 cannot settle attempt 2, queued or running.
    settle(
        &env.storage,
        &at(1, Mine::Unclaimed),
        "storage_unavailable",
        "x",
    )
    .await
    .unwrap();
    assert_eq!(row(&env, &id).await.0, "queued");
    exec(&env, "UPDATE jobs SET status = 'running'").await;
    settle(
        &env.storage,
        &at(1, Mine::Claimed),
        "storage_unavailable",
        "x",
    )
    .await
    .unwrap();
    assert_eq!(row(&env, &id).await.0, "running");
    // A worker of attempt 2 that never claimed cannot settle the claim.
    settle(
        &env.storage,
        &at(2, Mine::Unclaimed),
        "storage_unavailable",
        "x",
    )
    .await
    .unwrap();
    assert_eq!(row(&env, &id).await.0, "running");
    // The claimant can.
    settle(
        &env.storage,
        &at(2, Mine::Claimed),
        "storage_unavailable",
        "x",
    )
    .await
    .unwrap();
    assert_eq!(row(&env, &id).await.0, "failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_supervisor_that_wakes_late_leaves_a_later_attempt_alone() {
    let env = env().await;
    let id = add_job(&env, 1).await;
    // Attempt 1: the claim fails, and settling keeps failing until released.
    HOLD_SETTLE.lock().unwrap().push(id.clone());
    FAULTS.lock().unwrap().push((id.clone(), "claim", 1));
    spawn(env.storage.clone(), id.clone(), 1, |_, _| async { Ok(()) });
    wait_for(&SETTLES, &id, 1).await;
    // Meanwhile the job is cancelled and retried; attempt 2 is claimed and
    // its work held running.
    exec(&env, "UPDATE jobs SET status = 'queued', attempt = 2").await;
    let (running, finish) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    spawn(env.storage.clone(), id.clone(), 2, {
        let (running, finish) = (running.clone(), finish.clone());
        move |storage, claim| async move {
            running.notify_one();
            finish.notified().await;
            succeed(&storage, &claim).await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), running.notified())
        .await
        .unwrap();
    // Now attempt 1's supervisor gets its settle through.
    HOLD_SETTLE.lock().unwrap().retain(|j| *j != id);
    wait_for(&SETTLED, &id, 1).await;
    assert_eq!(row(&env, &id).await.0, "running");
    finish.notify_one();
    assert_eq!(finished(&env, &id, 2).await, row_of("succeeded", 2, None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_started_twice_runs_once() {
    let env = env().await;
    let id = add_job(&env, 1).await;
    let runs = Arc::new(AtomicUsize::new(0));
    // The first worker stops before its claim; a second claims and is held
    // running while the first goes on and tries to claim too.
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    START_GATES
        .lock()
        .unwrap()
        .push((id.clone(), entered.clone(), release.clone()));
    spawn(env.storage.clone(), id.clone(), 1, counted(runs.clone()));
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let (running, finish) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    spawn(env.storage.clone(), id.clone(), 1, {
        let (running, finish, runs) = (running.clone(), finish.clone(), runs.clone());
        move |storage, claim| async move {
            runs.fetch_add(1, SeqCst);
            running.notify_one();
            finish.notified().await;
            succeed(&storage, &claim).await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), running.notified())
        .await
        .unwrap();
    release.notify_one();
    wait_for(&DONE, &id, 1).await;
    assert_eq!(
        row(&env, &id).await.0,
        "running",
        "the first worker left it alone"
    );
    finish.notify_one();
    assert_eq!(finished(&env, &id, 2).await, row_of("succeeded", 1, None));
    assert_eq!(runs.load(SeqCst), 1);
}
