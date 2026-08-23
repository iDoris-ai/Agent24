# Agent24 Roadmap — Milestone → Feature

> 「未来要做什么」。具体怎么做 + 验收见 [`tasks.md`](tasks.md)。
> 编号：M\<里程碑\> → F\<里程碑\>.\<序号\>。记录日期：2026-08-23

本轮规划**只覆盖一件事**：把「Information / Context / Memory」从**已交付的底座**变成**在用的产品**。
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

> **当前聚焦：M1 / F1.1。** 每个 Feature 的 Task 拆分与状态见 [`tasks.md`](tasks.md)。
> M3 明确**不排期**：没有第二个真实用户时做授权 UX、角色层级、委托，会把形状全部猜错（ADR-030）。
