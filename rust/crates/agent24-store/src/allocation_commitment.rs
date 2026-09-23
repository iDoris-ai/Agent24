#![allow(dead_code, clippy::type_complexity, clippy::items_after_test_module)]
// Dormant until the future registration writer adopts this transaction helper.

use serde_json::json;
use sqlx::{Sqlite, Transaction};

use crate::{
    AllocationIntent, AllocationPhase, AllocationRecord, RootIdentity, WorkspaceKind,
    WorkspaceResult, WorkspaceState, WorkspaceStoreError,
};

fn bad() -> WorkspaceStoreError {
    WorkspaceStoreError::Database
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        LifecycleOwnerRef, NewScratchWorkspace, TrustedRootRegistration, WorkspaceInstant,
        WorkspaceProvenanceInput, WorkspaceTtl,
    };
    use agent24_protocol::WorkspaceId;

    fn intent() -> AllocationIntent {
        AllocationIntent::new(
            crate::AllocationId::parse("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
            WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
            "generation-1".into(),
            "scratch".into(),
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
            WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap(),
        )
        .unwrap()
    }

    async fn fixture() -> (crate::Store, AllocationIntent, RootIdentity) {
        let store = crate::Store::open_memory().await.unwrap();
        let intent = intent();
        store.reserve_workspace_allocation(&intent).await.unwrap();
        let root = RootIdentity::unix(&[3; 8], &[4; 8]).unwrap();
        let ws = NewScratchWorkspace::new(
            intent.workspace_id().clone(),
            TrustedRootRegistration::new("/scratch".into(), intent.root_generation().into(), root)
                .unwrap(),
            WorkspaceProvenanceInput::new("test".into(), None, None).unwrap(),
            LifecycleOwnerRef::parse("test-owner".into()).unwrap(),
            WorkspaceTtl::new(60_000).unwrap(),
        );
        store
            .create_workspace(&ws, ws.lifecycle_owner_ref(), intent.created_at())
            .await
            .unwrap();
        sqlx::query("UPDATE workspace_allocations SET phase='materialized', root_identity_kind='unix', root_unix_device=X'0303030303030303', root_unix_inode=X'0404040404040404' WHERE allocation_id=?")
            .bind(intent.allocation_id().as_str()).execute(crate::test_hooks::pool(&store)).await.unwrap();
        (store, intent, root)
    }

    #[tokio::test]
    async fn commits_once_and_exact_replay_adds_no_audit() {
        let (store, intent, root) = fixture().await;
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert!(matches!(
            commit_materialized_allocation_tx(
                &mut tx,
                &intent,
                RootIdentity::unix(&[9; 8], &[10; 8]).unwrap()
            )
            .await,
            Err(WorkspaceStoreError::Conflict(
                crate::WorkspaceConflict::RootIdentity
            ))
        ));
        drop(tx);
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        let record = commit_materialized_allocation_tx(&mut tx, &intent, root)
            .await
            .unwrap();
        assert_eq!(record.phase(), AllocationPhase::Committed);
        tx.commit().await.unwrap();
        let count = store.list_audit().await.unwrap().len();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        commit_materialized_allocation_tx(&mut tx, &intent, root)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(store.list_audit().await.unwrap().len(), count);
    }

    #[tokio::test]
    async fn audit_tampering_fails_and_rolls_back_commit() {
        let (store, intent, root) = fixture().await;
        let pool = crate::test_hooks::pool(&store);
        sqlx::query("CREATE TRIGGER alter_commit_audit AFTER INSERT ON audit_log WHEN NEW.action='workspace.allocation_committed' BEGIN UPDATE audit_log SET detail='{}' WHERE seq=NEW.seq; END")
            .execute(pool).await.unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert!(matches!(
            commit_materialized_allocation_tx(&mut tx, &intent, root).await,
            Err(WorkspaceStoreError::Database)
        ));
        drop(tx);
        assert_eq!(
            store
                .get_workspace_allocation(intent.allocation_id())
                .await
                .unwrap()
                .phase(),
            AllocationPhase::Materialized
        );
        assert_eq!(store.list_audit().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn legal_workspace_trigger_mutation_rolls_back_all_three_tables() {
        let (store, intent, root) = fixture().await;
        let pool = crate::test_hooks::pool(&store);
        sqlx::query("CREATE TRIGGER allocation_changes_workspace AFTER UPDATE OF phase ON workspace_allocations WHEN NEW.phase='committed' BEGIN UPDATE workspaces SET canonical_root='/other-root' WHERE id='ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'; END")
            .execute(pool).await.unwrap();
        let before: (String, i64, String) = sqlx::query_as(
            "SELECT canonical_root, revision, lifecycle_owner_ref FROM workspaces WHERE id=?",
        )
        .bind(intent.workspace_id().as_str())
        .fetch_one(pool)
        .await
        .unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert!(matches!(
            commit_materialized_allocation_tx(&mut tx, &intent, root).await,
            Err(WorkspaceStoreError::Database)
        ));
        drop(tx);
        let after: (String, i64, String) = sqlx::query_as(
            "SELECT canonical_root, revision, lifecycle_owner_ref FROM workspaces WHERE id=?",
        )
        .bind(intent.workspace_id().as_str())
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(after, before);
        assert_eq!(
            store
                .get_workspace_allocation(intent.allocation_id())
                .await
                .unwrap()
                .phase(),
            AllocationPhase::Materialized
        );
        assert_eq!(store.list_audit().await.unwrap().len(), 1);
        sqlx::query("DROP TRIGGER allocation_changes_workspace")
            .execute(pool)
            .await
            .unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        commit_materialized_allocation_tx(&mut tx, &intent, root)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn first_commit_requires_active_workspace_but_replay_allows_lifecycle_change() {
        let (store, intent, root) = fixture().await;
        sqlx::query("UPDATE workspaces SET state='expired' WHERE id=?")
            .bind(intent.workspace_id().as_str())
            .execute(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert!(matches!(
            commit_materialized_allocation_tx(&mut tx, &intent, root).await,
            Err(WorkspaceStoreError::Conflict(_))
        ));
        drop(tx);
        sqlx::query("UPDATE workspaces SET state='active' WHERE id=?")
            .bind(intent.workspace_id().as_str())
            .execute(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        commit_materialized_allocation_tx(&mut tx, &intent, root)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        sqlx::query("UPDATE workspaces SET state='expired' WHERE id=?")
            .bind(intent.workspace_id().as_str())
            .execute(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert_eq!(
            commit_materialized_allocation_tx(&mut tx, &intent, root)
                .await
                .unwrap()
                .phase(),
            AllocationPhase::Committed
        );
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn reserved_and_retained_phases_are_rejected() {
        let (store, intent, root) = fixture().await;
        let pool = crate::test_hooks::pool(&store);
        sqlx::query("UPDATE workspace_allocations SET phase='reserved', root_identity_kind=NULL, root_unix_device=NULL, root_unix_inode=NULL")
            .execute(pool).await.unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert!(matches!(
            commit_materialized_allocation_tx(&mut tx, &intent, root).await,
            Err(WorkspaceStoreError::Conflict(_))
        ));
        drop(tx);
        sqlx::query("UPDATE workspace_allocations SET phase='retained', root_identity_kind='unix', root_unix_device=X'0303030303030303', root_unix_inode=X'0404040404040404', failure_reason='retained'")
            .execute(pool).await.unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert!(matches!(
            commit_materialized_allocation_tx(&mut tx, &intent, root).await,
            Err(WorkspaceStoreError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn allocation_root_collision_blocks_commit() {
        let (store, intent, root) = fixture().await;
        let competitor = AllocationIntent::new(
            crate::AllocationId::parse("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X6").unwrap(),
            WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6").unwrap(),
            "generation-1".into(),
            "other-scratch".into(),
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
            intent.created_at().clone(),
        )
        .unwrap();
        store
            .reserve_workspace_allocation(&competitor)
            .await
            .unwrap();
        sqlx::query("UPDATE workspace_allocations SET phase='materialized', root_identity_kind='unix', root_unix_device=X'0303030303030303', root_unix_inode=X'0404040404040404' WHERE allocation_id=?")
            .bind(competitor.allocation_id().as_str()).execute(crate::test_hooks::pool(&store)).await.unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert!(matches!(
            commit_materialized_allocation_tx(&mut tx, &intent, root).await,
            Err(WorkspaceStoreError::Conflict(
                crate::WorkspaceConflict::RootIdentity
            ))
        ));
    }
}

fn columns(
    identity: RootIdentity,
) -> (
    &'static str,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
) {
    match identity {
        RootIdentity::Unix { device, inode } => (
            "unix",
            Some(device.to_vec()),
            Some(inode.to_vec()),
            None,
            None,
        ),
        RootIdentity::Windows {
            volume_serial,
            file_id,
        } => (
            "windows",
            None,
            None,
            Some(volume_serial.to_vec()),
            Some(file_id.to_vec()),
        ),
    }
}

async fn read(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
) -> WorkspaceResult<(AllocationRecord, crate::WorkspaceRow)> {
    let row =
        sqlx::query("SELECT * FROM workspace_allocations WHERE allocation_id = ? COLLATE BINARY")
            .bind(intent.allocation_id().as_str())
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| bad())?
            .ok_or(bad())?;
    let allocation = AllocationRecord::decode(&row).map_err(|_| bad())?;
    if allocation.id() != intent.allocation_id()
        || allocation.workspace_id() != intent.workspace_id()
        || allocation.root_generation() != intent.root_generation()
        || allocation.relative_name() != intent.relative_name()
        || allocation.parent_identity() != intent.parent_identity()
        || allocation.created_at() != intent.created_at()
    {
        return Err(bad());
    }
    let row = sqlx::query("SELECT * FROM workspaces WHERE id = ? COLLATE BINARY AND root_generation = ? COLLATE BINARY")
        .bind(intent.workspace_id().as_str()).bind(intent.root_generation()).fetch_optional(&mut **tx).await.map_err(|_| bad())?.ok_or(bad())?;
    let workspace = crate::WorkspaceRow::decode(&row).map_err(|_| bad())?;
    if workspace.id != *intent.workspace_id()
        || workspace.kind != WorkspaceKind::OrchestratorScratch
        || workspace.root.root_generation() != intent.root_generation()
    {
        return Err(bad());
    }
    Ok((allocation, workspace))
}

async fn collision(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
    root: RootIdentity,
) -> WorkspaceResult<bool> {
    let (kind, ud, ui, wv, wf) = columns(root);
    let a: Option<i64> = sqlx::query_scalar("SELECT 1 FROM workspace_allocations WHERE allocation_id <> ? COLLATE BINARY AND root_identity_kind = ? COLLATE BINARY AND root_unix_device IS ? AND root_unix_inode IS ? AND root_windows_volume IS ? AND root_windows_file_id IS ? LIMIT 1")
        .bind(intent.allocation_id().as_str()).bind(kind).bind(ud.clone()).bind(ui.clone()).bind(wv.clone()).bind(wf.clone())
        .fetch_optional(&mut **tx).await.map_err(|_| bad())?;
    let w: Option<i64> = sqlx::query_scalar("SELECT 1 FROM workspaces WHERE id <> ? COLLATE BINARY AND root_identity_kind = ? COLLATE BINARY AND unix_device IS ? AND unix_inode IS ? AND windows_volume_serial IS ? AND windows_file_id IS ? LIMIT 1")
        .bind(intent.workspace_id().as_str()).bind(kind).bind(ud).bind(ui).bind(wv).bind(wf)
        .fetch_optional(&mut **tx).await.map_err(|_| bad())?;
    Ok(a.is_some() || w.is_some())
}

/// Commit a matching materialized allocation and already inserted scratch workspace.
/// The caller owns the surrounding BEGIN IMMEDIATE and commits only after this returns.
pub(crate) async fn commit_materialized_allocation_tx(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
    root: RootIdentity,
) -> WorkspaceResult<AllocationRecord> {
    let (record, workspace) = read(tx, intent).await?;
    if workspace.root.identity() != root {
        return Err(WorkspaceStoreError::Conflict(
            crate::WorkspaceConflict::RootIdentity,
        ));
    }
    if record.phase() == AllocationPhase::Committed {
        if record.root_identity() != Some(root) {
            return Err(WorkspaceStoreError::Conflict(
                crate::WorkspaceConflict::RootIdentity,
            ));
        }
        return Ok(record);
    }
    if record.phase() != AllocationPhase::Materialized
        || record.root_identity() != Some(root)
        || workspace.state != WorkspaceState::Active
    {
        return Err(WorkspaceStoreError::Conflict(
            crate::WorkspaceConflict::AllocationIdentifier,
        ));
    }
    let original_workspace = workspace.clone();
    if collision(tx, intent, root).await? {
        return Err(WorkspaceStoreError::Conflict(
            crate::WorkspaceConflict::RootIdentity,
        ));
    }
    let (kind, ud, ui, wv, wf) = columns(root);
    let (pk, pud, pui, pwv, pwf) = columns(record.parent_identity());
    let result = sqlx::query("UPDATE workspace_allocations SET phase = 'committed' WHERE allocation_id = ? COLLATE BINARY AND workspace_id = ? COLLATE BINARY AND root_generation = ? COLLATE BINARY AND relative_name = ? COLLATE BINARY AND created_at = ? COLLATE BINARY AND phase = 'materialized' COLLATE BINARY AND root_identity_kind = ? COLLATE BINARY AND root_unix_device IS ? AND root_unix_inode IS ? AND root_windows_volume IS ? AND root_windows_file_id IS ? AND parent_identity_kind = ? COLLATE BINARY AND parent_unix_device IS ? AND parent_unix_inode IS ? AND parent_windows_volume IS ? AND parent_windows_file_id IS ? AND failure_reason IS NULL")
        .bind(intent.allocation_id().as_str()).bind(intent.workspace_id().as_str()).bind(intent.root_generation()).bind(intent.relative_name()).bind(intent.created_at().as_str())
        .bind(kind).bind(ud).bind(ui).bind(wv).bind(wf).bind(pk).bind(pud).bind(pui).bind(pwv).bind(pwf)
        .execute(&mut **tx).await.map_err(|_| bad())?;
    if result.rows_affected() != 1 {
        return Err(bad());
    }
    let (record, workspace) = read(tx, intent).await?;
    if record.phase() != AllocationPhase::Committed
        || record.root_identity() != Some(root)
        || workspace.root.identity() != root
        || workspace.state != WorkspaceState::Active
        || workspace != original_workspace
    {
        return Err(bad());
    }
    let detail = json!({"allocation_id": intent.allocation_id().as_str(), "workspace_id": intent.workspace_id().as_str(), "phase": "committed"});
    let audit = crate::Store::append_audit_tx(
        tx,
        record.created_at().as_str(),
        "workspace_allocation",
        "workspace.allocation_committed",
        &detail,
    )
    .await
    .map_err(|_| bad())?;
    let (record, workspace) = read(tx, intent).await?;
    if record.phase() != AllocationPhase::Committed
        || record.root_identity() != Some(root)
        || workspace.root.identity() != root
        || workspace != original_workspace
        || collision(tx, intent, root).await?
    {
        return Err(bad());
    }
    let persisted = sqlx::query(
        "SELECT ts, actor, action, detail, prev_hash, hash FROM audit_log WHERE seq = ?",
    )
    .bind(audit.seq)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| bad())?
    .ok_or(bad())?;
    use sqlx::Row;
    if persisted.try_get::<String, _>("ts").ok().as_deref() != Some(audit.ts.as_str())
        || persisted.try_get::<String, _>("actor").ok().as_deref() != Some(audit.actor.as_str())
        || persisted.try_get::<String, _>("action").ok().as_deref() != Some(audit.action.as_str())
        || persisted.try_get::<String, _>("detail").ok().as_deref()
            != Some(serde_json::to_string(&detail).map_err(|_| bad())?.as_str())
        || persisted.try_get::<String, _>("prev_hash").ok().as_deref()
            != Some(audit.prev_hash.as_str())
        || persisted.try_get::<String, _>("hash").ok().as_deref() != Some(audit.hash.as_str())
    {
        return Err(bad());
    }
    Ok(record)
}
