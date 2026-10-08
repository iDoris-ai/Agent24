//! 0005_job_input: a job's input is a JSON object or NULL.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

const JOB: &str = "job_01K75A0B1C2D3E4F5G6H7J8K9M";
const INSERT: &str = "INSERT INTO jobs (id, kind, status, origin, input)
                      VALUES (?, 'import', 'queued', '{\"kind\":\"page\"}', ?)";

async fn insert(db: &Db, input: Option<&str>) -> Result<(), sqlx::Error> {
    sqlx::query(INSERT)
        .bind(JOB)
        .bind(input)
        .execute(db.pool())
        .await
        .map(|_| ())
}

#[tokio::test]
async fn a_job_input_is_a_json_object_or_null() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path()).await.unwrap();
    // A NUL after a valid object would hide bytes from json_valid.
    for bad in ["not json", "[1]", "\"text\"", "1", "{\"a\":1}\u{0}junk"] {
        let err = insert(&db, Some(bad)).await.unwrap_err();
        assert!(
            err.to_string().contains("CHECK constraint failed"),
            "{bad:?}: {err}"
        );
    }
    insert(&db, None).await.unwrap();
    sqlx::query("UPDATE jobs SET input = json_object('upload_id', 'upl_x', 'title', 'T')")
        .execute(db.pool())
        .await
        .unwrap();
    let title: String = sqlx::query_scalar("SELECT input ->> '$.title' FROM jobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(title, "T");
}

#[tokio::test]
async fn a_job_input_once_set_is_fixed() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path()).await.unwrap();
    insert(&db, Some("{\"upload_id\":\"a\"}")).await.unwrap();
    for set in ["NULL", "json_object('upload_id', 'b')"] {
        let err = sqlx::query(&format!("UPDATE jobs SET input = {set}"))
            .execute(db.pool())
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("a job input cannot change once set"),
            "{set}: {err}"
        );
    }
    // Codex: REPLACE deletes first, so it is refused as a delete.
    let replace = format!(
        "INSERT OR REPLACE INTO jobs (id, kind, status, origin, input)
         VALUES ('{JOB}', 'import', 'queued', '{{\"kind\":\"page\"}}', json_object('upload_id', 'b'))"
    );
    let err = sqlx::query(&replace).execute(db.pool()).await.unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("jobs are never deleted") || text.contains("a job id is never reused"),
        "{err}"
    );
    let err = sqlx::query("DELETE FROM jobs")
        .execute(db.pool())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("jobs are never deleted"), "{err}");
    // Codex: renaming the job, then reusing its id, would swap the input.
    for set in [
        "id = 'job_01K75A0B1C2D3E4F5G6H7J8K9N'",
        "kind = 'extract'",
        "origin = '{\"kind\":\"run\",\"run_id\":\"r\",\"tool_call_id\":\"t\"}'",
        "created_at = '2020-01-01T00:00:00.000Z'",
    ] {
        let err = sqlx::query(&format!("UPDATE jobs SET {set}"))
            .execute(db.pool())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cannot change"), "{set}: {err}");
    }
    // An upsert's update half, and NULLs that OR REPLACE would fill in.
    let upsert = format!(
        "INSERT INTO jobs (id, kind, status, origin, input)
         VALUES ('{JOB}', 'import', 'queued', '{{\"kind\":\"page\"}}', NULL)
         ON CONFLICT (id) DO UPDATE SET input = json_object('upload_id', 'b')"
    );
    assert!(sqlx::query(&upsert).execute(db.pool()).await.is_err());
    for set in ["created_at = NULL", "kind = NULL", "input = NULL"] {
        let sql = format!("UPDATE OR REPLACE jobs SET {set}");
        assert!(sqlx::query(&sql).execute(db.pool()).await.is_err(), "{set}");
    }
    let input: String = sqlx::query_scalar("SELECT input FROM jobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(input, "{\"upload_id\":\"a\"}");
    // Other columns still move.
    sqlx::query("UPDATE jobs SET status = 'running'")
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn jobs_from_before_0005_keep_a_null_input() {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    let dir = tempfile::tempdir().unwrap();
    let all = sqlx::migrate!("./migrations");
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(all.migrations[..4].to_vec()),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    let options = SqliteConnectOptions::new()
        .filename(dir.path().join(DB_FILE))
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    before.run(&pool).await.unwrap();
    sqlx::query(&format!(
        "INSERT INTO jobs (id, kind, status, origin) VALUES ('{JOB}', 'import', 'failed', '{{\"kind\":\"page\"}}')"
    ))
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    let db = Db::open(dir.path()).await.unwrap();
    let (status, input): (String, Option<String>) =
        sqlx::query_as("SELECT status, input FROM jobs")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!((status.as_str(), input), ("failed", None));
}

#[tokio::test]
async fn replace_is_refused_even_without_recursive_triggers() {
    use sqlx::Connection;
    use sqlx::sqlite::SqliteConnectOptions;
    const UPL: &str = "upl_01K74Z3QJ8V5N2W9RTX6YB4MCD";
    const SHA: &str = "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path()).await.unwrap();
    insert(&db, Some("{\"upload_id\":\"a\"}")).await.unwrap();
    for sql in [
        format!("INSERT INTO uploads (id, total_size, sha256) VALUES ('{UPL}', 10, '{SHA}')"),
        format!(
            "INSERT INTO upload_chunks (upload_id, chunk_offset, chunk_size, sha256) VALUES ('{UPL}', 0, 4, '{SHA}')"
        ),
    ] {
        sqlx::query(&sql).execute(db.pool()).await.unwrap();
    }
    db.pool().close().await;
    let mut raw = sqlx::SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(dir.path().join(DB_FILE)),
    )
    .await
    .unwrap();
    let rt: i64 = sqlx::query_scalar("PRAGMA recursive_triggers")
        .fetch_one(&mut raw)
        .await
        .unwrap();
    assert_eq!(rt, 0, "this connection must not have recursive triggers");
    for (sql, message) in [
        (
            format!(
                "INSERT OR REPLACE INTO jobs (id, kind, status, origin, input) VALUES ('{JOB}', 'import', 'queued', '{{\"kind\":\"page\"}}', '{{\"upload_id\":\"b\"}}')"
            ),
            "a job id is never reused",
        ),
        (
            format!(
                "INSERT OR REPLACE INTO uploads (id, total_size, sha256) VALUES ('{UPL}', 99, '{SHA}')"
            ),
            "an upload id is never reused",
        ),
        (
            format!(
                "INSERT OR REPLACE INTO upload_chunks (upload_id, chunk_offset, chunk_size, sha256) VALUES ('{UPL}', 0, 2, '{SHA}')"
            ),
            "a recorded chunk is never replaced",
        ),
    ] {
        let err = sqlx::query(&sql).execute(&mut raw).await.unwrap_err();
        assert!(err.to_string().contains(message), "{sql}: {err}");
    }
    let (total, input): (i64, String) =
        sqlx::query_as("SELECT (SELECT total_size FROM uploads), (SELECT input FROM jobs)")
            .fetch_one(&mut raw)
            .await
            .unwrap();
    assert_eq!((total, input.as_str()), (10, "{\"upload_id\":\"a\"}"));
    // The same through the hidden rowid: a fresh business key on an
    // occupied rowid, or a rowid moved onto another row.
    const UPL2: &str = "upl_01K74Z3QJ8V5N2W9RTX6YB4MCE";
    let rowid = |table: &str| format!("(SELECT rowid FROM {table} LIMIT 1)");
    for (sql, message) in [
        (
            format!(
                "INSERT OR REPLACE INTO jobs (rowid, id, kind, status, origin) VALUES ({}, 'job_01K75A0B1C2D3E4F5G6H7J8K9N', 'import', 'queued', '{{\"kind\":\"page\"}}')",
                rowid("jobs")
            ),
            "a job id is never reused",
        ),
        (
            format!(
                "INSERT OR REPLACE INTO uploads (rowid, id, total_size, sha256) VALUES ({}, '{UPL2}', 5, '{SHA}')",
                rowid("uploads")
            ),
            "an upload id is never reused",
        ),
        (
            format!(
                "INSERT OR REPLACE INTO upload_chunks (rowid, upload_id, chunk_offset, chunk_size, sha256) VALUES ({}, '{UPL}', 4, 2, '{SHA}')",
                rowid("upload_chunks")
            ),
            "a recorded chunk is never replaced",
        ),
    ] {
        let err = sqlx::query(&sql).execute(&mut raw).await.unwrap_err();
        assert!(err.to_string().contains(message), "{sql}: {err}");
    }
    sqlx::query("INSERT INTO jobs (id, kind, status, origin) VALUES ('job_01K75A0B1C2D3E4F5G6H7J8K9P', 'import', 'queued', '{\"kind\":\"page\"}')")
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query(&format!(
        "INSERT INTO uploads (id, total_size, sha256) VALUES ('{UPL2}', 5, '{SHA}')"
    ))
    .execute(&mut raw)
    .await
    .unwrap();
    for (sql, message) in [
        (
            "UPDATE OR REPLACE jobs SET rowid = (SELECT min(rowid) FROM jobs) WHERE rowid = (SELECT max(rowid) FROM jobs)",
            "a job rowid cannot change",
        ),
        (
            "UPDATE OR REPLACE uploads SET rowid = (SELECT min(rowid) FROM uploads) WHERE rowid = (SELECT max(rowid) FROM uploads)",
            "an upload rowid cannot change",
        ),
    ] {
        let err = sqlx::query(sql).execute(&mut raw).await.unwrap_err();
        assert!(err.to_string().contains(message), "{sql}: {err}");
    }
    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM jobs), (SELECT count(*) FROM uploads), (SELECT count(*) FROM upload_chunks)",
    )
    .fetch_one(&mut raw)
    .await
    .unwrap();
    assert_eq!(counts, (2, 2, 1));
}

#[tokio::test]
async fn rowids_are_positive_so_automatic_ones_never_collide() {
    const UPL: &str = "upl_01K74Z3QJ8V5N2W9RTX6YB4MCD";
    const SHA: &str = "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path()).await.unwrap();
    let job = |id: &str| {
        format!(
            "INSERT INTO jobs (rowid, id, kind, status, origin) VALUES (?, '{id}', 'import', 'queued', '{{\"kind\":\"page\"}}')"
        )
    };
    let upload = |id: &str| {
        format!(
            "INSERT INTO uploads (rowid, id, total_size, sha256) VALUES (?, '{id}', 10, '{SHA}')"
        )
    };
    let chunk = |offset: u8| {
        format!(
            "INSERT INTO upload_chunks (rowid, upload_id, chunk_offset, chunk_size, sha256) VALUES (?, '{UPL}', {offset}, 2, '{SHA}')"
        )
    };
    // Automatic rowids (NULL) go through every guard.
    for sql in [job(JOB), upload(UPL), chunk(0)] {
        sqlx::query(&sql)
            .bind(None::<i64>)
            .execute(db.pool())
            .await
            .unwrap();
    }
    sqlx::query("UPDATE uploads SET received = 2")
        .execute(db.pool())
        .await
        .unwrap();
    for sql in [
        job("job_01K75A0B1C2D3E4F5G6H7J8K9N"),
        upload("upl_01K74Z3QJ8V5N2W9RTX6YB4MCE"),
        chunk(2),
    ] {
        for rowid in [-1_i64, 0] {
            let err = sqlx::query(&sql)
                .bind(rowid)
                .execute(db.pool())
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("rowids are positive"),
                "{sql} with {rowid}: {err}"
            );
        }
    }
    // Omitted rowids too.
    sqlx::query(&format!(
        "INSERT INTO upload_chunks (upload_id, chunk_offset, chunk_size, sha256) VALUES ('{UPL}', 2, 2, '{SHA}')"
    ))
    .execute(db.pool())
    .await
    .unwrap();
    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM jobs), (SELECT count(*) FROM uploads), (SELECT count(*) FROM upload_chunks)",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(counts, (1, 1, 2));
}

#[tokio::test]
async fn a_database_with_a_non_positive_rowid_is_not_migrated() {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    const UPL: &str = "upl_01K74Z3QJ8V5N2W9RTX6YB4MCD";
    const SHA: &str = "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    let upload =
        format!("INSERT INTO uploads (id, total_size, sha256) VALUES ('{UPL}', 10, '{SHA}')");
    for (table, rows) in [
        (
            "jobs",
            vec![format!(
                "INSERT INTO jobs (rowid, id, kind, status, origin) VALUES (0, '{JOB}', 'import', 'failed', '{{\"kind\":\"page\"}}')"
            )],
        ),
        (
            "uploads",
            vec![format!(
                "INSERT INTO uploads (rowid, id, total_size, sha256) VALUES (-1, '{UPL}', 10, '{SHA}')"
            )],
        ),
        (
            "upload_chunks",
            vec![
                upload.clone(),
                format!(
                    "INSERT INTO upload_chunks (rowid, upload_id, chunk_offset, chunk_size, sha256) VALUES (-1, '{UPL}', 0, 2, '{SHA}')"
                ),
            ],
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let all = sqlx::migrate!("./migrations");
        let before = sqlx::migrate::Migrator {
            migrations: std::borrow::Cow::Owned(all.migrations[..4].to_vec()),
            ..sqlx::migrate::Migrator::DEFAULT
        };
        let options = SqliteConnectOptions::new()
            .filename(dir.path().join(DB_FILE))
            .create_if_missing(true);
        // One connection throughout, so its TEMP schema can be inspected.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        before.run(&pool).await.unwrap();
        for sql in &rows {
            sqlx::query(sql).execute(&pool).await.unwrap();
        }
        let err = all.run(&pool).await.unwrap_err();
        assert!(
            err.to_string().contains("CHECK constraint failed"),
            "{table}: {err}"
        );
        // The failed migration is rolled back whole: still version 4, no
        // input column, no 0005 trigger, no scratch table left behind.
        let left: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT max(version) FROM _sqlx_migrations WHERE success),
                    (SELECT count(*) FROM pragma_table_info('jobs') WHERE name = 'input'),
                    (SELECT count(*) FROM sqlite_master WHERE name = 'jobs_input_is_fixed'),
                    (SELECT count(*) FROM sqlite_temp_master)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(left, (4, 0, 0, 0), "{table}");
        pool.close().await;
        assert!(Db::open(dir.path()).await.is_err(), "{table}");
    }
}
