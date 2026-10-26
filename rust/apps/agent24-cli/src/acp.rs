//! Minimal ACP-over-stdio bridge for the Open Design prototype.
//!
//! stdout is reserved for newline-delimited JSON-RPC. Agent24 execution stays
//! in `agent24d`: this module only maps ACP sessions/prompts onto the existing
//! REST + WebSocket run protocol.
//!
//! K1-6b.5c-2 (ADR-K1-02 §2.4): the ACP client is outside the kernel, so a
//! run's model deltas and failure message are relayed only once its peer view
//! (`/api/v1/peer/runs/{id}`) says it is not withheld. The view is read at
//! the first delta, failure or completion — sources are tagged before the
//! first model call — and read again after each tool the run starts, since
//! the kernel withholds a run that called a tool (its results are untagged).
//! Every prompt here runs in a session, so while the daemon keeps session
//! memory the kernel withholds all of them.

use std::collections::HashSet;
use std::future::Future;

use agent24_protocol::{
    Event, EventBody, PeerRun, Run, RunCreate, RunMode, Session, SessionCreate,
};
use futures_util::{Stream, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_tungstenite::tungstenite;

use super::{Endpoint, bearer, client};

const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const AGENT_ERROR: i64 = -32000;

/// What the ACP client sees in place of a withheld run's content. Fixed, so
/// nothing about the run can reach the client through it.
const WITHHELD_NOTICE: &str = "this run used sources that are not ordinary, so its content \
     is not shared with connected tools; open it in Agent24 to see it";

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
        workspace_id: None,
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
    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("connecting event stream: {e}"))?;

    let create = RunCreate {
        session_id: Some(session_id.to_owned()),
        prompt,
        workspace_id: None,
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

    let events = socket.filter_map(|frame| async move {
        match frame {
            Err(e) => Some(Err(format!("reading event stream: {e}"))),
            Ok(tungstenite::Message::Text(text)) => {
                serde_json::from_str::<Event>(&text).ok().map(Ok)
            }
            Ok(_) => None,
        }
    });
    relay_run(Box::pin(events), &run.id, session_id, stdout, || {
        peer_run(ep, &run.id)
    })
    .await
}

async fn peer_run(ep: &Endpoint, run_id: &str) -> Result<PeerRun, String> {
    let response = bearer(
        ep,
        client().get(format!("{}/api/v1/peer/runs/{run_id}", ep.base)),
    )
    .send()
    .await
    .map_err(|e| format!("reading peer run: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "reading peer run: daemon returned {}",
            response.status()
        ));
    }
    response
        .json()
        .await
        .map_err(|e| format!("reading peer run: {e}"))
}

/// Whether a run's content may be relayed, decided from its peer view and
/// kept until the run starts a tool. A failed read withholds, like a withheld
/// view.
#[derive(Default)]
struct RelayGate {
    relay: Option<bool>,
}

impl RelayGate {
    /// The run started a tool: what it streams next may rest on the result.
    fn forget(&mut self) {
        self.relay = None;
    }

    async fn relays<F>(&mut self, check: impl FnOnce() -> F) -> bool
    where
        F: Future<Output = Result<PeerRun, String>>,
    {
        if let Some(relay) = self.relay {
            return relay;
        }
        let relay = matches!(check().await, Ok(view) if !view.withheld);
        self.relay = Some(relay);
        relay
    }
}

fn message_chunk(session_id: &str, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "text": text }
            }
        }
    })
}

/// Relay one run's events to the ACP client until it ends.
async fn relay_run<W, F>(
    mut events: impl Stream<Item = Result<Event, String>> + Unpin,
    run_id: &str,
    session_id: &str,
    out: &mut W,
    mut check: impl FnMut() -> F,
) -> Result<Value, String>
where
    W: tokio::io::AsyncWrite + Unpin,
    F: Future<Output = Result<PeerRun, String>>,
{
    let mut gate = RelayGate::default();
    let mut notified = false;
    while let Some(event) = events.next().await {
        match event?.body {
            EventBody::ToolStarted(started) if started.run_id == run_id => gate.forget(),
            EventBody::ModelDelta(delta) if delta.run_id == run_id => {
                if gate.relays(&mut check).await {
                    write_frame(out, &message_chunk(session_id, &delta.text)).await?;
                } else if !notified {
                    notified = true;
                    write_frame(out, &message_chunk(session_id, WITHHELD_NOTICE)).await?;
                }
            }
            EventBody::RunCompleted(done) if done.run_id == run_id => {
                // Token counts measure the run's input and output, so a
                // withheld run ends without them.
                if !gate.relays(&mut check).await {
                    return Ok(json!({ "stopReason": "end_turn" }));
                }
                return Ok(json!({
                    "stopReason": "end_turn",
                    "usage": {
                        "inputTokens": done.usage.prompt_tokens,
                        "outputTokens": done.usage.completion_tokens
                    }
                }));
            }
            EventBody::RunFailed(failed) if failed.run_id == run_id => {
                if gate.relays(&mut check).await {
                    return Err(format!("run failed: {}", failed.error.message));
                }
                return Err(format!("run failed: {WITHHELD_NOTICE}"));
            }
            EventBody::RunCancelled(cancelled) if cancelled.run_id == run_id => {
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

async fn write_frame<W>(stdout: &mut W, value: &Value) -> Result<(), String>
where
    W: tokio::io::AsyncWrite + Unpin,
{
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
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_protocol::{
        ErrorBody, ModelDeltaPayload, RunCompletedPayload, RunFailedPayload, RunOutputPayload,
        RunStatus, Usage,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── K1-6b.5c-2: deltas and failures wait for the peer view ──────────────

    const SECRET: &str = "SECRET-7f3a";

    fn event(body: EventBody) -> Result<Event, String> {
        Ok(Event {
            v: 1,
            seq: 0,
            ts: "2026-10-25T00:00:00Z".to_owned(),
            body,
        })
    }

    fn delta(run_id: &str, text: &str) -> Result<Event, String> {
        event(EventBody::ModelDelta(ModelDeltaPayload {
            run_id: run_id.to_owned(),
            text: text.to_owned(),
        }))
    }

    fn failed(run_id: &str) -> Result<Event, String> {
        event(EventBody::RunFailed(RunFailedPayload {
            run_id: run_id.to_owned(),
            error: ErrorBody {
                code: "internal".to_owned(),
                message: format!("error {SECRET}"),
                details: None,
                hint: None,
            },
        }))
    }

    fn completed(run_id: &str) -> Result<Event, String> {
        event(EventBody::RunCompleted(RunCompletedPayload {
            run_id: run_id.to_owned(),
            output: RunOutputPayload {
                text: format!("output {SECRET}"),
            },
            usage: Usage {
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
                cost_usd: 0.0,
            },
        }))
    }

    fn view(withheld: bool) -> Result<PeerRun, String> {
        Ok(PeerRun {
            id: "run_1".to_owned(),
            status: RunStatus::Running,
            withheld,
            prompt: None,
            output: None,
            error: None,
            notice: withheld.then(|| "withheld".to_owned()),
        })
    }

    /// Relays `events` for run_1 with `answer` as its peer view; returns the
    /// result, what the ACP client was sent, and how often the view was read.
    async fn relay(
        events: Vec<Result<Event, String>>,
        answer: fn() -> Result<PeerRun, String>,
    ) -> (Result<Value, String>, String, usize) {
        let checks = AtomicUsize::new(0);
        let mut out = Vec::new();
        let result = relay_run(
            futures_util::stream::iter(events),
            "run_1",
            "sess_1",
            &mut out,
            || {
                checks.fetch_add(1, Ordering::SeqCst);
                async move { answer() }
            },
        )
        .await;
        (
            result,
            String::from_utf8(out).unwrap_or_default(),
            checks.load(Ordering::SeqCst),
        )
    }

    #[tokio::test]
    async fn an_ordinary_run_streams_its_deltas_after_one_check() {
        let (result, sent, checks) = relay(
            vec![delta("run_1", "a"), delta("run_1", "b"), completed("run_1")],
            || view(false),
        )
        .await;
        let result = result.unwrap();
        assert_eq!(result["stopReason"], "end_turn");
        assert_eq!(result["usage"]["outputTokens"], 2);
        let chunks: Vec<Value> = sent
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or_default())
            .collect();
        assert_eq!(
            chunks,
            [message_chunk("sess_1", "a"), message_chunk("sess_1", "b")]
        );
        assert_eq!(checks, 1);
    }

    #[tokio::test]
    async fn a_withheld_or_unreadable_run_sends_only_the_notice() {
        let answers: [fn() -> Result<PeerRun, String>; 2] =
            [|| view(true), || Err("daemon unreachable".to_owned())];
        for answer in answers {
            let (result, sent, checks) = relay(
                vec![
                    delta("run_1", &format!("a {SECRET}")),
                    delta("run_1", &format!("b {SECRET}")),
                    failed("run_1"),
                ],
                answer,
            )
            .await;
            assert_eq!(
                result.unwrap_err(),
                format!("run failed: {WITHHELD_NOTICE}")
            );
            assert_eq!(
                sent.lines().collect::<Vec<_>>(),
                [message_chunk("sess_1", WITHHELD_NOTICE).to_string()]
            );
            assert!(!sent.contains(SECRET), "{sent}");
            assert_eq!(checks, 1);
        }
    }

    #[tokio::test]
    async fn a_failure_before_any_delta_checks_the_peer_view_first() {
        let (result, _, checks) = relay(vec![failed("run_1")], || view(false)).await;
        assert_eq!(result.unwrap_err(), format!("run failed: error {SECRET}"));
        assert_eq!(checks, 1);

        let (result, sent, checks) = relay(vec![failed("run_1")], || view(true)).await;
        let err = result.unwrap_err();
        assert_eq!(err, format!("run failed: {WITHHELD_NOTICE}"));
        assert!(sent.is_empty(), "{sent}");
        assert_eq!(checks, 1);
    }

    fn tool_started(run_id: &str) -> Result<Event, String> {
        event(EventBody::ToolStarted(
            agent24_protocol::ToolStartedPayload {
                run_id: run_id.to_owned(),
                tool_call_id: "tc_1".to_owned(),
                tool: "fs_read".to_owned(),
                input_summary: String::new(),
            },
        ))
    }

    #[tokio::test]
    async fn deltas_after_a_tool_starts_wait_for_a_fresh_check() {
        // The kernel withholds a run once it has called a tool: the second
        // check answers withheld.
        let checks = AtomicUsize::new(0);
        let mut out = Vec::new();
        let result = relay_run(
            futures_util::stream::iter(vec![
                delta("run_1", "before"),
                tool_started("run_1"),
                delta("run_1", &format!("after {SECRET}")),
                delta("run_1", &format!("more {SECRET}")),
                completed("run_1"),
            ]),
            "run_1",
            "sess_1",
            &mut out,
            || {
                let n = checks.fetch_add(1, Ordering::SeqCst);
                async move { view(n > 0) }
            },
        )
        .await;
        let sent = String::from_utf8(out).unwrap_or_default();
        assert_eq!(
            sent.lines().collect::<Vec<_>>(),
            [
                message_chunk("sess_1", "before").to_string(),
                message_chunk("sess_1", WITHHELD_NOTICE).to_string(),
            ]
        );
        assert!(!sent.contains(SECRET), "{sent}");
        assert_eq!(result.unwrap(), json!({ "stopReason": "end_turn" }));
        assert_eq!(checks.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn another_runs_tool_does_not_trigger_a_fresh_check() {
        let (result, sent, checks) = relay(
            vec![
                delta("run_1", "a"),
                tool_started("run_2"),
                delta("run_1", "b"),
                completed("run_1"),
            ],
            || view(false),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(sent.lines().count(), 2, "{sent}");
        assert_eq!(checks, 1);
    }

    #[tokio::test]
    async fn a_withheld_run_completes_without_its_token_counts() {
        let (result, sent, checks) = relay(vec![completed("run_1")], || view(true)).await;
        assert_eq!(result.unwrap(), json!({ "stopReason": "end_turn" }));
        assert!(sent.is_empty(), "{sent}");
        assert_eq!(checks, 1);
    }

    #[tokio::test]
    async fn another_runs_events_are_ignored_without_a_check() {
        let (result, sent, checks) = relay(
            vec![
                delta("run_2", SECRET),
                failed("run_2"),
                event(EventBody::RunCancelled(
                    agent24_protocol::RunCancelledPayload {
                        run_id: "run_1".to_owned(),
                    },
                )),
            ],
            || view(true),
        )
        .await;
        assert_eq!(result.unwrap_err(), "run cancelled");
        assert!(sent.is_empty(), "{sent}");
        assert_eq!(checks, 0);
    }

    #[tokio::test]
    async fn another_runs_completion_or_cancellation_does_not_end_this_run() {
        let mut other_completed = completed("run_2");
        if let Ok(Event {
            body: EventBody::RunCompleted(done),
            ..
        }) = &mut other_completed
        {
            done.usage.completion_tokens = 99;
        }
        let (result, _, checks) = relay(
            vec![
                other_completed,
                event(EventBody::RunCancelled(
                    agent24_protocol::RunCancelledPayload {
                        run_id: "run_2".to_owned(),
                    },
                )),
                completed("run_1"),
            ],
            || view(false),
        )
        .await;
        assert_eq!(result.unwrap()["usage"]["outputTokens"], 2);
        assert_eq!(checks, 1);
    }

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
