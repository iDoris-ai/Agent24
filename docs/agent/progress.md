# Agent24 实时状态 — progress

> 「此刻仓库真实发生了什么」。由 `pilot run` 每一步更新。
> 更新时间：2026-09-11
>
> **ME-3 各刀在不在 main 上，不写在这里** —— 那张手写表过期过（见 #165），状态由探针给出：
> `bash docs/agent/me3-status.sh`。本文件只记「现在在做哪件事、为什么、卡在哪」。

## 当前聚焦

- **主线**：ME-3 进程外领域 OS → **v0.5.0**（用户 2026-09-10 裁决：ME-3 全做完再发版，3d 不推迟）。
  路线、任务分解（T1–T14）与其余待办见 [`PLAN-OOP-OS-AND-BACKLOG.md`](PLAN-OOP-OS-AND-BACKLOG.md)。
- **M1 / F1.1（`Authorizer` 判定接缝）**：`tasks.md` 里的状态未改动，**未重排、也未开工**。
  2026-08-23 之后的工作全部落在 ME-3 上；本文件上一版停在那一天（#140/#141 待合），与仓库脱节了十九天。

## 本轮待办（用户 2026-09-11 定，按顺序）

| # | 做什么 | PR | 完成条件 |
|---|---|---|---|
| 1 | ✅ ME-3b-4 受约束代理合入（PLAN T4） | [#173](https://github.com/iDoris-ai/Agent24/pull/173) | APPROVE 后与 #165 冲突 → 合 main 解冲突（只动 SPEC 状态表，取探针）→ 复扫重新 APPROVE → 已合并 `b1b4b82` |
| 2 | ✅ 变异脚手架收掉复审的三条阻塞（B1 红基线 / B2 测试数 / B3 中途被杀） | [#172](https://github.com/iDoris-ai/Agent24/pull/172) | 多轮复审后对 `dceba02` APPROVE → 已合并 `32ab103`（不阻塞项记 FU-51、FU-52） |
| 3 | ✅ 本文件与 `tasks.md` 对齐仓库真实状态，并记下这四条 | [#174](https://github.com/iDoris-ai/Agent24/pull/174) | 已合并 `09669bc` |
| 4 | ✅ ME-3b-5 两阶段热 disable（PLAN T5） | [#175](https://github.com/iDoris-ai/Agent24/pull/175) | 已合并 `4918201`（评审 S1–S3 记 FU-48–50） |

PR 的实时状态以 `gh pr view <n>` 为准，本表只记「为什么在等」。

## 阻塞项（BLOCKED）

- 无。

## 最近完成（2026-08-23 之后，全部 squash 合入 main）

- 2026-09-12 #172 变异脚手架（`docs/agent/mutate.sh` / `mutate.py`）。
- 2026-09-11 #175 3b-5 两阶段热 disable · #173 3b-4 受约束代理 · #174 状态文档对齐。
- 2026-09-11 #165 进程外领域 OS 的路线、v0.5.0 任务分解、`me3-status.sh` 探针。
- 2026-09-10 ME-3b 各刀：#164 3b-1 framing · #166 3b-2b `initialize` 线格式（含 FU-42）· #167 manifest `spawn` 字段 · #169 复审收尾 · #170 闭 FU-41（包根即执行边界）· #171 3b-3 起步（解析 spawn、铸一次性 token、独立进程组起进程）。
- 2026-09-09 ME-3a 发现与安装：#156 门 6 两步解析 · #157 磁盘发现 · #158 原子安装 · #159 门 6 欠账 · #160 抽出 `agent24-os-packages` · #161 `os install/uninstall` 接线；#162 3b-2a 版本协商；#163 3b 五刀切法。
- 2026-09-02 v0.3.0：#142 桥活性 · #143 调研 · #144 跟进批 · #145 发布 · #146 changelog 更正。
- 2026-08-23 #140 F8 记忆所有权 (org, space) · #141 ADR-030 + SPEC-ORG-SPACE。

## 仓库卫生（2026-09-11）

- 清掉 28 个 PR-Daemon 在本机评审时留下的临时 worktree、7 个 PR 已合并的同级 worktree（约 21 GB，多为 `rust/target` 与 `node_modules`）、54 个本地分支（PR 头快照与已合并分支）。
  每一项删前都核过：PR 已 MERGED、工作区干净；分支 tip 是该 PR 最终 head 或其祖先 —— 例外是 9 个「被后续评审轮次改过的旧草稿」（`me3b-cuts*`、`pr98-*`、`pr100-r`），它们不是最终版的祖先，也一并删了。
  删之前全部打包进 开发机（MacBook，不是 Mac mini）上的 `~/Dev/auraai/.archive/agent24-cleanup-2026-09-11.bundle`（`git bundle verify` 通过），只在那一台上可恢复。
- 2026-09-12 #172、#175 合并后，各自的 worktree 与分支（`Agent24-mutation` / `chore/mutation-harness`、`Agent24-me3b5` / `feat/me3b-5-drain`）按同样的核验删除，删前各打一个 bundle 放在同一目录。
- 保留：`feat/me4-cos72-skeleton`（Cos72 骨架，PLAN T10 要重做成进程外样例，远端也在）。

## 下一个 READY

- 本轮四条待办已全部完成。**ME3-T6 ME-3c 回调通道其余部分** 已开 [#176](https://github.com/iDoris-ai/Agent24/pull/176)，等外部评审；之后是 **ME3-SUP**（Supervisor 接线，落实 FU-46/49/50）。

## 本轮的三条纪律（从 F1/F8 二十余轮复审里带出来的，ME-3 仍适用）

1. **每条新回归测试都要变异验证** —— 把修复改回去，看它变红。过不了这关的测试，等于没写。（脚手架：`docs/agent/mutate.sh`，#172）
2. **不许出现比机制更强的措辞** —— 库层可用但没有生产调用方是 🟢，不是 ✅。
3. **判据本身要先被验过** —— 每条判据带正对照；「全部未开工」与「探针全坏了」不能是同一个读数。
