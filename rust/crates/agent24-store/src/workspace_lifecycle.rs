use agent24_protocol::{Workspace, WorkspaceId};
use serde_json::json;
use sqlx::{Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    LifecycleOwnerRef, Store, WorkspaceInstant, WorkspaceKind, WorkspaceResult, WorkspaceRow,
    WorkspaceState, WorkspaceStoreError, WorkspaceTtl,
};

const AUDIT_ACTOR: &str = "workspace_lifecycle";

fn decode_lifecycle_row(row: &SqliteRow) -> WorkspaceResult<WorkspaceRow> {
    let workspace = WorkspaceRow::decode(row)?;
    if workspace.kind == WorkspaceKind::LegacyCompat {
        return Err(WorkspaceStoreError::InvalidValue {
            field: "workspace_kind",
        });
    }
    Ok(workspace)
}

async fn select_workspace(
    tx: &mut Transaction<'_, Sqlite>,
    id: &WorkspaceId,
) -> WorkspaceResult<WorkspaceRow> {
    let row = sqlx::query("SELECT * FROM workspaces WHERE id = ? COLLATE BINARY LIMIT 1")
        .bind(id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?
        .ok_or(WorkspaceStoreError::NotFound)?;
    decode_lifecycle_row(&row)
}

fn next_revision(revision: u64) -> WorkspaceResult<u64> {
    revision.checked_add(1).ok_or(WorkspaceStoreError::Database)
}

async fn commit_unchanged(
    tx: Transaction<'_, Sqlite>,
    workspace: WorkspaceRow,
) -> WorkspaceResult<Workspace> {
    tx.commit()
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
    Ok(workspace.project())
}

async fn expire_workspace_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &WorkspaceId,
    now: &WorkspaceInstant,
    workspace: &WorkspaceRow,
) -> WorkspaceResult<WorkspaceRow> {
    let revision = next_revision(workspace.revision)?;
    let affected = sqlx::query(
        "UPDATE workspaces SET state = 'expired', revision = ?
         WHERE id = ? COLLATE BINARY AND state = 'active'
           AND expires_at = ? COLLATE BINARY AND expires_at <= ? COLLATE BINARY
           AND revision = ? AND root_generation = ? COLLATE BINARY",
    )
    .bind(i64::try_from(revision).map_err(|_| WorkspaceStoreError::Database)?)
    .bind(id.as_str())
    .bind(workspace.expires_at.as_str())
    .bind(now.as_str())
    .bind(i64::try_from(workspace.revision).map_err(|_| WorkspaceStoreError::Database)?)
    .bind(workspace.root.root_generation())
    .execute(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    if affected.rows_affected() != 1 {
        return Err(WorkspaceStoreError::Database);
    }

    let mut expected = workspace.clone();
    expected.state = WorkspaceState::Expired;
    expected.cleanup.state = WorkspaceState::Expired;
    expected.revision = revision;
    let reselected = select_workspace(tx, id).await?;
    if reselected != expected {
        return Err(WorkspaceStoreError::CorruptRow {
            table: "workspaces",
            field: "row",
        });
    }
    let detail = json!({
        "id": reselected.id.as_str(),
        "kind": reselected.kind.as_str(),
        "result_state": reselected.state.as_str(),
        "reason": "ttl",
    });
    Store::append_audit_tx(tx, now.as_str(), AUDIT_ACTOR, "workspace.expired", &detail)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
    Ok(reselected)
}

impl Store {
    /// Mark an active workspace expired once its persisted TTL has elapsed.
    pub async fn expire_workspace(
        &self,
        id: &WorkspaceId,
        now: &WorkspaceInstant,
    ) -> WorkspaceResult<Workspace> {
        let mut tx = self.begin_workspace_immediate().await?;
        let workspace = select_workspace(&mut tx, id).await?;
        if workspace.state != WorkspaceState::Active || now < &workspace.expires_at {
            return commit_unchanged(tx, workspace).await;
        }
        let reselected = expire_workspace_tx(&mut tx, id, now, &workspace).await?;
        tx.commit()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        Ok(reselected.project())
    }

    /// Request release of an active or expired workspace for its exact owner.
    pub async fn release_workspace(
        &self,
        id: &WorkspaceId,
        authorized_owner: &LifecycleOwnerRef,
        now: &WorkspaceInstant,
    ) -> WorkspaceResult<Workspace> {
        let mut tx = self.begin_workspace_immediate().await?;
        let workspace = select_workspace(&mut tx, id).await?;
        if workspace.authority.lifecycle_owner_ref != authorized_owner.as_str() {
            return Err(WorkspaceStoreError::InvalidValue {
                field: "lifecycle_owner_ref",
            });
        }
        if !matches!(
            workspace.state,
            WorkspaceState::Active | WorkspaceState::Expired
        ) {
            return commit_unchanged(tx, workspace).await;
        }

        let revision = next_revision(workspace.revision)?;
        let affected = sqlx::query(
            "UPDATE workspaces SET state = 'releasing', revision = ?
             WHERE id = ? COLLATE BINARY AND (state = 'active' OR state = 'expired')
               AND lifecycle_owner_ref = ? COLLATE BINARY AND revision = ?
               AND root_generation = ? COLLATE BINARY",
        )
        .bind(i64::try_from(revision).map_err(|_| WorkspaceStoreError::Database)?)
        .bind(id.as_str())
        .bind(authorized_owner.as_str())
        .bind(i64::try_from(workspace.revision).map_err(|_| WorkspaceStoreError::Database)?)
        .bind(workspace.root.root_generation())
        .execute(&mut *tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
        if affected.rows_affected() != 1 {
            return Err(WorkspaceStoreError::Database);
        }

        let mut expected = workspace.clone();
        expected.state = WorkspaceState::Releasing;
        expected.cleanup.state = WorkspaceState::Releasing;
        expected.revision = revision;
        let reselected = select_workspace(&mut tx, id).await?;
        if reselected != expected {
            return Err(WorkspaceStoreError::CorruptRow {
                table: "workspaces",
                field: "row",
            });
        }
        let detail = json!({
            "id": reselected.id.as_str(),
            "kind": reselected.kind.as_str(),
            "result_state": reselected.state.as_str(),
            "owner_ref": authorized_owner.as_str(),
            "reason": "owner_requested",
        });
        Store::append_audit_tx(
            &mut tx,
            now.as_str(),
            AUDIT_ACTOR,
            "workspace.release_requested",
            &detail,
        )
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
        tx.commit()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        Ok(reselected.project())
    }

    /// Extend an active workspace's expiry for its exact lifecycle owner.
    pub async fn renew_workspace(
        &self,
        id: &WorkspaceId,
        authorized_owner: &LifecycleOwnerRef,
        ttl: WorkspaceTtl,
        now: &WorkspaceInstant,
    ) -> WorkspaceResult<Workspace> {
        let mut tx = self.begin_workspace_immediate().await?;
        let workspace = select_workspace(&mut tx, id).await?;

        if workspace.authority.lifecycle_owner_ref != authorized_owner.as_str() {
            return Err(WorkspaceStoreError::InvalidValue {
                field: "lifecycle_owner_ref",
            });
        }
        if workspace.kind != WorkspaceKind::OrchestratorScratch {
            return Err(WorkspaceStoreError::InvalidValue {
                field: "workspace_kind",
            });
        }

        if workspace.state == WorkspaceState::Active && now >= &workspace.expires_at {
            expire_workspace_tx(&mut tx, id, now, &workspace).await?;
            tx.commit()
                .await
                .map_err(|_| WorkspaceStoreError::Database)?;
            return Err(WorkspaceStoreError::InvalidValue {
                field: "workspace_state",
            });
        }

        if workspace.state != WorkspaceState::Active {
            return Err(WorkspaceStoreError::InvalidValue {
                field: "workspace_state",
            });
        }

        let renewal_anchor = workspace
            .renewed_at
            .as_ref()
            .unwrap_or(&workspace.created_at);
        if now < renewal_anchor {
            return Err(WorkspaceStoreError::InvalidValue {
                field: "renewed_at",
            });
        }

        let proposed_expiry = now.checked_add_workspace_ttl(ttl)?;

        if workspace.renewed_at.as_ref() == Some(now) && workspace.expires_at == proposed_expiry {
            return commit_unchanged(tx, workspace).await;
        }
        if proposed_expiry <= workspace.expires_at {
            return Err(WorkspaceStoreError::InvalidValue {
                field: "expires_at",
            });
        }

        let revision = next_revision(workspace.revision)?;
        let affected = sqlx::query(
            "UPDATE workspaces SET renewed_at = ?, expires_at = ?, revision = ?
             WHERE id = ? COLLATE BINARY AND kind = ? COLLATE BINARY
               AND created_at = ? COLLATE BINARY AND state = 'active'
               AND lifecycle_owner_ref = ? COLLATE BINARY
               AND expires_at = ? COLLATE BINARY
               AND expires_at > ? COLLATE BINARY AND expires_at < ? COLLATE BINARY
               AND renewed_at IS ?
               AND revision = ? AND root_generation = ? COLLATE BINARY",
        )
        .bind(now.as_str())
        .bind(proposed_expiry.as_str())
        .bind(i64::try_from(revision).map_err(|_| WorkspaceStoreError::Database)?)
        .bind(id.as_str())
        .bind(workspace.kind.as_str())
        .bind(workspace.created_at.as_str())
        .bind(authorized_owner.as_str())
        .bind(workspace.expires_at.as_str())
        .bind(now.as_str())
        .bind(proposed_expiry.as_str())
        .bind(workspace.renewed_at.as_ref().map(WorkspaceInstant::as_str))
        .bind(i64::try_from(workspace.revision).map_err(|_| WorkspaceStoreError::Database)?)
        .bind(workspace.root.root_generation())
        .execute(&mut *tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
        if affected.rows_affected() != 1 {
            return Err(WorkspaceStoreError::Database);
        }

        let mut expected = workspace.clone();
        expected.renewed_at = Some(now.clone());
        expected.expires_at = proposed_expiry.clone();
        expected.ttl = ttl;
        expected.revision = revision;
        let reselected = select_workspace(&mut tx, id).await?;
        if reselected != expected {
            return Err(WorkspaceStoreError::CorruptRow {
                table: "workspaces",
                field: "row",
            });
        }
        let detail = json!({
            "id": reselected.id.as_str(),
            "kind": reselected.kind.as_str(),
            "result_state": reselected.state.as_str(),
            "owner_ref": authorized_owner.as_str(),
            "reason": "owner_requested",
        });
        Store::append_audit_tx(
            &mut tx,
            now.as_str(),
            AUDIT_ACTOR,
            "workspace.renewed",
            &detail,
        )
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
        tx.commit()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        Ok(reselected.project())
    }
}
