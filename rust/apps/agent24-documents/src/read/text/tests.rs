#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::engine::{Engine, EngineError, Layers, Parse};
use crate::events::Events;
use crate::state::AppState;
use crate::text_layer::{Block, EngineRef, Line, ParseStatus, Region, TextLayer};

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00";

/// What the stand-in engine reads: `pages` pages, `per_page` blocks of
/// `text` on each, and an unread region on each page in `unread`.
#[derive(Clone)]
struct Shape {
    pages: u32,
    per_page: usize,
    text: String,
    unread: Vec<u32>,
    fail: Option<EngineError>,
}

struct Scripted(Shape, AtomicUsize);

impl Engine for Scripted {
    fn engine(&self) -> EngineRef {
        EngineRef {
            id: "apple-pdfkit".into(),
            version: "26.6.2".into(),
        }
    }
    fn config(&self) -> Value {
        json!({ "blocks": "test" })
    }
    fn parse(&self, content: &str, _media: &str, _path: &Path) -> Parse {
        self.1.fetch_add(1, SeqCst);
        let s = self.0.clone();
        let (engine, config, content) = (self.engine(), self.config(), content.to_owned());
        Box::pin(async move {
            if let Some(e) = s.fail {
                return Err(e);
            }
            let blocks = (1..=s.pages)
                .flat_map(|p| (1..=s.per_page).map(move |n| (p, n)))
                .map(|(page, n)| Block {
                    block_id: format!("p{page}/b{n}"),
                    page,
                    text: s.text.clone(),
                    lines: vec![Line {
                        start: 0,
                        end: s.text.len(),
                        rect: [72.0, 100.0, 300.0, 112.0],
                    }],
                })
                .collect();
            let unparsed_regions: Vec<Region> = s
                .unread
                .iter()
                .map(|&page| Region {
                    page,
                    rects: vec![[0.0, 0.0, 612.0, 792.0]],
                    reason: "ocr_failed".into(),
                })
                .collect();
            Ok(TextLayer {
                v: 1,
                content_sha256: content,
                engine,
                config,
                pages: s.pages,
                parse_status: if unparsed_regions.is_empty() {
                    ParseStatus::Complete
                } else {
                    ParseStatus::Partial
                },
                unparsed_regions,
                blocks,
            })
        })
    }
}

fn shape(pages: u32, per_page: usize) -> Shape {
    Shape {
        pages,
        per_page,
        text: "通告".into(),
        unread: vec![],
        fail: None,
    }
}

struct Env {
    _dir: tempfile::TempDir,
    state: AppState,
    engine: Arc<Scripted>,
}

/// A document with r1 (`media`), read by an engine of `shape` (or none).
async fn env(shape: Option<Shape>, media: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let engine = Arc::new(Scripted(
        shape.clone().unwrap_or_else(|| self::shape(1, 1)),
        AtomicUsize::new(0),
    ));
    let layers = Layers::new(shape.map(|_| engine.clone() as Arc<dyn Engine>));
    let state = AppState::open_serving(dir.path(), Events::default(), layers).await;
    let storage = state.storage().await.unwrap();
    let blob = storage.blobs.put_bytes(b"%PDF-1.7 bytes").unwrap();
    for sql in [
        format!(
            "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{DOC}', 't', '{media}', 1)"
        ),
        format!(
            "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
             VALUES ('{DOC}', 1, '{}', {}, '{media}', 'import')",
            blob.sha256, blob.size
        ),
    ] {
        sqlx::query(&sql).execute(storage.db.pool()).await.unwrap();
    }
    Env {
        _dir: dir,
        state,
        engine,
    }
}

async fn get(env: &Env, query: &str) -> (StatusCode, Value) {
    let uri = format!("/documents/{DOC}/revisions/1/text{query}");
    let res = crate::router(env.state.clone())
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 21)
        .await
        .unwrap();
    assert!(bytes.len() <= super::PAGE_BYTES, "{} bytes", bytes.len());
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Every response from `query`, following cursors.
async fn all(env: &Env, query: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let (mut status, mut v) = get(env, query).await;
    loop {
        assert_eq!(status, StatusCode::OK, "{v}");
        let next = v["next_cursor"].as_str().map(str::to_owned);
        out.push(v);
        let Some(c) = next else { return out };
        (status, v) = get(env, &format!("?cursor={c}")).await;
    }
}

fn ids(pages: &[Value]) -> Vec<String> {
    pages
        .iter()
        .flat_map(|p| {
            p["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b["block_id"].as_str().unwrap().to_owned())
        })
        .collect()
}

#[tokio::test]
async fn a_small_layer_comes_back_whole_as_the_contract_shapes_it() {
    let env = env(Some(shape(2, 2)), "application/pdf").await;
    let (status, v) = get(&env, "").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        ids(std::slice::from_ref(&v)),
        ["p1/b1", "p1/b2", "p2/b1", "p2/b2"]
    );
    let b = &v["blocks"][0];
    assert_eq!(
        (b["page"].clone(), b["text"].clone()),
        (json!(1), json!("通告"))
    );
    assert_eq!(
        b["text_sha256"],
        crate::text_layer::sha256_address("通告".as_bytes())
    );
    assert_eq!(
        b["geometry"],
        json!({ "box": "CropBox", "unit": "pt", "origin": "top-left-rotated", "rects": [[72.0, 100.0, 300.0, 112.0]] })
    );
    assert_eq!(
        (v["parse_status"].clone(), v["unparsed_regions"].clone()),
        (json!("complete"), json!([]))
    );
    assert_eq!(
        (v["engine"]["id"].clone(), v["next_cursor"].clone()),
        (json!("apple-pdfkit"), Value::Null)
    );
    assert_eq!(v["document_id"], DOC);
    // Read again: from the pinned layer, not a second parse.
    assert_eq!(
        get(&env, "").await.1["text_layer_sha256"],
        v["text_layer_sha256"]
    );
    assert_eq!(env.engine.1.load(SeqCst), 1);
}

#[tokio::test]
async fn cursors_page_through_every_block_once() {
    let env = env(Some(shape(3, 2)), "application/pdf").await;
    let pages = all(&env, "?limit=4").await;
    assert_eq!(pages.len(), 2);
    assert_eq!(
        ids(&pages),
        ["p1/b1", "p1/b2", "p2/b1", "p2/b2", "p3/b1", "p3/b2"]
    );
    // Starting at a block.
    assert_eq!(
        ids(&all(&env, "?block=p2/b2").await),
        ["p2/b2", "p3/b1", "p3/b2"]
    );
}

#[tokio::test]
async fn a_response_covers_at_most_100_pages_and_512_kib() {
    let env = env(Some(shape(150, 1)), "application/pdf").await;
    let pages = all(&env, "?limit=200").await;
    assert_eq!(
        pages
            .iter()
            .map(|p| p["blocks"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [100, 50]
    );
    // Blocks of 16 KiB: cut by bytes, never by more than the cap.
    let big = Shape {
        text: "x".repeat(16 * 1024),
        ..shape(1, 80)
    };
    let env = self::env(Some(big), "application/pdf").await;
    let pages = all(&env, "?limit=200").await;
    assert!(pages.len() > 1 && ids(&pages).len() == 80);
}

#[tokio::test]
async fn unread_pages_are_reported_for_the_pages_a_response_covers() {
    // Nothing read at all: the cursor still walks every page.
    let none = Shape {
        per_page: 0,
        unread: (1..=150).collect(),
        ..shape(150, 0)
    };
    let env = env(Some(none), "application/pdf").await;
    let pages = all(&env, "").await;
    assert_eq!(pages.len(), 2);
    let regions: Vec<usize> = pages
        .iter()
        .map(|p| p["unparsed_regions"].as_array().unwrap().len())
        .collect();
    assert_eq!(
        (regions, pages[0]["parse_status"].clone()),
        (vec![100, 50], json!("partial"))
    );
    // Page 3 unread: the first response (pages 1–2) is complete.
    let some = Shape {
        unread: vec![3],
        ..shape(3, 1)
    };
    let env = self::env(Some(some), "application/pdf").await;
    let pages = all(&env, "?limit=2").await;
    assert_eq!(pages[0]["parse_status"], "complete");
    assert_eq!(pages[1]["unparsed_regions"][0]["reason"], "ocr_failed");
    let (status, v) = get(&env, "?require_complete=true").await;
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::UNPROCESSABLE_ENTITY, json!("partial_parse"))
    );
}

#[tokio::test]
async fn bad_requests_are_refused() {
    let env = env(Some(shape(2, 1)), "application/pdf").await;
    let c = get(&env, "?limit=1").await.1["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    for (query, want) in [
        (format!("?block=p1/b1&cursor={c}"), StatusCode::BAD_REQUEST),
        ("?limit=0".into(), StatusCode::BAD_REQUEST),
        ("?limit=201".into(), StatusCode::BAD_REQUEST),
        ("?cursor=zz".into(), StatusCode::BAD_REQUEST),
        ("?cursor=74".into(), StatusCode::BAD_REQUEST),
        ("?other=1".into(), StatusCode::BAD_REQUEST),
        ("?block=p9/b9".into(), StatusCode::NOT_FOUND),
    ] {
        assert_eq!(get(&env, &query).await.0, want, "{query}");
    }
    // A cursor is bound to its document and revision.
    let other = format!("/documents/{DOC}/revisions/2/text?cursor={c}");
    let res = crate::router(env.state.clone())
        .oneshot(Request::get(other).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = crate::router(env.state.clone())
        .oneshot(
            Request::get("/documents/doc_x/revisions/1/text")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn engine_trouble_is_engine_unavailable_or_a_parse_failure() {
    let cases = [
        (
            None,
            "application/pdf",
            StatusCode::SERVICE_UNAVAILABLE,
            "engine_unavailable",
        ),
        (
            Some(Shape {
                fail: Some(EngineError::Unsupported),
                ..shape(1, 1)
            }),
            "application/pdf",
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_format",
        ),
        (
            Some(Shape {
                fail: Some(EngineError::Failed("x".into())),
                ..shape(1, 1)
            }),
            "application/pdf",
            StatusCode::UNPROCESSABLE_ENTITY,
            "parse_failed",
        ),
        (
            Some(shape(1, 1)),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_format",
        ),
    ];
    for (shape, media, status, code) in cases {
        let env = env(shape, media).await;
        let (got, v) = get(&env, "").await;
        assert_eq!(
            (got, v["error"]["code"].clone()),
            (status, json!(code)),
            "{media} {v}"
        );
        if code == "engine_unavailable" {
            assert_eq!(
                (
                    v["error"]["details"]["retryable"].clone(),
                    v["error"]["details"]["engine"].clone()
                ),
                (json!(true), json!("apple-pdfkit"))
            );
        }
    }
}

#[tokio::test]
async fn capabilities_route_read_range_when_the_engine_is_here() {
    for (shape, available) in [(Some(shape(1, 1)), true), (None, false)] {
        let env = env(shape, "application/pdf").await;
        let caps = crate::capabilities(&env.state).await;
        let op = caps
            .operations
            .iter()
            .find(|o| o.op == "read_range")
            .unwrap();
        assert_eq!(op.available, available);
    }
}

#[tokio::test]
async fn a_cursor_reads_only_the_revision_and_layer_it_was_issued_for() {
    let env = env(Some(shape(2, 1)), "application/pdf").await;
    let storage = env.state.storage().await.unwrap();
    // A second document of the same bytes, so of the same layer.
    let twin = "doc_01K74Z3QJ8V5N2W9RTX6YB4M01";
    let mine = super::revision(&storage, DOC, 1).await.unwrap();
    for sql in [
        format!(
            "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{twin}', 't', 'application/pdf', 1)"
        ),
        format!(
            "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
             VALUES ('{twin}', 1, '{}', 14, 'application/pdf', 'import')",
            mine.content_sha256
        ),
    ] {
        sqlx::query(&sql).execute(storage.db.pool()).await.unwrap();
    }
    let c = get(&env, "?limit=1").await.1["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let uri = format!("/documents/{twin}/revisions/1/text?cursor={c}");
    let res = crate::router(env.state.clone())
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "another document's cursor"
    );
    // A cursor for this revision naming a layer read from other bytes.
    let blob = storage.blobs.put_bytes(b"other bytes").unwrap();
    let layer = layer_for(&env, &blob.sha256).await;
    let theirs = crate::text_layer::pin(&storage, &blob.sha256, &layer)
        .await
        .unwrap();
    let forged = super::encode_cursor(
        "t",
        &mine,
        &theirs,
        super::Position {
            page: 1,
            block: 0,
            hit: 0,
        },
        "",
    );
    assert_eq!(
        get(&env, &format!("?cursor={forged}")).await.0,
        StatusCode::BAD_REQUEST
    );
}

/// The stand-in engine's layer of `content`.
async fn layer_for(env: &Env, content: &str) -> TextLayer {
    env.engine
        .parse(content, "application/pdf", Path::new("/x"))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_cursor_must_point_into_its_layer_and_requests_are_checked_first() {
    let env = env(Some(shape(2, 1)), "application/pdf").await;
    get(&env, "").await;
    let storage = env.state.storage().await.unwrap();
    let r = super::revision(&storage, DOC, 1).await.unwrap();
    let layer = get(&env, "").await.1["text_layer_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    for (page, block, hit) in [(3, 0, 0), (u32::MAX, 0, 0), (1, 2, 0), (1, 0, 1)] {
        let at = super::Position { page, block, hit };
        let c = super::encode_cursor("t", &r, &layer, at, "");
        assert_eq!(
            get(&env, &format!("?cursor={c}")).await.0,
            StatusCode::BAD_REQUEST,
            "{at:?}"
        );
    }
    // Past the last block of a page is where the next page starts: fine.
    let c = super::encode_cursor(
        "t",
        &r,
        &layer,
        super::Position {
            page: 1,
            block: 1,
            hit: 0,
        },
        "",
    );
    assert_eq!(ids(&all(&env, &format!("?cursor={c}")).await), ["p2/b1"]);
    // No engine here, yet a bad request is still a bad request, as JSON.
    let none = self::env(None, "application/pdf").await;
    assert_eq!(get(&none, "?block=").await.0, StatusCode::BAD_REQUEST);
    for uri in [
        format!("/documents/{DOC}/revisions/nope/text"),
        format!("/documents/{DOC}/revisions/99999999999999999999/text"),
    ] {
        let res = crate::router(none.state.clone())
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(res.into_body(), 1 << 16)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"]["code"], "invalid_request");
    }
}
