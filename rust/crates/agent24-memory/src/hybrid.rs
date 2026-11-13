//! VR-3: hybrid recall — FTS top 20 plus vector top 20, merged by reciprocal
//! rank fusion (RRF), top `k` out (VECTOR-RECALL-PLAN §3.3).
//!
//! Rules this file keeps:
//! - **The ledger is the only authority.** The vector route joins
//!   `mem_embeddings` back to `mem_assertions` and returns the ledger row
//!   itself, so classification and `source_ref` are exactly what FTS would
//!   have returned for the same fact. Nothing is read from the vector table
//!   except the vector. The join keeps the owner scope, `recorded_to IS NULL`
//!   and `qualified = 1`.
//! - **Fallback is pure FTS, byte for byte.** When the vector route cannot or
//!   should not run (no vectors, breaker open, embedder
//!   error or timeout, nothing above [`MIN_COSINE`]) the result is
//!   `search_any(query, owner, k)` — the same call recall made before VR-3.
//! - **No query embedding unless the index can help.** An index with no vector
//!   for this owner under the current `(model_id, revision)` skips the embedder
//!   entirely. A partial index is used: un-embedded facts still arrive via FTS.
//! - **Known window (accepted):** the FTS read and the vector read are two
//!   queries, not one snapshot; a fact closed between them is caught by the
//!   recovery revalidation, not here.
//! - **A hung embedder costs one timeout, not one per turn.** A timeout opens
//!   a breaker; until it expires the vector route is skipped. The breaker state
//!   lives in this value, which `SessionMemory` holds: VR-4 must build exactly
//!   one `SessionMemory` (hence one retriever) per owner.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::{Row, SqlitePool};
use tokio::time::Instant;

use crate::Result;
use crate::assertion::AssertionLedger;
use crate::retriever::{FtsRetriever, SearchHit, is_searchable_query};
use crate::vector::{Embedder, blob_to_vec, cosine, index_revision, validate_embedding};

/// Candidates taken from each route before fusion.
pub const ROUTE_TOP_N: usize = 20;
/// RRF constant: `score = Σ 1 / (RRF_K + rank)`, rank starting at 1.
pub const RRF_K: f32 = 60.0;
/// Vector hits whose cosine is below this are dropped before fusion. Initial
/// value from the VR-0 measurements (paraphrases >= 0.86, unrelated <= 0.53);
/// VR-5 calibrates it on the eval set.
pub const MIN_COSINE: f32 = 0.62;
/// The query is cut to this many characters before it is embedded.
pub const MAX_QUERY_CHARS: usize = 2000;
/// Budget for embedding the query.
pub const QUERY_EMBED_TIMEOUT: Duration = Duration::from_millis(1500);
/// After a query-embedding timeout, the vector route is skipped for this long.
pub const BREAKER_COOLDOWN: Duration = Duration::from_secs(30);

/// FTS + vector retrieval with RRF. Cheap to share behind an `Arc`; it holds
/// the breaker state, so build one per owner/session, not one per query.
pub struct HybridRetriever {
    pool: SqlitePool,
    embedder: Arc<dyn Embedder>,
    fts: FtsRetriever,
    query_timeout: Duration,
    breaker_cooldown: Duration,
    breaker_until: Mutex<Option<Instant>>,
}

impl HybridRetriever {
    pub(crate) fn new(pool: SqlitePool, embedder: Arc<dyn Embedder>) -> Self {
        let fts = FtsRetriever::new(pool.clone());
        Self {
            pool,
            embedder,
            fts,
            query_timeout: QUERY_EMBED_TIMEOUT,
            breaker_cooldown: BREAKER_COOLDOWN,
            breaker_until: Mutex::new(None),
        }
    }

    /// Override the query-embedding timeout and the breaker cooldown (tests;
    /// the defaults are the plan's 1.5 s and 30 s).
    #[must_use]
    pub fn with_timing(mut self, query_timeout: Duration, breaker_cooldown: Duration) -> Self {
        self.query_timeout = query_timeout;
        self.breaker_cooldown = breaker_cooldown;
        self
    }

    fn breaker_open(&self) -> bool {
        let guard = self
            .breaker_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.is_some_and(|until| Instant::now() < until)
    }

    fn trip_breaker(&self) {
        let mut guard = self
            .breaker_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(Instant::now() + self.breaker_cooldown);
    }

    /// The top `k` current, qualified beliefs of `owner` for `query`.
    pub async fn search(&self, query: &str, owner: &str, k: usize) -> Result<Vec<SearchHit>> {
        if !is_searchable_query(query) || k == 0 {
            return Ok(Vec::new());
        }
        if self.breaker_open() {
            return self.fts.search_any(query, owner, k).await;
        }
        // A DB error here (e.g. BUSY) must not fail the whole recall: warn once
        // and use keyword search (plan §3.8).
        let usable = match self.index_usable(owner).await {
            Ok(usable) => usable,
            Err(err) => {
                tracing::warn!(error = %err, "vector index check failed; using keyword search");
                false
            }
        };
        if !usable {
            return self.fts.search_any(query, owner, k).await;
        }
        let vector_hits = match self.vector_route(query, owner).await {
            Ok(hits) => hits,
            Err(err) => {
                tracing::warn!(error = %err, "vector recall unavailable; using keyword search");
                Vec::new()
            }
        };
        if vector_hits.is_empty() {
            // Same call as the no-embedder path, so the output is identical.
            return self.fts.search_any(query, owner, k).await;
        }
        let fts_hits = self.fts.search_any(query, owner, ROUTE_TOP_N).await?;
        Ok(rrf_merge(&fts_hits, &vector_hits, k))
    }

    /// True when the owner has at least one vector under the current key.
    /// Completeness is NOT required (plan §3.3): facts not embedded yet are
    /// still found by the FTS side of the RRF merge. No vectors at all means
    /// the query is never embedded and the result is pure FTS.
    async fn index_usable(&self, owner: &str) -> Result<bool> {
        let revision = index_revision(self.embedder.revision());
        let has_any: i64 = sqlx::query_scalar(
            "SELECT EXISTS(
                 SELECT 1 FROM mem_embeddings e
                 JOIN mem_assertions a ON a.id = e.assertion_id
                 WHERE a.scope_owner = ? AND e.model_id = ? AND e.revision = ?
                   AND a.recorded_to IS NULL AND a.qualified = 1
             )",
        )
        .bind(owner)
        .bind(self.embedder.model_id())
        .bind(&revision)
        .fetch_one(&self.pool)
        .await?;
        Ok(has_any != 0)
    }

    /// Embed the query (bounded by the timeout), then rank the owner's
    /// current-key vectors. The rows are read AFTER the embedding returns, so a
    /// fact retracted while the query was being embedded is not returned.
    pub(crate) async fn vector_route(&self, query: &str, owner: &str) -> Result<Vec<SearchHit>> {
        // A pasted wall of text must not time out and trip the breaker: embed
        // only the first MAX_QUERY_CHARS characters (FTS still sees all of it).
        let embed_text: String = query.chars().take(MAX_QUERY_CHARS).collect();
        let embedded =
            tokio::time::timeout(self.query_timeout, self.embedder.embed(&embed_text)).await;
        let q = match embedded {
            Ok(result) => result?,
            Err(_elapsed) => {
                self.trip_breaker();
                return Err(crate::MemoryError::Embedder(format!(
                    "query embedding timed out after {:?}",
                    self.query_timeout
                )));
            }
        };
        validate_embedding(&*self.embedder, &q)?;

        let revision = index_revision(self.embedder.revision());
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
        .fetch_all(&self.pool)
        .await?;

        let mut scored: Vec<SearchHit> = Vec::new();
        for r in &rows {
            let score = cosine(&q.vector, &blob_to_vec(&r.get::<Vec<u8>, _>("vec")));
            if !score.is_finite() || score < MIN_COSINE {
                continue;
            }
            scored.push(SearchHit {
                score,
                assertion: AssertionLedger::row_to_assertion(r)?,
            });
        }
        scored.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.assertion.id.cmp(&b.assertion.id))
        });
        scored.truncate(ROUTE_TOP_N);
        Ok(scored)
    }
}

/// Reciprocal rank fusion of two ranked lists; same-score ties by id ascending.
fn rrf_merge(fts: &[SearchHit], vector: &[SearchHit], k: usize) -> Vec<SearchHit> {
    let mut merged: HashMap<&str, (f32, &SearchHit)> = HashMap::new();
    for list in [fts, vector] {
        for (i, hit) in list.iter().enumerate() {
            let contribution = 1.0 / (RRF_K + (i + 1) as f32);
            merged
                .entry(hit.assertion.id.as_str())
                .and_modify(|(score, _)| *score += contribution)
                .or_insert((contribution, hit));
        }
    }
    let mut out: Vec<SearchHit> = merged
        .into_values()
        .map(|(score, hit)| SearchHit {
            score,
            assertion: hit.assertion.clone(),
        })
        .collect();
    out.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.assertion.id.cmp(&b.assertion.id))
    });
    out.truncate(k);
    out
}

/// Test embedder: maps
/// a text to the vector of the first table entry whose key it contains, else a
/// fixed "unrelated" vector. Counts calls and can fail or hang on queries.
#[cfg(test)]
pub(crate) mod test_embedder {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::MemoryError;
    use crate::vector::Embedding;

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(crate) enum QueryMode {
        Ok,
        Fail,
        Hang,
    }

    pub(crate) struct TableEmbedder {
        pub(crate) table: Vec<(&'static str, Vec<f32>)>,
        pub(crate) query_mode: QueryMode,
        /// Texts starting with this are "queries"; others are facts (reindex).
        pub(crate) query_marker: &'static str,
        pub(crate) query_calls: AtomicUsize,
        pub(crate) last_query_chars: AtomicUsize,
    }

    impl TableEmbedder {
        pub(crate) fn new(table: Vec<(&'static str, Vec<f32>)>, query_mode: QueryMode) -> Self {
            Self {
                table,
                query_mode,
                query_marker: "Q:",
                query_calls: AtomicUsize::new(0),
                last_query_chars: AtomicUsize::new(0),
            }
        }
        pub(crate) fn calls(&self) -> usize {
            self.query_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Embedder for TableEmbedder {
        fn model_id(&self) -> &str {
            "table"
        }
        fn revision(&self) -> &str {
            "v1"
        }
        async fn embed(&self, text: &str) -> Result<Embedding> {
            if text.starts_with(self.query_marker) {
                self.query_calls.fetch_add(1, Ordering::SeqCst);
                self.last_query_chars
                    .store(text.chars().count(), Ordering::SeqCst);
                match self.query_mode {
                    QueryMode::Ok => {}
                    QueryMode::Fail => return Err(MemoryError::Embedder("down".into())),
                    QueryMode::Hang => std::future::pending::<()>().await,
                }
            }
            let vector = self
                .table
                .iter()
                .find(|(key, _)| text.contains(key))
                .map_or_else(|| vec![0.0, 0.0, 1.0], |(_, v)| v.clone());
            Ok(Embedding {
                model_id: "table".into(),
                revision: "v1".into(),
                dims: 3,
                normalized: false,
                vector,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::time::Instant as StdInstant;

    use serde_json::json;

    use super::test_embedder::{QueryMode, TableEmbedder};
    use super::*;
    use crate::KvStore;
    use crate::assertion::{Assertion, AssertionStore};
    use crate::event::{Origin, Scope, Trust};
    use crate::vector::{Embedding, vec_to_blob};
    use crate::writer::{Candidate, MemoryWriter};

    const PEANUT: &str = "I am allergic to peanuts";
    const CAT: &str = "my cat is named Tom";
    /// Shares no token with [`PEANUT`], so FTS finds nothing; the table
    /// embedder puts both on the same axis.
    const PARAPHRASE: &str = "Q:which snacks could hurt me";

    fn table() -> Vec<(&'static str, Vec<f32>)> {
        vec![
            ("peanut", vec![1.0, 0.0, 0.0]),
            ("snacks", vec![1.0, 0.0, 0.0]),
            ("cat", vec![0.0, 1.0, 0.0]),
            ("weather", vec![0.5, 0.0, 0.866]),
        ]
    }

    async fn put(kv: &KvStore, id: &str, owner: &str, object: &str) {
        let a = Assertion::new(
            id,
            Scope::owner(owner),
            "user",
            "said_to_remember",
            json!(object),
            vec!["e".into()],
        );
        kv.assertions().assert(&a).await.unwrap();
    }

    async fn setup(mode: QueryMode) -> (KvStore, Arc<TableEmbedder>, HybridRetriever) {
        let kv = KvStore::open_memory().await.unwrap();
        let emb = Arc::new(TableEmbedder::new(table(), mode));
        let hybrid = kv
            .hybrid_retriever(emb.clone())
            .with_timing(Duration::from_millis(300), Duration::from_secs(30));
        (kv, emb, hybrid)
    }

    async fn reindex(kv: &KvStore, emb: &Arc<TableEmbedder>, owner: &str) {
        kv.vector_retriever(emb.clone())
            .reindex(owner)
            .await
            .unwrap();
    }

    #[async_trait::async_trait]
    impl Embedder for Arc<TableEmbedder> {
        fn model_id(&self) -> &str {
            self.as_ref().model_id()
        }
        fn revision(&self) -> &str {
            self.as_ref().revision()
        }
        async fn embed(&self, text: &str) -> Result<Embedding> {
            self.as_ref().embed(text).await
        }
    }

    fn ids(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter().map(|h| h.assertion.id.as_str()).collect()
    }

    #[tokio::test]
    async fn paraphrase_with_zero_fts_hits_is_found_by_the_vector_route() {
        let (kv, emb, hybrid) = setup(QueryMode::Ok).await;
        put(&kv, "peanut", "u1", PEANUT).await;
        put(&kv, "cat", "u1", CAT).await;
        reindex(&kv, &emb, "u1").await;
        let fts = kv
            .retriever()
            .search_any(PARAPHRASE, "u1", 5)
            .await
            .unwrap();
        assert!(fts.is_empty(), "premise: FTS finds nothing: {fts:?}");
        let hits = hybrid.search(PARAPHRASE, "u1", 5).await.unwrap();
        assert_eq!(ids(&hits), ["peanut"]);
        assert_eq!(emb.calls(), 1);
    }

    #[tokio::test]
    async fn fallbacks_are_identical_to_pure_fts() {
        // A query FTS can answer, so "identical" is not "both empty".
        let query = "Q:peanuts";
        // error
        let (kv, emb, hybrid) = setup(QueryMode::Fail).await;
        put(&kv, "peanut", "u1", PEANUT).await;
        reindex(&kv, &emb, "u1").await;
        let fts = kv.retriever().search_any(query, "u1", 5).await.unwrap();
        assert!(!fts.is_empty());
        assert_eq!(hybrid.search(query, "u1", 5).await.unwrap(), fts);
        assert_eq!(emb.calls(), 1, "the embedder was tried");
        // timeout
        let (kv, emb, hybrid) = setup(QueryMode::Hang).await;
        let hybrid = hybrid.with_timing(Duration::from_millis(50), Duration::from_secs(30));
        put(&kv, "peanut", "u1", PEANUT).await;
        reindex(&kv, &emb, "u1").await;
        let fts = kv.retriever().search_any(query, "u1", 5).await.unwrap();
        assert_eq!(hybrid.search(query, "u1", 5).await.unwrap(), fts);
        assert_eq!(emb.calls(), 1);
        // empty index: no vectors, and the query is never embedded
        let (kv, emb, hybrid) = setup(QueryMode::Ok).await;
        put(&kv, "peanut", "u1", PEANUT).await;
        let fts = kv.retriever().search_any(query, "u1", 5).await.unwrap();
        assert_eq!(hybrid.search(query, "u1", 5).await.unwrap(), fts);
        assert_eq!(emb.calls(), 0, "empty index must not embed the query");
    }

    #[tokio::test]
    async fn a_partial_index_still_uses_vectors_and_keeps_unembedded_facts_via_fts() {
        let (kv, emb, hybrid) = setup(QueryMode::Ok).await;
        put(&kv, "peanut", "u1", PEANUT).await;
        reindex(&kv, &emb, "u1").await;
        // Fresh remember, not embedded yet; shares the token "cats" with the query.
        put(&kv, "fresh", "u1", "I walk my cats daily").await;
        let query = "Q:snacks cats";
        let fts = kv.retriever().search_any(query, "u1", 5).await.unwrap();
        assert_eq!(ids(&fts), ["fresh"], "premise: FTS finds only the new fact");
        let hits = hybrid.search(query, "u1", 5).await.unwrap();
        assert_eq!(emb.calls(), 1, "the vector route ran on a partial index");
        let got = ids(&hits);
        assert!(got.contains(&"peanut"), "vector hit missing: {got:?}");
        assert!(got.contains(&"fresh"), "un-embedded fact lost: {got:?}");
    }

    #[tokio::test]
    async fn a_low_cosine_vector_hit_is_dropped() {
        let (kv, emb, hybrid) = setup(QueryMode::Ok).await;
        put(&kv, "peanut", "u1", PEANUT).await;
        reindex(&kv, &emb, "u1").await;
        // cosine 0.5 with the stored vector: below MIN_COSINE.
        let hits = hybrid.search("Q:weather", "u1", 5).await.unwrap();
        assert!(hits.is_empty(), "no-answer must inject nothing: {hits:?}");
        assert_eq!(emb.calls(), 1, "the vector route did run");
    }

    #[tokio::test]
    async fn a_closed_fact_with_a_residual_vector_is_not_returned() {
        let (kv, emb, hybrid) = setup(QueryMode::Ok).await;
        put(&kv, "peanut", "u1", PEANUT).await;
        put(&kv, "cat", "u1", CAT).await;
        reindex(&kv, &emb, "u1").await;
        kv.assertions()
            .retract(&"peanut".to_owned(), "u1", "2026-01-02T00:00:00Z")
            .await
            .unwrap();
        // The VR-2 trigger removed its vectors; plant a residual row the way a
        // racing writer or an old database could leave one.
        let pool = crate::test_hooks::pool(&kv).clone();
        let stored = emb.embed("peanut").await.unwrap();
        sqlx::query(
            "INSERT INTO mem_embeddings
               (assertion_id, scope_owner, model_id, revision, dims, normalized, vec, at)
             VALUES ('peanut', 'u1', 'table', ?, 3, 0, ?, 't')",
        )
        .bind(index_revision("v1"))
        .bind(vec_to_blob(&stored.vector))
        .execute(&pool)
        .await
        .unwrap();
        let hits = hybrid.search(PARAPHRASE, "u1", 5).await.unwrap();
        assert!(hits.is_empty(), "retracted fact leaked: {:?}", ids(&hits));
    }

    #[tokio::test]
    async fn an_unqualified_fact_with_a_vector_is_not_returned() {
        let (kv, emb, hybrid) = setup(QueryMode::Ok).await;
        put(&kv, "cat", "u1", CAT).await;
        put(&kv, "peanut", "u1", PEANUT).await;
        reindex(&kv, &emb, "u1").await;
        sqlx::query("UPDATE mem_assertions SET qualified = 0 WHERE id = 'peanut'")
            .execute(crate::test_hooks::pool(&kv))
            .await
            .unwrap();
        let hits = hybrid.search(PARAPHRASE, "u1", 5).await.unwrap();
        assert!(hits.is_empty(), "{:?}", ids(&hits));
    }

    #[tokio::test]
    async fn another_owners_fact_is_never_returned() {
        let (kv, emb, hybrid) = setup(QueryMode::Ok).await;
        put(&kv, "mine", "u1", CAT).await;
        put(&kv, "theirs", "u2", PEANUT).await;
        reindex(&kv, &emb, "u1").await;
        reindex(&kv, &emb, "u2").await;
        let hits = hybrid.search(PARAPHRASE, "u1", 5).await.unwrap();
        assert!(hits.is_empty(), "cross-owner leak: {:?}", ids(&hits));
        let hits = hybrid.search(PARAPHRASE, "u2", 5).await.unwrap();
        assert_eq!(ids(&hits), ["theirs"]);
    }

    #[tokio::test]
    async fn a_hung_embedder_is_paid_for_once_not_every_turn() {
        let (kv, emb, hybrid) = setup(QueryMode::Hang).await;
        let hybrid = hybrid.with_timing(Duration::from_millis(500), Duration::from_secs(30));
        put(&kv, "peanut", "u1", PEANUT).await;
        reindex(&kv, &emb, "u1").await;
        let t0 = StdInstant::now();
        hybrid.search("Q:peanuts", "u1", 5).await.unwrap();
        let first = t0.elapsed();
        assert!(first >= Duration::from_millis(450), "{first:?}");
        let t1 = StdInstant::now();
        let second = hybrid.search("Q:peanuts", "u1", 5).await.unwrap();
        assert!(
            t1.elapsed() < Duration::from_millis(450),
            "{:?}",
            t1.elapsed()
        );
        // Primary assertion is the call count; the time bound above is only a
        // generous backstop (a second embed would cost the full 500 ms).
        assert_eq!(emb.calls(), 1, "the second turn must skip the vector route");
        let fts = kv
            .retriever()
            .search_any("Q:peanuts", "u1", 5)
            .await
            .unwrap();
        assert_eq!(second, fts);
    }

    #[tokio::test]
    async fn an_index_check_db_error_falls_back_to_fts_instead_of_failing() {
        let (kv, emb, hybrid) = setup(QueryMode::Ok).await;
        put(&kv, "peanut", "u1", PEANUT).await;
        reindex(&kv, &emb, "u1").await;
        // Make the index check fail with a DB error.
        sqlx::query("DROP TABLE mem_embeddings")
            .execute(crate::test_hooks::pool(&kv))
            .await
            .unwrap();
        let query = "Q:peanuts";
        let fts = kv.retriever().search_any(query, "u1", 5).await.unwrap();
        assert!(!fts.is_empty());
        assert_eq!(hybrid.search(query, "u1", 5).await.unwrap(), fts);
        assert_eq!(emb.calls(), 0);
    }

    #[tokio::test]
    async fn a_very_long_query_is_cut_before_it_is_embedded() {
        let (kv, emb, hybrid) = setup(QueryMode::Ok).await;
        put(&kv, "peanut", "u1", PEANUT).await;
        reindex(&kv, &emb, "u1").await;
        let long = format!("Q:snacks {}", "x".repeat(MAX_QUERY_CHARS * 3));
        hybrid.search(&long, "u1", 5).await.unwrap();
        assert_eq!(emb.calls(), 1);
        assert_eq!(
            emb.last_query_chars
                .load(std::sync::atomic::Ordering::SeqCst),
            MAX_QUERY_CHARS
        );
    }

    #[tokio::test]
    async fn the_breaker_closes_after_its_cooldown() {
        let (kv, emb, hybrid) = setup(QueryMode::Hang).await;
        let hybrid = hybrid.with_timing(Duration::from_millis(30), Duration::from_millis(80));
        put(&kv, "peanut", "u1", PEANUT).await;
        reindex(&kv, &emb, "u1").await;
        hybrid.search("Q:peanuts", "u1", 5).await.unwrap();
        tokio::time::sleep(Duration::from_millis(120)).await;
        hybrid.search("Q:peanuts", "u1", 5).await.unwrap();
        assert_eq!(emb.calls(), 2);
    }

    #[test]
    fn rrf_puts_a_fact_found_by_both_routes_first() {
        let hit = |id: &str| SearchHit {
            score: 0.0,
            assertion: Assertion::new(id, Scope::owner("u"), "s", "p", json!(id), vec![]),
        };
        let fts = [hit("a"), hit("both")];
        let vector = [hit("b"), hit("both")];
        let merged = rrf_merge(&fts, &vector, 3);
        assert_eq!(ids(&merged), ["both", "a", "b"]);
        assert_eq!(rrf_merge(&fts, &vector, 1).len(), 1);
    }

    /// Carried from the VR-2 review: the reassert goes through the real write
    /// path (`WriteGate::propose`, writer.rs "Review H3" branch), not a
    /// hand-written UPDATE.
    #[tokio::test]
    async fn reassert_through_the_writer_brings_the_vector_route_back() {
        let (kv, emb, hybrid) = setup(QueryMode::Ok).await;
        let candidate = || {
            Candidate::new(
                "peanut",
                Scope::owner("u1"),
                "user",
                "said_to_remember",
                json!(PEANUT),
                Origin {
                    source: "test".to_owned(),
                    trust: Trust::UserSaid,
                },
            )
            .with_evidence(vec!["ev".to_owned()])
            .remember()
        };
        put(&kv, "cat", "u1", CAT).await;
        kv.write_gate().propose(vec![candidate()]).await.unwrap();
        reindex(&kv, &emb, "u1").await;
        assert_eq!(
            ids(&hybrid.search(PARAPHRASE, "u1", 5).await.unwrap()),
            ["peanut"]
        );

        kv.assertions()
            .retract(&"peanut".to_owned(), "u1", "2026-01-02T00:00:00Z")
            .await
            .unwrap();
        assert!(hybrid.search(PARAPHRASE, "u1", 5).await.unwrap().is_empty());

        kv.write_gate().propose(vec![candidate()]).await.unwrap();
        let closed: Option<String> =
            sqlx::query_scalar("SELECT recorded_to FROM mem_assertions WHERE id = 'peanut'")
                .fetch_one(crate::test_hooks::pool(&kv))
                .await
                .unwrap();
        assert!(closed.is_none(), "the writer reasserted the fact");
        assert_eq!(
            kv.vector_retriever(emb.clone())
                .reindex("u1")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            ids(&hybrid.search(PARAPHRASE, "u1", 5).await.unwrap()),
            ["peanut"]
        );
    }
}
