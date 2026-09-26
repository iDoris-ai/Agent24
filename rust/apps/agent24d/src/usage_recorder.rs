//! ME4-4.2.3b — the model-usage writer. See
//! `docs/design/ME4-S2-model-callback.md`:
//! - §6.3 — `UsageRecorder`/`stop_usage_writer`: a single background task
//!   turns `UsageSink::record` calls (synchronous, from `model_callback.rs`,
//!   possibly from a `Drop`) into `agent24-store` upserts, off the calling
//!   task, so no handler ever awaits a disk write.
//! - §6.2 — the counting table this module's `record_of` implements: which
//!   `UsageOutcome` variant becomes which `(ServedBy, ModelUsageDelta)`.
//! - J11 (writer half) / J19 — this file's tests (`usage_by_module_*`, so
//!   `cargo test -p agent24d usage_by_module` finds them — review H2).
//!
//! **Two ways this task ends**, both by design (§6.3):
//! - **Normal**: every [`UsageSink`]-holding sender is dropped (the
//!   `ModelCallbackDeps` `serve` built and every `ModelGrant` a mounted
//!   module's `MethodsFor` closure holds) — the channel closes, whatever was
//!   already queued is written, then the task returns.
//! - **Hard stop**: `hard_stop` resolves first (production: the shutdown
//!   token cancelled, then `Shutdown::deadlines().modules` — cut-off plus
//!   `CONFIRM`, §3.3/§6.3). Nothing new is written after that instant:
//!   whatever is still queued is dropped and counted — into the SAME
//!   `dropped()` counter a full channel counts into (review, M2: from the
//!   outside, "never made it to the store" is one fact, not two).
//!   This never extends a shutdown past `deadlines().modules`, which already
//!   sits ahead of `persist`/`watchdog` (`lifecycle.rs`).
//!
//! `stop_usage_writer` is `serve`'s half of the contract: called from the
//! shutdown sequence, AFTER the out-of-process supervisors have stopped (so
//! every in-flight call's outcome — including the ones the cut-off itself
//! cancelled — has already reached this task's channel), it waits for the
//! writer up to the same `deadlines().modules` instant `hard_stop` uses, and
//! logs the final `dropped()` count as the shutdown's one summary line for
//! this subsystem (review, M2). Not waiting for the writer (J19's whole
//! point) is exactly the bug this exists to prevent: a record that landed in
//! the channel but never reached the store before the process exits.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use agent24_store::{ModelUsageDelta, ServedBy, Store};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::model_callback::{Served, UsageOutcome, UsageSink};

/// §6.3's "⚖️": bounded so a burst of calls cannot grow this task's queue
/// without limit; a full channel drops the newest record rather than
/// blocking the caller (the sink's synchronous, never-await contract — it
/// may run from a `Drop`).
const CHANNEL_CAPACITY: usize = 1024;

/// L3 (review): the wall clock `record_of` reads for `day`, injectable so a
/// test can pin an exact instant instead of depending on wall-clock time and
/// the host's timezone (the earlier wall-clock test could only catch a
/// `chrono::Local` regression when the host's local calendar day happened to
/// differ from UTC's at the moment the test ran). Production (`spawn`/
/// `spawn_with_write_delay`) always wires in [`SystemUtcClock`], a one-line
/// pass-through to `chrono::Utc::now()` — there is no longer anywhere for a
/// Local-vs-UTC mixup to hide other than that one line.
pub trait UtcClock: Send + Sync {
    fn now(&self) -> chrono::DateTime<chrono::Utc>;
}

/// The production clock.
#[derive(Debug, Default)]
pub struct SystemUtcClock;

impl UtcClock for SystemUtcClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}

fn served_by_of(served: Served) -> ServedBy {
    match served {
        Served::Local => ServedBy::Local,
        Served::Remote => ServedBy::Remote,
    }
}

/// One dequeued record: everything [`Store::record_module_model_usage`]
/// needs, already decided at `record`-time.
struct UsageRecord {
    module: String,
    day: String,
    served_by: ServedBy,
    delta: ModelUsageDelta,
}

/// §6.2, turned into one row's contribution. `day` is the UTC calendar day
/// of `now` — measured at `record`-time (not when the writer later dequeues
/// and stores it): two recorders (or one recorder that happens to straddle
/// midnight) must still land in the bucket the call actually happened in, in
/// the one timezone every reader of the table shares.
fn record_of(
    module: &str,
    outcome: UsageOutcome,
    now: chrono::DateTime<chrono::Utc>,
) -> UsageRecord {
    let day = now.date_naive().format("%Y-%m-%d").to_string();
    let (served_by, delta) = match outcome {
        UsageOutcome::Ok {
            served,
            prompt_tokens,
            completion_tokens,
        } => (
            served_by_of(served),
            ModelUsageDelta {
                calls_ok: 1,
                prompt_tokens,
                completion_tokens,
                ..Default::default()
            },
        ),
        // §6.2 row 2: the provider answered but the kernel refused to pass
        // it on — tokens were spent, so they are still recorded, on the tier
        // that actually served the call (not `none`).
        UsageOutcome::FailedAfterServe {
            served,
            prompt_tokens,
            completion_tokens,
        } => (
            served_by_of(served),
            ModelUsageDelta {
                calls_failed: 1,
                prompt_tokens,
                completion_tokens,
                ..Default::default()
            },
        ),
        // §6.2 row 3: never reached (or was refused by) a provider — the
        // `none` tier, no tokens (nothing to guess at).
        UsageOutcome::Failed => (
            ServedBy::None,
            ModelUsageDelta {
                calls_failed: 1,
                ..Default::default()
            },
        ),
        // §6.2 row 4.
        UsageOutcome::Cancelled => (
            ServedBy::None,
            ModelUsageDelta {
                calls_cancelled: 1,
                ..Default::default()
            },
        ),
    };
    UsageRecord {
        module: module.to_owned(),
        day,
        served_by,
        delta,
    }
}

/// §6.3: the single background writer. Cheap to hold — `Arc<Self>` is what
/// every `ModelCallbackDeps`/`ModelGrant` clones as its `Arc<dyn UsageSink>`.
pub struct UsageRecorder {
    tx: mpsc::Sender<UsageRecord>,
    /// Shared with the writer task (`run_writer`) via `Arc`, not merely
    /// owned here: a hard stop's lost records must land in the SAME counter
    /// a full channel counts into (review, M2), and only the writer task's
    /// own end-of-life code can see the former.
    dropped: Arc<AtomicU64>,
    clock: Arc<dyn UtcClock>,
}

impl UsageRecorder {
    /// Production entry point: no artificial delay before a dequeued record
    /// is written, and the real wall clock ([`SystemUtcClock`]).
    #[must_use]
    pub fn spawn(
        store: Store,
        hard_stop: impl Future<Output = ()> + Send + 'static,
    ) -> (Arc<Self>, JoinHandle<()>) {
        Self::spawn_with_write_delay(store, hard_stop, Duration::ZERO)
    }

    /// The test seam (§6.3): `write_delay` is awaited, per record, right
    /// before the store call — long enough to make "the record is queued but
    /// not yet in the store" an observable window (J19's
    /// `usage_by_module_waiting_for_the_writer_is_what_lands_the_record`;
    /// §6.3's own doc comment on this method calls it out by name).
    /// Production always calls [`Self::spawn`], i.e. `write_delay ==
    /// Duration::ZERO`, and still the real wall clock.
    #[must_use]
    pub fn spawn_with_write_delay(
        store: Store,
        hard_stop: impl Future<Output = ()> + Send + 'static,
        write_delay: Duration,
    ) -> (Arc<Self>, JoinHandle<()>) {
        Self::build(store, hard_stop, write_delay, Arc::new(SystemUtcClock))
    }

    /// L3 (review): the same, with an injectable [`UtcClock`] — test-only.
    /// Production never calls this; it always goes through [`Self::spawn`]/
    /// [`Self::spawn_with_write_delay`], which fix the clock to
    /// [`SystemUtcClock`].
    #[cfg(test)]
    #[must_use]
    pub fn spawn_with_clock(
        store: Store,
        hard_stop: impl Future<Output = ()> + Send + 'static,
        write_delay: Duration,
        clock: Arc<dyn UtcClock>,
    ) -> (Arc<Self>, JoinHandle<()>) {
        Self::build(store, hard_stop, write_delay, clock)
    }

    fn build(
        store: Store,
        hard_stop: impl Future<Output = ()> + Send + 'static,
        write_delay: Duration,
        clock: Arc<dyn UtcClock>,
    ) -> (Arc<Self>, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let recorder = Arc::new(Self {
            tx,
            dropped: Arc::clone(&dropped),
            clock,
        });
        let handle = tokio::spawn(run_writer(store, rx, hard_stop, write_delay, dropped));
        (recorder, handle)
    }

    /// How many records never made it to the store this run: either this
    /// sink could not hand them to the writer (channel full, or closed after
    /// a hard stop), or the writer itself discarded them at its hard stop —
    /// one counter for both (review, M2: "never landed" is a single fact).
    /// Public in production (design §6.3's own signature) — `stop_usage_writer`
    /// logs it once, as the shutdown's summary line for this subsystem.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// An independent handle to the SAME dropped-count, decoupled from this
    /// particular `Arc<UsageRecorder>`'s lifetime — used by
    /// [`stop_usage_writer`] so it can drop its (possibly last) `Arc`
    /// reference to the sender BEFORE awaiting the writer's join, and still
    /// read the final count afterward. Crate-internal: nothing outside this
    /// module needs it, `dropped()` above is the public accessor.
    pub(crate) fn dropped_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.dropped)
    }
}

impl UsageSink for UsageRecorder {
    fn record(&self, module: &str, outcome: UsageOutcome) {
        // `try_send`: synchronous, never blocks — the sink contract (§6.3)
        // this may be called from a `Drop`. A full or closed channel counts
        // into `dropped` regardless.
        if self
            .tx
            .try_send(record_of(module, outcome, self.clock.now()))
            .is_err()
        {
            let already_warned = self.dropped.fetch_add(1, Ordering::Relaxed) > 0;
            // Review, M2: rate-limited — only the FIRST drop in a sustained
            // overload logs; the running total (via the public `dropped()`,
            // the same accessor `stop_usage_writer`'s final summary reads)
            // is included so the one line still says how bad it's gotten,
            // rather than logging every single one at `warn` level for as
            // long as the overload lasts.
            if !already_warned {
                tracing::warn!(
                    module,
                    total_dropped = self.dropped(),
                    "the model usage channel is full or closed; dropping usage records \
                     (further drops in this run are counted, not logged individually — \
                     see the total in the writer's final summary)"
                );
            }
        }
    }
}

/// The writer task body. Ends either when every sender has been dropped
/// (`rx.recv()` returns `None` — the channel's own buffered records are
/// still delivered up to that point, so nothing queued before the last
/// sender dropped is lost) or when `hard_stop` resolves first (queued
/// records not yet written are then dropped and counted — see the module
/// doc comment).
async fn run_writer(
    store: Store,
    mut rx: mpsc::Receiver<UsageRecord>,
    hard_stop: impl Future<Output = ()> + Send + 'static,
    write_delay: Duration,
    dropped: Arc<AtomicU64>,
) {
    tokio::pin!(hard_stop);
    loop {
        tokio::select! {
            biased;
            () = &mut hard_stop => {
                // Closing first stops new sends from succeeding, though at
                // this point in `serve`'s shutdown sequence every sender is
                // already gone or about to be — this just makes the "no more
                // writes after the hard stop" boundary exact rather than
                // racing the last `try_send`.
                rx.close();
                let mut lost: u64 = 0;
                while rx.try_recv().is_ok() {
                    lost = lost.saturating_add(1);
                }
                if lost > 0 {
                    dropped.fetch_add(lost, Ordering::Relaxed);
                    tracing::warn!(
                        lost,
                        "the model usage writer hit its hard stop; dropping queued records"
                    );
                }
                return;
            }
            received = rx.recv() => {
                let Some(rec) = received else {
                    // Every sender dropped and the queue is empty: the
                    // normal, non-hard-stop end (§6.3).
                    return;
                };
                if !write_delay.is_zero() {
                    tokio::time::sleep(write_delay).await;
                }
                if let Err(e) = store
                    .record_module_model_usage(&rec.module, &rec.day, rec.served_by, rec.delta)
                    .await
                {
                    tracing::error!(module = %rec.module, "writing model usage failed: {e}");
                }
            }
        }
    }
}

/// §6.3/v3.1 M-2: `serve`'s shutdown sequence calls this AFTER the
/// out-of-process supervisors have stopped, so it is what actually lands
/// records already queued (including any the cut-off itself produced) before
/// the process exits — the bug J19 exists to catch is skipping this call, or
/// not awaiting it.
///
/// Takes `recorder` BY VALUE (not `&UsageRecorder`) and drops it internally,
/// before awaiting `writer` — deliberately: by the time `serve` calls this
/// (after every out-of-process supervisor has stopped), every OTHER sender
/// (each mounted module's `ModelGrant`) is already gone, so `recorder`'s own
/// reference is the only thing that could still be keeping the channel open.
/// Holding onto it across the `.await` below — e.g. by taking `&UsageRecorder`
/// and having `serve` keep its own clone alive for the rest of the function —
/// would make the channel's "every sender dropped" path never fire in
/// practice, silently forcing every shutdown to wait out the full
/// `hard_stop` deadline instead of returning as soon as the writer actually
/// finishes. `deadline` is the SAME instant the recorder's own `hard_stop`
/// resolves at (`Shutdown::deadlines().modules`), so this never waits past
/// the bound the writer itself already enforces — it can only return early
/// relative to it, never later.
pub async fn stop_usage_writer(
    recorder: Arc<UsageRecorder>,
    writer: JoinHandle<()>,
    deadline: Instant,
) {
    let dropped_handle = recorder.dropped_handle();
    drop(recorder);
    if tokio::time::timeout_at(deadline, writer).await.is_err() {
        tracing::warn!("the model usage writer did not finish by the modules deadline");
    }
    // Review, M2: one summary line either way — an operator scanning logs
    // for "did this shutdown lose anything" should find an answer here
    // without having to know this counter exists.
    let dropped = dropped_handle.load(Ordering::Relaxed);
    if dropped > 0 {
        tracing::warn!(
            dropped,
            "the model usage writer stopped; {dropped} record(s) were dropped this run"
        );
    } else {
        tracing::info!("the model usage writer stopped; no records were dropped this run");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::time::Duration as StdDuration;

    async fn all_rows(store: &Store, module: &str) -> Vec<agent24_store::ModelUsageRow> {
        store
            .module_model_usage(module, "2000-01-01")
            .await
            .unwrap()
    }

    // ── J11 (writer half) — sender 全部 drop 后写完队列并退出 ────────────────

    #[tokio::test]
    async fn usage_by_module_all_senders_dropped_drains_the_queue_and_the_task_returns() {
        let store = Store::open_memory().await.unwrap();
        let (recorder, writer) = UsageRecorder::spawn(store.clone(), std::future::pending());
        recorder.record(
            "m",
            UsageOutcome::Ok {
                served: Served::Local,
                prompt_tokens: 3,
                completion_tokens: 2,
            },
        );
        // The only sender left is `recorder`'s own `tx` — dropping the Arc
        // drops it, closing the channel for the writer to notice.
        drop(recorder);
        // Normal end: no hard stop needed, so a generous join here just
        // proves the task actually returns (not merely that the row lands).
        tokio::time::timeout(StdDuration::from_secs(5), writer)
            .await
            .expect("the writer must return once every sender is dropped")
            .unwrap();
        let rows = all_rows(&store, "m").await;
        assert_eq!(rows.len(), 1, "the queued record must have been written");
        assert_eq!(rows[0].calls_ok, 1);
    }

    /// Positive control for the above: with the `Arc<UsageRecorder>` still
    /// alive (a sender still exists), only `hard_stop` can make the writer
    /// return — it does not return on its own just because the queue is
    /// momentarily empty.
    #[tokio::test]
    async fn usage_by_module_with_a_sender_still_alive_only_hard_stop_ends_the_writer() {
        let store = Store::open_memory().await.unwrap();
        let hard_stop = tokio::sync::Notify::new();
        let hard_stop = Arc::new(hard_stop);
        let fired = {
            let hard_stop = hard_stop.clone();
            async move { hard_stop.notified().await }
        };
        let (recorder, writer) = UsageRecorder::spawn(store.clone(), fired);
        recorder.record("m", UsageOutcome::Failed);
        // Give the writer ample time to have processed the one record and
        // gone back to waiting — it must NOT have returned, because
        // `recorder` (a sender) is still alive.
        tokio::time::sleep(StdDuration::from_millis(200)).await;
        assert!(
            !writer.is_finished(),
            "a live sender must keep the writer running even with an empty queue"
        );
        hard_stop.notify_one();
        tokio::time::timeout(StdDuration::from_secs(5), writer)
            .await
            .expect("hard_stop must end the writer")
            .unwrap();
        drop(recorder);
    }

    // ── §6.2 mapping — each UsageOutcome lands the row §6.2 says it should ──

    #[tokio::test]
    async fn usage_by_module_each_outcome_maps_to_its_own_table_row() {
        let store = Store::open_memory().await.unwrap();
        let (recorder, writer) = UsageRecorder::spawn(store.clone(), std::future::pending());
        recorder.record(
            "m",
            UsageOutcome::Ok {
                served: Served::Remote,
                prompt_tokens: 10,
                completion_tokens: 4,
            },
        );
        recorder.record(
            "m",
            UsageOutcome::FailedAfterServe {
                served: Served::Local,
                prompt_tokens: 7,
                completion_tokens: 0,
            },
        );
        recorder.record("m", UsageOutcome::Failed);
        recorder.record("m", UsageOutcome::Cancelled);
        drop(recorder);
        tokio::time::timeout(StdDuration::from_secs(5), writer)
            .await
            .unwrap()
            .unwrap();

        let rows = all_rows(&store, "m").await;
        let remote = rows.iter().find(|r| r.served_by == "remote").unwrap();
        assert_eq!(remote.calls_ok, 1);
        assert_eq!(remote.prompt_tokens, 10);
        assert_eq!(remote.completion_tokens, 4);

        let local = rows.iter().find(|r| r.served_by == "local").unwrap();
        assert_eq!(
            local.calls_failed, 1,
            "FailedAfterServe counts as calls_failed on the tier that served it"
        );
        assert_eq!(local.prompt_tokens, 7, "tokens were spent, so they're kept");

        let none = rows.iter().find(|r| r.served_by == "none").unwrap();
        assert_eq!(
            none.calls_failed, 1,
            "Failed (never reached a provider) goes to the none row"
        );
        assert_eq!(
            none.calls_cancelled, 1,
            "Cancelled goes to the none row too, in the same row as Failed"
        );
        assert_eq!(none.prompt_tokens, 0, "none rows never carry tokens");
    }

    // ── L3 (review) — day 从可注入时钟取，固定时刻，无跨午夜竞态/时区依赖 ────

    struct FixedClock(chrono::DateTime<chrono::Utc>);

    impl UtcClock for FixedClock {
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            self.0
        }
    }

    #[tokio::test]
    async fn usage_by_module_day_is_taken_from_the_injected_utc_clock() {
        let store = Store::open_memory().await.unwrap();
        // A fixed instant, chosen with no relation to wall-clock time or any
        // host's timezone: the point of this test is that it passes
        // identically everywhere, always, not just when the host's local
        // date happens to match UTC's at run time.
        let instant = "2026-01-01T00:30:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let clock: Arc<dyn UtcClock> = Arc::new(FixedClock(instant));
        let (recorder, writer) = UsageRecorder::spawn_with_clock(
            store.clone(),
            std::future::pending(),
            Duration::ZERO,
            clock,
        );
        recorder.record("m", UsageOutcome::Failed);
        drop(recorder);
        tokio::time::timeout(StdDuration::from_secs(5), writer)
            .await
            .unwrap()
            .unwrap();
        let rows = all_rows(&store, "m").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].day, "2026-01-01",
            "the stored day must be the injected clock's UTC calendar day"
        );

        // Positive control: a second fixed instant, 30 minutes before
        // midnight UTC — guards the day boundary itself (must NOT round up
        // to the next day).
        let instant2 = "2026-01-01T23:59:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let clock2: Arc<dyn UtcClock> = Arc::new(FixedClock(instant2));
        let (recorder2, writer2) = UsageRecorder::spawn_with_clock(
            store.clone(),
            std::future::pending(),
            Duration::ZERO,
            clock2,
        );
        recorder2.record("n", UsageOutcome::Failed);
        drop(recorder2);
        tokio::time::timeout(StdDuration::from_secs(5), writer2)
            .await
            .unwrap()
            .unwrap();
        let rows2 = all_rows(&store, "n").await;
        assert_eq!(rows2[0].day, "2026-01-01");
    }

    /// Smoke test (not the UTC-day judgement itself — see the fixed-clock
    /// test above): production `spawn`/`spawn_with_write_delay` really do
    /// wire in the real wall clock, not a stub. A day boundary crossing
    /// exactly between `before` and `after` is the only way this could ever
    /// be flaky, hence the tolerant either/or assertion.
    #[tokio::test]
    async fn usage_by_module_spawn_wires_in_the_real_utc_clock() {
        let store = Store::open_memory().await.unwrap();
        let before = chrono::Utc::now()
            .date_naive()
            .format("%Y-%m-%d")
            .to_string();
        let (recorder, writer) = UsageRecorder::spawn(store.clone(), std::future::pending());
        recorder.record("m", UsageOutcome::Failed);
        drop(recorder);
        tokio::time::timeout(StdDuration::from_secs(5), writer)
            .await
            .unwrap()
            .unwrap();
        let after = chrono::Utc::now()
            .date_naive()
            .format("%Y-%m-%d")
            .to_string();
        let rows = all_rows(&store, "m").await;
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].day == before || rows[0].day == after,
            "production spawn() must record today's real UTC day, got {}",
            rows[0].day
        );
    }

    // ── J19 (channel half) — dropped()计数 满了/关了都算 ──────────────────

    #[tokio::test]
    async fn usage_by_module_a_full_channel_is_counted_as_dropped_not_blocked() {
        let store = Store::open_memory().await.unwrap();
        let (recorder, writer) = UsageRecorder::spawn(store, std::future::pending());
        // `#[tokio::test]` defaults to a current-thread runtime and this loop
        // never `.await`s, so the spawned writer task gets no chance to run
        // (and drain the buffer) until this loop is done — the channel fills
        // to exactly its capacity, and every send past that fails to enqueue.
        for _ in 0..(CHANNEL_CAPACITY + 5) {
            recorder.record("m", UsageOutcome::Failed);
        }
        assert_eq!(
            recorder.dropped(),
            5,
            "exactly the records past the channel's capacity must be counted as dropped"
        );
        writer.abort();
    }

    // ── J19 — hard stop drops what's still queued and returns ───────────────

    #[tokio::test]
    async fn usage_by_module_a_hard_stop_drops_the_queue_and_returns_without_writing_it() {
        let store = Store::open_memory().await.unwrap();
        let hard_stop = Arc::new(tokio::sync::Notify::new());
        let fired = {
            let hard_stop = hard_stop.clone();
            async move { hard_stop.notified().await }
        };
        // A write delay long enough that nothing enqueued has been written
        // by the time hard_stop fires.
        let (recorder, writer) =
            UsageRecorder::spawn_with_write_delay(store.clone(), fired, StdDuration::from_secs(5));
        recorder.record("m", UsageOutcome::Failed);
        recorder.record("m", UsageOutcome::Failed);
        hard_stop.notify_one();
        tokio::time::timeout(StdDuration::from_secs(5), writer)
            .await
            .expect("hard_stop must end the writer promptly, not after the write delay")
            .unwrap();
        assert!(
            all_rows(&store, "m").await.is_empty(),
            "records still queued at the hard stop must be dropped, not written"
        );
        drop(recorder);
    }

    // ── M2 (review) — 硬停时丢的也计进同一个 dropped() ───────────────────────

    #[tokio::test]
    async fn usage_by_module_dropped_counts_records_lost_at_a_hard_stop_too() {
        let store = Store::open_memory().await.unwrap();
        let hard_stop = Arc::new(tokio::sync::Notify::new());
        let fired = {
            let hard_stop = hard_stop.clone();
            async move { hard_stop.notified().await }
        };
        let (recorder, writer) =
            UsageRecorder::spawn_with_write_delay(store.clone(), fired, StdDuration::from_secs(5));
        recorder.record("m", UsageOutcome::Failed);
        recorder.record("m", UsageOutcome::Failed);
        assert_eq!(
            recorder.dropped(),
            0,
            "nothing dropped yet — both records were enqueued fine, just not written"
        );
        hard_stop.notify_one();
        tokio::time::timeout(StdDuration::from_secs(5), writer)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            recorder.dropped(),
            2,
            "the two records lost at the hard stop must show up in dropped(), \
             the same counter a full channel counts into"
        );
    }

    /// `stop_usage_writer` drops its own `Arc<UsageRecorder>` reference
    /// BEFORE awaiting the join (so it can never be the reason "every sender
    /// dropped" fails to fire) — this proves that ordering doesn't also lose
    /// the hard-stop's own count: a handle obtained BEFORE the call still
    /// reads the post-join value afterward, since both point at the same
    /// underlying counter.
    #[tokio::test]
    async fn usage_by_module_stop_usage_writer_still_reports_hard_stop_losses_after_its_own_drop() {
        let store = Store::open_memory().await.unwrap();
        let hard_stop = Arc::new(tokio::sync::Notify::new());
        let fired = {
            let hard_stop = hard_stop.clone();
            async move { hard_stop.notified().await }
        };
        let (recorder, writer) =
            UsageRecorder::spawn_with_write_delay(store.clone(), fired, StdDuration::from_secs(5));
        recorder.record("m", UsageOutcome::Failed);
        recorder.record("m", UsageOutcome::Failed);
        let dropped_handle = recorder.dropped_handle();
        hard_stop.notify_one();
        stop_usage_writer(recorder, writer, Instant::now() + StdDuration::from_secs(5)).await;
        assert_eq!(
            dropped_handle.load(Ordering::Relaxed),
            2,
            "the hard-stop losses must still be visible through an independently \
             held handle after stop_usage_writer runs"
        );
    }

    // ── J19 (v3.1 M-2, behavior half) — 等写者才等到落盘 ─────────────────────

    /// The behavior test named in the design doc (§8, J19's variant 2b):
    /// records one outcome, then calls `stop_usage_writer` directly (the
    /// same shape `serve` uses — it drops the last sender internally), and
    /// shows the write is NOT yet visible immediately after recording — only
    /// after `stop_usage_writer` returns. Mutating `stop_usage_writer` to
    /// return immediately (not awaiting `writer`) must turn the SECOND
    /// assertion red every run, not just sometimes (deterministic — the
    /// write delay makes the window wide and unconditional).
    #[tokio::test]
    async fn usage_by_module_waiting_for_the_writer_is_what_lands_the_record() {
        for _ in 0..3 {
            let store = Store::open_memory().await.unwrap();
            let (recorder, writer) = UsageRecorder::spawn_with_write_delay(
                store.clone(),
                std::future::pending(),
                StdDuration::from_millis(100),
            );
            recorder.record("m", UsageOutcome::Failed);
            assert!(
                all_rows(&store, "m").await.is_empty(),
                "negative control: right after recording, the 100ms write \
                 delay means the record has NOT reached the store yet"
            );
            stop_usage_writer(recorder, writer, Instant::now() + StdDuration::from_secs(5)).await;
            assert_eq!(
                all_rows(&store, "m").await.len(),
                1,
                "after stop_usage_writer returns, the record must be in the store"
            );
        }
    }

    /// The daemon-level J19 judgement (real `serve()`, real cut-off, module
    /// deps wiring, and the store re-open after exit) is
    /// `tests/me4_model_shutdown_wiring.rs`'s
    /// `model_shutdown_wiring_lands_the_cancelled_call_in_the_store` (review,
    /// H2: renamed so `cargo test -p agent24d model_shutdown_wiring` finds
    /// it); this file only owns the recorder's half of the contract.
    #[tokio::test]
    async fn usage_by_module_stop_usage_writer_does_not_wait_past_a_hard_stop_that_already_fired() {
        let store = Store::open_memory().await.unwrap();
        // hard_stop resolves at once: the writer should end almost
        // immediately, well inside stop_usage_writer's own generous deadline.
        let (recorder, writer) = UsageRecorder::spawn(store, std::future::ready(()));
        let started = Instant::now();
        stop_usage_writer(recorder, writer, started + StdDuration::from_secs(5)).await;
        assert!(
            started.elapsed() < StdDuration::from_secs(1),
            "an already-fired hard stop must not make stop_usage_writer wait out its deadline"
        );
    }
}
