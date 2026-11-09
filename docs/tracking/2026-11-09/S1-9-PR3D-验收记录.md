# S1-9 PR-3d：macOS OS 包验签

> 本文保留前两轮历史；最新调查、debug 开关和验收结果见 [第三轮验收记录](S1-9-PR3D-第三轮验收记录.md)。

## 设计

- 信任文件是 daemon 数据目录下的 `os-trusted-teams.json`，与 packages 路径覆盖无关。只允许 daemon API 修改；CLI 安装和 daemon scan/recheck 读取同一份宿主策略。内置 `HYAP444FN9` 永远在集合中。缺失、损坏或无法读取时仅保留内置团队；损坏记录警告。
- 更新持有跨进程文件锁，0600 临时文件写入并 fsync，再原子 rename 并 fsync 父目录。三个 CLI 命令经已认证的 daemon 路由；能力路由中写操作为 ModuleAdmin/Elevated，读取为 ModuleRead/Everyday。
- 每次验签重新读取信任集合，因此取消信任影响下一次 scan/recheck/install，不主动停止已经运行的模块。
- 共享 `SignatureVerifier` 增加默认拒绝的 `team_id` 方法；Codesign/CodesignWith 负责读取唯一、合法的 TeamIdentifier。包侧先检查团队集合，再调用共享 Developer ID/runtime 验证。对外只返回相对路径、团队与类型化原因，不返回 codesign 原始 stderr。
- scan/recheck 在完整哈希和原生格式检查后使用 AtRest；安装对复制后的 staging 使用 Install，成功才发布。公证失败提示刚公证的包可能需要等待几分钟。
- `package-integrity verify` 保持哈希和原生格式自检，不验签，帮助文字说明边界。
- 单元测试通过显式 Policy 注入接受 verifier。本轮默认特性的 CLI 黑盒测试将封包直接置入 `packages/<name>`，不再以 `os install` 安装 cc stub 作为准备步骤；安装负例明确验证 macOS 拒绝未签名包。本轮未新增运行时验签旁路。

## 关键测试

- `untrusted_team_is_refused_then_trust_and_untrust_take_effect`：外部团队拒绝 → 显式信任通过 → 撤销拒绝 → 损坏信任文件仍拒绝。
- `adhoc_unsigned_and_invalid_signature_are_refused`：缺少团队和无效签名均拒绝。
- `builtin_passes_and_other_platforms_do_not_call_verifier`：内置团队接受；Linux/Windows 不调用 Team 读取或验签。
- `team_reader_rejects_unsigned_adhoc_duplicate_and_malformed_claims`：假的 CommandRunner 覆盖无签名、ad-hoc、重复、非法 TeamIdentifier。
- `trust_roundtrip_builtin_and_corruption`：存储往返、0600、格式校验、内置团队不可移除、损坏回退。
- `scan_recheck_install_modes_and_failed_install_cleanup`：真实原生夹具走 scan/recheck/install，记录公证参数分别为 false/false/true，失败安装不留 staging，发布自检不调用验签。
- `cancellation_during_team_read_beats_signature_failure`：取消优先。
- `apple_platform_binary_is_not_implicitly_trusted`：真实 `/usr/bin/true` 拒绝。
- `trust_team_api_persists_lists_and_protects_builtin`、`trusted_team_commands_parse_and_help_explains_builtin`：API 与 CLI。

## 反面对照

先将包策略检查留为空实现，运行拒绝测试：未受信团队用例因返回 Ok 导致 unwrap_err 失败；无签名/无效签名用例因没有返回错误而失败。随后执行两项独立变异（均已恢复）：

1. 删除团队集合限制：`untrusted_team_is_refused_then_trust_and_untrust_take_effect` 返回 Ok，`unwrap_err` 失败，退出码 101。
2. 保留 Team 读取和集合检查，只将 verifier 的结果改为成功：`adhoc_unsigned_and_invalid_signature_are_refused` 的拒绝断言失败，退出码 101。
3. 恢复源码后 `cargo test -j 4 -p agent24-os-packages --lib signature::`：6 项通过，退出码 0。

## 拆分建议

变更超过 600 行，建议按依赖顺序拆为三片：信任存储及 CLI/API；共享 Team 读取和 scan/recheck 验签注入；Install 模式和安装清理测试。各片须带自身测试，后两片基于前片。未推送，未开 PR。


## 第一轮验证结果（历史，最终状态见第二轮）

命令在 `rust/` 下执行（实际使用等价 `--manifest-path rust/Cargo.toml`）。所有测试前设置 `ulimit -n 4096`，cargo 使用 mbx、`-j 4`，未设置 `CARGO_TARGET_DIR`。

| 检查 | 结果 |
| --- | --- |
| `cargo test -j 4 -p agent24-components -p agent24-os-packages -p agent24-os-proc` | 在 components 首个测试目标停止：76 通过、5 失败、2 忽略；5 项 download 重定向测试均因 TCP 监听 EPERM |
| 单独补跑 `cargo test -j 4 -p agent24-os-packages -p agent24-os-proc` | packages 96/96 通过；os-proc 单元测试 8/8 通过；macos_identity 集成 2 通过、2 因 EPERM 失败 |
| `cargo test -j 4 -p agent24-components sign::tests` | 20 通过、2 忽略；包括新 TeamIdentifier 解析用例 |
| `cargo test -j 4 -p agent24-cli` | 133 通过、56 失败、6 忽略；逐一检查 56 个失败块，全部包含 Operation not permitted；后续集成目标未执行 |
| CLI `trusted_team_commands` 定向测试 | 1/1 通过 |
| `cargo test -j 4 -p agent24-cli --features test-support --test os_uninstall_consents` | 5/5 通过，真实 CLI 安装/卸载夹具 |
| `cargo test -j 4 -p agent24d` | lib 目标的既有 iDoris 测试长时间等待，未完成；不能报告通过 |
| daemon `--bin agent24d trust_team_api` 定向测试 | 1/1 通过，不需要绑定端口 |
| `cargo clippy -j 4 --workspace --all-targets --all-features -- -D warnings` | 通过；首轮的 items-after-test-module 已修正 |
| `cargo fmt --all --check`、`git diff --check` | 通过 |

os-proc 环境失败：`apple_signed_process_fails_team_requirement_before_identifier`、`malformed_team_parameter_is_rejected_before_any_signature_work`，均在 `tests/macos_identity.rs:24` 遇到 EPERM。

没有签名/公证凭据，因此未做真实本方 Developer ID 的安装成功验收；真实 `/usr/bin/true` 拒绝测试已通过。未在 Linux/Windows 主机运行，非 macOS 不调用验签由平台分支测试覆盖。

上述失败不属于 AGENTS.md 的四项沙箱白名单，完整门禁没有通过，因此未提交、未推送、未开 PR。待非沙箱环境补跑完整门禁后，再按上面的三片建议提交，使用用户指定作者及尾注。

仍运行的本次 daemon lib 测试进程 PID 为 98690（cargo 95342、mbx 95318）；尝试 `kill -TERM 98690` 返回 Operation not permitted，沙箱内无法清理。需要宿主会话终止该次测试。日志位于 `/tmp/pr3d-daemon.log`，其余详细日志为 `/tmp/pr3d-*.log`。


## 第二轮修复（2026-11-09）

- 补齐三个 trusted-teams OpenAPI 操作、响应及 D6 `{code,message,retryable}` 操作错误体；认证错误沿用公共 401/403 契约。同步 CORE-2 §3 路由表，写操作维持 ModuleAdmin/Elevated，与 OS enable/disable 同级。
- `serve` 的包扫描实际在 `docked_startup::pre_start` 之后；失败由 `with_discovered` 改名引起。顺序测试更新为 `with_discovered_policy`，同时新增 `os_signature_policy()` 必须晚于 hook 的断言。
- `os_cli_no_ephemeral`、`os_uninstall_consents`、`uninstall_hot_stop` 的安装准备直接写入/移动到 `.agent24/packages/<name>`。保留安装撤销失败的断言，修复 store 后在 macOS 验证 `signature_invalid` 且无包落盘；无 HOME 的安装负例仍检查撤销提示。
- **签名错误三种（`signature_invalid` / `notarization_failed` / `untrusted_team`）在 PR-3c 的原因映射里都归入 §4 的 `signature_rejected`。** PR-3d 保留细分错误以供诊断。
- 本轮回归的反面对照来源为 Opus 沙箱外已报告的失败；新增 D6 断言验证业务错误的三字段结构。未将沙箱失败算作通过。
- 已离线执行 `pnpm install --frozen-lockfile --offline`。生成客户端保留为未提交改动；连续两次 `pnpm gen:api` 后 SHA-1 文件清单一致，生成无漂移。`git diff --exit-code packages/api-client` 相对 HEAD 非零是本轮新增 API 类型的预期差异，未通过暂存或丢弃生成文件伪造干净状态。

### 第二轮验证结果（默认特性，沙箱内）

| 检查 | 结果 |
| --- | --- |
| `cargo test -j 4 -p agent24d` | lib 的 iDoris 用例长时间等待，未完成；已通过执行会话发送 Ctrl-C，退出 130，不宣称全绿 |
| daemon `route_openapi_gate` + `kernel_routes_and_openapi` | 5 + 1 项通过 |
| daemon `section_3_rows_match_operation_and_level` | 1/1 通过；§3 固定行数由 93 同步为 96，保留逐行 operation/level 对照 |
| daemon `serve_hook_precedes` / `trust_team_api` | 各 1 项通过；含 D6 三字段错误断言 |
| `cargo test -j 4 -p agent24-cli` | 主测试 134 通过、56 失败、6 忽略；逐个检查 56 个 panic 块，均包含 Operation not permitted；后续集成目标未自动执行 |
| CLI `os_cli_no_ephemeral` / `os_uninstall_consents` | 单独补跑，3/3 与 5/5 通过；无需 `--features test-support` |
| CLI `uninstall_hot_stop` | 1 通过，2 个既有沙箱白名单用例失败，均为 daemon 启动 EPERM |
| `cargo test -j 4 -p agent24-os-packages -p agent24-components -p agent24-os-proc` | 在 components 停止：77 通过、5 个 download 监听 EPERM 失败、2 忽略 |
| 单独补跑 `cargo test -j 4 -p agent24-os-packages -p agent24-os-proc` | packages 96/96、os-proc 单元 8/8 通过；macos_identity 2 通过、2 个 EPERM 失败 |
| `cargo clippy -j 4 --workspace --all-targets --all-features -- -D warnings` | 最终代码检查通过 |
| `cargo fmt --all --check` / `git diff --check` | 通过 |
| `pnpm -w run lint` | 通过 |
| `pnpm gen:api` | 通过，重复生成哈希一致，新增生成类型保留在工作区 |
| `pnpm lint:openapi` | 首次 dlx 缓存写入 EPERM；将缓存改到 /tmp 后依赖下载 ENOTFOUND，未完成 |

完整门禁仍需 Opus 在沙箱外复跑，不能把这些非白名单环境失败算作通过。本轮未提交、未推送。日志：`/tmp/pr3d-round2-*.log`。本轮 daemon 执行会话已停止；第一轮文中历史 PID 未重新核验。

提交身份保持 `jhfnetboy <firsttorch66@gmail.com>`，尾注 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`；本轮因完整门禁未通过，所有改动留在工作区，未创建提交。
