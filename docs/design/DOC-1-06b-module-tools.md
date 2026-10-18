# DOC-1-06b —— 文档操作作为模块工具（#705 的 agent 暴露）

> 状态：设计，2026-10-18。负责人：David。任务：#705 剩余部分（agent 暴露 + 对话入口）。
> 依据：ADR-K1-01（模块工具契约）、ADR-K1-02（数据流）、ADR-K1-03（启用授权）、ADR-DOC-01 D4/D8、ADR-DOC-02 §4。K1 已随 #1022 发布到 main。

## 0. 结论

- 本轮分两段：**片 1（本设计，Documenting 模块侧）** 和 **内核授予路径（K1 线，片 1 之后另出设计，走 `ab/kernel`）**。
- 片 1 交付后的状态是**「已登记、不通告、调不到」**：文档模块声明七个只读工具并实现 `_a24/tools` 入口，但内核没有授予入口、远端档位守卫仍在，产品行为不变。
- 对话入口用 agent 页的 `POST /api/v1/runs`；聊天页 `/api/v1/chat` 不带工具，不改（David，2026-10-18）。
- K1-6b（工具结果有自己的敏感分类和出口门，#870/#874）合入之前，文档工具**一律不授予、不通告**，任何片都不撤远端档位守卫（David，2026-10-18）。

## 1. 现状（main @ 1ef85127）

| 项 | 现状 | 出处 |
|---|---|---|
| 工具声明 | `domain-os.yml` 可选的 `tools`；字段 `operation`、`description`、`input_schema`、`risk`、`output_privacy`、`timeout_ms`、`inline_wait_ms`、可选 `reversible_draft`；`deny_unknown_fields`，任一工具不合法则整份 manifest 被拒、模块挂不上 | `agent24-domain/src/tool.rs:112-162,271-287` |
| 操作名 | `[a-z0-9][a-z0-9_-]*`，**不能带点**；全名 `<module>.<operation>` | `tool.rs:112-120,363` |
| 调用 | `POST /api/v1/<module>/_a24/tools/<operation>`，正文 `{"context":…,"arguments":…}`，`context` 由宿主注入（`run_id`、`tool_call_id`、`module_id`、`operation`，`session_id` 可为 null）。不带额外请求头（`extra_headers` 为空） | `agent24d/src/module_tool_runtime.rs:80-115`（`:101` 空请求头）；`agent24-tools/src/lib.rs:473-481,858-866` |
| 期限 | `inline_wait` 是内核硬期限：内核用 `tokio::time::timeout(inline_wait, …)` 包住整次调用，超时记 `result_unknown`。取值来自 manifest 的 `inline_wait_ms`（`server.rs:1635`）。`timeout_ms` 只作为发给模块的 HTTP 期限（`KernelLimits.total`）。模块自身处理预算必须严格小于 `inline_wait_ms` 并留余量（`inline_wait_ms=25000` 时预算 20000，超出返回 `error{code:"timeout", retryable:true}`）。`extract` 不等待抽取完成，只提交或重放后立即返回 | `agent24-tools/src/lib.rs:868-884`；`agent24d/src/server.rs:1634-1635`；`module_tool_runtime.rs:108-110` |
| 结果信封 | `completed{payload,replayed}` / `pending{job}` / `error{code,retryable,details}`。内核**先解析信封再看状态码**：非 2xx 加合法的 `error` 信封时错误码保留；非 2xx 配 `completed` / `pending`，或正文解析失败，才是 `invalid_result`。模块仍一律返回 2xx。信封 `deny_unknown_fields`，`next_cursor` 等字段只能放在 `payload` 里。响应上限 **64 KiB**；`pending` 不交给模型，收成 `result_unknown` | `agent24-os-proto/src/kernel_call.rs:96-98,132-143,326-329`；`module_tool_runtime.rs:18`；`agent24-tools/src/lib.rs:886-900` |
| 错误码 | `invalid_input`、`permission_denied`、`module_unavailable`、`operation_unavailable`、`rate_limited`、`timeout`、`cancelled`、`response_too_large`、`invalid_result`、`module_error`、`result_unknown`；其他码收成 `module_error`，原码放进 `module_code`（只在不属于这张码表时才有值）。交给模型的错误 JSON 只有 `code`、`retryable`、`module_code`、`result_unknown`，没有 `details` | `agent24-domain/src/tool.rs:491-521`；`agent24-tools/src/lib.rs:1017-1044` |
| 授予 | 生产代码里没有调用 `grant_module_consent`，HTTP 只有列出、导出、撤销 | `agent24d/src/server.rs:1081-1092` |
| 风险 | 生产装配把所有模块工具写死为 `ManualInstall` + `External` + `High` | `server.rs:1576-1604` |
| 远端档位守卫 | 有任一远端档位时，`documents` 的工具不通告 | `module_tool_runtime.rs:119-128,172-174`；`server.rs:1618` |
| SDK | `agent24-os-sdk` 没有 `_a24/tools` 的入口，模块自己在 router 上实现 | — |

## 2. 契约修订（本设计 PR 一并改 ADR）

1. **工具名去掉操作名里的点**（ADR-DOC-02 §4 表格）：`documents.job.get` → `documents.job_get`；片 2 的 `change.propose`、`revision.list`、`revision.get` 同样改成 `change_propose`、`revision_list`、`revision_get`。全名仍按第一个点切出模块名。
2. **工具响应 ≤ 64 KiB**（ADR-DOC-02 §4「分页」）：REST 单页仍 ≤ 512 KiB；工具路径另按字节预算分页，序列化后的信封不超过 64 KiB（留 4 KiB 余量，预算 60 KiB），超出就提前截断并在 `payload` 里返回 `next_cursor`（信封 `deny_unknown_fields`，不能放在顶层，见 §1）。单个条目本身超过预算时返回 `error{code:"response_too_large"}`，不截断条目内容。
3. **抽取不用 `pending`，结果用 `extraction_get`**（ADR-DOC-02 §4 extract 行）：`documents.extract` 只提交或重放，立即返回 `completed{payload:{status, job_id}}`，不等待抽取完成，不带抽取值，也不带 `extraction_id`。`extraction_id` 只在任务成功、提交事务时生成（`agent24-documents/src/extract/job.rs:216-231` 生成 id，`:247-267` 与 job 的 `succeeded` 写在同一事务）；只有 `succeeded` 的 job 才带 `result.extraction_id`（`jobs.rs:113-117`）。agent 用 `documents.job_get` 查进度，成功后把该 id 交给只读工具 `documents.extraction_get` 分页读取。重放沿用 ADR-DOC-02 §5：业务键命中 `succeeded` 返回原 job；命中 `failed` 或 `interrupted` 把同一个 job 重新排队（`extract/handler.rs:203-218,253-255`），等于重跑；同一业务键但请求正文的规范化哈希不同，返回 422 `idempotency_key_reused`（`handler.rs:102-103,199-200`）。工具路径构造与 REST 相同的规范化请求正文（`revision`、`schema` 的 JCS），不含 `document_id`、`context` 和其他工具专有字段。
4. **关联信息在正文**（ADR-DOC-01 D4.3）：内核把 `run_id`、`tool_call_id` 放在请求正文的 `context` 里，不是 `x-a24-run-id` / `x-a24-tool-call-id` 头。模块以正文为准；操作日志的来源记 `run_id + tool_call_id`。

## 3. 片 1：模块侧（`rust/apps/agent24-documents`，`ab/documenting`）

### 3.1 工具清单

| 操作 | 全名 | 对应 REST | 参数（JSON Schema 要点） | `timeout_ms` / `inline_wait_ms` |
|---|---|---|---|---|
| `list` | `documents.list` | `GET /documents` | `cursor?`、`limit?`（1–200） | 30000 / 25000 |
| `get` | `documents.get` | `GET /documents/{id}` | `document_id` | 30000 / 25000 |
| `read_range` | `documents.read_range` | `GET …/revisions/{rev}/text` | `document_id`、`revision`、`block?`、`cursor?`、`limit?`、`require_complete?` | 30000 / 25000 |
| `find` | `documents.find` | `POST /documents/{id}/find` | `document_id`、`revision`、`query`、`cursor?`、`limit?`、`require_complete?` | 30000 / 25000 |
| `extract` | `documents.extract` | `POST /documents/{id}/extractions` | `document_id`、`revision`、`schema`（同 REST） | 30000 / 25000 |
| `job_get` | `documents.job_get` | `GET /jobs/{id}` | `job_id` | 30000 / 25000 |
| `extraction_get` | `documents.extraction_get` | `GET /extractions/{id}` | `extraction_id`、`cursor?`、`limit?` | 30000 / 25000 |

- 流程：`extract` → `completed{payload:{status, job_id}}`；agent 用 `job_get` 查进度；成功后 `job_get` 的 `result.extraction_id` 交给 `extraction_get`。`extract` 的参数里没有 `cursor`。`extraction_get` 对应 `GET /extractions/{id}`（`agent24-documents/src/lib.rs:184-187`；`protocol/openapi.yaml:2263-2282`），仍按 60 KiB 预算分页；单条值超过预算返回 `error{code:"response_too_large"}`，不截断。REST 单条值最大 508 KiB（512 KiB 页减去 4 KiB 信封，`extractions.rs:22-28`）。
- `extract` 不支持 `rerun`。`rerun:true` 需要 `Idempotency-Key` 请求头（`openapi.yaml:2214-2218`），内核工具请求不带额外请求头（`module_tool_runtime.rs:101`）。
- 全部 `risk: read`、`output_privacy: local_only`，不声明 `reversible_draft`。`input_schema` 取 `protocol/openapi.yaml` 参数的子集，排除项见上，`additionalProperties: false`。
- 不通告：upload、import、render、capabilities、job 的 cancel / retry（ADR-DOC-02 §4，不变）。
- `description` 写清用途和「只读、本机」，不含示例文档内容。

### 3.2 入口与行为

- 在 documents 的 router 上加 `POST /_a24/tools/{op}`，与 REST 路由**调用同一个服务层**（ADR-DOC-02 §4），不经 HTTP 自调用。
- 一律返回 2xx 加信封。
- **期限**。内核用 `tokio::time::timeout(inline_wait, …)` 包住整次调用，超时记 `result_unknown`（`agent24-tools/src/lib.rs:868-884`）；`inline_wait` 来自 manifest（`agent24d/src/server.rs:1635`）。`timeout_ms` 只作为发给模块的 HTTP 期限（`module_tool_runtime.rs:108-110`）。模块对每次工具调用的自身处理预算严格小于 `inline_wait_ms` 并留余量：`inline_wait_ms=25000` 时模块预算 20000，超出预算返回 `error{code:"timeout", retryable:true}`。`extract` 不等待抽取任务完成：只提交或重放后立即返回。
- **错误映射**。交给模型的错误 JSON 只有 `code`、`retryable`、`module_code`、`result_unknown`（`agent24-tools/src/lib.rs:1017-1044`）。`module_code` 只在错误码不属于内核码表时才有值（`agent24-domain/src/tool.rs:506-521`）。
  - 参数结构不合法（schema 层面）→ `invalid_input`；
  - 业务上需要 agent 区分的错误，直接用模块自己的码作 `code`。码以 `protocol/openapi.yaml` 的 `DocumentsErrorCode`（`3578-3600`）和 ADR-DOC-02 §6 的闭集为准，不另造。本片用到的是：`not_found`（文档、revision、job、抽取不存在都是这一码；闭集里没有 `document_not_found`、`revision_not_found`、`extraction_not_found`、`job_not_found`）、`invalid_request`（schema 通过之后的业务校验）、`unsupported_format`、`parse_failed`、`partial_parse`（`require_complete` 时文本不完整；闭集里没有 `text_incomplete`）、`idempotency_key_reused`，以及不可重试的 `storage_unavailable`（`corrupt`、`not_writable`、`disk_full`）。内核把这些码收成 `module_error`，原码放进 `module_code`；`retryable` 按 ADR-DOC-02 §6 该错误是否可重试填写（上列都是 `false`）；
  - `engine_unavailable`（Linux 等没有引擎，503，可重试；ADR-DOC-02 §3.1、§8；`extract/handler.rs:106-117`；`read.rs:85-89`）→ `module_unavailable`，`retryable:true`。调用时能力不可用定为 `module_unavailable`（ADR-K1-01 第 55 行）；`operation_unavailable` 只用于未知操作；
  - 存储暂不可用（`storage_unavailable` 且原因为 `locked` 或 `busy`）→ `module_unavailable`，`retryable:true`；
  - 未知操作 → `operation_unavailable`；
  - 超过字节预算的单个条目 → `response_too_large`；
  - 其余 → `module_error`。
- 模型看不到 `details`。交给模型的错误 JSON 没有这个字段；内核模块工具审计事件也没有 `details` 槽（`agent24-store/src/module_audit_event.rs:396-414`，`deny_unknown_fields`），`details` 写不进这条审计。agent 要区分的信息放在 `code` 里。
- `context` 宽松解析，不用 `deny_unknown_fields`（先例 `kernel_call.rs:154-172` 的 `FiredBody`）。必填 `run_id`、`tool_call_id`、`module_id`、`operation`；`session_id` 可以为 null（`agent24-tools/src/lib.rs:473-481`；临时 run 没有会话）。缺必填字段 → `invalid_input`。不读 `context` 以外的身份来源。
- 正文、抽取值不进日志和事件；日志只记操作、`run_id`、`tool_call_id`、结果码和字节数。

### 3.3 切片（每片 ≤ 300 行、≤ 8 文件，按顺序）

| 片 | 内容 | 关键测试 |
|---|---|---|
| A | 入口骨架：路由、解析 `context`/`arguments`、信封序列化、错误映射、未知操作 | 非法正文、缺 `context`、未知操作都返回 2xx + 对应错误码；客户端经代理访问 `_a24/tools` 得 404 |
| B | `list`、`get`、`job_get` | 结果与同参数的 REST 响应一致；`list` 在字节预算处截断并给 `next_cursor` |
| C | `read_range`、`find` | 大文档按字节预算分页，拼回后与 REST 全量一致；单条超预算返回 `response_too_large` |
| D | `extract` + `extraction_get` | 首次返回 `{status, job_id}`；成功后 `job_get` 给出 `extraction_id`，`extraction_get` 分页读回与 REST 一致；重放 `failed` 会重新排队的行为有测试 |
| E | `domain-os.yml` 声明七个工具 | 用内核同一校验函数解析 manifest 通过；七个全名正确；把任一工具的 `operation` 改成带点的名字，校验失败（反面对照） |

E 放最后：manifest 只要有一个工具不合法，整个模块就挂不上；先有入口再声明，任何一片单独合入都不改变产品行为。

### 3.4 不变量（每片 PR 写明并有测试或检查）

- 不实现、不调用任何授予接口；不改内核的风险装配和远端档位守卫。
- 工具结果 ≤ 64 KiB；不用 `pending` 信封。
- 正文不进日志和事件。
- REST 行为不变（现有契约测试全过）。

## 4. 内核授予路径（K1 线，片 1 之后另出设计）

只定方向，详细设计在片 1 合入后另起，走 `ab/kernel` 的 task PR：

1. 授予 API 与桌面确认界面：逐个工具展示 ADR-K1-03 的四项（可读、可改、可外发、宿主风险），用户确认后调用 `grant_module_consent`。
2. 风险按声明与确认推导：用户逐个确认过的工具，宿主风险跟随声明的 `risk`（ADR-DOC-01 D4.2「用户在启用时逐个确认过的工具」），替换写死的 `External`。
3. 派发时重查远端档位守卫（ADR-K1-01 §2.2「派发前重新检查所有门」；现在只在通告时查）。
4. 与 K1-6b 的关系：在 K1-6b 合入、文档工具结果有自己的分类之前，即使授予路径做好，documents 的工具仍不通告（§0）。

## 5. 验收（#705 剩余部分）

- 片 1：七个工具在 daemon 启动时登记成功（日志或 `GET` 工具清单可见），远端档位存在时不通告；REST 契约测试全过。
- 对话入口（内核授予路径和 K1-6b 之后）：在 agent 页（`/api/v1/runs`）里，用户确认启用后，agent 能 list / get / read_range / find / extract / job_get / extraction_get 已导入的文档，每次调用记审计。这一项不属于本轮片 1 的验收。

## 6. 记录在案的缺口

- 远端档位守卫只在通告时检查，派发时不查（`agent24-tools/src/lib.rs:1329-1345`）。实际影响小（同意门仍会拦住），归入 §4 第 3 条。
- `docs/agent/tasks.md:48-54` 的 K1 台账仍写 K1-5/6/7 为 `PLANNED`/`BLOCKED`，与 main 不符，K1 线下次改台账时更正。
- 导入不通告（Q4）：对话里只能操作页面里已导入的文档，DOC-1 期间接受。
