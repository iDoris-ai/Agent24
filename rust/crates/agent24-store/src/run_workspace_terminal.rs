use agent24_core::check_run_transition;
use agent24_protocol::{Run, RunStatus};
use sqlx::{Sqlite, Transaction};

use crate::{
    LeaseKind, Store, WorkspaceInstant, WorkspaceLeaseId, WorkspaceLeaseRow, WorkspaceResult,
    WorkspaceStoreError,
    repo::{RunPatch, row_to_run, transition_run_tx},
};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TerminalMutation<T> {
    Applied(T),
    Conflict,
}

pub(crate) async fn read_lease_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &WorkspaceLeaseId,
) -> WorkspaceResult<Option<WorkspaceLeaseRow>> {
    sqlx::query("SELECT * FROM workspace_leases WHERE lease_id=? COLLATE BINARY")
        .bind(id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?
        .map(|row| WorkspaceLeaseRow::decode(&row))
        .transpose()
}

pub(crate) async fn release_exact_terminal_lease_tx(
    tx: &mut Transaction<'_, Sqlite>,
    lease: &WorkspaceLeaseRow,
    ended_at: &WorkspaceInstant,
) -> WorkspaceResult<TerminalMutation<WorkspaceLeaseRow>> {
    let r = &lease.record;
    let changed = sqlx::query(
        "UPDATE workspace_leases SET released_at=? WHERE lease_id=? COLLATE BINARY
         AND workspace_id=? COLLATE BINARY AND root_generation=? COLLATE BINARY
         AND owner_id=? COLLATE BINARY AND kind='run' COLLATE BINARY AND released_at IS NULL",
    )
    .bind(ended_at.as_str())
    .bind(r.id.as_str())
    .bind(r.workspace_id.as_str())
    .bind(&r.root_generation)
    .bind(&r.owner_id)
    .execute(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    if changed.rows_affected() != 1 {
        return Ok(TerminalMutation::Conflict);
    }
    let mut expected = lease.clone();
    expected.record.released_at = Some(ended_at.clone());
    match read_lease_tx(tx, &r.id).await? {
        Some(actual) if actual == expected => Ok(TerminalMutation::Applied(expected)),
        _ => Err(WorkspaceStoreError::CorruptRow {
            table: "workspace_leases",
            field: "row",
        }),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunTerminalTransition {
    Applied(Box<Run>),
    Conflict,
}

fn terminal(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
    )
}

impl Store {
    /// Rehydrate the opaque active run-lease id for a persisted run.
    ///
    /// This is read-only. Callers must still use the workspace-aware terminal
    /// transition, which re-validates the lease under `BEGIN IMMEDIATE` before
    /// releasing it.
    pub async fn active_workspace_run_lease_id(
        &self,
        id: &str,
    ) -> WorkspaceResult<Option<WorkspaceLeaseId>> {
        let row = sqlx::query("SELECT * FROM runs WHERE id=? COLLATE BINARY LIMIT 1")
            .bind(id)
            .fetch_optional(self.pool())
            .await
            .map_err(|_| WorkspaceStoreError::Database)?
            .ok_or(WorkspaceStoreError::NotFound)?;
        let run = row_to_run(&row).map_err(|_| WorkspaceStoreError::CorruptRow {
            table: "runs",
            field: "row",
        })?;
        if run.workspace_id != run.input.workspace_id {
            return Err(WorkspaceStoreError::CorruptRow {
                table: "runs",
                field: "workspace_id",
            });
        }
        let rows = sqlx::query(
            "SELECT * FROM workspace_leases
             WHERE owner_id=? COLLATE BINARY AND kind='run' COLLATE BINARY
             ORDER BY lease_id COLLATE BINARY",
        )
        .bind(&run.id)
        .fetch_all(self.pool())
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
        let leases = rows
            .iter()
            .map(WorkspaceLeaseRow::decode)
            .collect::<WorkspaceResult<Vec<_>>>()?;

        let Some(workspace_id) = run.workspace_id.as_ref() else {
            return if leases.is_empty() {
                Ok(None)
            } else {
                Err(WorkspaceStoreError::CorruptRow {
                    table: "workspace_leases",
                    field: "row",
                })
            };
        };
        if leases.len() != 1 {
            return Err(WorkspaceStoreError::CorruptRow {
                table: "workspace_leases",
                field: "row",
            });
        }
        let lease = &leases[0];
        let created_at = WorkspaceInstant::parse(&run.created_at).map_err(|_| {
            WorkspaceStoreError::CorruptRow {
                table: "runs",
                field: "created_at",
            }
        })?;
        let root_generation: String = sqlx::query_scalar(
            "SELECT root_generation FROM workspaces WHERE id=? COLLATE BINARY LIMIT 1",
        )
        .bind(workspace_id.as_str())
        .fetch_optional(self.pool())
        .await
        .map_err(|_| WorkspaceStoreError::Database)?
        .ok_or(WorkspaceStoreError::CorruptRow {
            table: "workspaces",
            field: "id",
        })?;
        if lease.record.kind != LeaseKind::Run
            || lease.record.owner_id != run.id
            || lease.record.workspace_id != *workspace_id
            || lease.record.root_generation != root_generation
            || lease.record.released_at.is_some()
            || lease.record.acquired_at < created_at
        {
            return Err(WorkspaceStoreError::CorruptRow {
                table: "workspace_leases",
                field: "row",
            });
        }
        Ok(Some(lease.record.id.clone()))
    }

    /// Dormant store-only terminal transition for a workspace-bound run.
    pub async fn transition_workspace_run_terminal(
        &self,
        id: &str,
        to: RunStatus,
        patch: RunPatch,
        lease_id: &WorkspaceLeaseId,
        ended_at: &WorkspaceInstant,
    ) -> WorkspaceResult<RunTerminalTransition> {
        if !terminal(to) || patch.ended_at.as_deref() != Some(ended_at.as_str()) {
            return Err(WorkspaceStoreError::InvalidValue { field: "run" });
        }
        let mut tx = self.begin_workspace_immediate().await?;
        let row = sqlx::query("SELECT * FROM runs WHERE id=? COLLATE BINARY LIMIT 1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| WorkspaceStoreError::Database)?
            .ok_or(WorkspaceStoreError::NotFound)?;
        let before = row_to_run(&row).map_err(|_| WorkspaceStoreError::CorruptRow {
            table: "runs",
            field: "row",
        })?;
        let Some(workspace_id) = before.workspace_id.as_ref() else {
            return Ok(RunTerminalTransition::Conflict);
        };
        if before.input.workspace_id.as_ref() != Some(workspace_id)
            || check_run_transition(before.status, to).is_err()
        {
            return Ok(RunTerminalTransition::Conflict);
        }
        let created_at = WorkspaceInstant::parse(&before.created_at).map_err(|_| {
            WorkspaceStoreError::CorruptRow {
                table: "runs",
                field: "created_at",
            }
        })?;
        let root_generation: String = sqlx::query_scalar(
            "SELECT root_generation FROM workspaces WHERE id=? COLLATE BINARY LIMIT 1",
        )
        .bind(workspace_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?
        .ok_or(WorkspaceStoreError::CorruptRow {
            table: "workspaces",
            field: "id",
        })?;
        let Some(lease) = read_lease_tx(&mut tx, lease_id).await? else {
            return Ok(RunTerminalTransition::Conflict);
        };
        if lease.record.kind != LeaseKind::Run
            || lease.record.owner_id != before.id
            || lease.record.workspace_id != *workspace_id
            || lease.record.root_generation != root_generation
            || lease.record.released_at.is_some()
            || lease.record.acquired_at < created_at
            || lease.record.acquired_at > *ended_at
        {
            return Ok(RunTerminalTransition::Conflict);
        }

        let mut expected = before.clone();
        expected.status = to;
        if let Some(v) = &patch.output {
            expected.output = Some(v.clone());
        }
        if let Some(v) = &patch.error {
            expected.error = Some(v.clone());
        }
        if let Some(v) = &patch.usage {
            expected.usage = v.clone();
        }
        if let Some(v) = &patch.started_at {
            expected.started_at = Some(v.clone());
        }
        if let Some(v) = &patch.ended_at {
            expected.ended_at = Some(v.clone());
        }

        if !transition_run_tx(&mut tx, id, before.status, to, &patch)
            .await
            .map_err(|_| WorkspaceStoreError::Database)?
        {
            return Ok(RunTerminalTransition::Conflict);
        }
        let released = match release_exact_terminal_lease_tx(&mut tx, &lease, ended_at).await? {
            TerminalMutation::Applied(row) => row,
            TerminalMutation::Conflict => return Ok(RunTerminalTransition::Conflict),
        };
        let after = sqlx::query("SELECT * FROM runs WHERE id=? COLLATE BINARY LIMIT 1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| WorkspaceStoreError::Database)
            .and_then(|row| {
                row_to_run(&row).map_err(|_| WorkspaceStoreError::CorruptRow {
                    table: "runs",
                    field: "row",
                })
            })?;
        let after_lease = read_lease_tx(&mut tx, lease_id).await?;
        let after_root_generation: String = sqlx::query_scalar(
            "SELECT root_generation FROM workspaces WHERE id=? COLLATE BINARY LIMIT 1",
        )
        .bind(workspace_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?
        .ok_or(WorkspaceStoreError::CorruptRow {
            table: "workspaces",
            field: "id",
        })?;
        if after != expected
            || after_lease.as_ref() != Some(&released)
            || after_root_generation != root_generation
        {
            return Err(WorkspaceStoreError::CorruptRow {
                table: "runs",
                field: "row",
            });
        }
        tx.commit()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        Ok(RunTerminalTransition::Applied(Box::new(after)))
    }
}

#[cfg(test)]
#[rustfmt::skip]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use agent24_protocol::{RunInput, RunMode, Usage, WorkspaceId};

    const WS:&str="ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5"; const WS2:&str="ws_01J5M4Q2Y7N8P9R0S1T2V3W4X7"; const TS:&str="2026-09-19T00:00:00.000Z"; const END:&str="2026-09-19T00:00:01.000Z"; const LEASE:&str="wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
    fn run(id:&str,status:RunStatus,workspace:bool)->Run{let w=workspace.then(||WorkspaceId::parse(WS).unwrap());Run{id:id.into(),session_id:None,workspace_id:w.clone(),status,input:RunInput{prompt:"go".into(),workspace_id:w,model_override:None,mode:RunMode::Normal},output:None,error:None,usage:Usage::default(),schedule_id:None,created_at:TS.into(),started_at:None,ended_at:None}}
    async fn seed_workspace_row(st:&Store,id:&str){sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?,'2026-09-19T00:01:00.000Z',1,?,'g1','unix',X'0303030303030303',X'0404040404040404')").bind(id).bind(TS).bind(format!("/scratch/{id}")).execute(st.pool()).await.unwrap();}
    async fn seed_bound(st:&Store,status:RunStatus){sqlx::raw_sql("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES ('ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','orchestrator_scratch','active','test','external','orchestrator','owner','serial','2026-09-19T00:00:00.000Z','2026-09-19T00:01:00.000Z',1,'/scratch','g1','unix',X'0101010101010101',X'0202020202020202')").execute(st.pool()).await.unwrap();let r=run("r",status,true);sqlx::query("INSERT INTO runs (id,workspace_id,status,input,usage,created_at) VALUES (?,?,?,?,?,?)").bind(&r.id).bind(WS).bind(crate::repo::status_str(status)).bind(serde_json::to_string(&r.input).unwrap()).bind(serde_json::to_string(&r.usage).unwrap()).bind(TS).execute(st.pool()).await.unwrap();sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,?,'r','run',?)").bind(LEASE).bind(WS).bind("g1").bind(TS).execute(st.pool()).await.unwrap();}
    async fn lease_released(st:&Store)->Option<String>{sqlx::query_scalar("SELECT released_at FROM workspace_leases WHERE lease_id=?").bind(LEASE).fetch_one(st.pool()).await.unwrap()}

    #[tokio::test] async fn active_run_lease_lookup_handles_bound_and_legacy(){let st=Store::open_memory().await.unwrap();let legacy=run("legacy",RunStatus::Running,false);st.insert_run(&legacy).await.unwrap();assert_eq!(st.active_workspace_run_lease_id("legacy").await.unwrap(),None);seed_bound(&st,RunStatus::Running).await;assert_eq!(st.active_workspace_run_lease_id("r").await.unwrap().as_ref().map(WorkspaceLeaseId::as_str),Some(LEASE));}
    #[tokio::test] async fn active_run_lease_lookup_fails_closed_on_missing_or_released(){for mode in 0..2{let st=Store::open_memory().await.unwrap();seed_bound(&st,RunStatus::Running).await;if mode==0{sqlx::query("DELETE FROM workspace_leases WHERE lease_id=?").bind(LEASE).execute(st.pool()).await.unwrap();}else{sqlx::query("UPDATE workspace_leases SET released_at=? WHERE lease_id=?").bind(END).bind(LEASE).execute(st.pool()).await.unwrap();}assert!(matches!(st.active_workspace_run_lease_id("r").await,Err(WorkspaceStoreError::CorruptRow{table:"workspace_leases",field:"row"})));}}
    #[tokio::test] async fn active_run_lease_lookup_fails_closed_on_duplicate_history(){let st=Store::open_memory().await.unwrap();seed_bound(&st,RunStatus::Running).await;sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at,released_at) VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X8',?,'g1','r','run',?,?)").bind(WS).bind(TS).bind(END).execute(st.pool()).await.unwrap();assert!(matches!(st.active_workspace_run_lease_id("r").await,Err(WorkspaceStoreError::CorruptRow{table:"workspace_leases",field:"row"})));}
    #[tokio::test] async fn active_run_lease_lookup_fails_closed_on_binding_owner_kind_and_time(){for mode in 0..4{let st=Store::open_memory().await.unwrap();seed_bound(&st,RunStatus::Running).await;match mode{0=>{sqlx::query("UPDATE runs SET input=json_set(input,'$.workspace_id',?) WHERE id='r'").bind(WS2).execute(st.pool()).await.unwrap();},1=>{seed_workspace_row(&st,WS2).await;sqlx::query("UPDATE workspace_leases SET workspace_id=? WHERE lease_id=?").bind(WS2).bind(LEASE).execute(st.pool()).await.unwrap();},2=>{sqlx::query("UPDATE workspace_leases SET owner_id='other' WHERE lease_id=?").bind(LEASE).execute(st.pool()).await.unwrap();},3=>{sqlx::query("UPDATE workspace_leases SET kind='host',owner_id='h',daemon_generation='d',host_instance_id='h',expires_at='2026-09-19T00:00:30.000Z' WHERE lease_id=?").bind(LEASE).execute(st.pool()).await.unwrap();},_=>unreachable!()}let result=st.active_workspace_run_lease_id("r").await;assert!(matches!(result,Err(WorkspaceStoreError::CorruptRow{..})));}let st=Store::open_memory().await.unwrap();seed_bound(&st,RunStatus::Running).await;sqlx::query("UPDATE workspace_leases SET acquired_at='2026-09-18T23:59:59.999Z' WHERE lease_id=?").bind(LEASE).execute(st.pool()).await.unwrap();assert!(matches!(st.active_workspace_run_lease_id("r").await,Err(WorkspaceStoreError::CorruptRow{table:"workspace_leases",..})));}
    #[tokio::test] async fn active_run_lease_lookup_rejects_active_lease_owned_by_unbound_run(){let st=Store::open_memory().await.unwrap();seed_workspace_row(&st,WS2).await;let legacy=run("legacy",RunStatus::Running,false);st.insert_run(&legacy).await.unwrap();sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7',?,'g1','legacy','run',?)").bind(WS2).bind(TS).execute(st.pool()).await.unwrap();assert!(matches!(st.active_workspace_run_lease_id("legacy").await,Err(WorkspaceStoreError::CorruptRow{table:"workspace_leases",..})));}
    #[tokio::test] async fn active_run_lease_lookup_missing_run_is_not_found(){let st=Store::open_memory().await.unwrap();assert_eq!(st.active_workspace_run_lease_id("missing").await.unwrap_err(),WorkspaceStoreError::NotFound);}
    #[tokio::test] async fn terminal_states_release_exact_lease(){for to in [RunStatus::Completed,RunStatus::Failed,RunStatus::Cancelled]{let st=Store::open_memory().await.unwrap();seed_bound(&st,RunStatus::Running).await;let out=st.transition_workspace_run_terminal("r",to,RunPatch{ended_at:Some(END.into()),..Default::default()},&WorkspaceLeaseId::parse(LEASE).unwrap(),&WorkspaceInstant::parse(END).unwrap()).await.unwrap();assert!(matches!(out,RunTerminalTransition::Applied(ref run) if run.status==to));assert_eq!(lease_released(&st).await.as_deref(),Some(END));}}
    #[tokio::test] async fn wrong_missing_and_released_lease_fail_closed(){for mode in 0..3{let st=Store::open_memory().await.unwrap();seed_bound(&st,RunStatus::Running).await;let mut id=WorkspaceLeaseId::parse(LEASE).unwrap();if mode==0{id=WorkspaceLeaseId::parse("wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7").unwrap();sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,daemon_generation,host_instance_id,acquired_at,expires_at) VALUES (?,?,?,'host','host','d','host',?,'2026-09-19T00:00:30.000Z')").bind(id.as_str()).bind(WS).bind("g1").bind(TS).execute(st.pool()).await.unwrap();}else if mode==1{sqlx::query("DELETE FROM workspace_leases WHERE lease_id=?").bind(LEASE).execute(st.pool()).await.unwrap();}else{sqlx::query("UPDATE workspace_leases SET released_at=? WHERE lease_id=?").bind(END).bind(LEASE).execute(st.pool()).await.unwrap();}assert_eq!(st.transition_workspace_run_terminal("r",RunStatus::Cancelled,RunPatch{ended_at:Some(END.into()),..Default::default()},&id,&WorkspaceInstant::parse(END).unwrap()).await.unwrap(),RunTerminalTransition::Conflict);assert_eq!(st.get_run("r").await.unwrap().unwrap().status,RunStatus::Running);}}
    #[tokio::test] async fn trigger_mutation_rolls_back_run_and_lease(){let st=Store::open_memory().await.unwrap();seed_bound(&st,RunStatus::Running).await;sqlx::raw_sql("CREATE TRIGGER twist_terminal_lease AFTER UPDATE OF released_at ON workspace_leases BEGIN UPDATE workspace_leases SET acquired_at='2026-09-19T00:00:00.001Z' WHERE lease_id=NEW.lease_id; END;").execute(st.pool()).await.unwrap();assert!(matches!(st.transition_workspace_run_terminal("r",RunStatus::Cancelled,RunPatch{ended_at:Some(END.into()),..Default::default()},&WorkspaceLeaseId::parse(LEASE).unwrap(),&WorkspaceInstant::parse(END).unwrap()).await,Err(WorkspaceStoreError::CorruptRow{table:"workspace_leases",..})));assert_eq!(st.get_run("r").await.unwrap().unwrap().status,RunStatus::Running);assert_eq!(lease_released(&st).await,None);}
    #[tokio::test] async fn legacy_direct_transition_remains_available(){let st=Store::open_memory().await.unwrap();st.insert_run(&run("legacy",RunStatus::Queued,false)).await.unwrap();st.transition_run("legacy",RunStatus::Running,RunPatch::default()).await.unwrap();let done=st.transition_run("legacy",RunStatus::Cancelled,RunPatch{ended_at:Some(END.into()),..Default::default()}).await.unwrap();assert_eq!(done.status,RunStatus::Cancelled);}
    #[tokio::test] async fn public_bypass_and_orphan_sweep_spare_bound_run(){let st=Store::open_memory().await.unwrap();seed_bound(&st,RunStatus::Queued).await;let running=st.transition_run("r",RunStatus::Running,RunPatch{started_at:Some(TS.into()),..Default::default()}).await.unwrap();assert_eq!(running.status,RunStatus::Running);assert!(matches!(st.transition_run("r",RunStatus::Cancelled,RunPatch{ended_at:Some(END.into()),..Default::default()}).await,Err(crate::StoreError::Conflict(_))));assert_eq!(st.sweep_orphan_runs(END).await.unwrap(),0);assert_eq!(st.get_run("r").await.unwrap().unwrap().status,RunStatus::Running);assert_eq!(lease_released(&st).await,None);}
}
