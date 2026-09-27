# Agent24 实时状态 — progress

> 「此刻仓库真实发生了什么」。由 `pilot run` 每一步更新。
> 更新时间：2026-09-23
>
> **各刀在不在 main 上，不写在这里** —— 状态由探针给出：`bash docs/agent/me3-status.sh`（ME-4 的 `4a/4b/4c` 行随交付追加）。
> 本文件只记「现在在做哪件事、为什么、卡在哪」。

## 当前聚焦

- **主线**：**ME-4 外置 OS 的内核能力面 → v0.5.0**（用户 2026-09-23 裁决，定义见 [`PLAN-ME4-OS-CAPABILITIES.md`](PLAN-ME4-OS-CAPABILITIES.md)）。
  跨三个仓库：`iDoris-ai/Agent24`（内核回调 + SDK + 发布）、`iDoris-ai/Sin90`（M3/M4/M5 + 迁 SDK）、`MushroomDAO/Cos72`（mytask 最小样例）。
- **为什么先补回调不先做 SDK**：SDK 只封装握手/帧/回调通道，不带新能力；外置模块缺的是调度与推理这两个内核回调。
- **本地路径**：Agent24 `~/Dev/auraai/Agent24`；Sin90 `~/Dev/auraai/sin90-design`；Cos72 `~/Dev/mycelium/Cos72`（remote `MushroomDAO/Cos72`）。

## 下一个 READY（按顺序挑）

1. ME4-0.1 合并 Sin90 #2/#3/#4（已 APPROVED）。
2. ME4-1.1.1 调度回调设计冻结（Codex 评审到 approve）；可并行 ME4-4.1.1 推理回调设计。
3. ME4-0.3 / ME4-0.4 Sin90 CI 与 Codex 补审。

## 2026-09-23 夜 → 09-24 凌晨：无人值守一夜的战报

**模式**：统筹（Opus）+ Sonnet 子代理开发（≤3 并发）+ 全新上下文 Opus 子代理对抗评审（Codex 额度 09-23 耗尽、09-29 19:28 恢复，期间全部记 `ME4-CODEX-DEBT`）。用户指示：当晚只开 PR、不盯 PR 状态、不找 PR-Daemon 复审——所以**当晚没有任何合并**，有依赖的任务全部以 stacked PR 叠放，合并顺序写在各 PR body。

**Agent24 已开 PR**（按合并顺序；每个 PR body 写了自己的前后顺序）：
- #439 ME-4 规划（本文件所在分支）
- 调度线（#444 设计冻结 v3.1）：#447 保留路径 · #453→#454→#455→#456 存储层 · #462 协议视图 · #465 桌面端 · #469→#470→#471→#472 触发接口与 tick · #478→#479 REST 护栏 · #487→#488 回调 handler · 1.3.1 投递泵（进行中）· 1.5.1 黑盒（待做）
- 推理线（#446 设计冻结 v3.1）：#448 回环/代理修复（关闭 FU-72）→ #451 rpc 按方法超时 → #457 model_access → #461 回调基础 → #464 handler（未注册）
- 独立修复（基于 main）：#473 FU-74 CLI/worker 不走代理 · #476 FU-78 探针原子写 · #482 FU-79 supervisor 测试有界等待
- **跨线等待**：推理线的 4.2.2b2（授予 + 接线）需要调度线的 `CallbackDeps`（#488）与推理线的 handler（#464），两条 stacked 线在合并进 main 之前没有共同基点——两线合并后从 main 开工；4.2.3 / 4.3.1 随后。

**评审抓到的真问题（摘要）**：cron crate 星期字段 1=周日、日/星期取 AND（FU-71）；回环判定解析器与 reqwest 不一致 + 默认 client 读 HTTP_PROXY（FU-72，已修 #448）；CLI/worker 访问本机也走代理（FU-74）；调度设计里「只写 X-A24-Request-Id 不构成在途请求」「scheduler 先于 mount_all 启动会永久禁用模块 schedule」「..; 绕过保留路径」等（均在设计中修正）。

**明早要做的**：
1. 按各 PR body 的合并顺序合并（每合一个前先把子 PR 的 base 改成 main）。评审服务 clestons 今晚被占用，PR 等它审。
2. Codex 额度 09-29 恢复后按 followups.md 的 `ME4-CODEX-DEBT-*` 清单补审。
3. 用户手动：给 `iDoris-ai/Sin90`、`MushroomDAO/Cos72` main 开 ruleset。

## 2026-09-24 夜（第二段）：战报

**合并**（approve 覆盖当前 head 才合）：Agent24 #439（规划）、#473（FU-74）、#476（FU-78）、#482（FU-79）；Sin90 #5（规划）、#4、#6（CI）、#7（文档）、#29（SFU-9/10）。Sin90 main CI 绿。
**新开 PR**：Agent24 #500 → #501 → #502（ME4-1.3.1 fired 投递，Opus 2 轮）；Sin90 #35 → #36 → #37（T5.2.1 classify，Opus 3 轮）。
**rebase 待重审**：Sin90 #8、#9、#14（与更新后的 main 冲突，已 rebase 并留言）。
**进行中**：ME4-1.5.1 调度黑盒（10/10 绿，评审中）；Sin90 T5.4.1 propose（叠在 #37 上，Opus 第 3 轮评审中）。
**误操作 / 教训**：
- 为合 #439，把子 PR #444/#446 的 base 改成 main，**两者已有的 approve 被作废**（不可逆），需重审；已记入 memory。
- 按过时台账重做了一遍 T0.2（开了重复的 Sin90 #38，已关闭）；开工前应先查 PR 列表。
**待登记 Sin90 followups**：SFU-12（J7 检查器误拒 `pub(crate)`）、SFU-13（CreateTasks validate 缺标题 / direction 检查）、SFU-14（precheck 拿不到写锁时 fail-open）。

## 2026-09-26：ME4 S1/S2 收口，4.3.1 待评审

**合并**（按 GitHub 真实状态核对，台账已回填）：ME4-S1 调度回调线（1.1.1→1.2.1→1.2.2a/b/c/d→1.3.1→1.3.2→1.4.1→1.5.1）与 ME4-S2 推理回调线（4.1.1→4.2.1→4.2.2-0→4.2.2a→4.2.2b1a/b1b/b2→4.2.3a→4.2.3b）**全部合并**，共 30 个 PR，绝大多数于 2026-09-25 合入、最后两个（#506/#510/#511）于 2026-09-26 合入。ME4-1.3.1（#500→#501→#502）与 ME4-4.3.1 一样只经 Opus 本地评审，Codex 未审，计入 `ME4-CODEX-DEBT`。
**进行中**：ME4-4.3.1（推理黑盒验收 J14/J19 + 探针 4b）PR #512 已于 2026-09-26 合并，Opus 本地评审 2 轮（CHANGES → APPROVE，Medium 全修），Codex 未审，计入 `ME4-CODEX-DEBT-7`。ME4-M2/M3/M4b 门同日按 Sin90 `origin/main` 回填 DONE。
**下一步**：ME4-5.1.1 SDK 设计冻结（从调度/推理两个调用方提取公共 transport/握手），依赖 ME4-M4b 门（Sin90 M5 全 DONE，尚未开始）与本轮 4.3.1 收口。

## 阻塞项（BLOCKED）

- 无。

## 需要用户做的事

- `iDoris-ai/Sin90` main 目前**没有保护**（`rules/branches/main` 为空）。请开一条 ruleset：要求 1 个审批 + push 时 dismiss stale。
  在此之前 Sin90 的 PR 评审只是约定。

## 已知风险

- `PR-Daemon` 是否覆盖 `MushroomDAO` 组织未核实；Cos72 第一个 PR 开出后 30 分钟无裁决即如实上报，不自合。
- 本会话 `codex` MCP 连接失败；Codex 挑战走 `codex:codex-rescue` 插件子代理，不可用时按 CLAUDE.md Tier 2 本地评审并在 PR body 记债。

## 最近完成

- 2026-09-23 #356 T11 状态同步、#357 `SIN90-PET0-INTEGRATION.md` 重写 —— 合并。
- 2026-09-22 #342 T11 内核侧删除内置 Sin90、#343 待办快照。
- 2026-09-20 #262 T9 仓外包黑盒验收 —— ME-3 收口。

## 纪律（沿用）

1. 每条新回归测试都要变异验证（`docs/agent/mutate.sh`）。
2. 不许出现比机制更强的措辞（模块与 daemon 同 UID，§0 威胁模型）。
3. 判据本身要先被验过 —— 每条判据带正对照。
