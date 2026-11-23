//! `documents.compare` (L6-S05): read-only, two documents or two versions of
//! one, an answer small enough for a model, made of the module's facts only.

use super::*;
use crate::office::DOCX;
use crate::oplog::Origin;
use crate::versions::{Commit, commit};

const FIXTURE: &[u8] = include_bytes!("../../../tests/fixtures/docx-patch.docx");
const OTHER: &[u8] = include_bytes!("../../../tests/fixtures/template-lease.docx");
const W1: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00011";
const W2: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00012";

async fn word(state: &AppState, id: &str, title: &str, bytes: &[u8]) {
    let storage = state.storage().await.unwrap();
    let sha = storage.blobs.put_bytes(bytes).unwrap().sha256;
    exec(
        state,
        &format!(
            "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{id}', '{title}', '{DOCX}', 1);
             INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                    VALUES ('{id}', 1, '{sha}', {n}, '{DOCX}', 'import');",
            n = bytes.len()
        ),
    )
    .await;
}

async fn setup() -> (tempfile::TempDir, AppState) {
    let (dir, state) = opened().await;
    word(&state, W1, "租赁合同.docx", FIXTURE).await;
    word(&state, W2, "补充协议.docx", OTHER).await;
    (dir, state)
}

#[tokio::test]
async fn two_documents_are_compared_by_name_and_the_answer_is_small() {
    let (_d, state) = setup().await;
    let body = call(
        &state,
        "compare",
        json!({"document_id": "租赁合同", "other_document_id": "补充协议"}),
    )
    .await;
    let p = payload(&body);
    assert_eq!(p["only_text"], true);
    let total = p["summary"]["total"].as_u64().unwrap();
    assert!(total > 0);
    let shown = p["changes"].as_array().unwrap();
    assert!(shown.len() <= 10 && shown.len() as u64 == p["shown"].as_u64().unwrap());
    assert_eq!(p["more"], total > shown.len() as u64);
    assert!(
        shown
            .iter()
            .all(|c| c["id"].as_str().is_some_and(|i| i.starts_with('c')))
    );
    for c in shown {
        for side in ["left", "right"] {
            if let Some(t) = c[side]["text"].as_str() {
                assert!(t.chars().count() <= 121, "{t}");
            }
        }
    }
    assert!(p["note"].as_str().unwrap().contains("changes"));
}

#[tokio::test]
async fn one_document_defaults_to_the_last_two_versions() {
    let (_d, state) = setup().await;
    let storage = state.storage().await.unwrap();
    let c = Commit {
        expect: 1,
        bytes: OTHER.to_vec(),
        kind: DOCX,
        origin: &Origin::Page,
        op: "patch",
        details: json!({}),
    };
    commit(&storage, W1, c).await.unwrap();
    let p = payload(&call(&state, "compare", json!({"document_id": W1})).await);
    assert_eq!(
        (p["from_revision"].as_i64(), p["to_revision"].as_i64()),
        (Some(1), Some(2))
    );
    assert!(p["summary"]["total"].as_u64().unwrap() > 0);
    let same = payload(
        &call(
            &state,
            "compare",
            json!({"document_id": W1, "from_revision": 2, "to_revision": 2}),
        )
        .await,
    );
    assert_eq!(same["identical"], true);
}

#[tokio::test]
async fn a_document_with_one_version_says_so() {
    let (_d, state) = setup().await;
    let p = payload(&call(&state, "compare", json!({"document_id": W1})).await);
    assert_eq!(p["reason"], "single_version");
}

#[tokio::test]
async fn the_second_document_must_be_in_the_scope_too() {
    let (_d, state) = setup().await;
    let args = json!({"document_id": W1, "other_document_id": W2});
    let body = call_in(&state, "compare", &[W1], args.clone()).await;
    assert_module_error(&body, "not_found", false);
    let body = call_in(&state, "compare", &[W2], args.clone()).await;
    assert_module_error(&body, "not_found", false);
    assert!(payload(&call_in(&state, "compare", &[W1, W2], args).await)["summary"].is_object());
}

#[tokio::test]
async fn bad_arguments_are_refused() {
    let (_d, state) = setup().await;
    let body = call(&state, "compare", json!({"other_document_id": W2})).await;
    assert!(String::from_utf8_lossy(&body).contains("invalid_input"));
    let body = call(
        &state,
        "compare",
        json!({"document_id": W1, "other_document_id": W2, "extra": 1}),
    )
    .await;
    assert!(String::from_utf8_lossy(&body).contains("invalid_input"));
}

#[tokio::test]
async fn a_revision_below_one_is_refused_not_a_single_version() {
    let (_d, state) = setup().await;
    for bad in [
        json!({"document_id": W1, "to_revision": 0}),
        json!({"document_id": W1, "from_revision": -3}),
    ] {
        let body = call(&state, "compare", bad).await;
        assert!(
            String::from_utf8_lossy(&body).contains("invalid_request"),
            "{}",
            String::from_utf8_lossy(&body)
        );
    }
}

#[tokio::test]
async fn a_second_document_in_the_trash_is_said_to_be_deleted() {
    let (_d, state) = setup().await;
    exec(
        &state,
        &format!("UPDATE documents SET deleted_at = '2026-11-23T00:00:00.000Z' WHERE id = '{W2}'"),
    )
    .await;
    let p = payload(
        &call(
            &state,
            "compare",
            json!({"document_id": W1, "other_document_id": W2}),
        )
        .await,
    );
    assert_eq!(p["reason"], "deleted");
}
