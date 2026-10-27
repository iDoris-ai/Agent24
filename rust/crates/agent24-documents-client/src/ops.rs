//! The desktop's document operations other than import
//! (`window.agent24.documents.request`, ported from its `documentsRoute` and
//! `documentsCall`). A request names an operation, never a path: each one
//! maps to a fixed route under `/api/v1/documents`, and every id, cursor and
//! limit is checked against its pattern before anything is sent.
//!
//! One known difference: the desktop ignored keys it did not read, whatever
//! they held; a request that is not valid JSON as a whole (an unpaired
//! surrogate escape such as `\ud800` in any key) never reaches [`route`]
//! here, and the CLI refuses it as 400 `invalid_request`.

use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::import::{BASE, safe_u64, ulid_id};
use crate::response::{Response, to_response};
use crate::transport::{Method, Reply, Request, Transport};

/// What the list, text, find and extraction routes take as a cursor: 1–4096
/// lower-case hex.
fn cursor_ok(c: &str) -> bool {
    (1..=4096).contains(&c.len()) && c.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `1` to `999999999`, no leading zero.
fn ordinal(s: &str) -> bool {
    (1..=9).contains(&s.len()) && !s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit())
}

/// A text layer's block id: `p<page>/b<n>` (`text_layer.rs`).
fn block_ok(b: &str) -> bool {
    let Some((page, n)) = b.split_once('/') else {
        return false;
    };
    matches!((page.strip_prefix('p'), n.strip_prefix('b')), (Some(p), Some(n)) if ordinal(p) && ordinal(n))
}

/// One corner coordinate of a render `region`, as the OpenAPI pattern takes
/// it: `[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?`.
fn coordinate_ok(c: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let (mantissa, exponent) = match c.find(['e', 'E']) {
        Some(i) => (&c[..i], Some(&c[i + 1..])),
        None => (c, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((whole, fraction)) => digits(whole) && digits(fraction),
        None => digits(mantissa),
    };
    let exponent_ok = exponent.is_none_or(|e| digits(e.strip_prefix(['+', '-']).unwrap_or(e)));
    mantissa_ok && exponent_ok
}

/// `x0,y0,x1,y1`, at most 400 characters.
fn region_ok(r: &str) -> bool {
    r.len() <= 400 && r.split(',').count() == 4 && r.split(',').all(coordinate_ok)
}

/// An `Idempotency-Key`: 1–200 visible ASCII characters, no spaces.
fn key_ok(k: &str) -> bool {
    (1..=200).contains(&k.len()) && k.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// What a routed operation answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// A JSON resource or the error envelope ([`to_response`]).
    Json,
    /// A rendered page: PNG bytes and their scale ([`to_page`]).
    Png,
}

/// The request's optional keys, each `None` when present but malformed.
struct Keys<'a>(&'a Map<String, Value>);

impl<'a> Keys<'a> {
    fn id(&self, key: &str, prefix: &str) -> Option<&'a str> {
        self.0
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| ulid_id(v, prefix))
    }

    /// A revision or page: a safe integer, at least 1.
    fn number(&self, key: &str) -> Option<u64> {
        self.0.get(key).and_then(safe_u64).filter(|n| *n >= 1)
    }

    /// An absent key is fine; a present one must pass `ok`.
    fn optional<T>(&self, key: &str, ok: impl FnOnce(&'a Value) -> Option<T>) -> Option<Option<T>> {
        match self.0.get(key) {
            None => Some(None),
            Some(v) => ok(v).map(Some),
        }
    }

    fn cursor(&self) -> Option<Option<&'a str>> {
        self.optional("cursor", |v| v.as_str().filter(|c| cursor_ok(c)))
    }

    fn limit(&self) -> Option<Option<u64>> {
        self.optional("limit", |v| safe_u64(v).filter(|n| (1..=200).contains(n)))
    }

    fn flag(&self, key: &str) -> Option<Option<bool>> {
        self.optional(key, Value::as_bool)
    }
}

/// `path` with `query`'s pairs, if any.
fn with_query(path: String, query: &[String]) -> String {
    match query.is_empty() {
        true => path,
        false => format!("{path}?{}", query.join("&")),
    }
}

/// The fixed route for a request (`{"op":"list","cursor":..,"limit":..}`,
/// `{"op":"get","documentId":..}`, ...), or `None` if it is not one of the
/// allowed operations or carries a malformed id, cursor, limit or revision.
/// Keys it does not read are ignored, as the desktop did.
pub fn route(req: &Value) -> Option<Request> {
    routed(req).map(|(request, _)| request)
}

fn routed(req: &Value) -> Option<(Request, Answer)> {
    let k = Keys(req.as_object()?);
    let get = |path: String| (Method::Get, path, None, Vec::new(), Answer::Json);
    let (method, path, body, headers, answer) = match k.0.get("op")?.as_str()? {
        "capabilities" => get(format!("{BASE}/capabilities")),
        "list" => {
            let mut query = Vec::new();
            if let Some(c) = k.cursor()? {
                query.push(format!("cursor={c}"));
            }
            if let Some(limit) = k.limit()? {
                query.push(format!("limit={limit}"));
            }
            get(with_query(format!("{BASE}/documents"), &query))
        }
        "get" => get(format!("{BASE}/documents/{}", k.id("documentId", "doc_")?)),
        "readText" => {
            let document = k.id("documentId", "doc_")?;
            let revision = k.number("revision")?;
            let mut query = Vec::new();
            if let Some(b) = k.optional("block", |v| v.as_str().filter(|b| block_ok(b)))? {
                query.push(format!("block={b}"));
            }
            if let Some(c) = k.cursor()? {
                query.push(format!("cursor={c}"));
            }
            if let Some(limit) = k.limit()? {
                query.push(format!("limit={limit}"));
            }
            if let Some(complete) = k.flag("requireComplete")? {
                query.push(format!("require_complete={complete}"));
            }
            let path = format!("{BASE}/documents/{document}/revisions/{revision}/text");
            get(with_query(path, &query))
        }
        "find" => {
            let document = k.id("documentId", "doc_")?;
            let query = k.0.get("query")?.as_str()?;
            if !(1..=500).contains(&query.chars().count()) {
                return None;
            }
            let mut body = json!({"revision": k.number("revision")?, "query": query});
            if let Some(c) = k.cursor()? {
                body["cursor"] = json!(c);
            }
            if let Some(limit) = k.limit()? {
                body["limit"] = json!(limit);
            }
            if let Some(complete) = k.flag("requireComplete")? {
                body["require_complete"] = json!(complete);
            }
            let path = format!("{BASE}/documents/{document}/find");
            (Method::Post, path, Some(body), Vec::new(), Answer::Json)
        }
        "extract" => {
            let document = k.id("documentId", "doc_")?;
            // The fields are the OS's to check (`DocumentsExtractRequest`).
            let schema = k.0.get("schema").filter(|s| s.is_object())?;
            let mut body = json!({"revision": k.number("revision")?, "schema": schema});
            if let Some(rerun) = k.flag("rerun")? {
                body["rerun"] = json!(rerun);
            }
            let mut headers = Vec::new();
            if let Some(key) = k.optional("idempotencyKey", |v| v.as_str().filter(|s| key_ok(s)))? {
                headers.push(("Idempotency-Key".to_owned(), key.to_owned()));
            }
            let path = format!("{BASE}/documents/{document}/extractions");
            (Method::Post, path, Some(body), headers, Answer::Json)
        }
        "getExtraction" => {
            let extraction = k.id("extractionId", "ext_")?;
            let mut query = Vec::new();
            if let Some(c) = k.cursor()? {
                query.push(format!("cursor={c}"));
            }
            if let Some(limit) = k.limit()? {
                query.push(format!("limit={limit}"));
            }
            get(with_query(
                format!("{BASE}/extractions/{extraction}"),
                &query,
            ))
        }
        "renderPage" => {
            let document = k.id("documentId", "doc_")?;
            let (revision, page) = (k.number("revision")?, k.number("page")?);
            let mut query = Vec::new();
            let scale = |v: &Value| {
                v.as_f64()
                    .filter(|s| (0.25..=4.0).contains(s))
                    .map(|_| v.to_string())
            };
            if let Some(s) = k.optional("scale", scale)? {
                query.push(format!("scale={s}"));
            }
            if let Some(r) = k.optional("region", |v| v.as_str().filter(|r| region_ok(r)))? {
                // `+` in a query is a space: an exponent's sign is sent encoded.
                query.push(format!("region={}", r.replace('+', "%2B")));
            }
            let path = format!("{BASE}/documents/{document}/revisions/{revision}/pages/{page}");
            let (method, path, body, headers, _) = get(with_query(path, &query));
            (method, path, body, headers, Answer::Png)
        }
        op @ ("job" | "cancelJob" | "retryJob") => {
            let job = k.id("jobId", "job_")?;
            match op {
                "job" => get(format!("{BASE}/jobs/{job}")),
                "cancelJob" => post(format!("{BASE}/jobs/{job}/cancel")),
                _ => post(format!("{BASE}/jobs/{job}/retry")),
            }
        }
        _ => return None,
    };
    let (headers, body) = match body {
        Some(body) => {
            let mut headers = headers;
            headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
            (headers, Some(body.to_string().into_bytes()))
        }
        None => (headers, None),
    };
    let request = Request {
        method,
        path,
        headers,
        body,
    };
    Some((request, answer))
}

fn post(path: String) -> (Method, String, Option<Value>, Vec<(String, String)>, Answer) {
    (Method::Post, path, None, Vec::new(), Answer::Json)
}

/// A rendered page as one JSON resource: `{media_type, bytes, scale, png}`,
/// the PNG in standard base64 and `scale` the one the OS used
/// (`Documents-Render-Scale`, 0.25–4). A 2xx that is not a PNG with its
/// scale is `invalid_response`; an error is read as [`to_response`] reads it.
fn to_page(reply: Reply) -> Response {
    let Reply::Http {
        status,
        headers,
        body,
    } = &reply
    else {
        return to_response(reply);
    };
    if !(200..300).contains(status) {
        return to_response(reply);
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    let png = header("content-type")
        .and_then(|t| t.split(';').next())
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("image/png"))
        && body.starts_with(b"\x89PNG\r\n\x1a\n");
    let scale = header("documents-render-scale")
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|s| (0.25..=4.0).contains(s));
    match (*status, png, scale) {
        (200, true, Some(scale)) => Response::Ok {
            status: 200,
            data: json!({
                "media_type": "image/png",
                "bytes": body.len(),
                "scale": scale,
                "png": base64::engine::general_purpose::STANDARD.encode(body),
            }),
        },
        _ => Response::fail(
            *status,
            "invalid_response",
            &format!("unexpected {status} reply from the daemon"),
        ),
    }
}

/// The answer to a request [`route`] does not take: 400 `invalid_request`.
pub fn refused() -> Response {
    Response::fail(400, "invalid_request", "not an allowed documents operation")
}

/// Runs one allowed operation; anything else is [`refused`] here, unsent.
pub async fn call<T: Transport + Sync>(req: &Value, transport: &T) -> Response {
    match routed(req) {
        Some((request, Answer::Json)) => to_response(transport.send(request).await),
        Some((request, Answer::Png)) => to_page(transport.send(request).await),
        None => refused(),
    }
}

#[cfg(test)]
mod tests;
