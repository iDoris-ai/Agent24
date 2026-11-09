# PR-3d 第三轮：真实二进制的验签策略

## 为什么 agent24d 黑盒之前能过

**源码不能支持“macOS 默认构建的真实 agent24d 已通过这些黑盒”的结论。** `daemon_modules`、`me3f`、`me4_scheduler`、`me4_model`、`me4_node_module`、`module_tool_card`、`enable_epoch_startup` 以及 attach 中创建的本地包，使用 `seal_python_*` / `seal_exit_stub` 生成 integrity 声明和原生启动文件。它们经过 `serve → with_discovered_policy → scan_with_policy → read_package_with_policy → package_integrity::verify_with_policy → Policy::verify`；启动前的 `domain` recheck 也使用同一策略。没有发现这些已封包夹具专属的跳过验签路径。

旧 `os_signature_policy` 在 **agent24d 自己**的 `cfg(test)` 或 `test-support` 下返回 Accept。普通集成测试运行的 `CARGO_BIN_EXE_agent24d` 没有 `cfg(test)`；dev-dependency 打开 packages 的同名 feature 不会反向打开 agent24d 的 feature。因此，默认 macOS 构建应拒绝这些 ad-hoc 夹具。开启过 daemon feature 的二进制、旧二进制或没有实际执行到集成目标，都可能造成“通过”的观察；没有 Opus 当时的命令、构建指纹和完整日志，不能把其中一种推测写成已证实原因。此前本 worktree 的两轮报告明确记录 daemon 测试停在 lib 用例，未完成，不是这些黑盒通过的证据。

**另发现并修复了真实遗漏：无 `integrity` 的旧格式包从 `verify_contents` 直接返回成功，完全跳过签名检查。** 本轮保留旧格式的可选哈希语义，但它有 `spawn.command` 时仍必须通过宿主签名策略。这不是上述已封包夹具通过的解释，却是 PR-3d 必须补上的验签漏洞。新增拒绝 verifier 测试证明旧格式也会调用验签。

## 实现

- 删除按 feature 选择 Accept 的生产路径。daemon 与 CLI 的非单测构建始终使用 `Policy::default()`；单测保留原有注入策略。
- 只有 `#[cfg(debug_assertions)]` 编译的环境读取识别 `A24_TEST_ACCEPT_ADHOC_SIGNATURES=1`。release 分支恒为 false，变量名不进入这段 release 代码。
- ad-hoc 例外只在没有合法 Team ID 时尝试：codesign 展示成功、唯一 `Signature=adhoc`、唯一 `TeamIdentifier=not set`，并且 `codesign --verify --strict` 成功。无签名、损坏签名、混合/重复团队声明均拒绝。合法外部团队始终执行信任集合及 Developer ID/runtime/公证检查。
- daemon、packages、CLI 的 `test-support` 均有 release `compile_error!` 护栏。
- 两个应用的真实二进制测试由各自 `tests/support/signature_command.rs` 集中设置开关，并保留原先清空或继承环境的区别。CLI 既有生产拒绝用例显式移除开关；新增真实 CLI 开关对照和真实 daemon discovery 开关对照。
- macOS 的 `compile_c` 显式 ad-hoc 签名，避免 Intel 链接器未自动签名导致夹具随 CPU 架构变化。

## 验证与反面对照

所有 cargo 测试前设置 `ulimit -n 4096`，使用 mbx，不设置 `CARGO_TARGET_DIR`。详细命令与结果待本轮运行结束后追加。沙箱外仍应复跑用户指定的完整门禁，不能将 EPERM 失败视作全绿。

- 包库单测：99 通过，包含真实 codesign 开关/损坏签名检查、旧格式包验签检查。
- release 开关反面对照：从当前源码提取原样 `accept_adhoc_signatures` 函数，以 `rustc --test -C debug-assertions=no` 编译；设置环境变量为 1，断言仍为 false，通过。仅把 debug cfg 改为始终编译、release false 分支改为不编译后，同一个断言失败。变异只发生在 `/tmp/pr3d-release-switch-mutant.rs`，产品源码未变。日志 `/tmp/pr3d-round3-release-switch{,-mutant}.log`。

未 push、未开 PR。完整门禁通过前不提交；保留现有未提交改动。拟用身份仍为 `jhfnetboy <firsttorch66@gmail.com>`，尾注 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`。

### 真实 daemon 调查补证

本轮默认特性构建的 `module_tool_card` 对照用例在监听端口前已输出：`idle`、`noted`、`plain` 均 `was not loaded: signature_invalid`（开关为 0）。随后 daemon 因沙箱 EPERM 无法启动，测试未完成开启侧对照。因此已实证这些夹具确实进入生产 codesign 路径；不能再把“此前能过”解释为这些已封包夹具绕过 discovery 验签。完整日志：`/tmp/pr3d-round3-daemon-signature.log`。

### 完整门禁目前结果

| 命令/检查 | 结果 |
| --- | --- |
| `cargo test -j 4 -p agent24d` | 全部目标编译成功；lib 77 项仅观察到 1 项完成，多项 iDoris 用例超过 60 秒，已通过执行会话 Ctrl-C 停止；不能报告通过 |
| `cargo test -j 4 -p agent24-cli` | 134 通过、56 失败、6 忽略；逐个核验 56 个失败块均含 `Operation not permitted`，后续集成目标未执行 |
| `cargo test -j 4 -p agent24-os-packages -p agent24-components -p agent24-os-proc` | components 78 通过、5 个 download 监听 EPERM 失败、2 忽略，命令在此停止 |
| 单独补跑 packages + os-proc | packages 99 通过；os-proc 单元 8 通过；macos_identity 2 通过、2 个 EPERM 失败 |
| components `sign::tests` | 21 通过、2 忽略 |
| CLI `os_uninstall_consents` | 6/6 通过，含真实二进制的 ad-hoc 关闭拒绝、开启接受 |
| CLI `uninstall_hot_stop` | 1 通过、2 个既有白名单失败；daemon 启动 EPERM |
| daemon `discovery_signature_opt_in` | 开关关闭时三个包确实被验签拒绝；之后监听 EPERM，完整用例失败 |
| workspace all-targets/all-features clippy | 通过；最终收尾后再记录增量检查 |

CLI 首次完整运行多出的一项失败来自 `service::tests::passthrough_list_matches_what_the_daemon_actually_reads`。修复是在该源码扫描测试中排除 debug 专用签名开关，而不是把它加入生产 LaunchAgent 的持久化环境。复跑该项通过，CLI 恢复为上表的 56 项 EPERM。

### 变异结果与 release 产物

- 把信任判断变为 `if !_accept_adhoc && !teams.contains(&team)`：外部团队在开启开关时被误放行，`adhoc_opt_in_does_not_trust_foreign_unsigned_or_damaged_code` 失败，退出码 101。
- 删除旧格式包的新增 `policy.verify` 调用（恢复本轮修复前行为）：`legacy_manifest_cannot_skip_host_signature_policy` 失败，退出码 101。
- 两次变异均在 `finally` 中恢复原文件；恢复后 `cargo test -j 4 -p agent24-os-packages --lib signature::` 为 9/9 通过。日志 `/tmp/pr3d-round3-mutant-{foreign,legacy}.log`、`/tmp/pr3d-round3-restored.log`。
- `cargo build -j 4 -p agent24d --release` 成功，9m12s。对实际 `rust/target/release/agent24d` 执行 `strings -a` 和 `nm`，两者都不存在 `A24_TEST_ACCEPT_ADHOC_SIGNATURES`；工具退出码均为 0。日志 `/tmp/pr3d-round3-release.log` 与 `/tmp/pr3d-round3-release-inspect.log`。

### 最终收尾

- `cargo fmt --all --check`：通过。
- `cargo clippy -j 4 --workspace --all-targets --all-features -- -D warnings`：最终源码检查通过（47.37s），日志 `/tmp/pr3d-round3-clippy-final.log`。
- `git diff --check`：通过。
- 本轮 daemon 全量测试执行会话已停止；`lsof /tmp/pr3d-round3-daemon.log` 无持有进程。
- 无完整全绿声明，无提交、push 或 PR。上述非白名单失败和 daemon 未完成项仍需沙箱外复跑。
- release 护栏实测：`cargo build -j 4 -p agent24-os-packages --release --features test-support` 按预期退出 101，唯一编译错误是 `test-support must not be enabled in release builds`，日志 `/tmp/pr3d-round3-release-guard.log`。
