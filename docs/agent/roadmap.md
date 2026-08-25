# Agent24 Roadmap — Milestone → Feature

> 「未来要做什么」。具体怎么做 + 验收见 [`tasks.md`](tasks.md)。
> 编号：M\<里程碑\> → F\<里程碑\>.\<序号\>。记录日期：2026-08-23（2026-08-25 追加 M4/M5/M6）

**当前开发轮次只覆盖一件事**：把「Information / Context / Memory」从**已交付的底座**变成**在用的产品**（M1）。
M4–M6 是 2026-08-25 追加的**产品线**（Cos72 workspace），依据见
[`../reference-notes/macro.md`](../reference-notes/macro.md) §12 —— **登记在案，不与 M1 并行**。
长期设计与决策记录在 [`../decision.md`](../decision.md)（ADR-028/029/030）与 [`../specs/`](../specs/)，不在这里重复。

---

## M1 — 记忆成为产品

**目标**：一次真实对话在 M-D 的 EventLog 里留下**可重放**的事件；领域 OS 的记忆彼此隔离且**归属清晰**；「能不能访问」这件事第一次**有一个被问到的地方**。

> **为什么是这三件、按这个顺序**：M-D 的 12 个模块 crate 外引用**全为 0**，agent loop 用的还是 M-D 之前的 `CanonicalSession` —— 底座交付了，没接进去（`SPEC-ME-FOLLOWUPS.md` F2）。
> 而 agent loop 那份记忆今天用**裸 user id** 做 key，游离在 ADR-030 的空间模型之外（硬门槛 3）。
> **先接线再搬家，等于让 F1.3 建在一个已排定要移动的分区上** —— 所以顺序是 F1.1 → F1.2 → F1.3，不能调。

- **F1.1 判定接缝（原 F8b）** — 内核持有的 `Authorizer`，签名一次到位；默认实现**行为零变化**。贵的是调用点，不是策略逻辑。
- **F1.2 personal space（原 F8c）** — agent loop 自己的记忆搬进空间模型，消掉 ADR-030 硬门槛 3 那条例外。
- **F1.3 记忆接进 agent loop（原 F2）** — 会话轮次写进 EventLog；`Condenser` 取代 `CanonicalSession` 的压缩，后者降级为投影。

## M2 — M-E 收口余项（本轮不做，登记在案）

目标：把 `SPEC-ME-FOLLOWUPS.md` 剩下的债清掉。

- **F2.1 F4** — #134 复审判定单独治的五条（OpenAPI 缺端点、阻塞锁上 tokio worker、`os list` 临时 daemon 状态…）
- **F2.2 F3** — 命名空间 `/api/v1/os/<name>`（建议裁决已给，见 ADR-030 后续顺序讨论）
- **F2.3 F6 / F7** — 模块目录 symlink 安全（需 `openat`）/ 领域 OS 记忆配额

## M3 — 组织化（**等第二个真实用户**，不排期）

目标：ADR-030 的 F9/F10/F11。

- **F3.1** grants + groups + 交集判定 + 审计事件
- **F3.2** 持久化 Workspace
- **F3.3** `asserted_by` + 冲突断言并存

---

## M4 — Cos72 Workspace 底座（**依赖 M1 全部完成**）

**目标**：一条社区消息能被搜到、被 @link、被 agent 当作上下文用；而这条消息**在 EventLog 里留下事件**。

> **为什么排在 M1 之后，而不是并行**：workspace 的全部价值依赖「事件真的落进 EventLog、且按空间隔离」。
> **M1 F1.3 没做完，Cos72 写出来的事件就没有权威的地方可去** —— 和本文档拒绝调整
> F1.1→F1.2→F1.3 顺序的理由是同一条：不在一个已排定要移动的分区上盖房子。
>
> **为什么值得做**：M-D 记忆底座今天的 12 个模块 crate 外引用**全为 0**（`SPEC-ME-FOLLOWUPS.md` F2）。
> M1 让 agent loop 成为第一个消费者；**M4 让它有一件持续的、真实的工作**。

**边界（三条，不可动摇）**：
1. `CosEntity`（模块内部对象地址）与 `SpaceId`（内核分区键）是两件事，**不得混用**；模块面向的 API
   里**依然不出现 space 参数**（`architecture.md` 不可动摇边界第 2 条）。
2. 双向图存在**模块自己的库**（`~/.agent24/os/cos72/cos72.db`），**不进 `agent24-memory` 的 11 张表** ——
   否则 ADR-029 的边界线白画。
3. workspace 的每个有意义的动作**都经 `EventSink` 落一条事件**。

- **F4.1 `CosEntity` 与双向图** — 统一地址 + 边表 + 迁移。验收：@link 后两端互查得到；任一端删除时边的行为**有定义**。
- **F4.2 统一查询面** — 过滤 AST + 游标分页 + 混合类型一页 + frecency 排序。验收：乱序插入下游标翻页**无重无漏**。
- **F4.3 事件落底座** — 验收：重放 EventLog 能还原 workspace 状态摘要；**F1/F8 的跨模块隔离探针全部继续通过**。
- **F4.4 Agent 工具面** — `workspace.search` / `get` / `link`。验收：一次调用拿到跨类型结果（别让 agent 绕弯）。

## M5 — Cos72 三件套上线（依赖 M4）

**目标**：Cos72 是一个真实社区**能装、能用**的东西。ADR-004 的 `myshop` / `mytask` / `myvote` 作为
`CosEntity` 的子类型落在 M4 的底座上。

- **F5.1 `mytask`** — 任务 ↔ 积分，**绑在消息上下文上**（任务追踪器过时，是因为它和对话不在一个系统里）。
- **F5.2 `myshop`** — 积分兑换。
- **F5.3 `myvote`** — 提案 / 投票；提案是 `CosEntity`，与 docs 共用编辑与权限。
- **F5.4 渠道接入** — 复用已有 F3 微信 / F4 Nostr 作为 workspace 的出入站边，**不新建渠道**。

**「定制部署」不单独立里程碑，是 M5 的交付条件**：
`agent24 os install cos72 && os activate cos72` + 一个二进制 + 一个 SQLite，能让一个真实社区跑起来
（ADR-029 已定义这条流水）。**这正是 Macro 那种 41 个 Pulumi stack 的架构放弃掉的那一格**
（见 [`../reference-notes/macro.md`](../reference-notes/macro.md) §8）。

## M6 — Workspace 扩展面（**等真实需求，不排期**）

Calls（录音转写）、Email（Gmail OAuth）、CRDT 协作编辑、桌面壳设计系统。

**不排期的理由**：这四样各自是一个季度的工作量，且**都不是社区场景的第一需求**。
没有真实社区在用之前做它们会把形状猜错 —— 与 M3 不排期是同一条理由。

---

> **当前聚焦：M1 / F1.1。** 每个 Feature 的 Task 拆分与状态见 [`tasks.md`](tasks.md)。
> M3 明确**不排期**：没有第二个真实用户时做授权 UX、角色层级、委托，会把形状全部猜错（ADR-030）。
> **M4/M5 已排序但未拆 Task** —— M1 交付前不拆，避免规划层写出一堆建立在未定形状上的验收标准。
