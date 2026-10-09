//! The pinned text layer (ADR-DOC-02 §3, §3.1): what one engine build read
//! from one revision's bytes, stored as an immutable JSON blob and registered
//! in `text_layers` under `(content, engine, version, config)`. Reads and
//! anchors only ever use a stored layer, never a fresh parse.
//!
//! A layer is checked before it is stored and again when it is read, so
//! every block the routes serve fits the response caps and every byte of a
//! block's text belongs to exactly one line with a rectangle.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::blob::BlobError;
use crate::state::{Storage, blocking};

/// Block text, in UTF-8 bytes (§3.1): far below the 512 KiB response cap.
pub const MAX_BLOCK_BYTES: usize = 16 * 1024;
pub const MAX_LINES: usize = 64;
/// Rectangles in one page's unparsed region; more become the whole page.
pub const MAX_REGION_RECTS: usize = 16;
/// Why a region was not read: a short closed set (§3.1).
pub const REASONS: &[&str] = &["ocr_failed", "render_failed"];

/// `[x0, y0, x1, y1]` in CropBox points, origin top-left after `/Rotate`.
pub type Rect = [f64; 4];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextLayer {
    pub v: u32,
    /// The bytes it was read from: part of the layer, so two files that
    /// read the same never share a layer's address.
    pub content_sha256: String,
    pub engine: EngineRef,
    /// What the engine was asked to do; its hash is part of the layer's key.
    pub config: Value,
    /// Physical pages, blank ones included (1 for an image).
    pub pages: u32,
    pub parse_status: ParseStatus,
    pub unparsed_regions: Vec<Region>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineRef {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParseStatus {
    Complete,
    Partial,
}

/// A page, or part of one, that was not read (at most one per page).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Region {
    pub page: u32,
    pub rects: Vec<Rect>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {
    pub block_id: String,
    pub page: u32,
    pub text: String,
    pub lines: Vec<Line>,
}

/// One line of a block: its UTF-8 range in the block text (a line break
/// belongs to the line before it) and its rectangle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Line {
    pub start: usize,
    pub end: usize,
    pub rect: Rect,
}

fn sha256_address(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

/// The sha256 of a config's JSON. A config is a flat object of ASCII keys
/// with string, integer or boolean values ([`config_ok`]), whose JSON with
/// keys in order is canonical: no float or nesting to spell two ways.
#[must_use]
pub fn config_sha256(config: &Value) -> String {
    sha256_address(config.to_string().as_bytes())
}

#[must_use]
pub fn config_ok(config: &Value) -> bool {
    config.as_object().is_some_and(|m| {
        m.iter().all(|(k, v)| {
            !k.is_empty()
                && k.is_ascii()
                && (v.is_string() || v.is_boolean() || v.is_i64() || v.is_u64())
        })
    })
}

fn rect_ok(r: &Rect) -> bool {
    r.iter().all(|v| v.is_finite() && *v >= 0.0) && r[0] <= r[2] && r[1] <= r[3]
}

impl TextLayer {
    /// The hash of `config`, as `text_layers.config_sha256` keys it.
    #[must_use]
    pub fn config_sha256(&self) -> String {
        config_sha256(&self.config)
    }

    /// Why this layer breaks §3.1, if it does. Slice 1 reads paginated
    /// formats only (PDF, JPEG, PNG), so every block has a page.
    pub fn check(&self) -> Result<(), String> {
        if self.v != 1 || self.engine.id.is_empty() || self.engine.version.is_empty() {
            return Err("not a v1 layer from a named engine".into());
        }
        if !config_ok(&self.config) {
            return Err("config is not a flat object of ASCII keys and plain values".into());
        }
        if !crate::uploads::is_sha256_address(&self.content_sha256) || self.pages == 0 {
            return Err("no source content or no pages".into());
        }
        let partial = self.parse_status == ParseStatus::Partial;
        if partial == self.unparsed_regions.is_empty() {
            return Err("partial if and only if some region was not read".into());
        }
        let mut last_page = 0;
        for r in &self.unparsed_regions {
            if r.page <= last_page || r.page > self.pages {
                return Err(format!("region on page {}: one per page, in order", r.page));
            }
            last_page = r.page;
            if r.rects.is_empty()
                || r.rects.len() > MAX_REGION_RECTS
                || !r.rects.iter().all(rect_ok)
            {
                return Err(format!(
                    "region on page {}: 1 to {MAX_REGION_RECTS} valid rects",
                    r.page
                ));
            }
            if !REASONS.contains(&r.reason.as_str()) {
                return Err(format!("region on page {}: unknown reason", r.page));
            }
        }
        let (mut page, mut n) = (0, 0);
        for b in &self.blocks {
            if b.page < page || b.page == 0 || b.page > self.pages {
                return Err(format!("block {}: pages in reading order", b.block_id));
            }
            n = if b.page == page { n + 1 } else { 1 };
            page = b.page;
            if b.block_id != format!("p{page}/b{n}") {
                return Err(format!("block {}: expected p{page}/b{n}", b.block_id));
            }
            check_block(b).map_err(|why| format!("block {}: {why}", b.block_id))?;
        }
        Ok(())
    }
}

/// Text within the cap, 1 to 64 lines that cover it end to end on character
/// boundaries, each with a valid rectangle. A line break belongs to the line
/// it ends (§3.1), so a `\n` is only ever a line's last byte; a line split
/// for length ends without one.
fn check_block(b: &Block) -> Result<(), String> {
    if b.text.is_empty() || b.text.len() > MAX_BLOCK_BYTES {
        return Err(format!("text must be 1 to {MAX_BLOCK_BYTES} bytes"));
    }
    if b.lines.is_empty() || b.lines.len() > MAX_LINES {
        return Err(format!("1 to {MAX_LINES} lines"));
    }
    let mut at = 0;
    for l in &b.lines {
        if l.start != at || l.end <= l.start || !b.text.is_char_boundary(l.end) {
            return Err(format!(
                "line {}..{} does not continue at {at}",
                l.start, l.end
            ));
        }
        // Both ends are character boundaries by now, so this slice is safe.
        let line = &b.text[l.start..l.end];
        if line.strip_suffix('\n').unwrap_or(line).contains('\n') {
            return Err(format!(
                "line {}..{}: a line break inside it",
                l.start, l.end
            ));
        }
        if !rect_ok(&l.rect) {
            return Err(format!("line {}..{}: invalid rect", l.start, l.end));
        }
        at = l.end;
    }
    if at != b.text.len() {
        return Err("lines do not cover the text".into());
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum LayerError {
    #[error("text layer breaks §3.1: {0}")]
    Invalid(String),
    #[error(transparent)]
    Blob(#[from] BlobError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Stores `layer` for `content_sha256` and returns its address: the layer
/// already pinned under the same key if there is one (first one wins, since
/// anchors may point at it), else this one.
pub async fn pin(
    storage: &Arc<Storage>,
    content_sha256: &str,
    layer: &TextLayer,
) -> Result<String, LayerError> {
    layer.check().map_err(LayerError::Invalid)?;
    if layer.content_sha256 != content_sha256 {
        return Err(LayerError::Invalid(
            "the layer was read from other content".into(),
        ));
    }
    let config = layer.config_sha256();
    let bytes = serde_json::to_vec(layer).map_err(|e| LayerError::Invalid(e.to_string()))?;
    let store = storage.clone();
    let blob = blocking(move || store.blobs.put_bytes(&bytes)).await?;
    sqlx::query(
        "INSERT INTO text_layers (text_layer_sha256, content_sha256, engine_id, engine_version, config_sha256)
         VALUES (?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
    )
    .bind(&blob.sha256)
    .bind(content_sha256)
    .bind(&layer.engine.id)
    .bind(&layer.engine.version)
    .bind(&config)
    .execute(storage.db.pool())
    .await?;
    // A layer pinned under this key before (or by a racing pin) wins; this
    // blob, then unreferenced, goes with the blob GC (§2, Q2).
    pinned(storage, content_sha256, &layer.engine, &config)
        .await?
        .ok_or_else(|| LayerError::Invalid(format!("{} was not registered", blob.sha256)))
}

/// The layer pinned for this content, engine build and config, if any.
pub async fn pinned(
    storage: &Storage,
    content_sha256: &str,
    engine: &EngineRef,
    config_sha256: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT text_layer_sha256 FROM text_layers
         WHERE content_sha256 = ? AND engine_id = ? AND engine_version = ? AND config_sha256 = ?",
    )
    .bind(content_sha256)
    .bind(&engine.id)
    .bind(&engine.version)
    .bind(config_sha256)
    .fetch_optional(storage.db.pool())
    .await
}

/// A stored layer, re-hashed and re-checked.
pub async fn load(
    storage: &Arc<Storage>,
    text_layer_sha256: &str,
) -> Result<TextLayer, LayerError> {
    let (store, address) = (storage.clone(), text_layer_sha256.to_owned());
    let bytes = blocking(move || store.blobs.read_verified(&address)).await?;
    let layer: TextLayer =
        serde_json::from_slice(&bytes).map_err(|e| LayerError::Invalid(e.to_string()))?;
    layer.check().map_err(LayerError::Invalid)?;
    Ok(layer)
}

pub mod build;

#[cfg(test)]
mod tests;
