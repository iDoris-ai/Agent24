# 架构与方案（2026-11-17）

索引：[架构总入口](../../architecture/README.md)・[ADR 索引](../../adr/README.md)・同期另两份：[MILESTONES-AND-TASKS](MILESTONES-AND-TASKS.md)、[PROGRESS](PROGRESS.md)

| 项 | 内容 |
|---|---|
| 快照日期 | 2026-11-17（第三期） |
| HEAD sha | `b7eef06f2b6cde7189eb90d9e0a4840c0b202f1f`（`origin/main`，Merge #1881） |
| 上一期 | [2026-11-10](../2026-11-10/ARCH-AND-PLAN.md)，HEAD `c396478c9f8797c339096dbcdbf4d01b242d756f`（Merge #1780）。统计区间 `c396478c..b7eef06f2`，见 PROGRESS |
| 数据来源 | 1. 上期三份；2. `docs/adr/README.md`（HEAD）；3. 本期方案：[C9](../2026-11-12/C9-REQUIRES-AUTHZ-PLAN.md)、[技能加载器 v1](../2026-11-12/SKILL-LOADER-V1-PLAN.md)、[向量召回](../2026-11-13/VECTOR-RECALL-PLAN.md)、[模块沙箱](../2026-11-14/MODULE-SANDBOX-PLAN.md)、[入站准入](../2026-11-10/INBOUND-ADMISSION-PLAN.md)；4. `gh pr view`、`git log --first-parent --merges`、`gh release view`；5. 本仓行数（命令见 §1） |
| 生成方式 | 手工整理，不是脚本生成。上期没重核的行，核对 sha 仍写上期的值。下期复制本文时要重新核对后再改 |
| 节奏 | 每两周或每次 release 出一期（Q11）。最新日期目录就是当前权威，旧目录只读 |

## 0. 本期变化摘要

与上期快照（`c396478c`，#1780）相比。上期正文后来又手写补过第 2 期和 X11，那些合并提交仍落在本区间里，PROGRESS 表会再列一遍。

1. 第 2 期收完：S2-1 至 S2-4b、S2-6、S2-7 都在 2026-11-10 晚合入（#1788–#1794）。接着 SPLIT-1..4（#1859–#1862）把 `agent24-agent` 的 `lib.rs` 和 `agent24d` 的 `server.rs` 按职责拆开，纯移动。
2. X11 收口：产品隔离与删除旧 nostr-bridge（#1785），入站存储、准入、清理、微信只传输、Hyphae 入站（#1797、#1803–#1821、#1826）。手机遥控、ADR-041b 工单、自动回发不做（FU-125）。
3. C9 完成：`requires` 进授予（#1825、#1830、#1831、#1833、#1841）。第一方启用即同意。第三方同意卡不做。
4. 技能加载器 v1 完成：解析、迁移、导入、激活、注入、技能栏、CLI（#1824、#1827–#1829、#1832、#1834–#1836、#1840、#1842）。
5. 向量召回接进对话：VR-1..VR-5 经 `ab/vector-recall` 由 #1851 进 main，VR-6 改写问句直接进 main（#1856）。嵌入在本机，失败退回关键词。
6. 模块沙箱第一版：方案 #1858；macOS seatbelt #1863；每模块临时目录 #1865；组件页隔离状态 #1864。Linux 不套沙箱。SBX-3 暂不做。
7. 模型选择：默认对话模型可保存（#1879、#1881），后台起不来会显示原因（#1880）。按内存推荐模型等 iDoris #602。
8. S0-8 已合（#1823）：ADR-KB-02、ADR-046、ADR-035 改为部分取代。下一号 ADR-047。
9. 预览版重新发出两版：`v0.6.0-preview.20261114.1`、`v0.6.0-preview.20261117.1`。jason 2026-11-14、11-17 授权恢复发布，书面记录见 MILESTONES §0。
10. 只推主干写进 `AGENTS.md` / `CLAUDE.md`（#1846，2026-11-13）。

## 1. 架构现状一表

层的解释留在 [ARCHITECTURE-LAYERS.md](../../ARCHITECTURE-LAYERS.md)。✅ 已接线　🟡 已实现未接入产品路径或有重要缺口　📐 仅设计。

「上次核对 sha」：`b7eef06f2` 表示本期重新核对过。更早的 sha 表示沿用旧结论。

| 层 / 子系统 | 状态 | 权威文档 | 代码入口 | 上次核对 sha |
|---|---|---|---|---|
| L0 契约 | ✅；漂移门禁 S1-1 已收口 | [SPEC-002](../../specs/SPEC-002-protocol.md)、[ADR-045](../../adr/ADR-045-route-naming.md) | `rust/crates/agent24-protocol` | `f818b0bf` |
| L1 纯核心 | ✅ | [ADR-026](../../ADR-026-rust-core-polyglot.md) | `rust/crates/agent24-core` | `f818b0bf` |
| L2 持久化与 workspace | ✅；workspace-bound 缺口沿用上期 🟡 | [ADR-002](../../open-design-workspace/design/ADR-002-WORKSPACE-CONTRACT.md) | `rust/crates/agent24-store` | `f818b0bf` |
| L3 能力服务 | ✅ 为主；decide、worker 沿用上期 🟡。技能 crate 已接入产品路径 | [COMPONENT-ROADMAP](../../agent/COMPONENT-ROADMAP.md)、[技能方案](../2026-11-12/SKILL-LOADER-V1-PLAN.md) | `rust/crates/agent24-skill` | `b7eef06f2`（只核技能 crate 存在） |
| L4 运行时内核 | ✅；SPLIT-3 拆开 `lib.rs`（#1861） | [agent/architecture.md](../../agent/architecture.md) | `rust/crates/agent24-agent` | `b7eef06f2` |
| L4½ 领域 OS | ✅；完整性与验签沿用上期。**macOS OS 模块套 seatbelt**（#1863）；沙箱起不来则不启动；Linux 第一版不套，标未隔离（#1864） | [沙箱方案](../2026-11-14/MODULE-SANDBOX-PLAN.md) | `rust/crates/agent24-os-proto` | `b7eef06f2` |
| L5 agent24d | ✅。`rust/apps/agent24d/src` 全部 `.rs` **106,604** 行（128 个文件，含测试）。`server.rs` 518 行，只做模块入口。`serve()` 在 `server/serve.rs` 第 12–320 行。入站在 `inbound/` | [ADR-005](../../open-design-workspace/design/ADR-005-CAPABILITY-AUTHORITY.md) | `rust/apps/agent24d/src/server/serve.rs` | `b7eef06f2` |
| CORE-5 迁移保护 | 🟡 沿用上期（5f 在开关后；剩 5g/5h/5i） | [CORE-5 设计](../../agent/CORE-5-MIGRATION-GUARD-DESIGN.md) | `migration_coordinator.rs` | `f818b0bf` |
| L6 进程外领域 OS | ✅ 包规则沿用上期。Sin90 不在安装包里、侧边栏仍是占位：出处是预览版 20261117.1 的已知问题。按需安装见 §5，代码未合入 | [SPEC-ME3](../../specs/SPEC-ME3-OUT-OF-PROCESS.md) | `rust/apps/agent24-documents` | `b7eef06f2`（只核 release 正文） |
| L6 AgentEar | ✅ 沿用上期 | [A3-ATTACHED-MODULE](../../design/A3-ATTACHED-MODULE.md) | `agent24-attach`（S2-4b #1794） | `c396478c` |
| L6 Open Design | ✅ SidecarManager。首启就绪时限放宽（#1857，预览版正文写 15 秒到 60 秒） | [C8-SIDECAR-DESIGN](../2026-11-04/C8-SIDECAR-DESIGN.md) | `apps/desktop/src/main/sidecar-manager.ts` | `b7eef06f2`（只核 PR 标题与 release） |
| 组件机制 | ✅ C1–C9。第三方同意卡未做 | [ADR-038](../../adr/ADR-038-component-contract-v1.md)、[C9 方案](../2026-11-12/C9-REQUIRES-AUTHZ-PLAN.md) | `agent24-components`、`component_routes.rs` | `b7eef06f2` |
| 通讯（Hyphae） | ✅ 入站走 ADR-041a（X11）。锁仍只有 darwin-arm64、linux-x64，版本 `v0.30.1`。好友机制不在本仓排期 | [ADR-044](../../adr/ADR-044-communication.md) | `hyphae.lock.json` 的 `assets` | `b7eef06f2` |
| 记忆 | ✅ 关键词召回仍在。**向量混合召回已接入**（#1851、#1856）；嵌入不可用时退回关键词（release 20261114.1 正文） | [向量方案](../2026-11-13/VECTOR-RECALL-PLAN.md) | `rust/crates/agent24-memory` | `b7eef06f2` |
| 知识库 | 宿主凭据与授权已写成 ADR-KB-02（#1823）。Provider 是否接线，本期未重核代码 🟡 | [ADR-KB-02](../../knowledge/ADR-KB-02-HOST-AUTHORITY.md) | `agent24-knowledge` | `b7eef06f2`（只核 ADR） |
| 决策 / 硬件分档 | 沿用上期，未重核 | [PLAN-DECIDE](../../agent/PLAN-DECIDE.md) | `agent24-decide`、`agent24-hw` | `f818b0bf` |
| 外壳 | ✅。默认模型在设置里可选（#1879、#1881）。`ab/ui` 仍领先 main 15 个提交，其中 #1874–#1878 的合并提交不在 main | 上期 ADR-037 事实清单 | `apps/desktop` | `b7eef06f2` |

实测命令（在 `b7eef06f2`）：

```
find rust/apps/agent24d/src -name '*.rs' | xargs wc -l | tail -1   # 106604 total
find rust/apps/agent24d/src -name '*.rs' | wc -l                   # 128
wc -l rust/apps/agent24d/src/server.rs rust/apps/agent24d/src/server/serve.rs
# server.rs 518；serve.rs 320；pub async fn serve( 在 serve.rs:12，函数收到文件末行 320
```

上期快照时 agent24d 是 96,032 行、`server.rs` 9,345 行。那之后 S2 把 iDoris / Knowledge / attach 搬出，X11、技能、入站又在 agent24d 里加了文件。两个数字都含测试，口径相同，差出来的是这段历史，不是同一次瘦身的结果。上期补记里的 82,276 行（含 #1794）本期没有在那个提交上重测。

**OS 包与外仓。** S1-9 的规则没有改。Sin90、Cos72 的 2026-11-09 发布沿用上期，本期没有重跑 `gh release view`。Hyphae 锁文件本期读过：`assets` 只有 `darwin-arm64`、`linux-x64`，`version` 是 `v0.30.1`。

## 2. ADR 状态快照

状态列取自 `b7eef06f2` 的 [ADR 索引](../../adr/README.md)。索引写明共 66 条，下一个可用编号 **ADR-047**（索引第 7 行、第 184 行）。实现度只改本期有 PR 的行；其余沿用上期，「未核实」表示本期没重核代码。

| ADR | 索引状态（现） | 实现度 | 最近相关 PR | 下一步 |
|---|---|---|---|---|
| ADR-037 产品形态 | 已接受（决策 1–5；决策 6 待遥控设计） | 沿用上期；决策 6 仍未做（FU-125 写明遥控不做） | — | — |
| ADR-038 组件契约 v1 | 已接受（r5，2026-11-05） | C1–C9 完成。第三方同意卡未做（ADR-038 正文 C9 行，约第 375 行） | #1825 #1830 #1833 #1831 #1841 | 同意卡方案，见 §5 |
| ADR-039 概念模型 | 已接受 | 概念层 | — | — |
| ADR-040 技能契约 | 已接受（作为设计约束） | v1 已落地（加载、激活、注入、技能栏、CLI）。方案 §6 列出的市场、签名、主动技能仍不做 | #1827–#1842 中的 SL 片，见 MILESTONES | — |
| ADR-041a 入站事件与准入 | 已接受 | 入站名单、接受模式、微信只传输、Hyphae 拉取已落地。手机遥控与自动回发未做 | X11 各片，见 MILESTONES §2.4 | — |
| ADR-041b 工单 | 提议（沿用上期，本期未重读索引行） | 无。FU-125 写明本里程碑不做 | — | — |
| ADR-042 / ADR-043 | 沿用上期 | 未重核 | — | — |
| ADR-044 通讯 | 已接受（索引第 7 行把 ADR-044 列在已接受） | X7、X11 已落地。正文第 173 行仍写「预览版停发」，和本期两个 release 不一致，见 §4 | #1785 及 X11 | 停发那句要不要改，等 jason |
| ADR-045 路由命名 | 已接受 | 技能路由用了 `skills` 前缀（SL-3 #1832 标题与方案 §5）。门禁测试是否覆盖，未核实 | #1832 | — |
| ADR-046 iDoris 托管 | 已接受（2026-11-12，S0-8） | 记录 S2-2 已落地的托管。按内存推荐模型的接口不在本 ADR，等 iDoris #602 | #1823 #1790 | — |
| ADR-KB-02 知识库宿主 | 已接受（2026-11-12，S0-8） | 文档已合。代码是否与正文逐条一致，本期未重核 | #1823 | — |
| ADR-035 | 部分取代（ADR-046 取代交给 Electron main 的条款；stdin 交给 iDoris 仍有效） | 见 ADR-046 | #1823 | — |
| ADR-MEMORY-P1 等 | 已接受（设计，沿用上期） | 向量召回已接线。清除权、无痕等待办项本期未重核 | #1851 #1856 | — |
| ADR-KB-01a / 01b | 沿用上期 | 未重核 | — | — |
| ADR-032 / 036 | 已接受 | 未重核 | — | — |
| ADR-K1-01…04 | 已接受 | 未重核。沙箱第一版只管 OS 模块（沙箱方案 §5） | #1858 | — |

## 3. 在途方案（设计已入仓、代码未完或明确不做）

| 方案 | 设计文档 | 代码 | 等谁 |
|---|---|---|---|
| 模块沙箱 Linux、更多权限出口 | [MODULE-SANDBOX-PLAN](../2026-11-14/MODULE-SANDBOX-PLAN.md) §4 | SBX-1/1b/2 已合。SBX-3 暂不做。SBX-4 Landlock 未排 | 已拍板（方案 §5，2026-11-15） |
| 第三方模块同意卡 | C9 方案写明以后做；本仓还没有专文 | 未做。本地分支 `docs/third-party-consent-plan` 与 main 同 sha，无提交 | 方案本身还没写 |
| Sin90 按需安装（OSI-1） | 预览版 20261117.1 已知问题里写了下一版要一键安装。专文未在 main 上看到 | 未合入。见 §5 | 本地在做 |
| iDoris §2.9 模型推荐 | 不在本仓。iDoris #602 是规范修订，OPEN | Agent24 侧本期是手动选模型（#1879） | 等 iDoris #602 与 #600 |
| 好友机制 | [COMM-FRIENDS](../../design/COMM-FRIENDS.md) 仍在本仓 | 不在 Agent24 排期，归 Hyphae 仓库。Hyphae 仓进度未核实 | — |
| 决策 D1 接线、CORE-5 余项 | 上期计划文档 | 未重核，仍 BACKLOG | — |

上期表里的 C9、技能加载器、向量召回已经有实现，移出本表，明细在 MILESTONES。

## 4. 已知偏离（文档 vs 代码或 vs 发布）

| 文档位置 | 偏离 | 处理 |
|---|---|---|
| `docs/adr/ADR-044-communication.md` 第 173 行 | 仍写「预览版停发」。GitHub 已有 `v0.6.0-preview.20261114.1` 与 `v0.6.0-preview.20261117.1`（`gh release view`） | 本期只记录。谁批准恢复，仓内未找到书面裁决 |
| 技能方案 §5 | 写集成分支 `ab/skill`，task PR 基线是它。本期远端 `ab/` 列表里没有 `ab/skill`。SL 各片由 `feat/sl*` 直接合入 main | 已合入，不回溯。见 MILESTONES §2.6 |
| 上期尾巴 2 | `ab/comm`、`ab/decide` 要盘点存档。GitHub 上这两条分支现在 404 | 存档文本本期没找到，未核实 |
| 上期 PROGRESS §0 | 写「第 2 期尚未开始」。同文件 §5 和 MILESTONES 后来补记了 S2 已合 | 那些合并在 `c396478c` 之后，统计放本期 |
| Hyphae 锁平台 | 仍只有 darwin-arm64、linux-x64 | 继续 BACKLOG |
| 第三方内核能力 | C9 方案：同意卡落地前维持现状，不新增不收紧 | 一行以后再说，不在本期做 |
| 沙箱方案 §「以后再说」 | 只读且超过 PATH_MAX 深度的临时目录树不处理；mach 服务面未测，不宣称模块影响不到沙箱外 | 已写在方案里，不单开实现 |

**本期已对齐的（上期还标着未做）**

| 项 | 结果 |
|---|---|
| C9 | DONE，见 §0 |
| X11 / X7 | DONE。入站适配在 agent24d `inbound/`，不并入 comm |
| S0-8 | DONE（#1823） |
| 技能加载器「没开工」 | v1 已合 |
| 记忆向量「无消费者」 | 已接入对话循环 |
| ADR 索引 ADR-038 草案 | 上期已改成已接受；本期索引仍是已接受（r5） |

`decision.md` 与 CORE-5 设计的旧矛盾、CLI 与桌面的四项例外，沿用上期，本期未重新打开正文核对。

## 5. 在做与下一步

没有新的未决拍板要记在这里。沙箱三个问题已在方案 §5 写明（2026-11-15）。

| 事项 | 状态 | 出处 |
|---|---|---|
| OSI-1 Sin90 按需安装 | DOING。本地 worktree `Agent24/.wt/sin90`，分支 `feat/sin90-dynamic-install`，与 `origin/main` 同 sha，领先 0。工作区有未提交改动（桌面 IPC，以及未跟踪的 `os-package-*` 文件）。无远端分支，无 open PR。改动内容不当作已完成 | `git worktree list`、`git -C .wt/sin90 status`、`git ls-remote`（空）、`gh pr list --state open`（0） |
| 第三方模块同意卡 | 方案未写。本地分支 `docs/third-party-consent-plan` 与 main 同 sha，工作区干净，无远端 | 同上；C9 方案第 10–11 行 |
| followups 分批清理 | 未开工。本地分支 `docs/followups-triage-2026-11-17` 与 main 同 sha，工作区干净。open 的 `FU-` 有 85 条 | `grep -cE '^- \[ \] FU-' docs/agent/followups.md` |
| iDoris §2.9 模型推荐 | BLOCKED。本仓手动选模型已合（#1879、#1881）。推荐接口等 iDoris #602（OPEN，规范 §2.9，写明实现发布前 wire 1.1.0 仍是当前契约）。iDoris #600（OPEN）是计算服务第一版，任务说明要求一起等；它和推荐接口的调用关系，本期没读完实现，未核实 | `gh pr view 602/600 --repo iDoris-ai/iDoris`；预览版 20261117.1 已知问题 |
| S0-8 | 计划里的三项已由 #1823 合入。另有本地分支 `docs/s0-8-adr-cleanup`，与 main 同 sha，工作区干净，无远端。清理什么，分支上还没有文字 | #1823；`git -C .wt/s08 status` |
| 好友机制 | 不在 Agent24 排期，归 Hyphae 仓库。Hyphae 仓未核实 | 本跟踪任务说明（2026-11-17） |
| `ab/ui` 上未进 main 的 5 个 PR | #1874、#1875、#1876、#1877、#1878。合并提交不是 `origin/main` 的祖先。#1877 是设置里填本地 OpenAI 兼容服务，#1878 是对话里问这份文档 | `git merge-base --is-ancestor`，见 MILESTONES §1 |
