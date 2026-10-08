//! Jobs (ADR-DOC-02 §7): `GET /jobs/{job_id}`, cancel and retry. The row,
//! not any event, is the authority.

use axum::Json;
use axum::extract::Path as UrlPath;
use serde::Serialize;
use serde_json::{Value, json};

use crate::db::Db;
use crate::error::{ApiError, CODES};
use crate::id::{IdKind, is_id};
use crate::state::{Ready, Storage};
use crate::timestamp::is_timestamp;

/// `DocumentsJob`.
#[derive(Debug, Serialize, PartialEq)]
pub struct Job {
    pub job_id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<i64>,
    pub status: String,
    pub attempt: i64,
    pub progress: Option<Value>,
    pub error: Option<Value>,
    pub result: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    kind: String,
    document_id: Option<String>,
    revision: Option<i64>,
    status: String,
    attempt: i64,
    progress: Option<String>,
    error: Option<String>,
    created_at: String,
    updated_at: String,
    result_ref: Option<String>,
}

impl Row {
    /// The row as `DocumentsJob`, or why it is not one. The table's CHECKs
    /// allow rows the contract's status rules do not (a `failed` job with no
    /// error, a succeeded import with no document); such a row is a bug,
    /// never sent.
    fn checked(self) -> Result<Job, String> {
        // 0001's CHECKs read ids only up to a NUL, so check them whole here.
        if !is_id(IdKind::Job, &self.id)
            || self
                .document_id
                .as_deref()
                .is_some_and(|d| !is_id(IdKind::Document, d))
        {
            return Err(format!("ids {:?} / {:?}", self.id, self.document_id));
        }
        if !is_timestamp(&self.created_at) || !is_timestamp(&self.updated_at) {
            return Err(format!(
                "timestamps {:?} / {:?}",
                self.created_at, self.updated_at
            ));
        }
        let parse = |name: &str, s: Option<String>| {
            s.map(|s| serde_json::from_str::<Value>(&s).map_err(|e| format!("{name}: {e}")))
                .transpose()
        };
        let progress = parse("progress", self.progress)?;
        let error = parse("error", self.error)?;
        if let Some(p) = &progress {
            let count = |k: &str| p.get(k).and_then(Value::as_u64).is_some();
            let text = |k: &str| p.get(k).is_some_and(Value::is_string);
            let total = p
                .get("total")
                .is_some_and(|t| t.is_null() || t.as_u64().is_some());
            if !(text("stage") && count("done") && total && text("unit")) {
                return Err(format!("progress {p}"));
            }
        }
        let code = match &error {
            None => None,
            Some(e) => match (e.get("code").and_then(Value::as_str), e.get("message")) {
                (Some(code), Some(Value::String(_))) => Some(code.to_owned()),
                _ => return Err(format!("error {e}")),
            },
        };
        let closed = |c: &str| CODES.contains(&c);
        let fits = match (self.status.as_str(), code.as_deref()) {
            ("failed", Some(c)) => closed(c),
            ("failed", None) => false,
            ("cancelled", None | Some("cancelled")) => true,
            ("queued" | "running" | "cancelling" | "interrupted" | "succeeded", None) => true,
            _ => false,
        };
        if !fits {
            return Err(format!("status {} with error {code:?}", self.status));
        }
        // Only a succeeded job has a result; imports and extractions must name it.
        let result = match (self.status.as_str(), self.kind.as_str()) {
            ("succeeded", "import") => {
                if self.document_id.is_none() || self.revision != Some(1) {
                    return Err("a succeeded import without its document and r1".into());
                }
                Some(json!({ "document_id": self.document_id, "revision": self.revision }))
            }
            ("succeeded", "extract") => match self.result_ref.as_deref() {
                Some(ext) if is_id(IdKind::Extraction, ext) => {
                    Some(json!({ "extraction_id": ext }))
                }
                other => return Err(format!("a succeeded extraction with result {other:?}")),
            },
            _ => None,
        };
        Ok(Job {
            job_id: self.id,
            kind: self.kind,
            document_id: self.document_id,
            revision: self.revision,
            status: self.status,
            attempt: self.attempt,
            progress,
            error,
            result,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";

pub async fn load<'e, E: sqlx::SqliteExecutor<'e>>(
    executor: E,
    id: &str,
) -> Result<Option<Job>, sqlx::Error> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, kind, document_id, revision, status, attempt, progress, error, created_at, updated_at,
                result_ref
         FROM jobs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(executor)
    .await?;
    row.map(|row| {
        let id = row.id.clone();
        row.checked()
            .map_err(|why| sqlx::Error::Decode(format!("job {id} breaks §7: {why}").into()))
    })
    .transpose()
}

/// At startup, before any worker runs: a job that was queued or running
/// has no worker any more and becomes `interrupted` (the next call with its
/// key re-queues it, §7); one that was cancelling ends `cancelled`, with no
/// output committed (its result and status share a transaction).
pub async fn recover(db: &Db) -> Result<(), sqlx::Error> {
    // One statement, so one atomic step.
    sqlx::query(&format!(
        "UPDATE jobs SET updated_at = {NOW},
                status = CASE status WHEN 'cancelling' THEN 'cancelled' ELSE 'interrupted' END,
                error = CASE status WHEN 'cancelling'
                        THEN json_object('code', 'cancelled', 'message', 'cancelled before a restart')
                        ELSE error END
         WHERE status IN ('queued', 'running', 'cancelling')"
    ))
    .execute(db.pool())
    .await
    .map(|_| ())
}

fn job_id(id: &str) -> Result<(), ApiError> {
    if is_id(IdKind::Job, id) {
        Ok(())
    } else {
        Err(ApiError::invalid_request("not a job id"))
    }
}

/// A read-then-write on one job, under the write lock: `change` maps the
/// current status to the update to make (`None`: leave the job as it is).
async fn transition(
    storage: &Storage,
    id: &str,
    change: fn(&str) -> Result<Option<&'static str>, ApiError>,
) -> Result<Job, ApiError> {
    let result = async {
        let mut tx = storage.db.pool().begin_with("BEGIN IMMEDIATE").await?;
        let Some(job) = load(&mut *tx, id).await? else {
            return Ok(Err(ApiError::not_found("no such job")));
        };
        let update = match change(&job.status) {
            Ok(update) => update,
            Err(e) => return Ok(Err(e)),
        };
        if let Some(set) = update {
            sqlx::query(&format!(
                "UPDATE jobs SET {set}, updated_at = {NOW} WHERE id = ?"
            ))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        let job = load(&mut *tx, id).await?;
        tx.commit().await?;
        Ok(job.ok_or_else(|| ApiError::not_found("no such job")))
    }
    .await;
    match result {
        Ok(answer) => answer,
        Err(e) => Err(storage.db_failure(e).await),
    }
}

pub async fn get_job(
    Ready(storage): Ready,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Job>, ApiError> {
    job_id(&id)?;
    match load(storage.db.pool(), &id).await {
        Ok(Some(job)) => Ok(Json(job)),
        Ok(None) => Err(ApiError::not_found("no such job")),
        Err(e) => Err(storage.db_failure(e).await),
    }
}

/// Idempotent. A job not yet working, or stopped by a failure, is cancelled
/// at once, so a key hit will not revive it (§7); a running one is asked to
/// stop at its next stage boundary; a finished one stays as it is.
pub async fn cancel_job(
    Ready(storage): Ready,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Job>, ApiError> {
    job_id(&id)?;
    transition(&storage, &id, |status| {
        Ok(match status {
            "queued" | "failed" | "interrupted" => Some(
                "status = 'cancelled', error = json_object('code', 'cancelled', 'message', 'cancelled by the user')",
            ),
            "running" => Some("status = 'cancelling'"),
            _ => None,
        })
    })
    .await
    .map(Json)
}

/// The explicit restart (§7): failed, interrupted or cancelled → queued,
/// attempt + 1. Anything else is 400 with `details.status`.
pub async fn retry_job(
    Ready(storage): Ready,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Job>, ApiError> {
    job_id(&id)?;
    transition(&storage, &id, |status| match status {
        "failed" | "interrupted" | "cancelled" => Ok(Some(
            "status = 'queued', attempt = attempt + 1, error = NULL, progress = NULL",
        )),
        other => Err(
            ApiError::invalid_request(format!("a {other} job cannot be retried"))
                .with_detail("status", other.to_owned()),
        ),
    })
    .await
    .map(Json)
}

#[cfg(test)]
mod tests;
