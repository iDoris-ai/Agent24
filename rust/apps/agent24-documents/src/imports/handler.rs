//! `POST /imports` (ADR-DOC-02 §5.4, §7): checks the upload (complete,
//! whole-file hash, a format slice 1 reads), then claims the key — the
//! `upload_id` — and writes the job row under one write lock, starts its
//! worker, and answers 202.

use std::future::Future;
use std::io::Read;
use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::{media_type, worker};
use crate::error::ApiError;
use crate::id::{IdKind, is_id, new_id};
use crate::idem::request_sha256;
use crate::jobs::{Job, load};
use crate::state::{Ready, Storage, blocking, io_cause};
use crate::uploads::{data::upload_dir, present_string};

/// `DocumentsImportRequest`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportRequest {
    upload_id: String,
    #[serde(default, deserialize_with = "present_string")]
    title: Option<String>,
}

enum Checked {
    Ok,
    Mismatch,
    Unsupported,
}

/// Hashes the `received` bytes of an upload and reads its format.
fn check_data(path: &std::path::Path, received: u64, declared: &str) -> std::io::Result<Checked> {
    let mut reader = std::fs::File::open(path)?.take(received);
    let mut hasher = Sha256::new();
    let mut head = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut seen = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if head.len() < 8 {
            head.extend_from_slice(&buf[..n.min(8 - head.len())]);
        }
        hasher.update(&buf[..n]);
        seen += n as u64;
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(if seen != received || format!("sha256:{hex}") != declared {
        Checked::Mismatch
    } else if media_type(&head).is_none() {
        Checked::Unsupported
    } else {
        Checked::Ok
    })
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

type Answer = Result<(StatusCode, Json<Job>), Fail>;

/// Runs `step` in a task of its own: a client that goes away must not stop
/// a commit halfway from the worker start that belongs with it.
async fn detached<F: Future<Output = Answer> + Send + 'static>(step: F) -> Answer {
    match tokio::spawn(step).await {
        Ok(answer) => answer,
        // A spawned task is only cancelled when the runtime shuts down.
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

pub async fn import(
    Ready(storage): Ready,
    body: Result<Json<ImportRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let Json(req) = body.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    if !is_id(IdKind::Upload, &req.upload_id) {
        return Err(ApiError::invalid_request("not an upload id"));
    }
    let bad_title = |t: &String| !(1..=500).contains(&t.chars().count());
    if req.title.as_ref().is_some_and(bad_title) {
        return Err(ApiError::invalid_request(
            "title must be 1 to 500 characters",
        ));
    }
    let mut params = json!({ "upload_id": req.upload_id });
    if let Some(title) = &req.title {
        params["title"] = json!(title);
    }
    let request = request_sha256(&params).map_err(|e| ApiError::invalid_request(e.to_string()))?;
    match start(&storage, req, request).await {
        Ok(answer) => Ok(answer),
        Err(Fail::Api(e)) => Err(e),
        Err(Fail::Db(e)) => Err(storage.db_failure(e).await),
    }
}

async fn claimed(
    executor: impl sqlx::SqliteExecutor<'_>,
    upload_id: &str,
) -> Result<Option<(String, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT request_sha256, target_ref FROM idempotency WHERE kind = 'import' AND key = ?",
    )
    .bind(upload_id)
    .fetch_optional(executor)
    .await
}

async fn start(storage: &Arc<Storage>, req: ImportRequest, request: String) -> Answer {
    let pool = storage.db.pool();
    if let Some(hit) = claimed(pool, &req.upload_id).await? {
        return detached(replay(storage.clone(), hit, request)).await;
    }
    #[cfg(test)]
    super::handler_test::after_lookup(&req.upload_id).await;
    let row: Option<(i64, String, String, bool)> = sqlx::query_as(
        "SELECT received, status, sha256,
                julianday('now') > julianday(coalesce(last_chunk_at, created_at), '+24 hours')
         FROM uploads WHERE id = ?",
    )
    .bind(&req.upload_id)
    .fetch_optional(pool)
    .await?;
    let Some((received, status, sha256, stale)) = row else {
        return Err(Fail::Api(ApiError::not_found("no such upload")));
    };
    if status == "expired" || (status != "imported" && stale) {
        return Err(Fail::Api(ApiError::not_found("the upload has expired")));
    }
    if status == "receiving" {
        return Err(Fail::Api(
            ApiError::invalid_request("the upload is not complete")
                .with_detail("received", received),
        ));
    }
    let path = upload_dir(storage.data_dir(), &req.upload_id).join("data");
    let size = u64::try_from(received).map_err(|e| Fail::Db(sqlx::Error::Decode(Box::new(e))))?;
    match blocking(move || check_data(&path, size, &sha256)).await {
        Ok(Checked::Ok) => {}
        Ok(Checked::Mismatch) => {
            return Err(Fail::Api(ApiError::unprocessable(
                "upload_checksum_mismatch",
                "the uploaded bytes do not match the declared sha256",
            )));
        }
        Ok(Checked::Unsupported) => {
            return Err(Fail::Api(ApiError::unprocessable(
                "unsupported_format",
                "slice 1 imports PDF, JPEG and PNG",
            )));
        }
        Err(e) => return Err(Fail::Api(ApiError::storage_unavailable(io_cause(&e)))),
    }
    detached(claim(storage.clone(), req, request)).await
}

/// Claims the key and writes the job under the write lock; a request that
/// got here first wins, and this one replays it. Then starts the worker.
async fn claim(storage: Arc<Storage>, req: ImportRequest, request: String) -> Answer {
    let mut tx = storage.db.pool().begin_with("BEGIN IMMEDIATE").await?;
    if let Some(hit) = claimed(&mut *tx, &req.upload_id).await? {
        tx.rollback().await?;
        return replay(storage, hit, request).await;
    }
    let job_id = new_id(IdKind::Job).map_err(|e| {
        tracing::error!(error = %e, "documents: no randomness for a job id");
        Fail::Api(ApiError::internal("could not generate a job id"))
    })?;
    let input = json!({ "upload_id": req.upload_id, "title": req.title }).to_string();
    sqlx::query(
        "INSERT INTO jobs (id, kind, status, origin, input) VALUES (?, 'import', 'queued', '{\"kind\":\"page\"}', ?)",
    )
    .bind(&job_id)
    .bind(&input)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO idempotency (kind, key, request_sha256, target_ref) VALUES ('import', ?, ?, ?)",
    )
    .bind(&req.upload_id)
    .bind(&request)
    .bind(&job_id)
    .execute(&mut *tx)
    .await?;
    let job = load(&mut *tx, &job_id).await?;
    tx.commit().await?;
    #[cfg(test)]
    super::handler_test::after_claim(&req.upload_id).await;
    worker::spawn(storage, job_id, 1);
    job.map(|j| (StatusCode::ACCEPTED, Json(j)))
        .ok_or_else(|| Fail::Api(ApiError::internal("the job vanished")))
}

/// A key hit (§7): the same request returns the job — 202 while it works,
/// 200 once it succeeded or was cancelled (a cancelled job stays so); a
/// failed or interrupted one is queued again, attempt + 1, and is 202.
async fn replay(
    storage: Arc<Storage>,
    (stored, job_id): (String, String),
    request: String,
) -> Answer {
    if stored != request {
        return Err(Fail::Api(ApiError::idempotency_key_reused()));
    }
    // An explicit transaction, so a failed commit is an error, not a
    // re-queue that seems to have happened.
    let mut tx = storage.db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let stopped: Option<i64> = sqlx::query_scalar(
        "SELECT attempt FROM jobs WHERE id = ? AND status IN ('failed', 'interrupted')",
    )
    .bind(&job_id)
    .fetch_optional(&mut *tx)
    .await?;
    if stopped.is_some() {
        sqlx::query(
            "UPDATE jobs SET status = 'queued', attempt = attempt + 1, error = NULL, progress = NULL,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?",
        )
        .bind(&job_id)
        .execute(&mut *tx)
        .await?;
    }
    // The answer is the job as this transaction leaves it: once committed,
    // its worker may move it on at any time.
    let job = load(&mut *tx, &job_id)
        .await?
        .ok_or_else(|| Fail::Api(ApiError::internal("the job vanished")))?;
    tx.commit().await?;
    #[cfg(test)]
    super::handler_test::before_requeued_start(&job_id).await;
    if let Some(attempt) = stopped.map(|a| a + 1) {
        worker::spawn(storage.clone(), job_id.clone(), attempt);
    }
    #[cfg(test)]
    super::handler_test::after_replay(&job_id).await;
    let status = match job.status.as_str() {
        "succeeded" | "cancelled" => StatusCode::OK,
        _ => StatusCode::ACCEPTED,
    };
    Ok((status, Json(job)))
}
