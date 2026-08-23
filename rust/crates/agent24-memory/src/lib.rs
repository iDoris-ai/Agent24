//! Agent24 memory (M-D / D1).
//!
//! Two layers, both persisted in one SQLite file:
//! - **L0 KV store** ([`KvStore`]): a namespaced key/value store holding
//!   arbitrary JSON. Replaces the ad-hoc `module-state.ts` and is the
//!   substrate for higher layers.
//! - **Canonical session** ([`session`]): a session's conversation with
//!   threshold-triggered LLM-summary compaction, so an unbounded chat stays a
//!   bounded prompt.

pub mod artifact;
pub mod assertion;
pub mod condenser;
pub mod consolidator;
pub mod eval;
pub mod event;
pub mod knowledge;
pub mod reconcile;
pub mod replay;
pub mod retriever;
pub mod session;
pub mod trace;
pub mod vector;
pub mod writer;

use std::path::Path;
use std::str::FromStr;

use agent24_core::util::now_iso8601;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("summarizer: {0}")]
    Summarizer(String),
    /// A stored event could not be decoded during replay. Carries the offending
    /// event's id + seq so one bad row is diagnosable, not an anonymous failure
    /// of a whole owner's history (review #116 B2).
    #[error("replay: {0}")]
    Replay(String),
    /// A [`condenser::Condenser`] returned an error (its error type is a bare
    /// `String`); the eval harness wraps it so a condense failure is a typed
    /// `MemoryError` like the rest.
    #[error("condenser: {0}")]
    Condenser(String),
    /// An [`vector::Embedder`] misbehaved — returned an identity that disagrees
    /// with its declared one, a vector whose length differs from `dims`, or a
    /// non-finite component (review #123 M1/M2/M3).
    #[error("embedder: {0}")]
    Embedder(String),
}

pub type Result<T> = std::result::Result<T, MemoryError>;

/// One durably recorded domain-OS memory partition.
///
/// See `mem_os_partitions` (migrations 0012 and 0013) for why each field is
/// kept, and in particular why `module_name` and `first_seen_at` are write-once.
///
/// The row separates the LOGICAL identity (`org_id`, `space_id`) from the
/// PHYSICAL string it is stored under (`owner_key`) and from the encoding of
/// that string (`key_version`). Those are three different facts: the physical
/// key may be rewritten — that is what a key-version migration IS — while the
/// logical identity may not, because it is what the key encodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsPartitionRow {
    pub owner_key: String,
    pub key_version: String,
    pub org_id: String,
    pub space_id: String,
    pub logical_user: String,
    pub module_name: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
}

/// The identity of a partition, as the kernel states it when recording one.
///
/// A struct rather than six `&str` parameters. Every field is a string and
/// several are plausible in each other's position, so a positional call is one
/// transposition away from attributing a partition to the wrong org — and the
/// catalog's whole job is attribution. The compiler cannot catch that; a named
/// field can.
#[derive(Debug, Clone, Copy)]
pub struct OsPartitionIdentity<'a> {
    /// The physical `scope_owner` string.
    pub owner_key: &'a str,
    /// The encoding of `owner_key`.
    pub key_version: &'a str,
    /// The organisation, from `mem_orgs`. Opaque — never parsed.
    pub org_id: &'a str,
    /// The container within that org.
    pub space_id: &'a str,
    /// The user this partition was created for.
    pub user: &'a str,
    /// The module's manifest name at first sight.
    pub module: &'a str,
}

/// Every table keyed by `scope_owner` that [`KvStore::rekey_os_partition`] does
/// NOT move — so it can refuse instead of orphaning them.
///
/// `mem_events` and `mem_checkpoints` are absent because they ARE moved. The
/// assertion FTS shadow is absent because it is trigger-maintained from
/// `mem_assertions`, which is here.
///
/// A new owner-scoped table must be added to this list or explicitly moved.
/// Nothing enforces that mechanically — a schema-introspecting test was
/// considered and would have to encode the same list to know what to expect, so
/// it would restate this constant rather than check it.
const OTHER_OWNER_SCOPED_TABLES: &[&str] = &[
    "mem_artifacts",
    "mem_artifact_versions",
    "mem_assertions",
    "mem_consolidations",
    "mem_embeddings",
    "mem_instructions",
    "mem_trace_refs",
    "mem_trace_nodes",
];

/// One catalog row, read in the one place so two readers cannot disagree about
/// which column is which.
fn os_partition_row(r: &sqlx::sqlite::SqliteRow) -> OsPartitionRow {
    OsPartitionRow {
        owner_key: r.get("owner_key"),
        key_version: r.get("key_version"),
        org_id: r.get("org_id"),
        space_id: r.get("space_id"),
        logical_user: r.get("logical_user"),
        module_name: r.get("module_name"),
        first_seen_at: r.get("first_seen_at"),
        last_seen_at: r.get("last_seen_at"),
    }
}

/// L0: a namespaced JSON key-value store over SQLite (WAL, 5s busy timeout).
#[derive(Clone)]
pub struct KvStore {
    pool: SqlitePool,
}

impl KvStore {
    /// Open (creating if needed) a database file and run migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| MemoryError::Io(format!("mkdir: {e}")))?;
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            // FKs are OFF by default in SQLite; the trace projection (MD-8) relies
            // on a composite FK so a node's ref can never be unresolvable.
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// In-memory database for tests. A single connection: every `:memory:`
    /// connection is a distinct database.
    pub async fn open_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// An [`event::EventLog`] over the SAME database file — MD-2's append-only
    /// episodic authority shares the KV store's pool. (Same file, NOT the same
    /// transaction: `EventLog` runs its own statements on the pool, so a caller
    /// cannot yet atomically commit a KV write and an append together — that
    /// cross-store transaction seam is MD-2b work.)
    pub fn events(&self) -> event::EventLog {
        event::EventLog::new(self.pool.clone())
    }

    /// An [`artifact::ArtifactCas`] over the SAME database file — MD-2b's
    /// CAS-versioned editable-content authority shares the KV store's pool.
    pub fn artifacts(&self) -> artifact::ArtifactCas {
        artifact::ArtifactCas::new(self.pool.clone())
    }

    /// An [`assertion::AssertionLedger`] over the SAME database file — MD-3a's
    /// bi-temporal semantic authority shares the KV store's pool.
    pub fn assertions(&self) -> assertion::AssertionLedger {
        assertion::AssertionLedger::new(self.pool.clone())
    }

    /// An [`retriever::FtsRetriever`] over the SAME database file — MD-3b's
    /// full-text projection over the assertion ledger.
    pub fn retriever(&self) -> retriever::FtsRetriever {
        retriever::FtsRetriever::new(self.pool.clone())
    }

    /// A [`writer::WriteGate`] over the SAME database file — MD-4's governance
    /// write-gate, committing an assertion and its audit event atomically.
    pub fn write_gate(&self) -> writer::WriteGate {
        writer::WriteGate::new(self.pool.clone())
    }

    /// An [`consolidator::EventConsolidator`] over the SAME database file — MD-5's
    /// consolidation loop, with the default deterministic [`consolidator::CountSynth`].
    pub fn consolidator(&self) -> consolidator::EventConsolidator<consolidator::CountSynth> {
        consolidator::EventConsolidator::new(self.pool.clone(), consolidator::CountSynth)
    }

    /// An [`knowledge::InstructionStore`] over the SAME database file — MD-7's
    /// layered instruction/knowledge layer with a review-gated auto-memory inbox.
    pub fn knowledge(&self) -> knowledge::InstructionStore {
        knowledge::InstructionStore::new(self.pool.clone())
    }

    /// A [`trace::SymbolicTrace`] over the SAME database file — MD-8's symbolic
    /// task trace (full bodies spilled to refs, prompt keeps symbols + drill-down).
    pub fn trace(&self) -> trace::SymbolicTrace {
        trace::SymbolicTrace::new(self.pool.clone())
    }

    /// A [`vector::VectorRetriever`] over the SAME database file with a chosen
    /// [`vector::Embedder`] (MD-6). The embedder is caller-supplied (`OmlxEmbedder`
    /// in production, pending D4b) so the store never hard-depends on a model runtime.
    pub fn vector_retriever<E: vector::Embedder + Clone>(
        &self,
        embedder: E,
    ) -> vector::VectorRetriever<E> {
        vector::VectorRetriever::new(self.pool.clone(), embedder)
    }

    /// Record that `owner_key` is the memory partition of `(user, module)`, and
    /// that it was seen now.
    ///
    /// Idempotent, and **write-once on the immutable columns**: a repeat call with
    /// the same identity advances `last_seen_at` and nothing else, because
    /// `first_seen_at` and `module_name` exist to say what a partition ORIGINALLY
    /// was. A module rename must leave the old row intact — that row is the only
    /// thing that can tell a later migration what `…os:calendar` used to mean.
    ///
    /// A repeat call that DISAGREES about `key_version`, `org_id`, `space_id` or
    /// `module_name` is a [`MemoryError::Conflict`], not an update and not a
    /// silent success. The first version treated every conflict as success and
    /// updated only `last_seen_at`, so a key whose stored identity had drifted —
    /// through a future key-encoder change, another kernel caller, or corruption —
    /// would still return `Ok`, the kernel would hand out the handle, and the
    /// catalog would go on attributing new rows to the old identity. A catalog
    /// that reports success while disagreeing with itself is worse than no
    /// catalog, because the caller is entitled to believe it.
    ///
    /// # `logical_user` is NOT part of that identity, and cannot be
    ///
    /// It records which user first caused the partition to exist, write-once
    /// like `module_name` and `first_seen_at`. Guarding on it looked right and
    /// broke the entire point of F8, which review caught: a partition is owned
    /// by an (org, space), so every member of that org derives the SAME key. If
    /// the upsert demanded that the mounting user match the creator, then the
    /// day an org gained a second member, that member's mount would affect zero
    /// rows, return a conflict, and be refused the memory capability for good —
    /// while the code claimed to have made a second member cheap.
    ///
    /// The identity a key encodes is `(org_id, space_id)`. Who walked up to it
    /// is not part of it.
    pub async fn record_os_partition(&self, id: OsPartitionIdentity<'_>) -> Result<()> {
        let now = now_iso8601();
        // The guarded arm updates ZERO rows when the immutable columns disagree,
        // which is how the disagreement is detected: SQLite reports the conflict
        // as "handled" either way, so `rows_affected()` is the only signal.
        let res = sqlx::query(
            "INSERT INTO mem_os_partitions
                 (owner_key, key_version, org_id, space_id, logical_user,
                  module_name, first_seen_at, last_seen_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(owner_key) DO UPDATE SET last_seen_at = excluded.last_seen_at
               WHERE key_version = excluded.key_version
                 AND org_id = excluded.org_id
                 AND space_id = excluded.space_id
                 AND module_name = excluded.module_name",
        )
        .bind(id.owner_key)
        .bind(id.key_version)
        .bind(id.org_id)
        .bind(id.space_id)
        .bind(id.user)
        .bind(id.module)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(crate::MemoryError::Conflict(format!(
                "memory partition {:?} is already recorded with a different identity \
                 than ({}, {}, {}, {}) — refusing to re-attribute it",
                id.owner_key, id.key_version, id.org_id, id.space_id, id.module
            )));
        }
        Ok(())
    }

    /// The org `user` acts in, creating it on first sight.
    ///
    /// This is the seam the whole F8 design rests on, so it is worth being
    /// precise about what it does and does not decide.
    ///
    /// It RESOLVES BY MEMBERSHIP. The org id is opaque and generated; nothing
    /// derives it from the user's name and nothing parses it back. That is what
    /// keeps `v2` terminal for this dimension: adding members to an org, or
    /// renaming it, changes rows and never touches a key. The rejected
    /// alternative was `org_id = "u:" || user`, which reintroduces the defect F8
    /// exists to fix one level up — an org whose identity is a function of a
    /// user has to be re-issued the day it gains a second member, and its id is
    /// baked into every owner key.
    ///
    /// It FAILS CLOSED on ambiguity. A user in two orgs has no single answer,
    /// and picking one — the first, the newest, the alphabetically smallest —
    /// would silently bind every partition to an arbitrary org. There is no path
    /// that creates a second membership today; when there is, the caller will
    /// have to say WHICH org it is acting in, and this returning an error is
    /// what forces that rather than letting a default settle in.
    pub async fn ensure_org_for_user(&self, user: &str) -> Result<String> {
        if user.trim().is_empty() {
            return Err(MemoryError::Conflict(
                "cannot resolve an org for a blank user".into(),
            ));
        }
        if let Some(found) = self.resolve_org_for_user(user).await? {
            return Ok(found);
        }
        // Creating it is guarded against a concurrent creator, because losing that
        // race is not a retry — it is PERMANENT. Two callers that each minted a
        // fresh ULID would leave the user in two orgs, which is the ambiguity
        // above, which means every later resolve fails and the memory capability
        // is withheld from every module for good. A transient race producing an
        // unrecoverable state is worth a guard even where only one caller exists
        // today.
        //
        // The membership insert is the one that decides. It writes only if this
        // user still has no org, so exactly one of two racing transactions can
        // affect a row; the loser rolls back — taking its orphan `mem_orgs` row
        // with it, which is why the org is inserted inside the same transaction
        // rather than before it — and re-reads the winner's answer.
        let now = now_iso8601();
        let org_id = format!("org_{}", agent24_core::util::ulid());
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO mem_orgs (org_id, display_name, created_at) VALUES (?, ?, ?)")
            .bind(&org_id)
            .bind(user)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        let claimed = sqlx::query(
            "INSERT INTO mem_org_members (org_id, user_id, joined_at)
             SELECT ?, ?, ?
             WHERE NOT EXISTS (SELECT 1 FROM mem_org_members WHERE user_id = ?)",
        )
        .bind(&org_id)
        .bind(user)
        .bind(&now)
        .bind(user)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if claimed == 0 {
            tx.rollback().await?;
            // Read the winner's answer rather than assume it. One re-read, NOT a
            // recursive call: the losing branch is reachable only because a
            // membership row now exists, so there is nothing left to create and a
            // second attempt could only loop.
            return self.resolve_org_for_user(user).await?.ok_or_else(|| {
                MemoryError::Conflict(format!(
                    "another writer claimed the org for {user:?} and then removed it"
                ))
            });
        }
        tx.commit().await?;
        Ok(org_id)
    }

    /// The single org `user` belongs to, or `None` if they belong to none.
    ///
    /// `Err` on more than one — see [`Self::ensure_org_for_user`] for why an
    /// ambiguous membership must not resolve to a choice.
    async fn resolve_org_for_user(&self, user: &str) -> Result<Option<String>> {
        let existing: Vec<String> =
            sqlx::query("SELECT org_id FROM mem_org_members WHERE user_id = ? ORDER BY org_id ASC")
                .bind(user)
                .fetch_all(&self.pool)
                .await?
                .iter()
                .map(|r| r.get::<String, _>("org_id"))
                .collect();
        match existing.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(one.clone())),
            many => Err(MemoryError::Conflict(format!(
                "user {user:?} belongs to {} orgs ({many:?}); the caller must say \
                 which one it is acting in rather than have one chosen for it",
                many.len()
            ))),
        }
    }

    /// Every partition recorded under `key_version`, oldest first.
    ///
    /// What drives a key-version migration: the explicit list the F1 review
    /// insisted on, instead of prefix-matching strings that contain NUL.
    pub async fn os_partitions_with_key_version(
        &self,
        key_version: &str,
    ) -> Result<Vec<OsPartitionRow>> {
        let rows = sqlx::query(
            "SELECT owner_key, key_version, org_id, space_id, logical_user,
                    module_name, first_seen_at, last_seen_at
             FROM mem_os_partitions WHERE key_version = ?
             ORDER BY first_seen_at ASC, owner_key ASC",
        )
        .bind(key_version)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(os_partition_row).collect())
    }

    /// Move a partition from `old_key` to `new_key`, atomically with its EVENT
    /// rows and its projection bookmarks — and only those.
    ///
    /// Returns how many events moved.
    ///
    /// # It refuses rather than move the other eight owner-scoped tables
    ///
    /// `scope_owner` also keys `mem_artifacts`, `mem_artifact_versions`,
    /// `mem_assertions` (plus its trigger-maintained FTS shadow),
    /// `mem_consolidations`, `mem_embeddings`, `mem_instructions`,
    /// `mem_trace_refs` and `mem_trace_nodes`. This moves NONE of them. It
    /// instead refuses to run when any of them holds a row under `old_key`.
    ///
    /// The first version of this doc said "atomically with its rows" while
    /// touching two tables of ten, which review called correctly: after such a
    /// commit the catalog would point at `new_key` while artifacts, assertions
    /// and traces stayed behind, orphaned silently rather than loudly.
    ///
    /// Refusing is the right shape rather than a smaller one. Nothing can put a
    /// row in those tables under a partition key today — [`crate::KvStore`]
    /// hands a domain OS only an `EventLog`, so `remember` can write nowhere
    /// else — which makes a mover for them code with no caller, written against
    /// a guess about what a future writer would need (the assertion FTS shadow
    /// alone needs its own handling). A refusal is a fact this function can
    /// check. If a later change lets a module write assertions, this fails on
    /// the first partition instead of quietly leaving them behind, and whoever
    /// makes that change gets told to come back here.
    ///
    /// # Why this is one transaction and not two statements
    ///
    /// The catalog row and the event rows are the same fact stated twice. If the
    /// catalog moved and the events did not, the kernel would hand a module a
    /// handle onto an EMPTY partition while its memories sat under a key nothing
    /// points at any more — data that is not lost, not reachable, and no longer
    /// discoverable, which is precisely the orphaning the catalog exists to
    /// prevent. If the events moved and the catalog did not, the next startup
    /// would try the migration again and find the source empty.
    ///
    /// # The occupied-key check is a DIAGNOSTIC, not the safety property
    ///
    /// Stated precisely because the first version of this comment claimed
    /// otherwise, and a mutation check caught it: deleting the check below left
    /// every test passing. It had to — `owner_key` is the catalog's PRIMARY KEY,
    /// so the catalog `UPDATE` fails on its own, and because both statements are
    /// in one transaction the event move rolls back with it. What actually keeps
    /// two partitions from merging is the primary key plus the transaction.
    ///
    /// The check earns its place for a smaller reason: it turns a raw
    /// `SQLITE_CONSTRAINT` from a caller-facing API into a typed `Conflict` that
    /// says which two keys collided, and it declines to rewrite a partition's
    /// worth of events before finding out. Do not upgrade that description — and
    /// do not remove the transaction on the grounds that this check makes it
    /// unnecessary, because it is the other way round.
    pub async fn rekey_os_partition(
        &self,
        old_key: &str,
        new_key: &str,
        new_key_version: &str,
    ) -> Result<u64> {
        if old_key == new_key {
            return Err(MemoryError::Conflict(format!(
                "re-key of {old_key:?} to itself"
            )));
        }
        let mut tx = self.pool.begin().await?;
        // Checked INSIDE the transaction, so a writer cannot slip a row into one
        // of these between the check and the move.
        for table in OTHER_OWNER_SCOPED_TABLES {
            let n: i64 = sqlx::query_scalar(&format!(
                "SELECT COUNT(*) FROM {table} WHERE scope_owner = ?"
            ))
            .bind(old_key)
            .fetch_one(&mut *tx)
            .await?;
            if n > 0 {
                return Err(MemoryError::Conflict(format!(
                    "cannot re-key {old_key:?}: {table} holds {n} row(s) under it, and \
                     this function moves only events and checkpoints. Moving a partition \
                     while leaving those behind would orphan them silently — teach this \
                     function to move {table} before allowing it"
                )));
            }
        }
        let taken: Option<String> =
            sqlx::query("SELECT owner_key FROM mem_os_partitions WHERE owner_key = ?")
                .bind(new_key)
                .fetch_optional(&mut *tx)
                .await?
                .map(|r| r.get("owner_key"));
        if taken.is_some() {
            return Err(MemoryError::Conflict(format!(
                "cannot re-key {old_key:?}: {new_key:?} is already a recorded partition, \
                 and merging two partitions is exactly the cross-partition leak this \
                 catalog exists to prevent"
            )));
        }
        let moved = sqlx::query("UPDATE mem_events SET scope_owner = ? WHERE scope_owner = ?")
            .bind(new_key)
            .bind(old_key)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        // Projection bookmarks are owner-scoped too (0010), so they move with the
        // partition or they point at an owner that no longer exists.
        //
        // Nothing consumes a module's events today, so this moves zero rows in
        // practice — which is the reason to write it now rather than the reason
        // to skip it. "The table happens to be empty" is a fact about this week,
        // not a property of the re-key, and a consumer added later would leave
        // its bookmark behind silently. Losing one is survivable (0010 says so:
        // a lost checkpoint means re-folding from seq 0, correct but slower);
        // a bookmark stranded under a dead owner while a fresh one starts at 0
        // for the live key is the same cost with an orphan row left over.
        sqlx::query("UPDATE mem_checkpoints SET scope_owner = ? WHERE scope_owner = ?")
            .bind(new_key)
            .bind(old_key)
            .execute(&mut *tx)
            .await?;
        let updated = sqlx::query(
            "UPDATE mem_os_partitions SET owner_key = ?, key_version = ? WHERE owner_key = ?",
        )
        .bind(new_key)
        .bind(new_key_version)
        .bind(old_key)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated != 1 {
            return Err(MemoryError::Conflict(format!(
                "re-key of {old_key:?} updated {updated} catalog rows, expected exactly 1"
            )));
        }
        tx.commit().await?;
        Ok(moved)
    }

    /// Every partition ever recorded for `user`, oldest first.
    ///
    /// The answer an export or erase path needs — INCLUDING partitions belonging
    /// to modules that are disabled, uninstalled or renamed, which is why it
    /// reads the table rather than whatever mounted this run.
    pub async fn os_partitions_for(&self, user: &str) -> Result<Vec<OsPartitionRow>> {
        let rows = sqlx::query(
            "SELECT owner_key, key_version, org_id, space_id, logical_user,
                    module_name, first_seen_at, last_seen_at
             FROM mem_os_partitions WHERE logical_user = ?
             ORDER BY first_seen_at ASC, owner_key ASC",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(os_partition_row).collect())
    }

    /// Upsert a raw JSON value.
    pub async fn set(&self, namespace: &str, key: &str, value: &Value) -> Result<()> {
        sqlx::query(
            "INSERT INTO kv (namespace, key, value, updated_at) VALUES (?, ?, ?, ?)
             ON CONFLICT(namespace, key) DO UPDATE SET
                 value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(namespace)
        .bind(key)
        .bind(serde_json::to_string(value)?)
        .bind(now_iso8601())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Get a raw JSON value, or `None` if the key is absent.
    pub async fn get(&self, namespace: &str, key: &str) -> Result<Option<Value>> {
        let row = sqlx::query("SELECT value FROM kv WHERE namespace = ? AND key = ?")
            .bind(namespace)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(row) => Ok(Some(serde_json::from_str(&row.get::<String, _>("value"))?)),
            None => Ok(None),
        }
    }

    /// Typed upsert — serializes `value` to JSON.
    pub async fn put<T: Serialize>(&self, namespace: &str, key: &str, value: &T) -> Result<()> {
        self.set(namespace, key, &serde_json::to_value(value)?)
            .await
    }

    /// Typed get — deserializes into `T`, or `None` if absent.
    pub async fn fetch<T: DeserializeOwned>(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<T>> {
        match self.get(namespace, key).await? {
            Some(value) => Ok(Some(serde_json::from_value(value)?)),
            None => Ok(None),
        }
    }

    /// Delete a key; returns whether a row was removed.
    pub async fn delete(&self, namespace: &str, key: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM kv WHERE namespace = ? AND key = ?")
            .bind(namespace)
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// All keys in a namespace, sorted.
    pub async fn keys(&self, namespace: &str) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT key FROM kv WHERE namespace = ? ORDER BY key ASC")
            .bind(namespace)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| r.get::<String, _>("key")).collect())
    }

    /// All (key, value) pairs in a namespace, sorted by key.
    pub async fn entries(&self, namespace: &str) -> Result<Vec<(String, Value)>> {
        let rows = sqlx::query("SELECT key, value FROM kv WHERE namespace = ? ORDER BY key ASC")
            .bind(namespace)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                let key: String = r.get("key");
                let value: Value = serde_json::from_str(&r.get::<String, _>("value"))?;
                Ok((key, value))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::event::EventStore as _;
    use serde::Deserialize;

    #[tokio::test]
    async fn a_user_in_two_orgs_is_an_error_rather_than_a_guess() {
        // No API creates a second membership — this reaches for the pool
        // precisely because the state is not constructible through the surface,
        // and a guard that can never be exercised is a guard nobody can trust.
        //
        // What it pins: `ensure_org_for_user` must NOT pick one. Every partition
        // key contains the org id, so a silent choice here binds a user's whole
        // memory to whichever org happened to sort first, and the wrong choice
        // is only discoverable as missing history.
        let kv = KvStore::open_memory().await.unwrap();
        let first = kv.ensure_org_for_user("alice").await.unwrap();
        let now = now_iso8601();
        sqlx::query("INSERT INTO mem_orgs (org_id, display_name, created_at) VALUES (?, ?, ?)")
            .bind("org_second")
            .bind("Second")
            .bind(&now)
            .execute(&kv.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO mem_org_members (org_id, user_id, joined_at) VALUES (?, ?, ?)")
            .bind("org_second")
            .bind("alice")
            .bind(&now)
            .execute(&kv.pool)
            .await
            .unwrap();

        let err = kv
            .ensure_org_for_user("alice")
            .await
            .expect_err("two memberships have no single answer");
        assert!(matches!(err, MemoryError::Conflict(_)), "{err}");
        // And it did not quietly mint a THIRD org while failing.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mem_orgs")
            .fetch_one(&kv.pool)
            .await
            .unwrap();
        assert_eq!(count, 2, "{first} plus the one this test inserted");
    }

    /// Open a database migrated only as far as `stop_before`, so a migration can
    /// be tested against the schema it will actually meet.
    ///
    /// `KvStore::open*` runs every migration, which is why the first version of
    /// the 0013 tests proved nothing about 0013: they inserted v1-shaped rows
    /// into an already-upgraded schema and exercised only the Rust sweep. The
    /// upgrade path — the backfill, the org rows, the rebuilt catalog — was the
    /// one part of this change that touches data a user already has, and it was
    /// the one part with no test.
    async fn pool_migrated_up_to(path: &Path, stop_before: i64) -> SqlitePool {
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        // Through the real migrator, with the later migrations removed — NOT by
        // executing the SQL by hand. Hand-executing leaves `_sqlx_migrations`
        // empty, so the next `KvStore::open` starts again at 0001 and fails on
        // "table kv already exists" (which is how the first attempt at this
        // helper was caught).
        let mut migrator = sqlx::migrate!("./migrations");
        migrator.migrations = migrator
            .migrations
            .iter()
            .filter(|m| m.version < stop_before)
            .cloned()
            .collect::<Vec<_>>()
            .into();
        migrator.run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn migration_0013_gives_an_existing_0012_partition_an_org_and_a_space() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        let pool = pool_migrated_up_to(&path, 13).await;

        // A catalog row exactly as F1 left it: the 0012 schema, six columns.
        sqlx::query(
            "INSERT INTO mem_os_partitions
                 (owner_key, key_version, logical_user, module_name,
                  first_seen_at, last_seen_at)
             VALUES ('v1-key-for-sin90', 'v1', 'alice', 'sin90', '2026-08-22T00:00:00Z',
                     '2026-08-22T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mem_os_partitions
                 (owner_key, key_version, logical_user, module_name,
                  first_seen_at, last_seen_at)
             VALUES ('v1-key-for-cos72', 'v1', 'alice', 'cos72', '2026-08-22T01:00:00Z',
                     '2026-08-22T01:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        // Now run the rest, which is 0013.
        let kv = KvStore::open(&path).await.unwrap();

        let rows = kv.os_partitions_for("alice").await.unwrap();
        assert_eq!(rows.len(), 2, "the rebuild must not lose a row");
        for r in &rows {
            // The space the backfill wrote must be the one the KERNEL derives, or
            // the sweep never recomputes this partition's key and its history
            // disappears. This is the real version of the assertion that used to
            // compare a Rust constant against itself.
            assert_eq!(
                r.space_id,
                format!("os:{}", r.module_name),
                "0013's backfill and SpaceId::module_private must agree"
            );
            assert_eq!(r.key_version, "v1", "the rebuild must not claim a re-key");
            assert_eq!(r.org_id, rows[0].org_id, "one user, one org");
        }
        assert_eq!(
            rows[0].first_seen_at, "2026-08-22T00:00:00Z",
            "first_seen_at is what it always was"
        );

        // The org exists as a row, with the user as a member — so the resolver
        // finds it rather than minting a second one for the same person.
        assert_eq!(
            kv.ensure_org_for_user("alice").await.unwrap(),
            rows[0].org_id,
            "a migrated user must resolve to their migrated org"
        );
        let orgs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mem_orgs")
            .fetch_one(&kv.pool)
            .await
            .unwrap();
        assert_eq!(orgs, 1);
    }

    #[tokio::test]
    async fn migration_0013_is_fine_on_a_database_with_no_partitions() {
        // The common case, and the one where a backfill with a GROUP BY can
        // quietly do something odd (insert one all-NULL org row).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        pool_migrated_up_to(&path, 13).await.close().await;
        let kv = KvStore::open(&path).await.unwrap();
        let orgs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mem_orgs")
            .fetch_one(&kv.pool)
            .await
            .unwrap();
        assert_eq!(orgs, 0, "no partitions means no orgs to invent");
        let members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mem_org_members")
            .fetch_one(&kv.pool)
            .await
            .unwrap();
        assert_eq!(members, 0);
    }

    #[tokio::test]
    async fn a_second_member_of_an_org_mounts_the_same_partition() {
        // F8's entire justification is that a second member becomes cheap. Review
        // found the catalog forbidding exactly that: the upsert guarded on
        // `logical_user`, so once Alice had created the org's `os:calendar`
        // partition, Bob — same org, same space, therefore the SAME derived key —
        // affected zero rows, got a conflict, and was refused the memory
        // capability permanently. The commit would have claimed to have made a
        // second member possible while the storage layer said no.
        let kv = KvStore::open_memory().await.unwrap();
        let org = kv.ensure_org_for_user("alice").await.unwrap();
        let now = now_iso8601();
        sqlx::query("INSERT INTO mem_org_members (org_id, user_id, joined_at) VALUES (?, ?, ?)")
            .bind(&org)
            .bind("bob")
            .bind(&now)
            .execute(&kv.pool)
            .await
            .unwrap();
        assert_eq!(
            kv.ensure_org_for_user("bob").await.unwrap(),
            org,
            "a member resolves to the org they are IN, not a fresh one"
        );

        let id = |user| OsPartitionIdentity {
            owner_key: "k",
            key_version: "v2",
            org_id: &org,
            space_id: "os:calendar",
            user,
            module: "calendar",
        };
        kv.record_os_partition(id("alice")).await.unwrap();
        kv.record_os_partition(id("bob"))
            .await
            .expect("the second member must reach the org's own partition");

        // And the row still says who created it — write-once, like module_name.
        let rows = kv.os_partitions_for("alice").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].logical_user, "alice",
            "logical_user records the CREATOR; Bob mounting must not rewrite it"
        );
    }

    #[tokio::test]
    async fn a_rekey_refuses_when_another_owner_scoped_table_holds_rows() {
        // `scope_owner` keys ten tables; this moves two. The first version of the
        // doc said "atomically with its rows" and review was right that after such
        // a commit the catalog points at the new key while artifacts, assertions
        // and traces stay behind — orphaned silently.
        //
        // Nothing can put a row there through a module handle today, so the fix
        // is a refusal rather than a mover written against a guess.
        let kv = KvStore::open_memory().await.unwrap();
        let org = kv.ensure_org_for_user("alice").await.unwrap();
        kv.record_os_partition(OsPartitionIdentity {
            owner_key: "old",
            key_version: "v1",
            org_id: &org,
            space_id: "os:sin90",
            user: "alice",
            module: "sin90",
        })
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mem_instructions
                 (scope_owner, id, layer, priority, body, triggers, status, at)
             VALUES (?, 'i1', 'global', 0, 'remember this', '[]', 'active', ?)",
        )
        .bind("old")
        .bind(now_iso8601())
        .execute(&kv.pool)
        .await
        .unwrap();

        let err = kv
            .rekey_os_partition("old", "new", "v2")
            .await
            .expect_err("a partition with rows this cannot move must not move");
        assert!(matches!(err, MemoryError::Conflict(_)), "{err}");
        // And it refused BEFORE touching anything.
        let rows = kv.os_partitions_for("alice").await.unwrap();
        assert_eq!(rows[0].owner_key, "old");
        assert_eq!(rows[0].key_version, "v1");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_first_resolves_produce_exactly_one_org() {
        // Losing this race is not a retry, it is permanent: two callers that each
        // minted an org would leave the user in two, and every later resolve then
        // fails, withholding memory from every module for good.
        //
        // HONEST ABOUT WHAT THIS IS: a race test, so it is a NET rather than a
        // proof — an unguarded implementation could get lucky and serialise. It
        // is here because the guarded branch is not otherwise reachable from a
        // deterministic test (it needs a row to appear between one caller's read
        // and its write), and a guard with no test at all is how a guard gets
        // deleted. Mutation-checked: with the `WHERE NOT EXISTS` dropped, this
        // fails.
        //
        // A FILE database on purpose — `open_memory` holds a single connection,
        // which would serialise the callers and test nothing.
        let dir = tempfile::tempdir().unwrap();
        let kv = KvStore::open(&dir.path().join("m.db")).await.unwrap();
        let racers: Vec<_> = (0..8)
            .map(|_| {
                let kv = kv.clone();
                tokio::spawn(async move { kv.ensure_org_for_user("alice").await })
            })
            .collect();
        let mut answers = Vec::new();
        for r in racers {
            answers.push(r.await.unwrap().unwrap());
        }
        let first = &answers[0];
        assert!(
            answers.iter().all(|a| a == first),
            "every caller must get the SAME org: {answers:?}"
        );
        let orgs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mem_orgs")
            .fetch_one(&kv.pool)
            .await
            .unwrap();
        assert_eq!(orgs, 1, "a losing racer must not leave its org row behind");
    }

    #[tokio::test]
    async fn a_rekey_moves_the_events_and_the_catalog_row_together() {
        // The two are the same fact stated twice. Half a move is worse than no
        // move: a catalog row pointing at an empty partition means the module
        // gets a handle onto nothing while its memories sit under a key nothing
        // references any more — not lost, not reachable, not discoverable.
        let kv = KvStore::open_memory().await.unwrap();
        let org = kv.ensure_org_for_user("alice").await.unwrap();
        kv.record_os_partition(OsPartitionIdentity {
            owner_key: "old",
            key_version: "v1",
            org_id: &org,
            space_id: "os:sin90",
            user: "alice",
            module: "sin90",
        })
        .await
        .unwrap();
        let log = kv.events();
        log.append(&event::MemEvent::new(
            "e1",
            event::Scope::owner("old"),
            "note",
            serde_json::json!({}),
            event::Origin {
                source: "test".into(),
                trust: event::Trust::ToolOutput,
            },
        ))
        .await
        .unwrap();

        assert_eq!(kv.rekey_os_partition("old", "new", "v2").await.unwrap(), 1);
        let rows = kv.os_partitions_for("alice").await.unwrap();
        assert_eq!(rows.len(), 1, "a re-key MOVES a row, it does not add one");
        assert_eq!(rows[0].owner_key, "new");
        assert_eq!(rows[0].key_version, "v2");
        // The logical identity is what the key encodes, so it must NOT move.
        assert_eq!(rows[0].org_id, org);
        assert_eq!(rows[0].space_id, "os:sin90");
        assert_eq!(
            log.scan(&event::EventQuery::owner("new"))
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            log.scan(&event::EventQuery::owner("old"))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn recording_a_partition_that_claims_another_space_is_a_conflict() {
        // 0012 guarded the immutable columns; F8 adds two more that ARE the
        // identity. A key whose recorded space disagrees with the one the caller
        // is about to use means the encoder and the catalog have diverged, and
        // lending the handle would put one space's writes in another's rows.
        let kv = KvStore::open_memory().await.unwrap();
        let org = kv.ensure_org_for_user("alice").await.unwrap();
        let mut id = OsPartitionIdentity {
            owner_key: "k",
            key_version: "v2",
            org_id: &org,
            space_id: "os:sin90",
            user: "alice",
            module: "sin90",
        };
        kv.record_os_partition(id).await.unwrap();
        id.space_id = "os:cos72";
        let err = kv
            .record_os_partition(id)
            .await
            .expect_err("a drifted space must not be a silent success");
        assert!(matches!(err, MemoryError::Conflict(_)), "{err}");
    }

    #[tokio::test]
    async fn a_partition_cannot_belong_to_an_org_that_does_not_exist() {
        // Found by two of this file's own tests failing on it, which is the
        // better way to learn a constraint is real. `org_id` is a FOREIGN KEY, so
        // a partition cannot be attributed to an org nothing ever created —
        // exactly the orphan the catalog exists to prevent, one level up: a row
        // whose org cannot be looked up is a row no export or erase path can act
        // on.
        //
        // Worth pinning because it is easy to lose: SQLite has foreign keys OFF
        // by default, and this database only has them on because `KvStore::open`
        // asks for them (for MD-8's trace projection). Turning that off would
        // silently downgrade this from a guarantee to a comment.
        let kv = KvStore::open_memory().await.unwrap();
        let err = kv
            .record_os_partition(OsPartitionIdentity {
                owner_key: "k",
                key_version: "v2",
                org_id: "org_never_created",
                space_id: "os:sin90",
                user: "alice",
                module: "sin90",
            })
            .await
            .expect_err("an unknown org must not be recordable");
        assert!(
            matches!(err, MemoryError::Sqlx(_)),
            "expected the FK to reject it, got {err}"
        );
    }

    #[tokio::test]
    async fn set_get_delete_roundtrip() {
        let kv = KvStore::open_memory().await.unwrap();
        assert_eq!(kv.get("ns", "missing").await.unwrap(), None);

        kv.set("ns", "k", &serde_json::json!({"a": 1}))
            .await
            .unwrap();
        assert_eq!(
            kv.get("ns", "k").await.unwrap(),
            Some(serde_json::json!({"a": 1}))
        );

        // upsert overwrites
        kv.set("ns", "k", &serde_json::json!({"a": 2}))
            .await
            .unwrap();
        assert_eq!(
            kv.get("ns", "k").await.unwrap(),
            Some(serde_json::json!({"a": 2}))
        );

        assert!(kv.delete("ns", "k").await.unwrap());
        assert!(!kv.delete("ns", "k").await.unwrap());
        assert_eq!(kv.get("ns", "k").await.unwrap(), None);
    }

    #[tokio::test]
    async fn namespaces_are_isolated() {
        let kv = KvStore::open_memory().await.unwrap();
        kv.set("a", "shared", &serde_json::json!(1)).await.unwrap();
        kv.set("b", "shared", &serde_json::json!(2)).await.unwrap();
        assert_eq!(
            kv.get("a", "shared").await.unwrap(),
            Some(serde_json::json!(1))
        );
        assert_eq!(
            kv.get("b", "shared").await.unwrap(),
            Some(serde_json::json!(2))
        );
        // deleting in one namespace leaves the other
        kv.delete("a", "shared").await.unwrap();
        assert_eq!(kv.get("a", "shared").await.unwrap(), None);
        assert_eq!(
            kv.get("b", "shared").await.unwrap(),
            Some(serde_json::json!(2))
        );
    }

    #[tokio::test]
    async fn typed_put_fetch() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Prefs {
            theme: String,
            count: u32,
        }
        let kv = KvStore::open_memory().await.unwrap();
        let prefs = Prefs {
            theme: "dark".to_owned(),
            count: 3,
        };
        kv.put("cfg", "prefs", &prefs).await.unwrap();
        assert_eq!(
            kv.fetch::<Prefs>("cfg", "prefs").await.unwrap(),
            Some(prefs)
        );
        assert_eq!(kv.fetch::<Prefs>("cfg", "nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn keys_and_entries_are_sorted_and_scoped() {
        let kv = KvStore::open_memory().await.unwrap();
        kv.set("ns", "b", &serde_json::json!("B")).await.unwrap();
        kv.set("ns", "a", &serde_json::json!("A")).await.unwrap();
        kv.set("other", "z", &serde_json::json!("Z")).await.unwrap();
        assert_eq!(kv.keys("ns").await.unwrap(), vec!["a", "b"]);
        let entries = kv.entries("ns").await.unwrap();
        assert_eq!(entries[0], ("a".to_owned(), serde_json::json!("A")));
        assert_eq!(entries[1], ("b".to_owned(), serde_json::json!("B")));
        assert_eq!(kv.keys("other").await.unwrap(), vec!["z"]);
    }

    #[tokio::test]
    async fn persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("a24mem-{}", std::process::id()));
        let path = dir.join("mem.db");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let kv = KvStore::open(&path).await.unwrap();
            kv.set("ns", "k", &serde_json::json!("v")).await.unwrap();
        }
        let kv = KvStore::open(&path).await.unwrap();
        assert_eq!(
            kv.get("ns", "k").await.unwrap(),
            Some(serde_json::json!("v"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
