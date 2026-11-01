# ADR 总索引

索引：[架构总入口](../architecture/README.md)・本页是 ADR 索引

本页列出仓库里**所有** ADR（架构决策记录）。ADR 记录「为什么这样定」，一份一个决策；「现在是什么样、往哪走」看 [架构总入口](../architecture/README.md)。

- 共 **60** 份 ADR：`docs/adr/` 下 12 份，仓库其他位置 48 份（见各表「位置」，文件均未移动）。另有 5 份已被取代的初稿，单列在末尾，不计入总数。
- 状态取自文件自述：已接受 / 提议 / 草案 / 被替代；文件里读不到就写「未标」。「原文」列出的是文件原话，供核对。
- `docs/decision.md` 是一份**合订的决策日志**，ADR-001 至 ADR-037（缺 ADR-026）都在里面，没有单独文件，所以这些行都链到同一个文件，用编号在文内查找。
- 关联列里，ADR-xxx 指同一编号体系里的 ADR；有重号的编号会带上位置，详见末尾「编号与冲突」。

位置简称：
- **adr/**：`docs/adr/`
- **日志**：[`docs/decision.md`](../decision.md)
- **docs/**：`docs/` 根目录
- **DOC/**：`docs/documenting/adr/`
- **KB/**：`docs/knowledge/`
- **OD/**：`docs/open-design-workspace/design/`

## 1. 总纲与概念

| 编号 | 标题 | 状态 | 位置 | 一句话决策 | 关联 |
|---|---|---|---|---|---|
| ADR-039 | [概念模型：本体、能力、技能](ADR-039-concept-model.md) | 已接受（r2） | adr/ | 一个本体加两个概念（能力、技能）；对外只说这些，其余名词归入对内实现；新东西按此归类 | 上位于 ADR-038/040–043；[可进化性评估](../architecture/EVOLVABILITY-ASSESSMENT-2026-11-01.md) |
| ADR-037 | [产品形态与进程模型：agent24d 是本体，外壳都是客户端](../decision.md) | 提议 | 日志 | daemon 是本体、常驻；桌面、TUI、移动都是客户端；取代 ADR-027 中「瘦壳直连 daemon」部分 | ADR-020、ADR-022、ADR-027；[事实清单](../tracking/2026-10-26/SOURCE-ADR-037-facts.md) |
| ADR-026 | [Agent24 采用 Rust Core + Polyglot Worker 架构](../ADR-026-rust-core-polyglot.md) | 已接受 | docs/ | 内核用 Rust，Python 只做 ML Worker；含 11 条设计硬约束（含 fail-closed） | 取代 ADR-023、部分取代 ADR-024；收缩 ADR-016（docs/）；[分层总览](../ARCHITECTURE-LAYERS.md) |
| ADR-003 | [模块按「服务对象」分三层（Base/Community/Personal）](../decision.md) | 已接受 | 日志 | 模块按服务对象分三层 | ADR-002 |
| ADR-002 | [从「裁剪」改为「模块化适配层」](../decision.md) | 已接受 | 日志 | 不裁剪上游，改做模块化适配层 | ADR-001、ADR-003 |
| ADR-001 | [从 xiaoheishu/desktop 借鉴而非从零开发](../decision.md) | 已接受 | 日志 | 以 xiaoheishu 桌面为参考基础，不从零写 | ADR-002 |
| ADR-010 | [先做参考实现 + 渐进提取，不一开始就定接口](../decision.md) | 已接受 | 日志 | 先有参考实现，再从中提取接口 | ADR-011 |
| ADR-011 | [跨切关注点初步规划](../decision.md) | 已接受（M1+ 细化） | 日志 | 列出跨切关注点清单，后续里程碑细化 | ADR-010 |

## 2. 组件与扩展（组件、领域 OS、技能、Open Design 集成）

| 编号 | 标题 | 状态 | 位置 | 一句话决策 | 关联 |
|---|---|---|---|---|---|
| ADR-038 | [组件契约 v1](ADR-038-component-contract-v1.md) | 草案（r3） | adr/ | 组件是能力的安装包；定义清单（provides / requires / assurance）、签名验证与能力 id 规则 | ADR-039、ADR-K1-01、ADR-K1-03；[可进化性评估](../architecture/EVOLVABILITY-ASSESSMENT-2026-11-01.md) |
| ADR-040 | [技能契约 v1](ADR-040-skill-contract-v1.md) | 提议 | adr/ | 技能有版本、过评测才能激活、改动经审批可回滚；不持有权限 | ADR-039、ADR-038、ADR-041；[演进终稿](../research/deepseek-superagent/SUPERAGENT-EVOLUTION-2026-11-01.md) |
| ADR-029 | [内核 ↔ 领域 OS 边界与 DomainModule 挂载缝](../decision.md) | 已接受 | 日志 | 内核与领域 OS（Sin90、Cos72 等）分界，经 DomainModule 挂载缝接入，可装可换 | ADR-030、ADR-031、ADR-039 |
| ADR-031 | [ME-3 进程外模块的协议，按「缝」分别裁决](../decision.md) | 已接受 | 日志 | 进程外模块协议按各条缝逐个裁决，不按「大家都用什么」 | ADR-029、ADR-K1-01 |
| ADR-032 | [AgentEar（语音）与 iDoris（模型网关）接入 Agent24](../decision.md) | 已接受 | 日志 | AgentEar 以 A3 附着式模块接入；iDoris 作模型网关 | [A3 设计](../design/A3-ATTACHED-MODULE.md)、[接入设计](../design/INTEGRATION-AGENTEAR-IDORIS.md) |
| ADR-035 | [iDoris Admin session token 由可信 launcher 通过匿名 FD/HANDLE 一次性交付](../decision.md) | 已接受 | 日志 | token 经继承的匿名句柄交付，不落盘不走环境变量 | ADR-032；[launcher 设计](../agent/IDORIS-LAUNCHER-DESIGN.md) |
| ADR-036 | [硬件探测与整机内存账归 Agent24，模型选型归 iDoris](../decision.md) | 已接受 | 日志 | Agent24 管硬件探测和内存账（`agent24-hw`），iDoris 管选模型 | ADR-032；[HW-TIERS 设计](../design/HW-TIERS-capability-hardware-matrix.md) |
| ADR-033 | [`agent24-decide` 位于 L3，D0-1 只定契约](../decision.md) | 已接受 | 日志 | 决策模块放 L3，首片只定契约、不接任何调用点 | ADR-034 |
| ADR-034 | [`agent24-decide` 决策日志契约由 `agent24d` 实现](../decision.md) | 已接受 | 日志 | 决策日志契约归 daemon 实现，不放进 store crate | ADR-033 |
| ADR-DOC-01 | [Documenting 的定位、进程形态与对外暴露](../documenting/adr/ADR-DOC-01-placement-and-integration.md) | 已接受 | DOC/ | Documenting 作为进程外 OS `documents`，工具由模块声明、内核执行资料政策 | ADR-DOC-02、ADR-K1-01 |
| ADR-DOC-02 | [DocumentService 操作契约](../documenting/adr/ADR-DOC-02-operation-contract.md) | 已接受 | DOC/ | 唯一提交点、change set、类型化错误 | ADR-DOC-01、ADR-DOC-03 |
| ADR-DOC-03 | [DOCX 编辑引擎与转换 worker](../documenting/adr/ADR-DOC-03-docx-engine-and-conversion.md) | 已接受 | DOC/ | 只用 SuperDoc headless SDK，引擎按需下载 | ADR-DOC-01、ADR-DOC-02 |
| ADR-KB-01 | [知识能力进入 Agent24 唯一主对话](../knowledge/ADR-KB-01-SINGLE-CONVERSATION.md) | 未标（原文：方向已确认，工程契约待评审） | KB/ | 迁移入口与编排权，复用 WeKnora 流程；只有一个主对话 | [知识契约](../knowledge/CONTRACT-OPS-v1.md) |
| ADR-OD-001 | [Agent24 × Open Design 集成边界](../open-design-workspace/design/ADR-001-INTEGRATION-BOUNDARIES.md) | 已接受（P0 冻结） | OD/ | Agent24 是唯一 AI 控制面，Open Design 是创作工作区且不取得工具权限 | ADR-OD-002/003/004/005 |
| ADR-OD-002 | [`a24.workspace.v1`](../open-design-workspace/design/ADR-002-WORKSPACE-CONTRACT.md) | 已接受（P0 冻结） | OD/ | workspace 注册表是身份、根目录和 run 绑定的唯一来源 | ADR-OD-001、ADR-OD-006 |
| ADR-OD-003 | [`agent24.open-design.v1` ACP bridge](../open-design-workspace/design/ADR-003-ACP-BRIDGE.md) | 已接受（P0 冻结） | OD/ | Open Design 经 stdio ACP 调 bundled `agent24 acp` | ADR-OD-002 |
| ADR-OD-004 | [Electron Creative host contract](../open-design-workspace/design/ADR-004-DESKTOP-HOST.md) | 已接受（P0 冻结） | OD/ | Electron main 是桌面宿主与 sidecar 生命周期的唯一 owner | ADR-OD-002、ADR-OD-003 |
| ADR-OD-005 | [Creative capability 与 authority 隔离](../open-design-workspace/design/ADR-005-CAPABILITY-AUTHORITY.md) | 已接受 | OD/ | 应用层 capability 隔离单 bearer 的越权，并明确 TCB | ADR-OD-001、ADR-K1-03 |
| ADR-OD-006 | [Legacy Recovery Holds](../open-design-workspace/design/ADR-006-LEGACY-RECOVERY-HOLDS.md) | 已接受 | OD/ | 升级前的非终态 Run 迁入 recovery hold，不再各占 active lease | ADR-OD-002 |
| ADR-005 | [iDoris-SDK 不收纳 publishers，每个 publisher 独立 npm 包](../decision.md) | 已接受 | 日志 | publisher 各自独立发包 | ADR-006、ADR-012 |
| ADR-004 | [cos72 包含 myshop / mytask / myvote 三个核心子模块](../decision.md) | 已接受 | 日志 | cos72 由三个子模块组成 | 注意与 ADR-OD-004 不同号体系，见末尾 |
| ADR-009 | [SkillBank 与 Evolver 是互补的两个独立包，不合并](../decision.md) | 已接受 | 日志 | 两个独立包，各管各的 | ADR-014 |
| ADR-008 | [模型包不存权重，只放 metadata](../decision.md) | 已接受 | 日志 | 模型包只带元数据 | — |

## 3. 内核与权限

| 编号 | 标题 | 状态 | 位置 | 一句话决策 | 关联 |
|---|---|---|---|---|---|
| ADR-K1-01 | [通用模块工具契约](ADR-K1-01-module-tool-contract.md) | 已接受 | adr/ | 模块声明工具，与宿主授权取交集；稳定身份、有界调用、结果未知不自动重试 | ADR-K1-02/03/04、ADR-038、ADR-042；[PLAN-KERNEL-K1](../agent/PLAN-KERNEL-K1.md) |
| ADR-K1-02 | [会话数据流与安全模式](ADR-K1-02-session-dataflow-security-modes.md) | 已接受 | adr/ | 宿主建立会话数据的来源与处理模式并跨边界传播；LocalOnly 与获准云处理各有出口门 | ADR-K1-01、ADR-K1-03、ADR-041 |
| ADR-K1-03 | [启用授权、信任与撤销](ADR-K1-03-enable-authorization-trust-revocation.md) | 已接受 | adr/ | 启用模块时一次性确认逐工具权限，之后在范围内调用；可撤销；安装与签名不等于用户同意 | ADR-K1-01、ADR-038、ADR-OD-005 |
| ADR-K1-04 | [审计与共同授权入口](ADR-K1-04-audit-and-shared-authorization.md) | 已接受 | adr/ | 内核审计只记元数据；UI 与 Agent 共用同一身份、资源与授权权威 | ADR-K1-01、ADR-K1-03 |
| ADR-041 | [入站事件、准入与工单](ADR-041-inbound-events-admission-and-work-items.md) | 提议 | adr/ | 五条入站路径统一准入，引入工单与授权依据，由本体按规则决定 | ADR-039、ADR-040、ADR-042、ADR-K1-02 |
| ADR-042 | [待执行动作与对账](ADR-042-durable-actions-and-reconciliation.md) | 提议 | adr/ | 先存后做，统一待执行动作与对账（即 K1-01 §2.6 预留的另开 ADR，范围到 D3） | ADR-K1-01、ADR-041、ADR-026 |
| ADR-043 | [后台作业、并发与监督回路](ADR-043-background-jobs-concurrency-and-supervision.md) | 提议 | adr/ | 后台作业句柄、并发槽，以及按实质进展监督的回路；系统不得静默改技能 | ADR-K1-01、ADR-040、ADR-041 |
| ADR-016 | [模块安全与权限模型](../decision.md) | 已接受（M1 落地） | 日志 | 模块安全与权限分阶段落地 | 与 docs/ 下同号 ADR-016 重号，见末尾 |
| ADR-017 | [数据隐私与轨迹共享](../decision.md) | 已接受 | 日志 | 数据隐私与轨迹共享的边界 | ADR-K1-02 |
| ADR-019 | [LLM Gateway 模式](../decision.md) | 已接受 | 日志 | 能力模块不直连 LLM API，经注入的网关 | ADR-032 |

## 4. 记忆

| 编号 | 标题 | 状态 | 位置 | 一句话决策 | 关联 |
|---|---|---|---|---|---|
| ADR-028 | [记忆架构：可进化 / 可替换 / 可组合的分层模型](../decision.md) | 草案（原文：暂定 / 需 spike） | 日志 | 记忆分层、可替换；不冻结层边界，实现分期见 SPEC-MEMORY | ADR-030；[SPEC-MEMORY](../specs/SPEC-MEMORY.md)、[SPEC-MD-ME](../specs/SPEC-MD-ME.md) |
| ADR-030 | [记忆的所有权维度 = (组织, 空间)](../decision.md) | 已接受（不变量） | 日志 | 记忆按（组织，空间）归属，组织是一等实体 | ADR-028、ADR-029 |
| ADR-MEMORY-P1 | [个人记忆生命周期与清除权](ADR-MEMORY-P1-LIFECYCLE-AND-PURGE.md) | 提议（原文：待外审接受） | adr/ | 正常生命周期 no-loss；只有用户明确确认的 purge 才能破坏性删除 | [MEMORY 法律](../laws/MEMORY.md)、[PLAN-MEMORY-P1](../agent/PLAN-MEMORY-P1.md)、ADR-MEMORY-RECALL-MESSAGE-ORIGINS |
| ADR-MEMORY-RECALL-MESSAGE-ORIGINS | [记忆召回消息来源契约](ADR-MEMORY-RECALL-MESSAGE-ORIGINS.md) | 提议（原文：待外审接受） | adr/ | 召回消息的来源用实例身份证明，不靠文本 marker 判断 | ADR-MEMORY-P1；[R05-1 契约](../agent/R05-1-EXACT-CONTRACT.md) |

记忆两份 ADR 没有数字编号，编号一栏直接用文件名；召回来源那份的标题第一行写的是「R05」。

## 5. 数据与迁移

| 编号 | 标题 | 状态 | 位置 | 一句话决策 | 关联 |
|---|---|---|---|---|---|
| ADR-007 | [混合 Monorepo 策略](../decision.md) | 已接受 | 日志 | pnpm workspace，按「未来可拆」边界组织目录 | ADR-013 |
| ADR-012 | [iDoris-SDK 不合并进 @auraaihq/sdk](../decision.md) | 已接受 | 日志 | 两个 SDK 保持分开 | ADR-013 |
| ADR-013 | [iDoris-SDK 合并进 auraai-packages monorepo](../decision.md) | 已接受 | 日志 | 修正 ADR-012 的回答范围，M2 合并进 monorepo | ADR-012 |
| ADR-014 | [Agent24 在 M3 后被 @auraaihq/skills-* + skill-bank + evolver 替代](../decision.md) | 已接受 | 日志 | M3 后用拆出的包替代原框架 | ADR-015 |
| ADR-015 | [M3 后 Agent24 改名](../decision.md) | 已接受 | 日志 | M3 末改名，依赖 ADR-014 | ADR-014 |
| ADR-006 | [npm scope 用 `@auraaihq`](../decision.md) | 已接受 | 日志 | 包统一用 `@auraaihq` scope | ADR-016（docs/） |
| ADR-016 | [Agent24 迁移至消费 @auraaihq/* 包](../ADR-016-consume-auraai-packages.md) | 未标（原文：进行中） | docs/ | 把内部自研实现迁到消费 @auraaihq/* 包；范围被 ADR-026 收缩 | ADR-026；与日志里的 ADR-016 重号，见末尾 |

## 6. 通信与外壳

| 编号 | 标题 | 状态 | 位置 | 一句话决策 | 关联 |
|---|---|---|---|---|---|
| ADR-027 | [移动端 shell-agnostic 双壳（Tauri + Expo/React Native）](../decision.md) | 被替代（部分，被 ADR-037 取代「直连 daemon」及遗留项） | 日志 | 移动端不单选 Tauri，改 shell-agnostic 双壳；技术选型留待遥控设计 | ADR-018、ADR-037 |
| ADR-018 | [移动端技术路径选 Tauri 2.0](../decision.md) | 被替代（被 ADR-027） | 日志 | 原选 Tauri 2.0；其 M0–M4 约束仍有效 | ADR-027 |
| ADR-020 | [后端 Daemon 技术选型（TypeScript + Node http）](../decision.md) | 被替代（ADR-020 到 023 到 026） | 日志 | 原选 Node http daemon；现仅作契约测试参考实现 | ADR-023、ADR-026、ADR-037 |
| ADR-023 | [后端语言从 Node.js 切换到 Python FastAPI](../decision.md) | 被替代（被 ADR-026） | 日志 | 原定 M3 切 Python；已改 Rust | ADR-026 |
| ADR-021 | [一站式安装：Zero-CLI Onboarding](../decision.md) | 已接受 | 日志 | 用户无需命令行即可完成安装与引导 | ADR-022 |

通信（Hyphae / Nostr）目前没有独立 ADR，决策在设计文档里：见 [架构总入口 §通信](../architecture/README.md)。

## 7. 运行时与模型

| 编号 | 标题 | 状态 | 位置 | 一句话决策 | 关联 |
|---|---|---|---|---|---|
| ADR-022 | [LLM 运行时默认 MLX，UI 可切换](../decision.md) | 已接受（已收窄） | 日志 | 默认 MLX 只在 Apple Silicon 成立；实际是 oMLX 的 HTTP 服务；其他平台由 iDoris 按档位选 | ADR-036、ADR-037 |
| ADR-024 | [工作流引擎：asyncio.Queue + Step 模式](../decision.md) | 已接受（ADR-026 标其部分被取代） | 日志 | 原为 Python 工作流引擎 | ADR-026 |
| ADR-025 | [内存管理：串行 LLM 推理 + 模型热切换限制](../decision.md) | 已接受 | 日志 | 串行推理并限制热切换 | ADR-036 |

## 8. 演进（Super Agent）

ADR-040 至 ADR-043 来自 [Super Agent 演进终稿](../research/deepseek-superagent/SUPERAGENT-EVOLUTION-2026-11-01.md)，ADR-038 来自[可进化性评估终稿](../architecture/EVOLVABILITY-ASSESSMENT-2026-11-01.md)，都在 PR #1530 上；表见上面各节，这里只列清单：

| 编号 | 标题 | 状态 | 所在节 |
|---|---|---|---|
| ADR-038 | [组件契约 v1](ADR-038-component-contract-v1.md) | 草案（r3） | 组件与扩展 |
| ADR-040 | [技能契约 v1](ADR-040-skill-contract-v1.md) | 提议 | 组件与扩展 |
| ADR-041 | [入站事件、准入与工单](ADR-041-inbound-events-admission-and-work-items.md) | 提议 | 内核与权限 |
| ADR-042 | [待执行动作与对账](ADR-042-durable-actions-and-reconciliation.md) | 提议 | 内核与权限 |
| ADR-043 | [后台作业、并发与监督回路](ADR-043-background-jobs-concurrency-and-supervision.md) | 提议 | 内核与权限 |

## 初稿（已被取代，不计入总数）

Fable 初稿的五份 ADR 草案，原样保留、不再更新，最终版见上面。目录说明：[adr-drafts/README.md](../research/deepseek-superagent/adr-drafts/README.md)。

| 初稿编号 | 标题 | 去向 |
|---|---|---|
| ADR-040 | [技能契约 v1](../research/deepseek-superagent/adr-drafts/ADR-040-skill-contract-v1.md) | 即 ADR-040 |
| ADR-041 | [入站事件与准入](../research/deepseek-superagent/adr-drafts/ADR-041-inbound-events-and-admission.md) | 与初稿 ADR-042 合并为 ADR-041 |
| ADR-042 | [工单与授权依据](../research/deepseek-superagent/adr-drafts/ADR-042-work-items-and-mandates.md) | 并入 ADR-041 |
| ADR-043 | [待执行动作与对账](../research/deepseek-superagent/adr-drafts/ADR-043-durable-actions-and-reconciliation.md) | 即 ADR-042 |
| ADR-044 | [后台作业与监督回路](../research/deepseek-superagent/adr-drafts/ADR-044-background-jobs-and-supervision.md) | 即 ADR-043 |

## 编号与冲突

只列出，不改动。

**重号（同一编号对应不同决策）**

| 编号 | 位置一 | 位置二 | 说明 |
|---|---|---|---|
| ADR-016 | [日志](../decision.md)：模块安全与权限模型 | [docs/ADR-016-consume-auraai-packages.md](../ADR-016-consume-auraai-packages.md)：迁移至消费 @auraaihq/* 包 | 两份内容无关。ADR-026 的「修订」字段指的是后者 |
| ADR-001 至 ADR-006 | [日志](../decision.md) 的 ADR-001 至 ADR-006 | `docs/open-design-workspace/design/ADR-001` 至 `ADR-006` | OD 目录自成一套编号，本索引用 ADR-OD-00x 区分；文件名和标题里仍是 ADR-00x。其中 ADR-004 两边内容无关（cos72 子模块 vs 桌面宿主） |
| ADR-040 至 ADR-043 | `docs/adr/` 最终版 | `docs/research/deepseek-superagent/adr-drafts/` 初稿 | 初稿的 041 至 044 与最终版的 041 至 043 编号错位（初稿 042 并入 041，之后依次前移一位），对照见上面「初稿」表 |

**独立编号体系**（不算冲突，但容易混淆）：`ADR-K1-xx`（Kernel K1）、`ADR-DOC-xx`（Documenting）、`ADR-KB-01`（知识）、`ADR-MEMORY-*`（记忆，无数字编号）。

**缺号**

- ADR-026 不在 [日志](../decision.md) 里（日志从 ADR-025 直接跳到 ADR-027），它是单独的 [docs/ADR-026](../ADR-026-rust-core-polyglot.md)。
- 日志里 ADR-001 至 ADR-037 除 ADR-026 外齐全；ADR-038 起在 `docs/adr/`。
- ADR-044 只在初稿里出现，没有最终版。
- ADR-K1-04 在 ADR-K1-02 写作时标为「计划中」，现已存在。
- 记忆两份 ADR 没有数字编号，文件名即标识；R05 的标题第一行是「R05」。

**其他需要知道的**

- 日志头部说「按时间倒序追加」，实际是按编号正序。
- 根 [README](../../README.md) 的文档节写「ADR-001 ~ ADR-030」，日志实际到 ADR-037。
- 其他文档里的 `ADR-0007`、`ADR-0008` 是 AgentEar 仓库自己的 ADR，不在本仓库。
- `docs/agent/R05-2-D7-KIND-DECISION.md`、`R05-3-D7F-KIND-DECISION.md`、`docs/research/DECISION-*.md`、`improvement/decisions/` 名字里有 decision，但不是 ADR，未收录。
