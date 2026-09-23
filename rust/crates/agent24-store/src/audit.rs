//! Hash-chained audit log (ADR-026 §6.5 #10/#11, openfang-inspired).
//!
//! Every entry's hash covers the previous entry's hash — verifying the chain
//! detects any in-place tampering of the local audit table. Full detail lives
//! here (local-only DB); externally-visible logs stay redacted.

use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction, TypeInfo, ValueRef, sqlite::SqliteRow};

use crate::{Result, Store, StoreError, WorkspaceResult, WorkspaceStoreError};

#[derive(Debug, Clone, PartialEq)]
pub struct AuditEntry {
    pub seq: i64,
    pub ts: String,
    pub actor: String,
    pub action: String,
    pub detail: Value,
    pub prev_hash: String,
    pub hash: String,
}

const GENESIS: &str = "genesis";

/// Fully-decoded tail row and AUTOINCREMENT high-water captured in one read.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct StrictAuditTail {
    tuple: Option<RawAuditTuple>,
    high_water: Option<i64>,
}

#[derive(Clone, PartialEq, Eq)]
struct RawAuditTuple {
    seq: i64,
    ts: String,
    actor: String,
    action: String,
    detail: String,
    prev_hash: String,
    hash: String,
}

/// Exact fields a future append would use; this type performs no write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProspectiveAuditTuple {
    seq: i64,
    ts: String,
    actor: String,
    action: String,
    detail: String,
    prev_hash: String,
    hash: String,
}

#[allow(dead_code)]
impl ProspectiveAuditTuple {
    pub(crate) fn seq(&self) -> i64 {
        self.seq
    }
    pub(crate) fn prev_hash(&self) -> &str {
        &self.prev_hash
    }
    pub(crate) fn hash(&self) -> &str {
        &self.hash
    }
}

#[allow(dead_code)]
fn corrupt(field: &'static str) -> WorkspaceStoreError {
    WorkspaceStoreError::CorruptRow {
        table: "audit_log",
        field,
    }
}
#[allow(dead_code)]
fn strict_text(row: &SqliteRow, field: &'static str) -> WorkspaceResult<String> {
    let raw = row.try_get_raw(field).map_err(|_| corrupt(field))?;
    if raw.is_null() || raw.type_info().name() != "TEXT" {
        return Err(corrupt(field));
    }
    row.try_get(field).map_err(|_| corrupt(field))
}
#[allow(dead_code)]
fn strict_integer(
    row: &SqliteRow,
    table: &'static str,
    field: &'static str,
) -> WorkspaceResult<i64> {
    let raw = row
        .try_get_raw(field)
        .map_err(|_| WorkspaceStoreError::CorruptRow { table, field })?;
    if raw.is_null() || raw.type_info().name() != "INTEGER" {
        return Err(WorkspaceStoreError::CorruptRow { table, field });
    }
    row.try_get(field)
        .map_err(|_| WorkspaceStoreError::CorruptRow { table, field })
}
#[allow(dead_code)]
fn strict_seq(row: &SqliteRow) -> WorkspaceResult<i64> {
    strict_integer(row, "audit_log", "seq")
}
#[allow(dead_code)]
pub(crate) async fn strict_audit_chain_tx(tx: &mut Transaction<'_, Sqlite>) -> WorkspaceResult<()> {
    let rows = sqlx::query(
        "SELECT seq, ts, actor, action, detail, prev_hash, hash FROM audit_log ORDER BY seq ASC",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    let mut previous = GENESIS.to_owned();
    for (expected, row) in (1_i64..).zip(rows) {
        let (seq, ts, actor, action, detail, prior, hash) = (
            strict_seq(&row)?,
            strict_text(&row, "ts")?,
            strict_text(&row, "actor")?,
            strict_text(&row, "action")?,
            strict_text(&row, "detail")?,
            strict_text(&row, "prev_hash")?,
            strict_text(&row, "hash")?,
        );
        if seq != expected
            || prior != previous
            || entry_hash(&prior, &ts, &actor, &action, &detail) != hash
        {
            return Err(corrupt("chain"));
        }
        previous = hash;
    }
    Ok(())
}

async fn tail_tx(tx: &mut Transaction<'_, Sqlite>) -> WorkspaceResult<StrictAuditTail> {
    strict_audit_chain_tx(tx).await?;
    let row = sqlx::query("SELECT seq, ts, actor, action, detail, prev_hash, hash FROM audit_log ORDER BY seq DESC LIMIT 1")
        .fetch_optional(&mut **tx).await.map_err(|_| WorkspaceStoreError::Database)?;
    let tuple = row
        .map(|row| {
            Ok(RawAuditTuple {
                seq: strict_seq(&row)?,
                ts: strict_text(&row, "ts")?,
                actor: strict_text(&row, "actor")?,
                action: strict_text(&row, "action")?,
                detail: strict_text(&row, "detail")?,
                prev_hash: strict_text(&row, "prev_hash")?,
                hash: strict_text(&row, "hash")?,
            })
        })
        .transpose()?;
    let rows = sqlx::query("SELECT seq FROM sqlite_sequence WHERE name='audit_log' LIMIT 2")
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
    let high_water = match rows.as_slice() {
        [] => None,
        [row] => Some(strict_integer(row, "sqlite_sequence", "seq")?),
        _ => {
            return Err(WorkspaceStoreError::CorruptRow {
                table: "sqlite_sequence",
                field: "row",
            });
        }
    };
    if tuple.as_ref().map(|tuple| tuple.seq) != high_water {
        return Err(corrupt("tail"));
    }
    Ok(StrictAuditTail { tuple, high_water })
}

pub(crate) async fn strict_audit_tail_tx(
    tx: &mut Transaction<'_, Sqlite>,
) -> WorkspaceResult<StrictAuditTail> {
    tail_tx(tx).await
}

impl StrictAuditTail {
    pub(crate) fn prospective(
        &self,
        ts: &str,
        actor: &str,
        action: &str,
        detail: &str,
    ) -> WorkspaceResult<ProspectiveAuditTuple> {
        let (seq, prev_hash) = self
            .tuple
            .as_ref()
            .map(|tuple| (tuple.seq, tuple.hash.as_str()))
            .unwrap_or((0, GENESIS));
        let seq = seq.checked_add(1).ok_or(corrupt("seq"))?;
        Ok(ProspectiveAuditTuple {
            seq,
            ts: ts.into(),
            actor: actor.into(),
            action: action.into(),
            detail: detail.into(),
            prev_hash: prev_hash.into(),
            hash: entry_hash(prev_hash, ts, actor, action, detail),
        })
    }
}

#[allow(dead_code)]
pub(crate) async fn append_prospective_audit_tx(
    tx: &mut Transaction<'_, Sqlite>,
    tuple: &ProspectiveAuditTuple,
) -> WorkspaceResult<()> {
    sqlx::query(
        "INSERT INTO audit_log (seq,ts,actor,action,detail,prev_hash,hash) VALUES (?,?,?,?,?,?,?)",
    )
    .bind(tuple.seq)
    .bind(&tuple.ts)
    .bind(&tuple.actor)
    .bind(&tuple.action)
    .bind(&tuple.detail)
    .bind(&tuple.prev_hash)
    .bind(&tuple.hash)
    .execute(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn verify_prospective_tail_tx(
    tx: &mut Transaction<'_, Sqlite>,
    tuple: &ProspectiveAuditTuple,
) -> WorkspaceResult<()> {
    let tail = strict_audit_tail_tx(tx).await?;
    let actual = tail.tuple.ok_or(corrupt("tail"))?;
    (actual.seq == tuple.seq
        && actual.ts == tuple.ts
        && actual.actor == tuple.actor
        && actual.action == tuple.action
        && actual.detail == tuple.detail
        && actual.prev_hash == tuple.prev_hash
        && actual.hash == tuple.hash)
        .then_some(())
        .ok_or(corrupt("tail"))
}

#[allow(dead_code)]
pub(crate) async fn verify_prospective_audit_tx(
    tx: &mut Transaction<'_, Sqlite>,
    tuple: &ProspectiveAuditTuple,
) -> WorkspaceResult<()> {
    strict_audit_chain_tx(tx).await?;
    let row =
        sqlx::query("SELECT seq,ts,actor,action,detail,prev_hash,hash FROM audit_log WHERE seq=?")
            .bind(tuple.seq)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| WorkspaceStoreError::Database)?
            .ok_or(corrupt("tail"))?;
    (strict_seq(&row)? == tuple.seq
        && strict_text(&row, "ts")? == tuple.ts
        && strict_text(&row, "actor")? == tuple.actor
        && strict_text(&row, "action")? == tuple.action
        && strict_text(&row, "detail")? == tuple.detail
        && strict_text(&row, "prev_hash")? == tuple.prev_hash
        && strict_text(&row, "hash")? == tuple.hash)
        .then_some(())
        .ok_or(corrupt("tail"))
}

pub(crate) fn entry_hash(
    prev_hash: &str,
    ts: &str,
    actor: &str,
    action: &str,
    detail: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(b"|");
    hasher.update(ts.as_bytes());
    hasher.update(b"|");
    hasher.update(actor.as_bytes());
    hasher.update(b"|");
    hasher.update(action.as_bytes());
    hasher.update(b"|");
    hasher.update(detail.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

impl Store {
    pub(crate) async fn append_audit_tx(
        tx: &mut Transaction<'_, Sqlite>,
        ts: &str,
        actor: &str,
        action: &str,
        detail: &Value,
    ) -> Result<AuditEntry> {
        let detail_str = serde_json::to_string(detail)?;
        let prev_hash: String = sqlx::query("SELECT hash FROM audit_log ORDER BY seq DESC LIMIT 1")
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| r.get("hash"))
            .unwrap_or_else(|| GENESIS.to_owned());
        let hash = entry_hash(&prev_hash, ts, actor, action, &detail_str);
        let result = sqlx::query(
            "INSERT INTO audit_log (ts, actor, action, detail, prev_hash, hash)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(ts)
        .bind(actor)
        .bind(action)
        .bind(&detail_str)
        .bind(&prev_hash)
        .bind(&hash)
        .execute(&mut **tx)
        .await?;
        Ok(AuditEntry {
            seq: result.last_insert_rowid(),
            ts: ts.to_owned(),
            actor: actor.to_owned(),
            action: action.to_owned(),
            detail: detail.clone(),
            prev_hash,
            hash,
        })
    }

    /// Append an audit entry, chaining onto the latest hash. Serialized via
    /// BEGIN IMMEDIATE so concurrent appends cannot fork the chain.
    pub async fn append_audit(
        &self,
        ts: &str,
        actor: &str,
        action: &str,
        detail: &Value,
    ) -> Result<AuditEntry> {
        // BEGIN IMMEDIATE: take the write lock up front so two concurrent
        // appends can never read the same prev_hash and fork the chain
        // (a plain begin() is DEFERRED and only locks at first write).
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let entry = Self::append_audit_tx(&mut tx, ts, actor, action, detail).await?;
        tx.commit().await?;
        Ok(entry)
    }

    pub async fn list_audit(&self) -> Result<Vec<AuditEntry>> {
        let rows = sqlx::query("SELECT * FROM audit_log ORDER BY seq ASC")
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| {
                Ok(AuditEntry {
                    seq: r.get("seq"),
                    ts: r.get("ts"),
                    actor: r.get("actor"),
                    action: r.get("action"),
                    detail: serde_json::from_str(&r.get::<String, _>("detail"))?,
                    prev_hash: r.get("prev_hash"),
                    hash: r.get("hash"),
                })
            })
            .collect()
    }

    /// Walk the chain from genesis; any recomputed-hash mismatch or broken
    /// prev-link means tampering.
    pub async fn verify_audit_chain(&self) -> Result<()> {
        let rows = sqlx::query("SELECT * FROM audit_log ORDER BY seq ASC")
            .fetch_all(self.pool())
            .await?;
        let mut prev = GENESIS.to_owned();
        for r in &rows {
            let seq: i64 = r.get("seq");
            let prev_hash: String = r.get("prev_hash");
            let hash: String = r.get("hash");
            if prev_hash != prev {
                return Err(StoreError::Conflict(format!(
                    "audit chain broken at seq {seq}: prev link mismatch"
                )));
            }
            let recomputed = entry_hash(
                &prev_hash,
                &r.get::<String, _>("ts"),
                &r.get::<String, _>("actor"),
                &r.get::<String, _>("action"),
                &r.get::<String, _>("detail"),
            );
            if recomputed != hash {
                return Err(StoreError::Conflict(format!(
                    "audit chain broken at seq {seq}: hash mismatch"
                )));
            }
            prev = hash;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prospective_sequence_overflow_fails_closed() {
        let tail = StrictAuditTail {
            tuple: Some(RawAuditTuple {
                seq: i64::MAX,
                ts: String::new(),
                actor: String::new(),
                action: String::new(),
                detail: String::new(),
                prev_hash: String::new(),
                hash: String::new(),
            }),
            high_water: Some(i64::MAX),
        };
        assert_eq!(tail.prospective("t", "a", "a", "{}"), Err(corrupt("seq")));
    }
}
