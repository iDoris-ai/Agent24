#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::engine::{
    Engine, EngineError, Layers, Parse, Render, RenderAsk, RenderError, Rendered, WAIT,
};
use crate::events::Events;
use crate::state::AppState;
use crate::text_layer::EngineRef;

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nIDAT";
const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// A stand-in engine that renders as told, after `delay`, and keeps what it
/// was asked.
struct Painter {
    answer: Result<Rendered, RenderError>,
    delay: Duration,
    asked: Mutex<Vec<(String, PathBuf, RenderAsk)>>,
}

impl Engine for Painter {
    fn engine(&self) -> EngineRef {
        EngineRef {
            id: "apple-pdfkit".into(),
            version: "26.6.2".into(),
        }
    }
    fn config(&self) -> Value {
        json!({})
    }
    fn parse(&self, _content: &str, _media: &str, _path: &Path) -> Parse {
        Box::pin(async { Err(EngineError::Unsupported) })
    }
    fn render(&self, media: &str, path: &Path, ask: RenderAsk) -> Render {
        self.asked
            .lock()
            .unwrap()
            .push((media.to_owned(), path.to_owned(), ask));
        let (answer, delay) = (self.answer.clone(), self.delay);
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            answer
        })
    }
}

struct Env {
    _dir: tempfile::TempDir,
    state: AppState,
    painter: Arc<Painter>,
    blob: PathBuf,
}

fn ok() -> Result<Rendered, RenderError> {
    Ok(Rendered {
        scale: 0.5,
        png: PNG.to_vec(),
    })
}

/// A document with r1 (`media`), and a painter answering `answer` (or no
/// engine at all).
async fn env(answer: Option<Result<Rendered, RenderError>>, media: &str) -> Env {
    env_after(answer, Duration::ZERO, media).await
}

/// The same, with a painter that takes `delay` to answer.
async fn env_after(
    answer: Option<Result<Rendered, RenderError>>,
    delay: Duration,
    media: &str,
) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let painter = Arc::new(Painter {
        answer: answer.clone().unwrap_or_else(ok),
        delay,
        asked: Mutex::default(),
    });
    let layers = Layers::new(answer.map(|_| painter.clone() as Arc<dyn Engine>));
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
    let blob = storage.blobs.path_of(&blob.sha256).unwrap();
    Env {
        _dir: dir,
        state,
        painter,
        blob,
    }
}

struct Got {
    status: StatusCode,
    kind: Option<String>,
    scale: Option<String>,
    body: Vec<u8>,
}

impl Got {
    fn error(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

async fn get(state: &AppState, path: &str) -> Got {
    let res = crate::router(state.clone())
        .oneshot(
            Request::get(format!("/documents/{path}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (kind, scale) = {
        let header = |name: &str| {
            res.headers()
                .get(name)
                .map(|v| v.to_str().unwrap().to_owned())
        };
        (header("content-type"), header("documents-render-scale"))
    };
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 1 << 21)
        .await
        .unwrap()
        .to_vec();
    Got {
        status,
        kind,
        scale,
        body,
    }
}

fn page(n: &str, query: &str) -> String {
    format!("{DOC}/revisions/1/pages/{n}{query}")
}

#[tokio::test]
async fn a_page_is_a_png_with_the_scale_used() {
    let e = env(Some(ok()), "application/pdf").await;
    let got = get(&e.state, &page("2", "?scale=1.5&region=10,20.5,100,200")).await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.kind.as_deref(), Some("image/png"));
    assert_eq!(got.scale.as_deref(), Some("0.5"));
    assert_eq!(got.body, PNG);
    let ask = RenderAsk {
        page: 2,
        scale: 1.5,
        region: Some([10.0, 20.5, 100.0, 200.0]),
        max_bytes: 1 << 20,
    };
    assert_eq!(
        e.painter.asked.lock().unwrap().clone(),
        [("application/pdf".to_owned(), e.blob.clone(), ask)]
    );
    // Scale 1 and the whole page unless asked otherwise.
    get(&e.state, &page("1", "")).await;
    let asked = e.painter.asked.lock().unwrap()[1].2;
    assert_eq!((asked.page, asked.scale, asked.region), (1, 1.0, None));
}

#[tokio::test]
async fn bad_requests_are_refused_before_the_engine() {
    let e = env(Some(ok()), "application/pdf").await;
    for query in [
        page("0", ""),
        page("x", ""),
        page("1", "?scale=0.24"),
        page("1", "?scale=4.01"),
        page("1", "?scale=NaN"),
        page("1", "?scale=big"),
        page("1", "?zoom=2"),
        page("1", "?region=1,2,3"),
        page("1", "?region=1,2,3,4,5"),
        page("1", "?region=-1,0,10,10"),
        page("1", "?region=1e2,0,300,10"),
        page("1", "?region=.5,0,1,1"),
        page("1", "?region=1.,0,2,2"),
        page("1", "?region=10,0,5,10"),
        page("1", "?region=0,5,10,5"),
        "doc_x/revisions/1/pages/1".to_owned(),
    ] {
        let got = get(&e.state, &query).await;
        assert_eq!(got.status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(got.error()["error"]["code"], "invalid_request", "{query}");
    }
    assert!(e.painter.asked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn what_cannot_be_rendered_says_why() {
    let e = env(Some(ok()), "application/pdf").await;
    let missing = get(&e.state, &format!("{DOC}/revisions/2/pages/1")).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let past = get(&e.state, &page("4294967296", "")).await;
    assert_eq!(past.status, StatusCode::NOT_FOUND);
    // A flowing format has no pages, engine or not.
    for answer in [Some(ok()), None] {
        let docx = env(answer, DOCX).await;
        let got = get(&docx.state, &page("1", "")).await;
        assert_eq!(got.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(got.error()["error"]["code"], "unsupported_format");
    }
    let none = env(None, "application/pdf").await;
    let got = get(&none.state, &page("1", "")).await;
    assert_eq!(got.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(got.error()["error"]["code"], "engine_unavailable");
    assert!(e.painter.asked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn each_render_failure_has_its_answer() {
    for (failure, status, code) in [
        (
            RenderError::Unsupported,
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_format",
        ),
        (RenderError::NoPage, StatusCode::NOT_FOUND, "not_found"),
        (
            RenderError::OffPage,
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            RenderError::TooLarge,
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
        ),
        (
            RenderError::TooSlow,
            StatusCode::SERVICE_UNAVAILABLE,
            "engine_unavailable",
        ),
        (
            RenderError::Failed("cannot open".into()),
            StatusCode::UNPROCESSABLE_ENTITY,
            "parse_failed",
        ),
    ] {
        let e = env(Some(Err(failure.clone())), "application/pdf").await;
        let got = get(&e.state, &page("1", "")).await;
        assert_eq!(
            (got.status, got.error()["error"]["code"].clone()),
            (status, json!(code)),
            "{failure:?}"
        );
        assert_eq!(got.scale, None);
    }
}

/// Waiting for a slot and rendering share one deadline from the request,
/// so a client hears within the kernel proxy's 10 s: here 3 s waiting and a
/// render that would take a minute end at 8 s, not 11.
#[tokio::test]
async fn a_wait_and_a_slow_render_share_one_deadline() {
    let e = env_after(Some(ok()), Duration::from_secs(60), "application/pdf").await;
    let far = tokio::time::Instant::now() + Duration::from_secs(60);
    let layers = e.state.layers().clone();
    let held = (layers.render_slot(far).await, layers.render_slot(far).await);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        drop(held);
    });
    let start = std::time::Instant::now();
    let got = get(&e.state, &page("1", "")).await;
    let elapsed = start.elapsed();
    assert!(
        elapsed >= WAIT && elapsed < WAIT + Duration::from_secs(2),
        "{elapsed:?}"
    );
    assert_eq!(got.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(got.error()["error"]["code"], "engine_unavailable");
    assert_eq!(got.error()["error"]["message"], "the render took too long");
    assert_eq!(e.painter.asked.lock().unwrap().len(), 1);
}

/// With both slots taken, renders wait in a line of 16 until the deadline;
/// one more is turned away at once.
#[tokio::test]
async fn a_busy_engine_is_unavailable_and_its_line_is_bounded() {
    let e = env(Some(ok()), "application/pdf").await;
    let far = tokio::time::Instant::now() + Duration::from_secs(60);
    let layers = e.state.layers();
    let _held = (layers.render_slot(far).await, layers.render_slot(far).await);
    let start = std::time::Instant::now();
    let waiting: Vec<_> = (0..crate::engine::QUEUE)
        .map(|_| {
            let state = e.state.clone();
            tokio::spawn(async move { get(&state, &page("1", "")).await })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let turned_away = get(&e.state, &page("1", "")).await;
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(turned_away.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        turned_away.error()["error"]["message"],
        "the render engine is busy"
    );
    for w in waiting {
        let got = w.await.unwrap();
        assert_eq!(got.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(got.error()["error"]["message"], "the render engine is busy");
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed >= WAIT && elapsed < WAIT + Duration::from_secs(2),
        "{elapsed:?}"
    );
    assert!(e.painter.asked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn capabilities_route_render_when_the_engine_is_here() {
    for (answer, state) in [(Some(ok()), "ready"), (None, "absent")] {
        let e = env(answer, "application/pdf").await;
        let caps = serde_json::to_value(crate::capabilities(&e.state).await).unwrap();
        let render = caps["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["op"] == "render")
            .unwrap();
        assert_eq!(render["available"], state == "ready");
        let engine = caps["engines"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "render")
            .unwrap();
        assert_eq!(engine["state"], state);
    }
}

/// Storage is had within the request's deadline too; storage that cannot
/// be had is 503 with its cause.
#[tokio::test]
async fn storage_that_failed_is_unavailable() {
    let state = AppState::unavailable(crate::error::StorageCause::Corrupt);
    let got = get(&state, &page("1", "")).await;
    assert_eq!(got.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(got.error()["error"]["code"], "storage_unavailable");
}
