//! MD-3a: the semantic authority — an immutable BI-TEMPORAL assertion ledger
//! (SPEC-MD-ME §1/§2, ADR-028; graphiti's two intervals + invalidate-never-delete).
//!
//! An [`Assertion`] is a `(subject, predicate, object)` belief carrying TWO
//! independent time intervals: **valid-time** (`valid_from`/`valid_to`, when the
//! fact holds in the world) and **recorded-time** (`recorded_from`/`recorded_to`,
//! when the system believed it). [`AssertionStore::beliefs_as_of`] answers "what
//! did we believe at recorded time R about what was true at valid time V" — the
//! four-quadrant bi-temporal query.
//!
//! Contradictions are handled by SUPERSEDING, never deleting: asserting a new
//! version with `supersedes = old_id` closes the old row's `recorded_to`, so the
//! old belief is still visible when you roll `recorded_at` back. Evidence (event
//! ids) is never dropped.
//!
//! `qualified = false` is an unconfirmed candidate: it is excluded from
//! [`beliefs_as_of`] unless [`BeliefQuery::include_unqualified`] is set — the
//! "candidates do not enter default recall" governance rule (MD-4 keys on it).

use agent24_core::util::now_iso8601;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row, SqlitePool};

use crate::Result;
use crate::event::{EventId, Scope};

pub type AssertionId = String;

/// How a belief was acquired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    /// Stated by a speaker (`user said`).
    Said,
    /// Directly observed (a tool result, a sensor).
    Observed,
    /// Inferred/derived by the model from other facts.
    Derived,
}

impl Modality {
    fn as_str(self) -> &'static str {
        match self {
            Modality::Said => "said",
            Modality::Observed => "observed",
            Modality::Derived => "derived",
        }
    }
    /// Unrecognized values map to the LEAST-authoritative `Derived` (an unknown
    /// provenance is not trusted as a direct observation).
    fn parse(s: &str) -> Modality {
        match s {
            "said" => Modality::Said,
            "observed" => Modality::Observed,
            _ => Modality::Derived,
        }
    }
}

/// One bi-temporal belief. Build with [`Assertion::new`] then adjust fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assertion {
    pub id: AssertionId,
    pub scope: Scope,
    pub subject: String,
    pub predicate: String,
    pub object: Value,
    /// Valid-time interval: when the fact holds in the world. `valid_to = None`
    /// means still valid.
    pub valid_from: String,
    pub valid_to: Option<String>,
    /// Recorded-time interval: when the system believed it. `recorded_to = None`
    /// means still believed (the current record).
    pub recorded_from: String,
    pub recorded_to: Option<String>,
    pub evidence: Vec<EventId>,
    pub confidence: f32,
    pub modality: Modality,
    pub speaker: Option<String>,
    pub writer_version: String,
    /// The assertion id this one replaces, if it is a correction.
    pub supersedes: Option<AssertionId>,
    /// `false` = unconfirmed candidate; excluded from default recall.
    pub qualified: bool,
}

impl Assertion {
    /// A new qualified `Said` belief, valid and recorded from now, both intervals
    /// open, confidence 1.0. Adjust fields directly for other cases.
    pub fn new(
        id: impl Into<String>,
        scope: Scope,
        subject: impl Into<String>,
        predicate: impl Into<String>,
        object: Value,
        evidence: Vec<EventId>,
    ) -> Self {
        let now = now_iso8601();
        Self {
            id: id.into(),
            scope,
            subject: subject.into(),
            predicate: predicate.into(),
            object,
            valid_from: now.clone(),
            valid_to: None,
            recorded_from: now,
            recorded_to: None,
            evidence,
            confidence: 1.0,
            modality: Modality::Said,
            speaker: None,
            writer_version: "md3a".to_owned(),
            supersedes: None,
            qualified: true,
        }
    }
}

/// The bi-temporal query. `valid_at`/`recorded_at` default to "now" (the current
/// belief about the currently-valid fact) when `None`.
#[derive(Debug, Clone)]
pub struct BeliefQuery {
    pub owner: String,
    pub subject: Option<String>,
    pub valid_at: Option<String>,
    pub recorded_at: Option<String>,
    /// Include `qualified = false` candidates. Default false — candidates do not
    /// enter default recall.
    pub include_unqualified: bool,
}

impl BeliefQuery {
    /// Current beliefs for `owner`: both times = now, qualified only.
    pub fn owner(owner: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            subject: None,
            valid_at: None,
            recorded_at: None,
            include_unqualified: false,
        }
    }
    pub fn subject(mut self, s: impl Into<String>) -> Self {
        self.subject = Some(s.into());
        self
    }
    pub fn valid_at(mut self, t: impl Into<String>) -> Self {
        self.valid_at = Some(t.into());
        self
    }
    pub fn recorded_at(mut self, t: impl Into<String>) -> Self {
        self.recorded_at = Some(t.into());
        self
    }
    pub fn with_unqualified(mut self) -> Self {
        self.include_unqualified = true;
        self
    }
}

/// The immutable bi-temporal semantic authority.
#[async_trait]
pub trait AssertionStore: Send + Sync {
    /// Record a belief. If `a.supersedes` is set, the superseded assertion's
    /// recorded_to is CLOSED at `a.recorded_from` in the same transaction — the
    /// old row is kept (a correction, not a delete). Returns the id.
    async fn assert(&self, a: &Assertion) -> Result<AssertionId>;
    /// Stop believing an assertion as of `at`: closes its `recorded_to` (a no-op
    /// if already closed). The row is retained.
    async fn retract(&self, id: &AssertionId, at: &str) -> Result<()>;
    /// The beliefs matching `q`'s bi-temporal point (and optional subject),
    /// qualified-only unless asked otherwise.
    async fn beliefs_as_of(&self, q: &BeliefQuery) -> Result<Vec<Assertion>>;
}

/// SQLite-backed [`AssertionStore`] over the shared memory DB.
#[derive(Clone)]
pub struct AssertionLedger {
    pool: SqlitePool,
}

impl AssertionLedger {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    fn row_to_assertion(row: &sqlx::sqlite::SqliteRow) -> Result<Assertion> {
        Ok(Assertion {
            id: row.get("id"),
            scope: serde_json::from_str(&row.get::<String, _>("scope"))?,
            subject: row.get("subject"),
            predicate: row.get("predicate"),
            object: serde_json::from_str(&row.get::<String, _>("object"))?,
            valid_from: row.get("valid_from"),
            valid_to: row.get("valid_to"),
            recorded_from: row.get("recorded_from"),
            recorded_to: row.get("recorded_to"),
            evidence: serde_json::from_str(&row.get::<String, _>("evidence"))?,
            confidence: row.get::<f64, _>("confidence") as f32,
            modality: Modality::parse(&row.get::<String, _>("modality")),
            speaker: row.get("speaker"),
            writer_version: row.get("writer_version"),
            supersedes: row.get("supersedes"),
            qualified: row.get::<i64, _>("qualified") != 0,
        })
    }
}

#[async_trait]
impl AssertionStore for AssertionLedger {
    async fn assert(&self, a: &Assertion) -> Result<AssertionId> {
        let scope_json = serde_json::to_string(&a.scope)?;
        let object = serde_json::to_string(&a.object)?;
        let evidence = serde_json::to_string(&a.evidence)?;
        // Serialize the whole correction so a superseded row cannot be left open
        // by a crash between the two writes.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;

        if let Some(old) = &a.supersedes {
            // Close the old record at the moment the new one is recorded — only if
            // still open, so a re-run does not move an already-closed boundary.
            sqlx::query(
                "UPDATE mem_assertions SET recorded_to = ?
                 WHERE id = ? AND scope_owner = ? AND recorded_to IS NULL",
            )
            .bind(&a.recorded_from)
            .bind(old)
            .bind(&a.scope.owner)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            "INSERT INTO mem_assertions
                 (id, scope_owner, scope, subject, predicate, object,
                  valid_from, valid_to, recorded_from, recorded_to,
                  evidence, confidence, modality, speaker, writer_version,
                  supersedes, qualified)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&a.id)
        .bind(&a.scope.owner)
        .bind(&scope_json)
        .bind(&a.subject)
        .bind(&a.predicate)
        .bind(&object)
        .bind(&a.valid_from)
        .bind(&a.valid_to)
        .bind(&a.recorded_from)
        .bind(&a.recorded_to)
        .bind(&evidence)
        .bind(a.confidence as f64)
        .bind(a.modality.as_str())
        .bind(&a.speaker)
        .bind(&a.writer_version)
        .bind(&a.supersedes)
        .bind(i64::from(a.qualified))
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(a.id.clone())
    }

    async fn retract(&self, id: &AssertionId, at: &str) -> Result<()> {
        sqlx::query(
            "UPDATE mem_assertions SET recorded_to = ? WHERE id = ? AND recorded_to IS NULL",
        )
        .bind(at)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn beliefs_as_of(&self, q: &BeliefQuery) -> Result<Vec<Assertion>> {
        // "now" defaults: the current belief about the currently-valid fact.
        let now = now_iso8601();
        let valid_at = q.valid_at.as_deref().unwrap_or(&now);
        let recorded_at = q.recorded_at.as_deref().unwrap_or(&now);

        // Interval containment for a half-open [from, to) at each axis: from <= t
        // AND (to IS NULL OR t < to). A NULL upper bound is an open interval.
        let mut sql = String::from(
            "SELECT id, scope, subject, predicate, object, valid_from, valid_to,
                    recorded_from, recorded_to, evidence, confidence, modality,
                    speaker, writer_version, supersedes, qualified
             FROM mem_assertions
             WHERE scope_owner = ?
               AND valid_from <= ? AND (valid_to IS NULL OR ? < valid_to)
               AND recorded_from <= ? AND (recorded_to IS NULL OR ? < recorded_to)",
        );
        if !q.include_unqualified {
            sql.push_str(" AND qualified = 1");
        }
        if q.subject.is_some() {
            sql.push_str(" AND subject = ?");
        }
        sql.push_str(" ORDER BY subject ASC, recorded_from ASC, id ASC");

        let mut query = sqlx::query(&sql)
            .bind(&q.owner)
            .bind(valid_at)
            .bind(valid_at)
            .bind(recorded_at)
            .bind(recorded_at);
        if let Some(s) = &q.subject {
            query = query.bind(s);
        }
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(Self::row_to_assertion).collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::{KvStore, MemoryError};

    async fn ledger() -> AssertionLedger {
        KvStore::open_memory().await.unwrap().assertions()
    }

    fn assertion(id: &str, owner: &str, subject: &str, object: Value) -> Assertion {
        Assertion::new(
            id,
            Scope::owner(owner),
            subject,
            "is",
            object,
            vec!["e1".into()],
        )
    }

    #[tokio::test]
    async fn assert_then_read_current_belief() {
        let l = ledger().await;
        l.assert(&assertion("a1", "u1", "sky", serde_json::json!("blue")))
            .await
            .unwrap();
        let beliefs = l.beliefs_as_of(&BeliefQuery::owner("u1")).await.unwrap();
        assert_eq!(beliefs.len(), 1);
        assert_eq!(beliefs[0].object, serde_json::json!("blue"));
        assert_eq!(beliefs[0].evidence, vec!["e1".to_owned()]);
    }

    #[tokio::test]
    async fn contradiction_is_a_new_version_not_a_delete() {
        let l = ledger().await;
        // Recorded at r1: we believe the sky is blue.
        let mut v1 = assertion("v1", "u1", "sky", serde_json::json!("blue"));
        v1.recorded_from = "2020-01-01T00:00:00Z".into();
        v1.valid_from = "2020-01-01T00:00:00Z".into();
        l.assert(&v1).await.unwrap();

        // Recorded at r2: correction — the sky is grey. Supersedes v1.
        let mut v2 = assertion("v2", "u1", "sky", serde_json::json!("grey"));
        v2.recorded_from = "2020-06-01T00:00:00Z".into();
        v2.valid_from = "2020-06-01T00:00:00Z".into();
        v2.supersedes = Some("v1".into());
        l.assert(&v2).await.unwrap();

        // CURRENT belief = grey.
        let now = l.beliefs_as_of(&BeliefQuery::owner("u1")).await.unwrap();
        assert_eq!(now.len(), 1);
        assert_eq!(now[0].object, serde_json::json!("grey"));

        // ROLL RECORDED-TIME BACK to r1: we believed blue then. v1 is NOT deleted.
        let then = l
            .beliefs_as_of(
                &BeliefQuery::owner("u1")
                    .valid_at("2020-03-01T00:00:00Z")
                    .recorded_at("2020-03-01T00:00:00Z"),
            )
            .await
            .unwrap();
        assert_eq!(then.len(), 1);
        assert_eq!(
            then[0].object,
            serde_json::json!("blue"),
            "old belief preserved"
        );
    }

    #[tokio::test]
    async fn four_quadrant_valid_x_recorded() {
        // v1 valid [t1,t2) recorded [r1, r2); v2 valid [t2,∞) recorded [r2,∞).
        let l = ledger().await;
        let (t1, t2) = ("2020-01-01T00:00:00Z", "2021-01-01T00:00:00Z");
        let (r1, r2) = ("2020-02-01T00:00:00Z", "2021-02-01T00:00:00Z");
        let mut v1 = assertion("v1", "u1", "role", serde_json::json!("ic"));
        v1.valid_from = t1.into();
        v1.valid_to = Some(t2.into());
        v1.recorded_from = r1.into();
        l.assert(&v1).await.unwrap();
        let mut v2 = assertion("v2", "u1", "role", serde_json::json!("manager"));
        v2.valid_from = t2.into();
        v2.recorded_from = r2.into();
        v2.supersedes = Some("v1".into());
        l.assert(&v2).await.unwrap();

        let ask = |v: &str, r: &str| {
            let q = BeliefQuery::owner("u1").valid_at(v).recorded_at(r);
            let l = &l;
            async move {
                l.beliefs_as_of(&q)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|a| a.object)
                    .collect::<Vec<_>>()
            }
        };
        // valid in v1's window, recorded before r2 → ic.
        assert_eq!(ask(t1, r1).await, vec![serde_json::json!("ic")]);
        // valid in v2's window, recorded after r2 → manager.
        assert_eq!(ask(t2, r2).await, vec![serde_json::json!("manager")]);
        // valid in v2's window but recorded BEFORE r2 → we didn't know yet → none.
        assert_eq!(ask(t2, r1).await, Vec::<Value>::new());
        // valid in v1's window, recorded after r2 → v1's recorded interval closed
        // at r2, and v2 isn't valid back then → none.
        assert_eq!(ask(t1, r2).await, Vec::<Value>::new());
    }

    #[tokio::test]
    async fn unqualified_candidate_is_excluded_from_default_recall() {
        let l = ledger().await;
        let mut cand = assertion("c1", "u1", "plan", serde_json::json!("maybe"));
        cand.qualified = false;
        l.assert(&cand).await.unwrap();
        // Default recall: excluded.
        assert!(
            l.beliefs_as_of(&BeliefQuery::owner("u1"))
                .await
                .unwrap()
                .is_empty()
        );
        // Explicitly asked for: included.
        let with = l
            .beliefs_as_of(&BeliefQuery::owner("u1").with_unqualified())
            .await
            .unwrap();
        assert_eq!(with.len(), 1);
    }

    #[tokio::test]
    async fn retract_closes_the_record_but_keeps_the_row() {
        let l = ledger().await;
        let mut a = assertion("a1", "u1", "sky", serde_json::json!("blue"));
        a.recorded_from = "2020-01-01T00:00:00Z".into();
        a.valid_from = "2020-01-01T00:00:00Z".into();
        l.assert(&a).await.unwrap();
        l.retract(&"a1".to_owned(), "2020-06-01T00:00:00Z")
            .await
            .unwrap();
        // Currently not believed.
        assert!(
            l.beliefs_as_of(&BeliefQuery::owner("u1"))
                .await
                .unwrap()
                .is_empty()
        );
        // But still visible as of an earlier recorded time — not deleted.
        let earlier = l
            .beliefs_as_of(
                &BeliefQuery::owner("u1")
                    .valid_at("2020-03-01T00:00:00Z")
                    .recorded_at("2020-03-01T00:00:00Z"),
            )
            .await
            .unwrap();
        assert_eq!(earlier.len(), 1);
    }

    #[tokio::test]
    async fn scope_isolation_zero_cross_owner_leak() {
        let l = ledger().await;
        l.assert(&assertion("a", "alice", "s", serde_json::json!("A")))
            .await
            .unwrap();
        l.assert(&assertion("b", "bob", "s", serde_json::json!("B")))
            .await
            .unwrap();
        let alice = l.beliefs_as_of(&BeliefQuery::owner("alice")).await.unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].object, serde_json::json!("A"));
        assert!(!alice.iter().any(|x| x.object == serde_json::json!("B")));
    }

    #[tokio::test]
    async fn subject_filter_narrows() {
        let l = ledger().await;
        l.assert(&assertion("a", "u1", "sky", serde_json::json!("blue")))
            .await
            .unwrap();
        l.assert(&assertion("b", "u1", "grass", serde_json::json!("green")))
            .await
            .unwrap();
        let sky = l
            .beliefs_as_of(&BeliefQuery::owner("u1").subject("sky"))
            .await
            .unwrap();
        assert_eq!(sky.len(), 1);
        assert_eq!(sky[0].subject, "sky");
    }

    #[tokio::test]
    async fn empty_owner_is_rejected() {
        let l = ledger().await;
        let err = l
            .assert(&assertion("a", "", "s", serde_json::json!(1)))
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::Sqlx(_)), "{err}");
    }

    #[test]
    fn modality_roundtrips_and_unknown_is_derived() {
        for m in [Modality::Said, Modality::Observed, Modality::Derived] {
            assert_eq!(Modality::parse(m.as_str()), m);
        }
        assert_eq!(Modality::parse("bogus"), Modality::Derived);
    }
}
