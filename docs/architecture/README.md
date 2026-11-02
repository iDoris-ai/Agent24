# 架构总入口

索引：[ADR 索引](../adr/README.md)・本页是架构总入口

这里是看 Agent24 架构的首页。不用在 `docs/architecture/` 和 `docs/adr/` 之间来回找，从这一页出发，两边互相链接。

## architecture 和 ADR 的分工

**architecture 讲「现在是什么样、往哪走」**：结构、分层、现状、评估，会随代码更新，冲突时以代码为准。**ADR 讲「为什么这样定」**：一份一个决策，写明背景、备选、结论，定下来后不改写，被推翻才标被替代。想知道某层长什么样，看本页和分层总览；想知道某个设计为什么是这样，顺着本页链到对应的 ADR。全部 ADR 的清单、状态和编号冲突在 [ADR 总索引](../adr/README.md)。

## 先读这几份

按这个顺序读：

1. [ADR-039 概念模型](../adr/ADR-039-concept-model.md)：总纲。一个本体、两个概念（能力、技能），之后所有扩展类 ADR 都用它的名词。已接受。
2. [架构分层总览](../ARCHITECTURE-LAYERS.md)：L0 到 L7 各层是什么、依赖关系、未完成清单。
3. [可进化性评估（终稿）](EVOLVABILITY-ASSESSMENT-2026-11-01.md)：现在的结构能不能支撑持续进化、组件换了影响谁、自进化和训练的缺口。
4. [Super Agent 演进（终稿）](../research/deepseek-superagent/SUPERAGENT-EVOLUTION-2026-11-01.md)：向持续工作的 Super Agent 演进要补哪些机制，对应 ADR-040 至 ADR-043。
5. [ADR-038 组件契约 v1](../adr/ADR-038-component-contract-v1.md)：组件怎么声明、授权、签名。草案 r3。

## 按层和子系统

层的划分见[分层总览](../ARCHITECTURE-LAYERS.md) 第 2 节。下表每行：架构说明、对应 ADR、设计文档。状态以各文件自述为准，细节在 [ADR 索引](../adr/README.md)。

### 契约与内核（L0 到 L5）

| 主题 | 架构说明 | ADR | 设计与规格 |
|---|---|---|---|
| 总体架构与分层 | [分层总览](../ARCHITECTURE-LAYERS.md)；[M1 技术判断与边界](../agent/architecture.md)；[内核边界图](kernel-boundary.html) | ADR-026（Rust Core）、ADR-037（产品形态，提议）；均在 [决策日志](../decision.md) 与 [ADR-026](../ADR-026-rust-core-polyglot.md) | [协议规范 SPEC-002](../specs/SPEC-002-protocol.md)、[工程规范 SPEC-001](../specs/SPEC-001-engineering.md)；[产品形态事实清单](../tracking/2026-10-26/SOURCE-ADR-037-facts.md) |
| 审批、权限、审计 | [分层总览 §4 安全与权限边界](../ARCHITECTURE-LAYERS.md) | [ADR-K1-01](../adr/ADR-K1-01-module-tool-contract.md)、[K1-02](../adr/ADR-K1-02-session-dataflow-security-modes.md)、[K1-03](../adr/ADR-K1-03-enable-authorization-trust-revocation.md)、[K1-04](../adr/ADR-K1-04-audit-and-shared-authorization.md)；日志 ADR-016（模块安全）、ADR-017（隐私） | [审批法律](../laws/APPROVAL.md)；[K1-06a 授权路径](../design/K1-06a-grant-path.md)、[资源范围](../design/K1-06a-resource-scope.md)、[出口矩阵](../design/K1-06b-egress-matrix.md)；[T8 启用准入](../design/T8-ME3g-enable-admission-gate.md)；[内核计划 PLAN-KERNEL-K1](../agent/PLAN-KERNEL-K1.md) |
| 持续运行：入站、动作、作业 | [Super Agent 演进终稿](../research/deepseek-superagent/SUPERAGENT-EVOLUTION-2026-11-01.md) | [ADR-041](../adr/ADR-041-inbound-events-admission-and-work-items.md)、[ADR-042](../adr/ADR-042-durable-actions-and-reconciliation.md)、[ADR-043](../adr/ADR-043-background-jobs-concurrency-and-supervision.md)（均提议） | [关停可观测 SHUT](../design/SHUT-shutdown-observability.md)；[定时回调 ME4-S1](../design/ME4-S1-scheduler-callback.md)；[run 失败类型 FU-57](../design/FU-57-run-failure-kinds.md) |
| 迁移与启动保护 | [分层总览 §7 尚未完成](../ARCHITECTURE-LAYERS.md) | 日志 ADR-035（token 交付） | [CORE-5 迁移保护](../agent/CORE-5-MIGRATION-GUARD-DESIGN.md)；[CORE-2 接入注册](../agent/CORE-2-ENROLL-DESIGN.md)；[iDoris launcher](../agent/IDORIS-LAUNCHER-DESIGN.md) |

### 扩展面：领域 OS、组件、技能（L5½ 与 L6）

| 主题 | 架构说明 | ADR | 设计与规格 |
|---|---|---|---|
| 概念与组件 | [可进化性评估](EVOLVABILITY-ASSESSMENT-2026-11-01.md) | [ADR-039](../adr/ADR-039-concept-model.md)、[ADR-038](../adr/ADR-038-component-contract-v1.md) | [组件路线图](../agent/COMPONENT-ROADMAP.md) |
| 领域 OS 与进程外模块 | [分层总览 §2 L5½](../ARCHITECTURE-LAYERS.md) | 日志 ADR-029（边界与挂载缝）、ADR-031（进程外协议）、ADR-032（AgentEar、iDoris 接入） | [附着式模块 A3](../design/A3-ATTACHED-MODULE.md)；[进程外 WIRE](../specs/WIRE-OOP-MODULE.md)；[ME-3 规格](../specs/SPEC-ME3-OUT-OF-PROCESS.md)；[OS 能力计划 PLAN-ME4](../agent/PLAN-ME4-OS-CAPABILITIES.md)；[OS SDK ME4-S3](../design/ME4-S3-os-sdk.md) |
| 技能 | [Super Agent 演进终稿](../research/deepseek-superagent/SUPERAGENT-EVOLUTION-2026-11-01.md) | [ADR-040](../adr/ADR-040-skill-contract-v1.md)（提议） | [技能分发规格](../specs/SPEC-SKILL-DISTRIBUTION.md) |
| Documenting | [Documenting README](../documenting/README.md)、[基线](../documenting/BASELINE.md) | [ADR-DOC-01](../documenting/adr/ADR-DOC-01-placement-and-integration.md)、[DOC-02](../documenting/adr/ADR-DOC-02-operation-contract.md)、[DOC-03](../documenting/adr/ADR-DOC-03-docx-engine-and-conversion.md) | [DOC-1-06b 模块工具](../design/DOC-1-06b-module-tools.md) |
| 知识库 | [定位与差异](../knowledge/POSITIONING-AND-DIFFS.md) | [ADR-KB-01](../knowledge/ADR-KB-01-SINGLE-CONVERSATION.md) | [操作契约](../knowledge/CONTRACT-OPS-v1.md)、[只读契约](../knowledge/CONTRACT-READONLY-v1.md)、[状态契约](../knowledge/CONTRACT-STATE-v1.md)、[UI 桥契约](../knowledge/CONTRACT-UI-BRIDGE-v1.md)；[计划](../knowledge/PLAN-KNOWLEDGE-TASKS.md) |
| Open Design 创作区 | [Open Design 工作区 README](../open-design-workspace/README.md)、[P0 冻结索引](../open-design-workspace/design/README.md) | [ADR-OD-001 至 006](../open-design-workspace/design/README.md)（文件名 ADR-001 至 ADR-006，与日志同号，见 ADR 索引） | [兼容矩阵](../open-design-workspace/design/COMPATIBILITY-MATRIX.md)、[风险登记](../open-design-workspace/design/RISK-REGISTER.md)；[落地策略](../design/OPEN-DESIGN-INTEGRATION-LANDING-STRATEGY.md)；[iDoris 版改造计划](../design/OPEN-DESIGN-IDORIS-EDITION.md) |

### 记忆

| 主题 | 架构说明 | ADR | 设计与规格 |
|---|---|---|---|
| 记忆架构与生命周期 | [记忆法律](../laws/MEMORY.md)；[记忆策略](../research/MEMORY-STRATEGY.md) | 日志 ADR-028（分层，暂定）、ADR-030（所有权维度）；[MEMORY-P1](../adr/ADR-MEMORY-P1-LIFECYCLE-AND-PURGE.md)、[召回消息来源](../adr/ADR-MEMORY-RECALL-MESSAGE-ORIGINS.md)（均提议） | [SPEC-MEMORY](../specs/SPEC-MEMORY.md)、[SPEC-MD-ME](../specs/SPEC-MD-ME.md)；[PLAN-MEMORY-P1](../agent/PLAN-MEMORY-P1.md)；[P1-02C 配额失败设计](../agent/P1-02C-QUOTA-FAILURE-DESIGN.md)；[R05-1 精确契约](../agent/R05-1-EXACT-CONTRACT.md) |
| 上下文 | [上下文法律](../laws/CONTEXT.md) | | |

### 通信与外壳（L7）

| 主题 | 架构说明 | ADR | 设计与规格 |
|---|---|---|---|
| 通信（Hyphae、Nostr） | [分层总览](../ARCHITECTURE-LAYERS.md) | 暂无独立 ADR | [COMM-HYPHAE](../design/COMM-HYPHAE.md)、[HYPHAE-CLI-INTEGRATION](../design/HYPHAE-CLI-INTEGRATION.md)、[F4 Nostr 通道](../specs/F4-nostr-channel.md)；[COMM-FRIENDS 好友机制](../design/COMM-FRIENDS.md)（设计稿，已拍板 2026-11-02，排在 ADR-038 C1–C3 之后） |
| 桌面、TUI、移动 | [分层总览 L7](../ARCHITECTURE-LAYERS.md) | 日志 ADR-027（部分被 ADR-037 取代）、ADR-037 | [Windows 移植](../design/WINDOWS-PORT.md)；[多平台发布计划](../Deployment/RESEARCH.md) |

### 模型与硬件

| 主题 | 架构说明 | ADR | 设计与规格 |
|---|---|---|---|
| 运行时、网关、硬件分档 | [分层总览](../ARCHITECTURE-LAYERS.md) | 日志 ADR-019（LLM Gateway）、ADR-022（运行时，已收窄）、ADR-032、ADR-036 | [HW-TIERS](../design/HW-TIERS-capability-hardware-matrix.md)；[AgentEar 与 iDoris 接入](../design/INTEGRATION-AGENTEAR-IDORIS.md)；[ME4-S2 模型回调](../design/ME4-S2-model-callback.md) |
| 决策模型 | | 日志 ADR-033、ADR-034 | [PLAN-DECIDE](../agent/PLAN-DECIDE.md)；[DECISION-MODELS](../research/DECISION-MODELS.md) |

## 评估与研究：三步评审记录

两条线都按同一个流程走：**初稿 → Codex 独立评估 → 裁判整合出终稿**。初稿和 Codex 版保留原文，终稿是引用的依据。

| 线 | 初稿 | Codex（gpt-6-astra）独立评估 | 终稿（裁判整合） |
|---|---|---|---|
| 可进化性评估 | [Opus 初评](EVOLVABILITY-ASSESSMENT-2026-11-01-opus.md) | [Codex 评估](EVOLVABILITY-ASSESSMENT-2026-11-01-codex-astra.md) | [终稿](EVOLVABILITY-ASSESSMENT-2026-11-01.md) |
| Super Agent 演进 | [Fable 初稿](../research/deepseek-superagent/SUPERAGENT-EVOLUTION-2026-11-01-fable.md) | [Codex 评估](../research/deepseek-superagent/SUPERAGENT-EVOLUTION-2026-11-01-codex-astra.md) | [终稿](../research/deepseek-superagent/SUPERAGENT-EVOLUTION-2026-11-01.md) |

演进线的补充材料：[来源清单 SOURCES](../research/deepseek-superagent/SOURCES.md)；[ADR 初稿目录](../research/deepseek-superagent/adr-drafts/README.md)（初稿与最终 ADR 的对照）。

其他评审：[2026-10-27 代码评审](../review/CODE-REVIEW-2026-10-27.md)，其中[附录 A 架构与分层](../review/2026-10-27/A-architecture.md)。

## 跟踪文档

仓库目前没有「架构 + 方案 / 里程碑 / 进度」三份带日期的统一跟踪文档（`docs/tracking/2026-10-26/` 下只有一份[事实清单](../tracking/2026-10-26/SOURCE-ADR-037-facts.md)）。现有的跟踪文档：

- 状态权威：[tasks](../agent/tasks.md)、[progress](../agent/progress.md)、[roadmap](../agent/roadmap.md)、[followups](../agent/followups.md)
- 更早的里程碑线：[specs/TASKS.md](../specs/TASKS.md)、[ROADMAP](../ROADMAP.md)、[PLAN](../PLAN.md)
