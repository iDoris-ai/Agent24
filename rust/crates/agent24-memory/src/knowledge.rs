//! MD-7: the knowledge/instruction layer (L4) — hierarchical CLAUDE.md-style
//! instructions merged by precedence, trigger-injected, with a REVIEW-GATED
//! auto-memory inbox (SPEC-MD-ME §3 MD-7; gemini-cli's layered instructions +
//! "never auto-apply" inbox).
//!
//! - [`KnowledgeBase::merged`] concatenates the ACTIVE instructions in precedence
//!   order (priority ascending, so the highest-priority / most-specific layer
//!   comes LAST and wins for a top-down reader — CLAUDE.md semantics). Ties break
//!   by WRITE TIME then id, so a caller-chosen id no longer silently encodes
//!   precedence (review #124 M2).
//! - [`KnowledgeBase::triggered`] returns the active instructions whose triggers
//!   appear in a context string (conditional injection).
//! - [`KnowledgeBase::propose`] files an auto-memory PROPOSAL as `pending`; it is
//!   NEVER part of `merged`/`triggered` until [`KnowledgeBase::approve`] promotes
//!   it. Auto-memory is never auto-applied — a human gates it.
//!
//! **All writes are owner-scoped** — the row identity is the PAIR
//! `(scope_owner, id)`, so a foreign id can neither rewrite another owner's
//! instruction nor approve/reject their inbox (#119's lesson, applied to the
//! WRITE path too — review #124 B1).
//!
//! **What the review gate protects.** Not merely "a proposal cannot promote
//! itself", but "auto-memory cannot change the IN-FORCE instruction set without a
//! human". So `propose` also refuses to overwrite an ACTIVE row: silently
//! replacing an approved rule with a pending one would REMOVE it from `merged`
//! — un-approving by the back door (review #124 B2). Retracting a rule needs the
//! same human as adding one.

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
    /// Promote a pending proposal to active, with the REVIEWER's layer/priority —
    /// not the proposal's self-chosen ones. A proposal that picked
    /// `priority = i64::MAX` would otherwise outrank a human policy the moment it
    /// was approved, bundling "approve this text" with "approve this precedence"
    /// (review #124 M1). Owner-scoped; returns whether one moved.
    async fn approve(&self, id: &str, owner: &str, layer: &str, priority: i64) -> Result<bool>;
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

    /// Triggers, trimmed with blanks dropped: a whitespace-only trigger is a
    /// substring of nearly any context and would become a permanent injection
    /// surface once approved (review #124).
    fn clean_triggers(triggers: &[String]) -> Vec<String> {
        triggers
            .iter()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect()
    }

    /// Upsert on the (owner, id) PAIR. `only_when_pending` gates the update so a
    /// proposal cannot clobber an ACTIVE row (review #124 B2); the caller turns a
    /// no-op into a `Conflict` rather than reporting silent success.
    async fn upsert(
        &self,
        i: &Instruction,
        status: InstructionStatus,
        only_when_pending: bool,
    ) -> Result<u64> {
        let triggers = serde_json::to_string(&Self::clean_triggers(&i.triggers))?;
        let sql = if only_when_pending {
            "INSERT INTO mem_instructions
                 (scope_owner, id, layer, priority, body, triggers, status, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(scope_owner, id) DO UPDATE SET
                 layer = excluded.layer, priority = excluded.priority,
                 body = excluded.body, triggers = excluded.triggers,
                 status = excluded.status, at = excluded.at
                 WHERE mem_instructions.status = 'pending'"
        } else {
            "INSERT INTO mem_instructions
                 (scope_owner, id, layer, priority, body, triggers, status, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(scope_owner, id) DO UPDATE SET
                 layer = excluded.layer, priority = excluded.priority,
                 body = excluded.body, triggers = excluded.triggers,
                 status = excluded.status, at = excluded.at"
        };
        let res = sqlx::query(sql)
            .bind(&i.owner)
            .bind(&i.id)
            .bind(&i.layer)
            .bind(i.priority)
            .bind(&i.body)
            .bind(&triggers)
            .bind(status.as_str())
            .bind(agent24_core::util::now_iso8601())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
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
             ORDER BY priority ASC, at ASC, id ASC",
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
        // The trusted human write path: may overwrite this owner's own row.
        self.upsert(i, InstructionStatus::Active, false).await?;
        Ok(())
    }

    async fn propose(&self, i: &Instruction) -> Result<()> {
        // Forced pending (a proposal can never sneak in as active) AND refused
        // against an active row (it must not un-approve one either).
        let affected = self.upsert(i, InstructionStatus::Pending, true).await?;
        if affected == 0 {
            return Err(crate::MemoryError::Conflict(format!(
                "instruction {} is ACTIVE for owner {}; a proposal cannot overwrite \
                 an approved instruction — retracting one needs the same human review",
                i.id, i.owner
            )));
        }
        Ok(())
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
             ORDER BY priority ASC, at ASC, id ASC",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(Self::row_to_instruction).collect()
    }

    async fn approve(&self, id: &str, owner: &str, layer: &str, priority: i64) -> Result<bool> {
        let res = sqlx::query(
            "UPDATE mem_instructions SET status = 'active', layer = ?, priority = ?
             WHERE id = ? AND scope_owner = ? AND status = 'pending'",
        )
        .bind(layer)
        .bind(priority)
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

        assert!(k.approve("a", "u1", "learned", 0).await.unwrap());
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
        assert!(!k.approve("p", "bob", "learned", 0).await.unwrap());
        assert!(!k.reject("p", "bob").await.unwrap());
        assert_eq!(
            k.inbox("alice").await.unwrap().len(),
            1,
            "still pending for alice"
        );
    }

    #[tokio::test]
    async fn writes_are_owner_scoped_same_id_across_owners_do_not_collide() {
        // B1: the case the old isolation test could never hit — the SAME id under
        // two owners. bob's write must not rewrite alice's row (nor forge its
        // attribution).
        let k = kb().await;
        k.add_active(&instr("shared", "alice", "global", 0, "ALICE-RULE"))
            .await
            .unwrap();
        k.add_active(&instr("shared", "bob", "global", 0, "BOB-RULE"))
            .await
            .unwrap();
        assert_eq!(
            k.merged("alice").await.unwrap(),
            "ALICE-RULE",
            "alice intact"
        );
        assert_eq!(k.merged("bob").await.unwrap(), "BOB-RULE");
    }

    #[tokio::test]
    async fn proposal_cannot_overwrite_an_approved_instruction() {
        // B2: the review gate protects the IN-FORCE set, not just self-promotion.
        // A proposal reusing an active id must ERROR, not silently un-approve it.
        let k = kb().await;
        k.add_active(&instr("rule", "u1", "global", 0, "HUMAN-APPROVED-RULE"))
            .await
            .unwrap();
        let err = k
            .propose(&instr("rule", "u1", "learned", 0, "AUTO-MEMORY-CONTENT"))
            .await
            .unwrap_err();
        assert!(matches!(err, crate::MemoryError::Conflict(_)), "{err}");
        // The approved rule is still in force and the inbox stayed empty.
        assert_eq!(k.merged("u1").await.unwrap(), "HUMAN-APPROVED-RULE");
        assert!(k.inbox("u1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn cross_owner_proposal_cannot_touch_another_owners_active_rule() {
        let k = kb().await;
        k.add_active(&instr("rule", "alice", "global", 0, "ALICE-RULE"))
            .await
            .unwrap();
        // bob proposing under his own scope with the same id is fine and isolated.
        k.propose(&instr("rule", "bob", "learned", 0, "BOB-PROPOSAL"))
            .await
            .unwrap();
        assert_eq!(k.merged("alice").await.unwrap(), "ALICE-RULE", "untouched");
        assert!(
            k.inbox("alice").await.unwrap().is_empty(),
            "not in alice's inbox"
        );
        assert_eq!(k.inbox("bob").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn approve_uses_the_reviewers_priority_not_the_proposals() {
        // M1: a proposal picking i64::MAX must not outrank a human policy just by
        // being approved — the reviewer sets layer/priority.
        let k = kb().await;
        k.add_active(&instr("policy", "u1", "global", 100, "HUMAN-POLICY"))
            .await
            .unwrap();
        k.propose(&instr("greedy", "u1", "learned", i64::MAX, "AUTO-WINS"))
            .await
            .unwrap();
        assert!(k.approve("greedy", "u1", "learned", 10).await.unwrap());
        // Reviewer's priority 10 < 100 → the human policy still comes last (wins).
        assert_eq!(
            k.merged("u1").await.unwrap(),
            "AUTO-WINS\n\n---\n\nHUMAN-POLICY"
        );
    }

    #[tokio::test]
    async fn blank_triggers_are_dropped_not_stored() {
        // A whitespace-only trigger matches nearly any context — a permanent
        // injection surface once approved. It is trimmed away at write time.
        let k = kb().await;
        let mut i = instr("t", "u1", "global", 0, "body");
        i.triggers = vec!["  ".into(), "".into(), " deploy ".into()];
        k.add_active(&i).await.unwrap();
        assert!(
            k.triggered("u1", "unrelated chatter")
                .await
                .unwrap()
                .is_empty(),
            "blank trigger must not match everything"
        );
        assert_eq!(
            k.triggered("u1", "time to DEPLOY").await.unwrap().len(),
            1,
            "the real trigger still works, trimmed"
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
