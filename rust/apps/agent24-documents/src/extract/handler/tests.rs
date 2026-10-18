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
async fn a_rerun_key_names_a_rerun_of_one_document() {
    const DOC2: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4M01";
    let env = env(Some(script(vec![answer("present"), answer("present")]))).await;
    for sql in [
        format!(
            "INSERT INTO documents (id, title, media_type, head_revision)
             SELECT '{DOC2}', title, media_type, head_revision FROM documents WHERE id = '{DOC}'"
        ),
        format!(
            "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
             SELECT '{DOC2}', revision, content_sha256, size, media_type, origin
             FROM revisions WHERE document_id = '{DOC}'"
        ),
    ] {
        sqlx::query(&sql)
            .execute(env.storage.db.pool())
            .await
            .unwrap();
    }
    let (_, one) = post(&env, DOC, &body(true), Some("rerun-1")).await;
    let one = one["job_id"].as_str().unwrap().to_owned();
    assert_eq!(ended(&env, &one).await.0, "succeeded");
    // The same key and body on another document: that document's rerun,
    // not a replay of this one's job (§5.4).
    let (status, two) = post(&env, DOC2, &body(true), Some("rerun-1")).await;
    assert_eq!(
        (status, &two["document_id"]),
        (StatusCode::ACCEPTED, &json!(DOC2)),
        "{two}"
    );
    let two = two["job_id"].as_str().unwrap().to_owned();
    assert_ne!(two, one);
    assert_eq!(ended(&env, &two).await.0, "succeeded");
    for (doc, job) in [(DOC, &one), (DOC2, &two)] {
        let (status, again) = post(&env, doc, &body(true), Some("rerun-1")).await;
        assert_eq!((status, &again["job_id"]), (StatusCode::OK, &json!(job)));
    }
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

/// `"rerun": false` says what leaving it out says: the same job, not 422
/// (DOC-1-06b §2.3). Without `rerun` the hash is the one the whole body
/// always had, so a stored key still replays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_rerun_false_is_the_request_without_it() {
    let env = env(Some(script(vec![answer("present")]))).await;
    let (status, job) = post(&env, DOC, &body(false), None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{job}");
    let id = job["job_id"].as_str().unwrap().to_owned();
    assert_eq!(ended(&env, &id).await.0, "succeeded");
    let stored: String = sqlx::query_scalar("SELECT request_sha256 FROM idempotency")
        .fetch_one(env.storage.db.pool())
        .await
        .unwrap();
    assert_eq!(stored, request_sha256(&body(false)).unwrap());
    let mut explicit = body(false);
    explicit["rerun"] = json!(false);
    let (status, again) = post(&env, DOC, &explicit, None).await;
    assert_eq!(
        (status, &again["job_id"]),
        (StatusCode::OK, &json!(id)),
        "{again}"
    );
}

#[test]
fn only_rerun_false_is_left_out_of_the_hash() {
    let plain = body(false);
    let mut explicit = plain.clone();
    explicit["rerun"] = json!(false);
    assert_eq!(request_hash(&plain), request_sha256(&plain));
    assert_eq!(request_hash(&explicit), request_sha256(&plain));
    let rerun = body(true);
    assert_eq!(request_hash(&rerun), request_sha256(&rerun));
    assert_ne!(request_hash(&rerun), request_hash(&plain));
}

async fn origin_of(env: &Env, job: &str) -> Value {
    let origin: String = sqlx::query_scalar("SELECT origin FROM jobs WHERE id = ?")
        .bind(job)
        .fetch_one(env.storage.db.pool())
        .await
        .unwrap();
    serde_json::from_str(&origin).unwrap()
}

/// What a claim from a run looks like, with the key the page would use.
async fn claim_as(env: &Env, origin: Origin) -> (Job, bool) {
    let b = body(false);
    let req: ExtractRequest = serde_json::from_value(b.clone()).unwrap();
    let schema = schema_sha256(&req.schema.fields).unwrap();
    let key = format!("{DOC}/1/{schema}/{EXTRACTOR_VERSION}");
    let input = Input {
        document_id: DOC.into(),
        revision: 1,
        schema_sha256: schema,
        fields: req.schema.fields,
    };
    let request = request_hash(&b).unwrap();
    match claim(
        env.state.clone(),
        env.storage.clone(),
        key,
        request,
        input,
        origin,
    )
    .await
    {
        Ok(claimed) => claimed,
        Err(_) => panic!("the claim failed"),
    }
}

fn a_run() -> Origin {
    Origin::Run(crate::oplog::Run {
        run_id: "run_1".into(),
        tool_call_id: "call_1".into(),
    })
}

/// The page's job says so; a run's job names the run; a replay keeps the
/// first request's origin, whoever replays it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_keeps_the_origin_of_its_first_request() {
    let env = env(Some(script(vec![answer("present")]))).await;
    let (_, job) = post(&env, DOC, &body(false), None).await;
    let id = job["job_id"].as_str().unwrap().to_owned();
    assert_eq!(origin_of(&env, &id).await, json!({ "kind": "page" }));
    assert_eq!(ended(&env, &id).await.0, "succeeded");
    let (replayed, started) = claim_as(&env, a_run()).await;
    assert_eq!((replayed.job_id.as_str(), started), (id.as_str(), false));
    assert_eq!(origin_of(&env, &id).await, json!({ "kind": "page" }));

    let env = crate::extract::job::tests::env(Some(script(vec![answer("present")]))).await;
    let (job, started) = claim_as(&env, a_run()).await;
    assert!(started);
    let run = json!({ "kind": "run", "run_id": "run_1", "tool_call_id": "call_1" });
    assert_eq!(origin_of(&env, &job.job_id).await, run);
    assert_eq!(ended(&env, &job.job_id).await.0, "succeeded");
    let (status, again) = post(&env, DOC, &body(false), None).await;
    assert_eq!(
        (status, &again["job_id"]),
        (StatusCode::OK, &json!(job.job_id))
    );
    assert_eq!(origin_of(&env, &job.job_id).await, run);
}
