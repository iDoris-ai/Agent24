#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::state::{AppState, Storage};

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00";
const EXT: &str = "ext_01K75A0B1C2D3E4F5G6H7J8K9M";
const LAYER: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const SCHEMA: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

struct Store {
    _dir: tempfile::TempDir,
    state: AppState,
    storage: std::sync::Arc<Storage>,
    content: String,
}

/// A store with a document, its r1 and a text layer of it.
async fn store() -> Store {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    let storage = state.storage().await.unwrap();
    let blob = storage.blobs.put_bytes(b"%PDF-1.7 bytes").unwrap();
    for sql in [
        format!("INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{DOC}', 't', 'application/pdf', 1)"),
        format!(
            "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
             VALUES ('{DOC}', 1, '{}', {}, 'application/pdf', 'import')",
            blob.sha256, blob.size
        ),
        format!(
            "INSERT INTO text_layers (text_layer_sha256, content_sha256, engine_id, engine_version, config_sha256)
             VALUES ('{LAYER}', '{}', 'apple-pdfkit', '26.6.2', '{SCHEMA}')",
            blob.sha256
        ),
    ] {
        sqlx::query(&sql).execute(storage.db.pool()).await.unwrap();
    }
    Store {
        _dir: dir,
        state,
        storage,
        content: blob.sha256,
    }
}

fn anchor(content: &str, quote: &str) -> Anchor {
    Anchor {
        document_id: DOC.into(),
        revision: 1,
        content_sha256: content.into(),
        media_type: "application/pdf".into(),
        text_layer_sha256: LAYER.into(),
        engine: Engine {
            id: "apple-pdfkit".into(),
            version: "26.6.2".into(),
        },
        block_id: "p1/b3".into(),
        page: Some(1),
        block_text_sha256: SCHEMA.into(),
        text_range: TextRange {
            unit: "utf8".into(),
            start: 0,
            end: quote.len() as u64,
        },
        geometry: Some(Geometry {
            frame: "CropBox".into(),
            unit: "pt".into(),
            origin: "top-left-rotated".into(),
            rects: vec![[72.0, 100.0, 300.0, 112.0]],
        }),
        quote: quote.into(),
    }
}

fn missing(key: &str) -> ExtractedValue {
    ExtractedValue {
        key: key.into(),
        status: Status::Missing,
        value: None,
        normalized: None,
        anchors: None,
        unsourced_reason: None,
        missing_reason: Some(MissingReason::NotInDocument),
        candidates: None,
    }
}

/// A present value without evidence, its value `len` bytes long.
fn unsourced(key: &str, len: usize) -> ExtractedValue {
    ExtractedValue {
        status: Status::Present,
        value: Some("x".repeat(len)),
        anchors: Some(vec![]),
        unsourced_reason: Some("r".into()),
        missing_reason: None,
        ..missing(key)
    }
}

fn extraction(values: Vec<ExtractedValue>) -> NewExtraction {
    NewExtraction {
        id: EXT.into(),
        document_id: DOC.into(),
        revision: 1,
        schema_sha256: SCHEMA.into(),
        extractor_version: "1".into(),
        model_id: "qwen3-8b".into(),
        text_layer_sha256: LAYER.into(),
        values,
    }
}

async fn put(storage: &Storage, e: &NewExtraction) -> Result<(), sqlx::Error> {
    let mut tx = storage.db.pool().begin().await.unwrap();
    insert(&mut tx, e).await?;
    tx.commit().await
}

async fn get(state: &AppState, query: &str) -> (StatusCode, Value) {
    let res = crate::router(state.clone())
        .oneshot(
            Request::get(format!("/extractions/{query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 21)
        .await
        .unwrap();
    assert!(bytes.len() <= PAGE_BYTES, "{} bytes", bytes.len());
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn keys(page: &Value) -> Vec<String> {
    page["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["key"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn values_read_back_exactly_as_stored() {
    let s = store().await;
    let present = ExtractedValue {
        status: Status::Present,
        value: Some("2026年10月12日".into()),
        normalized: Some("2026-10-12".into()),
        anchors: Some(vec![anchor(&s.content, "停水时间：2026年10月12日")]),
        missing_reason: None,
        ..missing("shutoff_date")
    };
    let conflict = ExtractedValue {
        status: Status::Conflict,
        missing_reason: None,
        candidates: Some(vec![
            Candidate {
                value: "40元".into(),
                normalized: "40".into(),
                anchors: vec![anchor(&s.content, "补领40元/证")],
                unsourced_reason: None,
            },
            Candidate {
                value: "20元".into(),
                normalized: "20".into(),
                anchors: vec![],
                unsourced_reason: Some("the quote is not in the block".into()),
            },
        ]),
        ..missing("fee")
    };
    let unread = ExtractedValue {
        missing_reason: Some(MissingReason::Unread),
        ..missing("deadline")
    };
    let values = vec![present, conflict, unread];
    put(&s.storage, &extraction(values.clone())).await.unwrap();
    let (status, page) = get(&s.state, EXT).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let read: Vec<ExtractedValue> = serde_json::from_value(page["values"].clone()).unwrap();
    assert_eq!(read, values);
    for (field, want) in [
        ("extraction_id", json!(EXT)),
        ("document_id", json!(DOC)),
        ("revision", json!(1)),
        ("content_sha256", json!(s.content)),
        ("schema_sha256", json!(SCHEMA)),
        ("extractor_version", json!("1")),
        ("model_id", json!("qwen3-8b")),
        ("next_cursor", Value::Null),
    ] {
        assert_eq!(page[field], want, "{field}");
    }
    assert!(
        crate::timestamp::is_timestamp(page["created_at"].as_str().unwrap()),
        "{}",
        page["created_at"]
    );
}

#[tokio::test]
async fn pages_go_in_field_order_fifty_by_default() {
    let s = store().await;
    put(
        &s.storage,
        &extraction((0..51).map(|n| missing(&format!("f{n}"))).collect()),
    )
    .await
    .unwrap();
    let (_, first) = get(&s.state, EXT).await;
    assert_eq!(
        (keys(&first).len(), &first["next_cursor"]),
        (50, &json!("v50"))
    );
    let (_, rest) = get(&s.state, &format!("{EXT}?cursor=v50")).await;
    assert_eq!(
        (keys(&rest), &rest["next_cursor"]),
        (vec!["f50".to_owned()], &Value::Null)
    );
    let mut all = Vec::new();
    let mut page = get(&s.state, &format!("{EXT}?limit=7")).await.1;
    loop {
        all.extend(keys(&page));
        let Some(next) = page["next_cursor"].as_str() else {
            break;
        };
        page = get(&s.state, &format!("{EXT}?limit=7&cursor={next}"))
            .await
            .1;
    }
    assert_eq!(all, (0..51).map(|n| format!("f{n}")).collect::<Vec<_>>());
}

/// The size of `v` serialized, as stored.
fn size(v: &ExtractedValue) -> usize {
    serde_json::to_string(v).unwrap().len()
}

#[tokio::test]
async fn a_page_fills_to_512_kib_exactly_and_no_further() {
    // The envelope as the page counts it: the longest cursor, no values.
    let head = Extraction {
        extraction_id: EXT.into(),
        document_id: DOC.into(),
        revision: 1,
        content_sha256: SCHEMA.into(),
        schema_sha256: SCHEMA.into(),
        extractor_version: "1".into(),
        model_id: "qwen3-8b".into(),
        created_at: "2026-10-10T12:00:00.000Z".into(),
        values: vec![],
        next_cursor: Some("v99".into()),
    };
    let envelope = serde_json::to_vec(&head).unwrap().len();
    let base = size(&unsourced("b", 0));
    let a = unsourced("a", 200 * 1024);
    // Two values and their commas fill the page to the byte.
    let fill = PAGE_BYTES - envelope - (size(&a) + 1) - 1 - base;
    for (extra, on_first) in [(0, 2), (1, 1)] {
        let s = store().await;
        let b = unsourced("b", fill + extra);
        put(&s.storage, &extraction(vec![a.clone(), b, missing("c")]))
            .await
            .unwrap();
        let (status, page) = get(&s.state, EXT).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(keys(&page).len(), on_first, "extra {extra}");
    }
}

#[tokio::test]
async fn nothing_a_reader_would_reject_is_stored() {
    let s = store().await;
    let good = || unsourced("a", 1);
    let bad: Vec<(&str, ExtractedValue)> = vec![
        (
            "bad key",
            ExtractedValue {
                key: "Bad Key".into(),
                ..good()
            },
        ),
        (
            "no anchors, no reason",
            ExtractedValue {
                unsourced_reason: None,
                ..good()
            },
        ),
        (
            "anchors and a reason",
            ExtractedValue {
                anchors: Some(vec![anchor(&s.content, "q")]),
                ..good()
            },
        ),
        (
            "present without value",
            ExtractedValue {
                value: None,
                ..good()
            },
        ),
        (
            "missing with a value",
            ExtractedValue {
                value: Some("v".into()),
                ..missing("m")
            },
        ),
        (
            "17 anchors",
            ExtractedValue {
                anchors: Some(vec![anchor(&s.content, "q"); 17]),
                unsourced_reason: None,
                ..good()
            },
        ),
        (
            "anchor without geometry",
            ExtractedValue {
                anchors: Some(vec![Anchor {
                    geometry: None,
                    ..anchor(&s.content, "q")
                }]),
                unsourced_reason: None,
                ..good()
            },
        ),
        (
            "one candidate",
            ExtractedValue {
                status: Status::Conflict,
                missing_reason: None,
                candidates: Some(vec![Candidate {
                    value: "a".into(),
                    normalized: "a".into(),
                    anchors: vec![],
                    unsourced_reason: Some("r".into()),
                }]),
                ..missing("c")
            },
        ),
        ("too large", unsourced("big", MAX_VALUE_BYTES)),
        (
            "bad media type",
            ExtractedValue {
                anchors: Some(vec![Anchor {
                    media_type: "Application/PDF".into(),
                    page: None,
                    geometry: None,
                    ..anchor(&s.content, "q")
                }]),
                unsourced_reason: None,
                ..good()
            },
        ),
    ];
    for (what, v) in bad {
        assert!(
            put(&s.storage, &extraction(vec![missing("ok"), v]))
                .await
                .is_err(),
            "{what}"
        );
    }
    assert!(
        put(
            &s.storage,
            &extraction((0..101).map(|n| missing(&format!("f{n}"))).collect())
        )
        .await
        .is_err()
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM extractions")
        .fetch_one(s.storage.db.pool())
        .await
        .unwrap();
    assert_eq!(n, 0, "nothing was stored");
    // Rolled back with its job: never seen.
    let mut tx = s.storage.db.pool().begin().await.unwrap();
    insert(&mut tx, &extraction(vec![good()])).await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(get(&s.state, EXT).await.0, StatusCode::NOT_FOUND);
}

/// A value stored behind insert's back is checked on the way out.
#[tokio::test]
async fn a_broken_stored_value_is_the_os_failing() {
    // A declared anchor field that is null (a reader would take it as absent).
    let mut nulled = serde_json::to_value(ExtractedValue {
        status: Status::Present,
        value: Some("v".into()),
        anchors: Some(vec![anchor(SCHEMA, "q")]),
        missing_reason: None,
        ..missing("a")
    })
    .unwrap();
    nulled["anchors"][0]["page"] = Value::Null;
    for raw in [
        nulled.to_string(),
        "{}".to_owned(),
        json!({ "key": "a", "status": "present", "value": 42, "anchors": [], "unsourced_reason": "r" }).to_string(),
        json!({ "key": "a", "status": "present", "value": "v", "anchors": [] }).to_string(),
        // Fits the store's checks but not a page with its envelope.
        serde_json::to_string(&unsourced("a", PAGE_BYTES)).unwrap(),
        // No property is nullable; serde alone would read this as absent.
        json!({ "key": "a", "status": "missing", "missing_reason": "unread", "value": null }).to_string(),
        json!({ "key": "a", "status": "conflict", "candidates": [
            { "value": "a", "normalized": "a", "anchors": [], "unsourced_reason": "r" },
            { "value": "b", "normalized": "b", "anchors": [], "unsourced_reason": "r", "x": null } ] }).to_string(),
        json!({ "key": "a", "status": "present", "value": "v", "anchors": [{ "media_type": "garbage" }] }).to_string(),
    ] {
        let s = store().await;
        put(&s.storage, &extraction(vec![])).await.unwrap();
        sqlx::query("INSERT INTO extraction_values (extraction_id, ord, value) VALUES (?, 0, ?)")
            .bind(EXT)
            .bind(&raw)
            .execute(s.storage.db.pool())
            .await
            .unwrap();
        let (status, body) = get(&s.state, EXT).await;
        assert_eq!((status, &body["error"]["code"]), (StatusCode::INTERNAL_SERVER_ERROR, &json!("internal")), "{}", &raw[..raw.len().min(80)]);
    }
}

#[tokio::test]
async fn bad_requests_and_missing_extractions() {
    let s = store().await;
    put(&s.storage, &extraction(vec![missing("a")]))
        .await
        .unwrap();
    for query in [
        "ext_x".to_owned(),
        format!("{EXT}?limit=0"),
        format!("{EXT}?limit=201"),
        format!("{EXT}?cursor=x"),
        format!("{EXT}?cursor=v100"),
        format!("{EXT}?cursor=v-1"),
        format!("{EXT}?cursor=v001"),
        format!("{EXT}?zoom=1"),
    ] {
        let (status, body) = get(&s.state, &query).await;
        assert_eq!(
            (status, &body["error"]["code"]),
            (StatusCode::BAD_REQUEST, &json!("invalid_request")),
            "{query}"
        );
    }
    let (status, body) = get(&s.state, "ext_01K75A0B1C2D3E4F5G6H7J8K9N").await;
    assert_eq!(
        (status, &body["error"]["code"]),
        (StatusCode::NOT_FOUND, &json!("not_found"))
    );
}

/// What is sent is what was stored: an anchor's extra property (the
/// contract allows them) stays, and the page is counted as sent, not as
/// stored: `1e2` goes out as `100.0`.
#[tokio::test]
async fn values_go_out_as_stored_and_are_counted_as_sent() {
    let s = store().await;
    let mut a = serde_json::to_value(ExtractedValue {
        status: Status::Present,
        value: Some("v".into()),
        anchors: Some(vec![anchor(&s.content, "q")]),
        missing_reason: None,
        ..missing("a")
    })
    .unwrap();
    a["anchors"][0]["note"] = json!("kept");
    // Extra properties are the contract's to allow, nulls and all.
    a["anchors"][0]["extra"] = json!({ "optional": null });
    a["anchors"][0]["engine"]["metadata"] = Value::Null;
    // Rects whose numbers grow when sent: 18 bytes each stored, 26 sent.
    let rects = vec!["[1e2,1e2,1e2,1e2]"; 16 * 1024].join(",");
    let raw = a
        .to_string()
        .replace("[[72.0,100.0,300.0,112.0]]", &format!("[{rects}]"));
    assert!(raw.len() < 300 * 1024 && raw.contains("1e2"));
    put(&s.storage, &extraction(vec![])).await.unwrap();
    let b = serde_json::to_string(&unsourced("b", PAGE_BYTES - 2048 - raw.len() - 1024)).unwrap();
    for (ord, v) in [raw.as_str(), b.as_str()].into_iter().enumerate() {
        sqlx::query("INSERT INTO extraction_values (extraction_id, ord, value) VALUES (?, ?, ?)")
            .bind(EXT)
            .bind(ord as i64)
            .bind(v)
            .execute(s.storage.db.pool())
            .await
            .unwrap();
    }
    // Stored, both fit a page; sent, they do not: the second waits.
    let (status, page) = get(&s.state, EXT).await;
    assert_eq!(status, StatusCode::OK, "{}", page["error"]);
    assert_eq!(
        (keys(&page), &page["next_cursor"]),
        (vec!["a".to_owned()], &json!("v1"))
    );
    assert_eq!(page["values"][0]["anchors"][0]["note"], "kept");
    assert_eq!(
        page["values"][0]["anchors"][0]["extra"],
        json!({ "optional": null })
    );
    assert_eq!(
        page["values"][0]["anchors"][0]["engine"]["metadata"],
        Value::Null
    );
    assert_eq!(
        page["values"][0]["anchors"][0]["geometry"]["rects"][0],
        json!([100.0, 100.0, 100.0, 100.0])
    );
}
