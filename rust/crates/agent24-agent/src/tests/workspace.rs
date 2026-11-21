/// Insert a session row so `start_run` accepts it.
pub(crate) async fn seed_session(store: &Store, id: &str) {
    let now = now_iso8601();
    store
        .insert_session(&agent24_protocol::Session {
            id: id.to_owned(),
            title: "t".to_owned(),
            channel: "cli".to_owned(),
            workspace_id: None,
            created_at: now.clone(),
            updated_at: now,
        })
        .await
        .unwrap();
}

async fn seed_workspace(store: &Store, id: &str) {
    let now = workspace_timestamp(now_iso8601());
    let expires_at = WorkspaceInstant::parse(&now)
        .unwrap()
        .checked_add_workspace_ttl(
            agent24_store::WorkspaceTtl::new(6 * 24 * 60 * 60 * 1000).unwrap(),
        )
        .unwrap();
    sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?,?,1,'/test/workspace','generation-1','unix',X'0101010101010101',X'0202020202020202')")
        .bind(id).bind(&now).bind(expires_at.as_str()).execute(agent24_store::test_hooks::pool(store)).await.unwrap();
    sqlx::query("INSERT INTO workspace_allocations (allocation_id,workspace_id,root_generation,relative_name,parent_identity_kind,parent_unix_device,parent_unix_inode,root_identity_kind,root_unix_device,root_unix_inode,phase,created_at) VALUES ('wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5',?,'generation-1','root','unix',X'0303030303030303',X'0404040404040404','unix',X'0101010101010101',X'0202020202020202','committed',?)")
        .bind(id).bind(&now).execute(agent24_store::test_hooks::pool(store)).await.unwrap();
}

#[tokio::test]
async fn workspace_run_admission_and_terminal_release_are_active() {
    const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
    seed_workspace(&store, WORKSPACE_ID).await;
    let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
    store
        .insert_session(&agent24_protocol::Session {
            id: "bound".into(),
            title: "bound".into(),
            channel: "desktop".into(),
            workspace_id: Some(workspace_id.clone()),
            created_at: "2026-09-19T00:00:00.000Z".into(),
            updated_at: "2026-09-19T00:00:00.000Z".into(),
        })
        .await
        .unwrap();
    let first = manager
        .start_run(RunCreate {
            knowledge_query: None,
            idoris_headroom_confirmed: false,
            session_id: Some("bound".into()),
            workspace_id: Some(workspace_id.clone()),
            prompt: "workspace run".into(),
            model_override: None,
            mode: agent24_protocol::RunMode::Normal,
            knowledge_binding_id: None,
            skill_id: None,
        })
        .await
        .unwrap();
    let done = wait_terminal(&store, &first.id).await;
    assert_eq!(done.status, RunStatus::Completed);
    let first_release: Option<String> = sqlx::query_scalar(
        "SELECT released_at FROM workspace_leases WHERE owner_id=? AND kind='run'",
    )
    .bind(&first.id)
    .fetch_one(agent24_store::test_hooks::pool(&store))
    .await
    .unwrap();
    assert!(first_release.is_some());

    let second = manager
        .start_run(RunCreate {
            knowledge_query: None,
            idoris_headroom_confirmed: false,
            session_id: Some("bound".into()),
            workspace_id: Some(workspace_id),
            prompt: "workspace run two".into(),
            model_override: None,
            mode: agent24_protocol::RunMode::Normal,
            knowledge_binding_id: None,
            skill_id: None,
        })
        .await
        .unwrap();
    assert_eq!(
        wait_terminal(&store, &second.id).await.status,
        RunStatus::Completed
    );

    let error = manager
        .start_run(RunCreate {
            knowledge_query: None,
            idoris_headroom_confirmed: false,
            session_id: Some("bound".into()),
            workspace_id: None,
            prompt: "must not downgrade".into(),
            model_override: None,
            mode: agent24_protocol::RunMode::Normal,
            knowledge_binding_id: None,
            skill_id: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AgentError::Store(StoreError::Conflict(message))
            if message == "workspace-bound sessions require atomic admission"
    ));
}

#[tokio::test]
async fn workspace_bound_tool_call_never_downgrades_without_authority_service() {
    const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    let tools = ToolRegistry::new().with(Arc::new(agent24_tools::HttpFetchTool::new(true)));
    let provider = ScriptedProvider::new(vec![tool_call_turn(
        "http_fetch",
        serde_json::json!({ "url": "http://127.0.0.1:9/" }).to_string(),
    )]);
    let (manager, _sink, store) = manager_with_tools(Arc::new(provider), tools).await;
    seed_workspace(&store, WORKSPACE_ID).await;
    let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
    store
        .insert_session(&agent24_protocol::Session {
            id: "bound-tool".into(),
            title: "bound-tool".into(),
            channel: "desktop".into(),
            workspace_id: Some(workspace_id.clone()),
            created_at: now_iso8601(),
            updated_at: now_iso8601(),
        })
        .await
        .unwrap();
    let run = manager
        .start_run(RunCreate {
            knowledge_query: None,
            idoris_headroom_confirmed: false,
            session_id: Some("bound-tool".into()),
            workspace_id: Some(workspace_id),
            prompt: "try a tool".into(),
            model_override: None,
            mode: RunMode::Normal,
            knowledge_binding_id: None,
            skill_id: None,
        })
        .await
        .unwrap();
    let done = wait_terminal(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);
    assert!(
        done.output
            .unwrap()
            .text
            .contains("workspace authority unavailable")
    );
    assert_eq!(
        store.list_tool_calls(&run.id).await.unwrap()[0].status,
        ToolCallStatus::Denied
    );
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_bound_fs_uses_fresh_pinned_root_not_legacy_root() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    const WS: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    const LEASE: &str = "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
    let store = Store::open_memory().await.unwrap();
    let state = tempfile::tempdir().unwrap();
    let service = Arc::new(WorkspaceService::compose(store.clone(), state.path()).unwrap());
    let locator = format!("{WS}.g1");
    let parent = state.path().join("workspace-roots");
    let root = parent.join(&locator);
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let parent_meta = std::fs::metadata(&parent).unwrap();
    let root_meta = std::fs::metadata(&root).unwrap();
    let now = workspace_timestamp(now_iso8601());
    let expires = WorkspaceInstant::parse(&now)
        .unwrap()
        .checked_add_workspace_ttl(agent24_store::WorkspaceTtl::new(60_000).unwrap())
        .unwrap();
    sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?,?,1,?,'g1','unix',?,?)")
        .bind(WS).bind(&now).bind(expires.as_str()).bind(root.to_str().unwrap())
        .bind(root_meta.dev().to_le_bytes().to_vec()).bind(root_meta.ino().to_le_bytes().to_vec())
        .execute(agent24_store::test_hooks::pool(&store)).await.unwrap();
    sqlx::query("INSERT INTO workspace_allocations (allocation_id,workspace_id,root_generation,relative_name,parent_identity_kind,parent_unix_device,parent_unix_inode,root_identity_kind,root_unix_device,root_unix_inode,phase,created_at) VALUES ('wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5',?,'g1',?,'unix',?,?,'unix',?,?,'committed',?)")
        .bind(WS).bind(&locator).bind(parent_meta.dev().to_le_bytes().to_vec())
        .bind(parent_meta.ino().to_le_bytes().to_vec()).bind(root_meta.dev().to_le_bytes().to_vec())
        .bind(root_meta.ino().to_le_bytes().to_vec()).bind(&now)
        .execute(agent24_store::test_hooks::pool(&store)).await.unwrap();
    sqlx::query("INSERT INTO sessions (id,title,channel,workspace_id,created_at,updated_at) VALUES ('s','s','desktop',?,?,?)")
        .bind(WS).bind(&now).bind(&now).execute(agent24_store::test_hooks::pool(&store)).await.unwrap();
    sqlx::query("INSERT INTO runs (id,session_id,workspace_id,status,input,usage,created_at) VALUES ('r','s',?,'running',?,?,?)")
        .bind(WS).bind(format!(r#"{{"prompt":"go","workspace_id":"{WS}","model_override":null,"mode":"normal"}}"#))
        .bind(r#"{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0,"cost_usd":0.0}"#).bind(&now)
        .execute(agent24_store::test_hooks::pool(&store)).await.unwrap();
    sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,'g1','r','run',?)")
        .bind(LEASE).bind(WS).bind(&now).execute(agent24_store::test_hooks::pool(&store)).await.unwrap();
    let authority = service
        .bind_run_authority("r", &WorkspaceLeaseId::parse(LEASE).unwrap())
        .await
        .unwrap();
    let ctx = ToolContext::workspace_bound("r", Some("s".into()), None, "tc", authority);
    let legacy = tempfile::tempdir().unwrap();
    let read = agent24_tools::FsReadTool::new(vec![legacy.path().to_path_buf()]);
    let write = agent24_tools::FsWriteTool::new(vec![legacy.path().to_path_buf()]);
    let path = root.join("bound.txt");
    std::fs::write(&path, "bound").unwrap();
    let mut input = serde_json::Map::new();
    input.insert(
        "path".into(),
        serde_json::Value::String(path.to_string_lossy().into()),
    );
    assert_eq!(
        agent24_tools::Tool::call(&read, &ctx, &input, &CancellationToken::new())
            .await
            .unwrap(),
        "bound"
    );
    input.insert(
        "content".into(),
        serde_json::Value::String("updated".into()),
    );
    agent24_tools::Tool::call(&write, &ctx, &input, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "updated");
    let shell = agent24_tools::ShellExecTool::new(legacy.path().to_path_buf());
    let mut shell_input = serde_json::Map::new();
    shell_input.insert("argv".into(), serde_json::json!(["/bin/pwd"]));
    let shell_out =
        agent24_tools::Tool::call(&shell, &ctx, &shell_input, &CancellationToken::new())
            .await
            .unwrap();
    let shell_json: serde_json::Value = serde_json::from_str(&shell_out).unwrap();
    assert_eq!(shell_json["exit_code"], 0);
    assert_eq!(
        std::path::Path::new(shell_json["stdout"].as_str().unwrap().trim()),
        root.canonicalize().unwrap()
    );
    sqlx::query("UPDATE workspace_leases SET released_at=? WHERE lease_id=?")
        .bind(workspace_timestamp(now_iso8601()))
        .bind(LEASE)
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    assert!(
        agent24_tools::Tool::call(&read, &ctx, &input, &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(
        agent24_tools::Tool::call(&shell, &ctx, &shell_input, &CancellationToken::new(),)
            .await
            .is_err()
    );
}

/// K1-6b.4a (ADR-K1-02 §2.1.1, §5 item 9): tool egress is restricted by a
/// source's classification, not its kind. Returns whether a process
/// egress check passes for a run tagged with exactly `sources`.
async fn process_egress_allowed_for(sources: Vec<agent24_store::SourceRef>) -> bool {
    process_egress_allowed_when(sources, false).await
}

async fn process_egress_allowed_when(
    sources: Vec<agent24_store::SourceRef>,
    source_reads_fail: bool,
) -> bool {
    let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
    let now = now_iso8601();
    let run = Run {
        id: format!("run_{}", ulid()),
        session_id: None,
        workspace_id: None,
        status: RunStatus::Running,
        input: RunInput {
            prompt: "classified".into(),
            workspace_id: None,
            model_override: None,
            mode: RunMode::Normal,
            knowledge_binding_id: None,
        },
        output: None,
        error: None,
        usage: zero_usage(),
        schedule_id: None,
        created_at: now.clone(),
        started_at: Some(now.clone()),
        ended_at: None,
    };
    store.insert_run(&run).await.unwrap();
    for source in &sources {
        store
            .tag_run_source(&run.id, 0, source, &now)
            .await
            .unwrap();
    }
    if source_reads_fail {
        agent24_store::test_hooks::fail_source_tag_reads(&store);
    }
    let ctx = manager.tool_context_for(&run, "tc").await.unwrap();
    ctx.authorize_egress_if_restricted(
        agent24_domain::EgressPurpose::ProcessExecution,
        agent24_domain::EgressDestination::unknown(),
    )
    .await
    .is_ok()
}

fn classified(
    mut source: agent24_store::SourceRef,
    classification: agent24_store::SourceClassification,
) -> agent24_store::SourceRef {
    source.classification = classification;
    source
}

#[tokio::test]
async fn ordinary_sources_of_any_kind_do_not_restrict_tool_egress() {
    use agent24_store::{SourceClassification, SourceRef};
    let now = now_iso8601();
    let user = SourceRef::user_input("run_x", now.clone());
    assert_eq!(user.classification, SourceClassification::Ordinary);
    assert!(process_egress_allowed_for(vec![user.clone()]).await);
    // A non-user-input source the host has classified Ordinary (the shape
    // ordinary recalled memory takes in 6b.4c) is not egress material.
    let ordinary_recall = classified(
        SourceRef::unknown_memory(now),
        SourceClassification::Ordinary,
    );
    assert!(process_egress_allowed_for(vec![user, ordinary_recall]).await);
}

#[tokio::test]
async fn restricted_or_unknown_sources_of_any_kind_restrict_tool_egress() {
    use agent24_store::{SourceClassification, SourceRef};
    let now = now_iso8601();
    let user = SourceRef::user_input("run_x", now.clone());
    for source in [
        SourceRef::unknown_memory(now.clone()),
        classified(
            SourceRef::selected_material("doc-1", None, now.clone()),
            SourceClassification::Restricted,
        ),
        // Kind alone must not exempt a source: user input the host has
        // not classified Ordinary keeps the strict policy.
        classified(
            SourceRef::user_input("run_y", now.clone()),
            SourceClassification::Restricted,
        ),
        classified(
            SourceRef::user_input("run_y", now.clone()),
            SourceClassification::Unknown,
        ),
    ] {
        assert!(
            !process_egress_allowed_for(vec![user.clone(), source.clone()]).await,
            "{source:?} must restrict process egress"
        );
    }
}

/// K1-6b.5d: `tool_context_for` restricts tool egress when a run has no
/// source tags at all (ADR-K1-02 §2.1.1 item 4: missing evidence is not
/// permission) and when its tags cannot be read. The same Ordinary run,
/// readable, is allowed.
#[tokio::test]
async fn a_run_with_no_readable_sources_restricts_tool_egress() {
    let user = agent24_store::SourceRef::user_input("run_x", now_iso8601());
    assert!(process_egress_allowed_for(vec![user.clone()]).await);
    assert!(
        !process_egress_allowed_for(vec![]).await,
        "no tags must restrict"
    );
    assert!(
        !process_egress_allowed_when(vec![user], true).await,
        "unreadable tags must restrict"
    );
}

const TOOL_SECRET: &str = "TOOL-SECRET-91c2";

/// Returns [`TOOL_SECRET`]; the host classifies it as `source` says.
struct ReadingTool(agent24_tools::ToolResultSource);

#[async_trait]
impl agent24_tools::Tool for ReadingTool {
    fn info(&self) -> agent24_protocol::ToolInfo {
        agent24_protocol::ToolInfo::new("reader", "builtin", "reads", RiskClass::Read)
    }
    fn requires_outbound_policy(&self) -> bool {
        false
    }
    fn result_source(&self) -> agent24_tools::ToolResultSource {
        self.0.clone()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn call(
        &self,
        _: &ToolContext,
        _: &serde_json::Map<String, serde_json::Value>,
        _: &CancellationToken,
    ) -> Result<String, ToolError> {
        Ok(TOOL_SECRET.to_owned())
    }
}

/// Fails with [`TOOL_SECRET`], as an MCP `is_error` result or a module's
/// error body hands back what the other side sent; the host's default
/// classification applies.
struct FailingReader;

#[async_trait]
impl agent24_tools::Tool for FailingReader {
    fn info(&self) -> agent24_protocol::ToolInfo {
        agent24_protocol::ToolInfo::new("reader", "builtin", "reads", RiskClass::Read)
    }
    fn requires_outbound_policy(&self) -> bool {
        false
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn call(
        &self,
        _: &ToolContext,
        _: &serde_json::Map<String, serde_json::Value>,
        _: &CancellationToken,
    ) -> Result<String, ToolError> {
        Err(ToolError::Failed(TOOL_SECRET.to_owned()))
    }
}

/// Answers whether its call could leave the device, as `shell_exec` asks.
struct EgressProbe;

#[async_trait]
impl agent24_tools::Tool for EgressProbe {
    fn info(&self) -> agent24_protocol::ToolInfo {
        agent24_protocol::ToolInfo::new("probe", "builtin", "probes", RiskClass::Read)
    }
    fn requires_outbound_policy(&self) -> bool {
        false
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        _: &serde_json::Map<String, serde_json::Value>,
        _: &CancellationToken,
    ) -> Result<String, ToolError> {
        ctx.authorize_egress_if_restricted(
            agent24_domain::EgressPurpose::ProcessExecution,
            agent24_domain::EgressDestination::unknown(),
        )
        .await?;
        Ok("egress allowed".to_owned())
    }
}

fn calls(name: &str, id: &str) -> Msg {
    Msg::assistant(
        None,
        vec![agent24_models::ToolCallRequest {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: "{}".to_owned(),
        }],
    )
}

/// Runs `reader` then `probe`, and returns the probe's answer (the final
/// output) and the run's tags.
async fn read_then_probe(
    source: agent24_tools::ToolResultSource,
) -> (Run, Vec<agent24_store::StoredSourceTag>) {
    run_then_probe(Arc::new(ReadingTool(source))).await
}

/// [`read_then_probe`] with `reader` as the tool called first.
async fn run_then_probe(
    reader: Arc<dyn agent24_tools::Tool>,
) -> (Run, Vec<agent24_store::StoredSourceTag>) {
    let tools = ToolRegistry::new().with(reader).with(Arc::new(EgressProbe));
    let provider = ScriptedProvider::new(vec![calls("probe", "c2"), calls("reader", "c1")]);
    let (manager, _sink, store) = manager_with_tools(Arc::new(provider), tools).await;
    let run = manager.start_run(create()).await.unwrap();
    let run = wait_terminal(&store, &run.id).await;
    let tags = store.list_run_source_tags(&run.id).await.unwrap();
    (run, tags)
}

/// K1-6b.5f: a tool's result is tagged as the host classifies that tool,
/// and the tag restricts the run's next egress. An Ordinary result does
/// not.
#[tokio::test]
async fn a_tool_result_is_tagged_and_an_unknown_one_restricts_later_egress() {
    use agent24_store::{SourceClassification, SourceKind};
    let (run, tags) = read_then_probe(agent24_tools::ToolResultSource::unknown("reader")).await;
    assert_eq!(run.status, RunStatus::Completed);
    let output = run.output.unwrap().text;
    assert_eq!(
        output,
        "tool said: denied by policy: outbound policy unavailable"
    );
    let results: Vec<_> = tags
        .iter()
        .filter(|tag| tag.source.kind == SourceKind::ToolResult)
        .collect();
    // The refused probe returned fixed text, so only the reader's
    // result carries a tag.
    let [reader] = results.as_slice() else {
        panic!("only the reader produced content: {results:?}");
    };
    assert!(
        reader
            .source
            .source_id
            .starts_with("tool_result:tool:reader:tc_"),
        "the reader's result is tagged under its origin and call"
    );
    assert_eq!(reader.source.classification, SourceClassification::Unknown);

    let (run, tags) =
        read_then_probe(agent24_tools::ToolResultSource::ordinary("reader")).await;
    assert_eq!(run.output.unwrap().text, "tool said: egress allowed");
    assert!(
        tags.iter()
            .filter(|tag| tag.source.kind == SourceKind::ToolResult)
            .all(
                |tag| tag.source.classification == SourceClassification::Ordinary
                    || tag.source.source_id.starts_with("tool_result:tool:probe:")
            ),
    );
}

/// K1-6b.5f (PR-Daemon #1282 N1): a call that failed hands the model
/// whatever the other side sent, so its error is tagged like a result
/// and restricts the run's next egress.
#[tokio::test]
async fn a_failed_tool_call_is_tagged_and_restricts_later_egress() {
    let (run, tags) = run_then_probe(Arc::new(FailingReader)).await;
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(
        run.output.unwrap().text,
        "tool said: denied by policy: outbound policy unavailable"
    );
    let failed = tags
        .iter()
        .find(|tag| {
            tag.source
                .source_id
                .starts_with("tool_result:tool:reader:tc_")
        })
        .expect("the failed call's error is tagged");
    assert_eq!(
        failed.source.classification,
        agent24_store::SourceClassification::Unknown
    );
}

/// Answers from a script and remembers whether any request it was sent
/// carried [`TOOL_SECRET`].
struct WatchingProvider {
    script: ScriptedProvider,
    saw_secret: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl ModelProvider for WatchingProvider {
    fn name(&self) -> &str {
        "watching"
    }
    async fn complete(
        &self,
        req: &CompletionRequest,
        cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        if req.messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains(TOOL_SECRET))
        }) {
            self.saw_secret
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        self.script.complete(req, cancel).await
    }
    async fn models(
        &self,
        _: &CancellationToken,
    ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
        Ok(vec![])
    }
}

/// K1-6b.5f: a result whose tag cannot be written never reaches the
/// model; the run ends failed instead.
#[tokio::test]
async fn a_tool_result_whose_tag_cannot_be_written_never_reaches_the_model() {
    let provider = Arc::new(WatchingProvider {
        script: ScriptedProvider::new(vec![calls("reader", "c1")]),
        saw_secret: std::sync::atomic::AtomicBool::new(false),
    });
    let tools = ToolRegistry::new().with(Arc::new(ReadingTool(
        agent24_tools::ToolResultSource::unknown("reader"),
    )));
    let (manager, _sink, store) = manager_with_tools(provider.clone(), tools).await;
    sqlx::query(
        "CREATE TRIGGER no_tool_result_tags BEFORE INSERT ON run_source_tags \
         WHEN NEW.source_id LIKE 'tool_result:%' \
         BEGIN SELECT RAISE(ABORT, 'tag write refused'); END",
    )
    .execute(agent24_store::test_hooks::pool(&store))
    .await
    .unwrap();
    let run = manager.start_run(create()).await.unwrap();
    let run = wait_terminal(&store, &run.id).await;
    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(run.error.unwrap().code, "source_policy_unavailable");
    assert!(
        !provider
            .saw_secret
            .load(std::sync::atomic::Ordering::SeqCst),
        "the untagged result reached the model"
    );
}

#[tokio::test]
async fn stale_legacy_tool_context_rejects_current_bound_run() {
    const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
    seed_workspace(&store, WORKSPACE_ID).await;
    let created_at = workspace_timestamp(now_iso8601());
    let run = Run {
        id: format!("run_{}", ulid()),
        session_id: None,
        workspace_id: None,
        status: RunStatus::Running,
        input: RunInput {
            prompt: "legacy".into(),
            workspace_id: None,
            model_override: None,
            mode: RunMode::Normal,
            knowledge_binding_id: None,
        },
        output: None,
        error: None,
        usage: zero_usage(),
        schedule_id: None,
        created_at: created_at.clone(),
        started_at: Some(created_at.clone()),
        ended_at: None,
    };
    store.insert_run(&run).await.unwrap();
    let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
    let mut bound_input = run.input.clone();
    bound_input.workspace_id = Some(workspace_id.clone());
    sqlx::query("UPDATE runs SET workspace_id=?, input=? WHERE id=?")
        .bind(workspace_id.as_str())
        .bind(serde_json::to_string(&bound_input).unwrap())
        .bind(&run.id)
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,?,?,'run',?)")
        .bind(format!("wl_{}", ulid())).bind(WORKSPACE_ID).bind("generation-1")
        .bind(&run.id).bind(&created_at)
        .execute(agent24_store::test_hooks::pool(&store)).await.unwrap();

    assert!(matches!(
        manager.tool_context_for(&run, "tc").await,
        Err(AgentError::Workspace(WorkspaceStoreError::CorruptRow {
            table: "runs",
            field: "workspace_id"
        }))
    ));
}

#[tokio::test]
async fn workspace_admission_error_cleans_cancel_token() {
    const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
    seed_workspace(&store, WORKSPACE_ID).await;
    let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
    store
        .insert_session(&agent24_protocol::Session {
            id: "bound".into(),
            title: "bound".into(),
            channel: "desktop".into(),
            workspace_id: Some(workspace_id.clone()),
            created_at: now_iso8601(),
            updated_at: now_iso8601(),
        })
        .await
        .unwrap();
    sqlx::query("DROP TABLE workspace_allocations")
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    assert!(matches!(
        manager
            .start_run(RunCreate {
                knowledge_query: None,
                idoris_headroom_confirmed: false,
                session_id: Some("bound".into()),
                workspace_id: Some(workspace_id),
                prompt: "fail admission".into(),
                model_override: None,
                mode: RunMode::Normal,
                knowledge_binding_id: None,
                skill_id: None,
            })
            .await,
        Err(AgentError::Workspace(WorkspaceStoreError::Database))
    ));
    assert!(manager.cancels.lock().await.is_empty());
    assert!(store.list_runs(None).await.unwrap().is_empty());
}

#[tokio::test]
async fn workspace_terminal_fails_closed_when_exact_lease_is_missing() {
    const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    let (manager, sink, store) = manager_with(Arc::new(FixedProvider)).await;
    seed_workspace(&store, WORKSPACE_ID).await;
    let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
    store
        .insert_session(&agent24_protocol::Session {
            id: "bound".into(),
            title: "bound".into(),
            channel: "desktop".into(),
            workspace_id: Some(workspace_id.clone()),
            created_at: "2026-09-29T00:00:00.000Z".into(),
            updated_at: "2026-09-29T00:00:00.000Z".into(),
        })
        .await
        .unwrap();
    let created_at = workspace_timestamp(now_iso8601());
    let run = Run {
        id: format!("run_{}", ulid()),
        session_id: Some("bound".into()),
        workspace_id: Some(workspace_id.clone()),
        status: RunStatus::Queued,
        input: RunInput {
            prompt: "go".into(),
            workspace_id: Some(workspace_id),
            model_override: None,
            mode: RunMode::Normal,
            knowledge_binding_id: None,
        },
        output: None,
        error: None,
        usage: zero_usage(),
        schedule_id: None,
        created_at: created_at.clone(),
        started_at: None,
        ended_at: None,
    };
    let lease_id = WorkspaceLeaseId::parse(&format!("wl_{}", ulid())).unwrap();
    assert!(matches!(
        store
            .insert_run_with_workspace_admission(
                &run,
                Some(lease_id.clone()),
                &WorkspaceInstant::parse(&created_at).unwrap(),
            )
            .await
            .unwrap(),
        RunAdmission::Admitted { lease_id: Some(_) }
    ));
    store
        .transition_run(
            &run.id,
            RunStatus::Running,
            RunPatch {
                started_at: Some(created_at),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    sqlx::query("DELETE FROM workspace_leases WHERE lease_id=?")
        .bind(lease_id.as_str())
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();

    manager.finish_cancelled(&run.id).await;

    assert_eq!(
        store.get_run(&run.id).await.unwrap().unwrap().status,
        RunStatus::Running
    );
    assert!(
        !sink
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|event| event == "run.cancelled")
    );
}

#[tokio::test]
async fn cancelling_workspace_run_releases_exact_lease() {
    const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    let (manager, _sink, store) = manager_with(Arc::new(HangingProvider)).await;
    seed_workspace(&store, WORKSPACE_ID).await;
    let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
    store
        .insert_session(&agent24_protocol::Session {
            id: "bound".into(),
            title: "bound".into(),
            channel: "desktop".into(),
            workspace_id: Some(workspace_id.clone()),
            created_at: "2026-09-29T00:00:00.000Z".into(),
            updated_at: "2026-09-29T00:00:00.000Z".into(),
        })
        .await
        .unwrap();
    let run = manager
        .start_run(RunCreate {
            knowledge_query: None,
            idoris_headroom_confirmed: false,
            session_id: Some("bound".into()),
            workspace_id: Some(workspace_id),
            prompt: "hang".into(),
            model_override: None,
            mode: RunMode::Normal,
            knowledge_binding_id: None,
            skill_id: None,
        })
        .await
        .unwrap();
    manager.cancel_run(&run.id).await.unwrap();
    assert_eq!(
        wait_terminal(&store, &run.id).await.status,
        RunStatus::Cancelled
    );
    let released: Option<String> = sqlx::query_scalar(
        "SELECT released_at FROM workspace_leases WHERE owner_id=? AND kind='run'",
    )
    .bind(&run.id)
    .fetch_one(agent24_store::test_hooks::pool(&store))
    .await
    .unwrap();
    assert!(released.is_some());
}

#[tokio::test]
async fn tokenless_workspace_cancel_recovers_active_released_and_missing_lease() {
    const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    for mode in 0..4 {
        let (manager, sink, store) = manager_with(Arc::new(FixedProvider)).await;
        seed_workspace(&store, WORKSPACE_ID).await;
        let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
        store
            .insert_session(&agent24_protocol::Session {
                id: "bound".into(),
                title: "bound".into(),
                channel: "desktop".into(),
                workspace_id: Some(workspace_id.clone()),
                created_at: "2026-09-29T00:00:00.000Z".into(),
                updated_at: "2026-09-29T00:00:00.000Z".into(),
            })
            .await
            .unwrap();
        let created_at = workspace_timestamp(now_iso8601());
        let run = Run {
            id: format!("run_{}", ulid()),
            session_id: Some("bound".into()),
            workspace_id: Some(workspace_id.clone()),
            status: RunStatus::Queued,
            input: RunInput {
                prompt: "stale".into(),
                workspace_id: Some(workspace_id),
                model_override: None,
                mode: RunMode::Normal,
                knowledge_binding_id: None,
            },
            output: None,
            error: None,
            usage: zero_usage(),
            schedule_id: None,
            created_at: created_at.clone(),
            started_at: None,
            ended_at: None,
        };
        let lease_id = WorkspaceLeaseId::parse(&format!("wl_{}", ulid())).unwrap();
        assert!(matches!(
            store
                .insert_run_with_workspace_admission(
                    &run,
                    Some(lease_id.clone()),
                    &WorkspaceInstant::parse(&created_at).unwrap(),
                )
                .await
                .unwrap(),
            RunAdmission::Admitted { lease_id: Some(_) }
        ));
        store
            .transition_run(
                &run.id,
                RunStatus::Running,
                RunPatch {
                    started_at: Some(created_at.clone()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        if mode == 1 {
            sqlx::query("UPDATE workspace_leases SET released_at=? WHERE lease_id=?")
                .bind(&created_at)
                .bind(lease_id.as_str())
                .execute(agent24_store::test_hooks::pool(&store))
                .await
                .unwrap();
        } else if mode == 2 {
            sqlx::query("DELETE FROM workspace_leases WHERE lease_id=?")
                .bind(lease_id.as_str())
                .execute(agent24_store::test_hooks::pool(&store))
                .await
                .unwrap();
        } else if mode == 3 {
            sqlx::query("UPDATE runs SET input=json_set(input,'$.workspace_id',?) WHERE id=?")
                .bind("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X7")
                .bind(&run.id)
                .execute(agent24_store::test_hooks::pool(&store))
                .await
                .unwrap();
            assert!(matches!(
                manager.cancel_run(&run.id).await,
                Err(AgentError::Store(StoreError::Conflict(message)))
                    if message == "run workspace identity mismatch"
            ));
            let raw_status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id=?")
                .bind(&run.id)
                .fetch_one(agent24_store::test_hooks::pool(&store))
                .await
                .unwrap();
            assert_eq!(raw_status, "running");
            assert!(
                !sink
                    .0
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|event| event.as_str() == "run.cancelled")
            );
            continue;
        }

        assert_eq!(
            manager.cancel_run(&run.id).await.unwrap().status,
            RunStatus::Cancelled
        );
        let active: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM workspace_leases WHERE owner_id=? AND kind='run' AND released_at IS NULL",
        )
        .bind(&run.id)
        .fetch_one(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
        assert_eq!(active, 0);
        assert_eq!(
            sink.0
                .lock()
                .unwrap()
                .iter()
                .filter(|event| event.as_str() == "run.cancelled")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn timed_out_recovery_handles_released_and_missing_workspace_lease_history() {
    const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    for missing in [false, true] {
        let (manager, sink, store) = manager_with(Arc::new(FixedProvider)).await;
        seed_workspace(&store, WORKSPACE_ID).await;
        let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
        store
            .insert_session(&agent24_protocol::Session {
                id: "bound".into(),
                title: "bound".into(),
                channel: "desktop".into(),
                workspace_id: Some(workspace_id.clone()),
                created_at: "2026-09-29T00:00:00.000Z".into(),
                updated_at: "2026-09-29T00:00:00.000Z".into(),
            })
            .await
            .unwrap();
        let created_at = workspace_timestamp(now_iso8601());
        let run = Run {
            id: format!("run_{}", ulid()),
            session_id: Some("bound".into()),
            workspace_id: Some(workspace_id.clone()),
            status: RunStatus::Queued,
            input: RunInput {
                prompt: "stale approval".into(),
                workspace_id: Some(workspace_id),
                model_override: None,
                mode: RunMode::Normal,
                knowledge_binding_id: None,
            },
            output: None,
            error: None,
            usage: zero_usage(),
            schedule_id: None,
            created_at: created_at.clone(),
            started_at: None,
            ended_at: None,
        };
        let lease_id = WorkspaceLeaseId::parse(&format!("wl_{}", ulid())).unwrap();
        assert!(matches!(
            store
                .insert_run_with_workspace_admission(
                    &run,
                    Some(lease_id.clone()),
                    &WorkspaceInstant::parse(&created_at).unwrap(),
                )
                .await
                .unwrap(),
            RunAdmission::Admitted { lease_id: Some(_) }
        ));
        sqlx::query("UPDATE runs SET status='awaiting_approval', started_at=? WHERE id=?")
            .bind(&created_at)
            .bind(&run.id)
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        let approval = Approval {
            id: format!("apr_{}", ulid()),
            run_id: run.id.clone(),
            tool_call_id: format!("tc_{}", ulid()),
            kind: "exec".into(),
            summary: "expired".into(),
            payload: serde_json::Map::new(),
            available_decisions: vec!["approve".into(), "deny".into(), "abort".into()],
            standing_target: None,
            status: ApprovalStatus::TimedOut,
            decision: None,
            expires_at: created_at.clone(),
            created_at: created_at.clone(),
            decided_at: Some(created_at.clone()),
        };
        store.insert_approval(&approval).await.unwrap();
        if missing {
            sqlx::query("DELETE FROM workspace_leases WHERE lease_id=?")
                .bind(lease_id.as_str())
                .execute(agent24_store::test_hooks::pool(&store))
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE workspace_leases SET released_at=? WHERE lease_id=?")
                .bind(&created_at)
                .bind(lease_id.as_str())
                .execute(agent24_store::test_hooks::pool(&store))
                .await
                .unwrap();
        }

        assert_eq!(manager.recover_timed_out_approval_runs().await.unwrap(), 1);
        assert_eq!(
            store.get_run(&run.id).await.unwrap().unwrap().status,
            RunStatus::Cancelled
        );
        let active: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM workspace_leases WHERE owner_id=? AND kind='run' AND released_at IS NULL",
        )
        .bind(&run.id)
        .fetch_one(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
        assert_eq!(active, 0);
        let seen = sink.0.lock().unwrap().clone();
        assert_eq!(
            seen.iter()
                .filter(|event| event.as_str() == "run.cancelled")
                .count(),
            1
        );
        assert!(!seen.iter().any(|event| event.as_str() == "run.started"));
    }
}

