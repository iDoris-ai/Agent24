//! `POST /uploads/{upload_id}/chunks` (ADR-DOC-02 §5.6): append one chunk.
//!
//! The bytes go to `uploads/<upload_id>/data` ([`super::data`]), fsynced,
//! before the row that counts them commits.

use axum::Json;
use axum::body::Bytes;
use axum::extract::Path as UrlPath;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, StatusCode};
use sha2::{Digest, Sha256};

use super::data::{WriteError, write_at};
use super::{Failure, UPLOAD_COLUMNS, Upload, is_sha256_address};
use crate::error::{ApiError, StorageCause};
use crate::id::{IdKind, is_id};
use crate::idem::MAX_SAFE_INTEGER;
use crate::state::{Ready, Storage, blocking, io_cause};

/// Largest chunk body: 768 KiB, under the kernel proxy's 1 MiB limit.
pub const MAX_CHUNK: usize = 786_432;

/// Appends to one upload run one at a time, so a file write and the row
/// that counts it never interleave with another append's. Only this process
/// opens the data directory (the blob store holds its lock), so an
/// in-process lock is enough; 32 stripes, chosen by upload id.
static STRIPES: [tokio::sync::Mutex<()>; 32] = [const { tokio::sync::Mutex::const_new(()) }; 32];

fn stripe(upload_id: &str) -> &'static tokio::sync::Mutex<()> {
    let h = upload_id.bytes().fold(0usize, |h, b| {
        h.wrapping_mul(31).wrapping_add(usize::from(b))
    });
    &STRIPES[h % STRIPES.len()]
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, ApiError> {
    headers
        .get(name)
        .ok_or_else(|| ApiError::invalid_request(format!("the {name} header is required")))?
        .to_str()
        .map_err(|_| ApiError::invalid_request(format!("{name} must be ASCII")))
}

/// `Upload-Offset`: decimal digits only, at most 2^53 − 1.
fn upload_offset(headers: &HeaderMap) -> Result<i64, ApiError> {
    let raw = header(headers, "upload-offset")?;
    raw.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| raw.parse::<i64>().ok())
        .flatten()
        .filter(|n| *n <= MAX_SAFE_INTEGER)
        .ok_or_else(|| {
            ApiError::invalid_request(format!(
                "Upload-Offset must be an integer from 0 to {MAX_SAFE_INTEGER}"
            ))
        })
}

fn sha256_address(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

pub async fn append_chunk(
    Ready(storage): Ready,
    UrlPath(upload_id): UrlPath<String>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Upload>, ApiError> {
    if !is_id(IdKind::Upload, &upload_id) {
        return Err(ApiError::invalid_request("not an upload id"));
    }
    let offset = upload_offset(&headers)?;
    let claimed = header(&headers, "chunk-sha256")?;
    if !is_sha256_address(claimed) {
        return Err(ApiError::invalid_request(
            "Chunk-Sha256 must be `sha256:` and 64 lowercase hex digits",
        ));
    }
    let body = body.map_err(|e| match e.status() {
        StatusCode::PAYLOAD_TOO_LARGE => ApiError::payload_too_large(MAX_CHUNK),
        _ => ApiError::invalid_request(e.body_text()),
    })?;
    if body.is_empty() {
        return Err(ApiError::invalid_request("a chunk has at least one byte"));
    }
    // Waiting for the turn stays in the handler, so a client that goes away
    // while waiting leaves nothing behind. Once the turn is taken, it, the
    // file write and the commit move to a task of their own: dropping the
    // handler then must not release the turn while the write still runs.
    #[cfg(test)]
    tests::waiting_for_turn(&upload_id);
    let turn = stripe(&upload_id).lock().await;
    let claimed = claimed.to_owned();
    let task = tokio::spawn(async move {
        let _turn = turn;
        #[cfg(test)]
        tests::operation_started(&upload_id);
        match append(&storage, &upload_id, offset, &claimed, body).await {
            Ok(upload) => Ok(Json(upload)),
            Err(Failure::Api(e)) => Err(e),
            Err(Failure::Db(e)) => Err(storage.db_failure(e).await),
        }
    });
    match task.await {
        Ok(result) => result,
        // A spawned task is only cancelled when the runtime shuts down.
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

async fn fetch(storage: &Storage, id: &str) -> Result<Upload, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {UPLOAD_COLUMNS} FROM uploads WHERE id = ?"
    ))
    .bind(id)
    .fetch_one(storage.db.pool())
    .await
}

/// The §5.6 decision, in the contract's order: an expired or unknown upload
/// is 404; a range already received with the same hash is a 200 replay and
/// any other hash 409; the next offset appends once the body matches its
/// hash (else 400); any other offset is 409.
async fn append(
    storage: &Storage,
    id: &str,
    offset: i64,
    claimed: &str,
    body: Bytes,
) -> Result<Upload, Failure> {
    let pool = storage.db.pool();
    let row: Option<(i64, i64, String, bool)> = sqlx::query_as(
        "SELECT total_size, received, status,
                julianday('now') > julianday(coalesce(last_chunk_at, created_at), '+24 hours')
         FROM uploads WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some((total, received, status, stale)) = row else {
        return Err(Failure::Api(ApiError::not_found("no such upload")));
    };
    let live = matches!(status.as_str(), "receiving" | "complete");
    if status == "expired" || (live && stale) {
        if live {
            sqlx::query("UPDATE uploads SET status = 'expired' WHERE id = ?")
                .bind(id)
                .execute(pool)
                .await?;
        }
        return Err(Failure::Api(ApiError::not_found("the upload has expired")));
    }
    if offset < received {
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT sha256 FROM upload_chunks WHERE upload_id = ? AND chunk_offset = ?",
        )
        .bind(id)
        .bind(offset)
        .fetch_optional(pool)
        .await?;
        return match stored {
            Some(stored) if stored == claimed => Ok(fetch(storage, id).await?),
            _ => Err(Failure::Api(ApiError::upload_offset_mismatch(received))),
        };
    }
    if offset > received || status != "receiving" {
        return Err(Failure::Api(ApiError::upload_offset_mismatch(received)));
    }
    let size = i64::try_from(body.len()).unwrap_or(i64::MAX);
    if offset + size > total {
        return Err(Failure::Api(ApiError::invalid_request(format!(
            "the chunk runs past total_size ({total} bytes)"
        ))));
    }
    if sha256_address(&body) != claimed {
        return Err(Failure::Api(ApiError::invalid_request(
            "Chunk-Sha256 does not match the body",
        )));
    }

    let dir = super::data::upload_dir(storage.data_dir(), id);
    // Never negative (`offset == received`, which 0001's CHECK keeps ≥ 0);
    // if that ever changed, fail rather than write somewhere else.
    let start = u64::try_from(offset).map_err(|e| Failure::Db(sqlx::Error::Decode(Box::new(e))))?;
    match blocking(move || write_at(&dir, start, &body)).await {
        Ok(()) => {}
        Err(WriteError::Io(e)) => {
            tracing::error!(error = %e, upload = id, "documents: writing a chunk failed");
            return Err(Failure::Api(ApiError::storage_unavailable(io_cause(&e))));
        }
        Err(WriteError::Short { len }) => {
            tracing::error!(
                upload = id,
                len,
                offset,
                "documents: upload data is shorter than received"
            );
            return Err(Failure::Api(ApiError::storage_unavailable(
                StorageCause::Corrupt,
            )));
        }
    }

    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query(
        "INSERT INTO upload_chunks (upload_id, chunk_offset, chunk_size, sha256) VALUES (?, ?, ?, ?)",
    )
    .bind(id)
    .bind(offset)
    .bind(size)
    .bind(claimed)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE uploads SET received = ?1,
                last_chunk_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                status = CASE WHEN ?1 = total_size THEN 'complete' ELSE status END
         WHERE id = ?2",
    )
    .bind(offset + size)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(fetch(storage, id).await?)
}

#[cfg(test)]
mod tests;
