//! P1-02c c3 (design D3/D4, test T7): the failed-write audit trail and the
//! session gap ranges.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicU64, Ordering};

use agent24_memory::rejections::{MAX_REJECTION_ROWS, OwnerKind, Rejection};
use agent24_memory::{Clock, KvStore, MemoryErrorCode};

struct T(AtomicU64);
impl T {
    fn at(s: u64) -> Self {
        Self(AtomicU64::new(s))
    }
    fn set(&self, s: u64) {
        self.0.store(s, Ordering::SeqCst);
    }
}
impl Clock for T {
    fn now_epoch_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

const OWNER: &str = "owner\0key-a";

fn rej(owner: &str, code: MemoryErrorCode) -> Rejection<'_> {
    Rejection {
        owner_kind: OwnerKind::Module,
        owner_key: owner,
        code,
        rejected_kind: Some("note.created"),
        count_at: Some(7),
    }
}

#[tokio::test]
async fn one_rejection_is_one_row_without_the_owner_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    let kv = KvStore::open(&path).await.unwrap();
    let admin = sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    let r = kv.rejections();
    r.record_using_clock(&rej(OWNER, MemoryErrorCode::QuotaRows), &T::at(1000))
        .await
        .unwrap();
    let rows = r.list(OWNER).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].code, "memory_quota_rows");
    assert_eq!(rows[0].rejected_kind.as_deref(), Some("note.created"));
    assert_eq!(rows[0].suppressed_count, 0);
    // the raw partition key is not stored anywhere in the table
    let ids: Vec<String> = sqlx::query_scalar("SELECT owner_id FROM mem_write_rejections")
        .fetch_all(&admin)
        .await
        .unwrap();
    assert_eq!(ids.len(), 1);
    assert!(!ids[0].contains("key-a") && ids[0] != OWNER, "{ids:?}");
}

#[tokio::test]
async fn repeat_within_60s_only_bumps_suppressed_count() {
    let kv = KvStore::open_memory().await.unwrap();
    let r = kv.rejections();
    let clock = T::at(1000);
    for dt in [0, 10, 59] {
        clock.set(1000 + dt);
        r.record_using_clock(&rej(OWNER, MemoryErrorCode::QuotaRows), &clock)
            .await
            .unwrap();
    }
    let rows = r.list(OWNER).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].suppressed_count, 2);
    // a different code is not folded into it
    r.record_using_clock(&rej(OWNER, MemoryErrorCode::DiskFull), &clock)
        .await
        .unwrap();
    assert_eq!(r.list(OWNER).await.unwrap().len(), 2);
    // and after the window a new row opens
    clock.set(1000 + 61);
    r.record_using_clock(&rej(OWNER, MemoryErrorCode::QuotaRows), &clock)
        .await
        .unwrap();
    assert_eq!(r.list(OWNER).await.unwrap().len(), 3);
}

#[tokio::test]
async fn row_1001_overwrites_the_oldest_and_other_owners_are_untouched() {
    let kv = KvStore::open_memory().await.unwrap();
    let r = kv.rejections();
    let clock = T::at(0);
    r.record_using_clock(&rej("other", MemoryErrorCode::QuotaRows), &clock)
        .await
        .unwrap();
    for i in 0..=MAX_REJECTION_ROWS as u64 {
        clock.set(i * 61);
        r.record_using_clock(&rej(OWNER, MemoryErrorCode::QuotaRows), &clock)
            .await
            .unwrap();
    }
    let rows = r.list(OWNER).await.unwrap();
    assert_eq!(rows.len() as i64, MAX_REJECTION_ROWS);
    assert_eq!(
        rows.last().unwrap().at_epoch,
        61,
        "the t=0 row was overwritten"
    );
    assert_eq!(rows[0].at_epoch, MAX_REJECTION_ROWS * 61);
    assert_eq!(r.list("other").await.unwrap().len(), 1);
}

#[tokio::test]
async fn adjacent_failed_turns_merge_into_one_gap() {
    let kv = KvStore::open_memory().await.unwrap();
    let r = kv.rejections();
    let clock = T::at(10);
    for (turn, t) in [(3, 10), (4, 20), (5, 30), (9, 40)] {
        clock.set(t);
        r.record_session_gap_using_clock("s1", turn, MemoryErrorCode::QuotaBytes, &clock)
            .await
            .unwrap();
    }
    r.record_session_gap_using_clock("s2", 4, MemoryErrorCode::QuotaBytes, &clock)
        .await
        .unwrap();
    let gaps = r.session_gaps("s1").await.unwrap();
    assert_eq!(gaps.len(), 2);
    assert_eq!((gaps[0].turn_from, gaps[0].turn_to), (3, 5));
    assert_eq!((gaps[0].first_at, gaps[0].last_at), (10, 30));
    assert_eq!((gaps[1].turn_from, gaps[1].turn_to), (9, 9));
    assert_eq!(r.session_gaps("s2").await.unwrap().len(), 1);
}

#[tokio::test]
async fn gaps_stay_complete_after_the_rejection_ring_wraps() {
    let kv = KvStore::open_memory().await.unwrap();
    let r = kv.rejections();
    let clock = T::at(0);
    r.record_session_gap_using_clock("s1", 1, MemoryErrorCode::QuotaRows, &clock)
        .await
        .unwrap();
    r.record_session_gap_using_clock("s1", 2, MemoryErrorCode::QuotaRows, &clock)
        .await
        .unwrap();
    for i in 0..(MAX_REJECTION_ROWS as u64 + 50) {
        clock.set(i * 61);
        r.record_using_clock(&rej(OWNER, MemoryErrorCode::QuotaRows), &clock)
            .await
            .unwrap();
    }
    assert_eq!(
        r.list(OWNER).await.unwrap().len() as i64,
        MAX_REJECTION_ROWS
    );
    let gaps = r.session_gaps("s1").await.unwrap();
    assert_eq!(gaps.len(), 1);
    assert_eq!((gaps[0].turn_from, gaps[0].turn_to), (1, 2));
}
