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
struct Entry {
    seq: i64,
    id: String,
    kind: String,
    scope: Scope,
    body: Value,
    origin: Origin,
}
async fn entries(conn: &mut SqliteConnection, scope: &Scope) -> Result<Vec<Entry>> {
    sqlx::query("SELECT seq,id,kind,scope,payload AS body,json_object('source',origin_source,'trust',origin_trust) AS origin FROM mem_events WHERE scope_owner=? AND scope_session=? ORDER BY seq")
        .bind(&scope.owner).bind(&scope.session).fetch_all(conn).await?
        .into_iter().map(|row| Ok(Entry {
            seq: row.get("seq"), id: row.get("id"), kind: row.get("kind"),
            scope: serde_json::from_str(row.get("scope"))?,
            body: serde_json::from_str(row.get("body"))?,
            origin: serde_json::from_str(row.get("origin"))?,
        })).collect()
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
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
    /// With no caller turn token, the same latest user means retry: identical
    /// assistant/origins return the old IDs; different assistant/origins conflict.
    /// Consequently consecutive identical exchanges cannot represent new turns.
    pub async fn append_turn(
        &self,
        owner: &str,
        session: &str,
        user: &Msg,
        user_origin: Origin,
        assistant: &Msg,
        assistant_origin: Origin,
    ) -> Result<TurnIds> {
        let scope = Scope::owner(owner).with_session(session);
        let mut tx = self.pool.begin().await?;
        let stored = entries(&mut tx, &scope).await?;
        let users: Vec<_> = stored
            .iter()
            .filter(|e| e.kind == "message" && e.body["role"] == "user")
            .collect();
        let body = serde_json::to_value(user)?;
        let mut turn = users.len();
        let retry = if let Some(last) = users.last() {
            last.id == id((owner, session, turn - 1, "user"))? && last.body == body
        } else {
            false
        };
        if retry {
            turn -= 1;
        }
        let ids = TurnIds {
            user: id((owner, session, turn, "user"))?,
            assistant: id((owner, session, turn, "assistant"))?,
        };
        for (key, body, origin) in [
            (&ids.user, body, user_origin),
            (
                &ids.assistant,
                serde_json::to_value(assistant)?,
                assistant_origin,
            ),
        ] {
            if retry {
                if !stored.iter().any(|e| {
                    e.id == *key
                        && e.kind == "message"
                        && e.scope == scope
                        && e.body == body
                        && e.origin == origin
                }) {
                    return Err(MemoryError::Conflict(format!(
                        "session event {key} differs"
                    )));
                }
            } else {
                write(&mut tx, &scope, key.clone(), "message", body, origin).await?;
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
        let mut tx = self.pool.begin().await?;
        if !entries(&mut tx, &scope).await?.is_empty() {
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
        pending.push(("session.imported", json!({"from":"kv","messages":legacy.recent.len(),"had_summary":legacy.summary.is_some()}), Trust::System));
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
        let mut tx = self.pool.begin().await?;
        let nonce: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq),0) FROM mem_events")
            .fetch_one(&mut *tx)
            .await?;
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
        let stored = entries(&mut *self.pool.acquire().await?, &scope).await?;
        let mut view = SessionView::default();
        if let Some(latest) = stored.iter().rev().find(|e| e.kind == "session.summary") {
            view = serde_json::from_value(latest.body.clone())?;
        }
        for e in stored
            .into_iter()
            .filter(|e| e.kind == "message" && e.seq > view.covered_through_seq)
        {
            view.tail.push((e.seq, serde_json::from_value(e.body)?));
        }
        Ok(view)
    }
}
