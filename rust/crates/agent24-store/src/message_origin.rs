//! Per-message origin for a new run. Orthogonal to K1 `run_source_tags`.
//! A missing row is [`MessageOriginLookup::Missing`], never a NoRecall tag.

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{PendingRunMessage, Result, Store, StoreError};

pub const MESSAGE_ORIGIN_SCHEMA_VERSION: i64 = 1;

const INSERT_MESSAGE: &str = "INSERT INTO run_messages \
    (run_id, seq, role, content, tool_calls, tool_call_id, created_at) \
    VALUES (?, (SELECT COALESCE(MAX(seq), -1) + 1 FROM run_messages WHERE run_id = ?), \
    ?, ?, ?, ?, ?)";
const INSERT_ORIGIN: &str = "INSERT INTO run_message_origins \
    (run_id, seq, schema_version, kind, instance_digest, recall_ref) \
    VALUES (?, (SELECT MAX(seq) FROM run_messages WHERE run_id = ?), ?, ?, ?, ?)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageOrigin {
    HostRecall {
        recall_ref: Option<String>,
    },
    UserPrompt,
    SessionHistory,
    Assistant,
    ToolResult,
    /// Host-generated system notice (the memory pause notice). `recall_ref` is NULL.
    HostSystem,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OriginatedRunMessage {
    pub message: PendingRunMessage,
    pub origin: MessageOrigin,
}

/// One row of the first model-call snapshot.
/// `PauseNotice` is written with a `HostSystem` origin row in the same transaction.
#[derive(Debug, Clone, PartialEq)]
pub enum SnapshotRunMessage {
    Originated(OriginatedRunMessage),
    PauseNotice(PendingRunMessage),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessageOrigin {
    pub seq: i64,
    pub schema_version: i64,
    pub origin: MessageOrigin,
    pub instance_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageOriginLookup {
    Present(StoredMessageOrigin),
    Missing,
    Unreadable { seq: i64 },
}

/// Field order is the digest encoding. `content: null` is not `""`.
#[derive(Serialize)]
struct CanonicalMessageInstance<'a> {
    schema_version: i64,
    role: &'a str,
    content: Option<&'a str>,
    tool_calls: &'a Value,
    tool_call_id: Option<&'a str>,
}

pub fn message_instance_digest(
    role: &str,
    content: Option<&str>,
    tool_calls: &Value,
    tool_call_id: Option<&str>,
) -> Result<String> {
    let bytes = serde_json::to_vec(&CanonicalMessageInstance {
        schema_version: MESSAGE_ORIGIN_SCHEMA_VERSION,
        role,
        content,
        tool_calls,
        tool_call_id,
    })?;
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(format!("sha256:{hex}"))
}

fn origin_columns(origin: &MessageOrigin) -> Result<(&'static str, Option<&str>)> {
    match origin {
        MessageOrigin::HostRecall { recall_ref } => match recall_ref.as_deref() {
            Some("") => Err(StoreError::Conflict("empty recall ref".to_owned())),
            other => Ok(("HostRecall", other)),
        },
        MessageOrigin::UserPrompt => Ok(("UserPrompt", None)),
        MessageOrigin::SessionHistory => Ok(("SessionHistory", None)),
        MessageOrigin::Assistant => Ok(("Assistant", None)),
        MessageOrigin::ToolResult => Ok(("ToolResult", None)),
        MessageOrigin::HostSystem => Ok(("HostSystem", None)),
    }
}

fn origin_from_row(kind: &str, recall_ref: Option<String>) -> Option<MessageOrigin> {
    match kind {
        "HostRecall" if recall_ref.as_deref() != Some("") => {
            Some(MessageOrigin::HostRecall { recall_ref })
        }
        "UserPrompt" if recall_ref.is_none() => Some(MessageOrigin::UserPrompt),
        "SessionHistory" if recall_ref.is_none() => Some(MessageOrigin::SessionHistory),
        "Assistant" if recall_ref.is_none() => Some(MessageOrigin::Assistant),
        "ToolResult" if recall_ref.is_none() => Some(MessageOrigin::ToolResult),
        "HostSystem" if recall_ref.is_none() => Some(MessageOrigin::HostSystem),
        _ => None,
    }
}

fn decode_origin_row(row: &SqliteRow, requested_seq: i64) -> MessageOriginLookup {
    let seq = row.try_get("seq").unwrap_or(requested_seq);
    let parsed = try_origin_parts(row);
    let Some((schema_version, kind, instance_digest, recall_ref)) = parsed else {
        return MessageOriginLookup::Unreadable { seq };
    };
    if schema_version != MESSAGE_ORIGIN_SCHEMA_VERSION {
        return MessageOriginLookup::Unreadable { seq };
    }
    let Some(origin) = origin_from_row(&kind, recall_ref) else {
        return MessageOriginLookup::Unreadable { seq };
    };
    if !stored_message_matches_digest(row, &instance_digest) {
        return MessageOriginLookup::Unreadable { seq };
    }
    MessageOriginLookup::Present(StoredMessageOrigin {
        seq,
        schema_version,
        origin,
        instance_digest,
    })
}

fn stored_message_matches_digest(row: &SqliteRow, instance_digest: &str) -> bool {
    let Ok(role) = row.try_get::<String, _>("role") else {
        return false;
    };
    let Ok(content) = row.try_get::<Option<String>, _>("content") else {
        return false;
    };
    let Ok(raw_calls) = row.try_get::<String, _>("tool_calls") else {
        return false;
    };
    let Ok(tool_calls) = serde_json::from_str::<Value>(&raw_calls) else {
        return false;
    };
    let Ok(tool_call_id) = row.try_get::<Option<String>, _>("tool_call_id") else {
        return false;
    };
    let Ok(recomputed) = message_instance_digest(
        &role,
        content.as_deref(),
        &tool_calls,
        tool_call_id.as_deref(),
    ) else {
        return false;
    };
    recomputed == instance_digest
}

fn try_origin_parts(row: &SqliteRow) -> Option<(i64, String, String, Option<String>)> {
    Some((
        row.try_get("schema_version").ok()?,
        row.try_get("kind").ok()?,
        row.try_get("instance_digest").ok()?,
        row.try_get("recall_ref").ok()?,
    ))
}

impl Store {
    /// Messages and origins commit in one `BEGIN IMMEDIATE`, or neither does.
    pub async fn append_originated_run_messages(
        &self,
        run_id: &str,
        messages: &[OriginatedRunMessage],
        now: &str,
    ) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        for row in messages {
            let (kind, recall_ref) = origin_columns(&row.origin)?;
            let tool_calls_json = serde_json::to_string(&row.message.tool_calls)?;
            let digest = message_instance_digest(
                &row.message.role,
                row.message.content.as_deref(),
                &row.message.tool_calls,
                row.message.tool_call_id.as_deref(),
            )?;
            sqlx::query(INSERT_MESSAGE)
                .bind(run_id)
                .bind(run_id)
                .bind(&row.message.role)
                .bind(row.message.content.as_deref())
                .bind(&tool_calls_json)
                .bind(row.message.tool_call_id.as_deref())
                .bind(now)
                .execute(&mut *tx)
                .await?;
            sqlx::query(INSERT_ORIGIN)
                .bind(run_id)
                .bind(run_id)
                .bind(MESSAGE_ORIGIN_SCHEMA_VERSION)
                .bind(kind)
                .bind(&digest)
                .bind(recall_ref)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// The whole snapshot commits in one `BEGIN IMMEDIATE`, or none of it does.
    /// A pause notice writes a `HostSystem` origin row. An empty slice does not start a transaction.
    pub async fn append_snapshot_run_messages(
        &self,
        run_id: &str,
        messages: &[SnapshotRunMessage],
        now: &str,
    ) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        for row in messages {
            let (message, origin) = match row {
                SnapshotRunMessage::Originated(row) => (&row.message, &row.origin),
                SnapshotRunMessage::PauseNotice(message) => (message, &MessageOrigin::HostSystem),
            };
            let tool_calls_json = serde_json::to_string(&message.tool_calls)?;
            sqlx::query(INSERT_MESSAGE)
                .bind(run_id)
                .bind(run_id)
                .bind(&message.role)
                .bind(message.content.as_deref())
                .bind(&tool_calls_json)
                .bind(message.tool_call_id.as_deref())
                .bind(now)
                .execute(&mut *tx)
                .await?;
            let (kind, recall_ref) = origin_columns(origin)?;
            let digest = message_instance_digest(
                &message.role,
                message.content.as_deref(),
                &message.tool_calls,
                message.tool_call_id.as_deref(),
            )?;
            sqlx::query(INSERT_ORIGIN)
                .bind(run_id)
                .bind(run_id)
                .bind(MESSAGE_ORIGIN_SCHEMA_VERSION)
                .bind(kind)
                .bind(&digest)
                .bind(recall_ref)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn read_run_message_origin(
        &self,
        run_id: &str,
        seq: i64,
    ) -> Result<MessageOriginLookup> {
        let row = sqlx::query(
            "SELECT o.seq, o.schema_version, o.kind, o.instance_digest, o.recall_ref, \
                    m.role, m.content, m.tool_calls, m.tool_call_id \
             FROM run_message_origins o \
             LEFT JOIN run_messages m ON m.run_id = o.run_id AND m.seq = o.seq \
             WHERE o.run_id = ? AND o.seq = ?",
        )
        .bind(run_id)
        .bind(seq)
        .fetch_optional(self.pool())
        .await?;
        Ok(match row {
            Some(row) => decode_origin_row(&row, seq),
            None => MessageOriginLookup::Missing,
        })
    }
}
