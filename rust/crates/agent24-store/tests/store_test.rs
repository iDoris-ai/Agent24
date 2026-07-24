//! Integration tests over an in-memory database (C1 acceptance).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_protocol::{
    Approval, ApprovalStatus, Decision, ErrorBody, Run, RunInput, RunOutput, RunStatus, Schedule,
    ScheduleAction, ScheduleSpec, Session, ToolCall, ToolCallStatus, Usage,
};
use agent24_store::{RunPatch, Store, StoreError};

const TS: &str = "2026-07-24T00:00:00Z";

fn usage() -> Usage {
    Usage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        cost_usd: 0.0,
    }
}

fn run(id: &str) -> Run {
    Run {
        id: id.to_owned(),
        session_id: None,
        status: RunStatus::Queued,
        input: RunInput {
            prompt: "hello".to_owned(),
            model_override: None,
        },
        output: None,
        error: None,
        usage: usage(),
        schedule_id: None,
        created_at: TS.to_owned(),
        started_at: None,
        ended_at: None,
    }
}

fn approval(id: &str, run_id: &str) -> Approval {
    Approval {
        id: id.to_owned(),
        run_id: run_id.to_owned(),
        tool_call_id: "tc_1".to_owned(),
        kind: "exec".to_owned(),
        summary: "run a command".to_owned(),
        payload: serde_json::Map::new(),
        available_decisions: vec!["approve".to_owned(), "deny".to_owned(), "abort".to_owned()],
        standing_target: None,
        status: ApprovalStatus::Pending,
        decision: None,
        expires_at: "2026-07-24T00:05:00Z".to_owned(),
        created_at: TS.to_owned(),
        decided_at: None,
    }
}

#[tokio::test]
async fn session_roundtrip() {
    let store = Store::open_memory().await.unwrap();
    let s = Session {
        id: "sess_1".to_owned(),
        title: "t".to_owned(),
        channel: "cli".to_owned(),
        created_at: TS.to_owned(),
        updated_at: TS.to_owned(),
    };
    store.insert_session(&s).await.unwrap();
    assert_eq!(store.get_session("sess_1").await.unwrap().unwrap(), s);
    assert_eq!(store.list_sessions().await.unwrap().len(), 1);
}

#[tokio::test]
async fn run_lifecycle_happy_path() {
    let store = Store::open_memory().await.unwrap();
    store.insert_run(&run("run_1")).await.unwrap();

    let started = store
        .transition_run(
            "run_1",
            RunStatus::Running,
            RunPatch {
                started_at: Some(TS.to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(started.status, RunStatus::Running);
    assert_eq!(started.started_at.as_deref(), Some(TS));

    let done = store
        .transition_run(
            "run_1",
            RunStatus::Completed,
            RunPatch {
                output: Some(RunOutput {
                    text: "done".to_owned(),
                }),
                ended_at: Some(TS.to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(done.status, RunStatus::Completed);
    assert_eq!(done.output.unwrap().text, "done");
}

#[tokio::test]
async fn illegal_run_transition_is_rejected_with_row_intact() {
    let store = Store::open_memory().await.unwrap();
    store.insert_run(&run("run_1")).await.unwrap();

    // queued → completed is illegal
    let err = store
        .transition_run("run_1", RunStatus::Completed, RunPatch::default())
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Transition(_)), "{err:?}");
    assert_eq!(
        store.get_run("run_1").await.unwrap().unwrap().status,
        RunStatus::Queued
    );

    // terminal states absorb: completed → running is illegal
    store
        .transition_run("run_1", RunStatus::Running, RunPatch::default())
        .await
        .unwrap();
    store
        .transition_run("run_1", RunStatus::Cancelled, RunPatch::default())
        .await
        .unwrap();
    let err = store
        .transition_run("run_1", RunStatus::Running, RunPatch::default())
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Transition(_)));
}

#[tokio::test]
async fn missing_run_is_not_found() {
    let store = Store::open_memory().await.unwrap();
    let err = store
        .transition_run("nope", RunStatus::Running, RunPatch::default())
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)));
}

#[tokio::test]
async fn run_failure_records_error_body() {
    let store = Store::open_memory().await.unwrap();
    store.insert_run(&run("run_1")).await.unwrap();
    store
        .transition_run("run_1", RunStatus::Running, RunPatch::default())
        .await
        .unwrap();
    let failed = store
        .transition_run(
            "run_1",
            RunStatus::Failed,
            RunPatch {
                error: Some(ErrorBody {
                    code: "provider_unavailable".to_owned(),
                    message: "no provider".to_owned(),
                    details: None,
                }),
                ended_at: Some(TS.to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(failed.error.unwrap().code, "provider_unavailable");
    // status filter
    assert_eq!(
        store
            .list_runs(Some(RunStatus::Failed))
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .list_runs(Some(RunStatus::Queued))
            .await
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn tool_call_finishes_exactly_once() {
    let store = Store::open_memory().await.unwrap();
    store.insert_run(&run("run_1")).await.unwrap();
    store
        .insert_tool_call(&ToolCall {
            id: "tc_1".to_owned(),
            run_id: "run_1".to_owned(),
            tool: "http_fetch".to_owned(),
            input: serde_json::Map::new(),
            status: ToolCallStatus::Running,
            output_summary: None,
            started_at: TS.to_owned(),
            ended_at: None,
        })
        .await
        .unwrap();

    store
        .finish_tool_call(
            "tc_1",
            ToolCallStatus::Completed,
            Some("200 OK".to_owned()),
            TS.to_owned(),
        )
        .await
        .unwrap();
    // double finish → Conflict
    let err = store
        .finish_tool_call("tc_1", ToolCallStatus::Failed, None, TS.to_owned())
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)));
    // truly missing id → NotFound (not Conflict) — API taxonomy mirror of approvals
    let err = store
        .finish_tool_call(
            "tc_never_existed",
            ToolCallStatus::Completed,
            None,
            TS.to_owned(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)), "{err:?}");
    let calls = store.list_tool_calls("run_1").await.unwrap();
    assert_eq!(calls[0].status, ToolCallStatus::Completed);
    assert_eq!(calls[0].output_summary.as_deref(), Some("200 OK"));
}

#[tokio::test]
async fn approval_resolves_exactly_once_and_double_resolve_conflicts() {
    let store = Store::open_memory().await.unwrap();
    store.insert_run(&run("run_1")).await.unwrap();
    store
        .insert_approval(&approval("apr_1", "run_1"))
        .await
        .unwrap();

    let decision = Decision {
        kind: "approve".to_owned(),
        reason: None,
        extra: serde_json::Map::new(),
    };
    let resolved = store
        .resolve_approval(
            "apr_1",
            ApprovalStatus::Approved,
            Some(&decision),
            TS.to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(resolved.status, ApprovalStatus::Approved);
    assert_eq!(resolved.decision.unwrap().kind, "approve");

    // 409-semantics: second resolution conflicts
    let err = store
        .resolve_approval("apr_1", ApprovalStatus::Denied, None, TS.to_owned())
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)));
}

#[tokio::test]
async fn lingering_approvals_abort_on_startup_sweep() {
    let store = Store::open_memory().await.unwrap();
    store.insert_run(&run("run_1")).await.unwrap();
    store
        .insert_approval(&approval("apr_1", "run_1"))
        .await
        .unwrap();
    store
        .insert_approval(&approval("apr_2", "run_1"))
        .await
        .unwrap();

    let swept = store.abort_lingering_approvals(TS).await.unwrap();
    assert_eq!(swept, 2);
    for id in ["apr_1", "apr_2"] {
        assert_eq!(
            store.get_approval(id).await.unwrap().unwrap().status,
            ApprovalStatus::Aborted,
            "fail-closed sweep must abort {id}"
        );
    }
}

#[tokio::test]
async fn schedule_upsert_roundtrip_and_delete() {
    let store = Store::open_memory().await.unwrap();
    let mut schedule = Schedule {
        id: "sch_1".to_owned(),
        name: "morning digest".to_owned(),
        enabled: true,
        spec: ScheduleSpec::Cron {
            expr: "0 8 * * *".to_owned(),
            tz: Some("Asia/Shanghai".to_owned()),
        },
        action: ScheduleAction::AgentRun {
            prompt: "digest".to_owned(),
            session_id: None,
            model_override: None,
        },
        delivery: vec![],
        last_run_at: None,
        next_run_at: Some(TS.to_owned()),
        consecutive_failures: 0,
    };
    store.upsert_schedule(&schedule).await.unwrap();
    assert_eq!(
        store.get_schedule("sch_1").await.unwrap().unwrap(),
        schedule
    );

    schedule.enabled = false;
    schedule.consecutive_failures = 5;
    store.upsert_schedule(&schedule).await.unwrap();
    let loaded = store.get_schedule("sch_1").await.unwrap().unwrap();
    assert!(!loaded.enabled);
    assert_eq!(loaded.consecutive_failures, 5);

    assert!(store.delete_schedule("sch_1").await.unwrap());
    assert!(!store.delete_schedule("sch_1").await.unwrap());
}

#[tokio::test]
async fn update_schedule_runtime_preserves_user_fields_and_needs_the_row() {
    // The scheduler's fire path must touch only runtime columns: a concurrent
    // PATCH to name/spec/action/delivery must survive, and a deleted row must
    // not be resurrected (review C5).
    let store = Store::open_memory().await.unwrap();
    let schedule = Schedule {
        id: "sch_rt".to_owned(),
        name: "original".to_owned(),
        enabled: true,
        spec: ScheduleSpec::Every { secs: 3600 },
        action: ScheduleAction::AgentRun {
            prompt: "original prompt".to_owned(),
            session_id: None,
            model_override: None,
        },
        delivery: vec![],
        last_run_at: None,
        next_run_at: Some(TS.to_owned()),
        consecutive_failures: 0,
    };
    store.upsert_schedule(&schedule).await.unwrap();

    // Simulate a concurrent PATCH renaming + re-specing the row
    let mut patched = schedule.clone();
    patched.name = "renamed by patch".to_owned();
    patched.spec = ScheduleSpec::Every { secs: 120 };
    store.upsert_schedule(&patched).await.unwrap();

    // The scheduler fires with its STALE copy, writing runtime fields only
    let mut stale = schedule.clone();
    stale.last_run_at = Some(TS.to_owned());
    stale.next_run_at = Some(TS.to_owned());
    stale.consecutive_failures = 3;
    assert!(store.update_schedule_runtime(&stale).await.unwrap());

    let loaded = store.get_schedule("sch_rt").await.unwrap().unwrap();
    // user fields survive the runtime write
    assert_eq!(loaded.name, "renamed by patch");
    assert_eq!(loaded.spec, ScheduleSpec::Every { secs: 120 });
    // runtime fields applied
    assert_eq!(loaded.consecutive_failures, 3);
    assert_eq!(loaded.last_run_at.as_deref(), Some(TS));

    // deleted row: runtime update reports it's gone, never resurrects
    assert!(store.delete_schedule("sch_rt").await.unwrap());
    assert!(!store.update_schedule_runtime(&stale).await.unwrap());
    assert!(store.get_schedule("sch_rt").await.unwrap().is_none());
}

#[tokio::test]
async fn audit_chain_appends_and_detects_tampering() {
    let store = Store::open_memory().await.unwrap();
    for i in 0..3 {
        store
            .append_audit(TS, "daemon", "test.action", &serde_json::json!({ "i": i }))
            .await
            .unwrap();
    }
    let entries = store.list_audit().await.unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].prev_hash, "genesis");
    assert_eq!(entries[1].prev_hash, entries[0].hash);
    store.verify_audit_chain().await.unwrap();

    // Tamper with an early entry — verification must fail
    sqlx::query("UPDATE audit_log SET detail = '{\"i\":999}' WHERE seq = 1")
        .execute(store_pool(&store))
        .await
        .unwrap();
    assert!(store.verify_audit_chain().await.is_err());
}

// test-only access to the pool for the tamper test
fn store_pool(store: &Store) -> &sqlx::SqlitePool {
    // Store is Clone and pool is private — expose via a tiny helper using the
    // public API instead: we re-open is not possible for :memory:. Use the
    // crate's test hook.
    agent24_store::test_hooks::pool(store)
}

#[tokio::test]
async fn concurrent_audit_appends_never_fork_the_chain() {
    // File-backed store with the production pool (5 connections, WAL):
    // exercises the BEGIN IMMEDIATE serialization under real concurrency.
    let dir = std::env::temp_dir().join(format!("a24-store-test-{}", std::process::id()));
    let path = dir.join("audit-concurrency.db");
    let _ = std::fs::remove_file(&path);
    let store = Store::open(&path).await.unwrap();

    let mut handles = Vec::new();
    for i in 0..10 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            store
                .append_audit(
                    TS,
                    "test",
                    "concurrent.append",
                    &serde_json::json!({ "i": i }),
                )
                .await
                .unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    let entries = store.list_audit().await.unwrap();
    assert_eq!(entries.len(), 10);
    store.verify_audit_chain().await.unwrap();
    let _ = std::fs::remove_file(&path);
}
