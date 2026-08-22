//! MD-7: the knowledge/instruction layer (L4) — hierarchical CLAUDE.md-style
//! instructions merged by precedence, trigger-injected, with a REVIEW-GATED
//! auto-memory inbox (SPEC-MD-ME §3 MD-7; gemini-cli's layered instructions +
//! "never auto-apply" inbox).
//!
//! - [`KnowledgeBase::merged`] concatenates the ACTIVE instructions in precedence
//!   order (priority ascending, so the highest-priority / most-specific layer
//!   comes LAST and wins for a top-down reader — CLAUDE.md semantics).
//! - [`KnowledgeBase::triggered`] returns the active instructions whose triggers
//!   appear in a context string (conditional injection).
//! - [`KnowledgeBase::propose`] files an auto-memory PROPOSAL as `pending`; it is
//!   NEVER part of `merged`/`triggered` until [`KnowledgeBase::approve`] promotes
//!   it. Auto-memory is never auto-applied — a human gates it.
//!
//! All writes are owner-scoped (a foreign id cannot approve/reject another
//! owner's inbox — the #119 lesson).

use async_trait::async_trait;
use sqlx::{Row, SqlitePool};

use crate::Result;

/// Whether an instruction is in force (`Active`) or an un-approved proposal
/// waiting in the inbox (`Pending`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionStatus {
    Active,
    Pending,
}

impl InstructionStatus {
    fn as_str(self) -> &'static str {
        match self {
            InstructionStatus::Active => "active",
            InstructionStatus::Pending => "pending",
        }
    }
    fn parse(s: &str) -> InstructionStatus {
        match s {
            "active" => InstructionStatus::Active,
            _ => InstructionStatus::Pending,
        }
    }
}

/// One layered instruction (a CLAUDE.md-style block).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub id: String,
    pub owner: String,
    pub layer: String,
    /// Higher = later in the merge = takes precedence.
    pub priority: i64,
    pub body: String,
    pub triggers: Vec<String>,
    pub status: InstructionStatus,
}

impl Instruction {
    /// Build an instruction (status is set by the store method, not here).
    pub fn new(
        id: impl Into<String>,
        owner: impl Into<String>,
        layer: impl Into<String>,
        priority: i64,
        body: impl Into<String>,
        triggers: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            owner: owner.into(),
            layer: layer.into(),
            priority,
            body: body.into(),
            triggers,
            status: InstructionStatus::Active,
        }
    }
}

/// The layered instruction store + review-gated inbox.
#[async_trait]
pub trait KnowledgeBase: Send + Sync {
    /// Add a human-authored/approved instruction (ACTIVE — enters merge/triggers).
    async fn add_active(&self, i: &Instruction) -> Result<()>;
    /// File an auto-memory PROPOSAL (PENDING — inbox only, never auto-applied).
    async fn propose(&self, i: &Instruction) -> Result<()>;
    /// The active instructions concatenated in precedence order (priority asc).
    async fn merged(&self, owner: &str) -> Result<String>;
    /// Active instructions whose any trigger appears (case-insensitive) in
    /// `context`, precedence order.
    async fn triggered(&self, owner: &str, context: &str) -> Result<Vec<Instruction>>;
    /// The pending auto-memory proposals awaiting review.
    async fn inbox(&self, owner: &str) -> Result<Vec<Instruction>>;
    /// Promote a pending proposal to active. Owner-scoped; returns whether one
    /// moved.
    async fn approve(&self, id: &str, owner: &str) -> Result<bool>;
    /// Drop a pending proposal. Owner-scoped; returns whether one was removed.
    async fn reject(&self, id: &str, owner: &str) -> Result<bool>;
}

/// SQLite-backed [`KnowledgeBase`] over the shared memory DB.
#[derive(Clone)]
pub struct InstructionStore {
    pool: SqlitePool,
}

impl InstructionStore {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    async fn insert(&self, i: &Instruction, status: InstructionStatus) -> Result<()> {
        let triggers = serde_json::to_string(&i.triggers)?;
        sqlx::query(
            "INSERT INTO mem_instructions
                 (id, scope_owner, layer, priority, body, triggers, status, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                 layer = excluded.layer, priority = excluded.priority,
                 body = excluded.body, triggers = excluded.triggers,
                 status = excluded.status, at = excluded.at",
        )
        .bind(&i.id)
        .bind(&i.owner)
        .bind(&i.layer)
        .bind(i.priority)
        .bind(&i.body)
        .bind(&triggers)
        .bind(status.as_str())
        .bind(agent24_core::util::now_iso8601())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    fn row_to_instruction(row: &sqlx::sqlite::SqliteRow) -> Result<Instruction> {
        Ok(Instruction {
            id: row.get("id"),
            owner: row.get("scope_owner"),
            layer: row.get("layer"),
            priority: row.get("priority"),
            body: row.get("body"),
            triggers: serde_json::from_str(&row.get::<String, _>("triggers"))?,
            status: InstructionStatus::parse(&row.get::<String, _>("status")),
        })
    }

    async fn active(&self, owner: &str) -> Result<Vec<Instruction>> {
        let rows = sqlx::query(
            "SELECT id, scope_owner, layer, priority, body, triggers, status
             FROM mem_instructions
             WHERE scope_owner = ? AND status = 'active'
             ORDER BY priority ASC, id ASC",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(Self::row_to_instruction).collect()
    }
}

#[async_trait]
impl KnowledgeBase for InstructionStore {
    async fn add_active(&self, i: &Instruction) -> Result<()> {
        self.insert(i, InstructionStatus::Active).await
    }

    async fn propose(&self, i: &Instruction) -> Result<()> {
        // Forced pending regardless of the passed status — a proposal can never
        // sneak in as active.
        self.insert(i, InstructionStatus::Pending).await
    }

    async fn merged(&self, owner: &str) -> Result<String> {
        let bodies: Vec<String> = self
            .active(owner)
            .await?
            .into_iter()
            .map(|i| i.body)
            .collect();
        Ok(bodies.join("\n\n---\n\n"))
    }

    async fn triggered(&self, owner: &str, context: &str) -> Result<Vec<Instruction>> {
        let ctx = context.to_lowercase();
        Ok(self
            .active(owner)
            .await?
            .into_iter()
            .filter(|i| {
                i.triggers
                    .iter()
                    .any(|t| !t.is_empty() && ctx.contains(&t.to_lowercase()))
            })
            .collect())
    }

    async fn inbox(&self, owner: &str) -> Result<Vec<Instruction>> {
        let rows = sqlx::query(
            "SELECT id, scope_owner, layer, priority, body, triggers, status
             FROM mem_instructions
             WHERE scope_owner = ? AND status = 'pending'
             ORDER BY priority ASC, id ASC",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(Self::row_to_instruction).collect()
    }

    async fn approve(&self, id: &str, owner: &str) -> Result<bool> {
        let res = sqlx::query(
            "UPDATE mem_instructions SET status = 'active'
             WHERE id = ? AND scope_owner = ? AND status = 'pending'",
        )
        .bind(id)
        .bind(owner)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn reject(&self, id: &str, owner: &str) -> Result<bool> {
        let res = sqlx::query(
            "DELETE FROM mem_instructions
             WHERE id = ? AND scope_owner = ? AND status = 'pending'",
        )
        .bind(id)
        .bind(owner)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::KvStore;

    async fn kb() -> InstructionStore {
        KvStore::open_memory().await.unwrap().knowledge()
    }

    fn instr(id: &str, owner: &str, layer: &str, priority: i64, body: &str) -> Instruction {
        Instruction::new(id, owner, layer, priority, body, vec![])
    }

    #[tokio::test]
    async fn merged_respects_layer_precedence() {
        let k = kb().await;
        // Insert out of priority order; merged must order by priority asc.
        k.add_active(&instr("proj", "u1", "project", 10, "PROJECT"))
            .await
            .unwrap();
        k.add_active(&instr("glob", "u1", "global", 0, "GLOBAL"))
            .await
            .unwrap();
        k.add_active(&instr("sess", "u1", "session", 20, "SESSION"))
            .await
            .unwrap();
        let merged = k.merged("u1").await.unwrap();
        assert_eq!(merged, "GLOBAL\n\n---\n\nPROJECT\n\n---\n\nSESSION");
    }

    #[tokio::test]
    async fn trigger_injects_only_on_match() {
        let k = kb().await;
        let mut deploy = instr("d", "u1", "project", 0, "deploy carefully");
        deploy.triggers = vec!["deploy".into(), "release".into()];
        k.add_active(&deploy).await.unwrap();
        k.add_active(&instr("always", "u1", "global", 0, "always"))
            .await
            .unwrap();

        let hit = k.triggered("u1", "please DEPLOY the app").await.unwrap();
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].id, "d", "case-insensitive trigger hit");
        let miss = k.triggered("u1", "just chatting").await.unwrap();
        assert!(miss.is_empty(), "no trigger → no injection");
    }

    #[tokio::test]
    async fn auto_memory_proposal_is_never_auto_applied() {
        let k = kb().await;
        let mut p = instr("auto", "u1", "learned", 5, "the user prefers dark mode");
        p.triggers = vec!["theme".into()];
        k.propose(&p).await.unwrap();

        // NOT in merged, NOT triggered — pending is invisible to the agent.
        assert!(k.merged("u1").await.unwrap().is_empty());
        assert!(
            k.triggered("u1", "change the theme")
                .await
                .unwrap()
                .is_empty()
        );
        // It IS in the inbox for review.
        let inbox = k.inbox("u1").await.unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].id, "auto");
        assert_eq!(inbox[0].status, InstructionStatus::Pending);
    }

    #[tokio::test]
    async fn approve_promotes_reject_removes() {
        let k = kb().await;
        k.propose(&instr("a", "u1", "learned", 0, "APPROVED FACT"))
            .await
            .unwrap();
        k.propose(&instr("r", "u1", "learned", 0, "rejected fact"))
            .await
            .unwrap();

        assert!(k.approve("a", "u1").await.unwrap());
        assert!(k.reject("r", "u1").await.unwrap());
        // Approved is now merged; rejected is gone; inbox empty.
        assert_eq!(k.merged("u1").await.unwrap(), "APPROVED FACT");
        assert!(k.inbox("u1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn approve_reject_are_owner_scoped() {
        let k = kb().await;
        k.propose(&instr("p", "alice", "learned", 0, "alice pending"))
            .await
            .unwrap();
        // bob cannot approve or reject alice's inbox item by id.
        assert!(!k.approve("p", "bob").await.unwrap());
        assert!(!k.reject("p", "bob").await.unwrap());
        assert_eq!(
            k.inbox("alice").await.unwrap().len(),
            1,
            "still pending for alice"
        );
    }

    #[tokio::test]
    async fn scope_isolation_zero_cross_owner_leak() {
        let k = kb().await;
        k.add_active(&instr("a", "alice", "global", 0, "alice-only"))
            .await
            .unwrap();
        k.add_active(&instr("b", "bob", "global", 0, "bob-only"))
            .await
            .unwrap();
        assert_eq!(k.merged("alice").await.unwrap(), "alice-only");
        assert_eq!(k.merged("bob").await.unwrap(), "bob-only");
    }

    #[tokio::test]
    async fn empty_owner_is_rejected() {
        let k = kb().await;
        let err = k
            .add_active(&instr("x", "  ", "global", 0, "y"))
            .await
            .unwrap_err();
        assert!(matches!(err, crate::MemoryError::Sqlx(_)), "{err}");
    }
}
