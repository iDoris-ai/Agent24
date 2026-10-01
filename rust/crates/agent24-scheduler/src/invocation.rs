//! ME4-1.2.2b — §3 (D2): the trigger interface. Replaces the old
//! `RunTrigger::trigger(&ScheduleAction, &str) -> Result<String, String>`.
//!
//! See `docs/design/ME4-S1-scheduler-callback.md` §3. Reference
//! implementation: the scratch check crate's `src/invocation.rs` (design
//! frozen v3.1), ported here with one change: `FireTrigger` is
//! `agent24_store::FireTrigger` re-exported, not a second, same-named type
//! (`agent24-store/src/module_schedules.rs`'s doc comment on `FireTrigger`
//! explicitly asks for this — the store's minimal stand-in and this crate's
//! domain type must not diverge).

use agent24_protocol::ScheduleAction;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

pub use agent24_store::FireTrigger;

use crate::fire::FireId;

/// Who owns a module row. Both halves or neither — the type makes the
/// half-null row the migration's CHECK refuses unrepresentable in memory too.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModuleScheduleKey {
    pub owner_module: String,
    pub module_key: String,
}

/// What the fire is FOR. The two arms are disjoint by construction: an
/// `AgentRun` has no fire id (no delivery row exists for it), a module
/// delivery has no `ScheduleAction` (S1-2: it is not a user action).
#[derive(Debug, Clone, PartialEq)]
pub enum InvocationTarget {
    AgentRun(ScheduleAction),
    Module {
        owner: ModuleScheduleKey,
        fire_id: FireId,
    },
}

/// One firing, as handed to [`RunTrigger::trigger`].
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleInvocation {
    pub schedule_id: String,
    /// The slot: the `next_run_at` that came due (tick), or `now` (run_now).
    pub scheduled_for: DateTime<Utc>,
    /// When the kernel recorded the firing (stable across retries — it is
    /// read back from the delivery row, not re-taken per attempt).
    pub fired_at: DateTime<Utc>,
    pub trigger: FireTrigger,
    pub target: InvocationTarget,
}

/// Why a module delivery did not happen this time, none of which is the
/// module's fault — so none increments `consecutive_failures` (S1-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferReason {
    /// `mount_all` has not finished (the trigger's supervisor handle is unset).
    MountPending,
    /// No running supervisor slot by that name: never mounted, disabled in
    /// os.json, hot-disabled, uninstalled, refused, or the list is closed
    /// because the daemon is shutting down.
    NotRunning,
    /// `RequestRefused::NotReady` — Starting / placeholder generation.
    NotReady,
    /// `RequestRefused::Draining`.
    Draining,
    /// `RequestRefused::Stopping` — generation revoked (crash backoff,
    /// breaker tripped, stop in progress).
    Stopping,
    /// Admitted, but revoked before it was sent (`dispatch()` false, or
    /// `Abandoned { dispatched: false }`): nothing reached the module.
    NeverSent,
    /// `/dev/urandom` unreadable (approval token) or a duplicate kernel id —
    /// a kernel-side condition, retried later, never blamed on the module.
    KernelTransient,
}

impl DeferReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MountPending => "mount_pending",
            Self::NotRunning => "not_running",
            Self::NotReady => "not_ready",
            Self::Draining => "draining",
            Self::Stopping => "stopping",
            Self::NeverSent => "never_sent",
            Self::KernelTransient => "kernel_transient",
        }
    }
}

/// The result of one [`RunTrigger::trigger`] call.
#[derive(Debug, Clone, PartialEq)]
pub enum FireOutcome {
    /// `InvocationTarget::AgentRun` only — the run the trigger created.
    AgentRun { run_id: String },
    /// `InvocationTarget::Module` only — the module answered 2xx.
    ModuleDelivered { fire_id: FireId },
    /// Module target only — see [`DeferReason`]. Not a failure.
    Deferred { reason: DeferReason },
    /// A real failure: for an agent run, `RunManager` refused; for a module,
    /// it was Running and the request was sent (or a connection to its live
    /// generation could not be made) and did not come back 2xx in time.
    Failed { reason: String },
}

/// Fires a schedule. Implemented by the daemon (`KernelTrigger`, over
/// `RunManager` for agent runs and the module deliverer for module rows,
/// ME4-1.3.1) — the scheduler crate stays free of the agent loop and of
/// `agent24-os-proto`.
///
/// Infallible signature on purpose: every way it can go wrong is a
/// [`FireOutcome`] the scheduler must classify, not an `Err` it could `?`
/// past.
#[async_trait]
pub trait RunTrigger: Send + Sync {
    async fn trigger(&self, invocation: &ScheduleInvocation) -> FireOutcome;
}

/// `Scheduler::run_now`'s result (REST: `{"run_id"}` vs `{"fire_id"}`,
/// ME4-1.2.2c).
#[derive(Debug, Clone, PartialEq)]
pub enum RunNowOutcome {
    Run { run_id: String },
    Fire { fire_id: FireId },
}

/// How the tick/`run_now` AgentRun path classifies an outcome — identical
/// branches to the pre-ME4-1.2.2b `fire()`/`run_now()`. Anything but
/// `AgentRun`/`Failed` for an `AgentRun` target is a kernel bug (a
/// mis-wired `RunTrigger` answering as if the target were a module) and is
/// treated as a failure rather than panicking.
///
/// # Errors
/// The failure reason.
pub fn agent_run_result(outcome: FireOutcome) -> Result<String, String> {
    match outcome {
        FireOutcome::AgentRun { run_id } => Ok(run_id),
        FireOutcome::Failed { reason } => Err(reason),
        other => Err(format!("kernel bug: agent-run trigger answered {other:?}")),
    }
}
