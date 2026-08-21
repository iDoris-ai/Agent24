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
#[async_trait]
pub trait ArtifactStore: Send + Sync {
    /// The current version of an artifact, or `None` if `path` is untracked
    /// under this owner.
    async fn read(&self, path: &str, scope: &Scope) -> Result<Option<Artifact>>;
    /// Commit `a.body` as the next version, but only if `expect_version` matches
    /// the version currently stored (0 for a first create). A mismatch — a stale
    /// or wrong assumption about the current state — is a
    /// [`MemoryError::Conflict`], never a clobber. Returns the committed artifact
    /// with its assigned version and checksums.
    async fn cas_write(&self, a: Artifact, expect_version: u64) -> Result<Artifact>;
    /// Every committed version of `path` under this owner, oldest first.
    async fn history(&self, path: &str, scope: &Scope) -> Result<Vec<Artifact>>;
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
    async fn read(&self, path: &str, scope: &Scope) -> Result<Option<Artifact>> {
        let row = sqlx::query(
            "SELECT path, body, version, db_checksum, file_checksum, scope, updated_by, reason, at
             FROM mem_artifacts WHERE scope_owner = ? AND path = ?",
        )
        .bind(&scope.owner)
        .bind(path)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(Self::row_to_artifact).transpose()
    }

    async fn cas_write(&self, a: Artifact, expect_version: u64) -> Result<Artifact> {
        // The whole read-check-write runs in one transaction so the current
        // version we validate against cannot shift under us mid-write. The
        // UNIQUE(scope_owner, path, version) on the history table is the ultimate
        // guard: a racing writer that slips past the version check still collides
        // there and is rejected, so no two commits can share a version.
        let mut tx = self.pool.begin().await?;

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

        // History insert first: its PK rejects a concurrent duplicate version.
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

    async fn history(&self, path: &str, scope: &Scope) -> Result<Vec<Artifact>> {
        let rows = sqlx::query(
            "SELECT path, body, version, db_checksum, file_checksum, scope, updated_by, reason, at
             FROM mem_artifact_versions WHERE scope_owner = ? AND path = ?
             ORDER BY version ASC",
        )
        .bind(&scope.owner)
        .bind(path)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(Self::row_to_artifact).collect()
    }
}

/// Map the history-table PK collision (a concurrent writer already committed
/// this version) to a `Conflict`, leaving any other DB error untouched.
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

    fn draft(owner: &str, path: &str, body: &str) -> Artifact {
        Artifact::draft(path, body, Scope::owner(owner), "agent", "test write")
    }

    #[tokio::test]
    async fn create_then_read_current_version() {
        let s = store().await;
        let scope = Scope::owner("u1");
        assert!(s.read("core.md", &scope).await.unwrap().is_none());

        let a = s
            .cas_write(draft("u1", "core.md", "hello"), 0)
            .await
            .unwrap();
        assert_eq!(a.version, 1);
        assert_eq!(a.db_checksum, checksum("hello"));
        assert_eq!(a.file_checksum, a.db_checksum);

        let got = s.read("core.md", &scope).await.unwrap().unwrap();
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
        assert_eq!(
            s.read("n.md", &Scope::owner("u1"))
                .await
                .unwrap()
                .unwrap()
                .version,
            2
        );
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
        let cur = s.read("n.md", &Scope::owner("u1")).await.unwrap().unwrap();
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
        assert!(
            s.read("new.md", &Scope::owner("u1"))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn history_keeps_every_version_oldest_first() {
        let s = store().await;
        s.cas_write(draft("u1", "n.md", "v1"), 0).await.unwrap();
        s.cas_write(draft("u1", "n.md", "v2"), 1).await.unwrap();
        s.cas_write(draft("u1", "n.md", "v3"), 2).await.unwrap();
        let hist = s.history("n.md", &Scope::owner("u1")).await.unwrap();
        let versions: Vec<u64> = hist.iter().map(|a| a.version).collect();
        let bodies: Vec<&str> = hist.iter().map(|a| a.body.as_str()).collect();
        assert_eq!(versions, vec![1, 2, 3]);
        assert_eq!(bodies, vec!["v1", "v2", "v3"], "no version is destroyed");
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
            s.read("core.md", &Scope::owner("alice"))
                .await
                .unwrap()
                .unwrap()
                .body,
            "alice-core"
        );
        assert_eq!(
            s.read("core.md", &Scope::owner("bob"))
                .await
                .unwrap()
                .unwrap()
                .body,
            "bob-core"
        );
        // bob cannot see alice's history and vice versa.
        assert_eq!(
            s.history("core.md", &Scope::owner("bob"))
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn empty_owner_is_rejected() {
        // CHECK(scope_owner <> '') — an unowned artifact is not valid.
        let s = store().await;
        let err = s.cas_write(draft("", "n.md", "x"), 0).await.unwrap_err();
        assert!(matches!(err, MemoryError::Sqlx(_)), "{err}");
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
