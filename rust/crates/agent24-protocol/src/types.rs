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

/// `GET /api/v1/shutdown` (SHUT-1c): the shutdown budgets this daemon runs
/// with, what it warned about them, and what it found of the daemon before it
/// — so a budget that is too tight can be found and adjusted without reading
/// logs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShutdownReport {
    /// An ephemeral daemon keeps no shutdown evidence.
    pub ephemeral: bool,
    /// Where the evidence lives (`<state dir>/run`), for a daemon that keeps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_dir: Option<String>,
    /// `A24_MODULE_DRAIN_MS` in effect.
    pub drain_ms: u64,
    /// `A24_MODULE_STOP_GRACE_MS` in effect.
    pub stop_grace_ms: u64,
    /// From SIGTERM to the process gone, at the latest (2000 at the defaults).
    pub exit_bound_ms: u64,
    /// Values that were rejected (the default used instead), and evidence
    /// that could not be kept.
    #[serde(default)]
    pub config_warnings: Vec<String>,
    /// Open enum: `no_history` | `clean` | `unreadable` | `cleanup_failed` |
    /// `unconfirmed` (the daemon before did not confirm a clean shutdown).
    pub previous: String,
    /// What `previous` means and what to do about it, when there is something
    /// to say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_detail: Option<String>,
    /// The summary the daemon before left, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_shutdown: Option<LastShutdown>,
}

/// The gist of `last-shutdown.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LastShutdown {
    /// Open enum: `clean` | `degraded` | `timed_out`.
    pub stop_result: String,
    pub began_at_ms: u64,
    pub took_ms: u64,
    /// Modules whose leader was SIGKILLed after its stop grace — raise
    /// `A24_MODULE_STOP_GRACE_MS` if they need longer.
    #[serde(default)]
    pub killed_after_grace: Vec<String>,
    /// Modules whose drain ran out with requests still in flight, as
    /// `name (count)` — raise `A24_MODULE_DRAIN_MS` if requests run longer.
    #[serde(default)]
    pub cut_requests: Vec<String>,
    /// Records the summary left out to stay small.
    #[serde(default)]
    pub omitted_records: u64,
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

// ── Domain OS registry (M-E / ME-2) ──────────────────────────────────────────

/// One domain OS, as `agent24 os list` sees it.
///
/// It carries TWO states on purpose. `enabled` is what the config says now;
/// `state` is what the running daemon is actually doing with the module. They
/// diverge the moment someone toggles a module that needs a restart to follow
/// (any enable; a disable of a compiled-in module) — routes are built once at
/// startup, and hiding that divergence would leave a user staring at a module
/// that says "enabled" while every request 503s. A disable of a running
/// out-of-process module is applied at once (SUP-5), so its two agree — until
/// a later enable, which, like any enable, waits for the next start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DomainOsView {
    pub name: String,
    /// `/api/v1/<name>`, always derived from the kernel's CATALOGUE name — which
    /// exists before any module does. A constructed module whose manifest states a
    /// different name is refused rather than routed, so the two can never disagree
    /// on a mounted module.
    pub namespace: String,
    pub version: String,
    /// What `os.json` says right now.
    pub enabled: bool,
    /// What the RUNNING daemon is doing with it: what it did at startup, or —
    /// for a running module `os disable` stopped since — `disabled`. Open
    /// enum: `mounted` | `disabled` | `degraded` | `refused`.
    pub state: String,
    /// Why, when there is a reason worth acting on — a degradation or a refusal
    /// — or, for a `mounted` out-of-process module, a passing state worth
    /// knowing (`starting`, `stopping`, and `stop requested` for one `os
    /// disable` asked to stop that still admits requests), and for a module
    /// `disabled` while it ran, `stopping` until it has drained and stopped
    /// (SUP-5). Absent for a
    /// `mounted` module that is simply serving, and for a settled `disabled`,
    /// whose reason is that the user said so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Kernel capabilities the module actually GOT: the intersection of what its
    /// manifest asked for and what the kernel offers.
    ///
    /// EMPTY for anything that did not mount, in every case: a module that was
    /// never constructed, one that was refused, and one that failed to open its
    /// store all HOLD nothing — the last had grants computed and then never
    /// received a `KernelCtx`. Empty means "holds nothing", not "not known".
    #[serde(default)]
    pub granted: Vec<String>,
    /// Declared models the daemon could not find. Empty when satisfied, when
    /// nothing was declared, or when the check could not run — `resources` says
    /// which.
    #[serde(default)]
    pub missing_models: Vec<String>,
    /// Open enum: `ok` | `missing` | `unknown` | `not_checked`.
    pub resources: String,
    /// The config says something the running daemon has not applied: this
    /// module will only pick the change up on the next start. A disable that
    /// stopped the running module is applied, so it needs none (SUP-5).
    /// Deliberately not "it is enabled but not running" — a module that is
    /// enabled and merely unhealthy has no pending change, and `detail` is what
    /// the user should act on there. Defaults to `false` when absent, like its
    /// neighbours, so a list from an older daemon still reads (PR#183 approve
    /// Low).
    #[serde(default)]
    pub restart_required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DomainOsList {
    pub modules: Vec<DomainOsView>,
    /// A problem with the REGISTRY itself rather than with any one module — an
    /// entry that disables something this build does not provide, say.
    ///
    /// Reported separately because it is not a module's fault and no module's
    /// `restart_required` can express it: restarting changes nothing until the file
    /// is fixed, so this is the one instruction that helps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_error: Option<String>,
}

/// Body of `PATCH /api/v1/os/{name}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DomainOsUpdate {
    pub enabled: bool,
}

// ── Errors (SPEC-002 §5) ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorBody {
    /// Open enum: invalid_request, unauthorized, not_found, conflict,
    /// approval_already_resolved, provider_unavailable,
    /// run_not_cancellable (reserved), payload_too_large, internal,
    /// admission_refused (T8/ME-3g), module_panicked, module_killed
    /// (the latter two were already in use by ERR-1 but missing from this
    /// list — T8 audited and added them alongside its own new code)
    pub code: String,
    pub message: String,
    /// What to do about it, when there is a concrete next step (ERR-1). Kept
    /// separate from `message` (what happened) rather than folded into it or
    /// into `details`, so a client can render the two differently and a test
    /// can assert on one without parsing the other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Map<String, Value>>,
}

/// HTTP 4xx/5xx body: `{ "error": { code, message, hint?, details? } }`
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

/// H8 plan mode. `plan` starts the run under a read-only tool gate — only
/// Read-class tools and `propose_plan` are advertised — until the model submits
/// a plan the human approves, which unlocks the full tool set for that run.
/// Additive: absent/unknown deserializes to `normal`, so pre-H8 clients are
/// unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    #[default]
    Normal,
    Plan,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunInput {
    pub prompt: String,
    #[serde(default)]
    pub model_override: Option<String>,
    #[serde(default)]
    pub mode: RunMode,
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
    /// H8: `plan` starts the run read-only until a submitted plan is approved.
    #[serde(default)]
    pub mode: RunMode,
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
    /// The exact target a `approve_for_target` decision would bind to (H4):
    /// the channel address, recipient, or repository this call names. Present
    /// only when `available_decisions` offers that decision, so a UI can label
    /// the button with what it actually authorises ("always allow → #ops")
    /// rather than an unqualified "always allow".
    #[serde(default)]
    pub standing_target: Option<String>,
    pub status: ApprovalStatus,
    /// Set once resolved
    pub decision: Option<Decision>,
    /// After this instant the approval resolves to timed_out
    pub expires_at: String,
    pub created_at: String,
    pub decided_at: Option<String>,
}

// ── Module approval (T7b/ME-3e, `docs/design/T7b-ME3e-approvals.md`) ─────────
//
// A PARALLEL, independent type from `Approval`/`ApprovalBroker` above (design
// doc §"现状"4): `run_id` is not required, the interaction model is async
// submit-then-poll rather than synchronous blocking wait, and there is no
// "delivery" concept — a decision is a terminal fact the instant it is made,
// queried by `approval_id` as many times as a module likes.

/// Which of the two protocol methods a [`ModuleApproval`] was submitted
/// through. Carried explicitly on [`ApprovalAnswer`] too (design doc decision
/// 6, Codex round 5 High 4) so a caller can never mistake an `Advise` result
/// for a `Gate` one — the two differ by an order of magnitude in what they
/// guarantee, and `decision == Approved` alone does not say which this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModuleApprovalKind {
    /// A kernel-EXECUTED action. This round's closed set of executable
    /// actions is empty (T7b scope; see the design doc's opening section), so
    /// every `gate` submission is `forbidden` before a row is ever written —
    /// no `ModuleApproval` with this kind exists yet in this build.
    Gate,
    /// A module-domain action: the kernel records and presents it, but does
    /// not execute it and does not guarantee the module honors the answer
    /// (SPEC §6.1 — knowledge, not a safety control).
    Advise,
}

/// The one decision dimension a [`ModuleApproval`] has (design doc decision
/// 3) — there is no separate "delivered" state, because the async
/// submit-then-poll model has no delivery step to fail or get stuck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModuleApprovalDecision {
    Pending,
    Approved,
    Denied,
    /// Set by the periodic scan (design doc decision 5) once `expires_at`
    /// passes while still `Pending` — equivalent to a denial, fail-closed.
    TimedOut,
}

/// One module approval record (design doc decision 3). `(module, request_id,
/// kind)` is UNIQUE at the storage layer — both a data-integrity constraint
/// and the mechanism a resubmitted `{request_id, approval_token, action,
/// target, payload}` relies on to be idempotent (a lost response, retried by
/// the module, lands on the same row rather than a second one).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModuleApproval {
    /// Minted at submission time: 32 bytes random, hex-encoded. The only
    /// credential needed to query this record — NOT derived from
    /// `request_id` or anything else predictable, and treated as a secret
    /// worth withholding from an unrelated caller (it discloses `action`/
    /// `target`/`payload` to anyone who has it).
    pub id: String,
    /// From the callback connection's identity (the closure that built this
    /// module's `MethodsFor`) — never self-reported by the module.
    pub module: String,
    /// The proxied request's correlation id — one of the two halves of the
    /// idempotent submission key (with `module`/`kind`).
    pub request_id: String,
    pub kind: ModuleApprovalKind,
    /// Always `false` when `kind == Advise`. Always unreachable when
    /// `kind == Gate` this round (the closed set is empty, so no `Gate` row
    /// is ever created) — kept as a real field, not derived from `kind`
    /// alone, so a future non-empty closed set does not need a wire shape
    /// change.
    pub binding: bool,
    pub action: String,
    pub target: Option<String>,
    /// The kernel's own record of what it received at submission time — NOT
    /// a promise that the module will act on exactly this (SPEC §6.1: for
    /// `Advise`, this is knowledge, not a safety control). Never accepted as
    /// an "update" after submission; see [`approval_digest`].
    pub payload: Value,
    /// `approval_digest(&payload)`, computed ONCE at submission by the
    /// kernel — never self-reported by the module.
    pub payload_digest: String,
    pub decision: ModuleApprovalDecision,
    pub created_at: String,
    pub decided_at: Option<String>,
    /// After this instant a `Pending` record resolves to `TimedOut` (design
    /// doc decision 5's periodic scan judges this field).
    pub expires_at: String,
}

/// What a `submit`/`status` call on [`agent24_domain`]'s in-process
/// `ApprovalRequester` (design doc decision 6) gets back. A trimmed
/// projection of [`ModuleApproval`] — a module needs the decision and enough
/// to interpret it, not the full record (which also carries data another
/// module's `payload` should not casually flow through).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalAnswer {
    pub approval_id: String,
    pub kind: ModuleApprovalKind,
    /// `false` for `Advise`; unreachable (no `Gate` row exists) this round.
    pub binding: bool,
    /// `Pending` immediately after a `submit`; the current value on `status`.
    pub decision: ModuleApprovalDecision,
}

/// Failure modes of the in-process `ApprovalRequester`'s `submit`/`status`
/// (design doc decision 6).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApprovalRequestError {
    /// Only `submit(Gate, …)` can hit this: the action is not in the (this
    /// round, empty) kernel-executable closed set. Mapped to the wire's
    /// existing `Forbidden` kind (design doc decision 4 — SPEC §6.1's wording
    /// is literal about reusing it, not inventing a more precise kind).
    #[error("action not in the kernel-executable closed set")]
    ActionNotInClosedSet,
    /// `status` was asked for an `approval_id` that does not exist, or exists
    /// but belongs to a different module. Mapped to the wire's
    /// `agent24_os_proto::rpc::ErrorKind::NotFound` (design doc decision 2;
    /// not named as a doc link — this crate does not depend on
    /// `agent24-os-proto`) — deliberately one outcome for both (decision 4),
    /// so a caller cannot enumerate other modules' approval ids.
    #[error("approval not found")]
    NotFound,
    /// The storage layer failed. REST maps this to 503; the wire boundary
    /// maps it to `agent24-os-proto`'s existing internal-error kind. Does not
    /// apply to the periodic timeout scan (design doc decision 5) — that has
    /// no caller waiting on an answer, and simply retries next cycle.
    #[error("approval backend unavailable: {0}")]
    BackendUnavailable(String),
}

/// Recursively rebuild `value` with every object's keys inserted in SORTED
/// order — a `BTreeMap` intermediate guarantees this regardless of whether
/// `serde_json::Map` itself is a `BTreeMap` (the default) or an `IndexMap`
/// (the `preserve_order` feature, not enabled anywhere in this workspace
/// today, but [`approval_digest`] must not silently start disagreeing with
/// itself the day something enables it transitively).
fn sort_object_keys_recursively(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: std::collections::BTreeMap<&String, &Value> = map.iter().collect();
            let mut out = Map::with_capacity(sorted.len());
            for (k, v) in sorted {
                out.insert(k.clone(), sort_object_keys_recursively(v));
            }
            Value::Object(out)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(sort_object_keys_recursively).collect())
        }
        other => other.clone(),
    }
}

/// The one place a [`ModuleApproval`]'s `payload_digest` is computed (design
/// doc decision 6) — the wire handler (via `ModuleApprovalBroker::submit`)
/// and `PolicyApprovalBackend` both go through it rather than each hashing
/// their own copy. Keys are sorted recursively before hashing so the digest
/// does not depend on `serde_json`'s internal `Map` ordering.
#[must_use]
pub fn approval_digest(payload: &Value) -> String {
    use sha2::Digest;
    let canonical = sort_object_keys_recursively(payload);
    #[allow(
        clippy::expect_used,
        reason = "a Value that parsed can always be re-serialized"
    )]
    let bytes = serde_json::to_vec(&canonical).expect("Value serialization cannot fail");
    format!("sha256:{}", hex::encode(sha2::Sha256::digest(&bytes)))
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
