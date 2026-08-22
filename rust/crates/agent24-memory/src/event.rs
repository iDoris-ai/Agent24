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
    /// Exclusive upper bound on `seq`. With [`Self::newest`] this is the
    /// backward-paging cursor; on its own it pins a scan to a snapshot taken
    /// before a concurrent writer's appends.
    pub before_seq: Option<i64>,
    pub limit: Option<i64>,
    /// Return the NEWEST rows first (`seq DESC`) instead of the oldest.
    ///
    /// Added in F1 for a reason worth recording, because the forward-only shape
    /// looked sufficient and was not. A reader that wants "the most recent N"
    /// under ASC ordering has to walk the partition from the beginning — which is
    /// O(partition) for a bounded answer, and, when paged, MAY not terminate
    /// against a writer that keeps appending: a writer fast enough to keep every
    /// page full leaves the walk chasing a tail that keeps moving. (A slower one
    /// lets a short page through and the walk ends — the bound is worth having
    /// because it does not depend on relative speed, not because the failure is
    /// certain.) Paging DESC walks AWAY from new appends (the cursor only
    /// decreases), so it terminates by construction and reads O(N).
    pub newest_first: bool,
}

impl EventQuery {
    pub fn owner(owner: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            session: None,
            after_seq: None,
            before_seq: None,
            limit: None,
            newest_first: false,
        }
    }
    pub fn after(mut self, seq: i64) -> Self {
        self.after_seq = Some(seq);
        self
    }
    /// Only rows with `seq < seq`.
    pub fn before(mut self, seq: i64) -> Self {
        self.before_seq = Some(seq);
        self
    }
    /// Order by `seq DESC`. See [`EventQuery::newest_first`].
    pub fn newest(mut self) -> Self {
        self.newest_first = true;
        self
    }
    pub fn session(mut self, s: impl Into<String>) -> Self {
        self.session = Some(s.into());
        self
    }
    /// Cap the rows returned.
    ///
    /// Clamped to at least 1: SQLite reads a NEGATIVE `LIMIT` as "no limit", so
    /// `.limit(-1)` would have turned the one thing `scan` promises
    /// unconditionally — that every scan is bounded — into an unbounded read
    /// through a perfectly ordinary-looking call. A caller asking for a
    /// nonsensical count gets one row, not the table.
    pub fn limit(mut self, n: i64) -> Self {
        self.limit = Some(n.max(1));
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
    /// Record that `owner`'s named projection has folded events **up to
    /// `up_to_seq`**. Forward-only (a lower seq is a no-op), so an out-of-order or
    /// retried consumer can never REGRESS a checkpoint. This is the seq a consumer
    /// actually processed — NOT the global max — so a paginated or slow consumer
    /// never skips the events it hasn't folded yet (review #114 B1).
    ///
    /// OWNER-SCOPED: a checkpoint belongs to one owner's projection. Without the
    /// owner, two owners using the same name shared one row and one side's
    /// progress made the other skip events (review #126).
    async fn checkpoint_at(&self, name: &str, owner: &str, up_to_seq: i64) -> Result<()>;
    /// Convenience: mark `owner`'s named checkpoint as "everything of THIS
    /// OWNER'S so far is folded" (records that owner's max seq, never the global
    /// one). Returns that seq. Use `checkpoint_at` when a consumer folds only a
    /// bounded page.
    async fn checkpoint(&self, name: &str, owner: &str) -> Result<i64>;
    /// The seq `owner`'s named checkpoint last reached, if any.
    async fn checkpoint_seq(&self, name: &str, owner: &str) -> Result<Option<i64>>;
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
        if q.before_seq.is_some() {
            sql.push_str(" AND seq < ?");
        }
        sql.push_str(if q.newest_first {
            " ORDER BY seq DESC LIMIT ?"
        } else {
            " ORDER BY seq ASC LIMIT ?"
        });
        let mut query = sqlx::query(&sql).bind(&q.owner);
        if let Some(s) = &q.session {
            query = query.bind(s);
        }
        if let Some(a) = q.after_seq {
            query = query.bind(a);
        }
        if let Some(b) = q.before_seq {
            query = query.bind(b);
        }
        // `.max(1)` again at the point of binding, not only in the builder: the
        // field is public, so a struct update or a direct assignment reaches this
        // without passing through `limit()`. A negative LIMIT is unbounded in
        // SQLite, and "a scan is ALWAYS bounded" has to be true of the SQL, not of
        // the API's good manners.
        query = query.bind(q.limit.unwrap_or(DEFAULT_SCAN_LIMIT).max(1));
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(Self::row_to_stored).collect()
    }

    async fn checkpoint_at(&self, name: &str, owner: &str, up_to_seq: i64) -> Result<()> {
        // Forward-only: the guarded UPDATE only advances the checkpoint, so a
        // retried or out-of-order consumer can never regress it. Keyed by
        // (scope_owner, name) so one owner's progress cannot move another's.
        sqlx::query(
            "INSERT INTO mem_checkpoints (scope_owner, name, up_to_seq, at) VALUES (?, ?, ?, ?)
             ON CONFLICT(scope_owner, name) DO UPDATE SET
                 up_to_seq = excluded.up_to_seq, at = excluded.at
                 WHERE excluded.up_to_seq > mem_checkpoints.up_to_seq",
        )
        .bind(owner)
        .bind(name)
        .bind(up_to_seq)
        .bind(now_iso8601())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn checkpoint(&self, name: &str, owner: &str) -> Result<i64> {
        // THIS OWNER's max seq, not the table's: a global MAX would bookmark one
        // owner past another owner's events (review #126).
        let max: i64 =
            sqlx::query("SELECT COALESCE(MAX(seq), 0) AS m FROM mem_events WHERE scope_owner = ?")
                .bind(owner)
                .fetch_one(&self.pool)
                .await?
                .get("m");
        self.checkpoint_at(name, owner, max).await?;
        Ok(max)
    }

    async fn checkpoint_seq(&self, name: &str, owner: &str) -> Result<Option<i64>> {
        Ok(
            sqlx::query("SELECT up_to_seq FROM mem_checkpoints WHERE scope_owner = ? AND name = ?")
                .bind(owner)
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
        assert_eq!(log.checkpoint_seq("proj", "u1").await.unwrap(), None);
        log.append(&ev("a", "u1", None, "msg")).await.unwrap();
        let c1 = log.checkpoint("proj", "u1").await.unwrap();
        assert_eq!(log.checkpoint_seq("proj", "u1").await.unwrap(), Some(c1));
        log.append(&ev("b", "u1", None, "msg")).await.unwrap();
        let c2 = log.checkpoint("proj", "u1").await.unwrap();
        assert!(c2 > c1);
        assert_eq!(log.checkpoint_seq("proj", "u1").await.unwrap(), Some(c2));
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
        log.checkpoint_at("proj", "u1", 2).await.unwrap();
        assert_eq!(log.checkpoint_seq("proj", "u1").await.unwrap(), Some(2));
        // The next incremental scan still sees c/d/e — nothing was skipped.
        let rest = log.scan(&EventQuery::owner("u1").after(2)).await.unwrap();
        assert_eq!(rest.len(), 3);
        assert_eq!(rest[0].event.id, "c");
    }

    #[tokio::test]
    async fn checkpoint_at_is_forward_only() {
        let log = log().await;
        log.checkpoint_at("proj", "u1", 10).await.unwrap();
        log.checkpoint_at("proj", "u1", 3).await.unwrap(); // lower → no-op
        assert_eq!(log.checkpoint_seq("proj", "u1").await.unwrap(), Some(10));
        log.checkpoint_at("proj", "u1", 20).await.unwrap(); // higher → advances
        assert_eq!(log.checkpoint_seq("proj", "u1").await.unwrap(), Some(20));
    }

    #[tokio::test]
    async fn checkpoints_are_owner_scoped_same_name_does_not_collide() {
        // Review #126: mem_checkpoints had NO owner column and the API took no
        // owner, so two owners using the same checkpoint name shared ONE row —
        // one side advancing made the other's incremental scan skip events.
        let log = log().await;
        for id in ["a1", "a2", "a3"] {
            log.append(&ev(id, "alice", None, "msg")).await.unwrap();
        }
        log.append(&ev("b1", "bob", None, "msg")).await.unwrap();

        // Both use the SAME projection name.
        log.checkpoint_at("condenser", "alice", 3).await.unwrap();
        assert_eq!(
            log.checkpoint_seq("condenser", "bob").await.unwrap(),
            None,
            "alice's progress is invisible to bob"
        );
        log.checkpoint_at("condenser", "bob", 1).await.unwrap();
        assert_eq!(
            log.checkpoint_seq("condenser", "alice").await.unwrap(),
            Some(3)
        );
        assert_eq!(
            log.checkpoint_seq("condenser", "bob").await.unwrap(),
            Some(1)
        );
    }

    #[tokio::test]
    async fn checkpoint_records_the_owners_max_not_the_global_max() {
        // The second half of the same defect: checkpoint() used
        // `MAX(seq) FROM mem_events` across ALL owners, so alice's bookmark could
        // land past bob's events (or vice versa).
        let log = log().await;
        log.append(&ev("a1", "alice", None, "msg")).await.unwrap();
        // bob appends many more events, pushing the GLOBAL max far ahead.
        for id in ["b1", "b2", "b3", "b4"] {
            log.append(&ev(id, "bob", None, "msg")).await.unwrap();
        }
        let alice_cp = log.checkpoint("condenser", "alice").await.unwrap();
        let alice_max = log
            .scan(&EventQuery::owner("alice"))
            .await
            .unwrap()
            .last()
            .map(|e| e.seq)
            .unwrap();
        assert_eq!(
            alice_cp, alice_max,
            "alice's checkpoint is her own max, not the table's"
        );
        // And it must NOT have jumped past bob's events.
        let bob_max = log
            .scan(&EventQuery::owner("bob"))
            .await
            .unwrap()
            .last()
            .map(|e| e.seq)
            .unwrap();
        assert!(alice_cp < bob_max, "global max would have been {bob_max}");
    }

    #[tokio::test]
    async fn checkpoint_empty_owner_is_rejected() {
        let log = log().await;
        let err = log.checkpoint_at("proj", "  ", 1).await.unwrap_err();
        assert!(matches!(err, crate::MemoryError::Sqlx(_)), "{err}");
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

    #[tokio::test]
    async fn a_whitespace_only_owner_is_rejected_like_everywhere_else() {
        // `mem_events` shipped with `CHECK(scope_owner <> '')` while every table
        // added after it uses `trim(...)`. A "   " owner therefore passed HERE and
        // was refused everywhere else — an event nothing downstream could own.
        let kv = crate::KvStore::open_memory().await.unwrap();
        let log = kv.events();
        for bad in ["   ", "\t", "\n "] {
            let ev = MemEvent::new(
                format!("ws-{}", bad.len()),
                Scope::owner(bad),
                "note",
                serde_json::json!({}),
                Origin {
                    source: "t".to_owned(),
                    trust: Trust::UserSaid,
                },
            );
            assert!(
                log.append(&ev).await.is_err(),
                "a whitespace-only owner must be refused: {bad:?}"
            );
        }
        // And a real owner still works — the constraint must not have become
        // "reject everything".
        let ok = MemEvent::new(
            "fine",
            Scope::owner("alice"),
            "note",
            serde_json::json!({}),
            Origin {
                source: "t".to_owned(),
                trust: Trust::UserSaid,
            },
        );
        log.append(&ok).await.unwrap();
    }

    #[tokio::test]
    async fn the_rebuild_keeps_seq_and_the_indexes() {
        // The 0011 rebuild copies `seq` rather than regenerating it — renumbering
        // would make every stored projection checkpoint point at a different
        // event. And it recreates 0002's indexes under THEIR names, because a
        // table rebuild drops them.
        let kv = crate::KvStore::open_memory().await.unwrap();
        let log = kv.events();
        for id in ["a", "b", "c"] {
            let ev = MemEvent::new(
                id,
                Scope::owner("alice"),
                "note",
                serde_json::json!({}),
                Origin {
                    source: "t".to_owned(),
                    trust: Trust::UserSaid,
                },
            );
            log.append(&ev).await.unwrap();
        }
        let scanned = log.scan(&EventQuery::owner("alice")).await.unwrap();
        assert_eq!(scanned.len(), 3);
        assert!(
            scanned.windows(2).all(|w| w[0].seq < w[1].seq),
            "seq must still be a monotonic total order"
        );

        let idx: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'mem_events' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(&kv.pool)
        .await
        .unwrap();
        assert_eq!(
            idx,
            vec![
                "mem_events_owner_seq".to_owned(),
                "mem_events_session_seq".to_owned()
            ],
            "0002's indexes must survive the rebuild under their own names"
        );
    }

    #[tokio::test]
    async fn a_negative_limit_is_not_an_unbounded_scan() {
        // SQLite reads a NEGATIVE `LIMIT` as "no limit", so `.limit(-1)` would turn
        // `scan`'s one unconditional promise — every scan is bounded — into a full
        // table read through an ordinary-looking call. Clamped in the builder AND
        // at the bind, because the field is public.
        let kv = crate::KvStore::open_memory().await.unwrap();
        let log = kv.events();
        for i in 0..5 {
            log.append(&MemEvent::new(
                format!("e{i}"),
                Scope::owner("alice"),
                "chat",
                serde_json::json!({"i": i}),
                Origin {
                    source: "test".into(),
                    trust: Trust::UserSaid,
                },
            ))
            .await
            .unwrap();
        }
        assert_eq!(
            log.scan(&EventQuery::owner("alice").limit(-1))
                .await
                .unwrap()
                .len(),
            1,
            "a nonsensical count gets one row, not the table"
        );
        let mut q = EventQuery::owner("alice");
        q.limit = Some(-1); // straight past the builder
        assert_eq!(log.scan(&q).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_descending_query_reads_the_newest_and_leaves_ascending_callers_alone() {
        let kv = crate::KvStore::open_memory().await.unwrap();
        let log = kv.events();
        for i in 0..10 {
            log.append(&MemEvent::new(
                format!("e{i}"),
                Scope::owner("alice"),
                "chat",
                serde_json::json!({"i": i}),
                Origin {
                    source: "test".into(),
                    trust: Trust::UserSaid,
                },
            ))
            .await
            .unwrap();
        }
        let desc = log
            .scan(&EventQuery::owner("alice").newest().limit(3))
            .await
            .unwrap();
        assert_eq!(
            desc.iter().map(|r| r.event.id.as_str()).collect::<Vec<_>>(),
            vec!["e9", "e8", "e7"]
        );
        // `newest_first` defaults to false, so every existing incremental consumer
        // keeps its ascending, `after_seq`-driven behaviour.
        let asc = log
            .scan(&EventQuery::owner("alice").limit(3))
            .await
            .unwrap();
        assert_eq!(
            asc.iter().map(|r| r.event.id.as_str()).collect::<Vec<_>>(),
            vec!["e0", "e1", "e2"]
        );
        // `before` is exclusive and composes with the descending order.
        let before = log
            .scan(
                &EventQuery::owner("alice")
                    .newest()
                    .before(desc[2].seq)
                    .limit(2),
            )
            .await
            .unwrap();
        assert_eq!(
            before
                .iter()
                .map(|r| r.event.id.as_str())
                .collect::<Vec<_>>(),
            vec!["e6", "e5"]
        );
    }
}
