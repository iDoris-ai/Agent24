//! WS event protocol (SPEC-002 §3, protocol/events.schema.json).
//!
//! Envelope `{ v, seq, ts, type, payload }` — `type`/`payload` are adjacently
//! tagged onto [`EventBody`]. Every FIRST-PARTY variant carries an explicit
//! dotted `#[serde(rename = "run.started")]` name (ADR-026 hard constraint #8):
//! `rename_all` would wrongly produce `run_started`.
//!
//! ONE declared exemption (SPEC-002 §3): the [`EventBody::Module`] envelope's
//! `type` is the bare namespace tag `"module"`, not an event name — the real,
//! dotted event name lives in `payload.kind`. This is the sanctioned channel
//! for the second clause of hard constraint #8 too: a module's `payload` IS
//! opaque and clients dispatch on `payload.module`/`payload.kind`, which is
//! exactly what "no untyped-JSON parsing" forbids for FIRST-PARTY events.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::{Approval, ErrorBody, ModuleApprovalDecision, ModuleApprovalSubmitted, Usage};

/// Common envelope for every WS message. `seq` is monotonically increasing
/// per connection; a gap means the client must reconcile via REST (no replay
/// in v1). Clients MUST ignore unknown event types and unknown fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    /// Protocol major version — always 1
    pub v: u8,
    pub seq: u64,
    pub ts: String,
    #[serde(flatten)]
    pub body: EventBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "payload")]
pub enum EventBody {
    #[serde(rename = "run.started")]
    RunStarted(RunStartedPayload),
    #[serde(rename = "model.delta")]
    ModelDelta(ModelDeltaPayload),
    /// ME4-desktop-model-ui: kernel visibility into one completed
    /// `_a24/model/complete` call (agent24d/src/model_callback.rs) — which
    /// module called, which model/tier/provider actually served it, whether
    /// it succeeded, and how long it took. Broadcast once per call that
    /// reached routing (ok or failed), regardless of whether any WS client
    /// is listening. Deliberately carries NO prompt/response content — the
    /// call itself may be `LocalOnly`, and this event is not.
    #[serde(rename = "model.call")]
    ModelCall(ModelCallPayload),
    #[serde(rename = "run.completed")]
    RunCompleted(RunCompletedPayload),
    #[serde(rename = "memory.write_failed")]
    MemoryWriteFailed(MemoryWriteFailedPayload),
    /// M1-T10 review H1: emitted at run start instead of silently dropping an
    /// explicit "记住……" while personal memory is paused — the model is ALSO
    /// told (a data notice injected alongside recall), so it does not tell
    /// the user it remembered something it did not.
    #[serde(rename = "memory.write_skipped")]
    MemoryWriteSkipped(MemoryWriteSkippedPayload),
    #[serde(rename = "memory.recalled")]
    MemoryRecalled(MemoryRecalledPayload),
    #[serde(rename = "run.failed")]
    RunFailed(RunFailedPayload),
    #[serde(rename = "run.cancelled")]
    RunCancelled(RunCancelledPayload),
    #[serde(rename = "tool.started")]
    ToolStarted(ToolStartedPayload),
    #[serde(rename = "tool.completed")]
    ToolCompleted(ToolCompletedPayload),
    /// REQUEST class: the client MUST answer via POST /api/v1/approvals/{id}.
    /// Fail-closed: no answer before `expires_at` resolves to timed_out.
    #[serde(rename = "approval.required")]
    ApprovalRequired(Box<Approval>),
    #[serde(rename = "approval.resolved")]
    ApprovalResolved(ApprovalResolvedPayload),
    #[serde(rename = "schedule.fired")]
    ScheduleFired(ScheduleFiredPayload),
    #[serde(rename = "schedule.disabled")]
    ScheduleDisabled(ScheduleDisabledPayload),
    /// Module-delivery counterpart of `schedule.fired` (design
    /// `docs/design/ME4-S1-scheduler-callback.md` §5.5): emitted once the
    /// delivery pump's T2 transition (a 2xx from the module's
    /// `_a24/scheduler/fired` handler) lands in `schedule_deliveries`. Does
    /// NOT carry a `run_id` — module deliveries have none; only AgentRun rows
    /// emit `schedule.fired`. Wired up by ME4-1.3.1's delivery pump; this
    /// task (ME4-1.2.2a) only adds the wire type.
    #[serde(rename = "schedule.delivered")]
    ScheduleDelivered(ScheduleDeliveredPayload),
    /// REQUEST class (T7b/ME-3e, `docs/design/T7b-ME3e-approvals.md` decision
    /// 7): pushed the moment a `gate`/`advise` submission inserts a new
    /// `Pending` row. The client answers via
    /// `POST /api/v1/module-approvals/{id}` — `approval.required` is no
    /// longer the only REQUEST-class event.
    ///
    /// T7c/ME-3e (design doc criterion 18): the payload is
    /// [`ModuleApprovalSubmitted`], NOT the full `ModuleApproval` — a frozen
    /// submission-time snapshot that does not carry `executed_at`, which is
    /// state only ever learned by querying AFTER submission (REST/`status`).
    #[serde(rename = "module-approval.required")]
    ModuleApprovalRequired(Box<ModuleApprovalSubmitted>),
    /// Pushed the moment a decision becomes final — the decision CAS
    /// (REST `decide`) or the periodic timeout scan, whichever gets there
    /// first. There is no separate "delivered" event: in the async
    /// submit-then-poll model a decision IS the terminal state the instant
    /// it is made (design doc decision 5).
    #[serde(rename = "module-approval.resolved")]
    ModuleApprovalResolved {
        id: String,
        decision: ModuleApprovalDecision,
    },
    /// Opaque event from a loadable module (e.g. Sin90). The kernel carries it
    /// on the same WS stream without understanding its semantics — a generic
    /// capability, NOT knowledge of any specific module. The `type` is the bare
    /// namespace tag `module` (declared exemption to hard constraint #8's dotted
    /// rule, SPEC-002 §3); the real dotted event name is `payload.kind`. Clients
    /// dispatch on `payload.module` + `payload.kind`.
    #[serde(rename = "module")]
    Module(ModuleEventPayload),
}

impl EventBody {
    /// The dotted wire name of this event (e.g. `run.started`).
    pub fn wire_type(&self) -> &'static str {
        match self {
            EventBody::RunStarted(_) => "run.started",
            EventBody::ModelDelta(_) => "model.delta",
            EventBody::ModelCall(_) => "model.call",
            EventBody::RunCompleted(_) => "run.completed",
            EventBody::RunFailed(_) => "run.failed",
            EventBody::MemoryWriteFailed(_) => "memory.write_failed",
            EventBody::MemoryWriteSkipped(_) => "memory.write_skipped",
            EventBody::MemoryRecalled(_) => "memory.recalled",
            EventBody::RunCancelled(_) => "run.cancelled",
            EventBody::ToolStarted(_) => "tool.started",
            EventBody::ToolCompleted(_) => "tool.completed",
            EventBody::ApprovalRequired(_) => "approval.required",
            EventBody::ApprovalResolved(_) => "approval.resolved",
            EventBody::ScheduleFired(_) => "schedule.fired",
            EventBody::ScheduleDisabled(_) => "schedule.disabled",
            EventBody::ScheduleDelivered(_) => "schedule.delivered",
            EventBody::ModuleApprovalRequired(_) => "module-approval.required",
            EventBody::ModuleApprovalResolved { .. } => "module-approval.resolved",
            EventBody::Module(_) => "module",
        }
    }
}

/// A module-namespaced event (adjacently-tagged `type = "module"`). The
/// envelope shape is deliberately CLOSED — extension space is inside `payload`,
/// which the kernel relays verbatim and never inspects. This is the ONLY seam
/// by which a module reaches the WS stream, preserving the one-way dependency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModuleEventPayload {
    /// Owning module. MUST equal the module's manifest `id`
    /// (`protocol/module.schema.json`), hence the same pattern.
    #[schemars(regex(pattern = r"^(@[a-z0-9-~][a-z0-9-._~]*/)?[a-z0-9-~][a-z0-9-._~]*$"))]
    pub module: String,
    /// Module-defined event kind, dotted like a first-party name, e.g.
    /// `"task.transitioned"` — this is where the real event name lives.
    pub kind: String,
    /// Module-defined body; an OBJECT, opaque to the kernel and to clients that
    /// don't know this module (matches the generated TS `{ [k]: unknown }`).
    pub payload: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunStartedPayload {
    pub run_id: String,
    /// Null for transient runs (e.g. /chat)
    pub session_id: Option<String>,
    /// Set when fired by a schedule
    pub schedule_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelDeltaPayload {
    pub run_id: String,
    /// Streaming text increment
    pub text: String,
}

/// One completed `_a24/model/complete` call (ME4-desktop-model-ui). Mirrors
/// `agent24d::model_callback::ModelCompleteResult`'s own `model_id`/`tier`
/// plus the router's `Served::provider` and the usage sink's token counts —
/// never the call's `text`/messages, which stay off the WS entirely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelCallPayload {
    /// The module that placed the call (`_a24/model/complete`'s caller,
    /// e.g. `"agentear"`).
    pub module: String,
    /// Provider-reported model id, when one was reported (mirrors
    /// `ModelCompleteResult::model_id`) — `None` when the provider didn't
    /// say, or when no provider ever answered this call.
    pub model_id: Option<String>,
    /// Open enum, `"local" | "remote"` (matches the RPC result's own
    /// `tier`) — `None` when the call failed before any provider served it,
    /// so no tier was ever decided.
    pub tier: Option<String>,
    /// Which provider actually served the call (`Served::provider`, e.g.
    /// `"omlx"`/`"ollama"`) — `None` for the same reason `tier` can be.
    pub served_by: Option<String>,
    /// Whether the RPC call itself succeeded (a provider answering but the
    /// kernel then withholding the result — oversize, the LocalOnly
    /// tripwire — counts as `false`, same as `UsageOutcome::FailedAfterServe`).
    pub ok: bool,
    pub latency_ms: u64,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunCompletedPayload {
    pub run_id: String,
    pub output: RunOutputPayload,
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunOutputPayload {
    pub text: String,
}

/// The answer completed, but its session exchange could not be recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryWriteFailedPayload {
    pub session_id: String,
    pub reason: String,
}

/// M1-T10 review H1: `reason` is an open enum (today only `"paused"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryWriteSkippedPayload {
    pub run_id: String,
    /// Null for transient (session-less) runs, same as `RunStartedPayload`.
    pub session_id: Option<String>,
    pub reason: String,
}

/// Assertion ids that were actually included in a run's recalled context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryRecalledPayload {
    pub run_id: String,
    pub ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunFailedPayload {
    pub run_id: String,
    pub error: ErrorBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunCancelledPayload {
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolStartedPayload {
    pub run_id: String,
    pub tool_call_id: String,
    pub tool: String,
    /// Summarized — full input is audit-only
    pub input_summary: String,
}

/// Closed set per protocol/events.schema.json (a running tool never emits
/// tool.completed)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolCompletedStatus {
    Completed,
    Failed,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolCompletedPayload {
    pub run_id: String,
    pub tool_call_id: String,
    pub status: ToolCompletedStatus,
    pub output_summary: Option<String>,
}

/// Broadcast so every connected client converges
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalResolvedPayload {
    pub approval_id: String,
    pub run_id: String,
    /// Open enum — the Decision.type that resolved it, or timed_out/aborted
    pub decision_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScheduleFiredPayload {
    pub schedule_id: String,
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScheduleDisabledPayload {
    pub schedule_id: String,
    /// Open enum; currently only consecutive_failures
    pub reason: String,
}

/// design §5.5: `{schedule_id, module, key, fire_id, scheduled_for}`, all
/// required (no optional fields to force-require in export-schema.rs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScheduleDeliveredPayload {
    pub schedule_id: String,
    pub module: String,
    pub key: String,
    pub fire_id: String,
    /// ISO-8601 UTC (fmt_iso), the slot this fire was recorded for.
    pub scheduled_for: String,
}
