#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use crate::state::AppState;

const SHA: &str = "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M01";
const OTHER: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M02";

async fn opened() -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    for (id, title) in [(DOC, "合同.pdf"), (OTHER, "发票.pdf")] {
        exec(
            &state,
            &format!(
                "BEGIN;
                 INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{id}', '{title}', 'application/pdf', 1);
                 INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                        VALUES ('{id}', 1, '{SHA}', 5, 'application/pdf', 'import');
                 COMMIT;"
            ),
        )
        .await;
    }
    (dir, state)
}

async fn exec(state: &AppState, sql: &str) {
    let storage = state.storage().await.unwrap();
    sqlx::raw_sql(sql).execute(storage.db.pool()).await.unwrap();
}

async fn scalar(state: &AppState, sql: &str) -> String {
    let storage = state.storage().await.unwrap();
    sqlx::query_scalar(sql)
        .fetch_one(storage.db.pool())
        .await
        .unwrap()
}

async fn send(state: &AppState, method: Method, path: &str) -> (StatusCode, Value) {
    let res = crate::router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn ids(list: &Value) -> Vec<&str> {
    list["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["document_id"].as_str().unwrap())
        .collect()
}

/// Deleted from the page: gone from the list and every route of it, listed
/// as recently deleted, restored with its revision as it was.
#[tokio::test]
async fn a_deleted_document_is_hidden_until_it_is_restored() {
    let (_dir, state) = opened().await;
    let (status, gone) = send(&state, Method::POST, &format!("/documents/{DOC}/delete")).await;
    assert_eq!(status, StatusCode::OK, "{gone}");
    assert_eq!(gone["title"], "合同.pdf");
    let deleted_at = gone["deleted_at"].as_str().unwrap();
    // Restorable for exactly 7 days.
    let until = scalar(
        &state,
        &format!("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', '{deleted_at}', '+7 days')"),
    )
    .await;
    assert_eq!(gone["restorable_until"], until.as_str());

    let (_, list) = send(&state, Method::GET, "/documents").await;
    assert_eq!(ids(&list), [OTHER]);
    for path in [
        format!("/documents/{DOC}"),
        format!("/documents/{DOC}/paragraphs"),
        format!("/documents/{DOC}/revisions/1/text"),
        format!("/documents/{DOC}/content"),
    ] {
        let (status, body) = send(&state, Method::GET, &path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
    }
    // Deleted twice: not found, nothing written.
    let (status, _) = send(&state, Method::POST, &format!("/documents/{DOC}/delete")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (_, deleted) = send(&state, Method::GET, "/recently-deleted").await;
    assert_eq!(ids(&deleted), [DOC]);

    let (status, back) = send(&state, Method::POST, &format!("/documents/{DOC}/restore")).await;
    assert_eq!(status, StatusCode::OK, "{back}");
    assert_eq!(back["head"]["revision"], 1);
    assert_eq!(back["head"]["content_sha256"], SHA);
    let (_, list) = send(&state, Method::GET, "/documents").await;
    assert_eq!(ids(&list).len(), 2);
    let (_, deleted) = send(&state, Method::GET, "/recently-deleted").await;
    assert!(ids(&deleted).is_empty());
    // Not deleted: nothing to restore.
    let (status, _) = send(&state, Method::POST, &format!("/documents/{OTHER}/restore")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let ops = scalar(
        &state,
        "SELECT group_concat(op || ':' || origin, ',') FROM oplog ORDER BY seq",
    )
    .await;
    assert_eq!(ops, "delete:page,restore:page");
}

/// Past 7 days a deleted document is no longer listed and cannot be restored.
#[tokio::test]
async fn past_seven_days_a_deleted_document_is_gone_for_the_user() {
    let (_dir, state) = opened().await;
    exec(
        &state,
        &format!(
            "UPDATE documents SET deleted_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-7 days', '-1 minutes') WHERE id = '{DOC}';
             UPDATE documents SET deleted_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-6 days') WHERE id = '{OTHER}';"
        ),
    )
    .await;
    let (_, deleted) = send(&state, Method::GET, "/recently-deleted").await;
    assert_eq!(ids(&deleted), [OTHER]);
    let (status, _) = send(&state, Method::POST, &format!("/documents/{DOC}/restore")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&state, Method::POST, &format!("/documents/{OTHER}/restore")).await;
    assert_eq!(status, StatusCode::OK);
    // The rows and revisions are all still there.
    let revisions = scalar(&state, "SELECT CAST(COUNT(*) AS TEXT) FROM revisions").await;
    assert_eq!(revisions, "2");
}
