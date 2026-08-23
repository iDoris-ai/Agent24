# Agent24 实时状态 — progress

> 「此刻仓库真实发生了什么」。由 `pilot run` 每一步更新。
> 更新时间：2026-08-23 12:5x

## 当前聚焦

- **Milestone**：M1 记忆成为产品
- **Feature**：F1.1 判定接缝（原 F8b）
- **正在开发的 Task**：无（规划刚落地，尚未开工）
- **下一个动作**：T1.1.1 `Authorizer` 契约与默认实现

## 进行中 / 待回执的 PR

| Task | PR | 状态 | 备注 |
|:---|:---|:---|:---|
| —（F8，M1 的前置） | [#140](https://github.com/iDoris-ai/Agent24/pull/140) | PR_OPEN | CI 五项全绿；等外部评审裁决。**F1.1 的前置** |
| —（本规划 + ADR-030） | [#141](https://github.com/iDoris-ai/Agent24/pull/141) | PR_OPEN | 等外部评审裁决 |

## 阻塞项（BLOCKED）

- 无。

> ⚠️ **F1.1 的 task 依赖 #140 合并。** #140 未合并时 T1.1.1 不能开工 —— 它要用 F8 引入的 `SpaceId` / `OrgId`。若 #140 长时间无裁决，按 `reference/review-contract.md` 的超时路径如实汇报，**不要自己 approve**。

## 最近完成

- 2026-08-23 F8（PR #140 待合并）：记忆所有权维度改为 (org, space)，org 成为一等实体。五轮对抗复审，每轮找到的都是上一轮修复自己引入的问题。
- 2026-08-23 ADR-030 + SPEC-ORG-SPACE（PR #141 待合并）：组织/空间/授权/工作上下文的不变量与边界。Codex 复审 2 Critical / 14 High，两条 Critical 是文档自相矛盾。
- 2026-08-22 F1（PR #139，已合并）：`ScopedMemory`，两个领域 OS 不再共享记忆底座。
- 2026-08-22 F5（PR #138，已合并）：两处排序 + 一条比字面更弱的约束。

## 下一个 READY

- **T1.1.1** `Authorizer` 契约与默认实现（依赖：#140 合并）

## 本轮的三条纪律（从 F1/F8 二十余轮复审里带出来的）

1. **每条新回归测试都要变异验证** —— 把修复改回去，看它变红。过不了这关的测试，等于没写。
2. **不许出现比机制更强的措辞** —— 「schema 强制的隔离」「GDPR 就绪」「access control」在本轮都不成立。
3. **迁移只走目录，不前缀匹配** —— owner key 里含 NUL，`LIKE` 是 F1 逐条否掉过的做法。
