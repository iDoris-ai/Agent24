#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_store::{Store, test_hooks};
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;

async fn run_v6(path: &Path) {
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .unwrap()
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let mut migrator = Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await
        .unwrap();
    migrator
        .migrations
        .to_mut()
        .retain(|migration| migration.version <= 6);
    migrator.run(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, Option<i64>>("SELECT max(version) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        Some(6)
    );
    pool.close().await;
}

async fn run_v8(path: &Path) {
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .unwrap()
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let mut migrator = Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await
        .unwrap();
    migrator
        .migrations
        .to_mut()
        .retain(|migration| migration.version <= 8);
    migrator.run(&pool).await.unwrap();
    pool.close().await;
}

async fn migration_checksums(pool: &sqlx::SqlitePool) -> Vec<(i64, String, Vec<u8>)> {
    sqlx::query_as(
        "SELECT version, description, checksum FROM _sqlx_migrations WHERE version IN (7, 8) ORDER BY version",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn fresh_install_creates_registry_without_legacy_row() {
    let store = Store::open_memory().await.unwrap();
    assert_eq!(sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN ('workspaces', 'workspace_leases')")
        .fetch_one(test_hooks::pool(&store)).await.unwrap(), 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM workspaces WHERE kind = 'legacy_compat'"
        )
        .fetch_one(test_hooks::pool(&store))
        .await
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn opening_a_real_v6_database_applies_workspace_migrations_and_preserves_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent24.db");
    run_v6(&path).await;
    let store = Store::open(&path).await.unwrap();
    let columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pragma_table_info('workspaces') WHERE name = 'root_generation'",
    )
    .fetch_one(test_hooks::pool(&store))
    .await
    .unwrap();
    assert_eq!(columns, 1);
    let version: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
        .fetch_one(test_hooks::pool(&store))
        .await
        .unwrap();
    assert_eq!(version, 13);
    drop(store);
    let reopened = Store::open(&path).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM workspaces")
            .fetch_one(test_hooks::pool(&reopened))
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn opening_a_real_v8_database_preserves_main_rows_and_migration_checksums() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent24-v8.db");
    run_v8(&path).await;
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO schedules (id,name,enabled,spec,action,delivery,last_run_at,next_run_at,consecutive_failures)
         VALUES ('sch_v8','v8 schedule',1,'{"type":"every","secs":60}',
         '{"type":"agent_run","prompt":"keep","session_id":null,"model_override":null}',
         '[]',NULL,'2026-09-23T09:00:00Z',2)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO module_model_usage (module,day,served_by,calls_ok,prompt_tokens,completion_tokens)
         VALUES ('module-v8','2026-09-27','local',3,40,20)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let before = migration_checksums(&pool).await;
    assert_eq!(before.len(), 2);
    pool.close().await;

    let store = Store::open(&path).await.unwrap();
    let pool = test_hooks::pool(&store);
    assert_eq!(
        sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT name, enabled, consecutive_failures FROM schedules WHERE id='sch_v8'",
        )
        .fetch_one(pool)
        .await
        .unwrap(),
        ("v8 schedule".to_owned(), 1, 2)
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, i64, i64)>(
            r#"SELECT calls_ok, prompt_tokens, completion_tokens FROM module_model_usage
             WHERE module='module-v8' AND day='2026-09-27' AND served_by='local'"#,
        )
        .fetch_one(pool)
        .await
        .unwrap(),
        (3, 40, 20)
    );
    assert_eq!(migration_checksums(pool).await, before);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='workspaces'",
        )
        .fetch_one(pool)
        .await
        .unwrap(),
        1
    );
    drop(store);

    let reopened = Store::open(&path).await.unwrap();
    assert_eq!(
        migration_checksums(test_hooks::pool(&reopened)).await,
        before
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM schedules WHERE id='sch_v8'")
            .fetch_one(test_hooks::pool(&reopened))
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn old_workspace_ledger_checksum_mismatch_is_rejected_without_data_loss() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old-workspace-ledger.db");
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .unwrap()
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let mut migrator = Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await
        .unwrap();
    let mut historical = migrator
        .iter()
        .filter(|migration| migration.version <= 6)
        .cloned()
        .collect::<Vec<_>>();
    historical.push(Migration::new(
        7,
        "workspaces".into(),
        MigrationType::Simple,
        include_str!("../migrations/0009_workspaces.sql").into(),
        false,
    ));
    historical.push(Migration::new(
        8,
        "workspace_allocations".into(),
        MigrationType::Simple,
        include_str!("../migrations/0010_workspace_allocations.sql").into(),
        false,
    ));
    migrator.migrations = historical.into();
    migrator.run(&pool).await.unwrap();
    sqlx::query(
        r#"INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,
         lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,
         revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode)
         VALUES ('ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','orchestrator_scratch','active',
         'historical','external','orchestrator','owner','serial',
         '2026-09-27T00:00:00.000Z','2026-09-28T00:00:00.000Z',1,
         '/tmp/historical','generation-1','unix',?,?)"#,
    )
    .bind([0u8; 8].as_slice())
    .bind([1u8; 8].as_slice())
    .execute(&pool)
    .await
    .unwrap();
    let before = migration_checksums(&pool).await;
    assert_eq!(before.len(), 2);
    pool.close().await;

    let error = match Store::open(&path).await {
        Ok(_) => panic!("old workspace migration ledger must not be silently bridged"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        agent24_store::StoreError::Migrate(sqlx::migrate::MigrateError::VersionMismatch(7))
    ));

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
                .unwrap()
                .foreign_keys(true),
        )
        .await
        .unwrap();
    assert_eq!(migration_checksums(&pool).await, before);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM workspaces")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}
