//! Extractions (ADR-DOC-02 §3, §3.2): the values an extraction holds, as
//! the contract has them, and the rules the OS checks on them; storing
//! them, and `GET /extractions/{extraction_id}` (`documentsGetExtraction`): an
//! extraction and one page of its values, in the order of the requested
//! fields. An extraction is written once, with all its values, by the job
//! that made it, and never changes.

use axum::Json;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path as UrlPath, Query};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqliteConnection;

use crate::error::ApiError;
use crate::id::{IdKind, is_id};
use crate::state::Ready;

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
/// A page's serialized JSON stays under this (§4).
const PAGE_BYTES: usize = 512 * 1024;
/// The envelope of a page, at most: fixed-size ids, hashes and times, a
/// version of 64 bytes and a model id of 256, each byte escaped as `\u00XX`
/// at worst, and the longest cursor.
const MAX_ENVELOPE: usize = 4 * 1024;
/// One value, serialized, at most: so a page always holds at least one.
pub const MAX_VALUE_BYTES: usize = PAGE_BYTES - MAX_ENVELOPE;
/// Values in one extraction, at most: a request has at most 100 fields.
pub const MAX_VALUES: usize = 100;

/// An extraction to store: what it was made from and by, and its values
/// (each a `DocumentsExtractedValue`), in field order.
pub struct NewExtraction {
    pub id: String,
    pub document_id: String,
    pub revision: i64,
    pub schema_sha256: String,
    pub extractor_version: String,
    pub model_id: String,
    pub text_layer_sha256: String,
    pub values: Vec<ExtractedValue>,
}

/// Stores `e` on `conn`, which the caller has in a transaction with the job
/// that names it. A value that breaks the contract, too many values, or one
/// too large to page is refused: nothing a reader would reject is stored.
pub async fn insert(conn: &mut SqliteConnection, e: &NewExtraction) -> Result<(), sqlx::Error> {
    let refuse = |why: String| sqlx::Error::Protocol(format!("extraction {}: {why}", e.id));
    if e.values.len() > MAX_VALUES {
        return Err(refuse(format!("{} values", e.values.len())));
    }
    let mut stored = Vec::with_capacity(e.values.len());
    for v in &e.values {
        v.check().map_err(refuse)?;
        let json = serde_json::to_string(v).map_err(|e| refuse(e.to_string()))?;
        if json.len() > MAX_VALUE_BYTES {
            return Err(refuse(format!("{} is {} bytes", v.key, json.len())));
        }
        stored.push(json);
    }
    sqlx::query(
        "INSERT INTO extractions (id, document_id, revision, schema_sha256, extractor_version,
                                  model_id, text_layer_sha256)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&e.id)
    .bind(&e.document_id)
    .bind(e.revision)
    .bind(&e.schema_sha256)
    .bind(&e.extractor_version)
    .bind(&e.model_id)
    .bind(&e.text_layer_sha256)
    .execute(&mut *conn)
    .await?;
    for (ord, value) in stored.iter().enumerate() {
        sqlx::query("INSERT INTO extraction_values (extraction_id, ord, value) VALUES (?, ?, ?)")
            .bind(&e.id)
            .bind(i64::try_from(ord).unwrap_or(i64::MAX))
            .bind(value)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageParams {
    cursor: Option<String>,
    limit: Option<usize>,
}

/// `DocumentsExtraction`.
#[derive(Serialize)]
pub struct Extraction {
    extraction_id: String,
    document_id: String,
    revision: i64,
    content_sha256: String,
    schema_sha256: String,
    extractor_version: String,
    model_id: String,
    created_at: String,
    /// As stored, once checked: what was read is what is sent.
    values: Vec<Value>,
    next_cursor: Option<String>,
}

/// The cursor: the next value's place, `v` and 1–2 digits. Only ever read
/// against the extraction it is sent with, which cannot change.
fn decode_cursor(c: &str) -> Option<usize> {
    let n = c.strip_prefix('v')?;
    let ok = (1..=2).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| n.parse().ok())
        .flatten()
        .filter(|&n| n < MAX_VALUES)
}

pub async fn get_extraction(
    Ready(storage): Ready,
    path: Result<UrlPath<String>, PathRejection>,
    params: Result<Query<PageParams>, QueryRejection>,
) -> Result<Json<Extraction>, ApiError> {
    let UrlPath(id) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let Query(p) = params.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT);
    let from = match p.cursor.as_deref() {
        None => Some(0),
        Some(c) => decode_cursor(c),
    };
    let (Some(from), true, true) = (
        from,
        (1..=MAX_LIMIT).contains(&limit),
        is_id(IdKind::Extraction, &id),
    ) else {
        return Err(ApiError::invalid_request(
            "an extraction id; limit is 1 to 200; a cursor from the previous page",
        ));
    };
    let pool = storage.db.pool();
    // One transaction, so the extraction and its values are read together.
    let read = async {
        let mut tx = pool.begin().await?;
        let head: Option<(String, i64, String, String, String, String, String)> = sqlx::query_as(
            "SELECT e.document_id, e.revision, r.content_sha256, e.schema_sha256,
                    e.extractor_version, e.model_id, e.created_at
             FROM extractions e
             JOIN revisions r ON r.document_id = e.document_id AND r.revision = e.revision
             WHERE e.id = ?",
        )
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?;
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT ord, value FROM extraction_values WHERE extraction_id = ? AND ord >= ?
             ORDER BY ord LIMIT ?",
        )
        .bind(&id)
        .bind(i64::try_from(from).unwrap_or(i64::MAX))
        .bind(i64::try_from(limit + 1).unwrap_or(i64::MAX))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok::<_, sqlx::Error>((head, rows))
    };
    let (head, rows) = match read.await {
        Ok(read) => read,
        Err(e) => return Err(storage.db_failure(e).await),
    };
    let Some((
        document_id,
        revision,
        content_sha256,
        schema_sha256,
        extractor_version,
        model_id,
        created_at,
    )) = head
    else {
        return Err(ApiError::not_found("no such extraction"));
    };
    let mut page = Extraction {
        extraction_id: id,
        document_id,
        revision,
        content_sha256,
        schema_sha256,
        extractor_version,
        model_id,
        created_at,
        values: Vec::new(),
        next_cursor: None,
    };
    // The envelope with the longest cursor, then each value and its comma.
    let mut used = serde_json::to_vec(&Extraction {
        next_cursor: Some("v99".into()),
        ..page.clone_head()
    })
    .map_or(PAGE_BYTES, |v| v.len());
    for (ord, raw) in rows {
        // A stored value that breaks the contract is the OS's failure, as
        // is one that cannot fit a page: never sent as if it were fine.
        let broken = |why: String| {
            ApiError::internal(format!(
                "extraction {} value {ord}: {why}",
                page.extraction_id
            ))
        };
        let value: Value = serde_json::from_str(&raw).map_err(|e| broken(e.to_string()))?;
        if has_null(&value) {
            return Err(broken("a null".into()));
        }
        let typed: ExtractedValue =
            serde_json::from_value(value.clone()).map_err(|e| broken(e.to_string()))?;
        typed.check().map_err(broken)?;
        // Counted as it will be sent, not as stored.
        let size = serde_json::to_string(&value)
            .map_err(|e| broken(e.to_string()))?
            .len()
            + 1;
        if page.values.len() == limit || used + size > PAGE_BYTES {
            if page.values.is_empty() {
                return Err(broken(format!("{} bytes do not fit a page", raw.len())));
            }
            page.next_cursor = Some(format!("v{ord}"));
            break;
        }
        used += size;
        page.values.push(value);
    }
    Ok(Json(page))
}

impl Extraction {
    /// The same extraction with no values: for sizing the envelope.
    fn clone_head(&self) -> Self {
        Self {
            extraction_id: self.extraction_id.clone(),
            document_id: self.document_id.clone(),
            revision: self.revision,
            content_sha256: self.content_sha256.clone(),
            schema_sha256: self.schema_sha256.clone(),
            extractor_version: self.extractor_version.clone(),
            model_id: self.model_id.clone(),
            created_at: self.created_at.clone(),
            values: Vec::new(),
            next_cursor: None,
        }
    }
}

mod value;

pub use value::{
    Anchor, Candidate, Engine, ExtractedValue, Geometry, MissingReason, Status, TextRange, has_null,
};

#[cfg(test)]
mod tests;
