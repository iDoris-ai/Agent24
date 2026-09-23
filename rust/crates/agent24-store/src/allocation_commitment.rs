#![allow(dead_code, clippy::type_complexity, clippy::items_after_test_module)]
// Dormant until the future registration writer adopts this transaction helper.

use serde_json::json;
use sqlx::{Sqlite, Transaction};

use crate::{
    AllocationIntent, AllocationPhase, AllocationRecord, RootIdentity, WorkspaceKind,
    WorkspaceResult, WorkspaceState, WorkspaceStoreError,
};

fn bad() -> WorkspaceStoreError {
    WorkspaceStoreError::Database
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
    use std::{path::Path, sync::Arc};
    use tokio::sync::Barrier;

    fn intent() -> AllocationIntent {
        AllocationIntent::new(
            crate::AllocationId::parse("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
            WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
            "generation-1".into(),
            "scratch".into(),
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
            WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap(),
        )
        .unwrap()
    }

    async fn fixture() -> (crate::Store, AllocationIntent, RootIdentity) {
        let store = crate::Store::open_memory().await.unwrap();
        let intent = intent();
        store.reserve_workspace_allocation(&intent).await.unwrap();
        let root = RootIdentity::unix(&[3; 8], &[4; 8]).unwrap();
        let ws = NewScratchWorkspace::new(
            intent.workspace_id().clone(),
            TrustedRootRegistration::new("/scratch".into(), intent.root_generation().into(), root)
                .unwrap(),
            WorkspaceProvenanceInput::new("test".into(), None, None).unwrap(),
            LifecycleOwnerRef::parse("test-owner".into()).unwrap(),
            WorkspaceTtl::new(60_000).unwrap(),
        );
        store
            .create_workspace(&ws, ws.lifecycle_owner_ref(), intent.created_at())
            .await
            .unwrap();
        sqlx::query("UPDATE workspace_allocations SET phase='materialized', root_identity_kind='unix', root_unix_device=X'0303030303030303', root_unix_inode=X'0404040404040404' WHERE allocation_id=?")
            .bind(intent.allocation_id().as_str()).execute(crate::test_hooks::pool(&store)).await.unwrap();
        (store, intent, root)
    }

    async fn fixture_as(
        parent: RootIdentity,
        root: RootIdentity,
    ) -> (crate::Store, AllocationIntent, RootIdentity) {
        let store = crate::Store::open_memory().await.unwrap();
        let mut intent = intent();
        intent = AllocationIntent::new(
            intent.allocation_id().clone(),
            intent.workspace_id().clone(),
            intent.root_generation().into(),
            intent.relative_name().into(),
            parent,
            intent.created_at().clone(),
        )
        .unwrap();
        store.reserve_workspace_allocation(&intent).await.unwrap();
        let ws = NewScratchWorkspace::new(
            intent.workspace_id().clone(),
            TrustedRootRegistration::new(
                "C:/scratch".into(),
                intent.root_generation().into(),
                root,
            )
            .unwrap(),
            WorkspaceProvenanceInput::new("test".into(), None, None).unwrap(),
            LifecycleOwnerRef::parse("test-owner".into()).unwrap(),
            WorkspaceTtl::new(60_000).unwrap(),
        );
        store
            .create_workspace(&ws, ws.lifecycle_owner_ref(), intent.created_at())
            .await
            .unwrap();
        let (kind, unix_device, unix_inode, windows_volume, windows_file_id) = match root {
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
        sqlx::query("UPDATE workspace_allocations SET phase='materialized', root_identity_kind=?, root_unix_device=?, root_unix_inode=?, root_windows_volume=?, root_windows_file_id=? WHERE allocation_id=?")
            .bind(kind).bind(unix_device).bind(unix_inode).bind(windows_volume).bind(windows_file_id)
            .bind(intent.allocation_id().as_str()).execute(crate::test_hooks::pool(&store)).await.unwrap();
        (store, intent, root)
    }

    async fn baseline(
        store: &crate::Store,
        intent: &AllocationIntent,
    ) -> (String, String, Vec<crate::AuditEntry>) {
        let allocation: (String, Option<String>, String) = sqlx::query_as("SELECT phase, failure_reason, relative_name FROM workspace_allocations WHERE allocation_id=?")
            .bind(intent.allocation_id().as_str()).fetch_one(crate::test_hooks::pool(store)).await.unwrap();
        let workspace: (String, String, i64) =
            sqlx::query_as("SELECT canonical_root, state, revision FROM workspaces WHERE id=?")
                .bind(intent.workspace_id().as_str())
                .fetch_one(crate::test_hooks::pool(store))
                .await
                .unwrap();
        (
            format!("{allocation:?}"),
            format!("{workspace:?}"),
            store.list_audit().await.unwrap(),
        )
    }

    async fn assert_baseline(
        store: &crate::Store,
        intent: &AllocationIntent,
        before: &(String, String, Vec<crate::AuditEntry>),
    ) {
        assert_eq!(baseline(store, intent).await, *before);
    }

    async fn drop_trigger(store: &crate::Store, name: &str) {
        sqlx::query(&format!("DROP TRIGGER {name}"))
            .execute(crate::test_hooks::pool(store))
            .await
            .unwrap();
    }

    fn trigger(name: &str, event: &str, body: &str) -> String {
        format!("CREATE TRIGGER {name} {event} BEGIN {body}; END")
    }

    async fn install(store: &crate::Store, sql: String) {
        sqlx::query(&sql)
            .execute(crate::test_hooks::pool(store))
            .await
            .unwrap();
    }

    async fn success(store: &crate::Store, intent: &AllocationIntent, root: RootIdentity) {
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert_eq!(
            commit_materialized_allocation_tx(&mut tx, intent, root)
                .await
                .unwrap()
                .phase(),
            AllocationPhase::Committed
        );
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn commits_once_and_exact_replay_adds_no_audit() {
        let (store, intent, root) = fixture().await;
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert!(matches!(
            commit_materialized_allocation_tx(
                &mut tx,
                &intent,
                RootIdentity::unix(&[9; 8], &[10; 8]).unwrap()
            )
            .await,
            Err(WorkspaceStoreError::Conflict(
                crate::WorkspaceConflict::RootIdentity
            ))
        ));
        drop(tx);
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        let record = commit_materialized_allocation_tx(&mut tx, &intent, root)
            .await
            .unwrap();
        assert_eq!(record.phase(), AllocationPhase::Committed);
        tx.commit().await.unwrap();
        let count = store.list_audit().await.unwrap().len();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        commit_materialized_allocation_tx(&mut tx, &intent, root)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(store.list_audit().await.unwrap().len(), count);
    }

    #[tokio::test]
    async fn audit_tampering_fails_and_rolls_back_commit() {
        let (store, intent, root) = fixture().await;
        let pool = crate::test_hooks::pool(&store);
        sqlx::query("CREATE TRIGGER alter_commit_audit AFTER INSERT ON audit_log WHEN NEW.action='workspace.allocation_committed' BEGIN UPDATE audit_log SET detail='{}' WHERE seq=NEW.seq; END")
            .execute(pool).await.unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert!(matches!(
            commit_materialized_allocation_tx(&mut tx, &intent, root).await,
            Err(WorkspaceStoreError::Database)
        ));
        drop(tx);
        assert_eq!(
            store
                .get_workspace_allocation(intent.allocation_id())
                .await
                .unwrap()
                .phase(),
            AllocationPhase::Materialized
        );
        assert_eq!(store.list_audit().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn legal_workspace_trigger_mutation_rolls_back_all_three_tables() {
        let (store, intent, root) = fixture().await;
        let pool = crate::test_hooks::pool(&store);
        sqlx::query("CREATE TRIGGER allocation_changes_workspace AFTER UPDATE OF phase ON workspace_allocations WHEN NEW.phase='committed' BEGIN UPDATE workspaces SET canonical_root='/other-root' WHERE id='ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'; END")
            .execute(pool).await.unwrap();
        let before: (String, i64, String) = sqlx::query_as(
            "SELECT canonical_root, revision, lifecycle_owner_ref FROM workspaces WHERE id=?",
        )
        .bind(intent.workspace_id().as_str())
        .fetch_one(pool)
        .await
        .unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert!(matches!(
            commit_materialized_allocation_tx(&mut tx, &intent, root).await,
            Err(WorkspaceStoreError::Database)
        ));
        drop(tx);
        let after: (String, i64, String) = sqlx::query_as(
            "SELECT canonical_root, revision, lifecycle_owner_ref FROM workspaces WHERE id=?",
        )
        .bind(intent.workspace_id().as_str())
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(after, before);
        assert_eq!(
            store
                .get_workspace_allocation(intent.allocation_id())
                .await
                .unwrap()
                .phase(),
            AllocationPhase::Materialized
        );
        assert_eq!(store.list_audit().await.unwrap().len(), 1);
        sqlx::query("DROP TRIGGER allocation_changes_workspace")
            .execute(pool)
            .await
            .unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        commit_materialized_allocation_tx(&mut tx, &intent, root)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn first_commit_requires_active_workspace_but_replay_allows_lifecycle_change() {
        let (store, intent, root) = fixture().await;
        sqlx::query("UPDATE workspaces SET state='expired' WHERE id=?")
            .bind(intent.workspace_id().as_str())
            .execute(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert!(matches!(
            commit_materialized_allocation_tx(&mut tx, &intent, root).await,
            Err(WorkspaceStoreError::Conflict(_))
        ));
        drop(tx);
        sqlx::query("UPDATE workspaces SET state='active' WHERE id=?")
            .bind(intent.workspace_id().as_str())
            .execute(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        commit_materialized_allocation_tx(&mut tx, &intent, root)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        sqlx::query("UPDATE workspaces SET state='expired' WHERE id=?")
            .bind(intent.workspace_id().as_str())
            .execute(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert_eq!(
            commit_materialized_allocation_tx(&mut tx, &intent, root)
                .await
                .unwrap()
                .phase(),
            AllocationPhase::Committed
        );
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn reserved_and_retained_phases_are_rejected() {
        let (store, intent, root) = fixture().await;
        let pool = crate::test_hooks::pool(&store);
        sqlx::query("UPDATE workspace_allocations SET phase='reserved', root_identity_kind=NULL, root_unix_device=NULL, root_unix_inode=NULL")
            .execute(pool).await.unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert!(matches!(
            commit_materialized_allocation_tx(&mut tx, &intent, root).await,
            Err(WorkspaceStoreError::Conflict(_))
        ));
        drop(tx);
        sqlx::query("UPDATE workspace_allocations SET phase='retained', root_identity_kind='unix', root_unix_device=X'0303030303030303', root_unix_inode=X'0404040404040404', failure_reason='retained'")
            .execute(pool).await.unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert!(matches!(
            commit_materialized_allocation_tx(&mut tx, &intent, root).await,
            Err(WorkspaceStoreError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn allocation_root_collision_blocks_commit() {
        let (store, intent, root) = fixture().await;
        let competitor = AllocationIntent::new(
            crate::AllocationId::parse("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X6").unwrap(),
            WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6").unwrap(),
            "generation-1".into(),
            "other-scratch".into(),
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
            intent.created_at().clone(),
        )
        .unwrap();
        store
            .reserve_workspace_allocation(&competitor)
            .await
            .unwrap();
        sqlx::query("UPDATE workspace_allocations SET phase='materialized', root_identity_kind='unix', root_unix_device=X'0303030303030303', root_unix_inode=X'0404040404040404' WHERE allocation_id=?")
            .bind(competitor.allocation_id().as_str()).execute(crate::test_hooks::pool(&store)).await.unwrap();
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert!(matches!(
            commit_materialized_allocation_tx(&mut tx, &intent, root).await,
            Err(WorkspaceStoreError::Conflict(
                crate::WorkspaceConflict::RootIdentity
            ))
        ));
    }

    #[tokio::test]
    async fn windows_commit_and_replay_preserve_blobs_and_audit_chain() {
        let parent = RootIdentity::windows(&[11; 8], &[12; 16]).unwrap();
        let root = RootIdentity::windows(&[13; 8], &[14; 16]).unwrap();
        let (store, intent, root) = fixture_as(parent, root).await;
        success(&store, &intent, root).await;
        success(&store, &intent, root).await;
        let (kind, ud, ui, volume, file, parent_kind, pud, pui, pvolume, pfile): (String, Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>, String, Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>) = sqlx::query_as("SELECT root_identity_kind, root_unix_device, root_unix_inode, root_windows_volume, root_windows_file_id, parent_identity_kind, parent_unix_device, parent_unix_inode, parent_windows_volume, parent_windows_file_id FROM workspace_allocations WHERE allocation_id=?")
            .bind(intent.allocation_id().as_str()).fetch_one(crate::test_hooks::pool(&store)).await.unwrap();
        assert_eq!(
            (kind.as_str(), ud, ui, volume, file),
            ("windows", None, None, Some(vec![13; 8]), Some(vec![14; 16]))
        );
        assert_eq!(
            (parent_kind.as_str(), pud, pui, pvolume, pfile),
            ("windows", None, None, Some(vec![11; 8]), Some(vec![12; 16]))
        );
        let audit = store.list_audit().await.unwrap();
        assert_eq!(audit.len(), 2);
        let commit = audit.last().unwrap();
        assert_eq!(
            (commit.actor.as_str(), commit.action.as_str()),
            ("workspace_allocation", "workspace.allocation_committed")
        );
        assert_eq!(
            commit.detail,
            json!({"allocation_id": intent.allocation_id().as_str(), "workspace_id": intent.workspace_id().as_str(), "phase": "committed"})
        );
        assert_eq!(commit.prev_hash, audit[0].hash);
    }

    #[tokio::test]
    async fn ignored_cas_is_database_error_and_can_retry() {
        let (store, intent, root) = fixture().await;
        let before = baseline(&store, &intent).await;
        install(
            &store,
            trigger(
                "ignore_commit",
                "BEFORE UPDATE OF phase ON workspace_allocations WHEN NEW.phase='committed'",
                "SELECT RAISE(IGNORE)",
            ),
        )
        .await;
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert_eq!(
            commit_materialized_allocation_tx(&mut tx, &intent, root)
                .await
                .err()
                .unwrap(),
            WorkspaceStoreError::Database
        );
        drop(tx);
        assert_baseline(&store, &intent, &before).await;
        drop_trigger(&store, "ignore_commit").await;
        success(&store, &intent, root).await;
        assert_eq!(store.list_audit().await.unwrap().len(), before.2.len() + 1);
    }

    #[tokio::test]
    async fn phase_trigger_corrupting_allocation_rolls_back_and_retries() {
        let (store, intent, root) = fixture().await;
        let before = baseline(&store, &intent).await;
        install(&store, trigger("alter_allocation", "AFTER UPDATE OF phase ON workspace_allocations WHEN NEW.phase='committed'", "UPDATE workspace_allocations SET relative_name='tampered' WHERE allocation_id=NEW.allocation_id")).await;
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert_eq!(
            commit_materialized_allocation_tx(&mut tx, &intent, root)
                .await
                .err()
                .unwrap(),
            WorkspaceStoreError::Database
        );
        drop(tx);
        assert_baseline(&store, &intent, &before).await;
        drop_trigger(&store, "alter_allocation").await;
        success(&store, &intent, root).await;
    }

    #[tokio::test]
    async fn audit_triggers_are_verified_and_rollback_every_table() {
        for (name, body) in [
            (
                "audit_allocation",
                "UPDATE workspace_allocations SET relative_name='tampered' WHERE allocation_id='wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5'",
            ),
            (
                "audit_workspace",
                "UPDATE workspaces SET canonical_root='/tampered' WHERE id='ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'",
            ),
            (
                "audit_competitor",
                "UPDATE workspace_allocations SET phase='materialized',root_identity_kind='unix',root_unix_device=X'0303030303030303',root_unix_inode=X'0404040404040404' WHERE allocation_id='wa_01J5M4Q2Y7N8P9R0S1T2V3W4X6'",
            ),
            (
                "audit_detail",
                "UPDATE audit_log SET detail=detail||' ' WHERE seq=NEW.seq",
            ),
        ] {
            let (store, intent, root) = fixture().await;
            if name == "audit_competitor" {
                let other = AllocationIntent::new(
                    crate::AllocationId::parse("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X6").unwrap(),
                    WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6").unwrap(),
                    "generation-2".into(),
                    "other".into(),
                    RootIdentity::unix(&[7; 8], &[8; 8]).unwrap(),
                    intent.created_at().clone(),
                )
                .unwrap();
                store.reserve_workspace_allocation(&other).await.unwrap();
            }
            let before = baseline(&store, &intent).await;
            install(
                &store,
                trigger(
                    name,
                    "AFTER INSERT ON audit_log WHEN NEW.action='workspace.allocation_committed'",
                    body,
                ),
            )
            .await;
            let mut tx = store.begin_workspace_immediate().await.unwrap();
            assert_eq!(
                commit_materialized_allocation_tx(&mut tx, &intent, root)
                    .await
                    .err()
                    .unwrap(),
                WorkspaceStoreError::Database,
                "{name}"
            );
            drop(tx);
            assert_baseline(&store, &intent, &before).await;
            if name == "audit_competitor" {
                let row: (String, Option<String>, Option<Vec<u8>>) = sqlx::query_as("SELECT phase, root_identity_kind, root_unix_device FROM workspace_allocations WHERE allocation_id='wa_01J5M4Q2Y7N8P9R0S1T2V3W4X6'")
                    .fetch_one(crate::test_hooks::pool(&store)).await.unwrap();
                assert_eq!(row, ("reserved".into(), None, None));
            }
            drop_trigger(&store, name).await;
            success(&store, &intent, root).await;
        }
    }

    #[tokio::test]
    async fn caller_rollback_discards_helper_success_and_allows_retry() {
        let (store, intent, root) = fixture().await;
        let before = baseline(&store, &intent).await;
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        assert_eq!(
            commit_materialized_allocation_tx(&mut tx, &intent, root)
                .await
                .unwrap()
                .phase(),
            AllocationPhase::Committed
        );
        tx.rollback().await.unwrap();
        assert_baseline(&store, &intent, &before).await;
        success(&store, &intent, root).await;
    }

    #[tokio::test]
    async fn caller_commit_rejection_rolls_back_and_retry_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commit-reject.db");
        let (store, intent, root) = file_fixture(&path).await;
        let before = baseline(&store, &intent).await;
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
        let mut tx = store.begin_workspace_immediate().await.unwrap();
        commit_materialized_allocation_tx(&mut tx, &intent, root)
            .await
            .unwrap();
        assert!(tx.commit().await.is_err());
        drop(pinned);
        crate::test_hooks::pool(&store).close().await;
        let reopened = crate::Store::open(&path).await.unwrap();
        assert_baseline(&reopened, &intent, &before).await;
        success(&reopened, &intent, root).await;
    }

    async fn file_fixture(path: &Path) -> (crate::Store, AllocationIntent, RootIdentity) {
        let store = crate::Store::open(path).await.unwrap();
        let intent = intent();
        store.reserve_workspace_allocation(&intent).await.unwrap();
        let root = RootIdentity::unix(&[3; 8], &[4; 8]).unwrap();
        let ws = NewScratchWorkspace::new(
            intent.workspace_id().clone(),
            TrustedRootRegistration::new("/scratch".into(), intent.root_generation().into(), root)
                .unwrap(),
            WorkspaceProvenanceInput::new("test".into(), None, None).unwrap(),
            LifecycleOwnerRef::parse("test-owner".into()).unwrap(),
            WorkspaceTtl::new(60_000).unwrap(),
        );
        store
            .create_workspace(&ws, ws.lifecycle_owner_ref(), intent.created_at())
            .await
            .unwrap();
        sqlx::query("UPDATE workspace_allocations SET phase='materialized', root_identity_kind='unix', root_unix_device=X'0303030303030303', root_unix_inode=X'0404040404040404' WHERE allocation_id=?").bind(intent.allocation_id().as_str()).execute(crate::test_hooks::pool(&store)).await.unwrap();
        (store, intent, root)
    }

    #[tokio::test]
    async fn file_wal_same_intent_race_commits_once_and_reopens_durably() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("allocation.db");
        let (seed, intent, root) = file_fixture(&path).await;
        drop(seed);
        let left = crate::Store::open(&path).await.unwrap();
        let right = crate::Store::open(&path).await.unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let a = Arc::clone(&barrier);
        let b = Arc::clone(&barrier);
        let (l, r) = tokio::join!(
            async {
                a.wait().await;
                let mut tx = left.begin_workspace_immediate().await.unwrap();
                let result = commit_materialized_allocation_tx(&mut tx, &intent, root)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
                result
            },
            async {
                b.wait().await;
                let mut tx = right.begin_workspace_immediate().await.unwrap();
                let result = commit_materialized_allocation_tx(&mut tx, &intent, root)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
                result
            },
        );
        assert_eq!(l.phase(), AllocationPhase::Committed);
        assert_eq!(r.phase(), AllocationPhase::Committed);
        let reopened = crate::Store::open(&path).await.unwrap();
        assert_eq!(
            reopened
                .get_workspace_allocation(intent.allocation_id())
                .await
                .unwrap()
                .phase(),
            AllocationPhase::Committed
        );
        let audit = reopened.list_audit().await.unwrap();
        assert_eq!(audit.len(), 2);
        assert_eq!(audit[1].action, "workspace.allocation_committed");
        assert_eq!(audit[1].prev_hash, audit[0].hash);
    }
}

fn columns(
    identity: RootIdentity,
) -> (
    &'static str,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
) {
    match identity {
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
    }
}

async fn read(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
) -> WorkspaceResult<(AllocationRecord, crate::WorkspaceRow)> {
    let row =
        sqlx::query("SELECT * FROM workspace_allocations WHERE allocation_id = ? COLLATE BINARY")
            .bind(intent.allocation_id().as_str())
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| bad())?
            .ok_or(bad())?;
    let allocation = AllocationRecord::decode(&row).map_err(|_| bad())?;
    if allocation.id() != intent.allocation_id()
        || allocation.workspace_id() != intent.workspace_id()
        || allocation.root_generation() != intent.root_generation()
        || allocation.relative_name() != intent.relative_name()
        || allocation.parent_identity() != intent.parent_identity()
        || allocation.created_at() != intent.created_at()
    {
        return Err(bad());
    }
    let row = sqlx::query("SELECT * FROM workspaces WHERE id = ? COLLATE BINARY AND root_generation = ? COLLATE BINARY")
        .bind(intent.workspace_id().as_str()).bind(intent.root_generation()).fetch_optional(&mut **tx).await.map_err(|_| bad())?.ok_or(bad())?;
    let workspace = crate::WorkspaceRow::decode(&row).map_err(|_| bad())?;
    if workspace.id != *intent.workspace_id()
        || workspace.kind != WorkspaceKind::OrchestratorScratch
        || workspace.root.root_generation() != intent.root_generation()
    {
        return Err(bad());
    }
    Ok((allocation, workspace))
}

async fn collision(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
    root: RootIdentity,
) -> WorkspaceResult<bool> {
    let (kind, ud, ui, wv, wf) = columns(root);
    let a: Option<i64> = sqlx::query_scalar("SELECT 1 FROM workspace_allocations WHERE allocation_id <> ? COLLATE BINARY AND root_identity_kind = ? COLLATE BINARY AND root_unix_device IS ? AND root_unix_inode IS ? AND root_windows_volume IS ? AND root_windows_file_id IS ? LIMIT 1")
        .bind(intent.allocation_id().as_str()).bind(kind).bind(ud.clone()).bind(ui.clone()).bind(wv.clone()).bind(wf.clone())
        .fetch_optional(&mut **tx).await.map_err(|_| bad())?;
    let w: Option<i64> = sqlx::query_scalar("SELECT 1 FROM workspaces WHERE id <> ? COLLATE BINARY AND root_identity_kind = ? COLLATE BINARY AND unix_device IS ? AND unix_inode IS ? AND windows_volume_serial IS ? AND windows_file_id IS ? LIMIT 1")
        .bind(intent.workspace_id().as_str()).bind(kind).bind(ud).bind(ui).bind(wv).bind(wf)
        .fetch_optional(&mut **tx).await.map_err(|_| bad())?;
    Ok(a.is_some() || w.is_some())
}

/// Commit a matching materialized allocation and already inserted scratch workspace.
/// The caller owns the surrounding BEGIN IMMEDIATE and commits only after this returns.
pub(crate) async fn commit_materialized_allocation_tx(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &AllocationIntent,
    root: RootIdentity,
) -> WorkspaceResult<AllocationRecord> {
    let (record, workspace) = read(tx, intent).await?;
    if workspace.root.identity() != root {
        return Err(WorkspaceStoreError::Conflict(
            crate::WorkspaceConflict::RootIdentity,
        ));
    }
    if record.phase() == AllocationPhase::Committed {
        if record.root_identity() != Some(root) {
            return Err(WorkspaceStoreError::Conflict(
                crate::WorkspaceConflict::RootIdentity,
            ));
        }
        return Ok(record);
    }
    if record.phase() != AllocationPhase::Materialized
        || record.root_identity() != Some(root)
        || workspace.state != WorkspaceState::Active
    {
        return Err(WorkspaceStoreError::Conflict(
            crate::WorkspaceConflict::AllocationIdentifier,
        ));
    }
    let original_workspace = workspace.clone();
    if collision(tx, intent, root).await? {
        return Err(WorkspaceStoreError::Conflict(
            crate::WorkspaceConflict::RootIdentity,
        ));
    }
    let (kind, ud, ui, wv, wf) = columns(root);
    let (pk, pud, pui, pwv, pwf) = columns(record.parent_identity());
    let result = sqlx::query("UPDATE workspace_allocations SET phase = 'committed' WHERE allocation_id = ? COLLATE BINARY AND workspace_id = ? COLLATE BINARY AND root_generation = ? COLLATE BINARY AND relative_name = ? COLLATE BINARY AND created_at = ? COLLATE BINARY AND phase = 'materialized' COLLATE BINARY AND root_identity_kind = ? COLLATE BINARY AND root_unix_device IS ? AND root_unix_inode IS ? AND root_windows_volume IS ? AND root_windows_file_id IS ? AND parent_identity_kind = ? COLLATE BINARY AND parent_unix_device IS ? AND parent_unix_inode IS ? AND parent_windows_volume IS ? AND parent_windows_file_id IS ? AND failure_reason IS NULL")
        .bind(intent.allocation_id().as_str()).bind(intent.workspace_id().as_str()).bind(intent.root_generation()).bind(intent.relative_name()).bind(intent.created_at().as_str())
        .bind(kind).bind(ud).bind(ui).bind(wv).bind(wf).bind(pk).bind(pud).bind(pui).bind(pwv).bind(pwf)
        .execute(&mut **tx).await.map_err(|_| bad())?;
    if result.rows_affected() != 1 {
        return Err(bad());
    }
    let (record, workspace) = read(tx, intent).await?;
    if record.phase() != AllocationPhase::Committed
        || record.root_identity() != Some(root)
        || workspace.root.identity() != root
        || workspace.state != WorkspaceState::Active
        || workspace != original_workspace
    {
        return Err(bad());
    }
    let detail = json!({"allocation_id": intent.allocation_id().as_str(), "workspace_id": intent.workspace_id().as_str(), "phase": "committed"});
    let audit = crate::Store::append_audit_tx(
        tx,
        record.created_at().as_str(),
        "workspace_allocation",
        "workspace.allocation_committed",
        &detail,
    )
    .await
    .map_err(|_| bad())?;
    let (record, workspace) = read(tx, intent).await?;
    if record.phase() != AllocationPhase::Committed
        || record.root_identity() != Some(root)
        || workspace.root.identity() != root
        || workspace != original_workspace
        || collision(tx, intent, root).await?
    {
        return Err(bad());
    }
    let persisted = sqlx::query(
        "SELECT ts, actor, action, detail, prev_hash, hash FROM audit_log WHERE seq = ?",
    )
    .bind(audit.seq)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| bad())?
    .ok_or(bad())?;
    use sqlx::Row;
    if persisted.try_get::<String, _>("ts").ok().as_deref() != Some(audit.ts.as_str())
        || persisted.try_get::<String, _>("actor").ok().as_deref() != Some(audit.actor.as_str())
        || persisted.try_get::<String, _>("action").ok().as_deref() != Some(audit.action.as_str())
        || persisted.try_get::<String, _>("detail").ok().as_deref()
            != Some(serde_json::to_string(&detail).map_err(|_| bad())?.as_str())
        || persisted.try_get::<String, _>("prev_hash").ok().as_deref()
            != Some(audit.prev_hash.as_str())
        || persisted.try_get::<String, _>("hash").ok().as_deref() != Some(audit.hash.as_str())
    {
        return Err(bad());
    }
    Ok(record)
}
