//! Agent24 tool system (C3 scope).
//!
//! `Tool` trait + registry with a fixed dispatch pipeline:
//! normalize → capability whitelist → approval gate → timeout-wrapped execute.
//!
//! The approval gate is a **fail-closed stub** until C4 lands: any tool whose
//! `ToolInfo.requires_approval` is true (`shell_exec`, `fs_write`) is
//! auto-DENIED at dispatch — never silently executed. Only `http_fetch` and
//! `fs_read` run automatically in C3. Callers are expected to audit-log every
//! denial (the agent loop does).

pub mod env_whitelist;
mod local;
mod net;

pub use local::{FsReadTool, FsWriteTool, ShellExecTool};
pub use net::HttpFetchTool;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use agent24_domain::tool::{ModuleToolCallError, ModuleToolErrorCode, ModuleToolResult};
use agent24_domain::{EgressDestination, EgressGate, EgressPurpose, EgressRequest, EgressResource};
use agent24_protocol::{Decision, RiskClass, ToolInfo};
use agent24_workspace::WorkspaceRunAuthority;
use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

/// Per-call execution context.
#[derive(Clone)]
pub struct ToolContext {
    run_id: String,
    /// The session the run belongs to (scopes approve_for_session grants)
    session_id: Option<String>,
    /// The schedule that fired this run, when one did. A standing grant minted
    /// here belongs to the SCHEDULE rather than the session (H4): an unattended
    /// automation is the thing the user was consenting to, so revoking or
    /// deleting that automation must take its grants with it.
    schedule_id: Option<String>,
    /// The persisted tool-call row this execution belongs to
    tool_call_id: String,
    workspace: WorkspaceAuthority,
    egress: Option<Arc<dyn EgressGate>>,
    egress_resources: Vec<EgressResource>,
    egress_policy_unavailable: bool,
    authorization_generation: u64,
}

#[derive(Clone)]
enum WorkspaceAuthority {
    Legacy,
    Bound(Arc<WorkspaceRunAuthority>),
}

impl ToolContext {
    #[must_use]
    pub fn legacy(
        run_id: impl Into<String>,
        session_id: Option<String>,
        schedule_id: Option<String>,
        tool_call_id: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            session_id,
            schedule_id,
            tool_call_id: tool_call_id.into(),
            workspace: WorkspaceAuthority::Legacy,
            egress: None,
            egress_resources: Vec::new(),
            egress_policy_unavailable: false,
            authorization_generation: 0,
        }
    }

    /// Bind the host's current source policy and live authorization checker to this call.
    #[must_use]
    pub fn with_egress_policy(
        mut self,
        gate: Arc<dyn EgressGate>,
        resources: Vec<EgressResource>,
        generation: u64,
    ) -> Self {
        self.egress = Some(gate);
        self.egress_resources = resources;
        self.authorization_generation = generation;
        self
    }

    /// Bind the run's source resources when no live host gate is available.
    /// Restricted resources are then denied at the tool's egress boundary.
    #[must_use]
    pub fn with_egress_resources(mut self, resources: Vec<EgressResource>) -> Self {
        self.egress_resources = resources;
        self
    }

    /// Mark the source-policy lookup as unavailable. Per-run egress consumers
    /// fail closed when this marker is present, while ordinary runs with no
    /// restricted resources retain their pre-egress behavior.
    #[must_use]
    pub fn with_egress_policy_unavailable(mut self) -> Self {
        self.egress_policy_unavailable = true;
        self
    }

    /// Apply the live egress gate only when this run has restricted or
    /// explicitly authorized source resources. An empty resource set means
    /// this run carries no source material governed by ADR-K1-02 §2.4.
    pub async fn authorize_egress_if_restricted(
        &self,
        purpose: EgressPurpose,
        destination: EgressDestination,
    ) -> Result<(), ToolError> {
        if self.egress_policy_unavailable {
            return Err(ToolError::Denied("outbound policy unavailable".into()));
        }
        if self.egress_resources.is_empty() {
            return Ok(());
        }
        self.authorize_egress(purpose, destination).await
    }

    fn with_egress_gate(mut self, gate: Arc<dyn EgressGate>) -> Self {
        self.egress = Some(gate);
        self
    }

    /// A missing policy service or destination is denied. Call immediately before network I/O.
    pub async fn authorize_egress(
        &self,
        purpose: EgressPurpose,
        destination: EgressDestination,
    ) -> Result<(), ToolError> {
        let gate = self
            .egress
            .as_ref()
            .ok_or_else(|| ToolError::Denied("outbound policy unavailable".into()))?;
        let request = EgressRequest::remote(
            self.egress_resources.clone(),
            purpose,
            destination,
            self.authorization_generation,
        );
        request
            .authorize(gate.as_ref())
            .await
            .map_err(|_| ToolError::Denied("outbound request denied".into()))
    }

    #[doc(hidden)]
    #[must_use]
    pub fn workspace_bound(
        run_id: impl Into<String>,
        session_id: Option<String>,
        schedule_id: Option<String>,
        tool_call_id: impl Into<String>,
        authority: Arc<WorkspaceRunAuthority>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            session_id,
            schedule_id,
            tool_call_id: tool_call_id.into(),
            workspace: WorkspaceAuthority::Bound(authority),
            egress: None,
            egress_resources: Vec::new(),
            egress_policy_unavailable: false,
            authorization_generation: 0,
        }
    }

    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    #[must_use]
    pub fn schedule_id(&self) -> Option<&str> {
        self.schedule_id.as_deref()
    }

    #[must_use]
    pub fn tool_call_id(&self) -> &str {
        &self.tool_call_id
    }

    #[doc(hidden)]
    #[must_use]
    pub fn workspace_authority(&self) -> Option<&Arc<WorkspaceRunAuthority>> {
        match &self.workspace {
            WorkspaceAuthority::Legacy => None,
            WorkspaceAuthority::Bound(authority) => Some(authority),
        }
    }

    /// Derive a child call context while preserving workspace authority.
    #[must_use]
    pub fn derived(
        &self,
        session_id: Option<String>,
        schedule_id: Option<String>,
        tool_call_id: impl Into<String>,
    ) -> Self {
        Self {
            run_id: self.run_id.clone(),
            session_id,
            schedule_id,
            tool_call_id: tool_call_id.into(),
            workspace: self.workspace.clone(),
            egress: self.egress.clone(),
            egress_resources: self.egress_resources.clone(),
            egress_policy_unavailable: self.egress_policy_unavailable,
            authorization_generation: self.authorization_generation,
        }
    }
}

impl fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolContext")
            .field("run_id", &self.run_id)
            .field("session_id", &self.session_id)
            .field("schedule_id", &self.schedule_id)
            .field("tool_call_id", &self.tool_call_id)
            .field("egress_bound", &self.egress.is_some())
            .field(
                "workspace",
                &match self.workspace {
                    WorkspaceAuthority::Legacy => "legacy",
                    WorkspaceAuthority::Bound(_) => "bound",
                },
            )
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// Bad input / unknown tool — the model gets the message and may retry
    #[error("invalid: {0}")]
    Invalid(String),
    /// Blocked by policy (capability whitelist or approval gate) — fail-closed
    #[error("denied: {0}")]
    Denied(String),
    /// The tool ran and failed
    #[error("failed: {0}")]
    Failed(String),
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    #[error("cancelled")]
    Cancelled,
    /// The approval gate decided the whole run must stop (user chose abort)
    #[error("run aborted: {0}")]
    AbortRun(String),
}

/// What the approval gate says about one requires-approval dispatch.
pub enum GateDecision {
    Allow,
    Deny(String),
    /// Deny this call AND cancel the whole run
    AbortRun(String),
}

/// The policy hook consulted for every `requires_approval` tool. C3 ships the
/// fail-closed [`DenyAllGate`]; C4 installs an interactive broker-backed gate.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    /// `standing_target` is this call's value for the tool's declared target
    /// argument, when it has one and the call filled it — the only thing a
    /// target-scoped standing grant (H4) may ever be bound to. `None` means no
    /// such grant can be offered for this call.
    async fn check(
        &self,
        info: &ToolInfo,
        ctx: &ToolContext,
        input: &Map<String, Value>,
        standing_target: Option<&str>,
        cancel: &CancellationToken,
    ) -> GateDecision;

    /// H8 plan-mode approval. A submitted plan unlocks the full tool set for the
    /// rest of the run, so this ALWAYS asks a human — there is deliberately no
    /// auto-approval path (unlike [`check`], which a Guardian may fast-path).
    /// Default is fail-closed: with no interactive channel installed a plan can
    /// never proceed, exactly as `DenyAllGate` denies every gated tool.
    async fn check_plan(
        &self,
        _run_id: &str,
        _session_id: Option<&str>,
        _tool_call_id: &str,
        _summary: String,
        _payload: Map<String, Value>,
        _cancel: &CancellationToken,
    ) -> GateDecision {
        GateDecision::Deny(
            "plan mode requires an interactive approval channel (fail-closed)".to_owned(),
        )
    }

    /// Re-apply only the grant side-effect of a durable approval after restart.
    /// The one approved call is executed separately by `execute_preapproved`.
    async fn settle_resumed(
        &self,
        _approval_id: &str,
        _decision: &Decision,
        _info: &ToolInfo,
        _ctx: &ToolContext,
        _standing_target: Option<&str>,
    ) {
    }
}

/// A user's local adjustment of a tool's declared [`RiskClass`] (H2).
///
/// **Inviolable rule: this is USER-LOCAL and is never written by a module,
/// persona, or MCP server.** A package may *declare* what tools it wants; only
/// the person who owns the machine decides how far to trust them. If an
/// installer could write here, a marketplace entry would ship its own
/// exemption, and the conservative default for third-party code would be
/// worth nothing. Module tool risk is also host-computed and may be tightened,
/// but never relaxed below its configured class, so L-APPR-5 cannot be skipped.
pub trait RiskOverrides: Send + Sync {
    /// The user's class for `tool_name`, or `None` to keep the declared one.
    fn resolve(&self, tool_name: &str) -> Option<RiskClass>;
}

/// Fail-closed default: everything needing approval is denied.
pub struct DenyAllGate;

#[async_trait]
impl ApprovalGate for DenyAllGate {
    async fn check(
        &self,
        info: &ToolInfo,
        _ctx: &ToolContext,
        _input: &Map<String, Value>,
        _standing_target: Option<&str>,
        _cancel: &CancellationToken,
    ) -> GateDecision {
        GateDecision::Deny(format!(
            "tool {} requires approval and no approval channel is installed (fail-closed)",
            info.name
        ))
    }
}

/// One callable tool. `parameters` is the JSON Schema advertised to the model;
/// `call` returns the string handed back as the tool result message.
#[async_trait]
pub trait Tool: Send + Sync {
    fn info(&self) -> ToolInfo;

    /// Whether this tool always needs a registry-bound egress gate, regardless
    /// of per-run source provenance. Source-aware tools enforce the gate at
    /// their actual boundary using `ToolContext::authorize_egress_if_restricted`.
    fn requires_outbound_policy(&self) -> bool {
        true
    }

    async fn advertisable(&self) -> bool {
        true
    }

    /// JSON Schema for the input object
    fn parameters(&self) -> Value;

    /// The input field naming *where this call sends things* — the channel
    /// address, the recipient, the repository. `None` (the default) means the
    /// tool is not eligible for a target-scoped standing grant (H4) at all.
    ///
    /// Declared rather than guessed. A heuristic over parameter names would
    /// eventually bind a grant to the wrong field, and the failure mode of that
    /// mistake is a standing authorisation the user never meant to give.
    fn target_arg(&self) -> Option<String> {
        None
    }

    /// Per-tool execution budget, enforced by the registry
    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn isolates_cancellation(&self) -> bool {
        false
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        input: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError>;
}

#[derive(serde::Serialize)]
pub struct ModuleToolContext {
    pub run_id: String,
    pub session_id: Option<String>,
    pub tool_call_id: String,
    pub module_id: String,
    pub operation: String,
    pub authorized_resources: Vec<String>,
    pub authorization_ref: String,
}

#[async_trait]
pub trait ModuleToolAuthorization: Send + Sync {
    async fn authorize(
        &self,
        module: &str,
        operation: &str,
        ctx: &ToolContext,
    ) -> Result<ModuleToolGrantContext, ModuleToolCallError>;

    async fn has_current_consent(&self, _module: &str, _operation: &str) -> bool {
        false
    }
}

#[derive(Debug, Clone)]
pub struct ModuleToolGrantContext {
    pub authorized_resources: Vec<String>,
    pub authorization_ref: String,
    pub per_call_approval: bool,
}
pub struct DenyModuleToolAuthorization;

fn permission_denied() -> ModuleToolCallError {
    ModuleToolCallError::Module {
        code: ModuleToolErrorCode::PermissionDenied,
        retryable: false,
        details: None,
        unknown_code: None,
    }
}

#[async_trait]
impl ModuleToolAuthorization for DenyModuleToolAuthorization {
    async fn authorize(
        &self,
        _: &str,
        _: &str,
        _: &ToolContext,
    ) -> Result<ModuleToolGrantContext, ModuleToolCallError> {
        Err(permission_denied())
    }

    async fn has_current_consent(&self, _: &str, _: &str) -> bool {
        false
    }
}

#[async_trait]
pub trait ModuleToolRuntime: Send + Sync {
    async fn check_available(
        &self,
        module: &str,
        operation: &str,
    ) -> Result<(), ModuleToolCallError>;
    async fn invoke(
        &self,
        context: ModuleToolContext,
        arguments: Map<String, Value>,
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<ModuleToolResult, ModuleToolCallError>;
}

pub struct ModuleTool {
    module: String,
    operation: String,
    name: String,
    description: String,
    schema: Value,
    timeout: Duration,
    inline_wait: Duration,
    risk: RiskClass,
    advert_view: Arc<dyn agent24_domain::tool::ModuleToolAdvertView>,
    authorization: Arc<dyn ModuleToolAuthorization>,
    runtime: Arc<dyn ModuleToolRuntime>,
}

impl ModuleTool {
    #[allow(clippy::too_many_arguments)] // Mirrors the module advert plus the two host adapters.
    pub fn new(
        module: impl Into<String>,
        operation: impl Into<String>,
        description: impl Into<String>,
        schema: Value,
        timeout: Duration,
        inline_wait: Duration,
        risk: RiskClass,
        advert_view: Arc<dyn agent24_domain::tool::ModuleToolAdvertView>,
        authorization: Arc<dyn ModuleToolAuthorization>,
        runtime: Arc<dyn ModuleToolRuntime>,
    ) -> Result<Self, &'static str> {
        if inline_wait >= timeout {
            return Err("inline_wait must be shorter than timeout");
        }
        let module = module.into();
        let operation = operation.into();
        Ok(Self {
            name: format!("{module}.{operation}"),
            module,
            operation,
            description: description.into(),
            schema,
            timeout,
            inline_wait,
            risk,
            advert_view,
            authorization,
            runtime,
        })
    }
}

#[async_trait]
impl Tool for ModuleTool {
    fn requires_outbound_policy(&self) -> bool {
        // Enforced dynamically at invocation time via
        // `ToolContext::authorize_egress_if_restricted`.
        false
    }

    async fn advertisable(&self) -> bool {
        self.advert_view.module_ready(&self.module).await
            && self
                .advert_view
                .operation_available(&self.module, &self.operation)
                .await
            && self
                .advert_view
                .has_current_consent(&self.module, &self.operation)
                .await
            && self
                .advert_view
                .outbound_gate_ready(&self.module, &self.operation)
                .await
            && !self
                .advert_view
                .blocked_by_remote_tier_guard(&self.module, &self.operation)
                .await
    }

    fn info(&self) -> ToolInfo {
        ToolInfo::new(
            self.name.clone(),
            "module",
            self.description.clone(),
            self.risk,
        )
    }
    fn parameters(&self) -> Value {
        self.schema.clone()
    }
    fn timeout(&self) -> Duration {
        self.timeout
    }
    fn isolates_cancellation(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        input: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        ctx.authorize_egress_if_restricted(
            EgressPurpose::ModuleTool,
            EgressDestination::exact(&self.name),
        )
        .await?;
        let grant = self
            .authorization
            .authorize(&self.module, &self.operation, ctx)
            .await
            .map_err(|error| ToolError::Failed(module_error_json(error)))?;
        if grant.per_call_approval && !self.risk.requires_approval() {
            return Err(ToolError::Failed(module_error_json(permission_denied())));
        }
        if let Err(error) = self
            .runtime
            .check_available(&self.module, &self.operation)
            .await
        {
            return Err(ToolError::Failed(module_error_json(error)));
        }
        let context = ModuleToolContext {
            run_id: ctx.run_id().to_owned(),
            session_id: ctx.session_id().map(str::to_owned),
            tool_call_id: ctx.tool_call_id().to_owned(),
            module_id: self.module.clone(),
            operation: self.operation.clone(),
            authorized_resources: grant.authorized_resources,
            authorization_ref: grant.authorization_ref,
        };
        let call_cancel = cancel.child_token();
        let result = match tokio::time::timeout(
            self.inline_wait,
            self.runtime
                .invoke(context, input.clone(), self.timeout, call_cancel.clone()),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                call_cancel.cancel();
                return Err(ToolError::Failed(module_error_json(
                    ModuleToolCallError::ResultUnknown,
                )));
            }
        };
        match result {
            Ok(ModuleToolResult::Completed { payload, replayed }) => Ok(serde_json::json!({
                "kind":"completed", "payload":payload, "replayed":replayed
            })
            .to_string()),
            Ok(ModuleToolResult::Pending { .. }) => Err(ToolError::Failed(module_error_json(
                ModuleToolCallError::InvalidResult,
            ))),
            Err(failure) => Err(ToolError::Failed(module_error_json(failure))),
        }
    }
}

fn module_error_json(failure: ModuleToolCallError) -> String {
    let (code, retryable, unknown, result_unknown) = match failure {
        ModuleToolCallError::Module {
            code,
            retryable,
            unknown_code,
            ..
        } => (code, retryable, unknown_code, false),
        ModuleToolCallError::InvalidResult => {
            (ModuleToolErrorCode::InvalidResult, false, None, false)
        }
        ModuleToolCallError::ResultUnknown => {
            (ModuleToolErrorCode::ResultUnknown, false, None, true)
        }
        ModuleToolCallError::ResponseTooLarge => {
            (ModuleToolErrorCode::ResponseTooLarge, false, None, true)
        }
        ModuleToolCallError::Cancelled => (ModuleToolErrorCode::Cancelled, false, None, false),
        ModuleToolCallError::Timeout => (ModuleToolErrorCode::Timeout, false, None, false),
        ModuleToolCallError::ModuleUnavailable => {
            (ModuleToolErrorCode::ModuleUnavailable, false, None, false)
        }
    };
    let module_code = unknown.map(|raw| {
        raw.chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            .take(64)
            .collect::<String>()
    });
    serde_json::json!({"error":{"code":code,"retryable":retryable,"module_code":module_code,"result_unknown":result_unknown}}).to_string()
}

fn unknown_module_outcome(code: &str) -> ToolError {
    ToolError::Failed(
        serde_json::json!({"error":{"code":code,"retryable":false,"result_unknown":true}})
            .to_string(),
    )
}

/// What the agent loop advertises to the model (provider-neutral; the models
/// crate maps this onto the OpenAI function-calling wire shape).
#[derive(Debug, Clone)]
pub struct ToolAdvert {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
    /// Capability whitelist (C3: name-based). A registered-but-not-whitelisted
    /// tool is listable yet not dispatchable — deny wins over registration.
    allowed: BTreeSet<String>,
    gate: Arc<dyn ApprovalGate>,
    /// True once an interactive gate is installed — only then are
    /// requires-approval tools advertised to the model
    interactive_gate: bool,
    /// User-local risk adjustments (H2). None → declared classes stand.
    overrides: Option<Arc<dyn RiskOverrides>>,
    outbound_gate: Option<Arc<dyn EgressGate>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
            allowed: BTreeSet::new(),
            gate: Arc::new(DenyAllGate),
            interactive_gate: false,
            overrides: None,
            outbound_gate: None,
        }
    }

    /// Install the user's local risk overrides (H2).
    #[must_use]
    pub fn with_risk_overrides(mut self, overrides: Arc<dyn RiskOverrides>) -> Self {
        self.overrides = Some(overrides);
        self
    }

    /// Install an interactive approval gate (C4 broker). Requires-approval
    /// tools become advertisable; every dispatch of one still passes through
    /// the gate.
    #[must_use]
    pub fn with_gate(mut self, gate: Arc<dyn ApprovalGate>) -> Self {
        self.gate = gate;
        self.interactive_gate = true;
        self
    }

    /// Install the host-owned live egress checker.
    #[must_use]
    pub fn with_egress_gate(mut self, gate: Arc<dyn EgressGate>) -> Self {
        self.outbound_gate = Some(gate);
        self
    }

    /// Register a tool and whitelist it (the default for builtins).
    #[must_use]
    pub fn with(mut self, tool: Arc<dyn Tool>) -> Self {
        let name = tool.info().name;
        if self.tools.contains_key(&name) {
            tracing::warn!("refusing to register duplicate tool name {name}");
            return self;
        }
        self.allowed.insert(name.clone());
        self.tools.insert(name, tool);
        self
    }

    /// Register without whitelisting (dispatch will deny; used by tests and,
    /// later, by policy-managed module tools).
    #[must_use]
    pub fn with_unlisted(mut self, tool: Arc<dyn Tool>) -> Self {
        let name = tool.info().name;
        if self.tools.contains_key(&name) {
            tracing::warn!("refusing to register duplicate tool name {name}");
            return self;
        }
        self.tools.insert(name, tool);
        self
    }

    #[must_use]
    pub fn register_module_tool(&mut self, tool: Arc<dyn Tool>) -> Result<(), String> {
        let name = tool.info().name;
        if self.tools.contains_key(&name) {
            return Err(format!("refusing to register duplicate tool name {name}"));
        }
        self.allowed.insert(name.clone());
        self.tools.insert(name, tool);
        Ok(())
    }

    /// The default builtin set rooted at `workspace` (fs whitelist + shell cwd).
    pub fn builtin(workspace: std::path::PathBuf) -> Self {
        Self::new()
            .with(Arc::new(HttpFetchTool::new(false)))
            .with(Arc::new(FsReadTool::new(vec![workspace.clone()])))
            .with(Arc::new(FsWriteTool::new(vec![workspace.clone()])))
            .with(Arc::new(ShellExecTool::new(workspace)))
    }

    /// The local-read-only subset — `fs_read` ONLY (H9).
    ///
    /// This is what the explorer subagent runs against, and the guarantee is
    /// STRUCTURAL: the registry does not contain `fs_write`, `shell_exec`, the
    /// explorer itself, OR `http_fetch`, so a sub-run cannot write, execute,
    /// recurse, or reach the network no matter what the model asks for. There
    /// is nothing to bypass because those tools were never registered.
    ///
    /// `http_fetch` is deliberately EXCLUDED even though it is `Read`-class.
    /// `Read` means "no side effect on the machine", not "no egress": a GET can
    /// still send the workspace bytes an `fs_read` just returned to an arbitrary
    /// URL. In an ungated, model-spawned helper that is an exfiltration channel,
    /// so the explorer gets no network. A network-capable researcher, if ever
    /// wanted, must be a separate, GATED tool — not this one.
    pub fn read_only(workspace: std::path::PathBuf) -> Self {
        let reg = Self::new().with(Arc::new(FsReadTool::new(vec![workspace])));
        debug_assert!(
            reg.list().iter().all(|t| t.risk_class == RiskClass::Read),
            "read_only registry must contain only Read-class tools"
        );
        reg
    }

    /// True when an interactive approval gate (C4 broker) is installed.
    pub fn gate_is_interactive(&self) -> bool {
        self.interactive_gate
    }

    /// The class that actually governs a call: the tool's declared class, as
    /// adjusted by the user's local overrides (H2).
    ///
    /// **A user may correct our guess; they may not overrule our knowledge.**
    /// `external` on a third-party tool is a guess made in the absence of
    /// information — we did not write that code and cannot bound its effects —
    /// and the person who owns the machine has standing to correct it. A
    /// builtin's class is not a guess: we wrote `shell_exec` and know it runs
    /// commands. So an override may always TIGHTEN, and may relax anything
    /// third-party, but may not relax a builtin or module tool along
    /// [`RiskClass::escape_rank`].
    ///
    /// That single rule is what stops `shell_exec → read` ("stop asking me
    /// about shell") and `shell_exec → external` (which would quietly make it
    /// eligible for a standing grant under H4) — the two ways an override could
    /// turn into a permanent hole nobody remembers opening.
    pub fn effective_risk(&self, info: &ToolInfo) -> RiskClass {
        let declared = info.risk_class;
        let Some(over) = self
            .overrides
            .as_ref()
            .and_then(|o| o.resolve(&info.name))
            .filter(|o| *o != declared)
        else {
            return declared;
        };
        if matches!(info.source.as_str(), "builtin" | "module")
            && over.escape_rank() > declared.escape_rank()
        {
            tracing::warn!(
                "ignoring override {declared:?} → {over:?} for {} {}: this source's class \
                 may be tightened but not relaxed",
                info.source,
                info.name
            );
            return declared;
        }
        over
    }

    /// Whether dispatching `name` would consult the approval gate.
    pub fn tool_requires_approval(&self, name: &str) -> bool {
        self.tool_risk_class(name)
            .is_some_and(RiskClass::requires_approval)
    }

    /// The effective side-effect class of `name` (H1 + H2), if registered.
    pub fn tool_risk_class(&self, name: &str) -> Option<RiskClass> {
        self.tools
            .get(name.trim())
            .map(|t| self.effective_risk(&t.info()))
    }

    /// Sorted list for `GET /api/v1/tools`.
    ///
    /// Reports the EFFECTIVE class, not the declared one: the endpoint answers
    /// "what will happen if this is called", and a UI that showed a declared
    /// `external` for a tool the user has relaxed to `read` would be lying
    /// about the next dispatch.
    pub fn list(&self) -> Vec<ToolInfo> {
        self.tools
            .values()
            .filter(|tool| !tool.requires_outbound_policy() || self.outbound_gate.is_some())
            .map(|t| {
                let info = t.info();
                let effective = self.effective_risk(&info);
                ToolInfo::new(info.name, info.source, info.description, effective)
            })
            .collect()
    }

    /// Tools advertised to the model: whitelisted AND executable. Without an
    /// interactive gate, requires-approval tools are NOT advertised —
    /// offering a tool that dispatch always denies just burns model
    /// iterations. With one (C4), they are advertised and gated per call.
    pub fn adverts(&self) -> Vec<ToolAdvert> {
        self.tools
            .values()
            .filter(|t| {
                let info = t.info();
                self.allowed.contains(&info.name)
                    && (!t.requires_outbound_policy() || self.outbound_gate.is_some())
                    && (self.interactive_gate || !self.effective_risk(&info).requires_approval())
            })
            .map(|t| {
                let info = t.info();
                ToolAdvert {
                    name: info.name,
                    description: info.description,
                    parameters: t.parameters(),
                }
            })
            .collect()
    }

    pub async fn live_adverts(&self) -> Vec<ToolAdvert> {
        let mut adverts = Vec::new();
        for tool in self.tools.values() {
            let info = tool.info();
            if self.allowed.contains(&info.name)
                && (!tool.requires_outbound_policy() || self.outbound_gate.is_some())
                && (self.interactive_gate || !self.effective_risk(&info).requires_approval())
                && tool.advertisable().await
            {
                adverts.push(ToolAdvert {
                    name: info.name,
                    description: info.description,
                    parameters: tool.parameters(),
                });
            }
        }
        adverts
    }

    pub async fn live_plan_adverts(&self) -> Vec<ToolAdvert> {
        self.live_adverts()
            .await
            .into_iter()
            .filter(|a| self.tool_risk_class(&a.name) == Some(RiskClass::Read))
            .collect()
    }

    /// Tools advertised to the model while a run is in plan mode (H8): the
    /// read-only subset only. Write/exec/external tools are structurally absent
    /// from the model's options — there is nothing to bypass because they were
    /// never offered — until it submits a plan and the human approves it. The
    /// agent loop pairs this list with a synthetic `propose_plan` advert.
    pub fn plan_adverts(&self) -> Vec<ToolAdvert> {
        self.adverts()
            .into_iter()
            .filter(|a| self.tool_risk_class(&a.name) == Some(RiskClass::Read))
            .collect()
    }

    /// H8: ask the installed gate for a human decision on a submitted plan.
    /// Fail-closed when no interactive channel exists (see
    /// [`ApprovalGate::check_plan`]). `Allow` means the human approved it and
    /// the run may leave read-only.
    pub async fn request_plan(
        &self,
        run_id: &str,
        session_id: Option<&str>,
        tool_call_id: &str,
        summary: String,
        payload: Map<String, Value>,
        cancel: &CancellationToken,
    ) -> GateDecision {
        self.gate
            .check_plan(run_id, session_id, tool_call_id, summary, payload, cancel)
            .await
    }

    /// The dispatch pipeline. Every policy refusal is `ToolError::Denied` so
    /// the caller can persist a `denied` tool call + audit entry.
    pub async fn dispatch(
        &self,
        name: &str,
        ctx: &ToolContext,
        input: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        // 1. normalize / resolve
        let name = name.trim();
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::Invalid(format!("unknown tool: {name}")))?;
        if tool.requires_outbound_policy() && self.outbound_gate.is_none() {
            return Err(ToolError::Denied("outbound policy unavailable".into()));
        }

        // 2. capability whitelist
        if !self.allowed.contains(name) {
            return Err(ToolError::Denied(format!(
                "tool {name} is not in the capability whitelist"
            )));
        }

        // 3. approval gate — every requires-approval dispatch consults the
        // installed gate (fail-closed DenyAllGate unless C4's broker is wired).
        //
        // The gate is handed the EFFECTIVE info (declared class as adjusted by
        // the user's overrides), so the approval record it writes and the
        // Guardian assessment it may run both describe the call as it will
        // actually be governed, not as the tool declared itself.
        //
        // The predicate reads `risk_class`, NOT the `requires_approval` wire
        // field: the field is derived output kept for pre-H1 clients, and a
        // deserialized ToolInfo could in principle carry a stale `false`
        // alongside a gated class. Deriving at the decision point makes that
        // combination unrepresentable in the gate's view.
        let declared = tool.info();
        let effective = self.effective_risk(&declared);
        let info = ToolInfo::new(
            declared.name,
            declared.source,
            declared.description,
            effective,
        );
        // Resolve the standing-grant target BEFORE the gate: eligibility is a
        // property of this call (tool declares a target arg AND the call filled
        // it), and the gate must not have to reach back into the registry to
        // work it out.
        let standing_target = tool
            .target_arg()
            .and_then(|arg| input.get(&arg).and_then(Value::as_str))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned);
        if effective.requires_approval() {
            match self
                .gate
                .check(&info, ctx, input, standing_target.as_deref(), cancel)
                .await
            {
                GateDecision::Allow => {}
                GateDecision::Deny(reason) => return Err(ToolError::Denied(reason)),
                GateDecision::AbortRun(reason) => return Err(ToolError::AbortRun(reason)),
            }
        }

        // 4. execute under the tool's budget, cancellable at any point
        let call_ctx = self.outbound_gate.as_ref().map_or_else(
            || ctx.clone(),
            |gate| ctx.clone().with_egress_gate(Arc::clone(gate)),
        );
        Self::run_budgeted(tool, &call_ctx, input, cancel).await
    }

    /// Execute a tool whose approval was ALREADY granted out of band — the
    /// durable-resume path (H3). When a run is resumed after a restart (or after
    /// a parked approval is answered with no dispatch waiting), the human's
    /// decision already exists on the approval row; the broker's `settle_resumed`
    /// has replayed its grant side-effects and returned `Approved`. Running the
    /// call back through `dispatch` would ask a SECOND time.
    ///
    /// So this runs the same normalize + whitelist + budgeted-execute path but
    /// deliberately SKIPS the gate. It is the one execution path that does not
    /// ask, and must only be reached with an approval already in hand.
    pub async fn execute_preapproved(
        &self,
        name: &str,
        ctx: &ToolContext,
        input: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        let name = name.trim();
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::Invalid(format!("unknown tool: {name}")))?;
        // The whitelist is still enforced: "already approved" authorises the
        // side effect, not dispatch of a tool that was never dispatchable.
        if !self.allowed.contains(name) {
            return Err(ToolError::Denied(format!(
                "tool {name} is not in the capability whitelist"
            )));
        }
        if tool.requires_outbound_policy() && self.outbound_gate.is_none() {
            return Err(ToolError::Denied("outbound policy unavailable".into()));
        }
        let call_ctx = self.outbound_gate.as_ref().map_or_else(
            || ctx.clone(),
            |gate| ctx.clone().with_egress_gate(Arc::clone(gate)),
        );
        Self::run_budgeted(tool, &call_ctx, input, cancel).await
    }

    /// Restore grant side-effects for a durable approval without asking again.
    /// Current tool/risk policy is re-applied; if it no longer permits the
    /// recorded grant, the grant is simply not restored and the next call asks.
    pub async fn settle_resumed(
        &self,
        name: &str,
        ctx: &ToolContext,
        input: &Map<String, Value>,
        approval_id: &str,
        decision: &Decision,
        recorded_target: Option<&str>,
    ) {
        let name = name.trim();
        let Some(tool) = self.tools.get(name) else {
            return;
        };
        if !self.allowed.contains(name) {
            return;
        }
        let declared = tool.info();
        let effective = self.effective_risk(&declared);
        if !effective.requires_approval() {
            return;
        }
        let info = ToolInfo::new(
            declared.name,
            declared.source,
            declared.description,
            effective,
        );
        let standing_target = tool
            .target_arg()
            .and_then(|arg| input.get(&arg).and_then(Value::as_str))
            .map(str::trim)
            .filter(|target| !target.is_empty());
        if (decision.kind == "approve_for_session" && recorded_target.is_some())
            || (decision.kind == "approve_for_target" && recorded_target != standing_target)
        {
            tracing::warn!(
                "approval {approval_id}: recorded target no longer matches approved payload; not restoring grant"
            );
            return;
        }
        self.gate
            .settle_resumed(approval_id, decision, &info, ctx, standing_target)
            .await;
    }

    /// Step 4 of both dispatch paths: run the tool under its timeout budget,
    /// cancellable at any point.
    async fn run_budgeted(
        tool: &Arc<dyn Tool>,
        ctx: &ToolContext,
        input: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        let budget = tool.timeout();
        let call_cancel = if tool.isolates_cancellation() {
            cancel.child_token()
        } else {
            cancel.clone()
        };
        tokio::select! {
            r = tokio::time::timeout(budget, tool.call(ctx, input, &call_cancel)) => {
                match r {
                    Ok(result) => result,
                    Err(_) if tool.isolates_cancellation() => {
                        call_cancel.cancel();
                        Err(unknown_module_outcome("timeout"))
                    }
                    Err(_) => Err(ToolError::Timeout(budget)),
                }
            }
            () = cancel.cancelled() => {
                call_cancel.cancel();
                if tool.isolates_cancellation() {
                    Err(unknown_module_outcome("cancelled"))
                } else {
                    Err(ToolError::Cancelled)
                }
            }
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Compact single-line summary of a tool input for events/logs (full input is
/// audit-only). Truncated on a char boundary.
pub fn summarize_input(input: &Map<String, Value>) -> String {
    let s = Value::Object(input.clone()).to_string();
    truncate(&s, 200)
}

/// Truncate to at most `max` bytes on a char boundary, appending an ellipsis
/// marker when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated {} bytes]", &s[..end], s.len() - end)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    struct DenyAdvertView;

    #[async_trait]
    impl agent24_domain::tool::ModuleToolAdvertView for DenyAdvertView {
        async fn blocked_by_remote_tier_guard(&self, _: &str, _: &str) -> bool {
            true
        }
    }

    struct SlowTool;

    #[async_trait]
    impl Tool for SlowTool {
        fn requires_outbound_policy(&self) -> bool {
            false
        }

        fn info(&self) -> ToolInfo {
            ToolInfo::new("slow", "builtin", "sleeps", RiskClass::Read)
        }
        fn parameters(&self) -> Value {
            serde_json::json!({"type": "object"})
        }
        fn timeout(&self) -> Duration {
            Duration::from_millis(100)
        }
        async fn call(
            &self,
            _ctx: &ToolContext,
            _input: &Map<String, Value>,
            cancel: &CancellationToken,
        ) -> Result<String, ToolError> {
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(60)) => Ok("done".to_owned()),
                () = cancel.cancelled() => Err(ToolError::Cancelled),
            }
        }
    }

    fn ctx() -> ToolContext {
        ToolContext::legacy("run_test", None, None, "tc_test")
    }

    #[tokio::test]
    async fn unknown_tool_is_invalid() {
        let reg = ToolRegistry::new();
        let err = reg
            .dispatch("nope", &ctx(), &Map::new(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)), "{err}");
    }

    #[tokio::test]
    async fn non_whitelisted_tool_is_denied() {
        let reg = ToolRegistry::new().with_unlisted(Arc::new(SlowTool));
        let err = reg
            .dispatch("slow", &ctx(), &Map::new(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)), "{err}");
    }

    #[tokio::test]
    async fn ordinary_http_fetch_is_advertised_without_egress_policy() {
        let registry = ToolRegistry::new().with(Arc::new(HttpFetchTool::new(false)));
        assert_eq!(registry.adverts().len(), 1);
        assert!(registry.list().iter().any(|tool| tool.name == "http_fetch"));
    }

    #[tokio::test]
    async fn approval_stub_auto_denies_shell_exec_and_fs_write() {
        let dir = tempfile::tempdir().unwrap();
        let reg = ToolRegistry::builtin(dir.path().to_path_buf());
        for name in ["shell_exec", "fs_write"] {
            let err = reg
                .dispatch(name, &ctx(), &Map::new(), &CancellationToken::new())
                .await
                .unwrap_err();
            assert!(matches!(err, ToolError::Denied(_)), "{name}: {err}");
        }
        // and they are not advertised to the model
        let advertised: Vec<String> = reg.adverts().into_iter().map(|a| a.name).collect();
        assert_eq!(advertised, vec!["fs_read", "http_fetch"]);
        // Run-scoped egress checks live at actual network/process boundaries;
        // listing does not blanket-disable ordinary runs with no source resources.
        let listed = reg.list();
        assert!(listed.iter().any(|tool| tool.name == "shell_exec"));
        assert!(listed.iter().any(|tool| tool.name == "http_fetch"));
    }

    /// H1's whole point: `requires_approval` is DERIVED, so it cannot drift
    /// from the declared class the way two hand-maintained lists do. Asserted
    /// over the real registry rather than over constructed samples — a future
    /// tool that finds some way to set the field independently fails here.
    #[test]
    fn requires_approval_is_derived_for_every_registered_tool() {
        let dir = tempfile::tempdir().unwrap();
        for info in ToolRegistry::builtin(dir.path().to_path_buf()).list() {
            assert_eq!(
                info.requires_approval,
                info.risk_class.requires_approval(),
                "{} declares {:?} but carries requires_approval={}",
                info.name,
                info.risk_class,
                info.requires_approval
            );
        }
    }

    // ── H2: user-local risk overrides ────────────────────────────────────────

    struct FixedOverride(&'static str, RiskClass);
    impl RiskOverrides for FixedOverride {
        fn resolve(&self, tool_name: &str) -> Option<RiskClass> {
            (tool_name == self.0).then_some(self.1)
        }
    }

    /// A gate that must never be reached. Relaxing a tool to `read` has to mean
    /// the gate is not consulted at all — not that it is consulted and happens
    /// to allow — because "no gated call exists" is what makes the Guardian
    /// interaction a non-question.
    struct ExplodingGate;
    #[async_trait]
    impl ApprovalGate for ExplodingGate {
        async fn check(
            &self,
            info: &ToolInfo,
            _ctx: &ToolContext,
            _input: &Map<String, Value>,
            _standing_target: Option<&str>,
            _cancel: &CancellationToken,
        ) -> GateDecision {
            panic!("gate consulted for {} — it should not have been", info.name);
        }
    }

    fn mcp_style_tool() -> Arc<dyn Tool> {
        struct Remote;
        #[async_trait]
        impl Tool for Remote {
            fn requires_outbound_policy(&self) -> bool {
                false
            }

            fn info(&self) -> ToolInfo {
                ToolInfo::new(
                    "mcp_fs_read",
                    "mcp",
                    "third-party read",
                    RiskClass::External,
                )
            }
            fn parameters(&self) -> Value {
                serde_json::json!({"type": "object"})
            }
            async fn call(
                &self,
                _ctx: &ToolContext,
                _input: &Map<String, Value>,
                _cancel: &CancellationToken,
            ) -> Result<String, ToolError> {
                Ok("ran".to_owned())
            }
        }
        Arc::new(Remote)
    }

    struct ModuleFixtureRuntime;
    #[async_trait]
    impl ModuleToolRuntime for ModuleFixtureRuntime {
        async fn check_available(&self, _: &str, _: &str) -> Result<(), ModuleToolCallError> {
            Ok(())
        }
        async fn invoke(
            &self,
            _: ModuleToolContext,
            _: Map<String, Value>,
            _: Duration,
            _: CancellationToken,
        ) -> Result<ModuleToolResult, ModuleToolCallError> {
            Err(ModuleToolCallError::ModuleUnavailable)
        }
    }

    fn module_fixture(name: &str, operation: &str, risk: RiskClass) -> Arc<dyn Tool> {
        Arc::new(
            ModuleTool::new(
                name,
                operation,
                "module fixture",
                serde_json::json!({"type":"object"}),
                Duration::from_secs(2),
                Duration::from_secs(1),
                risk,
                Arc::new(DenyAdvertView),
                Arc::new(DenyModuleToolAuthorization),
                Arc::new(ModuleFixtureRuntime),
            )
            .unwrap(),
        )
    }

    #[test]
    fn module_tool_risk_override_cannot_relax_declared_risk() {
        let reg = ToolRegistry::new()
            .with(module_fixture("sample", "write", RiskClass::WriteLocal))
            .with_risk_overrides(Arc::new(FixedOverride("sample.write", RiskClass::Read)));

        assert_eq!(
            reg.tool_risk_class("sample.write"),
            Some(RiskClass::WriteLocal)
        );
        assert!(reg.tool_requires_approval("sample.write"));
    }

    #[test]
    fn module_tool_registration_reports_duplicate_names_without_replacing_existing_tool() {
        let registry = ToolRegistry::new().with(module_fixture("sample", "write", RiskClass::Read));
        let mut registry = registry;
        assert!(matches!(
            registry.register_module_tool(module_fixture("sample", "write", RiskClass::External)),
            Err(error) if error.contains("duplicate tool name sample.write")
        ));
    }

    #[test]
    fn registering_a_module_name_collision_keeps_the_existing_tool() {
        struct Existing;
        #[async_trait]
        impl Tool for Existing {
            fn requires_outbound_policy(&self) -> bool {
                false
            }

            fn info(&self) -> ToolInfo {
                ToolInfo::new("sample.read", "builtin", "existing", RiskClass::Read)
            }
            fn parameters(&self) -> Value {
                serde_json::json!({"type":"object"})
            }
            async fn call(
                &self,
                _: &ToolContext,
                _: &Map<String, Value>,
                _: &CancellationToken,
            ) -> Result<String, ToolError> {
                Ok("existing".to_owned())
            }
        }

        let reg = ToolRegistry::new()
            .with(Arc::new(Existing))
            .with(module_fixture("sample", "read", RiskClass::WriteLocal));
        let listed = reg.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].source, "builtin");
        assert_eq!(listed[0].risk_class, RiskClass::Read);
    }

    fn targeted_external_tool() -> Arc<dyn Tool> {
        struct Remote;
        #[async_trait]
        impl Tool for Remote {
            fn requires_outbound_policy(&self) -> bool {
                false
            }

            fn info(&self) -> ToolInfo {
                ToolInfo::new("mcp_post", "mcp", "post", RiskClass::External)
            }
            fn parameters(&self) -> Value {
                serde_json::json!({"type":"object"})
            }
            fn target_arg(&self) -> Option<String> {
                Some("channel".to_owned())
            }
            async fn call(
                &self,
                _ctx: &ToolContext,
                _input: &Map<String, Value>,
                _cancel: &CancellationToken,
            ) -> Result<String, ToolError> {
                Ok("ran".to_owned())
            }
        }
        Arc::new(Remote)
    }

    struct CountingReplayGate(std::sync::atomic::AtomicUsize);
    #[async_trait]
    impl ApprovalGate for CountingReplayGate {
        async fn check(
            &self,
            _info: &ToolInfo,
            _ctx: &ToolContext,
            _input: &Map<String, Value>,
            _standing_target: Option<&str>,
            _cancel: &CancellationToken,
        ) -> GateDecision {
            GateDecision::Allow
        }
        async fn settle_resumed(
            &self,
            _approval_id: &str,
            _decision: &Decision,
            _info: &ToolInfo,
            _ctx: &ToolContext,
            _standing_target: Option<&str>,
        ) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn resumed_target_grant_requires_recorded_target_to_match_payload() {
        let gate = Arc::new(CountingReplayGate(std::sync::atomic::AtomicUsize::new(0)));
        let reg = ToolRegistry::new()
            .with(targeted_external_tool())
            .with_gate(gate.clone());
        let input = serde_json::json!({"channel":"#ops"})
            .as_object()
            .unwrap()
            .clone();
        let decision = Decision {
            kind: "approve_for_target".to_owned(),
            reason: None,
            extra: Map::new(),
        };

        reg.settle_resumed("mcp_post", &ctx(), &input, "apr", &decision, Some("#other"))
            .await;
        assert_eq!(gate.0.load(std::sync::atomic::Ordering::SeqCst), 0);
        reg.settle_resumed("mcp_post", &ctx(), &input, "apr", &decision, Some("#ops"))
            .await;
        assert_eq!(gate.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn user_may_relax_a_third_party_tool_and_the_gate_is_then_skipped() {
        let reg = ToolRegistry::new()
            .with(mcp_style_tool())
            .with_gate(Arc::new(ExplodingGate))
            .with_risk_overrides(Arc::new(FixedOverride("mcp_fs_read", RiskClass::Read)));

        assert_eq!(reg.tool_risk_class("mcp_fs_read"), Some(RiskClass::Read));
        assert!(!reg.tool_requires_approval("mcp_fs_read"));
        let out = reg
            .dispatch(
                "mcp_fs_read",
                &ctx(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out, "ran");
    }

    /// Durable resume (H3): a call whose approval was granted out of band runs
    /// via `execute_preapproved`, which must NOT consult the gate — the human
    /// already decided. `ExplodingGate` proves the gate is never touched even
    /// for an `external`, requires-approval tool.
    #[tokio::test]
    async fn execute_preapproved_runs_a_gated_tool_without_asking() {
        let reg = ToolRegistry::new()
            .with(mcp_style_tool()) // external → requires approval
            .with_gate(Arc::new(ExplodingGate));
        assert!(reg.tool_requires_approval("mcp_fs_read"));
        let out = reg
            .execute_preapproved(
                "mcp_fs_read",
                &ctx(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            out, "ran",
            "the tool must run without the gate being consulted"
        );
    }

    /// "Already approved" authorises the side effect, not dispatch of a tool that
    /// was never dispatchable: the whitelist is still enforced.
    #[tokio::test]
    async fn execute_preapproved_still_enforces_the_whitelist() {
        let reg = ToolRegistry::new().with_unlisted(mcp_style_tool());
        let err = reg
            .execute_preapproved(
                "mcp_fs_read",
                &ctx(),
                &Map::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)), "{err:?}");
    }

    /// The line the override rule draws: `external` on third-party code is a
    /// GUESS the user may correct; a builtin's class is KNOWLEDGE they may not
    /// overrule. Both relaxations here would otherwise be permanent holes —
    /// `read` stops the asking entirely, `external` quietly makes shell
    /// eligible for a standing grant under H4.
    #[test]
    fn a_builtin_may_not_be_relaxed() {
        let dir = tempfile::tempdir().unwrap();
        for attempt in [RiskClass::Read, RiskClass::External, RiskClass::WriteLocal] {
            let reg = ToolRegistry::builtin(dir.path().to_path_buf())
                .with_risk_overrides(Arc::new(FixedOverride("shell_exec", attempt)));
            assert_eq!(
                reg.tool_risk_class("shell_exec"),
                Some(RiskClass::Exec),
                "shell_exec must stay exec despite an override to {attempt:?}"
            );
            assert!(reg.tool_requires_approval("shell_exec"));
        }
    }

    /// Tightening is always the user's call, on anything.
    #[test]
    fn a_builtin_may_be_tightened() {
        let dir = tempfile::tempdir().unwrap();
        let reg = ToolRegistry::builtin(dir.path().to_path_buf())
            .with_risk_overrides(Arc::new(FixedOverride("fs_read", RiskClass::Exec)));
        assert_eq!(reg.tool_risk_class("fs_read"), Some(RiskClass::Exec));
        assert!(reg.tool_requires_approval("fs_read"));
        // The tightened read tool disappears; ordinary HTTP stays available.
        let advertised: Vec<String> = reg.adverts().into_iter().map(|a| a.name).collect();
        assert_eq!(advertised, vec!["http_fetch"]);
    }

    /// `GET /api/v1/tools` must describe what will actually happen on the next
    /// dispatch, not what the tool declared about itself.
    #[test]
    fn listing_reports_the_effective_class() {
        let reg = ToolRegistry::new()
            .with(mcp_style_tool())
            .with_risk_overrides(Arc::new(FixedOverride("mcp_fs_read", RiskClass::Read)));
        let listed = reg.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].risk_class, RiskClass::Read);
        assert!(!listed[0].requires_approval);
    }

    /// H1 is an additive migration: the gating outcome must be byte-for-byte
    /// what it was before the risk classes existed. If a future edit changes a
    /// builtin's class, this test is where the behaviour change surfaces —
    /// which is the point. Update it deliberately, never to make CI green.
    #[test]
    fn builtin_classes_preserve_pre_h1_gating_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let reg = ToolRegistry::builtin(dir.path().to_path_buf());
        let expected = [
            ("fs_read", RiskClass::Read, false),
            ("http_fetch", RiskClass::Read, false),
            ("fs_write", RiskClass::WriteLocal, true),
            ("shell_exec", RiskClass::Exec, true),
        ];
        for (name, class, gated) in expected {
            assert_eq!(reg.tool_risk_class(name), Some(class), "{name}");
            assert_eq!(reg.tool_requires_approval(name), gated, "{name}");
        }
    }

    #[tokio::test]
    async fn slow_tool_hits_its_timeout_budget() {
        let reg = ToolRegistry::new().with(Arc::new(SlowTool));
        let started = std::time::Instant::now();
        let err = reg
            .dispatch("slow", &ctx(), &Map::new(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Timeout(_)), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_running_tool() {
        struct Hanging;
        #[async_trait]
        impl Tool for Hanging {
            fn requires_outbound_policy(&self) -> bool {
                false
            }

            fn info(&self) -> ToolInfo {
                ToolInfo::new("hang", "builtin", "", RiskClass::Read)
            }
            fn parameters(&self) -> Value {
                serde_json::json!({"type": "object"})
            }
            async fn call(
                &self,
                _ctx: &ToolContext,
                _input: &Map<String, Value>,
                cancel: &CancellationToken,
            ) -> Result<String, ToolError> {
                cancel.cancelled().await;
                Err(ToolError::Cancelled)
            }
        }
        let reg = ToolRegistry::new().with(Arc::new(Hanging));
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c.cancel();
        });
        let started = std::time::Instant::now();
        let err = reg
            .dispatch("hang", &ctx(), &Map::new(), &cancel)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Cancelled), "{err}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn module_tool_authorization_defaults_to_denial_before_dispatch() {
        struct Runtime;
        #[async_trait]
        impl ModuleToolRuntime for Runtime {
            async fn check_available(&self, _: &str, _: &str) -> Result<(), ModuleToolCallError> {
                Ok(())
            }
            async fn invoke(
                &self,
                _: ModuleToolContext,
                _: Map<String, Value>,
                _: Duration,
                _: CancellationToken,
            ) -> Result<ModuleToolResult, ModuleToolCallError> {
                Err(ModuleToolCallError::ModuleUnavailable)
            }
        }
        assert!(
            ModuleTool::new(
                "m",
                "op",
                "description",
                serde_json::json!({"type":"object"}),
                Duration::from_secs(2),
                Duration::from_secs(1),
                RiskClass::External,
                Arc::new(DenyAdvertView),
                Arc::new(DenyModuleToolAuthorization),
                Arc::new(Runtime),
            )
            .unwrap()
            .call(&ctx(), &Map::new(), &CancellationToken::new())
            .await
            .unwrap_err()
            .to_string()
            .contains("permission_denied")
        );
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "中文中文中文";
        let t = truncate(s, 4);
        assert!(t.starts_with('中'));
        assert!(t.contains("truncated"));
        assert_eq!(truncate("short", 100), "short");
    }
}
