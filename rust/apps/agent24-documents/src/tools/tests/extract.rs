//! `extract` (DOC-1-06b §3.3 slice D2): submits or replays as REST does,
//! from the run, its `oplog` row in the claim's own transaction.

use agent24_os_sdk::{ClientError, CompleteResult};

use super::*;
use crate::extract::job::tests::{DOC, Env, answer, ended, env, script};

fn schema(description: &str) -> Value {
    json!({ "fields": [{ "key": "date", "description": description, "type": "date" }] })
}

fn args() -> Value {
    json!({ "document_id": DOC, "revision": 1, "schema": schema("停水日期") })
}

fn other() -> Value {
    json!({ "document_id": DOC, "revision": 1, "schema": schema("另一个说明") })
}

fn failing() -> Result<CompleteResult, ClientError> {
    Err(ClientError::Forbidden("f".into()))
}

/// An `extract` call from run-1 / call-1, its envelope as JSON.
async fn submit(env: &Env, args: Value) -> Value {
    serde_json::from_slice(&call(&env.state, "extract", args).await).unwrap()
}

fn job_id(envelope: &Value) -> String {
    envelope["payload"]["job_id"].as_str().unwrap().to_owned()
}

fn done(status: &str, job: &str, replayed: bool) -> Value {
    let mut e = json!({ "kind": "completed", "payload": { "status": status, "job_id": job } });
    if replayed {
        e["replayed"] = json!(true);
    }
    e
}

async fn scalar<T>(env: &Env, sql: &str) -> T
where
    T: Send + Unpin + for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    sqlx::query_scalar(sql)
        .fetch_one(env.storage.db.pool())
        .await
        .unwrap()
}

/// The job's status and attempt.
async fn row(env: &Env, id: &str) -> (String, i64) {
    sqlx::query_as("SELECT status, attempt FROM jobs WHERE id = ?")
        .bind(id)
        .fetch_one(env.storage.db.pool())
        .await
        .unwrap()
}

/// A status a worker would take its time to reach, set directly: a claim
/// reads only `status` and `attempt`, whoever set them. Only a failed job
/// has an error.
async fn set(env: &Env, id: &str, status: &str) {
    sqlx::query(
        "UPDATE jobs SET status = ?1, error = CASE WHEN ?1 = 'failed'
             THEN json_object('code', 'parse_failed', 'message', 'm') END WHERE id = ?2",
    )
    .bind(status)
    .bind(id)
    .execute(env.storage.db.pool())
    .await
    .unwrap();
}

async fn oplog(env: &Env) -> Vec<Value> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT json_object('op', op, 'origin', origin, 'run_id', run_id,
                'tool_call_id', tool_call_id, 'document_id', document_id,
                'revision', revision, 'details', json(details)) FROM oplog ORDER BY seq",
    )
    .fetch_all(env.storage.db.pool())
    .await
    .unwrap();
    rows.iter()
        .map(|r| serde_json::from_str(r).unwrap())
        .collect()
}

fn line(job: &str) -> Value {
    json!({ "op": "extract", "origin": "run", "run_id": "run-1", "tool_call_id": "call-1",
            "document_id": DOC, "revision": 1, "details": { "job_id": job } })
}

async fn origin(env: &Env, id: &str) -> Value {
    let sql = format!("SELECT origin FROM jobs WHERE id = '{id}'");
    serde_json::from_str(&scalar::<String>(env, &sql).await).unwrap()
}

async fn rest_post(env: &Env, doc: &str, body: &Value) -> (StatusCode, Value) {
    let req = Request::post(format!("/documents/{doc}/extractions"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = crate::router(env.state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(100)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_extract_answers_its_job_logs_the_run_and_reads_back_as_rest() {
    let env = env(Some(script(vec![answer("present")]))).await;
    let first = submit(&env, args()).await;
    let id = job_id(&first);
    // Exactly `{status, job_id}`, the job as the claim left it; not replayed.
    assert_eq!(first, done("queued", &id, false));
    let run = json!({ "kind": "run", "run_id": "run-1", "tool_call_id": "call-1" });
    assert_eq!(origin(&env, &id).await, run);
    assert_eq!(oplog(&env).await, vec![line(&id)]);

    assert_eq!(ended(&env, &id).await.0, "succeeded");
    let job = payload(&call(&env.state, "job_get", json!({ "job_id": id })).await);
    let ext = job["result"]["extraction_id"].as_str().unwrap();
    let (status, want) = rest(&env.state, &format!("/extractions/{ext}")).await;
    assert_eq!(status, StatusCode::OK);
    let got = payload(
        &call(
            &env.state,
            "extraction_get",
            json!({ "extraction_id": ext }),
        )
        .await,
    );
    assert_eq!(got, want);

    let again = submit(&env, args()).await;
    assert_eq!(again, done("succeeded", &id, true));
    assert_eq!(oplog(&env).await, vec![line(&id), line(&id)]);
}

/// An `extract` call from run-1 / call-1 within `budget`, its envelope as
/// JSON.
async fn submit_within(env: &Env, args: Value, budget: Duration) -> Value {
    let app = axum::Router::new()
        .route(
            "/_a24/tools/{op}",
            axum::routing::post(
                move |s: State<AppState>,
                      o: Result<UrlPath<String>, PathRejection>,
                      b: Result<Bytes, BytesRejection>| {
                    call_tool_within(s, o, b, budget)
                },
            ),
        )
        .with_state(env.state.clone());
    let body = json!({ "context": context_with("extract", &[DOC]), "arguments": args });
    let req = Request::post("/_a24/tools/extract")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = tokio::time::timeout(Duration::from_secs(3), app.oneshot(req))
        .await
        .expect("no answer within the budget")
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Past the deadline the call answers `internal`, retryable, and the claim
/// it was waiting on still commits and starts its worker: the retry
/// replays that job.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claim_past_the_deadline_still_commits_and_the_retry_replays_it() {
    let env = env(Some(script(vec![answer("present")]))).await;
    // The write lock, held: the claim's `BEGIN IMMEDIATE` waits for it (up
    // to the 5 s busy timeout). `prepare` only reads, which WAL lets go on
    // under a writer, so the call gets past it and waits in the claim.
    let lock = env
        .storage
        .db
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    let late = submit_within(&env, args(), Duration::from_millis(500)).await;
    assert_eq!(
        late,
        json!({ "kind": "error", "code": "internal", "retryable": true })
    );
    lock.rollback().await.unwrap();
    // Only a claim already under way commits now: a call dropped in
    // `prepare` would have left nothing behind.
    let start = std::time::Instant::now();
    while scalar::<i64>(&env, "SELECT count(*) FROM jobs").await == 0 {
        assert!(start.elapsed() < Duration::from_secs(10), "never claimed");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let id: String = scalar(&env, "SELECT id FROM jobs").await;
    let key = "SELECT count(*) FROM idempotency WHERE kind = 'extract' AND target_ref = ";
    assert_eq!(scalar::<i64>(&env, &format!("{key}'{id}'")).await, 1);
    assert_eq!(oplog(&env).await, vec![line(&id)]);
    assert_eq!(ended(&env, &id).await.0, "succeeded");

    assert_eq!(submit(&env, args()).await, done("succeeded", &id, true));
    assert_eq!(oplog(&env).await, vec![line(&id), line(&id)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replay_is_marked_unless_its_job_runs_again() {
    let replies = vec![failing(), answer("present"), failing(), answer("present")];
    let env = env(Some(script(replies))).await;
    let a = job_id(&submit(&env, args()).await);
    assert_eq!(ended(&env, &a).await.0, "failed");
    for status in ["queued", "running", "cancelling", "cancelled"] {
        set(&env, &a, status).await;
        assert_eq!(submit(&env, args()).await, done(status, &a, true));
        // `replayed: true` is the deterministic check: the claim reports a
        // start exactly when it spawned a worker. The row below only backs it
        // up, and no test hook marks a spawn, so it waits: a stray worker
        // would claim the queued job and run it on the in-memory engine and
        // scripted model within milliseconds, well inside 100 ms; under the
        // other statuses its claim finds nothing queued and leaves the row.
        settle().await;
        assert_eq!(row(&env, &a).await, (status.to_owned(), 1));
    }
    set(&env, &a, "failed").await;
    assert_eq!(submit(&env, args()).await, done("queued", &a, false));
    assert_eq!(row(&env, &a).await.1, 2);
    assert_eq!(ended(&env, &a).await.0, "succeeded");

    let b = job_id(&submit(&env, other()).await);
    assert_eq!(ended(&env, &b).await.0, "failed");
    set(&env, &b, "interrupted").await;
    assert_eq!(submit(&env, other()).await, done("queued", &b, false));
    assert_eq!(row(&env, &b).await.1, 2);
    assert_eq!(ended(&env, &b).await.0, "succeeded");
    // One row per call, new or replayed.
    assert_eq!(oplog(&env).await.len(), 8);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_replays_the_page_s_job_and_only_the_run_is_logged() {
    let env = env(Some(script(vec![answer("present")]))).await;
    let page = json!({ "revision": 1, "schema": schema("停水日期") });
    let (status, job) = rest_post(&env, DOC, &page).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{job}");
    let id = job["job_id"].as_str().unwrap().to_owned();
    assert_eq!(ended(&env, &id).await.0, "succeeded");
    assert_eq!(submit(&env, args()).await, done("succeeded", &id, true));
    assert_eq!(origin(&env, &id).await, json!({ "kind": "page" }));
    // Neither the page's request nor its replay writes a row.
    assert_eq!(rest_post(&env, DOC, &page).await.0, StatusCode::OK);
    assert_eq!(oplog(&env).await, vec![line(&id)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oplog_row_that_cannot_be_written_takes_the_claim_with_it() {
    let env = env(Some(script(vec![failing()]))).await;
    let a = job_id(&submit(&env, args()).await);
    assert_eq!(ended(&env, &a).await.0, "failed");
    let trigger =
        "CREATE TRIGGER no_oplog BEFORE INSERT ON oplog BEGIN SELECT RAISE(ABORT, 'x'); END";
    exec(&env.state, trigger).await;
    let body = call(&env.state, "extract", other()).await;
    assert_module_error(&body, "internal", false);
    let counts = "SELECT (SELECT count(*) FROM jobs) * 10 + (SELECT count(*) FROM idempotency)";
    assert_eq!(scalar::<i64>(&env, counts).await, 11);
    let body = call(&env.state, "extract", args()).await;
    assert_module_error(&body, "internal", false);
    settle().await;
    assert_eq!(row(&env, &a).await, ("failed".to_owned(), 1));
}

/// Each refusal is REST's own code, with REST's `retryable`.
async fn as_rest(env: &Env, doc: &str, schema: Value, revision: i64, code: &str) {
    let (_, rest) = rest_post(env, doc, &json!({ "revision": revision, "schema": schema })).await;
    let args = json!({ "document_id": doc, "revision": revision, "schema": schema });
    let tool = submit(env, args).await;
    let error = &rest["error"];
    assert_eq!(error["code"], code, "{rest}");
    assert_eq!(
        (&tool["kind"], &tool["code"], &tool["retryable"]),
        (
            &json!("error"),
            &error["code"],
            &error["details"]["retryable"]
        ),
        "{tool}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extract_refuses_as_rest_does() {
    let no_model = env(None).await;
    as_rest(&no_model, DOC, schema("d"), 1, "engine_unavailable").await;
    let env = env(Some(script(vec![]))).await;
    as_rest(&env, DOC, schema("d"), 2, "not_found").await;
    as_rest(&env, "doc_x", schema("d"), 1, "invalid_request").await;
    let field = |k: &str| json!({ "key": k, "description": "d" });
    for fields in [
        vec![field("Date")],
        vec![field("a"), field("a")],
        (0..101).map(|n| field(&format!("f{n}"))).collect(),
    ] {
        as_rest(&env, DOC, json!({ "fields": fields }), 1, "invalid_request").await;
    }
    let docx = "doc_01K74Z3QJ8V5N2W9RTX6YB4M01";
    let media = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
    add(&env.state, docx, media).await;
    as_rest(&env, docx, schema("d"), 1, "unsupported_format").await;
    // No read engine (D10): the model alone does not do.
    let dir = tempfile::tempdir().unwrap();
    let layers = crate::engine::Layers::new(None);
    let state = AppState::open_serving(dir.path(), crate::events::Events::default(), layers).await;
    state.set_model(script(vec![]));
    add(&state, DOC, "application/pdf").await;
    let storage = state.storage().await.unwrap();
    let bare = Env {
        _dir: dir,
        state,
        storage,
    };
    as_rest(&bare, DOC, schema("d"), 1, "engine_unavailable").await;
    let rows = "SELECT (SELECT count(*) FROM jobs) + (SELECT count(*) FROM oplog)";
    for e in [&no_model, &env, &bare] {
        assert_eq!(scalar::<i64>(e, rows).await, 0);
    }
}

async fn add(state: &AppState, doc: &str, media: &str) {
    let sql = format!(
        "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{doc}', 't', '{media}', 1);
         INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
         VALUES ('{doc}', 1, 'sha256:{}', 1, '{media}', 'import');",
        "0".repeat(64)
    );
    exec(state, &sql).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arguments_or_a_context_outside_the_schema_are_invalid_input() {
    let env = env(Some(script(vec![]))).await;
    let with = |key: &str, value: Value| {
        let mut a = args();
        a[key] = value;
        a
    };
    let mut no_schema = args();
    no_schema.as_object_mut().unwrap().remove("schema");
    let null_type = json!({ "fields": [{ "key": "a", "description": "d", "type": null }] });
    for a in [
        no_schema,
        with("rerun", json!(false)),
        with("revision", json!("1")),
        with("schema", json!({ "fields": "date" })),
        with("schema", json!({ "fields": [], "model": "x" })),
        with("schema", null_type),
    ] {
        let body = call(&env.state, "extract", a.clone()).await;
        assert_kernel_error(&body, ModuleToolErrorCode::InvalidInput);
    }
    for id in ["run_id", "tool_call_id"] {
        let mut ctx = context_with("extract", &[DOC]);
        ctx[id] = json!("");
        let body = json!({ "context": ctx, "arguments": args() }).to_string();
        let body = post_to(&env.state, "extract", body).await;
        assert_kernel_error(&body, ModuleToolErrorCode::InvalidInput);
    }
    let rows = "SELECT (SELECT count(*) FROM jobs) + (SELECT count(*) FROM oplog)";
    assert_eq!(scalar::<i64>(&env, rows).await, 0);
}

/// Each acceptance: the job, its attempt, and the call's operation and grant.
async fn acceptances(env: &Env) -> Vec<(String, i64, String, Option<String>)> {
    sqlx::query_as(
        "SELECT job_id, attempt, operation, authorization_ref FROM job_acceptances ORDER BY rowid",
    )
    .fetch_all(env.storage.db.pool())
    .await
    .unwrap()
}

/// K1-06a-grant-path §2.5: a call records the attempt its job's work goes on
/// in, under its grant, so the grant's end stops it: a new job, one queued
/// again, one already working (the page's too). One with no work left to do
/// records nothing, and neither does the page.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_records_the_attempt_its_work_goes_on_in() {
    let env = env(Some(script(vec![failing(), failing(), failing()]))).await;
    let at = |id: &str, attempt| {
        (
            id.to_owned(),
            attempt,
            "extract".to_owned(),
            Some("auth-1".to_owned()),
        )
    };
    let a = job_id(&submit(&env, args()).await);
    assert_eq!(acceptances(&env).await, vec![at(&a, 1)]);
    assert_eq!(ended(&env, &a).await.0, "failed");
    assert_eq!(submit(&env, args()).await, done("queued", &a, false));
    assert_eq!(ended(&env, &a).await.0, "failed");
    // A cancelled job is not revived by a key hit (§7): no work goes on.
    set(&env, &a, "cancelled").await;
    submit(&env, args()).await;
    for status in ["queued", "running", "cancelling"] {
        set(&env, &a, status).await;
        submit(&env, args()).await;
    }
    let mut expected = vec![at(&a, 1), at(&a, 2)];
    expected.extend([at(&a, 2), at(&a, 2), at(&a, 2)]);
    assert_eq!(acceptances(&env).await, expected);

    let page = json!({ "revision": 1, "schema": schema("另一个说明") });
    let (_, job) = rest_post(&env, DOC, &page).await;
    let b = job["job_id"].as_str().unwrap().to_owned();
    assert_eq!(acceptances(&env).await.len(), 5, "the page records none");
    // Its worker ends first, its supervisor too, so none races the status
    // set below.
    assert_eq!(ended(&env, &b).await.0, "failed");
    crate::jobs::runner::tests::wait_for_done(&b, 1).await;
    set(&env, &b, "running").await;
    submit(&env, other()).await;
    assert_eq!(acceptances(&env).await.last(), Some(&at(&b, 1)));
}

/// #1201 G3/G4: an acceptance names the call it came from, its run and its
/// tool call. A replay of a succeeded job starts no work and records none;
/// nor does the page's own retry of a failed job, which no call asked for.
#[tokio::test]
async fn an_acceptance_names_its_call_and_work_no_call_asked_for_records_none() {
    let env = env(Some(script(vec![answer("present"), failing(), failing()]))).await;
    let a = job_id(&submit(&env, args()).await);
    assert_eq!(ended(&env, &a).await.0, "succeeded");
    let named: (String, String) =
        sqlx::query_as("SELECT run_id, tool_call_id FROM job_acceptances")
            .fetch_one(env.storage.db.pool())
            .await
            .unwrap();
    assert_eq!(named, ("run-1".to_owned(), "call-1".to_owned()));
    assert_eq!(submit(&env, args()).await, done("succeeded", &a, true));
    assert_eq!(
        acceptances(&env).await.len(),
        1,
        "a succeeded replay records none"
    );

    let b = job_id(&submit(&env, other()).await);
    assert_eq!(ended(&env, &b).await.0, "failed");
    crate::jobs::runner::tests::wait_for_done(&b, 1).await;
    let retry = Request::post(format!("/jobs/{b}/retry"))
        .body(Body::empty())
        .unwrap();
    let res = crate::router(env.state.clone())
        .oneshot(retry)
        .await
        .unwrap();
    assert!(res.status().is_success(), "{}", res.status());
    assert_eq!(row(&env, &b).await.1, 2);
    assert_eq!(
        acceptances(&env).await.len(),
        2,
        "the page's retry records none"
    );
}

/// K1-06a-grant-path §2.5.1: a call under a grant whose end was told
/// already is refused on arrival, before it claims: no job, no acceptance,
/// nothing logged, so no work of the ended grant starts after its notice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_call_under_an_ended_grant_is_refused_before_it_claims() {
    let env = env(Some(script(vec![failing()]))).await;
    let ended = crate::authorization::Ended {
        operation: "extract".into(),
        refs: Some(["auth-1".to_owned()].into()),
    };
    env.state.calls().end(&ended);
    let body = json!({ "context": context_with("extract", &[DOC]), "arguments": args() });
    let body = post_to(&env.state, "extract", body.to_string()).await;
    assert_kernel_error(&body, ModuleToolErrorCode::PermissionDenied);
    let rows = "SELECT (SELECT count(*) FROM jobs) + (SELECT count(*) FROM oplog)
                     + (SELECT count(*) FROM job_acceptances)";
    assert_eq!(scalar::<i64>(&env, rows).await, 0);
    assert_eq!(env.state.calls().in_flight(), 0);
}
