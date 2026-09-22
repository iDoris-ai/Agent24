use serde_json::json;
use sqlx::{Sqlite, Transaction};

use crate::{
    AllocationIntent, AllocationPhase, AllocationRecord, RootIdentity, Store, WorkspaceConflict,
    WorkspaceResult, WorkspaceStoreError,
};

async fn select_reserved(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
) -> WorkspaceResult<AllocationRecord> {
    let row = sqlx::query(
        "SELECT * FROM workspace_allocations
         WHERE allocation_id = ? COLLATE BINARY LIMIT 1",
    )
    .bind(intent.allocation_id().as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    let record = AllocationRecord::decode(&row)?;
    let exact = record.id() == intent.allocation_id()
        && record.workspace_id() == intent.workspace_id()
        && record.root_generation() == intent.root_generation()
        && record.relative_name() == intent.relative_name()
        && record.parent_identity() == intent.parent_identity()
        && record.created_at() == intent.created_at()
        && record.phase() == AllocationPhase::Reserved
        && record.root_identity().is_none()
        && record.failure_reason().is_none();
    exact
        .then_some(record)
        .ok_or(WorkspaceStoreError::CorruptRow {
            table: "workspace_allocations",
            field: "row",
        })
}

impl Store {
    /// Atomically persist a validated allocation intent and its audit evidence.
    ///
    /// This is intentionally crate-private: filesystem materialization and the
    /// public workspace service are later slices of the allocation protocol.
    pub(crate) async fn reserve_workspace_allocation(
        &self,
        intent: &AllocationIntent,
    ) -> WorkspaceResult<AllocationRecord> {
        let mut tx = self.begin_workspace_immediate().await?;
        let conflict: i64 = sqlx::query_scalar(
            "SELECT CASE
             WHEN EXISTS(SELECT 1 FROM workspace_allocations WHERE allocation_id = ? COLLATE BINARY) THEN 1
             WHEN EXISTS(SELECT 1 FROM workspace_allocations WHERE workspace_id = ? COLLATE BINARY) THEN 2
             WHEN EXISTS(SELECT 1 FROM workspace_allocations WHERE relative_name = ? COLLATE BINARY) THEN 3
             WHEN EXISTS(SELECT 1 FROM workspaces WHERE id = ? COLLATE BINARY) THEN 4 ELSE 0 END",
        )
        .bind(intent.allocation_id().as_str())
        .bind(intent.workspace_id().as_str())
        .bind(intent.relative_name())
        .bind(intent.workspace_id().as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
        let conflict = match conflict {
            0 => None,
            1 => Some(WorkspaceConflict::AllocationIdentifier),
            2 => Some(WorkspaceConflict::AllocationWorkspace),
            3 => Some(WorkspaceConflict::AllocationRelativeName),
            4 => Some(WorkspaceConflict::Identifier),
            _ => return Err(WorkspaceStoreError::Database),
        };
        if let Some(conflict) = conflict {
            return Err(WorkspaceStoreError::Conflict(conflict));
        }

        let (kind, unix_device, unix_inode, windows_volume, windows_file_id) =
            match intent.parent_identity() {
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
            };
        sqlx::query(
            "INSERT INTO workspace_allocations
             (allocation_id, workspace_id, root_generation, relative_name,
              parent_identity_kind, parent_unix_device, parent_unix_inode,
              parent_windows_volume, parent_windows_file_id, root_identity_kind,
              root_unix_device, root_unix_inode, root_windows_volume,
              root_windows_file_id, phase, created_at, failure_reason)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL,
                     'reserved', ?, NULL)",
        )
        .bind(intent.allocation_id().as_str())
        .bind(intent.workspace_id().as_str())
        .bind(intent.root_generation())
        .bind(intent.relative_name())
        .bind(kind)
        .bind(unix_device)
        .bind(unix_inode)
        .bind(windows_volume)
        .bind(windows_file_id)
        .bind(intent.created_at().as_str())
        .execute(&mut *tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;

        let record = select_reserved(&mut tx, intent).await?;
        let detail = json!({
            "allocation_id": record.id().as_str(),
            "workspace_id": record.workspace_id().as_str(),
            "phase": "reserved",
        });
        Store::append_audit_tx(
            &mut tx,
            record.created_at().as_str(),
            "workspace_allocation",
            "workspace.allocation_reserved",
            &detail,
        )
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
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
    use agent24_protocol::WorkspaceId;

    #[tokio::test]
    async fn reserves_both_parent_identities_with_audit() {
        let store = Store::open_memory().await.unwrap();
        for (aid, wid, name, identity) in [
            (
                "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
                "unix",
                RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
            ),
            (
                "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
                "windows",
                RootIdentity::windows(&[3; 8], &[4; 16]).unwrap(),
            ),
        ] {
            let intent = AllocationIntent::new(
                crate::AllocationId::parse(aid).unwrap(),
                WorkspaceId::parse(wid).unwrap(),
                "generation-1".into(),
                name.into(),
                identity,
                crate::WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap(),
            )
            .unwrap();
            let record = store.reserve_workspace_allocation(&intent).await.unwrap();
            assert_eq!(record.parent_identity(), identity);
            assert_eq!(record.phase(), AllocationPhase::Reserved);
            assert!(record.root_identity().is_none() && record.failure_reason().is_none());
        }
        let audit = store.list_audit().await.unwrap();
        assert_eq!(audit.len(), 2);
        assert!(
            audit
                .iter()
                .all(|entry| entry.action == "workspace.allocation_reserved")
        );
        store.verify_audit_chain().await.unwrap();
    }
}
