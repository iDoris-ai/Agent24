# SHUT —— 停机可观测、可调（语义说明 v5）

> 来源：用户 2026-09-13 裁决「停机时长保留默认（排空 0.8s + 停止宽限 0.5s，总 2s），但必须有日志、有跟踪、可调；参数有问题要能被发现、能被调整」。
> v1 经 Codex 设计审查（13 条，3 Critical）后重写：结局改为正交事实、标记带实例 ID、汇总同步持久化、截止时刻显式建模、非法参数改为告警 + 默认、新增实时出口。
> v2 经第二轮设计审查（1 Critical、4 High）后改为 v3：结局模型做成全函数且终态只写一次、标记在就绪之前写（与就绪/停机的原子判定同序）、默认值下才承诺 2 秒、坏文件不掩盖崩溃证据、`leader` 三态、`daemon` 进保留段、拆成三刀。
> v3 经第三轮设计审查（2 Critical）后改为 v4：**标记改为拿到单例锁后立刻写、由唯一的 RAII 守卫负责删除**（停机任务创建之前标记就已存在，竞态从结构上消失）；**进程组的结局与 Supervisor 的结局拆成两个事实**，前者在进程组确认为空的那一刻作为一个事务一次写全；`leader` 允许未知；账本用稳定的共享句柄；launchd 透传挪进 1b；三刀是叠加顺序。
> v4 经第四轮设计审查：两条 Critical 已关闭；v5 收掉其 2 High + 7 Medium（`process = none` 不判超时、HTTP 截止单列、看门狗取两者较晚、`StopFailed` 先于 Drop 写、`kill_attempted` 只在真的发了信号时写、守卫同步切换、`previous` 表拆开、`began` 由所有取消路径保证）。审查结论：修完即可实现。
> 分三刀实现（叠加：1a → 1b → 1c，各自在前一刀合并后才开）：**SHUT-1a**（os-proto：停止事实 + 当场告警 + `stop_record()` 旁路）→ **SHUT-1b**（daemon：参数、截止模型、汇总持久化、跨启动证据）→ **SHUT-1c**（实时出口：端点、`daemon status`、OpenAPI、保留段）。

## 状态 / States

**一次模块停止的记录 `StopRecord`** —— 不用一个重载的结局枚举，而是几条互相独立的事实，每条都有明确来源：

| 字段 | 取值 | 来源 |
|---|---|---|
| `reason` | `shutdown` \| `disable` | 发起方 |
| `process` | `none`（停止请求到达时尚未 spawn、在退避、已熔断）\| `running`（有进程组） | Supervisor 当时的运行态 |
| `drain.ended_by` | `idle` \| `deadline` \| `process_exited` \| `callback_ended` \| `skipped`（排空时长为 0 或这一代不在 Running）\| `cut`（排空途中被停机截断）\| 缺省（排空还没结束就中止了） | `drain_run` 的 select 分支 |
| `drain.elapsed_ms` | 排空实际用时 | 同上 |
| `abandoned` / `never_sent` | 撤销时已发出结局未知的请求数 / 未发出的请求数；**只在 `group = gone` 时有值**（与 `group` 同一个事务写入），否则为「未知」 | `Revocation`（已有；在 `gone()` 之前就已取得，作为参数交给 `gone`） |
| `leader` | `gone_before_term`（停止开始时主进程已经退出/崩溃）\| `exited_in_grace`（SIGTERM 后宽限内自己退出）\| `killed_after_grace`（**宽限用完仍在，且已对它发出 SIGKILL —— 「宽限可能不够」的信号**）\| 缺省（未知：`terminate` 在查状态、发信号、等待、回收的任一步出错）；仅 `process = running` 时有意义 | `terminate`：`exited_already` 与 `leader_exits_within(grace)` 分开记；`killed_after_grace` 只在宽限到期且 SIGKILL 调用已发出之后才成立 |
| `stop_elapsed_ms` | SIGTERM → 进程组确认为空 | `terminate` 计时 |
| `group` | `gone`（进程组确认为空）\| `failed`（`StopFailed`：SIGKILL 之后仍未能确认为空）\| `kill_attempted`（进程随 Supervisor 被 Drop，且 Drop **确实发出了** SIGKILL 调用；未确认）\| `kill_unavailable`（Drop 时无法安全判断进程组归属、没有发信号，或信号调用返回错误）\| 缺省（还没结束）；仅 `process = running` 时有意义 | **进程组的结局**。`gone` 在 `gone()` 回调那一刻写，**与 `leader`、`stop_elapsed_ms`、`abandoned`、`never_sent` 在同一把锁里一次写全**；`failed` 由 `finish` 在拿到 `StopFailed` 时、**在它装着的进程被 Drop 之前**写（否则 Drop 会先写成 `kill_attempted`）；`kill_attempted` / `kill_unavailable` 由进程的 Drop 按它实际走的分支写。**只写一次**（先写者赢）：`gone` 之后 Supervisor 再被中止或 panic，改变的是下一行，不是这一行 |
| `supervisor` | `stopped`（循环自己返回了 —— 包括停止失败 `SupervisorError::StopFailed` 的情形，那由 `group = failed` 表达）\| `cut_off`（**只由** `drain_and_stop_unless` 的放弃分支写：停机截断了热 disable）\| `killed`（循环被取消）\| `panicked` \| 缺省（还在跑） | **Supervisor 的结局**，与 `group` 正交；由停止的发起方拿到 `drain_and_stop[_unless]` 的结果后写 |

说明：`terminate` 在主进程退出后**总会**再对整组发一次 SIGKILL（清理忽略 TERM 的助手）—— 那是常规清理，不算「强杀」。「强杀」只看 `leader = killed_after_grace`。`timed_out` **不是**记录里的值：它是汇总在模块截止时刻读到「`group` 仍缺省」时给出的判断，所以记录与汇总不会争着写同一个字段。

**一次 daemon 停机的汇总 `ShutdownSummary`**：`instance_id`、生效参数、每条记录（在模块截止时刻拍快照），以及 `stop_result` —— 对快照的**全函数**，按下面顺序取第一个成立的：
1. `timed_out`：有记录 `process = running` 且 `group` 仍缺省，或 `process = none` 且 `supervisor` 仍缺省；
2. `degraded`：有记录 `group ∈ {failed, kill_attempted, kill_unavailable}`，或 `supervisor ∈ {cut_off, killed, panicked}`；
3. `clean`：其余（每条记录 `group = gone` 或 `process = none`，且 `supervisor` 为 `stopped` 或缺省）。

「宽限用完」「请求被切断」不改变 `stop_result`（停机本身完成了），它们由记录里的 `leader` 与 `abandoned/never_sent` 表达并单独告警。**汇总写不下来**（`write_failed`）只能出现在日志里；**看门狗强退 / 崩溃**写不了任何东西，只能由下次启动推断为 `unconfirmed`。

**跨启动证据**（都在 `<状态目录>/run/`，状态目录 = `$HOME/.agent24`，所有提示都写明是哪个状态目录）：
- `daemon.alive`：`{instance_id, pid, started_at}`。
- `last-shutdown.json`：最近一次**已持久化**的汇总。
- 启动时据此得出 `previous`（**标记优先**：只要标记在，就不会因为汇总读不出来而丢掉崩溃证据）：

| `daemon.alive` | `last-shutdown.json` | `previous` |
|---|---|---|
| 无 | 无 | `no_history` |
| 无 | 可读 | `clean`（附上汇总自己的 `stop_result`） |
| 无 | 读不出 | `unreadable`（warn，历史丢失，但没有崩溃证据） |
| 有且可读 | 可读且 `instance_id` 相同 | `cleanup_failed`（停机完成，只是删标记失败） |
| 有且可读 | 无、读不出、或 `instance_id` 不同 | `unconfirmed`（看门狗强退、崩溃、SIGKILL、断电：**无法确认干净结束**） |
| 有但读不出 | 任意 | `unconfirmed`（id 无从比对） |

  pid 只作诊断信息，不用来判断。

## 转移 / Transitions

1. **启动**：
   1. 最先读参数（见「对外语义」），非法值告警并回落默认，记进本次的 `config_warnings`。
   2. 拿单例锁；读两个文件得出 `previous`；非 `clean` / `no_history` 时启动日志 warn（写明状态目录与含义）。
   3. **紧接着**写 `daemon.alive`（新的随机 `instance_id`；「写临时文件 → fsync → rename → fsync 目录」），得到**唯一的** `MarkerGuard`。此时信号处理已注册、停机任务还没创建 —— 停机路径看到标记时它一定已经存在，不存在「先删后写」。**rename 已成功而之后的目录 fsync 报错**：照样返回一个认得这个 `instance_id` 的守卫，外加一条「标记的持久性不确定」告警（与 SPEC-ME3 对配置写入「已发布但持久性不确定」的处理同一套路）；rename 之前就失败才算「没有标记」。
   4. `MarkerGuard` 的删除规则只有三条：
      - **启动阶段**被 Drop（开库、绑端口等启动失败的处理路径）→ 删标记：那是启动失败，不是没停完；
      - 交给停机任务**之前**（在 `tokio::spawn` 调用之前、同步地）就把守卫切到「停机阶段」：此后 Drop **不删**（一个还没被 poll 过就随 unwind 丢掉的任务，或运行时在持久化途中取消它，都不会冒充干净结束）；
      - 停机阶段只有持久化作业**成功回执**才删（且只删 `instance_id` 相同的那个标记）。
   5. 标记写失败：warn，并在本次的 `config_warnings` 里记「本次运行没有崩溃证据」，照常启动；这次运行的汇总照写（删标记按 id 进行，没写成的标记不会被谁删）。
   6. **承诺的边界**：在拿到单例锁、写出标记**之前**就收到的停机（启动最早期），看门狗可能在标记出现之前结束进程——那个 daemon 还没启动任何东西，不留证据。
2. **每次模块停止结束**：Supervisor 在内部逐项填 `StopRecord`（排空结束填 drain 字段，确认进程组为空时在同一把锁里一次写全 `group`、`leader`、`stop_elapsed_ms`、`abandoned`、`never_sent`），**当场**：`leader = killed_after_grace` → warn；`drain.ended_by = deadline` 且 `abandoned + never_sent > 0` → warn；`group = failed` 或 `supervisor = panicked` → error。
3. **热 disable 的停止**进入一本「停止账本」：发起停止**之前**就建好条目（稳定 id、开始时刻、`Arc<Mutex<StopRecord>>` 共享句柄，即 `stop_record()`）。记录的终态字段先写、账本的「已完成」后写。`close()` 时**一次性固定**纳入本次停机的条目集合：已标「已完成」的只是历史（完成时已记日志）；其余纳入（其中「进程组已 gone、任务还在收尾」与「还在停」是两种不同状态，都照实记）。模块截止时刻对每条记录**在它自己的锁里**深拷贝快照。`cut_off` 只能由放弃分支写，`Status::Killed` 本身不推出 `cut_off`。
4. **停机**：`began` 由 `request()` 记下（不再用截止时刻倒推）；`request()` 幂等。**任何**观察到取消的路径（包括原始 `CancellationToken` 被外部取消后才醒来的停机任务）在读截止时刻之前都先调一次 `request()`，所以 `began` 总是由第一个到达者确定，且只确定一次。
   1. 模块：`close()` → 在 **模块截止时刻** 前等所有停止记录完成；到时 `group` 仍缺省的，汇总判 `timed_out`（记录本身不写）。
   2. 汇总：同一任务里构建，交给阻塞线程池做**一个作业**：「写临时文件 → fsync → rename → fsync 目录 → 删 `daemon.alive` → fsync 目录」；在 **持久化截止时刻** 前等它回执。任何一步失败即停止后续步骤（所以标记只在汇总已落盘后才会被删）。
   3. 超时或失败 → error 日志；标记还在 → 下次启动得出 `unconfirmed` 或 `cleanup_failed`。
   4. `serve` 返回前等这个任务（已有 `stopping.await`，其上界是持久化截止时刻）；运行时的 `shutdown_timeout(300ms)` 只在 `serve` 返回之后才开始，所以切不到持久化作业 —— 能切到它的只有看门狗，而看门狗在持久化截止之后 300ms。即便被切，守卫已在停机阶段，标记留着 → 下次启动 `unconfirmed`，如实。
5. **看门狗**：不变 —— 无 I/O、绝对时刻、`exit(0)`。它触发意味着上面某步卡住，证据留给下次启动。

## 不变式 / Invariants

- **截止时刻一律由 `began` 加参数算出，存在 `Shutdown` 里**，不从别的截止时刻倒推：
  `HTTP 截止 = began + 1500ms`（固定）；`模块截止 = began + drain + grace + CONFIRM(200ms)`；`持久化截止 = 模块截止 + PERSIST(200ms)`；`看门狗 = max(HTTP 截止, 持久化截止) + WATCHDOG_MARGIN(300ms)`。报告的「退出上限」就是看门狗时刻减 `began`。
  默认值下：HTTP 截止 1.5s、模块截止 1.5s、持久化截止 1.7s、看门狗 2.0s —— **`kill -TERM` 到退出仍 ≤ 2s（TASKS B2），与今天一致**；参数调到最小（排空 0、宽限 100ms）时，看门狗仍按 HTTP 截止算，不会比 HTTP 的 1.5s 更早。
- **2 秒是默认值下的承诺**：调大参数是运维者显式放宽这个上界。上限收紧为排空 ≤ 10000ms、宽限 ≤ 5000ms（最坏看门狗 ≈ 15.7s）；启动日志与实时出口写明本次的退出上限；TASKS B2 的措辞同步改成「默认参数下 ≤ 2s，调参后为 X」（SHUT-1b 一并改）。
- **HTTP 的优雅窗口固定 1.5s，不随模块参数变**：调大模块参数只推迟模块截止、持久化截止与看门狗，启动日志写明新的退出上限（「现在是 X 秒，默认 2 秒」）。
- `terminate` 内部的回收等待（`REAP_TIMEOUT` 5s）会被模块截止切断：这种情况记 `timed_out`，**不**冒充 `failed`。
- `daemon.alive` 只在汇总成功持久化之后才删；二者带同一个 `instance_id`。所以 `unconfirmed` 的准确含义是「**无法确认**有与那个实例匹配的已持久化汇总」（标记读不出时，id 无从比对，也归这里）。
- 每条 `leader = killed_after_grace` 都能对上一条 warn 和汇总里的一行（测试钉住）。
- ephemeral daemon **不创建、不读、不删、不覆盖**这两个文件（哨兵测试：预置一个持久标记，ephemeral 跑完它原样还在）。
- 不改 `SupervisorHandle::drain_and_stop[_unless]` 的签名：记录走旁路（`stop_record()` 在发起停止前取得一个共享句柄）。`process` 由 Supervisor 在 spawn 成功、拿到 `ModuleProcess` 时自己记下，不从有损的 `Status` watch 推断。
- **规范文档同步**（SHUT-1b）：TASKS B2，以及 SPEC-ME3「有界停机」段落里「共享的 1.5s 模块/HTTP 预算、0.5s 看门狗余量」的写法，连同 server.rs 的注释与测试一起改成新的截止模型。

## 对外语义 / External semantics

- **参数**（沿用 `A24_*` 惯例）：`A24_MODULE_DRAIN_MS`（0–10000，默认 800）、`A24_MODULE_STOP_GRACE_MS`（100–5000，默认 500）。
  **非法值：warn + 用默认值**，并进入实时出口的 `config_warnings`（写明原值、合法范围、生效值）—— 不拒绝启动：CLI 的 `daemon start` 会吞掉 daemon 的 stderr，launchd 下拒绝启动会变成节流的崩溃循环，一个 24/7 的 daemon 不该因为调参打错字而下线。
  两个变量加入 `PASSTHROUGH_VARS`（**在 SHUT-1b**：CLI 有一条扫描 daemon 源码的测试，daemon 读了而透传清单没有的环境变量会让它失败）；文档写明 launchd 服务改了它们要重装服务（`agent24 service install`）才生效。
- **日志**（tracing，模块名为字段）：
  - `module "x" did not exit within its 500ms stop grace; SIGKILLed after 512ms — raise A24_MODULE_STOP_GRACE_MS if it needs longer to flush`
  - `module "x": the 800ms drain ended with 2 request(s) still in flight (1 sent, outcome unknown; 1 never sent) — raise A24_MODULE_DRAIN_MS if requests routinely run longer`
  - 汇总：`shutdown (state dir ~/.agent24) took 1120ms: x completed (drain idle 3ms, exited in grace, stop 40ms); y completed (drain deadline 800ms, cut 2, grace expired, stop 512ms)`
- **`last-shutdown.json`**：`{version, instance_id, began_at_ms, took_ms, stop_result, params:{drain_ms, stop_grace_ms, http_deadline_ms, module_deadline_ms, persist_deadline_ms, watchdog_ms}, records:[ModuleStop…]}`（`began_at_ms` 是 Unix 纪元以来的墙钟毫秒；各截止为停机开始后的毫秒数），字段只增不删；形状由测试钉住。只从 1 MiB 以内的普通文件读取（不跟随链接）。
- **实时出口**（SHUT-1c）：`GET /api/v1/shutdown`（要 token；与已有的 `POST /api/v1/shutdown` 同一路径——GET 看停机配置与上次结果、POST 触发停机。实现时从原设想的 `/api/v1/daemon/shutdown` 改到这里：`shutdown` 本就是内核保留段，不必再占一个 `daemon` 段、平白收走一个模块名）→ `ShutdownReport {ephemeral, evidence_dir, drain_ms, stop_grace_ms, exit_bound_ms, config_warnings, previous, previous_detail, last_shutdown{stop_result, began_at_ms, took_ms, killed_after_grace, cut_requests, omitted_records}}`；`agent24 daemon status` 多打一段「停机」：生效参数、配置告警、上次停机的结论与被强杀/切断的模块。写进 `protocol/openapi.yaml`。
  **不动** `DomainOsView.detail`（它说的是模块此刻的状态，不塞历史）和 `DomainOsList` 的形状。

## 失败与并发 / Failure and concurrency

- 停机与热 disable 并发：见转移 3 的账本快照；一个停止只会进一次汇总。
- 停止中途进程自己崩了：`drain.ended_by = process_exited`，残留请求照实计入 `abandoned`，不算「排空超时」。
- 停止请求到达时还在 `Starting`：若子进程已存在 → `process = running`，照常 SIGTERM/宽限；若尚未 spawn → `process = none`，只有 `supervisor`。
- 进程组还没 gone 时 Supervisor panic / 被截断 / 被取消 → `group = kill_attempted`（进程 Drop），`supervisor` 分别为 `panicked` / `cut_off` / `killed`（在排空中被截断时 `drain.ended_by = cut`）；`abandoned/never_sent` 未知。
- 进程组**已经 gone**之后 Supervisor 才被中止或 panic（例如在之后的输出排空 `DRAIN_WAIT` 里）→ `group = gone` 不变（进程确实都没了），`supervisor` 如实记 `killed` / `panicked` / `cut_off`，`stop_result` 因此是 `degraded` —— 两个事实各说各的，不矛盾。
- 模块截止时刻与某条记录的终态同时到来：记录的终态是在锁里一次写全、只写一次的，汇总在锁里拷贝快照；快照早于写入就判 `timed_out`，晚于就用写入的值 —— 不会拷到半个终态。
- 汇总写到一半：临时文件 + rename，半截内容不会被当成合法记录；读到坏文件 → 按上表（有标记则 `unconfirmed`，无标记则 `unreadable`），不影响启动。
- 单例锁只按状态目录互斥：不同 `HOME` 的两个 daemon 各有各的历史，提示里都写明状态目录。

## 非目标 / Non-goals

- 不做按模块单独的宽限（manifest 字段）—— 等汇总数据说明谁需要再加。
- 不改「0.5s / 0.8s / 2s」这组默认值本身。
- 不做 Prometheus 等指标系统；日志 + 汇总文件 + 实时出口足以「发现并调整」。
- 看门狗不写任何东西；不试图区分「看门狗强退」与「崩溃」（二者都记 `unconfirmed`）。
- 本轮不做 `agent24 doctor` 命令；实时出口挂在 `daemon status` 上。
