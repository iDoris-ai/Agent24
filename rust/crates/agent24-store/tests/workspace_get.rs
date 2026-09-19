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
    for (name, event, table, message) in [
        (
            "deny_workspace_insert",
            "INSERT",
            "workspaces",
            "workspace insert denied",
        ),
        (
            "deny_workspace_update",
            "UPDATE",
            "workspaces",
            "workspace update denied",
        ),
        (
            "deny_workspace_delete",
            "DELETE",
            "workspaces",
            "workspace delete denied",
        ),
        (
            "deny_lease_insert",
            "INSERT",
            "workspace_leases",
            "lease insert denied",
        ),
        (
            "deny_lease_update",
            "UPDATE",
            "workspace_leases",
            "lease update denied",
        ),
        (
            "deny_lease_delete",
            "DELETE",
            "workspace_leases",
            "lease delete denied",
        ),
    ] {
        let statement = format!(
            "CREATE TRIGGER {name} BEFORE {event} ON {table}
             BEGIN SELECT RAISE(ABORT, '{message}'); END"
        );
        sqlx::query(&statement)
            .execute(test_hooks::pool(&store))
            .await
            .unwrap();
    }

    let before_workspaces: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspaces")
        .fetch_one(test_hooks::pool(&store))
        .await
        .unwrap();
    let before_leases: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_leases")
        .fetch_one(test_hooks::pool(&store))
        .await
        .unwrap();

    let id = WorkspaceId::parse(ID).unwrap();
    let got = store.get_workspace(&id).await.unwrap();
    assert_eq!(got.id, id);
    assert_eq!(got.state, "active");
    let after_workspaces: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspaces")
        .fetch_one(test_hooks::pool(&store))
        .await
        .unwrap();
    let after_leases: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_leases")
        .fetch_one(test_hooks::pool(&store))
        .await
        .unwrap();
    assert_eq!(after_workspaces, before_workspaces);
    assert_eq!(after_leases, before_leases);
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
    let held = test_hooks::pool(&store).acquire().await.unwrap();
    let queued = {
        let store = store.clone();
        let id = id.clone();
        tokio::spawn(async move { store.get_workspace(&id).await })
    };
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    assert!(!queued.is_finished(), "GET completed while pool was held");
    queued.abort();
    let error = queued.await.expect_err("queued GET should be cancelled");
    assert!(error.is_cancelled());
    drop(held);
    assert_eq!(store.get_workspace(&id).await.unwrap().id, id);
}

async fn tamper(store: &Store, sql: &str) {
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(test_hooks::pool(store))
        .await
        .unwrap();
    sqlx::query(sql)
        .execute(test_hooks::pool(store))
        .await
        .unwrap();
    sqlx::query("PRAGMA ignore_check_constraints = OFF")
        .execute(test_hooks::pool(store))
        .await
        .unwrap();
}

#[tokio::test]
async fn get_rejects_wrong_storage_for_private_root_and_identity() {
    let store = Store::open_memory().await.unwrap();
    insert_raw(&store, ID, "2026-09-19T00:01:00.000Z").await;
    tamper(
        &store,
        "UPDATE workspaces SET canonical_root = CAST('/secret/root' AS BLOB)
         WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'",
    )
    .await;
    let id = WorkspaceId::parse(ID).unwrap();
    assert_eq!(
        store.get_workspace(&id).await,
        Err(WorkspaceStoreError::CorruptRow {
            table: "workspaces",
            field: "canonical_root"
        })
    );

    let store = Store::open_memory().await.unwrap();
    insert_raw(&store, ID, "2026-09-19T00:01:00.000Z").await;
    tamper(
        &store,
        "UPDATE workspaces SET unix_device = CAST('private-identity' AS TEXT)
         WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'",
    )
    .await;
    let error = store.get_workspace(&id).await.unwrap_err();
    assert_eq!(
        error,
        WorkspaceStoreError::CorruptRow {
            table: "workspaces",
            field: "unix_device"
        }
    );
    assert!(!error.to_string().contains("private-identity"));
}

#[tokio::test]
async fn get_rejects_enum_and_cross_field_corruption() {
    let store = Store::open_memory().await.unwrap();
    insert_raw(&store, ID, "2026-09-19T00:01:00.000Z").await;
    tamper(
        &store,
        "UPDATE workspaces SET state = 'secret_state' WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'",
    )
    .await;
    let id = WorkspaceId::parse(ID).unwrap();
    assert_eq!(
        store.get_workspace(&id).await,
        Err(WorkspaceStoreError::CorruptRow {
            table: "workspaces",
            field: "state"
        })
    );

    let store = Store::open_memory().await.unwrap();
    insert_raw(&store, ID, "2026-09-19T00:01:00.000Z").await;
    tamper(
        &store,
        "UPDATE workspaces SET cleanup_error = 'private-cleanup'
         WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'",
    )
    .await;
    let error = store.get_workspace(&id).await.unwrap_err();
    assert_eq!(
        error,
        WorkspaceStoreError::CorruptRow {
            table: "workspaces",
            field: "state"
        }
    );
    assert!(!error.to_string().contains("private-cleanup"));
}

#[tokio::test]
async fn get_missing_table_is_static_database_error() {
    let store = Store::open_memory().await.unwrap();
    sqlx::query("DROP TABLE workspaces")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    let id = WorkspaceId::parse(ID).unwrap();
    let error = store.get_workspace(&id).await.unwrap_err();
    assert_eq!(error, WorkspaceStoreError::Database);
    assert_eq!(error.to_string(), "workspace database error");
    assert!(!error.to_string().contains("no such table"));
}
