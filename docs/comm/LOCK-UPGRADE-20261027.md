# Hyphae 固定二进制升级记录（2026-10-27，v0.27.0）

## 结果

- Agent24 基线：`982725e2`（origin/main，#1304）。
- Hyphae 来源：[`v0.27.0`](https://github.com/iDoris-ai/Hyphae/releases/tag/v0.27.0)，提交 `9126147e8eb5e497e3055f9eda27473fefbb91b2`。旧锁为 `144555ca6b043be77225786ddbf4e6bce5ad3228`（v0.26.3）。
- Go：`go1.26.4`；构建配方不变。
- `darwin/arm64` CLI SHA-256：`6b7969a3d4e9767e65927ebd0c2830d272c4b567f05e740a985291b9e462a01a`
- `linux/amd64`（锁里记作 linux-x64）CLI SHA-256：`98e5e9d6b45dce398df2e231e4bf5d7ed05c5e60c9382f56bebdd68becbd656c`
- `hyphae.lock.json` 在 Release 里的 SHA-256：`4a672a1828fa1bc8d73997038294db86ce5899915ebe02c1fa2a004114e04b54`；Release 自带的 lock 内容与本仓 lock 逐值一致。
- `hyphae-relay`：darwin/arm64 `1d6dfc5f69914953bd9b6145b39d2cf6ffa1291c28aaababad93c7baf8146a1b`；linux/amd64 `c6327e3571bc5544579d051d3a76d375a4289e2109d5417045351dbb252dc000`（仅记录，不进 lock）。
- 平台空缺：`darwin-x64`、`linux-arm64` 本版 Release 没有资产，保持 null。

## 证据

哈希取自 Release 的 `SHA256SUMS`（`gh release download v0.27.0 --repo iDoris-ai/Hyphae -p SHA256SUMS`）。

## v0.27.0 契约变化

- `groupchat accept` 返回 `pending` 时退出码 0。
- `send` 部分扇出失败时退出码 4，stderr 错误信封带 `data`。

Agent24 当前未接线群聊命令（见 #1346 第 2 项），本 PR 不改解析；后续接线须按退出码 + 信封解析，不能按「非 0 即失败」。

## 改动范围

只改 lock 值、`binary.rs` 断言、交付/回滚契约文档里的锁说明。没有改生产逻辑，没有改工作流。

## 验证（本机 macOS arm64，二进制取自 Release 并复算 sha256 与锁一致）

- `cargo fmt --all --check`、`cargo clippy -p agent24-comm -p agent24d --all-targets -- -D warnings`：通过。
- `cargo test -p agent24-comm -p agent24d`（`HYPHAE_TEST_BIN` 指向 v0.27.0）：0 FAILED；agent24-comm 单元 163 项、agent24d 单元 648 项通过。
- 联调（`--ignored`，`HYPHAE_JOINT_BIN`/`HYPHAE_JOINT_RELAY` 指向 v0.27.0）：`joint_round1`、`send_history_e2e`、`daemon_joint` 各 1 项，全部通过。
- 未验证：linux-x64 哈希的复现（交给 `hyphae-lock-verify`）；`unlock_real_binary`（#[ignore]，未跑）。

## 契约变化的处理

`runner.rs` 的 `parse_envelope` 按「退出码类别 + 信封」解析：退出码 0 要求 `ok:true`，1–5 要求 stderr 为 `ok:false` 信封并保留 `data`（`Envelope::Failed.data`）。因此 `send` 部分失败（退出码 4 + stderr `data`）已被原样支持；`groupchat accept` 的 `pending`（退出码 0）走成功分支。不存在「非 0 即失败」的粗解析，无需改代码。
