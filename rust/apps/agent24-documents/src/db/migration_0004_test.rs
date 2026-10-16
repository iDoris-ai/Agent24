//! 0004_upload_chunks: the upload name, chunk records, and how `received`
//! may move (ADR-DOC-02 §5.6).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

const UPL: &str = "upl_01K74Z3QJ8V5N2W9RTX6YB4MCD";
const SHA: &str = "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
const SHA2: &str = "sha256:486ea46224d1bb4fb680f34f7c9ad96a8f24ec88be73ea8e5a6c65260e9cb8a7";

async fn open() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path()).await.unwrap();
    (dir, db)
}

async fn exec(db: &Db, sql: &str) -> Result<(), sqlx::Error> {
    sqlx::query(sql).execute(db.pool()).await.map(|_| ())
}

/// The trigger's own message, so a test cannot pass on some other failure.
/// `message` may list alternatives as `a|b`. A REPLACE is refused by 0005's
/// BEFORE INSERT guards before SQLite deletes anything, so their message is
/// the one seen; the delete guard's message is what it would be without 0005.
async fn refused(db: &Db, sql: &str, message: &str) {
    let err = exec(db, sql).await.expect_err(sql);
    let text = err.to_string();
    assert!(
        message.split('|').any(|m| text.contains(m)),
        "{sql}: {text}"
    );
}

async fn upload(db: &Db, total: i64) {
    exec(
        db,
        &format!("INSERT INTO uploads (id, total_size, sha256) VALUES ('{UPL}', {total}, '{SHA}')"),
    )
    .await
    .unwrap();
}

fn chunk(offset: i64, size: i64) -> String {
    format!(
        "INSERT INTO upload_chunks (upload_id, chunk_offset, chunk_size, sha256) VALUES ('{UPL}', {offset}, {size}, '{SHA}')"
    )
}

fn received(n: i64) -> String {
    format!("UPDATE uploads SET received = {n} WHERE id = '{UPL}'")
}

#[tokio::test]
async fn the_filename_is_a_base_name_of_1_to_255_characters() {
    let (_dir, db) = open().await;
    let with = |name: &str| {
        format!(
            "INSERT INTO uploads (id, total_size, sha256, filename) VALUES ('{UPL}', 1, '{SHA}', '{name}')"
        )
    };
    for bad in ["", "a/b.pdf", "a\\b.pdf", &"x".repeat(256)] {
        refused(&db, &with(bad), "CHECK constraint failed").await;
    }
    // Characters, not bytes: 255 CJK characters are 765 bytes.
    exec(&db, &with(&"文".repeat(255))).await.unwrap();
}

#[tokio::test]
async fn chunks_append_at_the_bytes_received_and_fit_the_size() {
    let (_dir, db) = open().await;
    upload(&db, 10).await;
    const MSG: &str = "a chunk must start at the bytes received so far";
    refused(&db, &chunk(1, 4), MSG).await;
    exec(&db, &chunk(0, 4)).await.unwrap();
    exec(&db, &received(4)).await.unwrap();
    refused(&db, &chunk(6, 4), MSG).await; // a gap
    refused(&db, &chunk(4, 7), MSG).await; // past total_size
    exec(&db, &chunk(4, 6)).await.unwrap();
    exec(&db, &received(10)).await.unwrap();
    exec(
        &db,
        &format!("UPDATE uploads SET status = 'complete' WHERE id = '{UPL}'"),
    )
    .await
    .unwrap();
    // Already complete: no more chunks, even at the right offset.
    refused(&db, &chunk(10, 1), MSG).await;
}

#[tokio::test]
async fn an_upload_that_stopped_receiving_takes_no_chunk_that_would_fit() {
    let (_dir, db) = open().await;
    upload(&db, 10).await;
    exec(&db, &chunk(0, 4)).await.unwrap();
    exec(&db, &received(4)).await.unwrap();
    exec(
        &db,
        &format!("UPDATE uploads SET status = 'expired' WHERE id = '{UPL}'"),
    )
    .await
    .unwrap();
    refused(
        &db,
        &chunk(4, 2),
        "a chunk must start at the bytes received so far",
    )
    .await;
}

#[tokio::test]
async fn a_chunk_is_between_1_byte_and_768_kib() {
    let (_dir, db) = open().await;
    upload(&db, 2_000_000).await;
    refused(&db, &chunk(0, 0), "CHECK constraint failed").await;
    refused(&db, &chunk(0, 786_433), "CHECK constraint failed").await;
    exec(&db, &chunk(0, 786_432)).await.unwrap();
}

#[tokio::test]
async fn received_moves_only_to_the_end_of_the_recorded_chunks() {
    let (_dir, db) = open().await;
    upload(&db, 10).await;
    const MSG: &str = "received must equal the end of the recorded chunks";
    refused(&db, &received(3), MSG).await;
    exec(&db, &chunk(0, 4)).await.unwrap();
    refused(&db, &received(5), MSG).await;
    refused(&db, &received(3), MSG).await;
    exec(&db, &received(4)).await.unwrap();
    // Other columns can still change.
    exec(
        &db,
        &format!(
            "UPDATE uploads SET last_chunk_at = '2026-10-09T00:00:00.000Z' WHERE id = '{UPL}'"
        ),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn chunks_are_immutable_and_outlive_a_live_upload() {
    let (_dir, db) = open().await;
    upload(&db, 10).await;
    exec(&db, &chunk(0, 4)).await.unwrap();
    refused(
        &db,
        &format!("UPDATE upload_chunks SET chunk_size = 3 WHERE upload_id = '{UPL}'"),
        "upload chunks are immutable",
    )
    .await;
    let delete = format!("DELETE FROM upload_chunks WHERE upload_id = '{UPL}'");
    refused(&db, &delete, "chunks of a live upload cannot be deleted").await;
    // REPLACE is refused by 0005's BEFORE INSERT guard before anything is
    // deleted; the delete message is what 0004 alone would give (#834 review).
    refused(
        &db,
        &format!("REPLACE INTO upload_chunks (upload_id, chunk_offset, chunk_size, sha256) VALUES ('{UPL}', 0, 4, '{SHA}')"),
        "chunks of a live upload cannot be deleted|a recorded chunk is never replaced",
    )
    .await;
    // Complete but not yet imported: the chunks are still needed.
    exec(&db, &received(4)).await.unwrap();
    exec(&db, &chunk(4, 6)).await.unwrap();
    exec(&db, &received(10)).await.unwrap();
    exec(
        &db,
        &format!("UPDATE uploads SET status = 'complete' WHERE id = '{UPL}'"),
    )
    .await
    .unwrap();
    refused(&db, &delete, "chunks of a live upload cannot be deleted").await;
    exec(
        &db,
        &format!("UPDATE uploads SET status = 'expired' WHERE id = '{UPL}'"),
    )
    .await
    .unwrap();
    exec(&db, &delete).await.unwrap();
}

fn status(to: &str) -> String {
    format!("UPDATE uploads SET status = '{to}' WHERE id = '{UPL}'")
}

/// Binds `value` whole, embedded NUL included.
async fn insert_bound(db: &Db, sql: &str, value: &str) -> Result<(), sqlx::Error> {
    sqlx::query(sql)
        .bind(value)
        .execute(db.pool())
        .await
        .map(|_| ())
}

#[tokio::test]
async fn an_upload_starts_empty_and_receiving() {
    let (_dir, db) = open().await;
    const MSG: &str = "an upload starts empty and receiving";
    let insert = |received: i64, status: &str| {
        format!(
            "INSERT INTO uploads (id, total_size, sha256, received, status) VALUES ('{UPL}', 10, '{SHA}', {received}, '{status}')"
        )
    };
    refused(&db, &insert(4, "receiving"), MSG).await;
    refused(&db, &insert(0, "expired"), MSG).await;
    exec(&db, &insert(0, "receiving")).await.unwrap();
}

#[tokio::test]
async fn hidden_bytes_after_a_nul_are_refused() {
    let (_dir, db) = open().await;
    let with_id = format!("INSERT INTO uploads (id, total_size, sha256) VALUES (?, 10, '{SHA}')");
    for (sql, value) in [
        (with_id.clone(), format!("{UPL}\u{0}x")),
        (
            format!("INSERT INTO uploads (id, total_size, sha256) VALUES ('{UPL}', 10, ?)"),
            format!("{SHA}\u{0}x"),
        ),
        (
            format!(
                "INSERT INTO uploads (id, total_size, sha256, filename) VALUES ('{UPL}', 10, '{SHA}', ?)"
            ),
            "a\u{0}/b".to_owned(),
        ),
        (
            format!(
                "INSERT INTO uploads (id, total_size, sha256, filename) VALUES ('{UPL}', 10, '{SHA}', ?)"
            ),
            format!("a\u{0}{}", "x".repeat(256)),
        ),
    ] {
        assert!(insert_bound(&db, &sql, &value).await.is_err(), "{value:?}");
    }
    upload(&db, 10).await;
    let chunk_sha = format!(
        "INSERT INTO upload_chunks (upload_id, chunk_offset, chunk_size, sha256) VALUES ('{UPL}', 0, 4, ?)"
    );
    assert!(
        insert_bound(&db, &chunk_sha, &format!("{SHA}\u{0}x"))
            .await
            .is_err()
    );
    // Codex R2: 71 bytes in all, with the NUL inside.
    let inside = format!("sha256:{}\u{0}{}", "a".repeat(10), "X".repeat(53));
    assert_eq!(inside.len(), 71);
    assert!(insert_bound(&db, &chunk_sha, &inside).await.is_err());
    insert_bound(&db, &chunk_sha, SHA).await.unwrap();
}

#[tokio::test]
async fn an_upload_is_never_deleted_or_replaced() {
    let (_dir, db) = open().await;
    upload(&db, 10).await;
    exec(&db, &chunk(0, 4)).await.unwrap();
    exec(&db, &received(4)).await.unwrap();
    const MSG: &str = "uploads are never deleted";
    refused(&db, &format!("DELETE FROM uploads WHERE id = '{UPL}'"), MSG).await;
    // Codex: a replacement row could claim all 10 bytes with only 4 recorded.
    refused(
        &db,
        &format!("INSERT OR REPLACE INTO uploads (id, total_size, sha256, received, status) VALUES ('{UPL}', 10, '{SHA}', 0, 'receiving')"),
        "uploads are never deleted|an upload id is never reused",
    )
    .await;
}

#[tokio::test]
async fn the_declaration_never_changes() {
    let (_dir, db) = open().await;
    upload(&db, 10).await;
    const MSG: &str = "an upload declaration cannot change";
    for set in [
        "total_size = 20".to_owned(),
        format!("sha256 = '{}'", SHA2),
        "filename = 'other.pdf'".to_owned(),
        "created_at = '2020-01-01T00:00:00.000Z'".to_owned(),
    ] {
        refused(
            &db,
            &format!("UPDATE uploads SET {set} WHERE id = '{UPL}'"),
            MSG,
        )
        .await;
    }
}

#[tokio::test]
async fn status_only_moves_forward() {
    const MSG: &str = "upload status only moves forward";
    // receiving → imported skips complete.
    let (_dir, db) = open().await;
    upload(&db, 4).await;
    refused(&db, &status("imported"), MSG).await;
    exec(&db, &chunk(0, 4)).await.unwrap();
    exec(
        &db,
        &format!("UPDATE uploads SET received = 4, status = 'complete' WHERE id = '{UPL}'"),
    )
    .await
    .unwrap();
    refused(&db, &status("receiving"), MSG).await;
    // Codex R2: NULL with OR REPLACE would become the 'receiving' default.
    refused(
        &db,
        &format!("UPDATE OR REPLACE uploads SET status = NULL WHERE id = '{UPL}'"),
        MSG,
    )
    .await;
    exec(&db, &status("imported")).await.unwrap();
    for back in ["complete", "receiving", "expired"] {
        refused(&db, &status(back), MSG).await;
    }
    // receiving → expired is final, and expired bytes cannot be reset.
    let (_dir, db) = open().await;
    upload(&db, 10).await;
    exec(&db, &chunk(0, 4)).await.unwrap();
    exec(&db, &received(4)).await.unwrap();
    exec(&db, &status("expired")).await.unwrap();
    refused(&db, &status("receiving"), MSG).await;
    exec(
        &db,
        &format!("DELETE FROM upload_chunks WHERE upload_id = '{UPL}'"),
    )
    .await
    .unwrap();
    refused(
        &db,
        &received(0),
        "received must equal the end of the recorded chunks",
    )
    .await;
}

#[tokio::test]
async fn a_chunk_is_recorded_and_completed_in_one_transaction() {
    let (_dir, db) = open().await;
    upload(&db, 10).await;
    // Rolled back: nothing of it remains.
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
    sqlx::query(&chunk(0, 10)).execute(&mut *tx).await.unwrap();
    sqlx::query(&format!(
        "UPDATE uploads SET received = 10, status = 'complete' WHERE id = '{UPL}'"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    let chunks: i64 = sqlx::query_scalar("SELECT count(*) FROM upload_chunks")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(chunks, 0);
    // Committed: the handler's sequence works as one step.
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
    sqlx::query(&chunk(0, 10)).execute(&mut *tx).await.unwrap();
    sqlx::query(&format!(
        "UPDATE uploads SET received = 10, status = 'complete' WHERE id = '{UPL}'"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (received, status): (i64, String) = sqlx::query_as(&format!(
        "SELECT received, status FROM uploads WHERE id = '{UPL}'"
    ))
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!((received, status.as_str()), (10, "complete"));
}

#[tokio::test]
async fn an_upsert_cannot_rewrite_a_recorded_chunk() {
    let (_dir, db) = open().await;
    upload(&db, 10).await;
    exec(&db, &chunk(0, 4)).await.unwrap();
    let upsert = |sha: &str| {
        format!(
            "INSERT INTO upload_chunks (upload_id, chunk_offset, chunk_size, sha256) VALUES ('{UPL}', 0, 4, '{sha}') \
             ON CONFLICT (upload_id, chunk_offset) DO UPDATE SET sha256 = excluded.sha256"
        )
    };
    // The insert half is refused (0005's guard, or 0004's append rule once
    // `received` has moved), so the update half never runs.
    refused(
        &db,
        &upsert(SHA2),
        "upload chunks are immutable|a recorded chunk is never replaced",
    )
    .await;
    exec(&db, &received(4)).await.unwrap();
    refused(
        &db,
        &upsert(SHA2),
        "a chunk must start at the bytes received so far|a recorded chunk is never replaced",
    )
    .await;
    let sha: String = sqlx::query_scalar("SELECT sha256 FROM upload_chunks")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(sha, SHA);
}

#[tokio::test]
async fn an_upload_from_before_0004_is_closed() {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    let dir = tempfile::tempdir().unwrap();
    // 0001–0003 only. The fields are doc-hidden; sqlx is pinned in Cargo.lock.
    let all = sqlx::migrate!("./migrations");
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(all.migrations[..3].to_vec()),
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
    // Before 0004 a row could claim bytes without any chunk record.
    sqlx::query(&format!(
        "INSERT INTO uploads (id, total_size, sha256, received) VALUES ('{UPL}', 10, '{SHA}', 4)"
    ))
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let db = Db::open(dir.path()).await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM uploads")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(status, "expired");
}
