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
| ADR-039 | [概念模型：本体、能力、技能](ADR-039-concept-model.md) | 已接受（原文：已接受（r2，Fable 评审后修订）） | adr/ | 一个本体加两个概念（能力、技能）；对外只说这些，其余名词归入对内实现；新东西按此归类 | 上位于 ADR-038/040–043；[可进化性评估](../architecture/EVOLVABILITY-ASSESSMENT-2026-11-01.md)；ADR-K1-01、ADR-K1-03、ADR-004（未标明体系） |
| ADR-037 | [产品形态与进程模型：agent24d 是本体，外壳都是客户端](../decision.md) | 提议（原文：提议；待评审，批准前不派生产实现） | 日志 | 守护进程常驻为本体，外壳只做客户端；手机遥控不直连，以此取代旧的直连方案 | ADR-020、ADR-022、ADR-027；[事实清单](../tracking/2026-10-26/SOURCE-ADR-037-facts.md)；ADR-023、ADR-026、ADR-029、ADR-031、ADR-032、ADR-035、ADR-036、ADR-OD-004、ADR-OD-005 |
| ADR-026 | [Agent24 采用 Rust Core + Polyglot Worker 架构](../ADR-026-rust-core-polyglot.md) | 已接受（原文：采纳） | docs/ | 内核用 Rust，Python 只做 ML Worker；含 §6.5 的 11 条设计硬约束（第 2 条：审批请求-响应、fail-closed） | 取代 ADR-023、部分取代 ADR-024；收缩 ADR-016（docs/）；[分层总览](../ARCHITECTURE-LAYERS.md)；ADR-017、ADR-019、ADR-022 |
| ADR-003 | [模块按「服务对象」分三层（Base/Community/Personal）](../decision.md) | 已接受（原文：采纳） | 日志 | 主分类按服务对象，用前缀区分基础、社区和个人模块 | ADR-002 |
| ADR-002 | [从「裁剪」改为「模块化适配层」](../decision.md) | 已接受（原文：采纳（已修正初版方案）） | 日志 | 内核加三层模块，能力都做成可插拔，不裁掉原有能力 | ADR-001、ADR-003 |
| ADR-001 | [从 xiaoheishu/desktop 借鉴而非从零开发](../decision.md) | 已接受（原文：采纳） | 日志 | 把 xiaoheishu 当子模块引入，抽出通用部分，场景功能以后独立发包 | ADR-002 |
| ADR-010 | [先做参考实现 + 渐进提取，不一开始就定接口](../decision.md) | 已接受（原文：采纳） | 日志 | M0 不写接口，等做出参考实现后再提取规格 | ADR-011 |
| ADR-011 | [跨切关注点初步规划](../decision.md) | 已接受（原文：采纳（M1+ 细化）） | 日志 | 发现、信任、权限等七项先定起步策略，细项以后再补 | ADR-010 |

## 2. 组件与扩展（组件、领域 OS、技能、Open Design 集成）

| 编号 | 标题 | 状态 | 位置 | 一句话决策 | 关联 |
|---|---|---|---|---|---|
| ADR-038 | [组件契约 v1](ADR-038-component-contract-v1.md) | 草案（原文：草案 r3） | adr/ | 组件是能力的安装包；定义清单（provides / requires / assurance）、签名验证与能力 id 规则 | ADR-039、ADR-K1-01、ADR-K1-03；[可进化性评估](../architecture/EVOLVABILITY-ASSESSMENT-2026-11-01.md)；ADR-040 |
| ADR-040 | [技能契约 v1](ADR-040-skill-contract-v1.md) | 提议（原文：提议） | adr/ | 技能有版本、过评测才能激活、改动经审批可回滚；不持有权限 | ADR-039、ADR-038、ADR-041；[演进终稿](../research/deepseek-superagent/SUPERAGENT-EVOLUTION-2026-11-01.md)；ADR-037、ADR-043、ADR-K1-03、ADR-K1-04、ADR-MEMORY-P1 |
| ADR-029 | [内核 ↔ 领域 OS 边界与 DomainModule 挂载缝](../decision.md) | 已接受（原文：采纳（边界原则）） | 日志 | 领域系统可换且不改内核；原进程内编译方案已推翻，改走进程外 | ADR-030、ADR-031、ADR-039；ADR-026、ADR-028 |
| ADR-031 | [ME-3 进程外模块的协议，按「缝」分别裁决](../decision.md) | 已接受（原文：采纳（裁决 FU-14，解除 ME-3 的开工前置）） | 日志 | 模块挂载缝不用会话协议，只定义出站回调；外壳那条缝本篇不裁 | ADR-029、ADR-K1-01；ADR-030 |
| ADR-032 | [AgentEar（语音）与 iDoris（模型网关）接入 Agent24](../decision.md) | 已接受（原文：已接受） | 日志 | 语音端自启后附着接入；模型网关作兼容接口，不做成进程外模块 | [A3 设计](../design/A3-ATTACHED-MODULE.md)、[接入设计](../design/INTEGRATION-AGENTEAR-IDORIS.md) |
| ADR-035 | [iDoris Admin session token 由可信 launcher 通过匿名 FD/HANDLE 一次性交付](../decision.md) | 已接受（原文：采纳（jason 明确拍板）） | 日志 | token 经继承的匿名句柄交付，不落盘不走环境变量 | ADR-032；[launcher 设计](../agent/IDORIS-LAUNCHER-DESIGN.md) |
| ADR-036 | [硬件探测与整机内存账归 Agent24，模型选型归 iDoris](../decision.md) | 已接受（原文：采纳（jason 2026-10-14 拍板）） | 日志 | Agent24 管硬件探测和内存账（`agent24-hw`），iDoris 管选模型 | ADR-032；[HW-TIERS 设计](../design/HW-TIERS-capability-hardware-matrix.md)；ADR-033 |
| ADR-033 | [`agent24-decide` 位于 L3，D0-1 只定契约](../decision.md) | 已接受（原文：采纳（D0-1）） | 日志 | 决策服务放在第三层，这一片不接到任何现有调用，也不能放宽权限 | ADR-034 |
| ADR-034 | [`agent24-decide` 决策日志契约由 `agent24d` 实现](../decision.md) | 已接受（原文：采纳（D0-2）） | 日志 | 日志落库放在组合根进程，存储层与决策层互不依赖 | ADR-033 |
| ADR-DOC-01 | [Documenting 的定位、进程形态与对外暴露](../documenting/adr/ADR-DOC-01-placement-and-integration.md) | 已接受（原文：Accepted） | DOC/ | Documenting 作为进程外 OS `documents`，工具由模块声明、内核执行资料政策 | ADR-DOC-02、ADR-K1-01；ADR-029、ADR-031、ADR-K1-02 |
| ADR-DOC-02 | [DocumentService 操作契约](../documenting/adr/ADR-DOC-02-operation-contract.md) | 已接受（原文：Accepted） | DOC/ | 只有提交才产生版本，变更集提交后冻结，错误按错误码分支 | ADR-DOC-01、ADR-DOC-03；ADR-K1-01 |
| ADR-DOC-03 | [DOCX 编辑引擎与转换 worker](../documenting/adr/ADR-DOC-03-docx-engine-and-conversion.md) | 已接受（原文：Accepted） | DOC/ | 文档引擎只用 SuperDoc 无头组件并按需下载；转换走 LibreOffice，同样按需 | ADR-DOC-01、ADR-DOC-02 |
| ADR-KB-01 | [知识能力进入 Agent24 唯一主对话](../knowledge/ADR-KB-01-SINGLE-CONVERSATION.md) | 未标（原文：用户确认产品方向；工程契约待评审） | KB/ | 迁移入口与编排权，复用 WeKnora 流程；只有一个主对话 | [知识契约](../knowledge/CONTRACT-OPS-v1.md) |
| ADR-OD-001 | [Agent24 × Open Design 集成边界](../open-design-workspace/design/ADR-001-INTEGRATION-BOUNDARIES.md) | 已接受（原文：Accepted / P0 frozen） | OD/ | Agent24 是唯一 AI 控制面，Open Design 是创作工作区且不取得工具权限 | ADR-OD-002/003/004/005 |
| ADR-OD-002 | [`a24.workspace.v1`](../open-design-workspace/design/ADR-002-WORKSPACE-CONTRACT.md) | 已接受（原文：Accepted / P0 frozen） | OD/ | workspace 注册表是身份、根目录和 run 绑定的唯一来源 | ADR-OD-001、ADR-OD-006；ADR-OD-005 |
| ADR-OD-003 | [`agent24.open-design.v1` ACP bridge](../open-design-workspace/design/ADR-003-ACP-BRIDGE.md) | 已接受（原文：Accepted / P0 frozen） | OD/ | Open Design 经 stdio ACP 调 bundled `agent24 acp` | ADR-OD-002 |
| ADR-OD-004 | [Electron Creative host contract](../open-design-workspace/design/ADR-004-DESKTOP-HOST.md) | 已接受（原文：Accepted / P0 frozen） | OD/ | Electron main 是桌面宿主与 sidecar 生命周期的唯一 owner | ADR-OD-002、ADR-OD-003；ADR-OD-005 |
| ADR-OD-005 | [Creative capability 与 authority 隔离](../open-design-workspace/design/ADR-005-CAPABILITY-AUTHORITY.md) | 已接受（原文：Accepted / 2026-09-19） | OD/ | 创作边车不拿全权凭据，只拿短时受限令牌，并列入本地可信基 | ADR-OD-001、ADR-K1-03；ADR-OD-004 |
| ADR-OD-006 | [Legacy Recovery Holds](../open-design-workspace/design/ADR-006-LEGACY-RECOVERY-HOLDS.md) | 已接受（原文：Accepted / frozen for incremental G4 implementation） | OD/ | 升级前的非终态 Run 迁入 recovery hold，不再各占 active lease | ADR-OD-002 |
| ADR-005 | [iDoris-SDK 不收纳 publishers，每个 publisher 独立 npm 包](../decision.md) | 已接受（原文：采纳） | 日志 | 微信桥不收发布器，各平台发布器独立成包 | ADR-006、ADR-012 |
| ADR-004 | [cos72 包含 myshop / mytask / myvote 三个核心子模块](../decision.md) | 已接受（原文：采纳（用户明确指定）） | 日志 | 社区层含 cos72 伞包，依赖 myshop、mytask、myvote，另加通信与共享记忆 | 注意与 ADR-OD-004 不同号体系，见末尾 |
| ADR-009 | [SkillBank 与 Evolver 是互补的两个独立包，不合并](../decision.md) | 已接受（原文：采纳） | 日志 | 技能库与进化器分成两个包，进化器写入，检索只读技能库 | ADR-014 |
| ADR-008 | [模型包不存权重，只放 metadata](../decision.md) | 已接受（原文：采纳） | 日志 | 模型包不放权重，只放地址与校验信息，权重运行时再下载 | — |

## 3. 内核与权限

| 编号 | 标题 | 状态 | 位置 | 一句话决策 | 关联 |
|---|---|---|---|---|---|
| ADR-K1-01 | [通用模块工具契约](ADR-K1-01-module-tool-contract.md) | 已接受（原文：Accepted） | adr/ | 模块声明工具，与宿主授权取交集；稳定身份、有界调用、结果未知不自动重试 | ADR-K1-02/03/04、ADR-038、ADR-042；[PLAN-KERNEL-K1](../agent/PLAN-KERNEL-K1.md)；ADR-DOC-01、ADR-DOC-02 |
| ADR-K1-02 | [会话数据流与安全模式](ADR-K1-02-session-dataflow-security-modes.md) | 已接受（原文：Accepted） | adr/ | 宿主建立会话数据的来源与处理模式并跨边界传播；LocalOnly 与获准云处理各有出口门 | ADR-K1-01、ADR-K1-03、ADR-041；ADR-K1-04 |
| ADR-K1-03 | [启用授权、信任与撤销](ADR-K1-03-enable-authorization-trust-revocation.md) | 已接受（原文：Accepted） | adr/ | 启用模块时一次性确认逐工具权限，之后在范围内调用；可撤销；安装与签名不等于用户同意 | ADR-K1-01、ADR-038、ADR-OD-005；ADR-K1-02、ADR-K1-04 |
| ADR-K1-04 | [审计与共同授权入口](ADR-K1-04-audit-and-shared-authorization.md) | 已接受（原文：Accepted） | adr/ | 内核审计只记元数据；UI 与 Agent 共用同一身份、资源与授权权威 | ADR-K1-01、ADR-K1-03 |
| ADR-041 | [入站事件、准入与工单](ADR-041-inbound-events-admission-and-work-items.md) | 提议（原文：提议） | adr/ | 五条入站路径统一准入，引入工单与授权依据，由本体按规则决定 | ADR-039、ADR-040、ADR-042、ADR-K1-02；ADR-037、ADR-038、ADR-K1-03、ADR-MEMORY-P1 |
| ADR-042 | [待执行动作与对账](ADR-042-durable-actions-and-reconciliation.md) | 提议（原文：提议） | adr/ | 先存后做，统一待执行动作与对账（即 K1-01 §2.6 预留的另开 ADR，范围到 D3） | ADR-K1-01、ADR-041、ADR-026；ADR-038、ADR-039、ADR-043 |
| ADR-043 | [后台作业、并发与监督回路](ADR-043-background-jobs-concurrency-and-supervision.md) | 提议（原文：提议） | adr/ | 后台作业句柄、并发槽，以及按实质进展监督的回路；系统不得静默改技能 | ADR-K1-01、ADR-040、ADR-041；ADR-026、ADR-037、ADR-039、ADR-042、ADR-K1-02、初稿 ADR-044 |
| ADR-016 | [模块安全与权限模型](../decision.md) | 已接受（原文：采纳（M1 设计模块加载器时落地）） | 日志 | 起步用线程沙箱和清单权限，签名与虚拟机沙箱往后放 | 与 docs/ 下同号 ADR-016 重号，见末尾；ADR-011 |
| ADR-017 | [数据隐私与轨迹共享](../decision.md) | 已接受（原文：采纳） | 日志 | 轨迹默认只留本地；共享技能必须显式开启，且不上传原始轨迹 | ADR-K1-02 |
| ADR-019 | [LLM Gateway 模式](../decision.md) | 已接受（原文：采纳） | 日志 | 能力模块不直连 LLM API，经注入的网关 | ADR-032；ADR-017 |

## 4. 记忆

| 编号 | 标题 | 状态 | 位置 | 一句话决策 | 关联 |
|---|---|---|---|---|---|
| ADR-028 | [记忆架构：可进化 / 可替换 / 可组合的分层模型](../decision.md) | 未标（原文：暂定 / 需 spike） | 日志 | 不冻结分层；改为三份权威加可重建投影，先做评测与恢复试验 | ADR-030；[SPEC-MEMORY](../specs/SPEC-MEMORY.md)、[SPEC-MD-ME](../specs/SPEC-MD-ME.md)；ADR-016、ADR-029 |
| ADR-030 | [记忆的所有权维度 = (组织, 空间)](../decision.md) | 已接受（原文：采纳（不变量）） | 日志 | 记忆行的键只含组织与空间的不可变标识，组织单独建档 | ADR-028、ADR-029；ADR-016 |
| ADR-MEMORY-P1 | [个人记忆生命周期与清除权](ADR-MEMORY-P1-LIFECYCLE-AND-PURGE.md) | 提议（原文：PROPOSED / 待外审接受） | adr/ | 正常生命周期 no-loss；只有用户明确确认的 purge 才能破坏性删除 | [MEMORY 法律](../laws/MEMORY.md)、[PLAN-MEMORY-P1](../agent/PLAN-MEMORY-P1.md)、ADR-MEMORY-RECALL-MESSAGE-ORIGINS |
| ADR-MEMORY-RECALL-MESSAGE-ORIGINS | [记忆召回消息来源契约](ADR-MEMORY-RECALL-MESSAGE-ORIGINS.md) | 提议（原文：PROPOSED / 待外审接受） | adr/ | 召回消息的来源用实例身份证明，不靠文本 marker 判断 | ADR-MEMORY-P1；[R05-1 契约](../agent/R05-1-EXACT-CONTRACT.md) |

记忆两份 ADR 没有数字编号，编号一栏直接用文件名；召回来源那份的标题第一行写的是「R05」。

## 5. 数据与迁移

| 编号 | 标题 | 状态 | 位置 | 一句话决策 | 关联 |
|---|---|---|---|---|---|
| ADR-007 | [混合 Monorepo 策略](../decision.md) | 已接受（原文：采纳） | 日志 | 单仓按可拆边界分目录，用 pnpm workspace，半年内不拆 | ADR-013 |
| ADR-012 | [iDoris-SDK 不合并进 @auraaihq/sdk](../decision.md) | 已接受（原文：采纳） | 日志 | 微信桥不并进 @auraaihq/sdk，改由适配模块实现框架接口 | ADR-013 |
| ADR-013 | [iDoris-SDK 合并进 auraai-packages monorepo](../decision.md) | 已接受（原文：采纳（修正 ADR-012 的回答范围）） | 日志 | 修正前条范围：M2 迁进单体仓库并改名，不并进单个 SDK | ADR-012；ADR-007 |
| ADR-014 | [Agent24 在 M3 后被 @auraaihq/skills-* + skill-bank + evolver 替代](../decision.md) | 已接受（原文：采纳（M3 执行）） | 日志 | M3 前保持现状，届时把技能迁成包；迁完旧仓库归档 | ADR-015 |
| ADR-015 | [M3 后 Agent24 改名](../decision.md) | 已接受（原文：采纳（M3 末执行，依赖 ADR-014 完成）） | 日志 | 前条完成后立即改仓库名，并避免架构只绑桌面 | ADR-014 |
| ADR-006 | [npm scope 用 `@auraaihq`](../decision.md) | 已接受（原文：采纳） | 日志 | 包统一用 `@auraaihq` scope | ADR-016（docs/） |
| ADR-016 | [Agent24 迁移至消费 @auraaihq/* 包](../ADR-016-consume-auraai-packages.md) | 未标（原文：进行中（migration 分支活跃）） | docs/ | 改为消费 @auraaihq/* 包，自己只留 Electron 外壳 | ADR-026；与日志里的 ADR-016 重号，见末尾 |

## 6. 通信与外壳

| 编号 | 标题 | 状态 | 位置 | 一句话决策 | 关联 |
|---|---|---|---|---|---|
| ADR-027 | [移动端 shell-agnostic 双壳（Tauri + Expo/React Native）](../decision.md) | 已接受（部分被 ADR-037 取代；原文：部分取代） | 日志 | 取代 ADR-018，不钦定单一移动框架；直连守护进程部分已被 ADR-037 决策 6 取代，双壳选型待遥控设计时再定 | ADR-018、ADR-037；ADR-026 |
| ADR-018 | [移动端技术路径选 Tauri 2.0](../decision.md) | 被替代（原文：Superseded by ADR-027） | 日志 | 移动端原定 Tauri，已被 ADR-027 取代；避免桌面专属接口的约束仍有效 | ADR-027；ADR-015 |
| ADR-020 | [后端 Daemon 技术选型（TypeScript + Node http）](../decision.md) | 被替代（原文：已取代（ADR-020 → ADR-023 → ADR-026）） | 日志 | M2 选用 Node 内置 http；本篇已标被取代，只留作契约测试参考 | ADR-023、ADR-026、ADR-037 |
| ADR-023 | [后端语言从 Node.js 切换到 Python FastAPI](../decision.md) | 被替代（原文：Superseded by ADR-026） | 日志 | 原定 M3 改用 Python 接口框架；已被 ADR-026 取代，Python 降为模型工人 | ADR-026；ADR-020、ADR-022 |
| ADR-021 | [一站式安装：Zero-CLI Onboarding](../decision.md) | 已接受（原文：采纳（M2 实现 Onboarding Wizard，M3 完善 Ollama 捆绑）） | 日志 | 安装包不捆绑运行时，用五步向导检测并下载，按内存推荐模型 | ADR-022 |

通信（Hyphae / Nostr）目前没有独立 ADR，决策在设计文档里：见 [架构总入口 §通信](../architecture/README.md)。

## 7. 运行时与模型

| 编号 | 标题 | 状态 | 位置 | 一句话决策 | 关联 |
|---|---|---|---|---|---|
| ADR-022 | [LLM 运行时默认 MLX，UI 可切换](../decision.md) | 已接受（原文：采纳，已收窄） | 日志 | 默认 MLX 只在 Apple Silicon 成立；实际是 oMLX 的 HTTP 服务；其他平台由 iDoris 按档位选 | ADR-036、ADR-037；ADR-019、ADR-023 |
| ADR-024 | [工作流引擎：asyncio.Queue + Step 模式](../decision.md) | 已接受（原文：采纳） | 日志 | 工作流采用队列加步骤，不用外部调度服务，任务记在本地库 | ADR-026 |
| ADR-025 | [内存管理：串行 LLM 推理 + 模型热切换限制](../decision.md) | 已接受（原文：采纳） | 日志 | 大模型推理同时只跑一个，换模型先卸载，内存过低就暂停新任务 | ADR-036 |

## 8. 演进（Super Agent）

ADR-040 至 ADR-043 来自 [Super Agent 演进终稿](../research/deepseek-superagent/SUPERAGENT-EVOLUTION-2026-11-01.md)，ADR-038 来自[可进化性评估终稿](../architecture/EVOLVABILITY-ASSESSMENT-2026-11-01.md)，随 PR #1530 合入 main；表见上面各节，这里只列清单：

| 编号 | 标题 | 状态 | 所在节 |
|---|---|---|---|
| ADR-038 | [组件契约 v1](ADR-038-component-contract-v1.md) | 草案（原文：草案 r3） | 组件与扩展 |
| ADR-040 | [技能契约 v1](ADR-040-skill-contract-v1.md) | 提议（原文：提议） | 组件与扩展 |
| ADR-041 | [入站事件、准入与工单](ADR-041-inbound-events-admission-and-work-items.md) | 提议（原文：提议） | 内核与权限 |
| ADR-042 | [待执行动作与对账](ADR-042-durable-actions-and-reconciliation.md) | 提议（原文：提议） | 内核与权限 |
| ADR-043 | [后台作业、并发与监督回路](ADR-043-background-jobs-concurrency-and-supervision.md) | 提议（原文：提议） | 内核与权限 |

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

## 核对记录

2026-11-01 由 Grok 逐篇核对了「一句话决策」和「状态」两列，共改了 60 行。演进清单里重复出现的 5 行状态与上文对齐，不另计入。

核对时拿不准的条目：

- ADR-024：本篇状态行仍是「✅ 采纳」。ADR-026 另写「部分 Superseded」，但本篇没有改自己的状态。状态列按本篇填「已接受」，没有写成被替代。
- ADR-027：状态原文是「部分取代」。它取代 ADR-018，同时「移动瘦壳经 HTTP/WS 直连 daemon」及遗留项被 ADR-037 决策 6 取代。状态列写「已接受（部分被 ADR-037 取代）」，与架构总入口一致；双壳、不钦定单一框架，仍是本篇写下的决定。
- ADR-028：「暂定 / 需 spike」不在已接受、提议、草案、被替代、废弃之内，按未标处理。原先索引写成草案，是推断。
- docs/ 的 ADR-016：状态原文是「进行中（migration 分支活跃）」。进行中不在上述五档里，按未标处理。
- ADR-015：现行正文里改名前后的仓库名都写成 Agent24。「去掉 Desktop」只出现在用户引语里。一句话写成改仓库名，并避免架构只绑桌面。
- ADR-039：正文写「ADR-004」的目标形态是 scratch workspace。这与日志 ADR-004（cos72）和 ADR-OD-004（桌面宿主）都对不上。关联照原文记下，没有断定是哪一套编号。
- ADR-MEMORY-P1、ADR-MEMORY-RECALL-MESSAGE-ORIGINS：文首仍是「PROPOSED / 待外审接受」，同段又写本修订被 clestons APPROVE（外审接受的是设计与首批范围，不是改状态标签）。状态按文首填「提议」。
- ADR-DOC-01：「公开 API 不传绝对路径（ADR-001）」没写是日志还是开放设计。不传绝对路径更像 ADR-OD-001。关联没有补，避免指错。
- ADR-032 引用 AgentEar 仓库的 ADR-0008，KB-01 引用外部 WeKnora ADR-001，都不在本索引 60 份里，关联没有加。
- 已有、正文未再点名的关联编号（例如 K1-01 上的 ADR-038 与 ADR-042，K1-02 上的 ADR-041，K1-03 上的 ADR-038）按只补不删留着。
