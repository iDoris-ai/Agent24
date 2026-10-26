# C2 簇调查：计划、路线图、里程碑与任务（只读调查，2026-10-26）

调查基线：main = `3a433bb0`（2026-10-24 合 #1193）。集成分支取 `refs/remotes/origin/ab/*` 当前尖端。GitHub 数据取自 `gh`（仓库 iDoris-ai/Agent24），查询日 2026-10-26。
证据写法：PR 号均已用 `gh pr view/list` 核对过状态；SHA 写前 8 位。「未核实」表示本次没有拿到证据，不等于不存在。

---

## 0. 先看结论（给协调者）

1. 「权威状态源」`docs/agent/tasks.md`（main，最后改动 2026-10-16）与 git 现实有多处脱节：K1 台账仍写 `PLANNED/BLOCKED`（实际 10-16 已发布 #1022）、M2 release 仍写 `BACKLOG`（实际 #767 已于 10-08 合入）、D0-6 写「进行中」（#722 已合）、COMM-5b/6 写 `BACKLOG`（COMM-6a 已在 main，5b/6b.1/6b.2 在 ab/comm）、`roadmap.md`/tasks.md 仍说 M1-PLAN-v2 在 `ab/m1-memory`（已在 main）。详见第 3 节。
2. 目前有 **9 条活着的集成分支**，其中 4 条（kernel / comm / decide / idoris）距上次吸收 main 已超过一周（违反 AGENTS.md「每周至少吸收一次 main」）。K1 第二批（K1-6a-A*、6b.3–6b.5）约 60 个 PR 躺在 `ab/kernel`，**未发布到 main**，也没有 release PR。
3. 编号严重撞车：`M1/M2`（记忆里程碑 vs Hyphae M1 vs OA-M1/M2 vs v0.5.x）、`D1…`（决策 D1 vs 知识架构 D1–D8 vs M2 裁决 D1–D3）、`K1-1..7`（计划任务）vs `ADR-K1-01..04` vs 实际 PR 的 `K1-5.x/6a.x/6b.x/7.x`、`P0–P5`（记忆）vs ADR-032 P3–P5 vs A3 P0–P4。第 4 节列全。
4. 10-26 产品方向（agent24d 本体 / 每用户常驻 daemon / 开发者 API 仅本机+分权 token / TUI 对齐 / 先 Mac / 移动端只做遥控）里，**现有计划与其有一处直接冲突**：OD-M11 F11.1 与 KB-HOST-01 PR-A2（#1184，在 ab/weknora）都让 Desktop「自己拉起并拥有」daemon（stdin 管道绑死生命周期，桌面退出 daemon 就关，CLI 在桌面运行时不能管 daemon）。这与「每用户一个常驻 daemon」相反。其余多为「完全没有计划」的缺口，见第 5 节。
5. 历史上只写在旧计划里、不在 `AGENTS.md` 的有效约定，见第 6 节（尤其「2026-10-10 全局硬门：所有 PR 须 PR-Daemon 对精确 head APPROVE」只出现在 `PLAN-MEMORY-P1.md`，与 AGENTS.md 的分级门槛矛盾）。

---

## 1. 计划文档逐份清单

格式：路径 | 分支 | 日期 | 定义内容 | 文档自称状态。「分支」指本次读取所用的 ref。

### 1.A main 上的总览/权威类

| 路径 | 日期（最后改动） | 它定义什么 | 自称状态 |
|---|---|---|---|
| `docs/agent/tasks.md`（484 行） | main，2026-10-16 | 自称「当前唯一权威执行状态」。台账：Open Design/第三方线、K1 台账（K1-1…7）、COMM 台账（COMM-0…7、T01-E、DEP-C8/C9）、ME-4 台账（ME4-0.x…6.x）、ADR-032/A3 台账、ME-3 各刀（T4…T9、SUP-1…5、HYG、SHUT、FU-57/60/61/64、T8.5a/b/c-P/c-W、T7a/b、T8、T11）、M2 台账（M2-01…08）、M1 v2 台账（M1-T01…T14）、旧 M1（F1.1–F1.3，已被取代）。顶部「jason 裁决 2026-10-03」：主干=M1；OD 线 PAUSED；并行线 COMM；M1 后第一项 DM-SPIKE（2026-10-06 拍板） | 多处过期，见第 3 节 |
| `docs/agent/roadmap.md`（133 行） | main，2026-10-08 | 产品路线 M1–M6：M1 记忆成为产品（F1.1–F1.3）；M2 M-E 收口（✅10-08）；M3 组织化（等第二个真实用户，不排期）；M4 Cos72 Workspace 底座（F4.1–F4.4，依赖 M1）；M5 Cos72 三件套（F5.1–F5.4）；M6 Workspace 扩展面（不排期） | 顶部三段「现状说明」互相叠加（09-19 暂停、09-23 ME-4、10-03 OD 暂停），M1 一节仍是 08-23 旧文 |
| `docs/agent/COMPONENT-ROADMAP.md`（211 行） | main，2026-10-05（立于 10-03） | 12 个结构组件 C1–C12 的里程碑与波次 A/B/C/暂停；别名 K-1..4、W-1..5、A-1..6、O-1..5、ID-1..6、COMM-*、DEP-*、W3-0 | 「权威（配套）」；C3 现状、COMM 现状已过期 |
| `docs/PLAN.md`（482 行）、`docs/ROADMAP.md`（172 行） | main，09-27 / 09-19（ROADMAP 内容截止 2026-05-13） | Electron/Node 时代的产品计划与 M0–M4c | **已作废**（ADR-026 之前）。tasks.md 与 ROADMAP.md 自己标作废 |
| `docs/specs/TASKS.md`（608 行） | main，09-29（内容截止 2026-07-29） | v0.1–v0.2 时代 loop 任务队列：A0…A6、B1…B6、C1…C8、M-H 的 H1…H12、G1/G2/G3、F3/F4、P0–P4 优先级 | 历史；其中「个人助理优先级 P0–P4」仍可当产品原意参考 |
| `docs/WORKSTATION_PLAN.md`（264 行） | main，2026-05-10 | oMLX/Ollama 本地 AI 工作站规划（oMLX API 速查、64GB Mac） | 「规划中（未开始实现）」；实际 oMLX 已是默认运行时，规划被后续代码超越 |
| `docs/newsletter-plan.md`（105 行） | main，2026-07-24（正文 05-13/15） | Listmonk + Mycelium Blog 的 newsletter 方案，Agent24 机器做运营方 | 无状态字段；未在任何现行台账出现，未核实是否实施 |
| `improvement/README.md` 及 inbox/analysis/decisions | main，2026-09-09 | 长期子项目：TODO-A 架构自检、TODO-B 心智模型校准、TODO-C 先进 agent 调研（Claude Code、Codex CLI、Pi 各一份） | 「已建容器，尚未开工」；用户 08-22「等 milestone 告一段落再开始」；TODO-C 三项全 `⬜ 未开始`；TODO-A 有 2 项已 ✅（mem_checkpoints owner 维 #128、Sin90 迁移 ME-1b-b） |
| `docs/RELEASE-CHECKLIST.md`（v0.1.0）、`-v0.5.0.md`、`-v0.5.1.md` | main，07-24 / 09-30 / 10-01 | 发布清单；v0.5.1 清单精确到文件名并含「Release 中不得出现 .dmg/.zip」机械断言 | v0.5.0、v0.5.1 已执行完；v0.1.0 版仍适用于桌面 dmg 流程 |

### 1.B main 上的里程碑/专项计划

| 路径 | 日期 | 定义 | 自称状态 |
|---|---|---|---|
| `docs/agent/M1-PLAN-v2.md`（229 行） | main，立于 2026-10-01 冻结 | M1 记忆成为产品：F1 判定接缝+personal space（M1-T01、T02）、F2 对话以事件日志为权威（T03–T05）、F3 Retain（T06、T06b）、Recall（T07、T07a、T07b、T07.1/.2、T08）、F4 撤回/REST/UI（T09–T14）。8 条设计决定（无影子写、幂等键、失败语义、摘要契约、no-loss、旧数据恢复边界） | 「已冻结」；执行状态写在 tasks.md 的 M1 v2 台账（T01–T14 除 T06b/T07b 外全 DONE） |
| `docs/agent/PLAN-M2.md`（193 行） | main，2026-10-07 | M-E 收口余项：逐条核实 F4a–e、F5a–c、F3、F6、F7；task M2-01…08；裁决 D1–D3 | M2 台账：M2-01…07 DONE，M2-08 CANCELLED，release `BACKLOG`（过期，#767 已合） |
| `docs/agent/PLAN-ME4-OS-CAPABILITIES.md`（413 行）、`PLAN-OOP-OS-AND-BACKLOG.md`（294 行）、`GOAL-ME4.md` | main，09-29 | ME-3：T1–T14 主链；ME-4：ME4-M0…M6（调度回调、推理回调、SDK、Cos72 mytask、wire 文档、v0.5.0） | 已收口（v0.5.0），台账全 DONE |
| `docs/agent/PLAN-OD-NEXT.md`（94 行） | main，2026-10-03 | OD-M11（F11.0a/F11.0/F11.1/F11.2/F11.2b/F11.3/F11.4/F11.5）→ TPI-W → OD-M12 → OC-0…OC-3；§0 列出 M10「组件落地≠产品路径生效」的安全缺口 | 全部 `PAUSED`（jason 2026-10-03） |
| `docs/open-design-workspace/{PLAN,EXECUTION,PROTOTYPE-FIRST-PLAN}.md` | main，10-01（原文 09-19 / 09-29） | Open Design×Agent24 长期 P0–P9 目标；Prototype-First 里程碑；多 Agent 启动口令 | PLAN 自称「Approved / P2 in progress」，实际 M10 已于 10-03 落地并告一段落，后续 PAUSED |
| `docs/agent/PLAN-DECIDE.md`（131 行） | main 版本 10-07；ab/decide 版本 10-19 更新 | 决策服务 D0（D0-1…D0-8）、D1（规则地板+模型三段阈值）、D2（Guardian+入口路由）、D3（入站分类等）、D4（个人模型）。五档硬件方案 B（8/16/24/32/64GB） | D0 `IN_PROGRESS`（tasks.md），实际见第 2 节 |
| `docs/agent/PLAN-KERNEL-K1.md`（84 行） | main 版本 10-21（立于 10-07，#735） | 4 份 ADR（K1-1…4）+ 实现 K1-5/6/7（每 PR ≤300 行），M0–M3 | tasks.md 台账全 `PLANNED/BLOCKED`；实际 ADR 10-08 Accepted、实现分批进 main（#1022） |
| `docs/agent/PLAN-M1-MEMORY-HARDENING.md`、`PLAN-M1-SNAPSHOT-PROVENANCE.md` | main，10-19 归档入库（#1094，原稿 10-12） | M1 发布后最小修复：R1–R6、H01–H04；P02 运行输入快照来源契约 | 文首自称「归档，不构成 APPROVE，不授权实现」；P02 的迁移号 `0015` 已被占，且与 `0016_run_source_tags` 关系未答；H01 已交付（#1024） |
| `docs/agent/PLAN-SHOWCASE-DOWNLOAD.md`（24 行） | main，10-16 | SD-01：Design 组件下载可暂停/续传/显示速度 | 已发布（#1023，`046c5ba3`） |
| `docs/Deployment/README.md`、`TASKS.md`、`RESEARCH.md`、`HYPHAE-NETWORK-DELIVERY.md`、`HYPHAE-HEALTH-ROLLBACK-CONTRACT.md` | main，10-16 / 10-11 / 10-01 / 10-10 / 10-21 | 阶段 A（DEP-A1…A9 → v0.5.1）、B（DEP-B1/B2/B3/B5 → v0.5.2 签名）、C（C0 自动更新、C1–C3 Windows v0.6、C4–C6 远程/移动 PWA、C7 cargo-dist/Homebrew、C8/C9 Hyphae 锁与打包）、DEP-HN0…HN6（Hyphae 网络交付，≈20–30 人日）、`AGENT24_DATA_DIR`（MAINT-03）用法 | A 全 DONE；B 写 `BLOCKED（账号申请中）`（过期，见第 3 节）；C 多为 BLOCKED/BACKLOG |
| `docs/design/HW-TIERS-capability-hardware-matrix.md`、`docs/decision.md` ADR-036 | main，10-15 | HW-0…HW-5：探测 v2、能力梯、`/api/v1/hardware`、唯一准入点、实测回填；1–6 数字档；云端默认关 | HW-0 拍板（10-14）；HW-1 已合 |
| `docs/SIN90-PET0-INTEGRATION.md` | main，v0.3，2026-09-22 | Sin90 ↔ Pet0 ↔ Agent24 三方集成约定 | 外仓（Sin90、Pet0）自有台账；本仓只存契约 |

### 1.C 集成分支上的计划文档

`git diff --name-status origin/main...origin/ab/X -- docs` 找到的计划类文档：

| 分支 | 计划文档 | 日期 | 定义内容 | 自称状态 |
|---|---|---|---|---|
| ab/memory-p1 | `docs/agent/PLAN-MEMORY-P1.md`（另有 `docs/adr/ADR-MEMORY-P1-LIFECYCLE-AND-PURGE.md`、`ADR-MEMORY-RECALL-MESSAGE-ORIGINS.md`、`R05-0…R05-3`、`MODULE-PROGRESS-EVIDENCE-20261023.md`、`docs/research/MEMORY-P1-READINESS-PROBES-*`、`P1-02A/B-*`） | 2026-10-23 | C3 P1「个人记忆可控」。前置 L01/L02；R01/R06-H02、R02/R05/R06-H03、R03/R04；P1-01 生命周期 ADR；P1-02 容量/F7；P1-03 用量 API+告警；P1-04a/b 附件化+冷归档；P1-05 导出导入；P1-06 双时态/更正（F11）；P1-07 偏好；P1-08a/b/c purge；P1-09 无痕；P1-10 回执/UI；P1-11 联合验收 | 「DRAFT / 待外审」，「全部未派生产」 |
| ab/kernel | `docs/agent/PLAN-KERNEL-K1.md`（同 main 版）；`docs/design/K1-06a-grant-path.md`、`K1-06a-resource-scope.md`、`K1-06b-egress-matrix.md`、`docs/adr/ADR-K1-01…04`；`docs/design/HW-TIERS…`；`PLAN-M2.md`（副本） | 10-07 起，至 10-25 | K1 第二批：K1-6a-R（资源范围）/G（授予路径）/A1–A5、K1-6b.3–6b.5（出口统一与矩阵） | 无单独「第二批计划」，靠 design 文档+PR 标题；tasks.md 台账未更新 |
| ab/weknora | `docs/agent/PLAN-KNOWLEDGE-CONVERSATION.md`（KB-C0–C4）、`docs/knowledge/PLAN-KNOWLEDGE-TASKS.md`（任务方案 v2.1，2026-10-15）、`ADR-KB-01-SINGLE-CONVERSATION.md`、`CONTRACT-{READONLY,STATE,OPS,UI-BRIDGE}-v1.md`、`K1-DEPENDENCIES.md`、`POSITIONING-AND-DIFFS.md` | 10-11 / 10-15 / 10-24 入库 | KB-C0…C4；WS0…WS8；KB-CT-*、KB-C1-*、KB-HOST-01、KB-SEC-01a/b、KB-SVC-POLICY-01、KB-LITE-*、KB-C2-*、KB-C4-*、KB-CTX-*、KB-SOP-*、KB-ORG-*、KB-C3-EVAL | 「任务方案，不是实施授权；KB-C1–C4 仍 BACKLOG」（文字已落后于实际合并，见第 2 节） |
| ab/documenting | `docs/documenting/README.md`（David Xu，2026-10-07 baseline）、`adr/ADR-DOC-01/02/03`、`docs/design/DOC-1-06b-module-tools.md`；`PLAN-KNOWLEDGE-CONVERSATION.md` 副本 | 10-07 起 | DOC-1（slice 1 只读 V1 on S01；slice 2 模板走通）、DOC-2 读/理解、DOC-3 编辑/评审、DOC-4 渲染/交付；Line W、Line K | DOC-1 以 r1/r3/r4/r5 分批发布到 main |
| ab/office-assistant | `docs/agent/PLAN-OFFICE-ASSISTANT.md`（+`.en.md`）、`OA-K-KERNEL-GATE-DESIGN.md`、`OFFICE-ASSISTANT-TECH-DESIGN.md`、`-INTERN-BRIEF*.md`、`-INDEX*.md`、`SKILL-FEATURE-PROPOSAL.md`、`research/T007-office-assistant-report*.md` | 2026-10-16 | OA-M1 本地闭环骨架（OA-M1-01…10）、OA-M2 一条真实连接器、OA-M3 复用与边界扩展；决策 D-A（进程外 `office` 模块）、D-B/D-C（内核门 OA-K，建议并入 #735） | 「规划草案，待评审；需 jason 批准才可开工」 |
| ab/comm | `docs/agent/PLAN-COMM6b.md`、`docs/comm/COMM-5b-zero-run.md`、`docs/agent/TASK-maint-03-data-dir-isolation.md`、`docs/design/COMM-HYPHAE.md`（改）、`docs/Deployment/*`（旧版） | 10-07 | COMM-6b.1–6b.4 | 「规划；基线 ab/comm」 |
| ab/decide | `PLAN-DECIDE.md`（改）、`PLAN-DECIDE-D1.md`、`-D1-2A-L1-UNAVAILABLE.md`、`-D1-2B-L2-VALIDATION.md`、`-D1-2C-L3-LAYERS.md` | 10-07 / 10-12 / 10-19 | D1-1…D1-6（ONNX spike、#688 评审 L1–L4、组件目录+按需下载、retain.intent 接入、recall.gate 接入、验收） | D1 子计划均「提案/契约」 |
| ab/idoris | `docs/agent/PLAN-IDORIS-V031.md`、`IDORIS-MANUAL-BUDGET.md`、`docs/iDoris-consumer.md`、`docs/iDoris-integration-and-entry-router.md`（改） | 2026-10-15 | IDORIS-01…07（计划指针、TaskProfile、独占 provider、数据面 key+响应证据、预算交付、双进程组合验收、031 发布证据包） | 「以 bootstrap PR 合入 ab/idoris 为生效点」，已合（#980） |
| ab/maint | 无计划文档（单个 MAINT-01 修复 #949） | 10-14 | — | — |

> 注：`ab/kernel`、`ab/comm`、`ab/decide` 的 diff 里出现的 `PLAN-M2.md`、`Deployment/*`、`documenting/*` 是它们没吸收 main 时的差异假象，不是各自的计划。

---

## 2. 逐方向核实：真实状态

状态词：DONE / IN-PROGRESS / PAUSED / ABANDONED / SUPERSEDED / NOT-STARTED。

### 2.1 已收口的历史主线

| 里程碑 | 状态 | 证据 |
|---|---|---|
| ME-3（进程外领域 OS：T1–T9、SUP-1…5、HYG、SHUT、FU-57/60/61/64、T8.5a/b/c、T7a/b、T8、T9、T11） | DONE | tasks.md 全 DONE；`me3-status.sh` 3a–3g 14 行「已在 main」；T9 #262；T11 #342 |
| ME-4（调度回调、推理回调、SDK 原型、Sin90 M3–M5 门、Cos72 mytask、wire 文档、v0.5.0） | DONE | tag `v0.5.0`@f46194a（#589），Release 2026-09-30；ME4-6.1.3 Mac mini 干净机验收。遗留：ME4-0.4 Codex 补审 `BACKLOG`、ME4-CODEX-DEBT-5/7/8、FU-104（COMPONENT-ROADMAP O-1，波次 A）未做 |
| ADR-032/A3 AgentEar 附着 P0–P2 | DONE | #524–#544；`v0.4.0`（2026-09-29） |
| ADR-032 P3（推理回调流式）/P4（iDoris provider）/P5（提案闭环） | NOT-STARTED | 无 PR；见 2.12 对 P4 的部分重叠 |
| M10 Open Design 集成落地 main | DONE | #660 `ce861e4b`，设计记录 #662 |
| SD-01 Showcase 下载暂停/续传 | DONE | #1023 `046c5ba3`（10-16）；计划 `PLAN-SHOWCASE-DOWNLOAD.md` 的 `ab/showcase-download` 已无用 |
| v0.5.1（CLI macOS arm64/x64 + Linux x64/arm64；桌面 Linux AppImage+deb） | DONE | tag `v0.5.1`@4fb5a89，Release 2026-10-01（DEP-A1…A9 全 DONE）；Sin90 v0.5.1、Cos72 v0.1.1 配套 |

### 2.2 Open Design 后续线

| 里程碑 | 状态 | 证据/原因 |
|---|---|---|
| OD-M11（F11.0a、F11.0、F11.1、F11.2、F11.2b、F11.3、F11.4、F11.5） | PAUSED | tasks.md 与 PLAN-OD-NEXT：jason 2026-10-03「Open Design 线在 M10 告一阶段」。恢复时从 OD-M11 起，F11.4 届时再定。K1/KB-HOST 的接缝文档（ADR-K1-03、KB-HOST-01）明确「该线 PAUSED，不因此自动恢复」 |
| OD-M12、TPI-W、OC-0…OC-3 | PAUSED | 同上；依赖 OD-M11 F11.0a–F11.3 |
| COMPONENT-ROADMAP 里挂在这条线下的 W-1…W-5、A-1、A-2、C5 A-3b 的 TPI-W 验收门 | PAUSED | 同上 |

### 2.3 记忆线

| 里程碑 | 状态 | 证据 |
|---|---|---|
| M1 记忆即产品（C3 P0；M1-T01…T14） | DONE（T06b/T07b 为可选，未做） | release #720 `1e25795c`（10-07）；jason 2026-10-06 在 `c503bf1` 真机验收；T01 #636、T02 #639、T03 #637、T04 #646、T05 #648、T06 #649、T07a #638、T07 #652、T07.1 #674、T07.2/T08 #675、T09 #640、T10 #672、T11 #673、T12 #681、T13 #680、T14 #683。T06b（LLM 抽取）、T07b（向量召回）`BACKLOG`，实质被 C3 P2 吸收 |
| H01 重复撤回审计 ID 碰撞 | DONE | #1024 `dae043dd`（10-16） |
| #794 内置工具 approve_for_session 对 WriteLocal/Exec 生效 | IN-PROGRESS（issue 仍 open） | #1117 法律、#1118 拒绝 session grant、#1119 工作区根内写入规则、#1120 根外写入，均已合 main（10-20/21）；显式根确认/撤销入口、根内免问生产接线、根外恢复、审计仍待核销（见 MODULE-PROGRESS-EVIDENCE） |
| C3 P1 个人记忆可控（PLAN-MEMORY-P1）· 分支 ab/memory-p1 | IN-PROGRESS，未发布，计划自称 DRAFT | 已合入 ab/memory-p1（25 个 PR）：L01 证据/探针/Superfluid（#1146–#1148）、L02 同步 main（#1145）、P1-01 生命周期 ADR 草案（#1149）、R01 红证 + H02-a1/a2/b 审计幂等修复（#1151–#1154）、R03 回归（#1166、#1167）、R05 来源契约与实现（#1150、#1162、#1168、#1170、#1173、#1181、#1185、#1195、#1197、#1205、#1245）、P1-02a/b 容量探针（#1164、#1165）。open：#1247 生命周期 ADR §9 按 10-26 拍板定稿、#1248 R05-5。**未开始**：P1-02c F7 保留/压实、P1-03 用量 API/告警、P1-04 附件化/冷归档、P1-05 导出导入、P1-06 更正/双时态/F11、P1-07 偏好、P1-08 purge、P1-09 无痕、P1-10 UI 回执收口、P1-11 联合验收 |
| C3 P2（向量+RRF、LLM 抽取、K-4 ML Worker）/P3（共享空间，F9 社区子集，O-2b）/P4/P5 | NOT-STARTED | 无 PR |
| 清除权 ADR / F7 体积治理 | IN-PROGRESS | 含在 P1-01/P1-02 内；M2-07 把 F7 移交 C3 P1 且「须优先处理」 |
| C11 F9 其余、F10 | NOT-STARTED（等第二个真实用户） | roadmap M3 |
| C11 F11 `asserted_by` | 归入 P1-06 | COMPONENT-ROADMAP 10-03 裁决 |

### 2.4 产品路线 M2–M6

| 里程碑 | 状态 | 证据 |
|---|---|---|
| M2 M-E 收口余项 | DONE | release #767 `73d87f0b`（10-08）；M2-01…07 = #731、#745、#737、#747、#738、#746、#761；M2-08 CANCELLED（D2：接受检查级 symlink） |
| M3 组织化 | NOT-STARTED（有意不排期） | roadmap：没有第二个真实用户前不做 |
| M4 Cos72 Workspace 底座、M5 Cos72 三件套 | NOT-STARTED | 无任务拆分（roadmap 明写「M1 交付前不拆」，M1 现已交付，但未拆）；Cos72 仅有 mytask 最小样例（外仓 Cos72 v0.1.1） |
| M6 Workspace 扩展面 | NOT-STARTED（有意不排期） | |

### 2.5 K1 内核依赖（#735）· 分支 ab/kernel

| 里程碑 | 状态 | 证据 |
|---|---|---|
| K1-1…4 ADR-K1-01…04 | DONE | 提案 #753/#762/#748/#763（10-07/08），#766 标 Accepted（jason 10-08），#1081 修断链 |
| K1-5 通用模块工具调用协议（5.1 声明/校验、5.2 通告过滤、5.3a/b/c 协议/授权语义/运行时装配） | DONE（已发布） | ab/kernel #771/#773/#778/#781/#790，随 release #1022 合入 main |
| K1-6a 启用授权（6a.1 授权摘要+确认存储、6a.2 判定服务、6a.3 期限/变更/撤销、6a.4 查看/导出/撤销入口、6a.5 收窄可逆草稿例外） | DONE（已发布） | #769/#777/#779/#795/#791，随 #1022 |
| K1-6b.1/6b.2 来源标记、画像合并贯穿恢复委派 | DONE（已发布） | #774/#780，随 #1022 |
| K1-7.1/7.2 K-2 对接与类型化审计、调用审计接线 | DONE（已发布） | #770/#782/#792，随 #1022 |
| K1 第一批发布 | DONE | #1022 `56264ef9`（10-16，squash），发布说明 `docs/agent/K1-RELEASE-NOTES-20261016.md`；同批 #1024/#1025 |
| K1 第二批（仅在 ab/kernel，**未发布**）：6a-R/G 设计（#1121、#1125）、A1a/b（#1130、#1134、#1139、#1140）、A2a/b（#1132、#1133、#1138）、A3a/b/c（#1141、#1143、#1144）、A4a 启用纪元（#1155、#1157）、A4b 确认卡片+授予 POST+OpenAPI（#1160、#1172、#1176）、A5a/b/c 撤销与到期通知、发件箱（#1182、#1190、#1191、#1202、#1222、#1227、#1231、#1236、#1237）；6b.3 统一出站判定（#783）、6b.4a–d 来源分类/记忆分类/会话下限/self-wake（#1180、#1199、#1200、#1203、#1204）、6b.5a–c 出口矩阵与网络类测试（#1209、#1216、#1220、#1235） | IN-PROGRESS | open：#1243（K1-6b.5c-2a）、#1244（6b.5d）、#1239（A3–A5 Low 收尾）、#1183（L-APPR-7 就地修订）。**没有 release PR**。发布说明 §已知缺口：生产代码无 grant 路由、`CloudAuthorized` 无生产写入点、`http_fetch`/`shell_exec` 无统一出站判定——第二批正是在补这三条，但仍默认关闭 |
| #708 macOS 真实产品路径 E2E 验收 | NOT-STARTED（issue open，实为 Documenting 验收 harness） | K1-7 的出口条件挂在它上面 |
| C5 A-3 隐私法律 L-PRV（`docs/laws/PRIVACY.md`） | NOT-STARTED | `docs/laws/` 只有 APPROVAL/CONTEXT/MEMORY/README；K1 计划要求先单独提 PR |
| C1 K-2 审计字段约束 | DONE（随 K1-7.1/7.1b） | #770、#782 |
| C1 K-3 = SPEC-002/B4（OpenAPI 由 Rust 类型生成） | NOT-STARTED | M2-03（#737）只加了路由↔OpenAPI 覆盖门，不是生成 |
| ab/kernel 分支新鲜度 | 违规 | 末次吸收 main：#1016（10-16），约 10 天 |

### 2.6 决策服务 DECIDE · 分支 ab/decide

| 里程碑 | 状态 | 证据 |
|---|---|---|
| D0-1 crate、D0-2 日志、D0-3 探针/档位、D0-4 评测集、D0-5 横评、D0-7 许可证 | DONE | #688、#717、#689、#687、#718（均在 main，10-07） |
| D0-6 两台机器实测 | PARTIAL | #722（main，10-07）只在 M4 16GB 机器完成，SetFit 超时，计划里的 M4 24GB 复测未做（MODULE-PROGRESS-EVIDENCE） |
| D0-8 多尺寸补测+三语+许可证 | DONE（仅 ab/decide） | #734、#726、#727 |
| D1-1 ONNX int8 运行时 spike | DONE（仅 ab/decide） | #760 |
| D1-2 L1 失败层 / L2 答案校验 / L3 layers[] 契约 | IN-PROGRESS | L1 契约 #854/#855、实现 #908；L2 契约 #909、实现 #1033；L3 契约提案 #1035（clestons 对 `ed644992` APPROVE，10-19 合入 ab/decide；**仅契约，未实现**）；L4 ThresholdBands 应用点未做 |
| D1-3 组件目录+按需下载、D1-4 retain.intent 接入、D1-5 recall.gate 接入、D1-6 验收 | NOT-STARTED | |
| D2（Guardian 风险+入口路由 ID-1）、D3、D4 | NOT-STARTED | |
| ab/decide 分支新鲜度 | 违规 | 末次吸收 main：#1018（10-16）；尖端 10-19 后无活动 |

### 2.7 通信 COMM / Hyphae

| 里程碑 | 状态 | 证据 |
|---|---|---|
| COMM-0…4b、COMM-5a | DONE | tasks.md 证据列（548ccae … 9ed3549、2fc383f/ce90b93） |
| COMM-6a（桌面 COMM 类型化客户端 + 通信概览页） | DONE（tasks.md 未记） | #678（`331666ae`）、#679（`5e0b8506`）合入 main 10-06；#676 联调门 |
| COMM-5b 结构约束与零运行回归 | DONE（仅 ab/comm） | #857（10-21）；#830 关闭（被取代）。tasks.md 仍写 `BACKLOG` |
| COMM-6b.1 联系人新增、6b.2 relay 编辑/探测 | DONE（仅 ab/comm） | #768、#776（10-08）；计划 #741 |
| COMM-6b.3 身份创建/默认切换、6b.4 旧 HOME 导入向导 | BLOCKED → 门部分打开 | 门 = Hyphae 发版 + DEP-C8 升级锁。Hyphae v0.26.3 已于 10-24 发布，`hyphae.lock.json` 已在 main 升到 `144555ca`（#1193），但该锁只更新 darwin-arm64、linux-x64 两个平台，且 PLAN-COMM6b 要求「确认锁定产物含 Hyphae #132」未见记录 |
| COMM-7 收件历史/outbox/重试+双仓联调 | NOT-STARTED | |
| Hyphae M1 联调（群历史 A1、三身份读回 A2、非法 npub 分类） | DONE | #1135、#1136、#1137（main，10-22） |
| T01-E 入站收口、收口后入站高层授权与持久化 | NOT-STARTED | |
| DEP-C8 Hyphae 锁兼容晋升 | PARTIAL | #1193 升两个平台；四平台 client 锁+独立 relay 锁+组合验证未做；issue #751 open |
| DEP-C9 标准包携带 hyphae | NOT-STARTED | issue #749 open |
| DEP-HN0 契约冻结 | DONE（Agent24 侧）；Hyphae 侧未核实 | #881（10-10）、#905（10-21）已合 main；TASKS.md 仍写「待跨仓配套 follow-up 评审（非 DONE）」 |
| DEP-HN1…HN6 | NOT-STARTED | 「Hyphae M1 发布门」已于 10-24 关闭（v0.26.3），HN1 的四平台分包/relay manifest 是否交付未核实 |
| ab/comm 分支新鲜度 | 违规 | 末次吸收 main：#1017（10-16） |

### 2.8 Documenting（DOC-1…4）· 分支 ab/documenting

| 里程碑 | 状态 | 证据 |
|---|---|---|
| 设计基线、ADR-DOC-01/02/03 | DONE | #740（main，10-07）、#733、#744（Accepted）、#796 |
| DOC-1 slice 1（导入/渲染/带出处抽取，S01）发布 | DONE（预发布 r1–r5） | main：#875 r1（10-10）、#915 r3（10-13）、#979 r4（10-16）、#1068 r5（10-17，S01 两个本地模型门槛达标 + macOS 真机 21 项）；`documenting-r2-rc1`、`documenting-r3-rc1` 预发布 tag。PR 范围明确**不含** agent 工具/聊天路径、桌面渲染与抽取界面、第 2 片编辑 |
| DOC-1 chat 入口（模块声明工具，DOC-1-06b / #705 / #1142） | IN-PROGRESS | ab/documenting：#1082 设计、#1090、#1097、#1109、#1126 等；DOC-1 README 写明「没有它整个 DOC-1 不验收」 |
| r6 候选（详情+文字预览、导入取消重试、停止/撤销接收端…） | IN-PROGRESS | #1186、#1192、#1194、#1201、#1206、#1213、#1234 等，尚无 r6 release PR；open #1240 |
| DOC-1 slice 2 模板走通（#707）、DOC-2/DOC-3/DOC-4 | NOT-STARTED | 各 epic issue #709/#710/#711 open |
| Documenting 对 K1 的依赖 | 由 K1 第一批满足 | #1022；`has_remote_tier()` 存在远端档位时 `documents` 工具不通告 |

### 2.9 Knowledge / WeKnora（KB-C0…C4）· 分支 ab/weknora

服务端（`iDoris-ai/WeKnora`，`a24/develop@88e80f4`，其 #75 open）不在本仓；以下只算 Agent24 侧。

| 里程碑 | 状态 | 证据 |
|---|---|---|
| KB-C0 设计登记（ADR-KB-01、任务方案 v2.1 入库） | DONE | #1171、#1174（10-24） |
| KB-CT-02a/02b 只读契约、机器 schema、夹具、TS/Rust 双端校验 | DONE | #1044、#1053–#1065 |
| KB-CT-03 状态机/UI 桥/写操作语义契约「规范核心 v1」 | DONE（仅规范核心） | #1113–#1115（取代 #1070–#1077 关闭的草案） |
| KB-SEC-01a 对 K1 依赖与私有数据禁用门 | DONE | #1091 |
| KB-SEC-01b K1 发布后全路径出口验收 | NOT-STARTED | issue #1089 open；与 K1-6b.5 出口矩阵范围重叠（见 4.2） |
| KB-C1-02a wire 类型+MockTransport、KB-C1-02b HttpTransport | DONE | #1110–#1112、#1122、#1124、#1127、#1156、#1161、#1169 |
| KB-HOST-01（#1085）PR-A1/A2/B1a/B1b/B2a/B2b/C/D | DONE（均仅 ab/weknora） | #1177、#1184、#1196、#1207、#1198、#1215、#1219、#1224；issue #1085 仍 open（等 jason 确认「无 binding 的 run 不广告工具」，但 PR-D #1224 已实现该行为） |
| KB-C1-04 KnowledgeProvider | IN-PROGRESS | 第 1、2、3a、3b-1、3b-2、3c-1、3c-2a-1/2、3c-2b 片（#1221、#1223、#1225、#1226、#1228、#1229、#1230、#1233、#1238）；契约小补丁 #1241 open |
| KB-C1-05 knowledge.search/read Tool+引用 | IN-PROGRESS | a-1 证据包裹 #1232 已合；a-2-1 调用层 #1242 open |
| KB-C2（隔离知识页）、KB-C3、KB-C4、KB-LITE-*、KB-CTX-*、KB-SOP-*、KB-ORG-* | NOT-STARTED | 台账 `BACKLOG` |
| 「真实私有数据启用」 | 被阻 | 要 KB-C1-03（服务端授权，外仓）、KB-SEC-01b、KB-SVC-POLICY-01、K1 默认路径的授予入口 |

### 2.10 Office Assistant（OA）· 分支 ab/office-assistant

| 里程碑 | 状态 | 证据 |
|---|---|---|
| 规划（PLAN、TECH-DESIGN、OA-K 内核门设计、实习生简报三语、T007 报告） | DONE（文档） | 10-16 起，`ab/office-assistant` 合入文档（merge `876d6526`，10-20） |
| OA-M1 本地闭环骨架 | PAUSED（正式口径）／文档与 git 矛盾：外围小任务仍在走 | 正式口径：MODULE-PROGRESS-EVIDENCE 与 PLAN-MEMORY-P1 §6「继续暂停」；计划本身写「需 jason 批准立项」。但 git 上有：OA-M1-09 泰语界面校对 #1163（已合，10-24）、#1175、OA-M1-02 样本检查脚本 #1188/#1189、OA-BRIEF-TH #1208（均 open）。OA-M1-01 语义说明、-04…-08（记录/模型/确认卡/mock 连接器/取消重试）未见 PR |
| OA-K 内核门（建议并入 #735） | NOT-STARTED | 仅设计文档；K1 第二批的授予路径是其前置之一 |
| OA-M2、OA-M3 | NOT-STARTED | 被 OA-K、consent 授予入口、出站判定卡住 |

### 2.11 iDoris 集成 · 分支 ab/idoris

| 里程碑 | 状态 | 证据 |
|---|---|---|
| IDORIS-01 计划与消费方指针 | DONE（仅 ab/idoris） | #980 |
| IDORIS-02 TaskProfile 贯穿调用边界 / 共享编解码 | DONE（仅 ab/idoris） | #981（02a）、#983（02-04 独占网关） |
| IDORIS-03 专用 provider+独占路由 | DONE（仅 ab/idoris） | #983 |
| IDORIS-04 数据面 virtual key+响应证据 | DONE（仅 ab/idoris） | #984（04b 阻断子进程继承凭据）、#1028（回执） |
| IDORIS-05 扣除 core 后的字节预算交付 | DONE（仅 ab/idoris） | #982 |
| IDORIS-06 双进程组合验收 | DONE（仅 ab/idoris） | #1029（pair acceptance） |
| IDORIS-07 031 发布证据包 | NOT-STARTED | 未见 PR |
| IDORIS-08/11/12 测试夹具清理与诊断 | DONE | #985、#1040、#1100 |
| IDORIS-09 验收分支未提交工作分流 | 待 owner 决定 | issue #1101 open |
| #1067 允许 Baton GrokBot 的 CLA | ABANDONED（PR 关闭未合） | |
| ID-1（入口路由生成 TaskProfile）、ID-3、ID-4（跨平台后端）、ID-5、ID-6 | NOT-STARTED | COMPONENT-ROADMAP；IDORIS-02/03 部分覆盖 ID-2/ID-3 的一部分（见 4.2） |
| 出境脱敏 | NOT-STARTED | 先要 C5 A-3 隐私法律 |
| 分支新鲜度 | 违规 | 未见吸收 main 的 sync PR；尖端 10-20 |

### 2.12 硬件分档 HW-TIERS、维护

| 里程碑 | 状态 | 证据 |
|---|---|---|
| HW-0 设计定稿（jason 10-14 拍板） | DONE | #952、ADR-036（#964） |
| HW-1 `agent24-hw` crate、探测 v2、预算、decide 改读平台档位 | DONE | #966、#1025、#1026（v1 兼容 checker）、#1107；`agent24 hw probe` 等细节未逐条核 |
| HW-2 能力梯 schema/解析器/CI 准入/公开表生成 | NOT-STARTED | |
| HW-3 `/api/v1/hardware`、`/capabilities/plan`、首次运行页 | NOT-STARTED | IDORIS-05 只交付了「预算字节传给 iDoris」一小段 |
| HW-4 唯一准入点 lease+加载前复核+云端开关 | NOT-STARTED | |
| HW-5 实测回填 | NOT-STARTED | |
| MAINT-01（iDoris Admin token 帧校验） | DONE（仅 ab/maint） | #949，未发布到 main |
| MAINT-03 数据目录隔离（`AGENT24_DATA_DIR`） | DONE | #1015（main，10-16），文档在 Deployment/README |
| MAINT-02 | 未核实有无 | tasks 无记录 |

### 2.13 部署与发布 Deployment

| 里程碑 | 状态 | 证据 |
|---|---|---|
| 阶段 A / v0.5.1 | DONE | 见 2.1 |
| 阶段 B / v0.5.2（DEP-B1 证书、B2 dmg 签名公证、B3 CLI+模块签名、B5 发布） | 实际 PARTIAL：能力已具备，正式 v0.5.2 未发 | 台账全写 `BLOCKED（账号申请中）`。事实：已有 Developer ID 签名+公证的 arm64 dmg 预发布 `v0.5.1-preview.20261014.1`、`v0.5.1-preview.20261016.2`（构建自 main@0b8325b0，notarytool Accepted，spctl accepted）。**没有 v0.5.2 tag**；CLI 与模块包的签名公证（B3）、Intel dmg 未见 |
| DEP-C0 自动更新、C1 Windows 签名方式、C2/C3 Windows 移植与 v0.6 | BLOCKED/NOT-STARTED | DEP-A7 设计已 DONE（#610/#611）；实现被 C1 与 sidecar-host 落地卡 |
| DEP-C4 远程访问设计、C5 移动端设计（PWA）、C6 实现 | NOT-STARTED（BACKLOG） | 等「移动端方向」拍板 |
| DEP-C7 cargo-dist/Homebrew | NOT-STARTED（BACKLOG） | |
| DEP-C8/C9、HN0–HN6 | 见 2.7 | |
| 桌面端 Linux 包 | DONE | v0.5.1 AppImage+deb |

### 2.14 其他

| 项 | 状态 | 说明 |
|---|---|---|
| Sin90 / Pet0 | 外仓为主 | Sin90 M0–M5 已 DONE（ME4 台账门 `135ddb7`）；Pet0 与 SPIKE-00 对账不在本仓台账，未核实 |
| improvement/ TODO-A/B/C | NOT-STARTED（有意延后） | 用户 08-22 指令 |
| #1037 全局 Skill 能力（加载器/三源分发/导入/快速创建） | NOT-STARTED（待立项） | |
| docs/PLAN.md、ROADMAP.md Electron 时代 M0–M4c | SUPERSEDED | 被 ADR-026 Rust 重写、tasks.md/roadmap.md 取代 |
| docs/specs/TASKS.md loop 队列 | SUPERSEDED（历史完成） | v0.1/v0.2 |
| WORKSTATION_PLAN | SUPERSEDED（oMLX 已是默认运行时） | 内容有 oMLX API 速查价值，建议并入参考文档而非计划 |
| newsletter-plan（Listmonk） | 无证据已实施；未核实 | 不在任何现行台账 |

---

## 3. 计划勾选与 git 不一致（落后/超前）

### 3.1 落后（git 已完成，文档仍说未完成）

| 位置 | 文档写的 | git 事实 |
|---|---|---|
| `tasks.md` M2 台账末行 | `M2 release` `BACKLOG` | #767 已合 main（10-08） |
| `tasks.md` K1 台账 K1-1…7 | `PLANNED`/`BLOCKED` | K1-1…4 Accepted（10-08）；K1-5/6a.1–5/6b.1–2/7.1–2 已发布（#1022）；Documenting 的 PLAN 自己也承认「台账滞后」（PLAN-OFFICE-ASSISTANT §0.2） |
| `tasks.md` 顶部 D0 | `D0 IN_PROGRESS`，D0-6 进行中 | D0-6 PR #722 已合，缺 24GB 复测；ab/decide 已推到 D1-2c（MODULE-PROGRESS-EVIDENCE 已给出修订建议） |
| `tasks.md` COMM 台账 | COMM-5b/6/7 `BACKLOG` | COMM-6a 已在 main（#678/#679）；5b、6b.1、6b.2 已在 ab/comm（#857/#768/#776） |
| `tasks.md` 顶部/`roadmap.md` | M1 执行定义在 `ab/m1-memory` 的 M1-PLAN-v2 | 文件已在 main，分支已无意义 |
| `COMPONENT-ROADMAP.md` C3 现状 | 「M1 在 ab/m1-memory 完成大半，main 上 agent loop 只用 D1」 | M1 已于 10-07 合 main |
| `COMPONENT-ROADMAP.md` C8、波次 A | COMM-5b/6 排波次 A；DEP-C8 待办 | 见上；#1193 已部分满足 C8 |
| `COMPONENT-ROADMAP.md` 隐含「K-1 记忆接线待合回 main」 | | 已合 |
| `Deployment/TASKS.md` DEP-B1/B2/B3/B5 | `BLOCKED（账号申请中）` | 已有签名+公证的预发布 dmg；README「需要 jason 决定」第 1、2 条已变 |
| `Deployment/TASKS.md` DEP-HN1 | 「受 M1 发布门阻塞」 | Hyphae v0.26.3（M1 close-out）10-24 已发布 |
| `Deployment/TASKS.md` DEP-A9 | 「见 PR（本次）」 | 未回填 PR 号（#670 系列） |
| `PLAN-M1-SNAPSHOT-PROVENANCE.md` | 选定 `0015_run_input_provenance.sql` | `0015` 已是 `module_consents`，`0016_run_source_tags`、`0017` 也已占用；store 已到 `0018` |
| `PLAN-KERNEL-K1.md` §4 | 全部 task 未开始 | 见 2.5 |
| `PLAN-KNOWLEDGE-CONVERSATION.md` 状态表（KB-C0 `REVIEW`、KB-C1 `BACKLOG`） | | KB-C0 已落地，KB-C1 已有 17+ 个 PR 合入 ab/weknora |
| `PLAN-IDORIS-V031.md` 「本计划以 bootstrap PR 合入为生效点」 | | #980 已合 |
| `roadmap.md` 顶部叠加三段「现状」 | 09-19「M1–M6 全未开工，本轮不做」 | M1、M2 已完成；该段已不成立（文首仅标注「已不再成立，保留作历史」） |

### 3.2 超前（文档写已定/已进行，git 查无）

| 位置 | 文档写的 | git 核实 |
|---|---|---|
| `PLAN-MEMORY-P1.md` §5 「用户 2026-10-10 已生效的全局硬门」 | 所有 PR 均须 PR-Daemon 对精确 head APPROVE | `AGENTS.md` 在 main 与全部 ab/* 上都**没有**这条；AGENTS.md 仍是「普通 task 可先合」。只存在于该计划 |
| `OFFICE-ASSISTANT-INTERN-BRIEF.zh-CN.md` | 「低风险 PR 在 CI 通过且检查项已回应后即可合并」 | 与上一条矛盾，同样没有出处 |
| `tasks.md` 顶部「COMM 并行线…DEP-C8」 | 排为近期 | 实际只做了一半（锁升级），其余无进展 |
| `Deployment/TASKS.md` HN0 | 「待跨仓配套 follow-up 评审（非 DONE）」 | Agent24 侧 #881/#905 都合并；是否「超前」取决于 Hyphae #172/#173 之后的互链 PR，未核实 |
| `PLAN-OFFICE-ASSISTANT.md` D-A/D-B/D-C | 「作者拍板 2026-10-16」 | 只见于该文档；无 ADR、无 #735 登记 |
| `PLAN-DECIDE.md` 五档方案 B（8/16/24/32/64GB） | 「jason 2026-10-07 拍板」 | 与 HW-TIERS 的「1–6 数字档」并存；HW-1 已把 decide 内存档改读平台档位，旧 5 档表实际已被覆盖（见 4.3） |
| `COMPONENT-ROADMAP.md` 波次 A | 「ID-1 画像（规则版）」「K-2」 | K-2 已做；ID-1 没有任何 PR |

### 3.3 分支新鲜度（硬约束：每周至少吸收一次 main）

| 分支 | 尖端 | 末次吸收 main 的 PR | 相对 main 落后/领先（commit 数，含被 squash 的父链，仅作量级参考） |
|---|---|---|---|
| ab/kernel | 10-25 | #1016（10-16） | 落后 528 / 领先 267 |
| ab/comm | 10-21 | #1017（10-16） | 落后 528 / 领先 11 |
| ab/decide | 10-19 | #1018（10-16） | 落后 503 / 领先 25 |
| ab/idoris | 10-20 | 未见 | 落后 239 / 领先 36 |
| ab/maint | 10-14 | 无 | 落后 480 / 领先 2 |
| ab/documenting | 10-25 | #1066（10-17） | 落后 49 / 领先 87 |
| ab/weknora | 10-25 | #1102（10-19） | 落后 46 / 领先 201 |
| ab/office-assistant | 10-24 | 未见 | 落后 46 / 领先 8 |
| ab/memory-p1 | 10-26 | #1145（10-23） | 落后 3 / 领先 67 |

另有一批「一次性」分支名在 refs 中（`ab/kernel-NN-*`、`ab/weknora-03a-*`、`ab/memory-p1-47…50`、`ab/apple-preview-cloud-*`），是 task 分支残留，不是集成分支。

---

## 4. 各线之间的依赖与冲突

### 4.1 依赖链（以现状为准）

```
K1 第一批（已发布）
  └─ K1 第二批（ab/kernel，未发布）：授予 POST（默认关）→ 才有真实 consent
        ├─ Documenting chat 入口（#705/#1142 片 E）─ DOC-1 整体验收
        ├─ OA-K 内核门 ─ OA-M2
        ├─ KB-SEC-01b / KB-C1-05 真实数据 ─ KB-CTX-02
        └─ 出境脱敏（需 L-PRV 法律 + iDoris 网关）
记忆：M1(DONE) → P1（PLAN-MEMORY-P1，ADR 外审→H03/R05→F7/容量→导出/purge）→ P2 → P3 → Cos72 M4/M5(O-3/O-4)
      P1-06 更正/F11 ← R05 来源契约；D1 retain/recall 接入 ← M1(DONE) 且 与 P1 来源契约有交集
DECIDE D1-4/D1-5 ← D1-2c L3 契约实现 + D1-3 目录；D2 ← ID-1
Hyphae：Hyphae v0.26.3(已发) → DEP-C8 完整化 → COMM-6b.3/6b.4、DEP-HN1…HN6 → COMM-7 → Cos72 M5 渠道
iDoris：IDORIS-01…06(ab/idoris) → IDORIS-07 发布证据 → release PR；ID-1/ID-3/ID-5 ← 隐私法律 L-PRV
HW：HW-1(DONE) → HW-2 → HW-3(依赖 iDoris recommender 的 allocated_budget_bytes，IDORIS-05 已发出) → HW-4 → HW-5
Deployment：DEP-B(有预发布，需正式 v0.5.2) → DEP-C0 自动更新；DEP-C4/C5/C6 ← 移动端方向拍板 ← 开发者 API 令牌(OD-M11 F11.0，PAUSED)
```

### 4.2 同一件事在多份计划里编号不同 / 范围重叠

| 事项 | 出现的编号 | 说明/建议 |
|---|---|---|
| 向量召回、LLM 抽取 | M1-T06b/T07b（BACKLOG）= C3 P2 = K-4 ML Worker = D1 的 retain.intent 模型 | 三条线都在做「用模型判断是否记」；retain.intent 的 D1-4 与 P1/R05 的 WriteGate、P2 的候选抽取边界未对齐 |
| 出口/LocalOnly 全路径验收 | K1-6b.5（出口矩阵 a–f）= KB-SEC-01b = Documenting M4 | KB-SEC-01b 明说要「K1 经 main 发布后」，但 K1 的 6b.3–6b.5 还在 ab/kernel，KB 线的前提尚不成立 |
| iDoris provider 接入 | COMPONENT-ROADMAP ID-2 = ADR-032 P4 = IDORIS-03/04 = `iDoris-integration-and-entry-router.md` ID-1…6 | 三套编号；IDORIS-02/03 实现了 ID-2 的主体，却没回写 COMPONENT-ROADMAP |
| 硬件探测/档位 | PLAN-DECIDE D0-3 `HardwareProbe` + 8/16/24/32/64GB 五档；HW-TIERS HW-1 + 1–6 数字档；ID-6 动态硬件推荐；iDoris recommender | HW-1 已合并并让 decide 改读平台档位；PLAN-DECIDE §1.1 的五档表应标注已被 HW-TIERS 取代 |
| 宿主 recall 来源/注入 | M1-T07.1（数据块）、PLAN-M1-SNAPSHOT-PROVENANCE P02、PLAN-MEMORY-P1 R05、K1-6b.1 `run_source_tags`(0016)、KB-CTX-02（自动背景注入门） | 四处都在定义「谁插入了这条消息」；PROVENANCE 文档自己承认与 0016 关系未定；R05 在 ab/memory-p1 已实现「来源与消息同一事务」，KB-CTX-02 的 hook 要复用它 |
| 清除权/体积治理 | M2-07 F7、C3 P1「原文永不删」、决策日志 D0-2 的删除/保留期、KB 原件生命周期 | P1-08 purge 要覆盖决策日志、Documenting 索引副本、Knowledge 缓存，各计划只各管各的 |
| 内核门 | OA-K（OA 计划）、ADR-K1-01（模块工具）、DOC-1-06b、Cos72 | OA 计划说「建议并入 #735」，但 K1 计划里没有它的任务；需要在 K1 计划登记或明确拒收 |
| `has_remote()`/`has_remote_tier()` 保守限制 | K1 ADR-K1-02、Documenting 工具不通告、`memory` 路径、iDoris 网关 | 一旦用户配了任一远端模型，Documenting 工具就整体不通告；IDORIS-04（LocalOnly 看 locality 回执）上线后才能放宽 |
| Desktop 持有 daemon | OD-M11 F11.1（PAUSED）与 KB-HOST-01 PR-A2（已合 ab/weknora）做同一件事：capability 模式拉起并拥有 daemon | PR-A2 没有恢复 OD-M11，但已经把其中一半落地了；OD 线暂停声明与此事实不一致 |
| 桌面复用 CLI daemon | DEP-A5（复用 legacy daemon）vs KB-HOST-01（capability 模式不复用，CLI 退让） | #1187（open）已指出复用路径只做无鉴权健康探测 |
| 「Workspace」 | L2 文件系统工作区（OD-M11/W-*）、Cos72 社区 Workspace（roadmap M4）、SPEC-ORG-SPACE F10、Documenting「product Workspace」、Knowledge KnowledgeView | COMPONENT-ROADMAP 的 W-N 命名收敛仍是「随 W-3 一起做」，W-3 PAUSED |
| 发布门 | AGENTS.md「安全类 task 等 APPROVE」；PLAN-MEMORY-P1「所有 PR 等 APPROVE」；PLAN-IDORIS「授权/wire/数据写入等精确 head PRD 批准」；COMM6b「高风险片等 APPROVE」 | 四种口径 |

### 4.3 编号撞车清单

| 编号 | 含义 A | 含义 B | 含义 C |
|---|---|---|---|
| M1 | 记忆成为产品（roadmap） | Hyphae M1（群聊/outbox，v0.26.3） | OA-M1；`ab/m1-memory`；mac 笔记本（口语） |
| M2 | M-E 收口（PLAN-M2） | OA-M2；Hyphae「不是 Hyphae M2」 | v0.5.2 |
| D0…D4 | 决策服务阶段 | DOC-1…4（Documenting，原 D1–D4，已改名避让） | M2 裁决 D1–D3；知识架构决策 D1–D8；ME4 决策 D1–D4；ME3-SUP 用户裁决 D1–D4 |
| K1-1…K1-7 | PLAN-KERNEL-K1 任务（K1-1…4 = ADR task） | `ADR-K1-01…04`（文件名） | PR 实际用 K1-5.x、6a.x、6b.x、7.x；COMPONENT-ROADMAP 又有 K-1…K-4 |
| P0…P5 | C3 记忆阶段 | ADR-032 P3/P4/P5；A3 设计文档的 P3/P4（与 ADR-032 同义异号） | OD 长期 P0–P9；IDORIS 之前的 P4 门 |
| P1-01…P1-11 | PLAN-MEMORY-P1 task | | |
| ID-1…ID-6 | 入口路由/iDoris 提案 | IDORIS-01…07 | K1 的「ID-1/3」指同一份提案 |
| COMM-6 | COMM-HYPHAE §8 的整块 UI | COMM-6a（已合 main）、COMM-6b.1–6b.4（ab/comm） | issue #750/#752 用 `[COMM][A-INVITE]`、`[A-SETUP]` |
| DEP-C8/C9 | 原 Hyphae 锁/打包（Deployment） | issue #751/#749 `[DEP-C8][A-LOCK]`、`[DEP-C9][A-INSTALL]` | DEP-HN0…HN6 |
| KB-C0…C4 | 验收包 | KB-CT-*（契约）、KB-C1-0x（任务）、WS0…WS8、KB-HOST-01、KB-LITE-* | Line K/W（Documenting 的线） |
| T1…T14 | PLAN-OOP 主链 | T1.x.y（旧 M1 task）、T01-E（Hyphae）、T4…（ME3-T4…） | 另有 D4b、T006/T007（调研报告） |
| FU-n | followups.md | | `FU-` 前缀也在 PLAN-M2 与 ME-3 台账里混用 |
| HW-1…5 | HW-TIERS 落地 6 片 | 「HW-1」也被 PR 标题/发布说明用作「硬件探测修复」 | |
| R1…R6 | M1 Hardening 风险 | PLAN-MEMORY-P1 的 R01–R06 | Documenting 的 r1…r5（发布批次，小写） |

---

## 5. 计划与 10-26 产品方向的差距

10-26 方向：agent24d 是本体；每用户一个常驻 daemon；开发者 API 只开本机加分权 token；TUI 能力对齐；先 Mac 后 Win/Linux；移动端只做遥控。

### 5.1 已有且方向一致（可直接纳入）

- `agent24d` 已是内核；REST/WS 契约 `protocol/openapi.yaml`；路由↔OpenAPI 覆盖门（M2-03 #737）。
- 单例锁、`daemon.json`、`AGENT24_DATA_DIR` 隔离（MAINT-03 #1015）；CLI/桌面复用已运行 daemon（DEP-A5 #602）；`agent24 service install`（launchd，仅 macOS）。
- Mac 优先已是事实：v0.5.1 macOS CLI 全；桌面 dmg 仅 arm64 预发布，已签名公证。
- 移动端：DEP-C4/C5/C6 已写成「PWA 遥控，经 tailscale/relay」，与「只做遥控」一致；Windows 为 DEP-C（v0.6）。

### 5.2 计划之间的冲突（需要先裁决）

1. **Desktop 拥有 daemon（F11.1 / KB-HOST-01 PR-A2）vs 常驻 daemon**。PR-A2 把 stdin 管道交给 main，桌面退出则 daemon 关；CLI 在桌面运行时 `daemon stop`/需要 daemon 的命令一律「host authority unavailable」。需要裁决：保留「ProductHost 管道」仅用于一个启动期握手（daemon 之后脱离父进程），还是改为 daemon 自己持久化 host 凭据。影响 OD-M11 F11.1 的 exit 条件和 DEP-A5。
2. **DEP-A5「托盘停止只作用于桌面拉起的 daemon」** 与「每用户一个常驻、由系统服务管理」的差异：目前托盘能停 daemon，没有「永不被桌面停」的语义。
3. **K1 默认 loopback** 假设 vs 「开发者 API 仅本机」：现状 daemon 默认 `legacy_single_token`，capability 模式只有 Desktop 管道可用，没有给开发者的发令牌入口。

### 5.3 现有计划里完全没有覆盖的工作项

按 10-26 条目归类。无编号的是新项。

**A. agent24d 是本体 / 每用户常驻 daemon**
- 一份「客户端即薄壳」的架构 ADR：CLI、Desktop、TUI、渠道桥（wechat/nostr-bridge）、Pet0 等全部只经 daemon 的 REST/WS；Desktop 不再拥有业务状态与进程生命周期。（ARCHITECTURE-LAYERS 有层次图，但没有「daemon 先于任何 UI 存在」的约束与验收。）
- 常驻形态的安装与管理：macOS LaunchAgent 随 dmg 安装/卸载/升级、登录项开关、崩溃自愈与重启预算、`agent24 service` 的升级交接（旧 daemon 排空→新版本启动，FU-68 只是提示）、版本不匹配协商（桌面/CLI 与 daemon 版本不一致时的行为）。
- 多 OS 用户隔离：每 OS 用户一份数据根、socket/token 文件权限（`0700/0600` 已有，但没有「同机两个用户同时登录」的测试）、与 `AGENT24_DATA_DIR` 的关系。
- daemon 状态/健康/日志的统一入口（`agent24 daemon status` 已有；面向 Desktop 与 TUI 的统一「诊断包」导出无计划）。
- 升级与数据库迁移的不可回退风险（K1 发布说明已要求「升级前自行复制 agent24.db」）：缺自动备份与回滚方案。
- Linux systemd 与 Windows 服务注册：Deployment 明确列在「不在本计划内」，在「先 Mac 后 Win/Linux」下应改为有编号的后续项。

**B. 开发者 API 仅本机 + 分权 token**
- 令牌签发与管理的产品面：CLI/Desktop 创建、列出、撤销带范围的开发者令牌（范围语言：读记忆、发消息、调用某模块工具、读事件…），过期与审计。现有仅有 capability 令牌基础设施（mint/吊销/TTL/代际），但**路由级资源授权未实现且 PAUSED**（PLAN-OD-NEXT §0）。
- 本机绑定的硬保证：只监听 loopback（含 IPv6）、`Host`/`Origin` 校验、防 DNS rebinding、CORS 白名单；`http_fetch` 拒绝 loopback 是出站侧，入站侧无对应计划。
- 开发者 API 的稳定性契约：版本策略、弃用期、OpenAPI 生成（K-3 / SPEC-002/B4，波次 B，NOT-STARTED）、SDK（TS/Rust/Python）发布与示例。
- 与 K1 模块 consent、OD capability、渠道 token 三套授权的统一模型（ADR-K1-04 提到「UI 与 Agent 共同授权入口」，未覆盖外部开发者）。
- 开发者 API 的限流/配额/用量可见（ME4-4.2.3 只有模块推理用量）。
- 第三方桌面应用（Open Design、KnowledgeView）通过该 API 接入的统一路径：TPI-W 的「一个 workspace + 一个限定令牌」模型在 PAUSED 线里，需要拉出来作为 API 分权的实例。

**C. TUI 能力对齐**
- 没有任何 TUI 计划。现状：`rust/apps/agent24-cli/src/tui/`（app/mod/ui），对 `EventBody` 做穷举；T7b 明确「TUI Approvals 屏幕不扩展」；PLAN-M1-SNAPSHOT-PROVENANCE S3c 只涉及 TUI 对 409 的处理。
- 需要一张「Desktop 页面 × TUI」能力对照与缺口表：聊天、Runs、审批（含模块审批 T7b、K1 确认卡片/授予）、记忆页（列出/搜索/撤回/暂停，M1-T10/T11）、通信（COMM 概览/6b）、Documents、Knowledge、Design（OD 组件下载）、模型与硬件（HW-3）、语音（A3）、设置与诊断。
- TUI 与 Desktop 共享 API 客户端层与事件投影（避免两份手写类型漂移，PLAN-PROVENANCE 已指出 desktop `api.ts` 手写 Run 类型的风险）。
- TUI 的无障碍与 SSH/无显示环境（Linux 服务器）使用场景，与「先 Mac」的次序。
- 国际化：Desktop 已有英/中/泰（#950），TUI 无。

**D. 先 Mac，后 Win/Linux**
- Mac 优先的明确退出门：正式 v0.5.2（签名公证 dmg + 签名 CLI/模块包）、自动更新（DEP-C0）、Intel dmg 取舍、Mac mini 作为发布验收机的约束。目前只有预发布。
- Linux 桌面已经发了（v0.5.1），与「先 Mac」不矛盾，但没有「Linux 桌面维护预算」声明；AppImage 的 `LD_LIBRARY_PATH` 已知问题未回填。
- Windows：DEP-A7 设计已有，C1 签名资格待 jason；缺「Windows 上常驻 daemon 形态」（服务/计划任务）与 TUI/PWA 遥控在 Windows 的位置。
- 统一的平台矩阵文档（哪些能力在哪个平台可用）：HW-2 的「能力 × 硬件」公开表只管硬件，没有「能力 × OS」。

**E. 移动端只做遥控**
- 遥控的最小功能集（jason 须定，DEP-C4 的前置）：看事件流、批准/拒绝审批、发消息、看运行状态？是否允许改配置/授权？
- 远程访问安全设计（配对、令牌、TLS/tailscale/Nostr relay、威胁模型）已列 DEP-C4，未开工；与开发者令牌（B 项）、Hyphae relay（COMM）的复用关系未定。
- 「只遥控」意味着移动端不跑 daemon、不存记忆：需要在记忆 P1 的 purge/导出/无痕设计里写明遥控端不得持有副本。
- Hyphae 作为远程通道的可行性（COMM-7、T01-E 入站授权未收口）与 PWA 路线的取舍。

**F. 跨项**
- 产品入口统一后，COMPONENT-ROADMAP 的波次 A/B/C 需要重排：现有波次按组件排，不含「daemon 本体化」这条主线。
- 10-26 方向与 `ARCHITECTURE-LAYERS.md`、`README`、`docs/Deployment/README.md`「一句话结论」的对外表述需同步。

---

## 6. 历史上已定、只写在旧计划里的有效约定（必须保留）

按出处列。「状态」说明目前是否另有地方记载。

### 6.1 分支、PR、评审

| 约定 | 出处 | 状态 |
|---|---|---|
| 集成分支 `ab/<方向>`，task 分支 `ab/<方向>-NN-<短名>`，PR base 只能是本方向集成分支；约 10 个 task 开一次 release PR；release PR 由人开、PR-Daemon APPROVE + jason 真机验收 | AGENTS.md（10-07）；各计划重复 | 已在 AGENTS.md |
| 集成分支每周至少 merge（不 rebase）一次 main；ruleset `ab-integration` 按**精确分支名**保护，新方向先加名；禁止 force-push | AGENTS.md；PLAN-IDORIS-V031 | 已在 AGENTS.md；**当前 kernel/comm/decide/idoris 违反** |
| 安全/权限/授权/数据写入语义/存储迁移/wire 格式类 task PR 须 PR-Daemon APPROVE 后合入 | AGENTS.md | 已在 |
| **「所有 Agent24 PR 均须外部 PR-Daemon（clestons）对精确 head APPROVE、required CI 全绿且 mergeable；三条件满足立即合入集成分支，不向已批准 head 追加修改」（用户 2026-10-10 全局硬门）** | 仅 `PLAN-MEMORY-P1.md` §5 | **只在这份计划里**；与 AGENTS.md 冲突，需要裁决后写进 AGENTS.md |
| PR ≤300 行，**含测试与文档**（只排除锁与生成文件）；确实不可拆时说明理由 | AGENTS.md 说「不含锁文件/生成文件」；PLAN-MEMORY-P1 明确「测试文档不另计」 | AGENTS.md 未明说测试/文档是否计入 |
| 已 APPROVE 的 PR 禁止再推 commit（dismiss 不可逆）；叠加 PR 的子 PR 先 draft；合并自动删分支会杀死 stacked PR；改 base 会作废 approve | 用户记忆 + tasks.md ME4-1.1.1 注记 + ME-3 T5 注记 | 不在 AGENTS.md |
| 带状态机的项先写一页语义说明、让 Codex/评审先审设计，再写代码（SUP-5 教训，规则提案 jhfnetboy/PR-daemon#8） | tasks.md「ME3-NEXT 执行队列」 | 只在 tasks.md |
| 新回归测试一律做变异验证（把修复改回去确认变红，PR body 写明方式） | tasks.md 全局验收前置；AGENTS.md「测试先行」 | 已在，但「变异验证」措辞只在 tasks.md |
| commit author：派发给代理的任务用 `jhfnetboy <jhfnetboy@gmail.com>`；已签 CLA 的协作者用本人身份（David = stoptheworld-java）；AI 只写 Co-Authored-By | AGENTS.md | 已在 |
| 台账回填规则：task PR 合并后，`DONE`/证据由下一个 PR 顺带回填，或攒到最终台账收口 PR | PLAN-ME4 §一第 7 条 | 只在 PLAN-ME4（已收口计划） |
| 验收分档：「组件落地」「产品路径接线」「验收」「发行」分别记状态，不得互相代替（M10 教训） | PLAN-OD-NEXT §0；PLAN-KERNEL-K1 §3/§5；PLAN-MEMORY-P1 §5 | 分散在三份计划，无统一规则 |
| 判据要带正对照；裸 grep 计数不构成「存在」；假阴性比假阳性贵 | 用户记忆；PLAN-M2 §1「每条判据先给正对照」 | 只在 PLAN-M2 与记忆 |

### 6.2 门禁与环境

| 约定 | 出处 | 状态 |
|---|---|---|
| Rust 门禁三件套（fmt、clippy -D warnings、test --workspace）；`ulimit -n 4096`；所有 worktree 共用主 checkout 的 `CARGO_TARGET_DIR` | AGENTS.md | 已在 |
| Codex 沙箱 4 个白名单失败 | AGENTS.md | 已在 |
| TS 门禁（pnpm frozen install/typecheck/lint/test），改 protocol 额外 `lint:openapi`、`gen:api` 无 diff | AGENTS.md | 已在 |
| 所有开发在笔记本；Mac mini 未经 jason 允许不得执行任何任务；B 机因磁盘不足不再构建 Agent24 | AGENTS.md（10-03）；Deployment/TASKS 验收「禁止在 Mac mini 执行」（HN 系列） | 已在 AGENTS.md；Deployment 的 HN 干净机验收要求隔离身份 |
| pnpm only | CLAUDE.md/AGENTS.md | 已在 |

### 6.3 发布规则

| 约定 | 出处 | 状态 |
|---|---|---|
| 发布清单「冻结到文件名」，含机械断言（如 v0.5.1「Release 中不得出现 .dmg/.zip」）；干净机器只用 Release 资产验收；x64 无实机时在 Release Notes 标注「x64 未实机验证」 | RELEASE-CHECKLIST-v0.5.1、Deployment/TASKS A8 | 只在这些文档 |
| tag 过滤 `v[0-9]+.[0-9]+.[0-9]+`，`agent24-os-sdk-v*` 不得触发主流水线；A3 创建 Release，A6 只追加附件，两条流水线不抢同一个 SHA256SUMS 文件 | Deployment/TASKS A3/A6/A8 | 只在这里 |
| 未签名 dmg 不发布（macOS 15 起无法右键放行）；dmg 等签名公证后发；浏览器下载（带 quarantine）验收 | Deployment/TASKS H4/B5 | 只在这里 |
| 发布版本号线：`agent24-os-sdk`/`os-fd` 独立 0.1.0 版本线，不随主版本 | RELEASE-CHECKLIST-v0.5.1 §0 | 只在这里 |
| v0.5.0 的发布授权：跳过 alpha tag；C8 发布（tag/Release/上传）用户已授权 | 用户记忆；tasks.md | 记忆 |
| 预览包与开发环境共存须用 `AGENT24_DATA_DIR`；迁移 0015–0017 为 Simple 迁移不可回退，升级前自行备份 `agent24.db` | Deployment/README；K1-RELEASE-NOTES | 只在这里 |
| 按需下载组件（Open Design、决策模型）：不进安装包，pin 版本+sha256，原子落地，未安装时 `needs-download` | Deployment/TASKS DEP-A9；PLAN-DECIDE §0；PLAN-SHOWCASE-DOWNLOAD | 分散 |
| release PR `ab/memory-p1 → main` 等由人开、人审；每次验收区分「源码/定向测试/HTTP 接线/真机/发布」并记录精确 SHA、命令、退出码、证据路径 | PLAN-MEMORY-P1 §5 | 只在这里 |
| 一次发布一次干净机器验收，覆盖 arm64 Mac mini、Linux x64、Intel runner 冒烟 | Deployment/TASKS A8 | 只在这里 |

### 6.4 产品/架构约束（计划里的「不变量」）

| 约定 | 出处 |
|---|---|
| A 类判断（WriteGate、来源标记、owner/active 过滤、会话导入、Authorizer、capability 令牌、会话视图）永远不进决策服务；后端不可用返回 `unavailable`，不静默降级；`llm_simulation` 不冒充校准概率；决策日志只存本地、不上传、可导出可删除 | PLAN-DECIDE §0、§2.3 |
| 原文 `mem_events` 不因暂停/撤回/摘要/截断而删；撤回=失效+审计；旧压缩已丢的原文不承诺恢复；暂停开关对审批恢复的豁免（OpenAPI putMemorySettings） | M1-PLAN-v2、PLAN-M1-MEMORY-HARDENING |
| 容量满不静默清历史；配额/磁盘满不能报「已记住」 | PLAN-MEMORY-P1 §3、P1-03 |
| 记忆 P0→P1→P2→P3 顺序，未批准并行；F11 放 P1 | COMPONENT-ROADMAP、MEMORY-STRATEGY |
| 一次性 `private/*` 记忆回调、scoped 回调、Policy 回调的推迟条件（等 F8c/F9、等稳定消费者） | SPEC-ME3 §9、COMPONENT-ROADMAP O-2b |
| 不要在 `agent24-domain` 放带命名空间的参数；`CosEntity` 与 `SpaceId` 不得混用；模块面向的 API 不出现 space 参数；Cos72 双向图存模块自己的库，不进 `agent24-memory` | roadmap M4 边界三条 |
| 领域 OS 命名空间 `/api/v1/<name>` + 保留段名单（M2 D1 裁决，F3 关闭）；接受检查级 symlink（D2，F6 关闭） | PLAN-M2 裁决 |
| 第三方工具「一个 workspace + 一个限定令牌」接入模型；`creative_attachment_id` 泛化为 `tool_attachment` | COMPONENT-ROADMAP W-3 |
| ADR-032 冻结顺序：P3 流式 → P4 iDoris provider → P5 提案闭环；A3 设计文档 P3/P4 与之名同义异 | COMPONENT-ROADMAP C9 |
| 隐私法律先于实现冻结，配变异测试；出境脱敏由 iDoris 出口网关执行，Agent24 负责 LocalOnly 贯穿与非 LLM 出站启动断言（A-3a/A-3b） | COMPONENT-ROADMAP C2/C5 |
| 法律变更必须单独 PR；改变可观察行为的 PR 必须点名受影响的法律（L-APPR/L-MEM/L-CTX） | laws/README；PLAN-KERNEL-K1；PLAN-OFFICE-ASSISTANT |
| 「配置远端模型不能使 LocalOnly 失效」；`output_privacy: local_only` 自声明不能当网络许可；未支持的云模式明确 unavailable | PLAN-KERNEL-K1 §1 |
| 模块声明和第一方签名不构成宿主授权；读权限默认不存储，模块工具授予是唯一例外（L-APPR-7 修订 #1183 待合） | PLAN-KERNEL-K1；#1123 |
| 「不允许两套压缩实现并存」；no-loss：摘要失败不丢消息 | M1-PLAN-v2 F2、旧 T1.3.2 |
| 知识线：唯一主对话、专用隔离 KnowledgeView（无 preload、非持久分区、CSP）、集成模式显式允许列表（不是黑名单）、禁止消费既存共享授权、首期不缓存证据、持久 Off+generation | PLAN-KNOWLEDGE-TASKS v2.1、ADR-KB-01 |
| Documenting：单本地用户声明；模型调用 LocalOnly；不新增 `backendProxy` 用法；幂等键语义；originals 永不改，编辑产生新 revision；chat 入口缺失则 DOC-1 不验收 | documenting/README §14、ADR-DOC-01/02 |
| 硬件：整机预算由 Agent24 负责、模型选型由 iDoris 负责（ADR-036）；云端默认关；档位不是自动下载授权；换机/内存压力不自动升档下载 | HW-TIERS §10、ADR-036、PLAN-DECIDE §1.1 |
| Hyphae：client 显式传 `--notify=false --auto-reply=false`；入站默认不触发 run（F4b 冻结）；ACK/进程存活不冒充送达/健康；不得在 Mac mini 做 Hyphae 网络交付验收 | COMM-HYPHAE、HYPHAE-NETWORK-DELIVERY、Deployment/TASKS |
| OA：Agent24 是唯一执行控制面；不引入第二套 Agent 框架；不要求先加载 Sin90/Cos72；批准只绑定「这一个动作+最终 payload 版本」，一次批准不隐含长期授权 | PLAN-OFFICE-ASSISTANT §0.1、§2 |
| 用户偏好：中文、控范围（原型阶段不过度设计，评审只修真 bug，设计最多 1 轮评审即冻结）、M1 等已拍板里程碑不再等确认 | 用户记忆；M1-PLAN-v2 / Deployment/TASKS 评审注记 |

---

## 7. 对整合的建议（供协调者参考，非本次任务范围）

1. 第二份文档（里程碑/任务/执行计划）建议以「方向」为一级，每个方向只保留：目标、分支、编号体系、当前状态、证据、门（依赖/待拍板）。上面 2.x 节可以直接作为原料；各方向的「编号」建议统一前缀并保留旧编号映射表（4.3）。
2. 第一轮需要 jason 拍板才能收口的事：Desktop/daemon 所有权（5.2-1）、AGENTS.md 是否采纳「所有 PR 须 APPROVE」、PLAN-DECIDE 五档表是否正式被 HW-TIERS 取代、K1 第二批何时开 release PR、Office Assistant 是否正式恢复、DEP-B 是否把预发布升格为 v0.5.2。
3. 整合时不要把 `tasks.md` 里 ME-3/ME-4 的逐 PR 台账丢掉：它们是 v0.4.0/v0.5.0 的唯一逐 PR 证据，建议原样归档为「已收口历史」附件。

---

## 附：主要证据 PR 速查

| 事项 | PR/ref |
|---|---|
| M1 release | #720 `1e25795c` |
| M2 release | #767 `73d87f0b` |
| K1 第一批 release | #1022 `56264ef9` |
| Documenting slice 1 | #875 `0ebc3fc1`、#915 `eb7f546f`、#979 `600baba0`、#1068 `1ef85127` |
| H01 / HW-1 / SD-01 | #1024 `dae043dd` / #1025 `0b8325b0` / #1023 `046c5ba3` |
| Hyphae 锁 v0.26.3 | #1193 `3a433bb0` |
| #794 四片 | #1117 `0a46d5de`、#1118 `b4ee445b`、#1119 `ceeec888`、#1120 `2a67f891` |
| v0.5.0 / v0.5.1 tag | `f46194a` / `4fb5a89` |
| 签名预发布 | `v0.5.1-preview.20261014.1`、`v0.5.1-preview.20261016.2`（main@0b8325b0） |
| 集成分支 PR 计数（merged/open） | memory-p1 25/2、kernel 63/4、weknora 52/2、documenting 198/1、office-assistant 1/4、comm 5/0、decide 12/0、idoris 10/0、maint 1/0、m2 7/0、m1-memory 18/0 |
