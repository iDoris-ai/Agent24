//! `ModuleApprovalBroker` (T7b/ME-3e) — the single owner of `module_approvals`
//! writes and WS pushes, shared by:
//! - the out-of-process wire handlers (`crate::approval_callback`,
//!   `_a24/approval/gate`/`advise`/`status`),
//! - the in-process `PolicyApprovalBackend` (`crate::domain`),
//! - the REST endpoints (`crate::module_approvals`),
//! - the periodic timeout scan spawned from `server.rs`.
//!
//! See `docs/design/T7b-ME3e-approvals.md`, decisions 3-6.

use std::sync::Arc;

use agent24_protocol::{
    ApprovalAnswer, ApprovalRequestError, EventBody, ModuleApproval, ModuleApprovalDecision,
    ModuleApprovalKind,
};
use agent24_store::{Store, StoreError};
use tokio_util::sync::CancellationToken;

/// How long a submitted approval stays `Pending` before the periodic scan
/// times it out (design doc decision 5). Deliberately NOT
/// `A24_APPROVAL_TIMEOUT_SECS` — that env var governs the unrelated,
/// `run_id`-tied `agent24-policy::ApprovalBroker` table (design doc "现状" 4:
/// "本轮明确不照抄这套阻塞机制").
const MODULE_APPROVAL_TTL_SECS: u64 = 300;

/// How often the periodic scan runs (design doc decision 5: "比如每 10 秒跑
/// 一次，具体间隔留给实现阶段").
const SCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// The kernel-executable action closed set (design doc decisions 4 and 6) —
/// EMPTY this round (T7b scope, see the design doc's opening section): every
/// `gate` submission hits this and is `forbidden` before a row is ever
/// written. Shared by the wire handler and `PolicyApprovalBackend` so the two
/// paths can never disagree about what is in the closed set. T7c delivers
/// the first non-empty entry.
pub fn check_closed_set(_action: &str) -> Result<(), ApprovalRequestError> {
    Err(ApprovalRequestError::ActionNotInClosedSet)
}

/// Mint a 32-byte random, hex-encoded id (design doc decision 3:
/// `approval_id` is NOT a ULID and NOT derived from `request_id` or anything
/// else predictable). Hard-fails on missing entropy rather than falling back
/// to a weaker source — the same precedent as
/// `agent24_os_proto::launch::mint_token`.
fn mint_id() -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Injectable time source (mirrors `crate::events_emit::Clock`'s reasoning):
/// production reads the wall clock; a test drives it explicitly so judgement
/// 14/15/16 (timeout scan, decision-CAS-checks-expiry, scan-survives-an-error)
/// do not depend on real sleeps.
pub trait Clock: Send + Sync {
    fn now_epoch_secs(&self) -> u64;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_epoch_secs(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

fn now_iso(clock: &dyn Clock) -> String {
    agent24_core::util::iso8601_from_epoch_secs(clock.now_epoch_secs())
}

fn now_plus_iso(clock: &dyn Clock, secs: u64) -> String {
    agent24_core::util::iso8601_from_epoch_secs(clock.now_epoch_secs().saturating_add(secs))
}

fn to_answer(row: &ModuleApproval) -> ApprovalAnswer {
    ApprovalAnswer {
        approval_id: row.id.clone(),
        kind: row.kind,
        binding: row.binding,
        decision: row.decision,
    }
}

pub struct ModuleApprovalBroker {
    store: Store,
    events: crate::events::EventsHub,
    clock: Arc<dyn Clock>,
}

impl ModuleApprovalBroker {
    #[must_use]
    pub fn new(store: Store, events: crate::events::EventsHub) -> Arc<Self> {
        Self::with_clock(store, events, Arc::new(SystemClock))
    }

    #[must_use]
    pub fn with_clock(
        store: Store,
        events: crate::events::EventsHub,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            events,
            clock,
        })
    }

    /// The submit flow (design doc decision 4, steps 3/5/6) — EXCLUDING the
    /// capability check, the closed-set check (for `Gate`), and (for the
    /// out-of-process path) token admission: those are the CALLER's job,
    /// because only the caller knows which checks apply to its path (the
    /// wire handler has a `Generation` and a token to admit; the in-process
    /// `PolicyApprovalBackend` has neither).
    ///
    /// Step 3 (idempotent lookup) runs first and unconditionally: a resubmit
    /// with the same `(module, request_id, kind)` returns the existing row
    /// untouched, never re-validating anything or creating a second row.
    ///
    /// Split from [`Self::insert`] because the OUT-OF-PROCESS wire handler
    /// needs a checkpoint BETWEEN the two: `Generation::admit_approval_callback`
    /// (token admission) must run only when this lookup finds nothing, and
    /// only before [`Self::insert`] — never re-run on a resubmit that hits an
    /// existing row. The in-process path (no token to admit) composes both
    /// through [`Self::submit`] instead.
    pub async fn find_existing(
        &self,
        module: &str,
        request_id: &str,
        kind: ModuleApprovalKind,
    ) -> Result<Option<ApprovalAnswer>, ApprovalRequestError> {
        self.store
            .find_module_approval(module, request_id, kind)
            .await
            .map(|opt| opt.as_ref().map(to_answer))
            .map_err(|e| ApprovalRequestError::BackendUnavailable(e.to_string()))
    }

    /// Insert a brand-new `Pending` row and push `module-approval.required`
    /// (design doc decision 4, step 5). The caller must already have done
    /// the capability check, the closed-set check (for `Gate`), the
    /// idempotency lookup ([`Self::find_existing`] returned `None`), and —
    /// for the out-of-process path — token admission. This method does not
    /// re-check any of that.
    pub async fn insert(
        &self,
        module: &str,
        request_id: &str,
        kind: ModuleApprovalKind,
        action: String,
        target: Option<String>,
        payload: serde_json::Value,
    ) -> Result<ApprovalAnswer, ApprovalRequestError> {
        let id = mint_id().map_err(|e| ApprovalRequestError::BackendUnavailable(e.to_string()))?;
        let digest = agent24_protocol::approval_digest(&payload);
        let created_at = now_iso(self.clock.as_ref());
        let expires_at = now_plus_iso(self.clock.as_ref(), MODULE_APPROVAL_TTL_SECS);
        // `binding` is always false: `kind == Advise` whenever this line runs
        // (a `Gate` submission always hits the empty closed set — checked by
        // the caller — before this method is ever called).
        let row = self
            .store
            .insert_module_approval(
                &id,
                module,
                request_id,
                kind,
                false,
                &action,
                target.as_deref(),
                &payload,
                &digest,
                &created_at,
                &expires_at,
            )
            .await
            .map_err(|e| ApprovalRequestError::BackendUnavailable(e.to_string()))?;
        self.events
            .broadcast(EventBody::ModuleApprovalRequired(Box::new(row.clone())));
        Ok(to_answer(&row))
    }

    /// The full submit flow — [`Self::find_existing`] then, if nothing was
    /// found, [`Self::insert`] — for callers with NO checkpoint to run in
    /// between (the in-process `PolicyApprovalBackend`; there is no token to
    /// admit for a module compiled into the daemon). The out-of-process wire
    /// handler calls the two methods directly instead, with
    /// `Generation::admit_approval_callback` run in between.
    pub async fn submit(
        &self,
        module: &str,
        request_id: &str,
        kind: ModuleApprovalKind,
        action: String,
        target: Option<String>,
        payload: serde_json::Value,
    ) -> Result<ApprovalAnswer, ApprovalRequestError> {
        if let Some(existing) = self.find_existing(module, request_id, kind).await? {
            return Ok(existing);
        }
        self.insert(module, request_id, kind, action, target, payload)
            .await
    }

    /// `status` (design doc decision 4's query path): scoped by `module` in
    /// the storage query itself, so "belongs to another module" and "does
    /// not exist" are indistinguishable (judgement 11/12).
    pub async fn status(
        &self,
        module: &str,
        approval_id: &str,
    ) -> Result<ApprovalAnswer, ApprovalRequestError> {
        match self
            .store
            .get_module_approval_for_module(module, approval_id)
            .await
        {
            Ok(Some(row)) => Ok(to_answer(&row)),
            Ok(None) => Err(ApprovalRequestError::NotFound),
            Err(e) => Err(ApprovalRequestError::BackendUnavailable(e.to_string())),
        }
    }

    /// The full record, for REST `GET /api/v1/module-approvals/{id}` — unlike
    /// [`Self::status`], not scoped to a module: REST is an operator surface,
    /// not a module's own wire call.
    pub async fn get(&self, id: &str) -> Result<Option<ModuleApproval>, StoreError> {
        self.store.get_module_approval(id).await
    }

    pub async fn list(
        &self,
        decision: Option<ModuleApprovalDecision>,
    ) -> Result<Vec<ModuleApproval>, StoreError> {
        self.store.list_module_approvals(decision).await
    }

    /// REST `decide`: the decision CAS (design doc decision 3), then a WS
    /// push the instant it lands — there is no separate "delivered" step in
    /// the async model, the decision itself is the terminal state.
    pub async fn decide(
        &self,
        id: &str,
        to: ModuleApprovalDecision,
    ) -> Result<ModuleApproval, StoreError> {
        let now = now_iso(self.clock.as_ref());
        let row = self.store.decide_module_approval(id, to, &now).await?;
        self.events.broadcast(EventBody::ModuleApprovalResolved {
            id: row.id.clone(),
            decision: row.decision,
        });
        Ok(row)
    }

    /// Design doc decision 5: a periodic scan, not a per-row timer, started
    /// once from `server.rs` alongside the daemon's existing
    /// `CancellationToken`/`tokio::spawn` pattern (see `server.rs`'s
    /// `tokio::spawn(scheduler.run(...))`). A single transient `Store` error
    /// is logged and the loop continues — never `?`/`.unwrap()` on one
    /// iteration's DB call, because the periodic interval IS the retry
    /// mechanism (design doc: "周期本身就是重试机制").
    pub fn spawn_scan(self: &Arc<Self>, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
        self.spawn_scan_every(SCAN_INTERVAL, cancel)
    }

    /// [`Self::spawn_scan`] with an explicit interval — production always
    /// uses [`SCAN_INTERVAL`]; a test uses a short one so judgement 16
    /// (the loop survives repeated errors and resumes) does not need real
    /// 10-second waits.
    fn spawn_scan_every(
        self: &Arc<Self>,
        interval: std::time::Duration,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let broker = Arc::clone(self);
        tokio::spawn(async move { broker.scan_loop(interval, cancel).await })
    }

    async fn scan_loop(&self, interval: std::time::Duration, cancel: CancellationToken) {
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(interval) => {}
            }
            self.scan_once().await;
        }
    }

    /// One scan pass — separated from [`Self::scan_loop`] so a test can
    /// trigger it directly, without waiting out a real interval (judgement
    /// 14/16).
    pub async fn scan_once(&self) {
        let now = now_iso(self.clock.as_ref());
        match self.store.timeout_expired_module_approvals(&now).await {
            Ok(ids) => {
                for id in ids {
                    self.events.broadcast(EventBody::ModuleApprovalResolved {
                        id,
                        decision: ModuleApprovalDecision::TimedOut,
                    });
                }
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "module approval timeout scan failed this cycle; will retry next interval"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::sync::Mutex;

    struct TestClock(Mutex<u64>);
    impl TestClock {
        fn at(secs: u64) -> Arc<Self> {
            Arc::new(Self(Mutex::new(secs)))
        }
        fn advance(&self, by: u64) {
            *self.0.lock().unwrap() += by;
        }
    }
    impl Clock for TestClock {
        fn now_epoch_secs(&self) -> u64 {
            *self.0.lock().unwrap()
        }
    }

    /// A `Store` whose every call fails, for the scan-survives-an-error
    /// judgement (16). Wraps a real in-memory store's pool via a closed
    /// connection is awkward to fabricate portably, so instead this test
    /// exercises `scan_once`'s error path through a store that never had the
    /// migration run for the table it needs — the simplest reliable way to
    /// force a `Store` error without mocking the trait (there is no
    /// `Store` trait to mock; it's a concrete `sqlx` wrapper).
    async fn broker(
        clock: Arc<dyn Clock>,
    ) -> (Arc<ModuleApprovalBroker>, crate::events::EventsHub) {
        let store = Store::open_memory().await.unwrap();
        let events = crate::events::EventsHub::default();
        (
            ModuleApprovalBroker::with_clock(store, events.clone(), clock),
            events,
        )
    }

    #[tokio::test]
    async fn submit_then_status_round_trips() {
        let (broker, _events) = broker(Arc::new(SystemClock)).await;
        let answer = broker
            .submit(
                "probe",
                "req-1",
                ModuleApprovalKind::Advise,
                "send_email".to_owned(),
                Some("ops@example.com".to_owned()),
                serde_json::json!({"body": "hi"}),
            )
            .await
            .unwrap();
        assert_eq!(answer.kind, ModuleApprovalKind::Advise);
        assert!(!answer.binding);
        assert_eq!(answer.decision, ModuleApprovalDecision::Pending);

        let status = broker.status("probe", &answer.approval_id).await.unwrap();
        assert_eq!(status.decision, ModuleApprovalDecision::Pending);
    }

    // ── judgement 16a: idempotent resubmission ────────────────────────────

    #[tokio::test]
    async fn resubmitting_the_same_request_id_is_idempotent() {
        let (broker, _events) = broker(Arc::new(SystemClock)).await;
        let first = broker
            .submit(
                "probe",
                "req-1",
                ModuleApprovalKind::Advise,
                "a".to_owned(),
                None,
                serde_json::json!({"x": 1}),
            )
            .await
            .unwrap();
        let second = broker
            .submit(
                "probe",
                "req-1",
                ModuleApprovalKind::Advise,
                "a".to_owned(),
                None,
                serde_json::json!({"x": 1}),
            )
            .await
            .unwrap();
        assert_eq!(first.approval_id, second.approval_id);

        let rows = broker.list(None).await.unwrap();
        assert_eq!(rows.len(), 1, "no second row was created");
    }

    // ── judgement 11/12: cross-module and nonexistent lookups agree ───────

    #[tokio::test]
    async fn status_hides_cross_module_and_nonexistent_ids_the_same_way() {
        let (broker, _events) = broker(Arc::new(SystemClock)).await;
        let answer = broker
            .submit(
                "probe",
                "req-1",
                ModuleApprovalKind::Advise,
                "a".to_owned(),
                None,
                serde_json::json!({}),
            )
            .await
            .unwrap();

        let cross_module = broker.status("someone-else", &answer.approval_id).await;
        let nonexistent = broker.status("probe", "totally-made-up").await;
        assert!(matches!(cross_module, Err(ApprovalRequestError::NotFound)));
        assert!(matches!(nonexistent, Err(ApprovalRequestError::NotFound)));
    }

    // ── judgement 14/15: periodic scan + decide both respect expires_at ───

    #[tokio::test]
    async fn a_pending_row_times_out_once_its_deadline_passes_and_decide_then_conflicts() {
        let clock = TestClock::at(0);
        let (broker, events) = broker(clock.clone()).await;
        let mut rx = events.subscribe();
        let answer = broker
            .submit(
                "probe",
                "req-1",
                ModuleApprovalKind::Advise,
                "a".to_owned(),
                None,
                serde_json::json!({}),
            )
            .await
            .unwrap();
        let _ = rx.recv().await.unwrap(); // module-approval.required

        // Advance PAST the TTL, then scan.
        clock.advance(MODULE_APPROVAL_TTL_SECS + 1);
        broker.scan_once().await;
        let (_ts, resolved_body) = rx.recv().await.unwrap();
        assert_eq!(resolved_body.wire_type(), "module-approval.resolved");

        let status = broker.status("probe", &answer.approval_id).await.unwrap();
        assert_eq!(status.decision, ModuleApprovalDecision::TimedOut);

        // The decision CAS also refuses an expired-but-still-pending row —
        // in this case the row is already `TimedOut`, so decide conflicts
        // for that reason too; the CAS's OWN `expires_at` check is exercised
        // directly at the store layer (agent24-store tests).
        let err = broker
            .decide(&answer.approval_id, ModuleApprovalDecision::Approved)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    // ── judgement 16: the scan loop survives a transient store error ──────

    #[tokio::test]
    async fn scan_once_on_an_unmigrated_store_logs_and_returns_without_panicking() {
        // A store whose pool has no `module_approvals` table at all — the
        // simplest reliable way to force `timeout_expired_module_approvals`
        // to fail without a mock `Store` trait (there is none; it's a
        // concrete sqlx wrapper). `scan_once` must not panic or propagate.
        let store = Store::open_memory().await.unwrap();
        sqlx::query("DROP TABLE module_approvals")
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        let events = crate::events::EventsHub::default();
        let broker = ModuleApprovalBroker::with_clock(store, events, Arc::new(SystemClock));
        // Must return normally (not panic) — proving the loop would still be
        // alive to try again next interval.
        broker.scan_once().await;
        broker.scan_once().await;
    }

    #[tokio::test]
    async fn the_scan_loop_survives_repeated_store_errors_and_resumes_after_recovery() {
        let store = Store::open_memory().await.unwrap();
        let events = crate::events::EventsHub::default();
        let mut rx = events.subscribe();
        let clock = TestClock::at(0);
        let broker = ModuleApprovalBroker::with_clock(store, events, clock.clone());

        // Break the table out from under the live pool.
        sqlx::query("DROP TABLE module_approvals")
            .execute(agent24_store::test_hooks::pool(&broker.store))
            .await
            .unwrap();

        let cancel = CancellationToken::new();
        let handle = broker.spawn_scan_every(std::time::Duration::from_millis(15), cancel.clone());

        // Several broken iterations must not kill the loop.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(
            !handle.is_finished(),
            "the scan loop exited after a transient store error instead of retrying"
        );

        // Recovery: reapply the SAME migration file (via `include_str!`, so
        // this can never drift from the real schema) to recreate the table,
        // then seed an already-expired `pending` row directly.
        const MIGRATION: &str =
            include_str!("../../../crates/agent24-store/migrations/0005_module_approvals.sql");
        sqlx::raw_sql(MIGRATION)
            .execute(agent24_store::test_hooks::pool(&broker.store))
            .await
            .unwrap();
        let epoch0 = agent24_core::util::iso8601_from_epoch_secs(0);
        broker
            .store
            .insert_module_approval(
                "apr_recovered",
                "probe",
                "req-1",
                ModuleApprovalKind::Advise,
                false,
                "a",
                None,
                &serde_json::json!({}),
                "sha256:x",
                &epoch0,
                &epoch0, // expires_at == epoch 0
            )
            .await
            .unwrap();
        // The clock must read STRICTLY past `expires_at` for the scan's
        // `expires_at < now` to pick the row up.
        clock.advance(1);

        // The very next tick (well within a couple of the 15ms intervals)
        // must pick it up — proof the loop kept ticking through the broken
        // period and is not merely "not yet crashed".
        let resolved = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("the loop did not resume scanning after recovery")
            .unwrap();
        assert_eq!(resolved.1.wire_type(), "module-approval.resolved");

        cancel.cancel();
        let _ = handle.await;
    }

    // ── judgement 13: two concurrent decides race, exactly one wins ───────

    #[tokio::test]
    async fn concurrent_approve_and_deny_yield_exactly_one_winner() {
        let (broker, _events) = broker(Arc::new(SystemClock)).await;
        let answer = broker
            .submit(
                "probe",
                "req-1",
                ModuleApprovalKind::Advise,
                "a".to_owned(),
                None,
                serde_json::json!({}),
            )
            .await
            .unwrap();
        let id = answer.approval_id;

        let a = {
            let broker = broker.clone();
            let id = id.clone();
            tokio::spawn(async move { broker.decide(&id, ModuleApprovalDecision::Approved).await })
        };
        let b = {
            let broker = broker.clone();
            let id = id.clone();
            tokio::spawn(async move { broker.decide(&id, ModuleApprovalDecision::Denied).await })
        };
        let (a, b) = tokio::join!(a, b);
        let (a, b) = (a.unwrap(), b.unwrap());
        let successes = [a.is_ok(), b.is_ok()].into_iter().filter(|ok| *ok).count();
        let conflicts = [&a, &b]
            .into_iter()
            .filter(|r| matches!(r, Err(StoreError::Conflict(_))))
            .count();
        assert_eq!(successes, 1, "exactly one of the two decides must win");
        assert_eq!(
            conflicts, 1,
            "the loser must see a conflict, not a silent overwrite"
        );
    }

    // ── judgement 20: payload/digest are computed once, at submission ─────

    #[tokio::test]
    async fn payload_and_its_digest_are_fixed_at_submission_and_readable_afterwards() {
        let (broker, _events) = broker(Arc::new(SystemClock)).await;
        let payload = serde_json::json!({"to": "ops@example.com", "body": "hi"});
        let answer = broker
            .submit(
                "probe",
                "req-1",
                ModuleApprovalKind::Advise,
                "send_email".to_owned(),
                None,
                payload.clone(),
            )
            .await
            .unwrap();
        let row = broker.get(&answer.approval_id).await.unwrap().unwrap();
        assert_eq!(
            row.payload, payload,
            "the stored payload is the real one, not just a hash"
        );
        assert_eq!(
            row.payload_digest,
            agent24_protocol::approval_digest(&payload)
        );
    }
}
