//! Importing a file into the Documenting OS (ADR-DOC-01 D5, ADR-DOC-02 §5.6):
//! hash the file, start an upload, send it as raw chunks of at most 768 KiB
//! and start the import. Only the file's base name is ever sent (ADR-001),
//! and no error carries its path.
//!
//! A port of the desktop's `documents-import.ts`, which this replaces: the
//! same requests, retries, idempotency keys and bounds, so a file imported
//! by an older desktop and by this code reaches the same upload.

use std::collections::VecDeque;
use std::io::{ErrorKind, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::response::{Response, to_response};
use crate::transport::{Method, Request, Transport};

const BASE: &str = "/api/v1/documents";
/// The OS's chunk limit (ADR-DOC-02 §5.6).
pub const CHUNK_BYTES: u64 = 786_432;
/// Replies to send again with the same key (ADR-DOC-02 §6): the transport's
/// own failures, the kernel's "not dispatched" codes and its "dispatched,
/// outcome unknown" ones.
const RETRY: &[&str] = &[
    "backend_not_ready",
    "backend_unreachable",
    "backend_timeout",
    "module_overloaded",
    "module_stopping",
    "module_not_ready",
    "module_draining",
    "request_abandoned",
    "upstream_timeout",
    "upstream_connection_closed",
    "upstream_unavailable",
    "upstream_response_too_large",
    "duplicate_request_id",
    "module_panicked",
    "module_killed",
    "stop_failed",
];
/// Sends per step. Every step is idempotent (the key, a chunk replay, the
/// upload id), so a retry never doubles anything.
const ATTEMPTS: usize = 3;
/// 409 resumes allowed in one run, so a confused server cannot loop us.
const MAX_RESUMES: usize = 8;
/// Uploads tried in one run: the first, then one more for each that the OS
/// says has expired.
const MAX_GENERATIONS: usize = 64;
/// Files whose place in their upload chain is remembered (`ChainTail`).
const MAX_TAILS: usize = 1024;

/// Per file (its first key), the chain's first upload (`root`) and the last
/// upload in it that `/imports` answered 404 for (`after`): the next run in
/// this process starts its walk just past `after`. Only a shortcut: without
/// it a run walks from the start (each CLI run is a new process, so it
/// always does); the check on `root` before any import catches an OS that
/// lost its data. Oldest first, at most `MAX_TAILS`.
struct ChainTail {
    first: String,
    root: String,
    after: String,
}

static CHAIN_TAILS: Mutex<VecDeque<ChainTail>> = Mutex::new(VecDeque::new());

/// For tests: each fake OS starts with no uploads.
pub fn forget_chains() {
    if let Ok(mut tails) = CHAIN_TAILS.lock() {
        tails.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub sent: u64,
    pub total: u64,
}

/// The upload's Idempotency-Key: derived from what it creates (the file's
/// hash and size, and its name), so a retry, a second import of the same
/// file or two at once all reach the same upload, and through it the same
/// import job. `after` is the expired upload a new key replaces. The
/// `desktop-` prefix and the JSON array are the desktop's, kept so keys
/// match across clients and versions.
pub(crate) fn key_for(
    sha256: &str,
    total: u64,
    filename: Option<&str>,
    after: Option<&str>,
) -> String {
    let basis = json!([sha256, total, filename, after]).to_string();
    format!("desktop-{}", hex(&Sha256::digest(basis.as_bytes())))
}

/// The base name when the contract allows it (1–255 characters, no `/`,
/// `\` or NUL, valid UTF-8); otherwise none: the OS then titles the
/// document itself.
pub(crate) fn filename_of(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let chars = name.chars().count();
    ((1..=255).contains(&chars) && !name.contains(['/', '\\', '\0'])).then(|| name.to_owned())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `Number.MAX_SAFE_INTEGER`, so `isOffset` matches the desktop.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

struct Up {
    upload_id: String,
    received: u64,
    status: String,
}

fn with_tails<R>(f: impl FnOnce(&mut VecDeque<ChainTail>) -> R) -> R {
    let mut tails = match CHAIN_TAILS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    f(&mut tails)
}

fn chain_hint(first: &str) -> Option<(String, String)> {
    with_tails(|tails| {
        tails
            .iter()
            .find(|c| c.first == first)
            .map(|c| (c.root.clone(), c.after.clone()))
    })
}

fn chain_forget(first: &str) {
    with_tails(|tails| {
        if let Some(i) = tails.iter().position(|c| c.first == first) {
            tails.remove(i);
        }
    });
}

fn chain_remember(first: &str, root: &str, after: &str) {
    with_tails(|tails| {
        if let Some(i) = tails.iter().position(|c| c.first == first) {
            tails.remove(i);
        }
        if tails.len() >= MAX_TAILS {
            tails.pop_front();
        }
        tails.push_back(ChainTail {
            first: first.to_owned(),
            root: root.to_owned(),
            after: after.to_owned(),
        });
    });
}

fn upload_id_ok(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 30
        && b.starts_with(b"upl_")
        && matches!(b[4], b'0'..=b'7')
        && b[5..].iter().all(|c| {
            matches!(
                c,
                b'0'..=b'9' | b'A'..=b'H' | b'J'..=b'K' | b'M'..=b'N' | b'P'..=b'T' | b'V'..=b'Z'
            )
        })
}

fn safe_u64(v: &Value) -> Option<u64> {
    if let Some(u) = v.as_u64() {
        return (u <= MAX_SAFE_INTEGER).then_some(u);
    }
    let f = v.as_f64()?;
    (f.is_finite() && (0.0..=MAX_SAFE_INTEGER as f64).contains(&f) && f.fract() == 0.0)
        .then_some(f as u64)
}

fn is_offset(v: &Value, max: u64) -> Option<u64> {
    safe_u64(v).filter(|n| *n <= max)
}

fn same_u64(v: &Value, expected: u64) -> bool {
    v.as_u64() == Some(expected)
        || matches!(
            v.as_f64(),
            Some(f) if f.fract() == 0.0 && expected <= MAX_SAFE_INTEGER && f == expected as f64
        )
}

fn json_bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

fn retryable(res: &Response) -> bool {
    matches!(res, Response::Err { error, .. } if RETRY.contains(&error.code.as_str()))
}

fn gone(res: &Response) -> bool {
    matches!(res, Response::Err { status: 404, error } if error.code == "not_found")
}

fn accepted_upload(data: &Value, total: u64, sha256: &str) -> Option<Up> {
    let upload_id = data.get("upload_id")?.as_str()?;
    if !upload_id_ok(upload_id) || !same_u64(data.get("total_size")?, total) {
        return None;
    }
    if data.get("sha256")?.as_str()? != sha256 {
        return None;
    }
    Some(Up {
        upload_id: upload_id.to_owned(),
        received: is_offset(data.get("received")?, total)?,
        status: data
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    })
}

async fn sha256_of(path: &Path) -> std::io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("sha256:{}", hex(&hasher.finalize())))
}

async fn step<T: Transport + Sync>(transport: &T, request: Request) -> Response {
    let mut res = Response::fail(503, "backend_unreachable", "not sent");
    for _ in 0..ATTEMPTS {
        res = to_response(transport.send(request.clone()).await);
        if !retryable(&res) {
            return res;
        }
    }
    res
}

async fn create<T: Transport + Sync>(
    transport: &T,
    total: u64,
    sha256: &str,
    filename: Option<&str>,
    after: Option<&str>,
) -> Result<Up, Response> {
    let mut payload = json!({"total_size": total, "sha256": sha256});
    if let Some(name) = filename {
        payload["filename"] = Value::String(name.to_owned());
    }
    let request = Request {
        method: Method::Post,
        path: format!("{BASE}/uploads"),
        headers: vec![
            ("Content-Type".to_owned(), "application/json".to_owned()),
            (
                "Idempotency-Key".to_owned(),
                key_for(sha256, total, filename, after),
            ),
        ],
        body: Some(json_bytes(&payload)),
    };
    match step(transport, request).await {
        Response::Ok { data, .. } => accepted_upload(&data, total, sha256).ok_or_else(|| {
            Response::fail(
                502,
                "invalid_response",
                "the upload does not match the file",
            )
        }),
        other => Err(other),
    }
}

async fn send_chunks<T: Transport + Sync>(
    file: &mut tokio::fs::File,
    upload_id: &str,
    mut offset: u64,
    total: u64,
    transport: &T,
    on_progress: &mut (dyn FnMut(Progress) + Send),
) -> Option<Response> {
    let mut resumes = 0usize;
    while offset < total {
        on_progress(Progress {
            sent: offset,
            total,
        });
        let want_u = std::cmp::min(CHUNK_BYTES, total - offset);
        let Ok(want) = usize::try_from(want_u) else {
            return Some(Response::fail(
                400,
                "file_unreadable",
                "the file could not be read",
            ));
        };
        if file.seek(SeekFrom::Start(offset)).await.is_err() {
            return Some(Response::fail(
                400,
                "file_unreadable",
                "the file could not be read",
            ));
        }
        let mut bytes = vec![0u8; want];
        match file.read_exact(&mut bytes).await {
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::UnexpectedEof => {
                return Some(Response::fail(
                    409,
                    "file_changed",
                    "the file changed while it was being sent",
                ));
            }
            Err(_) => {
                return Some(Response::fail(
                    400,
                    "file_unreadable",
                    "the file could not be read",
                ));
            }
        }
        let reply = step(
            transport,
            Request {
                method: Method::Post,
                path: format!("{BASE}/uploads/{upload_id}/chunks"),
                headers: vec![
                    (
                        "Content-Type".to_owned(),
                        "application/octet-stream".to_owned(),
                    ),
                    ("Upload-Offset".to_owned(), offset.to_string()),
                    (
                        "Chunk-Sha256".to_owned(),
                        format!("sha256:{}", hex(&Sha256::digest(&bytes))),
                    ),
                ],
                body: Some(bytes),
            },
        )
        .await;
        let resume_at = match &reply {
            Response::Ok { data, .. } => {
                match data.get("received").and_then(|v| is_offset(v, total)) {
                    Some(received) if received > offset => {
                        offset = received;
                        continue;
                    }
                    _ => {
                        return Some(Response::fail(
                            502,
                            "invalid_response",
                            "the upload did not move forward",
                        ));
                    }
                }
            }
            Response::Err { error, .. } if error.code == "upload_offset_mismatch" => error
                .details
                .as_ref()
                .and_then(|d| d.get("received_offset"))
                .and_then(|v| is_offset(v, total)),
            Response::Err { .. } => None,
        };
        if let Some(at) = resume_at
            && at != offset
            && resumes < MAX_RESUMES
        {
            resumes += 1;
            offset = at;
            continue;
        }
        return Some(reply);
    }
    None
}

async fn upload<T: Transport + Sync>(
    path: &Path,
    total: u64,
    sha256: &str,
    transport: &T,
    on_progress: &mut (dyn FnMut(Progress) + Send),
) -> Response {
    let filename = filename_of(path);
    let name = filename.as_deref();
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(_) => return Response::fail(400, "file_unreadable", "the file could not be read"),
    };
    let first = key_for(sha256, total, name, None);
    let hint = chain_hint(&first);
    let mut root = hint.as_ref().map(|(root, _)| root.clone());
    let mut after = hint.as_ref().map(|(_, after)| after.clone());
    for _generation in 0..MAX_GENERATIONS {
        let up = match create(transport, total, sha256, name, after.as_deref()).await {
            Ok(up) => up,
            Err(answer) => return answer,
        };
        if after.is_none() {
            root = Some(up.upload_id.clone());
        }
        if up.status != "expired" {
            match send_chunks(
                &mut file,
                &up.upload_id,
                up.received,
                total,
                transport,
                on_progress,
            )
            .await
            {
                Some(sent) if !gone(&sent) => return sent,
                Some(_) => {}
                None => on_progress(Progress { sent: total, total }),
            }
        }
        if root.as_deref() != Some(up.upload_id.as_str()) {
            match create(transport, total, sha256, name, None).await {
                Err(answer) => return answer,
                Ok(top) if root.as_deref() != Some(top.upload_id.as_str()) => {
                    chain_forget(&first);
                    root = None;
                    after = None;
                    continue;
                }
                Ok(_) => {}
            }
        }
        let started = step(
            transport,
            Request {
                method: Method::Post,
                path: format!("{BASE}/imports"),
                headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
                body: Some(json_bytes(&json!({"upload_id": up.upload_id}))),
            },
        )
        .await;
        if !gone(&started) {
            return started;
        }
        let Some(root_id) = root.clone() else {
            return Response::fail(400, "file_unreadable", "the file could not be read");
        };
        after = Some(up.upload_id.clone());
        chain_remember(&first, &root_id, &up.upload_id);
    }
    Response::fail(404, "not_found", "the upload keeps expiring")
}

/// Imports the file at `path`; on success the answer is the import job
/// (`/imports`' reply). A file that cannot be read is 400 `file_unreadable`
/// ("the file could not be read"), never the I/O error's text.
pub async fn import_file<T: Transport + Sync>(
    path: &Path,
    transport: &T,
    on_progress: &mut (dyn FnMut(Progress) + Send),
) -> Response {
    let total = match tokio::fs::metadata(path).await {
        Ok(meta) => meta.len(),
        Err(_) => return Response::fail(400, "file_unreadable", "the file could not be read"),
    };
    let sha256 = match sha256_of(path).await {
        Ok(sha) => sha,
        Err(_) => return Response::fail(400, "file_unreadable", "the file could not be read"),
    };
    upload(path, total, &sha256, transport, on_progress).await
}

#[cfg(test)]
mod tests;
