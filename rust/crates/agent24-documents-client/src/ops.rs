//! The desktop's document operations other than import
//! (`window.agent24.documents.request`, ported from its `documentsRoute` and
//! `documentsCall`). A request names an operation, never a path: each one
//! maps to a fixed route under `/api/v1/documents`, and every id, cursor and
//! limit is checked against its pattern before anything is sent.

use serde_json::Value;

use crate::import::{BASE, safe_u64, ulid_id};
use crate::response::{Response, to_response};
use crate::transport::{Method, Request, Transport};

/// What the list and text routes take as a cursor: 1–4096 lower-case hex.
fn cursor_ok(c: &str) -> bool {
    (1..=4096).contains(&c.len()) && c.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The fixed route for a request (`{"op":"list","cursor":..,"limit":..}`,
/// `{"op":"get","documentId":..}`, ...), or `None` if it is not one of the
/// allowed operations or carries a malformed id, cursor, limit or revision.
/// Keys it does not read are ignored, as the desktop did.
pub fn route(req: &Value) -> Option<Request> {
    let r = req.as_object()?;
    let id = |key: &str, prefix: &str| {
        r.get(key)
            .and_then(Value::as_str)
            .filter(|v| ulid_id(v, prefix))
    };
    // An absent key is fine; a present one must be a well-formed cursor.
    let cursor = || match r.get("cursor") {
        None => Some(None),
        Some(v) => v.as_str().filter(|c| cursor_ok(c)).map(Some),
    };
    let get = |path: String| (Method::Get, path);
    let (method, path) = match r.get("op")?.as_str()? {
        "capabilities" => get(format!("{BASE}/capabilities")),
        "list" => {
            let mut query = Vec::new();
            if let Some(c) = cursor()? {
                query.push(format!("cursor={c}"));
            }
            if let Some(v) = r.get("limit") {
                let limit = safe_u64(v).filter(|n| (1..=200).contains(n))?;
                query.push(format!("limit={limit}"));
            }
            get(match query.is_empty() {
                true => format!("{BASE}/documents"),
                false => format!("{BASE}/documents?{}", query.join("&")),
            })
        }
        "get" => get(format!("{BASE}/documents/{}", id("documentId", "doc_")?)),
        "readText" => {
            let document = id("documentId", "doc_")?;
            let revision = r.get("revision").and_then(safe_u64).filter(|n| *n >= 1)?;
            let path = format!("{BASE}/documents/{document}/revisions/{revision}/text");
            get(match cursor()? {
                Some(c) => format!("{path}?cursor={c}"),
                None => path,
            })
        }
        "job" => get(format!("{BASE}/jobs/{}", id("jobId", "job_")?)),
        op @ ("cancelJob" | "retryJob") => {
            let job = id("jobId", "job_")?;
            let action = if op == "cancelJob" { "cancel" } else { "retry" };
            (Method::Post, format!("{BASE}/jobs/{job}/{action}"))
        }
        _ => return None,
    };
    Some(Request {
        method,
        path,
        headers: Vec::new(),
        body: None,
    })
}

/// The answer to a request [`route`] does not take: 400 `invalid_request`.
pub fn refused() -> Response {
    Response::fail(400, "invalid_request", "not an allowed documents operation")
}

/// Runs one allowed operation; anything else is [`refused`] here, unsent.
pub async fn call<T: Transport + Sync>(req: &Value, transport: &T) -> Response {
    match route(req) {
        Some(request) => to_response(transport.send(request).await),
        None => refused(),
    }
}

#[cfg(test)]
mod tests;
