//! SQLite online backup on the raw handles of two sqlx connections.
//!
//! No second SQLite binding: `lock_handle()` yields the `sqlite3*` sqlx already
//! owns and we call `sqlite3_backup_*` from the same `libsqlite3-sys` it links.
//! While a handle guard lives sqlx's worker thread makes no FFI calls on it.

use std::ffi::{CStr, c_int};
use std::fmt;

use libsqlite3_sys as ffi;
use sqlx::SqliteConnection;

#[derive(Debug, PartialEq, Eq)]
pub enum BackupError {
    Init(c_int),
    /// `sqlite3_backup_step(-1)` did not return `SQLITE_DONE`.
    Step(c_int),
    /// `sqlite3_backup_finish` did not return `SQLITE_OK`.
    Finish(c_int),
    Sqlx(String),
}

impl fmt::Display for BackupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sqlite backup failed: {self:?}")
    }
}

/// The only success shape: step returned DONE and finish returned OK.
pub fn check_rc(step: c_int, finish: c_int) -> Result<(), BackupError> {
    if step != ffi::SQLITE_DONE {
        return Err(BackupError::Step(step));
    }
    if finish != ffi::SQLITE_OK {
        return Err(BackupError::Finish(finish));
    }
    Ok(())
}

/// Copy the whole `main` database of `src` into `dst` in one step.
/// The caller closes both connections afterwards (and proves it).
#[allow(unsafe_code)]
pub async fn backup_main(
    src: &mut SqliteConnection,
    dst: &mut SqliteConnection,
) -> Result<(), BackupError> {
    let mut s = src
        .lock_handle()
        .await
        .map_err(|e| BackupError::Sqlx(e.to_string()))?;
    let mut d = dst
        .lock_handle()
        .await
        .map_err(|e| BackupError::Sqlx(e.to_string()))?;
    let main: &CStr = c"main";
    let (sp, dp) = (s.as_raw_handle().as_ptr(), d.as_raw_handle().as_ptr());
    // SAFETY: both pointers come from live, locked sqlx connections that outlive
    // this block (`s`, `d` are held); `main` is a NUL-terminated static. The
    // backup object is finished on every path before the guards drop.
    unsafe {
        let b = ffi::sqlite3_backup_init(dp, main.as_ptr(), sp, main.as_ptr());
        if b.is_null() {
            return Err(BackupError::Init(ffi::sqlite3_errcode(dp)));
        }
        let step = ffi::sqlite3_backup_step(b, -1);
        let finish = ffi::sqlite3_backup_finish(b);
        check_rc(step, finish)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::{Connection, Row};

    async fn open(p: &std::path::Path, create: bool) -> SqliteConnection {
        let o = SqliteConnectOptions::new()
            .filename(p)
            .create_if_missing(create);
        SqliteConnection::connect_with(&o).await.unwrap()
    }

    /// Spike: raw-handle backup over a sqlx connection works (WAL source with
    /// un-checkpointed rows), using the same libsqlite3-sys sqlx links.
    #[tokio::test]
    async fn spike_backup_wal_source_into_fresh_file() {
        let td = tempfile::tempdir().unwrap();
        let (a, b) = (td.path().join("a.db"), td.path().join("b.db"));
        let mut src = open(&a, true).await;
        sqlx::query("PRAGMA journal_mode=WAL")
            .execute(&mut src)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t(x)")
            .execute(&mut src)
            .await
            .unwrap();
        sqlx::query("INSERT INTO t VALUES (1),(2)")
            .execute(&mut src)
            .await
            .unwrap();
        let mut dst = open(&b, true).await;
        backup_main(&mut src, &mut dst).await.unwrap();
        let n: i64 = sqlx::query("SELECT count(*) FROM t")
            .fetch_one(&mut dst)
            .await
            .unwrap()
            .get(0);
        assert_eq!(n, 2);
        dst.close().await.unwrap();
        src.close().await.unwrap();
    }

    #[test]
    fn rc_must_be_done_then_ok() {
        assert_eq!(check_rc(ffi::SQLITE_DONE, ffi::SQLITE_OK), Ok(()));
        for step in [ffi::SQLITE_OK, ffi::SQLITE_BUSY, ffi::SQLITE_LOCKED] {
            assert_eq!(check_rc(step, ffi::SQLITE_OK), Err(BackupError::Step(step)));
        }
        assert_eq!(
            check_rc(ffi::SQLITE_DONE, ffi::SQLITE_IOERR),
            Err(BackupError::Finish(ffi::SQLITE_IOERR))
        );
    }
}
