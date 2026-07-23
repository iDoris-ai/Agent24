//! WS event protocol (SPEC-002 §3, protocol/events.schema.json).
//!
//! Envelope `{ v, seq, ts, type, payload }` — `type`/`payload` are adjacently
//! tagged onto [`EventBody`]. Every variant carries an explicit dotted
//! `#[serde(rename = "…")]` name (ADR-026 hard constraint #8): `rename_all`
//! would wrongly produce `run_started`.

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
        }
    }
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
