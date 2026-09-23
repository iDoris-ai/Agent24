use serde_json::json;
use sqlx::{Sqlite, Transaction};

use crate::{
    AllocationIntent, AllocationPhase, AllocationRecord, RootIdentity, Store, WorkspaceConflict,
    WorkspaceResult, WorkspaceRow, WorkspaceStoreError,
};

async fn select_record(
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

async fn select_workspace(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
) -> WorkspaceResult<WorkspaceRow> {
    let row = sqlx::query(
        "SELECT * FROM workspaces
         WHERE id = ? COLLATE BINARY AND root_generation = ? COLLATE BINARY LIMIT 1",
    )
    .bind(intent.workspace_id().as_str())
    .bind(intent.root_generation())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?
    .ok_or(WorkspaceStoreError::CorruptRow {
        table: "workspaces",
        field: "row",
    })?;
    WorkspaceRow::decode(&row)
}

fn matches_intent(record: &AllocationRecord, intent: &AllocationIntent) -> bool {
    record.id() == intent.allocation_id()
        && record.workspace_id() == intent.workspace_id()
        && record.root_generation() == intent.root_generation()
        && record.relative_name() == intent.relative_name()
        && record.parent_identity() == intent.parent_identity()
        && record.created_at() == intent.created_at()
}

struct RootColumns {
    kind: &'static str,
    unix_device: Option<Vec<u8>>,
    unix_inode: Option<Vec<u8>>,
    windows_volume: Option<Vec<u8>>,
    windows_file_id: Option<Vec<u8>>,
}

fn root_columns(identity: RootIdentity) -> RootColumns {
    match identity {
        RootIdentity::Unix { device, inode } => RootColumns {
            kind: "unix",
            unix_device: Some(device.to_vec()),
            unix_inode: Some(inode.to_vec()),
            windows_volume: None,
            windows_file_id: None,
        },
        RootIdentity::Windows {
            volume_serial,
            file_id,
        } => RootColumns {
            kind: "windows",
            unix_device: None,
            unix_inode: None,
            windows_volume: Some(volume_serial.to_vec()),
            windows_file_id: Some(file_id.to_vec()),
        },
    }
}

fn verify_materialized_after_write(
    record: &AllocationRecord,
    intent: &AllocationIntent,
    root_identity: RootIdentity,
) -> WorkspaceResult<()> {
    if matches_intent(record, intent)
        && record.phase() == AllocationPhase::Materialized
        && record.root_identity() == Some(root_identity)
        && record.failure_reason().is_none()
    {
        Ok(())
    } else {
        Err(WorkspaceStoreError::Database)
    }
}

fn verify_committed(
    workspace: &WorkspaceRow,
    intent: &AllocationIntent,
    root_identity: RootIdentity,
) -> WorkspaceResult<()> {
    if workspace.id == *intent.workspace_id()
        && workspace.root.root_generation() == intent.root_generation()
        && workspace.root.identity() == root_identity
    {
        Ok(())
    } else {
        Err(WorkspaceStoreError::CorruptRow {
            table: "workspaces",
            field: "row",
        })
    }
}

async fn legacy_workspace_exists(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
) -> WorkspaceResult<bool> {
    sqlx::query("SELECT 1 FROM workspaces WHERE id = ? COLLATE BINARY LIMIT 1")
        .bind(intent.workspace_id().as_str())
        .fetch_optional(&mut **tx)
        .await
        .map(|row| row.is_some())
        .map_err(|_| WorkspaceStoreError::Database)
}

async fn root_conflict(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
    identity: RootIdentity,
) -> WorkspaceResult<bool> {
    let allocation = root_columns(identity);
    let allocation = sqlx::query(
        "SELECT 1 FROM workspace_allocations
         WHERE allocation_id <> ? COLLATE BINARY AND root_identity_kind = ? COLLATE BINARY
           AND root_unix_device IS ? AND root_unix_inode IS ?
           AND root_windows_volume IS ? AND root_windows_file_id IS ? LIMIT 1",
    )
    .bind(intent.allocation_id().as_str())
    .bind(allocation.kind)
    .bind(allocation.unix_device)
    .bind(allocation.unix_inode)
    .bind(allocation.windows_volume)
    .bind(allocation.windows_file_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    if allocation.is_some() {
        return Ok(true);
    }

    let workspace = root_columns(identity);
    let workspace = sqlx::query(
        "SELECT 1 FROM workspaces
         WHERE root_identity_kind = ? COLLATE BINARY AND unix_device IS ? AND unix_inode IS ?
           AND windows_volume_serial IS ? AND windows_file_id IS ? LIMIT 1",
    )
    .bind(workspace.kind)
    .bind(workspace.unix_device)
    .bind(workspace.unix_inode)
    .bind(workspace.windows_volume)
    .bind(workspace.windows_file_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    Ok(workspace.is_some())
}

async fn check_exclusions(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
    identity: RootIdentity,
) -> WorkspaceResult<()> {
    if legacy_workspace_exists(tx, intent).await? {
        return Err(WorkspaceStoreError::Conflict(WorkspaceConflict::Identifier));
    }
    if root_conflict(tx, intent, identity).await? {
        return Err(WorkspaceStoreError::Conflict(
            WorkspaceConflict::RootIdentity,
        ));
    }
    Ok(())
}

impl Store {
    /// Materialize one reserved allocation journal row without touching a
    /// workspace registration or filesystem path. Errors are static/redacted;
    /// reconcile an uncertain commit with [`Store::get_workspace_allocation`]
    /// and retry the exact intent/root pair.
    pub async fn materialize_workspace_allocation(
        &self,
        intent: &AllocationIntent,
        root_identity: RootIdentity,
    ) -> WorkspaceResult<AllocationRecord> {
        let mut tx = self.begin_workspace_immediate().await?;
        let record = select_record(&mut tx, intent).await?;
        if !matches_intent(&record, intent) {
            return Err(WorkspaceStoreError::Conflict(
                WorkspaceConflict::AllocationIdentifier,
            ));
        }
        match record.phase() {
            AllocationPhase::Materialized => {
                if record.root_identity() != Some(root_identity) {
                    return Err(WorkspaceStoreError::Conflict(
                        WorkspaceConflict::RootIdentity,
                    ));
                }
                check_exclusions(&mut tx, intent, root_identity).await?;
                tx.commit()
                    .await
                    .map_err(|_| WorkspaceStoreError::Database)?;
                return Ok(record);
            }
            AllocationPhase::Committed => {
                if record.root_identity() != Some(root_identity) {
                    return Err(WorkspaceStoreError::Conflict(
                        WorkspaceConflict::RootIdentity,
                    ));
                }
                let workspace = select_workspace(&mut tx, intent).await?;
                verify_committed(&workspace, intent, root_identity)?;
                tx.commit()
                    .await
                    .map_err(|_| WorkspaceStoreError::Database)?;
                return Ok(record);
            }
            AllocationPhase::Retained => {
                return Err(WorkspaceStoreError::Conflict(
                    WorkspaceConflict::AllocationIdentifier,
                ));
            }
            AllocationPhase::Reserved => {}
        }

        check_exclusions(&mut tx, intent, root_identity).await?;
        let columns = root_columns(root_identity);
        let parent = root_columns(record.parent_identity());
        let update = sqlx::query(
            "UPDATE workspace_allocations
             SET root_identity_kind = ?, root_unix_device = ?, root_unix_inode = ?,
                 root_windows_volume = ?, root_windows_file_id = ?, phase = 'materialized'
             WHERE allocation_id = ? COLLATE BINARY AND phase = 'reserved' COLLATE BINARY
                   AND workspace_id = ? COLLATE BINARY
                   AND root_generation = ? COLLATE BINARY
                   AND relative_name = ? COLLATE BINARY
                   AND created_at = ? COLLATE BINARY
                   AND parent_identity_kind = ? COLLATE BINARY
                   AND parent_unix_device IS ? AND parent_unix_inode IS ?
                   AND parent_windows_volume IS ? AND parent_windows_file_id IS ?
                   AND root_identity_kind IS NULL AND root_unix_device IS NULL
                   AND root_unix_inode IS NULL AND root_windows_volume IS NULL
                   AND root_windows_file_id IS NULL AND failure_reason IS NULL",
        )
        .bind(columns.kind)
        .bind(columns.unix_device)
        .bind(columns.unix_inode)
        .bind(columns.windows_volume)
        .bind(columns.windows_file_id)
        .bind(intent.allocation_id().as_str())
        .bind(intent.workspace_id().as_str())
        .bind(intent.root_generation())
        .bind(intent.relative_name())
        .bind(intent.created_at().as_str())
        .bind(parent.kind)
        .bind(parent.unix_device)
        .bind(parent.unix_inode)
        .bind(parent.windows_volume)
        .bind(parent.windows_file_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
        if update.rows_affected() != 1 {
            return Err(WorkspaceStoreError::Database);
        }

        let record = select_record(&mut tx, intent)
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        verify_materialized_after_write(&record, intent, root_identity)?;
        let detail = json!({
            "allocation_id": record.id().as_str(),
            "workspace_id": record.workspace_id().as_str(),
            "phase": "materialized",
        });
        Store::append_audit_tx(
            &mut tx,
            record.created_at().as_str(),
            "workspace_allocation",
            "workspace.allocation_materialized",
            &detail,
        )
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;

        check_exclusions(&mut tx, intent, root_identity)
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        let record = select_record(&mut tx, intent)
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        verify_materialized_after_write(&record, intent, root_identity)?;
        tx.commit()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        Ok(record)
    }
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

    const AID: &str = "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    const WID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    const NOW: &str = "2026-09-19T00:00:00.000Z";

    fn intent(identity: RootIdentity) -> AllocationIntent {
        AllocationIntent::new(
            crate::AllocationId::parse(AID).unwrap(),
            WorkspaceId::parse(WID).unwrap(),
            "generation-1".to_owned(),
            "scratch".to_owned(),
            identity,
            crate::WorkspaceInstant::parse(NOW).unwrap(),
        )
        .unwrap()
    }

    async fn reserved(store: &Store, identity: RootIdentity) -> AllocationIntent {
        let intent = intent(identity);
        store.reserve_workspace_allocation(&intent).await.unwrap();
        intent
    }

    async fn exec(store: &Store, sql: &str) {
        sqlx::query(sql)
            .execute(crate::test_hooks::pool(store))
            .await
            .unwrap();
    }

    async fn committed_fixture(store: &Store, intent: &AllocationIntent, root: RootIdentity) {
        let owner = LifecycleOwnerRef::parse("materializer-test".to_owned()).unwrap();
        let workspace = NewScratchWorkspace::new(
            WorkspaceId::parse(WID).unwrap(),
            TrustedRootRegistration::new(
                "/registered/root".to_owned(),
                intent.root_generation().to_owned(),
                root,
            )
            .unwrap(),
            WorkspaceProvenanceInput::new("test".to_owned(), None, None).unwrap(),
            owner,
            WorkspaceTtl::new(1_000).unwrap(),
        );
        store
            .create_workspace(
                &workspace,
                workspace.lifecycle_owner_ref(),
                &WorkspaceInstant::parse(NOW).unwrap(),
            )
            .await
            .unwrap();
        exec(
            store,
            "UPDATE workspace_allocations
             SET root_identity_kind = 'unix', root_unix_device = X'0303030303030303',
                 root_unix_inode = X'0404040404040404', phase = 'committed'
             WHERE allocation_id = 'wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5'",
        )
        .await;
    }

    #[tokio::test]
    async fn materializes_both_identity_shapes_and_replays_without_audit() {
        for (identity, root) in [
            (
                RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
                RootIdentity::unix(&[3; 8], &[4; 8]).unwrap(),
            ),
            (
                RootIdentity::windows(&[5; 8], &[6; 16]).unwrap(),
                RootIdentity::windows(&[7; 8], &[8; 16]).unwrap(),
            ),
        ] {
            let store = Store::open_memory().await.unwrap();
            let intent = reserved(&store, identity).await;
            let record = store
                .materialize_workspace_allocation(&intent, root)
                .await
                .unwrap();
            assert_eq!(record.phase(), AllocationPhase::Materialized);
            store
                .materialize_workspace_allocation(&intent, root)
                .await
                .unwrap();
            assert_eq!(store.list_audit().await.unwrap().len(), 2);
        }
    }

    #[tokio::test]
    async fn committed_replay_requires_its_exact_registered_workspace() {
        let store = Store::open_memory().await.unwrap();
        let intent = reserved(&store, RootIdentity::unix(&[1; 8], &[2; 8]).unwrap()).await;
        let root = RootIdentity::unix(&[3; 8], &[4; 8]).unwrap();
        committed_fixture(&store, &intent, root).await;
        let record = store
            .materialize_workspace_allocation(&intent, root)
            .await
            .unwrap();
        assert_eq!(record.phase(), AllocationPhase::Committed);
        assert_eq!(store.list_audit().await.unwrap().len(), 1);
        assert!(matches!(
            store
                .materialize_workspace_allocation(
                    &intent,
                    RootIdentity::unix(&[9; 8], &[10; 8]).unwrap()
                )
                .await,
            Err(WorkspaceStoreError::Conflict(
                WorkspaceConflict::RootIdentity
            ))
        ));
    }

    #[tokio::test]
    async fn post_audit_conflicts_are_database_and_rollback() {
        let store = Store::open_memory().await.unwrap();
        let pool = crate::test_hooks::pool(&store);
        let intent = reserved(&store, RootIdentity::unix(&[1; 8], &[2; 8]).unwrap()).await;
        let root = RootIdentity::unix(&[3; 8], &[4; 8]).unwrap();
        committed_fixture(&store, &intent, root).await;
        exec(
            &store,
            "UPDATE workspace_allocations
             SET phase = 'reserved', root_identity_kind = NULL, root_unix_device = NULL,
                 root_unix_inode = NULL;
             UPDATE workspaces SET id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6',
                 unix_device = X'0909090909090909', unix_inode = X'0A0A0A0A0A0A0A0A'
             WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'",
        )
        .await;
        for (name, update) in [
            (
                "post_audit_workspace",
                "UPDATE workspaces SET id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'
                 WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6'",
            ),
            (
                "post_audit_root",
                "UPDATE workspaces SET unix_device = X'0303030303030303',
                    unix_inode = X'0404040404040404'
                 WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6'",
            ),
        ] {
            sqlx::query(&format!(
                "CREATE TRIGGER {name} AFTER INSERT ON audit_log
                 WHEN NEW.action = 'workspace.allocation_materialized'
                 BEGIN {update}; END"
            ))
            .execute(pool)
            .await
            .unwrap();
            assert!(matches!(
                store.materialize_workspace_allocation(&intent, root).await,
                Err(WorkspaceStoreError::Database)
            ));
            assert_eq!(
                store
                    .get_workspace_allocation(intent.allocation_id())
                    .await
                    .unwrap()
                    .phase(),
                AllocationPhase::Reserved
            );
            assert_eq!(store.list_audit().await.unwrap().len(), 1);
            sqlx::query(&format!("DROP TRIGGER {name}"))
                .execute(pool)
                .await
                .unwrap();
        }
        store
            .materialize_workspace_allocation(&intent, root)
            .await
            .unwrap();
        assert_eq!(store.list_audit().await.unwrap().len(), 2);
    }
}
