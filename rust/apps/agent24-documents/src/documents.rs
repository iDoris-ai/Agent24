//! Documents (ADR-DOC-02 §3, §4): `GET /documents`, newest first, and
//! `GET /documents/{document_id}` with its head revision.

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path as UrlPath, Query};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::id::{IdKind, is_id};
use crate::state::Ready;
use crate::timestamp::is_timestamp;
use crate::uploads::is_sha256_address;

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;
/// A page's serialized JSON stays under this (ADR-DOC-02 §4).
const PAGE_BYTES: usize = 512 * 1024;
/// A title the OS writes is 1 to 500 characters (import, §5.6).
const MAX_TITLE_CHARS: usize = 500;
/// Every cursor the OS issues: hex of a 24-byte timestamp, a space and a
/// 30-byte id.
const CURSOR_LEN: usize = 2 * (24 + 1 + 30);

/// `Document`.
#[derive(Debug, Serialize, PartialEq, sqlx::FromRow)]
pub struct Document {
    #[sqlx(rename = "id")]
    pub document_id: String,
    pub title: String,
    pub media_type: String,
    pub head_revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// `DocumentRevisionSummary`.
#[derive(Debug, Serialize, PartialEq)]
pub struct RevisionSummary {
    pub revision: i64,
    pub content_sha256: String,
    pub size: i64,
    pub media_type: String,
}

/// `DocumentDetail`.
#[derive(Debug, Serialize, PartialEq)]
pub struct DocumentDetail {
    #[serde(flatten)]
    pub document: Document,
    pub head: RevisionSummary,
}

/// `DocumentList`.
#[derive(Debug, Serialize, PartialEq)]
pub struct DocumentList {
    pub documents: Vec<Document>,
    pub next_cursor: Option<String>,
}

/// `DocumentsMediaType`: lowercase `type/subtype` without parameters.
fn is_media_type(s: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'+' | b'-'))
    };
    s.split_once('/')
        .is_some_and(|(t, sub)| part(t) && part(sub))
}

/// The row as the contract has it, or why not. The table's CHECKs read ids
/// only up to a NUL and leave timestamps and media types unchecked; such a
/// row is a bug, never sent.
fn checked(d: &Document) -> Result<(), String> {
    let ok = is_id(IdKind::Document, &d.document_id)
        && d.title.chars().count() <= MAX_TITLE_CHARS
        && is_media_type(&d.media_type)
        && d.head_revision >= 1
        && is_timestamp(&d.created_at)
        && is_timestamp(&d.updated_at);
    if ok {
        Ok(())
    } else {
        Err(format!("document {:?} breaks the contract", d.document_id))
    }
}

/// The cursor: the last document's `created_at` and id, hex-encoded so it
/// is opaque and safe in a query string.
fn encode_cursor(d: &Document) -> String {
    format!("{} {}", d.created_at, d.document_id)
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn decode_cursor(cursor: &str) -> Option<(String, String)> {
    if cursor.len() != CURSOR_LEN {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..cursor.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(cursor.get(i..i + 2)?, 16).ok())
        .collect();
    let text = String::from_utf8(bytes?).ok()?;
    let (created_at, id) = text.split_once(' ')?;
    (is_timestamp(created_at) && is_id(IdKind::Document, id))
        .then(|| (created_at.to_owned(), id.to_owned()))
}

#[derive(Debug, Deserialize)]
pub struct ListParams {
    cursor: Option<String>,
    limit: Option<String>,
}

pub async fn list_documents(
    Ready(storage): Ready,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<DocumentList>, ApiError> {
    let Query(params) = params.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let limit = match params.limit.as_deref() {
        None => DEFAULT_LIMIT,
        Some(l) => l
            .parse::<i64>()
            .ok()
            .filter(|l| (1..=MAX_LIMIT).contains(l))
            .ok_or_else(|| ApiError::invalid_request("limit must be 1 to 200"))?,
    };
    let after = match params.cursor.as_deref() {
        None => None,
        Some(c) => Some(decode_cursor(c).ok_or_else(|| ApiError::invalid_request("bad cursor"))?),
    };
    let (after_at, after_id) = after.unzip();
    // One more row than the page, to know whether another page follows.
    let rows: Vec<Document> = match sqlx::query_as(
        "SELECT id, title, media_type, head_revision, created_at, updated_at FROM documents
         WHERE ?1 IS NULL OR (created_at, id) < (?1, ?2)
         ORDER BY created_at DESC, id DESC LIMIT ?3",
    )
    .bind(after_at)
    .bind(after_id)
    .bind(limit + 1)
    .fetch_all(storage.db.pool())
    .await
    {
        Ok(rows) => rows,
        Err(e) => return Err(storage.db_failure(e).await),
    };
    if let Some(why) = rows.iter().find_map(|d| checked(d).err()) {
        return Err(storage.db_failure(sqlx::Error::Decode(why.into())).await);
    }
    let page = fit_page(rows, usize::try_from(limit).unwrap_or(usize::MAX));
    if let Some(page) = page {
        return Ok(Json(page));
    }
    // A row the contract allows is a few KiB at most (the title check above).
    let why = "a document too large for a page".into();
    Err(storage.db_failure(sqlx::Error::Decode(why)).await)
}

/// The page: at most `limit` rows and at most `PAGE_BYTES` of JSON, with a
/// cursor after the last row it holds when more follow. `None` if not even
/// one row fits.
fn fit_page(rows: Vec<Document>, limit: usize) -> Option<DocumentList> {
    let envelope = |documents: Vec<Document>, next_cursor: Option<String>| DocumentList {
        documents,
        next_cursor,
    };
    // The envelope with a full-length cursor, and one comma per row.
    let mut used = serde_json::to_vec(&envelope(Vec::new(), Some("0".repeat(CURSOR_LEN))))
        .map_or(PAGE_BYTES, |v| v.len());
    let total = rows.len();
    let mut documents = Vec::new();
    for d in rows.into_iter().take(limit) {
        let size = serde_json::to_vec(&d).map_or(PAGE_BYTES, |v| v.len()) + 1;
        if used + size > PAGE_BYTES {
            break;
        }
        used += size;
        documents.push(d);
    }
    if documents.is_empty() && total > 0 {
        return None;
    }
    let more = documents.len() < total;
    let next_cursor = more.then(|| documents.last().map(encode_cursor)).flatten();
    Some(envelope(documents, next_cursor))
}

#[derive(sqlx::FromRow)]
struct HeadRow {
    #[sqlx(flatten)]
    document: Document,
    revision: Option<i64>,
    content_sha256: Option<String>,
    size: Option<i64>,
    head_media_type: Option<String>,
}

impl HeadRow {
    /// The document and its head revision, or a decode error: a document
    /// without its head, or a row the contract does not allow, is a bug.
    fn checked(self) -> Result<DocumentDetail, sqlx::Error> {
        checked(&self.document).map_err(|why| sqlx::Error::Decode(why.into()))?;
        let id = &self.document.document_id;
        let head = match (
            self.revision,
            self.content_sha256,
            self.size,
            self.head_media_type,
        ) {
            (Some(revision), Some(content_sha256), Some(size), Some(media_type))
                if is_sha256_address(&content_sha256)
                    && size >= 0
                    && is_media_type(&media_type) =>
            {
                RevisionSummary {
                    revision,
                    content_sha256,
                    size,
                    media_type,
                }
            }
            _ => return Err(sqlx::Error::Decode(format!("head revision of {id}").into())),
        };
        Ok(DocumentDetail {
            document: self.document,
            head,
        })
    }
}

pub async fn get_document(
    Ready(storage): Ready,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<DocumentDetail>, ApiError> {
    if !is_id(IdKind::Document, &id) {
        return Err(ApiError::invalid_request("not a document id"));
    }
    // One statement, so the document and its head are read together.
    let row: Result<Option<HeadRow>, sqlx::Error> = sqlx::query_as(
        "SELECT d.id, d.title, d.media_type, d.head_revision, d.created_at, d.updated_at,
                r.revision, r.content_sha256, r.size, r.media_type AS head_media_type
         FROM documents d
         LEFT JOIN revisions r ON r.document_id = d.id AND r.revision = d.head_revision
         WHERE d.id = ?",
    )
    .bind(&id)
    .fetch_optional(storage.db.pool())
    .await;
    let found = row.and_then(|row| row.map(HeadRow::checked).transpose());
    match found {
        Ok(Some(detail)) => Ok(Json(detail)),
        Ok(None) => Err(ApiError::not_found("no such document")),
        Err(e) => Err(storage.db_failure(e).await),
    }
}

#[cfg(test)]
mod tests;
