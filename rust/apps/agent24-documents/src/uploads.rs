//! Chunked uploads (ADR-DOC-02 §5.6): `POST /uploads` starts one.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::ApiError;
use crate::id::{IdKind, new_id};
use crate::idem::{MAX_SAFE_INTEGER, request_sha256};
use crate::state::{Ready, Storage};

/// `DocumentsUploadRequest`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadRequest {
    /// Any JSON number here, so a bad one is our 400, not a parse error;
    /// [`UploadRequest::validate`] takes integer literals only. serde_json
    /// can round a fraction or exponent spelling (9007199254740991.0 parses
    /// as …990), so `10.0` and `1e1` are refused, as the contract says.
    total_size: serde_json::Number,
    sha256: String,
    #[serde(default, deserialize_with = "present_string")]
    filename: Option<String>,
}

/// A present `filename` must be a string: `null` is not the same as absent.
fn present_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    String::deserialize(d).map(Some)
}

/// `DocumentsUpload`.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Upload {
    pub upload_id: String,
    pub total_size: i64,
    pub sha256: String,
    pub received: i64,
    pub status: String,
    pub expires_at: String,
}

/// Columns for [`Upload`]: it expires 24 h after the last chunk, or after
/// creation if none has arrived yet.
const UPLOAD_COLUMNS: &str = "id AS upload_id, total_size, sha256, received, status, \
     strftime('%Y-%m-%dT%H:%M:%fZ', coalesce(last_chunk_at, created_at), '+24 hours') AS expires_at";

pub(crate) fn is_sha256_address(s: &str) -> bool {
    s.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

impl UploadRequest {
    /// Checks the request against `DocumentsUploadRequest`; returns the size.
    fn validate(&self) -> Result<i64, ApiError> {
        let Some(total_size) = self
            .total_size
            .as_i64()
            .filter(|t| (1..=MAX_SAFE_INTEGER).contains(t))
        else {
            return Err(ApiError::invalid_request(format!(
                "total_size must be an integer literal between 1 and {MAX_SAFE_INTEGER}"
            )));
        };
        if !is_sha256_address(&self.sha256) {
            return Err(ApiError::invalid_request(
                "sha256 must be `sha256:` and 64 lowercase hex digits",
            ));
        }
        if let Some(name) = &self.filename {
            let chars = name.chars().count();
            if !(1..=255).contains(&chars) || name.contains(['/', '\\', '\0']) {
                return Err(ApiError::invalid_request(
                    "filename must be a base name of 1 to 255 characters, without / \\ or NUL",
                ));
            }
        }
        Ok(total_size)
    }
}

/// The `Idempotency-Key` header: 1 to 200 characters of visible ASCII
/// without spaces (`DocumentsIdempotencyKey`).
fn idempotency_key(headers: &HeaderMap) -> Result<String, ApiError> {
    let key = headers
        .get("idempotency-key")
        .ok_or_else(|| ApiError::invalid_request("the Idempotency-Key header is required"))?
        .as_bytes();
    if !(1..=200).contains(&key.len()) || !key.iter().all(|b| (0x21..=0x7e).contains(b)) {
        return Err(ApiError::invalid_request(
            "Idempotency-Key must be 1 to 200 characters of visible ASCII without spaces",
        ));
    }
    Ok(String::from_utf8_lossy(key).into_owned())
}

/// Idempotent on `Idempotency-Key` (kind `upload`, §5.4): the same key and
/// request return the existing upload (200); the same key with a different
/// request is 422 `idempotency_key_reused`.
pub async fn create_upload(
    Ready(storage): Ready,
    headers: HeaderMap,
    body: Result<Json<UploadRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Upload>), ApiError> {
    let key = idempotency_key(&headers)?;
    let Json(req) = body.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    let total_size = req.validate()?;
    let mut params = json!({ "total_size": total_size, "sha256": req.sha256 });
    if let Some(name) = &req.filename {
        params["filename"] = json!(name);
    }
    let request = request_sha256(&params).map_err(|e| ApiError::invalid_request(e.to_string()))?;
    match create(&storage, &key, &request, total_size, &req).await {
        Ok(created) => Ok(created),
        Err(Failure::Api(e)) => Err(e),
        Err(Failure::Db(e)) => Err(storage.db_failure(e).await),
    }
}

enum Failure {
    Api(ApiError),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for Failure {
    fn from(e: sqlx::Error) -> Self {
        Failure::Db(e)
    }
}

async fn create(
    storage: &Storage,
    key: &str,
    request: &str,
    total_size: i64,
    req: &UploadRequest,
) -> Result<(StatusCode, Json<Upload>), Failure> {
    #[cfg(test)]
    tests::attempting(key);
    // IMMEDIATE takes the write lock before the lookup, so the lookup and the
    // claim are one step: two requests with one key cannot both miss it, and
    // the (kind, key) primary key never sees a second claim to recover from.
    // Behind another writer it waits up to the 5 s busy timeout, then is 503
    // `busy`.
    let mut tx = storage.db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let existing: Option<(String, String)> = sqlx::query_as(
        "SELECT request_sha256, target_ref FROM idempotency WHERE kind = 'upload' AND key = ?",
    )
    .bind(key)
    .fetch_optional(&mut *tx)
    .await?;
    #[cfg(test)]
    if existing.is_none() {
        tests::missed_lookup(key).await;
    }
    let (status, id) = match existing {
        Some((stored, id)) if stored == request => (StatusCode::OK, id),
        Some(_) => return Err(Failure::Api(ApiError::idempotency_key_reused())),
        None => {
            let id = new_id(IdKind::Upload).map_err(|e| {
                tracing::error!(error = %e, "documents: no randomness for an upload id");
                Failure::Api(ApiError::internal("could not generate an upload id"))
            })?;
            sqlx::query(
                "INSERT INTO uploads (id, total_size, sha256, filename) VALUES (?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(total_size)
            .bind(&req.sha256)
            .bind(&req.filename)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO idempotency (kind, key, request_sha256, target_ref) VALUES ('upload', ?, ?, ?)",
            )
            .bind(key)
            .bind(request)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
            (StatusCode::CREATED, id)
        }
    };
    let upload: Upload = sqlx::query_as(&format!(
        "SELECT {UPLOAD_COLUMNS} FROM uploads WHERE id = ?"
    ))
    .bind(&id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((status, Json(upload)))
}

#[cfg(test)]
mod tests;
