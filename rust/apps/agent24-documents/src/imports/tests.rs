#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tower::ServiceExt;

use super::media_type;
use crate::events::tests::{JOB, recorder, sent};
use crate::state::AppState;
use crate::uploads::chunks::MAX_CHUNK;

// ---- hooks the worker calls in tests ----

/// A worker for `upload_id` stops at a point until released.
struct Gate {
    upload_id: String,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

/// Gates once a worker has claimed its job, and once its bytes are stored.
static RUNNING_GATES: Mutex<Vec<Gate>> = Mutex::new(Vec::new());
static STORED_GATES: Mutex<Vec<Gate>> = Mutex::new(Vec::new());
/// Injected database failures: (job id, point, times left).
static FAULTS: Mutex<Vec<(String, &'static str, u32)>> = Mutex::new(Vec::new());

async fn pass_gate(gates: &Mutex<Vec<Gate>>, upload_id: &str) {
    let gate = {
        let mut gates = gates.lock().unwrap();
        let at = gates.iter().position(|g| g.upload_id == upload_id);
        at.map(|i| gates.remove(i))
    };
    if let Some(gate) = gate {
        gate.entered.notify_one();
        gate.release.notified().await;
    }
}

pub(super) async fn in_worker(upload_id: &str) {
    pass_gate(&RUNNING_GATES, upload_id).await;
}

pub(super) async fn after_store(upload_id: &str) {
    pass_gate(&STORED_GATES, upload_id).await;
}

pub(super) fn fault(job_id: &str, point: &str) -> Result<(), sqlx::Error> {
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

fn gate_in(gates: &Mutex<Vec<Gate>>, upload_id: &str) -> (Arc<Notify>, Arc<Notify>) {
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    gates.lock().unwrap().push(Gate {
        upload_id: upload_id.to_owned(),
        entered: entered.clone(),
        release: release.clone(),
    });
    (entered, release)
}

// ---- helpers ----

pub(super) const PDF: &[u8] = b"%PDF-1.7\n1 0 obj << >> endobj\ntrailer << >>\n%%EOF\n";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
const JPEG: &[u8] = b"\xff\xd8\xff\xe0\0\x10JFIF\0";

pub(super) struct Env {
    pub dir: tempfile::TempDir,
    pub state: AppState,
}

pub(super) async fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    Env { dir, state }
}

pub(super) fn sha(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

pub(super) async fn send(state: &AppState, req: Request<Body>) -> (StatusCode, Value) {
    let res = crate::router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A complete upload of `bytes`.
pub(super) async fn upload(state: &AppState, bytes: &[u8], filename: &str) -> String {
    let body = json!({ "total_size": bytes.len(), "sha256": sha(bytes), "filename": filename });
    let req = Request::post("/uploads")
        .header("content-type", "application/json")
        .header(
            "idempotency-key",
            format!("k-{}", crate::id::new_id(crate::id::IdKind::Job).unwrap()),
        )
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, v) = send(state, req).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let id = v["upload_id"].as_str().unwrap().to_owned();
    for (i, part) in bytes.chunks(MAX_CHUNK).enumerate() {
        let req = Request::post(format!("/uploads/{id}/chunks"))
            .header("upload-offset", (i * MAX_CHUNK).to_string())
            .header("chunk-sha256", sha(part))
            .body(Body::from(part.to_vec()))
            .unwrap();
        let (status, v) = send(state, req).await;
        assert_eq!(status, StatusCode::OK, "{v}");
    }
    id
}

/// Queues an import job for `upload_id` (with `faults` set) and starts its
/// worker, as the import handler does once it has claimed the key.
async fn queue_with(
    state: &AppState,
    upload_id: &str,
    title: Option<&str>,
    faults: &[(&'static str, u32)],
) -> String {
    let storage = state.storage().await.unwrap();
    let job_id = crate::id::new_id(crate::id::IdKind::Job).unwrap();
    let input = json!({ "upload_id": upload_id, "title": title }).to_string();
    sqlx::query(
        "INSERT INTO jobs (id, kind, status, origin, input) VALUES (?, 'import', 'queued', '{\"kind\":\"page\"}', ?)",
    )
    .bind(&job_id)
    .bind(&input)
    .execute(storage.db.pool())
    .await
    .unwrap();
    for &(point, times) in faults {
        FAULTS.lock().unwrap().push((job_id.clone(), point, times));
    }
    super::worker::spawn(storage, job_id.clone(), 1);
    job_id
}

async fn queue(state: &AppState, upload_id: &str, title: Option<&str>) -> String {
    queue_with(state, upload_id, title, &[]).await
}

/// The job once it stops working, as `GET /jobs/{id}` shows it.
pub(super) async fn settled(state: &AppState, job_id: &str) -> Value {
    let start = Instant::now();
    loop {
        let req = Request::get(format!("/jobs/{job_id}"))
            .body(Body::empty())
            .unwrap();
        let (status, v) = send(state, req).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        if !matches!(
            v["status"].as_str(),
            Some("queued" | "running" | "cancelling")
        ) {
            return v;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "job never settled: {v}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

pub(super) async fn count(state: &AppState, table: &str) -> i64 {
    let storage = state.storage().await.unwrap();
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(storage.db.pool())
        .await
        .unwrap()
}

pub(super) async fn upload_status(state: &AppState, id: &str) -> String {
    let storage = state.storage().await.unwrap();
    sqlx::query_scalar("SELECT status FROM uploads WHERE id = ?")
        .bind(id)
        .fetch_one(storage.db.pool())
        .await
        .unwrap()
}

pub(super) async fn exec(state: &AppState, sql: &str) {
    let storage = state.storage().await.unwrap();
    sqlx::query(sql).execute(storage.db.pool()).await.unwrap();
}

pub(super) fn failed_with(job: &Value, code: &str) {
    assert_eq!(
        (job["status"].as_str(), job["error"]["code"].as_str()),
        (Some("failed"), Some(code)),
        "{job}"
    );
}

/// Nothing of an import was written, and the upload is as it was.
pub(super) async fn nothing_imported(state: &AppState, upload_id: &str) {
    for table in ["documents", "revisions", "oplog"] {
        assert_eq!(count(state, table).await, 0, "{table}");
    }
    assert_eq!(upload_status(state, upload_id).await, "complete");
}

// ---- tests ----

#[test]
fn formats_are_told_by_their_first_bytes() {
    assert_eq!(media_type(PDF), Some("application/pdf"));
    assert_eq!(media_type(PNG), Some("image/png"));
    assert_eq!(media_type(JPEG), Some("image/jpeg"));
    for other in [
        &b"PK\x03\x04"[..],
        b"%PDF",
        b"\x89PNG\r\n\x1a",
        b"hello",
        b"",
    ] {
        assert_eq!(media_type(other), None, "{other:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_complete_upload_becomes_a_document_with_r1() {
    let env = env().await;
    let id = upload(&env.state, PDF, "通告.pdf").await;
    let job_id = queue(&env.state, &id, None).await;
    let job = settled(&env.state, &job_id).await;
    assert_eq!(job["status"], "succeeded", "{job}");
    assert_eq!(
        job["progress"],
        json!({ "stage": "store", "done": 1, "total": 1, "unit": "file" })
    );
    let doc = job["result"]["document_id"].as_str().unwrap().to_owned();
    assert_eq!(job["result"]["revision"], 1);
    let storage = env.state.storage().await.unwrap();
    let (title, media): (String, String) = sqlx::query_as(
        "SELECT title, media_type FROM documents WHERE id = ? AND head_revision = 1",
    )
    .bind(&doc)
    .fetch_one(storage.db.pool())
    .await
    .unwrap();
    assert_eq!(
        (title.as_str(), media.as_str()),
        ("通告.pdf", "application/pdf")
    );
    let (content, size): (String, i64) = sqlx::query_as(
        "SELECT content_sha256, size FROM revisions WHERE document_id = ? AND revision = 1",
    )
    .bind(&doc)
    .fetch_one(storage.db.pool())
    .await
    .unwrap();
    assert_eq!((content.clone(), size), (sha(PDF), PDF.len() as i64));
    assert_eq!(storage.blobs.read_verified(&content).unwrap(), PDF);
    assert_eq!(upload_status(&env.state, &id).await, "imported");
    let ops: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oplog WHERE op = 'import' AND document_id = ?")
            .bind(&doc)
            .fetch_one(storage.db.pool())
            .await
            .unwrap();
    assert_eq!(ops, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_title_overrides_the_filename_and_images_import_too() {
    let env = env().await;
    for (bytes, media) in [(PNG, "image/png"), (JPEG, "image/jpeg")] {
        let id = upload(&env.state, bytes, "scan").await;
        let job_id = queue(&env.state, &id, Some("收据")).await;
        let job = settled(&env.state, &job_id).await;
        let doc = job["result"]["document_id"].as_str().unwrap();
        let storage = env.state.storage().await.unwrap();
        let (title, got): (String, String) =
            sqlx::query_as("SELECT title, media_type FROM documents WHERE id = ?")
                .bind(doc)
                .fetch_one(storage.db.pool())
                .await
                .unwrap();
        assert_eq!((title.as_str(), got.as_str()), ("收据", media));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_without_its_input_fails() {
    let env = env().await;
    let storage = env.state.storage().await.unwrap();
    let job_id = crate::id::new_id(crate::id::IdKind::Job).unwrap();
    sqlx::query("INSERT INTO jobs (id, kind, status, origin) VALUES (?, 'import', 'queued', '{\"kind\":\"page\"}')")
        .bind(&job_id)
        .execute(storage.db.pool())
        .await
        .unwrap();
    super::worker::spawn(storage, job_id.clone(), 1);
    failed_with(&settled(&env.state, &job_id).await, "invalid_request");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bytes_changed_after_the_upload_completed_fail_the_import() {
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let path = env.dir.path().join("uploads").join(&id).join("data");
    let mut changed = PDF.to_vec();
    *changed.last_mut().unwrap() ^= 1;
    std::fs::write(&path, &changed).unwrap();
    let job_id = queue(&env.state, &id, None).await;
    failed_with(
        &settled(&env.state, &job_id).await,
        "upload_checksum_mismatch",
    );
    nothing_imported(&env.state, &id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_past_its_24_hours_is_not_imported() {
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    exec(
        &env.state,
        "UPDATE uploads SET last_chunk_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-25 hours')",
    )
    .await;
    let job_id = queue(&env.state, &id, None).await;
    failed_with(&settled(&env.state, &job_id).await, "not_found");
    nothing_imported(&env.state, &id).await;
    // Refused before any bytes were stored.
    let storage = env.state.storage().await.unwrap();
    assert!(!storage.blobs.contains(&sha(PDF)).unwrap());
}

/// Runs an import to just after its bytes are stored, applies `change`,
/// then lets it commit.
async fn change_after_store(env: &Env, id: &str, change: &str) -> Value {
    let (entered, release) = gate_in(&STORED_GATES, id);
    let job_id = queue(&env.state, id, None).await;
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    exec(
        &env.state,
        &change.replace("{job}", &job_id).replace("{upload}", id),
    )
    .await;
    release.notify_one();
    settled(&env.state, &job_id).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_that_expires_while_its_bytes_are_stored_is_not_imported() {
    let env = env().await;
    // Marked expired by a chunk request.
    let id = upload(&env.state, PDF, "a.pdf").await;
    let job = change_after_store(
        &env,
        &id,
        "UPDATE uploads SET status = 'expired' WHERE id = '{upload}'",
    )
    .await;
    failed_with(&job, "not_found");
    assert_eq!(count(&env.state, "documents").await, 0);
    // Still `complete`, but now past its deadline: only the clock says so.
    let env = self::env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let job = change_after_store(
        &env,
        &id,
        "UPDATE uploads SET last_chunk_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-25 hours') WHERE id = '{upload}'",
    )
    .await;
    failed_with(&job, "not_found");
    nothing_imported(&env.state, &id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_import_cancelled_before_or_after_its_bytes_are_stored_commits_nothing() {
    // Cancelled while it runs, before storing.
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let (entered, release) = gate_in(&RUNNING_GATES, &id);
    let job_id = queue(&env.state, &id, None).await;
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let req = Request::post(format!("/jobs/{job_id}/cancel"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&env.state, req).await.1["status"], "cancelling");
    release.notify_one();
    let job = settled(&env.state, &job_id).await;
    assert_eq!(
        (job["status"].as_str(), job["error"]["code"].as_str()),
        (Some("cancelled"), Some("cancelled"))
    );
    nothing_imported(&env.state, &id).await;
    // Cancelled once its bytes are in the blob store.
    let env = self::env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let job = change_after_store(
        &env,
        &id,
        "UPDATE jobs SET status = 'cancelling' WHERE id = '{job}'",
    )
    .await;
    assert_eq!(job["status"], "cancelled", "{job}");
    nothing_imported(&env.state, &id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failure_mid_commit_rolls_back_and_a_retry_finishes_the_import() {
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let job_id = queue_with(&env.state, &id, None, &[("after_document", 1)]).await;
    failed_with(&settled(&env.state, &job_id).await, "storage_unavailable");
    nothing_imported(&env.state, &id).await;
    // Its bytes are already in the blob store; an explicit retry finishes.
    let req = Request::post(format!("/jobs/{job_id}/retry"))
        .body(Body::empty())
        .unwrap();
    let (code, v) = send(&env.state, req).await;
    assert_eq!((code, v["attempt"].as_i64()), (StatusCode::OK, Some(2)));
    let job = settled(&env.state, &job_id).await;
    assert_eq!(job["status"], "succeeded", "{job}");
    assert_eq!(count(&env.state, "documents").await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retry_whose_client_goes_away_after_the_commit_still_runs() {
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let job_id = queue_with(&env.state, &id, None, &[("after_document", 1)]).await;
    assert_eq!(settled(&env.state, &job_id).await["status"], "failed");
    let (entered, release) = crate::jobs::tests::retry_gate(&job_id);
    let request = tokio::spawn({
        let (state, job_id) = (env.state.clone(), job_id.clone());
        async move {
            let req = Request::post(format!("/jobs/{job_id}/retry"))
                .body(Body::empty())
                .unwrap();
            send(&state, req).await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    release.notify_one();
    let job = settled(&env.state, &job_id).await;
    assert_eq!(
        (job["status"].as_str(), job["attempt"].as_i64()),
        (Some("succeeded"), Some(2)),
        "{job}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_complete_upload_in_another_format_is_not_imported() {
    let env = env().await;
    let id = upload(&env.state, b"just some text, hashed and complete", "a.txt").await;
    let job_id = queue(&env.state, &id, None).await;
    failed_with(&settled(&env.state, &job_id).await, "unsupported_format");
    nothing_imported(&env.state, &id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failure_after_every_result_row_is_written_rolls_them_all_back() {
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let job_id = queue_with(&env.state, &id, None, &[("before_success", 1)]).await;
    failed_with(&settled(&env.state, &job_id).await, "storage_unavailable");
    nothing_imported(&env.state, &id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_imports_of_one_upload_make_one_document() {
    // Both pass the first upload check and store the bytes; only one can
    // move the upload to imported under the write lock (Codex review).
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let (first_in, first_go) = gate_in(&STORED_GATES, &id);
    let (second_in, second_go) = gate_in(&STORED_GATES, &id);
    let a = queue(&env.state, &id, None).await;
    let b = queue(&env.state, &id, None).await;
    for entered in [&first_in, &second_in] {
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
    }
    first_go.notify_one();
    second_go.notify_one();
    let (a, b) = (settled(&env.state, &a).await, settled(&env.state, &b).await);
    let mut statuses = [a["status"].as_str(), b["status"].as_str()];
    statuses.sort_unstable();
    assert_eq!(statuses, [Some("failed"), Some("succeeded")], "{a} {b}");
    let loser = if a["status"] == "failed" { &a } else { &b };
    failed_with(loser, "not_found");
    for table in ["documents", "revisions", "oplog"] {
        assert_eq!(count(&env.state, table).await, 1, "{table}");
    }
    assert_eq!(upload_status(&env.state, &id).await, "imported");
}

// ---- events (ADR-DOC-02 §7) ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_import_announces_its_end_and_its_document() {
    let dir = tempfile::tempdir().unwrap();
    let (events, seen) = recorder(Duration::ZERO, 0);
    let state = AppState::open_with_events(dir.path(), events).await;
    let id = upload(&state, PDF, "通告.pdf").await;
    let job_id = queue(&state, &id, None).await;
    let job = settled(&state, &job_id).await;
    let doc = job["result"]["document_id"].clone();
    assert_eq!(
        sent(&seen, 2).await,
        [
            (
                "job.finished".to_owned(),
                json!({ "job_id": job_id, "kind": "import", "status": "succeeded", "attempt": 1, "error_code": null, "document_id": doc, "revision": 1 })
            ),
            (
                "document.imported".to_owned(),
                json!({ "document_id": doc, "revision": 1, "job_id": job_id })
            ),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_that_ends_a_job_announces_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let (events, seen) = recorder(Duration::ZERO, 0);
    let state = AppState::open_with_events(dir.path(), events).await;
    exec(&state, &format!("INSERT INTO jobs (id, kind, status, origin) VALUES ('{JOB}', 'import', 'failed', '{{\"kind\":\"page\"}}')")).await;
    exec(&state, &format!("UPDATE jobs SET error = json_object('code', 'storage_unavailable', 'message', 'm') WHERE id = '{JOB}'")).await;
    for _ in 0..2 {
        let req = Request::post(format!("/jobs/{JOB}/cancel"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(&state, req).await.1["status"], "cancelled");
    }
    assert_eq!(sent(&seen, 1).await[0].1["error_code"], "cancelled");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "a second cancel changes nothing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jobs_ended_by_a_restart_are_announced() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    for (id, status) in [
        (JOB, "running"),
        ("job_01K75A0B1C2D3E4F5G6H7J8K9N", "cancelling"),
    ] {
        exec(&state, &format!("INSERT INTO jobs (id, kind, status, origin) VALUES ('{id}', 'import', '{status}', '{{\"kind\":\"page\"}}')")).await;
    }
    drop(state);
    let (events, seen) = recorder(Duration::ZERO, 0);
    let _state = AppState::open_with_events(dir.path(), events).await;
    let sent = sent(&seen, 2).await;
    let ends: Vec<_> = sent
        .iter()
        .map(|(k, v)| {
            (
                k.as_str(),
                v["status"].as_str().unwrap(),
                v["error_code"].clone(),
            )
        })
        .collect();
    assert_eq!(
        ends,
        [
            ("job.finished", "interrupted", Value::Null),
            ("job.finished", "cancelled", json!("cancelled"))
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_attempt_and_its_retry_are_each_announced() {
    let dir = tempfile::tempdir().unwrap();
    let (events, seen) = recorder(Duration::ZERO, 0);
    let state = AppState::open_with_events(dir.path(), events).await;
    let id = upload(&state, PDF, "a.pdf").await;
    let job_id = queue_with(&state, &id, None, &[("after_document", 1)]).await;
    settled(&state, &job_id).await;
    let req = Request::post(format!("/jobs/{job_id}/retry"))
        .body(Body::empty())
        .unwrap();
    send(&state, req).await;
    settled(&state, &job_id).await;
    let ends: Vec<_> = sent(&seen, 3)
        .await
        .into_iter()
        .map(|(k, v)| {
            (
                k,
                v["status"].clone(),
                v["attempt"].clone(),
                v["error_code"].clone(),
            )
        })
        .collect();
    assert_eq!(
        ends,
        [
            (
                "job.finished".to_owned(),
                json!("failed"),
                json!(1),
                json!("storage_unavailable")
            ),
            (
                "job.finished".to_owned(),
                json!("succeeded"),
                json!(2),
                Value::Null
            ),
            (
                "document.imported".to_owned(),
                Value::Null,
                Value::Null,
                Value::Null
            ),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_announces_a_bounded_number_of_the_jobs_it_ended() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    let n = crate::jobs::RECOVERED_SHOWN + 20;
    let values: Vec<_> = (0..n)
        .map(|_| {
            format!(
                "('{}', 'import', 'running', '{{\"kind\":\"page\"}}')",
                crate::id::new_id(crate::id::IdKind::Job).unwrap()
            )
        })
        .collect();
    exec(
        &state,
        &format!(
            "INSERT INTO jobs (id, kind, status, origin) VALUES {}",
            values.join(",")
        ),
    )
    .await;
    drop(state);
    let (events, seen) = recorder(Duration::ZERO, 0);
    let state = AppState::open_with_events(dir.path(), events).await;
    sent(&seen, crate::jobs::RECOVERED_SHOWN).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(seen.lock().unwrap().len(), crate::jobs::RECOVERED_SHOWN);
    // All of them were ended, announced or not.
    assert_eq!(
        count(&state, "jobs WHERE status = 'interrupted'").await,
        n as i64
    );
}
