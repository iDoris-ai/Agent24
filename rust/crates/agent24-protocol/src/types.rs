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

impl ScheduleUpdate {
    /// True when no field is set (openapi: ScheduleUpdate is `minProperties: 1`,
    /// so an empty update is a 400, not a silent no-op).
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.enabled.is_none()
            && self.spec.is_none()
            && self.action.is_none()
            && self.delivery.is_none()
    }
}

// ── Tools ────────────────────────────────────────────────────────────────────

/// The intrinsic side-effect category of a tool (H1) — the single declared
/// property the approval path reads, replacing the hardcoded name sets the
/// policy layer used to carry inline.
///
/// The point of the split is NOT finer labelling: it is that each class earns a
/// different exemption path. Only [`RiskClass::External`] is eligible for a
/// target-scoped standing grant (H4); `Exec` asks every single time, forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    /// No side effects — always allowed.
    ///
    /// Note this covers *network reads* (`http_fetch`): a GET changes nothing.
    /// Its danger is exfiltration, which is a taint-propagation problem, not a
    /// side-effect class — filing it under `External` would wrongly make it
    /// eligible for standing grants.
    Read,
    /// Mutates the local workspace — path-scoped and gated.
    WriteLocal,
    /// Runs commands. Always gated, never eligible for a standing grant.
    Exec,
    /// Side effects that leave this machine — the unattended-inbox hook and the
    /// only class a target-scoped standing grant may cover.
    External,
}

impl RiskClass {
    /// Anything but a pure read needs the approval path's attention.
    ///
    /// This is the ONLY definition of "needs approval" in the system:
    /// [`ToolInfo::requires_approval`] is derived from it so the two can never
    /// drift apart the way two hand-maintained lists do.
    pub const fn requires_approval(self) -> bool {
        !matches!(self, RiskClass::Read)
    }

    /// Whether a target-scoped standing grant (H4) may ever cover this class.
    pub const fn standing_grant_eligible(self) -> bool {
        matches!(self, RiskClass::External)
    }

    /// How far this class can escape human review — the axis a user-local
    /// override (H2) is allowed to move a tool *down* but not *up*.
    ///
    /// This is deliberately NOT a "how scary is it" ranking: `write_local` and
    /// `exec` are not comparable that way. It orders the classes by the only
    /// thing an override can abuse, namely how much review the class lets a
    /// call skip:
    ///
    /// - `Read` (3) — skips the gate entirely
    /// - `External` (2) — gated, but a standing grant can pre-answer it (H4)
    /// - `WriteLocal` (1) — gated, never grant-eligible
    /// - `Exec` (0) — gated, never grant-eligible, asked every single time
    pub const fn escape_rank(self) -> u8 {
        match self {
            RiskClass::Read => 3,
            RiskClass::External => 2,
            RiskClass::WriteLocal => 1,
            RiskClass::Exec => 0,
        }
    }
}

/// Non-exhaustive on purpose: construction must go through [`ToolInfo::new`],
/// which is what makes `requires_approval` a derived value rather than a second
/// thing to remember to set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ToolInfo {
    pub name: String,
    /// Open enum: builtin | mcp | module
    pub source: String,
    pub description: String,
    /// Declared side-effect class (H1). Additive field: absent in payloads from
    /// a pre-H1 daemon, where it defaults to the most conservative class rather
    /// than the most permissive one — an unlabelled tool is treated as if it
    /// reaches off the machine.
    #[serde(default = "default_risk_class")]
    pub risk_class: RiskClass,
    /// DERIVED from `risk_class` — kept as a wire field for pre-H1 clients.
    /// Never set it independently; [`ToolInfo::new`] is the only writer.
    #[serde(default)]
    pub requires_approval: bool,
}

const fn default_risk_class() -> RiskClass {
    RiskClass::External
}

impl ToolInfo {
    pub fn new(
        name: impl Into<String>,
        source: impl Into<String>,
        description: impl Into<String>,
        risk_class: RiskClass,
    ) -> Self {
        Self {
            name: name.into(),
            source: source.into(),
            description: description.into(),
            risk_class,
            requires_approval: risk_class.requires_approval(),
        }
    }
}

#[cfg(test)]
mod risk_class_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn requires_approval_is_read_vs_everything_else() {
        assert!(!RiskClass::Read.requires_approval());
        assert!(RiskClass::WriteLocal.requires_approval());
        assert!(RiskClass::Exec.requires_approval());
        assert!(RiskClass::External.requires_approval());
    }

    /// H4's eligibility rule, asserted at its definition so a later edit that
    /// widens it has to delete a test that says why it is narrow.
    #[test]
    fn only_external_may_hold_a_standing_grant() {
        assert!(RiskClass::External.standing_grant_eligible());
        for class in [RiskClass::Read, RiskClass::WriteLocal, RiskClass::Exec] {
            assert!(
                !class.standing_grant_eligible(),
                "{class:?} must never be eligible — exec/write ask every time"
            );
        }
    }

    #[test]
    fn constructor_derives_the_wire_field() {
        for (class, expected) in [
            (RiskClass::Read, false),
            (RiskClass::WriteLocal, true),
            (RiskClass::Exec, true),
            (RiskClass::External, true),
        ] {
            let info = ToolInfo::new("t", "builtin", "d", class);
            assert_eq!(info.requires_approval, expected, "{class:?}");
            assert_eq!(info.risk_class, class);
        }
    }

    #[test]
    fn wire_names_are_snake_case() {
        let json = serde_json::to_value(ToolInfo::new(
            "fs_write",
            "builtin",
            "d",
            RiskClass::WriteLocal,
        ))
        .unwrap();
        assert_eq!(json["risk_class"], "write_local");
        assert_eq!(json["requires_approval"], true);
    }

    /// A payload from a pre-H1 daemon carries no `risk_class`. It must land on
    /// the most conservative class, NOT the most permissive one — an unlabelled
    /// tool is treated as if it reaches off the machine.
    #[test]
    fn missing_risk_class_defaults_fail_closed() {
        let info: ToolInfo = serde_json::from_value(serde_json::json!({
            "name": "legacy",
            "source": "mcp",
            "description": "",
            "requires_approval": false
        }))
        .unwrap();
        assert_eq!(info.risk_class, RiskClass::External);
        // The wire field deserializes as it was sent (false), which is exactly
        // why every decision point must read `risk_class`, never this field.
        assert!(!info.requires_approval);
        assert!(info.risk_class.requires_approval());
    }
}
