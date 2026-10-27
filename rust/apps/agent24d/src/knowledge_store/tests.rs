#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_knowledge::provider::record::{Change, Ids, Of};

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
struct T;

impl Ids for T {
    type Profile = String;
    type Binding = u32;
    type Run = u32;
    type Principal = String;
    type Identity = String;
    type Kb = String;
}

type C = Change<T>;

async fn open() -> (tempfile::TempDir, KnowledgeDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = KnowledgeDb::open(dir.path()).await.unwrap();
    (dir, db)
}

fn changes(seq: u64, changes: Vec<C>) -> Record<T> {
    Record {
        seq,
        entry: Entry::Changes(changes),
    }
}

fn void(seq: u64) -> Record<T> {
    Record {
        seq,
        entry: Entry::Void,
    }
}

fn bind(binding: u32) -> C {
    C::Bind {
        of: Of {
            profile: "p".into(),
            binding,
        },
        identity: "id".into(),
        generation: 1,
        confirmed: None,
    }
}

async fn log(db: &KnowledgeDb) -> Option<Result<Replayed<T>, Damaged>> {
    db.read::<T>().await.unwrap().log
}

async fn count(db: &KnowledgeDb, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

async fn exec(db: &KnowledgeDb, sql: &str) {
    sqlx::query(sql).execute(db.pool()).await.unwrap();
}

#[tokio::test]
async fn the_database_is_its_own_with_wal_and_full() {
    let (dir, db) = open().await;
    assert!(dir.path().join("knowledge").join("knowledge.db").is_file());
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    // FULL is 2.
    assert_eq!(count(&db, "PRAGMA synchronous").await, 2);
}

#[tokio::test]
async fn a_new_database_holds_no_state() {
    let (_dir, db) = open().await;
    let found = db.read::<T>().await.unwrap();
    assert!(found.log.is_none());
    assert_eq!(found.seal, Seal::Missing);
}

#[tokio::test]
async fn a_begun_log_replays_what_was_appended() {
    let (_dir, db) = open().await;
    db.begin(1).await.unwrap();
    assert_eq!(log(&db).await, Some(replay::<T>(1, vec![])));
    let records = vec![
        changes(1, vec![C::On]),
        changes(
            2,
            vec![
                bind(7),
                C::Servable(Of {
                    profile: "p".into(),
                    binding: 7,
                }),
            ],
        ),
        // A loosening found with its void is dropped (A.1).
        void(3),
        changes(3, vec![C::Enable("p".into())]),
        changes(4, vec![C::Disable("p".into())]),
    ];
    for record in &records {
        db.append(record).await.unwrap();
    }
    let expected = replay(1, records).unwrap();
    assert!(!expected.off);
    assert_eq!(log(&db).await, Some(Ok(expected)));
}

#[tokio::test]
async fn the_log_begins_where_it_was_begun() {
    let (_dir, db) = open().await;
    db.begin(5).await.unwrap();
    db.append(&changes(5, vec![C::On])).await.unwrap();
    assert_eq!(log(&db).await.unwrap().unwrap().next, 6);

    let (_dir, db) = open().await;
    db.begin(5).await.unwrap();
    db.append(&changes(1, vec![C::On])).await.unwrap();
    assert_eq!(log(&db).await, Some(Err(Damaged)));
}

#[tokio::test]
async fn beginning_again_at_another_number_conflicts() {
    let (_dir, db) = open().await;
    db.begin(1).await.unwrap();
    db.begin(1).await.unwrap();
    assert!(matches!(db.begin(2).await, Err(StoreError::Conflict)));
    assert_eq!(log(&db).await, Some(replay::<T>(1, vec![])));
}

#[tokio::test]
async fn records_without_a_begun_log_are_damaged() {
    let (_dir, db) = open().await;
    db.append(&changes(1, vec![C::On])).await.unwrap();
    assert_eq!(log(&db).await, Some(Err(Damaged)));
}

#[tokio::test]
async fn the_same_write_again_is_kept_once_and_another_conflicts() {
    let (_dir, db) = open().await;
    db.begin(1).await.unwrap();
    let first = changes(1, vec![C::On]);
    db.append(&first).await.unwrap();
    db.append(&first).await.unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM records").await, 1);

    let other = changes(1, vec![C::Off]);
    assert!(matches!(db.append(&other).await, Err(StoreError::Conflict)));
    assert!(db.holds(&first).await.unwrap());
    assert!(!db.holds(&other).await.unwrap());

    // A write and its void share the number (A.1).
    db.append(&void(1)).await.unwrap();
    db.append(&void(1)).await.unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM records").await, 2);
    let expected = replay(1, vec![first, void(1)]).unwrap();
    assert_eq!(log(&db).await, Some(Ok(expected)));
}

#[tokio::test]
async fn holds_tells_only_the_exact_record() {
    let (_dir, db) = open().await;
    let written = changes(2, vec![C::On]);
    assert!(!db.holds(&written).await.unwrap());
    db.append(&written).await.unwrap();
    assert!(db.holds(&written).await.unwrap());
    assert!(!db.holds(&changes(2, vec![C::On, C::Off])).await.unwrap());
    assert!(!db.holds(&changes(3, vec![C::On])).await.unwrap());
    assert!(!db.holds(&void(2)).await.unwrap());
    db.append(&void(2)).await.unwrap();
    assert!(db.holds(&void(2)).await.unwrap());
}

#[tokio::test]
async fn a_record_this_build_cannot_read_exactly_is_damaged() {
    for body in [
        r#"{"version":2,"changes":["on"]}"#,
        r#"{"version":1,"changes":["vanish"]}"#,
        "not json",
    ] {
        let (_dir, db) = open().await;
        db.begin(1).await.unwrap();
        exec(
            &db,
            &format!("INSERT INTO records (seq, kind, body) VALUES (1, 'changes', '{body}')"),
        )
        .await;
        assert_eq!(log(&db).await, Some(Err(Damaged)), "{body}");
    }
}

/// SQLite checks a column's type and `NOT NULL` when writing, not when
/// reading: in a damaged file a number may be anything, and must not read as
/// zero.
#[tokio::test]
async fn a_number_that_is_not_an_integer_reads_as_unreadable() {
    for value in ["NULL", "'7'", "2.5"] {
        let (_dir, db) = open().await;
        db.begin(1).await.unwrap();
        db.rebuild_seal(1).await.unwrap();
        exec(&db, "DROP TABLE seal").await;
        exec(&db, "CREATE TABLE seal (id INTEGER PRIMARY KEY, next)").await;
        exec(&db, &format!("INSERT INTO seal VALUES (1, {value})")).await;
        assert_eq!(db.read::<T>().await.unwrap().seal, Seal::Missing, "{value}");

        exec(&db, "DROP TABLE log").await;
        exec(&db, "CREATE TABLE log (id INTEGER PRIMARY KEY, start)").await;
        exec(&db, &format!("INSERT INTO log VALUES (1, {value})")).await;
        assert_eq!(log(&db).await, Some(Err(Damaged)), "{value}");

        let (_dir, db) = open().await;
        db.begin(1).await.unwrap();
        exec(&db, "DROP TABLE records").await;
        exec(&db, "CREATE TABLE records (seq, kind, body)").await;
        let on = r#"{"version":1,"changes":["on"]}"#;
        exec(
            &db,
            &format!("INSERT INTO records VALUES ({value}, 'changes', '{on}')"),
        )
        .await;
        assert_eq!(log(&db).await, Some(Err(Damaged)), "{value}");
    }
}

/// Without its `CHECK`s, a damaged file may hold any kind with any body:
/// only `changes` with a body and `void` without one read as records.
#[tokio::test]
async fn a_kind_and_body_that_do_not_match_are_damaged() {
    for row in [
        "'changes', NULL",
        "'void', 'x'",
        "'weird', NULL",
        r#"'weird', '{"version":1,"changes":["on"]}'"#,
        "NULL, NULL",
    ] {
        let (_dir, db) = open().await;
        db.begin(1).await.unwrap();
        exec(&db, "DROP TABLE records").await;
        exec(&db, "CREATE TABLE records (seq, kind, body)").await;
        exec(&db, &format!("INSERT INTO records VALUES (1, {row})")).await;
        assert_eq!(log(&db).await, Some(Err(Damaged)), "{row}");
    }
}

/// Text that is not UTF-8 is damaged, not a failure to read, and not read
/// with the byte replaced: the body would then be a change.
#[tokio::test]
async fn text_that_is_not_utf8_is_damaged() {
    let (_dir, db) = open().await;
    db.begin(1).await.unwrap();
    exec(
        &db,
        r#"INSERT INTO records VALUES (1, 'changes',
           '{"version":1,"changes":[{"enable":"' || CAST(x'ff' AS TEXT) || '"}]}')"#,
    )
    .await;
    assert_eq!(log(&db).await, Some(Err(Damaged)));

    let (_dir, db) = open().await;
    db.begin(1).await.unwrap();
    exec(&db, "DROP TABLE records").await;
    exec(&db, "CREATE TABLE records (seq, kind, body)").await;
    exec(
        &db,
        "INSERT INTO records VALUES (1, CAST(x'ff' AS TEXT), NULL)",
    )
    .await;
    assert_eq!(log(&db).await, Some(Err(Damaged)));
}

/// A write that fails is reported, never taken as persisted (A.1).
#[tokio::test]
async fn a_failed_write_is_reported_and_changes_nothing() {
    let (_dir, db) = open().await;
    db.begin(1).await.unwrap();
    let first = changes(1, vec![C::On]);
    db.append(&first).await.unwrap();
    db.rebuild_seal(2).await.unwrap();
    exec(&db, "PRAGMA query_only = ON").await;
    assert!(matches!(db.begin(1).await, Err(StoreError::Sqlx(_))));
    assert!(matches!(db.append(&first).await, Err(StoreError::Sqlx(_))));
    let second = changes(2, vec![C::Off]);
    assert!(matches!(db.append(&second).await, Err(StoreError::Sqlx(_))));
    assert!(matches!(db.rebuild_seal(3).await, Err(StoreError::Sqlx(_))));
    assert!(matches!(db.delete_seal().await, Err(StoreError::Sqlx(_))));
    exec(&db, "PRAGMA query_only = OFF").await;
    let found = db.read::<T>().await.unwrap();
    assert_eq!(found.log, Some(replay(1, vec![first])));
    assert_eq!(found.seal, Seal::Present { next: 2 });
}

#[tokio::test]
async fn the_seal_is_rebuilt_and_deleted() {
    let (_dir, db) = open().await;
    db.rebuild_seal(5).await.unwrap();
    assert_eq!(
        db.read::<T>().await.unwrap().seal,
        Seal::Present { next: 5 }
    );
    db.rebuild_seal(9).await.unwrap();
    assert_eq!(
        db.read::<T>().await.unwrap().seal,
        Seal::Present { next: 9 }
    );
    db.delete_seal().await.unwrap();
    assert_eq!(db.read::<T>().await.unwrap().seal, Seal::Missing);
    db.delete_seal().await.unwrap();
    assert_eq!(db.read::<T>().await.unwrap().seal, Seal::Missing);
}

#[tokio::test]
async fn a_seal_alone_is_no_log() {
    let (_dir, db) = open().await;
    db.rebuild_seal(1).await.unwrap();
    let found = db.read::<T>().await.unwrap();
    assert!(found.log.is_none());
    assert_eq!(found.seal, Seal::Present { next: 1 });
}

#[tokio::test]
async fn a_number_past_sqlite_is_refused() {
    let (_dir, db) = open().await;
    let past = u64::try_from(i64::MAX).unwrap() + 1;
    assert!(matches!(db.begin(past).await, Err(StoreError::Range)));
    assert!(matches!(
        db.rebuild_seal(past).await,
        Err(StoreError::Range)
    ));
    let record = changes(past, vec![C::On]);
    assert!(matches!(db.append(&record).await, Err(StoreError::Range)));
    assert!(matches!(db.holds(&record).await, Err(StoreError::Range)));
    assert!(log(&db).await.is_none());
    assert_eq!(db.read::<T>().await.unwrap().seal, Seal::Missing);
}

#[tokio::test]
async fn what_was_written_survives_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let records = vec![changes(1, vec![C::On]), void(2)];
    {
        let db = KnowledgeDb::open(dir.path()).await.unwrap();
        db.begin(1).await.unwrap();
        for record in &records {
            db.append(record).await.unwrap();
        }
        db.rebuild_seal(3).await.unwrap();
        db.pool().close().await;
    }
    let db = KnowledgeDb::open(dir.path()).await.unwrap();
    let found = db.read::<T>().await.unwrap();
    assert_eq!(found.log, Some(replay(1, records)));
    assert_eq!(found.seal, Seal::Present { next: 3 });
}

#[tokio::test]
async fn an_unknown_or_altered_schema_is_refused_and_kept() {
    for alter in [
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (9999, 'later', 1, x'00', 0)",
        "UPDATE _sqlx_migrations SET checksum = x'00' WHERE version = 1",
    ] {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = KnowledgeDb::open(dir.path()).await.unwrap();
            db.begin(1).await.unwrap();
            db.append(&changes(1, vec![C::On])).await.unwrap();
            exec(&db, alter).await;
            db.pool().close().await;
        }
        let reopened = KnowledgeDb::open(dir.path()).await;
        assert!(matches!(reopened, Err(StoreError::Migrate(_))), "{alter}");

        // Left as it was: the record is still there.
        let file = dir.path().join("knowledge").join("knowledge.db");
        let raw = SqlitePool::connect_with(SqliteConnectOptions::new().filename(file))
            .await
            .unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM records")
            .fetch_one(&raw)
            .await
            .unwrap();
        assert_eq!(n, 1, "{alter}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn an_unwritable_database_is_refused_at_open() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    {
        let db = KnowledgeDb::open(dir.path()).await.unwrap();
        db.pool().close().await;
    }
    let file = dir.path().join("knowledge").join("knowledge.db");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
    assert!(KnowledgeDb::open(dir.path()).await.is_err());
}
