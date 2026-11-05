# 架构与方案（2026-11-05）

索引：[架构总入口](../../architecture/README.md)・[ADR 索引](../../adr/README.md)・同期另两份：[MILESTONES-AND-TASKS](MILESTONES-AND-TASKS.md)、[PROGRESS](PROGRESS.md)

| 项 | 内容 |
|---|---|
| 快照日期 | 2026-11-05（首期） |
| HEAD sha | `f818b0bfd7f53cdf15fd0265bbb56f4bb8cb9e8b`（`origin/main`，Merge #1591） |
| 上一期 | 无（首期）。统计区间见 PROGRESS |
| 数据来源 | 1. [架构优化方案](../2026-11-04/ARCH-OPTIMIZATION-PLAN.md)（2026-11-04）及 jason 同日拍板；2. 三份梳理报告 R1 / R2 / R3（`docs/architecture/review-2026-11-03/`，在分支 `docs/arch-review-r1-module-map`、`docs/arch-review-r2-concept-inventory`、`docs/arch-review-r3-adr-docs` 上，尚未合入 main）；3. `docs/adr/README.md`（ADR 索引）；4. `docs/ARCHITECTURE-LAYERS.md` |
| 生成方式 | 手工整理，不是脚本生成。每行给出处；报告与 main 冲突时以 main 为准。表里的「上次核对 sha」都是 `f818b0bf`，下期复制本文时要重新核对后再改 |
| 节奏 | 每两周或每次 release 出一期（Q11）。最新日期目录就是当前权威，旧目录只读 |
| 取代 | 本期起，`docs/agent/tasks.md`、`progress.md`、`roadmap.md` 降为历史（见各文件文头） |

## 0. 本期变化摘要

首期，没有「与上期相比」。进入本期的几个大事实：

1. 预览版停发，主线转为组件整合与统一（2026-11-04）。
2. Q1 拍板 (b)：Open Design 由 `SidecarManager` 托管，#1580、#1584 已合；ADR-038 的 C1–C8 全部完成。`agent24-sidecar-host` 冻结。
3. Q3 拍板 (B)：agent24d 下沉三块（iDoris → `agent24-idoris`，Knowledge → `agent24-knowledge`，attach → 新建 `agent24-attach`），鉴权留核心，原 S2-5 取消。三块仍编进同一个 agent24d 进程。
4. Q12 通讯：Hyphae 按需下载；agent 的 Nostr 身份以 `<data>/comm/hyphae-home` 为准；所有外部触发 agent 的入口统一走 ADR-041 准入关卡。
5. Q15 OS 包完整性：文件清单加哈希，哈希不对即拒绝加载，不做「先只记录」的过渡。
6. 第 0 期（文档与 ADR 收口）7 片开工，见 MILESTONES。

## 1. 架构现状一表

层的解释留在 [ARCHITECTURE-LAYERS.md](../../ARCHITECTURE-LAYERS.md)（S0-5 在分支 `docs/p0-layers-regen` 上重新生成，依赖表由 `scripts/gen-layer-deps.py` 从 `cargo metadata` 生成）。下表只列状态与链接。✅ 已接线　🟡 已实现未接入产品路径或有重要缺口　📐 仅设计。

| 层 / 子系统 | 状态 | 权威文档 | 代码入口 | 上次核对 sha |
|---|---|---|---|---|
| L0 契约（protocol、domain、`protocol/*.yaml/json`） | ✅；OpenAPI 手写，由 Rust 生成（B4）📐 | [SPEC-002](../../specs/SPEC-002-protocol.md) | `rust/crates/agent24-protocol`、`rust/crates/agent24-domain` | `f818b0bf` |
| L1 纯核心（core） | ✅ | [ADR-026](../../ADR-026-rust-core-polyglot.md) | `rust/crates/agent24-core` | `f818b0bf` |
| L2 持久化与 workspace（store、workspace、os-cwd；平台原语 os-proc、os-sqlite） | ✅；workspace-bound run 仅 Unix 且默认路径不可达 🟡 | [ADR-002](../../open-design-workspace/design/ADR-002-WORKSPACE-CONTRACT.md) | `rust/crates/agent24-store`、`rust/crates/agent24-workspace` | `f818b0bf` |
| L3 能力服务（models、tools、policy、memory、scheduler、mcp、comm、hw、components、idoris、knowledge；decide 🟡、worker 🟡） | ✅ 为主；decide 只有 profile 路由，worker 无消费者 | [COMPONENT-ROADMAP](../../agent/COMPONENT-ROADMAP.md) | `rust/crates/agent24-*` | `f818b0bf` |
| L4 运行时内核（agent） | ✅ | [agent/architecture.md](../../agent/architecture.md) | `rust/crates/agent24-agent` | `f818b0bf` |
| L4½ 领域 OS 运行时（os-proto、os-fd、os-packages、os-sdk；原称 L5½） | ✅ | [SPEC-ME3](../../specs/SPEC-ME3-OUT-OF-PROCESS.md)、[WIRE-OOP-MODULE](../../specs/WIRE-OOP-MODULE.md) | `rust/crates/agent24-os-proto` | `f818b0bf` |
| L5 组合根 agent24d（非测试约 26,300 行；瘦身 Q3 (B) 未动手 📐） | ✅；capability 令牌 ✅，路由级资源授权 📐 | [ADR-005](../../open-design-workspace/design/ADR-005-CAPABILITY-AUTHORITY.md)、[CORE-2-ENROLL-DESIGN](../../agent/CORE-2-ENROLL-DESIGN.md) | `rust/apps/agent24d/src/server.rs` | `f818b0bf` |
| CORE-5 迁移保护 | 🟡 5f 已合但在 `ENABLED=false` 后面；剩 5g/5h/5i | [CORE-5-MIGRATION-GUARD-DESIGN](../../agent/CORE-5-MIGRATION-GUARD-DESIGN.md) | `rust/apps/agent24d/src/migration_coordinator.rs:20` | `f818b0bf` |
| L6 扩展面：进程外领域 OS（Sin90 / Cos72 / Documents） | ✅ | [SPEC-ME3](../../specs/SPEC-ME3-OUT-OF-PROCESS.md)、[ADR-K1-01](../../adr/ADR-K1-01-module-tool-contract.md) | `rust/apps/agent24-documents` | `f818b0bf` |
| L6 扩展面：附着模块（AgentEar） | ✅；C4 AgentEar 纵切（CORE-2）待 CORE-2 余项 | [A3-ATTACHED-MODULE](../../design/A3-ATTACHED-MODULE.md) | `rust/apps/agent24d/src/attach_registry.rs` | `f818b0bf` |
| L6 扩展面：Open Design（sidecar） | ✅ `SidecarManager` 托管（C8，#1580、#1584）；`agent24-sidecar-host` 冻结；Windows 暂不支持；路由级资源授权后的受限令牌 📐 | [C8-SIDECAR-DESIGN](../2026-11-04/C8-SIDECAR-DESIGN.md)、[PLAN-OD-NEXT](../../agent/PLAN-OD-NEXT.md) | `apps/desktop/src/main/sidecar-manager.ts` | `f818b0bf` |
| 组件机制（ADR-038：ComponentLock、下载器、安装器、签名、`/api/v1/components`） | ✅ C1–C8 全部完成；运行期契约检查未接 🟡 | [ADR-038](../../adr/ADR-038-component-contract-v1.md) | `rust/crates/agent24-components`、`rust/apps/agent24d/src/component_routes.rs` | `f818b0bf` |
| 通讯（Hyphae） | ✅ 按需安装、macOS 验签、NIP-49 备份恢复；好友机制 📐；无独立 ADR（ADR-044 草案在 S0-4） | [COMM-HYPHAE](../../design/COMM-HYPHAE.md)、[COMM-FRIENDS](../../design/COMM-FRIENDS.md) | `rust/crates/agent24-comm` | `f818b0bf` |
| 记忆（M-D） | ✅ recall 进 agent loop；consolidator / vector / condenser 无消费者 🟡；升级保护开关关闭 | [PLAN-MEMORY-P1](../../agent/PLAN-MEMORY-P1.md) | `rust/crates/agent24-memory`、`rust/crates/agent24-agent/src/session_memory.rs` | `f818b0bf` |
| 知识库（Knowledge） | ✅ 只读 search/read 已发布；Provider 未接线 🟡；管理页、深度子任务、写操作 📐 | [ADR-KB-01](../../knowledge/ADR-KB-01-SINGLE-CONVERSATION.md) | `rust/apps/agent24d/src/knowledge_host.rs` | `f818b0bf` |
| 决策服务（decide）、硬件分档（hw） | decide 🟡（D0 契约与规则后端，`DecisionLog` 无实现）；hw ✅ 但 runtimes 未探测 | [PLAN-DECIDE](../../agent/PLAN-DECIDE.md)、[HW-TIERS](../../design/HW-TIERS-capability-hardware-matrix.md) | `rust/crates/agent24-decide`、`rust/crates/agent24-hw` | `f818b0bf` |
| 外壳：桌面、CLI | ✅；CLI 缺桌面已有的业务入口（Q5） | [ADR-037 事实清单](../2026-10-26/SOURCE-ADR-037-facts.md) | `apps/desktop`、`rust/apps/agent24-cli` | `f818b0bf` |

## 2. ADR 状态快照

状态列取自 `f818b0bf` 的 [ADR 索引](../../adr/README.md)。**这是订正前的状态**：S0-1 / S0-2 合入后下期更新。「拟订正」列是方案 Q8 / Q9 的推荐（jason 2026-11-04 按推荐执行）。实现度取自 R3 §1，「未核实」表示 R3 也没有结论。

| ADR | 索引状态（现） | 拟订正 | 实现度 | 最近相关 PR | 下一步 |
|---|---|---|---|---|---|
| ADR-037 产品形态 | 提议 | 已接受（决策 1–5，决策 6 待遥控设计） | 部分：决策 1、3、4 骨架已落；决策 2 停靠态已写未启用；决策 6 未开始 | #1292 #1295 #1301 #1371 #1378 #1428 #1528 #1540 | S0-1 订正 |
| ADR-038 组件契约 v1 | 草案 r3 | r5 订正稿后已接受 | 完成 C1–C8；运行期契约检查未接 | #1539 #1545 #1549 #1551 #1580 #1584 | S0-2 |
| ADR-039 概念模型 | 已接受 | — | 概念层，无实现项 | — | — |
| ADR-040 技能契约 | 提议 | 接受为设计约束（C7 已完成，`requires` 非空可用） | 无（Skill 加载器没开工，#1037） | — | S0-1 |
| ADR-041 入站事件与准入 | 提议 | 拆 041a / 041b | 无；且 Q12 X11 要求所有外部入口统一走它 | — | S0-1；S0-4 写清与 ADR-044 的边界 |
| ADR-042 待执行动作与对账 | 提议 | 先接受 D3、D4 边界 | 无 | — | S0-1 |
| ADR-043 后台作业与监督回路 | 提议 | 先接受 D1 + D3（`running_jobs` / `budget`） | 无（`jobs` 表未实现） | — | S0-1 |
| ADR-044 通讯 | 未建 | 新建草案 | 通讯决策 31 条散在 COMM-HYPHAE / F4 / PR | — | S0-4 |
| ADR-045 路由命名约定 | 未建 | 新建 | 无 | — | S0-7 |
| ADR-MEMORY-P1、ADR-MEMORY-RECALL-MESSAGE-ORIGINS | 提议 | 已接受（设计，外审已 APPROVE：#1247、#1248） | 部分（P1-02c 进行中） | #1247 #1248 | S0-1 |
| ADR-028 记忆架构 | 未标 | 以收口段为正文，修订后接受 | 未核实 | — | S0-1 |
| ADR-KB-01 | 未标 | 拆 01a（已接受）/ 01b（提议） | 01a 已实现（#1503、#1504、#1506、#1507、#1510、#1511、#1515）；01b 未实现 | #1515 | S0-1 |
| docs/ADR-016（消费 @auraaihq 包） | 进行中（文首原文，索引与日志 ADR-016 重号） | 被替代，加别名 ADR-016P | 未实现，方向已取代 | #45 #1371 | S0-1 / S0-3 |
| ADR-005 / 008 / 009 / 021 | 已接受 | 废弃（从未实现） | 无 | — | S0-1 |
| ADR-DOC-03 | 已接受 | 搁置（待 #692） | 无 | — | S0-1 |
| ADR-032 / 035 / 036 | 已接受 | ADR-035 标部分取代（S0-8） | 已实现 | — | S0-8 |
| ADR-K1-01…04 | 已接受 | — | 已实现（Documents 是第一个消费方） | — | — |

编号规则（Q10）：只加别名，不改文件名；OD 的 ADR-001–006 记 `ADR-OD-00x`，docs/ADR-016 记 `ADR-016P`，初稿记 `DRAFT-F04x`，Codex 稿记 `DRAFT-C04x`。ADR 索引是唯一发号处，**下一号 ADR-044**（通讯），ADR-045 留给路由命名约定。

## 3. 在途方案（设计中、未成 ADR）

| 方案 | 设计文档 | 拟成 ADR 号 | 等谁拍板 |
|---|---|---|---|
| 架构优化方案（本期总纲） | [ARCH-OPTIMIZATION-PLAN](../2026-11-04/ARCH-OPTIMIZATION-PLAN.md) | — | 已按 §5 推荐拍板（2026-11-04） |
| 通讯 | [COMM-HYPHAE](../../design/COMM-HYPHAE.md)、[COMM-FRIENDS](../../design/COMM-FRIENDS.md) | ADR-044 | 前置 X1 / X7 / X11 已定，写稿在 S0-4 |
| 路由命名约定 | 方案 §3 B5、Q4 | ADR-045（或 SPEC-002 附录） | 约定选 (a)：只约束新路由；S0-7 |
| 好友机制（加好友 / 申请 / 陌生人消息，删除不等于屏蔽） | [COMM-FRIENDS](../../design/COMM-FRIENDS.md) | 待定 | 先做常规通信，之后设计 |
| 决策模型（agent24-decide） | [PLAN-DECIDE](../../agent/PLAN-DECIDE.md) | ADR-033 / 034 已接受 | D1 接线未排期 |
| 硬件分档 | [HW-TIERS](../../design/HW-TIERS-capability-hardware-matrix.md) | ADR-036 已接受 | — |

## 4. 已知偏离（文档 vs 代码）

来自方案 §3 的 A 类、F 类和 R3 §4。「修正」列是计划中的片，不是已完成。

| 文档位置 | 偏离 | 修正 |
|---|---|---|
| `docs/ARCHITECTURE-LAYERS.md` 依赖表等 | 错 4 行；L5½ 误导；unsafe 边界只写 2 个 crate；node-daemon 只覆盖 4/124 | S0-5（分支 `docs/p0-layers-regen`） |
| ADR-038 状态行 | 「草案 r3」落后正文（r4 / r5）；正文自相矛盾 16 处 | S0-2 |
| `decision.md:1508` vs `CORE-5-MIGRATION-GUARD-DESIGN.md:200` | ADR-037 状态矛盾 | S0-1 |
| `REGISTRY.md` documents 行 | 「契约来源」写 `domain-os.yml`，实际在 `rust/apps/agent24-documents/component.json` | S0-2 |
| 通讯 ADR 缺失 | 31 条决策散落；X1（随包 vs 按需）、X7（两套 Nostr 身份）、X11（入站执行入口）三处矛盾，已拍板 | S0-4 |
| 编号冲突 | 日志 ADR-001–006 与 OD ADR-001–006；两份 ADR-016；`SUPERAGENT-EVOLUTION-2026-11-01-codex-astra.md` 里已有一份「ADR-044」 | S0-3 |
| 桌面有业务、CLI 无入口 | Knowledge 绑定 / 凭据、presence、OD 下载启动、Comm 私钥备份恢复（违反 `AGENTS.md` 的 CLI 与桌面共用逻辑要求） | Q5 选 (b)，S2-7 |
| OS 包完整性 | 外仓 OS 包（Sin90、Cos72）没有文件清单与哈希校验；R2 评为现存唯一无人认领的「高」安全缺口 | Q15，计划排第 1 期末 |
| OpenAPI | 少 `GET /approvals/{id}`；门禁只比路径不比方法；`_a24/*` 无豁免 | S1-1 |
| 签名要求 | os-proc 的 requirement 比 components 松；Team ID 多处写死 | S1-2 |

## 5. 待拍板问题

2026-11-04 jason 已拍板 Q1、Q3、Q12、Q15，其余 Q2、Q4–Q11、Q13、Q14、Q16、Q17 全部按方案 §5 的「推荐」执行。本期**没有未决的拍板问题**。执行中如发现推荐不成立，在此登记并写明日期和出处：

| 日期 | 问题 | 登记人 |
|---|---|---|
| — | （空） | — |
