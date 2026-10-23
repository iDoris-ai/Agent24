#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::engine::{Engine, Layers, Parse};
use crate::events::Events;
use crate::state::AppState;
use crate::text_layer::{Block, EngineRef, Line, ParseStatus, Region, TextLayer};

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00";

/// An engine that reads the given blocks (one line per line of text).
struct Given(Vec<(u32, &'static str)>, u32, Vec<u32>);

impl Engine for Given {
    fn engine(&self) -> EngineRef {
        EngineRef {
            id: "apple-pdfkit".into(),
            version: "26.6.2".into(),
        }
    }
    fn config(&self) -> Value {
        json!({ "blocks": "test" })
    }
    fn parse(&self, content: &str, _: &str, _: &Path) -> Parse {
        let mut n = std::collections::HashMap::<u32, usize>::new();
        let blocks = self
            .0
            .iter()
            .map(|&(page, text)| {
                let k = n.entry(page).or_default();
                *k += 1;
                let mut at = 0;
                let lines = text
                    .split_inclusive('\n')
                    .enumerate()
                    .map(|(i, l)| {
                        let line = Line {
                            start: at,
                            end: at + l.len(),
                            rect: [
                                72.0,
                                100.0 + 20.0 * i as f64,
                                300.0,
                                112.0 + 20.0 * i as f64,
                            ],
                        };
                        at += l.len();
                        line
                    })
                    .collect();
                Block {
                    block_id: format!("p{page}/b{k}"),
                    page,
                    text: text.into(),
                    lines,
                }
            })
            .collect();
        let regions: Vec<Region> = self
            .2
            .iter()
            .map(|&page| Region {
                page,
                rects: vec![[0.0, 0.0, 612.0, 792.0]],
                reason: "ocr_failed".into(),
            })
            .collect();
        let layer = TextLayer {
            v: 1,
            content_sha256: content.into(),
            engine: self.engine(),
            config: self.config(),
            pages: self.1,
            parse_status: if regions.is_empty() {
                ParseStatus::Complete
            } else {
                ParseStatus::Partial
            },
            unparsed_regions: regions,
            blocks,
        };
        Box::pin(async move { Ok(layer) })
    }
}

async fn env(engine: Option<Arc<dyn Engine>>) -> AppState {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let state = AppState::open_serving(dir.path(), Events::default(), Layers::new(engine)).await;
    let storage = state.storage().await.unwrap();
    let blob = storage.blobs.put_bytes(b"%PDF-1.7 bytes").unwrap();
    for sql in [
        format!(
            "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{DOC}', 't', 'application/pdf', 1)"
        ),
        format!(
            "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
             VALUES ('{DOC}', 1, '{}', {}, 'application/pdf', 'import')",
            blob.sha256, blob.size
        ),
    ] {
        sqlx::query(&sql).execute(storage.db.pool()).await.unwrap();
    }
    state
}

async fn post(state: &AppState, body: Value) -> (StatusCode, Value) {
    let req = Request::post(format!("/documents/{DOC}/find"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = crate::router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 21)
        .await
        .unwrap();
    assert!(bytes.len() <= super::PAGE_BYTES);
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Every match of `query`, following cursors, `limit` at a time.
async fn all(state: &AppState, query: &str, limit: usize) -> Vec<Value> {
    let mut out = Vec::new();
    let mut body = json!({ "revision": 1, "query": query, "limit": limit });
    loop {
        let (status, v) = post(state, body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        out.extend(v["matches"].as_array().unwrap().iter().cloned());
        let Some(c) = v["next_cursor"].as_str() else {
            return out;
        };
        body["cursor"] = json!(c);
    }
}

#[tokio::test]
async fn a_match_is_an_anchor_with_the_lines_it_touches() {
    let state = env(Some(Arc::new(Given(
        vec![(1, "国务院办公厅\n端午\n节：6月19日放假"), (2, "端午节")],
        2,
        vec![],
    ))))
    .await;
    let (status, v) = post(&state, json!({ "revision": 1, "query": "端午节" })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let m = &v["matches"][0];
    assert_eq!(
        (m["block_id"].clone(), m["page"].clone(), m["quote"].clone()),
        (json!("p1/b1"), json!(1), json!("端午\n节"))
    );
    let text = "国务院办公厅\n端午\n节：6月19日放假";
    assert_eq!(
        m["text_range"],
        json!({ "unit": "utf8", "start": 19, "end": 29 })
    );
    assert_eq!(&text[19..29], "端午\n节");
    assert_eq!(
        m["block_text_sha256"],
        crate::text_layer::sha256_address(text.as_bytes())
    );
    // Only lines 2 and 3 of the block, which the match touches.
    assert_eq!(
        m["geometry"]["rects"],
        json!([[72.0, 120.0, 300.0, 132.0], [72.0, 140.0, 300.0, 152.0]])
    );
    for key in [
        "document_id",
        "revision",
        "content_sha256",
        "media_type",
        "text_layer_sha256",
        "engine",
    ] {
        assert!(!m[key].is_null(), "{key}");
    }
    assert_eq!(
        (
            v["matches"][1]["page"].clone(),
            v["parse_status"].clone(),
            v["next_cursor"].clone()
        ),
        (json!(2), json!("complete"), Value::Null)
    );
}

#[tokio::test]
async fn paging_continues_inside_a_block_and_over_pages_without_matches() {
    let state = env(Some(Arc::new(Given(
        vec![(1, "ab ab ab ab ab"), (150, "ab")],
        150,
        vec![],
    ))))
    .await;
    let got = all(&state, "ab", 2).await;
    let at: Vec<_> = got
        .iter()
        .map(|m| {
            (
                m["page"].as_u64().unwrap(),
                m["text_range"]["start"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(at, [(1, 0), (1, 3), (1, 6), (1, 9), (1, 12), (150, 0)]);
    // Pages 2–149 hold nothing: a response may carry no match yet a cursor.
    let (_, v) = post(&state, json!({ "revision": 1, "query": "zz" })).await;
    assert_eq!(
        (v["matches"].clone(), v["next_cursor"].is_string()),
        (json!([]), true)
    );
}

#[tokio::test]
async fn unread_regions_bound_what_no_match_means() {
    let state = env(Some(Arc::new(Given(vec![(1, "a"), (2, "b")], 3, vec![3])))).await;
    let (_, v) = post(&state, json!({ "revision": 1, "query": "a" })).await;
    assert_eq!(
        (
            v["parse_status"].clone(),
            v["unparsed_regions"][0]["page"].clone()
        ),
        (json!("partial"), json!(3))
    );
    let (status, v) = post(
        &state,
        json!({ "revision": 1, "query": "a", "require_complete": true }),
    )
    .await;
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::UNPROCESSABLE_ENTITY, json!("partial_parse"))
    );
}

#[tokio::test]
async fn bad_requests_are_refused_before_any_reading() {
    let state = env(Some(Arc::new(Given(vec![(1, "ab ab")], 1, vec![])))).await;
    let c = post(&state, json!({ "revision": 1, "query": "ab", "limit": 1 }))
        .await
        .1["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    for body in [
        json!({ "revision": 1, "query": "" }),
        json!({ "revision": 1, "query": "  " }),
        json!({ "revision": 1, "query": "x".repeat(501) }),
        json!({ "revision": 1, "query": "ab", "limit": 0 }),
        json!({ "revision": 1, "query": "ab", "limit": 201 }),
        json!({ "revision": 1, "query": "ab", "other": 1 }),
        json!({ "revision": 0, "query": "ab" }),
        json!({ "revision": 1, "query": "ab ", "cursor": c }),
        json!({ "revision": 1, "query": "ab", "cursor": "zz" }),
    ] {
        assert_eq!(
            post(&state, body.clone()).await.0,
            StatusCode::BAD_REQUEST,
            "{body}"
        );
    }
    assert_eq!(
        post(&state, json!({ "revision": 2, "query": "ab" }))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // A signed cursor past the block's matches, or past the page's blocks.
    let storage = state.storage().await.unwrap();
    let r = crate::read::revision(&storage, DOC, 1).await.unwrap();
    let layer = post(&state, json!({ "revision": 1, "query": "ab" }))
        .await
        .1["matches"][0]["text_layer_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let bound = crate::text_layer::sha256_address(b"ab");
    for (block, hit) in [(0, 2), (0, 9), (2, 0)] {
        let at = crate::read::Position {
            page: 1,
            block,
            hit,
        };
        let c = crate::read::encode_cursor("f", &r, &layer, at, &bound);
        let (status, _) = post(&state, json!({ "revision": 1, "query": "ab", "cursor": c })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{at:?}");
    }
    // A huge query of distinct characters is refused at once, not compiled.
    let huge: String = (0x2_0000..0x2_0000 + 100_000)
        .filter_map(char::from_u32)
        .collect();
    let start = std::time::Instant::now();
    assert_eq!(
        post(&state, json!({ "revision": 1, "query": huge }))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
    // 500 characters is fine, whatever their bytes.
    assert_eq!(
        post(&state, json!({ "revision": 1, "query": "通".repeat(500) }))
            .await
            .0,
        StatusCode::OK
    );
    // No engine: a bad request is still a bad request.
    let none = env(None).await;
    assert_eq!(
        post(&none, json!({ "revision": 1, "query": "" })).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(&none, json!({ "revision": 1, "query": "ab" })).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let caps = crate::capabilities(&state).await;
    assert!(
        caps.operations
            .iter()
            .any(|o| o.op == "find" && o.available)
    );
}

/// The helper's real output for two S01 samples: every gold quote is found
/// on the page its gold anchor names.
#[tokio::test]
async fn the_s01_gold_quotes_are_found_on_their_pages() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for sample in ["s01-02-en-epa-boil-water", "s01-03-zh-holiday-2026"] {
        let fixture = root.join(format!("tests/fixtures/pdfkit/{sample}.json"));
        let read: crate::text_layer::build::Read =
            serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
        let state = env(Some(Arc::new(Real(std::sync::Mutex::new(Some(read)))))).await;
        let gold: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join(format!(
                "../../../docs/documenting/samples/s01/{sample}/gold.json"
            )))
            .unwrap(),
        )
        .unwrap();
        let mut anchors = Vec::new();
        collect(&gold, &mut anchors);
        assert!(!anchors.is_empty());
        for (page, quote) in anchors {
            let found = all(&state, &quote, 200).await;
            assert!(
                found.iter().any(|m| m["page"] == page),
                "{sample} p{page}: {quote}"
            );
        }
    }
}

/// An engine whose report is a recorded helper report.
struct Real(std::sync::Mutex<Option<crate::text_layer::build::Read>>);

impl Engine for Real {
    fn engine(&self) -> EngineRef {
        EngineRef {
            id: "apple-pdfkit".into(),
            version: "26.6.2".into(),
        }
    }
    fn config(&self) -> Value {
        json!({ "blocks": "rows-v1" })
    }
    fn parse(&self, content: &str, _: &str, _: &Path) -> Parse {
        let read = self.0.lock().unwrap().take().unwrap();
        let layer = crate::text_layer::build::layer(content, self.engine(), self.config(), read);
        Box::pin(async move { layer.map_err(crate::engine::EngineError::Failed) })
    }
}

fn collect(v: &Value, out: &mut Vec<(u64, String)>) {
    match v {
        Value::Object(m) => {
            if let (Some(Value::String(q)), Some(p)) =
                (m.get("quote"), m.get("page").and_then(Value::as_u64))
            {
                out.push((p, q.clone()));
            }
            m.values().for_each(|v| collect(v, out));
        }
        Value::Array(a) => a.iter().for_each(|v| collect(v, out)),
        _ => {}
    }
}
