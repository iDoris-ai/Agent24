//! `POST /compositions` (D2 拆分合并 PDF): a new document of pages of
//! documents already here. Split is one part with some pages; merge is
//! several parts, all pages each. The engine writes the new PDF straight
//! into an upload of the OS's own, which is then imported as `POST
//! /imports` would: the answer is that import's job, and the new document is
//! the job's result.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::ApiError;
use crate::id::{IdKind, is_id, new_id};
use crate::imports::handler::import_upload;
use crate::jobs::Job;
use crate::read::revision;
use crate::state::{AppState, Storage, blob_cause, blocking, io_cause};
use crate::uploads::data::upload_dir;

/// Parts in one composition, at most.
pub const MAX_PARTS: usize = 20;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Part {
    document_id: String,
    /// `all` (the default) or 1-based pages and ranges, `1-3,5`.
    #[serde(default)]
    pages: Option<String>,
}

/// `DocumentsComposeRequest`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComposeRequest {
    parts: Vec<Part>,
    #[serde(default)]
    title: Option<String>,
}

/// `all`, or ranges like `1-3,5`: each page at least 1, a range not backwards.
#[must_use]
pub fn pages_ok(spec: &str) -> bool {
    if spec == "all" {
        return true;
    }
    let number = |s: &str| {
        (1..=6).contains(&s.len()) && !s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit())
    };
    spec.len() <= 200
        && spec.split(',').all(|part| match part.split_once('-') {
            Some((a, b)) => {
                number(a) && number(b) && a.parse::<u32>().ok() <= b.parse::<u32>().ok()
            }
            None => number(part),
        })
}

fn checked(req: &ComposeRequest) -> Result<(), ApiError> {
    if req.parts.is_empty() || req.parts.len() > MAX_PARTS {
        return Err(ApiError::invalid_request("1 to 20 parts"));
    }
    for part in &req.parts {
        if !is_id(IdKind::Document, &part.document_id) {
            return Err(ApiError::invalid_request("not a document id"));
        }
        if part.pages.as_deref().is_some_and(|p| !pages_ok(p)) {
            return Err(ApiError::invalid_request("pages is `all` or like 1-3,5"));
        }
    }
    if req
        .title
        .as_ref()
        .is_some_and(|t| !(1..=500).contains(&t.chars().count()))
    {
        return Err(ApiError::invalid_request(
            "title must be 1 to 500 characters",
        ));
    }
    Ok(())
}

/// The head revision's file of each part, with its pages.
async fn inputs(
    storage: &Arc<Storage>,
    parts: &[Part],
) -> Result<Vec<(PathBuf, String)>, ApiError> {
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        let head: Option<i64> =
            match sqlx::query_scalar("SELECT head_revision FROM documents WHERE id = ?")
                .bind(&part.document_id)
                .fetch_optional(storage.db.pool())
                .await
            {
                Ok(head) => head,
                Err(e) => return Err(storage.db_failure(e).await),
            };
        let head = head.ok_or_else(|| ApiError::not_found("no such document"))?;
        let r = revision(storage, &part.document_id, head).await?;
        if r.media_type != "application/pdf" {
            return Err(ApiError::unprocessable(
                "unsupported_format",
                "only PDF documents can be split or merged",
            ));
        }
        let file = storage
            .blobs
            .path_of(&r.content_sha256)
            .map_err(|e| ApiError::storage_unavailable(blob_cause(&e)))?;
        out.push((file, part.pages.clone().unwrap_or_else(|| "all".to_owned())));
    }
    Ok(out)
}

/// The largest chunk an upload records (`upload_chunks.chunk_size`).
const CHUNK: usize = 786_432;

fn sha(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

/// The file at `path`: its `sha256:<hex>`, size, and its chunks' offsets,
/// sizes and hashes, as an upload records them.
type Chunks = Vec<(i64, i64, String)>;
fn digest(path: &std::path::Path) -> std::io::Result<(String, i64, Chunks)> {
    let bytes = std::fs::read(path)?;
    let mut chunks = Vec::new();
    for (i, part) in bytes.chunks(CHUNK).enumerate() {
        let offset = i64::try_from(i * CHUNK).map_err(std::io::Error::other)?;
        let size = i64::try_from(part.len()).map_err(std::io::Error::other)?;
        chunks.push((offset, size, sha(part)));
    }
    let size = i64::try_from(bytes.len()).map_err(std::io::Error::other)?;
    Ok((sha(&bytes), size, chunks))
}

/// The upload's rows, as the chunked upload writes them (the triggers in
/// 0002/0004 allow no shortcut): receiving, each chunk, then complete.
async fn record_upload(
    storage: &Storage,
    id: &str,
    sha256: &str,
    size: i64,
    filename: &str,
    chunks: &Chunks,
) -> Result<(), sqlx::Error> {
    let mut tx = storage.db.pool().begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("INSERT INTO uploads (id, total_size, sha256, filename) VALUES (?, ?, ?, ?)")
        .bind(id)
        .bind(size)
        .bind(sha256)
        .bind(filename)
        .execute(&mut *tx)
        .await?;
    for (offset, chunk_size, chunk_sha) in chunks {
        sqlx::query(
            "INSERT INTO upload_chunks (upload_id, chunk_offset, chunk_size, sha256) VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(offset)
        .bind(chunk_size)
        .bind(chunk_sha)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE uploads SET received = ?1,
                    last_chunk_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                    status = CASE WHEN ?1 = total_size THEN 'complete' ELSE status END
             WHERE id = ?2",
        )
        .bind(offset + chunk_size)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

pub async fn compose(
    State(state): State<AppState>,
    body: Result<Json<ComposeRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let Json(req) = body.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    checked(&req)?;
    let storage = state
        .storage()
        .await
        .map_err(ApiError::storage_unavailable)?;
    let parts = inputs(&storage, &req.parts).await?;
    let upload_id = new_id(IdKind::Upload)
        .map_err(|_| ApiError::internal("could not generate an upload id"))?;
    let dir = upload_dir(storage.data_dir(), &upload_id);
    let made = dir.clone();
    blocking(move || std::fs::create_dir_all(&made))
        .await
        .map_err(|e| ApiError::storage_unavailable(io_cause(&e)))?;
    let data = dir.join("data");
    let target = data.clone();
    let composed = blocking(move || crate::pdf_compose::compose(&parts, &target)).await;
    let cleanup = |dir: PathBuf| async move {
        let _ = blocking(move || std::fs::remove_dir_all(dir)).await;
    };
    if let Err(e) = composed {
        cleanup(dir).await;
        return Err(match e {
            crate::pdf_compose::ComposeError::NoPage => {
                ApiError::invalid_request("a page is past the document's last page")
            }
            e => ApiError::unprocessable("compose_failed", e.to_string()),
        });
    }
    let read = data.clone();
    let (sha256, size, chunks) = match blocking(move || digest(&read)).await {
        Ok(d) => d,
        Err(e) => {
            cleanup(dir).await;
            return Err(ApiError::storage_unavailable(io_cause(&e)));
        }
    };
    let filename = format!(
        "{}.pdf",
        req.title
            .as_deref()
            .unwrap_or("composed")
            .chars()
            .filter(|c| !c.is_control())
            .map(|c| if c == '/' || c == '\\' { '_' } else { c })
            .take(200)
            .collect::<String>()
    );
    let wrote = record_upload(&storage, &upload_id, &sha256, size, &filename, &chunks).await;
    if let Err(e) = wrote {
        cleanup(dir).await;
        return Err(storage.db_failure(e).await);
    }
    import_upload(&storage, upload_id, req.title).await
}

#[cfg(test)]
mod tests {
    use super::pages_ok;

    #[test]
    fn pages_are_all_or_ranges() {
        for ok in ["all", "1", "1-3", "1-3,5", "2,4,6-9"] {
            assert!(pages_ok(ok), "{ok}");
        }
        for bad in [
            "", "0", "3-1", "1,,2", "a", "1-", "-2", "01", "1-3,", "1 - 3",
        ] {
            assert!(!pages_ok(bad), "{bad}");
        }
    }
}
