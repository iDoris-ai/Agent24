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
            headers: Vec::new(),
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
        headers: Vec::new(),
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

const EXT: &str = "ext_01K75A0B1C2D3E4F5G6H7J8K9M";

#[test]
fn the_read_extract_and_render_operations_map_to_their_fixed_routes() {
    let get = |p: String| Some((Method::Get, p));
    let text = format!("/api/v1/documents/documents/{DOC}/revisions/1/text");
    let page = format!("/api/v1/documents/documents/{DOC}/revisions/2/pages/3");
    let cases = [
        (
            json!({"op": "readText", "documentId": DOC, "revision": 1, "block": "p2/b10", "limit": 5, "requireComplete": true}),
            get(format!("{text}?block=p2/b10&limit=5&require_complete=true")),
        ),
        (
            json!({"op": "readText", "documentId": DOC, "revision": 1, "requireComplete": false}),
            get(format!("{text}?require_complete=false")),
        ),
        (
            json!({"op": "getExtraction", "extractionId": EXT}),
            get(format!("/api/v1/documents/extractions/{EXT}")),
        ),
        (
            json!({"op": "getExtraction", "extractionId": EXT, "cursor": "ab12", "limit": 200}),
            get(format!(
                "/api/v1/documents/extractions/{EXT}?cursor=ab12&limit=200"
            )),
        ),
        (
            json!({"op": "renderPage", "documentId": DOC, "revision": 2, "page": 3}),
            get(page.clone()),
        ),
        // The scale as JSON wrote it; an exponent's `+` encoded, or it is a space.
        (
            json!({"op": "renderPage", "documentId": DOC, "revision": 2, "page": 3, "scale": 0.25, "region": "0,1.5,1e+2,2E-1"}),
            get(format!("{page}?scale=0.25&region=0,1.5,1e%2B2,2E-1")),
        ),
        (
            json!({"op": "renderPage", "documentId": DOC, "revision": 2, "page": 3, "scale": 4}),
            get(format!("{page}?scale=4")),
        ),
    ];
    for (req, expected) in cases {
        assert_eq!(route_of(&req), expected, "{req}");
    }
}

#[test]
fn find_and_extract_send_their_body_as_json_and_extract_its_key() {
    let find = route(&json!({"op": "find", "documentId": DOC, "revision": 1, "query": "春节", "cursor": "ab12", "limit": 3, "requireComplete": true})).unwrap();
    assert_eq!(
        (find.method, find.path.as_str()),
        (
            Method::Post,
            format!("/api/v1/documents/documents/{DOC}/find").as_str()
        )
    );
    assert_eq!(
        find.headers,
        [("Content-Type".to_owned(), "application/json".to_owned())]
    );
    let body: Value = serde_json::from_slice(&find.body.unwrap()).unwrap();
    assert_eq!(
        body,
        json!({"revision": 1, "query": "春节", "cursor": "ab12", "limit": 3, "require_complete": true})
    );

    let schema = json!({"fields": [{"key": "date", "description": "the date", "type": "date"}]});
    let extract = route(&json!({"op": "extract", "documentId": DOC, "revision": 1, "schema": schema, "rerun": true, "idempotencyKey": "run-2"})).unwrap();
    assert_eq!(
        (extract.method, extract.path.as_str()),
        (
            Method::Post,
            format!("/api/v1/documents/documents/{DOC}/extractions").as_str()
        )
    );
    assert_eq!(
        extract.headers,
        [
            ("Idempotency-Key".to_owned(), "run-2".to_owned()),
            ("Content-Type".to_owned(), "application/json".to_owned())
        ]
    );
    let body: Value = serde_json::from_slice(&extract.body.unwrap()).unwrap();
    assert_eq!(
        body,
        json!({"revision": 1, "schema": schema, "rerun": true})
    );

    // Without a key or rerun: no header, no `rerun`; the OS answers a rerun
    // without a key (400), not this route.
    let plain =
        route(&json!({"op": "extract", "documentId": DOC, "revision": 1, "schema": schema}))
            .unwrap();
    assert_eq!(
        plain.headers,
        [("Content-Type".to_owned(), "application/json".to_owned())]
    );
    let body: Value = serde_json::from_slice(&plain.body.unwrap()).unwrap();
    assert_eq!(body, json!({"revision": 1, "schema": schema}));
}

#[test]
fn malformed_read_extract_and_render_requests_are_refused() {
    let schema = json!({"fields": []});
    for req in [
        json!({"op": "readText", "documentId": DOC, "revision": 1, "block": ""}),
        json!({"op": "readText", "documentId": DOC, "revision": 1, "block": "p1/b0"}),
        json!({"op": "readText", "documentId": DOC, "revision": 1, "block": "p01/b1"}),
        json!({"op": "readText", "documentId": DOC, "revision": 1, "block": "p1/b1&cursor=ab"}),
        json!({"op": "readText", "documentId": DOC, "revision": 1, "block": "p1"}),
        json!({"op": "readText", "documentId": DOC, "revision": 1, "block": 1}),
        json!({"op": "readText", "documentId": DOC, "revision": 1, "limit": 0}),
        json!({"op": "readText", "documentId": DOC, "revision": 1, "requireComplete": "true"}),
        json!({"op": "find", "documentId": DOC, "revision": 1}),
        json!({"op": "find", "documentId": DOC, "revision": 1, "query": ""}),
        json!({"op": "find", "documentId": DOC, "revision": 1, "query": "x".repeat(501)}),
        json!({"op": "find", "documentId": DOC, "query": "x"}),
        json!({"op": "find", "documentId": "doc_x", "revision": 1, "query": "x"}),
        json!({"op": "find", "documentId": DOC, "revision": 1, "query": "x", "cursor": "XY"}),
        json!({"op": "extract", "documentId": DOC, "revision": 1}),
        json!({"op": "extract", "documentId": DOC, "revision": 1, "schema": "fields"}),
        json!({"op": "extract", "documentId": DOC, "schema": schema}),
        json!({"op": "extract", "documentId": DOC, "revision": 1, "schema": schema, "rerun": "yes"}),
        json!({"op": "extract", "documentId": DOC, "revision": 1, "schema": schema, "idempotencyKey": "a b"}),
        json!({"op": "extract", "documentId": DOC, "revision": 1, "schema": schema, "idempotencyKey": ""}),
        json!({"op": "extract", "documentId": DOC, "revision": 1, "schema": schema, "idempotencyKey": "k".repeat(201)}),
        json!({"op": "extract", "documentId": DOC, "revision": 1, "schema": schema, "idempotencyKey": "键"}),
        json!({"op": "getExtraction", "extractionId": DOC}),
        json!({"op": "getExtraction", "extractionId": EXT, "limit": 201}),
        json!({"op": "renderPage", "documentId": DOC, "revision": 1}),
        json!({"op": "renderPage", "documentId": DOC, "revision": 1, "page": 0}),
        json!({"op": "renderPage", "documentId": DOC, "revision": 1, "page": 1, "scale": 0.2}),
        json!({"op": "renderPage", "documentId": DOC, "revision": 1, "page": 1, "scale": 4.5}),
        json!({"op": "renderPage", "documentId": DOC, "revision": 1, "page": 1, "scale": "1"}),
        json!({"op": "renderPage", "documentId": DOC, "revision": 1, "page": 1, "region": "0,0,1"}),
        json!({"op": "renderPage", "documentId": DOC, "revision": 1, "page": 1, "region": "0,0,1,1,2"}),
        json!({"op": "renderPage", "documentId": DOC, "revision": 1, "page": 1, "region": "-1,0,1,1"}),
        json!({"op": "renderPage", "documentId": DOC, "revision": 1, "page": 1, "region": "0,0,1.,1"}),
        json!({"op": "renderPage", "documentId": DOC, "revision": 1, "page": 1, "region": "0,0,1e,1"}),
        json!({"op": "renderPage", "documentId": DOC, "revision": 1, "page": 1, "region": "0,0,1,1&x=1"}),
    ] {
        assert_eq!(route(&req), None, "{req}");
    }
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

/// Answers every request with this reply.
struct Answers(Reply);

impl Transport for Answers {
    async fn send(&self, _: Request) -> Reply {
        self.0.clone()
    }
}

fn page_reply(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Reply {
    Reply::Http {
        status,
        headers: headers
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect(),
        body: body.to_vec(),
    }
}

#[tokio::test]
async fn a_rendered_page_comes_back_as_json_with_its_png_and_scale() {
    let render = json!({"op": "renderPage", "documentId": DOC, "revision": 1, "page": 1});
    let ok = Answers(page_reply(
        200,
        &[
            ("content-type", "image/png"),
            ("documents-render-scale", "0.5"),
        ],
        PNG,
    ));
    assert_eq!(
        call(&render, &ok).await,
        Response::Ok {
            status: 200,
            data: json!({"media_type": "image/png", "bytes": PNG.len(), "scale": 0.5, "png": "iVBORw0KGgoAAAANSUhEUg=="}),
        }
    );
    // Not a PNG with the scale the OS used: the reply breaks the contract.
    for (status, headers, body) in [
        (
            200,
            vec![
                ("content-type", "application/json"),
                ("documents-render-scale", "1"),
            ],
            br#"{"a":1}"#.as_slice(),
        ),
        (200, vec![("content-type", "image/png")], PNG),
        (
            200,
            vec![
                ("content-type", "image/png"),
                ("documents-render-scale", "8"),
            ],
            PNG,
        ),
        (
            200,
            vec![
                ("content-type", "image/png"),
                ("documents-render-scale", "x"),
            ],
            PNG,
        ),
        (
            200,
            vec![
                ("content-type", "image/png"),
                ("documents-render-scale", "1"),
            ],
            b"GIF89a".as_slice(),
        ),
        (
            204,
            vec![
                ("content-type", "image/png"),
                ("documents-render-scale", "1"),
            ],
            PNG,
        ),
        (302, vec![("location", "/x")], b"".as_slice()),
    ] {
        let res = call(&render, &Answers(page_reply(status, &headers, body))).await;
        assert_eq!(
            (res.status(), res.code()),
            (status, Some("invalid_response")),
            "{status} {headers:?}"
        );
    }
    // An error is the OS's envelope, as for any operation.
    let missing = Answers(page_reply(
        404,
        &[("content-type", "application/json")],
        br#"{"error":{"code":"not_found","message":"no such page"}}"#,
    ));
    assert_eq!(call(&render, &missing).await.code(), Some("not_found"));
    // A JSON operation never takes a PNG for its resource.
    let get = call(&json!({"op": "get", "documentId": DOC}), &ok).await;
    assert_eq!(get.code(), Some("invalid_response"));
}
