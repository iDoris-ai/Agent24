#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::tests::fixture_egress_policy;
use super::tests::*;
use super::*;
use agent24_models::router::Tier;
use agent24_policy::{ApprovalBroker, ApprovalRequest, BrokerGate, Verdict};
use agent24_protocol::{ApprovalStatus, Decision, RiskClass};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

struct Harness {
    manager: Arc<RunManager>,
    broker: Arc<ApprovalBroker>,
    store: Store,
    events: Arc<StdMutex<Vec<String>>>,
}

/// Real broker + gate + registry with a live shell_exec, driven by a
/// scripted provider asking for one shell_exec call.
async fn harness(workdir: std::path::PathBuf) -> Harness {
    let store = Store::open_memory().await.unwrap();
    let events = Arc::new(StdMutex::new(Vec::new()));
    let ev = Arc::clone(&events);
    let emit: Arc<dyn Fn(EventBody) + Send + Sync> = Arc::new(move |body: EventBody| {
        if let Ok(mut v) = ev.lock() {
            v.push(body.wire_type().to_owned());
        }
    });
    let broker = ApprovalBroker::new(store.clone(), Arc::clone(&emit), Duration::from_secs(30));
    let (egress_gate, _resources, generation) = fixture_egress_policy();
    let tools = ToolRegistry::builtin(workdir)
        .with_gate(Arc::new(BrokerGate::new(Arc::clone(&broker))))
        .with_egress_gate(Arc::clone(&egress_gate));
    struct FnSink(Arc<dyn Fn(EventBody) + Send + Sync>);
    impl EventSink for FnSink {
        fn emit(&self, body: EventBody) {
            (self.0)(body);
        }
    }
    let provider = ScriptedProvider::new(vec![Msg::assistant(
        None,
        vec![agent24_models::ToolCallRequest {
            id: "call_1".to_owned(),
            name: "shell_exec".to_owned(),
            arguments: serde_json::json!({ "argv": ["/bin/echo", "approved-output"] }).to_string(),
        }],
    )]);
    let manager = RunManager::new(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(
            Arc::new(provider),
            Tier::Local,
        )])),
        Arc::new(tools),
        Arc::new(FnSink(emit)),
        CancellationToken::new(),
    )
    .with_test_egress_policy(egress_gate, Vec::new(), generation);
    Harness {
        manager,
        broker,
        store,
        events,
    }
}

async fn wait_pending(store: &Store) -> String {
    for _ in 0..200 {
        let pending = store
            .list_approvals(Some(ApprovalStatus::Pending))
            .await
            .unwrap();
        if let Some(a) = pending.first() {
            return a.id.clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no pending approval appeared");
}

fn decision(kind: &str, reason: Option<&str>) -> Decision {
    Decision {
        kind: kind.to_owned(),
        reason: reason.map(str::to_owned),
        extra: serde_json::Map::new(),
    }
}

#[tokio::test]
async fn approved_shell_exec_actually_executes() {
    let dir = tempfile::tempdir().unwrap();
    let h = harness(dir.path().to_path_buf()).await;
    let run = h.manager.start_run(create()).await.unwrap();
    let id = wait_pending(&h.store).await;
    // While the approval is pending the run is AWAITING_APPROVAL (SPEC
    // §1.2) — the pending row is created inside the gate, which runs
    // strictly after the awaiting transition, so this read is race-free
    let blocked = h.store.get_run(&run.id).await.unwrap().unwrap();
    assert_eq!(blocked.status, RunStatus::AwaitingApproval);
    h.broker
        .resolve(&id, decision("approve", None))
        .await
        .unwrap();
    let done = wait_terminal(&h.store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);
    assert!(done.output.unwrap().text.contains("approved-output"));
    let calls = h.store.list_tool_calls(&run.id).await.unwrap();
    assert_eq!(calls[0].status, ToolCallStatus::Completed);
    let seen = h.events.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![
            "run.started",
            "tool.started",
            "approval.required",
            "approval.resolved",
            "tool.completed",
            "model.delta",
            "run.completed"
        ]
    );
}

// ── H8 plan mode ─────────────────────────────────────────────────────────

fn create_plan() -> RunCreate {
    RunCreate {
        knowledge_query: None,
        idoris_headroom_confirmed: false,
        session_id: None,
        workspace_id: None,
        prompt: "do the thing".to_owned(),
        model_override: None,
        mode: RunMode::Plan,
        knowledge_binding_id: None,
        skill_id: None,
    }
}

fn propose_plan_call() -> Msg {
    Msg::assistant(
        None,
        vec![agent24_models::ToolCallRequest {
            id: "plan_1".to_owned(),
            name: "propose_plan".to_owned(),
            arguments: serde_json::json!({ "plan": "delete the notes file" }).to_string(),
        }],
    )
}

/// A real broker + gate + builtin registry driven by a caller-supplied
/// script (unlike [`harness`], whose script is fixed).
async fn scripted_plan_harness(workdir: std::path::PathBuf, turns: Vec<Msg>) -> Harness {
    let store = Store::open_memory().await.unwrap();
    let events = Arc::new(StdMutex::new(Vec::new()));
    let ev = Arc::clone(&events);
    let emit: Arc<dyn Fn(EventBody) + Send + Sync> = Arc::new(move |body: EventBody| {
        if let Ok(mut v) = ev.lock() {
            v.push(body.wire_type().to_owned());
        }
    });
    let broker = ApprovalBroker::new(store.clone(), Arc::clone(&emit), Duration::from_secs(30));
    let (egress_gate, resources, generation) = fixture_egress_policy();
    let tools = ToolRegistry::builtin(workdir)
        .with_gate(Arc::new(BrokerGate::new(Arc::clone(&broker))))
        .with_egress_gate(Arc::clone(&egress_gate));
    struct FnSink(Arc<dyn Fn(EventBody) + Send + Sync>);
    impl EventSink for FnSink {
        fn emit(&self, body: EventBody) {
            (self.0)(body);
        }
    }
    let provider = ScriptedProvider::new(turns);
    let manager = RunManager::new(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(
            Arc::new(provider),
            Tier::Local,
        )])),
        Arc::new(tools),
        Arc::new(FnSink(emit)),
        CancellationToken::new(),
    )
    .with_test_egress_policy(egress_gate, resources, generation);
    Harness {
        manager,
        broker,
        store,
        events,
    }
}

#[tokio::test]
async fn plan_mode_advertises_only_read_tools_and_propose_plan() {
    let dir = tempfile::tempdir().unwrap();
    let h = scripted_plan_harness(dir.path().to_path_buf(), vec![]).await;
    let plan: Vec<String> = h
        .manager
        .tool_specs_for(true, false)
        .await
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert!(plan.contains(&"propose_plan".to_owned()));
    assert!(plan.contains(&"fs_read".to_owned()));
    assert!(
        !plan.contains(&"shell_exec".to_owned()),
        "plan mode must not offer exec"
    );
    assert!(
        !plan.contains(&"fs_write".to_owned()),
        "plan mode must not offer write"
    );
    let full: Vec<String> = h
        .manager
        .tool_specs_for(false, false)
        .await
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert!(full.contains(&"shell_exec".to_owned()));
    assert!(
        !full.contains(&"propose_plan".to_owned()),
        "propose_plan is plan-mode only"
    );
}

#[tokio::test]
async fn plan_approved_unlocks_the_run_to_completion() {
    let dir = tempfile::tempdir().unwrap();
    // ScriptedProvider pops from the end, so turns are listed last-first.
    let h = scripted_plan_harness(
        dir.path().to_path_buf(),
        vec![
            Msg::assistant(Some("done".to_owned()), vec![]),
            propose_plan_call(),
        ],
    )
    .await;
    let run = h.manager.start_run(create_plan()).await.unwrap();
    let id = wait_pending(&h.store).await;
    let ap = h.store.get_approval(&id).await.unwrap().unwrap();
    assert_eq!(ap.kind, "plan");
    assert_eq!(
        ap.available_decisions,
        vec!["approve".to_owned(), "deny".to_owned()],
        "a plan is human-only: no approve_for_session / target grant is offered"
    );
    assert_eq!(
        h.store.get_run(&run.id).await.unwrap().unwrap().status,
        RunStatus::AwaitingApproval
    );
    h.broker
        .resolve(&id, decision("approve", None))
        .await
        .unwrap();
    let done = wait_terminal(&h.store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);
    assert_eq!(done.output.unwrap().text, "done");
}

#[tokio::test]
async fn plan_denied_ends_the_run_without_executing() {
    let dir = tempfile::tempdir().unwrap();
    // The shell_exec turn would run only if the plan were approved — a
    // rejected plan must never reach it. (Turns are listed last-first: the
    // ScriptedProvider pops from the end, so propose_plan runs first.)
    let h = scripted_plan_harness(
        dir.path().to_path_buf(),
        vec![
            Msg::assistant(
                None,
                vec![agent24_models::ToolCallRequest {
                    id: "x".to_owned(),
                    name: "shell_exec".to_owned(),
                    arguments: serde_json::json!({ "argv": ["/bin/echo", "SHOULD_NOT_RUN"] })
                        .to_string(),
                }],
            ),
            propose_plan_call(),
        ],
    )
    .await;
    let run = h.manager.start_run(create_plan()).await.unwrap();
    let id = wait_pending(&h.store).await;
    h.broker
        .resolve(&id, decision("deny", Some("no")))
        .await
        .unwrap();
    let done = wait_terminal(&h.store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);
    assert!(done.output.unwrap().text.contains("rejected"));
    // Only the propose_plan call exists — shell_exec never ran.
    let calls = h.store.list_tool_calls(&run.id).await.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].tool, "propose_plan");
    assert_eq!(calls[0].status, ToolCallStatus::Denied);
}

#[tokio::test]
async fn plan_mode_denies_a_direct_write_call_at_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    // A rogue provider emits shell_exec DIRECTLY in plan mode, without ever
    // proposing a plan. It must be denied at the dispatch boundary — never
    // reaching the gate (where a standing grant / Guardian could otherwise
    // auto-approve it). Turns are last-first: final answer, then the rogue
    // call.
    let h = scripted_plan_harness(
        dir.path().to_path_buf(),
        vec![
            Msg::assistant(Some("stopped".to_owned()), vec![]),
            Msg::assistant(
                None,
                vec![agent24_models::ToolCallRequest {
                    id: "rogue".to_owned(),
                    name: "shell_exec".to_owned(),
                    arguments: serde_json::json!({ "argv": ["/bin/echo", "PWNED"] }).to_string(),
                }],
            ),
        ],
    )
    .await;
    let run = h.manager.start_run(create_plan()).await.unwrap();
    let done = wait_terminal(&h.store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);
    // The rogue call was refused before it could become a gated tool call,
    // so no shell_exec ever ran and the run finished on the next model turn.
    assert_eq!(done.output.unwrap().text, "stopped");
    let calls = h.store.list_tool_calls(&run.id).await.unwrap();
    assert!(
        calls.iter().all(|c| c.tool != "shell_exec"),
        "a write/exec call in plan mode must be denied at dispatch, never executed"
    );
}

/// Like [`harness`] but with an EMPTY script: the first model call after a
/// resume answers from the last tool result, so the run finishes as soon as
/// the parked call has been settled and executed.
async fn resume_harness(workdir: std::path::PathBuf) -> Harness {
    resume_harness_with(workdir, None).await
}

async fn resume_harness_with(
    workdir: std::path::PathBuf,
    extra: Option<Arc<dyn agent24_tools::Tool>>,
) -> Harness {
    resume_harness_using(workdir, extra, true).await
}

/// `fixture_egress: false` builds tool egress as production does: no
/// gate on the registry and no test policy, so a run's own source tags
/// alone decide what its tools may do.
async fn resume_harness_using(
    workdir: std::path::PathBuf,
    extra: Option<Arc<dyn agent24_tools::Tool>>,
    fixture_egress: bool,
) -> Harness {
    let store = Store::open_memory().await.unwrap();
    let events = Arc::new(StdMutex::new(Vec::new()));
    let ev = Arc::clone(&events);
    let emit: Arc<dyn Fn(EventBody) + Send + Sync> = Arc::new(move |body: EventBody| {
        if let Ok(mut v) = ev.lock() {
            v.push(body.wire_type().to_owned());
        }
    });
    let broker = ApprovalBroker::new(store.clone(), Arc::clone(&emit), Duration::from_secs(30));
    let (egress_gate, _resources, generation) = fixture_egress_policy();
    let mut tools =
        ToolRegistry::builtin(workdir).with_gate(Arc::new(BrokerGate::new(Arc::clone(&broker))));
    if fixture_egress {
        tools = tools.with_egress_gate(Arc::clone(&egress_gate));
    }
    if let Some(tool) = extra {
        tools.register_module_tool(tool).unwrap();
    }
    struct FnSink(Arc<dyn Fn(EventBody) + Send + Sync>);
    impl EventSink for FnSink {
        fn emit(&self, body: EventBody) {
            (self.0)(body);
        }
    }
    let manager = RunManager::new(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(
            Arc::new(ScriptedProvider::new(vec![])),
            Tier::Local,
        )])),
        Arc::new(tools),
        Arc::new(FnSink(emit)),
        CancellationToken::new(),
    );
    let manager = if fixture_egress {
        manager.with_test_egress_policy(egress_gate, Vec::new(), generation)
    } else {
        manager
    };
    Harness {
        manager,
        broker,
        store,
        events,
    }
}

/// [`resume_harness`] plus `explore`, sharing the recording router so a
/// test can see a provider call made from the sub-agent.
async fn resume_harness_with_explorer(
    workdir: std::path::PathBuf,
) -> (Harness, Arc<RecordingProvider>) {
    resume_harness_with_explorer_on(workdir, Store::open_memory().await.unwrap()).await
}

/// [`resume_harness_with_explorer`] over a store the test opened itself.
async fn resume_harness_with_explorer_on(
    workdir: std::path::PathBuf,
    store: Store,
) -> (Harness, Arc<RecordingProvider>) {
    let events = Arc::new(StdMutex::new(Vec::new()));
    let ev = Arc::clone(&events);
    let emit: Arc<dyn Fn(EventBody) + Send + Sync> = Arc::new(move |body: EventBody| {
        if let Ok(mut v) = ev.lock() {
            v.push(body.wire_type().to_owned());
        }
    });
    let broker = ApprovalBroker::new(store.clone(), Arc::clone(&emit), Duration::from_secs(30));
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let router = Arc::new(ModelRouter::with_defaults(vec![(
        provider.clone() as Arc<dyn agent24_models::ModelProvider>,
        Tier::Local,
    )]));
    let explorer = crate::subagent::ExplorerSubagent::new(
        Arc::clone(&router),
        Arc::new(ToolRegistry::read_only(workdir.clone())),
    );
    let tools = ToolRegistry::builtin(workdir)
        .with(Arc::new(explorer))
        .with_gate(Arc::new(BrokerGate::new(Arc::clone(&broker))));
    struct FnSink(Arc<dyn Fn(EventBody) + Send + Sync>);
    impl EventSink for FnSink {
        fn emit(&self, body: EventBody) {
            (self.0)(body);
        }
    }
    let manager = RunManager::new(
        store.clone(),
        router,
        Arc::new(tools),
        Arc::new(FnSink(emit)),
        CancellationToken::new(),
    );
    (
        Harness {
            manager,
            broker,
            store,
            events,
        },
        provider,
    )
}

/// Park shell_exec (approval target) and explore on one assistant turn.
/// `sourced` writes UserPrompt and Assistant origins. Otherwise the rows
/// are legacy `append_run_message` calls with no origin rows.
async fn resume_shell_and_explore(
    sourced: bool,
    decision_kind: &str,
    reason: Option<&str>,
    shell_token: &str,
) -> (Harness, Arc<RecordingProvider>, Run) {
    let dir = tempfile::tempdir().unwrap();
    let (h, provider) = resume_harness_with_explorer(dir.path().to_path_buf()).await;
    let shell_args = serde_json::json!({ "argv": ["/bin/echo", shell_token] });
    let explore_args = serde_json::json!({ "task": "SECRET-TASK-1197" });
    let calls = serde_json::json!([
        {
            "id": "call_shell",
            "name": "shell_exec",
            "arguments": shell_args.to_string()
        },
        {
            "id": "call_explore",
            "name": "explore",
            "arguments": explore_args.to_string()
        }
    ]);
    let run = run_in("run_1", RunStatus::AwaitingApproval);
    h.store.insert_run(&run).await.unwrap();
    if sourced {
        seed_proven_parked_thread(&h.store, "run_1", "run echo", calls).await;
        let user_source = agent24_store::SourceRef::user_input("run_1", now_iso8601());
        h.store
            .tag_run_source("run_1", 0, &user_source, &now_iso8601())
            .await
            .unwrap();
    } else {
        h.store
            .append_run_message(
                "run_1",
                "user",
                Some("run echo"),
                &serde_json::json!([]),
                None,
                &now_iso8601(),
            )
            .await
            .unwrap();
        h.store
            .append_run_message("run_1", "assistant", None, &calls, None, &now_iso8601())
            .await
            .unwrap();
    }
    let approval = seed_approval(
        "apr_1",
        "run_1",
        "tc_internal_1",
        shell_args.as_object().unwrap().clone(),
    );
    insert_parked(&h.store, &approval, "shell_exec").await;
    h.broker
        .resolve("apr_1", decision(decision_kind, reason))
        .await
        .unwrap();
    h.manager
        .resume_run("run_1".to_owned(), "apr_1".to_owned())
        .await
        .unwrap();
    let done = wait_terminal(&h.store, "run_1").await;
    (h, provider, done)
}

/// K1-R3 release review: a run that a build before migration 0031 started
/// and parked holds tool results no tag describes. After the upgrade it
/// is not resumed, however well its thread's origins are proven.
#[tokio::test]
async fn a_run_started_before_tool_result_tagging_is_not_resumed() {
    let dir = tempfile::tempdir().unwrap();
    let (h, provider) = resume_harness_with_explorer(dir.path().to_path_buf()).await;
    let shell_args = serde_json::json!({ "argv": ["/bin/echo", "PRE-UPGRADE"] });
    let calls = serde_json::json!([{
        "id": "call_shell",
        "name": "shell_exec",
        "arguments": shell_args.to_string()
    }]);
    h.store
        .insert_run(&run_in("run_1", RunStatus::AwaitingApproval))
        .await
        .unwrap();
    // What migration 0033 writes for a run unfinished at the upgrade.
    sqlx::query("INSERT INTO runs_before_tool_result_tagging (run_id) VALUES ('run_1')")
        .execute(agent24_store::test_hooks::pool(&h.store))
        .await
        .unwrap();
    seed_proven_parked_thread(&h.store, "run_1", "run echo", calls).await;
    let user_source = agent24_store::SourceRef::user_input("run_1", now_iso8601());
    h.store
        .tag_run_source("run_1", 0, &user_source, &now_iso8601())
        .await
        .unwrap();
    let approval = seed_approval(
        "apr_1",
        "run_1",
        "tc_internal_1",
        shell_args.as_object().unwrap().clone(),
    );
    insert_parked(&h.store, &approval, "shell_exec").await;
    h.broker
        .resolve("apr_1", decision("approve", None))
        .await
        .unwrap();
    h.manager
        .resume_run("run_1".to_owned(), "apr_1".to_owned())
        .await
        .unwrap();
    let done = wait_terminal(&h.store, "run_1").await;
    assert_resume_refused_before_tools(&h, &provider, &done);
    assert_shell_did_not_run(&h).await;
}

/// The whole upgrade path of the issue #1498 follow-up: a library a build
/// before tool result tagging left (schema 0030, a run parked on an
/// approval) is migrated straight to 0033, and the run's resume is
/// refused: no shell, no model call.
#[tokio::test]
async fn a_run_parked_at_schema_0030_is_not_resumed_after_migrating_to_0033() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("agent24.db");
    agent24_store::test_hooks::prepare_db_stopped_before(&db, 31)
        .await
        .unwrap();
    assert_eq!(
        agent24_store::test_hooks::applied_schema_version(&db)
            .await
            .unwrap(),
        30
    );
    {
        let legacy = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{}", db.display()))
            .await
            .unwrap();
        sqlx::raw_sql(
            "INSERT INTO runs (id, session_id, status, input, usage, created_at) VALUES \
                ('run_1', NULL, 'awaiting_approval', '{\"prompt\":\"go\"}', \
                '{\"prompt_tokens\":0,\"completion_tokens\":0,\"total_tokens\":0}', 't');",
        )
        .execute(&legacy)
        .await
        .unwrap();
        legacy.close().await;
    }
    let store = Store::open(&db).await.unwrap();
    assert_eq!(
        agent24_store::test_hooks::applied_schema_version(&db)
            .await
            .unwrap(),
        agent24_store::test_hooks::embedded_schema_version()
    );
    let (h, provider) = resume_harness_with_explorer_on(dir.path().to_path_buf(), store).await;
    let shell_args = serde_json::json!({ "argv": ["/bin/echo", "PRE-UPGRADE"] });
    let calls = serde_json::json!([{
        "id": "call_shell",
        "name": "shell_exec",
        "arguments": shell_args.to_string()
    }]);
    // The thread and approval as the old build left them, written through
    // the schema the upgrade produced; the run row itself is the legacy one.
    seed_proven_parked_thread(&h.store, "run_1", "run echo", calls).await;
    let user_source = agent24_store::SourceRef::user_input("run_1", now_iso8601());
    h.store
        .tag_run_source("run_1", 0, &user_source, &now_iso8601())
        .await
        .unwrap();
    let approval = seed_approval(
        "apr_1",
        "run_1",
        "tc_internal_1",
        shell_args.as_object().unwrap().clone(),
    );
    insert_parked(&h.store, &approval, "shell_exec").await;
    h.broker
        .resolve("apr_1", decision("approve", None))
        .await
        .unwrap();
    h.manager
        .resume_run("run_1".to_owned(), "apr_1".to_owned())
        .await
        .unwrap();
    let done = wait_terminal(&h.store, "run_1").await;
    assert_resume_refused_before_tools(&h, &provider, &done);
    assert_shell_did_not_run(&h).await;
}

fn assert_resume_refused_before_tools(h: &Harness, provider: &RecordingProvider, done: &Run) {
    let error = done.error.clone().unwrap();
    assert_eq!(done.status, RunStatus::Failed);
    assert_eq!(error.code, SOURCE_UNPROVEN_CODE);
    assert_eq!(error.message, SOURCE_UNPROVEN_MESSAGE);
    assert!(provider.seen.lock().unwrap().is_empty());
    assert!(
        !h.events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event == "run.started")
    );
}

async fn assert_shell_did_not_run(h: &Harness) {
    // The parked call's row is seeded as Running; the shell must not have
    // settled it (kernel resume reads the tool name from this row).
    assert!(
        h.store
            .list_tool_calls("run_1")
            .await
            .unwrap()
            .iter()
            .all(|call| matches!(call.status, ToolCallStatus::Running))
    );
    let thread = h.store.list_run_messages("run_1").await.unwrap();
    assert_eq!(thread.len(), 2);
    assert!(thread.iter().all(|message| message.role != "tool"));
}

#[tokio::test]
async fn legacy_resume_approve_does_not_run_shell_or_call_the_provider() {
    let (h, provider, done) =
        resume_shell_and_explore(false, "approve", None, "must-not-run").await;
    assert_resume_refused_before_tools(&h, &provider, &done);
    assert_shell_did_not_run(&h).await;
}

#[tokio::test]
async fn legacy_resume_deny_does_not_run_shell_or_call_the_provider() {
    let (h, provider, done) =
        resume_shell_and_explore(false, "deny", Some("not this time"), "must-not-run").await;
    assert_resume_refused_before_tools(&h, &provider, &done);
    assert_shell_did_not_run(&h).await;
}

#[tokio::test]
async fn sourced_resume_with_explore_still_completes_and_calls_the_provider() {
    let (h, provider, done) = resume_shell_and_explore(true, "approve", None, "sourced-ok").await;
    assert_eq!(done.status, RunStatus::Completed);
    assert_eq!(done.output.unwrap().text, "pong");
    let seen = provider.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().any(|messages| {
        messages
            .iter()
            .any(|message| message.content.as_deref() == Some("SECRET-TASK-1197"))
    }));
    let thread = h.store.list_run_messages("run_1").await.unwrap();
    assert!(thread.iter().any(|message| {
        message.role == "tool"
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("sourced-ok"))
    }));
    assert!(
        h.events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event == "run.started")
    );
}

const ROLE_TURN_TASK: &str = "SECRET-TASK-G3";

struct ScriptedRecordingProvider {
    recorder: Arc<RecordingProvider>,
    turn: StdMutex<Option<Msg>>,
}

#[async_trait::async_trait]
impl agent24_models::ModelProvider for ScriptedRecordingProvider {
    fn name(&self) -> &str {
        "scripted-recording"
    }

    async fn complete(
        &self,
        req: &agent24_models::CompletionRequest,
        cancel: &CancellationToken,
    ) -> Result<agent24_models::CompletionResponse, agent24_models::ModelError> {
        if let Some(turn) = self.turn.lock().unwrap().take() {
            self.recorder
                .seen
                .lock()
                .unwrap()
                .push(req.messages.clone());
            return Ok(agent24_models::CompletionResponse {
                message: turn,
                usage: Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    cost_usd: 0.0,
                },
                model_id: None,
            });
        }
        agent24_models::ModelProvider::complete(self.recorder.as_ref(), req, cancel).await
    }

    async fn models(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Vec<agent24_protocol::Model>, agent24_models::ModelError> {
        agent24_models::ModelProvider::models(self.recorder.as_ref(), cancel).await
    }
}

fn role_tool_turn(role: &str, marker: &std::path::Path) -> Msg {
    let mut turn = Msg::assistant(
        None,
        vec![
            agent24_models::ToolCallRequest {
                id: "call_shell".to_owned(),
                name: "shell_exec".to_owned(),
                arguments: serde_json::json!({
                    "argv": ["/usr/bin/touch", marker]
                })
                .to_string(),
            },
            agent24_models::ToolCallRequest {
                id: "call_explore".to_owned(),
                name: "explore".to_owned(),
                arguments: serde_json::json!({ "task": ROLE_TURN_TASK }).to_string(),
            },
        ],
    );
    turn.role = role.to_owned();
    turn
}

async fn role_turn_harness(
    workdir: std::path::PathBuf,
    turn: Msg,
) -> (Harness, Arc<RecordingProvider>) {
    let store = Store::open_memory().await.unwrap();
    let events = Arc::new(StdMutex::new(Vec::new()));
    let ev = Arc::clone(&events);
    let emit: Arc<dyn Fn(EventBody) + Send + Sync> = Arc::new(move |body: EventBody| {
        if let Ok(mut v) = ev.lock() {
            v.push(body.wire_type().to_owned());
        }
    });
    let broker = ApprovalBroker::new(store.clone(), Arc::clone(&emit), Duration::from_secs(30));
    let recorder = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let router = Arc::new(ModelRouter::with_defaults(vec![(
        Arc::new(ScriptedRecordingProvider {
            recorder: Arc::clone(&recorder),
            turn: StdMutex::new(Some(turn)),
        }) as Arc<dyn agent24_models::ModelProvider>,
        Tier::Local,
    )]));
    let explorer = crate::subagent::ExplorerSubagent::new(
        Arc::clone(&router),
        Arc::new(ToolRegistry::read_only(workdir.clone())),
    );
    let tools = ToolRegistry::builtin(workdir)
        .with(Arc::new(explorer))
        .with_gate(Arc::new(BrokerGate::new(Arc::clone(&broker))));
    struct FnSink(Arc<dyn Fn(EventBody) + Send + Sync>);
    impl EventSink for FnSink {
        fn emit(&self, body: EventBody) {
            (self.0)(body);
        }
    }
    let manager = RunManager::new(
        store.clone(),
        router,
        Arc::new(tools),
        Arc::new(FnSink(emit)),
        CancellationToken::new(),
    );
    (
        Harness {
            manager,
            broker,
            store,
            events,
        },
        recorder,
    )
}

fn saw_task(provider: &RecordingProvider) -> bool {
    provider.seen.lock().unwrap().iter().any(|messages| {
        messages
            .iter()
            .any(|message| message.content.as_deref() == Some(ROLE_TURN_TASK))
    })
}

async fn finish_role_turn(h: &Harness, run_id: &str) -> Run {
    for _ in 0..200 {
        let run = h.store.get_run(run_id).await.unwrap().unwrap();
        if agent24_core::run_is_terminal(run.status) {
            return run;
        }
        let pending = h
            .store
            .list_approvals(Some(ApprovalStatus::Pending))
            .await
            .unwrap();
        if let Some(approval) = pending.first() {
            h.broker
                .resolve(&approval.id, decision("approve", None))
                .await
                .unwrap();
            return wait_terminal(&h.store, run_id).await;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("run {run_id} neither finished nor parked for approval");
}

async fn drive_role_turn(role: &str) -> (Harness, Arc<RecordingProvider>, tempfile::TempDir, Run) {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("g3-marker");
    let (h, provider) =
        role_turn_harness(dir.path().to_path_buf(), role_tool_turn(role, &marker)).await;
    let run = h.manager.start_run(create()).await.unwrap();
    let done = finish_role_turn(&h, &run.id).await;
    (h, provider, dir, done)
}

#[tokio::test]
async fn user_role_with_tools_stops_before_shell_and_explore() {
    let (h, provider, dir, done) = drive_role_turn("user").await;
    let marker = dir.path().join("g3-marker");
    assert_eq!(done.status, RunStatus::Failed);
    let error = done.error.unwrap();
    assert_eq!(error.code, "provider_error");
    assert_eq!(error.message, PROVIDER_ROLE_MESSAGE);
    assert!(!marker.exists(), "shell_exec must not touch the marker");
    assert!(!saw_task(&provider), "explore must not call the provider");
    assert!(h.store.list_tool_calls(&done.id).await.unwrap().is_empty());
    let thread = h.store.list_run_messages(&done.id).await.unwrap();
    assert_eq!(
        thread
            .iter()
            .map(|message| message.role.as_str())
            .collect::<Vec<_>>(),
        vec!["user"]
    );
}

#[tokio::test]
async fn assistant_role_with_the_same_tools_runs_the_shell() {
    let (h, provider, dir, done) = drive_role_turn("assistant").await;
    let marker = dir.path().join("g3-marker");
    assert_eq!(done.status, RunStatus::Completed);
    assert!(
        marker.exists(),
        "approved shell_exec did not touch the marker"
    );
    assert!(saw_task(&provider), "explore should call the provider");
    assert!(
        h.store
            .list_tool_calls(&done.id)
            .await
            .unwrap()
            .iter()
            .any(|call| call.tool == "shell_exec" && call.status == ToolCallStatus::Completed)
    );
}

#[tokio::test]
async fn user_role_final_answer_is_not_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let (h, _) = role_turn_harness(dir.path().to_path_buf(), Msg::user("not-an-answer")).await;
    let run = h.manager.start_run(create()).await.unwrap();
    let done = finish_role_turn(&h, &run.id).await;
    assert_eq!(done.status, RunStatus::Failed);
    assert_eq!(done.error.unwrap().code, "provider_error");
    let thread = h.store.list_run_messages(&done.id).await.unwrap();
    assert!(
        thread
            .iter()
            .all(|message| message.content.as_deref() != Some("not-an-answer"))
    );
}

#[rustfmt::skip]
async fn assert_bound_recovery_followup_stops(h:&Harness,run:&Run,call:&agent24_models::ToolCallRequest,lease:&str){assert!(matches!(h.manager.run_tool_call(run,call,&CancellationToken::new(),true).await,Err(ParkedCallStop::RecoveryStopped)));let tool_calls:i64=sqlx::query_scalar("SELECT count(*) FROM tool_calls WHERE run_id='run_1'").fetch_one(agent24_store::test_hooks::pool(&h.store)).await.unwrap();assert_eq!(tool_calls,0);sqlx::query("UPDATE runs SET status='running',ended_at=NULL WHERE id='run_1'").execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();sqlx::query("UPDATE workspace_leases SET released_at=NULL WHERE lease_id=?").bind(lease).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();}
/// End-to-end crash recovery (H3): a run parked awaiting approval, its task
/// gone, is resumed once a human answers the restored approval — the parked
/// tool runs and the run completes, all reconstructed from the persisted
/// thread. A parked call that fails once it runs is settled the same way,
/// its error tagged like any result (K1-6b.5f, PR-Daemon #1282 N1). A
/// session run's settled result also records its origin for the session's
/// later turns (K1-6b.5f-2, PR-Daemon #1309 S2).
#[tokio::test]
async fn a_parked_run_resumes_after_its_approval_is_answered() {
    for (argv, answer, session) in [
        (vec!["/bin/echo", "resumed-output"], "resumed-output", None),
        (vec!["/nonexistent/agent24-resume-test"], "spawn", None),
        (
            vec!["/bin/echo", "resumed-output"],
            "resumed-output",
            Some("sess_1"),
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let h = resume_harness(dir.path().to_path_buf()).await;
        if let Some(sid) = session {
            seed_session(&h.store, sid).await;
        }

        // Seed the crash state directly: an awaiting_approval run with NO live
        // task, its thread persisted (user + assistant turn whose trailing
        // tool_call is unanswered), and a matching pending approval. The
        // assistant call's arguments MUST equal the approval payload, or
        // staleness re-validation would (correctly) refuse to run B for A.
        let args = serde_json::json!({ "argv": argv });
        let run = Run {
            id: "run_1".to_owned(),
            session_id: session.map(str::to_owned),
            workspace_id: None,
            status: RunStatus::AwaitingApproval,
            input: RunInput {
                prompt: "run echo".to_owned(),
                workspace_id: None,
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
                knowledge_binding_id: None,
            },
            output: None,
            error: None,
            usage: zero_usage(),
            schedule_id: None,
            created_at: now_iso8601(),
            started_at: Some(now_iso8601()),
            ended_at: None,
        };
        h.store.insert_run(&run).await.unwrap();
        let call = serde_json::json!([{ "id": "call_provider_1", "name": "shell_exec", "arguments": args.to_string() }]);
        seed_proven_parked_thread(&h.store, "run_1", "run echo", call).await;
        let source = agent24_store::SourceRef::user_input("run_1", now_iso8601());
        h.store
            .tag_run_source("run_1", 0, &source, &now_iso8601())
            .await
            .unwrap();
        let approval = Approval {
            id: "apr_1".to_owned(),
            run_id: "run_1".to_owned(),
            tool_call_id: "tc_internal_1".to_owned(),
            kind: "exec".to_owned(),
            summary: "shell_exec".to_owned(),
            payload: args.as_object().unwrap().clone(),
            available_decisions: vec!["approve".to_owned(), "deny".to_owned(), "abort".to_owned()],
            standing_target: None,
            status: ApprovalStatus::Pending,
            decision: None,
            expires_at: agent24_core::util::iso8601_after(Duration::from_secs(30)),
            created_at: now_iso8601(),
            decided_at: None,
        };
        insert_parked(&h.store, &approval, "shell_exec").await;

        // The human answers — no in-memory waiter is woken (the task is gone).
        h.broker
            .resolve("apr_1", decision("approve", None))
            .await
            .unwrap();
        // The daemon resumes the run off the persisted thread.
        h.manager
            .resume_run("run_1".to_owned(), "apr_1".to_owned())
            .await
            .unwrap();

        let done = wait_terminal(&h.store, "run_1").await;
        assert_eq!(done.status, RunStatus::Completed);
        // The parked shell_exec actually ran on resume; its output flowed into
        // the final answer.
        assert!(
            done.output.unwrap().text.contains(answer),
            "the parked tool did not run on resume"
        );
        // The reconstructed thread gained the tool result for the parked call,
        // keyed on the THREAD's provider id (call_provider_1) — NOT the
        // approval's tc_internal_1. Resume works across the two id namespaces.
        let thread = h.store.list_run_messages("run_1").await.unwrap();
        assert!(
            thread
                .iter()
                .any(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("call_provider_1")),
            "no tool result was recorded for the resumed call"
        );
        // K1-6b.5f: the settled call's result is tagged like any other, under
        // the approval's call id.
        let tags = h.store.list_run_source_tags("run_1").await.unwrap();
        let result = tags
            .iter()
            .find(|tag| tag.source.kind == agent24_store::SourceKind::ToolResult)
            .expect("the resumed shell_exec's result is tagged");
        assert_eq!(
            result.source.source_id,
            "tool_result:tool:shell_exec:tc_internal_1"
        );
        assert_eq!(
            result.source.classification,
            agent24_store::SourceClassification::Unknown
        );
        if let Some(sid) = session {
            assert_eq!(
                h.store.session_tool_result_origins(sid).await.unwrap(),
                ["tool:shell_exec"],
                "the resumed result's origin is recorded for the session"
            );
        }
    }
}

/// K1-6b.5d: a run parked on a shell_exec approval when the daemon
/// stopped is restored, approved and resumed. Resume rebuilds the tool
/// context from the run's persisted tags (the session floor is not merged
/// again, #1204 G4), so a Restricted source still refuses the child: it
/// never spawns. The same run with only Ordinary sources spawns, so the
/// fixture would show a leak.
#[tokio::test]
async fn a_restricted_run_resumed_after_a_restart_still_cannot_spawn() {
    for restricted in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("child-ran");
        let h = resume_harness_using(dir.path().to_path_buf(), None, false).await;
        let run = Run {
            id: "run_1".to_owned(),
            session_id: None,
            workspace_id: None,
            status: RunStatus::AwaitingApproval,
            input: RunInput {
                prompt: "touch it".to_owned(),
                workspace_id: None,
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
                knowledge_binding_id: None,
            },
            output: None,
            error: None,
            usage: zero_usage(),
            schedule_id: None,
            created_at: now_iso8601(),
            started_at: Some(now_iso8601()),
            ended_at: None,
        };
        h.store.insert_run(&run).await.unwrap();
        let args = serde_json::json!({ "argv": ["/usr/bin/touch", marker] });
        let call = serde_json::json!([{
            "id": "call_provider_1",
            "name": "shell_exec",
            "arguments": args.to_string()
        }]);
        seed_proven_parked_thread(&h.store, "run_1", "touch it", call).await;
        let user = agent24_store::SourceRef::user_input("run_1", now_iso8601());
        h.store
            .tag_run_source("run_1", 0, &user, &now_iso8601())
            .await
            .unwrap();
        if restricted {
            let mut material =
                agent24_store::SourceRef::selected_material("doc-1", None, now_iso8601());
            material.classification = agent24_store::SourceClassification::Restricted;
            h.store
                .tag_run_source("run_1", 0, &material, &now_iso8601())
                .await
                .unwrap();
        }
        let approval = seed_approval(
            "apr_1",
            "run_1",
            "tc_internal_1",
            args.as_object().unwrap().clone(),
        );
        insert_parked(&h.store, &approval, "shell_exec").await;

        assert_eq!(h.manager.restore_pending_approvals().await.unwrap(), (1, 0));
        h.broker
            .resolve("apr_1", decision("approve", None))
            .await
            .unwrap();
        h.manager
            .resume_run("run_1".to_owned(), "apr_1".to_owned())
            .await
            .unwrap();
        wait_terminal(&h.store, "run_1").await;

        assert_eq!(marker.exists(), !restricted, "restricted: {restricted}");
        if restricted {
            let thread = h.store.list_run_messages("run_1").await.unwrap();
            let result = thread
                .iter()
                .find(|m| m.role == "tool")
                .and_then(|m| m.content.clone())
                .expect("the refused call still gets a tool result");
            assert!(result.contains("outbound"), "{result}");
        }
    }
}

/// Says its approval names no grant the first time, as a module tool
/// resumed after a restart does (K1-6a-G §2.3), then runs.
struct StaleOnceTool {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl agent24_tools::Tool for StaleOnceTool {
    fn info(&self) -> agent24_protocol::ToolInfo {
        agent24_protocol::ToolInfo::new(
            "doc.write",
            "module",
            "writes a document",
            RiskClass::WriteLocal,
        )
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
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Err(ToolError::ApprovalStale)
        } else {
            Ok("rewritten".to_owned())
        }
    }
}

/// A resumed approval that names no grant is asked again, with the run
/// shown (and restorable) as awaiting that answer; then the call runs.
#[tokio::test]
async fn a_resumed_approval_naming_no_grant_is_asked_again_while_awaiting() {
    let dir = tempfile::tempdir().unwrap();
    let tool = Arc::new(StaleOnceTool {
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let h = resume_harness_with(dir.path().to_path_buf(), Some(tool.clone())).await;
    let run = Run {
        id: "run_1".to_owned(),
        session_id: None,
        workspace_id: None,
        status: RunStatus::AwaitingApproval,
        input: RunInput {
            prompt: "write it".to_owned(),
            workspace_id: None,
            model_override: None,
            mode: agent24_protocol::RunMode::Normal,
            knowledge_binding_id: None,
        },
        output: None,
        error: None,
        usage: zero_usage(),
        schedule_id: None,
        created_at: now_iso8601(),
        started_at: Some(now_iso8601()),
        ended_at: None,
    };
    h.store.insert_run(&run).await.unwrap();
    let call =
        serde_json::json!([{ "id": "call_provider_1", "name": "doc.write", "arguments": "{}" }]);
    seed_proven_parked_thread(&h.store, "run_1", "write it", call).await;
    let source = agent24_store::SourceRef::user_input("run_1", now_iso8601());
    h.store
        .tag_run_source("run_1", 0, &source, &now_iso8601())
        .await
        .unwrap();
    let approval = Approval {
        id: "apr_1".to_owned(),
        run_id: "run_1".to_owned(),
        tool_call_id: "tc_internal_1".to_owned(),
        kind: "module".to_owned(),
        summary: "doc.write".to_owned(),
        payload: serde_json::Map::new(),
        available_decisions: vec!["approve".to_owned(), "deny".to_owned(), "abort".to_owned()],
        standing_target: None,
        status: ApprovalStatus::Pending,
        decision: None,
        expires_at: agent24_core::util::iso8601_after(Duration::from_secs(30)),
        created_at: now_iso8601(),
        decided_at: None,
    };
    insert_parked(&h.store, &approval, "doc.write").await;
    h.broker
        .resolve("apr_1", decision("approve", None))
        .await
        .unwrap();
    h.manager
        .resume_run("run_1".to_owned(), "apr_1".to_owned())
        .await
        .unwrap();

    let asked_again = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pending = h
                .store
                .list_approvals(Some(ApprovalStatus::Pending))
                .await
                .unwrap();
            if let Some(again) = pending.into_iter().find(|a| a.id != "apr_1") {
                return again;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the resumed call is asked about again");
    let waiting = h.store.get_run("run_1").await.unwrap().unwrap();
    assert_eq!(waiting.status, RunStatus::AwaitingApproval);
    // #1379 review: had the daemon restarted here, the approval asked
    // again would still restore. It has no `tool_calls` row of its own;
    // it is bound to the parked call's provider id.
    let approved_tool = h.manager.approved_tool(&asked_again).await.unwrap();
    let thread = h.store.list_run_messages("run_1").await.unwrap();
    assert_eq!(
        crate::resume::assess_restore(
            &crate::resume::ApprovedCall {
                payload: &asked_again.payload,
                tool_call_id: &asked_again.tool_call_id,
                tool: approved_tool.as_deref(),
            },
            &asked_again.created_at,
            waiting.status,
            &thread,
            |_| true,
            "",
        ),
        crate::resume::RestoreDecision::Restore {
            tool_call_id: "call_provider_1".to_owned()
        }
    );
    h.broker
        .resolve(&asked_again.id, decision("approve", None))
        .await
        .unwrap();
    let done = wait_terminal(&h.store, "run_1").await;
    assert_eq!(done.status, RunStatus::Completed);
    assert_eq!(tool.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(done.output.unwrap().text.contains("rewritten"));
}

#[tokio::test]
async fn bound_resume_without_fresh_authority_cancels_before_running() {
    const WS: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    const LEASE: &str = "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
    let dir = tempfile::tempdir().unwrap();
    let h = resume_harness(dir.path().to_path_buf()).await;
    let workspace_id = agent24_protocol::WorkspaceId::parse(WS).unwrap();
    let now = workspace_timestamp(now_iso8601());
    let expires = WorkspaceInstant::parse(&now)
        .unwrap()
        .checked_add_workspace_ttl(agent24_store::WorkspaceTtl::new(60_000).unwrap())
        .unwrap();
    sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?,?,1,'/scratch','g1','unix',X'0101010101010101',X'0202020202020202')")
        .bind(WS).bind(&now).bind(expires.as_str())
        .execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
    h.store
        .insert_session(&agent24_protocol::Session {
            id: "sess_1".to_owned(),
            title: "session".to_owned(),
            channel: "desktop".to_owned(),
            workspace_id: Some(workspace_id.clone()),
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .await
        .unwrap();
    let input = RunInput {
        prompt: "run echo".to_owned(),
        workspace_id: Some(workspace_id.clone()),
        model_override: None,
        mode: RunMode::Normal,
        knowledge_binding_id: None,
    };
    sqlx::query("INSERT INTO runs (id,session_id,workspace_id,status,input,usage,created_at,started_at) VALUES ('run_1','sess_1',?,'awaiting_approval',?,?,?,?)")
        .bind(WS)
        .bind(serde_json::to_string(&input).unwrap())
        .bind(serde_json::to_string(&zero_usage()).unwrap())
        .bind(&now)
        .bind(&now)
        .execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
    sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,'g1','run_1','run',?)")
        .bind(LEASE).bind(WS).bind(&now)
        .execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
    h.store
        .append_run_message(
            "run_1",
            "user",
            Some("run echo"),
            &serde_json::json!([]),
            None,
            &now_iso8601(),
        )
        .await
        .unwrap();
    let args = serde_json::json!({ "argv": ["/bin/echo", "must-not-run"] });
    let call = serde_json::json!([{ "id": "call_provider_1", "name": "shell_exec", "arguments": args.to_string() }]);
    h.store
        .append_run_message("run_1", "assistant", None, &call, None, &now_iso8601())
        .await
        .unwrap();
    let approval = seed_approval(
        "apr_1",
        "run_1",
        "tc_internal_1",
        args.as_object().unwrap().clone(),
    );
    insert_parked(&h.store, &approval, "shell_exec").await;
    h.broker
        .resolve("apr_1", decision("approve", None))
        .await
        .unwrap();

    h.manager
        .resume_run("run_1".to_owned(), "apr_1".to_owned())
        .await
        .unwrap();

    assert_eq!(
        wait_terminal(&h.store, "run_1").await.status,
        RunStatus::Cancelled
    );
    assert!(!h.events.lock().unwrap().contains(&"run.started".to_owned()));
    let released: Option<String> =
        sqlx::query_scalar("SELECT released_at FROM workspace_leases WHERE lease_id=?")
            .bind(LEASE)
            .fetch_one(agent24_store::test_hooks::pool(&h.store))
            .await
            .unwrap();
    assert!(released.is_some());
}

#[tokio::test]
#[rustfmt::skip]
async fn bound_settle_authority_loss_after_running_cancels_recovery() {
    const WS:&str="ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5"; const LEASE:&str="wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
    let dir=tempfile::tempdir().unwrap(); let h=resume_harness(dir.path().to_path_buf()).await; let workspace_id=agent24_protocol::WorkspaceId::parse(WS).unwrap(); let now=workspace_timestamp(now_iso8601()); let expires=WorkspaceInstant::parse(&now).unwrap().checked_add_workspace_ttl(agent24_store::WorkspaceTtl::new(60_000).unwrap()).unwrap();
    sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?,?,1,'/scratch','g1','unix',X'0101010101010101',X'0202020202020202')").bind(WS).bind(&now).bind(expires.as_str()).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
    h.store.insert_session(&agent24_protocol::Session{id:"sess_1".into(),title:"session".into(),channel:"desktop".into(),workspace_id:Some(workspace_id.clone()),created_at:now.clone(),updated_at:now.clone()}).await.unwrap();
    let input=RunInput{prompt:"run echo".into(),workspace_id:Some(workspace_id),model_override:None,mode:RunMode::Normal,knowledge_binding_id:None};
    sqlx::query("INSERT INTO runs (id,session_id,workspace_id,status,input,usage,created_at,started_at) VALUES ('run_1','sess_1',?,'running',?,?,?,?)").bind(WS).bind(serde_json::to_string(&input).unwrap()).bind(serde_json::to_string(&zero_usage()).unwrap()).bind(&now).bind(&now).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
    sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,'g1','run_1','run',?)").bind(LEASE).bind(WS).bind(&now).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
    let run=h.store.get_run("run_1").await.unwrap().unwrap(); let payload=serde_json::json!({"argv":["/bin/echo","must-not-run"]}); let mut approval=seed_approval("apr_1","run_1","tc_internal_1",payload.as_object().unwrap().clone()); approval.status=ApprovalStatus::Approved; approval.decision=Some(decision("approve",None)); approval.decided_at=Some(now_iso8601());
    let call=agent24_models::ToolCallRequest{id:"call_provider_1".into(),name:"shell_exec".into(),arguments:payload.to_string()};
    assert_bound_recovery_followup_stops(&h,&run,&call,LEASE).await;
    assert!(matches!(h.manager.settle_parked_call(&run,&approval,&call,&CancellationToken::new()).await,Err(ParkedCallStop::RecoveryStopped)));
    assert_eq!(h.store.get_run("run_1").await.unwrap().unwrap().status,RunStatus::Cancelled);
    let released:Option<String>=sqlx::query_scalar("SELECT released_at FROM workspace_leases WHERE lease_id=?").bind(LEASE).fetch_one(agent24_store::test_hooks::pool(&h.store)).await.unwrap(); assert!(released.is_some()); assert!(h.events.lock().unwrap().contains(&"run.cancelled".to_owned()));
}

#[cfg(unix)]
#[tokio::test]
#[rustfmt::skip]
async fn bound_resume_later_call_authority_loss_stops_once_without_persistence() {
    use std::os::unix::fs::{MetadataExt,PermissionsExt}; const WS:&str="ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5"; const LEASE:&str="wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
    let dir=tempfile::tempdir().unwrap(); let state=tempfile::tempdir().unwrap(); let mut h=resume_harness(dir.path().to_path_buf()).await; let service=Arc::new(WorkspaceService::compose(h.store.clone(),state.path()).unwrap()); Arc::get_mut(&mut h.manager).unwrap().workspace=Some(service);
    let now=workspace_timestamp(now_iso8601()); let expires=WorkspaceInstant::parse(&now).unwrap().checked_add_workspace_ttl(agent24_store::WorkspaceTtl::new(60_000).unwrap()).unwrap(); let locator=format!("{WS}.g1"); let parent=state.path().join("workspace-roots"); let root=parent.join(&locator); std::fs::create_dir(&root).unwrap(); std::fs::set_permissions(&root,std::fs::Permissions::from_mode(0o700)).unwrap(); let pm=std::fs::metadata(&parent).unwrap(); let rm=std::fs::metadata(&root).unwrap();
    sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?,?,1,?,'g1','unix',?,?)").bind(WS).bind(&now).bind(expires.as_str()).bind(root.to_str().unwrap()).bind(rm.dev().to_le_bytes().to_vec()).bind(rm.ino().to_le_bytes().to_vec()).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
    sqlx::query("INSERT INTO workspace_allocations (allocation_id,workspace_id,root_generation,relative_name,parent_identity_kind,parent_unix_device,parent_unix_inode,root_identity_kind,root_unix_device,root_unix_inode,phase,created_at) VALUES ('wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5',?,'g1',?,'unix',?,?,'unix',?,?,'committed',?)").bind(WS).bind(&locator).bind(pm.dev().to_le_bytes().to_vec()).bind(pm.ino().to_le_bytes().to_vec()).bind(rm.dev().to_le_bytes().to_vec()).bind(rm.ino().to_le_bytes().to_vec()).bind(&now).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
    let wid=agent24_protocol::WorkspaceId::parse(WS).unwrap(); h.store.insert_session(&agent24_protocol::Session{id:"sess_1".into(),title:"s".into(),channel:"desktop".into(),workspace_id:Some(wid.clone()),created_at:now.clone(),updated_at:now.clone()}).await.unwrap(); let input=RunInput{prompt:"two".into(),workspace_id:Some(wid),model_override:None,mode:RunMode::Normal,knowledge_binding_id:None}; sqlx::query("INSERT INTO runs (id,session_id,workspace_id,status,input,usage,created_at,started_at) VALUES ('run_1','sess_1',?,'awaiting_approval',?,?,?,?)").bind(WS).bind(serde_json::to_string(&input).unwrap()).bind(serde_json::to_string(&zero_usage()).unwrap()).bind(&now).bind(&now).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap(); sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,'g1','run_1','run',?)").bind(LEASE).bind(WS).bind(&now).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
    let a1=serde_json::json!({"argv":["/bin/echo","first"]}); let a2=serde_json::json!({"argv":["/bin/echo","must-not-run"]}); let calls=serde_json::json!([{"id":"call_provider_1","name":"shell_exec","arguments":a1.to_string()},{"id":"call_provider_2","name":"shell_exec","arguments":a2.to_string()}]); seed_proven_parked_thread(&h.store,"run_1","two",calls).await; let approval=seed_approval("apr_1","run_1","tc_internal_1",a1.as_object().unwrap().clone()); insert_parked(&h.store, &approval, "shell_exec").await; h.broker.resolve("apr_1",decision("approve",None)).await.unwrap();
    let run=h.store.get_run("run_1").await.unwrap().unwrap(); assert!(h.manager.tool_context_for(&run,"preflight").await.is_ok()); sqlx::raw_sql(&format!("CREATE TRIGGER lose_resume_authority AFTER INSERT ON run_messages WHEN NEW.run_id='run_1' AND NEW.role='tool' AND NEW.tool_call_id='call_provider_1' BEGIN UPDATE workspace_leases SET released_at='{now}' WHERE lease_id='{LEASE}'; END;")).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap(); h.manager.resume_run("run_1".into(),"apr_1".into()).await.unwrap(); assert_eq!(wait_terminal(&h.store,"run_1").await.status,RunStatus::Cancelled);
    assert_eq!(h.store.list_tool_calls("run_1").await.unwrap().iter().map(|c|c.id.as_str()).collect::<Vec<_>>(),["tc_internal_1"]); let thread=h.store.list_run_messages("run_1").await.unwrap(); assert!(thread.iter().any(|m|m.role=="tool"&&m.tool_call_id.as_deref()==Some("call_provider_1"))); assert!(!thread.iter().any(|m|m.role=="tool"&&m.tool_call_id.as_deref()==Some("call_provider_2"))); assert_eq!(h.events.lock().unwrap().iter().filter(|e|e.as_str()=="run.cancelled").count(),1);
}

#[tokio::test]
async fn restored_exec_session_approval_does_not_mint_a_grant() {
    let dir = tempfile::tempdir().unwrap();
    let h = resume_harness(dir.path().to_path_buf()).await;
    let now = now_iso8601();
    h.store
        .insert_session(&agent24_protocol::Session {
            id: "sess_1".to_owned(),
            title: "session".to_owned(),
            channel: "desktop".to_owned(),
            workspace_id: None,
            created_at: now.clone(),
            updated_at: now,
        })
        .await
        .unwrap();
    let args = serde_json::json!({ "argv": ["/bin/echo", "resumed-output"] });
    let mut run = run_in("run_1", RunStatus::AwaitingApproval);
    run.session_id = Some("sess_1".to_owned());
    h.store.insert_run(&run).await.unwrap();
    let call = serde_json::json!([{ "id": "call_provider_1", "name": "shell_exec", "arguments": args.to_string() }]);
    seed_proven_parked_thread(&h.store, "run_1", "run echo", call).await;
    let mut approval = seed_approval(
        "apr_1",
        "run_1",
        "tc_internal_1",
        args.as_object().unwrap().clone(),
    );
    approval
        .available_decisions
        .insert(1, "approve_for_session".to_owned());
    insert_parked(&h.store, &approval, "shell_exec").await;

    h.broker
        .resolve("apr_1", decision("approve_for_session", None))
        .await
        .unwrap();
    h.manager
        .resume_run("run_1".to_owned(), "apr_1".to_owned())
        .await
        .unwrap();
    assert_eq!(
        wait_terminal(&h.store, "run_1").await.status,
        RunStatus::Completed
    );

    let mut probe = run_in("probe", RunStatus::Running);
    probe.session_id = Some("sess_1".to_owned());
    h.store.insert_run(&probe).await.unwrap();
    let broker = h.broker.clone();
    let cancel = CancellationToken::new();
    let request_cancel = cancel.clone();
    let waiter = tokio::spawn(async move {
        broker
            .request(
                ApprovalRequest {
                    run_id: "probe",
                    session_id: Some("sess_1"),
                    schedule_id: None,
                    tool_call_id: "tc_probe",
                    tool: "shell_exec",
                    source: "builtin",
                    kind: "exec",
                    risk: RiskClass::Exec,
                    per_call_approval: false,
                    standing_target: None,
                    summary: "probe".to_owned(),
                    payload: serde_json::Map::new(),
                    workspace_root: None,
                    captured_write_directory: None,
                },
                &request_cancel,
            )
            .await
    });
    let mut saw_pending = false;
    for _ in 0..50 {
        let rows = h.store.list_approvals(None).await.unwrap();
        if rows
            .iter()
            .any(|row| row.run_id == "probe" && row.status == ApprovalStatus::Pending)
        {
            saw_pending = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        saw_pending,
        "restored exec session decision must not skip the next ask"
    );
    cancel.cancel();
    assert!(matches!(waiter.await.unwrap(), Verdict::Aborted(_)));
    assert!(!h.store.list_audit().await.unwrap().iter().any(|entry| {
        entry.action == "approval.auto_granted" && entry.detail["run_id"] == "probe"
    }));
    assert!(!h.store.list_audit().await.unwrap().iter().any(|entry| {
        entry.action == "approval.session_grant" && entry.detail["tool"] == "shell_exec"
    }));
}

/// A denied restored approval does not execute the tool: the reason goes
/// back to the model and the run still completes (denial is not failure).
#[tokio::test]
async fn a_denied_parked_run_resumes_without_executing() {
    let dir = tempfile::tempdir().unwrap();
    let h = resume_harness(dir.path().to_path_buf()).await;
    let args = serde_json::json!({ "argv": ["/bin/echo", "should-not-run"] });
    let run = Run {
        id: "run_1".to_owned(),
        session_id: None,
        workspace_id: None,
        status: RunStatus::AwaitingApproval,
        input: RunInput {
            prompt: "run echo".to_owned(),
            workspace_id: None,
            model_override: None,
            mode: agent24_protocol::RunMode::Normal,
            knowledge_binding_id: None,
        },
        output: None,
        error: None,
        usage: zero_usage(),
        schedule_id: None,
        created_at: now_iso8601(),
        started_at: Some(now_iso8601()),
        ended_at: None,
    };
    h.store.insert_run(&run).await.unwrap();
    let call = serde_json::json!([{ "id": "call_provider_1", "name": "shell_exec", "arguments": args.to_string() }]);
    seed_proven_parked_thread(&h.store, "run_1", "run echo", call).await;
    let approval = Approval {
        id: "apr_1".to_owned(),
        run_id: "run_1".to_owned(),
        tool_call_id: "tc_internal_1".to_owned(),
        kind: "exec".to_owned(),
        summary: "shell_exec".to_owned(),
        payload: args.as_object().unwrap().clone(),
        available_decisions: vec!["approve".to_owned(), "deny".to_owned(), "abort".to_owned()],
        standing_target: None,
        status: ApprovalStatus::Pending,
        decision: None,
        expires_at: agent24_core::util::iso8601_after(Duration::from_secs(30)),
        created_at: now_iso8601(),
        decided_at: None,
    };
    insert_parked(&h.store, &approval, "shell_exec").await;

    h.broker
        .resolve("apr_1", decision("deny", Some("not this time")))
        .await
        .unwrap();
    h.manager
        .resume_run("run_1".to_owned(), "apr_1".to_owned())
        .await
        .unwrap();

    let done = wait_terminal(&h.store, "run_1").await;
    assert_eq!(done.status, RunStatus::Completed);
    // The denial reason reached the model (echoed by the exhausted script),
    // and the tool never produced its output.
    let text = done.output.unwrap().text;
    assert!(
        text.contains("not this time"),
        "denial reason not fed back: {text}"
    );
    assert!(
        !text.contains("should-not-run"),
        "denied tool ran anyway: {text}"
    );
}

/// The parked thread a current run would have stored: user prompt and the
/// assistant tool call, each with a present origin and no recall ref.
async fn seed_proven_parked_thread(
    store: &Store,
    run_id: &str,
    prompt: &str,
    call: serde_json::Value,
) {
    store
        .append_originated_run_messages(
            run_id,
            &[
                OriginatedRunMessage {
                    message: agent24_store::PendingRunMessage {
                        role: "user".into(),
                        content: Some(prompt.into()),
                        tool_calls: serde_json::json!([]),
                        tool_call_id: None,
                    },
                    origin: MessageOrigin::UserPrompt,
                },
                OriginatedRunMessage {
                    message: agent24_store::PendingRunMessage {
                        role: "assistant".into(),
                        content: None,
                        tool_calls: call,
                        tool_call_id: None,
                    },
                    origin: MessageOrigin::Assistant,
                },
            ],
            &now_iso8601(),
        )
        .await
        .unwrap();
}

/// A run row in a given status, for seeding crash/parked state directly.
fn run_in(id: &str, status: RunStatus) -> Run {
    Run {
        id: id.to_owned(),
        session_id: None,
        workspace_id: None,
        status,
        input: RunInput {
            prompt: "go".to_owned(),
            workspace_id: None,
            model_override: None,
            mode: agent24_protocol::RunMode::Normal,
            knowledge_binding_id: None,
        },
        output: None,
        error: None,
        usage: zero_usage(),
        schedule_id: None,
        created_at: now_iso8601(),
        started_at: Some(now_iso8601()),
        ended_at: None,
    }
}

/// A pending approval row.
/// Inserts `approval` after the `tool_calls` row of the `tool` call it was
/// asked for, in the order `run_tool_call` writes them: a restore checks
/// the parked call against that row's tool.
async fn insert_parked(store: &Store, approval: &Approval, tool: &str) {
    store
        .insert_tool_call(&ToolCall {
            id: approval.tool_call_id.clone(),
            run_id: approval.run_id.clone(),
            tool: tool.to_owned(),
            input: approval.payload.clone(),
            status: ToolCallStatus::Running,
            output_summary: None,
            started_at: now_iso8601(),
            ended_at: None,
        })
        .await
        .unwrap();
    store.insert_approval(approval).await.unwrap();
}

fn seed_approval(
    id: &str,
    run_id: &str,
    tool_call_id: &str,
    payload: serde_json::Map<String, serde_json::Value>,
) -> Approval {
    Approval {
        id: id.to_owned(),
        run_id: run_id.to_owned(),
        tool_call_id: tool_call_id.to_owned(),
        kind: "exec".to_owned(),
        summary: "shell_exec".to_owned(),
        payload,
        available_decisions: vec!["approve".to_owned(), "deny".to_owned(), "abort".to_owned()],
        standing_target: None,
        status: ApprovalStatus::Pending,
        decision: None,
        expires_at: agent24_core::util::iso8601_after(Duration::from_secs(30)),
        created_at: now_iso8601(),
        decided_at: None,
    }
}

#[test]
fn restored_decision_requires_offered_kind_and_matching_status() {
    let mut approval = seed_approval("apr", "run", "tc", serde_json::Map::new());
    approval.status = ApprovalStatus::Approved;
    let session = decision("approve_for_session", None);
    assert!(!restored_decision_is_consistent(&approval, &session));

    approval
        .available_decisions
        .push("approve_for_session".to_owned());
    assert!(restored_decision_is_consistent(&approval, &session));

    approval.status = ApprovalStatus::Denied;
    assert!(!restored_decision_is_consistent(&approval, &session));
}

#[tokio::test]
async fn startup_timeout_prevents_expired_approval_rebroadcast() {
    let dir = tempfile::tempdir().unwrap();
    let h = resume_harness(dir.path().to_path_buf()).await;
    h.store
        .insert_run(&run_in("run_expired", RunStatus::AwaitingApproval))
        .await
        .unwrap();
    let args = serde_json::json!({ "argv": ["/bin/echo", "must-not-run"] });
    h.store
        .append_run_message(
            "run_expired",
            "user",
            Some("go"),
            &serde_json::json!([]),
            None,
            &now_iso8601(),
        )
        .await
        .unwrap();
    let calls = serde_json::json!([{
        "id": "call_provider_expired",
        "name": "shell_exec",
        "arguments": args.to_string()
    }]);
    h.store
        .append_run_message(
            "run_expired",
            "assistant",
            None,
            &calls,
            None,
            &now_iso8601(),
        )
        .await
        .unwrap();
    let mut approval = seed_approval(
        "apr_expired",
        "run_expired",
        "tc_expired",
        args.as_object().unwrap().clone(),
    );
    approval.expires_at = "2020-01-01T00:00:00Z".to_owned();
    insert_parked(&h.store, &approval, "shell_exec").await;

    // Prove this fixture would be re-broadcast if startup restore ran
    // before expiry reconciliation; otherwise the regression is vacuous.
    assert_eq!(h.manager.restore_pending_approvals().await.unwrap(), (1, 0));
    h.events.lock().unwrap().clear();

    assert_eq!(
        h.broker
            .timeout_expired("2026-10-01T00:00:00Z")
            .await
            .unwrap(),
        1
    );
    assert_eq!(h.manager.restore_pending_approvals().await.unwrap(), (0, 0));
    assert_eq!(
        h.store
            .get_approval("apr_expired")
            .await
            .unwrap()
            .unwrap()
            .status,
        ApprovalStatus::TimedOut
    );
    let seen = h.events.lock().unwrap().clone();
    assert!(seen.iter().any(|event| event == "approval.resolved"));
    assert!(!seen.iter().any(|event| event == "approval.required"));
}

#[tokio::test]
async fn resume_rejects_approval_owned_by_another_run() {
    let dir = tempfile::tempdir().unwrap();
    let h = resume_harness(dir.path().to_path_buf()).await;
    h.store
        .insert_run(&run_in("run_a", RunStatus::AwaitingApproval))
        .await
        .unwrap();
    h.store
        .insert_run(&run_in("run_b", RunStatus::AwaitingApproval))
        .await
        .unwrap();
    let approval = seed_approval("apr_a", "run_a", "tc_a", serde_json::Map::new());
    h.store.insert_approval(&approval).await.unwrap();

    assert!(matches!(
        h.manager
            .resume_run("run_b".to_owned(), "apr_a".to_owned())
            .await,
        Err(AgentError::Store(StoreError::Conflict(message)))
            if message == "approval does not belong to run"
    ));
    assert_eq!(
        h.store.get_run("run_b").await.unwrap().unwrap().status,
        RunStatus::AwaitingApproval
    );
    assert!(!h.manager.cancels.lock().await.contains_key("run_b"));
}

/// Startup restore sweep (H3): a restorable pending approval is re-broadcast
/// and kept pending; a non-restorable one (here: its run already completed)
/// is aborted fail-closed. Replaces the old abort-everything sweep.
#[tokio::test]
async fn restore_sweep_keeps_the_restorable_and_aborts_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let h = resume_harness(dir.path().to_path_buf()).await;

    // Restorable: an awaiting_approval run with a consistent parked thread.
    let args = serde_json::json!({ "argv": ["/bin/echo", "hi"] });
    h.store
        .insert_run(&run_in("run_ok", RunStatus::AwaitingApproval))
        .await
        .unwrap();
    h.store
        .append_run_message(
            "run_ok",
            "user",
            Some("go"),
            &serde_json::json!([]),
            None,
            &now_iso8601(),
        )
        .await
        .unwrap();
    let call = serde_json::json!([{ "id": "call_provider_1", "name": "shell_exec", "arguments": args.to_string() }]);
    h.store
        .append_run_message("run_ok", "assistant", None, &call, None, &now_iso8601())
        .await
        .unwrap();
    insert_parked(
        &h.store,
        &seed_approval(
            "apr_ok",
            "run_ok",
            // tc_ namespace — deliberately DIFFERENT from the thread's
            // provider id (call_provider_1); correspondence is by payload.
            "tc_internal_1",
            args.as_object().unwrap().clone(),
        ),
        "shell_exec",
    )
    .await;

    // Non-restorable: a pending approval whose run already COMPLETED — the
    // status gate in plan_resume refuses it, so the sweep aborts it.
    h.store
        .insert_run(&run_in("run_done", RunStatus::Completed))
        .await
        .unwrap();
    h.store
        .insert_approval(&seed_approval(
            "apr_done",
            "run_done",
            "call_x",
            serde_json::Map::new(),
        ))
        .await
        .unwrap();

    let (restored, aborted) = h.manager.restore_pending_approvals().await.unwrap();
    assert_eq!((restored, aborted), (1, 1));
    // The restorable one is still pending and was re-announced.
    assert_eq!(
        h.store
            .get_approval("apr_ok")
            .await
            .unwrap()
            .unwrap()
            .status,
        ApprovalStatus::Pending
    );
    assert!(
        h.events
            .lock()
            .unwrap()
            .contains(&"approval.required".to_owned())
    );
    // The non-restorable one is aborted.
    assert_eq!(
        h.store
            .get_approval("apr_done")
            .await
            .unwrap()
            .unwrap()
            .status,
        ApprovalStatus::Aborted
    );
}

#[tokio::test]
async fn timed_out_recovery_cancels_only_tokenless_parked_runs_without_start_event() {
    let dir = tempfile::tempdir().unwrap();
    let h = resume_harness(dir.path().to_path_buf()).await;
    for id in ["run_tokenless", "run_live"] {
        h.store
            .insert_run(&run_in(id, RunStatus::AwaitingApproval))
            .await
            .unwrap();
        let mut approval = seed_approval(
            &format!("apr_{id}"),
            id,
            &format!("tc_{id}"),
            serde_json::Map::new(),
        );
        approval.status = ApprovalStatus::TimedOut;
        approval.decided_at = Some(now_iso8601());
        h.store.insert_approval(&approval).await.unwrap();
    }
    h.manager
        .cancels
        .lock()
        .await
        .insert("run_live".to_owned(), CancellationToken::new());

    assert_eq!(
        h.manager.recover_timed_out_approval_runs().await.unwrap(),
        1
    );
    assert_eq!(
        h.store
            .get_run("run_tokenless")
            .await
            .unwrap()
            .unwrap()
            .status,
        RunStatus::Cancelled
    );
    assert_eq!(
        h.store.get_run("run_live").await.unwrap().unwrap().status,
        RunStatus::AwaitingApproval
    );
    let seen = h.events.lock().unwrap().clone();
    assert_eq!(
        seen.iter()
            .filter(|event| event.as_str() == "run.cancelled")
            .count(),
        1
    );
    assert!(!seen.iter().any(|event| event.as_str() == "run.started"));
}

#[tokio::test]
async fn timed_out_inbound_approval_marks_the_inbound_row_done() {
    let dir = tempfile::tempdir().unwrap();
    let h = resume_harness(dir.path().to_path_buf()).await;
    h.store
        .insert_run(&run_in("run_inbound_timeout", RunStatus::AwaitingApproval))
        .await
        .unwrap();
    let mut approval = seed_approval(
        "apr_inbound_timeout",
        "run_inbound_timeout",
        "tc_inbound_timeout",
        serde_json::Map::new(),
    );
    approval.status = ApprovalStatus::TimedOut;
    approval.decided_at = Some(now_iso8601());
    h.store.insert_approval(&approval).await.unwrap();
    let event = agent24_store::NewInboundEvent {
        id: "hyphae:timeout".to_owned(),
        source: "hyphae".to_owned(),
        category: "direct".to_owned(),
        recipient_identity: None,
        thread_key: None,
        actor: Some("npub1".to_owned()),
        human: true,
        addressed: "direct".to_owned(),
        at: "2026-11-11T00:00:00Z".to_owned(),
        received_at: "2026-11-11T00:00:01Z".to_owned(),
        text: Some("parked".to_owned()),
        meta: None,
        disposition: "explicit".to_owned(),
        disposition_reason: None,
        policy_version: Some("p".to_owned()),
        run_state: Some("started".to_owned()),
    };
    assert_eq!(
        h.store.insert_inbound_event(&event).await.unwrap(),
        agent24_store::InboundInsert::Inserted
    );
    h.store
        .insert_inbound_run("run_inbound_timeout", "hyphae:timeout")
        .await
        .unwrap();

    // A fresh manager: the in-process inbound map from the previous
    // process is gone. Recovery still has to release the inbound row.
    assert_eq!(
        h.manager.recover_timed_out_approval_runs().await.unwrap(),
        1
    );
    assert_eq!(
        h.store
            .get_run("run_inbound_timeout")
            .await
            .unwrap()
            .unwrap()
            .status,
        RunStatus::Cancelled
    );
    assert_eq!(
        h.store
            .get_inbound_event("hyphae:timeout")
            .await
            .unwrap()
            .unwrap()
            .run_state
            .as_deref(),
        Some("done")
    );
}

#[tokio::test]
async fn timed_out_recovery_retries_a_failed_cancel_on_the_next_scan() {
    let dir = tempfile::tempdir().unwrap();
    let h = resume_harness(dir.path().to_path_buf()).await;
    h.store
        .insert_run(&run_in("run_retry", RunStatus::AwaitingApproval))
        .await
        .unwrap();
    let mut approval = seed_approval("apr_retry", "run_retry", "tc_retry", serde_json::Map::new());
    approval.status = ApprovalStatus::TimedOut;
    approval.decided_at = Some(now_iso8601());
    h.store.insert_approval(&approval).await.unwrap();
    sqlx::raw_sql(
        "CREATE TRIGGER block_retry_cancel BEFORE UPDATE OF status ON runs
         WHEN OLD.id='run_retry' AND NEW.status='cancelled'
         BEGIN SELECT RAISE(ABORT, 'blocked'); END;",
    )
    .execute(agent24_store::test_hooks::pool(&h.store))
    .await
    .unwrap();

    assert_eq!(
        h.manager.recover_timed_out_approval_runs().await.unwrap(),
        0
    );
    assert_eq!(
        h.store.get_run("run_retry").await.unwrap().unwrap().status,
        RunStatus::AwaitingApproval
    );
    assert_eq!(
        h.store.timed_out_approval_recovery_run_ids().await.unwrap(),
        vec!["run_retry".to_owned()]
    );

    sqlx::query("DROP TRIGGER block_retry_cancel")
        .execute(agent24_store::test_hooks::pool(&h.store))
        .await
        .unwrap();
    assert_eq!(
        h.manager.recover_timed_out_approval_runs().await.unwrap(),
        1
    );
    assert_eq!(
        h.store.get_run("run_retry").await.unwrap().unwrap().status,
        RunStatus::Cancelled
    );
}

#[tokio::test]
async fn restore_sweep_aborts_bound_approval_without_fresh_authority() {
    const WS: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    const LEASE: &str = "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
    let dir = tempfile::tempdir().unwrap();
    let h = resume_harness(dir.path().to_path_buf()).await;
    let workspace_id = agent24_protocol::WorkspaceId::parse(WS).unwrap();
    let now = workspace_timestamp(now_iso8601());
    let expires = WorkspaceInstant::parse(&now)
        .unwrap()
        .checked_add_workspace_ttl(agent24_store::WorkspaceTtl::new(60_000).unwrap())
        .unwrap();
    sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?,?,1,'/scratch','g1','unix',X'0101010101010101',X'0202020202020202')")
        .bind(WS).bind(&now).bind(expires.as_str())
        .execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
    h.store
        .insert_session(&agent24_protocol::Session {
            id: "sess_1".to_owned(),
            title: "session".to_owned(),
            channel: "desktop".to_owned(),
            workspace_id: Some(workspace_id.clone()),
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .await
        .unwrap();
    let input = RunInput {
        prompt: "run echo".to_owned(),
        workspace_id: Some(workspace_id),
        model_override: None,
        mode: RunMode::Normal,
        knowledge_binding_id: None,
    };
    sqlx::query("INSERT INTO runs (id,session_id,workspace_id,status,input,usage,created_at,started_at) VALUES ('run_1','sess_1',?,'awaiting_approval',?,?,?,?)")
        .bind(WS).bind(serde_json::to_string(&input).unwrap())
        .bind(serde_json::to_string(&zero_usage()).unwrap())
        .bind(&now).bind(&now)
        .execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
    sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,'g1','run_1','run',?)")
        .bind(LEASE).bind(WS).bind(&now)
        .execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
    h.store
        .append_run_message(
            "run_1",
            "user",
            Some("run echo"),
            &serde_json::json!([]),
            None,
            &now_iso8601(),
        )
        .await
        .unwrap();
    let args = serde_json::json!({ "argv": ["/bin/echo", "must-not-run"] });
    let call = serde_json::json!([{ "id": "call_provider_1", "name": "shell_exec", "arguments": args.to_string() }]);
    h.store
        .append_run_message("run_1", "assistant", None, &call, None, &now_iso8601())
        .await
        .unwrap();
    insert_parked(
        &h.store,
        &seed_approval(
            "apr_1",
            "run_1",
            "tc_internal_1",
            args.as_object().unwrap().clone(),
        ),
        "shell_exec",
    )
    .await;

    assert_eq!(h.manager.restore_pending_approvals().await.unwrap(), (0, 1));
    assert_eq!(
        h.store.get_approval("apr_1").await.unwrap().unwrap().status,
        ApprovalStatus::Aborted
    );
    assert_eq!(
        h.store.get_run("run_1").await.unwrap().unwrap().status,
        RunStatus::AwaitingApproval
    );
    assert!(
        !h.events
            .lock()
            .unwrap()
            .contains(&"approval.required".to_owned())
    );

    let ended_at = WorkspaceInstant::parse(&workspace_timestamp(now_iso8601())).unwrap();
    assert_eq!(
        h.store
            .sweep_workspace_orphan_runs(&ended_at)
            .await
            .unwrap(),
        agent24_store::WorkspaceOrphanSweep { released_leases: 1 }
    );
    assert_eq!(
        h.store.get_run("run_1").await.unwrap().unwrap().status,
        RunStatus::Cancelled
    );
    let released: Option<String> =
        sqlx::query_scalar("SELECT released_at FROM workspace_leases WHERE lease_id=?")
            .bind(LEASE)
            .fetch_one(agent24_store::test_hooks::pool(&h.store))
            .await
            .unwrap();
    assert!(released.is_some());
}

#[tokio::test]
async fn restore_sweep_propagates_storage_failure() {
    let dir = tempfile::tempdir().unwrap();
    let h = resume_harness(dir.path().to_path_buf()).await;
    sqlx::query("DROP TABLE approvals")
        .execute(agent24_store::test_hooks::pool(&h.store))
        .await
        .unwrap();
    assert!(matches!(
        h.manager.restore_pending_approvals().await,
        Err(AgentError::Store(_))
    ));

    const WS: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    let dir = tempfile::tempdir().unwrap();
    let h = resume_harness(dir.path().to_path_buf()).await;
    let now = workspace_timestamp(now_iso8601());
    let expires = WorkspaceInstant::parse(&now)
        .unwrap()
        .checked_add_workspace_ttl(agent24_store::WorkspaceTtl::new(60_000).unwrap())
        .unwrap();
    let input = RunInput {
        prompt: "go".into(),
        workspace_id: Some(agent24_protocol::WorkspaceId::parse(WS).unwrap()),
        model_override: None,
        mode: RunMode::Normal,
        knowledge_binding_id: None,
    };
    sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?,?,1,'/scratch','g1','unix',X'0101010101010101',X'0202020202020202')")
        .bind(WS).bind(&now).bind(expires.as_str()).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
    sqlx::query("INSERT INTO runs (id,workspace_id,status,input,usage,created_at) VALUES ('run_db',?,'awaiting_approval',?,?,?)")
        .bind(WS).bind(serde_json::to_string(&input).unwrap()).bind(serde_json::to_string(&zero_usage()).unwrap()).bind(&now)
        .execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
    h.store
        .append_run_message(
            "run_db",
            "user",
            Some("go"),
            &serde_json::json!([]),
            None,
            &now_iso8601(),
        )
        .await
        .unwrap();
    let args = serde_json::json!({"argv":["/bin/echo","hi"]});
    let calls = serde_json::json!([{"id":"call_provider_1","name":"shell_exec","arguments":args.to_string()}]);
    h.store
        .append_run_message("run_db", "assistant", None, &calls, None, &now_iso8601())
        .await
        .unwrap();
    insert_parked(
        &h.store,
        &seed_approval(
            "apr_db",
            "run_db",
            "tc_internal_1",
            args.as_object().unwrap().clone(),
        ),
        "shell_exec",
    )
    .await;
    sqlx::query("DROP TABLE workspace_leases")
        .execute(agent24_store::test_hooks::pool(&h.store))
        .await
        .unwrap();
    assert!(matches!(
        h.manager.restore_pending_approvals().await,
        Err(AgentError::Workspace(WorkspaceStoreError::Database))
    ));
    assert_eq!(
        h.store
            .get_approval("apr_db")
            .await
            .unwrap()
            .unwrap()
            .status,
        ApprovalStatus::Pending
    );
}

#[tokio::test]
async fn denied_approval_feeds_the_reason_back_to_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let h = harness(dir.path().to_path_buf()).await;
    let run = h.manager.start_run(create()).await.unwrap();
    let id = wait_pending(&h.store).await;
    h.broker
        .resolve(&id, decision("deny", Some("not on my machine")))
        .await
        .unwrap();
    let done = wait_terminal(&h.store, &run.id).await;
    // Run continues: the scripted provider echoes the tool result
    assert_eq!(done.status, RunStatus::Completed);
    assert!(done.output.unwrap().text.contains("not on my machine"));
    let calls = h.store.list_tool_calls(&run.id).await.unwrap();
    assert_eq!(calls[0].status, ToolCallStatus::Denied);
}

#[tokio::test]
async fn abort_decision_cancels_the_whole_run() {
    let dir = tempfile::tempdir().unwrap();
    let h = harness(dir.path().to_path_buf()).await;
    let run = h.manager.start_run(create()).await.unwrap();
    let id = wait_pending(&h.store).await;
    h.broker
        .resolve(&id, decision("abort", None))
        .await
        .unwrap();
    let done = wait_terminal(&h.store, &run.id).await;
    assert_eq!(done.status, RunStatus::Cancelled);
    let calls = h.store.list_tool_calls(&run.id).await.unwrap();
    assert_eq!(calls[0].status, ToolCallStatus::Denied);
    let seen = h.events.lock().unwrap().clone();
    assert!(seen.contains(&"run.cancelled".to_owned()));
}

#[tokio::test]
async fn cancelling_the_run_aborts_its_pending_approval() {
    let dir = tempfile::tempdir().unwrap();
    let h = harness(dir.path().to_path_buf()).await;
    let run = h.manager.start_run(create()).await.unwrap();
    let id = wait_pending(&h.store).await;
    h.manager.cancel_run(&run.id).await.unwrap();
    let done = wait_terminal(&h.store, &run.id).await;
    assert_eq!(done.status, RunStatus::Cancelled);
    for _ in 0..100 {
        let a = h.store.get_approval(&id).await.unwrap().unwrap();
        if a.status != ApprovalStatus::Pending {
            assert_eq!(a.status, ApprovalStatus::Aborted);
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("approval never left pending after run cancel");
}
