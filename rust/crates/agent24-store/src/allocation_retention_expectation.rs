#![allow(dead_code)]

use sqlx::{Sqlite, Transaction};

use crate::{
    AllocationIntent, AllocationPhase, Store, WorkspaceResult, WorkspaceStoreError,
    allocation_retention_evidence::RetentionEvidence,
    allocation_retention_plan::AllocationRetentionPlan,
};

const ACTOR: &str = "workspace_allocation";
const ACTION: &str = "workspace.allocation_retained";

/// Immutable, already-validated raw audit fields for one retention replay.
#[derive(Clone, PartialEq, Eq)]
struct AuditTuple {
    seq: i64,
    ts: String,
    actor: String,
    action: String,
    raw_detail: String,
    prev_hash: String,
    hash: String,
}

impl AuditTuple {
    fn from_evidence(evidence: &RetentionEvidence) -> Self {
        Self {
            seq: evidence.audit_seq,
            ts: evidence.audit_ts.clone(),
            actor: evidence.audit_actor.clone(),
            action: evidence.audit_action.clone(),
            raw_detail: evidence.audit_raw_detail.clone(),
            prev_hash: evidence.audit_prev_hash.clone(),
            hash: evidence.audit_hash.clone(),
        }
    }
}

/// Source and raw canonical audit tuple, without a tail or write precondition.
struct RetentionAuditExpectation {
    source: AllocationPhase,
    tuple: AuditTuple,
}

/// Exact state and audit expectation for an already-retained allocation.
pub(crate) struct AllocationRetentionExpectation {
    state: AllocationRetentionPlan,
    audit: RetentionAuditExpectation,
}

impl AllocationRetentionExpectation {
    fn from_evidence(evidence: &RetentionEvidence) -> WorkspaceResult<Self> {
        Ok(Self {
            state: AllocationRetentionPlan::from_evidence(evidence)?,
            audit: RetentionAuditExpectation {
                source: evidence.source_phase,
                tuple: AuditTuple::from_evidence(evidence),
            },
        })
    }

    /// Re-read and exactly compare retained state and its canonical audit tuple.
    pub(crate) async fn verify_replay_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        intent: &AllocationIntent,
    ) -> WorkspaceResult<()> {
        let evidence = Store::retained_allocation_evidence_tx(tx, intent).await?;
        self.state
            .verify(&evidence.allocation, evidence.workspace.as_ref())?;
        if self.audit.source == evidence.source_phase
            && self.audit.tuple == AuditTuple::from_evidence(&evidence)
        {
            Ok(())
        } else {
            Err(WorkspaceStoreError::CorruptRow {
                table: "audit_log",
                field: "retention_expectation",
            })
        }
    }
}

/// Replay expectation plus a separately strict audit-tail precondition.
pub(crate) struct AllocationRetentionAuditExpectation {
    replay: AllocationRetentionExpectation,
    tail: crate::audit::StrictAuditTail,
}

impl AllocationRetentionAuditExpectation {
    pub(crate) async fn verify_replay_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        intent: &AllocationIntent,
    ) -> WorkspaceResult<()> {
        self.replay.verify_replay_tx(tx, intent).await
    }

    pub(crate) async fn verify_tail_unchanged_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
    ) -> WorkspaceResult<()> {
        if self.tail == crate::audit::strict_audit_tail_tx(tx).await? {
            Ok(())
        } else {
            Err(WorkspaceStoreError::CorruptRow {
                table: "audit_log",
                field: "tail",
            })
        }
    }

    pub(crate) fn prospective_audit_tuple(
        &self,
        ts: &str,
        actor: &str,
        action: &str,
        raw_detail: &str,
    ) -> WorkspaceResult<crate::audit::ProspectiveAuditTuple> {
        self.tail.prospective(ts, actor, action, raw_detail)
    }
}

impl Store {
    /// Read one retained allocation and build its state/audit replay expectation.
    pub(crate) async fn retained_allocation_expectation_tx(
        tx: &mut Transaction<'_, Sqlite>,
        intent: &AllocationIntent,
    ) -> WorkspaceResult<AllocationRetentionExpectation> {
        let evidence = Self::retained_allocation_evidence_tx(tx, intent).await?;
        AllocationRetentionExpectation::from_evidence(&evidence)
    }

    /// Capture replay evidence and the exact audit append precondition in this transaction.
    pub(crate) async fn retained_allocation_audit_expectation_tx(
        tx: &mut Transaction<'_, Sqlite>,
        intent: &AllocationIntent,
    ) -> WorkspaceResult<AllocationRetentionAuditExpectation> {
        let replay = Self::retained_allocation_expectation_tx(tx, intent).await?;
        Ok(AllocationRetentionAuditExpectation {
            replay,
            tail: crate::audit::strict_audit_tail_tx(tx).await?,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        AllocationId, LifecycleOwnerRef, NewScratchWorkspace, RootIdentity,
        TrustedRootRegistration, WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceTtl,
    };
    use agent24_protocol::WorkspaceId;
    use serde_json::json;
    #[rustfmt::skip]
    fn intent() -> AllocationIntent {
        AllocationIntent::new(
            AllocationId::parse("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
            WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
            "generation-1".into(), "scratch".into(),
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
            WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap(),
        ).unwrap()
    }

    #[rustfmt::skip]
    async fn retained(store: &Store, input: &AllocationIntent, source: AllocationPhase) {
        store.reserve_workspace_allocation(input).await.unwrap();
        let root = RootIdentity::unix(&[3; 8], &[4; 8]).unwrap();
        if source != AllocationPhase::Reserved {
            store.materialize_workspace_allocation(input, root).await.unwrap();
            if source == AllocationPhase::Committed {
                let owner = LifecycleOwnerRef::parse("owner".into()).unwrap();
                let workspace = NewScratchWorkspace::new(input.workspace_id().clone(), TrustedRootRegistration::new("/scratch".into(), input.root_generation().into(), root).unwrap(), WorkspaceProvenanceInput::new("test".into(), None, None).unwrap(), owner.clone(), WorkspaceTtl::new(60_000).unwrap());
                store.register_materialized_scratch(input, &workspace, &owner, &WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap()).await.unwrap();
            }
        }
        sqlx::query("UPDATE workspace_allocations SET phase='retained',failure_reason='io_error' WHERE allocation_id=?").bind(input.allocation_id().as_str()).execute(crate::test_hooks::pool(store)).await.unwrap();
        let source = match source { AllocationPhase::Reserved => "reserved", AllocationPhase::Materialized => "materialized", AllocationPhase::Committed => "committed", AllocationPhase::Retained => unreachable!() };
        let mut detail = json!({"allocation_id":input.allocation_id().as_str(),"workspace_id":input.workspace_id().as_str(),"phase":"retained","source_phase":source});
        if source == "committed" { detail["workspace"] = serde_json::to_value(store.get_workspace(input.workspace_id()).await.unwrap()).unwrap(); }
        store.append_audit(input.created_at().as_str(), ACTOR, ACTION, &detail).await.unwrap();
    }

    #[rustfmt::skip]
    async fn expectation(store: &Store, input: &AllocationIntent) -> AllocationRetentionExpectation {
        let mut tx = store.pool().begin().await.unwrap();
        let expectation = Store::retained_allocation_expectation_tx(&mut tx, input).await.unwrap();
        tx.commit().await.unwrap();
        expectation
    }

    #[rustfmt::skip]
    async fn audit_expectation(store: &Store, input: &AllocationIntent) -> AllocationRetentionAuditExpectation {
        let mut tx = store.pool().begin().await.unwrap();
        let expectation = Store::retained_allocation_audit_expectation_tx(&mut tx, input).await.unwrap();
        tx.commit().await.unwrap(); expectation
    }

    #[tokio::test]
    #[rustfmt::skip]
    async fn replays_reserved_materialized_and_committed_tuples() {
        for source in [AllocationPhase::Reserved, AllocationPhase::Materialized, AllocationPhase::Committed] {
            let store = Store::open_memory().await.unwrap(); let input = intent(); retained(&store, &input, source).await;
            let expected = expectation(&store, &input).await; let before = store.list_audit().await.unwrap().len(); let mut tx = store.pool().begin().await.unwrap();
            expected.verify_replay_tx(&mut tx, &input).await.unwrap(); tx.commit().await.unwrap(); assert_eq!(store.list_audit().await.unwrap().len(), before);
        }
    }

    #[tokio::test]
    #[rustfmt::skip]
    async fn state_mismatches_and_unrelated_append_do_not_confuse_replay() {
        for (source, sql) in [(AllocationPhase::Reserved, "UPDATE workspace_allocations SET failure_reason='other'"), (AllocationPhase::Materialized, "UPDATE workspace_allocations SET root_unix_device=X'0909090909090909'"), (AllocationPhase::Committed, "UPDATE workspaces SET revision=2")] {
            let store = Store::open_memory().await.unwrap(); let input = intent(); retained(&store, &input, source).await;
            let expected = expectation(&store, &input).await; sqlx::query(sql).execute(crate::test_hooks::pool(&store)).await.unwrap();
            let mut tx = store.pool().begin().await.unwrap(); assert!(expected.verify_replay_tx(&mut tx, &input).await.is_err()); tx.commit().await.unwrap();
        }
        let store = Store::open_memory().await.unwrap(); let input = intent(); retained(&store, &input, AllocationPhase::Reserved).await;
        let expected = expectation(&store, &input).await; store.append_audit("2026-09-19T00:00:01.000Z", "other", "other", &json!({})).await.unwrap();
        let mut tx = store.pool().begin().await.unwrap(); expected.verify_replay_tx(&mut tx, &input).await.unwrap(); tx.commit().await.unwrap();
        let wrong = AllocationIntent::new(input.allocation_id().clone(), input.workspace_id().clone(), input.root_generation().into(), "other".into(), input.parent_identity(), input.created_at().clone()).unwrap();
        let mut tx = store.pool().begin().await.unwrap(); assert!(expected.verify_replay_tx(&mut tx, &wrong).await.is_err()); tx.commit().await.unwrap();
    }

    #[tokio::test]
    #[rustfmt::skip]
    async fn audit_tuple_tampering_fails_closed_but_keeps_transaction_usable() {
        for sql in ["UPDATE audit_log SET seq=3 WHERE seq=2", "UPDATE audit_log SET ts='2026-09-19T00:00:01.000Z' WHERE seq=2", "UPDATE audit_log SET actor='other' WHERE seq=2", "UPDATE audit_log SET action='other' WHERE seq=2", "UPDATE audit_log SET prev_hash='x',hash='x' WHERE seq=2", "UPDATE audit_log SET hash=X'00' WHERE seq=2"] {
            let store = Store::open_memory().await.unwrap(); let input = intent(); retained(&store, &input, AllocationPhase::Reserved).await;
            let expected = expectation(&store, &input).await; sqlx::query(sql).execute(crate::test_hooks::pool(&store)).await.unwrap();
            let mut tx = store.pool().begin().await.unwrap(); assert!(expected.verify_replay_tx(&mut tx, &input).await.is_err()); sqlx::query("INSERT INTO audit_log (ts,actor,action,detail,prev_hash,hash) VALUES ('2026-09-19T00:00:02.000Z','test','continued','{}','x','y')").execute(&mut *tx).await.unwrap(); tx.commit().await.unwrap();
        }
    }

    #[tokio::test]
    #[rustfmt::skip]
    async fn raw_detail_rehash_duplicate_and_missing_events_reject() {
        let store = Store::open_memory().await.unwrap(); let input = intent(); retained(&store, &input, AllocationPhase::Reserved).await;
        let expected = expectation(&store, &input).await;
        let (prev, raw): (String, String) = sqlx::query_as("SELECT prev_hash,detail FROM audit_log WHERE seq=2").fetch_one(crate::test_hooks::pool(&store)).await.unwrap();
        let raw = format!(" {raw}"); let hash = crate::audit::entry_hash(&prev, input.created_at().as_str(), ACTOR, ACTION, &raw);
        sqlx::query("UPDATE audit_log SET detail=?,hash=? WHERE seq=2").bind(raw).bind(hash).execute(crate::test_hooks::pool(&store)).await.unwrap();
        let mut tx = store.pool().begin().await.unwrap(); assert!(expected.verify_replay_tx(&mut tx, &input).await.is_err()); tx.rollback().await.unwrap();
        let store = Store::open_memory().await.unwrap(); let input = intent(); retained(&store, &input, AllocationPhase::Reserved).await;
        let expected = expectation(&store, &input).await; let detail = store.list_audit().await.unwrap().last().unwrap().detail.clone(); store.append_audit(input.created_at().as_str(), ACTOR, ACTION, &detail).await.unwrap();
        let mut tx = store.pool().begin().await.unwrap(); assert!(expected.verify_replay_tx(&mut tx, &input).await.is_err()); tx.rollback().await.unwrap();
        let store = Store::open_memory().await.unwrap(); let input = intent(); retained(&store, &input, AllocationPhase::Reserved).await;
        let expected = expectation(&store, &input).await; sqlx::query("DELETE FROM audit_log WHERE seq=2").execute(crate::test_hooks::pool(&store)).await.unwrap();
        let mut tx = store.pool().begin().await.unwrap(); assert!(expected.verify_replay_tx(&mut tx, &input).await.is_err()); tx.rollback().await.unwrap();
    }

    #[tokio::test]
    #[rustfmt::skip]
    async fn unchanged_tail_is_read_only_but_unrelated_append_only_breaks_tail() {
        let store = Store::open_memory().await.unwrap(); let input = intent(); retained(&store, &input, AllocationPhase::Reserved).await;
        let expected = audit_expectation(&store, &input).await; let before = store.list_audit().await.unwrap().len(); let mut tx = store.pool().begin().await.unwrap();
        expected.verify_replay_tx(&mut tx, &input).await.unwrap(); expected.verify_tail_unchanged_tx(&mut tx).await.unwrap();
        let tuple = expected.prospective_audit_tuple("2026-09-19T00:00:02.000Z", "next", "append", "{}").unwrap(); assert_eq!(tuple.seq(), 3); tx.commit().await.unwrap(); assert_eq!(store.list_audit().await.unwrap().len(), before);
        store.append_audit("2026-09-19T00:00:03.000Z", "other", "other", &json!({})).await.unwrap(); let mut tx = store.pool().begin().await.unwrap();
        expected.verify_replay_tx(&mut tx, &input).await.unwrap(); assert!(expected.verify_tail_unchanged_tx(&mut tx).await.is_err()); tx.commit().await.unwrap();
    }

    #[tokio::test]
    #[rustfmt::skip]
    async fn first_prospective_tuple_uses_genesis_and_checked_sequence() {
        let store = Store::open_memory().await.unwrap(); let mut tx = store.pool().begin().await.unwrap(); let tail = crate::audit::strict_audit_tail_tx(&mut tx).await.unwrap();
        let tuple = tail.prospective("2026-09-19T00:00:00.000Z", "actor", "action", "{}").unwrap(); assert_eq!(tuple.seq(), 1); assert_eq!(tuple.prev_hash(), "genesis"); assert_eq!(tuple.hash(), crate::audit::entry_hash("genesis", "2026-09-19T00:00:00.000Z", "actor", "action", "{}")); tx.commit().await.unwrap();
    }

    #[tokio::test]
    #[rustfmt::skip]
    async fn deleted_rehashed_or_blob_tail_rejects_and_transaction_can_continue() {
        for sql in ["DELETE FROM audit_log WHERE seq=2", "UPDATE audit_log SET hash=X'00' WHERE seq=2"] {
            let store = Store::open_memory().await.unwrap(); let input = intent(); retained(&store, &input, AllocationPhase::Reserved).await; let expected = audit_expectation(&store, &input).await;
            sqlx::query(sql).execute(crate::test_hooks::pool(&store)).await.unwrap(); let mut tx = store.pool().begin().await.unwrap(); let error = expected.verify_tail_unchanged_tx(&mut tx).await.unwrap_err();
            if sql.contains("hash=X") { assert_eq!(error, WorkspaceStoreError::CorruptRow { table: "audit_log", field: "hash" }); } else { assert!(matches!(error, WorkspaceStoreError::CorruptRow { .. })); } sqlx::query("INSERT INTO audit_log (ts,actor,action,detail,prev_hash,hash) VALUES ('2026-09-19T00:00:02.000Z','test','continued','{}','x','y')").execute(&mut *tx).await.unwrap(); tx.commit().await.unwrap();
        }
        let store = Store::open_memory().await.unwrap(); let input = intent(); retained(&store, &input, AllocationPhase::Reserved).await; let expected = audit_expectation(&store, &input).await;
        let raw = "{\"changed\":true}"; let hash = crate::audit::entry_hash(&store.list_audit().await.unwrap()[0].hash, input.created_at().as_str(), ACTOR, ACTION, raw);
        sqlx::query("UPDATE audit_log SET detail=?,hash=? WHERE seq=2").bind(raw).bind(hash).execute(crate::test_hooks::pool(&store)).await.unwrap(); let mut tx = store.pool().begin().await.unwrap(); assert!(expected.verify_tail_unchanged_tx(&mut tx).await.is_err()); tx.commit().await.unwrap();
    }

    #[tokio::test]
    #[rustfmt::skip]
    async fn duplicate_or_malformed_high_water_rejects_without_poisoning_caller_transaction() {
        for (sql, error) in [("INSERT INTO sqlite_sequence (name,seq) VALUES ('audit_log',2)", WorkspaceStoreError::CorruptRow { table: "sqlite_sequence", field: "row" }), ("INSERT INTO sqlite_sequence (name,seq) VALUES ('audit_log',X'00')", WorkspaceStoreError::CorruptRow { table: "sqlite_sequence", field: "row" }), ("UPDATE sqlite_sequence SET seq=X'00' WHERE name='audit_log'", WorkspaceStoreError::CorruptRow { table: "sqlite_sequence", field: "seq" }), ("UPDATE sqlite_sequence SET seq=3 WHERE name='audit_log'", WorkspaceStoreError::CorruptRow { table: "audit_log", field: "tail" })] {
            let store = Store::open_memory().await.unwrap(); let input = intent(); retained(&store, &input, AllocationPhase::Reserved).await; sqlx::query(sql).execute(crate::test_hooks::pool(&store)).await.unwrap(); let mut tx = store.pool().begin().await.unwrap();
            let actual = match Store::retained_allocation_audit_expectation_tx(&mut tx, &input).await { Err(actual) => actual, Ok(_) => unreachable!() }; assert_eq!(actual, error); assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log").fetch_one(&mut *tx).await.unwrap(), 2); tx.commit().await.unwrap();
        }
    }
}
