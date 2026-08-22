//! MD-3b: the FTS retriever — a full-text search PROJECTION over the assertion
//! ledger (SPEC-MD-ME §2/§3 MD-3; "FTS 检索 + scope 隔离"). One of the
//! rebuildable projections over the authorities, not an authority itself.
//!
//! [`FtsRetriever::search`] matches a query against the SQLite FTS5 index
//! (`mem_assertions_fts`, migration 0005) and joins the hits back to
//! [`crate::assertion`] so it can enforce three things the raw index cannot:
//! - **scope isolation** — only the querying `owner`'s assertions (zero leak);
//! - **current beliefs only** — `recorded_to IS NULL` (a superseded or retracted
//!   belief is out of default recall, even though its text is still indexed);
//! - **qualified only** — `qualified = 1` (unconfirmed candidates never surface),
//!   the same governance gate [`crate::assertion::AssertionStore::beliefs_as_of`]
//!   applies.
//!
//! The index is a projection: [`FtsRetriever::rebuild`] repopulates it
//! deterministically from the ledger, so it can be dropped and rebuilt.

use async_trait::async_trait;
use sqlx::{Row, SqlitePool};

use crate::Result;
use crate::assertion::{Assertion, AssertionLedger};

/// One search result: a current, qualified assertion and its relevance score
/// (higher = better; derived from FTS5 bm25).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub assertion: Assertion,
    pub score: f32,
}

/// Full-text retrieval over the semantic authority.
#[async_trait]
pub trait Retriever: Send + Sync {
    /// The top `limit` current, qualified beliefs of `owner` matching `query`,
    /// best match first. An empty or all-punctuation query returns nothing rather
    /// than erroring.
    async fn search(&self, query: &str, owner: &str, limit: usize) -> Result<Vec<SearchHit>>;
}

/// SQLite FTS5-backed [`Retriever`] over the shared memory DB.
#[derive(Clone)]
pub struct FtsRetriever {
    pool: SqlitePool,
}

impl FtsRetriever {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Rebuild the FTS projection from the ledger: clear it and re-index every
    /// assertion. Deterministic — the same ledger yields the same index — so the
    /// projection can be dropped and rebuilt (the authority+projection contract).
    pub async fn rebuild(&self) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("DELETE FROM mem_assertions_fts")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO mem_assertions_fts (id, scope_owner, subject, predicate, object)
             SELECT id, scope_owner, subject, predicate, object FROM mem_assertions",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// Turn free text into a safe FTS5 MATCH expression: SPLIT the input on every
/// non-alphanumeric character and quote each resulting term as a literal phrase,
/// AND-ing them. Splitting (not stripping) MATCHES the `unicode61` tokenizer used
/// by the index — the tokenizer breaks `e-mail` into `e`/`mail`, so the query
/// must too, or the literal text from the ledger would never match (review #120
/// B2). Quoting each term neutralizes FTS5 operators (`"`, `*`, `:`, `(`, `AND`,
/// `NEAR`, …), so arbitrary user input can never be a syntax error or an injected
/// query. Returns `None` if there is no searchable term (empty / all-punctuation),
/// so the caller returns no hits.
fn to_match_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\""))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

#[async_trait]
impl Retriever for FtsRetriever {
    async fn search(&self, query: &str, owner: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let Some(match_expr) = to_match_query(query) else {
            return Ok(Vec::new());
        };
        // Join the FTS hit back to the ledger to enforce scope + current + qualified.
        // bm25() is lower-is-better; negate so a higher score is a better match.
        let rows = sqlx::query(
            "SELECT a.id, a.scope, a.subject, a.predicate, a.object,
                    a.valid_from, a.valid_to, a.recorded_from, a.recorded_to,
                    a.evidence, a.confidence, a.modality, a.speaker, a.writer_version,
                    a.supersedes, a.qualified,
                    -bm25(mem_assertions_fts) AS score
             FROM mem_assertions_fts f
             JOIN mem_assertions a ON a.id = f.id
             WHERE mem_assertions_fts MATCH ?
               AND a.scope_owner = ?
               AND a.recorded_to IS NULL
               AND a.qualified = 1
             ORDER BY score DESC, a.id ASC
             LIMIT ?",
        )
        .bind(&match_expr)
        .bind(owner)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|r| {
                Ok(SearchHit {
                    assertion: AssertionLedger::row_to_assertion(r)?,
                    score: r.get::<f64, _>("score") as f32,
                })
            })
            .collect()
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

    async fn fixture() -> (KvStore, FtsRetriever) {
        let kv = KvStore::open_memory().await.unwrap();
        let r = kv.retriever();
        (kv, r)
    }

    fn a(id: &str, owner: &str, subject: &str, object: serde_json::Value) -> Assertion {
        Assertion::new(
            id,
            Scope::owner(owner),
            subject,
            "is",
            object,
            vec!["e".into()],
        )
    }

    #[tokio::test]
    async fn search_finds_a_matching_assertion() {
        let (kv, r) = fixture().await;
        let l = kv.assertions();
        l.assert(&a("a1", "u1", "favorite color", json!("blue")))
            .await
            .unwrap();
        l.assert(&a("a2", "u1", "home city", json!("Paris")))
            .await
            .unwrap();
        let hits = r.search("color", "u1", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].assertion.id, "a1");
    }

    #[tokio::test]
    async fn search_is_scope_isolated_zero_cross_owner_leak() {
        let (kv, r) = fixture().await;
        let l = kv.assertions();
        l.assert(&a("s1", "alice", "secret", json!("alice-treasure")))
            .await
            .unwrap();
        l.assert(&a("s2", "bob", "secret", json!("bob-treasure")))
            .await
            .unwrap();
        // Same query term "secret" — each owner sees only their own.
        let alice = r.search("secret", "alice", 10).await.unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].assertion.object, json!("alice-treasure"));
        let bob = r.search("secret", "bob", 10).await.unwrap();
        assert_eq!(bob.len(), 1);
        assert_eq!(bob[0].assertion.object, json!("bob-treasure"));
    }

    #[tokio::test]
    async fn superseded_belief_is_not_returned() {
        let (kv, r) = fixture().await;
        let l = kv.assertions();
        let mut v1 = a("v1", "u1", "role title", json!("engineer"));
        v1.recorded_from = "2020-01-01T00:00:00Z".into();
        l.assert(&v1).await.unwrap();
        let mut v2 = a("v2", "u1", "role title", json!("manager"));
        v2.recorded_from = "2021-01-01T00:00:00Z".into();
        v2.supersedes = Some("v1".into());
        l.assert(&v2).await.unwrap();
        // "role" matches both rows' text, but only the current belief returns.
        let hits = r.search("role", "u1", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].assertion.object, json!("manager"));
    }

    #[tokio::test]
    async fn retracted_belief_is_not_returned() {
        let (kv, r) = fixture().await;
        let l = kv.assertions();
        l.assert(&a("a1", "u1", "mood", json!("happy")))
            .await
            .unwrap();
        l.retract(&"a1".to_owned(), "u1", "2030-01-01T00:00:00Z")
            .await
            .unwrap();
        assert!(r.search("mood", "u1", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unqualified_candidate_is_not_returned() {
        let (kv, r) = fixture().await;
        let l = kv.assertions();
        let mut cand = a("c1", "u1", "guess", json!("maybe blue"));
        cand.qualified = false;
        l.assert(&cand).await.unwrap();
        assert!(r.search("guess", "u1", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn empty_or_punctuation_query_returns_nothing_not_an_error() {
        let (kv, r) = fixture().await;
        kv.assertions()
            .assert(&a("a1", "u1", "x", json!("y")))
            .await
            .unwrap();
        assert!(r.search("", "u1", 10).await.unwrap().is_empty());
        assert!(r.search("   ", "u1", 10).await.unwrap().is_empty());
        // FTS5 operators as bare input must not error — they are quoted away.
        assert!(r.search("\"* AND (", "u1", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn ranking_orders_best_match_first() {
        let (kv, r) = fixture().await;
        let l = kv.assertions();
        // "apple" appears in both subject and object of a1 (denser) vs once in a2.
        l.assert(&a("a1", "u1", "apple apple", json!("apple pie")))
            .await
            .unwrap();
        l.assert(&a("a2", "u1", "fruit", json!("one apple")))
            .await
            .unwrap();
        let hits = r.search("apple", "u1", 10).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].assertion.id, "a1", "denser match ranks first");
        assert!(hits[0].score >= hits[1].score);
    }

    #[tokio::test]
    async fn rebuild_actually_repopulates_a_cleared_projection() {
        // M1: prove rebuild DOES something. Clear the projection first — if it is
        // not cleared, a no-op rebuild() would pass just as well (the review's
        // `return Ok(())` mutation). Search must go 1 → 0 (cleared) → 1 (rebuilt).
        let (kv, r) = fixture().await;
        let l = kv.assertions();
        l.assert(&a("a1", "u1", "color", json!("blue")))
            .await
            .unwrap();
        let before = r.search("color", "u1", 10).await.unwrap();
        assert_eq!(before.len(), 1);

        sqlx::query("DELETE FROM mem_assertions_fts")
            .execute(&r.pool)
            .await
            .unwrap();
        assert!(
            r.search("color", "u1", 10).await.unwrap().is_empty(),
            "projection is empty after the clear"
        );

        r.rebuild().await.unwrap();
        let after = r.search("color", "u1", 10).await.unwrap();
        assert_eq!(
            after, before,
            "rebuild reproduces the index deterministically"
        );
        assert_eq!(after.len(), 1, "rebuild is not a no-op");
    }

    #[tokio::test]
    async fn punctuated_text_is_found_by_its_literal_form() {
        // B2: the ledger holds `e-mail`, `well-being`, an apostrophe. Searching
        // the SAME literal text must find them (the sanitizer must split, not
        // strip). Before the fix these were silent zero-hits.
        let (kv, r) = fixture().await;
        let l = kv.assertions();
        l.assert(&a("a1", "u1", "e-mail", json!("user@example.com")))
            .await
            .unwrap();
        l.assert(&a("a2", "u1", "well-being", json!("don't overspend")))
            .await
            .unwrap();
        assert_eq!(r.search("e-mail", "u1", 10).await.unwrap().len(), 1);
        assert_eq!(
            r.search("user@example.com", "u1", 10).await.unwrap().len(),
            1
        );
        assert_eq!(r.search("well-being", "u1", 10).await.unwrap().len(), 1);
        assert_eq!(r.search("don't", "u1", 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn migration_backfills_assertions_that_predate_the_index() {
        // B1: assertions written before 0005 (here: to a DB, then the index
        // emptied to simulate the pre-0005 state) must be searchable after a
        // rebuild — the same recovery the migration's backfill performs on
        // upgrade. (A pure migration-path test needs a fresh file; this exercises
        // the identical INSERT...SELECT that 0005 runs.)
        let (kv, r) = fixture().await;
        kv.assertions()
            .assert(&a("old", "u1", "legacy fact", json!("kept")))
            .await
            .unwrap();
        // Simulate "index did not exist when this row was written".
        sqlx::query("DELETE FROM mem_assertions_fts")
            .execute(&r.pool)
            .await
            .unwrap();
        assert!(r.search("legacy", "u1", 10).await.unwrap().is_empty());
        r.rebuild().await.unwrap(); // == 0005's backfill statement
        assert_eq!(r.search("legacy", "u1", 10).await.unwrap().len(), 1);
    }

    #[test]
    fn to_match_query_splits_like_the_tokenizer_and_drops_empties() {
        assert_eq!(
            to_match_query("hello world"),
            Some("\"hello\" \"world\"".to_owned())
        );
        assert_eq!(to_match_query("  spaced  "), Some("\"spaced\"".to_owned()));
        // B2: split on punctuation like the unicode61 tokenizer does.
        assert_eq!(to_match_query("e-mail"), Some("\"e\" \"mail\"".to_owned()));
        assert_eq!(
            to_match_query("user@example.com"),
            Some("\"user\" \"example\" \"com\"".to_owned())
        );
        assert_eq!(to_match_query(""), None);
        assert_eq!(to_match_query("*()\""), None);
    }
}
