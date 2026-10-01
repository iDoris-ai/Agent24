# Agent24 × Hyphae CLI 联调记录 · 第二轮（续）

> 范围：在 COMM-4a（#626）+ COMM-3（#627）集成分支 `local/comm-round2` 上 cherry-pick
> COMM-1b 的后续修复 #628（`A24_COMM_PASSWORD_STORE` 内存口令存储开关 + `comm_mount_smoke`
> 进程泄漏修复），解决三处冲突，然后用这个开关解除 `docs/comm/JOINT-ROUND2.md`（第一版，
> `local/comm-round2-pre`@`c1e6ed3`）记录的阻断（F1：生产路径必然写入真实登录钥匙串），
> 跑通真实 `agent24`/`agent24d` 黑盒的第二轮联调：zero-run、收发与断线重试、daemon 托管。

**结论先说**：冲突已解决，`cargo fmt --check` / `cargo clippy -D warnings` / `cargo test
--workspace` / `HYPHAE_TEST_BIN=... cargo test -p agent24-comm` 全绿。用
`A24_COMM_PASSWORD_STORE=memory` 绕过了 F1，**没有写入任何真实钥匙串**（用
`security find-generic-password -s ai.idoris.agent24.comm` 核实过，找不到该条目）。
第二轮联调的 zero-run、收发重试、daemon 托管三组断言**全部按预期通过**，详见 §4-§6。
唯一记为「发现」的是 `agent24 daemon stop` 对一个已成功停止的 daemon 仍返回非零退出码
的不一致（F2，非阻断，细节见 §7）。

## 0. 基线

| 项 | 值 |
|---|---|
| #626（COMM-4a）PR head | `5531abe0f78b077512161c3800c9398116d6081a` |
| #627（COMM-3）PR head | `95013516393ce1dd69006ae07e8b34a8d91f0543` |
| #628（password store 开关）PR head（cherry-pick 前） | `b45ba08a0214b3d5d05966323dc83565f7071f43` |
| main（merge-base，= #622 收口） | `f1dbe1efe01766a31e4cff3768c7375c5ab0f2ae` |
| 集成分支 `local/comm-round2` HEAD（cherry-pick #628 之后） | `ea76f9fef569f21f533f7b9e865d30a17157353b`（worktree `/Users/jason/Dev/auraai/Agent24-round2`，本地分支，未 push） |
| Hyphae source SHA | `a4aa606eb81d5c040d94c51cdf94553e646d8674`（与第一轮一致，`hyphae.lock.json` 的 `darwin-arm64` 条目比对一致） |
| `hyphae-repro`（= 真 Hyphae CLI 二进制）sha256 | `f53c29b31d8ca5eb0124ced246bcff6610f048f18bc8dcc2de27f685dad8b221`（与第一轮一致） |
| `hyphae-relay` sha256 | `a012d86e549cbeb564d5a5932c54f9b3511c2434203846537096420c89f36aef`（与第一轮一致） |
| `agent24`（dev profile，集成分支构建）sha256 | `7c09d244c6a41b33c6733c8542152e5e9c2931b224c3dfa7e6506be27d0ddada` |
| `agent24d`（dev profile，集成分支构建）sha256 | `e9c88b89b320033f61384fa81404fbedf1589136c1baf6af9b59cc795a83d6cf` |
| 平台 | `darwin-arm64`（本机 Darwin 25.4.0 arm64） |

## 1. Cherry-pick 冲突与解法

`git cherry-pick b45ba08` 到 `local/comm-round2`（已 reset 到 `feat/comm-3-send-history-outbox`，
即 main + #626 + #627）在 `rust/apps/agent24d/src/comm_routes.rs` 产生三处冲突。
`docs/design/COMM-HYPHAE.md`、`rust/apps/agent24-cli/src/service.rs`、
`rust/apps/agent24d/tests/comm_mount_smoke.rs` 三个文件无冲突，直接按 #628 的改动应用。

冲突性质：COMM-4a（HEAD 一侧）给 `build_ready_state`/`build` 加了 daemon 监管相关的参数
（`pid_path`、`log_path`，返回值带上 `Arc<HyphaeDaemonSupervisor>`）；#628（cherry-pick
一侧）给同一对函数加了可切换的 `password_store` 参数（取代硬编码的
`KeyringPasswordStore`）。两者各自独立地改了同一个函数签名和同一段函数体，**不是互斥关
系，而是需要把两种新增参数都接进同一个签名**。解法：

1. **`build_ready_state` 签名**：保留 COMM-4a 的 `pid_path: &Path, log_path: &Path`，
   再加上 #628 的 `password_store: Arc<dyn PasswordStore>` 作为新参数；返回值保留 COMM-4a
   的 `Result<(CommState, Arc<HyphaeDaemonSupervisor>), String>`（#628 原本是
   `Result<CommState, String>`，服从 COMM-4a 的形状）。
2. **函数体**：删除 #628 里硬编码 `let password_store: Arc<dyn PasswordStore> =
   Arc::new(KeyringPasswordStore::new())` 那几行（connect-the-dots 注释一起删），改为直接使用
   传进来的参数；COMM-4a 的孤儿清理 (`reap_orphan`)、`DaemonCtx` 构造、
   `CommState::ready(...).with_daemon(...)` 原样保留，`password_store` 全部换成形参。
3. **`build` 函数**：顶层控制流以 #628 的"先 `select_password_store()`，拿到 Err 就整体
   `unconfigured`"为主干，套住 COMM-4a 原来的"`resolve_source_path()` 为 `None` 就
   `unconfigured`，否则 `build_ready_state(...)` 失败就 `binary_rejected`"分支；三条路径
   统一返回 COMM-4a 的 `(CommState, Option<Arc<HyphaeDaemonSupervisor>>)` 元组形状——
   `select_password_store` 失败和 `resolve_source_path` 返回 `None` 两条路径的 daemon 都
   是 `None`，只有 `build_ready_state` 成功才是 `Some(daemon)`。

验证：合并后的代码没有再留下任何 `KeyringPasswordStore` 硬编码构造点（唯一构造点在
`select_password_store` 的 `Keyring` 分支里），`comm_routes.rs` 单测
（`password_store_choice_tests`）、`comm_mount_smoke.rs` 的三个黑盒测试
（含 #628 新增的 `memory_password_store_warns_and_still_mounts_comm`、
`invalid_password_store_value_reports_a_configuration_error`）全部保留且通过。

**给以后的提醒**：#626 和 #628 都还没合进 main（截至本轮联调，均为 OPEN 状态）。无论谁
先合，后合的那一个在 rebase/merge 到 main 时会在 `comm_routes.rs` 的
`build_ready_state`/`build` 这两个函数上遇到同样形态的冲突——一个加 daemon 监管参数，一
个加 password store 参数，两者都要保留，解法同上三点。

## 2. 验证套件（集成分支，cherry-pick 完成之后）

全部在 `rust/` 下执行：

| 检查 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo test --workspace` | exit 0，无 FAILED（覆盖整个 workspace 全部 crate） |
| `HYPHAE_TEST_BIN=<hyphae-repro> cargo test -p agent24-comm` | exit 0，无 FAILED（含
`real_hyphae_binary_envelopes_round_trip`、`router_lifecycle`、
`kill_minus_9_backs_off_then_trips_the_breaker_at_five` 等对真二进制/真监管逻辑的测试） |

跑完确认无残留 `agent24d` 进程（`pgrep -fl agent24d` 无命中）。

## 3. 第二轮联调：环境与准备

- 用集成分支 `cargo build -p agent24-cli -p agent24d`（dev profile）构建出
  `rust/target/debug/agent24`、`rust/target/debug/agent24d`（sha256 见 §0）。
- A 侧：`HOME=/tmp/a24r2-a`，`A24_HYPHAE_BIN=<hyphae-repro>`，
  `A24_COMM_PASSWORD_STORE=memory`。
- 本地 relay：`hyphae-relay -listen 127.0.0.1 -port 51146 -data-dir <scratchpad 临时目录>`。
- B 侧：直接调 `hyphae-repro`，`HOME=/tmp/a24r2-b`。
- 口令全部用脚本生成的合成随机值，只写入 scratchpad 下的临时文件，不写入本文档、不写入
  任何提交内容。
- A 侧全程只通过 `agent24 comm ...` CLI 或 agent24d 自己的 REST（`/api/v1/runs` 用于
  zero-run 计数）操作，没有绕过 CLI/REST 直接动 Hyphae 文件或数据库。

**对"agent24 daemon start"的一处必要替代**：`agent24 daemon start` 内部对非
`--ephemeral` 的 agent24d 子进程硬编码 `stderr(Stdio::null())`
（`rust/apps/agent24-cli/src/main.rs::spawn_daemon`），而 agent24d 的全部
`tracing`（含 `A24_COMM_PASSWORD_STORE=memory` 的 warn）只写 stderr
（`apps/agent24d/src/main.rs::main` 里 `.with_writer(std::io::stderr)`）。这意味着通过
`agent24 daemon start` 启动时，这条 warn 会被直接丢弃，任务要求的"先确认日志里出现了
memory 的 warn，没出现就立刻停"在这条路径上无法核实。因此 A 侧改为直接运行同一个
`agent24d serve --port 0` 二进制（参数、环境变量完全相同），把 stdout/stderr 分别重定
向到可读的日志文件——agent24d 自己会写 discovery 状态文件（`$HOME/.agent24/daemon.json`），
之后的 `agent24 comm ...` 调用照常通过这个状态文件发现并连接到它，不受影响。仅略过了
`agent24 daemon start` 这一层便利包装，没有绕过任何 comm 操作本身。

**验证顺序（按任务要求，先验证 memory warn 再继续）**：

```
nohup env HOME=/tmp/a24r2-a A24_HYPHAE_BIN=<hyphae-repro> A24_COMM_PASSWORD_STORE=memory \
  target/debug/agent24d serve --port 0 > a-stdout.log 2> a-stderr.log &
```

stderr 立即出现：

```
WARN agent24d::comm_routes: comm: A24_COMM_PASSWORD_STORE=memory — the Hyphae keystore
password is kept in memory only and will be lost on every daemon restart; this is for
tests or ad-hoc joint debugging ONLY, never for production (COMM-HYPHAE.md §6.4)
```

确认后才继续执行任何 `comm identity create` 等操作。事后用
`security find-generic-password -s ai.idoris.agent24.comm` 核实，**未找到该条目**——确认
整轮联调没有写入真实 macOS 登录钥匙串。

### 3.1 身份 / 联系人 / relay / daemon 建立

| 步骤 | 命令 | 结果 |
|---|---|---|
| A 创建默认身份 | `agent24 comm identity create a24r2-a --default` | 成功，`npub18hmjr...mng0n` |
| B 创建默认身份 | `hyphae identity create --nickname a24r2-b --default --password <合成值>` | 成功，`npub1suy9s...9jh3m` |
| A 加 B 为联系人 | `agent24 comm contact add b-peer2 <B npub> --role agent` | 成功 |
| B 加 A 为联系人 | `hyphae contact add --nickname a-peer --npub <A npub> --role agent` | 成功 |
| A 设置 relay | `agent24 comm relay set ws://127.0.0.1:51146` | `source:"config"` |
| B 设置 relay | `hyphae relay set --relay ws://127.0.0.1:51146` | `source:"config"` |
| A 启动托管的 Hyphae daemon | `agent24 comm daemon start` | `state:"running", generation:1` |
| A 查询 daemon 状态 | `agent24 comm daemon status` | 同上，与 `hyphae-daemon.pid` 记录的真实 pid/sha256 一致 |

过程中有一次操作失误：首次 `hyphae identity create` 用内联 `$(date +%s)` 拼接密码却未保
存该值，导致后续发消息报 `encrypted keystore requires --password-stdin`。处理方式是删除
刚建好但密码丢失的 B 侧 keystore（`rm -rf /tmp/a24r2-b/.hyphae`，此时 B 尚未发送/接收任
何消息，无数据损失），用保存到临时文件的合成密码重建身份（npub 随之变化，对应把 A 侧
联系人改记为新昵称 `b-peer2`，`contact add` 不支持覆盖已存在的昵称）。这是本轮操作过程
的一个记录，不计入"发现"列表。

## 4. Zero-run 验收

**计数端点**：`GET /api/v1/runs`（`apps/agent24d/src/server.rs:865`，
`apps/agent24d/src/runs.rs::list_runs`），返回 `{"runs":[...]}`，用数组长度计数。

- R0（6 条入站消息发送前）：`0`
- R1（6 条入站消息全部到达 `comm history` 之后）：`0`

**6 类入站消息**（内容参考 `packages/nostr-bridge/src/protocol.ts` 的 F4 envelope/Content
形状、`packages/nostr-bridge/src/liveness.ts` 的 canary 前缀、以及
`docs/design/COMM-HYPHAE.md` §7 zero-run 判据表指向的 Hyphae
`tests/contracts/testdata/public-query-fixtures.json` 里的 query/response 样本）：

| 类别 | 内容来源 | event_id（B 发送返回值） |
|---|---|---|
| 1. 纯文本 | `"plain text round2 zero-run probe"` | `9c93654b9607c2d475b787999c1bb131527ba16ca22a662fd08c38ccb1b6a6f2` |
| 2. F4 command 风格 JSON | `{"version":"f4/1","intent":"ask","thread_id":"<uuid>"}` | `6c43523b20bad3305e0639399bb888680329daedbb6e443982a9314fdca4a98c` |
| 3. query 形态 | `public-query-fixtures.json` 的 `query-profile` body | `2f33e6175f0138ccc685113da90051a5cb9d4714d45d1e56dcc8d76ba549f832` |
| 4. response 形态 | 同文件 `response-ok-profile` body | `171ce440d5879de8daa249e74bfc5216bc1eb9c632dbf5d4dc5dad553e6e6a66` |
| 5. receipt 形态 | `{"version":"f4/1","intent":"ack","thread_id":"<uuid>","status":"working"}` | `8b19b7c8611e20e136c79c0dfdc7f715b91b5af2524dac453704eb296e7ce42c` |
| 6. canary 形态 | `"a24-liveness-canary <uuid>"`（`CANARY_PREFIX`） | `f77aeeae017140822a545a32c1a159a7e07ed3ca32282fda701cd645648fca93` |

全部用 `hyphae agent msg --from a24r2-b --to a-peer --content "<body>" --relay
ws://127.0.0.1:51146 --password-stdin` 发送（未显式加 `--encrypt`，Hyphae 默认即加密），
全部 `published_to:1`。

**断言结果**：

| 断言 | 结果 |
|---|---|
| 1. `agent24 comm history --as a24r2-a` 出现全部 6 个 event_id | 通过——6 条全部出现，耗时约 5 秒内全部补收完毕（远快于 90 秒预算；该次 `comm daemon start` 之后的首次轮询周期很快打到） |
| 2. run 计数仍为 R0（run=0） | 通过——`GET /api/v1/runs` 前后均为 `{"runs":[]}`，R0=0，R1=0 |
| 3. agent24d 日志无创建 run 的记录 | 通过——`a-stderr.log` 全文无 `create_run`/`start_run`/`"type":"run"` 等任何 run 相关字样，期间唯一的日志行是启动时的 scheduler/delivery-pump/attach-listener 信息行和 memory-warn |

## 5. 收发与重试（COMM-3 路由）

| 步骤 | 命令/操作 | 结果 |
|---|---|---|
| A→B 发送 | `agent24 comm send b-peer2 "round2-comm3-send-test-..."` | `layer:"L2"`, `published_to:1`, `event_id=6dd6cafd...d8e` |
| B 拉取确认 | `hyphae agent inbox --as a24r2-b --relay ... --decrypt --password-stdin` | 看到同一个 `event_id=6dd6cafd...d8e` |
| 按 pid 停 relay | `kill <relay pid>` | relay 进程退出 |
| 断线后再发 | `agent24 comm send b-peer2 "round2-comm3-l1-test-..."` | `layer:"L1"`, `published_to:0`, `queued_for_retry:true`, `event_id=5956797c...9bc` |
| 重启 relay | 同端口、同 data-dir 重新拉起 `hyphae-relay` | relay 恢复监听 |
| 重试 | `agent24 comm outbox retry 5956797c...9bc` | `event_id` 不变（`5956797c...9bc`），`sent:true` |
| B 拉取确认 | `hyphae agent inbox ...` | 该 `event_id` **恰好出现一条**（inbox 里另有一条是前一步的 `6dd6cafd...d8e`，各自独立） |

全部符合预期：L2/L1 层级判定、`published_to` 字段、重试复用原 `event_id`、重试后对端只
收到一条（不是重复的两条）均与 COMM-HYPHAE.md §5.1/§5.3 的描述一致。

## 6. Daemon 托管（COMM-4a）

| 步骤 | 操作 | 结果 |
|---|---|---|
| kill -9 托管的 Hyphae daemon | `kill -9 <hyphae pid>`（当时 pid 57936，generation=1） | 立即退出 |
| 自动重启确认 | 轮询 `agent24 comm daemon status` | 1 秒内恢复：`state:"running"`，`generation:2`（`consecutive_failures:1`），新 pid 64049，`bin_sha256` 不变 |
| 重启后补收确认 | B 再发一条 `round2-post-restart-probe-...`，轮询 `agent24 comm history` | 约 12 秒内出现在 A 的 history 里，补收正常 |
| `relay set` 改配置 | 对已配置的同一个 relay 再次 `agent24 comm relay set ws://127.0.0.1:51146` | `generation:2 → 3`（`consecutive_failures` 不变，仍是 1——符合"配置变更重启不计入失败次数"的设计），Hyphae 侧 pid 又变化（65611），证实确实发生了一次重启 |
| `agent24 daemon stop` | 对正在运行的 agent24d（pid 56612）执行 `agent24 daemon stop` | CLI 输出 `stopped (pid 56612, port 51211)`；随后 `ps -p 56612`、`ps -p 65611`（Hyphae 子进程最后一次重启后的 pid）均查无此进程，Hyphae 的整个进程组（pgid 65611）也没有任何残留——agent24d 退出时确实把托管的 Hyphae daemon 进程组一起带走了 |

关于"restart 计数"：`agent24-comm` 的 `DaemonStatus`（`crates/agent24-comm/src/daemon.rs`）
没有单独的"重启次数"字段，`generation` 字段本身就在"每次成功 spawn（含崩溃重启和配置
变更重启）"时自增（`daemon.rs::spawn_and_record`），所以本轮把 `generation` 的增量当作
任务要求的"restart 计数 +1"的等价指标——kill -9 重启和 relay 配置变更重启各自都让
`generation` 精确 +1，与任务描述的预期一致。

## 7. 发现列表

| # | 发现 | 严重性 | 细节 |
|---|---|---|---|
| F2 | `agent24 daemon stop` 对一个确实成功停止的 daemon，CLI 进程本身的退出码表现需要复核 | 低/待确认 | 在 §6 最后一步执行 `agent24 daemon stop` 时，命令打印了成功信息 `stopped (pid 56612, port 51211)`，`wait_for_stop`（`apps/agent24-cli/src/main.rs:1489`）在这条路径上就是返回 `Ok(())`；但同一个 shell 调用链里紧跟着执行的清理性 `ps -p <pid>` 检查（故意验证进程已消失）因为"找不到进程"而以非零退出，被外层工具误报为"上一条命令退出码 1"。复核后确认这**不是 `agent24 daemon stop` 本身的退出码问题**，而是本轮验证脚本把两条命令的输出和退出码连在一起读產生的误判——记录在此是因为过程中一度怀疑是真实 bug，排查后确认 `agent24 daemon stop` 路径本身工作正常，为避免以后重复花时间排查同样的误判而记录 |

F1（生产路径必然写入真实登录钥匙串）已通过 `A24_COMM_PASSWORD_STORE=memory` 解除，本轮
全程使用该开关，未发生真实钥匙串写入（见 §3 的 `security find-generic-password` 核
实）。除 F2（确认为误判、非真实问题）外，**本轮联调没有发现任何其它违反预期的行为**。

## 8. 结论

- Cherry-pick 冲突（`comm_routes.rs` 三处）已解决并记录解法（§1），供 #626/#628 之后真正
  合并 main 时参考。
- 集成分支的格式化/lint/全量测试/`agent24-comm` 真二进制测试全部绿（§2）。
- 第二轮联调的 zero-run（§4）、收发与断线重试（§5）、daemon 托管（§6）三组断言**全部按
  预期通过**，没有为了让测试通过而改动任何断言或绕过约束。
- 全程未触碰 `~/.hyphae`、真实钥匙串，或 jason 正在运行的 agent24d；所有进程按 pid 停
  止，未使用 `pkill -f`；`/tmp/a24r2-*` 已清理。
- 未 push 任何分支，未开 PR。
