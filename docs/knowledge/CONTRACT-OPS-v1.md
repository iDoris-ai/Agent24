# Knowledge 初始化 / ingest / job / ContextPack / SOP 语义契约 v1（CONTRACT-OPS-v1）

> **状态**：**草案（Draft）**，待 fork 侧维护者（David）评审（文档 PR，base `ab/weknora`）。评审通过前不授权任何实现。
> **版本**：契约 v1，文档修订 r3 · **日期**：2026-10-17 · **作者**：David Xu
> **Issue**：#1013 ③（KB-CT-03），阶段 #1000；消费方 KB-C4-02b、KB-C4-04、KB-CTX-01、KB-CTX-02、KB-SOP-01、KB-SOP-02，另供 KB-C4-03、KB-C4-05、KB-SVC-POLICY-01 引用。
> **依据**：任务方案 v2.1（方案全文见 #986 第一条评论）；`docs/knowledge/CONTRACT-READONLY-v1.md`（下称 READONLY，按 r7）；`docs/knowledge/CONTRACT-STATE-v1.md`（下称 STATE 草案，按 r5）与 `docs/knowledge/CONTRACT-UI-BRIDGE-v1.md`（下称 BRIDGE 草案，按 r5）。**两份草案都尚未合入**：本文只引用它们的条款，不假设其中待决的条目已定；依赖关系登记在 §11 #C3、#C4，草案定稿时逐条复核。
> 来源标签沿用 READONLY：决策 `[D1]`–`[D8]`、任务条目（如 `[KB-C4-04]`）、READONLY 规则（如 `[R2.1]`）、STATE 草案规则（如 `[S4.5]`）、BRIDGE 草案规则（如 `[U1.3]`）、决策 issue（如 `[#996]`）。`[新设]` 表示本文新设计、在以上来源里都找不到依据的规则；与其他标签并列时，表示在该依据之上新增的部分。所有 `[新设]` 都待本 PR 评审（§11 #B0）。
> **分片**：分三片提交，一起合入，后一片在前一片之上纯追加。第一片：§0–§4（授权绑定、判定顺序、授权矩阵、幂等、初始化及其验收）、§10 第 1–8 条、§11 中标"第一片"的条目；第二片：§5–§6（ingest 与 job 及其验收）与 §11 中标"第二片"的条目；第三片：§7–§9（ContextPack、SOP 候选与批准及其验收）、§10 第 9–10 条与 §11 其余条目。单独阅读前面的片时，正文引用的后面各节在后续片里。
> **现状证据**：WeKnora 的 `file:line` 都按 READONLY 固定的受测 SHA `4b363d15`（`a24/develop` 之后的提交只改文档，Go 源码不变），路径相对 WeKnora 仓库根目录；Agent24 的现状沿用 STATE 草案 §9、BRIDGE 草案 §9（`ab/weknora` 的 `eb1a58b`）。汇总在 §10。

## 0. 范围、边界与变更规则

### 0.1 本文冻结什么
Agent24（Provider 与宿主，下称"客户端"）与 WeKnora fork（下称"服务端"）之间**会改变服务端状态的操作**的语义：默认库初始化、ingest 提交与 job、ContextPack 的批准、发布与加载、SOP 候选与批准；以及这些操作共用的授权绑定、判定顺序、幂等、费用报告与授权矩阵的结构。
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
| KB-CTX-02 | 自动注入入口（fresh / 续轮 / 恢复 / 审批继续 / 委派） | 本文定加载时的服务端复验与结果状态（§7） |

### 0.3 变更规则
沿用 READONLY §0.3。操作类集合、判定顺序、job 状态集合与转移、幂等作用域、批准绑定项、ContextPack 加载的复验项都是语义，改动升主版本（`CONTRACT-OPS-v2.md`）。新增 `reason`、`cancel_effect`、费用 `kind`、失败原因取值算 v1 修订；客户端必须把未知的 job 状态按 `failed` 兜底、未知 `cancel_effect` 按 `none` 兜底、未知 `enrichment` 按 `running` 兜底（费用按未结束处理）、未知 `stop_reason` 按"已停止，原因未知"展示、未知费用 `kind` 原样展示为"其他费用"[新设]。

## 术语
| 术语 | 定义 |
|---|---|
| 写操作 | 本文覆盖的、会改变服务端状态的操作；集合即 §2 矩阵的行。`job.read`、`pack.read`、`sop.approval.read` 不改变状态，一并列入以便表达授权；`pack.load` 除了把过期或失效的包记为 `expired` / `invalidated`（P04、P05）之外不改变状态。 |
| 操作类 | §2 矩阵的一行，如 `ingest.submit`。凭据的操作范围与权威授权 A 的操作维度都按操作类表达（W1.1）。 |
| 新建工作的操作 | 会让服务端开始新的处理并产生费用的操作：`init.default_kb`、`ingest.submit`、`ingest.reprocess`、`sop.commit`（W1.4）。 |
| 幂等键 | 客户端为一次逻辑写操作生成的随机标识（`idempotency_key`，至少 128 bit，ULID 或等价格式）；重试同一操作时原样重用（§3）。 |
| 原件 | 用户提交的原始字节（文件或文本）。`content_sha256` 是服务端对原件字节计算的 SHA-256。 |
| job | 服务端为**一个源修订的一次处理尝试**建立的异步记录，带 `job_id` 与建立它的操作类；状态见 §5。重新处理产生新 job，旧 job 不变。 |
| 阶段边界 | job 中下一步会产生外部调用或费用之前的检查点：开始解析、开始 embedding、提交索引并激活构建、每个增强子任务开始前（W1.3）。 |
| 增强子任务 | 主索引之外、会花费模型调用的后续处理（摘要、问题生成、图谱、wiki 等）；对应上游 `finalizing`（§10 第 3 条）。 |
| `authz_version` | 服务端对"某主体在某资源上的权威授权 A"分配的不透明版本标识；A 的任何变化都使它改变，只比较相等（W1.2）。 |
| `policy_version` | KB-SVC-POLICY-01 处理政策的版本标识，只比较相等。 |
| `config_version` | KB-C4-01 配置档的版本标识。 |
| 归属 | 默认库属于谁：调用者本人，或调用者所在的已验证组织（W4.1）。 |
| 费用 | job 已经引起的存储、解析、embedding、模型调用等消耗（W5.10）；本文只定报告形式，不定价格与承担方（#994、#995）。 |

## 1. 授权绑定与机制约束

- **W1.1** [D2/R2.1+新设] **每个写操作都由服务端逐请求重新计算授权**：principal 只取自凭据（R1.5）；有效范围 E 按 R2.1 计算；另外要求该操作类同时在凭据的操作范围内、并且在 A 对目标资源授予的操作里。任一项不满足返回 `forbidden`（`operation_not_allowed`；目标 KB 不在 E 内按 R2.3 返回 `empty_scope`）；文档、job、批准记录等资源不在 E 内或不属于调用者时，返回与不存在相同的 `not_found`（R2.6）。READONLY R2.7 现在把 v1 凭据的操作范围限定为 search 与 read，写操作需要 READONLY 同步修订（§11 #C2）。
- **W1.2** [D2+新设] 服务端在每个写操作的响应与审计里回报当次判定使用的 `authz_version`。客户端不得缓存它来跳过判定；它只用于追溯（ContextPack、SOP 批准记下它，W7.4、W8.4）。**粒度**：每个"主体 × 资源"一个值，资源包括 KB，也包括组织级的授予（如 `pack.publish`、`pack.approve`）；服务端可以保守地让它多变，用主体级或租户级的值也合规：多变只会让依赖它的记录更早失效或多复验一次，不会放宽授权（§11 #B1）。
- **W1.3** [KB-C4-04/KB-SVC-POLICY-01+新设] **后台作业沿用授权**：job 在建立时记下 `{操作类, principal, credential_id, tenant_id, target_kb, authz_version, policy_version, config_version, generation}`（generation 只记录，R7.5）。每个阶段边界（含每个增强子任务开始前）复验三项：① principal 当前仍在目标 KB 上持有**建立该 job 的操作类**（`ingest.submit`、`ingest.reprocess` 或 `sop.commit`；按当前 A，不按记下的 `authz_version`）；② 建立 job 所用凭据未被**撤销**（只是过期不算，job 寿命可以长于短期凭据）；③ 当前处理政策仍允许下一步的目的地与费用。主索引阶段 ① 或 ② 不成立 → `cancelled`（`authorization_revoked`，J07）；③ 不成立 → `failed`（`policy_denied` 或 `budget_exhausted`，J08）；增强阶段不成立时按 J16 只停增强。为使 ② 可判定，服务端的凭据撤销记录至少保留到引用该凭据的所有 job 进入终态且费用 `final`，凭据过期后也不得提前删除（§11 #B2）。
- **W1.4** [S1.5/R8.2+新设] **客户端准入**：Provider 只在 binding 处于可服务状态（S1.1，含 `degraded`）时发出写操作；**新建工作的操作**另要求最近一次就绪结果的 `operations.ingest` 为 `ready`。`job.read`、`job.cancel`、`pack.*`、`sop.approve`、`sop.approval.read`、`sop.withdraw`、`document.delete`、`chunk.edit` 只要求可服务，不看 ingest 就绪（降级时仍能取消与撤回）。不满足按 STATE 草案 §6 本地拒绝（`not_ready` 等）。这不改变 STATE 的状态派生（S1.3 不看 ingest）。写请求同样带 `request_id` 与 `generation`（R7.2），按 S4.6 接受。STATE 草案 S1.5 目前只写了 search 与 read，需要它把准入扩展到写操作（§11 #C3）。
- **W1.5** [S4.5/R7.1+新设] **作废与写操作**：S4.5 中止在途写请求时，服务端可能已经执行。Provider 给调用方的本地 `cancelled` 必须带 `details.write_outcome: "unknown"`，有幂等键的再带上该键（`job.cancel` 等天然幂等的操作直接重发即可）；界面不得据此声称"未保存"。**待决的写操作**（操作类、请求体、幂等键）由宿主持久保存，直到得到确定结果；重启之后、Tool 层的重试都**复用同一键与同一请求体**：已执行的得到回放（W3.2），未执行的正常执行；产生 job 的操作也可以按键查询（W5.11）。**不得**用新幂等键自动重发同一写操作。调用者 principal 变化后旧键不可用（W3.2），宿主保持 `write_outcome: "unknown"`，告知用户"该操作结果未知"，不自动重发。
- **W1.6** [#996/KB-SOP-02/KB-C4-03/U1.3+新设] **与业务决定无关的机制约束**（矩阵的"机制"列都由此而来）：
  - 授权只来自服务端的权威授权 A 与凭据，**不得**从 retrieve 能力、页面角色、上游 RBAC 角色、组织名输入、License、激活状态或请求体里的字段（如归属、创建人）推导（#996 正文、#1004 退出验收、KB-C4-03）。
  - 服务端**不信任宿主转述的批准**：批准只能是批准人用自己的凭据发出的已认证请求，服务端据此生成批准记录（W8.4）；请求里的 `approved: true` 之类字段一律忽略并记审计（KB-SOP-02）。
  - KnowledgeView 不是写操作入口：session JWT 的 `ops` 不含写操作（U1.3）；桥消息类型是闭集，`approval`、`approve`、`decision` 是禁止键（U6.2、U7.4）。
  - 宿主自身没有 principal，不能以自己的名义写；引导身份只用于初始化，且只代表调用者（W4.1，KB-C4-02a）。
  - 集成模式下 retrieve 级凭据不得取原件：上游 `/download`、batch-download 对本租户 retrieve key 开放（READONLY 矩阵 #5，`internal/router/routes_knowledge.go:120`、`:78`）。
- **W1.7** [R10.1/R10.3+新设] 每个写操作与每次 job 状态变化各记一条审计：`knowledge.init`、`knowledge.ingest.submit`、`knowledge.job.transition`、`knowledge.job.cancel`，以及 §7、§8 的 ContextPack 与 SOP 事件；字段按 R10.1，另加 `idempotency_key` 的哈希、`job_id`、`authz_version`、`policy_version`、费用摘要；不含原件、正文、query 与凭据（R10.3）。后台阶段的审计 principal 是建立 job 时记下的 principal，不是合成用户（R10.4）。
- **W1.8** [R7.7] 取消、撤权、撤回都追不回已经交付给模型或用户的内容，也追不回已经发生的费用；界面与审计不得声称可以。
- **W1.9** [R6.4+新设] **写操作的判定顺序**（服务端，取第一个不成立的）：① 凭据（`unauthorized`）→ ② 请求形状（`invalid_request`：缺字段、缺幂等键、`content_sha256` 不符、带 `knowledge_id` 而缺 `expected_source_revision` 等）→ ③ 授权（W1.1；`sop.commit` 的提交人权限也在这一步）→ ④ 幂等回放（W3.2）→ ⑤ 其余：`ingest.submit` 依次为政策版本（W5.5）、格式与大小（W5.2）、配额、目标文档存在、未撤回与 `origin` 限制（W5.1）、基础修订（W3.4）、内容去重（W3.5）；`ingest.reprocess` 依次为政策版本、文档存在且未撤回、修订为当前、已有占用 job（W5.13）；`document.delete`、`chunk.edit` 依次为文档存在与 `origin` 限制（W5.14）；`sop.commit` 见 W8.6；`pack.publish` 见 W7.15 → ⑥ 执行。回放先于 ⑤，所以首个请求已执行后，即使政策变了或批准已用，原样重试仍得到首次结果；回放仍先过 ③，失去授权的调用者拿不到回放。被 ①–⑤ 拒绝的请求不写幂等记录，唯一例外见 W3.2。
- **W1.10** [S5.3/R7.7+新设] **Off、切换、注销与在途 job**（v1 默认，§11 #A13）：Off、切换 profile 与注销都**不取消**已建立的 job，服务端照常处理并计费（W5.3）。注销只销毁宿主的本地凭据，不等于 W1.3 ② 的服务端撤销，job 不因此停止。宿主在执行这三个动作之前，若该 binding 有它发起、**未终态或 `cost.final` 为 false** 的 job，必须提示用户"有 n 个处理仍在进行、会继续产生费用"，并提供先取消的入口；动作之后宿主不能再取消（binding 不可服务），只能等回到可服务后处理。

## 2. 授权矩阵

- **W2.1** [#996+新设] 下表的行是本文的全部操作类，列是可能发起操作的主体类别。"待 #996"表示该格是业务决定，本文不填；"✗（机制）"表示无论 #996 怎么定都不允许，依据见最后一列。
- **W2.2** [#996+新设] **决定之前的 v1 默认**：所有"待 #996"的格按**拒绝**执行（fail closed）；开发与测试只在批准的样本租户上，由测试主体经服务端配置的临时授权进行，不构成产品授权，不得出现在对外构建里（READONLY §0.3 末条）。
- **W2.3** [新设] #996 决定后填格时，每格只能取"允许"、"允许，附条件（写明条件）"或"拒绝"；条件只能引用服务端可验证的事实（A、凭据、资源属性），不能引用客户端自报的角色。
- **W2.4** [新设] 职责分离（例如候选创建人能否批准自己的候选、发布人能否批准自己的 ContextPack）也属于 #996 的决定，写在相应行的条件里（§11 #A4）。**v1 的服务端只能执行它看得见的事实**：ContextPack 的起草人与批准人都在服务端留有记录，可以执行；SOP 候选只在宿主本地（W8.2），服务端不知道创建人，"创建人不得自批"在服务端保存候选或创建人以自己凭据登记候选（§11 #A12）之前，**不能作为服务端保证**，只能由宿主执行。

| 操作类 | 本机单用户（local-managed） | remote 个人 | 组织成员 | 组织中被授予特定职责者（职责名与授予方式待 #996） | 组织管理员 | 引导身份（KB-C4-02a） | 页面会话（session JWT） | 模型发起（Tool 调用） | 机制约束（与决定无关） |
|---|---|---|---|---|---|---|---|---|---|
| `init.default_kb` 初始化默认库 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 只能做这一项，且只代表调用者（机制） | ✗（机制） | 待 #996 | W1.1 服务端重验；归属取自调用者凭据与服务端成员资格（W4.1）；`existing` 不改授权（W4.3） |
| `ingest.submit` 提交原件 / 新源修订 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制，U1.3） | 待 #996 | W1.1；后台按 W1.3 复验；`origin: "sop"` 的文档不能经此修订（W5.1） |
| `job.read` 查询 job 与费用 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 目标 KB 必须在 E 内，否则 `not_found`（R2.6） |
| `job.cancel` 取消 job 或其剩余增强 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制，U6.2 的 `cancel` 只撤回桥消息） | 待 #996 | 不要求 ingest 就绪（W1.4）；费用如实报告（W5.10） |
| `ingest.reprocess` 显式重新处理 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 新费用：新 job，按当前政策；每个修订至多一个非终态 job（W5.13） |
| `document.download_original` 下载原件 | ✗（v1，R2.7）；之后待 #996 | 同左 | 同左 | 同左 | 同左 | ✗（机制） | ✗（机制，R2.7） | ✗（v1） | 不得由 retrieve 能力推导（W1.6） |
| `pack.draft` 起草、编辑 ContextPack | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 草稿不被加载（§7） |
| `pack.read` 读取 ContextPack 草稿与版本内容 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 草稿只对起草人、以及能读取全部来源（R4.6）且持有 `pack.approve` 的主体可读（W7.3）；其余 `not_found` |
| `pack.approve` 批准 ContextPack 版本 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制，U7.4） | 待 #996 | 批准人须能读取全部来源（R4.6），否则 `forbidden`；产生 `purpose: "pack.publish"` 的批准记录，由发布人出示；与 `sop.approve` 是不同授予（W7.15） |
| `pack.publish` 用批准发布 ContextPack | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 批准与发布是两步（W7.15）；每个 `pack_id` 至多一个 `published`（W7.16） |
| `pack.revoke` 撤销 ContextPack | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 撤销后不再加载，已注入的追不回（W1.8） |
| `pack.load` 被加载进 run | 按受众（§7） | 按受众 | 按受众 | 按受众 | 按受众 | ✗（机制） | ✗（机制） | 由宿主注入入口发起，不由模型选择 | 每次加载服务端复验（W7.9）；受众的取值与谁可设定待 #996 |
| `sop.candidate` 生成、编辑、撤回 SOP 候选 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 宿主本地操作；作废批准的通知需服务端授权（W8.3） |
| `sop.approve` 批准 SOP | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制，U7.4） | 待 #996 | 不信宿主 `approved` 布尔；批准记录单次有效、绑定 hash 与基础修订（W8.4） |
| `sop.approval.read` 查询批准状态、读回批准正文 | 仅批准人本人（机制） | 同左 | 同左 | 同左 | 同左 | ✗（机制） | ✗（机制） | ✗（机制） | 他人的批准记录按 R2.6 `not_found`（W8.2） |
| `sop.commit` 按批准写入正式库 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 服务端提交时重验批准（W8.6）；复用 ingest（§5） |
| `sop.withdraw` 撤回已发布 SOP | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | 撤回可观察（审计 + 文档状态）；已交付的追不回 |
| `document.delete` 删除文档 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | `origin: "sop"` 的文档按机制拒绝，只能 `sop.withdraw`（W5.14）；触发 J10、J17 |
| `chunk.edit` 编辑、回滚、启用 / 禁用分块，增删生成问题 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 待 #996 | `origin: "sop"` 的文档按机制拒绝（W5.14）；会改变 `chunk_revision`，使引用它的 ContextPack 失效（W7.9 ③） |

"模型发起"一列另见 §11 #A3：模型能否发起任何写操作、批准类操作是否必须由人在宿主界面确认，是产品决定。

## 3. 幂等与去重

- **W3.1** [KB-C4-04/KB-C4-02b+新设] 以下写操作**必须**带 `idempotency_key`：`init.default_kb`、`ingest.submit`、`ingest.reprocess`、`pack.draft`、`pack.approve`、`pack.publish`、`sop.approve`、`sop.commit`。缺失返回 `invalid_request`（`idempotency_key_missing`）。**`sop.commit` 的 `idempotency_key` 必须等于所用的 `approval_id`**，不等返回 `invalid_request`（`idempotency_key_mismatch`）。`job.cancel`、`pack.revoke`、`sop.withdraw`、`document.delete` 与作废批准的通知天然幂等，不要求；`chunk.edit` 沿用上游的编辑语义，本文只定授权与 `origin` 限制（W5.14）。
- **W3.2** [新设] **作用域**：幂等记录的键是 `(tenant_id, principal, 操作类, idempotency_key)`。记录同时保存请求指纹 = H(操作类、目标资源、`content_sha256`、影响结果的参数，含 `expected_policy_version`、`config_version`、基础修订与 `approval_id`)。只有到达 W1.9 ⑥（或得到去重 / 已有 job 的确定结果）的请求才写记录。
  - 同键同指纹、首个请求已完成：返回首次的结果（同一 `job_id` 或资源 ID，附当前状态），`details.replayed: true`，不新建、不重复计费。
  - 同键同指纹、首个请求仍在执行：返回 `unavailable`（`request_in_progress`，可重试，带 `retry_after_ms`），不并发执行第二次。它是单个请求的结果，Provider 不据此触发 STATE 的就绪探测（需 STATE 草案配合，§11 #C3）。
  - 同键不同指纹：`invalid_request`（`idempotency_key_reused`），不执行。
  - 不同主体用同一个键互不影响（作用域含 principal）；调用者 principal 变化后，旧键对新 principal 不可见（W5.11）。
  - **例外**：`sop.commit` 中会使批准作废的拒绝（W8.6 ③⑥⑦⑧）也写幂等记录，首个响应丢失后的重试得到同一个拒绝，而不是 ① 的 `approval_invalid`。
- **W3.3** [新设] **保留**：产生 job 的幂等记录至少保留到对应 job 进入终态之后 24 小时；不产生 job 的（初始化、ContextPack、批准）至少保留到写入后 24 小时（数值待 David，盲选分歧，§11 #B3）；过期后同键视为新请求。job 与费用记录另行保留，至少 90 天，且不短于对应的幂等记录（§11 #B20）。服务端重启不丢记录（持久化）。
- **W3.4** [新设] **原子性**：保存原件、建立源修订与 job、写幂等记录在同一个事务里完成（或以可恢复的两阶段完成），对外只有"全部完成"与"都未发生"两种结果；并发的同键请求只有一个执行（W3.2）。同一事务里还包括：W3.5 的内容去重判定；`expected_source_revision` 与目标文档当前源修订的 **compare-and-set**（并发提交基于同一修订时只有一个成功，其余 `stale/base_revision_changed`）；`sop.commit` 的批准校验与批准使用（W8.6）。上游的去重是先查后插，`knowledges` 表没有唯一约束（§10 第 2 条），并发上传同一文件会产生两行。
- **W3.5** [KB-C4-04+新设] **内容去重**（与幂等键无关、跨请求生效）：只比对处在**可检索生命周期**内的文档，即未撤回（W8.9）、不在删除中、未删除的文档（与上游排除 `deleting` 一致）。新建文档时，同一目标 KB 内 `content_sha256` 与规范化文件类型都相同、且某文档的当前源修订就是这份内容；修订已有文档时，只与**该文档自己的**当前源修订比较；`sop.commit` 另有限制：只命中本 KB 中 `origin: "sop"`、未撤回、当前修订有活动构建的文档，其他情况照常新建（W8.6 ⑨）。命中时不新建源修订与 job，返回 200，`details.outcome: "existing"`，附已有文档、当前修订与最近一个 job 的状态；写幂等记录；不改动已有记录的任何时间戳；`sop.commit` 命中时批准同样记为已使用。不跨 KB 去重。去重键的组成（§11 #B4）、命中的已有文档处于 `failed` / `cancelled` 时怎么处理（本条默认返回 `existing`、不自动重新处理，§11 #B5）都待 David（盲选分歧），决定前按本条执行。上游用 MD5、排除 `failed` 与 `deleting`、对来源连接器另按来源身份去重、命中时返回 409 并改写 `created_at`（§10 第 2 条），都与本条不同。
- **W3.6** [新设] 客户端声明的 `content_sha256` 与服务端计算值不符时返回 `invalid_request`（`content_hash_mismatch`），不保存原件。
- **W3.7** [D2+新设] 幂等记录、指纹与去重查询都不得泄露范围外资源：命中他人或范围外文档时不回报其存在，按无命中处理。

## 4. 初始化（默认库）

- **W4.1** [KB-C4-02b/KB-C4-02a+新设] `init.default_kb`：为一个**归属**建立默认 KB 并绑定授权。归属只取自调用者的凭据（个人归属即调用者 principal）与服务端记录的组织成员资格（组织归属要求调用者在该组织中已验证、并持有组织归属的 `init.default_kb`；组织身份见 KB-ORG-01、#992）；请求体里自报的归属、创建人一律忽略（W1.6）。请求带 `idempotency_key` 与客户端已知的 `config_version`；服务端以引导身份完成创建与授权绑定，引导身份只代表这次调用者，引导凭据不离开服务端（KB-C4-02a）。
- **W4.2** [KB-C4-02b+新设] **唯一性**：每个归属至多一个默认 KB，唯一键为 `(tenant_id, 归属, "default")`，由服务端约束保证（唯一索引或等价机制），不依赖先查后建。`config_version` 不进唯一键（§11 #B6）：配置档升级不新建默认库。
- **W4.3** [KB-C4-02b+新设] **结果**：`{knowledge_base_id, outcome: "created" | "existing", config_version, authz_version}`。`existing` 时**绝不创建或修改任何授权绑定**；调用者对已有默认库不在 E 内时返回 `forbidden`（`operation_not_allowed`），不回报 `knowledge_base_id`。`existing` 且 `config_version` 与请求不同时，附 `details.config_mismatch: true`；是否以及怎样迁移配置不在本文。
- **W4.4** [#1004+新设] **不覆盖**：初始化、重试、重复激活或登录都不得修改已有默认库里的资料、人工修订与配置。
- **W4.5** [KB-C4-02b+新设] **中断与回收**：创建分为"建 KB"与"绑定授权"两步时，对外不可观察到"有 KB 而无授权绑定"的中间态：服务端要么在同一事务里完成，要么把未完成的 KB 标为不可见，**只由同一归属、同一调用者的重试**继续完成；中间态期间同一归属的其他请求（同一调用者的其他幂等键、同组织的其他成员）得到 `unavailable`（`request_in_progress`，带 `retry_after_ms`），不阻塞等待；超过时限仍未完成的由服务端回收。回收不触及任何已有资料。
- **W4.6** [KB-C1-01b-b/R8.3] 初始化与就绪不互相等待：search 就绪不依赖默认 KB（R8.3）；初始化需要的是存储与配置档可用，就绪里另行表达（`operations.ingest`）。
- **差距**：上游没有默认库概念，auto-setup 只限 Wails 桌面版（READONLY 矩阵 #16）；建 KB 每次生成新 UUID，请求体可自带 `id` 与 `creator_id`，API key 调用时自带的 `creator_id` 会保留下来并影响"KB 创建人或 Admin+"的判定（§10 第 6 条）→ KB-C4-02b。

### 4.1 验收：授权、判定顺序、幂等与初始化

除注明外 profile 为 remote，binding 处于 `ready`，测试主体在样本租户上持有全部操作类的临时授权（W2.2）。KB-C4-02b、KB-C4-04、KB-C1-04、KB-HOST-01 按本表写测试。

| ID | 规则 | 构造 | 期望 |
|---|---|---|---|
| MT-01 | W2.2 | 非样本租户的任意主体调用任一写操作 | `forbidden`（`operation_not_allowed`）；不保存、不建 job |
| MT-02 | W1.6、U1.3 | 用 session JWT 调 `ingest.submit`、`job.cancel`、`sop.approve` | 都是 `unauthorized/credential_invalid`（受众不符） |
| MT-03 | W1.6 | 请求体带 `approved: true`、`role: "admin"`、组织名、License、归属或 `creator_id` 字段 | 不改变判定结果；字段被忽略并记审计 |
| MT-04 | W1.1、R2.3 | 凭据范围是 kb-a，`ingest.submit` 的目标是 kb-b | `forbidden/empty_scope`；不保存 |
| MT-05 | W1.1、R2.6 | `job.read` / `job.cancel` 指向 E 外 KB 的 job | 与不存在相同的 `not_found` |
| MT-06 | W1.6 | 集成模式下 retrieve 级凭据请求原件下载与批量下载 | 拒绝（v1 不开放） |
| MT-07 | W1.6 | 引导身份调用 `ingest.submit` | `forbidden/operation_not_allowed` |
| MT-08 | W1.4 | binding `ready`，`operations.ingest` 为 `not_ready`；分别调 `ingest.submit`、`job.cancel`、`job.read`、`pack.revoke`、`sop.withdraw` | 只有 `ingest.submit` 被本地拒绝 `not_ready`、不发网络请求；其余照常发出；STATE 状态不变 |
| MT-09 | W1.7 | 跑完 §4.1、§6、§9 全部用例后检查审计与日志 | 每次写操作、job 转移、ContextPack 与 SOP 事件各一条；无原件、正文、query、凭据 |
| MT-10 | W1.2 | 主体对 kb-a 的授予被撤销后，再对 kb-b 执行写操作；另测服务端只维护租户级的值 | 前者回报的 kb-a 版本与撤销前不同；后者每次授权变化都改变，依赖它的记录只会更早失效 |
| MT-11 | W1.9 | 首个 `ingest.submit` 已执行，之后政策版本改变，原样重试 | 回放首次结果，不返回 `stale/policy_changed` |
| MT-12 | W1.9 | 失去 `ingest.submit` 授权后用原幂等键重试 | `forbidden`，不回放 |
| MT-13 | W1.9 | 请求同时缺幂等键、目标 KB 越界 | 先报 `invalid_request/idempotency_key_missing` |
| MT-14 | W1.5 | S4.5 中止一个 `pack.approve`；回到可服务后原样重发 | 本地 `cancelled` 带 `write_outcome: "unknown"`；重发得回放或正常执行，只有一条批准记录 |
| MT-15 | W1.10 | binding 有一个 `processing` 的 job、一个 `indexed` 且 `cost.final: false` 的 job；用户依次 Off、切换、注销 | 每个动作前提示"2 个处理仍在进行"并提供取消入口；动作后两者都继续直到工作结束；注销不触发服务端撤销 |
| MT-16 | W3.2 | 同键同指纹并发两个请求，首个仍在执行 | 第二个得 `unavailable/request_in_progress`；只执行一次 |
| MT-17 | W3.2 | 主体 P1、P2 用同一个键提交 | 各自独立执行；P2 查不到 P1 的记录 |
| MT-18 | W3.3 | 记录超过保留期后同键重试；保留期内重启服务端后重试 | 前者按新请求；后者回放 |
| MT-19 | W3.7 | 用范围外文档的内容提交；重放他人的幂等键 | 不回报范围外文档或他人记录的存在 |
| MT-20 | W1.5 | 宿主有一个待决的 `pack.draft`，进程重启后 Tool 层重试；另测 principal 变化 | 前者复用同一键，只有一个草稿版本；后者保持 `write_outcome: "unknown"` 并提示用户，不自动重发 |
| MT-21 | W1.4 | binding `degraded`（`operations.ingest` 为 `ready`）时调 `ingest.submit` 与 `job.cancel` | 都照常发出 |
| IT-01 | W4.2、W4.5 | 同一归属并发 10 个 `init.default_kb`（不同幂等键） | 恰好一个 `created`；其余在中间态期间得 `unavailable/request_in_progress`，重试后得 `existing`，同一 `knowledge_base_id` |
| IT-02 | W4.5 | 建 KB 之后、绑定授权之前注入崩溃；之后用同一幂等键重试；另由同组织其他成员调用 | 崩溃期间对外不可见；同一调用者重试完成绑定；其他成员不能接续 |
| IT-03 | W4.3 | 已有默认库，用新 `config_version` 再初始化 | `existing`，`config_mismatch: true`；库内资料与配置不变 |
| IT-04 | W4.4 | 默认库已有资料与人工修订的分块；重复激活与登录 | 资料、修订与配置都不变 |
| IT-05 | W4.5 | 未完成的 KB 超过回收时限 | 被回收；其他 KB 与资料不受影响 |
| IT-06 | W4.6 | 默认库不存在时请求就绪 | search 就绪照常判定，不等初始化 |
| IT-07 | W4.3 | 已被撤权的组织成员再调用组织归属的初始化 | `forbidden`；不回报 `knowledge_base_id`；授权绑定不变 |
| IT-08 | W4.1 | 请求体自报另一个组织为归属；引导身份调用时未带调用者 | 归属按调用者凭据与成员资格判定；后者拒绝 |
| IT-09 | W4.5 | 中间态期间同组织另一成员调用初始化 | `unavailable/request_in_progress`，带 `retry_after_ms`；不接续、不阻塞 |

## 10. 现状证据（WeKnora `4b363d15`；Agent24 见 STATE 草案 §9、BRIDGE 草案 §9）

1. **上传入口**：文件、URL、手工三条入口都要求 `OwnedKBOrAdmin` 与 KB 写访问（`internal/router/routes_knowledge.go:73-75`；"KB 创建人或 Admin+"，`internal/router/rbac.go:445-450`），API key 需 `ingest` 能力且受 KB 白名单约束（`internal/types/tenant_api_key.go:86-91`）；角色只有 owner / admin / contributor / viewer 四级（`internal/types/tenant_member.go:19-32`）。服务层先拒绝视频（`internal/application/service/knowledge_create.go:48-51`）、FAQ 类 KB 拒绝文件（`:62-64`）、校验扩展名（`:74-77`），再算 hash（`:81`）、去重（`:90-117`）、查配额（`:121`），新记录为 `pending` / `disabled`（`:174-175`），先存文件（`:192`）再建记录（`:201`）；入队失败时把记录标为 `failed`（`:242`、`:260`，`markKnowledgeEnqueueFailed` 定义在 `:1153`），但仍返回已保存的知识且不报错（`:249`、`:267`）；任务 `MaxRetry(3)`（`:255`）。删除文档、编辑与回滚分块、增删生成问题的路由同样对"KB 创建人或 Admin+"开放（`internal/router/routes_knowledge.go:108`、`:40-52`）。
2. **去重**：hash 是 MD5（`internal/application/service/knowledge_util.go:107-117`）；查询排除 `failed` 与 `deleting`（`internal/application/repository/knowledge.go:417`），按文件类型区分（`:433`）；来源连接器的提交另按 `datasource_id` 与 `external_id` 限定去重范围（`knowledge_create.go:97-103`，`repository/knowledge.go:422-425`）；命中时改写已有记录的 `created_at`（`knowledge_create.go:112`），handler 返回 409 并带回已有文档（`internal/handler/knowledge.go:208-221`）。`knowledges` 表只有普通索引，没有唯一约束（`migrations/versioned/000000_init.up.sql:122-125`），先查后插可并发重复。
3. **状态与取消**：`parse_status` 有 pending / processing / finalizing / completed / failed / deleting / cancelled（`internal/types/knowledge.go:44-70`），`finalizing` 表示已可做向量检索、但增强子任务仍在花费资源（`:49-56`）；取消"保留已写入的分块与索引"（`:66-70`）。`CancelKnowledgeParse`（`internal/application/service/knowledge_process.go:2914-3014`）：pending、processing、**finalizing** 都可取消，后者用来停止增强子任务的模型费用（`:2955-2958`），取消后同样记为 `cancelled`（`:2970-2976`），所以上游的 `cancelled` 可能对应一份仍可检索的文档；对 completed / failed 返回 400（`:2951-2952`），对 cancelled 幂等（`:2946-2950`），同步改状态并清零子任务计数（`:2970-2976`），出队是尽力而为、Lite 模式（无 Redis）只能靠检查点（`:2924-2926`、`:3001`）；没有费用报告。reparse 先把状态重置为 pending（`:2902-2911`）。
4. **检索不看文档状态**：`isSearchableChunk` 只看分块的 `is_enabled`、`index_status` 与类型（`internal/application/service/knowledgebase_search_results.go:361-378`）。
5. **计量**：embedding 用量只为 Langfuse 估算（rune 数 / 4，`internal/models/embedding/langfuse_wrapper.go:85-89`），`Knowledge` 结构没有费用或用量字段（`internal/types/knowledge.go:180-200` 一带）。
6. **建 KB**：handler 把请求体整体绑定为 `KnowledgeBase`（`internal/handler/knowledgebase.go:401-402`），其中 `id` 与 `creator_id` 都可由请求体给出（`internal/types/knowledgebase.go:61`、`:77`）；服务层只在 `id` 为空时生成 UUID（`internal/application/service/knowledgebase.go:125-127`），只在调用者是真实用户时用其 ID 覆盖 `creator_id`（`:139-141`），所以 API key 调用时请求体里的 `creator_id` 会保留下来，影响"KB 创建人或 Admin+"的判定；JWT 需 Contributor+，API key 需 `manage_kbs`（`internal/router/routes_knowledge.go:204`）。
7. **后台作业的身份**：任务载荷只有 `tenant_id`、知识与 KB ID 等，没有 principal（`internal/types/task.go:300-323`）；`ProcessDocument` 只按租户建执行上下文（`knowledge_process.go:3415`），不复验提交人的授权。
8. 上游没有 ContextPack、SOP 或内容批准的概念。现有的两种"批准"都与本文无关：MCP 工具审批（`internal/types/chat.go:264-267`、`internal/types/mcp.go:135`），以及组织加入与升级申请的审批（`internal/application/service/organization.go:703` 起的 `ReviewJoinRequest`，按路由级 Admin 守卫放行）。

## 11. 待决与需要评审确认的点

分类：**A** 对外行为或产品范围，留给 David（含一切依赖 #996 的）；**B** 契约语义，已盲选；**C** 内核接口与外部依赖，只登记。问题清单与选项见 `knowledge-materials/kb-ct-03c/questions.md`。

| # | 事项 | v1 默认 | 状态 |
|---|---|---|---|
| A1 | **§2 矩阵每一格**（含个人档是否沿用组织矩阵、职责的名称与授予方式） | 全部拒绝，只在样本租户测试（W2.2） | 待 #996（第一片） |
| A2 | **v1 是否支持 URL 导入**（服务端代为抓取，是出口） | 不支持（W5.1） | 待 David；#994（第一片） |
| A3 | **模型（Tool 调用）能否发起写操作；批准类操作是否必须由人在宿主界面确认** | 都不开放（§2 模型列"待 #996"，按 W2.2 拒绝） | 待 David / #996（第一片） |
| A4 | **职责分离**：创建人能否批准自己的候选、发布人能否就是 ContextPack 的批准人；SOP 批准能否由批准人以外的人代为提交。SOP 侧"创建人不得自批"在服务端保存候选或创建人以自己凭据登记候选（#A12）之前**不能作为服务端保证**，只能由宿主执行；ContextPack 侧服务端可执行 | 写进矩阵条件（W2.4）；`sop.*` 用途的批准只能由批准人本人消费，`pack.publish` 用途的批准由发布人出示（W8.4、W7.15） | 待 #996（第一片） |
| A5 | **取消、失败、撤权后原件与部分产物是否删除**；撤回的 SOP 原件是否删除 | 都不删，如实报告（W5.9、W8.9） | 待决；#997（第一片） |
| A13 | **Off、切换、注销是否应取消在途 job** | 不取消；动作前提示并提供取消入口（W1.10） | 待 David（第一片） |
| B0 | 所有标 `[新设]` 的规则 | 按本文 | 本 PR 评审 |
| B1 | `authz_version` 的粒度 | 每个"主体 × 资源"一个值，资源含组织级授予；可以保守地多变（W1.2） | 已定（盲选一致），含补充（第一片） |
| B2 | 后台阶段复验的内容 | 建立 job 的操作类 + 凭据未撤销 + 政策；撤销记录保留到相关 job 结束（W1.3） | 已定（盲选一致），含补充（第一片） |
| B3 | 幂等记录的保留时长 | 终态（或写入）后 24 小时（W3.3） | 待 David（盲选分歧）（第一片） |
| B4 | 内容去重键 | KB + SHA-256 + 规范化文件类型（W3.5） | 待 David（盲选分歧）（第一片） |
| B5 | 去重命中的文档处于 `failed` / `cancelled` 时 | 返回 `existing`，不自动重新处理（W3.5） | 待 David（盲选分歧）（第一片） |
| B6 | `config_version` 是否进默认库唯一键 | 不进（W4.2） | 已定（盲选一致）（第一片） |
| B20 | job 与费用记录的保留期 | 至少 90 天，且不短于对应幂等记录（W3.3） | 本 PR 评审（第一片） |
| C1 | 服务端为写操作新增的端点、计量、幂等与唯一约束（KB-C4-04、KB-C4-02b） | 按本文语义实现 | WeKnora 实现（第一片） |
| C2 | **READONLY R2.7 需同步修订**：凭据的操作范围按操作类扩展到写操作；与 BRIDGE 草案 §10 #25 一并处理。另：取消后的文档级 read 暂按 `index_failed`，是否另设 `reason` 一并定；SOP 撤回后 search 不返回、read 返回 `not_found`（W8.9），是 READONLY 的新过滤条件，需要写进 R2.6 / R4.4 | 修订前写操作不实现 | READONLY 下一修订（第一片） |
| C3 | **依赖 STATE 草案**：S1.1、S1.3、S1.5（需扩展到写操作，W1.4）、S1.6、S4.5、S4.6（W1.5、W7.12）、S4.7（run 绑定，待决 #7）、S5.3、S5.6 与 STATE §10 #4（#A9）、§6 本地结果、S5.8；S3.1（`request_in_progress` 不应触发就绪探测，W3.2） | 按 STATE 草案 r5 | STATE 定稿时逐条复核（第一片） |
| C4 | **依赖 BRIDGE 草案**：U1.3（session JWT 不含写操作）、U6.2（桥消息闭集）、U7.4（`approval` 等禁止键）；BRIDGE §10 #25 与本文 #C2 一并处理 | 按 BRIDGE 草案 r5 | BRIDGE 定稿时逐条复核（第一片） |
| C5 | **RunCreate 缺 principal、profile 字段**（STATE 草案 §9 第 4 条）；写操作的 run 归属与审计需要它们。**涉及内核接口，需知会 jason** | 不定实现 | KB-HOST-01（第一片） |

## 变更记录
- r0（2026-10-17）：首稿，分两片。§0–§6：授权绑定与机制约束、授权矩阵结构（单元格待 #996）、幂等与去重、初始化、ingest 与 job、验收 MT / IT / JT；§7–§9：ContextPack、SOP 候选与批准、验收 PT / AT；§10 WeKnora 现状证据；§11。影响夹具：否（#1012 只覆盖只读）。
- r1（2026-10-17）：按 B 类盲选裁定修订。B8 改为取消与失败都不回退当前源修订；B15 改为只能原文摘录；B16 改为候选只在宿主本地；B17 改为批准有效期 1 小时；B1、B2 补充；B3–B5、B7、B11–B13、B19 标待 David（盲选分歧）；新增 #A10–#A12；SOP 转移编号改为 V01–V12。影响夹具：否。
- r2（2026-10-17）：按第 1 轮双审互评裁定修订。新增写操作判定顺序（W1.9，含同键在途、回放先于其余检查）与 Off / 切换 / 注销对在途 job 的 v1 默认（W1.10，#A13）；只有新建工作的操作要求 ingest 就绪（W1.4）；后台复验建立 job 的操作类（W1.3）；新增 `pack.approve` 操作类，下载原件一行标 v1 不开放；W2.4 写明 SOP 职责分离在服务端的限度；基础修订 compare-and-set 进 J01 事务（W3.4）；`sop.commit` 幂等键即 `approval_id`；初始化 `existing` 不改授权、归属取自凭据与成员资格（W4.1、W4.3、W4.5）；本地 `write_outcome`；验收 MT-10–MT-19、IT-07–IT-08；§10 更正入队失败、来源身份去重、建 KB 的 `creator_id`、组织申请审批、batch-download 行号。ingest 与 job 部分：增强阶段可单独停止（J15–J17，`cancel_effect`、`enrichment`）；每个源修订至多一个非终态 job，重新处理期间原活动构建保持、失败或取消不撤下，`ingest.reprocess` 带 `expected_policy_version`（W5.13）；`origin: "sop"` 的文档只能经 `sop.commit` 修订（W5.1）；部分产物按 `job_id` 隔离（W5.8）；入队失败与停在 `saved` 的转移（J05）；验收 JT-25–JT-34；§10 第 3 条补 finalizing 可取消。影响夹具：否。 ContextPack 与 SOP 部分：W7.9 改为先按受众与组织身份筛候选集，逐来源比对修订三元组、`chunk_id` 仍在活动构建、来源未撤回或删除（新 `reason` `source_withdrawn`、`not_yet_valid`）；发布时同样校验来源；ContextPack 批准与发布分两步（W7.15），`approval_digest` 按 JCS 规范化，发布串行化（W7.16）；摘录以整句为单位；W8.6 并入 W1.9 顺序、①⑧ 与批准使用同一事务、批准只能由批准人本人消费，补全失败时批准与候选的状态（V13–V15）；作废通知与 hash 绑定（W8.3）；批准正文的访问与删除（W8.2）；验收 PT-18–PT-24、AT-16–AT-21。影响夹具：否。
- r3 第一片（2026-10-17）：按第 2 轮双审互评裁定修订，改为三片。矩阵新增 `pack.read`、`sop.approval.read`、`document.delete`、`chunk.edit`；内容去重只比对可检索生命周期内的文档，`sop.commit` 的去重只命中可检索的 SOP 文档（W3.5）；使批准作废的拒绝也写幂等记录（W3.2）；`request_in_progress` 不触发就绪探测；job 与费用记录单独保留（W3.3，#B20）；待决写操作持久化、principal 变化时保持未知（W1.5）；W1.10 覆盖 `cost.final: false`；`degraded` 准入；初始化中间态返回 `request_in_progress`（W4.5）；reprocess 单列检查链（W1.9）；验收 MT-20–21、IT-09；§10 补删除与分块路由、更正行号。影响夹具：否。
