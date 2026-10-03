# ADR-002：`a24.workspace.v1`

> 状态：Accepted / P0 frozen
>
> 覆盖：A24-OD-01、A24-OD-02

## 1. Authority 与适用范围

Agent24 workspace registry 是 workspace identity、root、生命周期和 run binding 的唯一事实来源。ACP、Electron 和 Open Design 只消费 opaque `workspace_id`；它们不得注册任意路径、重新解释 ID 或维护平行 root 映射。

v1 只支持：

```text
kind = orchestrator_scratch
writeback_policy = external
concurrency_policy = serial
default TTL = 24 hours
maximum TTL = 7 days
```

`external` 表示 Agent24/Open Design 可在 scratch 内生成 artifact，但 source writeback、发布和部署必须由 workspace 外的显式流程完成。

## 2. 公开领域模型

```rust
Workspace {
    id: WorkspaceId,                   // ws_<ULID>; opaque
    kind: orchestrator_scratch,
    state: active | expired | releasing | released | cleanup_failed,
    provenance: {
        source: String,
        project_ref: Option<String>,
        base_revision: Option<String>,
    },
    writeback_policy: external,
    lifecycle_owner: {
        kind: orchestrator,
        reference: String,
    },
    concurrency_policy: serial,
    created_at: RFC3339,
    expires_at: RFC3339,
    renewed_at: Option<RFC3339>,
    released_at: Option<RFC3339>,
    revision: u64,
}
```

内部记录额外包含 `canonical_root`、root generation、cleanup/quarantine 信息和 active leases；这些内容绝不出现在普通 REST、WS、ACP、renderer IPC 或错误消息中。`lifecycle_owner` 是清理归因，不是额外授权主体。

## 3. Product REST/OpenAPI

```text
POST /api/v1/workspaces
GET  /api/v1/workspaces?state=...
GET  /api/v1/workspaces/{workspace_id}
POST /api/v1/workspaces/{workspace_id}/renew
POST /api/v1/workspaces/{workspace_id}/release
```

创建请求只接受 kind、provenance、writeback policy、lifecycle owner 和 bounded TTL；不接受 root/path。registry 在 Agent24 state directory 下原子创建 root，并保存 canonical identity。

`release` 返回 `202`，语义幂等。它不承诺同步删除目录；调用方根据 workspace state 观察结果。

## 4. 本机 bridge/host 接口

有两种受限接口，均不是 workspace authority 的替代品：

1. **ACP resolve**：持有 `creative_runtime` capability 的本机 bridge 可提交 ACP 标准提供的 absolute `cwd`，daemon canonicalize 后只与 capability 已绑定、active 的 registry root 做**精确匹配**并返回同一 `workspace_id`。它不创建 workspace，不返回 root，不接受 descendant 自动授权。相对路径、不存在路径、歧义、symlink escape 和其他 workspace 全部拒绝。
2. **Host mount lease**：Electron main 使用 daemon ready pipe 中只交给原始 parent、且不落 discovery 文件的 `product_host` capability 获取 `{lease_id, workspace_id, canonical_root, host_instance_id, expires_at}`。discovery、`creative_runtime`、renderer backend proxy 和带浏览器 Origin 的请求不能调用。root 只停留在 Electron main 和受管 Open Design sidecar handoff。

推荐本机路由：

```text
POST /api/v1/bridge/workspaces/resolve       creative_runtime + no Origin
POST /api/v1/host/workspaces/{id}/leases    product_host + no Origin
POST /api/v1/host/workspace-leases/{id}/renew
DELETE /api/v1/host/workspace-leases/{id}   product_host + no Origin
```

host lease 是短租约：默认 30 秒 heartbeat、90 秒 expiry，绑定随机 app `host_instance_id` 和 daemon generation。Electron crash 或断连后不需要不可靠的进程名扫描；lease 到期后 registry 才允许 cleanup。daemon restart 不自动信任旧 host lease，Electron 必须以当前 host capability 和同一 workspace 重新 attach；旧 generation 的 lease 在 grace period 后过期。所有 renew/release 都校验 lease、instance 与 generation，迟到请求不能复活旧 lease。

`cwd` 在 resolve 中只是“匹配 capability 已绑定 root”的输入，不授予路径；run API 始终只接受 `workspace_id`。Electron 的通用 `backendProxy` 必须显式拒绝 `/bridge/`、`/host/`、`/capabilities/` 和任何未来的 authority route。

MVP 要求 Open Design project cwd 等于 scratch root。允许 descendant cwd、多个 mount 或远端 root 属于后续 minor contract，不能由 bridge 自行采用最长前缀规则。

## 5. Run 与 Session binding

公开加法字段：

```text
RunCreate.workspace_id: string | null
RunInput.workspace_id: string | null
Run.workspace_id: string | null
SessionCreate.workspace_id: string | null
Session.workspace_id: string | null
```

规则：

- Creative/ACP run 必须显式传 active `workspace_id`；从 prompt、`cwd`、`_meta`、channel 或 session title 推导均禁止。
- 可选的 `Session.workspace_id` 是 immutable scope，不是默认推导。ACP 创建 workspace-bound Session；其每个 Run 仍显式传相同 ID。
- 任何显式非 legacy workspace Run 都必须使用同 workspace 的 bound Session；unbound Session 只能创建 legacy Run。若产品以后需要 bind-once，必须在无历史 Run 时以单事务完成，v1 不实现。这样禁止同一 session memory 从 workspace A 流入 B。
- Creative Session 在内部额外保存 ADR-005 的 `creative_attachment_id/creative_principal_id`；Run 原子继承。该 ownership 不向普通 UI 作为 authority 字段暴露，也不能由 RunCreate 覆盖。
- 旧 CLI/TUI/channel/schedule 请求省略字段时，映射到持久化 `legacy_compat` workspace；不移动现有 `~/.agent24/workspace`。
- 历史 Run 的 null 表示 contract 前 legacy。migration 必须把**所有非终态 Run（包括 `awaiting_approval`）及其 pending approval/tool call**原子回填到 singleton `legacy_compat` workspace，并创建可恢复 lease；否则 daemon 不得启动。新 legacy Run 在内部绑定 singleton legacy handle，并在审计中可识别。
- `/chat` 继续是无工具 transient path，保持 nullable 兼容。

同一 workspace 的 `serial` policy 只允许一个非终态 Run lease；第二个请求稳定返回 `409 workspace_busy`。不同 workspace 的 Run 可并行。

## 6. 不可伪造的运行时 capability

```rust
pub struct WorkspaceHandle {
    // private: workspace id, generation, canonical root, pinned cap-std Dir
}

pub struct ToolContext {
    pub run_id: String,
    pub session_id: Option<String>,
    pub schedule_id: Option<String>,
    pub tool_call_id: String,
    pub workspace_id: String,
    pub(crate) workspace: WorkspaceHandle,
}
```

registry 在 Run 原子绑定时创建 immutable `WorkspaceHandle`，RunManager 持有到 run terminal。工具不能从字符串自行构造 handle。

- `fs_read/fs_write` 使用 handle 中 pinned dirfd 和 beneath-only primitive；禁止退回 `canonicalize → check → open`。
- `shell_exec` cwd 固定为 handle root；explorer/subagent/self-wake 继承同一个 handle。
- MCP/network 工具即使不用 root，也携带 workspace ID 用于 policy/audit。
- v1 的 shell 约束只保证 cwd 和 approval scope；未引入 OS sandbox 前，不宣称任意 shell command 无法访问 root 外的主机路径。

## 7. Approval 与 audit scope

`ApprovalRequest`、tool call、standing grant 和 durable grant 增加 nullable `workspace_id`。`WriteLocal`/`Exec` 的 session/schedule grant 必须同时匹配 workspace；旧 grant 迁移到 `legacy_compat` scope，不能对所有新 workspace 生效。

相关 run/tool/approval 事件增加 additive nullable `workspace_id`。可选 workspace lifecycle 事件：

```text
workspace.created
workspace.expired
workspace.release_requested
workspace.released
workspace.cleanup_failed
```

事件和 hash-chain audit 只记录 ID、kind、state、owner ref、run/tool/approval correlation 和脱敏 reason，不记录 root、token 或未脱敏 tool input 到普通 UI。

## 8. 状态、lease、TTL 与恢复

```text
active ── release ──> releasing ── no leases ──> released
   └──── TTL ───────> expired ───── no leases ──> released
cleanup error ──────> cleanup_failed ── retry ──> releasing
```

- `expired/releasing/cleanup_failed/released` 均拒绝新 Run 和 host lease。
- 已存在的 Run/host lease 可完成；TTL 和 release 不得删除 active lease 的 root。
- Run terminal 持久化后才释放 Run lease；host 完成 detach 后释放 host lease。
- cleanup 先把 root 原子移动到 Agent24 管理的 quarantine，再异步删除；tombstone 保留用于审计。
- 每次 Run lease、host lease 和 renew 都在同一事务内执行 lazy expiry CAS：若 `expires_at <= now`，先把 active 改为 expired，再拒绝请求；不能依赖后台 sweeper 是否准时。
- daemon restart 先恢复 registry 和 Run leases，再根据数据库 Run 状态 reconcile；host leases 还必须经过上一节的 generation/expiry 规则。目录扫描不能成为删除依据。
- registry 持久化 root identity。Unix 至少记录并核对 `dev+ino`，Windows 记录 volume serial + file ID；restart 安全重开 root 后 identity 或 generation 不匹配即 fail closed/quarantine，不能仅凭相同路径恢复 handle。
- orphan 的非终态 Run 按既有 recovery policy 进入明确终态后才能解绑；失败时宁可保留 lease/root。

`Run create + workspace state check + serial lease`、`Run terminal + unbind`、`release/expire state change` 都必须在 SQLite 写事务中完成。

## 9. Store migration

只能追加 migration：

- `workspaces`：公开字段 + internal root/generation/cleanup；
- `workspace_leases`：`workspace_id, lease_id, kind(run|host), owner_id, generation, acquired_at, released_at`；
- `runs.workspace_id` nullable column；
- `sessions.workspace_id` nullable immutable scope；
- `creative_attachments` / `creative_principals` durable entitlement，以及 Session/Run 的 nullable internal owner principal；
- approval/standing grant 的 nullable workspace scope；
- singleton `legacy_compat` 记录与旧 root 映射；同一事务回填全部历史 Run，并为非终态/等待审批 Run 创建 active legacy lease、补齐 approval workspace scope。

建议 partial unique index 保证 `serial` workspace 只有一个 active Run lease。migration 失败必须 fail closed，不能退回全局 root 执行新 Run。

## 10. 错误 contract

沿用 `{error:{code,message,hint?,details?}}` 开放 code：

| HTTP | code | 含义 |
| ---: | --- | --- |
| 400 | `workspace_invalid` | 字段、ID、TTL 或 resolve cwd 非法 |
| 404 | `workspace_not_found` | ID/resolve root 未登记 |
| 409 | `workspace_expired` | 已过期 |
| 409 | `workspace_released` | releasing/released，不再接受新 lease |
| 409 | `workspace_busy` | serial workspace 已有 active Run |
| 409 | `workspace_binding_conflict` | Run/Session/lease identity 不一致 |
| 400 | `workspace_ttl_exceeded` | 超过最大 TTL |
| 503 | `workspace_unavailable` | root 无法安全打开 |
| 500 | `workspace_cleanup_failed` | quarantine/cleanup 失败 |

`details` 只允许 ID、state、run/lease ID、retry hint 和脱敏 reason。

## 11. 验收门禁

- 两个不同 workspace 的并行 Run 读写互不可见；同 workspace 第二个 Run 返回 `workspace_busy`。
- public create/run/IPC 不接受 path；bridge resolve 不能注册新 root，host route 不能被普通 bearer/renderer 调用。
- `..`、绝对越界、escaping symlink、parent swap 和 delete/recreate generation 全部失败。
- release/TTL 与 Run create 竞争时不存在删后绑定；active Run/host lease 不被删除。
- daemon restart 后 binding、approval resume 和 cleanup 精确恢复；不误删另一个 workspace。
- sweeper 被故意暂停时，已过 `expires_at` 的 workspace 仍不能取得任何新 lease；restart 后目录 delete/recreate identity mismatch 必须失败。
- workspace A 的 approval/grant 不能用于 B。
- shell cwd 正确且仍 fail-closed approval；测试明确记录它不是 OS sandbox。
- OpenAPI、Rust、TS、events schema、fixtures、contract tests 零漂移。

## 12. 分支边界

`feat/a24-workspace-registry` 负责 registry、root、REST、本机 bridge/host capability、TTL/cleanup/recovery 和 fixtures；不修改 RunManager 工具注入。

`feat/a24-run-workspace-binding` 必须基于前者，负责 Run/Session/store binding、WorkspaceHandle、fs/shell/explorer/subagent/self-wake、approval/grant、events/audit 和兼容入口。两个分支不得各自定义不同 Workspace 类型。
