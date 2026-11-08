# Knowledge 写操作语义契约 v2 · 规范核心（CONTRACT-OPS-v2）

> **状态**：规范核心 v2 r1（2026-11-09，#1668 KB-CT-03③）。v1（2026-10-20）从未实施，本文由它升主版本而来，变更清单见 §12。仍**没有实施**：Agent24 没有写操作入口，WeKnora 集成模式只放行 ready、search、read 三条路由。实现另开任务（11-11 后）。条款按 READONLY r8 写成，现行 READONLY 为 r11。
> **日期**：v1 2026-10-20；v2 2026-11-09 · **依据**：`docs/knowledge/CONTRACT-READONLY-v1.md` r8（下称 READONLY，规则写作 R*n.m*）；`CONTRACT-STATE-v1.md`（STATE 规范核心 v1（同批），条款写作 S*n.m*）、`CONTRACT-UI-BRIDGE-v1.md`（UI-BRIDGE 规范核心 v1（同批），条款写作 U*n.m*），依赖登记在 §9· **Issue**：#1013 ③（KB-CT-03）、#1668（v2：知识条目段）。
> 由完整版 `ab/weknora-03c-ops`（#1075 的分支）压缩而来。本文只写对外行为与不变式；实现机制交给代码与测试证明（§11）。

## 0. 范围与边界

- **0.1 冻结什么**：Agent24（Provider 与宿主，下称"客户端"）与 WeKnora fork（下称"服务端"）之间**会改变服务端状态的操作**的对外语义：默认库初始化、ingest 与 job、文档删除与分块编辑、ContextPack 的起草 / 批准 / 发布 / 加载、知识条目候选的提交 / 审阅 / 批准 / 撤回与已发布条目的下架，以及它们共用的授权、判定顺序、幂等、费用报告。不定 HTTP 路径、类型、存储结构、锁、键名与重试节奏；文中的 90 天、1 小时、20 条是已定工程值（§10）。
- **0.2 不管什么、交给谁**：
  - READONLY：凭据与签发、有效范围 E（R2.1）、修订三元组与活动构建（R5.1、R5.3）、错误信封与 `code` 闭集（READONLY §6）、generation 只回显（R7.5）、审计最小字段（R10.1）。本文直接引用，**不新增 `code`**，只新增 `details.reason` 取值（§6）。
  - STATE：binding、generation 何时递增、准入、作废、本地结果、run 绑定、证据占位。本文不新增 generation 递增事件，也不新增 STATE 的持久状态（§9；W1.5 的待决写记录是宿主侧记录，不属 STATE 状态）。
  - UI-BRIDGE：KnowledgeView 与消息桥（本文只要求它们不是写入口，W1.6）。
  - 业务决定：矩阵每一格、ContextPack 受众、职责分离、已发布条目由谁下架 → #996（v2 的落格见 W2.5）；处理目的地、费用上限与预算 → #994、KB-SVC-POLICY-01；费用承担 → #995；原件与产物的删除 → #997；组织身份 → KB-ORG-01、#992。
  - 实现任务（以测试证明细节，不约束本文）：服务端端点、计量、幂等记录、唯一约束与允许列表 → KB-C4-02b、KB-C4-04、KB-C1-01；Provider 准入、待决写操作、超时与预算 → KB-C1-04、KB-HOST-01；Lite 模式（无队列，靠检查点停止，语义同本文）→ KB-LITE-04；ContextPack 与注入入口 → KB-CTX-01、KB-CTX-02；知识条目 → KB-SOP-01（宿主侧草稿、脱敏与提交，Agent24#1669、#1670）、KB-SOP-02（服务端候选表、复验与写入，WeKnora#146），凭据范围 → WeKnora#135。
- **0.3 变更规则**：沿用 READONLY §0.3。操作类集合、判定顺序、job 状态集合、幂等作用域、批准绑定项、ContextPack 加载复验项是语义，改动升主版本；新增 `reason`、`cancel_effect`、费用 `kind`、失败原因算本版修订（v2 修订）。客户端兜底：未知 job 状态按 `failed`；未知 `cancel_effect` 按 `none`；未知 `enrichment` 按 `running`（费用按未结束）；未知 `stop_reason` 按"已停止，原因未知"；未知费用 `kind` 展示为"其他费用"；以 `approval_` 开头的未知 `reason` 按 `approval_invalid`。
- **0.4 术语**：**写操作**＝§2 列出的操作类（`job.read`、`pack.read`、`approval.read` 一并列入以表达授权）。**新建工作的操作**＝会让服务端开始新处理并产生费用的 `init.default_kb`、`ingest.submit`、`ingest.reprocess`、`candidate.approve`、`chunk.edit`。**job**＝一个源修订的一次处理尝试，带 `job_id` 与建立它的操作类。**阶段边界**＝下一步会产生外部调用或费用之前的检查点（开始解析、开始 embedding、激活构建、每个增强子任务开始前）。**增强**＝主索引之外的模型调用处理（摘要、问题生成、图谱、wiki 等）。**占用**＝job 处于 `saved`、`processing`，或 `indexed` 且 `enrichment: "running"`。**pack 范围**＝`pack_id` 所属的组织或个人范围。**知识条目**＝经批准写入目标 KB 的文档，`origin: "entry"`，带 `kind` 标签（§6）；SOP 是 `kind: "sop"` 的知识条目。**候选**＝作者提交、等待审阅的一份条目内容，存在服务端候选表里，不是文档。`authz_version` / `policy_version` / `config_version`＝权威授权 A、处理政策、配置档的不透明版本，只比较相等。

## 1. 授权与判定

- **W1.1** 每个写操作由服务端逐请求重新计算授权：principal 只取自凭据（R1.5），E 按 R2.1；操作类必须同时在凭据的操作范围内、且在 A 对目标所在范围的授予里。结果码见 W1.9 ③。
- **W1.2** 写操作的响应与审计回报当次判定用的 `authz_version`；客户端不得缓存它来跳过判定。粒度为"主体 × 资源"（资源含 KB 与组织级授予），服务端可以保守地多变，多变只会让依赖它的记录更早失效。
- **W1.3** **后台作业沿用授权**：job 记下建立时的操作类、principal、`credential_id`、目标 KB 与 `authz_version`、`policy_version`、`config_version`、generation（只记录）。每个阶段边界按序复验：① principal 当前仍在目标 KB 上持有**建立该 job 的操作类**；② 建立 job 的凭据未被撤销；③ 当前政策仍允许下一步的目的地与费用。主索引阶段 ①② 不成立 → `cancelled/authorization_revoked`；③ 不成立 → `failed`（`policy_denied` / `budget_exhausted`）；增强阶段不成立只停增强（`stop_reason` 同名）。多项不成立取序号最小者。**凭据过期不算撤销**；**续签替换旧凭据也不算撤销**（等同过期）；只有签发权威对该凭据或其续签后继的撤销使 ② 不成立。撤销事实须可判定到所有引用它的 job 终态且费用 `final`。
- **W1.4** **客户端准入**：Provider 只在 binding 可服务（含 `degraded`）**且**准入开放（STATE 术语：`persist_pending` 为假、无未解除的待复验、不在作废中）时发出写操作，否则按 STATE §6 给本地结果。另看最近一次就绪结果中操作对应的就绪键：`pack.*` → `operations.pack`；`candidate.*`、`entry.withdraw` → `operations.candidate`；其余新建工作的操作 → `operations.ingest`；`job.*`、`document.*`、`approval.*` 没有对应键。Provider 只发出 `candidate.propose`；`candidate.review`、`candidate.approve`、`entry.withdraw` 由嵌入页的 UI 会话发出（W1.6）。键为 `disabled` → 本地 `unavailable`、`reason: "operation_disabled"`、`retryable: false`（与服务端 W1.9 ② 同义，不新增 code）；新建工作的操作另要求键为 `ready`，`not_ready` 或尚无结果 → 本地 `not_ready`（可重试）。本地拒绝都不发网络请求、不改变 STATE 状态。写请求带 `request_id` 与 `generation`（R7.2），多 binding 时只发往目标 KB 所属的 binding；UI 会话发出的写请求（W1.6）只带 `request_id`，不带 `generation`（UI 会话没有宿主侧的 generation，服务端也不回显）；写响应回显 `request_id`（Provider 发出的还回显 `generation`，R7.5）并带 W1.2 的 `authz_version`；§8 的报文形状见 W8.14；写操作（含 `pack.load`）的响应都不是 STATE S5.10 的输入，`pack.load` 的 `scope_mismatch` 只按 W7.14 使该包占位。
- **W1.5** **结果未知**：写请求（`*.read` 与 `pack.load` 除外；`pack.load` 走 W7.10、W7.12，Off 记 `skipped/knowledge_off`）发出后，服务端结果没有交给调用方的任何路径——generation 作废中止、迟到或 generation 过期的响应被丢弃（S4.6）、追加前复查换码（包括 Off 换成的 `knowledge_off`）——本地结果一律带 `details.write_outcome: "unknown"`（有幂等键的附上键），界面不得声称"未保存"。待决写记录（操作类、请求体、幂等键、`binding_id`）在**发出请求之前**持久化，确认后才发出，持久化失败则不发出（按 STATE §6 得 `local_storage`），得到确定结果后清除；它是宿主侧记录，按 STATE S1.10 的先持久后生效原则处理，不是 STATE 的持久状态，与 0.2、I2 不冲突。重启或 Tool 重试只能在**同一 binding**（同一 `binding_id`）内**用同一键与同一请求体**重发，**不得换新键自动重发**。该 binding 已退役、身份元组（S2.4：principal、端点、租户或实例）任一项已变，或待决时间超过服务端幂等保留期（W3.3）时，宿主不再重发，保持 `unknown` 并提示用户自行确认。
- **W1.6** **与业务决定无关的机制约束**：
  - 授权只来自 A 与凭据，不从 retrieve 能力、页面角色、上游 RBAC、组织名、License、激活状态或请求体字段（归属、创建人、`approved`、`role`）推导；这些字段被忽略并记审计。
  - 服务端不信任宿主转述的批准：批准只能是批准人用自己的已认证会话发出的请求（W7.15、W8.5）；请求体里的 `approved`、`author`、`approver` 一律忽略。
  - 桥不是写入口：桥消息是闭集，`approval`、`approve`、`decision` 是禁止键（UI-BRIDGE U6.2 不变）。
  - **UI 会话只能做三类写操作**：session JWT 只能执行 `candidate.review`、`candidate.approve`、`entry.withdraw`，且只经集成闸门精确表中为它们列出的 UI 入口；principal 取自该会话所兑换的凭据 C（UI-BRIDGE U2、READONLY R1.5），等同本人的已认证请求；C 解析不出归属用户时得 `forbidden/operation_not_allowed`。每次执行时服务端复验：会话与 C 未撤销、principal 此刻持有该操作类（W2.5）、目标状态（W8.4）。其余写操作用 session JWT 调用得 `unauthorized/credential_invalid`。
  - **残余风险**（写明，不声称已消除）：服务端分不出一次 UI 请求是不是人真的点了按钮，页面内的脚本也能以同一会话发请求。缓解：`candidate.approve` 请求体必须带 `candidate_id` 与 `content_sha256`，由服务端核对候选当前内容（W8.5）；嵌入页 CSP 只允许自身脚本（UIB U4.2）；每次批准进审计。
  - 宿主没有自己的 principal；引导身份只能做 `init.default_kb`，且只代表调用者。
  - 集成模式下 retrieve 级凭据不得取原件（下载、批量下载、`/files` 均不开放，R2.7）。
- **W1.7** 每个写操作与每次 job、ContextPack、批准状态变化各记一条审计（事件名见 §6）；字段按 R10.1，另加幂等键哈希、`job_id`、`authz_version`、`policy_version`、费用摘要；不含原件、正文、query、凭据（R10.3）。后台阶段的审计 principal 是建立 job 时的 principal（R10.4）。
- **W1.8** 取消、撤权、撤回、撤销都追不回已交付给模型或用户的内容与已发生的费用（R7.7）；界面与审计不得声称可以。
- **W1.9** **服务端判定顺序**（取第一个不成立的）：① 凭据 → `unauthorized`；② 请求形状 → `invalid_request`（缺字段、缺幂等键、hash 不符、修订文档缺 `expected_source_revision` 等），随后判操作是否被关闭（R6.4：对应就绪键为 `disabled`，键的对应见 W1.4）→ `forbidden/operation_disabled`（不可重试，先于 ③）；③ 授权与目标解析，依次：(a) 操作类不在凭据操作范围、或调用者在**任何范围**都不持有该操作类 → `forbidden/operation_not_allowed`（与目标无关，先于解析目标）；请求显式指定的目标 KB 不在 E → `forbidden/empty_scope`；(b) **目标对调用者不可见** → `not_found`，不可见的各种情形（不存在、所在 KB 不在 E、已撤回、删除中、job 非本人发起、作为 `approval.read` / `approval.void` 目标的批准记录非本人、候选非本人提交且调用者在其目标 KB 上不持有 `candidate.review`、ContextPack 按 W7.3 不可见）响应完全相同，且先于 ④⑤，所以政策版本、格式、配额都不能用来区分它们；(c) 目标可见、但调用者在目标所在范围不持有该操作类 → `forbidden/operation_not_allowed`。④ 幂等回放（W3.2）；⑤ 操作特有检查（按 W5.1、W5.13、W5.14、W7.15、W8.2、W8.5 所列顺序）；⑥ 执行。回放先于 ⑤：首个请求已执行后，政策变化或批准已用都不影响原样重试拿到首次结果；回放后于 ③：失去授权的调用者拿不到回放。被 ①–⑤ 拒绝的请求不写幂等记录，例外见 W3.2。
  - **存在性不可区分的补充**：草稿对 `pack.approve` / `pack.publish` 的"可见"本身就要求在 pack 范围内持有同名操作类且能读全部来源，所以 (c) 不会出现，持有他处授予或读不了来源都得 `not_found`。`candidate.approve` 与 `candidate.review` 的目标是候选，候选可见本身要求调用者在候选的目标 KB 上持有对应操作类，所以 (c) 不会出现；候选指向的修订目标文档在 ⑤ 才检查（W8.5）。`pack.publish` 出示的 `approval_id` 是参数而非目标，在 ④ 之后判定。
  - **唯一例外**：同一主体在保留期内对**自己**已删除（含仍在删除中）、已撤回、已放弃、已作废、已撤销的对象重复执行天然幂等操作，返回 200 与 `details.already: true`（W3.1）。
- **W1.10** Off、取消选用或停用 profile、注销都**不取消**已建立的 job。注销只撤销该 binding 的长期登录态与 UI 会话族（U4.10、U4.11），**不撤销已签出的凭据 C**，因而不构成 W1.3 ② 的撤销，已建立 job 的授权不变。动作前若受影响 binding 有本宿主发起、未终态或 `cost.final` 为 `false` 的 job，宿主必须提示"有 n 个处理仍在进行、会继续产生费用"并提供先取消的入口（Off 汇总全部 binding）。使 binding 进入 `retired` 的事件（移除 profile、换实例、principal 变化、改端点或租户，S2.4）同样提示：这些 job 仍在服务端运行，退役后宿主无法再读取或取消，需用户自行确认；能预知的（移除 profile、改端点或租户）在动作前提示，其余在发现时提示。以上"不取消"针对远端服务；local-managed 停用或移除时 sidecar 停止（S2.3），其上的 job 随之中断（按 W5.7 恢复或 `failed/interrupted`），提示改为"有 n 个处理将中断"。
- **W1.11** **矩阵之外的上游写路由一律不开放**：凭据只能执行 §2 的操作类，其他会改变状态的上游路由（含**删除与清空 KB**、移动、复制、批量操作、手工知识更新、标题与图片信息更新、重新生成摘要）对凭据返回 `forbidden/operation_not_allowed`。集成模式下，任何会改变**未下架的 `origin: "entry"` 文档**检索可见内容、所在 KB 或存在与否的路由都按机制拒绝，例外只有 `candidate.approve`（`revise`）、`entry.withdraw` 与同字节同解读方式的 `ingest.reprocess`；上游的手工知识编辑与 `PUT /knowledge/manual/:id` 一类路由不能成为候选或条目的写入路径（它们只看库写权限、带 publish 直接入索引，会绕过 W8.5 的复验）。经本契约之外的途径（如上游管理员路由）删除或清空 KB 时，服务端必须**先**按 W8.10 下架其中未下架的条目文档、把**以该 KB 为目标的全部** `submitted` 候选（`create` 与 `revise` 都算）按 W8.6 退回（原因 `target_gone`，审计原因 `kb_deleted`），再执行（决定 #A14）。

## 2. 操作类与授权矩阵

- **W2.1** 操作类（闭集）：`init.default_kb`、`ingest.submit`、`ingest.reprocess`、`job.read`、`job.cancel`、`document.download_original`、`document.delete`、`chunk.edit`、`pack.draft`、`pack.discard`、`pack.read`、`pack.approve`、`pack.publish`、`pack.revoke`、`pack.load`、`approval.read`、`approval.void`（批准记录只剩 `pack.publish` 一种用途）、`entry.draft`（宿主本地，不改变服务端状态）、`candidate.propose`（提交候选、撤回自己的 `submitted` 候选、读自己的候选）、`candidate.review`（列出与阅读目标 KB 的候选、退回）、`candidate.approve`、`entry.withdraw`。主体类别：本机单用户、remote 个人、组织成员、组织中被授予特定职责者、组织管理员、引导身份、页面会话、模型发起。
- **W2.2** 每格由 #996 填"允许"、"允许，附条件"或"拒绝"，条件只能引用服务端可验证的事实；**决定之前全部按拒绝执行**（fail closed），开发测试只在批准的样本租户上用服务端配置的临时授权进行，不进对外构建。
- **W2.3** 与决定无关的固定格：引导身份只能 `init.default_kb`；页面会话只能 `candidate.review`、`candidate.approve`、`entry.withdraw`（W1.6），其余一律不能；`document.download_original` 在本版对所有主体拒绝；`approval.read`、`approval.void` 只限批准人本人；`pack.load` 按受众（W7.9），由宿主注入入口发起；模型发起只能 `pack.draft` 与 `entry.draft` 的生成和编辑，其余（含 `candidate.propose` 与一切批准）只能由人发起：`candidate.propose` 在宿主界面、审阅、批准与下架在嵌入页——服务端分不出模型与用户，这一列由宿主执行（决定 #A3）。
- **W2.4** 职责分离属于 #996 条件。服务端只能执行它看得见的事实：ContextPack 的起草人与批准人；候选的提交人与批准人（v2 候选在服务端，两者都看得见）。#996 首版允许作者批准自己的候选，服务端比较两者的 principal，相同时批准记录与审计带 `author_is_approver: true`；以后若组织选择「双人批准」，由服务端按这一事实拒绝（`forbidden/operation_not_allowed`），不需要升主版本。
- **W2.5** **#996 落格（David 2026-10-25 决定，General 2026-11-09 确认照搬到全部知识条目类型）**：`candidate.approve` 只给组织管理员；个人 Lite 本人持有全部操作类。以下三格 #996 没有逐字写到，由 General 2026-11-09 按 #996 推论定：`candidate.propose` 只要求目标 KB 在提交人的 E 内（propose 不是 #996 的「上传」：候选不入库，写入必经管理员批准，「用户不直接写」由凭据范围保证）；`candidate.review` 同 `candidate.approve`（组织管理员）；`entry.withdraw` 只给组织管理员——作者不能单方下架已进组织库的条目，只能请管理员处理。

## 3. 幂等与去重

- **W3.1** 必须带 `idempotency_key`（≥128 bit 随机）：`init.default_kb`、`ingest.submit`、`ingest.reprocess`、`pack.draft`、`pack.approve`、`pack.publish`、`candidate.propose` 的提交、`chunk.edit`；缺失为 `invalid_request/idempotency_key_missing`。`candidate.approve` 不另带键：以 `candidate_id` 为键，幂等作用域为 `(tenant, 批准人, candidate.approve, candidate_id)`，指纹含 `content_sha256`。天然幂等、不要求键：`pack.revoke`、`pack.discard`、`candidate.propose` 的撤回、`candidate.review` 的退回、`entry.withdraw`、`document.delete`、`approval.void`（重复执行按 W1.9 例外返回 `already: true`，其他主体 `not_found`；`pack.revoke` 另外对能读到该版本且持有 `pack.revoke` 的其他主体也返回 `already: true`）；`job.cancel` 重复执行按 W5.9 返回 `cancel_effect`。
- **W3.2** 幂等作用域为 `(tenant, principal, 操作类, idempotency_key)`，并比对请求指纹（操作类、目标、`content_sha256`、影响结果的参数）：同键同指纹已完成 → 首次结果加 `details.replayed: true`，不新建、不重复计费；同键同指纹仍在执行 → `unavailable/request_in_progress`（带 `retry_after_ms`，不触发就绪探测）；同键不同指纹 → `invalid_request/idempotency_key_reused`。不同主体的同一键互不可见。**例外**：使批准作废的拒绝（W7.15）、使候选转为 `returned` 的批准拒绝（W8.5）也写幂等记录，重试得到同一拒绝。
- **W3.3** 幂等记录与 job / 费用记录至少保留到 job 终态后 90 天；不产生 job 的写入后 90 天；服务端重启不丢；过期后同键视为新请求。
- **W3.4** **原子性**：保存原件、建立源修订与 job、写幂等记录、内容去重判定、`expected_source_revision` 的比较并设置、`candidate.approve` 的复验、候选状态转移、批准记录与 ingest job 的建立，对外只有"全部完成"或"都未发生"；并发基于同一修订的提交只有一个成功，其余 `stale/base_revision_changed`；并发同键只执行一次。
- **W3.5** **内容去重**（跨请求，与幂等键无关）：键为同一 KB 内的 `content_sha256` 加规范化文件类型；只比对未撤回、未删除、不在删除中的文档；修订已有文档时只与它自己的当前源修订比较；`candidate.approve` 只命中本 KB 中未下架、当前修订有活动构建的 `origin: "entry"` 文档。命中时返回 200、`details.outcome: "existing"`，不新建修订与 job、不改动已有时间戳、写幂等记录；最近 job 由调用者发起时附该 job，否则只附文档级处理状态（`index_pending` / `indexed` / `index_failed`），不回 `job_id` 与 `cost`。命中的文档处于失败或取消时同样 `existing`，不自动重新处理。不跨 KB。
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

- **W5.1** `ingest.submit` 带 `scope`（只含目标 KB）、键、原件（文件或文本）、`content_sha256`、`file_name` 与规范化文件类型、`expected_policy_version`；修订已有文档另带 `knowledge_id` 与 `expected_source_revision`（不等于当前 → `stale/base_revision_changed`，附 `current_revision`）。`origin: "entry"` 文档只能经 `candidate.approve`（`revise`）修订，`ingest.submit` 对它 `forbidden/operation_not_allowed`。服务端不收 URL（决定 #A2）。⑤ 的顺序：政策版本 → 格式与大小 → 配额 → `origin` → 基础修订 → 去重。
- **W5.2** 保存前拒绝：格式不在白名单或不会产生 v1 范围的 `text` 分块（R3.8）→ `invalid_request/unsupported_format`；超过大小 → `invalid_request/too_large`；配额不足 → `forbidden/quota_exceeded`；都不保存、不建 job。
- **W5.3** **断开不是取消**：job 建立后，连接断开、客户端超时、generation 作废、Off、切换、注销都不影响它；只有 `job.cancel`、W1.3 复验、新修订替换、文档删除或撤回、local-managed 的 sidecar 停止（W1.10）能停止它（READONLY R7.1 的断开即取消只适用于 search 与 read）。
- **W5.4** 每次被接受的提交分配新 `source_revision`，**立即**成为当前源修订，活动构建在激活前为 `null`；处理期间旧构建的 hit 只能是 `stale`（R5.3），**从不以 `fresh` 出现**。取消与失败都不回退当前源修订（决定 #A10、#B8）；恢复旧内容只能把旧原件重新提交为新修订。
- **W5.5** `ingest.submit`、`ingest.reprocess`、`chunk.edit` 的 `expected_policy_version` 与当前不同 → `stale/policy_changed`（附当前版本），不保存、不建 job、不发调用。
- **W5.6** job 状态闭集与原因枚举见 §6。`indexed` 表示本 job 的构建已激活且仍是该修订的活动构建时可检索；增强可能仍在进行（`cost.final: false`）。`saved`、`processing` 之外的状态不再变，`indexed` 只有 `enrichment` 会变。
- **W5.7** **可观察结果**：正常处理 `saved → processing → indexed`；入队失败、解析或依赖重试用尽、超过处理时限（含停在 `saved`）→ `failed`；中断后在恢复时限内继续则仍 `processing`，否则 `failed/interrupted`，job 不会永远停在非终态。增强子任务重试用尽、超过增强时限、或中断（含服务端重启）后未能在恢复时限内继续 → `enrichment: "stopped"`（`failed` / `timeout` / `interrupted`），job 仍 `indexed`、活动构建保留，`cost.final` 随后为 `true`；增强不会永远停在 `running`。同一文档新修订被接受 → 旧修订的占用 job `cancelled/superseded` 或增强 `stopped/superseded`；文档删除或撤回 → `cancelled/document_deleted|document_withdrawn` 或增强同名停止；凭据只是过期 → 不变。新构建激活时同一修订旧构建及其增强产物不再可检索、旧 job 增强停止。当前修订没有活动构建时，不带 `expected_revision` 的文档级 read：失败或取消后为 `unavailable/index_failed`，处理中为 `index_pending`，文档已删除或撤回为 `not_found`（R4.4）。
- **W5.8** 非 `indexed` job 写下的部分产物不得被检索、读取或成为活动构建；job 不再有工作（非占用）之后迟到的写入一律不接受；`indexed` 后增强阶段正常写下的产物保留。新 job 只认自己 `job_id` 的产物。
- **W5.9** **取消**：`job.cancel` 同步返回 `cancel_effect`：`saved` / `processing` → `cancelled`（job 变 `cancelled/user`）；增强进行中 → `enrichment_stopped`（仍 `indexed`，活动构建保留）；其他 → `none`（200，状态不变）。已发出的外部调用可能仍完成并计费。响应与 `job.read` 报告 `original_retained`（本版恒为 `true`）、`partial_artifacts`（v1 恒为 `retained_invisible`）与 `cost`；取消、失败、撤权都不删除原件与产物（决定 #A5）。
- **W5.10** **费用如实**：每个 job 带 `cost: {final, items[{kind, destination, units, unit_type, measured}]}`；不知道填 `units: null` 加 `measured: "unknown"`，**不得填 0**；估算标 `estimated`；`final: true` 只在不再有任何在途调用与增强之后，增强离开 `running` 后必然最终变为 `true`；界面只有 `final: true` 且全部项为精确 0 才能说"没有产生费用"。金额若给出放在可选 `amount` 并注明价格表版本。
- **W5.11** `job.read` 按 `job_id`，或按调用者当前 principal 的 `(操作类, idempotency_key)` 查询；返回操作类、状态、`enrichment`、`knowledge_id`、`source_revision`、`index_build_id`、`attempt`、原因、`cost`、各状态时间戳。本版默认只有发起人（仍须持有相应操作类）可读可取消，他人 `not_found`（决定 #A1）。文档删除后 job 与费用在 W3.3 的保留期内仍可查。
- **W5.12** job 状态与费用只是元数据，不作为知识证据进入模型上下文。
- **W5.13** **重新处理**：`ingest.reprocess` 带 `knowledge_id`、`source_revision`、键、`expected_policy_version`；⑤ 的顺序：政策版本 → 修订为当前（否则 `stale/base_revision_changed`）→ 占用。每个源修订至多一个占用中的 job，已有时返回 200、`details.outcome: "existing_job"`（附 job 规则同 W3.5）。原活动构建保持到新构建激活；新 job 失败或取消时原构建不撤下。新构建**不继承人工分块编辑**（新分块 `chunk_revision` 为 0），宿主在发起前必须提示"人工编辑将不会保留"。条目文档重新处理不需新批准。不做格式、配额、`origin` 与去重检查。
- **W5.14** **删除与分块编辑**：`document.delete` 触发删除后的停止（W5.7）；`chunk.edit`（编辑、回滚、删除、启用 / 禁用分块，增删与重新生成问题）⑤ 的顺序：政策版本 → `origin` → 当前政策的目的地（`forbidden/policy_denied`）与预算（`forbidden/budget_exhausted`），拒绝时零调用；成功响应带 W5.10 格式的 `cost`，并改变 `chunk_revision`（使引用它的 ContextPack 失效，W7.9）。两者对 `origin: "entry"` 文档一律 `forbidden/operation_not_allowed`。

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
| 批准记录 | 状态 `active`、`used`、`expired`、`void`；`purpose`：`pack.publish`（条目的批准不产生批准记录对象，见 W8.5） |
| 知识条目 `kind` | `sop`、`faq`、`note`（只是标签，不改存储与语义；新增取值算 v2 修订，客户端把未知取值显示为「其他」） |
| 候选状态（服务端） | `submitted`、`approved`、`returned`、`withdrawn`；只有 `submitted` 能转出（W8.4） |
| 候选退回原因 `return_reason` | `reviewer`（审阅人退回）、`conflicted`（W8.7）、`target_withdrawn`（修订目标已下架）、`target_gone`（修订目标已删除，或 KB 删除）、`unsupported_format`、`too_large` |
| 文档 `origin` | `upload`（ingest 提交）、`entry`（经 `candidate.approve` 写入的知识条目） |
| `invalid_request` 新增 reason | `idempotency_key_missing`、`idempotency_key_mismatch`、`idempotency_key_reused`、`content_hash_mismatch`、`unsupported_format`、`too_large`、`excerpt_mismatch`、`validity_too_long`、`validity_invalid` |
| `forbidden` 新增 reason | `quota_exceeded`、`budget_exhausted`、`scope_mismatch`、`approval_invalid`（本人记录可细分为 `approval_used` / `approval_expired` / `approval_void` / `approval_mismatch`）、`omitted_source_unreadable`；沿用 `empty_scope`、`operation_not_allowed`、`operation_disabled`、`policy_denied` |
| `stale` 新增 reason | `candidate_not_submitted`（W8.4）、`policy_changed`、`base_revision_changed`、`source_changed`、`source_withdrawn`、`version_superseded`、`version_not_draft`、`version_not_published` |
| `unavailable` 新增 reason | `request_in_progress`（可重试，带 `retry_after_ms`） |
| `not_found` | 不带 `reason`（R2.6 同码、同 message） |
| 审计事件 | `knowledge.init`、`.ingest.submit`、`.ingest.reprocess`、`.job.transition`、`.job.cancel`、`.document.delete`、`.chunk.edit`、`.pack.draft` / `.approve` / `.publish` / `.revoke` / `.discard` / `.transition` / `.load`、`.candidate.submit` / `.withdraw` / `.return` / `.approve`、`.entry.withdraw`、`.approval.void`（前缀均为 `knowledge`）；`kb_deleted` 只作审计原因 |

## 7. ContextPack

- **W7.1** ContextPack 是经批准、带版本、体量很小的背景包：`pack_id`、`pack_version`（单调递增，发布后不可变）、`content`、`sources[]`、`audience`、`token_budget` / `token_count`、`truncated` / `omitted_sources[]`、`valid_from` / `valid_until`、`approval_digest`、起草人 / 批准人 / 发布人、`approval_id`、发布时的 `authz_version` 与 `policy_version`、`state`。
- **W7.2** 受众 `audience.kind` ∈ `personal` / `org` / `group`（本版支持哪几种、谁能设定待 #996）；每次加载按当前成员资格判定，不认客户端自报；`org` / `group` 要求已验证组织身份。
- **W7.3** **来源、摘录与可见性**：`sources[]` 每项是带修订三元组的分块引用，只能取自校验通过的单分块 read（R4.7）；`pack.draft` 时服务端按 R4.6 复验起草人**当前**能读全部来源，任一读不了即按 R2.3 / R2.6 拒绝，历史 read 成功不构成授权。`content` 只能是来源分块的**整句原文摘录**（句界按 UAX #29，不得句内删字，段间省略用"……"，按最长匹配优先核对），发布时不符 → `invalid_request/excerpt_mismatch`。**可见性**：曾发布的版本对 `pack.read`——受众此刻不含读取者或缺组织身份 → `not_found`；否则读取者的 E 不覆盖全部来源 KB → `forbidden/scope_mismatch`（不回任何其他原因）；通过后可得状态、版本与 `reason`，只有通过 W7.9 ①–④ 的 `published` 版本返回 `content`。起草人、批准人、发布人可读自己相关版本的元数据，同样先过 E 覆盖检查。草稿只对起草人（当前能读全部来源）与在 pack 范围内持有 `pack.approve` 且能读全部来源者可见；作为 `pack.approve` / `pack.publish` 的目标时，可见＝在 pack 范围内持有同名操作类且能读全部来源；`discarded` 版本只对起草人的重复放弃可见。不可见一律 `not_found`。
- **W7.4** 加载时复验发布人：失去任一来源 KB 的读取或失去 `pack.publish` → 失效（`publisher_revoked`）。
- **W7.5** `valid_until` 必填，晚于发布时刻与 `valid_from`（否则 `validity_invalid`），距发布不超过 90 天（否则 `validity_too_long`）；有效区间 `[valid_from, valid_until)`；不自动续期。
- **W7.6** 截断只在发布前：`token_count` 超预算 → `invalid_request/too_large`；为预算放弃的来源列进 `omitted_sources`（`truncated: true`），服务端只校验格式与起草人读权（无权 → `forbidden/omitted_source_unreadable`）。加载时不截断，放不下整包即 `over_budget`。
- **W7.7** 包内容进入模型上下文时是带 Knowledge 来源标识的数据，不获得系统指令优先级。
- **W7.8** 只有 `published` 可加载。草稿不可原地修改，每次修改产生新版本；`pack.discard` 只能放弃 `draft`（否则 `stale/version_not_draft`），只限起草人（他人 `not_found`）；`pack.approve` 只对 `draft`（否则 `stale/version_not_draft`）；`pack.revoke` 只对 `published`（已 `revoked` 为 `already`，其他 `stale/version_not_published`）。`pack.load` 或 `pack.read` 首次发现 `published` 版本过期或失效时持久记为 `expired` / `invalidated`（这是读取唯一的写入），此后对所有主体一致、永不恢复；`revoked` / `expired` / `invalidated` 只能发布新版本续用。
- **W7.9** **每次加载服务端复验**：宿主注入入口对 run 绑定的每个可服务且准入开放（同 W1.4）的 binding 各发一次请求（其余 binding 按 STATE §6 给本地结果、不发请求）（带当前 generation、整份注入预算、`model_destination`），结果按 binding 分开。**候选集**：每个 `pack_id` 中版本号最大的曾发布版本，仅当其状态为 `published` / `expired` / `invalidated` 且受众此刻包含请求主体（`org` / `group` 另要求组织身份）时进入；**不在候选集的包一律不列出、不给 reason**；缺组织身份时结果带 `details.org_identity_missing: true`。逐包取第一个不成立的：① 请求主体的 E 覆盖全部来源 KB（`scope_mismatch`，放最前以免泄露来源变化；只触发 W7.14 的包级占位，不是 STATE S5.10 的输入）；② 状态已过期 / 失效按原 `reason`，早于 `valid_from` 为 `not_yet_valid`，到达 `valid_until` 为 `expired`；③ 逐来源：文档存在且未撤回（否则 `invalidated/source_withdrawn`），当前源修订、活动构建、`chunk_id` 在构建内且启用、`chunk_revision` 都与记下的一致（否则 `invalidated/source_changed`，整包失效）；④ W7.4；⑤ 政策允许发往 `model_destination`（`policy_denied`）；⑥ 整份预算放得下该包（`over_budget`，逐包判定、不累计）。全部成立为 `attached`。
- **W7.10** 宿主把各 binding 通过的包按确定全序整包计入**同一份**预算：类别 `personal` → `org` → `group`，同类按 profile 稳定标识，再按 `pack_id`；放不下的由宿主记 `skipped/over_budget`。结果：`attached`（至少一个包，其余逐包带 `reason`）、`empty`（候选集为空）、`skipped`（候选非空但都未通过）、`failed`（服务端错误）；Off 时不发请求，记 `skipped/knowledge_off`。
- **W7.11** fresh、续轮、恢复、审批继续、委派每次都重新加载，不跨调用缓存正文；委派子任务只拿到包引用（`pack_id`、`pack_version`）。
- **W7.12** 加载响应按 STATE 的接受规则处理，追加进上下文之前再查准入与 generation；Off 之后不注入。
- **W7.13** 每次加载记审计（请求主体、run、逐包 `pack_id`、`pack_version`、结果与 `reason`），不含包内容。
- **W7.14** **已注入内容的占位**（决定 #A9）：之后的模型调用里，下列已注入版本换成不含正文的占位：(a) 某次结果点名该 `pack_id` 的 vN 为 `expired` / `invalidated` → 该 `pack_id` 已注入且版本号 ≤ N 的全部版本；(b) 某次结果中该 `pack_id` 以任何结果出现（含逐包跳过）、版本号为 N → 已注入且版本号 **< N** 的旧版本，不以 N 实际注入为前提（被取代的旧版本不再单独复验，一律不再重发；决定 #B24）；(c) 该 `pack_id` 不再出现在结果里（只按 `attached` / `empty` / `skipped` 判定，`failed` 或未发请求不算）→ 全部已注入版本；(d) 某次结果对该 `pack_id` 的 vN 报 `scope_mismatch` → 已注入的 vN 及按 (b) 的更旧版本。(d) 是包级占位：不改 STATE S5.10 的可见集合、不递增 generation、不计入撤权窗口（S3.1）；读者永久读不了某个来源 KB 时，也只会让该包占位，不会计入窗口。来源 KB 的真实收窄仍由对该 KB 的 search、read、带 scope 的就绪（S5.10）发现。`policy_denied`、`over_budget`、`not_yet_valid` 本身不使 N 占位，只豁免 N 自身、不豁免 (b)。判断只依据该包所在 binding 按 W7.12 接受的加载结果，在组装本次模型请求之前执行；占位注明「已被新版取代」「已过期 / 已失效」或「当前无权读取全部来源」，按触发原因如实标注；已占位的不恢复。`failed` 或未发请求不构成新版或包消失的证据，不新增占位，也不恢复旧正文（未知变化不保证即时发现）。包内容作为证据记下**全部**来源 KB（S5.6 允许一条证据记多个 KB），任一被占位规则覆盖即占位。
- **W7.15** **批准与发布两步**：`pack.approve` 判定（W1.9 ③）批准人在 pack 范围持有 `pack.approve`（与 `candidate.approve` 是不同授予）且能读全部来源，以及 #996 的职责分离，产生 `purpose: "pack.publish"` 的批准记录（绑定 `pack_id`、`pack_version`、`approval_digest`、`policy_version`；`valid_until` 为签发后 1 小时，单次使用，由发布人出示）。`pack.publish` 由发布人**出示** `approval_id`；发布人不必是起草人或批准人。⑤ 的顺序与结果：批准记录存在、用途与 `pack_id` / `pack_version` 相符、`active` → 否则 `forbidden/approval_invalid`，批准不变；`approval_digest` 一致 → 否则同上；批准人仍持有 `pack.approve` 且能读全部来源 → 否则 `approval_invalid`，批准 `void`；`policy_version` 为当前 → 否则 `stale/policy_changed`，`void`；来源全部仍有效 → 否则 `stale/source_changed|source_withdrawn`，`void`；起草人仍能读全部 `omitted_sources` → 否则 `omitted_source_unreadable`，`void`；摘录、有效期、预算 → 对应 `invalid_request`，`void`；版本号大于该 `pack_id` 所有曾发布版本 → 否则 `stale/version_superseded`，`void`。成功时批准置 `used`，同一 `pack_id` 原 `published` 版本同时变 `superseded`。使批准作废的拒绝写幂等记录（W3.2）。**本人记录的细分**：调用者就是批准人时，`approval_invalid` 改写为 `approval_used` / `approval_expired` / `approval_void`（含本次因批准人失权而作废）/ `approval_mismatch`；他人或不存在的记录只给 `approval_invalid`（决定 #B19）。
- **W7.16** 每个 `pack_id` 任何时刻至多一个 `published`，同一 `pack_id` 的发布串行；旧版本不能在新版本之后重新发布，恢复旧内容只能以它为底起草新版本。

## 8. 知识条目：候选、批准与下架

- **W8.1** **草稿只在宿主**：条目草稿由宿主从用户选中的会话生成（`entry.draft`），存在宿主本地，可编辑、可删除，不改变服务端状态，服务端看不到。以下都是**宿主义务，不是服务端保证**（KB-SOP-01，Agent24#1669）：
  - 准入：用户本人选中会话，并显式确认；会话的 classification 为 `Ordinary`；`Restricted`、`Unknown` 或缺失一律拒绝。
  - 脱敏：确定性规则表命中即不能提交，这是唯一的硬闸门；模型只做高亮提示，既不阻断也不放行。
  - 来源：只取会话中的用户与助手正文。
  - 提交：只送用户确认过的正文与 W8.2 列出的元数据。
- **W8.2** **提交**（`candidate.propose`）。
  - 请求字段（不带 `expected_policy_version`：提交不做任何处理，处理政策在批准时取当前值，见 W8.5）：
    - `idempotency_key`
    - `target_knowledge_base_id`：首版只能是 document 类型的 KB；`kind` 只是标签，`faq` 不路由到 FAQ 类 KB
    - `mode`：`create`，或 `revise`（另带 `knowledge_id` 与 `expected_source_revision`）
    - `kind`
    - `title`：去掉首尾空白后 1–200 字符
    - `body`：≤ 64 KiB 的 UTF-8 文本，按受限 Markdown 解读。白名单：标题（`#`–`###`）、段落、有序与无序列表、强调、行内代码、围栏代码块、引用、`https://` 链接（只显示为链接文本与地址，不预取）。禁止：原始 HTML（代码之外出现 `<` 紧跟字母、`/`、`!` 或 `?`）、图片（`![`）、脚注、HTML 实体之外的转义序列。违反得 `invalid_request/unsupported_format`
    - `content_sha256`：按 W8.13 计算；`title`、`body` 必须已是规范形式（W8.13），服务端不替客户端规范化
    - `provenance`：`{source: "agent24_conversation", session_ref, message_range, skill_version, redaction_rules_version}`。`session_ref` 是宿主算出的不透明 hash（64 位小写 hex），不能还原出会话 id；`message_range` 是两个 0–100000 的整数，前者不大于后者；`skill_version` 匹配 `^[a-z0-9][a-z0-9._/-]{0,63}$`；`redaction_rules_version` 是 1–2147483647 的整数。**不带任何会话原文**；超限得 `invalid_request`。
  - ⑤ 的顺序（取第一个不成立的）：
    1. `kind` 在 §6 闭集内、长度合规（`invalid_request`）；
    2. `content_sha256` 与服务端按 W8.13 重算的值一致（否则 `invalid_request/content_hash_mismatch`，不保存）；
    3. `revise` 时目标文档对调用者可见、未下架、不在删除中（否则 `not_found`）且是由候选发布出来的条目文档（`origin: "entry"`；否则 `forbidden/operation_not_allowed`），并且 `expected_source_revision` 等于当前源修订（否则 `stale/base_revision_changed`，附 `current_revision`）；
    4. 该主体同时处于 `submitted` 的候选不超过 20 条（否则 `forbidden/quota_exceeded`）。
  - 成功后保存候选，返回 `{candidate_id, status: "submitted", content_sha256, submitted_at}`，`candidate_id` 是服务端生成的 ≥128 bit 随机值。
  - 同一主体在同一目标 KB 上以相同 `mode`、`knowledge_id`、`expected_source_revision`、`content_sha256` 已有 `submitted` 候选时，返回 200 和那一条，带 `details.outcome: "existing"`。
  - 提交**不建 job、不进索引、不产生费用**，也不在目标 KB 里留下任何文档或上游草稿行。
- **W8.3** **读面隔离**：
  - 候选不出现在 search、read、文档与分块列表、wiki、标签、任何计数与聚合、ContextPack 里，只能经 `candidate.*` 读到。
  - 作者只能读自己的候选；持有 `candidate.review` 的人只能读目标 KB 在自己授予内的候选；其他情形都是 `not_found`（W1.9 ③(b)）。
  - 审计不记 `title`、`body`、`note`。
- **W8.4** **候选状态机与并发**：
  - `submitted` 转出只有三条路：`approved`（W8.5）、`returned`（W8.6、W8.7、W1.11）、`withdrawn`（W8.8）。这三个都是终态。
  - 转移是比较并设置：并发时只有一个成功，其余得 `stale/candidate_not_submitted`。
  - 同一主体对自己已终态的候选重复执行同一动作（撤回已撤回的、退回已退回的），按 W1.9 例外返回 `already: true`。
  - 候选一旦提交就不能修改；要改，只能撤回后重新提交，得到新的 `candidate_id` 与新的 hash。
- **W8.5** **批准**（`candidate.approve`；不另带 `idempotency_key`，以 `candidate_id` 为幂等键，见 W3.1）。
  - 由批准人经嵌入页 UI 会话发出（W1.6），请求体必须带 `candidate_id` 与 `content_sha256`。③ 要求批准人此刻在候选的目标 KB 上持有 `candidate.approve`。
  - ⑤ 的顺序（取第一个不成立的）：
    1. 候选为 `submitted`（否则 `stale/candidate_not_submitted`）；
    2. 请求里的 `content_sha256` 等于候选的 `content_sha256`（否则 `invalid_request/content_hash_mismatch`，候选不变），且存储的正文按 W8.13 重算仍等于它（否则 `internal`，候选不变，记审计）；
    3. W5.2 的格式与大小检查（不成立时候选 → `returned`，原因为 `unsupported_format` 或 `too_large`）；
    4. `revise` 时目标文档仍可见、未下架、仍是条目文档（否则 `not_found`，候选 → `returned/target_withdrawn`，文档已删除时为 `target_gone`），当前源修订等于候选的基础修订（否则 `stale/base_revision_changed`，候选 → `returned/conflicted`，见 W8.7）；
    5. 去重（W3.5）。
  - 全部成立时，**原子地**完成以下几件事（W3.4）：
    - 候选 → `approved`；
    - 记一条批准，绑定：批准人 principal 与 `authz_version`、目标 KB、`mode` 与 `knowledge_id`、基础修订、`content_sha256`、`kind`、`candidate_id`、提交人 principal、`author_is_approver`（W2.4）、`policy_version`；
    - 按 W5 建立新源修订与 job：job 操作类为 `candidate.approve`，文档 `origin: "entry"`、带 `kind`，原件是条目正文，规范化文件类型为 Markdown；
    - 写幂等记录。

    以上在同一个数据库事务里完成；入队在事务之外，入队失败按 W5.7 使 job 进 `failed/enqueue_failed`，候选保持 `approved`，出口是 W5.13 的重新处理。ingest 作业由 standalone 进程执行，它不在时 fail closed（WeKnora#136）。
  - 返回 `{candidate_id, status: "approved", knowledge_id, source_revision, job_id, author_is_approver}`。去重命中时返回 `details.outcome: "existing"` 与已有文档，不建 job。
  - 批准与使用是同一个操作，**没有可出示、可过期、可作废的批准记录对象**；`approval.*` 只用于 ContextPack。
  - 宿主或请求体里送来的任何批准字段一律忽略（W1.6）。
  - 处理用的是批准时的当前政策；job 按 W1.3 沿用批准人的授权。
- **W8.6** **退回**（`candidate.review`）：审阅人把 `submitted` 候选退回为 `returned/reviewer`，可附 `note`（纯文本，≤ 1 KiB）。作者可以读到 `note`；`note` 随正文一起按 W8.9 删除。
- **W8.7** **冲突不覆盖**：
  - W8.5 第 4 步基础修订不等时，什么也不写入，候选 → `returned/conflicted`，目标文档的人工修订保持当前。
  - 作者要在新的当前修订上重新生成、重新提交。
  - 同目标 KB、同内容的重复批准经 W3.5 去重，是幂等的；同一来源、不同内容只会成为新的候选或新修订，从不覆盖。
- **W8.8** **候选撤回**（`candidate.propose` 的撤回动作）：作者把自己的 `submitted` 候选转为 `withdrawn`。候选不是 `submitted` 时（例如已 `approved`）得 `stale/candidate_not_submitted`；已批准的内容只能由持有 `entry.withdraw` 的人按 W8.10 下架。
- **W8.9** **保留**：
  - 候选转为 `returned` 或 `withdrawn` 后，服务端在 1 小时内删除 `title`、`body`、`note`，只留 hash 与元数据，不依赖任何请求。
  - `approved` 候选的正文就是文档原件，按 #997 处理。
  - 候选元数据保留到终态后 90 天（同 W3.3）。
- **W8.10** **已发布条目下架**（`entry.withdraw`，目标是任何未下架的 `origin: "entry"` 文档；普通文档得 `forbidden/operation_not_allowed`）。
  - **谁能下架**：持有 `entry.withdraw` 的人，经嵌入页 UI 会话发起，复验同 W1.6。按 W2.5，只有组织管理员能下架；个人 Lite 由本人下架；作者不是管理员时不能下架。
  - **效果**：
    - 取消在途 job、停止增强（`document_withdrawn`），之后不再激活构建；
    - 文档**立即**不再被 search 返回，read 得 `not_found`，也不出现在文档列表与计数里；
    - 引用它的 ContextPack 下次发布或加载得 `source_withdrawn`；
    - 原件不删，索引产物的清理按 #997。这里的语义是「先停供，再清索引」：下架之后，即使索引产物尚未清理，也不能被检索到。
  - **与新修订的关系**：下架后，对该文档的修订、重新处理一律按不存在处理，指向它的 `submitted` 候选在下次批准时得 `returned/target_withdrawn`；要恢复，只能以 `create` 模式提交新候选，得到一篇新文档。
  - 重复下架返回 `already: true`。
- **W8.11** 条目内容被检索或注入时是数据，不获得系统指令优先级。
- **W8.12** **审计**：
  - 记录事件 `knowledge.candidate.submit`、`.withdraw`、`.return`、`.approve`，以及 `knowledge.entry.withdraw`。
  - 每条字段：principal、`candidate_id`、`content_sha256`、目标 KB、`mode`、`knowledge_id`、基础修订、`kind`、`return_reason`、`author_is_approver`、`provenance.session_ref`、`skill_version`、`redaction_rules_version`；`entry.withdraw` 另记 `source_revision`。
  - 不记 `title`、`body`、`note`。
- **W8.13** **条目 hash（方案 1）**：
  - 服务端**不做 Unicode 规范化**，只对收到的确定字节计算；规范形式由客户端在保存草稿时就得到：`title` 不含首尾空白；`kind`、`title`、`body` 都不含 `\r`（含 `\r` 得 `invalid_request`）；三者都必须是合法的 Unicode 标量值序列，不含孤立代理项（JSON 里的 `\uD800` 之类），含孤立代理项的请求在 W1.9 ② 得 `invalid_request`，因为 ECMAScript 会把它转义输出、其他实现可能拒绝解析，两侧算不出同一个 hash。客户端算 hash 前检查（如 `String.prototype.isWellFormed`）。宿主建议在保存草稿时做 NFC 与换行统一，再按存下的文本计算 hash。
  - `content_sha256` = 小写 hex 的 SHA-256(UTF-8(`JSON.stringify([1, kind, title, body])`))。数组首位的 `1` 是方案号；序列化与 ECMAScript `JSON.stringify` 相同（不留空白；非 ASCII 字符原样输出 UTF-8，不转义成 `\u`；控制字符按其规则转义）。
  - 固定样例（含中文、emoji、引号、制表符与控制字符）见 `protocol/knowledge/fixtures/ops-v2/entry-hash.samples.json`，Rust 与 TS 两侧各自重算。改方案要升方案号，按 §0.3 属于语义变更。
- **W8.14** **机器可验证部分**：请求与响应的 schema 在 `protocol/knowledge/ops-v2/`（`candidate.schema.json`、`entry.schema.json`），用例在 `protocol/knowledge/fixtures/ops-v2/schema-cases.json`；`packages/contract-tests/src/knowledge-ops-v2.test.ts`（Ajv）与 `rust/crates/agent24-protocol/tests/knowledge_ops_v2.rs`（jsonschema）对每条用例给出同一结论，并核对 §6 的枚举与 schema 一致。schema 表达不了的规则（`message_range` 的先后、按字节的大小、受限 Markdown 白名单、hash 本身）由服务端与实现任务的测试证明。

## 9. 与 READONLY、STATE、UI-BRIDGE 的接口约定

- **I1（READONLY r8）**：E、凭据错误、`not_found` 的形状、信封、未知值兜底分别按 R2.1、READONLY §6、R2.6、R6.1、R6.2；R7.7 的"追不回"适用于全部写操作（W1.8）。需要 READONLY 下一修订配合（修订前不实现写操作）：R2.7 的凭据操作范围按 §2 操作类扩展到写操作；R9.2 `supported_operations` 增加写操作的能力声明（现只有 search / read）；R8.2 就绪 `operations` 增加 `pack`、`candidate` 的就绪键（现只有 search / read / ingest），R6.4 的"操作被关闭"按这些键判定（W1.4、W1.9 ②）；R2.6 的"越界与不存在不可区分"扩到 job、批准记录与 ContextPack（W1.9 ③）；条目下架与候选排除成为 search / read / 文档列表 / 计数的过滤条件（W8.3、W8.10）；用户凭据的操作范围不含 `ingest.*`，「用户不直接写」由范围保证（凭据范围的表达与就绪键由 WeKnora#135 定）；READONLY §6 表对各 `code` 的 `reason` 约束（如 `invalid_request`"其余情况不带"、`stale` 只带 `anchor_missing`）只约束只读操作，写操作按本文 §6 携带新增 reason，`not_found` 仍禁止 reason（按 READONLY §0.3 属兼容新增，客户端忽略未知 reason）；取消后的文档级 read 暂按 `index_failed`。
- **I2（STATE 规范核心 v1（同批））**：写操作与加载的准入为可服务且准入开放（STATE §9 OPS 条；额外条件见 W1.4、W7.9），不满足按 STATE §6 给本地结果；W1.4 的操作被关闭用 `unavailable` + `operation_disabled`（READONLY 已有的 reason，`retryable: false`），合乎 S6.1「只挂在 `unavailable` 上、不新增 code」，但 STATE §6 表尚无这一行，需 STATE 下一修订补上；结果未交给调用方的写请求（作废、S4.6 丢弃、追加前复查换码）由本文加 `write_outcome`，待决写记录是宿主侧记录、按 STATE S1.10 的先持久后生效原则先持久后发出（W1.5，STATE §9 委托）；写响应按 S4.6 接受、凭据类响应按 S3.1，服务端只回显 generation（R7.5），本文不新增递增事件；写响应（含 `pack.load`）都不是 S5.10 的输入，`scope_mismatch` 只触发 W7.14 (d) 的包级占位、不计入撤权窗口；ContextPack 证据按 S5.6 记多个 KB、任一被覆盖即占位（W7.14）；`request_in_progress` 不触发就绪探测（W3.2，STATE §9）；run 的 binding 集合决定加载范围（S4.7；W7.9）；条目草稿取自用户选中的会话，绑定到提交时的 binding（W8.1），binding 变化后的草稿要重新选目标库；多 binding 时写操作只发目标 binding、加载结果按 binding 分开（S4.9）。STATE 提到的"经本契约删除 KB"在 v1 不存在，只对应 W1.11 的非 v1 途径。
- **I3（UI-BRIDGE 规范核心 v1（同批））**：session JWT 只能执行 `candidate.review`、`candidate.approve`、`entry.withdraw`，且只经集成闸门精确表与宿主 UI 路由表（`knowledge-ui-routes.json`）里为它们列出的入口；其余写操作按 `credential_invalid`。READONLY r11 的「UI ops 只读」（R2.8）与 UIB U1.3 需下一修订把这三类列进去（WeKnora#147 嵌入页审阅）。桥消息闭集与禁止键不变（W1.6）。
- **I4（内核接口，需知会 jason）**：写操作的 run 归属需要 RunCreate 带 principal 与 profile（KB-HOST-01）；自动注入需要统一注入入口（KB-CTX-02；本版不自动注入）；`token_count` 的计数方法由 KB-CTX-01 / KB-CTX-02 定。

## 10. 已定决定（结论）

- #A1 矩阵每格待 #996；v1 全部拒绝，只在样本租户测试；job 只有发起人可读可取消。
- #A2 服务端不收 URL；宿主本机抓取后按文件提交。
- #A3 模型只能起草（ContextPack 草稿、条目草稿），提交由人在宿主界面发起，审阅与批准由人在嵌入页发起。
- #A4 职责分离按 #996（David 2026-10-25）：允许作者批准自己的候选，记 `author_is_approver`；双人批准是以后的组织选项（W2.4）。`pack.publish` 批准由发布人出示。
- #A5 取消、失败、撤权、撤回都不删原件与产物，如实报告；删除功能归 #997。
- #A6 受众种类与设定权待 #996；v1 随 #A1 拒绝。
- #A7 加载要求加载者能读全部来源（`scope_mismatch`）。条目写入目标 KB 后读者可多于原会话的参与者，这是有意设计，靠宿主脱敏与人工批准把关。
- #A8 已发布条目下架不需另一份批准，由持有 `entry.withdraw` 的人执行；只给组织管理员（W2.5，General 2026-11-09 按 #996 推论定）。
- #A9 撤销、过期、失效的已注入包内容在后续调用中换成占位。
- #A10 取消一次替换不撤销替换，当前源修订不回退。
- #A11 加载类别顺序 `personal` → `org` → `group`。
- #A12 v2 支持异人审批：候选提交到服务端候选表（#1667 R0 一致 B，`~/.claude/decisions/2026-11-09-m3-intake/`）；跨设备草稿不做（M3 不做）。
- #A13 Off、切换、注销不取消在途 job，动作前提示并给取消入口；binding 退役同样提示；local-managed 停用或移除会中断 job（W1.10）。
- #A14 删除或清空 KB 先下架其中未下架的条目、退回以该 KB 为目标的全部 `submitted` 候选；凭据不能删除或清空 KB（W1.11）。
- #B1 `authz_version` 粒度为主体 × 资源，可保守多变。 · #B2 后台复验建立 job 的操作类 + 凭据未撤销 + 政策。 · #B3 / #B20 幂等与 job 记录至少 90 天。
- #B4 去重键为 KB + SHA-256 + 规范化文件类型。 · #B5 命中失败或取消的文档返回 `existing`，不自动重做。 · #B6 `config_version` 不进默认库唯一键。
- #B7 上游 `finalizing` 映射为 `indexed` + `enrichment: "running"`。 · #B8 取消、失败不回退当前源修订。 · #B9 对无工作的 job 取消返回 200 `cancel_effect: "none"`。
- #B10 发布人失去来源读取或 `pack.publish` 则包失效。 · #B11 任一来源变化整包失效。 · #B12 有效期上限 90 天。 · #B13 只在发布前截断，加载放不下即跳过。
- #B14 / #B23 单一注入预算、确定全序（类别 → profile → `pack_id`），宿主整包裁定。 · #B15 只能整句原文摘录。 · #B16 候选在服务端候选表，不进索引、不建 job（W8.2、W8.3）。 · #B17 批准有效期 1 小时（只剩 ContextPack 的批准记录）。
- #B18 基础修订冲突拒绝写入，重新批准。 · #B19 批准失败统一 `approval_invalid`，本人记录细分 `approval_*`。 · #B21 `chunk.edit` 带政策版本、复验预算、报告费用。 · #B22 每主体 `active` 批准上限 20（ContextPack）；每主体同时 `submitted` 的候选上限 20（W8.2）。
- #B24 占位触发见 W7.14；被新版本取代也触发占位（(b)，不以新版实际注入为前提），由 Grok、Codex 讨论一致后 David 2026-10-20 确认采用。

## 11. 非规范材料

完整版（含转移表、验收用例清单、现状证据与参考设计）保留在分支 `ab/weknora-03c-ops`（#1075 的分支），由实现任务以测试证明，不约束本契约；两者冲突时以本核心为准。

## 12. v2 相对 v1 的语义变更（按 §0.3 升主版本）

v1 从未实施，所以下面的改动只在文档层；服务端与客户端都按 v2 实现。

1. 操作类：删 `sop.candidate`、`sop.approve`、`sop.commit`、`sop.withdraw`；增 `entry.draft`（宿主本地）、`candidate.propose`、`candidate.review`、`candidate.approve`、`entry.withdraw`（W2.1）。条目文档 `origin` 由 `sop` 改为 `entry`，SOP 成为 `kind` 的一个取值（§6）。
2. 候选位置：由「只在宿主本地、批准前服务端无记录」（v1 W8.2、#A12、#B16）改为「服务端候选表，只存不索引」（W8.2–W8.4）。来源：#1667 第 1 件 R0 一致 B。
3. 批准：由「批准记录 + 单独 `sop.commit`」改为「`candidate.approve` 原子地复验、记批准、建 job」（W8.5）；条目不再使用 `approval.*`，1 小时有效期与 20 条上限只剩 ContextPack。
4. 页面会话：由「不能执行任何写操作」改为「只能执行 `candidate.review`、`candidate.approve`、`entry.withdraw`」（W1.6、W2.3、I3），残余风险写明；桥不变。
5. 职责分离：服务端看得见提交人与批准人，记 `author_is_approver`（W2.4，#996）。
6. 撤回拆成两条：候选撤回（作者，`submitted → withdrawn`，W8.8）与已发布条目下架（`entry.withdraw`，W8.10）。
7. 新增条目 hash 方案 1（W8.13），新增 `stale/candidate_not_submitted`、候选状态与 `return_reason`（§6）。
8. 宿主义务写明（W8.1）：准入 Ordinary + 显式确认（David 2026-11-09 定）、规则表是唯一硬闸门、只取用户与助手正文。
9. §8 报文带公共字段（W1.4、W8.14）：Provider 发出的请求带 `request_id` 与 `generation`，UI 会话发出的只带 `request_id`；响应回显 `request_id`（Provider 的还回显 `generation`）并带 `authz_version`（W1.2）。v1 没有 schema，这一条只是把 W1.2、W1.4 写进 schema。

v1 文件改名为本文件（`git mv`），引用已同步；`rust/apps/agent24d/src/knowledge_probe.rs:18` 的注释仍写 OPS-v1，按 M3「不碰 agent24d」约束留到下次修改该文件时一并改。
