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

mod local;
mod net;

pub use local::{FsReadTool, FsWriteTool, ShellExecTool};
pub use net::HttpFetchTool;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use agent24_protocol::{RiskClass, ToolInfo};
use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

/// Per-call execution context.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub run_id: String,
    /// The session the run belongs to (scopes approve_for_session grants)
    pub session_id: Option<String>,
    /// The schedule that fired this run, when one did. A standing grant minted
    /// here belongs to the SCHEDULE rather than the session (H4): an unattended
    /// automation is the thing the user was consenting to, so revoking or
    /// deleting that automation must take its grants with it.
    pub schedule_id: Option<String>,
    /// The persisted tool-call row this execution belongs to
    pub tool_call_id: String,
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
}

/// A user's local adjustment of a tool's declared [`RiskClass`] (H2).
///
/// **Inviolable rule: this is USER-LOCAL and is never written by a module,
/// persona, or MCP server.** A package may *declare* what tools it wants; only
/// the person who owns the machine decides how far to trust them. If an
/// installer could write here, a marketplace entry would ship its own
/// exemption, and the conservative default for third-party code would be
/// worth nothing. Any future install path that touches this is a bug, not a
/// feature.
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

    async fn call(
        &self,
        ctx: &ToolContext,
        input: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError>;
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
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
            allowed: BTreeSet::new(),
            gate: Arc::new(DenyAllGate),
            interactive_gate: false,
            overrides: None,
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

    /// Register a tool and whitelist it (the default for builtins).
    #[must_use]
    pub fn with(mut self, tool: Arc<dyn Tool>) -> Self {
        let name = tool.info().name;
        self.allowed.insert(name.clone());
        self.tools.insert(name, tool);
        self
    }

    /// Register without whitelisting (dispatch will deny; used by tests and,
    /// later, by policy-managed module tools).
    #[must_use]
    pub fn with_unlisted(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.insert(tool.info().name, tool);
        self
    }

    /// The default builtin set rooted at `workspace` (fs whitelist + shell cwd).
    pub fn builtin(workspace: std::path::PathBuf) -> Self {
        Self::new()
            .with(Arc::new(HttpFetchTool::new(false)))
            .with(Arc::new(FsReadTool::new(vec![workspace.clone()])))
            .with(Arc::new(FsWriteTool::new(vec![workspace.clone()])))
            .with(Arc::new(ShellExecTool::new(workspace)))
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
    /// third-party, but may not relax a builtin along [`RiskClass::escape_rank`].
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
        if info.source == "builtin" && over.escape_rank() > declared.escape_rank() {
            tracing::warn!(
                "ignoring override {declared:?} → {over:?} for builtin {}: a builtin's class \
                 may be tightened but not relaxed",
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
        let budget = tool.timeout();
        tokio::select! {
            r = tokio::time::timeout(budget, tool.call(ctx, input, cancel)) => {
                r.map_err(|_| ToolError::Timeout(budget))?
            }
            () = cancel.cancelled() => Err(ToolError::Cancelled),
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

    struct SlowTool;

    #[async_trait]
    impl Tool for SlowTool {
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
        ToolContext {
            run_id: "run_test".to_owned(),
            session_id: None,
            schedule_id: None,
            tool_call_id: "tc_test".to_owned(),
        }
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
        // but ARE listed on /tools with the flag visible
        let listed = reg.list();
        assert_eq!(listed.len(), 4);
        assert!(
            listed
                .iter()
                .any(|t| t.name == "shell_exec" && t.requires_approval)
        );
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
        // and a tightened tool stops being advertised without an interactive gate
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

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "中文中文中文";
        let t = truncate(s, 4);
        assert!(t.starts_with('中'));
        assert!(t.contains("truncated"));
        assert_eq!(truncate("short", 100), "short");
    }
}
