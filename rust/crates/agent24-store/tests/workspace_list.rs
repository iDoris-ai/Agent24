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
