# ADR-DOC-02：DocumentService 操作契约

> **状态**：Proposed。生效条件同 ADR-DOC-01：PR-Daemon APPROVE + jason 确认；§9 的决策项由 David 拍板（Q1、Q2、Q4、Q5 已定）。
> **日期**：2026-10-07 · **作者**：David Xu · **Issue**：#701（DOC-1-02 第 2 部分）
> **依赖**：[ADR-DOC-01](ADR-DOC-01-placement-and-integration.md)（进程外 OS `documents`、模块声明工具、类型化 preload、D8 由内核执行的资料处理政策、D9 审计）。
> **依据**：[README](../README.md) §2.2 / §2.3 / §5 / §7 / §8 / §9.1 / §10 / §11 / §14 / §17 #1–#3 / §22.1；[BASELINE](../BASELINE.md) §7–§8。
>
> 文中行号按 `dd8f7ff` 核对。

## 0. 范围

本 ADR 规定 DOC-1 的操作契约：存储与唯一可编辑权威（#690，README §17 #1）、标识与溯源锚点、操作清单与风险映射、唯一提交点与幂等、类型化错误、job、可用性发现。DOC-2 及以后的操作只定方向。

## 1. 现状（与契约相关的事实）

| 项 | 事实 |
|---|---|
| 数据目录 | manifest 的 `data_dir` 必须等于由 `name` 推导的 `~/.agent24/os/documents/`（`rust/crates/agent24-domain/src/lib.rs:552-554`、`:797-800`），启动时以 `A24_DATA_DIR` 交给模块（`docs/specs/SPEC-ME3-OUT-OF-PROCESS.md:58`）。内核没有给文档用的 blob 区（BASELINE §7） |
| 迁移 / SQLite | store 用 WAL 并在打开时跑编号只追加的 `sqlx::migrate!`（`rust/crates/agent24-store/src/lib.rs:136`、`:143`） |
| 版本 CAS | `ArtifactStore::cas_write(a, expect_version)` 版本不符返回 `Conflict`（`rust/crates/agent24-memory/src/artifact.rs:104-109`、`:202-205`）；schedule `… WHERE id = ? AND revision = ?`（`rust/crates/agent24-store/src/module_schedules.rs:273-274`） |
| 幂等 | `FireId` 是带域分隔前缀、只由业务坐标决定的 SHA-256（`rust/crates/agent24-scheduler/src/fire.rs:25-44`），落库 `ON CONFLICT (fire_id) DO NOTHING`（`module_schedules.rs:443`）；`UNIQUE (module, request_id, kind)`（`rust/crates/agent24-store/migrations/0005_module_approvals.sql:39`） |
| job | **没有** job 抽象（BASELINE §8） |
| 错误信封 | `{error:{code,message,hint?,details?}}`，`code` 是开放枚举（`rust/crates/agent24-protocol/src/types.rs:247-264`）；413 用 `payload_too_large`（`rust/crates/agent24-domain/src/http.rs:121-122`） |
| 工具错误 | `ToolError` = `Invalid(String) / Denied(String) / Failed(String) / Timeout(Duration) / Cancelled / AbortRun(String)`（`rust/crates/agent24-tools/src/lib.rs:153-170`）。agent loop 把 `Cancelled` 与 `AbortRun` 同等处理，**会取消整个 run**（`rust/crates/agent24-agent/src/lib.rs:1347`、`:1986`） |
| 代理 | 首字节 10 s、总 30 s（`rust/crates/agent24-os-proto/src/proxy.rs:116`、`:124`）；响应体同样 1 MiB 上限（`:1709-1722`）；只自动重试无 body 的 GET/HEAD/OPTIONS（`:1546-1556`）；未派发即拒绝的 `module_stopping` / `module_overloaded` 等（`:1303-1436`），以及派发后结果未知的 503 `request_abandoned`（`:1286-1298`） |
| 事件 | `module` 信封：`payload.module` = manifest id，`payload.kind` 为点分名（`protocol/events.schema.json:762-786`）。`_a24/events/emit` 每个 generation 令牌桶容量 20、每秒补 5（`rust/apps/agent24d/src/events_emit.rs:23-25`），payload ≤ 256 节点、字符串 ≤ 8 KiB（`:39-41`）。`EventsHub` 是广播给**所有** WS 客户端的通道（`rust/apps/agent24d/src/events.rs:22-30`，每个 WS 订阅在 `:134`） |

## 2. 存储与唯一可编辑权威（#690）

| 方案 | 说明 | DOC-1 评估 |
|---|---|---|
| **A OS 管理原件与版本**（**已定**） | `data_dir` 内：`documents.db` 加内容寻址 blob 区 `blobs/sha256/<2>/<62>`。原件和每个 revision 都是不可变 blob | 与 ADR-DOC-01 D1/D7 一致；DOC-1 不依赖 Line K；“知识关闭”与“存储不可达”天然分开（§9.1） |
| B KB（WeKnora）管理不可变原件 | 原件放在 KB，OS 只管 revision | 导入、阅读都依赖 Line K 在线；同一原件在两处都有身份 |
| C 外部系统 | OS 只存映射和缓存 | 需要 connector 与凭据，留到 DOC-4 之后 |

**决定（David，2026-10-07）：采用 A。** B 以后仍可作为额外的**来源**：从 WeKnora 资源导入时记录 KB 资源 id，并把内容复制进 blob 区作为 r1。编辑权威仍在 OS，KB 继续引用 `document_id + revision`。这不需要重新设计。

1. **唯一可编辑权威**是 OS 中该文档的 `head_revision`。KB 索引和用户记忆只引用 `document_id + revision`（README §5）。OS **从不调用** `_a24/memory/*`，manifest 不申请 `memory`（M1）。
2. **持久化顺序**：blob 写到 `blobs/tmp/`（与目标同一文件系统）→ fsync 文件 → rename 到内容地址 → fsync 分片目录（分片目录是新建的还要 fsync 它的父目录）→ 然后才开 DB 事务写引用行。同一 hash 已存在就跳过写入。
   - SQLite 显式设置 `journal_mode=WAL` + `synchronous=FULL`，不依赖驱动默认值。理由：`NORMAL` 在系统崩溃或断电时可能丢掉最后几次已提交的事务，用户看到“已保存”的 revision 会消失；提交频率低，`FULL` 的代价可以忽略。
3. **scratch workspace 永远不是已保存 revision 的家**，DOC-1 不使用它（ADR-DOC-01 D7）。临时文件只有经 `import` 或 `revision.commit` 复制进 blob 区、校验过 sha256，才算保存。TTL 内没有提升的视为丢弃。
4. **删除与 GC（已定）**：DOC-1 不提供删除。没有被引用的 blob 保留 7 天后才 GC。`uploads/` 不参与这个 GC（§5.6）。
5. 存储布局不对外暴露（ADR-DOC-01 D1）。

## 3. 标识与溯源锚点

- **`document_id`**：`doc_` + ULID，由 OS 生成，不由路径、文件名或内容推导。同一份字节导入两次就是两个文档（blob 去重）。
- **`revision`**：每个文档从 1 开始连续编号，原件是 r1；之后**只有** `revision.commit` 产生 `head + 1`。revision 行不可变。**`content_sha256`** 是该 revision 文件字节的 `sha256:<hex>`。
- **文本层**：引擎从某个 revision 解析出的块和文本，存成不可变 blob（`text_layer_sha256`），并记录 `(content_sha256, engine id, version, config)`。**只要有锚点或抽取结果引用就钉住，永不淘汰**，引擎升级也不改写；没有引用的派生缓存（如页面渲染）可以淘汰重建。

**锚点**（README §8 / §11.4 逐值溯源）：

```json
{ "document_id": "doc_…", "revision": 1, "content_sha256": "sha256:…",
  "text_layer_sha256": "sha256:…", "engine": { "id": "…", "version": "…" },
  "block_id": "p3/b12", "page": 3, "block_text_sha256": "sha256:…",
  "text_range": { "unit": "utf8", "start": 120, "end": 168 },
  "geometry": { "box": "CropBox", "unit": "pt", "origin": "top-left-rotated",
                "rects": [[72.0, 410.5, 300.2, 428.0], [72.0, 430.0, 180.4, 447.5]] },
  "quote": "原文片段" }
```

- **定位**：`block_id` 是结构路径，DOCX 等流式格式用它定位，`page` 可省略；PDF / 扫描件给出 1 起的物理页号。`geometry` 以 PDF **CropBox** 为参照框，单位为乘过 `UserUnit` 的点，原点在应用 `/Rotate` 后显示页面的左上角；`rects[]` 覆盖跨行、跨栏的片段。
- **偏移**：单位是 **UTF-8 字节**，相对块文本，半开区间 `[start, end)`，按**逻辑（存储）顺序**计，不按双向文本的视觉顺序；起止必须落在字符边界，否则 `invalid_request`。
  - 理由：OS 和引擎都是 Rust/UTF-8；块文本原样存储、不做规范化，偏移因此确定。渲染进程统一经 `api-client` 的换算函数转成 UTF-16。
  - 高亮时界面扩展到**字素簇边界**（UAX #29：泰文、组合符、ZWJ 序列），存储的偏移不变。
- **重新解析与 OCR**：锚点绑定 `text_layer_sha256`，不要求 OCR 可复现。即使用同一个钉住版本重跑，也不假定逐字节一致，解析始终用那份存储的文本层。`block_text_sha256` + `quote` + `rects` 用于校验，不一致时显示“定位失效”，**不猜位置**。
- **不跨 revision 迁移**：r1 的锚点永远指向 r1。要在 r2 上定位，只能重新抽取。
- **无来源**：没有可核验来源的值，`anchor: null` 并附 `unsourced_reason`。

## 4. 操作清单（DOC-1）

- **调用路径**：路由前缀 `/api/v1/documents`，页面经 D5 的 preload 调用，agent 工具经内核 `_a24/tools/<op>` 调用。两条路径调用**同一个服务层**，操作日志记录来源（`page`，或 `run_id + tool_call_id`）。所有工具都声明 `output_privacy: local_only`（D8）。这只是**需求声明**，出站控制由内核的资料处理政策执行，见 ADR-DOC-01 D8 和 #735。
- **分页**：`list`、`find`、`read_range`、抽取结果、`job.get` 统一用 `cursor` / `limit`（默认 50，最大 200），返回 `next_cursor`；单页响应 ≤ 512 KiB，给 1 MiB 上限留余量。
- **风险映射**（用户确认后的内核 `RiskClass`，`types.rs:1036-1050`）：read → `Read`；create-record / mutate-draft / commit / create-artifact → `WriteLocal`；handoff → `External`。**DOC-1 没有第一方默认安装，所有工具实际按 `External` 处理、每次审批**，除非用户在启用时逐个确认（D4.2）。

| 片 | 操作 | REST | 工具 | 知识类 | 业务风险 |
|---|---|---|---|---|---|
| 1 | upload | `POST /uploads`（`Idempotency-Key`）；`POST /uploads/{id}/chunks`（≤768 KiB） | 不通告 | free | create-record |
| 1 | import | `POST /imports {upload_id}` → 202 job | **不通告**（Q4 已定：只从页面经原生文件对话框导入） | free | create-record |
| 1 | get / list | `GET /documents[/{id}]` | `documents.list`、`documents.get` | free | read |
| 1 | render | `GET /documents/{id}/revisions/{rev}/pages/{n}?scale=`（≤ 1 MiB；超出时自动降低 scale，仍超出则按瓦片分块） | 不通告 | free | read |
| 1 | read_range | `GET /documents/{id}/revisions/{rev}/text?block=&cursor=` | `documents.read_range` | free | read |
| 1 | find | `POST /documents/{id}/find {revision, query, cursor}` | `documents.find` | free | read |
| 1 | extract | `POST /documents/{id}/extractions {revision, schema}` → 202 job；`GET /extractions/{id}` | `documents.extract` | free（显式给定文档） | read |
| 1 | job | `GET /jobs/{id}`；`POST /jobs/{id}/cancel`；`POST /jobs/{id}/retry` | `documents.job.get`（cancel / retry 不通告，Q9） | free | read；cancel / retry 是 mutate-draft |
| 1 | capabilities | `GET /capabilities`（§8） | 不单独通告 | free | read |
| 2 | change.propose | `POST /documents/{id}/changes {base_revision, ops}` | `documents.change.propose` | optional | mutate-draft |
| 2 | change.review | `POST /changes/{cs}/review {decisions}` | **不通告** | free | mutate-draft |
| 2 | revision.commit | `POST /documents/{id}/revisions {base_revision, change_set_id, change_set_sha256}` | **不通告**（Q5 已定） | free | commit |
| 2 | revision.list / get | `GET /documents/{id}/revisions[/{rev}]` | `documents.revision.list`、`documents.revision.get` | free | read |
| 2 | export | `POST /documents/{id}/revisions/{rev}/exports {format, options}` → 202 job；`GET /exports/{id}/content?offset=` | `documents.export` | free | create-artifact |

- DOC-1 的 agent 只能读、查、抽取、提议修改、导出已提交的 revision。**导入只在页面经原生文件对话框完成（Q4）；review 和 commit 只在页面、在可见 diff 上由用户完成（Q5，README §11.6）**。以后若开放 agent 导入或对话内确认提交，按 `WriteLocal` 走审批，需修订本 ADR。
- **之后的阶段**（只定方向）：DOC-2 `search`（required）、`ask / summarize / compare / translate`（按 §9.1 逐次分类）；DOC-3 `draft.create`、`revision.restore`（用目标 revision 的内容生成 change set 再走 commit）；DOC-4 `preflight`、`package.assemble`（绑定已批准的 revision）、`delivery.prepare`（`External`，由 Agent24 执行）。

## 5. 唯一提交点与幂等

1. **只有 `revision.commit` 创建 revision**。`change.review` 只记录逐条决定并返回 `change_set_sha256`；“接受并保存” = 一次 review + **一次** commit；restore 也走 commit。已提交的 change set **冻结**，再 review 返回 `change_set_committed`。
2. **`change_set_sha256`** = `{ops, decisions}` 经 RFC 8785（JCS）规范化后的 SHA-256；文档和 base 不放进去，由提交键绑定。
   - **提交键** `commit_key` = `SHA-256(b"agent24-doc-commit-v1\0" ‖ utf8(document_id) ‖ b"\0" ‖ ascii_decimal(base_revision) ‖ b"\0" ‖ change_set_sha256 的 32 个原始字节)`，以小写十六进制存储。
3. **提交协议**：
   1. **只读预检（不加锁）**：
      - 按 `commit_key` 查键：命中且存储的 `change_set_id` 相同 → 返回原 revision（200，`replayed: true`）；`change_set_id` 不同 → 422 `idempotency_key_reused`。查键**先于 CAS**。
      - `head ≠ base` → 409 `revision_conflict`（`details.current_revision`）。
      - 哈希不符 → 409 `change_set_stale`；还有未决定条目 → 422 `change_set_incomplete`。
   2. **构建新字节**并按 §2.2 写入 blob。这一步超出 5 s 预算（远小于 10 s 首字节期限）时，立即返回 202 job，该 job 以 `commit_key` 为键。
      - **键在返回 202 之前就已占住**，`target_ref` 指向 job。
      - 之后再命中这个键：job 还在进行，就返回该 job；已成功，就返回它生成的 revision。
   3. **`BEGIN IMMEDIATE`**：重查键和 head；插入 `revision = base + 1`（`UNIQUE (document_id, revision)`），并 `UPDATE documents SET head_revision = ? WHERE id = ? AND head_revision = ?`；冻结 change set；写操作日志（D9）；返回 201。
   - **空提交**：全部条目被拒绝，或零个 op → 不产生 revision。返回 200 `{revision: head, committed: false}`，change set 冻结为 `closed_no_change`。目标内容与 head 相同的 restore 同样处理。
4. **键表** `idempotency(kind, key, request_sha256, target_ref, created_at)`，`UNIQUE (kind, key)`。kind 与键的来源：

     | kind | 键 |
     |---|---|
     | `upload` | 页面的 `Idempotency-Key` |
     | `import` | `upload_id` |
     | `extract` | `(document_id, revision, schema_sha256, extractor_version, model_id)`，键用实际的模型 id，不用 profile；需要重新抽取时，页面可以显式带 `rerun: true`，同时换一个新键 |
     | `propose` | `(document_id, base_revision, ops_sha256)` |
     | `export` | `(document_id, revision, format, options_sha256)` |
     | `commit` | `commit_key` |

   - 同一个键对应的 `request_sha256` 不同 → 422 `idempotency_key_reused`。
   - 占键在 `BEGIN IMMEDIATE` 内完成，由 SQLite 单写者串行化。若仍遇到 UNIQUE 冲突（其他连接），回滚后重读该行并按命中处理。
5. **agent 路径**：内核不重试，每次调用的 `tool_call_id` 都是新的，所以 **`tool_call_id` 只作信息记录，不作幂等键**，去重靠上表中由内容推导的键。页面遇到“结果未知”（`request_abandoned`、`upstream_timeout`）时用同一个键重发；代理不会重试 POST（`proxy.rs:1546-1556`）。
6. **上传**：状态在 `uploads/<upload_id>/` 和 `uploads` 表里，**不在 `tmp/`**，启动清理和孤儿 GC 都不碰它；最后一个块到达 24 h 后过期。每块带 `Upload-Offset` 和 `Chunk-Sha256`：
   - `offset` 等于已接收长度 → 追加并 fsync；这一段已接收且哈希相同 → 200 重放；其他情况 → 409 `upload_offset_mismatch`（`details.received_offset`）。
   - import 时校验整个文件的 sha256，不符 → 422 `upload_checksum_mismatch`。

## 6. 类型化错误

`documents` 命名空间的错误码是**闭集**，新增需修改本 ADR。

- **信封**：沿用现有信封；每个错误都带 `details.retryable`。**客户端按 `code` 分支，不按 HTTP 状态分支**。
- **工具路径**：映射成 `ToolError`，`code` 作为消息前缀。**任何模块响应都不得映射成 `ToolError::Cancelled`**，因为那会取消整个 run（§1）。
- **取消的 job 不是错误**：`GET /jobs/{id}` 返回 200，`status: cancelled`。`cancelled` 只出现在 `job.error.code` 里。

| code | HTTP | ToolError | 说明 |
|---|---|---|---|
| `invalid_request` | 400 | `Invalid` | 参数错误、锚点不在字符边界 |
| `not_found` | 404 | `Invalid` | 文档、revision、job、上传不存在或已过期 |
| `payload_too_large` | 413 | `Invalid` | 与 `http.rs:121-122` 同码 |
| `unsupported_format` | 422 | `Invalid` | 格式不在 §17 #3 已声明的范围内 |
| `parse_failed` | 422 | `Failed` | 解析整体失败 |
| `partial_parse` | 422 | `Failed` | 仅在请求 `require_complete` 时报错；默认是 200 + `parse_status: partial` + 未解析区域列表（§11.7） |
| `idempotency_key_reused` | 422 | `Invalid` | 同一个键对应了不同的请求 |
| `change_set_incomplete` | 422 | `Invalid` | 还有未决定的条目 |
| `upload_checksum_mismatch` | 422 | `Invalid` | 整个文件的哈希不符 |
| `knowledge_disabled` | 422 | `Failed` | 用户关闭了知识；required 类操作暂停（§9.1） |
| `revision_conflict` | 409 | `Failed` | `details.current_revision` |
| `change_set_stale` | 409 | `Failed` | change set 在客户端读取之后被重新 review 过 |
| `change_set_committed` | 409 | `Failed` | change set 已冻结；`details.revision` |
| `upload_offset_mismatch` | 409 | `Invalid` | `details.received_offset` |
| `stale_index` | 409 | `Failed` | 仅在请求 `require_fresh` 时报错；默认在结果里标出索引的 revision（§7） |
| `permission_denied` | 403 | `Denied` | OS 侧拒绝 |
| `knowledge_unavailable` | 503 | `Failed` | `retryable: true` |
| `engine_unavailable` | 503 | `Failed` | `details.engine`；`retryable: true` |
| `storage_unavailable` | 503 | `Failed` | `data_dir` 或 DB 不可用；与知识关闭是不同状态 |

**会看到但不属于本命名空间的内核 / 代理码**：

- **未派发，可以安全重试**：`module_overloaded`、`module_stopping`、`module_not_ready`、`module_draining`、`duplicate_request_id`（`agent24-os-proto/src/drain.rs:152-155`）。
- **派发了但结果未知，只能用同一个键重发**：
  - `request_abandoned`、`upstream_timeout`、`upstream_connection_closed`；
  - `upstream_unavailable`：它在 `proxy.rs:1587` 表示“未发出”，在 `:1731` 表示“响应中途断了”，无法区分，按未知处理；
  - `upstream_response_too_large`：模块可能已经执行；
  - `module_panicked`、`module_killed`、`stop_failed`。
- **默认规则**：未列出的码一律按“结果未知”处理，用同一个键重发。
- **内核工具代理产生**：`module_unavailable`（D4.4）。

## 7. Job

- **`jobs` 表**：`job_id`（`job_` + ULID）、`kind`、`document_id`、`revision`、`status`（`queued / running / cancelling / succeeded / failed / cancelled / interrupted`）、`attempt`、`progress {stage, done, total, unit}`、`error {code, message}`、`result_ref`、`origin`、时间戳；经键表与幂等键关联（§5.4）。
- **键命中时**：`queued` / `running` → 返回该 job；`succeeded` → 返回结果；`failed` / `interrupted` → **自动重新启用**（同一个 `job_id`，`attempt + 1`，回到 `queued`）。**`cancelled` 不会因键命中而重启**，返回原 job 及其 `cancelled` 状态，只能由用户显式调用 `POST /jobs/{id}/retry` 重启。这样，内容推导的键（例如 agent 调用 `extract` / `export`）不会悄悄撤销用户的取消。`POST /jobs/{id}/retry` 是同一动作的显式形式。
- **期限**：启动型操作只做校验、占键、写 job 行，立即返回 202 `{job_id}`。工具调用最多等 manifest 声明的 `inline_wait_ms`（**严格小于**注册的超时，至少留 5 s 余量），到期返回 `{job_id, status}`，agent 用 `documents.job.get` 查询。
- **完成**：结果行与 `status = succeeded` 在**同一个事务**里写入。
- **取消**：`POST /jobs/{id}/cancel` 幂等，置 `cancelling`；工作线程在阶段边界检查，终止引擎子进程，丢弃没有提交的输出。结果事务已提交的 job 保持 `succeeded`；commit job 一旦进入 `BEGIN IMMEDIATE` 就不可取消。
- **崩溃恢复**：OS 启动时 `running` → `interrupted`，`cancelling` → `cancelled`（结果与状态同事务，此时必然没有已提交的输出）；清理 `tmp/` 与 `blobs/tmp/`。**不自动续跑**：下一次带同一个键的调用重新启用 job，确定性 job 从最后完成的阶段续做。
- **事件**：manifest 申请 `events`，经 `_a24/events/emit` 发出，`payload.module = documents`，`kind` 取 `job.progress`、`job.finished`、`document.imported`、`revision.committed`。
  - **整个模块合计 ≤ 2 次/秒**（低于内核的每秒 5 次），进度按 job 合并只发最新一条；被 `rate_limited` 拒绝的进度事件直接丢弃。**终态事件**（`job.finished`、`revision.committed`）优先发送，被限流时短暂退避后重发一次；最终仍以 job 行为准。
  - **payload 只带 id、stage、计数、status、错误码**，不带标题、文件名、查询或内容：`EventsHub` 广播给所有 WS 客户端，D8 的资料处理政策管不到这条通道。
  - 事件只是提示，**job 行才是权威**；断线后用 `GET /jobs/{id}` 对账。
- **agent run 取消**：内核目前不会通知模块。run 取消后 job 继续运行，结果保留（Q7）。

## 8. 可用性与发现（README §10.3）

`GET /capabilities`：

```json
{ "os_version": "…", "storage": { "state": "ready|degraded|unavailable" },
  "engines": [{ "id": "…", "kind": "parse|ocr|render|export", "formats": ["pdf"],
                "state": "absent|installing|ready|failed", "version": "…" }],
  "knowledge": { "setting": "on|off",
                 "state": "registered|loading|ready|degraded|unavailable|disabled" },
  "operations": [{ "op": "extract", "available": false, "reason": "engine_unavailable" }] }
```

- `knowledge.state` 沿用 README §9.2 的六种状态。DOC-1 没有 required 类操作，所以只报告，不阻塞。
- `reason` 取自 §6 的闭集。
- **内核按这些数据过滤工具通告属于 Q7 的内核依赖**，目前不存在。在它落地前，工具照常通告，调用时返回类型化错误。

## 9. 待决事项

| # | 问题 | 建议 / 决定 | 决定人 |
|---|---|---|---|
| Q1 | #690：DOC-1 存储方案 | **已定（David，2026-10-07）**：方案 A；B 以后可以作为额外来源（§2） | David |
| Q2 | 删除与 GC 宽限期 | **已定（David，2026-10-07）**：DOC-1 不提供删除；未被引用的 blob 保留 7 天后 GC | David |
| Q3 | 资料库的备份 / 迁出格式、静态加密、磁盘配额 | DOC-1 不做，记为发布前事项 | David / jason |
| Q4 | agent 能否导入文件 | **已定（David，2026-10-07）**：DOC-1 不能，只从页面经原生文件对话框导入；以后可在审批下开放 | David |
| Q5 | agent 能否在用户确认后 commit | **已定（David，2026-10-07）**：DOC-1 没有 commit 工具，agent 只读和提议；review / commit 只在页面、diff 可见时进行；对话内确认提交以后可在审批下加入 | David |
| Q6 | 偏移单位用 UTF-8 字节（§3） | 接受 | David |
| Q7 | 内核工具 ADR 需要提供：结构化工具错误码、工具调用的取消信号、按 `/capabilities` 过滤工具通告、`inline_wait_ms` 声明 | 写入 C 的内核 ADR 需求 | jason / Agent24 kernel |
| Q8 | `partial_parse` / `stale_index` 何时作为结果状态、何时作为错误 | 默认作为结果状态，只在显式要求时报错 | David |
| Q9 | 是否向 agent 通告 `job.cancel` / `job.retry` | DOC-1 不通告；如果通告，按 `WriteLocal` 处理，且只能操作同一个 run 启动的 job | David |
| Q10 | commit 的内联预算（5 s）和 `inline_wait_ms` 的取值 | 先按 5 s，等第 2 片的 DOCX/PDF 实测后再调 | David |
