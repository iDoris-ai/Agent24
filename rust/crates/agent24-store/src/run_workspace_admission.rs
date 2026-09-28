use agent24_protocol::{Run, WorkspaceId};
use sqlx::{Row, Sqlite, Transaction};

use crate::{
    AllocationPhase, AllocationRecord, LeaseKind, Store, WorkspaceInstant, WorkspaceKind,
    WorkspaceLeaseId, WorkspaceLeaseRecord, WorkspaceLeaseRow, WorkspaceResult, WorkspaceRow,
    WorkspaceState, WorkspaceStoreError,
    repo::{insert_run_tx, row_to_run},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunAdmissionDenial {
    SessionNotFound,
    WorkspaceNotFound,
    BindingConflict,
    WorkspaceExpired,
    WorkspaceReleased,
    WorkspaceUnavailable,
    WorkspaceBusy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunAdmission {
    Admitted { lease_id: Option<WorkspaceLeaseId> },
    Denied(RunAdmissionDenial),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionScope {
    Missing,
    Unbound,
    Bound(WorkspaceId),
}

async fn session_scope_tx(
    tx: &mut Transaction<'_, Sqlite>,
    run: &Run,
) -> WorkspaceResult<Option<SessionScope>> {
    let Some(id) = run.session_id.as_deref() else {
        return Ok(None);
    };
    let Some(row) =
        sqlx::query("SELECT workspace_id FROM sessions WHERE id=? COLLATE BINARY LIMIT 1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| WorkspaceStoreError::Database)?
    else {
        return Ok(Some(SessionScope::Missing));
    };
    let raw = row
        .try_get::<Option<String>, _>("workspace_id")
        .map_err(|_| WorkspaceStoreError::CorruptRow {
            table: "sessions",
            field: "workspace_id",
        })?;
    let parsed =
        raw.map(WorkspaceId::parse)
            .transpose()
            .map_err(|_| WorkspaceStoreError::CorruptRow {
                table: "sessions",
                field: "workspace_id",
            })?;
    Ok(Some(
        parsed.map_or(SessionScope::Unbound, SessionScope::Bound),
    ))
}

async fn workspace_facts_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &WorkspaceId,
) -> WorkspaceResult<Option<(WorkspaceRow, AllocationRecord)>> {
    let Some(row) = sqlx::query("SELECT * FROM workspaces WHERE id=? COLLATE BINARY LIMIT 1")
        .bind(id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?
    else {
        return Ok(None);
    };
    let workspace = WorkspaceRow::decode(&row)?;
    let allocation = sqlx::query(
        "SELECT * FROM workspace_allocations WHERE workspace_id=? COLLATE BINARY LIMIT 1",
    )
    .bind(id.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?
    .ok_or(WorkspaceStoreError::CorruptRow {
        table: "workspace_allocations",
        field: "workspace_id",
    })?;
    let allocation = AllocationRecord::decode(&allocation)?;
    if allocation.phase() != AllocationPhase::Committed
        || allocation.workspace_id() != id
        || allocation.root_generation() != workspace.root.root_generation()
        || allocation.root_identity() != Some(workspace.root.identity())
    {
        return Err(WorkspaceStoreError::CorruptRow {
            table: "workspace_allocations",
            field: "root_binding",
        });
    }
    Ok(Some((workspace, allocation)))
}

async fn run_busy_tx(
    tx: &mut Transaction<'_, Sqlite>,
    workspace_id: &WorkspaceId,
    run_id: &str,
) -> WorkspaceResult<bool> {
    sqlx::query("SELECT 1 FROM workspace_leases WHERE kind='run' AND released_at IS NULL AND (workspace_id=? COLLATE BINARY OR owner_id=? COLLATE BINARY) LIMIT 1")
        .bind(workspace_id.as_str()).bind(run_id).fetch_optional(&mut **tx).await
        .map(|r| r.is_some()).map_err(|_| WorkspaceStoreError::Database)
}

impl Store {
    /// Dormant store-only admission. Runtime activation is intentionally later.
    pub async fn insert_run_with_workspace_admission(
        &self,
        run: &Run,
        lease_id: Option<WorkspaceLeaseId>,
        acquired_at: &WorkspaceInstant,
    ) -> WorkspaceResult<RunAdmission> {
        if run.workspace_id != run.input.workspace_id {
            return Err(WorkspaceStoreError::InvalidValue {
                field: "workspace_id",
            });
        }
        if run.created_at != acquired_at.as_str() {
            return Err(WorkspaceStoreError::InvalidValue {
                field: "acquired_at",
            });
        }
        let mut tx = self.begin_workspace_immediate().await?;
        let session = session_scope_tx(&mut tx, run).await?;
        if session == Some(SessionScope::Missing) {
            return Ok(RunAdmission::Denied(RunAdmissionDenial::SessionNotFound));
        }

        let Some(workspace_id) = run.workspace_id.as_ref() else {
            if matches!(session, Some(SessionScope::Bound(_))) {
                return Ok(RunAdmission::Denied(RunAdmissionDenial::BindingConflict));
            }
            if lease_id.is_some() {
                return Err(WorkspaceStoreError::InvalidValue { field: "lease_id" });
            }
            insert_run_tx(&mut tx, run)
                .await
                .map_err(|_| WorkspaceStoreError::Database)?;
            let row = sqlx::query("SELECT * FROM runs WHERE id=? COLLATE BINARY LIMIT 1")
                .bind(&run.id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| WorkspaceStoreError::Database)?;
            if row_to_run(&row).map_err(|_| WorkspaceStoreError::Database)? != *run {
                return Err(WorkspaceStoreError::CorruptRow {
                    table: "runs",
                    field: "row",
                });
            }
            tx.commit()
                .await
                .map_err(|_| WorkspaceStoreError::Database)?;
            return Ok(RunAdmission::Admitted { lease_id: None });
        };

        let lease_id = lease_id.ok_or(WorkspaceStoreError::InvalidValue { field: "lease_id" })?;
        if session != Some(SessionScope::Bound(workspace_id.clone())) {
            return Ok(RunAdmission::Denied(RunAdmissionDenial::BindingConflict));
        }
        let Some((workspace, allocation)) = workspace_facts_tx(&mut tx, workspace_id).await? else {
            return Ok(RunAdmission::Denied(RunAdmissionDenial::WorkspaceNotFound));
        };
        if workspace.kind != WorkspaceKind::OrchestratorScratch {
            return Ok(RunAdmission::Denied(
                RunAdmissionDenial::WorkspaceUnavailable,
            ));
        }
        if acquired_at < &workspace.created_at
            || workspace
                .renewed_at
                .as_ref()
                .is_some_and(|at| acquired_at < at)
        {
            return Ok(RunAdmission::Denied(
                RunAdmissionDenial::WorkspaceUnavailable,
            ));
        }
        if workspace.state == WorkspaceState::Active && acquired_at >= &workspace.expires_at {
            crate::workspace_lifecycle::expire_workspace_tx(
                &mut tx,
                workspace_id,
                acquired_at,
                &workspace,
            )
            .await?;
            tx.commit()
                .await
                .map_err(|_| WorkspaceStoreError::Database)?;
            return Ok(RunAdmission::Denied(RunAdmissionDenial::WorkspaceExpired));
        }
        match workspace.state {
            WorkspaceState::Active => {}
            WorkspaceState::Expired => {
                return Ok(RunAdmission::Denied(RunAdmissionDenial::WorkspaceExpired));
            }
            WorkspaceState::Releasing | WorkspaceState::Released => {
                return Ok(RunAdmission::Denied(RunAdmissionDenial::WorkspaceReleased));
            }
            WorkspaceState::CleanupFailed => {
                return Ok(RunAdmission::Denied(
                    RunAdmissionDenial::WorkspaceUnavailable,
                ));
            }
        }
        if run_busy_tx(&mut tx, workspace_id, &run.id).await? {
            return Ok(RunAdmission::Denied(RunAdmissionDenial::WorkspaceBusy));
        }

        insert_run_tx(&mut tx, run)
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,?,?,'run',?)")
            .bind(lease_id.as_str()).bind(workspace_id.as_str()).bind(workspace.root.root_generation())
            .bind(&run.id).bind(acquired_at.as_str()).execute(&mut *tx).await.map_err(|_| WorkspaceStoreError::Database)?;
        let run_row = sqlx::query("SELECT * FROM runs WHERE id=? COLLATE BINARY LIMIT 1")
            .bind(&run.id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        let lease_row =
            sqlx::query("SELECT * FROM workspace_leases WHERE lease_id=? COLLATE BINARY")
                .bind(lease_id.as_str())
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| WorkspaceStoreError::Database)?;
        let (after_workspace, after_allocation) = workspace_facts_tx(&mut tx, workspace_id)
            .await?
            .ok_or(WorkspaceStoreError::NotFound)?;
        let lease = WorkspaceLeaseRow::decode(&lease_row)?;
        let expected_lease = WorkspaceLeaseRecord {
            id: lease_id.clone(),
            workspace_id: workspace_id.clone(),
            root_generation: workspace.root.root_generation().to_owned(),
            owner_id: run.id.clone(),
            kind: LeaseKind::Run,
            daemon_generation: None,
            host_instance_id: None,
            acquired_at: acquired_at.clone(),
            expires_at: None,
            renewed_at: None,
            released_at: None,
        };
        if row_to_run(&run_row).map_err(|_| WorkspaceStoreError::Database)? != *run
            || after_workspace != workspace
            || after_allocation != allocation
            || lease.record != expected_lease
        {
            return Err(WorkspaceStoreError::CorruptRow {
                table: "workspace_leases",
                field: "row",
            });
        }
        tx.commit()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        Ok(RunAdmission::Admitted {
            lease_id: Some(lease_id),
        })
    }
}

#[cfg(test)]
#[rustfmt::skip]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use agent24_protocol::{RunInput, RunMode, RunStatus, Session, Usage};

    const WS: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    const TS: &str = "2026-09-19T00:00:00.000Z";
    const EXP: &str = "2026-09-19T00:01:00.000Z";
    fn lid(s:&str)->WorkspaceLeaseId{WorkspaceLeaseId::parse(s).unwrap()}
    fn run(id:&str,s:Option<&str>,w:Option<WorkspaceId>)->Run{Run{id:id.into(),session_id:s.map(str::to_owned),workspace_id:w.clone(),status:RunStatus::Queued,input:RunInput{prompt:"go".into(),workspace_id:w,model_override:None,mode:RunMode::Normal},output:None,error:None,usage:Usage::default(),schedule_id:None,created_at:TS.into(),started_at:None,ended_at:None}}
    async fn seed(store:&Store)->WorkspaceId{
        sqlx::raw_sql("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES ('ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','orchestrator_scratch','active','test','external','orchestrator','owner','serial','2026-09-19T00:00:00.000Z','2026-09-19T00:01:00.000Z',1,'/scratch','g1','unix',X'0101010101010101',X'0202020202020202'); INSERT INTO workspace_allocations (allocation_id,workspace_id,root_generation,relative_name,parent_identity_kind,parent_unix_device,parent_unix_inode,root_identity_kind,root_unix_device,root_unix_inode,phase,created_at) VALUES ('wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','g1','root','unix',X'0303030303030303',X'0404040404040404','unix',X'0101010101010101',X'0202020202020202','committed','2026-09-19T00:00:00.000Z');").execute(store.pool()).await.unwrap();WorkspaceId::parse(WS).unwrap()
    }
    async fn sess(store:&Store,id:&str,w:Option<WorkspaceId>){store.insert_session(&Session{id:id.into(),title:id.into(),channel:"desktop".into(),workspace_id:w,created_at:TS.into(),updated_at:TS.into()}).await.unwrap();}
    async fn counts(store:&Store)->(i64,i64){sqlx::query_as("SELECT (SELECT count(*) FROM runs),(SELECT count(*) FROM workspace_leases WHERE kind='run' AND released_at IS NULL)").fetch_one(store.pool()).await.unwrap()}

    #[tokio::test] async fn atomic_success_and_public_bypass(){let st=Store::open_memory().await.unwrap();let w=seed(&st).await;sess(&st,"s",Some(w.clone())).await;let r=run("r",Some("s"),Some(w));assert!(st.insert_run(&r).await.is_err());assert!(matches!(st.insert_run_with_workspace_admission(&r,Some(lid("wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6")),&WorkspaceInstant::parse(TS).unwrap()).await.unwrap(),RunAdmission::Admitted{lease_id:Some(_)}));assert_eq!(counts(&st).await,(1,1));}

    #[tokio::test] async fn binding_legacy_and_clock(){let st=Store::open_memory().await.unwrap();let w=seed(&st).await;sess(&st,"b",Some(w.clone())).await;sess(&st,"u",None).await;let now=WorkspaceInstant::parse(TS).unwrap();for r in [run("a",None,Some(w.clone())),run("b",Some("u"),Some(w.clone())),run("c",Some("b"),None)]{assert_eq!(st.insert_run_with_workspace_admission(&r,Some(lid("wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7")),&now).await.unwrap(),RunAdmission::Denied(RunAdmissionDenial::BindingConflict));}let mut old=run("old",Some("b"),Some(w));old.created_at="2026-09-18T23:59:59.000Z".into();assert_eq!(st.insert_run_with_workspace_admission(&old,Some(lid("wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7")),&WorkspaceInstant::parse(&old.created_at).unwrap()).await.unwrap(),RunAdmission::Denied(RunAdmissionDenial::WorkspaceUnavailable));let l=run("legacy",None,None);assert!(matches!(st.insert_run_with_workspace_admission(&l,None,&now).await.unwrap(),RunAdmission::Admitted{lease_id:None}));}

    #[tokio::test] async fn expiry_and_serial_busy(){let st=Store::open_memory().await.unwrap();let w=seed(&st).await;sess(&st,"s",Some(w.clone())).await;let mut e=run("e",Some("s"),Some(w.clone()));e.created_at=EXP.into();assert_eq!(st.insert_run_with_workspace_admission(&e,Some(lid("wl_01J5M4Q2Y7N8P9R0S1T2V3W4X8")),&WorkspaceInstant::parse(EXP).unwrap()).await.unwrap(),RunAdmission::Denied(RunAdmissionDenial::WorkspaceExpired));assert_eq!(counts(&st).await,(0,0));assert_eq!(st.get_workspace(&w).await.unwrap().state,"expired");let st=Store::open_memory().await.unwrap();let w=seed(&st).await;sess(&st,"s",Some(w.clone())).await;let now=WorkspaceInstant::parse(TS).unwrap();let a=run("a",Some("s"),Some(w.clone()));st.insert_run_with_workspace_admission(&a,Some(lid("wl_01J5M4Q2Y7N8P9R0S1T2V3W4X9")),&now).await.unwrap();let b=run("b",Some("s"),Some(w));assert_eq!(st.insert_run_with_workspace_admission(&b,Some(lid("wl_01J5M4Q2Y7N8P9R0S1T2V3W4XA")),&now).await.unwrap(),RunAdmission::Denied(RunAdmissionDenial::WorkspaceBusy));assert_eq!(counts(&st).await,(1,1));}
}
