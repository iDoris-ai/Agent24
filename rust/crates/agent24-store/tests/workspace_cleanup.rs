#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_store::{Store, test_hooks};

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const CREATED: &str = "2026-09-19T00:00:00.000Z";
const EXPIRES: &str = "2026-09-20T00:00:00.000Z";
const DEVICE: &[u8] = &[0; 8];
const INODE: &[u8] = &[1; 8];

async fn workspace(store: &Store) {
    sqlx::query(
        "INSERT INTO workspaces
         (id, kind, state, provenance_source, writeback_policy,
          lifecycle_owner_kind, lifecycle_owner_ref, concurrency_policy,
          created_at, expires_at, revision, canonical_root, root_generation,
          root_identity_kind, unix_device, unix_inode)
         VALUES (?, 'orchestrator_scratch', 'active', 'test', 'external',
                 'orchestrator', 'owner', 'serial', ?, ?, 1, '/tmp/ws',
                 'g1', 'unix', ?, ?)",
    )
    .bind(ID)
    .bind(CREATED)
    .bind(EXPIRES)
    .bind(DEVICE)
    .bind(INODE)
    .execute(test_hooks::pool(store))
    .await
    .unwrap();
}

#[tokio::test]
async fn cleanup_states_and_attempt_shape_are_checked() {
    let store = Store::open_memory().await.unwrap();
    workspace(&store).await;
    assert!(
        sqlx::query("UPDATE workspaces SET quarantine_root = '/q' WHERE id = ?")
            .bind(ID)
            .execute(test_hooks::pool(&store))
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE workspaces SET cleanup_attempts = 1 WHERE id = ?")
            .bind(ID)
            .execute(test_hooks::pool(&store))
            .await
            .is_err()
    );
    sqlx::query("UPDATE workspaces SET state = 'cleanup_failed', cleanup_error = 'busy', cleanup_retry_at = ?, cleanup_attempts = 1, cleanup_last_attempt_at = ? WHERE id = ?")
        .bind(EXPIRES).bind(CREATED).bind(ID).execute(test_hooks::pool(&store)).await.unwrap();
    assert!(
        sqlx::query("UPDATE workspaces SET released_at = ? WHERE id = ?")
            .bind(EXPIRES)
            .bind(ID)
            .execute(test_hooks::pool(&store))
            .await
            .is_err()
    );
    sqlx::query("UPDATE workspaces SET state = 'released', released_at = ?, quarantine_root = '/q', quarantined_at = ?, cleanup_error = NULL, cleanup_retry_at = NULL WHERE id = ?")
        .bind(EXPIRES).bind(EXPIRES).bind(ID).execute(test_hooks::pool(&store)).await.unwrap();
    assert!(
        sqlx::query("UPDATE workspaces SET cleanup_error = 'late' WHERE id = ?")
            .bind(ID)
            .execute(test_hooks::pool(&store))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn required_text_rejects_nul_and_noncanonical_timestamps() {
    let store = Store::open_memory().await.unwrap();
    let nul = sqlx::query(
        "INSERT INTO workspaces
         (id, kind, state, provenance_source, writeback_policy,
          lifecycle_owner_kind, lifecycle_owner_ref, concurrency_policy,
          created_at, expires_at, revision, canonical_root, root_generation,
          root_identity_kind, unix_device, unix_inode)
         VALUES (?, 'orchestrator_scratch', 'active', ?, 'external',
                 'orchestrator', 'owner', 'serial', ?, ?, 1, '/tmp/ws',
                 'g1', 'unix', ?, ?)",
    )
    .bind("ws_01J5M4Q2Y7N8P9R0S1T2V3Y6")
    .bind("source\0injected")
    .bind(CREATED)
    .bind(EXPIRES)
    .bind(DEVICE)
    .bind(INODE)
    .execute(test_hooks::pool(&store))
    .await;
    assert!(nul.is_err());
    workspace(&store).await;
    assert!(
        sqlx::query("UPDATE workspaces SET expires_at = '2026-09-20T00:00:00Z' WHERE id = ?")
            .bind(ID)
            .execute(test_hooks::pool(&store))
            .await
            .is_err()
    );
}
