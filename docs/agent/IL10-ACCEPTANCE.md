# IL-10 生命周期 API 验收记录

本片把 IL-03、IL-05、IL-06、IL-07/08 与 IL-09 接入 agent24d。工作区未提交、未推送。任务明确授权在本机 Mac mini 构建和测试。

## 实现与接口

`rust/apps/agent24d/src/idoris_host.rs` 持有安装记录、生命周期操作、IL-07 supervisor 与 IL-09 网关。`idoris_routes.rs` 提供以下宿主接口，能力操作统一为 `idoris.lifecycle`，仅 ProductHost 或 legacy bearer 可访问。

| 方法 | 路径 | 结果 |
| --- | --- | --- |
| GET | `/api/v1/idoris/status` | 非敏感状态投影 |
| POST | `/api/v1/idoris/start` | 202，生命周期操作 |
| POST | `/api/v1/idoris/stop` | 202，生命周期操作 |
| GET | `/api/v1/idoris/operations/{id}` | 阶段、结果、稳定错误码 |
| POST | `/api/v1/idoris/operations/{id}/cancel` | 取消启动，等待子进程回收 |

start、stop 接受可选 `Idempotency-Key`。同一标识重放返回原操作，跨 daemon 重启保留结果。同一标识用于不同动作会返回 `invalid_request`。不同请求与正在执行的生命周期操作冲突时返回 409 `busy`，supervisor 自动重启期间的 start 同样返回 busy。已达到目标状态的 start、stop 返回完成的操作。

启动可取消，取消会保存不再自动启动的意图。取消与启动完成并发时，操作在回收完成后才结束。停止进入回收阶段后不可取消，cancel 返回 409 `not_cancellable`。已完成操作的 cancel 返回原结果。异步错误在 operation 的 `error_code` 中返回，查询本身仍为 HTTP 200；同步准入错误按 `Code::http_status` 映射。

安装和连接入口留给 IL-11。CLI 入口留给 IL-12，五个 OpenAPI 操作都登记 `x-tui-exemption`。本分支不存在 `scripts/tui-coverage.mjs`，调用结果为 MODULE_NOT_FOUND。

## 恢复、停止与凭据

安装记录位于默认安装根的 `installation.json`，字段来自 `Installation`。记录包含版本、不可变发布目录、锁定归档路径、组件代次目录、key_id 引用、端口、角色、已记录的 core 预留和 oMLX 端点。记录不接受数据 secret 作为 key_id，不接受带凭据的 oMLX URL。

IL-11 可调用 `record_installation` 保存记录。下载所得归档需要保留，恢复启动时 `InstalledVersion::verify_existing` 用内置锁的 hash 重验归档和全部安装文件，再校验签名。daemon 隐藏参数 `__idoris-install-child` 在启动线程前转入 IL-03 安装子进程。

恢复只启动已配置且 `desired_running=true` 的实例。用户 stop 持久保存停止意图，daemon 自身关停不清除恢复意图。未配置安装保留可查询状态，不阻止 daemon 启动。生命周期元数据损坏或 restore 持久化失败时记录警告，将 iDoris 标记为不可用，status 返回 failed / persistence_failed；其余 daemon 功能继续启动。无 supervisor 的 Stop 也会检查遗留进程，复用 IL-07 的 lease 和 PID、启动时间、可执行路径、UID 核对；核对失败保留记录并拒绝发信号。L4 处理和崩溃重启继续由 IL-07/08 执行。

钥匙串实现使用 IL-06 的同用户独立命名空间，读取在 blocking task 中执行。就绪适配器读取 supervisor 的 watch 信号。生产输入每代重新计算实时预算并重新探测已选 oMLX，core 预留未知时拒绝启动。

停止先撤下网关，再等待旧 Arc 快照释放。IL-07 负责有界 drain、17 秒 TERM 宽限期、KILL 和 wait。配置已存在时，daemon watchdog 和持久化截止时间预留 50 秒，覆盖 30 秒 drain、17 秒 TERM 与回收余量。模块原有截止时间不变。关停任务在 restore 前注册，daemon 退出前等待其结束。

## 合并修复

- 统一 `InstalledVersion::fixture`，保留 IL-03、IL-05 和 IL-07/08 原有测试。
- setup 改用 rustix 1.x 的 `Signal::KILL`，真机 setup 测试改用异步安装签名。
- IL-06 操作锁和 IL-07 Lease 改为显式 unlock。原有两个并行测试曾分别报 Busy 和补偿记录未清除，修复后并行通过。
- IL-06、IL-07/08 和 IL-10 的测试移入 `tests/mod.rs`，避免环境转发扫描器把真机测试变量当成生产输入。原测试内容在规范化 include 路径后逐字一致。
- 登记内核保留路由名 `idoris`，避免模块同名挂载冲突。
- 保留任务开始前已有的 Cargo.toml 依赖并集合并，更新 Cargo.lock。

## 回归证据

以下测试先展示失败，再通过修复。

| 测试 | 修复前结果 |
| --- | --- |
| `lifecycle_routes_exist_and_creative_is_forbidden` | 宿主 status 为 404，预期 200 |
| `restore_unconfigured_installation_keeps_daemon_available` | restore 返回 NotConfigured |
| `stop_before_install_creates_private_metadata_directory` | 新目录 0755，预期 0700 |
| `receipt_rejects_plaintext_in_a_credential_reference` | 把 secret 当引用保存成功，预期拒绝 |
| `start_during_supervisor_restart_is_busy_and_stop_wins_readiness_updates` | 自动重启中接受了新的 start，预期 Busy |
| `stop_after_failed_start_reaches_stopped_without_a_process` | stop 成功后仍为 Failed，预期 Stopped |
| `cancelled_start_does_not_autostart_on_daemon_restart` | 取消启动后仍保存自动启动意图 |
| `keychain_lock_after_readiness_retains_the_stable_error_code` | 就绪后钥匙串锁定误报 StartFailed，预期 KeychainLocked |
| `cancel_interrupts_a_pending_keychain_read_before_spawn` | 等待钥匙串时取消操作超时 |
| `supervisor::identity::tests::dropping_lease_unlocks_inherited_descriptors` | Lease 已释放但继承描述符使再次 acquire 报 InstanceConflict |

其余主要测试为 `state_machine_idempotency_busy_and_stop`、`keychain_locked_is_stable_without_plaintext_or_spawn`、`l4_and_contract_failures_are_mapped_and_reaped`、`leftover_identity_conflict_is_preserved_without_spawning`、`cancel_and_shutdown_wait_for_process_reaping`、`shutdown_during_start_closes_admission_and_cancels`、`forced_stop_is_reported_and_does_not_leave_a_process`、`terminal_child_failure_closes_gateway_and_updates_status`、`stop_blocks_new_gateway_requests_and_drains_retained_snapshot`、`restore_replays_no_completed_operation_and_honors_stop_intent`。

`lifecycle_wire_fixtures_match_serde_and_openapi` 对 `protocol/fixtures/idoris` 的夹具做 Serde 往返和 OpenAPI 校验，并验证未知状态、错误码会被 schema 拒绝。status 序列化及磁盘记录检查不含数据 key、上游 key、Admin token。

## 评审修复前的本机验收

所有 cargo 测试均先 `ulimit -n 4096`。主 checkout target 不在沙箱可写范围，使用授权缓存 `CARGO_TARGET_DIR=~/.cache/a24-target-il10`，未创建 worktree 自有 `rust/target`。

第 0 步 `cargo test -p agent24-idoris` 为 130 通过、5 忽略。最终指定验收结果如下。

| 命令 | 结果 |
| --- | --- |
| `cargo test -p agent24-idoris -p agent24d` | 1039 通过，0 失败，8 忽略 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo fmt --all --check` | 通过 |
| `pnpm lint:openapi` | 通过，六项既有组件警告 |
| `pnpm gen:api` | 通过，重复生成 hash 无变化，生成结果保留在工作区 |
| `pnpm typecheck`、`pnpm lint` | 通过 |
| `pnpm test` | 1392 通过，1 忽略，使用下述 Node 参数 |
| `real_v040_install_existing_start_running_stop --ignored` | 通过 |
| `git diff --check`、全仓冲突标记扫描 | 通过 |

最终目标包验收包含新加的 Lease 继承描述符回归用例，`agent24-idoris` 原有测试全部保留。

额外执行 `cargo test --workspace --no-fail-fast`，该轮为 4707 通过、13 失败、29 忽略。四个失败属于 AGENTS.md 白名单，另外九个不在白名单，workspace 门禁不算通过。该轮之后补充的钥匙串时序、等待取消与 Lease 释放修改由最终目标包验收覆盖，其余 workspace 包没有改动。

白名单外失败为：

- `agent24-comm` 的 `daemon::tests::ps_lstart_marker_is_stable_across_different_caller_environments`，`ps` 无法读取存活 PID。
- `agent24-comm --test daemon_supervise` 的 `fixture_guard_reaps_the_leader_and_helper_after_a_test_panics`、`manual_start_resets_the_breaker_after_it_trips`、`config_change_bumps_generation_without_touching_failures`、`kill_minus_9_backs_off_then_trips_the_breaker_at_five`、`shutdown_stops_the_whole_group_and_clears_the_pid_file`、`shutdown_preempts_an_in_flight_config_restart_and_forbids_a_new_spawn` 未取得 PID 文件。`reap_orphan_kills_a_live_pid_whose_start_time_matches` 报 Operation not permitted。这些测试依赖进程身份或跨进程信号能力，源码没有修改。
- `agent24-documents` 的 `engine::pdfkit::tests::the_real_helper_reads_the_s01_samples`，真实 OCR helper 返回 Foundation._GenericObjCError 0。该失败的环境原因未进一步确认，源码没有修改。

环境转发扫描原先因真机测试变量失败，测试移入 `tests/` 后 `passthrough_list_matches_what_the_daemon_actually_reads` 单独重跑通过。OpenAPI 校验通过，六项警告均指向既有组件。连续两次 `pnpm gen:api` 的输出 hash 相同，生成结果留在工作区。`pnpm install --frozen-lockfile`、`pnpm typecheck` 和 `pnpm lint` 通过。

pnpm 使用仓库指定的 9.12.0，缓存和 Electron 下载缓存放在 `/tmp`。系统默认缓存路径首次被沙箱拒绝，改用可写缓存后安装通过。Node 26 的实验性 webstorage 会使桌面测试初始化访问不到 localStorage，使用 `NODE_OPTIONS=--no-experimental-webstorage` 后，`pnpm test` 为 1392 通过、1 忽略。

真机测试命令：

```sh
cd rust
export CARGO_TARGET_DIR=~/.cache/a24-target-il10
ulimit -n 4096
IDORIS_REAL_V040_DIR="$HOME/a24-codex/idoris-v040" \
  cargo test -p agent24d --lib real_v040_install_existing_start_running_stop -- --ignored --nocapture
```

该测试复制已解压目录到临时安装根，按锁定归档和签名重验已有安装，再执行 start、确认 running、stop。测试已通过。日志写入临时目录。测试使用内存数据 key，只检查公开 health；跳过 oMLX setup、模型绑定和实际推理，不代表连接或 chat 验收通过。


## 此前验收的进程清理限制

此前验收用 libproc 枚举进程，没有发现 iDoris，但发现六个 CLI attach 测试遗留的 agent24d。它们的可执行路径为 `~/.cache/a24-target-il10/debug/agent24d`，测试 HOME 位于已删除的 `/tmp/a24attach*`。当时发送信号前已核对 UID、启动时间和可执行路径。该记录只描述此前验收，不代表当前进程状态。

这些进程的 SIGTERM 全部返回 EPERM。`ps` 执行也被沙箱拒绝。测试 HOME 和发现文件已删除，无法用该发现文件恢复 HTTP 认证凭据。因此本次没有完成“无残留 agent24d”的要求，不能把进程清理记为通过。后续清理需要重新按当前进程身份复核。

本轮修复和验收仅在笔记本执行。上述限制来自此前验收的沙箱环境。没有修改未涉及的 COMM 或 Documenting 业务来规避失败，没有提交或推送。


## PR #1469 评审修复

| 项目 | 修复与回归测试 |
| --- | --- |
| F1 | `concurrent_submits_admit_exactly_one_start` 用 barrier 同时提交 32 个 start，只允许一个准入。 |
| F2 | `restore_replays_no_completed_operation_and_honors_stop_intent` 先 start、stop，再 reopen、restore，核对无活动操作且 spawn 记录未增加。 |
| F3 | monitor 在关闭通道分支检查自己的取消令牌和操作代次；`stale_closed_monitor_cannot_take_the_next_supervisor_or_block_status` 复现 A stop、B start 后旧 monitor 醒来，status 必须在 300ms 内返回；`stale_monitor_generation_is_rejected_even_without_cancellation` 单独覆盖代次核对。 |
| F4 | 先保存 operations，再提交 installation 意图；`rejected_start_does_not_persist_running_intent` 注入原第二次写入失败；`rejected_start_when_intent_write_fails_is_not_admitted_or_restored` 覆盖新顺序第二次写入失败。 |
| F5 | `stop_after_reopen_reaps_a_live_leftover_without_starting` 创建真实存活子进程和身份记录，保存 stop 意图后重开宿主，经 restore、Stop 确认进程退出、记录清除且没有 spawn；`stop_preserves_an_unverified_live_leftover_without_signalling` 覆盖 UID 不匹配。 |
| F6 | connect 返回后发现 stop 或旧代次时调用 block_gateway 撤回快照；`stopped_generation_withdraws_a_gateway_published_during_connect` 覆盖发布与 stop 的交错。 |
| server | `idoris_corrupt_operations_do_not_prevent_daemon_startup`、`idoris_restore_persistence_failure_does_not_prevent_daemon_startup` 启动真实 daemon，核对模型接口仍可访问且生命周期 start 返回 persistence_failed。 |

F3、F4、F5 和两个 server 测试在修复前分别出现 status 超时、残留启动意图、存活进程未回收、daemon 未就绪；修复后通过。变异验证共 7 项，均在对应回归测试中变红：禁用 submit 的 active 检查（F1）、强制 desired_running=true（F2）、去掉关闭通道时的取消与代次检查（F3）、单独去掉代次检查（F3-generation）、恢复先写启动意图的顺序（F4）、恢复无 supervisor 时 Stop 直接成功（F5）、去掉 connect 后的 block_gateway（F6）。变异脚本在 finally 中恢复源码；恢复后的最终门禁结果如下。


## 本轮最终验收

全部在笔记本、当前沙箱中执行，先设 `ulimit -n 4096`，构建缓存为 `CARGO_TARGET_DIR=~/.cache/a24-target-il10`。

| 命令 | 结果 |
| --- | --- |
| `cargo test -p agent24d idoris` 连续 5 次 | 每次 39 通过、0 失败、1 忽略 |
| `cargo test -p agent24d` | 918 通过、0 失败、3 忽略 |
| `cargo test -p agent24-idoris`（额外验证共用回收逻辑） | 153 通过、0 失败、5 忽略 |
| `cargo clippy -p agent24d --all-targets -- -D warnings` | 通过 |
| `cargo fmt --all --check` | 通过 |
| `git diff --check`、文档引用测试名存在性检查 | 通过 |

上述通过数按命令包含的测试二进制与 doc-tests 汇总；忽略项仍保留，未改成跳过新回归。新增 10 个测试并加强 1 个已有测试。F5 的两个真实进程测试在本机 macOS 上实际执行通过；macOS 专用测试通过 dev-dependency libproc 获取内核进程身份，没有复制生产回收实现。

修复前日志：`/tmp/il10-red.log`（F3/F4/F5）、`/tmp/il10-server-red.log`（两个 server 启动故障）。变异脚本和汇总：`/tmp/il10-mutations.py`、`/tmp/il10-mutations.log`；分项日志为 `/tmp/il10-mutation-*.log`。最终验收脚本和汇总：`/tmp/il10-verify.py`、`/tmp/il10-verify.log`；各命令日志为 `/tmp/il10-verify-*.log`。

本轮未执行 git commit、push、fetch 或 merge。
