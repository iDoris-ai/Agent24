# PLAN-ME4 — 外置 OS 的内核能力面 + 两个真实消费者 + SDK + v0.5.0

> 立于 2026-09-23。**本文件是 ME-4 这一轮的任务定义与验收标准的权威来源**（地位同 ME-3 时期的
> [`PLAN-OOP-OS-AND-BACKLOG.md`](PLAN-OOP-OS-AND-BACKLOG.md)）。
> **状态**（哪个 task 在做、PR 号）只写在 [`tasks.md`](tasks.md) 的「ME-4 台账」，这里不抄第二份。
> 跨仓库：Sin90 侧 task 的完整定义在 `iDoris-ai/Sin90` 的 `docs/agent/tasks.md`，这里只列**门**。
>
> **版本**：v3（2026-09-23）。v1 经 Codex 对抗评审 **REQUEST_CHANGES（3 Critical / 13 High / 4 Medium / 1 Low）**，
> 21 条逐条核对代码后全部采纳。v2 → 第 2 轮 REQUEST_CHANGES（4 条 PARTIAL + 5 High + 3 Medium + 1 Low）→ **v3** 全部采纳。改动记录见文末 §五。

---

## 〇、为什么是这个顺序（用户 2026-09-23 裁决）

**问题**：Sin90 已迁出内核（T11），但外置模块今天只能用内核的三样东西 —— 事件、审批、私有记忆
（`KERNEL_OOP_GRANTS = [Events, Approval, Memory]`，`rust/apps/agent24d/src/domain.rs:93`）。
个人 OS 需要的**调度**（Routine 提醒）和**模型推理**（AI v1）外置模块都调不到。

**关键事实（决定了顺序）**：T13 `agent24-os-sdk` 按 PLAN 定义**只封装**握手 + 帧 + 回调通道，
「只能调 `agent24-os-proto`」—— **它不带来任何新能力**。缺的是内核回调面本身，不是 SDK。
`Capability::Scheduler` / `Capability::Models` 枚举早就在（`agent24-domain/src/lib.rs:141-161`），
内核从未授予；SPEC-ME3 §9 写的是「先有消费者再有提供者」—— **Sin90 M3/M5 就是那个消费者**。

**所以**：以 Sin90 里程碑为主线，缺哪个内核能力补哪个；SDK 等有了第二个真实模块（Cos72）
再从两个真实调用方身上**提取**，而不是凭空设计。

```
ME4-M0 清账 ─┬─► ME4-M1 调度回调(Agent24) ─► ME4-M2 Sin90 M3 ─► ME4-M3 Sin90 M4 ─┐
             │                                                                   ├─► ME4-M5 SDK→Sin90迁移→Cos72→wire文档 ─► ME4-M6 v0.5.0
             └──────────────────────► ME4-M4a 推理回调(Agent24) ─► ME4-M4b Sin90 M5 ┘
```
（M4a 的**设计**可与 M2/M3 并行；**实现**排在 M1 之后，免得两条回调同时改 `mount_package`。
Sin90 M5 同时依赖 M4a 与 Sin90 M4。）

### 用户裁决（2026-09-23，本轮不再重议）

| # | 问题 | 裁决 |
|---|---|---|
| D1 | 模块注册的 cron 到点，内核怎么通知模块 | **内核经代理通道 POST 到模块自己的保留路由**（`/api/v1/<ns>/_a24/scheduler/fired`），不是只广播事件 |
| D2 | 模块经内核调模型能不能用远端 | **默认强制 LocalOnly**；manifest 显式声明才放开远端，且按模块限流、记用量 |
| D3 | Sin90 仓库流程 | **跟 Agent24 一样：PR + clestons 评审**；建 pilot 七件套；已 APPROVED 的 #2/#3/#4 先合 |
| D4 | 本轮终点 | **五步全做完再发 v0.5.0**（M6 Life Packs 留下一轮） |
| D5 | Cos72 放哪 | **`MushroomDAO/Cos72`**（当前只有 License/README 的空仓） |
| D6 | Cos72 做多大 | **最小真实闭环：mytask 任务 + 积分**（用到事件 + 记忆 + 审批）；myshop/myvote 留给 roadmap M5 |

**与旧文档的冲突，以本表为准**：
- `SPEC-ME3-OUT-OF-PROCESS.md` §3「`Models`/`Scheduler`/`Policy` 都不在本轮」、§9「不做 `Models`/`Scheduler` 回调」——
  **由 ME4-1.1.1 / ME4-4.1.1 的设计 PR 同步改写**（SPEC 与实现 1:1）。
- `docs/decision.md` ADR-029 §3「第一方 Sin90/Cos72 编译进 agentd」—— 已被 2026-09-10 裁决推翻（T11 已执行），ME4-6.1.1 补 ADR 修订记录。
- `docs/RELEASE-CHECKLIST.md` 仍是 v0.1.0 的内容 —— **v0.5.0 不照它执行**，照 ME4-6.0.1 冻结的专用清单。
- `roadmap.md` 的 M1（记忆成为产品）仍排在 v0.5.0 之后，不变。

---

## 一、本轮通用规矩（每个 task 都适用，不再逐条重复）

1. **一个 task = 一个分支 = 一个 PR；一个 Feature = 一个 worktree**（pilot 硬约束）。§三 的切法已按「共享 transport / 类型与存储 /
   单个 handler / REST 或 wire / 黑盒」预拆到 ≤300 行量级；实际仍超 300 行就继续按层拆成 stacked PR，合并父 PR 前先
   `gh api -X PATCH repos/<o>/<r>/pulls/<n> -f base=main` 把子 PR 改到 main（不用 `gh pr edit --base`）。
2. **带状态机或新线协议的 task，先写设计文档、送对抗评审（Codex，额度耗尽期间为 Opus 子代理）到 approve 并「冻结」，再写代码**。格式照 `docs/design/T8.5c-W-wire.md`：
   冻结头（每轮 C/H/M/L 计数）→ 版本改动记录 → §0 解决/不解决 → §1 现状（带行号，重新核过）→ 决策 → 判据（带正对照）→ 自审 → 接口清单 → 已接受残余风险。
   **文档里的 Rust 签名先放进 scratch crate `cargo check`**。**本文 §二 的「硬约束」是设计的下限，设计可以更严，不可以更松**；
   设计冻结后切法若变，先改 §三 与台账再开工。
3. **提 PR 前**：自审 → Codex 挑战（`codex:codex-rescue` 子代理）。**Codex 额度 2026-09-23 耗尽，到 2026-09-29 19:28 才恢复**：在此之前按 CLAUDE.md Tier 2，
   由一个**全新上下文的 Opus 子代理**做对抗评审（只给它 diff、task 定义与相关规范，要求 Critical/High/Medium/Low + file:line），再加 `security-review` skill；
   PR body 标「本地模型评审（Codex 未评审）」，在 Agent24 `followups.md` 记一条 `ME4-CODEX-DEBT` 清单，额度恢复后按清单补审。
   → 中立裁决 → 修 → 再挑战直到干净 → `bash ~/Dev/tools/PR-daemon/scripts/pre-pr-check.sh --base main`（先 ff 更新并 `--selftest`），
   PR body 按编号回应命中项。
4. **新回归测试一律变异验证**（`docs/agent/mutate.sh`）：把修复改回去必须变红；PR body 写明变异方式与结果。每条判据带正对照。
5. **验收命令不许空转**：凡是 `cargo test <过滤词>` 形式的验收，先跑 `cargo test <同参数> -- --list` 并断言匹配数 > 0（cargo 匹配零个测试也返回成功）。
   需要外部工具（node、python3）的测试，**工具缺失即失败，不许 skip**。
6. **评审与合并**：外部 `clestons`（PR-Daemon，常驻 Mac mini）。推完 `SendMessage` 给 `ListAgents` 现查到的 `pr-daemon-mini`
   （`OWNER/REPO#N @<40位sha> — 请复审`），发不出去不重试。**可合并的判据 = 存在一条 clestons 的 APPROVED review，其 `commit_id` 等于 PR 当前 `headRefOid`，且全部 check 为 SUCCESS**
   （`submittedAt` 晚于推送只是「有新 review」的信号，不是审批覆盖当前 head 的证明）。满足即 `gh pr merge --squash`；approve 之后绝不再往该分支推 commit。
   Agent24 走 `git-guard.sh merge-pr --integration main --allow-trunk`；Sin90/Cos72 在 main 未开保护前 git-guard 会 fail-closed，改用 `gh pr merge --squash`，**但上面的 exact-head 判据一条不少**。
7. **台账回填（closure ledger）**：task PR 合并后分支已不能再改 —— 所以 `DONE` 与证据由**下一个 task 的 PR 顺带回填**（只改 tasks.md/progress.md 的那几行），
   或积攒到本轮最后一个 PR —— **ME4-6.1.4 最终台账收口 PR**（在发布与干净机验收之后）。它是停止条件里唯一允许在最后一刻才合并的 PR。
8. **全局验收前置**（Agent24）：
   ```
   cd rust && cargo fmt --all --check \
     && cargo +1.98.0 clippy --workspace --all-targets -- -D warnings \
     && cargo +1.98.0 test --workspace
   ```
   Sin90 / Cos72 各自的前置见各自 `docs/agent/tasks.md` 顶部。
9. **每刀落地后更新探针**：`docs/agent/me3-status.sh` 增加 `4x` 行（格式 `probe "<label>" <file> "<symbol ERE>"`），交付 = 那一行在 `origin/main` 上变 ●。
10. **不许出现比机制更强的措辞**：模块与 daemon 同 UID（SPEC-ME3 §0），所有「隔离 / 只能来自内核」都是 **broker API 之内**的性质，
    同 UID 的敌意本地进程不在本轮威胁模型内。

---

## 二、技术规范（设计文档的输入约束 —— 设计可以细化、可以更严，不可以违反）

### S1 调度回调 `_a24/scheduler/*`（ME4-M1）

**现状（2026-09-23 两次核过）**：
- `agent24-scheduler`：`Scheduler::{create,get,list,update,delete,run_now,tick}`；`RunTrigger::trigger(&ScheduleAction, schedule_id) -> Result<run_id,String>`（`lib.rs:51-55`），
  成功值被当作 `run_id` 写进 `ScheduleFiredPayload`（`lib.rs:285`）与 REST `run_now` 响应（`schedules.rs:108`）。
- 表 `schedules(id PK, name, enabled, spec, action, delivery, last_run_at, next_run_at, consecutive_failures)`（`agent24-store/migrations/0001_initial.sql:57-67`）；
  id = `sch_<ulid>`，**无 owner、无幂等键**；`upsert_schedule` 会覆盖 runtime 字段（`repo.rs:493-498`），tick 的 runtime 回写只有 `WHERE id=?`、无 CAS（`repo.rs:525-529`）；store 连接池最多 5 连接（`agent24-store/lib.rs:58`）。
- `fire` 先推进 `next_run_at`（pre-advance、skip-missed）再触发（`lib.rs:238,269`）；失败不重试当前这次，只等下个周期；连续 5 次失败写 `enabled=false`（`lib.rs:290-294`）。
  `run_now` 直接触发，不看 enabled、不动 `next_run_at`、不走失败计数（`lib.rs:189-190`）。
- `ScheduleCreate`/`ScheduleUpdate` 直接携带 `ScheduleAction`，REST handler 不做 owner/action 检查（`types.rs:701-728`、`schedules.rs:42,95`）。
- **启动顺序**：scheduler tick 循环先起（`server.rs:1023-1037`），`mount_all` 在后（`server.rs:1250`）。`mount_package` 拿不到 scheduler（`domain.rs:1275-1286`）。
- **内核 → 模块没有主动通道**；唯一 kernel→module 的路是代理的 UDS 上游。代理**请求侧**先按前缀剥掉客户端的全部 `X-A24-*` 再注入内核自己的头（`proxy.rs:1227-1234`），
  **响应侧**只剥不注入（`proxy.rs:210-216`）；上游 URI 取 `OriginalUri` 原样转发（`proxy.rs:1249-1255`）。
- **在途请求**只由 `Generation::admit_request` 登记进 `in_flight`（`drain.rs:416-433`）；`Running` 状态下未知/缺失的 request id 仍返回 `Ok(None)`（`drain.rs:498-501`），
  memory handler 拿到 `None` 照样执行（`memory_callback.rs:127-136`）—— **只写一个 `X-A24-Request-Id` 头不构成在途请求**，而且 Running 时测不出来，要到 Draining 才暴露。

**硬约束**：
1. **所有权与身份**：`schedules` 增 `owner_module TEXT NULL`、`module_key TEXT NULL`（CHECK 同空同非空）、部分唯一索引
   `UNIQUE(owner_module, module_key) WHERE owner_module IS NOT NULL`、`revision INTEGER NOT NULL DEFAULT 0`、`user_suspended INTEGER NOT NULL DEFAULT 0`、
   `system_disabled_reason TEXT NULL`。**owner 由内核按挂载身份注入**，params 里没有 owner 字段；模块只看得见、改得了自己的 key。
2. **动作不外露**：模块投递是**存储内部的动作种类**，不进用户可反序列化的 `ScheduleAction`。REST `POST /api/v1/schedules` 不可能创建它；
   对 `owner_module IS NOT NULL` 的行，REST 不得修改 `action` / `owner_module` / `module_key` / `spec`（只允许见下条的暂停/恢复与删除）。模块永远不能注册 `AgentRun`。
3. **触发接口**：`RunTrigger` 改为接收 `ScheduleInvocation { schedule_id, owner_module, module_key, scheduled_for, fired_at, fire_id }`，
   返回 `FireOutcome::{AgentRun{run_id}, ModuleDelivered{fire_id}, Deferred{reason}, Failed{reason}}`；模块投递有自己的事件（不冒充 `run_id`），
   REST `run_now` 响应对模块行返回 `fire_id`。
4. **幂等 upsert（线性化）**：`upsert{key, spec, enabled?, label?}` 在 `BEGIN IMMEDIATE` 内读—比较—写，确定 `outcome: created|updated|unchanged`，每次写 `revision += 1`；
   tick 的 runtime 回写对 `revision`（或预期的 `spec/next_run_at`）做 CAS，失败则放弃本次回写 —— **tick 永远不能用旧 spec 算出的 `next_run_at` 覆盖新 spec**。
   `delete{key}` 幂等（不存在返回 `{outcome: absent}`）。`list{}` 返回本模块的**完整期望状态**（key、spec、enabled、user_suspended、system_disabled_reason），供模块对账比较整个状态而不只是 key。
5. **三种「不启用」严格区分**：
   - `enabled=false`（模块自己要求暂停）：模块下次 upsert 可改。
   - `user_suspended=1`（用户经 REST 暂停）：**模块 upsert 永远不能清除**，只有用户恢复。
   - `system_disabled_reason`（内核因真实投递失败禁用）：模块的下一次 upsert（通常是重新挂载后的启动对账）清除它。
6. **不可用 ≠ 失败**：模块处于 `Starting / Draining / Revoked / Disabled / 未安装 / daemon 正在停机` 时到点，结果是 `Deferred`，**不增加 `consecutive_failures`**；
   只有模块 `Running` 且投递真的失败（非 2xx / 超时 / 连接错误）才计失败。**scheduler 循环在 `mount_all` 完成之后才启动**。
   模块 hot-disable / uninstall 时原子暂停（disable）或删除（uninstall）其全部 schedules 的去留由设计裁决并写判据，但不得让它们在模块不在时累计失败。
7. **投递（D1）**：投递器经 daemon 新增的只读 accessor 找到 owner 当前的 `Generation`，调用公开的 `admit_request`（`drain.rs:416-448`）拿到并持有 `InFlight`；
   **发送前必须调用 `InFlight::dispatch()` 并检查结果**（`drain.rs:937-951`，防止 revoke 之后仍把已判为 never-sent 的请求发出去），用 `InFlight.id()` 写 `X-A24-Request-Id`，
   发 `POST /api/v1/<ns>/_a24/scheduler/fired` 到 `InFlight.upstream()`（不走 loopback HTTP + bearer），响应结束后 `finish()`。内核写的头：
   `X-A24-Schedule-Key`、`X-A24-Fire-Id`、`X-A24-Request-Id`；body `{key, scheduled_for, fired_at}`。超时有界（设计定，建议 10s）。
8. **投递语义 = 至少一次，且扛得住 daemon 崩溃**：`fire_id` 由 `(schedule_id, scheduled_for)` **确定性派生**，模块按 `fire_id` 去重。
   今天 `fire` 是「先持久化 pre-advance、再触发」（`agent24-scheduler/src/lib.rs:238-279`）—— 两步之间崩溃这次到点就永久丢了。所以：
   **pre-advance 与写一行投递记录在同一事务里**：新表 `schedule_deliveries(fire_id PK, schedule_id, scheduled_for, attempts, status pending|delivered|deferred|failed, next_attempt_at, last_error)`；
   投递器从这张表取 `pending/deferred` 且到期的行去投；**daemon 重启后继续投递未完成的行，沿用同一 `fire_id`**。
   同一次到点的重试有界（设计定次数与退避上限，建议 3 次、总时长 ≤ 下一次到点）；用尽仍失败才标 `failed` 并计一次失败；`deferred` 的行在模块恢复 Running 后继续投（过期策略设计定）。`run_now` 的 `scheduled_for = now`、`fire_id` 由其派生，
   且 `run_now` 对模块行**走与 tick 相同的失败/延迟语义**（设计写明）。
9. **保留路径**：代理对**客户端**发往 `/api/v1/<ns>/_a24/` 的请求一律 404 不转发。判定前先按明确规则规范化（解 percent-encoding、拒绝编码斜杠与非法序列、
   合并 `//`、处理 `.`/`..`，大小写按设计定），不能只对原始字符串做一次 `starts_with`。它保证的是「**经内核 HTTP 代理进入的外部客户端无法伪造 fired**」——
   不是「fired 只可能来自内核」（同 UID 进程可以直连模块 socket，§0）。
10. **准入与限流**：`KERNEL_OOP_GRANTS` 加 `Scheduler`；`Offer.provides` 加 `_a24/scheduler/`；未声明 → `forbidden`。每模块配额（建议 256 行）→ `quota_exceeded`；
    每挂载一个令牌桶（跨重启不重置）→ `rate_limited`。`spec` 复用 `next_fire.rs` 的校验。key `[a-z0-9._-]{1,128}`。params `deny_unknown_fields`，`_meta` 不参与授权。
    handler 用 `admit_callback_bound` + `bind_to_lifecycle`（照 `memory_callback.rs`，不复制 FU-70 的缺陷）。错误 `kind` 闭集若扩展，同步改 `ErrorKind::ALL` 与 SPEC §3 表（`rpc.rs:1938` 钉住文本）。

### S2 推理回调 `_a24/model/*`（ME4-M4a）

**现状**：`agent24-models`：`ModelRouter::complete(TaskProfile{privacy,complexity}, req, cancel)` 返回 `(provider_name, resp)`（`router.rs:284-307`）——
**是 provider 名，不是实际 model id**；`CompletionRequest{messages,model,tools,response_format}` **没有 `max_tokens`**（`lib.rs:92-101`），provider 请求体也不发（`lib.rs:493`），
OpenAI 响应解析丢了 `model` 字段（`lib.rs:317`）；chat 超时 120s（`lib.rs:206`）。回调 RPC 外层无条件套连接级 30s 超时（`rpc.rs:82,1264-1268`）；
`bind_to_lifecycle(None, …)` 不提供任何取消（`drain.rs:779-782`）。用量是单个全局内存计数器（`routes.rs:27-52`），`cost_usd` 恒 0。

**硬约束**：
1. **D2 隐私**：模块调用**默认强制 `Privacy::LocalOnly`**，params 里**没有**能改 privacy 的字段；manifest 新增显式声明（建议 `model_access: local_only | remote_allowed`，缺省 local_only）
   才允许远端；即使 remote_allowed，请求只能表达 complexity 偏好，路由由内核决定。**负对照**：只配远端 provider + local_only 模块 → 错误，且远端桩**收不到任何请求**。
2. **先扩模型契约**：`CompletionRequest.max_tokens`、`CompletionResponse.model_id`（实际模型 id）、OpenAI wire 的转发与解析，作为独立 task 先落（ME4-4.2.2a）。
3. 方法最小集：`_a24/model/complete{messages, response_format?, max_tokens?, complexity?}` → `{text, model_id, usage:{prompt_tokens, completion_tokens}}`；不开放 tools。
4. **超时与取消 —— 设计必须选定一个方案并写死**，二选一：(a) 同步调用 + **按方法的超时元数据**（model 方法有自己的上限，连接级 30s 不变），
   并把 RPC cancel、连接关闭、所绑请求结束三者统一桥接到 provider 的 `CancellationToken`；(b) 作业化（`start` → `poll`/`cancel`）。不许靠调大全局 `CALL_TIMEOUT`。
   没有 `request_id` 的后台调用（如调度 fired 期间以外的定时任务）怎么绑定生命周期，设计写明。
5. **限流 + 计量**：每模块并发上限（建议 2）+ 令牌桶；**按模块持久化用量**（独立 task ME4-4.2.3）：重启不清零、失败调用是否计量、远端费用字段如何填，设计写明；`GET /api/v1/usage` 可按模块查询。
6. **错误**：`ModelError` 映射到不泄露 provider 细节的闭集 kind（需要 `unavailable` 就扩展闭集 + SPEC）。
7. **测试不依赖真实 oMLX**：黑盒用本地 OpenAI 兼容桩（Python `http.server`）当 `OMLX_URL`；python3 缺失即失败。另留一条 `#[ignore]` 的真 oMLX 冒烟。

### S3 `agent24-os-sdk`（ME4-M5，T13）

1. 位置：`rust/crates/agent24-os-sdk`。**SDK 不做任何 socket 字节 I/O 与帧解析**：连接、帧、握手、错误闭集全部经 `agent24-os-proto` 的公开类型。
   判据是**结构性**的：**SDK 从不自己打开或接管 socket**。proto 提供两个入口：`Client::connect_from_env()`（读 `A24_CALLBACK_SOCK`/`A24_HANDSHAKE_TOKEN`，完成握手，
   只暴露类型化的 `call/notify/cancel` 与 `Offer`，不暴露底层流）和 `listener_from_env()`（读 `A24_LISTEN_FD`，返回可直接交给 `axum::serve` 的监听器）。
   SDK crate 的 `clippy.toml`：`disallowed-types` 列出 `tokio::net::UnixStream`、`tokio::net::UnixListener`、`std::os::unix::net::UnixStream`、`std::os::unix::net::UnixListener`，
   `disallowed-methods` 列出 `std::os::fd::FromRawFd::from_raw_fd`；配合全局 `clippy -D warnings`，SDK 源码里只要写出这些类型或方法就过不了 CI
   （从 proto 拿到的监听器靠类型推断直接交给 axum，SDK 源码不需要写出它的类型名）。拿不到字节流，就不可能自己解析帧。
   正对照：往 SDK 放一个 `UnixStream::connect` 或 `from_raw_fd` → clippy 变红。若 proto 缺少 SDK 需要的公开 API，**在 proto 里补**，不在 SDK 里重写。
2. 形状**从两个真实调用方提取**：Sin90 adapter（事件/记忆/审批/调度/推理 + fired 路由，含 T3.2.0 的多路复用 transport）与 Agent24 黑盒 Python 模块。
   目标：写一个 OS 只需「给 manifest + 给一个 axum Router + 声明能力」，拿到类型化的 `Events / Memory / Approval / Scheduler / Model` 客户端与 fired 回调注册点。
3. 分发：外部仓库以 git 依赖 + tag（`agent24-os-sdk-v0.1.0`）引用；SemVer 从 0.1.0 起。
4. 验收：Sin90 迁到 SDK 后 adapter **净删代码**且真实挂载黑盒不变；Cos72 用 SDK 写成。

### S4 Cos72 最小样例（ME4-M5，T10，`MushroomDAO/Cos72`）

- Rust 进程外包，`impl_kind: out_of_process_provider`，用 SDK；**业务真相**只在 `~/.agent24/os/cos72/cos72.db`。
- mytask 最小闭环：`POST /tasks`（发布，带积分）→ `POST /tasks/{id}/claim` → `POST /tasks/{id}/submit`
  → **发积分经内核审批**（`_a24/approval/gate`，人批准后才入账）→ `GET /points`（积分账本只追加，余额 = 账本回放）。
  每个动作发事件；任务完成摘要（派生副本，非真相）写 `_a24/memory/private/remember`。
- `CosEntity` 与 `SpaceId` 不混用（roadmap M4 边界 1）；myshop/myvote **不做**。

### S5 wire 文档 + 非 Rust 参考实现（ME4-M5，T14）

- `docs/specs/WIRE-OOP-MODULE.md`：握手、帧、全部 `_a24/*` 方法（含 scheduler/model）、错误闭集、fired 投递语义（至少一次 + fire_id 去重）、保留路径、manifest 字段。
- 判据（PLAN T14 原文）：**「不看 SDK 源码能不能写出来」** —— 让一个**只拿到这份文档**的全新子代理写最小 Node.js 模块，
  通过 3f 风格黑盒（挂载 → 代理 → 事件 → 记忆 → 调度 upsert/fired）。子代理卡住的每个点 = 文档缺口，补文档不补代码。node 缺失即失败。

### S6 v0.5.0 发布物（ME4-M6）

- v0.5.0 = 「ME-3 完整 + 调度/推理回调 + SDK + Sin90/Cos72 独立可装」。
- **发布物清单**（由 ME4-6.0.1 冻结，精确到文件名）：Agent24 daemon/CLI 二进制（macOS arm64 至少）；Sin90 与 Cos72 各自仓库的 GitHub Release 里的**可安装包**
  （tar.gz：`domain-os.yml` + `bin/<name>`）+ `SHA256SUMS`。`agent24 os install` 接受的是本地目录（`agent24-cli/main.rs:479-499`），所以安装步骤 = 下载 → 校验 → 解压 → `os install <目录>`。
- 发布命令前断言：`HEAD == 已审批 SHA`、工作树干净、tag 不存在、发布物与清单逐项匹配。

---

## 三、任务定义（ID 带 `ME4-` 前缀，免得和 M1 的 `T1.x.y`、ME-3 的 `T1–T14` 撞名）

> 字段：目标 / 范围 / 不做 / 依赖 / 交付物 / 验收命令 / 风险。状态见 `tasks.md`。
> 所有 `cargo test <过滤>` 验收都隐含 §一 第 5 条的 `-- --list` 非空断言。

### ME4-M0 起跑前清账

**ME4-0.1 合并 Sin90 已批准的 #2/#3/#4**（Sin90 `T0.1`）
- 范围：逐个核 exact-head（§一 第 6 条）→ `gh pr merge --squash`；先合的若让后面冲突：rebase 推送 = 旧 approve 失效，等 clestons 复审，不自合。
- 验收：`gh pr list -R iDoris-ai/Sin90 --state open --json number` 不含 2/3/4；Sin90 main 全局前置全绿。

**ME4-0.2 合并本规划 PR**（Agent24 `docs/me4-plan-2026-09-23`、Sin90 `docs/pilot-me4-plan`）—— 验收：两个 PR 均 MERGED。

**ME4-0.3 Sin90 CI + 陈旧文档**（Sin90 `T0.2` / `T0.3`）

**ME4-0.4 Codex 补审 Sin90 历史改动**（Sin90 `T0.4`）—— `0d66f24`/`4032e82`/`8056ade`/`ab66b37`（actor-key 门禁优先）；真问题各开修复 PR，残余进 Sin90 followups。

> **需用户手动做（不是 goal task）**：给 `iDoris-ai/Sin90`（及后面的 `MushroomDAO/Cos72`）main 开 ruleset（1 个审批 + dismiss stale）。

### ME4-M1 调度回调（Agent24）—— 规范 S1

**ME4-1.1.1 设计冻结：`docs/design/ME4-S1-scheduler-callback.md` + SPEC-ME3 §3/§8/§9 改写**
- 范围：按 S1 写设计，裁决 S1 第 6 条（hot-disable/uninstall 时 schedules 的去留）、第 8 条（重试次数/退避）、第 9 条（规范化规则）；
  SPEC §3 offer set 加 Scheduler、§8 加 ME-4a 行、§9 删掉 Scheduler。
- 验收：冻结头显示评审方（Codex；额度耗尽期间为全新上下文的 Opus 对抗评审，见 §一 第 3 条）末轮 approve 且无 Critical/High；Rust 片段 `cargo check` 过（PR body 附命令）；
  `grep -nE '^\| *`?_a24/scheduler/' docs/specs/SPEC-ME3-OUT-OF-PROCESS.md` 命中 §3 方法表的行（不是全文任意出现）。

**ME4-1.2.1 存储层**：迁移（owner/key/revision/user_suspended/system_disabled_reason + CHECK + 部分唯一索引）；store 的
`upsert_module_schedule`（`BEGIN IMMEDIATE` 读比写、outcome）、`delete_module_schedule`、`list_module_schedules`、`count_module_schedules`、runtime 回写的 revision CAS。
- 另含：`schedule_deliveries` 表与「pre-advance + 写投递行」同事务的 store 函数（S1-8）。
- 验收：`cargo +1.98.0 test -p agent24-store module_schedule`：同 key 两次 upsert 一行（正对照：不同 key 两行）；**两个连接 + barrier 并发 upsert 同 key → 一行、一个 created 一个 updated/unchanged**；
  tick 回写与 upsert 交错 → `next_run_at` 对应新 spec；CHECK 拒绝半空行；旧库迁移后原有行不变 —— 旧态用**在 agent24-store 测试模块新增的同形 helper**（照 `agent24-memory/src/lib.rs:1097` 的 `pool_migrated_up_to`，经真实 migrator 截断到当前最新迁移）建。变异：删唯一索引 / 去掉 CAS → 变红。

**ME4-1.2.2 触发接口与 REST 护栏**：`ScheduleInvocation`、`FireOutcome`、模块投递专用事件；`run_now` 对模块行返回 `fire_id`；
REST create 拒绝模块动作、模块行禁止改 action/owner/key/spec；`user_suspended` 的暂停/恢复入口。
- 验收：`cargo +1.98.0 test -p agent24d schedules_rest_guard`：用户创建模块投递 → 4xx；用户把模块行改成 `AgentRun` → 4xx；用户暂停模块行后模块 upsert `enabled=true` → 仍暂停（正对照：用户恢复后生效）；
  既有 `AgentRun` 行的 REST 行为不变（回归）。

**ME4-1.3.1 fired 投递器**：经 `Generation` 准入持有 `InFlight`、发送前 `dispatch()`、写内核头、有界超时、确定性 `fire_id`、从 `schedule_deliveries` 取行投递与重启续投、同次到点有界重试、`Deferred` 不计失败、scheduler 循环挪到 `mount_all` 之后。
- 验收：`cargo +1.98.0 test -p agent24d scheduler_deliver`：mock 上游收到带 `X-A24-Fire-Id` 的 POST；**同一次到点的重试 fire_id 相同**（正对照：下一次到点不同）；
  模块 Starting/Draining/未安装时到点 → `Deferred` 且 `consecutive_failures` 不变；Running 且上游 500 ×(重试上限) → 失败 +1；超时不阻塞 tick 循环；
  **投递持有的 request id 在 Draining 期间仍能通过 `admit_callback_bound`，随机 id 不能，投递结束后同一 id 不能**；
  **`dispatch()` 失败（generation 已 revoke）→ 上游零请求**（变异：去掉 dispatch 检查 → 变红）；
  **崩溃恢复**：写好投递行后、投递前杀掉（测试钩子模拟）→ 重启后该行以**同一 fire_id** 被投递（正对照：已 delivered 的行重启后不重投）。

**ME4-1.3.2 代理保留路径**：规范化后判定，客户端发往 `/api/v1/<ns>/_a24/*` 一律 404 不转发。
- 验收：`cargo +1.98.0 test -p agent24-os-proto reserved_path`：变体矩阵（大小写、`//`、`.`/`..`、`%5f`/`%5F`、混合编码、编码斜杠、query/fragment、非法 `%` 序列）全部 404 且 mock 上游零请求；
  正对照 `/api/v1/<ns>/anything` 与 `/api/v1/<ns>/a24x` 照常转发。变异：去掉规范化只留 `starts_with` → 编码变体用例变红。

**ME4-1.4.1 回调 handler**：`_a24/scheduler/{upsert,delete,list}`、`Grants`/`Offer` 接线、配额、令牌桶、生命周期绑定。
- 验收：`cargo +1.98.0 test -p agent24d scheduler_callback`：未声明能力 → forbidden；A 删 B 的 key → `absent` 且 B 的行还在；第 257 个 key → quota_exceeded；
  非法 cron → invalid params；`_meta` 夹带 owner 无效果；`list` 返回完整期望状态（含 user_suspended / system_disabled_reason）。

**ME4-1.5.1 黑盒验收 + 探针**：`rust/apps/agent24d/tests/me4_scheduler_blackbox.rs`（harness 形状照 `me3f_blackbox.rs`，`start()` 增加 `A24_SCHEDULER_TICK_SECS=1`）。
- 场景：Python 模块启动即 upsert `routine.x`（`At` = 当前 +3s）两次 → `/api/v1/schedules` 该模块 1 行；**真实 tick 到点** → 模块收到 fired（探针文件记 fire_id 与 scheduled_for）；
  重启 daemon（模块启动再 upsert）→ 仍 1 行，且挂载期间的 tick 不累计失败；`run_now` → 另一个 fire_id（正对照）；客户端伪造 fired（含编码变体）→ 404、探针文件无新增；
  模块 fired handler 阻塞时 hot-disable → handler 里带投递 request id 的记忆回调成功。
- 验收：`cargo +1.98.0 test -p agent24d --test me4_scheduler_blackbox` 连跑 10 次全绿；`bash docs/agent/me3-status.sh` 新增 `4a 调度回调` 行为 ●。

### ME4-M2 Sin90 M3 Routine & Rhythm（Sin90，门 = Sin90 T3.x 全 DONE）

Sin90 `F3.1 Routine 实体` → `F3.2 内核能力适配（T3.2.0 多路复用 transport，之后分叉：T3.2.1 类型化客户端 → T3.2.3 真实挂载 Offer 验收；T3.2.2 fired 路由只依赖 T3.1.1 + T3.2.0）` → `F3.3 outbox 对账（比较完整期望状态）` → `F3.4 Rhythm 开放` → `F3.5 真实挂载验收`。
- **门验收**（Sin90 `T3.5.1`）：真实 agent24d 下建「每周 3 次运动」→ 内核 1 行；重启 daemon → 仍 1 行；正对照：对账器对同一 routine 连发两次 upsert → 仍 1 行；
  用 cron 到**下一分钟**的测试 Routine 经**真实 tick** 到点 → Sin90 记 1 条 `routine.fired`（Sin90 的 Routine 只有 cron，不为测试扩张数据模型；`At` 只在 Agent24 自己的黑盒里用）；用户在内核侧暂停 → Sin90 对账不会把它重新打开。
- 依赖：ME4-1.5.1 已合并（Sin90 挂载测试的 `AGENT24_CHECKOUT` 必须指向**含调度回调的 Agent24 main**，跑之前 `git -C ../Agent24 fetch && merge --ff-only origin/main`）。

### ME4-M3 Sin90 M4 Review & Markdown（Sin90）

Sin90 `F4.1 Review 路由` → `F4.2 body_ref Markdown 外置` → `F4.3 周复盘草稿（纯事件回放）+ review Routine 自动草稿` → `F4.4 定稿摘要写进内核私有记忆`。
- **门验收**（Sin90 `T4.3.1` + `T4.4.1`）：周复盘草稿的小时数与事件回放逐位相等；负对照：绕过事件改表行 → 草稿不变；定稿摘要能从内核 recall 找回。

### ME4-M4a 推理回调（Agent24）—— 规范 S2

**ME4-4.1.1 设计冻结：`docs/design/ME4-S2-model-callback.md` + SPEC 改写 + manifest 字段**（可与 M2/M3 并行）—— 冻结标准同 ME4-1.1.1；明确回答 S2 第 1/4/5 条。

**ME4-4.2.1 manifest 字段 + 授权**：`model_access` 解析（缺省 local_only）、`KERNEL_OOP_GRANTS` 加 `Models`、`provides` 加 `_a24/model/`。依赖 4.1.1、**ME4-1.5.1**（会改 `mount_package` 接线，排在调度回调之后）。
- 验收：`cargo +1.98.0 test -p agent24-domain model_access`（缺省、非法值拒绝）+ `-p agent24d model_grant`：未声明 Models → forbidden。

**ME4-4.2.2a 模型契约扩展**（`agent24-models`）：`max_tokens`、`model_id`、wire 转发与解析、上下限。
- 验收：`cargo +1.98.0 test -p agent24-models max_tokens` 与 `cargo +1.98.0 test -p agent24-models model_id`（两条命令，各自 `-- --list` 非空）：桩服务器收到 `max_tokens`；响应的实际 model id 被返回（正对照：不是 provider 名）。可与 M1 并行（只动 `agent24-models`）。

**ME4-4.2.2b `_a24/model/complete` handler**：LocalOnly 强制、设计选定的超时/取消方案、并发上限、错误映射。依赖 4.2.1、4.2.2a、ME4-1.5.1。
- 验收：`cargo +1.98.0 test -p agent24d model_callback`：local_only 模块 + 只有远端 provider → 错误且远端桩零请求（负对照）；remote_allowed → 可达远端（正对照）；
  所绑请求结束 / RPC cancel / 连接关闭 → 桩服务器观察到连接被取消；第 3 个并发 → busy/rate_limited；超过 30s 但未超方法上限的调用成功（若选方案 a）。

**ME4-4.2.3 按模块用量**：持久化计量 schema + `GET /api/v1/usage?module=` 。依赖 4.2.2b。
- 验收：`cargo +1.98.0 test -p agent24d usage_by_module`：两个模块各调一次 → 各自计数；重启后计数仍在；失败调用按设计口径计。

**ME4-4.3.1 黑盒验收 + 探针**：`me4_model_blackbox.rs`（本地 OpenAI 兼容桩当 `OMLX_URL`，python3 缺失即失败）；另留 `#[ignore]` 真 oMLX 冒烟。
- 验收：`cargo +1.98.0 test -p agent24d --test me4_model_blackbox` 连跑 10 次全绿；探针 `4b 推理回调` 为 ●。

### ME4-M4b Sin90 M5 AI v1（Sin90，门 = Sin90 T5.x 全 DONE）

Sin90 `F5.0 设计补丁（新 Op 先进 DESIGN §2，送对抗评审）` → `F5.1 引擎梯 + sin90_ai_calls` → `F5.2 classify` → `F5.3 summarize` → `F5.4 propose` → `F5.5 验收`。
- **门验收**（Sin90 `T5.5.1`）：AI 产出的变更全部在 `sin90_proposals` 里有 `source ∈ {local_brain, executive}` 的行；AI 模块对 store 写接口的引用为 0（结构测试，带正对照）；
  远端桩不可达时 classify 仍可用（走本地桩）。
- 依赖：ME4-4.3.1、**ME4-M3 门**。

### ME4-M5 SDK → Sin90 迁移 → Cos72 → wire 文档

**ME4-5.1.1 SDK 设计冻结：`docs/design/ME4-S3-os-sdk.md`** —— 从 Sin90 adapter（T3.2.0 transport + 五种客户端 + fired）与黑盒 Python 模块逐行对照提取 API（规范 S3）。依赖：ME4-M4b 门。

**ME4-5.1.2a SDK transport + 握手**（只经 proto）+ S3 第 1 条的结构测试。
**ME4-5.1.2b 五种类型化客户端**（Events/Memory/Approval/Scheduler/Model）+ 单测。
**ME4-5.1.2c fired 注册点 + `examples/minimal.rs` + 挂载冒烟 + tag `agent24-os-sdk-v0.1.0` + 探针 `4c SDK`**。
- 验收：`cargo +1.98.0 test -p agent24-os-sdk`；S3 第 1 条的 clippy `disallowed-types` 判据（含正对照）；`examples/minimal` 挂载冒烟通过；`git ls-remote --tags origin agent24-os-sdk-v0.1.0` 非空。

**ME4-5.2.1 Sin90 迁到 SDK**（Sin90 `TS.1.1`）—— `src/adapter_agent24/` 净减；Sin90 真实挂载黑盒（M3/M4/M5 全部判据）不变全绿。

**ME4-5.3.x Cos72（T10，`MushroomDAO/Cos72`）**
- ME4-5.3.1 Cos72 仓库 `.pilot.yml` + `docs/agent` 七件套（按 S4 拆 task，门镜像到本表）。
- ME4-5.3.2 骨架：manifest + SDK 挂载 + SQLite 迁移 + 事件。
- ME4-5.3.3a mytask 实体与路由（发布/认领/提交）。
- ME4-5.3.3b 审批发积分 + 积分账本回放 + 完成摘要进内核记忆。
- ME4-5.3.4 真实挂载黑盒：安装 → 挂载 → 全流程 → 审批往返 → 与 Sin90 同时挂载时互相读不到对方的记忆与 schedules。
- 验收：Cos72 `cargo test` + `cargo test --test agent24_mount_blackbox -- --ignored` 全绿。
- 风险：PR-Daemon 是否覆盖 `MushroomDAO` 组织未核实 —— 第一个 PR 开出后 60 分钟无裁决即按 §四 停机规则上报，**不自合**。

**ME4-5.4.1 wire 文档 + Node.js 参考模块（T14）**（规范 S5）
- 验收：`rust/apps/agent24d/tests/me4_node_module_blackbox.rs` 通过（node 缺失即失败）；PR body 附「只给文档的子代理」写出的模块原稿、它卡住的点与对应的文档修补。

### ME4-M6 发布 v0.5.0（T12）—— 规范 S6

**ME4-6.0.1 冻结 v0.5.0 专用发布清单** `docs/RELEASE-CHECKLIST-v0.5.0.md`（发布物逐项文件名、构建命令、校验、断言、回滚），送对抗评审（Codex，额度耗尽期间为 Opus 子代理）到 approve。
**ME4-6.0.2 Sin90 / Cos72 发布物**：各自仓库的构建脚本产出可安装 tar.gz + `SHA256SUMS`，发各自的 GitHub Release（tag `v0.5.0` 或清单裁定的版本）。
**ME4-6.1.1 发布前收口**：ADR-029 §3 修订记录；`CHANGELOG.md`；版本号；`SIN90-PET0-INTEGRATION.md` 与新能力对齐；`me3-status.sh` 全部 ●；回填到此为止的台账。
**ME4-6.1.2 发布 Agent24 v0.5.0**：按 ME4-6.0.1 的清单执行（断言 `HEAD == 已审批 SHA`、干净、tag 不存在）。
**ME4-6.1.3 干净机器验收**：Mac mini（`ssh jason@100.107.243.106`）上**只用已发布资产**：装 daemon → 下载校验解压 Sin90、Cos72 → `agent24 os install <目录>` ×2 → `agent24 os list` 两者 `mounted`。
- 验收：清单里的每条命令输出记录进 PR/Release notes；`gh release view v0.5.0 -R iDoris-ai/Agent24 --json assets` 的资产名与清单逐项相等。
**ME4-6.1.4 最终台账收口 PR**（依赖 6.1.3）：三个仓库的 tasks.md/progress.md 回填 6.1.2/6.1.3 及此前未回填的所有 `DONE` 与证据；这是本轮最后一个 PR，停止条件在它合并后才成立。

---

## 四、无人值守的停机规则

- **阻塞发布的只有**：P0/P1 级正确性或安全问题（评审给出的 Critical/High，或变异/黑盒暴露的真实缺陷）。其余 followup 在 ME-4 启动时的账本基础上**冻结截止**：
  本轮新登记的 Medium/Low 自动归入下一轮（在 followups 里标 `ME4→next` 并写一句理由），不阻塞停止条件。
- **需要用户拍板**（产品方向 / 验收口径 / 架构取舍 / 花钱 / 计划外的对外发布）→ 相关 task 标 `BLOCKED` 记进 progress.md；继续做不受影响的 task；
  **当所有 READY task 都做完、剩下的全被 BLOCKED 挡住时，立即停下来带着问题清单问用户**，不空转。
- 评审 60 分钟无裁决：记 progress.md，回主循环；同一 PR 连续 3 次超时 → 视为评审服务不覆盖该仓库，标 `BLOCKED` 问用户。

---

## 五、v1 → v2 改动记录（Codex 对抗评审第 1 轮：REQUEST_CHANGES，21 条全部采纳）

| # | 级别 | 问题（已对照代码核实） | 处理 |
|---|---|---|---|
| 1 | Critical | 只写 `X-A24-Request-Id` 不构成在途请求（`drain.rs:416-433,498-501`），原黑盒在 Running 下假阳性 | S1-7 改为经 `Generation` 准入持有 `InFlight`；ME4-1.3.1/1.5.1 加 Draining 期间的正负对照 |
| 2 | Critical | scheduler 先于 `mount_all` 启动，挂载慢会累计失败直至永久禁用 | S1-6「不可用≠失败」+ scheduler 挪到 `mount_all` 后；S1-5 区分三种不启用 |
| 3 | Critical | `RELEASE-CHECKLIST.md` 仍硬编码 v0.1.0；`os install` 只收本地目录，没有模块发布物 | 新增 S6 + ME4-6.0.1/6.0.2/6.1.3 |
| 4 | High | `RunTrigger` 拿不到 owner/key/scheduled_for，成功值被当 `run_id` | S1-3 `ScheduleInvocation` + `FireOutcome`，ME4-1.2.2 |
| 5 | High | 公开 schedules API 可创建模块动作、把模块行改成 `AgentRun` | S1-2 动作不外露 + REST 护栏，ME4-1.2.2 负对照 |
| 6 | High | 唯一索引解决不了并发 upsert 的 outcome 与 tick/upsert 竞态 | S1-4 `BEGIN IMMEDIATE` + revision CAS，ME4-1.2.1 双连接 barrier 测试 |
| 7 | High | 「fired 只能来自内核」超出威胁模型；保留路径未覆盖编码绕过 | S1-9 措辞收窄 + 规范化规则 + 变体矩阵 |
| 8 | High | 黑盒只用 `run_now`，没验证真实到点 | ME4-1.5.1 / Sin90 T3.5.1 改为真实 tick（`At` 近时刻 + 1s tick） |
| 9 | High | fire_id 每次新生成、失败不重试，语义未闭合 | S1-8 至少一次 + 确定性 fire_id + 有界重试 |
| 10 | High | Sin90 对账只比 key，修不了禁用/漂移 | S1-4 `list` 返回完整期望状态；Sin90 T3.3.2 比较完整状态 |
| 11 | High | 模型契约没有 `max_tokens`、返回的是 provider 名 | S2-2 + ME4-4.2.2a |
| 12 | High | 超时/取消/用量/配额未定义 | S2-4 必须二选一写死；ME4-4.2.3 独立的按模块用量 |
| 13 | High | Sin90 callback client 串行化全部调用、重连丢 Offer、scheduler-only 连接被丢 | Sin90 新增 T3.2.0 多路复用 transport |
| 14 | High | 依赖表两处错误（M4b 漏依赖 M3 门；T3.2.1 验收依赖未声明） | 台账修正；Sin90 T3.2.1 拆出 T3.2.3 |
| 15 | High | 多个 task 明显超 300 行 | 拆 ME4-1.2.x、4.2.2a/b、4.2.3、5.1.2a/b/c、5.3.3a/b；Sin90 T3.2.x |
| 16 | High | 无保护分支直接合并；「approve 后不推」与「状态即时写回」矛盾 | §一 第 6 条 exact-head 判据；第 7 条 closure ledger |
| 17 | Medium | 过滤测试可零匹配通过；node 可 skip；SDK grep 可绕过 | §一 第 5 条 `--list` 非空；工具缺失即失败；S3-1 结构性判据 |
| 18 | Medium | 「模块数据不进内核记忆表」与 Sin90/Cos72 写 private memory 冲突 | architecture.md 改为「业务真相只在模块 SQLite，可写派生摘要到自己的私有分区」 |
| 19 | Medium | `pool_migrated_up_to` 不存在于 agent24-store | ME4-1.2.1 改为在 store 测试模块新增同形 helper |
| 20 | Medium | 停止条件不可判定、可能无限扩张 | 新增 §四 停机规则 |
| 21 | Low | `X-A24-*` 请求侧注入/响应侧纯剥离未区分 | S1 现状改写 |

**v2 → v3（Codex 第 2 轮：16 FIXED / 5 PARTIAL，新增 5 High / 3 Medium / 1 Low，全部采纳）**

| # | 级别 | 问题 | 处理 |
|---|---|---|---|
| R1-1 | Critical 残留 | 没要求发送前 `InFlight::dispatch()` | S1-7 写死 + ME4-1.3.1 变异判据 |
| R1-9 | High 残留 | pre-advance 与投递之间崩溃会丢这次到点，「至少一次」不成立 | S1-8 `schedule_deliveries` 同事务 + 重启续投同一 fire_id；ME4-1.2.1/1.3.1 判据 |
| R1-13 | High 残留 | 取消方法名写成 `$/cancel`，实际是 `$/cancelRequest`（`rpc.rs:94-95`） | Sin90 T3.2.0 改正并加精确 wire 测试 |
| R1-16 | High 残留 | 台账收口 PR 排在发布与干净机验收之前 | 新增 ME4-6.1.4 最终台账收口 PR |
| R1-17 | Medium 残留 | SDK 判据仍是 token grep | S3-1 改为「SDK 从不持有 socket」+ clippy `disallowed-types` |
| R1-18 | Medium 残留 | Sin90 architecture 仍写「不进内核记忆表」 | Sin90 architecture 改正 |
| N1 | High | `$/cancel` | 同 R1-13 |
| N2 | High | ME4-4.2.1 会改 `mount_package` 却不依赖 1.5.1 | 台账与 §三 加依赖；4.2.2a 保持可并行 |
| N3 | High | Sin90 TS.1.1 依赖已拆分不存在的 `ME4-5.1.2` | 改为 `ME4-5.1.2c` |
| N4 | High | spec.md「不做配额」与 ME-4 的调度/模型配额矛盾 | spec.md 收窄为「不做 F7 通用配额」 |
| N5 | High | Sin90 outbox 要标失败但 schema 只有 pending/done | Sin90 T3.3.1 先改 DESIGN §2 + 迁移加 `failed` 与失败字段、定义永久/可重试错误 |
| M1 | Medium | Sin90 验收要求 `At` 但 Routine 只有 cron | 改为下一分钟 cron |
| M2 | Medium | T3.2.2 依赖与依赖图不一致 | 明确从 T3.2.0 分叉 |
| M3 | Medium | 验收命令给 cargo 两个裸过滤词 | 拆成两条命令 |
| L1 | Low | Sin90 architecture 编号乱序 | 重新编号 |

**v3 → v3.1（第 3 轮：Codex 核到一半额度耗尽，余下由本地模型按 Tier 2 完成）**

Codex 在额度耗尽前已确认 v3 的文本修订落到了规范与台账；它正在核的三点由本地模型收尾：

| # | 问题 | 处理 |
|---|---|---|
| R3-1 | SDK 判据写成「禁掉 A、B、C 之外的一切」，语义反了；且模块必须把 `A24_LISTEN_FD` 变成监听器，SDK 需要一个合法入口 | S3-1 改为 proto 提供 `connect_from_env` / `listener_from_env`，clippy 列出被禁的具体类型与 `from_raw_fd` |
| R3-2 | `schedule_deliveries` 与「失败不重试」的说法是否冲突 | 不冲突：「失败不重试」在 S1「现状」段，描述的是今天的代码；新行为以 S1-8 为准 |
| R3-3 | GOAL 的子代理分工（每个子代理一个 worktree）与 pilot「一个 Feature 一个 worktree」冲突 | GOAL【分工】改为：统筹者建 Feature worktree，子代理在里面的指定分支上工作 |
| R3-4 | Codex 额度到 2026-09-29 才恢复 | §一 第 3 条：改由全新上下文的 Opus 子代理做对抗评审，并记 `ME4-CODEX-DEBT` 待补审 |

---

## 六、不在本轮

- Sin90 M6 Life Packs、Projection/可视化（Sin90 M7+）。
- Cos72 的 myshop / myvote / 渠道接入（roadmap M5）。
- `_a24/memory/scoped/*`、`Policy` 能力、模块签名（ME-6）、进程隔离（SPEC-ME3 §0）。
- roadmap M1（记忆成为产品）—— v0.5.0 发布后再捡。
- FU-69/FU-70 等既有 followups 不在主线里做，但 ME4 的新 handler 不得复制 FU-70 的缺陷。
