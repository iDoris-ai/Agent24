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
        let mut params = Map::new();
        params.insert("kind".to_owned(), Value::String(kind.to_owned()));
        params.insert("payload".to_owned(), Value::Object(payload));
        set_opt(
            &mut params,
            "request_id",
            request_id.map(|id| Value::String(id.as_str().to_owned())),
        );
        self.0.call(EMIT, Value::Object(params)).await?;
        Ok(())
    }

    /// Spawns [`EventSinkConfig::workers`] background tasks that drain a
    /// bounded queue and call [`Self::emit`] on this client's behalf, so a
    /// caller that does not want to `.await` every event gets a synchronous
    /// [`EventSink::emit`] instead.
    #[must_use]
    pub fn spawn_sink(&self, cfg: EventSinkConfig) -> EventSink {
        let (tx, rx) = mpsc::channel::<QueuedEvent>(cfg.queue_capacity);
        let rx = Arc::new(Mutex::new(rx));
        let dropped = Arc::new(AtomicU64::new(0));
        let semaphore = Arc::new(Semaphore::new(cfg.sub_quota.max(1)));
        for _ in 0..cfg.workers.max(1) {
            let rx = Arc::clone(&rx);
            let semaphore = Arc::clone(&semaphore);
            let client = self.clone();
            tokio::spawn(async move {
                loop {
                    let queued = { rx.lock().await.recv().await };
                    let Some(queued) = queued else {
                        return;
                    };
                    let Ok(permit) = Arc::clone(&semaphore).acquire_owned().await else {
                        return; // semaphore closed: nothing left to gate on.
                    };
                    let client = client.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Err(e) = client.emit(&queued.kind, queued.payload, None).await {
                            tracing::debug!(error = %e, kind = %queued.kind, "event sink: emit failed");
                        }
                    });
                }
            });
        }
        EventSink {
            tx,
            dropped,
            slot_wait: cfg.slot_wait,
        }
    }
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
/// never blocks its caller. A full queue gets one bounded wait (`slot_wait`)
/// on a background task before the event is dropped and counted.
pub struct EventSink {
    tx: mpsc::Sender<QueuedEvent>,
    dropped: Arc<AtomicU64>,
    slot_wait: Duration,
}

impl EventSink {
    /// Enqueue `kind`/`payload` for a background worker to send. Never
    /// blocks: a full queue gets `slot_wait` on a spawned task, then is
    /// dropped and logged (at every power-of-two drop count, so a
    /// persistently-full sink does not spam the log once per event).
    pub fn emit(&self, kind: &str, payload: Map<String, Value>) {
        let queued = QueuedEvent {
            kind: kind.to_owned(),
            payload,
        };
        match self.tx.try_send(queued) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => {}
            Err(mpsc::error::TrySendError::Full(queued)) => {
                let tx = self.tx.clone();
                let dropped = Arc::clone(&self.dropped);
                let slot_wait = self.slot_wait;
                tokio::spawn(async move {
                    if tokio::time::timeout(slot_wait, tx.send(queued))
                        .await
                        .is_err()
                    {
                        let n = dropped.fetch_add(1, Ordering::Relaxed) + 1;
                        if n.is_power_of_two() {
                            tracing::warn!(
                                dropped = n,
                                "event sink: queue stayed full; dropping events"
                            );
                        }
                    }
                });
            }
        }
    }

    /// Total events dropped so far (queue stayed full for the whole
    /// `slot_wait`).
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
}
