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
    #[cfg(test)]
    probe: Option<std::sync::Arc<regression_tests::Probe>>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct TurnIds {
    pub user: EventId,
    pub assistant: EventId,
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
    /// Uses a pool connected to a database already migrated by `KvStore`.
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
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
