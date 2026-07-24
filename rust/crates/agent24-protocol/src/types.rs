//! REST resource types (SPEC-002 §1, openapi.yaml components).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// ── System ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Health {
    /// Always "ok" when reachable
    pub status: String,
    pub version: String,
    /// Open enum: "node" | "rust" | future backends
    pub backend: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    #[serde(default)]
    pub cost_usd: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Model {
    pub id: String,
    /// Open enum: omlx | ollama | remote | …
    pub provider: String,
    /// Open enum — routing tier (M-D): local | remote | lora
    pub tier: String,
    pub loaded: bool,
}

// ── Errors (SPEC-002 §5) ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorBody {
    /// Open enum: invalid_request, unauthorized, not_found, conflict,
    /// approval_already_resolved, provider_unavailable,
    /// run_not_cancellable (reserved), payload_too_large, internal
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Map<String, Value>>,
}

/// HTTP 4xx/5xx body: `{ "error": { code, message, details? } }`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

// ── Chat (M-A compat surface) ────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChatMessage {
    /// Open enum: system | user | assistant
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChatResponse {
    pub message: ChatMessage,
    pub usage: Usage,
}

// ── Session ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Session {
    pub id: String,
    pub title: String,
    /// Open enum: desktop | cli | tui | schedule | wechat | nostr
    pub channel: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionCreate {
    #[serde(default)]
    pub title: String,
    #[serde(default = "default_channel")]
    pub channel: String,
}

fn default_channel() -> String {
    "desktop".to_owned()
}

// ── Run ──────────────────────────────────────────────────────────────────────

/// State machine (only legal transitions, SPEC-002 §1.2):
/// queued → running → completed | failed | cancelled;
/// running ⇄ awaiting_approval;
/// queued|running|awaiting_approval → cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    AwaitingApproval,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunInput {
    pub prompt: String,
    #[serde(default)]
    pub model_override: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunOutput {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Run {
    pub id: String,
    /// Null for transient runs (e.g. created by /chat)
    pub session_id: Option<String>,
    pub status: RunStatus,
    pub input: RunInput,
    /// Present (non-null) when status=completed
    pub output: Option<RunOutput>,
    /// Present (non-null) when status=failed
    pub error: Option<ErrorBody>,
    pub usage: Usage,
    /// Set when the run was fired by a schedule
    pub schedule_id: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunCreate {
    /// Omit/null to create a transient run
    #[serde(default)]
    pub session_id: Option<String>,
    pub prompt: String,
    #[serde(default)]
    pub model_override: Option<String>,
}

// ── ToolCall ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Running,
    Completed,
    Failed,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolCall {
    pub id: String,
    pub run_id: String,
    pub tool: String,
    /// Full detail persisted for audit; summarized externally
    pub input: Map<String, Value>,
    pub status: ToolCallStatus,
    /// Null while running
    pub output_summary: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
}

// ── Approval (fail-closed, SPEC-002 §1.4) ────────────────────────────────────

/// timed_out is equivalent to a denial (fail-closed)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Aborted,
    TimedOut,
}

/// OPEN SET, server-driven: valid `type` values for a given approval are
/// exactly its `available_decisions`. Known: approve, approve_for_session,
/// deny (reason required), abort. Future types may carry extra fields (kept
/// in `extra` via flatten). Fail-closed: the implementation default (broken
/// channel, cancelled run, daemon restart) is equivalent to abort/deny.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Decision {
    #[serde(rename = "type")]
    pub kind: String,
    /// Required when kind == "deny"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Approval {
    pub id: String,
    pub run_id: String,
    pub tool_call_id: String,
    /// Open enum: exec | fs_write | network | module
    pub kind: String,
    pub summary: String,
    /// Kind-specific detail (e.g. command argv, cwd, reason)
    pub payload: Map<String, Value>,
    /// Server-driven open set — UIs render exactly this list
    pub available_decisions: Vec<String>,
    pub status: ApprovalStatus,
    /// Set once resolved
    pub decision: Option<Decision>,
    /// After this instant the approval resolves to timed_out
    pub expires_at: String,
    pub created_at: String,
    pub decided_at: Option<String>,
}

// ── Schedule (SPEC-002 §1.5) ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScheduleSpec {
    /// 5/6-field cron expression; tz is an IANA timezone (default UTC)
    Cron {
        expr: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tz: Option<String>,
    },
    /// 60 ≤ secs ≤ 86400
    Every { secs: u32 },
    /// One-shot
    At { ts: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScheduleAction {
    AgentRun {
        prompt: String,
        /// Null = each firing creates a transient run
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        model_override: Option<String>,
    },
}

/// Open set — M-F adds channel/webhook/email targets
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeliveryTarget {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub spec: ScheduleSpec,
    pub action: ScheduleAction,
    pub delivery: Vec<DeliveryTarget>,
    pub last_run_at: Option<String>,
    /// Null when disabled or one-shot already fired
    pub next_run_at: Option<String>,
    /// Auto-disables the schedule at 5 (emits schedule.disabled)
    pub consecutive_failures: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScheduleCreate {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub spec: ScheduleSpec,
    pub action: ScheduleAction,
    #[serde(default)]
    pub delivery: Vec<DeliveryTarget>,
}

fn default_true() -> bool {
    true
}

/// Partial update (openapi ScheduleUpdate: any non-empty subset). A field
/// present-and-null is meaningful only where the wire allows null; for these
/// fields "absent" = leave unchanged. Changing `spec` recomputes next_run_at.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ScheduleUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<ScheduleSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ScheduleAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Vec<DeliveryTarget>>,
}

// ── Tools ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolInfo {
    pub name: String,
    /// Open enum: builtin | mcp | module
    pub source: String,
    pub description: String,
    #[serde(default)]
    pub requires_approval: bool,
}
