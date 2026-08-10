//! H9: the read-only explorer subagent.
//!
//! `explore` is a tool the main agent calls to answer a bounded, read-only
//! question ("where is X handled?", "what does this config set?") in its OWN
//! context. The main loop gets back a short answer instead of the dozens of
//! file-read round trips it took to reach it — the reads never enter the main
//! transcript, so they don't crowd out the task the user actually asked about.
//!
//! Three properties, and each is enforced by construction rather than by
//! checking:
//!
//! - **Independent context** — the sub-run starts from just its task string and
//!   a fixed system prompt. It shares no history with the caller and returns
//!   only its final text, so nothing it reads leaks upward except the answer.
//! - **Read-only, and network-free** — it runs against a
//!   [`ToolRegistry::read_only`] registry that contains only `fs_read`. There
//!   is no write/exec tool to deny because none was registered, and crucially
//!   no `http_fetch` either: `Read` class means "no side effect on the machine",
//!   not "no egress", and an ungated helper that can both read files and reach
//!   the network is an exfiltration channel.
//! - **No recursion** — that same registry does not contain `explore`, so a
//!   sub-run cannot spawn a sub-run. The depth is exactly one, always.
//! - **Bounded** — iterations, tool calls per turn, and total wall-clock are
//!   all capped, and a panic in the sub-loop is contained as a tool error.

use std::sync::Arc;

use agent24_models::router::{ModelRouter, TaskProfile};
use agent24_models::{CompletionRequest, ModelError, Msg, ToolSpec};
use agent24_protocol::{RiskClass, ToolInfo};
use agent24_tools::{Tool, ToolContext, ToolError, ToolRegistry};
use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

/// The explorer's own iteration budget. Smaller than the main loop's: an
/// exploration that hasn't converged in this many read/model round trips should
/// report what it has rather than burn the caller's time and tokens.
const MAX_EXPLORE_ITERATIONS: usize = 8;

/// Per-turn tool-call cap, mirroring the main loop's `MAX_TOOL_CALLS_PER_TURN`.
/// Without it a single model turn that returns thousands of `fs_read` calls
/// would run them all — one `explore` call could take hours. Calls beyond the
/// cap are answered ("skipped") so the transcript stays valid, not obeyed.
const MAX_EXPLORE_TOOL_CALLS_PER_TURN: usize = 16;

/// Absolute wall-clock ceiling for one `explore` call. The iteration and
/// per-turn caps bound the *number* of operations; this bounds their total
/// *time*, so a run of slow reads or a slow model can't hold the caller open
/// indefinitely.
const EXPLORE_WALL_CLOCK: std::time::Duration = std::time::Duration::from_secs(120);

/// Returned when the explorer produced no text at all — distinct from a real
/// empty answer so the caller can tell "found nothing" from "produced nothing".
const NO_FINDING: &str = "(the explorer produced no answer)";

const EXPLORER_SYSTEM_PROMPT: &str = "\
You are a read-only explorer. Another agent delegated a focused question to you \
because answering it takes many file reads and searches that would clutter its \
context. You can ONLY read — you have no ability to write files, run commands, \
or delegate further. Investigate using the read tools, then answer the question \
directly and concisely. Report what you found, cite the specific files or lines, \
and say plainly if the answer isn't there. Do not propose changes or take action.";

/// The read-only explorer, exposed to the main agent as the `explore` tool.
pub struct ExplorerSubagent {
    router: Arc<ModelRouter>,
    /// A registry containing ONLY read tools and NOT `explore` — this is what
    /// makes the read-only and no-recursion guarantees structural.
    tools: Arc<ToolRegistry>,
}

impl ExplorerSubagent {
    /// `tools` must be a read-only registry (see [`ToolRegistry::read_only`]).
    /// Passing anything broader would silently hand the sub-run write/exec or
    /// re-entrancy, so the daemon builds it with `read_only` and nothing else.
    pub fn new(router: Arc<ModelRouter>, tools: Arc<ToolRegistry>) -> Self {
        Self { router, tools }
    }

    fn specs_of(tools: &ToolRegistry) -> Vec<ToolSpec> {
        tools
            .adverts()
            .into_iter()
            .map(|a| ToolSpec {
                name: a.name,
                description: a.description,
                parameters: a.parameters,
            })
            .collect()
    }
}

#[async_trait]
impl Tool for ExplorerSubagent {
    fn info(&self) -> ToolInfo {
        // Read: spawning an explorer has only read side effects, because the
        // sub-run can only read. So it is not gated — advertised and runs like
        // any other read tool.
        ToolInfo::new(
            "explore",
            "builtin",
            "Delegate a focused, READ-ONLY investigation to a fresh sub-agent \
             with its own context. Use for questions that take many file reads \
             to answer (\"where is X handled?\", \"what does this config set?\") \
             so those reads don't crowd your own context. Input: a single clear \
             question. Returns the sub-agent's answer. It cannot write, run \
             commands, or delegate further.",
            RiskClass::Read,
        )
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The question to investigate, stated clearly and self-contained."
                }
            },
            "required": ["task"],
            "additionalProperties": false
        })
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        input: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        let task = input
            .get("task")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| ToolError::Invalid("task is required".to_owned()))?
            .to_owned();

        // A panic inside the sub-loop (a provider trait object is effectively
        // arbitrary code) must become a ToolError, not unwind past the caller:
        // `run_tool_call` persists this call's row as `running` BEFORE dispatch
        // and only finalizes it on a returned Result, so an escaping panic would
        // leave a dangling `running` tool_call. Run the loop in its OWN task and
        // observe the join result — the same panic-supervision the main run loop
        // uses in `execute()`. Cancellation still reaches the loop through the
        // shared token, and the wall-clock deadline bounds an orphan.
        let router = Arc::clone(&self.router);
        let tools = Arc::clone(&self.tools);
        let sub_ctx = ctx.clone();
        let sub_cancel = cancel.clone();
        let handle = tokio::spawn(async move {
            Self::explore_loop(router, tools, &sub_ctx, task, &sub_cancel).await
        });
        match handle.await {
            Ok(result) => result,
            Err(join_err) if join_err.is_cancelled() => Err(ToolError::Cancelled),
            Err(_) => Err(ToolError::Failed(
                "explorer sub-agent panicked; the exploration was abandoned".to_owned(),
            )),
        }
    }
}

impl ExplorerSubagent {
    /// The bounded read-only loop. Bounded three ways — iterations, tool calls
    /// per turn, and total wall-clock — so no single input shape can make one
    /// `explore` run unboundedly. Takes owned handles so it can run in its own
    /// task (see `call`).
    async fn explore_loop(
        router: Arc<ModelRouter>,
        tools: Arc<ToolRegistry>,
        ctx: &ToolContext,
        task: String,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        let deadline = tokio::time::Instant::now() + EXPLORE_WALL_CLOCK;
        let tool_specs = Self::specs_of(&tools);
        let mut messages = vec![Msg::system(EXPLORER_SYSTEM_PROMPT), Msg::user(task)];
        let mut last_text = String::new();

        for _ in 0..MAX_EXPLORE_ITERATIONS {
            if cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let request = CompletionRequest {
                messages: messages.clone(),
                model: None,
                tools: tool_specs.clone(),
                response_format: None,
            };
            // Privacy note: the explorer uses the SAME (default) profile as a
            // normal run. It is not more privileged than the main agent w.r.t.
            // remote models — the only NEW egress vector a sub-agent could add,
            // arbitrary network fetches, is removed by giving it no `http_fetch`
            // (see ToolRegistry::read_only). So no separate privacy tier is
            // warranted here; hardening the whole daemon to LocalOnly is a
            // config decision, not this tool's to make.
            let complete = router.complete(TaskProfile::default(), &request, cancel);
            let (_, response) = match tokio::time::timeout_at(deadline, complete).await {
                Ok(Ok(ok)) => ok,
                Ok(Err(ModelError::Cancelled)) => return Err(ToolError::Cancelled),
                Ok(Err(err)) => {
                    return Err(ToolError::Failed(format!("explorer model call: {err}")));
                }
                Err(_) => return Ok(budget_reached("time", &last_text)),
            };

            let msg = response.message;
            if msg.tool_calls.is_empty() {
                // Converged: the sub-agent answered. Empty content is reported
                // as NO_FINDING so the caller can distinguish it from a real
                // answer that happened to be short.
                let answer = msg.content.unwrap_or_default();
                return Ok(if answer.trim().is_empty() {
                    NO_FINDING.to_owned()
                } else {
                    answer
                });
            }
            if let Some(text) = &msg.content
                && !text.is_empty()
            {
                last_text = text.clone();
            }

            // Per-turn cap: answer every call so the transcript stays valid, but
            // only execute the first MAX_EXPLORE_TOOL_CALLS_PER_TURN — a runaway
            // fanout is bounded, not obeyed.
            let calls = msg.tool_calls.clone();
            messages.push(msg);
            for (idx, call) in calls.iter().enumerate() {
                if cancel.is_cancelled() {
                    return Err(ToolError::Cancelled);
                }
                if tokio::time::Instant::now() >= deadline {
                    return Ok(budget_reached("time", &last_text));
                }
                let result = if idx >= MAX_EXPLORE_TOOL_CALLS_PER_TURN {
                    format!(
                        "skipped: per-turn tool-call limit ({MAX_EXPLORE_TOOL_CALLS_PER_TURN}) exceeded"
                    )
                } else {
                    Self::dispatch_read(&tools, ctx, call, cancel).await?
                };
                messages.push(Msg::tool_result(call.id.clone(), result));
            }
        }

        Ok(budget_reached("step", &last_text))
    }
}

/// Format a budget-exhaustion result, carrying the best partial finding so a
/// caller gets "here is what I found so far" rather than a bare error it would
/// only retry blindly.
fn budget_reached(kind: &str, last_text: &str) -> String {
    if last_text.trim().is_empty() {
        format!("(exploration reached its {kind} budget without a conclusion)")
    } else {
        format!("(exploration reached its {kind} budget; partial finding)\n{last_text}")
    }
}

impl ExplorerSubagent {
    /// Dispatch one read tool call through the read-only registry. The registry
    /// has no gate installed and only read tools, so this never prompts and can
    /// only read. A tool error is fed back to the sub-model as a result string
    /// (so it can adjust) rather than aborting the whole exploration — except
    /// cancellation, which propagates.
    async fn dispatch_read(
        tools: &ToolRegistry,
        ctx: &ToolContext,
        call: &agent24_models::ToolCallRequest,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        let input = match serde_json::from_str::<Value>(&call.arguments) {
            Ok(Value::Object(map)) => map,
            Ok(_) => return Ok("tool error: arguments must be a JSON object".to_owned()),
            Err(_) if call.arguments.trim().is_empty() => Map::new(),
            Err(err) => return Ok(format!("tool error: arguments are not valid JSON: {err}")),
        };
        let sub_ctx = ToolContext {
            run_id: ctx.run_id.clone(),
            session_id: None,
            schedule_id: None,
            tool_call_id: format!("{}_explore", ctx.tool_call_id),
        };
        match tools.dispatch(&call.name, &sub_ctx, &input, cancel).await {
            Ok(output) => Ok(output),
            Err(ToolError::Cancelled) => Err(ToolError::Cancelled),
            Err(err) => Ok(format!("tool error: {err}")),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_models::router::Tier;
    use agent24_models::{CompletionResponse, ModelProvider, ToolCallRequest};
    use agent24_protocol::Usage;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A provider whose turns are scripted; when the script runs out it answers
    /// with a plain final message so a loop can always terminate.
    struct Scripted {
        turns: StdMutex<Vec<Msg>>,
        calls: AtomicUsize,
    }
    impl Scripted {
        fn new(turns: Vec<Msg>) -> Arc<Self> {
            Arc::new(Self {
                turns: StdMutex::new(turns),
                calls: AtomicUsize::new(0),
            })
        }
    }
    #[async_trait]
    impl ModelProvider for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut turns = self.turns.lock().unwrap();
            let message = if turns.is_empty() {
                Msg::assistant(Some("done".to_owned()), vec![])
            } else {
                turns.remove(0)
            };
            Ok(CompletionResponse {
                message,
                usage: Usage::default(),
            })
        }
        async fn models(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
            Ok(vec![])
        }
    }

    fn call(id: &str, name: &str, args: &str) -> ToolCallRequest {
        ToolCallRequest {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: args.to_owned(),
        }
    }

    fn ctx() -> ToolContext {
        ToolContext {
            run_id: "run_1".to_owned(),
            session_id: Some("sess_1".to_owned()),
            schedule_id: None,
            tool_call_id: "tc_1".to_owned(),
        }
    }

    fn explorer(provider: Arc<Scripted>, workspace: std::path::PathBuf) -> ExplorerSubagent {
        ExplorerSubagent::new(
            Arc::new(ModelRouter::with_defaults(vec![(provider, Tier::Local)])),
            Arc::new(ToolRegistry::read_only(workspace)),
        )
    }

    /// The registry the explorer runs against contains ONLY fs_read — the
    /// structural guarantee. Asserted directly so a future edit that adds a
    /// write, exec, network, or self tool to `read_only` fails here. `http_fetch`
    /// is the security-relevant one: it is Read-class but reaches the network,
    /// which in an ungated helper is an exfiltration channel.
    #[test]
    fn the_read_only_registry_is_fs_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let reg = ToolRegistry::read_only(dir.path().to_path_buf());
        let names: Vec<String> = reg.list().into_iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            vec!["fs_read".to_owned()],
            "explorer tool set drifted"
        );
        assert!(
            !names.contains(&"http_fetch".to_owned()),
            "explorer could reach the network — exfiltration channel"
        );
        assert!(
            !names.contains(&"fs_write".to_owned()),
            "explorer could write"
        );
        assert!(
            !names.contains(&"shell_exec".to_owned()),
            "explorer could exec"
        );
        assert!(
            !names.contains(&"explore".to_owned()),
            "explorer could recurse"
        );
    }

    #[tokio::test]
    async fn a_direct_answer_returns_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Scripted::new(vec![Msg::assistant(
            Some("X is in foo.rs".to_owned()),
            vec![],
        )]);
        let sub = explorer(Arc::clone(&provider), dir.path().to_path_buf());
        let mut input = Map::new();
        input.insert("task".to_owned(), Value::String("where is X?".to_owned()));
        let out = sub
            .call(&ctx(), &input, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out, "X is in foo.rs");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }

    /// A read tool call inside the sub-run is executed and its result fed back,
    /// then the next turn answers. Proves the nested read→model round trip works.
    #[tokio::test]
    async fn it_reads_a_file_then_answers() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("note.txt");
        std::fs::write(&file, "the answer is 42").unwrap();
        let args = format!(r#"{{"path":"{}"}}"#, file.display());
        let provider = Scripted::new(vec![
            Msg::assistant(None, vec![call("c1", "fs_read", &args)]),
            Msg::assistant(Some("the file says 42".to_owned()), vec![]),
        ]);
        let sub = explorer(provider, dir.path().to_path_buf());
        let mut input = Map::new();
        input.insert(
            "task".to_owned(),
            Value::String("what does note.txt say?".to_owned()),
        );
        let out = sub
            .call(&ctx(), &input, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out, "the file says 42");
    }

    /// A write attempt from inside the sub-run fails as an unknown tool (it was
    /// never registered) and comes back as a tool-error string the sub-model can
    /// react to — the exploration is NOT aborted, and nothing was written.
    #[tokio::test]
    async fn a_write_attempt_is_simply_not_available() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("should-not-exist.txt");
        let args = format!(r#"{{"path":"{}","content":"x"}}"#, target.display());
        let provider = Scripted::new(vec![
            Msg::assistant(None, vec![call("c1", "fs_write", &args)]),
            Msg::assistant(Some("I could not write; I am read-only".to_owned()), vec![]),
        ]);
        let sub = explorer(provider, dir.path().to_path_buf());
        let mut input = Map::new();
        input.insert("task".to_owned(), Value::String("write a file".to_owned()));
        let out = sub
            .call(&ctx(), &input, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out, "I could not write; I am read-only");
        assert!(!target.exists(), "the explorer must not have written");
    }

    /// A model that keeps asking for tools forever terminates at the budget with
    /// a partial finding, never runs unbounded.
    #[tokio::test]
    async fn it_stops_at_the_iteration_budget() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("n.txt");
        std::fs::write(&file, "x").unwrap();
        let args = format!(r#"{{"path":"{}"}}"#, file.display());
        // More tool-call turns than the budget → never a final answer.
        let turns: Vec<Msg> = (0..MAX_EXPLORE_ITERATIONS + 3)
            .map(|i| Msg::assistant(Some(format!("step {i}")), vec![call("c", "fs_read", &args)]))
            .collect();
        let provider = Scripted::new(turns);
        let sub = explorer(Arc::clone(&provider), dir.path().to_path_buf());
        let mut input = Map::new();
        input.insert("task".to_owned(), Value::String("loop".to_owned()));
        let out = sub
            .call(&ctx(), &input, &CancellationToken::new())
            .await
            .unwrap();
        assert!(out.contains("step budget"), "{out}");
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            MAX_EXPLORE_ITERATIONS
        );
    }

    #[tokio::test]
    async fn a_missing_task_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let sub = explorer(Scripted::new(vec![]), dir.path().to_path_buf());
        let err = sub
            .call(&ctx(), &Map::new(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)), "{err}");
    }

    fn task_input(t: &str) -> Map<String, Value> {
        let mut input = Map::new();
        input.insert("task".to_owned(), Value::String(t.to_owned()));
        input
    }

    /// A single turn returning thousands of tool calls must not run them all —
    /// only the first per-turn-cap execute; the rest are answered "skipped" so
    /// the transcript stays valid. This is the Critical fanout finding.
    #[tokio::test]
    async fn a_single_turn_fanout_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("n.txt");
        std::fs::write(&file, "x").unwrap();
        let args = format!(r#"{{"path":"{}"}}"#, file.display());
        // One turn with 5000 tool calls, then (if we ever get there) a final.
        let many: Vec<ToolCallRequest> = (0..5000)
            .map(|i| call(&format!("c{i}"), "fs_read", &args))
            .collect();
        let provider = Scripted::new(vec![
            Msg::assistant(None, many),
            Msg::assistant(Some("done".to_owned()), vec![]),
        ]);
        let sub = explorer(provider, dir.path().to_path_buf());
        // Must return promptly (well under the wall clock) rather than grinding
        // through 5000 reads.
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            sub.call(&ctx(), &task_input("fan out"), &CancellationToken::new()),
        )
        .await
        .expect("explore did not honour the per-turn cap")
        .unwrap();
        assert_eq!(out, "done");
    }

    /// A model that panics becomes a ToolError, not an unwind past the caller
    /// that would leave a dangling `running` tool_call row.
    #[tokio::test]
    async fn a_panicking_sub_model_becomes_a_tool_error() {
        struct Panicker;
        #[async_trait]
        impl ModelProvider for Panicker {
            fn name(&self) -> &str {
                "panicker"
            }
            async fn complete(
                &self,
                _req: &CompletionRequest,
                _cancel: &CancellationToken,
            ) -> Result<CompletionResponse, ModelError> {
                panic!("boom inside the sub-model");
            }
            async fn models(
                &self,
                _cancel: &CancellationToken,
            ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
                Ok(vec![])
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let sub = ExplorerSubagent::new(
            Arc::new(ModelRouter::with_defaults(vec![(
                Arc::new(Panicker),
                Tier::Local,
            )])),
            Arc::new(ToolRegistry::read_only(dir.path().to_path_buf())),
        );
        let err = sub
            .call(&ctx(), &task_input("x"), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Failed(_)), "{err}");
        assert!(err.to_string().contains("panicked"), "{err}");
    }

    /// An empty final answer is reported as NO_FINDING, not Ok(""), so the
    /// caller can tell "found nothing" from "produced nothing".
    #[tokio::test]
    async fn an_empty_answer_is_reported_distinctly() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Scripted::new(vec![Msg::assistant(Some("   ".to_owned()), vec![])]);
        let sub = explorer(provider, dir.path().to_path_buf());
        let out = sub
            .call(&ctx(), &task_input("nothing"), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out, NO_FINDING);
    }
}
