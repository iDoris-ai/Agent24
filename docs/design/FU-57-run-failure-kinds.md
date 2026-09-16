# FU-57 —— 一次运行为什么失败：五个类别

> 状态：设计稿 v3（v1 → v2：Codex 设计审查 2 High / 3 Medium / 2 Low 全部采纳；v2 → v3：第 2 轮 2 Medium 采纳，见文末）。来源：`followups.md` FU-57；ME3-NEXT 执行队列第三段。
> 不改状态机的**迁移**，不改 `RestartPolicy` 的输入、失败计数、配置的退避时长；给 `Starting` / `Backoff` / `GaveUp` 加一个载荷：**上一次失败属于哪一类、原话是什么**。代价如实写：判为 `io` 的那些失败，清理前多等最多 `exit_settle`（默认 100ms），所以下一次启动、`Backoff` 的发布、熔断时报的 `within` 墙钟都可能晚这么多。

## 问题

今天 Supervisor 把「绑端口失败 / 监听回调失败 / spawn 被拒」都记成 `Stopped::Exited`，把握手的一切失败都记成 `StartupTimeout`。`RestartPolicy` 不看原因，日志里有原话，所以只影响**看的人**：`agent24 os list` 上 `degraded · gave up after 5 failed runs within 60s` 的模块，是包坏了、被内核拒了、还是自己一启动就崩，今天分不出来。

## 五个类别（`FailureKind`）与穷举映射

类别说的是**「Supervisor 看到哪一步先失败」**，不是根因诊断；原话（`detail`）总是跟着。错误在分类之前保持**有类型**（今天握手那段 `map_err(|e| e.to_string())` 把类型抹掉了，要改）。

**分类按阶段，不按错误类型**：同一个 `EndpointError::Io` 出自 `listen_next`（准备回调 socket）是 `setup`，出自 `accept_one`（等模块连上来）是 `io`。所以不写一个 `EndpointError → FailureKind`，而是每个阶段一个穷举的分类函数：`listen`（`PathTooLong` / `UnsafeDirectory` / `Io` / 其余 → `setup`）、`accept`（`Timeout` → `timeout`，`ForeignPeer` → `refused`，`Io` → `io`，其余变体 `accept_one` 不会返回，按 `io` 记并注明）、`handshake`、`launch`、`serve`（`rpc::Ended`）、`exit`（`io::Result<Exit>`）。`AlreadyCreated` 只出自 `CallbackDir::create`，在逐次运行之前，不是运行失败。

| 类别 | 含义 | 来源（穷举） | 看的人该做什么 |
|---|---|---|---|
| `setup` | 进程还没出生：内核这一侧的准备失败 | 绑模块端口；`listen_next`；`LaunchError` 全部变体 | 修包或修环境；模块代码还没跑过 |
| `refused` | 模块和内核在协议上对不上 | `EndpointError::ForeignPeer`；`HandshakeFailed::Refused(_)` 全部（`Parse` / `NotInitialize` / `BadParams` / `AuthFailed` / `ManifestMismatch` / `VersionMismatch`）；握手首帧 `FrameError::TooLong`；运行中 `rpc::Ended::TooLong` | 对 manifest、版本、协议实现；不是崩溃 |
| `timeout` | 启动时限内没完成握手 | `EndpointError::Timeout`；`HandshakeFailed::Timeout` | 模块启动太慢、卡住，或根本没去连回调 |
| `io` | 回调通道断了或坏了 | `accept_one` 的其余 `EndpointError`；握手 `FrameError::Eof` / `FrameError::Io` / `HandshakeFailed::Write`；运行中 `rpc::Ended::PeerClosed` / `ReadFailed` / `WriteFailed`；`process.exited()` 返回 `Err`（观察失败：不知道它退没退） | 模块关了回调却没退出，或 socket 出错；看模块日志 |
| `exited` | 进程自己退出了 | **仅** `process.exited()` 返回 `Ok(exit)`（握手前、运行中、或下面的等待窗口里看到）；`detail` 带退出码或信号 | 模块崩溃或主动退出；看模块日志与退出码 |

**不在五类之内**（不产生 `Backoff`，不计入失败）：请求的停止（spawn 前、spawn 后复查、握手中、`Admitted::Stopping`、等待窗口里、`finish` 里看到的停止，全部照旧走 `StopRequested`）；`Unconfirmed` → `StopFailed`；panic / 取消 → `Panicked` / `Killed`；`Admitted::Revoked` 与运行中的 `rpc::Ended::Stopped`（只有撤销能触发，而撤销者已经拿走了唯一的 kill 许可，后面的 `finish` 会 `Unconfirmed` → `StopFailed`，所以正常到不了 `Backoff`；万一 `finish` 成功，仍记 `io`，`detail` 写明「不应发生」）。

## 唯一的判定难点：进程退出与回调断开同时发生

模块崩溃时内核先关它的 fd（回调 EOF），退出状态稍后才可见。于是「连上后就崩」的模块多半先被看到 EOF——报 `io`，和真实原因相反，且随调度抖动。

规则：**判为 `io` 的候选，先撤销、后等待**——等待放进 `finish` 里，在撤销之后、SIGTERM 之前：

1. `ended_at` 在发现失败的那一刻同步取（与今天一样，早于任何等待；`RestartPolicy::ran` 不受影响）。
2. `finish` 照旧**先** `begin_stop()`（撤销：新请求一律被拒）、**再**发布 `Stopping`。这两步不变，所以等待期间代理不会再放请求进来（v1 的 H1：等待若放在 `finish` 之前，这 100ms 里一个回调已断的代还在收请求）。
3. 仅当调用方标了「`io` 候选」时，接着 biased 地赛三路：**停止请求** > **`process.exited()`** > **`exit_settle` 计时器**。停止先到 → 直接往下走停止，本次运行照旧按「`finish` 里看到停止」成为 `StopRequested`；进程先退 → 分类改为 `exited`，`detail` 写「回调先断，随后进程 exited with code N」；计时器先到 → 保持 `io`。
4. 然后照旧 `stop_then(grace)`。

`exit_settle` 进 `Timings`（默认 100ms），测试可以用确定的数值。`process.exited()` 可重复 await、可取消（缓存首次看到的退出），在窗口里再 await 一次是安全的。

这是缓解不是保证：EOF 之后 `exit_settle` 以上才可见退出的进程仍报 `io`。不用停止记录的 `leader = gone_before_term` 来判：那条记录按停机写一次，普通失败的运行不写它（写了会污染之后真正的停机记录），而且它也只说明「在第一次 TERM 之前可见」，推不出因果。

## 在哪儿看得到

- `Status::Starting { attempt, after: Option<RunFailure> }`（`after` = 这次启动之前那次失败；第一次为 `None`）、`Status::Backoff { failures, delay, last: RunFailure }`、`Status::GaveUp { failures, within, last: RunFailure }`。`Stopping` 不带：那时这次运行的分类还没定，带上一次的会误导。载荷在状态里（同一次 `send_replace`），读者不会看到「状态是这次的、原因是上次的」。
- `RunFailure { kind: FailureKind, detail: String }`，`detail` 按字符边界截到 512 字节（握手诊断可能来自一个最长 1 MiB 的帧，这个值会被 watch 克隆、进 API 应答）。
- `agent24 os list` / `GET /api/v1/os` 的 `detail`：
  `restarting in 800ms after 2 failed run(s) — last: setup (could not start the module: …)`、
  `starting (run 3) — after: exited (exited with code 3)`、
  `gave up after 5 failed runs within 60s — last: refused (…manifest…)`。
  **不加新的 API 字段**：结构化错误码与解决办法属于 ERR-1，那时统一设计。
- 日志：`module run ended; restarting` 那一行加 `kind=`。`finish` 变成 `Unconfirmed` 时，原本那次运行的分类与原话也一起记进那条 error 日志（今天运行中的回调原因只在 `finish` 成功后才打）。
- `Stopped`（`supervise.rs`）是 `RestartPolicy::failed` 的旧入参，策略不看它；映射 `timeout → StartupTimeout`、其余 → `Exited`，并在它的文档里写明它是「粗粒度的旧信号」，`Exited` 不再意味着进程真的退出了。

## 判据（每条带正对照）

1. 纯映射测试：每个阶段分类函数的每个变体（`LaunchError`、`listen` / `accept` 两处的 `EndpointError`、`HandshakeFailed` 及其内的 `FrameError` / `HandshakeError`、`rpc::Ended`、`Result<Exit, io::Error>`）各一格，断言类别；`match` 不写通配，新增变体编译不过。
2. spawn 被拒（命令指向包里不存在的文件）→ `Backoff.last.kind == setup`；**对照**：命令改成立即 `exit 3` → `exited`，`detail` 含 `code 3`。
3. 握手给错令牌 → `refused`；**对照**：令牌正确 → `Running`。
4. 起来但从不连回调 → `timeout`；**对照**：同一模块正常握手 → `Running`。
5. **闸门夹具**：握手成功后关回调，然后等测试放行（`<data>/release` 出现）才 `exit 3`。`exit_settle = 10s`：测试看到 `Stopping` 之后才放行 → `exited`（`detail` 含 `code 3`）；`exit_settle = 0`：不放行 → `io`。两个结果都不依赖调度（进程在放行之前一定没退）；「去掉等待窗口」的变异让前者变成 `io`。
6. 等待窗口里发停止 → `handle.stop()` 按时返回、最终 `Stopped`、停止记录照常写成一次停止；「没有发布 `Backoff`」用日志断言（`module run ended; restarting` 与 `Backoff` 在同一处发出、一一对应；watch 会合并值，看不见一闪而过的状态）。
7. 等待期间代已撤销：窗口未到时断言 `generation` 已是 `Revoked`（H1 的回归测试）。
8. `Starting.after` 在第二次启动时是第一次的失败；第一次为 `None`。

## v1 → v2（设计审查采纳）

- H1：等待放进 `finish`，撤销与 `Stopping` 在前。
- H2：来源表补齐（`Refused` 全部变体、`TooLong` 归 `refused`、`exited()` 的 `Err` 归 `io`、`Revoked` / `Ended::Stopped` 移出五类）；分类前保留类型。
- M1：`exit_settle` 进 `Timings`，变异判据改为确定性；补纯映射测试、停止竞争测试、`timeout` 的正对照。
- M2：代价如实写（下一次启动与 `within` 可能晚 `exit_settle`）。
- M3：`Starting` 也带上一次失败；`Stopping` 故意不带；`Unconfirmed` 时原分类进日志。
- L1：不用停止记录判因果（写在正文）。
- L2：`Stopped` 标为旧信号；`detail` 截断 512 字节。

## v2 → v3（第 2 轮设计审查采纳）

- M1：分类按阶段（`listen` / `accept` / …），同一个 `EndpointError::Io` 在两处归不同类。
- M2：判据 5 改为闸门夹具，零窗口与长窗口两个结果都不依赖调度；判据 6 的「没发布 `Backoff`」改用日志断言。
- 第 2 轮确认：等待放在 `finish` 里、撤销与 `Stopping` 之后是对的；窗口里到来的停止由现有 `gone` 闭包原子地写成停止事实；`Unconfirmed` 覆盖暂定的分类；没有任何 `Stopping` 的消费者假设 SIGTERM 已发。
