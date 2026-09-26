//! ME4-4.2.3b — the model-usage writer. See
//! `docs/design/ME4-S2-model-callback.md`:
//! - §6.3 — `UsageRecorder`/`stop_usage_writer`: a single background task
//!   turns `UsageSink::record` calls (synchronous, from `model_callback.rs`,
//!   possibly from a `Drop`) into `agent24-store` upserts, off the calling
//!   task, so no handler ever awaits a disk write.
//! - §6.2 — the counting table this module's `record_of` implements: which
//!   `UsageOutcome` variant becomes which `(ServedBy, ModelUsageDelta)`.
//! - J11 (writer half) / J19 — this file's tests.
//!
//! **Two ways this task ends**, both by design (§6.3):
//! - **Normal**: every [`UsageSink`]-holding sender is dropped (the
//!   `ModelCallbackDeps` `serve` built and every `ModelGrant` a mounted
//!   module's `MethodsFor` closure holds) — the channel closes, whatever was
//!   already queued is written, then the task returns.
//! - **Hard stop**: `hard_stop` resolves first (production: the shutdown
//!   token cancelled, then `Shutdown::deadlines().modules` — cut-off plus
//!   `CONFIRM`, §3.3/§6.3). Nothing new is written after that instant:
//!   whatever is still queued is dropped and counted in one `warn!(lost)`.
//!   This never extends a shutdown past `deadlines().modules`, which already
//!   sits ahead of `persist`/`watchdog` (`lifecycle.rs`).
//!
//! `stop_usage_writer` is `serve`'s half of the contract: called from the
//! shutdown sequence, AFTER the out-of-process supervisors have stopped (so
//! every in-flight call's outcome — including the ones the cut-off itself
//! cancelled — has already reached this task's channel), it waits for the
//! writer up to the same `deadlines().modules` instant `hard_stop` uses. Not
//! waiting for it (J19's whole point) is exactly the bug this exists to
//! prevent: a record that landed in the channel but never reached the store
//! before the process exits.

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
/// **at the moment the outcome is recorded** — not when the writer later
/// dequeues and stores it — read fresh from `chrono::Utc::now()` every call:
/// two recorders (or one recorder that happens to straddle midnight) must
/// still land in the bucket the call actually happened in, in the one
/// timezone every reader of the table shares. Using the local clock instead
/// would put the same instant in a different bucket depending on the
/// daemon's `TZ` — this is deliberately never `chrono::Local`.
fn record_of(module: &str, outcome: UsageOutcome) -> UsageRecord {
    let day = chrono::Utc::now()
        .date_naive()
        .format("%Y-%m-%d")
        .to_string();
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
    dropped: AtomicU64,
}

impl UsageRecorder {
    /// Production entry point: no artificial delay before a dequeued record
    /// is written.
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
    /// `waiting_for_the_writer_is_what_lands_the_record`; §6.3's own doc
    /// comment on this method calls it out by name). Production always calls
    /// [`Self::spawn`], i.e. `write_delay == Duration::ZERO`.
    #[must_use]
    pub fn spawn_with_write_delay(
        store: Store,
        hard_stop: impl Future<Output = ()> + Send + 'static,
        write_delay: Duration,
    ) -> (Arc<Self>, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let recorder = Arc::new(Self {
            tx,
            dropped: AtomicU64::new(0),
        });
        let handle = tokio::spawn(run_writer(store, rx, hard_stop, write_delay));
        (recorder, handle)
    }

    /// How many records this sink could not hand to the writer (channel full
    /// or, after a hard stop, closed) or the writer discarded at its hard
    /// stop. Test-only accessor (mirrors `model_callback::MemoryUsageSink::
    /// take`'s gating) — the counter itself is always kept in production
    /// (`record`, below), but nothing in the production path reads it back
    /// today.
    #[cfg(test)]
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl UsageSink for UsageRecorder {
    fn record(&self, module: &str, outcome: UsageOutcome) {
        // `try_send`: synchronous, never blocks — the sink contract (§6.3)
        // this may be called from a `Drop`. A full or closed channel counts
        // and warns rather than panicking or awaiting.
        if self.tx.try_send(record_of(module, outcome)).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                module,
                "the model usage channel is full or closed; dropping a usage record"
            );
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
/// not awaiting it. `writer` is the `JoinHandle` [`UsageRecorder::spawn`]
/// returned; `deadline` is the SAME instant the recorder's own `hard_stop`
/// resolves at (`Shutdown::deadlines().modules`), so this never waits past
/// the bound the writer itself already enforces — it can only return early
/// relative to it, never later.
pub async fn stop_usage_writer(writer: JoinHandle<()>, deadline: Instant) {
    if tokio::time::timeout_at(deadline, writer).await.is_err() {
        tracing::warn!("the model usage writer did not finish by the modules deadline");
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
    async fn all_senders_dropped_drains_the_queue_and_the_task_returns() {
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
    async fn with_a_sender_still_alive_only_hard_stop_ends_the_writer() {
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
    async fn each_outcome_maps_to_its_own_table_row() {
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

    // ── day 用 UTC，不用 Local（mutation target） ───────────────────────────

    #[tokio::test]
    async fn the_day_is_todays_utc_calendar_day() {
        let store = Store::open_memory().await.unwrap();
        let (recorder, writer) = UsageRecorder::spawn(store.clone(), std::future::pending());
        recorder.record("m", UsageOutcome::Failed);
        drop(recorder);
        tokio::time::timeout(StdDuration::from_secs(5), writer)
            .await
            .unwrap()
            .unwrap();
        let want = chrono::Utc::now()
            .date_naive()
            .format("%Y-%m-%d")
            .to_string();
        let rows = all_rows(&store, "m").await;
        assert_eq!(
            rows.len(),
            1,
            "since_day filter ('2000-01-01') must include today's UTC row"
        );
        assert_eq!(
            rows[0].day, want,
            "the stored day must be today's UTC calendar day, not the local one \
             (mutating record_of to chrono::Local::now() must turn this red on \
             any host whose local date differs from UTC's right now)"
        );
    }

    // ── J19 (channel half) — dropped()计数 满了/关了都算 ──────────────────

    #[tokio::test]
    async fn a_full_channel_is_counted_as_dropped_not_blocked() {
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
    async fn a_hard_stop_drops_the_queue_and_returns_without_writing_it() {
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

    // ── J19 (v3.1 M-2, behavior half) — 等写者才等到落盘 ─────────────────────

    /// The behavior test named in the design doc (§8, J19's variant 2b):
    /// records one outcome, drops the sender (normal end, no hard stop
    /// involved), and shows the write is NOT yet visible immediately after —
    /// only after `stop_usage_writer` returns. Mutating `stop_usage_writer`
    /// to return immediately (not awaiting `writer`) must turn the SECOND
    /// assertion red every run, not just sometimes (deterministic — the
    /// write delay makes the window wide and unconditional).
    #[tokio::test]
    async fn waiting_for_the_writer_is_what_lands_the_record() {
        for _ in 0..3 {
            let store = Store::open_memory().await.unwrap();
            let (recorder, writer) = UsageRecorder::spawn_with_write_delay(
                store.clone(),
                std::future::pending(),
                StdDuration::from_millis(100),
            );
            recorder.record("m", UsageOutcome::Failed);
            drop(recorder); // last sender gone: normal end, writer starts draining
            assert!(
                all_rows(&store, "m").await.is_empty(),
                "negative control: right after dropping the sender, the 100ms \
                 write delay means the record has NOT reached the store yet"
            );
            stop_usage_writer(writer, Instant::now() + StdDuration::from_secs(5)).await;
            assert_eq!(
                all_rows(&store, "m").await.len(),
                1,
                "after stop_usage_writer returns, the record must be in the store"
            );
        }
    }

    /// The daemon-level J19 judgement (real `serve()`, real cut-off, module
    /// deps wiring) belongs to `server.rs`'s own tests / the black-box suite
    /// (4.3.1); this file only owns the recorder's half of the contract.
    #[tokio::test]
    async fn stop_usage_writer_does_not_wait_past_a_hard_stop_that_already_fired() {
        let store = Store::open_memory().await.unwrap();
        // hard_stop resolves at once: the writer should end almost
        // immediately, well inside stop_usage_writer's own generous deadline.
        let (recorder, writer) = UsageRecorder::spawn(store, std::future::ready(()));
        drop(recorder);
        let started = Instant::now();
        stop_usage_writer(writer, started + StdDuration::from_secs(5)).await;
        assert!(
            started.elapsed() < StdDuration::from_secs(1),
            "an already-fired hard stop must not make stop_usage_writer wait out its deadline"
        );
    }
}
