# Agent24 Rust Core 开发 Spec 体系

> 本目录是 ADR-026（Rust Core + Polyglot Worker 架构）的**可执行落地规范**，
> 专为 `/loop` 自主开发循环设计。
> 创建日期：2026-07-23 | 维护者：jason + Claude Code

## 文档索引

| 文件 | 内容 | 谁读 |
|---|---|---|
| [`LOOP.md`](./LOOP.md) | **loop 执行手册**——每轮迭代的算法、停止条件、汇报格式 | loop 会话每轮必读 |
| [`TASKS.md`](./TASKS.md) | **任务队列 + 状态跟踪**——全部里程碑的原子任务拆分，loop 的唯一工作来源 | loop 会话每轮必读 |
| [`SPEC-001-engineering.md`](./SPEC-001-engineering.md) | 工程规范：分支/PR/review 流程、编码标准、测试要求、CI、DoD | 实现任何任务前 |
| [`SPEC-002-protocol.md`](./SPEC-002-protocol.md) | v1 协议规范:数据结构、REST API、WS 事件、错误格式、认证 | 实现协议相关任务前 |

## 上游依据（冲突时以下列为准，但需先向用户报告冲突）

1. [`docs/ADR-026-rust-core-polyglot.md`](../ADR-026-rust-core-polyglot.md) — 架构决策 + §6.5 十一条设计硬约束
2. [`docs/reference-notes/codex.md`](../reference-notes/codex.md) — 审批/事件/TUI 设计参考
3. [`docs/reference-notes/openfang.md`](../reference-notes/openfang.md) — 整体架构参考 + 避坑清单
4. `docs/decision.md` — 历史 ADR（注意 ADR-023 已被 ADR-026 取代）

## 一段话总目标

把 Agent24 从「Electron 壳 + Node daemon」演进为「**Electron/TUI 外壳 + Rust 内核（agent24d）+ Python ML Worker**」，
交付一个 **24/7 常驻、可定时执行日常工作流、本地小模型优先、审批可审计**的个人/社区 Agent 平台，
并在 M-C 末发布 **v0.1.0 初始版本**。产品不是 coding agent；核心资产是调度器、工作流、记忆、审批、渠道。

## 里程碑总览与发布节奏

```
M-A  契约冻结 + 仓库重构     →  mock daemon 就绪，日常开发不阻塞
M-B  Rust 最小 daemon        →  tag v0.1.0-alpha（双后端可切换）
M-C  Agent Loop + 调度 + 审批 + TUI →  🚀 release v0.1.0（初始版本：dmg + CLI）
M-D  记忆 + 模型三层路由 + Guardian →  v0.2.0
M-E  模块生态桥接（node-host / MCP / PGL）→  v0.3.0
M-F  24/7 化 + 渠道（微信/Nostr）  →  v0.4.0
```

详细任务见 `TASKS.md`。每个任务 = 一个 PR = 一次完整的「实现 → 自我 review → Codex review → 提 PR」循环（见 SPEC-001 §3）。
