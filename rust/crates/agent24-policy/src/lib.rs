//! Agent24 approval system (C4).
//!
//! Fail-closed semantics throughout (SPEC-002 §1.4, ADR-026 hard constraint
//! #2): a pending approval resolves to a NEGATIVE outcome on every non-answer
//! path — timeout → `timed_out` (equivalent to deny), run cancellation /
//! dropped channel → `aborted`. The store row is authoritative; the in-memory
//! oneshot channel only wakes the waiting dispatch. Duplicate decisions lose
//! in the store's pending-only UPDATE and surface as 409s.
//!
//! Audit is double-written: full detail into the hash-chained audit table,
//! ids-only into logs (payloads never hit stderr).

pub mod guardian;
pub mod overrides;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use guardian::{AssessInput, Guardian, GuardianDecision};

use agent24_core::util::{now_iso8601, ulid};
use agent24_protocol::{Approval, ApprovalResolvedPayload, ApprovalStatus, Decision, EventBody};
use agent24_store::{Store, StoreError};
use serde_json::{Map, Value};
use std::sync::Mutex;

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use agent24_tools::{ApprovalGate, GateDecision, ToolContext, summarize_input};
use async_trait::async_trait;

/// Decision types the server offers on every C4 approval (open set,
/// server-driven — UIs render exactly this list).
pub const AVAILABLE_DECISIONS: [&str; 4] = ["approve", "approve_for_session", "deny", "abort"];

/// How a gated dispatch should proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Approved,
    /// Denied but the run continues (the reason goes back to the model)
    Denied(String),
    /// Deny AND cancel the whole run
    Aborted(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("approval {0} not found")]
    NotFound(String),
    #[error("approval {0} already resolved")]
    AlreadyResolved(String),
    #[error("invalid decision: {0}")]
    Invalid(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub struct ApprovalBroker {
    store: Store,
    emit: Arc<dyn Fn(EventBody) + Send + Sync>,
    /// approval_id → the waiting dispatch. std Mutex (never held across an
    /// await) so a Drop guard can clean it even when the waiting future is
    /// dropped mid-wait. The store row stays the single arbiter.
    pending: Mutex<HashMap<String, oneshot::Sender<Decision>>>,
    /// approve_for_session grants: (scope, tool) — scope is the session id,
    /// falling back to the run id for session-less runs. Bounded: on
    /// overflow the set is CLEARED (fail-closed — users get re-asked).
    grants: Mutex<HashSet<(String, String)>>,
    /// D3 auto-approver. When present, a low-risk verdict skips the human ask;
    /// absent (the default) means every gated call goes to a human.
    guardian: Option<Arc<Guardian>>,
    timeout: Duration,
}

const MAX_GRANTS: usize = 1024;

/// Removes the pending-map entry when the waiting future is dropped for any
/// reason — nothing may leak a sender (the watchdog then times the row out).
struct PendingGuard<'a> {
    broker: &'a ApprovalBroker,
    id: String,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut map) = self.broker.pending.lock() {
            map.remove(&self.id);
        }
    }
}

impl ApprovalBroker {
    pub fn new(
        store: Store,
        emit: Arc<dyn Fn(EventBody) + Send + Sync>,
        timeout: Duration,
    ) -> Arc<Self> {
        Self::with_guardian(store, emit, timeout, None)
    }

    /// Build a broker fronted by a [`Guardian`]. A low-risk verdict from the
    /// guardian auto-approves (audited, no human ask); every other outcome —
    /// high-risk, an unavailable/garbled model, or a hard-listed kind — falls
    /// through to the normal human approval flow.
    pub fn with_guardian(
        store: Store,
        emit: Arc<dyn Fn(EventBody) + Send + Sync>,
        timeout: Duration,
        guardian: Option<Arc<Guardian>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            emit,
            pending: Mutex::new(HashMap::new()),
            grants: Mutex::new(HashSet::new()),
            guardian,
            timeout,
        })
    }

    /// Append an audit record, propagating the store error. Used where the
    /// audit is a HARD precondition (an unattended auto-approval).
    async fn try_audit(&self, action: &str, detail: &Value) -> std::result::Result<(), StoreError> {
        self.store
            .append_audit(&now_iso8601(), "policy", action, detail)
            .await
            .map(|_| ())
    }

    async fn audit(&self, action: &str, detail: Value) {
        if let Err(err) = self.try_audit(action, &detail).await {
            tracing::error!("audit append failed: {err}");
        }
    }

    /// Block until the approval is decided (or fails closed). Called from the
    /// tool dispatch path via [`BrokerGate`].
    #[allow(clippy::too_many_arguments)]
    pub async fn request(
        &self,
        run_id: &str,
        session_id: Option<&str>,
        tool_call_id: &str,
        tool: &str,
        kind: &str,
        summary: String,
        payload: Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Verdict {
        let scope = session_id.unwrap_or(run_id).to_owned();
        let granted = self
            .grants
            .lock()
            .map(|g| g.contains(&(scope.clone(), tool.to_owned())))
            .unwrap_or(false);
        if granted {
            self.audit(
                "approval.auto_granted",
                serde_json::json!({ "run_id": run_id, "tool": tool, "scope": scope }),
            )
            .await;
            return Verdict::Approved;
        }

        // D3 Guardian: a low-risk verdict auto-approves (audited, no human ask);
        // every other outcome falls through to the human flow below. Fail-closed
        // — the guardian never DENIES here, only "approve now" or "ask a human".
        //
        // NOTE ON ORDERING: the guardian is consulted AFTER the session-grant
        // fast path above. A prior `approve_for_session` is an EXPLICIT human
        // pre-authorisation and outranks the model's per-call opinion, so a
        // granted (session, tool) never re-consults the guardian — by design.
        if let Some(guardian) = &self.guardian {
            let assess = AssessInput {
                tool,
                kind,
                summary: &summary,
                payload: &payload,
            };
            // Bound the assessment by the approval timeout: a stuck local model
            // must not hang the dispatch. Cancellation is already threaded into
            // evaluate(); this backstops a model that hangs without cancelling.
            // On timeout we fail closed to a human (AssessorUnavailable).
            let decision = match tokio::time::timeout(
                self.timeout,
                guardian.evaluate(&assess, cancel),
            )
            .await
            {
                Ok(decision) => decision,
                Err(_) => GuardianDecision::Escalate(guardian::Escalation::AssessorUnavailable(
                    "assessment timed out".to_owned(),
                )),
            };
            match decision {
                GuardianDecision::AutoApprove(assessment) => {
                    // No human saw this call, so the audit must prove WHAT was
                    // auto-approved — full payload, not just the summary (the
                    // human path's approval.required does the same). The audit
                    // is a HARD precondition: if it cannot be recorded we must
                    // NOT auto-approve — fall through to a human instead.
                    let detail = serde_json::json!({
                        "run_id": run_id, "tool": tool, "tool_call_id": tool_call_id,
                        "kind": kind, "risk_level": "low",
                        "rationale": assessment.rationale, "summary": summary,
                        "payload": payload,
                    });
                    match self.try_audit("approval.auto_approved", &detail).await {
                        Ok(()) => {
                            tracing::info!("guardian auto-approved {tool} for run {run_id}");
                            return Verdict::Approved;
                        }
                        Err(err) => {
                            // Fail-closed: no audit → no auto-approval. Best-effort
                            // record of the downgrade, then the human flow runs.
                            tracing::error!(
                                "guardian auto-approval audit failed ({err}); escalating to human"
                            );
                            self.audit(
                                "approval.guardian_escalated",
                                serde_json::json!({
                                    "run_id": run_id, "tool": tool, "tool_call_id": tool_call_id,
                                    "kind": kind, "reason": "audit_failed",
                                }),
                            )
                            .await;
                        }
                    }
                }
                GuardianDecision::Escalate(reason) => {
                    // Record WHY the guardian handed this to a human, then fall
                    // through to the normal pending-approval flow.
                    self.audit(
                        "approval.guardian_escalated",
                        serde_json::json!({
                            "run_id": run_id, "tool": tool, "tool_call_id": tool_call_id,
                            "kind": kind, "reason": reason.reason_code(),
                            "detail": reason.detail(),
                        }),
                    )
                    .await;
                    tracing::info!(
                        "guardian escalated {tool} for run {run_id}: {}",
                        reason.reason_code()
                    );
                }
            }
        }

        let now = now_iso8601();
        let approval = Approval {
            id: format!("apr_{}", ulid()),
            run_id: run_id.to_owned(),
            tool_call_id: tool_call_id.to_owned(),
            kind: kind.to_owned(),
            summary,
            payload,
            available_decisions: AVAILABLE_DECISIONS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            status: ApprovalStatus::Pending,
            decision: None,
            expires_at: agent24_core::util::iso8601_after(self.timeout),
            created_at: now,
            decided_at: None,
        };
        // Sender registered BEFORE the row becomes visible: a polling client
        // that resolves the instant the row appears always finds the sender.
        let (tx, mut rx) = oneshot::channel::<Decision>();
        if let Ok(mut map) = self.pending.lock() {
            map.insert(approval.id.clone(), tx);
        }
        let guard = PendingGuard {
            broker: self,
            id: approval.id.clone(),
        };
        if let Err(err) = self.store.insert_approval(&approval).await {
            tracing::error!("approval persist failed: {err}");
            drop(guard);
            // Fail closed: an approval that cannot be recorded is denied
            return Verdict::Denied("approval could not be recorded (fail-closed)".to_owned());
        }
        // Watchdog backstop: if this waiting future is dropped (task killed,
        // panic elsewhere) the row must still fail closed. The pending-only
        // UPDATE makes the normal paths win harmlessly (Conflict → no-op).
        {
            let store = self.store.clone();
            let emit = Arc::clone(&self.emit);
            let id = approval.id.clone();
            let run_id = run_id.to_owned();
            let after = self.timeout + Duration::from_secs(1);
            tokio::spawn(async move {
                tokio::time::sleep(after).await;
                if store
                    .resolve_approval(&id, ApprovalStatus::TimedOut, None, now_iso8601())
                    .await
                    .is_ok()
                {
                    tracing::warn!("approval {id} force-timed-out by watchdog");
                    (emit)(EventBody::ApprovalResolved(ApprovalResolvedPayload {
                        approval_id: id.clone(),
                        run_id: run_id.clone(),
                        decision_type: "timed_out".to_owned(),
                    }));
                    // The fail-closed backstop must be audited like every
                    // other resolution path (review C4 R2)
                    if let Err(err) = store
                        .append_audit(
                            &now_iso8601(),
                            "policy",
                            "approval.resolved",
                            &serde_json::json!({
                                "approval_id": id, "run_id": run_id,
                                "resolution": "timed_out", "via": "watchdog",
                            }),
                        )
                        .await
                    {
                        tracing::error!("watchdog audit append failed: {err}");
                    }
                }
            });
        }
        (self.emit)(EventBody::ApprovalRequired(Box::new(approval.clone())));
        self.audit(
            "approval.required",
            serde_json::json!({
                "approval_id": approval.id, "run_id": run_id,
                "tool": tool, "tool_call_id": tool_call_id,
                // Chain the full content: the audit log must prove WHAT was
                // asked, not merely that an id existed (review C4)
                "summary": approval.summary,
                "payload": approval.payload,
                "available_decisions": approval.available_decisions,
                "expires_at": approval.expires_at,
            }),
        )
        .await;
        tracing::info!("approval {} pending for run {run_id}", approval.id);

        let id = approval.id.clone();
        let verdict = tokio::select! {
            decision = &mut rx => match decision {
                Ok(decision) => self.apply_decision(&id, &scope, tool, decision).await,
                // Sender dropped without a decision — treat as abort
                Err(_) => Verdict::Aborted("approval channel dropped".to_owned()),
            },
            () = tokio::time::sleep(self.timeout) => {
                match self
                    .store
                    .resolve_approval(&id, ApprovalStatus::TimedOut, None, now_iso8601())
                    .await
                {
                    Ok(_) => {
                        self.broadcast_resolution(&id, run_id, "timed_out").await;
                        Verdict::Denied("approval timed out (fail-closed)".to_owned())
                    }
                    // A decision won the race against the timeout — the row
                    // (single arbiter) tells us which; never block on the
                    // channel here (fail-closed, review C4)
                    Err(StoreError::Conflict(_)) => self.verdict_from_row(&id, &scope, tool).await,
                    Err(err) => {
                        tracing::error!("approval timeout persist failed: {err}");
                        Verdict::Denied("approval store error (fail-closed)".to_owned())
                    }
                }
            }
            () = cancel.cancelled() => {
                match self
                    .store
                    .resolve_approval(&id, ApprovalStatus::Aborted, None, now_iso8601())
                    .await
                {
                    Ok(_) => self.broadcast_resolution(&id, run_id, "aborted").await,
                    Err(err) => tracing::debug!("approval abort persist skipped: {err}"),
                }
                Verdict::Aborted("run cancelled while approval pending".to_owned())
            }
        };
        drop(guard);
        verdict
    }

    /// Derive the verdict from the authoritative store row (used when a
    /// concurrent resolution won a race). Fail-closed on anything unexpected.
    async fn verdict_from_row(&self, id: &str, scope: &str, tool: &str) -> Verdict {
        match self.store.get_approval(id).await {
            Ok(Some(row)) => match row.status {
                ApprovalStatus::Approved => {
                    let kind = row.decision.as_ref().map(|d| d.kind.as_str());
                    if kind == Some("approve_for_session") {
                        self.grant(scope, tool);
                    }
                    Verdict::Approved
                }
                ApprovalStatus::Denied => Verdict::Denied(format!(
                    "denied by user: {}",
                    row.decision
                        .as_ref()
                        .and_then(|d| d.reason.as_deref())
                        .unwrap_or("no reason given")
                )),
                ApprovalStatus::Aborted => Verdict::Aborted("aborted by user".to_owned()),
                ApprovalStatus::TimedOut => {
                    Verdict::Denied("approval timed out (fail-closed)".to_owned())
                }
                ApprovalStatus::Pending => {
                    Verdict::Denied("approval in inconsistent state (fail-closed)".to_owned())
                }
            },
            other => {
                tracing::error!("approval {id} row unreadable after conflict: {other:?}");
                Verdict::Denied("approval store error (fail-closed)".to_owned())
            }
        }
    }

    fn grant(&self, scope: &str, tool: &str) {
        if let Ok(mut grants) = self.grants.lock() {
            if grants.len() >= MAX_GRANTS {
                // Fail-closed bound: clearing only means users get re-asked
                tracing::warn!("session-grant set full ({MAX_GRANTS}); clearing");
                grants.clear();
            }
            grants.insert((scope.to_owned(), tool.to_owned()));
        }
    }

    /// Interpret a decision that already won the store transition.
    async fn apply_decision(
        &self,
        approval_id: &str,
        scope: &str,
        tool: &str,
        decision: Decision,
    ) -> Verdict {
        match decision.kind.as_str() {
            "approve" => Verdict::Approved,
            "approve_for_session" => {
                self.grant(scope, tool);
                self.audit(
                    "approval.session_grant",
                    serde_json::json!({ "approval_id": approval_id, "scope": scope, "tool": tool }),
                )
                .await;
                Verdict::Approved
            }
            "deny" => Verdict::Denied(format!(
                "denied by user: {}",
                decision.reason.as_deref().unwrap_or("no reason given")
            )),
            "abort" => Verdict::Aborted("aborted by user".to_owned()),
            other => {
                // resolve() validates against available_decisions, so this is
                // unreachable in practice — fail closed anyway
                tracing::error!("approval {approval_id}: unknown decision type {other}");
                Verdict::Denied(format!("unknown decision type {other} (fail-closed)"))
            }
        }
    }

    async fn broadcast_resolution(&self, approval_id: &str, run_id: &str, resolution: &str) {
        (self.emit)(EventBody::ApprovalResolved(ApprovalResolvedPayload {
            approval_id: approval_id.to_owned(),
            run_id: run_id.to_owned(),
            decision_type: resolution.to_owned(),
        }));
        self.audit(
            "approval.resolved",
            serde_json::json!({ "approval_id": approval_id, "run_id": run_id, "resolution": resolution }),
        )
        .await;
        tracing::info!("approval {approval_id} resolved: {resolution}");
    }

    /// Apply a client decision (REST `POST /api/v1/approvals/{id}`).
    /// Store-first: the pending-only UPDATE is the single arbiter, so a
    /// duplicate/late decision surfaces as 409 and is discarded.
    pub async fn resolve(&self, id: &str, decision: Decision) -> Result<Approval, ResolveError> {
        let approval = self
            .store
            .get_approval(id)
            .await?
            .ok_or_else(|| ResolveError::NotFound(id.to_owned()))?;
        // 409 comes BEFORE decision validation: a bad decision against an
        // already-resolved approval is still "already resolved" per openapi
        if approval.status != ApprovalStatus::Pending {
            return Err(ResolveError::AlreadyResolved(id.to_owned()));
        }
        if !approval
            .available_decisions
            .iter()
            .any(|d| d == &decision.kind)
        {
            return Err(ResolveError::Invalid(format!(
                "decision type {} is not offered (available: {})",
                decision.kind,
                approval.available_decisions.join(", ")
            )));
        }
        if decision.kind == "deny" && decision.reason.as_deref().unwrap_or("").is_empty() {
            return Err(ResolveError::Invalid(
                "deny requires a non-empty reason".to_owned(),
            ));
        }
        let status = match decision.kind.as_str() {
            "approve" | "approve_for_session" => ApprovalStatus::Approved,
            "deny" => ApprovalStatus::Denied,
            "abort" => ApprovalStatus::Aborted,
            other => return Err(ResolveError::Invalid(format!("unknown decision {other}"))),
        };
        let resolved = self
            .store
            .resolve_approval(id, status, Some(&decision), now_iso8601())
            .await
            .map_err(|err| match err {
                StoreError::Conflict(_) => ResolveError::AlreadyResolved(id.to_owned()),
                StoreError::NotFound(_) => ResolveError::NotFound(id.to_owned()),
                other => ResolveError::Store(other),
            })?;
        // Wake the waiting dispatch (absent waiter is fine — the row rules)
        let tx = self.pending.lock().ok().and_then(|mut map| map.remove(id));
        if let Some(tx) = tx {
            let _ = tx.send(decision.clone());
        }
        self.broadcast_resolution(id, &resolved.run_id, &decision.kind)
            .await;
        Ok(resolved)
    }
}

/// The C4 gate: every requires-approval dispatch becomes a blocking approval
/// request through the broker.
pub struct BrokerGate {
    broker: Arc<ApprovalBroker>,
}

impl BrokerGate {
    pub fn new(broker: Arc<ApprovalBroker>) -> Self {
        Self { broker }
    }
}

#[async_trait]
impl ApprovalGate for BrokerGate {
    async fn check(
        &self,
        info: &agent24_protocol::ToolInfo,
        ctx: &ToolContext,
        input: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> GateDecision {
        let kind = match info.name.as_str() {
            "shell_exec" => "exec",
            "fs_write" => "fs_write",
            "http_fetch" => "network",
            _ => "module",
        };
        let summary = format!("{}: {}", info.name, summarize_input(input));
        let verdict = self
            .broker
            .request(
                &ctx.run_id,
                ctx.session_id.as_deref(),
                &ctx.tool_call_id,
                &info.name,
                kind,
                summary,
                input.clone(),
                cancel,
            )
            .await;
        match verdict {
            Verdict::Approved => GateDecision::Allow,
            Verdict::Denied(reason) => GateDecision::Deny(reason),
            Verdict::Aborted(reason) => GateDecision::AbortRun(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::sync::Mutex as StdMutex;

    struct Recorded(Arc<StdMutex<Vec<String>>>);

    async fn broker_with_timeout(timeout: Duration) -> (Arc<ApprovalBroker>, Recorded, Store) {
        broker_with(timeout, None).await
    }

    async fn broker_with(
        timeout: Duration,
        guardian: Option<Arc<Guardian>>,
    ) -> (Arc<ApprovalBroker>, Recorded, Store) {
        let store = Store::open_memory().await.unwrap();
        let events = Arc::new(StdMutex::new(Vec::new()));
        let ev = Arc::clone(&events);
        let emit: Arc<dyn Fn(EventBody) + Send + Sync> = Arc::new(move |body: EventBody| {
            if let Ok(mut v) = ev.lock() {
                v.push(body.wire_type().to_owned());
            }
        });
        (
            ApprovalBroker::with_guardian(store.clone(), emit, timeout, guardian),
            Recorded(events),
            store,
        )
    }

    struct FixedAssessor(guardian::RiskLevel);

    #[async_trait]
    impl guardian::RiskAssessor for FixedAssessor {
        async fn assess(
            &self,
            _input: &guardian::AssessInput<'_>,
            _cancel: &CancellationToken,
        ) -> std::result::Result<guardian::RiskAssessment, guardian::AssessError> {
            Ok(guardian::RiskAssessment {
                level: self.0,
                rationale: "fixed".to_owned(),
            })
        }
    }

    fn guardian_rating(level: guardian::RiskLevel) -> Arc<Guardian> {
        Arc::new(Guardian::new(Arc::new(FixedAssessor(level))))
    }

    /// An assessor that never returns in time — used to prove the broker bounds
    /// guardian latency and fails closed.
    struct HangingAssessor;

    #[async_trait]
    impl guardian::RiskAssessor for HangingAssessor {
        async fn assess(
            &self,
            _input: &guardian::AssessInput<'_>,
            _cancel: &CancellationToken,
        ) -> std::result::Result<guardian::RiskAssessment, guardian::AssessError> {
            // Far longer than any test's broker timeout; the timeout wrapper
            // must drop this future rather than wait it out.
            tokio::time::sleep(Duration::from_secs(3600)).await;
            unreachable!("hanging assessor should be cancelled by the broker timeout")
        }
    }

    async fn seed_run(store: &Store, id: &str) {
        store
            .insert_run(&agent24_protocol::Run {
                id: id.to_owned(),
                session_id: None,
                status: agent24_protocol::RunStatus::Running,
                input: agent24_protocol::RunInput {
                    prompt: "p".to_owned(),
                    model_override: None,
                },
                output: None,
                error: None,
                usage: agent24_protocol::Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    cost_usd: 0.0,
                },
                schedule_id: None,
                created_at: now_iso8601(),
                started_at: Some(now_iso8601()),
                ended_at: None,
            })
            .await
            .unwrap();
    }

    fn decision(kind: &str, reason: Option<&str>) -> Decision {
        Decision {
            kind: kind.to_owned(),
            reason: reason.map(str::to_owned),
            extra: Map::new(),
        }
    }

    /// Spawn a request(), then resolve it once the row is visible.
    async fn round_trip(kind: &'static str, reason: Option<&'static str>) -> (Verdict, Store) {
        let (broker, _events, store) = broker_with_timeout(Duration::from_secs(30)).await;
        seed_run(&store, "run_1").await;
        // (seed once; every round_trip uses run_1)
        let b = Arc::clone(&broker);
        let waiter = tokio::spawn(async move {
            b.request(
                "run_1",
                Some("sess_1"),
                "tc_1",
                "shell_exec",
                "exec",
                "shell_exec: {}".to_owned(),
                Map::new(),
                &CancellationToken::new(),
            )
            .await
        });
        let id = wait_for_pending(&store).await;
        broker.resolve(&id, decision(kind, reason)).await.unwrap();
        (waiter.await.unwrap(), store)
    }

    async fn wait_for_pending(store: &Store) -> String {
        for _ in 0..200 {
            let pending = store
                .list_approvals(Some(ApprovalStatus::Pending))
                .await
                .unwrap();
            if let Some(a) = pending.first() {
                return a.id.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no pending approval appeared");
    }

    #[tokio::test]
    async fn approve_resolves_the_waiting_dispatch() {
        let (verdict, store) = round_trip("approve", None).await;
        assert_eq!(verdict, Verdict::Approved);
        let all = store.list_approvals(None).await.unwrap();
        assert_eq!(all[0].status, ApprovalStatus::Approved);
        assert!(all[0].decided_at.is_some());
    }

    #[tokio::test]
    async fn deny_returns_the_reason_to_the_model() {
        let (verdict, store) = round_trip("deny", Some("too risky")).await;
        assert_eq!(
            verdict,
            Verdict::Denied("denied by user: too risky".to_owned())
        );
        let all = store.list_approvals(None).await.unwrap();
        assert_eq!(all[0].status, ApprovalStatus::Denied);
        assert_eq!(all[0].decision.as_ref().unwrap().kind, "deny");
    }

    #[tokio::test]
    async fn abort_aborts_the_run() {
        let (verdict, _store) = round_trip("abort", None).await;
        assert!(matches!(verdict, Verdict::Aborted(_)));
    }

    #[tokio::test]
    async fn deny_without_reason_is_invalid() {
        let (broker, _events, store) = broker_with_timeout(Duration::from_secs(30)).await;
        seed_run(&store, "run_1").await;
        let b = Arc::clone(&broker);
        let waiter = tokio::spawn(async move {
            b.request(
                "run_1",
                None,
                "tc_1",
                "fs_write",
                "fs_write",
                "s".to_owned(),
                Map::new(),
                &CancellationToken::new(),
            )
            .await
        });
        let id = wait_for_pending(&store).await;
        let err = broker
            .resolve(&id, decision("deny", None))
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::Invalid(_)), "{err}");
        // still pending — resolve with approve to unblock
        broker
            .resolve(&id, decision("approve", None))
            .await
            .unwrap();
        assert_eq!(waiter.await.unwrap(), Verdict::Approved);
    }

    #[tokio::test]
    async fn unknown_decision_type_is_invalid() {
        let (broker, _events, store) = broker_with_timeout(Duration::from_secs(30)).await;
        seed_run(&store, "run_1").await;
        let b = Arc::clone(&broker);
        let _waiter = tokio::spawn(async move {
            b.request(
                "run_1",
                None,
                "tc_1",
                "fs_write",
                "fs_write",
                "s".to_owned(),
                Map::new(),
                &CancellationToken::new(),
            )
            .await
        });
        let id = wait_for_pending(&store).await;
        let err = broker
            .resolve(&id, decision("approve_and_remember", None))
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::Invalid(_)), "{err}");
    }

    #[tokio::test]
    async fn duplicate_decision_is_a_conflict() {
        let (broker, _events, store) = broker_with_timeout(Duration::from_secs(30)).await;
        seed_run(&store, "run_1").await;
        let b = Arc::clone(&broker);
        let _waiter = tokio::spawn(async move {
            b.request(
                "run_1",
                None,
                "tc_1",
                "shell_exec",
                "exec",
                "s".to_owned(),
                Map::new(),
                &CancellationToken::new(),
            )
            .await
        });
        let id = wait_for_pending(&store).await;
        broker
            .resolve(&id, decision("approve", None))
            .await
            .unwrap();
        let err = broker
            .resolve(&id, decision("deny", Some("late")))
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::AlreadyResolved(_)), "{err}");
    }

    #[tokio::test]
    async fn resolved_approval_beats_decision_validation_with_409() {
        // openapi: an already-resolved approval is 409 even for a decision
        // type that would otherwise be invalid
        let (broker, _events, store) = broker_with_timeout(Duration::from_secs(30)).await;
        seed_run(&store, "run_1").await;
        let b = Arc::clone(&broker);
        let _waiter = tokio::spawn(async move {
            b.request(
                "run_1",
                None,
                "tc_1",
                "shell_exec",
                "exec",
                "s".to_owned(),
                Map::new(),
                &CancellationToken::new(),
            )
            .await
        });
        let id = wait_for_pending(&store).await;
        broker
            .resolve(&id, decision("approve", None))
            .await
            .unwrap();
        let err = broker
            .resolve(&id, decision("definitely_not_offered", None))
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::AlreadyResolved(_)), "{err}");
    }

    #[tokio::test]
    async fn timeout_fails_closed_as_timed_out() {
        let (broker, events, store) = broker_with_timeout(Duration::from_millis(100)).await;
        seed_run(&store, "run_1").await;
        let verdict = broker
            .request(
                "run_1",
                None,
                "tc_1",
                "shell_exec",
                "exec",
                "s".to_owned(),
                Map::new(),
                &CancellationToken::new(),
            )
            .await;
        assert!(
            matches!(verdict, Verdict::Denied(ref r) if r.contains("timed out")),
            "{verdict:?}"
        );
        let all = store.list_approvals(None).await.unwrap();
        assert_eq!(all[0].status, ApprovalStatus::TimedOut);
        let seen = events.0.lock().unwrap().clone();
        assert_eq!(seen, vec!["approval.required", "approval.resolved"]);
    }

    #[tokio::test]
    async fn run_cancellation_aborts_a_pending_approval() {
        let (broker, _events, store) = broker_with_timeout(Duration::from_secs(30)).await;
        seed_run(&store, "run_1").await;
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c.cancel();
        });
        let verdict = broker
            .request(
                "run_1",
                None,
                "tc_1",
                "shell_exec",
                "exec",
                "s".to_owned(),
                Map::new(),
                &cancel,
            )
            .await;
        assert!(matches!(verdict, Verdict::Aborted(_)), "{verdict:?}");
        let all = store.list_approvals(None).await.unwrap();
        assert_eq!(all[0].status, ApprovalStatus::Aborted);
    }

    #[tokio::test]
    async fn approve_for_session_grants_skip_the_next_ask() {
        let (broker, _events, store) = broker_with_timeout(Duration::from_secs(30)).await;
        seed_run(&store, "run_1").await;
        let b = Arc::clone(&broker);
        let waiter = tokio::spawn(async move {
            b.request(
                "run_1",
                Some("sess_1"),
                "tc_1",
                "shell_exec",
                "exec",
                "s".to_owned(),
                Map::new(),
                &CancellationToken::new(),
            )
            .await
        });
        let id = wait_for_pending(&store).await;
        broker
            .resolve(&id, decision("approve_for_session", None))
            .await
            .unwrap();
        assert_eq!(waiter.await.unwrap(), Verdict::Approved);

        // Second ask in the same session: instant approve, NO new row
        seed_run(&store, "run_2").await;
        let verdict = broker
            .request(
                "run_2",
                Some("sess_1"),
                "tc_2",
                "shell_exec",
                "exec",
                "s".to_owned(),
                Map::new(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(verdict, Verdict::Approved);
        assert_eq!(store.list_approvals(None).await.unwrap().len(), 1);

        let audits = store.list_audit().await.unwrap();
        assert!(audits.iter().any(|a| a.action == "approval.auto_granted"));
        store.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    async fn grants_are_scoped_to_session_and_tool() {
        // An approve_for_session grant must NOT leak across sessions or tools:
        // a different session, or a different tool in the same session, still
        // creates a fresh pending approval (review C4 grant-scoping).
        let (broker, _events, store) = broker_with_timeout(Duration::from_millis(80)).await;
        seed_run(&store, "run_1").await;
        let b = Arc::clone(&broker);
        let waiter = tokio::spawn(async move {
            b.request(
                "run_1",
                Some("sess_1"),
                "tc_1",
                "shell_exec",
                "exec",
                "s".to_owned(),
                Map::new(),
                &CancellationToken::new(),
            )
            .await
        });
        let id = wait_for_pending(&store).await;
        broker
            .resolve(&id, decision("approve_for_session", None))
            .await
            .unwrap();
        assert_eq!(waiter.await.unwrap(), Verdict::Approved);

        // Different SESSION, same tool → not granted → asks (times out closed)
        seed_run(&store, "run_2").await;
        let other_session = broker
            .request(
                "run_2",
                Some("sess_2"),
                "tc_2",
                "shell_exec",
                "exec",
                "s".to_owned(),
                Map::new(),
                &CancellationToken::new(),
            )
            .await;
        assert!(
            matches!(other_session, Verdict::Denied(_)),
            "a different session must still be asked: {other_session:?}"
        );

        // Same session, different TOOL → not granted → asks (times out closed)
        seed_run(&store, "run_3").await;
        let other_tool = broker
            .request(
                "run_3",
                Some("sess_1"),
                "tc_3",
                "fs_write",
                "fs_write",
                "s".to_owned(),
                Map::new(),
                &CancellationToken::new(),
            )
            .await;
        assert!(
            matches!(other_tool, Verdict::Denied(_)),
            "a different tool must still be asked: {other_tool:?}"
        );
    }

    #[tokio::test]
    async fn guardian_low_risk_auto_approves_without_a_human() {
        let (broker, events, store) = broker_with(
            Duration::from_secs(30),
            Some(guardian_rating(guardian::RiskLevel::Low)),
        )
        .await;
        seed_run(&store, "run_1").await;
        let mut payload = Map::new();
        payload.insert("path".to_owned(), serde_json::json!("/tmp/x"));
        let verdict = broker
            .request(
                "run_1",
                Some("sess_1"),
                "tc_1",
                "fs_write",
                "fs_write",
                "fs_write: /tmp/x".to_owned(),
                payload,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(verdict, Verdict::Approved);
        // No human was asked: no approval row, no ApprovalRequired event.
        assert!(store.list_approvals(None).await.unwrap().is_empty());
        assert!(events.0.lock().unwrap().is_empty());
        // The auto-approval is audited with rationale AND the full payload (no
        // human saw it), and the hash chain holds.
        let audits = store.list_audit().await.unwrap();
        let rec = audits
            .iter()
            .find(|a| a.action == "approval.auto_approved")
            .expect("auto_approved audit");
        assert_eq!(rec.detail["risk_level"], "low");
        assert_eq!(rec.detail["payload"]["path"], "/tmp/x");
        store.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    async fn guardian_high_risk_escalates_to_a_human() {
        // High risk → fall through to the human flow. With no resolver it times
        // out (fail-closed), which proves a real pending approval was created.
        let (broker, events, store) = broker_with(
            Duration::from_millis(100),
            Some(guardian_rating(guardian::RiskLevel::High)),
        )
        .await;
        seed_run(&store, "run_1").await;
        let verdict = broker
            .request(
                "run_1",
                Some("sess_1"),
                "tc_1",
                "shell_exec",
                "exec",
                "shell_exec: rm -rf /".to_owned(),
                Map::new(),
                &CancellationToken::new(),
            )
            .await;
        assert!(
            matches!(verdict, Verdict::Denied(ref r) if r.contains("timed out")),
            "{verdict:?}"
        );
        // A human approval row was created and then failed closed.
        let all = store.list_approvals(None).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].status, ApprovalStatus::TimedOut);
        // The escalation reason is audited, and the human ask was emitted.
        let audits = store.list_audit().await.unwrap();
        assert!(
            audits
                .iter()
                .any(|a| a.action == "approval.guardian_escalated")
        );
        assert!(
            events
                .0
                .lock()
                .unwrap()
                .contains(&"approval.required".to_owned())
        );
        store.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    async fn guardian_slow_assessor_is_bounded_and_fails_closed() {
        // A hung assessor must not hang the dispatch: the broker timeout bounds
        // it, the guardian escalates, and the human flow then times out closed.
        let hanging = Arc::new(Guardian::new(Arc::new(HangingAssessor)));
        let (broker, _events, store) = broker_with(Duration::from_millis(80), Some(hanging)).await;
        seed_run(&store, "run_1").await;
        let verdict = broker
            .request(
                "run_1",
                Some("sess_1"),
                "tc_1",
                "shell_exec",
                "exec",
                "s".to_owned(),
                Map::new(),
                &CancellationToken::new(),
            )
            .await;
        assert!(
            matches!(verdict, Verdict::Denied(ref r) if r.contains("timed out")),
            "{verdict:?}"
        );
        // The escalation was audited as an assessor problem, and a human row
        // was created (proving we fell through, not silently approved).
        let audits = store.list_audit().await.unwrap();
        assert!(audits.iter().any(|a| {
            a.action == "approval.guardian_escalated"
                && a.detail["reason"] == "assessor_unavailable"
        }));
        assert_eq!(store.list_approvals(None).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn guardian_respects_session_grant_before_assessing() {
        // An approve_for_session grant short-circuits BEFORE the guardian, so a
        // granted tool never re-consults the model.
        let (broker, _events, store) = broker_with(
            Duration::from_secs(30),
            Some(guardian_rating(guardian::RiskLevel::High)),
        )
        .await;
        seed_run(&store, "run_1").await;
        let b = Arc::clone(&broker);
        let waiter = tokio::spawn(async move {
            b.request(
                "run_1",
                Some("sess_1"),
                "tc_1",
                "shell_exec",
                "exec",
                "s".to_owned(),
                Map::new(),
                &CancellationToken::new(),
            )
            .await
        });
        let id = wait_for_pending(&store).await;
        broker
            .resolve(&id, decision("approve_for_session", None))
            .await
            .unwrap();
        assert_eq!(waiter.await.unwrap(), Verdict::Approved);

        // Second call, same session+tool: granted → Approved with NO new row,
        // even though the guardian would have rated it high.
        seed_run(&store, "run_2").await;
        let verdict = broker
            .request(
                "run_2",
                Some("sess_1"),
                "tc_2",
                "shell_exec",
                "exec",
                "s".to_owned(),
                Map::new(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(verdict, Verdict::Approved);
        assert_eq!(store.list_approvals(None).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn resolve_unknown_id_is_not_found() {
        let (broker, _events, _store) = broker_with_timeout(Duration::from_secs(1)).await;
        let err = broker
            .resolve("apr_nope", decision("approve", None))
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::NotFound(_)), "{err}");
    }
}
