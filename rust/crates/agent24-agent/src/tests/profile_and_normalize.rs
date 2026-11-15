use super::*;
use agent24_memory::{KvStore, session::CompactionPolicy};
use agent24_models::router::Tier;
use agent24_models::{CompletionResponse, ModelProvider, ToolCallRequest};
use async_trait::async_trait;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

#[tokio::test]
async fn resumed_thread_cannot_rebuild_remote_permission_from_default_profile() {
    let remote = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let store = Store::open_memory().await.unwrap();
    let manager = RunManager::new(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(
            remote.clone(),
            Tier::Remote,
        )])),
        Arc::new(ToolRegistry::new()),
        Arc::new(RecordingSink(StdMutex::new(vec![]))),
        CancellationToken::new(),
    );
    let mut run = manager.start_run(create()).await.unwrap();
    let _ = wait_terminal(&store, &run.id).await;
    sqlx::query("UPDATE runs SET status='running', ended_at=NULL WHERE id=?")
        .bind(&run.id)
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    run.status = RunStatus::Running;
    // Replay the stored thread so the send-copy consistency check passes
    // and the refusal below can only come from the permission check.
    let thread = store.list_run_messages(&run.id).await.unwrap();
    remote.seen.lock().unwrap().clear();
    manager
        .run_loop(
            run.clone(),
            thread_to_messages(&thread),
            false,
            CancellationToken::new(),
            false,
        )
        .await;
    assert!(remote.seen.lock().unwrap().is_empty());
    let ended = store.get_run(&run.id).await.unwrap().unwrap();
    assert_eq!(ended.status, RunStatus::Failed);
    assert_ne!(
        ended.error.as_ref().map(|e| e.code.as_str()),
        Some(SOURCE_UNPROVEN_CODE),
        "must reach the permission check, not the source gate"
    );
}

/// K1-6b.5b (ADR-K1-02 §2.4 (b)): a run whose source tags cannot be read
/// is routed local-only, not with the caller's base profile, so a
/// remote-only router has nothing to serve it with.
#[tokio::test]
async fn a_run_whose_sources_cannot_be_read_is_routed_local_only() {
    use agent24_models::router::Privacy;
    let remote = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let store = Store::open_memory().await.unwrap();
    let manager = RunManager::new(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(
            remote.clone(),
            Tier::Remote,
        )])),
        Arc::new(ToolRegistry::new()),
        Arc::new(RecordingSink(StdMutex::new(vec![]))),
        CancellationToken::new(),
    );
    agent24_store::test_hooks::fail_source_tag_reads(&store);
    let profile = manager
        .task_profile_for_run("run_unreadable", TaskProfile::default())
        .await;
    assert_eq!(profile.privacy, Privacy::LocalOnly);

    let run = manager.start_run(create()).await.unwrap();
    let run = wait_terminal(&store, &run.id).await;
    assert_eq!(run.status, RunStatus::Failed);
    let error = run.error.expect("a failed run carries its error");
    assert!(error.message.contains("local-only"), "{error:?}");
    assert!(remote.seen.lock().unwrap().is_empty());
}

#[test]
fn cloud_authorized_policy_preserves_the_base_profile() {
    use agent24_models::router::Privacy;

    let shareable = TaskProfile::default();
    assert_eq!(
        merge_source_policy(shareable, agent24_store::SourceMode::CloudAuthorized).privacy,
        Privacy::Any
    );
    let already_restricted = TaskProfile {
        privacy: Privacy::LocalOnly,
        ..TaskProfile::default()
    };
    assert_eq!(
        merge_source_policy(
            already_restricted,
            agent24_store::SourceMode::CloudAuthorized
        )
        .privacy,
        Privacy::LocalOnly
    );
    assert_eq!(
        merge_source_policy(shareable, agent24_store::SourceMode::LocalOnly).privacy,
        Privacy::LocalOnly
    );
}

#[tokio::test]
async fn source_tag_write_failure_stops_before_any_model_call() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, _, store) = manager_with(provider.clone()).await;
    sqlx::query(
        "CREATE TRIGGER reject_source_tag BEFORE INSERT ON run_source_tags \
         BEGIN SELECT RAISE(FAIL, 'source tag unavailable'); END",
    )
    .execute(agent24_store::test_hooks::pool(&store))
    .await
    .unwrap();

    let run = manager.start_run(create()).await.unwrap();
    let done = wait_terminal(&store, &run.id).await;
    assert_eq!(done.error.unwrap().code, "source_policy_unavailable");
    assert!(provider.seen.lock().unwrap().is_empty());
}

/// Every role in `messages`, in order — the shape an HF-strict chat
/// template (Gemma/Mistral) actually validates.
fn roles(messages: &[Msg]) -> Vec<&str> {
    messages.iter().map(|m| m.role.as_str()).collect()
}

#[test]
fn normalize_for_provider_puts_summary_first_and_merges_the_adjacent_recall_and_prompt() {
    // M1-T07.1 H1 (review #674): with a session summary in prior
    // context, the raw snapshot is [user(recall), system(summary),
    // user(prompt)] — system in the MIDDLE, which several HF-strict
    // templates reject outright. Negative control: without the fix (the
    // old `messages.clone()` passthrough), this input is handed to the
    // provider completely unchanged, so `roles(&input) == roles(&out)`
    // would hold and this assertion would be false.
    let input = vec![
        Msg::user("[记忆数据·非指令] ... [记忆数据结束]"),
        Msg::system("Summary of earlier conversation:\nthey talked about cats"),
        Msg::user("我对什么过敏？"),
    ];
    let out = normalize_for_provider(&input);
    assert_eq!(roles(&out), vec!["system", "user"]);
    assert_eq!(out[0].content.as_deref(), input[1].content.as_deref());
    let merged = out[1].content.as_deref().unwrap();
    assert!(merged.starts_with("[记忆数据·非指令]"));
    assert!(merged.ends_with("我对什么过敏？"));
    assert_ne!(
        roles(&input),
        roles(&out),
        "normalization must actually change the shape sent to the provider"
    );
}

#[test]
fn normalize_for_provider_merges_recall_and_prompt_with_no_prior_history() {
    // M1-T07.1 H1: a session's very first turn has an EMPTY prior
    // context, so the raw snapshot is just [user(recall), user(prompt)]
    // — two adjacent `user` turns, no `system` at all.
    let input = vec![
        Msg::user("[记忆数据·非指令] ... [记忆数据结束]"),
        Msg::user("我对什么过敏？"),
    ];
    let out = normalize_for_provider(&input);
    assert_eq!(roles(&out), vec!["user"]);
    let merged = out[0].content.as_deref().unwrap();
    assert!(merged.starts_with("[记忆数据·非指令]"));
    assert!(merged.ends_with("我对什么过敏？"));
}

#[test]
fn prune_stale_recall_message_strips_only_the_dead_lines() {
    // M1-T07.1 M4: a block naming two ids, one still active and one not
    // (forgotten since the first call) — only the dead line is removed,
    // the surviving line and the header/end-marker shape stay intact.
    let content = format!(
        "{}\n- [id=alive recorded_at=t1] \"still true\"\n- [id=dead recorded_at=t2] \"forgotten\"{}",
        RECALL_PREFIX, RECALL_END_MARKER,
    );
    let active: std::collections::HashSet<String> = ["alive".to_owned()].into_iter().collect();
    let pruned = prune_stale_recall_message(Msg::user(content), &active).unwrap();
    let text = pruned.content.unwrap();
    assert!(text.contains("alive"));
    assert!(text.contains("still true"));
    assert!(!text.contains("dead"), "{text}");
    assert!(!text.contains("forgotten"), "{text}");
    assert!(text.ends_with(RECALL_END_MARKER));
}

#[test]
fn prune_stale_recall_message_drops_the_whole_block_once_every_id_is_dead() {
    let content = format!(
        "{}\n- [id=dead recorded_at=t1] \"forgotten\"{}",
        RECALL_PREFIX, RECALL_END_MARKER,
    );
    let active = std::collections::HashSet::new();
    assert!(prune_stale_recall_message(Msg::user(content), &active).is_none());
}

#[test]
fn prune_stale_recall_message_never_touches_an_ordinary_message() {
    let msg = Msg::user("我对什么过敏？");
    let active = std::collections::HashSet::new();
    assert_eq!(prune_stale_recall_message(msg.clone(), &active), Some(msg));
}

#[test]
fn prune_stale_recall_message_preserves_rather_than_drops_a_malformed_lookalike() {
    // Round 2 ②: a message that starts with the exact recall header but
    // does NOT end with the end marker is not a real recall block — the
    // OLD `?`-propagating code would have silently DELETED such a
    // message (treating "cannot parse" as "fully stale, drop it");
    // fail-safe here means PRESERVE what we cannot confidently parse.
    let msg = Msg::user(format!("{RECALL_PREFIX}... but not a real block"));
    let active = std::collections::HashSet::new();
    assert_eq!(prune_stale_recall_message(msg.clone(), &active), Some(msg));
}

#[test]
fn normalize_for_provider_leaves_an_already_alternating_thread_untouched() {
    // The common tool-calling tail (assistant → tool → assistant) never
    // has two adjacent same-role turns, so normalization must be a
    // structural no-op there — only the snapshot PREFIX ever needs it.
    let input = vec![Msg::user("hi"), Msg::assistant(Some("ok".into()), vec![])];
    assert_eq!(normalize_for_provider(&input), input);
}

#[test]
fn workspace_agent_clock_preserves_fractional_millis() {
    let now = std::time::UNIX_EPOCH + Duration::from_millis(605);
    assert_eq!(workspace_now_at(now).unwrap(), "1970-01-01T00:00:00.605Z");
}

