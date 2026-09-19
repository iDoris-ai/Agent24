#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    Store, WorkspaceListCursor, WorkspaceListLimit, WorkspaceListQuery, WorkspaceState,
    WorkspaceStoreError, test_hooks,
};

const EXPIRES: &str = "2026-09-20T00:00:00.000Z";

async fn insert(store: &Store, id: &str, state: &str, created_at: &str) {
    sqlx::query(
        "INSERT INTO workspaces
         (id, kind, state, provenance_source, writeback_policy,
          lifecycle_owner_kind, lifecycle_owner_ref, concurrency_policy,
          created_at, expires_at, revision, canonical_root, root_generation,
          root_identity_kind, unix_device, unix_inode)
         VALUES (?, 'orchestrator_scratch', ?, 'git', 'external',
                 'orchestrator', 'list-owner', 'serial', ?, ?, 1, ?,
                 'list-generation', 'unix', ?, ?)",
    )
    .bind(id)
    .bind(state)
    .bind(created_at)
    .bind(EXPIRES)
    .bind(format!("/private/list/{id}"))
    .bind([1; 8].as_slice())
    .bind(vec![id.as_bytes()[28]; 8])
    .execute(test_hooks::pool(store))
    .await
    .unwrap();
}

fn query(
    state: Option<WorkspaceState>,
    after: Option<WorkspaceListCursor>,
    limit: u16,
) -> WorkspaceListQuery {
    WorkspaceListQuery::new(state, after, WorkspaceListLimit::new(limit).unwrap())
}

fn ids(page: &agent24_store::WorkspacePage) -> Vec<String> {
    page.items()
        .iter()
        .map(|workspace| workspace.id.to_string())
        .collect()
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
async fn list_orders_by_created_desc_then_binary_id_desc_and_projects() {
    let store = Store::open_memory().await.unwrap();
    insert(
        &store,
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1",
        "active",
        "2026-09-19T00:01:00.000Z",
    )
    .await;
    insert(
        &store,
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X2",
        "active",
        "2026-09-19T00:01:00.000Z",
    )
    .await;
    insert(
        &store,
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X3",
        "expired",
        "2026-09-19T00:00:00.000Z",
    )
    .await;

    let page = store.list_workspaces(&query(None, None, 10)).await.unwrap();
    assert_eq!(
        ids(&page),
        vec![
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X2",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X3",
        ]
    );
    assert!(page.next_cursor().is_none());
    let wire = serde_json::to_string(&page.items()[0]).unwrap();
    assert!(!wire.contains("/private/list/"));
    assert!(!wire.contains("list-generation"));
}

#[tokio::test]
async fn list_keyset_pages_are_contiguous_and_exact_limit_has_no_cursor() {
    let store = Store::open_memory().await.unwrap();
    for (id, created_at) in [
        ("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1", "2026-09-19T00:00:00.000Z"),
        ("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X2", "2026-09-19T00:01:00.000Z"),
        ("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X3", "2026-09-19T00:02:00.000Z"),
        ("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X4", "2026-09-19T00:03:00.000Z"),
        ("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5", "2026-09-19T00:04:00.000Z"),
    ] {
        insert(&store, id, "active", created_at).await;
    }

    let first = store.list_workspaces(&query(None, None, 2)).await.unwrap();
    assert_eq!(
        ids(&first),
        vec![
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X4",
        ]
    );
    let second = store
        .list_workspaces(&query(None, first.next_cursor().cloned(), 2))
        .await
        .unwrap();
    assert_eq!(
        ids(&second),
        vec![
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X3",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X2",
        ]
    );
    let third = store
        .list_workspaces(&query(None, second.next_cursor().cloned(), 2))
        .await
        .unwrap();
    assert_eq!(ids(&third), vec!["ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1"]);
    assert!(third.next_cursor().is_none());
}

#[tokio::test]
async fn list_state_filter_applies_before_keyset_boundary() {
    let store = Store::open_memory().await.unwrap();
    insert(
        &store,
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1",
        "active",
        "2026-09-19T00:00:00.000Z",
    )
    .await;
    insert(
        &store,
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X2",
        "expired",
        "2026-09-19T00:01:00.000Z",
    )
    .await;
    insert(
        &store,
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X3",
        "active",
        "2026-09-19T00:02:00.000Z",
    )
    .await;

    let active = store
        .list_workspaces(&query(Some(WorkspaceState::Active), None, 10))
        .await
        .unwrap();
    assert_eq!(
        ids(&active),
        vec![
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X3",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1",
        ]
    );
    let after = WorkspaceListCursor::new(
        agent24_store::WorkspaceInstant::parse("2026-09-19T00:02:00.000Z").unwrap(),
        WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X3").unwrap(),
    );
    let page = store
        .list_workspaces(&query(Some(WorkspaceState::Active), Some(after), 10))
        .await
        .unwrap();
    assert_eq!(ids(&page), vec!["ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1"]);
}

#[tokio::test]
async fn list_decodes_the_sentinel_and_fails_the_whole_page_on_corruption() {
    let store = Store::open_memory().await.unwrap();
    insert(
        &store,
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1",
        "active",
        "2026-09-19T00:00:00.000Z",
    )
    .await;
    insert(
        &store,
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X2",
        "active",
        "2026-09-19T00:01:00.000Z",
    )
    .await;
    tamper(
        &store,
        "UPDATE workspaces SET state = 'private-corruption'
         WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1'",
    )
    .await;

    let error = store
        .list_workspaces(&query(None, None, 1))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        WorkspaceStoreError::CorruptRow {
            table: "workspaces",
            field: "state",
        }
    );
    assert_eq!(error.to_string(), "corrupt workspaces row: invalid state");
    assert!(!error.to_string().contains("private-corruption"));
}

#[tokio::test]
async fn list_empty_and_database_failures_have_static_results() {
    let store = Store::open_memory().await.unwrap();
    let empty = store
        .list_workspaces(&query(Some(WorkspaceState::Released), None, 1))
        .await
        .unwrap();
    assert!(empty.items().is_empty());
    assert!(empty.next_cursor().is_none());

    sqlx::query("DROP TABLE workspaces")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    let error = store
        .list_workspaces(&query(None, None, 1))
        .await
        .unwrap_err();
    assert_eq!(error, WorkspaceStoreError::Database);
    assert_eq!(error.to_string(), "workspace database error");
}

#[tokio::test]
async fn list_performs_no_workspace_or_lease_mutation() {
    let store = Store::open_memory().await.unwrap();
    insert(
        &store,
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1",
        "active",
        "2026-09-19T00:00:00.000Z",
    )
    .await;
    for (name, event, table) in [
        ("list_deny_workspace_insert", "INSERT", "workspaces"),
        ("list_deny_workspace_update", "UPDATE", "workspaces"),
        ("list_deny_workspace_delete", "DELETE", "workspaces"),
        ("list_deny_lease_insert", "INSERT", "workspace_leases"),
        ("list_deny_lease_update", "UPDATE", "workspace_leases"),
        ("list_deny_lease_delete", "DELETE", "workspace_leases"),
    ] {
        let statement = format!(
            "CREATE TRIGGER {name} BEFORE {event} ON {table}
             BEGIN SELECT RAISE(ABORT, 'list mutation denied'); END"
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

    let page = store.list_workspaces(&query(None, None, 10)).await.unwrap();
    assert_eq!(page.items().len(), 1);
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
async fn list_does_not_see_an_uncommitted_wal_row_but_sees_commit() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("registry.sqlite"))
        .await
        .unwrap();
    let mut tx = test_hooks::pool(&store).begin().await.unwrap();
    sqlx::query(
        "INSERT INTO workspaces
         (id, kind, state, provenance_source, writeback_policy,
          lifecycle_owner_kind, lifecycle_owner_ref, concurrency_policy,
          created_at, expires_at, revision, canonical_root, root_generation,
          root_identity_kind, unix_device, unix_inode)
         VALUES (?, 'orchestrator_scratch', 'active', 'git', 'external',
                 'orchestrator', 'list-owner', 'serial', ?, ?, 1, ?,
                 'list-generation', 'unix', ?, ?)",
    )
    .bind("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1")
    .bind("2026-09-19T00:00:00.000Z")
    .bind(EXPIRES)
    .bind("/private/list/wal")
    .bind([7; 8].as_slice())
    .bind([8; 8].as_slice())
    .execute(&mut *tx)
    .await
    .unwrap();

    let before_commit = store.list_workspaces(&query(None, None, 10)).await.unwrap();
    assert!(before_commit.items().is_empty());
    tx.commit().await.unwrap();
    let after_commit = store.list_workspaces(&query(None, None, 10)).await.unwrap();
    assert_eq!(ids(&after_commit), vec!["ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1"]);
}

#[tokio::test]
async fn cancelled_queued_list_releases_the_single_memory_pool_waiter() {
    let store = Store::open_memory().await.unwrap();
    insert(
        &store,
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1",
        "active",
        "2026-09-19T00:00:00.000Z",
    )
    .await;
    let held = test_hooks::pool(&store).acquire().await.unwrap();
    let queued = {
        let store = store.clone();
        tokio::spawn(async move { store.list_workspaces(&query(None, None, 10)).await })
    };
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    assert!(!queued.is_finished(), "LIST completed while pool was held");
    queued.abort();
    let error = queued.await.expect_err("queued LIST should be cancelled");
    assert!(error.is_cancelled());
    drop(held);
    let page = store.list_workspaces(&query(None, None, 10)).await.unwrap();
    assert_eq!(ids(&page), vec!["ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1"]);
}

#[tokio::test]
async fn list_cursor_keeps_inter_page_boundary_stable_when_newer_rows_arrive() {
    let store = Store::open_memory().await.unwrap();
    insert(
        &store,
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1",
        "active",
        "2026-09-19T00:00:00.000Z",
    )
    .await;
    insert(
        &store,
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X2",
        "active",
        "2026-09-19T00:01:00.000Z",
    )
    .await;
    insert(
        &store,
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X3",
        "active",
        "2026-09-19T00:01:00.000Z",
    )
    .await;
    let first = store.list_workspaces(&query(None, None, 1)).await.unwrap();
    assert_eq!(ids(&first), vec!["ws_01J5M4Q2Y7N8P9R0S1T2V3W4X3"]);

    insert(
        &store,
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X4",
        "active",
        "2026-09-19T00:02:00.000Z",
    )
    .await;
    let second = store
        .list_workspaces(&query(None, first.next_cursor().cloned(), 10))
        .await
        .unwrap();
    assert_eq!(
        ids(&second),
        vec![
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X2",
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X1",
        ]
    );
}
