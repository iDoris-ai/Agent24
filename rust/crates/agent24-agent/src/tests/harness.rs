struct RecordingSink(StdMutex<Vec<String>>);

impl EventSink for RecordingSink {
    fn emit(&self, body: EventBody) {
        if let Ok(mut v) = self.0.lock() {
            v.push(body.wire_type().to_owned());
        }
    }
}

fn usage_one() -> Usage {
    Usage {
        prompt_tokens: 1,
        completion_tokens: 1,
        total_tokens: 2,
        cost_usd: 0.0,
    }
}

struct FixedProvider;

#[async_trait]
impl ModelProvider for FixedProvider {
    fn name(&self) -> &str {
        "fixed"
    }
    async fn complete(
        &self,
        _req: &CompletionRequest,
        _cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
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

/// Answers "pong" and records the messages it was handed, so a test can
/// assert what context the loop actually built.
pub(super) struct RecordingProvider {
    pub(super) seen: StdMutex<Vec<Vec<Msg>>>,
}

#[async_trait]
impl ModelProvider for RecordingProvider {
    fn name(&self) -> &str {
        "recording"
    }
    async fn complete(
        &self,
        req: &CompletionRequest,
        _cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        self.seen.lock().unwrap().push(req.messages.clone());
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

/// A summarizer that must never be reached in tests that stay under the
/// compaction threshold — calling it is the failure.
struct UnusedSummarizer;

#[async_trait]
impl Summarizer for UnusedSummarizer {
    async fn summarize(
        &self,
        _prior: Option<&str>,
        _messages: &[Msg],
    ) -> std::result::Result<String, String> {
        Err("summarizer should not be needed in this test".to_owned())
    }
}

/// Plays a fixed sequence of assistant turns, then echoes the last tool
/// result as the final answer.
pub(crate) struct ScriptedProvider {
    turns: StdMutex<Vec<Msg>>,
}

impl ScriptedProvider {
    pub(crate) fn new(turns: Vec<Msg>) -> Self {
        Self {
            turns: StdMutex::new(turns),
        }
    }
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    fn name(&self) -> &str {
        "scripted"
    }
    async fn complete(
        &self,
        req: &CompletionRequest,
        _cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        let next = self.turns.lock().unwrap().pop();
        let message = match next {
            Some(turn) => turn,
            None => {
                // Script exhausted: answer with the last tool result
                let last_tool = req
                    .messages
                    .iter()
                    .rev()
                    .find(|m| m.role == "tool")
                    .and_then(|m| m.content.clone())
                    .unwrap_or_else(|| "no tool result".to_owned());
                Msg::assistant(Some(format!("tool said: {last_tool}")), vec![])
            }
        };
        Ok(CompletionResponse {
            message,
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

#[tokio::test]
async fn headroom_confirmation_is_consumed_by_the_first_model_call() {
    struct Recording {
        seen: StdMutex<Vec<bool>>,
    }
    #[async_trait]
    impl ModelProvider for Recording {
        fn name(&self) -> &str {
            "headroom-recording"
        }
        async fn complete(
            &self,
            req: &CompletionRequest,
            _: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            let mut seen = self.seen.lock().unwrap();
            seen.push(req.idoris_headroom_confirmed);
            let message = if seen.len() == 1 {
                tool_call_turn("unknown_tool", "{}".into())
            } else {
                Msg::assistant(Some("done".into()), vec![])
            };
            Ok(CompletionResponse {
                message,
                usage: usage_one(),
                model_id: None,
            })
        }
        async fn models(
            &self,
            _: &CancellationToken,
        ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
            Ok(vec![])
        }
    }
    let provider = Arc::new(Recording {
        seen: StdMutex::new(vec![]),
    });
    let (manager, _, store) = manager_with(provider.clone()).await;
    let run = manager
        .start_run(RunCreate {
            idoris_headroom_confirmed: true,
            ..create()
        })
        .await
        .unwrap();
    let done = wait_terminal(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed, "{done:?}");
    assert_eq!(*provider.seen.lock().unwrap(), [true, false]);
    let run = manager.start_run(create()).await.unwrap();
    assert_eq!(
        wait_terminal(&store, &run.id).await.status,
        RunStatus::Completed
    );
    assert_eq!(*provider.seen.lock().unwrap(), [true, false, false]);
}

struct HangingProvider;

#[async_trait]
impl ModelProvider for HangingProvider {
    fn name(&self) -> &str {
        "hanging"
    }
    async fn complete(
        &self,
        _req: &CompletionRequest,
        cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        cancel.cancelled().await;
        Err(ModelError::Cancelled)
    }
    async fn models(
        &self,
        _cancel: &CancellationToken,
    ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
        Ok(vec![])
    }
}

async fn manager_with_tools(
    provider: Arc<dyn ModelProvider>,
    tools: ToolRegistry,
) -> (Arc<RunManager>, Arc<RecordingSink>, Store) {
    let store = Store::open_memory().await.unwrap();
    let sink = Arc::new(RecordingSink(StdMutex::new(vec![])));
    let manager = RunManager::new(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(provider, Tier::Local)])),
        Arc::new(tools),
        sink.clone(),
        CancellationToken::new(),
    );
    (manager, sink, store)
}

struct FixtureEgressGate;

#[async_trait]
impl agent24_domain::EgressGate for FixtureEgressGate {
    async fn check(
        &self,
        request: &agent24_domain::EgressRequest,
    ) -> Result<(), agent24_domain::EgressDecision> {
        let fixture_resource = request.resources.len() == 1
            && request.resources[0].resource_id == "agent-test-fixture"
            && request.resources[0].revision.as_deref() == Some("fixture-rev-1")
            && request.resources[0].authorization_ref.as_deref()
                == Some("agent-test-fixture-grant")
            && request.resources[0].policy_version == 1
            && !request.resources[0].local_only;
        let fixture_destination = request
            .destination
            .id()
            .is_some_and(|id| id.starts_with("http://127.0.0.1:"));
        if request.purpose == agent24_domain::EgressPurpose::HttpFetch
            && request.remote
            && request.authorization_generation == 1
            && fixture_resource
            && fixture_destination
        {
            Ok(())
        } else {
            Err(agent24_domain::EgressDecision)
        }
    }
}

pub(crate) fn fixture_egress_policy() -> (
    Arc<dyn agent24_domain::EgressGate>,
    Vec<agent24_domain::EgressResource>,
    u64,
) {
    (
        Arc::new(FixtureEgressGate),
        vec![
            agent24_domain::EgressResource::cloud_authorized(
                "agent-test-fixture",
                "fixture-rev-1",
                1,
            )
            .with_authorization_ref("agent-test-fixture-grant"),
        ],
        1,
    )
}

async fn manager_with_fixture_egress(
    provider: Arc<dyn ModelProvider>,
    tools: ToolRegistry,
) -> (Arc<RunManager>, Arc<RecordingSink>, Store) {
    let (manager, sink, store) = manager_with_tools(provider, tools).await;
    let (gate, resources, generation) = fixture_egress_policy();
    (
        manager.with_test_egress_policy(gate, resources, generation),
        sink,
        store,
    )
}

async fn manager_with(
    provider: Arc<dyn ModelProvider>,
) -> (Arc<RunManager>, Arc<RecordingSink>, Store) {
    manager_with_tools(provider, ToolRegistry::new()).await
}

/// A manager with D1 session memory attached (in-memory KV).
async fn manager_with_memory(
    provider: Arc<dyn ModelProvider>,
    summarizer: Arc<dyn Summarizer>,
) -> (Arc<RunManager>, Store, KvStore) {
    manager_with_memory_and_tools(provider, summarizer, ToolRegistry::new()).await
}

struct EventLog(StdMutex<Vec<EventBody>>);

impl EventSink for EventLog {
    fn emit(&self, body: EventBody) {
        self.0.lock().unwrap().push(body);
    }
}

fn skipped_reasons(log: &EventLog) -> Vec<MemoryWriteSkippedPayload> {
    log.0
        .lock()
        .unwrap()
        .iter()
        .filter_map(|body| match body {
            EventBody::MemoryWriteSkipped(payload) => Some(payload.clone()),
            _ => None,
        })
        .collect()
}

/// Same as [`manager_with_memory`], but the sink keeps event bodies.
async fn manager_with_memory_log(
    provider: Arc<dyn ModelProvider>,
) -> (Arc<RunManager>, Store, KvStore, Arc<EventLog>) {
    let store = Store::open_memory().await.unwrap();
    let log = Arc::new(EventLog(StdMutex::new(vec![])));
    let kv = KvStore::open_memory().await.unwrap();
    let manager = RunManager::with_memory(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(provider, Tier::Local)])),
        Arc::new(ToolRegistry::new()),
        log.clone(),
        CancellationToken::new(),
        Some(
            SessionMemory::new(kv.clone(), Arc::new(UnusedSummarizer))
                .with_owner("test-owner".into()),
        ),
    );
    (manager, store, kv, log)
}

async fn manager_with_memory_and_tools(
    provider: Arc<dyn ModelProvider>,
    summarizer: Arc<dyn Summarizer>,
    tools: ToolRegistry,
) -> (Arc<RunManager>, Store, KvStore) {
    let store = Store::open_memory().await.unwrap();
    let sink = Arc::new(RecordingSink(StdMutex::new(vec![])));
    let kv = KvStore::open_memory().await.unwrap();
    let manager = RunManager::with_memory(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(provider, Tier::Local)])),
        Arc::new(tools),
        sink,
        CancellationToken::new(),
        Some(SessionMemory::new(kv.clone(), summarizer).with_owner("test-owner".into())),
    );
    (manager, store, kv)
}

