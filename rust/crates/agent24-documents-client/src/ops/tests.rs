// Ports of the documentsRoute, toResponse and documentsCall cases in
// apps/desktop/src/main/documents.test.ts.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use serde_json::{Value, json};

use super::*;
use crate::response::ApiError;
use crate::transport::Reply;

const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4MCD";
const JOB: &str = "job_01K75A0B1C2D3E4F5G6H7J8K9M";

fn route_of(req: &Value) -> Option<(Method, String)> {
    route(req).map(|r| {
        assert!(r.headers.is_empty() && r.body.is_none(), "{req}");
        (r.method, r.path)
    })
}

#[test]
fn each_allowed_operation_maps_to_its_fixed_route() {
    let get = |p: String| Some((Method::Get, p));
    let post = |p: String| Some((Method::Post, p));
    let cases = [
        (
            json!({"op": "capabilities"}),
            get("/api/v1/documents/capabilities".into()),
        ),
        (
            json!({"op": "list"}),
            get("/api/v1/documents/documents".into()),
        ),
        (
            json!({"op": "list", "cursor": "ab12", "limit": 20}),
            get("/api/v1/documents/documents?cursor=ab12&limit=20".into()),
        ),
        (
            json!({"op": "list", "limit": 200}),
            get("/api/v1/documents/documents?limit=200".into()),
        ),
        (
            json!({"op": "get", "documentId": DOC}),
            get(format!("/api/v1/documents/documents/{DOC}")),
        ),
        (
            json!({"op": "readText", "documentId": DOC, "revision": 2}),
            get(format!(
                "/api/v1/documents/documents/{DOC}/revisions/2/text"
            )),
        ),
        (
            json!({"op": "readText", "documentId": DOC, "revision": 1, "cursor": "ab12"}),
            get(format!(
                "/api/v1/documents/documents/{DOC}/revisions/1/text?cursor=ab12"
            )),
        ),
        (
            json!({"op": "job", "jobId": JOB}),
            get(format!("/api/v1/documents/jobs/{JOB}")),
        ),
        (
            json!({"op": "cancelJob", "jobId": JOB}),
            post(format!("/api/v1/documents/jobs/{JOB}/cancel")),
        ),
        (
            json!({"op": "retryJob", "jobId": JOB}),
            post(format!("/api/v1/documents/jobs/{JOB}/retry")),
        ),
        // Keys an operation does not read are ignored, as the desktop did.
        (
            json!({"op": "capabilities", "path": "/api/v1/runs"}),
            get("/api/v1/documents/capabilities".into()),
        ),
    ];
    for (req, expected) in cases {
        assert_eq!(route_of(&req), expected, "{req}");
    }
}

#[test]
fn anything_else_is_refused_and_no_path_ever_comes_from_the_request() {
    let long_cursor = "a".repeat(4097);
    for req in [
        json!(null),
        json!("list"),
        json!({"op": "proxy", "path": "/api/v1/runs"}),
        json!({"op": "__proto__"}),
        json!({"op": "constructor"}),
        json!({"op": 1}),
        json!({}),
        json!({"op": "get", "documentId": "../../runs"}),
        json!({"op": "get", "documentId": format!("{DOC}/../x")}),
        json!({"op": "get", "documentId": format!("{DOC}%2F..")}),
        json!({"op": "get", "documentId": "doc_x"}),
        // The first ULID character is 0–7, and I, L, O, U are not Crockford.
        json!({"op": "get", "documentId": "doc_81K74Z3QJ8V5N2W9RTX6YB4MCD"}),
        json!({"op": "get", "documentId": "doc_01K74Z3QJ8V5N2W9RTX6YB4MCI"}),
        json!({"op": "get", "documentId": "doc_01k74z3qj8v5n2w9rtx6yb4mcd"}),
        json!({"op": "get"}),
        json!({"op": "job", "jobId": DOC}),
        json!({"op": "cancelJob", "jobId": format!("{JOB}?x=1")}),
        json!({"op": "retryJob"}),
        json!({"op": "list", "cursor": "not hex"}),
        json!({"op": "list", "cursor": "AB12"}),
        json!({"op": "list", "cursor": "ab&limit=1"}),
        json!({"op": "list", "cursor": ""}),
        json!({"op": "list", "cursor": long_cursor}),
        json!({"op": "list", "cursor": 12}),
        json!({"op": "list", "cursor": null}),
        json!({"op": "list", "limit": 0}),
        json!({"op": "list", "limit": 201}),
        json!({"op": "list", "limit": 1.5}),
        json!({"op": "list", "limit": -1}),
        json!({"op": "list", "limit": "20"}),
        json!({"op": "readText", "documentId": DOC}),
        json!({"op": "readText", "documentId": DOC, "revision": 0}),
        json!({"op": "readText", "documentId": DOC, "revision": 1.5}),
        json!({"op": "readText", "documentId": DOC, "revision": 9_007_199_254_740_992_u64}),
        json!({"op": "readText", "documentId": DOC, "revision": "1"}),
        json!({"op": "readText", "documentId": DOC, "revision": "1/../../runs"}),
        json!({"op": "readText", "documentId": "doc_x", "revision": 1}),
        json!({"op": "readText", "documentId": DOC, "revision": 1, "cursor": "ab&block=x"}),
        json!({"op": "readText", "documentId": DOC, "revision": 1, "cursor": ""}),
    ] {
        assert_eq!(route_of(&req), None, "{req}");
    }
}

#[derive(Default)]
struct Fake {
    sent: Mutex<Vec<(Method, String)>>,
}

impl Transport for Fake {
    async fn send(&self, request: Request) -> Reply {
        self.sent
            .lock()
            .unwrap()
            .push((request.method, request.path));
        Reply::Http {
            status: 200,
            body: br#"{"documents":[],"next_cursor":null}"#.to_vec(),
        }
    }
}

#[tokio::test]
async fn call_sends_an_allowed_operation_and_refuses_the_rest_unsent() {
    let fake = Fake::default();
    assert_eq!(
        call(&json!({"op": "list"}), &fake).await,
        Response::Ok {
            status: 200,
            data: json!({"documents": [], "next_cursor": null}),
        }
    );
    assert_eq!(
        call(&json!({"op": "get", "documentId": "../x"}), &fake).await,
        Response::Err {
            status: 400,
            error: ApiError::new("invalid_request", "not an allowed documents operation"),
        }
    );
    assert_eq!(
        *fake.sent.lock().unwrap(),
        [(Method::Get, "/api/v1/documents/documents".to_owned())]
    );
}

#[test]
fn a_reply_is_the_resource_its_error_envelope_or_invalid_response() {
    let reply = |status: u16, body: &str| Reply::Http {
        status,
        body: body.as_bytes().to_vec(),
    };
    assert_eq!(
        to_response(reply(
            404,
            r#"{"error":{"code":"not_found","message":"no such job"}}"#
        )),
        Response::Err {
            status: 404,
            error: ApiError::new("not_found", "no such job"),
        }
    );
    for (status, body) in [
        (200, "not json"),
        (200, ""),
        (200, "null"),
        (302, r#"{"documents":[]}"#),
        (500, "oops"),
        (503, r#"{"error":"text only"}"#),
        (600, r#"{"error":{"code":"not_found","message":"x"}}"#),
    ] {
        let res = to_response(reply(status, body));
        assert_eq!(
            (res.status(), res.code()),
            (status, Some("invalid_response")),
            "{status} {body}"
        );
    }
}
