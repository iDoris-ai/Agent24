# ADR-DOC-02：DocumentService 操作契约

> **状态**：Proposed。生效条件同 ADR-DOC-01：PR-Daemon APPROVE + jason 确认，§9 的决策项由 David 拍板。
> **日期**：2026-10-07 · **作者**：David Xu · **Issue**：#701（DOC-1-02 第 2 部分）
> **依赖**：[ADR-DOC-01](ADR-DOC-01-placement-and-integration.md)（进程外 OS `documents`、模块声明工具、类型化 preload、D8 隐私污点、D9 审计）。
> **依据**：[README](../README.md) §2.2 / §2.3 / §5 / §7 / §8 / §9.1 / §10 / §11 / §14 / §17 #1–#3 / §22.1；[BASELINE](../BASELINE.md) §7–§8。
>
> 文中行号按 `dd8f7ff` 核对。

## 0. 范围

本 ADR 规定 DOC-1 的操作契约：存储与唯一可编辑权威（#690，README §17 #1）、标识与寻址、操作清单与风险映射、唯一提交点与幂等、类型化错误、job、可用性发现。DOC-2 及以后的操作只定语义方向，细节随对应阶段补 ADR。

## 1. 现状（与契约相关的事实）

| 项 | 事实 |
|---|---|
| 数据目录 | 模块启动时拿到 `A24_DATA_DIR`（`SPEC-ME3-OUT-OF-PROCESS.md:58`），即 `~/.agent24/os/documents/`。内核没有给文档用的 blob 区（BASELINE §7） |
| 迁移 | store 在打开时跑 `sqlx::migrate!`，编号只追加（`agent24-store/src/lib.rs:143`） |
| 版本 CAS | `ArtifactStore::cas_write(a, expect_version)`，版本不符返回 `Conflict`，不覆盖（`agent24-memory/src/artifact.rs:104-109`、`:202-205`）；schedule 的 `revision = revision + 1 … WHERE id = ? AND revision = ?`（`agent24-store/src/module_schedules.rs:273-274`） |
| 幂等 | `FireId` = 带域分隔前缀的 SHA-256，只由业务坐标决定、与重试次数无关（`agent24-scheduler/src/fire.rs:25-44`）；落库用 `ON CONFLICT (fire_id) DO NOTHING`（`module_schedules.rs:443`）；`module_approvals` 用 `UNIQUE (module, request_id, kind)`（`migrations/0005_module_approvals.sql:39`） |
| job | **没有** job 抽象（BASELINE §8） |
| 错误信封 | `{error:{code,message,hint?,details?}}`（`agent24-domain/src/http.rs:35`；`agent24-protocol/src/types.rs:247-264`），`code` 是开放枚举；413 用 `payload_too_large`（`http.rs:121-122`） |
| 工具错误 | `ToolError` = `Invalid / Denied / Failed / Timeout / Cancelled / AbortRun`，只带字符串（`agent24-tools/src/lib.rs:153-170`） |
| 代理 | 首字节 10 s、总时长 30 s（`agent24-os-proto/src/proxy.rs:116`、`:124`）；响应体同样 1 MiB 上限，超出 502 `upstream_response_too_large`（`proxy.rs:1709-1722`）；**只自动重试无 body 的 GET/HEAD/OPTIONS**（`proxy.rs:1546-1556`）；在途时模块被停返回 503 `request_abandoned`，结果未知（`proxy.rs:1286-1298`） |
| 事件 | `module` 信封：`payload.module` 必须等于 manifest id，`payload.kind` 为点分事件名，`payload.payload` 不透明（`protocol/events.schema.json:762-786`）；经 `_a24/events/emit` 发出，只能发自己的事件（`SPEC-ME3-OUT-OF-PROCESS.md:194`），回调错误闭集里有 `rate_limited`（`:177`） |

## 2. 存储与唯一可编辑权威（#690）

| 方案 | 说明 | DOC-1 评估 |
|---|---|---|
| **A OS 管理原件与版本**（推荐） | `data_dir` 内：`documents.db`（SQLite WAL，自带编号迁移）+ 内容寻址 blob 区 `blobs/sha256/<2>/<62>`。原件和每个 revision 都是不可变 blob；元数据、change set、job、抽取结果、产物、操作日志在 DB | 与 ADR-DOC-01 D1/D7 一致；DOC-1 不需要 Line K（README §14）；“知识关闭”和“原件存储不可达”天然是两个状态（§9.1） |
| B KB（WeKnora）管理不可变原件，OS 只管 revision | 原件进 KB，OS 引用 KB 资源 id | 导入/阅读都依赖 Line K 在线，违反 DOC-1“不需要 Line K”；原件在两处有身份 |
| C 外部系统（Drive / SharePoint 等） | OS 只存映射和缓存 | 需要 connector 与凭据，单用户本机阶段不成立；留作 DOC-4 之后的导入/交付来源 |

**建议 DOC-1 采用 A**，并约束：

1. **唯一可编辑权威**是 OS 中某文档的 `head_revision`。KB 索引、用户记忆只引用 `document_id + revision`（README §5“三个库三个主人”），不持有可编辑副本。
2. **写入顺序**：blob 先写 `tmp/` → fsync → rename 到内容地址（同 hash 已存在则跳过）→ 再在事务里写引用它的行。未被引用的 blob 由启动时 GC 在宽限期后清理。崩溃最多留下孤儿 blob，不会留下指向缺失 blob 的行。
3. **scratch workspace 永远不是已保存 revision 的家**。DOC-1 完全不用它（ADR-DOC-01 D7）。以后 agent run 在 scratch 里产出的临时文件，**只有**经 `import`（新文档）或 `revision.commit`（新 revision）复制进 blob 区、校验 sha256 后才算保存；在 TTL 内未提升即视为丢弃，UI 如实显示。
4. 存储布局不对外暴露，对外只用 §3 的标识（ADR-DOC-01 D1，为向内核名词迁移留路）。

留给 David 决定的见 §9 Q1–Q3。

## 3. 标识与寻址

- **`document_id`**：`doc_` + ULID，由 OS 生成，不由路径、文件名或内容推导。同一份字节导入两次是两个文档（blob 去重，UI 提示“内容相同”）。
- **`revision`**：每文档从 1 开始的连续整数。导入的原件是 r1；之后**只有** `revision.commit` 产生新值（`head + 1`）。revision 行不可变。对外写作 `doc_…@3`。
- **`content_sha256`**：该 revision 文件字节的 `sha256:<hex>`，导出、引用、产物绑定（§7.1）都带它。
- **派生数据**（文本层、块、页面渲染）按 `(content_sha256, engine_id, engine_version)` 缓存，可重建，不是权威。

**溯源锚点**（README §8 / §11.4 的逐值溯源）：

```json
{ "document_id": "doc_…", "revision": 1, "content_sha256": "sha256:…",
  "page": 3, "block_id": "p3-b12",
  "text_range": { "unit": "utf8", "start": 120, "end": 168 },
  "bbox": { "unit": "pt", "origin": "top-left", "rect": [72.0, 410.5, 300.2, 428.0] },
  "quote": "原文片段", "engine": { "id": "…", "version": "…" } }
```

- `page` 为 1 起的物理页序号（不是印刷页码）；`block_id` 只在该 revision + 引擎版本内稳定，所以锚点**必须**带 revision。
- `text_range` 相对 `block_id` 的块文本，半开区间 `[start, end)`。**单位定为 UTF-8 字节**，理由：OS 与引擎侧是 Rust/UTF-8，无需换算；块文本按引擎输出原样存储、不做 Unicode 规范化，偏移因此确定；起止必须落在字符边界，否则拒收（`invalid_request`）。渲染进程（JS 是 UTF-16）由 `api-client` 提供换算函数，CJK / emoji 一律经它，不允许前端自行按 `length` 计算。
- `bbox` 用于扫描件/OCR 和版面定位：PDF 点（1/72 in），原点在**按旋转后显示**的页面左上角。纯文本格式省略。
- `quote` 是校验用的原文片段：UI 取锚点文本与 `quote` 不一致时显示“定位失效”，**不猜位置**（README §8）。
- 找不到可核验来源的值：`anchor: null` + `unsourced_reason`，界面显示为“无来源”。

## 4. 操作清单（DOC-1）

公共路由前缀 `/api/v1/documents`，页面经 ADR-DOC-01 D5 的类型化 preload 调用；Agent 工具名 `documents.<op>`，经内核 `_a24/tools/<op>` 调用（D4）。所有返回文档内容或元数据的操作 `output_privacy: local_only`（D8），DOC-1 不设例外。

**风险映射**（README §10.2）：业务标签 → 用户确认后的内核 `RiskClass`（`types.rs:1036-1050`）：read → `Read`；mutate-draft / commit / create-artifact → `WriteLocal`；handoff → `External`。**DOC-1 期间没有第一方默认安装，所有 Documenting 工具实际按 `External` 处理、每次审批，除非用户在启用时逐个确认**（ADR-DOC-01 D4.2）。

| 片 | 操作 | REST | 工具 | 知识类 | 业务风险 |
|---|---|---|---|---|---|
| 1 | upload | `POST /uploads`；`POST /uploads/{id}/chunks`（≤768 KiB，带 offset/sha256 头） | 不通告 | free | create-record |
| 1 | import | `POST /imports {upload_id}` → 202 job | 不通告（Q4） | free | create-record |
| 1 | get / list | `GET /documents`、`GET /documents/{id}` | `documents.list`、`documents.get` | free | read |
| 1 | render | `GET /documents/{id}/revisions/{rev}/pages/{n}?scale=`（单页 ≤ 1 MiB，超出分块） | 不通告 | free | read |
| 1 | read_range | `GET /documents/{id}/revisions/{rev}/text?page=&block=` | `documents.read_range` | free | read |
| 1 | find | `POST /documents/{id}/find {revision, query}` | `documents.find` | free | read |
| 1 | extract | `POST /documents/{id}/extractions {revision, schema}` → 202 job；`GET /extractions/{id}` | `documents.extract` | free（显式给定文档） | read |
| 1 | job | `GET /jobs/{id}`、`POST /jobs/{id}/cancel` | `documents.job.get`、`documents.job.cancel` | free | read |
| 1 | capabilities | `GET /capabilities`（§8） | 随工具通告 | free | read |
| 2 | change.propose | `POST /documents/{id}/changes {base_revision, ops}` | `documents.change.propose` | optional | mutate-draft |
| 2 | change.review | `POST /changes/{cs}/review {decisions}` | **不通告** | free | mutate-draft |
| 2 | revision.commit | `POST /documents/{id}/revisions {base_revision, change_set_id, change_set_sha256}` | **不通告**（Q5） | free | commit |
| 2 | revision.list / get | `GET /documents/{id}/revisions[/{rev}]` | `documents.revision.list` | free | read |
| 2 | export | `POST /documents/{id}/revisions/{rev}/exports {format, options}` → 202 job；`GET /exports/{id}/content?offset=`（≤768 KiB 分块） | `documents.export` | free | create-artifact |

- 二进制只走 octet-stream 分块（D5），因为代理请求、响应**两个方向**都是 1 MiB 上限。
- DOC-1 的 agent 入口只读 + 提议：agent 能读、找、抽取、提议修改、导出已提交的 revision；**review 和 commit 只在页面由用户完成**，满足 README §11.6 单用户下“编辑与批准是不同的确认步骤”。
- **之后的阶段**（只定方向）：`search`（required，DOC-2）；`ask / summarize / compare / translate`（按 §9.1 逐次分类，DOC-2）；`draft.create`、`revision.restore`（DOC-3，restore = 以目标 revision 内容生成 change set 再走 commit，产生新 revision，带 `restored_from`）；`preflight`（read）、`package.assemble`（create-artifact，绑定已批准 revision，§7.1）、`delivery.prepare`（handoff → `External`，副作用由 Agent24 执行，DOC-4）。

## 5. 唯一提交点与幂等

1. **只有 `revision.commit` 创建 revision**。`change.review` 只记录逐条 accept/reject 决定，返回该 change set 当前的 `change_set_sha256`；“接受并保存”是页面组合：一次 review + **一次** commit。restore 也走 commit。
2. **`change_set_sha256`** = 对 `{document_id, base_revision, ops, decisions}` 做 RFC 8785（JCS）规范化后的 SHA-256。改任何一条决定都会得到新值。
3. **提交键** `commit_key` = `SHA-256("agent24-doc-commit-v1\0" ‖ document_id ‖ "\0" ‖ base_revision ‖ "\0" ‖ change_set_sha256)`，照 `FireId` 的域分隔写法。`revisions` 表上 `UNIQUE (commit_key)` 和 `UNIQUE (document_id, revision)` 双重约束。
4. **提交算法**（单个 `BEGIN IMMEDIATE` 事务，blob 已按 §2.2 先落盘）：
   1. 按 `commit_key` 查已有行：命中则返回该 revision（200，`replayed: true`）。**必须先于 CAS**，否则成功后的重试会因 head 已前进而误报冲突；
   2. `head_revision ≠ base_revision` → 409 `revision_conflict`，`details.current_revision`；
   3. 校验 change set 属于该文档、全部条目已决定、哈希一致；
   4. 插入 `revision = base + 1`，`UPDATE documents SET head_revision = ? WHERE id = ? AND head_revision = ?`（同 `module_schedules.rs:273-274` 的 CAS 形状），写操作日志（D9）。返回 201。
5. **其他写操作的幂等**：`import` 以 `upload_id` 为键；`export` 以 `(document_id, revision, format, options_sha256)` 为键；`change.propose`、`extract` 以调用方幂等键为键。键都用 UNIQUE + 命中即返回原记录，不用 `ON CONFLICT DO NOTHING` 静默吞掉。
6. **调用方幂等键**：agent 路径用内核传入的 `x-a24-tool-call-id`（D4.3）；页面路径用主进程按用户动作生成的 `Idempotency-Key` 头（非 `X-A24-*`，否则会被代理剥掉）。commit 以 `commit_key` 为准，`tool_call_id` 只记入日志。
7. **不自动重试**：内核工具调用不重试（D4.3），代理也不重试 POST（`proxy.rs:1546-1556`）。遇到 503 `request_abandoned` / 504 `upstream_timeout` 这类“结果未知”，客户端用**同一个键**重发，由上面的规则保证不重复。

## 6. 类型化错误

`documents` 命名空间的错误码是**闭集**，新增需改本 ADR。都放在现有信封 `{error:{code,message,hint?,details?}}` 里；工具路径由内核代理工具映射到 `ToolError`，`code` 作为消息前缀 `"<code>: <message>"` 保留给模型。

| code | HTTP | ToolError | 说明 |
|---|---|---|---|
| `invalid_request` | 400 | `Invalid` | 参数错误、锚点不在字符边界 |
| `not_found` | 404 | `Invalid` | 文档 / revision / job 不存在 |
| `payload_too_large` | 413 | `Invalid` | 单块或请求超限（与 `http.rs:121-122` 同码） |
| `unsupported_format` | 415 | `Invalid` | 格式不在 §17 #3 已声明范围 |
| `parse_failed` | 422 | `Failed` | 解析整体失败 |
| `partial_parse` | 422 | `Failed` | 仅当调用方要求 `require_complete`；否则是 200 + `parse_status: partial` + 未解析区域列表（§11.7） |
| `revision_conflict` | 409 | `Failed` | `details.current_revision` |
| `stale_index` | 409 | `Failed` | 索引落后于请求的 revision（DOC-2 起出现，§7） |
| `knowledge_disabled` | 409 | `Failed` | 用户关闭知识；required 类操作暂停（§9.1） |
| `knowledge_unavailable` | 503 | `Failed` | 知识服务未就绪 |
| `engine_unavailable` | 503 | `Failed` | 引擎未安装或失败；`details.engine` |
| `module_unavailable` | 503 | `Failed` | **只由内核工具代理产生**（D4.4）；REST 路径上模块停机时由代理返回自己的 `module_*` / `upstream_*` 码 |
| `permission_denied` | 403 | `Denied` | OS 侧拒绝（如未确认的操作）；内核审批拒绝仍是内核自己的 `Denied` |
| `cancelled` | 409 | `Cancelled` | 读取已取消 job 的结果 |

## 7. Job

- **OS 自有 `jobs` 表**：`job_id`（`job_` + ULID）、`kind`、`document_id`、`revision`、`status`（`queued / running / cancelling / succeeded / failed / cancelled / interrupted`）、`progress {stage, done, total, unit}`、`error {code, message}`、`result_ref`、`idempotency_key UNIQUE`、`origin`（`page` 或 `run_id + tool_call_id`，仅作信息）、时间戳。
- **在期限内返回**：启动型操作只做校验 + 写 job 行，然后返回 202 `{job_id}`，远低于 10 s 首字节期限；工作在 OS 后台执行。工具调用在 `KernelLimits.total` 内等结果，到期仍未完成就返回 `{job_id, status}`，agent 用 `documents.job.get` 查询。
- **进度事件**：manifest 申请 `events`，经 `_a24/events/emit` 发 `payload.module = documents`，`kind` ∈ `job.progress`、`job.finished`、`document.imported`、`revision.committed`。每个 job 进度事件限速 ≤ 2 次/秒。**事件只是提示，权威是 job 行**；断线重连后用 `GET /jobs/{id}` 对账。
- **取消**：`POST /jobs/{id}/cancel` 幂等，置 `cancelling`，工作线程在阶段边界检查，并终止引擎子进程；未提交的临时输出丢弃。若 job 已越过自己的提交点（产物行已写），取消失败，job 如实以 `succeeded` 结束。取消**从不**回滚已提交的 revision（commit 是同步操作，不是 job）。
- **崩溃恢复**：OS 启动时把 `running` 改为 `interrupted`、`cancelling` 改为 `cancelled`，清 `tmp/`，GC 孤儿 blob。**不自动续跑**（尤其是调模型的 extract）；用户或 agent 显式重试时复用同一幂等键，确定性 job 从最后完成的阶段（如页）续做。
- **agent run 取消**：目前内核不会通知模块。run 取消后 job 继续，结果保留可查；`_a24/tools/<op>` 的取消信号列为内核 ADR 的需求（§9 Q7）。

## 8. 可用性与发现（README §10.3）

`GET /capabilities`，同一份数据供内核过滤工具通告（D4.4）：

```json
{ "os_version": "…", "storage": { "state": "ready|degraded|unavailable" },
  "engines": [{ "id": "…", "kind": "parse|ocr|render|export", "formats": ["pdf"],
                "state": "absent|installing|ready|failed", "version": "…" }],
  "knowledge": { "setting": "on|off",
                 "state": "registered|loading|ready|degraded|unavailable|disabled" },
  "operations": [{ "op": "extract", "available": false, "reason": "engine_unavailable" }] }
```

- `knowledge.state` 沿用 README §9.2 的六态；DOC-1 没有 required 类操作，只报告，不阻塞。
- `storage.unavailable` 与知识关闭是不同状态（§9.1）：此时所有写操作不可用。
- `reason` 取自 §6 闭集。

## 9. 待决事项

| # | 问题 | 建议 | 决定人 |
|---|---|---|---|
| Q1 | #690：DOC-1 是否采用方案 A（OS 管理原件与版本） | 采用；B/C 作为以后的引用/来源，不做权威 | David |
| Q2 | 删除与保留：DOC-1 是否提供删除；删“唯一原件”的确认流程（§11.6）；blob GC 宽限期 | DOC-1 不提供删除；GC 宽限 7 天 | David |
| Q3 | 资料库的备份 / 迁出格式、静态加密、磁盘配额 | DOC-1 不做，记为发布前事项 | David / jason |
| Q4 | agent 是否能导入文件（来源只能是用户选择或 scratch，后者 DOC-1 不用） | DOC-1 不通告 import 工具 | David |
| Q5 | agent 是否能在用户确认后 commit | DOC-1 不通告；DOC-3 随编辑/批准分离再议 | David / jason |
| Q6 | 偏移单位 UTF-8 字节（§3）是否接受 | 接受 | David |
| Q7 | 内核工具 ADR 需带：结构化工具错误码（`ToolError` 目前只有字符串）、工具调用取消信号、按 `/capabilities` 过滤通告 | 写入 C 的内核 ADR 需求 | jason / Agent24 kernel |
| Q8 | `partial_parse` / `stale_index` 作为结果状态还是错误的边界 | 默认是结果状态，只在显式要求时报错 | David |
