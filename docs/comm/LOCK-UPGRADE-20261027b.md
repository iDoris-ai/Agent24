# Hyphae 固定二进制升级记录（2026-10-27 第二次，v0.27.1）

## 结果

- Agent24 基线：`82f74573`（origin/main，#1354）。
- Hyphae 来源：[`v0.27.1`](https://github.com/iDoris-ai/Hyphae/releases/tag/v0.27.1)，提交 `859d8fb7e79c67beb3c7a5a5d6d58c36baac7d0f`（Hyphae PR #291 合并提交）。旧锁为 v0.27.0 `9126147e8eb5e497e3055f9eda27473fefbb91b2`（见 [LOCK-UPGRADE-20261027](LOCK-UPGRADE-20261027.md)）。
- Go：`go1.26.4`；构建配方不变。
- `darwin/arm64` CLI SHA-256：`da109e22b7790a89b49dea7aa920b9a47f1813df4a3bbb063a37f5287fae3a9d`
- `linux/amd64`（锁里记作 linux-x64）CLI SHA-256：`83e8bd3fbce314e09f3de26d203b48d9b438388e4bb641145fd5298cc00ae718`
- `hyphae.lock.json` 在 Release 里的 SHA-256：`618e50ceddab61f05f059ef64ce5226402f42c4fe2c31e72824887ffac26561c`；Release 自带的 lock 内容与本仓 lock 逐值一致。
- `hyphae-relay`（与 v0.27.0 哈希相同，仅记录，不进 lock）：darwin/arm64 `1d6dfc5f69914953bd9b6145b39d2cf6ffa1291c28aaababad93c7baf8146a1b`；linux/amd64 `c6327e3571bc5544579d051d3a76d375a4289e2109d5417045351dbb252dc000`。
- 平台空缺：`darwin-x64`、`linux-arm64` 本版 Release 没有资产，保持 null。

## 证据

哈希取自 Release 的 `SHA256SUMS`（`gh release download v0.27.1 --repo iDoris-ai/Hyphae -p SHA256SUMS`）；darwin-arm64 二进制下载后本机 `shasum -a 256` 复算与锁一致。

## v0.27.1 相对 v0.27.0 的变化

Release 说明：仅新增群聊 TUI（`hyphae tui groups`），命令 JSON 契约不变。核对 `git diff v0.27.0 v0.27.1`：`internal/groupchat`、`internal/daemon` 只新增导出函数（`ListMembers`、`SendGroupMessage`、`EnsureGroupPublisher`）供 TUI 使用，其余为 `internal/tui`、文档与测试。Agent24 当前未接线群聊命令，不改解析。

## 改动范围

只改 lock 值、`binary.rs` 断言、交付/回滚契约文档里的锁说明。没有改生产逻辑，没有改工作流。

## 验证（本机 macOS arm64，`HYPHAE_TEST_BIN`/`HYPHAE_JOINT_BIN` 指向 v0.27.1）

- `cargo fmt --all --check`、`cargo clippy -p agent24-comm -p agent24d --all-targets -- -D warnings`：通过。
- `cargo test -p agent24-comm`：0 FAILED（单元 163 项）；`cargo test -p agent24d comm`：0 FAILED（24 项）。
- 联调（`--ignored`，`HYPHAE_JOINT_BIN`/`HYPHAE_JOINT_RELAY` 指向 v0.27.1 / 同哈希 relay）：`joint_round1`、`send_history_e2e`、`daemon_joint` 各 1 项，全部通过。
- 未验证：linux-x64 哈希的复现（交给 `hyphae-lock-verify`）；`unlock_real_binary`（#[ignore]，未跑）；TUI 本身（Agent24 不消费）。
