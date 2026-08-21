//! MD-2: the episodic authority — an append-only, immutable event log
//! (SPEC-MD-ME §1/§2, ADR-028).
//!
//! [`EventLog`] is the source of truth that the condensers (MD-1) and future
//! projections (FTS/vector/KG) are VIEWS over: it is never rewritten, `id` is
//! the client idempotency key, and `seq` is the monotonic total order used for
//! scans and projection checkpoints. Every event carries a mandatory
//! [`Scope::owner`] (governance: no unowned memory) and an [`Origin`] (trust
//! provenance the write-gate keys on in MD-4).

use agent24_core::util::now_iso8601;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row, SqlitePool};

use crate::Result;

pub type EventId = String;

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
}

impl Trust {
    fn as_str(self) -> &'static str {
        match self {
            Trust::UserSaid => "user_said",
            Trust::ToolOutput => "tool_output",
            Trust::WebFetch => "web_fetch",
            Trust::Model => "model",
            Trust::System => "system",
        }
    }
    fn parse(s: &str) -> Trust {
        match s {
            "user_said" => Trust::UserSaid,
            "tool_output" => Trust::ToolOutput,
            "web_fetch" => Trust::WebFetch,
            "system" => Trust::System,
            _ => Trust::Model,
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

/// Scan filter. `after_seq` drives incremental projection (fold only what a
/// checkpoint hasn't seen).
#[derive(Debug, Clone, Default)]
pub struct EventQuery {
    pub owner: Option<String>,
    pub session: Option<String>,
    pub after_seq: Option<i64>,
    pub limit: Option<i64>,
}

impl EventQuery {
    pub fn owner(owner: impl Into<String>) -> Self {
        Self {
            owner: Some(owner.into()),
            ..Default::default()
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
}

/// The append-only episodic authority.
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Append an event; **idempotent on `id`** (a re-append returns the stored
    /// seq and writes nothing new). Returns the event's monotonic seq.
    async fn append(&self, e: &MemEvent) -> Result<i64>;
    /// Events in seq order under a filter.
    async fn scan(&self, q: &EventQuery) -> Result<Vec<StoredEvent>>;
    /// Record a named projection checkpoint at the current max seq; returns it.
    async fn checkpoint(&self, name: &str) -> Result<i64>;
    /// The seq a named checkpoint last reached, if any.
    async fn checkpoint_seq(&self, name: &str) -> Result<Option<i64>>;
}

/// SQLite-backed event log. Shares the memory DB pool (see
/// [`crate::KvStore::events`]) so the log and KV live in one file/transaction
/// domain.
#[derive(Clone)]
pub struct EventLog {
    pool: SqlitePool,
}

impl EventLog {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
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
        // Already present — return its seq (idempotent replay).
        let seq: i64 = sqlx::query("SELECT seq FROM mem_events WHERE id = ?")
            .bind(&e.id)
            .fetch_one(&self.pool)
            .await?
            .get("seq");
        Ok(seq)
    }

    async fn scan(&self, q: &EventQuery) -> Result<Vec<StoredEvent>> {
        let mut sql = String::from(
            "SELECT seq, id, scope, kind, payload, origin_source, origin_trust, causal, at
             FROM mem_events WHERE 1=1",
        );
        if q.owner.is_some() {
            sql.push_str(" AND scope_owner = ?");
        }
        if q.session.is_some() {
            sql.push_str(" AND scope_session = ?");
        }
        if q.after_seq.is_some() {
            sql.push_str(" AND seq > ?");
        }
        sql.push_str(" ORDER BY seq ASC");
        if q.limit.is_some() {
            sql.push_str(" LIMIT ?");
        }
        let mut query = sqlx::query(&sql);
        if let Some(o) = &q.owner {
            query = query.bind(o);
        }
        if let Some(s) = &q.session {
            query = query.bind(s);
        }
        if let Some(a) = q.after_seq {
            query = query.bind(a);
        }
        if let Some(l) = q.limit {
            query = query.bind(l);
        }
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(Self::row_to_stored).collect()
    }

    async fn checkpoint(&self, name: &str) -> Result<i64> {
        let max: i64 = sqlx::query("SELECT COALESCE(MAX(seq), 0) AS m FROM mem_events")
            .fetch_one(&self.pool)
            .await?
            .get("m");
        sqlx::query(
            "INSERT INTO mem_checkpoints (name, up_to_seq, at) VALUES (?, ?, ?)
             ON CONFLICT(name) DO UPDATE SET up_to_seq = excluded.up_to_seq, at = excluded.at",
        )
        .bind(name)
        .bind(max)
        .bind(now_iso8601())
        .execute(&self.pool)
        .await?;
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
}
