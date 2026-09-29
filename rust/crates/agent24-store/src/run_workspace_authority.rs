use agent24_protocol::{RunStatus, WorkspaceId};

use crate::{
    AllocationPhase, AllocationRecord, LeaseKind, RootIdentity, Store, WorkspaceInstant,
    WorkspaceKind, WorkspaceLeaseId, WorkspaceLeaseRow, WorkspaceResult, WorkspaceRootSnapshot,
    WorkspaceRow, WorkspaceState, WorkspaceStoreError, repo::row_to_run,
};

/// Point-in-time persistence evidence for later workspace authority minting.
#[doc(hidden)]
#[derive(Clone, PartialEq, Eq)]
pub struct RunWorkspaceAuthoritySnapshot {
    root: WorkspaceRootSnapshot,
}

impl RunWorkspaceAuthoritySnapshot {
    pub fn workspace_id(&self) -> &WorkspaceId {
        self.root.workspace_id()
    }

    pub fn root_generation(&self) -> &str {
        self.root.root().root_generation()
    }

    pub fn relative_name(&self) -> &str {
        self.root.relative_name()
    }

    pub fn parent_identity(&self) -> RootIdentity {
        self.root.parent_identity()
    }

    pub fn root_identity(&self) -> RootIdentity {
        self.root.root().identity()
    }

    pub fn canonical_root_matches(&self, candidate: &str) -> bool {
        self.root.root().canonical_root() == candidate
    }
}

fn corrupt(table: &'static str, field: &'static str) -> WorkspaceStoreError {
    WorkspaceStoreError::CorruptRow { table, field }
}

impl Store {
    /// Read the complete persisted authority evidence for one live workspace run
    /// from one SQLite snapshot. No authority is minted here.
    #[doc(hidden)]
    pub async fn run_workspace_authority_snapshot(
        &self,
        run_id: &str,
        lease_id: &WorkspaceLeaseId,
        now: &WorkspaceInstant,
    ) -> WorkspaceResult<RunWorkspaceAuthoritySnapshot> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        let row = sqlx::query("SELECT * FROM runs WHERE id=? COLLATE BINARY LIMIT 1")
            .bind(run_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| WorkspaceStoreError::Database)?
            .ok_or(WorkspaceStoreError::NotFound)?;
        let run = row_to_run(&row).map_err(|_| corrupt("runs", "row"))?;
        let workspace_id = run
            .workspace_id
            .as_ref()
            .filter(|id| run.input.workspace_id.as_ref() == Some(*id))
            .ok_or_else(|| corrupt("runs", "workspace_id"))?;
        if !matches!(
            run.status,
            RunStatus::Queued | RunStatus::Running | RunStatus::AwaitingApproval
        ) || run.ended_at.is_some()
        {
            return Err(corrupt("runs", "status"));
        }
        let session_id = run
            .session_id
            .as_deref()
            .ok_or_else(|| corrupt("runs", "session_id"))?;
        let session_workspace = sqlx::query_scalar::<_, Option<String>>(
            "SELECT workspace_id FROM sessions WHERE id=? COLLATE BINARY LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?
        .ok_or_else(|| corrupt("sessions", "id"))?
        .ok_or_else(|| corrupt("sessions", "workspace_id"))?;
        if WorkspaceId::parse(&session_workspace)
            .map_err(|_| corrupt("sessions", "workspace_id"))?
            != *workspace_id
        {
            return Err(corrupt("sessions", "workspace_id"));
        }
        let created_at =
            WorkspaceInstant::parse(&run.created_at).map_err(|_| corrupt("runs", "created_at"))?;
        if now < &created_at {
            return Err(corrupt("runs", "created_at"));
        }

        let lease_rows = sqlx::query("SELECT * FROM workspace_leases WHERE owner_id=? COLLATE BINARY AND kind='run' COLLATE BINARY ORDER BY lease_id COLLATE BINARY")
            .bind(run_id).fetch_all(&mut *tx).await.map_err(|_| WorkspaceStoreError::Database)?;
        let leases = lease_rows
            .iter()
            .map(WorkspaceLeaseRow::decode)
            .collect::<WorkspaceResult<Vec<_>>>()?;
        if leases.len() != 1 {
            return Err(corrupt("workspace_leases", "row"));
        }
        let lease = &leases[0].record;
        if &lease.id != lease_id
            || lease.released_at.is_some()
            || lease.kind != LeaseKind::Run
            || lease.owner_id != run.id
            || lease.workspace_id != *workspace_id
            || lease.acquired_at < created_at
            || now < &lease.acquired_at
        {
            return Err(corrupt("workspace_leases", "row"));
        }

        let workspace_row =
            sqlx::query("SELECT * FROM workspaces WHERE id=? COLLATE BINARY LIMIT 1")
                .bind(workspace_id.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| WorkspaceStoreError::Database)?
                .ok_or_else(|| corrupt("workspaces", "id"))?;
        let workspace = WorkspaceRow::decode(&workspace_row)?;
        if workspace.kind != WorkspaceKind::OrchestratorScratch
            || workspace.state != WorkspaceState::Active
            || now < &workspace.created_at
            || workspace.renewed_at.as_ref().is_some_and(|at| now < at)
            || now >= &workspace.expires_at
            || lease.root_generation != workspace.root.root_generation()
        {
            return Err(corrupt("workspaces", "authority"));
        }

        let allocation_row = sqlx::query(
            "SELECT * FROM workspace_allocations WHERE workspace_id=? COLLATE BINARY LIMIT 1",
        )
        .bind(workspace_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?
        .ok_or_else(|| corrupt("workspace_allocations", "workspace_id"))?;
        let allocation = AllocationRecord::decode(&allocation_row)?;
        if allocation.phase() != AllocationPhase::Committed
            || allocation.workspace_id() != workspace_id
            || allocation.root_generation() != workspace.root.root_generation()
            || allocation.root_identity() != Some(workspace.root.identity())
        {
            return Err(corrupt("workspace_allocations", "root_binding"));
        }

        let snapshot = RunWorkspaceAuthoritySnapshot {
            root: WorkspaceRootSnapshot::new(
                workspace_id.clone(),
                allocation.relative_name().to_owned(),
                allocation.parent_identity(),
                workspace.root.clone(),
            ),
        };
        tx.commit()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    const WS: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    const WS2: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X7";
    const LEASE: &str = "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
    const OTHER: &str = "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X8";
    const TS: &str = "2026-09-19T00:00:00.000Z";
    const NOW: &str = "2026-09-19T00:00:01.000Z";
    async fn seed() -> Store {
        let s = Store::open_memory().await.unwrap();
        sqlx::raw_sql(
            r#"
            INSERT INTO workspaces
                (id, kind, state, provenance_source, writeback_policy, lifecycle_owner_kind,
                 lifecycle_owner_ref, concurrency_policy, created_at, expires_at, revision,
                 canonical_root, root_generation, root_identity_kind, unix_device, unix_inode)
            VALUES
                ('ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5', 'orchestrator_scratch', 'active', 'test',
                 'external', 'orchestrator', 'owner', 'serial', '2026-09-19T00:00:00.000Z',
                 '2026-09-19T00:01:00.000Z', 1, '/scratch', 'g1', 'unix',
                 X'0101010101010101', X'0202020202020202');

            INSERT INTO workspace_allocations
                (allocation_id, workspace_id, root_generation, relative_name, parent_identity_kind,
                 parent_unix_device, parent_unix_inode, root_identity_kind, root_unix_device,
                 root_unix_inode, phase, created_at)
            VALUES
                ('wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5', 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5', 'g1',
                 'root', 'unix', X'0303030303030303', X'0404040404040404', 'unix',
                 X'0101010101010101', X'0202020202020202', 'committed',
                 '2026-09-19T00:00:00.000Z');

            INSERT INTO sessions (id, title, channel, workspace_id, created_at, updated_at)
            VALUES ('s', 's', 'desktop', 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5',
                    '2026-09-19T00:00:00.000Z', '2026-09-19T00:00:00.000Z');

            INSERT INTO runs (id, session_id, workspace_id, status, input, usage, created_at)
            VALUES ('r', 's', 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5', 'running',
                    '{"prompt":"go","workspace_id":"ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5","model_override":null,"mode":"normal"}',
                    '{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0,"cost_usd":0.0}',
                    '2026-09-19T00:00:00.000Z');

            INSERT INTO workspace_leases
                (lease_id, workspace_id, root_generation, owner_id, kind, acquired_at)
            VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6', 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5',
                    'g1', 'r', 'run', '2026-09-19T00:00:00.000Z');
            "#,
        )
        .execute(s.pool())
        .await
        .unwrap();
        s
    }
    fn lid(v: &str) -> WorkspaceLeaseId {
        WorkspaceLeaseId::parse(v).unwrap()
    }
    fn now() -> WorkspaceInstant {
        WorkspaceInstant::parse(NOW).unwrap()
    }
    async fn fails(s: &Store, id: &WorkspaceLeaseId, at: &WorkspaceInstant) {
        assert!(
            s.run_workspace_authority_snapshot("r", id, at)
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn valid_snapshot() {
        let s = seed().await;
        let snap = s
            .run_workspace_authority_snapshot("r", &lid(LEASE), &now())
            .await
            .unwrap();
        assert_eq!(snap.workspace_id().as_str(), WS);
        assert_eq!(snap.root_generation(), "g1");
        assert_eq!(snap.relative_name(), "root");
        assert_eq!(
            snap.parent_identity(),
            RootIdentity::unix(&[3; 8], &[4; 8]).unwrap()
        );
        assert_eq!(
            snap.root_identity(),
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap()
        );
        assert!(snap.canonical_root_matches("/scratch"));
        assert!(!snap.canonical_root_matches("/other"));
    }
    #[tokio::test]
    async fn wrong_released_and_duplicate_lease_fail() {
        for mode in 0..3 {
            let s = seed().await;
            if mode == 1 {
                sqlx::query("UPDATE workspace_leases SET released_at=?")
                    .bind(NOW)
                    .execute(s.pool())
                    .await
                    .unwrap();
            } else if mode == 2 {
                sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at,released_at) VALUES (?,?,?,'r','run',?,?)").bind(OTHER).bind(WS).bind("g1").bind(TS).bind(NOW).execute(s.pool()).await.unwrap();
            }
            let id = if mode == 0 { lid(OTHER) } else { lid(LEASE) };
            fails(&s, &id, &now()).await;
        }
    }
    #[tokio::test]
    async fn run_binding_and_liveness_fail_closed() {
        for sql in [
            format!("UPDATE runs SET input=json_set(input,'$.workspace_id','{WS2}') WHERE id='r'"),
            "UPDATE runs SET status='completed' WHERE id='r'".into(),
            format!("UPDATE runs SET ended_at='{NOW}' WHERE id='r'"),
        ] {
            let s = seed().await;
            sqlx::query(&sql).execute(s.pool()).await.unwrap();
            fails(&s, &lid(LEASE), &now()).await;
        }
    }
    #[tokio::test]
    async fn session_binding_is_required() {
        for mode in 0..3 {
            let s = seed().await;
            if mode == 0 {
                sqlx::query("UPDATE sessions SET workspace_id=NULL WHERE id='s'")
                    .execute(s.pool())
                    .await
                    .unwrap();
            } else {
                let mut c = s.pool().acquire().await.unwrap();
                sqlx::query("PRAGMA foreign_keys=OFF")
                    .execute(&mut *c)
                    .await
                    .unwrap();
                if mode == 1 {
                    sqlx::query("UPDATE sessions SET workspace_id=? WHERE id='s'")
                        .bind(WS2)
                        .execute(&mut *c)
                        .await
                        .unwrap();
                } else {
                    sqlx::query("DELETE FROM sessions WHERE id='s'")
                        .execute(&mut *c)
                        .await
                        .unwrap();
                }
                sqlx::query("PRAGMA foreign_keys=ON")
                    .execute(&mut *c)
                    .await
                    .unwrap();
            }
            fails(&s, &lid(LEASE), &now()).await;
        }
    }
    #[tokio::test]
    async fn inactive_or_invalid_clock_workspace_fails() {
        let s = seed().await;
        sqlx::query("UPDATE workspaces SET state='expired'")
            .execute(s.pool())
            .await
            .unwrap();
        fails(&s, &lid(LEASE), &now()).await;
        let s = seed().await;
        let late = WorkspaceInstant::parse("2026-09-19T00:01:00.000Z").unwrap();
        fails(&s, &lid(LEASE), &late).await;
        for sql in [
            "UPDATE workspaces SET created_at='2026-09-19T00:00:02.000Z'",
            "UPDATE workspaces SET renewed_at='2026-09-19T00:00:02.000Z'",
            "UPDATE workspace_leases SET acquired_at='2026-09-19T00:00:02.000Z'",
        ] {
            let s = seed().await;
            sqlx::query(sql).execute(s.pool()).await.unwrap();
            fails(&s, &lid(LEASE), &now()).await;
        }
    }
    #[tokio::test]
    async fn generation_allocation_and_identity_drift_fail() {
        for mode in 0..4 {
            let s = seed().await;
            if mode == 0 {
                let mut c = s.pool().acquire().await.unwrap();
                sqlx::query("PRAGMA foreign_keys=OFF")
                    .execute(&mut *c)
                    .await
                    .unwrap();
                sqlx::query("UPDATE workspace_leases SET root_generation='g2'")
                    .execute(&mut *c)
                    .await
                    .unwrap();
                sqlx::query("PRAGMA foreign_keys=ON")
                    .execute(&mut *c)
                    .await
                    .unwrap();
            } else if mode == 1 {
                sqlx::query("DELETE FROM workspace_allocations")
                    .execute(s.pool())
                    .await
                    .unwrap();
            } else if mode == 2 {
                sqlx::query(
                    "UPDATE workspace_allocations SET phase='retained',failure_reason='other'",
                )
                .execute(s.pool())
                .await
                .unwrap();
            } else {
                sqlx::query("UPDATE workspace_allocations SET root_unix_inode=X'0909090909090909'")
                    .execute(s.pool())
                    .await
                    .unwrap();
            }
            fails(&s, &lid(LEASE), &now()).await;
        }
    }
}
