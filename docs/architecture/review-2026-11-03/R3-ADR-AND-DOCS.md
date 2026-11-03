# 架构梳理 R3：ADR 与架构文档的整理建议、文档与代码的漂移、跟踪文档骨架

索引：[架构总入口](../README.md)・[ADR 索引](../../adr/README.md)

| 项 | 内容 |
|---|---|
| 日期 | 2026-11-03 |
| HEAD sha | `9865d7a0303cf73e2766e2f20a174159dc1bd975`（main，Merge #1555，2026-11-03 10:32 +0700） |
| 数据来源 | 仓库源码（`rust/`、`apps/`、`packages/`、`protocol/`）、`docs/` 全部 ADR 与设计文档、本地 `git log` / `git show` / `git diff bb434b61..HEAD`、`cargo metadata --no-deps --format-version 1 --offline`、`git grep -I` 引用计数 |
| 生成方式 | 只读静态核查：读源码、grep、git 历史。**没有构建、没有运行、没有联网**。PR 号一律取自本地 merge 提交（"Merge pull request #NNNN from iDoris-ai/…"）或提交标题里的 `(#N)`；GitHub 上的 PR 正文、评论、issue 状态、ruleset 都看不到，涉及处写「未核实」 |
| 必读输入 | `CLAUDE.md`、`AGENTS.md`、`docs/architecture/README.md`、`docs/adr/README.md`、`docs/ARCHITECTURE-LAYERS.md`、`docs/adr/ADR-039-concept-model.md`、`docs/architecture/EVOLVABILITY-ASSESSMENT-2026-11-01.md` |
| 判定口径 | 现状一律以代码为准。实现度：**已实现**（在产品路径上）/ **部分** / **底座在、未接线**（有代码，产品路径没有调用方）/ **未实现**（grep 零命中或只有文档）。读不到写「未核实」 |
| 改动范围 | 只新增本文件，不改任何代码和其他文档 |

> 关键事实已由主笔逐条复核（`migration_coordinator.rs:20` `ENABLED=false`、`main.rs:114` 断言、`agent24-idoris/Cargo.toml:9` 依赖 components、`decision.md:1508` 与 `CORE-5-MIGRATION-GUARD-DESIGN.md:200` 的矛盾、`runner.rs:269` 直接 SIGKILL、`component_contract_mismatch` 只在 ADR-038 自身出现 2 次、`tasks.md:70-76` K1 状态、`nostr-bridge/src/config.ts:45`、`fetch_verified` 在 `rust/` 零命中）。其余明细由分工核查得出，均附 file:line，可按行复查。

---

## 0. 摘要

### 0.1 一句话

**ADR 的状态普遍落后于代码，架构文档在 11-01 修过一次后又被 #1539（组件契约 C1）等 PR 甩开**；通讯有 31 条已定决策却没有 ADR，而且其中有几处文档互相矛盾；跟踪文档散在 5 处以上，自称「唯一权威」的 `tasks.md` 有 24 条过期。

### 0.2 最重要的 5 条发现

| # | 发现 | 关键证据 | 建议 |
|---|---|---|---|
| 1 | **「提议 / 草案」的 ADR 实际早已在实施**。ADR-037 写「提议，批准前不派生产实现」（`docs/decision.md:1508`），main 上已有至少 9 个按它实施的 PR（#1292 #1295 #1301 #1364 #1371 #1378 #1428 #1528 #1540）；而 `CORE-5-MIGRATION-GUARD-DESIGN.md:200` 却说「ADR-037 已于 #1249 接受，状态行已由 #1304 订正」，两处直接矛盾（`git log -L1508,1508:docs/decision.md` 显示状态行自 `a198a94b`（2026-10-26 初稿）起从未改过；#1304 只改了 decision.md 的其他 4 行）。ADR-038 写「草案 r3」（`ADR-038:5`），正文已写到 r4/r5（`:356`、`:439`），C1/C1b/C3a/C3b 已随 #1539 #1545 #1549 #1551 #1552 合入。MEMORY-P1 与 R05 已有外审 APPROVE 记录（#1247、#1248），文首仍写 PROPOSED | §1.1 总表 | 一次性订正状态：037 → 已接受（决策 1–5）；038 → 出 r5 订正稿后接受；MEMORY-P1 / R05 → 已接受（设计） |
| 2 | **ADR-038 正文自相矛盾，§7 的订正没回写到 D1–D9**；还有三处实现偏离：D3 运行期契约检查没做（`component_contract_mismatch` 在代码里零命中，iDoris 失配映射成 `failed` 而非 `incompatible`）；D7 的 `requires` 没有任何授权代码读取；D6 写「CLI 超时先 SIGTERM，2 秒后 SIGKILL」，代码直接 SIGKILL 进程组（`agent24-comm/src/runner.rs:269-285`）。§7.11 自己承认的偏离：schema 1 不再可读（D2 原文 `:137` 仍写「要能读」）、`provides[].name/summary` 改可选、`stop_grace_ms` 未接线 | §1.A.1.3、§1.A.1.5 | r5：把 §7.1–§7.11 回写进 D2/D2a/D4/D5；D6 二选一（改代码或改正文）；D7 标「待接线」 |
| 3 | **一批「已接受」的日志 ADR 已被事实取代或从未实现**：取代——ADR-001、006、012/013、014/015、016（日志）、024，以及 docs/ 下的 ADR-016（写「进行中」，迁移分支已不存在，代码零 `@auraaihq/*` 依赖）；从未实现——ADR-005、008、009、021（相关文件在全部 git 历史中从未出现）、ADR-DOC-03（引擎只有 PDFKit） | §1.B.6 | 索引加「事实取代 / 搁置」两档，逐条注明取代者；docs/ADR-016 改「被替代（ADR-026 §6）」 |
| 4 | **架构文档又漂移了**：`ARCHITECTURE-LAYERS.md` 依赖表把 idoris / components 的方向写反（#1539 做了依赖反转），`worker → protocol` 在 11-01 重新生成时就漏了；§7 第 10 条（组件安装器借 iDoris 锁）已整条过期，第 9 条（CORE-5）部分过期（5f-1/2/3 已合入，仍关在 `ENABLED=false` 后）；L6 仍说 ADR-039「计划中、审阅中」，实际已接受。EVOLVABILITY 建议 2、6 已完成。`tasks.md` 有 24 条过期（例：K1-1~4 写 `PLANNED`，四份 ADR 在 10-08 已 Accepted；COMM-5b/6 写 `BACKLOG`，已由 #676/#679 完成；M2 release 写 `BACKLOG`，#767 已合）；`roadmap.md` 10-08 之后没更新，缺 CORE-5、ADR-038 等台账 | §4 | 按 §4 各表的「修正建议」列逐条改；先改依赖表和 §7 |
| 5 | **通讯（Hyphae/Nostr）有 31 条已定决策没有 ADR，还有实质矛盾**：`HYPHAE-NETWORK-DELIVERY.md:10,31` 说「随包交付、不靠首次联网下载」，COMM-HYPHAE R4 和 #1489 做成了按需下载；nostr-bridge 与 comm 各用一套 Nostr 身份，且 nostr-bridge 不认 `A24_HYPHAE_BIN`（`config.ts:45`）；入站执行最终走 T01-E、ADR-041 还是遥控入口没有定。另有两项决策只在 PR 里（NIP-49 私钥备份 #1535、群聊 #1377）。通讯之外，iDoris 由 agent24d 托管、Knowledge 宿主接口 KB-HOST-01（正文只在 issue #1085）、SPEC-EDITIONS 版本边界也缺 ADR | §3 | 先裁掉三处矛盾，再按 §3.A.3 提纲写通讯 ADR（拟用 ADR-044） |

### 0.3 需要 jason 拍板的问题

| # | 问题 | 选项 / 本文建议 | 详见 |
|---|---|---|---|
| Q1 | 是否按本文一次性订正 ADR 状态（037、038、MEMORY-P1、R05、028、KB-01、docs/ADR-016）？ADR-037 到底接受了没有（`decision.md:1508` 与 `CORE-5…:200` 矛盾） | 建议：037 → 已接受（决策 1–5），决策 6 待遥控设计；038 → r5 后接受；MEMORY-P1 / R05 → 已接受（设计）；028 → 以收口段为正文、修订后接受；KB-01 拆成 01a（已接受）/ 01b（提议）；docs/ADR-016 → 被替代 | §1.1 |
| Q2 | ADR-040–043 怎么定？042 Q1 与 043 D1 循环依赖，先做哪个 | 建议：040 接受为设计，实施挂在 ADR-038 C7 之后；041 拆成「入站事件 + 准入」与「工单 + 授权依据」；042 先接受 D3、D4 边界条款；043 先接受 D1 + D3（只含 `running_jobs`/`budget`），打破循环。所有新表等 CORE-5 `ENABLED=true` 之后 | §1.A.2–§1.A.5 |
| Q3 | ADR-038 D6：CLI 超时改代码（先 SIGTERM）还是改正文（直接 SIGKILL 进程组）？ | 二选一；本文不预设 | §1.A.1.5 #11 |
| Q4 | Hyphae 客户端随包交付还是按需下载？ | 代码现状是按需（#1489）；需决定是否作废 `HYPHAE-NETWORK-DELIVERY.md:10,25,31` | §3.A.2 X1 |
| Q5 | agent 的 Nostr 身份以谁为准（nostr-bridge 还是 comm 的专用 HOME）？入站执行最终走 T01-E、ADR-041 准入还是 ADR-037 决策 6 遥控？ | 建议在写通讯 ADR 前先定 | §3.A.2 X7、X11 |
| Q6 | 编号方案：只加别名、不改文件名（OD → `ADR-OD-00x`；docs/ADR-016 → `ADR-016P`；初稿 → `DRAFT-F04x`），新号从 ADR-044 起、ADR 索引是唯一发号处，是否同意？ | 建议同意 | §2.3 |
| Q7 | 「已接受但从未实现」的 ADR-005、008、009、021、DOC-03 标「废弃」还是「搁置」？ | 建议 005/008/009/021 标「废弃（方向已被 ADR-026/036/040 取代）」，DOC-03 标「搁置（待 #692 格式矩阵）」 | §1.B.6 |
| Q8 | 跟踪文档放 `docs/tracking/<日期>/`、每周一生成；`tasks.md` / `progress.md` / `roadmap.md` 降为历史，是否同意？ | 建议同意 | §5 |

---

## 1. ADR 状态建议

### 1.1 总表（所有「提议 / 草案 / 未标 / 进行中」的 ADR，以及状态与事实不符的「已接受」ADR）

| ADR | 状态原文（file:line） | 实现度 | 主要 PR | 正文与实现是否一致 | 建议 | 一句话理由 |
|---|---|---|---|---|---|---|
| ADR-037 产品形态与进程模型 | 「🟡 提议…批准前不派生产实现」`docs/decision.md:1508` | 决策 1、3、4 骨架已落地；决策 2 部分（停靠态已写未启用，常驻服务未做）；决策 6 未开始 | #1292 #1295 #1301 #1364 #1371 #1378 #1428 #1528 #1540 #1484 | 不一致：状态与已合入的实现矛盾；背景段仍是 `3a433bb0` 的事实；登记握手只有 macOS | **修订后接受**（决策 1–5），拆出 G-API-4 事件流授权、「桌面默认走常驻服务」两项后续 | 已按它实施 9 个以上 PR，「提议」既不真实，也让后来者无法判断哪些已定 |
| ADR-038 组件契约 v1 | 「草案 r3」`docs/adr/ADR-038-component-contract-v1.md:5` | C1、C1b、C3a、C3b 已实现；D3 握手、D6 取消、D7 授权、D9 登记表与多提供者未实现 | #1539 #1545 #1546 #1549 #1551 #1552 | 不一致：16 处（§1.A.1.5），含 §7.11 的 schema 1、`provides` 可选、`stop_grace_ms` 未接线等偏离 | **修订后接受**（先出 r5 订正稿） | 实现已按它推进；继续挂草案只会让正文和代码继续分叉 |
| ADR-040 技能契约 v1 | 「提议」`ADR-040:5` | 零代码（`rust/` 下 `skill` 零命中） | #1530（仅文档） | 一致（没有代码） | **接受（作为设计约束）**，实施挂在 ADR-038 C7 之后；D6 主动技能标「v1 不做」 | 与已接受的 ADR-039 规则 3 一致；真正的阻塞是 REGISTRY 与 `skills` 字段 |
| ADR-041 入站事件、准入与工单 | 「提议」`ADR-041:5` | 零代码（`inbound_events`、`work_item`、`mandate` 零命中） | #1530 | 背景行号漂移；要求 ADR-038 加 `events` 块未被吸收 | **拆分后接受**：041a 入站事件 + 准入；041b 工单 + 授权依据 + 订阅 | 一份含 4 张表、10 条决策、5 个子系统；前半独立有价值 |
| ADR-042 待执行动作与对账 | 「提议」`ADR-042:5` | 零代码 | #1530 | 与 ADR-043 循环依赖；`provides[].tools` 是字符串数组，挂不上 `idempotency_key` | **接受边界条款 D3、D4**；D1/D2 实施排在 041 工单之后（`mandate_id` 可空即可先做 `channel_send` 收据） | 边界条款现在拍板可防后续 PR 越界，没有代价 |
| ADR-043 后台作业、并发与监督 | 「提议」`ADR-043:5` | 零代码（`/health` 无 `work` 条目） | #1530 | 与 042 循环依赖 | **拆分**：D1 + D3（只含 `running_jobs`/`budget`）先接受；D2/D4/D5 留提议 | D1 解决有证据的真问题（`shell_exec` 同步 30 s，`agent24-tools/src/local.rs:508`），且可不经 042 先落 |
| ADR-028 记忆分层 | 「🟡 暂定 / 需 spike」`docs/decision.md:1023` | 三份权威中 EventLog、AssertionLedger 已接线；FTS 可重建投影已实现；ArtifactStore、向量、Consolidator 底座在、未接线 | #114 #116 #117 #119 #120 #123 | 不一致：原「决策」段（四层、拆 4 个 crate）已被同 ADR 收口段推翻但未标；spike 早已合入 | **修订后接受**（以收口段为正文，原决策段标被取代；Condenser 拆分、向量、`memory rebuild` 进开放问题） | 「需 spike」已不成立，后续 M1、P1 都按「权威 + 投影」在做 |
| ADR-MEMORY-P1 生命周期与清除权 | 「PROPOSED / 待外审接受」，同段写「被 clestons APPROVE」`ADR-MEMORY-P1:5` | 暂停、撤回、配额失败回执与留痕、磁盘门已实现；purge、无痕、导出、更正未实现 | #1247（外审）#1275 #1284 #1285 #1296 #1302 #1341 | 不一致：状态自相矛盾；4 处过时引用 | **修订后接受，并拆分**（已落地 / 已接受待实现） | 外审接受记录已存在 |
| ADR-MEMORY-RECALL-MESSAGE-ORIGINS（R05） | 「PROPOSED / 待外审接受」，同段写 APPROVE `:5` | 来源表、同事务写入、按 kind 整形、冷恢复、HostSystem 已实现；K1 同事务、G1、诊断消费者未实现 | #1248（外审）#1168 #1195 #1197 #1250 #1251 #1261 | 不一致：列为「仍开着」的三项已关；#1245 写「未合并」实已合 | **修订后接受**；G1（`recall_ref` 未对照账本，`agent24-agent/src/lib.rs:556-570`）单独开一片 | G1 是唯一仍碰发送安全的缺口 |
| ADR-KB-01 唯一主对话 | 无状态标签（「用户确认产品方向；工程契约待评审」）`docs/knowledge/ADR-KB-01-SINGLE-CONVERSATION.md:5` | 单一主对话 + 只读 search/read 证据已发布；KnowledgeView、深度子任务、写操作未做 | #1503–#1511 #1515 #1524 | 不一致：`CONTRACT-READONLY-v1.md:3` 仍写「评审通过前不授权任何实现」 | **拆分**：KB-01a 已接受；KB-01b 提议 | 前半已发布，后半未开工 |
| ADR-016（docs/）消费 @auraaihq 包 | 「进行中（migration 分支活跃）」`docs/ADR-016-consume-auraai-packages.md:5` | 未实现，方向已被取代：所有 `package.json` 零 `@auraaihq/*` 依赖；迁移分支不存在；唯一残留 `packages/node-daemon/src/module-discovery.ts:26` | #45（打捞入库，提交信息自称「superseded in direction」）#1371 | 不一致 | **取代**：改「被替代（ADR-026 §6；ADR-037 决策 1 REV-01 完成清理）」，加别名 ADR-016P | 代码零消费、分支不在、ADR-026 已收缩 |
| ADR-029 内核↔领域 OS 边界 | 「采纳（边界原则）」，后半句「挂载缝为实现工作项」`decision.md:1082` | 挂载缝已实现 | — | 局部过时 | 状态行补一句「挂载缝已实现」 | — |
| ADR-033 decide 在 L3 | 「采纳」，正文「agent24d 不感知这个 crate」`decision.md:1369` | agent24d 已用它服务 `/api/v1/decide/profile` | — | 局部过时 | 状态行补「D0-3 起 agent24d 接入」 | — |
| 日志 ADR-001/006/012/013/014/015/016、ADR-024 | 「✅ 采纳」 | 被事实取代 | 见 §1.B.6 | 不一致 | **取代**（逐条写取代者） | 见 §1.B.6 |
| 日志 ADR-005/008/009/021、ADR-DOC-03 | 「✅ 采纳」/「Accepted」 | 从未实现 | — | 不一致 | **废弃**（005/008/009/021）/ **搁置**（DOC-03） | 见 §1.B.6、Q7 |

下面 §1.A 是演进类（037、038、040–043）的逐项明细，§1.B 是记忆、知识、ADR-016 与其余 ADR 的逐项明细。



### 1.A 演进类明细：ADR-038、040–043、037

判定口径：**已实现** = 代码路径存在且接线到产品路径；**部分** = 有代码但未接线、未覆盖全部条款，或与正文语义有出入；**未实现** = grep 零命中或只有文档。ADR-040–043 由 #1530（merge `e5a1c671`）一起入库，之后正文只有 `f40702e3`（评审修订）与 `e6ee1896`（索引）两次文档提交。

### 1.A.1 ADR-038 组件契约 v1

#### 1.A.1.1 状态原文
- `docs/adr/ADR-038-component-contract-v1.md:5`：「状态：**草案 r3**（2026-11-01）。r2 按 ADR-039 修订…；r3 按 Fable 评审修订」
- 但正文已经写到 r4（`:356` §7「r4 补充（2026-11-02…）」）和 r5（`:439`「信任假设（r5，评审 M1 / L3）」）。**状态行落后正文两个版次。**
- 索引 `docs/adr/README.md:37` 记为「草案（原文：草案 r3）」。
- 最后一次修改：`e299c943`（2026-11-02，#1552）。

#### 1.A.1.2 相关 PR（逐片）

| 片 | PR | 关键提交 |
|---|---|---|
| C1a / C1b'（iDoris） | #1539 `feat/component-c1`（merge `7e9bc3b1`） | `0d009848` C1a：ComponentLock schema 2、通用下载器、签名验证、依赖反转；`b9c7bf51` spctl 改 `--type install`；`aa0f09f4` iDoris 锁升 schema 2、迁托管根；`33878fa2` Install / AtRest 两种验签模式 |
| C3a | #1545 `feat/component-c3a`（`77790807`） | `045d1333` 宽限统一 15 s、env 改名；`f78a17cc` `GET /api/v1/components` 与上报路由 |
| C2（仅 Hyphae 侧） | #1546 `chore/hyphae-v0.29.0`（`612f0f0f`） | `b3f75f1b`「Hyphae lock 升到 v0.29.0（560d2bd1，含 C2 contract/health）」，只升锁 |
| C3b | #1549 `feat/component-c3b`（`01f49e3a`） | `d9906854` 主进程上报 OD；`2a77236b` 桌面页面改读统一组件状态 |
| C1b（Hyphae） | #1551 `feat/hyphae-v0.30.0-signed`（`af4b1603`） | `69c2674e` Hyphae 锁换成签名版 v0.30.0（schema 2）、接通 D2a；`2d97f696` 验签失败单独错误码、锁不能关掉公证 |
| §7.11 拒绝信号 | #1552 `feat/hyphae-rejection-signal`（`e8b58246`） | `db635718` 启动验签拒绝信号；`cccba2a4` §7.11 改为 code-page 哈希路线 |

#### 1.A.1.3 决策逐项实现度

| 决策 | 实现度 | 证据（file:line） | PR |
|---|---|---|---|
| **D1** 范围（所有运行形态） | 部分 | `agent24-components/src/state.rs:29-45` 定义了 `Form::{Resident,Sidecar,Cli,Attach,Remote,Mcp}`；`component_routes.rs:372-379` `collect()` 只汇总 idoris、hyphae、attach、documents、knowledge、open-design。**MCP、领域 OS（除 documents）、进程内 DomainModule 不在列表里**（`component_routes.rs:39-40` `BUILTIN_COMPONENT_IDS` 中也没有） | #1545 |
| **D2** `ComponentLock` schema 2 | 已实现（有订正） | `lock.rs:12-38` 结构体与字段（`provides`/`requires`/`assurance`/`stop_grace_ms`）；`lock.rs:13` `deny_unknown_fields`；`lock.rs:181-183` 非 2 的 schema 一律拒绝；`lock.rs:239-243` `executables` 必填且 ⊂ `files`（§7.7）；`lock.rs:254` macOS 资产必须有 `signing`；`lock.rs:268-290` `release_url_base` 限定 github.com；锁文件 `agent24-comm/hyphae.lock.json:61`、`agent24-idoris/idoris.lock.json:2` 都是 `"schema": 2`；iDoris L1–L6 已经进 `assurance`（`idoris.lock.json:39-46`） | #1539、#1551 |
| D2 托管根 `~/.agent24/components/<id>/<version>/` | 已实现 | `data_dir.rs:6-9`（macOS/Linux = `$HOME/.agent24`）；Hyphae `hyphae_install.rs:86-87` `state_dir.join("components/hyphae").join(version)`；iDoris `agent24-idoris/src/install.rs:27-30` `data_dir.join("components").join("idoris")`，`server.rs:1928-1933` 接线；缓存 `hyphae_install.rs:204` `components/hyphae-cache`（§7.8）；目录 0700 + 属主校验 `storage.rs:184-215` | #1539 |
| D2 iDoris 迁移（§7.6/§7.9） | 部分 | `idoris_host/provision.rs:189-195` `legacy_deletable` 白名单，`:517-561` 只登记 `legacy-install.deletable.json`，不删；**`agent24 idoris gc` 子命令未找到**（`agent24-cli/src/main.rs` grep `gc` 零命中） | #1539 |
| D2 依赖反转（L3） | 已实现 | `agent24-idoris/Cargo.toml:9` 依赖 `agent24-components`；`agent24-components/Cargo.toml` 不依赖 idoris；`lib.rs:1-4` 注释「never the other way round」；`run_install_child` 在 `lib.rs:20` 导出 | #1539 |
| D2 「schema 1 要能读，先转换」 | 已取消（正文未改） | `lock.rs:3-4`「Schema 1 is not readable at all」；grep `migrate_schema1|fetch_verified` 零命中；`hyphae.release.json` 已删除（`ls` 不存在） | #1551 |
| **D2a** 签名验证 | 已实现 | `sign.rs:43-72` `signing_requirement`（非 darwin 返回 `None`；`notarized:false` 直接 `ManifestViolation`，`:62-66`）；`sign.rs:178-184` 团队要求含 Developer ID OID；`:241` bundle 用 `--deep`；`:256-258` Hardened Runtime 检查；`:264-268` 裸文件 `spctl --type install`、bundle 用 `execute`；`:277-284` 判定条件为 `accepted` 加 `source=Notarized Developer ID`；`:227-233` `check_platform` 放开到所有 macOS 架构；`:102-103` 工具超时 60 s。Install / AtRest：`sign.rs:21-25`；Hyphae 接线 `hyphae_install.rs:139-217`，`comm_routes.rs:365` AtRest；错误码 `comm_routes.rs:802-825` `hyphae_hash_mismatch` / `hyphae_signature_rejected` | #1539、#1551、#1552 |
| D2a 可复现比对 | 部分 | CI `.github/workflows/hyphae-lock-verify.yml:1-14`：linux 比 `sha256`，darwin 比 `unsigned_sha256`。「签名资产 = 复现构建重签」的 code-page 比对**未做**（ADR `:440` 自己写着「后续要补」） | #1551 |
| **D3** 契约版本握手 | 部分 | iDoris：HTTP `/health` 的 `contract_version` 校验已有（`agent24-idoris/src/lib.rs:236-245` `check_contract`、`supervisor.rs:950-961`），但失配映射为 `Failed`，不是 `incompatible`（`component_routes.rs:130-144` `from_idoris` 没有 Incompatible 分支）。Hyphae：锁里有 `contract`（`hyphae.lock.json:25-30`），Agent24 **从不调用 `hyphae contract`**（在 `agent24-comm/src`、`hyphae_install.rs`、`comm_routes.rs` 中 grep `"contract"` / `"health"` 子命令都零命中），契约只作为 `detail` 展示（`component_routes.rs:239-242`）。统一错误码 `component_contract_mismatch` **全仓零命中** | #1539（iDoris 早已有） |
| **D4** 绑定 | 部分 | HTTP（iDoris `/health`）已有；CLI `contract`/`health`：Hyphae v0.29.0+ 已提供（`b3f75f1b`），Agent24 侧没有接；`HYPHAE_OUTPUT=json` 与 envelope 解析沿用现有 runner；attach / os-proto 不变；进程内与 MCP 绑定未接入 `/api/v1/components` | #1546（只升锁） |
| **D5** 状态机 | 部分 | 状态集与转移表：`state.rs:7-22`、`:48-70`（与 ADR `:186-209` 逐行一致）；子集：`state.rs:72-110`。**`incompatible` 与 `degraded` 在 `component_routes.rs` 里没有任何来源产出**（grep `Incompatible|Degraded` 只命中 `MountOutcome::Degraded`→`Failed`，`:320`）；Hyphae 仍按 pid 判定（`:177-187`，注释写「no `health` before C2, so never degraded」）；Knowledge 没有探测（`:335-336`「There is no probe yet (C5)」）。映射函数 `from_sidecar`/`from_open_design`（`state.rs:113-139`）**在产品代码里没有调用**（只在 `state/tests.rs`），OD 的映射实际在桌面完成（`open-design-report.ts:86-93`） | #1545 |
| D5 宽限 15 s | 已实现（锁字段未用） | `lifecycle.rs:51-58` `GRACE_ENV="A24_COMPONENT_STOP_GRACE_MS"`、legacy 别名 `A24_MODULE_STOP_GRACE_MS`、`GRACE_DEFAULT_MS=15_000`；`shutdown_bounds.rs:10` `GRACE_MAX_MS=30_000`。锁里的 `stop_grace_ms` **没有消费方**（grep 只命中测试 `hyphae_install.rs:303`），与 §7.11 `:435` 一致 | #1545 |
| D5 `GET /api/v1/components` + 脱敏 | 已实现 | 路由 `server.rs:1453-1455`；Operation `route_policy.rs:67-68`、`operations.rs:54-55,141-142`；不在 creative allowlist（`operations.rs:80-94`）；脱敏 `component_routes.rs:67-76,106-124,386-392`（非 host 只给 `{id,version,state,provides}`） | #1545 |
| D5 OD 桌面上报 + 心跳 | 已实现 | `open-design-report.ts:11-12` `OPEN_DESIGN_HEARTBEAT_MS = 30_000`、上报路径；daemon 侧 TTL `component_routes.rs:35` 90 s，只聚合不拒收（`:463-500`）；只收 `DESKTOP_OWNED`（`:42`） | #1545、#1549 |
| D5 三个页面改读统一接口 | 已实现 | `renderer/capabilities/useCapabilityStatus.ts:3`、`pages/comm/Communication.tsx:31`、`components/idoris/IdorisPanel.tsx:3` 都 import `componentApi` | #1549 |
| **D6** 错误与取消 | 部分 | `$/cancelRequest` 已有（`os-proto/src/module.rs:285,407`）；`CALL_CANCEL_GRACE = 2s`（`lifecycle.rs:28`）。**CLI 超时不是「SIGTERM，2 s 后 SIGKILL」**：`agent24-comm/src/runner.rs:269-285` `kill_and_reap` 直接对进程组发 SIGKILL，`:603-609` 超时分支调用它；`reap_window` 2 s（`:414`）只是等待回收的时间 | 早于本 ADR |
| **D7** 授权 = `requires` ∩ grants ∩ 同意 | 未实现 | `lock.rs:33-34` 只做了解析；在 `rust/apps`、`rust/crates` 的非测试代码里 grep `.requires` / `requires.kernel` / `.assurance` 零命中，没有任何授权路径读取它 | — |
| **D8** 升级与替换 | 未实现（流程条款） | 契约兼容用例 §3 / 「mock 第二实现」（§3 第 7 条）：**未核实**，没有找到对应测试文件 | — |
| **D9** 清单 / 能力 id | 部分 | `provides` 已经从锁读出并暴露（`component_routes.rs:93-104`）；能力 id 字符集检查 `lock.rs:191-199,291-295`。**未做**：`docs/capabilities/REGISTRY.md` 不存在（`ls` 报 No such file）；`lock.rs:293` 注释「Registered-id check (REGISTRY.md) belongs to the CI added in C7」；`skills: []` 占位不在锁结构（`deny_unknown_fields`，没有 `skills` 字段），也不在 API `Item`（`component_routes.rs:51-65`）；`summary`、`tools` 不对外输出（`Provide` 只有 `id`、`name`，`:44-49`）；`~/.agent24/components.json` 默认提供者、多提供者拒绝解析、期望 / 实际比对（`unavailable`/`incompatible`）都是零命中 | #1545 |

#### 1.A.1.4 迁移片进度（ADR-038 §4）

| 片 | 状态 | 证据 |
|---|---|---|
| C1 | 已完成 | #1539，见上表 |
| C1b | 已完成（Agent24 侧） | #1551；`hyphae.lock.json:62-65` `signing`，`:47-54` `unsigned_sha256` |
| C2 | Hyphae 侧完成，**Agent24 侧未做** | `b3f75f1b` 只升锁；Agent24 没有 `contract`/`health` 调用 |
| C3（a / b） | 已完成，除 degraded / incompatible 外 | #1545、#1549 |
| C4 AgentEar | 部分：已登记 attach 子集 | `component_routes.rs:272-300`；§3 用例 2/3/4：未核实 |
| C5 Knowledge | 未做 | `component_routes.rs:335-336` |
| C6 Documents D9 清单 | 未做 | `documents_item` 没有 `provides`（`component_routes.rs:304-333`） |
| C7 REGISTRY / MCP / 领域 OS 清单 | 未做 | REGISTRY 不存在；MCP 不在列表 |
| C8 OD | 上报已做；迁到 SidecarManager 未做 | `open-design-report.ts`；`SidecarManager` 未接产品路径（ADR `:63` 自述） |

#### 1.A.1.5 正文与实现的不一致（含正文自相矛盾）

| # | 位置 | 正文 | 实际 / 冲突 |
|---|---|---|---|
| 1 | `:5` | 状态「草案 r3」 | 正文已经到 r4 / r5（`:356`、`:439`） |
| 2 | `:137` | 「schema 1 锁文件要能读：先在加载时转换」 | §7.11 `:432` 已删除；代码 `lock.rs:3-4` 拒读。D2 正文没有回写 |
| 3 | `:143` D2a | 「不填时是 `files` 里所有 Mach-O 文件…」 | `:129` 和 §7.7 `:413-415` 已改为必填，代码 `lock.rs:239` 也是必填。同一 ADR 内两处冲突 |
| 4 | `:146` D2a | 「发布 CI 记录签名前的摘要」 | §7.4 `:392` 改为本地两次构建；正文仍是原句 |
| 5 | `:117-119` D2 示例 / D9 `:275` | `provides` 每项有三语 `name`/`summary` + `tools` | §7.11 `:433` 改为可选；Hyphae 锁只带 id（`hyphae.lock.json:32-45`），`tools` 为空 → D9「工具名对不上 → incompatible」无法判定 |
| 6 | `:129` 后 / `:126` | `signing` 必填规则「见 D2a」；`notarized` 在示例中可选 | §7.11 `:438` + `sign.rs:62-66`：macOS 上 `notarized:false` 直接拒绝。D2a 正文仍写「`notarized: true` 时还要验证公证」 |
| 7 | `:158`、`:290` | 失配统一报 `component_contract_mismatch`，状态 `incompatible` | 全仓零命中；iDoris 失配 → `Failed` |
| 8 | `:198-201`、§7.3 | 健康探测 → `degraded` | 没有任何来源产出 `Degraded`（Hyphae 无 health 调用，Knowledge 无探测） |
| 9 | `:229-232` 映射表 | `downloading`/`verifying` → installing（daemon 侧映射） | Rust `state.rs:126-128` 明确不映射这两个（注释「not in D5's table」）；实际由桌面 `open-design-report.ts:90-93` 映射。功能等价，但「C3 实现时用这张表」的落点换成了 TS，且 Rust 映射函数是死代码 |
| 10 | `:238`、`:115` | 宽限期「锁里可配（`stop_grace_ms`）」 | 锁字段没有消费方（§7.11 `:435` 已承认），只有全局 env |
| 11 | `:253` D6 | CLI 超时「先 SIGTERM，2 秒后 SIGKILL」 | `runner.rs:269-285` 直接 SIGKILL 进程组 |
| 12 | `:260` D7 | 授权按 `requires` 求交 | `requires` 未被任何授权代码读取 |
| 13 | `:277` D9、`:95` D1 | `skills` 占位 `[]`；「技能（ADR-040 待写）」 | ADR-040 已存在；`skills` 字段在锁里不合法（`deny_unknown_fields`）、API 没有 |
| 14 | `:41-46` §1 背景 | Hyphae「schema 1、无契约版本」，`fetch_verified` 借用 iDoris 锁 | C1 / C1b 后已不成立（背景未标注「r4 前现状」） |
| 15 | `:432` §7.11 | 「`agent24-comm` 不能依赖带 reqwest 的 `agent24-components`」 | 与 D2 `:131`「iDoris 和 Hyphae 都改用它」相比是边界收窄；代码 `agent24-comm/Cargo.toml:87-89` 只作 dev-dependency，符合 §7.11，但 D2 正文未改 |
| 16 | `:244` D5 | 路由登记 `AssuranceLevel::Everyday` | 两条路由都是 Everyday（`route_policy.rs:67-68`），report 另由 `host_only_route` 限 host（`component_routes.rs:513`）。一致，列出备查 |

#### 1.A.1.6 建议：**修订后接受**（出 r5 订正稿，再拍板为「已接受」）
理由：C1 / C1b / C3 已经按本 ADR 合入 6 个 PR，实现事实上已在执行，长期停在「草案 r3」只会让正文和代码继续分叉（上表 #2–#6 都是正文没有回写 §7 订正）。r5 应当：① 把 §7.1–§7.11 回写进 D2 / D2a / D4 / D5，删除自相矛盾的旧句；② 明确 D3 / D9 的未完成项（`component_contract_mismatch`、`incompatible`/`degraded` 产出、REGISTRY、`skills`）分别归 C2-Agent24 侧与 C7；③ D6 CLI 取消要么改代码（先 SIGTERM），要么把正文改成「直接 SIGKILL 进程组」并说明理由；④ D7 标为「C9 待接线」，否则 `requires` 只是摆设。

---

### 1.A.2 ADR-040 技能契约 v1

#### 1.A.2.1 状态原文
`docs/adr/ADR-040-skill-contract-v1.md:5`：「状态：**提议**（2026-11-01，三步评审终稿，待 jason 拍板）」。入库 PR #1530（`d4053f12`）。

#### 1.A.2.2 代码核查（命令）
- `grep -rniE "skill" rust --include=*.rs -l` → **零命中**（ADR `:14` 写「`rust/` 下 `skill` 零命中」，至今仍然成立）。
- `grep -rniE "SkillLoader|skill_registry|SKILL\.md|skill_manifest_invalid"`（rust、apps、packages 的 .rs/.ts/.tsx）→ 零命中。
- `find . -name SKILL.md`、`find . -type d -name skills`（不含 node_modules）→ 零命中。
- `apps/desktop/src`、`packages/*/src` 中 grep `skill`（不分大小写）→ 零命中。
- 可复用件确实存在：`agent24-memory/src/knowledge.rs:98,111` `propose`/`approve`（ADR D5 引用的收件箱）。
- 前置依赖：`docs/capabilities/REGISTRY.md` 不存在（ADR-038 C7 未做）。

#### 1.A.2.3 决策逐项实现度

| 决策 | 实现度 | 证据 | PR |
|---|---|---|---|
| D1 `SKILL.md` + frontmatter | 未实现 | 见 §1.A.2.2 的 grep | — |
| D2 不持权限、运行前依赖解析、固定版本绑定 | 未实现 | 依赖 ADR-038 D9 多提供者解析，该项也未实现（§1.A.1.3 D9） | — |
| D3 安装 / 激活 / 执行三阶段 | 未实现 | — | — |
| D4 不可变版本、默认更新、回滚 | 未实现 | `docs/specs/SPEC-SKILL-DISTRIBUTION.md` 存在（ADR `:7` 自述「设计定稿未实现」） | — |
| D5 评测与审批门 | 未实现（可复用件在） | `knowledge.rs:98-111` | — |
| D6 主动技能接受模式 | 未实现 | grep `AcceptMode|accept_mode` 零命中 | — |
| D7 能力中心「技能」栏 | 未实现 | 渲染进程无 `skill` 字样 | — |

#### 1.A.2.4 不一致清单
| # | 内容 |
|---|---|
| 1 | D1 `:38`「C7 之前加载器只接受 `requires: []`」：这是正确的降级，但 ADR-038 D9 的 `skills` 占位在代码里不存在（锁 `deny_unknown_fields`），D1 `:41`「组件附带的技能」目前没有载体 |
| 2 | `:14` 背景的事实（零命中、REGISTRY 不存在）到 HEAD 仍成立，没有过时 |
| 3 | 与 ADR-041 D3 / D6 相互依赖：主动技能（D6）是 041 研判的前提，041 又是 D6 的触发来源，两者都没有代码 |

#### 1.A.2.5 建议：**接受（作为设计约束），实施挂在 ADR-038 C7 之后**
理由：正文与 ADR-039（已接受，`ADR-039-concept-model.md:5`）规则 3 一致，评审记录完整，没有与代码冲突的地方（因为没有代码）。真正的阻塞是 ADR-038 C7（REGISTRY + `skills` 字段），所以拍板时应同时把 ADR-038 D9 的 `skills` 写成锁的合法可选字段。D6 主动技能建议标「v1 不做」，先只做 `manual`，这样与 041 解耦。

---

### 1.A.3 ADR-041 入站事件、准入与工单

#### 1.A.3.1 状态原文
`docs/adr/ADR-041-inbound-events-admission-and-work-items.md:5`：「状态：**提议**（2026-11-01，三步评审终稿，待 jason 拍板）」。入库 PR #1530。

#### 1.A.3.2 代码核查（rust、apps/desktop/src、packages/*/src、protocol 范围内的 grep 计数）

| 关键词 | 命中 | 说明 |
|---|---|---|
| `inbound_events` | 0 | — |
| `work_item` / `WorkItem` | 0 / 0 | — |
| `mandate` | 1 | `agent24d/tests/me3f_blackbox.rs:612` 注释「SPEC-mandated」，无关 |
| `subscriptions` | 0 | — |
| `AcceptMode` / `accept_mode` | 0 | — |
| `admission` | 430 | 全部是别的语义：`os_memory.rs`(86)、`domain.rs`(45)、`model_callback.rs`(33)、`agent24-store/src/run_workspace_admission.rs`(28) 等，是内存 / 模型 / 工作区准入，不是入站事件准入 |
| `policy_version` | 31 | `agent24-store/src/source_policy.rs:132` 等，是 K1-02 来源策略版本，不是 D4 的「准入策略版本哈希」 |
| store 迁移 | — | 最新为 `0033_runs_before_tool_result_tagging.sql`，没有 inbound / work_items / mandates 表 |

背景事实复核（行号有小漂移，内容仍然成立）：
- `ScheduleAction::AgentRun` 在 `agent24-protocol/src/types.rs:877`（ADR 写 `:875`）；`RunStatus` 在 `types.rs:412`（ADR 写 `:411`）。
- `plan_resume` 只恢复 AwaitingApproval：`agent24-agent/src/resume.rs:90`（ADR 写 `:87`）。
- Hyphae `POST /inbox/pull` 在 `agent24-comm/src/router.rs:183`（ADR 写 `:174`），仍是被动转发。
- nostr-bridge 去重仍是内存 `Set`，上限 10 000：`packages/nostr-bridge/src/inbound.ts:31,35`。

#### 1.A.3.3 决策逐项实现度

| 决策 | 实现度 | 证据 | PR |
|---|---|---|---|
| D1 统一入站事件入口 | 未实现 | `inbound_events` 0；五条路径仍各走各的（见 3.2 背景复核） | — |
| D2 来源职责 / D9 清单 `events` 块 | 未实现 | `ComponentLock` 无 `events` 字段（`lock.rs:12-38`，`deny_unknown_fields` 会拒绝） | — |
| D3 确定性准入 | 未实现 | 名单判定仍在桥内（nostr-bridge / wechat-bridge） | — |
| D4 接受模式与策略版本 | 未实现 | `AcceptMode` 0 | — |
| D5 工单 | 未实现 | `work_item` 0；`RunStatus` 未加 `work_item_id`（`types.rs:412`） | — |
| D6 授权依据 | 未实现 | `mandates` 0 | — |
| D7 订阅 | 未实现 | `subscriptions` 0 | — |
| D8 就绪 ≠ 交付 ≠ 验收 | 未实现 | 依赖 ADR-042 收据 | — |
| D9 停止 / 撤销 / 暂停 | 未实现（零件在） | run 取消用现有 `CancellationToken`；K1-03 撤销已有；工单层没有 | — |
| D10 数据流与 `GET /api/v1/work-items` | 未实现 | `operations.rs` 无对应 Operation | — |

#### 1.A.3.4 不一致清单
| # | 内容 |
|---|---|
| 1 | §1 表格与 `:24` 的行号已经漂移（`types.rs:875→877`、`:411→412`、`resume.rs:87→90`、`router.rs:174→183`），内容仍然正确 |
| 2 | `:39` 要求 ADR-038 D9 加 `events` 块，但 ADR-038 r4 / §7 没有吸收；而 `ComponentLock` 是 `deny_unknown_fields`，未来加字段要走 schema 演进（加可选字段属于 minor，但需要改解析器） |
| 3 | `:117`「所有新表都在 CORE-5 5f–5g 接线之后才进 main」：CORE-5 迁移协调器仍是 `pub const ENABLED: bool = false`（`agent24d/src/migration_coordinator.rs:20`），前置条件未满足 |
| 4 | 篇幅：一份 ADR 里含 4 张新表、10 条决策，跨来源适配、准入、工单、授权依据、订阅五个子系统 |

#### 1.A.3.5 建议：**拆分后接受**
理由：D1–D3（入站事件表 + 来源适配 + 确定性准入）独立有价值，可以先落地并替换 nostr-bridge 的内存去重（`inbound.ts:31-35`）；D5–D8（工单 / 授权依据 / 订阅 / 完成条件）依赖 ADR-042 收据和 ADR-040 主动技能，宜单独成篇、晚一步拍板。两部分都要等 `migration_coordinator::ENABLED=true` 之后再加表，拍板时应把这个门写成显式前置。

---

### 1.A.4 ADR-042 待执行动作与对账

#### 1.A.4.1 状态原文
`docs/adr/ADR-042-durable-actions-and-reconciliation.md:5`：「状态：**提议**（2026-11-01，三步评审终稿，待 jason 拍板）」。入库 PR #1530。

#### 1.A.4.2 代码核查

| 关键词 | 命中 | 说明 |
|---|---|---|
| `pending_actions` / `PendingAction` | 0 / 0 | — |
| `channel_receipts` | 0 | — |
| `reconcil` | 173 | 是别的语义：`agent24-memory/src/reconcile.rs:1-15`（MD-2c 文件双谱系对账）、TUI 重连对账等，不是动作对账 |
| `outbox` | 314 | Hyphae outbox 路由 `agent24-comm/src/router.rs:184-186`（`/outbox`、`/outbox/{event_id}/retry`、`/outbox/clear`，仍是用户入口）；`agent24-sidecar-host/src/outbox.rs:1`「Private fixed-slot output admission」，与本 ADR 无关 |
| `idempotency_key` | 61 | 全部是 Documents HTTP API 的幂等键（`protocol/openapi.yaml:2340,2433,2695` `idempotency_key_reused`；`packages/contract-tests/...documents*`），不是 D2 / D3 的通道或模块幂等声明 |

#### 1.A.4.3 决策逐项实现度

| 决策 | 实现度 | 证据 | PR |
|---|---|---|---|
| D1 待执行动作队列（同事务入队） | 未实现 | 0 命中；`schedule_fire` 仍是 `agent24-scheduler::deliveries` 原状态机 | — |
| D2 通道收据 + 幂等键 + 对账 | 未实现 | `channel_receipts` 0；Hyphae outbox 仍是权威，`/outbox/clear` 仍对外（`router.rs:186`），与 D2 `:48` 相反 | — |
| D3 模块工具窄例外 | 未实现（v1 本就只定边界，`:55`） | ADR-038 D9 没有 `provides[].tools[].idempotency_key`（`lock.rs:92-104`，`tools: Vec<String>`） | — |
| D4 不宣称 exactly-once | 不适用 / 文档条款 | — | — |
| D5 对外文案 | 未实现 | — | — |

#### 1.A.4.4 不一致清单
| # | 内容 |
|---|---|
| 1 | `:18` 写 Hyphae outbox 在 `router.rs:175-177`，现在在 `:184-186`（漂移） |
| 2 | `:91`「ADR-038 D9 `events.channels[].idempotency_key` 与 `provides[].tools[].idempotency_key` 见终稿 §6 建议修订」：ADR-038 未吸收；而且 `provides[].tools` 现在是字符串数组（`lock.rs:103`），挂不上每个工具的属性，需要改 schema |
| 3 | 依赖链：D1 的 `mandate_id`、`run_launch` 依赖 ADR-041 D5 / D6，041 未实现 |

#### 1.A.4.5 建议：**接受（边界条款），实施排在 041 工单之后**
理由：D3（K1-01 §2.6 的唯一窄例外、闭集条件）和 D4（不宣称 exactly-once）是对现有约束的澄清，现在拍板能防止后续 PR 越界，没有代价。D1 / D2 的表可以先只做 `channel_send` + 收据，先给 Hyphae 发送路径用，不必等工单；`mandate_id` 保持可空即可解耦。同时要给 ADR-038 提 schema 修订，把 `tools` 改成对象数组。

---

### 1.A.5 ADR-043 后台作业、并发与监督回路

#### 1.A.5.1 状态原文
`docs/adr/ADR-043-background-jobs-concurrency-and-supervision.md:5`：「状态：**提议**（2026-11-01，三步评审终稿，待 jason 拍板）」。入库 PR #1530，评审修订 `f40702e3`。

#### 1.A.5.2 代码核查

| 关键词 | 命中 | 说明 |
|---|---|---|
| `background_after_ms` | 0 | — |
| `job_wait` / `job_kill` / `job_output` | 0 | — |
| `ConcurrencySlot` / `run_slot` / `concurrency slot` | 0 | — |
| `oldest_waiting` / `stalled_work_items` | 0 | — |
| `job_id` | 447 | 全部在 Documents：`agent24-documents/src/jobs/runner.rs`(31)、`imports/*`、`openapi.yaml`(17)，是文档导入作业，不是 shell 后台作业 |
| `/health` 结构 | — | `agent24-protocol/src/types.rs:23-34` 只有 `status/version/backend/protocol_version/migrations`；`agent24d/src/server.rs:803-827` 恒返回 `"ok"`，**没有 `work` 条目** |

背景事实复核：
- `shell_exec` 预算 30 s：`agent24-tools/src/local.rs:508` `budget: Duration::from_secs(30)`，仍然同步。
- `module_tool_runtime.rs:620` `pending: AtomicBool` 仍是内部标志。
- Guardian 存在：`agent24-policy/src/guardian.rs:1-6`；`self_wake.rs` 存在；`eval/decide/` 下有 `recall_gate.jsonl`、`retain_intent.jsonl`、`tool_risk.jsonl`。

#### 1.A.5.3 决策逐项实现度

| 决策 | 实现度 | 证据 | PR |
|---|---|---|---|
| D1 后台作业（`jobs` 表、`background_after_ms`、`job_*` 工具） | 未实现 | 见 5.2 | — |
| D2 并发槽 1 + 1、预算、同资源写权 | 未实现 | 0 命中 | — |
| D3 `/health.work` 确定性指标 | 未实现 | `types.rs:23-34` | — |
| D4 独立审查器 | 未实现（Guardian 形态零件在） | `guardian.rs` | — |
| D5 六层评测 | 未实现（只有专项评测） | `eval/decide/*.jsonl`、`packages/documents-accept` | — |
| D6 不做的 | 符合（没有 Watchdog Session） | — | — |
| D7 对外 | 未实现 | — | — |

#### 1.A.5.4 不一致清单
| # | 内容 |
|---|---|
| 1 | D1 `:27`、`:95` 依赖 ADR-042 `deliver_input`，而 ADR-042 Q1（`ADR-042:96`）反过来说「`deliver_input` 可能先做 ADR-043 的作业再回答」，两份 ADR 循环依赖 |
| 2 | D3 `:41` 的指标 `oldest_waiting_s`、`failing_actions`、`stalled_work_items` 分别依赖 042 队列和 041 工单，D3 单独落地时只剩 `running_jobs`、`budget` |
| 3 | D3「随 CORE-5 5f 的 health 契约一起改」：CORE-5 5f 已经新增了 `DockedHealth`（`migration_guard/dock.rs:5-9`），但没有 `work` 字段，协调器仍 `ENABLED=false` |

#### 1.A.5.5 建议：**拆分**——D1 + D3（只含 `running_jobs`/`budget`）先接受；D2 / D4 / D5 留提议
理由：D1 解决的是有代码证据的真实问题（`shell_exec` 同步 30 s，`local.rs:508`），而且只依赖 `self_wake` 已证明的「完成事件回会话」路径，可以不经 ADR-042 先落一个最小版本，打破与 042 的循环依赖。D2 并发槽、D4 审查器依赖工单（041）与动作队列（042），现在拍板没有可验证的落点；D5 是评测方法论，适合并入 S6 计划，不必作为 ADR 决策。

---

### 1.A.6 ADR-037 产品形态与进程模型（`docs/decision.md`）

#### 1.A.6.1 状态原文
- `docs/decision.md:1505`「## ADR-037：产品形态与进程模型 —— agent24d 是本体，外壳都是客户端」
- `docs/decision.md:1508`「**状态**：🟡 提议（产品方向由 jason 2026-10-26 拍板；本文待 Codex 挑战与 clestons 评审，**批准前不派生产实现**）」
- 事实基线 `:1509`：main `3a433bb0`，见 `docs/tracking/2026-10-26/SOURCE-ADR-037-facts.md`
- 但其他已接受的条目已经按它修订：`decision.md:725`（ADR-020「自 ADR-037 起标注」）、`:832`（ADR-022 已收窄）、`:995`（ADR-027 部分取代）。代码里也直接引用它，例如 `data_dir.rs:1`「(ADR-037 decision 3)」、`scripts/tui-coverage.mjs:2`「TUI-1 / ADR-037」。**「批准前不派生产实现」与事实不符。**

#### 1.A.6.2 决策逐项实现度

| 决策 | 子项 | 实现度 | 证据 | PR |
|---|---|---|---|---|
| **1** 外壳只做客户端 | node-daemon 不进安装包 | 已实现 | `backend-manager.ts:139-150`「REV-01: desktop no longer depends on or builds @agent24/node-daemon」；`apps/desktop/package.json` 的 extraResources 只带 `agent24d`/`agent24` 和 `.open-design-component`（`:71-130`） | #1371（`ab/core-rev01-node-daemon-cleanup`） |
| | oMLX IPC 迁出桌面 | 未实现 | `apps/desktop/src/main/ipc/index.ts:442,460,470,486` 仍有 `OmlxDetect/Models/Start/Warmup` | — |
| | 渲染进程不借全权 token（路径白名单） | 已实现 | `apps/desktop/src/main/backend-proxy.ts:3,56,157-166` `ALLOWED_PROXY_ROUTES` | #1358 |
| **2** 常驻 daemon | macOS LaunchAgent | 已有（CLI 手动） | `agent24-cli/src/service.rs:1`；plist 参数 `serve --port 0 --ready-omit-token`（`service.rs:60-100`），**仍是 legacy 模式，没有 `--auth-mode capabilities --enroll-endpoint`** | 早于 ADR |
| | systemd `--user` / Windows 计划任务 | 未实现 | grep `systemd` 只命中注释（`os-proto/src/launch.rs:40`、`agent24-cli/src/main.rs:2927`）；`schtasks` 零命中 | — |
| | 桌面首次运行征得同意、安装服务 | 未实现 | 在 `apps/desktop/src/main` 中 grep `LaunchAgent|launchctl|service install|openAtLogin` 零命中；桌面仍以子进程拉起（`backend-manager.ts:387` `serve --port 0 --auth-mode capabilities --host-bootstrap-stdio`） | — |
| | 桌面退出不停 daemon、托盘「完全退出」 | 未实现 | `main.ts:488-497` `will-quit` → `backendManager.stop()`；托盘 grep「完全退出」零命中 | — |
| | TUI / CLI 不再默认 `--ephemeral` | 未实现 | `agent24-cli/src/main.rs:5`「no daemon found → spawn an ephemeral agent24d」；`:1005` `spawn_daemon(true)` | — |
| | 服务模式 `--enroll-endpoint` | 已实现（daemon 侧） | `agent24d/src/main.rs:161-165`；`server.rs:3437-3467` `StartupMode::Service`；`state_file.rs:57,112` `enroll_endpoint` | #1378（`ab/core-02e-service-mode`） |
| | `/health` 加协议版本与迁移水位 | 已实现 | `types.rs:30-33`；`server.rs:822-826`；桌面 `tryReuseExternal` 比对 `protocol_version`（`backend-manager.ts:292,305,497-503`） | #1301（`ab/core-04-health-version`） |
| | 迁移前快照、失败回滚、停靠态 | 部分（已写未启用） | `migration_guard/{snapshot,restore,replay,dock,startup}.rs`、`docked_router.rs`、`docked_startup.rs:1-11`；**`migration_coordinator.rs:20` `pub const ENABLED: bool = false`**；`daemon repair --retry-migration` 零命中 | #1428、#1528、#1540 |
| **3** 客户端分级 | 本机登记握手（UDS + 对端身份） | 部分（仅 macOS） | `enroll_endpoint.rs:1-23`（`enroll.sock`，Team ID `HYAP444FN9`，标识白名单）；身份只有 `IdentityKind::MacosSecCode`（`:46-63`）；全部 `#[cfg(unix)]`（`:69,79,169`），没有 Windows 命名管道和 Linux 可信哈希清单 | #1378 |
| | 日常级 / 提升级 + 在场证明 | 部分 | `capabilities/first_party.rs:1,20-21`（进程绑定日常令牌 TTL 3600 s，单次提升授权 60 s）；桌面确认通道 `presence_desktop.rs:1-10`。FIDO2 / 配对设备（headless）：未实现（零命中） | #1447（`feat/core2-agentear-s3`）等 |
| | 开发者令牌签发（`agent24 token create`） | 未实现 | CLI 子命令列表（`agent24-cli/src/main.rs:49-368`）没有 `Token` | — |
| | G-API-1 路由级 Operation 映射 | 部分 / 未核实全覆盖 | `capabilities/route_policy.rs` 逐路由登记（如 `:67-68`）；是否「每条路由」都有映射：未核实 | — |
| | G-API-2 Host / Origin 校验 | 已实现 | `server.rs:1709-1756`（缺 Host 或非回环 Host 拒绝；非 `null` 的 Origin 403 `forbidden_origin`） | #1364（`fix/pre-release-daemon-hardening`） |
| | G-API-3 OpenAPI 一致性 CI | 部分 / 未核实 | `docked_router.rs:685` 按 OpenAPI 校验；专门的「代码路由 = OpenAPI」CI：未核实 | — |
| | G-API-4 事件流按令牌过滤 | 未实现 | `events.rs:46-49` 「Broadcast an event to all connected clients」；`client_loop`（`:84-120`）不按主体或资源过滤 | — |
| | G-API-5 令牌治理 | 未核实 | — | — |
| | 平台数据目录契约 | 已实现 | `agent24-protocol/src/data_dir.rs:1-12`（`AGENT24_DATA_DIR` > `$HOME/.agent24` > `%LOCALAPPDATA%\Agent24`）；TS 镜像 `apps/desktop/src/main/data-dir.ts`、`packages/data-dir/src/index.ts`；共享夹具 `protocol/fixtures/data-dir/cases.json` | #1292（`ab/core-01-data-dir`） |
| **4** TUI 能力对齐 | 覆盖 CI | 已实现 | `scripts/tui-coverage.mjs:1-18`；`.github/workflows/ci.yml:234-235` | #1295（`ab/tui-01-coverage-ci`） |
| | 视图补齐（聊天、日程、记忆…） | 未实现 | `tui/app.rs:22-25` `Focus { Runs, Approvals }`；豁免表 `protocol/tui-coverage-exemptions.yaml`：`planned-tui-2` 20 条、`planned-tui-3` 23 条、`visual` 3 条（`openapi.yaml` 共 124 个 operationId） | — |
| **5** 平台顺序 | Mac 签名公证 | 部分 / 未核实 | `release.yml:41-51` 有 macOS 构建矩阵，但 `release*.yml` 中 grep `codesign|notarytool|notariz` 零命中（ADR 写的是在本机签名，见 ADR-038 §7.4 `:394`），CI 内的签名流程未核实 | — |
| | Linux（systemd、Secret Service、hw） | 部分 | `release.yml:59-67` 有 linux 构建；`agent24-hw/src/probe.rs:113-115` 有 Linux 分支；Secret Service 只在 `agent24-comm/Cargo.toml` 命中（细节未核实）；systemd 未实现 | — |
| | Windows | 部分 | `windows-check.yml`、`sidecar-windows.yml` 存在；`release.yml` 发布矩阵只有 macos / linux（`:41-67`）；`package.json` 有 `.exe` extraResources（`:49-53`）；登记端点没有 Windows 实现 | — |
| **6** 移动端经 Hyphae 遥控 | 设计文档 `REMOTE-CONTROL-HYPHAE.md` | 未实现 | `find docs -iname "*REMOTE*"` 无结果；全仓只有 `decision.md` 提到该名字 | — |
| | 配对、受限作用域、遥控命令 | 未实现 | 无相关代码 | — |

#### 1.A.6.3 与事实清单（SOURCE-ADR-037-facts）对照的变化
| 事实清单原文（基线 `3a433bb0`） | HEAD 现状 |
|---|---|
| §1.1「拉起命令 `serve --port 0`…不传 `--auth-mode`，即 legacy 单 token」 | 桌面在非 Windows 上默认 capabilities + host-bootstrap（`backend-manager.ts:135-137,387`） |
| §1.2「`Health` 只有 `status/version/backend`」（亦见 `decision.md:1543`） | 已加 `protocol_version`、`migrations`（`types.rs:30-33`） |
| §1.3「没有 systemd unit、没有 Windows service」 | 不变 |
| §1.4「capability 模式的 daemon.json 不含 token」 | 不变，另加 `enroll_endpoint`（`state_file.rs:57`） |
| `decision.md:1518`「常驻 daemon 目前没有任何路径给第二个客户端发令牌」 | 已有 `enroll.sock` 登记握手（macOS） |
| `decision.md:1518`「REST 没有 Host/Origin 校验」 | 已有（`server.rs:1709-1756`） |
| `decision.md:1520`「node-daemon 仍被打进所有平台的安装包」 | 已移除（#1371） |

#### 1.A.6.4 正文与实现的不一致
| # | 内容 |
|---|---|
| 1 | 状态「提议，批准前不派生产实现」与已合入的 #1292、#1295、#1301、#1364、#1371、#1378、#1428、#1528、#1540 等实现 PR 矛盾 |
| 2 | 背景段（`:1517-1520`）描述的是 `3a433bb0` 的事实，多条已经过时（见 6.3），没有加「截至基线」注记 |
| 3 | 决策 2 要求「产品默认走常驻」，代码路径相反：桌面仍以子进程 + stdin 管道拥有 daemon（`backend-manager.ts:384-390`），这正是 `:1531` 要降为「子进程模式退路」的那条路径；LaunchAgent 仍是 legacy 模式 |
| 4 | 决策 3 的登记握手正文写「三个平台各一份实现」，实际只有 macOS SecCode |
| 5 | 决策 2 的停靠态写得很细（标记文件、`daemon repair --retry-migration`），代码已写但 `ENABLED=false`，修复命令不存在 |

#### 1.A.6.5 建议：**修订后接受**（并拆出两份后续文档）
理由：决策 1、3、4 的骨架（数据目录、`/health` 版本、Host/Origin、登记握手、覆盖 CI、node-daemon 移除）已经按本文在 main 上落地 9 个以上 PR，继续挂「提议，批准前不派生产实现」既不真实，也让后来者无法判断哪些已定。建议：① 改状态为「已接受（决策 1–5），决策 6 待 `REMOTE-CONTROL-HYPHAE.md`」；② 背景改为「基线事实 + HEAD 差异表」；③ 把仍然空白、可以独立评审的 G-API-4（事件流授权）与「桌面默认改走常驻服务（含 LaunchAgent 切 capabilities 服务模式、`will-quit` 不停 daemon）」作为显式的下一步里程碑，后者是本 ADR 核心承诺里落差最大的一项。

---

### 1.A.7 未核实项（汇总）
- ADR-038 §3 验收用例是否逐条存在（尤其第 7 条「mock 第二实现」、第 11 条 SIGTERM / 15 s），以及 C4 AgentEar 的用例 2/3/4。
- ADR-037 G-API-1（是否每条路由都有 Operation 映射）、G-API-3（代码路由与 OpenAPI 一致性 CI 是否存在）、G-API-5（按令牌限流 / 配额 / 审计）。
- Linux Secret Service 的实际使用位置（只在 `agent24-comm/Cargo.toml` 有命中）。
- `git log --ancestry-path` 推断的「引入 PR」可能取到较早的集成 merge；ADR-038 各片的 PR 号由 merge 内提交列表直接确认（§1.2），可信度更高。


---


### 1.B 记忆、知识、ADR-016 与其余 ADR 的明细

路径都从仓库根写起；daemon 在 `rust/apps/agent24d/`。实现度分四档：已实现 / 部分 / 底座在、未接线 / 未实现。

### 1.B.1 ADR-MEMORY-P1：个人记忆生命周期与清除权

**状态原文**（`docs/adr/ADR-MEMORY-P1-LIFECYCLE-AND-PURGE.md:5`）：「**PROPOSED / 待外审接受，2026-10-26 修订（原 DRAFT 2026-10-23）。** 第 9 节五项已由产品负责人 jason 于 2026-10-26 拍板。本修订在精确 head 上被 clestons APPROVE，即作为 adr-s9 的外审接受记录。接受的是设计与首版范围，不批准任何迁移或生产实现」。外审接受记录是 PR #1247（`59d2130d`），见 `docs/agent/PLAN-MEMORY-P1.md:54`。

**发布情况**：`ab/memory-p1` 已经整体合入 main，即 PR #1341（`c98745b0`，2026-10-27）。之后 memory crate 只改过 FLAKY 修复和吸收 main 的合并（`7bf1d61a`、`abe154e2` 等），没有新的 P1 功能。

#### 1.B.1.1 决策逐项实现度

| 决策（ADR 节） | 实现度 | 证据 file:line | PR |
|---|---|---|---|
| 正常生命周期 no-loss，只有 purge 能做破坏性删除（§1:15） | **部分**：现有路径都不删除（EventLog、AssertionLedger 是 append-only），但 purge 这个例外本身还不存在 | `rust/crates/agent24-memory/migrations/0002_events.sql:1`（"append-only, immutable"）；`0004_assertions.sql:1`；`rust/crates/agent24-memory/src/wal_maintenance.rs:6`（"no row is deleted (those belong to purge/archive)"） | #114 #119 |
| 暂停 pause（§2 第 1 行，已有） | **已实现** | `rust/apps/agent24d/src/memory_routes.rs:263,291`（GET/PUT settings）；`rust/apps/agent24d/src/server.rs:1561-1563`；恢复时的来源门 `rust/crates/agent24-agent/src/lib.rs:64`（`source_unproven`）、`:1688`（`drive_resume`） | #720（M1），#1197 |
| 撤回 retract（§2 第 2 行，已有） | **已实现**：配额满和磁盘低时仍可用 | `memory_routes.rs:213`（`delete_assertion`）；测试 `memory_routes.rs:1102`（满额 204）、`:1126`（满盘 507）、`:1202`；`migrations/0019_retract_quota_exempt.sql:1-20` | #1275（c1），#1296（c4 disk gate） |
| 更正 supersede（§2 第 3 行，拟议） | **底座在、未接线**：账本支持 `supersedes` 和双时相，但 WriteGate 没有 supersede 路径，REST 也没有 | `rust/crates/agent24-memory/src/assertion.rs:12,185,269`；`eval_m1.rs:16`（"`WriteGate` has no supersede path yet"）；`protocol/openapi.yaml` 的 memory 路径只有 `:1863,1918,1974` 三条 | #119（底座） |
| 清除 purge（§2、§4–§6，拟议） | **未实现**：rust/ 下没有 purge、tombstone、恢复控制存储、fence 或删除世代的代码 | `grep -i purge` 在 memory crate 和 `memory_routes.rs` 里只命中注释（`lib.rs:564`、`wal_maintenance.rs:6`、`tests/p1_capacity_probe.rs:2` 的「不启动 purge」）；OpenAPI 没有 purge 路径 | — |
| 无痕 incognito（§2、§7） | **未实现**：rust/ 和 apps/desktop/src 里都搜不到 incognito 或「无痕」 | grep 结果为空 | — |
| 冷归档、JSON/Markdown 导出导入（§3、§6） | **未实现**：只有旧会话导入 `session_log.rs:168 import_legacy`，没有导出 | `PLAN-MEMORY-P1.md:58-59`（P1-04b、P1-05 全部 `[ ]`） | — |
| 容量与配额：0014 的默认值 200000 行 / 256 MiB（§3:48） | **已实现，与文中一致** | `migrations/0014_owner_usage_quota.sql:83`（`VALUES ('*', 200000, 268435456)`） | #207 |
| 配额失败要显式报告：配额满时不能说「已记住」，写失败要留痕（§1:15、§3:48） | **已实现**（P1-02c c1–c5） | `migrations/0020_write_rejections.sql:13,30`（`mem_write_rejections`、`mem_session_gaps`）；`src/disk_gate.rs:1-15`；`openapi.yaml:5163-5191`（`memory_receipt` 的 failed/not_saved/partial）；`src/wal_maintenance.rs` | #1275 #1283 #1284 #1285 #1296 #1302 |
| 「purge 控制记录要有独立保留空间」（§3:48） | **未实现**：0020 只放写失败留痕，不放 purge 意图 | `0020_write_rejections.sql:3-7` | — |
| 消息来源独立建表（§3:43、§9 末段） | **已实现** | `rust/crates/agent24-store/migrations/0027_*`、`0028_*`、`0029_run_message_origin_host_system.sql:7`；`rust/crates/agent24-store/src/message_origin.rs:228` | #1168 #1195 #1251 |
| H02 同秒审计 ID 碰撞（§8:103） | **已实现** | `rust/crates/agent24-memory/src/writer.rs:577`（`commit_with_audit_using_clock`）、`:1271`（测试） | #1152 #1153 #1154 #1263 |
| §9 ① 首版只做全个人域 ～ ⑤ 无痕首版限制 | **仅决策**，五项都只放开了「格式/设计」，没有对应代码 | `ADR:109-117`；`PLAN-MEMORY-P1.md:57-65`（P1-04～P1-09 全部 `[ ]`） | #1247（决策） |

#### 1.B.1.2 正文与实现不一致

| # | 正文 | 现状 | 证据 |
|---|---|---|---|
| a | 状态行写「PROPOSED / 待外审接受」，同一段又写「被 clestons APPROVE，即外审接受记录」 | 自相矛盾。ADR 索引已经点出这一点（`docs/adr/README.md` 核对记录） | `ADR:5`；`PLAN-MEMORY-P1.md:54` |
| b | §9⑤ 写 `append_snapshot_run_messages` 在 `agent24-agent/src/lib.rs` | 定义在 `rust/crates/agent24-store/src/message_origin.rs:228`，`agent24-agent/src/lib.rs:2200` 只是调用 | grep |
| c | §3 表头写「路径从仓库根起」 | 实际写的是 `agent24-memory/src/event.rs`、`agent24d/src/server.rs`，少了 `rust/crates/` 和 `rust/apps/` 前缀 | `ADR:33-46` |
| d | §3:43 和 §9 末段只列来源迁移 0027、0028 | 之后还有 0029（HostSystem）、0031（tool result origins）、0033；列表已过时 | `rust/crates/agent24-store/migrations/` |
| e | §3 把「可编辑 artifacts」「摘要」「工具正文 trace」都列为清除对象 | 这些 store 在产品路径上没有调用方：`.artifacts()`、`.consolidator()`、`.trace()` 在 rust/apps 和 agent24-agent 的非测试代码里各 0 处；只有 events、assertions、retriever 接了线 | `rust/crates/agent24-memory/src/lib.rs:529,588,601`；计数命令见 §3.1 |
| f | §1:13 说 H02、P02 红证待修 | 都已修复（见上表）。§8:103 已更新，§1 没有 | — |

#### 1.B.1.3 建议：**修订后接受，同时拆分**

- 把 §9 的五项决策和 §2 的操作契约定为「已接受」，状态行改成「已接受（设计与首版范围；不含迁移和实现许可）」。外审接受记录（#1247）已经有了，继续挂 PROPOSED 只会让人以为它还没过审。
- 拆出两块：
  - **已落地部分**：配额失败、撤回豁免、写失败留痕、磁盘门。这部分可以由 P1-02C 设计稿引用后单独收口。
  - **未落地部分**：purge、无痕、导出、更正。保留为「已接受、待实现」，按 PLAN 里 P1-04～P1-09 的门推进。
- 同步修正 1.2 里 b、c、d、f 四处过时引用。

---

### 1.B.2 ADR-MEMORY-RECALL-MESSAGE-ORIGINS（R05）

**状态原文**（`docs/adr/ADR-MEMORY-RECALL-MESSAGE-ORIGINS.md:5`）：「**PROPOSED / 待外审接受，2026-10-26 修订（原 DRAFT 2026-10-23）。** 本修订在精确 head 上被 clestons APPROVE，即作为 R05 的外审接受记录。接受的是设计，不等于 H03 全部验收」。接受记录是 PR #1248（`4d2d2100`）。

#### 1.B.2.1 决策逐项实现度

| 决策 | 实现度 | 证据 file:line | PR |
|---|---|---|---|
| 独立来源表 `run_message_origins`，复用 `(run_id,seq)`，不改用 K1 标签（§2） | **已实现** | `rust/crates/agent24-store/src/message_origin.rs:28,101,114`；`migrations/0029_*.sql:7`（kind 闭集含 HostSystem） | #1168 #1195 #1251 |
| 消息与来源同一事务写入（§3.1） | **已实现** | `message_origin.rs:228`；调用点 `rust/crates/agent24-agent/src/lib.rs:2200` | #1181 |
| 消息、来源、K1 标签三者同一事务（§3.1、§6） | **未实现**（ADR 自己也列为开着的门） | `lib.rs:2215,2232,2270,2327,2358` 是在 snapshot 之后单独调用 `tag_run_source` | 推迟 |
| 来源无法证明时整次拒发，含审批恢复（§3.5、§4） | **已实现** | `lib.rs:64`；`lib.rs:2488-2495`（缺行或读失败时 `return None`）；`source_send_gate_tests.rs` | #1197 #1251 |
| 按持久化的 kind 整形发送副本，不按 marker 文本判断（§1:13、§3.3–3.4，R05-5 4b） | **已实现**：`request_messages_for` 按存下来的来源决定处理方式，只对 HostRecall 按当前账本裁剪，其余 kind 逐字发送 | `lib.rs:2444-2537`（注释 "shaped by the origin kind PERSISTED … never by its text"）；`send_trim_tests.rs` | #1250 |
| 冷恢复矩阵测试（§5、§6） | **已实现** | `PLAN-MEMORY-P1.md:51` 列了 `cold_resume_*` 测试 | #1261 |
| HostSystem kind 及其写点：暂停通知（§6、§7①） | **已实现** | `migrations/0029_*.sql:2,7`；`lib.rs:72`；`snapshot_origin_tests.rs:94` | #1251 |
| 非 assistant 角色的回合拒绝（G3） | **已实现** | `docs/agent/R05-4-H03-CLOSEOUT.md:70` | #1205 |
| G1：把 `recall_ref` 当作成员证明前，先解析成 id 数组并对照账本 | **未实现**：`shape_host_recall` 仍从正文文本解析 `[id=`，用 `active_ids` 过滤，不读 `recall_ref` 里的 id | `lib.rs:556-570`；`lib.rs:2497-2500` 只检查 `recall_ref` 是 None 还是 Some | — |
| 诊断消费者读取来源绑定（PLAN R06-H03 c） | **未实现** | `PLAN-MEMORY-P1.md:52`（「代码里没有读取来源绑定的诊断入口」） | — |
| 旧 run 没有来源行时停发、不回填（§7②） | **已实现** | `lib.rs:2490`（非 Present 时 `return None`）；`0029` 注释「Existing unsourced rows are not backfilled」 | #1197 #1251 |

#### 1.B.2.2 正文与实现不一致（全部是代码已经走到前面、文档没跟上）

| # | 正文 | 现状 |
|---|---|---|
| a | §1:13「`request_messages_for` 当前按第一个 user message 的 prefix/end marker 判断……这一点今天仍成立；修复见 R05-5 第 2 节，尚未实现」 | 已由 #1250 实现（`lib.rs:2444-2537`）。marker 现在只在 HostRecall 内部用来解析 id（`lib.rs:556-561`） |
| b | §6:76「合并明细见 R05-4（PR #1245，head `968d2db3`，未合并）」 | #1245 已合并（`a9ba2ed3`） |
| c | §6「仍开着的硬门」列了「按来源裁剪」「冷恢复矩阵」「HostSystem 迁移与写点」 | 这三项已由 #1250、#1261、#1251 关闭。真正还开着的只有 K1 同事务、G1、诊断消费者，以及 G2/G4/G5（G2/G4/G5 有触发条件，见 R05-4 §3） |
| d | §2:23 只写了迁移 0027、0028 | 还有 0029（HostSystem） |
| e | §2 表中 HostSystem 一行写「待 4a 实现」 | 已实现 |
| f | 状态行同时写 PROPOSED 和 APPROVE | 与 ADR-MEMORY-P1 同样的矛盾 |

#### 1.B.2.3 建议：**修订后接受**

- 状态行改成「已接受（设计；#1248）」。
- 重写 §1 和 §6，改成「已落地 / 仍开着」两张表。仍开着的只列 K1 同事务、G1、诊断消费者和 G2/G4/G5，并写明各自的触发条件。
- G1 是唯一还碰发送安全的缺口：现在 id 仍从正文解析，没有对照 `recall_ref`。建议另开一份小 ADR 或单独一片处理，不再挂在本 ADR 名下。

---

### 1.B.3 ADR-028：记忆分层，「暂定 / 需 spike」（docs/decision.md）

**状态原文**（`docs/decision.md:1023`）：「🟡 暂定 / 需 spike（2026-08-21 经 Codex 对抗式复审后从"采纳 85%"下调……保留本地优先+无强制外部服务的约束，但**不冻结**当前层边界、两列"双时相"、全局文件权威、crate 拆分）」。

#### 1.B.3.1 决策逐项实现度（以 Codex 复审收口后的修订方向为准，`decision.md:1059-1075`）

| 决策 | 实现度 | 证据 file:line | PR |
|---|---|---|---|
| 三份持久权威之一：EventLog，append-only、带 origin/trust | **已实现，已接线** | `rust/crates/agent24-memory/src/event.rs:91,128,250,430`；`migrations/0002_events.sql:1`；产品调用 `.events()` 17 处 | #114 |
| 三份持久权威之二：ArtifactStore，CAS 版本 + 磁盘双血缘对账 | **底座在、未接线** | `src/artifact.rs:100,117`；`src/reconcile.rs:1-12`；产品调用 `.artifacts()` 0 处 | #115 #118 |
| 三份持久权威之三：AssertionLedger，双区间双时相 | **已实现，已接线** | `migrations/0004_assertions.sql:6-8,26-29`（`valid_from/valid_to` + `recorded_from/recorded_to`）；`src/assertion.rs:268,285`；产品调用 `.assertions()` 16 处 | #119 |
| 可重建投影：FTS | **已实现**：`FtsRetriever::rebuild` 在一个事务里清空、按账本重建，再清除 `needs_rebuild`；启动时如果标记还在就重建 | `src/retriever.rs:52-73`；`src/lib.rs:467-476`（`rebuild_fts_if_needed`）；`migrations/0017_assertions_fts_cjk.sql:15-16`（`mem_fts_state`）；测试 `retriever.rs:466,828` | #120，`7d08d5f7`（M1-T07a） |
| 可重建投影：向量，`Embedding{model_id,revision,dims,normalized,vector}` + 可续跑的 reindex | **底座在、未接线**：只有测试用的 `HashEmbedder`，`OmlxEmbedder` 标为 pending | `src/vector.rs:36-46,146,288`；`migrations/0007_embeddings.sql:15-23`；rust/apps 里没有使用 `VectorRetriever` 的地方 | #123 |
| 投影带 generation/checkpoint | **部分**：事件投影有 `mem_checkpoints`（0002、0010 按 owner 分）；FTS 只有一个 `needs_rebuild` 布尔标记，没有 generation；consolidator 写明「checkpoint-gated incremental pass is a future optimization」 | `migrations/0002_events.sql:28`；`0010_checkpoints_owner.sql:20`；`0017_*.sql:15`；`src/consolidator.rs:20` | #114 #128 |
| Condenser 拆成 EventStore/ContextSelector/ContextTransformer/ContextRenderer/RetentionPolicy | **未按修订实现**：只有一个 `Condenser` trait 加 typed `ContextFragment`/`ContextProjection`；修订里那几个类型名在 rust/ 下 0 处 | `src/condenser.rs:27,58,127,187,237` | #113 |
| 先在一个 crate 里定 trait，不拆 crate | **已实现**（仍是单个 `agent24-memory`） | `ls rust/crates`（没有 memory-core/episodic/…） | — |
| 强制非空 owner、能力受限 handle、delete≠invalidate（「三条硬的」） | **已实现** | ADR-030 F8（#140）；ScopedMemory（#139）；`migrations/0012/0013/0016`（分区目录） | #139 #140 |
| 自动写只用于可信用户话语 + 显式 remember | **已实现**（WriteGate + 审计） | `src/writer.rs`；`agent24-agent/src/retain.rs:216` | M1 #720 |
| 下一步：评测/恢复 spike（两个投影器、崩溃重放、语料），过了再冻结 trait | **spike 已完成**：MD-1b crash-replay、MD-1c LongMemEval harness，提交信息写了「freeze MD-1 signatures」；M1-T08 召回评测基线 | `src/replay.rs:1-10`；`src/eval.rs:1-20`；`src/eval_m1.rs` | #116 #117，`a75a0d75` |
| `memory rebuild` 写明能恢复什么、不能恢复什么 | **未实现**：CLI 里没有 `memory rebuild` 命令；重建只在启动时由内部标记触发 | `rust/apps/agent24-cli/src/main.rs` grep 为空 | — |

#### 1.B.3.2 正文与实现不一致

| # | 正文 | 现状 |
|---|---|---|
| a | 「决策」段（`decision.md:1033-1043`）仍写四层、每层一个 trait、拆 4 个 crate、「文件即真源」 | 已被同一 ADR 的收口段推翻，但原文没有标删除线或「已修订」，读者容易照旧文实现 |
| b | 状态写「需 spike」 | spike 早在 2026-08-22 合入（#116 #117），之后的 M1、P1 都按「权威 + 投影」在做 |
| c | 背景（`:1027`）说「现状只有两层：KvStore + CanonicalSession」 | 现状已是 EventLog/ArtifactCas/AssertionLedger/FTS/Vector/Consolidator/Trace/Instruction 共 20 个迁移 |
| d | 「嵌入默认本地 oMLX」 | 没有 oMLX embedder（`vector.rs:44-46` 写的是 pending D4b）；ADR-MEMORY-P1 §3:39 把向量定为 P2 N/A |
| e | SPEC-MEMORY 头部写「其余段落待 spike 后整体重写」 | 没有重写（`docs/specs/SPEC-MEMORY.md:6`） |

#### 1.B.3.3 建议：**修订后接受**（以收口段为正文；原「决策」段标为被取代）

- 接受的内容：三份权威 + 可重建投影 + 单 crate + 强制 owner + 双区间时相。这几条都有代码，也有 #116 #117 的 spike 证据。
- 继续不冻结的内容：Condenser 的拆分、投影 generation、向量/oMLX、`memory rebuild` 的对外语义。这几条写进「开放问题」。
- ArtifactStore 和 Consolidator 目前没有产品调用方。如果 P1 不打算接线，建议在 ADR 里明确写「底座保留，不在产品路径」，否则 ADR-MEMORY-P1 §3 的清除清单就在覆盖不存在的落点。

---

### 1.B.4 ADR-KB-01：知识能力进入 Agent24 唯一主对话

**状态原文**（`docs/knowledge/ADR-KB-01-SINGLE-CONVERSATION.md:5`）：「2026-10-11｜用户确认产品方向；工程契约/任务分配待评审，本 PR 仅更新设计与验收计划。」文件里没有状态标签，索引记为「未标」。

**发布情况**：ab/weknora 两次发布进了 main，即 #1396（`release/weknora-r1`，2026-10-27）和 #1515（`release/weknora-r2-20261030`，2026-11-01）。第二次包含 M1–M5：#1503 m1-backend、#1504 m2-tools、#1506 m3-desktop、#1507 m3b-chat、#1510 m5a-same-source、#1511 m5-accept、#1516、#1519、#1524。

#### 1.B.4.1 决策逐项实现度

| 决策 | 实现度 | 证据 file:line | PR |
|---|---|---|---|
| 只有一个主对话：知识问答在主对话里完成，不建第二个聊天 | **已实现**：Knowledge 打开时，主对话的回合改走 `/api/v1/runs`，run 绑定 binding | `apps/desktop/src/renderer/pages/chat-knowledge.ts:1-6`（decision 2026-10-30-main-chat-knowledge-path）；`openapi.yaml:5253,5265`（`knowledge_binding_id`） | #1506 #1507 |
| 简单检索返回证据，主回答由 Agent24 生成 | **已实现**：只有 `knowledge_search` 和 `knowledge_read` 两个工具，证据作为数据包裹 | `rust/apps/agent24d/src/knowledge_tools.rs:1-11`；`docs/agent/KB-M5-ACCEPTANCE.md:39-41`（E2–E4） | #1504 #1511 |
| 通过可信 adapter 走 REST，不直连 WeKnora 的 MCP | **已实现** | `rust/apps/agent24d/src/knowledge_backend.rs:1-12`（`HttpTransport`）；`rust/crates/agent24-knowledge/src/http.rs:146,191` | #1503 |
| 身份和范围来自已验证的宿主上下文；只有 ProductHost 能激活 binding；run 与 binding 的绑定只存在内存里 | **已实现** | `rust/apps/agent24d/src/knowledge_host.rs:1-31`；`openapi.yaml:1086`（`host_authority_required`） | #1503（KB-HOST-01） |
| Off、撤权、切换 profile 时停止在途调用 | **已实现**（以 activation 作为 generation，结果回来时 binding 或 C 变了就作废） | `knowledge_backend.rs:6-10`；`knowledge_tools.rs:7-11` | #1503 #1524 |
| 出口：LocalOnly 和获准的云处理分开授权 | **部分**：每次调用按 run 授权出口（`authorize_egress_if_restricted(HttpFetch, unknown)`），但 WeKnora 侧的解析、OCR、embedding 是否本地，宿主无法强制 | `knowledge_tools.rs:92,124` | #1504 |
| crate 内的 Provider 状态机（STATE/OPS 契约） | **底座在、未接线**（「The crate's Provider … is not wired yet」） | `knowledge_backend.rs:6`；`rust/crates/agent24-knowledge/src/provider/*` | — |
| KnowledgeView 只管资料、Wiki、版本、同步（KB-C2） | **未实现**：桌面只有一个设置页，填端点、KB id 和密钥 | `apps/desktop/src/renderer/pages/settings/KnowledgeSettings.tsx`；pages 目录下没有 Knowledge 页 | — |
| 深度知识子任务，由 WeKnora 执行器执行（KB-C3） | **未实现** | `agent24-knowledge/src` 里没有 executor 或 agent-chat 调用 | — |
| ContextPack、SOP 候选、写操作（OPS 契约，KB-C4） | **未实现**：只有 `provider/evidence.rs` 和 `provider/state.rs` 提到 pack 或 ingest | grep 结果 | — |

#### 1.B.4.2 正文与实现不一致

| # | 正文或相关文档 | 现状 |
|---|---|---|
| a | KB-01:26「API/schema 是拟议契约，不声称已交付」 | 只读 search/read 已经交付并发布（#1515） |
| b | `docs/knowledge/CONTRACT-READONLY-v1.md:3`「草案（Draft）…评审通过前不授权任何实现」 | 已有实现并发布，契约状态没跟上 |
| c | `docs/agent/PLAN-KNOWLEDGE-CONVERSATION.md:19` 写「KB-C1 / BACKLOG」 | KB-C1 的主路径（主对话 → search/read → 证据 → 引用）已由 M1–M5 完成 |
| d | KB-01:9 把 KnowledgeView 写成用户入口的一部分 | KB-R1 验收写明「这次发布没有 Knowledge 的界面入口」（`docs/agent/KB-R1-ACCEPTANCE.md:3`）；r2 只加了设置页 |
| e | 「深度子任务」写在用户流程里（:11） | 未做，也没有标明延后 |
| f | KB-M5 验收「结果」只写「见 PR 正文中贴出的 `report.md`」 | 仓库内没有结果，**未核实** |

#### 1.B.4.3 建议：**拆分**

- 拆成 **KB-01a「单一主对话 + 只读检索证据」**，标为**已接受**，证据是 #1503–#1511 和 #1515。
- 拆成 **KB-01b「KnowledgeView 管理页、深度子任务、写操作/ContextPack/SOP」**，标为**提议**，挂到 KB-C2～C4。
- 同时把 CONTRACT-READONLY-v1 的状态改成与实现一致，或者注明「实现先于评审」，并记一笔决定（2026-10-30-m1-thin-wiring）。现在这个文档写的是「评审通过前不授权任何实现」，而代码已经发布。

---

### 1.B.5 ADR-016（docs/）：迁移至消费 @auraaihq/* 包

**状态原文**（`docs/ADR-016-consume-auraai-packages.md:5`）：「**状态**: 进行中（migration 分支活跃）」，日期 2026-05-15（`:6`）。

#### 1.B.5.1 决策逐项实现度

| 决策 | 实现度 | 证据 | PR |
|---|---|---|---|
| Agent24 改为消费 `@auraaihq/*` 包（`:30`） | **未实现，方向已被取代** | 所有 `package.json` 里都没有 `@auraaihq/*` 依赖。workspace 包全部叫 `@agent24/*` 且 `private: true`（`packages/*/package.json:2,4`、`apps/desktop/package.json:2,5`）。desktop 只依赖 `@agent24/api-client`、`@agent24/data-dir`（`apps/desktop/package.json:33-35`）。`pnpm-workspace.yaml` 只有 `apps/*`、`packages/*` | — |
| 阶段 1C，PR7–PR12：Agent24 换用 idoris/core/memory/boxlite-client（`:109-116`） | **未实现**：要替换的 `llm-gateway.ts`、`capability-registry.ts`、`module-installer.ts`、`module-state.ts`、`server.ts` 仍在 `packages/node-daemon/src/`，而 node-daemon 本身已降为契约参考实现 | `ls packages/node-daemon/src`；`docs/ADR-026-rust-core-polyglot.md:229`；desktop 不再依赖 node-daemon（`6cb808ee` REV-01，PR #1371） | #1371 |
| 唯一残留：npm 模块发现把 `@auraaihq` 当官方 scope | 残留，只在 node-daemon 里 | `packages/node-daemon/src/module-discovery.ts:26`（`OFFICIAL_SCOPES = ['@auraaihq', '@agent24']`） | — |
| 分支 `migration/consume-packages`、tag `pre-migration-2026-05-15`（`:140-144`） | tag 在；分支本地不存在。本 ADR 是在 #45 时从该分支「打捞」回 main 的，提交信息明确写「superseded in direction by ADR-026's Rust core, kept for the decision record」 | `git tag` → `pre-migration-2026-05-15`；`git branch -a` 没有 migration 分支；`git show c367e195`（#45，2026-07-24） | #45 |
| ADR-026 §6 的「范围收缩」 | 收缩后各项在本仓库都已无消费者。sdk 摒弃，core 停止加码，idoris 降级（内核实际走 Rust `agent24-models/src/idoris.rs`、`router.rs`），memory 仅留 L0 KV（但没有 TS 消费者） | `docs/ADR-026-rust-core-polyglot.md:9,198-207` | — |

#### 1.B.5.2 不一致

- 状态写「进行中（migration 分支活跃）」，但分支已不存在，ADR-026（`:9`）已把它「范围收缩」，#45 的提交信息已写明「superseded in direction」。
- 背景（`:13`）引用的 `src/backend/*`、`src/shared/ipc-types.ts` 在当前布局下都不存在（`ls src` 报错；现在是 `apps/desktop/src/...`）。
- 与日志里的 ADR-016（模块安全）重号（索引「编号与冲突」已列出）。

#### 1.B.5.3 判断与建议：**已被事实取代，改为「被替代（by ADR-026 §6；ADR-037 决策 1 REV-01 完成清理）」**

- 理由：代码零消费；迁移分支已不在；Rust 内核接管了网关、模块生命周期和记忆；desktop 连 node-daemon 都不再依赖（#1371）。
- 建议在状态行注明唯一残留是 `module-discovery.ts:26` 的 scope 常量。要不要顺手清掉，交给 node-daemon 的维护者决定。
- 建议给它换一个不冲突的编号，或在文件名前加 `superseded-`，避免与日志 ADR-016 混淆。

---

### 1.B.6 其余 ADR 状态扫描

#### 1.B.6.1 docs/decision.md ADR-001～037（缺 026）

| ADR | 状态原文 file:line | 判断 | 证据 |
|---|---|---|---|
| 001 借鉴 xiaoheishu | `decision.md:14` ✅ 采纳 | **名义已接受，事实已废**：submodule 只是登记着，没有 checkout，也没有引用；desktop 是独立实现 | `.gitmodules:1-3`；`ls vendor/xiaoheishu` 为空；apps/ 里 grep xiaoheishu 0 处 |
| 002 / 003 模块化适配层、三层分类 | `:45` / `:76` ✅ 采纳 | 原则类 ADR；三层前缀在现行代码里没有体现（现在用 ADR-039 的能力/技能概念）。**未核实**是否已被 ADR-039 正式取代 | ADR-039 在索引里标「上位」 |
| 004 cos72 含 myshop/mytask/myvote | `:110` ✅ 采纳 | **本仓库未实现**：cos72 只作为测试夹具名出现；按 ADR-029 应是独立仓库的进程外 OS，**未核实** | `rust/crates/agent24-os-packages/src/discovery.rs:348`（测试） |
| 005 publishers 独立 npm 包 | `:136` ✅ 采纳 | **本仓库从未实现**（publish-* 0 处）；外部仓库**未核实** | grep 结果 |
| 006 npm scope 用 `@auraaihq` | `:166` ✅ 采纳（「所有包名 `@auraaihq/{name}`」`:182`） | **与事实不符**：本仓库所有包都叫 `@agent24/*`（private） | `packages/*/package.json:2` |
| 007 pnpm 混合 monorepo | `:189` ✅ | 已实现 | `pnpm-workspace.yaml` |
| 008 模型包只放 metadata | `:230` ✅ | **本仓库从未实现**：没有 `@auraaihq/models-*`，也没有 node-llama-cpp；模型选型已归 iDoris（ADR-036） | grep 0 处；`decision.md:1471` |
| 009 SkillBank/Evolver 两包 | `:269` ✅ | **从未实现**（0 处）；技能方向已由 ADR-040（提议）重新定义 | grep；`docs/adr/ADR-040-skill-contract-v1.md:5` |
| 010 / 011 | `:304` / `:336` ✅ | 过程类，不判断 | — |
| 012 / 013 iDoris-SDK 进 auraai-packages、改名 `@auraaihq/wechat-bridge` | `:355` / `:405` ✅ | **事实取代**：微信桥在本仓库重写为 `@agent24/wechat-bridge`（private），对接 agent24d v1 API；「iDoris」这个名字现在指模型网关（ADR-032） | `packages/wechat-bridge/package.json:2-5`（`aa7c48f7`，#78） |
| 014 M3 后 Agent24 被 skills-* 替代并归档 | `:442` ✅ 采纳（M3 执行） | **事实作废**：本仓库就是 Agent24 主产品（Rust+TS），ADR 里说的 skills/、hooks/、install.sh 都不存在；正文里的仓库名已被统一替换成 Agent24，前后指代混乱 | `ls skills hooks install.sh` 均报不存在；`apps/desktop/package.json:61` productName 是 Agent24 |
| 015 M3 后改名 | `:492` ✅ 采纳（依赖 014） | **事实作废或已完成，无法判定**：依赖的 014 没有发生；「去掉 Desktop」的意图在 ADR-027/037 里另有落地 | 同上 |
| 016（日志）模块安全：worker_thread + manifest 权限 | `:540` ✅ 采纳 | **被事实取代**：Node 模块加载器已从 desktop 删除（#1371）；现行权限模型是 K1-01/K1-03（`module_tool_runtime.rs`、`consent_routes.rs`）加 ADR-038 签名；keytar 和 worker_thread 都 0 处 | `rust/apps/agent24d/src/module_tool_runtime.rs`；`6cb808ee` |
| 017 轨迹默认本地 | `:571` ✅ | 原则；**未逐项核实** | — |
| 018 | `:603` ⛔ 被 027 取代 | 状态正确 | — |
| 019 LLM Gateway | `:691` ✅ | 已实现（Rust `agent24-models/src/router.rs`、`idoris.rs`） | `router.rs:614` |
| 020 | `:725` ⛔ 已取代 | 状态正确 | — |
| 021 Zero-CLI Onboarding（Ollama 下载向导、`onboarding-complete.json`） | `:767` ✅ 采纳（M2 实现） | **状态写已接受，但从未实现**：`ollama-manager`、`hardware-detect`、`onboarding/` 在全部 git 历史里都没有出现过；硬件探测改归 ADR-036 `agent24-hw`，模型归 iDoris | `git log -- '*ollama-manager*' '*onboarding*'` 为空；`rust/crates/agent24-hw/src/` |
| 022 默认 MLX（已收窄） | `:832` | 状态已更新，不展开 | — |
| 023 | `:877` ⛔ | 状态正确 | — |
| 024 工作流 asyncio.Queue + Step（Python） | `:917` ✅ 采纳 | **被事实取代**：ADR-026 写「ADR-024 部分 Superseded」（`ADR-026:8`），原计划承接它的 `agent24-workflow` crate（`ADR-026:82`）不存在；OpenAPI 里没有 `/api/v1/workflow/run`、`/api/v1/task/{id}`；没有 asyncio 代码。本篇状态行没改 | `ls rust/crates`；`grep '/api/v1/workflow' openapi.yaml` 为空 |
| 025 串行 LLM 推理 + 热切换 | `:965` ✅ | **未核实或未见实现**：agent24-models 里没搜到全局单槽推理门（grep `Semaphore::new(1)` 只命中 comm、enroll、documents）；内存账已归 ADR-036 | grep 结果 |
| 027 双壳（部分取代） | `:995` | 状态正确；Tauri/Expo 壳仓库里 0 处（只有 `apps/desktop`） | `ls apps` |
| 028 | `:1023` 暂定 | 见 §1.B.3 | — |
| 029 内核↔领域 OS 边界 | `:1082` ✅ 采纳；「`DomainModule` 挂载缝为实现工作项」 | 状态行的后半句已过时：挂载缝已实现（`agent24-domain/src/lib.rs:1402`、`agent24d/src/domain.rs:742`）；正文 `:1128-1131` 已写「进程内编译这一支已被推翻」 | — |
| 030 / 031 / 032 / 035 / 036 | 已接受 | 抽查有实现：F8 #140；`agent24 acp`（`rust/apps/agent24-cli/src/main.rs:1079`）；`idorisStatus`（`apps/desktop/src/renderer/components/idoris/api.ts`）；`agent24-hw` 接到 `agent24d/src/hardware.rs` | — |
| 033 agent24-decide 在 L3，「agent24d 不感知这个 crate」 | `:1369` ✅ | 第 3 点已过时：D0-3 已经让 agent24d 使用它（`rust/apps/agent24d/src/decide_routes.rs:14`、`server.rs:1484` `/api/v1/decide/profile`）。这是计划内的下一阶段，建议在状态行补一句 | — |
| 034 DecisionLog 由 agent24d 实现 | `:1398` ✅ | 与 ADR 一致，停在 D0-2：store 侧 `rust/crates/agent24-store/src/decision_log.rs:1-12` 已有，agent24d 侧还没有 `DecisionLog` 实现（grep 0 处），ADR 也写明留给 D1 | — |
| 037 产品形态，agent24d 是本体 | `:1508` 🟡 提议，「批准前不派生产实现」 | **状态落后于实现**：已经开工并合入 main。#1301（ab/core-04-health-version）给 `/health` 加了 `protocol_version`/`migrations`（`agent24-protocol/src/types.rs:15-30`、`openapi.yaml:4991`）；CORE-4 外壳校验协议版本（`427f3cdd`）；#1323 迁移保护设计；停靠标记 `rust/apps/agent24d/src/migration_guard/dock.rs:10,24`（`70f1e9a1`）；REV-01 删 node-daemon 依赖 #1371；ab/core 合入 main #1484。建议：**尽快评审定为接受**，或在状态行写明哪些决策已获准先行实现 | 同左 |

#### 1.B.6.2 docs/adr/（K1、038–043）

| ADR | 状态原文 | 判断 | 证据 |
|---|---|---|---|
| ADR-038 组件契约 v1 | `docs/adr/ADR-038-component-contract-v1.md:5`「草案 r3」（2026-11-01） | **草案状态，但 C1/C3 已实现并合入**：`agent24-components` 有 ComponentLock schema 2、下载器、签名验证 | `rust/crates/agent24-components/src/lib.rs:1-7`；PR #1539（component-c1）、#1545（c3a）、#1549（c3b），都在 2026-11-02 |
| ADR-039 概念模型 | `:5` 已接受 | 不展开 | — |
| ADR-040～043 | 都是 `:5`「提议（…待 jason 拍板）」 | 与事实一致：rust/ 下没有 skill version、work item、durable action、background job 的实现（grep 0 处） | grep |
| ADR-K1-01～04 | Accepted | 有实现：`module_tool_runtime.rs`、`consent_routes.rs`、store 迁移 0015–0025 | `ls rust/apps/agent24d/src`；`rust/crates/agent24-store/migrations/` |

#### 1.B.6.3 docs/documenting/adr、docs/open-design-workspace/design

| ADR | 状态原文 | 判断 | 证据 |
|---|---|---|---|
| ADR-DOC-01 / 02 | Accepted | 有实现：`rust/apps/agent24-documents/`（authorization、oplog、read、imports…） | `ls rust/apps/agent24-documents/src` |
| **ADR-DOC-03 DOCX 引擎（SuperDoc headless）+ LibreOffice 转换** | `docs/documenting/adr/ADR-DOC-03-docx-engine-and-conversion.md:5`「Accepted（2026-10-08）」，附条件「#692 格式矩阵冻结前……以 §6 的重新评估条件为准」 | **已接受但从未实现**：documents 的引擎只有 PDFKit（`rust/apps/agent24-documents/src/engine.rs:1-4`「slice 1's is the PDFKit helper」，`engine/` 下只有 `pdfkit.rs`）；rust/ 下 grep superdoc、soffice、libreoffice 都是 0 处 | 同左 |
| ADR-OD-001～006 | 「Accepted / P0 frozen」（如 `ADR-001-INTEGRATION-BOUNDARIES.md:5`） | 有实现：`agent24 acp`（`agent24-cli/src/main.rs:1079`）、`rust/apps/agent24-sidecar-host/`、`apps/desktop/src/main/creative-serve-web.ts`、OD pin（#1555）。没有逐条核对 | — |

#### 1.B.6.4 汇总：状态需要更正的条目

| 类别 | 条目 |
|---|---|
| 写着提议或未标，实际已有接受记录或已实现 | ADR-MEMORY-P1（#1247）、R05（#1248）、ADR-028（spike #116/#117 已完成）、ADR-KB-01（#1515 已发布）、ADR-037（#1301/#1371/#1484 已开工）、ADR-038（C1/C3 已合入） |
| 写着已接受，实际已被事实取代 | 日志 ADR-001、006、012/013、014/015、016（日志）、024；docs/ ADR-016（状态写进行中） |
| 写着已接受，实际从未实现 | ADR-005、008、009、021、DOC-03；025 未核实 |
| 状态行局部过时 | ADR-029（挂载缝已实现）、ADR-033（agent24d 已接入） |


---

## 2. 编号冲突的整理方案

### 2.1 现状与受影响引用数（`git grep -I`，HEAD 9865d7a0）

| 冲突 | 命令 | 结果 | 说明 |
|---|---|---|---|
| ADR-016 重号 | `git grep -I -E 'ADR-016' \| wc -l` | **27 行 / 11 个文件** | 其中指向 docs/ 那份（消费 @auraaihq 包）的：`docs/ADR-026-rust-core-polyglot.md` 第 9、198、200、229、240、252 行共 6 行，加 ADR 索引 2 行、`protocol/module.schema.json:5`（同时点名两份）1 行；按文件路径链接的只有 3 处（`git grep -n 'ADR-016-consume'`：ADR-026:252、adr/README.md:97、:151，另 schema 描述里 1 处）。其余都指日志里的「模块安全与权限模型」（`docs/decision.md:537`）：decision.md 1068/1073/1226/1232、`docs/PLAN.md:402`、`docs/ROADMAP.md:153`、`docs/specs/SPEC-MD-ME.md:280`、`docs/specs/SPEC-ME3-OUT-OF-PROCESS.md:801`、`README.md:90`、`docs/architecture/README.md:30`。「阶段 3 签名 + AirAccount」对应日志 ADR-016 的「M3 增强版」（`docs/decision.md:555-558`），docs/ 那份的「阶段 3」是 MCP Server 暴露（`docs/ADR-016-consume-auraai-packages.md:119`），所以 ROADMAP/SPEC 这几处判为日志版 |
| 日志 ADR-001–006 与 OD ADR-001–006 | `git grep -I -E 'ADR-00[1-6]\b' \| wc -l` | **137 行 / 41 个文件** | 按目录：`docs/open-design-workspace/design` 26、`docs/adr/README.md` 20、`docs/agent/PLAN-OD-NEXT.md` 19、`docs/ARCHITECTURE-LAYERS.md` 11、`docs/decision.md` 10、knowledge 9、documenting 8、tracking 4、reference-notes 4、代码/契约 4（`protocol/openapi.yaml:6432`、`packages/api-client/src/openapi.d.ts:3278`、`rust/apps/agent24-documents/migrations/0004_upload_chunks.sql:8`、`rust/crates/agent24-documents-client/src/import.rs:3`），另 `apps/desktop/src/main/documents-import.ts:3` |
| 其中带文件名链接的 OD 引用 | `git grep -I -E 'ADR-00[1-6]-[A-Z]' \| wc -l` | **23 行 / 10 个文件** | 这些链接已经无歧义，改号时**不能动文件名** |
| 其中裸写、需要靠上下文判断的 | 人工逐条（见下表） | 约 60 行 | 见 §2.2 |
| 初稿 ADR-040–044 与最终 040–043 错位 | `git grep -I -c -E 'ADR-04[0-4]' -- docs/research/deepseek-superagent` | 初稿目录 6 个文件共 38 行；三份演进稿 57 行 | 链接到 `adr-drafts/` 路径的 17 行 / 9 个文件。Codex 稿（`SUPERAGENT-EVOLUTION-2026-11-01-codex-astra.md:109,145`）还有**第三套**编号（「ADR-040—ADR-044」「ADR-044：有界并行…」），终稿里「立 ADR-043 记忆版本化」（`SUPERAGENT-EVOLUTION-2026-11-01.md:73`）用的也是 Codex 的编号 |

### 2.2 裸写 ADR-00x 的归属（抽查判定）

| 位置 | 原文 | 判定 |
|---|---|---|
| `README.md:271`、`docs/ROADMAP.md:31` | 「ADR-001 ~ ADR-030」「ADR-001~025」 | 日志（且范围已过期，日志到 ADR-037） |
| `docs/PLAN.md:80` | ADR-003、004、006、007 | 日志 |
| `docs/agent/roadmap.md:108`、`docs/design/ME4-S3-os-sdk.md:9`、`docs/reference-notes/macro.md:233,329,340,365` | ADR-004 的 myshop/mytask/myvote | 日志（cos72） |
| `docs/ARCHITECTURE-LAYERS.md:132,205,209,214,245,277,279` | ADR-002/004/005/001 | OD（同文 136、173、201、290 行带 OD 路径） |
| `docs/agent/PLAN-OD-NEXT.md` 19 行 | ADR-001/002/004/005 | OD（`:13` 明说指 open-design-workspace） |
| `docs/knowledge/PLAN-KNOWLEDGE-TASKS.md:18,31,80,137,163`、`POSITIONING-AND-DIFFS.md:56,72,82` | ADR-004（非持久分区） | OD |
| `docs/knowledge/POSITIONING-AND-DIFFS.md:11`、`ADR-KB-01-SINGLE-CONVERSATION.md:46` | fork ADR-001 | **外部**（WeKnora 仓库的 ADR-001） |
| `docs/documenting/BASELINE.md:214,246,287,373` | ADR-002 §6/§10、ADR-005 | OD |
| `docs/adr/ADR-039-concept-model.md:34,70` | 「ADR-004 的目标形态」= scratch workspace | **未核实**。ADR 索引核对记录（`docs/adr/README.md:183`）已指出与日志 ADR-004、OD ADR-004 都对不上；从内容看更接近 OD ADR-002（workspace）或 `ARCHITECTURE-LAYERS.md:212` 的「每个 Open Design 项目绑定一个 scratch workspace」 |
| `docs/documenting/adr/ADR-DOC-01-placement-and-integration.md:136`、4 处代码/契约注释、`apps/desktop/src/main/documents-import.ts:3` | 「只传 base name，不传路径（ADR-001）」 | **未核实**。索引核对记录（`docs/adr/README.md:185`）倾向 OD ADR-001；本轮没有在 OD ADR-001 里找到对应条款原文 |

### 2.3 方案：只加别名，不改文件名，不改日志编号

原则：**已有的文件链接一个都不断**；规范名只在新写的文字里使用；旧文字「下次修改时顺手改」（与 ADR-039 §4.1 的旧文档规则一致）。

| 冲突 | 规范名（今后新文档必须写） | 具体动作 | 不动的东西 | 需要改的引用数 |
|---|---|---|---|---|
| 日志 ADR-001–037 | 保持 `ADR-0xx` 不变，是全局编号的主人 | 无 | 日志全部编号 | 0 |
| OD ADR-001–006 | **`ADR-OD-001` … `ADR-OD-006`**（索引已在用） | ① 在 6 个 OD 文件标题下加一行「规范编号：ADR-OD-00x（文件名保留旧号以保持链接）」；② `docs/open-design-workspace/design/README.md` 加同样说明；③ 把 2.2 表里判为 OD 的裸写引用（约 45 行）在下次修改各文件时改成 `ADR-OD-00x` | 6 个文件名、23 处文件名链接 | 立即：7 个文件头；渐进：约 45 行 |
| docs/ADR-016（消费 @auraaihq 包） | **`ADR-016P`**（P = packages）；或者在接受 B 节的「取代」建议后直接标「被 ADR-026 取代」，以后不再被引用 | ① 文件头加「规范编号：ADR-016P；与日志 ADR-016（模块安全）无关」；② ADR-026 第 9、198、200、229、240、252 行改写成 ADR-016P（6 行，一个文件）；③ `protocol/module.schema.json:5` 描述里的两处点名分别写清 | 文件名 `ADR-016-consume-auraai-packages.md` | 1 + 6 + 1 = 8 行 |
| 初稿 ADR-040–044 | **`DRAFT-F040` … `DRAFT-F044`**（F = Fable 初稿）；Codex 稿编号写 **`DRAFT-C040` …** | ① 每份初稿标题前加「初稿编号 DRAFT-F04x，最终版见 …」；② 终稿 `SUPERAGENT-EVOLUTION-2026-11-01.md:30,73` 把 Codex 编号的「ADR-043」写成 `DRAFT-C043`，避免与最终 ADR-043（后台作业）撞名 | 初稿文件名、17 处 `adr-drafts/` 链接 | 立即：5 个初稿文件头 + 终稿 2 行 |
| 无数字编号的记忆 ADR | 保持文件名即标识 | 可选：给 `ADR-MEMORY-RECALL-MESSAGE-ORIGINS.md` 加别名 `ADR-MEMORY-R05`，对应其标题第一行的「R05」 | 文件名 | 0（可选 1） |

**防回归（建议，不在本 PR 做）**：在 CI 加一个只报告不阻断的 grep：`docs/` 下（除 `docs/decision.md` 和 `docs/open-design-workspace/`）新增裸写 `ADR-00[1-6]` 时提示写成 `ADR-OD-00x` 或带链接。新号统一从 `docs/adr/` 下一个空号（ADR-044）起发，ADR 索引是唯一发号处。

---

## 3. 缺失的 ADR

结论先行：
- 通讯（Hyphae/Nostr）共抽出 **31 条已定决策**（C01–C31），其中 C20、C21、C22 只在 PR / 提交 / 代码注释里，设计文档没收；**19 个未决或矛盾的问题**（X1–X19），其中 X1（随包还是按需）、X7（两套身份）、X11（入站执行入口）必须在写 ADR 前裁掉。
- 通讯之外核实了 22 条候选（O1–O22）：建议**新开 ADR** 的 3 条——O3 iDoris 由 agent24d 托管、O9 Knowledge 宿主接口 KB-HOST-01（建议 ADR-KB-02）、O18 SPEC-EDITIONS 版本边界；**并入现有 ADR** 的 6 条——O1 OD 按需下载并入 ADR-038，O4 CORE-2、O5 CORE-5、O11 预览版签名公证、O12 Windows 移植并入 ADR-037，O7 Guardian 默认关闭补进 ADR-026 §6.5；其余已有 ADR 覆盖或不需要 ADR。
- 通讯 ADR 建议编号 **ADR-044**（`docs/adr/` 下一个空号，见 §2.3）。提纲见 §3.A.3，只列章节、要回答的问题和要引用的证据，不写结论。


### 3.A 通讯（Hyphae / Nostr）

#### 3.A.1 已做出的决策

| # | 决策 | 出处（file:line） | 代码是否落地（file:line） | 相关 PR |
|---|---|---|---|---|
| C01 | 通信服务放在 agent24d 内（模块 `comm`，路由 `/api/v1/comm/*`），CLI 和桌面 UI 都是这套 REST 的客户端 | `docs/design/COMM-HYPHAE.md:12`（D1）；`docs/design/HYPHAE-CLI-INTEGRATION.md:30-31` | 已落地：`rust/crates/agent24-comm/src/router.rs:169-186`；`rust/apps/agent24d/src/comm_routes.rs`；路由权限 `rust/apps/agent24d/src/capabilities/route_policy.rs:142-160` | #612（COMM-0 设计，提交 fc83fe16）、#614、#622、#627 |
| C02 | Hyphae daemon 是 agent24d 的子进程，固定传 `--notify=false --auto-reply=false`，并显式传 `--relay` | `COMM-HYPHAE.md:13`（D2）、`:307-311` | 已落地：`rust/crates/agent24-comm/src/daemon.rs:720-731`。**和文档不一致**：文档 `:308` 要求的 `--watch-interval 30 --retry-interval 60` 代码里没有传 | #626、#642、#645（COMM-4a） |
| C03 | 用专用 HOME `<state_dir>/comm/hyphae-home/`；旧 `~/.hyphae` 只能经显式 import **复制**过来 | `COMM-HYPHAE.md:14`（D3）、`:229-254`（§4.1） | 已落地：`daemon.rs:741`（HOME 和 cwd 都设为 home）；`import.rs:144`（staging）；`import.rs:17-22`（会探测 `daemon.lock`） | #635（COMM-2b） |
| C04 | keystore 口令存在系统钥匙串里，只经 `--password-stdin` 传，不进 argv、env 或配置。service 为 `ai.idoris.agent24.comm`，account 为 `hex(sha256(salt))`。`A24_COMM_PASSWORD_STORE=memory` 只给测试和联调用 | `COMM-HYPHAE.md:15`（D4）、`:62`、`:331-344` | 已落地：`password_store.rs:33`；`comm_routes.rs:419-446` | #621（COMM-1b）；内存开关 #628（见 `docs/comm/JOINT-ROUND2.md:3-5`） |
| C05 | F4b 冻结：白名单发来的入站消息默认不触发 run，设 `A24_NOSTR_F4B_INBOUND=1` 才恢复。只拦截分派这一步，轮询和 canary 照常。解冻后只分派启动之后到达的消息 | `COMM-HYPHAE.md:16`（D5）、`:379-386`；`docs/specs/F4-nostr-channel.md:239-245`；`HYPHAE-CLI-INTEGRATION.md:73` | 已落地：`packages/nostr-bridge/src/config.ts:39-41,62`；`inbound.ts:206,221-245,249-257`；`main.ts:59-84` | #613、#618（COMM-5a） |
| C06 | 基础通信 zero-run，并加结构约束：comm 依赖图有白名单，禁止 HTTP 客户端 crate，`CommState` 不含 Store、ModelRouter 等 | `COMM-HYPHAE.md:16`、`:351-369` | 已落地：`rust/crates/agent24-comm/tests/comm_dependency_allowlist.rs:104,119,164`；`tests/comm_zero_run.rs` | COMM-5b 的 PR 号未核实；#1286（修 zero-run 测试偶发失败） |
| C07 | Hyphae 版本不写死，记在 lock 清单里，用 sha256 校验，不看 `--version`；改 lock 的 PR 要写明原因 | `COMM-HYPHAE.md:17`（D6）、`:416-435` | 已落地：`binary.rs:26`（用 `include_str!` 嵌入 lock）；`rust/crates/agent24-comm/hyphae.lock.json`；CI `.github/workflows/hyphae-lock-verify.yml` | #614、#621；之后的 lock 升级：#1193、#1352、#1368、#1431、#1546、#1551 |
| C08 | 保留 `A24_SPEAKER_BIN`（会打 warn），新增 `A24_HYPHAE_BIN`。查找顺序：HYPHAE → SPEAKER → 组件目录（重新校验 hash）→ 同目录；不查 PATH | `COMM-HYPHAE.md:19`（D8）、`:151` | daemon 侧已落地：`comm_routes.rs:251-300`；`agent24-cli/src/service.rs:186-189`。**nostr-bridge 没落地**：`packages/nostr-bridge/src/config.ts:45` 只读 `A24_SPEAKER_BIN`，默认值是 `agent-speaker` | #1489（HY-1） |
| C09 | 任务编号用 COMM-*，和 Hyphae 的 T20-A / T20-B / T21 / T22 对照 | `COMM-HYPHAE.md:18`（D7）、`:394-404` | 流程约定，不涉及代码 | #612 |
| C10 | 改 keystore 的命令统一经 `KeystoreWriteLock` 串行执行（H3） | `COMM-HYPHAE.md:158` | 已落地：`keystore_lock.rs`；`tests/keystore_write_lock.rs` | #621 |
| C11 | 错误有一个封闭集合并映射到 HTTP 状态码；`partial` 原样透传 data；send 不自动重试，也没有「重发」，只能按原 event_id 走 outbox retry | `COMM-HYPHAE.md:170-185`、`:284-292` | 已落地：`error.rs:30,79`；send、retry 的路由在 `router.rs:181,185` | #627（COMM-3） |
| C12 | 消息状态分四层。L3「对端确认」只占位，UI 永远不显示「已送达」 | `COMM-HYPHAE.md:258-267`；`HYPHAE-CLI-INTEGRATION.md:34` | 已落地：`router.rs:1610`（返回 `layer` 字段）；在 `Communication.i18n.ts` 里 grep「delivered/送达」0 命中 | #627；COMM-6/7 的 PR 未核实 |
| C13 | daemon 状态拆成 process、relay_probe、catch_up 三项分别给出；catch_up 永远不报「完成」 | `COMM-HYPHAE.md:269-280` | 已落地：`daemon.rs:5,627` | #633（COMM-4b） |
| C14 | 退避沿用 RestartPolicy（500 ms 起翻倍，5 次后熔断）；孤儿进程按 pid 加启动时间识别；关机时并入 SHUT-1b，记录名 `comm.hyphae` | `COMM-HYPHAE.md:312-328` | 已落地：`restart_policy.rs:2,29`；`daemon.rs:1910`；`rust/apps/agent24d/src/server.rs:2806,2828` | #626、#642、#645、#1100 |
| C15 | Hyphae 不打进 DMG，运行时按需下载（HY-1）。桌面页提供「下载并设置」并弹原生确认框；安装属于 `ComponentInstall`、提升级 | `COMM-HYPHAE.md:152,156`；R4 `:462` | 已落地：`comm_routes.rs:635`（`/install`）；`route_policy.rs:142`；`apps/desktop/src/main/ipc/index.ts:632-634` | #1489（HY-1）、#1496（HY-2） |
| C16 | 发布信息只有一个来源：逐字节复制上游 schema 2 的 `ComponentLock`。macOS 安装时做全量验签和公证检查，每次启动做离线 codesign；Linux 只靠 sha256 加可复现构建 | `COMM-HYPHAE.md:153`；`docs/adr/ADR-038-component-contract-v1.md:145-147,324`（C1b） | 已落地：`rust/apps/agent24d/src/hyphae_install.rs:1-6,126-170,173-246`；`hyphae.lock.json:46,61-65` | #1551（提交 69c2674e）、#1552 |
| C17 | `source != "config"` 时当作未配置，daemon 启动时总是显式传 relay。默认 relay 改为 Hyphae 自建的 `wss://relay.aastar.io`（2026-11-01） | `COMM-HYPHAE.md:459`（R1） | 已落地：`daemon.rs:729-731`；`apps/desktop/src/renderer/pages/comm/relayDefaults.ts:1-6`；设置 relay 前弹原生确认 `ipc/index.ts:937` | #1529 |
| C18 | Windows 在 DEP-A7 之前一律返回 `unsupported_platform` | `COMM-HYPHAE.md:460`（R2） | 已落地：`error.rs:30` | 未核实 |
| C19 | 新增 `POST /comm/inbox/pull`，供手动或没有 daemon 的场景拉取新消息 | `COMM-HYPHAE.md:199,210-219` | 已落地：`router.rs:183` | #627 |
| C20 | 提升级的 comm 写操作改走主进程专用 IPC，并做严格校验；加联系人要原生确认。**send 没有在场证明**，这条残余风险被接受，等 CORE-2d / 7c 再收口 | **只在 PR 和代码注释里**：提交 b9d81e55（H1）；`ipc/index.ts:850-878` | 已落地，残余风险也照原样保留 | #1535 |
| C21 | 私钥备份和恢复用 NIP-49 `ncryptsec`；备份文件由 daemon 自己写（`O_EXCL|O_NOFOLLOW`，0600） | **只在 PR 里**：提交 b9d81e55、fb254c18、5f32e55d。COMM-HYPHAE 里 grep `ncryptsec/NIP-49` 0 命中；只有 `COMM-FRIENDS.md:24` 作为现状提到 | 已落地：`export.rs:1-25`；`router.rs:171-172` | #1535 |
| C22 | 接入 Hyphae 群聊（COMM-G1）。这一项在 COMM-0 里是非目标 | **只在提交里**：cac4c796。`COMM-HYPHAE.md:29` 把群聊列为非目标 | 已落地：`groupchat.rs:1-17,59-61` | #1377 |
| C23 | 好友机制（Q1–Q6 已拍板）：协议归 Agent24（明文前缀 `agent24.friend/v1`，走 kind 30078 + NIP-44）；默认不告诉对方被拒；裸 Hyphae 对端回一条消息就算同意；已有联系人默认算好友；v1 存本地并纳入备份；不做 NIP-05 | `docs/design/COMM-FRIENDS.md:5,291,295-300`；状态机 `:63-82`；协议 `:108-130` | **没落地**：grep `friends.json` 和 `agent24.friend` 都是 0 命中；路线图上排在 ADR-038 C1–C3 之后（`:342`） | #1537 |
| C24 | 好友不等于可以指挥我的 agent：好友表不写进 `A24_NOSTR_ALLOWED_NPUBS`，协议消息也 zero-run | `COMM-FRIENDS.md:220-225` | 没落地（属于 C23） | #1537 |
| C25 | F4：不 vendor 源码，只驱动二进制（subprocess + `--json`）；信封固定三个（say / announce / listen），意图是开放枚举；内容格式为 `f4/1` | `F4-nostr-channel.md:44,70-79,83-87,91-104` | 已落地：`packages/nostr-bridge/src/speaker.ts:1-2`；`protocol.ts`（没有逐行核对） | 未核实 |
| C26 | F4：每条 agent msg 加唯一 `d` 标签（方案 A，由上游实现） | `F4-nostr-channel.md:120` | 在 Hyphae（agent-speaker）上游，Agent24 里没有对应代码 | 上游 agent-speaker；Agent24 侧未核实 |
| C27 | F4：入站改读 `history inbox --as`，用真实的 hex event_id 去重 | `F4-nostr-channel.md:122,139` | 已落地：去重在 `inbound.ts:58-66` | 未核实 |
| C28 | F4：对外只发布业务能力，不发原子工具；首次启动默认注册，默认开 | `F4-nostr-channel.md:158-160,202` | 已落地：`main.ts:38-42`；`bridge.ts:31-51` | 未核实 |
| C29 | 移动端只做遥控，经 Hyphae 配对连到用户自己的 daemon；普通 Hyphae 消息仍然不触发任何动作 | `docs/decision.md:1605-1612`（ADR-037 决策 6） | 没落地；`docs/design/REMOTE-CONTROL-HYPHAE.md` 不存在（find 0 命中） | #1249 |
| C30 | Hyphae 收件箱和 nostr-bridge 改成入站适配器，写入 `inbound_events`（ADR-041 状态为提议） | `docs/adr/ADR-041-inbound-events-admission-and-work-items.md:18-19,34` | 没落地 | #1530 |
| C31 | 两仓分工：Hyphae 管身份、加密、history/outbox、协议和发布；Agent24 管打包、兼容晋升、凭据、安装 API、UI 和进程监管 | `docs/Deployment/HYPHAE-NETWORK-DELIVERY.md:9` | 部分落地（同 C01–C16） | #881（DEP-HN0） |

#### 3.A.2 尚未决定、或文档之间相互矛盾的问题

| # | 问题 | 证据 |
|---|---|---|
| X1 | **Hyphae 是随包交付还是按需下载，两份文档说法相反** | `HYPHAE-NETWORK-DELIVERY.md:10`（「随包交付的进程外 sidecar」）、`:25`（标准包含 hyphae）、`:31`（「client 不依赖首次联网下载」） vs `COMM-HYPHAE.md:152,462` 和 #1489（「不打入 DMG」，运行时下载） |
| X2 | lock 格式在同一份文档里自相矛盾 | `COMM-HYPHAE.md:418-427`（§8.1：schema 1、含 `null`）vs `:153`（schema 2）；实际文件 `hyphae.lock.json:61` 是 `schema: 2`；`ADR-038:130` 规定不写 `null` |
| X3 | 下载器、缓存路径、版本号的描述已经过时 | `COMM-HYPHAE.md:154` 写的是 `fetch_verified`、`idoris/component-cache/hyphae`、`v0.27.2`；代码是 `hyphae_install.rs:199-204`（`download_locked_asset`、`components/hyphae-cache`），lock 版本是 `v0.30.0`（`hyphae.lock.json:72`）。提交 69c2674e 已经删掉 `fetch_verified`，但 `docs/ARCHITECTURE-LAYERS.md:153,317` 还写着「借用 iDoris 下载器」 |
| X4 | daemon 启动参数和文档不一致 | `COMM-HYPHAE.md:307-308` vs `daemon.rs:720-731`：代码没传 `--watch-interval` 和 `--retry-interval` |
| X5 | F4b 的拦截点写错了位置 | `COMM-HYPHAE.md:380` 说在「`InboundBridge.handle` 开头」；实际判断在 `pollOnce`（`inbound.ts:206,221`），`handle`（`inbound.ts:58`）本身不看这个开关 |
| X6 | COMM-5a 要求 nostr-bridge 支持 `A24_HYPHAE_BIN`，但没有实现 | `COMM-HYPHAE.md:394`、D8 `:19` vs `config.ts:45` |
| X7 | **两套 Nostr 身份并存，「agent 的身份」到底是哪一个没有定** | nostr-bridge 不覆盖 HOME（在 `speaker.ts` 里 grep `HOME` 0 命中，运行时实际用哪个 HOME 未核实），用自己的白名单 `A24_NOSTR_ALLOWED_NPUBS`；comm 用专用 HOME。ADR-041 把这个问题列为 Q4 开放问题（`ADR-041:133`） |
| X8 | 正文进 argv（G9）这个已知限制已经能解掉，但没跟进 | `COMM-HYPHAE.md:164,473` 记为已知限制；`COMM-FRIENDS.md:127` 说 v0.28.1 已有 `--content-file`；代码 `router.rs:1673` 仍然传 `--content` |
| X9 | 和 Hyphae 有关的差距（G1–G9）状态没有回写 | `COMM-HYPHAE.md:467` 说 lock 停在 `a4aa606`，没跟进 #104，但现在的 lock 已是 `0e0d4ea4`/v0.30.0；提交 b3f75f1b 说 v0.29.0 已带 contract/health，G2「没有结构化健康状态」也没有更新 |
| X10 | 「非目标」和实际做的事不一致；要不要公开广播能力，没有定 | 群聊列为非目标（`COMM-HYPHAE.md:29`），却由 #1377 做了；`profile publish` 是非目标（`:28`），nostr-bridge 却默认注册（`main.ts:38`，`F4:202`「默认开」）；`COMM-FRIENDS.md:125` 又明确说名片不走公开的 `profile publish` |
| X11 | **入站执行最终走哪条路，没有定** | F4 原设计是入站即走审批门后的 run（`F4:54,197-200`）；后来被 D5 冻结，执行改等「T01-E 高层授权」（`COMM-HYPHAE.md:26`）；同时 ADR-041（提议）和 ADR-037 决策 6（遥控）各自提出一个入口；`HYPHAE-NETWORK-DELIVERY.md:11` 写的是「不包含远程执行、任务委派」。T01-E 和 ADR-041 是什么关系，没有任何文档说明 |
| X12 | 默认用公共 relay，算不算「暗中回落」，没有定义 | `HYPHAE-NETWORK-DELIVERY.md:20`（「不暗中回落到公共网络」）vs 桌面预填 `relay.aastar.io`（`relayDefaults.ts:2`，#1529）。桌面设置前有原生确认（`ipc/index.ts:937`），但 CLI 和 headless 场景下的默认值没有定义 |
| X13 | comm send 是提升级，却没有在场证明（明确接受的残余风险） | `ipc/index.ts:853-857`；什么时候收口取决于 CORE-2d / 7c |
| X14 | lock 里的 `stop_grace_ms` 没有接线 | `hyphae.lock.json:70` 写的是 15000；提交 69c2674e 写明「stop_grace_ms 暂未接线」；`COMM-HYPHAE.md:320` 仍用环境变量 `A24_COMPONENT_STOP_GRACE_MS` |
| X15 | COMM-FRIENDS 要求在 COMM-HYPHAE §2 补一行「关系状态在 `comm/friends.json`」，还没补 | `COMM-FRIENDS.md:182` vs `COMM-HYPHAE.md:65-69` |
| X16 | 各文档引用的 Hyphae 版本基线不一样 | `COMM-HYPHAE.md:5`（a4aa606）、`COMM-FRIENDS.md:7`（v0.28.1）、`ADR-038:107`（44189b8e）、实际 lock 是 v0.30.0 |
| X17 | 消息层协议还没定：对端回执（L3）、NIP-17 gift wrap（藏起 `p` 标签）都等 T01 | `COMM-HYPHAE.md:27,264`；`COMM-FRIENDS.md:135,215` |
| X18 | COMM-6 / COMM-7（T21/T22 UI 和双仓联调）是否完成，没有证据 | log 里只找到「COMM6a 通信页」（b759245a）；`docs/comm/COMM6-API-ACCEPTANCE.md`、`COMM6-UI-ACCEPTANCE.md` 存在；COMM-7 的 PR 和验收记录未找到 |
| X19 | COMM-FRIENDS 遗留的开放问题：群聊里的好友语义、`allow_agent` 开关的审批等级、要不要进记忆 purge | `COMM-FRIENDS.md:348-351` |

#### 3.A.3 「ADR-0xx 通讯（Hyphae/Nostr）」提纲草案

只列章节、每节要回答的问题和要引用的证据，不写结论。

**0. 头部**
- 回答：状态写什么；上位 ADR 是哪几份（ADR-037 决策 1/6、ADR-038、ADR-039、ADR-041）；本篇取代或吸收哪些文档的「已定决策」节（COMM-HYPHAE §0，F4 §4–§6，COMM-FRIENDS §8）。
- 证据：`COMM-HYPHAE.md:3,8-19`；`COMM-FRIENDS.md:5-6`；`docs/adr/README.md` 第 6 节末句；`docs/architecture/README.md:56`。

**1. 背景与范围**
- 回答：「通信」在 ADR-039 里属于哪一类能力；两仓边界怎么划（C31）；哪些不在本篇范围（遥控命令格式、T01 协议、iDoris、Sin90）。
- 证据：`ADR-039:33,91`；`HYPHAE-NETWORK-DELIVERY.md:9-11`；`HYPHAE-CLI-INTEGRATION.md:74`。

**2. 决策 1：进程与服务归属**
- 回答：comm 放在 agent24d 内，CLI 和 UI 只是 REST 客户端；agent24d 监管 Hyphae daemon；桌面不 spawn Hyphae；和 ADR-037 决策 1 怎么对齐。
- 证据：C01、C02、C14；`COMM-HYPHAE.md:347-349`；`daemon.rs:720-745`；`server.rs:2806`。

**3. 决策 2：二进制来源、分发与信任**
- 回答：随包还是按需（先裁掉 X1）；lock 是唯一来源（schema 2，C16）；各平台信任根怎么定（macOS 签名加公证、Linux 只靠 sha256 加可复现）；开发者覆盖变量和查找顺序（C08）；nostr-bridge 要不要跟着改（X6）；升级 lock 的流程。
- 证据：`COMM-HYPHAE.md:151-157,462`；`HYPHAE-NETWORK-DELIVERY.md:10,25,31`；`hyphae_install.rs:173-246`；`comm_routes.rs:251-300`；`ADR-038:145-147,324`；`docs/comm/LOCK-UPGRADE-*.md`；#1489、#1551。

**4. 决策 3：数据归属与身份**
- 回答：专用 HOME，旧 HOME 只能显式导入；Hyphae 文件是消息的唯一来源，Agent24 只存关系状态；默认身份只有一个来源；**nostr-bridge 的身份和 comm 的身份怎么统一**（X7）；备份范围（NIP-49 私钥，联系人包）。
- 证据：C03、C21；`COMM-HYPHAE.md:65-69`；`COMM-FRIENDS.md:176-182`；`export.rs:1-25`；`ADR-041:133`。

**5. 决策 4：凭据与授权等级**
- 回答：口令放钥匙串、只走 stdin；降级开关；comm 读、写分别是哪一级（日常 / 提升）；send 没有在场证明这条残余风险什么时候收口；安装组件属于哪个 Operation。
- 证据：C04、C15、C20；`route_policy.rs:142-160`；`ipc/index.ts:850-878`；`docs/agent/CORE-2-ENROLL-DESIGN.md:49`。

**6. 决策 5：zero-run 与入站执行边界**
- 回答：基础通信 zero-run，用什么做可证伪的判据；F4b 冻结和解冻重放的边界；入站执行最终走 T01-E、ADR-041 准入，还是遥控入口（X11）；好友不等于授权。
- 证据：C05、C06、C24、C29、C30；`COMM-HYPHAE.md:351-386`；`inbound.ts:221-257`；`comm_dependency_allowlist.rs:104-165`；`COMM-FRIENDS.md:220-225`；`decision.md:1605-1612`；`ADR-041:18-34`。

**7. 决策 6：消息状态语义与可靠性**
- 回答：四层状态，永不显示「已送达」；部分成功怎么呈现；不重发、只按原 event_id 重试；daemon 三项状态互不推导；去重键用什么。
- 证据：C11–C13；`COMM-HYPHAE.md:258-292`；`HYPHAE-CLI-INTEGRATION.md:45-52`；`router.rs:1610`。

**8. 决策 7：网络与 relay**
- 回答：默认 relay 由谁决定，公共 relay 是否算「回落」（X12）；本机节点和服务器节点三种入口是不是产品目标；监听范围和 TLS 的底线。
- 证据：C17；`COMM-HYPHAE.md:459`；`relayDefaults.ts`；`HYPHAE-NETWORK-DELIVERY.md:14-31`；#1529。

**9. 决策 8：应用层协议与命名空间**
- 回答：F4 的三信封加开放意图是否仍然有效；`agent24.friend/v1` 前缀归谁；kind 30078 和 d 标签的约定；公开能力广播（profile publish / 默认注册）留不留（X10）；群聊算不算范围内（C22）。
- 证据：C23、C25–C28；`F4-nostr-channel.md:58-152,156-202`；`COMM-FRIENDS.md:106-135`；`groupchat.rs:1-17`；`main.ts:38-42`。

**10. 隐私与威胁模型**
- 回答：relay 能看到什么（`p` 标签是明文）；日志和 argv 里会出现正文明文（G9、X8）；屏蔽只在本机生效；同 UID 进程不在防护范围内。
- 证据：`COMM-HYPHAE.md:161-164,473`；`COMM-FRIENDS.md:207-216,166`；`router.rs:1673`。

**11. 平台**
- 回答：Windows 什么时候放开，env 白名单怎么补；macOS 钥匙串 ACL 和签名绑定带来的问题。
- 证据：`COMM-HYPHAE.md:460-461`；`error.rs:30`；`docs/design/WINDOWS-PORT.md:130`。

**12. 被否决的方案**
- 回答：每个否决项要写出理由出处。候选：Hyphae 原生好友状态机、用 kind 3 当好友、NIP-17、systemd 凭据、周期探测、日志轮转、`GET /comm/inbox`、靠 `--version` 校验。
- 证据：`COMM-FRIENDS.md:132-135`；`COMM-HYPHAE.md:32,209,493`。

**13. 后果、残余风险与待办**
- 回答：X2–X5、X8、X9、X14–X16 这些文档或代码偏差谁来订正；对 Hyphae 的配合请求（G1–G9、HY-F1–F6）。
- 证据：§3.A.2；`COMM-HYPHAE.md:455-473`；`COMM-FRIENDS.md:302-313`。

**14. 关联与取代**
- 回答：和 ADR-037 / 038 / 039 / 041 / K1-02 是什么关系；F4 spec、COMM-HYPHAE §0、COMM-FRIENDS §8 改成「实施说明」。
- 证据：`ADR-041:93`（入站文本的来源标签）；`ADR-038:42`。

---

### 3.B 其他只在设计文档、计划或 PR 里、没有 ADR 的重要决策

| # | 决策一句话 | 现在在哪（file:line） | 代码是否落地（证据） | 建议 | 理由 |
|---|---|---|---|---|---|
| O1 | Open Design 不再打进安装包，首次打开 Design 页时按需下载并校验（Owner 2026-10-05 裁决） | `docs/ARCHITECTURE-LAYERS.md:208`；提交 d82c0a12 正文；`ADR-038:46`（只记为现状） | 已落地：`apps/desktop/src/main/open-design-component.ts`；提交 406bf752 | **并入 ADR-038**（新增一条分发决策）；同时给 ADR-OD-004/005 补修订说明 | ADR-OD-005 `:17,94` 和 ADR-OD-004 `:89` 仍写「bundled sidecar」；ADR-038 本身管锁和下载，状态还是草案，能吸收。可以和 ADR-021「不捆绑运行时」、ADR-DOC-03 按需下载放在一起，写成统一原则 |
| O2 | Hyphae 也按需下载 | 见 §3.A C15、X1 | 已落地 | **写进新的通讯 ADR**（§3.A.3），同时引用 ADR-038 | 和 `HYPHAE-NETWORK-DELIVERY.md:10,31` 相反，必须先裁掉 |
| O3 | iDoris 由 agent24d 托管（安装、setup、签发 key、监督），推翻旧边界「Agent24 不拥有或启动 iDoris」 | `docs/agent/IDORIS-LAUNCHER-DESIGN.md:41-50`、§13.1 `:283`（iDoris 已接受）、§14.1 `:329-332`；旧边界 `docs/agent/PLAN-IDORIS-V031.md:39` | 已落地：`rust/apps/agent24d/src/managed_idoris.rs`、`idoris_host.rs`、`idoris_routes.rs`；IL-11 / IL-13 提交（956fb705、01bb550a、db52fd92） | **新 ADR**（ADR-032 的后继，或作为 ADR-032 修订） | ADR-032 的决策是「网关作兼容接口」，ADR-035 只管 token 怎么交付，都没有托管和生命周期这件事；这是推翻旧计划的决定，按规矩应该进 ADR |
| O4 | CORE-2：一方外壳登记、身份校验、两级权限、在场证明；D1–D9（例如 comm 写操作是提升级、FirstPartyShell 的 TTL 为 1 小时、模块路由暂归日常级并待外审） | `docs/agent/CORE-2-ENROLL-DESIGN.md:7,27-51,100,106` | 已落地：`enroll_api.rs`、`enroll_endpoint.rs`、`capabilities/route_policy.rs`、`types.rs:14-18`（`FirstPartyShell`） | **并入 ADR-037**（决策 3 的实施附录或修订记录） | 上位就是 ADR-037 决策 2、3（`decision.md:1560-1595`）；CORE-2 新增的等级表和 TTL 应该回写进去。注意：`decision.md:1508` 的状态仍是「提议」，而 `CORE-5-MIGRATION-GUARD-DESIGN.md:200`（Q6）说 ADR-037 已经被 #1249 接受、状态行已由 #1304 订正，两处对不上 |
| O5 | CORE-5：双库迁移保护（checkpoint 加 backup API、全局停写、停靠态、G=2·S 空间预算、严格降级规则） | `docs/agent/CORE-5-MIGRATION-GUARD-DESIGN.md:20-40,191-213` | 已落地：`rust/apps/agent24d/src/migration_guard/`、`migration_coordinator.rs`（只看了目录，没有逐片核对） | **并入 ADR-037**（决策 2 的 F1 条） | `decision.md:1547-1555` 已经规定了快照方案和 repair 路径；CORE-5 是实施设计。G=2·S、降级规则这类新约束建议回写 |
| O6 | capability token 的受众：`ProductHost` / `FirstPartyShell` / `CreativeRuntime` | ADR-OD-005 `:21-28`（product_host / creative_runtime）；`CORE-2-ENROLL-DESIGN.md:100`（新增 FirstPartyShell）；`ARCHITECTURE-LAYERS.md:236` | 已落地：`rust/apps/agent24d/src/capabilities/types.rs:14-18` | **不需要新 ADR**；FirstPartyShell 并入 ADR-037 决策 3。以后把 Creative 专名泛化为按工具区分的 `ToolRuntime{tool}` 时（`docs/agent/COMPONENT-ROADMAP.md:86`，现已暂停），再修订 ADR-OD-005 或另立新 ADR | 现有三个受众都有 ADR 依据；只有泛化这件事还没决定 |
| O7 | Guardian 自动审批：**默认关闭**（`A24_GUARDIAN=1` 才开），`A24_GUARDIAN_ALWAYS_REVIEW` 默认是 `exec`，只用 LocalOnly 模型，失败一律升级给人 | ADR-026 §6.5 第 5 条 `docs/ADR-026-rust-core-polyglot.md:217`（写的是「24/7 自动审批」，没提默认关）；`docs/laws/APPROVAL.md:45`；`docs/specs/TASKS.md:188,514` | 已落地：`rust/apps/agent24d/src/server.rs:471-497`；`rust/crates/agent24-policy/src/guardian.rs:1-17` | **修订 ADR-026 §6.5 第 5 条**（补上默认关和 always-review），可以考虑在 laws/APPROVAL 加一条；不需要新 ADR | 原则已经在 ADR 里，只缺「默认关」这条安全默认值，而它现在只出现在 TASKS 和代码里 |
| O8 | ModelRouter / TaskProfile：LocalOnly 失败时不外发（fail-closed）；Local 层必须走 loopback_only；有 cooldown 反馈回路 | ADR-026 §4 `:172-183`；ADR-017；ADR-019；ADR-036 第 5 条（`decision.md` ADR-036 节）；`docs/specs/TASKS.md:184` | 已落地：`rust/crates/agent24-models/src/router.rs:1-13,33-38`。ADR-036 第 5 条要求的「云端补位开关」，在 `router.rs` 里没找到独立开关（可能由 EgressGate 承担），**未核实** | **不需要新 ADR**；建议在 ADR-026 §4 补一条「Local 判定 = loopback_only」 | 路由原则已分散写在 ADR-017、019、026、036 里；`router.rs:34` 说的「v3.1 L-1」不变量出自哪里，未核实 |
| O9 | Knowledge 宿主接口（KB-HOST-01）：只有 ProductHost 能激活或撤销 binding，交付短期凭据 C（只在内存）；记录归写入它的 capability 所有（2026-10-24 决定）；用序号防回退；run 创建时钉住 binding；没有 binding 的 run 不广告相关工具 | **只在 GitHub issue #1085**，仓库里没有这份设计文档（`docs/agent/MODULE-PROGRESS-EVIDENCE-20261023.md:19,28` 只记了状态）；代码注释 `rust/apps/agent24d/src/knowledge_host.rs:1-15` | 已落地：`knowledge_host.rs`、`knowledge_tools.rs`；提交 21fdea7c（PR-A1）、60fd69b2（PR-C）、c718b832（PR-D） | **新 ADR（建议编号 ADR-KB-02）** | 设计正文在仓库外；它定了凭据交接和授权归属，属于安全边界。ADR-KB-01 只管「单一主对话」 |
| O10 | 组件签名：锁里的 sha256 是签名后的制品；可复现比对在签名前做；Hyphae 签名版（C1b） | ADR-038 `:17,145-147,324,356+` | 已落地：`hyphae_install.rs:126-170`；#1551 | **不需要新 ADR**；建议把 ADR-038 从「草案 r3/r4」推进到接受 | 已经在 ADR-038 里；C1b 已落地，但状态还是草案 |
| O11 | 发布签名：预览版也必须用 Developer ID 签名并公证，走仓库内流水线（jason 2026-10-27） | `docs/RELEASE-CHECKLIST-v0.6.0-preview.md:89-93`；提交 c40fee98 | 已落地：#1353（98e94bc8），`.github/workflows/release-desktop.yml` | **并入 ADR-037 决策 5** | ADR-037 `decision.md:1600` 已提到签名加公证，但没写「预览版也签」；`CORE-2-ENROLL-DESIGN.md:140-142` 当时认为流水线未被证实，需要回写 |
| O12 | Windows 移植：模块传输用命名管道 + DACL + 对端 pid→SID，wire 帧不变，用 `A24_LISTEN_PIPE` 替代 `A24_LISTEN_FD`；W7 门槛 jason 选 (a) | `docs/design/WINDOWS-PORT.md:3,12-19`；ADR-037 决策 5 引用了它（`decision.md:1602`） | **基本没落地**：grep `A24_LISTEN_PIPE` 0 命中；只有 CI `windows-check.yml`、`sidecar-windows.yml` | **并入 ADR-037 决策 5**（已经在引用）；动手实现前再确认要不要修订 ADR-031 的 wire 契约 | 平台顺序已在 ADR-037 里；命名管道是 ADR-031 的 Windows 版实现，不另起 |
| O13 | 硬件档位 1–6、云端补位默认关、decide 的 T0–T3 改读档位、探测归 Agent24 | `docs/design/HW-TIERS-capability-hardware-matrix.md:283-290` | 已落地：`rust/crates/agent24-hw/src/budget.rs:18-39`（L1–L6）；云端开关见 O8，未核实 | **不需要**，已在 ADR-036（`decision.md:1468-1471`，第 3、5、7 条） | HW-TIERS 末尾自己写了「已记录为 ADR-036」 |
| O14 | 里程碑方向分支工作流：`ab/<方向>` 集成分支，约 10 个 PR 发一次 release，release PR 必须 PR-Daemon APPROVE，ruleset 保护（jason 2026-10-07） | `AGENTS.md:12-31` | 流程约定；ruleset `ab-integration`（`AGENTS.md:30`）在 GitHub 上，本地无法核实 | **不需要 ADR** | 是工程流程，不是架构决策；放在 AGENTS.md 或 CONTRIBUTING 就够了 |
| O15 | docs/laws/*（APPROVAL / CONTEXT / MEMORY）：可观察的、有编号的不变量，改行为的 PR 必须对齐 | `docs/laws/README.md:14,24-38` | 法律层由测试和 PR 守住（没有逐条核对） | **不需要 ADR**；建议在 ADR 索引加一节「法律」的链接 | README `:14` 已经说清楚法律和 ADR 分工不同；SPEC-EDITIONS 里的 L-ED-3「拟」还没进 laws（grep `L-ED` 在 docs/laws 里 0 命中） |
| O16 | iDoris Admin token 由 launcher 经匿名 FD/HANDLE 交付 | `IDORIS-LAUNCHER-DESIGN.md` | 已落地（见 O3） | **不需要**，已有 ADR-035 | 已覆盖；只有托管这件事缺 ADR，见 O3 |
| O17 | Documents 是进程外的 OS `documents` | ADR-DOC-01（`docs/documenting/adr/ADR-DOC-01-placement-and-integration.md:5`，Accepted） | `packages/documents-accept` 等（没有逐项核对） | **不需要** | 已有 ADR-DOC-01 |
| O18 | 版本边界：审批和组织化的判定机制全部开源；企业版只做管理界面、报表和 SSO；本地优先在所有版本都是默认且完整的；「≤50 人」这条线待确认 | `docs/specs/SPEC-EDITIONS.md:3,99-140` | 产品边界，没有直接对应的代码 | **新 ADR（产品层）**；或者至少登记进 ADR 索引 | 这份文档自称「产品决策文档」，2026-09-02 已裁决，却不在 60 份 ADR 里；它会约束 M3 排期和以后的商业化，还带一条「拟」法律 |
| O19 | `agent24-os-sdk` 的形态（Q1–Q11，jason 2026-09-26 全按推荐；v0.1.0 定位为原型） | `docs/design/ME4-S3-os-sdk.md:3,20,42` | 已落地：`rust/crates/agent24-os-sdk` | **不需要新 ADR**；在 ADR-031 的关联里加上它 | 是 ADR-031 协议的 SDK 实施；`docs/agent/PLAN-OOP-OS-AND-BACKLOG.md:165` 的「T0.2 两个都要」也属于这一块 |
| O20 | Open Design 的 iDoris 版：产品名「iDoris Design」、外壳导航不改、图标重描 | `docs/design/OPEN-DESIGN-IDORIS-EDITION.md:344` | 未核实 | **不需要 ADR** | 品牌和文案，属于产品决定 |
| O21 | 模块签名（ME-6，sigstore keyless 加信任策略） | `docs/agent/COMPONENT-ROADMAP.md:93`（「远期，需拍板」） | 没落地 | **还没决定**；定下来时并入 ADR-038 或另立新 ADR | 目前只是候选，不是决定 |
| O22 | OpenCreator 接入前要先立一份边界 ADR（OC-1） | `COMPONENT-ROADMAP.md:114`（暂停） | 没落地 | **将来新 ADR**（计划中） | 文档自己列了要写，目前暂停 |

#### 3.C 核实不了的地方
- GitHub 上的 PR 正文、评论、ruleset 都看不到；#1085、iDoris#588 的设计正文只能从仓库里的引用间接了解。
- COMM-5b、COMM-6、COMM-7 的 PR 号在本地 log 里没找到。
- nostr-bridge 运行时实际用哪个 HOME、`protocol.ts` 的逐行实现，没有核对。
- ADR-036 第 5 条「云端补位开关」在路由层是否落地，没有核实。


---

## 4. 文档与代码的漂移

| 文档 | 最后修改 | 过期 | 部分过期 | 未核实 | 最需要先改的 |
|---|---|---|---|---|---|
| `docs/ARCHITECTURE-LAYERS.md` | 2026-11-01（e6ee1896） | 依赖表 4 行、L3 components/idoris 两行、L5 `required_operation` 位置、L6 引言；§7 第 10 条 | L3 comm、L5 API/组件托管/CORE-5、L6 ④、L7；§7 第 2、9、20 条 | §7 第 15、22 条 | 依赖表（#1539 反转了 idoris→components）、§7 第 9/10 条、L6 引言（ADR-039 已接受） |
| `docs/architecture/EVOLVABILITY-ASSESSMENT-2026-11-01.md` | 2026-11-01 | 裁决 7、8，风险 3、7，建议 2、6（已完成） | 裁决 6（Hyphae 锁已有契约字段，但运行期未检查）、建议 1 | 未核实项原样 | 加「2026-11-03 状态更新」附注；行号漂移 5 处 |
| `docs/agent/roadmap.md` | 2026-10-08（c97d209f） | 顶部现状、M1「当前主干」、OD「全部暂停」、「当前聚焦 ME-3」等 9 处 | M4、M5 3 处 | — | 顶部写明「当前路线以 COMPONENT-ROADMAP 为准」，M 系列转历史 |
| `docs/agent/tasks.md` | 2026-10-27（1f7d60a7） | 24 条（K1-1~7、COMM-5b/6、M2 release、D0-6 等） | 含在 24 条内（OD-M11/12、COMM-7、DEP-C8 等） | 3 条 | 补 CORE-5、ADR-038 台账；K1、COMM、M2 状态订正 |

逐条明细如下（每表：文档位置 | 原文 | 现状证据 | 判定 | 修正建议）。


### 4.1 docs/ARCHITECTURE-LAYERS.md

#### 4.1.1 依赖表（§1.1，L71–99）

`cargo metadata` 重算结果（只计正常依赖，省略 `agent24-` 前缀）：`components` → （无）；`idoris` → components, hw；`worker` → protocol。其余行与文档一致：protocol、sidecar-host-protocol、os-fd、os-cwd、os-proc、os-sqlite、comm、hw、knowledge、documents-client 都没有内部依赖；core、domain、models、decide、store、workspace、tools、memory、policy、scheduler、mcp、agent（dev 依赖 policy）、os-proto、os-packages、os-sdk、documents、agent24d（dev 依赖 os-sdk，另有 comm）、agent24-cli、sidecar-host 也都一致。

| 文档位置 | 文档原文（摘短） | 现状（证据） | 判定 | 修正建议 |
|---|---|---|---|---|
| ARCHITECTURE-LAYERS.md:75 | 「…/ worker / comm / hw / knowledge / documents-client \| （无内部依赖）」 | `rust/crates/agent24-worker/Cargo.toml:10` 写着 `agent24-protocol = { path = ... }`；`git show bb434b61:rust/crates/agent24-worker/Cargo.toml` 第 10 行也一样，**说明 11-01 那次「重新生成」时就已经错了** | 过期（一直不准） | 把 worker 移出「无内部依赖」行，单列 `worker \| protocol`；`components` 改进这一行（见下） |
| :78 | 「idoris \| hw」 | `rust/crates/agent24-idoris/Cargo.toml:9-10` 依赖 components 和 hw；#1539 C1 做了「依赖反转」（合并提交 7e9bc3b1，子提交 0d009848） | 过期 | 改为 `idoris \| components, hw` |
| :79 | 「components \| idoris」 | `agent24-components/Cargo.toml` 没有任何内部依赖；`src/lib.rs:1-4`：「Component crates (agent24-idoris) … depend on this crate, never the other way round」 | 过期 | 删掉这一行，把 components 放进「无内部依赖」行 |
| :97 | 「`components` 的下载器借用 `idoris`（见 §7）」「`worker` 当前没有消费者」 | 方向反了，现在是 idoris 借 components（`agent24-idoris/src/download.rs:1`「iDoris's use of the generic downloader (agent24-components)」）。worker 确实没有消费者：rust/ 里除工作区成员表外只有 3 处注释提到它 | 部分过期 | 改为「`idoris` 和 agent24d 的 Hyphae 安装器都依赖 `components`（ADR-038 C1，#1539）」；worker 那句保留 |
| :99 | 「表由 `cargo metadata --no-deps` 重新生成（…2026-11-01）」 | 见上，worker→protocol 当时就漏了 | 过期 | 改成 2026-11-03 重新生成，并在 PR 里附脚本输出 |
| 其余各行（:76–77、:80–95） | — | 与 `cargo metadata` 结果逐行一致 | 仍准确 | — |

#### 4.1.2 §1 总图和 §2 各层

| 文档位置 | 文档原文（摘短） | 现状（证据） | 判定 | 修正建议 |
|---|---|---|---|---|
| :5 文件头 | 「2026-11-01 更新：对应 main `bb434b61`」 | 之后合入的 #1539、#1540、#1545、#1549、#1551 改动了 L3、L5 和 §7 的事实（见下） | 过期 | 更新到 `9865d7a0` |
| :46 总图 L5 | 「组件托管：iDoris 托管、Hyphae 按需安装、Knowledge 宿主接口、模块工具 K1」 | 新增统一组件状态面：`agent24d/src/component_routes.rs:1`（`GET /api/v1/components`、`POST /api/v1/components/{id}/report`，#1545 C3a），挂载在 `server.rs:1453`；停靠诊断路由 `docked_router.rs:128-131`（#1528） | 部分过期 | 补「统一组件状态 `/components`（ADR-038 C3）」和「停靠诊断 router（CORE-5f，未启用）」 |
| :54 总图 L3 | 「components · idoris」（隐含 idoris 在上、components 在下） | 实际层次反过来，见 §4.1.1 | 部分过期 | 文字顺序无所谓，但 §1.1 和 L3 表要一致 |
| :112–116 L0 | openapi 手写；B4 仍是计划 | #1546、#1545 都是手改 `protocol/openapi.yaml`（`git show 612f0f0f --stat`） | 仍准确 | — |
| :120 L1、:122–138 L2 | core、store、workspace、os-cwd 的描述 | 这几个 crate 在 `bb434b61..HEAD` 里没有改动 | 仍准确 | — |
| :144 L3 models | iDoris provider ✅、`IDORIS_URL`、`TaskProfile::default()` | `agent24-models/src/router.rs:359` 读 `IDORIS_URL`；crate 没有改动 | 仍准确 | — |
| :145–149 L3 tools、policy、memory、scheduler、mcp | — | crate 没有改动；`session_memory.rs:168-171` 用 `retriever().search_any` | 仍准确 | — |
| :150 L3 comm | 「Hyphae（Nostr）CLI 调用、keystore…」「二进制哈希校验、密码只走 stdin」 | 新增的能力：①Hyphae 锁升到 schema 2 的 `ComponentLock`，带 `signing.macos_team_id`、`contract`、`provides`（`agent24-comm/hyphae.lock.json:24-29`、`:62-65`，v0.30.0）；②启动时做 macOS 验签，拒绝原因用 `signature_rejected` / `hash_mismatch` 表示（`comm_routes.rs:95-96`、`:802-809`，#1551/#1552）；③NIP-49 私钥备份与恢复（#1535、#1538）；④群聊 `groupchat.rs`；⑤带路径参数的路由经 runtime_router 转发（#1554） | 部分过期 | 「约束」列改为「锁里的 sha256 加 macOS 签名或公证校验（D2a）、拒绝信号、密码只走 stdin」，并补上备份/恢复和群聊 |
| :151 L3 worker | 「workspace 内没有任何 crate 依赖它」 | 没有 crate 依赖它，这点仍然成立；但它自己依赖 protocol | 仍准确 | 与 §1.1 的修正一致即可 |
| :152 L3 idoris | 「固定版本的 release 元数据（`idoris.lock.json`）、下载校验、安装…」「只依赖 `agent24-hw`」 | `idoris.lock.json` 已是 schema 2（`"schema": 2`，带 contract 对象和 signing）；下载和安装器只剩薄封装（`idoris/src/download.rs` 28 行、`install.rs` 98 行，实现都在 components）；依赖 components 和 hw；发布物迁到托管根 `<data>/components/<id>/<version>/`（`components/src/storage.rs:2-3`，#1539 的 aa0f09f4「C1b' 发布物迁到托管根」） | 过期 | 改为「iDoris 的 lock 视图（schema 2）、钥匙串、supervisor、oMLX 探测；下载、安装和验签用 `agent24-components`；依赖 components 和 hw」 |
| :153 L3 components | 「`fetch_verified` 按锁定的 URL + sha256 下载」「**借用 iDoris 的下载器和锁结构**（缓存目录必须在 `idoris` 树内）」 | 全仓已经没有 `fetch_verified` 这个符号（grep 为空）。`components/src/lib.rs:1-23` 现在导出 `ComponentLock`（schema 2）、`download_locked_asset` / `download_pinned`、不可变安装器、`run_install_child`、`SignatureVerifier`、`VerifyMode`（Install/AtRest）和 `ComponentState` / `Form`。Hyphae 安装走 `hyphae_install.rs:7-12` 的 `download_locked_asset` 和 `signing_requirement`（#1539） | 过期 | 整行重写：「通用组件机制（ADR-038 C1/C3）：schema-2 `ComponentLock`、通用下载器、不可变安装器、D2a 签名校验、组件状态机；被 idoris 和 agent24d 的 `hyphae_install` 使用，自身没有内部依赖」 |
| :154–156 L3 knowledge、hw、decide | — | crate 没有改动；`knowledge_backend.rs:6` 写明 Provider 未接线；`hw/src/probe.rs:162` 的 `runtimes: Vec::new()`；全仓没有 `impl DecisionLog for` | 仍准确 | — |
| :160–162 L4 | 正常依赖列表 | 与 cargo metadata 一致；agent crate 没有改动 | 仍准确 | — |
| :168 L5 对外 API | 列了 runs、sessions、approvals、comm、os、attached… | 漏了 `/api/v1/components`（server.rs:1453）。docked router 的 `/api/v1/health` 和 `/api/v1/diagnostics/migration*` 只在停靠态提供（docked_router.rs:128-131） | 部分过期 | 补上 `components`；workspace 没有 REST 路由这点仍然成立（server.rs 里 grep `"/api/v1/workspace` 为空） |
| :170 L5 legacy | 「`agent24d serve` 的缺省值」 | `agent24d/src/main.rs:155` 是 `default_value_t = AuthModeArg::LegacySingleToken` | 仍准确 | — |
| :171 L5 capabilities | 「…默认拒绝的全量路由映射（`src/capabilities/route_policy.rs`、`server.rs` 的 `required_operation`）」 | `required_operation` 定义在 `capabilities/route_policy.rs:199`，在 route_policy.rs:220 被调用；server.rs 里 grep 为 0 次，`bb434b61` 时也是 0 次 | 过期（位置写错） | 改为「`route_policy.rs` 的 `required_operation` / `decide`」 |
| :172 L5 CORE-2 | enroll、first_party、presence | `capabilities/first_party.rs` 在 #1540 里改了 66 行，用于停靠态登记通道（e373e158「docked 令牌收窄」）；文档描述本身没错 | 仍准确（可补充） | 可补一句「停靠态也开登记通道，令牌更窄（CORE-5f-3）」 |
| :173 L5 尚未实现 | 「`CreativeRuntime` 令牌仍只放行 `GET /api/v1/models`」「backendProxy 隔离…是否已完成未核实」「`agent24 acp` 是否在 capability 模式下可用未核实」 | ①`route_policy.rs:208-210` 只放行 GET `/api/v1/models`，**仍准确**。②`apps/desktop/src/main/backend-proxy.ts:1-8` 已有 A1 白名单（`ALLOWED_PROXY_ROUTES`），提升级操作不经代理而走专用 IPC；#1538 又把 comm 的 import/relay/unlock 移出了代理（backend-proxy.ts 的 diff）。③CLI 的 `Endpoint` 在 capability 模式下持有 enroll 会话（`agent24-cli/src/main.rs:578-589`），`acp.rs:28,45` 用的就是 `Endpoint`。L6 表 :211 自己也写了「capability 模式是 enroll 得到的一方日常令牌（`acp.rs`）」，和这里自相矛盾 | 部分过期 | ②改为「Desktop backendProxy 已有路由白名单，提升级走专用 IPC（backend-proxy.ts）；是否满足 F11.0 的全部验收未核实」；③改为「代码路径存在（Endpoint 带 enroll 会话），真机未核实」 |
| :174 L5 领域 OS 挂载 | `KERNEL_GRANTS` / `KERNEL_OOP_GRANTS` | `domain.rs:75` 起；domain.rs 在这段时间只改了 2 行 | 仍准确 | — |
| :177 L5 组件托管 | idoris_host、managed_idoris、hyphae_install、knowledge_*、hardware、decide_routes | 漏了 `component_routes`（#1545）；idoris_host/provision.rs 在 #1539 里改了 221 行（迁到托管根） | 部分过期 | 补「`component_routes`（统一组件状态，ADR-038 C3a；OD 由桌面上报，C3b #1549）」 |
| :178 L5 CORE-5 | 「`ENABLED = false`，生产启动不调用 `run`；`main.rs` 有编译期断言…把它钉死。`migration_guard/` 是快照 / 回滚等配套」 | `migration_coordinator.rs:20` 仍是 `ENABLED: bool = false`；断言在 `main.rs:114`；全仓没有 `migration_coordinator::run` 的调用，**这部分仍成立**。新增的部分：`server.rs:2083-2105` 在启动路径里调用 `docked_startup::pre_start(docked_startup::guard_enabled(), …)`，`guard_enabled()` 返回 `ENABLED`（docked_startup.rs:73-79），只有 `test-enrollment` 调试构建加 `AGENT24_TEST_MIGRATION_GUARD=1` 才能强开；`docked_startup.rs`（755 行）、`docked_router.rs`（883 行）、`migration_guard/startup.rs` 都是新文件（#1528 5f-2、#1540 5f-3） | 部分过期 | 改为「🟡 5f（停靠类型、router、启动接线）已落 main，但都在 `ENABLED=false` 后面；生产启动不调用 `run`，`pre_start` 因 guard 为 false 直接跳过；剩 5g 外壳识别、5h 安装器回退、5i 授权 retry（CORE-5-MIGRATION-GUARD-DESIGN.md:166-168）」 |
| :184–190 L5½ | os-proto、os-fd、os-packages、os-sdk | os-proto 只在 `supervisor.rs` 改了 4 行 | 仍准确 | — |
| :194 L6 引言 | 「按**计划中的** ADR-039（…**审阅中**）…统一契约见**计划中的** ADR-038（组件契约 v1）」 | `docs/adr/ADR-039-concept-model.md:5` 写「状态：**已接受（r2…）**（2026-11-01，jason 认可）」，ADR 索引 `docs/adr/README.md:24` 也是「已接受」。ADR-038 已存在，状态是「草案 r3」（`ADR-038-component-contract-v1.md:5`；#1537 的提交信息称 r4），而且 C1、C3a、C3b 已经实现（#1539、#1545、#1549）。另外 ADR-039 的用语是「本体 / 能力 / 技能」（README.md:24），不是这里说的「组件」 | 过期 | 改为「按 ADR-039（已接受），对外只有本体、能力、技能三个概念；下面四类外挂在实现上都是『组件』（能力的安装包，ADR-038 草案，C1/C3 已实现），区别只在运行形态」 |
| :198–200 L6 ①②③ | — | `Build::InProcess` 只出现在 domain.rs 的测试段（≥:1985）和注释里 | 仍准确 | — |
| :207 L6 ④ 进程托管 | 现行是 `CreativeServeWeb`，目标形态 🟡 SidecarManager | `creative-serve-web.ts` 现在 `import type { SidecarState } from './sidecar-contract'`，有 `headlessPhase`（#1549 C3b）。也就是说用了 sidecar 的状态类型，但没有用 SidecarManager；除它自身和测试外，没有别处引用 `sidecar-manager.ts` | 仍准确（可补充） | 可补「OD 状态已按 sidecar 状态词表上报给 `/components`（C3b）」 |
| :208 L6 分发 | 按需下载 | pin 已升到 0.26.1（`creative-serve-web.ts` 的 `OPEN_DESIGN_PIN_VERSION = '0.26.1'`，#1544）；#1533 补了 Mac 出包的按需组件 | 仍准确 | 可写明 pin 版本 |
| :210 L6 运行时协议 | 「Open Design 每个会话拉起 `agent24 acp`」 | #1555：桌面把 Agent24.app 自带的 CLI 作为 `AGENT24_BIN` 交给 OD（creative-serve-web.ts 的 `resolveBundledAgent24Cli`） | 部分过期 | 补「OD 用的 `agent24` 由桌面经 `AGENT24_BIN` 指定为包内 CLI（#1555）」 |
| :211–212 L6 凭据、workspace | ACP 用 Endpoint 令牌；`workspace_id: None` | `acp.rs:157,198` 是 `workspace_id: None` | 仍准确 | — |
| :220–227 L7 | desktop、cli、packages | `ls packages` 正好是 7 个，与文档一致；但没有提桌面的组件状态上报（`open-design-report.ts`）和 renderer 的 `componentApi.ts` / `useComponentList.ts` | 部分过期 | L7 desktop 补「组件页改读 `/api/v1/components`；OD 状态由 main 上报（C3b）」 |
| §3、§4（:233–269） | 两层「能力」、安全边界 | 都仍然成立。只是新增了一类边界没写：组件发布物的签名或公证校验（D2a，`components/src/sign.rs`，用于 iDoris 和 Hyphae） | 部分过期 | §4 可加一条「组件制品边界 ✅：锁定 sha256 + macOS Developer ID 签名或公证（D2a），Install/AtRest 两种模式」，并注明它**不是**领域 OS 签名（ME-6） |
| §5（:275–284） | 扩展影响表 | 内容仍成立；「② 与 ④ 两套扩展模型长期并存」已有 ADR-038、ADR-039 作为统一方向 | 部分过期 | 「建议」列可指向 ADR-038/039 |
| §6（:288–303） | M10 经验 | 历史记录 | 仍准确 | — |

#### 4.1.3 §7「尚未完成」逐条（:308–329，共 22 条）

| # | 文档位置 | 文档原文（摘短） | 现状（证据） | 判定 | 修正建议 |
|---|---|---|---|---|---|
| 1 | :308 | 路由级资源授权等 📐；`CreativeRuntime` 只能读模型目录 | `route_policy.rs:208-210` | 仍准确 | — |
| 2 | :309 | capability 模式在 Windows 和 `serve` 缺省路径上不是默认；`acp` 在 capability 模式下是否可用未核实 | 前半句：`backend-manager.ts:135-137`（非 win32 才用 capabilities）、`main.rs:155`。后半句：`Endpoint` 持有 enroll 会话（cli main.rs:578-589），acp 用 `Endpoint` | 部分过期 | 后半句改为「代码路径存在，真机未核实」 |
| 3 | :310 | Creative 分区是全局持久 | `creative-serve-web.ts:75` 是 `'persist:agent24-creative'` | 仍准确 | — |
| 4 | :311 | workspace-bound run 仅 Unix | workspace crate 没有改动 | 仍准确 | — |
| 5 | :312 | workspace 对外路由未开放 | server.rs 里 grep `"/api/v1/workspace` 为空 | 仍准确 | — |
| 6 | :313 | 没有生产内置的进程内 DomainModule | `Build::InProcess` 只在测试段出现（domain.rs:2094/2105/4987，测试模块从 :1985 开始） | 仍准确 | — |
| 7 | :314 | SidecarManager / sidecar-host 未接入 | 只有 sidecar-manager.ts 自身引用它；桌面里 grep `agent24-sidecar-host` 为空 | 仍准确 | — |
| 8 | :315 | acp 不传 `workspace_id` | `acp.rs:157,198` | 仍准确 | — |
| 9 | :316 | CORE-5 未接线：`ENABLED=false`、编译期断言、「**启动路径不调用它**」 | ENABLED 和断言仍成立（`migration_coordinator.rs:20`、`main.rs:114`）。但启动路径已经接上了受开关控制的停靠入口：`server.rs:2083-2105` → `docked_startup::pre_start`（#1540）。`run` 仍然没有调用方 | 部分过期 | 改为「`ENABLED=false`；5f-1/2/3 已合（70f1e9a1、#1528、#1540），启动路径在 `ENABLED` 后面挂了停靠入口；`run` 未调用；剩 5g/5h/5i」 |
| 10 | :317 | 组件安装器借用 iDoris 的锁结构；`fetch_verified` 复用 `IdorisLock`；尚无独立的组件锁和安装器 | #1539 后这条全部反过来：`ComponentLock` 在 components/src/lock.rs:14，下载器在 download.rs:137/155，安装器在 install.rs:221，签名在 sign.rs；idoris 依赖 components；`fetch_verified` 已删除 | 过期 | 删掉这条。换成新的未完成项：「ADR-038 剩余切片：C2 运行期契约检查（Agent24 侧没有 `hyphae-cli` 契约检查代码，grep 只在测试里命中）、C4 AgentEar、C5 Knowledge、C6 Documents、C7 领域 OS / MCP 清单（ADR-038:327-330）；Hyphae 锁只有 darwin-arm64 和 linux-x64 两个平台（hyphae.lock.json:2-22）」 |
| 11 | :318 | decide：没有 `DecisionLog` 实现，agent loop 没有调用点 | grep `impl.*DecisionLog for` 为空；decide crate 没有改动 | 仍准确 | — |
| 12 | :319 | hw：runtimes 未探测 | `hw/src/probe.rs:162` 是 `runtimes: Vec::new()` | 仍准确 | — |
| 13 | :320 | knowledge 的 Provider 未接线 | `knowledge_backend.rs:6` | 仍准确 | — |
| 14 | :321 | worker 只有 Rust 侧，没有消费者，也没有 Python 服务端 | 没有调用方；仓库里找不到 worker 的 .py | 仍准确 | — |
| 15 | :322 | 推理回调流式（P3）📐 | `model_callback.rs` 里 grep `stream` 为空；没有深查 | 未核实（倾向仍成立） | — |
| 16 | :323 | 任务画像生成 📐；出境脱敏归 iDoris | models 和 agent crate 没有改动 | 仍准确 | — |
| 17 | :324 | Web3（AAStar）没有接线 | 在 rust/ 里 grep `aastar\|web3` 为空 | 仍准确 | — |
| 18 | :325 | OpenAPI 由 Rust 生成（B4）📐 | openapi.yaml 仍然手改（#1546 的 diff） | 仍准确 | — |
| 19 | :326 | workspace 只有 scratch 一种受信构造 | crate 没有改动 | 仍准确 | — |
| 20 | :327 | 领域 OS 签名（ME-6）📐 | os-packages 里 grep `signature\|codesign` 为空 | 仍准确 | 注明组件签名（D2a）已有，但不覆盖领域 OS 包 |
| 21 | :328 | `docs/laws/` 只有三部 | `ls docs/laws` 结果是 APPROVAL、CONTEXT、MEMORY、README | 仍准确 | — |
| 22 | :329 | Review 遗留 #661、#663 | 没有网络，看不到 issue 状态 | 未核实 | — |

---

### 4.2 docs/architecture/EVOLVABILITY-ASSESSMENT-2026-11-01.md

| 文档位置 | 文档原文（摘短） | 现状（证据） | 判定 | 修正建议 |
|---|---|---|---|---|
| :5 | 「对应 main `bb434b61`」 | 之后的 #1539、#1540、#1546、#1551 改了表里的 4 条事实（见下） | 部分过期 | 文首加一段「2026-11-03 状态更新」，或者另写附录 |
| :19 裁决 1 | `migration_coordinator.rs:20` `ENABLED=false` | 仍在 :20 | 仍准确 | — |
| :19 | `main.rs:111` 用 `assert!(!ENABLED)` 钉死 | 现在在 `main.rs:114`（#1540 等改了 main.rs，共 5 个提交） | 行号漂移 | 改成 :114 |
| :19 | `server.rs:521-537` 失败只 warn 然后返回 None | `open_memory_base` 现在在 `server.rs:539-555`（:539 定义，:549 `tracing::warn!`） | 行号漂移，事实仍成立 | 改成 :539-555 |
| :19 | `server.rs:2097` 直接接受 None | 现在在 `server.rs:2187`（`let memory_base = open_memory_base(ephemeral).await;`）。而且在它前面的 :2083-2105 多了 `docked_startup::pre_start` 停靠入口（ENABLED=false 时跳过） | 行号漂移，部分过期 | 改成 :2187，并补「前置停靠入口已接、处于关闭状态」 |
| :19 | `MemoryConn::`/`StoreConn::` 在 agent24d 里除 coordinator 外零调用 | 在 agent24d/src 里排除 migration_coordinator.rs 后 grep 为空 | 仍准确 | — |
| :20 裁决 2 | `session_memory.rs:60-71` 的字段是 `kv: KvStore, log: SessionLog`；`recall()` 第 168 行；`lib.rs:576` | 文件在 `bb434b61..HEAD` 里没有改动：struct 从 :60 开始，`kv: KvStore` 在 :61；`.retriever().search_any` 在 :168-171；memory `lib.rs:576` 是 `pub fn retriever(&self) -> retriever::FtsRetriever` | 仍准确 | — |
| :21 裁决 3 | memory crate 有 16 个 `pub trait`，出了 crate 只有 Summarizer 被当 trait 用 | `grep -c "pub trait" agent24-memory/src/*.rs` 合计 16；crate 没有改动 | 仍准确 | — |
| :22–23 裁决 4、5 | 没有导出接口；`retriever.rs:55` 的 `rebuild()`；`lib.rs:467` | memory_routes.rs 只有 assertions 和 settings 的路由（:28、:151、:208、:262、:285）；grep `MEMORY_SCHEMA_VERSION` 和 `memory/export` 都为空；`retriever.rs:55`、`lib.rs:467` 都对得上；迁移脚本到 0020 为止 | 仍准确 | — |
| :24 裁决 6 | iDoris 是「`contract_major/min_contract_minor`、`check_contract`」，**唯一**带语义版本协商的组件；「`hyphae.lock.json` 只锁二进制 sha256，**无契约版本字段**」 | ①iDoris 锁已是 schema 2，契约字段改成对象 `"contract": { "name": "idoris-http", "major": 1, "min_minor": 1, "tested": "1.1.0" }`（idoris.lock.json:9）；`check_contract` 仍在（idoris/src/lib.rs:236，在 supervisor.rs:962 被调用）。②**Hyphae 锁现在有契约字段**：`"contract": {"major":1,"min_minor":0,"name":"hyphae-cli","tested":"1.0"}`、`signing`、`provides`、`reproducible`（hyphae.lock.json:24-29、:30-44、:62-65，v0.30.0，#1546、#1551）。但 Agent24 在**运行期没有向 Hyphae 做契约握手**：`accepts_contract` 只用在锁的自校验（components/src/lock.rs:215）和测试里（hyphae_install.rs:304），`hyphae-cli` 只出现在测试 | 部分过期 | 改为「Hyphae 锁已有 schema-2 契约、签名和能力字段（C1b、C2），但运行期契约检查还没接」；「唯一」改成「运行期做契约检查的只有 iDoris」 |
| :24 | `attach_registry.rs:913` 的 `protocol_version: 1` 是测试数据 | 现在在 :928（`bb434b61` 时在 :913，之后有 2 个提交改了这个文件） | 行号漂移 | 改成 :928 |
| :24 | Documents 没有锁文件，也没有版本协商 | 找不到 documents 的 `*.lock.json`；ADR-038 C6 写的是「等它独立发版时再补 D2 的锁」（ADR-038:329）；`component_routes.rs:3-8` 已经把 Documents 的状态纳入 `/components` | 仍准确（可补充） | 可补「状态已纳入 `/api/v1/components`」 |
| :25 裁决 7 | 「`agent24-components/src/lib.rs:36-46` 用 `IdorisLock::embedded().clone()`…Cargo 唯一内部依赖是 `agent24-idoris`。唯一调用方是 `hyphae_install.rs:35`」 | 全部不再成立：`components/src/lib.rs` 现在只有 26 行的模块和导出声明，没有 IdorisLock；components 没有任何内部依赖；全仓没有 `fetch_verified`；`hyphae_install.rs:7-12` 改用 `download_locked_asset` 和 `signing_requirement`（#1539，0d009848「依赖反转」） | 过期（已修复） | 裁决改为「成立（11-01），已由 #1539 C1 修复」 |
| :26 裁决 8 | 「ARCHITECTURE-LAYERS.md 过时：文件头写 `122fff7`…第 134 行…第 137/301 行」 | ARCHITECTURE-LAYERS.md 已由 #1526（63f0b5c7）按 `bb434b61` 修正：:5 文件头、:144 写了 iDoris provider ✅、:147 写了 recall 已消费。文中的 134/137/301 行号对现在的文件已经没有意义 | 过期（已修复） | 标「已由 #1526 修正」；新的漂移见本报告第 1 节 |
| :27 裁决 9 | worker 没有调用方；注释在 `net.rs:590`、`tui/mod.rs:416`、`main.rs:4016` | 前两处行号不变；cli `main.rs` 的那条注释现在在 :4101（`bb434b61` 时在 :4016）。`MlWorker` 只有 embed、transcribe、health 三个方法（worker/src/lib.rs:138-147），没有训练接口 | 事实仍成立，一处行号漂移 | `main.rs:4016` 改成 `:4101` |
| :28 裁决 10 | 代码里没有 Skill 加载器；#1037 是 OPEN | 全仓 grep `SkillLoader\|skill_loader\|SkillRegistry` 为空；另外 ADR-040 技能契约 v1 已经「提议」（adr/README.md:38，#1530）。#1037 的状态没有网络无法核实 | 代码部分仍准确，issue 未核实 | 补「ADR-040（提议）已给出技能契约」 |
| :31 | 「Codex 独有：…`fetch_verified` 寄生 iDoris 锁」 | 同裁决 7 | 过期 | 标「已修复（#1539）」 |
| :36–40 优点 | PROTOCOL_VERSION、iDoris 样板、os-proto 和 Knowledge 的协商等 | 相关 crate 都没有改动，仍成立；iDoris 锁的形态现在是 schema 2 | 仍准确 | — |
| :44 风险 1 | 迁移协调器 `ENABLED=false`，memory 打不开就 warn | 仍成立（server.rs:549 warn） | 仍准确 | 可补「5f 已合、仍关闭」 |
| :45 风险 2 | SessionMemory 和 RunManager 拿的是具体类型 | agent crate 没有改动 | 仍准确 | — |
| :46 风险 3 | 「只有 iDoris 有完整的『锁 + 契约版本 + 能力』三件套。Hyphae 锁只锁二进制…`fetch_verified` 还借 iDoris 的锁结构用」「接一个新组件要再发明一遍下载、校验」 | Hyphae 锁现在也有三件套（契约在 hyphae.lock.json:24-29，能力在 :30-44），外加签名；通用下载器、安装器和验签已经抽到 components（#1539）；统一状态模型和 `/components` 也已有（#1545） | 过期（大部分已缓解） | 改为「锁和下载已统一（C1）、状态已统一（C3）；还缺：运行期契约检查（Hyphae C2 的 Agent24 侧）、AgentEar/Knowledge/Documents 的 C4–C6」 |
| :47–49 风险 4–6 | 没有导出、没有 Skill、MlWorker 是空接口 | 见上，都仍成立 | 仍准确 | — |
| :50 风险 7 | 架构文档落后代码三周以上 | 已由 #1526 修正；现在只落后约 2 天（见 §4.1） | 过期 | 改写或删除 |
| :54 建议 1 | 「把 `server.rs:2097` 的 `open_memory_base` 换成 `migration_coordinator::run`…删掉 `main.rs:111` 的断言」 | 行号应为 server.rs:2187、main.rs:114；5f 的停靠和协议已做完（#1528、#1540） | 部分过期 | 更新行号，并注明前置 5f 已完成、剩 5g/5h/5i |
| :55 建议 2 | 「第一步：把 `IdorisLock` 抽成 `agent24-components::ComponentLock`，让 `fetch_verified` 不再借 iDoris 的锁；Hyphae 锁补 `contract_major/min_contract_minor`」 | 第一步已完成：#1539（ComponentLock schema 2）、#1546 和 #1551（Hyphae 锁带 contract 对象，字段名是 `major` / `min_minor`，不是 `contract_major`）；ADR-038 本身也已写成（草案 r3/r4） | 过期（已完成） | 标完成，下一步改为 C2（运行期契约检查）和 C4–C7 |
| :56–58 建议 3–5 | Retriever 注入、记忆导出、Skill 立项 | 都没有实现迹象（相关 crate 没有改动，grep 为空） | 仍准确 | 建议 5 补 ADR-040 |
| :59 建议 6 | 「更新 ARCHITECTURE-LAYERS.md：改第 134、137、301 行和文件头 commit」 | #1526 已做 | 过期（已完成） | 标完成 |
| :63–68 未核实项 | sidecar-host-protocol 的 `frame.rs:308` 等 | 那个 crate 没有改动；这里没有再查 | 未核实 | — |

---

### 4.3 docs/agent/roadmap.md（最后修改 2026-10-08，c97d209f）

| 文档位置 | 文档原文（摘短） | 现状（证据） | 判定 | 修正建议 |
|---|---|---|---|---|
| roadmap.md:3 | 「2026-10-03：Open Design 集成线 M10 已落地 → 下一阶段」 | 这一段是 10-03 的快照；之后 M1 已落地、M2 已收口、K1、Knowledge、v0.6.0-preview 都已经发生（tag 到 `v0.6.0-preview.20261028.11`） | 过期 | 顶部加 2026-11 的现状段 |
| :6、:8 | 「OD-M11 → TPI-W → OC…上述后续里程碑全部 `PAUSED`」 | OD 线实际已经重启：ODX-1 把 pin 升到 v0.26.1（#1544），ODX-2/3/6 继续升 pin、交出 `AGENT24_BIN`（#1555），#1541 合入了已拍板的 OD iDoris 版改造计划（`docs/design/OPEN-DESIGN-IDORIS-EDITION.md`）；F11.1 的 capability 启动已在 Desktop 上默认开启（afe7d4fb，#1085 KB-HOST-01） | 过期 | 改为「OD 线以 ODX（iDoris 版改造）形式恢复；OD-M11 中 F11.1 部分已做（capability 默认），F11.0a/F11.2/F11.3 未做」 |
| :8 | 「**M1 恢复为当前主干**，执行定义见 `ab/m1-memory` 分支的 `docs/agent/M1-PLAN-v2.md`」 | M1 已于 2026-10-07 经 #720 合入 main（1e25795c）；`docs/agent/M1-PLAN-v2.md` 已经在 main 上 | 过期 | 改为「M1 ✅ 2026-10-07（#720）」，链接改为 main 上的文件 |
| :21–37 | 「⏸️ 2026-09-19 现状说明 —— 本文档 M1–M6 全部未开工」 | :8 已声明这段是历史，但标题仍然像在描述现状；M1 已完成 | 过期（已自声明为历史） | 折叠或移到附录，标题加「（历史）」 |
| :44 | 「**当前开发轮次只覆盖一件事**：…（M1）」 | M1 已完成（tasks.md:369、:394） | 过期 | 删除或改写 |
| :51–61 M1 一节 | F1.1 Authorizer、F1.2 personal space、F1.3 记忆接进 agent loop，没有状态标记 | 已按 M1 v2 交付，对照关系见 tasks.md:401：F1.1 → M1-T01（#636），F1.2 → T02（#639）加 T05（#648），F1.3 → T03（#637）、T04（#646）、T05。代码证据：agent loop 用 `SessionLog`（agent24-agent/src/lib.rs:2178 的注释、session_failure_tests.rs:575），`CanonicalSession` 只剩在测试里做 legacy 导入（memory_receipt_tests.rs:6、:342） | 过期 | 标题加「✅ 2026-10-07（#720）」，并给出 F→M1-Txx 的对照 |
| :55、:91 | 「M-D 的 12 个模块 crate 外引用**全为 0**，agent loop 用的还是…`CanonicalSession`」 | agent loop 已消费 `KvStore`、`SessionLog` 和 `retriever().search_any`（session_memory.rs:60-61、:168-171；agent/src/lib.rs:1028、:2034） | 过期 | 删除，或改为「（2026-08 时如此，M1 后不再成立）」 |
| :63–71 M2 | 「✅ 2026-10-08 收口」 | 成立；另外 `ab/m2` 已经通过 #767 合入 main（73d87f0b，2026-10-08） | 仍准确（可补充） | 补「#767 落 main」 |
| :73–79 M3 | 等第二个真实用户，不排期 | 没有发现相反的证据 | 仍准确 | — |
| :83 M4 | 「**依赖 M1 全部完成**」 | M1 已完成，前置条件已满足；但 M4 没有开工记录（未核实 Cos72 仓库） | 部分过期 | 改为「前置 M1 已满足；是否排期待裁决」 |
| :111 M5 F5.1 | `mytask` 未做 | Cos72 的 mytask 实体、路由、审批发积分已在 ME-4 里交付（tasks.md:140-141：Cos72 #5、#6、#7）；roadmap :19 自己也提到了 | 部分过期 | F5.1 标「最小样例已在 ME4-5.3 交付」 |
| :114 M5 F5.4 | 「复用已有 F3 微信 / F4 Nostr」 | Nostr 现在走 Hyphae（agent24-comm，见 tasks.md COMM 台账）；`packages/nostr-bridge` 还在 | 部分过期 | 改为「Nostr 走 Hyphae（COMM）」 |
| :130 | 「当前聚焦是 ME-3 / v0.5.0」 | v0.5.0 已发（tasks.md:147），现在是 v0.6.0-preview 系列 | 过期 | 删除 |
| :133 | 「M4/M5 已排序但未拆 Task —— M1 交付前不拆」 | M1 已交付，这个条件已经不再阻塞 | 过期 | 改为「M1 已交付；拆不拆待裁决」 |
| 全文缺失 | — | 10-08 之后的主线一条都没有登记：K1（K1-R2 #1280、K1-R3 #1497）、Knowledge（`ab/weknora`、#1515）、Documenting（发布 r1–r8，#1532）、iDoris 集成（ADR-032 P4）、CORE-2、CORE-5（#1528、#1540）、ADR-038/039 组件化（#1527、#1539、#1545、#1549）、Hyphae 签名版（#1551）、v0.6.0-preview tag。COMPONENT-ROADMAP.md 最后改于 11-02（37538d78） | 过期 | 本文只保留 M 系列历史，顶部写明「当前路线以 COMPONENT-ROADMAP.md 为准」，或者补一张 2026-11 的现状表 |

---

### 4.4 docs/agent/tasks.md（506 行，最后修改 2026-10-27）

明显过期的条目共 24 条，另附若干仍准确的条目。

| # | 文档位置 | 文档原文（摘短） | 现状（证据） | 判定 | 修正建议 |
|---|---|---|---|---|---|
| 1 | tasks.md:3 | 「当前唯一权威的执行状态来源（2026-09-19 立，**2026-09-28 更新**）」 | 文件后来又改到 10-27；顶部「当前状态」仍停在 10-03 | 过期 | 更新日期，并写明各台账各自的更新日期 |
| 2 | :10 | PLAN-ME4 是「**ME-4 当前主线**」的权威文档 | ME-4 已随 v0.5.0 收口（同一文件 :19、:149） | 过期 | 改为「ME-4（已收口，历史）」 |
| 3 | :16 | roadmap 行：「▶️ **M1 已恢复为当前主干**…任务定义在 `ab/m1-memory` 分支的 M1-PLAN-v2.md」 | M1 已于 10-07 经 #720 落 main（1e25795c）；`docs/agent/M1-PLAN-v2.md` 已在 main 上 | 过期 | 改为「M1 ✅（#720）；roadmap.md 只剩历史」 |
| 4 | :19 | 「**当前状态（2026-10-03）**：…**v0.5.1** 已发布」 | 已发到 `v0.6.0-preview.20261028.11`（`git tag --sort=-creatordate`）；K1、Knowledge、Documenting、CORE、组件化各线都在推进 | 过期 | 重写「当前状态」段 |
| 5 | :22 | 「**主干 = M1 记忆即产品（C3 P0）**，在 `ab/m1-memory` 上继续」 | M1 已完成（:369、:394 自己也写了） | 过期 | 改为历史，并写明当前主干 |
| 6 | :23 | 「OD-M11 / OD-M12 / TPI-W / OC 全部 `PAUSED`…Desktop / ACP 仍用全权 token」 | Desktop 在 Unix 上默认 capabilities（backend-manager.ts:135-137，afe7d4fb #1085）；ACP 在 capability 模式下用 enroll 令牌（cli main.rs:578-589）；OD 线已恢复（#1541、#1544、#1555） | 过期 | 改写安全缺口：「全权 token」改为「一方日常令牌（非 `CreativeRuntime`）」；OD 状态改为 ODX 进行中 |
| 7 | :24 | 「并行线 = COMM：COMM-5b → COMM-6 → COMM-7、DEP-C8」 | 见 #16–#19：COMM-5b 和 COMM-6 已做，C8 部分完成 | 过期 | 同步更新 |
| 8 | :25 | 「**D0 `IN_PROGRESS`**（…D0-6 Mac mini 实测进行中）」 | D0-6 已合并：#722（df747c71，2026-10-07，「feat/decide-d0-6-mini」），889c7f1f「D0-6 补两机实测结论」 | 过期 | D0-6 标 DONE（#722）；D0 整体状态以 PLAN-DECIDE 为准（未核实是否全部收口） |
| 9 | :37 | KB-C0 设计与契约 `REVIEW` | 契约已定稿，见 `docs/knowledge/CONTRACT-*.md`（同文件 :50 自己也这么说） | 部分过期 | 标 DONE 或注明以看板为准 |
| 10 | :38 | KB-C1 主对话只读知识 `BACKLOG` | KB-C1-04 已有大量实现提交（如 6d6b9317、4cf08cc3，10-27）；`release/weknora-r2`（#1515 = bb434b61）已落 main；agent24d 里有 `knowledge_tools.rs`、`knowledge_host.rs`、`knowledge_backend.rs`；ARCHITECTURE-LAYERS.md:154 写「`knowledge_search` / `knowledge_read` 工具」 | 过期 | 改为 `IN_PROGRESS` 或 DONE（以看板为准；看板没有网络，未核实）。:48 说「上表是快照」，建议直接删掉状态列 |
| 11 | :59 | OD-M11（F11.0a…F11.5）`PAUSED` | F11.1「capability 启动与 daemon 所有权」已在产品路径上（backend-manager.ts:135-137）；F11.0「高权限接口隔离」的桌面侧已有 backendProxy 白名单（backend-proxy.ts:1-8，#1538 又收紧）；F11.0a、F11.2、F11.2b、F11.3 仍未做（`persist:agent24-creative`、`workspace_id: None`） | 部分过期 | 逐个 F 标状态，不再整体写 PAUSED |
| 12 | :60 | OD-M12（…上游同步与 fork PR #4）`PAUSED` | 上游同步在做：pin 0.22.2 → 0.26.1（#1544），再到 f5beffcb（#1555） | 部分过期 | 「上游同步」标进行中，并登记 ODX 系列 |
| 13 | :70–73 | K1-1…K1-4（ADR-K1-01…04）`PLANNED` | 四份 ADR 都是「**状态**：**Accepted**（2026-10-08 jason 确认；PR-Daemon 已 APPROVE）」（docs/adr/ADR-K1-01…04 的第 5 行） | 过期 | 标 DONE（2026-10-08） |
| 14 | :74 | K1-5 通用模块工具调用协议实现 `BLOCKED` | `agent24d/src/module_tool_runtime.rs` 已存在（a2ed970a「K1-5.3c 装配模块工具运行时」，10-08）；`release/kernel-k1-r2`（#1280，10-27）和 `release/kernel-k1-r3`（#1497，10-29）都已合入 main；ARCHITECTURE-LAYERS.md:176 写了 K1 ✅ | 过期 | 标 DONE，附 #1280 和 #1497 |
| 15 | :75 | K1-6 隐私策略、来源标记与启用授权实现 `BLOCKED` | 有大量 K1-6b.* 提交（896ad91a「撤掉 has_remote 过渡门」、2f814620「模块模型回调一律 LocalOnly」，#1380），都已随 K1-R2/R3 进 main | 过期 | 标 DONE 或 `IN_PROGRESS`（具体切片完成度未核实） |
| 16 | :76 | K1-7 模块调用审计、共同授权与 #708 E2E `BLOCKED` | ADR-K1-04 已 Accepted，前置已解除；实现完成度未查 | 部分过期 | 至少改为不再是 BLOCKED（前置已满足），完成度未核实 |
| 17 | :93 | COMM-5b §7 结构约束测试 `BACKLOG` | `agent24-comm/tests/comm_dependency_allowlist.rs:1`「COMM-5b structural boundary」、`comm_zero_run.rs:1`「COMM-5b T2」，由 fc862cf3（#676，10-06）加入 | 过期 | 标 DONE（#676） |
| 18 | :94 | COMM-6 UI（身份、联系人、relay、daemon 状态、导入向导）`BACKLOG` | `apps/desktop/src/renderer/pages/comm/Communication.tsx`（#679，10-06；API 客户端 #678）；`api.ts:258-306` 覆盖了 identity、contact、relay、daemon start/stop 和 probe，:327 有 home import；后续又有 #1535（可用性、备份恢复）、#1543（真机反馈）、#1529（默认 relay） | 过期 | 标 DONE（#678、#679、#1535） |
| 19 | :95 | COMM-7 UI（收件历史、outbox、重试 + 双仓联调）`BACKLOG` | 历史已在 UI 里：`api.ts:377` 调 `/api/v1/comm/history`，backendProxy 也放行了这个路由（#1538）；`outbox` 在 Communication.tsx 里出现 0 次（只在 api.ts 的 import 结果里出现）；重试没查到 | 部分过期 | 改为 `IN_PROGRESS`（历史已做，outbox 和重试未做） |
| 20 | :97 | DEP-C8/C9 Hyphae lock 升级 / 发布包加入 hyphae `BACKLOG` | 锁已升过多次：v0.28.1（#1535）→ v0.29.0（#1546）→ 签名版 v0.30.0（#1551），并加了拒绝信号（#1552）。但 C8 的验收要求是「四平台 client 锁、独立 relay 锁」（docs/Deployment/TASKS.md:71），而当前锁只有 darwin-arm64 和 linux-x64（hyphae.lock.json:2-22）。C9 的「标准包内置 hyphae」实际上改成了按需安装（`agent24d/src/hyphae_install.rs:1`「Pinned Hyphae installation owned by the daemon」） | 部分过期 | C8 改为 `IN_PROGRESS`，写明已到 v0.30.0、缺两个平台和 relay 锁；C9 写明现行方案是按需安装，是否仍要内置需要重新裁决 |
| 21 | :365 | M2-07 证据「本 PR」 | 实际是 #761（abe6fa3a「ab/m2-07-closeout」） | 过期 | 改成 #761 |
| 22 | :367 | **M2 release** `ab/m2` → main `BACKLOG` | #767 已于 2026-10-08 合并（73d87f0b「Merge pull request #767 from iDoris-ai/ab/m2」）；roadmap.md:63 也写了「✅ 收口」 | 过期 | 标 DONE（#767） |
| 23 | :396–500 | 旧 M1 的 F1.1–F1.3，T1.1.1 `READY`、T1.1.2…T1.3.3 `BACKLOG` | :401 已声明「下方各条的 READY/BACKLOG 状态为历史快照」，但小标题上的状态没改；这些都已由 M1-T01…T05 交付（:375-379，#636–#648） | 过期（已自声明为历史） | 删掉这 100 多行（M1-PLAN-v2.md 有出处），或者把每个小标题的状态改成 `SUPERSEDED → M1-Txx` |
| 24 | :191–199 | 「2026-09-22 待办清单」中「P3…未开始：T13（os-sdk）+ T14（wire 文档）」 | 这段已在 :191 标为「已被取代」；T13 和 T14 实际已完成：#516（os-sdk v0.1.0）、#585（WIRE-OOP-MODULE），见 :135、:143 | 过期（已自声明取代） | 可以整段删掉 |
| 25 | 全文缺失 | 没有 CORE-5、ADR-038、HW-1、Documenting、iDoris 的台账 | `grep -n "CORE-5\|CORE-2\|ADR-038\|HW-1\|IDORIS" docs/agent/tasks.md` 结果为空。实际已合入：CORE-5f-1（70f1e9a1）、5f-2（#1528）、5f-3（#1540）；ADR-038 C1（#1539）、C3a（#1545）、C3b（#1549）；Hyphae C1b（#1551）和 C2 锁（#1546）；CORE-2 一方外壳登记；HW-1（cb11d7bc #966） | 过期（缺登记） | 增加「CORE 台账」和「组件契约（ADR-038）台账」，或者写明这些线以 COMPONENT-ROADMAP.md 或各自 PLAN 为准 |

**仍准确（合并成一行）**：
- tasks.md:58 M10 DONE（#660）。
- ME-4 台账 :106–149（各 PR 都在历史里）。
- A3 台账 :157–169。
- M1 v2 台账 :375–394：DONE 状态对得上 #636–#720。
- :381 M1-T06b 和 :386 M1-T07b 仍是 `BACKLOG`：`vector.rs` 在 agent 和 daemon 里没有调用方（rust/apps 和 agent24-agent/src 里 grep `vector::` 为空）。
- M2 台账 M2-01…06 DONE（#731、#737、#745、#747、#738、#746）。

**未核实**：
- :109 ME4-0.4（Sin90 仓库）。
- :96 T01-E（Hyphae 侧）。
- :37–41 KB 各工作包的看板状态（没有网络）。


---

## 5. 三份跟踪文档骨架

### 5.1 现有跟踪文档盘点（证据：`wc -l`、`git log -1 -- <file>`，HEAD 9865d7a0）

| 文档 | 行数 | 最后提交 | 自述地位 | 实际情况 |
|---|---|---|---|---|
| `docs/agent/tasks.md` | 506 | 2026-10-27（1f7d60a7） | 「当前唯一权威的执行状态来源（2026-09-19 立，2026-09-28 更新）」（`tasks.md:3`） | 9 个按方向分的台账段（Knowledge、OD、K1、COMM、ME-4、A3、M2、M1 v2、M1），没有 ADR-038 C1–C3、CORE-5 5f、Hyphae 签名等 10-28 之后的线（详见第 4 节） |
| `docs/agent/progress.md` | 117 | 2026-09-30（2f4778dd） | 「此刻仓库真实发生了什么」「更新时间：2026-09-28」（`progress.md:3-4`） | 一个多月没更新 |
| `docs/agent/roadmap.md` | 133 | 2026-10-08（c97d209f） | M1–M6 产品路线 | M1 之后的变化（DM-SPIKE、记忆 P1、组件契约）未反映 |
| `docs/agent/followups.md` | 148 | 2026-11-01（d4053f12） | append-only 账本 | `grep -c '^- \[ \]'` = 82，`grep -c '^- \[x\]'` = 39 |
| `docs/agent/COMPONENT-ROADMAP.md` | 212 | 2026-11-02（37538d78） | 组件 × 里程碑（`tasks.md:12` 称权威配套） | 有维护规则（§5，`COMPONENT-ROADMAP.md:209-212`），是现存唯一写了「谁在什么时候更新」的跟踪文档 |
| `docs/specs/TASKS.md`、`docs/ROADMAP.md`、`docs/PLAN.md` | 608 / 172 / 482 | 09-29 / 09-19 / 09-27 | ROADMAP 自标「⛔ 已作废」（`ROADMAP.md:3`）；tasks.md 把 PLAN §六 与 ROADMAP 标作废（`tasks.md:17`） | 历史 |
| `docs/open-design-workspace/STATUS.md`、`PROGRESS-2026-09-2x.md` | — | — | OD 线自己的状态 | OD 线已 PAUSED（`tasks.md:23`） |
| `docs/tracking/2026-10-26/` | 1 个文件 | — | 只有 `SOURCE-ADR-037-facts.md` | `docs/architecture/README.md:81` 已指出没有统一跟踪文档 |

问题归纳：**状态散在 5 处以上**，自称「唯一权威」的 tasks.md 本身已经落后；只有 COMPONENT-ROADMAP 写了维护规则；没有一份按日期快照，回看「某天的计划是什么」只能翻 git 历史。

### 5.2 设计：`docs/tracking/<YYYY-MM-DD>/` 下三份

放在已有的 `docs/tracking/` 目录，按快照日期分目录（与已有的 `2026-10-26/` 一致）。**最新日期的目录就是当前权威**；旧目录只读、不改。

#### (1) `ARCH-AND-PLAN.md` — 架构 + 方案

```
# 架构与方案（YYYY-MM-DD）
数据来源 / 生成方式 / HEAD sha
## 0. 本期变化摘要（与上一期相比，≤10 行）
## 1. 架构现状一表
   | 层 / 子系统 | 状态（✅🟡📐） | 权威文档 | 代码入口 file | 上次核对 sha |
   （只列状态与链接；层的解释留在 ARCHITECTURE-LAYERS.md）
## 2. ADR 状态快照
   | ADR | 状态 | 实现度（无/部分/完成） | 最近相关 PR | 下一步 |
## 3. 在途方案（设计中、未成 ADR）
   | 方案 | 设计文档 | 拟成 ADR 号 | 等谁拍板 |
## 4. 已知偏离（文档 vs 代码）
   | 文档位置 | 偏离 | 修正 PR / 负责人 |
## 5. 待拍板问题
```

#### (2) `MILESTONES-AND-TASKS.md` — 里程碑 + 任务

```
# 里程碑与任务（YYYY-MM-DD）
数据来源 / 生成方式 / HEAD sha
## 0. 当前主干与并行线（jason 最近裁决，带日期与出处）
## 1. 里程碑总表
   | 方向 | 里程碑 | 集成分支 ab/* | 目标 | 状态 | 依赖 | 计划文档 |
## 2. 按方向的任务表（每方向一节）
   | 任务 id | 标题 | 状态 READY/DOING/IN-PR(#)/DONE(#)/BLOCKED/PAUSED | 负责 | PR | 验收 |
## 3. 暂停与放弃
   | 项 | 何时、谁裁决 | 恢复条件 |
## 4. 跟进项汇总（只放计数和链接，明细仍在 followups.md）
   | 来源 | open | done | 本期新增 |
```

#### (3) `PROGRESS.md` — 进度

```
# 进度（YYYY-MM-DD）
数据来源 / 生成方式 / HEAD sha / 统计区间（上一期 sha..本期 sha）
## 0. 本期一句话
## 1. 本期合入的 PR
   | PR | 方向 | 任务 id | 一句话 | 是否改变架构/ADR 状态 |
   （由 git log <上期sha>..<本期sha> --merges 生成）
## 2. 发布
   | 版本 | 日期 | release PR | 用例验收记录 |
## 3. 阻塞与风险
## 4. 需要 jason 做的事
## 5. 下期计划（只写任务 id，定义在 MILESTONES-AND-TASKS）
```

### 5.3 迁移映射（只给映射，本轮不迁移内容）

| 现有来源 | 去向 | 迁移方式 | 原文件之后 |
|---|---|---|---|
| `docs/ARCHITECTURE-LAYERS.md` §1、§2 各层状态标记、§7 未完成清单 | ARCH-AND-PLAN §1、§4 | 状态标记与清单抽成表；层的解释留在原文 | 保留为解释性文档，状态以跟踪文档为准，文头加指针 |
| `docs/adr/README.md` 状态列 | ARCH-AND-PLAN §2 | 每期复制状态列 + 补实现度 | 保留为索引（唯一发号处） |
| `docs/architecture/EVOLVABILITY-ASSESSMENT-2026-11-01.md` 建议 1–6 | ARCH-AND-PLAN §3 / MILESTONES §2 | 每条建议变一个任务 id | 保留（评估记录不改） |
| `docs/agent/COMPONENT-ROADMAP.md` §1–§3 | MILESTONES §1 | 组件 × 里程碑表直接搬 | 保留组件路线解释；状态以跟踪文档为准 |
| `docs/agent/roadmap.md` M1–M6 | MILESTONES §1、§3 | 未完成的进 §1，暂停 / 不排期的进 §3 | 文头标「历史，见 tracking」 |
| `docs/agent/tasks.md` 各方向台账 | MILESTONES §2 | 每段一节；已 DONE 的历史段不搬，只链回 | 文头改「历史台账，现行见 tracking」 |
| `docs/agent/PLAN-*.md`、`docs/knowledge/PLAN-KNOWLEDGE-TASKS.md` | MILESTONES §2「计划文档」列 | 只链接，不复制定义 | 保留（任务定义权威） |
| `docs/agent/progress.md` | PROGRESS §0、§3–§5 | 当期部分搬；战报段不搬 | 标「停更」，链到 tracking |
| `docs/agent/followups.md` | MILESTONES §4 | 只放计数（`count-open`）和链接 | **保留为明细权威**（append-only 规则不变） |
| `docs/open-design-workspace/STATUS.md`、`PROGRESS-*.md` | MILESTONES §3（OD 线 PAUSED） | 只链接 | 保留为 OD 历史 |
| `docs/specs/TASKS.md`、`docs/ROADMAP.md`、`docs/PLAN.md` §六 | 不迁 | — | 已作废，维持现状 |
| `docs/agent/me3-status.sh` | PROGRESS §1 生成方式参考 | 思路复用：用脚本从 git 生成 | 保留 |

### 5.4 维护规则（谁、什么时候）

| 触发事件 | 谁更新 | 更新哪份、哪节 | 时限 |
|---|---|---|---|
| 每周一（与 AGENTS.md「集成分支每周至少吸收一次 main」同节奏） | 值班的编码代理（PR-Daemon 或笔记本 pilot） | 新建 `docs/tracking/<日期>/` 三份：复制上一期，PROGRESS §1 由 `git log <上期>..HEAD --merges` 生成，其余按变化改 | 当天，单独开 `docs/tracking-<日期>` PR |
| 某 task PR 合入集成分支或 main | 该 PR 作者（代理） | 不改快照；只在 PR 描述里写「任务 id」，供下周生成 PROGRESS §1 | — |
| release PR `ab/<方向>` → main 合入 | 发布负责人 | 当期 PROGRESS §2 追加一行（版本、release PR、用例记录链接） | 合入当天 |
| ADR 状态变化（接受 / 取代 / 废弃） | 改 ADR 的 PR 作者 | 同一个 PR 里改 `docs/adr/README.md`；ARCH-AND-PLAN §2 等下期快照 | 同 PR |
| jason 拍板（新主干、暂停、恢复） | 记录拍板的人 | 当期 MILESTONES §0 / §3 直接改（这是快照唯一允许在期内修改的地方），并写日期和出处 | 当天 |
| 发现文档与代码偏离 | 发现者 | ARCH-AND-PLAN §4 登记；修正走普通 PR | 下期快照前 |

规则补充：
- 快照目录一旦有下一期就**冻结**，只允许修错别字和断链。
- 三份文件开头都写「数据来源、生成方式、HEAD sha」，与本评审文档一致。
- 跟踪文档只放**状态和链接**，任务定义、设计、决策理由仍在各自的计划文档 / 设计文档 / ADR 里，不复制。


---

## 6. 未核实项汇总

| 项 | 原因 |
|---|---|
| GitHub 上的 PR 正文、评论、issue 状态（#661、#663、#1037、#1085）、ruleset `ab-integration` | 本会话没有 GitHub 访问 |
| ADR-038 §3 验收用例是否逐条存在（第 7 条 mock 第二实现、第 11 条 SIGTERM/15 s），C4 AgentEar 用例 2/3/4 | 没有找到对应测试文件，未逐个核对 |
| ADR-037 G-API-1（每条路由都有 Operation 映射）、G-API-3（代码路由与 OpenAPI 一致性 CI）、G-API-5（令牌治理） | 未逐条核对 |
| macOS 发布签名是否在 CI 内完成（`release*.yml` 中 `codesign|notarytool` 零命中） | ADR-038 §7.4 写本机签名，CI 侧未核实 |
| ADR-025 串行推理单槽门、ADR-036 第 5 条云端补位开关 | `agent24-models` 内未找到独立实现，可能由其他机制承担 |
| ARCHITECTURE-LAYERS §7 第 15 条推理回调流式（iDoris P3） | `model_callback.rs` 无 `stream`，未深查 |
| COMM-5b、COMM-6、COMM-7 的 PR 号；nostr-bridge 运行时实际 HOME | 本地 log 未找到 / 未运行 |
| KB 各工作包看板状态、KB-M5 验收结果（只写「见 PR 正文」） | 看板与 PR 正文在仓库外 |
| ME4-0.4（Sin90 仓库）、T01-E（Hyphae 仓库）、Cos72 M4 是否开工 | 在其他仓库 |
| 2.2 表中 ADR-039 `:34,70` 的「ADR-004」与 Documents 相关「ADR-001」（4 处代码注释）的归属 | 原文没写体系，OD 原文里未找到对应条款 |

