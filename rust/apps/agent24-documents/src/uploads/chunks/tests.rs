#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Mutex, mpsc};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{MAX_CHUNK, sha256_address};
use crate::error::StorageCause;
use crate::state::AppState;
use crate::uploads::data::hooks::{DIR_SYNCS, FAIL_DIR_SYNC, GATES, Gate};

struct Env {
    dir: tempfile::TempDir,
    state: AppState,
}

async fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    Env { dir, state }
}

async fn send(state: &AppState, req: Request<Body>) -> (StatusCode, Value) {
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

/// A new upload of `total` bytes whose whole-file hash is that of `whole`.
async fn upload(state: &AppState, whole: &[u8]) -> String {
    let body = json!({ "total_size": whole.len(), "sha256": sha256_address(whole) });
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
    v["upload_id"].as_str().unwrap().to_owned()
}

async fn chunk(
    state: &AppState,
    id: &str,
    offset: &str,
    sha: &str,
    bytes: &[u8],
) -> (StatusCode, Value) {
    let req = Request::post(format!("/uploads/{id}/chunks"))
        .header("content-type", "application/octet-stream")
        .header("upload-offset", offset)
        .header("chunk-sha256", sha)
        .body(Body::from(bytes.to_vec()))
        .unwrap();
    send(state, req).await
}

/// The next chunk of `bytes`, at `offset`, with its own hash.
async fn put(state: &AppState, id: &str, offset: usize, bytes: &[u8]) -> (StatusCode, Value) {
    chunk(
        state,
        id,
        &offset.to_string(),
        &sha256_address(bytes),
        bytes,
    )
    .await
}

fn data(env: &Env, id: &str) -> Option<Vec<u8>> {
    std::fs::read(env.dir.path().join("uploads").join(id).join("data")).ok()
}

async fn scalar(state: &AppState, sql: &str) -> i64 {
    let storage = state.storage().await.unwrap();
    sqlx::query_scalar(sql)
        .fetch_one(storage.db.pool())
        .await
        .unwrap()
}

const WHOLE: &[u8] = b"0123456789";

/// Upload ids, once per request that started waiting for its turn, and once
/// per operation that got one.
static WAITING: Mutex<Vec<String>> = Mutex::new(Vec::new());
static STARTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub(super) fn waiting_for_turn(id: &str) {
    WAITING.lock().unwrap().push(id.to_owned());
}

pub(super) fn operation_started(id: &str) {
    STARTED.lock().unwrap().push(id.to_owned());
}

fn count(list: &Mutex<Vec<String>>, id: &str) -> usize {
    list.lock().unwrap().iter().filter(|x| *x == id).count()
}

/// Waits until `n` requests for `id` have started waiting for their turn.
async fn until_waiting(id: &str, n: usize) {
    let start = std::time::Instant::now();
    while count(&WAITING, id) < n {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "no request waited"
        );
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
}

#[tokio::test]
async fn chunks_append_until_the_upload_is_complete() {
    let env = env().await;
    let id = upload(&env.state, WHOLE).await;
    let (status, v) = put(&env.state, &id, 0, &WHOLE[..4]).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        (v["received"].as_i64(), v["status"].as_str()),
        (Some(4), Some("receiving"))
    );
    let (status, v) = put(&env.state, &id, 4, &WHOLE[4..]).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        (v["received"].as_i64(), v["status"].as_str()),
        (Some(10), Some("complete"))
    );
    assert_eq!(data(&env, &id).unwrap(), WHOLE);
    let rows = scalar(
        &env.state,
        &format!("SELECT count(*) FROM upload_chunks WHERE upload_id = '{id}'"),
    )
    .await;
    assert_eq!(rows, 2);
}

#[tokio::test]
async fn a_chunk_already_received_replays_with_its_hash_and_conflicts_otherwise() {
    let env = env().await;
    let id = upload(&env.state, WHOLE).await;
    put(&env.state, &id, 0, &WHOLE[..4]).await;
    put(&env.state, &id, 4, &WHOLE[4..7]).await;
    let (status, v) = put(&env.state, &id, 0, &WHOLE[..4]).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["received"], 7);
    // Another hash for a received range is 409, as is an offset inside a chunk.
    let (status, v) = put(&env.state, &id, 0, b"abcd").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "upload_offset_mismatch");
    assert_eq!(
        v["error"]["details"],
        json!({ "retryable": false, "received_offset": 7 })
    );
    let (status, _) = put(&env.state, &id, 2, &WHOLE[2..4]).await;
    assert_eq!(status, StatusCode::CONFLICT);
    // A gap past the bytes received is 409 too.
    let (status, v) = put(&env.state, &id, 8, &WHOLE[8..]).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(v["error"]["details"]["received_offset"], 7);
    assert_eq!(data(&env, &id).unwrap(), &WHOLE[..7]);
}

#[tokio::test]
async fn a_new_chunk_must_match_its_hash_and_fit_the_size() {
    let env = env().await;
    let id = upload(&env.state, WHOLE).await;
    let (status, v) = chunk(&env.state, &id, "0", &sha256_address(b"other"), &WHOLE[..4]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "invalid_request");
    let (status, _) = put(&env.state, &id, 0, b"0123456789AB").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(data(&env, &id), None, "nothing was written");
    assert_eq!(scalar(&env.state, "SELECT received FROM uploads").await, 0);
    // A complete upload takes no more chunks.
    put(&env.state, &id, 0, WHOLE).await;
    let (status, v) = put(&env.state, &id, 10, b"x").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(v["error"]["details"]["received_offset"], 10);
}

#[tokio::test]
async fn bad_requests_are_400_unknown_uploads_404() {
    let env = env().await;
    let id = upload(&env.state, WHOLE).await;
    let sha = sha256_address(&WHOLE[..4]);
    for offset in ["", "-1", "+1", " 1", "1.0", "0x1", "9007199254740992"] {
        let (status, _) = chunk(&env.state, &id, offset, &sha, &WHOLE[..4]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{offset:?}");
    }
    for bad_sha in ["", "sha256:abc", &sha.to_uppercase()] {
        let (status, _) = chunk(&env.state, &id, "0", bad_sha, &WHOLE[..4]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad_sha:?}");
    }
    let (status, _) = chunk(&env.state, &id, "0", &sha256_address(b""), b"").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "empty body");
    let (status, _) = chunk(&env.state, "upl_x", "0", &sha, &WHOLE[..4]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let other = "upl_01K74Z3QJ8V5N2W9RTX6YB4MCD";
    let (status, v) = chunk(&env.state, other, "0", &sha, &WHOLE[..4]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(v["error"]["code"], "not_found");
    assert_eq!(scalar(&env.state, "SELECT received FROM uploads").await, 0);
}

#[tokio::test]
async fn a_chunk_over_768_kib_is_413() {
    let env = env().await;
    let big = vec![7u8; MAX_CHUNK + 1];
    let id = upload(&env.state, &big).await;
    let (status, v) = put(&env.state, &id, 0, &big).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(v["error"]["code"], "payload_too_large");
    let (status, v) = put(&env.state, &id, 0, &big[..MAX_CHUNK]).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["received"], MAX_CHUNK);
}

#[tokio::test]
async fn an_upload_24_hours_after_its_last_chunk_is_expired() {
    let env = env().await;
    let id = upload(&env.state, WHOLE).await;
    put(&env.state, &id, 0, &WHOLE[..4]).await;
    let storage = env.state.storage().await.unwrap();
    sqlx::query("UPDATE uploads SET last_chunk_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-24 hours', '-1 second')")
        .execute(storage.db.pool())
        .await
        .unwrap();
    let (status, v) = put(&env.state, &id, 4, &WHOLE[4..]).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
    let status: String = sqlx::query_scalar("SELECT status FROM uploads")
        .fetch_one(storage.db.pool())
        .await
        .unwrap();
    assert_eq!(status, "expired");
    // Even a replay of a received chunk is gone with it.
    let (status, _) = put(&env.state, &id, 0, &WHOLE[..4]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn bytes_a_crash_left_past_received_are_overwritten() {
    let env = env().await;
    let id = upload(&env.state, WHOLE).await;
    put(&env.state, &id, 0, &WHOLE[..4]).await;
    // Written but never counted: as if the process died before the commit.
    let path = env.dir.path().join("uploads").join(&id).join("data");
    std::fs::write(&path, b"0123XXXXXXXXXXXX").unwrap();
    let (status, _) = put(&env.state, &id, 4, &WHOLE[4..6]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data(&env, &id).unwrap(), &WHOLE[..6]);
}

#[tokio::test]
async fn data_shorter_than_received_is_storage_corruption() {
    let env = env().await;
    let id = upload(&env.state, WHOLE).await;
    put(&env.state, &id, 0, &WHOLE[..4]).await;
    let path = env.dir.path().join("uploads").join(&id).join("data");
    std::fs::write(&path, b"01").unwrap();
    let (status, v) = put(&env.state, &id, 4, &WHOLE[4..]).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        v["error"]["details"],
        json!({ "retryable": false, "cause": "corrupt" })
    );
    // Not padded with zeros to make it fit.
    assert_eq!(data(&env, &id).unwrap(), b"01");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_chunks_at_one_offset_append_once() {
    let env = env().await;
    let id = upload(&env.state, WHOLE).await;
    let bodies: Vec<Vec<u8>> = (0..8u8).map(|i| vec![b'a' + i; 4]).collect();
    let calls: Vec<_> = bodies
        .iter()
        .cloned()
        .map(|b| {
            let (state, id) = (env.state.clone(), id.clone());
            tokio::spawn(async move { (put(&state, &id, 0, &b).await.0, b) })
        })
        .collect();
    let mut winners = Vec::new();
    for call in calls {
        let (status, b) = call.await.unwrap();
        match status {
            StatusCode::OK => winners.push(b),
            StatusCode::CONFLICT => {}
            other => panic!("{other}"),
        }
    }
    assert_eq!(winners.len(), 1);
    assert_eq!(data(&env, &id).unwrap(), winners[0]);
    let stored: String = {
        let storage = env.state.storage().await.unwrap();
        sqlx::query_scalar("SELECT sha256 FROM upload_chunks")
            .fetch_one(storage.db.pool())
            .await
            .unwrap()
    };
    assert_eq!(stored, sha256_address(&winners[0]));
}

#[tokio::test]
async fn unavailable_storage_is_503() {
    let state = AppState::unavailable(StorageCause::Locked);
    let (status, v) = chunk(
        &state,
        "upl_01K74Z3QJ8V5N2W9RTX6YB4MCD",
        "0",
        &sha256_address(b"x"),
        b"x",
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(v["error"]["details"]["cause"], "locked");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_goes_away_mid_write_keeps_the_turn_until_the_write_is_done() {
    let env = env().await;
    let id = upload(&env.state, WHOLE).await;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    GATES.lock().unwrap().push(Gate {
        id: id.clone(),
        entered: entered_tx,
        release: release_rx,
    });
    let a = tokio::spawn({
        let (state, id) = (env.state.clone(), id.clone());
        async move { put(&state, &id, 0, b"AAAA").await }
    });
    tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
        .await
        .unwrap();
    // The client of A disconnects while A's bytes are being written.
    a.abort();
    assert!(a.await.unwrap_err().is_cancelled());
    let b = tokio::spawn({
        let (state, id) = (env.state.clone(), id.clone());
        async move { put(&state, &id, 0, b"BBBB").await }
    });
    // A's request and B's have both asked for the turn; A's operation has it.
    until_waiting(&id, 2).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!b.is_finished(), "B ran while A's write was still going");
    assert_eq!(count(&STARTED, &id), 1);
    release_tx.send(()).unwrap();
    let (status, v) = b.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(data(&env, &id).unwrap(), b"AAAA");
    let storage = env.state.storage().await.unwrap();
    let stored: String = sqlx::query_scalar("SELECT sha256 FROM upload_chunks")
        .fetch_one(storage.db.pool())
        .await
        .unwrap();
    assert_eq!(stored, sha256_address(b"AAAA"));
}

#[tokio::test]
async fn a_retry_after_a_failed_directory_sync_syncs_the_directories() {
    let env = env().await;
    let id = upload(&env.state, WHOLE).await;
    FAIL_DIR_SYNC.lock().unwrap().push(id.clone());
    let (status, v) = put(&env.state, &id, 0, &WHOLE[..4]).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(scalar(&env.state, "SELECT received FROM uploads").await, 0);
    let before = DIR_SYNCS.lock().unwrap().len();
    let (status, v) = put(&env.state, &id, 0, &WHOLE[..4]).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let synced: Vec<String> = DIR_SYNCS.lock().unwrap()[before..].to_vec();
    let upload_dir = env.dir.path().join("uploads").join(&id);
    for dir in [
        upload_dir.clone(),
        env.dir.path().join("uploads"),
        env.dir.path().to_owned(),
    ] {
        let dir = dir.display().to_string();
        assert!(
            synced.contains(&dir),
            "{dir} not synced on the retry: {synced:?}"
        );
    }
}

#[tokio::test]
async fn a_received_range_is_judged_by_its_header_hash_before_the_body() {
    let env = env().await;
    let id = upload(&env.state, WHOLE).await;
    put(&env.state, &id, 0, &WHOLE[..4]).await;
    // The stored hash replays even if the body differs; nothing is written.
    let (status, _) = chunk(&env.state, &id, "0", &sha256_address(&WHOLE[..4]), b"zzzz").await;
    assert_eq!(status, StatusCode::OK);
    // Another valid hash there is 409, not the new chunk's 400.
    let (status, _) = chunk(&env.state, &id, "0", &sha256_address(b"q"), b"zzzz").await;
    assert_eq!(status, StatusCode::CONFLICT);
    // So is a gap, whatever the body.
    let (status, _) = chunk(&env.state, &id, "6", &sha256_address(b"q"), b"zzzz").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(data(&env, &id).unwrap(), &WHOLE[..4]);
}

#[tokio::test]
async fn an_append_moves_the_expiry_to_24_hours_after_it() {
    let env = env().await;
    let id = upload(&env.state, WHOLE).await;
    put(&env.state, &id, 0, &WHOLE[..4]).await;
    let storage = env.state.storage().await.unwrap();
    sqlx::query(
        "UPDATE uploads SET last_chunk_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-23 hours')",
    )
    .execute(storage.db.pool())
    .await
    .unwrap();
    let (status, v) = put(&env.state, &id, 4, &WHOLE[4..]).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let expires = v["expires_at"].as_str().unwrap();
    let hours = scalar(
        &env.state,
        &format!("SELECT CAST(round((julianday('{expires}') - julianday('now')) * 24) AS INTEGER)"),
    )
    .await;
    assert_eq!(hours, 24);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_goes_away_while_waiting_leaves_nothing_behind() {
    let env = env().await;
    let id = upload(&env.state, WHOLE).await;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    GATES.lock().unwrap().push(Gate {
        id: id.clone(),
        entered: entered_tx,
        release: release_rx,
    });
    let a = tokio::spawn({
        let (state, id) = (env.state.clone(), id.clone());
        async move { put(&state, &id, 0, &WHOLE[..4]).await }
    });
    tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
        .await
        .unwrap();
    // B queues for the turn, then its client disconnects.
    let b = tokio::spawn({
        let (state, id) = (env.state.clone(), id.clone());
        async move { put(&state, &id, 4, &WHOLE[4..]).await }
    });
    until_waiting(&id, 2).await;
    b.abort();
    assert!(b.await.unwrap_err().is_cancelled());
    release_tx.send(()).unwrap();
    assert_eq!(a.await.unwrap().0, StatusCode::OK);
    // The mutex is fair: a detached B would have run before C.
    let (status, v) = put(&env.state, &id, 4, &WHOLE[4..6]).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(count(&STARTED, &id), 2, "only A and C ever ran");
    assert_eq!(data(&env, &id).unwrap(), &WHOLE[..6]);
}
