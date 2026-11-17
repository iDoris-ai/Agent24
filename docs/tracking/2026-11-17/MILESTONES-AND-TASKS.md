# 里程碑与任务（2026-11-17）

索引：[架构总入口](../../architecture/README.md)・同期另两份：[ARCH-AND-PLAN](ARCH-AND-PLAN.md)、[PROGRESS](PROGRESS.md)

| 项 | 内容 |
|---|---|
| 快照日期 | 2026-11-17（第三期） |
| HEAD sha | `b7eef06f2b6cde7189eb90d9e0a4840c0b202f1f`（`origin/main`，Merge #1881） |
| 上一期 | [2026-11-10](../2026-11-10/MILESTONES-AND-TASKS.md)，HEAD `c396478c` |
| 数据来源 | 1. 上期任务表与本期四份方案；2. `gh pr view` 与 `gh pr list --state merged --search "merged:>=2026-11-10"`；3. `git log --first-parent --merges c396478c..origin/main`；4. 文末命令的分支计数与 followups 计数 |
| 生成方式 | 手工整理。§1 的领先 / 落后用文末命令实测。followups 的 `FU-` 计数用文末那条 `grep`。状态按 2026-11-17 的 main 填 |
| 状态词 | `READY` 可开工　`DOING` 在做　`IN-PR(#)` 已开 PR 待评审　`DONE(#)` 已合入　`BLOCKED` 被阻塞　`PAUSED` 暂停　`CANCELLED` 取消或作废　`BACKLOG` 已规划未排 |
| 节奏 | 每两周或每次 release 出一期（Q11） |

## 0. 当前主干与并行线（jason 最近裁决）

只记本期新出现的。2026-11-10 及更早的见[上期 §0](../2026-11-10/MILESTONES-AND-TASKS.md)。

| 日期 | 裁决 | 出处 |
|---|---|---|
| 2026-11-13 | **只推主干。** 边缘情况、低概率事件先不做；评审只修打断主流程的问题；文档一轮就冻结 | #1846；`AGENTS.md` 文首 |
| 2026-11-12 | **C9**：第一方签名、随桌面装的模块，启用即同意，只展示拿到的内核能力。第三方同意卡以后做。同意卡落地前，第三方内核能力维持现状 | [C9 方案](../2026-11-12/C9-REQUIRES-AUTHZ-PLAN.md) 第 10–11 行 |
| 2026-11-12 | **X11 收口范围**：手机遥控、ADR-041b 工单、自动回发 run 结果，本里程碑不做 | FU-125；#1826 |
| 2026-11-15 | **沙箱**：第一版只管 OS 模块。要联网或读更多文件一律不给（SBX-3 暂不做）。macOS 上沙箱起不来就拒绝启动；Linux 第一版照常启动并标未隔离 | [沙箱方案 §5](../2026-11-14/MODULE-SANDBOX-PLAN.md) |
| 2026-11-17 | **好友机制归 Hyphae 仓库**，不在 Agent24 排期。Hyphae 仓进度未核实 | 本跟踪任务说明 |
| 2026-11-17 | **Sin90 随 Agent24 发包**（OSI-2，像 documents 一样打进 DMG），并做方向 / 节奏 / 任务三层完整页面（先出方案 SIN90-UI-0）；好友机制归 Hyphae 仓库 | jason 2026-11-17 |
| 2026-11-14 / 11-17 | **恢复发预览版**：11-14 weknora 进 main 后授权发 20261114.1；11-17 验收「手动选模型」后授权发 20261117.1。11-04「停发」就此改口 | jason 2026-11-14、2026-11-17（会话口头裁决，本行为书面记录） |
| 2026-11-04 | 预览版停发 | 上期；已被上一行改口 |

## 1. 里程碑总表

领先 / 落后是 2026-11-17 对 `origin/main`（`b7eef06f2`）的实测：`git rev-list --left-right --count origin/main...origin/ab/<方向>`。左数是 main 有、该分支没有的提交数；右数是该分支多出来的提交数。括号里是上期同位置的数（上期写法是「落后 / 领先」）。

| 方向 | 里程碑 | 集成分支 | 目标 | 状态 | 计划文档 |
|---|---|---|---|---|---|
| 架构优化 | 第 0–2 期 | 已合入 main | 文档收口、低风险结构调整、agent24d 瘦身 | **全部 DONE**（S1-6、S1-8、S2-5 CANCELLED）。S0-8 本期改为 DONE(#1823) | 上期 §2.1 |
| 架构优化 | 拆大文件 | 直接合入 main | `lib.rs` / `server.rs` 按职责拆开，行为不变 | SPLIT-1..4 DONE | #1859–#1862 |
| 组件 | C9 | 直接合入 main | `requires` 接入授权 | DONE | [C9 方案](../2026-11-12/C9-REQUIRES-AUTHZ-PLAN.md) |
| 技能 | 加载器 v1 | 方案写 `ab/skill`，远端没有这条分支；各片直接合入 main | ADR-040 v1 | DONE | [技能方案](../2026-11-12/SKILL-LOADER-V1-PLAN.md) |
| 记忆 | 向量召回 | `ab/vector-recall` 已不在远端（合入后删除，删除记录未单查） | 混合召回进对话循环 | VR-1..VR-6 DONE | [向量方案](../2026-11-13/VECTOR-RECALL-PLAN.md) |
| 模块沙箱 | SBX-0..2 | 直接合入 main | macOS seatbelt、临时目录、隔离状态 | DONE。SBX-3 PAUSED，SBX-4 BACKLOG | [沙箱方案](../2026-11-14/MODULE-SANDBOX-PLAN.md) |
| 模型选择 | MS-1 / 1b / 2 | 直接合入 main | 默认对话模型、服务模式可保存、启动失败可见 | DONE。按内存推荐 BLOCKED | #1879 #1881 #1880；iDoris #602 |
| 通讯 | X11、X7 | 直接合入 main | 外部入口走准入 | DONE。好友机制不排期 | [入站方案](../2026-11-10/INBOUND-ADMISSION-PLAN.md) |
| 知识库 | WeKnora | `ab/weknora`（main 领先 **20**（上期 55），分支领先 **0**（上期 49）） | 本期集成 #1852、#1854、#1872 | 分支已快进到被 main 包含；main 又多了 20 个提交 | [ADR-KB-01](../../knowledge/ADR-KB-01-SINGLE-CONVERSATION.md) |
| UI | ab/ui | `ab/ui`（**26** / **15**；上期 61 / 82） | 本期集成 #1814、#1849、#1873 | 领先的 15 个提交还没进 main，含 #1874–#1878 | — |
| 内核 | CORE-5 / CORE-2 | `ab/kernel`（1409 / 1；上期 909 / 1） | 沿用上期 | 未重核 | [CORE-5 设计](../../agent/CORE-5-MIGRATION-GUARD-DESIGN.md) |
| Documenting | T1 | `ab/documenting`（953 / 0；上期 453 / 0） | David 负责 | 未重核内容 | `docs/documenting/` |
| 其他集成分支 | — | 见下方清单 | 内容未逐个读 | 未核实 | — |

其他 `origin/ab/*`（左 main 领先，右分支领先）：

| 分支 | main 领先 | 分支领先 | 上期 |
|---|---|---|---|
| `ab/kernel-22-egress-exec-memory` | 3396 | 1 | 2896 / 1 |
| `ab/maint` | 3391 | 2 | 2891 / 2 |
| `ab/office-assistant` | 2957 | 27 | 2457 / 27 |
| `ab/ui-b3-daemon-test` | 531 | 3 | 61 / 33 |
| `ab/ui-b5a-upgrade` | 531 | 5 | 61 / 35 |
| `ab/ui-b5b-crash-loop` | 531 | 6 | 61 / 36 |
| `ab/ui-pdf-split-merge` | 26 | 19 | 上期未列 |
| `ab/weknora-03a-state-machine` | 2919 | 9 | 2419 / 9 |
| `ab/weknora-03b-ui-bridge` | 2919 | 6 | 2419 / 6 |
| `ab/weknora-03c-ops` | 2919 | 15 | 2419 / 15 |
| `ab/weknora-1670-h1-submit` | 523 | 7 | 55 / 39 |
| `ab/weknora-1670-h1a-md` | 523 | 1 | 55 / 33 |
| `ab/weknora-1715-kb3b` | 523 | 3 | 55 / 35 |

本期远端**已经没有**的上期分支：`ab/comm`、`ab/decide`（GitHub API 404）、`ab/vector-recall`、`ab/weknora-1670-h1-submit-2`、`ab/weknora-1670-h2-ui-2`。`ab/memory-p1` 上期也没有，本期仍没有。`ab/comm` 与 `ab/decide` 的盘点存档文本没找到，**未核实**。

`ab/ui` 上、合并提交还不是 main 祖先的 PR：#1874、#1875、#1876、#1877、#1878（`git merge-base --is-ancestor <merge> origin/main` 退出码非 0）。同一次搜索里其余合进 `ab/ui`、`ab/weknora`、`ab/vector-recall` 的 PR，合并提交已经是 main 的祖先，见 PROGRESS §1 后的对照。

## 2. 按方向的任务表

第 0 期 S0-1..S0-7、第 1 期 S1-1..S1-9、第 2 期 S2-1..S2-7 的逐片说明在[上期 §2.1](../2026-11-10/MILESTONES-AND-TASKS.md)。本期只改状态有变化的行，并补上 11-10 快照之后合入的片。合入 main 的标题取自合并提交正文，并与 `gh pr list` 的 `title` 对过，没有对不上的；#1811 不在那次搜索结果里，用 `gh pr view` 核对过，标题一致。基线是 main，除非另行写明。

### 2.1 架构优化（第 0–2 期与拆文件）

| 任务 id | 标题 | 状态 | PR |
|---|---|---|---|
| S0-1..S0-7 | 见上期 | DONE | 见上期 |
| S0-8 | KB-HOST-01 落成 ADR-KB-02；iDoris 托管 ADR-046；ADR-035 标部分取代 | DONE | #1823（2026-11-12） |
| S1-* / S2-5 | 见上期 | DONE 或 CANCELLED | 见上期 |
| S2-1 | 拆 `server.rs`，`serve()` 1536→267 行 | DONE | #1791。之后 SPLIT-4 又把 `serve()` 移到 `server/serve.rs` |
| S2-2 | iDoris 托管下沉 | DONE | #1790。FU-115、FU-116 已关（#1799） |
| S2-3 | Knowledge 宿主下沉 | DONE | #1792 |
| S2-4a / 4b | attach 抽缝、新建 `agent24-attach` | DONE | #1793、#1794。FU-118、FU-119 已关（#1802） |
| S2-6 | 停止语义盘点 | DONE | #1788。FU-114 已关（#1800） |
| S2-7 | 桌面专属例外写进 `AGENTS.md` | DONE | #1789 |
| SPLIT-1 | `agent24-agent` 的 `lib.rs` 内联测试搬出 | DONE | #1859 |
| SPLIT-2 | `server.rs` 内联测试搬到 `server/tests` | DONE | #1860 |
| SPLIT-3 | `agent24-agent` 的 `lib.rs` 业务代码按职责拆开 | DONE | #1861 |
| SPLIT-4 | `server.rs` 业务代码按职责拆开 | DONE | #1862 |
| — | 依赖表检查进 CI | DONE | #1798（FU-117） |
| — | Codex 补审 S2 五片 | DONE | FU-113 已勾。followups 写 2026-11-12 只读补审无发现 |

S2 这几片的合并提交在上期快照 sha 之后，所以上期正文里的「已合」是事后补记，统计表在本期 PROGRESS。

### 2.2 组件统一（C9）

C1–C8 见上期，状态不变。

| 任务 id | 标题 | 状态 | PR |
|---|---|---|---|
| C9 方案 | `requires` 接入授权怎么切 | DONE | #1822 |
| C9-A | 声明与解析；三处授予行为这片不改 | DONE | #1825 |
| C9-B | 开发桥令牌、显式路由门、审计 | DONE | #1830 |
| C9-C | bridge-token CLI、桥只读令牌、去掉 legacy 入站 | DONE | #1833 |
| C9-D | `/components` 展示挂载后的实际授予 | DONE | #1831 |
| C9 收口 | 桥游标失败收紧、重签先落盘再撤旧、文档订正 | DONE | #1841 |
| 第三方同意卡 | 启用前的同意、记录、撤销 | BACKLOG（方案未写，见 §2.8） | — |
| Hyphae 锁平台 | 仍只有 darwin-arm64、linux-x64 | BACKLOG | `hyphae.lock.json` |

### 2.3 内核（CORE-2 / CORE-5）

本期未重核，沿用上期。

### 2.4 通讯

| 任务 id | 标题 | 状态 | PR |
|---|---|---|---|
| X11 ① / X7 | 微信桥产品隔离；删除旧 nostr-bridge。身份归 comm，入站适配在 agent24d | DONE | #1785 |
| X11 方案 | 入站准入实施方案 | DONE | #1797 |
| X11-2a | 入站事件表与 store | DONE | #1805、#1806 |
| X11-2b | 准入纯函数与策略版本 | DONE | #1803。`max_age` / `rate_limit` 不进哈希 → FU-120 |
| X11-2d | 入站正文到期清理 | DONE | #1808、#1812。会话里有过 `/chat` 后不按事件清 → FU-121 |
| X11-2f | 名单、接受模式、事件列表。写操作 host-only | DONE | #1809、#1811。深分页索引 → FU-122 |
| X11-2c | 起 run、InboundService、接线闸门 | DONE | #1810、#1813、#1816。瞬时错误误标、空白正文 → FU-123 |
| X11-2g | 微信入站路由；桥只传输 | DONE | #1818、#1819 |
| X11-2e | 群聊游标迁移；Hyphae 入站适配 | DONE | #1820、#1821。每轮最多 200 条、占位限额 → FU-124 |
| X11-2h | 防绕过测试与文档回写 | DONE | #1826。遥控 / 工单 / 回发 → FU-125；审计摘要与自唤醒 prompt → FU-126 |
| COMM-6b | 概览页整表替换 Relay + 手动探测 | DONE | #1786。这不是好友机制 |
| 尾巴 2 | `ab/comm`、`ab/decide` 盘点存档 | 未核实 | 两条远端分支 404，存档文档没找到 |
| COMM-FRIENDS | 好友机制 | 不排期 | 归 Hyphae 仓库。本仓设计稿还在，Hyphae 进度未核实 |

### 2.5 记忆与决策

| 任务 id | 标题 | 状态 | PR | 备注 |
|---|---|---|---|---|
| 向量方案 | 分片与拍板 | DONE | #1843 | |
| VR-1 | 本机嵌入客户端 | DONE | #1845 | 合入 `ab/vector-recall`，再随 #1851 进 main |
| VR-2 | 关闭事实即删向量（迁移） | DONE | #1844 | 同上 |
| VR-3 | 混合检索接进召回 | DONE | #1847 | 同上 |
| VR-4 | 就绪后补嵌并接到会话记忆 | DONE | #1848 | 同上 |
| VR-5 | 中文换说法评测与文档收口 | DONE | #1850 | 同上 |
| VR 进 main | 发布合并 VR-1…VR-5 | DONE | #1851 | 分支名就是 `ab/vector-recall` |
| VR-6 | 问句先改写成短检索语 | DONE | #1856 | 直接合入 main |
| D1、记忆 P1 其余 | 决策日志接线、清除权等 | 未重核 | — | 沿用上期 BACKLOG / 未核实 |

任务说明里的「#1844–#1851」这一段号码，不全是向量片：#1846 是只推主干，#1849 是 `ab/ui` 集成。向量片是上表这些。

### 2.6 技能加载器 v1

方案 §5 写集成分支 `ab/skill`。远端没有这个分支。下面各片的 merge 都在 main 的 first-parent 上。

| 任务 id | 标题 | 状态 | PR |
|---|---|---|---|
| 方案 r2 | 已拍板的实施说明 | DONE | #1824 |
| SL-1 | 纯解析与文本一致性（新 crate `agent24-skill`） | DONE | #1827 |
| SL-2 迁移 | 迁移 0036：修订、指针、激活、评测结果 | DONE | #1828 |
| SL-2b | 修订存储与指针移动 | DONE | #1829 |
| SL-3 | 导入 API 与用户目录扫描 | DONE | #1832 |
| SL-3 后续 | 坏条目、守卫、去重、413 文案 | DONE | #1836 |
| SL-4 | 激活与回滚 | DONE | #1834 |
| SL-5 | 准入、钉修订、注入、SkillText 来源标签 | DONE | #1835 |
| SL-6 | 能力中心技能栏 | DONE | #1840 |
| SL-7 | CLI：`skill list/show/install/rescan/activate` | DONE | #1842 |

#1827 与 #1842 之间还合入了别的方向（C9、X11-2h、clippy #1839）。那些不是技能片。

### 2.7 模块沙箱与模型选择

| 任务 id | 标题 | 状态 | PR |
|---|---|---|---|
| 方案 | 分片与 2026-11-15 拍板。SBX-0 是探针，方案写明不出 PR；探针记录在这份文档里 | DONE | #1858 |
| SBX-1 | macOS `sandbox-exec` 启动 OS 模块 | DONE | #1863 |
| SBX-1b | 每个模块只用自己的 `<data>/tmp`，不再放行用户 TMPDIR | DONE | #1865 |
| SBX-2 | `/components` 与组件页显示已隔离 / 未隔离 | DONE | #1864 |
| SBX-3 | 模块要更多权限时的出口 | PAUSED | 方案 §5 选了「一律不给」 |
| SBX-4 | Linux Landlock | BACKLOG | 方案 §4 |
| MS-1 | 默认对话模型可选可保存；oMLX 密钥不再写死 | DONE | #1879 |
| MS-1b | 服务模式下 shell 令牌也能保存默认模型 | DONE | #1881。隐藏字符或 121–128 字的 id 在确认阶段被拒 → FU-131 |
| MS-2 | 后台起不来时显示原因；安装服务前先 `launchctl enable` | DONE | #1880 |
| 按内存推荐 | 接 iDoris §2.9 | BLOCKED | 等 iDoris #602、#600，都是 OPEN |

### 2.8 在做 / 下一步

| 任务 id | 标题 | 状态 | 备注 |
|---|---|---|---|
| OSI-1 | Sin90 按需安装 | DOING | 只在本地 worktree，未提交，无 PR。见 ARCH §5 |
| — | 第三方模块同意卡方案 | READY | 分支已开，没有正文 |
| — | followups 分批清理 | READY | 85 条 open `FU-`。分支已开，没有正文 |
| — | iDoris 模型推荐接入 | BLOCKED | 等外仓 #602 与 #600 |
| S0-8 订正 | `docs/s0-8-adr-cleanup`（#1883） | 在审 | S0-8 首版是 #1823；#1883 按现行代码订正 ADR-KB-02 / ADR-046 / ADR-035 |

### 2.9 同期集成（合进 main 的那几笔）

| PR | 日期（git 作者日） | 一句话 |
|---|---|---|
| #1814 | 2026-11-11 | CLA allowlist 加上旧账号，直接进 main |
| #1849 | 2026-11-13 | `ab/ui` @ `4e6d70e5` 进 main |
| #1852 | 2026-11-13 | `ab/weknora` @ `002f6c4d` 进 main |
| #1854 | 2026-11-13 | `ab/weknora` @ `088cfd47` 进 main。预览版 20261114.1 的 main 就在这个合并上 |
| #1872 | 2026-11-15 | `ab/weknora` @ `30616f6a` 进 main（K1–K3，只改测试和注释） |
| #1873 | 2026-11-15 | `ab/ui` @ `63c1eda6` 进 main（登记被拒时说明原因） |
| #1857 | 2026-11-14 | Open Design 首次启动就绪时限放宽 |
| #1839 | 2026-11-13 | clippy 1.100 `needless_bool`，修 macOS CI |
| #1782 | 2026-11-10 | 上期三份跟踪文档 |
| #1795 | 2026-11-10 | 第 2 期状态回写，登记 FU-113–119 |

合进集成分支、且合并提交已经在 main 里的 PR，清单在 PROGRESS。还没进 main 的是 #1874–#1878。

## 3. 暂停与放弃

| 项 | 何时、谁裁决 | 现状 |
|---|---|---|
| 预览版停发 | 2026-11-04 jason | 已改口：2026-11-14、11-17 jason 授权恢复发布（见 §0） |
| Open Design 线 OD-M11 等 | 2026-10-03 | 沿用上期 PAUSED，未重核 |
| `agent24-sidecar-host` | Q1 = (b) | 沿用上期，冻结 |
| S1-6、S1-8、S2-5 | 2026-11-04 | CANCELLED，见上期 |
| SBX-3 | 2026-11-15 方案 §5 | 暂不做 |
| 好友机制 | 2026-11-17 本任务说明 | 不在本仓排期 |
| 手机遥控、ADR-041b、自动回发 | 2026-11-12 FU-125 | 本里程碑不做 |
| 移动端 | 沿用上期 | 暂不做 |
| C9 | 2026-11-10 曾未排期 | 2026-11-12 起已完成，不再暂停 |

## 4. 跟进项汇总

明细仍在 [followups.md](../../agent/followups.md)。

上期在 `c396478c` 上数的是全部 `- [ ]` / `- [x]`：82 / 39。那个提交上的文件，本期重数了一次，数字相同；其中 open 的 `FU-` 是 73，另有 9 条 `ME4-CODEX-DEBT-*` 没有 `FU-` 前缀。

本期 HEAD 上：

| 计数 | 命令 | 结果 |
|---|---|---|
| open 的 `FU-` | `grep -cE '^- \[ \] FU-' docs/agent/followups.md` | **85** |
| 全部 open 勾选 | `grep -cE '^- \[ \]'` | 94 |
| 全部 done 勾选 | `grep -cE '^- \[x\]'` | 46 |

`c396478c..origin/main` 的 `followups.md` diff 只有新增行，没有改旧行的勾。对得上的开关是：

- 关掉 7 条：FU-113、FU-114、FU-115、FU-116、FU-117、FU-118、FU-119（S2 评审与 FU-114 的 oMLX 误杀）。
- 新开 12 条：FU-120 至 FU-124（X11）、FU-125 至 FU-126（X11 明确不做和已知限制）、FU-127 至 FU-130（C9 遗留，正文写本轮不修）、FU-131（MS-1b，异常模型 id）。

9 条没有 `FU-` 前缀的 open 项仍是 `ME4-CODEX-DEBT-*`（followups.md 第 99–108 行一带）。本期不重做上期那种五类粗分。

文件最后一次被改到，是 `3cb7641d3`（#1881，2026-11-17），那一笔加上了 FU-131。

## 附：本表用到的命令

```
git rev-list --left-right --count origin/main...origin/ab/<方向>
git branch -r | grep 'origin/ab/'
git merge-base --is-ancestor <集成 PR 的 merge sha> origin/main
gh pr view <n> --json title,mergedAt,state,baseRefName
gh pr list --state merged --search "merged:>=2026-11-10" --limit 200 --json number,title,mergedAt,baseRefName,headRefName,mergeCommit
grep -cE '^- \[ \] FU-' docs/agent/followups.md
grep -cE '^- \[ \]' docs/agent/followups.md
grep -cE '^- \[x\]' docs/agent/followups.md
git diff --stat c396478c9f8797c339096dbcdbf4d01b242d756f..origin/main -- docs/agent/followups.md
```
