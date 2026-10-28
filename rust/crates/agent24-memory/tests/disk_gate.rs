//! P1-02c c4 (design D5, test T8, memory layer): the pre-write disk check and
//! the SQLITE_FULL mapping, on both the content (A class) and retraction paths.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use agent24_memory::artifact::{Artifact, ArtifactStore};
use agent24_memory::assertion::{Assertion, AssertionStore, Forget};
use agent24_memory::consolidator::Consolidator;
use agent24_memory::disk_gate::{
    CACHE_TTL, DiskGate, DiskProbe, DiskSample, GateClock, WriteClass,
};
use agent24_memory::event::{EventStore, MemEvent, Origin, Scope, Trust};
use agent24_memory::knowledge::{Instruction, KnowledgeBase};
use agent24_memory::trace::TaskTrace;
use agent24_memory::writer::{Candidate, MemoryWriter};
use agent24_memory::{KvStore, MemoryError, MemoryErrorCode, OsPartitionIdentity};
use agent24_models::Msg;
use serde_json::json;
use sqlx::SqlitePool;

const MIB: u64 = 1024 * 1024;
const OWNER: &str = "personal-owner-key";

#[derive(Default)]
struct FakeProbe {
    free: AtomicU64,
    wal: AtomicU64,
    calls: AtomicU64,
}

impl DiskProbe for FakeProbe {
    fn sample(&self, _db: &Path) -> io::Result<DiskSample> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(DiskSample {
            free_bytes: self.free.load(Ordering::SeqCst),
            wal_bytes: self.wal.load(Ordering::SeqCst),
        })
    }
}

#[derive(Default)]
struct FakeClock(AtomicU64);

impl GateClock for FakeClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
}

struct Fixture {
    kv: KvStore,
    admin: SqlitePool,
    probe: Arc<FakeProbe>,
    clock: Arc<FakeClock>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn set_free(&self, bytes: u64) {
        self.probe.free.store(bytes, Ordering::SeqCst);
    }
    /// Move past the cache so the next check re-reads the probe.
    fn expire_cache(&self) {
        self.clock
            .0
            .fetch_add(CACHE_TTL.as_millis() as u64 + 1, Ordering::SeqCst);
    }
    async fn count(&self, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&self.admin)
            .await
            .unwrap()
    }
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    let probe = Arc::new(FakeProbe::default());
    probe.free.store(10_000 * MIB, Ordering::SeqCst);
    let clock = Arc::new(FakeClock::default());
    let kv = KvStore::open(&path)
        .await
        .unwrap()
        .with_disk_gate(DiskGate::with_probe(&path, probe.clone(), clock.clone()));
    let admin = SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    let org = kv.ensure_org_for_user("u").await.unwrap();
    kv.record_personal_partition(OsPartitionIdentity {
        owner_key: OWNER,
        key_version: "v2",
        org_id: &org,
        space_id: "usr:u",
        user: "u",
        module: "@agent",
    })
    .await
    .unwrap();
    Fixture {
        kv,
        admin,
        probe,
        clock,
        _dir: dir,
    }
}

fn origin() -> Origin {
    Origin {
        source: "test".into(),
        trust: Trust::UserSaid,
    }
}

fn note(id: &str) -> MemEvent {
    MemEvent::new(id, Scope::owner(OWNER), "note", json!({"t": "x"}), origin())
}

async fn add_assertion(f: &Fixture, id: &str) {
    f.kv.assertions()
        .assert(&Assertion::new(
            id,
            Scope::owner(OWNER),
            "s",
            "p",
            json!(id),
            vec!["e".into()],
        ))
        .await
        .unwrap();
}

/// Fire every A-class write path; each must fail with `DiskLow`.
async fn assert_every_content_path_refused(f: &Fixture) {
    let kv = &f.kv;
    let e = kv.events().append(&note("blocked")).await.unwrap_err();
    assert_eq!(e.code(), MemoryErrorCode::DiskLow, "events.append");

    let e = kv
        .session_log()
        .append_turn(
            OWNER,
            "s1",
            1,
            &Msg::user("hi"),
            origin(),
            &Msg::assistant(Some("yo".into()), vec![]),
            origin(),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), MemoryErrorCode::DiskLow, "append_turn");

    let e = kv
        .session_log()
        .append_summary(OWNER, "s1", "sum", 1)
        .await
        .unwrap_err();
    assert_eq!(e.code(), MemoryErrorCode::DiskLow, "append_summary");

    let e = kv
        .write_gate()
        .propose(vec![
            Candidate::new(
                "c1",
                Scope::owner(OWNER),
                "user",
                "prefers",
                json!("tea"),
                origin(),
            )
            .with_evidence(vec!["ev".into()])
            .remember(),
        ])
        .await
        .unwrap_err();
    assert_eq!(e.code(), MemoryErrorCode::DiskLow, "write_gate.propose");

    let e = kv
        .assertions()
        .assert(&Assertion::new(
            "a-blocked",
            Scope::owner(OWNER),
            "s",
            "p",
            json!("o"),
            vec!["e".into()],
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code(), MemoryErrorCode::DiskLow, "assertions.assert");

    let e = kv
        .artifacts()
        .cas_write(
            Artifact::draft("n.md", "body", Scope::owner(OWNER), "agent", "t"),
            0,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), MemoryErrorCode::DiskLow, "artifacts.cas_write");
}

#[tokio::test]
async fn low_disk_refuses_every_content_write_and_adds_no_rows() {
    let f = fixture().await;
    let (events, asserts, artifacts) = (
        f.count("mem_events").await,
        f.count("mem_assertions").await,
        f.count("mem_artifacts").await,
    );
    f.set_free(100 * MIB);
    assert_every_content_path_refused(&f).await;
    assert_eq!(f.count("mem_events").await, events);
    assert_eq!(f.count("mem_assertions").await, asserts);
    assert_eq!(f.count("mem_artifacts").await, artifacts);
}

#[tokio::test]
async fn content_write_passes_at_the_floor_and_is_refused_one_byte_under() {
    let f = fixture().await;
    f.set_free(512 * MIB);
    f.kv.events().append(&note("at-floor")).await.unwrap();
    f.set_free(512 * MIB - 1);
    f.expire_cache();
    let err = f.kv.events().append(&note("under")).await.unwrap_err();
    assert!(matches!(err, MemoryError::DiskLow));
    assert_eq!(err.code().as_str(), "memory_disk_low");
}

#[tokio::test]
async fn retraction_is_exempt_down_to_16_mib() {
    let f = fixture().await;
    add_assertion(&f, "a1").await;
    add_assertion(&f, "a2").await;

    // 16 MiB <= free < 512 MiB: content refused, retraction goes through.
    f.set_free(20 * MIB);
    f.expire_cache();
    assert_eq!(
        f.kv.events().append(&note("n")).await.unwrap_err().code(),
        MemoryErrorCode::DiskLow
    );
    assert_eq!(
        f.kv.forget(OWNER, "a1", "2020-01-01T00:00:00Z")
            .await
            .unwrap(),
        Forget::Forgotten
    );

    // < 16 MiB: the retraction is refused too, and nothing is written.
    f.set_free(16 * MIB - 1);
    f.expire_cache();
    let before = f.count("mem_events").await;
    let err =
        f.kv.forget(OWNER, "a2", "2020-01-01T00:00:00Z")
            .await
            .unwrap_err();
    assert_eq!(err.code(), MemoryErrorCode::DiskLow);
    assert_eq!(f.count("mem_events").await, before);
    let open: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM mem_assertions WHERE id='a2' AND recorded_to IS NULL",
    )
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(open, 1, "the refused retraction left the belief open");
}

#[tokio::test]
async fn the_probe_is_cached_for_five_seconds_then_re_read() {
    let f = fixture().await;
    f.kv.events().append(&note("n1")).await.unwrap();
    assert_eq!(f.probe.calls.load(Ordering::SeqCst), 1);

    // The disk "fills up" but the cached sample still stands inside the TTL.
    f.set_free(MIB);
    f.clock.0.fetch_add(4_999, Ordering::SeqCst);
    f.kv.events().append(&note("n2")).await.unwrap();
    assert_eq!(f.probe.calls.load(Ordering::SeqCst), 1, "served from cache");

    // Past the TTL the probe is read again and the write is refused.
    f.clock.0.fetch_add(2, Ordering::SeqCst);
    let err = f.kv.events().append(&note("n3")).await.unwrap_err();
    assert_eq!(err.code(), MemoryErrorCode::DiskLow);
    assert_eq!(f.probe.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn the_content_floor_follows_twice_the_wal() {
    assert_eq!(
        DiskGate::threshold(WriteClass::Content, 0),
        512 * MIB,
        "small WAL: the 512 MiB floor"
    );
    assert_eq!(
        DiskGate::threshold(WriteClass::Content, 1024 * MIB),
        2048 * MIB
    );
    assert_eq!(
        DiskGate::threshold(WriteClass::Retract, 1024 * MIB),
        16 * MIB,
        "the retraction floor does not scale with the WAL"
    );

    let f = fixture().await;
    f.set_free(1500 * MIB);
    f.probe.wal.store(512 * MIB, Ordering::SeqCst); // 2 x WAL = 1024 MiB < 1500 MiB
    f.kv.events().append(&note("ok")).await.unwrap();
    f.probe.wal.store(1024 * MIB, Ordering::SeqCst); // 2 x WAL = 2048 MiB > 1500 MiB
    f.expire_cache();
    let err = f.kv.events().append(&note("big-wal")).await.unwrap_err();
    assert_eq!(err.code(), MemoryErrorCode::DiskLow);
}

#[tokio::test]
async fn the_real_probe_reads_the_database_volume_and_the_wal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    let kv = KvStore::open(&path).await.unwrap();
    kv.events().append(&note("n")).await.unwrap();
    let s = agent24_memory::disk_gate::StatvfsProbe
        .sample(&path)
        .unwrap();
    assert!(s.free_bytes > 0);
    let wal = std::fs::metadata(dir.path().join("memory.sqlite-wal"))
        .map(|m| m.len())
        .unwrap_or(0);
    assert_eq!(s.wal_bytes, wal);
}

#[tokio::test]
async fn ephemeral_stores_are_never_gated() {
    let kv = KvStore::open_memory().await.unwrap();
    // No volume: the gate is disabled, so even an absurd floor cannot trip it.
    kv.events().append(&note("mem")).await.unwrap();
}

#[tokio::test]
async fn sqlite_full_maps_to_disk_full_on_content_and_retraction() {
    let f = fixture().await;
    add_assertion(&f, "a-full").await;
    sqlx::query(
        "CREATE TRIGGER fake_full BEFORE INSERT ON mem_events
         BEGIN SELECT RAISE(ABORT, 'database or disk is full'); END",
    )
    .execute(&f.admin)
    .await
    .unwrap();

    // The gate lets both through (plenty of "free space"); SQLite then fails.
    let err = f.kv.events().append(&note("x")).await.unwrap_err();
    assert_eq!(err.code(), MemoryErrorCode::DiskFull, "content path");
    let err =
        f.kv.forget(OWNER, "a-full", "2020-01-01T00:00:00Z")
            .await
            .unwrap_err();
    assert_eq!(err.code(), MemoryErrorCode::DiskFull, "retraction path");
}

fn candidate(id: &str) -> Candidate {
    Candidate::new(
        id,
        Scope::owner(OWNER),
        "user",
        "prefers",
        json!(id),
        origin(),
    )
    .with_evidence(vec!["ev".into()])
    .remember()
}

#[tokio::test]
async fn low_disk_also_refuses_consolidator_knowledge_and_trace() {
    let f = fixture().await;
    f.set_free(100 * MIB);
    let kv = &f.kv;
    let rows = |t: &'static str| {
        let f = &f;
        async move { f.count(t).await }
    };
    let (cons, instr, trace) = (
        rows("mem_consolidations").await,
        rows("mem_instructions").await,
        rows("mem_trace_refs").await,
    );

    let e = kv.consolidator().run_once(OWNER).await.unwrap_err();
    assert_eq!(e.code(), MemoryErrorCode::DiskLow, "consolidator.run_once");

    let i = Instruction::new("i1", OWNER, "user", 1, "be brief", vec![]);
    let e = kv.knowledge().add_active(&i).await.unwrap_err();
    assert_eq!(e.code(), MemoryErrorCode::DiskLow, "knowledge.add_active");
    let e = kv.knowledge().propose(&i).await.unwrap_err();
    assert_eq!(e.code(), MemoryErrorCode::DiskLow, "knowledge.propose");
    let e = kv
        .knowledge()
        .approve("i1", OWNER, "user", 1)
        .await
        .unwrap_err();
    assert_eq!(e.code(), MemoryErrorCode::DiskLow, "knowledge.approve");

    let e = kv
        .trace()
        .record(OWNER, "run1", 1, "tool", "sym", "body")
        .await
        .unwrap_err();
    assert_eq!(e.code(), MemoryErrorCode::DiskLow, "trace.record");

    assert_eq!(rows("mem_consolidations").await, cons);
    assert_eq!(rows("mem_instructions").await, instr);
    assert_eq!(rows("mem_trace_refs").await, trace);
}

/// A probe that reports plenty of space on its first reading and none after.
struct FillsUpProbe(AtomicU64);

impl DiskProbe for FillsUpProbe {
    fn sample(&self, _db: &Path) -> io::Result<DiskSample> {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        Ok(DiskSample {
            free_bytes: if n == 0 { 10_000 * MIB } else { MIB },
            wal_bytes: 0,
        })
    }
}

/// A clock that jumps past the cache TTL on every reading, so each gate check
/// re-probes: this isolates "is the gate consulted per item" from the cache.
struct JumpingClock(AtomicU64);

impl GateClock for JumpingClock {
    fn now(&self) -> Duration {
        Duration::from_secs(self.0.fetch_add(10, Ordering::SeqCst))
    }
}

#[tokio::test]
async fn a_disk_that_fills_mid_batch_refuses_the_remaining_items() {
    let f = fixture().await;
    let path = f._dir.path().join("memory.sqlite");
    let kv = f.kv.clone().with_disk_gate(DiskGate::with_probe(
        &path,
        Arc::new(FillsUpProbe(AtomicU64::new(0))),
        Arc::new(JumpingClock(AtomicU64::new(0))),
    ));
    let before = f.count("mem_assertions").await;
    let err = kv
        .write_gate()
        .propose(vec![candidate("b1"), candidate("b2"), candidate("b3")])
        .await
        .unwrap_err();
    assert_eq!(err.code(), MemoryErrorCode::DiskLow);
    // Item 1 committed before the disk filled; items 2 and 3 were refused.
    assert_eq!(f.count("mem_assertions").await, before + 1);
}
