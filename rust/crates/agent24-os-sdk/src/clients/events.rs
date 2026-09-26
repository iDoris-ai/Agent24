//! ME4-S3 §2.4/§3.5 — `EventsClient` (`_a24/events/emit`) and `EventSink`, a
//! bounded fire-and-forget queue over it for callers that do not want to
//! `.await` every emit (the common case: "an action happened, tell the
//! kernel, keep going").

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use agent24_os_proto::module::Connection;
use serde_json::{Map, Value};
use tokio::sync::{Mutex, Semaphore, mpsc};

use super::{Core, set_opt};
use crate::context::RequestId;
use crate::error::ClientError;

pub const EMIT: &str = "_a24/events/emit";
pub(crate) const METHODS: [&str; 1] = [EMIT];
const OFFER_PREFIX: &str = "_a24/events/";

#[derive(Clone)]
pub struct EventsClient(Core);

impl EventsClient {
    #[must_use]
    pub fn new(conn: &Arc<Connection>) -> Option<Self> {
        Core::new(conn, OFFER_PREFIX).map(Self)
    }

    /// # Errors
    /// See [`ClientError`].
    pub async fn emit(
        &self,
        kind: &str,
        payload: Map<String, Value>,
        request_id: Option<&RequestId>,
    ) -> Result<(), ClientError> {
        self.0
            .call(EMIT, build_emit_params(kind, payload, request_id))
            .await?;
        Ok(())
    }

    /// Spawns [`EventSinkConfig::workers`] background tasks that drain a
    /// bounded queue and call `_a24/events/emit` on this client's behalf, so
    /// a caller that does not want to `.await` every event gets a
    /// synchronous [`EventSink::emit`] instead.
    ///
    /// Ported from Sin90's `KernelEventSink::new` (`adapter_agent24/mod.rs`,
    /// N-M1/N-M2): each worker gates a dequeued event through `semaphore`
    /// (the sub-quota, [`EventSinkConfig::sub_quota`]) before it ever
    /// competes for the connection's own in-flight semaphore, and both
    /// waits share ONE combined `deadline` (`now + slot_wait`) — whatever is
    /// left of that budget after the sub-quota wait is what gets passed on
    /// as the call's own `slot_wait`, so a queued event can wait at most
    /// [`EventSinkConfig::slot_wait`] end to end, never that much twice.
    #[must_use]
    pub fn spawn_sink(&self, cfg: EventSinkConfig) -> EventSink {
        let (tx, rx) = mpsc::channel::<QueuedEvent>(cfg.queue_capacity);
        let rx = Arc::new(Mutex::new(rx));
        let dropped = Arc::new(AtomicU64::new(0));
        let semaphore = Arc::new(Semaphore::new(cfg.sub_quota.max(1)));
        let slot_wait = cfg.slot_wait;
        for _ in 0..cfg.workers.max(1) {
            let rx = Arc::clone(&rx);
            let semaphore = Arc::clone(&semaphore);
            let client = self.clone();
            tokio::spawn(async move {
                loop {
                    let queued = { rx.lock().await.recv().await };
                    let Some(queued) = queued else {
                        return; // every `EventSink` (and its `Sender`) dropped.
                    };
                    let deadline = tokio::time::Instant::now() + slot_wait;
                    let permit = match tokio::time::timeout_at(
                        deadline,
                        Arc::clone(&semaphore).acquire_owned(),
                    )
                    .await
                    {
                        Ok(Ok(permit)) => permit,
                        Ok(Err(_closed)) => return, // semaphore closed: nothing left to gate on.
                        Err(_elapsed) => {
                            tracing::warn!(
                                kind = %queued.kind,
                                "event sink: dropped — timed out waiting for the emit sub-quota"
                            );
                            continue;
                        }
                    };
                    // Whatever's left of the combined budget, after the
                    // sub-quota wait, is what's left to wait for a slot on
                    // the connection's own semaphore.
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    let _permit = permit;
                    let params = build_emit_params(&queued.kind, queued.payload, None);
                    // No second `tokio::spawn` here (Sin90 N-M2): the fixed
                    // worker pool itself is what bounds concurrency, so this
                    // just awaits the call directly.
                    if let Err(e) = client.0.call_with_slot_wait(EMIT, params, remaining).await {
                        tracing::warn!(error = %e, kind = %queued.kind, "event sink: emit failed or was dropped");
                    }
                }
            });
        }
        EventSink { tx, dropped }
    }
}

/// Shared by [`EventsClient::emit`] and [`EventsClient::spawn_sink`]'s
/// worker loop, so the wire shape (`kind`/`payload`/optional `request_id`,
/// "omit don't null") lives in exactly one place.
fn build_emit_params(
    kind: &str,
    payload: Map<String, Value>,
    request_id: Option<&RequestId>,
) -> Value {
    let mut params = Map::new();
    params.insert("kind".to_owned(), Value::String(kind.to_owned()));
    params.insert("payload".to_owned(), Value::Object(payload));
    set_opt(
        &mut params,
        "request_id",
        request_id.map(|id| Value::String(id.as_str().to_owned())),
    );
    Value::Object(params)
}

/// Default `256`/`4`/`32`/`5s` — the numbers Sin90's production sink used
/// (ME4-S3 §1.2 row 11).
#[derive(Debug, Clone, Copy)]
pub struct EventSinkConfig {
    pub queue_capacity: usize,
    pub workers: usize,
    pub sub_quota: usize,
    pub slot_wait: Duration,
}

impl Default for EventSinkConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 256,
            workers: 4,
            sub_quota: 32,
            slot_wait: Duration::from_secs(5),
        }
    }
}

struct QueuedEvent {
    kind: String,
    payload: Map<String, Value>,
}

/// A bounded, best-effort event queue: [`Self::emit`] is synchronous and
/// never blocks its caller. Ported from Sin90's `KernelEventSink::emit`
/// (N-M2): a full queue means the event is dropped and counted immediately
/// — no `tokio::spawn` per call, unlike the earlier version of this type.
pub struct EventSink {
    tx: mpsc::Sender<QueuedEvent>,
    dropped: Arc<AtomicU64>,
}

impl EventSink {
    /// Enqueue `kind`/`payload` for a background worker to send. Never
    /// blocks and never spawns: a full (or closed) queue is dropped and
    /// counted right here, logged at every power-of-two drop count so a
    /// persistently-full sink does not spam the log once per event (Sin90
    /// N-M2 / Codex 2026-09-22 review Medium #6).
    pub fn emit(&self, kind: &str, payload: Map<String, Value>) {
        let queued = QueuedEvent {
            kind: kind.to_owned(),
            payload,
        };
        if self.tx.try_send(queued).is_err() {
            let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_power_of_two() {
                tracing::warn!(
                    kind,
                    dropped_total = n,
                    "event sink: queue full, dropping event"
                );
            }
        }
    }

    /// Total events dropped so far (the bounded queue was full, or already
    /// closed, at `emit()` time).
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(all(test, feature = "test-util"))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;
    use crate::testing;

    #[tokio::test]
    async fn emit_sends_kind_and_payload_and_omits_absent_request_id() {
        let (conn, mut peer) = testing::fake_kernel(vec![OFFER_PREFIX.to_owned()]).await;
        let client = EventsClient::new(&conn).expect("offer covers events");
        let mut payload = Map::new();
        payload.insert("k".to_owned(), json!("v"));
        let call =
            tokio::spawn(async move { client.emit("task.transitioned", payload, None).await });
        let req = testing::read_request(&mut peer).await;
        assert_eq!(req["method"], EMIT);
        assert_eq!(req["params"]["kind"], "task.transitioned");
        assert_eq!(req["params"]["payload"], json!({"k": "v"}));
        assert!(
            req["params"]
                .as_object()
                .unwrap()
                .get("request_id")
                .is_none(),
            "absent request_id must not be sent, not sent as null"
        );
        testing::respond(&mut peer, &req, json!({})).await;
        call.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn emit_sends_request_id_when_given() {
        let (conn, mut peer) = testing::fake_kernel(vec![OFFER_PREFIX.to_owned()]).await;
        let client = EventsClient::new(&conn).expect("offer covers events");
        let id = RequestId::for_test("req-7");
        let call = tokio::spawn(async move { client.emit("k", Map::new(), Some(&id)).await });
        let req = testing::read_request(&mut peer).await;
        assert_eq!(req["params"]["request_id"], "req-7");
        testing::respond(&mut peer, &req, json!({})).await;
        call.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn without_the_offer_prefix_new_returns_none() {
        let (conn, _peer) = testing::fake_kernel(vec![]).await;
        assert!(EventsClient::new(&conn).is_none());
    }

    /// Ported from Sin90's
    /// `kernel_event_sink_drops_and_counts_when_the_bounded_queue_is_full`:
    /// `EventSink::emit` only ever does a non-blocking `try_send` — a full
    /// queue means the event is dropped and counted RIGHT HERE, not queued
    /// onto a spawned task. Filling the queue in a tight, non-yielding loop
    /// on the (default, current-thread) `#[tokio::test]` runtime means none
    /// of the fixed worker tasks get a chance to drain anything while we
    /// fill it, so `dropped()` already reads the exact overflow count
    /// immediately after the loop returns — no `.await` happens in between.
    /// The earlier (pre-fix) implementation could not pass this: it only
    /// incremented `dropped` inside a spawned task after `slot_wait` (a real
    /// 5s here) elapsed, so an immediate read would have seen `0`.
    #[tokio::test]
    async fn emit_drops_and_counts_immediately_when_the_queue_is_full() {
        let (conn, _peer) = testing::fake_kernel(vec![OFFER_PREFIX.to_owned()]).await;
        let client = EventsClient::new(&conn).expect("offer covers events");
        let cfg = EventSinkConfig {
            queue_capacity: 8,
            workers: 1,
            sub_quota: 1,
            slot_wait: Duration::from_secs(5),
        };
        let sink = client.spawn_sink(cfg);

        let overflow = 5;
        for i in 0..(cfg.queue_capacity + overflow) {
            sink.emit(&format!("flood.{i}"), Map::new());
        }
        assert_eq!(sink.dropped(), overflow as u64);
    }
}
