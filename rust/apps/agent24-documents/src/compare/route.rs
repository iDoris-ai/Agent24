//! `POST /documents/compare` (L6-S05): two documents, or two versions of one,
//! compared by their text. Read-only: nothing is written and no one is asked.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use serde::{Deserialize, Serialize};

use super::{BlockRef, Change, Comparison, Summary, Unit, compare as diff};
use crate::engine::Requester;
use crate::error::ApiError;
use crate::id::{IdKind, is_id};
use crate::office::DOCX;
use crate::state::{AppState, Ready, Storage, blob_cause, blocking};

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    document_id: String,
    /// Absent: the document's current version.
    revision: Option<i64>,
}

impl Target {
    pub(crate) fn new(document_id: String, revision: Option<i64>) -> Self {
        Self {
            document_id,
            revision,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompareRequest {
    left: Target,
    right: Target,
    /// Changes to return from `offset` (default 100, at most 500).
    limit: Option<usize>,
    offset: Option<usize>,
}

impl CompareRequest {
    pub(crate) fn new(left: Target, right: Target, limit: usize) -> Self {
        Self {
            left,
            right,
            limit: Some(limit),
            offset: None,
        }
    }
}

#[derive(Serialize)]
pub struct Compared {
    left: Resolved,
    right: Resolved,
    pub(crate) identical: bool,
    only_text: bool,
    pub(crate) summary: Summary,
    pub(crate) changes: Vec<Change>,
    /// `summary.total` is of all changes; `changes` holds a page of them.
    pub(crate) truncated: bool,
    next_offset: Option<usize>,
}

impl Compared {
    /// The versions compared: left's and right's.
    pub(crate) fn revisions(&self) -> (i64, i64) {
        (self.left.revision, self.right.revision)
    }
}

#[derive(Serialize)]
pub struct Resolved {
    document_id: String,
    revision: i64,
    media_type: String,
}

/// Word documents compare by paragraph; PDFs and images by text block.
fn kind_of(media_type: &str) -> &'static str {
    if media_type == DOCX { "docx" } else { "layer" }
}

async fn resolve(storage: &Storage, t: &Target) -> Result<crate::read::Revision, ApiError> {
    if !is_id(IdKind::Document, &t.document_id) {
        return Err(ApiError::invalid_request("not a document id"));
    }
    if !crate::trash::is_live(storage, &t.document_id).await? {
        return Err(ApiError::not_found("no such document"));
    }
    let rev = match t.revision {
        Some(r) => r,
        None => {
            let head: Result<Option<i64>, _> =
                sqlx::query_scalar("SELECT head_revision FROM documents WHERE id = ?")
                    .bind(&t.document_id)
                    .fetch_optional(storage.db.pool())
                    .await;
            match head {
                Ok(h) => h.ok_or_else(|| ApiError::not_found("no such document"))?,
                Err(e) => return Err(storage.db_failure(e).await),
            }
        }
    };
    crate::read::revision(storage, &t.document_id, rev).await
}

async fn units(
    state: &AppState,
    storage: &Arc<Storage>,
    r: &crate::read::Revision,
    by: Requester,
) -> Result<Vec<Unit>, ApiError> {
    if r.media_type == DOCX {
        let blobs = storage.clone();
        let sha = r.content_sha256.clone();
        let bytes = blocking(move || blobs.blobs.read_verified(&sha))
            .await
            .map_err(|e| ApiError::storage_unavailable(blob_cause(&e)))?;
        let listed = blocking(move || crate::docx_patch::list_paragraphs(&bytes))
            .await
            .map_err(|e| ApiError::unprocessable("parse_failed", e.to_string()))?;
        return Ok(listed
            .into_iter()
            .map(|p| Unit {
                block_ref: BlockRef::Paragraph {
                    paragraph_index: p.index,
                },
                text: p.text,
            })
            .collect());
    }
    let (_, layer) = crate::read::current_layer(state, storage, r, by).await?;
    Ok(layer
        .blocks
        .into_iter()
        .map(|b| Unit {
            block_ref: BlockRef::Block {
                block_id: b.block_id,
                page: b.page,
            },
            text: b.text,
        })
        .collect())
}

pub async fn compare(
    State(state): State<AppState>,
    Ready(storage): Ready,
    body: Result<Json<CompareRequest>, JsonRejection>,
) -> Result<Json<Compared>, ApiError> {
    let Json(req) = body.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    run(&state, &storage, req, Requester::Page).await.map(Json)
}

/// The comparison itself, for `POST /documents/compare` and the
/// `documents.compare` tool alike.
pub(crate) async fn run(
    state: &AppState,
    storage: &Arc<Storage>,
    req: CompareRequest,
    by: Requester,
) -> Result<Compared, ApiError> {
    let limit = req.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ApiError::invalid_request("limit is 1 to 500"));
    }
    let offset = req.offset.unwrap_or(0);
    let (l, r) = (
        resolve(storage, &req.left).await?,
        resolve(storage, &req.right).await?,
    );
    if kind_of(&l.media_type) != kind_of(&r.media_type) {
        return Err(ApiError::unprocessable(
            "unsupported_format",
            "only two Word documents, or two PDFs or images, can be compared with each other",
        ));
    }
    let (lu, ru) = (
        units(state, storage, &l, by.clone()).await?,
        units(state, storage, &r, by).await?,
    );
    let Comparison {
        identical,
        only_text,
        summary,
        changes,
    } = diff(&lu, &ru);
    let total = changes.len();
    let page: Vec<Change> = changes.into_iter().skip(offset).take(limit).collect();
    let end = offset.saturating_add(page.len());
    let resolved = |x: crate::read::Revision| Resolved {
        document_id: x.document_id,
        revision: x.revision,
        media_type: x.media_type,
    };
    Ok(Compared {
        left: resolved(l),
        right: resolved(r),
        identical,
        only_text,
        summary,
        changes: page,
        truncated: end < total,
        next_offset: (end < total).then_some(end),
    })
}

#[cfg(test)]
mod tests;
