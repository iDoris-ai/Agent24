# Knowledge 初始化 / ingest / job / ContextPack / SOP 语义契约 v1（CONTRACT-OPS-v1）

> **状态**：**草案（Draft）**，待 fork 侧维护者（David）评审（文档 PR，base `ab/weknora`）。评审通过前不授权任何实现。
> **版本**：契约 v1，文档修订 r8 · **日期**：2026-10-20 · **作者**：David Xu
> **Issue**：#1013 ③（KB-CT-03），阶段 #1000；消费方 KB-C4-02b、KB-C4-04、KB-CTX-01、KB-CTX-02、KB-SOP-01、KB-SOP-02，另供 KB-C4-03、KB-C4-05、KB-SVC-POLICY-01 引用。
> **依据**：任务方案 v2.1（方案全文见 #986 第一条评论）；`docs/knowledge/CONTRACT-READONLY-v1.md`（下称 READONLY，按 r7）；`docs/knowledge/CONTRACT-STATE-v1.md`（下称 STATE 草案，按 r14，PR #1070–#1072）与 `docs/knowledge/CONTRACT-UI-BRIDGE-v1.md`（下称 BRIDGE 草案，按 r5 与 David 2026-10-17 对它的决定）。**两份草案都尚未合入**：本文只引用它们的条款，依赖关系登记在 §11 #C3、#C4，草案合入时逐条复核。
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
| STATE 草案 | binding、generation、准入、作废流程、本地结果、run 绑定（S4.7，绑定 binding 集合）、多个 binding 同时激活（S4.9）、占位（S5.6）、崩溃安全不变式（S1.10） | 引用；本文不新增 generation 递增事件，也不新增 STATE 的持久状态 |
| BRIDGE 草案 | session JWT 的 `ops` 不含写操作（U1.3）；桥消息闭集与禁止键（U6.2、U7.4） | 引用：KnowledgeView 与桥都不是写操作的入口（W1.6） |
| KB-C4-01 | 版本化配置档、能力预检、格式白名单 | 本文只引用 `config_version` 与"不支持的格式在保存前拒绝"（W5.2） |
| KB-C4-02a | 引导凭据与日常凭据分离、平台 key 不进 Desktop | 本文定引导身份在矩阵里只能做初始化（§2） |
| KB-C4-03 | 激活凭据桥、三问引导 | License 与激活不授予任何写权限（W1.6） |
| KB-SVC-POLICY-01、#994 | 处理目的地、费用上限、预算 | 本文只定 `policy_version` 的绑定与复验时机（W1.3、W5.5）；政策内容不在本文 |
| #997 | 撤权、退出、取消后的原件与产物保留 | v1 都不删、如实报告保留了什么（W5.9，§11 #A5 已定）；之后的删除功能归 #997 |
| KB-CTX-02 | 自动注入入口（fresh / 续轮 / 恢复 / 审批继续 / 委派） | 本文定加载时的服务端复验与结果状态（§7） |

### 0.3 变更规则
沿用 READONLY §0.3。操作类集合、判定顺序、job 状态集合与转移、幂等作用域、批准绑定项、ContextPack 加载的复验项都是语义，改动升主版本（`CONTRACT-OPS-v2.md`）。新增 `reason`、`cancel_effect`、费用 `kind`、失败原因取值算 v1 修订；客户端必须把未知的 job 状态按 `failed` 兜底、未知 `cancel_effect` 按 `none` 兜底、未知 `enrichment` 按 `running` 兜底（费用按未结束处理）、未知 `stop_reason` 按"已停止，原因未知"展示、未知费用 `kind` 原样展示为"其他费用"[新设]。

## 术语
| 术语 | 定义 |
|---|---|
| 写操作 | 本文覆盖的、会改变服务端状态的操作；集合即 §2 矩阵的行。`job.read`、`pack.read`、`approval.read` 一并列入以便表达授权；`pack.read` 与 `pack.load` 唯一会写入的是把首次发现的过期或失效持久记为 `expired` / `invalidated`（W7.8），此外不改变状态。`sop.candidate` 是宿主本地操作、不改变服务端状态，列入矩阵只为表达"模型发起"一列（#A3）。 |
| 操作类 | §2 矩阵的一行，如 `ingest.submit`。凭据的操作范围与权威授权 A 的操作维度都按操作类表达（W1.1）。 |
| 新建工作的操作 | 会让服务端开始新的处理并产生费用的操作：`init.default_kb`、`ingest.submit`、`ingest.reprocess`、`sop.commit`，以及会触发重新 embedding 或模型调用的 `chunk.edit`（W1.4、W5.14）。 |
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

- **W1.1** [D2/R2.1+新设] **每个写操作都由服务端逐请求重新计算授权**：principal 只取自凭据（R1.5）；有效范围 E 按 R2.1 计算；另外要求该操作类同时在凭据的操作范围内、并且在 A 对目标资源授予的操作里。任一项不满足返回 `forbidden`（`operation_not_allowed`；请求显式指定的目标 KB（如 `ingest.submit` 的 `scope`，W5.1）不在 E 内按 R2.3 返回 `empty_scope`）；文档、job、批准记录等资源不在 E 内或不属于调用者时，以及只带资源标识（如只带 `knowledge_id` 的 `ingest.reprocess`）、从资源解析出的 KB 不在 E 内时，返回与不存在相同的 `not_found`（R2.6；R2.6 原文只说文档，扩到 job、批准记录与 ContextPack 登记在 §11 #C2），判定位置与适用范围见 W1.9 ③（批准记录只在作为 `approval.read`、`approval.void` 的目标时如此）。READONLY R2.7 现在把 v1 凭据的操作范围限定为 search 与 read，写操作需要 READONLY 同步修订（§11 #C2）。
- **W1.2** [D2+新设] 服务端在每个写操作的响应与审计里回报当次判定使用的 `authz_version`。客户端不得缓存它来跳过判定；它只用于追溯（ContextPack、SOP 批准记下它，W7.4、W8.4）。**粒度**：每个"主体 × 资源"一个值，资源包括 KB，也包括组织级的授予（如 `pack.publish`、`pack.approve`）；服务端可以保守地让它多变，用主体级或租户级的值也合规：多变只会让依赖它的记录更早失效或多复验一次，不会放宽授权（§11 #B1）。
- **W1.3** [KB-C4-04/KB-SVC-POLICY-01+新设] **后台作业沿用授权**：job 在建立时记下 `{操作类, principal, credential_id, tenant_id, target_kb, authz_version, policy_version, config_version, generation}`（generation 只记录，R7.5）。每个阶段边界（含每个增强子任务开始前）复验三项：① principal 当前仍在目标 KB 上持有**建立该 job 的操作类**（`ingest.submit`、`ingest.reprocess` 或 `sop.commit`；按当前 A，不按记下的 `authz_version`）；② 建立 job 所用凭据未被**撤销**（只是过期不算，job 寿命可以长于短期凭据）；③ 当前处理政策仍允许下一步的目的地与费用。主索引阶段 ① 或 ② 不成立 → `cancelled`（`authorization_revoked`，J07）；③ 不成立 → `failed`（`policy_denied` 或 `budget_exhausted`，J08）；增强阶段不成立时按 J16 只停增强。同一阶段边界上多项不成立时，按 ①②③ 的顺序取第一个不成立的（① 或 ② 不成立即按 J07 / J16 的 `authorization_revoked`，不再看 ③）。为使 ② 可判定，服务端的凭据撤销记录至少保留到引用该凭据的所有 job 进入终态且费用 `final`，凭据过期后也不得提前删除（§11 #B2）。
- **W1.4** [S1.5/R8.2+新设] **客户端准入**：Provider 只在 binding 处于可服务状态（S1.1，含 `degraded`）时发出写操作；**新建工作的操作**另要求最近一次就绪结果的 `operations.ingest` 为 `ready`。`job.read`、`job.cancel`、`pack.*`、`sop.approve`、`approval.read`、`approval.void`、`pack.discard`、`sop.withdraw`、`document.delete` 只要求可服务，不看 ingest 就绪（降级时仍能取消与撤回）。不满足按 STATE 草案 §6 本地拒绝（`not_ready` 等）。这不改变 STATE 的状态派生（S1.3 不看 ingest）。写请求同样带 `request_id` 与 `generation`（R7.2），按 S4.6 接受；多个 binding 同时激活时（S4.9），写操作只发往目标 KB 所属的那一个 binding。写操作收到的凭据类响应按 S3.1 处理（凭据无效类按 T42，旧凭据的撤权信号按复验）；写操作的响应不是 S5.10 服务端收窄的输入（S5.10 只看 search、read 与就绪探测）。STATE 草案 S1.5 目前只写了 search 与 read，需要它把准入扩展到写操作（§11 #C3）。
- **W1.5** [S4.5/R7.1+新设] **作废与写操作**：S4.5 中止在途写请求时，服务端可能已经执行。Provider 给调用方的本地 `cancelled` 必须带 `details.write_outcome: "unknown"`，有幂等键的再带上该键（`job.cancel` 等天然幂等的操作直接重发即可）；界面不得据此声称"未保存"。**待决的写操作**（操作类、请求体、幂等键）由宿主持久保存，直到得到确定结果；重启之后、Tool 层的重试都**复用同一键与同一请求体**：已执行的得到回放（W3.2），未执行的正常执行；产生 job 的操作也可以按键查询（W5.11）。**不得**用新幂等键自动重发同一写操作。待决写操作保存得比服务端的幂等记录保留期（W3.3）更久时，宿主不再用原键重发（服务端会当作新请求），改记 `write_outcome: "unknown"` 并提示用户自行确认。调用者 principal 变化后旧键不可用（W3.2），宿主保持 `write_outcome: "unknown"`，告知用户"该操作结果未知"，不自动重发。
- **W1.6** [#996/KB-SOP-02/KB-C4-03/U1.3+新设] **与业务决定无关的机制约束**（矩阵的"机制"列都由此而来）：
  - 授权只来自服务端的权威授权 A 与凭据，**不得**从 retrieve 能力、页面角色、上游 RBAC 角色、组织名输入、License、激活状态或请求体里的字段（如归属、创建人）推导（#996 正文、#1004 退出验收、KB-C4-03）。
  - 服务端**不信任宿主转述的批准**：批准只能是批准人用自己的凭据发出的已认证请求，服务端据此生成批准记录（W8.4）；请求里的 `approved: true` 之类字段一律忽略并记审计（KB-SOP-02）。
  - KnowledgeView 不是写操作入口：session JWT 的 `ops` 不含写操作（U1.3）；桥消息类型是闭集，`approval`、`approve`、`decision` 是禁止键（U6.2、U7.4）。
  - 宿主自身没有 principal，不能以自己的名义写；引导身份只用于初始化，且只代表调用者（W4.1，KB-C4-02a）。
  - 集成模式下 retrieve 级凭据不得取原件：上游 `/download`、batch-download 对本租户 retrieve key 开放（READONLY 矩阵 #5，`internal/router/routes_knowledge.go:120`、`:78`）；`/files` 在 API-key 门之外、本租户成员拿到 `file_path` 即可取原件，同样不开放（R2.7）。
- **W1.7** [R10.1/R10.3+新设] 每个写操作与每次 job 状态变化各记一条审计：`knowledge.init`、`knowledge.ingest.submit`、`knowledge.ingest.reprocess`、`knowledge.job.transition`、`knowledge.job.cancel`、`knowledge.document.delete`、`knowledge.chunk.edit`，以及 §7、§8 的 ContextPack 与 SOP 事件（W8.11）；字段按 R10.1，另加 `idempotency_key` 的哈希、`job_id`、`authz_version`、`policy_version`、费用摘要；不含原件、正文、query 与凭据（R10.3）。后台阶段的审计 principal 是建立 job 时记下的 principal，不是合成用户（R10.4）。
- **W1.8** [R7.7] 取消、撤权、撤回都追不回已经交付给模型或用户的内容，也追不回已经发生的费用；界面与审计不得声称可以。
- **W1.9** [R6.4+新设] **写操作的判定顺序**（服务端，取第一个不成立的）：① 凭据（`unauthorized`）→ ② 请求形状（`invalid_request`：缺字段、缺幂等键、`content_sha256` 不符、带 `knowledge_id` 而缺 `expected_source_revision` 等）→ ③ 授权与**目标解析**：W1.1 的操作类与 KB 判定（请求显式指定的目标 KB 不在 E 内为 `empty_scope`；从目标资源解析出的 KB 不在 E 内按本步的 `not_found`）；请求指向的文档（`knowledge_id`）存在、在 E 内、未撤回（W8.9）、不在删除中；job 存在且对调用者可见（W5.11）；`approval.read`、`approval.void` 的目标批准记录存在且属于调用者（W8.2）；ContextPack 存在且对调用者可见（W7.3），其中草稿对 `pack.approve` 可见指调用者在该 pack 所在范围内持有 `pack.approve`，对 `pack.publish` 可见指在该范围内持有 `pack.publish`；`sop.commit` 的提交人权限、`pack.approve` 批准人与 `pack.publish` 发布人对全部来源的读权（R4.6、W7.15）、`pack.discard` 只限起草人（P06）也在这一步。**以下不在本步判定** [新设]：`pack.publish` 出示的与 `sop.commit` 使用的 `approval_id` 是参数而不是目标，分别由 W7.15、W8.6-① 在回放之后判定；批准绑定的 `sop.commit` 目标文档存在且未撤回，由 W8.6-⑤b 在回放之后判定（批准人批准时已见过该目标，后判不泄露它是否存在）。目标不成立的各种情形（不存在、E 外、已撤回、删除中、不属于调用者）返回**同一个** `not_found`（R2.6），先于 ④⑤ 的一切检查，所以 `expected_policy_version`、格式或配额都不能用来区分它们 [新设]；唯一例外是同一主体在保留期内对自己已受理的删除或撤回（含仍在删除中）、已放弃或已作废的对象重复执行天然幂等操作，返回 `already: true`（W3.1）→ ④ 幂等回放（W3.2）→ ⑤ 其余：`ingest.submit` 依次为政策版本（W5.5）、格式与大小（W5.2）、配额、`origin` 限制（W5.1）、基础修订（W3.4）、内容去重（W3.5）；`ingest.reprocess` 依次为政策版本、修订为当前、已有占用 job（W5.13）；`document.delete` 为 `origin` 限制（W5.14）；`sop.withdraw` 为是 `origin: "sop"` 文档（普通文档为 `forbidden/operation_not_allowed`）；`chunk.edit` 依次为政策版本、`origin` 限制、当前政策下的目的地与预算（W5.14）；`pack.approve` 为该版本是 `draft`（否则 `stale/version_not_draft`），其余见 W7.15；`pack.revoke` 为该版本是 `published`（已 `revoked` 按 W3.1 返回 `already: true`，其他状态 `stale/version_not_published`）；`pack.discard` 为该版本是 `draft`（已 `discarded` 返回 `already: true`，其他状态 `stale/version_not_draft`）；`sop.commit` 见 W8.6；`pack.publish` 见 W7.15 → ⑥ 执行。回放先于 ⑤，所以首个请求已执行后，即使政策变了或批准已用，原样重试仍得到首次结果；回放仍先过 ③，失去授权的调用者拿不到回放。被 ①–⑤ 拒绝的请求不写幂等记录，唯一例外见 W3.2。
- **W1.10** [S5.3/R7.7+新设] **Off、取消选用或停用 profile、注销与在途 job**（§11 #A13 已定）：这些动作都**不取消**已建立的 job，服务端照常处理并计费（W5.3）。注销只销毁宿主的本地凭据，不等于 W1.3 ② 的服务端撤销，job 不因此停止。宿主在执行动作之前，若受影响的 binding 有它发起、**未终态或 `cost.final` 为 false** 的 job，必须提示用户"有 n 个处理仍在进行、会继续产生费用"，并提供先取消的入口；Off 作用于全部 binding（S4.9），提示汇总全部 binding，取消选用、停用与注销只看该 profile 的 binding。动作之后宿主不能再取消（binding 不可服务），只能等回到可服务后处理。
- **W1.11** [KB-C1-01+新设] **矩阵之外的上游写路由一律不开放**：v1 凭据只能执行 §2 矩阵里的操作类；上游其他会改变状态的路由（手工知识更新、移动、批量删除、批量重新处理、标签与文件夹、清空或删除 KB、复制 KB、图片信息更新、重新生成摘要等，§10 第 1 条）对 v1 凭据返回 `forbidden`（`operation_not_allowed`），由集成模式的允许列表执行（KB-C1-01）。另外，**任何会改变未撤回的 `origin: "sop"` 文档检索可见的内容（正文、分块及其启用状态、图片信息、标题、描述或摘要）、所在 KB 或存在与否的路由**，在集成模式下都按机制拒绝，只有 `sop.commit`、`sop.withdraw`，以及按 W5.13 对同一字节、同一解读方式重新建索引的 `ingest.reprocess` 例外；删除或清空 KB 不在机制拒绝之列：任何凭据下都先按 W5.14 撤回其中未撤回的 SOP 文档，再执行（§11 #A14 已定）。

## 2. 授权矩阵

- **W2.1** [#996+新设] 下表的行是本文的全部操作类，列是可能发起操作的主体类别。"待 #996"表示该格是业务决定，本文不填；"✗（机制）"表示无论 #996 怎么定都不允许，依据见最后一列。
- **W2.2** [#996+新设] **决定之前的 v1 默认**：所有"待 #996"的格按**拒绝**执行（fail closed）；开发与测试只在批准的样本租户上，由测试主体经服务端配置的临时授权进行，不构成产品授权，不得出现在对外构建里（READONLY §0.3 末条）。
- **W2.3** [新设] #996 决定后填格时，每格只能取"允许"、"允许，附条件（写明条件）"或"拒绝"；条件只能引用服务端可验证的事实（A、凭据、资源属性），不能引用客户端自报的角色。
- **W2.4** [新设] 职责分离（例如候选创建人能否批准自己的候选、发布人能否批准自己的 ContextPack）也属于 #996 的决定，写在相应行的条件里（§11 #A4）。**v1 的服务端只能执行它看得见的事实**：ContextPack 的起草人与批准人都在服务端留有记录，可以执行；SOP 候选只在宿主本地（W8.2），服务端不知道创建人，"创建人不得自批"在服务端保存候选或创建人以自己凭据登记候选（§11 #A12）之前，**不能作为服务端保证**，只能由宿主执行。

| 操作类 | 本机单用户（local-managed） | remote 个人 | 组织成员 | 组织中被授予特定职责者（职责名与授予方式待 #996） | 组织管理员 | 引导身份（KB-C4-02a） | 页面会话（session JWT） | 模型发起（Tool 调用） | 机制约束（与决定无关） |
|---|---|---|---|---|---|---|---|---|---|
| `init.default_kb` 初始化默认库 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 只能做这一项，且只代表调用者（机制） | ✗（机制） | ✗（#A3） | W1.1 服务端重验；归属取自调用者凭据与服务端成员资格（W4.1）；`existing` 不改授权（W4.3） |
| `ingest.submit` 提交原件 / 新源修订 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制，U1.3） | ✗（#A3） | W1.1；后台按 W1.3 复验；`origin: "sop"` 的文档不能经此修订（W5.1） |
| `job.read` 查询 job 与费用 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | ✗（#A3） | 目标 KB 必须在 E 内；v1 默认只有发起人可读（W5.11）；否则 `not_found`（R2.6） |
| `job.cancel` 取消 job 或其剩余增强 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制，U6.2 的 `cancel` 只撤回桥消息） | ✗（#A3） | 不要求 ingest 就绪（W1.4）；v1 默认只有发起人可取消（W5.11）；费用如实报告（W5.10） |
| `ingest.reprocess` 显式重新处理 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | ✗（#A3） | 新费用：新 job，按当前政策；每个修订至多一个占用中的 job（W5.13） |
| `document.download_original` 下载原件 | ✗（v1，R2.7）；之后待 #996 | 同左 | 同左 | 同左 | 同左 | ✗（机制） | ✗（机制，R2.7） | ✗（v1） | 不得由 retrieve 能力推导（W1.6） |
| `pack.draft` 起草 ContextPack（每次修改产生新版本） | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 允许起草（#A3）；主体格仍按 #996 | 草稿不被加载；版本不可变（P01） |
| `pack.discard` 放弃 ContextPack 草稿 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | ✗（#A3） | 只能放弃 `draft`，且只限起草人（P06、W1.9 ③）；重复放弃返回 `already: true` |
| `pack.read` 读取 ContextPack | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | ✗（#A3） | 受众或组织身份不成立 `not_found`；任何曾发布版本按 W7.9 ① 起判，① 不成立只回 `scope_mismatch`；只有通过 ①–④ 的 `published` 版本返回正文；草稿与起草人、批准人、发布人另按 W7.3 |
| `pack.approve` 批准 ContextPack 版本 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制，U7.4） | ✗（#A3，批准只由人确认） | 草稿对调用者不可见（未在该 pack 所在范围内持有 `pack.approve`，W1.9 ③）为 `not_found`；可见时批准人还须能读取全部来源（R4.6），否则 `forbidden`；产生 `purpose: "pack.publish"` 的批准记录，由发布人出示；与 `sop.approve` 是不同授予（W7.15） |
| `pack.publish` 用批准发布 ContextPack | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | ✗（#A3） | 批准与发布是两步（W7.15）；每个 `pack_id` 至多一个 `published`（W7.16） |
| `pack.revoke` 撤销 ContextPack | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | ✗（#A3） | 撤销后不再加载，已注入的追不回（W1.8） |
| `pack.load` 被加载进 run | 按受众（§7） | 按受众 | 按受众 | 按受众 | 按受众 | ✗（机制） | ✗（机制） | 由宿主注入入口发起，不由模型选择 | 每次加载服务端复验（W7.9）；受众的取值与谁可设定待 #996 |
| `sop.candidate` 生成、编辑、撤回 SOP 候选 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | 允许生成与编辑候选（#A3，宿主本地）；撤回候选由人发起 | 宿主本地操作；作废已有批准用 `approval.void`（W8.3） |
| `sop.approve` 批准 SOP | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制，U7.4） | ✗（#A3，批准只由人确认） | 不信宿主 `approved` 布尔；批准记录单次有效、绑定 hash 与基础修订（W8.4） |
| `approval.read` 查询批准状态、读回批准正文（SOP 与 ContextPack 两种用途） | 仅批准人本人（机制） | 同左 | 同左 | 同左 | 同左 | ✗（机制） | ✗（机制） | ✗（机制） | 按 `approval_id`；他人的批准记录按 R2.6 `not_found`（W8.2） |
| `approval.void` 作废自己的批准（SOP 与 ContextPack 两种用途） | 仅批准人本人（机制） | 同左 | 同左 | 同左 | 同左 | ✗（机制） | ✗（机制） | ✗（机制） | 按 `approval_id`；他人的批准记录按 R2.6 `not_found`（W8.3） |
| `sop.commit` 按批准写入正式库 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | ✗（#A3） | 服务端提交时重验批准（W8.6）；复用 ingest（§5） |
| `sop.withdraw` 撤回 SOP 文档 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | ✗（#A3） | 目标是未撤回的 `origin: "sop"` 文档，不要求已发布；取消在途 job、停止增强（W8.9）；重复撤回返回 `already: true`；已交付的追不回 |
| `document.delete` 删除文档 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | ✗（#A3） | `origin: "sop"` 的文档按机制拒绝，只能 `sop.withdraw`（W5.14）；触发 J10、J17；重复删除返回 `already: true` |
| `chunk.edit` 编辑、回滚、删除、启用 / 禁用分块，增删与重新生成问题 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | 待 #996 | ✗（机制） | ✗（机制） | ✗（#A3） | `origin: "sop"` 的文档按机制拒绝；带 `expected_policy_version`、报告费用（W5.14）；会改变 `chunk_revision`，使引用它的 ContextPack 失效（W7.9 ③） |

"模型发起"一列按 §11 #A3（David 已定）：模型只能做起草类操作，即起草 ContextPack（服务端草稿，不发布）与生成、编辑 SOP 候选（宿主本地）；提交原件、发布、批准、SOP 写入与撤回都只能由人发起，批准只能由人在宿主界面确认。服务端分不出请求来自模型还是用户（同一个凭据），这一列由宿主执行：Tool 层只提供起草类工具，其余操作只在宿主界面里由用户发起；起草格本身仍受 #996 的主体格约束。
矩阵之外的上游写路由对 v1 凭据一律不开放（W1.11）。

## 3. 幂等与去重

- **W3.1** [KB-C4-04/KB-C4-02b+新设] 以下写操作**必须**带 `idempotency_key`：`init.default_kb`、`ingest.submit`、`ingest.reprocess`、`pack.draft`、`pack.approve`、`pack.publish`、`sop.approve`、`sop.commit`、`chunk.edit`（会产生费用，W5.14）。缺失返回 `invalid_request`（`idempotency_key_missing`）。**`sop.commit` 的 `idempotency_key` 必须等于所用的 `approval_id`**，不等返回 `invalid_request`（`idempotency_key_mismatch`）。`pack.revoke`、`pack.discard`、`sop.withdraw`、`document.delete`、`approval.void` 天然幂等，不要求：同一主体在 W3.3 的保留期内重复执行、且仍通过 W1.1 时返回 200 与 `details.already: true`，不区分"刚才是我做的"之外的任何信息；其他主体按 R2.6。`pack.revoke` 另外：能按 W7.3 读到该版本、且持有 `pack.revoke` 的其他主体对已撤销的版本再撤销，同样返回 `already: true`。`job.cancel` 也不要求幂等键，重复执行按 J11 返回 `cancel_effect`，不另带 `already`。
- **W3.2** [新设] **作用域**：幂等记录的键是 `(tenant_id, principal, 操作类, idempotency_key)`。记录同时保存请求指纹 = H(操作类、目标资源、`content_sha256`、影响结果的参数，含 `expected_policy_version`、`config_version`、基础修订、`approval_id` 与规范化文件类型)。只有到达 W1.9 ⑥（或得到去重 / 已有 job 的确定结果）的请求才写记录。
  - 同键同指纹、首个请求已完成：返回首次的结果（同一 `job_id` 或资源 ID，附当前状态），`details.replayed: true`，不新建、不重复计费。
  - 同键同指纹、首个请求仍在执行：返回 `unavailable`（`request_in_progress`，可重试，带 `retry_after_ms`），不并发执行第二次。它是单个请求的结果，Provider 不据此触发 STATE 的就绪探测（需 STATE 草案配合，§11 #C3）。
  - 同键不同指纹：`invalid_request`（`idempotency_key_reused`），不执行。
  - 不同主体用同一个键互不影响（作用域含 principal）；调用者 principal 变化后，旧键对新 principal 不可见（W5.11）。
  - **例外**：会使批准作废的拒绝也写幂等记录，首个响应丢失后的重试得到同一个拒绝，而不是"批准不可用"：`sop.commit` 的 W8.6-③⑤b⑥⑦⑧（⑦ 只限格式与大小，配额不足不作废批准、不写记录），`pack.publish` 的 W7.15 中批准作废的情形。
- **W3.3** [新设] **保留**（§11 #B3、#B20 已定）：幂等记录与 job 记录同寿命。job 与费用记录至少保留到 job 终态后 90 天；产生 job 的幂等记录随对应 job 记录一起保留与删除；不产生 job 的（初始化、ContextPack、批准、`chunk.edit`，以及天然幂等操作的 `already` 判定）至少保留到写入后 90 天。过期后同键视为新请求。服务端重启不丢记录（持久化）。
- **W3.4** [新设] **原子性**：保存原件、建立源修订与 job、写幂等记录在同一个事务里完成（或以可恢复的两阶段完成），对外只有"全部完成"与"都未发生"两种结果；并发的同键请求只有一个执行（W3.2）。同一事务里还包括：W3.5 的内容去重判定；`expected_source_revision` 与目标文档当前源修订的 **compare-and-set**（并发提交基于同一修订时只有一个成功，其余 `stale/base_revision_changed`）；`sop.commit` 的批准校验与批准使用（W8.6）。上游的去重是先查后插，`knowledges` 表没有唯一约束（§10 第 2 条），并发上传同一文件会产生两行。
- **W3.5** [KB-C4-04+新设] **内容去重**（与幂等键无关、跨请求生效）：只比对处在**可检索生命周期**内的文档，即未撤回（W8.9）、不在删除中、未删除的文档（与上游排除 `deleting` 一致）。新建文档时，同一目标 KB 内 `content_sha256` 与规范化文件类型都相同、且某文档的当前源修订就是这份内容；修订已有文档时，只与**该文档自己的**当前源修订比较；`sop.commit` 另有限制：只命中本 KB 中 `origin: "sop"`、未撤回、当前修订有活动构建的文档，其他情况照常新建（W8.6-⑨）。命中时不新建源修订与 job，返回 200，`details.outcome: "existing"`，附已有文档与当前修订；最近一个 job 由调用者发起时附该 job，否则只附文档级处理状态（`index_pending` / `indexed` / `index_failed`），不回 `job_id` 与 `cost`（W5.11）；写幂等记录；不改动已有记录的任何时间戳；`sop.commit` 命中时批准同样记为已使用。不跨 KB 去重。去重键是 KB、`content_sha256` 与规范化文件类型（§11 #B4 已定）；命中的已有文档处于 `failed` / `cancelled` 时同样返回 `existing`、不自动重新处理，由调用方显式 `ingest.reprocess`（§11 #B5 已定）。上游用 MD5、排除 `failed` 与 `deleting`、对来源连接器另按来源身份去重、命中时返回 409 并改写 `created_at`（§10 第 2 条），都与本条不同。
- **W3.6** [新设] 客户端声明的 `content_sha256` 与服务端计算值不符时返回 `invalid_request`（`content_hash_mismatch`），不保存原件。
- **W3.7** [D2+新设] 幂等记录、指纹与去重查询都不得泄露范围外资源：命中他人的幂等记录或范围外文档时不回报其存在，按无命中处理。

## 4. 初始化（默认库）

- **W4.1** [KB-C4-02b/KB-C4-02a+新设] `init.default_kb`：为一个**归属**建立默认 KB 并绑定授权。归属只取自调用者的凭据（个人归属即调用者 principal）与服务端记录的组织成员资格（组织归属要求调用者在该组织中已验证、并持有组织归属的 `init.default_kb`；组织身份见 KB-ORG-01、#992）；请求体里自报的归属、创建人一律忽略（W1.6）。请求带 `idempotency_key` 与客户端已知的 `config_version`；服务端以引导身份完成创建与授权绑定，引导身份只代表这次调用者，引导凭据不离开服务端（KB-C4-02a）。
- **W4.2** [KB-C4-02b+新设] **唯一性**：每个归属至多一个默认 KB，唯一键为 `(tenant_id, 归属, "default")`，由服务端约束保证（唯一索引或等价机制），不依赖先查后建。`config_version` 不进唯一键（§11 #B6）：配置档升级不新建默认库。
- **W4.3** [KB-C4-02b+新设] **结果**：`{knowledge_base_id, outcome: "created" | "existing", config_version, authz_version}`。`existing` 时**绝不创建或修改任何授权绑定**；可见性按权威授权 A 判定（不按凭据范围 C，合法归属人用尚未包含新库的旧凭据也能得到 `existing`）：调用者在 A 中对已有默认库没有授权时返回 `forbidden`（`operation_not_allowed`），不回报 `knowledge_base_id`。`created` 之后，客户端按 STATE 草案的续签变宽（T27）重新取得包含新库的凭据。`existing` 且 `config_version` 与请求不同时，附 `details.config_mismatch: true`；是否以及怎样迁移配置不在本文。
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
| MT-06 | W1.6 | 集成模式下 retrieve 级凭据请求原件下载与批量下载；本租户成员拿 `file_path` 访问 `/files` | 都拒绝（v1 不开放） |
| MT-07 | W1.6 | 引导身份调用 `ingest.submit` | `forbidden/operation_not_allowed` |
| MT-08 | W1.4 | binding `ready`，`operations.ingest` 为 `not_ready`；分别调 `ingest.submit`、`chunk.edit`、`job.cancel`、`job.read`、`pack.revoke`、`sop.withdraw` | `ingest.submit`、`chunk.edit` 被本地拒绝 `not_ready`、不发网络请求；其余照常发出；STATE 状态不变 |
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
| MT-20 | W1.5 | 宿主有一个待决的 `pack.draft`，进程重启后 Tool 层重试；另测 principal 变化；另测待决操作已超过服务端保留期 | 第一种复用同一键，只有一个草稿版本；后两种保持 `write_outcome: "unknown"` 并提示用户，不自动重发 |
| MT-21 | W1.4 | binding `degraded`（`operations.ingest` 为 `ready`）时调 `ingest.submit` 与 `job.cancel` | 都照常发出 |
| MT-22 | W1.11 | v1 凭据调用上游的手工知识更新、移动、批量删除、清空 KB、删除 KB、复制 KB、图片信息更新、重新生成摘要 | 都 `forbidden/operation_not_allowed` |
| MT-23 | W5.11、W3.5、W5.13 | 同一 KB 的成员 Q 读取、取消 P 发起的 job；Q 提交与 P 的文档相同的内容；Q 对 P 的 job 正在占用的修订 `ingest.reprocess` | 前两项 v1 默认 `not_found`（只有发起人可读可取消）；后两项得 `existing` / `existing_job`，只附文档级处理状态，不含 P 的 `job_id` 与 `cost` |
| MT-24 | W3.1、W1.9 ③ | 同一主体重复 `document.delete`（含首次删除仍在进行时重复）、`sop.withdraw`、`pack.discard`、`pack.revoke`；另一主体对同一对象重复；能读到该包、持有 `pack.revoke` 的另一主体对已撤销版本再撤销 | 第一项 200，`already: true`；第二项 `not_found`（`pack.revoke` 除外）；第三项 200，`already: true` |
| MT-25 | W1.9 ③ | 对不存在的文档带错误的 `expected_policy_version` 调 `chunk.edit` 与 `ingest.reprocess`；带不支持的格式修订不存在的文档；对不存在的文档调 `document.delete`；对普通文档调 `sop.withdraw` | 前四项都是 `not_found`（目标解析先于政策、格式与配额）；最后一项 `forbidden/operation_not_allowed` |
| MT-26 | W1.9 ③、W1.1、R2.6 | 同一批请求（显式指定目标 KB 的，如修订文档的 `ingest.submit`，`scope` 一律取调用者 E 内的 kb-a；`ingest.reprocess`、`chunk.edit` 等只带 `knowledge_id`，不带 `scope`）分别指向 E 外 KB 里存在的文档、不存在的 `knowledge_id`、已撤回的文档、删除中的文档，每种都配错误的 `expected_policy_version`、不支持的格式与超出配额 | 各情形的响应完全相同（同一 `code`、`reason` 与 `details` 字段集合），都不暴露政策、格式或配额的判定结果 |
| IT-01 | W4.2、W4.5 | 同一归属并发 10 个 `init.default_kb`（不同幂等键） | 恰好一个 `created`；其余在中间态期间得 `unavailable/request_in_progress`，重试后得 `existing`，同一 `knowledge_base_id` |
| IT-02 | W4.5 | 建 KB 之后、绑定授权之前注入崩溃；之后用同一幂等键重试；另由同组织其他成员调用 | 崩溃期间对外不可见；同一调用者重试完成绑定；其他成员不能接续 |
| IT-03 | W4.3 | 已有默认库，用新 `config_version` 再初始化 | `existing`，`config_mismatch: true`；库内资料与配置不变 |
| IT-04 | W4.4 | 默认库已有资料与人工修订的分块；重复激活与登录 | 资料、修订与配置都不变 |
| IT-05 | W4.5 | 未完成的 KB 超过回收时限 | 被回收；其他 KB 与资料不受影响 |
| IT-06 | W4.6 | 默认库不存在时请求就绪 | search 就绪照常判定，不等初始化 |
| IT-07 | W4.3 | 已被撤权的组织成员再调用组织归属的初始化 | `forbidden`；不回报 `knowledge_base_id`；授权绑定不变 |
| IT-08 | W4.1 | 请求体自报另一个组织为归属；引导身份调用时未带调用者 | 归属按调用者凭据与成员资格判定；后者拒绝 |
| IT-09 | W4.5 | 中间态期间同组织另一成员调用初始化 | `unavailable/request_in_progress`，带 `retry_after_ms`；不接续、不阻塞 |
| IT-10 | W4.3 | 归属人用不含新库的旧凭据再次初始化；随后续签 | `existing`，回报 `knowledge_base_id`；续签后的凭据范围包含新库 |

## 5. ingest 与 job

### 5.1 提交
- **W5.1** [KB-C4-04+新设] `ingest.submit` 的请求：`scope`（只含目标 KB）、`idempotency_key`、原件（文件字节或文本）、`content_sha256`、`file_name` 与规范化文件类型、`expected_policy_version`；更新已有文档时另带 `knowledge_id` 与 `expected_source_revision`（缺失为 `invalid_request`；不等于当前源修订为 `stale`，`reason: "base_revision_changed"`，附 `current_revision`；文档不存在或正在删除为 `not_found`）。**`origin: "sop"` 的文档只能经 `sop.commit` 修订**，对它的 `ingest.submit` 返回 `forbidden`（`operation_not_allowed`）。已撤回的文档（W8.9）按不存在处理：修订、重新处理都返回 `not_found`。v1 只收文件与文本，服务端不接受 URL、不代为抓取；需要导入网页时，由宿主在本机抓取后按文件提交（§11 #A2 已定），宿主抓取的出口规则归宿主侧（#994），不在本文。
- **W5.2** [KB-C4-01/R3.8+新设] **保存前拒绝**：格式不在配置档白名单、或其输出不会产生 v1 范围内的 `text` 分块（R3.8）时，返回 `invalid_request`（`unsupported_format`），不保存原件、不建 job；超过大小上限返回 `invalid_request`（`too_large`）；配额不足返回 `forbidden`（`quota_exceeded`）。上游在算 hash 之前拒绝视频与非法扩展名，FAQ 类 KB 拒绝文件上传，配额检查在去重之后（§10 第 1 条）。
- **W5.3** [新设] **断开不是取消**：J01 之后，提交请求的连接断开、客户端超时、generation 作废、Off、切换与注销都不影响 job（W1.10）；只有 `job.cancel`、W1.3 的复验、J09、J10 与 J17 能停止它。这与只读请求不同（READONLY R7.1 的断开即取消只适用于 search 与 read）。
- **W5.4** [R5.1–R5.3+新设] **源修订**：每次被接受的提交（非去重命中）分配一个新的 `source_revision`，在 J01 时成为该文档的**当前源修订**；它的活动构建在 J04 之前为 `null`。这与 READONLY R5.3"活动构建还没建好时 `index_build_id: null`"、R4.4 的 `index_pending` / `index_failed` 一致。于是新修订处理期间，旧修订构建上的 hit（若仍保留）按 R5.3 为 `stale`。**取消与失败都不回退当前源修订**（§11 #B8）：本修订仍是当前，只是没有活动构建；要恢复旧内容，需把旧原件重新提交为新源修订。取消不等于撤销这次替换（§11 #A10 已定）。同一修订的重新处理不在此列，见 W5.13。
- **W5.5** [KB-SVC-POLICY-01/#994+新设] `ingest.submit` 与 `ingest.reprocess` 的 `expected_policy_version` 与服务端当前政策版本不同时返回 `stale`（`policy_changed`，附当前版本），不保存、不建 job：用户同意时看到的处理目的地与费用上限，必须就是 job 执行时依据的政策。它在幂等指纹里（W3.2）。

### 5.2 状态
- **W5.6** [KB-C4-04+新设] job 状态是闭集：

| 状态 | 含义 | 原件 | 本 job 的构建可检索 | 终态 | `job.cancel` |
|---|---|---|---|---|---|
| `saved` | 原件已持久保存，源修订与 job 已记录，尚未开始处理 | 已保存 | 否 | 否 | 生效（J06） |
| `processing` | 解析、切分、embedding、建索引进行中 | 已保存 | 否（未激活的部分产物不可检索，W5.8） | 否 | 生效，异步完成（J06、W5.9） |
| `indexed` | 本 job 的索引构建已激活为活动构建（R5.1）；增强子任务可能仍在进行（`cost.final: false`） | 已保存 | 是，且仅当它仍是该修订的活动构建（被 J04 替换后不再可检索） | 是（状态不再变，增强可单独停止） | 增强进行中：停止剩余增强（J15）；否则无效果（J11） |
| `failed` | 处理失败；带 `failure_reason` | 已保存 | 否（同一修订之前的活动构建不受影响，W5.13） | 是 | 无效果（J11） |
| `cancelled` | 被取消；带 `cancel_reason` | 按 W5.9 报告 | 否（同上） | 是 | 无效果（J11） |

  `failure_reason`：`parse_failed`、`embedding_failed`、`index_failed`、`policy_denied`、`budget_exhausted`、`storage_failed`（J01 之后的存储故障）、`enqueue_failed`、`interrupted`、`timeout`、`internal`。`cancel_reason`：`user`、`authorization_revoked`、`superseded`、`document_deleted`、`document_withdrawn`。`indexed` 的 `enrichment` 字段：`none`（没有增强）、`running`、`done`、`stopped`（带 `stop_reason`：`user`、`authorization_revoked`、`policy_denied`、`budget_exhausted`、`superseded`、`document_deleted`、`document_withdrawn`，以及增强自身出问题时的 `failed`（子任务重试用尽）、`timeout`（超过增强时限）、`interrupted`（中断后未能在恢复时限内继续））。`enrichment` 离开 `running` 之后，所有在途调用结束时 `cost.final` 必须变为 `true`。
  **上游映射**：`pending` → `saved`；`processing` → `processing`；`completed` → `indexed`；`failed` → `failed`；`cancelled` → `cancelled`，但上游在 `finalizing` 时取消也记为 `cancelled`（§10 第 3 条），这时映射为 `indexed` 加 `enrichment: "stopped"`（`user`）；`deleting` 不是 job 状态（文档删除，J10）；手工知识的 `draft` 不是 job（§10 第 9 条）；`finalizing`（主解析完成、增强子任务仍在花费资源，已可检索）映射为 `indexed` 加 `enrichment: "running"`、`cost.final: false`，直到增强子任务结束（§11 #B7 已定）。

### 5.3 转移
- **W5.7** [新设] 转移表（同一 job 的转移串行；`saved`、`processing` 之外的状态不再变，`indexed` 只有 `enrichment` 还会变）：

| ID | 前置 | 事件 | 后置 | 当前源修订与活动构建 | 依据 |
|---|---|---|---|---|---|
| J01 | — | 提交被接受（W3.4 同一事务） | `saved` | 新提交：本修订成为当前（W5.4）；重新处理：不变（W5.13） | [KB-C4-04] |
| J02 | — | 内容去重（W3.5）、同键回放（W3.2）或该修订已有占用中的 job（W5.13） | 不新建 job | 不变 | [新设] |
| J03 | `saved` | 阶段边界复验通过，开始处理 | `processing` | 不变 | [W1.3] |
| J04 | `processing` | 索引构建完成并激活 | `indexed` | 本 job 的构建成为该修订的活动构建；同一修订之前的构建被替换（R5.1），其 job 若增强仍在进行，按 J17 停止 | [R5.1] |
| J05 | `saved` / `processing` | 入队失败、解析或依赖失败且重试用尽、进程正常但超过处理时限（含停在 `saved` 的） | `failed`（`enqueue_failed` / `parse_failed` / … / `timeout`） | 不变 | [R4.4] |
| J06 | `saved` / `processing` | `job.cancel` | `cancelled`（`user`）；`cancel_effect: "cancelled"` | 不变 | [KB-C4-04+新设] |
| J07 | `saved` / `processing` | 阶段边界复验：授权被撤（W1.3 ①②） | `cancelled`（`authorization_revoked`） | 不变 | [W1.3] |
| J08 | `saved` / `processing` | 阶段边界复验：政策不允许或预算耗尽（W1.3 ③） | `failed`（`policy_denied` / `budget_exhausted`） | 不变 | [KB-SVC-POLICY-01] |
| J09 | `saved` / `processing` | 同一文档的新源修订被接受（J01） | `cancelled`（`superseded`） | 变为更新的修订 | [新设] |
| J10 | `saved` / `processing` | 文档被删除，或被 `sop.withdraw` 撤回 | `cancelled`（`document_deleted` / `document_withdrawn`） | 文档不存在或已撤回 | [新设] |
| J11 | `indexed`（增强不在进行）/ `failed` / `cancelled` | `job.cancel` | 不变；200，`cancel_effect: "none"` | 不变 | [新设] |
| J12 | `indexed`（增强不在进行）/ `failed` / `cancelled` | `ingest.reprocess`（W5.13） | 原 job 不变；新 job `saved`（J01） | 不变 | [新设] |
| J13 | `processing` | 服务端重启或工作进程中断 | 在恢复时限内恢复则仍 `processing`；否则 `failed`（`interrupted`） | 不变 | [新设] |
| J14 | `saved` / `processing` / `indexed`（增强中） | 建立 job 所用凭据过期（未撤销） | 不变 | 不变 | [W1.3] |
| J15 | `indexed`（`enrichment: "running"`） | `job.cancel` | `indexed`；`enrichment: "stopped"`（`user`）；`cancel_effect: "enrichment_stopped"` | 不变；活动构建保留 | [§10 第 3 条+新设] |
| J16 | `indexed`（`enrichment: "running"`） | 增强子任务开始前复验不成立（W1.3） | `indexed`；`enrichment: "stopped"`（`authorization_revoked` / `policy_denied` / `budget_exhausted`）；记审计与费用 | 不变 | [W1.3] |
| J17 | `indexed`（`enrichment: "running"`） | 同一文档的新源修订被接受（J01）、文档被删除或撤回；另有防御性触发：同一修订的新构建激活（J04；W5.13 的占用规则使它正常不会发生，只用故障注入测试） | `indexed`；`enrichment: "stopped"`（`superseded` / `document_deleted` / `document_withdrawn`） | 变为更新的修订 / 文档不存在或已撤回 / 不变 | [新设] |
| J18 | `indexed`（`enrichment: "running"`） | 增强子任务重试用尽、超过增强时限，或中断后未能在恢复时限内继续（含服务端重启） | `indexed`；`enrichment: "stopped"`（`failed` / `timeout` / `interrupted`）；`cost.final` 随后为 `true` | 不变；活动构建保留 | [新设] |

  新修订的构建激活（J04）之前，**旧修订**的构建是否保留不作要求：保留时只能按 R5.3 作为 `stale` 线索，不得作为当前构建返回 `fresh`（取消不撤销替换，§11 #A10，所以不要求保留）。上游 reparse 在重建前先清理旧分块（READONLY R4.4 引用的 `internal/application/service/knowledge_process.go:423`）。当前修订没有活动构建时，不带 `expected_revision` 的文档级 read：J05–J08 之后返回 `unavailable/index_failed`（R4.4；取消是否另设 `reason` 随 §11 #C2 的 READONLY 修订）；J09 之后当前修订是更新的那个，按它的进度返回 `index_pending` 或其结果；J10 之后返回 `not_found`。
- **W5.8** [R5.3+新设] 部分产物按 `job_id` 标记：`processing`、`failed`、`cancelled` 的 job 已写下的分块与索引**不得**被检索或读取到，也不得成为活动构建；一个 job **不再有工作之后**（不处于 `saved`、`processing`，且 `enrichment` 不为 `running`）迟到写下的产物一律丢弃，`indexed` 之后增强阶段正常写下的产物照常保留；构建被替换后，属于旧构建的增强产物不再可检索，之后迟到的写入同样丢弃。本条的"丢弃"只指不接受迟到的写入，不删除已经写下的产物（W5.9）。新 job 只认自己 `job_id` 的产物。上游取消时保留已写入的分块与索引（§10 第 3 条），检索过滤只看分块的 `is_enabled`、`index_status` 与类型，不看文档的解析状态（§10 第 4 条）→ KB-C4-04。

### 5.4 取消、费用与重新处理
- **W5.9** [KB-C4-04+新设] **取消**：`job.cancel` 同步记录取消并返回 `cancel_effect`（`cancelled` / `enrichment_stopped` / `none`），随后在每个阶段边界停止；已经发出的外部调用可能仍会完成并计费。响应与之后的 `job.read` 如实报告：`original_retained`（原件是否仍保存）、`partial_artifacts`（`discarded` / `retained_invisible`）、`cost`（W5.10）。取消、失败与撤权都不删除原件与部分产物，只如实报告（§11 #A5 已定，之后的删除功能归 #997）：v1 的 `original_retained` 恒为 `true`；`partial_artifacts` 对 J01 之后、终态之前写下的产物恒为 `retained_invisible`，`discarded` 在 v1 不出现（W5.8 的迟到写入不被接受，不算已有产物）。Lite 模式没有队列出队，只能靠检查点停止（§10 第 3 条），同样适用本条。
- **W5.10** [KB-C4-04/KB-SVC-POLICY-01+新设] **费用报告**：每个 job 带 `cost: { final, items: [{ kind, destination, units, unit_type, measured }] }`。
  - `kind`：`storage`、`parse`、`embedding`、`multimodal`、`llm_enrichment`（摘要、问题生成、图谱、wiki 等）；`destination` 取处理政策里的目的地标识；`unit_type`：`bytes`、`pages`、`tokens`、`calls`、`images`；`measured`：`exact`、`estimated`、`unknown`。
  - **如实**：不知道就填 `units: null` 加 `unknown`，**不得填 0**；估算必须标 `estimated`。`final: true` 只在不再有任何工作可能产生费用之后（所有在途调用都已结束、增强子任务都已终止或停止）。界面只有在 `final: true` 且全部项为精确 0 时，才能说"没有产生费用"。
  - 金额、币种与承担方不在本文（#994、#995）；服务端若给出金额，放在可选的 `amount` 里并标明依据的价格表版本。
  - 上游没有按文档持久化的计量，embedding 用量只在 Langfuse 追踪里按 rune 数 / 4 估算（§10 第 5 条）→ KB-C4-04、KB-SVC-POLICY-01。
- **W5.11** [新设] **查询**：`job.read` 可按 `job_id`，或按 `(操作类, idempotency_key)` 查询（作用域同 W3.2，只能查自己当前 principal 的键；principal 变化后旧键不可查，只能按 `job_id`）；返回操作类、状态、`enrichment`、`knowledge_id`、`source_revision`、`index_build_id`（`indexed` 时）、`attempt`、`failure_reason` / `cancel_reason`、`cost`、各状态时间戳。目标 KB 不在 E 内时 `not_found`（R2.6）。**谁能读取与取消 job**：v1 默认只有发起该 job 的 principal（按 W1.1 仍须持有相应操作类），同 KB 的其他成员按 R2.6 得 `not_found`；是否放宽为"KB 上持有该操作类的人都可以"属于 #996 的条件（§11 #A1）。文档删除（J10、J17）之后，job 与费用仍可在 W3.3 规定的 job 与费用记录保留期内查询。
- **W5.12** [D4] job 状态与费用只是元数据，不是知识证据，不进入模型上下文作为引文；Tool 层展示时按普通状态处理。
- **W5.13** [新设] **重新处理**：`ingest.reprocess` 带 `knowledge_id`、`source_revision`、`idempotency_key` 与 `expected_policy_version`，判定顺序见 W1.9：目标解析（文档存在、在 E 内、未撤回、不在删除中，否则统一 `not_found`）在 ③；⑤ 里 `source_revision` 不是当前源修订为 `stale/base_revision_changed`，该修订已有**占用中**的 job 见下。不做格式、配额、`origin` 与内容去重检查（没有新原件）。**占用**指 job 处于 `saved`、`processing`，或 `indexed` 且 `enrichment: "running"`；每个源修订至多一个占用中的 job，已有时不新建，返回 200，`details.outcome: "existing_job"`；该 job 由调用者发起时附该 job，否则只附文档级处理状态，不回 `job_id` 与 `cost`（W5.11，J02）；判定与创建在同一事务里。重新处理期间，该修订原有的活动构建**保持到新 job 的 J04 才被替换**；新 job `failed` 或 `cancelled` 时原活动构建不撤下，文档照常可检索。**新构建不继承旧构建上的人工分块编辑**（分块属于构建，R5.1）：该修订有 `chunk_revision > 0` 的分块时，宿主在发起重新处理之前必须提示用户"人工编辑将不会保留"。SOP 文档的重新处理不需要新批准：批准绑定的是字节与解读方式，不绑定分块边界（W8.4）。
- **W5.14** [S-5+新设] **删除与分块编辑**：`document.delete` 与 `chunk.edit`（编辑、回滚、删除、启用 / 禁用分块，增删与重新生成问题）按矩阵授权；对 `origin: "sop"` 的文档一律 `forbidden`（`operation_not_allowed`），SOP 文档只能经 `sop.commit` 修订、经 `sop.withdraw` 撤回（通用规则见 W1.11）。删除或清空 KB 时，KB 内未撤回的 SOP 文档**一并撤回**（§11 #A14 已定）：每份先按 W8.9 撤回（取消在途 job、停止增强、记审计 `knowledge.sop.withdraw`，原因 `kb_deleted`），再执行删除或清空；不论经哪种凭据删除或清空都如此（W1.11）。`document.delete` 触发 J10、J17；同一主体在保留期内重复删除返回 200 与 `already: true`（W3.1）。`chunk.edit` 会同步重新 embedding，增删与重新生成问题会调用模型（§10 第 1 条），所以它带 `expected_policy_version`（不符为 `stale/policy_changed`）、执行前按当前政策复验目的地与预算（目的地不被允许为 `forbidden/policy_denied`，预算不足为 `forbidden/budget_exhausted`，都不发出调用）、响应带与 W5.10 同格式的 `cost`，并要求 ingest 就绪（W1.4；§11 #B21 已定）；它改变 `chunk_revision`（R5.1），引用该分块的 ContextPack 在下次发布或加载时失效（W7.9 ③）。

## 6. 验收：ingest 与 job

前提同 §4.1。KB-C4-04、KB-C1-04 按本表写测试。

| ID | 规则 | 构造 | 期望 |
|---|---|---|---|
| JT-01 | J01、W3.4 | 正常提交 | `saved`；原件、源修订、job、幂等记录都已持久；新修订成为当前 |
| JT-02 | J02、W3.2 | 同键同指纹重放三次（含首个响应丢失） | 同一 `job_id`，`replayed: true`；只计费一次 |
| JT-03 | W3.2 | 同键、不同 `content_sha256` | `invalid_request/idempotency_key_reused` |
| JT-04 | W3.4 | 不同键并发提交同一文件到同一 KB | 只有一个新源修订与 job；其余为 `existing` |
| JT-05 | J02、W3.5 | 提交与当前修订相同的内容；修订文档 B 时内容等于文档 A 的当前内容 | 前者 `outcome: "existing"`、不新建 job、时间戳不变；后者为 B 新建修订 |
| JT-06 | W3.6 | 声明的 `content_sha256` 与实际不符 | `invalid_request/content_hash_mismatch`；不保存 |
| JT-07 | W5.2 | 不支持的格式；超过大小上限；配额不足 | 依次 `unsupported_format`、`too_large`、`forbidden/quota_exceeded`；都不保存 |
| JT-08 | W5.5 | `expected_policy_version` 已过期 | `stale/policy_changed`，附当前版本；不保存 |
| JT-09 | W5.1 | 更新已有文档：`expected_source_revision` 不是当前值；缺 `expected_source_revision`；文档正在删除 | 依次 `stale/base_revision_changed`、`invalid_request`、`not_found`；都不保存 |
| JT-10 | J03、J04 | 正常处理 | `saved` → `processing` → `indexed`；`index_build_id` 成为活动构建；READONLY search 能命中 |
| JT-11 | J05 | 解析失败且重试用尽；另测入队失败；另测停在 `saved` 超过时限 | 依次 `failed/parse_failed`、`failed/enqueue_failed`、`failed/timeout`；文档级 read 得 `index_failed` |
| JT-12 | J06、W5.9 | `processing` 中取消，测试桩让一次 embedding 调用在取消后才返回 | 立即 `cancelled/user`，`cancel_effect: "cancelled"`；`cost.final` 先为 false，调用结束后为 true 且含该次调用；部分产物不可检索；原件保留并如实报告（`original_retained: true`，`partial_artifacts: "retained_invisible"`） |
| JT-13 | J06、W5.4 | 文档已有 indexed 的修订 r1，提交 r2 后在 `processing` 中取消 | 当前源修订仍为 r2，没有活动构建；文档级 read 得 `unavailable/index_failed`；r1 的 hit 若出现只能是 `stale`；重新提交 r1 的原件得到新修订 r3 |
| JT-14 | J07 | `processing` 中撤销提交人对目标 KB 的 ingest 授权；另测撤销提交所用凭据 | 下一个阶段边界都 → `cancelled/authorization_revoked`；不再产生新调用 |
| JT-15 | J14 | 提交后凭据自然过期（未撤销） | job 照常完成 |
| JT-16 | J08 | 处理中政策改为禁止 embedding 目的地；另测预算耗尽 | 下一个阶段边界 → `failed/policy_denied`、`failed/budget_exhausted`；未授权目的地零调用 |
| JT-17 | J09 | r1 处理中提交 r2 | r1 的 job `cancelled/superseded`；当前为 r2 |
| JT-18 | J11 | 对增强已结束的 `indexed`、`failed`、`cancelled` 的 job 取消 | 200，`cancel_effect: "none"`，状态不变 |
| JT-19 | J12、W5.13 | 对 `failed` 的 job 显式重新处理；对非当前修订重新处理 | 前者新 `job_id`，旧 job 仍 `failed`，按当前政策计费；后者 `stale/base_revision_changed` |
| JT-20 | J13、J05 | 处理中重启服务端并在恢复时限内恢复；中断后超过恢复时限；进程正常但超过处理时限 | 依次继续处理、`failed/interrupted`、`failed/timeout`；不会永远停在 `processing` |
| JT-21 | W5.3、W1.5 | 提交后立即断开连接；另测 STATE 草案 S4.5 中止在途提交 | job 照常处理；后者 Provider 得本地 `cancelled`，带 `write_outcome: "unknown"` 与幂等键，回到可服务后按键查到真实 job |
| JT-22 | W5.8 | 取消或失败后，用分块 ID 直接 read、用相关 query search；被取消的 job 在终态后又写下产物 | 部分产物都不出现；迟到产物被丢弃 |
| JT-23 | W5.10 | 测试桩让某目的地无法计量 | 该项 `units: null`、`measured: "unknown"`，不为 0；界面不显示"没有费用" |
| JT-24 | W5.7 | r2 的 job 失败后 search | r1 的 hit 不出现，或只以 `stale` 出现，不生成确定引用；从不以 `fresh` 出现 |
| JT-25 | J10、W5.11 | 处理中删除文档；之后按 `job_id` 查询 | job `cancelled/document_deleted`；job 与费用仍可查 |
| JT-26 | J15 | `indexed` 且增强进行中时取消 | `cancel_effect: "enrichment_stopped"`；状态仍 `indexed`，活动构建可检索；增强停止，`cost.final` 随后为 true |
| JT-27 | J16 | 增强进行中撤销提交人授权；另测政策禁止增强目的地 | 下一个增强子任务开始前停止：`enrichment: "stopped"`，`stop_reason` 分别为 `authorization_revoked`、`policy_denied`；有审计与费用 |
| JT-28 | J17 | r1 `indexed` 且增强进行中时提交 r2 | r1 的增强停止（`superseded`），不再为 r1 花费 |
| JT-29 | W5.13 | 不同幂等键并发两个 `ingest.reprocess` 同一修订 | 只有一个新 job；另一个得 `existing_job` |
| JT-30 | W5.13 | 已 `indexed` 的修订重新处理，新 job 失败；另测新 job 被取消 | 两种情况下原活动构建始终可检索，从未撤下 |
| JT-31 | W5.13、W5.5 | `ingest.reprocess` 缺 `expected_policy_version`；版本已过期 | 依次 `invalid_request`、`stale/policy_changed` |
| JT-32 | W3.4 | 两个提交都带 `expected_source_revision: r1` 并发到达 | 只有一个建立 r2；另一个 `stale/base_revision_changed` |
| JT-33 | W1.3 | 用 `ingest.reprocess` 建立的 job；提交人只有 `ingest.reprocess`、没有 `ingest.submit` | J03 照常，不被误取消 |
| JT-34 | W1.4 | `operations.ingest` 为 `not_ready` 时对 `processing` 的 job 取消 | 照常发出并生效 |
| JT-35 | W5.13 | 修订已 `indexed` 且增强进行中时 `ingest.reprocess` | `outcome: "existing_job"`，不新建 job |
| JT-36 | J04、J17、W5.8 | 故障注入：绕过占用规则，让同一修订的新 job 在旧 job 增强进行中激活构建 | 旧 job `enrichment: "stopped"`（`superseded`）；旧构建及其增强产物不再可检索，之后迟到的写入被丢弃 |
| JT-37 | J18 | 增强子任务重试用尽；超过增强时限；增强中重启且超过恢复时限 | 依次 `stopped`（`failed` / `timeout` / `interrupted`）；`cost.final` 都随后变为 `true` |
| JT-38 | W3.5 | 提交的内容等于一份已撤回文档、或删除中文档的当前内容 | 不命中去重，照常新建文档与 job |
| JT-39 | W5.13、W5.1、W1.9 ③ | 对已撤回文档重新处理或修订；对删除中的文档重新处理 | 都 `not_found`；删除中的文档不新建 job |
| JT-40 | W5.14 | v1 凭据：对普通文档 `document.delete`；对 `origin: "sop"` 的文档 `document.delete`、编辑分块、删除分块、禁用分块、增删生成问题。集成模式下的上游非 v1 凭据（Admin JWT）：删除、清空含未撤回 SOP 的 KB；删除只含已撤回 SOP 的 KB | 第一项成功并触发 J10 / J17；v1 的其余各项 `forbidden/operation_not_allowed`；含未撤回 SOP 的 KB 删除或清空时，其中的 SOP 先逐份撤回（审计原因 `kb_deleted`，在途 job `cancelled/document_withdrawn`），再删除或清空；只含已撤回 SOP 的直接放行 |
| JT-41 | W5.8 | `indexed` 之后增强阶段正常写下产物；之后增强停止，再有迟到产物 | 前者保留；后者丢弃 |
| JT-42 | W5.14 | `chunk.edit` 时政策版本已变；政策禁止 embedding 目的地；预算不足 | 依次 `stale/policy_changed`、`forbidden/policy_denied`、`forbidden/budget_exhausted`，后两者零调用；成功时响应带 `cost` |
| JT-43 | W5.13 | 修订有人工编辑的分块时发起重新处理 | 宿主先提示"人工编辑将不会保留"；新构建激活后分块为上游重新切分的结果，`chunk_revision` 为 0 |
| JT-44 | W3.5、#B5 | P 发起的当前修订 job 为 `failed`，P 与同 KB 的 Q 分别提交同内容；另测该 job 为 `cancelled` | 都 `outcome: "existing"`，不新建源修订与 job、不自动重新处理；P 得到该 job，Q 只得文档级状态 `index_failed` |
| JT-45 | W1.3、J07、J08、J16 | `processing` 中同时撤销提交所用凭据、并让政策禁止下一步目的地；另在增强进行中同样构造 | 前者 `cancelled/authorization_revoked`，不是 `failed/policy_denied`；后者 `enrichment: "stopped"`（`authorization_revoked`） |

## 7. ContextPack

### 7.1 结构
- **W7.1** [KB-CTX-01] ContextPack 是经批准、带版本、体量很小的背景包，供自动注入（KB-CTX-02）。字段：`pack_id`、`pack_version`（单调递增，发布后不可变）、`content`（UTF-8 文本）、`sources[]`、`audience`、`token_budget` 与 `token_count`、`truncated` 与 `omitted_sources[]`、`valid_from` / `valid_until`、`approval_digest`、起草人、批准人与发布人、`approval_id`、发布时记下的 `authz_version` 与 `policy_version`、`state`。
- **W7.2** [KB-CTX-01/#996+新设] **受众**：`audience.kind` 取 `personal`（只给发布主体本人）、`org`（一个已验证组织的成员）、`group`（组织内的指定子集）之一；v1 支持哪几种、谁可以设定哪种受众，待 #996（§11 #A6）。受众在**每次加载时**由服务端按当前成员资格判定，不认客户端自报（W1.6）。`org` 与 `group` 需要已验证的组织身份（KB-ORG-01、#992）。
- **W7.3** [KB-CTX-01/R4.6/R4.7+新设] **来源与摘录**：`sources[]` 每项为 `{knowledge_base_id, knowledge_id, chunk_id, source_revision, index_build_id, chunk_revision}`，只能取自以带 `chunk_id` 的单分块 read 校验通过的分块（R4.7）；起草时按 R4.6 判定起草人、批准时判定批准人（W7.15）、发布时判定发布人有权读取全部来源。**`pack.read`** 对一般主体沿用 W7.9 的顺序：受众在此刻不包含读取者、或 `org`/`group` 包缺已验证组织身份时，按 R2.6 得 `not_found`（只套用 W7.9 候选集的受众与组织身份条件，不套用版本条件）；任何曾发布的版本（`published`、`superseded`、`revoked`、`expired`、`invalidated`）都按 W7.9 ① 起判；① 不成立只回 `scope_mismatch`，不回任何底层 `reason`；① 成立后可得状态、版本与 `reason` 等元数据；只有通过 ①–④ 的 `published` 版本才返回 `content`。**另列**：草稿只对起草人（按 R4.6 复验其**当前**读权）与在**该 pack 所在范围**内持有 `pack.approve` 且能读全部来源的主体（同 W7.15）可读，其他主体 `not_found`；某版本的起草人、批准人与发布人可以读该版本的元数据，读元数据同样先过 ①，① 不成立只回 `scope_mismatch`，读 `content` 时同样要求当前能读全部来源。`content` 只能是这些分块正文的**原文摘录**：每段摘录是来源分块里连续的一个或多个完整句子（句子边界按 Unicode UAX #29），不得在句内删字；段与段之间省略的内容必须用可见的省略标记"……"表示。核对按**最长匹配优先**：先把尽可能长的一段（含其中的"……"）当作来源原文去匹配，匹配不上才把"……"视为段间省略，所以来源句本身含"……"不会被误拒。服务端发布时逐段核对摘录出自记下修订的分块正文，不符返回 `invalid_request`（`excerpt_mismatch`）；需要撰写的内容先作为 SOP 经批准写入 KB，再从中摘录（§11 #B15）；这条路径是有意设计的降密点，见 W8.1。
- **W7.4** [KB-CTX-01+新设] **授权版本**：发布时记下起草人、批准人与发布人的 `authz_version`。加载时复验发布人：发布人失去对任一来源 KB 的读取或失去 `pack.publish` 时，包失效（W7.9 ④，§11 #B10）。
- **W7.5** [KB-CTX-01+新设] **有效期**：`valid_until` 必填，距发布不超过 90 天（§11 #B12 已定），超过上限拒绝发布（`invalid_request/validity_too_long`）。`valid_until` 必须晚于发布时刻，给了 `valid_from` 时还必须晚于 `valid_from`，否则拒绝发布（`invalid_request/validity_invalid`）。`valid_from` 可选，缺省为发布时刻；`approval_digest` 按提交时的值计算（缺省即 `null`），发布时刻不进摘要。有效区间为 [`valid_from`, `valid_until`)：当前时间等于 `valid_until` 即已过期（P04 与 W7.9 ② 同此）。不自动续期，过期后不加载，续用只能发布新版本。
- **W7.6** [KB-CTX-01+新设] **截断**：`token_budget` 在起草时给定（方案的 1–2k tokens 是待验证预算）。截断只发生在发布之前：服务端发布时计算 `token_count`（标明计数方法，§11 #C8），超过预算拒绝发布（`invalid_request/too_large`）；起草人为了预算放弃的来源列进 `omitted_sources[]`，这时 `truncated: true`。`omitted_sources` 是起草人的声明，服务端只校验格式与读权（起草人无权读取的项拒绝，`forbidden`），不核验是否完整。加载时不再截断：注入预算放不下整个包时，该包 `skipped`（`over_budget`），因为截断会改变批准过的内容（§11 #B13 已定）。
- **W7.7** [D4/KB-CTX-01] 包内容进入模型上下文时是带 Knowledge 来源标识的数据，不获得系统指令优先级；包裹格式归 KB-CTX-02、KB-C1-05。

### 7.2 状态与转移
- **W7.8** [KB-CTX-01+新设] 状态：`draft`、`published`、`superseded`、`revoked`、`expired`、`invalidated`（带 `reason`）、`discarded`。只有 `published` 可被加载。**过期与失效的持久化**：`pack.load` 或 `pack.read` 首次发现某个 `published` 版本已过期或失效时，把它持久记为 `expired` / `invalidated`（这是 `pack.read` 唯一的写入），之后不再恢复为 `published`（即使发布人的读权后来恢复）；所以不论此前是否被加载过，后来的读取与加载看到的都是同一个状态。

| ID | 前置 | 事件 | 后置 | 依据 |
|---|---|---|---|---|
| P01 | — / 已有版本 | `pack.draft`（新建或修改） | 新版本 `draft`，`pack_version` 大于该 `pack_id` 的所有已有版本；草稿不可原地修改，每次修改都产生新版本，旧草稿保持原样 | [KB-CTX-01] |
| P02 | `draft` | `pack.publish`，W7.15 的检查全部通过 | `published`；同一 `pack_id` 之前的 `published` 版本在同一事务里变为 `superseded`（W7.16） | [KB-CTX-01] |
| P03 | `published` | `pack.revoke` | `revoked` | [KB-CTX-01] |
| P04 | `published` | 加载或读取时发现当前时间已到达 `valid_until`（含等于） | `expired`，持久化（W7.8） | [KB-CTX-01] |
| P05 | `published` | 加载或读取时发现来源变化、来源撤回或删除、发布人授权失效（W7.9 ③④） | `invalidated`（`source_changed` / `source_withdrawn` / `publisher_revoked`），持久化（W7.8） | [KB-CTX-01+新设] |
| P06 | `draft` | `pack.discard` | `discarded`；记审计 `knowledge.pack.discard`（代替 `knowledge.pack.transition`，不重复记） | [新设] |

  `revoked`、`expired`、`invalidated` 都不能恢复为 `published`，只能起草并发布新版本。每次转移记审计 `knowledge.pack.transition`。

### 7.3 加载
- **W7.9** [KB-CTX-01/KB-CTX-02/#1005+新设] **每次加载服务端复验**。请求由宿主的注入入口经 Provider 发出，对该 run 绑定集合（S4.7）里每个可服务的 binding 各发一次加载请求，各用该 binding 当前的 generation；结果按 binding 分开（S4.9）。`injection_budget_tokens` 每个请求都带整份注入预算。请求带 `model_destination`（该 run 将使用的模型目的地；这是客户端声明，服务端只能据此判定政策，声明属实由宿主负责）。
  - **候选集**：每个 `pack_id` 中**版本号最大的、曾经发布过的版本**（状态为 `published`、`expired`、`invalidated`、`revoked` 或 `superseded`；`draft` 与 `discarded` 不参与比较），只有它的状态是 `published`、`expired` 或 `invalidated` 时才进入，受众在**此刻**包含请求主体；`org` / `group` 包另要求请求主体有已验证的组织身份。所以放弃一个新草稿不影响旧版本，过期、失效的包每次加载都同样报告。不在候选集里的包**一律不列出、不给 reason**（不泄露存在）。没有已验证组织身份时，加载结果另带 `details.org_identity_missing: true`，不逐包回报（#1005：组织身份缺失时只加载个人包）。
  - 对候选集里的每个包按顺序取第一个不成立的检查：① 请求主体当前的有效范围覆盖全部来源 KB，否则 `scope_mismatch`（§11 #A7 已定；放在最前，范围外的成员看不到来源的变化）；② 状态已是 `expired` 或 `invalidated` 的，按原 `reason` 跳过；当前时间早于 `valid_from` 为 `not_yet_valid`，不早于 `valid_until` 为 `expired`；③ **逐来源**：文档存在、未撤回（W8.9），否则 `invalidated/source_withdrawn`；当前源修订、活动构建与记下的 `source_revision`、`index_build_id` 都相同，`chunk_id` 仍在该活动构建里、启用、类型为 `text`，且 `chunk_revision` 相同，否则 `invalidated/source_changed`（任一来源不符则整个包失效，需要重新发布；§11 #B11 已定）；④ 按 W7.4 复验发布人，否则 `invalidated/publisher_revoked`；⑤ 处理政策允许把包内容发往请求里声明的模型目的地（`model_destination`，#994），否则 `policy_denied`；⑥ 剩余注入预算放得下整个包，否则 `over_budget`。服务端按整份预算判定的 `over_budget` 只是初判，最终是否计入由宿主按 W7.10 的全序裁定。全部成立为 `attached`。
- **W7.10** [KB-CTX-02/#1005+新设] **顺序与结果**：所有 binding 通过检查的包按一个确定的全序依次整包计入**同一份**注入预算：先按类别（`personal` → `org` → `group`，§11 #A11 已定），同类按 profile 的稳定标识字典序，再按 `pack_id` 字典序（§11 #B14、#B23）；放不下的包由宿主记为 `skipped/over_budget`，不截断。一次加载的结果是 `attached`（至少一个包）、`empty`（候选集为空）、`skipped`（候选集非空但都没通过，逐包给出 `reason`）或 `failed`（服务端错误，`code` 按 READONLY §6）；结果为 `attached` 时，未通过的包同样逐包带 `reason`。Off 时宿主不发加载请求，入口结果记 `skipped`（`knowledge_off`）。
- **W7.11** [S5.8/#1005+新设] fresh、续轮、恢复、审批继续、委派每次都重新加载，不跨调用缓存包内容（S5.8）。委派的子任务用自己的 run 重新加载，不继承父 run 已加载的正文，只能拿到包的引用（`pack_id`、`pack_version`）。
- **W7.12** [S4.6] 加载响应按 S4.6 接受，并在追加进上下文之前再查一次准入与 generation；Off 之后不注入。
- **W7.13** [R10.1/R10.3+新设] 每次加载记审计 `knowledge.pack.load`：请求主体、run、逐包的 `pack_id`、`pack_version`、结果与 `reason`；不含包内容。
- **W7.14** [R7.7/S5.6] 撤销、过期、失效都追不回已经注入的内容（R7.7）。之后的模型调用里，历史中已注入的包内容换成不含正文的占位（§11 #A9 已定，与 STATE §10 #4 一致），触发条件（§11 #B24）：某次加载结果里，该 (`pack_id`, `pack_version`) 被明示为 `expired` 或 `invalidated`（任何 `reason`），或该 `pack_id` 不再出现在结果里。后者覆盖撤销，也会覆盖受众移除，两者客户端无法区分（W7.9 候选集），一并占位。`scope_mismatch`、`policy_denied`、`over_budget`、`not_yet_valid`，以及被新版本取代且新版本 `attached`，都不触发占位。连接级的撤权、注销等事件对包内容同样适用 STATE S5.6 的占位（包内容也是带 `binding_id` 的 Knowledge 证据）。Off 时不加载，不处理历史（STATE §10 #4）。

### 7.4 批准与发布
- **W7.15** [KB-CTX-01/KB-SOP-01+新设] **两步**：① 批准人调用 `pack.approve`，服务端判定其持有该 `pack_id` 所在组织或个人范围的 `pack.approve`（与 `sop.approve` 是不同授予，持有后者不能批准包）、按 R4.6 **能读取全部来源**（否则 `forbidden`，不产生批准记录）、#996 的职责分离条件（服务端知道起草人，可以执行），生成 `purpose: "pack.publish"` 的批准记录，绑定 `pack_id`、`pack_version`、`approval_digest` 与 `policy_version`，有效期同 W8.4，单次使用；② 发布人调用 `pack.publish` 并**出示** `approval_id`（这种用途的批准由发布人消费；发布人与批准人能否是同一人由 #996 定，§11 #A4）。服务端按 W1.9 的顺序判定：发布人持有 `pack.publish` 且能读取全部来源（W1.9 ③，在回放与批准检查之前）→ 回放 → 批准记录存在、`purpose` 为 `pack.publish`、`pack_id` 与 `pack_version` 与本次发布一致、`active`（未使用、未过期）→ 批准人当前仍持有 `pack.approve` 且仍能读取全部来源 → `approval_digest` 一致 → `policy_version` 仍是当前版本（否则 `stale/policy_changed`）→ 来源按 W7.9 ③ 全部仍有效（否则 `stale`，`source_changed` / `source_withdrawn`）→ 摘录核对（W7.3）、有效期（W7.5 的上限与下界）、预算（W7.6）、版本顺序（W7.16）→ 批准使用与 P02 同一事务。**逐项的拒绝码与批准状态**：发布人缺 `pack.publish` 或缺来源读权 → `forbidden/operation_not_allowed`，批准不变（这一项在 ③ 判定，所以来源已变时无读权的发布人也只得到它）；批准记录不存在、`purpose` 或 `pack_id` / `pack_version` 不符、已使用或已过期 → `forbidden/approval_invalid`，批准不变；`approval_digest` 不符 → `forbidden/approval_invalid`，批准不变；批准人失去 `pack.approve` 或来源读权 → `forbidden/approval_invalid`，批准 `void`；政策变化 → `stale/policy_changed`，批准 `void`；来源变化或撤回 → `stale/source_changed` / `source_withdrawn`，批准 `void`；摘录核对失败（`invalid_request/excerpt_mismatch`）、有效期超限（`invalid_request/validity_too_long`）、有效期不合法（`invalid_request/validity_invalid`）、超过预算（`invalid_request/too_large`）→ 批准 `void`（批准一律作废，需要重新批准）；`version_superseded` → `stale`，批准 `void`。`approval_invalid` 只在批准记录属于调用方本人（调用方就是批准人）时在 `details.reason` 里区分原因：`approval_used`、`approval_expired`、`approval_void`、`approval_mismatch`（`purpose`、`pack_id`/`pack_version` 或 `approval_digest` 不符）；其他调用方不区分（§11 #B19 已定）；审计照常记 `internal_reason`。批准人可经 `approval.read` 查看自己批准的状态，经 `approval.void` 作废它。批准作废的情形都按 W3.2 的例外写幂等记录。`approval_digest` = SHA-256（RFC 8785 JCS 规范化后的 `{content, sources, audience, valid_from, valid_until, token_budget, omitted_sources}`）。用途不同的批准互不通用。
- **W7.16** [KB-CTX-01+新设] 同一 `pack_id` 的发布串行化：每个 `pack_id` 至多一个 `published`；要发布的版本号必须**大于该 `pack_id` 所有曾经发布过的版本**（含已 `revoked`、`superseded`、`expired`、`invalidated` 的），否则拒绝（`stale`，`version_superseded`）。旧版本不能在新版本之后重新发布；要恢复旧内容，以它为底起草新版本。

## 8. SOP 候选与批准

### 8.1 候选
- **W8.1** [KB-SOP-01/R4.6/R4.7+新设] **从已授权会话生成**：候选记下 `derived_from: {binding_id, run_id, citations[]}`，`citations` 每项是带修订三元组的分块引用，`chunk_revision` 不得为 `null`（R4.7）。候选由宿主生成；生成所用的 binding 已 `retired`、或不在该 run 的绑定集合里时，宿主不得生成（S1.6、S4.7）。请求批准时服务端按 R4.6 判定请求者当前有权读取每个引用来源，不满足按 R2.3、R2.6 拒绝。**降密点**：SOP 写入的目标 KB 的读者可以多于来源 KB 的读者，经批准写入后，再从中摘录进 ContextPack 也不受 W7.9 ① 对原来源的限制。这是有意的设计，靠人工批准把关；v1 默认不比较目标 KB 与来源 KB 的读者范围（§11 #A7）。
- **W8.2** [KB-SOP-01+新设] **不进可检索正式库**：候选只保存在宿主本地（生成它的设备），批准之前服务端没有候选记录（§11 #B16）；请求批准（`sop.approve`）时宿主连同该候选版本的正文、`file_name` 与规范化文件类型上送；服务端先按 W5.2 的同一口径检查格式与大小（不符为 `invalid_request/unsupported_format` 或 `too_large`），并限制同时处于 `active` 的批准数量为每个主体 20 条（工程值，可按实测调整，§11 #B22 已定），超过为 `forbidden/quota_exceeded`，再服务端计算 SHA-256 并与请求的 `content_sha256` 核对、校验 W8.1 的引用，把正文随批准记录保存。这份正文只供 W8.6 比对与审计，只有批准人本人可以经 `approval.read` 按 `approval_id` 读回（他人 `not_found`）；ContextPack 用途的批准经 `approval.read` 只回状态与 `approval_digest`，正文在对应的 pack 版本里，按 W7.3 读取；批准记录变为 `used` 或 `void` 后、或过期后，服务端在 1 小时内删除正文、只留 hash（过期由服务端后台清理，不依赖任何请求）[新设]。READONLY 的 search、read 永远不返回候选或批准记录里的正文，ContextPack 不能引用它们。v1 不支持批准人不是提出人、或跨设备审批，只能在候选所在的宿主上请求批准（§11 #A12 已定）。候选状态（W8.12）由宿主维护。
- **W8.3** [KB-SOP-01+新设] **可编辑、可撤回**：每次编辑产生新的 `candidate_version`（各版本不可变）。编辑或撤回已批准的候选时，宿主以批准人凭据调用 `approval.void`（按 `approval_id` 把批准记录置为 `void`，ContextPack 用途的批准同样适用；只有批准人本人可以作废，他人或不存在的 `approval_id` 按 R2.6 得 `not_found`；对已 `void` 的再作废返回 200 与 `already: true`）。**服务端只保证 hash 绑定**：作废之前，旧批准仍可用于提交**旧版本的字节**（它们确实被批准过），新字节一律不符；作废请求失败时批准最迟按 W8.4 的有效期过期。

### 8.2 批准记录与服务端重验
- **W8.4** [KB-SOP-01/KB-SOP-02/W1.6+新设] **批准记录**由批准人用自己的凭据发出的已认证请求产生（不是宿主转述，W1.6；是否必须由人在宿主界面确认见 §11 #A3），服务端生成 `approval_id` 并绑定：
  - **主体**：批准人 principal 与当时的 `authz_version`；`sop.commit`、`sop.withdraw` 用途的批准**只能由批准人本人消费**（由他人代为提交属 #996，§11 #A4）；`pack.publish` 用途的批准由发布人出示（W7.15）；
  - **目标**：`knowledge_base_id`，以及 `mode: "create"`（新建文档）或 `mode: "revise"`（修订已有文档，带 `knowledge_id`）。`revise` 的目标必须是未撤回的 `origin: "sop"` 文档：目标不存在或已撤回按 R2.6 得 `not_found`，存在但不是 SOP 文档得 `forbidden`（`operation_not_allowed`），都不产生批准记录；v1 不支持把人工文档转成 SOP 文档；
  - **内容 hash**：将要写入的原件字节的 `content_sha256`，必须等于所批准的 `candidate_version` 的内容；
  - **文件类型**：规范化文件类型（必填）与 `file_name`（可选）；提交时必须一致，批准绑定的是**字节加解读方式**（类型与文件名），同样的字节换一种类型会走不同的解析器；它不绑定分块边界，配置档变化后重新处理得到的分块可以不同（W5.13）；
  - **基础修订**：`revise` 时为目标文档当时的当前 `source_revision`；`create` 时为 `null`；
  - **用途**：`purpose`，取消费它的操作类名：`sop.commit`、`sop.withdraw`、`pack.publish`（W7.15）之一；`sop.withdraw` 用途需要绑定哪些项（显然不含内容 hash 与候选）随 §11 #A8 另定；
  - 另含 `candidate_id`、`candidate_version`、`policy_version`、`approved_at`、`valid_until`（1 小时，§11 #B17）。批准记录只能使用一次；状态为 `active`、`used`、`expired`、`void`。
- **W8.5** [#996+新设] 产生批准记录时，服务端判定批准人当前持有**与 `purpose` 对应的批准操作类**（`sop.commit`、`sop.withdraw` 对应 `sop.approve`，`pack.publish` 对应 `pack.approve`），以及 #996 定下、服务端看得见的职责分离条件（W2.4、§11 #A4）。
- **W8.6** [KB-SOP-02+新设] **`sop.commit` 的判定**，在 W1.9 的框架里按顺序（下文写作 W8.6-① 等，与 W1.9 的 ①–⑥ 无关）：凭据 → 形状（`idempotency_key` 必须等于 `approval_id`，W3.1）→ 提交人持有 `sop.commit`（`forbidden/operation_not_allowed`）→ 回放（同一 `approval_id` 已执行则返回同一个 job，先于"未使用"检查）→ W8.6-① 批准记录存在、属于提交人、`active`、未过期；W8.6-② `purpose` 为 `sop.commit`；W8.6-③ 批准人**当前**仍持有目标上的 `sop.approve`；W8.6-④ 提交字节的 SHA-256 等于批准的 `content_sha256`，规范化文件类型（以及批准里有的 `file_name`）与批准一致；W8.6-⑤a 目标（KB、`mode`、`knowledge_id`）与批准的目标一致；W8.6-⑤b `revise` 时目标文档仍存在、未撤回、仍是 `origin: "sop"`；W8.6-⑥ `policy_version` 仍是当前版本；W8.6-⑦ W5.2 的保存前检查；W8.6-⑧ `revise` 时目标文档的当前源修订等于批准的基础修订，`create` 时同一批准还没有产生过文档（W8.6-① 已要求 `active`，这一子句只是并发下的保险）；W8.6-⑨ 内容去重，只命中本 KB 中 `origin: "sop"`、未撤回、当前修订有活动构建的文档（W3.5），否则照常新建。
  - W8.6-①②④⑤a：`forbidden`（`approval_invalid`）；批准记录属于调用方本人时，`details.reason` 区分 `approval_used`、`approval_expired`、`approval_void`、`approval_mismatch`（`purpose`、内容 hash、文件类型或目标不符）；用别人的 `approval_id` 或不存在的记录，与不存在相同、不带 `reason`（§11 #B19 已定）；审计照常记 `internal_reason`；批准记录状态不变。W8.6-⑤b：`not_found`（与不存在相同，W8.9），批准 `void`（目标不会再回来）。W8.6-③：`forbidden`（`operation_not_allowed`），批准 `void`。W8.6-⑥：`stale`（`policy_changed`），批准 `void`。W8.6-⑦：`unsupported_format`、`too_large` 按 W5.2 拒绝，批准 `void`；`quota_exceeded`（配额是暂时的）返回 `forbidden/quota_exceeded`，批准保持 `active`，不写幂等记录。W8.6-⑧：`stale`（`base_revision_changed`，附 `current_revision`），批准 `void`（W8.7）。使批准作废的拒绝（W8.6-③⑤b⑥⑧，以及 ⑦ 中的格式与大小）写幂等记录（W3.2 例外），重试得到同一拒绝。
  - 宿主请求里的 `approved` 布尔或批准人姓名一律不参与判定（W1.6）。
  - W8.6-① 与 W8.6-⑧ 的判定、批准记录置为 `used` 与 J01 在**同一事务**里完成（W3.4）：同一基础修订上的两个批准并发提交，只有一个成功，另一个得 W8.6-⑧。
- **W8.7** [#1005+新设] **冲突不覆盖**：W8.6-⑧ 不成立时不写入任何东西，批准记录作废，候选变为 `conflicted`；创建人在目标的新当前修订上生成新的候选版本（新修订），重新走批准（§11 #B18）。目标文档里的人工修订保持当前。
- **W8.8** [KB-SOP-02] **复用 ingest**：`sop.commit` 建立的 job 操作类为 `sop.commit`、文档记为 `origin: "sop"`，按 §5 转移；SOP 文档只有在 job `indexed` 之后才可检索。job 被取消或失败后，批准仍是已使用；按 W5.13 显式重新处理不需要新批准（内容与修订未变）。`origin: "sop"` 的文档不能经 `ingest.submit` 修订（W5.1）。
- **W8.9** [#1005/R7.7+新设] **撤回 SOP 文档**（`sop.withdraw`）：目标是任何**未撤回的 `origin: "sop"` 文档**，不要求已发布（`create` 后 job 失败或被取消、从未可检索的 SOP 文档也能撤回）。是否需要 `purpose: "sop.withdraw"` 的批准待 #996（§11 #A8）。撤回与删除一样作用于在途工作：`saved` / `processing` 的 job 按 J10 取消（`document_withdrawn`），增强按 J17 停止（`document_withdrawn`），之后不再有 J04 激活。撤回后文档记为 `withdrawn`：分块不再被 search 返回，read 返回与不存在相同的 `not_found`；审计（`knowledge.sop.withdraw`）可观察；引用它的 ContextPack 在下次发布或加载时按 W7.9 ③ 得 `source_withdrawn`。原件不删除（§11 #A5 已定）。同一主体在保留期内对已撤回的文档再次撤回，返回 200 与 `details.already: true`（W3.1），其他主体得 `not_found`。撤回之后 v1 不接受对该文档的 `sop.commit`（revise）、重新处理或重新发布（都按不存在处理），要恢复只能用新的 `create` 模式批准建立新文档。撤回给 READONLY 的 search、read 增加了新的过滤条件，登记在 §11 #C2。
- **W8.10** [D4/#1005] SOP 内容被检索或注入时与其他知识一样是数据，不获得系统指令优先级。
- **W8.11** [R10.1/R10.3+新设] 审计事件：`knowledge.sop.approve`（含规范化文件类型）、`knowledge.approval.void`（两种用途）、`knowledge.sop.commit`（含 `internal_reason`）、`knowledge.sop.withdraw`；ContextPack 另有 `knowledge.pack.draft`、`knowledge.pack.approve`、`knowledge.pack.publish`、`knowledge.pack.revoke`、`knowledge.pack.discard`；记 `approval_id`、`candidate_version`、`content_sha256` 或 `approval_digest` 与基础修订，不含内容。

### 8.3 候选状态与转移
- **W8.12** [新设] 候选状态（宿主维护）：`draft`、`approved`、`committed`、`published`、`conflicted`、`withdrawn`、`retracted`。宿主通过查询批准记录状态得知过期与作废。

| ID | 前置 | 事件 | 后置 | 批准记录 | 依据 |
|---|---|---|---|---|---|
| V01 | — | 宿主生成候选（W8.1） | `draft`（版本 1） | — | [KB-SOP-01] |
| V02 | `draft` / `approved` | 编辑 | `draft`（新版本） | `approval.void` 成功后 `void`（W8.3） | [KB-SOP-01] |
| V03 | `draft` | 批准（W8.5 通过） | `approved` | `active` | [KB-SOP-01] |
| V04 | `approved` | 宿主查到批准已过期 | `draft` | `expired` | [新设] |
| V05 | `approved` | `sop.commit` 通过 W8.6 | `committed`（J01）；W8.6-⑨ 去重命中（已有可检索的 `origin: "sop"` 文档）时直接 `published` | `used` | [KB-SOP-02] |
| V06 | `approved` | `sop.commit` 在 W8.6-③⑤b⑥ 或 ⑦ 的格式与大小不成立 | `draft`；W8.6-⑤b（`revise` 目标已撤回或删除）时只能改以 `create` 模式重新请求批准 | `void` | [KB-SOP-02] |
| V07 | `approved` | `sop.commit` 在 W8.6-⑧ 不成立 | `conflicted` | `void` | [#1005] |
| V08 | `conflicted` | 在新当前修订上生成新版本 | `draft` | — | [#1005] |
| V09 | `committed` | job `indexed`（J04），且文档未被撤回 | `published` | — | [KB-SOP-02] |
| V10 | `committed` | job `failed` / `cancelled` | `committed`（job 状态如实显示；可按 W5.13 重新处理） | — | [KB-SOP-02] |
| V11 | `draft` / `approved` / `conflicted` | 创建人撤回 | `withdrawn` | `approval.void` 成功后 `void` | [KB-SOP-01] |
| V12 | `committed` / `published` | `sop.withdraw` | `retracted`；在途 job 取消、增强停止，此后 V09 不再发生 | — | [#1005] |
| V13 | `committed` / `published` | 编辑 | 生成新候选（`revise` 模式，基础修订为当前修订），原候选不变 | — | [新设] |
| V14 | `committed` | 创建人撤回 | 不作为候选操作；要停止就 `job.cancel`，要撤下文档就 `sop.withdraw`（V12） | — | [新设] |
| V15 | `approved` | `sop.commit` 在 W8.6-①②④⑤a 不成立 | `approved`；收到 `approval_invalid` 后宿主经 `approval.read` 查询批准状态（有 `details.reason` 时直接按它更新），按查到的 `expired` / `void` / `used` 更新候选 | 不变（或已因此前的请求变化） | [KB-SOP-02] |
| V16 | `retracted` | 任何操作 | 不变；恢复只能另起候选，以 `create` 模式建立新文档（W8.9） | — | [新设] |
| V17 | `approved` | `sop.commit` 在 W8.6-⑦ 因配额不足被拒 | `approved`，配额恢复后用同一批准重试 | 不变（`active`） | [新设] |

## 9. 验收：ContextPack 与 SOP

前提同 §4.1。KB-CTX-01、KB-CTX-02、KB-SOP-01、KB-SOP-02 按本表写测试。

| ID | 规则 | 构造 | 期望 |
|---|---|---|---|
| PT-01 | W7.3 | 起草时引用起草人无权读取的分块；引用 `chunk_revision` 为 `null` 的 hit；`content` 含来源分块里没有的文字；句内删字；省略处没有"……" | 都被拒绝，后三项为 `invalid_request/excerpt_mismatch` |
| PT-02a | W7.6 | 内容超过 `token_budget` 时发布 | `invalid_request/too_large` |
| PT-02b | W7.6 | 为预算放弃部分来源后发布 | 发布成功；被放弃的来源在 `omitted_sources`，`truncated: true` |
| PT-03 | W7.15、P01 | 批准 v3 后修改产生 v4，用 v3 的批准发布 v4；用 SOP 用途的批准发布 | 都 `forbidden/approval_invalid`；v3 的批准仍 `active` |
| PT-04 | P02、W7.16 | 发布 v2；并发发布 v2、v3；v3 发布后再发布 v2 | v1 `superseded`；任何时刻只有一个 `published`；最后一项 `stale/version_superseded` |
| PT-05 | W7.9 ②、P04 | 过了 `valid_until` 加载；早于 `valid_from` 加载 | `skipped/expired`；`skipped/not_yet_valid` |
| PT-06 | W7.9 候选集 | 受众外的主体加载；受众外且包已过期；受众外且包已失效 | 结果里都没有这个包，不给 reason |
| PT-07 | W7.9 候选集 | 没有已验证组织身份时加载 | 个人包 `attached`；组织包不出现，结果带 `org_identity_missing: true` |
| PT-08 | W7.9 ① | 受众内、但对某个来源 KB 无权的主体加载 | `skipped/scope_mismatch` |
| PT-09 | W7.9 ③、P05 | 来源分块被编辑（只改 `chunk_revision`）后加载 | `skipped`，包 `invalidated/source_changed` |
| PT-10 | W7.9 ④ | 撤销发布人对某个来源 KB 的读取后加载；撤销发布人的 `pack.publish` 后加载 | 都 `invalidated/publisher_revoked` |
| PT-11 | W7.9 ⑤ | 政策不允许发往该 run 的模型目的地 | `skipped/policy_denied`；未授权目的地零调用 |
| PT-12 | W7.9 ⑥、W7.10 | 注入预算放得下个人包、放不下组织包 | 个人包 `attached`，组织包 `skipped/over_budget`，不截断 |
| PT-13 | W7.10 | Off 时进入注入入口；候选集为空 | 前者不发请求、`skipped/knowledge_off`；后者 `empty` |
| PT-14 | W7.11 | fresh、续轮、恢复、审批继续、委派各触发一次 | 每次都发加载请求；子任务只拿到包引用，不继承正文 |
| PT-15 | W7.12 | 加载响应到达后、追加进上下文之前 Off | 不注入 |
| PT-16 | P03、W7.14 | 撤销后加载；检查审计 | 不再加载；审计有撤销与加载记录，不含包内容 |
| PT-17 | W7.7 | 包内容含"忽略之前的指令"一类文本 | 按数据注入，不改变系统指令（提示注入负例） |
| PT-18 | W7.9 ③ | 来源文档被删除后加载 | `invalidated/source_withdrawn` |
| PT-19 | W7.9 ③ | 来源修订被重新处理（新构建，旧 `chunk_id` 不再存在）后加载 | `invalidated/source_changed` |
| PT-20 | W7.15 | 只持有 `sop.approve` 的主体调用 `pack.approve` | `forbidden/operation_not_allowed` |
| PT-21 | W7.15 | 批准后来源分块被编辑，再发布；发布人无权读取某个来源；批准后来源已变且发布人无权读取某个来源 | 前者 `stale/source_changed`，批准 `void`；后者 `forbidden`，批准不变；都不发布；第三项 `forbidden/operation_not_allowed`，批准不变（读权在 ③ 判定，看不到来源变化） |
| PT-22 | W7.5 | `valid_until` 超过上限 | `invalid_request/validity_too_long` |
| PT-23 | P06 | 起草人 `pack.discard` 草稿后试图发布；重复 `pack.discard`；放弃已发布的版本；非起草人放弃 | 发布被拒，草稿 `discarded`；重复放弃 `already: true`；放弃已发布版本 `stale/version_not_draft`；非起草人放弃 `not_found` |
| PT-24 | W7.10 | 同一主体连续加载两次，候选集不变 | 两次的包顺序完全相同 |
| PT-25 | W7.15 | 无权读取某个来源的主体调用 `pack.approve`；无权读取来源、也不是起草人的主体读草稿 | 前者 `forbidden`，不产生批准记录；后者 `not_found` |
| PT-26 | W7.15 | 批准人 A 批准后，发布人 B 出示 A 的批准发布（样本授权允许）；出示针对另一个 `pack_id` 的批准；批准后政策版本改变再发布 | 依次成功；`approval_invalid`；`stale/policy_changed`，批准 `void` |
| PT-27 | W7.9 候选集 | 包过期后由主体 X 首次加载，之后主体 Y 加载；从未被加载的失效包被 `pack.read` | X、Y 都得 `skipped/expired`；读取显示 `invalidated` |
| PT-28 | W7.9 候选集 | v2 `published`，之后起草 v3 并放弃 | v2 仍 `attached` |
| PT-29 | W7.16 | v3 发布后撤销，再发布 v2 | `stale/version_superseded`；v2 不发布 |
| PT-30 | W7.3 | 受众内、能读全部来源的主体读 `superseded`、`revoked`、`expired`、`invalidated` 版本；起草人失去来源读权后读自己的草稿；在其他 pack 范围持有 `pack.approve` 的主体读草稿 | 前四者只有元数据、无 `content`；后两者 `not_found` |
| PT-31 | W7.15、W3.2 | 批准人失去 `pack.approve` 后发布，首个响应丢失后原样重试 | 两次都得 `forbidden/approval_invalid`（审计 `internal_reason` 为批准人失权）；批准 `void` |
| PT-32 | W7.15 | 批准人失去来源读权、`version_superseded` 后发布；发布人缺 `pack.publish` 时发布 | 前两者批准 `void`；后者批准不变 |
| PT-33 | W7.9 ①、W7.3 | 某来源 KB 范围外的受众成员，在包因来源变化失效后加载与 `pack.read`；受众外主体 `pack.read` | 前者都只得 `scope_mismatch`，看不到 `source_changed`；后者 `not_found` |
| PT-34 | W7.5、P04 | 当前时间恰好等于 `valid_until` 时加载 | `skipped/expired` |
| PT-35 | W7.8 | 包因发布人失去读权而失效（经 `pack.read` 首次发现），之后发布人恢复读权再加载 | 仍为 `invalidated/publisher_revoked`，不恢复 |
| PT-36 | W7.3 | 来源句本身含"……"时原样摘录 | 发布成功，不报 `excerpt_mismatch` |
| PT-37 | W7.6 | `omitted_sources` 含起草人无权读取的项 | `forbidden`，不发布 |
| PT-38 | W7.15、W3.2 | `pack.publish` 成功后首个响应丢失，原样重发 | 回放同一结果，批准仍为 `used`，不重复发布 |
| PT-39 | W7.15 | 摘录核对失败、有效期超限、有效期不合法、超过预算时发布 | 依次 `excerpt_mismatch`、`validity_too_long`、`validity_invalid`、`too_large`；批准都 `void` |
| PT-40 | W7.5 | `valid_until` 早于或等于发布时刻；给了 `valid_from` 且 `valid_until` 不晚于它 | 都 `invalid_request/validity_invalid`，不发布，上一个 `published` 版本不变 |
| PT-41 | W1.9 | 对 `draft`、`expired`、`invalidated` 版本 `pack.revoke`；对 `published`、`discarded` 版本 `pack.approve` | 前者 `stale/version_not_published`；后者 `stale/version_not_draft`；都不产生批准记录或状态变化 |
| PT-42 | W7.3 | 发布人被移出某个来源 KB 后，读自己发布、已因来源变化失效的版本的元数据 | `scope_mismatch`，看不到 `source_changed` |
| PT-43 | W7.10 | run 绑定两个 binding（profile p1、p2），各有 personal 与 org 包，总预算只放得下三个包 | 顺序为 p1 personal、p2 personal、p1 org、p2 org（按 profile 标识与 `pack_id`）；第四个由宿主记 `skipped/over_budget`；两次加载顺序相同 |
| PT-44 | W7.14 | 已注入 (p, v2) 后，分别出现：下次加载 v2 `expired`；v2 `invalidated/source_changed`；p 从结果中消失；v2 `skipped/over_budget`；v2 `skipped/policy_denied`；v3 发布并 `attached` | 前三种历史中的 v2 内容换成占位；后三种不占位（v3 的情形历史中的 v2 照旧） |
| AT-01 | W8.1 | 候选引用了请求者已失去读取权的来源，请求批准 | 拒绝，不产生批准记录 |
| AT-02 | W8.2 | 候选生成后、批准前查服务端；批准后用 search、read 与 ContextPack 起草去找候选正文；上送正文与 `content_sha256` 不符；批准使用后 1 小时读回正文 | 服务端没有候选记录；都找不到；第三项拒绝；最后一项正文已删、只剩 hash |
| AT-03 | W8.3、V02 | 批准后编辑候选：`approval.void` 成功后提交旧字节；未作废时提交新字节；未作废时提交旧字节 | 依次 `approval_invalid`（批准 `void`）、`approval_invalid`（hash 不符）、成功（服务端只保证 hash 绑定） |
| AT-04 | W8.4、W8.6 | 宿主请求带 `approved: true`，但没有批准记录 | `forbidden/approval_invalid`；审计记下被忽略的字段 |
| AT-05 | W8.6、W3.1 | 同一 `approval_id` 提交两次（含首个响应丢失）；`idempotency_key` 与 `approval_id` 不同 | 前者同一个 job，第二次不因"已使用"被拒；后者 `invalid_request/idempotency_key_mismatch` |
| AT-06 | W8.6-④ | 提交的字节与批准的 hash 不同 | `forbidden/approval_invalid`；批准仍 `active` |
| AT-07 | W8.6-⑤a | 用批准 kb-a 的记录写入 kb-b | `forbidden/approval_invalid`；批准仍 `active` |
| AT-08 | W8.6-③、V06 | 批准后撤销批准人的 `sop.approve` 再提交 | `forbidden/operation_not_allowed`；批准 `void`；候选回到 `draft` |
| AT-09 | W8.7、V07、V08 | 候选 X 以 r1 为基础修订获批后，另一条批准经 `sop.commit` 把目标修订为 r2，再提交 X | `stale/base_revision_changed`；不覆盖；X `conflicted`；在 r2 上生成新版本后可重新批准 |
| AT-10 | V04 | 过了批准的 `valid_until` 再提交；宿主随后查询批准状态 | `forbidden/approval_invalid`；查到 `expired`，候选回到 `draft` |
| AT-11 | W8.8、V09 | 正常提交 | job `saved` → `processing`（J03 复验 `sop.commit`）→ `indexed`；之后 search 能命中，候选 `published` |
| AT-12 | V10、W5.13 | SOP 的 job 被取消后显式重新处理 | 不需要新批准；新 job |
| AT-13 | W8.9、V12、W7.9 ③ | 撤回已发布的 SOP；随后加载引用它的 ContextPack | search 不再命中，read 得 `not_found`；包 `invalidated/source_withdrawn`；审计可观察 |
| AT-14 | W8.10 | SOP 内容含提示注入文本，被检索进主对话 | 按数据处理（提示注入负例） |
| AT-15 | V11、W8.3 | 创建人撤回已批准的候选：`approval.void` 成功；`approval.void` 失败 | 前者批准 `void`、之后提交被拒；后者批准在 1 小时内过期 |
| AT-16 | W5.1、W8.8 | 持有 `ingest.submit` 的主体带 `knowledge_id` 修订一份 `origin: "sop"` 的文档 | `forbidden/operation_not_allowed`；不保存 |
| AT-17 | W8.6、W3.4 | 同一基础修订上的两个不同批准并发 `sop.commit` | 只有一个建立新修订；另一个 `stale/base_revision_changed`，其批准 `void` |
| AT-18 | W8.6 | 非批准人用别人的 `approval_id` 提交；没有 `sop.commit` 的主体提交 | 前者 `approval_invalid`（与不存在相同）；后者在批准校验之前就得 `operation_not_allowed` |
| AT-19 | W8.6-②⑥⑦ | 用 `purpose: "pack.publish"` 的批准提交；提交前政策已变；提交的格式不受支持 | 依次 `approval_invalid`、`stale/policy_changed`、`unsupported_format`；后两者批准 `void` |
| AT-20 | W8.6、W3.1、W3.5 | `create` 模式的批准成功后，用同一 `approval_id` 作幂等键再提交；用另一幂等键提交；`revise` 时内容与当前修订相同 | 依次回放同一个 job；`invalid_request/idempotency_key_mismatch`；`outcome: "existing"`，批准记为已使用 |
| AT-21 | W8.5 | 没有 `sop.approve` 的主体请求批准 | `forbidden/operation_not_allowed`，不产生批准记录 |
| AT-22 | W8.6-⑨、W3.5、V05 | `create` 模式提交的内容等于本 KB 一份普通文档、一份 `failed` 的 SOP 文档、一份已撤回 SOP 文档的当前内容 | 三种都照常新建 SOP 文档与 job；候选 `committed`，不是 `published` |
| AT-23 | W8.4 | 对普通文档申请 `revise` 模式的批准 | `forbidden/operation_not_allowed`，不产生批准记录 |
| AT-24 | W5.14 | 对已发布 SOP 文档编辑分块、删除文档 | `forbidden/operation_not_allowed`；只能 `sop.withdraw` |
| AT-25 | W8.9、V16 | 撤回后对该文档提交 `revise` 批准、重新处理 | 都 `not_found`；新的 `create` 批准可以建立新文档 |
| AT-26 | W8.2 | 他人经 `approval.read` 读批准正文；批准过期后 1 小时 | 前者 `not_found`；后者正文已删、只剩 hash |
| AT-27 | W8.6、W3.2、V15 | W8.6-③ 使批准作废的那次提交响应丢失，原样重试 | 重试得到同一个 `forbidden/operation_not_allowed`；宿主查询到 `void` |
| AT-28 | W8.4、W8.6-④ | 批准的是 Markdown，用同样的字节以 `.html` 或 `.txt` 提交 | `forbidden/approval_invalid`；批准仍 `active` |
| AT-29 | W8.9、J10、J17、V12 | `revise` 的 r2 job 处理中撤回；增强进行中撤回；`create` 后 job 失败、从未可检索的 SOP 文档撤回 | job `cancelled/document_withdrawn`；增强 `stopped/document_withdrawn`；第三种撤回成功；之后都不会 J04 激活，候选不变成 `published` |
| AT-30 | W8.3 | 他人调用 `approval.void`；对不存在的 `approval_id` 调用；对已 `void` 的再调用；批准人作废自己的 ContextPack 批准后发布人出示它 | 前两者 `not_found`；第三项 200，`already: true`；最后一项 `approval_invalid` |
| AT-31 | W1.11、W5.14 | 对 SOP 文档调用上游的跨 KB 移动、更新手工知识、更新标题、禁用分块（经允许列表之外的凭据亦然） | 都拒绝；文档、标题与分块状态都不变 |
| AT-32 | W8.2 | 请求批准的正文超过大小上限或格式不受支持；同一主体活动批准数已达上限 | 依次 `too_large`、`unsupported_format`、`forbidden/quota_exceeded`；都不产生批准记录 |
| AT-33 | W8.4 | 对已撤回或不存在的文档申请 `revise` 批准 | `not_found`（不是 `forbidden`） |
| AT-34 | V13、V14、W8.9 | 已 `published` 的候选再编辑；`committed` 时由有权主体 `sop.withdraw`；无权主体 `sop.withdraw` | 生成 `revise` 模式的新候选，原候选不变；`retracted`；`forbidden/operation_not_allowed` |
| AT-35 | W8.6-⑤b、V06 | 批准 `revise` 后，目标文档被 `sop.withdraw`，再提交；首个响应丢失后原样重试 | 两次都 `not_found`；批准 `void`；候选回到 `draft`，只能改以 `create` 模式重新请求批准 |
| AT-36 | W8.9、W3.1 | 同一主体重复撤回；另一主体撤回同一已撤回文档 | 前者 200，`already: true`；后者 `not_found` |
| AT-37 | W8.6-⑦、V17 | 批准后配额已满再提交；配额恢复后用同一批准重试 | 前者 `forbidden/quota_exceeded`，批准仍 `active`，不写幂等记录；后者成功 |
| AT-38 | W8.6、W7.15 | 批准人用自己已过期、已使用、已作废的批准提交；用 hash 不符的字节提交；非批准人用别人的 `approval_id` 提交 | 前四种 `approval_invalid`，`details.reason` 依次为 `approval_expired`、`approval_used`、`approval_void`、`approval_mismatch`；最后一种 `approval_invalid` 不带 `reason` |

## 10. 现状证据（WeKnora `4b363d15`；Agent24 见 STATE 草案 §9、BRIDGE 草案 §9）

1. **上传入口**：文件、URL、手工三条入口都要求 `OwnedKBOrAdmin` 与 KB 写访问（`internal/router/routes_knowledge.go:73-75`；"KB 创建人或 Admin+"，`internal/router/rbac.go:445-450`），API key 需 `ingest` 能力且受 KB 白名单约束（`internal/types/tenant_api_key.go:86-91`）；角色只有 owner / admin / contributor / viewer 四级（`internal/types/tenant_member.go:19-32`）。服务层先拒绝视频（`internal/application/service/knowledge_create.go:48-51`）、FAQ 类 KB 拒绝文件（`:62-64`）、校验扩展名（`:74-77`），再算 hash（`:81`）、去重（`:90-117`）、查配额（`:121`），新记录为 `pending` / `disabled`（`:174-175`），先存文件（`:192`）再建记录（`:201`）；入队失败时把记录标为 `failed`（`:242`、`:260`，`markKnowledgeEnqueueFailed` 定义在 `:1153`），但仍返回已保存的知识且不报错（`:249`、`:267`）；任务 `MaxRetry(3)`（`:255`）。会改变文档或 KB 的其他上游写路由（`internal/router/routes_knowledge.go`）：删除分块与删除文档的全部分块（`:40`、`:42`）、编辑与回滚分块（`:44-45`）、增删与重新生成问题（`:51-53`）、重命名文件夹（`:80`）、清空 KB（`:83`，Admin）、删除文档（`:108`）、更新文档标题、描述与元数据（`:109`）、重新生成摘要（`:110`）、更新手工知识（`:111`）、图片信息更新（`:122`）、批量标签（`:130`）、批量重新处理（`:131`）、批量删除（`:132`）、移动到文件夹与跨 KB 移动（`:133-134`）、更新 KB（`:217`）、删除 KB（`:221`）、复制 KB（`:239`）；单个对象的路由大多要求"KB 创建人或 Admin+"；清空 KB（`:83`）要求 Admin，批量、移动与复制 KB（`:239`）只要求 Contributor+。编辑分块会同步重新 embedding（`internal/application/service/chunk.go:669-718`）；文档标题会拼进每条索引文本（`internal/application/service/knowledge_index_content.go:12-21`），元数据变化会排队刷新摘要（`internal/application/service/knowledge.go:820-825`，模型调用）。
2. **去重**：hash 是 MD5（`internal/application/service/knowledge_util.go:107-117`）；查询排除 `failed` 与 `deleting`（`internal/application/repository/knowledge.go:417`），按文件类型区分（`:433`）；来源连接器的提交另按 `datasource_id` 与 `external_id` 限定去重范围（`knowledge_create.go:97-103`，`repository/knowledge.go:422-425`）；命中时改写已有记录的 `created_at`（`knowledge_create.go:112`），handler 返回 409 并带回已有文档（`internal/handler/knowledge.go:208-221`）。`knowledges` 表只有普通索引，没有唯一约束（`migrations/versioned/000000_init.up.sql:122-125`），先查后插可并发重复。
3. **状态与取消**：`parse_status` 有 pending / processing / finalizing / completed / failed / deleting / cancelled（`internal/types/knowledge.go:44-70`），`finalizing` 表示已可做向量检索、但增强子任务仍在花费资源（`:49-56`）；取消"保留已写入的分块与索引"（`:66-70`）。`CancelKnowledgeParse`（`internal/application/service/knowledge_process.go:2914-3014`）：pending、processing、**finalizing** 都可取消，后者用来停止增强子任务的模型费用（`:2955-2958`），取消后同样记为 `cancelled`（`:2970-2976`），所以上游的 `cancelled` 可能对应一份仍可检索的文档；对 completed / failed 返回 400（`:2951-2952`），对 cancelled 幂等（`:2946-2950`），同步改状态并清零子任务计数（`:2970-2976`），出队是尽力而为、Lite 模式（无 Redis）只能靠检查点（`:2924-2926`、`:3001`）；没有费用报告。reparse 先把状态重置为 pending（`:2902-2911`）。
4. **检索不看文档状态**：`isSearchableChunk` 只看分块的 `is_enabled`、`index_status` 与类型（`internal/application/service/knowledgebase_search_results.go:361-378`）。
5. **计量**：embedding 用量只为 Langfuse 估算（rune 数 / 4，`internal/models/embedding/langfuse_wrapper.go:85-89`），`Knowledge` 结构没有费用或用量字段（`internal/types/knowledge.go:180-200` 一带）。
6. **建 KB**：handler 把请求体整体绑定为 `KnowledgeBase`（`internal/handler/knowledgebase.go:401-402`），其中 `id` 与 `creator_id` 都可由请求体给出（`internal/types/knowledgebase.go:61`、`:77`）；服务层只在 `id` 为空时生成 UUID（`internal/application/service/knowledgebase.go:125-127`），只在调用者是真实用户时用其 ID 覆盖 `creator_id`（`:139-141`），所以 API key 调用时请求体里的 `creator_id` 会保留下来，影响"KB 创建人或 Admin+"的判定；JWT 需 Contributor+，API key 需 `manage_kbs`（`internal/router/routes_knowledge.go:204`）。
7. **后台作业的身份**：任务载荷只有 `tenant_id`、知识与 KB ID 等，没有 principal（`internal/types/task.go:300-323`）；`ProcessDocument` 只按租户建执行上下文（`knowledge_process.go:3415`），不复验提交人的授权。
8. 上游没有 ContextPack、SOP 或内容批准的概念。现有的两种"批准"都与本文无关：MCP 工具审批（`internal/types/chat.go:264-267`、`internal/types/mcp.go:135`），以及组织加入与升级申请的审批（`internal/application/service/organization.go:703` 起的 `ReviewJoinRequest`，按路由级 Admin 守卫放行）。
9. **手工知识的草稿**（可能被拿来承载 SOP 候选，W8.2 不采用）：未指定状态时默认为草稿（`knowledge_create.go:759`），草稿以 `parse_status: "draft"` 入库、不处理不建索引（`:793`），发布才转为 `pending`（`:807`）；草稿与其他文档同在 KB 的知识列表里，该列表对 Viewer+ 开放（`internal/router/routes_knowledge.go:76`；列表是否过滤草稿未核实）。更新手工知识只把版本号加一，不比较调用方看到的版本（`knowledge_create.go:1043`），后写覆盖先写。
10. **Agent24**：RunManager 没有通用的 context hook，任务前只调用 `memory.recall`（STATE 草案 §9 第 2 条）；`RunCreate` 没有 principal 与 profile（第 4 条）。KB-CTX-02 的注入入口与 W8.1 的 `derived_from` 都依赖新增这些字段与入口（§11 #C5、#C6）。

## 11. 待决与需要评审确认的点

分类：**A** 对外行为或产品范围，留给 David（含一切依赖 #996 的）；**B** 契约语义，已盲选；**C** 内核接口与外部依赖，只登记。问题清单与选项见 `knowledge-materials/kb-ct-03c/questions.md`。

| # | 事项 | v1 默认 | 状态 |
|---|---|---|---|
| A1 | **§2 矩阵每一格**（含个人档是否沿用组织矩阵、职责的名称与授予方式；job 能否由同 KB 的其他成员读取与取消，W5.11） | 全部拒绝，只在样本租户测试（W2.2） | 待 #996；David 已定 v1 全部拒绝（第一片） |
| A2 | **v1 是否支持 URL 导入**（服务端代为抓取，是出口） | 服务端不收 URL；宿主在本机抓取后按文件提交（W5.1） | 已定（David）（第一片） |
| A3 | **模型（Tool 调用）能否发起写操作；批准类操作是否必须由人在宿主界面确认** | 只开放起草类：起草 ContextPack、生成与编辑 SOP 候选；其余与所有批准只由人发起（§2 表后说明） | 已定（David）；起草格仍待 #996（第一片） |
| A4 | **职责分离**：创建人能否批准自己的候选、发布人能否就是 ContextPack 的批准人；SOP 批准能否由批准人以外的人代为提交。SOP 侧"创建人不得自批"在服务端保存候选或创建人以自己凭据登记候选（#A12）之前**不能作为服务端保证**，只能由宿主执行；ContextPack 侧服务端可执行 | 写进矩阵条件（W2.4）；`sop.*` 用途的批准只能由批准人本人消费，`pack.publish` 用途的批准由发布人出示（W8.4、W7.15） | 待 #996；v1 随 #A1 拒绝（第一片） |
| A5 | **取消、失败、撤权后原件与部分产物是否删除**；撤回的 SOP 原件是否删除 | 都不删，如实报告（W5.9、W8.9） | 已定（David）（第一片） |
| A13 | **Off、切换、注销是否应取消在途 job** | 不取消；动作前提示并提供取消入口（W1.10） | 已定（David）（第一片） |
| A14 | **删除或清空 KB 时其中的 SOP 文档怎么处理**（一并撤回、迁移，还是拒绝） | 一并撤回其中未撤回的 SOP 文档，再删除或清空（W5.14） | 已定（David）（第二片） |
| A10 | **取消一次替换（新修订处理中取消）是否等于撤销这次替换**，即回到上一个源修订；若是，旧构建必须保留到新构建激活 | 不等于：当前源修订不回退（W5.4、W5.7） | 已定（David）（第二片） |
| A6 | **ContextPack 的受众**：v1 支持哪几种（`personal` / `org` / `group`）、谁可以设定哪种 | 只在样本租户测试（W2.2） | 待 #996；v1 随 #A1 拒绝（第三片） |
| A7 | **加载 ContextPack 是否要求加载者对全部来源有读取权**（不要求则组织包可以把受限文档的摘录带给无权的成员）；以及**降密点**：经批准把受限来源写成 SOP、再摘录进包，是否要求目标 KB 的读者是来源读者的子集（W8.1） | 要求，否则 `scope_mismatch`（W7.9 ①）；降密点不检查，靠人工批准把关（W8.1） | 已定（David）（第三片） |
| A8 | **撤回 SOP 文档是否需要批准**，由谁撤回，`sop.withdraw` 用途的批准绑定哪些项 | 矩阵"待 #996"，按 W2.2 拒绝（W8.9） | 待 #996；v1 随 #A1 拒绝（第三片） |
| A9 | **撤销、过期、失效的 ContextPack 已注入内容是否在后续模型调用里重发** | 历史中已注入的包内容换成占位，触发条件见 W7.14 | 已定（David）（第三片） |
| A11 | **ContextPack 加载时 `personal`、`org`、`group` 三类的先后** | 依次为 `personal`、`org`、`group`（W7.10） | 已定（David）（第三片） |
| A12 | **批准人不是提出人、或跨设备审批时，SOP 候选是否需要服务端保存**（也决定 #A4 的 SOP 职责分离能否由服务端执行） | 不支持，只能在候选所在宿主上请求批准（W8.2） | 已定（David）（第三片） |
| B0 | 所有标 `[新设]` 的规则 | 按本文 | 本 PR 评审 |
| B1 | `authz_version` 的粒度 | 每个"主体 × 资源"一个值，资源含组织级授予；可以保守地多变（W1.2） | 已定（盲选一致），含补充（第一片） |
| B2 | 后台阶段复验的内容 | 建立 job 的操作类 + 凭据未撤销 + 政策；撤销记录保留到相关 job 结束（W1.3） | 已定（盲选一致），含补充（第一片） |
| B3 | 幂等记录的保留时长 | 与 job 记录同寿命：至少保留到终态后 90 天，不产生 job 的写入后 90 天（W3.3） | 已定（David）（第一片） |
| B4 | 内容去重键 | KB + SHA-256 + 规范化文件类型（W3.5） | 已定（David）（第一片） |
| B5 | 去重命中的文档处于 `failed` / `cancelled` 时 | 返回 `existing`，不自动重新处理（W3.5） | 已定（David）（第一片） |
| B6 | `config_version` 是否进默认库唯一键 | 不进（W4.2） | 已定（盲选一致）（第一片） |
| B20 | job 与费用记录的保留期 | 至少 90 天，且不短于对应幂等记录（W3.3） | 已定（David）（第一片） |
| B21 | `chunk.edit` 的费用与政策：带 `expected_policy_version`、复验政策与预算、报告费用，还是 v1 只开放不产生外部调用的子操作 | 前者（W5.14） | 已定（David）（第二片） |
| B22 | 每个主体同时处于 `active` 的 SOP 批准数上限 | 每个主体 20 条，超过为 `forbidden/quota_exceeded`（W8.2） | 已定（David），工程值（第三片） |
| B23 | 多个 binding 时 ContextPack 的加载顺序与预算 | 单一预算、全序：类别 → profile 稳定标识 → `pack_id`，宿主整包裁定（W7.10） | 已定（Grok/Opus 交叉确认）（第二片） |
| B24 | 已注入包内容换成占位的触发条件 | 明示 `expired`/`invalidated` 或 `pack_id` 从结果消失；`scope_mismatch`、`policy_denied`、`over_budget`、`not_yet_valid`、被新版本取代不触发（W7.14） | 已定（Grok/Opus 交叉确认，讨论一轮后一致）（第二片） |
| B7 | 上游 `finalizing` 的映射 | `indexed` 加 `enrichment: "running"`、`cost.final: false`；增强可单独停止（W5.6、J15–J18） | 已定（David）（第二片） |
| B8 | 取消、失败后当前源修订是否回退 | 都不回退（W5.4、J05–J07）；同一修订的重新处理失败不撤下原活动构建（W5.13） | 已定（盲选一致）；r1 由"取消回退"改来（第二片） |
| B9 | 对 `indexed`（增强不在进行）、`failed`、`cancelled` 的 job 取消的响应 | 200，`cancel_effect: "none"`（J11） | 已定（盲选一致）（第二片） |
| B10 | 加载时如何复验 ContextPack 发布人 | 发布人失去对任一来源的读取或失去 `pack.publish` 时失效（W7.4） | 已定（盲选一致）（第三片） |
| B11 | 来源变化时 | 整个包失效（W7.9 ③） | 已定（David）（第三片） |
| B12 | ContextPack 有效期上限 | 90 天（W7.5） | 已定（David），90 天（第三片） |
| B13 | 截断发生在哪里 | 只在发布前；加载放不下就跳过（W7.6） | 已定（David）（第三片） |
| B14 | 加载顺序 | 先按类别、同类按 `pack_id` 的确定全序（W7.10）；类别先后见 #A11 | 已定（盲选一致）（第三片） |
| B15 | ContextPack 内容的形式 | 只能是来源分块的整句原文摘录，省略处用"……"（W7.3） | 已定（盲选一致）；r1 由"可撰写"改来（第三片） |
| B16 | SOP 候选保存在哪里 | 只在宿主本地，请求批准时连同正文上送，服务端校验正文与 hash（W8.2）；异人或跨设备审批见 #A12 | 已定（盲选一致）；r1 由"服务端候选区"改来（第三片） |
| B17 | 批准记录的有效期 | 1 小时（W8.4） | 已定（盲选一致）；r1 由 24 小时改来（第三片） |
| B18 | 基础修订冲突时 | 拒绝写入，候选在新修订上生成新版本并重新批准（W8.7） | 已定（盲选一致）（第三片） |
| B19 | 批准校验失败对调用方是否区分原因 | 统一 `approval_invalid`；批准属于调用方本人时 `details.reason` 区分原因（W8.6、W7.15）；目标已撤回或删除另为 `not_found`（W8.6-⑤b） | 已定（David）（第三片） |
| C1 | 服务端为写操作新增的端点、计量、幂等与唯一约束（KB-C4-04、KB-C4-02b） | 按本文语义实现 | WeKnora 实现（第一片） |
| C2 | **READONLY R2.7 需同步修订**：凭据的操作范围按操作类扩展到写操作；与 BRIDGE 草案 §10 #25 一并处理。另：取消后的文档级 read 暂按 `index_failed`，是否另设 `reason` 一并定；SOP 撤回后 search 不返回、read 返回 `not_found`（W8.9），是 READONLY 的新过滤条件，需要写进 R2.6 / R4.4。R2.6 原文只说文档，本文把"越界与不存在不可区分"扩到 job、批准记录与 ContextPack（W1.1、W1.9 ③）；新增的 `details.reason`（`version_not_draft`、`version_not_published`、`validity_invalid`、`approval_used`、`approval_expired`、`approval_void`、`approval_mismatch`、`kb_deleted`，以及 §3、§5 的 `policy_changed`、`base_revision_changed`、`unsupported_format`、`too_large`、`quota_exceeded`、`content_hash_mismatch`、`budget_exhausted`）按 READONLY §0.3 是兼容新增。 | 修订前写操作不实现 | READONLY 下一修订（第一片） |
| C3 | **依赖 STATE 草案 r14**（PR #1070–#1072）：S1.1、S1.3、S1.5（需扩展到写操作，W1.4）、S1.6、S1.10、S3.1（凭据无效类 T42、旧凭据复验，W1.4）、S4.5、S4.6（W1.4、W1.5、W7.12；S4.6 的空值回显待 STATE 修订后复核，见 #1071 的评审意见）、S4.7（run 绑定 binding 集合，W7.9、W8.1）、S4.9（多个 binding，W1.4、W1.10、W7.9、W7.10）、S5.3、S5.6 与 §10 #4（#A9、W7.14）、§6 本地结果、S5.8；S3.1 另需写明 `request_in_progress` 不触发就绪探测（W3.2）。另：STATE S5.10 提到"经 #1013③ 的接口"删除 KB，而本文 v1 没有删除或清空 KB 的操作类（W1.11），STATE 那一句在 v1 只对应宿主界面经允许列表之外的路由，需在 STATE 下一修订里对齐 | 按 STATE 草案 r14 | STATE 合入时逐条复核（第一片） |
| C4 | **依赖 BRIDGE 草案**：U1.3（session JWT 不含写操作）、U6.2（桥消息闭集）、U7.4（`approval` 等禁止键）；BRIDGE §10 #25 与本文 #C2 一并处理。BRIDGE 的 David 决定：票据兑换失败与 UI 路径族撤销复用 `credential_invalid`（#5、#20），与 MT-02 一致 | 按 BRIDGE 草案 r5 | BRIDGE 定稿时逐条复核（第一片） |
| C5 | **RunCreate 缺 principal、profile 字段**（STATE 草案 §9 第 4 条）；写操作的 run 归属与审计需要它们。**涉及内核接口，需知会 jason** | 不定实现 | KB-HOST-01（第一片） |
| C6 | **KB-CTX-02 的统一注入入口**：RunManager 现在没有 context hook（§10 第 10 条），fresh / 续轮 / 恢复 / 审批继续 / 委派要走同一入口。**涉及内核接口，需知会 jason** | 不定实现；v1 不自动注入（D4） | KB-CTX-02（第三片） |
| C7 | 组织身份与成员资格的来源（`org` / `group` 受众、W7.9 候选集） | 没有已验证组织身份时只加载个人包 | KB-ORG-01、#992（第三片） |
| C8 | `token_count` 的计数方法（服务端分词器，还是按目标模型）；注入预算由谁给出 | 服务端计数并标明方法；预算由 KB-CTX-02 给出 | KB-CTX-01、KB-CTX-02（第三片） |

## 变更记录
- r0（2026-10-17）：首稿，分两片。§0–§6：授权绑定与机制约束、授权矩阵结构（单元格待 #996）、幂等与去重、初始化、ingest 与 job、验收 MT / IT / JT；§7–§9：ContextPack、SOP 候选与批准、验收 PT / AT；§10 WeKnora 现状证据；§11。影响夹具：否（#1012 只覆盖只读）。
- r1（2026-10-17）：按 B 类盲选裁定修订。B8 改为取消与失败都不回退当前源修订；B15 改为只能原文摘录；B16 改为候选只在宿主本地；B17 改为批准有效期 1 小时；B1、B2 补充；B3–B5、B7、B11–B13、B19 标待 David（盲选分歧）；新增 #A10–#A12；SOP 转移编号改为 V01–V12。影响夹具：否。
- r2（2026-10-17）：按第 1 轮双审互评裁定修订。新增写操作判定顺序（W1.9，含同键在途、回放先于其余检查）与 Off / 切换 / 注销对在途 job 的 v1 默认（W1.10，#A13）；只有新建工作的操作要求 ingest 就绪（W1.4）；后台复验建立 job 的操作类（W1.3）；新增 `pack.approve` 操作类，下载原件一行标 v1 不开放；W2.4 写明 SOP 职责分离在服务端的限度；基础修订 compare-and-set 进 J01 事务（W3.4）；`sop.commit` 幂等键即 `approval_id`；初始化 `existing` 不改授权、归属取自凭据与成员资格（W4.1、W4.3、W4.5）；本地 `write_outcome`；验收 MT-10–MT-19、IT-07–IT-08；§10 更正入队失败、来源身份去重、建 KB 的 `creator_id`、组织申请审批、batch-download 行号。ingest 与 job 部分：增强阶段可单独停止（J15–J17，`cancel_effect`、`enrichment`）；每个源修订至多一个非终态 job，重新处理期间原活动构建保持、失败或取消不撤下，`ingest.reprocess` 带 `expected_policy_version`（W5.13）；`origin: "sop"` 的文档只能经 `sop.commit` 修订（W5.1）；部分产物按 `job_id` 隔离（W5.8）；入队失败与停在 `saved` 的转移（J05）；验收 JT-25–JT-34；§10 第 3 条补 finalizing 可取消。影响夹具：否。 ContextPack 与 SOP 部分：W7.9 改为先按受众与组织身份筛候选集，逐来源比对修订三元组、`chunk_id` 仍在活动构建、来源未撤回或删除（新 `reason` `source_withdrawn`、`not_yet_valid`）；发布时同样校验来源；ContextPack 批准与发布分两步（W7.15），`approval_digest` 按 JCS 规范化，发布串行化（W7.16）；摘录以整句为单位；W8.6 并入 W1.9 顺序、①⑧ 与批准使用同一事务、批准只能由批准人本人消费，补全失败时批准与候选的状态（V13–V15）；作废通知与 hash 绑定（W8.3）；批准正文的访问与删除（W8.2）；验收 PT-18–PT-24、AT-16–AT-21。影响夹具：否。
- r3 第一片（2026-10-17）：按第 2 轮双审互评裁定修订，改为三片。矩阵新增 `pack.read`、`sop.approval.read`、`document.delete`、`chunk.edit`；内容去重只比对可检索生命周期内的文档，`sop.commit` 的去重只命中可检索的 SOP 文档（W3.5）；使批准作废的拒绝也写幂等记录（W3.2）；`request_in_progress` 不触发就绪探测；job 与费用记录单独保留（W3.3，#B20）；待决写操作持久化、principal 变化时保持未知（W1.5）；W1.10 覆盖 `cost.final: false`；`degraded` 准入；初始化中间态返回 `request_in_progress`（W4.5）；reprocess 单列检查链（W1.9）；验收 MT-20–21、IT-09；§10 补删除与分块路由、更正行号。影响夹具：否。
- r3 第二片（2026-10-17）：增强占用修订、J04 与 J17 停旧增强、`stop_reason` 增 `failed` / `timeout` / `interrupted` 与 J18、`cost.final` 必然结束（W5.6、W5.7、W5.13）；`indexed` 构建被替换后不可检索；W5.8 改为"不再有工作"；`finalizing` 取消映射为 `indexed` + `stopped(user)`；删除与分块编辑对 SOP 文档按机制拒绝（W5.14）；已撤回文档不可修订与重新处理；J05 / J13 超时语义分开；验收 JT-20 改写、JT-35–41。影响夹具：否。
- r3 第三片（2026-10-17）：`pack.approve` 校验批准人的来源读权，定义草稿读取范围（W7.3、W7.15）；`pack.publish` 的批准由发布人出示、检查用途与目标并比较 `policy_version`、写明失败时的批准状态；`sop.*` 用途才限本人消费（W8.4）；`revise` 目标必须已是 SOP 文档；候选集改为各 `pack_id` 最新的非草稿版本，过期与失效每次都报告；加载请求带 `model_destination`；降密点写明并入 #A7（W8.1）；批准正文的读取与删除时限（W8.2）；撤回后不接受修订与重新处理（W8.9、V16）；V15 收到拒绝后查询批准状态；验收 AT-09、AT-20 改写，AT-22–27、PT-25–27。影响夹具：否。
- r4 第一片（2026-10-17）：按第 3 轮双审互评裁定修订。新增 W1.11：矩阵之外的上游写路由对 v1 凭据一律不开放，改变 SOP 文档正文、所在 KB 或存在与否的路由按机制拒绝；矩阵新增 `sop.approval.void`，`chunk.edit` 并入删除分块并改为新建工作的操作，`sop.withdraw` 不要求已发布，`pack.read` 只对通过检查的 `published` 版本返回正文；幂等例外加入 `pack.publish`，指纹含文件类型；初始化 `existing` 按 A 判定、创建后续签；job 只有发起人可读可取消；审计事件补全；§10 补全上游写路由；验收 MT-22–23、IT-10。影响夹具：否。
- r4 第二片（2026-10-17）：撤回与删除一样取消在途 job、停止增强（`document_withdrawn`）；J17 的同修订触发标为防御性，JT-36 改为故障注入；`chunk.edit` 带 `expected_policy_version` 并报告费用（#B21）；删除或清空含 SOP 的 KB 默认拒绝（#A14）；无活动构建时的文档级 read 按 J05–J10 分别说明；B7 备选说明补 J18 与 `cancelled` 保留构建的冲突；验收 JT-40 扩充、JT-42。影响夹具：否。
- r4 第三片（2026-10-17）：候选集取每个 `pack_id` 版本号最大的曾发布版本，W7.16 要求大于所有曾发布版本；草稿每次修改产生新版本；`pack.read` 正文范围与起草人读权复验；加载检查把范围放到最前；有效区间为左闭右开；批准绑定规范化文件类型；`sop.approve` 检查格式大小并限制活动批准数（#B22）；`sop.approval.void`；撤回目标为任何未撤回的 SOP 文档，V09、V12、V14 同步；已撤回目标一律 `not_found`；验收 PT-03、PT-21、AT-02 改写，PT-28–34、AT-28–34。影响夹具：否。
- r5 第一片（2026-10-17）：按第 4 轮双审互评裁定修订。矩阵新增 `pack.discard`；`approval.read` / `approval.void`（由 `sop.approval.*` 改名）按 `approval_id` 覆盖 SOP 与 ContextPack 两种用途；天然幂等操作重复执行返回 `already: true`（W3.1），`sop.withdraw` 的检查链（W1.9）；W1.11 的保护面扩到检索可见的内容；宿主待决写操作超过服务端保留期改记未知（W1.5）；`/files` 不开放；`pack.read` 沿用加载顺序；§10 补 `:80`、`:109`、`:130`、`:131`、`:217` 与标题进索引、元数据刷新摘要；W8.6 编号改写为 W8.6-①；验收 MT-06、MT-08、MT-20 扩充，MT-24–25。影响夹具：否。
- r5 第二片（2026-10-17）：重新处理不继承人工分块编辑，宿主须提示（W5.13）；批准绑定字节加解读方式、不绑定分块边界；重复删除返回 `already: true`；JT-40 改用上游非 v1 凭据验证 #A14 的默认；验收 JT-43。影响夹具：否。
- r5 第三片（2026-10-17）：W8.6-⑤ 拆为 ⑤a（目标不符，`approval_invalid`，批准不变）与 ⑤b（目标已撤回或删除，`not_found`，批准作废、写幂等记录），V06 同步；`pack.publish` 逐项写出拒绝码与批准状态（W7.15）；过期与失效在首次被加载或读取发现时持久化（W7.8、P04、P05）；`pack.discard`（P06）；批准数上限改为 `forbidden/quota_exceeded`；省略标记按最长匹配优先；`omitted_sources` 负例；验收 PT-23、PT-31、PT-33、AT-07、AT-30–32 改写，PT-35–39、AT-35–36。影响夹具：否。
- r6 第一片（2026-10-18）：按第 5 轮裁定与 David 的决定修订。目标解析（存在、在 E 内、未撤回、不在删除中）统一放在 W1.9 ③、返回同一个 `not_found`，pack 操作补检查链与 `stale/version_not_draft`、`version_not_published`；`pack.discard` 只限起草人；去重与 existing_job 不泄露他人 job；`job.cancel` 以 J11 为准；W1.11 限定未撤回的 SOP；矩阵"模型发起"列按 #A3；幂等记录与 job 同寿命（#B3）；去重键与失败命中（#B4、#B5）；对齐 STATE r14（多个 binding、凭据类响应）；验收 MT-23–MT-26。影响夹具：否。
- r6 第二片（2026-10-18）：URL 由宿主抓取后按文件提交（#A2）；取消不撤销替换（#A10）；`finalizing` 映射为 `indexed`（#B7）；原件与部分产物不删（#A5）；删除或清空 KB 时一并撤回 SOP（#A14）；JT-40 改写。影响夹具：否。
- r6 第三片（2026-10-18）：`pack.read` 只套用受众与组织身份条件，元数据也先过 ①；有效期下界；发布人读权移到 ③；批准原因按 b′ 区分（#B19）；配额不足不作废批准（V17）；多个 binding 的加载顺序与预算（#B23）；已注入包内容的占位触发（#A9、#B24）；对齐 STATE r14 的 run 绑定集合；验收 PT-21、PT-23、PT-30 改写，PT-40–PT-44、AT-37–AT-38。影响夹具：否。
- r7 第一片（2026-10-20）：按 PR #1073 评审及 #1074、#1075 落在本片的意见修订。#1073 F1（= #1075 M2）：W1.9 ③ 划出批准绑定的 `sop.commit` 目标，改由 W8.6-⑤b 在回放之后判定。#1075 M1（含 #1073 F5、F6）：W1.9 ③ 的批准记录可见只用于 `approval.read` / `approval.void` 的目标，`pack.publish` 出示的、`sop.commit` 使用的 `approval_id` 是参数，由 W7.15、W8.6-① 在回放之后判定；草稿可见性按操作分别定义（`pack.approve`、`pack.publish` 各看范围内的同名操作类），批准人读全部来源并入 ③；矩阵 `pack.approve` 行改为草稿不可见 `not_found`、可见而读不了全部来源 `forbidden`；W1.1 同步限定批准记录。#1074 F2（选 ①）：W1.11 删除或清空 KB 不再按机制拒绝，任何凭据下都先按 W5.14 撤回其中未撤回的 SOP 再执行，删去"不论经哪种凭据""将来开放时"。#1073 F3：W1.1 区分显式指定的目标 KB（`empty_scope`）与从资源解析出的 KB（`not_found`），MT-26 写明请求形状。#1073 F4：③ 的 `already: true` 例外扩到同一主体已受理的删除或撤回（含仍在删除中），MT-24 补一格。#1073 F7：#C3 注明 S4.6 待 STATE 修订后复核。#1073 F8：W3.7 改为"他人的幂等记录或范围外文档"。#1073 F9：术语表注明 `sop.candidate` 是宿主本地操作。影响夹具：否。
- r8 第二片（2026-10-20）：按 PR #1074 评审修订。#1074 F1：W5.13 不再另列检查链，改为"判定顺序见 W1.9"，写明目标解析（存在、在 E 内、未撤回、不在删除中）在 ③、统一 `not_found`；JT-39 加删除中文档的重新处理（`not_found`）。#1074 F2（本片部分，与 r7 第一片 W1.11 一致）：W5.14 删去"这两条上游路由对 v1 凭据不开放，本条是开放之后的语义"，改为不论经哪种凭据删除或清空 KB 都先撤回 SOP。#1074 F3：W5.9 写明 v1 `original_retained` 恒为 `true`、`partial_artifacts` 恒为 `retained_invisible`、`discarded` 不出现；W5.8 写明"丢弃"只指不接受迟到写入；JT-12、JT-36 同步。#1074 F4：新增 JT-44 覆盖 #B5。#1074 F5：W5.14 给出 `chunk.edit` 复验政策与预算的拒绝码（`forbidden/policy_denied`、`forbidden/budget_exhausted`），W1.9 ⑤ 的 `chunk.edit` 链补这一步，JT-42 补预算不足；#C2 的新增 `reason` 清单补 `policy_changed`、`base_revision_changed`、`unsupported_format`、`too_large`、`quota_exceeded`、`content_hash_mismatch`、`budget_exhausted`。#1074 F6：§11 #B23、#B24 改标"第二片"；W5.3 能停止 job 的事件补 J17。#1074 F7：W1.3 写明同一阶段边界多项不成立时按 ①②③ 取第一个；新增 JT-45。#1074 F8：§11 #B9 限定为 `indexed`（增强不在进行）、`failed`、`cancelled`，与 J11 一致。影响夹具：否。
