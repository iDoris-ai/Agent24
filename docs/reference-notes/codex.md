# 研读笔记：codex-rs — 权限审批 / 事件协议 / TUI

> 来源：`vendor/reference/codex/codex-rs/`（openai/codex，Apache-2.0，本地只读克隆）
> 日期：2026-07-23 | 用途：Agent24 M-B/M-C 设计输入（ADR-026）
> 所有 `path:line` 相对 `codex-rs/` 目录。

---

## 0. crate 结构一览（共 97 crate，Cargo workspace）

与三个专题最相关的分层：

| 层 | crate | 职责 |
|---|---|---|
| **协议契约** | `protocol` | 核心类型：`EventMsg`、`Op`、`ReviewDecision`、`AskForApproval`、`SandboxPolicy`、审批事件（`approvals.rs`）。agent 内核与所有 client 之间的"内部" wire 协议 |
| | `app-server-protocol` | 对外 JSON-RPC 协议：`ClientRequest` / `ServerRequest` / `ServerNotification`。审批在这里是 server→client 的 **request**（要求回包），而非单向 notification |
| **内核** | `core` | agent 主循环、session/turn 状态机、审批仲裁、sandbox 选择、工具编排（`orchestrator`） |
| | `state` / `thread-manager-sample` | 会话/线程状态 |
| **传输** | `app-server-transport` | stdio / unix-socket / websocket 三种传输，JSON-RPC 帧编解码 |
| | `app-server` / `app-server-daemon` | 常驻服务进程 + 生命周期管理（pid/lock/update） |
| | `app-server-client` | Rust 侧 client SDK |
| **沙箱** | `sandboxing` | 跨平台 sandbox 抽象（`SandboxManager` / `SandboxType`） |
| | `linux-sandbox` | Landlock + bubblewrap（`landlock.rs` `bwrap.rs`） |
| | `windows-sandbox-rs` | Windows sandbox |
| | `execpolicy` | 命令前缀规则引擎（allow/deny/prompt/forbidden），审批"记住"时写入的规则 |
| **前端** | `tui` | ratatui + crossterm 终端 UI |
| | `exec` | 非交互 headless 执行入口 |
| **周边** | `otel` `rollout` `rollout-trace` `hooks` `mcp-server` `memories` | 遥测、会话录制持久化、hook、MCP、记忆 |

---

## 专题 A — 权限 / 审批

### A1. 审批策略等级（`protocol/src/protocol.rs:918` `AskForApproval`）

```rust
pub enum AskForApproval {
    UnlessTrusted,              // wire: "untrusted" —— 只有已知只读安全命令自动放行，其余全部问
    OnRequest,                  // 默认 —— 模型自己决定何时请求审批；文件系统不受限时不问
    Granular(GranularApprovalConfig),  // 细粒度：按类别 true=允许弹窗 / false=自动拒绝
    Never,                      // 从不问，失败直接回给模型
}
```

`GranularApprovalConfig`（`protocol.rs:945`）按审批**来源**分桶开关：`sandbox_approval` / `rules`（execpolicy prompt 规则）/ `skill_approval` / `request_permissions` / `mcp_elicitations`。`false` 语义是"静默自动拒绝"，不是"放行"——设计要点：**拒绝也是一种明确决策**。

### A2. Sandbox 策略及其与审批的关系（`protocol.rs:1005` `SandboxPolicy`）

```rust
pub enum SandboxPolicy {
    DangerFullAccess,                       // 无限制
    ReadOnly { network_access },            // 只读
    ExternalSandbox { network_access },     // 已在外部沙箱内
    WorkspaceWrite { writable_roots, network_access, exclude_tmpdir_env_var, exclude_slash_tmp },
}
```

平台实现：macOS = seatbelt（`sandbox-exec`，`core/src/sandboxing/mod.rs:151`）；Linux = Landlock + bwrap；Windows = `windows-sandbox-rs`。

**审批 vs 沙箱是两道正交闸门**，由工具编排器串起来（`core/src/tools/orchestrator.rs` 模块 doc 原话）：

> `approval → select sandbox → attempt → retry with an escalated sandbox strategy on denial (no re-approval thanks to caching)`

`default_exec_approval_requirement`（`core/src/tools/sandboxing.rs:198`）把 `(AskForApproval, FileSystemSandboxPolicy)` 映射成三态（`sandboxing.rs:156`）：

```rust
enum ExecApprovalRequirement {
    Skip { bypass_sandbox, proposed_execpolicy_amendment },  // 免审批
    NeedsApproval { reason, proposed_execpolicy_amendment }, // 需弹窗
    Forbidden { reason },                                    // 直接禁止（granular=false 时）
}
```

**「沙箱失败 → 升级重试」流程**（`orchestrator.rs:293-390`）：命令先在沙箱内尝试；若 `SandboxErr::Denied` 且 `tool.escalate_on_failure()` 且策略允许，则**二次向用户请求**"是否脱离沙箱重跑"（携带 `retry_reason`）。同一命令同一 session 二次不重复问（审批缓存）。

### A3. 审批请求如何从执行层冒泡到 UI（核心机制，最值得抄）

用 **per-turn 的 `oneshot` channel 表**：

- `TurnState`（`core/src/state/turn.rs:88`）持有：
  ```rust
  pending_approvals:            HashMap<String, oneshot::Sender<ReviewDecision>>,
  pending_request_permissions:  HashMap<String, PendingRequestPermissions>,
  pending_user_input:           HashMap<String, oneshot::Sender<RequestUserInputResponse>>,
  pending_elicitations:         HashMap<(String, RequestId), oneshot::Sender<ElicitationResponse>>,
  pending_dynamic_tools:        HashMap<String, oneshot::Sender<DynamicToolResponse>>,
  ```

- **请求**（`core/src/session/mod.rs:2265` `request_command_approval`）：
  1. `let (tx, rx) = oneshot::channel();`
  2. 把 `tx` 插入 `pending_approvals[effective_approval_id]`（**先登记后发事件**，避免竞态）
  3. 发 `EventMsg::ExecApprovalRequest(...)`
  4. `rx.await.unwrap_or(ReviewDecision::Abort)` —— 执行 task 挂起直到用户决策；channel 断开默认 `Abort`（**fail-safe**）

- **回传**（`core/src/session/mod.rs:2841` `notify_approval`）：从 map 取出 `tx`，`tx.send(decision)`。找不到就 `warn`（幂等安全）。
  - `approval_id` 语义（`approvals.rs:227`）：命令级审批用 `call_id`；子命令（execve 拦截）才带独立 `approval_id`，`effective_approval_id()` 做 fallback。

- Client 侧提交决策走 `Op::ExecApproval { id, turn_id, decision }` / `Op::PatchApproval` 等（`protocol.rs:585-631`）。

- `clear_pending_waiters()`（`turn.rs:126`）在 turn 结束/中断时一次性清空所有等待者，drop tx → 所有挂起 rx 收到 `Abort`。**干净的取消语义，必须照抄**。

### A4. 决策类型（`protocol.rs:4088` `ReviewDecision`）

```rust
pub enum ReviewDecision {
    Approved,
    ApprovedExecpolicyAmendment { proposed_execpolicy_amendment },  // 批准 + 写规则，未来同前缀免问
    ApprovedForSession,                                             // 本 session 同类免问
    NetworkPolicyAmendment { network_policy_amendment },            // 持久化 allow/deny 某 host
    Denied { rejection: String },                                   // 拒绝但继续 turn（rejection 回给模型）
    TimedOut,
    Abort,                                                          // 拒绝并中止
}
impl Default → Denied{"denied"}   // fail-closed
```

"记住我的选择"三种粒度：session 缓存 / execpolicy 前缀规则 / network host 规则。可用决策集由 `ExecApprovalRequestEvent::default_available_decisions()`（`approvals.rs:293`）按上下文动态生成，服务端下发 `available_decisions`，UI 只渲染这个列表——**决策集是数据驱动的，不是 UI 写死的**。

### A5. 审计 / 记录

- **PII 安全遥测**：`ReviewDecision::to_opaque_string()`（`protocol.rs:4138`）产出 `"approved"` / `"denied"` 等固定串，不含命令内容；`otel.sandbox_outcome(...)` 记 outcome + 耗时。
- **会话持久化**：审批 response item 经 `rollout` crate 落盘，可回放。
- **Guardian（LLM 自动审批员）**：`approvals.rs:126-207` `GuardianAssessmentEvent`——可选的"自动审批"通道，带 `risk_level`(low/medium/high/critical)、`user_authorization`、`rationale`、`status`(in_progress/approved/denied/timed_out/aborted)。把"是否需要人"交给模型评估，产生结构化审计记录。`Op::ApproveGuardianDeniedAction` 允许用户对 guardian 拒绝的动作放行一次。**对 Agent24「24/7 无人值守但可审计」极有参考价值**。

---

## 专题 B — 事件协议

### B1. 两层协议

**内层（`protocol` crate，agent↔session）**：`Submission { id, op: Op }` 进，`Event { id, msg: EventMsg }` 出，`id` 关联请求与事件流。

- `Op`（`protocol.rs:528`）client→agent：`UserInput` / `Interrupt` / `ExecApproval` / `PatchApproval` / `Compact` / `Review` / `Shutdown` …
- `EventMsg`（`protocol.rs:1289`，~90 个变体，`#[serde(tag="type", rename_all="snake_case")]`）：
  - 生命周期：`TurnStarted` / `TurnComplete` / `TurnAborted`(interrupted/replaced/review_ended/budget_limited) / `ShutdownComplete`
  - 流式增量：`AgentMessageContentDelta` / `ReasoningContentDelta` / `PlanDelta` / `ExecCommandOutputDelta`
  - 工具：`ExecCommandBegin/End` / `McpToolCallBegin/End` / `WebSearchBegin/End` / `PatchApplyBegin/Updated/End`
  - **审批（需回包）**：`ExecApprovalRequest` / `ApplyPatchApprovalRequest` / `RequestPermissions` / `RequestUserInput` / `ElicitationRequest` / `GuardianAssessment`
  - item 语义层：`ItemStarted` / `ItemCompleted`（v2 结构化 thread item）
  - 计量与错误：`TokenCount` / `Error` / `Warning` / `StreamError`

**外层（`app-server-protocol`，JSON-RPC 2.0）**：给桌面/IDE/远程 client。`OutgoingMessage`（`app-server-transport/src/outgoing_message.rs:22`）：

```rust
enum OutgoingMessage { Request(ServerRequest), AppServerNotification(...), Response, Error }
```

### B2. 核心设计：审批 = JSON-RPC Request，流式 = Notification

整个协议最重要的一条：

- 流式输出（delta、begin/end）→ `ServerNotification`（`common.rs:1421`），单向无回包。
- 审批 → `ServerRequest`（`common.rs:1251`，宏 `server_request_definitions!` 生成），**server→client 请求，带 `id`，client 必须回 `ServerResponse`**：
  ```rust
  ExecCommandApproval { params: ExecCommandApprovalParams, response: ExecCommandApprovalResponse },
  ApplyPatchApproval  { params: ApplyPatchApprovalParams,  response: ApplyPatchApprovalResponse },
  ```
  `ExecCommandApprovalParams`（`v1.rs:154`）: `conversation_id, call_id, approval_id?, command, cwd, reason?, parsed_cmd`；Response 只含 `{ decision }`。
- 请求-响应关联靠 JSON-RPC `id`，**与内层 oneshot 表解耦**。

### B3. 序列化 / 传输（`app-server-transport`）

- 三种传输并存：`stdio.rs`（IDE 默认）、`unix_socket.rs`（本地 daemon）、`websocket.rs`（axum + tokio-tungstenite）。
- WS：非 loopback 监听**强制 auth**（`auth::authorize_upgrade`，拒绝带 `Origin` 头的浏览器请求防 CSRF），带 `/healthz` `/readyz`。WS 出站 channel 容量 `32*1024`，远大于内部 channel——容忍 client 短暂落后于突发输出。
- 全部同一套 `OutgoingMessage` 编码，传输层无关。

### B4. client 消费模式

`app-server-client` 维护 pending-request 表（对称于服务端 oneshot 表）：收 `ServerRequest` → 弹审批 UI → 按 `id` 回 `ServerResponse`；收 `Notification` → 更新 UI 状态。

---

## 专题 C — TUI

### C1. 技术栈

`ratatui` + `crossterm`（`bracketed-paste`, `event-stream`）+ `tokio` + `tokio-stream`（broadcast/mpsc 转 Stream）。异步 event-stream 驱动，非阻塞。

### C2. 状态管理：单向 `AppEvent` 事件源

中心是 `AppEvent` 枚举（`tui/src/app_event.rs`）+ `AppEventSender`（`app_event_sender.rs`）。所有 UI 变更走"发 AppEvent → 主循环处理"：

- 后端 stream event → UI 内部模型 → 相应 `AppEvent`（如 `ExecApprovalRequest` → `ApprovalRequest` 入队）
- 用户决策 → `AppEvent::SubmitThreadOp { thread_id, op }`（`app_event_sender.rs:80`）回传后端
- 便捷方法 `exec_approval()` / `patch_approval()` 等各自封装成 `SubmitThreadOp`

### C3. 审批弹窗（`tui/src/bottom_pane/approval_overlay.rs`，2383 行）

- `ApprovalRequest` 枚举（`:70`）：`Exec` / `Permissions` / `ApplyPatch` / `McpElicitation`，各带 display model。
- `ApprovalOverlay`（`:171`）持 `queue: Vec<ApprovalRequest>`——**多个并发审批排队**，`enqueue_request` 入队、`advance_queue`（`:477`）弹下一个。后端可乱序并发发审批，UI 串行呈现。
- 每个选项 `SelectionItem` 携带类型安全的 `ApprovalDecision`；`apply_selection`（`:310`）分发决策 → `SubmitThreadOp` 回传 → 插一条历史记录（`:385`）。选项列表来自服务端 `available_decisions`，UI 不硬编码。
- 模块 doc 两条硬契约（`:1`）：**(1) 选择必发显式决策事件；(2) `Esc` 永远映射到 `Cancel`**——避免"关窗 = 静默继续"的危险默认。
- 审批若在流式输出中途到达，可**延后**呈现（`approval_events.rs`），先让输出滚完。
- `Ctrl-C` 中止并清空队列（`:1246` 测试）。

---

## 对 Agent24 的落地建议（approval API + WS schema）

1. **审批用"请求-响应"语义，别用单向通知。** 流式（`model.delta`/`tool.begin`/`tool.end`）走 WS notification（无回包）；审批走带 `id` 的 request，client 必须回 `POST /api/v1/approvals/{id}`，body = `{ decision }`，`{id}` 即 WS `approval.required` 事件里的 `approval_id`。

2. **服务端用 `HashMap<approval_id, oneshot::Sender<Decision>>` per-turn 表挂起工具执行。** 照抄 `request_command_approval`/`notify_approval`：登记 tx → 发事件 → `rx.await`；REST 命中 → `tx.send(decision)`。**默认必须 fail-closed**：channel 断开/turn 取消 → `Abort`/`Denied`；turn 结束 `clear_pending_waiters()` 一次性 drop 所有 tx。

3. **决策类型分层，"可用决策集"由服务端数据驱动下发。** 至少 `approve` / `approve_for_session` / `approve_and_remember(rule)` / `deny(reason)` / `abort`。`approval.required` 事件带 `available_decisions` 数组，UI 只渲染它——未来加"记住 host/命令前缀"规则时客户端零改动。

4. **审批策略抄 `AskForApproval` 四态 + 按来源分桶的细粒度配置；审批与 sandbox 解耦为两道正交闸门。** 编排顺序固定：`审批 → 选沙箱 → 尝试 → 失败按策略升级重试（二次审批带 reason，session 内不重复问）`。

5. **审计写两处、遥测脱敏。** 决策落会话录制（可回放）；另发 PII-free 遥测（只记 `approved`/`denied` + 命令 hash + 耗时）。24/7 无人值守场景参考 **Guardian 模式**：模型做"自动审批员"，产出 `{risk_level, user_authorization, rationale, status}` 结构化记录，低风险自动放行、高风险升级给人——每步可审计。

## 关键文件速查

| 主题 | 位置 |
|---|---|
| 审批机制 | `core/src/session/mod.rs:2265,2841` + `core/src/state/turn.rs:88` |
| 审批/策略类型 | `protocol/src/approvals.rs` + `protocol.rs:{918,1005,1289,4088}` |
| 编排/沙箱重试 | `core/src/tools/orchestrator.rs` + `sandboxing.rs:156` |
| 对外 JSON-RPC | `app-server-protocol/src/protocol/common.rs:{1251,1552}` + `v1.rs:131-170` |
| 传输 | `app-server-transport/src/transport/{websocket,stdio,unix_socket}.rs` |
| TUI | `tui/src/bottom_pane/approval_overlay.rs` + `tui/src/app_event_sender.rs:74` |
