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

use crate::types::{Approval, ErrorBody, Usage};

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
    #[serde(rename = "run.completed")]
    RunCompleted(RunCompletedPayload),
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
            EventBody::RunCompleted(_) => "run.completed",
            EventBody::RunFailed(_) => "run.failed",
            EventBody::RunCancelled(_) => "run.cancelled",
            EventBody::ToolStarted(_) => "tool.started",
            EventBody::ToolCompleted(_) => "tool.completed",
            EventBody::ApprovalRequired(_) => "approval.required",
            EventBody::ApprovalResolved(_) => "approval.resolved",
            EventBody::ScheduleFired(_) => "schedule.fired",
            EventBody::ScheduleDisabled(_) => "schedule.disabled",
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
