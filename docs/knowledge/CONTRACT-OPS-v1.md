# Knowledge 写操作语义契约 v1 · 规范核心（CONTRACT-OPS-v1）

> **状态**：规范核心 v1（草案），待 fork 侧维护者（David）评审；评审通过前不授权任何实现。
> **日期**：2026-10-20 · **依据**：`docs/knowledge/CONTRACT-READONLY-v1.md` r8（下称 READONLY，规则写作 R*n.m*）；`CONTRACT-STATE-v1.md`（STATE 规范核心 v1（同批），条款写作 S*n.m*）、`CONTRACT-UI-BRIDGE-v1.md`（UI-BRIDGE 规范核心 v1（同批），条款写作 U*n.m*），依赖登记在 §9· **Issue**：#1013 ③（KB-CT-03）。
> 由完整版 `ab/weknora-03c-ops`（#1075 的分支）压缩而来。本文只写对外行为与不变式；实现机制交给代码与测试证明（§11）。

## 0. 范围与边界

- **0.1 冻结什么**：Agent24（Provider 与宿主，下称"客户端"）与 WeKnora fork（下称"服务端"）之间**会改变服务端状态的操作**的对外语义：默认库初始化、ingest 与 job、文档删除与分块编辑、ContextPack 的起草 / 批准 / 发布 / 加载、SOP 候选 / 批准 / 写入 / 撤回，以及它们共用的授权、判定顺序、幂等、费用报告。不定 HTTP 路径、类型、存储结构、锁、键名与重试节奏；文中的 90 天、1 小时、20 条是已定工程值（§10）。
- **0.2 不管什么、交给谁**：
  - READONLY：凭据与签发、有效范围 E（R2.1）、修订三元组与活动构建（R5.1、R5.3）、错误信封与 `code` 闭集（READONLY §6）、generation 只回显（R7.5）、审计最小字段（R10.1）。本文直接引用，**不新增 `code`**，只新增 `details.reason` 取值（§6）。
  - STATE：binding、generation 何时递增、准入、作废、本地结果、run 绑定、证据占位。本文不新增 generation 递增事件，也不新增 STATE 的持久状态（§9）。
  - UI-BRIDGE：KnowledgeView 与消息桥（本文只要求它们不是写入口，W1.6）。
  - 业务决定：矩阵每一格、ContextPack 受众、职责分离、SOP 撤回是否需批准 → #996；处理目的地、费用上限与预算 → #994、KB-SVC-POLICY-01；费用承担 → #995；原件与产物的删除 → #997；组织身份 → KB-ORG-01、#992。
  - 实现任务（以测试证明细节，不约束本文）：服务端端点、计量、幂等记录、唯一约束与允许列表 → KB-C4-02b、KB-C4-04、KB-C1-01；Provider 准入、待决写操作、超时与预算 → KB-C1-04、KB-HOST-01；Lite 模式（无队列，靠检查点停止，语义同本文）→ KB-LITE-04；ContextPack 与注入入口 → KB-CTX-01、KB-CTX-02；SOP → KB-SOP-01、KB-SOP-02。
- **0.3 变更规则**：沿用 READONLY §0.3。操作类集合、判定顺序、job 状态集合、幂等作用域、批准绑定项、ContextPack 加载复验项是语义，改动升主版本；新增 `reason`、`cancel_effect`、费用 `kind`、失败原因算 v1 修订。客户端兜底：未知 job 状态按 `failed`；未知 `cancel_effect` 按 `none`；未知 `enrichment` 按 `running`（费用按未结束）；未知 `stop_reason` 按"已停止，原因未知"；未知费用 `kind` 展示为"其他费用"；以 `approval_` 开头的未知 `reason` 按 `approval_invalid`。
- **0.4 术语**：**写操作**＝§2 列出的操作类（`job.read`、`pack.read`、`approval.read` 一并列入以表达授权）。**新建工作的操作**＝会让服务端开始新处理并产生费用的 `init.default_kb`、`ingest.submit`、`ingest.reprocess`、`sop.commit`、`chunk.edit`。**job**＝一个源修订的一次处理尝试，带 `job_id` 与建立它的操作类。**阶段边界**＝下一步会产生外部调用或费用之前的检查点（开始解析、开始 embedding、激活构建、每个增强子任务开始前）。**增强**＝主索引之外的模型调用处理（摘要、问题生成、图谱、wiki 等）。**占用**＝job 处于 `saved`、`processing`，或 `indexed` 且 `enrichment: "running"`。**pack 范围**＝`pack_id` 所属的组织或个人范围。`authz_version` / `policy_version` / `config_version`＝权威授权 A、处理政策、配置档的不透明版本，只比较相等。

## 1. 授权与判定

- **W1.1** 每个写操作由服务端逐请求重新计算授权：principal 只取自凭据（R1.5），E 按 R2.1；操作类必须同时在凭据的操作范围内、且在 A 对目标所在范围的授予里。结果码见 W1.9 ③。
- **W1.2** 写操作的响应与审计回报当次判定用的 `authz_version`；客户端不得缓存它来跳过判定。粒度为"主体 × 资源"（资源含 KB 与组织级授予），服务端可以保守地多变，多变只会让依赖它的记录更早失效。
- **W1.3** **后台作业沿用授权**：job 记下建立时的操作类、principal、`credential_id`、目标 KB 与 `authz_version`、`policy_version`、`config_version`、generation（只记录）。每个阶段边界按序复验：① principal 当前仍在目标 KB 上持有**建立该 job 的操作类**；② 建立 job 的凭据未被撤销；③ 当前政策仍允许下一步的目的地与费用。主索引阶段 ①② 不成立 → `cancelled/authorization_revoked`；③ 不成立 → `failed`（`policy_denied` / `budget_exhausted`）；增强阶段不成立只停增强（`stop_reason` 同名）。多项不成立取序号最小者。**凭据过期不算撤销**；**续签替换旧凭据也不算撤销**（等同过期）；只有签发权威对该凭据或其续签后继的撤销使 ② 不成立。撤销事实须可判定到所有引用它的 job 终态且费用 `final`。
- **W1.4** **客户端准入**：Provider 只在 binding 可服务（含 `degraded`）**且**准入开放（STATE 术语：`persist_pending` 为假、无未解除的待复验、不在作废中）时发出写操作，否则按 STATE §6 给本地结果；新建工作的操作另要求最近一次就绪结果 `operations.ingest` 为 `ready`，否则本地拒绝 `not_ready`。本地拒绝都不发网络请求、不改变 STATE 状态。其余写操作（取消、读取、撤回、删除、放弃、作废、`pack.*`、`sop.approve`）不加 `operations.ingest` 条件。写请求带 `request_id` 与 `generation`（R7.2），多 binding 时只发往目标 KB 所属的 binding；写操作的响应不作为 STATE 服务端收窄的输入。
- **W1.5** **结果未知**：作废中止的在途写请求，服务端可能已执行；Provider 给调用方的本地 `cancelled` 必须带 `details.write_outcome: "unknown"`（有幂等键的附上键），界面不得声称"未保存"。宿主持久保存待决写操作（操作类、请求体、幂等键）直到得到确定结果；重启或 Tool 重试只能在**同一 binding**（同一 `binding_id`）内**用同一键与同一请求体**重发，**不得换新键自动重发**。该 binding 已退役、身份元组（S2.4：principal、端点、租户或实例）任一项已变，或待决时间超过服务端幂等保留期（W3.3）时，宿主不再重发，保持 `unknown` 并提示用户自行确认。
- **W1.6** **与业务决定无关的机制约束**：
  - 授权只来自 A 与凭据，不从 retrieve 能力、页面角色、上游 RBAC、组织名、License、激活状态或请求体字段（归属、创建人、`approved`、`role`）推导；这些字段被忽略并记审计。
  - 服务端不信任宿主转述的批准：批准只能是批准人用自己凭据发出的已认证请求（W8.4）。
  - KnowledgeView 与桥不是写入口：session JWT 调用任何写操作得 `unauthorized/credential_invalid`；桥消息是闭集，`approval`、`approve`、`decision` 是禁止键。
  - 宿主没有自己的 principal；引导身份只能做 `init.default_kb`，且只代表调用者。
  - 集成模式下 retrieve 级凭据不得取原件（下载、批量下载、`/files` 均不开放，R2.7）。
- **W1.7** 每个写操作与每次 job、ContextPack、批准状态变化各记一条审计（事件名见 §6）；字段按 R10.1，另加幂等键哈希、`job_id`、`authz_version`、`policy_version`、费用摘要；不含原件、正文、query、凭据（R10.3）。后台阶段的审计 principal 是建立 job 时的 principal（R10.4）。
- **W1.8** 取消、撤权、撤回、撤销都追不回已交付给模型或用户的内容与已发生的费用（R7.7）；界面与审计不得声称可以。
- **W1.9** **服务端判定顺序**（取第一个不成立的）：① 凭据 → `unauthorized`；② 请求形状 → `invalid_request`（缺字段、缺幂等键、hash 不符、修订文档缺 `expected_source_revision` 等）；③ 授权与目标解析，依次：(a) 操作类不在凭据操作范围、或调用者在**任何范围**都不持有该操作类 → `forbidden/operation_not_allowed`（与目标无关，先于解析目标）；请求显式指定的目标 KB 不在 E → `forbidden/empty_scope`；(b) **目标对调用者不可见** → `not_found`，不可见的各种情形（不存在、所在 KB 不在 E、已撤回、删除中、job 非本人发起、作为 `approval.read` / `approval.void` 目标的批准记录非本人、ContextPack 按 W7.3 不可见）响应完全相同，且先于 ④⑤，所以政策版本、格式、配额都不能用来区分它们；(c) 目标可见、但调用者在目标所在范围不持有该操作类 → `forbidden/operation_not_allowed`。④ 幂等回放（W3.2）；⑤ 操作特有检查（按 W5.1、W5.13、W5.14、W7.15、W8.6 所列顺序）；⑥ 执行。回放先于 ⑤：首个请求已执行后，政策变化或批准已用都不影响原样重试拿到首次结果；回放后于 ③：失去授权的调用者拿不到回放。被 ①–⑤ 拒绝的请求不写幂等记录，例外见 W3.2。
  - **存在性不可区分的补充**：草稿对 `pack.approve` / `pack.publish` 的"可见"本身就要求在 pack 范围内持有同名操作类且能读全部来源，所以 (c) 不会出现，持有他处授予或读不了来源都得 `not_found`。`sop.commit` 不在 ③ 解析目标文档（目标只取自批准记录），但目标 KB 上的授权仍在 ④ 之前判定（W8.6）；`pack.publish`、`sop.commit` 出示的 `approval_id` 是参数而非目标，在 ④ 之后判定。
  - **唯一例外**：同一主体在保留期内对**自己**已删除（含仍在删除中）、已撤回、已放弃、已作废、已撤销的对象重复执行天然幂等操作，返回 200 与 `details.already: true`（W3.1）。
- **W1.10** Off、取消选用或停用 profile、注销都**不取消**已建立的 job。注销只撤销该 binding 的长期登录态与 UI 会话族（U4.10、U4.11），**不撤销已签出的凭据 C**，因而不构成 W1.3 ② 的撤销，已建立 job 的授权不变。动作前若受影响 binding 有本宿主发起、未终态或 `cost.final` 为 `false` 的 job，宿主必须提示"有 n 个处理仍在进行、会继续产生费用"并提供先取消的入口（Off 汇总全部 binding）。
- **W1.11** **矩阵之外的上游写路由一律不开放**：v1 凭据只能执行 §2 的操作类，其他会改变状态的上游路由（含**删除与清空 KB**、移动、复制、批量操作、手工知识更新、标题与图片信息更新、重新生成摘要）对 v1 凭据返回 `forbidden/operation_not_allowed`。集成模式下，任何会改变**未撤回的 `origin: "sop"` 文档**检索可见内容、所在 KB 或存在与否的路由都按机制拒绝，例外只有 `sop.commit`、`sop.withdraw` 与同字节同解读方式的 `ingest.reprocess`。经 v1 之外的途径（如上游管理员路由）删除或清空 KB 时，服务端必须**先**按 W8.9 撤回其中未撤回的 SOP 文档（审计原因 `kb_deleted`），再执行（决定 #A14）。

## 2. 操作类与授权矩阵

- **W2.1** 操作类（闭集）：`init.default_kb`、`ingest.submit`、`ingest.reprocess`、`job.read`、`job.cancel`、`document.download_original`、`document.delete`、`chunk.edit`、`pack.draft`、`pack.discard`、`pack.read`、`pack.approve`、`pack.publish`、`pack.revoke`、`pack.load`、`sop.candidate`（宿主本地，不改变服务端状态）、`sop.approve`、`approval.read`、`approval.void`、`sop.commit`、`sop.withdraw`。主体类别：本机单用户、remote 个人、组织成员、组织中被授予特定职责者、组织管理员、引导身份、页面会话、模型发起。
- **W2.2** 每格由 #996 填"允许"、"允许，附条件"或"拒绝"，条件只能引用服务端可验证的事实；**决定之前全部按拒绝执行**（fail closed），开发测试只在批准的样本租户上用服务端配置的临时授权进行，不进对外构建。
- **W2.3** 与决定无关的固定格：引导身份只能 `init.default_kb`；页面会话一律不能；`document.download_original` 在 v1 对所有主体拒绝；`approval.read`、`approval.void` 只限批准人本人；`pack.load` 按受众（W7.9），由宿主注入入口发起；模型发起只能 `pack.draft` 与 `sop.candidate` 的生成和编辑，其余（含一切批准）只能由人在宿主界面发起——服务端分不出模型与用户，这一列由宿主执行（决定 #A3）。
- **W2.4** 职责分离属于 #996 条件。服务端只能执行它看得见的事实：ContextPack 的起草人与批准人可执行；SOP 候选只在宿主本地，"创建人不得自批"在 v1 不是服务端保证。

## 3. 幂等与去重

- **W3.1** 必须带 `idempotency_key`（≥128 bit 随机）：`init.default_kb`、`ingest.submit`、`ingest.reprocess`、`pack.draft`、`pack.approve`、`pack.publish`、`sop.approve`、`sop.commit`、`chunk.edit`；缺失为 `invalid_request/idempotency_key_missing`。`sop.commit` 的键必须等于所用 `approval_id`，否则 `invalid_request/idempotency_key_mismatch`。天然幂等、不要求键：`pack.revoke`、`pack.discard`、`sop.withdraw`、`document.delete`、`approval.void`（重复执行按 W1.9 例外返回 `already: true`，其他主体 `not_found`；`pack.revoke` 另外对能读到该版本且持有 `pack.revoke` 的其他主体也返回 `already: true`）；`job.cancel` 重复执行按 W5.9 返回 `cancel_effect`。
- **W3.2** 幂等作用域为 `(tenant, principal, 操作类, idempotency_key)`，并比对请求指纹（操作类、目标、`content_sha256`、影响结果的参数）：同键同指纹已完成 → 首次结果加 `details.replayed: true`，不新建、不重复计费；同键同指纹仍在执行 → `unavailable/request_in_progress`（带 `retry_after_ms`，不触发就绪探测）；同键不同指纹 → `invalid_request/idempotency_key_reused`。不同主体的同一键互不可见。**例外**：使批准作废的拒绝也写幂等记录，重试得到同一拒绝（W7.15、W8.6）。
- **W3.3** 幂等记录与 job / 费用记录至少保留到 job 终态后 90 天；不产生 job 的写入后 90 天；服务端重启不丢；过期后同键视为新请求。
- **W3.4** **原子性**：保存原件、建立源修订与 job、写幂等记录、内容去重判定、`expected_source_revision` 的比较并设置、`sop.commit` 的批准校验与使用，对外只有"全部完成"或"都未发生"；并发基于同一修订的提交只有一个成功，其余 `stale/base_revision_changed`；并发同键只执行一次。
- **W3.5** **内容去重**（跨请求，与幂等键无关）：键为同一 KB 内的 `content_sha256` 加规范化文件类型；只比对未撤回、未删除、不在删除中的文档；修订已有文档时只与它自己的当前源修订比较；`sop.commit` 只命中本 KB 中未撤回、当前修订有活动构建的 `origin: "sop"` 文档。命中时返回 200、`details.outcome: "existing"`，不新建修订与 job、不改动已有时间戳、写幂等记录；最近 job 由调用者发起时附该 job，否则只附文档级处理状态（`index_pending` / `indexed` / `index_failed`），不回 `job_id` 与 `cost`。命中的文档处于失败或取消时同样 `existing`，不自动重新处理。不跨 KB。
- **W3.6** 声明的 `content_sha256` 与服务端计算值不符 → `invalid_request/content_hash_mismatch`，不保存。
- **W3.7** 幂等、指纹与去重不得泄露他人记录或范围外文档：命中时按无命中处理。

## 4. 初始化（默认库）

- **W4.1** `init.default_kb` 为一个**归属**建立默认 KB 并绑定授权。归属只取自调用者凭据（个人）或服务端记录的已验证组织成员资格（组织，另需持有组织归属的 `init.default_kb`）；请求体自报的归属一律忽略。引导凭据不离开服务端。
- **W4.2** 每个归属至多一个默认 KB（服务端约束保证，不依赖先查后建）；`config_version` 不参与唯一性，配置档升级不新建默认库。
- **W4.3** 结果 `{knowledge_base_id, outcome: "created" | "existing", config_version, authz_version}`。`existing` **绝不创建或修改授权绑定**；可见性按 A（不按凭据范围）判定，A 中无授权 → `forbidden/operation_not_allowed`，不回报 `knowledge_base_id`；`config_version` 不同附 `details.config_mismatch: true`。`created` 后客户端经续签取得包含新库的凭据（续签不影响已有 job，W1.3）。
- **W4.4** 初始化、重试、重复激活或登录都不修改已有默认库的资料、人工修订与配置。
- **W4.5** 对外不可观察"有 KB 而无授权绑定"；未完成期间只有同一调用者的同键重试能继续完成，同一归属的其他请求得 `unavailable/request_in_progress`；超时未完成的由服务端回收，不触及已有资料。
- **W4.6** 初始化与就绪不互相等待：search 就绪不依赖默认库（R8.3）。

## 5. ingest 与 job

- **W5.1** `ingest.submit` 带 `scope`（只含目标 KB）、键、原件（文件或文本）、`content_sha256`、`file_name` 与规范化文件类型、`expected_policy_version`；修订已有文档另带 `knowledge_id` 与 `expected_source_revision`（不等于当前 → `stale/base_revision_changed`，附 `current_revision`）。`origin: "sop"` 文档只能经 `sop.commit` 修订，`ingest.submit` 对它 `forbidden/operation_not_allowed`。服务端不收 URL（决定 #A2）。⑤ 的顺序：政策版本 → 格式与大小 → 配额 → `origin` → 基础修订 → 去重。
- **W5.2** 保存前拒绝：格式不在白名单或不会产生 v1 范围的 `text` 分块（R3.8）→ `invalid_request/unsupported_format`；超过大小 → `invalid_request/too_large`；配额不足 → `forbidden/quota_exceeded`；都不保存、不建 job。
- **W5.3** **断开不是取消**：job 建立后，连接断开、客户端超时、generation 作废、Off、切换、注销都不影响它；只有 `job.cancel`、W1.3 复验、新修订替换、文档删除或撤回能停止它（READONLY R7.1 的断开即取消只适用于 search 与 read）。
- **W5.4** 每次被接受的提交分配新 `source_revision`，**立即**成为当前源修订，活动构建在激活前为 `null`；处理期间旧构建的 hit 只能是 `stale`（R5.3），**从不以 `fresh` 出现**。取消与失败都不回退当前源修订（决定 #A10、#B8）；恢复旧内容只能把旧原件重新提交为新修订。
- **W5.5** `ingest.submit`、`ingest.reprocess`、`chunk.edit` 的 `expected_policy_version` 与当前不同 → `stale/policy_changed`（附当前版本），不保存、不建 job、不发调用。
- **W5.6** job 状态闭集与原因枚举见 §6。`indexed` 表示本 job 的构建已激活且仍是该修订的活动构建时可检索；增强可能仍在进行（`cost.final: false`）。`saved`、`processing` 之外的状态不再变，`indexed` 只有 `enrichment` 会变。
- **W5.7** **可观察结果**：正常处理 `saved → processing → indexed`；入队失败、解析或依赖重试用尽、超过处理时限（含停在 `saved`）→ `failed`；中断后在恢复时限内继续则仍 `processing`，否则 `failed/interrupted`，job 不会永远停在非终态。增强子任务重试用尽、超过增强时限、或中断（含服务端重启）后未能在恢复时限内继续 → `enrichment: "stopped"`（`failed` / `timeout` / `interrupted`），job 仍 `indexed`、活动构建保留，`cost.final` 随后为 `true`；增强不会永远停在 `running`。同一文档新修订被接受 → 旧修订的占用 job `cancelled/superseded` 或增强 `stopped/superseded`；文档删除或撤回 → `cancelled/document_deleted|document_withdrawn` 或增强同名停止；凭据只是过期 → 不变。新构建激活时同一修订旧构建及其增强产物不再可检索、旧 job 增强停止。当前修订没有活动构建时，不带 `expected_revision` 的文档级 read：失败或取消后为 `unavailable/index_failed`，处理中为 `index_pending`，文档已删除或撤回为 `not_found`（R4.4）。
- **W5.8** 非 `indexed` job 写下的部分产物不得被检索、读取或成为活动构建；job 不再有工作（非占用）之后迟到的写入一律不接受；`indexed` 后增强阶段正常写下的产物保留。新 job 只认自己 `job_id` 的产物。
- **W5.9** **取消**：`job.cancel` 同步返回 `cancel_effect`：`saved` / `processing` → `cancelled`（job 变 `cancelled/user`）；增强进行中 → `enrichment_stopped`（仍 `indexed`，活动构建保留）；其他 → `none`（200，状态不变）。已发出的外部调用可能仍完成并计费。响应与 `job.read` 报告 `original_retained`（v1 恒为 `true`）、`partial_artifacts`（v1 恒为 `retained_invisible`）与 `cost`；取消、失败、撤权都不删除原件与产物（决定 #A5）。
- **W5.10** **费用如实**：每个 job 带 `cost: {final, items[{kind, destination, units, unit_type, measured}]}`；不知道填 `units: null` 加 `measured: "unknown"`，**不得填 0**；估算标 `estimated`；`final: true` 只在不再有任何在途调用与增强之后，增强离开 `running` 后必然最终变为 `true`；界面只有 `final: true` 且全部项为精确 0 才能说"没有产生费用"。金额若给出放在可选 `amount` 并注明价格表版本。
- **W5.11** `job.read` 按 `job_id`，或按调用者当前 principal 的 `(操作类, idempotency_key)` 查询；返回操作类、状态、`enrichment`、`knowledge_id`、`source_revision`、`index_build_id`、`attempt`、原因、`cost`、各状态时间戳。v1 默认只有发起人（仍须持有相应操作类）可读可取消，他人 `not_found`（决定 #A1）。文档删除后 job 与费用在 W3.3 的保留期内仍可查。
- **W5.12** job 状态与费用只是元数据，不作为知识证据进入模型上下文。
- **W5.13** **重新处理**：`ingest.reprocess` 带 `knowledge_id`、`source_revision`、键、`expected_policy_version`；⑤ 的顺序：政策版本 → 修订为当前（否则 `stale/base_revision_changed`）→ 占用。每个源修订至多一个占用中的 job，已有时返回 200、`details.outcome: "existing_job"`（附 job 规则同 W3.5）。原活动构建保持到新构建激活；新 job 失败或取消时原构建不撤下。新构建**不继承人工分块编辑**（新分块 `chunk_revision` 为 0），宿主在发起前必须提示"人工编辑将不会保留"。SOP 文档重新处理不需新批准。不做格式、配额、`origin` 与去重检查。
- **W5.14** **删除与分块编辑**：`document.delete` 触发删除后的停止（W5.7）；`chunk.edit`（编辑、回滚、删除、启用 / 禁用分块，增删与重新生成问题）⑤ 的顺序：政策版本 → `origin` → 当前政策的目的地（`forbidden/policy_denied`）与预算（`forbidden/budget_exhausted`），拒绝时零调用；成功响应带 W5.10 格式的 `cost`，并改变 `chunk_revision`（使引用它的 ContextPack 失效，W7.9）。两者对 `origin: "sop"` 文档一律 `forbidden/operation_not_allowed`。

## 6. 枚举

| 对象 | 取值 |
|---|---|
| job 状态 | `saved`、`processing`、`indexed`、`failed`、`cancelled` |
| `failure_reason` | `parse_failed`、`embedding_failed`、`index_failed`、`policy_denied`、`budget_exhausted`、`storage_failed`、`enqueue_failed`、`interrupted`、`timeout`、`internal` |
| `cancel_reason` | `user`、`authorization_revoked`、`superseded`、`document_deleted`、`document_withdrawn` |
| `enrichment` / `stop_reason` | `none`、`running`、`done`、`stopped`；`stop_reason`：`user`、`authorization_revoked`、`policy_denied`、`budget_exhausted`、`superseded`、`document_deleted`、`document_withdrawn`、`failed`、`timeout`、`interrupted` |
| `cancel_effect` / `partial_artifacts` | `cancelled`、`enrichment_stopped`、`none`；`retained_invisible`、`discarded`（v1 不出现） |
| 费用 | `kind`：`storage`、`parse`、`embedding`、`multimodal`、`llm_enrichment`；`unit_type`：`bytes`、`pages`、`tokens`、`calls`、`images`；`measured`：`exact`、`estimated`、`unknown` |
| `details.outcome` | `created`、`existing`、`existing_job` |
| ContextPack 状态 | `draft`、`published`、`superseded`、`revoked`、`expired`、`invalidated`（`source_changed` / `source_withdrawn` / `publisher_revoked`）、`discarded` |
| 加载逐包 `reason` / 结果 | `scope_mismatch`、`expired`、`invalidated`、`not_yet_valid`、`policy_denied`、`over_budget`；结果 `attached`、`empty`、`skipped`（宿主另有 `knowledge_off`）、`failed` |
| 批准记录 | 状态 `active`、`used`、`expired`、`void`；`purpose`：`sop.commit`、`sop.withdraw`、`pack.publish` |
| SOP 候选（宿主） | `draft`、`approved`、`committed`、`published`、`conflicted`、`withdrawn`、`retracted` |
| `invalid_request` 新增 reason | `idempotency_key_missing`、`idempotency_key_mismatch`、`idempotency_key_reused`、`content_hash_mismatch`、`unsupported_format`、`too_large`、`excerpt_mismatch`、`validity_too_long`、`validity_invalid` |
| `forbidden` 新增 reason | `quota_exceeded`、`budget_exhausted`、`scope_mismatch`、`approval_invalid`（本人记录可细分为 `approval_used` / `approval_expired` / `approval_void` / `approval_mismatch`）、`omitted_source_unreadable`；沿用 `empty_scope`、`operation_not_allowed`、`policy_denied` |
| `stale` 新增 reason | `policy_changed`、`base_revision_changed`、`source_changed`、`source_withdrawn`、`version_superseded`、`version_not_draft`、`version_not_published` |
| `unavailable` 新增 reason | `request_in_progress`（可重试，带 `retry_after_ms`） |
| `not_found` | 不带 `reason`（R2.6 同码、同 message） |
| 审计事件 | `knowledge.init`、`.ingest.submit`、`.ingest.reprocess`、`.job.transition`、`.job.cancel`、`.document.delete`、`.chunk.edit`、`.pack.draft` / `.approve` / `.publish` / `.revoke` / `.discard` / `.transition` / `.load`、`.sop.approve` / `.commit` / `.withdraw`、`.approval.void`（前缀均为 `knowledge`）；`kb_deleted` 只作审计原因 |

## 7. ContextPack

- **W7.1** ContextPack 是经批准、带版本、体量很小的背景包：`pack_id`、`pack_version`（单调递增，发布后不可变）、`content`、`sources[]`、`audience`、`token_budget` / `token_count`、`truncated` / `omitted_sources[]`、`valid_from` / `valid_until`、`approval_digest`、起草人 / 批准人 / 发布人、`approval_id`、发布时的 `authz_version` 与 `policy_version`、`state`。
- **W7.2** 受众 `audience.kind` ∈ `personal` / `org` / `group`（v1 支持哪几种、谁能设定待 #996）；每次加载按当前成员资格判定，不认客户端自报；`org` / `group` 要求已验证组织身份。
- **W7.3** **来源、摘录与可见性**：`sources[]` 每项是带修订三元组的分块引用，只能取自校验通过的单分块 read（R4.7）；`pack.draft` 时服务端按 R4.6 复验起草人**当前**能读全部来源，任一读不了即按 R2.3 / R2.6 拒绝（同 W8.1），历史 read 成功不构成授权。`content` 只能是来源分块的**整句原文摘录**（句界按 UAX #29，不得句内删字，段间省略用"……"，按最长匹配优先核对），发布时不符 → `invalid_request/excerpt_mismatch`。**可见性**：曾发布的版本对 `pack.read`——受众此刻不含读取者或缺组织身份 → `not_found`；否则读取者的 E 不覆盖全部来源 KB → `forbidden/scope_mismatch`（不回任何其他原因）；通过后可得状态、版本与 `reason`，只有通过 W7.9 ①–④ 的 `published` 版本返回 `content`。起草人、批准人、发布人可读自己相关版本的元数据，同样先过 E 覆盖检查。草稿只对起草人（当前能读全部来源）与在 pack 范围内持有 `pack.approve` 且能读全部来源者可见；作为 `pack.approve` / `pack.publish` 的目标时，可见＝在 pack 范围内持有同名操作类且能读全部来源；`discarded` 版本只对起草人的重复放弃可见。不可见一律 `not_found`。
- **W7.4** 加载时复验发布人：失去任一来源 KB 的读取或失去 `pack.publish` → 失效（`publisher_revoked`）。
- **W7.5** `valid_until` 必填，晚于发布时刻与 `valid_from`（否则 `validity_invalid`），距发布不超过 90 天（否则 `validity_too_long`）；有效区间 `[valid_from, valid_until)`；不自动续期。
- **W7.6** 截断只在发布前：`token_count` 超预算 → `invalid_request/too_large`；为预算放弃的来源列进 `omitted_sources`（`truncated: true`），服务端只校验格式与起草人读权（无权 → `forbidden/omitted_source_unreadable`）。加载时不截断，放不下整包即 `over_budget`。
- **W7.7** 包内容进入模型上下文时是带 Knowledge 来源标识的数据，不获得系统指令优先级。
- **W7.8** 只有 `published` 可加载。草稿不可原地修改，每次修改产生新版本；`pack.discard` 只能放弃 `draft`（否则 `stale/version_not_draft`），只限起草人（他人 `not_found`）；`pack.approve` 只对 `draft`（否则 `stale/version_not_draft`）；`pack.revoke` 只对 `published`（已 `revoked` 为 `already`，其他 `stale/version_not_published`）。`pack.load` 或 `pack.read` 首次发现 `published` 版本过期或失效时持久记为 `expired` / `invalidated`（这是读取唯一的写入），此后对所有主体一致、永不恢复；`revoked` / `expired` / `invalidated` 只能发布新版本续用。
- **W7.9** **每次加载服务端复验**：宿主注入入口对 run 绑定的每个可服务且准入开放（同 W1.4）的 binding 各发一次请求（其余 binding 按 STATE §6 给本地结果、不发请求）（带当前 generation、整份注入预算、`model_destination`），结果按 binding 分开。**候选集**：每个 `pack_id` 中版本号最大的曾发布版本，仅当其状态为 `published` / `expired` / `invalidated` 且受众此刻包含请求主体（`org` / `group` 另要求组织身份）时进入；**不在候选集的包一律不列出、不给 reason**；缺组织身份时结果带 `details.org_identity_missing: true`。逐包取第一个不成立的：① 请求主体的 E 覆盖全部来源 KB（`scope_mismatch`，放最前以免泄露来源变化）；② 状态已过期 / 失效按原 `reason`，早于 `valid_from` 为 `not_yet_valid`，到达 `valid_until` 为 `expired`；③ 逐来源：文档存在且未撤回（否则 `invalidated/source_withdrawn`），当前源修订、活动构建、`chunk_id` 在构建内且启用、`chunk_revision` 都与记下的一致（否则 `invalidated/source_changed`，整包失效）；④ W7.4；⑤ 政策允许发往 `model_destination`（`policy_denied`）；⑥ 整份预算放得下该包（`over_budget`，逐包判定、不累计）。全部成立为 `attached`。
- **W7.10** 宿主把各 binding 通过的包按确定全序整包计入**同一份**预算：类别 `personal` → `org` → `group`，同类按 profile 稳定标识，再按 `pack_id`；放不下的由宿主记 `skipped/over_budget`。结果：`attached`（至少一个包，其余逐包带 `reason`）、`empty`（候选集为空）、`skipped`（候选非空但都未通过）、`failed`（服务端错误）；Off 时不发请求，记 `skipped/knowledge_off`。
- **W7.11** fresh、续轮、恢复、审批继续、委派每次都重新加载，不跨调用缓存正文；委派子任务只拿到包引用（`pack_id`、`pack_version`）。
- **W7.12** 加载响应按 STATE 的接受规则处理，追加进上下文之前再查准入与 generation；Off 之后不注入。
- **W7.13** 每次加载记审计（请求主体、run、逐包 `pack_id`、`pack_version`、结果与 `reason`），不含包内容。
- **W7.14** **已注入内容的占位**（决定 #A9）：之后的模型调用里，下列已注入版本换成不含正文的占位：(a) 某次结果点名该 `pack_id` 的 vN 为 `expired` / `invalidated` → 该 `pack_id` 已注入且版本号 ≤ N 的全部版本；(b) 某次结果中该 `pack_id` 以任何结果出现（含逐包跳过）、版本号为 N → 已注入且版本号 **< N** 的旧版本，不以 N 实际注入为前提（被取代的旧版本不再单独复验，一律不再重发；决定 #B24）；(c) 该 `pack_id` 不再出现在结果里（只按 `attached` / `empty` / `skipped` 判定，`failed` 或未发请求不算）→ 全部已注入版本。`scope_mismatch`、`policy_denied`、`over_budget`、`not_yet_valid` 只豁免 N 自身，不豁免 (b)。判断只依据该包所在 binding 按 W7.12 接受的加载结果，在组装本次模型请求之前执行；占位注明「已被新版取代」或「已过期 / 已失效」，按触发原因如实标注；已占位的不恢复。`failed` 或未发请求不构成新版或包消失的证据，不新增占位，也不恢复旧正文（未知变化不保证即时发现）。包内容作为证据记下**全部**来源 KB，STATE 的收窄、撤权与删除 KB 对其中任一来源生效即占位。
- **W7.15** **批准与发布两步**：`pack.approve` 判定（W1.9 ③）批准人在 pack 范围持有 `pack.approve`（与 `sop.approve` 是不同授予）且能读全部来源，以及 #996 的职责分离，产生 `purpose: "pack.publish"` 的批准记录（绑定 `pack_id`、`pack_version`、`approval_digest`、`policy_version`，有效期与单次使用同 W8.4）。`pack.publish` 由发布人**出示** `approval_id`；发布人不必是起草人或批准人。⑤ 的顺序与结果：批准记录存在、用途与 `pack_id` / `pack_version` 相符、`active` → 否则 `forbidden/approval_invalid`，批准不变；`approval_digest` 一致 → 否则同上；批准人仍持有 `pack.approve` 且能读全部来源 → 否则 `approval_invalid`，批准 `void`；`policy_version` 为当前 → 否则 `stale/policy_changed`，`void`；来源全部仍有效 → 否则 `stale/source_changed|source_withdrawn`，`void`；起草人仍能读全部 `omitted_sources` → 否则 `omitted_source_unreadable`，`void`；摘录、有效期、预算 → 对应 `invalid_request`，`void`；版本号大于该 `pack_id` 所有曾发布版本 → 否则 `stale/version_superseded`，`void`。成功时批准置 `used`，同一 `pack_id` 原 `published` 版本同时变 `superseded`。使批准作废的拒绝写幂等记录（W3.2）。**本人记录的细分**：调用者就是批准人时，`approval_invalid` 改写为 `approval_used` / `approval_expired` / `approval_void`（含本次因批准人失权而作废）/ `approval_mismatch`；他人或不存在的记录只给 `approval_invalid`（决定 #B19）。
- **W7.16** 每个 `pack_id` 任何时刻至多一个 `published`，同一 `pack_id` 的发布串行；旧版本不能在新版本之后重新发布，恢复旧内容只能以它为底起草新版本。

## 8. SOP 候选与批准

- **W8.1** 候选由宿主从已授权会话生成，记下 `derived_from: {binding_id, run_id, citations[]}`（引用带修订三元组，`chunk_revision` 非 `null`）；binding 已退役或不在该 run 绑定集合内时不得生成。请求批准时服务端按 R4.6 判定请求者当前能读每个引用来源，否则按 R2.3 / R2.6 拒绝。**降密点**：SOP 目标 KB 的读者可多于来源读者，写入后再摘录进 ContextPack 不受原来源限制；这是有意设计，靠人工批准把关（决定 #A7）。
- **W8.2** 候选只在宿主本地，批准前服务端没有候选记录。`sop.approve` 上送该候选版本的正文、`file_name` 与规范化文件类型；服务端按 W5.2 口径检查格式与大小，限制每主体同时 `active` 的批准 20 条（超过 `forbidden/quota_exceeded`），核对 `content_sha256` 与 W8.1 的引用，把正文随批准记录保存，只有批准人本人可经 `approval.read` 读回（ContextPack 用途只回状态与摘要）。批准记录变 `used` / `void` / `expired` 后 1 小时内服务端删除正文、只留 hash（不依赖任何请求）。search、read、ContextPack 永远拿不到候选或批准正文。v1 只能在候选所在宿主上由提出人请求批准（决定 #A12）。
- **W8.3** 每次编辑产生新的不可变 `candidate_version`。编辑或撤回已批准候选时宿主以批准人凭据调用 `approval.void`（只限批准人本人，他人或不存在 `not_found`，重复 `already: true`）。服务端只保证 hash 绑定：作废前旧批准仍可提交旧字节；作废失败时批准最迟按有效期过期。
- **W8.4** **批准记录**由批准人凭据发出的已认证请求产生，绑定：批准人 principal 与 `authz_version`；目标 KB 与 `mode`（`create`，或 `revise` 带 `knowledge_id`——目标须为未撤回的 `origin: "sop"` 文档，不可见为 `not_found`，存在但不是 SOP 为 `forbidden/operation_not_allowed`，均不产生记录）；`content_sha256`；规范化文件类型（必填）与 `file_name`（可选），即**字节加解读方式**，不绑定分块边界；基础修订（`revise` 为当时的当前修订，`create` 为 `null`）；`purpose`；`candidate_id`、`candidate_version`、`policy_version`、`valid_until`（1 小时）。单次使用。`sop.*` 用途只能由批准人本人消费，`pack.publish` 用途由发布人出示。
- **W8.5** 产生批准记录时，服务端判定批准人当前持有与 `purpose` 对应的批准操作类（`sop.*` → `sop.approve`，`pack.publish` → `pack.approve`）及服务端可见的职责分离条件。
- **W8.6** **`sop.commit` 的判定**：凭据 → 形状（键等于 `approval_id`）→ 持有 `sop.commit`（`forbidden/operation_not_allowed`；**不在此解析目标文档**）→ 目标 KB 授权：批准记录存在且属于提交人时，其目标 KB 须在 E 内（否则 `not_found`）且提交人在该 KB 上持有 `sop.commit`（否则 `forbidden/operation_not_allowed`），不成立时不回放、批准不变、不写幂等记录；记录不存在或非本人时跳过本步、交 ① 判定 → 回放（同一 `approval_id` 已执行返回同一 job）→ ① 批准记录存在、属于提交人、`active`；② `purpose` 为 `sop.commit`；③ 批准人当前仍持有目标上的 `sop.approve`；④ 字节 hash 与文件类型（及批准里的 `file_name`）一致；⑤a 目标 KB、`mode`、`knowledge_id` 与批准一致；⑤b `revise` 时目标文档存在、未撤回、不在删除中、仍是 SOP（只有这项目标存在性后置于回放，见完整版 AT-35；授权不后置）；⑥ `policy_version` 为当前；⑦ W5.2 检查；⑧ `revise` 时当前源修订等于基础修订；⑨ 去重（W3.5）。结果：①②④⑤a → `forbidden/approval_invalid`（本人记录按 W7.15 细分），批准不变；③ → `forbidden/operation_not_allowed`，`void`；⑤b → `not_found`（各种不可见情形相同），`void`；⑥ → `stale/policy_changed`，`void`；⑦ 格式或大小 → `void`，配额不足 → `forbidden/quota_exceeded`、批准保持 `active`、不写幂等记录；⑧ → `stale/base_revision_changed`（附 `current_revision`），`void`。使批准作废的拒绝写幂等记录（W3.2）。因为回放前的目标 KB 判定只对本人的批准记录进行、目标文档只在 ①–⑤a 之后检查，非批准人或伪造 `approval_id` 永远只得 `approval_invalid`，无法探测目标是否存在。
- **W8.7** **冲突不覆盖**：⑧ 不成立时不写入任何东西，候选变 `conflicted`，须在新当前修订上生成新版本重新批准；目标文档的人工修订保持当前。
- **W8.8** `sop.commit` 复用 ingest：job 操作类为 `sop.commit`、文档记 `origin: "sop"`，只有 `indexed` 后可检索；job 失败或取消后批准仍是已使用，按 W5.13 重新处理不需新批准。
- **W8.9** **撤回**（`sop.withdraw`）：目标是任何未撤回的 `origin: "sop"` 文档（不要求曾可检索；普通文档 → `forbidden/operation_not_allowed`）；是否需批准待 #996。撤回取消在途 job、停止增强（`document_withdrawn`），之后不再激活构建；文档不再被 search 返回，read 得 `not_found`；引用它的 ContextPack 下次发布或加载得 `source_withdrawn`；原件不删。撤回后对该文档的修订、重新处理一律按不存在处理，恢复只能用新的 `create` 批准建新文档。
- **W8.10** SOP 内容被检索或注入时是数据，不获得系统指令优先级。
- **W8.11** SOP 与批准审计记 `approval_id`、`candidate_version`、`content_sha256` 或 `approval_digest`、基础修订与规范化文件类型，不含内容。
- **W8.12** 候选状态由宿主维护（§6），可观察规则：批准成功 → `approved`；查到批准过期或作废 → 回 `draft`；`sop.commit` 成功 → `committed`（去重命中已可检索的 SOP 文档时直接 `published`），job `indexed` 且未撤回 → `published`；W8.6 ③⑤b⑥⑦（格式 / 大小）→ `draft`（⑤b 只能改用 `create`）；⑧ → `conflicted`；配额不足 → 保持 `approved`；收到 `approval_*` 拒绝后宿主经 `approval.read` 查状态再更新；编辑已提交的候选生成新的 `revise` 候选；`committed` / `published` 被撤回 → `retracted`（终态）。

## 9. 与 READONLY、STATE、UI-BRIDGE 的接口约定

- **I1（READONLY r8）**：E、凭据错误、`not_found` 的形状、信封、未知值兜底分别按 R2.1、READONLY §6、R2.6、R6.1、R6.2；R7.7 的"追不回"适用于全部写操作（W1.8）。需要 READONLY 下一修订配合（修订前不实现写操作）：R2.7 的凭据操作范围按 §2 操作类扩展到写操作；R2.6 的"越界与不存在不可区分"扩到 job、批准记录与 ContextPack（W1.9 ③）；SOP 撤回成为 search / read 的新过滤条件（W8.9）；READONLY §6 表对各 `code` 的 `reason` 约束（如 `invalid_request`"其余情况不带"、`stale` 只带 `anchor_missing`）只约束只读操作，写操作按本文 §6 携带新增 reason，`not_found` 仍禁止 reason（按 READONLY §0.3 属兼容新增，客户端忽略未知 reason）；取消后的文档级 read 暂按 `index_failed`。
- **I2（STATE 规范核心 v1（同批））**：写操作与加载的准入为可服务且准入开放（STATE §9 OPS 条；额外条件见 W1.4、W7.9），不满足按 STATE §6 给本地结果；作废中止写请求的本地 `cancelled` 由本文加 `write_outcome`（W1.5，STATE §9 委托）；写响应按 S4.6 接受、凭据类响应按 S3.1，服务端只回显 generation（R7.5），本文不新增递增事件；`request_in_progress` 不触发就绪探测（W3.2，STATE §9）；run 的 binding 集合决定加载与候选生成范围（S4.7；W7.9、W8.1）；多 binding 时写操作只发目标 binding、加载结果按 binding 分开（S4.9）。**STATE 需补**：ContextPack 证据记全部来源 KB，S5.6 的占位须对多 KB 证据生效（W7.14）；STATE 提到的"经本契约删除 KB"在 v1 不存在，只对应 W1.11 的非 v1 途径。
- **I3（UI-BRIDGE 规范核心 v1（同批））**：session JWT 的操作不含写操作，票据或会话凭据用于写操作按 `credential_invalid`；桥消息闭集与禁止键见 W1.6。
- **I4（内核接口，需知会 jason）**：写操作的 run 归属需要 RunCreate 带 principal 与 profile（KB-HOST-01）；自动注入需要统一注入入口（KB-CTX-02；v1 不自动注入）；`token_count` 的计数方法由 KB-CTX-01 / KB-CTX-02 定。

## 10. 已定决定（结论）

- #A1 矩阵每格待 #996；v1 全部拒绝，只在样本租户测试；job 只有发起人可读可取消。
- #A2 服务端不收 URL；宿主本机抓取后按文件提交。
- #A3 模型只能起草（ContextPack 草稿、SOP 候选），其余与所有批准由人在宿主界面发起。
- #A4 职责分离待 #996，写进矩阵条件；`sop.*` 批准只能本人消费，`pack.publish` 批准由发布人出示。若 #996 定"创建人不得自批"，在 #A12 放开前 v1 的 SOP 整体不可用。
- #A5 取消、失败、撤权、撤回都不删原件与产物，如实报告；删除功能归 #997。
- #A6 受众种类与设定权待 #996；v1 随 #A1 拒绝。
- #A7 加载要求加载者能读全部来源（`scope_mismatch`）；SOP 降密点不检查，靠人工批准。
- #A8 SOP 撤回是否需批准待 #996；v1 随 #A1 拒绝。
- #A9 撤销、过期、失效的已注入包内容在后续调用中换成占位。
- #A10 取消一次替换不撤销替换，当前源修订不回退。
- #A11 加载类别顺序 `personal` → `org` → `group`。
- #A12 v1 不支持异人或跨设备审批，候选不上服务端。
- #A13 Off、切换、注销不取消在途 job，动作前提示并给取消入口。
- #A14 删除或清空 KB 先撤回其中未撤回的 SOP；v1 凭据不能删除或清空 KB（W1.11）。
- #B1 `authz_version` 粒度为主体 × 资源，可保守多变。 · #B2 后台复验建立 job 的操作类 + 凭据未撤销 + 政策。 · #B3 / #B20 幂等与 job 记录至少 90 天。
- #B4 去重键为 KB + SHA-256 + 规范化文件类型。 · #B5 命中失败或取消的文档返回 `existing`，不自动重做。 · #B6 `config_version` 不进默认库唯一键。
- #B7 上游 `finalizing` 映射为 `indexed` + `enrichment: "running"`。 · #B8 取消、失败不回退当前源修订。 · #B9 对无工作的 job 取消返回 200 `cancel_effect: "none"`。
- #B10 发布人失去来源读取或 `pack.publish` 则包失效。 · #B11 任一来源变化整包失效。 · #B12 有效期上限 90 天。 · #B13 只在发布前截断，加载放不下即跳过。
- #B14 / #B23 单一注入预算、确定全序（类别 → profile → `pack_id`），宿主整包裁定。 · #B15 只能整句原文摘录。 · #B16 候选只在宿主本地。 · #B17 批准有效期 1 小时。
- #B18 基础修订冲突拒绝写入，重新批准。 · #B19 批准失败统一 `approval_invalid`，本人记录细分 `approval_*`。 · #B21 `chunk.edit` 带政策版本、复验预算、报告费用。 · #B22 每主体 `active` 批准上限 20。
- #B24 占位触发见 W7.14；被新版本取代也触发占位（(b)，不以新版实际注入为前提），由 Grok、Codex 讨论一致后 David 2026-10-20 确认采用。

## 11. 非规范材料

完整版（含转移表、验收用例清单、现状证据与参考设计）保留在分支 `ab/weknora-03c-ops`（#1075 的分支），由实现任务以测试证明，不约束本契约；两者冲突时以本核心为准。
