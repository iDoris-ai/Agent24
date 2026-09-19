# ADR-003：`agent24.open-design.v1` ACP bridge

> 状态：Accepted / P0 frozen
>
> 覆盖：A24-OD-03、OD-02
>
> 依赖：`a24.workspace.v1`

## 1. 角色与 transport

```text
Open Design daemon (ACP Client)
└─ spawn bundled `agent24 acp` (ACP Agent)
   ├─ stdin  ← JSON-RPC requests/notifications
   ├─ stdout → JSON-RPC responses/notifications only
   └─ stderr ← diagnostics only
```

固定 ACP v1、JSON-RPC 2.0、newline-delimited UTF-8 stdio；每行一个 message，不支持 batch 或嵌入换行。stdout 出现日志即为 contract failure。

bridge 采用 `attach_only`：Open Design adapter 用 additive `RuntimeContext.conversationId` 经 owner-authenticated broker 定位 project root 之外的 host-private runtime handoff；bridge 从中读取 endpoint、版本和短期 `creative_runtime` capability，验证 workspace/attachment/principal/session/daemon/host/sidecar generation/expiry 后连接现有 `agent24d`。capability mode 的 `daemon.json` 根本不含 token，bridge 更不能获得 `product_host`。handoff 不存在、过期、被 revoke 或认证失败就返回确定错误；禁止调用 CLI 的 ephemeral fallback，禁止静默启动第二个 control plane。

## 2. Capability advertisement

首版必须支持：

- `initialize`
- `session/new`
- `session/prompt`
- `session/cancel`
- `$/cancel_request`
- `session/set_config_option`，唯一稳定 option 为 `model`
- `session/update`

`session/load` 只有在第 5 节 transcript contract 已实现并通过 replay fixture 后才能广告 `loadSession=true`。未实现时明确广告 false，不能返回空历史伪装成功。`session/resume`、`authenticate`、`logout` 首版不广告。

固定 pin `open-design-v0.22.2@7395321` 另有必须显式兼容的 profile：Open Design runtime def 设置 `resumesSessionViaAcpLoad: true`；其 generic engine 会从 `session/new/load` 顶层读取 `openCodeSessionId` 作为 durable handle，并在 `session/load` response 中额外要求 `sessionId`。因此 `agent24.open-design.v1` 在标准 ACP 字段之外返回：

```json
{
  "sessionId": "sess_...",
  "openCodeSessionId": "sess_..."
}
```

两个值在 v1 均等于 Agent24 session ID。该扩展只为已 pin 的 generic engine interop，必须由真实 Open Design fixture 覆盖；上游 engine 移除非标准要求后可在 profile major/minor 迁移中撤掉，不能假设所有 ACP Client 都需要它。

`session/new/load.mcpServers` 必须为空；非空 fail closed 为 `unsupported_mcp_servers`。Open Design 不能借 ACP 注入 command/env/MCP，工具唯一来源是 Agent24 registry + policy。

## 3. Workspace 与 session mapping

```text
ACP sessionId = Agent24 session.id
ACP session 1:1 immutable workspace_id
每个 session/prompt = 一个 Agent24 Run
Run.session_id = ACP sessionId
Run.workspace_id = ACP session workspace_id
```

`session/new`：

1. 验证 `cwd` 为 absolute、存在且无 NUL；
2. 调用 ADR-002 bridge resolve，只允许精确匹配 `creative_runtime.workspace_id` 对应的 active `orchestrator_scratch` root；
3. 创建 `Session(workspace_id=resolved_id, channel=open_design)`；
4. 返回 Agent24 session ID；
5. 后续 prompt 永远显式传同一 workspace ID。

`cwd`、prompt、resource `_meta` 均不能覆盖 workspace。一个 ACP session 同时只允许一个 active prompt，冲突返回 `session_busy`。

`session/load` 必须同时验证：Session 存在且 workspace-bound；Session owner principal 等于 capability principal；新 cwd resolve 到同一 ID；transcript 可完整 replay。任何一项失败都不创建新的隐式 session。token/sidecar/daemon rotation 可 remint 同 principal 并继续 load，但相同 workspace 的另一 conversation principal 不可访问。

## 4. Prompt content v1

- 支持 `text` block，按原顺序合成为 Agent24 prompt。
- 支持 workspace 内 `resource_link`：只接受可解析为当前 workspace-relative resource 的 URI，由 bridge 输出相对引用；绝不把 canonical host path写入 prompt/event。
- `image`、`audio`、embedded binary 和 workspace 外 resource 首版返回 `unsupported_content_type`；不能静默丢弃。
- `_meta.agent24` 只可承载 correlation/status/usage，不可授权、改 workspace 或批准工具。

后续增加 Agent24 structured multimodal RunInput 属于兼容 minor 版本；在该字段进入主干前，Open Design adapter 不能宣称 image input 已由 Agent24 runtime 支持。

## 5. Transcript/load contract

P3 必须在 Agent24 authority 内提供只读 transcript，例如：

```text
GET /api/v1/sessions/{session_id}/transcript
```

返回稳定排序的 user prompt、assistant final content、必要的 tool/status summary、run ID、workspace ID 与 terminal state；不返回 secret、绝对路径或未脱敏 tool input。内容由现有 Run/run_messages/tool records 生成，不建立 bridge 私有历史数据库。

`session/load` 先按 transcript replay `session/update`，再完成 load response。若历史不完整、workspace 不匹配或存在未知 contract version，fail closed。WS delta 不需要逐 token 原样重放，但最终 assistant content、turn 顺序和 terminal state 必须一致。

## 6. Run/WS 映射

bridge **先订阅 WS，再创建 Run**，通过 run/session/workspace ID 过滤：

| Agent24 | ACP 输出 |
| --- | --- |
| `run.started` | bounded status；建立当前 run correlation |
| `model.delta` | `session/update: agent_message_chunk` |
| `tool.started` | `session/update: tool_call`，pending/in_progress |
| `tool.completed` | `tool_call_update`，completed/failed |
| `approval.required` | pending tool/status + approval correlation |
| `approval.resolved` | 对应 tool status；Agent24 决定继续/拒绝 |
| `run.completed` | prompt response，`stopReason=end_turn` + usage meta |
| `run.cancelled` | prompt response，`stopReason=cancelled` |
| `run.failed` | JSON-RPC application error |

只发送 Agent24 已脱敏的 `input_summary/output_summary`；不把完整 tool input、token 或绝对路径传给 Open Design。

ACP v1 的 `ToolCallStatus` 只有 `pending | in_progress | completed | failed`。Agent24 denied 必须映射成 `failed`，并在脱敏 content/`_meta.agent24.denial` 中标注 `denied` reason；不得发送非 schema 的 `denied` status。

`approval.required` **绝不**转换为 ACP `session/request_permission`。Open Design 当前可能自动选择 allow-style option；该行为不能成为 Agent24 授权。唯一审批入口仍是 Agent24 host 的 approvals API/UI，Creative 只显示“等待 Agent24 审批”。

daemon auth middleware 还必须从服务端拒绝 `creative_runtime` 的 approval decision、grant/override、capability mint/revoke、host lease、workspace create/release、shutdown 与其他 workspace/session/run；不能只依赖 bridge 不发这些请求。事件流按 capability resource claims 过滤。

## 7. Reconcile 与取消

WS v1 无 replay。seq gap、lag 或断线时：

1. 从 host-private runtime directory 重新读取 handoff；只接受同 workspace 与 daemon/host/sidecar generation 的续期 `creative_runtime` capability；
2. `GET /runs/{run_id}` 对账；
3. terminal 则用权威 final output/usage 合成终态；
4. non-terminal 则重新订阅，只继续未来事件；
5. 对丢失 delta 不猜测、不重复拼接，发送 bounded `reconnected/reconciled` status。

因此 v1 保证恢复 state/final result，不保证补发断线窗口内每个增量 token。该限制写入 compatibility matrix 和 fixture。

官方 `session/cancel` notification 与 `$/cancel_request` 对 active Run 只调用 Agent24 cancel；不得直接 kill daemon。固定 Open Design pin 会错误地把 `session/cancel` 作为带 id request 发送，bridge 的 pin-specific compatibility profile 必须接受并返回空 success response，同时执行完全相同的取消语义；其他 request-shaped notification 不做宽松处理。bridge 等待或 reconcile 到 terminal。bridge 进程收到 shutdown 时尝试取消其 active Run，然后退出；超时仍不能终止产品 daemon。

## 8. Model 与 usage

`session/set_config_option(model)` 的 options 来自 `/api/v1/models`。`auto` 省略 override，使用 Agent24 默认 routing；其他值必须在创建 Run 前验证，未知值返回 `model_not_found`。

Agent24 usage 为权威：`prompt_tokens, completion_tokens, total_tokens, cost_usd`。首版放入 prompt response 的 `_meta.agent24.usage`；不伪造 ACP unstable context-window usage。ACP 将来提供稳定 usage 字段时做 additive minor mapping。

## 9. 错误 contract

标准 JSON-RPC code：parse `-32700`、invalid request `-32600`、unknown method `-32601`、invalid params `-32602`、internal `-32603`、cancel `-32800`。

application `data.kind`：

```text
workspace_not_registered
workspace_mismatch
workspace_unavailable
daemon_unavailable
daemon_auth_failed
session_busy
session_not_loadable
model_not_found
unsupported_mcp_servers
unsupported_content_type
run_failed
```

错误 data 不得含 token、canonical path、SQL、provider credential 或完整 tool input。

## 10. Open Design adapter 的唯一知识面

```ts
{
  id: 'agent24',
  name: 'Agent24',
  bin: 'agent24',
  buildArgs: () => ['acp'],
  streamFormat: 'acp-json-rpc',
  versionArgs: ['--version'],
  resumesSessionViaAcpLoad: true
}
```

packaged host 把该 `bin` 解析到 pin 匹配的 bundled CLI；不允许系统 PATH 偶然覆盖。adapter 不知道 Agent24 URL/token、workspace registry/root、approval API、model provider 或 Electron sidecar。

## 11. Fixture 门禁

至少覆盖：initialize/unsupported version；registered/relative/symlink/unregistered cwd；session/new 的 non-empty MCP 拒绝与 pinned session/load 缺省空 MCP兼容；model success/unknown；text/resource/unsupported media；两轮 prompt；tool start/completion/denied→failed schema validation；approval pending 后 Agent24 host 批准；OD auto-allow 不生效；官方 cancel notification 与 pinned cancel request；queued/running/awaiting-approval cancel；provider failure；WS gap + REST terminal/nonterminal reconcile；daemon restart/token rotation；load replay 与不完整历史；pinned OD `resumesSessionViaAcpLoad/openCodeSessionId/sessionId` round-trip；malformed/unknown/notification；stdout purity；usage mapping。最终门禁必须直接驱动 pin 的 Open Design generic ACP engine，不能只用 bridge 自建 JSON fixtures。

P4 只有在这些 fixture 对 `agent24 acp` 通过后，才能给 Open Design 注册 runtime。若 Open Design 通用 `acp-json-rpc` engine 无法消费本 profile，必须回到设计门禁，不能在 fork 内复制一个 Agent24 专用 parser。
