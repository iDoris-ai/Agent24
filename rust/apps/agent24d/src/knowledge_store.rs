//! `knowledge.db` (KB-C1-04 6d-1, design F2): the Provider's log and seal
//! (CONTRACT-STATE-v1 A.1–A.3) in a database of their own under
//! `<state_dir>/knowledge/`. A record is one atomic write of the changes
//! `agent24_knowledge::provider::stored` encodes, or the void of its number.
//! The store decides nothing: it keeps what the Provider hands it and gives
//! it back, and what it cannot give back exactly reads as damaged (S1.8).
//!
//! Pragmas are set on the connection as in `documents.db`:
//! `journal_mode=WAL` and `synchronous=FULL`, since a write counts as
//! persisted only once it would survive a power loss (A.1). One connection;
//! the caller issues writes one at a time, in order (ledger.rs).
//!
//! The migrations are this database's own. A database with a migration this
//! build does not have, or with one altered, is refused, never migrated or
//! rebuilt: opening fails and its records stay as they are. (The connection
//! has set WAL by then, which a file this build created already has.)

use std::path::Path;
use std::time::Duration;

use agent24_knowledge::provider::recover::Seal;
use agent24_knowledge::provider::replay::{Damaged, Entry, Record, Replayed, replay};
use agent24_knowledge::provider::stored::{self, StoredIds};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

pub const DIR: &str = "knowledge";
pub const DB_FILE: &str = "knowledge.db";

#[derive(Debug)]
pub enum StoreError {
    Sqlx(sqlx::Error),
    /// A migration this build does not have, or one altered.
    Migrate(sqlx::migrate::MigrateError),
    Io(std::io::Error),
    Encode(serde_json::Error),
    /// A number past what SQLite holds (`i64::MAX`).
    Range,
    /// The number already holds a different write of the same kind (A.1), or
    /// the log was begun at another number.
    Conflict,
}

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

impl From<sqlx::migrate::MigrateError> for StoreError {
    fn from(e: sqlx::migrate::MigrateError) -> Self {
        Self::Migrate(e)
    }
}

/// What a start reads back, for `recover` (A.2, A.3).
#[derive(Debug)]
pub struct Found<I: StoredIds> {
    /// `None` with no persisted state at all: no log begun and no record.
    /// Records without a begun log, or any that cannot be read exactly,
    /// are damaged.
    pub log: Option<Result<Replayed<I>, Damaged>>,
    pub seal: Seal,
}

pub struct KnowledgeDb {
    pool: SqlitePool,
}

impl KnowledgeDb {
    /// Open (creating if needed) `<state_dir>/knowledge/knowledge.db` and run
    /// its migrations.
    pub async fn open(state_dir: &Path) -> Result<Self, StoreError> {
        let dir = state_dir.join(DIR);
        std::fs::create_dir_all(&dir).map_err(StoreError::Io)?;
        // `filename`, not a `sqlite://` URL: a path may contain `?` or `%`.
        let options = SqliteConnectOptions::new()
            .filename(dir.join(DB_FILE))
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./knowledge_migrations").run(&pool).await?;
        // SQLite opens a read-only file read-only without an error. Rewrite a
        // header field to its own value so an unwritable database fails here,
        // not on the first write.
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&pool)
            .await?;
        sqlx::query(&format!("PRAGMA user_version = {version}"))
            .execute(&pool)
            .await?;
        Ok(Self { pool })
    }

    #[cfg(test)]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// The log, replayed from its start, and the seal, in one read
    /// transaction. SQLite checks a column's type when writing, not when
    /// reading: a number that is not an integer reads as `NULL` here, the
    /// seal then missing and the log damaged, never as zero. Text is read as
    /// bytes, so text that is not UTF-8 is damaged, not an error; a kind that
    /// is not text reads as `NULL`, a body neither text nor `NULL` as empty,
    /// neither ever a record.
    pub async fn read<I: StoredIds>(&self) -> Result<Found<I>, StoreError> {
        let mut tx = self.pool.begin().await?;
        let next: Option<i64> =
            sqlx::query_scalar("SELECT next FROM seal WHERE id = 1 AND typeof(next) = 'integer'")
                .fetch_optional(&mut *tx)
                .await?;
        let seal = match next.and_then(|next| u64::try_from(next).ok()) {
            Some(next) => Seal::Present { next },
            None => Seal::Missing,
        };

        let start: Option<Option<i64>> = sqlx::query_scalar(
            "SELECT CASE typeof(start) WHEN 'integer' THEN start END FROM log WHERE id = 1",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT CASE typeof(seq) WHEN 'integer' THEN seq END, \
             CASE typeof(kind) WHEN 'text' THEN CAST(kind AS BLOB) END, \
             CASE typeof(body) WHEN 'text' THEN CAST(body AS BLOB) \
             WHEN 'null' THEN NULL ELSE x'' END \
             FROM records ORDER BY seq, kind",
        )
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let decoded: Result<Vec<Record<I>>, Damaged> = rows
            .into_iter()
            .map(|(seq, kind, body)| record_from_row(seq, kind.as_deref(), body))
            .collect();
        let start = start.map(|s| s.and_then(|s| u64::try_from(s).ok()).ok_or(Damaged));
        let log = match (start, decoded) {
            (None, Ok(records)) if records.is_empty() => None,
            (None, _) => Some(Err(Damaged)),
            (Some(start), decoded) => Some(start.and_then(|start| replay(start, decoded?))),
        };
        Ok(Found { log, seal })
    }

    /// Begin the log at `start` (A.3). Beginning it again at the same number
    /// changes nothing; at another, it is a conflict.
    pub async fn begin(&self, start: u64) -> Result<(), StoreError> {
        let start = i64::try_from(start).map_err(|_| StoreError::Range)?;
        let inserted =
            sqlx::query("INSERT INTO log (id, start) VALUES (1, ?) ON CONFLICT(id) DO NOTHING")
                .bind(start)
                .execute(&self.pool)
                .await?
                .rows_affected();
        if inserted == 1 {
            return Ok(());
        }
        let stored: i64 = sqlx::query_scalar("SELECT start FROM log WHERE id = 1")
            .fetch_one(&self.pool)
            .await?;
        if stored == start {
            Ok(())
        } else {
            Err(StoreError::Conflict)
        }
    }

    /// Persist `record`. The same record again changes nothing (a retry,
    /// A.1); a different write of the same kind at its number is a conflict,
    /// and the one already there stays.
    pub async fn append<I: StoredIds>(&self, record: &Record<I>) -> Result<(), StoreError> {
        let (seq, kind, body) = record_columns(record)?;
        let inserted = sqlx::query(
            "INSERT INTO records (seq, kind, body) VALUES (?, ?, ?) \
             ON CONFLICT(seq, kind) DO NOTHING",
        )
        .bind(seq)
        .bind(kind)
        .bind(body.as_deref())
        .execute(&self.pool)
        .await?
        .rows_affected();
        if inserted == 1 {
            return Ok(());
        }
        let stored: Option<String> =
            sqlx::query_scalar("SELECT body FROM records WHERE seq = ? AND kind = ?")
                .bind(seq)
                .bind(kind)
                .fetch_one(&self.pool)
                .await?;
        if stored == body {
            Ok(())
        } else {
            Err(StoreError::Conflict)
        }
    }

    /// Whether the log holds exactly `record`: asked after a write whose
    /// outcome is unknown (A.1).
    pub async fn holds<I: StoredIds>(&self, record: &Record<I>) -> Result<bool, StoreError> {
        let (seq, kind, body) = record_columns(record)?;
        let stored: Option<Option<String>> =
            sqlx::query_scalar("SELECT body FROM records WHERE seq = ? AND kind = ?")
                .bind(seq)
                .bind(kind)
                .fetch_optional(&self.pool)
                .await?;
        Ok(stored.is_some_and(|stored| stored == body))
    }

    /// Rebuild the seal at `next` (A.2), over any there.
    pub async fn rebuild_seal(&self, next: u64) -> Result<(), StoreError> {
        let next = i64::try_from(next).map_err(|_| StoreError::Range)?;
        sqlx::query(
            "INSERT INTO seal (id, next) VALUES (1, ?) \
             ON CONFLICT(id) DO UPDATE SET next = excluded.next",
        )
        .bind(next)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete the seal (A.2); none there changes nothing.
    pub async fn delete_seal(&self) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM seal").execute(&self.pool).await?;
        Ok(())
    }
}

fn record_columns<I: StoredIds>(
    record: &Record<I>,
) -> Result<(i64, &'static str, Option<String>), StoreError> {
    let seq = i64::try_from(record.seq).map_err(|_| StoreError::Range)?;
    match &record.entry {
        Entry::Changes(changes) => {
            let body = stored::encode(changes).map_err(StoreError::Encode)?;
            Ok((seq, "changes", Some(body)))
        }
        Entry::Void => Ok((seq, "void", None)),
    }
}

/// A row of `records` as read: seq, kind and body.
type Row = (Option<i64>, Option<Vec<u8>>, Option<Vec<u8>>);

fn record_from_row<I: StoredIds>(
    seq: Option<i64>,
    kind: Option<&[u8]>,
    body: Option<Vec<u8>>,
) -> Result<Record<I>, Damaged> {
    let seq = seq.and_then(|seq| u64::try_from(seq).ok()).ok_or(Damaged)?;
    let entry = match (kind, body) {
        (Some(b"changes"), Some(body)) => {
            let body = String::from_utf8(body).map_err(|_| Damaged)?;
            Entry::Changes(stored::decode::<I>(&body)?)
        }
        (Some(b"void"), None) => Entry::Void,
        _ => return Err(Damaged),
    };
    Ok(Record { seq, entry })
}

#[cfg(test)]
mod tests;
