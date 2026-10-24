#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use agent24_os_sdk::{ClientError, CompleteRequest, CompleteResult, ServedTier, Usage};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::*;
use crate::engine::{Engine, EngineError, Layers, Parse};
use crate::events::Events;
use crate::extract::run::{Call, Model};
use crate::text_layer::{Block, EngineRef, Line, TextLayer};

pub(crate) const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M00";

/// A text engine that reads one block, "停水 10月12日".
pub(crate) struct OneBlock;

impl Engine for OneBlock {
    fn engine(&self) -> EngineRef {
        EngineRef {
            id: "apple-pdfkit".into(),
            version: "26.6.2".into(),
        }
    }
    fn config(&self) -> Value {
        json!({ "test": 1 })
    }
    fn parse(&self, content: &str, _media: &str, _path: &Path) -> Parse {
        let (engine, config, content) = (self.engine(), self.config(), content.to_owned());
        Box::pin(async move {
            let text = "停水 10月12日".to_owned();
            Ok::<_, EngineError>(TextLayer {
                v: 1,
                content_sha256: content,
                engine,
                config,
                pages: 1,
                parse_status: ParseStatus::Complete,
                unparsed_regions: vec![],
                blocks: vec![Block {
                    block_id: "p1/b1".into(),
                    page: 1,
                    lines: vec![Line {
                        start: 0,
                        end: text.len(),
                        rect: [72.0, 100.0, 300.0, 112.0],
                    }],
                    text,
                }],
            })
        })
    }
}

/// A model that answers from a script; it can mark the job cancelling
/// before it answers.
pub(crate) struct Script {
    pub(crate) replies: Mutex<VecDeque<Result<CompleteResult, ClientError>>>,
    pub(crate) cancel: Mutex<Option<(Arc<Storage>, String)>>,
}

impl Model for Script {
    fn complete(&self, _req: CompleteRequest) -> Call {
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("a call past the script");
        let cancel = self.cancel.lock().unwrap().take();
        Box::pin(async move {
            if let Some((storage, id)) = cancel {
                sqlx::query("UPDATE jobs SET status = 'cancelling' WHERE id = ?")
                    .bind(id)
                    .execute(storage.db.pool())
                    .await
                    .unwrap();
            }
            reply
        })
    }
}

pub(crate) fn answer(status: &str) -> Result<CompleteResult, ClientError> {
    let value = match status {
        "present" => {
            json!({ "present": { "value": "10月12日", "normalized": "2026-10-12",
                                 "evidence": [{ "block_id": "p1/b1", "quote": "停水 10月12日" }] } })
        }
        _ => json!({ "missing": { "reason": "not_in_document" } }),
    };
    Ok(CompleteResult {
        text: json!({ "values": { "date": value } }).to_string(),
        model_id: Some("qwen3-8b".into()),
        tier: ServedTier::Local,
        usage: Usage {
            prompt_tokens: 1,
            completion_tokens: 1,
        },
    })
}

pub(crate) struct Env {
    pub(crate) _dir: tempfile::TempDir,
    pub(crate) state: AppState,
    pub(crate) storage: Arc<Storage>,
}

/// A document with r1, a text engine, and (if given) a model.
pub(crate) async fn env(model: Option<Arc<Script>>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open_serving(
        dir.path(),
        Events::default(),
        Layers::new(Some(Arc::new(OneBlock))),
    )
    .await;
    if let Some(m) = model {
        state.set_model(m);
    }
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
    Env {
        _dir: dir,
        state,
        storage,
    }
}

pub(crate) fn script(replies: Vec<Result<CompleteResult, ClientError>>) -> Arc<Script> {
    Arc::new(Script {
        replies: Mutex::new(replies.into()),
        cancel: Mutex::default(),
    })
}

pub(crate) fn input() -> Value {
    let fields = vec![Field {
        key: "date".into(),
        description: "停水日期".into(),
        kind: Some("date".into()),
    }];
    json!({ "document_id": DOC, "revision": 1, "schema_sha256": schema_sha256(&fields).unwrap(), "fields": fields })
}

/// A queued extract job with `input`, its worker started.
pub(crate) async fn start(env: &Env, input: &Value) -> String {
    let id = new_id(IdKind::Job).unwrap();
    sqlx::query(
        "INSERT INTO jobs (id, kind, document_id, revision, status, origin, input)
         VALUES (?, 'extract', ?, 1, 'queued', '{\"kind\":\"page\"}', ?)",
    )
    .bind(&id)
    .bind(DOC)
    .bind(input.to_string())
    .execute(env.storage.db.pool())
    .await
    .unwrap();
    spawn(env.state.clone(), env.storage.clone(), id.clone(), 1);
    id
}

/// The job once it has ended: status, error code, result.
pub(crate) async fn ended(env: &Env, id: &str) -> (String, Option<String>, Option<String>) {
    let start = Instant::now();
    loop {
        let row: (String, Option<String>, Option<String>) =
            sqlx::query_as("SELECT status, error ->> '$.code', result_ref FROM jobs WHERE id = ?")
                .bind(id)
                .fetch_one(env.storage.db.pool())
                .await
                .unwrap();
        if !["queued", "running", "cancelling"].contains(&row.0.as_str()) {
            let job = crate::jobs::load(env.storage.db.pool(), id).await;
            assert!(matches!(job, Ok(Some(_))), "{id}: {job:?}");
            return row;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{id} never ended: {row:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_writes_its_extraction_with_its_success() {
    let env = env(Some(script(vec![answer("present")]))).await;
    let id = start(&env, &input()).await;
    let (status, code, result) = ended(&env, &id).await;
    assert_eq!((status.as_str(), code), ("succeeded", None));
    let ext = result.unwrap();
    let res = crate::router(env.state.clone())
        .oneshot(
            Request::get(format!("/extractions/{ext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        (
            &body["model_id"],
            &body["schema_sha256"],
            &body["extractor_version"]
        ),
        (
            &json!("qwen3-8b"),
            &input()["schema_sha256"],
            &json!(crate::extract::EXTRACTOR_VERSION)
        )
    );
    let v = &body["values"][0];
    assert_eq!(
        (&v["key"], &v["status"], &v["value"]),
        (&json!("date"), &json!("present"), &json!("10月12日"))
    );
    assert_eq!(v["anchors"][0]["quote"], "停水 10月12日");
    // The job's result names it, as GET /jobs reads it.
    let job = crate::jobs::load(env.storage.db.pool(), &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&job).unwrap()["result"],
        json!({ "extraction_id": ext })
    );
}

/// A model (or none), the job's input, and the status and code it ends with.
type Case = (
    Option<Arc<Script>>,
    Value,
    &'static str,
    Option<&'static str>,
);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_way_a_job_cannot_finish_ends_it_with_its_code() {
    let cases: Vec<Case> = vec![
        (None, input(), "failed", Some("engine_unavailable")),
        (
            Some(script(vec![])),
            {
                let mut i = input();
                i["schema_sha256"] = json!(format!("sha256:{}", "0".repeat(64)));
                i
            },
            "failed",
            Some("invalid_request"),
        ),
        (
            Some(script(vec![])),
            json!({ "upload_id": "x" }),
            "failed",
            Some("invalid_request"),
        ),
        (
            Some(script(vec![Err(ClientError::Forbidden("f".into()))])),
            input(),
            "failed",
            Some("engine_unavailable"),
        ),
        (
            // Malformed free, then held twice (ME4-S2c).
            Some(script(
                (0..3)
                    .map(|_| {
                        answer("x").map(|mut r| {
                            r.text = "{}".into();
                            r
                        })
                    })
                    .collect(),
            )),
            input(),
            "failed",
            Some("parse_failed"),
        ),
        (
            Some(script(vec![Err(ClientError::Cancelled)])),
            input(),
            "interrupted",
            None,
        ),
    ];
    for (model, input, want, code) in cases {
        let env = env(model).await;
        let id = start(&env, &input).await;
        let (status, got, result) = ended(&env, &id).await;
        assert_eq!(
            (status.as_str(), got.as_deref(), result),
            (want, code, None),
            "{input}"
        );
    }
    // A revision that is not there.
    let env = env(Some(script(vec![]))).await;
    let mut i = input();
    i["revision"] = json!(2);
    let id = start(&env, &i).await;
    assert_eq!(ended(&env, &id).await.1.as_deref(), Some("not_found"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_cancelled_while_the_model_works_ends_cancelled_with_nothing_written() {
    let model = script(vec![answer("present")]);
    let env = env(Some(model.clone())).await;
    let i = input();
    // Mark it cancelling from inside the model's call.
    let id = new_id(IdKind::Job).unwrap();
    *model.cancel.lock().unwrap() = Some((env.storage.clone(), id.clone()));
    sqlx::query(
        "INSERT INTO jobs (id, kind, document_id, revision, status, origin, input)
         VALUES (?, 'extract', ?, 1, 'queued', '{\"kind\":\"page\"}', ?)",
    )
    .bind(&id)
    .bind(DOC)
    .bind(i.to_string())
    .execute(env.storage.db.pool())
    .await
    .unwrap();
    spawn(env.state.clone(), env.storage.clone(), id.clone(), 1);
    assert_eq!(
        ended(&env, &id).await,
        ("cancelled".into(), Some("cancelled".into()), None)
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM extractions")
        .fetch_one(env.storage.db.pool())
        .await
        .unwrap();
    assert_eq!(n, 0);
}

/// A text engine that never finishes reading.
struct Stuck;

impl Engine for Stuck {
    fn engine(&self) -> EngineRef {
        EngineRef {
            id: "apple-pdfkit".into(),
            version: "26.6.2".into(),
        }
    }
    fn config(&self) -> Value {
        json!({ "test": 2 })
    }
    fn parse(&self, _content: &str, _media: &str, _path: &Path) -> Parse {
        Box::pin(std::future::pending())
    }
}

/// Codex: a job waiting minutes for its layer still sees its cancel within
/// a second or so, and calls no model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_waiting_for_its_layer_sees_its_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open_serving(
        dir.path(),
        Events::default(),
        Layers::new(Some(Arc::new(Stuck))),
    )
    .await;
    state.set_model(script(vec![]));
    let storage = state.storage().await.unwrap();
    let env = Env {
        _dir: dir,
        state,
        storage,
    };
    let blob = env.storage.blobs.put_bytes(b"%PDF-1.7 stuck").unwrap();
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
        sqlx::query(&sql)
            .execute(env.storage.db.pool())
            .await
            .unwrap();
    }
    let id = start(&env, &input()).await;
    let begun = Instant::now();
    while sqlx::query_scalar::<_, String>("SELECT status FROM jobs WHERE id = ?")
        .bind(&id)
        .fetch_one(env.storage.db.pool())
        .await
        .unwrap()
        != "running"
    {
        assert!(begun.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    sqlx::query("UPDATE jobs SET status = 'cancelling' WHERE id = ?")
        .bind(&id)
        .execute(env.storage.db.pool())
        .await
        .unwrap();
    let cancelled = Instant::now();
    assert_eq!(
        ended(&env, &id).await,
        ("cancelled".into(), Some("cancelled".into()), None)
    );
    assert!(
        cancelled.elapsed() < LAYER_POLL * 3,
        "{:?}",
        cancelled.elapsed()
    );
    // The build goes on, asked for by this attempt: what a revocation stops.
    let this = Requester::Job { id, attempt: 1 };
    assert_eq!(env.state.layers().stop(|r| *r == this).len(), 1);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retried_extract_job_runs_again() {
    let model = script(vec![
        Err(ClientError::Forbidden("f".into())),
        answer("missing"),
    ]);
    let env = env(Some(model)).await;
    let id = start(&env, &input()).await;
    assert_eq!(ended(&env, &id).await.0, "failed");
    let res = crate::router(env.state.clone())
        .oneshot(
            Request::post(format!("/jobs/{id}/retry"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let (status, _, result) = ended(&env, &id).await;
    assert_eq!(status, "succeeded");
    assert!(result.is_some());
}
