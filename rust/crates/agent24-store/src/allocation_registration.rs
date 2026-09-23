#![allow(dead_code)]

use sqlx::{Sqlite, Transaction};

use crate::{
    AllocationIntent, AllocationPhase, AllocationRecord, LifecycleOwnerRef, NewScratchWorkspace,
    Store, WorkspaceInstant, WorkspaceKind, WorkspaceResult, WorkspaceRow, WorkspaceState,
    WorkspaceStoreError, allocation_commitment::commit_materialized_allocation_tx,
    workspace_registry::insert_workspace,
};

pub(crate) enum RegistrationOutcome {
    Registered {
        allocation: AllocationRecord,
        workspace: WorkspaceRow,
    },
    AlreadyCommitted {
        allocation: AllocationRecord,
        workspace: WorkspaceRow,
    },
}

fn mismatch() -> WorkspaceStoreError {
    WorkspaceStoreError::Conflict(crate::WorkspaceConflict::AllocationIdentifier)
}

async fn allocation(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
) -> WorkspaceResult<AllocationRecord> {
    let row = sqlx::query(
        "SELECT * FROM workspace_allocations
         WHERE allocation_id = ? COLLATE BINARY LIMIT 1",
    )
    .bind(intent.allocation_id().as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?
    .ok_or(WorkspaceStoreError::NotFound)?;
    AllocationRecord::decode(&row)
}

async fn workspace(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
) -> WorkspaceResult<WorkspaceRow> {
    let row = sqlx::query("SELECT * FROM workspaces WHERE id = ? COLLATE BINARY LIMIT 1")
        .bind(intent.workspace_id().as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?
        .ok_or(WorkspaceStoreError::CorruptRow {
            table: "workspaces",
            field: "row",
        })?;
    WorkspaceRow::decode(&row)
}

fn matches_allocation(record: &AllocationRecord, intent: &AllocationIntent) -> bool {
    record.id() == intent.allocation_id()
        && record.workspace_id() == intent.workspace_id()
        && record.root_generation() == intent.root_generation()
        && record.relative_name() == intent.relative_name()
        && record.parent_identity() == intent.parent_identity()
        && record.created_at() == intent.created_at()
        && record.failure_reason().is_none()
}

fn matches_workspace(
    row: &WorkspaceRow,
    intent: &AllocationIntent,
    input: &NewScratchWorkspace,
    owner: &LifecycleOwnerRef,
    expires_at: &WorkspaceInstant,
) -> bool {
    row.id == *intent.workspace_id()
        && row.kind == WorkspaceKind::OrchestratorScratch
        && row.state == WorkspaceState::Active
        && row.root == *input.root()
        && row.created_at == *intent.created_at()
        && row.expires_at == *expires_at
        && row.renewed_at.is_none()
        && row.released_at.is_none()
        && row.revision == 1
        && row.ttl == input.ttl()
        && row.authority.provenance_source == input.provenance().source()
        && row.authority.provenance_project_ref.as_deref() == input.provenance().project_ref()
        && row.authority.provenance_base_revision.as_deref() == input.provenance().base_revision()
        && row.authority.lifecycle_owner_kind == "orchestrator"
        && row.authority.lifecycle_owner_ref == owner.as_str()
        && row.authority.writeback_policy == "external"
        && row.authority.concurrency_policy == "serial"
        && row.cleanup.state == WorkspaceState::Active
        && row.cleanup.quarantine_root.is_none()
        && row.cleanup.quarantined_at.is_none()
        && row.cleanup.attempts == 0
        && row.cleanup.last_attempt_at.is_none()
        && row.cleanup.error.is_none()
        && row.cleanup.retry_at.is_none()
}

impl Store {
    /// Register one materialized scratch allocation and return its public
    /// workspace snapshot. This is a registration result, not an admission
    /// check; callers must use the lifecycle APIs to establish current use.
    ///
    /// A fresh registration rejects an expired instant, while an exact
    /// committed replay returns the original validated snapshot without
    /// writing or re-reading it after commit.
    pub async fn register_workspace_allocation(
        &self,
        intent: &AllocationIntent,
        input: &NewScratchWorkspace,
        owner: &LifecycleOwnerRef,
        registration_now: &WorkspaceInstant,
    ) -> WorkspaceResult<agent24_protocol::Workspace> {
        let outcome = self
            .register_materialized_scratch(intent, input, owner, registration_now)
            .await?;
        let workspace = match outcome {
            RegistrationOutcome::Registered { workspace, .. }
            | RegistrationOutcome::AlreadyCommitted { workspace, .. } => workspace,
        };
        Ok(workspace.project())
    }

    /// Register the exact scratch workspace for a materialized allocation and
    /// commit both journal and registry changes under one SQLite write lock.
    pub(crate) async fn register_materialized_scratch(
        &self,
        intent: &AllocationIntent,
        input: &NewScratchWorkspace,
        owner: &LifecycleOwnerRef,
        registration_now: &WorkspaceInstant,
    ) -> WorkspaceResult<RegistrationOutcome> {
        if input.lifecycle_owner_ref() != owner {
            return Err(WorkspaceStoreError::InvalidValue {
                field: "lifecycle_owner_ref",
            });
        }
        if input.id() != intent.workspace_id()
            || input.root().root_generation() != intent.root_generation()
        {
            return Err(mismatch());
        }
        let expires_at = intent.created_at().checked_add_workspace_ttl(input.ttl())?;
        let mut tx = self.begin_workspace_immediate().await?;
        let record = allocation(&mut tx, intent).await?;
        if !matches_allocation(&record, intent) {
            return Err(mismatch());
        }
        let root = input.root().identity();
        match record.phase() {
            AllocationPhase::Committed => {
                if record.root_identity() != Some(root) {
                    return Err(WorkspaceStoreError::Conflict(
                        crate::WorkspaceConflict::RootIdentity,
                    ));
                }
                let row = workspace(&mut tx, intent).await?;
                if !matches_workspace(&row, intent, input, owner, &expires_at) {
                    return Err(WorkspaceStoreError::CorruptRow {
                        table: "workspaces",
                        field: "row",
                    });
                }
                tx.commit()
                    .await
                    .map_err(|_| WorkspaceStoreError::Database)?;
                Ok(RegistrationOutcome::AlreadyCommitted {
                    allocation: record,
                    workspace: row,
                })
            }
            AllocationPhase::Materialized if record.root_identity() == Some(root) => {
                if registration_now < intent.created_at() || registration_now >= &expires_at {
                    return Err(WorkspaceStoreError::InvalidValue {
                        field: "expires_at",
                    });
                }
                insert_workspace(&mut tx, input, intent.created_at(), &expires_at).await?;
                let row = workspace(&mut tx, intent).await?;
                if !matches_workspace(&row, intent, input, owner, &expires_at) {
                    return Err(WorkspaceStoreError::Database);
                }
                // The workspace INSERT may run database-owned triggers. Do
                // not let one advance the journal behind this transaction's
                // back: the commitment helper treats an exact Committed row
                // as a replay, which would otherwise skip the commit audit.
                let post_insert = allocation(&mut tx, intent).await?;
                if !matches_allocation(&post_insert, intent)
                    || post_insert.phase() != AllocationPhase::Materialized
                    || post_insert.root_identity() != Some(root)
                {
                    return Err(WorkspaceStoreError::Database);
                }
                let committed = commit_materialized_allocation_tx(&mut tx, intent, root).await?;
                if committed.phase() != AllocationPhase::Committed
                    || committed.root_identity() != Some(root)
                {
                    return Err(WorkspaceStoreError::Database);
                }
                tx.commit()
                    .await
                    .map_err(|_| WorkspaceStoreError::Database)?;
                Ok(RegistrationOutcome::Registered {
                    allocation: committed,
                    workspace: row,
                })
            }
            AllocationPhase::Materialized => Err(WorkspaceStoreError::Conflict(
                crate::WorkspaceConflict::RootIdentity,
            )),
            AllocationPhase::Reserved | AllocationPhase::Retained => Err(mismatch()),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        AllocationId, RootIdentity, TrustedRootRegistration, WorkspaceProvenanceInput, WorkspaceTtl,
    };
    use agent24_protocol::WorkspaceId;

    fn inputs() -> (
        AllocationIntent,
        NewScratchWorkspace,
        LifecycleOwnerRef,
        RootIdentity,
    ) {
        let parent = RootIdentity::unix(&[1; 8], &[2; 8]).unwrap();
        let root = RootIdentity::unix(&[3; 8], &[4; 8]).unwrap();
        let intent = AllocationIntent::new(
            AllocationId::parse("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
            WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
            "generation-1".into(),
            "scratch".into(),
            parent,
            WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap(),
        )
        .unwrap();
        let owner = LifecycleOwnerRef::parse("orchestrator-1".into()).unwrap();
        let input = NewScratchWorkspace::new(
            intent.workspace_id().clone(),
            TrustedRootRegistration::new("/scratch".into(), intent.root_generation().into(), root)
                .unwrap(),
            WorkspaceProvenanceInput::new("test".into(), None, None).unwrap(),
            owner.clone(),
            WorkspaceTtl::new(60_000).unwrap(),
        );
        (intent, input, owner, root)
    }

    #[tokio::test]
    async fn registers_and_exact_replay_adds_no_audit() {
        let store = Store::open_memory().await.unwrap();
        let (intent, input, owner, root) = inputs();
        store.reserve_workspace_allocation(&intent).await.unwrap();
        store
            .materialize_workspace_allocation(&intent, root)
            .await
            .unwrap();
        let now = WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap();
        assert!(matches!(
            store
                .register_materialized_scratch(&intent, &input, &owner, &now)
                .await
                .unwrap(),
            RegistrationOutcome::Registered { .. }
        ));
        let audit_count = store.list_audit().await.unwrap().len();
        assert!(matches!(
            store
                .register_materialized_scratch(&intent, &input, &owner, &now)
                .await
                .unwrap(),
            RegistrationOutcome::AlreadyCommitted { .. }
        ));
        assert_eq!(store.list_audit().await.unwrap().len(), audit_count);
        let row = store
            .get_workspace(&intent.workspace_id().clone())
            .await
            .unwrap();
        assert_eq!(row.created_at, intent.created_at().as_str());
    }

    #[tokio::test]
    async fn rejects_expired_reservation_without_registering() {
        let store = Store::open_memory().await.unwrap();
        let (intent, input, owner, root) = inputs();
        store.reserve_workspace_allocation(&intent).await.unwrap();
        store
            .materialize_workspace_allocation(&intent, root)
            .await
            .unwrap();
        let now = WorkspaceInstant::parse("2026-09-19T00:01:00.000Z").unwrap();
        assert!(matches!(
            store
                .register_materialized_scratch(&intent, &input, &owner, &now)
                .await,
            Err(WorkspaceStoreError::InvalidValue {
                field: "expires_at"
            })
        ));
        assert!(matches!(
            store
                .get_workspace_allocation(intent.allocation_id())
                .await
                .unwrap()
                .phase(),
            AllocationPhase::Materialized
        ));
    }

    #[tokio::test]
    async fn failed_commit_rolls_back_insert_and_exact_retry_registers() {
        let store = Store::open_memory().await.unwrap();
        let (intent, input, owner, root) = inputs();
        store.reserve_workspace_allocation(&intent).await.unwrap();
        store
            .materialize_workspace_allocation(&intent, root)
            .await
            .unwrap();
        let now = WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap();
        let audit_count = store.list_audit().await.unwrap().len();
        let wrong_owner = LifecycleOwnerRef::parse("other-orchestrator".into()).unwrap();
        assert!(matches!(
            store
                .register_materialized_scratch(&intent, &input, &wrong_owner, &now)
                .await,
            Err(WorkspaceStoreError::InvalidValue {
                field: "lifecycle_owner_ref"
            })
        ));

        let pool = crate::test_hooks::pool(&store);
        sqlx::query(
            "CREATE TRIGGER reject_registration_commit
             AFTER UPDATE OF phase ON workspace_allocations
             WHEN NEW.phase = 'committed'
             BEGIN SELECT RAISE(ABORT, 'injected commit failure'); END",
        )
        .execute(pool)
        .await
        .unwrap();
        assert!(matches!(
            store
                .register_materialized_scratch(&intent, &input, &owner, &now)
                .await,
            Err(WorkspaceStoreError::Database)
        ));
        let phase: String =
            sqlx::query_scalar("SELECT phase FROM workspace_allocations WHERE allocation_id = ?")
                .bind(intent.allocation_id().as_str())
                .fetch_one(pool)
                .await
                .unwrap();
        let workspace_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workspaces WHERE id = ?")
                .bind(intent.workspace_id().as_str())
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(phase, "materialized");
        assert_eq!(workspace_count, 0);
        assert_eq!(store.list_audit().await.unwrap().len(), audit_count);

        sqlx::query("DROP TRIGGER reject_registration_commit")
            .execute(pool)
            .await
            .unwrap();
        assert!(matches!(
            store
                .register_materialized_scratch(&intent, &input, &owner, &now)
                .await
                .unwrap(),
            RegistrationOutcome::Registered { .. }
        ));
        let record = store
            .get_workspace_allocation(intent.allocation_id())
            .await
            .unwrap();
        assert_eq!(record.phase(), AllocationPhase::Committed);
        let commits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'workspace.allocation_committed'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(commits, 1);
        assert_eq!(store.list_audit().await.unwrap().len(), audit_count + 1);
    }

    #[tokio::test]
    async fn insert_cannot_advance_allocation_past_commit_audit() {
        let store = Store::open_memory().await.unwrap();
        let (intent, input, owner, root) = inputs();
        store.reserve_workspace_allocation(&intent).await.unwrap();
        store
            .materialize_workspace_allocation(&intent, root)
            .await
            .unwrap();
        let now = WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap();
        let audit_count = store.list_audit().await.unwrap().len();
        let pool = crate::test_hooks::pool(&store);
        sqlx::query(
            "CREATE TRIGGER precommit_registration_insert
             AFTER INSERT ON workspaces
             BEGIN
               UPDATE workspace_allocations SET phase = 'committed'
               WHERE workspace_id = NEW.id;
             END",
        )
        .execute(pool)
        .await
        .unwrap();

        assert!(matches!(
            store
                .register_materialized_scratch(&intent, &input, &owner, &now)
                .await,
            Err(WorkspaceStoreError::Database)
        ));
        let phase: String =
            sqlx::query_scalar("SELECT phase FROM workspace_allocations WHERE allocation_id = ?")
                .bind(intent.allocation_id().as_str())
                .fetch_one(pool)
                .await
                .unwrap();
        let workspace_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workspaces WHERE id = ?")
                .bind(intent.workspace_id().as_str())
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(phase, "materialized");
        assert_eq!(workspace_count, 0);
        assert_eq!(store.list_audit().await.unwrap().len(), audit_count);
    }
}

#[cfg(test)]
#[allow(clippy::type_complexity, clippy::unwrap_used)]
mod adversarial_tests;
