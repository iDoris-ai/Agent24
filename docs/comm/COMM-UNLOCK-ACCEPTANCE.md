# COMM-unlock 本地验收

## 冻结契约

- 新增 `POST /api/v1/comm/unlock`，继承 comm router 外层 bearer 鉴权。
- JSON 严格接受 `{ "password": "…", "remember": false }`；`remember` 可省略并默认为 false，额外字段拒绝。口令必须是 UTF-8 的 1..=4096 字节。
- 先用当前 embedded lock 的真实 Hyphae CLI 执行 `identity check-password --password-stdin`。只接受成功 envelope 且 `data.encrypted == true`、`data.valid == true`；失败不登记口令。
- check 成功后再次读取 `keystore.json` 的 salt。salt 未变化时才将口令登记到同一个 `Account::from_salt`；变化则返回 conflict。
- 解锁不启动 daemon。`remember=false` 写入 KeyringPasswordStore 的 Zeroizing session overlay，OS keychain 写操作数为 0；`remember=true` 只在 OS 写入成功后报告 remembered=true，写入失败保留原 session 值、不登记本次新输入。MemoryPasswordStore 对两个参数都只返回 remembered=false。
- `agent24 comm unlock [--remember]` 仅从 stdin 读口令。TTY 输入关闭回显；只剥离一条结尾 LF 或 CRLF，保留空格和其他字符。读取器最多消费 4099 字节，超限立即拒绝；unlock HTTP timeout 为 30 秒。口令不在 argv 或环境变量里；无效长度和所有结束路径均尽力清除应用自有缓冲区。
- JSON parse/type/missing/unknown-field 与 Axum body-limit rejection 均归一为 HTTP 400 `{error:"invalid",message:"invalid unlock request"}`；消息固定且不包含提交值。

## 验收测试

`tests/unlock_real_binary.rs` 在常规套件中有具名 `#[ignore]`，因此默认 package/workspace 测试不依赖本地二进制或环境变量。显式 `--ignored` 时必须提供 `HYPHAE_TEST_BIN`，缺失时直接 panic/fail；测试只接受当前平台 `hyphae.lock.json` 的 embedded hash，不接受旧 reference binary。它在临时 HOME 创建加密身份，确认新 MemoryPasswordStore 初始无口令、错误 unlock 返回 locked 且文件清单/内容不变，正确 unlock 后仍不改变 keystore 文件清单/内容、`remembered=false`，再用 store 取回的口令成功运行真实 `identity create` 密码操作。router 单测覆盖 malformed/type/missing/unknown/超 body cap 均为固定 `invalid` 400 且不回显口令；store 单测覆盖默认实现 fail-closed、session-only 删除/新 store 隔离、replacement 与持久失败；CLI 单测覆盖 LF/CRLF、空格、UTF-8 字节边界、无换行超长有界读取和 PTY echo 异常路径恢复。

此前的本地 macOS arm64 验收记录如下；第三轮复验命令与新日志附后。默认可运行测试不需设置真实二进制环境变量：

```sh
cargo test -p agent24-comm -- --test-threads=1
cargo build -p agent24d
cargo test -p agent24-cli
cargo clippy -p agent24-comm -p agent24-cli --all-targets -- -D warnings
cargo fmt --all -- --check
```

此前命令均 exit 0。串行 comm 全套包含 158 个单元测试及全部可运行集成测试；4 个既有测试按原标记 ignored。CLI 全套为 45 个单元测试、3 个 attach_cli 和 3 个 uninstall_hot_stop 测试通过，4 个既有测试 ignored。Clippy 使用 `-D warnings` 通过，fmt check 通过。CLI 集成测试需要先构建 `agent24d`，否则 attach_cli 无法找到被测 daemon。

原始日志位于 worktree 外：`/tmp/comm-unlock-agent24-comm-serial.log`、`/tmp/comm-unlock-agent24d-build.log`、`/tmp/comm-unlock-agent24-cli-after-build.log`、`/tmp/comm-unlock-clippy.log`。串行 comm 全套前，并行运行曾触发既有 `daemon_supervise::shutdown_during_a_blocked_start_leaves_no_process_and_never_reports_running` 的 2 秒调度敏感超时（“blocked start never reached password store”）；该单测独立重跑通过，之后整个 comm 套件串行重跑通过。独立重跑日志为 `/tmp/comm-unlock-daemon-supervise-flake.log`。日志仅用于本地验证，未提交；不含口令明文。

第三轮针对真实验收和修订的验证命令：

```sh
cargo test -p agent24-comm --test unlock_real_binary
HYPHAE_TEST_BIN=/tmp/hyphae-darwin-arm64-run1.KhZFmh/hyphae cargo test -p agent24-comm --test unlock_real_binary -- --ignored --nocapture
env -u HYPHAE_TEST_BIN cargo test -p agent24-comm --test unlock_real_binary -- --ignored
cargo test -p agent24-comm -- --test-threads=1
cargo test -p agent24-cli
cargo clippy -p agent24-comm -p agent24-cli --all-targets -- -D warnings
cargo fmt --all -- --check
```

第三轮验证结果：默认 comm 全套 exit 0（163 单测 passed，4 个既有集成测试及新 `unlock_real_binary` ignored；其余可运行集成测试通过）；CLI 全套 exit 0（47 单测 passed/4 ignored，6 个集成测试 passed）；clippy `-D warnings`、fmt check exit 0。默认新真实测试单独运行 exit 0 且显示 ignored。显式真实二进制测试 exit 0；显式 ignored 缺 env 测试按预期 exit 101 并报告 `HYPHAE_TEST_BIN ... NotPresent`，因此不会静默假绿。真实测试验证 HOME 文件清单/内容 hash 不变、失败 unlock 不登记以及成功 unlock 后的 password-required `identity create` smoke。

第三轮原始日志：`/tmp/comm-unlock-round3-real-default.log`、`/tmp/comm-unlock-round3-real-ignored.log`、`/tmp/comm-unlock-round3-real-missing-env.log`、`/tmp/comm-unlock-round3-agent24-comm.log`、`/tmp/comm-unlock-round3-agent24-cli.log`、`/tmp/comm-unlock-round3-clippy.log`。仅记录命令摘要和退出状态，不含密码、keystore 内容或 token，均在 worktree 外且未提交。
