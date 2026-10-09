//! The import job's work (ADR-DOC-02 §2.2, §7), run by [`runner`]: copy the
//! upload's bytes into the blob store, then write the document, its r1, the
//! upload's `imported` and the job's success in one transaction.

use std::io::{self, Read};
use std::sync::Arc;

use serde_json::{Value, json};

use super::media_type;
use crate::blob::BlobError;
use crate::error::StorageCause;
use crate::id::{IdKind, new_id};
use crate::jobs::runner::{self, Claim};
use crate::state::{Storage, blocking};
use crate::uploads::data::upload_dir;

/// SQL: the upload is still importable — complete, and within its 24 h.
const UPLOAD_LIVE: &str = "status = 'complete'
     AND julianday('now') <= julianday(coalesce(last_chunk_at, created_at), '+24 hours')";

/// Runs attempt `attempt` of import job `job_id` in the background.
pub fn spawn(storage: Arc<Storage>, job_id: String, attempt: i64) {
    runner::spawn(storage, job_id, attempt, |storage, claim| async move {
        import(&storage, &claim).await
    });
}

/// Keeps the first bytes that pass through, so the format is read from the
/// very bytes being stored and hashed.
struct Head<R> {
    inner: R,
    head: Vec<u8>,
}

impl<R: Read> Read for Head<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        let want = 8usize.saturating_sub(self.head.len()).min(n);
        self.head.extend_from_slice(&buf[..want]);
        Ok(n)
    }
}

async fn import(storage: &Arc<Storage>, claim: &Claim) -> Result<(), sqlx::Error> {
    let field = |k: &str| {
        claim
            .input()
            .and_then(|i| i.get(k))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let (Some(upload_id), title) = (field("upload_id"), field("title")) else {
        return claim
            .settle(storage, "invalid_request", "the job has no upload")
            .await;
    };

    #[cfg(test)]
    super::tests::in_worker(&upload_id).await;

    let row: Option<(i64, String, Option<String>)> = sqlx::query_as(&format!(
        "SELECT received, sha256, filename FROM uploads WHERE id = ? AND {UPLOAD_LIVE}"
    ))
    .bind(&upload_id)
    .fetch_optional(storage.db.pool())
    .await?;
    let Some((received, sha256, filename)) = row else {
        return claim
            .settle(storage, "not_found", "the upload is gone or expired")
            .await;
    };

    // Stage `store`: the bytes into the blob store (§2.2's write order).
    let path = upload_dir(storage.data_dir(), &upload_id).join("data");
    let (store, size) = (storage.clone(), received.unsigned_abs());
    let stored = blocking(move || -> Result<_, BlobError> {
        let mut reader = Head {
            inner: std::fs::File::open(&path)?.take(size),
            head: Vec::new(),
        };
        let blob = store.blobs.put(&mut reader)?;
        Ok((blob, media_type(&reader.head)))
    })
    .await;
    let (blob, media) = match stored {
        Ok(stored) => stored,
        Err(e) => {
            tracing::error!(error = %e, job = claim.job_id(), "documents: storing an import failed");
            let cause = match &e {
                BlobError::Io(io) => crate::state::io_cause(io),
                _ => StorageCause::Corrupt,
            };
            let message = format!("storing the file failed ({})", cause.as_str());
            return claim.settle(storage, "storage_unavailable", &message).await;
        }
    };
    if blob.sha256 != sha256 || blob.size != size {
        return claim
            .settle(
                storage,
                "upload_checksum_mismatch",
                "the stored bytes do not match",
            )
            .await;
    }
    let Some(media) = media else {
        return claim
            .settle(
                storage,
                "unsupported_format",
                "slice 1 imports PDF, JPEG and PNG",
            )
            .await;
    };

    #[cfg(test)]
    super::tests::after_store(&upload_id).await;

    // Commit: only while this attempt is running (a cancelled one commits
    // nothing; the runner then settles it), and only while the upload is
    // still importable — its move to `imported` must happen, or nothing does.
    let Some(mut commit) = claim.begin_commit(storage).await? else {
        return Ok(());
    };
    let moved = sqlx::query(&format!(
        "UPDATE uploads SET status = 'imported' WHERE id = ? AND {UPLOAD_LIVE}"
    ))
    .bind(&upload_id)
    .execute(commit.conn())
    .await?
    .rows_affected();
    if moved != 1 {
        drop(commit);
        return claim
            .settle(storage, "not_found", "the upload expired")
            .await;
    }
    let Ok(document_id) = new_id(IdKind::Document) else {
        drop(commit);
        return claim
            .settle(storage, "storage_unavailable", "no randomness for an id")
            .await;
    };
    let title = title.or(filename).unwrap_or_else(|| "Untitled".into());
    sqlx::query("INSERT INTO documents (id, title, media_type, head_revision) VALUES (?, ?, ?, 1)")
        .bind(&document_id)
        .bind(&title)
        .bind(media)
        .execute(commit.conn())
        .await?;
    #[cfg(test)]
    super::tests::fault(claim.job_id(), "after_document")?;
    sqlx::query(
        "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
         VALUES (?, 1, ?, ?, ?, 'import')",
    )
    .bind(&document_id)
    .bind(&blob.sha256)
    .bind(received)
    .bind(media)
    .execute(commit.conn())
    .await?;
    // The document and its r1 go together (0003 guards the head; this
    // guards the pair, #811).
    let r1: i64 =
        sqlx::query_scalar("SELECT count(*) FROM revisions WHERE document_id = ? AND revision = 1")
            .bind(&document_id)
            .fetch_one(commit.conn())
            .await?;
    if r1 != 1 {
        drop(commit);
        return claim
            .settle(storage, "storage_unavailable", "r1 was not written")
            .await;
    }
    sqlx::query(
        "INSERT INTO oplog (op, document_id, revision, origin, details)
         VALUES ('import', ?, 1, 'page', json_object('job_id', ?, 'upload_id', ?))",
    )
    .bind(&document_id)
    .bind(claim.job_id())
    .bind(&upload_id)
    .execute(commit.conn())
    .await?;
    #[cfg(test)]
    super::tests::fault(claim.job_id(), "before_success")?;
    let progress = json!({ "stage": "store", "done": 1, "total": 1, "unit": "file" });
    commit
        .succeed(Some(&document_id), Some(1), Some(&progress))
        .await
}
