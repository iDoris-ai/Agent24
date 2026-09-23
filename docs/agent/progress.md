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

**Agent24 已开 PR**（按合并顺序）：
- #439 ME-4 规划（本文件所在分支）
- #444 ME4-1.1.1 调度回调设计冻结 v3.1（3 轮 Opus 评审，第 3 轮 APPROVE；保留路径经 659,373 条路径穷举验证）
  - #447 ME4-1.3.2 保留路径实现（Opus APPROVE + 跟进 commit）
  - #453 → #454 → #455 → #456 ME4-1.2.1 存储层四刀（Opus APPROVE + M-1..M-3 修复）
- #446 ME4-4.1.1 推理回调设计冻结 v3.1（3 轮 Opus 评审，第 3 轮 APPROVE）
  - #448 ME4-4.2.2a 回环判定/代理/重定向安全修复（关闭 FU-72）+ 模型契约扩展
  - #451 ME4-4.2.2-0 rpc 按方法超时 + ErrorKind unavailable
- 进行中（未开 PR）：ME4-1.2.2a 协议与视图、ME4-4.2.1 manifest 字段

**评审抓到的真问题（摘要）**：cron crate 星期字段 1=周日、日/星期取 AND（FU-71）；回环判定解析器与 reqwest 不一致 + 默认 client 读 HTTP_PROXY（FU-72，已修 #448）；CLI/worker 访问本机也走代理（FU-74）；调度设计里「只写 X-A24-Request-Id 不构成在途请求」「scheduler 先于 mount_all 启动会永久禁用模块 schedule」「..; 绕过保留路径」等（均在设计中修正）。

**明早要做的**：
1. 按各 PR body 的合并顺序合并（每合一个前先把子 PR 的 base 改成 main）。评审服务 clestons 今晚被占用，PR 等它审。
2. Codex 额度 09-29 恢复后按 followups.md 的 `ME4-CODEX-DEBT-*` 清单补审。
3. 用户手动：给 `iDoris-ai/Sin90`、`MushroomDAO/Cos72` main 开 ruleset。

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
