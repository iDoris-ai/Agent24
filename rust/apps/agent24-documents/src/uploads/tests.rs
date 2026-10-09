#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::error::StorageCause;
use crate::state::AppState;

const SHA: &str = "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

struct Env {
    _dir: tempfile::TempDir,
    state: AppState,
}

async fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    Env { _dir: dir, state }
}

async fn post(state: &AppState, key: Option<&str>, body: &str) -> (StatusCode, Value) {
    let mut req = Request::post("/uploads").header("content-type", "application/json");
    if let Some(key) = key {
        req = req.header("idempotency-key", key);
    }
    let res = crate::router(state.clone())
        .oneshot(req.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn body(total: i64, filename: Option<&str>) -> String {
    let mut v = json!({ "total_size": total, "sha256": SHA });
    if let Some(name) = filename {
        v["filename"] = json!(name);
    }
    v.to_string()
}

async fn count(state: &AppState, sql: &str) -> i64 {
    let storage = state.storage().await.unwrap();
    sqlx::query_scalar(sql)
        .fetch_one(storage.db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_new_upload_is_201_receiving_and_expires_in_24_hours() {
    let env = env().await;
    let (status, v) = post(&env.state, Some("k1"), &body(10, Some("报告.pdf"))).await;
    assert_eq!(status, StatusCode::CREATED);
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "expires_at",
            "received",
            "sha256",
            "status",
            "total_size",
            "upload_id"
        ]
    );
    let id = v["upload_id"].as_str().unwrap();
    assert!(id.starts_with("upl_") && id.len() == 30, "{id}");
    assert_eq!(
        (v["total_size"].as_i64(), v["received"].as_i64()),
        (Some(10), Some(0))
    );
    assert_eq!(
        (v["sha256"].as_str(), v["status"].as_str()),
        (Some(SHA), Some("receiving"))
    );
    let expires = v["expires_at"].as_str().unwrap();
    assert!(expires.ends_with('Z') && expires.len() == 24, "{expires}");
    let hours = count(
        &env.state,
        &format!("SELECT CAST(round((julianday('{expires}') - julianday('now')) * 24) AS INTEGER)"),
    )
    .await;
    assert_eq!(hours, 24);
    let stored = count(
        &env.state,
        &format!("SELECT count(*) FROM uploads WHERE id = '{id}' AND filename = '报告.pdf'"),
    )
    .await;
    assert_eq!(stored, 1);
}

#[tokio::test]
async fn the_same_key_and_request_replay_the_upload() {
    let env = env().await;
    let (_, first) = post(&env.state, Some("k1"), &body(10, Some("a.pdf"))).await;
    // The same request written differently is the same request (JCS).
    let reordered = format!(r#"{{ "filename": "a.pdf", "sha256": "{SHA}", "total_size": 10 }}"#);
    let (status, again) = post(&env.state, Some("k1"), &reordered).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again, first);
    assert_eq!(count(&env.state, "SELECT count(*) FROM uploads").await, 1);
}

#[tokio::test]
async fn the_same_key_with_another_request_is_422() {
    let env = env().await;
    post(&env.state, Some("k1"), &body(10, Some("a.pdf"))).await;
    for other in [
        body(11, Some("a.pdf")),
        body(10, Some("b.pdf")),
        body(10, None),
    ] {
        let (status, v) = post(&env.state, Some("k1"), &other).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{other}");
        assert_eq!(v["error"]["code"], "idempotency_key_reused");
    }
    // Another key is another upload.
    let (status, _) = post(&env.state, Some("k2"), &body(10, Some("a.pdf"))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(count(&env.state, "SELECT count(*) FROM uploads").await, 2);
}

/// Transactions attempted and lookups that missed, per `race-` key.
static ATTEMPTS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static MISSES: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Whether a second request had attempted its transaction while the first
/// sat between its lookup and its claim.
static OVERLAPPED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn tally(list: &Mutex<Vec<String>>, key: &str) -> usize {
    list.lock().unwrap().iter().filter(|k| *k == key).count()
}

pub(super) fn attempting(key: &str) {
    if key.starts_with("race-") {
        ATTEMPTS.lock().unwrap().push(key.to_owned());
    }
}

/// After a miss, hold the transaction until another request has attempted
/// its own (or 2 s pass). With the write lock taken first, that request
/// waits in BEGIN; with a deferred BEGIN it would read the key table too.
pub(super) async fn missed_lookup(key: &str) {
    if !key.starts_with("race-") {
        return;
    }
    MISSES.lock().unwrap().push(key.to_owned());
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(2) {
        if tally(&ATTEMPTS, key) >= 2 {
            OVERLAPPED.lock().unwrap().push(key.to_owned());
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_with_one_key_create_one_upload() {
    let env = env().await;
    // Open every pooled connection first; otherwise opening them serializes
    // the requests and the race below never happens.
    {
        let storage = env.state.storage().await.unwrap();
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(storage.db.pool().acquire().await.unwrap());
        }
    }
    let calls = (0..8).map(|_| {
        let state = env.state.clone();
        tokio::spawn(async move { post(&state, Some("race-1"), &body(10, None)).await })
    });
    // Spawn them all before awaiting any: `map` alone is lazy.
    let calls: Vec<_> = calls.collect();
    let mut created = 0;
    let mut ids = std::collections::HashSet::new();
    for call in calls {
        let (status, v) = call.await.unwrap();
        match status {
            StatusCode::CREATED => created += 1,
            StatusCode::OK => {}
            other => panic!("{other}: {v}"),
        }
        ids.insert(v["upload_id"].as_str().unwrap().to_owned());
    }
    assert_eq!((created, ids.len()), (1, 1));
    assert_eq!(count(&env.state, "SELECT count(*) FROM uploads").await, 1);
    // The write lock is taken before the lookup, so only the first missed.
    assert_eq!(
        tally(&OVERLAPPED, "race-1"),
        1,
        "the requests never overlapped"
    );
    assert_eq!(tally(&MISSES, "race-1"), 1);
}

#[tokio::test]
async fn a_bad_key_or_body_is_400_and_creates_nothing() {
    let env = env().await;
    let long = "k".repeat(201);
    for key in [
        None,
        Some(""),
        Some(long.as_str()),
        Some("a b"),
        Some("a\tb"),
        Some("é"), // obs-text: a valid header value, but not visible ASCII
    ] {
        let (status, v) = post(&env.state, key, &body(10, None)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{key:?}");
        assert_eq!(v["error"]["code"], "invalid_request");
    }
    let bad = [
        body(0, None),
        body(1 << 53, None),
        body(10, Some("a/b.pdf")),
        body(10, Some("a\\b.pdf")),
        body(10, Some("a\u{0}.pdf")),
        body(10, Some("")),
        body(10, Some(&"x".repeat(256))),
        json!({ "total_size": 10, "sha256": SHA.to_uppercase() }).to_string(),
        json!({ "total_size": 10, "sha256": format!("sha256:{}", SHA[7..].to_uppercase()) })
            .to_string(),
        json!({ "total_size": 10, "sha256": "sha256:abc" }).to_string(),
        json!({ "total_size": 10 }).to_string(),
        json!({ "total_size": 10, "sha256": SHA, "extra": 1 }).to_string(),
        json!({ "total_size": "10", "sha256": SHA }).to_string(),
        "not json".to_owned(),
    ];
    for b in bad {
        let (status, v) = post(&env.state, Some("k1"), &b).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{b}");
        assert_eq!(v["error"]["code"], "invalid_request", "{b}");
        assert_eq!(v["error"]["details"]["retryable"], false);
    }
    // 255 characters is fine, though it is 765 bytes.
    let (status, _) = post(&env.state, Some("k1"), &body(10, Some(&"文".repeat(255)))).await;
    assert_eq!(status, StatusCode::CREATED);
    // So is a 200-character key.
    let (status, _) = post(&env.state, Some(&"k".repeat(200)), &body(10, None)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(count(&env.state, "SELECT count(*) FROM uploads").await, 2);
    assert_eq!(
        count(&env.state, "SELECT count(*) FROM idempotency").await,
        2
    );
}

#[tokio::test]
async fn unavailable_storage_is_503_with_the_cause() {
    let state = AppState::unavailable(StorageCause::DiskFull);
    let (status, v) = post(&state, Some("k1"), &body(10, None)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        v["error"]["details"],
        json!({ "retryable": false, "cause": "disk_full" })
    );
}

#[tokio::test]
async fn a_null_filename_is_not_an_absent_one() {
    let env = env().await;
    let null = json!({ "total_size": 10, "sha256": SHA, "filename": null }).to_string();
    let (status, v) = post(&env.state, Some("k1"), &null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    // Nor on replay of an upload created without a filename.
    post(&env.state, Some("k2"), &body(10, None)).await;
    let (status, _) = post(&env.state, Some("k2"), &null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_size_must_be_an_integer_literal() {
    let env = env().await;
    // A fraction or exponent spelling can be rounded by the parser, e.g.
    // 9007199254740991.0 to …990 and 4503599627370495.5 to …495.
    for bad in [
        "10.0",
        "1e1",
        "9007199254740991.0",
        "4503599627370495.5",
        "-1",
        "0",
    ] {
        let raw = format!(r#"{{"total_size":{bad},"sha256":"{SHA}"}}"#);
        let (status, v) = post(&env.state, Some("k1"), &raw).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(v["error"]["code"], "invalid_request");
    }
    let raw = format!(r#"{{"total_size":9007199254740991,"sha256":"{SHA}"}}"#);
    let (status, v) = post(&env.state, Some("k1"), &raw).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(v["total_size"], 9_007_199_254_740_991_i64);
}

#[tokio::test]
async fn a_replay_returns_the_upload_as_it_is_now() {
    let env = env().await;
    let (_, first) = post(&env.state, Some("k1"), &body(10, None)).await;
    let id = first["upload_id"].as_str().unwrap();
    let storage = env.state.storage().await.unwrap();
    for sql in [
        format!(
            "INSERT INTO upload_chunks (upload_id, chunk_offset, chunk_size, sha256) VALUES ('{id}', 0, 4, '{SHA}')"
        ),
        format!(
            "UPDATE uploads SET received = 4, last_chunk_at = '2030-01-02T03:04:05.678Z' WHERE id = '{id}'"
        ),
    ] {
        sqlx::query(&sql).execute(storage.db.pool()).await.unwrap();
    }
    let (status, again) = post(&env.state, Some("k1"), &body(10, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["received"], 4);
    assert_eq!(again["expires_at"], "2030-01-03T03:04:05.678Z");
}

#[tokio::test]
async fn a_database_held_by_another_writer_is_503_busy() {
    let env = env().await;
    let storage = env.state.storage().await.unwrap();
    let mut holder = storage.db.pool().acquire().await.unwrap();
    sqlx::query("BEGIN EXCLUSIVE")
        .execute(&mut *holder)
        .await
        .unwrap();
    // Waits out the 5 s busy timeout.
    let (status, v) = post(&env.state, Some("k1"), &body(10, None)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(
        v["error"]["details"],
        json!({ "retryable": true, "cause": "busy" })
    );
    sqlx::query("ROLLBACK").execute(&mut *holder).await.unwrap();
}

#[tokio::test]
async fn a_body_without_a_json_content_type_is_400() {
    let env = env().await;
    let req = Request::post("/uploads")
        .header("idempotency-key", "k1")
        .body(Body::from(body(10, None)))
        .unwrap();
    let res = crate::router(env.state.clone()).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["error"]["code"], "invalid_request");
}
