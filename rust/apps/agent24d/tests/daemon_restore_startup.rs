//! R03: daemon 启动恢复回归。
//!
//! 按 `server::serve` 的启动顺序，对落盘的 SQLite 依次调用既有函数
//! `timeout_expired` → `recover_timed_out_approval_runs` → `restore_pending_approvals`
//! → `sweep_orphan_runs` → `sweep_workspace_orphan_runs`（均为现有 public API，
//! 未新增任何生产入口），断言：未过期 pending approval 被恢复而不是取消；
//! 原 recall snapshot 的 content/seq 与启动前逐字节相同；个人记忆 pause 不删除原文。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{sync::Arc, time::Duration};

use agent24_agent::{EventSink, RECALL_END_MARKER, RECALL_PREFIX, RunManager};
use agent24_memory::KvStore;
use agent24_models::router::ModelRouter;
use agent24_policy::ApprovalBroker;
use agent24_protocol::{
    Approval, ApprovalStatus, EventBody, Run, RunInput, RunMode, RunStatus, Usage,
};
use agent24_store::{Store, WorkspaceInstant};
use serde_json::{Map, json};
use tokio_util::sync::CancellationToken;

struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _: EventBody) {}
}

const OWNER: &str = "r03-owner";
const RUN_ID: &str = "run_r03";
const APPROVAL_ID: &str = "appr_r03";

#[tokio::test]
async fn startup_restores_unexpired_pending_approval_and_keeps_snapshot_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let now = agent24_core::util::now_iso8601();

    // ── 启动前（"上一个进程"）：落盘 run、原 snapshot、未过期 pending approval ──
    let store = Store::open(&dir.path().join("agent24.db")).await.unwrap();
    store
        .insert_run(&Run {
            id: RUN_ID.to_owned(),
            session_id: None,
            workspace_id: None,
            status: RunStatus::AwaitingApproval,
            input: RunInput {
                prompt: "我对什么过敏？".to_owned(),
                workspace_id: None,
                model_override: None,
                mode: RunMode::Normal,
                knowledge_binding_id: None,
            },
            output: None,
            error: None,
            usage: Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                cost_usd: 0.0,
            },
            schedule_id: None,
            created_at: now.clone(),
            started_at: Some(now.clone()),
            ended_at: None,
        })
        .await
        .unwrap();
    let snapshot_text = format!(
        "{RECALL_PREFIX}\n- [id=m1 recorded_at={now}] \"我对花生过敏\"{RECALL_END_MARKER}\n\n我对什么过敏？"
    );
    store
        .append_run_message(RUN_ID, "user", Some(&snapshot_text), &json!([]), None, &now)
        .await
        .unwrap();
    store
        .append_run_message(
            RUN_ID,
            "assistant",
            None,
            &json!([{
                "id": "call-r03",
                "name": "shell_exec",
                "arguments": json!({"argv": ["/bin/echo", "r03"]}).to_string(),
            }]),
            None,
            &now,
        )
        .await
        .unwrap();
    let mut payload = Map::new();
    payload.insert("argv".into(), json!(["/bin/echo", "r03"]));
    store
        .insert_tool_call(&agent24_protocol::ToolCall {
            id: "tc_r03".to_owned(),
            run_id: RUN_ID.to_owned(),
            tool: "shell_exec".to_owned(),
            input: payload.clone(),
            status: agent24_protocol::ToolCallStatus::Running,
            output_summary: None,
            started_at: now.clone(),
            ended_at: None,
        })
        .await
        .unwrap();
    store
        .insert_approval(&Approval {
            id: APPROVAL_ID.to_owned(),
            run_id: RUN_ID.to_owned(),
            tool_call_id: "tc_r03".to_owned(),
            kind: "exec".to_owned(),
            summary: "shell_exec: /bin/echo r03".to_owned(),
            payload,
            available_decisions: vec!["approve".to_owned(), "deny".to_owned()],
            standing_target: None,
            status: ApprovalStatus::Pending,
            decision: None,
            expires_at: agent24_core::util::iso8601_after(Duration::from_secs(3600)),
            created_at: now.clone(),
            decided_at: None,
        })
        .await
        .unwrap();

    // pause：个人记忆开关关闭；只是对未来读写的闸门，不得触碰既有 snapshot。
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    kv.set_memory_enabled(OWNER, false).await.unwrap();

    let before = store.list_run_messages(RUN_ID).await.unwrap();
    assert_eq!(before.len(), 2);
    let snapshot_before = before[0].clone();
    assert_eq!(snapshot_before.seq, 0);
    assert_eq!(
        snapshot_before.content.as_deref(),
        Some(snapshot_text.as_str())
    );

    // 模拟守护进程重启：丢弃旧句柄，从同一批磁盘文件重开。
    drop(store);
    drop(kv);
    let store = Store::open(&dir.path().join("agent24.db")).await.unwrap();

    let broker = ApprovalBroker::new(
        store.clone(),
        Arc::new(|_: EventBody| {}),
        Duration::from_secs(60),
    );
    let runs = RunManager::new(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![])),
        Arc::new(agent24_tools::ToolRegistry::builtin(
            dir.path().to_path_buf(),
        )),
        Arc::new(NullSink) as Arc<dyn EventSink>,
        CancellationToken::new(),
    );

    // ── 与 server::serve 相同的启动顺序 ──
    let approval_now = agent24_core::util::now_iso8601();
    let expired = broker.timeout_expired(&approval_now).await.unwrap();
    let recovered_timeouts = runs.recover_timed_out_approval_runs().await.unwrap();
    let (restored, aborted) = runs.restore_pending_approvals().await.unwrap();
    let orphans = store
        .sweep_orphan_runs(&agent24_core::util::now_iso8601())
        .await
        .unwrap();
    let workspace_now = WorkspaceInstant::parse(
        &agent24_core::util::iso8601_millis_at(std::time::SystemTime::now()).unwrap(),
    )
    .unwrap();
    let workspace_orphans = store
        .sweep_workspace_orphan_runs(&workspace_now)
        .await
        .unwrap();

    // 未过期 pending approval：被恢复，不是超时/取消/中止。
    assert_eq!(expired, 0, "未过期的 approval 不得被 timeout_expired 处理");
    assert_eq!(recovered_timeouts, 0);
    assert_eq!((restored, aborted), (1, 0), "必须恢复而不是中止");
    assert_eq!(orphans, 0, "恢复后的 run 不得被 legacy orphan sweep 取消");
    assert_eq!(workspace_orphans.released_leases, 0);
    let approval = store.get_approval(APPROVAL_ID).await.unwrap().unwrap();
    assert_eq!(approval.status, ApprovalStatus::Pending);
    assert!(approval.decision.is_none() && approval.decided_at.is_none());
    let run = store.get_run(RUN_ID).await.unwrap().unwrap();
    assert_eq!(run.status, RunStatus::AwaitingApproval);
    assert!(run.ended_at.is_none());

    // snapshot：content / seq 与启动前逐字节相同，历史未被删除或改写。
    let after = store.list_run_messages(RUN_ID).await.unwrap();
    assert_eq!(
        after, before,
        "启动恢复后整条 run_messages 必须与启动前相同"
    );
    assert_eq!(after[0].seq, snapshot_before.seq);
    assert_eq!(
        after[0].content.as_ref().unwrap().as_bytes(),
        snapshot_text.as_bytes(),
        "snapshot content 必须逐字节相同"
    );
    assert!(after[0].content.as_deref().unwrap().contains(RECALL_PREFIX));

    // pause 状态仍在，且原文没有因 pause 被删除。
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    assert!(!kv.memory_enabled(OWNER).await.unwrap(), "pause 开关应保持");
}
