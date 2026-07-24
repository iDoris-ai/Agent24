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

use agent24_protocol::ToolInfo;
use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

/// Per-call execution context (grows in C4+: session, approval channel).
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub run_id: String,
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
}

/// One callable tool. `parameters` is the JSON Schema advertised to the model;
/// `call` returns the string handed back as the tool result message.
#[async_trait]
pub trait Tool: Send + Sync {
    fn info(&self) -> ToolInfo;

    /// JSON Schema for the input object
    fn parameters(&self) -> Value;

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
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
            allowed: BTreeSet::new(),
        }
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

    /// Sorted list for `GET /api/v1/tools`.
    pub fn list(&self) -> Vec<ToolInfo> {
        self.tools.values().map(|t| t.info()).collect()
    }

    /// Tools advertised to the model: whitelisted AND auto-executable. Tools
    /// stuck behind the C4 approval stub are NOT advertised — offering a tool
    /// that dispatch always denies just burns model iterations.
    pub fn adverts(&self) -> Vec<ToolAdvert> {
        self.tools
            .values()
            .filter(|t| {
                let info = t.info();
                self.allowed.contains(&info.name) && !info.requires_approval
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

        // 3. approval gate — C4 stub: fail-closed auto-deny
        if tool.info().requires_approval {
            return Err(ToolError::Denied(format!(
                "tool {name} requires approval; approvals land in C4 — auto-denied (fail-closed)"
            )));
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
            ToolInfo {
                name: "slow".to_owned(),
                source: "builtin".to_owned(),
                description: "sleeps".to_owned(),
                requires_approval: false,
            }
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
                ToolInfo {
                    name: "hang".to_owned(),
                    source: "builtin".to_owned(),
                    description: String::new(),
                    requires_approval: false,
                }
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
