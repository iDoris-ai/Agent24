use agent24_protocol::RunStatus;
use sqlx::{Sqlite, Transaction};

use crate::{
    LeaseKind, Store, WorkspaceInstant, WorkspaceLeaseRow, WorkspaceResult, WorkspaceStoreError,
    repo::{RunPatch, row_to_run, transition_run_tx},
    run_workspace_terminal::{TerminalMutation, release_exact_terminal_lease_tx},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceOrphanSweep {
    pub released_leases: u64,
}

async fn run_lease_history_tx(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
) -> WorkspaceResult<Vec<WorkspaceLeaseRow>> {
    let rows = sqlx::query(
        "SELECT * FROM workspace_leases
         WHERE owner_id=? COLLATE BINARY AND kind='run' COLLATE BINARY
         ORDER BY lease_id COLLATE BINARY",
    )
    .bind(run_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    rows.iter().map(WorkspaceLeaseRow::decode).collect()
}

async fn cancel_with_lease_tx(
    tx: &mut Transaction<'_, Sqlite>,
    run: &agent24_protocol::Run,
    lease: &WorkspaceLeaseRow,
    ended_at: &WorkspaceInstant,
) -> WorkspaceResult<()> {
    let workspace_id = run
        .workspace_id
        .as_ref()
        .ok_or(WorkspaceStoreError::CorruptRow {
            table: "runs",
            field: "workspace_id",
        })?;
    let created_at =
        WorkspaceInstant::parse(&run.created_at).map_err(|_| WorkspaceStoreError::CorruptRow {
            table: "runs",
            field: "created_at",
        })?;
    let root_generation: String = sqlx::query_scalar(
        "SELECT root_generation FROM workspaces WHERE id=? COLLATE BINARY LIMIT 1",
    )
    .bind(workspace_id.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?
    .ok_or(WorkspaceStoreError::CorruptRow {
        table: "workspaces",
        field: "id",
    })?;
    if run.input.workspace_id.as_ref() != Some(workspace_id)
        || lease.record.kind != LeaseKind::Run
        || lease.record.owner_id != run.id
        || lease.record.workspace_id != *workspace_id
        || lease.record.root_generation != root_generation
        || lease.record.released_at.is_some()
        || lease.record.acquired_at < created_at
        || lease.record.acquired_at > *ended_at
    {
        return Err(WorkspaceStoreError::CorruptRow {
            table: "workspace_leases",
            field: "row",
        });
    }
    let patch = RunPatch {
        ended_at: Some(ended_at.as_str().to_owned()),
        ..RunPatch::default()
    };
    if !transition_run_tx(tx, &run.id, run.status, RunStatus::Cancelled, &patch)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?
    {
        return Err(WorkspaceStoreError::CorruptRow {
            table: "runs",
            field: "status",
        });
    }
    let released = match release_exact_terminal_lease_tx(tx, lease, ended_at).await? {
        TerminalMutation::Applied(row) => row,
        TerminalMutation::Conflict => {
            return Err(WorkspaceStoreError::CorruptRow {
                table: "workspace_leases",
                field: "row",
            });
        }
    };
    let row = sqlx::query("SELECT * FROM runs WHERE id=? COLLATE BINARY LIMIT 1")
        .bind(&run.id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
    let after = row_to_run(&row).map_err(|_| WorkspaceStoreError::CorruptRow {
        table: "runs",
        field: "row",
    })?;
    let mut expected = run.clone();
    expected.status = RunStatus::Cancelled;
    expected.ended_at = Some(ended_at.as_str().to_owned());
    let history = run_lease_history_tx(tx, &run.id).await?;
    let after_root: String = sqlx::query_scalar(
        "SELECT root_generation FROM workspaces WHERE id=? COLLATE BINARY LIMIT 1",
    )
    .bind(workspace_id.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    if after != expected
        || history.len() != 1
        || history.first() != Some(&released)
        || after_root != root_generation
    {
        return Err(WorkspaceStoreError::CorruptRow {
            table: "runs",
            field: "row",
        });
    }
    Ok(())
}

impl Store {
    /// Dormant startup reconciliation for workspace-bound non-terminal runs.
    pub async fn sweep_workspace_orphan_runs(
        &self,
        ended_at: &WorkspaceInstant,
    ) -> WorkspaceResult<WorkspaceOrphanSweep> {
        let mut tx = self.begin_workspace_immediate().await?;
        let rows = sqlx::query(
            "SELECT * FROM runs
             WHERE workspace_id IS NOT NULL
               AND (status IN ('queued','running')
                    OR (status='awaiting_approval' AND id NOT IN
                        (SELECT run_id FROM approvals WHERE status='pending')))
             ORDER BY id COLLATE BINARY",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
        let mut result = WorkspaceOrphanSweep { released_leases: 0 };
        for row in rows {
            let run = row_to_run(&row).map_err(|_| WorkspaceStoreError::CorruptRow {
                table: "runs",
                field: "row",
            })?;
            if run.status == RunStatus::AwaitingApproval {
                let pending: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM approvals
                     WHERE run_id=? COLLATE BINARY AND status='pending' COLLATE BINARY)",
                )
                .bind(&run.id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| WorkspaceStoreError::Database)?;
                if pending {
                    continue;
                }
            }
            let history = run_lease_history_tx(&mut tx, &run.id).await?;
            if history.is_empty() {
                return Err(WorkspaceStoreError::CorruptRow {
                    table: "workspace_leases",
                    field: "row",
                });
            }
            if history.len() != 1 || history[0].record.released_at.is_some() {
                return Err(WorkspaceStoreError::CorruptRow {
                    table: "workspace_leases",
                    field: "row",
                });
            }
            cancel_with_lease_tx(&mut tx, &run, &history[0], ended_at).await?;
            result.released_leases += 1;
        }
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM runs
             WHERE workspace_id IS NOT NULL
               AND (status IN ('queued','running')
                    OR (status='awaiting_approval' AND id NOT IN
                        (SELECT run_id FROM approvals WHERE status='pending')))",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
        if remaining != 0 {
            return Err(WorkspaceStoreError::CorruptRow {
                table: "runs",
                field: "status",
            });
        }
        tx.commit()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        Ok(result)
    }
}

#[cfg(test)]
#[rustfmt::skip]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use agent24_protocol::{RunInput, RunMode, Usage, WorkspaceId};
    const WS:&str="ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5"; const TS:&str="2026-09-19T00:00:00.000Z"; const END:&str="2026-09-19T00:00:01.000Z"; const LEASE:&str="wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
    #[tokio::test] async fn sweep_releases_exact_lease_and_spares_parked(){
        let st=Store::open_memory().await.unwrap();
        sqlx::raw_sql("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES ('ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','orchestrator_scratch','active','test','external','orchestrator','owner','serial','2026-09-19T00:00:00.000Z','2026-09-19T00:01:00.000Z',1,'/scratch','g1','unix',X'0101010101010101',X'0202020202020202')").execute(st.pool()).await.unwrap();
        let input=serde_json::to_string(&RunInput{prompt:"go".into(),workspace_id:Some(WorkspaceId::parse(WS).unwrap()),model_override:None,mode:RunMode::Normal}).unwrap(); let usage=serde_json::to_string(&Usage::default()).unwrap();
        for (id,status) in [("leased","running"),("parked","awaiting_approval")]{sqlx::query("INSERT INTO runs (id,workspace_id,status,input,usage,created_at) VALUES (?,?,?,?,?,?)").bind(id).bind(WS).bind(status).bind(&input).bind(&usage).bind(TS).execute(st.pool()).await.unwrap();}
        sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,'g1','leased','run',?)").bind(LEASE).bind(WS).bind(TS).execute(st.pool()).await.unwrap();
        sqlx::query("INSERT INTO approvals (id,run_id,tool_call_id,kind,summary,payload,available_decisions,status,expires_at,created_at) VALUES ('a','parked','t','exec','s','{}','[]','pending','2026-09-19T00:02:00.000Z',?)").bind(TS).execute(st.pool()).await.unwrap();
        assert_eq!(st.sweep_workspace_orphan_runs(&WorkspaceInstant::parse(END).unwrap()).await.unwrap(),WorkspaceOrphanSweep{released_leases:1});
        assert_eq!(st.get_run("leased").await.unwrap().unwrap().status,RunStatus::Cancelled); assert_eq!(st.get_run("parked").await.unwrap().unwrap().status,RunStatus::AwaitingApproval);
        let released:Option<String>=sqlx::query_scalar("SELECT released_at FROM workspace_leases WHERE lease_id=?").bind(LEASE).fetch_one(st.pool()).await.unwrap(); assert_eq!(released.as_deref(),Some(END));
    }
    #[tokio::test] async fn trigger_created_pending_approval_spares_later_candidate(){
        let st=Store::open_memory().await.unwrap();
        let ws2="ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
        sqlx::raw_sql("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES ('ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','orchestrator_scratch','active','test','external','orchestrator','owner-a','serial','2026-09-19T00:00:00.000Z','2026-09-19T00:01:00.000Z',1,'/scratch/a','g1','unix',X'0101010101010101',X'0202020202020202'),('ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6','orchestrator_scratch','active','test','external','orchestrator','owner-b','serial','2026-09-19T00:00:00.000Z','2026-09-19T00:01:00.000Z',1,'/scratch/b','g2','unix',X'0303030303030303',X'0404040404040404')").execute(st.pool()).await.unwrap();
        let input_a=serde_json::to_string(&RunInput{prompt:"go".into(),workspace_id:Some(WorkspaceId::parse(WS).unwrap()),model_override:None,mode:RunMode::Normal}).unwrap(); let input_b=serde_json::to_string(&RunInput{prompt:"go".into(),workspace_id:Some(WorkspaceId::parse(ws2).unwrap()),model_override:None,mode:RunMode::Normal}).unwrap(); let usage=serde_json::to_string(&Usage::default()).unwrap();
        sqlx::query("INSERT INTO runs (id,workspace_id,status,input,usage,created_at) VALUES ('a-running',?,'running',?,?,?),('b-parked',?,'awaiting_approval',?,?,?)").bind(WS).bind(&input_a).bind(&usage).bind(TS).bind(ws2).bind(&input_b).bind(&usage).bind(TS).execute(st.pool()).await.unwrap();
        sqlx::raw_sql("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','g1','a-running','run','2026-09-19T00:00:00.000Z'),('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6','g2','b-parked','run','2026-09-19T00:00:00.000Z'); CREATE TRIGGER park_later AFTER UPDATE OF released_at ON workspace_leases WHEN NEW.owner_id='a-running' BEGIN INSERT INTO approvals (id,run_id,tool_call_id,kind,summary,payload,available_decisions,status,expires_at,created_at) VALUES ('a-late','b-parked','t','exec','s','{}','[]','pending','2026-09-19T00:02:00.000Z','2026-09-19T00:00:00.000Z'); END;").execute(st.pool()).await.unwrap();
        assert_eq!(st.sweep_workspace_orphan_runs(&WorkspaceInstant::parse(END).unwrap()).await.unwrap(),WorkspaceOrphanSweep{released_leases:1});
        assert_eq!(st.get_run("a-running").await.unwrap().unwrap().status,RunStatus::Cancelled); assert_eq!(st.get_run("b-parked").await.unwrap().unwrap().status,RunStatus::AwaitingApproval);
        let leases:Vec<(String,Option<String>)>=sqlx::query_as("SELECT owner_id,released_at FROM workspace_leases ORDER BY owner_id").fetch_all(st.pool()).await.unwrap(); assert_eq!(leases,vec![("a-running".into(),Some(END.into())),("b-parked".into(),None)]);
        let status:String=sqlx::query_scalar("SELECT status FROM approvals WHERE id='a-late'").fetch_one(st.pool()).await.unwrap(); assert_eq!(status,"pending");
    }
    #[tokio::test] async fn missing_history_still_fails_closed(){
        let st=Store::open_memory().await.unwrap();
        sqlx::raw_sql("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES ('ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','orchestrator_scratch','active','test','external','orchestrator','owner','serial','2026-09-19T00:00:00.000Z','2026-09-19T00:01:00.000Z',1,'/scratch','g1','unix',X'0101010101010101',X'0202020202020202')").execute(st.pool()).await.unwrap();
        let input=serde_json::to_string(&RunInput{prompt:"go".into(),workspace_id:Some(WorkspaceId::parse(WS).unwrap()),model_override:None,mode:RunMode::Normal}).unwrap(); let usage=serde_json::to_string(&Usage::default()).unwrap();
        sqlx::query("INSERT INTO runs (id,workspace_id,status,input,usage,created_at) VALUES ('missing',?,'running',?,?,?)").bind(WS).bind(&input).bind(&usage).bind(TS).execute(st.pool()).await.unwrap();
        assert!(matches!(st.sweep_workspace_orphan_runs(&WorkspaceInstant::parse(END).unwrap()).await,Err(WorkspaceStoreError::CorruptRow{table:"workspace_leases",field:"row"}))); assert_eq!(st.get_run("missing").await.unwrap().unwrap().status,RunStatus::Running);
    }
}
