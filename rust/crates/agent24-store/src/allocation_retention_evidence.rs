#![allow(dead_code)]

use serde_json::json;
use sqlx::{Row, Sqlite, Transaction};

use crate::{
    AllocationIntent, AllocationPhase, AllocationRecord, Store, WorkspaceConflict, WorkspaceResult,
    WorkspaceRow, WorkspaceStoreError,
};

pub(crate) struct RetentionEvidence {
    pub(crate) allocation: AllocationRecord,
    pub(crate) source_phase: AllocationPhase,
    pub(crate) workspace: Option<WorkspaceRow>,
    pub(crate) audit_seq: i64,
    pub(crate) audit_ts: String,
    pub(crate) audit_raw_detail: String,
    pub(crate) audit_prev_hash: String,
    pub(crate) audit_hash: String,
}

fn corrupt(table: &'static str, field: &'static str) -> WorkspaceStoreError {
    WorkspaceStoreError::CorruptRow { table, field }
}

fn matches_intent(record: &AllocationRecord, intent: &AllocationIntent) -> bool {
    record.id() == intent.allocation_id()
        && record.workspace_id() == intent.workspace_id()
        && record.root_generation() == intent.root_generation()
        && record.relative_name() == intent.relative_name()
        && record.parent_identity() == intent.parent_identity()
        && record.created_at() == intent.created_at()
}

async fn allocation(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
) -> WorkspaceResult<AllocationRecord> {
    let row = sqlx::query(
        "SELECT * FROM workspace_allocations WHERE allocation_id = ? COLLATE BINARY LIMIT 1",
    )
    .bind(intent.allocation_id().as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?
    .ok_or(WorkspaceStoreError::NotFound)?;
    AllocationRecord::decode(&row)
}

async fn registry(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
) -> WorkspaceResult<WorkspaceRow> {
    let row = sqlx::query("SELECT * FROM workspaces WHERE id = ? COLLATE BINARY AND root_generation = ? COLLATE BINARY LIMIT 1")
        .bind(intent.workspace_id().as_str()).bind(intent.root_generation()).fetch_optional(&mut **tx).await
        .map_err(|_| WorkspaceStoreError::Database)?.ok_or(corrupt("workspaces", "row"))?;
    WorkspaceRow::decode(&row)
}

async fn registry_absent(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
) -> WorkspaceResult<bool> {
    sqlx::query("SELECT 1 FROM workspaces WHERE id = ? COLLATE BINARY LIMIT 1")
        .bind(intent.workspace_id().as_str())
        .fetch_optional(&mut **tx)
        .await
        .map(|row| row.is_none())
        .map_err(|_| WorkspaceStoreError::Database)
}

fn exact_workspace(
    row: &WorkspaceRow,
    intent: &AllocationIntent,
    root: crate::RootIdentity,
) -> bool {
    row.id == *intent.workspace_id()
        && row.kind == crate::WorkspaceKind::OrchestratorScratch
        && row.state == crate::WorkspaceState::Active
        && row.root.root_generation() == intent.root_generation()
        && row.root.identity() == root
}

async fn source_audit(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
    record: &AllocationRecord,
) -> WorkspaceResult<(
    AllocationPhase,
    Option<WorkspaceRow>,
    (i64, String, String, String, String),
)> {
    crate::audit::strict_audit_chain_tx(tx).await?;
    let rows = sqlx::query("SELECT seq, ts, detail, prev_hash, hash FROM audit_log WHERE actor='workspace_allocation' AND action='workspace.allocation_retained'")
        .fetch_all(&mut **tx).await.map_err(|_| WorkspaceStoreError::Database)?;
    let mut event = None;
    for row in rows {
        let seq: i64 = row
            .try_get("seq")
            .map_err(|_| corrupt("audit_log", "seq"))?;
        let ts: String = row
            .try_get("ts")
            .map_err(|_| corrupt("audit_log", "detail"))?;
        let raw: String = row
            .try_get("detail")
            .map_err(|_| corrupt("audit_log", "detail"))?;
        let value: serde_json::Value =
            serde_json::from_str(&raw).map_err(|_| corrupt("audit_log", "detail"))?;
        let prev_hash: String = row
            .try_get("prev_hash")
            .map_err(|_| corrupt("audit_log", "prev_hash"))?;
        let hash: String = row
            .try_get("hash")
            .map_err(|_| corrupt("audit_log", "hash"))?;
        if value
            .get("allocation_id")
            .and_then(serde_json::Value::as_str)
            == Some(intent.allocation_id().as_str())
            && event
                .replace((seq, ts, raw, prev_hash, hash, value))
                .is_some()
        {
            return Err(corrupt("audit_log", "detail"));
        }
    }
    let (seq, ts, raw, prev_hash, hash, detail) = event.ok_or(corrupt("audit_log", "detail"))?;
    if ts != record.created_at().as_str() {
        return Err(corrupt("audit_log", "detail"));
    }
    let source = detail
        .get("source_phase")
        .and_then(serde_json::Value::as_str)
        .ok_or(corrupt("audit_log", "detail"))?;
    let base = |source| json!({"allocation_id": intent.allocation_id().as_str(), "workspace_id": intent.workspace_id().as_str(), "phase": "retained", "source_phase": source});
    let (phase, workspace, expected) = match source {
        "reserved" if record.root_identity().is_none() && registry_absent(tx, intent).await? => {
            (AllocationPhase::Reserved, None, base(source))
        }
        "materialized"
            if record.root_identity().is_some() && registry_absent(tx, intent).await? =>
        {
            (AllocationPhase::Materialized, None, base(source))
        }
        "committed" => {
            let root = record
                .root_identity()
                .ok_or(corrupt("workspace_allocations", "root_identity_kind"))?;
            let workspace = registry(tx, intent).await?;
            if !exact_workspace(&workspace, intent, root) {
                return Err(WorkspaceStoreError::Conflict(
                    WorkspaceConflict::RootIdentity,
                ));
            }
            let expected = json!({"allocation_id": intent.allocation_id().as_str(), "workspace_id": intent.workspace_id().as_str(), "phase": "retained", "source_phase": source, "workspace": workspace.project()});
            (AllocationPhase::Committed, Some(workspace), expected)
        }
        _ => return Err(corrupt("audit_log", "detail")),
    };
    if raw != serde_json::to_string(&expected).map_err(|_| WorkspaceStoreError::Database)? {
        return Err(corrupt("audit_log", "detail"));
    }
    Ok((phase, workspace, (seq, ts, raw, prev_hash, hash)))
}

impl Store {
    pub(crate) async fn retained_allocation_evidence_tx(
        tx: &mut Transaction<'_, Sqlite>,
        intent: &AllocationIntent,
    ) -> WorkspaceResult<RetentionEvidence> {
        let allocation = allocation(tx, intent).await?;
        if !matches_intent(&allocation, intent)
            || allocation.phase() != AllocationPhase::Retained
            || allocation.failure_reason().is_none()
        {
            return Err(WorkspaceStoreError::Conflict(
                WorkspaceConflict::AllocationIdentifier,
            ));
        }
        let (
            source_phase,
            workspace,
            (audit_seq, audit_ts, audit_raw_detail, audit_prev_hash, audit_hash),
        ) = source_audit(tx, intent, &allocation).await?;
        Ok(RetentionEvidence {
            allocation,
            source_phase,
            workspace,
            audit_seq,
            audit_ts,
            audit_raw_detail,
            audit_prev_hash,
            audit_hash,
        })
    }

    pub(crate) async fn retained_allocation_evidence(
        &self,
        intent: &AllocationIntent,
    ) -> WorkspaceResult<RetentionEvidence> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        let evidence = Self::retained_allocation_evidence_tx(&mut tx, intent).await?;
        tx.commit()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        Ok(evidence)
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

    fn intent() -> AllocationIntent {
        AllocationIntent::new(
            AllocationId::parse("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
            WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
            "generation-1".into(),
            "scratch".into(),
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
            WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap(),
        )
        .unwrap()
    }
    async fn retained(store: &Store, input: &AllocationIntent, source: &str, root: bool) {
        store.reserve_workspace_allocation(input).await.unwrap();
        if root {
            store
                .materialize_workspace_allocation(
                    input,
                    RootIdentity::unix(&[3; 8], &[4; 8]).unwrap(),
                )
                .await
                .unwrap();
        }
        sqlx::query("UPDATE workspace_allocations SET phase='retained', failure_reason='io_error' WHERE allocation_id=?")
            .bind(input.allocation_id().as_str()).execute(crate::test_hooks::pool(store)).await.unwrap();
        store.append_audit(input.created_at().as_str(), "workspace_allocation", "workspace.allocation_retained", &json!({"allocation_id": input.allocation_id().as_str(), "workspace_id": input.workspace_id().as_str(), "phase":"retained", "source_phase":source})).await.unwrap();
    }

    #[tokio::test]
    async fn reserved_and_materialized_evidence_are_read_only() {
        for (source, root, phase) in [
            ("reserved", false, AllocationPhase::Reserved),
            ("materialized", true, AllocationPhase::Materialized),
        ] {
            let store = Store::open_memory().await.unwrap();
            let input = intent();
            retained(&store, &input, source, root).await;
            let before = store.list_audit().await.unwrap().len();
            let mut tx = store.pool().begin().await.unwrap();
            let proof = Store::retained_allocation_evidence_tx(&mut tx, &input)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            assert_eq!(proof.source_phase, phase);
            assert!(proof.workspace.is_none());
            assert_eq!(proof.allocation.phase(), AllocationPhase::Retained);
            assert!(
                proof.audit_seq > 0
                    && !proof.audit_ts.is_empty()
                    && !proof.audit_raw_detail.is_empty()
                    && !proof.audit_prev_hash.is_empty()
                    && !proof.audit_hash.is_empty()
            );
            assert_eq!(store.list_audit().await.unwrap().len(), before);
        }
    }

    #[tokio::test]
    async fn canonical_timestamp_unique_and_chain_fail_closed() {
        let store = Store::open_memory().await.unwrap();
        let input = intent();
        retained(&store, &input, "reserved", false).await;
        assert!(store.retained_allocation_evidence(&input).await.is_ok());
        let detail = store
            .list_audit()
            .await
            .unwrap()
            .last()
            .unwrap()
            .detail
            .clone();
        store
            .append_audit(
                input.created_at().as_str(),
                "workspace_allocation",
                "workspace.allocation_retained",
                &detail,
            )
            .await
            .unwrap();
        assert!(store.retained_allocation_evidence(&input).await.is_err());
        let store = Store::open_memory().await.unwrap();
        let input = intent();
        store.reserve_workspace_allocation(&input).await.unwrap();
        sqlx::query("UPDATE workspace_allocations SET phase='retained', failure_reason='io_error' WHERE allocation_id=?").bind(input.allocation_id().as_str()).execute(crate::test_hooks::pool(&store)).await.unwrap();
        store.append_audit("2026-09-19T00:00:01.000Z", "workspace_allocation", "workspace.allocation_retained", &json!({"allocation_id":input.allocation_id().as_str(),"workspace_id":input.workspace_id().as_str(),"phase":"retained","source_phase":"reserved"})).await.unwrap();
        assert!(store.retained_allocation_evidence(&input).await.is_err());
    }

    #[tokio::test]
    async fn committed_source_requires_the_exact_registered_workspace() {
        let store = Store::open_memory().await.unwrap();
        let input = intent();
        store.reserve_workspace_allocation(&input).await.unwrap();
        let root = RootIdentity::unix(&[3; 8], &[4; 8]).unwrap();
        store
            .materialize_workspace_allocation(&input, root)
            .await
            .unwrap();
        let owner = LifecycleOwnerRef::parse("owner".into()).unwrap();
        let workspace = NewScratchWorkspace::new(
            input.workspace_id().clone(),
            TrustedRootRegistration::new("/scratch".into(), input.root_generation().into(), root)
                .unwrap(),
            WorkspaceProvenanceInput::new("test".into(), None, None).unwrap(),
            owner.clone(),
            WorkspaceTtl::new(60_000).unwrap(),
        );
        let now = WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap();
        store
            .register_materialized_scratch(&input, &workspace, &owner, &now)
            .await
            .unwrap();
        sqlx::query("UPDATE workspace_allocations SET phase='retained', failure_reason='io_error' WHERE allocation_id=?").bind(input.allocation_id().as_str()).execute(crate::test_hooks::pool(&store)).await.unwrap();
        let public = store.get_workspace(input.workspace_id()).await.unwrap();
        store.append_audit(input.created_at().as_str(), "workspace_allocation", "workspace.allocation_retained", &json!({"allocation_id":input.allocation_id().as_str(),"workspace_id":input.workspace_id().as_str(),"phase":"retained","source_phase":"committed","workspace":public})).await.unwrap();
        let proof = store.retained_allocation_evidence(&input).await.unwrap();
        assert_eq!(proof.source_phase, AllocationPhase::Committed);
        assert!(proof.workspace.is_some());
    }

    #[tokio::test]
    async fn blob_tail_is_corrupt_and_transaction_can_continue() {
        let store = Store::open_memory().await.unwrap();
        let input = intent();
        retained(&store, &input, "reserved", false).await;
        sqlx::query("UPDATE audit_log SET hash=X'00' WHERE seq=(SELECT MAX(seq) FROM audit_log)")
            .execute(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert!(matches!(
            store.retained_allocation_evidence(&input).await,
            Err(WorkspaceStoreError::CorruptRow {
                table: "audit_log",
                field: "hash"
            })
        ));
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(rows, 2);
    }

    #[tokio::test]
    async fn legal_hash_chain_with_a_sequence_gap_is_rejected() {
        let store = Store::open_memory().await.unwrap();
        let input = intent();
        retained(&store, &input, "reserved", false).await;
        sqlx::query("UPDATE audit_log SET seq=3 WHERE seq=2")
            .execute(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert!(matches!(
            store.retained_allocation_evidence(&input).await,
            Err(WorkspaceStoreError::CorruptRow {
                table: "audit_log",
                field: "chain"
            })
        ));
    }

    #[tokio::test]
    async fn caller_transaction_recovers_after_evidence_error_and_commits_write() {
        let store = Store::open_memory().await.unwrap();
        let input = intent();
        retained(&store, &input, "reserved", false).await;
        sqlx::query("UPDATE audit_log SET hash=X'00' WHERE seq=2")
            .execute(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        let mut tx = store.pool().begin().await.unwrap();
        assert!(
            Store::retained_allocation_evidence_tx(&mut tx, &input)
                .await
                .is_err()
        );
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(rows, 2);
        sqlx::query("INSERT INTO audit_log (ts, actor, action, detail, prev_hash, hash) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(input.created_at().as_str()).bind("test").bind("continued").bind("{}").bind("x").bind("y")
            .execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(rows, 3);
    }
}
