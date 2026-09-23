# Agent24 × Open Design workspace

本目录保存 Agent24 与 Open Design 融合工作的原始讨论、已选技术路径和待评审实施计划。

## 当前状态

- Git 分支：`feat/open-design-workspace`
- 独立 worktree：`/Users/jason/Dev/auraai/Agent24-open-design-workspace`
- 初始基线提交：`04ccd3a0f6271e2a5b54c4668e83cac1636127ae`
- 已审计 Agent24 主干至：`bb6f62bad5e75fdf64a375bb4acaec50f1230683`（#253 merge）
- 状态：**P0、A24-OD-00 安全门禁与 P1 基线门禁均 PASS；P2 workspace contract 执行中；Wave 9 状态见 [STATUS.md](STATUS.md)**
- 实时台账：[STATUS.md](STATUS.md)

## 文档

- [原始共享会话](source/chatgpt-shared-conversation-2026-09-19.md)
- [实施计划（待 review）](PLAN.md)
- [如何启动与多 Agent 执行规则](EXECUTION.md)
- [Agent24 主干依赖台账](AGENT24-DEPENDENCIES.md)
- [执行状态、PR 栈与门禁证据](STATUS.md)
- [2026-09-22 阶段进展与 Workspace 概念模型](PROGRESS-2026-09-22.md)

## 原始会话完整性

原始会话文件是通过 agent-reach 的网页阅读路径，从以下公开分享页取得的 Markdown 快照，保存时未增删或改写任何内容：

`https://chatgpt.com/share/6aae8de8-f8ec-83ec-942a-3a32c0fa8cac`

- 抓取日期：2026-09-19（Asia/Bangkok）
- 行数：976
- 字节数：21,585
- SHA-256：`65bbf892f1fd516dd7db1d88299bc4bc0c094f2d311b569550de7b0a0af172e6`

该文件是网页阅读器返回内容的逐字节归档，不是重新整理的摘要，也不是浏览器 DOM/HTML 存档。

## 协作约定

原始 `Agent24` worktree 保持在 `main`，供当前其他工作继续使用；本工作只在独立 worktree 上进行。若 `main` 有新变化，本分支先审计差异，再采用显式 merge 同步，不对已共享的集成分支做强制 rebase，也不覆盖其他 worktree 的未提交内容。
