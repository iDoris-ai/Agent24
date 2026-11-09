# S1-9 PR-3d 第四轮最终验收：合入 main（PR-3c）并修 Codex 四条

本轮在 `feat/s1-9-pr3d-macos-signature` 上完成 `git merge origin/main`（main 已含 PR-3c #1756，`1ff79230`）。以 PR-3c 的必需清单和类型化原因为准，叠上本分支的宿主验签。第三轮记录里「无 `integrity` 但有 `spawn.command` 时仍验签并放行」已经删除，不再成立。前三轮在沙箱里没有跑完全部门禁，那些数字只作历史，不以它们为通过。

## 冲突怎么解

六个冲突文件：`agent24d` 的 `domain.rs`、`os_routes.rs`、`server.rs`，以及 `agent24-os-packages` 的 `discovery.rs`、`package_integrity.rs`、`test_support.rs`。

- 没有 `manifest.integrity()` 时，`verify_contents` 直接返回 `IntegrityError::Missing`。签名策略不能代替清单。`scan`、`recheck`、`scan_one` 都走 `verify_with_policy`，在哈希和原生格式检查之后调用传入的 `signature::Policy`。生产入口 `verify_required` / `scan` / `recheck` / `scan_one` 使用 `Policy::default()`。`enable` 重扫调用 `scan_one_with_policy`，与写配置共用 `os_control` 锁。
- `SignatureInvalid`、`NotarizationFailed`、`UntrustedTeam`、`SignatureRejected` 的 `reason()` / `code()` 都是 `signature_rejected`（D6 为 `os_package.signature_rejected`）。
- 公开文案：前三种里，`SignatureInvalid`、`NotarizationFailed`、`SignatureRejected` 的 `public_message()` 精确为 `signature_rejected`，不含路径和 codesign 输出。合法 10 位 Team ID 的 `UntrustedTeam` 为 `signature_rejected: {team}; 如信任该开发者：agent24 os trust-team {team}`。非法团队串退回精确的 `signature_rejected`。`Display` 仍给主机日志全文。`/api/v1/os` 的拒绝行 detail 是 `{reason.code()}: {public_message()}`，因此 ad-hoc 关闭时字面是 `signature_rejected: signature_rejected`。这是和 PR-3c 列表格式共用的结果，没有为了去重去改 view。
- `test_support` 同时保留 PR-3c 的 `seal_*` / `n1_*`（真实 `discovery::scan`）和本分支的 `compile_c` ad-hoc、`signature_policy`、给进程内单测用的假策略 `scan`。
- daemon 启动用同一个 `os_signature_policy()` 做 `with_discovered_policy` 和 `retain_rechecked`。`cfg(test)` 的进程内扫描走假策略；真实二进制黑盒由 `tests/support/signature_command.rs` 设置 `A24_TEST_ACCEPT_ADHOC_SIGNATURES=1`。该变量只在 `debug_assertions` 下读取；release 恒拒绝 ad-hoc，变量名不进 release 分支。

## Codex 终审四条

1. **提权。** `agent24 os trust-team` / `untrust-team` 在 `cmd_os` 里提前返回，经 `attach_only_with_enrollment(true)` 和 `send_elevating` 发 PUT/DELETE，空 body。服务端 `resolve_target` 把这两个模板列入 ModuleAdmin 可提权路由，权限与路由策略 §3 的 Elevated 一致。GET 仍是 Everyday，不提权。空 body 的提权摘要由 daemon 生成（`trust OS package team {id}` / `stop trusting OS package team {id}`）。
2. **信任文件。** 读取在同一个 fd 上 `open(RDONLY|CLOEXEC|NOFOLLOW|NONBLOCK)`，`fstat` 必须是普通文件、属主为当前 uid、mode 不含 group/other 写（与写入 0600 对应，不要求恰好 0600），最长 64KiB。符号链接、他人可写、非普通文件、损坏或过大：只信内置团队并记警告。文件不存在仍静默。
3. **缺清单。** 见上，无清单即 `IntegrityManifestMissing`，不调用验签。
4. **原因映射。** 见上，三变体归入 `signature_rejected`。

## 门禁

沙箱外，`rust/` 下，mbx，不设 `CARGO_TARGET_DIR`，`-j 4`，测试前 `ulimit -n 4096`。未起 Docker。前端用 pnpm。

本会话的 shell 在 fd 3 上继承了一条可读管道。trampoline 必须留下 fd 3（模块监听槽），所以直接在这个 shell 里跑时，`the_daemon_binary_becomes_the_module_and_keeps_its_own_fds_from_it` 会看到 `0 1 2 3`。这不是漏关高位 fd。下面的 daemon / CLI 测试在子进程里关掉 fd 3 再 exec cargo，该项在全量里通过。产品代码没有为这条管道改关闭范围。

| 检查 | 结果 |
| --- | --- |
| `cargo test -j 4 -p agent24-os-packages -p agent24-components -p agent24-os-proc -p agent24-os-proto` | 上一会话已绿：components 85 通过 / 2 忽略；os-packages 107 通过（含四个 `n1_*`、t4、信任文件）；os-proc lib 8；macos_identity 4；team_id_consistency 5；os-proto lib 442；doctest 2+6 |
| `cargo test -j 4 -p agent24d` | 全绿。22 个测试目标合计 1433 通过、0 失败、5 忽略。含 lib 75、bin 单测 1298、`module_tool_card` 5（拒绝行断言）、`trampoline` 2。日志 `/tmp/pr3d-agent24d.log` |
| `cargo test -j 4 -p agent24-cli` | 全绿。18 个测试目标合计 275 通过、0 失败、6 忽略。含 `capabilities_trust_team_completes_elevation` 与沙箱白名单里的 `uninstall_hot_stop` 两项。日志 `/tmp/pr3d-cli.log` |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy -j 4 --workspace --all-targets --all-features -- -D warnings` | 通过（42.59s）。日志 `/tmp/pr3d-clippy.log` |
| `pnpm gen:api` | 通过，相对索引无 diff |
| `pnpm lint:openapi` | 通过（valid）。8 条既有 warning：`/os/trusted-teams/{team}` 与 `/{name}/delegation`、`/{name}/stop` 的 ambiguous path，以及 documents 未使用组件 / `unsourced_reason` |
| `vitest run src/openapi*.test.ts` | 在 `packages/contract-tests` 下展开 glob 后 8 个文件、74 项通过。仓库根的同一命令不会展开 `src/openapi*`（该目录不在根上），vitest 收到字面过滤器并报无文件 |
| `cargo build -p agent24d -p agent24-cli --release` 后 `strings` | 通过（1m26s）。`strings -a` 在 `rust/target/release/agent24d` 和 `rust/target/release/agent24` 里都没有 `A24_TEST_ACCEPT_ADHOC`。日志 `/tmp/pr3d-release.log` |

## 变异（改回去即红，随后已恢复）

每条改完只跑对应测试，确认失败后把源码按原字节写回，再跑同一条确认通过。日志 `/tmp/pr3d-mutations.log`。

| 改回去 | 测试 | 红因 |
| --- | --- | --- |
| `read_trust_file` 改回 `fs::read` | `trusted_teams::tests::symlink_writable_and_nonregular_trust_files_stay_builtin_only` | 符号链接被跟读。`left: {"ABCDE12345", "HYAP444FN9"}`，`right: {"HYAP444FN9"}`，消息 `a symlink must not be followed`。测试先在这一条失败退出，0666 和目录断言没有跑到。恢复后整条测试通过，那两臂仍在同一测试里 |
| 无清单改为 `Ok(())` | `signature::macos_tests::legacy_manifest_cannot_skip_host_signature_policy` | `unwrap_err()` 遇到 `Ok(())`，缺清单不再是错误 |
| `SignatureInvalid` 的公开文案改为 `codesign: {path}` | `integrity::tests::macos_signature_failures_share_signature_rejected` | 断言 `public.starts_with("signature_rejected")` 失败，实际文案是 `codesign: bin/run` |
| `resolve_target` 去掉 trusted-teams 的 PUT/DELETE | `capabilities::first_party::tests::trusted_team_writes_are_elevatable_and_reads_are_not`（`--bin agent24d`，不在 lib） | PUT `unwrap()` 得到 `Err(NotElevatedOperation)` |
| `send_elevating` 收到第一次响应后不再 elevate | `capabilities_trust_team_completes_elevation` | 命令失败。`stdout` 为空，`stderr` 为 `error: needs approval`，停在单次 403 |

## 规模与拆分

相对 `origin/main`，不含锁文件和生成的 `openapi.d.ts`，约 64 个文件，+2239 / −215。超过默认 300 行。按依赖可以拆成：信任存储与 CLI/API（含本轮提权和信任文件 fd）；scan / recheck / scan_one 验签与原因映射；Install 模式与安装清理。本轮按任务要求只做 merge commit 和随后的修复 commit，不在这一轮再拆 PR、不 push。
