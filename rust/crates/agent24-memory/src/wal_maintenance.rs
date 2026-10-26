//! P1-02c-c5 (design D7): idle-time `wal_checkpoint(TRUNCATE)` for `memory.db`.
//!
//! The WAL only ever grows between checkpoints while readers keep it pinned;
//! c0 measured a ~4 MiB peak that returns to zero after a checkpoint. This
//! module bounds it without touching logical data: no `VACUUM`, no
//! `incremental_vacuum`, no row is deleted (those belong to purge/archive).
//!
//! # Idle signal
//!
//! No write path is instrumented. The maintainer owns ONE detached connection
//! (not counted against the pool) and reads `PRAGMA data_version` on it:
//! SQLite bumps that value whenever a *different* connection commits, so any
//! write through the pool — events, assertions, kv, session log, FTS — moves
//! it, and our own checkpoint does not. "Idle" means the value has not moved
//! for [`WalMaintenanceConfig::idle_after`].
//!
//! # Policy (see [`WalMaintainer::tick`])
//!
//! 1. not idle                      -> skip
//! 2. last success < `min_interval` -> skip
//! 3. a busy result backs off one `idle_after` (no retry bombardment)
//! 4. empty WAL                     -> skip
//! 5. `PRAGMA wal_checkpoint(PASSIVE)` first (backfill without the writer
//!    lock); only if it finished everything (`log == checkpointed`, not busy)
//!    follow with `TRUNCATE`, which then holds the writer lock only briefly.
//!    Otherwise the round is `partial`/`busy` and waits for the next window.
//!    `busy_timeout = 0`: a pinned reader is an answer, never a wait or error.
//!
//! Results go to `tracing` only (counters + WAL size before/after).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use sqlx::sqlite::SqliteConnection;

use crate::{KvStore, Result};

/// Tunables; [`Default`] is the production setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalMaintenanceConfig {
    /// No memory write for this long before a checkpoint is attempted.
    pub idle_after: Duration,
    /// Minimum spacing between two SUCCESSFUL checkpoints.
    pub min_interval: Duration,
    /// How often the runner calls [`WalMaintainer::tick`].
    pub poll_interval: Duration,
}

impl Default for WalMaintenanceConfig {
    fn default() -> Self {
        Self {
            idle_after: Duration::from_secs(60),
            min_interval: Duration::from_secs(300),
            poll_interval: Duration::from_secs(5),
        }
    }
}

/// Why a tick did nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// A write was seen less than `idle_after` ago.
    Active,
    /// The last successful checkpoint is younger than `min_interval`.
    MinInterval,
    /// A busy attempt backs off one idle window.
    Backoff,
    /// The WAL file is absent or empty: nothing to truncate.
    Clean,
}

/// What one tick did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    Skipped(SkipReason),
    /// Partial: PASSIVE could not backfill everything (a reader pins older
    /// frames); TRUNCATE was NOT issued.
    Partial {
        wal_before: u64,
    },
    /// A lock was unavailable; retried after the next idle window.
    Busy {
        wal_before: u64,
    },
    Done {
        wal_before: u64,
        wal_after: u64,
    },
}

/// The idle checkpoint worker for one file-backed [`KvStore`].
pub struct WalMaintainer {
    conn: SqliteConnection,
    wal_path: PathBuf,
    cfg: WalMaintenanceConfig,
    last_version: i64,
    last_write: Instant,
    last_ok: Option<Instant>,
    retry_not_before: Option<Instant>,
    busy_total: u64,
    ok_total: u64,
    partial_total: u64,
    truncate_attempts: u64,
}

impl KvStore {
    /// A WAL maintainer over this store's database file, with `now` as the
    /// start of the first idle window. `None` for [`KvStore::open_memory`]
    /// (no file, no WAL).
    pub async fn wal_maintainer(
        &self,
        cfg: WalMaintenanceConfig,
        now: Instant,
    ) -> Result<Option<WalMaintainer>> {
        // Decide file-vs-memory on a POOLED connection and only then detach:
        // an in-memory pool's single connection IS the database, and
        // detaching it would destroy the store for everyone else.
        let mut pooled = self.pool.acquire().await?;
        let file: Option<String> =
            sqlx::query_scalar("SELECT file FROM pragma_database_list WHERE name = 'main'")
                .fetch_optional(&mut *pooled)
                .await?;
        let Some(file) = file.filter(|f| !f.is_empty()) else {
            return Ok(None);
        };
        let mut conn = pooled.detach();
        // Never wait on a reader: busy is an answer, not a stall.
        sqlx::query("PRAGMA busy_timeout = 0")
            .execute(&mut conn)
            .await?;
        let last_version = data_version(&mut conn).await?;
        let mut wal = file.clone();
        wal.push_str("-wal");
        Ok(Some(WalMaintainer {
            conn,
            wal_path: PathBuf::from(wal),
            cfg,
            last_version,
            last_write: now,
            last_ok: None,
            retry_not_before: None,
            busy_total: 0,
            ok_total: 0,
            partial_total: 0,
            truncate_attempts: 0,
        }))
    }
}

async fn data_version(conn: &mut SqliteConnection) -> Result<i64> {
    Ok(sqlx::query_scalar("PRAGMA data_version")
        .fetch_one(conn)
        .await?)
}

impl WalMaintainer {
    pub fn config(&self) -> WalMaintenanceConfig {
        self.cfg
    }

    pub fn busy_total(&self) -> u64 {
        self.busy_total
    }

    pub fn ok_total(&self) -> u64 {
        self.ok_total
    }

    pub fn partial_total(&self) -> u64 {
        self.partial_total
    }

    /// How many times TRUNCATE was actually issued (PASSIVE excluded).
    pub fn truncate_attempts(&self) -> u64 {
        self.truncate_attempts
    }

    fn wal_bytes(&self) -> u64 {
        std::fs::metadata(&self.wal_path).map_or(0, |m| m.len())
    }

    fn back_off(&mut self, now: Instant, wal_before: u64, busy: bool) -> TickOutcome {
        self.retry_not_before = Some(now + self.cfg.idle_after);
        if busy {
            self.busy_total += 1;
        } else {
            self.partial_total += 1;
        }
        tracing::info!(
            target: "agent24_memory::wal",
            wal_before,
            busy_total = self.busy_total,
            partial_total = self.partial_total,
            ok_total = self.ok_total,
            "memory.db wal checkpoint {}; retrying after the next idle window",
            if busy { "busy" } else { "partial" }
        );
        if busy {
            TickOutcome::Busy { wal_before }
        } else {
            TickOutcome::Partial { wal_before }
        }
    }

    /// Observe writes, decide, and (when due) checkpoint. `now` is injected so
    /// the policy is testable without sleeping.
    pub async fn tick(&mut self, now: Instant) -> Result<TickOutcome> {
        let version = data_version(&mut self.conn).await?;
        if version != self.last_version {
            self.last_version = version;
            self.last_write = now;
        }
        if now.saturating_duration_since(self.last_write) < self.cfg.idle_after {
            return Ok(TickOutcome::Skipped(SkipReason::Active));
        }
        if let Some(ok) = self.last_ok
            && now.saturating_duration_since(ok) < self.cfg.min_interval
        {
            return Ok(TickOutcome::Skipped(SkipReason::MinInterval));
        }
        if self.retry_not_before.is_some_and(|t| now < t) {
            return Ok(TickOutcome::Skipped(SkipReason::Backoff));
        }
        let wal_before = self.wal_bytes();
        if wal_before == 0 {
            return Ok(TickOutcome::Skipped(SkipReason::Clean));
        }
        // Step 1: PASSIVE does the heavy backfill WITHOUT the writer lock, so
        // writers are not stalled however large the WAL has grown. Row is
        // (busy, log frames, checkpointed frames).
        let (busy, log, done): (i64, i64, i64) = sqlx::query_as("PRAGMA wal_checkpoint(PASSIVE)")
            .fetch_one(&mut self.conn)
            .await?;
        if busy != 0 {
            return Ok(self.back_off(now, wal_before, true));
        }
        // Step 2: only a fully backfilled WAL is worth truncating — then the
        // TRUNCATE holds the writer lock for next to no backfill work.
        if log != done {
            return Ok(self.back_off(now, wal_before, false));
        }
        self.truncate_attempts += 1;
        let (busy, _log, _done): (i64, i64, i64) =
            sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
                .fetch_one(&mut self.conn)
                .await?;
        if busy != 0 {
            return Ok(self.back_off(now, wal_before, true));
        }
        let wal_after = self.wal_bytes();
        self.ok_total += 1;
        self.last_ok = Some(now);
        self.retry_not_before = None;
        tracing::info!(
            target: "agent24_memory::wal",
            wal_before,
            wal_after,
            busy_total = self.busy_total,
            ok_total = self.ok_total,
            "memory.db wal checkpoint (truncate) done"
        );
        Ok(TickOutcome::Done {
            wal_before,
            wal_after,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    const IDLE: Duration = Duration::from_secs(60);
    const MIN: Duration = Duration::from_secs(300);

    fn cfg() -> WalMaintenanceConfig {
        WalMaintenanceConfig {
            idle_after: IDLE,
            min_interval: MIN,
            poll_interval: Duration::from_secs(5),
        }
    }

    async fn store(dir: &tempfile::TempDir) -> KvStore {
        KvStore::open(&dir.path().join("memory.db")).await.unwrap()
    }

    /// Write a batch through the pool (grows the WAL; autocheckpoint is
    /// disabled by a pinned reader in the callers that need a big WAL).
    async fn write_batch(kv: &KvStore, tag: &str, n: usize) {
        for i in 0..n {
            kv.put("wal-test", &format!("{tag}-{i}"), &vec![i; 64])
                .await
                .unwrap();
        }
    }

    fn wal_len(dir: &tempfile::TempDir) -> u64 {
        std::fs::metadata(dir.path().join("memory.db-wal")).map_or(0, |m| m.len())
    }

    #[tokio::test]
    async fn in_memory_store_has_no_maintainer() {
        let kv = KvStore::open_memory().await.unwrap();
        assert!(
            kv.wal_maintainer(cfg(), Instant::now())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn idle_truncates_the_wal_and_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let kv = store(&dir).await;
        let t0 = Instant::now();
        let mut m = kv.wal_maintainer(cfg(), t0).await.unwrap().unwrap();
        write_batch(&kv, "a", 200).await;
        assert!(wal_len(&dir) > 0, "fixture must leave a WAL behind");

        // First tick sees the writes: active, no checkpoint.
        assert_eq!(
            m.tick(t0 + Duration::from_secs(1)).await.unwrap(),
            TickOutcome::Skipped(SkipReason::Active)
        );
        // 60s without writes -> truncate.
        let out = m.tick(t0 + Duration::from_secs(62)).await.unwrap();
        let TickOutcome::Done {
            wal_before,
            wal_after,
        } = out
        else {
            panic!("expected Done, got {out:?}");
        };
        assert!(wal_before > 0);
        assert_eq!(wal_after, 0);
        assert_eq!(wal_len(&dir), 0);
        assert_eq!(m.ok_total(), 1);
        // Data intact.
        let v: Option<Vec<usize>> = kv.fetch("wal-test", "a-199").await.unwrap();
        assert_eq!(v.unwrap().len(), 64);
    }

    #[tokio::test]
    async fn continuous_writes_never_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let kv = store(&dir).await;
        let t0 = Instant::now();
        let mut m = kv.wal_maintainer(cfg(), t0).await.unwrap().unwrap();
        // A write between every pair of ticks, for 10 minutes of fake time.
        for step in 1..=120u64 {
            write_batch(&kv, &format!("s{step}"), 1).await;
            let out = m.tick(t0 + Duration::from_secs(step * 5)).await.unwrap();
            assert_eq!(out, TickOutcome::Skipped(SkipReason::Active), "step {step}");
        }
        assert_eq!(m.ok_total(), 0);
    }

    #[tokio::test]
    async fn own_checkpoint_does_not_look_like_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let kv = store(&dir).await;
        let t0 = Instant::now();
        let mut m = kv.wal_maintainer(cfg(), t0).await.unwrap().unwrap();
        write_batch(&kv, "a", 50).await;
        m.tick(t0 + Duration::from_secs(1)).await.unwrap();
        assert!(matches!(
            m.tick(t0 + Duration::from_secs(70)).await.unwrap(),
            TickOutcome::Done { .. }
        ));
        // No further write: the next tick must be Clean/MinInterval, not Active.
        assert_ne!(
            m.tick(t0 + Duration::from_secs(75)).await.unwrap(),
            TickOutcome::Skipped(SkipReason::Active)
        );
    }

    #[tokio::test]
    async fn min_interval_spaces_successful_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let kv = store(&dir).await;
        let t0 = Instant::now();
        let mut m = kv.wal_maintainer(cfg(), t0).await.unwrap().unwrap();
        write_batch(&kv, "a", 50).await;
        m.tick(t0 + Duration::from_secs(1)).await.unwrap();
        assert!(matches!(
            m.tick(t0 + Duration::from_secs(70)).await.unwrap(),
            TickOutcome::Done { .. }
        ));
        // New writes, then idle again well before 300s from the success.
        write_batch(&kv, "b", 50).await;
        m.tick(t0 + Duration::from_secs(80)).await.unwrap();
        assert_eq!(
            m.tick(t0 + Duration::from_secs(150)).await.unwrap(),
            TickOutcome::Skipped(SkipReason::MinInterval)
        );
        assert_eq!(m.ok_total(), 1);
        // Past the floor it runs again.
        assert!(matches!(
            m.tick(t0 + Duration::from_secs(70 + 301)).await.unwrap(),
            TickOutcome::Done { .. }
        ));
        assert_eq!(m.ok_total(), 2);
    }

    #[tokio::test]
    async fn busy_reader_is_not_an_error_and_next_window_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let kv = store(&dir).await;
        let t0 = Instant::now();
        let mut m = kv.wal_maintainer(cfg(), t0).await.unwrap().unwrap();

        // A reader that keeps a snapshot open, pinning WAL frames; a write
        // afterwards puts frames the checkpoint cannot recycle behind it.
        let mut reader = kv.pool.acquire().await.unwrap();
        sqlx::query("BEGIN").execute(&mut *reader).await.unwrap();
        let _: i64 = sqlx::query_scalar("SELECT count(*) FROM kv")
            .fetch_one(&mut *reader)
            .await
            .unwrap();
        write_batch(&kv, "a", 100).await;

        m.tick(t0 + Duration::from_secs(1)).await.unwrap();
        let out = m.tick(t0 + Duration::from_secs(70)).await.unwrap();
        // The reader pins older frames: PASSIVE cannot backfill everything,
        // so TRUNCATE must not even be issued.
        assert!(matches!(out, TickOutcome::Partial { .. }), "got {out:?}");
        assert_eq!(m.partial_total(), 1);
        assert_eq!(
            m.truncate_attempts(),
            0,
            "TRUNCATE issued on a partial pass"
        );
        assert!(wal_len(&dir) > 0);

        // Same idle window: backed off, no bombardment.
        assert_eq!(
            m.tick(t0 + Duration::from_secs(75)).await.unwrap(),
            TickOutcome::Skipped(SkipReason::Backoff)
        );
        assert_eq!(m.partial_total(), 1);

        // Reader goes away; the next idle window succeeds.
        sqlx::query("ROLLBACK").execute(&mut *reader).await.unwrap();
        drop(reader);
        let out = m.tick(t0 + Duration::from_secs(135)).await.unwrap();
        assert!(
            matches!(out, TickOutcome::Done { wal_after: 0, .. }),
            "got {out:?}"
        );
        assert_eq!(m.ok_total(), 1);
        assert_eq!(wal_len(&dir), 0);
    }

    #[tokio::test]
    async fn clean_wal_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let kv = store(&dir).await;
        let t0 = Instant::now();
        let mut m = kv.wal_maintainer(cfg(), t0).await.unwrap().unwrap();
        write_batch(&kv, "a", 5).await;
        m.tick(t0 + Duration::from_secs(1)).await.unwrap();
        m.tick(t0 + Duration::from_secs(70)).await.unwrap();
        assert_eq!(
            m.tick(t0 + Duration::from_secs(400)).await.unwrap(),
            TickOutcome::Skipped(SkipReason::Clean)
        );
    }

    use crate::event::{EventStore, MemEvent, Origin, Scope, Trust};

    fn event(id: &str) -> MemEvent {
        MemEvent::new(
            id,
            Scope::owner("u"),
            "msg",
            serde_json::json!({"k": id}),
            Origin {
                source: "test".to_owned(),
                trust: Trust::UserSaid,
            },
        )
    }

    async fn event_roundtrip(kv: &KvStore, id: &str) {
        kv.events().append(&event(id)).await.unwrap();
        assert!(
            kv.events()
                .get("u", &id.to_owned())
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn ephemeral_store_survives_maintainer_probe() {
        let kv = KvStore::open_memory().await.unwrap();
        event_roundtrip(&kv, "before").await;
        assert!(
            kv.wal_maintainer(cfg(), Instant::now())
                .await
                .unwrap()
                .is_none()
        );
        // The single connection IS the database; it must still be there.
        event_roundtrip(&kv, "after").await;
    }

    #[tokio::test]
    async fn detaching_does_not_disturb_the_file_pool() {
        let dir = tempfile::tempdir().unwrap();
        let kv = store(&dir).await;
        event_roundtrip(&kv, "before").await;
        let _m = kv
            .wal_maintainer(cfg(), Instant::now())
            .await
            .unwrap()
            .unwrap();
        for i in 0..20 {
            event_roundtrip(&kv, &format!("after-{i}")).await;
        }
    }

    #[tokio::test]
    async fn writers_are_not_stalled_while_a_big_wal_is_checkpointed() {
        let dir = tempfile::tempdir().unwrap();
        let kv = store(&dir).await;
        let t0 = Instant::now();
        let mut m = kv.wal_maintainer(cfg(), t0).await.unwrap().unwrap();
        // Pin a reader so autocheckpoint cannot keep the WAL small, build ~10 MiB.
        let mut reader = kv.pool.acquire().await.unwrap();
        sqlx::query("BEGIN").execute(&mut *reader).await.unwrap();
        let _: i64 = sqlx::query_scalar("SELECT count(*) FROM kv")
            .fetch_one(&mut *reader)
            .await
            .unwrap();
        let blob = "x".repeat(4096);
        for i in 0..2500 {
            kv.put("big", &format!("k{i}"), &blob).await.unwrap();
        }
        assert!(wal_len(&dir) > 8 * 1024 * 1024);
        m.tick(t0 + Duration::from_secs(1)).await.unwrap();
        sqlx::query("ROLLBACK").execute(&mut *reader).await.unwrap();
        drop(reader);

        let writer = kv.clone();
        let w = tokio::spawn(async move {
            let mut worst = Duration::ZERO;
            for i in 0..50 {
                let t = Instant::now();
                writer.put("conc", &format!("w{i}"), &i).await.unwrap();
                worst = worst.max(t.elapsed());
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            worst
        });
        let out = m.tick(t0 + Duration::from_secs(70)).await.unwrap();
        let worst = w.await.unwrap();
        assert!(matches!(out, TickOutcome::Done { .. }), "got {out:?}");
        assert!(
            worst < Duration::from_secs(1),
            "a write stalled for {worst:?}"
        );
    }
}
