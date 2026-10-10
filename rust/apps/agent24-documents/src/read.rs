//! Reading a revision's text layer (ADR-DOC-02 §3.1, §4): what `GET text`
//! and `find` share. The layer is the current engine's, built on demand (a
//! bounded wait, then 503 to retry); a cursor keeps reading the layer it was
//! issued for, and is bound to its revision.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::engine::{LayerFailure, WAIT, pdfkit};
use crate::error::ApiError;
use crate::id::{IdKind, is_id};
use crate::state::{AppState, Storage, blob_cause};
use crate::text_layer::{self, LayerError, ParseStatus, Region, TextLayer};

/// A response's serialized JSON stays under this (§4).
pub const PAGE_BYTES: usize = 512 * 1024;
/// Pages one response covers, at most (§3.1).
pub const PAGES: u32 = 100;

/// The revision a read is about, as stored.
pub struct Revision {
    pub document_id: String,
    pub revision: i64,
    pub content_sha256: String,
    pub media_type: String,
}

pub async fn revision(storage: &Storage, id: &str, rev: i64) -> Result<Revision, ApiError> {
    if !is_id(IdKind::Document, id) || rev < 1 {
        return Err(ApiError::invalid_request("not a document id or revision"));
    }
    let row: Result<Option<(String, String)>, _> = sqlx::query_as(
        "SELECT content_sha256, media_type FROM revisions WHERE document_id = ? AND revision = ?",
    )
    .bind(id)
    .bind(rev)
    .fetch_optional(storage.db.pool())
    .await;
    let row = match row {
        Ok(row) => row,
        Err(e) => return Err(storage.db_failure(e).await),
    };
    let (content_sha256, media_type) =
        row.ok_or_else(|| ApiError::not_found("no such document revision"))?;
    Ok(Revision {
        document_id: id.to_owned(),
        revision: rev,
        content_sha256,
        media_type,
    })
}

/// The current engine's layer of `r`, built if need be.
pub async fn current_layer(
    state: &AppState,
    storage: &Arc<Storage>,
    r: &Revision,
) -> Result<(String, TextLayer), ApiError> {
    if !pdfkit::FORMATS.contains(&r.media_type.as_str()) {
        return Err(ApiError::unprocessable(
            "unsupported_format",
            "slice 1 reads PDF, JPEG and PNG",
        ));
    }
    let address = state
        .layers()
        .layer(storage, &r.content_sha256, &r.media_type, WAIT)
        .await;
    let address = address.map_err(layer_error)?;
    let layer = load(storage, &address).await?;
    Ok((address, layer))
}

/// The answer for a layer that could not be had (§6).
fn layer_error(f: LayerFailure) -> ApiError {
    let engine = pdfkit::ENGINE_ID;
    match f {
        LayerFailure::NoEngine => {
            ApiError::engine_unavailable(engine, "no read engine on this platform")
        }
        LayerFailure::Busy => ApiError::engine_unavailable(engine, "the read engine is busy"),
        LayerFailure::Pending => {
            ApiError::engine_unavailable(engine, "the text layer is still being built")
        }
        LayerFailure::Engine(crate::engine::EngineError::Unsupported) => ApiError::unprocessable(
            "unsupported_format",
            "the read engine does not read this file",
        ),
        LayerFailure::Engine(e) => ApiError::unprocessable("parse_failed", e.to_string()),
        LayerFailure::Storage(Some(cause), _) => ApiError::storage_unavailable(cause),
        LayerFailure::Storage(None, why) => ApiError::internal(why),
    }
}

/// A pinned layer, re-checked; that it is broken is the OS's failure.
pub async fn load(storage: &Arc<Storage>, address: &str) -> Result<TextLayer, ApiError> {
    text_layer::load(storage, address)
        .await
        .map_err(|e| match e {
            // Lost, unreadable or corrupt bytes: storage, with its cause (§6).
            LayerError::Blob(b) => ApiError::storage_unavailable(blob_cause(&b)),
            other => ApiError::internal(format!("the text layer {address}: {other}")),
        })
}

/// Where a page of results starts: a page, and a block on it (and, for
/// `find`, a match in that block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub page: u32,
    pub block: usize,
    pub hit: usize,
}

/// Signs cursors: a key of this process, so a cursor cannot be edited or
/// made up, and one from before a restart is simply refused.
fn cursor_key() -> Option<&'static [u8; 64]> {
    static KEY: std::sync::OnceLock<[u8; 64]> = std::sync::OnceLock::new();
    if let Some(key) = KEY.get() {
        return Some(key);
    }
    let mut key = [0; 64];
    if getrandom::fill(&mut key).is_err() {
        // Kept unset, so it is tried again; meanwhile no cursor verifies.
        tracing::error!("documents: no randomness for cursor keys");
        return None;
    }
    Some(KEY.get_or_init(|| key))
}

/// HMAC-SHA-256 (RFC 2104) of `payload` under the cursor key, if there is one.
fn mac(payload: &[u8]) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let key = cursor_key()?;
    let pad = |b: u8| key.map(|k| k ^ b);
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(payload)
        .finalize();
    Some(
        Sha256::new()
            .chain_update(pad(0x5c))
            .chain_update(inner)
            .finalize()
            .into(),
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// An opaque cursor: `kind doc rev layer page block hit query`, signed.
pub fn encode_cursor(kind: &str, r: &Revision, layer: &str, at: Position, query: &str) -> String {
    let text = format!(
        "{kind} {} {} {layer} {} {} {} {query}",
        r.document_id, r.revision, at.page, at.block, at.hit
    );
    format!(
        "{}{}",
        hex(text.as_bytes()),
        // Without a key, a tag that never verifies.
        hex(&mac(text.as_bytes()).unwrap_or_default()[..16])
    )
}

/// The layer and position a cursor names, if this process signed it for
/// `kind`, this revision and `query`.
pub fn decode_cursor(
    kind: &str,
    r: &Revision,
    cursor: &str,
    query: &str,
) -> Option<(String, Position)> {
    if cursor.len() > 2048
        || cursor.len() < 32
        || !cursor.is_ascii()
        || !cursor.len().is_multiple_of(2)
    {
        return None;
    }
    let bytes: Vec<u8> = (0..cursor.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(cursor.get(i..i + 2)?, 16).ok())
        .collect::<Option<_>>()?;
    let (payload, tag) = bytes.split_at(bytes.len() - 16);
    if mac(payload)?[..16] != *tag {
        return None;
    }
    let text = std::str::from_utf8(payload).ok()?;
    let mut parts = text.splitn(8, ' ');
    let mut next = || parts.next();
    let (k, doc, rev, layer) = (next()?, next()?, next()?, next()?);
    let (page, block, hit, q) = (
        next()?.parse().ok()?,
        next()?.parse().ok()?,
        next()?.parse().ok()?,
        next()?,
    );
    let ours = k == kind && doc == r.document_id && rev == r.revision.to_string() && q == query;
    (ours && crate::uploads::is_sha256_address(layer) && page >= 1)
        .then(|| (layer.to_owned(), Position { page, block, hit }))
}

/// The layer a cursor names, if it was read from this revision's bytes.
pub async fn cursor_layer(
    storage: &Arc<Storage>,
    r: &Revision,
    address: &str,
) -> Result<TextLayer, ApiError> {
    let layer = load(storage, address).await?;
    if layer.content_sha256 != r.content_sha256 {
        return Err(ApiError::invalid_request(
            "the cursor is not for this revision",
        ));
    }
    Ok(layer)
}

/// `DocumentsGeometry`.
pub fn geometry(rects: Vec<[f64; 4]>) -> Value {
    json!({ "box": "CropBox", "unit": "pt", "origin": "top-left-rotated", "rects": rects })
}

/// `parse_status` and `unparsed_regions` for pages `first..=last` (§3.1).
pub fn coverage(layer: &TextLayer, first: u32, last: u32) -> (ParseStatus, Vec<Value>) {
    let regions: Vec<Value> = layer
        .unparsed_regions
        .iter()
        .filter(|r: &&Region| (first..=last).contains(&r.page))
        .map(|r| json!({ "page": r.page, "geometry": geometry(r.rects.clone()), "reason": r.reason }))
        .collect();
    let status = if regions.is_empty() {
        ParseStatus::Complete
    } else {
        ParseStatus::Partial
    };
    (status, regions)
}

#[cfg(test)]
mod tests;
