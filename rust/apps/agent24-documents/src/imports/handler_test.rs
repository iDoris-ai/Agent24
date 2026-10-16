//! `POST /imports`: checks, the key claim, replays and the worker start.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::tests::{PDF, count, env, exec, send, settled, sha, upload};
use crate::idem::request_sha256;
use crate::state::AppState;

/// Gates between a claim's commit and its worker's start (by upload id),
/// after a replay's commit (by job id), after the first key lookup found
/// nothing (by upload id), and between a re-queue's commit and its worker's
/// start (by job id).
type Gate = (String, Arc<Notify>, Arc<Notify>);
static CLAIM_GATES: Mutex<Vec<Gate>> = Mutex::new(Vec::new());
static REPLAY_GATES: Mutex<Vec<Gate>> = Mutex::new(Vec::new());
static LOOKUP_GATES: Mutex<Vec<Gate>> = Mutex::new(Vec::new());
static REQUEUE_GATES: Mutex<Vec<Gate>> = Mutex::new(Vec::new());

async fn pass(gates: &Mutex<Vec<Gate>>, key: &str) {
    let gate = {
        let mut gates = gates.lock().unwrap();
        let at = gates.iter().position(|g| g.0 == key);
        at.map(|i| gates.remove(i))
    };
    if let Some((_, entered, release)) = gate {
        entered.notify_one();
        release.notified().await;
    }
}

pub(super) async fn after_claim(upload_id: &str) {
    pass(&CLAIM_GATES, upload_id).await;
}

pub(super) async fn after_replay(job_id: &str) {
    pass(&REPLAY_GATES, job_id).await;
}

pub(super) async fn after_lookup(upload_id: &str) {
    pass(&LOOKUP_GATES, upload_id).await;
}

pub(super) async fn before_requeued_start(job_id: &str) {
    pass(&REQUEUE_GATES, job_id).await;
}

fn gate_in(gates: &Mutex<Vec<Gate>>, key: &str) -> (Arc<Notify>, Arc<Notify>) {
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    gates
        .lock()
        .unwrap()
        .push((key.to_owned(), entered.clone(), release.clone()));
    (entered, release)
}

async fn import(state: &AppState, body: Value) -> (StatusCode, Value) {
    let req = Request::post("/imports")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    send(state, req).await
}

/// An upload declared with the hash of `declared` but sent as `sent`.
async fn upload_as(state: &AppState, declared: &[u8], sent: &[u8]) -> String {
    let body = json!({ "total_size": declared.len(), "sha256": sha(declared) });
    let req = Request::post("/uploads")
        .header("content-type", "application/json")
        .header(
            "idempotency-key",
            format!("k-{}", crate::id::new_id(crate::id::IdKind::Job).unwrap()),
        )
        .body(Body::from(body.to_string()))
        .unwrap();
    let (_, v) = send(state, req).await;
    let id = v["upload_id"].as_str().unwrap().to_owned();
    let req = Request::post(format!("/uploads/{id}/chunks"))
        .header("upload-offset", "0")
        .header("chunk-sha256", sha(sent))
        .body(Body::from(sent.to_vec()))
        .unwrap();
    assert_eq!(send(state, req).await.0, StatusCode::OK);
    id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_import_starts_a_job_that_makes_the_document() {
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let (status, v) = import(&env.state, json!({ "upload_id": id })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    assert_eq!(
        (v["kind"].as_str(), v["attempt"].as_i64()),
        (Some("import"), Some(1))
    );
    let job = settled(&env.state, v["job_id"].as_str().unwrap()).await;
    assert_eq!(job["status"], "succeeded", "{job}");
    assert_eq!(count(&env.state, "documents").await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_request_replays_and_another_is_refused() {
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let (_, first) = import(&env.state, json!({ "upload_id": id })).await;
    let job_id = first["job_id"].as_str().unwrap().to_owned();
    settled(&env.state, &job_id).await;
    let (status, again) = import(&env.state, json!({ "upload_id": id })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (again["job_id"].as_str(), again["status"].as_str()),
        (Some(job_id.as_str()), Some("succeeded"))
    );
    let (status, v) = import(&env.state, json!({ "upload_id": id, "title": "other" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["error"]["code"], "idempotency_key_reused");
    assert_eq!(count(&env.state, "documents").await, 1);
}

#[tokio::test]
async fn bad_requests_and_uploads_that_are_not_ready() {
    let env = env().await;
    // Half sent: 400 with what has arrived.
    let body = json!({ "total_size": PDF.len(), "sha256": sha(PDF) });
    let req = Request::post("/uploads")
        .header("content-type", "application/json")
        .header("idempotency-key", "half")
        .body(Body::from(body.to_string()))
        .unwrap();
    let id = send(&env.state, req).await.1["upload_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let req = Request::post(format!("/uploads/{id}/chunks"))
        .header("upload-offset", "0")
        .header("chunk-sha256", sha(&PDF[..5]))
        .body(Body::from(PDF[..5].to_vec()))
        .unwrap();
    send(&env.state, req).await;
    let (status, v) = import(&env.state, json!({ "upload_id": id })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        v["error"]["details"],
        json!({ "retryable": false, "received": 5 })
    );
    let (status, _) = import(
        &env.state,
        json!({ "upload_id": "upl_01K74Z3QJ8V5N2W9RTX6YB4MCD" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    for body in [
        json!({ "upload_id": "upl_x" }),
        json!({ "upload_id": id, "title": "" }),
        json!({ "upload_id": id, "title": "t".repeat(501) }),
        json!({ "upload_id": id, "title": null }),
        json!({ "upload_id": id, "extra": 1 }),
        json!({}),
    ] {
        let (status, _) = import(&env.state, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    // An expired upload is gone.
    let done = upload(&env.state, PDF, "a.pdf").await;
    exec(&env.state, &format!(
        "UPDATE uploads SET last_chunk_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-25 hours') WHERE id = '{done}'"
    ))
    .await;
    assert_eq!(
        import(&env.state, json!({ "upload_id": done })).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(count(&env.state, "jobs").await, 0);
}

#[tokio::test]
async fn bytes_that_do_not_match_or_cannot_be_read_are_422_and_start_nothing() {
    let env = env().await;
    let mut other = PDF.to_vec();
    *other.last_mut().unwrap() ^= 1;
    let mismatched = upload_as(&env.state, &other, PDF).await;
    let (status, v) = import(&env.state, json!({ "upload_id": mismatched })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(v["error"]["code"], "upload_checksum_mismatch");
    let text = upload(&env.state, b"just some text", "a.txt").await;
    let (status, v) = import(&env.state, json!({ "upload_id": text })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["error"]["code"], "unsupported_format");
    assert_eq!(count(&env.state, "jobs").await, 0);
}

/// A job row and its key, as a crash or a cancel left them.
async fn left_behind(state: &AppState, upload_id: &str, status: &str) -> String {
    let job_id = crate::id::new_id(crate::id::IdKind::Job).unwrap();
    let request = request_sha256(&json!({ "upload_id": upload_id })).unwrap();
    let error = match status {
        "cancelled" => "json_object('code', 'cancelled', 'message', 'x')",
        "failed" => "json_object('code', 'storage_unavailable', 'message', 'x')",
        _ => "NULL",
    };
    exec(state, &format!(
        "INSERT INTO jobs (id, kind, status, origin, input, error, progress)
         VALUES ('{job_id}', 'import', '{status}', '{{\"kind\":\"page\"}}', json_object('upload_id', '{upload_id}'), {error},
                 json_object('stage', 'store', 'done', 0, 'total', 1, 'unit', 'file'))"
    ))
    .await;
    exec(state, &format!(
        "INSERT INTO idempotency (kind, key, request_sha256, target_ref) VALUES ('import', '{upload_id}', '{request}', '{job_id}')"
    ))
    .await;
    job_id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_import_runs_again_and_a_cancelled_one_stays_cancelled() {
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let job_id = left_behind(&env.state, &id, "interrupted").await;
    let (status, v) = import(&env.state, json!({ "upload_id": id })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    assert_eq!(
        (v["job_id"].as_str(), v["attempt"].as_i64()),
        (Some(job_id.as_str()), Some(2))
    );
    assert_eq!(settled(&env.state, &job_id).await["status"], "succeeded");

    let env = super::tests::env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let job_id = left_behind(&env.state, &id, "cancelled").await;
    let (status, v) = import(&env.state, json!({ "upload_id": id })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        (
            v["job_id"].as_str(),
            v["status"].as_str(),
            v["attempt"].as_i64()
        ),
        (Some(job_id.as_str()), Some("cancelled"), Some(1))
    );
    assert_eq!(count(&env.state, "documents").await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_imports_of_one_upload_share_one_job() {
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let calls: Vec<_> = (0..8)
        .map(|_| {
            let (state, id) = (env.state.clone(), id.clone());
            tokio::spawn(async move { import(&state, json!({ "upload_id": id })).await })
        })
        .collect();
    let mut jobs = std::collections::HashSet::new();
    for c in calls {
        let (status, v) = c.await.unwrap();
        assert!(
            matches!(status, StatusCode::ACCEPTED | StatusCode::OK),
            "{status}: {v}"
        );
        jobs.insert(v["job_id"].as_str().unwrap().to_owned());
    }
    assert_eq!(jobs.len(), 1);
    assert_eq!(
        settled(&env.state, jobs.iter().next().unwrap()).await["status"],
        "succeeded"
    );
    assert_eq!(count(&env.state, "documents").await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_import_whose_client_goes_away_after_the_claim_still_runs() {
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    CLAIM_GATES
        .lock()
        .unwrap()
        .push((id.clone(), entered.clone(), release.clone()));
    let request = tokio::spawn({
        let (state, id) = (env.state.clone(), id.clone());
        async move { import(&state, json!({ "upload_id": id })).await }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    release.notify_one();
    let storage = env.state.storage().await.unwrap();
    let job_id: String = sqlx::query_scalar("SELECT id FROM jobs")
        .fetch_one(storage.db.pool())
        .await
        .unwrap();
    assert_eq!(settled(&env.state, &job_id).await["status"], "succeeded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replay_of_a_working_job_returns_it_unchanged_as_202() {
    for status in ["queued", "running", "cancelling"] {
        let env = env().await;
        let id = upload(&env.state, PDF, "a.pdf").await;
        let job_id = left_behind(&env.state, &id, status).await;
        let (code, v) = import(&env.state, json!({ "upload_id": id })).await;
        assert_eq!(code, StatusCode::ACCEPTED, "{status}: {v}");
        assert_eq!(
            (v["status"].as_str(), v["attempt"].as_i64()),
            (Some(status), Some(1)),
            "{status}"
        );
        assert_eq!(v["job_id"], job_id);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_import_is_queued_again_with_its_error_and_progress_cleared() {
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let job_id = left_behind(&env.state, &id, "failed").await;
    let (code, v) = import(&env.state, json!({ "upload_id": id })).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{v}");
    assert_eq!(
        (v["status"].as_str(), v["attempt"].as_i64()),
        (Some("queued"), Some(2))
    );
    assert_eq!(
        (v["error"].clone(), v["progress"].clone()),
        (Value::Null, Value::Null)
    );
    assert_eq!(settled(&env.state, &job_id).await["status"], "succeeded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replay_answers_with_the_job_as_its_commit_left_it() {
    // The re-queued attempt finishes before the answer goes out: still 202,
    // showing the job queued.
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let job_id = left_behind(&env.state, &id, "interrupted").await;
    let (entered, release) = gate_in(&REPLAY_GATES, &job_id);
    let request = tokio::spawn({
        let (state, id) = (env.state.clone(), id.clone());
        async move { import(&state, json!({ "upload_id": id })).await }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    assert_eq!(settled(&env.state, &job_id).await["status"], "succeeded");
    release.notify_one();
    let (code, v) = request.await.unwrap();
    assert_eq!(
        (code, v["status"].as_str()),
        (StatusCode::ACCEPTED, Some("queued")),
        "{v}"
    );

    // A running job fails after the replay looked at it: the answer is the
    // running job, not a failed one with a 202 that re-queued nothing.
    let env = super::tests::env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let job_id = left_behind(&env.state, &id, "running").await;
    let (entered, release) = gate_in(&REPLAY_GATES, &job_id);
    let request = tokio::spawn({
        let (state, id) = (env.state.clone(), id.clone());
        async move { import(&state, json!({ "upload_id": id })).await }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    exec(&env.state, &format!(
        "UPDATE jobs SET status = 'failed', error = json_object('code', 'parse_failed', 'message', 'x') WHERE id = '{job_id}'"
    ))
    .await;
    release.notify_one();
    let (code, v) = request.await.unwrap();
    assert_eq!(
        (code, v["status"].as_str()),
        (StatusCode::ACCEPTED, Some("running")),
        "{v}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn any_title_of_1_to_500_characters_is_kept_as_sent() {
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let title = format!("a\u{0}b{}", "文".repeat(497));
    let (code, v) = import(&env.state, json!({ "upload_id": id, "title": title })).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{v}");
    let job = settled(&env.state, v["job_id"].as_str().unwrap()).await;
    let storage = env.state.storage().await.unwrap();
    let stored: String = sqlx::query_scalar("SELECT title FROM documents WHERE id = ?")
        .bind(job["result"]["document_id"].as_str().unwrap())
        .fetch_one(storage.db.pool())
        .await
        .unwrap();
    assert_eq!(stored, title);
    // And the same request replays.
    let (code, _) = import(&env.state, json!({ "upload_id": id, "title": title })).await;
    assert_eq!(code, StatusCode::OK);
}

/// Two first imports of one upload that both found no key: the write lock
/// lets one claim it; the other replays it, or is refused if it differs
/// (Codex review).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_imports_that_race_past_the_lookup_claim_the_key_once() {
    for titles in [[None, None], [Some("a"), Some("b")]] {
        let env = env().await;
        let id = upload(&env.state, PDF, "a.pdf").await;
        let gates = [gate_in(&LOOKUP_GATES, &id), gate_in(&LOOKUP_GATES, &id)];
        let calls: Vec<_> = titles
            .iter()
            .map(|title| {
                let (state, mut body) = (env.state.clone(), json!({ "upload_id": id }));
                if let Some(t) = title {
                    body["title"] = json!(t);
                }
                tokio::spawn(async move { import(&state, body).await })
            })
            .collect();
        for (entered, _) in &gates {
            tokio::time::timeout(Duration::from_secs(5), entered.notified())
                .await
                .unwrap();
        }
        for (_, release) in &gates {
            release.notify_one();
        }
        let mut answers = Vec::new();
        for c in calls {
            answers.push(c.await.unwrap());
        }
        assert_eq!(count(&env.state, "jobs").await, 1, "{titles:?}");
        let storage = env.state.storage().await.unwrap();
        let keys: i64 =
            sqlx::query_scalar("SELECT count(*) FROM idempotency WHERE kind = 'import'")
                .fetch_one(storage.db.pool())
                .await
                .unwrap();
        assert_eq!(keys, 1, "{titles:?}");
        let codes: Vec<_> = answers.iter().map(|(code, _)| *code).collect();
        if titles[0] == titles[1] {
            // The claim is 202; the replay is 202 while the job works, or
            // 200 if the worker has already finished it (#838 CI).
            assert!(codes.contains(&StatusCode::ACCEPTED), "{answers:?}");
            assert!(
                codes
                    .iter()
                    .all(|c| matches!(*c, StatusCode::ACCEPTED | StatusCode::OK)),
                "{answers:?}"
            );
            assert_eq!(answers[0].1["job_id"], answers[1].1["job_id"]);
        } else {
            assert!(codes.contains(&StatusCode::ACCEPTED), "{answers:?}");
            let refused = answers
                .iter()
                .find(|(code, _)| *code == StatusCode::UNPROCESSABLE_ENTITY)
                .expect("one is refused");
            assert_eq!(refused.1["error"]["code"], "idempotency_key_reused");
        }
    }
}

/// A failed import re-queued by a replay whose client goes away before the
/// worker starts still runs attempt 2 (Codex review).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_requeue_whose_client_goes_away_before_its_worker_starts_still_runs() {
    let env = env().await;
    let id = upload(&env.state, PDF, "a.pdf").await;
    let job_id = left_behind(&env.state, &id, "failed").await;
    let (entered, release) = gate_in(&REQUEUE_GATES, &job_id);
    let request = tokio::spawn({
        let (state, id) = (env.state.clone(), id.clone());
        async move { import(&state, json!({ "upload_id": id })).await }
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
