# ADR-DOC-02：DocumentService 操作契约

> **状态**：**Accepted**（2026-10-07）。生效条件都已满足：
> - PR-Daemon 已 APPROVE（#736；幂等统一修正见 #742）；
> - jason 已在 [#740](https://github.com/iDoris-ai/Agent24/pull/740#issuecomment-6040439976) 确认；
> - §9 的决策项由 David 拍板（Q1、Q2、Q4、Q5 已定）。
>
> **jason 确认时记录的已知事项**（不阻塞）：
> 1. DOC-1 所有文档工具按 `External` 处理，每次调用都要审批。改善它依赖 #735 的“启用时权限确认”，这一项已列为内核优先事项。
> 2. Agent run 取消后，模块侧的 job 会继续跑（Q7）。内核的取消信号、结构化工具错误码已在 #735 跟踪。
> 3. Q3（备份 / 迁出格式、静态加密、磁盘配额）是**发布前**必须决定的事项，由 jason 和 David 共同决定。
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
{ "document_id": "doc_…", "revision": 1, "content_sha256": "sha256:…", "media_type": "application/pdf",
  "text_layer_sha256": "sha256:…", "engine": { "id": "…", "version": "…" },
  "block_id": "p3/b12", "page": 3, "block_text_sha256": "sha256:…",
  "text_range": { "unit": "utf8", "start": 120, "end": 168 },
  "geometry": { "box": "CropBox", "unit": "pt", "origin": "top-left-rotated",
                "rects": [[72.0, 410.5, 300.2, 428.0], [72.0, 430.0, 180.4, 447.5]] },
  "quote": "原文片段" }
```

- **定位**：`block_id` 是结构路径，DOCX 等流式格式只用它定位，不给 `page`；PDF / 扫描件给出 1 起的物理页号。`media_type` 标明锚点所在 revision 的格式：分页格式（PDF、JPEG、PNG）必须同时给出 `page` 和 `geometry`，流式格式两者都不给（2026-10-09 增补，让锚点能自我说明是否分页，回应 OpenAPI B2a 的审查）。`geometry` 以 PDF **CropBox** 为参照框，单位为乘过 `UserUnit` 的点，原点在应用 `/Rotate` 后显示页面的左上角；`rects[]` 覆盖跨行、跨栏的片段。
- **偏移**：单位是 **UTF-8 字节**，相对块文本，半开区间 `[start, end)`，按**逻辑（存储）顺序**计，不按双向文本的视觉顺序；起止必须落在字符边界，否则 `invalid_request`。
  - 理由：OS 和引擎都是 Rust/UTF-8；块文本原样存储、不做规范化，偏移因此确定。渲染进程统一经 `api-client` 的换算函数转成 UTF-16。
  - 高亮时界面扩展到**字素簇边界**（UAX #29：泰文、组合符、ZWJ 序列），存储的偏移不变。
- **重新解析与 OCR**：锚点绑定 `text_layer_sha256`，不要求 OCR 可复现。即使用同一个钉住版本重跑，也不假定逐字节一致，解析始终用那份存储的文本层。`block_text_sha256` + `quote` + `rects` 用于校验，不一致时显示“定位失效”，**不猜位置**。
- **不跨 revision 迁移**：r1 的锚点永远指向 r1。要在 r2 上定位，只能重新抽取。
- **无来源**：没有可核验来源的值，`anchors` 为空并附 `unsourced_reason`。有来源的值可以带多个锚点（`anchors[]`），共同覆盖整句命题：值本身，以及决定含义的条件、对象、否定词、表格行列标题，可跨块、跨页（与 `samples/README.md` §4.2 的金标结构一致；2026-10-09 由单个 `anchor` 改为数组，回应 OpenAPI B2b 的审查）。

### 3.1 第 1 片文本层的落地（建议，待 David 确认，2026-10-10）

第 1 片读取引擎是 PDFKit + Vision（ADR-DOC-01 D6 修订）。以下是落地细节的建议，实现按它做；David 改了哪条，就按改后的版本调整实现。对应 §9 的 Q11–Q14。

- **辅助进程**：`agent24-documents-pdfkit`，单个 Swift 源文件，用 `swiftc` 构建，随 OS package 放在 `bin/agent24-documents` 旁边。
  - 每次解析启动一次：`parse <文件路径>`，结果以一份 JSON 写到 stdout。非零退出码表示整份失败（`parse_failed`）；部分页失败不算整份失败，见下面的「部分解析」。
  - Rust 侧限时（每份 120 s）、限输出（64 MiB），超时或超限就杀掉进程，记为 `parse_failed`。
  - 整个 OS 同时最多运行 2 个辅助进程，再来的排队等待（最多 16 个），队列满就回 503 `engine_unavailable`（`retryable: true`）。
  - 签名和公证跟随 OS package 的发布流程（jason）。开发期不签名。
  - Linux 上没有这个辅助进程：`engines[]` 报 `absent`，读取类操作报 `engine_unavailable`（D10）。
- **引擎标识**：`engine.id = apple-pdfkit`，`engine.version` 是 macOS 的版本号（如 `26.6`），因为 PDFKit 的行为随系统版本变化。辅助进程自身的协议版本、切分规则版本和下面的各项上限写进 config，参与 `config_sha256`（canonical JSON 的 sha256）。
- **何时解析**：
  - 导入成功后，导入 job 尽力而为地多做一步：用当前引擎解析出文本层并钉住。这一步失败不影响导入，导入不依赖引擎（Linux 上照常成功）。
  - `read_range` / `find` 找不到当前引擎和 config 的文本层时，就在后台开始解析，并最多等 8 s（内核代理要求 10 s 内返回响应头）。到时还没完成，回 503 `engine_unavailable`（`retryable: true`），解析继续进行，客户端稍后重试即可。
  - 同一组 `(content_sha256, engine, version, config_sha256)` 同时只解析一次，等待者共享结果。
- **块**：每页按 PDFKit 给出的行，在阅读顺序上把相邻行合并成段落；行距明显变大，或者左缘不对齐，就开始新段落。
  - 上限：块文本 ≤ 16 KiB（UTF-8），每块 ≤ 64 行。超出就在行边界拆成下一个块；单行本身超长，就在字符边界拆开。拆分在分配 `block_id`、计算哈希之前完成，所以任何一个块序列化后都远小于响应的 512 KiB 上限。
  - `block_id` 是 `p{页}/b{序号}`，序号在页内从 1 起按阅读顺序编号。图片（JPEG、PNG）只有第 1 页。
  - 块文本按引擎原样存储，不做规范化（§3）。行与行之间用 `\n` 连接，这个 `\n` 算作**前一行**的一部分，所以块文本里每个字节都属于某一行。
- **几何**：每个块记录它的每一行，包括这一行在块文本里的 UTF-8 范围（含行尾的 `\n`）和它的矩形。
  - 块的 `geometry.rects` 是这些行的矩形。
  - 命中的锚点取它覆盖到的那些**整行**的矩形，至少一个。第 1 片不做字符级的矩形，高亮可能比原文宽，但不会漏掉。
- **部分解析**：按页判断。
  - 页上的每个图像区域（扫描页、有页脚文字的扫描页、印章、截图）都用 Vision 做 OCR，与 PDFKit 读出的文字不重叠的行照常组成块；OCR 没读出文字的图像，按无文字处理。
  - OCR 失败，或者这页没法渲染：未能读出的区域记进 `unparsed_regions`（`page` + 区域的 `geometry` + `reason`），每页最多一项：多个区域合并成一个 `rects` 列表，最多 16 个矩形，超过就改用整页的一个矩形（宁宽勿漏）；`reason` 取自一个短的固定集合（如 `ocr_failed`、`render_failed`）。按 README §11 第 7 条明确标出，不被当作“没有内容”。
  - 既没有文字也没有图像的空白页：不算未解析，结果里就是没有块。
  - 图片（JPEG、PNG）直接用 Vision OCR，失败就是整份 `parse_failed`。
  - 文本层记录整份是否完整。`require_complete` 时，只要有未解析页就回 422 `partial_parse`，不返回内容。
- **分页**：`read_range` 和 `find` 都按页向前推进。
  - 一次响应最多覆盖 100 页，并按序列化字节数截断（同 `list`）；被截断的页，下次从这页中间的那个块（或那个命中）接着来。
  - 响应里的 `parse_status` 和 `unparsed_regions` 只描述**这次响应覆盖的页**：这些页里有未解析的区域就是 `partial`，并列出这些区域。整份是否完整，用 `require_complete` 判断。
  - 一次响应可以没有块或命中（比如覆盖的页全是未解析页，或者都没有命中），只要还没到最后一页，`next_cursor` 就不为空。所以全是扫描页的文档同样能翻完，`unparsed_regions` 也不会超过 100 项。
- **游标**：不透明。它绑定 `(document_id, revision, text_layer_sha256)` 和下一个位置：
  - `read_range` 的位置是页号加页内的块序号；
  - `find` 的位置是页号、页内的块序号加块内的命中序号，同时绑定查询的哈希。所以一个块里超过一页的命中，也能从块中间接着翻。
  - 换了查询，或者游标指向另一个文本层，回 400 `invalid_request`。
  - 翻页过程中总是读游标里那个已钉住的文本层，即使期间有了新引擎版本的文本层。
- **文本层 blob**：一份 JSON（`v: 1`），包含引擎、config、`parse_status`、`unparsed_regions`，以及块、行和矩形。存进 blob 区，`text_layer_sha256` 就是它的 blob 地址。`text_layers` 表登记 `(content_sha256, engine, version, config_sha256)`，同一组合只解析一次。没有游标的请求，用当前引擎和 config 的那一层。
- **find 的匹配**：只在单个块内匹配，不跨块。
  - 匹配的是存储的原文，只放宽两点，而且这两点都不改变偏移的对应关系：ASCII 字母不区分大小写；查询里的一段空白，可以匹配原文里任意一段空白（包括换行）。
  - 查询不能只由空白组成（400 `invalid_request`）。
  - 不做 Unicode 规范化，不做全角/半角折叠。中文原文不受这两点影响。
  - 结果按块的阅读顺序排列；一个块内的多处命中，按出现顺序各自成为一个锚点，互不重叠。

## 4. 操作清单（DOC-1）

- **调用路径**：路由前缀 `/api/v1/documents`，页面经 D5 的 preload 调用，agent 工具经内核 `_a24/tools/<op>` 调用。两条路径调用**同一个服务层**，操作日志记录来源（`page`，或 `run_id + tool_call_id`）。所有工具都声明 `output_privacy: local_only`（D8）。这只是**需求声明**，出站控制由内核的资料处理政策执行，见 ADR-DOC-01 D8 和 #735。
- **分页**：`list`、`find`、`read_range`、抽取结果统一用 `cursor` / `limit`（默认 50，最大 200），返回 `next_cursor`；单页响应 ≤ 512 KiB，给 1 MiB 上限留余量。`job.get` 返回单个 job，不分页；job 的结果如果是集合（例如抽取结果），由结果自己的资源分页（2026-10-09 澄清，回应 OpenAPI B1 的审查）。
- **风险映射**（用户确认后的内核 `RiskClass`，`types.rs:1036-1050`）：read → `Read`；create-record / mutate-draft / commit / create-artifact → `WriteLocal`；handoff → `External`。**DOC-1 没有第一方默认安装，所有工具实际按 `External` 处理、每次审批**，除非用户在启用时逐个确认（D4.2）。

| 片 | 操作 | REST | 工具 | 知识类 | 业务风险 |
|---|---|---|---|---|---|
| 1 | upload | `POST /uploads`（`Idempotency-Key`）；`POST /uploads/{id}/chunks`（≤768 KiB） | 不通告 | free | create-record |
| 1 | import | `POST /imports {upload_id}` → 202 job | **不通告**（Q4 已定：只从页面经原生文件对话框导入） | free | create-record |
| 1 | get / list | `GET /documents[/{id}]` | `documents.list`、`documents.get` | free | read |
| 1 | render | `GET /documents/{id}/revisions/{rev}/pages/{n}?scale=`（≤ 1 MiB；超出时自动降低 scale，降到 0.25 仍超出则 413，由客户端用 `region` 分块请求） | 不通告 | free | read |
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
     | `extract` | `(document_id, revision, schema_sha256, extractor_version, model_id)`，键用实际的模型 id，不用 profile；需要重新抽取时，页面可以显式带 `rerun: true`，同时换一个新键：新键由 `Idempotency-Key` 请求头给出，并入上面的业务键，所以它只在同一组业务坐标内有效，换了 schema 或实际模型就是另一个键（2026-10-09 澄清） |
     | `propose` | `(document_id, base_revision, ops_sha256)` |
     | `export` | `(document_id, revision, format, options_sha256)` |
     | `commit` | `commit_key` |

   - 同一个键对应的 `request_sha256` 不同 → 422 `idempotency_key_reused`。
   - **`request_sha256` 的规范化范围**：只覆盖**有效业务参数**，经 RFC 8785（JCS）规范化后计算 SHA-256。仅用于追踪的易变字段一律排除：`tool_call_id`、`run_id`、`x-a24-*` 请求 id、`Idempotency-Key` 头本身、时间戳、客户端版本。这样，同一个业务请求即使带着新的追踪 id，也不会被误报为 `idempotency_key_reused`。
   - **授权每次都重新检查**：键命中时，先做本次调用的授权与可用性检查，再返回已有结果。幂等命中不能跳过授权。
   - 占键在 `BEGIN IMMEDIATE` 内完成，由 SQLite 单写者串行化。若仍遇到 UNIQUE 冲突（其他连接），回滚后重读该行并按命中处理。
5. **agent 路径**：内核不重试，每次调用的 `tool_call_id` 都是新的。**两层分工**与 ADR-DOC-01 D4.3 一致：`tool_call_id` / `run_id` 只用于可信调用关联与审计（写进操作日志），**业务幂等只看上表的业务键**，DOC-1 没有传输层去重。页面遇到“结果未知”（`request_abandoned`、`upstream_timeout`）时用同一个键重发；代理不会重试 POST（`proxy.rs:1546-1556`）。
6. **上传**：状态在 `uploads/<upload_id>/` 和 `uploads` 表里，**不在 `tmp/`**，启动清理和孤儿 GC 都不碰它；最后一个块到达 24 h 后过期。每块带 `Upload-Offset` 和 `Chunk-Sha256`：
   - `offset` 等于已接收长度 → 追加并 fsync；这一段已接收且哈希相同 → 200 重放；其他情况 → 409 `upload_offset_mismatch`（`details.received_offset`）。
   - 上传仍在接收、且 `offset` 等于已接收长度的新块：`Chunk-Sha256` 与块内容不符，或会超出 `total_size` → 400 `invalid_request`。已接收的那一段只按存下的哈希判断（见上一条）；已收齐或已导入的上传不再接受新块，先返回 409（同上）。
   - import 时校验整个文件的 sha256，不符 → 422 `upload_checksum_mismatch`。

7. **幂等验收**（#740 要求，记录到 #708）：
   - **不同 `tool_call_id`、相同有效业务参数** → 复用同一个 job 或业务结果，不重复产生副作用。
   - **相同业务键、不同有效业务载荷** → 拒绝，返回 422 `idempotency_key_reused`。
   - **已取消 job 的普通重放** → 不会复活，返回原 job 及其 `cancelled` 状态。只有显式调用 `POST /jobs/{id}/retry` 才会重启（§7）。

## 6. 类型化错误

`documents` 命名空间的错误码是**闭集**，新增需修改本 ADR。唯一的例外是下文的“OS 的意外故障”。

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
| `storage_unavailable` | 503 | `Failed` | `data_dir` 或 DB 不可用；与知识关闭是不同状态。`details.cause` 说明原因；`retryable` **按原因定**（见表下说明） |

**`storage_unavailable` 的 `retryable`**（David，2026-10-08，回应 #803 评审）：不像 `engine_unavailable` / `knowledge_unavailable` 那样固定为 `true`，而是由 OS 按原因决定。这是有意的不对称，不要统一改成 `true`。

| `details.cause` | `retryable` | 含义 |
|---|---|---|
| `locked` / `busy` | `true` | DB 被锁或暂时打不开，稍后重试即可 |
| `corrupt` | `false` | DB 或 blob 区校验失败，需要用户处理 |
| `not_writable` | `false` | `data_dir` 不可写（权限、只读卷） |
| `disk_full` | `false` | 磁盘已满，释放空间后由用户重试 |

界面对 `retryable: false` 的情况给出处理提示，不自动重试。

**OS 的意外故障：共用内核码的唯一例外**：失败既不在上表、也不是存储不可用（例如数据库约束被触发，说明 OS 自身有 bug）时，OS 自己返回 500，用内核通用信封的 `internal` 码，`details.retryable: false`。这是唯一一个由 OS 产生、却不在本命名空间闭集里的码；OpenAPI 中 documents 路由的 500 响应和 `ModuleProxyError` 的说明里都注明了这一点。
- 工具路径映射成 `ToolError::Failed`，`internal` 作为消息前缀；
- 不自动重试；结果未知，用户重试时按下面的“默认规则”用同一个键重发。

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
- **取消**：`POST /jobs/{id}/cancel` 幂等。`running` 置 `cancelling`，工作线程在阶段边界检查，终止引擎子进程，丢弃没有提交的输出；没有工作线程在跑的 `queued` / `failed` / `interrupted` 直接置 `cancelled`，带 `cancelled` 标记（`failed` 原有的错误码被它替换），之后键命中也不会复活；其余状态不变。结果事务已提交的 job 保持 `succeeded`；commit job 一旦进入 `BEGIN IMMEDIATE` 就不可取消。
- **崩溃恢复**：OS 启动时 `running` → `interrupted`，`queued` 也 → `interrupted`（进程重启后已没有处理它的工作线程；否则按键命中规则它会一直停在 `queued`），`cancelling` → `cancelled`（结果与状态同事务，此时必然没有已提交的输出）；清理 `tmp/` 与 `blobs/tmp/`。**不自动续跑**：下一次带同一个键的调用重新启用 job，确定性 job 从最后完成的阶段续做。
- **输入**：job 的 `input`（重新入队或重试时据此再次运行）只能写入一次，写入后不可再改；迁移前的旧 job 为 NULL，之后仍可补写一次。
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
| Q11 | 第 1 片 PDFKit 辅助进程的形态：每次解析启动一次、stdout 一份 JSON、每份限时 120 s、同时最多 2 个；用 `swiftc` 构建并随 OS package 分发，签名和公证跟随发布流程（§3.1） | 建议如左 | David；签名和打包：jason |
| Q12 | 块的切分与 `block_id`（`p{页}/b{序号}`，按行距和左缘把相邻行合并成段落，每块 ≤ 16 KiB、≤ 64 行），几何只做到整行；部分解析按页判断，每个图像区域都做 OCR，失败的区域标为未解析；分页按页推进，一次最多 100 页，`parse_status` 描述本次覆盖的页（§3.1） | 建议如左 | David |
| Q13 | 文本层在导入后尽力预解析；读取或查找时缺了就后台解析，最多等 8 s，否则回 503 `engine_unavailable`（可重试）（§3.1） | 建议如左 | David |
| Q14 | find 的匹配规则：块内匹配，只放宽 ASCII 大小写和空白，不做 Unicode 规范化（§3.1） | 建议如左 | David |
