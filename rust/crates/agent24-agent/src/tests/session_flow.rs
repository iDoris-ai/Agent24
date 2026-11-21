/// Run one prompt in a session and wait for it to reach a terminal state.
async fn run_in_session(
    manager: &Arc<RunManager>,
    store: &Store,
    session_id: &str,
    prompt: &str,
) -> Run {
    let run = manager
        .start_run(RunCreate {
            knowledge_query: None,
            idoris_headroom_confirmed: false,
            session_id: Some(session_id.to_owned()),
            workspace_id: None,
            prompt: prompt.to_owned(),
            model_override: None,
            mode: agent24_protocol::RunMode::Normal,
            knowledge_binding_id: None,
            skill_id: None,
        })
        .await
        .unwrap();
    for _ in 0..200 {
        let current = store.get_run(&run.id).await.unwrap().unwrap();
        if current.status != RunStatus::Running && current.status != RunStatus::Queued {
            return current;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("run did not finish");
}

// ── K1-6b.4b: memory write/recall classification (ADR-K1-02 §2.1.1) ──

const TEAL: &str = "my favourite colour is teal";

/// Seed one explicit-remember assertion for the test owner, optionally
/// with a stored classification (`None` = a legacy NULL `source_ref`).
async fn seed_memory(
    kv: &KvStore,
    id: &str,
    classification: Option<agent24_store::SourceClassification>,
) {
    use agent24_memory::event::Scope;
    use agent24_memory::writer::{Candidate, MemoryWriter};
    let mut candidate = Candidate::new(
        id,
        Scope::owner("test-owner"),
        "user",
        "said_to_remember",
        serde_json::json!(TEAL),
        Origin {
            source: "test".into(),
            trust: Trust::UserSaid,
        },
    )
    .with_evidence(vec![format!("ev-{id}")]);
    if let Some(classification) = classification {
        candidate = candidate.with_source_ref(
            agent24_store::AssertionSourceRef::from_sources([(
                "seed".to_owned(),
                classification,
            )])
            .to_value(),
        );
    }
    kv.write_gate()
        .propose(vec![candidate.remember()])
        .await
        .unwrap();
}

/// The stored classification of the owner's current assertion whose
/// object is `text`.
async fn stored_classification(
    kv: &KvStore,
    text: &str,
) -> agent24_store::SourceClassification {
    use agent24_memory::assertion::{AssertionStore, BeliefQuery};
    let beliefs = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner("test-owner"))
        .await
        .unwrap();
    let belief = beliefs
        .iter()
        .find(|belief| belief.object == serde_json::json!(text))
        .unwrap_or_else(|| panic!("no assertion {text:?} in {beliefs:?}"));
    agent24_store::AssertionSourceRef::classification_of(belief.source_ref.as_ref())
}

#[tokio::test]
async fn a_recalled_fact_tags_the_run_with_its_stored_classification() {
    use agent24_store::SourceClassification::{Ordinary, Restricted, Unknown};
    // (stored classification, tag classification, process egress allowed)
    for (stored, expected, egress_allowed) in [
        (Some(Ordinary), Ordinary, true),
        (Some(Restricted), Restricted, false),
        (None, Unknown, false),
    ] {
        let provider = Arc::new(RecordingProvider {
            seen: StdMutex::new(vec![]),
        });
        let (manager, store, kv) =
            manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
        seed_session(&store, "sess_recall").await;
        seed_memory(&kv, "a-teal", stored).await;

        let run = run_in_session(&manager, &store, "sess_recall", "what colour is teal").await;
        let seen = provider.seen.lock().unwrap().clone();
        assert!(
            seen.iter()
                .flatten()
                .any(|msg| msg.content.as_deref().is_some_and(|c| c.contains(TEAL))),
            "the fact must actually be recalled for this case to mean anything"
        );
        let snapshot = store.run_policy_snapshot(&run.id).await.unwrap();
        let tag = snapshot
            .sources
            .iter()
            .find(|source| source.source_id == "memory_assertion:a-teal")
            .unwrap_or_else(|| panic!("no recall tag in {:?}", snapshot.sources));
        assert_eq!(tag.classification, expected, "{stored:?}");
        assert_eq!(tag.kind, agent24_store::SourceKind::MemoryRecall);

        let ctx = manager.tool_context_for(&run, "tc").await.unwrap();
        let allowed = ctx
            .authorize_egress_if_restricted(
                agent24_domain::EgressPurpose::ProcessExecution,
                agent24_domain::EgressDestination::unknown(),
            )
            .await
            .is_ok();
        assert_eq!(allowed, egress_allowed, "{stored:?}");
    }
}

#[tokio::test]
async fn a_remembered_fact_carries_the_strictest_source_of_its_run() {
    use agent24_store::SourceClassification::{Ordinary, Restricted, Unknown};
    let remembered = "my favourite colour is teal and blue";
    let prompt = format!("记住 {remembered}");
    // (seeded memory recalled by the same prompt, classification written)
    for (seeded, expected) in [
        (None, Ordinary),
        (Some(Some(Restricted)), Restricted),
        (Some(None), Unknown),
        (Some(Some(Ordinary)), Ordinary),
    ] {
        let provider = Arc::new(RecordingProvider {
            seen: StdMutex::new(vec![]),
        });
        let (manager, store, kv) =
            manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
        seed_session(&store, "sess_write").await;
        if let Some(stored) = seeded {
            seed_memory(&kv, "a-teal", stored).await;
        }
        run_in_session(&manager, &store, "sess_write", &prompt).await;
        assert_eq!(
            stored_classification(&kv, remembered).await,
            expected,
            "{seeded:?}"
        );
    }
}

#[tokio::test]
async fn remembering_identical_text_again_never_loosens_its_classification() {
    use agent24_store::SourceClassification::{Ordinary, Restricted};
    let remembered = "my favourite colour is teal and blue";
    let prompt = format!("记住 {remembered}");
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, kv) = manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "sess_again").await;
    run_in_session(&manager, &store, "sess_again", &prompt).await;
    assert_eq!(stored_classification(&kv, remembered).await, Ordinary);
    // The same text again, from a run that also recalls a restricted fact.
    seed_memory(&kv, "a-teal", Some(Restricted)).await;
    run_in_session(&manager, &store, "sess_again", &prompt).await;
    assert_eq!(stored_classification(&kv, remembered).await, Restricted);
    // And once restricted, a later ordinary-only run does not lower it.
    use agent24_memory::assertion::AssertionStore;
    kv.assertions()
        .retract(&"a-teal".to_owned(), "test-owner", &now_iso8601())
        .await
        .unwrap();
    run_in_session(&manager, &store, "sess_again", &prompt).await;
    assert_eq!(stored_classification(&kv, remembered).await, Restricted);
}

#[tokio::test]
async fn a_recall_tag_that_cannot_be_written_stops_the_run_before_the_model() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "sess_fail").await;
    seed_memory(
        &kv,
        "a-teal",
        Some(agent24_store::SourceClassification::Ordinary),
    )
    .await;
    sqlx::raw_sql(
        "CREATE TRIGGER no_recall_tags BEFORE INSERT ON run_source_tags \
         WHEN NEW.source_id LIKE 'memory_assertion:%' BEGIN SELECT RAISE(ABORT, 'no'); END;",
    )
    .execute(agent24_store::test_hooks::pool(&store))
    .await
    .unwrap();
    let run = run_in_session(&manager, &store, "sess_fail", "what colour is teal").await;
    assert_eq!(run.status, RunStatus::Failed);
    assert!(
        provider.seen.lock().unwrap().is_empty(),
        "the model must not be called"
    );
}

#[tokio::test]
async fn the_chat_surface_writes_the_folded_classification() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, kv) = manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "sess_chat_write").await;
    seed_memory(
        &kv,
        "a-teal",
        Some(agent24_store::SourceClassification::Restricted),
    )
    .await;
    let remembered = "my favourite colour is teal and blue";
    let prompt = format!("记住 {remembered}");
    let (_, _, source_ref) = manager
        .chat_memory_prelude("run_chat", Some("sess_chat_write"), &prompt, false)
        .await
        .unwrap();
    let outcome = manager
        .chat_remember_turn("run_chat", "sess_chat_write", &prompt, "ok", source_ref)
        .await;
    assert_eq!(outcome, MemoryWriteOutcome::Saved);
    assert_eq!(
        stored_classification(&kv, remembered).await,
        agent24_store::SourceClassification::Restricted
    );
}

async fn process_egress_allowed_in(manager: &Arc<RunManager>, run: &Run) -> bool {
    manager
        .tool_context_for(run, "tc")
        .await
        .unwrap()
        .authorize_egress_if_restricted(
            agent24_domain::EgressPurpose::ProcessExecution,
            agent24_domain::EgressDestination::unknown(),
        )
        .await
        .is_ok()
}

#[tokio::test]
async fn an_ordinary_session_stays_ordinary_across_turns() {
    use agent24_store::SourceClassification::Ordinary;
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, kv) = manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "sess_turns").await;
    run_in_session(&manager, &store, "sess_turns", "记住 alpha is first").await;
    assert_eq!(stored_classification(&kv, "alpha is first").await, Ordinary);
    // The second run carries the first turn as session context.
    let run = run_in_session(&manager, &store, "sess_turns", "记住 beta is second").await;
    assert_eq!(stored_classification(&kv, "beta is second").await, Ordinary);
    assert!(process_egress_allowed_in(&manager, &run).await);
}

#[tokio::test]
async fn a_restricted_earlier_turn_restricts_the_later_turns_of_its_session() {
    use agent24_memory::assertion::AssertionStore;
    use agent24_store::SourceClassification::Restricted;
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, kv) = manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "sess_floor").await;
    seed_memory(&kv, "a-teal", Some(Restricted)).await;
    run_in_session(&manager, &store, "sess_floor", "what colour is teal").await;
    // The fact itself is gone; the later turn sees it only as session
    // context, which the session floor still classifies.
    kv.assertions()
        .retract(&"a-teal".to_owned(), "test-owner", &now_iso8601())
        .await
        .unwrap();
    let run = run_in_session(&manager, &store, "sess_floor", "记住 beta is second").await;
    assert_eq!(
        stored_classification(&kv, "beta is second").await,
        Restricted
    );
    let snapshot = store.run_policy_snapshot(&run.id).await.unwrap();
    let floor = snapshot
        .sources
        .iter()
        .find(|source| source.source_id == "session_floor:sess_floor")
        .unwrap_or_else(|| panic!("no session floor tag in {:?}", snapshot.sources));
    assert_eq!(floor.classification, Restricted);
    assert_eq!(floor.kind, agent24_store::SourceKind::SessionContext);
    assert!(!process_egress_allowed_in(&manager, &run).await);
}

#[tokio::test]
async fn prior_session_context_without_a_recorded_floor_is_unknown() {
    use agent24_store::SourceClassification::Unknown;
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, kv) = manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "sess_lost").await;
    run_in_session(&manager, &store, "sess_lost", "记住 alpha is first").await;
    // The earlier turn is in the session, but its floor is not.
    sqlx::raw_sql("DELETE FROM session_source_floor")
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    let run = run_in_session(&manager, &store, "sess_lost", "记住 beta is second").await;
    assert_eq!(stored_classification(&kv, "beta is second").await, Unknown);
    assert!(!process_egress_allowed_in(&manager, &run).await);
}

#[tokio::test]
async fn a_session_floor_that_cannot_be_written_stops_the_run_before_the_model() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, _kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "sess_nofloor").await;
    sqlx::raw_sql(
        "CREATE TRIGGER no_floor BEFORE INSERT ON session_source_floor \
         BEGIN SELECT RAISE(ABORT, 'no'); END;",
    )
    .execute(agent24_store::test_hooks::pool(&store))
    .await
    .unwrap();
    let run = run_in_session(&manager, &store, "sess_nofloor", "hello").await;
    assert_eq!(run.status, RunStatus::Failed);
    assert!(
        provider.seen.lock().unwrap().is_empty(),
        "the model must not be called"
    );
}

// ── K1-6b.4d: runs started by kernel-created schedules (ADR-K1-02 §2.2) ──

async fn run_scheduled(
    manager: &Arc<RunManager>,
    store: &Store,
    session_id: Option<&str>,
    schedule_id: &str,
    prompt: &str,
) -> Run {
    let run = manager
        .start_run_with_schedule(
            RunCreate {
                knowledge_query: None,
                idoris_headroom_confirmed: false,
                session_id: session_id.map(str::to_owned),
                workspace_id: None,
                prompt: prompt.to_owned(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
                knowledge_binding_id: None,
                skill_id: None,
            },
            Some(schedule_id.to_owned()),
        )
        .await
        .unwrap();
    for _ in 0..200 {
        let current = store.get_run(&run.id).await.unwrap().unwrap();
        if current.status != RunStatus::Running && current.status != RunStatus::Queued {
            return current;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("run did not finish");
}

#[tokio::test]
async fn a_self_wake_run_carries_the_classification_recorded_at_creation() {
    use agent24_store::SourceClassification::{Ordinary, Restricted, Unknown};
    // No session and no memory: nothing else could restrict this run.
    for (origin, egress_allowed) in [(Ordinary, true), (Restricted, false), (Unknown, false)] {
        let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
        store
            .record_schedule_origin("wake_x", "run_creator", origin, &now_iso8601())
            .await
            .unwrap();
        let run = run_scheduled(&manager, &store, None, "wake_x", "check the build").await;
        let snapshot = store.run_policy_snapshot(&run.id).await.unwrap();
        let tag = snapshot
            .sources
            .iter()
            .find(|source| source.source_id == "schedule_origin:wake_x")
            .unwrap_or_else(|| panic!("no schedule origin tag in {:?}", snapshot.sources));
        assert_eq!(tag.classification, origin);
        assert_eq!(tag.kind, agent24_store::SourceKind::ScheduleOrigin);
        assert_eq!(
            process_egress_allowed_in(&manager, &run).await,
            egress_allowed,
            "{origin:?}"
        );
    }
}

#[tokio::test]
async fn a_restricted_self_wake_raises_its_session_floor_for_later_turns() {
    use agent24_store::SourceClassification::Restricted;
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, kv) = manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "sess_wake").await;
    store
        .record_schedule_origin("wake_r", "run_creator", Restricted, &now_iso8601())
        .await
        .unwrap();
    // A scheduled prompt cannot itself write a qualified memory, but the
    // woken turn joins the session, so a later user turn inherits it.
    run_scheduled(
        &manager,
        &store,
        Some("sess_wake"),
        "wake_r",
        "check the build",
    )
    .await;
    run_in_session(&manager, &store, "sess_wake", "记住 gamma is third").await;
    assert_eq!(
        stored_classification(&kv, "gamma is third").await,
        Restricted
    );
}

#[tokio::test]
async fn a_user_schedule_run_has_no_schedule_origin() {
    let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
    let run = run_scheduled(&manager, &store, None, "user_sched", "check the build").await;
    let snapshot = store.run_policy_snapshot(&run.id).await.unwrap();
    assert!(
        snapshot
            .sources
            .iter()
            .all(|source| source.kind != agent24_store::SourceKind::ScheduleOrigin),
        "{:?}",
        snapshot.sources
    );
    assert!(process_egress_allowed_in(&manager, &run).await);
}

#[tokio::test]
async fn a_schedule_origin_that_cannot_be_read_stops_the_run_before_the_model() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, _kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    sqlx::raw_sql("DROP TABLE schedule_source_origin")
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    let run = run_scheduled(&manager, &store, None, "wake_gone", "check the build").await;
    assert_eq!(run.status, RunStatus::Failed);
    assert!(
        provider.seen.lock().unwrap().is_empty(),
        "the model must not be called"
    );
}

#[tokio::test]
async fn a_run_whose_own_sources_cannot_be_read_stops_before_the_model() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, _kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "sess_noread").await;
    agent24_store::test_hooks::fail_source_tag_reads(&store);
    let run = run_in_session(&manager, &store, "sess_noread", "hello").await;
    assert_eq!(run.status, RunStatus::Failed);
    let error = run.error.expect("a failed run carries its error");
    assert_eq!(error.code, "source_policy_unavailable");
    assert!(error.message.contains("session source floor"), "{error:?}");
    assert!(
        provider.seen.lock().unwrap().is_empty(),
        "the model must not be called"
    );
}

// ── K1-6b.5f-2: tool result origins across the turns of a session ──

/// K1-6b.5f-2: what a tool returned in one turn restricts the later turns
/// of its session through its origin, without raising the floor; an
/// ordinary result leaves nothing behind.
#[tokio::test]
async fn a_later_turn_carries_the_origins_of_its_sessions_tool_results() {
    use agent24_store::SourceClassification::{Ordinary, Unknown};
    for (source, inherited) in [
        (
            agent24_tools::ToolResultSource::unknown("reader"),
            Some(Unknown),
        ),
        (agent24_tools::ToolResultSource::ordinary("reader"), None),
    ] {
        let tools = ToolRegistry::new().with(Arc::new(ReadingTool(source)));
        let provider = ScriptedProvider::new(vec![calls("reader", "c1")]);
        let (manager, store, _kv) = manager_with_memory_and_tools(
            Arc::new(provider),
            Arc::new(UnusedSummarizer),
            tools,
        )
        .await;
        seed_session(&store, "sess_tools").await;
        let first = run_in_session(&manager, &store, "sess_tools", "read it").await;
        assert_eq!(first.status, RunStatus::Completed, "{:?}", first.error);
        let later = run_in_session(&manager, &store, "sess_tools", "and then").await;
        let snapshot = store.run_policy_snapshot(&later.id).await.unwrap();
        let classification_of = |id: &str| {
            snapshot
                .sources
                .iter()
                .find(|source| source.source_id == id)
                .map(|source| source.classification)
        };
        assert_eq!(
            classification_of("session_floor:sess_tools"),
            Some(Ordinary)
        );
        assert_eq!(
            classification_of("tool_result:tool:reader:session:sess_tools"),
            inherited
        );
        assert_eq!(
            process_egress_allowed_in(&manager, &later).await,
            inherited.is_none()
        );
    }
}

/// Codex 6b.5f-2 r1 C2: the origin is recorded even while memory is not
/// configured, since the run may finish after a restart that has it.
#[tokio::test]
async fn a_session_records_its_tool_result_origins_without_memory() {
    let tools = ToolRegistry::new().with(Arc::new(ReadingTool(
        agent24_tools::ToolResultSource::unknown("reader"),
    )));
    let provider = ScriptedProvider::new(vec![calls("reader", "c1")]);
    let (manager, _sink, store) = manager_with_tools(Arc::new(provider), tools).await;
    seed_session(&store, "sess_nomemory").await;
    let run = run_in_session(&manager, &store, "sess_nomemory", "read it").await;
    assert_eq!(run.status, RunStatus::Completed, "{:?}", run.error);
    assert_eq!(
        store
            .session_tool_result_origins("sess_nomemory")
            .await
            .unwrap(),
        ["tool:reader"]
    );
}

#[tokio::test]
async fn a_session_whose_tool_result_origins_cannot_be_read_stops_before_the_model() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, _kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "sess_noorigins").await;
    sqlx::raw_sql("DROP TABLE session_tool_result_origins")
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    let run = run_in_session(&manager, &store, "sess_noorigins", "hello").await;
    assert_eq!(run.status, RunStatus::Failed);
    let error = run.error.expect("a failed run carries its error");
    assert_eq!(error.code, "source_policy_unavailable");
    assert!(error.message.contains("tool result origins"), "{error:?}");
    assert!(
        provider.seen.lock().unwrap().is_empty(),
        "the model must not be called"
    );
}

/// K1-6b.5f-2: a result whose session origin cannot be recorded is not
/// tagged either, and never reaches the model.
#[tokio::test]
async fn a_tool_result_whose_session_origin_cannot_be_written_never_reaches_the_model() {
    let provider = Arc::new(WatchingProvider {
        script: ScriptedProvider::new(vec![calls("reader", "c1")]),
        saw_secret: std::sync::atomic::AtomicBool::new(false),
    });
    let tools = ToolRegistry::new().with(Arc::new(ReadingTool(
        agent24_tools::ToolResultSource::unknown("reader"),
    )));
    let (manager, store, _kv) =
        manager_with_memory_and_tools(provider.clone(), Arc::new(UnusedSummarizer), tools)
            .await;
    seed_session(&store, "sess_noorigin").await;
    sqlx::raw_sql(
        "CREATE TRIGGER no_origin BEFORE INSERT ON session_tool_result_origins \
         BEGIN SELECT RAISE(ABORT, 'no'); END;",
    )
    .execute(agent24_store::test_hooks::pool(&store))
    .await
    .unwrap();
    let run = run_in_session(&manager, &store, "sess_noorigin", "read it").await;
    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(run.error.unwrap().code, "source_policy_unavailable");
    assert!(
        !provider
            .saw_secret
            .load(std::sync::atomic::Ordering::SeqCst),
        "the result reached the model"
    );
}

/// K1-6b.5f-2: /chat offers no tools, but what earlier /runs turns of its
/// session took in from tools is part of its context.
#[tokio::test]
async fn the_chat_prelude_carries_its_sessions_tool_result_origins() {
    use agent24_store::SourceClassification::{Ordinary, Unknown};
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, _kv) = manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
    let pool = agent24_store::test_hooks::pool(&store);
    sqlx::raw_sql(
        "INSERT INTO session_tool_result_origins (session_id, origin, created_at) \
         VALUES ('chat_tools', 'tool:shell_exec', 't')",
    )
    .execute(pool)
    .await
    .unwrap();
    for (sid, expected) in [("chat_tools", Unknown), ("chat_plain", Ordinary)] {
        let (_, _, source_ref) = manager
            .chat_memory_prelude("run_chat", Some(sid), "hello", false)
            .await
            .unwrap();
        assert_eq!(source_ref.classification, expected, "{sid}");
    }
    sqlx::raw_sql("DROP TABLE session_tool_result_origins")
        .execute(pool)
        .await
        .unwrap();
    assert!(
        manager
            .chat_memory_prelude("run_chat", Some("chat_tools"), "hello", false)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_chat_prelude_classifies_client_history_by_the_session_floor() {
    use agent24_store::SourceClassification::{Ordinary, Restricted, Unknown};
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, _kv) = manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
    store
        .merge_session_floor("chat_restricted", Restricted, false, &now_iso8601())
        .await
        .unwrap();
    // (session, client history has non-user turns, classification)
    for (sid, has_non_user_history, expected) in [
        ("chat_a", false, Ordinary),
        // chat_a's floor now exists: its own earlier replies are Ordinary.
        ("chat_a", true, Ordinary),
        // No floor: replies the server has no record of are Unknown.
        ("chat_b", true, Unknown),
        ("chat_restricted", true, Restricted),
        ("chat_restricted", false, Restricted),
    ] {
        let (_, _, source_ref) = manager
            .chat_memory_prelude("run_hist", Some(sid), "hello", has_non_user_history)
            .await
            .unwrap();
        assert_eq!(
            source_ref.classification, expected,
            "{sid} {has_non_user_history}"
        );
    }
}

#[tokio::test]
async fn a_chat_prelude_whose_floor_cannot_be_written_fails() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, _kv) = manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
    sqlx::raw_sql(
        "CREATE TRIGGER no_floor BEFORE INSERT ON session_source_floor \
         BEGIN SELECT RAISE(ABORT, 'no'); END;",
    )
    .execute(agent24_store::test_hooks::pool(&store))
    .await
    .unwrap();
    assert!(
        manager
            .chat_memory_prelude("run_x", Some("chat_x"), "hello", false)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_run_whose_sources_cannot_be_read_writes_unknown_memory() {
    let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
    // No tags at all is no evidence either.
    assert_eq!(
        manager
            .run_assertion_source_ref("run_untagged")
            .await
            .classification,
        agent24_store::SourceClassification::Unknown
    );
    sqlx::query("DROP TABLE run_source_tags")
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    assert_eq!(
        manager
            .run_assertion_source_ref("run_untagged")
            .await
            .classification,
        agent24_store::SourceClassification::Unknown
    );
}

#[tokio::test]
async fn the_chat_prelude_folds_the_caller_input_and_every_recalled_fact() {
    use agent24_store::SourceClassification::{Ordinary, Restricted, Unknown};
    for (seeded, expected) in [
        (None, Ordinary),
        (Some(Some(Ordinary)), Ordinary),
        (Some(Some(Restricted)), Restricted),
        (Some(None), Unknown),
    ] {
        let provider = Arc::new(RecordingProvider {
            seen: StdMutex::new(vec![]),
        });
        let (manager, store, kv) =
            manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
        seed_session(&store, "sess_chat").await;
        if let Some(stored) = seeded {
            seed_memory(&kv, "a-teal", stored).await;
        }
        let (prelude, _, source_ref) = manager
            .chat_memory_prelude("run_chat", Some("sess_chat"), "what colour is teal", false)
            .await
            .unwrap();
        assert_eq!(prelude.is_empty(), seeded.is_none(), "{seeded:?}");
        assert_eq!(source_ref.classification, expected, "{seeded:?}");
        assert!(
            source_ref
                .source_ids
                .contains(&"user_input:run_chat".to_owned())
        );
    }
}

#[tokio::test]
async fn a_session_remembers_across_runs() {
    // D1 made live: without memory every run starts from the bare prompt.
    // With it, the second run in a session must SEE the first exchange.
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, _kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "sess_mem").await;

    run_in_session(&manager, &store, "sess_mem", "first question").await;
    run_in_session(&manager, &store, "sess_mem", "second question").await;

    let seen = provider.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "expected one completion per run");
    // Run 1 saw only its own prompt.
    assert_eq!(seen[0].len(), 1);
    assert_eq!(seen[0][0].content.as_deref(), Some("first question"));
    // Run 2 saw the remembered exchange BEFORE its own prompt.
    let second = &seen[1];
    assert!(
        second.len() > 1,
        "second run had no prior context: {second:?}"
    );
    let texts: Vec<&str> = second.iter().filter_map(|m| m.content.as_deref()).collect();
    assert!(texts.contains(&"first question"), "{texts:?}");
    assert!(texts.contains(&"pong"), "{texts:?}");
    assert_eq!(texts.last(), Some(&"second question"));
}

#[tokio::test]
async fn a_run_that_never_parks_sheds_its_copy_of_prior_context_once_terminal() {
    // M1-T07.1 M3 (growth control, review #674): the snapshot persists a
    // COPY of the session's prior context into every run's own
    // `run_messages` row set, so a long session would otherwise grow
    // that table ~O(session length) on every single turn. Once a run
    // that never parked for approval reaches a terminal state, its copy
    // of prior context — pure duplication of what `SessionLog` already
    // holds — is reclaimed; its OWN prompt/answer stay.
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, _kv) = manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "sess_shed").await;

    let run1 = manager
        .start_run(RunCreate {
            knowledge_query: None,
            idoris_headroom_confirmed: false,
            session_id: Some("sess_shed".to_owned()),
            workspace_id: None,
            prompt: "first question".to_owned(),
            model_override: None,
            mode: agent24_protocol::RunMode::Normal,
            knowledge_binding_id: None,
            skill_id: None,
        })
        .await
        .unwrap();
    let run1 = wait_terminal(&store, &run1.id).await;
    assert_eq!(run1.status, RunStatus::Completed);
    // Run 1 had no prior context of its own (first turn in the
    // session) — nothing to shed, its own thread is untouched.
    let thread1 = store.list_run_messages(&run1.id).await.unwrap();
    assert_eq!(
        thread1.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
        vec!["user", "assistant"]
    );

    let run2 = manager
        .start_run(RunCreate {
            knowledge_query: None,
            idoris_headroom_confirmed: false,
            session_id: Some("sess_shed".to_owned()),
            workspace_id: None,
            prompt: "second question".to_owned(),
            model_override: None,
            mode: agent24_protocol::RunMode::Normal,
            knowledge_binding_id: None,
            skill_id: None,
        })
        .await
        .unwrap();
    let run2 = wait_terminal(&store, &run2.id).await;
    assert_eq!(run2.status, RunStatus::Completed);
    // The cleanup runs in `execute()`'s OWN task, AFTER `run_loop`
    // returns and AFTER the terminal transition is already visible — it
    // is eventual, not atomic with "status == Completed". Poll instead
    // of reading the thread the instant the status flips.
    let thread2 = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let thread = store.list_run_messages(&run2.id).await.unwrap();
            if thread.len() == 2 {
                break thread;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("prior-context cleanup should land shortly after completion");
    // Run 2's copy of run 1's exchange ("first question"/"pong") is
    // gone; its OWN prompt and answer remain. Both turns' assistant
    // reply is literally "pong" (same mock provider), so the shape
    // (exactly 2 rows, not 4) is the real proof — "first question" is
    // the one string unique to run 1's copy.
    let texts: Vec<_> = thread2.iter().filter_map(|m| m.content.clone()).collect();
    assert!(!texts.contains(&"first question".to_owned()), "{texts:?}");
    assert_eq!(texts, vec!["second question", "pong"]);
    assert_eq!(
        thread2.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
        vec!["user", "assistant"],
        "{thread2:?}"
    );
}

#[tokio::test]
async fn concurrent_runs_in_one_session_keep_both_turns() {
    // remember_exchange appends a numbered turn, and runs execute
    // in background tasks. Without a per-session lock two runs finishing
    // together both load the old state and the later save drops the other's
    // turn. Both exchanges must survive.
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, kv) = manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "sess_race").await;

    let mut ids = Vec::new();
    for prompt in ["alpha", "beta"] {
        let run = manager
            .start_run(RunCreate {
                knowledge_query: None,
                idoris_headroom_confirmed: false,
                session_id: Some("sess_race".to_owned()),
                workspace_id: None,
                prompt: prompt.to_owned(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
                knowledge_binding_id: None,
                skill_id: None,
            })
            .await
            .unwrap();
        ids.push(run.id);
    }
    for id in &ids {
        wait_terminal(&store, id).await;
    }
    // Memory is committed before completion becomes visible.
    let session = kv
        .session_log()
        .load_view("test-owner", "sess_race")
        .await
        .unwrap();
    let texts: Vec<&str> = session
        .tail
        .iter()
        .filter_map(|(_, m)| m.content.as_deref())
        .collect();
    assert!(texts.contains(&"alpha"), "lost a turn: {texts:?}");
    assert!(texts.contains(&"beta"), "lost a turn: {texts:?}");
    assert_eq!(session.tail.len(), 4, "{texts:?}");
}

#[tokio::test]
async fn many_concurrent_writers_on_one_session_lose_nothing() {
    // Codex (low): the two-run test can pass even unlocked if tokio happens
    // to serialize. Drive remember_exchange directly from many tasks at once
    // so turn number allocation and append must hold the same lock.
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, kv) = manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "sess_many").await;

    const WRITERS: usize = 12;
    let mut tasks = Vec::new();
    for i in 0..WRITERS {
        let m = Arc::clone(&manager);
        tasks.push(tokio::spawn(async move {
            m.remember_exchange(
                Some("sess_many"),
                &format!("q{i}"),
                &format!("a{i}"),
                Origin {
                    source: "agent_loop".into(),
                    trust: Trust::UserSaid,
                },
                agent24_store::AssertionSourceRef::from_sources([]),
            )
            .await;
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }

    let session = kv
        .session_log()
        .load_view("test-owner", "sess_many")
        .await
        .unwrap();
    let texts: Vec<&str> = session
        .tail
        .iter()
        .filter_map(|(_, m)| m.content.as_deref())
        .collect();
    // Every writer's prompt AND answer must have survived.
    for i in 0..WRITERS {
        assert!(
            texts.contains(&format!("q{i}").as_str()),
            "lost q{i}: {texts:?}"
        );
        assert!(
            texts.contains(&format!("a{i}").as_str()),
            "lost a{i}: {texts:?}"
        );
    }
    assert_eq!(session.tail.len(), WRITERS * 2, "{texts:?}");
}

/// A summarizer that signals when it is entered and then blocks, so a test
/// can cancel at a deterministic point INSIDE the memory write.
struct SlowSummarizer {
    entered: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Summarizer for SlowSummarizer {
    async fn summarize(
        &self,
        _prior: Option<&str>,
        _messages: &[Msg],
    ) -> std::result::Result<String, String> {
        self.entered.notify_one();
        tokio::time::sleep(SLOW_SUMMARIZER_BLOCK).await;
        Ok("summary".to_owned())
    }
}

/// Long enough that completing normally is clearly distinguishable from
/// being interrupted by the cancel.
const SLOW_SUMMARIZER_BLOCK: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn cancel_during_the_memory_write_still_cancels_the_run() {
    // Codex (regression I introduced): moving the memory write before
    // transition_run leaves the run non-terminal for up to
    // MEMORY_WRITE_BUDGET. Cancel must still win in that window — "cancel
    // works in any non-terminal state" is the C2 contract.
    let store = Store::open_memory().await.unwrap();
    let sink = Arc::new(RecordingSink(StdMutex::new(vec![])));
    let kv = KvStore::open_memory().await.unwrap();
    // max_recent 1 → the very first turn overflows, so compaction (and the
    // slow summarizer) runs inside the memory write.
    let policy = CompactionPolicy {
        max_recent: 1,
        keep_recent: 0,
        max_summary_chars: 500,
    };
    let entered = Arc::new(tokio::sync::Notify::new());
    let manager = RunManager::with_memory(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(
            Arc::new(FixedProvider),
            Tier::Local,
        )])),
        Arc::new(ToolRegistry::new()),
        sink,
        CancellationToken::new(),
        Some(
            SessionMemory::new(
                kv,
                Arc::new(SlowSummarizer {
                    entered: Arc::clone(&entered),
                }),
            )
            .with_owner("test-owner".into())
            .with_policy(policy),
        ),
    );
    seed_session(&store, "sess_cancel").await;
    let run = manager
        .start_run(RunCreate {
            knowledge_query: None,
            idoris_headroom_confirmed: false,
            session_id: Some("sess_cancel".to_owned()),
            workspace_id: None,
            prompt: "hi".to_owned(),
            model_override: None,
            mode: agent24_protocol::RunMode::Normal,
            knowledge_binding_id: None,
            skill_id: None,
        })
        .await
        .unwrap();
    // Deterministic: wait until the summarizer is actually entered, so the
    // cancel provably lands INSIDE the memory write (not before the run
    // started, and not after it finished).
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .expect("summarizer should have been entered");
    let started = std::time::Instant::now();
    let _ = manager.cancel_run(&run.id).await;
    let final_run = wait_terminal(&store, &run.id).await;
    assert_eq!(
        final_run.status,
        RunStatus::Cancelled,
        "cancel was ignored during the memory write"
    );
    // Latency is the real assertion: without the select! on the cancel token
    // the run would sit until the summarizer returned, so finishing far
    // sooner proves the write was actually interrupted.
    assert!(
        started.elapsed() < SLOW_SUMMARIZER_BLOCK / 2,
        "cancel did not interrupt the memory write (took {:?})",
        started.elapsed()
    );
}

#[tokio::test]
async fn cancel_while_waiting_on_another_runs_session_lock_is_prompt() {
    // Reviewer-found (clestons): session_context() takes the per-session
    // lock BEFORE the model is ever contacted. Run A can hold that lock for
    // up to MEMORY_WRITE_BUDGET while compacting, so Run B parked here must
    // still be cancellable — C2: cancel works in any non-terminal state.
    let store = Store::open_memory().await.unwrap();
    let sink = Arc::new(RecordingSink(StdMutex::new(vec![])));
    let kv = KvStore::open_memory().await.unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    // max_recent 1 → run A's very first turn compacts, so its slow
    // summarizer runs while holding the session lock.
    let policy = CompactionPolicy {
        max_recent: 1,
        keep_recent: 0,
        max_summary_chars: 500,
    };
    let manager = RunManager::with_memory(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(
            Arc::new(FixedProvider),
            Tier::Local,
        )])),
        Arc::new(ToolRegistry::new()),
        sink,
        CancellationToken::new(),
        Some(
            SessionMemory::new(
                kv,
                Arc::new(SlowSummarizer {
                    entered: Arc::clone(&entered),
                }),
            )
            .with_owner("test-owner".into())
            .with_policy(policy),
        ),
    );
    seed_session(&store, "sess_lockwait").await;

    // Run A: proceed until it is inside compaction, holding the lock.
    let run_a = manager
        .start_run(RunCreate {
            knowledge_query: None,
            idoris_headroom_confirmed: false,
            session_id: Some("sess_lockwait".to_owned()),
            workspace_id: None,
            prompt: "a".to_owned(),
            model_override: None,
            mode: agent24_protocol::RunMode::Normal,
            knowledge_binding_id: None,
            skill_id: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .expect("run A should have entered the summarizer holding the lock");

    // Run B: blocks in session_context() waiting for A's lock.
    let run_b = manager
        .start_run(RunCreate {
            knowledge_query: None,
            idoris_headroom_confirmed: false,
            session_id: Some("sess_lockwait".to_owned()),
            workspace_id: None,
            prompt: "b".to_owned(),
            model_override: None,
            mode: agent24_protocol::RunMode::Normal,
            knowledge_binding_id: None,
            skill_id: None,
        })
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let started = std::time::Instant::now();
    let _ = manager.cancel_run(&run_b.id).await;
    let final_b = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        wait_terminal(&store, &run_b.id),
    )
    .await
    .expect("run B stayed stuck in an uncancellable lock wait");
    assert_eq!(final_b.status, RunStatus::Cancelled, "{final_b:?}");
    assert!(
        started.elapsed() < SLOW_SUMMARIZER_BLOCK / 2,
        "cancel waited out the lock holder ({:?})",
        started.elapsed()
    );
    let _ = run_a;
}

#[tokio::test]
async fn without_memory_runs_do_not_accumulate_context() {
    // The default (no SessionMemory) keeps prior behaviour exactly.
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, _sink, store) = manager_with(provider.clone()).await;
    seed_session(&store, "sess_plain").await;
    run_in_session(&manager, &store, "sess_plain", "first question").await;
    run_in_session(&manager, &store, "sess_plain", "second question").await;
    let seen = provider.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[1].len(),
        1,
        "context leaked without memory: {:?}",
        seen[1]
    );
}

#[tokio::test]
async fn an_ordinary_run_is_user_input_and_an_inbound_run_is_unknown_and_blocked_from_egress() {
    use agent24_store::{NewInboundEvent, SourceClassification, SourceKind};
    use agent24_tools::Tool;

    let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
    let ordinary = manager.start_run(create()).await.unwrap();
    let ordinary = wait_terminal(&store, &ordinary.id).await;
    let tags = store.list_run_source_tags(&ordinary.id).await.unwrap();
    let user = tags
        .iter()
        .find(|tag| tag.source.kind == SourceKind::UserInput)
        .unwrap();
    assert_eq!(user.source.classification, SourceClassification::Ordinary);
    assert!(user.source.source_id.starts_with("user_input:"));
    // Ordinary sources are not egress material. Do not call http_fetch:
    // an empty resource set is allowed and would open a socket.
    assert!(process_egress_allowed_in(&manager, &ordinary).await);

    let event_id = "hyphae:agent-egress";
    assert_eq!(
        store
            .insert_inbound_event(&NewInboundEvent {
                id: event_id.to_owned(),
                source: "hyphae".to_owned(),
                category: "direct".to_owned(),
                recipient_identity: None,
                thread_key: None,
                actor: Some("npub1".to_owned()),
                human: true,
                addressed: "direct".to_owned(),
                at: "2026-11-11T00:00:00Z".to_owned(),
                received_at: "2026-11-11T00:00:01Z".to_owned(),
                text: Some("fetch http://1.1.1.1/".to_owned()),
                meta: None,
                disposition: "explicit".to_owned(),
                disposition_reason: None,
                policy_version: Some("p".to_owned()),
                run_state: Some("starting".to_owned()),
            })
            .await
            .unwrap(),
        agent24_store::InboundInsert::Inserted
    );
    let started = manager
        .start_run_inbound(
            RunCreate {
                knowledge_query: None,
                idoris_headroom_confirmed: false,
                session_id: Some("sess_inbound".to_owned()),
                workspace_id: None,
                prompt: "fetch http://1.1.1.1/".to_owned(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
                knowledge_binding_id: None,
                skill_id: None,
            },
            InboundOrigin {
                event_id: event_id.to_owned(),
                channel: "hyphae".to_owned(),
            },
        )
        .await
        .unwrap();
    let InboundRunStart::Started(inbound) = started else {
        panic!("event was not linked");
    };
    let _ = wait_terminal(&store, &inbound.id).await;
    let tags = store.list_run_source_tags(&inbound.id).await.unwrap();
    let user = tags
        .iter()
        .find(|tag| tag.source.kind == SourceKind::UserInput)
        .unwrap();
    assert_eq!(user.source.classification, SourceClassification::Unknown);
    assert!(user.source.source_id.starts_with("inbound_external:"));
    let session = store.get_session("sess_inbound").await.unwrap().unwrap();
    assert_eq!(session.channel, "hyphae");

    let ctx = manager.tool_context_for(&inbound, "tc_out").await.unwrap();
    let mut input = serde_json::Map::new();
    input.insert(
        "url".to_owned(),
        serde_json::Value::String("http://1.1.1.1/".to_owned()),
    );
    let err = agent24_tools::HttpFetchTool::new(false)
        .call(&ctx, &input, &CancellationToken::new())
        .await
        .expect_err("unknown inbound text must not egress");
    match err {
        agent24_tools::ToolError::Denied(message) => {
            assert!(message.contains("outbound policy unavailable"), "{message}");
        }
        other => panic!("expected denial before the request, got {other}"),
    }
}

