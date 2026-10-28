//! CORE-2 AgentEar slice S3 — the desktop confirmation channel
//! (`docs/agent/CORE-2-AGENTEAR-SLICE.md` §3.3 / §3.4).
//!
//! [`DesktopPresence`] is a challenge queue. The daemon side calls
//! [`PresenceProvider::confirm`], which enqueues a challenge and waits; the
//! desktop main process long-polls `GET /api/v1/presence/challenges/next`,
//! shows a native dialog, and answers with `POST /api/v1/presence/challenges/{id}`.
//! Both routes are ProductHost-only. Without a desktop poller seen in the last
//! [`POLLER_FRESH_FOR`], `confirm` answers `Unavailable` at once: a fake
//! "desktop is there" channel is never offered.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use agent24_protocol::state_file::AuthMode;
use async_trait::async_trait;
use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::sync::{Notify, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::capabilities::{Audience, Authorization, Operation, UnixSeconds, unix_now};
use crate::server::{AppState, error_response};

// ---------------------------------------------------------------------------
// TODO(S2 merge): replace this mirror with `capabilities::presence::*`
// (slice §3.3). Kept to the bare minimum so the rebase is mechanical: delete
// this block and import the same names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresenceOutcome {
    Allowed,
    Denied,
    Unavailable,
    TimedOut,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequesterIdentity {
    pub identifier: String,
    pub pid: i32,
}

#[derive(Clone, Debug)]
pub struct PresenceRequest {
    pub challenge_id: String,
    pub operation: Operation,
    pub method: String,
    pub path: String,
    pub summary: String,
    pub requester: RequesterIdentity,
    pub expires_at: UnixSeconds,
}

#[async_trait]
pub trait PresenceProvider: Send + Sync {
    async fn confirm(&self, req: PresenceRequest, cancel: CancellationToken) -> PresenceOutcome;
}
// ---------------------------------------------------------------------------

/// A desktop poller seen this recently counts as connected.
pub const POLLER_FRESH_FOR: Duration = Duration::from_secs(30);
/// Longest long-poll the route honours; the query `wait` is clamped to it.
pub const MAX_WAIT_SECONDS: u64 = 25;
/// Decided / expired challenges stay as tombstones this long past their
/// deadline so a late second answer is 409 / 410 rather than "unknown".
const TOMBSTONE_FOR: Duration = Duration::from_secs(120);
/// Queue bound: a runaway requester cannot grow it without limit.
const MAX_PENDING: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Decision {
    Allow,
    Deny,
}

enum Phase {
    Waiting,
    Decided,
    Expired,
}

struct Entry {
    req: PresenceRequest,
    deadline: Instant,
    delivered: bool,
    phase: Phase,
    tx: Option<oneshot::Sender<Decision>>,
}

#[derive(Default)]
struct Inner {
    last_poll: Option<Instant>,
    order: VecDeque<String>,
    entries: HashMap<String, Entry>,
}

#[derive(Default)]
pub struct DesktopPresence {
    inner: Mutex<Inner>,
    wake: Notify,
}

/// What `GET .../next` hands the desktop.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Delivered {
    pub challenge_id: String,
    pub operation: &'static str,
    pub summary: String,
    pub requester: RequesterIdentity,
    pub expires_at: UnixSeconds,
}

#[derive(Debug, Eq, PartialEq)]
pub enum DecideError {
    /// Already decided: the first answer wins.
    Conflict,
    /// Expired, timed out, cancelled, or never existed.
    Gone,
}

impl DesktopPresence {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn poller_fresh(inner: &Inner, now: Instant) -> bool {
        inner
            .last_poll
            .is_some_and(|at| now.saturating_duration_since(at) <= POLLER_FRESH_FOR)
    }

    fn prune(inner: &mut Inner, now: Instant) {
        inner.entries.retain(|_, entry| {
            matches!(entry.phase, Phase::Waiting) || entry.deadline + TOMBSTONE_FOR > now
        });
        let entries = &inner.entries;
        inner.order.retain(|id| entries.contains_key(id));
    }

    /// First undelivered, live challenge, marked delivered. No await between
    /// the check and the mark, so a dropped request cannot lose a challenge.
    fn take_next(&self, now: Instant) -> Option<Delivered> {
        let mut guard = self.lock();
        let inner = &mut *guard;
        for id in &inner.order {
            let Some(entry) = inner.entries.get_mut(id) else {
                continue;
            };
            if entry.delivered || !matches!(entry.phase, Phase::Waiting) || entry.deadline <= now {
                continue;
            }
            entry.delivered = true;
            return Some(Delivered {
                challenge_id: entry.req.challenge_id.clone(),
                operation: entry.req.operation.action(),
                summary: entry.req.summary.clone(),
                requester: entry.req.requester.clone(),
                expires_at: entry.req.expires_at,
            });
        }
        None
    }

    /// Long-poll: refreshes the poller's liveness on entry and exit.
    pub async fn next(&self, wait: Duration) -> Option<Delivered> {
        let end = Instant::now() + wait;
        loop {
            let notified = self.wake.notified();
            tokio::pin!(notified);
            // Register before checking, so a wake between check and await is kept.
            notified.as_mut().enable();
            let now = Instant::now();
            self.lock().last_poll = Some(now);
            if let Some(found) = self.take_next(now) {
                return Some(found);
            }
            if now >= end {
                return None;
            }
            tokio::select! {
                () = &mut notified => {}
                () = tokio::time::sleep_until(end) => {}
            }
            self.lock().last_poll = Some(Instant::now());
        }
    }

    /// Record the desktop's answer. First decision wins.
    pub fn decide(&self, id: &str, allow: bool) -> Result<(), DecideError> {
        let now = Instant::now();
        let mut inner = self.lock();
        let entry = inner.entries.get_mut(id).ok_or(DecideError::Gone)?;
        match entry.phase {
            Phase::Decided => return Err(DecideError::Conflict),
            Phase::Expired => return Err(DecideError::Gone),
            Phase::Waiting => {}
        }
        if entry.deadline <= now {
            entry.phase = Phase::Expired;
            return Err(DecideError::Gone);
        }
        entry.phase = Phase::Decided;
        let decision = if allow {
            Decision::Allow
        } else {
            Decision::Deny
        };
        if let Some(tx) = entry.tx.take() {
            let _ = tx.send(decision);
        }
        Ok(())
    }

    /// Settle after the wait ended without a decision in hand. `decide` holds
    /// the same lock while it sends, so reading the receiver here cannot miss
    /// a decision that landed in the same instant. On deadline that decision
    /// is honoured; on cancel the caller is gone and the answer is `TimedOut`.
    fn settle(
        &self,
        id: &str,
        rx: &mut oneshot::Receiver<Decision>,
        honour_late: bool,
    ) -> PresenceOutcome {
        let mut inner = self.lock();
        let late = rx.try_recv().ok();
        if let Some(entry) = inner.entries.get_mut(id) {
            if matches!(entry.phase, Phase::Waiting) {
                entry.phase = Phase::Expired;
            }
            entry.tx = None;
        }
        match late {
            Some(d) if honour_late => outcome(d),
            _ => PresenceOutcome::TimedOut,
        }
    }
}

fn outcome(decision: Decision) -> PresenceOutcome {
    match decision {
        Decision::Allow => PresenceOutcome::Allowed,
        Decision::Deny => PresenceOutcome::Denied,
    }
}

#[async_trait]
impl PresenceProvider for DesktopPresence {
    async fn confirm(&self, req: PresenceRequest, cancel: CancellationToken) -> PresenceOutcome {
        let now = Instant::now();
        let remaining = Duration::from_secs(req.expires_at.saturating_sub(unix_now()));
        if remaining.is_zero() {
            return PresenceOutcome::TimedOut;
        }
        let id = req.challenge_id.clone();
        let deadline = now + remaining;
        let (tx, mut rx) = oneshot::channel();
        {
            let mut inner = self.lock();
            Self::prune(&mut inner, now);
            if !Self::poller_fresh(&inner, now) {
                return PresenceOutcome::Unavailable;
            }
            let waiting = inner
                .entries
                .values()
                .filter(|e| matches!(e.phase, Phase::Waiting))
                .count();
            if waiting >= MAX_PENDING || inner.entries.contains_key(&id) {
                return PresenceOutcome::Unavailable;
            }
            inner.order.push_back(id.clone());
            inner.entries.insert(
                id.clone(),
                Entry {
                    req,
                    deadline,
                    delivered: false,
                    phase: Phase::Waiting,
                    tx: Some(tx),
                },
            );
        }
        self.wake.notify_waiters();
        tokio::select! {
            got = &mut rx => got.map_or(PresenceOutcome::TimedOut, outcome),
            () = tokio::time::sleep_until(deadline) => self.settle(&id, &mut rx, true),
            () = cancel.cancelled() => self.settle(&id, &mut rx, false),
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP

#[derive(Deserialize)]
pub struct NextQuery {
    wait: Option<u64>,
}

#[derive(Deserialize)]
pub struct DecisionBody {
    decision: String,
}

/// The routes exist only in capabilities mode and only for the ProductHost.
/// Legacy mode answers as if they were not registered (404).
/// `Some(refusal)` when the request must not proceed.
fn gate(state: &AppState, authorization: Option<Extension<Authorization>>) -> Option<Response> {
    if state.auth_mode != AuthMode::Capabilities {
        return Some(error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "No v1 route",
        ));
    }
    match authorization {
        Some(Extension(auth)) if auth.claims.audience == Audience::ProductHost => None,
        _ => Some(error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Operation not allowed",
        )),
    }
}

/// `GET /api/v1/presence/challenges/next?wait=<=25` (PresenceRead).
pub async fn get_next(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
    query: Result<Query<NextQuery>, QueryRejection>,
) -> Response {
    if let Some(denied) = gate(&state, authorization) {
        return denied;
    }
    let Ok(Query(query)) = query else {
        return error_response(StatusCode::BAD_REQUEST, "bad_request", "Invalid wait");
    };
    let wait = Duration::from_secs(query.wait.unwrap_or(MAX_WAIT_SECONDS).min(MAX_WAIT_SECONDS));
    match state.presence_desktop.next(wait).await {
        Some(c) => Json(serde_json::json!({
            "challenge_id": c.challenge_id,
            "operation": c.operation,
            "summary": c.summary,
            "requester": {"identifier": c.requester.identifier, "pid": c.requester.pid},
            "expires_at": c.expires_at,
        }))
        .into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

/// `POST /api/v1/presence/challenges/{id}` (PresenceDecide).
pub async fn post_decision(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<DecisionBody>, JsonRejection>,
) -> Response {
    if let Some(denied) = gate(&state, authorization) {
        return denied;
    }
    let (Ok(Path(id)), Ok(Json(body))) = (path, body) else {
        return error_response(StatusCode::BAD_REQUEST, "bad_request", "Invalid request");
    };
    let allow = match body.decision.as_str() {
        "allow" => true,
        "deny" => false,
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "decision must be \"allow\" or \"deny\"",
            );
        }
    };
    match state.presence_desktop.decide(&id, allow) {
        Ok(()) => Json(serde_json::json!({"challenge_id": id, "state": "decided"})).into_response(),
        Err(DecideError::Conflict) => error_response(
            StatusCode::CONFLICT,
            "already_decided",
            "This challenge already has a decision",
        ),
        Err(DecideError::Gone) => error_response(
            StatusCode::GONE,
            "challenge_gone",
            "This challenge expired or does not exist",
        ),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Method, Request, header};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::server::build_router;

    fn request(id: &str, ttl: u64) -> PresenceRequest {
        PresenceRequest {
            challenge_id: id.to_owned(),
            operation: Operation::ModuleAdmin,
            method: "POST".to_owned(),
            path: "/api/v1/attached".to_owned(),
            summary: "Register module agentear".to_owned(),
            requester: RequesterIdentity {
                identifier: "agent24".to_owned(),
                pid: 4242,
            },
            expires_at: unix_now() + ttl,
        }
    }

    async fn connected() -> Arc<DesktopPresence> {
        let presence = Arc::new(DesktopPresence::new());
        assert!(presence.next(Duration::ZERO).await.is_none());
        presence
    }

    fn spawn_confirm(
        presence: &Arc<DesktopPresence>,
        req: PresenceRequest,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<PresenceOutcome> {
        let presence = presence.clone();
        tokio::spawn(async move { presence.confirm(req, cancel).await })
    }

    #[tokio::test(start_paused = true)]
    async fn unavailable_without_a_recent_poller() {
        let presence = Arc::new(DesktopPresence::new());
        let out = presence
            .confirm(request("c0", 60), CancellationToken::new())
            .await;
        assert_eq!(out, PresenceOutcome::Unavailable);

        assert!(presence.next(Duration::ZERO).await.is_none());
        tokio::time::advance(POLLER_FRESH_FOR + Duration::from_secs(1)).await;
        let out = presence
            .confirm(request("c1", 60), CancellationToken::new())
            .await;
        assert_eq!(out, PresenceOutcome::Unavailable);
    }

    #[tokio::test(start_paused = true)]
    async fn allow_and_deny_map_and_late_second_answers_conflict() {
        let presence = connected().await;
        for (id, allow, want) in [
            ("a1", true, PresenceOutcome::Allowed),
            ("d1", false, PresenceOutcome::Denied),
        ] {
            let task = spawn_confirm(&presence, request(id, 60), CancellationToken::new());
            let got = presence.next(Duration::from_secs(5)).await.unwrap();
            assert_eq!(got.challenge_id, id);
            assert_eq!(got.operation, "module.admin");
            assert_eq!(got.requester.pid, 4242);
            presence.decide(id, allow).unwrap();
            assert_eq!(task.await.unwrap(), want);
            // First decision wins, also after confirm() has returned.
            assert_eq!(presence.decide(id, !allow), Err(DecideError::Conflict));
            assert_eq!(presence.decide(id, allow), Err(DecideError::Conflict));
        }
        assert_eq!(presence.decide("nope", true), Err(DecideError::Gone));
    }

    #[tokio::test(start_paused = true)]
    async fn a_challenge_is_handed_out_once() {
        let presence = connected().await;
        let task = spawn_confirm(&presence, request("o1", 60), CancellationToken::new());
        assert!(presence.next(Duration::from_secs(5)).await.is_some());
        assert!(presence.next(Duration::ZERO).await.is_none());
        presence.decide("o1", true).unwrap();
        assert_eq!(task.await.unwrap(), PresenceOutcome::Allowed);
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_maps_to_timed_out_and_late_decision_is_gone() {
        let presence = connected().await;
        let task = spawn_confirm(&presence, request("t1", 60), CancellationToken::new());
        assert!(presence.next(Duration::from_secs(5)).await.is_some());
        tokio::time::advance(Duration::from_secs(61)).await;
        assert_eq!(task.await.unwrap(), PresenceOutcome::TimedOut);
        assert_eq!(presence.decide("t1", true), Err(DecideError::Gone));
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_maps_to_timed_out_and_late_decision_is_gone() {
        let presence = connected().await;
        let cancel = CancellationToken::new();
        let task = spawn_confirm(&presence, request("x1", 60), cancel.clone());
        assert!(presence.next(Duration::from_secs(5)).await.is_some());
        cancel.cancel();
        assert_eq!(task.await.unwrap(), PresenceOutcome::TimedOut);
        assert_eq!(presence.decide("x1", true), Err(DecideError::Gone));
    }

    #[tokio::test(start_paused = true)]
    async fn an_already_expired_request_times_out_without_queueing() {
        let presence = connected().await;
        let mut req = request("e1", 0);
        req.expires_at = unix_now().saturating_sub(1);
        assert_eq!(
            presence.confirm(req, CancellationToken::new()).await,
            PresenceOutcome::TimedOut
        );
        assert!(presence.next(Duration::ZERO).await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn long_poll_returns_none_after_the_wait() {
        let presence = connected().await;
        let started = Instant::now();
        assert!(presence.next(Duration::from_secs(25)).await.is_none());
        assert!(started.elapsed() >= Duration::from_secs(25));
    }

    // -- HTTP ------------------------------------------------------------

    async fn call(
        router: &axum::Router,
        method: Method,
        uri: &str,
        bearer: &str,
        body: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"));
        let body = match body {
            Some(text) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(text.to_owned())
            }
            None => Body::empty(),
        };
        let res = router
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    const NEXT: &str = "/api/v1/presence/challenges/next?wait=0";
    const DECIDE: &str = "/api/v1/presence/challenges/abc";
    const ALLOW: &str = r#"{"decision":"allow"}"#;
    const DENY: &str = r#"{"decision":"deny"}"#;

    #[tokio::test]
    async fn non_product_host_audiences_get_403() {
        let (state, host, creative) = crate::server::tests::capability_state().await;
        let router = build_router(state);
        let (status, _) = call(&router, Method::GET, NEXT, &creative, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = call(&router, Method::POST, DECIDE, &creative, Some(ALLOW)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // Control: the host passes the same gate (204 / 410, not 403).
        let (status, _) = call(&router, Method::GET, NEXT, &host, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(&router, Method::POST, DECIDE, &host, Some(ALLOW)).await;
        assert_eq!(status, StatusCode::GONE);
    }

    #[tokio::test]
    async fn the_routes_do_not_exist_in_legacy_mode() {
        let state = crate::server::tests::state().await;
        let token = state.token.to_string();
        let router = build_router(state);
        let (status, body) = call(&router, Method::GET, NEXT, &token, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
        let (status, _) = call(&router, Method::POST, DECIDE, &token, Some(ALLOW)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn http_round_trip_first_decision_wins_then_409_then_410() {
        let (state, host, _) = crate::server::tests::capability_state().await;
        let presence = state.presence_desktop.clone();
        let router = build_router(state);

        // No poll yet -> unavailable.
        assert_eq!(
            presence
                .confirm(request("h0", 60), CancellationToken::new())
                .await,
            PresenceOutcome::Unavailable
        );
        let (status, _) = call(&router, Method::GET, NEXT, &host, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let task = spawn_confirm(&presence, request("h1", 60), CancellationToken::new());
        let (status, body) = call(
            &router,
            Method::GET,
            "/api/v1/presence/challenges/next?wait=25",
            &host,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["challenge_id"], "h1");
        assert_eq!(body["operation"], "module.admin");
        assert_eq!(body["summary"], "Register module agentear");
        assert_eq!(body["requester"]["identifier"], "agent24");
        assert_eq!(body["requester"]["pid"], 4242);
        assert!(body["expires_at"].as_u64().unwrap() > unix_now());

        let uri = "/api/v1/presence/challenges/h1";
        let bad = Some(r#"{"decision":"maybe"}"#);
        let (status, _) = call(&router, Method::POST, uri, &host, bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&router, Method::POST, uri, &host, Some(DENY)).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(&router, Method::POST, uri, &host, Some(ALLOW)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(task.await.unwrap(), PresenceOutcome::Denied);

        let unknown = "/api/v1/presence/challenges/unknown";
        let (status, _) = call(&router, Method::POST, unknown, &host, Some(ALLOW)).await;
        assert_eq!(status, StatusCode::GONE);
    }

    #[tokio::test]
    async fn an_expired_challenge_is_410_over_http() {
        let (state, host, _) = crate::server::tests::capability_state().await;
        let presence = state.presence_desktop.clone();
        let router = build_router(state);
        assert!(presence.next(Duration::ZERO).await.is_none());
        let task = spawn_confirm(&presence, request("z1", 1), CancellationToken::new());
        assert_eq!(task.await.unwrap(), PresenceOutcome::TimedOut);
        let uri = "/api/v1/presence/challenges/z1";
        let (status, _) = call(&router, Method::POST, uri, &host, Some(ALLOW)).await;
        assert_eq!(status, StatusCode::GONE);
    }
}
