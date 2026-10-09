#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::error::StorageCause;
use crate::state::AppState;

use super::Row;

const JOB: &str = "job_01K75A0B1C2D3E4F5G6H7J8K9M";
const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4MCD";
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

async fn exec(state: &AppState, sql: &str) {
    let storage = state.storage().await.unwrap();
    sqlx::query(sql).execute(storage.db.pool()).await.unwrap();
}

/// The error a job in `status` carries, as the contract has it.
fn error_for(status: &str) -> &'static str {
    match status {
        "failed" => "json_object('code', 'parse_failed', 'message', 'x')",
        "cancelled" => "json_object('code', 'cancelled', 'message', 'x')",
        _ => "NULL",
    }
}

async fn add_job_as(state: &AppState, id: &str, status: &str) {
    let error = error_for(status);
    exec(
        state,
        &format!(
            "INSERT INTO jobs (id, kind, status, origin, error)
             VALUES ('{id}', 'import', '{status}', '{{\"kind\":\"page\"}}', {error})"
        ),
    )
    .await;
}

async fn add_job(state: &AppState, status: &str) {
    add_job_as(state, JOB, status).await;
}

/// A succeeded import needs its document and r1 (0002 triggers).
async fn add_succeeded_import(state: &AppState) {
    exec(
        state,
        &format!(
            "BEGIN;
             INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{DOC}', 't', 'application/pdf', 1);
             INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                    VALUES ('{DOC}', 1, '{SHA}', 1, 'application/pdf', 'import');
             INSERT INTO jobs (id, kind, status, origin, document_id, revision, progress)
                    VALUES ('{JOB}', 'import', 'succeeded', '{{\"kind\":\"page\"}}', '{DOC}', 1,
                            '{{\"stage\":\"store\",\"done\":1,\"total\":1,\"unit\":\"file\"}}');
             COMMIT;"
        ),
    )
    .await;
}

async fn call(state: &AppState, method: &str, path: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let res = crate::router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn a_job_is_read_in_the_contract_shape() {
    let env = env().await;
    add_job(&env.state, "queued").await;
    let (status, v) = call(&env.state, "GET", &format!("/jobs/{JOB}")).await;
    assert_eq!(status, StatusCode::OK);
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    // No document or revision yet: absent, not null (the schema has no null).
    assert_eq!(
        keys,
        [
            "attempt",
            "created_at",
            "error",
            "job_id",
            "kind",
            "progress",
            "result",
            "status",
            "updated_at"
        ]
    );
    assert_eq!(v["job_id"], JOB);
    assert_eq!(
        (v["kind"].as_str(), v["status"].as_str()),
        (Some("import"), Some("queued"))
    );
    assert_eq!(v["progress"], Value::Null);
    assert_eq!(v["error"], Value::Null);
    assert_eq!(v["result"], Value::Null);
}

#[tokio::test]
async fn a_succeeded_import_carries_its_document_and_r1_as_the_result() {
    let env = env().await;
    add_succeeded_import(&env.state).await;
    let (_, v) = call(&env.state, "GET", &format!("/jobs/{JOB}")).await;
    assert_eq!(v["result"], json!({ "document_id": DOC, "revision": 1 }));
    assert_eq!(
        (v["document_id"].as_str(), v["revision"].as_i64()),
        (Some(DOC), Some(1))
    );
    assert_eq!(
        v["progress"],
        json!({ "stage": "store", "done": 1, "total": 1, "unit": "file" })
    );
}

#[tokio::test]
async fn unknown_bad_and_unavailable() {
    let env = env().await;
    for (method, path) in [
        ("GET", format!("/jobs/{JOB}")),
        ("POST", format!("/jobs/{JOB}/cancel")),
        ("POST", format!("/jobs/{JOB}/retry")),
    ] {
        let (status, v) = call(&env.state, method, &path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(v["error"]["code"], "not_found");
        let bad = path.replace(JOB, "job_x");
        assert_eq!(
            call(&env.state, method, &bad).await.0,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
        let down = AppState::unavailable(StorageCause::Busy);
        assert_eq!(
            call(&down, method, &path).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}

#[tokio::test]
async fn cancel_is_idempotent_and_never_revives_or_undoes() {
    for (from, to, marker) in [
        ("queued", "cancelled", true),
        ("failed", "cancelled", true),
        ("interrupted", "cancelled", true),
        ("running", "cancelling", false),
        ("cancelling", "cancelling", false),
        ("cancelled", "cancelled", true),
        ("succeeded", "succeeded", false),
    ] {
        let env = env().await;
        if from == "succeeded" {
            add_succeeded_import(&env.state).await;
        } else {
            add_job(&env.state, from).await;
        }
        for round in 0..2 {
            // A no-op leaves the whole row alone, `updated_at` too (#831
            // review): from an old `updated_at`, so a write cannot hide in the
            // same millisecond.
            let no_op = round == 1 || from == to;
            let before = if no_op {
                exec(
                    &env.state,
                    "UPDATE jobs SET updated_at = '2000-01-01T00:00:00.000Z'",
                )
                .await;
                Some(call(&env.state, "GET", &format!("/jobs/{JOB}")).await.1)
            } else {
                None
            };
            let (status, v) = call(&env.state, "POST", &format!("/jobs/{JOB}/cancel")).await;
            assert_eq!(status, StatusCode::OK, "{from}");
            assert_eq!(v["status"], to, "{from}");
            if marker {
                assert_eq!(v["error"]["code"], "cancelled", "{from}");
            } else {
                assert_eq!(v["error"], Value::Null, "{from}");
            }
            if let Some(before) = before {
                assert_eq!(v, before, "{from}");
            }
        }
    }
}

#[tokio::test]
async fn retry_restarts_only_a_stopped_job() {
    for from in ["failed", "interrupted", "cancelled"] {
        let env = env().await;
        add_job(&env.state, from).await;
        exec(
            &env.state,
            "UPDATE jobs SET progress = json_object('stage', 's', 'done', 1, 'total', 2, 'unit', 'page')",
        )
        .await;
        let (status, v) = call(&env.state, "POST", &format!("/jobs/{JOB}/retry")).await;
        assert_eq!(status, StatusCode::OK, "{from}");
        assert_eq!(
            (v["status"].as_str(), v["attempt"].as_i64()),
            (Some("queued"), Some(2)),
            "{from}"
        );
        assert_eq!(
            (v["error"].clone(), v["progress"].clone()),
            (Value::Null, Value::Null)
        );
    }
    for from in ["queued", "running", "cancelling", "succeeded"] {
        let env = env().await;
        if from == "succeeded" {
            add_succeeded_import(&env.state).await;
        } else {
            add_job(&env.state, from).await;
        }
        let (_, before) = call(&env.state, "GET", &format!("/jobs/{JOB}")).await;
        let (status, v) = call(&env.state, "POST", &format!("/jobs/{JOB}/retry")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{from}");
        assert_eq!(
            v["error"]["details"],
            json!({ "retryable": false, "status": from })
        );
        let (_, after) = call(&env.state, "GET", &format!("/jobs/{JOB}")).await;
        assert_eq!(after, before, "{from}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_retries_restart_a_job_once() {
    let env = env().await;
    add_job(&env.state, "failed").await;
    let calls: Vec<_> = (0..8)
        .map(|_| {
            let state = env.state.clone();
            tokio::spawn(async move { call(&state, "POST", &format!("/jobs/{JOB}/retry")).await })
        })
        .collect();
    let mut restarted = 0;
    for c in calls {
        let (status, v) = c.await.unwrap();
        match status {
            StatusCode::OK => restarted += 1,
            StatusCode::BAD_REQUEST => assert_eq!(v["error"]["details"]["status"], "queued"),
            other => panic!("{other}: {v}"),
        }
    }
    assert_eq!(restarted, 1);
    let (_, v) = call(&env.state, "GET", &format!("/jobs/{JOB}")).await;
    assert_eq!(v["attempt"], 2);
}

#[tokio::test]
async fn a_row_that_breaks_the_status_rules_is_never_sent() {
    for (status, set) in [
        ("failed", "error = NULL"),
        (
            "failed",
            "error = json_object('code', 'oops', 'message', 'x')",
        ),
        ("failed", "error = json_object('code', 'parse_failed')"),
        (
            "cancelled",
            "error = json_object('code', 'parse_failed', 'message', 'x')",
        ),
        (
            "running",
            "error = json_object('code', 'parse_failed', 'message', 'x')",
        ),
        (
            "interrupted",
            "error = json_object('code', 'parse_failed', 'message', 'x')",
        ),
        (
            "queued",
            "progress = json_object('stage', 1, 'done', 0, 'total', 1, 'unit', 'p')",
        ),
        (
            "queued",
            "progress = json_object('stage', 's', 'done', -1, 'total', 1, 'unit', 'p')",
        ),
        ("succeeded", "error = NULL"),
        ("queued", "created_at = 'not-a-date'"),
        ("queued", "updated_at = '2026-02-30T00:00:00.000Z'"),
        ("queued", "created_at = '2026-10-09T24:59:59.999Z'"),
        ("queued", "updated_at = '2026-10-09T24:00:00.000Z'"),
        ("queued", "created_at = '-1000-10-09T00:00:00.000Z'"),
        ("queued", "updated_at = '-4712-01-01T12:00:00.000Z'"),
        ("queued", "created_at = '0300-02-29T00:00:00.000Z'"),
    ] {
        let env = env().await;
        if let Some(created) = set.strip_prefix("created_at = ") {
            // 0005 fixes the creation time once written, so set it at insert.
            exec(
                &env.state,
                &format!(
                    "INSERT INTO jobs (id, kind, status, origin, created_at)
                     VALUES ('{JOB}', 'import', '{status}', '{{\"kind\":\"page\"}}', {created})"
                ),
            )
            .await;
        } else {
            add_job(&env.state, "queued").await;
            exec(
                &env.state,
                &format!("UPDATE jobs SET status = '{status}', {set}"),
            )
            .await;
        }
        let (code, v) = call(&env.state, "GET", &format!("/jobs/{JOB}")).await;
        assert_eq!(
            code,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{status} {set}: {v}"
        );
        assert_eq!(v["error"]["code"], "internal");
    }
}

#[tokio::test]
async fn a_restart_interrupts_working_jobs_and_finishes_cancelling_ones() {
    let dir = tempfile::tempdir().unwrap();
    let ids = [
        ("job_01K75A0B1C2D3E4F5G6H7J8K90", "queued", "interrupted"),
        ("job_01K75A0B1C2D3E4F5G6H7J8K91", "running", "interrupted"),
        ("job_01K75A0B1C2D3E4F5G6H7J8K92", "cancelling", "cancelled"),
        ("job_01K75A0B1C2D3E4F5G6H7J8K93", "failed", "failed"),
        (
            "job_01K75A0B1C2D3E4F5G6H7J8K94",
            "interrupted",
            "interrupted",
        ),
    ];
    {
        let state = AppState::open(dir.path()).await;
        for (id, status, _) in ids {
            add_job_as(&state, id, status).await;
        }
    }
    let state = AppState::open(dir.path()).await;
    for (id, _, after) in ids {
        let (code, v) = call(&state, "GET", &format!("/jobs/{id}")).await;
        assert_eq!(code, StatusCode::OK, "{id}: {v}");
        assert_eq!(v["status"], after, "{id}");
        if after == "cancelled" {
            assert_eq!(v["error"]["code"], "cancelled");
        }
    }
}

#[tokio::test]
async fn an_id_with_hidden_bytes_after_a_nul_is_never_sent() {
    let env = env().await;
    let storage = env.state.storage().await.unwrap();
    let hidden = format!("{DOC}\u{0}junk");
    let mut tx = storage.db.pool().begin().await.unwrap();
    sqlx::query(
        "INSERT INTO documents (id, title, media_type, head_revision) VALUES (?, 't', 'application/pdf', 1)",
    )
    .bind(&hidden)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(&format!(
        "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
         VALUES (?, 1, '{SHA}', 1, 'application/pdf', 'import')"
    ))
    .bind(&hidden)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO jobs (id, kind, status, origin, document_id, revision)
         VALUES (?, 'import', 'succeeded', '{\"kind\":\"page\"}', ?, 1)",
    )
    .bind(JOB)
    .bind(&hidden)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (code, v) = call(&env.state, "GET", &format!("/jobs/{JOB}")).await;
    assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR, "{v}");
}

/// A valid queued row, for checking each guard on its own (Codex review).
fn row() -> Row {
    Row {
        id: JOB.into(),
        kind: "import".into(),
        document_id: None,
        revision: None,
        status: "queued".into(),
        attempt: 1,
        progress: Some(r#"{"stage":"s","done":0,"total":1,"unit":"p"}"#.into()),
        error: None,
        created_at: "2026-10-09T00:00:00.000Z".into(),
        updated_at: "2026-10-09T00:00:00.000Z".into(),
        result_ref: None,
    }
}

#[test]
fn each_row_guard_rejects_on_its_own() {
    assert!(row().checked().is_ok());
    let progress = |p: &str| Row {
        progress: Some(p.into()),
        ..row()
    };
    let succeeded = |kind: &str| Row {
        status: "succeeded".into(),
        kind: kind.into(),
        ..row()
    };
    let bad = [
        (
            "progress without total",
            progress(r#"{"stage":"s","done":0,"unit":"p"}"#),
        ),
        (
            "progress with a text total",
            progress(r#"{"stage":"s","done":0,"total":"1","unit":"p"}"#),
        ),
        (
            "progress without unit",
            progress(r#"{"stage":"s","done":0,"total":null}"#),
        ),
        (
            "bad created_at",
            Row {
                created_at: "2026-02-30T00:00:00.000Z".into(),
                ..row()
            },
        ),
        (
            "bad updated_at",
            Row {
                updated_at: "0300-02-29T00:00:00.000Z".into(),
                ..row()
            },
        ),
        (
            "succeeded import with r2",
            Row {
                document_id: Some(DOC.into()),
                revision: Some(2),
                ..succeeded("import")
            },
        ),
        (
            "succeeded import without a revision",
            Row {
                document_id: Some(DOC.into()),
                ..succeeded("import")
            },
        ),
        (
            "succeeded extraction without a result",
            succeeded("extract"),
        ),
        (
            "succeeded extraction with a bad id",
            Row {
                result_ref: Some("ext_x".into()),
                ..succeeded("extract")
            },
        ),
    ];
    for (name, r) in bad {
        assert!(r.checked().is_err(), "{name}");
    }
    let ext = "ext_01K75A0B1C2D3E4F5G6H7J8K9M";
    let job = Row {
        result_ref: Some(ext.into()),
        ..succeeded("extract")
    }
    .checked()
    .unwrap();
    assert_eq!(job.result, Some(json!({ "extraction_id": ext })));
    // A kind the OS has no result shape for has none.
    assert_eq!(succeeded("export").checked().unwrap().result, None);
}
