use agent24_protocol::{Workspace, WorkspaceId};
use sqlx::{Sqlite, Transaction};

use crate::{AllocationId, AllocationRecord};
use crate::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, WorkspaceInstant,
    WorkspaceListCursor, WorkspaceListQuery, WorkspacePage, WorkspaceResult, WorkspaceRow,
    WorkspaceStoreError,
};

const LIST_ALL: &str = "SELECT * FROM workspaces
                        ORDER BY created_at COLLATE BINARY DESC, id COLLATE BINARY DESC
                        LIMIT ?";
const LIST_STATE: &str = "SELECT * FROM workspaces
                          WHERE state COLLATE BINARY = ? COLLATE BINARY
                          ORDER BY created_at COLLATE BINARY DESC, id COLLATE BINARY DESC
                          LIMIT ?";
const LIST_CURSOR: &str = "SELECT * FROM workspaces
                           WHERE (created_at COLLATE BINARY < ? COLLATE BINARY
                                  OR (created_at COLLATE BINARY = ? COLLATE BINARY
                                      AND id COLLATE BINARY < ? COLLATE BINARY))
                           ORDER BY created_at COLLATE BINARY DESC, id COLLATE BINARY DESC
                           LIMIT ?";
const LIST_STATE_CURSOR: &str = "SELECT * FROM workspaces
                                 WHERE state COLLATE BINARY = ? COLLATE BINARY
                                   AND (created_at COLLATE BINARY < ? COLLATE BINARY
                                        OR (created_at COLLATE BINARY = ? COLLATE BINARY
                                            AND id COLLATE BINARY < ? COLLATE BINARY))
                                 ORDER BY created_at COLLATE BINARY DESC, id COLLATE BINARY DESC
                                 LIMIT ?";

fn decode_row(row: &sqlx::sqlite::SqliteRow) -> WorkspaceResult<Workspace> {
    WorkspaceRow::decode(row)
        .map(|workspace| workspace.project())
        .map_err(|error| match error {
            WorkspaceStoreError::CorruptRow { .. } => error,
            _ => WorkspaceStoreError::CorruptRow {
                table: "workspaces",
                field: "row",
            },
        })
}

async fn exists(
    tx: &mut Transaction<'_, Sqlite>,
    query: &str,
    value: &str,
) -> WorkspaceResult<bool> {
    sqlx::query(query)
        .bind(value)
        .fetch_optional(&mut **tx)
        .await
        .map(|row| row.is_some())
        .map_err(|_| WorkspaceStoreError::Database)
}

async fn identity_exists(
    tx: &mut Transaction<'_, Sqlite>,
    identity: RootIdentity,
) -> WorkspaceResult<bool> {
    let row = match identity {
        RootIdentity::Unix { device, inode } => {
            sqlx::query(
                "SELECT 1 FROM workspaces
                 WHERE root_identity_kind = 'unix' AND unix_device = ? AND unix_inode = ?
                 LIMIT 1",
            )
            .bind(device.to_vec())
            .bind(inode.to_vec())
            .fetch_optional(&mut **tx)
            .await
        }
        RootIdentity::Windows {
            volume_serial,
            file_id,
        } => {
            sqlx::query(
                "SELECT 1 FROM workspaces
                 WHERE root_identity_kind = 'windows'
                   AND windows_volume_serial = ? AND windows_file_id = ?
                 LIMIT 1",
            )
            .bind(volume_serial.to_vec())
            .bind(file_id.to_vec())
            .fetch_optional(&mut **tx)
            .await
        }
    };
    row.map(|value| value.is_some())
        .map_err(|_| WorkspaceStoreError::Database)
}

async fn allocation_workspace_exists(
    tx: &mut Transaction<'_, Sqlite>,
    workspace_id: &str,
) -> WorkspaceResult<bool> {
    exists(
        tx,
        "SELECT 1 FROM workspace_allocations
         WHERE phase IN ('materialized', 'committed', 'retained')
           AND workspace_id = ? COLLATE BINARY
         LIMIT 1",
        workspace_id,
    )
    .await
}

async fn allocation_identity_exists(
    tx: &mut Transaction<'_, Sqlite>,
    identity: RootIdentity,
) -> WorkspaceResult<bool> {
    let row = match identity {
        RootIdentity::Unix { device, inode } => {
            sqlx::query(
                "SELECT 1 FROM workspace_allocations
                 WHERE phase IN ('materialized', 'committed', 'retained')
                   AND root_identity_kind = 'unix'
                   AND root_unix_device = ? AND root_unix_inode = ?
                 LIMIT 1",
            )
            .bind(device.to_vec())
            .bind(inode.to_vec())
            .fetch_optional(&mut **tx)
            .await
        }
        RootIdentity::Windows {
            volume_serial,
            file_id,
        } => {
            sqlx::query(
                "SELECT 1 FROM workspace_allocations
                 WHERE phase IN ('materialized', 'committed', 'retained')
                   AND root_identity_kind = 'windows'
                   AND root_windows_volume = ? AND root_windows_file_id = ?
                 LIMIT 1",
            )
            .bind(volume_serial.to_vec())
            .bind(file_id.to_vec())
            .fetch_optional(&mut **tx)
            .await
        }
    };
    row.map(|value| value.is_some())
        .map_err(|_| WorkspaceStoreError::Database)
}

async fn insert_workspace(
    tx: &mut Transaction<'_, Sqlite>,
    input: &NewScratchWorkspace,
    now: &WorkspaceInstant,
    expires_at: &WorkspaceInstant,
) -> WorkspaceResult<()> {
    let (identity_kind, unix_device, unix_inode, windows_volume_serial, windows_file_id) =
        match input.root().identity() {
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
        "INSERT INTO workspaces
         (id, kind, state, provenance_source, provenance_project_ref,
          provenance_base_revision, writeback_policy, lifecycle_owner_kind,
          lifecycle_owner_ref, concurrency_policy, created_at, expires_at,
          renewed_at, released_at, revision, canonical_root, root_generation,
          root_identity_kind, unix_device, unix_inode, windows_volume_serial,
          windows_file_id, quarantine_root, quarantined_at, cleanup_attempts,
          cleanup_last_attempt_at, cleanup_error, cleanup_retry_at)
         VALUES (?, 'orchestrator_scratch', 'active', ?, ?, ?, 'external',
                 'orchestrator', ?, 'serial', ?, ?, NULL, NULL, 1, ?, ?, ?,
                 ?, ?, ?, ?, NULL, NULL, 0, NULL, NULL, NULL)",
    )
    .bind(input.id().as_str())
    .bind(input.provenance().source())
    .bind(input.provenance().project_ref())
    .bind(input.provenance().base_revision())
    .bind(input.lifecycle_owner_ref().as_str())
    .bind(now.as_str())
    .bind(expires_at.as_str())
    .bind(input.root().canonical_root())
    .bind(input.root().root_generation())
    .bind(identity_kind)
    .bind(unix_device)
    .bind(unix_inode)
    .bind(windows_volume_serial)
    .bind(windows_file_id)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(|_| WorkspaceStoreError::Database)
}

impl Store {
    /// Read one allocation journal row without mutating any store table.
    pub async fn get_workspace_allocation(
        &self,
        id: &AllocationId,
    ) -> WorkspaceResult<AllocationRecord> {
        let row = sqlx::query(
            "SELECT * FROM workspace_allocations
             WHERE allocation_id = ? COLLATE BINARY LIMIT 1",
        )
        .bind(id.as_str())
        .fetch_optional(self.pool())
        .await
        .map_err(|_| WorkspaceStoreError::Database)?
        .ok_or(WorkspaceStoreError::NotFound)?;
        AllocationRecord::decode(&row)
    }

    /// Read one page directly from the pool without applying lifecycle policy.
    pub async fn list_workspaces(
        &self,
        query: &WorkspaceListQuery,
    ) -> WorkspaceResult<WorkspacePage> {
        let fetch_limit = i64::from(query.limit().value()) + 1;
        let statement = match (query.state(), query.after()) {
            (None, None) => sqlx::query(LIST_ALL),
            (Some(state), None) => sqlx::query(LIST_STATE).bind(state.as_str()),
            (None, Some(after)) => sqlx::query(LIST_CURSOR)
                .bind(after.created_at().as_str())
                .bind(after.created_at().as_str())
                .bind(after.id().as_str()),
            (Some(state), Some(after)) => sqlx::query(LIST_STATE_CURSOR)
                .bind(state.as_str())
                .bind(after.created_at().as_str())
                .bind(after.created_at().as_str())
                .bind(after.id().as_str()),
        };
        let rows = statement
            .bind(fetch_limit)
            .fetch_all(self.pool())
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        let rows = rows
            .iter()
            .map(WorkspaceRow::decode)
            .collect::<WorkspaceResult<Vec<_>>>()?;
        let limit = usize::from(query.limit().value());
        let next_cursor = (rows.len() > limit).then(|| {
            let row = &rows[limit - 1];
            WorkspaceListCursor::new(row.created_at.clone(), row.id.clone())
        });
        let items = rows
            .into_iter()
            .take(limit)
            .map(|row| row.project())
            .collect();
        Ok(WorkspacePage::new(items, next_cursor))
    }

    /// Read one workspace directly from the pool without applying lifecycle policy.
    pub async fn get_workspace(&self, id: &WorkspaceId) -> WorkspaceResult<Workspace> {
        let row = sqlx::query("SELECT * FROM workspaces WHERE id = ? COLLATE BINARY LIMIT 1")
            .bind(id.as_str())
            .fetch_optional(self.pool())
            .await
            .map_err(|_| WorkspaceStoreError::Database)?
            .ok_or(WorkspaceStoreError::NotFound)?;
        decode_row(&row)
    }

    /// Create an orchestrator-owned scratch workspace under one SQLite write lock.
    pub async fn create_workspace(
        &self,
        input: &NewScratchWorkspace,
        authorized_owner: &LifecycleOwnerRef,
        now: &WorkspaceInstant,
    ) -> WorkspaceResult<Workspace> {
        if input.lifecycle_owner_ref() != authorized_owner {
            return Err(WorkspaceStoreError::InvalidValue {
                field: "lifecycle_owner_ref",
            });
        }
        let expires_at = now.checked_add_workspace_ttl(input.ttl())?;
        let mut tx = self.begin_workspace_immediate().await?;

        if exists(
            &mut tx,
            "SELECT 1 FROM workspaces WHERE id = ? LIMIT 1",
            input.id().as_str(),
        )
        .await?
        {
            return Err(WorkspaceStoreError::Conflict(
                crate::WorkspaceConflict::Identifier,
            ));
        }
        if allocation_workspace_exists(&mut tx, input.id().as_str()).await? {
            return Err(WorkspaceStoreError::Conflict(
                crate::WorkspaceConflict::Identifier,
            ));
        }
        if exists(
            &mut tx,
            "SELECT 1 FROM workspaces WHERE canonical_root = ? COLLATE BINARY LIMIT 1",
            input.root().canonical_root(),
        )
        .await?
        {
            return Err(WorkspaceStoreError::Conflict(
                crate::WorkspaceConflict::CanonicalRoot,
            ));
        }
        if identity_exists(&mut tx, input.root().identity()).await? {
            return Err(WorkspaceStoreError::Conflict(
                crate::WorkspaceConflict::RootIdentity,
            ));
        }
        if allocation_identity_exists(&mut tx, input.root().identity()).await? {
            return Err(WorkspaceStoreError::Conflict(
                crate::WorkspaceConflict::RootIdentity,
            ));
        }

        insert_workspace(&mut tx, input, now, &expires_at).await?;
        let row = sqlx::query("SELECT * FROM workspaces WHERE id = ?")
            .bind(input.id().as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        let workspace_row = WorkspaceRow::decode(&row)?;
        let workspace_identity = workspace_row.root.identity();
        let workspace = workspace_row.project();

        // Keep the allocation journal and legacy registry mutually exclusive
        // even when a trigger or a future write path mutates the journal after
        // the workspace INSERT. Reserved rows intentionally remain compatible.
        if allocation_workspace_exists(&mut tx, input.id().as_str()).await? {
            return Err(WorkspaceStoreError::Conflict(
                crate::WorkspaceConflict::Identifier,
            ));
        }
        if allocation_identity_exists(&mut tx, workspace_identity).await? {
            return Err(WorkspaceStoreError::Conflict(
                crate::WorkspaceConflict::RootIdentity,
            ));
        }
        tx.commit()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        Ok(workspace)
    }
}
