# C3 簇：进度 / 状态 / 交接 + 内外链接（只读调查，2026-10-26）

基线：main = `3a433bb0`（2026-10-24，#1193）；集成分支取自 `refs/remotes/origin/ab/*`（memory-p1 tip 已是 `a9ba2ed3` / #1245）。
方法注记：(1) release 合并有两种形态：K1 是 squash（#1022），documenting / m1 / m2 是 merge commit。所以 `rev-list origin/main..ab/kernel` 的 267 被严重高估，下表给两个口径。(2) "今天是否成立"一律以 git 合并历史或 gh 状态为依据。

---

## 一、Agent24 内部进度类文档（路径｜日期｜覆盖线｜当时结论｜今天是否成立）

| 路径 | 日期 | 覆盖线 | 当时结论 | 今天 |
|---|---|---|---|---|
| `docs/agent/progress.md` | 头写 09-28，末段 09-30，最后提交 09-30 | ME-4（调度/推理回调、SDK、A3 AgentEar 附着）、v0.4.0 -> v0.5.0 发布、一夜无人值守战报 | 头部"下一步 v0.4.0 发版"；v0.5.0 干净机验收通过；遗留 ME4-CODEX-DEBT-10、FU-104/105 | **过期**。头部"当前聚焦/下一个 READY"仍写 v0.4.0 待发，实际 v0.4.0 09-29、v0.5.0 09-30、v0.5.1 10-01 均已发。10-01 之后的 M1/M2/K1/COMM/Documenting/Memory-P1 全无。只有史实段可用 |
| `docs/agent/me3-status.sh` | 最后提交 09-26 | ME-3 各刀探针，读 origin/main | "状态由探针给，不手写" | 机制仍成立，ME-3 / ME-4a 行有效；不含 10-01 之后任何线。是脚本不是叙述 |
| `docs/agent/HANDOFF-MEMORY-HARDENING.md` | 10-12（回家暂停检查点，后追加恢复记录），最后提交 10-16 | 记忆正确性/隐私修复线 `ab/m1-memory-hardening` | 暂停->恢复；H01 重复撤回修复 PR #910；P02 来源契约 BLOCKED_NEEDS_CONTRACT | **过期**。#910 已 10-13 合入该集成分支，H01 以 #1024 落 main（10-16）；集成分支已删；工作由 `ab/memory-p1` 接手。文中 orca worktree、`/tmp/a24-memory-hardening/` 需另核 |
| `docs/agent/HANDOFF-ORCA-2026-10-08.md` | 10-08 晚（自称快照，要求现查），最后提交 10-16 | 总指挥交接：K1 主线、决策服务其次、COMM 并行、其余暂停 | "K1 剩 4 片 + E2E，ab/kernel 落后 main 50 提交" | **大部分过期**。K1 已 squash 发布（#1022，10-16）；ab/kernel 已进入 A5/6b.5（10-16 后 40 个合并）；决策线停在 D1（最后合并 10-12）；documenting / weknora / memory-p1 / office-assistant 四条线它没有。**仍有效**：§2 硬规则（评审后禁推 commit、delete_branch_on_merge 陷阱、author=jhfnetboy、pre-pr-check）与 §7 现查命令，宜并入协作规则而非进度 |
| `docs/open-design-workspace/STATUS.md` | 头 09-29（Prototype-First 转向），正文冻结 09-23，最后提交 10-01 | Open Design 集成，门禁表 P0-P9、A24-OD-00..05 | 序列化 workspace/sidecar 安全栈；"M1-M4 happy path 已闭环" | **过期**。M10 已 10-03 落 main（#660，#662 补设计）；10-03 裁决 OD 线暂停（OD-M11/12、TPI-W、OC 全 PAUSED）。门禁表"IN PROGRESS"非现状 |
| `.../PROGRESS-2026-09-22.md` | 09-23 快照 | OD workspace 概念模型与阶段进度 | 约 25-30% 产品路线 | 过期（被 M10 取代）；概念模型一节可作 ADR 背景 |
| `.../PROGRESS-2026-09-28.md` | 09-28 | OD 小步台账，当日 8 个 PR（#555-#562） | 全 OPEN | 过期：#556 等已合入 integration 分支，经 #660 落 main |
| `.../PROGRESS-2026-09-29.md` | 09-29 | OD Prototype-First M1-M4 | #563 launcher 等 | 过期（#563 09-29 已合；M10 已落 main） |
| `.../HANDOFF-2026-09-27.md` | 09-27 | OD capability 栈 #241/#242、G8 | 待 fresh external review | 过期：#241/#242 均 10-03 MERGED。此文件在所有 ab/* 分支上都有，内容相同 |
| `CHANGELOG.md` | 最后提交 10-08；最新条目 `[0.5.1] 2026-10-01` | 全仓库发布记录 | `[Unreleased]` 只有 DOC-1 骨架与 D0-1 | **部分过期**。0.5.1 及以前是史实；Unreleased 缺 K1(#1022)、M1/M2 release、HW-1、Documenting r1-r5、COMM v0.26.3 锁、#794 修复；4 个预览 tag 也没有条目 |
| `docs/RELEASE-CHECKLIST.md` | 07-24 | v0.1.0（通用流程：安装器 / mother test） | 勾选框全空 | 通用流程仍被 0.5.x 清单引用，内容 v0.1.0 专用 |
| `docs/RELEASE-CHECKLIST-v0.5.0.md` | 09-30（冻结） | v0.5.0 三仓库发布 | 精确到文件名 | 史实，已执行 |
| `docs/RELEASE-CHECKLIST-v0.5.1.md` | 10-01（冻结） | DEP-A8 多平台发布 | CLI mac/linux + 桌面 Linux | 史实，已执行（v0.5.1 = Latest） |
| `docs/VERIFICATION.md` | 05-13 | M1-M3 + M4a/b/c BoxLite（"feat/m4c 待合并"） | 手工验证手册 | **严重过期**：Node/Electron 时代，仍写 `AuraAIHQ/Agent24` 克隆地址，未覆盖 Rust core |
| `docs/SOAK-F5.md` | 最后提交 10-01 | F5 7x24 泡测 runbook（Mac mini 7 天） | 代码侧就绪，时钟只能在机器上走 | 方法有效；仓库里没有"是否跑过"的结果记录；依赖 Mac mini（需 jason 允许） |
| `docs/demo/2026-09-27-live-demo/README.md` | 09-27 | Agent24 + Sin90 现场演示 | main@f504ae0 + oMLX 全链路通 | 史实证据；`run-demo.sh` 写死 `/Users/jason/Dev/auraai/sin90-design` |
| `docs/demo/2026-09-30-v050-desktop-audit/README.md` | 09-30 | v0.5.0 桌面真机验收 | 基线 f46194a9，只验证不改码 | 史实证据 |
| 同类未点名：`docs/agent/tasks.md` | 最后提交 10-16，头写"唯一权威执行状态" | 总台账 | "当前状态（2026-10-03）" | 部分过期：10-03 之后（K1 发布、Documenting、Memory-P1、WeKnora、OA、Hyphae 锁升级）多在分支文档中，未回写 |
| 同类：`roadmap.md`(10-08)、`COMPONENT-ROADMAP.md`(10-05)、`followups.md`(10-08)、`M1-ACCEPTANCE.md`(10-06)、`Deployment/TASKS.md`(10-11)、`PLAN-DECIDE.md`(10-07)、`K1-RELEASE-NOTES-20261016.md`(10-20) | | | | 均停在 10-16 之前；M1-ACCEPTANCE 的框全空（M1 已于 10-06 验收、10-07 #720 合 main，文件未回写） |
| **仓外** `/Users/jason/.pi/agent/handoffs/HANDOFF-MEMORY-P1-20261024.md` | 10-24 | Memory P1 | tip `09771b6d`；#1166/#1167 待 clestons | **过期**：#1166/#1167 已合（`2fabb228`/`5ad99157`），被 10-26 版取代 |
| `.../HANDOFF-MEMORY-P1-20261026.md` | 10-26 11:20 | Memory P1 | tip `b9e4eb5f`；#1245 H03 收口说明等待 clestons | **已被超越**：origin/ab/memory-p1 现 `a9ba2ed3`，#1245、#1246 已合；#1247/#1248 新开（#1248 CHANGES_REQUESTED）。文末"接棒第一条命令"校验 tip 会不匹配。**硬门 8 条仍有效** |
| `/Users/jason/Dev/auraai/Agent24-archive/20261026/` | 10-26 | 分支清理归档 | 见 §二.6 | 有效记录 |

**其他 HANDOFF/STATUS/PROGRESS 文件**：main 与各集成分支上只有上表三处（`docs/agent/HANDOFF-*`、`docs/open-design-workspace/HANDOFF-2026-09-27.md`）；对 9 条集成分支做 `--diff-filter=A` 没有新增同名文件。集成分支上新增的状态类文档：memory-p1 的 `MODULE-PROGRESS-EVIDENCE-20261023.md`、`R05-0..4*.md`、`PLAN-MEMORY-P1.md`；weknora 的 `docs/knowledge/PLAN-KNOWLEDGE-TASKS.md`、`CONTRACT-{STATE,UI-BRIDGE,OPS,READONLY}-v1.md`、`K1-DEPENDENCIES.md`；kernel 的 `PLAN-KERNEL-K1.md`、`design/K1-06*.md`；decide 的 `PLAN-DECIDE-D1*.md`、`research/jev-study/*`；idoris 的 `PLAN-IDORIS-V031.md`、`IDORIS-MANUAL-BUDGET.md`、`docs/iDoris-consumer.md`；office-assistant 的 `PLAN-OFFICE-ASSISTANT*.md`、`OFFICE-ASSISTANT-INDEX*.md`；comm 的 `PLAN-COMM6b.md`、`docs/comm/COMM-5b-zero-run.md`。

---

## 二、实际进展（以 git 为准）

### 1. 已发布版本

| 版本 | 日期 | 形态 | 包含的线 |
|---|---|---|---|
| v0.1.0 | 07-24 | Release | Rust core + 桌面基线 |
| v0.2.0 / v0.2.1 | 07-26 | Release | M-H 人类边界 / explorer 安全加固 |
| v0.3.0 | 09-02 | Release | 领域 OS + 记忆重做 + Nostr 收官 |
| v0.4.0 | 09-29（tag bb1945d6） | Release | 进程外领域 OS + 内核回调面 + AgentEar 附着(A3) + SDK 原型 |
| v0.5.0 | 09-30（f46194a9） | Release | ME-4 收口；Sin90 v0.5.0 / Cos72 v0.1.0 同发 |
| **v0.5.1** | 10-01（4fb5a892） | Release，**GitHub "Latest"** | DEP-A8 多平台：CLI mac/linux + 桌面 Linux |
| documenting-r2-rc1 | 10-13 | Pre-release | Documenting 验收包 r2（#900，mac arm64）；tag 4e6eb551（10-10） |
| documenting-r3-rc1 | 10-13 | Pre-release | Documenting r3 |
| v0.5.1-preview.20261014.1 | 10-14 | Pre-release | 体验预览；tag = `eb7f546f`（#915 Documenting r3 合并） |
| v0.5.1-preview.20261016.2 | 10-19 | Pre-release | ARM64 核心预览（签名+公证）；tag `0b8325b0`（#1025 HW-1），**早于 K1 squash (#1022) 之后的 #1024 等**，不含此后任何工作 |
| `agent24-os-sdk-v0.1.0` | | tag | SDK 原型 |
| `stable/main-2026-10-02` | | 稳定点 tag | `9ed35496`（COMM-4b） |
| 其余 tag | | `v0.0.0-m0`、`v0.1.0-m1`..`v0.4.0-m4`、`pre-migration-2026-05-15`、`local-backup-a24-967-r3-785b5dc6`（本地备份，非发布） | |

**v0.5.1 之后 main 已前进 1300+ 提交（first-parent 90 个）而没有再发正式版**：K1、M1、M2、Documenting slice1 r1-r5、HW-1、iDoris Admin 卡、COMM/Hyphae v0.26.3 锁、#794 修复都在 "Latest" 之后；CHANGELOG 同样落后。

### 2. main 最近 60 次 first-parent 合并，按线分类（10-24 -> 09-24）

- **COMM / Hyphae**：#1193（锁 v0.26.3）、#1137 / #1136 / #1135（M1 联调：群聊历史、joint round、npub 分类）
- **#794 授权 / 写权限（法律 + 修复）**：#1117（laws workspace-write grant）、#1118、#1119、#1120；#1031（L-APPR-5 追溯评审）；#905（HN0 health/rollback 冻结）
- **Documenting 发布**：#875 r1、#915 r3、#979 r4、#1068 r5；#740（ab/documenting）、#715 / #685（设计、D1 基线）
- **内核/修复**：#1105（死亡钩子）、#1108（consent 草稿须核验属性）、#1107（v1 兼容 free_disk_bytes）；K1 本身是 squash `56264ef9` #1022（不出现在 merge 列表）
- **iDoris**：#789 / #772（admin status card）
- **Memory（M1/M2）**：#767（ab/m2 = M2 M-E，10-08）、#720（release/m1-sync，10-07）、#721、#666（mainline-m1-resume）
- **Decide（D0）**：#722、#718、#717、#689、#688、#687、#686、#684、#682
- **Open Design**：#670、#668、#667、#662、#660（M10 候选）、#650、#643、#569、#572
- **流程 / CI**：#728、#724、#723（feature-branch 工作流）、#716、#647、#641（clippy）
- **架构 / 路线文档**：#665、#664、#345 / #344（DSH laws）
- **Sidecar / Capability（09-24..27）**：#254 / #255 / #259 / #260 / #261、#228

10-01 以来 first-parent 非 merge（squash）落点另有：K1 #1022、SD-01 #1023、HW-1 #1025 / #1026 / #966、ADR-036 #964、英中泰 i18n #950、Overview #951、MAINT-03 #1015，以及 COMM-1b..4b 一串。

### 3. 各集成分支一览

"合并数" = `origin/main..ab/X` 的 first-parent merge 数（总 / 2026-10-16 之后）。

| 线 | 分支 | rev-list 领先 / 落后 | 合并数 | tip | 状态 |
|---|---|---|---|---|---|
| Kernel K1 | `ab/kernel` | 267 / 528 | 62 / **40** | 4f45100c 10-25 | K1 10-16 以 #1022 squash 发布；其后是 A5 级（grant / enable / revoke 通知、egress 矩阵、6b.4 / 6b.5）40 个合并未发 main。open #1244、#1243、#1239、#1183 |
| Documenting | `ab/documenting`（+ `-180-revoke-followups`） | 87 / 49 | 25 / 25 | a3ef4d5b 10-25 | r1-r5 已发；r5（#1068）后 25 个合并（DOC-1-169..179：撤销、停止、flake）未发。open #1240 |
| Memory P1 | `ab/memory-p1`（+ `-47/-48/-49/-50`） | 63 / 3 | 25 / 25 | a9ba2ed3 10-26 | 活跃：R05 来源契约、H03；open #1248（CHANGES_REQUESTED）、#1247 |
| WeKnora / Knowledge | `ab/weknora`（+ `-c1-05a2`、`-ct-state-notes`、03a/03b/03c 及 -1/-2 变体） | 201 / 46 | 49 / 49 | 13a2f944 10-25 | 活跃，从未发 main；新 crate `agent24-knowledge`。open #1242、#1241 |
| iDoris | `ab/idoris`（+ `-13-cla-allow-baton`） | 36 / 239 | 8 / 5 | fa517176 10-20 | 停在 10-20（v0.3.1 范围，R9 三项决策 OWNER REVIEW PENDING） |
| Decide | `ab/decide` | 25 / 503 | 7 / 0 | 1ea40c62 10-19 | D1 L1 / L2 合同合入后无后续，最后合并 10-12 |
| COMM | `ab/comm` | 11 / 528 | 4 / 1 | ca86c0b5 10-21 | COMM-05b 零运行守卫 10-21；其余 COMM 直接进 main |
| Office Assistant | `ab/office-assistant`（+ 4 task 分支） | 8 / 46 | 2 / 2 | ce090c54 10-24 | 泰语 UI / 评测样本；open #1208、#1189、#1188、#1175 |
| Maint | `ab/maint` | 2 / 480 | 1 / 0 | 8aaf474a 10-14 | 停滞 |
| rustdoc 两条 | `ab/apple-preview-cloud-…` | 3 / 6 | | | 已被 #1027 取代 |

各线最近合并（节选）：
- kernel：#1220 6b5c 网络出口测试、#1235 6b5 矩阵跟进、#1237 notifier 不被 panic 带死、#1236 dispatch 绑定 run、#1231 grant-end 重投、#1227 notifier、#1222 outbox、#1202 A5c 设计、#1216 6b5b、#1209 6b5a
- documenting：#1234 revoke-by-ref、#1213 revoke-receiver、#1218 detail 焦点回退、#1217 / #1214 flake、#1206 stop 机制、#1212 timings、#1201 job acceptances、#1194 layer-stop、#1192 text-preview
- memory-p1：#1246 timings 相对 ts、#1205 G3 角色、#1197 H03 拒绝发送、#1195 D7f 写入、#1185 D7f kinds、#1181 快照来源、#1173 D7 kinds、#1170 R05 精确契约、#1168 来源表 0021、#1167 pause HTTP
- weknora：#1238 3c2b、#1232 c1-05a1、#1233 3c2、#1230 3c2a1、#1229 3c1、#1228 3b2、#1226 3b、#1225 3a、#1224 host-d、#1223 c1-04-2
- idoris：#1029 pair 验收、#1040 CI fixture、#1100 comm daemon 监督、#1028 receipts、#983 gateway、#984 probe-env、#981 codec、#980 plan 指针
- decide：#909 D1 L2 合同、#908 D1 L1、#760 onnx、#754 D1 计划、#734 模型家族、#727 Jev 研究、#726 三语评测
- comm：#857、#776、#768、#741；office-assistant：#1163 + OA 计划并入；maint：#949

### 4. open PR（13 个，无以 main 为 base 者，无 release PR）

| # | base <- head | 评审 | 主题 |
|---|---|---|---|
| 1248 | ab/memory-p1 <- memory-p1-49-r05-5-send-trim | **CHANGES_REQUESTED** | R05-5 发送副本按来源 kind 整形、HostSystem 来源 |
| 1247 | ab/memory-p1 <- memory-p1-50-lifecycle-s9 | 无 | 生命周期 ADR §9 定稿（adr-s9 待接受） |
| 1244 | ab/kernel <- kernel-60-6b5d-module-process-tests | 无 | K1-6b.5d 出口测试 |
| 1243 | ab/kernel <- kernel-59a-6b5c2a-peer-run-views | 无 | peer 视图只放行自成一体的 run |
| 1242 | ab/weknora <- weknora-c1-05a2 | 无 | KB-C1-05 knowledge.search/read 调用层 |
| 1241 | ab/weknora <- weknora-ct-state-notes | 无 | CONTRACT-STATE 窗口上限 |
| 1240 | ab/documenting <- documenting-180-revoke-followups | 无 | DOC-1-39b 跟进 |
| 1239 | ab/kernel <- kernel-66-a5-lows | 无 | A3-A5 Low 收尾 |
| 1208 / 1189 / 1188 / 1175 | ab/office-assistant <- 09-thai-brief / 04-samples / 03-sample-checker / 02-thai-ui-copy | 无 | OA 泰语简报 / 样本 / 检查脚本 / UI 文案 |
| 1183 | ab/kernel <- kernel-47-l-appr-7 | 无 | L-APPR-7 laws 就地修订（改 `docs/laws/`，按规则须 jason 确认） |

### 5. 各线当前进度一览

| 线 | 已在 main | 在集成分支未发 | 阻塞 / 待决 |
|---|---|---|---|
| 发布 | v0.5.1 Latest；4 个预发布 | — | main 比 Latest 前进 1300+ 提交，无正式新版；CHANGELOG 未跟 |
| M1/M2 记忆 | #720、#767（10-07 / 10-08）；H01 #1024；#1166 / #1167 | `ab/memory-p1` 25 合并：R05 来源（迁移 0021 / 0022）、H03 发送拒绝 / 恢复核对 / G3 | R05 ADR 仍 DRAFT；adr-s9 未接受，挡 P1-02c 与 purge；G1/G2/G4/G5/K1 同事务/冷恢复推迟；#1248 CHANGES_REQUESTED |
| K1 内核 | #1022（10-16）+ #1030 / #1034 | `ab/kernel` 40 合并 | 4 个 open PR；#1183 改法律文本需 jason |
| Documenting | r1-r5 | `ab/documenting` 25 合并 | David 负责；Line K（#713）KB 合同 OPEN |
| WeKnora | — | `ab/weknora` 49 合并 + `agent24-knowledge` | 跨仓约定 #904 OPEN；从未发 main |
| COMM / Hyphae | v0.26.3 锁；群聊历史 #1135-1137 | `ab/comm` 4 合并 | Hyphae 侧文档期望 Agent24 实现群聊界面，main 未见 UI |
| Decide | D0 全合 | `ab/decide` 7 合并（D1） | 10-12 后停；交接称 K1 发布后开工，未见启动 |
| iDoris 接入 | admin 卡 #772 / #789 | `ab/idoris` 8 合并（receipts / gateway / codec / pair） | R9 v1.2 三项决策 OWNER REVIEW PENDING；provider 代码只在 ab/idoris |
| HW-TIERS | agent24-hw、ADR-036、#1025-#1026、#1107 | — | 基本完成 |
| Office Assistant | — | `ab/office-assistant` 2 合并 | 泰语 / 评测样本 |
| Open Design | M10（#660）；SD-01（#1023） | — | OD-M11/12 / TPI-W / OC 暂停 |
| 桌面 mac CI | 签名预览包 | `feat/desktop-macos-dmg` | mac CI 是否还要（UNSURE，GROUP-A） |

### 6. 未合并分支调查（`Agent24-archive/20261026/tier2-review/`）

- 目录现有：`GROUP-A.md`（桌面 / 发布 / 预览 / 本地验证类，118 行）、`prs.tsv`（1144 行：899 MERGED / 230 CLOSED / 15 OPEN）、`tier2-uniq.txt`。**GROUP-B.md 调查时尚不存在**。
- GROUP-A 结论：26 条里绝大多数 SUPERSEDED（#1023、#676、#626-#630、#543 / #544 / #550、#673、#1026 / #1027 / #1030 / #1107 / #1108 等）；**VALUABLE 1 条**：`origin/repro/1020-orphan-module`（`docs/agent/REPRO-1020-20261016.md`，被 #1105 PR 正文引用，但 main 上悬空）；**UNSURE 2 条**：`feat/desktop-macos-dmg`（mac CI 要不要）、`feat/me4-cos72-skeleton`（T10 Cos72 进程内骨架）；OBSOLETE 1 条：`review/convergence-prs-20261016`。
- 上层目录另有 `deleted-local-branches.txt`（24）、`deleted-remote-branches.txt`（5）、4 组 `ab-memory-p1-*` diff/tgz 备份。
- 小订正：GROUP-A 写"PR 全是 squash 合并"，实际 release 多为 merge commit，只有 K1 等少数是 squash；其结论以文件内容对比为准，不受影响。

---

## 三、内外链接

### 3.1 出站（Agent24 -> 外部）

扫描范围：origin/main 的 `docs/**`、`README.md`、`AGENTS.md`、`CLAUDE.md`。

**A. 生态仓库**

| 目标 | 存在？ | 所在文件 | 用途 | 问题 |
|---|---|---|---|---|
| `iDoris-ai/Agent24`（86 处） | 是 | 几乎所有 docs | 自引 | 无 |
| `iDoris-ai/agent24`（小写） | 是（重定向） | ARCHITECTURE-LAYERS、SOAK-F5、SIN90-PET0-INTEGRATION、RELEASE-CHECKLIST-v0.5.1、ADR-K1-01 / 02 | 自引 | 大小写不一 |
| `AuraAIHQ/Agent24` | 重定向到 iDoris-ai/Agent24 | `docs/PLAN.md`(2)、`docs/VERIFICATION.md` | 旧组织地址 | 过期组织名 |
| `AuraAIHQ/simple-agent` / `iDoris-SDK` / `iDoris` / `agent-speaker` | 重定向到 `iDoris-ai/simple-agent` / `iDoris-SDK` / `iDoris` / **`Hyphae`** | `docs/PLAN.md` | 旧规划 | 全部旧名（PLAN.md 已被 tasks.md 标"作废"） |
| `iDoris-ai/Hyphae`（18） | 是 | `Deployment/HYPHAE-NETWORK-DELIVERY.md`(4)、`HYPHAE-HEALTH-ROLLBACK-CONTRACT.md`、`SOAK-F5.md`、`followups.md`、`comm/LOCK-UPGRADE-20261024.md`、`design/COMM-HYPHAE.md`、`design/HYPHAE-CLI-INTEGRATION.md`(5) | 通信 sidecar 发布 / 锁定 / 契约 | 有效。但 `HYPHAE-HEALTH-ROLLBACK-CONTRACT.md` 含本机路径 `/Users/jason/Dev/iDoris/Hyphae`（**不存在**；实际 `/Users/jason/Dev/auraai/agent-speaker`）；`followups.md` FU-34 仍用旧名 agent-speaker（史实） |
| `iDoris-ai/Sin90` | 是（v0.5.1） | README、tasks.md、v050 审计 README | 进程外 OS 参考实现 | 有效 |
| `MushroomDAO/Cos72` | 是（v0.1.1） | tasks.md、审计 README | 第二个进程外 OS 样例 | 有效 |
| `iDoris-ai/open-design-agent24` | 是 | `open-design-workspace/STATUS.md` | OD fork | 有效 |
| `nexu-io/open-design` | 是 | `open-design.pin.json`、chatgpt 对话 | 上游 OD | 有效 |
| `Tencent/WeKnora` | 是 | `research/WEKNORA-INTEGRATION-PROPOSAL.md` | 上游知识库 | 有效；fork `iDoris-ai/WeKnora` 存在，main 的 docs 未链接 |
| `MushroomDAO/Xiaoheishu` | 是 | README（`vendor/xiaoheishu` submodule）、PLAN.md | 参考外壳 | 有效 |
| `jhfnetboy/researcher`（私有）、`jhfnetboy/PR-daemon`（私有）、`jhfnetboy/ProcessKit-rs`、`jhfnetboy/listmonk` | 存在，前两个 private | documenting ADR-DOC-03、samples README、WEKNORA 提案；tasks.md；WINDOWS-PORT、PROCESSKIT-PK4-PIN | 研究资料 / 工具 | 私有链接对外部读者 404 |
| `krillinai/OpenCreator` | 是 | `PLAN-OD-NEXT.md` | OC 线（暂停） | 有效 |
| `iDoris-ai/AgentEar` | 是（v0.28.0） | 26 个文件提到名字，**无 github 链接**，靠 `design/INTEGRATION-AGENTEAR-IDORIS.md`（ADR-032） | A3 附着约定 | 见 3.4 |
| `iDoris-ai/iDoris`、`iDoris-Components`（私有） | 存在 | main 无直链，仅文字 | | |

**B. 研究参考（约 30 个，非生态）**：mem0ai/mem0 (51)、getzep/graphiti (40)、topoteretes/cognee (28)、MemTensor/MemOS (27)、letta-ai/letta (26) 与 letta-code、openai/openai-agents-python (24)、langchain-ai/langmem (17) / langgraph、LongMemEval、HippoRAG、hindsight、A-mem、MemoryOS、Voyager、dgm、EvoAgentX、SkillRL、semantic-router 等，分布在 `docs/research/**`、`docs/reference-notes/**`；另 `anthropics/claude-code`(5)、`QwenLM/Qwen3Guard`、`facebookresearch/XNLI`、`notofonts/noto-cjk`。未逐个核存在性，合并时原样迁移即可。

**C. 本机绝对路径（换机即失效）**
- `CLAUDE.md` L7-9：`@/Users/jason/Dev/Brood/protocol/MISSION.md`、`orgs/auraai/PROFILE.md`、`orgs/auraai/INTERFACES.md`。目标存在，但该 checkout 停在分支 `sync-progress-2026-08-17`（有未提交改动）；其 `INTERFACES.md` blob 与 AAStarCommunity/Brood main 相同（f8af3dd7）。**每个 Agent24 会话都会读入这些内容。**
- `HANDOFF-ORCA-2026-10-08.md`：`/Users/jason/Dev/tools/PR-Daemon/scripts/pre-pr-check.sh`、`Agent24-wt-kernel-23`（临时 worktree）。
- `comm/JOINT-ROUND2.md`：`/Users/jason/Dev/auraai/Agent24-round2`（该目录已不在 auraai/）；`comm/LOCK-UPGRADE-20261005.md`：`.../agent-speaker`（存在）。
- `demo/*/run-demo.sh`、`stop-demo.sh`：`/Users/jason/Dev/auraai/sin90-design`（存在）。
- `docs/decision.md` L23：`/Users/jason/Dev/mycelium/blog/submodules/xiaoheishu/desktop`。
- `AGENTS.md`：无仓库外路径。

**D. `../../` 相对路径**：全部指向仓库内（`../../rust/...`、`../../protocol/openapi.yaml`、`../../apps/desktop/...`），见 `Deployment/HYPHAE-*`、`adr/ADR-K1-*`、`agent/PLAN-M1-SNAPSHOT-PROVENANCE.md`。**没有跨仓库的 `../../`**（`VERIFICATION.md:665` 的 `../../../etc/passwd` 是测试用攻击串）。这些相对链接及 `#L74` 行号锚点依赖文件原位，移动文档会断。

### 3.2 入站（外部 -> Agent24）

扫描：`/Users/jason/Dev/auraai/*` 下所有带 .git 的目录 + Dev 下 maxdepth 3 的 384 个 .git。命中 = 含 agent24（不区分大小写）的 .md 数：

| 仓库（本地路径） | .md 数 | 备注 |
|---|---|---|
| `auraai/iDoris`（iDoris-ai/iDoris） | 171 | 分支 tools/slack-listen（远端 gone），10-18；v0.3.0 Latest |
| `auraai/internal-AI`（= iDoris-ai/iDoris-Components，私有） | 84 | main，10-06 |
| `auraai/agent-speaker`（= **Hyphae**） | 56 | 分支 groupchat-hg4-…，10-26；v0.26.3 Latest |
| `Dev/Brood`、`Dev/aastar/Brood`（AAStarCommunity/Brood，两份 clone） | 27 / 26 | 前者在 sync-progress-2026-08-17 且有未提交改动，后者 main 07-29 |
| `tools/AgentEar` | 23 | v0.28.0，10-26 |
| `iDoris-bridge`（`Dev/` 与 `Dev/auraai/` 两份） | 19 / 17 | 10-21，新仓库 |
| `auraai/sin90-design`（= iDoris-ai/Sin90）；`Dev/iDoris-ai/Sin90` 另一份 | 12 / 5 | sin90-design 落后 origin 3 个提交；`mycelium/Sin90`（MushroomDAO/Sin90）为 04-29 旧 clone |
| `mycelium/Cos72` | 8 | main 10-01 |
| `auraai/auraai-packages` | 7 | 05-29 |
| `auraai/WeKnora`（iDoris-ai/WeKnora fork） | 5 | a24/develop，10-11 |
| `mycelium/launch`、`launch-wip`、`blog` | 7 / 7 / 7 | 春季旧文 |
| `tools/Pet0` | 1 | 分支 docs/agent24-integration，08-09 |
| `auraai/Self-FDE-WorkBench` 2、`auraai/dalanggou` 1（本地模型策略文）、`auraai/open-design-agent24` 1（+wt-brand 1） | | 提及性质 |
| `tools/PR-Daemon` 与 5 个 PR-daemon-* 克隆 | 700-1100 | 工具仓库，评审语料 / 规则示例；Agent24 文档反向引用其 `pre-pr-check.sh` |
| `backup/*`、`jhfnetboy/{pintusorn,goutou,DevLoop}`、`tools/{MediaBot,see-balance,ab-codex}` | 1-8 | 提及而已 |
| auraai/ 下零命中 | | simple-agent、ProcessKit-rs(-pk5)、OpenCrab、NextStop、infoCrab、iDoris-website、iDoris-SDK、CrabKB、courses、AuraAI、ai-atlas、AgentSocial、agent-speaker-relay |

（`auraai/Agent24-r05-5`、`Agent24-lifecycle-s9`、`Agent24-adr-037` 是 Agent24 自己的 worktree，不算入站。）

**精确深链到 Agent24 文件的入站引用**

| 外部仓库 | 引用的 Agent24 文件 | 在 main 上？ |
|---|---|---|
| internal-AI | `decision.md`(36)、`agent/PLAN-OD-NEXT.md`(7)、`specs/SPEC-ME3-OUT-OF-PROCESS.md`(5)、`agent/tasks.md`(5)、`specs/SPEC-ORG-SPACE.md`(4)、`design/OPEN-DESIGN-M10-MAIN-LANDING-GUIDE.md`(4)、`design/COMM-HYPHAE.md`(4)、`ARCHITECTURE-LAYERS.md`(4)、`specs/SPEC-002-protocol.md`、`open-design-workspace/design/ADR-001..006`、`design/ME4-S3-os-sdk.md`、`design/A3-ATTACHED-MODULE.md`、`agent/roadmap.md`、`agent/PLAN-ME4-OS-CAPABILITIES.md`、`agent/COMPONENT-ROADMAP.md`、`specs/WIRE-OOP-MODULE.md`、`specs/TASKS.md`、`iDoris-integration-and-entry-router.md`(7)、`design/INTEGRATION-AGENTEAR-IDORIS.md`、`specs/SPEC-MEMORY.md`、`agent/research.md`、`ADR-016` | 全部存在 |
| iDoris | `decision.md`(4，固定 sha)、`iDoris-integration-and-entry-router.md`、`design/INTEGRATION-AGENTEAR-IDORIS.md`、`design/HW-TIERS-capability-hardware-matrix.md`（固定 sha 961a6408）、`specs/TASKS.md`、`ADR-026`、`PLAN.md`、`ADR-016`、`agent/tasks.md`、**`design/MODEL-CAPABILITY-DESIGN.md`** | `MODEL-CAPABILITY-DESIGN.md` **不存在于任何 ref**（iDoris docs/17 声称两仓各一份；Agent24 侧从未入库；该文已标废弃） |
| Hyphae | `design/COMM-HYPHAE.md`、`design/HYPHAE-CLI-INTEGRATION.md`、`Deployment/TASKS.md`、`Deployment/HYPHAE-NETWORK-DELIVERY.md`、`agent/COMPONENT-ROADMAP.md`、`comm/LOCK-UPGRADE-20261005.md`；固定 sha 的源码：`agent24-models/src/{lib,router}.rs`、`agent24-comm/{hyphae.lock.json,src/router.rs,tests/joint_round1.rs}`、`agent24d/src/domain.rs`、`agent24-cli/src/main.rs`、`agent24-os-packages/src/discovery.rs`、`packages/nostr-bridge/src/{protocol,inbound}.ts` | 文档存在；固定 sha 链接不受影响 |
| Sin90 | `agent/PLAN-OOP-OS-AND-BACKLOG.md`(3)、`design/ME4-S2-model-callback.md`(2)、`design/ME4-S1-scheduler-callback.md`、`specs/SPEC-ME3-OUT-OF-PROCESS.md`、`SIN90-PET0-INTEGRATION.md`、`agent/tasks.md`、`PLAN-ME4-OS-CAPABILITIES.md`；commit `164120b` / `9ab5b6e`；`rust/apps/agent24d/tests/me3f_blackbox.rs` | 存在 |
| Cos72 | `agent/tasks.md`(2)、`PLAN-ME4-OS-CAPABILITIES.md`；运行时 `AGENT24_CHECKOUT=$HOME/Dev/auraai/Agent24` 同级 checkout | 存在 |
| AgentEar | `design/INTEGRATION-AGENTEAR-IDORIS.md`(5)、`design/A3-ATTACHED-MODULE.md`(4)；`contracts/` 对应 Agent24 CI | 存在 |
| Pet0 | `SIN90-PET0-INTEGRATION.md`(2)、`specs/SIN90-domain.md` | 存在 |
| WeKnora fork | `docs/knowledge/ADR-KB-01-SINGLE-CONVERSATION.md`；跨仓 issue #904、#713（均 OPEN） | ADR 文件**只在 `ab/weknora` / `ab/documenting`，不在 main** |
| iDoris-bridge | README 中 3 处 `Agent24/blob/main/README.md` | 存在 |
| Brood | 无对 Agent24 文件的深链，只引用仓库地址和本地路径 | |

### 3.3 外部对 Agent24 的期望 / 约定归类

**A. 接口约定**
1. **iDoris × Agent24 边界与接口规范**（`iDoris/docs/interfaces/iDoris-Agent24-边界与接口规范.md`，修订 1.2，10-15，称"唯一跨仓规范权威"，iDoris 主持）。期望：Agent24 保留"短消费方指针 + 反向 HW-TIERS/ADR-036 链接，不复制第二份契约"；`IDORIS_URL` 必须显式配置；两个逻辑 provider `idoris-local` / `idoris-any`；两把 key；Admin v0 五个只读 GET，Electron main 固定客户端，65-byte 帧 FD 传递；`X-iDoris-Fallback` 等头。协商记录 `negotiation/R1-Agent24-答复.md`（Agent24 09-27，基线 main@f7fe994）、`R9-v1.2-协商提案.md`（OWNER REVIEW PENDING）。Agent24 现状：`apps/desktop/src/main/idoris-admin.ts` 在 main；`agent24-models/src/idoris.rs`、`docs/iDoris-consumer.md`、`PLAN-IDORIS-V031.md` **只在 `ab/idoris`**；**main 上没有指向该规范的指针**。
2. **Hyphae**：`docs/cooperation/Agent24.md`、`Agent24-contract-fixtures.md`、`agent/hyphae-agent24-distribution-install-update.md`（10-10 决定：进程外 sidecar）、`agent/group-chat-agent24-interface-20261025.md`（Hyphae 提供 `hyphae groupchat … --json` 契约，**期望 Agent24 实现群聊界面**）。Agent24 `hyphae.lock.json` 锁 `144555ca`（v0.26.3），与 Hyphae 文档基线一致；#1135-1137 已合群聊历史 / 联调片，界面未见。
3. **AgentEar × Agent24**：`agent24-embedding.md`（自称"实施约定草案"）、ADR-0008、`contracts/README.md`；A3 附着协议；`os attach add/list --json`。AgentEar 认为"待 Agent24 合并 A3 后对 main 复验；jason 真机 C10"。A3 已在 v0.4.0 合入。
4. **Sin90 ↔ Pet0 集成约定**：权威源 = `Agent24/docs/SIN90-PET0-INTEGRATION.md` v0.3（提交 09-30）；Pet0 的 `docs/AGENT24_INTEGRATION.md` 是镜像，草案 v0.1，08-09。
5. **WeKnora fork**：`docs/agent24/*`（ADR-001 唯一主对话、TODO-B、PROPOSAL、FORK-MAINTENANCE、PATCH-LEDGER，10-11）+ 跨仓 issue #904 / #713。期望 Agent24 为唯一对话入口，WeKnora 只做知识服务。

**B. 集成计划 / 依赖**
- **Brood** `orgs/auraai/INTERFACES.md`（04-27）：Agent24 = 基于 Claude Code 的自主任务执行框架 / MemPalace / 评估系统；agent-speaker v0.25.0；"待完善：iDoris 推理接口 / AI 激励结算 / 跨 agent 协作协议"。`PROFILE.md`、`protocol/REPO_REGISTRY.md`、`ECOSYSTEM_MAP.md`、`PGL/*`（Agent24 = AgentStore 主承载平台，`interface_version: "agent24-v1"` skill 接口）、`orgs/mycelium/INTERFACES.md`（Sin90 可选消费 Agent24）、`docs/AGENT24_DSH_PARALLEL.md`。
- **iDoris-bridge**：`ADR-0001`、`08-ROADMAP-ACCEPTANCE.md`（"Agent24 Native Adapter 调用真实 Session / Run"），依赖 Agent24 Run / Session / Approval / OS SDK 与 Agent24 -> iDoris provider 的真实状态。
- **Cos72 / Sin90**：依赖 `agent24-os-sdk`、`agent24 os install`、`agent24d` 黑盒测试；同级 checkout 约定。

**C. 镜像 / 同步副本**
- **iDoris-Components（internal-AI）** `docs/components/agent24/{README,ARCHITECTURE-LAYERS,COMPONENT-ROADMAP,DESIGN-DOCS}.md` + `research/components/agent24/**`、`research/components/agentear/**`。自声"冲突以 Agent24 为准"；基线 main@122fff7（10-03），落后 main 704 提交。
- **Pet0** `AGENT24_INTEGRATION.md`。
- iDoris `docs/agent/*` 是**iDoris 自己**的 pilot 文档集（命名同构），不是 Agent24 的镜像；其 `ecosystem-boundaries.md`、`handoff-agent24.md`、`09-对Agent24的需求.md`、`17-…协作草案.md`、`roadmap.md`、`00-导读` 均已标"已废弃（2026-09-27），以总体规划为准"。

### 3.4 外部说法与 Agent24 现状矛盾 / 过期

| # | 外部文件 | 外部说法 | Agent24 现状 |
|---|---|---|---|
| 1 | Brood `orgs/auraai/INTERFACES.md`、`PROFILE.md` | Claude Code 自主任务框架；"MemPalace 模块"；agent-speaker v0.25.0 | Rust 内核 + `agent24d` + 进程外领域 OS；记忆是 `agent24-memory` crate，main 无 MemPalace 代码（仅历史文档提及）；agent-speaker 已改名 Hyphae（锁 v0.26.3）。**该文件被 Agent24 `CLAUDE.md` 用 `@` 导入，每个会话都读到** |
| 2 | Brood `protocol/REPO_REGISTRY.md` | Agent24 = Skills 系统（/evolve /evaluate），依赖 agent-speaker；列出 `iDoris-ai/Agent24-Desktop` | `Agent24-Desktop` 在 GitHub **404**；桌面在 `apps/desktop` |
| 3 | Brood `ECOSYSTEM_MAP.md` L96 / L119 | 含 `git clone …Agent24-Desktop.git` | 克隆会失败 |
| 4 | Brood `docs/REPO_STATUS.md` | Agent24 v0.2.1，08-12 | 最新 v0.5.1 |
| 5 | Brood `backlog/tasks/task-34`、`doc-7` | "72%""96 commits Sin90 内核集成" | 08-17 快照；Sin90 已迁出内核（T11） |
| 6 | Brood `research/Hermes/README.md` | Agent24 "35-45%"，建议评估 pivot 为 Hermes plugin | 研究意见，未采纳（ADR-026 Rust core） |
| 7 | Sin90 `README.md`、`docs/STATUS.md`（GitHub main 亦同） | "仍没有 Rust SDK"（STATUS 核实于 09-20） | `agent24-os-sdk` 0.1.0 已于 09-26 / 27 合并；Sin90 v0.5.x 已发 |
| 8 | Pet0 `AGENT24_INTEGRATION.md` | v0.1 草案，08-09，"Sin90 是 Agent24 内置基础领域模型" | 权威 v0.3：Sin90 为进程外包；镜像落后 |
| 9 | internal-AI `docs/components/agent24/*` | 基线 main@122fff7 | 落后 704 提交；缺 K1、Memory-P1、Documenting、WeKnora、HW-TIERS 落地 |
| 10 | iDoris 边界规范 1.2 | Agent24 审计基线 A0 = `6072fd37`；"Agent24 保留短消费方指针" | main 无指针；provider 在 ab/idoris 未发；stable（v0.5.1）早于 A0。规范自己如实标注实现差距 |
| 11 | iDoris docs/17 | "Agent24 `docs/design/MODEL-CAPABILITY-DESIGN.md`" | 文件从未入 Agent24 任何 ref；iDoris 侧已废弃 |
| 12 | Hyphae `cooperation/Agent24.md` 早期条目 | 10-01 时点（"默认 provider 仍是 OMLX/Ollama"等） | 时点记录，已变 |
| 13 | AgentEar `agent24-embedding.md` | "实施约定草案，双方确认后才冻结" | A3 已于 v0.4.0 合入 main；是否正式冻结无记录 |
| 14 | WeKnora fork 文档 | "不恢复 Agent24 暂停路线"；工程接口待评审（10-11） | `ab/weknora` 此后 49 个合并 + `agent24-knowledge`，路线已启动 |
| 15 | iDoris-bridge ADR-0001 | 依赖"Agent24 -> iDoris Provider 真实生产状态" | provider 只在 `ab/idoris` |
| 16 | blog 旧文（04-27、05-04） | 自进化 Skills 系统 | 旧定位，公开博客 |
| 17 | 全部 `AuraAIHQ/*` 链接 | 旧组织名 | 都重定向，但属旧名 |

---

## 四、建议（只列出，不改）：三份新文档落地后，入站链接需外部更新

**必须改（会断链或造成错误指引）**
1. **Brood**：`orgs/auraai/INTERFACES.md`、`PROFILE.md` 重写 Agent24 条目（Rust 内核 / 进程外 OS / 记忆 / Hyphae / 版本），并指向新"进度"文档；`protocol/REPO_REGISTRY.md`、`ECOSYSTEM_MAP.md` 删除 `Agent24-Desktop`；`docs/REPO_STATUS.md` 刷新。同时 Agent24 `CLAUDE.md` 的 `@/Users/jason/Dev/Brood/...` 导入依赖本机 clone，需决定去留。
2. **Pet0**：`docs/AGENT24_INTEGRATION.md` 镜像按权威 v0.3 重生成，或只留指针（若新文档移动 `SIN90-PET0-INTEGRATION.md` 须同步）。
3. **Sin90**：README、`docs/STATUS.md` 的"没有 Rust SDK"；`docs/agent/tasks.md` 中对 Agent24 `agent/tasks.md`、`PLAN-ME4-OS-CAPABILITIES.md` 的引用（若合并改名）。
4. **internal-AI（iDoris-Components）**：`docs/components/agent24/*` 重新同步；其对 `decision.md`(36)、`PLAN-OD-NEXT.md`、`agent/tasks.md`、`roadmap.md`、`COMPONENT-ROADMAP.md`、`ARCHITECTURE-LAYERS.md` 等的链接，凡新文档变动路径的都需更新。
5. **iDoris**：规范 1.2 的"Agent24 消费方指针"需要落点（`docs/iDoris-consumer.md` 在 ab/idoris）；悬空的 `MODEL-CAPABILITY-DESIGN.md` 引用；`agent/tasks.md`、`specs/TASKS.md`、`PLAN.md` 链接如有移动需更新。固定 sha 链接不受影响。
6. **WeKnora fork**：`docs/agent24/*` 与 `ab/weknora` 的 `ADR-KB-01` 路径对应；#904 / #713 链接保持；"暂停路线"措辞更新。

**视新文档放哪再定**
7. **Hyphae**：`cooperation/Agent24.md` 等对 `design/COMM-HYPHAE.md`、`HYPHAE-CLI-INTEGRATION.md`、`Deployment/TASKS.md`、`HYPHAE-NETWORK-DELIVERY.md`、`comm/LOCK-UPGRADE-20261005.md`、`COMPONENT-ROADMAP.md` 的 `blob/main` 链接（固定 sha 的不会断）。群聊界面期望可登记进"进度"。
8. **AgentEar**：对 `INTEGRATION-AGENTEAR-IDORIS.md`、`A3-ATTACHED-MODULE.md` 的引用；只合并进度类则无影响。
9. **Cos72**：`agent/tasks.md`、`PLAN-ME4-OS-CAPABILITIES.md`。
10. **iDoris-bridge**：README 里 3 处指向 Agent24 README 的链接。

**不用改**：blog 旧文、launch、auraai-packages、Self-FDE-WorkBench、dalanggou、固定 sha 链接、PR-Daemon 系列（只需保证 `pre-pr-check.sh` 路径不变）。

**给三份文档取舍的提示**
- "进度"应显式写出 Latest v0.5.1 与 main 的差距，并补 CHANGELOG 的 v0.5.1 之后条目。
- `HANDOFF-ORCA` 硬规则、Pi 交接的 8 条硬门属协作规则，宜迁入 AGENTS.md，不放进度。
- 手写状态是过期根源；本报告使用的 `rev-list` / `gh` 命令可作现成探针。"领先 main N 个提交"应改用"自 release 以来的合并数"口径（K1 squash 使旧口径失真）。
- 悬空/待处置项：`repro/1020-orphan-module` 报告（VALUABLE）、`feat/desktop-macos-dmg`、`feat/me4-cos72-skeleton`；GROUP-B.md 尚未出现。

## 附：探针命令（均只读）
`git tag --sort=-creatordate`、`gh release list --repo iDoris-ai/Agent24`、`git log --merges --first-parent --format='%h %cs %s' -60 origin/main`、`git rev-list --count origin/main..origin/ab/X`、`git rev-list --merges --first-parent --count [--since=2026-10-16] origin/main..origin/ab/X`、`gh pr list --state open --json …`、`gh api repos/<o>/<r> --jq .full_name`、`git grep … origin/main`、`grep -r -c -i agent24 --include=*.md`（各外部仓库）。
