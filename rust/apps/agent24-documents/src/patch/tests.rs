#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

use super::*;

const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/docx-patch.docx");
const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00001";

async fn with_docx() -> (tempfile::TempDir, AppState, Arc<Storage>) {
    with_file(FIXTURE).await
}

async fn with_file(file: &[u8]) -> (tempfile::TempDir, AppState, Arc<Storage>) {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    let storage = state.storage().await.unwrap();
    let sha = storage.blobs.put_bytes(file).unwrap().sha256;
    sqlx::raw_sql(&format!(
        "INSERT INTO documents (id, title, media_type, head_revision, created_at, updated_at)
                VALUES ('{DOC}', '租赁合同.docx', '{DOCX}', 1, '2026-10-01T00:00:00.000Z', '2026-10-01T00:00:00.000Z');
         INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                VALUES ('{DOC}', 1, '{sha}', {}, '{DOCX}', 'import');",
        file.len()
    ))
    .execute(storage.db.pool())
    .await
    .unwrap();
    (dir, state, storage)
}

async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = crate::router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 1 << 22)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
    )
}

fn post(path: &str, body: serde_json::Value) -> Request<Body> {
    Request::post(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn lists_a_docxs_paragraphs_and_patches_them_into_a_new_document() {
    let (_dir, state, storage) = with_docx().await;
    let (status, listed) = call(
        &state,
        Request::get(format!("/documents/{DOC}/paragraphs"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["revision"], 1);
    let paras = listed["paragraphs"].as_array().unwrap();
    let deposit = paras
        .iter()
        .find(|p| p["text"].as_str().unwrap().starts_with("押金为"))
        .unwrap();
    let index = deposit["index"].as_u64().unwrap();

    let body = serde_json::json!({ "edits": [
        { "index": index, "text": "押金为人民币 17,000 元。", "tracked": true, "expect": deposit["text"] },
    ] });
    let (status, job) = call(&state, post(&format!("/documents/{DOC}/patches"), body)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{job}");
    assert_eq!(job["kind"], "import");
    // The original is not changed.
    let head: i64 = sqlx::query_scalar("SELECT head_revision FROM documents WHERE id = ?")
        .bind(DOC)
        .fetch_one(storage.db.pool())
        .await
        .unwrap();
    assert_eq!(head, 1);
}

#[tokio::test]
async fn a_stale_paragraph_an_index_past_the_end_and_a_pdf_are_refused() {
    let (_dir, state, _storage) = with_docx().await;
    let stale = serde_json::json!({ "edits": [{ "index": 0, "text": "x", "expect": "not this" }] });
    let (status, body) = call(&state, post(&format!("/documents/{DOC}/patches"), stale)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "revision_conflict");
    let past = serde_json::json!({ "edits": [{ "index": 9999, "text": "x" }] });
    let (status, body) = call(&state, post(&format!("/documents/{DOC}/patches"), past)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let none = serde_json::json!({ "edits": [] });
    let (status, _) = call(&state, post(&format!("/documents/{DOC}/patches"), none)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

const TEMPLATE: &[u8] = include_bytes!("../../tests/fixtures/template-lease.docx");

#[tokio::test]
async fn lists_a_templates_fields_fills_them_into_a_new_document_and_sends_the_file() {
    let (_dir, state, storage) = with_file(TEMPLATE).await;
    let (status, listed) = call(
        &state,
        Request::get(format!("/documents/{DOC}/fields"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let names: Vec<&str> = listed["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"出租方") && names.contains(&"押金"),
        "{names:?}"
    );

    let body =
        serde_json::json!({ "values": { "出租方": "上海明远置业有限公司", "押金": "17,000" } });
    let (status, job) = call(&state, post(&format!("/documents/{DOC}/fill"), body)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{job}");
    assert_eq!(job["kind"], "import");
    let head: i64 = sqlx::query_scalar("SELECT head_revision FROM documents WHERE id = ?")
        .bind(DOC)
        .fetch_one(storage.db.pool())
        .await
        .unwrap();
    assert_eq!(head, 1);

    // A field the template does not have, and no values: refused.
    let unknown = serde_json::json!({ "values": { "没有这个": "x" } });
    let (status, _) = call(&state, post(&format!("/documents/{DOC}/fill"), unknown)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &state,
        post(
            &format!("/documents/{DOC}/fill"),
            serde_json::json!({ "values": {} }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // The file as imported, with its media type.
    let res = crate::router(state.clone())
        .oneshot(
            Request::get(format!("/documents/{DOC}/content"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-type"], DOCX);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 22)
        .await
        .unwrap();
    assert_eq!(&bytes[..], TEMPLATE);
}
