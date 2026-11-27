//! `documents.preflight` (L8 DOC-4): read-only, by title, nothing changed.

use super::*;
use crate::office::DOCX;

const LEASE: &[u8] = include_bytes!("../../../tests/fixtures/template-lease.docx");
const W1: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00021";
const W2: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00022";

async fn put(state: &AppState, id: &str, title: &str, media: &str, bytes: &[u8]) {
    let storage = state.storage().await.unwrap();
    let sha = storage.blobs.put_bytes(bytes).unwrap().sha256;
    exec(
        state,
        &format!(
            "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{id}', '{title}', '{media}', 1);
             INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                    VALUES ('{id}', 1, '{sha}', {n}, '{media}', 'import');",
            n = bytes.len()
        ),
    )
    .await;
}

async fn revisions(state: &AppState) -> i64 {
    let storage = state.storage().await.unwrap();
    sqlx::query_scalar("SELECT COUNT(*) FROM revisions")
        .fetch_one(storage.db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_template_named_by_title_reports_its_fields_and_author_and_changes_nothing() {
    let (_d, state) = opened().await;
    put(&state, W1, "租赁合同模板.docx", DOCX, LEASE).await;
    let p = payload(&call(&state, "preflight", json!({"document_id": "租赁合同模板"})).await);
    assert_eq!(p["document_id"], W1);
    assert_eq!(p["format"], "docx");
    assert_eq!(p["clean"], false);
    assert_eq!(p["cleanable"], true);
    let kinds: Vec<&str> = p["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"unfilled_fields") && kinds.contains(&"author"),
        "{kinds:?}"
    );
    assert!(p["note"].as_str().unwrap().contains("只读"));
    assert_eq!(revisions(&state).await, 1);
}

#[tokio::test]
async fn an_image_is_refused_and_extra_arguments_are_invalid() {
    let (_d, state) = opened().await;
    put(&state, W2, "照片.png", "image/png", b"\x89PNG").await;
    let body = call(&state, "preflight", json!({"document_id": W2})).await;
    assert_module_error(&body, "unsupported_format", false);
    let body = call(&state, "preflight", json!({"document_id": W2, "fix": true})).await;
    assert!(String::from_utf8_lossy(&body).contains("invalid_input"));
}
