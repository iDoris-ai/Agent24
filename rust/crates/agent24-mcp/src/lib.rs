//! Agent24 MCP adapter (M-E / E1).
//!
//! Connects an external MCP server and exposes its tools as ordinary
//! [`agent24_tools::Tool`]s, so they enter the SAME dispatch pipeline as the
//! built-ins.
//!
//! That bridging choice is a security decision, not a convenience one: tools
//! registered in the `ToolRegistry` inherit the C4 approval gate and the D3
//! Guardian automatically. Reaching an MCP server through a side channel would
//! silently give a third-party server's tools a path that bypasses approval —
//! exactly what must not happen for code we did not write.
//!
//! The `rmcp` SDK is confined to this crate (ADR-026's own mitigation for
//! third-party SDK churn: adapter layer only, kernel depends on nothing here).
//! Pinned to the stable 2.x line rather than the 3.0 beta.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use agent24_protocol::{RiskClass, ToolInfo};
use agent24_tools::{Tool, ToolContext, ToolError};
use async_trait::async_trait;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("spawning MCP server {0}: {1}")]
    Spawn(String, String),
    #[error("MCP handshake with {0} failed: {1}")]
    Handshake(String, String),
    #[error("MCP call failed: {0}")]
    Call(String),
    #[error("cancelled")]
    Cancelled,
}

/// How to launch a stdio MCP server.
#[derive(Debug, Clone)]
pub struct McpServerSpec {
    /// Short id used to namespace the tool names it contributes.
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

impl McpServerSpec {
    pub fn new(name: impl Into<String>, command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args,
        }
    }
}

/// A connected MCP server. Dropping it tears down the child process.
pub struct McpServer {
    name: String,
    service: RunningService<RoleClient, ()>,
}

impl McpServer {
    /// Spawn the server and complete the MCP initialize handshake.
    ///
    /// `cancel` is handed to rmcp's own `serve_with_ct`, so a daemon shutdown
    /// tears the session (and the child) down rather than leaking it — the
    /// kernel threads cancellation everywhere and this must not be the gap.
    pub async fn connect(
        spec: &McpServerSpec,
        cancel: CancellationToken,
    ) -> Result<Self, McpError> {
        let mut cmd = tokio::process::Command::new(&spec.command);
        cmd.args(&spec.args);
        let transport = TokioChildProcess::new(cmd)
            .map_err(|e| McpError::Spawn(spec.name.clone(), e.to_string()))?;
        let service = ()
            .serve_with_ct(transport, cancel)
            .await
            .map_err(|e| McpError::Handshake(spec.name.clone(), e.to_string()))?;
        Ok(Self {
            name: spec.name.clone(),
            service,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The server's advertised tools.
    pub async fn list_tools(&self) -> Result<Vec<rmcp::model::Tool>, McpError> {
        self.service
            .list_all_tools()
            .await
            .map_err(|e| McpError::Call(e.to_string()))
    }

    async fn call(
        &self,
        tool: &str,
        args: Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<CallToolResult, McpError> {
        // #[non_exhaustive] upstream: build via Default and assign.
        let mut params = CallToolRequestParams::default();
        params.name = Cow::Owned(tool.to_owned());
        params.arguments = Some(args);
        tokio::select! {
            r = self.service.call_tool(params) => r.map_err(|e| McpError::Call(e.to_string())),
            () = cancel.cancelled() => Err(McpError::Cancelled),
        }
    }
}

/// Namespaced tool name: `mcp_{server}_{tool}`.
///
/// Namespacing is required, not cosmetic — two servers may each expose a
/// `search`, and an unqualified collision would let one server silently shadow
/// another's tool (or a built-in).
pub fn qualified_name(server: &str, tool: &str) -> String {
    format!("mcp_{}_{}", sanitize(server), sanitize(tool))
}

/// Sanitize one half of a qualified name.
///
/// `_` is the SEPARATOR in `mcp_{server}_{tool}`, so it must not survive inside
/// either half — otherwise the split is ambiguous and two ordinary configs
/// collide: server `web` + tool `search_x` and server `web_search` + tool `x`
/// both yielded `mcp_web_search_x`, and `ToolRegistry::with()` is a plain insert,
/// so one silently shadowed the other. Mapping `_` (and anything else outside
/// `[A-Za-z0-9-]`) to `-` leaves only `-` inside the halves, which makes the `_`
/// positions unambiguous.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Flatten an MCP tool result into the string the agent loop feeds back.
pub fn render_result(result: &CallToolResult) -> String {
    let mut out = String::new();
    for block in &result.content {
        if let ContentBlock::Text(t) = block {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&t.text);
        }
    }
    if out.is_empty()
        && let Some(structured) = &result.structured_content
    {
        out = structured.to_string();
    }
    out
}

/// One MCP tool, adapted onto the kernel's [`Tool`] contract.
pub struct McpTool {
    server: Arc<McpServer>,
    /// The name on the wire to the MCP server (unqualified).
    remote_name: String,
    /// The namespaced name the model sees.
    name: String,
    description: String,
    parameters: Value,
}

impl McpTool {
    pub fn new(server: Arc<McpServer>, tool: &rmcp::model::Tool) -> Self {
        let remote_name = tool.name.to_string();
        Self {
            name: qualified_name(server.name(), &remote_name),
            description: tool
                .description
                .as_ref()
                .map(|d| d.to_string())
                .unwrap_or_default(),
            parameters: Value::Object((*tool.input_schema).clone()),
            remote_name,
            server,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn info(&self) -> ToolInfo {
        ToolInfo::new(
            self.name.clone(),
            // ToolInfo.source is an open enum (builtin | mcp | module) — mark
            // these as mcp so a UI/audit can tell third-party tools apart.
            "mcp",
            self.description.clone(),
            // Third-party code we did not write, whose side effects we cannot
            // bound: classified External so it goes through the human approval
            // path and never auto-dispatches. H2 will let the USER relax an
            // individual server's read-only tools to `Read`; nothing a server
            // or a module ships may relax itself.
            RiskClass::External,
        )
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(60)
    }

    async fn call(
        &self,
        _ctx: &ToolContext,
        input: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        match self
            .server
            .call(&self.remote_name, input.clone(), cancel)
            .await
        {
            Ok(result) => {
                if result.is_error.unwrap_or(false) {
                    return Err(ToolError::Failed(render_result(&result)));
                }
                Ok(render_result(&result))
            }
            Err(McpError::Cancelled) => Err(ToolError::Failed("cancelled".to_owned())),
            Err(err) => Err(ToolError::Failed(err.to_string())),
        }
    }
}

/// Connect a server and build its tools, ready to register.
pub async fn connect_and_build_tools(
    spec: &McpServerSpec,
    cancel: CancellationToken,
) -> Result<(Arc<McpServer>, Vec<Arc<dyn Tool>>), McpError> {
    let server = Arc::new(McpServer::connect(spec, cancel).await?);
    let listed = server.list_tools().await?;
    let tools: Vec<Arc<dyn Tool>> = listed
        .iter()
        .map(|t| Arc::new(McpTool::new(Arc::clone(&server), t)) as Arc<dyn Tool>)
        .collect();
    tracing::info!(
        "MCP server {} contributed {} tool(s)",
        spec.name,
        tools.len()
    );
    Ok((server, tools))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn names_are_namespaced_so_servers_cannot_shadow_each_other() {
        assert_eq!(qualified_name("files", "search"), "mcp_files_search");
        assert_ne!(
            qualified_name("files", "search"),
            qualified_name("web", "search")
        );
    }

    #[test]
    fn ordinary_configs_do_not_collide() {
        // Reviewer-found, reproduced before fixing: two entirely conventional
        // server names silently produced the SAME qualified name, and the
        // registry's insert meant one permanently shadowed the other.
        assert_ne!(
            qualified_name("web", "search_x"),
            qualified_name("web_search", "x"),
        );
        // The separator is unambiguous: exactly two `_` after the `mcp` prefix.
        let q = qualified_name("web_search", "x");
        assert_eq!(q.matches('_').count(), 2, "{q}");
    }

    #[test]
    fn hostile_names_are_sanitized() {
        // A server must not be able to inject separators, whitespace or
        // path-ish characters into the function name the model sees.
        assert_eq!(qualified_name("a b", "x/y"), "mcp_a-b_x-y");
        let q = qualified_name("a.b", "c:d");
        assert!(!q.contains('.'), "{q}");
        assert!(!q.contains(':'), "{q}");
        assert!(!qualified_name("../etc", "p").contains('/'));
        // Underscores never survive inside a half.
        assert!(!sanitize("a_b").contains('_'));
    }

    #[test]
    fn render_result_joins_text_blocks() {
        let result =
            CallToolResult::success(vec![ContentBlock::text("one"), ContentBlock::text("two")]);
        assert_eq!(render_result(&result), "one\ntwo");
    }

    #[test]
    fn empty_content_falls_back_to_structured_output() {
        let mut result = CallToolResult::success(vec![]);
        result.structured_content = Some(serde_json::json!({"ok": true}));
        assert!(render_result(&result).contains("\"ok\""));
    }
}
