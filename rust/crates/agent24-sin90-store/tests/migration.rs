//! ME-1b-b: migrating `~/.agent24/sin90.db` to the per-module directory.
//!
//! The failure this guards against is SILENT: without a migration the daemon
//! opens the new path, `create_if_missing` obliges, and the user sees a Sin90
//! with no directions, no blocks and no proposals — while the real database sits
//! untouched one directory up. Every test here is about that, or about the ways a
//! migration can make it worse (a half-copy renamed into place, a legacy file
//! overwriting newer data, a crash leaving nothing openable).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_sin90_store::Sin90Store;
use std::path::Path;

/// A legacy database with one direction in it, at `path`.
async fn seed_legacy(path: &Path) -> String {
    let store = Sin90Store::open(path).await.unwrap();
    let d = store
        .create_direction("ship the mounter", "2026-Q3")
        .await
        .unwrap();
    // Drop the pool so the WAL is checkpointed the way a stopped daemon would
    // leave it. (The interesting case — data still in the WAL — is covered
    // separately below.)
    drop(store);
    d.id
}

#[tokio::test]
async fn a_legacy_database_is_migrated_rather_than_silently_replaced_by_an_empty_one() {
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("sin90.db");
    let dest = tmp.path().join("os/sin90/sin90.db");
    let id = seed_legacy(&legacy).await;

    let store = Sin90Store::open_migrating_from(&dest, &legacy)
        .await
        .unwrap();
    let dirs = store.list_directions().await.unwrap();
    assert_eq!(
        dirs.len(),
        1,
        "the user's data must survive the move; an empty store here is the exact \
         silent data loss this exists to prevent"
    );
    assert_eq!(dirs[0].id, id);
    assert_eq!(dirs[0].title, "ship the mounter");

    assert!(dest.exists(), "the destination database was created");
    assert!(
        legacy.exists(),
        "the legacy file is kept as a rollback snapshot (its CONTENTS are never \
         modified; SQLite may still touch its -wal/-shm sidecars while reading it)"
    );
    assert!(
        !tmp.path().join("os/sin90/sin90.db.migrating").exists(),
        "the temp snapshot must not be left behind"
    );
}

#[tokio::test]
async fn data_living_only_in_the_wal_is_migrated_too() {
    // The reason this is `VACUUM INTO` and not three file renames: with an open
    // WAL, committed rows can live only in `sin90.db-wal`. Moving the main file
    // alone would drop them while looking like it worked.
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("sin90.db");
    let dest = tmp.path().join("os/sin90/sin90.db");

    let live = Sin90Store::open(&legacy).await.unwrap();
    let d = live
        .create_direction("written into the wal", "2026-Q3")
        .await
        .unwrap();
    assert!(
        legacy.with_extension("db-wal").exists(),
        "precondition: the write went to a WAL"
    );
    // The pool stays OPEN across the migration. That is not literally a killed
    // daemon, but it is what keeps the row in the WAL rather than the main file,
    // which is the condition under test. (Mutation-checked: replacing `VACUUM INTO`
    // with a copy of the main file alone makes this assert `left: 0`.)

    let store = Sin90Store::open_migrating_from(&dest, &legacy)
        .await
        .unwrap();
    let dirs = store.list_directions().await.unwrap();
    assert_eq!(dirs.len(), 1, "WAL-resident data must be copied");
    assert_eq!(dirs[0].id, d.id);
    drop(live);
}

#[tokio::test]
async fn migration_is_idempotent_and_the_destination_always_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("sin90.db");
    let dest = tmp.path().join("os/sin90/sin90.db");
    seed_legacy(&legacy).await;

    let store = Sin90Store::open_migrating_from(&dest, &legacy)
        .await
        .unwrap();
    // Work done AFTER the migration — the case that a re-migration would destroy.
    store
        .create_direction("added after moving", "2026-Q4")
        .await
        .unwrap();
    drop(store);

    // Second start: legacy is still there, but the destination must not be
    // overwritten from it. (This is what makes a downgrade-then-upgrade safe:
    // the old file is stale, and staleness must never win.)
    let store = Sin90Store::open_migrating_from(&dest, &legacy)
        .await
        .unwrap();
    let titles: Vec<_> = store
        .list_directions()
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.title)
        .collect();
    assert_eq!(
        titles.len(),
        2,
        "post-migration work must survive a second start: {titles:?}"
    );
    assert!(titles.iter().any(|t| t == "added after moving"));
}

#[tokio::test]
async fn a_fresh_install_with_no_legacy_database_just_opens() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("os/sin90/sin90.db");
    let legacy = tmp.path().join("sin90.db");

    let store = Sin90Store::open_migrating_from(&dest, &legacy)
        .await
        .unwrap();
    assert!(store.list_directions().await.unwrap().is_empty());
    assert!(dest.exists());
    assert!(!legacy.exists(), "no legacy file is invented");
}

#[tokio::test]
async fn a_stale_temp_file_from_a_crashed_attempt_does_not_block_a_retry() {
    // `VACUUM INTO` refuses to write to an existing file, so a crash between
    // creating the temp and renaming it would otherwise wedge the migration
    // permanently — every subsequent start failing for a reason that has nothing
    // to do with the user's data.
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("sin90.db");
    let dest = tmp.path().join("os/sin90/sin90.db");
    seed_legacy(&legacy).await;
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(
        dest.parent().unwrap().join("sin90.db.migrating"),
        b"garbage",
    )
    .unwrap();

    let store = Sin90Store::open_migrating_from(&dest, &legacy)
        .await
        .unwrap();
    assert_eq!(store.list_directions().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_corrupt_legacy_file_degrades_instead_of_producing_an_empty_store() {
    // The worst possible outcome is a working-LOOKING empty Sin90, because the
    // user would then start entering data into it and the two would diverge. An
    // error degrades the module to 503, which is loud and recoverable.
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("sin90.db");
    let dest = tmp.path().join("os/sin90/sin90.db");
    std::fs::write(&legacy, b"this is definitely not a sqlite database").unwrap();

    let err = Sin90Store::open_migrating_from(&dest, &legacy).await;
    assert!(
        err.is_err(),
        "a corrupt legacy database must NOT fall through to create_if_missing"
    );
    assert!(
        !dest.exists(),
        "and must not leave a half-made destination behind"
    );
    assert!(
        legacy.exists(),
        "the legacy file is still there to recover from"
    );
}

#[tokio::test]
async fn a_half_created_destination_does_not_hide_the_legacy_data() {
    // Existence is not proof of a completed migration. An interrupted first start
    // (or a stray `touch`) leaves a zero-byte destination; opening it would run
    // migrations, produce a perfectly valid EMPTY Sin90, and leave the user's real
    // database one directory up, invisible — with no error anywhere.
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("sin90.db");
    let dest = tmp.path().join("os/sin90/sin90.db");
    let id = seed_legacy(&legacy).await;
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(&dest, b"").unwrap();

    let Err(err) = Sin90Store::open_migrating_from(&dest, &legacy).await else {
        panic!("an uninitialized destination must not silently win");
    };
    let msg = err.to_string();
    assert!(msg.contains("not an initialized sin90 database"), "{msg}");
    // The remediation text is asserted too: an error that only says "no" leaves a
    // frightened user with a database they do not understand. It must also NOT tell
    // them to delete anything — this file could hold newer data than the legacy one.
    assert!(
        msg.contains("MOVE the incomplete file aside"),
        "the error must say what to do, got: {msg}"
    );
    assert!(
        msg.contains("do not delete it"),
        "and must warn AGAINST deleting: this file could hold newer data than the \
         legacy one, so 'remove it' would be the worst possible advice. Got: {msg}"
    );
    // Nothing was touched.
    assert_eq!(
        std::fs::metadata(&dest).unwrap().len(),
        0,
        "the incomplete destination must be left exactly as found"
    );

    // And once the incomplete file is moved aside, the migration runs normally —
    // the refusal is recoverable, not a dead end.
    std::fs::remove_file(&dest).unwrap();
    let store = Sin90Store::open_migrating_from(&dest, &legacy)
        .await
        .unwrap();
    let dirs = store.list_directions().await.unwrap();
    assert_eq!(dirs.len(), 1);
    assert_eq!(
        dirs[0].id, id,
        "and it is the ORIGINAL row, not a new empty one"
    );
}

#[tokio::test]
async fn a_partially_migrated_legacy_database_is_not_treated_as_initialized() {
    // The subtlest of the three silent-empty roads: a database that has the sqlx
    // migration TABLE and even a `sin90_`-prefixed table, but no migration that
    // ever completed. Checking for the table alone would call this initialized,
    // copy it, and then fail — or worse, succeed into something half-built.
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("sin90.db");
    let dest = tmp.path().join("os/sin90/sin90.db");
    {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use std::str::FromStr;
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", legacy.display()))
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        // sqlx's own shape, with the success flag CLEARED — a migration that began
        // and did not finish.
        sqlx::query(
            "CREATE TABLE _sqlx_migrations (
                 version BIGINT PRIMARY KEY, description TEXT, installed_on TIMESTAMP,
                 success BOOLEAN, checksum BLOB, execution_time BIGINT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO _sqlx_migrations VALUES (1, 'x', NULL, 0, NULL, 0)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sin90_directions (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    let Err(err) = Sin90Store::open_migrating_from(&dest, &legacy).await else {
        panic!("a database with no COMPLETED migration must not be adopted");
    };
    assert!(
        err.to_string()
            .contains("not an initialized sin90 database"),
        "{err}"
    );
    assert!(!dest.exists(), "and nothing was created at the destination");
}

#[tokio::test]
async fn a_legacy_file_that_is_not_a_sin90_database_is_refused_not_copied() {
    // `quick_check` proves INTEGRITY, never IDENTITY: an unrelated SQLite database
    // passes it, gets copied into place, and then Sin90's migrations create empty
    // tables inside it. Valid file, valid schema, no data — the silent-empty
    // outcome again, by a different road.
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("sin90.db");
    let dest = tmp.path().join("os/sin90/sin90.db");

    // A real, healthy SQLite database that is simply not ours.
    {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use std::str::FromStr;
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", legacy.display()))
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE someone_elses_data (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
    assert!(legacy.exists());

    let Err(err) = Sin90Store::open_migrating_from(&dest, &legacy).await else {
        panic!("a foreign database must not be adopted as Sin90 data");
    };
    assert!(
        err.to_string()
            .contains("not an initialized sin90 database"),
        "{err}"
    );
    assert!(!dest.exists(), "and nothing was created at the destination");
}
