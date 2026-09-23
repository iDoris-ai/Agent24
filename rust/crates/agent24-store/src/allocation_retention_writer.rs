#![allow(dead_code)]

use serde_json::json;
use sqlx::{Acquire, Row, Sqlite, Transaction};

use crate::{
    AllocationFailureReason, AllocationIntent, AllocationPhase, AllocationRecord, RootIdentity,
    Store, WorkspaceConflict, WorkspaceKind, WorkspaceResult, WorkspaceRow, WorkspaceState,
    WorkspaceStoreError,
};

const ACTOR: &str = "workspace_allocation";
const ACTION: &str = "workspace.allocation_retained";

#[derive(Clone)]
pub(crate) struct FrozenAllocation {
    record: AllocationRecord,
    source: AllocationPhase,
    reason: AllocationFailureReason,
    registry: Option<WorkspaceRow>,
    raw_detail: String,
    tail: crate::audit::StrictAuditTail,
    prospective: crate::audit::ProspectiveAuditTuple,
}

/// Immutable write decision; fresh and observed paths intentionally differ.
pub(crate) struct AllocationRetentionAttempt(Attempt);
enum Attempt {
    Fresh(Box<FrozenAllocation>),
    Replay {
        intent: AllocationIntent,
        reason: AllocationFailureReason,
        expectation: Box<crate::allocation_retention_expectation::AllocationRetentionExpectation>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AllocationRetentionResult {
    Applied,
    Observed,
}

fn conflict() -> WorkspaceStoreError {
    WorkspaceStoreError::Conflict(WorkspaceConflict::AllocationIdentifier)
}

fn phase(phase: AllocationPhase) -> &'static str {
    match phase {
        AllocationPhase::Reserved => "reserved",
        AllocationPhase::Materialized => "materialized",
        AllocationPhase::Committed => "committed",
        AllocationPhase::Retained => "retained",
    }
}

fn same_identity(a: &AllocationRecord, b: &AllocationRecord) -> bool {
    a.id() == b.id()
        && a.workspace_id() == b.workspace_id()
        && a.root_generation() == b.root_generation()
        && a.relative_name() == b.relative_name()
        && a.parent_identity() == b.parent_identity()
        && a.root_identity() == b.root_identity()
        && a.created_at() == b.created_at()
}

async fn allocation(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
) -> WorkspaceResult<AllocationRecord> {
    let row = sqlx::query(
        "SELECT * FROM workspace_allocations WHERE allocation_id=? COLLATE BINARY LIMIT 1",
    )
    .bind(intent.allocation_id().as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?
    .ok_or(WorkspaceStoreError::NotFound)?;
    let record = AllocationRecord::decode(&row)?;
    (record.id() == intent.allocation_id()
        && record.workspace_id() == intent.workspace_id()
        && record.root_generation() == intent.root_generation()
        && record.relative_name() == intent.relative_name()
        && record.parent_identity() == intent.parent_identity()
        && record.created_at() == intent.created_at())
    .then_some(record)
    .ok_or(conflict())
}

async fn registry(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
) -> WorkspaceResult<Option<WorkspaceRow>> {
    sqlx::query("SELECT * FROM workspaces WHERE id=? COLLATE BINARY LIMIT 1")
        .bind(intent.workspace_id().as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?
        .map(|row| WorkspaceRow::decode(&row))
        .transpose()
}

fn valid_source(record: &AllocationRecord, registry: Option<&WorkspaceRow>) -> WorkspaceResult<()> {
    match record.phase() {
        AllocationPhase::Reserved if record.root_identity().is_none() && registry.is_none() => {
            Ok(())
        }
        AllocationPhase::Materialized if record.root_identity().is_some() && registry.is_none() => {
            Ok(())
        }
        AllocationPhase::Committed
            if record.root_identity().is_some()
                && registry.is_some_and(|row| {
                    row.id == *record.workspace_id()
                        && row.kind == WorkspaceKind::OrchestratorScratch
                        && row.state == WorkspaceState::Active
                        && row.root.root_generation() == record.root_generation()
                        && Some(row.root.identity()) == record.root_identity()
                }) =>
        {
            Ok(())
        }
        _ => Err(conflict()),
    }
}

async fn no_retention_event(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
) -> WorkspaceResult<()> {
    let rows = sqlx::query("SELECT detail FROM audit_log WHERE actor='workspace_allocation' AND action='workspace.allocation_retained'")
        .fetch_all(&mut **tx).await.map_err(|_| WorkspaceStoreError::Database)?;
    rows.into_iter()
        .all(|row| {
            row.try_get::<String, _>("detail")
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .and_then(|value| {
                    value
                        .get("allocation_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .as_deref()
                != Some(intent.allocation_id().as_str())
        })
        .then_some(())
        .ok_or(conflict())
}

fn detail(
    record: &AllocationRecord,
    source: AllocationPhase,
    registry: Option<&WorkspaceRow>,
) -> WorkspaceResult<String> {
    let source = phase(source);
    let mut value = json!({"allocation_id":record.id().as_str(),"workspace_id":record.workspace_id().as_str(),"phase":"retained","source_phase":source});
    if let Some(row) = registry {
        value["workspace"] =
            serde_json::to_value(row.project()).map_err(|_| WorkspaceStoreError::Database)?;
    }
    serde_json::to_string(&value).map_err(|_| WorkspaceStoreError::Database)
}

struct RootColumns(
    Option<&'static str>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
);
fn columns(identity: Option<RootIdentity>) -> RootColumns {
    match identity {
        None => RootColumns(None, None, None, None, None),
        Some(RootIdentity::Unix { device, inode }) => RootColumns(
            Some("unix"),
            Some(device.to_vec()),
            Some(inode.to_vec()),
            None,
            None,
        ),
        Some(RootIdentity::Windows {
            volume_serial,
            file_id,
        }) => RootColumns(
            Some("windows"),
            None,
            None,
            Some(volume_serial.to_vec()),
            Some(file_id.to_vec()),
        ),
    }
}

async fn observed(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
    reason: &AllocationFailureReason,
) -> WorkspaceResult<()> {
    let evidence = Store::retained_allocation_evidence_tx(tx, intent).await?;
    (evidence.allocation.failure_reason() == Some(reason))
        .then_some(())
        .ok_or(conflict())
}

impl Store {
    /// Freeze one fresh retention or validate an already-applied exact replay.
    pub(crate) async fn prepare_allocation_retention_tx(
        tx: &mut Transaction<'_, Sqlite>,
        intent: &AllocationIntent,
        reason: AllocationFailureReason,
    ) -> WorkspaceResult<AllocationRetentionAttempt> {
        let record = allocation(tx, intent).await?;
        if record.phase() == AllocationPhase::Retained {
            observed(tx, intent, &reason).await?;
            let expectation = Self::retained_allocation_expectation_tx(tx, intent).await?;
            return Ok(AllocationRetentionAttempt(Attempt::Replay {
                intent: intent.clone(),
                reason,
                expectation: Box::new(expectation),
            }));
        }
        if record.failure_reason().is_some() {
            return Err(conflict());
        }
        let registry = registry(tx, intent).await?;
        valid_source(&record, registry.as_ref())?;
        no_retention_event(tx, intent).await?;
        let raw_detail = detail(&record, record.phase(), registry.as_ref())?;
        let tail = crate::audit::strict_audit_tail_tx(tx).await?;
        let prospective =
            tail.prospective(intent.created_at().as_str(), ACTOR, ACTION, &raw_detail)?;
        Ok(AllocationRetentionAttempt(Attempt::Fresh(Box::new(
            FrozenAllocation {
                source: record.phase(),
                record,
                reason,
                registry,
                raw_detail,
                tail,
                prospective,
            },
        ))))
    }

    /// Retain inside a savepoint only; callers decide their outer transaction boundary.
    pub(crate) async fn retain_allocation_tx(
        tx: &mut Transaction<'_, Sqlite>,
        attempt: &AllocationRetentionAttempt,
    ) -> WorkspaceResult<AllocationRetentionResult> {
        let mut savepoint = tx
            .begin()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        let result = retain_inner(&mut savepoint, attempt).await;
        match result {
            Ok(value) => {
                savepoint
                    .commit()
                    .await
                    .map_err(|_| WorkspaceStoreError::Database)?;
                Ok(value)
            }
            Err(error) => {
                savepoint
                    .rollback()
                    .await
                    .map_err(|_| WorkspaceStoreError::Database)?;
                Err(error)
            }
        }
    }
}

async fn retain_inner(
    tx: &mut Transaction<'_, Sqlite>,
    attempt: &AllocationRetentionAttempt,
) -> WorkspaceResult<AllocationRetentionResult> {
    match &attempt.0 {
        Attempt::Replay {
            intent,
            reason,
            expectation,
        } => {
            expectation.verify_replay_tx(tx, intent).await?;
            observed(tx, intent, reason).await?;
            Ok(AllocationRetentionResult::Observed)
        }
        Attempt::Fresh(fresh) => retain_fresh(tx, fresh).await,
    }
}

async fn retain_fresh(
    tx: &mut Transaction<'_, Sqlite>,
    fresh: &FrozenAllocation,
) -> WorkspaceResult<AllocationRetentionResult> {
    let intent = AllocationIntent::new(
        fresh.record.id().clone(),
        fresh.record.workspace_id().clone(),
        fresh.record.root_generation().into(),
        fresh.record.relative_name().into(),
        fresh.record.parent_identity(),
        fresh.record.created_at().clone(),
    )
    .map_err(|_| conflict())?;
    let current = allocation(tx, &intent).await?;
    if current.phase() == AllocationPhase::Retained {
        let evidence = Store::retained_allocation_evidence_tx(tx, &intent).await?;
        if evidence.source_phase == fresh.source
            && same_identity(&evidence.allocation, &fresh.record)
            && evidence.allocation.failure_reason() == Some(&fresh.reason)
            && evidence.workspace == fresh.registry
            && evidence.audit_raw_detail == fresh.raw_detail
        {
            crate::audit::verify_prospective_audit_tx(tx, &fresh.prospective).await?;
            return Ok(AllocationRetentionResult::Observed);
        }
        return Err(conflict());
    }
    if !same_identity(&current, &fresh.record)
        || current.phase() != fresh.source
        || current.failure_reason().is_some()
        || registry(tx, &intent).await? != fresh.registry
        || crate::audit::strict_audit_tail_tx(tx).await? != fresh.tail
    {
        return Err(conflict());
    }
    no_retention_event(tx, &intent).await?;
    let parent = columns(Some(fresh.record.parent_identity()));
    let root = columns(fresh.record.root_identity());
    let expected = fresh.registry.as_ref();
    let registry_root = columns(expected.map(|row| row.root.identity()));
    let result = sqlx::query(
        "UPDATE workspace_allocations SET phase='retained',failure_reason=?
         WHERE allocation_id=? COLLATE BINARY AND workspace_id=? COLLATE BINARY AND root_generation=? COLLATE BINARY
           AND relative_name=? COLLATE BINARY AND created_at=? COLLATE BINARY AND phase=? COLLATE BINARY
           AND parent_identity_kind IS ? AND parent_unix_device IS ? AND parent_unix_inode IS ? AND parent_windows_volume IS ? AND parent_windows_file_id IS ?
           AND root_identity_kind IS ? AND root_unix_device IS ? AND root_unix_inode IS ? AND root_windows_volume IS ? AND root_windows_file_id IS ? AND failure_reason IS NULL
           AND ((?=0 AND NOT EXISTS(SELECT 1 FROM workspaces WHERE id=? COLLATE BINARY)) OR (?=1 AND EXISTS(SELECT 1 FROM workspaces WHERE id=? COLLATE BINARY AND kind=? AND state='active' AND canonical_root=? AND root_generation=? AND root_identity_kind IS ? AND unix_device IS ? AND unix_inode IS ? AND windows_volume_serial IS ? AND windows_file_id IS ? AND lifecycle_owner_kind=? AND lifecycle_owner_ref=? AND writeback_policy=? AND concurrency_policy=? AND revision=?)))")
        .bind(fresh.reason.as_str()).bind(fresh.record.id().as_str()).bind(fresh.record.workspace_id().as_str())
        .bind(fresh.record.root_generation()).bind(fresh.record.relative_name()).bind(fresh.record.created_at().as_str()).bind(phase(fresh.source))
        .bind(parent.0).bind(parent.1).bind(parent.2).bind(parent.3).bind(parent.4)
        .bind(root.0).bind(root.1).bind(root.2).bind(root.3).bind(root.4)
        .bind(i64::from(expected.is_some())).bind(fresh.record.workspace_id().as_str())
        .bind(i64::from(expected.is_some())).bind(fresh.record.workspace_id().as_str())
        .bind(expected.map(|row| row.kind.as_str())).bind(expected.map(|row| row.root.canonical_root())).bind(expected.map(|row| row.root.root_generation()))
        .bind(registry_root.0).bind(registry_root.1).bind(registry_root.2).bind(registry_root.3).bind(registry_root.4)
        .bind(expected.map(|row| row.authority.lifecycle_owner_kind.as_str())).bind(expected.map(|row| row.authority.lifecycle_owner_ref.as_str()))
        .bind(expected.map(|row| row.authority.writeback_policy.as_str())).bind(expected.map(|row| row.authority.concurrency_policy.as_str())).bind(expected.map(|row| row.revision as i64))
        .execute(&mut **tx).await.map_err(|_| WorkspaceStoreError::Database)?;
    if result.rows_affected() != 1 {
        return Err(conflict());
    }
    let after = allocation(tx, &intent).await?;
    if !same_identity(&after, &fresh.record)
        || after.phase() != AllocationPhase::Retained
        || after.failure_reason() != Some(&fresh.reason)
        || registry(tx, &intent).await? != fresh.registry
        || crate::audit::strict_audit_tail_tx(tx).await? != fresh.tail
    {
        return Err(conflict());
    }
    crate::audit::append_prospective_audit_tx(tx, &fresh.prospective).await?;
    let evidence = Store::retained_allocation_evidence_tx(tx, &intent).await?;
    if evidence.source_phase != fresh.source
        || !same_identity(&evidence.allocation, &fresh.record)
        || evidence.allocation.failure_reason() != Some(&fresh.reason)
        || evidence.workspace != fresh.registry
        || fresh.raw_detail != evidence.audit_raw_detail
    {
        return Err(conflict());
    }
    crate::audit::verify_prospective_tail_tx(tx, &fresh.prospective).await?;
    Ok(AllocationRetentionResult::Applied)
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
    use std::{
        sync::{Arc, Condvar, Mutex},
        time::Duration,
    };

    const GATE_WAIT: Duration = Duration::from_secs(5);

    #[derive(Default)]
    struct GateState {
        entered: bool,
        released: bool,
        expired: bool,
    }

    /// A connection-local SQLite callback gate. The deadline makes a failed
    /// cancellation test bounded without relying on scheduler sleeps.
    struct RetentionGate {
        state: Mutex<GateState>,
        changed: Condvar,
    }

    impl RetentionGate {
        fn enter(&self) {
            let mut state = self.state.lock().unwrap();
            state.entered = true;
            self.changed.notify_all();
            while !state.released {
                let (next, timeout) = self.changed.wait_timeout(state, GATE_WAIT).unwrap();
                state = next;
                if timeout.timed_out() {
                    state.expired = true;
                    self.changed.notify_all();
                    return;
                }
            }
        }

        fn wait_until_entered(&self) -> bool {
            let state = self.state.lock().unwrap();
            let (state, _) = self
                .changed
                .wait_timeout_while(state, GATE_WAIT, |state| !state.entered)
                .unwrap();
            state.entered && !state.expired
        }

        fn assert_not_expired(&self) {
            assert!(
                !self.state.lock().unwrap().expired,
                "retention gate watchdog expired"
            );
        }

        fn release(&self) {
            let mut state = self.state.lock().unwrap();
            state.released = true;
            self.changed.notify_all();
        }
    }

    /// Ensure an assertion or early return cannot strand SQLite's callback
    /// thread while this test owns the only in-memory connection.
    struct GateRelease(Arc<RetentionGate>);

    impl Drop for GateRelease {
        fn drop(&mut self) {
            self.0.release();
        }
    }

    #[rustfmt::skip]
    fn intent(parent: RootIdentity) -> AllocationIntent { AllocationIntent::new(AllocationId::parse("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(), WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(), "generation-1".into(), "scratch".into(), parent, WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap()).unwrap() }
    #[rustfmt::skip]
    async fn source(store: &Store, input: &AllocationIntent, phase: AllocationPhase, root: RootIdentity) {
        store.reserve_workspace_allocation(input).await.unwrap(); if phase != AllocationPhase::Reserved { store.materialize_workspace_allocation(input, root).await.unwrap(); }
        if phase == AllocationPhase::Committed { let owner = LifecycleOwnerRef::parse("owner".into()).unwrap(); let row = NewScratchWorkspace::new(input.workspace_id().clone(), TrustedRootRegistration::new("/scratch".into(), input.root_generation().into(), root).unwrap(), WorkspaceProvenanceInput::new("test".into(),None,None).unwrap(),owner.clone(),WorkspaceTtl::new(60_000).unwrap()); store.register_materialized_scratch(input,&row,&owner,&WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap()).await.unwrap(); }
    }
    #[rustfmt::skip]
    async fn prepare(store: &Store, input: &AllocationIntent, reason: &str) -> AllocationRetentionAttempt { let mut tx=store.pool().begin().await.unwrap(); let value=Store::prepare_allocation_retention_tx(&mut tx,input,AllocationFailureReason::parse(reason).unwrap()).await.unwrap(); tx.commit().await.unwrap(); value }

    #[tokio::test]
    #[rustfmt::skip]
    async fn applies_all_sources_and_root_shapes_then_observes_retry() {
        for (phase,root) in [(AllocationPhase::Reserved,RootIdentity::unix(&[3;8],&[4;8]).unwrap()),(AllocationPhase::Materialized,RootIdentity::windows(&[3;8],&[4;16]).unwrap()),(AllocationPhase::Committed,RootIdentity::unix(&[3;8],&[4;8]).unwrap())] { let store=Store::open_memory().await.unwrap(); let input=intent(RootIdentity::unix(&[1;8],&[2;8]).unwrap()); source(&store,&input,phase,root).await; let attempt=prepare(&store,&input,"io_error").await; let mut tx=store.pool().begin().await.unwrap(); assert_eq!(Store::retain_allocation_tx(&mut tx,&attempt).await.unwrap(),AllocationRetentionResult::Applied); tx.commit().await.unwrap(); store.append_audit("2026-09-19T00:00:01.000Z","other","other",&serde_json::json!({})).await.unwrap(); let mut tx=store.pool().begin().await.unwrap(); assert_eq!(Store::retain_allocation_tx(&mut tx,&attempt).await.unwrap(),AllocationRetentionResult::Observed); tx.commit().await.unwrap(); }
    }

    #[tokio::test]
    #[rustfmt::skip]
    async fn stale_inputs_and_post_audit_trigger_rollback_savepoint_only() {
        for sql in ["UPDATE workspace_allocations SET root_unix_device=X'0909090909090909'", "UPDATE workspace_allocations SET phase='retained',failure_reason='other'", "CREATE TRIGGER bad AFTER INSERT ON audit_log WHEN NEW.action='workspace.allocation_retained' BEGIN UPDATE workspace_allocations SET failure_reason='other'; END"] { let store=Store::open_memory().await.unwrap(); let input=intent(RootIdentity::unix(&[1;8],&[2;8]).unwrap()); source(&store,&input,AllocationPhase::Materialized,RootIdentity::unix(&[3;8],&[4;8]).unwrap()).await; let attempt=prepare(&store,&input,"io_error").await; sqlx::query(sql).execute(crate::test_hooks::pool(&store)).await.unwrap(); let mut tx=store.pool().begin().await.unwrap(); assert!(Store::retain_allocation_tx(&mut tx,&attempt).await.is_err()); assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM audit_log").fetch_one(&mut *tx).await.unwrap(),2); tx.commit().await.unwrap(); }
        let store=Store::open_memory().await.unwrap(); let input=intent(RootIdentity::unix(&[1;8],&[2;8]).unwrap()); source(&store,&input,AllocationPhase::Committed,RootIdentity::unix(&[3;8],&[4;8]).unwrap()).await; let attempt=prepare(&store,&input,"io_error").await; sqlx::query("CREATE TRIGGER mutate BEFORE UPDATE OF phase ON workspace_allocations WHEN NEW.phase='retained' BEGIN UPDATE workspaces SET revision=2; END").execute(crate::test_hooks::pool(&store)).await.unwrap(); sqlx::query("CREATE TRIGGER restore AFTER INSERT ON audit_log WHEN NEW.action='workspace.allocation_retained' BEGIN UPDATE workspaces SET revision=1; END").execute(crate::test_hooks::pool(&store)).await.unwrap(); let mut tx=store.pool().begin().await.unwrap(); assert!(Store::retain_allocation_tx(&mut tx,&attempt).await.is_err()); assert_eq!(sqlx::query_scalar::<_,i64>("SELECT revision FROM workspaces").fetch_one(&mut *tx).await.unwrap(),1); tx.commit().await.unwrap();
        let store=Store::open_memory().await.unwrap(); let input=intent(RootIdentity::unix(&[1;8],&[2;8]).unwrap()); source(&store,&input,AllocationPhase::Reserved,RootIdentity::unix(&[3;8],&[4;8]).unwrap()).await; let attempt=prepare(&store,&input,"io_error").await; store.append_audit("2026-09-19T00:00:01.000Z","other","other",&serde_json::json!({})).await.unwrap(); let mut tx=store.pool().begin().await.unwrap(); assert!(Store::retain_allocation_tx(&mut tx,&attempt).await.is_err()); tx.commit().await.unwrap();
        let store=Store::open_memory().await.unwrap(); let input=intent(RootIdentity::unix(&[1;8],&[2;8]).unwrap()); source(&store,&input,AllocationPhase::Materialized,RootIdentity::unix(&[3;8],&[4;8]).unwrap()).await; let attempt=prepare(&store,&input,"io_error").await; sqlx::query("CREATE TRIGGER lower AFTER UPDATE OF phase ON workspace_allocations WHEN NEW.phase='retained' BEGIN UPDATE sqlite_sequence SET seq=0 WHERE name='audit_log'; END").execute(crate::test_hooks::pool(&store)).await.unwrap(); let mut tx=store.pool().begin().await.unwrap(); assert!(Store::retain_allocation_tx(&mut tx,&attempt).await.is_err()); assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM audit_log").fetch_one(&mut *tx).await.unwrap(),2); assert_eq!(sqlx::query_scalar::<_,i64>("SELECT seq FROM sqlite_sequence WHERE name='audit_log'").fetch_one(&mut *tx).await.unwrap(),2); assert_eq!(sqlx::query_scalar::<_,String>("SELECT phase||COALESCE(failure_reason,'') FROM workspace_allocations").fetch_one(&mut *tx).await.unwrap(),"materialized"); tx.commit().await.unwrap();
    }

    #[tokio::test]
    #[rustfmt::skip]
    async fn outer_rollback_discards_success() { let store=Store::open_memory().await.unwrap(); let input=intent(RootIdentity::unix(&[1;8],&[2;8]).unwrap()); source(&store,&input,AllocationPhase::Reserved,RootIdentity::unix(&[3;8],&[4;8]).unwrap()).await; let attempt=prepare(&store,&input,"io_error").await; let mut tx=store.pool().begin().await.unwrap(); Store::retain_allocation_tx(&mut tx,&attempt).await.unwrap(); tx.rollback().await.unwrap(); assert_eq!(store.get_workspace_allocation(input.allocation_id()).await.unwrap().phase(),AllocationPhase::Reserved); }

    #[tokio::test]
    #[rustfmt::skip]
    async fn frozen_replay_rejects_mutated_retained_root() { let store=Store::open_memory().await.unwrap(); let input=intent(RootIdentity::unix(&[1;8],&[2;8]).unwrap()); source(&store,&input,AllocationPhase::Materialized,RootIdentity::unix(&[3;8],&[4;8]).unwrap()).await; let fresh=prepare(&store,&input,"io_error").await; let mut tx=store.pool().begin().await.unwrap(); Store::retain_allocation_tx(&mut tx,&fresh).await.unwrap(); tx.commit().await.unwrap(); let replay=prepare(&store,&input,"io_error").await; sqlx::query("UPDATE workspace_allocations SET root_unix_device=X'0909090909090909'").execute(crate::test_hooks::pool(&store)).await.unwrap(); let mut tx=store.pool().begin().await.unwrap(); assert!(Store::retain_allocation_tx(&mut tx,&replay).await.is_err()); tx.commit().await.unwrap(); }

    #[derive(Debug, PartialEq, Eq)]
    struct RawSnapshot {
        allocations: Vec<Vec<String>>,
        registry: Vec<Vec<String>>,
        audit: Vec<Vec<String>>,
        high_water: Vec<Vec<String>>,
    }

    /// Return every stored value as SQLite's `quote(value), typeof(value)` pair.
    /// This deliberately avoids decoded model values: trigger rollback must restore
    /// storage classes as well as the semantic rows consumed by the writer.
    async fn raw_table(tx: &mut Transaction<'_, Sqlite>, table: &str) -> Vec<Vec<String>> {
        let columns = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(&mut **tx)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.try_get::<String, _>("name").unwrap())
            .collect::<Vec<_>>();
        let fields = columns
            .iter()
            .map(|name| format!("quote(\"{name}\"), typeof(\"{name}\")"))
            .collect::<Vec<_>>()
            .join(", ");
        sqlx::query(&format!("SELECT {fields} FROM \"{table}\" ORDER BY rowid"))
            .fetch_all(&mut **tx)
            .await
            .unwrap()
            .into_iter()
            .map(|row| {
                (0..row.len())
                    .map(|index| row.try_get::<String, _>(index).unwrap())
                    .collect()
            })
            .collect()
    }

    async fn raw_snapshot_tx(tx: &mut Transaction<'_, Sqlite>) -> RawSnapshot {
        RawSnapshot {
            allocations: raw_table(tx, "workspace_allocations").await,
            registry: raw_table(tx, "workspaces").await,
            audit: raw_table(tx, "audit_log").await,
            high_water: raw_table(tx, "sqlite_sequence").await,
        }
    }

    async fn raw_snapshot(store: &Store) -> RawSnapshot {
        let mut tx = store.pool().begin().await.unwrap();
        let snapshot = raw_snapshot_tx(&mut tx).await;
        tx.commit().await.unwrap();
        snapshot
    }

    async fn caller_sentinels(tx: &mut Transaction<'_, Sqlite>) -> String {
        sqlx::query(
            "SELECT group_concat(marker, '|')
             FROM (SELECT marker FROM retention_caller_sentinels ORDER BY rowid)",
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap()
        .try_get(0)
        .unwrap()
    }

    #[tokio::test]
    async fn adversarial_retention_triggers_rollback_raw_storage_and_keep_caller_usable() {
        const AFTER_ALLOCATION_UPDATE: &str =
            "AFTER UPDATE OF phase ON workspace_allocations WHEN NEW.phase='retained'";
        const AFTER_AUDIT_INSERT: &str =
            "AFTER INSERT ON audit_log WHEN NEW.action='workspace.allocation_retained'";

        // The same destructive matrix runs at each writer boundary.  The update
        // boundary targets the pre-existing audit tail; the insert boundary uses
        // the freshly inserted `NEW` row where that makes the attack sharper.
        for (boundary, event, cases) in [
            (
                "update",
                AFTER_ALLOCATION_UPDATE,
                [
                    (
                        "allocation_mutate",
                        "UPDATE workspace_allocations SET relative_name='tampered' WHERE allocation_id=NEW.allocation_id",
                    ),
                    (
                        "allocation_delete",
                        "DELETE FROM workspace_allocations WHERE allocation_id=NEW.allocation_id",
                    ),
                    (
                        "registry_mutate",
                        "UPDATE workspaces SET lifecycle_owner_ref='tampered' WHERE id=NEW.workspace_id",
                    ),
                    (
                        "registry_delete",
                        "DELETE FROM workspaces WHERE id=NEW.workspace_id",
                    ),
                    (
                        "audit_tail_mutate",
                        "UPDATE audit_log SET hash='tampered' WHERE seq=(SELECT max(seq) FROM audit_log)",
                    ),
                    (
                        "audit_tail_delete",
                        "DELETE FROM audit_log WHERE seq=(SELECT max(seq) FROM audit_log)",
                    ),
                    (
                        "high_water_mutate",
                        "UPDATE sqlite_sequence SET seq=0 WHERE name='audit_log'",
                    ),
                    (
                        "high_water_delete",
                        "DELETE FROM sqlite_sequence WHERE name='audit_log'",
                    ),
                ],
            ),
            (
                "audit",
                AFTER_AUDIT_INSERT,
                [
                    (
                        "allocation_mutate",
                        "UPDATE workspace_allocations SET relative_name='tampered' WHERE allocation_id=(SELECT json_extract(NEW.detail, '$.allocation_id'))",
                    ),
                    (
                        "allocation_delete",
                        "DELETE FROM workspace_allocations WHERE allocation_id=(SELECT json_extract(NEW.detail, '$.allocation_id'))",
                    ),
                    (
                        "registry_mutate",
                        "UPDATE workspaces SET lifecycle_owner_ref='tampered' WHERE id=(SELECT json_extract(NEW.detail, '$.workspace_id'))",
                    ),
                    (
                        "registry_delete",
                        "DELETE FROM workspaces WHERE id=(SELECT json_extract(NEW.detail, '$.workspace_id'))",
                    ),
                    (
                        "audit_tail_mutate",
                        "UPDATE audit_log SET hash='tampered' WHERE seq=NEW.seq",
                    ),
                    (
                        "audit_tail_delete",
                        "DELETE FROM audit_log WHERE seq=NEW.seq",
                    ),
                    (
                        "high_water_mutate",
                        "UPDATE sqlite_sequence SET seq=0 WHERE name='audit_log'; UPDATE audit_log SET seq=seq+1 WHERE seq=NEW.seq",
                    ),
                    (
                        "high_water_delete",
                        "DELETE FROM sqlite_sequence WHERE name='audit_log'; UPDATE audit_log SET seq=seq+1 WHERE seq=NEW.seq",
                    ),
                ],
            ),
        ] {
            for (case, body) in cases {
                let store = Store::open_memory().await.unwrap();
                let input = intent(RootIdentity::unix(&[1; 8], &[2; 8]).unwrap());
                source(
                    &store,
                    &input,
                    AllocationPhase::Committed,
                    RootIdentity::unix(&[3; 8], &[4; 8]).unwrap(),
                )
                .await;
                let attempt = prepare(&store, &input, "io_error").await;
                let trigger_name = format!("retention_{boundary}_{case}");
                sqlx::query(&format!(
                    "CREATE TRIGGER {trigger_name} {event} BEGIN {body}; END"
                ))
                .execute(crate::test_hooks::pool(&store))
                .await
                .unwrap();
                sqlx::query("CREATE TEMP TABLE retention_caller_sentinels (marker TEXT NOT NULL)")
                    .execute(crate::test_hooks::pool(&store))
                    .await
                    .unwrap();

                let before = raw_snapshot(&store).await;
                let mut tx = store.pool().begin().await.unwrap();
                sqlx::query("INSERT INTO retention_caller_sentinels(marker) VALUES ('before')")
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                assert!(
                    Store::retain_allocation_tx(&mut tx, &attempt)
                        .await
                        .is_err(),
                    "{boundary}/{case} must be rejected"
                );
                assert_eq!(raw_snapshot_tx(&mut tx).await, before, "{boundary}/{case}");
                assert_eq!(
                    caller_sentinels(&mut tx).await,
                    "before",
                    "{boundary}/{case}"
                );

                sqlx::query("INSERT INTO retention_caller_sentinels(marker) VALUES ('after')")
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                assert_eq!(
                    caller_sentinels(&mut tx).await,
                    "before|after",
                    "{boundary}/{case}"
                );
                tx.commit().await.unwrap();

                assert_eq!(raw_snapshot(&store).await, before, "{boundary}/{case}");
                let mut proof = store.pool().begin().await.unwrap();
                assert_eq!(
                    caller_sentinels(&mut proof).await,
                    "before|after",
                    "{boundary}/{case}"
                );
                proof.commit().await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn cancellation_during_retention_update_rolls_back_savepoint_and_keeps_outer_usable() {
        let store = Store::open_memory().await.unwrap();
        let input = intent(RootIdentity::unix(&[1; 8], &[2; 8]).unwrap());
        source(
            &store,
            &input,
            AllocationPhase::Materialized,
            RootIdentity::unix(&[3; 8], &[4; 8]).unwrap(),
        )
        .await;
        let attempt = prepare(&store, &input, "io_error").await;
        let before = raw_snapshot(&store).await;

        let gate = Arc::new(RetentionGate {
            state: Mutex::new(GateState::default()),
            changed: Condvar::new(),
        });
        let release = GateRelease(gate.clone());
        let mut connection = store.pool().acquire().await.unwrap();
        let callback_gate = gate.clone();
        connection
            .lock_handle()
            .await
            .unwrap()
            .create_collation("retention_gate", move |left, right| {
                callback_gate.enter();
                left.cmp(right)
            })
            .unwrap();
        sqlx::query(
            "CREATE TEMP TRIGGER retention_gate_update
             AFTER UPDATE OF phase ON workspace_allocations
             WHEN NEW.phase='retained'
             BEGIN
                 SELECT NEW.allocation_id COLLATE retention_gate = NEW.workspace_id;
             END",
        )
        .execute(&mut *connection)
        .await
        .unwrap();

        let mut tx = connection.begin().await.unwrap();
        sqlx::query("CREATE TEMP TABLE retention_caller_sentinels (marker TEXT NOT NULL)")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("INSERT INTO retention_caller_sentinels(marker) VALUES ('before')")
            .execute(&mut *tx)
            .await
            .unwrap();

        let entered_gate = gate.clone();
        let entered = tokio::task::spawn_blocking(move || entered_gate.wait_until_entered());
        let mut retain = Box::pin(Store::retain_allocation_tx(&mut tx, &attempt));
        tokio::select! {
            result = &mut retain => panic!("retention finished before the gate: {result:?}"),
            result = entered => assert!(matches!(result, Ok(true)), "retention gate was not entered"),
        }
        gate.assert_not_expired();
        drop(retain);
        drop(release);

        // This waits behind the released callback and proves SQLx finished the
        // cancellation cleanup before the caller reuses its outer transaction.
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT 1")
                .fetch_one(&mut *tx)
                .await
                .unwrap(),
            1
        );
        assert_eq!(raw_snapshot_tx(&mut tx).await, before);
        assert_eq!(caller_sentinels(&mut tx).await, "before");

        sqlx::query("INSERT INTO retention_caller_sentinels(marker) VALUES ('after')")
            .execute(&mut *tx)
            .await
            .unwrap();
        assert_eq!(caller_sentinels(&mut tx).await, "before|after");
        tx.commit().await.unwrap();
        drop(connection);

        assert_eq!(raw_snapshot(&store).await, before);
        let mut proof = store.pool().begin().await.unwrap();
        assert_eq!(caller_sentinels(&mut proof).await, "before|after");
        proof.commit().await.unwrap();
    }

    #[tokio::test]
    async fn file_wal_frozen_retention_loser_retries_observed_after_winner_outer_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("retention-wal.sqlite");
        let seed = Store::open(&path).await.unwrap();
        let input = intent(RootIdentity::unix(&[1; 8], &[2; 8]).unwrap());
        let reason = AllocationFailureReason::parse("io_error").unwrap();
        source(
            &seed,
            &input,
            AllocationPhase::Committed,
            RootIdentity::unix(&[3; 8], &[4; 8]).unwrap(),
        )
        .await;
        let before = raw_snapshot(&seed).await;
        drop(seed);

        // Separate Store pools plus held PoolConnections make these two physical
        // WAL connections explicit.  No later operation may select another one.
        let left = Store::open(&path).await.unwrap();
        let right = Store::open(&path).await.unwrap();
        let mut left_connection = left.pool().acquire().await.unwrap();
        let mut right_connection = right.pool().acquire().await.unwrap();
        for connection in [&mut left_connection, &mut right_connection] {
            sqlx::query("PRAGMA busy_timeout=0")
                .execute(&mut **connection)
                .await
                .unwrap();
        }

        let mut prepare_left = left_connection.begin().await.unwrap();
        let left_attempt =
            Store::prepare_allocation_retention_tx(&mut prepare_left, &input, reason.clone())
                .await
                .unwrap();
        prepare_left.commit().await.unwrap();
        let mut prepare_right = right_connection.begin().await.unwrap();
        let right_attempt =
            Store::prepare_allocation_retention_tx(&mut prepare_right, &input, reason.clone())
                .await
                .unwrap();
        prepare_right.commit().await.unwrap();
        let (prospective, tail) = match &left_attempt.0 {
            Attempt::Fresh(fresh) => (fresh.prospective.clone(), fresh.tail.clone()),
            Attempt::Replay { .. } => panic!("both attempts must freeze before any write"),
        };
        match &right_attempt.0 {
            Attempt::Fresh(fresh) => {
                assert_eq!(fresh.prospective, prospective);
                assert!(fresh.tail == tail);
            }
            Attempt::Replay { .. } => panic!("both attempts must freeze before any write"),
        }

        let gate = Arc::new(RetentionGate {
            state: Mutex::new(GateState::default()),
            changed: Condvar::new(),
        });
        let release = GateRelease(gate.clone());
        let callback_gate = gate.clone();
        left_connection
            .lock_handle()
            .await
            .unwrap()
            .create_collation("retention_wal_gate", move |left, right| {
                callback_gate.enter();
                left.cmp(right)
            })
            .unwrap();
        sqlx::query(
            "CREATE TEMP TRIGGER retention_wal_gate_update
             AFTER UPDATE OF phase ON workspace_allocations
             WHEN NEW.phase='retained'
             BEGIN
                 SELECT NEW.allocation_id COLLATE retention_wal_gate = NEW.workspace_id;
             END",
        )
        .execute(&mut *left_connection)
        .await
        .unwrap();

        let mut winner = left_connection.begin().await.unwrap();
        let mut loser = right_connection.begin().await.unwrap();
        sqlx::query("CREATE TEMP TABLE retention_caller_sentinels (marker TEXT NOT NULL)")
            .execute(&mut *loser)
            .await
            .unwrap();
        sqlx::query("INSERT INTO retention_caller_sentinels(marker) VALUES ('before')")
            .execute(&mut *loser)
            .await
            .unwrap();
        // Establish B's WAL read snapshot while A has not started its update.
        assert_eq!(raw_snapshot_tx(&mut loser).await, before);

        let entered_gate = gate.clone();
        let entered = tokio::task::spawn_blocking(move || entered_gate.wait_until_entered());
        let mut retain = Box::pin(Store::retain_allocation_tx(&mut winner, &left_attempt));
        tokio::select! {
            result = &mut retain => panic!("winner finished before the update gate: {result:?}"),
            result = entered => assert!(matches!(result, Ok(true)), "winner did not reach allocation update"),
        }
        gate.assert_not_expired();

        assert_eq!(
            Store::retain_allocation_tx(&mut loser, &right_attempt).await,
            Err(WorkspaceStoreError::Database),
            "a stale WAL reader must fail only when its frozen fresh write promotes"
        );
        gate.assert_not_expired();
        assert_eq!(raw_snapshot_tx(&mut loser).await, before);
        assert_eq!(caller_sentinels(&mut loser).await, "before");
        sqlx::query("INSERT INTO retention_caller_sentinels(marker) VALUES ('after')")
            .execute(&mut *loser)
            .await
            .unwrap();
        assert_eq!(caller_sentinels(&mut loser).await, "before|after");
        loser.commit().await.unwrap();

        drop(release);
        assert_eq!(
            retain.await.unwrap(),
            AllocationRetentionResult::Applied,
            "the gated winner owns only its savepoint"
        );
        gate.assert_not_expired();

        // A successful savepoint is not durable: the caller's outer transaction
        // has not committed, so B observes the exact pre-commit bytes.
        let mut precommit_observer = right_connection.begin().await.unwrap();
        assert_eq!(
            caller_sentinels(&mut precommit_observer).await,
            "before|after"
        );
        assert_eq!(raw_snapshot_tx(&mut precommit_observer).await, before);
        precommit_observer.commit().await.unwrap();
        winner.commit().await.unwrap();

        let mut retry = right_connection.begin().await.unwrap();
        let retry_before = raw_snapshot_tx(&mut retry).await;
        assert_eq!(
            Store::retain_allocation_tx(&mut retry, &right_attempt)
                .await
                .unwrap(),
            AllocationRetentionResult::Observed
        );
        assert_eq!(raw_snapshot_tx(&mut retry).await, retry_before);
        retry.commit().await.unwrap();

        drop(left_connection);
        drop(right_connection);
        drop(left);
        drop(right);
        let reopened = Store::open(&path).await.unwrap();
        let mut final_tx = reopened.pool().begin().await.unwrap();
        crate::audit::verify_prospective_audit_tx(&mut final_tx, &prospective)
            .await
            .unwrap();
        crate::audit::verify_prospective_tail_tx(&mut final_tx, &prospective)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT seq FROM sqlite_sequence WHERE name='audit_log'")
                .fetch_one(&mut *final_tx)
                .await
                .unwrap(),
            prospective.seq()
        );
        final_tx.commit().await.unwrap();
        let durable = raw_snapshot(&reopened).await;
        assert_eq!(durable, retry_before);
        assert_eq!(durable.registry, before.registry);
        assert_eq!(
            durable.high_water,
            vec![vec![
                "'audit_log'".into(),
                "text".into(),
                prospective.seq().to_string(),
                "integer".into(),
            ]]
        );
        assert_eq!(
            reopened
                .get_workspace_allocation(input.allocation_id())
                .await
                .unwrap()
                .phase(),
            AllocationPhase::Retained
        );
        assert!(
            reopened
                .get_workspace_allocation(input.allocation_id())
                .await
                .unwrap()
                .failure_reason()
                == Some(&reason)
        );
        assert_eq!(
            reopened
                .list_audit()
                .await
                .unwrap()
                .iter()
                .filter(|entry| entry.action == ACTION)
                .count(),
            1
        );
    }
}
