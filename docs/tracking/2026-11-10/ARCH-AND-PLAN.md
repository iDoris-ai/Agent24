# 架构与方案（2026-11-10）

索引：[架构总入口](../../architecture/README.md)・[ADR 索引](../../adr/README.md)・同期另两份：[MILESTONES-AND-TASKS](MILESTONES-AND-TASKS.md)、[PROGRESS](PROGRESS.md)

| 项 | 内容 |
|---|---|
| 快照日期 | 2026-11-10（第二期） |
| HEAD sha | `c396478c9f8797c339096dbcdbf4d01b242d756f`（`origin/main`，Merge #1780） |
| 上一期 | [2026-11-05](../2026-11-05/ARCH-AND-PLAN.md)，HEAD `f818b0bfd7f53cdf15fd0265bbb56f4bb8cb9e8b`。统计区间 `f818b0bf..c396478c`，见 PROGRESS |
| 数据来源 | 1. [架构优化方案](../2026-11-04/ARCH-OPTIMIZATION-PLAN.md)（2026-11-04）及 jason 拍板；2. `docs/adr/README.md`（ADR 索引）；3. `docs/ARCHITECTURE-LAYERS.md`；4. 本地 git 与 `gh pr view`（核对 PR 合并状态）；5. [S1-9 设计](../2026-11-04/S1-9-OS-PACKAGE-INTEGRITY-DESIGN.md)（#1611、r2 #1723）；6. 本仓代码的实测行数（命令见 §1 表后） |
| 生成方式 | 手工整理，不是脚本生成。首期那批梳理报告 R1 / R2 / R3 本期没有重读；凡本期没重新核对的行，「上次核对 sha」仍写 `f818b0bf`，不冒充已核对。下期复制本文时要重新核对后再改 |
| 节奏 | 每两周或每次 release 出一期（Q11）。最新日期目录就是当前权威，旧目录只读 |

## 0. 本期变化摘要

与首期（2026-11-05）相比：

1. 第 0 期（文档与 ADR 收口）S0-1 至 S0-7 全部合入：ADR-038 r5 已接受（#1592）、ADR-044 通讯与 ADR-045 路由命名约定已写成并改为已接受（#1593、#1600、#1601），layers 重新生成（#1594），首期跟踪文档（#1595），状态与编号订正（#1596）。S0-8 仍未做。
2. 第 1 期全部完成：S1-1 OpenAPI 漂移、S1-2 签名单源、S1-3 `daemon.json` 单一解析器、S1-4 状态映射、S1-5 OD pin 单源、S1-7 休眠代码收窄已合；S1-6、S1-8 取消。
3. 首期「未编号」的 OS 包完整性（Q15）现编号 **S1-9**，已完成：文件清单与哈希、哈希不对即拒绝、macOS 验签（可 trust-team）、SDK 0.2.0 握手前清单自检均已合入。外仓 Sin90、Cos72 已按新规则出了签名包。详见 MILESTONES §2.1。
4. **更正首期一处误标**：首期把组件统一的 C2（Hyphae 运行期契约检查）标成 BACKLOG，实际在首期之前就已完成（见 §4）。
5. 本期发现并订正一处文档不一致：ADR 索引仍把 ADR-038 写成「草案（原文：草案 r3）」，而正文已是「已接受（r5，2026-11-05）」。已在本 PR 修正索引（`docs/adr/README.md` 两处表格），见 §2。
6. 第 2 期（agent24d 瘦身）2026-11-10 当天推进：尾巴（本跟踪文档、旧集成分支、X11 ① / X7）清完后，S2-1/2/3/4a 合入，S2-6、S2-7 按拍板改为文档，S2-4b #1794 在审；agent24d 从 96,032 行减到 82,276 行（含 #1794）。逐片见 MILESTONES-AND-TASKS 第 2 期表。

## 1. 架构现状一表

层的解释留在 [ARCHITECTURE-LAYERS.md](../../ARCHITECTURE-LAYERS.md)（依赖表已由 #1594 用 `scripts/gen-layer-deps.py` 从 `cargo metadata` 重新生成）。下表只列状态与链接。✅ 已接线　🟡 已实现未接入产品路径或有重要缺口　📐 仅设计。

「上次核对 sha」：`c396478c` 表示本期重新核对过；`f818b0bf` 表示沿用首期结论、本期未重新核对。

| 层 / 子系统 | 状态 | 权威文档 | 代码入口 | 上次核对 sha |
|---|---|---|---|---|
| L0 契约（protocol、domain、`protocol/*.yaml/json`） | ✅；OpenAPI 手写，由 Rust 生成（B4）📐；漂移门禁已收口（S1-1，#1606） | [SPEC-002](../../specs/SPEC-002-protocol.md)、[ADR-045](../../adr/ADR-045-route-naming.md) | `rust/crates/agent24-protocol`、`rust/crates/agent24-domain` | `f818b0bf`（S1-1 变化见 MILESTONES） |
| L1 纯核心（core） | ✅ | [ADR-026](../../ADR-026-rust-core-polyglot.md) | `rust/crates/agent24-core` | `f818b0bf` |
| L2 持久化与 workspace（store、workspace、os-cwd；平台原语 os-proc、os-sqlite） | ✅；workspace-bound run 仅 Unix 且默认路径不可达 🟡；store 休眠代码已收窄（S1-7，#1612） | [ADR-002](../../open-design-workspace/design/ADR-002-WORKSPACE-CONTRACT.md) | `rust/crates/agent24-store`、`rust/crates/agent24-workspace` | `f818b0bf` |
| L3 能力服务（models、tools、policy、memory、scheduler、mcp、comm、hw、components、idoris、knowledge；decide 🟡、worker 🟡） | ✅ 为主；decide 只有 profile 路由，worker 无消费者 | [COMPONENT-ROADMAP](../../agent/COMPONENT-ROADMAP.md) | `rust/crates/agent24-*` | `f818b0bf` |
| L4 运行时内核（agent） | ✅ | [agent/architecture.md](../../agent/architecture.md) | `rust/crates/agent24-agent` | `f818b0bf` |
| L4½ 领域 OS 运行时（os-proto、os-fd、os-packages、os-sdk） | ✅；**OS 包完整性已落地**：清单必需、哈希不对即拒绝、五类拒绝原因、macOS 验签；os-sdk 0.2.0 握手前清单自检（tag `agent24-os-sdk-v0.2.0`，2026-11-09） | [SPEC-ME3](../../specs/SPEC-ME3-OUT-OF-PROCESS.md)、[WIRE-OOP-MODULE](../../specs/WIRE-OOP-MODULE.md)、[S1-9 设计](../2026-11-04/S1-9-OS-PACKAGE-INTEGRITY-DESIGN.md)（#1611、#1723） | `rust/crates/agent24-os-packages`、`rust/crates/agent24-os-sdk` | `c396478c`（PR 清单见 PROGRESS） |
| L5 组合根 agent24d（`rust/apps/agent24d/src` 全部 `.rs` 共 96,032 行，含内联测试；`server.rs` 9,345 行，其中 `serve()` 起于 1978 行、止于 3513 行，约 1,536 行；`mod tests` 起于 4050 行；瘦身 Q3 (B) 未动手 📐） | ✅；capability 令牌 ✅，路由级资源授权 📐 | [ADR-005](../../open-design-workspace/design/ADR-005-CAPABILITY-AUTHORITY.md)、[CORE-2-ENROLL-DESIGN](../../agent/CORE-2-ENROLL-DESIGN.md) | `rust/apps/agent24d/src/server.rs` | `c396478c` |
| CORE-5 迁移保护 | 🟡 5f 已合但在 `ENABLED=false` 后面；剩 5g/5h/5i | [CORE-5-MIGRATION-GUARD-DESIGN](../../agent/CORE-5-MIGRATION-GUARD-DESIGN.md) | `rust/apps/agent24d/src/migration_coordinator.rs:20` | `f818b0bf` |
| L6 扩展面：进程外领域 OS（Sin90 / Cos72 / Documents） | ✅；Sin90 v0.5.2、Cos72 v0.1.2 已是签名公证加完整性清单的包（见下「OS 包与外仓」）；Cos72 是独立项目，只做基础对齐 | [SPEC-ME3](../../specs/SPEC-ME3-OUT-OF-PROCESS.md)、[ADR-K1-01](../../adr/ADR-K1-01-module-tool-contract.md) | `rust/apps/agent24-documents` | `c396478c` |
| L6 扩展面：附着模块（AgentEar） | ✅；C4 AgentEar 纵切（CORE-2）待 CORE-2 余项 | [A3-ATTACHED-MODULE](../../design/A3-ATTACHED-MODULE.md) | `rust/apps/agent24d/src/attach_registry.rs` | `f818b0bf` |
| L6 扩展面：Open Design（sidecar） | ✅ `SidecarManager` 托管（C8）；`agent24-sidecar-host` 冻结；Windows 暂不支持；OD pin 已单一来源（S1-5，#1602） | [C8-SIDECAR-DESIGN](../2026-11-04/C8-SIDECAR-DESIGN.md)、[PLAN-OD-NEXT](../../agent/PLAN-OD-NEXT.md) | `apps/desktop/src/main/sidecar-manager.ts` | `f818b0bf` |
| 组件机制（ADR-038：ComponentLock、下载器、安装器、签名、`/api/v1/components`） | ✅ C1–C8 全部完成（含 C2）；C9（`requires` 接入授权）未排期 | [ADR-038](../../adr/ADR-038-component-contract-v1.md) | `rust/crates/agent24-components`、`rust/apps/agent24d/src/component_routes.rs` | `c396478c`（C2 见 §4） |
| 通讯（Hyphae） | ✅ 按需安装、macOS 验签、NIP-49 备份恢复；**ADR-044 已接受**；好友机制 📐；锁只有 darwin-arm64、linux-x64 两个平台 | [ADR-044](../../adr/ADR-044-communication.md)、[COMM-HYPHAE](../../design/COMM-HYPHAE.md)、[COMM-FRIENDS](../../design/COMM-FRIENDS.md) | `rust/crates/agent24-comm` | `c396478c`（锁平台数已核对）；其余 `f818b0bf` |
| 记忆（M-D） | ✅ recall 进 agent loop；consolidator / vector / condenser 无消费者 🟡；升级保护开关关闭 | [PLAN-MEMORY-P1](../../agent/PLAN-MEMORY-P1.md) | `rust/crates/agent24-memory`、`rust/crates/agent24-agent/src/session_memory.rs` | `f818b0bf` |
| 知识库（Knowledge） | ✅ 只读 search/read 已发布；Provider 未接线 🟡；管理页、深度子任务、写操作 📐；本期 ab/weknora 又有五批集成合并（#1620、#1663、#1665、#1693、#1697），内容细节未核实 | [ADR-KB-01](../../knowledge/ADR-KB-01-SINGLE-CONVERSATION.md) | `rust/apps/agent24d/src/knowledge_host.rs` | `f818b0bf` |
| 决策服务（decide）、硬件分档（hw） | decide 🟡（D0 契约与规则后端，`DecisionLog` 无实现）；hw ✅ 但 runtimes 未探测 | [PLAN-DECIDE](../../agent/PLAN-DECIDE.md)、[HW-TIERS](../../design/HW-TIERS-capability-hardware-matrix.md) | `rust/crates/agent24-decide`、`rust/crates/agent24-hw` | `f818b0bf` |
| 外壳：桌面、CLI | ✅；CLI 缺桌面已有的业务入口（Q5）；本期 ab/ui 有四次集成合并（#1637、#1658、#1716、#1728），内容见 PROGRESS 标题，细节未核实 | [ADR-037 事实清单](../2026-10-26/SOURCE-ADR-037-facts.md) | `apps/desktop`、`rust/apps/agent24-cli` | `f818b0bf` |

实测命令（在 `c396478c`）：

```
find rust/apps/agent24d/src -name '*.rs' | xargs wc -l | tail -1   # 96032 total
wc -l rust/apps/agent24d/src/server.rs                             # 9345
grep -n 'pub async fn serve' rust/apps/agent24d/src/server.rs      # 1978
awk 'NR>=1978 && /^}/ {print NR; exit}' rust/apps/agent24d/src/server.rs   # 3513
```

首期写的「agent24d 非测试约 26,300 行」是另一个口径，本期没有按该口径重算，所以两期数字不能直接比较。

**OS 包与外仓（S1-9 结果）。** 规则现在是：OS 包必须带文件清单和哈希，哈希不对即拒绝加载，没有过渡期；安装包里的可执行文件只允许本平台签名原生程序，解释器类模块（如 Node 参考模块）走 attach（N3）；macOS 上验签，默认只信任本方 Team，可 `trust-team` 增加受信团队；`os install` 离线验签，不在线查公证。外仓对应：Sin90 #80、#81、#82、#83（v0.5.2）、Cos72 #14、#15、#16（v0.1.2），两个 Release 各带 linux-arm64、linux-x64、macos-arm64、macos-x64 四个包和 `SHA256SUMS`（`gh release view` 核对）。包是否都已签名公证，本期没有逐个验，**未核实**；按外仓 PR 标题，正式 Release 只上传 Linux 包，macOS 包由本机签名后上传。

## 2. ADR 状态快照

状态列取自 `c396478c` 的 [ADR 索引](../../adr/README.md)（含本 PR 对 ADR-038 的订正）。首期的「拟订正」已在第 0 期全部执行。实现度本期没有重做，沿用首期取自 R3 的结论，「未核实」表示 R3 也没有结论。

**本期发现的不一致**：索引表格把 ADR-038 写成「草案（原文：草案 r3）」，而 ADR-038 文首是「已接受（r5 订正稿，2026-11-05）」。#1592 改了正文没同步索引，索引的核对记录还写着「本次不动，以 #1592 为准」。本 PR 已把索引两处表格改为「已接受（r5，2026-11-05）」并在核对记录补一行。

| ADR | 索引状态（现） | 实现度 | 最近相关 PR | 下一步 |
|---|---|---|---|---|
| ADR-037 产品形态 | 已接受（决策 1–5；决策 6 待遥控设计） | 部分：决策 1、3、4 骨架已落；决策 2 停靠态已写未启用；决策 6 未开始 | #1292 #1295 #1301 #1371 #1378 #1428 #1528 #1540 | — |
| ADR-038 组件契约 v1 | 已接受（r5，2026-11-05；本 PR 订正索引） | 完成 C1–C8；C9 未排期 | #1539 #1545 #1549 #1551 #1580 #1584 #1592 | — |
| ADR-039 概念模型 | 已接受 | 概念层，无实现项 | — | — |
| ADR-040 技能契约 | 已接受（作为设计约束） | 无（Skill 加载器没开工，#1037） | — | — |
| ADR-041a 入站事件与准入 | 已接受 | 无；ADR-044 与 Q12 X11 要求所有外部入口统一走它 | — | X11 落地（见 MILESTONES §2.4） |
| ADR-041b 工单、授权依据与订阅 | 提议 | 无 | — | — |
| ADR-042 待执行动作与对账 | 部分接受（D3、D4 边界已接受，其余提议） | 无 | — | — |
| ADR-043 后台作业与监督回路 | 部分接受（D1、D3 已接受，其余提议） | 无（`jobs` 表未实现） | — | — |
| ADR-044 通讯 | 已接受（2026-11-05） | 决策已写成文；X7、X11 落地未做 | #1593 #1601 | X7、X11 |
| ADR-045 路由命名约定 | 已接受（2026-11-05） | 只约束新路由；门禁测试是否已有，未核实 | #1600 #1601 | — |
| ADR-MEMORY-P1、ADR-MEMORY-RECALL-MESSAGE-ORIGINS | 已接受（设计） | 部分（purge、无痕、导出、更正待实现） | #1247 #1248 | — |
| ADR-028 记忆架构 | 已接受（以收口段为正文） | 未核实 | — | — |
| ADR-KB-01a / 01b | 01a 已接受、01b 提议 | 01a 已实现；01b 未实现 | #1515 | — |
| docs/ADR-016（消费 @auraaihq 包，别名 ADR-016P） | 被替代（取代者 ADR-026 §6） | 未实现，方向已取代 | — | — |
| ADR-005 / 008 / 009 / 021 | 废弃 | 无 | — | — |
| ADR-DOC-03 | 搁置（待 #692） | 无 | — | — |
| ADR-032 / 035 / 036 | 已接受 | 已实现；ADR-035 标部分取代未做（S0-8） | — | S0-8 |
| ADR-K1-01…04 | 已接受 | 已实现（Documents 是第一个消费方） | — | — |

编号规则（Q10）：只加别名，不改文件名。ADR 索引是唯一发号处，**下一号 ADR-046**（索引 `docs/adr/README.md` 第 7 行、「发号规则」均如此写）。ADR-044（通讯）、ADR-045（路由命名约定）已有文件。

## 3. 在途方案（设计中、未成 ADR）

| 方案 | 设计文档 | 拟成 ADR 号 | 等谁拍板 |
|---|---|---|---|
| 架构优化方案（总纲） | [ARCH-OPTIMIZATION-PLAN](../2026-11-04/ARCH-OPTIMIZATION-PLAN.md) | — | 已按 §5 推荐拍板（2026-11-04）；第 2 期 S2-1 起 |
| 好友机制（加好友 / 申请 / 陌生人消息，删除不等于屏蔽） | [COMM-FRIENDS](../../design/COMM-FRIENDS.md) | 待定 | jason 定为之后做，本轮不动；先做常规通信 |
| 组件 `requires` 接入授权（C9） | [ADR-038](../../adr/ADR-038-component-contract-v1.md) | — | 未排期，不在第 0–2 期 |
| 决策模型（agent24-decide） | [PLAN-DECIDE](../../agent/PLAN-DECIDE.md) | ADR-033 / 034 已接受 | D1 接线未排期 |
| 硬件分档 | [HW-TIERS](../../design/HW-TIERS-capability-hardware-matrix.md) | ADR-036 已接受 | — |

首期在本表里的「通讯 ADR-044」「路由命名 ADR-045」已成文并接受，移出本表。

## 4. 已知偏离（文档 vs 代码）

「修正」列是计划中的片，不是已完成。已完成的移到下面「本期已修正」。

| 文档位置 | 偏离 | 修正 |
|---|---|---|
| `decision.md:1508` vs `CORE-5-MIGRATION-GUARD-DESIGN.md:200` | ADR-037 状态矛盾 | S0-1 已订正索引；这两处正文本期未重新核对，**未核实** |
| 桌面有业务、CLI 无入口 | Knowledge 绑定 / 凭据、presence、OD 下载启动、Comm 私钥备份恢复（违反 `AGENTS.md` 的 CLI 与桌面共用逻辑要求） | S2-7（#1789）：四项都写为 `AGENTS.md` 的桌面专属例外；前两类在 daemon 上只允许 ProductHost，CLI 调不通 |
| 通讯入口 | 外部入口尚未统一走 ADR-041a 准入；nostr-bridge 尚未并入 comm、仍是一条独立身份来源 | X11、X7（尾巴，见 MILESTONES §2.4） |
| Hyphae 锁平台 | `rust/crates/agent24-comm/hyphae.lock.json` 只有 darwin-arm64、linux-x64 | BACKLOG |
| `server.rs` | 单文件 9,345 行，`serve()` 约 1,536 行 | S2-1（#1791）已拆：`serve()` 267 行，启动步骤在 `serve_steps.rs` |

**本期已修正**

| 文档位置 | 偏离 | 结果 |
|---|---|---|
| ADR 索引 ADR-038 行 | 写「草案（原文：草案 r3）」，正文已是已接受 r5 | 本 PR 订正 |
| 首期 MILESTONES §2.2 的 C2 行 | **第一期误标 BACKLOG**（称「Agent24 侧没有向 Hyphae 做 `hyphae-cli` 契约握手」）。实际 C2 已完成：Hyphae contract 握手与 health 由提交 `93da3e900`（feat(comm): A1）、`953c3acb5`（A1 评审修订）落地，合并为 #1574，见 ADR-038 §4 C2 行。该合并在首期快照之前（首期 PROGRESS 已列 #1574） | 本期 MILESTONES 改为 DONE |
| `docs/ARCHITECTURE-LAYERS.md` 依赖表等 | 错 4 行等 | S0-5 已合（#1594） |
| ADR-038 状态行落后正文 | 「草案 r3」vs r4/r5 | S0-2 已合（#1592）；索引遗漏见 §2 |
| 通讯 ADR 缺失 | X1、X7、X11 三处矛盾 | ADR-044 已接受（#1593、#1601） |
| 编号冲突 | 日志与 OD 的 ADR-001–006、两份 ADR-016 | S0-3 已合（#1596） |
| OpenAPI | 少 `GET /approvals/{id}`；门禁只比路径不比方法 | S1-1 已合（#1606） |
| 签名要求 | os-proc 比 components 松；Team ID 多处写死 | S1-2 已合（#1608） |
| OS 包完整性 | 无文件清单与哈希校验 | S1-9 已合，见 §1 |

`REGISTRY.md` documents 行的「契约来源」写法（首期 S0-2 范围）本期没有重新核对，**未核实**。

## 5. 待拍板问题

2026-11-04 jason 已拍板 Q1、Q3、Q12、Q15，其余按方案 §5 推荐执行。2026-11-09、11-10 的新裁决见 [MILESTONES §0](MILESTONES-AND-TASKS.md)。本期**没有未决的拍板问题**，两项明确推迟的事列在这里，避免被当成遗漏：

| 日期 | 事项 | 状态 |
|---|---|---|
| 2026-11-10 | 好友机制 | jason 定为之后做，不在本轮 |
| 2026-11-10 | C9（`requires` 接入授权） | 未排期，不在第 0–2 期 |
