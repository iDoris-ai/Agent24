#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_store::{Store, test_hooks};
use sqlx::migrate::Migrator;
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
    let allocation_table: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'workspace_allocations'",
    )
    .fetch_one(test_hooks::pool(&store))
    .await
    .unwrap();
    assert_eq!(allocation_table, 1);
    let version: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
        .fetch_one(test_hooks::pool(&store))
        .await
        .unwrap();
    assert_eq!(version, 8);
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
