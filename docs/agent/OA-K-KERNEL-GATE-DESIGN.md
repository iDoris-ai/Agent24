# OA-K：模块请求、内核执行的外部动作门（详细设计）

状态：**设计提议（Draft）**，待内核 ADR、待实现。本文没有任何部分已实现；下文所有接口形状、表结构、方法名、错误码都是**提议**。
日期：2026-10-16。作者：Claude Opus 5.5（`claude-opus-5-5`），受作者委托。
基线：本 worktree HEAD `4b424883`（分支 `docs/oa-k-design`）。行号都按这个提交。核对不了的地方写明"未核实"。
上游：[`OFFICE-ASSISTANT-DESIGN-DISCUSSION.md`](OFFICE-ASSISTANT-DESIGN-DISCUSSION.md) §5.3 风险 1/2、§5.4；[`PLAN-OFFICE-ASSISTANT.md`](PLAN-OFFICE-ASSISTANT.md) D7。
归属：**#735（K1 内核依赖）**，不属于 OA 业务线。**不改变主线（C3 记忆 P1）的优先级。**

> 一句话：模块交给内核一份 `(tool, 确切 input, action_key)`。内核把它存成一条有约束力的 `ModuleApproval`（`kind = gate`），请人逐次审批这份确切输入。批准后内核自己执行一次，过出站门，写哈希链审计，返回类型化回执。内核不自动重试。

---

## 0. 术语

| 词 | 本文含义 |
|---|---|
| **OA-K 动作**（action） | 一次"模块请求、内核执行"的外部效果调用。对象是**内核注册表里的一个工具**加上一份确切 input |
| `action_key` | **模块自选的业务幂等键**（新增）。不是 `request_id` |
| `request_id` | 内核为每个代理请求、每次内核调用铸造的**关联 id**（现有），配一枚一次性 `approval_token` |
| 审批记录 | 复用现有 `module_approvals` 行（`kind = gate`、`binding = true`） |
| 执行记录 | **新增**的 `module_action_executions` 行，与审批记录 1:1 |
| 回执（receipt） | 内核返回的类型化执行结果。**只表示"内核这边看到了什么"，不表示对方已收到** |

---

## 1. 问题陈述：今天模块驱动的外部动作没有约束性批准原语

### 1.1 证据

1. **`Gate` 的闭集里只有 `schedule_callback`。** `validate_gate_action` 不是这个动作就直接返回 `ActionNotInClosedSet`（`rust/apps/agent24d/src/module_approval_broker.rs:71-88`）。闭集检查在消耗 token 之前做（`rust/apps/agent24d/src/approval_callback.rs:179-197`），映射成 wire 上的 `forbidden`（`:112-114`）。"执行"`schedule_callback` 只是周期扫描把 `executed_at` 置上（`module_approval_broker.rs:536-550`；SQL 见 `agent24-store/src/module_approvals.rs:289-293`），内核今天不替模块执行任何外部动作。
2. **`Advise` 没有约束力。** 类型注释原文是"the kernel records and presents it, but does not execute it and does not guarantee the module honors the answer"（`rust/crates/agent24-protocol/src/types.rs:563-566`）。`payload` 字段注释写得更直白："for `Advise`, this is knowledge, not a safety control"（`types.rs:611-614`）。`binding` 对 `Advise` 恒为 `false`（`module_approval_broker.rs:373`）。
3. **模块工具的 consent 在生产路径上写不进去。** `grant_module_consent` 的调用点全在 `#[cfg(test)]` 下（`docs/agent/K1-RELEASE-NOTES-20261016.md:27`）。宿主 HTTP 只有列表、导出、撤销（同文件 `:22-26`）。而且 `ModuleTool` 是"内核调模块"的方向（`rust/apps/agent24d/src/module_tool_runtime.rs:79-117` 发 `POST /api/v1/{module}/_a24/tools/{op}`），和"模块请内核去做"正好相反，即使 consent 写得进去也补不上这个缺口。
4. **工具流水线只有模型驱动的入口。** `ToolRegistry::dispatch`（`rust/crates/agent24-tools/src/lib.rs:1329-1402`）要一个带 `run_id`、`tool_call_id` 的 `ToolContext`（`:39-50`）。`approvals` 表和 `tool_calls` 表的 `run_id` 都是 `NOT NULL REFERENCES runs(id)`（`rust/crates/agent24-store/migrations/0001_initial.sql:28-54`）。ADR-DOC-01 也写明"模块也不能调用 Agent 工具"（`docs/documenting/adr/ADR-DOC-01-placement-and-integration.md:53`）。
5. **附着模块连 `Advise` 都提交不了。** 附着代没有在途请求，`admit_request` 对 `Attached` 无条件拒绝（`rust/crates/agent24-os-proto/src/drain.rs:479-481`），所以不存在可用的 `request_id`/`approval_token`；`ATTACH_OOP_GRANTS` 也只有 Events/Models（`rust/apps/agent24d/src/attach_registry.rs:155`）。

### 1.2 后果

今天写一个"发邮件 / 写 Notion"的模块，最顺手的实现只有两种：

- 用 `Advise` 记一笔，模块自己看批准结果、自己发；
- 跳过内核，模块直接发。

这两种实现里，"批准绑定最终 payload"都只是文案。外发发生在模块进程里，ADR-K1-02 §2.4 列的出口门（`docs/adr/ADR-K1-02-session-dataflow-security-modes.md:57-65`）管不到。同 UID 的威胁模型本来就不防"合作但有 bug"的模块多发一封邮件（`docs/ARCHITECTURE-LAYERS.md:179`）。

### 1.3 目标与非目标

**目标**

- G1：外部效果只在内核执行，模块进程里不放外发代码。
- G2：人审的对象是**确切 input**，由内核渲染。模块给的自然语言只作附注。
- G3：payload 一变，旧批准就不能用来执行新内容。
- G4：执行前过出站门（K1-6b.3）；没接通就 fail-closed。
- G5：每次提交、决定、派发、终态都写进哈希链审计。
- G6：未知结果如实报 `result_unknown`，内核不重试。
- G7：形状对四类扩展面通用，内核里不出现 office/mail/notion 这类领域名词（与 T11、ADR-DOC-01 D4 一致）。

**非目标**（本设计**不做**）

- 常驻授权、`approve_for_target`、批量批准（要先单独改法律，见 §5.4）。
- 模块经 OA-K 调**读**类工具，或调另一个模块的工具（跨 OS 读取是 ADR-DOC-01 §3 的另一项，`:216`）。
- `shell_exec`、`fs_write`、通用 `http_fetch` 走 OA-K。
- 让 sidecar 用 OA-K。

---

## 2. 接口设计（提议，待 ADR）

### 2.1 总体选型：复用审批记录，新增执行记录，不复用 Run / ToolCall

| 内核状态机 | OA-K 复用还是新增 | 理由 |
|---|---|---|
| **Run**（`RunStatus`，`agent24-core/src/transitions.rs:25`） | **不复用，不建 run** | OA-K 动作没有 agent loop、没有模型轮次、不进 run 事件流。为它造一个假 run，会把"模块动作"混进会话历史，还会被 `mem_events` 记下原文（讨论稿 §5.3 风险 4） |
| **Approval**（run 绑定的 `approvals` 表 + `agent24-policy` 阻塞式 broker） | **不复用** | `run_id NOT NULL REFERENCES runs(id)`（`0001_initial.sql:42`）；交互模型是同步阻塞等待。模块回调受请求生命周期约束（`approval_callback.rs:212-231`），人审可能要几分钟，挂不住 |
| **ModuleApproval**（`module_approvals`，T7b/T7c） | **复用作审批记录** | 已有：异步"提交后轮询"、32 字节随机 id、内核算 `payload_digest`、`expires_at → timed_out`、`binding`、REST 决定路由（`server.rs:1135-1139`）、WS 推送（`module_approval_broker.rs:327-340`） |
| **ToolCall**（`tool_calls` 表） | **不复用行**，复用**执行半段**的代码 | 行要 `run_id` FK（`0001_initial.sql:30`）。复用 `ToolRegistry` 的白名单 + 限时执行（`lib.rs:1414-1434` 的 `execute_preapproved` 先例、`:1486` 起的 `run_budgeted`），以及 K1-7.1 的 PreDispatch/Terminal 审计模式（`agent24-store/src/module_audit_event.rs:402-415`） |
| **新增：ActionExecution** | **新增**（第四条持久状态机） | 现有表里没有地方放"执行到哪了、结果是什么、未知"。迁移移植进 `agent24-core` 的迁移表（与 ADR-026 一致），见 §2.6 |

**为什么不直接在 `_a24/approval/gate` 的闭集里加一项。** 现有授予了 `approval` 的模块，会因此**静默获得**"让内核替自己外发"的能力，等于权限升级。同一种 `Capability` 也说不清"能请人审批"和"能触发外发"是两件事。所以：

- **新增一个 `Capability`（暂名 `actions`）和一组方法 `_a24/action/*`**。规模与"给 `Capability` 加新能力"相同（`ARCHITECTURE-LAYERS.md:256`），必须先有 ADR。
- 审批记录复用 `module_approvals`，`action` 字段写保留值 `oak.tool`（暂名）。**`validate_gate_action` 必须继续对 `oak.tool` 返回 `ActionNotInClosedSet`**，这样旧的 `_a24/approval/gate` 路径造不出 OA-K 行，校验也绕不过去。OA-K 用自己的校验函数。

### 2.2 方法族（模块 → 内核）

三个方法照现有惯例**无条件注册**，在 `call()` 里按 capability 判定（先例：`approval_callback.rs:1-10`、`domain.rs:1533-1575`）。只有授予了 `actions` 时，`provides` 里才列出 `_a24/action/`（先例：`domain.rs:1822-1826`）。

#### `_a24/action/submit`

```jsonc
// params（deny_unknown_fields；只额外允许 _meta）
{
  "action_key": "mtg-7f3a:deliver:v2",   // 必填。模块业务幂等键，[A-Za-z0-9._:-]{1,128}
  "tool": "notion.page_create",          // 必填。内核注册表里的工具名，精确匹配，不 trim
  "input": { /* 确切 input，JSON 对象 */ },// 必填。序列化后 ≤ 64 KiB
  "note": "把 10/16 周会纪要写进 Ops 空间", // 可选。≤ 512 字符，UI 标"模块自述，未经核实"
  "supersedes": "mtg-7f3a:deliver:v1",   // 可选。原子撤回同模块的这个待审动作（§2.5）
  "retry_of": null,                      // 可选。见 §4.6，只能指向一个结果未知或失败的动作
  "expires_in_secs": 600,                // 可选。默认 300，夹到 [60, 1800]（ADR 拍板）
  "request_id": "…",                     // 必填（进程外）。在途内核请求的关联 id
  "approval_token": "…",                 // 必填（进程外）。该请求的一次性 token
  "_meta": {}
}
```

```jsonc
// result：ActionAnswer
{
  "action_id": "<32B hex>",          // 执行记录 id，随机，不可推导
  "approval_id": "<32B hex>",        // 对应 module_approvals.id（kind=gate, binding=true）
  "action_key": "mtg-7f3a:deliver:v2",
  "tool": "notion.page_create",
  "payload_digest": "sha256:…",      // 内核算的，见 §2.4
  "decision": "pending",             // pending | approved | denied | timed_out | withdrawn
  "expires_at": "2026-10-16T09:10:00Z",
  "execution": {                     // 见 §2.6；pending 时 state=not_started
    "state": "not_started",
    "code": null, "retryable": null,
    "started_at": null, "ended_at": null,
    "output": null, "output_digest": null, "output_truncated": false
  },
  "replayed": false                  // true：命中已有记录，原样返回（ADR-K1-01 §2.6 的 replayed 先例）
}
```

#### `_a24/action/status`

`{ "action_key": "…" }` 或 `{ "action_id": "…" }`，二选一，返回 `ActionAnswer`。按 `module` 在存储查询里限定作用域："不存在"和"属于别的模块"不可区分（照 `module_approval_broker.rs:425-442` 的先例）。**不需要**在途请求，也不经 `Generation`（照 `approval_callback.rs:276-279`）。

#### `_a24/action/withdraw`

`{ "action_key": "…", "reason": "superseded|user_cancelled|stale" }`，返回 `ActionAnswer`。语义见 §2.5。

> 没有 `execute` 方法。**执行由内核在批准落地时自己触发**（§2.6），模块不在环里。这样做有两个好处：批准之后模块即使挂了，动作照样能如实落定；模块也没有"挑时机执行"或"执行两次"的入口。

### 2.3 提交处理顺序（进程外路径）

照 `ApprovalSubmitHandler::call` 的步骤（`approval_callback.rs:152-273`）排，加三步：

1. **capability**：未授予 `actions` → `forbidden`。不扣配额，不碰记录。
2. **参数形状**：`action_key` 语法、`input` 是对象且 ≤ 64 KiB、`note` 长度、`expires_in_secs` 范围。不合格 → `-32602`。
3. **幂等查找**（**先于** token 准入，照 `:199-210`）：按 `(module, action_key)` 查执行记录。
   - 查到且新算的 digest 相同 → 原样返回，`replayed: true`。**不消耗 token，不新建审批，不再执行。**
   - 查到但 digest 不同 → `payload_mismatch`（新错误码）。**绝不更新已有记录。**
4. **工具资格**（§3.2 的规则表）：`tool` 在注册表里存在；有效风险是 `External`；声明了 `target_arg` 且 input 填了它；在 manifest 的 `actions:` 声明 ∩ 宿主授予（§5）之内；不在禁止表里。不满足 → `action_not_allowed`（新码；不复用 `forbidden`，免得和"没有 capability"混在一起）。
5. **schema 校验**：用工具的 `parameters()`（`lib.rs:397`）校验 `input`。不合格 → `-32602`。
6. **token 准入 + 写入，绑在请求生命周期上**（照 `:212-271`，含"写入已提交、生命周期恰好到期"的补报分支）。一个事务里写三样：`module_approvals`（`kind=gate, binding=1, action='oak.tool', target=规范化目标, payload=信封`）、`module_action_executions`（`state=not_started`）、审计 `oak.action.submitted`。可选：同一事务里处理 `supersedes`。
7. **推送**：`module-approval.required`（复用现有事件，`ModuleApprovalSubmitted` 原样带 payload，`types.rs:646-660`）。

**进程内路径**（今天无消费者）：跳过第 6 步的 token。当前 `PolicyApprovalBackend` 每次都铸随机 `request_id`（`domain.rs:292`、`:321`），**进程内今天实际没有幂等**。OA-K 改用 `action_key`，这个问题自然消失。

### 2.4 审批对象与 `payload_digest`

**审批对象是信封，不是模块的摘要。** 内核在第 6 步构造：

```jsonc
{
  "oak": 1,                                  // 信封版本
  "module": "office",                        // 来自回调连接身份，不由模块自报（types.rs:596-598 先例）
  "tool": "notion.page_create",
  "tool_fingerprint": "sha256:…",            // 工具的 parameters schema + 有效风险 + target_arg + source 的摘要
  "target": "workspace:ops",                 // 从 input[target_arg] 规范化而来（trim、非空）
  "input": { … },                            // 原样存储；执行时用的就是这一份
  "note": "…",                               // 进入 digest：保证"展示过的卡片"与记录一致
  "idempotency_key": "oak:office:mtg-7f3a:deliver:v2"  // 仅当工具声明 idempotency_arg（§4.6）
}
```

- `payload_digest = approval_digest(信封)`。函数是现成的，递归排序键后 SHA-256（`types.rs:765-780`）。**注意：今天的 digest 只覆盖 `payload`，不覆盖 `action`/`target` 列**，所以 tool、target、module 必须放进信封，不能只依赖表的其它列。
- **执行时只读存储里的信封**，绝不读模块后来重发的字节。这样"批准后换内容"在结构上就做不到。digest 的作用是：展示、执行、回执、审计四处对同一个值，以及重提交时识别内容是否变了。
- `tool_fingerprint` 让工具升级（schema、风险等级、target_arg 变化）后旧批准失效：派发前重算，不一致 → `refused`（§2.6）。
- `approval_digest` 不是 RFC 8785 规范化（浮点数格式由 serde_json 决定）。因为 digest 只在内核内部、由同一实现算一次再比较，这一点可以接受。如果将来要让模块或第三方**独立复算** digest，需要另定规范化（ADR 记一笔）。

### 2.5 审批决定、超时与撤回

| 决定 | 谁触发 | 语义 |
|---|---|---|
| `pending → approved` | 宿主 REST `decide`（现有 CAS：`decision='pending' AND expires_at >= now`，`agent24-store/src/module_approvals.rs:219-227`） | CAS 成功后**同一调用**投递执行（§2.6）。只提供 `approve`/`deny`，不提供任何常驻选项 |
| `pending → denied` | 宿主 REST | 终态，不执行 |
| `pending → timed_out` | 现有周期扫描（10 s 一次，`module_approval_broker.rs:30`、`:518-535`） | fail-closed，等同拒绝。OA-K 的 TTL 按行算（`expires_at` 本来就是逐行字段），不再写死 300 s（`:26`） |
| `pending → withdrawn` | 模块 `withdraw` / `supersedes`；内核在模块停用、consent 撤销、附着注册轮换或撤销时 | **新决定值**。不能借用 `denied`，那会把"模块撤回"记成"人拒绝" |

**`withdrawn` 需要重建表。** `decision` 列有 `CHECK (decision IN ('pending','approved','denied','timed_out'))`（`migrations/0005_module_approvals.sql`），SQLite 改不了 CHECK，只能新建表、拷数据、改名。这是**不可回退的迁移**（K1 发布说明 `:89-121` 讲过 store 迁移不可回退的后果），必须写进 ADR。

**撤回的边界：**

- `pending` 或 `approved` 但 `not_started`：CAS 到 `withdrawn`，保证不执行。
- `executing`：不能"撤回"，只能**请求取消**（§2.7）。返回当前状态，由调用方自己看 `execution.state`。
- 已终态：原样返回，不报错（幂等）。

### 2.6 执行状态机与触发

```
                    ┌──────────── withdrawn（模块撤回 / 停用 / consent 撤销）
                    │
not_started ──(批准 CAS 成功，派发前复核全部通过)──► executing ──► succeeded
     │                                                 │            failed{code}
     │                                                 │            timeout
     ├──(派发前复核失败)──► refused{reason}             │            cancelled
     │                                                 └──────────► result_unknown
     └──(重启时仍未开始 / 超出 execute_by)──► abandoned
（决定为 denied / timed_out 的记录停在 not_started，对外报 decision，不执行）
```

- **触发**：在 REST `decide` 的 CAS 成功之后（今天的 `decide` 在 `module_approval_broker.rs:461-473`），由 broker 投递给执行器任务。`execute_by = decided_at + 60 s`。
- **派发前复核**（全部要过，任一失败就 `refused`，并写明原因码）：模块仍启用；`actions` 授予仍在；宿主对 `(module, tool)` 的授予仍有效（§5）；工具仍注册、仍在白名单（照 `execute_preapproved` 仍查白名单，`lib.rs:1426-1432`）；`tool_fingerprint` 未变；有效风险仍是 `External`；**出站门允许**（§4.3）。注意：模块进程**不必在运行**，执行不经过模块。
- **at-most-once**：`not_started → executing` 是一条 CAS（`WHERE state='not_started'`，照 `execute_due_schedule_callbacks` 的 `executed_at IS NULL` 先例，`module_approvals.rs:277-293`）。**PreDispatch 审计写入成功之后**才调用工具（照 K1-7.1 的 fail-closed：`module_audit_event.rs:403-405`）。
- **执行**：新加一个 `ToolRegistry` 入口（暂名 `execute_module_action`）：normalize → 白名单 → **跳过人审门**（批准已在手，`execute_preapproved` 的先例）→ 出站门 → `run_budgeted`。`ToolContext` 用合成关联 id（`run_id = "oak-<action_id>"`，`tool_call_id = action_id`），**明确不是 run**。合成 run_id 在 `run_source_tags` 里查不到，按 K1 规则有效模式就是 `LocalOnly`（发布说明 `:31`）。这是有意的 fail-closed。
- **结果映射**：`Ok` → `succeeded`；`ToolError::Failed` → `failed`；`Timeout` 且可能已派发 → `result_unknown`，不报 `timeout`（照 `kernel_call.rs:315-317` 把 `MaybeSent` 映射成 `ResultUnknown`）；确定没有派发的超时 → `timeout`；取消 → `cancelled` 或 `result_unknown`（同理）。
- **输出**：≤ 64 KiB，超出就截断并置 `output_truncated`；另存 `output_digest`。输出一律标为**不可信证据**（ADR-DOC-01 D4 第 6 条）。

### 2.7 超时与取消（三层，各管各的）

| 层 | 值 | 到期效果 |
|---|---|---|
| 提交调用 | 绑定所在请求的生命周期（`bind_to_lifecycle`，`approval_callback.rs:258-271`） | 返回 `timeout`；如果写入已经提交，补报后照常返回（现有分支） |
| 审批等待 | `expires_at`（默认 300 s，上限 1800 s） | `timed_out`，不执行 |
| 执行 | 工具自己的 `timeout()`（`lib.rs:411-413`），上限取 OA-K 全局值（提议 120 s） | 可能已派发 → `result_unknown`；否则 → `timeout` |

**取消**：`withdraw` 打在 `executing` 上，或宿主停用模块，只会对该次执行的 `CancellationToken` 发 cancel（工具要 `isolates_cancellation`，照 `ModuleTool`，`lib.rs:682-684`）。**不声称回滚。** 没有 run 级取消可以继承，因为 OA-K 本来就没有 run。

### 2.8 错误码（wire）

| 码 | 何时 | 现有 / 新增 |
|---|---|---|
| `forbidden` | 未授予 `actions` | 现有（`approval_callback.rs:172-177` 同款） |
| `-32602` invalid params | 形状 / schema / `action_key` 语法 / target 缺失 | 现有 |
| `action_not_allowed` | 工具资格不满足（§3.2）或宿主未授予 | **新增** |
| `payload_mismatch` | 同 `action_key` 不同 digest | **新增** |
| `not_ready` / `revoked` / `token_invalid` | 准入（`approval_callback.rs:76-90`） | 现有 |
| `timeout` | 提交调用的生命周期到期 | 现有 |
| `not_found` | `status`/`withdraw` 查不到（含跨模块） | 现有 |
| `rate_limited` | 待审数超上限（提议每模块 16 条），或令牌桶耗尽 | 现有 kind（`events_emit` 用过，A3 文档 `:202`） |
| `internal` | 存储不可用；`error.data` 不得带路径、SQL、token（`approval_callback.rs:122-129`） | 现有 |

执行结果**不走错误码**，走 `ActionAnswer.execution.state/code`。模块轮询拿到的是"结果"，不是"调用失败"。

### 2.9 `request_id`、`action_key` 与幂等的关系（重点）

- **今天的 `request_id` 不是业务键。** 它是内核为每个代理请求或内核调用铸造的关联 id（`proxy.rs:87`、`kernel_call.rs:347-356`），配一枚**一次性** token（`drain.rs:590-610`，用后 `used = true`）。`module_approvals` 的 `UNIQUE(module, request_id, kind)`（`0005_module_approvals.sql`）保护的是"**同一次请求里**重发 submit 不会建两行"，**挡不住**模块在下一次请求里用同样内容再提交一次。
- **OA-K 照这个 UNIQUE 先例，但换了键**：执行记录上加 `UNIQUE(module, action_key)`，由模块按业务语义选键（与 ADR-K1-01 §2.6"业务幂等由模块按业务键实现"一致，`docs/adr/ADR-K1-01-module-tool-contract.md:87`）。`request_id` 只用于可信关联与审计。`module_approvals` 原有的 UNIQUE 照常成立：每次新提交都来自一个新的在途请求。
- **一个在途请求只能提交一个 OA-K 动作**，因为 token 是一次性的，而且与 `gate`/`advise` 共用。工作流层"每个 action 步骤由一次用户操作或一次调度触发"正好对得上。如果将来需要一次请求提交多个动作，要在 ADR 里另定"每请求动作 token"，不能放宽现有 token 的语义。
- **内核不自动重试**，任何情况下都不（ADR-DOC-01 D4 第 3 条，`:103`；ADR-K1-01 `:83`、`:87`）。`result_unknown`、`failed`、`timeout` 都是终态。想重来，模块要用**新的** `action_key` 再提交，人要**再审一次**。

---

## 3. 相容性矩阵（四类扩展面）

### 3.1 总表

| 扩展面 | OA-K 适用？ | 怎么接 | 限制 / 坑 |
|---|---|---|---|
| ① 进程内 DomainModule 🟡（生产 catalogue 为空，`ARCHITECTURE-LAYERS.md:178`） | **契约上适用，首版不实现** | 将来在 `KernelCtx` 上加 `ActionRequester` 句柄，与 `ApprovalRequester` 同构（`domain.rs:1386-1392`）；`KERNEL_GRANTS` 加 `actions` 要等句柄存在（`domain.rs:62-76` 的规则："没有句柄就不授予"） | 没有 token 准入。幂等只靠 `action_key`（顺带修掉今天进程内每次随机 `request_id` 的问题，`domain.rs:292`）。进程内模块是可信代码，本来就能直接碰注册表；OA-K 对它的价值是**统一审计与人审**，不是隔离 |
| ② 进程外领域 OS ✅（Sin90 / Cos72 / documents；将来的 `office`） | **适用，首个落地面** | **复用现有回调 socket 与 `Methods` 表**，只是在 `build_methods_for` 里多注册三个 handler（`domain.rs:1462` 起）；`KERNEL_OOP_GRANTS` 加 `actions`（`:116-122`）；manifest 新增 `actions:` 声明段；os-sdk 加 `ActionClient` | 只在 Unix 上（ADR-DOC-01 D10）。提交时必须在一个在途内核请求里（代理请求、`fired` 投递、模块工具调用都行，都铸 token：`proxy.rs:1235-1252`、`kernel_call.rs:347-356`）。后台自发、没有请求的提交会被拒（`token_invalid`），这是有意的 |
| ③ 附着模块 ✅（AgentEar） | **首版不开放**；要开须另立 ADR | 方法可以挂在同一条附着连接上（`build_grant` 复用 `build_methods_for`，`attach_registry.rs:190-226`），但**准入锚要新增**（见 3.2） | 没有在途请求，所以没有 token（`drain.rs:479-481`；A3 文档 `:206`）。今天连 `approval` 都不授予（`attach_registry.rs:155`；A3 文档 `:377` 把 approval 放在 P3）。token 长期有效、断连不杀进程（A3 文档 `:40`、`:44`）。generation 计数在 daemon 重启后从 1 重来（A3 文档 `:240`），**不能当绑定键** |
| ④ Sidecar（Open Design） | **不适用，不开放** | 无。sidecar 的动作经 `agent24 acp` 变成 run（`ARCHITECTURE-LAYERS.md:190`），工具调用在 agent loop 里由模型发起，已经逐次过工具审批 | sidecar 没有回调 socket，也没有 `Capability` 握手。capability 模式下 `CreativeRuntime` 只能访问 `/api/v1/models`（`:158`）。ACP 的 permission response 不是授权凭据（`:239`）。如果将来要给 sidecar 一个 REST 形态的 OA-K，必须先做 ADR-005 的路由级资源授权，并另开 ADR |

### 3.2 附着与进程外：谁持有回调 socket、谁派发代际

| 维度 | ② 进程外（A1） | ③ 附着（A3） | 对 OA-K 的含义 |
|---|---|---|---|
| 回调 socket 归谁 | **内核**为每一代建 `~/.agent24/run/<pid>/<n>.sock`，0700，**只 accept 一次**，accept 后删路径（A3 文档 `:39`） | **内核**持有**一个常驻监听** `~/.agent24/attach/agent24d.sock`，可多次 accept；**由模块拨入**（A3 文档 `:39`） | 两边都是内核持有监听端，方法表都由 `build_methods_for` 构造 |
| 谁派发代际 | **supervisor**：拉起进程 → 新 `Generation`（`domain.rs:1853` 的 `Current::new(Generation::starting())`），`MethodsFor` 每代调用一次 | **`AttachSlot`**：握手提交成功时 +1（A3 文档 `:240`）；`Grant`（offer + `MethodsFor`）**每次准入 manifest 只建一次**，重连复用（`attach_registry.rs:175-188`） | 进程外的代际 = 进程寿命；附着的代际 = 连接寿命，进程活着但代际可以换 |
| 内核 → 模块 | HTTP over 模块 UDS，铸 `request_id` + token（`kernel_call.rs:338-356`） | 同一条连接上的 JSON-RPC `_a24/command/invoke`，5 s（A3 文档 `:204`），**不铸 token** | 只有进程外能给 OA-K 提供"在途请求"这个准入锚 |
| 断连 | 连接即生命线：断了这一代就结束，进程被停 | 断了只撤销这一代，进程不受影响，可用同一 token 重连 | 附着上的待审 OA-K 不能绑在 generation 上，否则一次断连就把它们全部丢弃；也不能绑在 token 上，因为 token 长期有效 |
| **OA-K：复用还是新增** | **复用**现有回调与 T7b 的 token 准入，只新增方法 | 方法挂法可以**复用**，**准入锚必须新增** | 附着要开放 OA-K，ADR 须回答：①用什么代替在途请求作准入锚（提议：不要求请求绑定，改为每注册一个待审上限加令牌桶，靠逐次人审兜底）；②待审动作绑在**注册**上（`token_id` + `manifest_digest`），轮换、撤销、重新注册时内核把它们 `withdrawn`；③要先把 `approval`/`actions` 加进 `ATTACH_OOP_GRANTS`，这本身就是一次隐私放宽，须走 A3 §3.5 的宿主显式确认 |

### 3.3 工具资格规则（四类通用）

| 规则 | 理由 / 先例 |
|---|---|
| 只能是**内核注册表里**的工具（第一方连接器工具、MCP 工具） | G1。连接器代码在内核或内核监管的 MCP 进程里，不在业务模块里 |
| 有效风险必须是 `External`（`ToolRegistry::effective_risk`，`lib.rs:1194`） | MCP 本来就固定为 `External`（`agent24-mcp/src/lib.rs:269-273`）。`Read` 走 OA-K 等于开一条跨 OS 读取通道，不在本设计范围内 |
| 必须声明 `target_arg`，且这次调用填了它（`lib.rs:399-408`） | ADR-K1-02 §4："目的地无法精确识别 → 拒绝"。审批卡片要显示确切目的地 |
| **禁止表**：`shell_exec`、`fs_write`、`fs_read`、`http_fetch`、任何 `ModuleTool`（`source = "module"`） | Exec 永远逐次问且不可约束子进程网络（ADR-K1-02 §2.4 第 3 条）；模块 → 内核 → 模块是另一项 📐（ADR-DOC-01 `:216`） |
| 在 manifest `actions:` ∩ 宿主授予之内（§5） | "申请与授权取交集"（ADR-DOC-01 D4 第 1 条） |

---

## 4. 安全论证

### 4.1 外发不离内核

- 模块进程里**不需要**任何外发代码：模块只提交信封。连接器在内核注册表里，MCP server 由内核按 `mcp.json` 拉起。
- **诚实的边界**：这**只是结构上的保证，不是 OS 沙箱**。同 UID 的模块进程仍然可以自己开 socket（`ARCHITECTURE-LAYERS.md:234`）。OA-K 让"正确的写法"同时成为"最省事的写法"。要防敌意模块，需要 OS 级出网隔离，那不在当前威胁模型内。
- 验收要加一条**结构判据**：`office` 模块的依赖树里不得出现 HTTP 客户端 crate（判据本身要先用正对照验过，见记忆 `feedback_verification_criteria`）。

### 4.2 审批对象是确切 input

- 审批卡片由**宿主**用存储里的信封渲染：工具名、目的地、`input` 的结构化视图、`payload_digest`。`note` 放在单独区域，标"模块自述，未经核实"。卡片**不得**只显示 `note`。
- 只给 `approve` / `deny` 两个选项，比 L-APPR-4（external 不给 `approve_for_session`，`docs/laws/APPROVAL.md:75-76`）和 L-APPR-5（常驻授权要窄到确切目标，`:89`）都严，所以**不需要改法律**。
- **Guardian 不得自动批准 OA-K**。照 `check_plan` 的先例，"这条路径总是问人"（`lib.rs:320-337`）。
- **今天 Desktop 没有渲染 module approval 的界面**：在 `apps/` 下检索 `module-approval` / `moduleApproval` / `ModuleApproval`，没有命中，只有 `packages/api-client` 的生成类型和 `contract-tests` 命中。所以落地切片里要加一片宿主卡片（§7 OAK-7）。
- **legacy 全权 bearer 下的残余风险**：REST `decide` 只要求 bearer（`server.rs:1135-1139`）。同 UID 的模块读得到 `daemon.json`，**技术上可以自己批准自己**。今天 run 审批也一样，属于"不防敌意本地二进制"的已知边界（`ARCHITECTURE-LAYERS.md:179`、`:236`）。要真正挡住，需要 capability 模式下 `decide` 只对 `ProductHost` 开放（ADR-005）。ADR 必须把这一条写成**显式残余风险**，不能写成"已由人审保证"。

### 4.3 payload 变化后旧批准失效

三道，各自独立：

1. **键**：同 `action_key` 不同 digest → `payload_mismatch`，不更新（§2.3 第 3 步）。
2. **执行用存储副本**：执行时读存储里的信封，模块后来发什么都无关（§2.4）。
3. **工具漂移**：派发前重算 `tool_fingerprint`，不一致 → `refused{tool_changed}`（§2.6）。

修订内容的正确流程：模块用新键提交，并带 `supersedes: 旧键`，内核在同一事务里把旧的撤回。即使模块忘了撤回，旧批准也只能执行**旧的、人看过的**内容，绝不会执行新内容。

### 4.4 出站门（K1-6b.3）怎么挂

- 挂点：执行器"派发前复核"的最后一步（§2.6），调用 6b.3 的统一出站判定。输入是：资源（来源引用；OA-K 没有 run，查不到标签即 `LocalOnly`）、用途（`oak:<tool>`）、目的地（工具的服务标识 + 规范化 target）、授权代次。参见 ADR-K1-02 `:106`。
- **6b.3 落地之前**：执行器里放一个**写死拒绝**的桩。只放行开发配置注册的 `mock.*` 工具（这类工具本身没有外部效果），其余一律 `refused{outbound_unavailable}`。不得以"人已经批准了"为由放行（ADR-K1-02 `:67`："工具审批……与出站隐私授权是独立门，必须全部通过"）。
- **与 ADR-K1-02 §2.3 的冲突，必须先定**：办公内容默认都是 `LocalOnly`（用户输入的标签固定为 LocalOnly，发布说明 `:31`）。按 §2.3，发到邮件或 Notion 需要一份绑定"**资料 × 用途 × 目的地**"的显式授权（ADR-K1-02 `:55`），而今天生产路径**没有任何入口**写 `CloudAuthorized`（发布说明 `:31`）。所以即使 6b.3 接好，OA-K 也发不出去任何一封真实邮件，**除非 ADR 明确规定**：一次 OA-K 逐次批准**就是**一份一次性授权，绑定 `payload_digest`（资料）× `tool`（用途）× `target`（目的地）× `execute_by`（有效期）× 宿主 decide 路由（签发者）× 当前策略版本。这一条要改 ADR-K1-02 的口径，所以列为"必须先定的一件事"（§8 末尾）。

### 4.5 审计（哈希链）

照 K1-7.1 的类型化闭集事件（`module_audit_event.rs:402-472`），新增一组 OA-K 事件。走同一条 `append_audit` 的 `BEGIN IMMEDIATE` 链（同文件 `:8-9`）：

| action（内核保留字串） | 何时 | 字段（只放有界标识，不放正文） |
|---|---|---|
| `oak.action.submitted` | 提交事务内 | actor=`module:<name>`、action_id、approval_id、module_id、operation_id=tool、payload_digest（小写 hex）、request_id |
| `oak.action.decided` | decide CAS 后，或扫描超时、撤回时 | action_id、decision、决定者（`host` / `scan` / `module` / `kernel`） |
| `oak.action.pre_dispatch` | **调用工具之前**；写不进去就不派发 | action_id、attempt_id、tool_fingerprint |
| `oak.action.terminal` | 终态 | action_id、result（`succeeded/failed/timeout/cancelled/result_unknown/refused/abandoned`）、duration_ms、output_digest |

- 正文（邮件内容）**不进审计**；审计里只有 digest。正文留在 `module_approvals.payload` 里。这带来一条**留存问题**：内核 store 会长期留着第三方正文。今天这张表没有清理路径，C3 P1 的删除权和体积治理还没做（`tasks.md:25`，讨论稿 §5.3 风险 4）。ADR 必须给出 OA-K 信封的保留期与 scrub 规则（提议：终态后 30 天把 `payload.input` 换成 `{"scrubbed": true}`，digest 保留）。
- K1-7.1 的引用校验会拒绝"像凭据"的值。小写 hex digest 按代码注释可以通过（`module_audit_event.rs:124-126`）；`sha256:` 前缀里的冒号能不能过 `AuditRef` 的字符集，**未核实**。
- 需要修订 **ADR-K1-04**，扩充事件闭集。

### 4.6 失败与未知结果的对账

| 情形 | 内核记成什么 | 模块和用户看到什么 | 怎么对账 |
|---|---|---|---|
| 批准前被拒、超时、撤回 | `decision` 终态，`not_started` | 没有发生动作 | 无需对账 |
| 批准后、派发前复核失败 | `refused{reason}` | 没有发生动作 | 无需对账 |
| 批准后 daemon 崩溃，重启时仍未开始，或超过 `execute_by` | `abandoned` | 没有发生动作。用户需要重新提交、重新批准 | 无需对账。**宁可不做，也不在重启后执行一个旧批准** |
| 派发后正常返回 | `succeeded` / `failed{code}` | 按结果显示。`succeeded` 显示为"连接器报告成功"，不显示"已送达" | 无需 |
| **外部成功、本地超时、连接断开、取消** | `result_unknown` | "**可能已发送**"，附目的地，提示到目的地核对 | 见下 |
| 派发中 daemon 崩溃（`executing` 遗留） | 重启扫描补写 `result_unknown`（照 `append_interrupted_module_tool_terminal`，`module_audit_event.rs:505-524`） | 同上 | 同上 |

**`result_unknown` 的对账路径（内核始终不重试）：**

1. **连接器级幂等键（可选）**：工具可以声明 `idempotency_arg`（新 trait 方法，默认 `None`）。声明了，内核就把确定性键 `oak:<module>:<action_key>` 注入 input。**注入发生在 digest 之前**，人审看到的就是带这个键的确切输入。
2. **`retry_of`**：模块提交新动作时可以带 `retry_of: 旧键`。只允许指向 `result_unknown`、`timeout`，或 `failed` 且 `retryable` 的动作。新动作**继承旧动作的 idempotency_key**，于是支持幂等键的外部服务会自己去重。新动作仍然要**再审一次**；审批卡片明确标出"这是对一次结果未知动作的重试"。
3. **业务投影**：用户在模块界面上标"已核对：已送达 / 未送达"。这一标记写在模块自己的业务状态里，作为回执的业务投影。**不回写内核**，内核的 `result_unknown` 永远不升级成 `succeeded`（报告 §6 第 14 行同族："mock 回执不能升级为已送达"）。

---

## 5. 与 K1 的关系

### 5.1 现状

- `ModuleTool` 只有在 `has_current_consent` 时才通告（`lib.rs:650-666`）。授权走 `ModuleToolAuthorization::authorize`（`:484-495`），生产实现是 `StoreConsentGate`：查不到、过期、撤销、指纹不符都否决（发布说明 `:21`）。
- consent 的**授予**在生产路径上**不存在**（发布说明 `:27`）。第一个加授予路径的 PR 必须同时处理 F1/F3/F6（`:59`）。
- consent 与 `Gate`/`Advise` **没有交集**：consent 管"内核能不能调模块的工具"，`Gate`/`Advise` 管"模块请求的审批记录"。

### 5.2 OA-K 要不要等授予路由

**要，但只卡真实连接器，不卡 mock。**

- OA-K 需要一份宿主持有的、启用时的授予："模块 M 可以经 OA-K 请求工具 T"。没有它，任何装了 `actions` 的模块都能请求任何 External 工具，只靠逐次人审兜底。审批疲劳下，这条防线是虚的。
- **推荐复用 K1-6a 的 consent 存储与授予路由**，操作 id 用独立的命名空间（提议 `oak/<tool>`，与模块自己的 operation 不会撞名），撤销语义和停用、卸载时的撤销照 K1 已落地的 fail-closed 行为走（发布说明 `:37-56`）。这样 OA-K 自动继承 F1/F3/F6 的修复义务，不另造一套同意系统。
- 归属：**K1-6a（ADR-K1-03 的"启用授权"）那一片**，不是 OA 线。OA-K 的执行器切片（OAK-5）可以先合，只放行 `mock.*`；**接入任何真实连接器之前，必须等 K1-6a 授予路由落地**。
- 不推荐的替代：一份 user-local 的允许清单文件（照 L-APPR-1 risk override 的形状）。更便宜，但会多出第二套同意系统；而且它与"启用 OS 时确认工具清单"（ADR-DOC-01 D4 第 1 条）的界面也对不上。

### 5.3 与 K1-6b.3 的关系

见 §4.4。OA-K 是 6b.3 统一出站判定的一个**新消费者**。6b.3 的 ADR 原文要求"任一未接入消费者不通告/不可用"（ADR-K1-02 `:106`），OA-K 执行器照此处理：判定 API 不存在时，执行器就是写死拒绝的桩。

### 5.4 法律

- 首版只给 `approve`/`deny`，比 L-APPR-4/5 更严，**不改法律**。PR 描述里点名 L-APPR-4、L-APPR-5，说明凭什么符合。
- 将来要给 OA-K 加常驻授权（比如"总是允许写到 Ops 空间"），必须**先单独提一个法律 PR**（`docs/laws/README.md:34-42`："不得在同一个 PR 里既改法律又改行为"），并处理 L-APPR-7 读权限的缺口（`APPROVAL.md:116-124`）。

---

## 6. 备选与不做的方案

### 6.1 备选 A（更便宜）：连接器做成 `office` 模块自己的 `ModuleTool`

**形状**：`office` 在 manifest 里声明 `mail_send` 等工具，内核按 ADR-K1-01 通告给 agent loop。模型调用时，内核走工具审批，再 `POST /api/v1/office/_a24/tools/mail_send`，由模块进程去发。

**它省掉的**：新 Capability、新表、执行器、表重建迁移。K1 契约已经在了，只缺授予路由（§5.2）。估计省掉 OAK-1 到 OAK-6 的大半。

**它的代价**：

1. **外发留在模块进程里。** 内核出站门（6b.3）管不到模块进程自己发出的网络流量。ADR-K1-02 §2.4 第 3 条要求"宿主当前无法约束子进程目的地时，对带敏感资料的该动作拒绝"。按这条原文，**带 LocalOnly 资料的 `mail_send` 一律得拒绝**，所以备选 A 在 K1-02 下其实发不出带纪要内容的邮件。要发，就得在 ADR 里显式偏离。
2. **人审的对象变成模型的工具调用**，不是工作流层确定的 payload。确定性驱动没有入口（讨论稿 §5.1 "没有非模型调用方走工具流水线的入口"），所以"计划版本 + 批准绑定最终 payload"要靠模型老老实实把 artifact 原样抄进参数。
3. **ADR-DOC-01 D4 第 2 条**：第一方默认安装落地之前，`office` 的工具也是 `External`（`:99`），每次都审，这一点与 OA-K 相同，不构成优势。
4. **审计**：K1-7.1 只审计"内核调了模块工具"，看不到模块进程内部实际发了什么。

**选它时，ADR 必须显式写下的偏离：**

- 偏离 ADR-K1-02 §2.4 第 2、3 条：外发由模块进程执行，内核出站门**不覆盖**。列明这些动作的资料范围（只允许用户逐次批准的 artifact）。
- 偏离讨论稿 §5.4（"外发只在内核执行"），以及本文 G1。
- 残余风险：模块有 bug 时，可能对已批准的目的地多发，或发到错误的目的地。缓解只能靠模块自己的业务幂等和自测，内核拿不出证据。
- 退出条件：OA-K 落地后，连接器从模块迁回内核注册表，迁移期间两条路径不能并存。

### 6.2 备选 B：在 `_a24/approval/gate` 闭集里加 `oak.tool`

否决，见 §2.1。会让已授予 `approval` 的模块静默升级权限。

### 6.3 备选 C：为每个动作建一个合成 run，走现有工具审批

否决。审批模型是阻塞式的，挂不进模块回调的生命周期。还会污染会话与记忆日志（§2.1 表的 Run 行）。

### 6.4 不做

- 内核自动重试或传输层去重（ADR-K1-01 `:89` 不授权）。
- 批量批准、常驻授权（§5.4）。
- 附着模块与 sidecar 的 OA-K（§3.1）。
- 模块经 OA-K 读数据或调别的模块。

---

## 7. 落地切片（每片 ≤300 行）

前提：**主线（C3 记忆 P1）不变。** OA-K 并入 #735 的内核评审带宽，OA 计划本身也声明不改变主线优先级（`PLAN-OFFICE-ASSISTANT.md:8`）。所有切片都动权限、审计或协议，按 K1 惯例属**高风险**，须 PR-Daemon APPROVE（`tasks.md:56`）。

| 片 | 内容 | 估行 | 风险 | 依赖 | 走 ADR？ |
|---|---|---|---|---|---|
| **OAK-0** | **内核 ADR（并入 #735，暂编 ADR-K1-05）**：本文 §2–§6 的定稿；附带三处修订：ADR-K1-02 §2.3 的一次性授权口径、ADR-K1-04 的事件闭集、ADR-K1-03 的 `oak/<tool>` 命名空间。只有文档 | ~250（文档） | 高 | 无 | **是（本片就是 ADR）** |
| OAK-1 | `agent24-protocol`：`ActionSubmit`、`ActionAnswer`、`ActionExecutionState`、`ModuleApprovalDecision::Withdrawn`、错误码；导出 schema、加 fixture。不接线 | ~220 | 中高 | OAK-0 | 依 ADR |
| OAK-2 | `agent24-store`：新迁移 `module_action_executions`（`UNIQUE(module, action_key)`、`approval_id` FK）；重建 `module_approvals` 给 `decision` 加 `withdrawn`（**不可回退**）；CAS 函数；`agent24-core` 加执行状态迁移表 | ~300 | **高**（不可回退迁移） | OAK-1 | **是**（迁移不可回退，须 ADR 明示） |
| OAK-3 | `agent24d` broker：信封构造与 digest、幂等查找、`payload_mismatch`、`supersedes`、`withdraw`、按行 TTL。**不执行**（执行器桩一律 `abandoned`）。确保 `validate_gate_action` 仍拒 `oak.tool` | ~280 | 高 | OAK-2 | 依 ADR |
| OAK-4a | `agent24-domain`：manifest `actions:` 段、`Capability::Actions`；`KERNEL_OOP_GRANTS` 加入；offer 加 `_a24/action/`。附着与进程内**不加** | ~200 | 高（新 Capability） | OAK-0 | **是**（新增 Capability，`ARCHITECTURE-LAYERS.md:256`） |
| OAK-4b | `agent24d`：三个 wire handler（§2.3 顺序）、限流、待审上限；Python 假模块黑盒测试 | ~300 | 高 | OAK-3、OAK-4a | 依 ADR |
| OAK-5 | 执行器：decide 钩子、派发前复核、`ToolRegistry::execute_module_action`、出站门**桩**（只放 `mock.*`）、结果映射、重启扫描（`abandoned` / `result_unknown`） | ~300 | **高** | OAK-4b | 依 ADR |
| OAK-6 | 哈希链审计：四个 OA-K 事件、PreDispatch 写不进就不派发、中断补写；信封 scrub 规则 | ~250 | 高 | OAK-5、ADR-K1-04 修订 | **是**（ADR-K1-04 修订） |
| OAK-7 | Desktop 审批卡片：渲染信封（工具、目的地、结构化 input、digest、`note` 分区），只有 approve/deny；`result_unknown` 的"可能已发送"显示 | ~280 | 中 | OAK-3 | 否（对照 L-APPR-4/5 点名即可） |
| OAK-8 | `agent24-os-sdk`：`ActionClient`、`RequestContext` 透传、示例；`office` 侧用 mock 连接器跑通 E2E | ~250 | 中 | OAK-4b | 否 |
| **门 G-real** | 接入第一个真实连接器 | — | — | **K1-6a 授予路由（含 F1/F3/F6）+ K1-6b.3 出站判定 + OAK-0 中 §4.4 的口径获批** | — |

建议顺序：OAK-0 →（OAK-1 ∥ OAK-4a）→ OAK-2 → OAK-3 → OAK-4b → OAK-5 → OAK-6 →（OAK-7 ∥ OAK-8）。按 stacked PR 开；子 PR 开草稿（记忆 `feedback_stacked_pr_draft`）。

**OA 线的影响**：OAK-0 到 OAK-8 全部完成后，OA-M2 只能对接 `mock.*`。真实连接器要等门 G-real。讨论稿 §5.2 第 4 问的结论（"OA-K 落地之前，M2 不排日期"）不变。

---

## 8. 未核实清单

1. **Desktop 有没有 module approval 界面**：只在 `apps/` 下做了关键字检索，没有命中。没有逐个打开文件确认，也没有排除换名实现的可能。
2. **合成 `run_id` 的 `ToolContext::legacy` 能不能安全喂给内建工具和 MCP 工具**：没有读 MCP 适配器和各内建工具，不确定它们会不会按 `run_id` 回查 `runs` 表。OAK-5 必须先写这一项的反例测试。
3. **`AuditRef` 的字符集**能不能接受 `sha256:` 前缀，未核实。只核了 `looks_credential_like` 对小写 hex 放行（`module_audit_event.rs:124-126`）。
4. **`agent24-policy` 审批 broker 与 Guardian 的内部实现**没有读。"Guardian 不得自动批准 OA-K"是提议，不知道现有 Guardian 有没有可能经由别的路径看到 module approval。
5. **ADR-K1-03 与 ADR-K1-04 原文**没有通读，只读了 ADR-K1-01 的相关段落和 ADR-K1-02 的 §2.3–§6。§5.2 关于"复用 consent 命名空间"的可行性，以 ADR-K1-03 原文为准。
6. **`rpc::ErrorKind` 的完整列表**没有读（`agent24-os-proto/src/rpc.rs`）。`rate_limited` 是否就是现成的 kind，依据是 A3 文档 `:202` 和 `events_emit` 的引用。
7. **SQLite 表重建迁移**在现有 `module_approvals` 数据、两个部分索引（`0005`、`0006`）下怎么写，没有试。估行可能偏低。
8. **REST `decide` 路由**的处理器（`crate::module_approvals`）没有读，只读了 broker 的 `decide`（`module_approval_broker.rs:461-473`）。不确定处理器是否还有别的副作用。
9. **`tasks.md` 的 K1 台账**还写着 `PLANNED`/`BLOCKED`（`:48-54`），与 K1 已发布的说明对不上。引用"高风险须 APPROVE"时，以台账的规则文字为准，状态值没有采信。
10. 所有估行都没有经过编译或原型验证。
11. **这次没有对抗复核。** 本会话里 codex MCP 连接失败（CONNECTION_CLOSED）。按记忆 `feedback_codex_challenge_before_pr`，本文进 PR 之前应该先过一轮 Codex 挑战。

---

### 必须先定的一件事

**一次 OA-K 逐次批准（确切信封 × 工具 × 目的地 × 短时效）算不算 ADR-K1-02 §2.3 所说的"资料 × 用途 × 目的地"显式授权（一次性）？**

- **算**：ADR-K1-02 要相应修订。OA-K 就是生产路径上第一个、也是最窄的 `CloudAuthorized` 签发入口，真实连接器在 6b.3 落地后可以接。
- **不算**：OA-K 只能驱动不出本机的动作，或者只能处理不带 LocalOnly 资料的动作。OfficeAssistant 的"把纪要发出去"要再等一套独立的云授权产品入口。

这个问题先于 OAK-0 的其余部分。它决定 OA-K 是"办公助手的外发通道"，还是"只是一个更严格的审批记录"。
