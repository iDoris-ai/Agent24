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
use sqlx::{Row, SqliteConnection, SqlitePool};

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
        let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT id, scope_owner, subject, predicate, object FROM mem_assertions ORDER BY id",
        )
        .fetch_all(&mut *tx)
        .await?;
        for (id, owner, subject, predicate, object) in rows {
            Self::index_tx(&mut tx, &id, &owner, &subject, &predicate, &object).await?;
        }
        sqlx::query("DELETE FROM mem_fts_state WHERE k = 'needs_rebuild'")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn index_tx(
        conn: &mut SqliteConnection,
        id: &str,
        owner: &str,
        subject: &str,
        predicate: &str,
        object: &str,
    ) -> Result<()> {
        let cjk = cjk_bigrams(&format!("{subject} {predicate} {object}"));
        sqlx::query(
            "INSERT INTO mem_assertions_fts (id, scope_owner, subject, predicate, object, cjk)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(owner)
        .bind(subject)
        .bind(predicate)
        .bind(object)
        .bind(cjk)
        .execute(conn)
        .await?;
        Ok(())
    }

    /// Search matching any token in the query.
    pub async fn search_any(
        &self,
        query: &str,
        owner: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        self.search_mode(query, owner, limit, " OR ").await
    }
}

/// Overlapping bigrams of contiguous Han text; retain single-character runs.
pub(crate) fn cjk_bigrams(text: &str) -> String {
    text.split(|ch| !is_han(ch))
        .filter(|run| !run.is_empty())
        .flat_map(|run| {
            let chars: Vec<char> = run.chars().collect();
            if chars.len() == 1 {
                vec![chars[0].to_string()]
            } else {
                chars.windows(2).map(|pair| pair.iter().collect()).collect()
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

fn is_han(ch: char) -> bool {
    matches!(ch as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F)
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
/// Whether a query has any searchable term. The SHARED predicate both retrievers
/// use so a degenerate (empty / all-punctuation) query returns nothing rather
/// than erroring, in BOTH the FTS and vector implementations of the same trait
/// contract (review #123 B2).
pub(crate) fn is_searchable_query(query: &str) -> bool {
    to_match_query(query).is_some()
}

fn to_match_query(query: &str) -> Option<String> {
    to_match_query_with(query, " ")
}

fn to_match_query_with(query: &str, joiner: &str) -> Option<String> {
    let cjk = cjk_bigrams(query);
    let terms: Vec<String> = query
        .split(|ch: char| !ch.is_alphanumeric() || is_han(ch))
        .filter(|term| !term.is_empty())
        .chain(cjk.split_whitespace())
        .map(|term| format!("\"{term}\""))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(joiner))
    }
}

#[async_trait]
impl Retriever for FtsRetriever {
    async fn search(&self, query: &str, owner: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.search_mode(query, owner, limit, " ").await
    }
}

impl FtsRetriever {
    async fn search_mode(
        &self,
        query: &str,
        owner: &str,
        limit: usize,
        joiner: &str,
    ) -> Result<Vec<SearchHit>> {
        let Some(match_expr) = to_match_query_with(query, joiner) else {
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
    use crate::event::{Origin, Trust};
    use crate::writer::{Candidate, MemoryWriter};
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
        assert!(
            r.search("color missing", "u1", 10)
                .await
                .unwrap()
                .is_empty()
        );
        let any = r.search_any("color missing", "u1", 10).await.unwrap();
        assert_eq!(any, hits);
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
        assert!(r.search_any("mood", "u1", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unqualified_candidate_is_not_returned() {
        let (kv, r) = fixture().await;
        let l = kv.assertions();
        let mut cand = a("c1", "u1", "guess", json!("maybe blue"));
        cand.qualified = false;
        l.assert(&cand).await.unwrap();
        assert!(r.search("guess", "u1", 10).await.unwrap().is_empty());
        assert!(r.search_any("guess", "u1", 10).await.unwrap().is_empty());
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

    #[tokio::test]
    async fn cjk_search_matches_terms_and_full_question() {
        let (kv, r) = fixture().await;
        let l = kv.assertions();
        l.assert(&a("peanut", "u1", "user", json!("我对花生过敏")))
            .await
            .unwrap();
        l.assert(&a("duck", "u1", "user", json!("我喜欢北京烤鸭")))
            .await
            .unwrap();
        for q in ["花生", "过敏"] {
            assert_eq!(
                r.search(q, "u1", 10).await.unwrap()[0].assertion.id,
                "peanut"
            );
            assert_eq!(
                r.search_any(q, "u1", 10).await.unwrap()[0].assertion.id,
                "peanut"
            );
        }
        let q = "我对什么过敏？";
        assert!(r.search(q, "u1", 10).await.unwrap().is_empty());
        assert!(r.search_any(q, "other", 10).await.unwrap().is_empty());
        let hits = r.search_any(q, "u1", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].assertion.id, "peanut");
        assert_eq!(r.search_any(q, "u1", 1).await.unwrap(), hits);
    }

    #[tokio::test]
    async fn write_gate_cjk_is_searchable_and_rebuildable() {
        let (kv, r) = fixture().await;
        let candidate = Candidate::new(
            "new-cn",
            Scope::owner("u2"),
            "user",
            "said",
            json!("我对花生过敏"),
            Origin {
                source: "test".into(),
                trust: Trust::UserSaid,
            },
        )
        .with_evidence(vec!["event-1".into()])
        .remember();
        kv.write_gate().propose(vec![candidate]).await.unwrap();
        assert_eq!(
            r.search_any("花生", "u2", 10).await.unwrap()[0]
                .assertion
                .id,
            "new-cn"
        );
        let q = "我对什么过敏？";
        let hits = r.search_any(q, "u2", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        sqlx::query("DELETE FROM mem_assertions_fts")
            .execute(&r.pool)
            .await
            .unwrap();
        assert!(r.search_any(q, "u2", 10).await.unwrap().is_empty());
        r.rebuild().await.unwrap();
        assert_eq!(r.search_any(q, "u2", 10).await.unwrap(), hits);
    }

    #[tokio::test]
    async fn rebuild_failure_rolls_back_index_and_keeps_rebuild_marker() {
        let (kv, r) = fixture().await;
        let l = kv.assertions();
        l.assert(&a("a1", "u1", "alpha", json!("one")))
            .await
            .unwrap();
        l.assert(&a("a2", "u1", "beta", json!("two")))
            .await
            .unwrap();
        let before: Vec<(String, String)> =
            sqlx::query_as("SELECT id, subject FROM mem_assertions_fts ORDER BY id")
                .fetch_all(&r.pool)
                .await
                .unwrap();
        let before_search = r.search("alpha", "u1", 10).await.unwrap();
        sqlx::query("INSERT OR REPLACE INTO mem_fts_state VALUES ('needs_rebuild','1')")
            .execute(&r.pool)
            .await
            .unwrap();
        sqlx::query("CREATE TRIGGER fail_rebuild BEFORE INSERT ON mem_assertions_fts_content WHEN new.c0='a2' BEGIN SELECT RAISE(ABORT, 'injected rebuild failure'); END")
            .execute(&r.pool).await.unwrap();
        assert!(r.rebuild().await.is_err());
        let after: Vec<(String, String)> =
            sqlx::query_as("SELECT id, subject FROM mem_assertions_fts ORDER BY id")
                .fetch_all(&r.pool)
                .await
                .unwrap();
        assert_eq!(after, before);
        assert_eq!(r.search("alpha", "u1", 10).await.unwrap(), before_search);
        let marker: String =
            sqlx::query_scalar("SELECT v FROM mem_fts_state WHERE k='needs_rebuild'")
                .fetch_one(&r.pool)
                .await
                .unwrap();
        assert_eq!(marker, "1");
    }

    #[test]
    fn cjk_bigram_rules_cover_runs_single_chars_and_non_cjk() {
        assert_eq!(cjk_bigrams("中文测试 A词"), "中文 文测 测试 词");
        assert_eq!(cjk_bigrams("a中b"), "中");
        assert_eq!(cjk_bigrams("abc 123"), "");
        assert_eq!(cjk_bigrams("中文，测试"), "中文 测试");
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
