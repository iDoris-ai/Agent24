#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::Request;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::office::DOCX;
use crate::oplog::Origin;
use crate::state::AppState;
use crate::versions::{Commit, commit};

const FIXTURE: &[u8] = include_bytes!("../../../tests/fixtures/docx-patch.docx");
const OTHER: &[u8] = include_bytes!("../../../tests/fixtures/template-lease.docx");
const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00001";
const PDF: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00002";

async fn setup() -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    let storage = state.storage().await.unwrap();
    let sha = storage.blobs.put_bytes(FIXTURE).unwrap().sha256;
    sqlx::raw_sql(&format!(
        "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{DOC}', 'a.docx', '{DOCX}', 1);
         INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                VALUES ('{DOC}', 1, '{sha}', {n}, '{DOCX}', 'import');
         INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{PDF}', 'b.pdf', 'application/pdf', 1);
         INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                VALUES ('{PDF}', 1, '{sha}', {n}, 'application/pdf', 'import');",
        n = FIXTURE.len()
    ))
    .execute(storage.db.pool())
    .await
    .unwrap();
    let c = Commit {
        expect: 1,
        bytes: OTHER.to_vec(),
        kind: DOCX,
        origin: &Origin::Page,
        op: "patch",
        details: json!({}),
    };
    commit(&storage, DOC, c).await.unwrap();
    (dir, state)
}

async fn compare(state: &AppState, body: Value) -> (u16, Value) {
    let req = Request::post("/documents/compare")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = crate::router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 22)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn side(id: &str, revision: Option<i64>) -> Value {
    match revision {
        Some(r) => json!({"document_id": id, "revision": r}),
        None => json!({"document_id": id}),
    }
}

#[tokio::test]
async fn a_version_against_itself_is_identical() {
    let (_d, state) = setup().await;
    let (s, v) = compare(
        &state,
        json!({"left": side(DOC, Some(1)), "right": side(DOC, Some(1))}),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (v["identical"].as_bool(), v["only_text"].as_bool()),
        (Some(true), Some(true))
    );
    assert_eq!(v["summary"]["total"], 0);
}

#[tokio::test]
async fn two_versions_of_one_document_differ_and_head_is_the_default() {
    let (_d, state) = setup().await;
    let (_, explicit) = compare(
        &state,
        json!({"left": side(DOC, Some(1)), "right": side(DOC, Some(2))}),
    )
    .await;
    let (s, v) = compare(
        &state,
        json!({"left": side(DOC, Some(1)), "right": side(DOC, None)}),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["right"]["revision"], 2);
    assert_eq!(v["identical"], false);
    assert!(v["summary"]["total"].as_u64().unwrap() > 0);
    assert_eq!(v["changes"], explicit["changes"]);
    assert!(v["changes"][0]["left"].is_object() || v["changes"][0]["right"].is_object());
}

#[tokio::test]
async fn the_changes_come_in_pages_and_the_summary_stays_whole() {
    let (_d, state) = setup().await;
    let both = json!({"left": side(DOC, Some(1)), "right": side(DOC, Some(2))});
    let (_, all) = compare(&state, both.clone()).await;
    let total = all["summary"]["total"].as_u64().unwrap();
    assert!(total > 1);
    let mut req = both;
    req["limit"] = json!(1);
    let (s, first) = compare(&state, req.clone()).await;
    assert_eq!(s, 200);
    assert_eq!(first["changes"].as_array().unwrap().len(), 1);
    assert_eq!(
        (first["truncated"].as_bool(), first["next_offset"].as_u64()),
        (Some(true), Some(1))
    );
    assert_eq!(first["summary"]["total"].as_u64(), Some(total));
    req["offset"] = json!(1);
    let (_, second) = compare(&state, req).await;
    assert_eq!(second["changes"][0], all["changes"][1]);
}

#[tokio::test]
async fn a_word_document_and_a_pdf_are_refused_plainly() {
    let (_d, state) = setup().await;
    let (s, v) = compare(
        &state,
        json!({"left": side(DOC, None), "right": side(PDF, None)}),
    )
    .await;
    assert_eq!(s, 422);
    assert_eq!(v["error"]["code"], "unsupported_format");
}

#[tokio::test]
async fn bad_requests_and_missing_documents() {
    let (_d, state) = setup().await;
    let ok = side(DOC, None);
    let (s, _) = compare(&state, json!({"left": ok, "right": side("doc_nope", None)})).await;
    assert_eq!(s, 400);
    let (s, _) = compare(
        &state,
        json!({"left": ok, "right": side("doc_01K74Z3QJ8V5N2W9RTX6Y00009", None)}),
    )
    .await;
    assert_eq!(s, 404);
    let (s, _) = compare(&state, json!({"left": ok, "right": side(DOC, Some(9))})).await;
    assert_eq!(s, 404);
    let (s, _) = compare(&state, json!({"left": ok, "right": ok, "limit": 0})).await;
    assert_eq!(s, 400);
    let (s, _) = compare(&state, json!({"left": ok, "right": ok, "extra": 1})).await;
    assert_eq!(s, 400);
}

#[tokio::test]
async fn comparing_writes_nothing() {
    let (_d, state) = setup().await;
    let storage = state.storage().await.unwrap();
    let count = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT (SELECT count(*) FROM oplog) + (SELECT count(*) FROM revisions)",
        )
        .fetch_one(storage.db.pool())
        .await
        .unwrap()
    };
    let before = count().await;
    compare(
        &state,
        json!({"left": side(DOC, Some(1)), "right": side(DOC, Some(2))}),
    )
    .await;
    assert_eq!(count().await, before);
}
