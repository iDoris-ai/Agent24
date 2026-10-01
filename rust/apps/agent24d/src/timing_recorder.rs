//! ME4-desktop-model-ui — per-call timing ledger, writer side
//! (`agent24-store`'s `model_call_timings` table, migration
//! `0013_model_call_timings.sql`). Mirrors `usage_recorder.rs`'s shape (a
//! bounded mpsc channel + one background task, so no handler ever awaits a
//! disk write) but with a much looser contract: this ledger is a debugging
//! aid ("慢在哪一步"), never a source of truth like the usage counters
//! `usage_recorder.rs` writes, so there is no `stop_*`/`hard_stop`/
//! `dropped()` apparatus here — losing the last few in-flight rows at
//! shutdown, or the newest row when the channel is briefly full, is fully
//! acceptable. The channel simply closes when every sender (one per
//! `ModelCallbackDeps`/`AppState` clone) is dropped, the task drains
//! whatever's already queued, and returns.
//!
//! Retention (design ask, "只留最近 30 天或最多 10 万行，取先到者"): pruned
//! once immediately on spawn (covers "启动时") and again every
//! [`PRUNE_EVERY`] records after that (covers "定期") — good enough for a
//! diagnostic ledger; missing a cycle just means one extra batch of rows
//! survives until the next one, never unbounded growth.

use std::sync::Arc;

use agent24_models::ModelError;
use agent24_store::{NewCallTiming, Store};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Bounded so a burst cannot grow this task's queue without limit — smaller
/// than `usage_recorder.rs`'s 1024 (§6.3) since a dropped timing row is a
/// shrug, not a metering gap.
const CHANNEL_CAPACITY: usize = 256;
/// Design ask: "最近 30 天".
pub const RETENTION_MAX_AGE_DAYS: u64 = 30;
/// Design ask: "最多 10 万行".
pub const RETENTION_MAX_ROWS: u32 = 100_000;
/// "定期清理" — every this many written rows, prune again.
const PRUNE_EVERY: u32 = 200;

/// One completed model call's timing facts — content-free BY CONSTRUCTION:
/// there is no field here a prompt, a reply, or a transcript could end up
/// in (mirrors `agent24_protocol::ModelCallPayload`'s same guarantee for the
/// WS event this shares its call sites with).
#[derive(Debug, Clone, Default)]
pub struct TimingObservation {
    /// `"chat"` (`/api/v1/chat`) or `"module:<name>"` (`_a24/model/complete`).
    pub source: String,
    pub model_id: Option<String>,
    /// `"local"` | `"remote"`, or `None` when no provider ever answered.
    pub tier: Option<String>,
    pub served_by: Option<String>,
    pub ok: bool,
    pub error_kind: Option<String>,
    /// A module's own finer-grained sub-step (AgentEar's asr/tts/…), when it
    /// reports one — set by `agentear_timings.rs`, `None` from every other
    /// call site.
    pub step: Option<String>,
    /// AgentEar's own per-turn correlation ids (opaque identifiers, never
    /// content) — set by `agentear_timings.rs` only; every other call site
    /// leaves both `None` (neither `_a24/model/complete` nor `/api/v1/chat`
    /// is turn-scoped).
    pub session_id: Option<String>,
    pub seq: Option<u64>,
    pub first_token_ms: Option<u64>,
    pub total_ms: u64,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

/// Where a completed call's timing goes. Synchronous and non-blocking by
/// contract, same as `model_callback.rs`'s `UsageSink` — it may be called
/// from a context that must never await.
pub trait TimingSink: Send + Sync {
    fn record(&self, obs: TimingObservation);
}

/// Test-only in-memory sink — nothing in the production path constructs it
/// (production always goes through `TimingRecorder::spawn`'s channel sink).
#[cfg(test)]
#[derive(Default)]
pub struct MemoryTimingSink(std::sync::Mutex<Vec<TimingObservation>>);

#[cfg(test)]
impl MemoryTimingSink {
    pub fn take(&self) -> Vec<TimingObservation> {
        std::mem::take(
            &mut *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

#[cfg(test)]
impl TimingSink for MemoryTimingSink {
    fn record(&self, obs: TimingObservation) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(obs);
    }
}

struct ChannelSink(mpsc::Sender<TimingObservation>);

impl TimingSink for ChannelSink {
    fn record(&self, obs: TimingObservation) {
        // Never blocks/awaits: a full channel drops the newest observation —
        // see this module's doc comment on why that's an acceptable loss
        // here, unlike `usage_recorder.rs`'s exact counters.
        let _ = self.0.try_send(obs);
    }
}

fn to_new_call_timing(obs: TimingObservation) -> NewCallTiming {
    NewCallTiming {
        ts: agent24_core::util::now_iso8601(),
        source: obs.source,
        model_id: obs.model_id,
        tier: obs.tier,
        served_by: obs.served_by,
        ok: obs.ok,
        error_kind: obs.error_kind,
        step: obs.step,
        // Review M2: `agentear_timings.rs` now records THROUGH this same
        // sink/writer (unified counting, periodic pruning, and serialized
        // writes — no more separate direct-to-store path) — so its
        // session_id/seq simply pass through here like every other field.
        session_id: obs.session_id,
        seq: obs.seq,
        first_token_ms: obs.first_token_ms,
        total_ms: obs.total_ms,
        prompt_tokens: obs.prompt_tokens,
        completion_tokens: obs.completion_tokens,
    }
}

async fn prune(store: &Store) {
    let cutoff = agent24_core::util::iso8601_before(std::time::Duration::from_secs(
        RETENTION_MAX_AGE_DAYS * 86_400,
    ));
    if let Err(e) = store.prune_call_timings(&cutoff, RETENTION_MAX_ROWS).await {
        tracing::warn!("model_call_timings: prune failed: {e}");
    }
}

pub struct TimingRecorder;

impl TimingRecorder {
    /// Spawns the background writer; every `TimingSink::record` from any
    /// clone of the returned `Arc` funnels through the SAME task (and so the
    /// same `since_prune` counter), matching `usage_recorder.rs`'s "one
    /// writer task" shape.
    pub fn spawn(store: Store) -> (Arc<dyn TimingSink>, JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel::<TimingObservation>(CHANNEL_CAPACITY);
        let handle = tokio::spawn(async move {
            prune(&store).await;
            let mut since_prune: u32 = 0;
            while let Some(obs) = rx.recv().await {
                let row = to_new_call_timing(obs);
                if let Err(e) = store.record_call_timing(&row).await {
                    tracing::warn!("model_call_timings: write failed: {e}");
                }
                since_prune += 1;
                if since_prune >= PRUNE_EVERY {
                    since_prune = 0;
                    prune(&store).await;
                }
            }
        });
        (Arc::new(ChannelSink(tx)), handle)
    }
}

/// Coarse classification of a router-level failure, for `error_kind` —
/// shared by both call sites that record a FAILED timing row
/// (`_a24/model/complete` and `/api/v1/chat`), so the two never invent their
/// own, possibly-disagreeing vocabulary for the same four [`ModelError`]
/// variants.
pub fn timing_error_kind(e: &ModelError) -> &'static str {
    match e {
        ModelError::Unavailable(_) => "unavailable",
        ModelError::Provider(_) => "provider_error",
        ModelError::Rejected { .. } => "rejected",
        ModelError::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn obs(source: &str, total_ms: u64) -> TimingObservation {
        TimingObservation {
            source: source.to_owned(),
            model_id: Some("m".to_owned()),
            tier: Some("local".to_owned()),
            served_by: Some("omlx".to_owned()),
            ok: true,
            total_ms,
            prompt_tokens: Some(1),
            completion_tokens: Some(1),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn spawn_writes_every_recorded_observation_to_the_store() {
        let store = Store::open_memory().await.unwrap();
        // `Store` is a cheap `Clone` (an `Arc`-backed pool) — this handle
        // reads back what the background task (which owns the OTHER clone)
        // writes.
        let reader = store.clone();
        let (sink, _handle) = TimingRecorder::spawn(store);
        sink.record(obs("chat", 100));
        sink.record(obs("module:agentear", 200));

        // The writer task is a separate tokio task — poll until both rows
        // land rather than assuming a fixed delay is enough.
        for _ in 0..50 {
            if reader
                .query_call_timings(None, None, 10)
                .await
                .unwrap()
                .len()
                >= 2
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let rows = reader.query_call_timings(None, None, 10).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r.source == "chat" && r.total_ms == 100));
        assert!(
            rows.iter()
                .any(|r| r.source == "module:agentear" && r.total_ms == 200)
        );
    }

    #[test]
    fn timing_error_kind_covers_every_model_error_variant() {
        assert_eq!(
            timing_error_kind(&ModelError::Unavailable("x".into())),
            "unavailable"
        );
        assert_eq!(
            timing_error_kind(&ModelError::Provider("x".into())),
            "provider_error"
        );
        assert_eq!(
            timing_error_kind(&ModelError::Rejected {
                status: 400,
                message: "x".into()
            }),
            "rejected"
        );
        assert_eq!(timing_error_kind(&ModelError::Cancelled), "cancelled");
    }

    #[test]
    fn memory_sink_records_in_order() {
        let sink = MemoryTimingSink::default();
        sink.record(obs("chat", 1));
        sink.record(obs("chat", 2));
        let taken = sink.take();
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].total_ms, 1);
        assert_eq!(taken[1].total_ms, 2);
        assert!(sink.take().is_empty(), "take() drains");
    }
}
