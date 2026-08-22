//! MD-2: the episodic authority — an append-only, immutable event log
//! (SPEC-MD-ME §1/§2, ADR-028).
//!
//! [`EventLog`] is the source of truth that the condensers (MD-1) and future
//! projections (FTS/vector/KG) are VIEWS over: it is never rewritten, `id` is
//! the client idempotency key, and `seq` is the monotonic total order used for
//! scans and projection checkpoints. `seq` is monotonic but may be SPARSE: an
//! idempotent re-append burns an AUTOINCREMENT value before the `ON CONFLICT`
//! resolves, so gaps are normal and `MAX(seq)` is NOT a row count — only its
//! ordering is relied on. Every event carries a mandatory
//! [`Scope::owner`] (governance: no unowned memory) and an [`Origin`] (trust
//! provenance the write-gate keys on in MD-4).

use agent24_core::util::now_iso8601;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row, SqlitePool};

use crate::Result;

pub type EventId = String;

/// Hard cap on a single scan when the caller gives no `limit` — a scan must
/// never fetch an unbounded set (review #114 B3).
const DEFAULT_SCAN_LIMIT: i64 = 50_000;

/// Where a memory belongs. `owner` is MANDATORY; the rest narrow it. Used for
/// isolation and (MD-4+) capability-scoped access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
}

impl Scope {
    pub fn owner(owner: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            agent: None,
            session: None,
            run: None,
        }
    }
    pub fn with_session(mut self, s: impl Into<String>) -> Self {
        self.session = Some(s.into());
        self
    }
}

/// How much to trust an event's content. The write-gate (MD-4) keys
/// default-persist eligibility on this — a `WebFetch` claim must not silently
/// become durable memory the way a `UserSaid` one may.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    UserSaid,
    ToolOutput,
    WebFetch,
    Model,
    System,
    /// An UNRECOGNIZED on-disk trust value — the STRICTEST tier: the write-gate
    /// (MD-4) must never auto-persist it. We land here rather than silently
    /// downgrading an unknown to `Model`, which on a read→write roundtrip would
    /// launder e.g. `"untrusted_web_scrape"` into `"model"` (review #114 Low).
    Unknown,
}

impl Trust {
    fn as_str(self) -> &'static str {
        match self {
            Trust::UserSaid => "user_said",
            Trust::ToolOutput => "tool_output",
            Trust::WebFetch => "web_fetch",
            Trust::Model => "model",
            Trust::System => "system",
            Trust::Unknown => "unknown",
        }
    }
    fn parse(s: &str) -> Trust {
        match s {
            "user_said" => Trust::UserSaid,
            "tool_output" => Trust::ToolOutput,
            "web_fetch" => Trust::WebFetch,
            "model" => Trust::Model,
            "system" => Trust::System,
            _ => Trust::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub source: String,
    pub trust: Trust,
}

/// One immutable episodic event. `id` is the client-stable idempotency key (a
/// re-append with a seen id is a no-op returning the stored seq).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemEvent {
    pub id: EventId,
    pub scope: Scope,
    pub kind: String,
    pub body: Value,
    pub origin: Origin,
    #[serde(default)]
    pub causal: Vec<EventId>,
    pub at: String,
}

impl MemEvent {
    /// Build an event stamped `now`, with an empty causal set.
    pub fn new(
        id: impl Into<String>,
        scope: Scope,
        kind: impl Into<String>,
        body: Value,
        origin: Origin,
    ) -> Self {
        Self {
            id: id.into(),
            scope,
            kind: kind.into(),
            body,
            origin,
            causal: Vec::new(),
            at: now_iso8601(),
        }
    }
}

/// A stored event = a [`MemEvent`] plus its assigned monotonic sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    pub seq: i64,
    pub event: MemEvent,
}

/// Scan filter. `owner` is REQUIRED — a scan is ALWAYS owner-scoped, matching
/// the mandatory-owner write side. There is deliberately no `Default`: the
/// natural incremental-scan shape `{ after_seq: Some(cp), ..Default::default() }`
/// would otherwise silently cross every tenant (review #114 B3). An admin path
/// that genuinely needs all owners can be added explicitly when a consumer needs
/// it. `after_seq` drives incremental projection.
#[derive(Debug, Clone)]
pub struct EventQuery {
    pub owner: String,
    pub session: Option<String>,
    pub after_seq: Option<i64>,
    pub limit: Option<i64>,
}

impl EventQuery {
    pub fn owner(owner: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            session: None,
            after_seq: None,
            limit: None,
        }
    }
    pub fn after(mut self, seq: i64) -> Self {
        self.after_seq = Some(seq);
        self
    }
    pub fn session(mut self, s: impl Into<String>) -> Self {
        self.session = Some(s.into());
        self
    }
    pub fn limit(mut self, n: i64) -> Self {
        self.limit = Some(n);
        self
    }
}

/// The append-only episodic authority.
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Append an event; **idempotent on `id`** (a re-append returns the stored
    /// seq and writes nothing new). Returns the event's monotonic seq.
    async fn append(&self, e: &MemEvent) -> Result<i64>;
    /// Events in seq order under a filter.
    async fn scan(&self, q: &EventQuery) -> Result<Vec<StoredEvent>>;
    /// Record that a named projection has folded events **up to `up_to_seq`**.
    /// Forward-only (a lower seq is a no-op), so an out-of-order or retried
    /// consumer can never REGRESS a checkpoint. This is the seq a consumer
    /// actually processed — NOT the global max — so a paginated or slow consumer
    /// never skips the events it hasn't folded yet (review #114 B1).
    async fn checkpoint_at(&self, name: &str, up_to_seq: i64) -> Result<()>;
    /// Convenience: mark a named checkpoint as "everything so far is folded"
    /// (records the current global max seq). Returns that seq. Use `checkpoint_at`
    /// when a consumer folds only a bounded page.
    async fn checkpoint(&self, name: &str) -> Result<i64>;
    /// The seq a named checkpoint last reached, if any.
    async fn checkpoint_seq(&self, name: &str) -> Result<Option<i64>>;
}

/// SQLite-backed event log. Shares the memory DB pool (see
/// [`crate::KvStore::events`]) so the log and KV live in the same DB file — but
/// NOT (yet) the same transaction (that cross-store seam is MD-2b).
#[derive(Clone)]
pub struct EventLog {
    pool: SqlitePool,
}

impl EventLog {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Append an event on a caller-owned connection with a PLAIN insert — an `id`
    /// collision is an error that rolls the caller's transaction back, NOT a
    /// silent no-op. `pub(crate)` so MD-4's write-gate can write an audit event in
    /// the SAME transaction as the assertion it audits (review #121 B1): if the
    /// audit cannot be written, the belief must not be either. The write-gate
    /// mints a CONTENT-ADDRESSED audit id, so a collision means either an exact
    /// replay (the assertion collides first) or a pre-occupied id — both correctly
    /// abort rather than commit a belief the audit does not describe.
    pub(crate) async fn append_tx(
        conn: &mut sqlx::sqlite::SqliteConnection,
        e: &MemEvent,
    ) -> Result<()> {
        let scope_json = serde_json::to_string(&e.scope)?;
        let payload = serde_json::to_string(&e.body)?;
        let causal = serde_json::to_string(&e.causal)?;
        sqlx::query(
            "INSERT INTO mem_events
                 (id, scope_owner, scope_session, scope, kind, payload,
                  origin_source, origin_trust, causal, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&e.id)
        .bind(&e.scope.owner)
        .bind(&e.scope.session)
        .bind(&scope_json)
        .bind(&e.kind)
        .bind(&payload)
        .bind(&e.origin.source)
        .bind(e.origin.trust.as_str())
        .bind(&causal)
        .bind(&e.at)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    fn row_to_stored(row: &sqlx::sqlite::SqliteRow) -> Result<StoredEvent> {
        let scope: Scope = serde_json::from_str(&row.get::<String, _>("scope"))?;
        let body: Value = serde_json::from_str(&row.get::<String, _>("payload"))?;
        let causal: Vec<EventId> = serde_json::from_str(&row.get::<String, _>("causal"))?;
        Ok(StoredEvent {
            seq: row.get("seq"),
            event: MemEvent {
                id: row.get("id"),
                scope,
                kind: row.get("kind"),
                body,
                origin: Origin {
                    source: row.get("origin_source"),
                    trust: Trust::parse(&row.get::<String, _>("origin_trust")),
                },
                causal,
                at: row.get("at"),
            },
        })
    }
}

#[async_trait]
impl EventStore for EventLog {
    async fn append(&self, e: &MemEvent) -> Result<i64> {
        let scope_json = serde_json::to_string(&e.scope)?;
        let payload = serde_json::to_string(&e.body)?;
        let causal = serde_json::to_string(&e.causal)?;
        // Idempotent: a seen id inserts nothing; RETURNING yields the new seq
        // only on a real insert.
        let inserted = sqlx::query(
            "INSERT INTO mem_events
                 (id, scope_owner, scope_session, scope, kind, payload,
                  origin_source, origin_trust, causal, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO NOTHING
             RETURNING seq",
        )
        .bind(&e.id)
        .bind(&e.scope.owner)
        .bind(&e.scope.session)
        .bind(&scope_json)
        .bind(&e.kind)
        .bind(&payload)
        .bind(&e.origin.source)
        .bind(e.origin.trust.as_str())
        .bind(&causal)
        .bind(&e.at)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(row) = inserted {
            return Ok(row.get("seq"));
        }
        // The id already exists. This is an idempotent replay ONLY if the stored
        // event MATCHES (same owner AND same payload). A cross-tenant id collision
        // (two sessions each minting "msg-1") or a same-owner payload change must
        // NOT be silently swallowed and handed the other event's seq — that would
        // fold one tenant's write into another's row on a layer whose whole point
        // is scope isolation (review #114 B2). `id` is a client-stable key that is
        // GLOBAL-unique in this table, so a collision needs no malice.
        let existing = sqlx::query("SELECT seq, scope_owner, payload FROM mem_events WHERE id = ?")
            .bind(&e.id)
            .fetch_one(&self.pool)
            .await?;
        let stored_owner: String = existing.get("scope_owner");
        let stored_payload: String = existing.get("payload");
        if stored_owner != e.scope.owner || stored_payload != payload {
            return Err(crate::MemoryError::Conflict(format!(
                "event id {} already exists with a different owner/payload — refusing to alias it",
                e.id
            )));
        }
        Ok(existing.get("seq"))
    }

    async fn scan(&self, q: &EventQuery) -> Result<Vec<StoredEvent>> {
        // owner is ALWAYS bound (a scan is always owner-scoped); an explicit or
        // default LIMIT is ALWAYS applied so a scan cannot fetch an unbounded set.
        let mut sql = String::from(
            "SELECT seq, id, scope, kind, payload, origin_source, origin_trust, causal, at
             FROM mem_events WHERE scope_owner = ?",
        );
        if q.session.is_some() {
            sql.push_str(" AND scope_session = ?");
        }
        if q.after_seq.is_some() {
            sql.push_str(" AND seq > ?");
        }
        sql.push_str(" ORDER BY seq ASC LIMIT ?");
        let mut query = sqlx::query(&sql).bind(&q.owner);
        if let Some(s) = &q.session {
            query = query.bind(s);
        }
        if let Some(a) = q.after_seq {
            query = query.bind(a);
        }
        query = query.bind(q.limit.unwrap_or(DEFAULT_SCAN_LIMIT));
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(Self::row_to_stored).collect()
    }

    async fn checkpoint_at(&self, name: &str, up_to_seq: i64) -> Result<()> {
        // Forward-only: the guarded UPDATE only advances the checkpoint, so a
        // retried or out-of-order consumer can never regress it.
        sqlx::query(
            "INSERT INTO mem_checkpoints (name, up_to_seq, at) VALUES (?, ?, ?)
             ON CONFLICT(name) DO UPDATE SET up_to_seq = excluded.up_to_seq, at = excluded.at
                 WHERE excluded.up_to_seq > mem_checkpoints.up_to_seq",
        )
        .bind(name)
        .bind(up_to_seq)
        .bind(now_iso8601())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn checkpoint(&self, name: &str) -> Result<i64> {
        let max: i64 = sqlx::query("SELECT COALESCE(MAX(seq), 0) AS m FROM mem_events")
            .fetch_one(&self.pool)
            .await?
            .get("m");
        self.checkpoint_at(name, max).await?;
        Ok(max)
    }

    async fn checkpoint_seq(&self, name: &str) -> Result<Option<i64>> {
        Ok(
            sqlx::query("SELECT up_to_seq FROM mem_checkpoints WHERE name = ?")
                .bind(name)
                .fetch_optional(&self.pool)
                .await?
                .map(|r| r.get::<i64, _>("up_to_seq")),
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::KvStore;

    async fn log() -> EventLog {
        KvStore::open_memory().await.unwrap().events()
    }

    fn ev(id: &str, owner: &str, session: Option<&str>, kind: &str) -> MemEvent {
        let mut scope = Scope::owner(owner);
        scope.session = session.map(str::to_owned);
        MemEvent::new(
            id,
            scope,
            kind,
            serde_json::json!({"k": id}),
            Origin {
                source: "test".to_owned(),
                trust: Trust::UserSaid,
            },
        )
    }

    #[tokio::test]
    async fn append_and_scan_in_seq_order() {
        let log = log().await;
        let s1 = log.append(&ev("a", "u1", None, "msg")).await.unwrap();
        let s2 = log.append(&ev("b", "u1", None, "msg")).await.unwrap();
        assert!(s2 > s1);
        let all = log.scan(&EventQuery::owner("u1")).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].event.id, "a");
        assert_eq!(all[1].event.id, "b");
        assert_eq!(all[0].seq, s1);
    }

    #[tokio::test]
    async fn append_is_idempotent_on_id() {
        let log = log().await;
        let first = log.append(&ev("dup", "u1", None, "msg")).await.unwrap();
        let again = log.append(&ev("dup", "u1", None, "msg")).await.unwrap();
        assert_eq!(first, again, "same id returns the same seq");
        assert_eq!(log.scan(&EventQuery::owner("u1")).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn scope_isolation_by_owner_and_session() {
        let log = log().await;
        log.append(&ev("a", "u1", Some("s1"), "msg")).await.unwrap();
        log.append(&ev("b", "u1", Some("s2"), "msg")).await.unwrap();
        log.append(&ev("c", "u2", Some("s1"), "msg")).await.unwrap();
        // owner filter isolates users
        assert_eq!(log.scan(&EventQuery::owner("u1")).await.unwrap().len(), 2);
        assert_eq!(log.scan(&EventQuery::owner("u2")).await.unwrap().len(), 1);
        // session filter narrows within an owner
        let s1 = log
            .scan(&EventQuery::owner("u1").session("s1"))
            .await
            .unwrap();
        assert_eq!(s1.len(), 1);
        assert_eq!(s1[0].event.id, "a");
    }

    #[tokio::test]
    async fn after_seq_is_incremental() {
        let log = log().await;
        let s1 = log.append(&ev("a", "u1", None, "msg")).await.unwrap();
        log.append(&ev("b", "u1", None, "msg")).await.unwrap();
        let rest = log.scan(&EventQuery::owner("u1").after(s1)).await.unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].event.id, "b");
    }

    #[tokio::test]
    async fn checkpoint_records_and_advances() {
        let log = log().await;
        assert_eq!(log.checkpoint_seq("proj").await.unwrap(), None);
        log.append(&ev("a", "u1", None, "msg")).await.unwrap();
        let c1 = log.checkpoint("proj").await.unwrap();
        assert_eq!(log.checkpoint_seq("proj").await.unwrap(), Some(c1));
        log.append(&ev("b", "u1", None, "msg")).await.unwrap();
        let c2 = log.checkpoint("proj").await.unwrap();
        assert!(c2 > c1);
        assert_eq!(log.checkpoint_seq("proj").await.unwrap(), Some(c2));
    }

    #[tokio::test]
    async fn body_origin_causal_roundtrip() {
        let log = log().await;
        let mut e = ev("x", "u1", Some("s"), "note");
        e.body = serde_json::json!({"nested": {"n": 42}, "list": [1, 2]});
        e.causal = vec!["p1".to_owned(), "p2".to_owned()];
        e.origin.trust = Trust::WebFetch;
        log.append(&e).await.unwrap();
        let got = &log.scan(&EventQuery::owner("u1")).await.unwrap()[0].event;
        assert_eq!(got.body, e.body);
        assert_eq!(got.causal, e.causal);
        assert_eq!(got.origin.trust, Trust::WebFetch);
        assert_eq!(got.scope.session.as_deref(), Some("s"));
    }

    // ---- review #114 fixes ----

    #[tokio::test]
    async fn checkpoint_at_records_what_was_folded_not_global_max() {
        // B1: a consumer folds a bounded PAGE (seq 1..2) while more events exist.
        // A global-MAX checkpoint would skip the unfolded ones forever.
        let log = log().await;
        for id in ["a", "b", "c", "d", "e"] {
            log.append(&ev(id, "u1", None, "msg")).await.unwrap();
        }
        // Fold only up to seq 2.
        log.checkpoint_at("proj", 2).await.unwrap();
        assert_eq!(log.checkpoint_seq("proj").await.unwrap(), Some(2));
        // The next incremental scan still sees c/d/e — nothing was skipped.
        let rest = log.scan(&EventQuery::owner("u1").after(2)).await.unwrap();
        assert_eq!(rest.len(), 3);
        assert_eq!(rest[0].event.id, "c");
    }

    #[tokio::test]
    async fn checkpoint_at_is_forward_only() {
        let log = log().await;
        log.checkpoint_at("proj", 10).await.unwrap();
        log.checkpoint_at("proj", 3).await.unwrap(); // lower → no-op
        assert_eq!(log.checkpoint_seq("proj").await.unwrap(), Some(10));
        log.checkpoint_at("proj", 20).await.unwrap(); // higher → advances
        assert_eq!(log.checkpoint_seq("proj").await.unwrap(), Some(20));
    }

    #[tokio::test]
    async fn append_id_collision_across_owners_is_a_conflict_not_a_swallow() {
        // B2: alice writes "evt-1"; bob writing the SAME id must NOT be swallowed
        // into alice's row and handed her seq.
        let log = log().await;
        log.append(&ev("evt-1", "alice", None, "note"))
            .await
            .unwrap();
        let err = log
            .append(&ev("evt-1", "bob", None, "payment"))
            .await
            .unwrap_err();
        assert!(matches!(err, crate::MemoryError::Conflict(_)), "{err}");
        // bob's write did not land; alice's row is untouched.
        assert_eq!(log.scan(&EventQuery::owner("bob")).await.unwrap().len(), 0);
        assert_eq!(
            log.scan(&EventQuery::owner("alice")).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn append_same_id_same_owner_different_payload_is_a_conflict() {
        let log = log().await;
        let mut a = ev("evt-1", "u1", None, "note");
        a.body = serde_json::json!({"v": 1});
        log.append(&a).await.unwrap();
        let mut b = ev("evt-1", "u1", None, "note");
        b.body = serde_json::json!({"v": 2}); // same id+owner, different payload
        assert!(matches!(
            log.append(&b).await.unwrap_err(),
            crate::MemoryError::Conflict(_)
        ));
    }

    #[tokio::test]
    async fn append_true_replay_same_event_returns_same_seq() {
        let log = log().await;
        let e = ev("evt-1", "u1", None, "note");
        let s1 = log.append(&e).await.unwrap();
        let s2 = log.append(&e).await.unwrap(); // identical → idempotent
        assert_eq!(s1, s2);
        assert_eq!(log.scan(&EventQuery::owner("u1")).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn empty_owner_is_rejected() {
        // CHECK(scope_owner <> '') — "" is unowned memory, not a valid owner.
        let log = log().await;
        let err = log.append(&ev("x", "", None, "note")).await.unwrap_err();
        assert!(matches!(err, crate::MemoryError::Sqlx(_)), "{err}");
    }

    #[test]
    fn unknown_trust_maps_to_strictest_not_model() {
        // An unrecognized on-disk trust must not launder into "model".
        assert_eq!(Trust::parse("untrusted_web_scrape"), Trust::Unknown);
        assert_eq!(Trust::Unknown.as_str(), "unknown");
        for t in [
            Trust::UserSaid,
            Trust::ToolOutput,
            Trust::WebFetch,
            Trust::Model,
            Trust::System,
            Trust::Unknown,
        ] {
            assert_eq!(Trust::parse(t.as_str()), t, "roundtrip {t:?}");
        }
    }
}
