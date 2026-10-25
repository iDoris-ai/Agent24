//! `read_range` and `find` (DOC-1-06b §3.3 slice C): the same answers as
//! REST, paged at the tool budget.

use std::path::Path;

use super::*;
use crate::engine::{Engine, EngineError, Layers, Parse};
use crate::events::Events;
use crate::text_layer::{Block, EngineRef, Line, ParseStatus, Region, TextLayer};

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00"; // shadows the parent's

/// An engine that reads `pages` (each page's block texts) and an unread
/// region of `rects` rectangles on each page in `unread`.
struct Fixed {
    pages: Vec<Vec<String>>,
    unread: Vec<u32>,
    rects: usize,
}

impl Engine for Fixed {
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
        let blocks = (1..)
            .zip(&self.pages)
            .flat_map(|(page, texts)| {
                (1..).zip(texts).map(move |(n, text)| Block {
                    block_id: format!("p{page}/b{n}"),
                    page,
                    text: text.clone(),
                    lines: vec![Line {
                        start: 0,
                        end: text.len(),
                        rect: [72.0, 100.0, 300.0, 112.0],
                    }],
                })
            })
            .collect();
        // Long decimals, so a region is about 1.2 KiB at 16 rects.
        let rect = [
            0.123_456_789_012_345,
            1.123_456_789_012_345,
            611.123_456_789_01,
            791.123_456_789_01,
        ];
        let unparsed_regions: Vec<Region> = (self.unread.iter())
            .map(|&page| Region {
                page,
                rects: vec![rect; self.rects],
                reason: "ocr_failed".into(),
            })
            .collect();
        let layer = TextLayer {
            v: 1,
            content_sha256: content.to_owned(),
            engine: self.engine(),
            config: self.config(),
            pages: u32::try_from(self.pages.len()).unwrap(),
            parse_status: if unparsed_regions.is_empty() {
                ParseStatus::Complete
            } else {
                ParseStatus::Partial
            },
            unparsed_regions,
            blocks,
        };
        Box::pin(async move { Ok::<_, EngineError>(layer) })
    }
}

fn fixed(pages: Vec<Vec<String>>, unread: Vec<u32>, rects: usize) -> Option<Fixed> {
    Some(Fixed {
        pages,
        unread,
        rects,
    })
}

/// A document with a PDF r1, read by `engine` (or by none).
async fn env(engine: Option<Fixed>) -> (tempfile::TempDir, AppState) {
    env_with(engine.map(|e| Arc::new(e) as Arc<dyn Engine>)).await
}

async fn env_with(engine: Option<Arc<dyn Engine>>) -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let layers = Layers::new(engine);
    let state = AppState::open_serving(dir.path(), Events::default(), layers).await;
    let storage = state.storage().await.unwrap();
    let blob = storage.blobs.put_bytes(b"%PDF-1.7 bytes").unwrap();
    exec(
        &state,
        &format!(
            "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{DOC}', 't', 'application/pdf', 1);
             INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
             VALUES ('{DOC}', 1, '{}', {}, 'application/pdf', 'import');",
            blob.sha256, blob.size
        ),
    )
    .await;
    (dir, state)
}

async fn rest_find(state: &AppState, body: Value) -> (StatusCode, Value) {
    let req = Request::post(format!("/documents/{DOC}/find"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = crate::router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn text_path(query: &str) -> String {
    format!("/documents/{DOC}/revisions/1/text{query}")
}

/// Every tool page of `op` from `args`, following cursors; each fits the
/// kernel's 64 KiB (`post_to`) and the payload budget.
async fn pages(state: &AppState, op: &str, args: Value) -> Vec<Value> {
    let mut out = Vec::new();
    let mut args = args;
    loop {
        let page = payload(&call(state, op, args.clone()).await);
        let size = serde_json::to_vec(&page).unwrap().len();
        assert!(size <= PAYLOAD_BYTES, "{size} bytes");
        let next = page["next_cursor"].as_str().map(str::to_owned);
        out.push(page);
        let Some(c) = next else { return out };
        args["cursor"] = json!(c);
        args.as_object_mut().unwrap().remove("block");
    }
}

/// The regions of every response: a page split across two responses is
/// reported by each, so once per page.
fn regions(pages: &[Value]) -> Value {
    let mut all = joined(pages, "unparsed_regions")
        .as_array()
        .unwrap()
        .clone();
    all.dedup_by(|a, b| a["page"] == b["page"]);
    Value::Array(all)
}

fn joined(pages: &[Value], field: &str) -> Value {
    Value::Array(
        pages
            .iter()
            .flat_map(|p| p[field].as_array().unwrap().clone())
            .collect(),
    )
}

#[tokio::test]
async fn read_range_and_find_answer_what_rest_answers() {
    let page = |p: u32| vec![format!("page {p} needle"), format!("then needle {p}")];
    let (_dir, state) = env(fixed((1..=3).map(page).collect(), vec![2], 1)).await;
    let doc = |more: Value| {
        let mut args = json!({ "document_id": DOC, "revision": 1 });
        args.as_object_mut()
            .unwrap()
            .extend(more.as_object().unwrap().clone());
        args
    };
    for (query, more) in [
        ("", json!({})),
        ("?limit=3", json!({ "limit": 3 })),
        (
            "?block=p2/b2&limit=2",
            json!({ "block": "p2/b2", "limit": 2 }),
        ),
    ] {
        let (status, want) = rest(&state, &text_path(query)).await;
        assert_eq!(status, StatusCode::OK, "{want}");
        let got = payload(&call(&state, "read_range", doc(more.clone())).await);
        assert_eq!(got, want, "{more}");
        if let Some(c) = want["next_cursor"].as_str() {
            let (_, want) = rest(&state, &text_path(&format!("?cursor={c}"))).await;
            let got = payload(&call(&state, "read_range", doc(json!({ "cursor": c }))).await);
            assert_eq!(got, want);
        }
    }
    for body in [
        json!({ "revision": 1, "query": "needle" }),
        json!({ "revision": 1, "query": "NEEDLE", "limit": 4 }),
    ] {
        let (status, want) = rest_find(&state, body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{want}");
        let mut args = body.clone();
        args["document_id"] = json!(DOC);
        assert_eq!(payload(&call(&state, "find", args.clone()).await), want);
        if let Some(c) = want["next_cursor"].as_str() {
            let mut body = body.clone();
            body["cursor"] = json!(c);
            let (_, want) = rest_find(&state, body).await;
            args["cursor"] = json!(c);
            assert_eq!(payload(&call(&state, "find", args).await), want);
        }
    }
    // Errors the service finds keep their own codes.
    for (op, args, code) in [
        ("read_range", doc(json!({ "revision": 2 })), "not_found"),
        (
            "find",
            json!({ "document_id": "doc_01K74Z3QJ8V5N2W9RTX6YB4M01", "revision": 1, "query": "a" }),
            "not_found",
        ),
        ("read_range", doc(json!({ "block": "p9/b9" })), "not_found"),
        (
            "read_range",
            doc(json!({ "cursor": "nope" })),
            "invalid_request",
        ),
        (
            "read_range",
            doc(json!({ "revision": 0 })),
            "invalid_request",
        ),
        (
            "find",
            doc(json!({ "query": "a", "cursor": "nope" })),
            "invalid_request",
        ),
        ("find", doc(json!({ "query": "  " })), "invalid_request"),
        (
            "read_range",
            doc(json!({ "require_complete": true })),
            "partial_parse",
        ),
        (
            "find",
            doc(json!({ "query": "a", "require_complete": true })),
            "partial_parse",
        ),
    ] {
        let body = call(&state, op, args).await;
        assert_module_error(&body, code, false);
    }
}

#[tokio::test]
async fn arguments_outside_the_read_schemas_are_invalid_input() {
    let (_dir, state) = env(fixed(vec![vec!["a".into()]], vec![], 1)).await;
    let read = |more: Value| {
        let mut args = json!({ "document_id": DOC, "revision": 1 });
        args.as_object_mut()
            .unwrap()
            .extend(more.as_object().unwrap().clone());
        args
    };
    let find = |more: Value| {
        let mut args = read(json!({ "query": "a" }));
        args.as_object_mut()
            .unwrap()
            .extend(more.as_object().unwrap().clone());
        args
    };
    for (op, args) in [
        ("read_range", json!({ "document_id": DOC })),
        ("read_range", json!({ "revision": 1 })),
        ("read_range", read(json!({ "revision": "1" }))),
        ("read_range", read(json!({ "extra": 1 }))),
        ("read_range", read(json!({ "query": "a" }))),
        ("read_range", read(json!({ "block": null }))),
        ("read_range", read(json!({ "cursor": null }))),
        ("read_range", read(json!({ "limit": null }))),
        ("read_range", read(json!({ "limit": 0 }))),
        ("read_range", read(json!({ "limit": 201 }))),
        ("read_range", read(json!({ "limit": 1.5 }))),
        ("read_range", read(json!({ "require_complete": null }))),
        ("read_range", read(json!({ "require_complete": "yes" }))),
        ("find", read(json!({}))),
        ("find", find(json!({ "query": null }))),
        ("find", find(json!({ "query": 7 }))),
        ("find", find(json!({ "block": "p1/b1" }))),
        ("find", find(json!({ "cursor": null }))),
        ("find", find(json!({ "limit": 0 }))),
        ("find", find(json!({ "limit": 201 }))),
        ("find", find(json!({ "require_complete": null }))),
        ("find", find(json!({ "document_id": null }))),
    ] {
        let body = call(&state, op, args.clone()).await;
        assert_eq!(
            parse_module_tool_envelope(&body),
            Err(ModuleToolCallError::Module {
                code: ModuleToolErrorCode::InvalidInput,
                retryable: false,
                details: None,
                unknown_code: None,
            }),
            "{op} {args}"
        );
    }
    // The bounds themselves pass.
    for limit in [1, 200] {
        payload(&call(&state, "read_range", read(json!({ "limit": limit }))).await);
        payload(&call(&state, "find", find(json!({ "limit": limit }))).await);
    }
}

#[tokio::test]
async fn without_an_engine_both_are_engine_unavailable_and_retryable() {
    let (_dir, state) = env(None).await;
    for (op, args) in [
        ("read_range", json!({ "document_id": DOC, "revision": 1 })),
        (
            "find",
            json!({ "document_id": DOC, "revision": 1, "query": "a" }),
        ),
    ] {
        let body = call(&state, op, args).await;
        assert_module_error(&body, "engine_unavailable", true);
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({ "kind": "error", "code": "engine_unavailable", "retryable": true })
        );
    }
}

#[tokio::test]
async fn a_large_document_pages_at_the_tool_budget_back_to_the_rest_answer() {
    // 10 pages of 20 blocks of about 1 KiB: one REST response, several
    // tool pages.
    let text = |p: usize, n: usize| format!("needle {p}/{n} {}", "lorem ipsum ".repeat(80));
    let doc: Vec<Vec<String>> = (1..=10)
        .map(|p| (1..=20).map(|n| text(p, n)).collect())
        .collect();
    let (_dir, state) = env(fixed(doc, vec![3, 7], 16)).await;
    let (status, want) = rest(&state, &text_path("?limit=200")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(want["next_cursor"].is_null());
    let args = json!({ "document_id": DOC, "revision": 1, "limit": 200 });
    let got = pages(&state, "read_range", args).await;
    assert!(got.len() >= 3, "{} pages", got.len());
    assert_eq!(joined(&got, "blocks"), want["blocks"]);
    assert_eq!(regions(&got), want["unparsed_regions"]);
    // Cut at the budget: one more block would not have fit.
    for page in &got[..got.len() - 1] {
        let size = serde_json::to_vec(page).unwrap().len();
        let one = serde_json::to_vec(&page["blocks"][0]).unwrap().len();
        let regions = serde_json::to_vec(&want["unparsed_regions"]).unwrap().len();
        assert!(size + one + regions > PAYLOAD_BYTES, "{size} bytes");
    }

    let body = json!({ "revision": 1, "query": "needle", "limit": 200 });
    let (status, want) = rest_find(&state, body).await;
    assert_eq!(status, StatusCode::OK);
    assert!(want["next_cursor"].is_null());
    assert_eq!(want["matches"].as_array().unwrap().len(), 200);
    let args = json!({ "document_id": DOC, "revision": 1, "query": "needle", "limit": 200 });
    let got = pages(&state, "find", args).await;
    assert!(got.len() >= 2, "{} pages", got.len());
    assert_eq!(joined(&got, "matches"), want["matches"]);
    assert_eq!(regions(&got), want["unparsed_regions"]);
}

#[tokio::test]
async fn a_block_that_escaping_blows_over_the_budget_is_payload_too_large() {
    // 16 KiB of a control character: within the layer's cap in UTF-8, six
    // times that once JSON-escaped.
    let wide = "\u{1}".repeat(crate::text_layer::MAX_BLOCK_BYTES);
    let (_dir, state) = env(fixed(vec![vec!["fine".into()], vec![wide]], vec![], 1)).await;
    // REST's 512 KiB holds it.
    let (status, want) = rest(&state, &text_path("")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(want["blocks"].as_array().unwrap().len(), 2);
    // The tool gives what fits, then refuses the block rather than cut it.
    let args = json!({ "document_id": DOC, "revision": 1 });
    let first = payload(&call(&state, "read_range", args.clone()).await);
    assert_eq!(first["blocks"], json!([want["blocks"][0]]));
    let mut next = args;
    next["cursor"] = first["next_cursor"].clone();
    let body = call(&state, "read_range", next).await;
    assert_module_error(&body, "payload_too_large", false);
    let at_block = json!({ "document_id": DOC, "revision": 1, "block": "p2/b1" });
    let body = call(&state, "read_range", at_block).await;
    assert_module_error(&body, "payload_too_large", false);

    // A match can get as wide (whitespace in the query matches a run of
    // any length: "a"+16000×U+000B+"b" for "a b"); here a tiny budget
    // stands in for one.
    let storage = state.storage().await.unwrap();
    let req = |limit| FindRequest {
        revision: 1,
        query: "fine".into(),
        cursor: None,
        limit: Some(limit),
        require_complete: false,
    };
    let e = find::search(&state, &storage, DOC, req(1), 256, Requester::Page)
        .await
        .err()
        .unwrap();
    assert_eq!(e.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_module_error(
        &serde_json::to_vec(&refused(&e)).unwrap(),
        "payload_too_large",
        false,
    );
    // The same search at the tool budget fits.
    assert!(
        find::search(
            &state,
            &storage,
            DOC,
            req(1),
            PAYLOAD_BYTES,
            Requester::Page
        )
        .await
        .is_ok()
    );
}

/// A match too wide for the budget on the first page searched is refused:
/// put back for the next response, it would come back empty with the
/// cursor where it was, every time (`first.page > start.page`, find.rs).
#[tokio::test]
async fn a_match_too_wide_on_the_first_page_is_payload_too_large() {
    let wide = format!("a{}b", "\u{b}".repeat(16000));
    let (_dir, state) = env(fixed(vec![vec![wide], vec!["a b".into()]], vec![], 1)).await;
    let (status, want) = rest_find(&state, json!({ "revision": 1, "query": "a b" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(want["matches"].as_array().unwrap().len(), 2);
    // One call, no cursor followed: a regression answers, it does not hang.
    let args = json!({ "document_id": DOC, "revision": 1, "query": "a b" });
    let body = call(&state, "find", args).await;
    assert_module_error(&body, "payload_too_large", false);
}

#[tokio::test]
async fn regions_over_half_the_budget_shorten_the_window_not_refuse_it() {
    // 100 pages, each with a block and a 16-rect unread region: about 120
    // KiB of regions, which one 60 KiB answer cannot cover.
    let doc: Vec<Vec<String>> = (1..=100).map(|p| vec![format!("page {p}")]).collect();
    let (_dir, state) = env(fixed(doc, (1..=100).collect(), 16)).await;
    let body = json!({ "revision": 1, "query": "absent" });
    let (status, want) = rest_find(&state, body).await;
    assert_eq!(status, StatusCode::OK);
    assert!(serde_json::to_vec(&want["unparsed_regions"]).unwrap().len() > PAYLOAD_BYTES);
    let args = json!({ "document_id": DOC, "revision": 1, "query": "absent" });
    let got = pages(&state, "find", args).await;
    assert!(got.len() >= 2, "{} pages", got.len());
    assert_eq!(joined(&got, "matches"), json!([]));
    assert_eq!(regions(&got), want["unparsed_regions"]);

    let (_, want) = rest(&state, &text_path("?limit=200")).await;
    let args = json!({ "document_id": DOC, "revision": 1, "limit": 200 });
    let got = pages(&state, "read_range", args).await;
    assert_eq!(joined(&got, "blocks"), want["blocks"]);
    assert_eq!(regions(&got), want["unparsed_regions"]);
}

/// Pages 2 to 40 unread (a 16-rect region each): with a first entry of
/// about 36 KiB, more than the window's regions leave room for.
fn after_a_wide_first_page(first: String, others: impl Fn(u32) -> Vec<String>) -> Option<Fixed> {
    let doc = std::iter::once(vec![first]).chain((2..=40).map(others));
    fixed(doc.collect(), (2..=40).collect(), 16)
}

#[tokio::test]
async fn a_wide_first_block_ends_its_answer_at_its_page() {
    // 6000 bytes of a control character: 36 KB once JSON-escaped.
    let wide = "\u{1}".repeat(6000);
    let (_dir, state) = env(after_a_wide_first_page(wide, |_| vec![])).await;
    let (status, want) = rest(&state, &text_path("")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(want["next_cursor"].is_null());
    let args = json!({ "document_id": DOC, "revision": 1 });
    let got = pages(&state, "read_range", args).await;
    // Page 1 alone, then the regions from page 2 on.
    assert_eq!(got[0]["blocks"], json!([want["blocks"][0]]));
    assert_eq!(got[0]["parse_status"], json!("complete"));
    assert_eq!(got[0]["unparsed_regions"], json!([]));
    assert!(got.len() >= 2, "{} pages", got.len());
    assert_eq!(joined(&got, "blocks"), want["blocks"]);
    assert_eq!(regions(&got), want["unparsed_regions"]);
}

#[tokio::test]
async fn a_wide_first_match_ends_its_answer_at_its_page() {
    // "a b" matches across 6000 vertical tabs: a quote of 36 KB escaped.
    let wide = format!("a{}b", "\u{b}".repeat(6000));
    // Even pages have a block that does not match; odd ones none.
    let other = |p: u32| {
        if p.is_multiple_of(2) {
            vec![format!("no match on {p}")]
        } else {
            vec![]
        }
    };
    let (_dir, state) = env(after_a_wide_first_page(wide, other)).await;
    let body = json!({ "revision": 1, "query": "a b" });
    let (status, want) = rest_find(&state, body).await;
    assert_eq!(status, StatusCode::OK);
    assert!(want["next_cursor"].is_null());
    assert_eq!(want["matches"].as_array().unwrap().len(), 1);
    let args = json!({ "document_id": DOC, "revision": 1, "query": "a b" });
    let got = pages(&state, "find", args).await;
    assert_eq!(got[0]["matches"], want["matches"]);
    assert_eq!(got[0]["parse_status"], json!("complete"));
    assert_eq!(got[0]["unparsed_regions"], json!([]));
    assert!(got.len() >= 2, "{} pages", got.len());
    assert_eq!(joined(&got, "matches"), want["matches"]);
    assert_eq!(regions(&got), want["unparsed_regions"]);
}

/// Pages 1 to 20 unread (a 16-rect region each) and without a block, then
/// `wide` on page 21 and `"a b tail"` on 22: the regions before the wide
/// entry and the entry do not fit one answer, each alone does.
fn before_a_wide_page(wide: String) -> Option<Fixed> {
    let doc = (1..=20)
        .map(|_| vec![])
        .chain([vec![wide], vec!["a b tail".into()]]);
    fixed(doc.collect(), (1..=20).collect(), 16)
}

#[tokio::test]
async fn regions_before_a_wide_first_block_go_ahead_of_it() {
    let wide = format!("a{}b", "\u{b}".repeat(6000));
    let (_dir, state) = env(before_a_wide_page(wide)).await;
    let (status, want) = rest(&state, &text_path("")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(want["next_cursor"].is_null());
    let args = json!({ "document_id": DOC, "revision": 1 });
    let got = pages(&state, "read_range", args).await;
    // Pages 1 to 20's regions alone, the cursor at the wide block.
    assert_eq!(got[0]["blocks"], json!([]));
    assert_eq!(got[0]["unparsed_regions"], want["unparsed_regions"]);
    assert!(got.len() >= 2, "{} pages", got.len());
    assert_eq!(got[1]["blocks"][0], want["blocks"][0]);
    assert_eq!(joined(&got, "blocks"), want["blocks"]);
    assert_eq!(regions(&got), want["unparsed_regions"]);
}

#[tokio::test]
async fn regions_before_a_wide_first_match_go_ahead_of_it() {
    let wide = format!("a{}b", "\u{b}".repeat(6000));
    let (_dir, state) = env(before_a_wide_page(wide)).await;
    let body = json!({ "revision": 1, "query": "a b" });
    let (status, want) = rest_find(&state, body).await;
    assert_eq!(status, StatusCode::OK);
    assert!(want["next_cursor"].is_null());
    assert_eq!(want["matches"].as_array().unwrap().len(), 2);
    let args = json!({ "document_id": DOC, "revision": 1, "query": "a b" });
    let got = pages(&state, "find", args).await;
    assert_eq!(got[0]["matches"], json!([]));
    assert_eq!(got[0]["unparsed_regions"], want["unparsed_regions"]);
    assert!(got.len() >= 2, "{} pages", got.len());
    assert_eq!(got[1]["matches"][0], want["matches"][0]);
    assert_eq!(joined(&got, "matches"), want["matches"]);
    assert_eq!(regions(&got), want["unparsed_regions"]);
}

#[tokio::test]
async fn the_list_service_holds_its_own_limit_range() {
    let (_dir, state) = opened().await;
    let storage = state.storage().await.unwrap();
    for limit in [0, -1, 201] {
        let e = documents::list(&storage, None, None, limit, PAYLOAD_BYTES)
            .await
            .err()
            .unwrap();
        assert_eq!(
            (e.code(), e.status()),
            ("invalid_request", StatusCode::BAD_REQUEST)
        );
    }
    for limit in [1, 200] {
        assert!(
            documents::list(&storage, None, None, limit, PAYLOAD_BYTES)
                .await
                .is_ok()
        );
    }
}

/// An engine whose parse never ends.
struct Stuck;

impl Engine for Stuck {
    fn engine(&self) -> EngineRef {
        EngineRef {
            id: "apple-pdfkit".into(),
            version: "26.6.2".into(),
        }
    }
    fn config(&self) -> Value {
        json!({ "blocks": "test" })
    }
    fn parse(&self, _content: &str, _media: &str, _path: &Path) -> Parse {
        Box::pin(std::future::pending())
    }
}

/// Stops the build `by` asked for, once there is one: it was asked for by
/// `by` if this ends.
async fn stop_once_asked(state: &AppState, by: &Requester) {
    let start = std::time::Instant::now();
    while state.layers().stop(|r| r == by).is_empty() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "no build by {by:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// K1-06a-grant-path §2.5: a read's build is asked for by its tool's
/// operation and grant, or by the page; stopped, the read is retryable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_s_build_is_asked_for_by_its_tool_or_its_page() {
    let (_dir, state) = env_with(Some(Arc::new(Stuck))).await;
    for (op, args) in [
        ("read_range", json!({ "document_id": DOC, "revision": 1 })),
        (
            "find",
            json!({ "document_id": DOC, "revision": 1, "query": "a" }),
        ),
    ] {
        let read = tokio::spawn({
            let state = state.clone();
            async move { call(&state, op, args).await }
        });
        // Asked for under the call's grant (K1-06a §2.5.1).
        let by = crate::authorization::Accepted {
            operation: op.to_owned(),
            authorization_ref: Some("auth-1".to_owned()),
        };
        stop_once_asked(&state, &Requester::Tool(by)).await;
        assert_module_error(&read.await.unwrap(), "engine_unavailable", true);
    }
    let page = tokio::spawn({
        let state = state.clone();
        async move {
            let req = Request::get(text_path("")).body(Body::empty()).unwrap();
            crate::router(state).oneshot(req).await.unwrap().status()
        }
    });
    stop_once_asked(&state, &Requester::Page).await;
    assert_eq!(page.await.unwrap(), StatusCode::SERVICE_UNAVAILABLE);
}

/// K1-06a-grant-path §2.5: a read whose authorization ends while it runs
/// answers `permission_denied`, and the notice is answered once it has.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_its_authorization_ends_under_gives_nothing() {
    let (_dir, state) = env_with(Some(Arc::new(Stuck))).await;
    let read = tokio::spawn({
        let state = state.clone();
        async move {
            let args = json!({ "document_id": DOC, "revision": 1, "query": "a" });
            call(&state, "find", args).await
        }
    });
    let start = std::time::Instant::now();
    while state.calls().in_flight() == 0 {
        assert!(start.elapsed() < Duration::from_secs(5), "never began");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let wait = Duration::from_secs(5);
    assert!(
        crate::authorization::stop(
            &state,
            &crate::authorization::Ended::by_operation("find"),
            wait
        )
        .await
    );
    let answer = tokio::time::timeout(Duration::from_secs(2), read).await;
    let answer = answer.expect("the call goes on").unwrap();
    assert_kernel_error(&answer, ModuleToolErrorCode::PermissionDenied);
}

/// [`Fixed`], answering only once `open` is notified.
struct Gated {
    inner: Fixed,
    open: Arc<tokio::sync::Notify>,
}

impl Engine for Gated {
    fn engine(&self) -> EngineRef {
        self.inner.engine()
    }
    fn config(&self) -> Value {
        self.inner.config()
    }
    fn parse(&self, content: &str, media: &str, path: &Path) -> Parse {
        let parse = self.inner.parse(content, media, path);
        let open = Arc::clone(&self.open);
        Box::pin(async move {
            open.notified().await;
            parse.await
        })
    }
}

/// #1206 B2: a read that would find its text, overtaken by its
/// authorization's end before it answers, answers `permission_denied` and
/// gives none of it. A call begun after the end is not overtaken (#1206
/// B1). Ended by its grant's reference (A5c-3), a late call under that
/// grant is refused on arrival; one under another grant answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_that_would_succeed_gives_nothing_once_its_authorization_ends() {
    let open = Arc::new(tokio::sync::Notify::new());
    let inner = fixed(vec![vec!["the needle".into()]], vec![], 0).unwrap();
    let engine = Gated {
        inner,
        open: Arc::clone(&open),
    };
    let (_dir, state) = env_with(Some(Arc::new(engine))).await;
    let find = |state: AppState| async move {
        let args = json!({ "document_id": DOC, "revision": 1, "query": "needle" });
        call(&state, "find", args).await
    };
    let read = tokio::spawn(find(state.clone()));
    let start = std::time::Instant::now();
    while state.calls().in_flight() == 0 {
        assert!(start.elapsed() < Duration::from_secs(5), "never began");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    state
        .calls()
        .end(&crate::authorization::Ended::by_operation("find"));
    open.notify_one();
    let answer = tokio::time::timeout(Duration::from_secs(5), read).await;
    let answer = answer.expect("the read answers").unwrap();
    assert_kernel_error(&answer, ModuleToolErrorCode::PermissionDenied);
    let quote = r#""quote":"needle""#;
    assert!(!String::from_utf8_lossy(&answer).contains("needle"));
    // Begun after the end, the same read answers with its text.
    let after = find(state.clone()).await;
    let after = String::from_utf8_lossy(&after).into_owned();
    assert!(after.contains(quote), "{after}");
    let ended = crate::authorization::Ended {
        operation: "find".into(),
        refs: Some(["auth-1".to_owned()].into()),
    };
    state.calls().end(&ended);
    assert_kernel_error(
        &find(state.clone()).await,
        ModuleToolErrorCode::PermissionDenied,
    );
    assert_eq!(state.calls().in_flight(), 0);
    let mut context = context_with("find", &[DOC]);
    context["authorization_ref"] = json!("auth-2");
    let args = json!({ "document_id": DOC, "revision": 1, "query": "needle" });
    let body = json!({ "context": context, "arguments": args });
    let other = post_to(&state, "find", body.to_string()).await;
    let other = String::from_utf8_lossy(&other).into_owned();
    assert!(other.contains(quote), "{other}");
}
