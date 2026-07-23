# SPEC-002: v1 协议规范（数据结构 / REST / WS 事件 / 错误 / 认证）

> `protocol/` 目录是机器可读真源（openapi.yaml + events.schema.json + module.schema.json）；
> 本文档是其人类可读规范。两者冲突时以 `protocol/` 为准并修正本文档。
> 设计依据：ADR-026 §6.5 硬约束 #2/#3/#8；`docs/reference-notes/codex.md` 专题 B。

---

## 0. 总原则

1. **两类消息**：流式/状态变化 = WS **notification**（单向，无回包）；需要用户决策 = **request**（带 id，客户端必须经 REST 回包）。目前唯一的 request 类事件是 `approval.required`。
2. 所有路径带版本前缀 `/api/v1/`。旧的非版本化路由（`/api/modules` 等模块系统）在 M-E 前保持原样并存，不纳入 v1 契约。
3. 命名：JSON 字段一律 `snake_case`；事件类型 `名词.动词过去式/现在时`（`run.started`）；id 均为字符串（ULID）。
4. 时间戳：ISO 8601 UTC 字符串（`2026-07-23T12:00:00Z`）。**可空字段在 wire 上恒出现**（值为 `null`），不允许省略字段——保证 Rust serde 与 TS 生成类型一致。
5. 客户端类型不手写：TS 从 openapi.yaml 生成（`packages/api-client`）。**真源分两阶段**：B1 之前，`protocol/` 手写文件是唯一真源；B1 起，`agent24-protocol` Rust 类型成为生成源（schemars/utoipa 导出并覆盖 `protocol/` 文件），CI 校验「导出结果 == 仓库内文件」零漂移。

## 1. 核心数据结构

### 1.1 Session — 会话（一段与 agent 的持续对话上下文）
```jsonc
{
  "id": "sess_01H…",
  "title": "每日 RSS 摘要",
  "channel": "desktop",          // desktop | cli | tui | schedule | wechat | nostr（后两者 M-F）
  "created_at": "…",
  "updated_at": "…"
}
```

### 1.2 Run — 一次 agent 执行（用户消息或 schedule 触发 → 完成/失败/取消）
```jsonc
{
  "id": "run_01H…",
  "session_id": "sess_…",        // 可为 null：transient run（如 /chat 触发）不隶属任何 session
  "status": "queued | running | awaiting_approval | completed | failed | cancelled",
  "input": { "prompt": "整理下载目录", "model_override": null },
  "output": { "text": "…" },      // completed 时
  "error": { "code": "…", "message": "…" },  // failed 时
  "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0, "cost_usd": 0 },
  "schedule_id": null,             // 由 schedule 触发时非空
  "created_at": "…", "started_at": "…", "ended_at": "…"
}
```
状态机（唯一合法转移）：
```
queued → running → completed | failed | cancelled
running ⇄ awaiting_approval（审批挂起/恢复）
queued|running|awaiting_approval → cancelled（用户取消，取消必须在任意等待点生效）
```

### 1.3 ToolCall — Run 内的一次工具执行
```jsonc
{
  "id": "tc_01H…",
  "run_id": "run_…",
  "tool": "shell_exec",
  "input": { … },                 // 工具入参（审计落库；对外摘要化）
  "status": "running | completed | failed | denied",
  "output_summary": "…",
  "started_at": "…", "ended_at": "…"
}
```

### 1.4 Approval — 审批请求（fail-closed）
```jsonc
{
  "id": "apr_01H…",
  "run_id": "run_…",
  "tool_call_id": "tc_…",
  "kind": "exec | fs_write | network | module",   // 可扩展
  "summary": "执行 shell 命令: rm -rf ~/Downloads/tmp",
  "payload": { "command": ["rm","-rf","…"], "cwd": "…", "reason": "…" },
  "available_decisions": ["approve", "approve_for_session", "deny", "abort"],
  "status": "pending | approved | denied | aborted | timed_out",
  "decision": null,               // 决议后填 { "type": "deny", "reason": "…" }
  "expires_at": "…",              // 超时 → timed_out（等效拒绝）
  "created_at": "…", "decided_at": null
}
```
**Decision 类型**（可扩展；服务端经 `available_decisions` 数据驱动下发，UI 只渲染下发列表）：
```jsonc
{ "type": "approve" }
{ "type": "approve_for_session" }        // 本 session 同类免问
{ "type": "deny", "reason": "…" }        // 拒绝但 run 继续（reason 回给模型）
{ "type": "abort" }                      // 拒绝并取消整个 run
// M-D+: { "type": "approve_and_remember", "rule": { … } }
```
服务端实现约束（硬约束 #2）：`HashMap<approval_id, oneshot::Sender<Decision>>` per-run 表；
sender Drop（run 取消/进程退出）= 挂起方收到 `abort`；`Decision` 的 Default = deny。

### 1.5 Schedule — 挂钟定时工作流（产品灵魂）
```jsonc
{
  "id": "sch_01H…",
  "name": "每日晨报",
  "enabled": true,
  "spec": { "type": "cron", "expr": "0 8 * * *", "tz": "Asia/Shanghai" },
         // 或 { "type": "every", "secs": 3600 }（60 ≤ secs ≤ 86400）
         // 或 { "type": "at", "ts": "…" }（一次性）
  "action": { "type": "agent_run", "prompt": "抓取 RSS 并生成摘要", "session_id": null, "model_override": null },
  "delivery": [ { "type": "desktop_notification" } ],   // M-F 扩展 channel/webhook/email
  "last_run_at": null, "next_run_at": "…",
  "consecutive_failures": 0        // 达到 5 自动 enabled=false 并发事件
}
```
实现约束（硬约束 #7）：`cron` crate + chrono-tz；到期即 pre-advance `next_run_at`；tick `MissedTickBehavior::Skip`；持久化原子写。

### 1.6 Usage / Model
```jsonc
// GET /api/v1/models 条目
{ "id": "Qwen3-8B-4bit", "provider": "omlx", "tier": "local", "loaded": true }
// tier: local | remote | lora（M-D 起路由用）
```

## 2. REST API

Base：`http://127.0.0.1:<port>/api/v1`。M-A mock（node-daemon）固定端口 8765 无 token；
M-B agent24d 起动态端口 + `Authorization: Bearer <token>`（启动时 stdout 输出 ready 行，见 §4）。

| Method | Path | 说明 | 里程碑 |
|---|---|---|---|
| GET | `/health` | `{ status: "ok", version, backend: "node"\|"rust" }` | M-A |
| POST | `/chat` | 直通聊天 `{messages[], model?}` → `{message, usage}`。服务端为每次调用创建 **transient Run**（`session_id` 为 null，不要求先建 session），并照常发出 `run.started` / `model.delta` / `run.completed` 事件（兼容现 UI，长期由 runs 取代） | M-A |
| GET | `/models` | 模型列表（含 tier/loaded） | M-A |
| GET | `/usage` | 累计用量 | M-A |
| POST | `/sessions` / GET `/sessions` / GET `/sessions/{id}` | 会话 CRUD（创建/列表/详情） | M-C |
| POST | `/runs` | `{session_id?, prompt, model_override?}` → Run（202，异步执行；进度走 WS） | M-C |
| GET | `/runs/{id}` · GET `/runs?status=…` | Run 查询 | M-C |
| POST | `/runs/{id}/cancel` | 请求取消（幂等；任何状态可调） | M-C |
| GET | `/approvals?status=pending` | 待审批列表（TUI/UI 轮询兜底） | M-C |
| POST | `/approvals/{id}` | body = Decision；对已决议/过期返回 409 `approval_already_resolved` | M-C |
| POST | `/schedules` | 创建（body = Schedule 去 id/last_run_at/next_run_at/consecutive_failures；`enabled` 默认 true、`delivery` 默认 `[]`）→ 201 Schedule | M-C |
| GET | `/schedules` · `/schedules/{id}` | 列表 / 详情 | M-C |
| PATCH | `/schedules/{id}` | 部分更新（name/enabled/spec/action/delivery 任意子集）→ 200 Schedule（spec 变更即重算 next_run_at） | M-C |
| DELETE | `/schedules/{id}` | 删除 → 204 | M-C |
| POST | `/schedules/{id}/run_now` | 立即触发一次 → 202 `{run_id}`（不改变 next_run_at） | M-C |
| GET | `/tools` | 已注册工具清单（含来源 builtin/mcp/module） | M-C |

## 3. WS 事件协议

端点：`GET /api/v1/events`（Upgrade: websocket；同 REST 认证）。

**信封**（所有消息统一）：
```jsonc
{ "v": 1, "seq": 42, "ts": "…", "type": "run.started", "payload": { … } }
```
`seq` 为连接内单调递增；客户端检测跳号后用 REST 全量对账（v1 不做断线重放）。

**Notification（单向，无回包）**：

| type | payload 要点 |
|---|---|
| `run.started` | `{ run_id, session_id, schedule_id }`（后两者恒出现、可为 null：transient run 的 session_id 为 null；非 schedule 触发的 schedule_id 为 null） |
| `run.completed` | `{ run_id, output, usage }` |
| `run.failed` | `{ run_id, error }` |
| `run.cancelled` | `{ run_id }` |
| `model.delta` | `{ run_id, text }`（流式文本增量） |
| `tool.started` | `{ run_id, tool_call_id, tool, input_summary }` |
| `tool.completed` | `{ run_id, tool_call_id, status, output_summary }` |
| `approval.resolved` | `{ approval_id, run_id, decision_type }`（多客户端同步收敛） |
| `schedule.fired` | `{ schedule_id, run_id }` |
| `schedule.disabled` | `{ schedule_id, reason }`（reason 为开放枚举，当前唯一取值 `consecutive_failures`） |

**Request（必须回包，经 REST）**：

| type | payload | 回包途径 |
|---|---|---|
| `approval.required` | Approval 对象全文（§1.4，含 `available_decisions`、`expires_at`） | `POST /api/v1/approvals/{id}` |

实现约束（硬约束 #8）：Rust 侧事件为 `#[serde(tag = "type")]` 强类型 enum，**每个变体显式 `#[serde(rename = "run.started")]` 式点分命名**（注意：`rename_all = "snake_case"` 会错误产出 `run_started`，禁止依赖它命名事件）；
TS 侧类型由 `protocol/events.schema.json` 生成。**禁止任何一侧手解析无类型 JSON。**

## 4. 认证与进程握手（M-B 起）

```
Electron/CLI spawn: agent24d serve --port 0
agent24d stdout 输出一行: {"type":"ready","port":49317,"token":"<32B 随机>","version":"…"}
之后所有请求: Authorization: Bearer <token>
```
解析方约定：**逐行扫描 stdout，取首个 `type=="ready"` 的 JSON 行**——不要求它是绝对首行
（daemon 初始化日志可能在其之前，如 node mock 的 BoxLite 绑定加载日志）。
- 只绑定 `127.0.0.1`；token 每次启动重新生成；拒绝带浏览器 `Origin` 头的 WS 升级（防 CSRF）；**唯一免认证端点：`GET /api/v1/health`**（存活探测）
- mock（node-daemon）豁免 token 但必须实现同样的 ready 行（token 可为空串），保证 BackendManager 逻辑统一

## 5. 错误格式

```jsonc
// HTTP 4xx/5xx 统一 body
{ "error": { "code": "invalid_request", "message": "prompt is required", "details": { } } }
```
错误码（可扩展）：`invalid_request` `unauthorized` `not_found` `conflict`
`approval_already_resolved` `provider_unavailable` `run_not_cancellable`(保留) `internal`。
WS 层错误用 `run.failed` / close code，不另设错误信封。

## 6. 兼容与演进规则

- v1 内只做**加法**（新端点/新可选字段/新事件类型）；破坏性变更升 v2 前缀
- 客户端必须忽略未知事件 type 与未知字段（前向兼容）
- `available_decisions`、`kind`、`tier` 等枚举视为开放集合，客户端对未知值需有兜底渲染
