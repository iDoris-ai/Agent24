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
use rand::RngCore;
use tokio::sync::broadcast;

use crate::server::AppState;

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

impl EventsHub {
    /// Broadcast an event to all connected clients (no-op with none connected).
    pub fn broadcast(&self, body: EventBody) {
        let ts = now_iso8601();
        let _ = self.tx.send((ts, body));
    }

    fn subscribe(&self) -> broadcast::Receiver<(String, EventBody)> {
        self.tx.subscribe()
    }
}

/// ISO 8601 UTC without external chrono dep (second precision is enough for ts)
fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // civil-from-days algorithm (Howard Hinnant), valid for the unix era
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// ULID (Crockford base32): 48-bit ms timestamp + 80-bit randomness
pub fn ulid() -> String {
    const B32: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut out = [0u8; 26];
    let mut t = ms;
    for i in (0..10).rev() {
        out[i] = B32[(t % 32) as usize];
        t /= 32;
    }
    let mut rnd = [0u8; 16];
    rand::rng().fill_bytes(&mut rnd);
    for i in 0..16 {
        out[10 + i] = B32[(rnd[i] % 32) as usize];
    }
    String::from_utf8_lossy(&out).into_owned()
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
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            break; // client gone
                        }
                        seq += 1; // only after a successful send — no seq holes
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

    #[test]
    fn ulid_shape_and_uniqueness() {
        let a = ulid();
        let b = ulid();
        assert_eq!(a.len(), 26);
        assert!(
            a.bytes()
                .all(|c| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&c))
        );
        assert_ne!(a, b);
    }

    #[test]
    fn iso8601_shape() {
        let ts = now_iso8601();
        // e.g. 2026-07-24T12:00:00Z
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
        assert!(ts.starts_with("20"));
    }

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
