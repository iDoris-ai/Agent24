#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_store::{Store, WorkspaceRow, test_hooks};

const CREATED: &str = "2026-09-19T00:00:00.000Z";
const EXPIRES: &str = "2026-09-20T00:00:00.000Z";
const DEVICE: &[u8] = &[0; 8];
const INODE: &[u8] = &[1; 8];

async fn seed(store: &Store, id: &str) {
    sqlx::query(
        "INSERT INTO workspaces
         (id, kind, state, provenance_source, writeback_policy,
          lifecycle_owner_kind, lifecycle_owner_ref, concurrency_policy,
          created_at, expires_at, revision, canonical_root, root_generation,
          root_identity_kind, unix_device, unix_inode)
         VALUES (?, 'orchestrator_scratch', 'active', 'test', 'external',
                 'orchestrator', 'owner', 'serial', ?, ?, 1, ?, 'g1',
                 'unix', ?, ?)",
    )
    .bind(id)
    .bind(CREATED)
    .bind(EXPIRES)
    .bind(format!("/tmp/{id}"))
    .bind(DEVICE)
    .bind(INODE)
    .execute(test_hooks::pool(store))
    .await
    .unwrap();
}

async fn decode(store: &Store, id: &str) -> agent24_store::WorkspaceResult<WorkspaceRow> {
    let row = sqlx::query("SELECT * FROM workspaces WHERE id = ?")
        .bind(id)
        .fetch_one(test_hooks::pool(store))
        .await
        .unwrap();
    WorkspaceRow::decode(&row)
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
async fn workspace_decoder_rejects_unknown_policy_and_blank_required_text() {
    let store = Store::open_memory().await.unwrap();
    let id = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    seed(&store, id).await;
    tamper(
        &store,
        "UPDATE workspaces SET provenance_source = '   ' WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'",
    )
    .await;
    assert!(decode(&store, id).await.is_err());

    let store = Store::open_memory().await.unwrap();
    seed(&store, id).await;
    tamper(
        &store,
        "UPDATE workspaces SET writeback_policy = 'internal' WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'",
    )
    .await;
    assert!(decode(&store, id).await.is_err());
}

#[tokio::test]
async fn workspace_decoder_rejects_wrong_revision_and_identity_storage() {
    let id = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
    let store = Store::open_memory().await.unwrap();
    seed(&store, id).await;
    tamper(
        &store,
        "UPDATE workspaces SET revision = 1.5 WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6'",
    )
    .await;
    assert!(decode(&store, id).await.is_err());

    let store = Store::open_memory().await.unwrap();
    seed(&store, id).await;
    tamper(
        &store,
        "UPDATE workspaces SET unix_device = '00000000' WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6'",
    )
    .await;
    assert!(decode(&store, id).await.is_err());
}

#[tokio::test]
async fn workspace_decoder_rejects_quarantine_before_renewal() {
    let store = Store::open_memory().await.unwrap();
    let id = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X7";
    seed(&store, id).await;
    tamper(
        &store,
        "UPDATE workspaces
         SET state = 'releasing', renewed_at = '2026-09-19T00:00:01.000Z',
             quarantine_root = '/q', quarantined_at = '2026-09-19T00:00:00.500Z'
         WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X7'",
    )
    .await;
    assert!(decode(&store, id).await.is_err());
}

#[tokio::test]
async fn workspace_decoder_rejects_sqlite_incompatible_leap_seconds() {
    let store = Store::open_memory().await.unwrap();
    let id = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X8";
    seed(&store, id).await;
    tamper(
        &store,
        "UPDATE workspaces SET created_at = '2015-02-18T23:59:60.000Z' WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X8'",
    )
    .await;
    assert!(decode(&store, id).await.is_err());
}

#[tokio::test]
async fn workspace_decoder_matches_cleanup_state_nullability() {
    let id = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X9";
    let store = Store::open_memory().await.unwrap();
    seed(&store, id).await;
    tamper(
        &store,
        "UPDATE workspaces SET state = 'releasing', quarantine_root = '   ', quarantined_at = '2026-09-19T00:00:02.000Z' WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X9'",
    )
    .await;
    assert!(decode(&store, id).await.is_err());

    let store = Store::open_memory().await.unwrap();
    seed(&store, id).await;
    tamper(
        &store,
        "UPDATE workspaces SET state = 'releasing', cleanup_error = 'stale', cleanup_retry_at = '2026-09-19T00:00:02.000Z' WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X9'",
    )
    .await;
    assert!(decode(&store, id).await.is_err());

    let store = Store::open_memory().await.unwrap();
    seed(&store, id).await;
    tamper(
        &store,
        "UPDATE workspaces SET state = 'cleanup_failed', cleanup_error = 'busy', cleanup_retry_at = '2026-09-19T00:00:02.000Z', cleanup_attempts = 1, cleanup_last_attempt_at = '2026-09-19T00:00:01.000Z' WHERE id = 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X9'",
    )
    .await;
    assert!(decode(&store, id).await.is_ok());
}
