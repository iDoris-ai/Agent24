//! `POST /documents/{id}/extractions` (ADR-DOC-02 §3.2, §5.4, §7;
//! `documentsExtract`): checks the request, then claims its key — the
//! document, revision, schema hash and extractor version, or for a rerun
//! the document and its `Idempotency-Key` — and writes the job under one write lock,
//! starts its worker, and answers 202. A key already claimed replays its
//! job, as an import does.

use std::collections::HashSet;
use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::job::{Input, schema_sha256, spawn};
use super::{EXTRACTOR_VERSION, Field};
use crate::engine::pdfkit;
use crate::error::ApiError;
use crate::extractions::is_field_key;
use crate::id::{IdKind, is_id, new_id};
use crate::idem::request_sha256;
use crate::jobs::{Job, load};
use crate::state::{AppState, Storage};
use crate::uploads::idempotency_key;

const KINDS: [&str; 7] = [
    "text",
    "date",
    "datetime",
    "amount",
    "number",
    "identifier",
    "boolean",
];

/// `DocumentsExtractRequest`, as sent.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Schema {
    fields: Vec<Field>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractRequest {
    revision: i64,
    schema: Schema,
    #[serde(default)]
    rerun: bool,
}

/// What is wrong with `fields`, if anything (`DocumentsExtractField`).
fn bad_fields(fields: &[Field]) -> Option<&'static str> {
    let mut keys = HashSet::new();
    if !(1..=100).contains(&fields.len()) {
        return Some("a schema has 1 to 100 fields");
    }
    for f in fields {
        if !is_field_key(&f.key) {
            return Some("a field key is [a-z][a-z0-9_]{0,63}");
        }
        if !keys.insert(f.key.as_str()) {
            return Some("field keys are unique");
        }
        if !(1..=500).contains(&f.description.chars().count()) {
            return Some("a field description is 1 to 500 characters");
        }
        if f.kind.as_deref().is_some_and(|k| !KINDS.contains(&k)) {
            return Some(
                "a field type is text, date, datetime, amount, number, identifier or boolean",
            );
        }
    }
    None
}

pub async fn extract(
    State(state): State<AppState>,
    path: Result<UrlPath<String>, PathRejection>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let UrlPath(document_id) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let Json(body) = body.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let req: ExtractRequest = serde_json::from_value(body.clone())
        .map_err(|e| ApiError::invalid_request(e.to_string()))?;
    if !is_id(IdKind::Document, &document_id) || req.revision < 1 {
        return Err(ApiError::invalid_request("not a document id or revision"));
    }
    if let Some(why) = bad_fields(&req.schema.fields) {
        return Err(ApiError::invalid_request(why));
    }
    // A rerun names itself: its key joins the business key (§5.4).
    let rerun = if req.rerun {
        Some(idempotency_key(&headers)?)
    } else {
        None
    };
    // The whole body, as sent, is what a reused key is compared on (§5.4).
    let request = request_sha256(&body).map_err(|e| ApiError::invalid_request(e.to_string()))?;
    let schema = schema_sha256(&req.schema.fields).map_err(ApiError::invalid_request)?;
    if state.model().is_none() {
        return Err(ApiError::engine_unavailable(
            "local-model",
            "the kernel offers this module no model",
        ));
    }
    // Nor without the read engine that builds the text layer (D10): checked
    // before any key is claimed or job queued again.
    if state.layers().engine().is_none() {
        return Err(ApiError::engine_unavailable(
            pdfkit::ENGINE_ID,
            "no read engine on this platform",
        ));
    }
    let storage = state
        .storage()
        .await
        .map_err(ApiError::storage_unavailable)?;
    let row: Result<Option<String>, sqlx::Error> = sqlx::query_scalar(
        "SELECT media_type FROM revisions WHERE document_id = ? AND revision = ?",
    )
    .bind(&document_id)
    .bind(req.revision)
    .fetch_optional(storage.db.pool())
    .await;
    let media = match row {
        Ok(Some(media)) => media,
        Ok(None) => return Err(ApiError::not_found("no such document revision")),
        Err(e) => return Err(storage.db_failure(e).await),
    };
    if !pdfkit::FORMATS.contains(&media.as_str()) {
        return Err(ApiError::unprocessable(
            "unsupported_format",
            "slice 1 reads PDF, JPEG and PNG",
        ));
    }
    // A first extraction is keyed by what it is (§5.4). A rerun is keyed by
    // its Idempotency-Key within the document: resending it replays it, and
    // the same key with another body is 422 (David, 2026-10-10).
    let key = match &rerun {
        None => format!(
            "{document_id}/{}/{schema}/{EXTRACTOR_VERSION}",
            req.revision
        ),
        Some(rerun) => format!("{document_id}/rerun/{rerun}"),
    };
    let input = Input {
        document_id,
        revision: req.revision,
        schema_sha256: schema,
        fields: req.schema.fields,
    };
    // In a task of its own: a client that goes away must not stop a commit
    // halfway from the worker start that belongs with it.
    let task = tokio::spawn(claim(state, storage.clone(), key, request, input));
    match task.await {
        Ok(Ok(answer)) => Ok(answer),
        Ok(Err(Fail::Api(e))) => Err(e),
        Ok(Err(Fail::Db(e))) => Err(storage.db_failure(e).await),
        // A spawned task is only cancelled when the runtime shuts down.
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

enum Fail {
    Api(ApiError),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for Fail {
    fn from(e: sqlx::Error) -> Self {
        Fail::Db(e)
    }
}

/// Claims `key` and writes the job under the write lock, then starts its
/// worker; a key already claimed replays its job instead (§7): the same
/// request returns it, 202 while it works and 200 once it succeeded or was
/// cancelled; a failed or interrupted one is queued again, attempt + 1.
async fn claim(
    state: AppState,
    storage: Arc<Storage>,
    key: String,
    request: String,
    input: Input,
) -> Result<(StatusCode, Json<Job>), Fail> {
    let mut tx = storage.db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let hit: Option<(String, String)> = sqlx::query_as(
        "SELECT request_sha256, target_ref FROM idempotency WHERE kind = 'extract' AND key = ?",
    )
    .bind(&key)
    .fetch_optional(&mut *tx)
    .await?;
    let (job_id, attempt) = match hit {
        Some((stored, _)) if stored != request => {
            return Err(Fail::Api(ApiError::idempotency_key_reused()));
        }
        Some((_, job_id)) => {
            let stopped: Option<i64> = sqlx::query_scalar(
                "SELECT attempt FROM jobs WHERE id = ? AND status IN ('failed', 'interrupted')",
            )
            .bind(&job_id)
            .fetch_optional(&mut *tx)
            .await?;
            if stopped.is_some() {
                sqlx::query(
                    "UPDATE jobs SET status = 'queued', attempt = attempt + 1, error = NULL,
                            progress = NULL, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                     WHERE id = ?",
                )
                .bind(&job_id)
                .execute(&mut *tx)
                .await?;
            }
            (job_id, stopped.map(|a| a + 1))
        }
        None => {
            let job_id = new_id(IdKind::Job).map_err(|e| {
                tracing::error!(error = %e, "documents: no randomness for a job id");
                Fail::Api(ApiError::internal("could not generate a job id"))
            })?;
            sqlx::query(
                "INSERT INTO jobs (id, kind, document_id, revision, status, origin, input)
                 VALUES (?, 'extract', ?, ?, 'queued', '{\"kind\":\"page\"}', ?)",
            )
            .bind(&job_id)
            .bind(&input.document_id)
            .bind(input.revision)
            .bind(json!(input).to_string())
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO idempotency (kind, key, request_sha256, target_ref)
                 VALUES ('extract', ?, ?, ?)",
            )
            .bind(&key)
            .bind(&request)
            .bind(&job_id)
            .execute(&mut *tx)
            .await?;
            (job_id, Some(1))
        }
    };
    // The answer is the job as this transaction leaves it.
    let job = load(&mut *tx, &job_id)
        .await?
        .ok_or_else(|| Fail::Api(ApiError::internal("the job vanished")))?;
    tx.commit().await?;
    if let Some(attempt) = attempt {
        spawn(state, storage, job_id, attempt);
    }
    let status = match job.status.as_str() {
        "succeeded" | "cancelled" => StatusCode::OK,
        _ => StatusCode::ACCEPTED,
    };
    Ok((status, Json(job)))
}

#[cfg(test)]
mod tests;
