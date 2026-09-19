#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration,
    WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceStoreError, WorkspaceTtl, test_hooks,
};
use sqlx::Row;

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const NOW: &str = "2026-09-19T00:00:00.000Z";

fn input() -> NewScratchWorkspace {
    NewScratchWorkspace::new(
        WorkspaceId::parse(ID).unwrap(),
        TrustedRootRegistration::new(
            "/private/get-root".into(),
            "generation-get".into(),
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
        )
        .unwrap(),
        WorkspaceProvenanceInput::new("git".into(), Some("project".into()), Some("base".into()))
            .unwrap(),
        LifecycleOwnerRef::parse("owner-get".into()).unwrap(),
        WorkspaceTtl::new(60_000).unwrap(),
    )
}

async fn create(store: &Store) -> agent24_protocol::Workspace {
    store
        .create_workspace(
            &input(),
            &LifecycleOwnerRef::parse("owner-get".into()).unwrap(),
            &WorkspaceInstant::parse(NOW).unwrap(),
        )
        .await
        .unwrap()
}

async fn insert_raw(store: &Store, id: &str, expires_at: &str) {
    sqlx::query(
        "INSERT INTO workspaces
         (id, kind, state, provenance_source, writeback_policy,
          lifecycle_owner_kind, lifecycle_owner_ref, concurrency_policy,
          created_at, expires_at, revision, canonical_root, root_generation,
          root_identity_kind, unix_device, unix_inode)
         VALUES (?, 'orchestrator_scratch', 'active', 'git', 'external',
                 'orchestrator', 'owner-get', 'serial', ?, ?, 1, ?,
                 'generation-get', 'unix', ?, ?)",
    )
    .bind(id)
    .bind(NOW)
    .bind(expires_at)
    .bind(format!("/raw/{id}"))
    .bind([1; 8].as_slice())
    .bind([2; 8].as_slice())
    .execute(test_hooks::pool(store))
    .await
    .unwrap();
}

#[tokio::test]
async fn get_returns_existing_workspace_with_redacted_projection() {
    let store = Store::open_memory().await.unwrap();
    let created = create(&store).await;
    let id = WorkspaceId::parse(ID).unwrap();

    let got = store.get_workspace(&id).await.unwrap();
    assert_eq!(got, created);
    let wire = serde_json::to_string(&got).unwrap();
    for secret in ["/private/get-root", "generation-get"] {
        assert!(!wire.contains(secret), "GET projection leaked {secret}");
    }
}

#[tokio::test]
async fn get_missing_is_static_not_found() {
    let store = Store::open_memory().await.unwrap();
    let id = WorkspaceId::parse(ID).unwrap();

    assert_eq!(
        store.get_workspace(&id).await,
        Err(WorkspaceStoreError::NotFound)
    );
    assert_eq!(
        WorkspaceStoreError::NotFound.to_string(),
        "workspace not found"
    );
}

#[tokio::test]
async fn get_does_not_expire_a_past_workspace_or_mutate_its_row() {
    let store = Store::open_memory().await.unwrap();
    insert_raw(&store, ID, "2026-09-19T00:00:01.000Z").await;
    let id = WorkspaceId::parse(ID).unwrap();
    let before = sqlx::query(
        "SELECT state, expires_at, cleanup_attempts, cleanup_error
         FROM workspaces WHERE id = ?",
    )
    .bind(ID)
    .fetch_one(test_hooks::pool(&store))
    .await
    .unwrap();

    let got = store.get_workspace(&id).await.unwrap();
    assert_eq!(got.state, "active");
    let after = sqlx::query(
        "SELECT state, expires_at, cleanup_attempts, cleanup_error
         FROM workspaces WHERE id = ?",
    )
    .bind(ID)
    .fetch_one(test_hooks::pool(&store))
    .await
    .unwrap();
    assert_eq!(
        before.get::<String, _>("state"),
        after.get::<String, _>("state")
    );
    assert_eq!(
        before.get::<String, _>("expires_at"),
        after.get::<String, _>("expires_at")
    );
    assert_eq!(
        before.get::<i64, _>("cleanup_attempts"),
        after.get::<i64, _>("cleanup_attempts")
    );
    assert_eq!(
        before.get::<Option<String>, _>("cleanup_error"),
        after.get::<Option<String>, _>("cleanup_error")
    );
}

#[tokio::test]
async fn get_returns_valid_inactive_and_cleanup_failed_rows() {
    let store = Store::open_memory().await.unwrap();
    insert_raw(&store, ID, "2026-09-19T00:01:00.000Z").await;
    let id = WorkspaceId::parse(ID).unwrap();

    sqlx::query("UPDATE workspaces SET state = 'expired' WHERE id = ?")
        .bind(ID)
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    assert_eq!(store.get_workspace(&id).await.unwrap().state, "expired");

    sqlx::query(
        "UPDATE workspaces SET state = 'cleanup_failed', cleanup_attempts = 1,
         cleanup_last_attempt_at = ?, cleanup_error = 'busy', cleanup_retry_at = ?
         WHERE id = ?",
    )
    .bind(NOW)
    .bind("2026-09-19T00:00:02.000Z")
    .bind(ID)
    .execute(test_hooks::pool(&store))
    .await
    .unwrap();
    let got = store.get_workspace(&id).await.unwrap();
    assert_eq!(got.state, "cleanup_failed");
}

#[tokio::test]
async fn get_triggers_no_workspace_or_lease_mutation() {
    let store = Store::open_memory().await.unwrap();
    create(&store).await;
    sqlx::query(
        "CREATE TRIGGER deny_workspace_update BEFORE UPDATE ON workspaces
         BEGIN SELECT RAISE(ABORT, 'workspace update denied'); END",
    )
    .execute(test_hooks::pool(&store))
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER deny_lease_update BEFORE UPDATE ON workspace_leases
         BEGIN SELECT RAISE(ABORT, 'lease update denied'); END",
    )
    .execute(test_hooks::pool(&store))
    .await
    .unwrap();

    let id = WorkspaceId::parse(ID).unwrap();
    assert_eq!(store.get_workspace(&id).await.unwrap().id, id);
}

#[tokio::test]
async fn get_does_not_see_uncommitted_wal_row_but_sees_commit() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("registry.sqlite"))
        .await
        .unwrap();
    let id = WorkspaceId::parse(ID).unwrap();
    let mut tx = test_hooks::pool(&store).begin().await.unwrap();
    sqlx::query(
        "INSERT INTO workspaces
         (id, kind, state, provenance_source, writeback_policy,
          lifecycle_owner_kind, lifecycle_owner_ref, concurrency_policy,
          created_at, expires_at, revision, canonical_root, root_generation,
          root_identity_kind, unix_device, unix_inode)
         VALUES (?, 'orchestrator_scratch', 'active', 'git', 'external',
                 'orchestrator', 'owner-get', 'serial', ?, ?, 1, ?,
                 'generation-get', 'unix', ?, ?)",
    )
    .bind(ID)
    .bind(NOW)
    .bind("2026-09-19T00:01:00.000Z")
    .bind("/wal/get")
    .bind([3; 8].as_slice())
    .bind([4; 8].as_slice())
    .execute(&mut *tx)
    .await
    .unwrap();
    assert_eq!(
        store.get_workspace(&id).await,
        Err(WorkspaceStoreError::NotFound)
    );
    tx.commit().await.unwrap();
    assert_eq!(store.get_workspace(&id).await.unwrap().id, id);
}

#[tokio::test]
async fn cancelled_queued_get_releases_pool_waiter() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("registry.sqlite"))
        .await
        .unwrap();
    create(&store).await;
    let id = WorkspaceId::parse(ID).unwrap();
    let mut held = Vec::new();
    for _ in 0..5 {
        held.push(test_hooks::pool(&store).acquire().await.unwrap());
    }
    let queued = {
        let store = store.clone();
        let id = id.clone();
        tokio::spawn(async move { store.get_workspace(&id).await })
    };
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    queued.abort();
    let _ = queued.await;
    drop(held);
    assert_eq!(store.get_workspace(&id).await.unwrap().id, id);
}
