# DOC-1-06b —— 文档操作作为模块工具（#705 的 agent 暴露）

> 状态：设计，2026-10-18。负责人：David。任务：#705 剩余部分（agent 暴露 + 对话入口）。
> 依据：ADR-K1-01（模块工具契约）、ADR-K1-02（数据流）、ADR-K1-03（启用授权）、ADR-DOC-01 D4/D8、ADR-DOC-02 §4。K1 已随 #1022 发布到 main。

## 0. 结论

- 本轮分两段：**片 1（本设计，Documenting 模块侧）** 和 **内核授予路径（K1 线，片 1 之后另出设计，走 `ab/kernel`）**。
- 片 1 交付后的状态是**「已登记、不通告、调不到」**：文档模块声明七个工具（`risk: read`；其中 `extract` 会在本机提交抽取任务，其余六个只读）并实现 `_a24/tools` 入口，但内核没有授予入口、远端档位守卫仍在，产品行为不变。
- 对话入口用 agent 页的 `POST /api/v1/runs`；聊天页 `/api/v1/chat` 不带工具，不改（David，2026-10-18）。
- **「K1-6a 与 K1-6b 都完成并验收前，文档工具不授予」是本设计的前提**：验收按 ADR-K1-02:71，含反例测试和 #708 产品验收；只是合入，或只完成 6a，都不够（K1-6b 是工具结果有自己的敏感分类和出口门，#870/#874）。在此之前文档工具一律不授予，任何片都不撤远端档位守卫（David，2026-10-18）。前提落在「不授予」而不是「不通告」，理由是：有了授予以后，不通告不等于调不到。`register_module_tool` 把模块工具放进 `allowed`（`agent24-tools/src/lib.rs:1131-1139`）；`dispatch`（`:1329-1400`）只查工具存在、白名单和审批门，不调 `advertisable()`；`ModuleTool` 的调用路径只查授予（`:796`）和 `check_available`（`:840` → `agent24d/src/module_tool_runtime.rs:66-76`），不查 `blocked_by_remote_tier_guard`。模型按名字直接调用即可绕过通告。ADR-K1-02:71 也写明不能依赖这道旧门保护。
  - **现状（2026-10-28）：前提已满足。**
    - K1-6a 已合入 main（#1280）；K1-6b 的 6b.5a–5g 都已合入（工具结果打来源标签、run 之后的模型调用按来源 LocalOnly、模块的模型回调一律 LocalOnly）。
    - #708 产品验收的证据见 #1419 正文，David 于 2026-10-28 确认足够。
    - 远端档位守卫已由 #1419 在 ab/kernel 撤除（be5b3020），随下一次内核发布进 main，再合回 ab/documenting。本段引用的 `blocked_by_remote_tier_guard` 及其行号是撤门前的代码。
    - 前提满足不等于已授予：documents 的 manifest 至今没有声明 `tools`。正式声明会改变对外行为，走 #705，由 David 定。manifest 是编进模块二进制的（`agent24-documents/src/lib.rs:42` 的 `include_str!`），握手时内核核对它的 digest（`agent24-os-proto/src/supervisor.rs:50`，对不上即 `manifest_mismatch`），所以声明时 manifest 和二进制要一起发布。

## 1. 现状（main @ 1ef85127）

| 项 | 现状 | 出处 |
|---|---|---|
| 工具声明 | `domain-os.yml` 可选的 `tools`；字段 `operation`、`description`、`input_schema`、`risk`、`output_privacy`、`timeout_ms`、`inline_wait_ms`、可选 `reversible_draft`；`deny_unknown_fields`，任一工具不合法则整份 manifest 被拒、模块挂不上 | `agent24-domain/src/tool.rs:112-162,271-287` |
| 操作名 | `[a-z0-9][a-z0-9_-]*`，**不能带点**；全名 `<module>.<operation>` | `tool.rs:112-120,363` |
| 调用 | `POST /api/v1/<module>/_a24/tools/<operation>`，正文 `{"context":…,"arguments":…}`，`context` 由宿主注入（`run_id`、`tool_call_id`、`module_id`、`operation`，`session_id` 可为 null）。不带额外请求头（`extra_headers` 为空） | `agent24d/src/module_tool_runtime.rs:80-115`（`:101` 空请求头）；`agent24-tools/src/lib.rs:473-481,858-866` |
| 期限 | `inline_wait_ms` 是同步等待的期限：内核用 `tokio::time::timeout(inline_wait, …)` 包住对模块的调用，超时记 `result_unknown`。`timeout_ms` 是整次调用的外层硬上限，从派发前的审计和同意检查算起包住整次调用，超时后由中断钩子 `ModuleTool::audit_interrupted_with_invocation`（`agent24-tools/src/lib.rs:703-767`）决定结果。`run_budgeted` 只有在钩子返回 `None` 时才走 `:1537` 的 `unknown_module_outcome("timeout")`。主路径上（没有已落库终态、审计正常，钩子得到 `AppendedUnknown` / `NotFound`）模型看到 `code:"result_unknown"`、`retryable:false`、`result_unknown:true`（`:1047-1050`）；钩子在拿锁、构造时间戳或追加审计时失败会返回 `Err`（`:715-718,745,767`），钩子返回 `Err` 或超过 5 s 时，`run_budgeted` 给出 `code:"module_tool_audit_degraded_result_unknown"`（`:1538-1540`）；终态已落库（`InterruptedTerminalOutcome::Existing`）时还原那个终态：`Success` 返回 `{"completed":true,"response_available":false}`（`:756-762`），其他终态经 `persisted_terminal_error`（`:763-765,906-926`）还原成原来的错误码（可能是 `timeout`、`cancelled`、`permission_denied`、`module_error`、`result_unknown`）。`timeout_ms` 同时也是发给模块的 HTTP 期限（`KernelLimits.total`）。正常情况下先到 `inline_wait`。取值来自 manifest。模块自身处理预算必须严格小于 `inline_wait_ms` 并留余量（`inline_wait_ms=25000` 时预算 20000）。模块自身处理超出预算时返回 `error{code:"internal", retryable:true}`（`module_code` 为 `internal`）。不用 `timeout`：`timeout` 是内核码（`agent24-domain/src/tool.rs:513`），模块返回它时 `module_code` 为空，模型会把它当成宿主侧的超时（终态已落库时还原出的宿主超时也是 `timeout`），含义混淆。`extract` 不等待抽取完成，只提交或重放后立即返回 | `agent24-tools/src/lib.rs:679-680,703-767,868-884,906-926,1047-1050,1486-1542`；`agent24d/src/server.rs:1634-1635`；`module_tool_runtime.rs:108-110`；`agent24-domain/src/tool.rs:513` |
| 结果信封 | `completed{payload,replayed}` / `pending{job}` / `error{code,retryable,details}`。内核**先解析信封再看状态码**：非 2xx 加合法的 `error` 信封时错误码保留；非 2xx 配 `completed` / `pending`，或正文解析失败，才是 `invalid_result`。模块仍一律返回 2xx。信封 `deny_unknown_fields`，`next_cursor` 等字段只能放在 `payload` 里。响应上限 **64 KiB**；`pending` 不交给模型，收成 `result_unknown` | `agent24-os-proto/src/kernel_call.rs:96-98,132-143,326-329`；`module_tool_runtime.rs:18`；`agent24-tools/src/lib.rs:886-900` |
| 错误码 | `invalid_input`、`permission_denied`、`module_unavailable`、`operation_unavailable`、`rate_limited`、`timeout`、`cancelled`、`response_too_large`、`invalid_result`、`module_error`、`result_unknown`；其他码收成 `module_error`，原码放进 `module_code`（只在不属于这张码表时才有值）。交给模型的错误 JSON 只有 `code`、`retryable`、`module_code`、`result_unknown`，没有 `details` | `agent24-domain/src/tool.rs:491-521`；`agent24-tools/src/lib.rs:1017-1044` |
| 授予 | 生产代码里没有调用 `grant_module_consent`，HTTP 只有列出、导出、撤销 | `agent24d/src/server.rs:1081-1092` |
| 风险 | 生产装配把所有模块工具写死为 `ManualInstall` + `External` + `High` | `server.rs:1576-1604` |
| 远端档位守卫 | 有任一远端档位时，`documents` 的工具不通告 | `module_tool_runtime.rs:119-128,172-174`；`server.rs:1618` |
| SDK | `agent24-os-sdk` 没有 `_a24/tools` 的入口，模块自己在 router 上实现 | — |

## 2. 契约修订（本设计 PR 一并改 ADR）

1. **工具名去掉操作名里的点**（ADR-DOC-02 §4 表格）：`documents.job.get` → `documents.job_get`；片 2 的 `change.propose`、`revision.list`、`revision.get` 同样改成 `change_propose`、`revision_list`、`revision_get`。全名仍按第一个点切出模块名。
2. **工具响应 ≤ 64 KiB**（ADR-DOC-02 §4「分页」）：REST 单页仍 ≤ 512 KiB；工具路径另按字节预算分页，序列化后的信封不超过 64 KiB（留 4 KiB 余量，预算 60 KiB），超出就提前截断并在 `payload` 里返回 `next_cursor`（信封 `deny_unknown_fields`，不能放在顶层，见 §1）。预算作为服务层分页函数的参数传入，`next_cursor` 在这个预算下算出，不能先按 512 KiB 生成再事后截断。现有服务层把 512 KiB 写死（`agent24-documents/src/documents.rs:18`、`fit_page` `:166-189`、`read.rs:22`、`extractions.rs:22`）：REST 仍传 512 KiB，工具路径传 60 KiB。单个条目本身超过预算时返回 `error{code:"payload_too_large"}`，不截断条目内容。不用内核码 `response_too_large`：内核 `result_code` 把它归成 `ResultUnknown`（`agent24-tools/src/lib.rs:962-967`），`module_error_json` 给模型 `result_unknown:true`（`:1017-1044`），把确定的拒绝报成结果未知。`payload_too_large` 在 `DocumentsErrorCode` 闭集里（`protocol/openapi.yaml:3578-3600`；`agent24-documents/src/error.rs:113`），内核收成 `module_error`，原码进 `module_code`。
3. **抽取不用 `pending`，结果用 `extraction_get`**（ADR-DOC-02 §4 extract 行）：`documents.extract` 只提交或重放，立即返回 `completed{payload:{status, job_id}}`，不等待抽取完成，不带抽取值，也不带 `extraction_id`。`extraction_id` 只在任务成功、提交事务时生成（`agent24-documents/src/extract/job.rs:216-231` 生成 id，`:247-267` 与 job 的 `succeeded` 写在同一事务）；只有 `succeeded` 的 job 才带 `result.extraction_id`（`jobs.rs:113-117`）。agent 用 `documents.job_get` 查进度，成功后把该 id 交给只读工具 `documents.extraction_get` 分页读取。重放沿用 ADR-DOC-02 §5：业务键命中 `succeeded` 返回原 job；命中 `failed` 或 `interrupted` 把同一个 job 重新排队（`extract/handler.rs:203-218,253-255`），等于重跑；同一业务键但请求正文的规范化哈希不同，返回 422 `idempotency_key_reused`（`handler.rs:102-103,199-200`）。工具路径发给服务层的请求正文是 `{revision, schema}`，不含 `document_id`、`context`、`rerun` 和其他工具专有字段。REST 先规范化请求正文，再算 `request_sha256`（片 D1 改 `extract/handler.rs:102-103` 的哈希输入）：`rerun` 为 false 时从正文里删掉这个字段，不补上。OpenAPI 现在仍写对整个请求正文做 JCS 哈希（`protocol/openapi.yaml:2219-2221`），片 D1 同步这句。不带 `rerun` 的旧请求哈希不变，已存 job 不受影响；显式 `"rerun": false` 与省略落到同一哈希，重放同一个 job，不 422。`ExtractRequest` 的字段仍是 `revision`、`schema`、`rerun`（省略时 serde 收成 false，`:48-53`）；规范化之后哈希不同仍返回 422（`:199-200`）。验收 harness 的普通抽取只发 `{revision, schema}`（`packages/documents-accept/src/extract-checks.ts:24`）；OpenAPI 的 `required` 只有 `revision`、`schema`，`rerun` 可省略，`default: false`（`protocol/openapi.yaml:4290-4308`）。唯一的残余：以前若有显式发 `rerun: false` 存下的 job，这个业务键此后会一直 422：`claim()` 在返回 422 前什么都不写（`handler.rs:199-200`），哈希只在键不存在时插入（`:236-244`），存下的旧哈希不会被改掉。目前没有客户端显式发 `rerun: false`，所以没有存量。
4. **关联信息在正文**（ADR-DOC-01 D4.3）：内核把 `run_id`、`tool_call_id` 放在请求正文的 `context` 里，不是 `x-a24-run-id` / `x-a24-tool-call-id` 头。模块以正文为准。操作日志在来源为 `run` 时记下 `run_id` 和 `tool_call_id` 两列。认领函数接收 `origin` 参数（片 D1）。现在 `extract/handler.rs:226-229` 插 job 时把 `origin` 写死为 `{"kind":"page"}`。表结构支持 `{"kind":"run","run_id","tool_call_id"}`（`migrations/0001_init.sql:64-82`），`0005_job_input.sql:18-21` 禁止事后改 `origin`。工具路径传 `{"kind":"run", run_id, tool_call_id}`。重放已有 job 时 job 的 `origin` 保持首次来源。生产里写 `oplog` 的只有导入（`imports/worker.rs:178-186`），`extract` 现在不写。片 D1 提供写入 `oplog` 的服务层函数。片 D2：工具发起的 extract（新建或重放）调用它，在 `oplog` 写一行，`origin` 列是 `'run'`，并填 `run_id`、`tool_call_id`（`migrations/0001_init.sql:99-110`：`origin` 只能是 `'page'` 或 `'run'`，`'run'` 时这两列必填）。这是两列加一个短字符串，不是 jobs 表那种 JSON。只读工具不写 `oplog`，内核审计已覆盖。

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

- 流程：`extract` → `completed{payload:{status, job_id}}`；agent 用 `job_get` 查进度；成功后 `job_get` 的 `result.extraction_id` 交给 `extraction_get`。`extract` 的参数里没有 `cursor`。`extraction_get` 对应 `GET /extractions/{id}`（`agent24-documents/src/lib.rs:184-187`；`protocol/openapi.yaml:2263-2282`），仍按 60 KiB 预算分页；单条值超过预算返回 `error{code:"payload_too_large"}`，不截断（见 §2 第 2 条）。REST 单条值最大 508 KiB（512 KiB 页减去 4 KiB 信封，`extractions.rs:22-28`）。第一条放不下一页时，现在 `broken()` 返回 `ApiError::internal`（`extractions.rs:222-224`）；片 D2 在工具预算下改为 `payload_too_large`。
- `extract` 不支持 `rerun:true`。`rerun:true` 需要 `Idempotency-Key` 请求头（`openapi.yaml:2214-2218`），内核工具请求不带额外请求头（`module_tool_runtime.rs:101`）。工具路径的请求正文是 `{revision, schema}`，不带 `rerun`（§2 第 3 条）。
- 全部 `risk: read`、`output_privacy: local_only`，不声明 `reversible_draft`。`input_schema` 取 `protocol/openapi.yaml` 参数的子集，排除项见上，`additionalProperties: false`。
- 不通告：upload、import、render、capabilities、job 的 cancel / retry（ADR-DOC-02 §4，不变）。
- 工具操作名和 `GET /capabilities` 的 `op` 不是同一套。`/capabilities` 不改名，名字以 `SLICE1_OPERATIONS` 为准（`agent24-documents/src/lib.rs:96-106`）：`upload`、`import`、`get`、`list`、`job`、`render`、`read_range`、`find`、`extract`。其中没有 `job_get`、`extraction_get`。

| 工具操作 | `/capabilities` 的 `op` |
|---|---|
| `list` | `list` |
| `get` | `get` |
| `read_range` | `read_range` |
| `find` | `find` |
| `extract` | `extract` |
| `job_get` | `job` |
| `extraction_get` | `job`（只要求存储） |

- Q7 落地时，内核按这张表过滤通告和调用（ADR-K1-01:55：`/capabilities` 里未知或不可用的操作不可通告、不可调用；ADR-DOC-02 §8 与 Q7）。`job_get` 与 `extraction_get` 都看 `job`。`extraction_get` 只要求存储：`get_extraction` 的参数是 `Ready(storage)`（`extractions.rs:120-121`）。`/capabilities` 的 `extract` 是 `Needs::Model`（`agent24-documents/src/lib.rs:105,148`），要引擎和模型都就绪，所以 `extraction_get` 不看它。过滤落地前工具照常通告，调用时返回类型化错误（ADR-DOC-02 §8）。没有引擎时 `find` / `extract` 在调用时返回 `engine_unavailable`（§3.2；ADR-DOC-02 §3.1、§8）。
- `description` 写清用途，不含示例文档内容。`extract` 单独写明：会在本机提交抽取任务（不修改文档），结果用 `documents.job_get` 查进度，成功后用 `documents.extraction_get` 读取；可能返回 `engine_unavailable`，超过约 60 KiB 的单个值不能经工具读取。其余六个写「只读、本机」。`risk: read` 仍是七个都声明（见上）。

### 3.2 入口与行为

- 在 documents 的 router 上加 `POST /_a24/tools/{op}`，与 REST 路由**调用同一个服务层**（ADR-DOC-02 §4），不经 HTTP 自调用。
- 一律返回 2xx 加信封。
- **重放标记**。命中业务键时按 job 的状态分（`extract/handler.rs:203-218,253-255`）：`queued` / `running` / `succeeded` 返回原 job，信封 `replayed: true`（`agent24-os-proto/src/kernel_call.rs:101-102`；ADR-K1-01:87 允许）。`failed` / `interrupted` 由 `claim` 重新排队并再执行一次，属于新执行，`replayed: false`。`cancelling` / `cancelled` 不在重排条件里（`:204` 只查 `failed` 和 `interrupted`；`jobs.status` 允许 `cancelling`，`migrations/0001_init.sql:70`），返回原 job，同样 `replayed: true`。新建 job 为 `false`。片 D2 的测试断言这几支。
- **分页预算**。预算作为服务层分页函数的参数传入，`next_cursor` 在这个预算下算出，不能先按 512 KiB 生成再事后截断。现有分页把 512 KiB 写死（`documents.rs:18`、`fit_page` `:166-189`、`read.rs:22`、`extractions.rs:22`）。REST 仍传 512 KiB；工具路径传 60 KiB（§2 第 2 条）。
- **第一条放不下**。
  - `extraction_get`：单条值可达约 508 KiB（`MAX_VALUE_BYTES`，`extractions.rs:22-28`）。第一条放不下一页时，现在 `broken()` 返回 `ApiError::internal`（`:222-224`）。片 D2 在工具预算下改为 `payload_too_large`，并加测试。
  - `list`：`fit_page`（`documents.rs:166-189`）一条都放不下时返回 `None`。调用方（`:154-160`）现在走 `storage.db_failure`。片 B 改为 `payload_too_large`。
  - `read_range` / `find`：单个 block 受 `MAX_BLOCK_BYTES`（16 KiB）和 `MAX_LINES`（64）限制（`text_layer.rs:20-21,185-189`）。返回前按实际覆盖的页重算 `unparsed_regions`（`read/text.rs:181`、`read/find.rs:194`）。pdfkit 每个未解析页一个矩形（`engines/pdfkit/ocr.swift:131,139`）。block 按 UTF-8 计 ≤ 16 KiB（`check_block` 看 `b.text.len()`，`text_layer.rs:184-215`）。JSON 转义可能膨胀：控制字符写成 `\u00XX`（`idem.rs:36-37`：`\b` `\f` `\n` `\r` `\t` 是短转义，其余控制字符是 `\u00xx`），16 KiB 的块序列化后可以超过 60 KiB。所以固定开销加第一条仍超预算时返回 `payload_too_large`，这个兜底不是纯防御。片 C 用这样的块做反例测试。层校验允许一个 region 最多 16 个矩形（`MAX_REGION_RECTS`，`text_layer.rs:23`；再多就收成整页，`text_layer/build.rs:94-96`），一次响应最多覆盖 `PAGES`（100）页（`read.rs:24`）。现有 `used` 先按这一窗的全部 regions 预留，再装条目（`read/text.rs:153-158`、`read/find.rs:149-158`）。工具预算下是否放得下，按将返回的正文算（固定开销、这一条、实际覆盖页的 regions）；预留不能单独把第一条判超。
- **期限**。`inline_wait_ms` 是同步等待的期限：内核用 `tokio::time::timeout(inline_wait, …)` 包住对模块的这次调用，超时记 `result_unknown`（`agent24-tools/src/lib.rs:868-884`）。`timeout_ms` 是整次调用的外层硬上限，也是发给模块的 HTTP 期限（`KernelLimits.total`，`module_tool_runtime.rs:108-110`）。`ModuleTool::timeout()` 返回 `timeout_ms`（`:679-680`），`run_budgeted`（`:1486-1542`）用它包住整次调用，从派发前的审计和同意检查算起（同意在 `:796`，派发前审计在 `:810`，都在这次计时之内）。到点先取消，再等 5 s 宽限，仍未结束就调中断钩子 `audit_interrupted_with_invocation`。`:1537` 的 `unknown_module_outcome("timeout")` 只在钩子返回 `None` 时才走到；`ModuleTool` 的钩子见 `agent24-tools/src/lib.rs:703-767`。主路径上（没有已落库终态、审计正常，钩子得到 `AppendedUnknown` / `NotFound`）模型看到 `code:"result_unknown"`、`retryable:false`、`result_unknown:true`（`:1047-1050`）；钩子在拿锁、构造时间戳或追加审计时失败会返回 `Err`（`:715-718,745,767`），钩子返回 `Err` 或超过 5 s 时，`run_budgeted` 给出 `code:"module_tool_audit_degraded_result_unknown"`（`:1538-1540`）；终态已落库（`InterruptedTerminalOutcome::Existing`）时还原那个终态：`Success` 返回 `{"completed":true,"response_available":false}`（`:756-762`），其他终态经 `persisted_terminal_error`（`:763-765,906-926`）还原成原来的错误码（可能是 `timeout`、`cancelled`、`permission_denied`、`module_error`、`result_unknown`）。正常情况下先到 `inline_wait`（`inline_wait` 必须短于 `timeout`，`:626-627`；取值来自 manifest，`agent24d/src/server.rs:1634-1635`）。模块对每次工具调用的自身处理预算严格小于 `inline_wait_ms` 并留余量：`inline_wait_ms=25000` 时模块预算 20000。模块自身处理超出预算时返回 `error{code:"internal", retryable:true}`（`module_code` 为 `internal`）。不用 `timeout`：`timeout` 是内核码（`agent24-domain/src/tool.rs:513`），模块返回它时 `module_code` 为空（`result_code` 把它归成 `Timeout`，`agent24-tools/src/lib.rs:957-961`），模型会把它当成宿主侧的超时（终态已落库时还原出的宿主超时也是 `timeout`），含义混淆。`extract` 不等待抽取任务完成：只提交或重放后立即返回。
- **错误映射**。交给模型的错误 JSON 只有 `code`、`retryable`、`module_code`、`result_unknown`（`agent24-tools/src/lib.rs:1017-1044`）。`module_code` 只在错误码不属于内核码表时才有值（`agent24-domain/src/tool.rs:506-521`）。
  - 参数分两层（与 ADR-DOC-02 §6 工具路径一致）：在模块入口按 `input_schema` 校验不过（结构层面）→ 内核码 `invalid_input`，`module_code` 为空；通过 schema、进入服务层后的业务参数错误 → `DocumentsErrorCode` 的 `invalid_request`，内核收成 `module_error`，原码进 `module_code`（见下一条）；
  - 业务上需要 agent 区分的错误，直接用模块自己的码作 `code`。码以 `protocol/openapi.yaml` 的 `DocumentsErrorCode`（`3578-3600`）和 ADR-DOC-02 §6 的闭集为准，不另造。本片用到的是：`not_found`（文档、revision、job、抽取不存在都是这一码；闭集里没有 `document_not_found`、`revision_not_found`、`extraction_not_found`、`job_not_found`）、`invalid_request`（schema 通过之后的业务校验）、`unsupported_format`、`parse_failed`、`partial_parse`（`require_complete` 时文本不完整；闭集里没有 `text_incomplete`）、`idempotency_key_reused`、`payload_too_large`（单条超过工具字节预算），以及不可重试的 `storage_unavailable`（`corrupt`、`not_writable`、`disk_full`）。内核把这些码收成 `module_error`，原码放进 `module_code`；`retryable` 按 ADR-DOC-02 §6 该错误是否可重试填写（上列都是 `false`）；
  - `engine_unavailable`（Linux 等没有引擎，503，可重试；ADR-DOC-02 §3.1、§8；`extract/handler.rs:106-117`；`read.rs:85-89`）原样返回，`retryable:true`。`module_unavailable` 在内核码表里（`agent24-domain/src/tool.rs:510`）。把这一码放进应答时 `module_code` 为空，和宿主侧的停用、熔断分不开。ADR-K1-01 第 55 行讲宿主侧生命周期，第 75 行（§2.4）规定模块细分码收成 `module_error` 并保留原码。文档模块的应答不用 `module_unavailable`；
  - 存储 `locked` / `busy` 返回原码 `storage_unavailable`，`retryable:true`；`corrupt` / `not_writable` / `disk_full` 仍是原码 `storage_unavailable`，`retryable:false`。模块给出的 `retryable` 由内核保留（`agent24-tools/src/lib.rs:1022,1037`）；
  - 未知操作由内核在派发前拦下：名字不在工具表里是 `unknown tool`（`agent24-tools/src/lib.rs:1337-1341`）；操作不在登记集合里，`check_available` 走 `unavailable()`（`module_tool_runtime.rs:54-60`，只在 `:71-76` 调用），得到内核码 `module_unavailable`、`retryable:false`。模块返回 `operation_unavailable` 只是 manifest 与 router 不一致时的防御；
  - 超过字节预算的单个条目 → `payload_too_large`（不用内核码 `response_too_large`，见 §2 第 2 条）；
  - 意外故障返回 `error{code:"internal", retryable:false}`。模块自身处理超出预算时返回 `error{code:"internal", retryable:true}`。同一个码，靠 `retryable` 区分：意外故障 `false`，超出处理预算 `true`。两者都不是结果未知，`module_code` 都是 `internal`。`internal` 不在 `DocumentsErrorCode` 闭集里（`protocol/openapi.yaml:3578-3600` 没有这一项），也不在内核码表里；内核收成 `module_error`（ADR-DOC-02 §6：这仍是 OS 产生、却不在本命名空间闭集里的唯一一个码，但有两种场景）。
- 模型看不到 `details`。交给模型的错误 JSON 没有这个字段；内核模块工具审计事件也没有 `details` 槽（`agent24-store/src/module_audit_event.rs:396-414`，`deny_unknown_fields`），`details` 写不进这条审计。agent 要区分的信息放在模块信封的 `code` 里（模型看到的是 `module_code`）。
- `context` 宽松解析，不用 `deny_unknown_fields`（先例 `kernel_call.rs:154-172` 的 `FiredBody`）。必填 `run_id`、`tool_call_id`、`module_id`、`operation`；`session_id` 可以为 null（`agent24-tools/src/lib.rs:473-481`；临时 run 没有会话）。缺必填字段 → `invalid_input`。不读 `context` 以外的身份来源。内核每次调用还注入 `authorized_resources`、`authorization_ref`（`agent24-tools/src/lib.rs:479-480,864-865`）。生产里 `authorized_resources` 恒为空（`agent24-policy/src/consent_gate.rs:244`）。片 1 忽略这两个字段：宽松解析收下，但不使用。不能把 `authorized_resources` 当白名单（授予之后每次调用都会被拒），也不能「非空才检查」（检查失败时会放行）。
- 正文、抽取值不进日志和事件；日志只记操作、`run_id`、`tool_call_id`、结果码和字节数。

### 3.3 切片（每片 ≤ 300 行、≤ 8 文件，按顺序）

| 片 | 内容 | 关键测试 |
|---|---|---|
| A | 入口骨架：路由、解析 `context`/`arguments`、信封序列化、错误映射、未知操作 | 非法正文、缺 `context`、未知操作都返回 2xx + 对应错误码；客户端经代理访问 `_a24/tools` 得 404 |
| B | `list`、`get`、`job_get` | 结果与同参数的 REST 响应一致；`list` 在字节预算处截断并给 `next_cursor`；`fit_page` 返回 `None` 时调用方返回 `payload_too_large`（`documents.rs:154-160`） |
| C | `read_range`、`find` | 大文档按字节预算分页，拼回后与 REST 全量一致。普通正文下第一条在 60 KiB 下放得下；JSON 转义膨胀时固定开销加第一条仍超预算，返回 `payload_too_large`（§3.2） |
| D1 | REST：extract 请求哈希规范化（`extract/handler.rs:102-103`，`rerun` 为 false 时省略再哈希）；同步 `protocol/openapi.yaml:2219-2221`（现在写对整个请求正文做 JCS 哈希）；同步改 `packages/contract-tests/src/openapi-documents-extract.test.ts:88`（`toMatch(/JCS hash of the whole request body/)` 锁住这句）；重新生成 `packages/api-client/src/openapi.d.ts`（第 1200 行是生成副本）；认领函数接收 `origin`（`handler.rs:226-229`）；写入 `oplog` 的服务层函数 | 显式 `rerun: false` 与省略命中同一 job、不 422；不带 `rerun` 的旧哈希不变；`origin` 透传；`pnpm gen:api`（根 `package.json` 的 `pnpm --filter @agent24/api-client gen`）之后无漂移 |
| D2 | 工具：`extract` 与 `extraction_get`；`extraction_get` 的 `payload_too_large`（`extractions.rs:222-224`）；信封 `replayed`；工具发起时写 `oplog` | 首次返回 `{status, job_id}`；成功后 `job_get` 给出 `extraction_id`，值都不超过预算时，`extraction_get` 分页读回与 REST 一致；`queued` / `running` / `succeeded` 或 `cancelling` / `cancelled` 返回原 job、`replayed: true`；`failed` / `interrupted` 重新排队再执行一次，`replayed: false`；新建时为 `false`；`extraction_get` 第一条放不下时是 `payload_too_large`（改 `extractions.rs:222-224`）；工具发起的抽取（新建或重放）写一行 `oplog`（`origin` 为 `'run'`）；页面先占键后工具重放，job 的 `origin` 仍是 `{"kind":"page"}`，`oplog` 有本次 run 的一行 |
| E | `domain-os.yml` 声明七个工具 | 用内核同一校验函数解析 manifest 通过；七个全名正确；把任一工具的 `operation` 改成带点的名字，校验失败（反面对照）。另加一个 agent24d 测试，只加测试，不改内核行为（§5） |

片 D1（REST）：认领函数接收 `origin` 参数（改 `extract/handler.rs:226-229`，现在写死 `{"kind":"page"}`）。页面仍传 `{"kind":"page"}`。重放已有 job 时 job 的 `origin` 保持首次来源。`0005_job_input.sql:18-21` 禁止事后改 `origin`。哈希：改 `extract/handler.rs:102-103` 的输入，`rerun` 为 false 时先省略再算；同步 `protocol/openapi.yaml:2219-2221`。`packages/contract-tests/src/openapi-documents-extract.test.ts:88` 的 `toMatch(/JCS hash of the whole request body/)` 锁住这句，一并改。`packages/api-client/src/openapi.d.ts` 第 1200 行是生成副本，用根 `package.json` 的 `pnpm gen:api`（即 `pnpm --filter @agent24/api-client gen`）重新生成，之后无漂移。测试覆盖显式 `rerun: false` 与省略命中同一 job、不 422，不带 `rerun` 的旧哈希不变，以及 `origin` 透传（§2 第 3 条）。这一片提供写入 `oplog` 的服务层函数，调用放在片 D2。

片 D2（工具）：`extract` 与 `extraction_get`。工具路径传 `{"kind":"run", run_id, tool_call_id}`。`extraction_get` 在工具预算下把 `extractions.rs:222-224` 的 `ApiError::internal` 改为 `payload_too_large`，并加测试。`queued` / `running` / `succeeded` 或 `cancelling` / `cancelled` 返回原 job、信封 `replayed: true`；`failed` / `interrupted` 重新排队再执行，属于新执行，`replayed: false`；新建 job 为 `false`（§3.2）。测试断言这几支。`oplog`：工具发起的 extract，新建和重放都写一行，`origin` 为 `'run'`，带这次的 `run_id`、`tool_call_id`（§2 第 4 条）。测试沿用拆分前这一片的工具侧条目：首次返回 `{status, job_id}`；成功后 `job_get` 给出 `extraction_id`，值都不超过预算时，`extraction_get` 分页读回与 REST 一致；`failed` / `interrupted` 重新排队再执行且 `replayed: false`；页面先占键、工具重放时，job 的 `origin` 仍是 `{"kind":"page"}`，`oplog` 有本次 run 的一行。只读工具不写 `oplog`。

E 放最后：manifest 只要有一个工具不合法，整个模块就挂不上；先有入口再声明，任何一片单独合入都不改变产品行为。片 E 之后的 documents 包只能装到认识 `tools` 的 daemon（K1 之后）。`RawManifest` 是 `deny_unknown_fields`（`agent24-domain/src/lib.rs:415-417`），`tools` 是 K1 加的；旧 daemon 见到 `tools` 会拒整份 manifest，模块挂不上。`DAEMON_PROTOCOL_VERSION` 仍是 2（`:410`），`min_daemon_protocol` 表达不了「需要 K1」。documents 靠 `agent24 os install` 手装。片 E 的 PR 同步 `rust/apps/agent24-documents/README.md` 的安装说明。

### 3.4 不变量（每片 PR 写明并有测试或检查）

- 不实现、不调用任何授予接口；不改内核的风险装配和远端档位守卫。
- 工具结果 ≤ 64 KiB；不用 `pending` 信封。
- 正文不进日志和事件。
- REST 行为除 extract 请求哈希规范化外不变（显式 `rerun:false` 与省略从 422 变为命中同一 job）。现有契约测试在这条之外全过。省略 `rerun` 的现有请求哈希保持不变（§2 第 3 条）。`list` 在 `fit_page` 返回 `None` 时改为 `payload_too_large`；契约允许的一行只有几 KiB（`documents.rs:158` 的注释），512 KiB 的 REST 页走不到这条。

## 4. 内核授予路径（K1 线，片 1 之后另出设计）

只定方向，详细设计在片 1 合入后另起，走 `ab/kernel` 的 task PR：

1. 授予 API 与桌面确认界面：逐个工具展示 ADR-K1-03 的四项（可读、可改、可外发、宿主风险），用户确认后调用 `grant_module_consent`。
2. 风险按声明与确认推导，两样分开。用户逐个确认之后，内核 `RiskClass` 可跟随声明的 `risk`（ADR-DOC-01 D4.2「用户在启用时逐个确认过的工具」），替换 `server.rs:1598,1636` 写死的 `External`。`HostRiskLevel` 另算：第三方（`ConsentSource::ManualInstall`）由 `ToolPermissionSummary::new` 钳回 `High`（`agent24-store/src/module_consents.rs:123-127`）；宿主判定第一方（`ConsentSource::FirstParty`）时沿用传入的 `host_risk`（`:124`）。
3. 派发时重查远端档位守卫（ADR-K1-01 §2.2「派发前重新检查所有门」；现在只在通告时查，`dispatch` 和 `ModuleTool` 调用路径都不查，见 §0）。**顺序约束**：这一条必须不晚于第 1 条（授予 API 与确认界面）和第 2 条（`RiskClass` 跟随声明）落地，最好同一片；不能先有授予、后补派发时的守卫。要有测试证明：有授予、有远端档位时，按名字直接 `dispatch` 文档工具被拒。
4. 与 K1-6a / K1-6b 的关系：**「K1-6a 与 K1-6b 都完成并验收前，文档工具不授予」是本设计的前提**（§0）。即使授予路径做好，在此之前也不给文档工具授予：有了授予以后，不通告不等于调不到。撤远端档位守卫的条件同样按 ADR-K1-02:71：K1-6a 与 K1-6b 都完成并验收，含反例测试和 #708 产品验收；只是合入，或只完成 6a，都不够。
5. 资源范围：授予时如何填 `authorized_resources`（例如按 `document_id`）由授予路径设计决定。文档模块在工具变得可调用之前，补上按 `authorized_resources` 的校验（ADR-K1-01 §2.3）。片 1 不校验（§3.2）。
6. 协议号：`tools` 是 K1 加进 `RawManifest` 的，旧 daemon 因 `deny_unknown_fields` 会拒带 `tools` 的 manifest；`DAEMON_PROTOCOL_VERSION` 仍是 2，`min_daemon_protocol` 表达不了「需要 K1」（§3.3 片 E）。请内核考虑升协议号，或给 `tools` 一个能力门。
7. 模型可见的函数名：全名 `documents.list` 带点，`OpenAiCompatProvider` 原样写进 `function.name`（`agent24-models/src/lib.rs:829`），而 OpenAI / Anthropic 的函数名只允许 `[a-zA-Z0-9_-]`。MCP 用 `mcp_{server}_{tool}` 避开（`agent24-mcp/src/lib.rs:184-186`）。函数名形状由内核处理。片 1 不改。

## 5. 验收（#705 剩余部分）

- 片 1：片 E 加一个 agent24d 测试，只加测试，不改内核行为。装入 documents 的 manifest 后，`GET /api/v1/tools`（`agent24d/src/routes.rs:722-723`，列出 `state.tools.list()`，实现见 `agent24-tools/src/lib.rs:1237`）含七个全名。通告要能证明守卫在起作用：`advertisable`（`agent24-tools/src/lib.rs:652-665`）同时要求模块就绪、操作已登记、`has_current_consent`、远端档位守卫为假。生产没有授予（`agent24d/src/server.rs:1081-1092` 只有列出、导出、撤销），generation 不是 `Running` 时 `module_ready` 也为假（`module_tool_runtime.rs:152-154`；`generation` 在 `:48-49` 要求 `DrainState::Running`）。测试在模块已运行、且注入了有效授予（`ModuleToolAuthorization`）的前提下，对比有无远端档位：无远端档位时七个工具可通告，有远端档位时不可通告；或直接断言 `blocked_by_remote_tier_guard`（`module_tool_runtime.rs:172-174`）。`live_adverts`（`agent24-tools/src/lib.rs:1271`）只收录 `advertisable` 的工具。REST 契约测试全过。
  - 这是片 1 时、撤门前的验收。#1419 撤掉远端档位守卫时，这几条门测试随门一起删除（ab/kernel be5b3020）；之后通告不再看远端档位，远端由逐 run 的出口门挡住（见 §0 的现状）。
- 对话入口（内核授予路径，以及 K1-6a 与 K1-6b 都完成并验收之后；含反例测试和 #708 产品验收，只是合入或只完成 6a 都不够，ADR-K1-02:71。**「K1-6a 与 K1-6b 都完成并验收前，文档工具不授予」是本设计的前提**，见 §0）：在 agent 页（`/api/v1/runs`）里，用户确认启用后，agent 能 list / get / read_range / find / extract / job_get / extraction_get 已导入的文档，每次调用记审计。这一项不属于本轮片 1 的验收。

## 6. 记录在案的缺口

- 远端档位守卫只在通告时检查，派发时不查（`agent24-tools/src/lib.rs:1329-1345`）。目前没有授予，同意门会拒绝；有了授予以后不通告不等于调不到（§0），所以 §4 第 3 条必须不晚于授予落地（可同一片）。
- `docs/agent/tasks.md:48-54` 的 K1 台账仍写 K1-5/6/7 为 `PLANNED`/`BLOCKED`，与 main 不符，K1 线下次改台账时更正。
- 导入不通告（Q4）：对话里只能操作页面里已导入的文档，DOC-1 期间接受。
- 片 1 不按 `authorized_resources` 校验（§3.2）。生产里这个字段恒为空（`agent24-policy/src/consent_gate.rs:244`）。当白名单会在授予后每次拒绝，「非空才检查」会在检查失败时放行。授予时如何填资源范围，以及文档模块在工具可调用之前补上校验，归入 §4 第 5 条（ADR-K1-01 §2.3）。
- 通告给模型的全名带点（`documents.list`）。`OpenAiCompatProvider` 原样写进 `function.name`（`agent24-models/src/lib.rs:829`）；OpenAI / Anthropic 的函数名只允许 `[a-zA-Z0-9_-]`。MCP 用 `mcp_{server}_{tool}` 避开（`agent24-mcp/src/lib.rs:184-186`）。模型可见的函数名形状由内核处理（§4 第 7 条）。片 1 不改。
- 工具路径读不到超过预算的单个抽取值，REST 可读。游标按值切（`extractions.rs:110-112`，`next_cursor` 形如 `v{ord}`，`:226`），一条放不进 60 KiB 就返回 `payload_too_large`，不在值内分段。单值最大约 508 KiB（`MAX_VALUE_BYTES`，`:22-28`），REST 的 512 KiB 页放得下。以后可让 `extraction_get` 支持值内分段。
- 工具路径的 `find` 遇到超过预算的单条匹配就停住，后面的匹配读不到，REST 可读。query 里的空白能匹配任意长的一段空白，例如 `"a b"` 匹配 `"a"+16000×U+000B+"b"`，U+000B 转义后占 6 字节，一条 anchor 就超过 60 KiB。这时返回 `payload_too_large`，游标不能跳过这一条。REST 的 512 KiB 页放得下。以后可让游标跳过超预算的条目，或在匹配里截断 `quote`。
