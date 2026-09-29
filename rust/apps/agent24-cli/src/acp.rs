//! Minimal ACP-over-stdio bridge for the Open Design prototype.
//!
//! stdout is reserved for newline-delimited JSON-RPC. Agent24 execution stays
//! in `agent24d`: this module only maps ACP sessions/prompts onto the existing
//! REST + WebSocket run protocol.

use std::collections::HashSet;

use agent24_protocol::{Event, EventBody, Run, RunCreate, RunMode, Session, SessionCreate};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_tungstenite::tungstenite;

use super::{Endpoint, bearer, client};

const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const AGENT_ERROR: i64 = -32000;

#[derive(Default)]
struct BridgeState {
    sessions: HashSet<String>,
}

pub(super) async fn serve(ep: &Endpoint) -> Result<(), String> {
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();
    let mut state = BridgeState::default();

    while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => {
                write_frame(
                    &mut stdout,
                    &rpc_error(Value::Null, INVALID_REQUEST, "invalid JSON-RPC request"),
                )
                .await?;
                continue;
            }
        };
        let Some(id) = request.get("id").cloned() else {
            // The prototype does not consume client notifications yet.
            continue;
        };
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            write_frame(
                &mut stdout,
                &rpc_error(id, INVALID_REQUEST, "missing method"),
            )
            .await?;
            continue;
        };
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

        match method {
            "initialize" => {
                write_frame(
                    &mut stdout,
                    &rpc_result(
                        id,
                        json!({
                            "protocolVersion": 1,
                            "agentInfo": { "name": "Agent24", "version": env!("CARGO_PKG_VERSION") }
                        }),
                    ),
                )
                .await?;
            }
            "session/new" => match create_session(ep, &params).await {
                Ok(session) => {
                    state.sessions.insert(session.id.clone());
                    write_frame(
                        &mut stdout,
                        &rpc_result(id, json!({ "sessionId": session.id })),
                    )
                    .await?;
                }
                Err(err) => write_frame(&mut stdout, &rpc_error(id, AGENT_ERROR, &err)).await?,
            },
            "session/prompt" => {
                let Some(session_id) = params.get("sessionId").and_then(Value::as_str) else {
                    write_frame(
                        &mut stdout,
                        &rpc_error(id, INVALID_PARAMS, "missing sessionId"),
                    )
                    .await?;
                    continue;
                };
                if !state.sessions.contains(session_id) {
                    write_frame(
                        &mut stdout,
                        &rpc_error(id, INVALID_PARAMS, "unknown sessionId"),
                    )
                    .await?;
                    continue;
                }
                let Some(prompt) = prompt_text(&params) else {
                    write_frame(
                        &mut stdout,
                        &rpc_error(id, INVALID_PARAMS, "prompt contains no text"),
                    )
                    .await?;
                    continue;
                };
                match run_prompt(ep, session_id, prompt, &mut stdout).await {
                    Ok(result) => write_frame(&mut stdout, &rpc_result(id, result)).await?,
                    Err(err) => write_frame(&mut stdout, &rpc_error(id, AGENT_ERROR, &err)).await?,
                }
            }
            _ => {
                write_frame(
                    &mut stdout,
                    &rpc_error(id, METHOD_NOT_FOUND, "method not found"),
                )
                .await?
            }
        }
    }
    Ok(())
}

async fn create_session(ep: &Endpoint, params: &Value) -> Result<Session, String> {
    let title = params
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Open Design")
        .to_owned();
    let create = SessionCreate {
        title,
        channel: "creative".to_owned(),
    };
    let response = bearer(
        ep,
        client()
            .post(format!("{}/api/v1/sessions", ep.base))
            .json(&create),
    )
    .send()
    .await
    .map_err(|e| format!("creating session: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "creating session: daemon returned {}",
            response.status()
        ));
    }
    response
        .json()
        .await
        .map_err(|e| format!("reading session: {e}"))
}

async fn run_prompt(
    ep: &Endpoint,
    session_id: &str,
    prompt: String,
    stdout: &mut tokio::io::Stdout,
) -> Result<Value, String> {
    // Subscribe before creating the run so a fast local model cannot complete
    // between POST /runs and the WebSocket handshake.
    let ws_url = format!("{}/api/v1/events", ep.base.replacen("http", "ws", 1));
    let request = ws_request(&ws_url, &ep.token)?;
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("connecting event stream: {e}"))?;

    let create = RunCreate {
        session_id: Some(session_id.to_owned()),
        prompt,
        model_override: None,
        mode: RunMode::Normal,
    };
    let response = bearer(
        ep,
        client()
            .post(format!("{}/api/v1/runs", ep.base))
            .json(&create),
    )
    .send()
    .await
    .map_err(|e| format!("starting run: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "starting run: daemon returned {}",
            response.status()
        ));
    }
    let run: Run = response
        .json()
        .await
        .map_err(|e| format!("reading run: {e}"))?;

    while let Some(frame) = socket.next().await {
        let frame = frame.map_err(|e| format!("reading event stream: {e}"))?;
        let tungstenite::Message::Text(text) = frame else {
            continue;
        };
        let Ok(event) = serde_json::from_str::<Event>(&text) else {
            continue;
        };
        match event.body {
            EventBody::ModelDelta(delta) if delta.run_id == run.id => {
                write_frame(
                    stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": { "text": delta.text }
                            }
                        }
                    }),
                )
                .await?;
            }
            EventBody::RunCompleted(done) if done.run_id == run.id => {
                return Ok(json!({
                    "stopReason": "end_turn",
                    "usage": {
                        "inputTokens": done.usage.prompt_tokens,
                        "outputTokens": done.usage.completion_tokens
                    }
                }));
            }
            EventBody::RunFailed(failed) if failed.run_id == run.id => {
                return Err(format!("run failed: {}", failed.error.message));
            }
            EventBody::RunCancelled(cancelled) if cancelled.run_id == run.id => {
                return Err("run cancelled".to_owned());
            }
            _ => {}
        }
    }
    Err("event stream closed before run completed".to_owned())
}

fn prompt_text(params: &Value) -> Option<String> {
    let prompt = params.get("prompt")?.as_array()?;
    let parts: Vec<&str> = prompt
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn ws_request(url: &str, token: &str) -> Result<tungstenite::handshake::client::Request, String> {
    use tungstenite::client::IntoClientRequest;
    let mut request = url.into_client_request().map_err(|e| e.to_string())?;
    if !token.is_empty() {
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}")
                .parse()
                .map_err(|_| "invalid bearer token".to_owned())?,
        );
    }
    Ok(request)
}

async fn write_frame(stdout: &mut tokio::io::Stdout, value: &Value) -> Result<(), String> {
    let mut encoded = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    encoded.push(b'\n');
    stdout
        .write_all(&encoded)
        .await
        .map_err(|e| e.to_string())?;
    stdout.flush().await.map_err(|e| e.to_string())
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_text_prompt_parts_in_order() {
        let params = json!({
            "prompt": [
                { "type": "text", "text": "first" },
                { "type": "resource_link", "uri": "/tmp/a.png" },
                { "type": "text", "text": "second" }
            ]
        });
        assert_eq!(prompt_text(&params).as_deref(), Some("first\nsecond"));
    }

    #[test]
    fn rpc_envelopes_preserve_client_ids() {
        assert_eq!(
            rpc_result(json!("req-7"), json!({ "protocolVersion": 1 })),
            json!({ "jsonrpc": "2.0", "id": "req-7", "result": { "protocolVersion": 1 } })
        );
        assert_eq!(
            rpc_error(json!(9), INVALID_PARAMS, "bad"),
            json!({ "jsonrpc": "2.0", "id": 9, "error": { "code": -32602, "message": "bad" } })
        );
    }
}
