//! `extraction_get` (DOC-1-06b §3.3 slice D2): the same pages as REST,
//! paged at the tool budget; a value over it is refused, not cut.

use super::*;
use crate::extractions::{ExtractedValue, MissingReason, NewExtraction, Status, insert};

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00"; // shadows the parent's
const LAYER: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const SCHEMA: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const SMALL: &str = "ext_01K75A0B1C2D3E4F5G6H7J8K9M";
const BIG_FIRST: &str = "ext_01K75A0B1C2D3E4F5G6H7J8K9P";
const BIG_SECOND: &str = "ext_01K75A0B1C2D3E4F5G6H7J8K9Q";

/// A present value without evidence, its value `len` bytes long.
fn value(key: &str, len: usize) -> ExtractedValue {
    ExtractedValue {
        key: key.into(),
        status: Status::Present,
        value: Some("x".repeat(len)),
        normalized: None,
        anchors: Some(vec![]),
        unsourced_reason: Some("r".into()),
        missing_reason: None,
        candidates: None,
    }
}

fn missing(key: &str) -> ExtractedValue {
    ExtractedValue {
        status: Status::Missing,
        value: None,
        anchors: None,
        unsourced_reason: None,
        missing_reason: Some(MissingReason::NotInDocument),
        ..value(key, 0)
    }
}

/// A document, its r1 and a text layer, with three extractions: five small
/// values; a first value over the tool budget; a small one, then that.
async fn env() -> (tempfile::TempDir, AppState) {
    let (dir, state) = opened().await;
    let storage = state.storage().await.unwrap();
    let blob = storage.blobs.put_bytes(b"%PDF-1.7 bytes").unwrap();
    let sha = &blob.sha256;
    exec(
        &state,
        &format!(
            "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{DOC}', 't', 'application/pdf', 1);
             INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
             VALUES ('{DOC}', 1, '{sha}', {}, 'application/pdf', 'import');
             INSERT INTO text_layers (text_layer_sha256, content_sha256, engine_id, engine_version, config_sha256)
             VALUES ('{LAYER}', '{sha}', 'apple-pdfkit', '26.6.2', '{SCHEMA}');",
            blob.size
        ),
    )
    .await;
    // Over 60 KiB, well within what a REST page (and `insert`) allows.
    let big = || value("big", 70 * 1024);
    let mut tx = storage.db.pool().begin().await.unwrap();
    for (id, values) in [
        (SMALL, (0..5).map(|n| missing(&format!("f{n}"))).collect()),
        (BIG_FIRST, vec![big()]),
        (BIG_SECOND, vec![value("small", 10), big()]),
    ] {
        let e = NewExtraction {
            id: id.into(),
            document_id: DOC.into(),
            revision: 1,
            schema_sha256: SCHEMA.into(),
            extractor_version: "1".into(),
            model_id: "qwen3-8b".into(),
            text_layer_sha256: LAYER.into(),
            values,
        };
        insert(&mut tx, &e).await.unwrap();
    }
    tx.commit().await.unwrap();
    (dir, state)
}

#[tokio::test]
async fn extraction_get_pages_as_rest_pages() {
    let (_dir, state) = env().await;
    let (status, want) = rest(&state, &format!("/extractions/{SMALL}")).await;
    assert_eq!(status, StatusCode::OK);
    let got = payload(&call(&state, "extraction_get", json!({ "extraction_id": SMALL })).await);
    assert_eq!(got, want);
    let mut args = json!({ "extraction_id": SMALL, "limit": 2 });
    let mut query = "?limit=2".to_owned();
    let mut pages = 0;
    loop {
        let (status, want) = rest(&state, &format!("/extractions/{SMALL}{query}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            payload(&call(&state, "extraction_get", args.clone()).await),
            want
        );
        pages += 1;
        let Some(c) = want["next_cursor"].as_str() else {
            break;
        };
        args["cursor"] = json!(c);
        query = format!("?limit=2&cursor={c}");
    }
    assert_eq!(pages, 3);
}

#[tokio::test]
async fn a_value_over_the_tool_budget_is_payload_too_large_and_rest_reads_it() {
    let (_dir, state) = env().await;
    let body = call(
        &state,
        "extraction_get",
        json!({ "extraction_id": BIG_FIRST }),
    )
    .await;
    assert_module_error(&body, "payload_too_large", false);
    let (status, page) = rest(&state, &format!("/extractions/{BIG_FIRST}")).await;
    assert_eq!(status, StatusCode::OK, "{}", page["error"]);
    assert_eq!(page["values"][0]["key"], "big");

    // A small first value goes alone; the large one is then refused.
    let args = json!({ "extraction_id": BIG_SECOND });
    let first = payload(&call(&state, "extraction_get", args.clone()).await);
    assert_eq!(first["values"].as_array().unwrap().len(), 1);
    assert_eq!(first["values"][0]["key"], "small");
    assert_eq!(first["next_cursor"], "v1");
    let mut next = args;
    next["cursor"] = first["next_cursor"].clone();
    let body = call(&state, "extraction_get", next).await;
    assert_module_error(&body, "payload_too_large", false);
}

#[tokio::test]
async fn extraction_get_refuses_as_the_other_tools_do() {
    let (_dir, state) = env().await;
    let missing_ext = "ext_01K75A0B1C2D3E4F5G6H7J8K9R";
    let (status, rest_err) = rest(&state, &format!("/extractions/{missing_ext}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(rest_err["error"]["code"], "not_found");
    for (args, code) in [
        (json!({ "extraction_id": missing_ext }), "not_found"),
        (json!({ "extraction_id": "ext_x" }), "invalid_request"),
        (
            json!({ "extraction_id": SMALL, "cursor": "nope" }),
            "invalid_request",
        ),
        (
            json!({ "extraction_id": SMALL, "cursor": "v100" }),
            "invalid_request",
        ),
    ] {
        let body = call(&state, "extraction_get", args.clone()).await;
        assert_module_error(&body, code, false);
    }
    for args in [
        json!({}),
        json!({ "extraction_id": null }),
        json!({ "extraction_id": 1 }),
        json!({ "extraction_id": SMALL, "limit": 0 }),
        json!({ "extraction_id": SMALL, "limit": 201 }),
        json!({ "extraction_id": SMALL, "limit": null }),
        json!({ "extraction_id": SMALL, "limit": "5" }),
        json!({ "extraction_id": SMALL, "cursor": null }),
        json!({ "extraction_id": SMALL, "extra": true }),
    ] {
        let body = call(&state, "extraction_get", args.clone()).await;
        assert_eq!(
            parse_module_tool_envelope(&body),
            Err(ModuleToolCallError::Module {
                code: ModuleToolErrorCode::InvalidInput,
                retryable: false,
                details: None,
                unknown_code: None,
            }),
            "{args}"
        );
    }
    for limit in [1, 200] {
        let args = json!({ "extraction_id": SMALL, "limit": limit });
        payload(&call(&state, "extraction_get", args).await);
    }
}
