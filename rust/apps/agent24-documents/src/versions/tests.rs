#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::Request;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{Commit, Committed};
use crate::error::ApiError;
use crate::office::DOCX;
use crate::oplog::Origin;
use crate::state::AppState;

const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/docx-patch.docx");
const OTHER: &[u8] = include_bytes!("../../tests/fixtures/template-lease.docx");
const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00001";

async fn with_file(file: &[u8]) -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    let storage = state.storage().await.unwrap();
    let sha = storage.blobs.put_bytes(file).unwrap().sha256;
    sqlx::raw_sql(&format!(
        "INSERT INTO documents (id, title, media_type, head_revision)
                VALUES ('{DOC}', '租赁合同.docx', '{DOCX}', 1);
         INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                VALUES ('{DOC}', 1, '{sha}', {}, '{DOCX}', 'import');",
        file.len()
    ))
    .execute(storage.db.pool())
    .await
    .unwrap();
    (dir, state)
}

async fn send(state: &AppState, req: Request<Body>) -> (u16, Vec<u8>) {
    let res = crate::router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let body = axum::body::to_bytes(res.into_body(), 1 << 22)
        .await
        .unwrap();
    (status, body.to_vec())
}

async fn call(state: &AppState, req: Request<Body>) -> (u16, Value) {
    let (status, body) = send(state, req).await;
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn post(path: &str, body: Value) -> Request<Body> {
    Request::post(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).unwrap()
}

async fn scalar(state: &AppState, sql: &str) -> i64 {
    let storage = state.storage().await.unwrap();
    sqlx::query_scalar(sql)
        .fetch_one(storage.db.pool())
        .await
        .unwrap()
}

/// Commits `bytes` over `expect`, as a patch saved to the document would.
async fn commit(state: &AppState, expect: i64, bytes: &[u8]) -> Result<Committed, ApiError> {
    let storage = state.storage().await.unwrap();
    let c = Commit {
        expect,
        bytes: bytes.to_vec(),
        kind: DOCX,
        origin: &Origin::Page,
        op: "patch",
        details: json!({}),
    };
    super::commit(&storage, DOC, c).await
}

#[tokio::test]
async fn a_commit_is_the_next_version_and_the_old_one_stays() {
    let (_dir, state) = with_file(FIXTURE).await;
    let saved = commit(&state, 1, OTHER).await.unwrap();
    assert_eq!(
        (saved.revision, saved.previous_revision, saved.committed),
        (2, 1, true)
    );
    assert_eq!(saved.title, "租赁合同.docx");
    assert_eq!(scalar(&state, "SELECT count(*) FROM documents").await, 1);
    assert_eq!(
        scalar(&state, "SELECT head_revision FROM documents").await,
        2
    );
    let (status, now) = send(&state, get(&format!("/documents/{DOC}/content"))).await;
    assert_eq!((status, now.as_slice()), (200, OTHER));
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM oplog WHERE op = 'patch' AND revision = 2 AND origin = 'page'"
        )
        .await,
        1
    );
    let (status, list) = call(&state, get(&format!("/documents/{DOC}/revisions"))).await;
    assert_eq!(status, 200, "{list}");
    assert_eq!(list["head_revision"], 2);
    let revs: Vec<(i64, String)> = list["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["revision"].as_i64().unwrap(),
                r["origin"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(revs, [(2, "commit".to_owned()), (1, "import".to_owned())]);
}

#[tokio::test]
async fn a_version_not_the_head_is_never_written_over() {
    let (_dir, state) = with_file(FIXTURE).await;
    commit(&state, 1, OTHER).await.unwrap();
    // Someone read r1 and saves over it after r2: refused, nothing written.
    let e = commit(&state, 1, b"PK not this").await.unwrap_err();
    assert_eq!((e.status().as_u16(), e.code()), (409, "revision_conflict"));
    assert_eq!(scalar(&state, "SELECT count(*) FROM revisions").await, 2);
    // Another format is not a version of a DOCX.
    let storage = state.storage().await.unwrap();
    let pdf = Commit {
        expect: 2,
        bytes: b"%PDF-1.7".to_vec(),
        kind: "application/pdf",
        origin: &Origin::Page,
        op: "patch",
        details: json!({}),
    };
    assert!(super::commit(&storage, DOC, pdf).await.is_err());
    assert_eq!(scalar(&state, "SELECT count(*) FROM revisions").await, 2);
}

#[tokio::test]
async fn restoring_an_old_version_adds_one_with_its_content_and_keeps_them_all() {
    let (_dir, state) = with_file(FIXTURE).await;
    commit(&state, 1, OTHER).await.unwrap();
    let restore = format!("/documents/{DOC}/revisions/1/restore");
    // Read at r1, restored after r2: refused.
    let (status, body) = call(&state, post(&restore, json!({ "expect_revision": 1 }))).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "revision_conflict");
    assert_eq!(body["error"]["details"]["current_revision"], 2);
    let (status, saved) = call(&state, post(&restore, json!({ "expect_revision": 2 }))).await;
    assert_eq!(status, 201, "{saved}");
    assert_eq!(
        (&saved["revision"], &saved["committed"]),
        (&json!(3), &json!(true))
    );
    let (_, now) = send(&state, get(&format!("/documents/{DOC}/content"))).await;
    assert_eq!(now.as_slice(), FIXTURE);
    assert_eq!(scalar(&state, "SELECT count(*) FROM revisions").await, 3);
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM oplog WHERE op = 'restore_revision' AND revision = 3"
        )
        .await,
        1
    );
    // Restoring what is already current writes nothing.
    let (status, same) = call(&state, post(&restore, json!({ "expect_revision": 3 }))).await;
    assert_eq!((status, &same["committed"]), (200, &json!(false)));
    assert_eq!(scalar(&state, "SELECT count(*) FROM revisions").await, 3);
    // A version that does not exist.
    let none = format!("/documents/{DOC}/revisions/9/restore");
    let (status, _) = call(&state, post(&none, json!({ "expect_revision": 3 }))).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn a_deleted_document_has_no_versions_to_list_restore_or_commit() {
    let (_dir, state) = with_file(FIXTURE).await;
    let (status, _) = call(&state, post(&format!("/documents/{DOC}/delete"), json!({}))).await;
    assert_eq!(status, 200);
    let (status, _) = call(&state, get(&format!("/documents/{DOC}/revisions"))).await;
    assert_eq!(status, 404);
    let restore = format!("/documents/{DOC}/revisions/1/restore");
    let (status, _) = call(&state, post(&restore, json!({ "expect_revision": 1 }))).await;
    assert_eq!(status, 404);
    assert_eq!(
        commit(&state, 1, OTHER).await.unwrap_err().code(),
        "not_found"
    );
    assert_eq!(scalar(&state, "SELECT count(*) FROM revisions").await, 1);
}
