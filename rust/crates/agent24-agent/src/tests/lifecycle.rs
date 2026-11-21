pub(crate) fn create() -> RunCreate {
    RunCreate {
        knowledge_query: None,
        idoris_headroom_confirmed: false,
        session_id: None,
        workspace_id: None,
        prompt: "hi".to_owned(),
        model_override: None,
        mode: agent24_protocol::RunMode::Normal,
        knowledge_binding_id: None,
        skill_id: None,
    }
}

pub(crate) async fn wait_terminal(store: &Store, id: &str) -> Run {
    for _ in 0..100 {
        let run = store.get_run(id).await.unwrap().unwrap();
        if agent24_core::run_is_terminal(run.status) {
            return run;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("run {id} never reached a terminal state");
}

#[tokio::test]
async fn run_completes_with_full_event_sequence() {
    let (manager, sink, store) = manager_with(Arc::new(FixedProvider)).await;
    let run = manager.start_run(create()).await.unwrap();
    assert_eq!(run.status, RunStatus::Queued);
    let done = wait_terminal(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);
    assert_eq!(done.output.unwrap().text, "pong");
    assert_eq!(done.usage.total_tokens, 2);
    assert!(done.started_at.is_some() && done.ended_at.is_some());
    let events = sink.0.lock().unwrap().clone();
    assert_eq!(events, vec!["run.started", "model.delta", "run.completed"]);
}

#[tokio::test]
async fn cancelling_a_hanging_run_lands_cancelled_within_a_second() {
    let (manager, sink, store) = manager_with(Arc::new(HangingProvider)).await;
    let run = manager.start_run(create()).await.unwrap();
    // Let it reach running
    tokio::time::sleep(Duration::from_millis(50)).await;
    let started = std::time::Instant::now();
    manager.cancel_run(&run.id).await.unwrap();
    let done = wait_terminal(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "cancel was not prompt"
    );
    let events = sink.0.lock().unwrap().clone();
    assert_eq!(events, vec!["run.started", "run.cancelled"]);
}

#[tokio::test]
async fn cancel_is_idempotent_on_terminal_runs() {
    let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
    let run = manager.start_run(create()).await.unwrap();
    let done = wait_terminal(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);
    // cancel after completion: unchanged, no error
    let after = manager.cancel_run(&run.id).await.unwrap();
    assert_eq!(after.status, RunStatus::Completed);
}

#[tokio::test]
async fn unknown_session_is_rejected() {
    let (manager, _sink, _store) = manager_with(Arc::new(FixedProvider)).await;
    let err = manager
        .start_run(RunCreate {
            knowledge_query: None,
            idoris_headroom_confirmed: false,
            session_id: Some("sess_nope".to_owned()),
            workspace_id: None,
            prompt: "hi".to_owned(),
            model_override: None,
            mode: agent24_protocol::RunMode::Normal,
            knowledge_binding_id: None,
            skill_id: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, AgentError::SessionNotFound(_)));
}

/// Records the tool names offered on each model call.
struct ToolNamesProvider {
    offered: StdMutex<Vec<Vec<String>>>,
}

#[async_trait]
impl ModelProvider for ToolNamesProvider {
    fn name(&self) -> &str {
        "tool-names"
    }
    async fn complete(
        &self,
        req: &CompletionRequest,
        _cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        let names = req.tools.iter().map(|t| t.name.clone()).collect();
        self.offered.lock().unwrap().push(names);
        Ok(CompletionResponse {
            message: Msg::assistant(Some("pong".to_owned()), vec![]),
            usage: usage_one(),
            model_id: None,
        })
    }
    async fn models(
        &self,
        _cancel: &CancellationToken,
    ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
        Ok(vec![])
    }
}

struct KnowledgeOnlyTool;

#[async_trait]
impl agent24_tools::Tool for KnowledgeOnlyTool {
    fn requires_outbound_policy(&self) -> bool {
        false
    }

    fn info(&self) -> agent24_protocol::ToolInfo {
        agent24_protocol::ToolInfo::new(
            "kb_read",
            "builtin",
            "reads Knowledge",
            agent24_protocol::RiskClass::Read,
        )
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn needs_knowledge_binding(&self) -> bool {
        true
    }
    async fn call(
        &self,
        _ctx: &agent24_tools::ToolContext,
        _input: &serde_json::Map<String, serde_json::Value>,
        _cancel: &CancellationToken,
    ) -> Result<String, agent24_tools::ToolError> {
        Ok("ok".to_owned())
    }
}

#[tokio::test]
async fn only_a_run_that_names_a_knowledge_binding_is_offered_a_tool_that_needs_one() {
    let provider = Arc::new(ToolNamesProvider {
        offered: StdMutex::new(vec![]),
    });
    let tools = ToolRegistry::new().with(Arc::new(KnowledgeOnlyTool));
    let (manager, _sink, store) = manager_with_tools(provider.clone(), tools).await;
    let unbound = manager.start_run(create()).await.unwrap();
    wait_terminal(&store, &unbound.id).await;
    let bound = manager
        .start_run(RunCreate {
            idoris_headroom_confirmed: false,
            knowledge_binding_id: Some("b1".to_owned()),
            ..create()
        })
        .await
        .unwrap();
    wait_terminal(&store, &bound.id).await;
    let offered = provider.offered.lock().unwrap().clone();
    assert_eq!(
        offered,
        vec![Vec::<String>::new(), vec!["kb_read".to_owned()]]
    );
    // The same holds in plan mode, next to `propose_plan`.
    let names =
        |specs: Vec<ToolSpec>| -> Vec<String> { specs.into_iter().map(|s| s.name).collect() };
    assert_eq!(
        names(manager.tool_specs_for(true, false).await),
        vec!["propose_plan"]
    );
    assert_eq!(
        names(manager.tool_specs_for(true, true).await),
        vec!["kb_read", "propose_plan"]
    );
}

/// Records the messages and the tool names of each model call.
struct SeenProvider {
    seen: StdMutex<Vec<Vec<Msg>>>,
    tools: StdMutex<Vec<Vec<String>>>,
}

#[async_trait]
impl ModelProvider for SeenProvider {
    fn name(&self) -> &str {
        "seen"
    }
    async fn complete(
        &self,
        req: &CompletionRequest,
        _cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        self.seen.lock().unwrap().push(req.messages.clone());
        self.tools
            .lock()
            .unwrap()
            .push(req.tools.iter().map(|t| t.name.clone()).collect());
        Ok(CompletionResponse {
            message: Msg::assistant(Some("pong".to_owned()), vec![]),
            usage: usage_one(),
            model_id: None,
        })
    }
    async fn models(
        &self,
        _cancel: &CancellationToken,
    ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
        Ok(vec![])
    }
}

/// Stands in for agent24d's `knowledge_search`: answers `reply`, counts calls.
struct KnowledgeSearchStub {
    reply: String,
    calls: std::sync::atomic::AtomicUsize,
    /// Fails instead of answering (an outage, a timeout).
    fail: bool,
}

#[async_trait]
impl agent24_tools::Tool for KnowledgeSearchStub {
    fn requires_outbound_policy(&self) -> bool {
        false
    }
    fn info(&self) -> agent24_protocol::ToolInfo {
        agent24_protocol::ToolInfo::new(
            "knowledge_search",
            "builtin",
            "searches Knowledge",
            agent24_protocol::RiskClass::Read,
        )
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn needs_knowledge_binding(&self) -> bool {
        true
    }
    async fn call(
        &self,
        _ctx: &agent24_tools::ToolContext,
        input: &serde_json::Map<String, serde_json::Value>,
        _cancel: &CancellationToken,
    ) -> Result<String, agent24_tools::ToolError> {
        assert_eq!(input.get("query").and_then(|q| q.as_str()), Some("烧开水要多久"));
        assert_eq!(input.get("max_results").and_then(|q| q.as_u64()), Some(3));
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail {
            return Err(agent24_tools::ToolError::Failed("knowledge unreachable".into()));
        }
        Ok(self.reply.clone())
    }
}

/// M1 retrieval first (David 2026-11-21): a run with a binding and a
/// `knowledge_query` searches once before the model is asked; the hits reach
/// the model as that search's call and result. No hit leaves the thread as it
/// was; so do no binding and a 「记住…」 query (no search at all).
#[tokio::test]
async fn a_knowledge_query_is_searched_before_the_model_and_only_hits_go_in() {
    const HIT: &str = r#"{"kind":"knowledge.search","evidence":[{"rank":1,"snippet":"水烧开约需 5 分钟"}]}"#;
    const NONE: &str = r#"{"kind":"knowledge.search","evidence":[]}"#;
    for (reply, binding, query, searched, goes_in) in [
        (HIT, Some("b1"), "烧开水要多久", 1, true),
        (NONE, Some("b1"), "烧开水要多久", 1, false),
        (HIT, None, "烧开水要多久", 0, false),
        (HIT, Some("b1"), "记住我喜欢芒果", 0, false),
    ] {
        let provider = Arc::new(SeenProvider {
            seen: StdMutex::new(vec![]),
            tools: StdMutex::new(vec![]),
        });
        let stub = Arc::new(KnowledgeSearchStub {
            reply: reply.to_owned(),
            calls: std::sync::atomic::AtomicUsize::new(0),
            fail: false,
        });
        let tools = ToolRegistry::new().with(stub.clone());
        let (manager, _sink, store) = manager_with_tools(provider.clone(), tools).await;
        let run = manager
            .start_run(RunCreate {
                prompt: format!("（知识库已开启）\n{query}"),
                knowledge_binding_id: binding.map(str::to_owned),
                knowledge_query: Some(query.to_owned()),
                ..create()
            })
            .await
            .unwrap();
        let done = wait_terminal(&store, &run.id).await;
        assert_eq!(done.status, RunStatus::Completed, "{query} {binding:?}");
        assert_eq!(
            stub.calls.load(std::sync::atomic::Ordering::SeqCst),
            searched,
            "{query} {binding:?}"
        );
        let first = provider.seen.lock().unwrap()[0].clone();
        // PM (0.6.75): a turn searched first is not offered the search again.
        let offered = provider.tools.lock().unwrap()[0].clone();
        // A 「记住…」 turn is left out: with #1972 it is offered no tools at all.
        if !query.starts_with("记住") {
            assert_eq!(
                offered.contains(&"knowledge_search".to_owned()),
                binding.is_some() && !goes_in,
                "{query} {binding:?} {reply}"
            );
        }
        let result = first.iter().find(|m| m.role == "tool");
        assert_eq!(result.is_some(), goes_in, "{query} {binding:?} {reply}");
        if goes_in {
            let got: serde_json::Value =
                serde_json::from_str(result.unwrap().content.as_deref().unwrap()).unwrap();
            assert_eq!(got["evidence"][0]["snippet"], "水烧开约需 5 分钟");
            let asked = first.iter().find(|m| !m.tool_calls.is_empty()).unwrap();
            assert_eq!(asked.tool_calls[0].name, "knowledge_search");
        }
    }
}

/// M1 (0.6.73): 「记住…」 is saved by personal memory, not by a tool; the model
/// is offered none on that turn (it called fs_write). Any other turn still is.
#[tokio::test]
async fn a_remember_turn_is_offered_no_tools() {
    let provider = Arc::new(ToolNamesProvider {
        offered: StdMutex::new(vec![]),
    });
    let tools = ToolRegistry::new().with(Arc::new(ReadingTool(
        agent24_tools::ToolResultSource::unknown("reader"),
    )));
    let (manager, store, _kv) =
        manager_with_memory_and_tools(provider.clone(), Arc::new(UnusedSummarizer), tools).await;
    for prompt in ["记住：我最喜欢的水果是芒果。", "把这句话改得礼貌一点"] {
        let run = manager
            .start_run(RunCreate {
                prompt: prompt.into(),
                ..create()
            })
            .await
            .unwrap();
        wait_terminal(&store, &run.id).await;
    }
    let offered = provider.offered.lock().unwrap().clone();
    assert_eq!(offered, vec![Vec::<String>::new(), vec!["reader".to_owned()]]);
}

/// Notes, on its first call, what `before_execute` had recorded by then.
struct FirstStepProvider {
    noted: Arc<StdMutex<Option<String>>>,
    at_first_step: StdMutex<Option<Option<String>>>,
}

#[async_trait]
impl ModelProvider for FirstStepProvider {
    fn name(&self) -> &str {
        "first-step"
    }
    async fn complete(
        &self,
        _req: &CompletionRequest,
        _cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        let noted = self.noted.lock().unwrap().clone();
        self.at_first_step.lock().unwrap().get_or_insert(noted);
        Ok(CompletionResponse {
            message: Msg::assistant(Some("pong".to_owned()), vec![]),
            usage: usage_one(),
            model_id: None,
        })
    }
    async fn models(
        &self,
        _cancel: &CancellationToken,
    ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
        Ok(vec![])
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn before_execute_is_done_before_the_run_takes_its_first_step() {
    let noted = Arc::new(StdMutex::new(None));
    let provider = Arc::new(FirstStepProvider {
        noted: noted.clone(),
        at_first_step: StdMutex::new(None),
    });
    let (manager, _sink, store) = manager_with(provider.clone()).await;
    let run = manager
        .start_run_then(create(), |run| {
            // Slow on purpose: a run already executing would reach the
            // model on the other worker meanwhile and find nothing noted.
            std::thread::sleep(Duration::from_millis(300));
            *noted.lock().unwrap() = Some(run.id.clone());
        })
        .await
        .unwrap();
    assert_eq!(
        wait_terminal(&store, &run.id).await.status,
        RunStatus::Completed
    );
    assert_eq!(
        *provider.at_first_step.lock().unwrap(),
        Some(Some(run.id.clone()))
    );
}

#[tokio::test]
async fn before_execute_is_not_called_for_a_run_that_was_not_created() {
    let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
    let mut called = false;
    let err = manager
        .start_run_then(
            RunCreate {
                idoris_headroom_confirmed: false,
                session_id: Some("sess_nope".to_owned()),
                ..create()
            },
            |_| called = true,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AgentError::SessionNotFound(_)));
    assert!(!called);
    // Nor for one the workspace admission denies (no session bound to the
    // workspace): the run was never stored.
    let err = manager
        .start_run_then(
            RunCreate {
                idoris_headroom_confirmed: false,
                workspace_id: Some(
                    agent24_protocol::WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5")
                        .unwrap(),
                ),
                ..create()
            },
            |_| called = true,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AgentError::WorkspaceAdmission(_)), "{err:?}");
    assert!(!called);
    assert!(store.list_runs(None).await.unwrap().is_empty());
}

#[tokio::test]
async fn panicking_provider_still_lands_a_terminal_state() {
    // review #36: a panic in the execution path must not wedge the run
    // non-terminal — the supervisor lands Failed and cleans the token map
    struct PanickingProvider;
    #[async_trait]
    impl ModelProvider for PanickingProvider {
        fn name(&self) -> &str {
            "panics"
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            panic!("provider blew up");
        }
        async fn models(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
            Ok(vec![])
        }
    }
    let (manager, sink, store) = manager_with(Arc::new(PanickingProvider)).await;
    let run = manager.start_run(create()).await.unwrap();
    let done = wait_terminal(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Failed);
    assert_eq!(done.error.unwrap().code, "internal");
    // the cancels map entry is gone: cancel after the panic is a no-op
    // on an already-terminal run, not a dangling token
    let after = manager.cancel_run(&run.id).await.unwrap();
    assert_eq!(after.status, RunStatus::Failed);
    assert!(manager.cancels.lock().await.is_empty());
    let events = sink.0.lock().unwrap().clone();
    assert_eq!(events, vec!["run.started", "run.failed"]);
}

#[tokio::test]
async fn provider_failure_lands_failed_with_error_body() {
    struct DownProvider;
    #[async_trait]
    impl ModelProvider for DownProvider {
        fn name(&self) -> &str {
            "down"
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            Err(ModelError::Unavailable("refused".to_owned()))
        }
        async fn models(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
            Ok(vec![])
        }
    }
    let (manager, sink, store) = manager_with(Arc::new(DownProvider)).await;
    let run = manager.start_run(create()).await.unwrap();
    let done = wait_terminal(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Failed);
    assert_eq!(done.error.unwrap().code, "provider_unavailable");
    let events = sink.0.lock().unwrap().clone();
    assert_eq!(events, vec!["run.started", "run.failed"]);
}

// ── C3: tool execution in the loop ───────────────────────────────────────

/// Canned-response HTTP fixture on a real socket.
async fn http_fixture(body: &'static str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/")
}

fn tool_call_turn(name: &str, arguments: String) -> Msg {
    Msg::assistant(
        None,
        vec![ToolCallRequest {
            id: "call_1".to_owned(),
            name: name.to_owned(),
            arguments,
        }],
    )
}

#[tokio::test]
async fn model_fetches_a_url_through_http_fetch() {
    let url = http_fixture("fixture payload 42").await;
    // allow_local: the fixture lives on loopback
    let (egress_gate, _, _) = fixture_egress_policy();
    let tools = ToolRegistry::new()
        .with(Arc::new(agent24_tools::HttpFetchTool::new(true)))
        .with_egress_gate(egress_gate);
    let provider = ScriptedProvider::new(vec![tool_call_turn(
        "http_fetch",
        serde_json::json!({ "url": url }).to_string(),
    )]);
    let (manager, sink, store) = manager_with_fixture_egress(Arc::new(provider), tools).await;
    let run = manager.start_run(create()).await.unwrap();
    let done = wait_terminal(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);
    let text = done.output.unwrap().text;
    assert!(text.contains("fixture payload 42"), "{text}");
    // two completions' usage accumulated
    assert_eq!(done.usage.total_tokens, 4);

    let events = sink.0.lock().unwrap().clone();
    assert_eq!(
        events,
        vec![
            "run.started",
            "tool.started",
            "tool.completed",
            "model.delta",
            "run.completed"
        ]
    );
    let calls = store.list_tool_calls(&run.id).await.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].tool, "http_fetch");
    assert_eq!(calls[0].status, ToolCallStatus::Completed);
    assert!(calls[0].ended_at.is_some());
}

#[tokio::test]
async fn a_tool_round_trip_persists_the_full_message_thread() {
    // H3 foundation: the durable thread must be a faithful, ordered image of
    // the loop's in-memory `messages` — user prompt → assistant turn bearing
    // the tool_call → the tool result answering it → closing assistant answer.
    // This is what a restarted daemon reconstructs a suspended run from.
    let url = http_fixture("fixture payload 42").await;
    let (egress_gate, _, _) = fixture_egress_policy();
    let tools = ToolRegistry::new()
        .with(Arc::new(agent24_tools::HttpFetchTool::new(true)))
        .with_egress_gate(egress_gate);
    let provider = ScriptedProvider::new(vec![tool_call_turn(
        "http_fetch",
        serde_json::json!({ "url": url }).to_string(),
    )]);
    let (manager, _sink, store) = manager_with_fixture_egress(Arc::new(provider), tools).await;
    let run = manager.start_run(create()).await.unwrap();
    let done = wait_terminal(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);

    let thread = store.list_run_messages(&run.id).await.unwrap();
    assert_eq!(
        thread.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
        vec!["user", "assistant", "tool", "assistant"],
        "{thread:?}"
    );
    // The persisted assistant turn carries the very tool_call the loop keys
    // resume off, and the tool row names the call it answers.
    assert_eq!(thread[1].content, None);
    assert_eq!(thread[1].tool_calls[0]["name"], "http_fetch");
    assert_eq!(thread[2].tool_call_id.as_deref(), Some("call_1"));
    // The closing answer is recorded so the thread is self-contained.
    assert!(
        thread[3]
            .content
            .as_deref()
            .unwrap()
            .contains("fixture payload 42"),
        "{:?}",
        thread[3].content
    );
}

#[tokio::test]
async fn approval_stub_denial_is_persisted_audited_and_survivable() {
    let dir = tempfile::tempdir().unwrap();
    let tools = ToolRegistry::builtin(dir.path().to_path_buf());
    let provider = ScriptedProvider::new(vec![tool_call_turn(
        "shell_exec",
        serde_json::json!({ "argv": ["/bin/echo", "hi"] }).to_string(),
    )]);
    let (manager, sink, store) = manager_with_tools(Arc::new(provider), tools).await;
    let run = manager.start_run(create()).await.unwrap();
    let done = wait_terminal(&store, &run.id).await;
    // The denial goes back to the model, which still answers → completed
    assert_eq!(done.status, RunStatus::Completed);
    assert!(done.output.unwrap().text.contains("denied by policy"));

    let calls = store.list_tool_calls(&run.id).await.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].status, ToolCallStatus::Denied);

    let events = sink.0.lock().unwrap().clone();
    assert!(events.contains(&"tool.started".to_owned()));
    assert!(events.contains(&"tool.completed".to_owned()));

    // audit chain has the denial and still verifies
    store.verify_audit_chain().await.unwrap();
    let entries = store.list_audit().await.unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e.action == "tool.denied" && e.actor == "policy")
    );
}

#[tokio::test]
async fn invalid_tool_arguments_fail_the_call_not_the_run() {
    let tools = ToolRegistry::new().with(Arc::new(agent24_tools::HttpFetchTool::new(true)));
    let provider =
        ScriptedProvider::new(vec![tool_call_turn("http_fetch", "{not json".to_owned())]);
    let (manager, _sink, store) = manager_with_tools(Arc::new(provider), tools).await;
    let run = manager.start_run(create()).await.unwrap();
    let done = wait_terminal(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);
    let calls = store.list_tool_calls(&run.id).await.unwrap();
    assert_eq!(calls[0].status, ToolCallStatus::Failed);
    assert!(
        calls[0]
            .output_summary
            .as_deref()
            .unwrap()
            .contains("not valid JSON")
    );
}

#[tokio::test]
async fn endless_tool_requests_hit_max_iterations() {
    /// Always asks for another tool call — never a final answer.
    struct GreedyProvider;
    #[async_trait]
    impl ModelProvider for GreedyProvider {
        fn name(&self) -> &str {
            "greedy"
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            Ok(CompletionResponse {
                message: Msg::assistant(
                    None,
                    vec![ToolCallRequest {
                        id: "call_x".to_owned(),
                        name: "nope".to_owned(),
                        arguments: "{}".to_owned(),
                    }],
                ),
                usage: usage_one(),
                model_id: None,
            })
        }
        async fn models(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
            Ok(vec![])
        }
    }
    let (manager, _sink, store) =
        manager_with_tools(Arc::new(GreedyProvider), ToolRegistry::new()).await;
    let run = manager.start_run(create()).await.unwrap();
    let done = wait_terminal(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Failed);
    assert_eq!(done.error.unwrap().code, "max_iterations");
    let calls = store.list_tool_calls(&run.id).await.unwrap();
    assert_eq!(calls.len(), MAX_ITERATIONS);
    assert!(calls.iter().all(|c| c.status == ToolCallStatus::Failed));
}

/// 0.6.75 (General): ten passages with every field took a 169-token turn to
/// 5207 and the 4-bit model was stopped at prefill. What the search first
/// puts in is three passages, each cut to 200 characters, citing fields only.
#[test]
fn what_the_first_search_puts_in_is_three_short_passages() {
    let long = "水".repeat(600);
    let hits: Vec<serde_json::Value> = (1..=10)
        .map(|rank| {
            serde_json::json!({
                "rank": rank, "use": "clue", "possibly_stale": false,
                "knowledge_base_id": "kb", "knowledge_id": format!("k{rank}"),
                "chunk_id": format!("c{rank}"), "content_tag": "t",
                "snippet": long, "snippet_truncated": true,
                "display": {"title": "烧水指南", "file_name": "a.pdf"}
            })
        })
        .collect();
    let full = serde_json::json!({
        "notice": "x".repeat(300), "kind": "knowledge.search",
        "completeness": "complete", "scope_narrowed": false, "failures": [],
        "evidence": hits
    })
    .to_string();
    let compact = crate::compact_evidence_for_tests(&full).unwrap();
    let v: serde_json::Value = serde_json::from_str(&compact).unwrap();
    let evidence = v["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 3);
    assert_eq!(evidence[0]["snippet"].as_str().unwrap().chars().count(), 201);
    assert_eq!(evidence[0]["knowledge_id"], "k1");
    assert_eq!(evidence[0]["title"], "烧水指南");
    assert!(evidence[0].get("knowledge_base_id").is_none());
    // PR-Daemon #1976 F1: `kind` stays, for agent24d's record of the hits.
    assert_eq!(v["kind"], "knowledge.search");
    assert_eq!(evidence[0]["use"], "clue");
    // About 600 Chinese characters and short ASCII keys: well under 1000 tokens.
    assert!(compact.chars().count() < 1200, "{}", compact.chars().count());
    assert!(full.chars().count() > 6000);
    assert!(crate::compact_evidence_for_tests(r#"{"evidence":[]}"#).is_none());
}

/// PR-Daemon #1976: a search first that fails leaves the thread as it was and
/// the answer still comes; a plan-mode run is not searched first at all.
#[tokio::test]
async fn a_failed_first_search_or_a_plan_run_goes_on_without_it() {
    const HIT: &str = r#"{"kind":"knowledge.search","evidence":[{"rank":1,"snippet":"x"}]}"#;
    for (fail, mode, searched) in [
        (true, agent24_protocol::RunMode::Normal, 1),
        (false, agent24_protocol::RunMode::Plan, 0),
    ] {
        let provider = Arc::new(SeenProvider {
            seen: StdMutex::new(vec![]),
            tools: StdMutex::new(vec![]),
        });
        let stub = Arc::new(KnowledgeSearchStub {
            reply: HIT.to_owned(),
            calls: std::sync::atomic::AtomicUsize::new(0),
            fail,
        });
        let (manager, _sink, store) =
            manager_with_tools(provider.clone(), ToolRegistry::new().with(stub.clone())).await;
        let run = manager
            .start_run(RunCreate {
                prompt: "烧开水要多久".into(),
                knowledge_binding_id: Some("b1".into()),
                knowledge_query: Some("烧开水要多久".into()),
                mode,
                ..create()
            })
            .await
            .unwrap();
        let done = wait_terminal(&store, &run.id).await;
        assert_eq!(stub.calls.load(std::sync::atomic::Ordering::SeqCst), searched, "{mode:?}");
        if matches!(mode, agent24_protocol::RunMode::Normal) {
            assert_eq!(done.status, RunStatus::Completed);
            let first = provider.seen.lock().unwrap()[0].clone();
            assert!(first.iter().all(|m| m.role != "tool"), "a failed search put nothing in");
        }
    }
}
