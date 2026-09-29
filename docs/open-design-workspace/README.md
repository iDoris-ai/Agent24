# Agent24 × Open Design workspace

本目录保存 Agent24 与 Open Design 融合工作的原始讨论、已选技术路径和待评审实施计划。

## 当前状态

- 当前 integration 分支：`integration/open-design-main-sync-wave20`
- integration head：`d62f65ad97c2c68a9ed8aff99407685b5046b28d`
- 已包含当前 `origin/main@32072b02a3c7`；当前 integration 相对 main 只有 Open Design 历史增量，没有漏同步的 main commit。
- 当前有效 A24-OD-02 栈：**#555 → #556 → #558 → #559 → #560 → #561 → #562**；#557 为独立文档台账。
- 状态：**P0 / P1 已完成；A24-OD-00 authority 基础已进入主线；P2 workspace contract 正在完成 run admission、terminal lease release 与 restart orphan reconciliation，仍未激活 Creative/ACP/runtime。**
- 最新执行台账：[PROGRESS-2026-09-28.md](PROGRESS-2026-09-28.md)
- 历史与长期门禁：[STATUS.md](STATUS.md)

## 文档

- [原始共享会话](source/chatgpt-shared-conversation-2026-09-19.md)
- [实施计划（待 review）](PLAN.md)
- [如何启动与多 Agent 执行规则](EXECUTION.md)
- [Agent24 主干依赖台账](AGENT24-DEPENDENCIES.md)
- [执行状态、PR 栈与门禁证据](STATUS.md)
- [2026-09-22 阶段进展与 Workspace 概念模型](PROGRESS-2026-09-22.md)
- [2026-09-28～29 A24-OD-02 小步执行台账](PROGRESS-2026-09-28.md)

## 原始会话完整性

原始会话文件是通过 agent-reach 的网页阅读路径，从以下公开分享页取得的 Markdown 快照，保存时未增删或改写任何内容：

`https://chatgpt.com/share/6aae8de8-f8ec-83ec-942a-3a32c0fa8cac`

- 抓取日期：2026-09-19（Asia/Bangkok）
- 行数：976
- 字节数：21,585
- SHA-256：`65bbf892f1fd516dd7db1d88299bc4bc0c094f2d311b569550de7b0a0af172e6`

该文件是网页阅读器返回内容的逐字节归档，不是重新整理的摘要，也不是浏览器 DOM/HTML 存档。

## 协作约定

原始 `Agent24` worktree 保持在 `main`，供当前其他工作继续使用；Open Design 工作继续使用独立 worktree/stack。若 `main` 有新变化，先在 integration 分支审计并采用 ordinary merge 同步；已发布 PR 栈禁止 rebase/force-push。已明确 superseded 或已合并且 clean 的旧 worktree 可以清理，但不得覆盖仍有 unique unfinished 内容或仍承担 OPEN PR 的施工分支。
