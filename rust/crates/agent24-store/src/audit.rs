//! Hash-chained audit log (ADR-026 §6.5 #10/#11, openfang-inspired).
//!
//! Every entry's hash covers the previous entry's hash — verifying the chain
//! detects any in-place tampering of the local audit table. Full detail lives
//! here (local-only DB); externally-visible logs stay redacted.

use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;

use crate::{Result, Store, StoreError};

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

fn entry_hash(prev_hash: &str, ts: &str, actor: &str, action: &str, detail: &str) -> String {
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
    /// Append an audit entry, chaining onto the latest hash. Serialized via
    /// BEGIN IMMEDIATE so concurrent appends cannot fork the chain.
    pub async fn append_audit(
        &self,
        ts: &str,
        actor: &str,
        action: &str,
        detail: &Value,
    ) -> Result<AuditEntry> {
        let detail_str = serde_json::to_string(detail)?;
        // BEGIN IMMEDIATE: take the write lock up front so two concurrent
        // appends can never read the same prev_hash and fork the chain
        // (a plain begin() is DEFERRED and only locks at first write).
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let prev_hash: String = sqlx::query("SELECT hash FROM audit_log ORDER BY seq DESC LIMIT 1")
            .fetch_optional(&mut *tx)
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
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
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
