use super::*;
use crate::{
    AllocationId, RootIdentity, TrustedRootRegistration, WorkspaceProvenanceInput, WorkspaceTtl,
};
use agent24_protocol::WorkspaceId;
use sqlx::Row;
use std::{path::Path, sync::Arc};
use tokio::sync::Barrier;

const AID: &str = "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const WID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const CREATED: &str = "2026-09-19T00:00:00.000Z";
const NOW: &str = "2026-09-19T00:00:01.000Z";

fn now() -> WorkspaceInstant {
    WorkspaceInstant::parse(NOW).unwrap()
}

fn intent(
    workspace: &str,
    generation: &str,
    name: &str,
    parent: RootIdentity,
    created: &str,
) -> AllocationIntent {
    AllocationIntent::new(
        AllocationId::parse(AID).unwrap(),
        WorkspaceId::parse(workspace).unwrap(),
        generation.into(),
        name.into(),
        parent,
        WorkspaceInstant::parse(created).unwrap(),
    )
    .unwrap()
}

fn scratch(
    id: &str,
    generation: &str,
    root: RootIdentity,
    owner: LifecycleOwnerRef,
) -> NewScratchWorkspace {
    NewScratchWorkspace::new(
        WorkspaceId::parse(id).unwrap(),
        TrustedRootRegistration::new("/scratch".into(), generation.into(), root).unwrap(),
        WorkspaceProvenanceInput::new("test".into(), None, None).unwrap(),
        owner,
        WorkspaceTtl::new(60_000).unwrap(),
    )
}

fn expected() -> AllocationIntent {
    intent(
        WID,
        "generation-1",
        "scratch",
        RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
        CREATED,
    )
}

fn values() -> (
    AllocationIntent,
    NewScratchWorkspace,
    LifecycleOwnerRef,
    RootIdentity,
) {
    let root = RootIdentity::unix(&[3; 8], &[4; 8]).unwrap();
    let intent = expected();
    let owner = LifecycleOwnerRef::parse("orchestrator-1".into()).unwrap();
    (
        intent,
        scratch(WID, "generation-1", root, owner.clone()),
        owner,
        root,
    )
}

async fn materialized(
    store: &Store,
) -> (
    AllocationIntent,
    NewScratchWorkspace,
    LifecycleOwnerRef,
    RootIdentity,
) {
    let (intent, input, owner, root) = values();
    store.reserve_workspace_allocation(&intent).await.unwrap();
    store
        .materialize_workspace_allocation(&intent, root)
        .await
        .unwrap();
    (intent, input, owner, root)
}

async fn baseline(store: &Store) -> (String, String, Vec<crate::AuditEntry>) {
    let allocations = sqlx::query_scalar(
        "SELECT coalesce(group_concat(quote(allocation_id)||quote(workspace_id)||quote(root_generation)||quote(relative_name)||quote(parent_identity_kind)||quote(parent_unix_device)||quote(parent_unix_inode)||quote(parent_windows_volume)||quote(parent_windows_file_id)||quote(root_identity_kind)||quote(root_unix_device)||quote(root_unix_inode)||quote(root_windows_volume)||quote(root_windows_file_id)||quote(phase)||quote(created_at)||quote(failure_reason)), '') FROM workspace_allocations",
    )
    .fetch_one(crate::test_hooks::pool(store))
    .await
    .unwrap();
    let workspaces = sqlx::query_scalar(
        "SELECT coalesce(group_concat(quote(id)||quote(kind)||quote(state)||quote(provenance_source)||quote(provenance_project_ref)||quote(provenance_base_revision)||quote(writeback_policy)||quote(lifecycle_owner_kind)||quote(lifecycle_owner_ref)||quote(concurrency_policy)||quote(created_at)||quote(expires_at)||quote(renewed_at)||quote(released_at)||quote(revision)||quote(canonical_root)||quote(root_generation)||quote(root_identity_kind)||quote(unix_device)||quote(unix_inode)||quote(windows_volume_serial)||quote(windows_file_id)||quote(quarantine_root)||quote(quarantined_at)||quote(cleanup_attempts)||quote(cleanup_last_attempt_at)||quote(cleanup_error)||quote(cleanup_retry_at)), '') FROM workspaces",
    )
    .fetch_one(crate::test_hooks::pool(store))
    .await
    .unwrap();
    (allocations, workspaces, store.list_audit().await.unwrap())
}

async fn unchanged(store: &Store, before: &(String, String, Vec<crate::AuditEntry>)) {
    assert_eq!(baseline(store).await, *before);
}

#[tokio::test]
async fn entry_and_time_table_rejects_every_mismatch_without_writes() {
    for case in [
        "owner",
        "workspace",
        "generation",
        "parent",
        "relative",
        "created",
        "root",
    ] {
        let store = Store::open_memory().await.unwrap();
        let (_, input, owner, root) = materialized(&store).await;
        let wrong = LifecycleOwnerRef::parse("other-orchestrator".into()).unwrap();
        let (call_intent, call_input, call_owner) = match case {
            "owner" => (expected(), input, wrong),
            "workspace" => (
                expected(),
                scratch(
                    "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
                    "generation-1",
                    root,
                    owner.clone(),
                ),
                owner,
            ),
            "generation" => (
                expected(),
                scratch(WID, "generation-2", root, owner.clone()),
                owner,
            ),
            "parent" => (
                intent(
                    WID,
                    "generation-1",
                    "scratch",
                    RootIdentity::unix(&[9; 8], &[10; 8]).unwrap(),
                    CREATED,
                ),
                input,
                owner,
            ),
            "relative" => (
                intent(
                    WID,
                    "generation-1",
                    "other",
                    RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
                    CREATED,
                ),
                input,
                owner,
            ),
            "created" => (
                intent(
                    WID,
                    "generation-1",
                    "scratch",
                    RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
                    NOW,
                ),
                input,
                owner,
            ),
            "root" => (
                expected(),
                scratch(
                    WID,
                    "generation-1",
                    RootIdentity::unix(&[11; 8], &[12; 8]).unwrap(),
                    owner.clone(),
                ),
                owner,
            ),
            _ => unreachable!(),
        };
        let before = baseline(&store).await;
        assert!(
            store
                .register_materialized_scratch(&call_intent, &call_input, &call_owner, &now())
                .await
                .is_err(),
            "{case}"
        );
        unchanged(&store, &before).await;
    }
    for (phase, moment) in [
        ("reserved", NOW),
        ("retained", NOW),
        ("materialized", "2026-09-18T23:59:59.999Z"),
        ("materialized", "2026-09-19T00:01:00.000Z"),
    ] {
        let store = Store::open_memory().await.unwrap();
        let (intent, input, owner, _) = materialized(&store).await;
        if phase == "reserved" {
            sqlx::query("UPDATE workspace_allocations SET phase='reserved', root_identity_kind=NULL, root_unix_device=NULL, root_unix_inode=NULL WHERE allocation_id=?").bind(intent.allocation_id().as_str()).execute(crate::test_hooks::pool(&store)).await.unwrap();
        } else if phase == "retained" {
            sqlx::query("UPDATE workspace_allocations SET phase='retained', failure_reason='retained' WHERE allocation_id=?").bind(intent.allocation_id().as_str()).execute(crate::test_hooks::pool(&store)).await.unwrap();
        }
        let before = baseline(&store).await;
        assert!(
            store
                .register_materialized_scratch(
                    &intent,
                    &input,
                    &owner,
                    &WorkspaceInstant::parse(moment).unwrap()
                )
                .await
                .is_err(),
            "{phase}:{moment}"
        );
        unchanged(&store, &before).await;
    }
}

#[tokio::test]
async fn insert_commit_and_audit_triggers_roll_back_the_complete_baseline() {
    for (name, event, body) in [
        (
            "ignore_insert",
            "BEFORE INSERT ON workspaces",
            "SELECT RAISE(IGNORE)",
        ),
        (
            "alter_workspace",
            "AFTER INSERT ON workspaces",
            "UPDATE workspaces SET canonical_root='/tampered' WHERE id=NEW.id",
        ),
        (
            "alter_journal",
            "AFTER INSERT ON workspaces",
            "UPDATE workspace_allocations SET relative_name='tampered' WHERE workspace_id=NEW.id",
        ),
        (
            "alter_commit",
            "AFTER UPDATE OF phase ON workspace_allocations WHEN NEW.phase='committed'",
            "UPDATE workspace_allocations SET relative_name='tampered' WHERE allocation_id=NEW.allocation_id",
        ),
        (
            "alter_audit",
            "AFTER INSERT ON audit_log WHEN NEW.action='workspace.allocation_committed'",
            "UPDATE audit_log SET detail=detail||' ' WHERE seq=NEW.seq",
        ),
    ] {
        let store = Store::open_memory().await.unwrap();
        let (intent, input, owner, _) = materialized(&store).await;
        let before = baseline(&store).await;
        sqlx::query(&format!("CREATE TRIGGER {name} {event} BEGIN {body}; END"))
            .execute(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert!(
            store
                .register_materialized_scratch(&intent, &input, &owner, &now())
                .await
                .is_err(),
            "{name}"
        );
        unchanged(&store, &before).await;
    }
}

#[tokio::test]
async fn committed_replay_requires_the_initial_exact_active_row() {
    let store = Store::open_memory().await.unwrap();
    let (intent, input, owner, _) = materialized(&store).await;
    assert!(matches!(
        store
            .register_materialized_scratch(&intent, &input, &owner, &now())
            .await
            .unwrap(),
        RegistrationOutcome::Registered { .. }
    ));
    let audit = store.list_audit().await.unwrap();
    assert!(matches!(
        store
            .register_materialized_scratch(&intent, &input, &owner, &now())
            .await
            .unwrap(),
        RegistrationOutcome::AlreadyCommitted { .. }
    ));
    assert_eq!(store.list_audit().await.unwrap(), audit);
    let wrong = scratch(
        WID,
        "generation-1",
        RootIdentity::unix(&[9; 8], &[10; 8]).unwrap(),
        owner.clone(),
    );
    assert!(matches!(
        store
            .register_materialized_scratch(&intent, &wrong, &owner, &now())
            .await,
        Err(WorkspaceStoreError::Conflict(
            crate::WorkspaceConflict::RootIdentity
        ))
    ));
    for sql in [
        "DELETE FROM workspaces WHERE id='ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'",
        "UPDATE workspaces SET revision=2 WHERE id='ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'",
        "UPDATE workspaces SET state='expired' WHERE id='ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'",
        "UPDATE workspaces SET renewed_at='2026-09-19T00:00:02.000Z' WHERE id='ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'",
    ] {
        let store = Store::open_memory().await.unwrap();
        let (intent, input, owner, _) = materialized(&store).await;
        store
            .register_materialized_scratch(&intent, &input, &owner, &now())
            .await
            .unwrap();
        sqlx::query(sql)
            .execute(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        let before = baseline(&store).await;
        assert!(matches!(
            store
                .register_materialized_scratch(&intent, &input, &owner, &now())
                .await,
            Err(WorkspaceStoreError::CorruptRow { .. })
        ));
        unchanged(&store, &before).await;
    }
}

async fn file_materialized(
    path: &Path,
) -> (
    Store,
    AllocationIntent,
    NewScratchWorkspace,
    LifecycleOwnerRef,
) {
    let store = Store::open(path).await.unwrap();
    let (intent, input, owner, root) = values();
    store.reserve_workspace_allocation(&intent).await.unwrap();
    store
        .materialize_workspace_allocation(&intent, root)
        .await
        .unwrap();
    (store, intent, input, owner)
}

#[tokio::test]
async fn file_wal_race_commits_once_and_reopens_one_workspace_and_audit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registration.sqlite");
    let (seed, intent, input, owner) = file_materialized(&path).await;
    drop(seed);
    let left = Store::open(&path).await.unwrap();
    let right = Store::open(&path).await.unwrap();
    let gate = Arc::new(Barrier::new(2));
    let (left_gate, right_gate) = (Arc::clone(&gate), Arc::clone(&gate));
    let (first, second) = tokio::join!(
        async {
            left_gate.wait().await;
            left.register_materialized_scratch(&intent, &input, &owner, &now())
                .await
                .unwrap()
        },
        async {
            right_gate.wait().await;
            right
                .register_materialized_scratch(&intent, &input, &owner, &now())
                .await
                .unwrap()
        },
    );
    assert!(matches!(
        (first, second),
        (
            RegistrationOutcome::Registered { .. },
            RegistrationOutcome::AlreadyCommitted { .. }
        ) | (
            RegistrationOutcome::AlreadyCommitted { .. },
            RegistrationOutcome::Registered { .. }
        )
    ));
    let reopened = Store::open(&path).await.unwrap();
    let counts: (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM workspaces), (SELECT count(*) FROM audit_log WHERE action='workspace.allocation_committed')").fetch_one(crate::test_hooks::pool(&reopened)).await.unwrap();
    assert_eq!(counts, (1, 1));
    reopened.verify_audit_chain().await.unwrap();
}

#[tokio::test]
async fn rejected_commit_reopens_materialized_without_workspace_or_audit_then_retries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("commit-rejected.sqlite");
    let (store, intent, input, owner) = file_materialized(&path).await;
    let before = baseline(&store).await;
    let mut pinned = Vec::new();
    for _ in 0..4 {
        pinned.push(crate::test_hooks::pool(&store).acquire().await.unwrap());
    }
    let mut connection = crate::test_hooks::pool(&store).acquire().await.unwrap();
    connection
        .lock_handle()
        .await
        .unwrap()
        .set_commit_hook(|| false);
    drop(connection);
    assert_eq!(
        store
            .register_materialized_scratch(&intent, &input, &owner, &now())
            .await
            .err(),
        Some(WorkspaceStoreError::Database)
    );
    drop(pinned);
    crate::test_hooks::pool(&store).close().await;
    let reopened = Store::open(&path).await.unwrap();
    unchanged(&reopened, &before).await;
    assert!(matches!(
        reopened
            .register_materialized_scratch(&intent, &input, &owner, &now())
            .await
            .unwrap(),
        RegistrationOutcome::Registered { .. }
    ));
}

#[tokio::test]
async fn windows_identity_blobs_are_preserved_in_database_rows() {
    let store = Store::open_memory().await.unwrap();
    let parent = RootIdentity::windows(&[11; 8], &[12; 16]).unwrap();
    let root = RootIdentity::windows(&[13; 8], &[14; 16]).unwrap();
    let intent = intent(WID, "generation-1", "windows", parent, CREATED);
    let owner = LifecycleOwnerRef::parse("orchestrator-1".into()).unwrap();
    let input = scratch(WID, "generation-1", root, owner.clone());
    store.reserve_workspace_allocation(&intent).await.unwrap();
    store
        .materialize_workspace_allocation(&intent, root)
        .await
        .unwrap();
    store
        .register_materialized_scratch(&intent, &input, &owner, &now())
        .await
        .unwrap();
    let row = sqlx::query("SELECT root_identity_kind, root_unix_device, root_unix_inode, root_windows_volume, root_windows_file_id, parent_identity_kind, parent_unix_device, parent_unix_inode, parent_windows_volume, parent_windows_file_id FROM workspace_allocations WHERE allocation_id=?").bind(intent.allocation_id().as_str()).fetch_one(crate::test_hooks::pool(&store)).await.unwrap();
    assert_eq!(
        (
            row.get::<String, _>(0),
            row.get::<Option<Vec<u8>>, _>(1),
            row.get::<Option<Vec<u8>>, _>(2),
            row.get::<Option<Vec<u8>>, _>(3),
            row.get::<Option<Vec<u8>>, _>(4)
        ),
        (
            "windows".into(),
            None,
            None,
            Some(vec![13; 8]),
            Some(vec![14; 16])
        )
    );
    let workspace: (String, Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>) = sqlx::query_as("SELECT root_identity_kind, unix_device, unix_inode, windows_volume_serial, windows_file_id FROM workspaces WHERE id=?").bind(intent.workspace_id().as_str()).fetch_one(crate::test_hooks::pool(&store)).await.unwrap();
    assert_eq!(
        workspace,
        (
            "windows".into(),
            None,
            None,
            Some(vec![13; 8]),
            Some(vec![14; 16])
        )
    );
}
