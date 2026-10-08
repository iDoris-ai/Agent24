//! `documents.db` (ADR-DOC-02 §2): the reference rows over the blob store.
//!
//! Pragmas are set explicitly on every pooled connection rather than relying
//! on driver defaults:
//! - `journal_mode=WAL` and `synchronous=FULL`: with `NORMAL` a power loss can
//!   drop the last committed transactions, and a revision the user saw as
//!   saved would disappear (§2.2). Commits are rare, so `FULL` costs little.
//! - `foreign_keys=ON`: revisions and jobs cannot point at a missing document.
//! - `recursive_triggers=ON`: `INSERT OR REPLACE` and upserts then fire the
//!   delete triggers that keep revisions, text layers and the oplog fixed.

use std::path::Path;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

pub const DB_FILE: &str = "documents.db";

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

#[derive(Clone)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    /// Open (creating if needed) `<data_dir>/documents.db` and run migrations.
    pub async fn open(data_dir: &Path) -> Result<Self, DbError> {
        // `filename`, not a `sqlite://` URL: a path may contain `?` or `%`.
        let options = SqliteConnectOptions::new()
            .filename(data_dir.join(DB_FILE))
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .pragma("recursive_triggers", "ON")
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4MCD";
    const DOC2: &str = "doc_01K74Z3QJ8V5N2W9RTX6YB4MCE";
    const JOB: &str = "job_01K74Z3QJ8V5N2W9RTX6YB4MCD";
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

    async fn add_document(db: &Db, id: &str) -> Result<(), sqlx::Error> {
        exec(
            db,
            &format!("INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{id}', 't', 'application/pdf', 1)"),
        )
        .await
    }

    async fn add_revision(db: &Db, id: &str, rev: &str, sha: &str) -> Result<(), sqlx::Error> {
        exec(
            db,
            &format!(
                "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin) \
                 VALUES ('{id}', {rev}, '{sha}', 1, 'application/pdf', 'import')"
            ),
        )
        .await
    }

    async fn count(db: &Db, sql: &str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
    }

    #[tokio::test]
    async fn every_pooled_connection_has_the_required_pragmas() {
        let (_dir, db) = open().await;
        let mut conns = Vec::new();
        for _ in 0..4 {
            conns.push(db.pool().acquire().await.unwrap());
        }
        for conn in &mut conns {
            let q = |sql: &'static str| sqlx::query_scalar::<_, String>(sql);
            assert_eq!(
                q("PRAGMA journal_mode")
                    .fetch_one(&mut **conn)
                    .await
                    .unwrap(),
                "wal"
            );
            let n = |sql: &'static str| sqlx::query_scalar::<_, i64>(sql);
            assert_eq!(
                n("PRAGMA synchronous")
                    .fetch_one(&mut **conn)
                    .await
                    .unwrap(),
                2,
                "2 = FULL"
            );
            assert_eq!(
                n("PRAGMA foreign_keys")
                    .fetch_one(&mut **conn)
                    .await
                    .unwrap(),
                1
            );
            assert_eq!(
                n("PRAGMA recursive_triggers")
                    .fetch_one(&mut **conn)
                    .await
                    .unwrap(),
                1
            );
        }
    }

    #[tokio::test]
    async fn reopening_keeps_data_and_paths_with_url_characters_work() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a?b%20c");
        std::fs::create_dir_all(&dir).unwrap();
        add_document(&Db::open(&dir).await.unwrap(), DOC)
            .await
            .unwrap();
        let db = Db::open(&dir).await.unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM documents").await, 1);
        assert!(
            dir.join(DB_FILE).is_file(),
            "the file is where the path says"
        );
    }

    #[tokio::test]
    async fn ids_are_case_sensitive_prefixed_ulids_and_never_null() {
        let (_dir, db) = open().await;
        add_document(&db, DOC).await.unwrap();
        for bad in [
            "DOC_01K74Z3QJ8V5N2W9RTX6YB4MCF",
            "doc_",
            "doc_a",
            "docX01K74Z3QJ8V5N2W9RTX6YB4MCF",
            "doc_81K74Z3QJ8V5N2W9RTX6YB4MCF",
            "doc_01K74Z3QJ8V5N2W9RTX6YB4MCI",
        ] {
            assert!(add_document(&db, bad).await.is_err(), "{bad}");
        }
        let null_id = "INSERT INTO documents (id, title, media_type, head_revision) VALUES (NULL, 't', 'x', 1)";
        assert!(exec(&db, null_id).await.is_err());
        let job = |id: &str| {
            format!(
                "INSERT INTO jobs (id, kind, status, origin) VALUES ('{id}', 'import', 'queued', '{{\"kind\":\"page\"}}')"
            )
        };
        exec(&db, &job(JOB)).await.unwrap();
        assert!(
            exec(&db, &job("JOB_01K74Z3QJ8V5N2W9RTX6YB4MCE"))
                .await
                .is_err()
        );
        let upl = |id: &str| {
            format!("INSERT INTO uploads (id, total_size, sha256) VALUES ('{id}', 10, '{SHA}')")
        };
        exec(&db, &upl(UPL)).await.unwrap();
        assert!(exec(&db, &upl("upl_x")).await.is_err());
    }

    #[tokio::test]
    async fn content_addresses_must_be_sha256_lowercase_hex() {
        let (_dir, db) = open().await;
        add_document(&db, DOC).await.unwrap();
        add_revision(&db, DOC, "1", SHA).await.unwrap();
        for bad in [
            "sha256:",
            "sha256:00",
            "SHA256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            "sha256:2CF24DBA5FB0A30E26E83B2AC5B9E29E1B161E5C1FA7425E73043362938B9824",
            "sha256:zzf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        ] {
            assert!(add_revision(&db, DOC, "2", bad).await.is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn integer_columns_hold_integers_only() {
        let (_dir, db) = open().await;
        add_document(&db, DOC).await.unwrap();
        assert!(add_revision(&db, DOC, "1.5", SHA).await.is_err());
        assert!(add_revision(&db, DOC, "'oops'", SHA).await.is_err());
        assert!(add_revision(&db, DOC, "0", SHA).await.is_err());
    }

    #[tokio::test]
    async fn revision_numbers_are_unique_per_document_not_globally() {
        let (_dir, db) = open().await;
        add_document(&db, DOC).await.unwrap();
        add_document(&db, DOC2).await.unwrap();
        add_revision(&db, DOC, "1", SHA).await.unwrap();
        add_revision(&db, DOC, "2", SHA2).await.unwrap();
        add_revision(&db, DOC2, "1", SHA).await.unwrap();
        assert!(
            add_revision(&db, DOC, "1", SHA2).await.is_err(),
            "UNIQUE (document_id, revision)"
        );
        assert!(
            add_revision(&db, "doc_01K74Z3QJ8V5N2W9RTX6YB4MCF", "1", SHA)
                .await
                .is_err(),
            "foreign key"
        );
    }

    #[tokio::test]
    async fn revisions_cannot_be_changed_deleted_or_replaced() {
        let (_dir, db) = open().await;
        add_document(&db, DOC).await.unwrap();
        add_revision(&db, DOC, "1", SHA).await.unwrap();
        let upd = exec(
            &db,
            &format!("UPDATE revisions SET content_sha256 = '{SHA2}'"),
        )
        .await;
        assert!(
            upd.unwrap_err()
                .to_string()
                .contains("revisions are immutable")
        );
        let del = exec(&db, "DELETE FROM revisions").await;
        assert!(del.unwrap_err().to_string().contains("never deleted"));
        let replace = format!(
            "INSERT OR REPLACE INTO revisions (document_id, revision, content_sha256, size, media_type, origin) \
             VALUES ('{DOC}', 1, '{SHA2}', 9, 'application/pdf', 'commit')"
        );
        assert!(
            exec(&db, &replace).await.is_err(),
            "REPLACE must not overwrite a revision"
        );
        let sha: String = sqlx::query_scalar("SELECT content_sha256 FROM revisions")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(sha, SHA);
    }

    #[tokio::test]
    async fn text_layers_are_pinned() {
        let (_dir, db) = open().await;
        let insert = |or: &str, content: &str| {
            format!(
                "INSERT {or} INTO text_layers (text_layer_sha256, content_sha256, engine_id, engine_version, config_sha256) \
                 VALUES ('{SHA}', '{content}', 'apple-pdfkit', '26.6', '{SHA}')"
            )
        };
        exec(&db, &insert("", SHA)).await.unwrap();
        assert!(
            exec(&db, "UPDATE text_layers SET engine_version = '27'")
                .await
                .is_err()
        );
        assert!(exec(&db, "DELETE FROM text_layers").await.is_err());
        assert!(
            exec(&db, &insert("OR REPLACE", SHA2)).await.is_err(),
            "REPLACE must not retarget a layer"
        );
    }

    #[tokio::test]
    async fn the_oplog_is_append_only_and_origins_are_exclusive() {
        let (_dir, db) = open().await;
        exec(
            &db,
            "INSERT INTO oplog (op, origin) VALUES ('import', 'page')",
        )
        .await
        .unwrap();
        exec(&db, "INSERT INTO oplog (op, origin, run_id, tool_call_id) VALUES ('extract', 'run', 'r1', 't1')")
            .await
            .unwrap();
        for bad in [
            "INSERT INTO oplog (op, origin) VALUES ('extract', 'run')",
            "INSERT INTO oplog (op, origin, run_id, tool_call_id) VALUES ('extract', 'run', '', 't1')",
            "INSERT INTO oplog (op, origin, run_id) VALUES ('extract', 'run', 'r1')",
            "INSERT INTO oplog (op, origin, run_id) VALUES ('import', 'page', 'r1')",
        ] {
            assert!(exec(&db, bad).await.is_err(), "{bad}");
        }
        assert!(
            exec(&db, "UPDATE oplog SET op = 'x'")
                .await
                .unwrap_err()
                .to_string()
                .contains("append-only")
        );
        assert!(
            exec(&db, "DELETE FROM oplog")
                .await
                .unwrap_err()
                .to_string()
                .contains("append-only")
        );
        let replace = "INSERT OR REPLACE INTO oplog (seq, op, origin) VALUES (1, 'forged', 'page')";
        assert!(
            exec(&db, replace).await.is_err(),
            "REPLACE must not overwrite an entry"
        );
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM oplog WHERE op = 'forged'").await,
            0
        );
    }

    #[tokio::test]
    async fn every_job_status_is_accepted_and_origin_json_is_checked() {
        let (_dir, db) = open().await;
        let ids = ["0", "1", "2", "3", "4", "5", "6"];
        let statuses = [
            "queued",
            "running",
            "cancelling",
            "succeeded",
            "failed",
            "cancelled",
            "interrupted",
        ];
        for (n, status) in ids.iter().zip(statuses) {
            let id = format!("job_01K74Z3QJ8V5N2W9RTX6YB4MC{n}");
            exec(&db, &format!("INSERT INTO jobs (id, kind, status, origin) VALUES ('{id}', 'import', '{status}', '{{\"kind\":\"page\"}}')"))
                .await
                .unwrap();
        }
        let job = |status: &str, origin: &str| {
            format!(
                "INSERT INTO jobs (id, kind, status, origin) VALUES ('{JOB}', 'import', '{status}', '{origin}')"
            )
        };
        assert!(
            exec(&db, &job("paused", "{\"kind\":\"page\"}"))
                .await
                .is_err()
        );
        assert!(exec(&db, &job("queued", "page")).await.is_err(), "not JSON");
        assert!(
            exec(&db, &job("queued", "{\"kind\":\"run\",\"run_id\":\"r1\"}"))
                .await
                .is_err()
        );
        assert!(
            exec(&db, &job("queued", "{\"kind\":\"page\",\"run_id\":\"r1\"}"))
                .await
                .is_err()
        );
        exec(
            &db,
            &job(
                "queued",
                "{\"kind\":\"run\",\"run_id\":\"r1\",\"tool_call_id\":\"t1\"}",
            ),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn idempotency_keys_are_unique_per_kind_over_the_closed_kind_set() {
        let (_dir, db) = open().await;
        let put = |kind: &str| {
            format!(
                "INSERT INTO idempotency (kind, key, request_sha256, target_ref) VALUES ('{kind}', 'k', '{SHA}', 'job_x')"
            )
        };
        for kind in ["upload", "import", "extract", "propose", "export", "commit"] {
            exec(&db, &put(kind)).await.unwrap();
        }
        assert!(
            exec(&db, &put("import")).await.is_err(),
            "same (kind, key) twice"
        );
        assert!(
            exec(&db, &put("delete")).await.is_err(),
            "kind outside ADR-DOC-02 §5.4"
        );
    }

    // ---- 0002_integrity (PR-Daemon on #807) ----

    #[tokio::test]
    async fn the_head_can_only_move_to_an_existing_revision() {
        let (_dir, db) = open().await;
        add_document(&db, DOC).await.unwrap();
        add_revision(&db, DOC, "1", SHA).await.unwrap();
        let head =
            |rev: i64| format!("UPDATE documents SET head_revision = {rev} WHERE id = '{DOC}'");
        assert!(exec(&db, &head(99)).await.is_err());
        add_revision(&db, DOC, "2", SHA2).await.unwrap();
        exec(&db, &head(2)).await.unwrap();
    }

    #[tokio::test]
    async fn run_origin_ids_must_be_strings() {
        let (_dir, db) = open().await;
        let job = |origin: &str| {
            format!(
                "INSERT INTO jobs (id, kind, status, origin) VALUES ('{JOB}', 'extract', 'queued', '{origin}')"
            )
        };
        assert!(
            exec(
                &db,
                &job(r#"{"kind":"run","run_id":123,"tool_call_id":{"a":1}}"#)
            )
            .await
            .is_err()
        );
        assert!(
            exec(
                &db,
                &job(r#"{"kind":"run","run_id":"r1","tool_call_id":7}"#)
            )
            .await
            .is_err()
        );
        exec(
            &db,
            &job(r#"{"kind":"run","run_id":"r1","tool_call_id":"t1"}"#),
        )
        .await
        .unwrap();
        let retarget = format!(
            r#"UPDATE jobs SET origin = '{{"kind":"run","run_id":5,"tool_call_id":"t1"}}' WHERE id = '{JOB}'"#
        );
        assert!(exec(&db, &retarget).await.is_err());
    }

    #[tokio::test]
    async fn a_job_revision_needs_its_document_and_must_exist() {
        let (_dir, db) = open().await;
        add_document(&db, DOC).await.unwrap();
        add_revision(&db, DOC, "1", SHA).await.unwrap();
        let job = |doc: &str, rev: &str| {
            format!(
                "INSERT INTO jobs (id, kind, document_id, revision, status, origin) VALUES ('{JOB}', 'extract', {doc}, {rev}, 'queued', '{{\"kind\":\"page\"}}')"
            )
        };
        assert!(
            exec(&db, &job("NULL", "1")).await.is_err(),
            "revision without document"
        );
        assert!(
            exec(&db, &job(&format!("'{DOC}'"), "2")).await.is_err(),
            "revision that does not exist"
        );
        exec(&db, &job(&format!("'{DOC}'"), "1")).await.unwrap();
        assert!(
            exec(
                &db,
                &format!("UPDATE jobs SET revision = 9 WHERE id = '{JOB}'")
            )
            .await
            .is_err()
        );
        exec(
            &db,
            &format!("UPDATE jobs SET revision = NULL WHERE id = '{JOB}'"),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn an_upload_is_complete_only_with_every_byte() {
        let (_dir, db) = open().await;
        let upl = |status: &str, received: i64| {
            format!(
                "INSERT INTO uploads (id, total_size, sha256, received, status) VALUES ('{UPL}', 10, '{SHA}', {received}, '{status}')"
            )
        };
        assert!(exec(&db, &upl("complete", 4)).await.is_err());
        assert!(exec(&db, &upl("imported", 9)).await.is_err());
        exec(&db, &upl("receiving", 4)).await.unwrap();
        assert!(
            exec(
                &db,
                &format!("UPDATE uploads SET status = 'complete' WHERE id = '{UPL}'")
            )
            .await
            .is_err()
        );
        exec(
            &db,
            &format!("UPDATE uploads SET received = 10, status = 'complete' WHERE id = '{UPL}'"),
        )
        .await
        .unwrap();
    }

    // ---- 0003_documents_guard (PR-Daemon on #810) ----

    #[tokio::test]
    async fn import_creates_a_document_and_r1_in_one_transaction() {
        let (_dir, db) = open().await;
        let mut tx = db.pool().begin().await.unwrap();
        sqlx::query(&format!("INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{DOC}', 't', 'application/pdf', 1)"))
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(&format!(
            "INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin) VALUES ('{DOC}', 1, '{SHA}', 1, 'application/pdf', 'import')"
        ))
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM revisions").await, 1);
    }

    /// Codex R3 on #810: a REPLACE in the same transaction, before r1 exists.
    #[tokio::test]
    async fn a_replace_before_r1_cannot_point_the_head_anywhere() {
        let (_dir, db) = open().await;
        let mut tx = db.pool().begin().await.unwrap();
        sqlx::query(&format!("INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{DOC}', 't', 'application/pdf', 1)"))
            .execute(&mut *tx)
            .await
            .unwrap();
        let replace = format!(
            "INSERT OR REPLACE INTO documents (id, title, media_type, head_revision) VALUES ('{DOC}', 't', 'application/pdf', 77)"
        );
        assert!(sqlx::query(&replace).execute(&mut *tx).await.is_err());
    }

    #[tokio::test]
    async fn documents_are_never_deleted() {
        let (_dir, db) = open().await;
        add_document(&db, DOC).await.unwrap();
        let del = exec(&db, "DELETE FROM documents").await;
        assert!(del.unwrap_err().to_string().contains("never deleted"));
    }

    /// Without recursive_triggers (e.g. someone using the sqlite3 shell), a
    /// REPLACE skips the delete trigger; the insert trigger still holds.
    #[tokio::test]
    async fn the_insert_guard_holds_on_a_connection_without_recursive_triggers() {
        use sqlx::Connection;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).await.unwrap();
        add_document(&db, DOC).await.unwrap();
        add_revision(&db, DOC, "1", SHA).await.unwrap();
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
        let replace = |id: &str| {
            format!(
                "INSERT OR REPLACE INTO documents (id, title, media_type, head_revision) VALUES ('{id}', 't', 'application/pdf', 77)"
            )
        };
        assert!(sqlx::query(&replace(DOC)).execute(&mut raw).await.is_err());
        // Codex R3's window: the document row exists but r1 does not yet. A
        // guard keyed only on "revisions exist" would let this through.
        sqlx::query(&format!("INSERT INTO documents (id, title, media_type, head_revision) VALUES ('{DOC2}', 't', 'application/pdf', 1)"))
            .execute(&mut raw)
            .await
            .unwrap();
        assert!(sqlx::query(&replace(DOC2)).execute(&mut raw).await.is_err());
    }

    #[tokio::test]
    async fn upload_progress_cannot_exceed_its_size() {
        let (_dir, db) = open().await;
        let upl = |received: i64| {
            format!(
                "INSERT INTO uploads (id, total_size, sha256, received) VALUES ('{UPL}', 10, '{SHA}', {received})"
            )
        };
        assert!(exec(&db, &upl(11)).await.is_err());
        exec(&db, &upl(10)).await.unwrap();
    }
}
