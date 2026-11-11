//! Append-only sessions. Callers must serialize each session's reads and writes.
use crate::{
    MemoryError, Result,
    event::{EventId, EventLog, MemEvent, Origin, Scope, Trust},
    session::CanonicalSession,
};
use agent24_models::Msg;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection, SqlitePool};

#[derive(Clone)]
pub struct SessionLog {
    pool: SqlitePool,
    gate: crate::disk_gate::DiskGate,
    #[cfg(test)]
    probe: Option<std::sync::Arc<regression_tests::Probe>>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct TurnIds {
    pub user: EventId,
    pub assistant: EventId,
}
/// Rows [`SessionLog::purge_session`] removed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SessionPurge {
    /// `mem_events` rows of the session (messages, summaries, import marker).
    pub events: u64,
    /// `mem_session_gaps` rows of the session.
    pub gaps: u64,
    /// Legacy `kv` blob (`session` namespace) that would be re-imported.
    pub legacy_blob: u64,
}

impl SessionPurge {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ImportOutcome {
    Imported { events: usize },
    AlreadyImported,
    NothingToImport,
}
#[derive(Default, serde::Deserialize)]
pub struct SessionView {
    pub summary: Option<String>,
    pub covered_through_seq: i64,
    #[serde(skip)]
    pub tail: Vec<(i64, Msg)>,
}

fn id(value: impl serde::Serialize) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&value)?)))
}
async fn write(
    conn: &mut SqliteConnection,
    scope: &Scope,
    id: String,
    kind: &str,
    body: Value,
    origin: Origin,
) -> Result<i64> {
    EventLog::append_tx(conn, &MemEvent::new(id, scope.clone(), kind, body, origin)).await?;
    Ok(sqlx::query_scalar("SELECT last_insert_rowid()")
        .fetch_one(conn)
        .await?)
}
fn system_origin(source: &str) -> Origin {
    Origin {
        source: source.into(),
        trust: Trust::System,
    }
}

impl SessionLog {
    /// Uses a pool connected to a database already migrated by `KvStore`. The
    /// disk gate is a REQUIRED argument so a caller cannot get an ungated log by
    /// omission; pass `DiskGate::disabled()` explicitly to opt out (tests,
    /// ephemeral pools). Production code gets a gated one from
    /// `KvStore::session_log()`.
    pub fn new(pool: SqlitePool, gate: crate::disk_gate::DiskGate) -> Self {
        Self {
            pool,
            gate,
            #[cfg(test)]
            probe: None,
        }
    }

    /// The caller's stable turn number distinguishes repeated identical user
    /// messages across turns while making retries of one turn idempotent.
    /// Allocate a new number per (owner, session) turn; retries must reuse the
    /// original number, including after a restart.
    #[allow(clippy::too_many_arguments)]
    pub async fn append_turn(
        &self,
        owner: &str,
        session: &str,
        turn_no: u64,
        user: &Msg,
        user_origin: Origin,
        assistant: &Msg,
        assistant_origin: Origin,
    ) -> Result<TurnIds> {
        self.gate.check(crate::disk_gate::WriteClass::Content)?;
        let scope = Scope::owner(owner).with_session(session);
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let ids = TurnIds {
            user: id((owner, session, turn_no, "user"))?,
            assistant: id((owner, session, turn_no, "assistant"))?,
        };
        let stored = sqlx::query(
            "SELECT id,kind,scope,payload AS body,json_object('source',origin_source,'trust',origin_trust) AS origin FROM mem_events WHERE id IN (?,?)",
        )
        .bind(&ids.user)
        .bind(&ids.assistant)
        .fetch_all(&mut *tx)
        .await?;
        #[cfg(test)]
        if let Some(probe) = &self.probe {
            probe.observe(stored.len()).await;
        }

        match stored.len() {
            0 => {
                write(
                    &mut tx,
                    &scope,
                    ids.user.clone(),
                    "message",
                    serde_json::to_value(user)?,
                    user_origin,
                )
                .await?;
                write(
                    &mut tx,
                    &scope,
                    ids.assistant.clone(),
                    "message",
                    serde_json::to_value(assistant)?,
                    assistant_origin,
                )
                .await?;
            }
            2 => {
                let expected = [
                    (&ids.user, user, &user_origin),
                    (&ids.assistant, assistant, &assistant_origin),
                ];
                for (key, message, origin) in expected {
                    let row = stored
                        .iter()
                        .find(|row| row.get::<String, _>("id") == *key)
                        .ok_or_else(|| {
                            MemoryError::Conflict(format!("session event {key} is missing"))
                        })?;
                    let row_scope: Scope = serde_json::from_str(row.get("scope"))?;
                    let row_body: Value = serde_json::from_str(row.get("body"))?;
                    let row_origin: Origin = serde_json::from_str(row.get("origin"))?;
                    if row.get::<String, _>("kind") != "message"
                        || row_scope != scope
                        || row_body != serde_json::to_value(message)?
                        || row_origin != *origin
                    {
                        return Err(MemoryError::Conflict(format!(
                            "session event {key} differs"
                        )));
                    }
                }
            }
            _ => {
                return Err(MemoryError::Conflict(format!(
                    "session turn {} has an incomplete or unexpected event pair",
                    turn_no
                )));
            }
        }
        tx.commit().await?;
        Ok(ids)
    }

    pub async fn import_legacy(
        &self,
        owner: &str,
        legacy: &CanonicalSession,
    ) -> Result<ImportOutcome> {
        self.gate.check(crate::disk_gate::WriteClass::Content)?;
        let scope = Scope::owner(owner).with_session(&legacy.session_id);
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM mem_events WHERE scope_owner=? AND scope_session=?)",
        )
        .bind(owner)
        .bind(&legacy.session_id)
        .fetch_one(&mut *tx)
        .await?;
        #[cfg(test)]
        if let Some(probe) = &self.probe {
            probe.observe(1).await;
        }
        if exists {
            return Ok(ImportOutcome::AlreadyImported);
        }
        if legacy.summary.is_none() && legacy.recent.is_empty() {
            return Ok(ImportOutcome::NothingToImport);
        }
        let mut pending = Vec::new();
        if let Some(summary) = &legacy.summary {
            pending.push((
                "session.summary",
                json!({"summary":summary,"covered_through_seq":0}),
                Trust::System,
            ));
        }
        for msg in &legacy.recent {
            let trust = match msg.role.as_str() {
                "user" => Trust::UserSaid,
                "assistant" => Trust::Model,
                "tool" => Trust::ToolOutput,
                _ => Trust::System,
            };
            pending.push(("message", serde_json::to_value(msg)?, trust));
        }
        pending.push((
            "session.imported",
            json!({"from":"kv","messages":legacy.recent.len(),"had_summary":legacy.summary.is_some()}),
            Trust::System,
        ));
        for (index, (kind, body, trust)) in pending.iter().enumerate() {
            let key = id(("import", &scope, index))?;
            let origin = Origin {
                trust: *trust,
                ..system_origin("migration")
            };
            write(&mut tx, &scope, key, kind, body.clone(), origin).await?;
        }
        tx.commit().await?;
        Ok(ImportOutcome::Imported {
            events: pending.len(),
        })
    }

    pub async fn append_summary(
        &self,
        owner: &str,
        session: &str,
        summary: &str,
        covered_through_seq: i64,
    ) -> Result<i64> {
        self.gate.check(crate::disk_gate::WriteClass::Content)?;
        let scope = Scope::owner(owner).with_session(session);
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let nonce: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq),0) FROM mem_events")
            .fetch_one(&mut *tx)
            .await?;
        #[cfg(test)]
        if let Some(probe) = &self.probe {
            probe.observe(1).await;
        }
        let body = json!({"summary":summary,"covered_through_seq":covered_through_seq});
        let key = id(("summary", &scope, nonce))?;
        let seq = write(
            &mut tx,
            &scope,
            key,
            "session.summary",
            body,
            system_origin("session_log"),
        )
        .await?;
        tx.commit().await?;
        Ok(seq)
    }

    /// Delete one whole session of one owner: its events, its write-gap rows
    /// and the legacy blob (without that, the next read would import the old
    /// text back). One transaction; deleting nothing is a success.
    ///
    /// This is the sanctioned exception to the append-only rule, used for
    /// retention (inbound text expiry). The quota triggers on `mem_events`
    /// keep the owner usage counters right.
    ///
    /// It does NOT touch `mem_assertions`. Retracting would be wrong and
    /// would not delete the text anyway: an assertion id is a hash of owner
    /// and text, so a later, legitimate "remember" of the same sentence by the
    /// user shares the row, and a retracted row keeps its `object`. The
    /// guarantee that an inbound run never writes an assertion is made at the
    /// source (X11-2c: inbound runs are not allowed through
    /// `retain::persist`), and this purge DEPENDS on it.
    ///
    /// `mem_session_gaps` is keyed by session id only (no owner column), so
    /// its delete below is by session id alone. Inbound sessions are fresh
    /// per message, so a collision with another owner's session id is not
    /// expected.
    pub async fn purge_session(&self, owner: &str, session: &str) -> Result<SessionPurge> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let events = sqlx::query("DELETE FROM mem_events WHERE scope_owner=? AND scope_session=?")
            .bind(owner)
            .bind(session)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let gaps = sqlx::query("DELETE FROM mem_session_gaps WHERE session_id=?")
            .bind(session)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let legacy_blob = sqlx::query("DELETE FROM kv WHERE namespace='session' AND key=?")
            .bind(session)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok(SessionPurge {
            events,
            gaps,
            legacy_blob,
        })
    }

    pub async fn load_view(&self, owner: &str, session: &str) -> Result<SessionView> {
        let scope = Scope::owner(owner).with_session(session);
        let mut tx = self.pool.begin().await?;
        let summary = sqlx::query(
            "SELECT payload AS body FROM mem_events WHERE scope_owner=? AND scope_session=? AND kind='session.summary' ORDER BY seq DESC LIMIT 1",
        )
        .bind(owner)
        .bind(session)
        .fetch_optional(&mut *tx)
        .await?;
        #[cfg(test)]
        if let Some(probe) = &self.probe {
            probe.observe(usize::from(summary.is_some())).await;
        }
        let mut view = match summary {
            Some(row) => serde_json::from_str(row.get("body"))?,
            None => SessionView::default(),
        };
        let tail = sqlx::query(
            "SELECT seq,payload AS body FROM mem_events WHERE scope_owner=? AND scope_session=? AND kind='message' AND seq>? ORDER BY seq",
        )
        .bind(&scope.owner)
        .bind(session)
        .bind(view.covered_through_seq)
        .fetch_all(&mut *tx)
        .await?;
        #[cfg(test)]
        if let Some(probe) = &self.probe {
            probe.observe(tail.len()).await;
        }
        for row in tail {
            view.tail
                .push((row.get("seq"), serde_json::from_str(row.get("body"))?));
        }
        tx.commit().await?;
        Ok(view)
    }
}

#[cfg(test)]
#[path = "session_log_regression_tests.rs"]
mod regression_tests;
