//! E1b: the HTTP side of [`crate::docx_patch`].
//!
//! `GET /documents/{id}/paragraphs`: the head revision's paragraphs, by index.
//! `POST /documents/{id}/patches`: those paragraphs edited (plainly, or as
//! tracked changes) in a NEW document; the original is never changed. The
//! answer is the new document's import job, as `POST /compositions` answers.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::docx_patch::{self, MAX_EDITS, ParaEdit, PatchError};
use crate::error::ApiError;
use crate::id::{IdKind, is_id};
use crate::jobs::Job;
use crate::office::DOCX;
use crate::state::{AppState, Storage, blob_cause, blocking, io_cause};

/// A new paragraph text, at most.
const TEXT_MAX: usize = 100_000;

#[derive(Debug, Serialize)]
pub struct Paragraph {
    index: usize,
    text: String,
    in_table: bool,
}

#[derive(Debug, Serialize)]
pub struct Paragraphs {
    document_id: String,
    revision: i64,
    paragraphs: Vec<Paragraph>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditBody {
    index: usize,
    text: String,
    #[serde(default)]
    tracked: bool,
    #[serde(default)]
    expect: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchRequest {
    edits: Vec<EditBody>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    author: Option<String>,
}

fn patch_failure(e: PatchError) -> ApiError {
    match e {
        PatchError::NotDocx(why) => ApiError::unprocessable("parse_failed", why),
        PatchError::TooLarge => {
            ApiError::unprocessable("payload_too_large", "the document is too large to edit")
        }
        PatchError::IndexOutOfRange(_) | PatchError::Invalid(_) => {
            ApiError::invalid_request(e.to_string())
        }
        PatchError::Stale(i) => ApiError::unprocessable(
            "revision_conflict",
            format!("paragraph {i} has changed since it was read; read the paragraphs again"),
        ),
    }
}

/// The document's head revision as DOCX bytes, with its number and title.
async fn head_docx(
    storage: &Arc<Storage>,
    id: &str,
) -> Result<(i64, Option<String>, Vec<u8>), ApiError> {
    if !is_id(IdKind::Document, id) {
        return Err(ApiError::invalid_request("not a document id"));
    }
    let row: Option<(i64, Option<String>)> =
        match sqlx::query_as("SELECT head_revision, title FROM documents WHERE id = ?")
            .bind(id)
            .fetch_optional(storage.db.pool())
            .await
        {
            Ok(row) => row,
            Err(e) => return Err(storage.db_failure(e).await),
        };
    let (head, title) = row.ok_or_else(|| ApiError::not_found("no such document"))?;
    let revision = crate::read::revision(storage, id, head).await?;
    if revision.media_type != DOCX {
        return Err(ApiError::unprocessable(
            "unsupported_format",
            "only Word (DOCX) documents can be edited paragraph by paragraph",
        ));
    }
    let blobs = storage.clone();
    let sha = revision.content_sha256.clone();
    let bytes = blocking(move || blobs.blobs.read_verified(&sha))
        .await
        .map_err(|e| ApiError::storage_unavailable(blob_cause(&e)))?;
    Ok((head, title, bytes))
}

pub async fn paragraphs(
    State(state): State<AppState>,
    path: Result<UrlPath<String>, PathRejection>,
) -> Result<Json<Paragraphs>, ApiError> {
    let UrlPath(id) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let storage = state
        .storage()
        .await
        .map_err(ApiError::storage_unavailable)?;
    let (revision, _, bytes) = head_docx(&storage, &id).await?;
    let listed = blocking(move || docx_patch::list_paragraphs(&bytes))
        .await
        .map_err(patch_failure)?;
    Ok(Json(Paragraphs {
        document_id: id,
        revision,
        paragraphs: listed
            .into_iter()
            .map(|p| Paragraph {
                index: p.index,
                text: p.text,
                in_table: p.in_table,
            })
            .collect(),
    }))
}

pub async fn patch(
    State(state): State<AppState>,
    path: Result<UrlPath<String>, PathRejection>,
    body: Result<Json<PatchRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let UrlPath(id) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let Json(req) = body.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    if req.edits.is_empty() || req.edits.len() > MAX_EDITS {
        return Err(ApiError::invalid_request(format!(
            "1 to {MAX_EDITS} edits are needed"
        )));
    }
    if req.edits.iter().any(|e| e.text.chars().count() > TEXT_MAX) {
        return Err(ApiError::invalid_request(format!(
            "a paragraph text is longer than {TEXT_MAX} characters"
        )));
    }
    let title_ok = |t: &str| (1..=500).contains(&t.chars().count());
    if req.title.as_deref().is_some_and(|t| !title_ok(t)) {
        return Err(ApiError::invalid_request(
            "title must be 1 to 500 characters",
        ));
    }
    if req
        .author
        .as_deref()
        .is_some_and(|a| !(1..=100).contains(&a.chars().count()))
    {
        return Err(ApiError::invalid_request(
            "author must be 1 to 100 characters",
        ));
    }
    let storage = state
        .storage()
        .await
        .map_err(ApiError::storage_unavailable)?;
    let (_, original_title, bytes) = head_docx(&storage, &id).await?;
    let title = req.title.unwrap_or_else(|| {
        let base = original_title.unwrap_or_else(|| "文档".to_owned());
        let stem = base
            .strip_suffix(".docx")
            .or_else(|| base.strip_suffix(".DOCX"))
            .unwrap_or(&base)
            .to_owned();
        format!("{stem}（修订）").chars().take(500).collect()
    });
    let author = req.author.unwrap_or_else(|| "Agent24".to_owned());
    let edits: Vec<ParaEdit> = req
        .edits
        .into_iter()
        .map(|e| ParaEdit {
            index: e.index,
            text: e.text,
            tracked: e.tracked,
            expect: e.expect,
        })
        .collect();
    // The revisions' date, in UTC (w:date is an ISO 8601 time).
    let now: String = match sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now')")
        .fetch_one(storage.db.pool())
        .await
    {
        Ok(now) => now,
        Err(e) => return Err(storage.db_failure(e).await),
    };
    // Checked before a new upload is made: a refused patch leaves nothing behind.
    let patched = blocking(move || docx_patch::apply(&bytes, &edits, &author, &now))
        .await
        .map_err(patch_failure)?;
    crate::derived::import_derived(&storage, Some(title), "docx", move |out| {
        std::fs::write(out, &patched).map_err(|e| ApiError::storage_unavailable(io_cause(&e)))
    })
    .await
}

#[cfg(test)]
mod tests;
