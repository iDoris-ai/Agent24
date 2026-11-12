# 里程碑与任务（2026-11-10）

索引：[架构总入口](../../architecture/README.md)・同期另两份：[ARCH-AND-PLAN](ARCH-AND-PLAN.md)、[PROGRESS](PROGRESS.md)

| 项 | 内容 |
|---|---|
| 快照日期 | 2026-11-10（第二期） |
| HEAD sha | `c396478c9f8797c339096dbcdbf4d01b242d756f`（`origin/main`，Merge #1780） |
| 上一期 | [2026-11-05](../2026-11-05/MILESTONES-AND-TASKS.md)，HEAD `f818b0bf` |
| 数据来源 | 1. [架构优化方案 §4 分片计划](../2026-11-04/ARCH-OPTIMIZATION-PLAN.md) 与 jason 拍板；2. [S1-9 设计](../2026-11-04/S1-9-OS-PACKAGE-INTEGRITY-DESIGN.md)（#1611、r2 #1723）；3. `gh pr view <n> --json title,mergedAt,state` 逐个核对每个 PR 的合并状态；4. 本地 git 的分支状态与 tag（下文命令）；5. `docs/agent/followups.md` |
| 生成方式 | 手工整理。§1 的分支领先 / 落后数用文末命令重新实测；§4 的计数用文末命令生成，「按来源分类」是人工只看标题行判断的，不是脚本。状态按 2026-11-10 的实际情况填 |
| 状态词 | `READY` 可开工　`DOING` 在做　`IN-PR(#)` 已开 PR 待评审　`DONE(#)` 已合入　`BLOCKED` 被阻塞　`PAUSED` 暂停　`CANCELLED` 取消或作废　`BACKLOG` 已规划未排 |
| 节奏 | 每两周或每次 release 出一期（Q11）。拍板类变化允许在期内直接改 §0 和 §3，并写日期和出处 |

## 0. 当前主干与并行线（jason 最近裁决）

本期新增在最前面；2026-11-04 及更早的裁决沿用首期，不重复，见[首期 §0](../2026-11-05/MILESTONES-AND-TASKS.md)。

| 日期 | 裁决 | 出处 |
|---|---|---|
| 2026-11-10 | **次序**：先清尾巴再开第 2 期。尾巴三件：(1) 本跟踪文档第二期；(2) 两条旧集成分支 `ab/comm`、`ab/decide` 盘点存档；(3) 通讯 X11（外部入口统一走 ADR-041a 准入）与 X7（身份归 comm（已定），入站适配不并入 comm（落在 agent24d inbound 模块））。然后第 2 期从 S2-1 拆 `server.rs` 开始 | jason 2026-11-10；X7 归宿 2026-11-12 回写 |
| 2026-11-10 | **不在本轮**：好友机制（定为之后）、C9（`requires` 接入授权）；`docs/agent/followups.md` 的 82 条只做分类，不逐条修 | jason 2026-11-10 |
| 2026-11-10 | 闭环计划不再逐项问 jason | jason 2026-11-10 |
| 2026-11-10 | **macOS 签名信任**：默认只信任本方 Team，可 `trust-team` 增加；`os install` 离线验签，**不在线查公证**（公证不由 Agent24 检查）。落地为 S1-9 PR-3d（#1780） | jason 2026-11-10；#1780；S1-9 设计 §5 r3 |
| 2026-11-09 | **N3**：OS 包安装包里的可执行文件只允许签名的本平台原生程序；解释器类模块（Node 等）走 A3 attach，不进安装包 | jason 2026-11-09；#1737、#1733 |
| 2026-11-09 | **SDK 0.2.0**：模块在握手前先对磁盘清单自检（N6） | jason 2026-11-09；#1730、tag `agent24-os-sdk-v0.2.0` |
| 2026-11-09 | **Cos72 是独立项目**，Agent24 只做基础对齐（SDK 0.2.0、签名公证、完整性清单），不为它改架构 | jason 2026-11-09；Cos72 #14、#15、#16 |
| 2026-11-09 | **分工**：写码用 Codex / Grok；agy 只做调研；最终验收 Opus + Codex | jason 2026-11-09 / 11-10 |
| 2026-11-04 | 预览版停发；Q1 = (b) SidecarManager；Q3 = (B) 下沉三块、鉴权留核心；Q12 通讯；Q15 OS 包完整性；其余 Q 按推荐 | 首期 §0 |
| 2026-10-03 | Open Design 线（OD-M11 / OD-M12 / TPI-W / OC）在 M10 告一阶段，`PAUSED` | `docs/agent/tasks.md` |

## 1. 里程碑总表

「集成分支」的领先 / 落后数是 2026-11-10 对 `origin/main`（`c396478c`）的实测：`git rev-list --left-right --count origin/main...origin/ab/<方向>`，左数是 main 领先、右数是该分支领先。括号内为首期同命令的数。

| 方向 | 里程碑 | 集成分支 | 目标 | 状态 | 依赖 | 计划文档 |
|---|---|---|---|---|---|---|
| 架构优化（总纲） | 第 0 期：文档与 ADR 收口 | `docs/p0-*`（已合并） | 订正文档与 ADR，补通讯 ADR、路由约定、跟踪文档 | S0-1～S0-7 DONE；S0-8 BACKLOG | 无 | [方案 §4.1](../2026-11-04/ARCH-OPTIMIZATION-PLAN.md) |
| 架构优化 | 第 1 期：低风险结构调整 | `main`（逐片分支，已合并） | OpenAPI、签名单源、`daemon.json`、状态映射、OD pin、休眠代码、OS 包完整性 | **全部 DONE**（S1-6、S1-8 CANCELLED） | 无 | 方案 §4.2、S1-9 设计 |
| 架构优化 | 第 2 期：agent24d 瘦身 | 待定 | 拆 `server.rs`，下沉 iDoris / Knowledge / attach，进程组终止原语，CLI 补齐 | 2026-11-10 推进：S2-1/2/3/4a 已合，S2-6/S2-7 按拍板降级为文档已合，S2-4b #1794 已合；agent24d 96,032 → 82,276 行（含已合的 #1794） | S0-5 已合 | 方案 §4.3 |
| 组件统一 | ADR-038 / ADR-039 收尾 | `main` | 组件契约、清单、统一状态面 | C1–C8 DONE（含 C2，见 §2.2）；C9 未排期 | — | [ADR-038](../../adr/ADR-038-component-contract-v1.md) |
| 通讯（Hyphae） | COMM 系列、X11、X7、好友机制 | `ab/comm`（**2939 落后**（2487），该分支领先 11（11）；最后一次提交 2026-10-21） | 尾巴：X11、X7；好友机制之后 | `ab/comm` 盘点存档：未做（尾巴 2）；ADR-044 DONE | Q12 已定 | [ADR-044](../../adr/ADR-044-communication.md)、[COMM-HYPHAE](../../design/COMM-HYPHAE.md) |
| 决策模型 | D0–D3 | `ab/decide`（**2914 落后**（2462），分支领先 25（25）） | D0 契约 / 规则后端已有，D1 接线未排期 | `ab/decide` 盘点存档：未做（尾巴 2）；当前排期未核实 | — | [PLAN-DECIDE](../../agent/PLAN-DECIDE.md) |
| 记忆 | P1 | `ab/memory-p1`：本期远端**无此分支**（首期同样未列出），**未核实**是否已合并删除 | 个人记忆生命周期与清除权 | 未核实 | — | [PLAN-MEMORY-P1](../../agent/PLAN-MEMORY-P1.md) |
| 内核 | K1 / CORE-2 / CORE-5 | `ab/kernel`（main 领先 909（457），分支领先 1（1）） | CORE-5 剩 5g/5h/5i；CORE-2 余项与 AgentEar 纵切 | 在 jason 自己的线上 | — | [CORE-5 设计](../../agent/CORE-5-MIGRATION-GUARD-DESIGN.md) |
| Documenting | T1 | `ab/documenting`（453（138），分支领先 0（0）） | David 负责 | 已合入 main | — | `docs/documenting/` |
| 知识库 | WeKnora | `ab/weknora`（**main 领先 55，分支领先 49**；首期 135 / 0） | David 负责。本期集成合并 #1620、#1663、#1665、#1693、#1697 | 分支又有 49 个未进 main 的提交，内容未核实 | — | [ADR-KB-01](../../knowledge/ADR-KB-01-SINGLE-CONVERSATION.md) |
| UI | ab/ui（M4、M5 B1） | `ab/ui`（main 领先 61，分支领先 82）；`ab/ui-b3-daemon-test`（61 / 33）、`ab/ui-b5a-upgrade`（61 / 35）、`ab/ui-b5b-crash-loop`（61 / 36） | 本期集成合并 #1637、#1658、#1716、#1728（首期没有这条线） | 在 jason / 子代理线上，细节未核实 | — | — |
| 其他集成分支 | maint / office-assistant / 其余 | `ab/maint`（2891 / 2）、`ab/office-assistant`（2457 / 27）、`ab/kernel-22-egress-exec-memory`（2896 / 1）、`ab/weknora-03a-state-machine`（2419 / 9）、`ab/weknora-03b-ui-bridge`（2419 / 6）、`ab/weknora-03c-ops`（2419 / 15）、`ab/weknora-1670-h1-submit`（55 / 39）、`-h1-submit-2`（55 / 57）、`-h1a-md`（55 / 33）、`-h2-ui-2`（55 / 62）、`ab/weknora-1715-kb3b`（55 / 35）。首期列的 `ab/documenting-adr039-1159-rulings` 本期远端已无 | 内容未逐个核实 | 未核实 | — | — |

## 2. 按方向的任务表

### 2.1 架构优化方案（方案 §4 全部分片）

派给列沿用首期。下表 PR 号都用 `gh pr view` 核对过标题与合并状态（均为 MERGED）。

#### 第 0 期：文档与 ADR 收口（不碰代码）

| 任务 id | 标题 | 状态 | PR |
|---|---|---|---|
| S0-1 | ADR 状态一次性订正 | DONE(#1596)；ADR-038 索引遗漏本 PR 补订正 | #1596（与 S0-3 同 PR） |
| S0-2 | ADR-038 r5 订正稿 | DONE(#1592) | #1592 |
| S0-3 | 编号别名 | DONE(#1596) | #1596 |
| S0-4 | 通讯 ADR-044 | DONE(#1593)；#1601 改为已接受 | #1593、#1601 |
| S0-5 | ARCHITECTURE-LAYERS 重新生成 | DONE(#1594) | #1594 |
| S0-6 | 三份跟踪文档首期 | DONE(#1595) | #1595 |
| S0-7 | 路由命名约定（ADR-045） | DONE(#1600)；#1601 改为已接受 | #1600、#1601 |
| S0-8（可后置） | KB-HOST-01 落成 ADR-KB-02；iDoris 托管 ADR（O3）；ADR-035 标部分取代 | BACKLOG | — |

#### 第 1 期：低风险结构调整

| 任务 id | 标题 | 状态 | PR |
|---|---|---|---|
| S1-1 | OpenAPI 漂移收口 | DONE(#1606) | #1606 |
| S1-2 | 签名要求与 Team ID 单源 | DONE(#1608) | #1608 |
| S1-3 | TS `daemon.json` 单一解析器 | DONE(#1609) | #1609 |
| S1-4 | 状态映射与契约阈值残项 | DONE(#1605) | #1605 |
| S1-5 | OD pin 单一来源 | DONE(#1602) | #1602 |
| S1-6 | C8 收尾 | CANCELLED：Q1 = (b)，由 #1580 取代 | — |
| S1-7 | 休眠代码收窄 | DONE(#1612)；CI 补编休眠 feature #1613 | #1612、#1613 |
| S1-8 | `os-*` 改名 | CANCELLED：Q7 = (c) | — |
| S1-9 | OS 包完整性（首期「未编号」Q15，现编号 S1-9） | DONE（子任务见下表） | 见下 |

S1-9 子任务（设计文档：[S1-9-OS-PACKAGE-INTEGRITY-DESIGN](../2026-11-04/S1-9-OS-PACKAGE-INTEGRITY-DESIGN.md)）：

| 子任务 | 内容 | 状态 | PR |
|---|---|---|---|
| 设计 | 方案与上线顺序；r2 加 N3/N4/N6 前置、模块读磁盘清单 | DONE | #1611、r2 #1723 |
| PR-1 | 共享文件校验、生成工具、旧包兼容读取 | DONE | #1614 |
| N6 | os-sdk 0.2.0 握手前清单自检（tag `agent24-os-sdk-v0.2.0`，2026-11-09） | DONE | #1730 |
| N3 | 安装包可执行文件须为本平台原生程序；Node 参考模块改走 A3 attach | DONE | #1737、#1733 |
| N4 | 完整性校验随 daemon 关闭可取消 | DONE | #1739 |
| 发布脚本 | `package-integrity generate --arch` / `verify --arch`（跨架构打包） | DONE | #1750、#1751 |
| PR-3a | 测试包改为原生可执行并封清单（强制仍关闭） | DONE | #1754 |
| PR-3b | 随包 documents 模块安装时生成清单 | DONE | #1753 |
| PR-3c | 清单改为必需，五类拒绝原因，桌面提示 | DONE | #1756 |
| PR-3d | macOS 验签（受信 Team ID，可 trust-team）；`os install` 离线 | DONE | #1780 |
| 外仓 Sin90 | #80（签名公证 + 清单）、#81（SDK 0.2.0）、#82（v0.5.2）、#83（`--arch`） | DONE | iDoris-ai/Sin90 #80 #81 #82 #83 |
| 外仓 Cos72 | #14（对齐 S1-9）、#15（0.1.2）、#16（`--arch`）；Cos72 是独立项目，只做基础对齐 | DONE | MushroomDAO/Cos72 #14 #15 #16 |

S1-9 期间的 CI 与测试稳定性：#1749（Linux Rust 作业超时 20 → 30 分钟）、#1755（macOS 30 → 45 分钟）；#1615（idoris keys 夹具遇 ETXTBSY 重试）、#1607（idoris keys 与 idoris_host 偶发超时）。

#### 第 2 期：agent24d 瘦身（Q3 = (B)）

前置：S0-5 已合。每片先移动、后修行为，同一 PR 不混。**S2-4 排在 S2-1、S2-2 之后**。**2026-11-10 状态**：S2-1 #1791、S2-2 #1790、S2-3 #1792、S2-4a #1793 已合；S2-4b #1794 已合；S2-6 #1788、S2-7 #1789 按 jason 拍板改为文档。本机 Codex 额度到 11-15，这几片只有独立 Opus 本地评审 + PR-Daemon，Codex 补审记为 FU-113。实测基线（`c396478c`）：`server.rs` 9,345 行，`serve()` 起于 1978 行、止于 3513 行（约 1,536 行），agent24d 全部 `.rs` 共 96,032 行（含内联测试）。S2-2 / S2-3 / S2-4 验收里「减约 N 行」是方案 §4.3 的估计，口径与上面不同，开工时重新量。

| 任务 id | 标题 | 状态 | 负责 | 验收 |
|---|---|---|---|---|
| S2-1 | 拆 `server.rs` | DONE（#1791，`d76189a6`） | Grok + Opus 收尾 | `serve()` 1,536 → 267 行；启动黑盒测试全绿（含 `test-enrollment` 的 docked_startup）；源码断言测试收紧到等价或更严，带变异对照 |
| S2-2 | iDoris 托管下沉 `agent24-idoris` | DONE（#1790，`a5096360`） | Grok + Opus 收尾 | agent24d 减 6,056 行（含测试）；77 条用例随迁；用 `IdorisSlot` 打断 models↔idoris 依赖环。R1 C5 未做 → FU-115；可见性收窄 → FU-116 |
| S2-3 | Knowledge 宿主下沉 `agent24-knowledge` | DONE（#1792，`a8a749bd`） | Grok + Opus 收尾（凭据类，PR-Daemon 已 APPROVE） | agent24d 减 4,447 行；C5 探测测试原样通过；`provider/` 按 jason 拍板冻结；凭据与 endpoint 校验下沉进 registry；knowledge-store CI 改在新 crate 上跑 20 条 |
| S2-4 | attach 新建 `agent24-attach`（排在 S2-1、S2-2 之后） | S2-4a DONE（#1793，`3e07baf3`）；S2-4b DONE（#1794，`378dbca6`） | Grok + Opus 收尾 | 拆两片：4a 在 agent24d 内抽 `AttachGrantBuilder` 等接口，4b 新建 crate 原样搬入；agent24d 减约 4,400 行；C4 用例与 a3_2b/a3_3 黑盒原样通过。过滤未被测试锁住 → FU-118 |
| S2-5 | 鉴权抽 `agent24-auth` | CANCELLED：Q3 = (B) | — | — |
| S2-6 | 进程组终止原语统一 → **降级为停止语义盘点**（jason 2026-11-10 (a)） | DONE（#1788） | Codex 盘点 + Opus 成文 | [S2-6-STOP-SEMANTICS.md](S2-6-STOP-SEMANTICS.md) 覆盖 Rust 的进程组停止调用点与桌面侧计划范围内的现状；不做统一实现 |
| S2-7 | CLI 补齐（Q5 = (b)）→ **改为写明桌面专属例外**（jason 2026-11-10 (a)） | DONE（#1789） | Opus | Comm 私钥备份恢复、Knowledge 绑定/凭据在 daemon 上只允许桌面（ProductHost）调用，CLI 入口调不通，所以不补；连同 OD 下载、presence 一起写进 `AGENTS.md` 例外。CLI 试做存档在本地分支 `feat/s2-7-cli-parity` |

### 2.2 组件统一（ADR-038）

| 任务 id | 标题 | 状态 | PR | 备注 |
|---|---|---|---|---|
| C1（C1a / C1b / C1b'） | ComponentLock schema 2、通用下载器与签名验证、依赖反转 | DONE | #1539 | |
| C2 | 运行期契约检查（Hyphae `hyphae-cli` 契约握手与 health） | **DONE**（**第一期误标 BACKLOG**） | #1574；提交 `93da3e900`、`953c3acb5`（feat(comm): A1） | 见 ADR-038 §4 C2 行。首期称「Agent24 侧没有向 Hyphae 做契约握手」是错的，该合并早于首期快照；iDoris 的 `check_contract` 另有一条路径 |
| C3a / C3b | `/api/v1/components` 统一状态面；OD 状态由桌面上报 | DONE | #1545、#1549 | |
| C4–C7 | AgentEar、Knowledge、Documents、领域 OS / MCP 清单 | DONE | #1572、#1571、#1570、#1575 | |
| C8 | Open Design 改由 `SidecarManager` 托管 | DONE | #1580、#1584 | Q1 = (b) |
| C9 | `requires` 接入授权 | BACKLOG（未排期，不在第 0–2 期） | — | jason 2026-11-10；2026-11-12 起排为主线，实施方案见 [C9-REQUIRES-AUTHZ-PLAN](../2026-11-12/C9-REQUIRES-AUTHZ-PLAN.md) |
| — | Hyphae 锁只有 darwin-arm64、linux-x64 两个平台 | BACKLOG | — | `rust/crates/agent24-comm/hyphae.lock.json`，本期核对仍是这两个 |

### 2.3 内核（CORE-2 / CORE-5）

本期未动，沿用首期，**未重新核对**。

| 任务 id | 标题 | 状态 | PR | 备注 |
|---|---|---|---|---|
| CORE-5f-1 / 5f-2 / 5f-3 | 停靠类型、router、启动接线 | DONE（均在 `ENABLED=false` 后面） | 70f1e9a1、#1528、#1540 | |
| CORE-5g / 5h / 5i | 外壳识别、安装器回退、授权 retry | BACKLOG | — | 开片前对照遗留硬要求 |
| CORE-2 余项、AgentEar 纵切 | | BACKLOG | — | [CORE-2-AGENTEAR-SLICE](../../agent/CORE-2-AGENTEAR-SLICE.md) |

### 2.4 通讯

| 任务 id | 标题 | 状态 | PR | 备注 |
|---|---|---|---|---|
| ADR-044 | 通讯 ADR | DONE（已接受） | #1593、#1601 | |
| 尾巴 2 | 旧集成分支 `ab/comm`、`ab/decide` 盘点存档 | READY（jason 2026-11-10 列为开第 2 期前要清的尾巴） | — | 首期记「需先吸收 main 或归档」；现落后 2939 / 2914 提交；只盘点存档，是否吸收未定 |
| 尾巴 3a：X11 | 外部入口统一走 ADR-041a 准入 | DONE | #1785、#1797、#1805、#1806、#1803、#1810、#1813、#1816、#1808、#1812、#1820、#1821、#1809、#1811、#1818、#1819 | ① 产品隔离 #1785。②③④ 见 [INBOUND-ADMISSION-PLAN](INBOUND-ADMISSION-PLAN.md) §4.1。2e 迁移 #1820 已合（bb03081e）；2e 第 2 部分 Hyphae 轮询走 #1821，已合（a93f5df3）。手机遥控、041b 工单、自动回发本里程碑不做 |
| 尾巴 3b：X7 | 身份归 comm（已定），入站适配不并入 comm（落在 agent24d inbound 模块） | DONE（身份：#1785 删 nostr-bridge；入站适配在 agent24d，见尾巴 3a，不并入 comm） | #1785 | 旧说法「并入 comm」已由 2026-11-12 回写取代。产品构建不启动 wechat-bridge |
| COMM-FRIENDS | 好友机制 | BACKLOG（jason 定为之后，本轮不做） | — | 好友不等于能指挥 agent |

### 2.5 记忆与决策

本期未动，**未重新核对**。

| 任务 id | 标题 | 状态 | 备注 |
|---|---|---|---|
| M1-T06b、M1-T07b | `vector.rs` 等未被 agent 与 daemon 调用 | BACKLOG | R3 §4.3 |
| D1 | 决策日志接线 | BACKLOG | 无实现 |
| 其余 | 记忆 P1（P1-02c 等）、决策 D0-6 | 未核实 | 旧台账已不更新 |

## 3. 暂停与放弃

| 项 | 何时、谁裁决 | 恢复条件 |
|---|---|---|
| 预览版发布 | 2026-11-04 jason | 组件整合与统一告一段落后再定；本期无新预览版 tag |
| Open Design 线 OD-M11 / OD-M12 / TPI-W / OC | 2026-10-03 jason（`PAUSED`） | 路由级资源授权等遗留缺口有人排期时，从 OD-M11 起步 |
| `agent24-sidecar-host`（Rust） | 2026-11-04 Q1 = (b)，冻结 | Windows 支持被排上日程 |
| S1-6、S1-8、S2-5 | 2026-11-04（Q1 = (b)；Q7 = (c)；Q3 = (B)） | 见各行 |
| 好友机制、C9 | 2026-11-10 jason：之后 / 未排期 | 尾巴清完、第 2 期之后再议 |
| 移动端 | 暂不做，将来仅遥控 | ADR-037 决策 6 的遥控设计 |

## 4. 跟进项汇总

明细仍在 [followups.md](../../agent/followups.md)（append-only，规则不变）。

| 来源 | open | done | 本期新增 |
|---|---|---|---|
| `docs/agent/followups.md`（最后提交 `d4053f127`，2026-11-01，与首期相同） | 82 | 39 | 0（文件自首期起没有新提交，数字不变） |

### 本期按来源分类

对 82 条 open 项逐条只看标题行（`- [ ]` 那一行）的前半段，粗分五类；拿不准的归「未分类」。**这是人工粗分，边界有主观成分，只用来看大致分布，不作为排期依据**。

| 类 | 条数 | 说明 |
|---|---|---|
| 安全 | 8 | 身份与分区抢占、信任边界、提示词注入路径、计时旁路、TOCTOU 等 |
| 正确性 | 23 | 竞态、投递语义、错误分类、幂等、进程回收等 |
| 测试 | 16 | 偶发失败、缺回归测试、CI 盲区、变异脚手架 |
| 文档 | 9 | 注释、SPEC 与代码不符、ADR 裁决待写 |
| 体验 | 4 | 记忆可见性、`os list` 不显示项、排序、提示语 |
| 未分类 | 22 | 其中 9 条是 `ME4-CODEX-DEBT-*`（评审补审债，属流程类），其余 13 条是设计取舍、环境问题或归属不明 |
| 合计 | 82 | 与计数命令一致 |

## 附：本表用到的命令

```
git rev-list --left-right --count origin/main...origin/ab/<方向>
git branch -r | grep 'origin/ab/'
git tag --sort=-creatordate --format='%(refname:short)|%(creatordate:short)' | head
gh pr view <n> --json title,mergedAt,state
grep -c '^- \[ \]' docs/agent/followups.md ; grep -c '^- \[x\]' docs/agent/followups.md
git log -1 --format='%h %ad' --date=short -- docs/agent/followups.md
```
