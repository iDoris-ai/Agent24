//! L6-F2: soft delete (ADR-DOC-02 Q2, revised by David 2026-11-22).
//!
//! Deleting marks `documents.deleted_at`; the row, its revisions and their
//! blobs stay. A deleted document is not in `GET /documents`, not behind any
//! `/documents/{id}/…` route but `restore` ([`hide_deleted`]), not in a tool's
//! title lookup and not reachable by any tool (`tools::admit`). For
//! [`RESTORE_DAYS`] it is listed by `GET /recently-deleted` and can be
//! restored; after that it is gone for the user. Reclaiming its bytes needs a
//! blob GC, which DOC-1 does not have yet (FU-177).

use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path as UrlPath, Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::json;

use crate::documents::{self, DocumentDetail};
use crate::error::ApiError;
use crate::id::{IdKind, is_id};
use crate::oplog::{self, Origin};
use crate::state::{AppState, Ready, Storage};

/// How long a deleted document can be restored.
pub const RESTORE_DAYS: u32 = 7;
/// Deleted documents listed at most.
const MAX_LISTED: i64 = 200;
const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";

/// A deleted document, as delete answers and the recently-deleted list shows.
#[derive(Debug, Serialize, PartialEq, sqlx::FromRow)]
pub struct Deleted {
    #[sqlx(rename = "id")]
    pub document_id: String,
    pub title: String,
    pub media_type: String,
    pub created_at: String,
    pub deleted_at: String,
    pub restorable_until: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct DeletedList {
    pub documents: Vec<Deleted>,
}

/// The columns of [`Deleted`], from a row of `documents`.
fn deleted_columns() -> String {
    format!(
        "id, title, media_type, created_at, deleted_at,
         strftime('%Y-%m-%dT%H:%M:%fZ', deleted_at, '+{RESTORE_DAYS} days') AS restorable_until"
    )
}

/// Whether `id` names a document that is here and not deleted.
pub(crate) async fn is_live(storage: &Storage, id: &str) -> Result<bool, ApiError> {
    let found =
        sqlx::query_scalar::<_, i64>("SELECT 1 FROM documents WHERE id = ? AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(storage.db.pool())
            .await;
    match found {
        Ok(found) => Ok(found.is_some()),
        Err(e) => Err(storage.db_failure(e).await),
    }
}

/// Marks `id` deleted, with its `oplog` row, in one transaction. A document
/// that is not here, or already deleted, is not found.
pub(crate) async fn delete(
    storage: &Storage,
    id: &str,
    origin: &Origin,
) -> Result<Deleted, ApiError> {
    if !is_id(IdKind::Document, id) {
        return Err(ApiError::invalid_request("not a document id"));
    }
    let sql = format!(
        "UPDATE documents SET deleted_at = {NOW} WHERE id = ? AND deleted_at IS NULL
         RETURNING head_revision, {}",
        deleted_columns()
    );
    let done: Result<Option<Deleted>, sqlx::Error> = async {
        let mut tx = storage.db.pool().begin().await?;
        let Some((revision, deleted)) = sqlx::query_as::<_, RevisionAnd>(&sql)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .map(RevisionAnd::split)
        else {
            return Ok(None);
        };
        let details = json!({ "title": deleted.title });
        oplog::append(&mut tx, "delete", id, revision, origin, &details).await?;
        tx.commit().await?;
        Ok(Some(deleted))
    }
    .await;
    match done {
        Ok(Some(deleted)) => Ok(deleted),
        Ok(None) => Err(ApiError::not_found("no such document")),
        Err(e) => Err(storage.db_failure(e).await),
    }
}

#[derive(sqlx::FromRow)]
struct RevisionAnd {
    head_revision: i64,
    #[sqlx(flatten)]
    deleted: Deleted,
}

impl RevisionAnd {
    fn split(self) -> (i64, Deleted) {
        (self.head_revision, self.deleted)
    }
}

/// Clears `deleted_at` of a document deleted less than [`RESTORE_DAYS`] ago,
/// with its `oplog` row; the document as `GET /documents/{id}` then shows it.
pub(crate) async fn restore(
    storage: &Storage,
    id: &str,
    origin: &Origin,
) -> Result<DocumentDetail, ApiError> {
    if !is_id(IdKind::Document, id) {
        return Err(ApiError::invalid_request("not a document id"));
    }
    let sql = format!(
        "UPDATE documents SET deleted_at = NULL
         WHERE id = ? AND deleted_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-{RESTORE_DAYS} days')
         RETURNING head_revision"
    );
    let done: Result<bool, sqlx::Error> = async {
        let mut tx = storage.db.pool().begin().await?;
        let Some(revision) = sqlx::query_scalar::<_, i64>(&sql)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(false);
        };
        oplog::append(&mut tx, "restore", id, revision, origin, &json!({})).await?;
        tx.commit().await?;
        Ok(true)
    }
    .await;
    match done {
        Ok(true) => documents::get(storage, id).await,
        Ok(false) => Err(ApiError::not_found("no recently deleted document")),
        Err(e) => Err(storage.db_failure(e).await),
    }
}

/// The documents deleted less than [`RESTORE_DAYS`] ago, last deleted first.
pub(crate) async fn recently_deleted(storage: &Storage) -> Result<DeletedList, ApiError> {
    let sql = format!(
        "SELECT {} FROM documents
         WHERE deleted_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-{RESTORE_DAYS} days')
         ORDER BY deleted_at DESC, id DESC LIMIT {MAX_LISTED}",
        deleted_columns()
    );
    match sqlx::query_as(&sql).fetch_all(storage.db.pool()).await {
        Ok(documents) => Ok(DeletedList { documents }),
        Err(e) => Err(storage.db_failure(e).await),
    }
}

fn path_id(path: Result<UrlPath<String>, PathRejection>) -> Result<String, ApiError> {
    path.map(|UrlPath(id)| id)
        .map_err(|e| ApiError::invalid_request(e.body_text()))
}

/// `POST /documents/{document_id}/delete`, from the page.
pub async fn delete_document(
    Ready(storage): Ready,
    path: Result<UrlPath<String>, PathRejection>,
) -> Result<Json<Deleted>, ApiError> {
    delete(&storage, &path_id(path)?, &Origin::Page)
        .await
        .map(Json)
}

/// `POST /documents/{document_id}/restore`, from the page.
pub async fn restore_document(
    Ready(storage): Ready,
    path: Result<UrlPath<String>, PathRejection>,
) -> Result<Json<DocumentDetail>, ApiError> {
    restore(&storage, &path_id(path)?, &Origin::Page)
        .await
        .map(Json)
}

/// `GET /recently-deleted`.
pub async fn list_recently_deleted(Ready(storage): Ready) -> Result<Json<DeletedList>, ApiError> {
    recently_deleted(&storage).await.map(Json)
}

/// Every `/documents/{id}` route but `restore` answers a deleted document as
/// not found, before its handler runs. Storage that cannot be opened is
/// left to the handler, which answers that itself.
pub async fn hide_deleted(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let mut segments = req.uri().path().trim_start_matches('/').split('/');
    let id = match (segments.next(), segments.next(), segments.next()) {
        (Some("documents"), Some(id), rest) if rest != Some("restore") => id.to_owned(),
        _ => return next.run(req).await,
    };
    if !is_id(IdKind::Document, &id) {
        return next.run(req).await;
    }
    let Ok(storage) = state.storage().await else {
        return next.run(req).await;
    };
    match is_live(&storage, &id).await {
        Ok(true) => next.run(req).await,
        Ok(false) => ApiError::not_found("no such document").into_response(),
        Err(e) => e.into_response(),
    }
}

#[cfg(test)]
mod tests;
