//! The scope (K1-06a §2, §5): outside it reads exactly as absent, a
//! malformed one refuses the call before storage, nothing carries over
//! between calls, and lists and cursors see only what is in it.

use super::*;

/// Two stored documents: `A` (n = 1, the parent's `DOC`) and `B` (n = 2).
const A: &str = DOC;
const B: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00002";
/// Well formed, never stored.
const C: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00099";
const NO_JOB: &str = "job_01K75A0B1C2D3E4F5G6H7J8K00";

async fn two_documents() -> (tempfile::TempDir, AppState) {
    let (dir, state) = opened().await;
    add_document(&state, 1, "'a'").await;
    add_document(&state, 2, "'b'").await;
    (dir, state)
}

/// The arguments of each operation that names a document, on `doc`.
fn on(doc: &str) -> [(&'static str, Value); 4] {
    let schema = json!({ "fields": [{ "key": "k", "type": "string", "description": "d" }] });
    [
        ("get", json!({ "document_id": doc })),
        ("read_range", json!({ "document_id": doc, "revision": 1 })),
        (
            "find",
            json!({ "document_id": doc, "revision": 1, "query": "a" }),
        ),
        (
            "extract",
            json!({ "document_id": doc, "revision": 1, "schema": schema }),
        ),
    ]
}

/// §5.1, §5.2: a stored document outside the scope answers byte for byte
/// as one that does not exist, for every operation that names one, under
/// an empty scope too; and nothing was started for it.
#[tokio::test]
async fn outside_the_scope_reads_exactly_as_absent() {
    let (_dir, state) = two_documents().await;
    for ((op, outside), (_, unknown)) in on(B).into_iter().zip(on(C)) {
        let absent = call_in(&state, op, &[A], unknown).await;
        assert_module_error(&absent, "not_found", false);
        assert_eq!(
            call_in(&state, op, &[A], outside.clone()).await,
            absent,
            "{op}"
        );
        assert_eq!(call_in(&state, op, &[], outside).await, absent, "{op}");
    }
    // The same answer as the service's own for a document it has none of.
    let service = call_in(&state, "get", &[A, C], json!({ "document_id": C })).await;
    let outside = call_in(&state, "get", &[A], json!({ "document_id": B })).await;
    assert_eq!(outside, service);
    let storage = state.storage().await.unwrap();
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(storage.db.pool())
        .await
        .unwrap();
    assert_eq!(jobs, 0, "an extract outside the scope started a job");
}

/// Inside the scope the same calls reach the service.
#[tokio::test]
async fn inside_the_scope_the_service_answers() {
    let (_dir, state) = two_documents().await;
    let body = call_in(&state, "get", &[A, B], json!({ "document_id": B })).await;
    assert_eq!(payload(&body)["document_id"], json!(B));
}

/// §5.4: a scope this module cannot read refuses the call before storage
/// (the state here has none: a read would answer `storage_unavailable`).
#[tokio::test]
async fn a_malformed_scope_refuses_every_operation_before_storage() {
    let too_many: Vec<String> = (0..=crate::scope::MAX_ENTRIES)
        .map(|n| format!("doc:doc_{n}"))
        .collect();
    let ops = [
        ("list", json!({})),
        ("get", json!({ "document_id": A })),
        ("job_get", json!({ "job_id": JOB })),
        ("read_range", json!({ "document_id": A, "revision": 1 })),
        (
            "find",
            json!({ "document_id": A, "revision": 1, "query": "a" }),
        ),
        (
            "extraction_get",
            json!({ "extraction_id": "ext_01K75A0B1C2D3E4F5G6H7J8K9M" }),
        ),
    ];
    for scope in [
        json!(null),
        json!(format!("doc:{A}")),
        json!([1]),
        json!(["doc:*"]),
        json!(["doc:"]),
        json!([format!("Doc:{A}")]),
        json!(["job:x"]),
        json!(["doc_01"]),
        json!(too_many),
    ] {
        for (op, args) in &ops {
            let mut ctx = context_for(op);
            ctx["authorized_resources"] = scope.clone();
            let body = json!({ "context": ctx, "arguments": args }).to_string();
            let body = post(op, body).await;
            assert_kernel_error(&body, ModuleToolErrorCode::PermissionDenied);
        }
    }
}

/// An absent `authorized_resources` is an empty scope, not a malformed one.
#[tokio::test]
async fn an_absent_scope_is_empty() {
    let (_dir, state) = two_documents().await;
    let mut ctx = context_for("get");
    ctx.as_object_mut().unwrap().remove("authorized_resources");
    let body = json!({ "context": ctx, "arguments": { "document_id": A } });
    let body = post_to(&state, "get", body.to_string()).await;
    let absent = call_in(&state, "get", &[C], json!({ "document_id": C })).await;
    assert_eq!(body, absent);
}

/// §5.6: a scope in the arguments is not a scope; the schema refuses it.
#[tokio::test]
async fn a_scope_in_the_arguments_is_invalid_input() {
    let (_dir, state) = two_documents().await;
    for smuggled in [
        json!({ "document_id": B, "authorized_resources": [format!("doc:{B}")] }),
        json!({ "document_id": B, "context": context_with("get", &[B]) }),
    ] {
        let body = call_in(&state, "get", &[A], smuggled).await;
        assert_kernel_error(&body, ModuleToolErrorCode::InvalidInput);
    }
}

/// §5.7: each call reads its own scope; a wider one before does not carry.
#[tokio::test]
async fn a_narrower_scope_after_a_wider_one_holds() {
    let (_dir, state) = two_documents().await;
    let args = json!({ "document_id": B });
    payload(&call_in(&state, "get", &[A, B], args.clone()).await);
    let absent = call_in(&state, "get", &[C], json!({ "document_id": C })).await;
    assert_eq!(call_in(&state, "get", &[A], args).await, absent);
}

/// §5.3: a job is reached through its document; one on a document outside
/// the scope, or on none, reads as no job at all.
#[tokio::test]
async fn a_job_outside_the_scope_reads_as_no_job() {
    let (_dir, state) = two_documents().await;
    add_failed_job(&state, "'it broke'").await; // on A
    let no_job = call_in(&state, "job_get", &[A, B], json!({ "job_id": NO_JOB })).await;
    assert_module_error(&no_job, "not_found", false);
    let job = json!({ "job_id": JOB });
    let found = payload(&call_in(&state, "job_get", &[A], job.clone()).await);
    assert_eq!(found["job_id"], json!(JOB));
    assert_eq!(call_in(&state, "job_get", &[B], job.clone()).await, no_job);
    assert_eq!(call_in(&state, "job_get", &[], job.clone()).await, no_job);
    let orphan = "job_01K75A0B1C2D3E4F5G6H7J8K01";
    exec(
        &state,
        &format!(
            "INSERT INTO jobs (id, kind, status, origin, error)
             VALUES ('{orphan}', 'probe', 'failed', '{{\"kind\":\"page\"}}',
                     json_object('code', 'parse_failed', 'message', 'no document'))"
        ),
    )
    .await;
    let orphan = json!({ "job_id": orphan });
    assert_eq!(call_in(&state, "job_get", &[A, B], orphan).await, no_job);
}

/// §5.1, §5.5: a list holds only the scope's documents, newest first, and
/// pages as if the others were not there: a cursor taken under one scope
/// pages on under another with nothing from outside it.
#[tokio::test]
async fn a_list_and_its_cursors_see_only_the_scope() {
    let (_dir, state) = opened().await;
    let ids: Vec<String> = five_documents(&state).await;
    let (d1, d3) = (ids[1].as_str(), ids[3].as_str());
    let page = |body: Vec<u8>| {
        let p = payload(&body);
        let got: Vec<String> = p["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["document_id"].as_str().unwrap().to_owned())
            .collect();
        (got, p["next_cursor"].as_str().map(str::to_owned))
    };
    let (all, next) = page(call_in(&state, "list", &[d1, d3], json!({})).await);
    assert_eq!((all, next), (vec![d3.to_owned(), d1.to_owned()], None));
    let (first, next) = page(call_in(&state, "list", &[d1, d3], json!({ "limit": 1 })).await);
    assert_eq!(first, vec![d3.to_owned()]);
    let cursor = next.expect("another document in the scope follows");
    let more = json!({ "limit": 1, "cursor": cursor });
    let (second, last) = page(call_in(&state, "list", &[d1, d3], more.clone()).await);
    // `d0` follows `d1` in storage but not in the scope: no further page.
    assert_eq!((second, last), (vec![d1.to_owned()], None));
    let (narrower, _) = page(call_in(&state, "list", &[d3], more).await);
    assert!(narrower.is_empty(), "{narrower:?}");
    let (none, next) = page(call_in(&state, "list", &[], json!({})).await);
    assert_eq!((none, next), (vec![], None));
    let (unknown, _) = page(call_in(&state, "list", &[C], json!({})).await);
    assert!(unknown.is_empty());
}

/// Documents 0 to 4, all at one timestamp: newest first is id descending.
async fn five_documents(state: &AppState) -> Vec<String> {
    let mut ids = Vec::new();
    for n in 0..5 {
        ids.push(add_document(state, n, &format!("'doc {n}'")).await);
    }
    ids
}
