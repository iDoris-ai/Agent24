//! `GET /documents/{id}/revisions/{rev}/text` (ADR-DOC-02 §3.1, §4;
//! `documentsReadText`): the blocks of the pinned layer, page by page.

use axum::Json;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path as UrlPath, Query, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    PAGE_BYTES, PAGES, Position, Revision, coverage, current_layer, cursor_layer, decode_cursor,
    encode_cursor, geometry, revision,
};
use crate::error::ApiError;
use crate::state::{AppState, Ready};
use crate::text_layer::{Block, EngineRef, ParseStatus, TextLayer, sha256_address};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;

fn block_json(b: &Block) -> Value {
    json!({
        "block_id": b.block_id, "page": b.page, "text": b.text,
        "text_sha256": sha256_address(b.text.as_bytes()),
        "geometry": geometry(b.lines.iter().map(|l| l.rect).collect()),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextParams {
    block: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
    #[serde(default)]
    require_complete: bool,
}

/// `DocumentsTextPage`.
#[derive(Serialize)]
pub struct TextPage {
    document_id: String,
    revision: i64,
    content_sha256: String,
    media_type: String,
    text_layer_sha256: String,
    engine: EngineRef,
    parse_status: ParseStatus,
    unparsed_regions: Vec<Value>,
    blocks: Vec<Value>,
    next_cursor: Option<String>,
}

pub async fn read_text(
    State(state): State<AppState>,
    Ready(storage): Ready,
    path: Result<UrlPath<(String, i64)>, PathRejection>,
    params: Result<Query<TextParams>, QueryRejection>,
) -> Result<Json<TextPage>, ApiError> {
    let UrlPath((id, rev)) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let Query(p) = params.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT);
    let both = p.block.is_some() && p.cursor.is_some();
    if !(1..=MAX_LIMIT).contains(&limit) || both || p.block.as_deref() == Some("") {
        return Err(ApiError::invalid_request(
            "limit is 1 to 200; a block id is not empty; give block or cursor, not both",
        ));
    }
    let r = revision(&storage, &id, rev).await?;
    let (address, layer, start) = match &p.cursor {
        Some(c) => {
            let bad = || ApiError::invalid_request("not a cursor for this read");
            let (address, at) = decode_cursor("t", &r, c, "").ok_or_else(bad)?;
            let layer = cursor_layer(&storage, &r, &address).await?;
            // A position this layer has: a page, and at most past its last block.
            let on_page = layer.blocks.iter().filter(|b| b.page == at.page).count();
            if at.page > layer.pages || at.block > on_page || at.hit != 0 {
                return Err(bad());
            }
            (address, layer, at)
        }
        None => {
            let (address, layer) = current_layer(&state, &storage, &r).await?;
            let at = match &p.block {
                None => Position {
                    page: 1,
                    block: 0,
                    hit: 0,
                },
                Some(b) => {
                    start_of(&layer, b).ok_or_else(|| ApiError::not_found("no such block"))?
                }
            };
            (address, layer, at)
        }
    };
    if p.require_complete && layer.parse_status == ParseStatus::Partial {
        return Err(ApiError::unprocessable(
            "partial_parse",
            "part of the document could not be read",
        ));
    }
    Ok(Json(page_of(&r, &address, &layer, start, limit)))
}

/// The position of block `id`, if the layer has it.
fn start_of(layer: &TextLayer, id: &str) -> Option<Position> {
    let b = layer.blocks.iter().find(|b| b.block_id == id)?;
    let block = layer
        .blocks
        .iter()
        .filter(|x| x.page == b.page)
        .position(|x| x.block_id == id)?;
    Some(Position {
        page: b.page,
        block,
        hit: 0,
    })
}

/// One response from `start`: whole pages' blocks until `limit` blocks, 512
/// KiB or [`PAGES`] pages; the cursor resumes at the first block left out.
fn page_of(
    r: &Revision,
    address: &str,
    layer: &TextLayer,
    start: Position,
    limit: usize,
) -> TextPage {
    let mut out = TextPage {
        document_id: r.document_id.clone(),
        revision: r.revision,
        content_sha256: r.content_sha256.clone(),
        media_type: r.media_type.clone(),
        text_layer_sha256: address.to_owned(),
        engine: layer.engine.clone(),
        parse_status: ParseStatus::Complete,
        unparsed_regions: Vec::new(),
        blocks: Vec::new(),
        next_cursor: None,
    };
    let full_cursor = encode_cursor(
        "t",
        r,
        address,
        Position {
            page: u32::MAX,
            block: usize::MAX,
            hit: 0,
        },
        "",
    );
    let last_page = layer.pages.min(start.page.saturating_add(PAGES - 1));
    // Room for the envelope, a cursor, and regions of every covered page.
    let (_, all_regions) = coverage(layer, start.page, last_page);
    let mut used = serde_json::to_vec(&out).map_or(PAGE_BYTES, |v| v.len())
        + full_cursor.len()
        + serde_json::to_vec(&all_regions).map_or(PAGE_BYTES, |v| v.len());
    let mut at = start;
    'pages: while at.page <= last_page {
        let blocks: Vec<&Block> = layer.blocks.iter().filter(|b| b.page == at.page).collect();
        while at.block < blocks.len() {
            let json = block_json(blocks[at.block]);
            let size = serde_json::to_vec(&json).map_or(PAGE_BYTES, |v| v.len()) + 1;
            if out.blocks.len() == limit || (used + size > PAGE_BYTES && !out.blocks.is_empty()) {
                break 'pages;
            }
            used += size;
            out.blocks.push(json);
            at.block += 1;
        }
        at = Position {
            page: at.page + 1,
            block: 0,
            hit: 0,
        };
    }
    // The pages it gave blocks of (or that have none): a page it stopped
    // at before its first block is the next response's.
    let end = if at.block == 0 { at.page - 1 } else { at.page };
    (out.parse_status, out.unparsed_regions) = coverage(layer, start.page, end.min(last_page));
    if at.page <= layer.pages {
        out.next_cursor = Some(encode_cursor("t", r, address, at, ""));
    }
    out
}

#[cfg(test)]
mod tests;
