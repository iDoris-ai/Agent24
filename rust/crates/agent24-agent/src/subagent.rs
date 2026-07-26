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
//! - **Read-only** — it runs against a [`ToolRegistry::read_only`] registry that
//!   contains only `Read`-class tools. There is no write/exec tool to deny
//!   because none was registered.
//! - **No recursion** — that same registry does not contain `explore`, so a
//!   sub-run cannot spawn a sub-run. The depth is exactly one, always.

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

    fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools
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
            .ok_or_else(|| ToolError::Invalid("task is required".to_owned()))?;

        let tool_specs = self.tool_specs();
        let mut messages = vec![
            Msg::system(EXPLORER_SYSTEM_PROMPT),
            Msg::user(task.to_owned()),
        ];
        let mut last_text = String::new();

        for _ in 0..MAX_EXPLORE_ITERATIONS {
            if cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let request = CompletionRequest {
                messages: messages.clone(),
                model: None,
                tools: tool_specs.clone(),
            };
            let (_, response) = match self
                .router
                .complete(TaskProfile::default(), &request, cancel)
                .await
            {
                Ok(ok) => ok,
                Err(ModelError::Cancelled) => return Err(ToolError::Cancelled),
                // The explorer is a helper — a model failure inside it becomes a
                // tool failure the caller sees and can react to, not a crash.
                Err(err) => return Err(ToolError::Failed(format!("explorer model call: {err}"))),
            };

            let msg = response.message;
            if msg.tool_calls.is_empty() {
                // Converged: the sub-agent answered.
                return Ok(msg.content.unwrap_or_default());
            }
            if let Some(text) = &msg.content
                && !text.is_empty()
            {
                last_text = text.clone();
            }

            let calls = msg.tool_calls.clone();
            messages.push(msg);
            for call in &calls {
                if cancel.is_cancelled() {
                    return Err(ToolError::Cancelled);
                }
                let result = self.dispatch_read(ctx, call, cancel).await?;
                messages.push(Msg::tool_result(call.id.clone(), result));
            }
        }

        // Budget exhausted. Return the best partial rather than nothing — a
        // caller that gets "here is what I found so far" can decide what to do;
        // one that gets an error just retries blindly.
        Ok(if last_text.is_empty() {
            "(exploration reached its step budget without a conclusion)".to_owned()
        } else {
            format!("(exploration reached its step budget; partial finding)\n{last_text}")
        })
    }
}

impl ExplorerSubagent {
    /// Dispatch one read tool call through the read-only registry. The registry
    /// has no gate installed and only read tools, so this never prompts and can
    /// only read. A tool error is fed back to the sub-model as a result string
    /// (so it can adjust) rather than aborting the whole exploration — except
    /// cancellation, which propagates.
    async fn dispatch_read(
        &self,
        ctx: &ToolContext,
        call: &agent24_models::ToolCallRequest,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        let input = match serde_json::from_str::<Value>(&call.arguments) {
            Ok(Value::Object(map)) => map,
            Ok(_) => return Ok("tool error: arguments must be a JSON object".to_owned()),
            Err(err) if call.arguments.trim().is_empty() => {
                let _ = err;
                Map::new()
            }
            Err(err) => return Ok(format!("tool error: arguments are not valid JSON: {err}")),
        };
        let sub_ctx = ToolContext {
            run_id: ctx.run_id.clone(),
            session_id: None,
            schedule_id: None,
            tool_call_id: format!("{}_explore", ctx.tool_call_id),
        };
        match self
            .tools
            .dispatch(&call.name, &sub_ctx, &input, cancel)
            .await
        {
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

    /// The registry the explorer runs against contains only read tools and not
    /// itself. This is the structural guarantee — assert it directly so a future
    /// edit that adds a write tool or the explorer to `read_only` fails here.
    #[test]
    fn the_read_only_registry_cannot_write_or_recurse() {
        let dir = tempfile::tempdir().unwrap();
        let reg = ToolRegistry::read_only(dir.path().to_path_buf());
        let names: Vec<String> = reg.list().into_iter().map(|t| t.name).collect();
        assert!(names.contains(&"fs_read".to_owned()));
        assert!(names.contains(&"http_fetch".to_owned()));
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
}
