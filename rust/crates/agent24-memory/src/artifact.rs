//! MD-2b: the editable-content authority — a CAS-versioned markdown/core store
//! (SPEC-MD-ME §1/§2, ADR-028).
//!
//! An [`Artifact`] is user- or agent-authored content (a note, a persona core)
//! that, unlike an immutable [`crate::event::MemEvent`], is EDITED over time.
//! Every edit goes through [`ArtifactStore::cas_write`] with the version the
//! writer believed it was editing; a stale `expect_version` is rejected
//! ([`crate::MemoryError::Conflict`]) rather than silently clobbering a
//! concurrent write. Each committed version is retained ([`ArtifactStore::history`]),
//! so the store is an append-of-versions, never a destructive overwrite.
//!
//! **Dual lineage (basic-memory).** Two checksums travel with every artifact:
//! [`Artifact::db_checksum`] hashes the body the DB authoritatively holds;
//! [`Artifact::file_checksum`] hashes what the DB last observed on disk. MD-2b
//! keeps them EQUAL at write time — a DB-side write is assumed flushed to its
//! file. The point of carrying both now is MD-2c: reconciliation walks the real
//! filesystem, and when a file was edited outside the store its on-disk hash no
//! longer matches `db_checksum`. That divergence is what reconciliation acts on,
//! and it must never resolve a divergence by silently deleting. This slice does
//! not touch the filesystem; it establishes the versioned authority the
//! reconciler will sit on top of.

use agent24_core::util::now_iso8601;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};

use crate::event::Scope;
use crate::{MemoryError, Result};

/// Lowercase-hex SHA-256 of a body — the content checksum both lineages use.
pub fn checksum(body: &str) -> String {
    let digest = Sha256::digest(body.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        // hex of a byte never fails to format into a String.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// One versioned piece of editable content, owner-scoped by `path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub path: String,
    pub body: String,
    /// Monotonic per (owner, path): the FIRST committed version is 1.
    pub version: u64,
    /// Hash of `body` as the DB holds it (the authority).
    pub db_checksum: String,
    /// Hash the DB last observed on disk. Equal to `db_checksum` until MD-2c
    /// reconciliation observes a divergent file.
    pub file_checksum: String,
    pub scope: Scope,
    pub updated_by: String,
    pub reason: String,
    pub at: String,
}

impl Artifact {
    /// A proposed write of `body` to `path` under `scope`, with provenance. The
    /// version/checksums are assigned by [`ArtifactStore::cas_write`]; the values
    /// here are placeholders it overwrites.
    pub fn draft(
        path: impl Into<String>,
        body: impl Into<String>,
        scope: Scope,
        updated_by: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        let body = body.into();
        let sum = checksum(&body);
        Self {
            path: path.into(),
            body,
            version: 0,
            db_checksum: sum.clone(),
            file_checksum: sum,
            scope,
            updated_by: updated_by.into(),
            reason: reason.into(),
            at: now_iso8601(),
        }
    }
}

/// The CAS-versioned editable-content authority.
///
/// **Identity is `(owner, path)` — narrowing is NOT isolation.** An artifact
/// (persona core, note) belongs to an OWNER and is meant to span sessions, so
/// `read`/`history` take a bare `owner: &str`, not a `&Scope`. This is a
/// deliberate contrast with the episodic [`crate::event::EventStore`], which DOES
/// filter by `scope.session` (an event belongs to the run that emitted it).
/// Taking a `&Scope` here would falsely imply that passing a session narrows the
/// read — it would not (review #115 B2). `cas_write` still records the writer's
/// full [`Artifact::scope`] as PROVENANCE, but only `scope.owner` is identity.
#[async_trait]
pub trait ArtifactStore: Send + Sync {
    /// The current version of an artifact, or `None` if `path` is untracked
    /// under `owner`.
    async fn read(&self, path: &str, owner: &str) -> Result<Option<Artifact>>;
    /// Commit `a.body` as the next version, but only if `expect_version` matches
    /// the version currently stored (0 for a first create). A mismatch — a stale
    /// or wrong assumption about the current state — is a
    /// [`MemoryError::Conflict`], never a clobber. Returns the committed artifact
    /// with its assigned version and checksums.
    async fn cas_write(&self, a: Artifact, expect_version: u64) -> Result<Artifact>;
    /// Every committed version of `path` under `owner`, oldest first.
    async fn history(&self, path: &str, owner: &str) -> Result<Vec<Artifact>>;
}

/// SQLite-backed [`ArtifactStore`]. Shares the memory DB pool (see
/// [`crate::KvStore::artifacts`]).
#[derive(Clone)]
pub struct ArtifactCas {
    pool: SqlitePool,
}

impl ArtifactCas {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    fn row_to_artifact(row: &sqlx::sqlite::SqliteRow) -> Result<Artifact> {
        let scope: Scope = serde_json::from_str(&row.get::<String, _>("scope"))?;
        Ok(Artifact {
            path: row.get("path"),
            body: row.get("body"),
            // versions are stored as i64 (SQLite has no u64); they are assigned
            // by us starting at 1 and only ever increment, so this never wraps.
            version: row.get::<i64, _>("version") as u64,
            db_checksum: row.get("db_checksum"),
            file_checksum: row.get("file_checksum"),
            scope,
            updated_by: row.get("updated_by"),
            reason: row.get("reason"),
            at: row.get("at"),
        })
    }
}

#[async_trait]
impl ArtifactStore for ArtifactCas {
    async fn read(&self, path: &str, owner: &str) -> Result<Option<Artifact>> {
        let row = sqlx::query(
            "SELECT path, body, version, db_checksum, file_checksum, scope, updated_by, reason, at
             FROM mem_artifacts WHERE scope_owner = ? AND path = ?",
        )
        .bind(owner)
        .bind(path)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(Self::row_to_artifact).transpose()
    }

    async fn cas_write(&self, a: Artifact, expect_version: u64) -> Result<Artifact> {
        // BEGIN IMMEDIATE takes the write lock UP FRONT, so the whole
        // read-check-write is serialized against other writers and `busy_timeout`
        // actually applies. A plain DEFERRED begin (`pool.begin()`) instead lets
        // two writers both take read locks and then collide on the lock UPGRADE,
        // which SQLite fails IMMEDIATELY with SQLITE_BUSY (busy_timeout
        // deliberately does not wait on upgrades) — surfacing a raw "database is
        // locked" to the loser instead of the `Conflict` this API's retry
        // contract promises. Under a real multi-connection WAL pool that was 96%
        // of losers (review #115 B1); with BEGIN IMMEDIATE they serialize and
        // every loser gets a clean version `Conflict`.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;

        let current: Option<i64> =
            sqlx::query("SELECT version FROM mem_artifacts WHERE scope_owner = ? AND path = ?")
                .bind(&a.scope.owner)
                .bind(&a.path)
                .fetch_optional(&mut *tx)
                .await?
                .map(|r| r.get::<i64, _>("version"));
        let current = current.unwrap_or(0) as u64;

        if expect_version != current {
            return Err(MemoryError::Conflict(format!(
                "artifact {} is at version {current}, write expected {expect_version}",
                a.path
            )));
        }

        let new_version = current + 1;
        let body_sum = checksum(&a.body);
        // MD-2b: a DB write is assumed flushed, so both lineages agree here.
        // MD-2c reconciliation is what later sets file_checksum from real disk.
        let committed = Artifact {
            version: new_version,
            db_checksum: body_sum.clone(),
            file_checksum: body_sum,
            at: now_iso8601(),
            ..a
        };
        let scope_json = serde_json::to_string(&committed.scope)?;
        let version_i64 = new_version as i64;

        // Insert the history row, then move the pointer. With BEGIN IMMEDIATE the
        // writers are already serialized, so this order is just for a clean
        // single-statement failure surface. The UNIQUE(owner, path, version) it
        // can violate is DEFENSE IN DEPTH the normal path never reaches (0 hits
        // under ~1400 racing writes, review #115 B3), NOT the load-bearing
        // concurrency guard — that is the write lock above.
        sqlx::query(
            "INSERT INTO mem_artifact_versions
                 (scope_owner, path, version, body, db_checksum, file_checksum,
                  scope, updated_by, reason, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&committed.scope.owner)
        .bind(&committed.path)
        .bind(version_i64)
        .bind(&committed.body)
        .bind(&committed.db_checksum)
        .bind(&committed.file_checksum)
        .bind(&scope_json)
        .bind(&committed.updated_by)
        .bind(&committed.reason)
        .bind(&committed.at)
        .execute(&mut *tx)
        .await
        .map_err(cas_race_to_conflict(&committed.path, new_version))?;

        // Move the current-version pointer forward.
        sqlx::query(
            "INSERT INTO mem_artifacts
                 (scope_owner, path, version, body, db_checksum, file_checksum,
                  scope, updated_by, reason, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(scope_owner, path) DO UPDATE SET
                 version = excluded.version, body = excluded.body,
                 db_checksum = excluded.db_checksum, file_checksum = excluded.file_checksum,
                 scope = excluded.scope, updated_by = excluded.updated_by,
                 reason = excluded.reason, at = excluded.at",
        )
        .bind(&committed.scope.owner)
        .bind(&committed.path)
        .bind(version_i64)
        .bind(&committed.body)
        .bind(&committed.db_checksum)
        .bind(&committed.file_checksum)
        .bind(&scope_json)
        .bind(&committed.updated_by)
        .bind(&committed.reason)
        .bind(&committed.at)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(committed)
    }

    async fn history(&self, path: &str, owner: &str) -> Result<Vec<Artifact>> {
        let rows = sqlx::query(
            "SELECT path, body, version, db_checksum, file_checksum, scope, updated_by, reason, at
             FROM mem_artifact_versions WHERE scope_owner = ? AND path = ?
             ORDER BY version ASC",
        )
        .bind(owner)
        .bind(path)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(Self::row_to_artifact).collect()
    }
}

/// Map a history-table PK collision to a `Conflict`, leaving any other DB error
/// untouched. This is the DEFENSE-IN-DEPTH path (see `cas_write`): with the
/// writer transaction serialized by BEGIN IMMEDIATE the normal path loses at the
/// version check, not here — but a future direct-to-history writer would.
fn cas_race_to_conflict(path: &str, version: u64) -> impl FnOnce(sqlx::Error) -> MemoryError {
    let path = path.to_owned();
    move |e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => MemoryError::Conflict(format!(
            "artifact {path} version {version} was committed concurrently"
        )),
        _ => MemoryError::Sqlx(e),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::KvStore;

    async fn store() -> ArtifactCas {
        KvStore::open_memory().await.unwrap().artifacts()
    }

    /// A unique temp DB path per test (tokio runs tests concurrently in one
    /// binary, so a shared filename would collide).
    fn temp_db(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("a24mem-artifact-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("mem.db")
    }

    fn draft(owner: &str, path: &str, body: &str) -> Artifact {
        Artifact::draft(path, body, Scope::owner(owner), "agent", "test write")
    }

    #[tokio::test]
    async fn create_then_read_current_version() {
        let s = store().await;
        assert!(s.read("core.md", "u1").await.unwrap().is_none());

        let a = s
            .cas_write(draft("u1", "core.md", "hello"), 0)
            .await
            .unwrap();
        assert_eq!(a.version, 1);
        assert_eq!(a.db_checksum, checksum("hello"));
        assert_eq!(a.file_checksum, a.db_checksum);

        let got = s.read("core.md", "u1").await.unwrap().unwrap();
        assert_eq!(got.body, "hello");
        assert_eq!(got.version, 1);
    }

    #[tokio::test]
    async fn cas_write_advances_version_on_matching_expect() {
        let s = store().await;
        s.cas_write(draft("u1", "n.md", "v1"), 0).await.unwrap();
        let v2 = s.cas_write(draft("u1", "n.md", "v2"), 1).await.unwrap();
        assert_eq!(v2.version, 2);
        assert_eq!(v2.body, "v2");
        assert_eq!(s.read("n.md", "u1").await.unwrap().unwrap().version, 2);
    }

    #[tokio::test]
    async fn cas_write_rejects_stale_expect_version() {
        let s = store().await;
        s.cas_write(draft("u1", "n.md", "v1"), 0).await.unwrap();
        s.cas_write(draft("u1", "n.md", "v2"), 1).await.unwrap();
        // A writer that still thinks it is at version 1 must be rejected, not
        // clobber v2.
        let err = s
            .cas_write(draft("u1", "n.md", "stale"), 1)
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::Conflict(_)), "{err}");
        // v2 is intact.
        let cur = s.read("n.md", "u1").await.unwrap().unwrap();
        assert_eq!(cur.body, "v2");
        assert_eq!(cur.version, 2);
    }

    #[tokio::test]
    async fn first_write_with_nonzero_expect_is_a_conflict() {
        let s = store().await;
        // The path does not exist yet (current version 0); expecting 1 is wrong.
        let err = s
            .cas_write(draft("u1", "new.md", "x"), 1)
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::Conflict(_)), "{err}");
        assert!(s.read("new.md", "u1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn history_keeps_every_version_oldest_first() {
        let s = store().await;
        s.cas_write(draft("u1", "n.md", "v1"), 0).await.unwrap();
        s.cas_write(draft("u1", "n.md", "v2"), 1).await.unwrap();
        s.cas_write(draft("u1", "n.md", "v3"), 2).await.unwrap();
        let hist = s.history("n.md", "u1").await.unwrap();
        let versions: Vec<u64> = hist.iter().map(|a| a.version).collect();
        let bodies: Vec<&str> = hist.iter().map(|a| a.body.as_str()).collect();
        assert_eq!(versions, vec![1, 2, 3]);
        assert_eq!(bodies, vec!["v1", "v2", "v3"], "no version is destroyed");
    }

    #[tokio::test]
    async fn history_orders_by_version_not_physical_row_order() {
        // Low#4: the public API always writes in order, so insertion order equals
        // version order and never exercises `ORDER BY version`. Insert version
        // rows OUT OF ORDER via raw SQL and prove history() still sorts them.
        let s = store().await;
        for v in [3i64, 1, 2] {
            sqlx::query(
                "INSERT INTO mem_artifact_versions
                     (scope_owner, path, version, body, db_checksum, file_checksum,
                      scope, updated_by, reason, at)
                 VALUES ('u1', 'n.md', ?, ?, 'x', 'x', '{\"owner\":\"u1\"}', 'a', 'r', 't')",
            )
            .bind(v)
            .bind(format!("body-{v}"))
            .execute(&s.pool)
            .await
            .unwrap();
        }
        let hist = s.history("n.md", "u1").await.unwrap();
        let versions: Vec<u64> = hist.iter().map(|a| a.version).collect();
        assert_eq!(versions, vec![1, 2, 3], "ORDER BY version, not row order");
    }

    #[tokio::test]
    async fn same_path_is_isolated_across_owners() {
        let s = store().await;
        // Two owners independently create the SAME path — no collision, no leak.
        let a = s
            .cas_write(draft("alice", "core.md", "alice-core"), 0)
            .await
            .unwrap();
        let b = s
            .cas_write(draft("bob", "core.md", "bob-core"), 0)
            .await
            .unwrap();
        assert_eq!(a.version, 1);
        assert_eq!(b.version, 1);
        assert_eq!(
            s.read("core.md", "alice").await.unwrap().unwrap().body,
            "alice-core"
        );
        assert_eq!(
            s.read("core.md", "bob").await.unwrap().unwrap().body,
            "bob-core"
        );
        // bob cannot see alice's history and vice versa.
        assert_eq!(s.history("core.md", "bob").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn read_is_owner_scoped_narrowing_is_not_isolation() {
        // B2: an artifact belongs to an OWNER and spans sessions. Two DIFFERENT
        // sessions of the SAME owner see and edit the SAME core — the write from
        // sess-A is visible to sess-B, and identity ignores the session. (The
        // signature takes `owner: &str`, so a caller cannot even be misled into
        // thinking a session narrows the read.)
        let s = store().await;
        let a_scope = Scope::owner("u1").with_session("sess-A");
        s.cas_write(
            Artifact::draft("core.md", "from-A", a_scope, "agent", "w"),
            0,
        )
        .await
        .unwrap();
        // Read by owner sees it regardless of any session.
        let got = s.read("core.md", "u1").await.unwrap().unwrap();
        assert_eq!(got.body, "from-A");
        // A different session of the same owner edits the same identity.
        let b_scope = Scope::owner("u1").with_session("sess-B");
        let v2 = s
            .cas_write(
                Artifact::draft("core.md", "from-B", b_scope, "agent", "w"),
                1,
            )
            .await
            .unwrap();
        assert_eq!(v2.version, 2, "same (owner, path) identity across sessions");
    }

    #[tokio::test]
    async fn empty_owner_is_rejected() {
        // CHECK(trim(scope_owner) <> '') — an unowned artifact is not valid.
        let s = store().await;
        let err = s.cas_write(draft("", "n.md", "x"), 0).await.unwrap_err();
        assert!(matches!(err, MemoryError::Sqlx(_)), "{err}");
    }

    #[tokio::test]
    async fn whitespace_owner_is_rejected() {
        // Low: "   " is unowned memory too (trim()).
        let s = store().await;
        let err = s.cas_write(draft("   ", "n.md", "x"), 0).await.unwrap_err();
        assert!(matches!(err, MemoryError::Sqlx(_)), "{err}");
    }

    #[tokio::test]
    async fn empty_path_is_rejected() {
        // Low: CHECK(trim(path) <> '') was previously unexercised.
        let s = store().await;
        let err = s.cas_write(draft("u1", "", "x"), 0).await.unwrap_err();
        assert!(matches!(err, MemoryError::Sqlx(_)), "{err}");
        let err = s.cas_write(draft("u1", "  ", "x"), 0).await.unwrap_err();
        assert!(matches!(err, MemoryError::Sqlx(_)), "{err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_cas_write_on_real_pool_yields_one_winner_rest_conflict() {
        // B1: the regression that single-connection `open_memory()` cannot catch.
        // On a real multi-connection WAL pool, N writers race the SAME
        // expect_version; exactly one wins and EVERY loser must get a clean
        // `Conflict`, never a raw "database is locked" (which BEGIN IMMEDIATE
        // prevents by serializing on the write lock).
        let path = temp_db("cas-race");
        let store = KvStore::open(&path).await.unwrap();
        let s = store.artifacts();
        s.cas_write(draft("u1", "n.md", "seed"), 0).await.unwrap(); // version 1

        let n = 6;
        let mut handles = Vec::new();
        for i in 0..n {
            let s2 = s.clone();
            handles.push(tokio::spawn(async move {
                s2.cas_write(draft("u1", "n.md", &format!("w{i}")), 1).await
            }));
        }
        let (mut ok, mut conflict, mut other) = (0, 0, 0);
        for h in handles {
            match h.await.unwrap() {
                Ok(_) => ok += 1,
                Err(MemoryError::Conflict(_)) => conflict += 1,
                Err(e) => {
                    other += 1;
                    eprintln!("unexpected non-Conflict error: {e}");
                }
            }
        }
        assert_eq!(ok, 1, "exactly one writer wins");
        assert_eq!(
            other, 0,
            "every loser gets Conflict, never a raw lock error"
        );
        assert_eq!(conflict, n - 1);
        // Final state is a single clean version-2 pointer over a contiguous
        // 2-version history — no lost update, no version hole.
        assert_eq!(s.read("n.md", "u1").await.unwrap().unwrap().version, 2);
        let versions: Vec<u64> = s
            .history("n.md", "u1")
            .await
            .unwrap()
            .iter()
            .map(|a| a.version)
            .collect();
        assert_eq!(versions, vec![1, 2]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn checksum_is_stable_and_content_addressed() {
        assert_eq!(checksum("hello"), checksum("hello"));
        assert_ne!(checksum("hello"), checksum("world"));
        // known SHA-256 of "hello"
        assert_eq!(
            checksum("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}
