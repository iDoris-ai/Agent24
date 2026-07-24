//! WS event hub — GET /api/v1/events (SPEC-002 §3).
//!
//! Broadcast bus carries `(ts, EventBody)`; each connection keeps its own
//! monotonic `seq` and wraps the envelope at send time. Browser-Origin
//! upgrades are rejected pre-upgrade (403). Auth is enforced by the shared
//! middleware (bearer token) before this handler runs.

use agent24_protocol::{Event, EventBody};
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::Response;
use tokio::sync::broadcast;

use crate::server::AppState;

/// A client that never drains its receive buffer must not pin this task
/// forever — bound each send and disconnect on expiry (same treatment as the
/// provider-side request timeouts in B3).
const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Outbound bus capacity — sized generously (codex-rs practice) so a briefly
/// slow client survives a turn's burst; laggards get RecvError::Lagged and are
/// disconnected to reconcile via REST (v1 has no replay).
const BUS_CAPACITY: usize = 4096;

#[derive(Clone)]
pub struct EventsHub {
    tx: broadcast::Sender<(String, EventBody)>,
}

impl Default for EventsHub {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(BUS_CAPACITY);
        Self { tx }
    }
}

impl agent24_agent::EventSink for EventsHub {
    fn emit(&self, body: EventBody) {
        self.broadcast(body);
    }
}

impl EventsHub {
    /// Broadcast an event to all connected clients (no-op with none connected).
    pub fn broadcast(&self, body: EventBody) {
        let ts = agent24_core::util::now_iso8601();
        let _ = self.tx.send((ts, body));
    }

    fn subscribe(&self) -> broadcast::Receiver<(String, EventBody)> {
        self.tx.subscribe()
    }
}

pub async fn ws_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    // Presence check (not truthiness): reject any browser-originated upgrade,
    // including `Origin: null` (SPEC-002 §4)
    if headers.get("origin").is_some() {
        return crate::server::error_response(
            axum::http::StatusCode::FORBIDDEN,
            "unauthorized",
            "Browser-originated WebSocket upgrades are not allowed",
        );
    }
    let hub = state.events.clone();
    upgrade.on_upgrade(move |socket| client_loop(socket, hub))
}

async fn client_loop(mut socket: WebSocket, hub: EventsHub) {
    let mut rx = hub.subscribe();
    let mut seq: u64 = 0;
    loop {
        tokio::select! {
            received = rx.recv() => {
                match received {
                    Ok((ts, body)) => {
                        let event = Event { v: 1, seq, ts, body };
                        let Ok(text) = serde_json::to_string(&event) else { continue };
                        match tokio::time::timeout(SEND_TIMEOUT, socket.send(Message::Text(text.into())))
                            .await
                        {
                            Ok(Ok(())) => {
                                seq += 1; // only after a successful send — no seq holes
                            }
                            Ok(Err(_)) => break, // client gone
                            Err(_) => {
                                tracing::warn!("events client stalled >{SEND_TIMEOUT:?}, disconnecting");
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("events client lagged by {n}, disconnecting");
                        break; // client must reconcile via REST
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            // Drain client frames so pings/closes are processed
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    Some(Ok(_)) => {} // ignore client payloads (server→client protocol)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn broadcast_reaches_subscriber_with_ts() {
        let hub = EventsHub::default();
        let mut rx = hub.subscribe();
        hub.broadcast(EventBody::RunCancelled(
            agent24_protocol::RunCancelledPayload {
                run_id: "run_x".to_owned(),
            },
        ));
        let (ts, body) = rx.recv().await.unwrap();
        assert!(ts.ends_with('Z'));
        assert_eq!(body.wire_type(), "run.cancelled");
    }
}
