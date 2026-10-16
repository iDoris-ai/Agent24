# Knowledge 只读接入契约 v1（CONTRACT-READONLY-v1）

> **状态**：**草案（Draft）**，待 jason 或 fork 侧维护者评审（文档 PR，base `ab/weknora`）。评审通过前不授权任何实现。
> **版本**：契约 v1（报文字段 `protocol_version: 1`），文档修订 r1 · **日期**：2026-10-16 · **作者**：David Xu
> **Issue**：#1011（KB-CT-02a），阶段 #1000；探测依据 iDoris-ai/WeKnora#5。
> **依据**：任务方案 v2.1（方案全文见 #986 第一条评论）。每条规则后方括号里是来源：决策 `[D1]`–`[D8]`（方案 §1），或任务条目与方案章节（如 `[KB-C1-04]`、`[KB-C1-01b-b]`、`[方案 §4]`）。
> **探测证据**：WeKnora 仓库 `docs/agent24/CAPABILITY-MATRIX.md`（KB-CT-01 PR①，iDoris-ai/WeKnora#7，以合入版为准；第 5 轮评审的遗留项见 iDoris-ai/WeKnora#8），下文"矩阵 #n"指其第 n 项。
> **受测 SHA**：WeKnora `4b363d153122797c857871ce7e4bb63626efc637`（`a24/develop` = 上游 v0.8.2 + 2 个 Agent24 提交，Go 源码与 v0.8.2 相同）。
> 文中 WeKnora 的 `file:line` 都按此 SHA，路径相对 WeKnora 仓库根目录。矩阵是静态源码阅读，运行证据待 KB-CT-01 PR② 产出；两者冲突时先回写矩阵，再按 §0.3 修订本文。

## 0. 范围、边界与变更规则

### 0.1 本文冻结什么
Agent24 的 KnowledgeProvider（下称"客户端"）与 WeKnora fork（下称"服务端"）之间，**首版只读接入**的语义：信任边界、范围、search、read、索引状态与 stale、错误分类、取消、就绪、能力发现、审计最小字段。
本文只定语义，不定实现：不规定 HTTP 路径、Go/Rust 类型、服务端在单个 `score_space` 内的融合算法、缓存结构、具体上限数值。§11 示例里的字段名是给 #1012 的 schema 直接使用的建议命名，包裹层（`request` / `response` / `status`）只用于展示。
§3 的线上响应由服务端产出。有效范围跨多个 `score_space` 时，由 Provider 层（Agent24 adapter）拆分请求并合并结果；这条合并规则（R3.7）属于 Provider 层，同样在本文冻结。

### 0.2 与相邻任务的边界
| 留给 | 内容 | 本文怎么引用 |
|---|---|---|
| #1012（KB-CT-02b） | `protocol/knowledge/v1/*.schema.json`、`protocol/knowledge/fixtures/v1/` 夹具与摘要清单、Rust/Ajv/Go 测试接线、Agent24 对外 REST 进 `protocol/openapi.yaml` | 本文给语义与字段名；schema 的类型、必填、长度上限、`format` 由 #1012 定 |
| #1013 ①（KB-CT-03） | profile / Off / 撤权状态机，generation 何时递增 | 本文只冻结"请求带 generation、迟到响应丢弃"（§7） |
| #1013 ② | 一次性交换票据、UI session JWT、消息桥 | 本文只用可重复使用的短期访问凭据（§1） |
| #1013 ③ | 初始化、ingest、job、ContextPack、SOP | v1 不含任何写操作；ingest 只出现在就绪表达里（§8） |
| KB-C1-05 | Tool 层证据包裹（"数据不是指令"）、引用 UI | 本文只规定 hit/read 携带哪些修订与定位信息 |

### 0.3 变更规则
- **语义变更升主版本**：改字段含义、改范围或 stale 判定、改某个错误码的 `retryable`、删字段、新增或删除错误码（错误码是闭集，客户端按 `code` 分支）→ 新文件 `CONTRACT-READONLY-v2.md` 与 `protocol_version: 2`，v1 保留到双方迁移完成。
- **兼容新增记为 v1 修订**（r1、r2…）：新增可选字段、新的 `details.reason` 取值、新的能力声明键。客户端必须忽略未知字段与未知 `reason`（与 `protocol/openapi.yaml` 的约定一致；未知 `code` 的处理见 R6.2）。
- **草案期**：v1 在评审通过、并且被 #1012 夹具引用之前处于草案期；这期间的语义修正记为 v1 修订（r1、r2…），不升主版本。KB-CT-01 PR② 的运行证据推翻某条语义时，也按这条处理。
- 每次修订在文末变更记录写明日期、修订号、是否影响夹具；影响夹具的修订要同一 PR 更新 #1012 的清单。
- schema 与本文冲突时，谁都不默认胜出：在同一 PR 里改正错误的一方。
- 文中标"待决"的条目链接到决策 issue；决定之前，实现按本文写的保守默认执行，不得自行放宽。

## 术语
| 术语 | 定义 |
|---|---|
| principal（主体） | 一次请求最终代表的人。由**签发权威**认定并绑定进凭据；服务端只认凭据里的 principal，不认请求自报。 |
| 签发权威（issuing authority） | 验证登录、成员资格与授权后签发凭据的可信服务端组件。remote 下是独立的可信签发服务，local-managed 下是本实例。 |
| 凭据（credential） | 本文专指可重复使用的**短期访问凭据**：绑定 principal、租户、范围、受众、到期时间，带唯一标识（`credential_id`，即 jti 或 key id）并可撤销。 |
| profile | Agent24 连接一个服务端的配置档：`local-managed`（宿主托管的本机实例）或 `remote`（远端服务）。 |
| 资源（resource） | 知识库（KB，`knowledge_base_id`）、文档（`knowledge_id`）、分块（`chunk_id`）。 |
| scope（范围） | 一组 KB ID，表示可触及的资源边界。分权威授权 A、凭据范围 C、请求范围 Q 三种，交集为有效范围 E（§2）。 |
| revision（修订） | 三个层次：**源修订** `source_revision`（原件某个不可变版本）、**索引构建** `index_build_id`（某个源修订按某套解析/切分/embedding 配置的一次建索引）、**分块修订** `chunk_revision`（分块正文的编辑计数）。前两项是文档级修订，三项合称修订三元组（§5）。 |
| 活动构建（active build） | 某个源修订当前对检索生效的那次索引构建；每个源修订至多一个（R5.1）。 |
| score_space | 同一 embedding 模型与检索配置的分组标识；只有同一 score_space 内的原始分数可比较（R3.6）。 |
| generation（代） | 客户端为每个"profile × principal"绑定维护的单调递增整数。任何使在途工作作废的事件都让它加一（规则见 #1013①）。 |
| deadline / 预算 | 调用方给出的总时限与资源上限（返回量、并发、字节）。 |
| partial | 结果可用但不完整：部分分支失败或超时。是一种成功状态，不是错误。 |

## 1. 信任边界

- **R1.1** [D2] 签发权威只在可信服务端一侧，凭据的签发、收窄、撤销都由它完成。**remote 下 Desktop 不持有任何签名密钥**，只持有并出示凭据；local-managed 的例外见 R1.4。
- **R1.2** [D2] **租户 HMAC 绝不下发 Desktop**：持有它就能为任意 `sub` 签名。平台 API key 同样绝不进 Desktop，也不用于知识读取。
  证据：signed_token 只校验 HS256/aud/exp 后取出 `sub`（矩阵 #2，`internal/middleware/auth.go:613-659`）；租户 Owner 可经 `POST /tenants/:id/api-principal-test-token` 为任意外部主体签发（`internal/handler/tenant.go:1021-1095`）；平台 key 经 X-Tenant-ID 进入任意租户且白名单为空（矩阵 #10，`auth.go:388-408`）。
- **R1.3** [D2] remote：凭据由可信签发服务在验证登录、成员资格、授权之后签发，绑定用户、可撤销、范围受限。签发服务部署在哪、由谁交付与运营**待决**（#992）；决定之前只允许单用户样本与 Mock。
- **R1.4** [D2] local-managed：签发由本实例完成；宿主持有**本实例专属**的签名密钥与秘密（实例凭据、握手令牌、持久化的 JWT_SECRET 等），只用于本实例，不跨实例、不跨 profile 复用，也不与 remote 共用。实例托管细节见 KB-LITE-01a、KB-HOST-01。
- **R1.5** [D2] 服务端只信凭据里绑定的 principal。`direct` 模式（直接读 `X-External-User-ID`，`auth.go:580-594`）在集成模式下必须拒绝；改 principal Mode 不算撤权手段（矩阵 #11）。
- **R1.6** [D2] 防重放要求凭据标识配有撤销或消费机制；只有 jti 字段不算防重放。上游 signed_token 无 jti、无单主体撤销（矩阵 #2、#11）。
- **R1.7** [D2] 凭据不进 renderer、URL、普通日志，不跨 origin 发送（KB-HOST-01、KB-C1-02b）。
- **差距**：上游 principal 不参与 KB ACL，不进审计，且决定会话、记忆、MCP OAuth、沙箱凭据归属（矩阵 #2）→ KB-C1-03；集成模式下关闭 direct、限制服务端签发入口 → KB-C1-03、KB-C1-01。

## 2. 范围语义

- **R2.1** [D2] **有效范围 E = 权威授权 A ∩ 凭据范围 C ∩ 请求范围 Q**，由服务端在每个请求上计算；客户端提供的 Q 只能收窄，不能扩大。
- **R2.2** [D2] **服务端内部 deny-all 默认**：A 或 C 缺失或为空，都按空集处理，绝不解释为"全部"。凭据签发时范围为空就拒绝签发。客户端输入的处理见 R2.5。
- **R2.3** [D2] **空交集拒绝**：Q 形状合法但 E 为空（包括 A 为空）时，整个请求返回 `forbidden`，`details.reason: "empty_scope"`，不执行检索、不计算 query embedding。示例见 §11.1。
- **R2.4** [D2] **永不回写空数组**：任何一方在收窄后得到空集时，必须拒绝，不得把 `[]` 写进下游请求、凭据或配置。上游在多处把空列表当"全部"或"回退到默认"。
- **R2.5** [D2] **search 与 read 都必须带 `scope.knowledge_base_ids`**。Q 缺失、为 `[]` 或无法解析，一律返回 `invalid_request`；C 无法解析返回 `unauthorized`，`reason: "credential_invalid"`。Q 与 A ∩ C 部分重叠时按交集执行，响应用 `effective_scope` 回报实际范围，不说明其余 ID 是不存在还是无权。
- **R2.6** [D2] **越界资源与不存在不可区分**：read 的目标文档或分块必须落在 E 内；落在 E 之外时，返回与"不存在"完全相同的 `not_found`（同码、同 message、无 reason）。search 结果永远不含 E 之外的资源。示例见 §11.2。
- **R2.7** [D3/D4] v1 凭据的操作范围只含 search 与 read；下载原件、preview、`/files` 不在 v1 内（矩阵 #5、#6；谁可下载原件待决 #996）。集成模式不消费既存的组织共享与共享 Agent 授权（`internal/application/access/knowledgebase.go:117-154`）。
- **证据与差距**：
  - 上游空白名单 = 全租户：`AllowsKnowledgeBase` / `AllowsKnowledgeBases` 遇空列表返回 true（矩阵 #1，`internal/types/tenant_api_key.go:328-330`、`:345-347`）；`apiKeyAny()`（定义在 `internal/router/rbac.go:226-228`）对任何有效 key 放行（`internal/middleware/api_key_gate.go:149-151`）。
  - hybrid-search 的 `knowledge_base_ids` 为空时回退到路径里的 KB（`internal/types/search.go:244-247`），是 R2.4 的直接风险点。
  - 上游受白名单限制的 key 越界时"任一越界即整体 403"（`tenant_api_key.go:378-386`），且校验在查库之前（`internal/middleware/kb_access.go:202`）；白名单放行之后，KB 不存在返回 404、他租户无共享返回 403（`kb_access.go:214-220`；`access/knowledgebase.go:102`、`:155`），后者可用来探测存在性，与 R2.6 不符。
  - 上游没有用户级授权 A，同租户内无 per-user ACL（矩阵 #10）。A 在 v1 单用户样本里等于签发时的授权；如何表达 A（凭据声明还是服务端查询）由 KB-C1-03 定，差异权限是否首版支持待决（#993）。
  - → KB-C1-03（∩ 与空拒绝）、KB-C1-01（允许列表、禁消费既存共享）。

## 3. search

- **R3.1** [D1] 检索走 hybrid-search 一路；`/knowledge-search` 不用于集成：它用 `logger.CloneContext` 脱开请求 ctx（矩阵 #3，`internal/handler/session/qa.go:829`），参数错误返回 400（`qa.go:834-844`、`:874-884`），白名单拒绝返回 Forbidden（`:886-889`），服务层错误一律 500 并带回内部错误原文（`:902-905`）。
- **R3.2** [D1] 请求包含：`query`（文本）、`scope`（§2）、`budget`，以及通用字段 `protocol_version`、`request_id`、`generation`（§7）。
- **R3.3** [D1] `budget`：`deadline_ms`（相对时限，避免时钟偏差）、`max_results`、`max_concurrency`（**单次请求内**服务端对各分支的并发上限）。Provider 层对多次请求的总预算另行控制（KB-C1-04）。超过服务端上限时**截到上限**，不报错，响应用 `applied_budget` 回报实际值；上限在能力声明里公开（§9）。
- **R3.4** [D1] **完整性**：服务端按 E 内的 KB 分支执行。全部分支成功 → `completeness: "complete"`；至少一个分支成功完成（哪怕 0 命中）而其余失败或超时 → `"partial"`；一个都没成功 → 全部因 deadline 失败时返回 `timeout`，否则返回 `unavailable`。
- **R3.5** [D1] **partial**：`partial_failures[]` 每项为 `{ "knowledge_base_id", "code": "timeout" | "unavailable", "retryable" }`。客户端可以只针对失败的 KB 发一个新请求重试（新 `request_id`、当前 generation），计入剩余总预算；结果按 R3.7 合并、去重、重新编号。partial 结果可以使用，但 Tool 层必须标出不完整（KB-C1-05）。示例见 §11.6。
- **R3.6** [D1] **排序与 score_space**：响应里 `hits` 的顺序就是权威顺序，`rank` 从 1 连续编号。`score` 只是参考，并带 `score_space`；**不同 score_space 的原始分数不可比较**。v1 服务端端点只接受 E 落在同一个 score_space 的请求；E 跨多个 score_space 时返回 `invalid_request`，`reason: "mixed_score_space"`，`details.groups` 为 `[{ "score_space", "knowledge_base_ids" }]`，只列 E 内的 KB，不泄露范围外的。
- **R3.7** [D1] **Provider 层合并**：跨 score_space 时，Agent24 adapter 按 `details.groups`（或已知分组）拆成每组一个请求，再合并：组内保持服务端顺序；组间按秩轮转交错（round-robin，各组第 1 名、各组第 2 名……，组的先后按请求里 KB 的首次出现顺序）；按 `chunk_id` 去重（保留先出现者）；重新编号 `rank`；合并后的 `completeness` 取各组中最差的那个（`complete` 优于 `partial`），`partial_failures` 取并集。
- **R3.8** [D1] 每个 hit 带：`knowledge_base_id`、`knowledge_id`、`chunk_id`、修订三元组 `revision`、`locator`、`index_state`（§5）、`score`、`score_space`、受字节上限约束的 `snippet`。snippet 是检索证据，不是正文；引用正文用 read。
- **R3.9** [D1] `locator.start` / `locator.end` 只在分块正文未被改写时有效。服务端对父块或邻块扩展后的内容不得返回偏移（`locator.valid: false`），客户端据此不生成精确引用。
- **R3.10** [KB-SVC-POLICY-01/#994] 只读检索也可能产生处理出口：开向量检索且调用方没给 query_embedding 时，服务端会算一次 query embedding（矩阵 #3，`internal/application/service/knowledgebase_search.go:206-213`）。允许的处理目的地待决（#994），由 KB-SVC-POLICY-01 执行；客户端不得假定 search 只在本地发生。
- **差距**（矩阵 #3）：
  - 无调用方 deadline，内部只有 12s 解析上限与多组 30s 超时（`knowledgebase_search_storegroup.go:21`、`knowledgebase_search_fanout.go:23`）。
  - fan-out 是全有或全无：任一组失败都收成 VectorStoreUnavailable（`knowledgebase_search_fanout.go:89-104`），**没有 partial**。
  - hit 无修订，`ContentRevision` 与 `ContentRewritten` 都不序列化（`internal/types/search.go:220`、`:226`）；多库检索要求 embedding 一致（`knowledgebase_search.go:178`）。
  - → KB-C1-01c ①③。

## 4. read

- **R4.1** [D1] 请求包含：`scope`（§2，规则与 search 相同）、`knowledge_id`；可选 `chunk_id`；`expected_revision`；`page`（`cursor`、`limit`）；`max_bytes`；以及通用字段。从引用发起的 read **必须**带 `expected_revision`（取自 search hit）。
- **R4.2** [D1] **只返回分块正文**：响应 `segments[]` 每项含 `chunk_id`、`chunk_revision`、`text`。元数据、标题、描述、spans、stages 都不是正文，不得当作引文出示。上游 `/knowledge/:id/spans`、`/stages` 是处理阶段时间线（`internal/handler/knowledge.go:649` 起），但会序列化 `input`、`output`、`metadata`（`internal/types/knowledge_span.go:92-96`），是否带出解析正文待探测（iDoris-ai/WeKnora#8）；无论是否带出，都不得作为正文或引文来源。
- **R4.3** [D1] **粒度**：
  - 带 `chunk_id`：只返回这一个分块（单个 segment），`expected_revision` 比较全部三项；
  - 不带 `chunk_id`：按文档顺序分页，`expected_revision` 只能含、也只比较 `source_revision` 与 `index_build_id`（多带 `chunk_revision` 为 `invalid_request`）。
  - 响应顶层 `revision` 只放文档级两项；`chunk_revision` 放在各 segment 里。
- **R4.4** [D1] **校验检索时修订**：当前修订 = 当前源修订 + 其活动构建（R5.3）。任一比较项不同即为不匹配；文档在 E 内、但锚点分块不在当前活动构建里，也是不匹配。**v1 不提供旧版本**：不匹配时一律返回 `stale`，`details.current_revision` 给出当前文档级两项（活动构建未就绪时 `index_build_id: null`）；锚点分块仍在活动构建中时另附 `details.current_chunk_revision`。不返回旧正文。上游 `/revisions` 能取历史正文（`internal/handler/chunk.go:257-268`），但编辑可能是删改敏感内容，是否允许取旧版本与数据保留要求一起**待决**（#997）。示例见 §11.5。
- **R4.5** [D1] **字节与分页上限**：`max_bytes` 与 `page.limit` 超过服务端上限时截到上限；正文在字符边界截断，`truncated: true` 并给 `next_cursor`。游标绑定首页的文档级两项；这两项变了，后续页返回 `stale`。翻页期间单个分块被编辑不算 stale，segment 照常返回读取时的 `chunk_revision`。
- **R4.6** [D2] read 每次都按 §2 重新计算 E，目标资源必须在 E 内；"之前 search 返回过"不构成授权。
- **差距**（矩阵 #4）：能读分块（分页默认 10、最大 100，`internal/handler/chunk.go:117-125`），但不能按指定修订读并校验，没有 stale；服务层错误一律 500 并带回内部错误原文（`chunk.go:74-75`）→ KB-C1-01c ②。

## 5. 索引状态与 stale

- **R5.1** [D1] **三者关系**：一个 `knowledge_id` 有一串源修订；每个源修订可以有多次索引构建，但**至多一个活动构建**；每个分块属于一次索引构建，`chunk_revision` 在该构建内随人工编辑或回滚递增。
- **R5.2** [D1] 三者都是服务端分配的**不透明标识**（`chunk_revision` 为整数）。`updated_at`、`file_hash` 不充当索引修订：相同文件在不同解析或 embedding 配置下会产生不同索引。
- **R5.3** [D1] **当前修订**（`current_revision`）= 当前源修订 + 它的活动构建；活动构建还没建好时 `index_build_id: null`，此时该文档的 hit 与 read 都按 stale 处理。`index_state` 取值：
  - `fresh`：hit 所在的构建就是当前源修订的活动构建，并且分块当前修订已经进入检索存储；
  - `stale`：不满足 `fresh`，例如源修订已更新、构建不再活动，或分块已编辑而新修订尚未进入检索存储；
  - `failed`：该修订的索引失败；
  - `unknown`：服务端无法判定。**`unknown` 按 `stale` 对待**，不得当作 `fresh` 展示。
- **R5.4** [D1] **展示**：`stale` / `failed` / `unknown` 的 hit 可以作为线索出示，但必须标"可能已过期"，不生成确定引用；确定引用只来自 `fresh` 的 hit 或修订校验通过的 read。
- **证据与差距**：Chunk 有 `content_revision` 与 `index_status`（ready/processing/failed，`internal/types/chunk.go:132-135`），可映射为分块修订与 R5.3 的一部分；Knowledge 有 `parse_status` 与 `file_hash`（`internal/types/knowledge.go:189`、`:218`）；**没有源修订、索引构建与活动构建标识，也没有三者映射**（矩阵 #4）→ KB-C1-01c ③。

## 6. 错误分类

- **R6.1** [D8] **信封**：集成端点输出独立的字符串码信封 `{"error": {"code", "message", "hint"?, "details": {"retryable", "request_id", "generation", "reason"?, "current_generation"?}}}`，与 Agent24 现有信封同形；`current_generation` 只出现在客户端本地的 `cancelled` 里。上游信封不同：`code` 是整数（`internal/errors/errors.go:9` 的 `ErrorCode int`，AppError 见 `:56-61`），没有 `hint`，`details` 里也没有 `retryable`、`request_id`、`generation`，外层还有 `success: false`（`internal/middleware/error_handler.go:23-43`）。上游前端依赖现有信封，所以不改它，只在集成端点另出（KB-C1-01c ①）。
- **R6.2** [D8] `code` 是闭集，**客户端按 `code` 分支，不按 HTTP 状态分支**，HTTP 状态是建议映射。v1 客户端遇到未知 `code` 一律按 `internal` 处理、不重试。这与 `protocol/openapi.yaml` 的开放枚举约定不同，是有意为之：未知错误码可能出自更严格的拒绝，按可重试处理会绕过它。
- **R6.3** [D2] `message` 是固定的通用文本，不得含内部错误原文、query、正文、凭据或资源是否存在的提示。上游把 `err.Error()` 原样返回（矩阵 #3，`internal/handler/knowledgebase.go:366-372`）→ KB-C1-01c ①。
- **R6.4** [D2] **判定顺序**：`invalid_request`（请求形状）→ `unauthorized`（凭据）→ 计算 E（`forbidden`；search 的 `mixed_score_space` 在此之后判定）→ 资源存在且在 E 内（否则 `not_found`）→ 修订比较（`stale`）→ 执行（`timeout` / `unavailable` / `cancelled` / partial）。由此：
  - 越界资源即使带 `expected_revision`，也返回 `not_found`；
  - search 之后文档被删，read 返回 `not_found`；
  - 文档还在、但锚点分块不在当前活动构建里，返回 `stale`。

| code | 建议 HTTP | retryable | 含义与 `details` |
|---|---|---|---|
| `invalid_request` | 400 | false | 请求形状不合法：Q 缺失、为 `[]` 或无法解析，字段越界；`reason: "mixed_score_space"` 时带 `groups`（R3.6）。**补充项**，见 §12 |
| `unauthorized` | 401 | false | 凭据缺失、无法解析、无效、过期或已撤销；`reason`：`credential_missing` / `credential_invalid` / `credential_expired` / `credential_revoked`。换新凭据后以新 `request_id` 重发 |
| `forbidden` | 403 | false | 凭据有效但不允许：`reason`：`empty_scope`（E 为空，含 A 为空）/ `operation_not_allowed` / `policy_denied` |
| `not_found` | 404 | false | 资源不存在、已删除，**或在 E 之外**（R2.6），无 reason |
| `stale` | 409 | false | 请求的修订不是当前修订且不可取，或锚点分块不在活动构建里；`current_revision`，可选 `current_chunk_revision`。重新 search |
| `timeout` | 504 | true | 服务端 deadline 内没有任何分支成功；在剩余总预算内可重试 |
| `cancelled` | —（多半无响应） | false | 请求被取消（§7）。客户端本地的 `cancelled` 写进 Provider 本地结果与宿主侧日志；服务端审计只在服务端确实观察到取消时记录 |
| `unavailable` | 503 | true | 依赖不可用或未就绪；`reason`：`not_ready` / `dependency` / `overloaded`；可带 `retry_after_ms` |
| `internal` | 500 | false | 服务端意外故障；客户端遇到未知 `code` 也按此处理。**补充项**，见 §12 |

- **R6.5** [D1] `partial` 不是错误码：它是 search 成功响应的 `completeness`（R3.4）。
- **R6.6** [D1] 重试一律是新请求（新 `request_id`、当前 `generation`），计入同一总预算；`retryable: false` 的错误不自动重试。

## 7. 取消与 generation

- **R7.1** [KB-C1-04] 客户端取消在途请求的方式是中止该 HTTP 请求。服务端必须让取消沿请求上下文传导，停止扇出与后续计算；服务端观察到取消时，审计结果记 `cancelled`。
- **R7.2** [KB-C1-04] 每个请求带 `request_id`（客户端生成，唯一）与 `generation`；服务端在响应和错误信封里原样回显两者。
- **R7.3** [KB-C1-04] 客户端只接受 `request_id` 匹配在途请求、且 `generation` 等于当前值的响应；否则**丢弃**：不注入上下文、不进缓存、不作为证据，Provider 返回本地 `cancelled` 并写宿主侧日志。示例见 §11.4。
- **R7.4** [KB-C1-04] 作废顺序：先阻止新调用，再取消在途请求，最后丢弃迟到响应。generation 何时递增、Off 是否持久等状态规则由 #1013① 定。
- **R7.5** [方案 §4] 方案 §4 风险表写的是"两端校验 generation"。v1 服务端**只回显、不校验**，理由：服务端校验需要把 generation 绑定进凭据，这依赖尚未冻结的身份交换（#1013②）与 KB-C1-03。在此之前由客户端单端丢弃保证"迟到响应不被消费"；与方案 §4 的差异交评审裁定（§12 #6）。
- **R7.6** [D2] 撤权在下一个请求生效；对已在途的请求，服务端应在组装响应前复验凭据与有效范围，复验失败按 `unauthorized` 或 `forbidden` 返回。上游 key、JWT、成员关系逐请求查库，但在途的流式或长请求不因撤权中止（矩阵 #11，`internal/application/service/tenant_api_key.go:91-100`）→ KB-C1-03。
- **R7.7** [D2] 取消和撤权都不能追回已经交付给模型或用户的内容，界面与审计不得声称可以做到（保留与删除要求待决 #997）。
- **差距**：上游 hybrid-search 用请求 ctx，断开会传导（矩阵 #3，`internal/handler/knowledgebase.go:359`）；但父 ctx 被取消时 fan-out 返回 `ctx.Err()`（`knowledgebase_search_fanout.go:95-97`），handler 把它包成 500（`knowledgebase.go:366-372`）。`/knowledge-search` 不传导（R3.1）。所以 `cancelled` 与 partial（R3.4）对上游都是净新增 → KB-C1-01c ①。

## 8. 就绪

- **R8.1** [KB-C1-01b-b/D8] **health ≠ ready**：health 只表示进程存活，不带业务含义。上游 `/health` 写死返回 ok（矩阵 #9，`internal/router/router.go:145-147`）。
- **R8.2** [KB-C1-01b-b/D8] 就绪**按操作**表达：`operations.search`、`operations.read`、`operations.ingest` 各有 `state`（`ready` / `not_ready` / `disabled`）与可选 `reason`。只读 v1 下 `ingest` 为 `disabled`。
- **R8.3** [KB-C1-01b-b] search 就绪只取决于检索所需的存储、向量库与（需要时）query embedding 模型，不依赖 chat、docreader 或默认 KB，避免与初始化互相等待。
- **R8.4** [D2] **按 KB 就绪**：请求可带 `scope.knowledge_base_ids`，按 §2 计算 E（规则相同）；响应 `knowledge_bases[]` 每项为 `{ "knowledge_base_id", "search": { "state", "reason"? } }`，只列 E 内的 KB，范围外的 ID 直接省略，不区分不存在与无权。示例见 §11.7。
- **R8.5** [KB-C1-01b-b] 服务端就绪与 Agent24 宿主的 profile 状态（registered / loading / ready / degraded / unavailable / disabled）是两回事；后者由前者映射而来，映射规则见 #1013①。
- **差距**：没有 /ready，也没有按操作的探针（矩阵 #9）→ KB-C1-01b-b。

## 9. 能力发现

- **R9.1** [D8] 能力发现扩展 `/system/capabilities`：在现有 `edition`、`capabilities` 之外新增 `integration` 对象（上游响应结构见 `internal/handler/deployment_capabilities.go:29-39`）。
- **R9.2** [D8] `integration.agent24_knowledge` 至少包含：`protocol_versions`（支持的契约主版本列表）、`mode`（`integration` / `standalone`）、`operations`（声明支持的操作）、`limits`（R3.3、R4.5 的上限）。
- **R9.3** [D8] 能力声明只说明"支持什么"，**不说明依赖是否健康或业务是否就绪**；就绪只看 §8。集成模式下声明必须如实反映已禁用的路由（KB-C1-01）。
- **R9.4** [D8] 版本协商：客户端取双方都支持的最高主版本；没有交集时把该 profile 视为不兼容，不回退到未版本化的上游接口。
- **R9.5** [D8] retrieve 级凭据必须能读到能力声明和服务端版本。上游 `/system/capabilities` 任意 key 可读，但 `/system/info` 要 manage_vector_stores 或 full access（矩阵 #7、#8，`internal/router/routes_auth_tenant.go:252-255`）→ KB-C1-01b-a。

```json
{ "edition": "lite", "capabilities": { "integrations.api": { "supported": true } },
  "integration": { "agent24_knowledge": {
    "protocol_versions": [1], "mode": "integration", "operations": ["search", "read"],
    "limits": { "max_results": 50, "max_concurrency": 4, "max_deadline_ms": 30000,
                "max_read_bytes": 262144, "max_page_limit": 100, "max_snippet_bytes": 2048 } } } }
```
上面的数值只是示意，实际上限由服务端配置与 #1012 定。

## 10. 审计最小字段

- **R10.1** [D2] 每次 search 与 read 都产生一条审计事件（`knowledge.search` / `knowledge.read`），至少包含：
  `occurred_at`、`request_id`、`generation`、`principal`（凭据绑定的主体）、`credential_id`、`tenant_id`、`effective_scope`（KB ID 列表）、
  `resource`（read：`knowledge_id` 与 `chunk_id` 列表；search：命中的 `chunk_id` 列表与数量）、`revision`（read 时的文档级两项与各 segment 的分块修订；search 时各 hit 的三元组）、
  `outcome`（`ok` / `partial` / 错误码）、`metering`（`latency_ms`、`result_count`、`bytes_returned`、`embedding_calls`）。
  未认证的请求（`credential_missing` / `credential_invalid`）也记一条：`principal`、`credential_id`、`tenant_id` 置 null，`outcome` 为 `unauthorized`。
- **R10.2** [D2] 审计与日志**不含正文、snippet、query 原文和凭据**。是否记录带密钥的 query 摘要由 KB-OBS-01 决定；未决定前不记任何形式的 query。
- **R10.3** [D2] principal 必须是凭据绑定的主体，不得用"租户里最早的真人"或合成用户代替。
- **差距**：`audit_logs` 只有 `actor_user_id`，租户 key 的操作记到 `created_at` 最早的用户名下，principal 不进审计（矩阵 #2，`internal/types/audit_log.go:220`、`internal/middleware/auth.go:513`）→ KB-C1-03；访问日志记请求与响应正文各 10KB，handler 与服务层记 query 全文，配置 Langfuse 时 query 与命中预览进追踪（矩阵 #13，`internal/middleware/logger.go:96-124`、`:221-258`）→ KB-OBS-01。

## 11. 示例

约定：请求头都带 `Authorization: Bearer <redacted>`；ID 都是占位值；`request` / `response` / `status` 只是展示用的包裹层。各示例互相独立。

### 11.1 空范围（R2.3）
凭据范围是 `kb-a`，请求只要 `kb-z`（不存在或无权，不区分）。
```json
{ "request": { "op": "knowledge.search", "body": {
    "protocol_version": 1, "request_id": "01JB2Q7M3W5X8Y9Z0A1B2C3D4E", "generation": 7,
    "query": "报销流程", "scope": { "knowledge_base_ids": ["kb-z"] },
    "budget": { "deadline_ms": 8000, "max_results": 10, "max_concurrency": 2 } } },
  "response": { "status": 403, "body": { "error": {
    "code": "forbidden", "message": "No knowledge base in the effective scope.",
    "details": { "retryable": false, "reason": "empty_scope",
                 "request_id": "01JB2Q7M3W5X8Y9Z0A1B2C3D4E", "generation": 7 } } } } }
```

### 11.2 越界资源（R2.6、R6.4）
有效范围 `{kb-a}`；`doc-x` 存在，但属于 `kb-b`。即使带了 `expected_revision`，响应也与不存在的文档逐字相同。
```json
{ "request": { "op": "knowledge.read", "body": {
    "protocol_version": 1, "request_id": "01JB2Q8A0B1C2D3E4F5G6H7J8K", "generation": 7,
    "scope": { "knowledge_base_ids": ["kb-a"] }, "knowledge_id": "doc-x",
    "expected_revision": { "source_revision": "src-1", "index_build_id": "ib-1" },
    "page": { "cursor": null, "limit": 20 }, "max_bytes": 65536 } },
  "response": { "status": 404, "body": { "error": {
    "code": "not_found", "message": "Resource not found.",
    "details": { "retryable": false, "request_id": "01JB2Q8A0B1C2D3E4F5G6H7J8K", "generation": 7 } } } } }
```

### 11.3 撤权（R7.6）
签发权威已撤销该凭据，下一个请求即被拒绝。若撤的是用户对 KB 的授权而凭据仍有效，则表现为 11.1 或 11.2。
```json
{ "request": { "op": "knowledge.search", "body": {
    "protocol_version": 1, "request_id": "01JB2Q9C3D4E5F6G7H8J9K0M1N", "generation": 7,
    "query": "报销流程", "scope": { "knowledge_base_ids": ["kb-a"] },
    "budget": { "deadline_ms": 8000, "max_results": 10, "max_concurrency": 2 } } },
  "response": { "status": 401, "body": { "error": {
    "code": "unauthorized", "message": "Credential is not valid.",
    "hint": "Obtain a new credential from the issuing authority.",
    "details": { "retryable": false, "reason": "credential_revoked",
                 "request_id": "01JB2Q9C3D4E5F6G7H8J9K0M1N", "generation": 7 } } } } }
```

### 11.4 取消（R7.3）
请求以 generation 7 发出；用户关闭知识（Off），宿主把 generation 递增到 8 并中止请求；服务端在中止前已写出的响应迟到。
客户端收到的迟到响应（回显 generation 7）被丢弃，不进入上下文：
```json
{ "status": 200, "body": { "protocol_version": 1, "request_id": "01JB2QA4E5F6G7H8J9K0M1N2P3",
  "generation": 7, "completeness": "complete", "effective_scope": { "knowledge_base_ids": ["kb-a"] },
  "applied_budget": { "deadline_ms": 8000, "max_results": 10, "max_concurrency": 2 },
  "hits": [ { "rank": 1, "knowledge_base_id": "kb-a", "knowledge_id": "doc-1", "chunk_id": "chk-11",
    "revision": { "source_revision": "src-3", "index_build_id": "ib-5", "chunk_revision": 2 },
    "locator": { "valid": true, "start": 120, "end": 410 }, "index_state": "fresh",
    "score": 0.82, "score_space": "emb-g1", "snippet": { "text": "<redacted>", "truncated": false } } ] } }
```
Provider 的本地结果（同时写宿主侧日志；服务端没有观察到取消时，服务端审计不记 `cancelled`）：
```json
{ "error": { "code": "cancelled", "message": "Request was cancelled.",
  "details": { "retryable": false, "request_id": "01JB2QA4E5F6G7H8J9K0M1N2P3",
               "generation": 7, "current_generation": 8 } } }
```

### 11.5 stale（R4.4、R5.3）
与 11.4 无关。引用来自源修订 `src-8` 的活动构建 `ib-20`；之后原件被重新上传为 `src-9`，新构建尚未建好。
```json
{ "request": { "op": "knowledge.read", "body": {
    "protocol_version": 1, "request_id": "01JB3R0A1B2C3D4E5F6G7H8J9K", "generation": 15,
    "scope": { "knowledge_base_ids": ["kb-c"] }, "knowledge_id": "doc-7", "chunk_id": "chk-70",
    "expected_revision": { "source_revision": "src-8", "index_build_id": "ib-20", "chunk_revision": 1 },
    "max_bytes": 65536 } },
  "response": { "status": 409, "body": { "error": {
    "code": "stale", "message": "The requested revision is no longer available.",
    "hint": "Search again to obtain the current revision.",
    "details": { "retryable": false, "request_id": "01JB3R0A1B2C3D4E5F6G7H8J9K", "generation": 15,
      "current_revision": { "source_revision": "src-9", "index_build_id": null } } } } } }
```
修订一致时的成功响应形状：`{ "protocol_version", "request_id", "generation", "effective_scope", "knowledge_id", "revision": { "source_revision", "index_build_id" }, "segments": [{ "chunk_id", "chunk_revision", "text" }], "truncated", "next_cursor" }`。

### 11.6 partial（R3.4、R3.5）
`kb-a`、`kb-b` 同属 `emb-g1`；`kb-b` 的分支超时，`kb-a` 成功。
```json
{ "request": { "op": "knowledge.search", "body": {
    "protocol_version": 1, "request_id": "01JB3S1B2C3D4E5F6G7H8J9K0M", "generation": 21,
    "query": "差旅标准", "scope": { "knowledge_base_ids": ["kb-a", "kb-b"] },
    "budget": { "deadline_ms": 5000, "max_results": 10, "max_concurrency": 2 } } },
  "response": { "status": 200, "body": {
    "protocol_version": 1, "request_id": "01JB3S1B2C3D4E5F6G7H8J9K0M", "generation": 21,
    "completeness": "partial", "effective_scope": { "knowledge_base_ids": ["kb-a", "kb-b"] },
    "applied_budget": { "deadline_ms": 5000, "max_results": 10, "max_concurrency": 2 },
    "hits": [ { "rank": 1, "knowledge_base_id": "kb-a", "knowledge_id": "doc-2", "chunk_id": "chk-21",
      "revision": { "source_revision": "src-4", "index_build_id": "ib-9", "chunk_revision": 0 },
      "locator": { "valid": true, "start": 0, "end": 512 }, "index_state": "fresh",
      "score": 0.77, "score_space": "emb-g1", "snippet": { "text": "<redacted>", "truncated": true } } ],
    "partial_failures": [ { "knowledge_base_id": "kb-b", "code": "timeout", "retryable": true } ] } } }
```

### 11.7 按 KB 就绪（R8.2、R8.4）
有效范围是 `{kb-a, kb-b}`；`kb-z` 在范围外，被省略。
```json
{ "request": { "op": "knowledge.ready", "body": {
    "protocol_version": 1, "request_id": "01JB3T2C3D4E5F6G7H8J9K0M1N", "generation": 3,
    "scope": { "knowledge_base_ids": ["kb-a", "kb-b", "kb-z"] } } },
  "response": { "status": 200, "body": {
    "protocol_version": 1, "request_id": "01JB3T2C3D4E5F6G7H8J9K0M1N", "generation": 3, "live": true,
    "operations": { "search": { "state": "ready" }, "read": { "state": "ready" },
                    "ingest": { "state": "disabled", "reason": "read_only_v1" } },
    "knowledge_bases": [ { "knowledge_base_id": "kb-a", "search": { "state": "ready" } },
                         { "knowledge_base_id": "kb-b", "search": { "state": "not_ready", "reason": "index_building" } } ] } } }
```

## 12. 待决与需要评审确认的点

| # | 事项 | 本文的保守默认 | 链接 |
|---|---|---|---|
| 1 | 远端签发服务的部署、交付、运营与吊销责任 | 只做单用户样本与 Mock，不放宽 R1.2 | #992 |
| 2 | 权威授权 A 的粒度：全员同权还是差异权限 | v1 单用户，A = 签发时授权 | #993 |
| 3 | search 时 query embedding 等处理目的地与费用上限 | 不假定本地；未授权目的地零调用 | #994 |
| 4 | 谁可下载原件、preview 是否开放 | v1 不含下载、preview、`/files` | #996 |
| 5 | 旧修订正文能否取回、撤权后历史与引用如何保留 | v1 一律 `stale`，不返回旧正文 | #997 |
| 6 | 服务端是否校验 generation。**与方案 §4"两端校验"有差异，交评审裁定**（R7.5） | 服务端只回显 | #1013①②、KB-C1-03 |
| 7 | 是否在 #1011 列出的 8 类之外补 `invalid_request`、`internal` | 本文补上，评审时确认 | 本 PR 评审 |
| 8 | `integration.agent24_knowledge` 的键名 | 按本文命名，#1012 可改名但不改语义 | #1012 |
| 9 | **越界资源与不存在返回相同的 `not_found`**（R2.6）。上游现在区分 404 与 403，改动会改变对外行为 | 按本文：不可区分，防止探测资源是否存在 | 本 PR 评审 |
| 10 | **部分越界按交集执行并回报 `effective_scope`**（R2.5）。上游白名单是任一越界即整体 403 | 按本文：取交集；交集为空时按 R2.3 拒绝 | 本 PR 评审 |
| 11 | **`partial` 是成功状态，不是错误码**（R3.4、R6.5）。#1011 原文把 partial 列为错误类别；#1012 的夹具清单要把 partial 从"拒绝例"挪到"成功例" | 按本文：`completeness: "partial"` | 本 PR 评审、#1012 |
| 12 | **read 也必须带 `scope`**（R2.5、R4.1），目标资源须在 E 内 | 按本文 | 本 PR 评审 |
| 13 | **服务端端点只接受单一 score_space，跨分组由 adapter 轮转合并**（R3.6、R3.7） | 按本文 | 本 PR 评审 |
| 14 | **文档级 read 只比较 `source_revision` 与 `index_build_id`**（R4.3、R4.5），翻页中单个分块被编辑不算 stale | 按本文 | 本 PR 评审 |

## 变更记录
- r0（2026-10-16）：首稿。
- r1（2026-10-16）：按第 1 轮双审互评裁定修订。read 必须带 scope；Q、C、A 的错误码分开；新增判定顺序（R6.4）；新增活动构建，重写 `fresh` 与 `current_revision`；read 分单分块与文档级两种粒度；服务端只接受单一 score_space，Provider 层按秩轮转合并；`partial_failures` 带 `retryable`，并写明重试规则；信封与上游的差异，未知 `code` 按 `internal` 处理；写明 generation 只回显的理由；新增草案期规则；新增按 KB 就绪；新增示例 11.6、11.7；11.5 改为独立场景；来源标签可以是任务条目。影响夹具：是（#1012 尚未建清单）。
