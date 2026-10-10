#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::Request;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::extract::job::tests::{DOC, Env, answer, ended, env, script};

const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

fn body(rerun: bool) -> Value {
    let mut b = json!({ "revision": 1, "schema": { "fields": [
        { "key": "date", "description": "停水日期", "type": "date" } ] } });
    if rerun {
        b["rerun"] = json!(true);
    }
    b
}

async fn post(env: &Env, doc: &str, body: &Value, key: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::post(format!("/documents/{doc}/extractions"))
        .header("content-type", "application/json");
    if let Some(k) = key {
        req = req.header("idempotency-key", k);
    }
    let res = crate::router(env.state.clone())
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_starts_a_job_and_the_same_request_replays_it() {
    let env = env(Some(script(vec![answer("present")]))).await;
    let (status, job) = post(&env, DOC, &body(false), None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{job}");
    assert_eq!(
        (&job["kind"], &job["document_id"], &job["revision"]),
        (&json!("extract"), &json!(DOC), &json!(1))
    );
    let id = job["job_id"].as_str().unwrap().to_owned();
    assert_eq!(ended(&env, &id).await.0, "succeeded");
    // The same request: the same job, done, 200 with its result.
    let (status, again) = post(&env, DOC, &body(false), None).await;
    assert_eq!(
        (status, &again["job_id"], &again["status"]),
        (StatusCode::OK, &json!(id), &json!("succeeded"))
    );
    assert!(again["result"]["extraction_id"].is_string());
    // An Idempotency-Key without rerun is ignored, even a malformed one.
    let (status, ignored) = post(&env, DOC, &body(false), Some("not a key")).await;
    assert_eq!((status, &ignored["job_id"]), (StatusCode::OK, &json!(id)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rerun_is_a_new_job_under_its_own_key() {
    let env = env(Some(script(vec![answer("present"), answer("missing")]))).await;
    let (_, first) = post(&env, DOC, &body(false), None).await;
    ended(&env, first["job_id"].as_str().unwrap()).await;
    let (status, err) = post(&env, DOC, &body(true), None).await;
    assert_eq!(
        (status, &err["error"]["code"]),
        (StatusCode::BAD_REQUEST, &json!("invalid_request"))
    );
    let (status, rerun) = post(&env, DOC, &body(true), Some("rerun-1")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_ne!(rerun["job_id"], first["job_id"]);
    let rid = rerun["job_id"].as_str().unwrap().to_owned();
    assert_eq!(ended(&env, &rid).await.0, "succeeded");
    // The same rerun again replays it; the same key with another body is 422.
    let (status, again) = post(&env, DOC, &body(true), Some("rerun-1")).await;
    assert_eq!((status, &again["job_id"]), (StatusCode::OK, &json!(rid)));
    let mut other = body(true);
    other["schema"]["fields"][0]["description"] = json!("另一个说明");
    let (status, err) = post(&env, DOC, &other, Some("rerun-1")).await;
    assert_eq!(
        (status, &err["error"]["code"]),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            &json!("idempotency_key_reused")
        )
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_job_is_queued_again_by_its_key() {
    let env = env(Some(script(vec![
        Err(agent24_os_sdk::ClientError::Forbidden("f".into())),
        answer("missing"),
    ])))
    .await;
    let (_, job) = post(&env, DOC, &body(false), None).await;
    let id = job["job_id"].as_str().unwrap().to_owned();
    assert_eq!(ended(&env, &id).await.0, "failed");
    let (status, again) = post(&env, DOC, &body(false), None).await;
    assert_eq!(
        (status, &again["job_id"], &again["attempt"]),
        (StatusCode::ACCEPTED, &json!(id), &json!(2))
    );
    assert_eq!(ended(&env, &id).await.0, "succeeded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_requests_and_what_cannot_be_extracted() {
    let env = env(Some(script(vec![]))).await;
    let field = |k: &str, d: &str| json!({ "key": k, "description": d });
    let with_fields = |f: Vec<Value>| json!({ "revision": 1, "schema": { "fields": f } });
    for (what, b) in [
        ("no fields", with_fields(vec![])),
        (
            "101 fields",
            with_fields((0..101).map(|n| field(&format!("f{n}"), "d")).collect()),
        ),
        ("a bad key", with_fields(vec![field("Date", "d")])),
        (
            "a key twice",
            with_fields(vec![field("a", "d"), field("a", "e")]),
        ),
        ("an empty description", with_fields(vec![field("a", "")])),
        (
            "a long description",
            with_fields(vec![field("a", &"字".repeat(501))]),
        ),
        (
            "a type not known",
            with_fields(vec![
                json!({ "key": "a", "description": "d", "type": "money" }),
            ]),
        ),
        (
            "a null type",
            with_fields(vec![
                json!({ "key": "a", "description": "d", "type": null }),
            ]),
        ),
        (
            "revision 0",
            json!({ "revision": 0, "schema": { "fields": [field("a", "d")] } }),
        ),
        (
            "a property not known",
            json!({ "revision": 1, "schema": { "fields": [field("a", "d")] }, "model": "x" }),
        ),
    ] {
        let (status, err) = post(&env, DOC, &b, None).await;
        assert_eq!(
            (status, &err["error"]["code"]),
            (StatusCode::BAD_REQUEST, &json!("invalid_request")),
            "{what}"
        );
    }
    let (status, _) = post(&env, "doc_x", &body(false), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut two = body(false);
    two["revision"] = json!(2);
    assert_eq!(post(&env, DOC, &two, None).await.0, StatusCode::NOT_FOUND);
    // A flowing format has no text layer slice 1 reads.
    sqlx::query("UPDATE documents SET media_type = ? WHERE id = ?")
        .bind(DOCX)
        .bind(DOC)
        .execute(env.storage.db.pool())
        .await
        .unwrap();
    let docx = "doc_01K74Z3QJ8V5N2W9RTX6YB4M01";
    for sql in [
        format!(
            "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{docx}', 't', '{DOCX}', 1)"
        ),
        format!(
            "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin) VALUES ('{docx}', 1, 'sha256:{}', 1, '{DOCX}', 'import')",
            "0".repeat(64)
        ),
    ] {
        sqlx::query(&sql)
            .execute(env.storage.db.pool())
            .await
            .unwrap();
    }
    let (status, err) = post(&env, docx, &body(false), None).await;
    assert_eq!(
        (status, &err["error"]["code"]),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            &json!("unsupported_format")
        )
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_model_from_the_kernel_there_is_no_extraction() {
    let env = env(None).await;
    let (status, err) = post(&env, DOC, &body(false), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        (&err["error"]["code"], &err["error"]["details"]["engine"]),
        (&json!("engine_unavailable"), &json!("local-model"))
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(env.storage.db.pool())
        .await
        .unwrap();
    assert_eq!(n, 0);
    // Capabilities agree: extract needs the engine and a model.
    let caps = serde_json::to_value(crate::capabilities(&env.state).await).unwrap();
    let extract = caps["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["op"] == "extract")
        .unwrap()
        .clone();
    assert_eq!(
        (&extract["available"], &extract["reason"]),
        (&json!(false), &json!("engine_unavailable"))
    );
    let with = crate::extract::job::tests::env(Some(script(vec![]))).await;
    let caps = serde_json::to_value(crate::capabilities(&with.state).await).unwrap();
    let extract = caps["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["op"] == "extract")
        .unwrap()
        .clone();
    assert_eq!(extract["available"], json!(true));
}

/// No read engine (Linux, D10): 503 before any job or key is written, and
/// a key that is there is not queued again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_read_engine_there_is_no_extraction() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open_serving(
        dir.path(),
        crate::events::Events::default(),
        crate::engine::Layers::new(None),
    )
    .await;
    state.set_model(script(vec![]));
    let storage = state.storage().await.unwrap();
    for sql in [
        format!(
            "INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{DOC}', 't', 'application/pdf', 1)"
        ),
        format!(
            "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin) VALUES ('{DOC}', 1, 'sha256:{}', 1, 'application/pdf', 'import')",
            "0".repeat(64)
        ),
    ] {
        sqlx::query(&sql).execute(storage.db.pool()).await.unwrap();
    }
    let env = Env {
        _dir: dir,
        state,
        storage,
    };
    let (status, err) = post(&env, DOC, &body(false), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        (&err["error"]["code"], &err["error"]["details"]["engine"]),
        (&json!("engine_unavailable"), &json!("apple-pdfkit"))
    );
    let (jobs, keys): (i64, i64) =
        sqlx::query_as("SELECT (SELECT count(*) FROM jobs), (SELECT count(*) FROM idempotency)")
            .fetch_one(env.storage.db.pool())
            .await
            .unwrap();
    assert_eq!((jobs, keys), (0, 0));
}
