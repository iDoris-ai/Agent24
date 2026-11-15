//! [`vector::Embedder`] adapter over [`agent24_models::EmbeddingClient`].
//!
//! `revision` is not a version the server tells us. OpenAI-compatible
//! embeddings don't have one. On open we embed a fixed probe, store that
//! vector in the `embed_probe` kv namespace, and hash it. A later open that
//! still cosines at or above [`PROBE_COSINE_MIN`] keeps the stored revision,
//! so fp16 noise does not rebuild the index. Below that, the same model name
//! is treated as new weights and the revision changes.
//!
//! The probe sentence carries [`vector::EMBED_TEXT_FORMAT`]. A model that
//! embeds the sentence returns a different vector when that token changes, so
//! this digest moves with the assertion text layout. The index key appends
//! the same token; this digest does not hash it again. Old rows stay under
//! the old key until a later slice re-embeds them.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;

use agent24_core::util::now_iso8601;
use agent24_models::EmbeddingClient;

use crate::vector::{Embedder, Embedding};
use crate::{KvStore, MemoryError, Result};

/// kv namespace for the canonical probe vector. No migration: `kv` already exists.
pub const EMBED_PROBE_NAMESPACE: &str = "embed_probe";

/// Cosine of the new probe against the stored one, below which the model
/// content counts as changed. Orthogonal probes (cosine 0) must cross it;
/// a threshold of 0 would not.
pub const PROBE_COSINE_MIN: f32 = 0.999;

/// The sentence embedded to name a revision. The format token is
/// [`crate::vector::EMBED_TEXT_FORMAT`], the same one the index key appends.
pub fn probe_input() -> String {
    format!(
        "agent24-embed-probe text-format={}",
        crate::vector::EMBED_TEXT_FORMAT
    )
}

/// `<dims>:<sha256 of the 3-decimal probe, first 12 hex chars>`.
/// The text-format token stays in [`probe_input`] and in the index key.
pub fn revision_of(vector: &[f32]) -> String {
    let mut payload = String::with_capacity(vector.len() * 8);
    for (i, component) in vector.iter().enumerate() {
        if i > 0 {
            payload.push(',');
        }
        payload.push_str(&format!("{component:.3}"));
    }
    let digest = Sha256::digest(payload.as_bytes());
    let mut hex = String::with_capacity(12);
    for byte in digest.iter().take(6) {
        hex.push_str(&format!("{byte:02x}"));
    }
    format!("{}:{hex}", vector.len())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredProbe {
    revision: String,
    text_format: String,
    dims: u32,
    vector: Vec<f32>,
}

/// Embedder whose `revision` is pinned by the probe vector in `kv`.
#[derive(Clone)]
pub struct LocalEmbedder {
    client: EmbeddingClient,
    model_id: String,
    revision: String,
}

impl LocalEmbedder {
    /// Embed the probe, compare it with the stored one, and keep or replace
    /// the revision. The stored vector is left untouched when the cosine still
    /// holds, so a slow drift of noisy probes cannot walk the revision.
    pub async fn open(kv: &KvStore, client: EmbeddingClient) -> Result<Self> {
        let model_id = client.model().to_owned();
        let mut probed = client.embed(&[&probe_input()]).await.map_err(embed_err)?;
        let vector = one_vector(&mut probed)?;
        let revision = claim_revision(kv, &model_id, &vector).await?;
        Ok(Self {
            client,
            model_id,
            revision,
        })
    }
}

/// Compare the probe with the stored row and write a replacement, under one
/// write lock. The lock is taken after the embed call. A row that does not
/// decode is a miss and is overwritten. When two opens race, the later one
/// reads the row the earlier one committed and keeps that revision while the
/// cosine still holds.
async fn claim_revision(kv: &KvStore, model_id: &str, vector: &[f32]) -> Result<String> {
    let fresh = revision_of(vector);
    let dims = u32::try_from(vector.len())
        .map_err(|_| MemoryError::Embedder("embedding dims do not fit u32".into()))?;
    let mut tx = kv.pool().begin_with("BEGIN IMMEDIATE").await?;
    let row = sqlx::query("SELECT value FROM kv WHERE namespace = ? AND key = ?")
        .bind(EMBED_PROBE_NAMESPACE)
        .bind(model_id)
        .fetch_optional(&mut *tx)
        .await?;
    // A row that does not decode is a miss. The write below replaces it.
    let stored = row.and_then(|row| serde_json::from_str::<StoredProbe>(row.get("value")).ok());
    if let Some(prev) = stored
        && prev.text_format == crate::vector::EMBED_TEXT_FORMAT
        && prev.dims as usize == vector.len()
        && prev.vector.len() == vector.len()
        && crate::vector::cosine(&prev.vector, vector) >= PROBE_COSINE_MIN
    {
        tx.commit().await?;
        return Ok(prev.revision);
    }
    let value = serde_json::to_string(&StoredProbe {
        revision: fresh.clone(),
        text_format: crate::vector::EMBED_TEXT_FORMAT.to_owned(),
        dims,
        vector: vector.to_vec(),
    })?;
    sqlx::query(
        "INSERT INTO kv (namespace, key, value, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(namespace, key) DO UPDATE SET
             value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(EMBED_PROBE_NAMESPACE)
    .bind(model_id)
    .bind(value)
    .bind(now_iso8601())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(fresh)
}

fn one_vector(vectors: &mut Vec<Vec<f32>>) -> Result<Vec<f32>> {
    if vectors.len() != 1 {
        return Err(MemoryError::Embedder(format!(
            "embedding count {} != 1",
            vectors.len()
        )));
    }
    Ok(vectors.swap_remove(0))
}

fn embed_err(err: agent24_models::ModelError) -> MemoryError {
    MemoryError::Embedder(err.to_string())
}

fn is_unit(vector: &[f32]) -> bool {
    let norm = vector
        .iter()
        .map(|component| component * component)
        .sum::<f32>()
        .sqrt();
    (norm - 1.0).abs() <= 1e-3
}

#[async_trait]
impl Embedder for LocalEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn revision(&self) -> &str {
        &self.revision
    }

    async fn embed(&self, text: &str) -> Result<Embedding> {
        let mut vectors = self.client.embed(&[text]).await.map_err(embed_err)?;
        let vector = one_vector(&mut vectors)?;
        let dims = u32::try_from(vector.len())
            .map_err(|_| MemoryError::Embedder("embedding dims do not fit u32".into()))?;
        Ok(Embedding {
            model_id: self.model_id.clone(),
            revision: self.revision.clone(),
            dims,
            normalized: is_unit(&vector),
            vector,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::time::Duration;

    use agent24_models::{DEFAULT_EMBED_MODEL, EMBED_DIMS, EmbeddingClient};
    use sqlx::Connection;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    fn axis(axis: usize) -> Vec<f64> {
        let mut vector = vec![0.0; EMBED_DIMS];
        vector[axis] = 1.0;
        vector
    }

    fn body(vector: &[f64]) -> String {
        serde_json::json!({
            "object": "list",
            "data": [{"object": "embedding", "index": 0, "embedding": vector}],
            "model": DEFAULT_EMBED_MODEL,
        })
        .to_string()
    }

    /// One response per entry, in order. The handle resolves to the request bodies.
    /// The receiver fires once after each response is written.
    async fn serve(
        vectors: Vec<Vec<f64>>,
    ) -> (
        String,
        tokio::task::JoinHandle<Vec<String>>,
        tokio::sync::mpsc::UnboundedReceiver<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (done_tx, done_rx) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let mut captured = Vec::new();
            for vector in vectors {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut got = Vec::new();
                let mut buf = [0u8; 8192];
                let (header_end, len) = loop {
                    let nread = sock.read(&mut buf).await.unwrap();
                    assert!(nread > 0, "client closed before the request ended");
                    got.extend_from_slice(&buf[..nread]);
                    if let Some(idx) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&got[..idx]);
                        let len = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if got.len() >= idx + 4 + len {
                            break (idx + 4, len);
                        }
                    }
                };
                captured
                    .push(String::from_utf8_lossy(&got[header_end..header_end + len]).into_owned());
                let payload = body(&vector);
                let raw = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                sock.write_all(raw.as_bytes()).await.unwrap();
                let _ = done_tx.send(());
            }
            captured
        });
        (url, server, done_rx)
    }

    fn client(url: &str) -> EmbeddingClient {
        EmbeddingClient::direct(url, "k", DEFAULT_EMBED_MODEL)
            .unwrap()
            .with_timeout(Duration::from_secs(5))
    }

    #[tokio::test]
    async fn probe_revision_is_stable_and_embed_returns_it() {
        let (url, server, _done) = serve(vec![axis(0), axis(0), axis(0)]).await;
        let kv = KvStore::open_memory().await.unwrap();
        let first = LocalEmbedder::open(&kv, client(&url)).await.unwrap();
        let second = LocalEmbedder::open(&kv, client(&url)).await.unwrap();
        assert_eq!(first.model_id(), DEFAULT_EMBED_MODEL);
        assert_eq!(
            first.revision(),
            second.revision(),
            "same probe keeps the revision"
        );
        assert!(
            first.revision().starts_with("1024:"),
            "{}",
            first.revision()
        );
        assert_eq!(first.revision().len(), "1024:".len() + 12);

        let embedding = first.embed("你好").await.unwrap();
        assert_eq!(embedding.model_id, DEFAULT_EMBED_MODEL);
        assert_eq!(embedding.revision, first.revision());
        assert_eq!(embedding.dims, EMBED_DIMS as u32);
        assert_eq!(embedding.vector.len(), EMBED_DIMS);
        assert!(
            embedding
                .vector
                .iter()
                .all(|component| component.is_finite())
        );
        assert!(embedding.normalized);
        assert_eq!(embedding.vector[0], 1.0);

        let seen = server.await.unwrap();
        assert!(
            seen.iter()
                .any(|body| body.contains(crate::vector::EMBED_TEXT_FORMAT)),
            "probe input carries the text-format token: {seen:?}"
        );
        assert!(
            seen.iter().any(|body| body.contains("agent24-embed-probe")),
            "{seen:?}"
        );
        let keys = kv.keys(EMBED_PROBE_NAMESPACE).await.unwrap();
        assert_eq!(keys, vec![DEFAULT_EMBED_MODEL.to_owned()]);
    }

    #[tokio::test]
    async fn same_model_name_with_new_vectors_changes_revision() {
        // Axis 0 then axis 1: cosine is 0, which is below 0.999 and not below 0.
        let (url, server, _done) = serve(vec![axis(0), axis(1)]).await;
        let kv = KvStore::open_memory().await.unwrap();
        let first = LocalEmbedder::open(&kv, client(&url)).await.unwrap();
        let second = LocalEmbedder::open(&kv, client(&url)).await.unwrap();
        assert_ne!(
            first.revision(),
            second.revision(),
            "a probe cosine under {PROBE_COSINE_MIN} must mint a new revision"
        );
        assert!(second.revision().starts_with("1024:"));
        server.await.unwrap();
    }

    #[test]
    fn revision_digest_hashes_the_vector_decimals() {
        let vector = [1.0_f32, 0.0];
        let mut payload = String::new();
        for (i, component) in vector.iter().enumerate() {
            if i > 0 {
                payload.push(',');
            }
            payload.push_str(&format!("{component:.3}"));
        }
        let digest = Sha256::digest(payload.as_bytes());
        let mut hex = String::new();
        for byte in digest.iter().take(6) {
            hex.push_str(&format!("{byte:02x}"));
        }
        assert_eq!(revision_of(&vector), format!("2:{hex}"));
        assert_eq!(
            probe_input(),
            format!(
                "agent24-embed-probe text-format={}",
                crate::vector::EMBED_TEXT_FORMAT
            )
        );
        assert!(!probe_input().contains("assertion-text-v1"));
    }

    #[tokio::test]
    async fn undecodable_probe_row_is_replaced() {
        let (url, server, _done) = serve(vec![axis(0), axis(0)]).await;
        let kv = KvStore::open_memory().await.unwrap();
        sqlx::query("INSERT INTO kv (namespace, key, value, updated_at) VALUES (?, ?, ?, ?)")
            .bind(EMBED_PROBE_NAMESPACE)
            .bind(DEFAULT_EMBED_MODEL)
            .bind("{")
            .bind("t")
            .execute(crate::test_hooks::pool(&kv))
            .await
            .unwrap();
        let first = LocalEmbedder::open(&kv, client(&url)).await.unwrap();
        let second = LocalEmbedder::open(&kv, client(&url)).await.unwrap();
        assert_eq!(first.revision(), second.revision());
        let stored = kv
            .fetch::<StoredProbe>(EMBED_PROBE_NAMESPACE, DEFAULT_EMBED_MODEL)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.revision, first.revision());
        assert_eq!(stored.text_format, crate::vector::EMBED_TEXT_FORMAT);
        server.await.unwrap();
    }

    fn near_axis() -> Vec<f64> {
        let mut vector = vec![0.0; EMBED_DIMS];
        vector[0] = 0.999;
        vector[1] = 0.01;
        vector
    }

    #[tokio::test]
    async fn concurrent_opens_agree_on_the_stored_revision() {
        let first = axis(0);
        let second = near_axis();
        let as_f32 = |values: &[f64]| {
            values
                .iter()
                .map(|component| *component as f32)
                .collect::<Vec<_>>()
        };
        let left = as_f32(&first);
        let right = as_f32(&second);
        assert!(
            crate::vector::cosine(&left, &right) >= PROBE_COSINE_MIN,
            "{}",
            crate::vector::cosine(&left, &right)
        );
        assert_ne!(revision_of(&left), revision_of(&right));

        let dir = tempfile::tempdir().unwrap();
        let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
        let pool = crate::test_hooks::pool(&kv).clone();
        let mut hold = pool.acquire().await.unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *hold)
            .await
            .unwrap();
        let probe_opts = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(dir.path().join("memory.db"))
            .busy_timeout(Duration::from_millis(50));
        let mut probe = sqlx::SqliteConnection::connect_with(&probe_opts)
            .await
            .unwrap();
        let blocked = sqlx::query("BEGIN IMMEDIATE").execute(&mut probe).await;
        assert!(
            blocked.is_err(),
            "held BEGIN IMMEDIATE did not block a second writer: {blocked:?}"
        );

        let (url, server, mut done) = serve(vec![first, second]).await;
        let left_url = url.clone();
        let right_url = url;
        let left_kv = kv.clone();
        let right_kv = kv.clone();
        let left_open =
            tokio::spawn(async move { LocalEmbedder::open(&left_kv, client(&left_url)).await });
        let right_open =
            tokio::spawn(async move { LocalEmbedder::open(&right_kv, client(&right_url)).await });
        done.recv().await.unwrap();
        done.recv().await.unwrap();
        // Both probes have a response. Hold the write lock until each open
        // has checked out a connection for its claim, so the two reads are
        // not ordered by the release.
        let wait_started = std::time::Instant::now();
        let idle_before = pool.num_idle();
        while pool.num_idle() + 2 > idle_before && wait_started.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        sqlx::query("ROLLBACK").execute(&mut *hold).await.unwrap();
        drop(hold);

        let left_embedder = left_open.await.unwrap().unwrap();
        let right_embedder = right_open.await.unwrap().unwrap();
        assert_eq!(left_embedder.revision(), right_embedder.revision());
        let stored = kv
            .fetch::<StoredProbe>(EMBED_PROBE_NAMESPACE, DEFAULT_EMBED_MODEL)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.revision, left_embedder.revision());
        server.await.unwrap();
    }
}
