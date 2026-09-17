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
    ModuleApprovalKind, ModuleApprovalSubmitted,
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

/// The kernel-executable action closed set (design doc decisions 4 and 6).
/// T7b shipped this EMPTY (every `gate` submission was `forbidden` before a
/// row was ever written). T7c/ME-3e (`docs/design/T7c-ME3e-gate-execution.md`,
/// "闭集匹配") adds the first entry, `schedule_callback` — a one-shot RFC3339
/// callback, no cron/every. Matching is exact: no trim, no case-insensitive
/// compare (judgement 7 — `"Schedule_Callback"`/`" schedule_callback "` stay
/// `forbidden`).
///
/// Shared by the wire handler (`crate::approval_callback`) and
/// `PolicyApprovalBackend` (`crate::domain`) so the two paths can never
/// disagree about what is in the closed set OR about how `target` gets
/// canonicalized — both call THIS function, not their own copy (judgement
/// 16).
pub fn validate_gate_action(
    action: &str,
    target: Option<&str>,
) -> Result<CanonicalGateAction, ApprovalRequestError> {
    if action != "schedule_callback" {
        return Err(ApprovalRequestError::ActionNotInClosedSet);
    }
    let target = target.ok_or_else(|| {
        ApprovalRequestError::InvalidTarget(
            "schedule_callback requires a target timestamp".to_owned(),
        )
    })?;
    let target = canonicalize_schedule_target(target)?;
    Ok(CanonicalGateAction {
        action: action.to_owned(),
        target,
    })
}

/// What [`validate_gate_action`] hands back on success — `target` is already
/// [`canonicalize_schedule_target`]'s output, which the CALLER must persist
/// instead of whatever string the module originally sent (design doc
/// "闭集匹配").
pub struct CanonicalGateAction {
    pub action: String,
    pub target: String,
}

/// Validate AND reformat a caller-supplied `target` into the EXACT format
/// [`now_iso`] produces — `YYYY-MM-DDTHH:MM:SSZ`, UTC, whole seconds, `Z`
/// suffix — so the scan's string comparison against `now_iso()`'s own output
/// is equivalent to a real time comparison (design doc "时间规范化"). Accepts
/// any legal RFC3339 offset and any number of fractional-second digits;
/// sub-second precision is TRUNCATED (floored), never rounded — `12:00:00.9Z`
/// canonicalizes to `12:00:00Z`, not `12:00:01Z` (judgement 13). A leap
/// second (`:60`) is rejected as an invalid target, the same as any other
/// unparseable string — not specially accommodated.
///
/// Formats straight off the parsed `chrono::DateTime` (converted to UTC),
/// never through a `u64` epoch-seconds intermediate — a `u64` conversion
/// would reject every legal pre-1970 RFC3339 timestamp, and design doc
/// judgement 9 requires an already-past `target` (which includes
/// pre-epoch instants) to be ACCEPTED, not rejected as if it were malformed.
fn canonicalize_schedule_target(target: &str) -> Result<String, ApprovalRequestError> {
    let parsed = chrono::DateTime::parse_from_rfc3339(target).map_err(|e| {
        ApprovalRequestError::InvalidTarget(format!("target is not a valid RFC3339 timestamp: {e}"))
    })?;
    // chrono represents a leap second by pushing the nanosecond field past
    // 1_000_000_000 while keeping `.second()` at 59 — detect that encoding
    // and refuse it rather than silently normalizing it away.
    if parsed.timestamp_subsec_nanos() >= 1_000_000_000 {
        return Err(ApprovalRequestError::InvalidTarget(
            "leap seconds are not accepted in a target".to_owned(),
        ));
    }
    // `%S` prints the whole-second field only — the fractional part is
    // simply never emitted, which IS the truncation (flooring) judgement 13
    // requires; there is no rounding step to get wrong.
    Ok(parsed
        .with_timezone(&chrono::Utc)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string())
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
        // T7c/ME-3e: this is a HAND-WRITTEN field-by-field copy, not a
        // struct-to-struct mapping — `executed_at` must be listed explicitly
        // here or it silently stays `None` forever even though the stored
        // row has a real value (design doc "决策4" note, Codex round 2 M4).
        executed_at: row.executed_at.clone(),
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
        // T7c/ME-3e (Codex round 2 High 3): `binding` MUST track `kind` for
        // real now — T7b hardcoded `false` here because a `Gate` submission
        // could never reach this line (the closed set was empty, so the
        // caller's closed-set check always returned first). That premise no
        // longer holds: `schedule_callback` is the first `action` that
        // actually reaches `insert()` as a `Gate`.
        let binding = kind == ModuleApprovalKind::Gate;
        let row = self
            .store
            .insert_module_approval(
                &id,
                module,
                request_id,
                kind,
                binding,
                &action,
                target.as_deref(),
                &payload,
                &digest,
                &created_at,
                &expires_at,
            )
            .await
            .map_err(|e| ApprovalRequestError::BackendUnavailable(e.to_string()))?;
        // T7c/ME-3e (design doc criterion 18): the WS event carries a frozen
        // `ModuleApprovalSubmitted` snapshot, NOT the full `ModuleApproval` —
        // `executed_at` must never appear on the submission event.
        self.events
            .broadcast(EventBody::ModuleApprovalRequired(Box::new(
                ModuleApprovalSubmitted::from(&row),
            )));
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
    ///
    /// T7c/ME-3e adds a SECOND query (design doc "扩展 T7b 的周期扫描",
    /// Codex round 2 Medium 2): the two `match`es below are fully
    /// independent — one failing is logged and does not `?`/early-`return`,
    /// so it can never stop the other from running this same cycle
    /// (judgement 14). Neither shares a transaction with the other.
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
        // No `RETURNING`/per-row WS push here on purpose (design doc "这一步
        // 不需要 RETURNING/逐条推事件"): T7c does not add a WS event for
        // "executed" this round — `status` polling is enough, and there is
        // no per-id follow-up work the way the timeout branch above has.
        match self.store.execute_due_schedule_callbacks(&now).await {
            Ok(0) => {}
            Ok(n) => tracing::debug!(count = n, "executed {n} due schedule_callback approval(s)"),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "module approval schedule_callback execution scan failed this cycle; \
                     will retry next interval"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use sqlx::Row;
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

        // Recovery: reapply the SAME migration files (via `include_str!`, so
        // this can never drift from the real schema) to recreate the table —
        // BOTH `0005` (the table itself) and `0006` (T7c/ME-3e's
        // `executed_at` column + index), since `row_to_module_approval` now
        // reads `executed_at` unconditionally (design doc note on
        // `module_approval_broker.rs:525`-ish) — then seed an already-expired
        // `pending` row directly.
        const MIGRATION_0005: &str =
            include_str!("../../../crates/agent24-store/migrations/0005_module_approvals.sql");
        const MIGRATION_0006: &str = include_str!(
            "../../../crates/agent24-store/migrations/0006_module_approval_executed_at.sql"
        );
        sqlx::raw_sql(MIGRATION_0005)
            .execute(agent24_store::test_hooks::pool(&broker.store))
            .await
            .unwrap();
        sqlx::raw_sql(MIGRATION_0006)
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

    // ── T7c/ME-3e: `canonicalize_schedule_target` (judgement 13) ──────────

    #[test]
    fn the_same_real_instant_canonicalizes_byte_identically_regardless_of_offset_or_fraction_digits()
     {
        let expected = "2026-01-01T00:00:00Z";
        for input in [
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00+00:00",
            "2026-01-01T08:00:00+08:00",
            "2025-12-31T16:00:00-08:00",
            "2026-01-01T00:00:00.000Z",
            "2026-01-01T00:00:00.000000Z",
            "2026-01-01T00:00:00.000000000Z",
            // Truncated (floored), not rounded: .9 must NOT become :01.
            "2026-01-01T00:00:00.9Z",
            "2026-01-01T00:00:00.999999999Z",
        ] {
            assert_eq!(
                canonicalize_schedule_target(input).unwrap(),
                expected,
                "input {input:?} did not canonicalize to the expected instant"
            );
        }
    }

    #[test]
    fn a_leap_second_is_rejected_as_an_invalid_target() {
        assert!(matches!(
            canonicalize_schedule_target("2026-06-30T23:59:60Z"),
            Err(ApprovalRequestError::InvalidTarget(_))
        ));
    }

    #[test]
    fn garbage_targets_are_rejected_as_invalid_not_panics() {
        for bad in ["not-a-timestamp", "2026-13-40T99:99:99Z"] {
            assert!(
                matches!(
                    canonicalize_schedule_target(bad),
                    Err(ApprovalRequestError::InvalidTarget(_))
                ),
                "{bad:?} should have been rejected"
            );
        }
    }

    // ── judgement 9: a pre-1970 target is a legal RFC3339 timestamp and
    // must be ACCEPTED, not rejected as if malformed (Codex review) ────────
    #[test]
    fn a_pre_epoch_target_is_accepted_and_canonicalized_not_rejected() {
        assert_eq!(
            canonicalize_schedule_target("1969-12-31T23:59:59Z").unwrap(),
            "1969-12-31T23:59:59Z"
        );
        // Also exercise an offset + fractional digits pre-epoch, same as the
        // post-epoch equivalence test above, to prove the non-u64 formatting
        // path canonicalizes pre-epoch instants exactly like post-epoch ones.
        assert_eq!(
            canonicalize_schedule_target("1969-12-31T23:59:59.999+00:00").unwrap(),
            "1969-12-31T23:59:59Z"
        );
        assert_eq!(
            canonicalize_schedule_target("1900-01-01T00:00:00Z").unwrap(),
            "1900-01-01T00:00:00Z"
        );
    }

    // ── T7c/ME-3e: `validate_gate_action` (judgement 1/7/12) ───────────────

    #[test]
    fn schedule_callback_with_a_valid_target_is_accepted_and_canonicalized() {
        let canonical =
            validate_gate_action("schedule_callback", Some("2026-01-01T00:00:00.5Z")).unwrap();
        assert_eq!(canonical.action, "schedule_callback");
        assert_eq!(canonical.target, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn schedule_callback_without_a_target_is_invalid_target_not_forbidden() {
        // Judgement 12: missing `target` is "in the closed set, bad
        // arguments" (`InvalidTarget`/-32602), never `ActionNotInClosedSet`.
        assert!(matches!(
            validate_gate_action("schedule_callback", None),
            Err(ApprovalRequestError::InvalidTarget(_))
        ));
    }

    #[test]
    fn schedule_callback_with_an_unparseable_target_is_invalid_target() {
        assert!(matches!(
            validate_gate_action("schedule_callback", Some("whenever")),
            Err(ApprovalRequestError::InvalidTarget(_))
        ));
    }

    #[test]
    fn any_other_action_is_not_in_the_closed_set_exact_match_only() {
        // Judgement 7: no trim, no case-insensitive compare.
        for (action, target) in [
            ("transfer_funds", None),
            ("Schedule_Callback", Some("2026-01-01T00:00:00Z")),
            (" schedule_callback", Some("2026-01-01T00:00:00Z")),
            ("schedule_callback ", Some("2026-01-01T00:00:00Z")),
            ("SCHEDULE_CALLBACK", Some("2026-01-01T00:00:00Z")),
        ] {
            assert!(
                matches!(
                    validate_gate_action(action, target),
                    Err(ApprovalRequestError::ActionNotInClosedSet)
                ),
                "{action:?} should not be in the closed set"
            );
        }
    }

    // ── T7c/ME-3e: `insert()`'s `binding` derivation (judgement 11) ────────

    #[tokio::test]
    async fn insert_derives_binding_from_kind_gate_true_advise_false() {
        let (broker, _events) = broker(Arc::new(SystemClock)).await;
        let gate = broker
            .submit(
                "probe",
                "req-gate",
                ModuleApprovalKind::Gate,
                "schedule_callback".to_owned(),
                Some("2026-01-01T00:00:00Z".to_owned()),
                serde_json::json!({}),
            )
            .await
            .unwrap();
        assert!(gate.binding, "a Gate row must be binding");
        assert_eq!(gate.executed_at, None, "not executed until scanned");

        let advise = broker
            .submit(
                "probe",
                "req-advise",
                ModuleApprovalKind::Advise,
                "schedule_callback".to_owned(),
                Some("2026-01-01T00:00:00Z".to_owned()),
                serde_json::json!({}),
            )
            .await
            .unwrap();
        assert!(!advise.binding, "an Advise row must never be binding");
    }

    // ── T7c/ME-3e: end-to-end scan wiring (judgement 2/3/9/10/13/15) ───────

    #[tokio::test]
    async fn an_approved_schedule_callback_stays_unexecuted_until_its_target_is_reached() {
        // Clock starts strictly BEFORE `target` (epoch 500 vs. target's
        // epoch 1_000), so "approving alone does not execute it" and
        // "target == now counts as reached" (judgement 13) can both be
        // demonstrated without contradicting each other: the clock only
        // reaches the target's exact instant after the approval.
        let clock = TestClock::at(500);
        let (broker, events) = broker(clock.clone()).await;
        let mut rx = events.subscribe();
        // `target` deliberately given with an offset and a fractional digit
        // that is NOT epoch 1_000's canonical `now_iso` spelling — proving
        // this whole flow goes through `canonicalize_schedule_target`, not a
        // raw string compare. `ModuleApprovalBroker::submit`/`insert` are the
        // low-level primitive both real callers (the wire handler,
        // `PolicyApprovalBackend`) sit on top of — per their own doc
        // comments, canonicalization is the CALLER's job, so this test does
        // it explicitly here too, exactly as those callers do (their own
        // dedicated tests in `approval_callback.rs`/`domain.rs` prove they
        // actually call `validate_gate_action` before reaching this point).
        let target_instant = agent24_core::util::iso8601_from_epoch_secs(1_000);
        let raw_target = "1970-01-01T00:16:40.999+00:00"; // epoch 1000, +999ms
        let canonical = validate_gate_action("schedule_callback", Some(raw_target)).unwrap();
        assert_eq!(canonical.target, target_instant);
        let submitted = broker
            .submit(
                "probe",
                "req-1",
                ModuleApprovalKind::Gate,
                "schedule_callback".to_owned(),
                Some(canonical.target.clone()),
                serde_json::json!({}),
            )
            .await
            .unwrap();
        let _ = rx.recv().await.unwrap(); // module-approval.required

        let stored = broker.get(&submitted.approval_id).await.unwrap().unwrap();
        assert_eq!(
            stored.target.as_deref(),
            Some(target_instant.as_str()),
            "the stored target must be the canonicalized form"
        );

        // Judgement 2: pending, not yet approved — status keeps returning
        // executed_at: null no matter how many times the scan runs.
        broker.scan_once().await;
        let status = broker
            .status("probe", &submitted.approval_id)
            .await
            .unwrap();
        assert_eq!(status.executed_at, None);

        // Approve it (decision CAS) — this alone does not execute it: the
        // clock (epoch 500) has not yet reached `target` (epoch 1_000).
        broker
            .decide(&submitted.approval_id, ModuleApprovalDecision::Approved)
            .await
            .unwrap();
        let _ = rx.recv().await.unwrap(); // module-approval.resolved (decision)
        broker.scan_once().await;
        let status = broker
            .status("probe", &submitted.approval_id)
            .await
            .unwrap();
        assert_eq!(
            status.executed_at, None,
            "approving alone must not execute it — only the scan, once target is reached, does"
        );

        // Advance the clock to EXACTLY the target's instant — no further.
        clock.advance(500);

        // The clock is ALREADY at the target's exact instant (judgement 13:
        // `<=`, not `<`) — the very next scan must execute it, with no
        // further advance needed.
        broker.scan_once().await;
        let status = broker
            .status("probe", &submitted.approval_id)
            .await
            .unwrap();
        assert_eq!(status.executed_at.as_deref(), Some(target_instant.as_str()));
        // No extra WS event for "executed" this round (design doc: no
        // RETURNING/per-row push) — nothing else should have arrived.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn denied_and_pending_schedule_callbacks_are_never_executed_even_once_their_target_passes()
     {
        // Judgement 4/10 at the broker/scan_once level (store-level CAS
        // already covers this directly; this proves `scan_once` itself never
        // routes a denied or still-pending row into execution).
        let clock = TestClock::at(0);
        let (broker, _events) = broker(clock.clone()).await;
        let past_target = "1970-01-01T00:00:00Z"; // epoch 0 — already "due" at clock 0

        let denied = broker
            .submit(
                "probe",
                "req-denied",
                ModuleApprovalKind::Gate,
                "schedule_callback".to_owned(),
                Some(past_target.to_owned()),
                serde_json::json!({}),
            )
            .await
            .unwrap();
        broker
            .decide(&denied.approval_id, ModuleApprovalDecision::Denied)
            .await
            .unwrap();

        let pending = broker
            .submit(
                "probe",
                "req-pending",
                ModuleApprovalKind::Gate,
                "schedule_callback".to_owned(),
                Some(past_target.to_owned()),
                serde_json::json!({}),
            )
            .await
            .unwrap();

        broker.scan_once().await;

        let denied_status = broker.status("probe", &denied.approval_id).await.unwrap();
        assert_eq!(denied_status.executed_at, None);
        let pending_status = broker.status("probe", &pending.approval_id).await.unwrap();
        assert_eq!(pending_status.executed_at, None);
    }

    #[tokio::test]
    async fn a_fresh_brokers_very_first_scan_catches_an_old_due_approved_row_no_startup_sweep_needed()
     {
        // Judgement 15: the "daemon restarted" scenario is just an ordinary
        // scan against state nothing in memory remembers — a stateless
        // conditional query, run for the first time, already covers it.
        //
        // This is tested across TWO SEPARATE `ModuleApprovalBroker`
        // instances sharing the SAME underlying `Store` (`Store` is
        // `Clone` over one pool) — broker A creates and approves the row,
        // is then dropped entirely (simulating the daemon process exiting),
        // and broker B — constructed fresh afterwards, with no shared
        // in-memory state of its own — must catch it on ITS very first
        // scan. Reusing one broker object for both halves (as an earlier
        // version of this test did) would also pass if the row's
        // persistence were broken, because nothing would have forced a
        // reload from storage (Codex review, criterion 15).
        let clock = TestClock::at(10_000);
        let store = Store::open_memory().await.unwrap();
        let events = crate::events::EventsHub::default();
        let long_past_target = "1970-01-01T00:00:00Z";

        let submitted = {
            let broker_a =
                ModuleApprovalBroker::with_clock(store.clone(), events.clone(), clock.clone());
            let submitted = broker_a
                .submit(
                    "probe",
                    "req-1",
                    ModuleApprovalKind::Gate,
                    "schedule_callback".to_owned(),
                    Some(long_past_target.to_owned()),
                    serde_json::json!({}),
                )
                .await
                .unwrap();
            broker_a
                .decide(&submitted.approval_id, ModuleApprovalDecision::Approved)
                .await
                .unwrap();
            submitted
            // `broker_a` is dropped here — nothing about broker B below
            // reuses this instance.
        };

        let broker_b = ModuleApprovalBroker::with_clock(store, events, clock);
        // The VERY FIRST call to `scan_once` on broker B.
        broker_b.scan_once().await;

        let status = broker_b
            .status("probe", &submitted.approval_id)
            .await
            .unwrap();
        assert!(
            status.executed_at.is_some(),
            "a freshly-constructed broker's first scan must pick up a row an \
             earlier, now-dropped broker instance left approved and due"
        );
    }

    // ── judgement 9 (end-to-end): a pre-1970 target is accepted at submit
    // time AND actually gets executed by the scan, not just canonicalized
    // in isolation (Codex review — the unit-level canonicalization test
    // above does not by itself prove the scan's SQL string comparison
    // still works for a pre-epoch value) ──────────────────────────────────
    #[tokio::test]
    async fn a_pre_epoch_target_is_accepted_at_submit_and_executed_by_the_scan() {
        let clock = TestClock::at(0); // "now" == 1970-01-01T00:00:00Z
        let (broker, _events) = broker(clock.clone()).await;
        let pre_epoch_target = "1969-12-31T23:59:59Z";

        let submitted = broker
            .submit(
                "probe",
                "req-1",
                ModuleApprovalKind::Gate,
                "schedule_callback".to_owned(),
                Some(pre_epoch_target.to_owned()),
                serde_json::json!({}),
            )
            .await
            .expect("a past, pre-epoch target must be accepted at submission time");
        let stored = broker.get(&submitted.approval_id).await.unwrap().unwrap();
        assert_eq!(stored.target.as_deref(), Some(pre_epoch_target));

        broker
            .decide(&submitted.approval_id, ModuleApprovalDecision::Approved)
            .await
            .unwrap();

        // "now" (epoch 0) is already past the pre-epoch target — the very
        // next scan must execute it.
        broker.scan_once().await;

        let status = broker
            .status("probe", &submitted.approval_id)
            .await
            .unwrap();
        assert_eq!(
            status.executed_at.as_deref(),
            Some("1970-01-01T00:00:00Z"),
            "a pre-1970 target must be executed once its instant has passed, \
             same as any other already-past target"
        );
    }

    // ── judgement 8: concurrent scans race, exactly one executes the row ──

    #[tokio::test]
    async fn two_concurrent_execution_scans_execute_a_due_row_exactly_once() {
        let clock = TestClock::at(0);
        let (broker, _events) = broker(clock.clone()).await;
        let submitted = broker
            .submit(
                "probe",
                "req-1",
                ModuleApprovalKind::Gate,
                "schedule_callback".to_owned(),
                Some("1970-01-01T00:00:00Z".to_owned()),
                serde_json::json!({}),
            )
            .await
            .unwrap();
        broker
            .decide(&submitted.approval_id, ModuleApprovalDecision::Approved)
            .await
            .unwrap();

        let now = now_iso(clock.as_ref());
        let a = {
            let store = broker.store.clone();
            let now = now.clone();
            tokio::spawn(async move { store.execute_due_schedule_callbacks(&now).await.unwrap() })
        };
        let b = {
            let store = broker.store.clone();
            let now = now.clone();
            tokio::spawn(async move { store.execute_due_schedule_callbacks(&now).await.unwrap() })
        };
        let (a, b) = tokio::join!(a, b);
        let total = a.unwrap() + b.unwrap();
        assert_eq!(
            total, 1,
            "exactly one of the two concurrent scans must have executed the row"
        );

        let status = broker
            .status("probe", &submitted.approval_id)
            .await
            .unwrap();
        assert_eq!(status.executed_at.as_deref(), Some(now.as_str()));
    }

    // ── judgement 14: the two scan queries fail and succeed independently ──

    #[tokio::test]
    async fn the_execution_scan_can_fail_while_the_timeout_scan_still_succeeds() {
        let clock = TestClock::at(0);
        let (broker, events) = broker(clock.clone()).await;
        let mut rx = events.subscribe();

        // An expired PENDING row for the timeout scan to catch.
        let expiring = broker
            .submit(
                "probe",
                "req-expiring",
                ModuleApprovalKind::Advise,
                "a".to_owned(),
                None,
                serde_json::json!({}),
            )
            .await
            .unwrap();
        let _ = rx.recv().await.unwrap(); // module-approval.required
        clock.advance(MODULE_APPROVAL_TTL_SECS + 1);

        // Break ONLY the execution query: drop `target` (and its own
        // partial index, which references it) — the timeout query never
        // references `target` at all.
        let pool = agent24_store::test_hooks::pool(&broker.store);
        sqlx::query("DROP INDEX idx_module_approvals_pending_schedule")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("ALTER TABLE module_approvals DROP COLUMN target")
            .execute(pool)
            .await
            .unwrap();

        broker.scan_once().await;

        let (_ts, resolved) = rx.recv().await.unwrap();
        assert_eq!(resolved.wire_type(), "module-approval.resolved");
        let row = sqlx::query("SELECT decision FROM module_approvals WHERE id = ?")
            .bind(&expiring.approval_id)
            .fetch_one(agent24_store::test_hooks::pool(&broker.store))
            .await
            .unwrap();
        let decision: String = row.get("decision");
        assert_eq!(
            decision, "timed_out",
            "the timeout scan must still have succeeded even though the execution \
             scan's query failed this same cycle"
        );
    }

    #[tokio::test]
    async fn the_timeout_scan_can_fail_while_the_execution_scan_still_succeeds() {
        let clock = TestClock::at(0);
        let (broker, _events) = broker(clock.clone()).await;

        let submitted = broker
            .submit(
                "probe",
                "req-1",
                ModuleApprovalKind::Gate,
                "schedule_callback".to_owned(),
                Some("1970-01-01T00:00:00Z".to_owned()),
                serde_json::json!({}),
            )
            .await
            .unwrap();
        broker
            .decide(&submitted.approval_id, ModuleApprovalDecision::Approved)
            .await
            .unwrap();

        // Break ONLY the timeout query: drop its own dedicated partial index
        // and the `expires_at` column it filters on — the execution query
        // never references `expires_at` at all.
        let pool = agent24_store::test_hooks::pool(&broker.store);
        sqlx::query("DROP INDEX idx_module_approvals_pending_expiry")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("ALTER TABLE module_approvals DROP COLUMN expires_at")
            .execute(pool)
            .await
            .unwrap();

        broker.scan_once().await;

        let row = sqlx::query("SELECT executed_at FROM module_approvals WHERE id = ?")
            .bind(&submitted.approval_id)
            .fetch_one(pool)
            .await
            .unwrap();
        let executed_at: Option<String> = row.get("executed_at");
        assert!(
            executed_at.is_some(),
            "the execution scan must still have succeeded even though the timeout \
             scan's query failed this same cycle"
        );
    }
}
