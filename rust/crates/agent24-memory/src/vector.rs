//! MD-6: the local vector retriever — an OPTIONAL semantic-recall projection over
//! the assertion ledger (SPEC-MD-ME §3 MD-6; local-first, no mandatory vector
//! service).
//!
//! [`VectorRetriever::reindex`] embeds the owner's current, qualified assertions
//! under the current [`Embedder`]'s `(model_id, revision)` and stores the vectors;
//! [`VectorRetriever::search`] embeds the query and ranks by cosine similarity
//! over ONLY the current model's vectors, joined back to the ledger so the same
//! governance gates as MD-3 apply (current + qualified + owner-scoped).
//!
//! Design points from the acceptance:
//! - **model-change reindex state machine** — vectors are keyed by
//!   `(assertion_id, model_id, revision)`. Switching the embedder makes the new
//!   model's rows simply absent; `search` then FALLS BACK to the FTS retriever
//!   until `reindex` fills them, and old-model rows are left untouched
//!   (mixed-version coexistence; reindex never drops data).
//! - **resumable** — `reindex` embeds only assertions MISSING a current-model
//!   vector, so an interrupted run resumes where it left off and a completed run
//!   is a no-op.
//!
//! The real [`Embedder`] is `OmlxEmbedder` (oMLX, local) — pending D4b's model
//! worker, so it is a documented boundary here; this slice ships the trait seam +
//! storage + cosine + reindex/fallback mechanics with deterministic test
//! embedders. The "semantic recall beats pure FTS" benchmark likewise needs a
//! real model and is deferred, not faked.

use async_trait::async_trait;
use sqlx::{Row, SqlitePool};

use crate::Result;
use crate::assertion::AssertionLedger;
use crate::retriever::{FtsRetriever, Retriever, SearchHit};

/// A reproducible embedding: the vector plus the model identity that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding {
    pub model_id: String,
    pub revision: String,
    pub dims: u32,
    pub normalized: bool,
    pub vector: Vec<f32>,
}

/// Produces embeddings. The seam `OmlxEmbedder` (pending D4b) slots into; tests
/// use deterministic embedders. An embedder MUST be deterministic — the same text
/// yields the same vector — or reindex/search stop being reproducible.
#[async_trait]
pub trait Embedder: Send + Sync {
    fn model_id(&self) -> &str;
    fn revision(&self) -> &str;
    async fn embed(&self, text: &str) -> Result<Embedding>;
}

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Cosine similarity of two equal-length vectors; 0.0 if either is zero-norm or
/// the lengths differ (a mismatched-dims row simply cannot match).
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// SQLite-backed local vector retriever over the shared memory DB, with an FTS
/// fallback for owners/queries with no current-model vectors.
#[derive(Clone)]
pub struct VectorRetriever<E: Embedder + Clone> {
    pool: SqlitePool,
    embedder: E,
    fts: FtsRetriever,
}

impl<E: Embedder + Clone> VectorRetriever<E> {
    pub(crate) fn new(pool: SqlitePool, embedder: E) -> Self {
        let fts = FtsRetriever::new(pool.clone());
        Self {
            pool,
            embedder,
            fts,
        }
    }

    /// Embed the owner's current, qualified assertions that LACK a vector under
    /// the current `(model_id, revision)`, and store them. Resumable: only the
    /// missing ones are embedded, so an interrupted run resumes and a finished run
    /// is a no-op. Returns the number newly embedded.
    pub async fn reindex(&self, owner: &str) -> Result<usize> {
        // Current, qualified assertions with no current-model embedding yet.
        let rows = sqlx::query(
            "SELECT a.id, a.subject, a.predicate, a.object
             FROM mem_assertions a
             WHERE a.scope_owner = ? AND a.recorded_to IS NULL AND a.qualified = 1
               AND NOT EXISTS (
                 SELECT 1 FROM mem_embeddings e
                 WHERE e.assertion_id = a.id AND e.model_id = ? AND e.revision = ?
               )
             ORDER BY a.id ASC",
        )
        .bind(owner)
        .bind(self.embedder.model_id())
        .bind(self.embedder.revision())
        .fetch_all(&self.pool)
        .await?;

        let mut n = 0usize;
        for r in &rows {
            let id: String = r.get("id");
            let subject: String = r.get("subject");
            let predicate: String = r.get("predicate");
            let object: String = r.get("object");
            let text = format!("{subject} {predicate} {object}");
            let emb = self.embedder.embed(&text).await?;
            sqlx::query(
                "INSERT INTO mem_embeddings
                     (assertion_id, scope_owner, model_id, revision, dims, normalized, vec, at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(assertion_id, model_id, revision) DO NOTHING",
            )
            .bind(&id)
            .bind(owner)
            .bind(&emb.model_id)
            .bind(&emb.revision)
            .bind(i64::from(emb.dims))
            .bind(i64::from(emb.normalized))
            .bind(vec_to_blob(&emb.vector))
            .bind(agent24_core::util::now_iso8601())
            .execute(&self.pool)
            .await?;
            n += 1;
        }
        Ok(n)
    }
}

#[async_trait]
impl<E: Embedder + Clone> Retriever for VectorRetriever<E> {
    async fn search(&self, query: &str, owner: &str, limit: usize) -> Result<Vec<SearchHit>> {
        // Load current-model vectors joined to current, qualified assertions.
        let rows = sqlx::query(
            "SELECT a.id, a.scope, a.subject, a.predicate, a.object,
                    a.valid_from, a.valid_to, a.recorded_from, a.recorded_to,
                    a.evidence, a.confidence, a.modality, a.speaker, a.writer_version,
                    a.supersedes, a.qualified, e.vec
             FROM mem_embeddings e
             JOIN mem_assertions a ON a.id = e.assertion_id
             WHERE e.scope_owner = ? AND e.model_id = ? AND e.revision = ?
               AND a.recorded_to IS NULL AND a.qualified = 1",
        )
        .bind(owner)
        .bind(self.embedder.model_id())
        .bind(self.embedder.revision())
        .fetch_all(&self.pool)
        .await?;

        // No current-model vectors → fall back to lexical FTS (local-first, never
        // a hard dependency on the vector index being built).
        if rows.is_empty() {
            return self.fts.search(query, owner, limit).await;
        }

        let q = self.embedder.embed(query).await?;
        let mut scored: Vec<SearchHit> = rows
            .iter()
            .map(|r| {
                let vec = blob_to_vec(&r.get::<Vec<u8>, _>("vec"));
                Ok(SearchHit {
                    score: cosine(&q.vector, &vec),
                    assertion: AssertionLedger::row_to_assertion(r)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        // Deterministic order: score desc, then id asc to break ties.
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.assertion.id.cmp(&b.assertion.id))
        });
        scored.truncate(limit);
        Ok(scored)
    }
}

/// A deterministic test embedder: hashes each whitespace token to a bucket in a
/// fixed-dim bag-of-hashed-tokens vector, L2-normalized. Not semantic (that needs
/// a real model) but STABLE, so the reindex/version/fallback mechanics are
/// testable without a model.
#[derive(Debug, Clone)]
pub struct HashEmbedder {
    pub model_id: String,
    pub revision: String,
    pub dims: usize,
}

impl HashEmbedder {
    pub fn new(model_id: impl Into<String>, revision: impl Into<String>, dims: usize) -> Self {
        Self {
            model_id: model_id.into(),
            revision: revision.into(),
            dims,
        }
    }
}

#[async_trait]
impl Embedder for HashEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }
    fn revision(&self) -> &str {
        &self.revision
    }
    async fn embed(&self, text: &str) -> Result<Embedding> {
        let mut v = vec![0.0f32; self.dims];
        for tok in text.split_whitespace() {
            // FNV-1a over the token → a stable bucket.
            let mut h: u64 = 0xcbf29ce484222325;
            for b in tok.bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x100000001b3);
            }
            v[(h as usize) % self.dims] += 1.0;
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        Ok(Embedding {
            model_id: self.model_id.clone(),
            revision: self.revision.clone(),
            dims: self.dims as u32,
            normalized: true,
            vector: v,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::KvStore;
    use crate::assertion::{Assertion, AssertionStore};
    use crate::event::Scope;
    use serde_json::json;

    async fn store() -> KvStore {
        KvStore::open_memory().await.unwrap()
    }

    async fn assert(kv: &KvStore, id: &str, owner: &str, subject: &str, object: &str) {
        let a = Assertion::new(
            id,
            Scope::owner(owner),
            subject,
            "is",
            json!(object),
            vec!["e".into()],
        );
        kv.assertions().assert(&a).await.unwrap();
    }

    #[tokio::test]
    async fn reindex_embeds_missing_and_is_resumable() {
        let kv = store().await;
        assert(&kv, "a1", "u1", "color", "blue").await;
        assert(&kv, "a2", "u1", "city", "paris").await;
        let v = kv.vector_retriever(HashEmbedder::new("m", "v1", 32));
        assert_eq!(v.reindex("u1").await.unwrap(), 2, "both embedded");
        // Re-run: nothing missing → no-op (resumable/idempotent).
        assert_eq!(v.reindex("u1").await.unwrap(), 0);
        // A new assertion → only it is embedded on the next run.
        assert(&kv, "a3", "u1", "pet", "cat").await;
        assert_eq!(v.reindex("u1").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn cosine_ranks_the_closest_assertion_first() {
        // A hand embedder maps texts to chosen vectors so ranking is assertable.
        #[derive(Clone)]
        struct HandEmbedder;
        #[async_trait]
        impl Embedder for HandEmbedder {
            fn model_id(&self) -> &str {
                "hand"
            }
            fn revision(&self) -> &str {
                "v1"
            }
            async fn embed(&self, text: &str) -> Result<Embedding> {
                let vector = if text.contains("cat") {
                    vec![1.0, 0.0]
                } else if text.contains("dog") {
                    vec![0.9, 0.1]
                } else {
                    vec![0.0, 1.0]
                };
                Ok(Embedding {
                    model_id: "hand".into(),
                    revision: "v1".into(),
                    dims: 2,
                    normalized: false,
                    vector,
                })
            }
        }
        let kv = store().await;
        assert(&kv, "cat", "u1", "pet", "cat").await;
        assert(&kv, "dog", "u1", "pet", "dog").await;
        assert(&kv, "sky", "u1", "weather", "sky").await;
        let v = kv.vector_retriever(HandEmbedder);
        v.reindex("u1").await.unwrap();
        let hits = v.search("cat", "u1", 3).await.unwrap();
        assert_eq!(hits[0].assertion.id, "cat", "exact match first");
        assert_eq!(hits[1].assertion.id, "dog", "closest neighbor second");
        assert!(hits[0].score >= hits[1].score);
    }

    #[tokio::test]
    async fn model_change_falls_back_to_fts_then_reindexes() {
        let kv = store().await;
        assert(&kv, "a1", "u1", "favorite color", "blue").await;
        // Index under model v1.
        let v1 = kv.vector_retriever(HashEmbedder::new("m", "v1", 32));
        v1.reindex("u1").await.unwrap();
        assert!(!v1.search("color", "u1", 5).await.unwrap().is_empty());

        // "Change the model" to v2: no v2 vectors yet → search falls back to FTS
        // (which still finds it lexically), and v1 rows are untouched.
        let v2 = kv.vector_retriever(HashEmbedder::new("m", "v2", 32));
        let fallback = v2.search("color", "u1", 5).await.unwrap();
        assert_eq!(fallback.len(), 1, "FTS fallback while v2 index is empty");

        // Reindex under v2, then v2 search uses vectors; v1 rows still present.
        assert_eq!(v2.reindex("u1").await.unwrap(), 1);
        assert!(!v2.search("color", "u1", 5).await.unwrap().is_empty());
        let v1_rows: i64 = sqlx::query(
            "SELECT COUNT(*) AS n FROM mem_embeddings WHERE model_id='m' AND revision='v1'",
        )
        .fetch_one(&v2.pool)
        .await
        .unwrap()
        .get("n");
        assert_eq!(v1_rows, 1, "reindex never drops the old model's rows");
    }

    #[tokio::test]
    async fn search_is_scope_isolated() {
        let kv = store().await;
        assert(&kv, "a", "alice", "secret", "alpha").await;
        assert(&kv, "b", "bob", "secret", "beta").await;
        let v = kv.vector_retriever(HashEmbedder::new("m", "v1", 32));
        v.reindex("alice").await.unwrap();
        v.reindex("bob").await.unwrap();
        let alice = v.search("secret", "alice", 5).await.unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].assertion.object, json!("alpha"));
    }

    #[tokio::test]
    async fn empty_index_falls_back_to_fts() {
        let kv = store().await;
        assert(&kv, "a1", "u1", "topic", "rust").await;
        // Never reindexed → no vectors → FTS fallback still finds it.
        let v = kv.vector_retriever(HashEmbedder::new("m", "v1", 32));
        let hits = v.search("rust", "u1", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn blob_roundtrips_and_cosine_is_sane() {
        let v = vec![0.5f32, -1.0, 2.0];
        assert_eq!(blob_to_vec(&vec_to_blob(&v)), v);
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 0.0]), 0.0, "mismatched dims → 0");
    }
}
