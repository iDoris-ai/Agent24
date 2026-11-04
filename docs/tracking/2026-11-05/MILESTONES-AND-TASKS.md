# 里程碑与任务（2026-11-05）

索引：[架构总入口](../../architecture/README.md)・同期另两份：[ARCH-AND-PLAN](ARCH-AND-PLAN.md)、[PROGRESS](PROGRESS.md)

| 项 | 内容 |
|---|---|
| 快照日期 | 2026-11-05（首期） |
| HEAD sha | `f818b0bfd7f53cdf15fd0265bbb56f4bb8cb9e8b`（`origin/main`，Merge #1591） |
| 数据来源 | 1. [架构优化方案 §4 分片计划](../2026-11-04/ARCH-OPTIMIZATION-PLAN.md) 与 jason 2026-11-04 拍板；2. 旧台账 `docs/agent/tasks.md`、`docs/agent/COMPONENT-ROADMAP.md`（任务定义仍以各 PLAN 为准）；3. 三份梳理报告 R1 / R2 / R3 的 §4 对账（`docs/architecture/review-2026-11-03/`，在各自分支上）；4. `git` 的分支状态（下文命令） |
| 生成方式 | 手工整理。§1 的分支领先 / 落后数用文末命令生成。状态按 2026-11-05 的实际情况填 |
| 状态词 | `READY` 可开工　`DOING` 在做　`IN-PR(#)` 已开 PR 待评审　`DONE(#)` 已合入　`BLOCKED` 被阻塞　`PAUSED` 暂停　`CANCELLED` 取消或作废　`BACKLOG` 已规划未排 |
| 节奏 | 每两周或每次 release 出一期（Q11）。拍板类变化允许在期内直接改 §0 和 §3，并写日期和出处 |

## 0. 当前主干与并行线（jason 最近裁决）

| 日期 | 裁决 | 出处 |
|---|---|---|
| 2026-11-04 | **预览版停发**，主线是组件整合与统一：架构优化方案分片落地 + ADR-038 收尾 | jason 拍板；[方案文首进度段](../2026-11-04/ARCH-OPTIMIZATION-PLAN.md) |
| 2026-11-04 | **Q1 = (b)**：Open Design 改由 `SidecarManager` 托管，`agent24-sidecar-host` 冻结。#1580、#1584 已合，ADR-038 C1–C8 全部完成 | 方案 Q1；#1580、#1584 |
| 2026-11-04 | **Q3 = (B)**：agent24d 下沉三块（iDoris、Knowledge、新建 attach），鉴权留核心，不抽 `agent24-auth`，原 S2-5 取消；三块仍编进同一个 agent24d 进程 | 方案 Q3 |
| 2026-11-04 | **Q12 通讯**：Hyphae 按需下载；Nostr 身份以 `<data>/comm/hyphae-home` 钥匙库为准（nostr-bridge 以后并入 comm，即 Q6 选 (c)）；所有外部触发 agent 的入口统一走 ADR-041 准入关卡（含 Nostr 入站、语音输入、远程 @agent、手机遥控） | 方案 Q12 |
| 2026-11-04 | **Q15**：OS 包（Sin90、Cos72 等）要有文件清单和哈希，哈希不对即拒绝加载，不做「先只记录」的过渡 | 方案 Q15 |
| 2026-11-04 | 其余 Q2、Q4–Q11、Q13、Q14、Q16、Q17 全部按方案 §5「推荐」执行 | 方案 §5 |
| 2026-10-03 | Open Design 线（OD-M11 / OD-M12 / TPI-W / OC）在 M10 告一阶段，`PAUSED` | `docs/agent/tasks.md`；[COMPONENT-ROADMAP §3](../../agent/COMPONENT-ROADMAP.md) |

## 1. 里程碑总表

按方向列。「集成分支」的领先 / 落后数是 2026-11-05 对 `origin/main` 的实测（`git rev-list --left-right --count origin/main...origin/ab/<方向>`，左数是 main 领先、右数是该分支领先）。

| 方向 | 里程碑 | 集成分支 | 目标 | 状态 | 依赖 | 计划文档 |
|---|---|---|---|---|---|---|
| 架构优化（本期总纲） | 第 0 期：文档与 ADR 收口 | `docs/p0-*`（逐片分支） | 把文档与 ADR 订正到与代码一致，补通讯 ADR、路由约定、跟踪文档 | DOING（7 片） | 无 | [方案 §4.1](../2026-11-04/ARCH-OPTIMIZATION-PLAN.md) |
| 架构优化 | 第 1 期：低风险结构调整 | 待定 | OpenAPI 漂移、签名单源、`daemon.json` 单一解析器、状态映射、OD pin 单源、休眠代码、OS 包完整性 | READY（等第 0 期腾出评审） | 无硬依赖 | 方案 §4.2 |
| 架构优化 | 第 2 期：agent24d 瘦身 | 待定 | 拆 `server.rs`，下沉 iDoris / Knowledge / attach，进程组终止原语 | BACKLOG | S0-5 先把新边界写进文档 | 方案 §4.3 |
| 组件统一 | ADR-038 / ADR-039 收尾 | `main` | 组件契约、清单、统一状态面；运行期契约检查接线 | C1–C8 DONE；ADR 订正 DOING（S0-2） | S0-2 | [ADR-038](../../adr/ADR-038-component-contract-v1.md)、[COMPONENT-ROADMAP](../../agent/COMPONENT-ROADMAP.md) |
| 通讯（Hyphae） | COMM 系列、好友机制 | `ab/comm`（main 领先 2487，该分支领先 11） | 常规通信先做；好友机制之后；ADR-044 | DOING；ADR-044 在 S0-4 | Q12 已定 | [COMM-HYPHAE](../../design/COMM-HYPHAE.md)、[COMM-FRIENDS](../../design/COMM-FRIENDS.md) |
| 决策模型 | D0–D3 | `ab/decide`（main 领先 2462，分支领先 25） | 决策服务 D0 契约 / 规则后端已有，D1 接线未排期 | 未核实当前排期 | — | [PLAN-DECIDE](../../agent/PLAN-DECIDE.md) |
| 记忆 | P1 | `ab/memory-p1`（本次未列出远端分支，**未核实**） | 个人记忆生命周期与清除权 | 未核实；MEMORY-P1 设计 ADR 拟接受（S0-1） | S0-1 | [PLAN-MEMORY-P1](../../agent/PLAN-MEMORY-P1.md) |
| 内核 | K1 / CORE-2 / CORE-5 | `ab/kernel`（main 领先 457，分支领先 1） | CORE-5 剩 5g 外壳识别、5h 安装器回退、5i 授权 retry；CORE-2 余项与 AgentEar 纵切 | 在 jason 自己的线上 | — | [CORE-5 设计](../../agent/CORE-5-MIGRATION-GUARD-DESIGN.md)、[CORE-2 AgentEar 纵切](../../agent/CORE-2-AGENTEAR-SLICE.md) |
| Documenting | T1 | `ab/documenting`（main 领先 138，分支领先 0） | David 负责；已集成合并 #1591 | 已合入 main | — | `docs/documenting/` |
| 知识库 | WeKnora | `ab/weknora`（main 领先 135，分支领先 0） | David 负责；已集成合并 #1589 | 已合入 main | — | [ADR-KB-01](../../knowledge/ADR-KB-01-SINGLE-CONVERSATION.md) |
| 其他集成分支 | maint / office-assistant | `ab/maint`（分支领先 2）、`ab/office-assistant`（分支领先 27） | 本期未核实内容 | 未核实 | — | — |

## 2. 按方向的任务表

### 2.1 架构优化方案（方案 §4 全部分片）

派给列：**Remote** 云端会话（文档、机械重构）；**Grok** / **Codex** 本机写码。2026-11-04 起「拍板」类前置都已按推荐定下，所以下表不再有 `BLOCKED`。

#### 第 0 期：文档与 ADR 收口（不碰代码）

| 任务 id | 标题 | 状态 | 负责 | PR / 分支 | 验收（摘自方案 §4.1） |
|---|---|---|---|---|---|
| S0-1 | ADR 状态一次性订正 | DOING | Remote | 待开 | 索引每行状态与文首一致；`CORE-5-…:200` 与 `decision.md:1508` 不再矛盾；每个「取代」写明取代者。前置 Q8、Q9 已定 |
| S0-2 | ADR-038 r5 订正稿 | DOING | Remote | 待开 | R3 §1.A.1.5 的 16 条逐条有处置；`registry_lists_every_capability_id_the_components_emit` 仍绿。C8 按 Q1 (b) 改写 |
| S0-3 | 编号别名 | DOING | Remote | 待开 | 不改任何文件名；`git grep 'ADR-00[1-6]-[A-Z]'` 链接数不变（23）；索引写明唯一发号处、下一号 ADR-044 |
| S0-4 | 通讯 ADR-044 草案 | DOING | Remote | 待开 | 每条决策有出处 file:line；X1 / X7 / X11 有结论；与 ADR-041 边界写清。前置 Q12 已定、S0-3 |
| S0-5 | ARCHITECTURE-LAYERS 重新生成 | DOING（已推送，PR 待开） | Remote（本机子代理） | 分支 `docs/p0-layers-regen` | 依赖表与 `cargo metadata --no-deps` 输出逐行一致，脚本 `scripts/gen-layer-deps.py` |
| S0-6 | 三份跟踪文档首期 | DOING（本文件所属） | Remote | 分支 `docs/p0-tracking-first-issue` | 三份都带数据来源 / 生成方式 / HEAD sha；PROGRESS §1 由 `git log --merges` 生成；方案 §4 的分片进本表 |
| S0-7 | 路由命名约定（ADR-045） | DOING | Remote | 待开 | 约定可被一个门禁测试检查（新增路径不得含下划线、参数名在白名单内）。前置 Q4 已定：只约束新路由 |
| S0-8（可后置） | KB-HOST-01 落成 ADR-KB-02；iDoris 托管 ADR（O3）；ADR-035 标部分取代 | BACKLOG | Remote | — | 仓内有凭据交接与授权归属的正文。前置 S0-3 |

#### 第 1 期：低风险结构调整

| 任务 id | 标题 | 状态 | 负责 | PR | 验收 |
|---|---|---|---|---|---|
| S1-1 | OpenAPI 漂移收口 | READY | Grok | — | 故意删掉一个方法时门禁变红；api-client 生成物无其他差异。sin90 10 个端点按 Q4 (iii)：留本仓并标「外部契约，实现以 Sin90 为准」 |
| S1-2 | 签名要求与 Team ID 单源 | READY | Grok（Opus 评审） | — | 非 Developer ID 证书签名的二进制 enroll 失败；改任一处 Team ID 一致性测试变红。dev 构建按 Q2 (a)：只认 `test-enrollment` feature |
| S1-3 | TS `daemon.json` 单一解析器 | READY | Grok（Opus 评审鉴权语义） | — | 共享 fixture 覆盖 legacy / capabilities / 未知 `auth_mode` / 缺 token；bridge 在 capabilities 模式下明确报错而非发空 token |
| S1-4 | 状态映射与契约阈值残项 | READY | Grok | — | 1.0.x iDoris 在托管与非托管两条路径结论一致；`/components` 不再出现 `failed/starting`。Hyphae `locked` 按 Q14 (a) 映射 `stopped` + reason `locked` |
| S1-5 | OD pin 单一来源 | READY | Grok | — | 改 pin 只需改一个文件；漏改时测试变红 |
| S1-6 | C8 收尾（推荐方案） | CANCELLED：Q1 改为 (b)，由 #1580 取代 | — | #1580 DONE | — |
| S1-7 | 休眠代码收窄 | READY | Grok | — | `dead_code` 告警重新有意义；`agent24-sidecar-host` 按 Q1 冻结。store 休眠链先和 workspace G 系列计划对齐，不删 |
| S1-8（可选） | `os-*` 改名 `agent24-sys-*` | CANCELLED：Q7 选 (c) 不改名，文档里写清（S0-5 已做） | — | — | — |
| 未编号 | OS 包完整性（Q15） | BACKLOG（方案排在第 1 期末，规模 M） | Grok（Opus 评审，安全类） | — | OS 包有文件清单与哈希；哈希不对即拒绝加载，无过渡期 |

#### 第 2 期：agent24d 瘦身（Q3 = (B)）

前置：S0-5 完成。每片先移动、后修行为，同一 PR 不混。

| 任务 id | 标题 | 状态 | 负责 | PR | 验收 |
|---|---|---|---|---|---|
| S2-1 | 拆 `server.rs` | BACKLOG | Remote + Grok 收尾 | — | `serve()` < 300 行；启动黑盒测试全绿；源码断言测试同步更新 |
| S2-2 | iDoris 托管下沉 `agent24-idoris` | BACKLOG | Grok | — | agent24d 减约 2,500 行；idoris 自身测试覆盖原用例 |
| S2-3 | Knowledge 宿主下沉 `agent24-knowledge` | BACKLOG | Grok（Opus 评审；凭据类须等 PR-Daemon APPROVE） | — | agent24d 减约 1,800 行；C5 探测测试原样通过 |
| S2-4 | attach 新建 `agent24-attach` | BACKLOG | Grok | — | agent24d 减约 1,500 行；C4 用例原样通过 |
| S2-5 | 鉴权抽 `agent24-auth` | CANCELLED：Q3 = (B)，鉴权留 agent24d 核心 | — | — | — |
| S2-6 | 进程组终止原语统一 | BACKLOG（#1580 已合，前置已满足；Q13 选 (b)） | Grok | — | 同一组回收 / 超时 / 僵尸测试跑在共用实现上 |
| S2-7 | CLI 补齐（Q5 = (b)） | BACKLOG | Grok | — | 先补 Comm 私钥备份恢复和 Knowledge 绑定，release 用例可走 CLI；OD 下载与 presence 留桌面并在 `AGENTS.md` 写明例外 |

### 2.2 组件统一（ADR-038）

| 任务 id | 标题 | 状态 | PR | 验收 / 备注 |
|---|---|---|---|---|
| C1（C1a / C1b / C1b'） | ComponentLock schema 2、通用下载器与签名验证、依赖反转 | DONE | #1539 | R3 §4.1 |
| C2 | 运行期契约检查 | BACKLOG | — | Agent24 侧没有向 Hyphae 做 `hyphae-cli` 契约握手，只有 iDoris 的 `check_contract` |
| C3a / C3b | `/api/v1/components` 统一状态面；OD 状态由桌面上报 | DONE | #1545、#1549 | |
| C4–C7 | AgentEar、Knowledge、Documents、领域 OS / MCP 清单 | DONE（按 jason 2026-11-04 的结论 C1–C8 全部完成） | 见 ADR-038 §4 | 逐项核对在 S0-2 |
| C8 | Open Design 改由 `SidecarManager` 托管 | DONE | #1580、#1584 | Q1 = (b) |
| — | Hyphae 锁只有 darwin-arm64、linux-x64 两个平台 | BACKLOG | — | `rust/crates/agent24-comm/hyphae.lock.json` |

### 2.3 内核（CORE-2 / CORE-5）

| 任务 id | 标题 | 状态 | PR | 备注 |
|---|---|---|---|---|
| CORE-5f-1 / 5f-2 / 5f-3 | 停靠类型、router、启动接线 | DONE（均在 `ENABLED=false` 后面） | 70f1e9a1、#1528、#1540 | |
| CORE-5g / 5h / 5i | 外壳识别、安装器回退、授权 retry | BACKLOG | — | 开片前对照遗留硬要求（5e-2 掉电模型门、5f `rollback_failed` phase、5g 的 `spawn_blocking`/锁/路径/同文件系统） |
| CORE-2 余项、AgentEar 纵切 | | BACKLOG | — | [CORE-2-AGENTEAR-SLICE](../../agent/CORE-2-AGENTEAR-SLICE.md) |

### 2.4 通讯

| 任务 id | 标题 | 状态 | PR | 备注 |
|---|---|---|---|---|
| ADR-044 | 通讯 ADR 草案 | DOING（S0-4） | — | |
| X11 落地 | 外部入口统一走 ADR-041 准入 | BACKLOG | — | 先要 ADR-041 拆 041a / 041b 并接受（S0-1） |
| X7 落地 | nostr-bridge 不再是身份来源，并入 comm | BACKLOG | — | Q6 = (c)；在此之前产品构建禁止 bridge 用 legacy 全权 token |
| COMM-FRIENDS | 好友机制 | BACKLOG | — | 先做常规通信，之后设计；好友不等于能指挥 agent |

### 2.5 记忆与决策

| 任务 id | 标题 | 状态 | 备注 |
|---|---|---|---|
| M1-T06b、M1-T07b | `vector.rs` 等未被 agent 与 daemon 调用 | BACKLOG | R3 §4.3；`docs/agent/tasks.md` 台账 |
| D1 | 决策日志接线（`DecisionLog` 实现、召回门控） | BACKLOG | 无实现，agent loop 无调用点 |
| 其余 | 记忆 P1（P1-02c 等）、决策 D0-6 | 未核实 | 旧台账 `docs/agent/tasks.md` 已不更新，下期核对后补 |

## 3. 暂停与放弃

| 项 | 何时、谁裁决 | 恢复条件 |
|---|---|---|
| 预览版发布 | 2026-11-04 jason | 组件整合与统一告一段落后再定 |
| Open Design 线 OD-M11 / OD-M12 / TPI-W / OC | 2026-10-03 jason（`PAUSED`） | 路由级资源授权等 M10 遗留缺口（[PLAN-OD-NEXT](../../agent/PLAN-OD-NEXT.md) §0）有人排期时，从 OD-M11 起步 |
| `agent24-sidecar-host`（Rust） | 2026-11-04 Q1 = (b)，冻结，只留 Windows CI 编译 | Windows 支持被排上日程 |
| S1-6、S2-5、S1-8 | 2026-11-04（Q1 = (b)；Q3 = (B)；Q7 = (c)） | 见各行 |
| 移动端 | 暂不做，将来仅遥控 | ADR-037 决策 6 的遥控设计 |
| 旧 `docs/specs/TASKS.md`、`docs/ROADMAP.md`、`docs/PLAN.md` | 已作废（各文件文头） | 不恢复 |

## 4. 跟进项汇总

明细仍在 [followups.md](../../agent/followups.md)（append-only，规则不变）。计数命令：`grep -c '^- \[ \]' docs/agent/followups.md`。

| 来源 | open | done | 本期新增 |
|---|---|---|---|
| `docs/agent/followups.md`（最后提交 `d4053f127`，2026-11-01） | 82 | 39 | 首期，无上期可比 |

## 附：本表用到的命令

```
git rev-list --left-right --count origin/main...origin/ab/<方向>
grep -c '^- \[ \]' docs/agent/followups.md ; grep -c '^- \[x\]' docs/agent/followups.md
```
