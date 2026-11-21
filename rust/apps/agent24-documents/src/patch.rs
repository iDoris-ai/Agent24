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
    pub(crate) index: usize,
    pub(crate) text: String,
    in_table: bool,
}

#[derive(Debug, Serialize)]
pub struct Paragraphs {
    document_id: String,
    revision: i64,
    pub(crate) paragraphs: Vec<Paragraph>,
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
    if bytes.len() > CONTENT_MAX {
        return Err(ApiError::payload_too_large(CONTENT_MAX));
    }
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
    paragraphs_of(&storage, id).await.map(Json)
}

/// The service behind `GET …/paragraphs` and the `paragraphs` tool.
pub async fn paragraphs_of(storage: &Arc<Storage>, id: String) -> Result<Paragraphs, ApiError> {
    let (revision, _, bytes) = head_docx(storage, &id).await?;
    let listed = blocking(move || docx_patch::list_paragraphs(&bytes))
        .await
        .map_err(patch_failure)?;
    Ok(Paragraphs {
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
    })
}

pub async fn patch(
    State(state): State<AppState>,
    path: Result<UrlPath<String>, PathRejection>,
    body: Result<Json<PatchRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let UrlPath(id) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let Json(req) = body.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let storage = state
        .storage()
        .await
        .map_err(ApiError::storage_unavailable)?;
    patch_run(&storage, id, req).await
}

/// The service behind `POST …/patches` and the `patch` tool.
pub async fn patch_run(
    storage: &Arc<Storage>,
    id: String,
    req: PatchRequest,
) -> Result<(StatusCode, Json<Job>), ApiError> {
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
    let (_, original_title, bytes) = head_docx(storage, &id).await?;
    let title = req
        .title
        .unwrap_or_else(|| derived_title(original_title, "（修订）"));
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
    let now = now_utc(storage).await?;
    // Checked before a new upload is made: a refused patch leaves nothing behind.
    let patched = blocking(move || docx_patch::apply(&bytes, &edits, &author, &now))
        .await
        .map_err(patch_failure)?;
    import_docx(storage, title, patched).await
}

/// The time now in UTC, ISO 8601 (a revision's w:date, core.xml's modified).
async fn now_utc(storage: &Arc<Storage>) -> Result<String, ApiError> {
    match sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now')")
        .fetch_one(storage.db.pool())
        .await
    {
        Ok(now) => Ok(now),
        Err(e) => Err(storage.db_failure(e).await),
    }
}

/// `<original title, extension aside><suffix>`, at most 500 characters.
fn derived_title(original: Option<String>, suffix: &str) -> String {
    let base = original.unwrap_or_else(|| "文档".to_owned());
    let stem = base
        .strip_suffix(".docx")
        .or_else(|| base.strip_suffix(".DOCX"))
        .unwrap_or(&base)
        .to_owned();
    format!("{stem}{suffix}").chars().take(500).collect()
}

/// The new DOCX imported as a document of its own.
async fn import_docx(
    storage: &Arc<Storage>,
    title: String,
    bytes: Vec<u8>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    crate::derived::import_derived(storage, Some(title), "docx", move |out| {
        std::fs::write(out, &bytes).map_err(|e| ApiError::storage_unavailable(io_cause(&e)))
    })
    .await
}

// ── D5a: template fields ──

#[derive(Debug, Serialize)]
pub struct Field {
    name: String,
    count: usize,
}

#[derive(Debug, Serialize)]
pub struct Fields {
    document_id: String,
    revision: i64,
    fields: Vec<Field>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FillRequest {
    values: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    title: Option<String>,
}

pub async fn fields(
    State(state): State<AppState>,
    path: Result<UrlPath<String>, PathRejection>,
) -> Result<Json<Fields>, ApiError> {
    let UrlPath(id) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let storage = state
        .storage()
        .await
        .map_err(ApiError::storage_unavailable)?;
    fields_of(&storage, id).await.map(Json)
}

/// The service behind `GET …/fields` and the `fields` tool.
pub async fn fields_of(storage: &Arc<Storage>, id: String) -> Result<Fields, ApiError> {
    let (revision, _, bytes) = head_docx(storage, &id).await?;
    let listed = blocking(move || docx_patch::list_fields(&bytes))
        .await
        .map_err(patch_failure)?;
    Ok(Fields {
        document_id: id,
        revision,
        fields: listed
            .into_iter()
            .map(|f| Field {
                name: f.name,
                count: f.count,
            })
            .collect(),
    })
}

pub async fn fill(
    State(state): State<AppState>,
    path: Result<UrlPath<String>, PathRejection>,
    body: Result<Json<FillRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let UrlPath(id) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let Json(req) = body.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let storage = state
        .storage()
        .await
        .map_err(ApiError::storage_unavailable)?;
    fill_run(&storage, id, req).await
}

/// The service behind `POST …/fill` and the `fill` tool: a new document of
/// the template with its fields filled.
pub async fn fill_run(
    storage: &Arc<Storage>,
    id: String,
    req: FillRequest,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    if req.values.is_empty() || req.values.len() > MAX_EDITS {
        return Err(ApiError::invalid_request(format!(
            "1 to {MAX_EDITS} values are needed"
        )));
    }
    if req
        .values
        .iter()
        .any(|(k, v)| k.chars().count() > 64 || v.chars().count() > TEXT_MAX)
    {
        return Err(ApiError::invalid_request(
            "a field name or value is too long",
        ));
    }
    if req
        .title
        .as_deref()
        .is_some_and(|t| !(1..=500).contains(&t.chars().count()))
    {
        return Err(ApiError::invalid_request(
            "title must be 1 to 500 characters",
        ));
    }
    let (_, original_title, bytes) = head_docx(storage, &id).await?;
    let title = req
        .title
        .unwrap_or_else(|| derived_title(original_title, "（填写）"));
    let now = now_utc(storage).await?;
    let values = req.values;
    let filled = blocking(move || docx_patch::fill(&bytes, &values, &now))
        .await
        .map_err(patch_failure)?;
    import_docx(storage, title, filled).await
}

#[cfg(test)]
mod tests;

// ── D5a: the head revision's file, to be saved where the user wants it ──

/// The largest file sent back whole.
const CONTENT_MAX: usize = 50 * 1024 * 1024;

/// `GET /documents/{id}/content`: the head revision's bytes as imported, with
/// its media type, so a filled template or patched document can be saved as a
/// file (the desktop's 「保存到下载」, `agent24 documents save`).
pub async fn content(
    State(state): State<AppState>,
    path: Result<UrlPath<String>, PathRejection>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let UrlPath(id) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    if !is_id(IdKind::Document, &id) {
        return Err(ApiError::invalid_request("not a document id"));
    }
    let storage = state
        .storage()
        .await
        .map_err(ApiError::storage_unavailable)?;
    let head: Option<i64> =
        match sqlx::query_scalar("SELECT head_revision FROM documents WHERE id = ?")
            .bind(&id)
            .fetch_optional(storage.db.pool())
            .await
        {
            Ok(row) => row,
            Err(e) => return Err(storage.db_failure(e).await),
        };
    let head = head.ok_or_else(|| ApiError::not_found("no such document"))?;
    let revision = crate::read::revision(&storage, &id, head).await?;
    let blobs = storage.clone();
    let sha = revision.content_sha256.clone();
    let bytes = blocking(move || blobs.blobs.read_verified(&sha))
        .await
        .map_err(|e| ApiError::storage_unavailable(blob_cause(&e)))?;
    if bytes.len() > CONTENT_MAX {
        return Err(ApiError::payload_too_large(CONTENT_MAX));
    }
    Ok((
        [(axum::http::header::CONTENT_TYPE, revision.media_type)],
        bytes,
    )
        .into_response())
}
