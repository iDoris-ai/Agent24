//! L6-F5: a document's versions (ADR-DOC-02 §2.1, §10, as revised for F5).
//!
//! A rewrite or a filled template may be saved as the next revision of the
//! document itself, rather than as a new document. [`commit`] writes it:
//! the bytes into the blob store, then, in one `BEGIN IMMEDIATE`
//! transaction, the revision `head + 1` (origin `commit`), the head moved
//! to it, and an `oplog` row. It is a compare-and-set: the caller says which
//! head it read (`expect_revision`); any other head is a 409
//! `revision_conflict` with `current_revision`, and nothing is written, so
//! a write never lands on a version its author did not see.
//!
//! Revisions are immutable (0001's triggers): restoring an old version
//! ([`restore`]) commits its bytes again as a new revision; history is never
//! rewritten and every version stays.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::ApiError;
use crate::id::{IdKind, is_id};
use crate::oplog::{self, Origin};
use crate::state::{AppState, Storage, blob_cause, blocking};

/// Where a rewrite or a filled template is saved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveTo {
    /// A document of its own (the default, as before F5).
    #[default]
    NewDocument,
    /// The next revision of the document it was made from.
    NewRevision,
}

/// One version, as the list shows it.
#[derive(Debug, Serialize, PartialEq, sqlx::FromRow)]
pub struct Version {
    pub revision: i64,
    pub content_sha256: String,
    pub size: i64,
    pub media_type: String,
    /// `import` (r1) or `commit`.
    pub origin: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Versions {
    pub document_id: String,
    pub head_revision: i64,
    /// Newest first.
    pub revisions: Vec<Version>,
}

/// What a commit made: `revision` is the new head; `committed` is false
/// when the content was already the head's and nothing was written.
#[derive(Debug, Serialize, PartialEq)]
pub struct Committed {
    pub document_id: String,
    pub title: String,
    pub revision: i64,
    pub previous_revision: i64,
    pub committed: bool,
}

/// The versions of a live document, newest first.
pub async fn list(storage: &Storage, id: &str) -> Result<Versions, ApiError> {
    if !is_id(IdKind::Document, id) {
        return Err(ApiError::invalid_request("not a document id"));
    }
    let done: Result<Option<Versions>, sqlx::Error> = async {
        let head: Option<i64> = sqlx::query_scalar(
            "SELECT head_revision FROM documents WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(storage.db.pool())
        .await?;
        let Some(head_revision) = head else {
            return Ok(None);
        };
        let revisions = sqlx::query_as(
            "SELECT revision, content_sha256, size, media_type, origin, created_at
             FROM revisions WHERE document_id = ? ORDER BY revision DESC",
        )
        .bind(id)
        .fetch_all(storage.db.pool())
        .await?;
        Ok(Some(Versions {
            document_id: id.to_owned(),
            head_revision,
            revisions,
        }))
    }
    .await;
    match done {
        Ok(Some(v)) => Ok(v),
        Ok(None) => Err(ApiError::not_found("no such document")),
        Err(e) => Err(storage.db_failure(e).await),
    }
}

pub(crate) fn conflict(current: i64) -> ApiError {
    ApiError::conflict(
        "revision_conflict",
        format!("the document is at version {current} now; read it again"),
    )
    .with_detail("current_revision", current)
}

enum Outcome {
    Done(Committed),
    Gone,
    Moved(i64),
    WrongType,
}

/// Commits `bytes` as revision `expect + 1` of `id`, if its head is still
/// `expect` (see the module docs), in format `kind`, which must be the
/// document's own. `op` and `details` go to the `oplog`.
pub struct Commit<'a> {
    pub expect: i64,
    pub bytes: Vec<u8>,
    pub kind: &'a str,
    pub origin: &'a Origin,
    pub op: &'a str,
    pub details: serde_json::Value,
}

pub async fn commit(
    storage: &Arc<Storage>,
    id: &str,
    c: Commit<'_>,
) -> Result<Committed, ApiError> {
    let Commit {
        expect,
        bytes,
        kind,
        origin,
        op,
        details,
    } = c;
    if !is_id(IdKind::Document, id) || expect < 1 {
        return Err(ApiError::invalid_request(
            "a document id and expect_revision (1 or more) are needed",
        ));
    }
    let size = i64::try_from(bytes.len()).map_err(|_| ApiError::payload_too_large(0))?;
    // The bytes first (§2.2's write order): a blob no revision names is
    // harmless; a revision without its blob is not.
    let blobs = storage.clone();
    let blob = blocking(move || blobs.blobs.put_bytes(&bytes))
        .await
        .map_err(|e| ApiError::storage_unavailable(blob_cause(&e)))?;
    let done: Result<Outcome, sqlx::Error> = async {
        let mut tx = storage.db.pool().begin_with("BEGIN IMMEDIATE").await?;
        let row: Option<(i64, String, String, String)> = sqlx::query_as(
            "SELECT d.head_revision, d.title, d.media_type, r.content_sha256
             FROM documents d JOIN revisions r ON r.document_id = d.id AND r.revision = d.head_revision
             WHERE d.id = ? AND d.deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((head, title, media_type, head_sha)) = row else {
            return Ok(Outcome::Gone);
        };
        if head != expect {
            return Ok(Outcome::Moved(head));
        }
        if kind != media_type {
            return Ok(Outcome::WrongType);
        }
        if head_sha == blob.sha256 {
            return Ok(Outcome::Done(Committed {
                document_id: id.to_owned(),
                title,
                revision: head,
                previous_revision: head,
                committed: false,
            }));
        }
        let next = head + 1;
        sqlx::query(
            "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
             VALUES (?, ?, ?, ?, ?, 'commit')",
        )
        .bind(id)
        .bind(next)
        .bind(&blob.sha256)
        .bind(size)
        .bind(&media_type)
        .execute(&mut *tx)
        .await?;
        let moved = sqlx::query(
            "UPDATE documents SET head_revision = ?,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ? AND head_revision = ?",
        )
        .bind(next)
        .bind(id)
        .bind(head)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if moved != 1 {
            // Under BEGIN IMMEDIATE nothing else writes; dropping rolls back.
            return Ok(Outcome::Moved(head));
        }
        oplog::append(&mut tx, op, id, next, origin, &details).await?;
        tx.commit().await?;
        Ok(Outcome::Done(Committed {
            document_id: id.to_owned(),
            title,
            revision: next,
            previous_revision: head,
            committed: true,
        }))
    }
    .await;
    match done {
        Ok(Outcome::Done(c)) => Ok(c),
        Ok(Outcome::Gone) => Err(ApiError::not_found("no such document")),
        Ok(Outcome::Moved(head)) => Err(conflict(head)),
        Ok(Outcome::WrongType) => Err(ApiError::unprocessable(
            "unsupported_format",
            "a new version must be in the document's own format",
        )),
        Err(e) => Err(storage.db_failure(e).await),
    }
}

/// Makes old revision `from` the current version again: its bytes committed
/// as a new revision (history is kept), if the head is still `expect`.
pub async fn restore(
    storage: &Arc<Storage>,
    id: &str,
    from: i64,
    expect: i64,
    origin: &Origin,
) -> Result<Committed, ApiError> {
    if !crate::trash::is_live(storage, id).await? {
        return Err(ApiError::not_found("no such document"));
    }
    let old = crate::read::revision(storage, id, from).await?;
    let blobs = storage.clone();
    let sha = old.content_sha256.clone();
    let bytes = blocking(move || blobs.blobs.read_verified(&sha))
        .await
        .map_err(|e| ApiError::storage_unavailable(blob_cause(&e)))?;
    let c = Commit {
        expect,
        bytes,
        kind: &old.media_type,
        origin,
        op: "restore_revision",
        details: json!({ "restored_from": from }),
    };
    commit(storage, id, c).await
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreRequest {
    expect_revision: i64,
}

/// `GET /documents/{document_id}/revisions`.
pub async fn list_revisions(
    State(state): State<AppState>,
    path: Result<UrlPath<String>, PathRejection>,
) -> Result<Json<Versions>, ApiError> {
    let UrlPath(id) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let storage = state
        .storage()
        .await
        .map_err(ApiError::storage_unavailable)?;
    list(&storage, &id).await.map(Json)
}

/// `POST /documents/{document_id}/revisions/{revision}/restore`, from the page.
pub async fn restore_revision(
    State(state): State<AppState>,
    path: Result<UrlPath<(String, i64)>, PathRejection>,
    body: Result<Json<RestoreRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Committed>), ApiError> {
    let UrlPath((id, from)) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let Json(req) = body.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let storage = state
        .storage()
        .await
        .map_err(ApiError::storage_unavailable)?;
    let done = restore(&storage, &id, from, req.expect_revision, &Origin::Page).await?;
    Ok((status_of(&done), Json(done)))
}

/// 201 for a new revision, 200 when nothing was written.
#[must_use]
pub fn status_of(c: &Committed) -> StatusCode {
    if c.committed {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    }
}

#[cfg(test)]
mod tests;
