# S2-6 进程组停止语义盘点（2026-11-10）

## 结论

S2-6 原计划把 Rust 各处「TERM → 等待 → KILL → 回收」统一成一个 `kill_group_gracefully`。2026-11-10 按 jason 拍板 (a) **降级为本盘点，不做统一实现**：

- 各调用点的差异不在顺序，而在顺序之外：是否先固定 PID、KILL 前是否重验身份（start marker / 归属）、是否收养后代、超时是否随 shutdown deadline 动态缩短、析构里能否 await。这些规则各不相同，并且都有测试守着。
- Codex 试做的首版（本地存档，未合入）只能把顺序抽成一个带回调的骨架，实际信号与回收仍留在各调用点，代码净增约 530 行，测试也没有覆盖产品路径。收益小于风险，所以没有采用。
- 以后要改其中任何一处的停止语义，先对照本表，并保持该调用点已有测试原样通过。

Q13 = (b) 不变：comm 按次 CLI 超时直接 SIGKILL 整个进程组，不先发 TERM。

## Rust 调用点

用于发现调用点的检索：

```sh
grep -rn 'killpg\|kill(-\|SIGTERM\|SIGKILL\|Signal::SIGTERM\|setpgid\|process_group' rust
```

探测用的信号 0、`process_group(0)`（创建隔离组）、测试夹具和注释不算停止调用，不列入下表。

下表只收**进程组**停止。对单个子进程的 `child.kill()`（例如 `agent24-cli/src/main.rs:859`、`:1035` 停掉自己拉起的 agent24d，`agent24-components/src/install.rs:470`、`:477`，`agent24-hw/src/probe.rs:263`、`:270`，`agent24-idoris/src/keys.rs:401`，`agent24-sidecar-host/src/posix.rs:174`）不在本盘点范围内，上面的检索也找不到它们。

| 调用点（main `bed5ef4c` 行号） | 停止序列 | 超时 |
|---|---|---|
| `rust/crates/agent24-os-proto/src/supervise.rs:632` terminate | TERM；peek leader 不回收；即使 leader 退出也 KILL；wait leader；确认整组 | 入参 grace（库默认 3s，可由 owner 覆盖）；KILL 后 leader 5s；回收后组确认另 5s |
| `rust/crates/agent24-comm/src/daemon.rs:295` owned daemon | TERM 失败即结束；等 leader；无条件 KILL；可选 child.wait；确认组空 | 入参 grace，50ms 轮询并动态缩短到 shutdown_deadline；wait 500ms；组确认另 500ms |
| `rust/crates/agent24-comm/src/daemon.rs:451` orphan | TERM；等组空；仍存在则重验 start marker；匹配才 KILL；确认组空 | 入参 grace；KILL 后 500ms |
| `rust/crates/agent24-comm/src/runner.rs:271` CLI timeout | 直接 KILL 组；有界 wait；零窗口不确认 | window，生产默认 2s |
| `rust/crates/agent24-idoris/src/supervisor.rs:697` stop | 记录成员；TERM；wait leader + 收养 + 等组；不成功才验证归属并 KILL；wait、收养、确认组 | timings.terminate（默认 17s），leader/后代共用一个 deadline；KILL 后 child.wait 无超时，组确认 5s |
| `rust/crates/agent24-idoris/src/supervisor/identity.rs:315` orphan | TERM；等退出；必要时身份校验后 KILL；再等；最后等组空 | terminate（默认 17s）；KILL 后 5s；最后组空另 5s |
| `rust/crates/agent24-comm/src/runner.rs:290` background reaper | 重复直接 KILL + wait，直到 wait future 完成（包括原来的 wait 返回 Err 情况） | 每轮 1s，无总上限 |
| `rust/crates/agent24-comm/src/runner.rs:339` best_effort | 单次 KILL；无等待 | 无 |
| `rust/crates/agent24-idoris/src/setup.rs:271` setup 失败/取消 | KILL 组；child.kill().await；child.wait().await | 无回收超时 |
| `rust/crates/agent24-idoris/src/supervisor.rs:730` finish | 已回收 leader 后，验证后代归属，KILL；确认组空；归属未知则失败 | 组确认 5s |
| `rust/crates/agent24-idoris/src/supervisor.rs:837` Drop | best effort 有归属才 KILL | 不等回收 |
| `rust/crates/agent24-os-proto/src/supervise.rs:775` Drop | 撤销 permit；尚未回收的 leader 固定 PID；KILL 并记录结果 | 不等回收 |
| `rust/apps/agent24-sidecar-host/src/posix.rs:234,242,304,479,488` | terminate 发 TERM；独立 force 请求发 KILL；独立 reap；Drop（:479）在 leader 尚未回收时先组 SIGKILL（:488，不等待），再转交 child、group 和 permit | host 默认 graceful 5s、force 10s；posix leader 退出 5s、reap 5s、组空 5s |
| `rust/apps/agent24-cli/src/service.rs:380` | launchctl kill SIGTERM 服务任务 | 不在此等待/KILL |
| `rust/apps/agent24d/tests/me3f_blackbox.rs:230` 等测试清理 | 对 daemon 单 PID TERM，等待后 child.kill | 测试自有窗口 |

`supervise.rs` 的 signal / leader_exited / group_empties_within、idoris 的 signal_group / killpg_outcome / group_exists、sidecar-host 的 signal / reap_step / signal_accepted 是上表各调用点的信号、身份和观测适配层。

## 桌面（TS）侧

本节只覆盖 S2-6 原计划要统一的模块（backend-manager、creative-serve-web / Open Design），不是桌面全部的进程停止点。

| 现状 | 行为 |
|---|---|
| `apps/desktop/src/main/backend-manager.ts:1016` killChild | 单个 child SIGTERM；立即清空引用，无 KILL、无等待 |
| `apps/desktop/src/main/creative-serve-web.ts:724` terminateOwnedChild | checkout child TERM，exit 事件完成；5s 后 child SIGKILL 并完成，不等强杀确认 |
| creative-serve-web packaged 路径 → SidecarManager / `open-design-sidecar.ts:177` | 当前源码是 launcher 单 PID TERM（避免连同服务同时 TERM），15s 后组 KILL，并确认组空；这与“TERM 整组”的计划简称有差异。另有启动失败清理（`open-design-sidecar.ts:354`）：写 PGID 文件失败时立即组 SIGKILL，不先 TERM、不等待、不确认组空 |

若以后统一，必须显式区分“只 TERM 不等待”、checkout 的 5s、packaged 的 15s 及 PID/PGID 目标。

计划范围之外、盘点时顺带看到的两处：

- `apps/desktop/src/main/ipc/index.ts:625` OmlxStop：先对自有 child 发 SIGTERM，再在非 Windows 上 `pkill -f 'omlx serve'`。后者也会匹配用户自己启动的 omlx，无等待、无 KILL 升级、无退出确认。**列为待办**：停止时只应针对本应用启动的进程。
- `apps/desktop/src/main/cli-tool.ts:56` CLI 版本探测：`execFile` 3 秒超时，超时直接对单个子进程 SIGKILL，不涉及进程组。
