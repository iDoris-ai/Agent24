# Agent24 任务台账 — Task

## 🔴 本文件是当前唯一权威的执行状态来源（2026-09-19 立，2026-09-23 更新）

仓库里有四份路线图/进展文档，**以谁为准只有一个答案**：

| 文档 | 地位 |
|---|---|
| **本文件 `docs/agent/tasks.md`** | ✅ **权威**。当前在做什么、做到哪一步，以这里为准 |
| [`PLAN-ME4-OS-CAPABILITIES.md`](PLAN-ME4-OS-CAPABILITIES.md) | ✅ **权威**（配套，2026-09-23 起）。**ME-4 当前主线**的任务定义、技术规范（S1–S5）与验收标准 |
| [`PLAN-OOP-OS-AND-BACKLOG.md`](PLAN-OOP-OS-AND-BACKLOG.md) | ✅ **权威**（配套）。ME-3 各刀（已收口）的任务定义与验收标准（§五「主链」T1–T14） |
| [`me3-status.sh`](me3-status.sh) | ✅ **权威**（可执行）。`bash docs/agent/me3-status.sh` 直接读 `origin/main` 的代码回答「哪一刀已经在 main 上」 |
| [`roadmap.md`](roadmap.md)（M1–M6 产品路线） | ⏸️ **暂停中**。是「未来要做什么」，不是「现在在做什么」；M1 等 v0.5.0 发布后再捡 |
| [`../PLAN.md`](../PLAN.md) §六 Roadmap、[`../ROADMAP.md`](../ROADMAP.md) | ⛔ **已作废**。Rust 核心重写（ADR-026）之前的 Electron/Node.js 时代规划，仅供历史考古，**不要照它排期** |

**当前执行（2026-09-23 起）**：**ME-4 —— 外置 OS 的内核能力面**（调度回调 → Sin90 M3/M4 → 推理回调 + Sin90 M5 → SDK/Cos72/wire 文档 → v0.5.0）。
定义见 [`PLAN-ME4-OS-CAPABILITIES.md`](PLAN-ME4-OS-CAPABILITIES.md)，状态见下方「ME-4 台账」。ME-3 已于 2026-09-20 收口、T11 已于 2026-09-22 交付（下面两段是历史记录）。

## ME-4 台账（2026-09-23 立；本表是 ME-4 唯一的状态来源）

> 定义/验收在 PLAN-ME4 §三；Sin90 侧 task 的定义与状态在 `iDoris-ai/Sin90` 的 `docs/agent/tasks.md`，这里只记**门**。
> 状态：BACKLOG · READY · IN_PROGRESS · BLOCKED · PR_OPEN · CHANGES_REQUESTED · APPROVED · DONE。推进时回填 PR/commit。

| ID | 仓库 | 任务 | 依赖 | 状态 | 证据 |
|---|---|---|---|---|---|
| ME4-0.1 | Sin90 | 合并已批准的 #2/#3/#4（Sin90 T0.1） | — | `IN_PROGRESS` | #2 `0949d37`、#3 `bd84e9a` 已合并；#4 rebase 待重审 |
| ME4-0.2 | 两仓 | 合并本规划 PR（Agent24 `docs/me4-plan-2026-09-23` / Sin90 `docs/pilot-me4-plan`） | — | `PR_OPEN`（开 PR 后回填） | |
| ME4-0.3 | Sin90 | CI + 陈旧文档（Sin90 T0.2/T0.3） | 0.1 | `BACKLOG` | |
| ME4-0.4 | Sin90 | Codex 补审历史改动（Sin90 T0.4） | 0.1 | `BACKLOG` | |
| ME4-1.1.1 | Agent24 | 调度回调设计冻结 + SPEC 改写 | 0.2 | `BACKLOG` | |
| ME4-1.2.1 | Agent24 | schedules 存储层（owner/key/revision/暂停三态 + 并发安全 upsert + `schedule_deliveries`） | 1.1.1 | `BACKLOG` | |
| ME4-1.2.2 | Agent24 | `ScheduleInvocation`/`FireOutcome` + REST 护栏 | 1.2.1 | `BACKLOG` | |
| ME4-1.3.1 | Agent24 | fired 投递器（InFlight 准入 + dispatch、确定性 fire_id、重启续投、Deferred 不计失败、scheduler 挪到 mount_all 后） | 1.2.2 | `BACKLOG` | |
| ME4-1.3.2 | Agent24 | 代理保留 `/_a24/` 路径（规范化后判定） | 1.1.1 | `BACKLOG` | |
| ME4-1.4.1 | Agent24 | `_a24/scheduler/*` handler + 授权/配额/限流 | 1.2.1 | `BACKLOG` | |
| ME4-1.5.1 | Agent24 | 调度黑盒验收（真实 tick）+ 探针 `4a` | 1.3.1, 1.3.2, 1.4.1 | `BACKLOG` | |
| ME4-M2 门 | Sin90 | Sin90 M3（T3.1.1–T3.5.1）全 DONE | 1.5.1, 0.1 | `BACKLOG` | |
| ME4-M3 门 | Sin90 | Sin90 M4（T4.1.1–T4.4.1）全 DONE | M2 门 | `BACKLOG` | |
| ME4-4.1.1 | Agent24 | 推理回调设计冻结 + SPEC + manifest 字段 | 0.2 | `BACKLOG`（可与 M2/M3 并行） | |
| ME4-4.2.1 | Agent24 | `model_access` manifest 字段 + Models 授权 | 4.1.1, 1.5.1 | `BACKLOG` | |
| ME4-4.2.2a | Agent24 | `agent24-models` 契约扩展（max_tokens / model_id） | 4.1.1 | `BACKLOG` | |
| ME4-4.2.2b | Agent24 | `_a24/model/complete` handler | 4.2.1, 4.2.2a, 1.5.1 | `BACKLOG` | |
| ME4-4.2.3 | Agent24 | 按模块持久化用量 | 4.2.2b | `BACKLOG` | |
| ME4-4.3.1 | Agent24 | 推理黑盒验收 + 探针 `4b` | 4.2.2b, 4.2.3 | `BACKLOG` | |
| ME4-M4b 门 | Sin90 | Sin90 M5（T5.0.1–T5.5.1）全 DONE | 4.3.1, M3 门 | `BACKLOG` | |
| ME4-5.1.1 | Agent24 | SDK 设计冻结（从两个调用方提取） | M4b 门 | `BACKLOG` | |
| ME4-5.1.2a | Agent24 | SDK transport + 握手（只经 proto）+ 结构测试 | 5.1.1 | `BACKLOG` | |
| ME4-5.1.2b | Agent24 | SDK 五种类型化客户端 | 5.1.2a | `BACKLOG` | |
| ME4-5.1.2c | Agent24 | fired 注册点 + example + tag + 探针 `4c` | 5.1.2b | `BACKLOG` | |
| ME4-5.2.1 | Sin90 | Sin90 迁到 SDK（Sin90 TS.1.1） | 5.1.2c | `BACKLOG` | |
| ME4-5.3.1 | Cos72 | Cos72 仓库 pilot 七件套 | 5.1.2c | `BACKLOG` | |
| ME4-5.3.2 | Cos72 | 骨架（manifest + SDK 挂载 + 迁移 + 事件） | 5.3.1 | `BACKLOG` | |
| ME4-5.3.3a | Cos72 | mytask 实体与路由 | 5.3.2 | `BACKLOG` | |
| ME4-5.3.3b | Cos72 | 审批发积分 + 账本回放 + 摘要进记忆 | 5.3.3a | `BACKLOG` | |
| ME4-5.3.4 | Cos72 | Cos72 真实挂载黑盒（含与 Sin90 共存隔离） | 5.3.3b, 5.2.1 | `BACKLOG` | |
| ME4-5.4.1 | Agent24 | wire 文档 + Node.js 参考模块（T14） | 5.1.2c | `BACKLOG` | |
| ME4-6.0.1 | Agent24 | 冻结 v0.5.0 专用发布清单 | 5.3.4, 5.4.1 | `BACKLOG` | |
| ME4-6.0.2 | Sin90+Cos72 | 模块发布物（tar.gz + SHA256SUMS + Release） | 6.0.1 | `BACKLOG` | |
| ME4-6.1.1 | Agent24 | 发布前收口（ADR 修订/CHANGELOG/版本/回填台账） | 6.0.2 | `BACKLOG` | |
| ME4-6.1.2 | Agent24 | 发布 v0.5.0 | 6.1.1 | `BACKLOG` | |
| ME4-6.1.3 | Mac mini | 干净机器只用发布物安装验收 | 6.1.2 | `BACKLOG` | |
| ME4-6.1.4 | 三仓 | 最终台账收口 PR（本轮最后一个 PR） | 6.1.3 | `BACKLOG` | |

**需要用户手动做**（不是 goal task）：给 `iDoris-ai/Sin90` 与 `MushroomDAO/Cos72` 的 main 开 ruleset（1 个审批 + dismiss stale）。

> 台账回填规则（PLAN-ME4 §一 第 7 条）：task PR 合并后，`DONE`/证据由下一个 PR 顺带回填，或攒到 ME4-6.1.4 的最终台账收口 PR。

**T8.5c-W-wire 实现已完成**（`DONE` — [#224](https://github.com/iDoris-ai/Agent24/pull/224)+[#225](https://github.com/iDoris-ai/Agent24/pull/225)，2026-09-19；语义说明 [`T8.5c-W-wire.md`](../design/T8.5c-W-wire.md) v5，5 轮设计评审冻结；代码 2 轮 Codex 代码评审——首轮 5 Medium(判据覆盖面问题，未发现生产代码缺陷)，修复后二轮 approve，另发现 1 Low(超时预算 50ms→300ms)已修复）：`Generation::admit_callback_bound`（#224，修复真实 TOCTOU 竞态，单锁内原子完成"准入+取绑定生命周期"）+ `memory_callback.rs` 的 `RememberHandler`/`RecallHandler`/`RecentHandler` 三个 Handler、`map_memory_error` 结构性 default-deny 堵住 `QuotaExceeded` 的 owner/partition key 泄露、`_a24/memory/scoped/*` 不注册产出 `-32601`（#225）。`cargo test --workspace`：1300 passed。**已知覆盖缺口**（Codex 确认风险可接受，留作后续 follow-up）：真实子进程握手验证 `Offer.provides` 包含 memory 能力这条端到端测试未覆盖，不影响生产代码正确性。

至此 **T8.5c-W（mount + wire）整体交付完毕**。

## 🎉 T9 已交付，ME-3 专项整体收口（2026-09-20）

**T9（ME-3f 仓外包端到端验收）`DONE`** — [#262](https://github.com/iDoris-ai/Agent24/pull/262)（2026-09-19；8 轮 Codex 对抗式代码评审，第 8 轮 APPROVE 无新发现）：新增 `rust/apps/agent24d/tests/me3f_blackbox.rs`，核心测试 `a_package_from_outside_the_repo`——daemon 先以空 packages 目录起一次 → 停止 → 在跟仓库物理无关的临时目录下生成并安装一个 `impl_kind: out_of_process_provider` 的 Python 模块包 → 用同一个已编译好的二进制重启（全程无 `cargo build`）→ 真实断言五件事全部成立：挂载（`/api/v1/os` 报 `mounted`）、路由代理（真实 HTTP 经内核代理命中模块）、事件转发（真实 WS 消费者边界观测投递）、记忆读写（真实 `remember`+`recall`，精确 id/body 关联）、审批往返（代理真实注入的 request-id/approval-token，先错误 token 验证拒绝不消耗真实 token，再真实 token 验证成功）。负对照 `an_in_process_declaration_for_an_uncompiled_crate_is_still_refused` 证明挂载校验没有被意外放宽。

评审过程中第 6 轮独立通读抓到一个真实资源泄漏：`stop()` 用 SIGKILL 终止 daemon，绕过了 daemon 自己负责 reap 模块子进程的正常关闭路径，导致 Python 模块进程永久孤儿化（`ps` 实测修复前累积 34 个孤儿）；改成 SIGTERM+有界等待+SIGKILL 兜底、塞进 `Running::drop()` 覆盖所有退出路径（含 panic）后归零，第 8 轮进一步把发信号换成 `rustix::process::kill_process`（daemon 自己 supervisor 同款 API），消除 shell 出去的 `kill` 命令带来的 PATH 依赖。`cargo test --workspace` 全绿，`me3f_blackbox` 单独重跑 20+ 次稳定通过。

`bash docs/agent/me3-status.sh` 核对：**3a-3g 全部 14 行"已在 main"，一个不剩**——ME-3（进程外领域 OS）专项到此整体交付完毕。

## 🎉 T11 已交付（2026-09-22）

**T11（Sin90 迁出内核）`DONE`** — [#342](https://github.com/iDoris-ai/Agent24/pull/342)（`bb9b9d5`，2026-09-22；`clestons` APPROVED，CI 全绿后合并）：删掉编译进内核的 `agent24-sin90{,-os,-store}` 三个 crate（`agent24d`/`agent24-cli`/`agent24-protocol` 相应接线一并清理，净减 ~5000 行）。合并前已独立编译该分支二进制、跑通 Sin90 侧真实端到端挂载黑盒测试（`AGENT24_CHECKOUT` 指向该分支 → `agent24_mount_blackbox.rs --ignored`：挂载/代理/路由行为不变/事件转发 4 条判据全过）。配套的 [#343](https://github.com/iDoris-ai/Agent24/pull/343)（2026-09-20 待办快照文档）同日合并。

## 🔴 2026-09-22 待办清单 —— ⛔ 已被上方「ME-4 台账」取代（2026-09-23）

> 逐条去向：P0 Codex 补审 → ME4-0.4；P1 `SIN90-PET0-INTEGRATION.md` 重写 → 已由 #357 合并；T10/T12/T13/T14 → ME4-5.x / ME4-6.x。下面保留原文作历史。

- **P0，Codex 额度已恢复（原定 2026-09-22 19:18，现已过点）**：`iDoris-ai/Sin90` 的 M0/M1/M2/挂载修复（commit `0d66f24`/`4032e82`/`8056ade`/`ab66b37`）目前只经过本地自审，没有真正的对抗式评审——尤其挂载修复里的 actor-key 门禁安全问题（`ab66b37`），应该优先送审。
- **P1**：`docs/SIN90-PET0-INTEGRATION.md` 整篇假设"内核内置 Sin90"，T11 #342 已合并，这个假设已不成立，需要独立重写。
- **P2，明确暂停中**：T10（Cos72）——`feat/me4-cos72-skeleton` 分支保留，除非用户明确说继续，不要主动捡起来。
- **P2**：T12（发布 v0.5.0）——依赖 T10（暂停）+ T11（已 `DONE`）。
- **P3，可并行，未开始**：T13（`agent24-os-sdk`）+ T14（wire 文档）。

完整会话记忆见协调 Claude 的 `project_todo_2026-09-20` 记忆条目（本机 `~/.claude/projects/-Users-jason-Dev-auraai-Agent24/memory/`）。

**收口后路径（用户 2026-09-19 拍板，T11 已完成）**：

```
T11（Sin90 迁出内核，DONE）→ T10（Cos72 进程外样例，暂停）→ T12（发布 v0.5.0）
```
（T13 `agent24-os-sdk` / T14 wire 文档可并行。）

**之后**：v0.5.0 发布后回头捡 [`roadmap.md`](roadmap.md) 的 **M1（记忆成为产品）**；`../PLAN.md` / `../ROADMAP.md` 完全作废。

> **探针的历史偏差记录**（`me3-status.sh` 这一行的"预言过期"发生过 4 次，如实记账）：
> `3e`/`3g` 曾因重构前的旧文件路径/旧符号名被误报"未开工"（3e = [#199](https://github.com/iDoris-ai/Agent24/pull/199)/[#201](https://github.com/iDoris-ai/Agent24/pull/201)/[#203](https://github.com/iDoris-ai/Agent24/pull/203)，2026-09-17；3g = [#196](https://github.com/iDoris-ai/Agent24/pull/196)，2026-09-16），[#222](https://github.com/iDoris-ai/Agent24/pull/222) 修正。
> `3d` 的占位符号在 #222 里猜测会落在 `os_memory.rs`，T8.5c-W-wire 实际把它放进了新文件 `memory_callback.rs`，[#234](https://github.com/iDoris-ai/Agent24/pull/234) 修正。
> `3f` 的坐标（`me3f_blackbox.rs` + `a_package_from_outside_the_repo`）是唯一一次"设计探针时预留的坐标，实现落地后直接对上、不用改"——4 次偏差里唯一的例外，记一笔正对照。

---

> 前置：[`roadmap.md`](roadmap.md)（M→F）·[`architecture.md`](architecture.md) ·[`spec.md`](spec.md)
> 每个 Task 自包含，可独立开发与验收。**验收标准可机器验证**。
> 状态：BACKLOG · READY · IN_PROGRESS · BLOCKED · PR_OPEN · CHANGES_REQUESTED · APPROVED · DONE
>
> **全局验收前置**（每个 task 都适用，不再逐条重复）：
> ```
> cd rust && cargo fmt --all --check \
>   && cargo +1.98.0 clippy --workspace --all-targets -- -D warnings \
>   && cargo +1.98.0 test --workspace
> ```
> **新回归测试一律做变异验证**：把修复改回去，确认测试变红；在 PR body 里写明变异方式与结果。

---

## ME-3 / v0.5.0 —— 当前主线（用户 2026-09-10 裁决）

> 完整任务分解（PLAN 的 T1–T14）在 [`PLAN-OOP-OS-AND-BACKLOG.md`](PLAN-OOP-OS-AND-BACKLOG.md) §五，这里**不抄第二份**。
> 这里只放**正在做的**那几条，且 ID 带 `ME3-` 前缀，免得和下面 M1 的 `T1.x.y` 撞名。
> **哪一刀已在 main 上，跑 `bash docs/agent/me3-status.sh`，不看本表。**

### ME3-T4 3b-4 受约束代理合入  `DONE` — [#173](https://github.com/iDoris-ai/Agent24/pull/173)（`b1b4b82`，2026-09-11）
- 已 APPROVE（head `00f5e27`）；#165 合入后与之冲突（只在 SPEC 状态表）→ 合 main 解冲突、取探针（`0163803`）→ 复扫对新 head 重新 APPROVE → 合并。
- **验收**：探针 `3b-4 受约束代理` 一行变 `●`。

### ME3-MUT 变异脚手架收掉三条阻塞  `DONE` — [#172](https://github.com/iDoris-ai/Agent24/pull/172)（`32ab103`，2026-09-12）
- 外部评审对 head `dceba02` APPROVE；批准里的六条不阻塞项记为 FU-52，自证偶发失败记为 FU-51。
- 两轮 PR 前复审发现 bash 版的问题同根(用 shell 手搓进程监督器),核心改写为 `mutate.py`;自证 45 组、元测试 26 种回退全红。
- B1 红基线不拦后续 `mut` · B2 0 个测试的基线 / 测试数变化读成「判据不承重」· B3 中途被杀留下被变异的源码；同轮收掉非阻塞项（崩溃被读成编译失败、超时只杀直接子进程、`set -u` 泄漏、路径打错建出空文件、不认识的输出落到 🔴、自证不断言）。
- **验收**：`bash docs/agent/mutate-selftest.sh` exit 0；把每条修复改回去自证都 exit 1（PR body 列回退清单）。

### ME3-DOC 状态文档对齐  `DONE` — [#174](https://github.com/iDoris-ai/Agent24/pull/174)(`09669bc`)
- `progress.md` 停在 2026-08-23，与仓库脱节十九天；本条把它和本文件对齐，并记下本轮四条待办。

### ME3-T5 3b-5 两阶段热 disable（SPEC §4）  `DONE` — [#175](https://github.com/iDoris-ai/Agent24/pull/175)（`4918201`，2026-09-11）
- **优先级**：high（ME-3b 的最后一刀）
- **依赖**：3b-3（库层已在 main）· ME3-T4 ✅（要接进 `proxy.rs`；不叠在 #173 上开 stacked PR —— 合并自动删分支会把叠在上面的 PR 一起关掉）
- **目标**：停一个模块时，「宽限期内收不收新请求、在途 handler 还能不能回调、generation 什么时候撤」三件事由一个状态机定死，而且**撤 generation 早于杀进程**由类型保证，不靠调用顺序。
- **开发范围**：
  1. `agent24-os-proto` 里一个纯状态机：`Starting → Running → Draining → Revoked`（没有单独的 `Stopped`，理由见 SPEC「3b-5 落地时定死的几条」）；在途请求登记（带 `request_id`）；drain 在「在途清零」与「宽限到期」先到者结束。
  2. **回调准入**是它的一个纯判定：Running 全收；Draining 只收**携带并命中活跃 `request_id`** 的；Revoking 之后一律拒。回调通道本身是 ME-3c，本刀只交判定，3c 调它。
  3. **杀进程组的入口要求一个只能由「撤销 generation」产出的值**，于是「先杀后撤」写不出来（编译不过），而不是「测试里没这么写」。
  4. 接进代理：Draining 期间新的被代理请求 503（`error.code` 区分 draining / not-ready / overloaded，§2.1 已预留）；在途请求继续；drain 超时的在途请求 503、结果记为「未知」写日志，不假装成功。
- **明确不做**：回调通道（3c）；把状态机接到 daemon 的 `os disable` 路径 —— 今天 daemon 对 `out_of_process` 包在挂载时就拒绝，**没有一个运行中的进程外模块可以被热停**。
- **CLI help 的到期标记**：`agent24-cli/src/main.rs` 那两行「applies at the next daemon start」**本刀之后仍然为真**（理由同上），所以本刀**不改它们**，只把到期标记的触发条件从「3b-5 落地」改成「daemon 的 `os disable` 真的作用于运行中的模块」。PLAN T5 原文写的「同时改 help」是按「3b-5 = 接线完成」写的，与 3b-3 只交付库层的实情不符 —— 在 PR 里讲清，不静默改掉一句会变假的 help。
- **验收**（取自 SPEC §8 ME-3b 格与 PLAN T5，每条带正对照）：
  - Draining 期间新的被代理请求 503 且 `code = module_draining`；**对照**：同一时刻已在途的请求正常完成（200）。
  - Draining 期间：携带活跃 `request_id` 的回调被放行；不带 `request_id` 的被拒；携带**已结束**请求 id 的被拒。**对照**：Running 期间三者都放行。
  - drain 超时：仍在途的请求得到 503（不是 200、不是挂住），日志里有「结果未知」。
  - generation 撤销先于 `terminate_group`：由类型保证 + 一条 `compile_fail` doctest 证明「未撤销就杀」写不出来；**对照**：撤销后能杀的那条 doctest 编译通过。
  - Revoking 之后一切回调拒绝，包括持有活跃 `request_id` 的。
- **涉及文件**：`rust/crates/agent24-os-proto/src/{drain.rs,proxy.rs,lib.rs}`（探针预言的符号是 `drain.rs` 的 `pub enum DrainState`，照它命名）、`rust/apps/agent24-cli/src/main.rs`（仅注释）、`docs/specs/SPEC-ME3-OUT-OF-PROCESS.md`（记本刀的边界）
- **证据**：

### ME3-T6 3c 回调通道其余部分（SPEC §3 + §8 ME-3c 格）  `DONE` — [#176](https://github.com/iDoris-ai/Agent24/pull/176)（`c75e919`，2026-09-12）
- **优先级**：high（T1–T6 齐了即可发 v0.4.0 握手层）
- **依赖**：3b-1 framing、3b-2b `initialize`（均已在 main）。**不依赖** #175（3b-5）：本刀 offer set 为空，没有任何业务方法，「draining 期间回调准入」要等第一个业务方法（3d/3e）才有落点
- **目标**：握手之后那条回调连接上的一切协议行为定死，实现者不需要猜并发、取消与错误分类
- **开发范围**（`agent24-os-proto/src/rpc.rs`，探针预言符号 `pub fn dispatch`）：
  1. `dispatch`：把握手之后的**一帧**分类成「立即回错」「派发给 handler」「取消某个在途请求」「忽略（未知 notification）」—— 纯函数，不碰连接
  2. 一个连接循环：并发在途、响应可乱序、按 id 配对；超时回 `timeout` 且不重试；连接断开则在途请求就地中止、**不产生响应**
  3. 错误闭集 `error.data.kind`（SPEC §3 原文十个），常量带「整词出现在 SPEC 引文里」的测试（§8 的规则）
- **明确不做**：任何业务方法（offer set 为空，调任何方法都是 `-32601`，**不得为了凑 forbidden 测试提前注册方法**）；把连接循环接进 daemon（那是 Supervisor 的事，见 ME3-SUP）
- **SPEC 的空档，本刀要补并请评审拍板**：§3 说「并发上限见 §5，超限回 busy」，但 §5 没有给数字；超时也没有数字。取与代理侧一致的 **每连接 64 个在途 / 单次 30s**，写进 SPEC
- **收尾**：批准时的不阻塞项 F2（回收块读帧不看背压）、F3（只发通知时写端已死没人发现）、F5（方法名与信封键回显不截断）在 `fix/me3c-review-lows` 修掉，各带一条经变异验证的测试；F4（⚖️ 64 的内存依据低估）改文档并记 FU-53；30s 改为「所属请求剩余时间」记 FU-54。
- **验收**（取自 SPEC §8 ME-3c 格，每条带正对照）：握手后的超长行被拒并断连；并发在途按 id 配对、响应可乱序；仍在途的 id 被复用 → 该请求失败；`$/cancelRequest` 使目标请求回 cancelled；连接断开则在途请求中止且不产生响应；超时不重试；握手后 params 解析失败 `-32602` 且不派发；坏 params 只失败该行、连接继续；重复 JSON key 被拒；握手后畸形 JSON `-32700` 只失败该行；握手后重复 `initialize` → `-32600` 只失败该行；业务方法一律 `-32601`
- **证据**：外部评审对 `d983af9` APPROVE（DeepSeek → Opus → Codex → Opus 四轮）；评审方 11 格变异全红；`rpc` 38 条测试、CI 5/5 绿。

### ME3-SUP 3b-3 的 Supervisor —— 让 daemon 真的持有并监督模块进程  `DONE` — #178–#184（2026-09-13）
- **为什么单列**：3b-3 目前只交付了库层零件（`RestartPolicy`、`terminate_group`、`launch::spawn`），daemon 里没有任何代码真正持有一个模块进程。没有它：3b-5 的热 disable 接不进 `os disable`；`KillPermit` 绑定不到具体进程（FU-46）；**T9（3f 仓外包端到端验收）跑不起来**；T7（3e）的 handler 也需要一条已经绑定到某一代的真实回调连接
- **规划时的三个发现**（2026-09-12，各自核实过）：① `domain.rs` 那道拒绝，真实磁盘包根本走不到 —— `server.rs` 给发现到的包配的 `build` 闭包直接 `Err`，所以绊线测试 `an_out_of_process_manifest_is_refused_not_half_mounted` 测的是另一条路；② SPEC 的「fd 3 传监听 socket」需要 `pre_exec`，与全仓 `forbid(unsafe_code)` 冲突；③ `initialize` 的 `id: u64` 违反 SPEC §3「请求 ID 类型：字符串」
- **用户裁决（2026-09-12）**：
  - **D1 回调断线 = 这一代结束**：同一代不许重连；「回调 EOF ⇒ 模块必须退出」写进 wire 契约（于是 daemon 被 SIGKILL 后，孤儿模块会自己退出）
  - **D2 只做热 disable**：enable 仍下次启动生效；熔断后要恢复就重启 daemon
  - **D3 fd 3 用 `command-fds` 依赖传**：unsafe 在依赖里，本仓仍零 unsafe，SPEC 不改
  - **D3′ 其余 fd 由跳板进程在子进程一侧标 close-on-exec**（SUP-1 第二轮复审发现父侧标记挡不住并发窗口后补定；另两个选项是「开一处审计过的 unsafe」与「接受残留窗口」）：本仓仍零 unsafe
  - **D4 每一代新 bind 一个端口**：代理按代取上游地址（FU-50 随之闭上；旧一代 backlog 里的请求不会被新进程执行）
- **切成五刀**（SUP-1、SUP-2 可并行；解除挂载拒绝在 SUP-4；ME-3g 与签名都不是它的前提 —— F4e 是正确性问题，不是 §0 意义上的安全问题）：
  - **SUP-1 进程所有权 + 启动加固** `DONE` — [#178](https://github.com/iDoris-ai/Agent24/pull/178)（`f86b9d4`，2026-09-12；PR 前 Codex 3 轮 + 5 次窄范围复核，外部评审一次 REQUEST_CHANGES（B1：generation 可被共享）后 APPROVE）：`ModuleProcess` 是子进程与它那一代的唯一持有者，只有 `stop(self)` 能杀，而它先撤销**自己那一代**；`revoke` 收成 crate 内可见（FU-46）；drop 也是先撤后杀；spawn 清环境变量到白名单、监听 socket 作为 fd 3、经跳板启动（其余 fd 在子进程一侧标 close-on-exec）、stdin 为 null、stdout/stderr 限行长限速率读走；整棵包目录树属主校验（安装器配套去掉组/其他写权限）；组没清完不收尸、stop 取消安全且以「组已空」为成功；`RestartPolicy::ready` 改为 `ran`（在进程结束时调用，否则计数永不清零）
  - **SUP-2 回调端点 + 握手驱动** `DONE` — [#179](https://github.com/iDoris-ai/Agent24/pull/179)（`a108de3`，2026-09-13；批准时的 Low/Info 在 SUP-3a 分支第一个提交收掉）：UDS 目录 `0700` 且校验属主、每代一个短路径、只 accept 一次；握手失败先写错误行再断连；`initialize` 的 id 改字符串；`rpc::serve` 加停止输入
  - **SUP-3 `Supervisor` 循环**（2026-09-13 再拆两刀：两者合在一起远超可审规模 —— 代理的上游地址今天在挂载时写死，改动面近百处）：
    - **SUP-3a Supervisor 循环** `DONE` — [#180](https://github.com/iDoris-ai/Agent24/pull/180)（`67c0ad9`，2026-09-13；PR 前 Codex 11 轮，第 5 轮起把「接管」协议换成「一个槽位一个 Supervisor」；外部评审一次 APPROVE，2 条 Low 记 FU-58/59）：一个模块的一生 —— 换上新的一代 → 绑新端口（D4）→ 监听回调 → 经跳板启动 → 限时握手 → ready → 以绑定到这一代的 `Methods` 服务回调连接（FU-49）→ 等「进程退出 / 回调断开（D1）/ 收到停止」之一 → 先撤后杀 → 退避重启或熔断；`SupervisorHandle { stop, status }`；FU-44 端到端；用 Python 写的模拟模块测
    - **SUP-3b 代理按代取上游地址** `DONE` — [#181](https://github.com/iDoris-ai/Agent24/pull/181)（`3758836`，2026-09-13；PR 前 Codex 5 轮，外部评审一次 APPROVE，2 条 Info 记 FU-62/63）：`Generation` 带上它那一代的地址（`serving_at`，占位代永不 Running），代理每个请求发往准入它的那一代（D4）；上游连接按代复用、只复用无请求体的请求，其余一律用后即弃并中止驱动，撤销那一代的空闲连接立即关闭（FU-47、FU-50）
  - **SUP-4 接进 daemon，解除挂载拒绝** `DONE` — [#182](https://github.com/iDoris-ai/Agent24/pull/182)（`63060bf`，2026-09-13；PR 前 Codex 8 轮，外部评审一次 APPROVE）：`Installed` 分进程内/进程外两种；绊线测试挪到真实路径；daemon 退出时有界地等所有 Supervisor 停完（否则子进程成孤儿）
  - **SUP-5 热 disable** `DONE` — [#183](https://github.com/iDoris-ai/Agent24/pull/183)（`420c2eb`，2026-09-13；PR 前 Codex 7 轮（第 7 轮无 Medium+），外部评审一次 APPROVE，1 条 Low 在跟进批次收掉）：`os disable` 对运行中的进程外模块走两阶段停止 —— 写配置与交出停止在 daemon 自己的任务里一步完成，PATCH 等到这一代不再准入才回 200（超时 `503 disable_pending`、停止失败 `500 stop_failed`），停机等热停止并在绝对截断时刻中止；CLI help、`os_routes.rs` 文档、`restart_required` 的承诺同时改写
- **ME3-SUP 全部完成**（SUP-1 … SUP-5，#178–#183）。余下跟进项见 `followups.md`；FU-57/PROBE/FU-60/FU-61/FU-64/ERR-1 均已收尾（#191/#190/#192/#193/#194），「三、收尾 ME3-SUP 的跟进项」全部 `DONE`
- **SUP-1 验收**（每条带正对照，变异验证）：外部拿不到许可证、调不了 `revoke`、碰不到子进程（5 条 `compile_fail`，各被对应变异单独弄红，对照 `stop` 能编译）；子进程环境里没有父进程的 `CARGO_MANIFEST_DIR`，只有白名单与 `A24_*`；fd 3 上能 accept；子进程只开着 fd 0–3；模块死后端口立刻拒绝；往 stderr 灌 2 MiB 不阻塞；他人可写的包目录 / `bin/` / 程序被拒；`stop` 撤销的是自己那一代并报告 abandoned / never_sent；drop 先撤后杀；首领按时退出、忽略 SIGTERM 的助手仍被杀
- **依赖**：T6（回调连接循环）✅

### ME3-NEXT 执行队列（2026-09-13 用户裁决；按顺序做，直到 T12 发布 v0.5.0）

> 来源：ME3-SUP 收尾汇报。用户裁决：停机时长按建议保留默认值（排空 0.8s + 停止宽限 0.5s，总 2s），**但必须有日志、有跟踪、可调**；FU-60 改走 Unix socket；FU-61、FU-64 按下面的建议做。
> 每一项仍走完整流程：worktree → 实现 → 变异验证 → Codex 对抗轮 → pre-pr-check → PR → PR-Daemon → 合并。**带状态机的项先写一页语义说明并让 Codex 审设计，再写代码**（SUP-5 的教训：7 轮 Codex 里 4 轮在补语义；规则提案已提给 PR-Daemon：jhfnetboy/PR-daemon#8）。

**一、先止血：测试不许再漏进程**（2026-09-13 发现 31 个遗留的「忽略 SIGTERM」测试夹具进程，空转 1–2 天，已全部 SIGKILL）
- **HYG-1 顽固模块测试夹具的寿命上限** `DONE` — [#186](https://github.com/iDoris-ai/Agent24/pull/186)（`c51a562`，2026-09-15）：`supervise.rs` 三个 shell 顽固夹具经 `stubborn(cap, body)` 生成，带同组、忽略 TERM 的看门狗，120s 到时 `kill -KILL 0`；Drop 守卫在测试进程被杀时来不及跑，所以用夹具自带的寿命上限（主防线）
- **HYG-2 变异脚本收尾** `DONE` — [#186](https://github.com/iDoris-ai/Agent24/pull/186)：`mutate.py` 每次运行专属 TMPDIR，结束后按命令行清扫被 init 收养的孤儿并以「⚠️ 漏收进程」警告（纵深防御；exec 走的孤儿认不出，记 FU-65）

**二、停机可观测、可调**（用户裁决：参数有问题要能被发现、能被调整）
- **SHUT-1 停机可观测性** `DONE`（2026-09-19 校对：三刀 #187/#188/#189 均已合并，原状态 `IN_PROGRESS`/`IN_REVIEW` 过期）—— 先写语义说明 [`docs/design/SHUT-shutdown-observability.md`](../design/SHUT-shutdown-observability.md)（写代码前 Codex 设计审查 4 轮，v5 定稿），拆三刀叠加：**SHUT-1a** 停止事实记录 + 当场告警 `DONE` — [#187](https://github.com/iDoris-ai/Agent24/pull/187)（`d94217b`）；**SHUT-1b** 参数可调、截止模型、汇总落盘 `last-shutdown.json`、`daemon.alive` 跨启动证据 `DONE` — [#188](https://github.com/iDoris-ai/Agent24/pull/188)（`51e19fa`）；**SHUT-1c** 实时出口（`GET /api/v1/shutdown`、`agent24 daemon status` 的「停机」一段、OpenAPI + api-client、contract 测试）`DONE` — [#189](https://github.com/iDoris-ai/Agent24/pull/189)（2026-09-16）
- **SHUT-2 参数可调** —— 并入 SHUT-1b：`A24_MODULE_DRAIN_MS`（0–10000，默认 800）/ `A24_MODULE_STOP_GRACE_MS`（100–5000，默认 500），非法值告警回落默认（不拒绝启动：CLI 吞 stderr、launchd 会崩溃循环），调大时上界诚实变大并写进启动日志；TASKS B2 改为「默认参数下 ≤ 2s」
- **SHUT-3 测试** —— 分散进 1a/1b：超宽限被杀有记录有告警、排空到期切断有记录有告警、SIGKILL 后下次启动告警 / 干净后不告警、ephemeral 不碰证据；0.3s/1s 退出的对照由 `leader` 事实与宽限测试覆盖

**三、收尾 ME3-SUP 的跟进项**
- **FU-57 Supervisor 失败细分** `DONE` — [#191](https://github.com/iDoris-ai/Agent24/pull/191)（语义说明 [`FU-57-run-failure-kinds.md`](../design/FU-57-run-failure-kinds.md) v3，写码前 2 轮设计审查；代码 3 轮 Codex，最后一轮无 Medium+）：`failure.rs` 按阶段穷举分类 setup / refused / io / timeout / exited；`Starting.after` / `Backoff.last` / `GaveUp.last` 带上；回调断开先撤销再等 `exit_settle`（默认 100ms），崩溃报 `exited` 而不是 `io`。
- **PROBE 修 `me3-status.sh`** `DONE` — [#190](https://github.com/iDoris-ai/Agent24/pull/190)（5 轮 Codex，最后一轮无 Medium+）：找今天的符号 `pub (async )?fn spawn` / `pub fn supervise`；新增 ◌ 态「文件已在 main、符号不在」；一遍从左到右的词法扫描去掉注释/字面量再认符号，替换时两端垫空格防止拼出伪造的 `/*`；自证在 REF 上探两个已交付的 SUP 符号。
- **FU-60 模块 HTTP 改走 Unix socket** `DONE` — [#192](https://github.com/iDoris-ai/Agent24/pull/192)（语义说明 [`FU-60-unix-domain-inbound.md`](../design/FU-60-unix-domain-inbound.md) v4，写码前 3 轮设计审查；PR-Daemon 首轮 REQUEST_CHANGES 抓到一个真实的 CI flake——`ModuleListenPath` 生命周期测试的 `(dev, ino)` 身份判据在 inode 复用下会误删 replacement，非环境问题；改成合成不匹配节点的确定性测试后 re-review 翻 APPROVE）：D4 从「每代一个新端口」改为「每代一个新 socket 路径」，`CallbackDir::open_generation()` 一次原子发号绑出回调 socket（`.sock`）与入站监听 socket（`.l`）一对，放在同一个 `0700` 目录下；`ModuleListenPath` 守卫的生命周期跟 `ModuleProcess` 同寿命而不是随daemon 侧 fd 关闭而删（模块可能连续被 accept 好几个小时）；转发给模块的 `Host` 头改成固定合成值 `agent24-module.invalid`，不再从地址派生；rebase 到含 FU-57 的 main 后接上它的 `setup` 分类（`failure::listen` 文案改中性，不再点名回调还是入站监听）。没有 TIME_WAIT，也不占用端口号命名空间。**判据**（v4 收窄为确定性断言，压测挪到人工/按需）：上游连接与继承的 fd 均为 `AF_UNIX`；daemon 侧 fd 早于路径清理关闭（模块死后立即拒绝、路径仍在）；`stop()` 之后路径消失、`NotFound`；spawn 失败不留孤儿 `.l`；`Host` 头是固定值、不含状态目录路径片段。
- **FU-61 包在运行中被卸载或替换**（按建议：检测变更、报「需要重启」，不做快照）`DONE` — [#193](https://github.com/iDoris-ai/Agent24/pull/193)（`8b3225c`，2026-09-16；语义说明 [`FU-61-package-changed.md`](../design/FU-61-package-changed.md) v6，写码前 5 轮设计审查，终审无 Medium+）：`discovery::recheck` 重启前核对清单目录/摘要；包不在或清单变了 → 不重启，状态 `package_changed`，带唯一真实有效的解决办法（重启 daemon）；新增 `POST /api/v1/os/{name}/stop`（不写 `os.json`，回避 PATCH 会留 tombstone 拖垮整个注册表的坑）给 `agent24 os uninstall` 做尽力而为的热停；`agent24 daemon stop` 改为等到单例锁真正释放再报成功，顺带修好一个既有的不可靠重启建议。判据：运行中卸载 → 状态报包已移除而非熔断，且不影响其它模块；原地替换 → 不进入握手失败循环。快照方案记为后续选项，不做。
- **FU-64 连接复用撞上模块关闭**（按建议：三件一起做）`DONE` — [#194](https://github.com/iDoris-ai/Agent24/pull/194)（`fa587cc`，2026-09-16；语义说明 [`FU-64-ERR-1-reuse-race-and-hints.md`](../design/FU-64-ERR-1-reuse-race-and-hints.md) v6，写码前 6 轮设计审查，终审无 Medium+；代码另 3 轮 Codex 代码审查，终轮无 Low+）：① 幂等请求（无请求体的 GET/HEAD/OPTIONS）遇到「发出时连接已被关」自动换新连接重发一次，重试强制走全新连接、发送前二次核对撤销；② 复用连接的空闲上限短于常见的模块 keep-alive（默认 4000ms，`A24_MODULE_IDLE_CONN_MAX_MS` 可调），到期不再复用；③ 剩下的 502 返回结构化提示：`code: upstream_connection_closed`、说明「模块在请求发出的同时关闭了连接；这个请求可能已被处理，也可能没有」、解决办法「确认无副作用后重试；频繁出现请调大模块的 keep-alive」。判据：每次应答后立即关连接的模块，连续无间隔 2000 个 GET 零 502；POST 在同一情形下的 502 带上述字段。
- **ERR-1 所有「连不上模块」的应答都带原因与解决办法** `DONE` — [#194](https://github.com/iDoris-ai/Agent24/pull/194)（与 FU-64 同一个 PR、同一份语义说明）：`module_not_ready` / `module_draining` / `module_stopping`（含拆出的 `Stopping` 过渡态）/ `circuit_breaker_tripped` / `package_changed` / `stop_failed` / `module_panicked`（新）/ `module_killed`（新）/ `disable_pending` 统一带 `hint`，CLI 原样打印；六处「重启 daemon」建议统一成一条托管感知的共享常量（`RESTART_DAEMON_INSTRUCTION`），不再无条件建议会导致 launchd 脱管的命令（关联 FU-68）。判据：每个错误码一条测试，断言 `hint` 非空且写明可执行的下一步。

**四、主线到发布**（任务定义与验收见 [`PLAN-OOP-OS-AND-BACKLOG.md`](PLAN-OOP-OS-AND-BACKLOG.md) §五「主链」）
- **T8.5 ME-3d 记忆回调**（`private/*`）——原计划一次性交付，设计文档 [`T8.5-ME3d-memory-private.md`](../design/T8.5-ME3d-memory-private.md)（worktree `Agent24-t8.5`）走了 3 轮 Codex 设计审查仍未收敛（v1 2 Critical/6 High/5 Medium，v2 2 Critical/4 High，v3 仍 2 Critical 未关掉，且 v3 为了关 FU-54 改 `agent24-os-proto` 的 `drain.rs` 请求生命周期信号时，破坏了现有 `progress()`/`revoke()`/`admit_approval_callback()` 依赖 `in_flight` map 成员即"活着"这条不变式——Codex 判定"改动已不是 pure additive，牵动共享基础设施"，建议拆分。2026-09-17 用户拍板采纳，按 Codex 给的三段切法重排（**编号变更，不再用单一 T8.5**）：
  - **T8.5a 请求生命周期信号**（`agent24-os-proto`/`drain.rs`，闭 FU-54）`DONE` — [#205](https://github.com/iDoris-ai/Agent24/pull/205)（`771ccde`，2026-09-17；语义说明 [`T8.5a-request-lifecycle-signal.md`](../design/T8.5a-request-lifecycle-signal.md) v2，2 轮 Codex 设计评审——v1 1 Critical(参考实现选了同步函数当判据载体)/1 High(budget 读常量不读配置态)/1 Medium/2 Low，v2 APPROVE；代码 2 轮 Codex 代码评审——首轮 1 Medium 真实竞态(`bind_to_lifecycle` 的 `select!` 未加 `biased`，work 与已结束请求同时 ready 时可能误报 `RequestEnded` 导致重复副作用)，修复后二轮 APPROVE）：`InFlightEntry` 新增 `ended: watch::Sender<bool>`(`send_replace`)+`deadline`，跟 `ApprovalToken` 同生共死，不引入墓碑，`progress`/`revoke`/`admit_approval_callback`/`admit_callback` 四个现有读者逻辑不变；`admit_request` 新增显式 `now`/`budget` 参数；共享 helper `bind_to_lifecycle` 实现 `min(30s, 请求剩余时间)` 协作式取消；`events_emit.rs` 为唯一真实接线点。
  - **T8.5b 权威事件用量计数 + 迁移/rekey**（`agent24-memory`，为真配额打底）`DONE` — [#207](https://github.com/iDoris-ai/Agent24/pull/207)（`aa24e05`，2026-09-18；语义说明 [`T8.5b-authoritative-quota.md`](../design/T8.5b-authoritative-quota.md) v1，1 轮设计评审 APPROVE；代码 3 轮 Codex 代码评审——首轮 reject：1 High(新 owner 首条写入因标量子查询无匹配行返回 NULL、COALESCE 落空而完全绕过配额)+1 Medium(M1 默认配额行保护只挡 DELETE 不挡改名)+2 Low，修复后二轮 approve-with-followups，追加封死 payload 列裸改缺口后三轮 approve-with-followups）：三个触发器（`AFTER INSERT`/`UPDATE OF scope_owner`/`DELETE`）附着 `mem_events` 表本身维护 `mem_owner_usage`，权威性不依赖任何 Rust 调用点；`BEFORE INSERT` 触发器做原子配额检查，`WHEN NOT EXISTS` 守卫让幂等重放跳过配额；`LENGTH(CAST(payload AS BLOB))` 真实字节计数；`'*'` 默认配额行防删除+防改名双重保护；`mem_events_bu_payload_immutable` 封死裸改 payload 缺口。17 条判据落成测试，关键修复均做变异验证。
  - **T8.5c `_a24/memory/private/*` RPC 族本身**——依赖 T8.5a（请求生命周期，已合并 #205）+ T8.5b（权威配额，已合并 #207）都落地后才能设计分页/取消/配额的最终形态，两者均已就绪。v1 设计文档（`docs/design/T8.5c-memory-private-rpc.md`，worktree `Agent24-t8.5c`）送 1 轮 Codex 设计评审 reject（2 Critical：响应字节预算没并入游标状态机导致大 payload 场景下永久跳过记录；memory 限流器设计成每 generation 重建满桶，直接违反 SPEC §5"跨 generation/重启不重置"的 MUST——恶意模块可靠自杀重启刷新限流。3 High：`MemoryEntitlement` 没把 grant/handle/Offer/MountReport 收成一个事实源、durable catalog 幂等与"本轮活跃分区清单"混为一谈、配额错误响应泄露内部 owner/partition key）。Codex 判定"方法族本身"这个范围仍然太大、两个正交问题（分页/游标资源状态机 vs 挂载/wire接线/entitlement）互相污染判据，建议再拆。2026-09-18 用户拍板采纳，重排为：
    - **T8.5c-P 分页/游标/资源计费状态机** `DONE` — [#210](https://github.com/iDoris-ai/Agent24/pull/210)+[#211](https://github.com/iDoris-ai/Agent24/pull/211)+[#212](https://github.com/iDoris-ai/Agent24/pull/212)（2026-09-19；语义说明 [`T8.5c-P-pagination-cursor.md`](../design/T8.5c-P-pagination-cursor.md) v4，4 轮 Codex 设计评审——v1 reject(1 Critical+3 High+2 Medium+1 Low)→v2 reject(3 High+3 Medium+2 Low)→v3 reject(1 High+6 Medium+1 Low)→v4 approve-with-followups；代码 3 轮 Codex 代码评审收敛 APPROVE，首轮 1 Medium(判据 9b 手工信号量证明力不够，改真实文件后端 pool+池外独立连接真实占写锁)、二轮 3 Low(sleep 同步不可靠等)全部修复）：为避免一次性提交 2000+ 行的大 PR，实现按依赖顺序拆成 3 个 PR 顺序合并——#210(存储层 `EventLog::scan_stream` 流式扫描)→#211(`RateLimiter` 加权扣费/退款)→#212(核心状态机 `os_memory_page.rs`：cursor=最后一个已解决行、`Reservation`(拥有 `Arc<RateLimiter>` 无生命周期参数满足 `CallFuture: 'static+Send`)、`Needle`/`PageMode` 统一 normalize、跨模块共享 `Arc<Semaphore>` 连接池准入)。`OsScopedMemory::{remember_checked,recall_page,recent_page}` 做成 inherent 方法(显式接收 lifecycle/limiter/admission 参数)——T8.5c v1 的 JSON-RPC Handler/`MemoryEntitlement` 在仓库里完全不存在，mount 层单例创建、真实 wire 接线留给 T8.5c-W。15 条判据全部落成测试。
    - **T8.5c-W 挂载/wire接线/entitlement**——v1 设计文档（`docs/design/T8.5c-W-mount-wire-entitlement.md`，worktree `Agent24-t8.5c-w`）送 1 轮 Codex 设计评审 reject（2 Critical：`Capability::Memory` 没被加进 `KERNEL_OOP_GRANTS`，按设计原样实现功能永远不可达；admission 容量假设 5 个连接，但 ephemeral 模式连接池实际只有 1 个，会让 OOP 调用完全饿死进程内路径，违反 T8.5c-P §12 的 MUST。3 High：catalog 拆分只堵了内存态清单没堵住持久 `last_seen_at` 幽灵刷新；错误脱敏只处理 `QuotaExceeded` 一种、其它底层错误原样透传上 wire——**这个具体缺口（`os_memory.rs:787`）是已合并代码里真实存在的信息泄露 bug，但目前没有任何 JSON-RPC Handler 接线到这几个方法，外部攻击面不可达，不需要单独热修，随 T8.5c-W 一起修**；真实资源竞争判据没进 W 的验收）。Codex 建议按时序分组拆成两份（不是按决策编号逐个拆——W1/W2/W3 共享同一条 `lend→entitlement→supervisor注册→catalog激活` 时序，拆散会漏跨阶段不变式）。2026-09-19 用户拍板采纳，重排为：
      - **T8.5c-W-mount 挂载/entitlement/限流器/catalog** `DONE` — [#216](https://github.com/iDoris-ai/Agent24/pull/216)+[#217](https://github.com/iDoris-ai/Agent24/pull/217)+[#218](https://github.com/iDoris-ai/Agent24/pull/218)（2026-09-19；语义说明 [`T8.5c-W-mount.md`](../design/T8.5c-W-mount.md) v6，6 轮 Codex 设计评审——v1 reject(2 High+3 Medium+1 Low)→v2 reject(1 Critical+3 High+5 Medium)→v3 reject(3 High+4 Medium+2 Low)→v4 reject(1 High+2 Medium+1 Low，首次无 Critical)→v5 reject(1 High，纯口径一致性)→v6 approve/FREEZE；同步把 `T8.5c-P-pagination-cursor.md` 修订到 v5 §13，正式声明 admission MUST 契约排除 ephemeral 单连接池，不是下游文档单方面解释）：`KERNEL_OOP_GRANTS` 加入 `Capability::Memory`；admission `Semaphore` 构造挪进 `KvStore::open`/`open_memory` 内部（ephemeral 结构性不发放 OOP memory 能力，而不是构造一个容量为 0 的死锁 permit）；`OsMemoryCatalog` 拆成 `ensure_recorded`/`mark_mounted` 两阶段，修正 `last_seen_at` 假活跃问题（迁移 0015）。实现按依赖顺序拆 3 个 PR：#216(设计冻结文档本身)→#217(agent24-memory 存储层)→#218(agent24d 挂载接线)。中途 #217 因 `last_seen_at: String→Option<String>` 破坏了 agent24d 里两个既有测试的编译被 `clestons` 打回，修复后 #217 独立合入；#218 rebase 到新 base 上（冲突全部出在同一批测试的改名，保留 #218 自己的 `ensure_recorded`/`mark_mounted` 版本）后重新过 `clestons` 评审收敛。收尾 follow-up `DONE` — [#219](https://github.com/iDoris-ai/Agent24/pull/219)：补 rebase 冲突解决时冲掉的一条回归断言（"已有真实 `last_seen_at` 的分区再次 `ensure_recorded` 不得被重置"）+ 一处过时注释。
      - **T8.5c-W-wire JSON-RPC Handler/错误映射/scoped 路由** `DONE` — [#221](https://github.com/iDoris-ai/Agent24/pull/221)(设计冻结)+[#224](https://github.com/iDoris-ai/Agent24/pull/224)+[#225](https://github.com/iDoris-ai/Agent24/pull/225)（2026-09-19；语义说明 [`T8.5c-W-wire.md`](../design/T8.5c-W-wire.md) v5，5 轮 Codex 设计评审——v1 reject(2 Critical+5 High+2 Medium+2 Low)→v2 reject(1 Critical+4 High+2 Medium+1 Low)→v3 reject(2 High+1 Medium+2 Low，首次无 Critical)→v4 reject(1 High+1 Medium+1 Low)→v5 approve/FREEZE；代码 2 轮 Codex 代码评审——首轮 5 Medium(判据覆盖面，未发现生产代码缺陷)，修复后二轮 approve，另发现 1 Low 已修复）：详见上方"当前正在做"之前的完成记录。旧稿 `T8.5c-W-mount-wire-entitlement.md`（拆分前 v1，未送审）已删除。
    两者设计各自独立评审收敛，实现阶段仍可合一个 PR 交付。
  三轮评审详情见 Codex session `01a0ae9e-79a4-76f1-93c5-b160579de7c5`（`codex resume 01a0ae9e-79a4-76f1-93c5-b160579de7c5` 可续）。
- **T7 ME-3e 事件 + 审批** —— 设计阶段拆成三块：**T7a**（能力授予接线 + `_a24/events/emit`）`DONE` — [#199](https://github.com/iDoris-ai/Agent24/pull/199)（`147f8bf`，2026-09-17；语义说明 [`T7a-ME3e-grants-and-events.md`](../design/T7a-ME3e-grants-and-events.md) v4，写码前 4 轮设计审查，终审 0 Medium+；代码 1 轮 Codex 代码审查，修复 events 专属资源上限的度量方式）：`Offer`/`Grants`/`MethodsFor` 从「进程外模块永远拿不到能力授予」改成「按 manifest 声明真授予」，交付第一个真实回调方法 `_a24/events/emit`；`dispatch()` 新增对所有方法通用的 params 体积预算（节点数/深度/字符串字节，含 object key）。SPEC §8 的 offer set 阶梯按实际交付顺序补记（Memory 未排期，Events 独立先行）。**T7b**（`gate`/`advise`/`status` 模块审批）`DONE` — [#201](https://github.com/iDoris-ai/Agent24/pull/201)（`a37e9ef`，2026-09-17；语义说明 [`T7b-ME3e-approvals.md`](../design/T7b-ME3e-approvals.md) v6，写码前 5 轮设计审查——前 4 轮针对同步阻塞模型，第 4 轮发现该模型与现有 30 秒 RPC/代理超时冲突，架构改为异步提交+轮询后第 5 轮收敛；代码 1 轮 Codex 代码审查）：`approval_token` 与 `request_id` 同一次 `admit_request` 原子登记；提交按 `(module, request_id, kind)` 幂等去重；`gate` 命中空闭集不消耗令牌；`ModuleApproval` 单一 `decision` 维度 + 周期扫描判定超时（容忍单次存储失败、daemon 重启无需特殊清扫）；REST `/api/v1/module-approvals` + WS `module-approval.{required,resolved}`。`gate` 本轮闭集仍为空，真实执行留给 T7c。**T7c**（`gate` 第一个内核可执行动作：`schedule_callback`）`DONE` — [#203](https://github.com/iDoris-ai/Agent24/pull/203)（`5129c55`+`e0c4c12`，2026-09-17；语义说明 [`T7c-ME3e-gate-execution.md`](../design/T7c-ME3e-gate-execution.md) v3，2 轮设计审查——第 1 轮发现"接 `agent24-scheduler` 引擎"这条路有 4 个 Critical，第 2 轮确认"改成直接扩展 T7b 自己的周期扫描"消除了全部 Critical；代码 2 轮 Codex 代码审查，首轮 1 High(迁移文件未入库)+2 Medium(pre-epoch 时间戳误拒/`executed_at` 泄漏进冻结事件)+3 Low，二轮 APPROVE；合入后外部评审又独立抓到 year≥10000 时间戳字典序比较破口，同 PR 追加 `0..=9999` 年份守卫后合并）：`execute_due_schedule_callbacks` 独立 CAS 扫描；`validate_gate_action`/`canonicalize_schedule_target` 双路（wire/in-process）共用；`ModuleApprovalSubmitted` 专用 WS payload 不含 `executed_at`。T7（a/b/c）三块全部合入 main。
- **T8 ME-3g 启用路径准入** `DONE` — [#196](https://github.com/iDoris-ai/Agent24/pull/196)（`de03b4b`，2026-09-16；语义说明 [`T8-ME3g-enable-admission-gate.md`](../design/T8-ME3g-enable-admission-gate.md) v7，写码前 6 轮设计审查，终审无 Medium+；代码 1 轮 Codex 代码审查）：`PATCH /api/v1/os/{name}` 新增准入门，只在这个名字恰好一条 `os_reports` 报告时触碰（重名维持既有的无条件放行，明确划出范围）——已经是 `Refused` 直接拒绝；`Disabled` 的进程外模块现场重扫清单（按目录匹配、先查名字防改名绕过、再查交付方式自洽性），编译进内核的 `Disabled` 模块关不上（核对需要调用 `build()`，架构本身的安全线逼出的限制，记 `FU-6x` 独立跟进）。`AppState` 新增 `packages_root`/`package_dirs`；`ErrorBody.code` 补 `admission_refused`，顺手补全 FU-64/ERR-1 时代漏掉的 `module_panicked`/`module_killed`。17 条判据全部落成确定性测试。
- **T9 ME-3f 仓外包端到端 —— 验收** `DONE` — [#262](https://github.com/iDoris-ai/Agent24/pull/262)（2026-09-19）：详见上方"T9 已交付，ME-3 专项整体收口"记录。**ME-3 专项到此全部完成。**
- **T13 `agent24-os-sdk`** 与 **T14 wire 文档 + 非 Rust 参考实现**（可与 T9 之后并行）
- **T11 Sin90 迁出内核** `DONE` — [#342](https://github.com/iDoris-ai/Agent24/pull/342)（2026-09-22，详见上方"T11 已交付"记录）
- **T10 Cos72 进程外样例**（重做暂停中的 `feat/me4-cos72-skeleton`，明确暂停）→ **T12 发布 v0.5.0**

**五、仓外事项**
- **PR-Daemon 规则 S1**：状态机改动先交一页语义说明，否则 block。已提 [jhfnetboy/PR-daemon#8](https://github.com/jhfnetboy/PR-daemon/issues/8)，等 PR-Daemon 落地；落地前本仓库按上面的约定人工执行。

---

## M1 —— 记忆成为产品（2026-08-23 规划；状态未改动，未重排）

## F1.1 — 判定接缝（原 F8b）

> 依赖 PR #140（F8）已合并。**若 #140 尚未合并，本 Feature 全部 task 保持 BACKLOG。**

### T1.1.1 `Authorizer` 契约与默认实现  `READY`
- **优先级**：high
- **目标**：内核持有一个判定点，签名一次到位。
- **开发范围**：在 `rust/apps/agent24d/src/authz.rs` 新建 `Actor` / `Op` / `AccessRequest` / `Decision` / `Authorizer`（签名逐字照 `architecture.md`「契约 / 接口」一节）；实现 `ModulePrivateOnly`：`allow ⟺ space == SpaceId::module_private(req.module)`。
- **明确不做**：不接线（T1.1.2 做）；不引入任何存储；不放进 `agent24-domain`（那是模块契约，出现能命名空间的参数就是模块能跨的边界）。
- **依赖**：无
- **交付物**：`agent24d/src/authz.rs` + 单测
- **验收命令**：`cd rust && cargo +1.98.0 test -p agent24d --bin agent24d authz`
- **验收要求**：至少三条断言 —— 自有空间 allow；他模块空间 deny；`Decision.reason` 非空（审计要用）。
- **涉及文件**：`rust/apps/agent24d/src/authz.rs`、`rust/apps/agent24d/src/main.rs`(mod 声明)
- **风险/回滚**：纯新增，无回滚风险
- **证据**：

### T1.1.2 把判定点接进句柄发放路径  `BACKLOG`
- **优先级**：high
- **目标**：句柄发放**经过**判定点，且**行为零变化**。
- **开发范围**：`MemoryLease::lend` 在 `catalogue.record(...)` 之前调用 `Authorizer::decide`；deny 则不发句柄并 `tracing::warn!` 带上 `reason`。
- **明确不做**：不改判定逻辑；不给模块任何选择空间的能力。
- **依赖**：T1.1.1
- **交付物**：接线 + 「行为零变化」证明
- **验收命令**：`cd rust && cargo +1.98.0 test --workspace`
- **验收要求**：F1/F8 既有的**跨模块隔离探针全部继续通过**；新增一条测试断言「换成一个恒 deny 的 Authorizer 时，模块拿不到 memory 能力」——**这条是变异验证的落点**。
- **涉及文件**：`rust/apps/agent24d/src/domain.rs`、`rust/apps/agent24d/src/authz.rs`
- **风险/回滚**：判定写错会让所有模块失去记忆 → 验收必须包含既有隔离探针全绿
- **证据**：

---

## F1.2 — personal space（原 F8c）

### T1.2.1 `SpaceId::personal` 与不相交性  `BACKLOG`
- **优先级**：high
- **目标**：agent loop 的记忆在模型里**有一个名字**，且不可能与模块空间相撞。
- **开发范围**：`SpaceId::personal(user) -> "usr:<user>"`；一条把 `usr:` 与 `os:` 不相交性钉死的测试（**扫小的交叉积，不是两个手挑的例子** —— 照 F8 的 `the_partition_key_is_versioned_and_unambiguous` 的做法）。
- **明确不做**：不迁移任何数据（T1.2.2 做）；不 bump `KEY_VERSION`。
- **依赖**：T1.1.2
- **交付物**：构造器 + 不相交性测试
- **验收命令**：`cd rust && cargo +1.98.0 test -p agent24d --bin agent24d space`
- **涉及文件**：`rust/apps/agent24d/src/os_memory.rs`
- **证据**：

### T1.2.2 把 agent loop 的记忆迁进 personal space  `BACKLOG`
- **优先级**：high
- **目标**：消掉 ADR-030 硬门槛 3 —— agent loop 的记忆不再用裸 user id 做 key。
- **开发范围**：迁移 `0014_personal_space.sql`（**只登记目录行，不重写 owner_key**，理由见 `spec.md`）；启动时复用 `rekey_os_partition` 把裸 user id 分区搬到 `partition_key(org, SpaceId::personal(user))`。
- **明确不做**：**不在 SQL 里算 key**（SQLite `length()` 数字符不数字节，非 ASCII user id 会得到与内核不一致的 key —— 0013 已经踩过这条，注释里写清）。
- **依赖**：T1.2.1
- **交付物**：0014 + re-key 接线 + 真实升级路径测试
- **验收命令**：`cd rust && cargo +1.98.0 test -p agent24-memory --lib migration_0014 && cargo +1.98.0 test --workspace`
- **验收要求**：用 `pool_migrated_up_to(&path, 14)` 建一个 0013 态的库 + 一条裸 user id 的事件 → 跑 0014 + sweep → **断言那条事件在新 key 下读得到、老 key 下读不到**；再跑一次 sweep **移动 0 个**（幂等）。
- **风险/回滚**：**这是本轮唯一动到真实用户数据的 task。** re-key 必须一个事务；其余八张 owner-scoped 表有行就整体拒绝（`rekey_os_partition` 已有此行为，**不得放宽**）。
- **证据**：

---

## F1.3 — 记忆接进 agent loop（原 F2）

### T1.3.1 会话轮次写进 EventLog  `BACKLOG`
- **优先级**：high
- **目标**：让「情节权威」名副其实 —— 今天 EventLog 里根本没有对话。
- **开发范围**：agent loop 每一轮追加 `MemEvent`（`kind=chat.user` / `chat.assistant`），scope 用 T1.2.1 的 personal space key。
- **明确不做**：还不动 `CanonicalSession` 的压缩（T1.3.2 做）；不改 `Condenser` 本身。
- **依赖**：T1.2.2
- **交付物**：接线 + 端到端测试
- **验收命令**：`cd rust && cargo +1.98.0 test --workspace`
- **验收要求**：一次模拟对话后，`EventLog` 里能按 seq 顺序读回**完整轮次**；`replay` 出的对话与直接读的**逐条相等**。
- **涉及文件**：`rust/crates/agent24-agent/`、`rust/apps/agent24d/`
- **证据**：

### T1.3.2 `Condenser` 取代 `CanonicalSession` 的压缩  `BACKLOG`
- **优先级**：high
- **目标**：一份压缩实现，不是两套并存。
- **开发范围**：压缩改由 `Condenser`（token 预算触发、策略可换）承担；`CanonicalSession` **降级为投影**，`save(kv)` 那条把会话存成 KV blob 的路径退役。
- **明确不做**：**不允许两套并存** —— `SPEC-ME-FOLLOWUPS.md` F2 与 `architecture.md` 核心判断 3 已定死。不新增压缩策略。
- **依赖**：T1.3.1
- **交付物**：切换 + no-loss 保证的等价证明
- **验收命令**：`cd rust && cargo +1.98.0 test --workspace`
- **验收要求**：`CanonicalSession` 原有的 **no-loss 保证必须仍然成立**（摘要失败不丢消息、下次重试）—— 用一条「摘要器必然失败」的测试钉住；一条长对话经压缩后 `covers(n)` 与实际覆盖轮次一致。
- **风险/回滚**：改的是真实会话的压缩路径。**若切换后 no-loss 无法在 `Condenser` 下等价成立 → 标 `BLOCKED`，在 progress.md 写清，不要自行放宽保证。**
- **证据**：

### T1.3.3 崩溃重放对真实会话生效  `BACKLOG`
- **优先级**：mid
- **目标**：兑现 MD-1b —— 重放的是真实对话，不是空的。
- **开发范围**：一条端到端测试：写入对话 → 模拟崩溃（丢弃内存态）→ 从 EventLog 重放 → 上下文与崩溃前**逐条相等**。
- **明确不做**：不新增重放机制（`replay` 已有），只证明它对真实会话生效。
- **依赖**：T1.3.2
- **交付物**：端到端测试
- **验收命令**：`cd rust && cargo +1.98.0 test --workspace replay`
- **证据**：

---

## 依赖链（一眼看清顺序）

```
T1.1.1 → T1.1.2 → T1.2.1 → T1.2.2 → T1.3.1 → T1.3.2 → T1.3.3
```

**严格串行**，没有可并行的分支 —— 每一步都建立在上一步的存储形状上。
