//! P1-02c-0 capacity MEASUREMENT (design D9). Not a correctness test and not
//! part of default CI: every case is `#[ignore]`. Run with
//! `cargo test -p agent24-agent --test p1_02c0_capacity_measure -- --ignored --nocapture --test-threads=1`.
//!
//! Drives the real personal write path: `RunManager::chat_remember_turn` ->
//! `SessionMemory::remember` -> `SessionLog::append_turn` (+ compaction, + the
//! explicit-remember `retain::persist` -> `WriteGate`). Only synthetic data in a
//! temp dir; no user library is opened. Default quotas are never changed.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use agent24_agent::{EventSink, MemoryWriteOutcome, RunManager, SessionMemory};
use agent24_memory::KvStore;
use agent24_memory::session::Summarizer;
use agent24_models::Msg;
use agent24_models::router::ModelRouter;
use agent24_protocol::EventBody;
use agent24_store::Store;
use agent24_tools::ToolRegistry;
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const OWNER: &str = "p1-02c0-owner";
const TURNS: usize = 3000;
const SESSION_TURNS: usize = 100;
/// One in this many turns is an explicit "记住/remember" turn.
const REMEMBER_EVERY: usize = 20;
const SUMMARY_CHARS: usize = 800;

struct FixedSummary;
#[async_trait]
impl Summarizer for FixedSummary {
    async fn summarize(&self, _: Option<&str>, _: &[Msg]) -> Result<String, String> {
        Ok(synth(&mut Rng(7), SUMMARY_CHARS))
    }
}
struct Sink;
impl EventSink for Sink {
    fn emit(&self, _: EventBody) {}
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + (self.next() as usize) % (hi - lo + 1)
    }
}

/// `n` chars: ~35% CJK (3 bytes UTF-8), ~65% ASCII letters/spaces.
fn synth(rng: &mut Rng, n: usize) -> String {
    const CJK: &[char] = &[
        '我', '你', '今', '天', '记', '住', '项', '目', '会', '议', '明', '早', '九', '点', '喜',
        '欢', '咖', '啡', '计', '划', '进', '度', '代', '码', '测', '试', '文', '档', '设', '计',
        '用', '户',
    ];
    const ASCII: &[u8] = b"abcdefghijklmnopqrstuvwxyz      ";
    (0..n)
        .map(|_| {
            if rng.range(0, 99) < 35 {
                CJK[rng.range(0, CJK.len() - 1)]
            } else {
                ASCII[rng.range(0, ASCII.len() - 1)] as char
            }
        })
        .collect()
}

struct Tier {
    name: &'static str,
    user: (usize, usize),
    assistant: (usize, usize),
}

fn file_len(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

async fn run_tier(tier: Tier) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p1-02c0.sqlite");
    let wal = dir.path().join("p1-02c0.sqlite-wal");
    let kv = KvStore::open(&path).await.unwrap();
    let m = RunManager::with_memory(
        Store::open_memory().await.unwrap(),
        Arc::new(ModelRouter::with_defaults(vec![])),
        Arc::new(ToolRegistry::new()),
        Arc::new(Sink),
        CancellationToken::new(),
        Some(SessionMemory::new(kv, Arc::new(FixedSummary)).with_owner(OWNER.into())),
    );
    let admin = sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    let mut rng = Rng(42);
    let mut wal_peak = 0u64;
    let mut snaps: Vec<(i64, i64, u64, u64)> = vec![(0, 0, 0, 0)];
    let started = std::time::Instant::now();
    for t in 0..TURNS {
        let sid = format!("s{}", t / SESSION_TURNS);
        let ulen = rng.range(tier.user.0, tier.user.1);
        let alen = rng.range(tier.assistant.0, tier.assistant.1);
        let mut prompt = synth(&mut rng, ulen);
        let remembers = t % REMEMBER_EVERY == 0;
        if remembers {
            prompt = format!("记住 {prompt}");
        }
        let answer = synth(&mut rng, alen);
        let outcome = m.chat_remember_turn(&sid, &prompt, &answer).await;
        let want = if remembers {
            MemoryWriteOutcome::Saved
        } else {
            MemoryWriteOutcome::NotApplicable
        };
        // A failed or skipped write would silently under-count capacity.
        assert_eq!(outcome, want, "turn {t}: unexpected write outcome");
        wal_peak = wal_peak.max(file_len(&wal));
        if (t + 1) % 1000 == 0 {
            let (rows, bytes): (i64, i64) =
                sqlx::query_as("SELECT row_count, byte_count FROM mem_owner_usage WHERE owner=?")
                    .bind(OWNER)
                    .fetch_one(&admin)
                    .await
                    .unwrap();
            let (pages, ps): (i64, i64) = sqlx::query_as(
                "SELECT page_count, page_size FROM pragma_page_count(), pragma_page_size()",
            )
            .fetch_one(&admin)
            .await
            .unwrap();
            snaps.push((rows, bytes, (pages * ps) as u64, wal_peak));
        }
    }
    let secs = started.elapsed().as_secs_f64();
    let wal_before = file_len(&wal);
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&admin)
        .await
        .unwrap();
    let wal_after = file_len(&wal);
    let main_len = file_len(&path);

    // Payload distribution, same byte definition as the quota.
    let mut sizes: Vec<i64> = sqlx::query_scalar(
        "SELECT LENGTH(CAST(payload AS BLOB)) FROM mem_events WHERE scope_owner=? ORDER BY 1",
    )
    .bind(OWNER)
    .fetch_all(&admin)
    .await
    .unwrap();
    sizes.sort_unstable();
    let n = sizes.len();
    let avg = sizes.iter().sum::<i64>() as f64 / n as f64;
    let p95 = sizes[(n * 95 / 100).min(n - 1)];
    let by_kind: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT kind, COUNT(*), SUM(LENGTH(CAST(payload AS BLOB))) FROM mem_events WHERE scope_owner=? GROUP BY kind ORDER BY 2 DESC",
    )
    .bind(OWNER)
    .fetch_all(&admin)
    .await
    .unwrap();

    println!("MEASURE tier={} turns={TURNS} secs={secs:.1}", tier.name);
    for (i, w) in snaps.windows(2).enumerate() {
        println!(
            "MEASURE tier={} block={} rows={} quota_bytes={} main_pages_bytes_end={} wal_peak_so_far={}",
            tier.name,
            i + 1,
            w[1].0 - w[0].0,
            w[1].1 - w[0].1,
            w[1].2,
            w[1].3
        );
    }
    let last = snaps[snaps.len() - 1];
    println!(
        "MEASURE tier={} total_rows={} total_quota_bytes={} main_pages_bytes={} main_file_len={main_len} wal_peak={wal_peak} wal_before_ckpt={wal_before} wal_after_truncate={wal_after}",
        tier.name, last.0, last.1, last.2
    );
    println!(
        "MEASURE tier={} payload_rows={n} payload_avg={avg:.1} payload_p95={p95} payload_max={}",
        tier.name,
        sizes[n - 1]
    );
    let by_kind_counts: Vec<(String, i64)> =
        by_kind.iter().map(|(k, c, _)| (k.clone(), *c)).collect();
    for (k, c, b) in by_kind {
        println!("MEASURE tier={} kind={k} rows={c} bytes={b}", tier.name);
    }
    // Expected rows from the synthetic shape: 2 messages per turn, one
    // write_decision per explicit remember, one summary per compaction
    // (simulated from the default policy: fold to 20 when the tail exceeds 40).
    let (mut want_summaries, mut tail) = (0usize, 0usize);
    for t in 0..TURNS {
        if t % SESSION_TURNS == 0 {
            tail = 0;
        }
        tail += 2;
        if tail > 40 {
            want_summaries += 1;
            tail = 20;
        }
    }
    let count = |k: &str| {
        by_kind_counts
            .iter()
            .find(|(kind, _)| kind == k)
            .map_or(0, |(_, c)| *c as usize)
    };
    let want_decisions = TURNS.div_ceil(REMEMBER_EVERY);
    assert_eq!(count("message"), 2 * TURNS);
    assert_eq!(count("session.summary"), want_summaries);
    assert_eq!(count("mem.write_decision"), want_decisions);
    assert_eq!(n, 2 * TURNS + want_summaries + want_decisions);
    assert_eq!(
        last.0 as usize, n,
        "quota row_count must equal mem_events rows"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "P1-02c-0 measurement; run explicitly with --ignored"]
async fn measure_short_conversation() {
    run_tier(Tier {
        name: "short",
        user: (20, 80),
        assistant: (100, 400),
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "P1-02c-0 measurement; run explicitly with --ignored"]
async fn measure_long_conversation() {
    run_tier(Tier {
        name: "long",
        user: (200, 1500),
        assistant: (1500, 6000),
    })
    .await;
}
