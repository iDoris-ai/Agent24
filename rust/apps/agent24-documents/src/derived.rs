//! A document the OS makes itself (D2 split/merge, D1 Office → PDF): the
//! file is written straight into an upload of the OS's own, recorded as the
//! chunked upload would record it, then imported as `POST /imports` would.
//! The answer is that import's job; the new document is its result.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::http::StatusCode;
use sha2::{Digest, Sha256};

use crate::error::ApiError;
use crate::id::{IdKind, new_id};
use crate::imports::handler::import_upload;
use crate::jobs::Job;
use crate::state::{Storage, blocking, io_cause};
use crate::uploads::data::upload_dir;

/// The largest chunk an upload records (`upload_chunks.chunk_size`).
const CHUNK: usize = 786_432;

fn sha(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

/// The file at `path`: its `sha256:<hex>`, size, and its chunks' offsets,
/// sizes and hashes, as an upload records them.
type Chunks = Vec<(i64, i64, String)>;
fn digest(path: &std::path::Path) -> std::io::Result<(String, i64, Chunks)> {
    let bytes = std::fs::read(path)?;
    let mut chunks = Vec::new();
    for (i, part) in bytes.chunks(CHUNK).enumerate() {
        let offset = i64::try_from(i * CHUNK).map_err(std::io::Error::other)?;
        let size = i64::try_from(part.len()).map_err(std::io::Error::other)?;
        chunks.push((offset, size, sha(part)));
    }
    let size = i64::try_from(bytes.len()).map_err(std::io::Error::other)?;
    Ok((sha(&bytes), size, chunks))
}

/// The upload's rows, as the chunked upload writes them (the triggers in
/// 0002/0004 allow no shortcut): receiving, each chunk, then complete.
async fn record_upload(
    storage: &Storage,
    id: &str,
    sha256: &str,
    size: i64,
    filename: &str,
    chunks: &Chunks,
) -> Result<(), sqlx::Error> {
    let mut tx = storage.db.pool().begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("INSERT INTO uploads (id, total_size, sha256, filename) VALUES (?, ?, ?, ?)")
        .bind(id)
        .bind(size)
        .bind(sha256)
        .bind(filename)
        .execute(&mut *tx)
        .await?;
    for (offset, chunk_size, chunk_sha) in chunks {
        sqlx::query(
            "INSERT INTO upload_chunks (upload_id, chunk_offset, chunk_size, sha256) VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(offset)
        .bind(chunk_size)
        .bind(chunk_sha)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE uploads SET received = ?1,
                    last_chunk_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                    status = CASE WHEN ?1 = total_size THEN 'complete' ELSE status END
             WHERE id = ?2",
        )
        .bind(offset + chunk_size)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// `make` writes the new file at the path it is given (in a blocking task);
/// it is then imported under `title` as `<title>.<extension>`.
pub async fn import_derived<F>(
    storage: &Arc<Storage>,
    title: Option<String>,
    extension: &str,
    make: F,
) -> Result<(StatusCode, Json<Job>), ApiError>
where
    F: FnOnce(&Path) -> Result<(), ApiError> + Send + 'static,
{
    let upload_id = new_id(IdKind::Upload)
        .map_err(|_| ApiError::internal("could not generate an upload id"))?;
    let dir = upload_dir(storage.data_dir(), &upload_id);
    let made = dir.clone();
    blocking(move || std::fs::create_dir_all(&made))
        .await
        .map_err(|e| ApiError::storage_unavailable(io_cause(&e)))?;
    let data = dir.join("data");
    let target = data.clone();
    let cleanup = |dir: PathBuf| async move {
        let _ = blocking(move || std::fs::remove_dir_all(dir)).await;
    };
    if let Err(e) = blocking(move || make(&target)).await {
        cleanup(dir).await;
        return Err(e);
    }
    let read = data.clone();
    let (sha256, size, chunks) = match blocking(move || digest(&read)).await {
        Ok(d) => d,
        Err(e) => {
            cleanup(dir).await;
            return Err(ApiError::storage_unavailable(io_cause(&e)));
        }
    };
    let filename = format!(
        "{}.{extension}",
        title
            .as_deref()
            .unwrap_or("derived")
            .chars()
            .filter(|c| !c.is_control())
            .map(|c| if c == '/' || c == '\\' { '_' } else { c })
            .take(200)
            .collect::<String>()
    );
    if let Err(e) = record_upload(storage, &upload_id, &sha256, size, &filename, &chunks).await {
        cleanup(dir).await;
        return Err(storage.db_failure(e).await);
    }
    import_upload(storage, upload_id, title).await
}
