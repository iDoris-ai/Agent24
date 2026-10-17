# Knowledge 初始化 / ingest / job / ContextPack / SOP 语义契约 v1（CONTRACT-OPS-v1）

> **状态**：**草案（Draft）**，待 fork 侧维护者（David）评审（文档 PR，base `ab/weknora`）。评审通过前不授权任何实现。
> **版本**：契约 v1，文档修订 r0 · **日期**：2026-10-17 · **作者**：David Xu
> **Issue**：#1013 ③（KB-CT-03），阶段 #1000；消费方 KB-C4-02b、KB-C4-04、KB-CTX-01、KB-CTX-02、KB-SOP-01、KB-SOP-02，另供 KB-C4-03、KB-C4-05、KB-SVC-POLICY-01 引用。
> **依据**：任务方案 v2.1（方案全文见 #986 第一条评论）；`docs/knowledge/CONTRACT-READONLY-v1.md`（下称 READONLY，按 r7）；`docs/knowledge/CONTRACT-STATE-v1.md`（下称 STATE 草案，按 r5）与 `docs/knowledge/CONTRACT-UI-BRIDGE-v1.md`（下称 BRIDGE 草案，按 r5）。**两份草案都尚未合入**：本文只引用它们的条款，不假设其中待决的条目已定；依赖关系登记在 §11 #C3、#C4，草案定稿时逐条复核。
> 来源标签沿用 READONLY：决策 `[D1]`–`[D8]`、任务条目（如 `[KB-C4-04]`）、READONLY 规则（如 `[R2.1]`）、STATE 草案规则（如 `[S4.5]`）、BRIDGE 草案规则（如 `[U1.3]`）、决策 issue（如 `[#996]`）。`[新设]` 表示本文新设计、在以上来源里都找不到依据的规则；与其他标签并列时，表示在该依据之上新增的部分。所有 `[新设]` 都待本 PR 评审（§11 #B0）。
> **分片**：分两片提交，一起合入。第一片：§0–§6（授权绑定、授权矩阵、幂等、初始化、ingest 与 job 及其验收）、§10、§11 中标"第一片"的条目；第二片：§7–§9（ContextPack、SOP 候选与批准及其验收）与 §11 其余条目。单独阅读第一片时，正文引用的 W7.x、W8.x 都在第二片。
> **现状证据**：WeKnora 的 `file:line` 都按 READONLY 固定的受测 SHA `4b363d15`（`a24/develop` 之后的提交只改文档，Go 源码不变），路径相对 WeKnora 仓库根目录；Agent24 的现状沿用 STATE 草案 §9、BRIDGE 草案 §9（`ab/weknora` 的 `eb1a58b`）。汇总在 §10。

## 0. 范围、边界与变更规则

### 0.1 本文冻结什么
Agent24（Provider 与宿主，下称"客户端"）与 WeKnora fork（下称"服务端"）之间**会改变服务端状态的操作**的语义：默认库初始化、ingest 提交与 job、ContextPack 的发布与加载、SOP 候选与批准；以及这些操作共用的授权绑定、幂等、费用报告与授权矩阵的结构。
本文只定语义，不定 HTTP 路径、Go/Rust 类型、具体上限与保留时长数值（标 [新设] 的数值是工程值）。**谁可以做什么**是 #996 的业务决定：§2 只给出矩阵的行与列和与决定无关的机制约束，单元格写"待 #996"。

### 0.2 与相邻任务的边界
| 留给 | 内容 | 本文怎么引用 |
|---|---|---|
| READONLY | 凭据与签发权威（§1）、有效范围 E（§2）、修订三元组与活动构建（§5）、错误信封与 `code` 闭集（§6）、generation 只回显（R7.5）、审计最小字段（§10） | 直接引用；本文只新增 `details.reason` 取值（READONLY §0.3 的兼容新增），不新增 `code` |
| STATE 草案 | binding、generation、准入、作废流程、本地结果、run 绑定（S4.7，待决） | 引用；本文不新增 generation 递增事件 |
| BRIDGE 草案 | session JWT 的 `ops` 不含写操作（U1.3）；桥消息闭集与禁止键（U6.2、U7.4） | 引用：KnowledgeView 与桥都不是写操作的入口（W1.6） |
| KB-C4-01 | 版本化配置档、能力预检、格式白名单 | 本文只引用 `config_version` 与"不支持的格式在保存前拒绝"（W5.2） |
| KB-C4-02a | 引导凭据与日常凭据分离、平台 key 不进 Desktop | 本文定引导身份在矩阵里只能做初始化（§2） |
| KB-C4-03 | 激活凭据桥、三问引导 | License 与激活不授予任何写权限（W1.6） |
| KB-SVC-POLICY-01、#994 | 处理目的地、费用上限、预算 | 本文只定 `policy_version` 的绑定与复验时机（W1.3、W5.5）；政策内容不在本文 |
| #997 | 撤权、退出、取消后的原件与产物保留 | 本文只要求如实报告保留了什么（W5.9），删除与否待决（§11 #A5） |
| KB-CTX-02 | 自动注入入口（fresh / 续轮 / 恢复 / 审批继续 / 委派） | 本文定加载时的服务端复验与结果状态（第二片 §7） |

### 0.3 变更规则
沿用 READONLY §0.3。操作类集合、job 状态集合与转移、幂等作用域、批准绑定项、ContextPack 加载的复验项都是语义，改动升主版本（`CONTRACT-OPS-v2.md`）。新增 `reason`、费用 `kind`、失败原因取值算 v1 修订；客户端必须把未知的 job 状态按 `failed` 兜底、未知费用 `kind` 原样展示为"其他费用"[新设]。

## 术语
| 术语 | 定义 |
|---|---|
| 写操作 | 本文覆盖的、会改变服务端状态的操作；集合即 §2 矩阵的行。 |
| 操作类 | §2 矩阵的一行，如 `ingest.submit`。凭据的操作范围与权威授权 A 的操作维度都按操作类表达（W1.1）。 |
| 幂等键 | 客户端为一次逻辑写操作生成的随机标识（`idempotency_key`，至少 128 bit，ULID 或等价格式）；重试同一操作时原样重用（§3）。 |
| 原件 | 用户提交的原始字节（文件或文本）。`content_sha256` 是服务端对原件字节计算的 SHA-256。 |
| job | 服务端为**一个源修订的一次处理尝试**建立的异步记录，带 `job_id`；状态见 §5。重新处理产生新 job，旧 job 不变。 |
| 阶段边界 | job 中下一步会产生外部调用或费用之前的检查点：开始解析、开始 embedding、提交索引并激活构建、每个增强子任务开始前（W1.3）。 |
| `authz_version` | 服务端对"某主体在某资源上的权威授权 A"分配的不透明版本标识；A 的任何变化都使它改变，只比较相等（W1.2）。 |
| `policy_version` | KB-SVC-POLICY-01 处理政策的版本标识，只比较相等。 |
| `config_version` | KB-C4-01 配置档的版本标识。 |
| 费用 | job 已经引起的存储、解析、embedding、模型调用等消耗（W5.10）；本文只定报告形式，不定价格与承担方（#994、#995）。 |

## 1. 授权绑定与机制约束

- **W1.1** [D2/R2.1+新设] **每个写操作都由服务端逐请求重新计算授权**：principal 只取自凭据（R1.5）；有效范围 E 按 R2.1 计算；另外要求该操作类同时在凭据的操作范围内、并且在 A 对目标资源授予的操作里。任一项不满足返回 `forbidden`（`operation_not_allowed`；目标 KB 不在 E 内按 R2.3 返回 `empty_scope`）；文档、job、候选等资源不在 E 内时返回与不存在相同的 `not_found`（R2.6）。READONLY R2.7 现在把 v1 凭据的操作范围限定为 search 与 read，写操作需要 READONLY 同步修订（§11 #C2）。
- **W1.2** [D2+新设] 服务端在每个写操作的响应与审计里回报当次判定使用的 `authz_version`。客户端不得缓存它来跳过判定；它只用于追溯（ContextPack、SOP 批准记下它，W7.x、W8.x）。粒度见 §11 #B1。
- **W1.3** [KB-C4-04/KB-SVC-POLICY-01+新设] **后台作业沿用授权**：job 在提交时记下 `{principal, credential_id, tenant_id, target_kb, authz_version, policy_version, config_version, generation}`（generation 只记录，R7.5）。每个阶段边界复验三项：① principal 当前仍在目标 KB 上持有 `ingest.submit`（按当前 A，不按记下的 `authz_version`）；② 提交所用凭据未被**撤销**（只是过期不算，job 寿命可以长于短期凭据）；③ 当前处理政策仍允许下一步的目的地与费用。① 或 ② 不成立 → `cancelled`（`authorization_revoked`，J07）；③ 不成立 → `failed`（`policy_denied` 或 `budget_exhausted`，J08）。复验内容的取舍见 §11 #B2。
- **W1.4** [S1.5/R8.2+新设] **客户端准入**：Provider 只在 binding 处于可服务状态（S1.1）、且最近一次就绪结果里对应操作（`operations.ingest`）为 `ready` 时发出写操作；否则按 STATE 草案 §6 本地拒绝（`not_ready` 等）。这不改变 STATE 的状态派生（S1.3 不看 ingest）。写请求同样带 `request_id` 与 `generation`（R7.2），按 S4.6 接受。STATE 草案 S1.5 目前只写了 search 与 read，需要它把准入扩展到写操作（§11 #C3）。
- **W1.5** [S4.5/R7.1+新设] **作废与写操作**：S4.5 中止在途写请求时，服务端可能已经提交（J01）。Provider 给调用方的本地 `cancelled` 必须带 `details.outcome: "unknown"` 与该请求的 `idempotency_key`；界面不得据此声称"未保存"。binding 回到可服务状态后，客户端用同一幂等键查询（W5.11）得到真实结果。**不得**在新 generation 下用新幂等键自动重发同一写操作。
- **W1.6** [#996/KB-SOP-02/KB-C4-03/U1.3+新设] **与业务决定无关的机制约束**（矩阵的"机制"列都由此而来）：
  - 授权只来自服务端的权威授权 A 与凭据，**不得**从 retrieve 能力、页面角色、上游 RBAC 角色、组织名输入、License 或激活状态推导（#996 正文、#1004 退出验收、KB-C4-03）。
  - 服务端**不信任宿主转述的批准**：批准只能是批准人用自己的凭据发出的已认证请求，服务端据此生成批准记录（W8.x）；请求里的 `approved: true` 之类字段一律忽略并记审计（KB-SOP-02）。
  - KnowledgeView 不是写操作入口：session JWT 的 `ops` 不含写操作（U1.3）；桥消息类型是闭集，`approval`、`approve`、`decision` 是禁止键（U6.2、U7.4）。
  - 宿主自身没有 principal，不能以自己的名义写；引导身份只用于初始化（KB-C4-02a）。
  - 集成模式下 retrieve 级凭据不得取原件：上游 `/download`、batch-download 对本租户 retrieve key 开放（READONLY 矩阵 #5，`internal/router/routes_knowledge.go:120`）。
- **W1.7** [R10.1/R10.3+新设] 每个写操作与每次 job 状态变化各记一条审计：`knowledge.init`、`knowledge.ingest.submit`、`knowledge.job.transition`、`knowledge.job.cancel`（第二片另有 ContextPack 与 SOP 事件），字段按 R10.1，另加 `idempotency_key` 的哈希、`job_id`、`authz_version`、`policy_version`、费用摘要；不含原件、正文、query 与凭据（R10.3）。后台阶段的审计 principal 是提交时记下的 principal，不是合成用户（R10.4）。
- **W1.8** [R7.7] 取消、撤权、撤回都追不回已经交付给模型或用户的内容，也追不回已经发生的费用；界面与审计不得声称可以。

## 2. 授权矩阵

- **W2.1** [#996+新设] 下表的行是本文的全部操作类，列是可能发起操作的主体类别。"待 #996"表示该格是业务决定，本文不填；"✗（机制）"表示无论 #996 怎么定都不允许，依据见最后一列。
- **W2.2** [#996+新设] **决定之前的 v1 默认**：所有"待 #996"的格按**拒绝**执行（fail closed）；开发与测试只在批准的样本租户上，由测试主体经服务端配置的临时授权进行，不构成产品授权，不得出现在对外构建里（READONLY §0.3 末条）。
- **W2.3** [新设] #996 决定后填格时，每格只能取"允许"、"允许，附条件（写明条件）"或"拒绝"；条件只能引用服务端可验证的事实（A、凭据、资源属性），不能引用客户端自报的角色。
- **W2.4** [新设] 职责分离（例如候选创建人能否批准自己的候选、发布人能否批准自己的 ContextPack）也属于 #996 的决定，写在相应行的条件里（§11 #A4）。

| 操作类 | 本机单用户（local-managed） | remote 个人 | 组织成员 | 组织中被授予特定职责者（职责名与授予方式待 #996） | 组织管理员 | 引导身份（KB-C4-02a） | 页面会话（session JWT） | 模型发起（Tool 调用） | 机制约束（与决定无关） |
|---|---|---|---|---|---|---|---|---|---|
| `init.default_kb` 初始化默认库 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 只能做这一项（机制） | ✗（机制） | 待 #996 | W1.1 服务端重验；结果只绑定**已验证**主体（W4.2）；不覆盖已有资料（W4.4） |
| `ingest.submit` 提交原件 / 新源修订 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制，U1.3） | 待 #996 | W1.1；后台按 W1.3 复验；政策与预算由服务端执行 |
| `job.read` 查询 job 与费用 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 目标 KB 必须在 E 内，否则 `not_found`（R2.6） |
| `job.cancel` 取消 job | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制，U6.2 的 `cancel` 只撤回桥消息） | 待 #996 | 取消后费用如实报告（W5.10） |
| `ingest.reprocess` 显式重新处理 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 会产生新费用：新 job，按当前政策（J12） |
| `document.download_original` 下载原件 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制，R2.7） | 待 #996 | v1 不开放（R2.7）；不得由 retrieve 能力推导（W1.6） |
| `pack.draft` 起草、编辑 ContextPack | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 草稿不被加载（第二片） |
| `pack.publish` 批准并发布 ContextPack | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制，U7.4） | 待 #996 | 批准由批准人自己的已认证请求产生（W1.6）；与 SOP 批准分开（`purpose`） |
| `pack.revoke` 撤销 ContextPack | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 撤销后不再加载，已注入的追不回（W1.8） |
| `pack.load` 被加载进 run | 按受众（第二片） | 按受众 | 按受众 | 按受众 | 按受众 | ✗（机制） | ✗（机制） | 由宿主注入入口发起，不由模型选择 | 每次加载服务端复验（第二片）；受众的取值与谁可设定待 #996 |
| `sop.candidate` 生成、编辑、撤回 SOP 候选 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 只能引用生成时有权读取的来源（第二片）；候选不进可检索正式库 |
| `sop.approve` 批准 SOP | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制，U7.4） | 待 #996 | 不信宿主 `approved` 布尔；批准记录单次有效、绑定 hash 与基础修订（第二片） |
| `sop.commit` 按批准写入正式库 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 服务端提交时重验批准（第二片）；复用 ingest（§5） |
| `sop.withdraw` 撤回已发布 SOP | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 撤回可观察（审计 + 文档状态）；已交付的追不回 |

"模型发起"一列另见 §11 #A3：模型能否发起任何写操作、批准类操作是否必须由人在宿主界面确认，是产品决定。

## 3. 幂等与去重

- **W3.1** [KB-C4-04/KB-C4-02b+新设] 以下写操作**必须**带 `idempotency_key`：`init.default_kb`、`ingest.submit`、`ingest.reprocess`、`pack.publish`、`sop.candidate` 的创建、`sop.approve`、`sop.commit`。缺失返回 `invalid_request`（`idempotency_key_missing`）。`job.cancel`、`pack.revoke`、`sop.withdraw` 天然幂等，不要求。
- **W3.2** [新设] **作用域**：幂等记录的键是 `(tenant_id, principal, 操作类, idempotency_key)`。记录同时保存请求指纹 = H(操作类、目标资源、`content_sha256`、影响结果的参数，含 `expected_policy_version`、`config_version` 与基础修订)。
  - 同键同指纹：返回首次的结果（同一 `job_id` 或资源 ID，附当前状态），`details.replayed: true`，不新建 job、不重复计费；这时仍按 W1.1 重新判定授权，授权已失去时返回 `forbidden` 而不是回放结果。
  - 同键不同指纹：`invalid_request`（`idempotency_key_reused`），不执行。
  - 不同主体用同一个键互不影响（作用域含 principal）。
- **W3.3** [新设] **保留**：幂等记录至少保留到对应 job 进入终态之后 24 小时（数值见 §11 #B3）；过期后同键视为新请求。服务端重启不丢记录（持久化）。
- **W3.4** [新设] **原子性**：保存原件、建立源修订与 job、写幂等记录在同一个事务里完成（或以可恢复的两阶段完成），对外只有"全部完成"与"都未发生"两种结果；并发的同键请求只有一个执行，其余按同键回放；W3.5 的内容去重判定与创建同样原子。上游的去重是先查后插，`knowledges` 表没有唯一约束（§10 第 2 条），并发上传同一文件会产生两行。
- **W3.5** [KB-C4-04+新设] **内容去重**（与幂等键无关、跨请求生效）：同一目标 KB 内，`content_sha256` 与规范化文件类型都相同、且该文档当前源修订就是这份内容时，`ingest.submit` 不新建源修订与 job，返回 200，`details.outcome: "existing"`，附已有文档、当前修订与最近一个 job 的状态；不改动已有记录的任何时间戳。不跨 KB 去重。去重键的组成、命中的已有文档处于 `failed` / `cancelled` 时怎么处理，见 §11 #B4、#B5。上游用 MD5、排除 `failed` 与 `deleting`、命中时返回 409 并改写 `created_at`（§10 第 2 条），都与本条不同。
- **W3.6** [新设] 客户端声明的 `content_sha256` 与服务端计算值不符时返回 `invalid_request`（`content_hash_mismatch`），不保存原件。
- **W3.7** [D2+新设] 幂等记录、指纹与去重查询都不得泄露范围外资源：命中他人或范围外文档时不回报其存在，按无命中处理。

## 4. 初始化（默认库）

- **W4.1** [KB-C4-02b/KB-C4-02a+新设] `init.default_kb`：为一个**归属**建立默认 KB 并绑定授权。归属是已验证的主体（个人）或已验证的组织（组织身份见 KB-ORG-01、#992）。请求带 `idempotency_key` 与客户端已知的 `config_version`；服务端以引导身份完成创建与授权绑定，引导凭据不离开服务端（KB-C4-02a）。
- **W4.2** [KB-C4-02b+新设] **唯一性**：每个归属至多一个默认 KB，唯一键为 `(tenant_id, 归属, "default")`，由服务端约束保证（唯一索引或等价机制），不依赖先查后建。`config_version` 不进唯一键（§11 #B6）：配置档升级不新建默认库。
- **W4.3** [KB-C4-02b+新设] **结果**：`{knowledge_base_id, outcome: "created" | "existing", config_version, authz_version}`。`existing` 且 `config_version` 与请求不同时，附 `details.config_mismatch: true`；是否以及怎样迁移配置不在本文。
- **W4.4** [#1004+新设] **不覆盖**：初始化、重试、重复激活或登录都不得修改已有默认库里的资料、人工修订与配置。
- **W4.5** [KB-C4-02b+新设] **中断与回收**：创建分为"建 KB"与"绑定授权"两步时，对外不可观察到"有 KB 而无授权绑定"的中间态：服务端要么在同一事务里完成，要么把未完成的 KB 标为不可见并在重试时继续完成，超过时限仍未完成的由服务端回收。回收不触及任何已有资料。
- **W4.6** [KB-C1-01b-b/R8.3] 初始化与就绪不互相等待：search 就绪不依赖默认 KB（R8.3）；初始化需要的是存储与配置档可用，就绪里另行表达（`operations.ingest`）。
- **差距**：上游没有默认库概念，auto-setup 只限 Wails 桌面版（READONLY 矩阵 #16）；建 KB 每次生成新 UUID，客户端可在请求体里自带 `id`（handler 绑定整个 KnowledgeBase 结构），重复 `id` 走数据库主键冲突，未见类型化的幂等返回（§10 第 6 条）→ KB-C4-02b。

## 5. ingest 与 job

### 5.1 提交
- **W5.1** [KB-C4-04+新设] `ingest.submit` 的请求：`scope`（只含目标 KB）、`idempotency_key`、原件（文件字节或文本）、`content_sha256`、`file_name` 与规范化文件类型、`expected_policy_version`；更新已有文档时另带 `knowledge_id` 与 `expected_source_revision`（必须等于当前源修订，否则 `stale`，`reason: "base_revision_changed"`，附 `current_revision`）。v1 只收文件与文本；URL 导入会让服务端代为抓取，属出口，不在 v1（§11 #A2）。
- **W5.2** [KB-C4-01/R3.8+新设] **保存前拒绝**：格式不在配置档白名单、或其输出不会产生 v1 范围内的 `text` 分块（R3.8）时，返回 `invalid_request`（`unsupported_format`），不保存原件、不建 job；超过大小上限返回 `invalid_request`（`too_large`）；配额不足返回 `forbidden`（`quota_exceeded`）。上游在算 hash 之前拒绝视频与非法扩展名，FAQ 类 KB 拒绝文件上传，配额检查在去重之后（§10 第 1 条）。
- **W5.3** [新设] **断开不是取消**：J01 提交之后，提交请求的连接断开、客户端超时或 generation 作废都不影响 job；只有 `job.cancel`（或 W1.3 的复验）能取消。这与只读请求不同（READONLY R7.1 的断开即取消只适用于 search 与 read）。
- **W5.4** [R5.1–R5.3+新设] **源修订**：每次被接受的提交（非去重命中）分配一个新的 `source_revision`，在 J01 时成为该文档的**当前源修订**；它的活动构建在 J04 之前为 `null`。这与 READONLY R5.3"活动构建还没建好时 `index_build_id: null`"、R4.4 的 `index_pending` / `index_failed` 一致。于是新修订处理期间，旧修订构建上的 hit 按 R5.3 为 `stale`。
- **W5.5** [KB-SVC-POLICY-01/#994+新设] `expected_policy_version` 与服务端当前政策版本不同时返回 `stale`（`policy_changed`，附当前版本），不保存原件：用户同意上传时看到的处理目的地与费用上限，必须就是 job 执行时依据的政策。

### 5.2 状态
- **W5.6** [KB-C4-04+新设] job 状态是闭集：

| 状态 | 含义 | 原件 | 本修订可检索 | 终态 | `job.cancel` |
|---|---|---|---|---|---|
| `saved` | 原件已持久保存，源修订与 job 已记录，尚未开始处理 | 已保存 | 否 | 否 | 生效（J06） |
| `processing` | 解析、切分、embedding、建索引进行中 | 已保存 | 否（未激活的部分产物不可检索，W5.8） | 否 | 生效，异步完成（J06、W5.9） |
| `indexed` | 本修订的索引构建已激活为活动构建（R5.1） | 已保存 | 是 | 是 | 无效果（J11） |
| `failed` | 处理失败；带 `failure_reason` | 已保存 | 否 | 是 | 无效果（J11） |
| `cancelled` | 被取消；带 `cancel_reason` | 按 W5.9 报告 | 否 | 是 | 无效果（J11） |

  `failure_reason`：`parse_failed`、`embedding_failed`、`index_failed`、`policy_denied`、`budget_exhausted`、`storage_failed`、`enqueue_failed`、`interrupted`、`timeout`、`internal`。`cancel_reason`：`user`、`authorization_revoked`、`superseded`、`document_deleted`。
  **上游映射**：`pending` → `saved`；`processing` → `processing`；`completed` → `indexed`；`failed` → `failed`；`cancelled` → `cancelled`；`deleting` 不是 job 状态（文档删除，J10）；手工知识的 `draft` 不是 job（见第二片）；`finalizing`（主解析完成、增强子任务仍在花费资源，已可检索）的映射见 §11 #B7，v1 草稿按 `indexed` 加 `cost.final: false` 处理。

### 5.3 转移
- **W5.7** [新设] 转移表（同一 job 的转移串行；终态不再转移）：

| ID | 前置 | 事件 | 后置 | 当前源修订 | 依据 |
|---|---|---|---|---|---|
| J01 | — | 提交被接受（W3.4 同一事务） | `saved` | 变为本修订（W5.4） | [KB-C4-04] |
| J02 | — | 提交命中内容去重（W3.5）或同键回放（W3.2） | 不新建 job | 不变 | [新设] |
| J03 | `saved` | 阶段边界复验通过，开始处理 | `processing` | 不变 | [W1.3] |
| J04 | `processing` | 索引构建完成并激活 | `indexed` | 不变；本修订的构建成为活动构建，旧修订的构建不再对当前修订生效（R5.1） | [R5.1] |
| J05 | `saved` / `processing` | 解析或依赖失败且重试用尽、超过处理时限 | `failed` | 不变（文档级 read 得 `index_failed`，R4.4） | [R4.4] |
| J06 | `saved` / `processing` | `job.cancel` | `cancelled`（`user`） | 回退到上一个源修订（若有），它的活动构建恢复为当前（§11 #B8） | [KB-C4-04+新设] |
| J07 | `saved` / `processing` | 阶段边界复验：授权被撤（W1.3 ①②） | `cancelled`（`authorization_revoked`） | 同 J06 | [W1.3] |
| J08 | `saved` / `processing` | 阶段边界复验：政策不允许或预算耗尽（W1.3 ③） | `failed`（`policy_denied` / `budget_exhausted`） | 不变 | [KB-SVC-POLICY-01] |
| J09 | `saved` / `processing` | 同一文档的新源修订被接受（J01） | `cancelled`（`superseded`） | 变为更新的修订 | [新设] |
| J10 | `saved` / `processing` | 文档被删除 | `cancelled`（`document_deleted`） | 文档不存在 | [新设] |
| J11 | `indexed` / `failed` / `cancelled` | `job.cancel` | 不变；返回 200，`details.cancel_effect: "none"` | 不变 | [新设] |
| J12 | `indexed` / `failed` / `cancelled` | `ingest.reprocess`（显式） | 原 job 不变；为该源修订新建 job（`saved`） | 该修订成为当前（`cancelled` 的修订由此恢复） | [新设] |
| J13 | `processing` | 服务端重启或工作进程中断 | 恢复后仍 `processing`；超过处理时限 → `failed`（`interrupted`） | 不变 | [新设] |
| J14 | `saved` / `processing` | 提交所用凭据过期（未撤销） | 不变 | 不变 | [W1.3] |

  `indexed` 之前，旧修订的活动构建必须保留，直到新构建激活（J04）；J05–J10 之后它仍可按 R5.3 作为 `stale` 线索或（J06、J07 回退后）重新作为当前构建。上游 reparse 在重建前先清理旧分块（READONLY R4.4 引用的 `internal/application/service/knowledge_process.go:423`），与本条不符 → KB-C4-04。
- **W5.8** [R5.3+新设] `processing`、`failed`、`cancelled` 的 job 已写下的部分分块与索引**不得**被检索或读取到，也不得成为活动构建。上游取消时保留已写入的分块与索引（§10 第 3 条），检索过滤只看分块的 `is_enabled`、`index_status` 与类型，不看文档的解析状态（§10 第 4 条）→ KB-C4-04。

### 5.4 取消与费用
- **W5.9** [KB-C4-04+新设] **取消**：`job.cancel` 同步把状态记为 `cancelled` 并返回，随后在每个阶段边界停止；已经发出的外部调用可能仍会完成并计费。响应与之后的 `job.read` 如实报告：`original_retained`（原件是否仍保存）、`partial_artifacts`（`discarded` / `retained_invisible`）、`cost`（W5.10）。取消不删除原件；删除与保留要求待决（§11 #A5）。Lite 模式没有队列出队，只能靠检查点停止（§10 第 3 条），同样适用本条。
- **W5.10** [KB-C4-04/KB-SVC-POLICY-01+新设] **费用报告**：每个 job 带 `cost: { final, items: [{ kind, destination, units, unit_type, measured }] }`。
  - `kind`：`storage`、`parse`、`embedding`、`multimodal`、`llm_enrichment`（摘要、问题生成、图谱、wiki 等）；`destination` 取处理政策里的目的地标识；`unit_type`：`bytes`、`pages`、`tokens`、`calls`、`images`；`measured`：`exact`、`estimated`、`unknown`。
  - **如实**：不知道就填 `units: null` 加 `unknown`，**不得填 0**；估算必须标 `estimated`。`final: true` 只在不再有任何工作可能产生费用之后（取消后所有在途调用都已结束、增强子任务都已终止）。界面只有在 `final: true` 且全部项为精确 0 时，才能说"没有产生费用"。
  - 金额、币种与承担方不在本文（#994、#995）；服务端若给出金额，放在可选的 `amount` 里并标明依据的价格表版本。
  - 上游没有按文档持久化的计量，embedding 用量只在 Langfuse 追踪里按 rune 数 / 4 估算（§10 第 5 条）→ KB-C4-04、KB-SVC-POLICY-01。
- **W5.11** [新设] **查询**：`job.read` 可按 `job_id`，或按 `(操作类, idempotency_key)` 查询（作用域同 W3.2，只能查自己的键）；返回状态、`knowledge_id`、`source_revision`、`index_build_id`（`indexed` 时）、`attempt`、`failure_reason` / `cancel_reason`、`cost`、各状态时间戳。目标 KB 不在 E 内时 `not_found`（R2.6）。
- **W5.12** [D4] job 状态与费用只是元数据，不是知识证据，不进入模型上下文作为引文；Tool 层展示时按普通状态处理。

## 6. 验收：授权、幂等、初始化与 job

除注明外 profile 为 remote，binding 处于 `ready`，测试主体在样本租户上持有全部操作类的临时授权（W2.2）。KB-C4-02b、KB-C4-04、KB-C1-04、KB-HOST-01 按本表写测试。

| ID | 规则 | 构造 | 期望 |
|---|---|---|---|
| MT-01 | W2.2 | 非样本租户的任意主体调用任一写操作 | `forbidden`（`operation_not_allowed`）；不保存、不建 job |
| MT-02 | W1.6、U1.3 | 用 session JWT 调 `ingest.submit`、`job.cancel`、`sop.approve` | 都是 `unauthorized/credential_invalid`（受众不符） |
| MT-03 | W1.6 | 请求体带 `approved: true`、`role: "admin"`、组织名或 License 字段 | 不改变判定结果；字段被忽略并记审计 |
| MT-04 | W1.1、R2.3 | 凭据范围是 kb-a，`ingest.submit` 的目标是 kb-b | `forbidden/empty_scope`；不保存 |
| MT-05 | W1.1、R2.6 | `job.read` / `job.cancel` 指向 E 外 KB 的 job | 与不存在相同的 `not_found` |
| MT-06 | W1.6 | 集成模式下 retrieve 级凭据请求原件下载 | 拒绝（v1 不开放） |
| MT-07 | W1.6 | 引导身份调用 `ingest.submit` | `forbidden/operation_not_allowed` |
| MT-08 | W1.4 | binding `ready`，就绪结果 `operations.ingest` 为 `not_ready` | Provider 本地拒绝 `not_ready`，不发网络请求；STATE 状态不变 |
| MT-09 | W1.7 | 跑完 JT-01–JT-20 后检查审计与日志 | 每次写操作与转移各一条；无原件、正文、query、凭据 |
| IT-01 | W4.2 | 同一归属并发 10 个 `init.default_kb`（不同幂等键） | 恰好一个 `created`，其余 `existing`，同一 `knowledge_base_id` |
| IT-02 | W4.5 | 建 KB 之后、绑定授权之前注入崩溃；之后用同一幂等键重试 | 崩溃期间对外不可见；重试完成绑定，返回同一 KB |
| IT-03 | W4.3 | 已有默认库，用新 `config_version` 再初始化 | `existing`，`config_mismatch: true`；库内资料与配置不变 |
| IT-04 | W4.4 | 默认库已有资料与人工修订的分块；重复激活与登录 | 资料、修订与配置都不变 |
| IT-05 | W4.5 | 未完成的 KB 超过回收时限 | 被回收；其他 KB 与资料不受影响 |
| IT-06 | W4.6 | 默认库不存在时请求就绪 | search 就绪照常判定，不等初始化 |
| JT-01 | J01、W3.4 | 正常提交 | `saved`；原件、源修订、job、幂等记录都已持久；新修订成为当前 |
| JT-02 | J02、W3.2 | 同键同指纹重放三次（含首个响应丢失） | 同一 `job_id`，`replayed: true`；只计费一次 |
| JT-03 | W3.2 | 同键、不同 `content_sha256` | `invalid_request/idempotency_key_reused` |
| JT-04 | W3.4 | 不同键并发提交同一文件到同一 KB | 只有一个新源修订与 job；其余为 `existing` |
| JT-05 | J02、W3.5 | 提交与当前修订相同的内容 | 200，`outcome: "existing"`；不新建 job；时间戳不变 |
| JT-06 | W3.6 | 声明的 `content_sha256` 与实际不符 | `invalid_request/content_hash_mismatch`；不保存 |
| JT-07 | W5.2 | 不支持的格式；超过大小上限；配额不足 | 依次 `unsupported_format`、`too_large`、`forbidden/quota_exceeded`；都不保存 |
| JT-08 | W5.5 | `expected_policy_version` 已过期 | `stale/policy_changed`，附当前版本；不保存 |
| JT-09 | W5.1 | 更新已有文档，`expected_source_revision` 不是当前值 | `stale/base_revision_changed`；不保存 |
| JT-10 | J03、J04 | 正常处理 | `saved` → `processing` → `indexed`；`index_build_id` 成为活动构建；READONLY search 能命中 |
| JT-11 | J05 | 解析失败且重试用尽 | `failed/parse_failed`；文档级 read 得 `index_failed` |
| JT-12 | J06、W5.9 | `processing` 中取消，测试桩让一次 embedding 调用在取消后才返回 | 立即 `cancelled/user`；`cost.final` 先为 false，调用结束后为 true 且含该次调用；部分产物不可检索；原件保留并如实报告 |
| JT-13 | J06 | 文档已有 indexed 的修订 r1，提交 r2 后在 `processing` 中取消 | 当前源修订回到 r1，r1 的构建恢复为当前；search 命中 r1 为 `fresh` |
| JT-14 | J07 | `processing` 中撤销提交人对目标 KB 的 ingest 授权 | 下一个阶段边界 → `cancelled/authorization_revoked`；不再产生新调用 |
| JT-15 | J14 | 提交后凭据自然过期（未撤销） | job 照常完成 |
| JT-16 | J08 | 处理中政策改为禁止 embedding 目的地 | 下一个阶段边界 → `failed/policy_denied`；未授权目的地零调用 |
| JT-17 | J09 | r1 处理中提交 r2 | r1 的 job `cancelled/superseded`；当前为 r2 |
| JT-18 | J11 | 对 `indexed`、`failed`、`cancelled` 的 job 取消 | 200，`cancel_effect: "none"`，状态不变 |
| JT-19 | J12 | 对 `failed` 的 job 显式重新处理 | 新 `job_id`，旧 job 仍 `failed`；新 job 按当前政策计费 |
| JT-20 | J13 | 处理中重启服务端；另测超过处理时限 | 前者恢复后继续；后者 `failed/interrupted`；不会永远停在 `processing` |
| JT-21 | W5.3 | 提交后立即断开连接；另测 STATE 草案 S4.5 中止在途提交 | job 照常处理；后者 Provider 得本地 `cancelled`，带 `outcome: "unknown"` 与幂等键，回到可服务后按键查到真实 job |
| JT-22 | W5.8 | 取消或失败后，用分块 ID 直接 read、用相关 query search | 部分产物都不出现 |
| JT-23 | W5.10 | 测试桩让某目的地无法计量 | 该项 `units: null`、`measured: "unknown"`，不为 0；界面不显示"没有费用" |
| JT-24 | W5.7 | r2 的 job 失败后 search | r1 的构建仍保留；其 hit 为 `stale`，不生成确定引用 |

## 10. 现状证据（WeKnora `4b363d15`；Agent24 见 STATE 草案 §9、BRIDGE 草案 §9）

1. **上传入口**：文件、URL、手工三条入口都要求 `OwnedKBOrAdmin` 与 KB 写访问（`internal/router/routes_knowledge.go:73-75`；"KB 创建人或 Admin+"，`internal/router/rbac.go:445-450`），API key 需 `ingest` 能力且受 KB 白名单约束（`internal/types/tenant_api_key.go:86-91`）；角色只有 owner / admin / contributor / viewer 四级（`internal/types/tenant_member.go:19-32`）。服务层先拒绝视频（`internal/application/service/knowledge_create.go:48-51`）、FAQ 类 KB 拒绝文件（`:62-64`）、校验扩展名（`:74-77`），再算 hash（`:81`）、去重（`:90-117`）、查配额（`:121`），先存文件再建记录（`:174-175` 为 `pending` / `disabled`），入队失败仍返回已保存的知识且不报错（`:241-268`），任务 `MaxRetry(3)`（`:255`）。
2. **去重**：hash 是 MD5（`internal/application/service/knowledge_util.go:107-117`）；查询排除 `failed` 与 `deleting`（`internal/application/repository/knowledge.go:417`），按文件类型区分（`:433`）；命中时改写已有记录的 `created_at`（`knowledge_create.go:112`），handler 返回 409 并带回已有文档（`internal/handler/knowledge.go:208-221`）。`knowledges` 表只有普通索引，没有唯一约束（`migrations/versioned/000000_init.up.sql:122-125`），先查后插可并发重复。
3. **状态与取消**：`parse_status` 有 pending / processing / finalizing / completed / failed / deleting / cancelled（`internal/types/knowledge.go:44-70`），`finalizing` 表示已可做向量检索、但增强子任务仍在花费资源（`:49-56`）；取消"保留已写入的分块与索引"（`:66-70`）。`CancelKnowledgeParse`（`internal/application/service/knowledge_process.go:2914-3014`）：对 completed / failed 返回 400（`:2951-2952`），对 cancelled 幂等（`:2946-2950`），同步改状态并清零子任务计数（`:2970-2976`），出队是尽力而为、Lite 模式（无 Redis）只能靠检查点（`:2924-2926`、`:3001`）；没有费用报告。reparse 先把状态重置为 pending（`:2902-2911`）。
4. **检索不看文档状态**：`isSearchableChunk` 只看分块的 `is_enabled`、`index_status` 与类型（`internal/application/service/knowledgebase_search_results.go:361-378`）。
5. **计量**：embedding 用量只为 Langfuse 估算（rune 数 / 4，`internal/models/embedding/langfuse_wrapper.go:85-89`），`Knowledge` 结构没有费用或用量字段（`internal/types/knowledge.go:180-200` 一带）。
6. **建 KB**：handler 把请求体整体绑定为 `KnowledgeBase`（含 `id`，`internal/handler/knowledgebase.go:401-402`），服务层只在 `id` 为空时生成 UUID（`internal/application/service/knowledgebase.go:125-127`）；API key 建的 KB 没有创建人（`:130-140`）；JWT 需 Contributor+，API key 需 `manage_kbs`（`internal/router/routes_knowledge.go:204`）。
7. **后台作业的身份**：任务载荷只有 `tenant_id`、知识与 KB ID 等，没有 principal（`internal/types/task.go:300-323`）；`ProcessDocument` 只按租户建执行上下文（`knowledge_process.go:3415`），不复验提交人的授权。
8. 上游没有 ContextPack、SOP 或内容批准的概念；仓库里唯一的"批准"是 MCP 工具审批（`internal/types/chat.go:264-267`、`internal/types/mcp.go:135`）。

## 11. 待决与需要评审确认的点

分类：**A** 对外行为或产品范围，留给 David（含一切依赖 #996 的）；**B** 契约语义，可以盲选；**C** 内核接口与外部依赖，只登记。问题清单与选项见 `knowledge-materials/kb-ct-03c/questions.md`。

| # | 事项 | v1 默认 | 状态 |
|---|---|---|---|
| A1 | **§2 矩阵每一格**（含个人档是否沿用组织矩阵、职责的名称与授予方式） | 全部拒绝，只在样本租户测试（W2.2） | 待 #996（第一片） |
| A2 | **v1 是否支持 URL 导入**（服务端代为抓取，是出口） | 不支持（W5.1） | 待 David；#994（第一片） |
| A3 | **模型（Tool 调用）能否发起写操作；批准类操作是否必须由人在宿主界面确认** | 都不开放（§2 模型列"待 #996"，按 W2.2 拒绝） | 待 David / #996（第一片） |
| A4 | **职责分离**：创建人能否批准自己的候选、发布人能否批准自己的 ContextPack | 写进矩阵条件（W2.4） | 待 #996（第一片） |
| A5 | **取消、失败、撤权后原件与部分产物是否删除** | 都不删，如实报告（W5.9） | 待决；#997（第一片） |
| B0 | 所有标 `[新设]` 的规则 | 按本文 | 本 PR 评审 |
| B1 | `authz_version` 的粒度 | 每个"主体 × 资源"一个值（W1.2） | 本 PR 评审（第一片） |
| B2 | 后台阶段复验的内容 | A + 凭据未撤销 + 政策（W1.3） | 本 PR 评审（第一片） |
| B3 | 幂等记录的保留时长 | 终态后 24 小时（W3.3） | 本 PR 评审（第一片） |
| B4 | 内容去重键 | KB + SHA-256 + 规范化文件类型（W3.5） | 本 PR 评审（第一片） |
| B5 | 去重命中的文档处于 `failed` / `cancelled` 时 | 返回 `existing`，不自动重新处理（W3.5） | 本 PR 评审（第一片） |
| B6 | `config_version` 是否进默认库唯一键 | 不进（W4.2） | 本 PR 评审（第一片） |
| B7 | 上游 `finalizing` 的映射 | `indexed` 加 `cost.final: false`（W5.6） | 本 PR 评审（第一片） |
| B8 | 取消、失败后当前源修订是否回退 | 取消回退，失败不回退（J05、J06） | 本 PR 评审（第一片） |
| B9 | 对终态 job 取消的响应 | 200，`cancel_effect: "none"`（J11） | 本 PR 评审（第一片） |
| C1 | 服务端为写操作新增的端点、计量与幂等存储（KB-C4-04、KB-C4-02b） | 按本文语义实现 | WeKnora 实现 |
| C2 | **READONLY R2.7 需同步修订**：凭据的操作范围按操作类扩展到写操作；与 BRIDGE 草案 §10 #25 一并处理 | 修订前写操作不实现 | READONLY 下一修订（第一片） |
| C3 | **依赖 STATE 草案**：S1.1、S1.3、S1.5（需扩展到写操作，W1.4）、S4.5、S4.6（W1.5）、S4.7（run 绑定，待决 #7）、§6 本地结果、S5.8 | 按 STATE 草案 r5 | STATE 定稿时逐条复核（第一片） |
| C4 | **依赖 BRIDGE 草案**：U1.3（session JWT 不含写操作）、U6.2（桥消息闭集）、U7.4（`approval` 等禁止键）；BRIDGE §10 #25 与本文 #C2 一并处理 | 按 BRIDGE 草案 r5 | BRIDGE 定稿时逐条复核（第一片） |
| C5 | **RunCreate 缺 principal、profile 字段**（STATE 草案 §9 第 4 条）；写操作的 run 归属与审计需要它们。**涉及内核接口，需知会 jason** | 不定实现 | KB-HOST-01（第一片） |

## 变更记录
- r0 第一片（2026-10-17）：首稿。§0–§6：授权绑定与机制约束、授权矩阵结构（单元格待 #996）、幂等与去重、初始化、ingest 与 job（状态、转移 J01–J14、取消与费用报告）、验收 MT / IT / JT；§10 WeKnora 现状证据；§11 第一片条目。影响夹具：否（#1012 只覆盖只读）。
