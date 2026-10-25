#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::engine::{Engine, LayerFailure, Layers, Parse, WAIT};
use crate::events::Events;
use crate::text_layer::EngineRef;

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00";
const SHORT: Duration = Duration::from_millis(300);

/// A text engine that never finishes reading.
struct Stuck;

impl Engine for Stuck {
    fn engine(&self) -> EngineRef {
        EngineRef {
            id: "apple-pdfkit".into(),
            version: "26.6.2".into(),
        }
    }
    fn config(&self) -> Value {
        json!({ "test": 3 })
    }
    fn parse(&self, _content: &str, _media: &str, _path: &Path) -> Parse {
        Box::pin(std::future::pending())
    }
}

/// A text engine that never finishes reading, and takes a while to put a
/// stopped read down.
struct Slow;

/// Held by [`Slow`]'s read: dropping it is the read being put down.
struct PutDown;

impl Drop for PutDown {
    fn drop(&mut self) {
        std::thread::sleep(Duration::from_secs(1));
    }
}

impl Engine for Slow {
    fn engine(&self) -> EngineRef {
        Stuck.engine()
    }
    fn config(&self) -> Value {
        Stuck.config()
    }
    fn parse(&self, _content: &str, _media: &str, _path: &Path) -> Parse {
        let put_down = PutDown;
        Box::pin(async move {
            let _held = put_down;
            std::future::pending().await
        })
    }
}

struct Env {
    _dir: tempfile::TempDir,
    state: AppState,
    storage: Arc<Storage>,
    content: String,
}

/// A PDF r1, read by an engine that never finishes.
async fn env() -> Env {
    env_with(Arc::new(Stuck)).await
}

async fn env_with(engine: Arc<dyn Engine>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let layers = Layers::new(Some(engine));
    let state = AppState::open_serving(dir.path(), Events::default(), layers).await;
    let storage = state.storage().await.unwrap();
    let blob = storage.blobs.put_bytes(b"%PDF-1.7 stuck").unwrap();
    for sql in [
        format!(
            "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{DOC}', 't', 'application/pdf', 1)"
        ),
        format!(
            "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
             VALUES ('{DOC}', 1, '{}', {}, 'application/pdf', 'import')",
            blob.sha256, blob.size
        ),
    ] {
        sqlx::query(&sql).execute(storage.db.pool()).await.unwrap();
    }
    Env {
        _dir: dir,
        state,
        storage,
        content: blob.sha256,
    }
}

fn job_id(n: u8) -> String {
    format!("job_01K75A0B1C2D3E4F5G6H7J8K{n:02}")
}

/// An extract job in `status` at `attempt`, made by the page.
async fn job(env: &Env, id: &str, status: &str, attempt: i64) {
    sqlx::query(
        "INSERT INTO jobs (id, kind, document_id, revision, status, attempt, origin)
         VALUES (?, 'extract', ?, 1, ?, ?, '{\"kind\":\"page\"}')",
    )
    .bind(id)
    .bind(DOC)
    .bind(status)
    .bind(attempt)
    .execute(env.storage.db.pool())
    .await
    .unwrap();
}

/// A call of `operation` accepted for the job's `attempt`.
async fn accept(env: &Env, id: &str, attempt: i64, operation: &str) {
    let run = Run {
        run_id: "run-1".into(),
        tool_call_id: format!("call-{id}-{attempt}"),
    };
    let accepted = Accepted {
        operation: operation.into(),
        authorization_ref: Some("ga1:x".into()),
    };
    let mut conn = env.storage.db.pool().acquire().await.unwrap();
    record(&mut conn, id, attempt, &run, &accepted)
        .await
        .unwrap();
}

async fn status(env: &Env, id: &str) -> (String, Option<Value>) {
    let (status, error): (String, Option<String>) =
        sqlx::query_as("SELECT status, error FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_one(env.storage.db.pool())
            .await
            .unwrap();
    (status, error.map(|e| serde_json::from_str(&e).unwrap()))
}

/// What a worker does at its next safe point: a cancelling job ends cancelled.
async fn settle_once_cancelling(storage: Arc<Storage>, id: String) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let done = sqlx::query(
                "UPDATE jobs SET status = 'cancelled' WHERE id = ? AND status = 'cancelling'",
            )
            .bind(&id)
            .execute(storage.db.pool())
            .await
            .unwrap();
            if done.rows_affected() == 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the attempt was never asked to stop");
}

/// A read of the layer, waiting on its build, asked for `by`.
fn reading(env: &Env, by: Requester) -> tokio::task::JoinHandle<Result<String, LayerFailure>> {
    let (layers, storage, content) = (
        env.state.layers().clone(),
        env.storage.clone(),
        env.content.clone(),
    );
    tokio::spawn(async move {
        layers
            .layer(&storage, &content, "application/pdf", WAIT, by)
            .await
    })
}

/// Tells of `operation`'s end until `read` ends: a notice before the read's
/// build is asked for has nothing of it to stop.
async fn stop_until_ended(
    env: &Env,
    operation: &str,
    read: tokio::task::JoinHandle<Result<String, LayerFailure>>,
) -> Result<String, LayerFailure> {
    let start = std::time::Instant::now();
    while !read.is_finished() {
        stop(&env.state, operation, SHORT).await;
        assert!(start.elapsed() < Duration::from_secs(5), "never stopped");
    }
    read.await.unwrap()
}

/// K1-06a-grant-path §2.5: the attempts the operation's calls were accepted
/// for are cancelled; a page's job, an earlier attempt's call and another
/// operation's are not.
#[tokio::test]
async fn queued_attempts_its_calls_were_accepted_for_are_cancelled() {
    let env = env().await;
    let ids: Vec<String> = (1..=4).map(job_id).collect();
    for id in &ids {
        job(&env, id, "queued", 1).await;
    }
    accept(&env, &ids[0], 1, "extract").await;
    // Read as a set: a repeated row, and one with no reference, count once.
    accept(&env, &ids[0], 1, "extract").await;
    let run = Run {
        run_id: "run-1".into(),
        tool_call_id: "call-unnamed".into(),
    };
    let unnamed = Accepted {
        operation: "extract".into(),
        authorization_ref: None,
    };
    let mut conn = env.storage.db.pool().acquire().await.unwrap();
    record(&mut conn, &ids[0], 1, &run, &unnamed).await.unwrap();
    drop(conn);
    // ids[1] is the page's own. ids[2]'s call was for an earlier attempt:
    sqlx::query("UPDATE jobs SET attempt = 2 WHERE id = ?")
        .bind(&ids[2])
        .execute(env.storage.db.pool())
        .await
        .unwrap();
    accept(&env, &ids[2], 1, "extract").await;
    accept(&env, &ids[3], 1, "other").await;
    assert!(stop(&env.state, "extract", SHORT).await);
    let cancelled = json!({ "code": "cancelled", "message": "cancelled: its authorization ended" });
    assert_eq!(
        status(&env, &ids[0]).await,
        ("cancelled".into(), Some(cancelled))
    );
    for id in &ids[1..] {
        assert_eq!(status(&env, id).await.0, "queued");
    }
    // The whole module's end stops every operation's.
    assert!(stop(&env.state, ALL, SHORT).await);
    assert_eq!(status(&env, &ids[3]).await.0, "cancelled");
    assert_eq!(status(&env, &ids[1]).await.0, "queued");
    assert_eq!(status(&env, &ids[2]).await.0, "queued");
}

/// A running attempt is asked to stop; the answer waits for it to, and says
/// so only once it has.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_attempt_stops_before_the_answer_says_so() {
    let env = env().await;
    let id = job_id(1);
    job(&env, &id, "running", 1).await;
    accept(&env, &id, 1, "extract").await;
    assert!(!stop(&env.state, "extract", SHORT).await);
    assert_eq!(status(&env, &id).await, ("cancelling".into(), None));
    // Its settle will say why.
    let marked: Vec<(String, i64)> = sqlx::query_as("SELECT job_id, attempt FROM job_revocations")
        .fetch_all(env.storage.db.pool())
        .await
        .unwrap();
    assert_eq!(marked, vec![(id.clone(), 1)]);
    let worker = tokio::spawn(settle_once_cancelling(env.storage.clone(), id.clone()));
    assert!(stop(&env.state, "extract", Duration::from_secs(5)).await);
    worker.await.unwrap();
    assert_eq!(status(&env, &id).await.0, "cancelled");
}

/// The builds the operation's calls asked for stop, and those its accepted
/// attempts asked for; another operation's go on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_builds_its_calls_and_attempts_asked_for_stop() {
    let env = env().await;
    let read = reading(&env, Requester::Tool("find".into()));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(stop(&env.state, "read_range", SHORT).await);
    assert!(!read.is_finished(), "another operation's build stopped");
    assert_eq!(
        stop_until_ended(&env, "find", read).await,
        Err(LayerFailure::Stopped)
    );
    // The whole module's end stops any operation's.
    let read = reading(&env, Requester::Tool("list".into()));
    assert_eq!(
        stop_until_ended(&env, ALL, read).await,
        Err(LayerFailure::Stopped)
    );
    let id = job_id(1);
    job(&env, &id, "running", 1).await;
    accept(&env, &id, 1, "extract").await;
    let by = Requester::Job {
        id: id.clone(),
        attempt: 1,
    };
    let read = reading(&env, by);
    tokio::spawn(settle_once_cancelling(env.storage.clone(), id));
    assert_eq!(
        stop_until_ended(&env, "extract", read).await,
        Err(LayerFailure::Stopped)
    );
}

async fn notify(env: &Env, body: &str) -> StatusCode {
    let req = Request::post("/_a24/authorization/revoked")
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap();
    crate::router(env.state.clone())
        .oneshot(req)
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn the_notice_answers_204_once_stopped_and_400_to_what_is_not_one() {
    let env = env().await;
    let id = job_id(1);
    job(&env, &id, "queued", 1).await;
    accept(&env, &id, 1, "extract").await;
    let notice = r#"{"operation":"extract","revoked_at":"2026-10-24T00:00:00Z"}"#;
    assert_eq!(notify(&env, notice).await, StatusCode::NO_CONTENT);
    assert_eq!(status(&env, &id).await.0, "cancelled");
    let long = format!(r#"{{"operation":"{}"}}"#, "x".repeat(65));
    for bad in ["", "{}", r#"{"operation":""}"#, long.as_str()] {
        assert_eq!(notify(&env, bad).await, StatusCode::BAD_REQUEST, "{bad}");
    }
}

/// 0007: an acceptance is written once and kept; what it names is bounded.
#[tokio::test]
async fn acceptances_are_append_only_and_bounded() {
    let env = env().await;
    let id = job_id(1);
    job(&env, &id, "queued", 1).await;
    accept(&env, &id, 1, "extract").await;
    let pool = env.storage.db.pool();
    for sql in [
        "UPDATE job_acceptances SET operation = 'find'",
        "DELETE FROM job_acceptances",
    ] {
        let e = sqlx::query(sql).execute(pool).await.unwrap_err();
        assert!(e.to_string().contains("append-only"), "{e}");
    }
    let run = Run {
        run_id: "run-1".into(),
        tool_call_id: "call-2".into(),
    };
    let mut conn = pool.acquire().await.unwrap();
    for (operation, grant) in [
        ("x".repeat(65), Some("ga1:x".to_owned())),
        (String::new(), None),
        ("extract".to_owned(), Some("g".repeat(257))),
        ("extract".to_owned(), Some(String::new())),
    ] {
        let accepted = Accepted {
            operation,
            authorization_ref: grant,
        };
        assert!(record(&mut conn, &id, 1, &run, &accepted).await.is_err());
    }
}

/// Work not stopped within the wait is not said to be: 503, which the
/// kernel records as a stop it could not confirm.
#[tokio::test]
async fn the_notice_answers_503_while_the_work_goes_on() {
    let env = env().await;
    let id = job_id(1);
    job(&env, &id, "running", 1).await;
    accept(&env, &id, 1, "extract").await;
    let notice = r#"{"operation":"extract","revoked_at":"2026-10-24T00:00:00Z"}"#;
    assert_eq!(notify(&env, notice).await, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(status(&env, &id).await.0, "cancelling");
}

/// A build an attempt asked for after an earlier notice already ended the
/// attempt is stopped by the next: it is still that attempt's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_an_ended_attempt_asked_for_stops_too() {
    let env = env().await;
    let id = job_id(1);
    job(&env, &id, "cancelled", 1).await;
    accept(&env, &id, 1, "extract").await;
    let read = reading(&env, Requester::Job { id, attempt: 1 });
    assert_eq!(
        stop_until_ended(&env, "extract", read).await,
        Err(LayerFailure::Stopped)
    );
}

/// Waits until the job's attempt asks for a build.
async fn asking(env: &Env, id: &str, attempt: i64) {
    asking_or(env, id, attempt, || false).await;
}

/// [`asking`], or until `done`: a notice already polling may stop the build,
/// and so end its asking, before it is seen.
async fn asking_or(env: &Env, id: &str, attempt: i64, done: impl Fn() -> bool) {
    let start = std::time::Instant::now();
    while !done()
        && !env
            .state
            .layers()
            .job_requesters()
            .contains(&(id.to_owned(), attempt))
    {
        assert!(start.elapsed() < Duration::from_secs(5), "never asked");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A build an attempt asks for after the notice, before it stops, is
/// stopped before the answer: one notice, as the kernel sends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_asked_for_while_the_attempt_stops_stops_before_the_answer() {
    let env = env().await;
    let id = job_id(1);
    job(&env, &id, "running", 1).await;
    accept(&env, &id, 1, "extract").await;
    let notice = {
        let state = env.state.clone();
        tokio::spawn(async move { stop(&state, "extract", Duration::from_secs(5)).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let read = reading(
        &env,
        Requester::Job {
            id: id.clone(),
            attempt: 1,
        },
    );
    asking_or(&env, &id, 1, || read.is_finished()).await;
    settle_once_cancelling(env.storage.clone(), id).await;
    assert!(notice.await.unwrap());
    let read = tokio::time::timeout(Duration::from_secs(2), read).await;
    assert_eq!(
        read.expect("the build goes on").unwrap(),
        Err(LayerFailure::Stopped)
    );
}

/// A build an earlier attempt the operation's call was accepted for asked
/// for stops, though the job's current attempt is the user's own retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_an_earlier_accepted_attempt_asked_for_stops() {
    let env = env().await;
    let id = job_id(1);
    job(&env, &id, "queued", 2).await;
    accept(&env, &id, 1, "extract").await;
    let read = reading(
        &env,
        Requester::Job {
            id: id.clone(),
            attempt: 1,
        },
    );
    asking(&env, &id, 1).await;
    assert!(stop(&env.state, "extract", Duration::from_secs(5)).await);
    assert_eq!(read.await.unwrap(), Err(LayerFailure::Stopped));
    assert_eq!(
        status(&env, &id).await.0,
        "queued",
        "the retry is the user's"
    );
}

/// Storage that does not answer does not hold the answer past its wait,
/// and the calls' builds, which need none, stop all the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_locked_store_answers_not_stopped_within_the_wait() {
    let env = env().await;
    let read = reading(&env, Requester::Tool("find".into()));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut lock = env.storage.db.pool().acquire().await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *lock)
        .await
        .unwrap();
    let start = std::time::Instant::now();
    assert!(!stop(&env.state, "find", SHORT).await);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(read.await.unwrap(), Err(LayerFailure::Stopped));
    sqlx::query("ROLLBACK").execute(&mut *lock).await.unwrap();
}

/// The answer says the builds stopped only once they have: a build slow to
/// put its read down holds the answer until it is down.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_answer_waits_for_the_builds_to_be_down() {
    let env = env_with(Arc::new(Slow)).await;
    let read = reading(&env, Requester::Tool("find".into()));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(stop(&env.state, "find", Duration::from_secs(5)).await);
    let read = tokio::time::timeout(Duration::from_millis(300), read).await;
    assert_eq!(
        read.expect("answered before the build was down").unwrap(),
        Err(LayerFailure::Stopped)
    );
}

/// A call in flight when its operation's authorization ends holds the
/// answer until it ends, and is told its end; a call of another
/// operation, or one begun after, is not. The whole module's end is every
/// operation's.
#[tokio::test]
async fn the_answer_waits_for_the_calls_in_flight_and_they_give_nothing() {
    let env = env().await;
    let calls = env.state.calls();
    let (find, list) = (calls.begin("find"), calls.begin("list"));
    assert!(!stop(&env.state, "find", SHORT).await, "a call in flight");
    assert!(find.ended());
    assert!(!list.ended());
    assert!(!calls.begin("find").ended(), "begun after the end");
    drop(find);
    assert!(stop(&env.state, "find", SHORT).await);
    assert!(!stop(&env.state, ALL, SHORT).await);
    assert!(list.ended());
    drop(list);
    assert!(stop(&env.state, ALL, SHORT).await);
}

/// A claim its call commits after the notice's first pass, while the call
/// is still in flight, is cancelled before the answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claim_its_call_commits_after_the_notice_is_cancelled() {
    let env = env().await;
    let call = env.state.calls().begin("extract");
    let notice = {
        let state = env.state.clone();
        tokio::spawn(async move { stop(&state, "extract", Duration::from_secs(5)).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    let id = job_id(1);
    job(&env, &id, "queued", 1).await;
    accept(&env, &id, 1, "extract").await;
    drop(call);
    assert!(notice.await.unwrap());
    assert_eq!(status(&env, &id).await.0, "cancelled");
}

/// A build a call in flight asks for after the notice's first pass, its
/// call then answering, is stopped before the answer: one notice, as the
/// kernel sends (#1206 gate 2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_a_call_asks_for_after_the_first_pass_stops_before_the_answer() {
    let env = env().await;
    let call = env.state.calls().begin("find");
    let notice = {
        let state = env.state.clone();
        tokio::spawn(async move { stop(&state, "find", Duration::from_secs(5)).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    let read = reading(&env, Requester::Tool("find".into()));
    let start = std::time::Instant::now();
    while !read.is_finished() {
        assert!(start.elapsed() < Duration::from_secs(5), "never stopped");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(call.finish());
    assert!(notice.await.unwrap());
    assert!(matches!(read.await.unwrap(), Err(LayerFailure::Stopped)));
}

/// A call's answer is decided as it leaves the ledger: an end after it
/// does not count it. Its claim's hold counts as begun when the call did,
/// and stays in flight after the call answers (#1206 gate 1).
#[test]
fn a_call_answered_before_an_end_is_not_counted_and_its_claim_is() {
    let calls = Arc::new(Calls::default());
    let find = calls.begin("find");
    assert!(!find.finish());
    calls.end("find");
    assert_eq!(calls.before_an_end("find"), 0, "answered before the end");
    let find = calls.begin("find");
    calls.end("find");
    assert!(find.finish(), "the end came first");
    let extract = calls.begin("extract");
    calls.end("extract");
    let claim = extract.hold();
    assert!(extract.finish());
    assert!(claim.ended(), "begun when its call did");
    assert_eq!(calls.before_an_end("extract"), 1, "its claim still holds");
    drop(claim);
    assert_eq!(calls.in_flight(), 0);
}

/// A call knows whether its operation's authorization, or the whole
/// module's, ended since it began; those still in flight are counted.
#[test]
fn a_call_knows_whether_its_authorization_ended_since_it_began() {
    let calls = Arc::new(Calls::default());
    let (find, list) = (calls.begin("find"), calls.begin("list"));
    calls.end("find");
    assert!(find.ended());
    assert!(!list.ended());
    assert!(!calls.begin("find").ended(), "begun after the end");
    assert_eq!(calls.before_an_end("find"), 1);
    assert_eq!(calls.before_an_end("list"), 0);
    assert_eq!(calls.in_flight(), 2);
    drop(find);
    assert_eq!(calls.before_an_end("find"), 0);
    calls.end(ALL);
    assert!(list.ended());
    assert_eq!(calls.before_an_end(ALL), 1);
    drop(list);
    assert_eq!(calls.in_flight(), 0);
}
