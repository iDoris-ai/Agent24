# DOC-1-06b —— 文档操作作为模块工具（#705 的 agent 暴露）

> 状态：设计，2026-10-18。负责人：David。任务：#705 剩余部分（agent 暴露 + 对话入口）。
> 依据：ADR-K1-01（模块工具契约）、ADR-K1-02（数据流）、ADR-K1-03（启用授权）、ADR-DOC-01 D4/D8、ADR-DOC-02 §4。K1 已随 #1022 发布到 main。

## 0. 结论

- 本轮分两段：**片 1（本设计，Documenting 模块侧）** 和 **内核授予路径（K1 线，片 1 之后另出设计，走 `ab/kernel`）**。
- 片 1 交付后的状态是**「已登记、不通告、调不到」**：文档模块声明六个只读工具并实现 `_a24/tools` 入口，但内核没有授予入口、远端档位守卫仍在，产品行为不变。
- 对话入口用 agent 页的 `POST /api/v1/runs`；聊天页 `/api/v1/chat` 不带工具，不改（David，2026-10-18）。
- K1-6b（工具结果有自己的敏感分类和出口门，#870/#874）合入之前，文档工具**一律不授予、不通告**，任何片都不撤远端档位守卫（David，2026-10-18）。

## 1. 现状（main @ 1ef85127）

| 项 | 现状 | 出处 |
|---|---|---|
| 工具声明 | `domain-os.yml` 可选的 `tools`；字段 `operation`、`description`、`input_schema`、`risk`、`output_privacy`、`timeout_ms`、`inline_wait_ms`、可选 `reversible_draft`；`deny_unknown_fields`，任一工具不合法则整份 manifest 被拒、模块挂不上 | `agent24-domain/src/tool.rs:112-162,271-287` |
| 操作名 | `[a-z0-9][a-z0-9_-]*`，**不能带点**；全名 `<module>.<operation>` | `tool.rs:112-120,363` |
| 调用 | `POST /api/v1/<module>/_a24/tools/<operation>`，正文 `{"context":…,"arguments":…}`，`context` 由宿主注入（`run_id`、`session_id`、`tool_call_id` 等） | `agent24d/src/module_tool_runtime.rs:80-115`；`agent24-tools/src/lib.rs:858-866` |
| 结果信封 | `completed{payload,replayed}` / `pending{job}` / `error{code,retryable,details}`；**非 2xx 一律当 `invalid_result`**；响应上限 **64 KiB**；`pending` 不交给模型，收成 `result_unknown` | `agent24-os-proto/src/kernel_call.rs:96-145,326-329`；`module_tool_runtime.rs:18`；`agent24-tools/src/lib.rs:886-900` |
| 错误码 | `invalid_input`、`permission_denied`、`module_unavailable`、`operation_unavailable`、`rate_limited`、`timeout`、`cancelled`、`response_too_large`、`invalid_result`、`module_error`、`result_unknown`；其他码收成 `module_error` | `agent24-domain/src/tool.rs:491-` |
| 授予 | 生产代码里没有调用 `grant_module_consent`，HTTP 只有列出、导出、撤销 | `agent24d/src/server.rs:1081-1092` |
| 风险 | 生产装配把所有模块工具写死为 `ManualInstall` + `External` + `High` | `server.rs:1576-1604` |
| 远端档位守卫 | 有任一远端档位时，`documents` 的工具不通告 | `module_tool_runtime.rs:119-128,172-174`；`server.rs:1618` |
| SDK | `agent24-os-sdk` 没有 `_a24/tools` 的入口，模块自己在 router 上实现 | — |

## 2. 契约修订（本设计 PR 一并改 ADR）

1. **工具名去掉操作名里的点**（ADR-DOC-02 §4 表格）：`documents.job.get` → `documents.job_get`；片 2 的 `change.propose`、`revision.list`、`revision.get` 同样改成 `change_propose`、`revision_list`、`revision_get`。全名仍按第一个点切出模块名。
2. **工具响应 ≤ 64 KiB**（ADR-DOC-02 §4「分页」）：REST 单页仍 ≤ 512 KiB；工具路径另按字节预算分页，序列化后的信封不超过 64 KiB（留 4 KiB 余量，预算 60 KiB），超出就提前截断并返回 `next_cursor`。单个条目本身超过预算时返回 `error{code:"response_too_large"}`，不截断条目内容。
3. **抽取不用 `pending`**（ADR-DOC-02 §4 extract 行）：`documents.extract` 提交或重放抽取；任务已完成就在 `completed.payload` 里带抽取结果（按 2 分页），没完成就返回 `completed{payload:{status, job_id, extraction_id}}`，agent 用 `documents.job_get` 查进度，完成后**用同样参数再调一次 `documents.extract`** 取结果。这依赖 ADR-DOC-02 §5 的业务键幂等：同一文档、revision、schema、`extractor_version` 的重复提交返回已有的抽取。若实现时发现重放取不到结果，改为增加只读工具 `documents.extraction_get`，并在实现 PR 里写明。
4. **关联信息在正文**（ADR-DOC-01 D4.3）：内核把 `run_id`、`tool_call_id` 放在请求正文的 `context` 里，不是 `x-a24-run-id` / `x-a24-tool-call-id` 头。模块以正文为准；操作日志的来源记 `run_id + tool_call_id`。

## 3. 片 1：模块侧（`rust/apps/agent24-documents`，`ab/documenting`）

### 3.1 工具清单

| 操作 | 全名 | 对应 REST | 参数（JSON Schema 要点） | `timeout_ms` / `inline_wait_ms` |
|---|---|---|---|---|
| `list` | `documents.list` | `GET /documents` | `cursor?`、`limit?`（1–200） | 30000 / 25000 |
| `get` | `documents.get` | `GET /documents/{id}` | `document_id` | 30000 / 25000 |
| `read_range` | `documents.read_range` | `GET …/revisions/{rev}/text` | `document_id`、`revision`、`block?`、`cursor?`、`limit?` | 30000 / 25000 |
| `find` | `documents.find` | `POST /documents/{id}/find` | `document_id`、`revision`、`query`、`cursor?`、`limit?` | 30000 / 25000 |
| `extract` | `documents.extract` | `POST /documents/{id}/extractions`，结果 `GET /extractions/{id}` | `document_id`、`revision`、`schema`（同 REST）、`cursor?` | 30000 / 25000 |
| `job_get` | `documents.job_get` | `GET /jobs/{id}` | `job_id` | 30000 / 25000 |

- 全部 `risk: read`、`output_privacy: local_only`，不声明 `reversible_draft`。`input_schema` 与 `protocol/openapi.yaml` 的对应参数一致，`additionalProperties: false`。
- 不通告：upload、import、render、capabilities、job 的 cancel / retry（ADR-DOC-02 §4，不变）。
- `description` 写清用途和「只读、本机」，不含示例文档内容。

### 3.2 入口与行为

- 在 documents 的 router 上加 `POST /_a24/tools/{op}`，与 REST 路由**调用同一个服务层**（ADR-DOC-02 §4），不经 HTTP 自调用。
- 一律返回 2xx 加信封；业务错误映射到内核认可的码，模块自己的错误码放进 `details.reason`：
  - 参数不合法、文档或 revision 不存在 → `invalid_input`（`retryable:false`）；
  - 存储暂不可用 → `module_unavailable`（`retryable:true`）；
  - 未知操作 → `operation_unavailable`；
  - 超过字节预算的单个条目 → `response_too_large`；
  - 其余 → `module_error`。
- 不读 `context` 以外的身份来源；`context` 缺字段时按 `invalid_input` 拒绝。
- 正文、抽取值不进日志和事件；日志只记操作、`run_id`、`tool_call_id`、结果码和字节数。

### 3.3 切片（每片 ≤ 300 行、≤ 8 文件，按顺序）

| 片 | 内容 | 关键测试 |
|---|---|---|
| A | 入口骨架：路由、解析 `context`/`arguments`、信封序列化、错误映射、未知操作 | 非法正文、缺 `context`、未知操作都返回 2xx + 对应错误码；客户端经代理访问 `_a24/tools` 得 404 |
| B | `list`、`get`、`job_get` | 结果与同参数的 REST 响应一致；`list` 在字节预算处截断并给 `next_cursor` |
| C | `read_range`、`find` | 大文档按字节预算分页，拼回后与 REST 全量一致；单条超预算返回 `response_too_large` |
| D | `extract` | 首次提交返回 `job_id`；完成后同参数重调返回结果且不新建 job；结果分页 |
| E | `domain-os.yml` 声明六个工具 | 用内核同一校验函数解析 manifest 通过；六个全名正确；把任一工具的 `operation` 改成带点的名字，校验失败（反面对照） |

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

- 片 1：六个工具在 daemon 启动时登记成功（日志或 `GET` 工具清单可见），远端档位存在时不通告；REST 契约测试全过。
- 对话入口（内核授予路径和 K1-6b 之后）：在 agent 页（`/api/v1/runs`）里，用户确认启用后，agent 能 list / get / read_range / find / extract / job_get 已导入的文档，每次调用记审计。这一项不属于本轮片 1 的验收。

## 6. 记录在案的缺口

- 远端档位守卫只在通告时检查，派发时不查（`agent24-tools/src/lib.rs:1329-1345`）。实际影响小（同意门仍会拦住），归入 §4 第 3 条。
- `docs/agent/tasks.md:48-54` 的 K1 台账仍写 K1-5/6/7 为 `PLANNED`/`BLOCKED`，与 main 不符，K1 线下次改台账时更正。
- 导入不通告（Q4）：对话里只能操作页面里已导入的文档，DOC-1 期间接受。
