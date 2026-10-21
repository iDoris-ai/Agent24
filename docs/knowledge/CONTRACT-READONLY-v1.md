# Knowledge 只读接入契约 v1（CONTRACT-READONLY-v1）

> **状态**：**草案（Draft）**，待 fork 侧维护者（David）评审（文档 PR，base `ab/weknora`）。评审通过前不授权任何实现。
> **版本**：契约 v1（报文字段 `protocol_version: 1`），文档修订 r9 · **日期**：2026-10-21 · **作者**：David Xu
> **Issue**：#1011（KB-CT-02a），阶段 #1000；探测依据 iDoris-ai/WeKnora#5。
> **依据**：任务方案 v2.1（方案全文见 #986 第一条评论）。每条规则后方括号里是来源：决策 `[D1]`–`[D8]`（方案 §1），或任务条目与方案章节（如 `[KB-C1-04]`、`[KB-C1-01b-b]`、`[方案 §4]`）。
> `[新设]` 表示本文新设计、在决策与方案里都找不到依据的规则；与其他标签并列（如 `[D1+新设]`）时，表示在该依据之上新增的部分。所有 `[新设]` 都待本 PR 评审（§12 #15）。
> **探测证据**：WeKnora 仓库 `docs/agent24/CAPABILITY-MATRIX.md`（KB-CT-01 PR①，iDoris-ai/WeKnora#7，以合入版为准；第 5 轮评审的遗留项见 iDoris-ai/WeKnora#8），下文"矩阵 #n"指其第 n 项。
> **受测 SHA**：WeKnora `4b363d153122797c857871ce7e4bb63626efc637`（`a24/develop` = 上游 v0.8.2 + 2 个 Agent24 提交，Go 源码与 v0.8.2 相同）。
> 文中 WeKnora 的 `file:line` 都按此 SHA，路径相对 WeKnora 仓库根目录。矩阵是静态源码阅读，运行证据待 KB-CT-01 PR② 产出；两者冲突时先回写矩阵，再按 §0.3 修订本文。

## 0. 范围、边界与变更规则

### 0.1 本文冻结什么
Agent24 的 KnowledgeProvider（下称"客户端"）与 WeKnora fork（下称"服务端"）之间，**首版只读接入**的语义：信任边界、范围、search、read、索引状态与 stale、错误分类、取消、就绪、能力发现、审计最小字段。
本文只定语义，不定实现：不规定 HTTP 路径、Go/Rust 类型、服务端在单个 `score_space` 内的融合算法、缓存结构、具体上限数值。§11 示例里的字段名是给 #1012 的 schema 直接使用的建议命名，包裹层（`request` / `response` / `status`）只用于展示。
§3 的线上响应由服务端产出。重试补齐与跨 `score_space` 合并由 Provider 层（Agent24 adapter）完成；这些合并规则（R3.7）属于 Provider 层，同样在本文冻结。

### 0.2 与相邻任务的边界
| 留给 | 内容 | 本文怎么引用 |
|---|---|---|
| #1012（KB-CT-02b） | `protocol/knowledge/v1/*.schema.json`、`protocol/knowledge/fixtures/v1/` 夹具与摘要清单、Rust/Ajv/Go 测试接线、Agent24 对外 REST 进 `protocol/openapi.yaml` | 本文给语义与字段名；schema 的类型、必填、长度上限、`format` 由 #1012 定 |
| #1013 ①（KB-CT-03） | profile / Off / 撤权状态机，generation 何时递增 | 本文只冻结"请求带 generation、迟到响应丢弃"（§7） |
| #1013 ② | 一次性交换票据、UI session JWT、消息桥，Provider 如何取得凭据 | 本文只用可重复使用的短期访问凭据（§1） |
| #1013 ③ | 初始化、ingest、job、ContextPack、SOP | v1 不含任何写操作；ingest 只出现在就绪表达里（§8） |
| KB-LITE-01a | Lite 启动握手、端口回报 | 不在本文范围（§8） |
| KB-C1-05 | Tool 层证据包裹（"数据不是指令"）、引用 UI | 本文只规定 hit/read 携带哪些修订与定位信息，以及引用义务（R4.7） |

### 0.3 变更规则
- **语义变更升主版本**：改字段含义、改范围或 stale 判定、改某个错误码的 `retryable`、删字段、新增或删除错误码（错误码是闭集，客户端按 `code` 分支）→ 新文件 `CONTRACT-READONLY-v2.md` 与 `protocol_version: 2`，v1 保留到双方迁移完成。
- **兼容新增记为 v1 修订**（r1、r2…）：新增可选字段、新的 `details.reason` 取值、新的能力声明键，以及 `index_state`、`completeness`、就绪 `state` 的新取值。后者之所以算兼容新增，是因为 R6.2 为这些枚举规定了保守兜底。`partial_failures[].code` 是错误码闭集的子集，新增取值属于语义变更。客户端必须忽略未知字段与未知 `reason`。
- **草案期**：v1 在评审通过、被 #1012 夹具引用、并且 iDoris-ai/WeKnora#7（矩阵）已合入之前处于草案期；这期间的语义修正记为 v1 修订（r1、r2…），不升主版本。KB-CT-01 PR② 的运行证据推翻某条语义时，也按这条处理。
- 每次修订在文末变更记录写明日期、修订号、是否影响夹具；影响夹具的修订要同一 PR 更新 #1012 的清单。
- schema 与本文冲突时，谁都不默认胜出：在同一 PR 里改正错误的一方。
- 文中标"待决"的条目链接到决策 issue；决定之前，实现按本文写的 v1 默认执行，不得自行放宽。

## 术语
| 术语 | 定义 |
|---|---|
| principal（主体） | 一次请求最终代表的人。由**签发权威**认定并绑定进凭据；服务端只认凭据里的 principal，不认请求自报。 |
| 签发权威（issuing authority） | 验证登录、成员资格与授权后签发凭据的可信服务端组件。remote 下是独立的可信签发服务，local-managed 下是本实例。 |
| 凭据（credential） | 本文专指可重复使用的**短期访问凭据**：绑定 principal、租户、范围、受众、到期时间，带唯一标识（`credential_id`，即 jti 或 key id）并可撤销。 |
| profile | Agent24 连接一个服务端的配置档：`local-managed`（宿主托管的本机单用户实例）或 `remote`（远端服务）。 |
| 资源（resource） | 知识库（KB，`knowledge_base_id`）、文档（`knowledge_id`）、分块（`chunk_id`）。 |
| scope（范围） | 一组 KB ID，表示可触及的资源边界。分权威授权 A、凭据范围 C、请求范围 Q 三种，交集为有效范围 E（§2）。 |
| revision（修订） | 修订标识（r9 起由内容派生，§5）：**源修订** `source_revision`（源指纹的带密钥摘要）、**构建标识** `index_build_id`（文档有构建时，其可服务分块集合的带密钥摘要）、**分块修订** `chunk_revision`（上游编辑计数，只作参考）、**内容标签** `content_tag`（分块完整正文的带密钥摘要，判断正文是否变化以它为准）。前两项是文档级修订。 |
| 可服务分块（servable chunk） | r9 取代 r8 的"活动构建"：文档**有构建**（parse_status 为 completed 或 finalizing）时，属于该文档、未删除、`chunk_type` 为 `text` 且启用的分块。v1 只返回可服务分块（R5.1）。 |
| score_space | 同一 embedding 模型与检索配置的分组标识；一次服务端请求只能落在一个 score_space（R3.6）。`score` 只能比较**同一个响应**里的 hit，跨响应（包括同一 score_space 的重试）不可比。 |
| generation（代） | 客户端为每个"profile × principal"绑定维护的单调递增整数。任何使在途工作作废的事件都让它加一（规则见 #1013①）。 |
| deadline / 预算 | 调用方给出的时限与资源上限（返回量、并发、字节）。 |
| partial | 结果可用但不完整：部分分支失败或超时。是一种成功状态，不是错误。 |

## 1. 信任边界

- **R1.1** [D2] 签发权威只在可信服务端一侧，凭据的签发、收窄、撤销都由它完成。**remote 下 Desktop 不持有任何签名密钥**，只持有并出示凭据；local-managed 见 R1.4。
- **R1.2** [D2+新设] **remote 与共享租户下，租户 HMAC 绝不下发 Desktop**：持有它就能为任意 `sub` 签名（local-managed 的收窄解释见 R1.4，§12 #17）。[KB-C4-02a] 平台 API key 在任何 profile 下都不进 Desktop，也不用于知识读取。
  证据：signed_token 要求 `aud=weknora`、exp 必填、HS256（矩阵 #2，`internal/middleware/auth.go:623-626`），剩余有效期不超过 24h、nbf、token 的 tenant_id 与 key 所属租户一致（`:644-651`），之后只取出 `sub`（校验函数 `:613-659`）；租户 Owner 可经 `POST /tenants/:id/api-principal-test-token` 为任意外部主体签发（`internal/handler/tenant.go:1021-1095`）；平台 key 经 X-Tenant-ID 进入任意租户（矩阵 #10，`auth.go:388-408`），创建请求没有 KB 白名单字段，白名单为空（`internal/handler/system.go:101-105`）。
- **R1.3** [D2] remote：凭据由可信签发服务在验证登录、成员资格、授权之后签发，绑定用户、可撤销、范围受限。签发服务部署在哪、由谁交付与运营**待决**（#992）；决定之前只允许单用户样本与 Mock。
- **R1.4** [D2+新设] local-managed：实例是本机单用户实例，principal 就是本机唯一用户，签发由本实例完成。宿主可以持有**本实例专属**的秘密（含该实例自己的 JWT_SECRET 或 HMAC），只用于本实例，不跨实例、不跨 profile 使用，也不与 remote 共用（D2 原文"local-managed：宿主只管理本实例专属秘密"；已定，David，2026-10-17，§12 #17）。Provider 如何取得凭据见 KB-HOST-01 与 #1013②；实例托管见 KB-LITE-01a。
  **前提**：KB-LITE-01a（只监听本机、端口回报、握手）与 KB-C1-01（关闭自助注册入口）完成之前，"本机单用户实例"不成立：无头 Lite 默认监听 `0.0.0.0` 并开放自助注册（矩阵 #12、#15）。在此之前 local-managed 只用于受控样本。
- **R1.5** [D2] 服务端只信凭据里绑定的 principal。`direct` 模式（直接读 `X-External-User-ID`，`auth.go:580-594`）在集成模式下必须拒绝；改 principal Mode 不算撤权手段（矩阵 #11）。
- **R1.6** [D2] 防重放要求凭据标识配有撤销或消费机制；只有 jti 字段不算防重放。上游 signed_token 无 jti、无单主体撤销（矩阵 #2、#11）。
- **R1.7** [D2] 凭据不进 renderer、URL、普通日志，不跨 origin 发送（KB-HOST-01、KB-C1-02b）。
- **差距**：上游 principal 不参与 KB ACL，不进审计，且决定会话、记忆、MCP OAuth、沙箱凭据归属（矩阵 #2）→ KB-C1-03；集成模式下关闭 direct、限制服务端签发入口 → KB-C1-03、KB-C1-01。

## 2. 范围语义

- **R2.1** [D2] **有效范围 E = 权威授权 A ∩ 凭据范围 C ∩ 请求范围 Q**，由服务端在每个请求上计算；客户端提供的 Q 只能收窄，不能扩大。
- **R2.2** [D2] **服务端内部 deny-all 默认**：A 缺失或为空、C 为空集，都按空集处理，绝不解释为"全部"，结果按 R2.3 返回 `forbidden/empty_scope`。C 缺失或无法解析是凭据问题，返回 `unauthorized/credential_invalid`。凭据签发时范围为空就拒绝签发。客户端输入的处理见 R2.5。
- **R2.3** [D2] **空交集拒绝**：Q 形状合法但 E 为空（包括 A 为空、C 为空集）时，整个请求返回 `forbidden`，`details.reason: "empty_scope"`，不执行检索、不计算 query embedding。示例见 §11.1。`operations.X` 为 `disabled` 时先按 R6.4 返回 `operation_disabled`，不进入本条。
- **R2.4** [D2] **永不回写空数组**：任何一方在收窄后得到空集时，必须拒绝，不得把 `[]` 写进下游请求、凭据或配置。上游在多处把空列表当"全部"或"回退到默认"。
- **R2.5** [D2+新设] **search 与 read 都必须带 `scope.knowledge_base_ids`**（就绪请求可选，见 R8.2）。Q 缺失、为 `[]` 或无法解析，一律返回 `invalid_request`；C 缺失或无法解析返回 `unauthorized`，`reason: "credential_invalid"`（C 为空集按 R2.2）。Q 与 A ∩ C 部分重叠时按交集执行，响应用 `effective_scope` 回报实际范围，不说明其余 ID 是不存在还是无权；`effective_scope` 比请求范围窄时，Tool 层必须标出"范围已被收窄"。
- **R2.6** [新设] **越界资源与不存在不可区分**：这里的资源只指 `knowledge_id` 对应的文档。read 的目标文档必须落在 E 内；落在 E 之外时，返回与"不存在"完全相同的 `not_found`（同码、同 message、无 reason）。正在删除（deleting）的文档视同已删除（r9）。`chunk_id` 一律按锚点校验（R4.4），不走这条。search 结果永远不含 E 之外的资源。示例见 §11.2。
- **R2.7** [D3/D4] v1 凭据的操作范围只含 search 与 read；下载原件、preview、`/files` 不在 v1 内（矩阵 #5、#6；谁可下载原件待决 #996）。集成模式不消费既存的组织共享与共享 Agent 授权（`internal/application/access/knowledgebase.go:120-154`）。
- **证据与差距**：
  - 上游空白名单 = 全租户：`AllowsKnowledgeBase` / `AllowsKnowledgeBases` 遇空列表返回 true（矩阵 #1，`internal/types/tenant_api_key.go:328-330`、`:345-347`）；`apiKeyAny()`（定义在 `internal/router/rbac.go:226-228`）对任何有效 key 放行（`internal/middleware/api_key_gate.go:149-151`）。
  - hybrid-search 的 `knowledge_base_ids` 为空时回退到路径里的 KB（`internal/application/service/knowledgebase_search.go:134-137`；参数说明见 `internal/types/search.go:244-247`），是 R2.4 的直接风险点。
  - 上游受白名单限制的 key 越界时"任一越界即整体 403"（`tenant_api_key.go:378-386`），且校验在查库之前（`internal/middleware/kb_access.go:202`）；白名单放行之后，KB 不存在返回 404、他租户无共享返回 403（`kb_access.go:214-220`；`access/knowledgebase.go:102`、`:155`），后者可用来探测存在性，与 R2.6 不符。
  - 上游没有用户级授权 A，同租户内无 per-user ACL（矩阵 #10）。A 在 v1 单用户样本里等于签发时的授权；如何表达 A（凭据声明还是服务端查询）由 KB-C1-03 定，差异权限是否首版支持待决（#993）。
  - → KB-C1-03（∩ 与空拒绝）、KB-C1-01（允许列表、禁消费既存共享）。

## 3. search

- **R3.1** [D1] 检索走 hybrid-search 一路；`/knowledge-search` 不用于集成：它用 `logger.CloneContext` 脱开请求 ctx（矩阵 #3，`internal/handler/session/qa.go:829`），参数错误返回 400（`qa.go:834-844`、`:874-884`），白名单拒绝返回 Forbidden（`:886-889`），服务层错误一律 500 并带回内部错误原文（`:902-905`）。
- **R3.2** [D1] 请求包含：`query`（文本）、`scope`（§2）、`budget`，以及通用字段 `protocol_version`、`request_id`、`generation`（§7）。
- **R3.3** [D1+新设] `budget`：`deadline_ms`（相对时限，避免时钟偏差）、`max_results`、`max_concurrency`（**单次请求内**服务端对各分支的并发上限）。Provider 层对多次请求的总预算另行控制（KB-C1-04）。超过服务端上限时**截到上限**，不报错，响应用 `applied_budget` 回报实际值；上限在能力声明里公开（§9）。
- **R3.4** [D1+新设] **完整性**：服务端按 E 内的 KB 分支执行（E 内不可搜索的 KB 视为失败分支，R3.5；检索所需的基础设施就绪时，空 KB 是成功分支，否则与其他 KB 一样按 `dependency` 失败）。全部分支成功 → `completeness: "complete"`（E 内全是空 KB 时为 `complete` 加 0 命中）；至少一个分支成功完成（哪怕 0 命中）而其余失败或超时 → `"partial"`；一个都没成功 → 所有失败都因到达 deadline 时返回 `timeout`，否则返回 `unavailable`。原因混杂时，顶层错误取"最不可重试"的一项：只要任一失败不可重试，整体 `retryable: false`，`reason` 优先取不可重试的那一项；错误信封的 `details` 可以携带 `partial_failures`，让客户端分别处理。
- **R3.5** [D1+新设] **partial**：`partial_failures[]` 每项为 `{ "knowledge_base_id", "code": "timeout" | "unavailable", "reason", "retryable" }`；`reason` 取 `timeout`、`dependency`、`not_ready`、`overloaded`、`index_building`、`unsupported_kb_type`、`index_failed`（没有文档有构建、也没有在构建中的文档，即全部 failed 或 cancelled，r9），前五项可重试，后两项不可重试；`code` 为 `timeout` 当且仅当 `reason` 为 `timeout`。`dependency` 表示该分支所需的基础设施（存储、向量库、需要时的 embedding 模型）不可用，与 R8.4 对齐；`not_ready` 表示服务端整体或该组尚未完成初始化，只出现在组级错误的合并（R3.7(b)）与顶层 `unavailable`；客户端对两者的重试处理相同，区分只用于诊断与审计。这与 R3.7(b)（合并时 `reason` 用该组错误的值）及 §6 中 search 的 `unavailable` reason 一致。E 内没有可搜索内容的 KB（取值同 R8.4：构建中为 `index_building`，全部 failed 或 cancelled 为 `index_failed`，有构建但可服务集合都为空为 `unsupported_kb_type`；正在删除的文档不计入，r9）也列入 `partial_failures`（`code: "unavailable"`），`completeness` 记为 `partial`；空 KB（还没有任何文档）是成功分支，0 命中，不列入。E 内没有任何分支成功时按 R3.4 返回错误。上游的失败单位是 store group：一组失败时，组内所有 KB 都要列入（`knowledgebase_search_fanout.go:89-104`）。客户端可以只针对可重试的失败 KB 发一个新请求重试（新 `request_id`、当前 generation），计入剩余总预算，结果按 R3.7(a) 合并。partial 结果可以使用，但 Tool 层必须标出不完整（KB-C1-05）。示例见 §11.6。
- **R3.6** [D1+新设] **排序与 score_space**：响应里 `hits` 的顺序就是权威顺序，`rank` 从 1 连续编号。`score` 只是参考，并带 `score_space`；**`score` 只能比较同一个响应里的 hit**：上游 hybrid 用 RRF，分数只取决于本次请求内的名次（`internal/application/service/knowledgebase_search_fusion.go:33-53`、`:125-126`），纯关键词模式按本次请求内的最高分缩放（`:40-47`、`:94` 起）。v1 服务端端点只接受 E 落在同一个 score_space 的请求；E 跨多个 score_space 时返回 `invalid_request`，`reason: "mixed_score_space"`，`details.groups` 为 `[{ "score_space", "knowledge_base_ids" }]`，只列 E 内的 KB，不泄露范围外的。
- **R3.7** [新设] **Provider 层合并**：
  - (a) **重试补齐**（同一 score_space）：把重试结果当成新的一组，与原结果按 (b) 的秩轮转规则合并（原结果在前），按 `chunk_id` 去重（保留先出现者），截到原请求的 `max_results`，重新编号 `rank`。**不得按 `score` 合并**（R3.6）。重试成功的 KB 从 `partial_failures` 移除，`completeness` 按 R3.4 重新计算。
  - (b) **跨组合并**：按 `details.groups`（或已知分组）每组发一个请求。组内保持服务端顺序；组间按秩轮转交错（各组第 1 名、各组第 2 名……，组的先后按请求里 KB 的首次出现顺序）；按 `chunk_id` 去重（保留先出现者）；总命中截到调用方的 `max_results`，重新编号 `rank`。组级错误：`forbidden` 且 `reason` 为 `empty_scope` 时，把该组的 KB 从合并结果的 `effective_scope` 移除，不算整体失败，所有组都是 `empty_scope` 时返回 `forbidden`；其他 `forbidden`，以及 `unauthorized`、`invalid_request`、`internal`，合并结果整体返回这个错误，其他组已拿到的结果全部丢弃。`timeout` 与 `unavailable` 并入 partial：只要还有一组成功，该组的 KB 都并入 `partial_failures`（`code` 与 `reason` 用该组错误的值，`reason` 缺省时 `timeout` 对 `timeout`、`unavailable` 对 `dependency`），合并结果为 `partial`；所有组都失败时，按 R3.4 返回 `timeout` 或 `unavailable`。`effective_scope` 取各组并集；`applied_budget` 逐项取各组实际值中最严的；`completeness` 取各组中最差的，`partial_failures` 取并集。
  - **合并结果里的 `score`**：来自不同响应，彼此不可比较（R3.6）。Provider 交给 Tool 层的合并结果不带 `score`，只保留 `rank`；需要分数时只能在单个服务端响应内部使用。
  - (c) **分组从哪来**：按 KB 就绪响应（R8.4）里每个 KB 带的 `score_space`，或 `mixed_score_space` 错误的 `details.groups`。
- **R3.8** [D1+新设] 每个 hit 带：`knowledge_base_id`、`knowledge_id`、`chunk_id`、`chunk_type`、修订标识 `revision`（`source_revision`、`index_build_id`、`chunk_revision`、`content_tag`，§5）、`locator`、`index_state`（§5）、`score`、`score_space`、受字节上限约束的 `snippet`，以及可选的 `display: { title, file_name }`。snippet 是检索证据，不是正文；`display` 只是展示元数据，**不是正文，不能作为引文**；引用正文用 read（R4.7）。snippet 必须从与 `content_tag` **同一次回表取到的同一串数据库正文**中截取，不得使用索引侧的匹配文本、标题或上下文头（r9）。
  **hit 只能是检索命中**：v1 的 hit 不包含上下文扩展附带的父块、邻块与关系块。服务端在这个端点上要关闭扩展（上游 `SkipContextEnrichment` 默认 false，`internal/types/search.go:248-251`），或把附带结果过滤掉。
  **分块类型**：v1 的 hit 与 read 只包含 `text` 类型、且 `is_enabled` 为真的分块，`chunk_type` 恒为 `"text"`；`parent_text`、模型生成的类型（summary、entity、relationship、table_summary、table_column）、image_ocr / image_caption、faq、web_search、wiki_page 一律不出现（用户可见的取舍，§12 #16）。上游类型清单见 `internal/types/chunk.go:17-39`，`IsEnabled` 在 `:141`，`ChunkType` 字段在 `:156`；上游检索路径的 `isSearchableChunk` 放行 text、summary、table_column、table_summary、faq、image_ocr、image_caption，并丢弃 processing 与 failed 状态的分块（`internal/application/service/knowledgebase_search_results.go:361-378`）；read 侧的 chunk 列表默认只返回 text（`internal/handler/chunk.go:128`）。
- **R3.9** [D1+新设] hit 的正文、`chunk_revision` 与 `content_tag` 都取自数据库当前行，并且在同一个数据库快照里核对该分块属于可服务集合（R5.1）；不可服务的分块不进 hit。召回来自检索索引，**索引里的版本可能比数据库旧**（§13），这影响召回质量，不影响标识与正文的一致性。精确引用只来自单分块 read（R4.7）。
- **R3.10** [D1+新设] **locator**：坐标按该索引构建的解析输出文本中的 rune 偏移理解，区间 `[start, end)`，待 PR② 实测。`locator.valid` 为 `true` 须同时满足：`chunk_revision == 0`、分块正文未被改写（父块或邻块扩展）、区间合法（`end > start`）、正文 rune 长度等于 `end − start`；否则为 `false`，不给偏移。**v1 的 locator 只为将来精确定位预留；r9 起 v1 服务端恒给 `valid: false`、不给偏移**（`chunk_revision == 0` 不能证明正文未被改写，§13）：v1 没有能取到原文区间的接口，客户端不得依赖它取文本。read 的 `segment.text` 是分块正文，不等于解析输出文本的 `[start, end)` 区间。依据：上游 `chunkTrusted`（`internal/application/service/chat_pipeline/merge_overlap.go:94-99`）；`StartAt` / `EndAt`（`internal/types/chunk.go:147-150`）。
- **R3.11** [KB-SVC-POLICY-01/#994] 只读检索也可能产生处理出口：开向量检索且调用方没给 query_embedding 时，服务端会算一次 query embedding（矩阵 #3，`internal/application/service/knowledgebase_search.go:206-213`）。允许的处理目的地待决（#994），由 KB-SVC-POLICY-01 执行；客户端不得假定 search 只在本地发生。
- **差距**（矩阵 #3）：
  - 无调用方 deadline，内部只有 12s 解析上限与多组 30s 超时（`knowledgebase_search_storegroup.go:21`、`knowledgebase_search_fanout.go:23`）。
  - fan-out 是全有或全无：任一组失败都收成 VectorStoreUnavailable（`knowledgebase_search_fanout.go:89-104`），**没有 partial**。
  - 默认做上下文扩展，把父块、关系块、邻块作为额外项追加（`knowledgebase_search_results.go:129-170`），父块继承原命中的分数（`:146`），其余为 0。
  - 上游检索放行多种分块类型（R3.8），v1 的类型过滤是净新增；丢弃 processing / failed 分块与上游一致（`knowledgebase_search_results.go:366-371`，R5.3）。
  - hit 无修订，`ContentRevision` 与 `ContentRewritten` 都不序列化（`internal/types/search.go:220`、`:226`）；`ContentRevision` 赋值自数据库当前行（`knowledgebase_search_results.go:337`），由于 processing / failed 已被丢弃，与已入索引的版本一致（R3.9）；多库检索要求 embedding 一致（`knowledgebase_search.go:178`）。
  - → KB-C1-01c ①③。

## 4. read

- **R4.1** [D1] 请求包含：`scope`（§2，规则与 search 相同）、`knowledge_id`；可选 `chunk_id`、`expected_revision`、`page`（`cursor`、`limit`；`limit` 可选，单分块续读只需 `cursor`）、`max_bytes`、`deadline_ms`；以及通用字段。从引用发起的 read **必须**带 `chunk_id` 与 `expected_revision.content_tag`（取自 search hit，r9）；非引用发起的 read 可以不带。
- **R4.2** [D1] **只返回分块正文**：响应 `segments[]` 每项含 `chunk_id`、`chunk_type`（v1 恒为 `"text"`，R3.8）、`chunk_revision`、`content_tag`、`text`；响应可带可选 `display: { title, file_name }`（不是正文，不能作为引文）。元数据、标题、描述、spans、stages 都不是正文，不得当作引文出示。上游 `/knowledge/:id/spans`、`/stages` 是处理阶段时间线（`internal/handler/knowledge.go:649` 起），但会序列化 `input`、`output`、`metadata`（`internal/types/knowledge_span.go:92-94`）以及 `error_code`、`error_message`（`:95-96`），后者也可能带出原文；是否带出解析正文待探测（iDoris-ai/WeKnora#8）；无论是否带出，都不得作为正文或引文来源。
- **R4.3** [新设] **粒度**：
  - 带 `chunk_id`：只返回这一个分块（单个 segment）。`expected_revision` 可选；带了就**必须**含 `content_tag`，缺了返回 `invalid_request`；`source_revision`、`index_build_id`、`chunk_revision` 可以一并带上，只作参考，不参与判定（r9）。被 `max_bytes` 截断时，用 `next_cursor` 继续读同一个分块，游标绑定 `content_tag`，`content_tag` 覆盖整段分块正文；
  - 不带 `chunk_id`：按文档顺序分页，`expected_revision` 只能含、也只比较 `source_revision` 与 `index_build_id`（多带 `chunk_revision` 为 `invalid_request`）。
  - 响应顶层 `revision` 只放文档级两项；`chunk_revision` 与 `content_tag` 放在各 segment 里。
- **R4.4** [D1+新设] **校验检索时修订**：当前修订 = 当前源修订 + 当前可服务分块集合（R5.3）。标识与正文在同一个数据库快照里取得（r9）。**v1 不提供旧版本**，不返回旧正文。
  - **带 `chunk_id`**（文档已按 R2.6 判定在 E 内）：按顺序判定（r9）。(1) 分块不在该文档当前的可服务集合里（不存在、已删除、属于其他文档包括 E 外文档、文档没有构建、已禁用、非 `text`）：返回 `stale`，`reason: "anchor_missing"`，`current_revision` 照常给出，**这几种情况彼此不可区分**。(2) 在可服务集合里，带了 `expected_revision` 且 `content_tag` 与当前不同：返回 `stale`，不带 `reason`，附 `current_revision`、`current_chunk_revision` 与 `current_content_tag`，**不附正文**。(3) 其余：200。文档里其他分块的增删（`index_build_id` 变化）不影响单分块 read：分块原样还在就可以读。上游重新解析时会删除旧分块并以新 ID 重建，旧 `chunk_id` 随之落入 (1)；删除失败留下残留时见 §13。示例见 §11.5。
  - **不带 `chunk_id`**：带 `expected_revision` 且两项任一不同，返回 `stale`，`current_revision` 给出当前两项（没有构建时 `index_build_id` 为 `null`）。没带 `expected_revision` 又没有构建时：构建中（pending / processing）返回 `unavailable`，`reason: "index_pending"`，`retryable: true`，可带 `retry_after_ms`；构建失败或已取消（failed / cancelled，r9）返回 `unavailable`，`reason: "index_failed"`，`retryable: false`。有构建但可服务集合为空时返回 200，`segments: []`。`index_pending` / `index_failed` 只用于这种文档级 read。
  - 上游 `/revisions` 能取历史正文（`internal/handler/chunk.go:257-268`），但编辑可能是删改敏感内容。可否取回旧版本与 #997 的保留要求相关，本身另需决策（§12 #5）。
- **R4.5** [D1+新设] **上限与时限**：`max_bytes` 与 `page.limit` 超过服务端上限时截到上限；正文在字符边界截断，`truncated: true` 并给 `next_cursor`。`next_cursor` 也用于按 `page.limit` 翻页，与 `truncated` 无关。`deadline_ms` 内没完成返回 `timeout`；不传时以服务端上限为准。游标绑定首页的修订：文档级分页绑定两项，并且每一页都在与本页正文相同的快照里重算 `index_build_id` 后比对；单分块读绑定 `content_tag`（R4.3，r9）；任一项变了，后续页返回 `stale`。同时带 `expected_revision` 与游标时，先按 `expected_revision` 判定（R4.4），再按游标判定；任一不是当前修订都返回 `stale`，`current_revision` 照常给出。只有文档级分页时，翻页期间单个分块被编辑不算 stale，segment 照常返回读取时的 `chunk_revision`。
- **R4.6** [D2] read 每次都按 §2 重新计算 E，目标资源必须在 E 内；"之前 search 返回过"不构成授权。
- **R4.7** [D1+新设] **引用义务**：精确引用必须来自带 `chunk_id` 与 `expected_revision.content_tag` 的单分块 read，且判定为 200（R4.4）。单分块读跨页时各页 `content_tag` 必须一致（R4.5）；`content_tag` 覆盖整段正文，客户端要先拿齐整段才能拼接引用。文档级 read 的 segment 与 search 的 snippet 只能作为"当前正文"或线索出示，不生成确定引用（r9）。
- **差距**（矩阵 #4）：能读分块（分页默认 10、最大 100，`internal/handler/chunk.go:117-125`），但不能按指定修订读并校验，没有 stale；服务层错误一律 500 并带回内部错误原文（`chunk.go:74-75`）→ KB-C1-01c ②。

## 5. 索引状态与 stale

- **R5.1** [D1+新设] **标识由内容派生**（r9 取代 r8 的"源修订—索引构建—活动构建"三者映射）：文档**有构建**指 parse_status 为 completed 或 finalizing；有构建时，其**可服务分块集合**是属于该文档、未删除、`text` 且启用的分块。`index_build_id` 是 knowledge_id 与可服务集合（分块 ID 排序后）的带密钥摘要，有构建但集合为空时照算，没有构建时为 `null`；`content_tag` 是 `chunk_id` 与该分块完整正文的带密钥摘要；`source_revision` 是 knowledge_id 与源指纹的带密钥摘要，**源指纹**由上游知识行的 `file_hash`、`file_path`、`source` 与手工知识的 version 组成。所有派生标识都是服务端密钥的 HMAC-SHA256，输入按"域前缀（`source` / `build` / `chunk`）、各组成项"依次以 NUL 字节分隔、文本按 UTF-8 编码，输出为小写十六进制，对外不透明；缺少密钥时服务端拒绝进入集成模式；换密钥后全部标识都变，旧引用一律按 stale 处理。上游重建时以新 ID 生成分块，旧 `chunk_id` 不再可服务（R4.4）。
- **R5.2** [D1+新设] `source_revision`、`index_build_id`、`content_tag` 是**不透明标识**（R5.1）。`chunk_revision` 沿用上游的编辑计数，0 表示未编辑，**只作参考**：上游某些写路径改了正文却不递增它，判断正文是否变化以 `content_tag` 为准（r9）。`updated_at`、`file_hash` 不直接对外。
- **R5.3** [D1+新设] **当前修订**（`current_revision`）= 当前 `source_revision` + 当前 `index_build_id`（没有构建时为 `null`）。hit 的 `index_state` 取值：
  - `fresh`：r9 的 v1 服务端对每个 hit 都给 `fresh`：hit 必须在同一快照里属于可服务集合（R3.9），不可服务的直接丢弃；
  - `stale`：保留给客户端兼容，r9 的 v1 服务端不再给出；
  - `unknown`：保留给客户端兼容，r9 的 v1 服务端不再给出（`chunk_revision` 恒为整数）；客户端收到时按 `stale` 对待。客户端遇到**无法识别**的取值是另一回事，按 R6.2 兜底。
  **v1 沿用上游的丢弃行为**：上游标记为处理中或索引失败（`index_status` 为 processing / failed）的分块不作为 hit（`knowledgebase_search_results.go:366-371`）。这只是一层额外过滤：`index_status` 为 ready 时，索引里的版本仍可能比数据库旧（§13）；无论哪种情况，hit 的正文与标识都取自数据库当前行（R3.9，r9）。
- **R5.4** [D1] **展示**：`stale` / `unknown` 的 hit 展示方式相同：可以作为线索出示，但必须标"可能已过期"，不生成确定引用；确定引用只来自 R4.7。
- **证据与差距**：Chunk 有 `content_revision` 与 `index_status`（ready/processing/failed，`internal/types/chunk.go:132-135`），可映射为分块修订与 R5.3 的一部分；Knowledge 有 `parse_status` 与 `file_hash`（`internal/types/knowledge.go:189`、`:218`）；**没有源修订、索引构建与活动构建标识，也没有三者映射**（矩阵 #4）→ KB-C1-01c ③；r9 改为由内容派生的标识（R5.1），不需要三者映射，代价见 §13。

## 6. 错误分类

- **R6.1** [KB-C1-01c①+新设] **信封**：集成端点输出独立的字符串码信封 `{"error": {"code", "message", "hint"?, "details": {"retryable", "request_id", "generation", "reason"?, ...}}}`（`reason` 是否出现以 §6 为准），与 Agent24 现有信封同形。`request_id` 与 `generation` 只在三种情形下可以为 `null`：请求体无法解析时（`invalid_request`）；请求体能解析，但 `request_id` 或 `generation` 本身缺失或不合法时（`invalid_request`，无法回显的那一项为 `null`；`protocol_version` 先判，所以这时也可能是 `unsupported_protocol_version`）；以及按判定顺序（R6.4）在解析请求体之前就失败的 `unauthorized`、`internal`。其余情形必须回显。上游信封不同：`code` 是整数（`internal/errors/errors.go:9` 的 `ErrorCode int`，AppError 见 `:56-61`），没有 `hint`，`details` 里也没有 `retryable`、`request_id`、`generation`，外层还有 `success: false`（`internal/middleware/error_handler.go:23-43`）。上游前端依赖现有信封，所以不改它，只在集成端点另出（KB-C1-01c ①）。
- **R6.2** [KB-C1-01c①+新设] `code` 是闭集（`partial_failures[].code` 是它的子集），**客户端按 `code` 分支，不按 HTTP 状态分支**，HTTP 状态是建议映射。**未知值兜底**：未知 `code`（含 `partial_failures[].code`）按 `internal` 分支，是否重试以该项自带的 `retryable` 为准，没有 `retryable` 就不重试；未知 `index_state` 按 `stale`；未知 `completeness` 按 `partial`；未知就绪 `state` 按 `not_ready`。这与 `protocol/openapi.yaml` 的开放枚举约定不同，是有意为之：未知错误码可能出自更严格的拒绝，不能按 HTTP 状态猜成可重试。
- **R6.3** [KB-C1-01c①] `message` 是固定的通用文本，不得含内部错误原文、query、正文、凭据或资源是否存在的提示。上游把 `err.Error()` 原样返回（矩阵 #3，`internal/handler/knowledgebase.go:366-372`）→ KB-C1-01c ①。
- **R6.4** [新设] **服务端判定顺序**：凭据（`unauthorized`）→ 请求形状（`invalid_request`）→ 操作是否关闭（`forbidden`，`reason: "operation_disabled"`，§12 #20）→ 计算 E（`forbidden`；search 的 `mixed_score_space` 在 E 之后判定）→ 文档（`knowledge_id`）存在且在 E 内（否则 `not_found`）→ 锚点与修订（`stale`）→ 执行（`timeout` / `unavailable` / partial）。由此：
  - 越界资源即使带 `expected_revision`，也返回 `not_found`；
  - search 之后文档被删，read 返回 `not_found`；
  - 带 `chunk_id` 时锚点不成立（不在当前可服务集合里：不存在、已删除、属于其他文档含 E 外文档、文档没有构建、已禁用、非 `text`），返回 `stale`（`anchor_missing`），彼此不可区分；锚点成立但 `content_tag` 不同，返回不带 `reason` 的 `stale`（R4.4，r9）；
  - 不带 `chunk_id`、没有构建且未带 `expected_revision`，返回 `unavailable`（`index_pending` / `index_failed`，R4.4）；
  - 服务端不支持请求的 `protocol_version`：`invalid_request`，`reason: "unsupported_protocol_version"`，`details.supported` 列出支持的版本。`protocol_version` 是请求形状一步的第一项，先于其余形状检查；`request_id`、`generation` 能回显就回显，不能则为 `null`（R6.1）；
  - 请求形状通过后、计算 E 之前，先判对应操作是否被关闭：`operations.search` / `operations.read` 为 `disabled` 时返回 `forbidden`，`reason: "operation_disabled"`（不可重试），优先于 `empty_scope`，也先于 `operation_not_allowed`（凭据不允许此操作，R2.7）（§12 #20）。

| code | 建议 HTTP | retryable | 含义与 `details` |
|---|---|---|---|
| `invalid_request` | 400 | false | `reason` 条件必填：带 `groups` 时必须为 `mixed_score_space`，带 `supported` 时必须为 `unsupported_protocol_version`，其余情况不带。请求形状不合法：Q 缺失、为 `[]` 或无法解析，字段越界，带 `chunk_id` 时 `expected_revision` 缺 `content_tag`，文档级 `expected_revision` 缺项（r9）；`reason`：`mixed_score_space`（带 `groups`）/ `unsupported_protocol_version`（带 `supported`）。**补充项**，见 §12 |
| `unauthorized` | 401 | false | `reason` 必填。凭据缺失、无法解析、无效、过期或已撤销；`reason`：`credential_missing` / `credential_invalid` / `credential_expired` / `credential_revoked` |
| `forbidden` | 403 | false | `reason` 必填。凭据有效但不允许：`reason`：`empty_scope`（E 为空，含 A 为空、C 为空集）/ `operation_not_allowed`（凭据不允许此操作，R2.7）/ `operation_disabled`（服务端关闭了该操作，换凭据也没用，R6.4、§12 #20）/ `policy_denied` |
| `not_found` | 404 | false | 文档（`knowledge_id`）不存在、已删除（含正在删除，r9），**或在 E 之外**（R2.6）。`reason` 禁止。分块问题一律走 `stale/anchor_missing` |
| `stale` | 409 | false | 修订不是当前修订且不可取（`current_revision`；`current_chunk_revision` 与 `current_content_tag` 只在带 `chunk_id`、锚点成立但 `content_tag` 不同时出现，与 `anchor_missing` 互斥（r9）），或 `reason: "anchor_missing"`（带 `chunk_id` 时锚点不成立，R4.4）。`reason` 条件必填：锚点不成立时必须为 `anchor_missing`；`content_tag` 不同、文档级修订不一致时不带。重新 search |
| `timeout` | 504 | true | `reason` 可选，只能为 `timeout`；缺省时按 `timeout` 理解（同 R3.7(b)）。没有分支成功，且所有失败都因到达 deadline；read 在 `deadline_ms` 内没完成 |
| `unavailable` | 503 | true（`index_failed`、`unsupported_kb_type` 为 false；原因混杂时按 R3.4 取最不可重试的一项） | `reason` 可选；但 `retryable` 为 `false` 时必填，取不可重试的那一项（`unsupported_kb_type` 或 `index_failed`，R3.4）；缺省时按 `dependency`（可重试）理解（同 R3.7(b)）。没有分支成功且至少一个失败不是 deadline，或依赖不可用、未就绪；`reason`：`not_ready` / `dependency` / `overloaded` / `index_building` / `unsupported_kb_type` / `index_failed`（search，R3.5；`details` 可带 `partial_failures`）/ `index_pending`（文档级 read，稍后再查就绪）/ `index_failed`；可带 `retry_after_ms` |
| `internal` | 500 | false | `reason` 禁止。服务端意外故障；客户端遇到未知 `code` 也按此处理。**补充项**，见 §12 |
| `cancelled` | 不上线 | false | **只出现在客户端本地**（§7）：中止的请求没有响应，服务端不会发出它；#1012 的线上错误 schema 不收录。`details` 另带 `current_generation` |

- **R6.5** [D1+新设] `partial` 不是错误码：它是 search 成功响应的 `completeness`（R3.4）。
- **R6.6** [D1+新设] 重试一律是新请求（新 `request_id`、当前 `generation`），计入同一总预算。`retryable: false` 指不能用同一凭据、同一请求原样重发；换新凭据后发新请求不算自动重试。

## 7. 取消与 generation

- **R7.1** [KB-C1-04] 客户端取消在途请求的方式是中止该 HTTP 请求。服务端必须让取消沿请求上下文传导，停止扇出与后续计算；服务端观察到连接中止时，审计 `outcome` 记 `cancelled`。
- **R7.2** [KB-C1-04] 每个请求带 `request_id`（客户端生成，唯一）与 `generation`；服务端在响应和错误信封里原样回显两者；例外是 R6.1 允许回显为 `null` 的错误信封。
- **R7.3** [KB-C1-04] 客户端只接受 `request_id` 匹配在途请求、且 `generation` 等于当前值的响应；否则**丢弃**：不注入上下文、不进缓存、不作为证据，Provider 返回本地 `cancelled` 并写宿主侧日志。示例见 §11.4。R6.1 允许回显为 `null` 的错误信封不按回显配对，而按发起它的那次 HTTP 交换配对：该次请求仍在途、且发出时的 `generation` 等于当前值，就照常接受（例如 `unauthorized` 照常进入续签或撤权处理），不因回显为 `null` 丢弃；否则同样丢弃，迟到响应照样返回本地 `cancelled`。
- **R7.4** [KB-C1-04] 作废顺序：先阻止新调用，再取消在途请求，最后丢弃迟到响应。generation 何时递增、Off 是否持久等状态规则由 #1013① 定。
- **R7.5** [新设] 方案 §4 风险表写的是"两端校验 generation"。v1 服务端**只回显、不校验**，比方案 §4 弱，是临时取舍。理由：服务端校验需要把 generation 绑定进凭据，这依赖尚未冻结的身份交换（#1013②）与 KB-C1-03。在此之前由客户端单端丢弃保证"迟到响应不被消费"；交评审裁定（§12 #6）。
- **R7.6** [D2+新设] 撤权在下一个请求生效。对已在途的请求，服务端应在组装响应前复验凭据与有效范围：凭据失效返回 `unauthorized`；E 缩小但没变空时，结果按新的 E 过滤，不补齐，并回报新的 `effective_scope`；`partial_failures` 同步过滤，只保留 E 内的 KB，过滤完一项不剩时 `completeness` 改为 `complete`，否则保持不变；E 变空时返回 `forbidden`。上游 key、JWT、成员关系逐请求查库，但在途的流式或长请求不因撤权中止（矩阵 #11，`internal/application/service/tenant_api_key.go:91-100`）→ KB-C1-03。
- **R7.7** [D2] 取消和撤权都不能追回已经交付给模型或用户的内容，界面与审计不得声称可以做到（保留与删除要求待决 #997）。
- **差距**：上游 hybrid-search 用请求 ctx，断开会传导（矩阵 #3，`internal/handler/knowledgebase.go:359`）；但父 ctx 被取消时 fan-out 返回 `ctx.Err()`（`knowledgebase_search_fanout.go:95-97`），handler 把它包成 500（`knowledgebase.go:366-372`）。`/knowledge-search` 不传导（R3.1）。所以取消处理与 partial（R3.4）对上游都是净新增 → KB-C1-01c ①。

## 8. 就绪

- **R8.1** [KB-C1-01b-b/D8] **health ≠ ready**：无凭据的存活检查用 `/health`，只表示进程存活，不带业务含义。上游 `/health` 写死返回 ok（矩阵 #9，`internal/router/router.go:145-147`）。Lite 的启动握手属于 KB-LITE-01a，不在本文范围。
- **R8.2** [KB-C1-01b-b/D8+新设] 就绪探针要求凭据，`scope` 可选。不带 scope：只返回 `live`（进程存活，布尔）与 `operations`：`search`、`read`、`ingest` 各有 `state`（`ready` / `not_ready` / `disabled`）与可选 `reason`；`operations` 只反映基础设施（存储、向量库、需要时的 embedding 模型），不依赖任何 KB。只读 v1 下 `ingest` 为 `disabled`。带 scope：另返回 `knowledge_bases`（R8.4）。
  **两层关系**：`operations.X` 为 `ready` 只说明基础设施能服务 X；某个 KB 能否服务 X 看它自己的状态。`operations.X` 不是 `ready` 时，所有 KB 的 X 也都不是 `ready`；反之不成立（如 §11.7 里 `operations.search` 为 `ready` 而 `kb-b` 为 `not_ready`）。
- **R8.3** [KB-C1-01b-b] search 就绪只取决于检索所需的存储、向量库与（需要时）query embedding 模型，不依赖 chat、docreader 或默认 KB，避免与初始化互相等待。
- **R8.4** [KB-C1-01b-b+新设] **按 KB 就绪**：带 `scope.knowledge_base_ids` 时按 §2 计算 E（规则相同，E 为空返回 `forbidden`）；响应 `knowledge_bases[]` 每项为 `{ "knowledge_base_id", "score_space", "search": { "state", "reason"? }, "read": { "state", "reason"? } }`，只列 E 内的 KB，范围外的 ID 直接省略，不区分不存在与无权。**聚合**：正在删除（deleting）的文档在下列判定中一律不计入（r9）。某 KB 的 search 为 `ready`，当且仅当 `operations.search` 为 `ready`（R8.2），并且满足以下之一：(i) KB 为空（还没有任何文档；正在删除的文档不计入，r9）；(ii) KB 内至少一个文档有构建且可服务分块集合非空（R5.1，r9）。read 同理，看 `operations.read`。不为 `ready` 时，按下面的顺序取第一个满足的 `reason`：`operations.X` 为 `disabled`（被关闭，不是故障）→ `operation_disabled`（不可重试，§12 #19）；基础设施不可用 → `dependency`（可重试）；没有文档有构建、且至少一个文档在构建中（pending / processing）→ `index_building`（可重试）；没有文档有构建、也没有在构建中的文档（全部 failed 或 cancelled，r9）→ `index_failed`（不可重试）；有构建但可服务集合都为空（如 FAQ 类 KB、text 分块全部禁用，R3.8）→ `unsupported_kb_type`（不可重试）。示例见 §11.7。`operations.X` 为 `disabled` 时，KB 级 X 的 `state` 为 `not_ready`、`reason` 为 `operation_disabled`，`operation_disabled` 也只在这时出现；`dependency` 当且仅当 `operations.X` 为 `not_ready` 时出现。
- **R8.5** [KB-C1-01b-b] 服务端就绪与 Agent24 宿主的 profile 状态（registered / loading / ready / degraded / unavailable / disabled）是两回事；后者由前者映射而来，映射规则见 #1013①。
- **差距**：没有 /ready，也没有按操作的探针（矩阵 #9）→ KB-C1-01b-b。

## 9. 能力发现

- **R9.1** [D8] 能力发现扩展 `/system/capabilities`：在其 `data` 层现有的 `edition`、`capabilities` 之外新增 `integration` 对象（data 结构见 `internal/handler/deployment_capabilities.go:29-39`；外层是 `{code, msg, data}`，见 `:127-133`）。
- **R9.2** [D8+新设] `integration.agent24_knowledge` 至少包含：`protocol_versions`（支持的契约主版本列表）、`mode`（`integration` / `standalone`）、`supported_operations`（声明支持的操作，与就绪里的 `operations` 对象区分）、`server_version`（服务端版本，满足 R9.5）、`limits`（R3.3、R4.5 的上限）。
- **R9.3** [D8] 能力声明只说明"支持什么"，**不说明依赖是否健康或业务是否就绪**；就绪只看 §8。集成模式下声明必须如实反映已禁用的路由（KB-C1-01）。
- **R9.4** [D8+新设] **兼容判定**：缺少 `agent24_knowledge` 声明、`mode` 不是 `integration`，或双方没有共同的主版本时，客户端把该 profile 视为不兼容，不连接，也不回退到未版本化的上游接口。有共同版本时取最高的那个。
- **R9.5** [D8] retrieve 级凭据必须能读到能力声明和服务端版本。上游 `/system/capabilities` 任意 key 可读，但 `/system/info` 要 manage_vector_stores 或 full access（矩阵 #7、#8，`internal/router/routes_auth_tenant.go:252-255`）→ KB-C1-01b-a。

下例只是 `data` 层：
```json
{ "edition": "lite", "capabilities": { "integrations.api": { "supported": true } },
  "integration": { "agent24_knowledge": {
    "protocol_versions": [1], "mode": "integration", "supported_operations": ["search", "read"],
    "server_version": "<server-version>",
    "limits": { "max_results": 50, "max_concurrency": 4, "max_deadline_ms": 30000,
                "max_read_bytes": 262144, "max_page_limit": 100, "max_snippet_bytes": 2048 } } } }
```
上面的数值只是示意，实际上限由服务端配置与 #1012 定。

## 10. 审计最小字段

- **R10.1** [#1011 内容10 / KB-C1-03 / KB-OBS-01] 每次 search、read 与就绪请求都产生一条审计事件（`knowledge.search` / `knowledge.read` / `knowledge.ready`），至少包含：
  `occurred_at`、`request_id`、`generation`、`principal`（凭据绑定的主体）、`credential_id`、`tenant_id`、`effective_scope`（KB ID 列表）、
  `resource`（read：`knowledge_id` 与 `chunk_id` 列表；search：命中的 `chunk_id` 列表与数量）、`revision`（read 时的文档级两项与各 segment 的分块修订；search 时各 hit 的 `source_revision`、`index_build_id`、`chunk_revision`；**不记 `content_tag`**，r9）、
  `outcome`（`ok` / `partial` / 错误码 / `cancelled`）、`metering`（具体字段 `latency_ms`、`result_count`、`bytes_returned`、`embedding_calls` 为 [新设]）。
- **R10.2** [#1011 内容10 / KB-C1-03 / KB-OBS-01+新设] **未认证请求也记一条最小审计**，`outcome` 为 `unauthorized`：`credential_missing` / `credential_invalid` 时 `principal`、`credential_id`、`tenant_id` 都为 null；`credential_expired` / `credential_revoked` 只在签名校验通过后才记 `credential_id` 与 `principal`；签名不通过的按 `credential_invalid` 记 null。
- **R10.3** [#1011 内容10 / KB-C1-03 / KB-OBS-01] 审计与日志**不含正文、snippet、query 原文和凭据**。是否记录带密钥的 query 摘要由 KB-OBS-01 决定；未决定前不记任何形式的 query。`content_tag` 也不进审计与日志（r9）。
- **R10.4** [#1011 内容10 / KB-C1-03 / KB-OBS-01] principal 必须是凭据绑定的主体，不得用"租户里最早的真人"或合成用户代替。
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
有效范围 `{kb-a}`；`doc-x` 存在，但属于 `kb-b`。即使带了 `expected_revision`，响应除回显字段（`request_id`、`generation`）外与不存在的文档相同。
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
签发权威已撤销该凭据，下一个请求即被拒绝。若撤的是用户对 KB 的授权而凭据仍有效：E 变空时表现为 11.1，目标资源落到 E 外时表现为 11.2；只是部分重叠时按交集执行，响应里的 `effective_scope` 会缩小。
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
请求以 generation 7 发出：
```json
{ "op": "knowledge.search", "body": {
    "protocol_version": 1, "request_id": "01JB2QA4E5F6G7H8J9K0M1N2P3", "generation": 7,
    "query": "报销流程", "scope": { "knowledge_base_ids": ["kb-a"] },
    "budget": { "deadline_ms": 8000, "max_results": 10, "max_concurrency": 2 } } }
```
用户关闭知识（Off），宿主把 generation 递增到 8 并中止请求。前提：中止时服务端的响应已经写进缓冲区，因此仍然到达，成为迟到响应。客户端收到的迟到响应（回显 generation 7）被丢弃，不进入上下文：
```json
{ "status": 200, "body": { "protocol_version": 1, "request_id": "01JB2QA4E5F6G7H8J9K0M1N2P3",
  "generation": 7, "completeness": "complete", "effective_scope": { "knowledge_base_ids": ["kb-a"] },
  "applied_budget": { "deadline_ms": 8000, "max_results": 10, "max_concurrency": 2 },
  "hits": [ { "rank": 1, "knowledge_base_id": "kb-a", "knowledge_id": "doc-1", "chunk_id": "chk-11",
    "chunk_type": "text",
    "revision": { "source_revision": "src-3", "index_build_id": "ib-5", "chunk_revision": 0, "content_tag": "ct-11a" },
    "locator": { "valid": false }, "index_state": "fresh",
    "score": 0.82, "score_space": "emb-g1", "snippet": { "text": "<redacted>", "truncated": false } } ] } }
```
Provider 的本地结果（同时写宿主侧日志；服务端只有观察到连接中止时才在审计记 `cancelled`）：
```json
{ "error": { "code": "cancelled", "message": "Request was cancelled.",
  "details": { "retryable": false, "request_id": "01JB2QA4E5F6G7H8J9K0M1N2P3",
               "generation": 7, "current_generation": 8 } } }
```

### 11.5 stale（R4.4、R5.3）
与 11.4 无关。引用来自 hit：`chk-70` 当时的内容标签是 `ct-70a`、分块修订是 1；之后它被人工编辑为 2，内容标签变为 `ct-70b`，文档级两项不变。
```json
{ "request": { "op": "knowledge.read", "body": {
    "protocol_version": 1, "request_id": "01JB3R0A1B2C3D4E5F6G7H8J9K", "generation": 15,
    "scope": { "knowledge_base_ids": ["kb-c"] }, "knowledge_id": "doc-7", "chunk_id": "chk-70",
    "expected_revision": { "content_tag": "ct-70a", "chunk_revision": 1 },
    "max_bytes": 65536, "deadline_ms": 5000 } },
  "response": { "status": 409, "body": { "error": {
    "code": "stale", "message": "The requested revision is no longer available.",
    "hint": "Search again to obtain the current revision.",
    "details": { "retryable": false, "request_id": "01JB3R0A1B2C3D4E5F6G7H8J9K", "generation": 15,
      "current_revision": { "source_revision": "src-8", "index_build_id": "ib-20" },
      "current_chunk_revision": 2, "current_content_tag": "ct-70b" } } } } }
```
其他情形（R4.4，r9）：原件被重新上传、旧分块已被删除时，带 `chunk_id` 的 read 返回 `stale`，`reason: "anchor_missing"`（旧 `chunk_id` 不在可服务集合里）；文档里只是增删了**别的**分块时，原分块照常可读。不带 `chunk_id` 的文档级 read 带着旧的 `expected_revision`，返回 `stale`，`current_revision` 给出新的两项（没有构建时 `index_build_id` 为 `null`）；不带 `expected_revision` 的文档级 read 遇到构建中，返回 `unavailable`（`index_pending`，`hint` 为"稍后再查就绪"）。
修订一致时的成功响应形状：`{ "protocol_version", "request_id", "generation", "effective_scope", "knowledge_id", "revision": { "source_revision", "index_build_id" }, "display"?, "segments": [{ "chunk_id", "chunk_type", "chunk_revision", "content_tag", "text" }], "truncated", "next_cursor" }`。

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
      "chunk_type": "text",
      "revision": { "source_revision": "src-4", "index_build_id": "ib-9", "chunk_revision": 0, "content_tag": "ct-40a" },
      "locator": { "valid": false }, "index_state": "fresh",
      "score": 0.77, "score_space": "emb-g1", "snippet": { "text": "<redacted>", "truncated": true },
      "display": { "title": "<redacted>", "file_name": "<redacted>" } } ],
    "partial_failures": [ { "knowledge_base_id": "kb-b", "code": "timeout", "reason": "timeout", "retryable": true } ] } } }
```

### 11.7 按 KB 就绪（R8.2、R8.4）
有效范围是 `{kb-a, kb-b, kb-f, kb-e}`；`kb-z` 在范围外，被省略。基础设施就绪，所以 `operations.search` 为 `ready`；`kb-b` 处于首次构建期间，`kb-f` 是 FAQ 类 KB，`kb-e` 是还没有文档的空 KB。
```json
{ "request": { "op": "knowledge.ready", "body": {
    "protocol_version": 1, "request_id": "01JB3T2C3D4E5F6G7H8J9K0M1N", "generation": 3,
    "scope": { "knowledge_base_ids": ["kb-a", "kb-b", "kb-f", "kb-e", "kb-z"] } } },
  "response": { "status": 200, "body": {
    "protocol_version": 1, "request_id": "01JB3T2C3D4E5F6G7H8J9K0M1N", "generation": 3, "live": true,
    "operations": { "search": { "state": "ready" }, "read": { "state": "ready" },
                    "ingest": { "state": "disabled", "reason": "read_only_v1" } },
    "knowledge_bases": [
      { "knowledge_base_id": "kb-a", "score_space": "emb-g1",
        "search": { "state": "ready" }, "read": { "state": "ready" } },
      { "knowledge_base_id": "kb-b", "score_space": "emb-g2",
        "search": { "state": "not_ready", "reason": "index_building" },
        "read": { "state": "not_ready", "reason": "index_building" } },
      { "knowledge_base_id": "kb-f", "score_space": "emb-g1",
        "search": { "state": "not_ready", "reason": "unsupported_kb_type" },
        "read": { "state": "not_ready", "reason": "unsupported_kb_type" } },
      { "knowledge_base_id": "kb-e", "score_space": "emb-g1",
        "search": { "state": "ready" }, "read": { "state": "ready" } } ] } } }
```

## 12. 待决与需要评审确认的点

| # | 事项 | v1 默认 | 链接 |
|---|---|---|---|
| 1 | 远端签发服务的部署、交付、运营与吊销责任 | 只做单用户样本与 Mock，不放宽 R1.2 | #992 |
| 2 | 权威授权 A 的粒度：全员同权还是差异权限 | v1 单用户，A = 签发时授权 | #993 |
| 3 | search 时 query embedding 等处理目的地与费用上限 | 不假定本地；未授权目的地零调用 | #994 |
| 4 | 谁可下载原件、preview 是否开放 | v1 不含下载、preview、`/files` | #996 |
| 5 | 能否取回旧修订正文：与 #997 的保留要求相关，可否取回本身另需决策 | v1 一律 `stale`，不返回旧正文 | 本 PR 评审，必要时另开决策 issue；#997 |
| 6 | 服务端是否校验 generation。**比方案 §4"两端校验"弱，是临时取舍，交评审裁定**（R7.5） | 服务端只回显 | #1013①②、KB-C1-03 |
| 7 | 是否在 #1011 列出的 8 类之外补 `invalid_request`、`internal` | 本文补上，评审时确认 | 本 PR 评审 |
| 8 | `integration.agent24_knowledge` 的键名 | 按本文命名，#1012 可改名但不改语义 | #1012 |
| 9 | **越界资源与不存在返回相同的 `not_found`**（R2.6）。上游现在区分 404 与 403，改动会改变对外行为 | 按本文：不可区分，防止探测资源是否存在 | 已定（David，2026-10-17） |
| 10 | **部分越界按交集执行并回报 `effective_scope`**（R2.5）。上游白名单是任一越界即整体 403 | 按本文：取交集；交集为空时按 R2.3 拒绝 | 已定（David，2026-10-17） |
| 11 | **`partial` 是成功状态，不是错误码**（R3.4、R6.5）。#1011 原文把 partial 列为错误类别；#1012 的夹具清单要把 partial 从"拒绝例"挪到"成功例" | 按本文：`completeness: "partial"` | 本 PR 评审、#1012 |
| 12 | **read 也必须带 `scope`**（R2.5、R4.1），目标资源须在 E 内 | 按本文 | 本 PR 评审 |
| 13 | **服务端端点只接受单一 score_space，Provider 层按 R3.7 补齐与轮转合并** | 按本文 | 本 PR 评审 |
| 14 | **文档级 read 只比较 `source_revision` 与 `index_build_id`**（R4.3、R4.5），翻页中单个分块被编辑不算 stale | 按本文 | 本 PR 评审 |
| 15 | 所有标 `[新设]` 的规则 | 按本文 | 本 PR 评审 |
| 16 | **v1 只收 `text` 分块**（R3.8）：FAQ、图片 OCR / 描述、表格摘要类内容在 v1 搜不到，是用户可见的范围取舍 | 按本文 | 已定（David，2026-10-17） |
| 17 | **local-managed 宿主可否持有本实例的 HMAC / JWT_SECRET**（R1.4），是对 D2"绝不下发"的收窄解释 | 可以持有，限本实例专属、只用于本实例（R1.4） | 已定（David，2026-10-17） |
| 18 | **#1012 的取消夹具**由三部分组成：一条线上成功响应、一条 Provider 本地 `cancelled` 结果、一条"迟到响应被丢弃"的测试用例；不进 Go 端编码测试 | 按本文 | #1012 |
| 19 | **`operations.X` 为 `disabled` 时，KB 级 X 的 `reason` 取什么**（R8.2、R8.4）：盲选结论分歧 | KB 级 X 为 `not_ready`，`reason` 为新增的 `operation_disabled`（不可重试），排在 R8.4 原因顺序第一位（兼容新增） | 已定（David，2026-10-17） |
| 20 | **`operations.search` / `operations.read` 为 `disabled` 时，search / read 请求本身返回什么错误码**：契约未定义 | 对应请求返回 `forbidden`，`reason: "operation_disabled"`（不可重试，兼容新增）；判定位置在请求形状之后、计算 E 之前，优先于 `empty_scope`（R6.4） | 已定（David 确认，见 [#1012 评论](https://github.com/iDoris-ai/Agent24/issues/1012#issuecomment-6239062330)；Grok、Opus 盲选一致，2026-10-18） |

## 13. 已知差距（r9）

r9 用由内容派生的标识换掉了"可证明的构建归属"（KB-C1-01c ③ 的设计取舍，WeKnora#38）。下面几项不在 v1 的保证范围内，客户端与 Tool 层据此设计：
- **残留分块**：上游删除旧分块失败时只打警告、继续处理，残留分块会进入可服务集合，文档级正文可能混入旧内容；单分块引用以 `content_tag` 核对，不受影响。
- **索引滞后**：召回来自检索索引，索引里的版本可能比数据库旧；hit 展示的正文与标识取自数据库当前行，一致但不保证就是被检索打分的那个版本。
- **并发覆盖**：上游部分写路径按整行保存，可能覆盖并发编辑，表现为正文变了而 `chunk_revision` 不变；`content_tag` 能发现。
- **URL 类知识**：上游对 URL 类知识的 `file_hash` 只按 URL 计算，页面正文变化不一定改变 `source_revision`；重新抓取会重建分块，单分块引用仍以 `content_tag` 核对。

## 变更记录
- r0（2026-10-16）：首稿。
- r1（2026-10-16）：按第 1 轮双审互评裁定修订。read 必须带 scope；Q、C、A 的错误码分开；新增判定顺序（R6.4）；新增活动构建，重写 `fresh` 与 `current_revision`；read 分单分块与文档级两种粒度；服务端只接受单一 score_space，Provider 层按秩轮转合并；`partial_failures` 带 `retryable`，并写明重试规则；信封与上游的差异，未知 `code` 按 `internal` 处理；写明 generation 只回显的理由；新增草案期规则；新增按 KB 就绪；新增示例 11.6、11.7；11.5 改为独立场景；来源标签可以是任务条目。影响夹具：是（#1012 尚未建清单）。
- r2（2026-10-16）：按第 2 轮双审互评裁定修订。重试补齐与跨组合并分成两条（R3.7 a/b/c）；locator 只在未编辑、未改写时有效，坐标为原文 rune 偏移（R3.10）；v1 只含 `text` 且启用的分块，hit 与 segment 带 `chunk_type`；hit 的 `chunk_revision` 定义（R3.9）；能力声明 `mode` 必须为 `integration`，`operations` 改名 `supported_operations`；R1.2 / R1.4 区分 remote 与 local-managed；新增引用义务（R4.7）；timeout 判定统一，read 增加 `deadline_ms`；未知枚举兜底；`cancelled` 只在客户端本地；判定顺序改为先认证，新增 `unsupported_protocol_version`；`index_pending`；就绪定义 `live`、按 KB 给出 search/read 与 `score_space`，并要求凭据；`failed` 优先于 `stale`；新增 `[新设]` 标签与 `display` 元数据；修正证据引用。影响夹具：是（#1012 尚未建清单）。
- r3（2026-10-16）：按第 3 轮双审互评裁定修订。`score` 只在同一响应内可比，重试补齐改为按秩轮转合并；单分块读的游标绑定三项；hit 不含上下文扩展结果；分块类型的证据改引 `isSearchableChunk` 并补差距；跨组合并遇到 `unauthorized` / `forbidden` / `invalid_request` / `internal` 整体失败；就绪 `scope` 可选，写明 `operations` 与按 KB 状态的关系，新增 `unsupported_kb_type`；local-managed 的前提；没有活动构建时拆成 `stale` 与 `unavailable`（`index_pending` / `index_failed`），新增 `anchor_missing`；locator 坐标改绑索引构建；`partial_failures[].code` 属错误码闭集，未知值以自带 `retryable` 为准；取消夹具组成；来源标签调整；草案期结束条件加 WeKnora#7；证据引用修正；能力声明加 `server_version`；范围收窄的标注与不补齐；11.4 补请求体，11.5 改为分块编辑场景，11.7 增加 FAQ 类 KB。影响夹具：是（#1012 尚未建清单）。
- r4（2026-10-17）：按第 4 轮双审互评裁定修订。`chunk_id` 跨构建不稳定（v1 约定），构建变了一律 `anchor_missing`；带 `chunk_id` 的 read 判定统一为 `anchor_missing`，`expected_revision` 可选但须三项齐全，`index_pending` / `index_failed` 只用于文档级 read；v1 不返回编辑后未入索引或索引失败的分块作为 hit，`index_state` 去掉 `failed`；补标 `[新设]`；`partial_failures` 增加 `reason`，E 内不可搜索的 KB 列入 partial，失败单位为 store group；locator 有效条件补齐并改为预留；证据修正（`:337`、knowledge_span `:92-96`、`knowledgebase_search.go:134-137`）；C 缺失与 C 为空集分开；示例措辞；审计中过期/撤销凭据的记录条件；R5.2 类型说明；跨组合并对 `empty_scope` 的处理；R7.6 同步过滤 `partial_failures`；KB 级就绪的聚合规则；#997 的挂靠。影响夹具：是（#1012 尚未建清单）。
- r5（2026-10-17）：按第 5 轮评审（David 选定三项）修订。带 `chunk_id` 的 read 把文档判定与锚点判定分开：`not_found` 只看文档，分块问题一律 `stale/anchor_missing` 且彼此不可区分，锚点还要求 `index_build_id` 等于活动构建，`chunk_id` 不跨构建复用；空 KB 是成功分支，就绪为 `ready`；全部索引失败的 KB 记 `index_failed`；就绪聚合改为"活动构建里有 v1 范围内的分块"；无分支成功且原因混杂时取最不可重试的一项，`details` 可带 `partial_failures`；local-managed 在 jason 确认前宿主不持有签发秘密；§12 #9、#10、#16、#17 改为需 jason 确认。影响夹具：是（#1012 尚未建清单）。
- r6（2026-10-17）：按第 6 轮双审互评（David 选定三项，不再审）修订。空 KB 的就绪与成功分支加上基础设施就绪的前提，补 `dependency` 原因并定出就绪原因的优先级；合并结果的 `score` 不可比较，交给 Tool 时去掉；R8.2、R8.4、R3.10、R9.2 补标 `[新设]`。不影响夹具清单，只需在 #1012 增加“基础设施不可用时空 KB 为 `not_ready/dependency`”一例。
- r7（2026-10-17）：§12 #9、#10、#16、#17 由 David（fork 侧维护者、Knowledge 负责人）确认，不涉及内核接口，无须 jason 确认。#9、#10、#16 按原文；#17 采用确认后的放宽：local-managed 宿主可以持有本实例专属的秘密（R1.4、R1.2 同步）。不影响夹具清单。
- r8（2026-10-17）：按 #1012 待决问题的裁定修订（按 David 2026-10-17 授权的夜间规则，由 Opus 和 Sonnet 代替 Grok 和 Codex 盲选，两票一致才执行；PR 合入前请 David 确认。裁定见 [#1012 评论](https://github.com/iDoris-ai/Agent24/issues/1012#issuecomment-6224023854)）。R3.5 的 `partial_failures[].reason` 增加 `not_ready`、`overloaded`，与 R3.7(b) 及 §6 一致（兼容新增），并写明 `code` 为 `timeout` 当且仅当 `reason` 为 `timeout`；§6 表逐码写明 `reason` 必填、条件必填、可选或禁止，R6.1 注明以 §6 为准；文件头修订号改为当前值；§6 的 `unavailable` 在 `retryable` 为 false 时必须带不可重试的 reason；R4.5 写明 `next_cursor` 也用于翻页；R4.1 的 `page.limit` 可选；R6.1 写明回显为 `null` 的情形（含请求体能解析但 `request_id` / `generation` 本身缺失或不合法）；R8.4 新增 KB 级 `reason` `operation_disabled`（`operations.X` 为 `disabled` 时使用，不可重试，排在原因顺序第一位；§12 #19，David 2026-10-17 定），并写明 `dependency` 当且仅当 `operations.X` 为 `not_ready`；§12 新增 #19、#20（#20 待决，给出 v1 默认值，覆盖 search 与 read）。§6 中 `unavailable` 在 `retryable` 为 false 时 reason 必填一项不在 Q3 裁定原文里，是 #1012 第 2 轮评审补充的（与 R3.4 一致）。PR-Daemon 评审后再补（按 Grok、Opus 盲选一致执行，David 已确认，见 [#1012 评论](https://github.com/iDoris-ai/Agent24/issues/1012#issuecomment-6239062330)，2026-10-18）：R7.2、R7.3 写明回显为 `null` 的错误信封按发起它的 HTTP 交换配对；R6.4 写明 `protocol_version` 先判，R6.1 相应允许 `unsupported_protocol_version` 回显 `null`；R4.5 写明 `expected_revision` 与游标同时出现时的判定顺序；R3.5 区分 `dependency` 与 `not_ready`；R8.4 写明 disabled 时 KB 级 `state` 为 `not_ready`；#20 的 v1 默认改为新增的 `forbidden/operation_disabled`，并写明判定位置（#20 已定）。影响夹具：是，#1012 清单改为 r8，新增一个带 `overloaded` 的 partial 夹具、一个 `operation_disabled` 的就绪夹具、#20 默认值的 search 与 read 两个 `operation_disabled` 夹具，以及一个解析前就失败、回显为 `null` 的 `unauthorized` 夹具。
- r9（2026-10-21）：KB-C1-01c ③ 改用内容寻址（WeKnora#38，设计经 Grok 与 Codex 两轮双审，David 选定并确认）。新增 `content_tag`（分块完整正文的带密钥摘要），出现在 hit.revision、segment、单分块 `expected_revision` 与 stale 详情（`current_content_tag`）；"活动构建"改为"有构建时的可服务分块集合"，`index_build_id`、`source_revision` 改为由内容派生的带密钥摘要（R5.1）；单分块 read 先判可服务、再比 `content_tag`，其他分块增删不影响（R4.3、R4.4、R6.4）；`chunk_revision` 只作参考（R5.2）；snippet 与 `content_tag` 同源（R3.8、R3.9）；文档级翻页每页同快照重算（R4.5）；引用只来自单分块 read（R4.7）；cancelled 不可服务、文档级 read 为 `index_failed`，deleting 视同已删除（R2.6、R4.4、R8.4）；就绪按"有构建"重写判定（R8.4）；审计不记 `content_tag`（R10.1、R10.3）；locator 恒为 `valid: false`（R3.10）；新增 §13 已知差距。PR 双审第 1、2 轮补充：R5.1 写明各标识的输入组成与 HMAC 编码；R3.5、R8.4 不计入 deleting；单分块 `expected_revision` 的 `chunk_revision` 允许 null；夹具新增 read.stale-chunk-overwritten、read.document-empty。影响夹具：是（同一 PR 更新清单与 schema）。
