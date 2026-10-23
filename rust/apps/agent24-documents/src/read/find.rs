//! `POST /documents/{id}/find` (ADR-DOC-02 §3.1, §4; `documentsFind`):
//! matches of a query in one revision's pinned text layer, each an anchor.
//! Matching is within a block, in the stored text, relaxed only where the
//! offsets stay the text's own: ASCII case, a run of whitespace for a run of
//! whitespace, and a line break that may be skipped (Chinese wraps without
//! a space).

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path as UrlPath, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::matcher::Query;
use super::{
    PAGE_BYTES, PAGES, Position, Revision, coverage, current_layer, cursor_layer, decode_cursor,
    encode_cursor, geometry, revision,
};
use crate::error::ApiError;
use crate::state::{AppState, Ready};
use crate::text_layer::{Block, ParseStatus, TextLayer, sha256_address};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
const MAX_QUERY_CHARS: usize = 500;

/// `DocumentsFindRequest`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindRequest {
    revision: i64,
    query: String,
    cursor: Option<String>,
    limit: Option<usize>,
    #[serde(default)]
    require_complete: bool,
}

/// `DocumentsFindResult`.
#[derive(Serialize)]
pub struct FindResult {
    parse_status: ParseStatus,
    unparsed_regions: Vec<Value>,
    matches: Vec<Value>,
    next_cursor: Option<String>,
}

pub async fn find(
    State(state): State<AppState>,
    Ready(storage): Ready,
    path: Result<UrlPath<String>, PathRejection>,
    body: Result<Json<FindRequest>, JsonRejection>,
) -> Result<Json<FindResult>, ApiError> {
    let UrlPath(id) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let Json(req) = body.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let limit = req.limit.unwrap_or(DEFAULT_LIMIT);
    let chars = req.query.chars().count();
    let bad = || {
        ApiError::invalid_request(
            "limit is 1 to 200; the query is 1 to 500 characters, not only spaces",
        )
    };
    // Sizes first: compiling allocates per distinct character.
    if !(1..=MAX_LIMIT).contains(&limit) || !(1..=MAX_QUERY_CHARS).contains(&chars) {
        return Err(bad());
    }
    let query = Query::new(&req.query).ok_or_else(bad)?;
    let find = |text: &str| query.find(&req.query, text);
    // The cursor holds the query's hash: bound to it, and short.
    let bound = sha256_address(req.query.as_bytes());
    let r = revision(&storage, &id, req.revision).await?;
    let (address, layer, start) = match &req.cursor {
        Some(c) => {
            let bad = || ApiError::invalid_request("not a cursor for this find");
            let (address, at) = decode_cursor("f", &r, c, &bound).ok_or_else(bad)?;
            let layer = cursor_layer(&storage, &r, &address).await?;
            let on_page: Vec<&Block> = layer.blocks.iter().filter(|b| b.page == at.page).collect();
            let hits = on_page.get(at.block).map_or(0, |b| find(&b.text).len());
            if at.page > layer.pages || at.block > on_page.len() || (at.hit > 0 && at.hit >= hits) {
                return Err(bad());
            }
            (address, layer, at)
        }
        None => {
            let (address, layer) = current_layer(&state, &storage, &r).await?;
            (
                address,
                layer,
                Position {
                    page: 1,
                    block: 0,
                    hit: 0,
                },
            )
        }
    };
    if req.require_complete && layer.parse_status == ParseStatus::Partial {
        return Err(ApiError::unprocessable(
            "partial_parse",
            "part of the document could not be read",
        ));
    }
    let next = |at: Position| encode_cursor("f", &r, &address, at, &bound);
    Ok(Json(page_of(
        &r, &address, &layer, &find, start, limit, next,
    )))
}

/// The anchor of `text[start..end]` in `b` (`DocumentsAnchor`).
fn anchor(
    r: &Revision,
    address: &str,
    layer: &TextLayer,
    b: &Block,
    (start, end): (usize, usize),
) -> Value {
    let rects: Vec<[f64; 4]> = b
        .lines
        .iter()
        .filter(|l| l.start < end && l.end > start)
        .map(|l| l.rect)
        .collect();
    json!({
        "document_id": r.document_id, "revision": r.revision, "content_sha256": r.content_sha256,
        "media_type": r.media_type, "text_layer_sha256": address, "engine": layer.engine,
        "block_id": b.block_id, "page": b.page, "block_text_sha256": sha256_address(b.text.as_bytes()),
        "text_range": { "unit": "utf8", "start": start, "end": end },
        "geometry": geometry(rects), "quote": &b.text[start..end],
    })
}

/// One response from `start`: matches page by page until `limit`, 512 KiB
/// or [`PAGES`] pages; the cursor resumes at the first match left out.
fn page_of(
    r: &Revision,
    address: &str,
    layer: &TextLayer,
    find: &dyn Fn(&str) -> Vec<(usize, usize)>,
    start: Position,
    limit: usize,
    next: impl Fn(Position) -> String,
) -> FindResult {
    let mut out = FindResult {
        parse_status: ParseStatus::Complete,
        unparsed_regions: Vec::new(),
        matches: Vec::new(),
        next_cursor: None,
    };
    let last_page = layer.pages.min(start.page.saturating_add(PAGES - 1));
    let (_, all_regions) = coverage(layer, start.page, last_page);
    let full_cursor = next(Position {
        page: u32::MAX,
        block: usize::MAX,
        hit: usize::MAX,
    });
    let mut used = serde_json::to_vec(&out).map_or(PAGE_BYTES, |v| v.len())
        + full_cursor.len()
        + serde_json::to_vec(&all_regions).map_or(PAGE_BYTES, |v| v.len());
    let mut at = start;
    'pages: while at.page <= last_page {
        let blocks: Vec<&Block> = layer.blocks.iter().filter(|b| b.page == at.page).collect();
        while at.block < blocks.len() {
            let found = find(&blocks[at.block].text);
            while at.hit < found.len() {
                let json = anchor(r, address, layer, blocks[at.block], found[at.hit]);
                let size = serde_json::to_vec(&json).map_or(PAGE_BYTES, |v| v.len()) + 1;
                if out.matches.len() == limit
                    || (used + size > PAGE_BYTES && !out.matches.is_empty())
                {
                    break 'pages;
                }
                used += size;
                out.matches.push(json);
                at.hit += 1;
            }
            at = Position {
                block: at.block + 1,
                hit: 0,
                ..at
            };
        }
        at = Position {
            page: at.page + 1,
            block: 0,
            hit: 0,
        };
    }
    // The pages it searched: one it stopped at before searching is the next's.
    let end = if at.block == 0 && at.hit == 0 {
        at.page - 1
    } else {
        at.page
    };
    (out.parse_status, out.unparsed_regions) = coverage(layer, start.page, end.min(last_page));
    if at.page <= layer.pages {
        out.next_cursor = Some(next(at));
    }
    out
}

#[cfg(test)]
mod tests;
