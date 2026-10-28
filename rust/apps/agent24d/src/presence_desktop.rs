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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent24_protocol::state_file::AuthMode;
use async_trait::async_trait;
use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::sync::{Notify, Semaphore, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::capabilities::presence::{
    PresenceOutcome, PresenceProvider, PresenceRequest, RequesterIdentity,
};
use crate::capabilities::{Audience, Authorization, UnixSeconds, unix_now};
use crate::server::{AppState, error_response};

/// A desktop poller seen this recently counts as connected.
pub const POLLER_FRESH_FOR: Duration = Duration::from_secs(30);
/// Longest long-poll the route honours; the query `wait` is clamped to it.
pub const MAX_WAIT_SECONDS: u64 = 25;
/// Decided challenges stay as tombstones this long past their deadline so a
/// late second answer is 409 rather than 410. Expired, cancelled or dropped
/// ones are removed at once (a late answer is 410 either way), so a peer
/// that elevates and disconnects cannot fill `MAX_RECORDS` (#1447 F4).
const TOMBSTONE_FOR: Duration = Duration::from_secs(120);
/// Queue bound: a runaway requester cannot grow it without limit.
const MAX_PENDING: usize = 64;
/// Includes tombstones; reject new challenges rather than evict first-decision history.
const MAX_RECORDS: usize = 128;
/// Excess authenticated polls receive 429 and may retry after a slot is released.
const MAX_ACTIVE_POLLS: usize = 16;

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

pub struct DesktopPresence {
    inner: Mutex<Inner>,
    wake: Notify,
    polls: Semaphore,
}

impl Default for DesktopPresence {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            wake: Notify::new(),
            polls: Semaphore::new(MAX_ACTIVE_POLLS),
        }
    }
}

/// Dropping confirm (for example on UDS disconnect) must also retire its entry.
struct PendingConfirmation<'a> {
    presence: &'a DesktopPresence,
    id: &'a str,
}

impl Drop for PendingConfirmation<'_> {
    fn drop(&mut self) {
        let mut inner = self.presence.lock();
        if let Some(entry) = inner.entries.get_mut(self.id)
            && matches!(entry.phase, Phase::Waiting)
        {
            entry.phase = Phase::Expired;
            entry.tx = None;
        }
        DesktopPresence::prune(&mut inner, Instant::now());
    }
}

impl Entry {
    fn expired(&self, now: Instant, wall_now: UnixSeconds) -> bool {
        self.deadline <= now || self.req.expires_at <= wall_now
    }
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
        let wall_now = unix_now();
        inner.entries.retain(|_, entry| {
            if matches!(entry.phase, Phase::Waiting) && entry.expired(now, wall_now) {
                entry.phase = Phase::Expired;
                entry.tx = None;
            }
            match entry.phase {
                Phase::Expired => false,
                Phase::Waiting | Phase::Decided => entry.deadline + TOMBSTONE_FOR > now,
            }
        });
        let entries = &inner.entries;
        inner.order.retain(|id| entries.contains_key(id));
    }

    /// First undelivered, live challenge, marked delivered. No await between
    /// the check and the mark, so a dropped request cannot lose a challenge.
    fn take_next(&self, now: Instant) -> Option<Delivered> {
        let mut guard = self.lock();
        let inner = &mut *guard;
        Self::prune(inner, now);
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
        Self::prune(&mut inner, now);
        let entry = inner.entries.get_mut(id).ok_or(DecideError::Gone)?;
        match entry.phase {
            Phase::Decided => return Err(DecideError::Conflict),
            Phase::Expired => return Err(DecideError::Gone),
            Phase::Waiting => {}
        }
        if entry.expired(now, unix_now()) {
            entry.phase = Phase::Expired;
            entry.tx = None;
            Self::prune(&mut inner, now);
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
        Self::prune(&mut inner, Instant::now());
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
        let wall_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let remaining = Duration::from_secs(req.expires_at).saturating_sub(wall_now);
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
            if waiting >= MAX_PENDING
                || inner.entries.len() >= MAX_RECORDS
                || inner.entries.contains_key(&id)
            {
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
        let _pending = PendingConfirmation {
            presence: self,
            id: &id,
        };
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
    let Some(presence) = &state.presence_desktop else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "presence_unavailable",
            "No desktop presence channel",
        );
    };
    let Ok(_permit) = presence.polls.try_acquire() else {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "presence_poll_limit",
            "Too many active presence polls",
        );
    };
    let wait = Duration::from_secs(query.wait.unwrap_or(MAX_WAIT_SECONDS).min(MAX_WAIT_SECONDS));
    match presence.next(wait).await {
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
    let Some(presence) = &state.presence_desktop else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "presence_unavailable",
            "No desktop presence channel",
        );
    };
    match presence.decide(&id, allow) {
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
    use crate::capabilities::Operation;
    use crate::enroll_api::EnrollAuthority;
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

    #[tokio::test(start_paused = true)]
    async fn dropped_confirm_releases_waiting_and_expired_records_are_pruned_by_poll() {
        let presence = connected().await;
        for n in 0..64 {
            let id = format!("drop-{n}");
            let task = spawn_confirm(&presence, request(&id, 60), CancellationToken::new());
            presence.next(Duration::from_secs(1)).await.unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
        assert!(
            presence
                .lock()
                .entries
                .values()
                .all(|e| !matches!(e.phase, Phase::Waiting))
        );
        tokio::time::advance(Duration::from_secs(181)).await;
        presence.next(Duration::ZERO).await;
        assert!(presence.lock().entries.is_empty());
        let task = spawn_confirm(
            &presence,
            request("recovered", 60),
            CancellationToken::new(),
        );
        assert_eq!(
            presence
                .next(Duration::from_secs(1))
                .await
                .unwrap()
                .challenge_id,
            "recovered"
        );
        presence.decide("recovered", true).unwrap();
        assert_eq!(task.await.unwrap(), PresenceOutcome::Allowed);
    }

    #[tokio::test(start_paused = true)]
    async fn prune_expires_waiting_even_if_confirm_has_not_resumed() {
        let presence = connected().await;
        let task = spawn_confirm(
            &presence,
            request("expired-waiting", 60),
            CancellationToken::new(),
        );
        presence.next(Duration::from_secs(1)).await.unwrap();
        let mut inner = presence.lock();
        let now = Instant::now() + Duration::from_secs(61);
        DesktopPresence::prune(&mut inner, now);
        // Expired: removed outright, no tombstone (F4).
        assert!(!inner.entries.contains_key("expired-waiting"));
        assert!(inner.order.is_empty());
        drop(inner);
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn total_records_are_bounded_even_after_decisions() {
        let presence = connected().await;
        for n in 0..128 {
            let id = format!("done-{n}");
            let task = spawn_confirm(&presence, request(&id, 60), CancellationToken::new());
            presence.next(Duration::from_secs(1)).await.unwrap();
            presence.decide(&id, true).unwrap();
            assert_eq!(task.await.unwrap(), PresenceOutcome::Allowed);
        }
        assert_eq!(
            presence
                .confirm(request("overflow", 60), CancellationToken::new())
                .await,
            PresenceOutcome::Unavailable
        );
        assert_eq!(presence.lock().entries.len(), 128);
        tokio::time::advance(Duration::from_secs(181)).await;
        presence.next(Duration::ZERO).await;
        assert!(presence.lock().entries.is_empty());
    }

    /// #1447 F4: cancelled or dropped confirmations leave no record, so a
    /// peer that elevates and disconnects over and over cannot make `confirm`
    /// answer `Unavailable` for everyone.
    #[tokio::test(start_paused = true)]
    async fn cancelled_and_dropped_confirmations_free_their_records() {
        let presence = connected().await;
        for n in 0..(MAX_RECORDS * 2) {
            let id = format!("gone-{n}");
            let cancel = CancellationToken::new();
            let task = spawn_confirm(&presence, request(&id, 60), cancel.clone());
            presence.next(Duration::from_secs(1)).await.unwrap();
            if n % 2 == 0 {
                cancel.cancel();
                assert_eq!(task.await.unwrap(), PresenceOutcome::TimedOut);
            } else {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            }
            assert_eq!(presence.decide(&id, true), Err(DecideError::Gone));
        }
        assert!(presence.lock().entries.is_empty());
        // No time has passed, yet a fresh challenge is still accepted.
        let task = spawn_confirm(&presence, request("fresh", 60), CancellationToken::new());
        assert_eq!(
            presence
                .next(Duration::from_secs(1))
                .await
                .unwrap()
                .challenge_id,
            "fresh"
        );
        presence.decide("fresh", true).unwrap();
        assert_eq!(task.await.unwrap(), PresenceOutcome::Allowed);
    }

    #[tokio::test]
    async fn absolute_expiry_rejects_decision_before_rounded_monotonic_deadline() {
        let presence = connected().await;
        let task = spawn_confirm(
            &presence,
            request("wall-expiry", 60),
            CancellationToken::new(),
        );
        presence.next(Duration::from_secs(1)).await.unwrap();
        // Simulate the wall clock crossing expires_at while the monotonic deadline is still ahead.
        presence
            .lock()
            .entries
            .get_mut("wall-expiry")
            .unwrap()
            .req
            .expires_at = unix_now();
        assert_eq!(presence.decide("wall-expiry", true), Err(DecideError::Gone));
        assert_eq!(task.await.unwrap(), PresenceOutcome::TimedOut);
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

    /// #1447 F1, second layer: `gate` refuses a real first-party shell
    /// authorization on its own, independent of the operation filter.
    #[tokio::test]
    async fn gate_refuses_a_first_party_authorization() {
        let (state, _, _) = crate::server::tests::capability_state().await;
        let token = state
            .first_party
            .as_ref()
            .unwrap()
            .issue(&crate::capabilities::first_party::test_peer("daemon-test"))
            .unwrap();
        let auth = state
            .capabilities
            .as_ref()
            .unwrap()
            .validate_bearer(
                &token.secret,
                Operation::ConfigRead,
                &crate::capabilities::Resource::global(),
                unix_now(),
            )
            .unwrap();
        assert_eq!(auth.claims.audience, Audience::FirstPartyShell);
        let refused = gate(&state, Some(Extension(auth))).unwrap();
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn the_routes_do_not_exist_in_legacy_mode() {
        let state = crate::server::tests::state().await;
        let token = state.token.to_string();
        let router = build_router(state);
        for method in [
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
            Method::HEAD,
        ] {
            let unknown = call(
                &router,
                method.clone(),
                "/api/v1/unknown-presence",
                &token,
                Some(ALLOW),
            )
            .await;
            for uri in [NEXT, DECIDE] {
                assert_eq!(
                    call(&router, method.clone(), uri, &token, Some(ALLOW)).await,
                    unknown,
                    "{method} {uri}"
                );
            }
        }
    }

    #[tokio::test]
    async fn active_long_polls_are_bounded_and_aborting_releases_slots() {
        let (state, host, _) = crate::server::tests::capability_state().await;
        let presence = state.presence_desktop.clone().unwrap();
        let router = build_router(state);
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let router = router.clone();
            let host = host.clone();
            tasks.push(tokio::spawn(async move {
                call(
                    &router,
                    Method::GET,
                    "/api/v1/presence/challenges/next?wait=25",
                    &host,
                    None,
                )
                .await
            }));
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while presence.polls.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            call(&router, Method::GET, NEXT, &host, None).await.0,
            StatusCode::TOO_MANY_REQUESTS
        );
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
        assert_eq!(
            call(&router, Method::GET, NEXT, &host, None).await.0,
            StatusCode::NO_CONTENT
        );
    }

    #[tokio::test]
    async fn http_round_trip_first_decision_wins_then_409_then_410() {
        let (state, host, _) = crate::server::tests::capability_state().await;
        let presence = state.presence_desktop.clone().unwrap();
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
        let presence = state.presence_desktop.clone().unwrap();
        let router = build_router(state);
        assert!(presence.next(Duration::ZERO).await.is_none());
        let task = spawn_confirm(&presence, request("z1", 1), CancellationToken::new());
        assert_eq!(task.await.unwrap(), PresenceOutcome::TimedOut);
        let uri = "/api/v1/presence/challenges/z1";
        let (status, _) = call(&router, Method::POST, uri, &host, Some(ALLOW)).await;
        assert_eq!(status, StatusCode::GONE);
    }
}
