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
//!   `(assertion_id, model_id, revision)`. The stored revision is
//!   `{embedder revision}:{EMBED_TEXT_FORMAT}`, so a change to the text fed to
//!   the embedder is a different key and the old rows are embedded again.
//!   Switching the embedder makes the new key's rows absent; `search` then
//!   FALLS BACK to the FTS retriever until `reindex` fills them. Once that key
//!   is complete for the owner, `reindex` deletes the owner's rows under every
//!   other key. Switching back to an older model re-embeds every current fact
//!   (a few thousand rows is an acceptable cost; plan §3.6).
//! - **resumable** — `reindex` embeds assertions that lack a current-key vector,
//!   so an interrupted run resumes where it left off. A finished run whose key
//!   is already complete inserts nothing, and still deletes the other keys.
//!
//! The production embedder is [`crate::embedder::LocalEmbedder`] (loopback oMLX
//! or iDoris). Daemon startup attaches it from `agent24d`'s embed backfill.
//! This module is the trait, the storage, and the cosine / reindex mechanics.
//! CI exercises them with deterministic test embedders. The real-model Hit@5
//! comparison is the ignored `eval_m1_omlx` test, not part of CI.

use async_trait::async_trait;
use sqlx::{Row, SqlitePool};

use crate::assertion::AssertionLedger;
use crate::retriever::{FtsRetriever, Retriever, SearchHit, is_searchable_query};
use crate::{MemoryError, Result};

/// A reproducible embedding: the vector plus the model identity that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding {
    pub model_id: String,
    pub revision: String,
    pub dims: u32,
    pub normalized: bool,
    pub vector: Vec<f32>,
}

/// Produces embeddings. [`crate::embedder::LocalEmbedder`] is the loopback
/// client; tests use deterministic embedders. An embedder MUST be deterministic
/// for a fixed `(model_id, revision)` — the same text yields the same vector —
/// or reindex and search stop being reproducible.
#[async_trait]
pub trait Embedder: Send + Sync {
    fn model_id(&self) -> &str;
    fn revision(&self) -> &str;
    async fn embed(&self, text: &str) -> Result<Embedding>;
}

/// Generation of the text [`VectorRetriever::reindex`] feeds the embedder.
/// Stored in the revision key (`{embedder revision}:{EMBED_TEXT_FORMAT}`) so
/// rows embedded from the older JSON-quoted object do not share a key with the
/// plain-text object. VR-1's startup probe must fold this same token into its
/// probe sentence so the probe digest moves when the format changes.
pub const EMBED_TEXT_FORMAT: &str = "txt1";

pub(crate) fn index_revision(model_revision: &str) -> String {
    format!("{model_revision}:{EMBED_TEXT_FORMAT}")
}

/// `object` is a JSON value. A JSON string is embedded as its text; any other
/// JSON value is embedded as compact JSON.
fn object_plain_text(object_json: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(object_json) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => object_json.to_owned(),
    }
}

fn embed_input(subject: &str, predicate: &str, object_json: &str) -> String {
    format!("{subject} {predicate} {}", object_plain_text(object_json))
}

pub(crate) fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

pub(crate) fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    // Manual 4-byte stride rather than `chunks_exact(4)` (CI clippy flags a
    // constant chunk size); trailing <4 bytes are impossible given the migration's
    // `CHECK(length(vec) = dims * 4)` but are simply ignored here.
    let mut out = Vec::with_capacity(b.len() / 4);
    let mut i = 0;
    while i + 4 <= b.len() {
        out.push(f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]));
        i += 4;
    }
    out
}

/// Cosine similarity of two equal-length vectors; 0.0 if either is zero-norm or
/// the lengths differ (a mismatched-dims row simply cannot match).
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> f32 {
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

/// Reject a misbehaving embedder's output (see [`VectorRetriever::check_embedding`]).
pub(crate) fn validate_embedding<E: Embedder + ?Sized>(
    embedder: &E,
    emb: &Embedding,
) -> Result<()> {
    if emb.model_id != embedder.model_id() || emb.revision != embedder.revision() {
        return Err(MemoryError::Embedder(format!(
            "declared {}/{} but returned {}/{}",
            embedder.model_id(),
            embedder.revision(),
            emb.model_id,
            emb.revision
        )));
    }
    if emb.dims as usize != emb.vector.len() {
        return Err(MemoryError::Embedder(format!(
            "dims {} != vector length {}",
            emb.dims,
            emb.vector.len()
        )));
    }
    if !emb.vector.iter().all(|x| x.is_finite()) {
        return Err(MemoryError::Embedder(
            "non-finite vector component".to_owned(),
        ));
    }
    Ok(())
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

    /// Reject a misbehaving embedder before its output can corrupt the index or a
    /// search: the returned identity must match the declared one (else reindex
    /// would embed forever and search never find it, review #123 M1), the vector
    /// length must equal `dims` (M2), and every component must be finite (M3).
    fn check_embedding(&self, emb: &Embedding) -> Result<()> {
        validate_embedding(&self.embedder, emb)
    }

    /// Embed the owner's current, qualified assertions that LACK a vector under
    /// the current `(model_id, revision)`, and store them. Resumable: the missing
    /// ones are embedded, so an interrupted run resumes. Returns the number newly
    /// embedded. When the current key then covers every current qualified fact,
    /// rows under every other key for this owner are deleted.
    pub async fn reindex(&self, owner: &str) -> Result<usize> {
        let revision = index_revision(self.embedder.revision());
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
        .bind(&revision)
        .fetch_all(&self.pool)
        .await?;

        let mut n = 0usize;
        // One fact that never embeds (for example text the model rejects) must
        // not keep every later fact without a vector. Skip it and go on; only
        // when every row fails is the error returned (embedder down).
        let mut first_err = None;
        for r in &rows {
            let id: String = r.get("id");
            let subject: String = r.get("subject");
            let predicate: String = r.get("predicate");
            let object: String = r.get("object");
            let text = embed_input(&subject, &predicate, &object);
            let emb = match self.embedder.embed(&text).await {
                Ok(emb) => match self.check_embedding(&emb) {
                    Ok(()) => emb,
                    Err(err) => {
                        tracing::warn!(error = %err, "skipping a fact whose embedding is invalid");
                        first_err.get_or_insert(err);
                        continue;
                    }
                },
                Err(err) => {
                    tracing::warn!(error = %err, "skipping a fact that failed to embed");
                    first_err.get_or_insert(err);
                    continue;
                }
            };
            // The fact can be retracted while `embed` runs. Validity is decided
            // in this statement, so a close that has already committed inserts
            // zero rows and leaves no orphan.
            let res = sqlx::query(
                "INSERT INTO mem_embeddings
                     (assertion_id, scope_owner, model_id, revision, dims, normalized, vec, at)
                 SELECT ?, ?, ?, ?, ?, ?, ?, ?
                 WHERE EXISTS (
                     SELECT 1 FROM mem_assertions a
                     WHERE a.id = ? AND a.scope_owner = ?
                       AND a.recorded_to IS NULL AND a.qualified = 1
                 )
                 ON CONFLICT(assertion_id, model_id, revision) DO NOTHING",
            )
            .bind(&id)
            .bind(owner)
            .bind(&emb.model_id)
            .bind(&revision)
            .bind(i64::from(emb.dims))
            .bind(i64::from(emb.normalized))
            .bind(vec_to_blob(&emb.vector))
            .bind(agent24_core::util::now_iso8601())
            .bind(&id)
            .bind(owner)
            .execute(&self.pool)
            .await?;
            // Count rows actually inserted, not conflicts or skipped closes.
            n += res.rows_affected() as usize;
        }
        if let Some(err) = first_err
            && n == 0
        {
            return Err(err);
        }
        self.drop_other_revisions_if_complete(owner, &revision)
            .await?;
        Ok(n)
    }

    /// Current qualified facts for `owner`, and how many of them have a vector
    /// under this embedder's key. `(embedded, total)`.
    pub async fn coverage(&self, owner: &str) -> Result<(u64, u64)> {
        let revision = index_revision(self.embedder.revision());
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM mem_assertions a
             WHERE a.scope_owner = ? AND a.recorded_to IS NULL AND a.qualified = 1",
        )
        .bind(owner)
        .fetch_one(&self.pool)
        .await?;
        let embedded: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM mem_embeddings e
             WHERE e.scope_owner = ? AND e.model_id = ? AND e.revision = ?
               AND EXISTS (
                 SELECT 1 FROM mem_assertions a
                 WHERE a.id = e.assertion_id AND a.recorded_to IS NULL AND a.qualified = 1
               )",
        )
        .bind(owner)
        .bind(self.embedder.model_id())
        .bind(revision)
        .fetch_one(&self.pool)
        .await?;
        Ok((
            u64::try_from(embedded).unwrap_or(0),
            u64::try_from(total).unwrap_or(0),
        ))
    }

    /// Delete this owner's embeddings outside the current key. The completeness
    /// read and the delete share one write transaction, so a fact inserted
    /// after the read cannot land in between. A key that is still missing a
    /// current qualified fact keeps the other rows: search of a partial key
    /// falls back to FTS, and switching back to the older model still finds
    /// its vectors.
    async fn drop_other_revisions_if_complete(&self, owner: &str, revision: &str) -> Result<()> {
        // Most reindex calls have nothing outside the current key; skip the
        // write lock then.
        let stale: i64 = sqlx::query(
            "SELECT EXISTS(
                 SELECT 1 FROM mem_embeddings
                 WHERE scope_owner = ? AND NOT (model_id = ? AND revision = ?)
             ) AS stale",
        )
        .bind(owner)
        .bind(self.embedder.model_id())
        .bind(revision)
        .fetch_one(&self.pool)
        .await?
        .get("stale");
        if stale == 0 {
            return Ok(());
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let incomplete: i64 = sqlx::query(
            "SELECT EXISTS(
                 SELECT 1 FROM mem_assertions a
                 WHERE a.scope_owner = ? AND a.recorded_to IS NULL AND a.qualified = 1
                   AND NOT EXISTS (
                     SELECT 1 FROM mem_embeddings e
                     WHERE e.assertion_id = a.id AND e.model_id = ? AND e.revision = ?
                   )
             ) AS inc",
        )
        .bind(owner)
        .bind(self.embedder.model_id())
        .bind(revision)
        .fetch_one(&mut *tx)
        .await?
        .get("inc");
        if incomplete != 0 {
            tx.rollback().await?;
            return Ok(());
        }
        sqlx::query(
            "DELETE FROM mem_embeddings
             WHERE scope_owner = ?
               AND NOT (model_id = ? AND revision = ?)",
        )
        .bind(owner)
        .bind(self.embedder.model_id())
        .bind(revision)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Delete every embedding stored under `owner`, every model and revision.
    /// The rows are a projection: a later [`Self::reindex`] fills the current
    /// key back in. Purge calls [`crate::KvStore::delete_owner_embeddings`].
    pub async fn delete_owner_embeddings(&self, owner: &str) -> Result<u64> {
        delete_owner_rows(&self.pool, owner).await
    }
}

pub(crate) async fn delete_owner_rows(pool: &SqlitePool, owner: &str) -> Result<u64> {
    // `mem_embeddings.scope_owner` is a copy; the ledger owner decides (#123
    // M4). Match either so a drifted copy is still purged.
    let res = sqlx::query(
        "DELETE FROM mem_embeddings
         WHERE scope_owner = ?
            OR assertion_id IN (SELECT id FROM mem_assertions WHERE scope_owner = ?)",
    )
    .bind(owner)
    .bind(owner)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

#[async_trait]
impl<E: Embedder + Clone> Retriever for VectorRetriever<E> {
    async fn search(&self, query: &str, owner: &str, limit: usize) -> Result<Vec<SearchHit>> {
        // A degenerate query returns nothing (never errors) — the SAME trait
        // contract FtsRetriever honors, checked BEFORE the embedder (review #123 B2).
        if !is_searchable_query(query) {
            return Ok(Vec::new());
        }
        let revision = index_revision(self.embedder.revision());

        // Probe completeness and fetch vectors in ONE read transaction, so a
        // concurrent insert cannot slip between the two and make the "complete"
        // verdict stale.
        let mut tx = self.pool.begin().await?;
        // Fall back to FTS unless the index is COMPLETE for this owner: a PARTIAL
        // index (the normal window between reindex runs) must not silently miss
        // un-embedded assertions by returning only the embedded fraction (B1).
        let incomplete: i64 = sqlx::query(
            "SELECT EXISTS(
                 SELECT 1 FROM mem_assertions a
                 WHERE a.scope_owner = ? AND a.recorded_to IS NULL AND a.qualified = 1
                   AND NOT EXISTS (
                     SELECT 1 FROM mem_embeddings e
                     WHERE e.assertion_id = a.id AND e.model_id = ? AND e.revision = ?
                   )
             ) AS inc",
        )
        .bind(owner)
        .bind(self.embedder.model_id())
        .bind(&revision)
        .fetch_one(&mut *tx)
        .await?
        .get("inc");
        if incomplete != 0 {
            drop(tx); // read-only; nothing to commit
            return self.fts.search(query, owner, limit).await;
        }

        // Complete index: rank by cosine over the current model's vectors. Isolate
        // by the AUTHORITY owner (a.scope_owner), not the projection's redundant
        // copy (review #123 M4).
        let rows = sqlx::query(
            "SELECT a.id, a.scope, a.subject, a.predicate, a.object,
                    a.valid_from, a.valid_to, a.recorded_from, a.recorded_to,
                    a.evidence, a.source_ref, a.confidence, a.modality, a.speaker, a.writer_version,
                    a.supersedes, a.qualified, e.vec
             FROM mem_embeddings e
             JOIN mem_assertions a ON a.id = e.assertion_id
             WHERE a.scope_owner = ? AND e.model_id = ? AND e.revision = ?
               AND a.recorded_to IS NULL AND a.qualified = 1",
        )
        .bind(owner)
        .bind(self.embedder.model_id())
        .bind(&revision)
        .fetch_all(&mut *tx)
        .await?;
        drop(tx);
        if rows.is_empty() {
            return Ok(Vec::new()); // complete + empty = the owner has no beliefs
        }

        let q = self.embedder.embed(query).await?;
        self.check_embedding(&q)?;
        let mut scored: Vec<SearchHit> = Vec::with_capacity(rows.len());
        for r in &rows {
            let vec = blob_to_vec(&r.get::<Vec<u8>, _>("vec"));
            let score = cosine(&q.vector, &vec);
            // Drop a non-finite score rather than let a NaN break the total order
            // (review #123 M3); cosine already handles zero-norm/len-mismatch.
            if !score.is_finite() {
                continue;
            }
            scored.push(SearchHit {
                score,
                assertion: AssertionLedger::row_to_assertion(r)?,
            });
        }
        // Total order: score desc (total_cmp), then id asc to break ties.
        scored.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
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
            // Clamp to at least 1: dims == 0 would panic on the `% self.dims` in
            // embed (review #123 minor).
            dims: dims.max(1),
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

        assert(&kv, "b1", "u2", "favorite color", "blue").await;
        v1.reindex("u2").await.unwrap();

        // "Change the model" to v2: no v2 vectors yet → search falls back to FTS
        // (which still finds it lexically). v1 rows are still there.
        let v2 = kv.vector_retriever(HashEmbedder::new("m", "v2", 32));
        let fallback = v2.search("color", "u1", 5).await.unwrap();
        assert_eq!(fallback.len(), 1, "FTS fallback while v2 index is empty");

        // Reindex under v2. u1's key is complete, so u1's v1 rows go. u2 is a
        // different owner and keeps its v1 rows.
        assert_eq!(v2.reindex("u1").await.unwrap(), 1);
        assert!(!v2.search("color", "u1", 5).await.unwrap().is_empty());
        let v1_key = index_revision("v1");
        assert_eq!(rows_for(&v2.pool, "u1", &v1_key).await, 0);
        assert_eq!(rows_for(&v2.pool, "u2", &v1_key).await, 1);

        // A later v2 run inserts nothing, and a stale key inserted underneath
        // is removed because the v2 key is already complete.
        sqlx::query(
            "INSERT INTO mem_embeddings
                 (assertion_id, scope_owner, model_id, revision, dims, normalized, vec, at)
             SELECT assertion_id, scope_owner, model_id, 'stale', dims, normalized, vec, at
             FROM mem_embeddings WHERE scope_owner = 'u1' LIMIT 1",
        )
        .execute(&v2.pool)
        .await
        .unwrap();
        assert_eq!(v2.reindex("u1").await.unwrap(), 0);
        assert_eq!(rows_for(&v2.pool, "u1", "stale").await, 0);
        assert_eq!(rows_for(&v2.pool, "u2", &v1_key).await, 1);

        // Switching back to v1 finds nothing stored and embeds the fact again.
        assert_eq!(v1.reindex("u1").await.unwrap(), 1);
        assert_eq!(rows_for(&v1.pool, "u1", &v1_key).await, 1);
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

    #[tokio::test]
    async fn partial_index_falls_back_to_fts_not_confident_wrong_answer() {
        // B1: a1 embedded, a2 added after → the index is INCOMPLETE. Searching for
        // a2's term must fall back to FTS (and find a2), not return only the
        // embedded fraction with a confident wrong hit.
        let kv = store().await;
        assert(&kv, "a1", "u1", "color", "blue").await;
        let v = kv.vector_retriever(HashEmbedder::new("m", "v1", 32));
        v.reindex("u1").await.unwrap();
        assert(&kv, "a2", "u1", "city", "zanzibar").await; // NOT reindexed
        let hits = v.search("zanzibar", "u1", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].assertion.id, "a2",
            "FTS fallback finds the un-embedded one"
        );
    }

    #[tokio::test]
    async fn degenerate_query_returns_nothing_like_fts() {
        // B2: same trait contract as FtsRetriever, on a fully-built index.
        let kv = store().await;
        assert(&kv, "a1", "u1", "topic", "rust").await;
        let v = kv.vector_retriever(HashEmbedder::new("m", "v1", 32));
        v.reindex("u1").await.unwrap();
        assert!(v.search("", "u1", 5).await.unwrap().is_empty());
        assert!(v.search("   ", "u1", 5).await.unwrap().is_empty());
        assert!(v.search("!@#$%", "u1", 5).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn misbehaving_embedder_is_rejected_not_re_embedded_forever() {
        // M1: an embedder whose returned identity disagrees with its declared one
        // would otherwise be re-embedded every run and never found → hard error.
        #[derive(Clone)]
        struct LiarEmbedder;
        #[async_trait]
        impl Embedder for LiarEmbedder {
            fn model_id(&self) -> &str {
                "declared"
            }
            fn revision(&self) -> &str {
                "v1"
            }
            async fn embed(&self, _t: &str) -> Result<Embedding> {
                Ok(Embedding {
                    model_id: "actual".into(),
                    revision: "v9".into(),
                    dims: 2,
                    normalized: false,
                    vector: vec![1.0, 0.0],
                })
            }
        }
        let kv = store().await;
        assert(&kv, "a1", "u1", "x", "y").await;
        let v = kv.vector_retriever(LiarEmbedder);
        assert!(matches!(
            v.reindex("u1").await.unwrap_err(),
            MemoryError::Embedder(_)
        ));
    }

    #[tokio::test]
    async fn dims_mismatch_is_rejected() {
        // M2: dims=384 but a 2-float vector must not land as a silently-0-scoring row.
        #[derive(Clone)]
        struct WrongDims;
        #[async_trait]
        impl Embedder for WrongDims {
            fn model_id(&self) -> &str {
                "m"
            }
            fn revision(&self) -> &str {
                "v1"
            }
            async fn embed(&self, _t: &str) -> Result<Embedding> {
                Ok(Embedding {
                    model_id: "m".into(),
                    revision: "v1".into(),
                    dims: 384,
                    normalized: true,
                    vector: vec![1.0, 0.0],
                })
            }
        }
        let kv = store().await;
        assert(&kv, "a1", "u1", "x", "y").await;
        let v = kv.vector_retriever(WrongDims);
        assert!(matches!(
            v.reindex("u1").await.unwrap_err(),
            MemoryError::Embedder(_)
        ));
    }

    #[tokio::test]
    async fn isolation_uses_authority_owner_not_the_projection_copy() {
        // M4: even if a projection row's redundant scope_owner is tampered, the
        // authority (mem_assertions.scope_owner) gates the result — no leak.
        let kv = store().await;
        assert(&kv, "victim", "alice", "secret", "alpha").await;
        let v = kv.vector_retriever(HashEmbedder::new("m", "v1", 32));
        v.reindex("alice").await.unwrap();
        // Tamper the projection's owner copy to "bob".
        sqlx::query("UPDATE mem_embeddings SET scope_owner = 'bob' WHERE assertion_id = 'victim'")
            .execute(&v.pool)
            .await
            .unwrap();
        // bob must NOT see alice's assertion (the join filters on a.scope_owner).
        assert!(v.search("secret", "bob", 5).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn hash_embedder_zero_dims_does_not_panic() {
        let e = HashEmbedder::new("m", "v1", 0);
        let emb = e.embed("hello world").await.unwrap();
        assert_eq!(emb.dims, 1, "clamped to 1, no modulo-by-zero panic");
    }

    #[test]
    fn blob_roundtrips_and_cosine_is_sane() {
        let v = vec![0.5f32, -1.0, 2.0];
        assert_eq!(blob_to_vec(&vec_to_blob(&v)), v);
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 0.0]), 0.0, "mismatched dims → 0");
    }

    #[test]
    fn plain_object_text_unwraps_a_json_string() {
        assert_eq!(object_plain_text("\"blue\""), "blue");
        assert_eq!(
            embed_input("user", "said_to_remember", "\"blue\""),
            "user said_to_remember blue"
        );
        assert_eq!(object_plain_text("{\"k\":1}"), "{\"k\":1}");
        assert_eq!(index_revision("v1"), format!("v1:{EMBED_TEXT_FORMAT}"));
    }

    async fn rows_for(pool: &SqlitePool, owner: &str, revision: &str) -> i64 {
        sqlx::query(
            "SELECT COUNT(*) AS n FROM mem_embeddings WHERE scope_owner = ? AND revision = ?",
        )
        .bind(owner)
        .bind(revision)
        .fetch_one(pool)
        .await
        .unwrap()
        .get("n")
    }

    async fn rows_of(pool: &SqlitePool, assertion_id: &str) -> i64 {
        sqlx::query("SELECT COUNT(*) AS n FROM mem_embeddings WHERE assertion_id = ?")
            .bind(assertion_id)
            .fetch_one(pool)
            .await
            .unwrap()
            .get("n")
    }

    async fn insert_vec(pool: &SqlitePool, id: &str, owner: &str, model: &str, revision: &str) {
        sqlx::query(
            "INSERT INTO mem_embeddings
                 (assertion_id, scope_owner, model_id, revision, dims, normalized, vec, at)
             VALUES (?, ?, ?, ?, 1, 1, ?, 't')",
        )
        .bind(id)
        .bind(owner)
        .bind(model)
        .bind(revision)
        .bind(1.0f32.to_le_bytes().to_vec())
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn retract_deletes_embeddings_under_every_model() {
        let kv = store().await;
        assert(&kv, "a1", "u1", "color", "blue").await;
        let v = kv.vector_retriever(HashEmbedder::new("m", "v1", 32));
        v.reindex("u1").await.unwrap();
        insert_vec(&v.pool, "a1", "u1", "other", "r9").await;
        assert_eq!(rows_of(&v.pool, "a1").await, 2);
        kv.assertions()
            .retract(&"a1".into(), "u1", "2026-01-02T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(rows_of(&v.pool, "a1").await, 0);
    }

    #[tokio::test]
    async fn supersede_deletes_the_old_assertions_embeddings() {
        let kv = store().await;
        assert(&kv, "old", "u1", "city", "paris").await;
        let v = kv.vector_retriever(HashEmbedder::new("m", "v1", 32));
        v.reindex("u1").await.unwrap();
        insert_vec(&v.pool, "old", "u1", "other", "r9").await;
        let mut next = Assertion::new(
            "new",
            Scope::owner("u1"),
            "city",
            "is",
            json!("lyon"),
            vec!["e".into()],
        );
        next.supersedes = Some("old".into());
        kv.assertions().assert(&next).await.unwrap();
        assert_eq!(rows_of(&v.pool, "old").await, 0);
        assert_eq!(rows_of(&v.pool, "new").await, 0);
        assert_eq!(v.reindex("u1").await.unwrap(), 1);
        assert_eq!(rows_of(&v.pool, "new").await, 1);
    }

    #[tokio::test]
    async fn reassert_then_reindex_brings_the_embedding_back() {
        let kv = store().await;
        assert(&kv, "a1", "u1", "color", "blue").await;
        let v = kv.vector_retriever(HashEmbedder::new("m", "v1", 32));
        v.reindex("u1").await.unwrap();
        kv.assertions()
            .retract(&"a1".into(), "u1", "2026-01-02T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(rows_of(&v.pool, "a1").await, 0);
        // The re-assert transition: recorded_to returns to NULL. The trigger's
        // WHEN clause does not match, and the missing vector is embedded again.
        sqlx::query("UPDATE mem_assertions SET recorded_to = NULL WHERE id = 'a1'")
            .execute(&v.pool)
            .await
            .unwrap();
        assert_eq!(rows_of(&v.pool, "a1").await, 0);
        assert_eq!(v.reindex("u1").await.unwrap(), 1);
        assert_eq!(rows_of(&v.pool, "a1").await, 1);
    }

    #[tokio::test]
    async fn clearing_recorded_to_keeps_existing_embeddings() {
        // Close the fact first (the trigger deletes whatever vectors exist, here
        // none), then plant a residual row beside the closed fact. Writing
        // recorded_to back to NULL is the re-assert transition; the WHEN clause
        // does not match, so the residual row stays.
        let kv = store().await;
        let pool = crate::test_hooks::pool(&kv).clone();
        let closed = Assertion::new(
            "a1",
            Scope::owner("u1"),
            "color",
            "is",
            json!("blue"),
            vec!["e".into()],
        );
        kv.assertions().assert(&closed).await.unwrap();
        sqlx::query("UPDATE mem_assertions SET recorded_to = 't' WHERE id = 'a1'")
            .execute(&pool)
            .await
            .unwrap();
        insert_vec(&pool, "a1", "u1", "m", "r").await;
        sqlx::query("UPDATE mem_assertions SET recorded_to = NULL WHERE id = 'a1'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(rows_of(&pool, "a1").await, 1);
    }

    /// A fact the embedder always rejects must not keep the facts after it
    /// without vectors; when every fact fails, the error is returned.
    #[tokio::test]
    async fn one_fact_that_never_embeds_does_not_block_the_others() {
        #[derive(Clone)]
        struct RejectOne(HashEmbedder);
        #[async_trait]
        impl Embedder for RejectOne {
            fn model_id(&self) -> &str {
                self.0.model_id()
            }
            fn revision(&self) -> &str {
                self.0.revision()
            }
            async fn embed(&self, text: &str) -> Result<Embedding> {
                if text.contains("poison") {
                    return Err(MemoryError::Embedder("too long".into()));
                }
                self.0.embed(text).await
            }
        }
        let kv = store().await;
        assert(&kv, "a0", "u1", "note", "poison").await;
        assert(&kv, "a1", "u1", "color", "blue").await;
        assert(&kv, "a2", "u1", "city", "lyon").await;
        let v = kv.vector_retriever(RejectOne(HashEmbedder::new("m", "v1", 32)));
        assert_eq!(v.reindex("u1").await.unwrap(), 2);
        assert_eq!(rows_of(&v.pool, "a1").await, 1);
        assert_eq!(rows_of(&v.pool, "a2").await, 1);
        assert_eq!(rows_of(&v.pool, "a0").await, 0);

        let only_bad = store().await;
        assert(&only_bad, "b0", "u1", "note", "poison").await;
        let v = only_bad.vector_retriever(RejectOne(HashEmbedder::new("m", "v1", 32)));
        assert!(v.reindex("u1").await.is_err());
    }

    #[tokio::test]
    async fn reindex_inserts_nothing_when_retract_lands_during_embed() {
        #[derive(Clone)]
        struct RetractOnEmbed {
            kv: KvStore,
            inner: HashEmbedder,
        }
        #[async_trait]
        impl Embedder for RetractOnEmbed {
            fn model_id(&self) -> &str {
                self.inner.model_id()
            }
            fn revision(&self) -> &str {
                self.inner.revision()
            }
            async fn embed(&self, text: &str) -> Result<Embedding> {
                self.kv
                    .assertions()
                    .retract(&"a1".into(), "u1", "2026-01-02T00:00:00Z")
                    .await?;
                self.inner.embed(text).await
            }
        }
        let kv = store().await;
        assert(&kv, "a1", "u1", "color", "blue").await;
        let v = kv.vector_retriever(RetractOnEmbed {
            kv: kv.clone(),
            inner: HashEmbedder::new("m", "v1", 32),
        });
        assert_eq!(v.reindex("u1").await.unwrap(), 0);
        assert_eq!(rows_of(&v.pool, "a1").await, 0);
    }

    #[tokio::test]
    async fn reindex_embeds_the_object_as_plain_text() {
        #[derive(Clone)]
        struct Capture {
            seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
            inner: HashEmbedder,
        }
        #[async_trait]
        impl Embedder for Capture {
            fn model_id(&self) -> &str {
                self.inner.model_id()
            }
            fn revision(&self) -> &str {
                self.inner.revision()
            }
            async fn embed(&self, text: &str) -> Result<Embedding> {
                self.seen.lock().unwrap().push(text.to_owned());
                self.inner.embed(text).await
            }
        }
        let kv = store().await;
        assert(&kv, "a1", "u1", "color", "blue").await;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let v = kv.vector_retriever(Capture {
            seen: seen.clone(),
            inner: HashEmbedder::new("m", "v1", 32),
        });
        v.reindex("u1").await.unwrap();
        let got = seen.lock().unwrap().clone();
        assert_eq!(got, vec!["color is blue".to_owned()]);
        assert!(!got[0].contains('"'));
    }

    #[tokio::test]
    async fn bare_revision_rows_are_reembedded_under_the_text_format_key() {
        let kv = store().await;
        assert(&kv, "a1", "u1", "color", "blue").await;
        let pool = crate::test_hooks::pool(&kv).clone();
        insert_vec(&pool, "a1", "u1", "m", "v1").await;
        let v = kv.vector_retriever(HashEmbedder::new("m", "v1", 32));
        assert_eq!(v.reindex("u1").await.unwrap(), 1);
        assert_eq!(rows_for(&v.pool, "u1", "v1").await, 0);
        assert_eq!(rows_for(&v.pool, "u1", &index_revision("v1")).await, 1);
    }

    #[tokio::test]
    async fn incomplete_reindex_keeps_older_revision_rows() {
        #[derive(Clone)]
        struct AddDuring {
            kv: KvStore,
            inner: HashEmbedder,
        }
        #[async_trait]
        impl Embedder for AddDuring {
            fn model_id(&self) -> &str {
                self.inner.model_id()
            }
            fn revision(&self) -> &str {
                self.inner.revision()
            }
            async fn embed(&self, text: &str) -> Result<Embedding> {
                assert(&self.kv, "a2", "u1", "city", "zanzibar").await;
                self.inner.embed(text).await
            }
        }
        let kv = store().await;
        assert(&kv, "a1", "u1", "color", "blue").await;
        let v1 = kv.vector_retriever(HashEmbedder::new("m", "v1", 32));
        v1.reindex("u1").await.unwrap();
        let v2 = kv.vector_retriever(AddDuring {
            kv: kv.clone(),
            inner: HashEmbedder::new("m", "v2", 32),
        });
        assert_eq!(v2.reindex("u1").await.unwrap(), 1);
        assert_eq!(rows_for(&v2.pool, "u1", &index_revision("v1")).await, 1);
        assert_eq!(rows_for(&v2.pool, "u1", &index_revision("v2")).await, 1);
    }

    #[tokio::test]
    async fn delete_owner_embeddings_removes_every_model_for_that_owner() {
        let kv = store().await;
        assert(&kv, "a", "alice", "color", "blue").await;
        assert(&kv, "b", "bob", "color", "red").await;
        let v = kv.vector_retriever(HashEmbedder::new("m", "v1", 32));
        v.reindex("alice").await.unwrap();
        v.reindex("bob").await.unwrap();
        insert_vec(&v.pool, "a", "alice", "other", "r9").await;
        let n = kv.delete_owner_embeddings("alice").await.unwrap();
        assert!(n >= 1);
        assert_eq!(rows_of(&v.pool, "a").await, 0);
        assert_eq!(rows_of(&v.pool, "b").await, 1);
    }

    #[tokio::test]
    async fn migration_0021_clears_closed_residuals_and_keeps_live_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        let pool = pool_migrated_before(&path, 21).await;
        for (id, recorded_to) in [("closed", Some("t")), ("live", None)] {
            sqlx::query(
                "INSERT INTO mem_assertions
                     (id, scope_owner, scope, subject, predicate, object,
                      valid_from, recorded_from, recorded_to, evidence,
                      confidence, modality, writer_version, qualified)
                 VALUES (?, 'u', '{\"owner\":\"u\"}', 's', 'p', '\"text\"',
                         't0', 't0', ?, '[]', 1.0, 'said', 't', 1)",
            )
            .bind(id)
            .bind(recorded_to)
            .execute(&pool)
            .await
            .unwrap();
        }
        insert_vec(&pool, "closed", "u", "m", "r1").await;
        insert_vec(&pool, "closed", "u", "other", "r9").await;
        insert_vec(&pool, "live", "u", "m", "r1").await;
        insert_vec(&pool, "live", "u", "other", "r9").await;
        pool.close().await;

        let kv = KvStore::open(&path).await.unwrap();
        let pool = crate::test_hooks::pool(&kv);
        assert_eq!(rows_of(pool, "closed").await, 0);
        assert_eq!(rows_of(pool, "live").await, 2);
        sqlx::query("UPDATE mem_assertions SET recorded_to = 't2' WHERE id = 'live'")
            .execute(pool)
            .await
            .unwrap();
        assert_eq!(rows_of(pool, "live").await, 0);
    }

    async fn pool_migrated_before(path: &std::path::Path, stop_before: i64) -> SqlitePool {
        use std::str::FromStr;
        let options =
            sqlx::sqlite::SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
                .unwrap()
                .create_if_missing(true)
                .foreign_keys(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let mut migrator = sqlx::migrate!("./migrations");
        migrator.migrations = migrator
            .migrations
            .iter()
            .filter(|m| m.version < stop_before)
            .cloned()
            .collect::<Vec<_>>()
            .into();
        migrator.run(&pool).await.unwrap();
        pool
    }
}
