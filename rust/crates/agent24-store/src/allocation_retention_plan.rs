#![allow(dead_code)]

use sqlx::{Sqlite, Transaction};

use crate::{
    AllocationFailureReason, AllocationId, AllocationPhase, AllocationRecord, RootIdentity, Store,
    WorkspaceInstant, WorkspaceResult, WorkspaceRow, WorkspaceState, WorkspaceStoreError,
    allocation_retention_evidence::RetentionEvidence,
};
use agent24_protocol::WorkspaceId;

/// Immutable, exact replay identity for a retained allocation journal row.
///
/// This deliberately carries the original allocation inputs rather than an
/// `AllocationIntent`: the plan is a state snapshot and has no writer API.
#[derive(Clone)]
struct AllocationReplay {
    allocation_id: AllocationId,
    workspace_id: WorkspaceId,
    root_generation: String,
    relative_name: String,
    parent_identity: RootIdentity,
    created_at: WorkspaceInstant,
}

impl AllocationReplay {
    fn from_record(record: &AllocationRecord) -> Self {
        Self {
            allocation_id: record.id().clone(),
            workspace_id: record.workspace_id().clone(),
            root_generation: record.root_generation().into(),
            relative_name: record.relative_name().into(),
            parent_identity: record.parent_identity(),
            created_at: record.created_at().clone(),
        }
    }

    fn matches(&self, record: &AllocationRecord) -> bool {
        self.allocation_id == *record.id()
            && self.workspace_id == *record.workspace_id()
            && self.root_generation == record.root_generation()
            && self.relative_name == record.relative_name()
            && self.parent_identity == record.parent_identity()
            && self.created_at == *record.created_at()
    }
}

/// Nullable allocation-root values for a future exact `WHERE` clause.
///
/// Reserved retention intentionally has no root identity. Keeping that null
/// state explicit prevents a later writer from treating it as a wildcard.
struct NullableRootCas {
    root_generation: String,
    root_identity: Option<RootIdentity>,
}

impl NullableRootCas {
    fn from_record(record: &AllocationRecord) -> Self {
        Self {
            root_generation: record.root_generation().into(),
            root_identity: record.root_identity(),
        }
    }

    fn matches(&self, record: &AllocationRecord) -> bool {
        self.root_generation == record.root_generation()
            && self.root_identity == record.root_identity()
    }
}

/// Read-only decision state for one already-retained allocation.
///
/// `current` and `expected` are intentionally both `Retained`: this plan is
/// for an exact replay, not the future state-changing retention writer. The
/// evidence seam owns audit-chain validation; this type retains only the
/// state required to verify a replay under the caller's transaction.
pub(crate) struct AllocationRetentionPlan {
    current: AllocationPhase,
    expected: AllocationPhase,
    reason: AllocationFailureReason,
    source: AllocationPhase,
    replay: AllocationReplay,
    root_cas: NullableRootCas,
    registry: Option<WorkspaceRow>,
}

impl AllocationRetentionPlan {
    fn from_evidence(evidence: RetentionEvidence) -> WorkspaceResult<Self> {
        let reason = evidence.allocation.failure_reason().cloned().ok_or(
            WorkspaceStoreError::CorruptRow {
                table: "workspace_allocations",
                field: "failure_reason",
            },
        )?;
        Ok(Self {
            current: evidence.allocation.phase(),
            expected: AllocationPhase::Retained,
            reason,
            source: evidence.source_phase,
            replay: AllocationReplay::from_record(&evidence.allocation),
            root_cas: NullableRootCas::from_record(&evidence.allocation),
            registry: evidence.workspace,
        })
    }

    /// Purely verify the allocation and optional registry row against this
    /// immutable plan. The complete private `WorkspaceRow` is compared so a
    /// lifecycle transition cannot be mistaken for the original active row.
    pub(crate) fn verify(
        &self,
        allocation: &AllocationRecord,
        registry: Option<&WorkspaceRow>,
    ) -> WorkspaceResult<()> {
        if self.current != AllocationPhase::Retained
            || self.expected != AllocationPhase::Retained
            || allocation.phase() != self.expected
            || allocation.failure_reason() != Some(&self.reason)
            || !self.replay.matches(allocation)
            || !self.root_cas.matches(allocation)
        {
            return Err(WorkspaceStoreError::Conflict(
                crate::WorkspaceConflict::AllocationIdentifier,
            ));
        }

        match self.source {
            AllocationPhase::Reserved
                if self.root_cas.root_identity.is_none()
                    && self.registry.is_none()
                    && registry.is_none() =>
            {
                Ok(())
            }
            AllocationPhase::Materialized
                if self.root_cas.root_identity.is_some()
                    && self.registry.is_none()
                    && registry.is_none() =>
            {
                Ok(())
            }
            AllocationPhase::Committed
                if self.root_cas.root_identity.is_some()
                    && self.registry.as_ref() == registry
                    && registry.is_some_and(|workspace| {
                        workspace.id == *allocation.workspace_id()
                            && workspace.state == WorkspaceState::Active
                            && workspace.root.root_generation() == allocation.root_generation()
                            && Some(workspace.root.identity()) == self.root_cas.root_identity
                    }) =>
            {
                Ok(())
            }
            _ => Err(WorkspaceStoreError::CorruptRow {
                table: "workspace_allocations",
                field: "retention_state",
            }),
        }
    }
}

impl Store {
    /// Build a read-only retained-allocation plan in the caller's transaction.
    pub(crate) async fn retained_allocation_state_plan_tx(
        tx: &mut Transaction<'_, Sqlite>,
        intent: &crate::AllocationIntent,
    ) -> WorkspaceResult<AllocationRetentionPlan> {
        let evidence = Self::retained_allocation_evidence_tx(tx, intent).await?;
        AllocationRetentionPlan::from_evidence(evidence)
    }

    /// Build a read-only retained-allocation plan in a short transaction.
    pub(crate) async fn retained_allocation_state_plan(
        &self,
        intent: &crate::AllocationIntent,
    ) -> WorkspaceResult<AllocationRetentionPlan> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        let plan = Self::retained_allocation_state_plan_tx(&mut tx, intent).await?;
        tx.commit()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        Ok(plan)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, TrustedRootRegistration,
        WorkspaceProvenanceInput, WorkspaceTtl,
    };
    use serde_json::json;

    fn intent() -> crate::AllocationIntent {
        crate::AllocationIntent::new(
            AllocationId::parse("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
            WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
            "generation-1".into(),
            "scratch".into(),
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
            WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap(),
        )
        .unwrap()
    }

    async fn retained(
        store: &Store,
        input: &crate::AllocationIntent,
        source: AllocationPhase,
    ) -> Option<WorkspaceRow> {
        store.reserve_workspace_allocation(input).await.unwrap();
        let root = RootIdentity::unix(&[3; 8], &[4; 8]).unwrap();
        let workspace = if source != AllocationPhase::Reserved {
            store
                .materialize_workspace_allocation(input, root)
                .await
                .unwrap();
            if source == AllocationPhase::Committed {
                let owner = LifecycleOwnerRef::parse("owner".into()).unwrap();
                let workspace = NewScratchWorkspace::new(
                    input.workspace_id().clone(),
                    TrustedRootRegistration::new(
                        "/scratch".into(),
                        input.root_generation().into(),
                        root,
                    )
                    .unwrap(),
                    WorkspaceProvenanceInput::new("test".into(), None, None).unwrap(),
                    owner.clone(),
                    WorkspaceTtl::new(60_000).unwrap(),
                );
                let now = WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap();
                store
                    .register_materialized_scratch(input, &workspace, &owner, &now)
                    .await
                    .unwrap();
                Some(
                    sqlx::query("SELECT * FROM workspaces WHERE id=?")
                        .bind(input.workspace_id().as_str())
                        .fetch_one(crate::test_hooks::pool(store))
                        .await
                        .map(|row| WorkspaceRow::decode(&row).unwrap())
                        .unwrap(),
                )
            } else {
                None
            }
        } else {
            None
        };
        sqlx::query(
            "UPDATE workspace_allocations SET phase='retained', failure_reason='io_error'
             WHERE allocation_id=?",
        )
        .bind(input.allocation_id().as_str())
        .execute(crate::test_hooks::pool(store))
        .await
        .unwrap();
        let source = match source {
            AllocationPhase::Reserved => "reserved",
            AllocationPhase::Materialized => "materialized",
            AllocationPhase::Committed => "committed",
            AllocationPhase::Retained => unreachable!(),
        };
        let mut detail = json!({
            "allocation_id": input.allocation_id().as_str(),
            "workspace_id": input.workspace_id().as_str(),
            "phase": "retained",
            "source_phase": source,
        });
        if let Some(row) = &workspace {
            detail["workspace"] = serde_json::to_value(row.project()).unwrap();
        }
        store
            .append_audit(
                input.created_at().as_str(),
                "workspace_allocation",
                "workspace.allocation_retained",
                &detail,
            )
            .await
            .unwrap();
        workspace
    }

    async fn record(store: &Store, input: &crate::AllocationIntent) -> AllocationRecord {
        store
            .get_workspace_allocation(input.allocation_id())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn plans_all_three_sources_and_nullable_root_cas() {
        for (source, root) in [
            (AllocationPhase::Reserved, None),
            (
                AllocationPhase::Materialized,
                Some(RootIdentity::unix(&[3; 8], &[4; 8]).unwrap()),
            ),
            (
                AllocationPhase::Committed,
                Some(RootIdentity::unix(&[3; 8], &[4; 8]).unwrap()),
            ),
        ] {
            let store = Store::open_memory().await.unwrap();
            let input = intent();
            let workspace = retained(&store, &input, source).await;
            let plan = store.retained_allocation_state_plan(&input).await.unwrap();
            assert_eq!(plan.current, AllocationPhase::Retained);
            assert_eq!(plan.expected, AllocationPhase::Retained);
            assert_eq!(plan.source, source);
            assert_eq!(plan.root_cas.root_identity, root);
            assert_eq!(plan.registry, workspace);
            plan.verify(&record(&store, &input).await, plan.registry.as_ref())
                .unwrap();
        }
    }

    #[tokio::test]
    async fn committed_plan_requires_exact_active_registry_lifecycle() {
        let store = Store::open_memory().await.unwrap();
        let input = intent();
        retained(&store, &input, AllocationPhase::Committed).await;
        let plan = store.retained_allocation_state_plan(&input).await.unwrap();
        let allocation = record(&store, &input).await;
        let mut stale = plan.registry.clone().unwrap();
        stale.revision += 1;
        assert!(plan.verify(&allocation, Some(&stale)).is_err());
        stale = plan.registry.clone().unwrap();
        stale.state = WorkspaceState::Expired;
        assert!(plan.verify(&allocation, Some(&stale)).is_err());
        assert!(plan.verify(&allocation, None).is_err());
    }

    #[tokio::test]
    async fn verifier_rejects_stale_phase_root_reason_and_replay() {
        let store = Store::open_memory().await.unwrap();
        let input = intent();
        retained(&store, &input, AllocationPhase::Materialized).await;
        let plan = store.retained_allocation_state_plan(&input).await.unwrap();
        sqlx::query("UPDATE workspace_allocations SET phase='materialized', failure_reason=NULL WHERE allocation_id=?")
            .bind(input.allocation_id().as_str()).execute(crate::test_hooks::pool(&store)).await.unwrap();
        assert!(plan.verify(&record(&store, &input).await, None).is_err());
        sqlx::query("UPDATE workspace_allocations SET phase='retained', failure_reason='io_error', root_unix_device=X'0909090909090909' WHERE allocation_id=?")
            .bind(input.allocation_id().as_str()).execute(crate::test_hooks::pool(&store)).await.unwrap();
        assert!(plan.verify(&record(&store, &input).await, None).is_err());
        sqlx::query("UPDATE workspace_allocations SET root_unix_device=X'0303030303030303', failure_reason='other_reason' WHERE allocation_id=?")
            .bind(input.allocation_id().as_str()).execute(crate::test_hooks::pool(&store)).await.unwrap();
        assert!(plan.verify(&record(&store, &input).await, None).is_err());
        sqlx::query(
            "UPDATE workspace_allocations SET failure_reason='io_error' WHERE allocation_id=?",
        )
        .bind(input.allocation_id().as_str())
        .execute(crate::test_hooks::pool(&store))
        .await
        .unwrap();
        let mut replay = plan.replay.clone();
        replay.relative_name = "other".into();
        let forged = AllocationRetentionPlan { replay, ..plan };
        assert!(forged.verify(&record(&store, &input).await, None).is_err());
    }

    #[tokio::test]
    async fn plan_tx_is_read_only_and_pure_verifier_needs_no_store() {
        let store = Store::open_memory().await.unwrap();
        let input = intent();
        retained(&store, &input, AllocationPhase::Reserved).await;
        let allocation_before: (String, Option<String>, Option<Vec<u8>>) = sqlx::query_as(
            "SELECT phase, failure_reason, root_unix_device FROM workspace_allocations
             WHERE allocation_id=?",
        )
        .bind(input.allocation_id().as_str())
        .fetch_one(crate::test_hooks::pool(&store))
        .await
        .unwrap();
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        let mut tx = store.pool().begin().await.unwrap();
        let plan = Store::retained_allocation_state_plan_tx(&mut tx, &input)
            .await
            .unwrap();
        let row = sqlx::query("SELECT * FROM workspace_allocations WHERE allocation_id=?")
            .bind(input.allocation_id().as_str())
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        plan.verify(&AllocationRecord::decode(&row).unwrap(), None)
            .unwrap();
        tx.commit().await.unwrap();
        let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(after, before);
        let allocation_after: (String, Option<String>, Option<Vec<u8>>) = sqlx::query_as(
            "SELECT phase, failure_reason, root_unix_device FROM workspace_allocations
             WHERE allocation_id=?",
        )
        .bind(input.allocation_id().as_str())
        .fetch_one(crate::test_hooks::pool(&store))
        .await
        .unwrap();
        assert_eq!(allocation_after, allocation_before);
    }
}
