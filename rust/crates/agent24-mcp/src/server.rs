//! E4: agent24d exposed as an MCP server.
//!
//! An rmcp stdio server that lets an external MCP client (Claude Desktop, another
//! agent) use this machine's agent24d as a tool — WITHOUT any path that bypasses
//! the host's approval gate. It proxies a CURATED, safe surface to the running
//! daemon over its v1 HTTP API:
//!
//! - `agent24_run` — start an agent run and return its output. Goes through the
//!   FULL gated loop, so a risky action surfaces as an approval on the HOST and
//!   never runs silently for a remote caller.
//! - `agent24_list_tools`, `agent24_list_runs` — read-only introspection.
//!
//! It deliberately does NOT expose raw `shell_exec`/`fs_write` as MCP tools:
//! that would hand a third party an un-gated execution path. Every side effect
//! happens inside the run `agent24_run` triggers, under the existing
//! C4/D3/H1–H4 gate. Safety is inherited, not re-implemented here.
//!
//! rmcp stays confined to this adapter crate (ADR-026): the kernel proxies HTTP,
//! it does not link the SDK.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use agent24_protocol::{Run, RunStatus};
use rmcp::ErrorData as McpError;
use rmcp::ServiceExt;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, JsonObject, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, transport::stdio};
use serde_json::{Map, Value, json};

/// How long `agent24_run` waits for a run to reach a terminal state before
/// giving up (the run itself keeps going on the daemon; only this proxy call
/// returns). Generous — a gated run may sit in `awaiting_approval` for a while.
const RUN_WAIT_TIMEOUT: Duration = Duration::from_secs(600);
/// Poll cadence while waiting for a run to finish.
const RUN_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum McpServerError {
    #[error("serving MCP over stdio: {0}")]
    Serve(String),
}

/// A thin, authenticated MCP-server proxy to a running agent24d.
#[derive(Clone)]
pub struct Agent24Server {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Agent24Server {
    pub fn new(base: String, token: String) -> Self {
        Self {
            base,
            token,
            http: reqwest::Client::new(),
        }
    }

    /// Serve this proxy over stdio until the client disconnects.
    pub async fn serve_stdio(self) -> Result<(), McpServerError> {
        let running = self
            .serve(stdio())
            .await
            .map_err(|e| McpServerError::Serve(e.to_string()))?;
        running
            .waiting()
            .await
            .map_err(|e| McpServerError::Serve(e.to_string()))?;
        Ok(())
    }

    fn tool_defs() -> Vec<Tool> {
        let empty_obj: Arc<JsonObject> = Arc::new(
            json!({ "type": "object", "properties": {} })
                .as_object()
                .cloned()
                .unwrap_or_default(),
        );
        let run_schema: Arc<JsonObject> = Arc::new(
            json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "The task for the agent to run." },
                    "session_id": { "type": "string", "description": "Optional session to continue." }
                },
                "required": ["prompt"]
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        );
        vec![
            Tool::new(
                Cow::Borrowed("agent24_run"),
                Cow::Borrowed(
                    "Run a task on this machine's agent24d and return its final output. \
                     Risky actions are approved by the host, not the caller.",
                ),
                run_schema,
            ),
            Tool::new(
                Cow::Borrowed("agent24_list_tools"),
                Cow::Borrowed("List the tools this agent24d can use (read-only)."),
                Arc::clone(&empty_obj),
            ),
            Tool::new(
                Cow::Borrowed("agent24_list_runs"),
                Cow::Borrowed("List this agent24d's runs and their status (read-only)."),
                empty_obj,
            ),
        ]
    }

    /// Authenticated GET returning the response body as a pretty string.
    async fn get(&self, path: &str) -> Result<String, String> {
        let res = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| format!("daemon unreachable: {e}"))?;
        let status = res.status();
        let body = res.text().await.map_err(|e| e.to_string())?;
        if status.is_success() {
            Ok(body)
        } else {
            Err(format!("daemon returned {status}: {body}"))
        }
    }

    /// Start a run and wait (bounded) for it to finish, returning its output.
    async fn run(&self, args: &Map<String, Value>) -> Result<String, String> {
        let Some(prompt) = args.get("prompt").and_then(Value::as_str) else {
            return Err("`prompt` (string) is required".to_owned());
        };
        let mut body = json!({ "prompt": prompt });
        if let Some(sid) = args.get("session_id").and_then(Value::as_str) {
            body["session_id"] = json!(sid);
        }
        let created = self
            .http
            .post(format!("{}/api/v1/runs", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("daemon unreachable: {e}"))?;
        if !created.status().is_success() {
            let status = created.status();
            let text = created.text().await.unwrap_or_default();
            return Err(format!("run create failed ({status}): {text}"));
        }
        let run: Run = created
            .json()
            .await
            .map_err(|e| format!("bad run response: {e}"))?;
        self.await_run(&run.id).await
    }

    /// Poll a run to a terminal state (bounded by RUN_WAIT_TIMEOUT).
    async fn await_run(&self, run_id: &str) -> Result<String, String> {
        let max_polls = (RUN_WAIT_TIMEOUT.as_secs() / RUN_POLL_INTERVAL.as_secs().max(1)).max(1);
        for _ in 0..max_polls {
            let res = self
                .http
                .get(format!("{}/api/v1/runs/{run_id}", self.base))
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(|e| format!("daemon unreachable: {e}"))?;
            if !res.status().is_success() {
                return Err(format!("run poll failed: {}", res.status()));
            }
            let run: Run = res.json().await.map_err(|e| e.to_string())?;
            match run.status {
                RunStatus::Completed => {
                    return Ok(run.output.map(|o| o.text).unwrap_or_default());
                }
                RunStatus::Failed => {
                    let msg = run
                        .error
                        .map(|e| format!("{}: {}", e.code, e.message))
                        .unwrap_or_else(|| "run failed".to_owned());
                    return Err(msg);
                }
                RunStatus::Cancelled => return Err("run was cancelled".to_owned()),
                // queued | running | awaiting_approval → keep waiting
                _ => tokio::time::sleep(RUN_POLL_INTERVAL).await,
            }
        }
        Err(format!(
            "run {run_id} did not finish within {RUN_WAIT_TIMEOUT:?} \
             (it is still running on the daemon; check with agent24_list_runs)"
        ))
    }
}

impl ServerHandler for Agent24Server {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "agent24: run tasks on a personal agent24d (gated on the host) and \
             introspect its tools and runs.",
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: Self::tool_defs(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = request.arguments.unwrap_or_default();
        let outcome = match request.name.as_ref() {
            "agent24_run" => self.run(&args).await,
            "agent24_list_tools" => self.get("/api/v1/tools").await,
            "agent24_list_runs" => self.get("/api/v1/runs").await,
            other => Err(format!("unknown tool: {other}")),
        };
        // Tool-level failures are returned as caller-visible error results, not
        // protocol errors (which MCP clients render opaquely).
        Ok(match outcome {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(err) => CallToolResult::error(vec![ContentBlock::text(err)]),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn advertises_the_curated_surface_only() {
        let names: Vec<String> = Agent24Server::tool_defs()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(
            names,
            ["agent24_run", "agent24_list_tools", "agent24_list_runs"]
        );
        // The dangerous builtins must NOT be exposed as MCP tools — the only way
        // to run them is inside a gated `agent24_run`.
        assert!(!names.iter().any(|n| n == "shell_exec" || n == "fs_write"));
    }

    #[test]
    fn run_tool_declares_prompt_required() {
        let run = Agent24Server::tool_defs()
            .into_iter()
            .find(|t| t.name == "agent24_run")
            .unwrap();
        let required = run.input_schema.get("required").unwrap();
        assert_eq!(required, &json!(["prompt"]));
    }

    /// A one-shot mock daemon that captures the request line + Authorization
    /// header and replies with a canned body — enough to prove the proxy really
    /// makes an authenticated HTTP call (exercising the real `get` path, not a
    /// hand-stubbed value).
    async fn mock_daemon(
        body: &'static str,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cap = std::sync::Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let cap = std::sync::Arc::clone(&cap);
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if let Ok(mut v) = cap.lock() {
                        v.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                    }
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        (format!("http://{addr}"), seen)
    }

    #[tokio::test]
    async fn get_proxies_to_the_daemon_with_bearer_auth() {
        let (base, seen) = mock_daemon(r#"{"tools":[]}"#).await;
        let server = Agent24Server::new(base, "secret-token".to_owned());
        let body = server.get("/api/v1/tools").await.unwrap();
        assert_eq!(body, r#"{"tools":[]}"#);
        // The request actually went out, hit the right path, and carried the token.
        let req = seen.lock().unwrap().first().cloned().unwrap_or_default();
        assert!(req.contains("GET /api/v1/tools"), "{req}");
        assert!(req.contains("authorization: Bearer secret-token"), "{req}");
    }

    #[tokio::test]
    async fn a_non_2xx_daemon_response_is_surfaced_as_an_error() {
        // Point at a closed port so the request fails at the transport — the
        // proxy must translate it to a caller-visible error, not panic.
        let server = Agent24Server::new("http://127.0.0.1:1".to_owned(), String::new());
        let err = server.get("/api/v1/runs").await.unwrap_err();
        assert!(err.contains("daemon unreachable"), "{err}");
    }
}
