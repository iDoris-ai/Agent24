# Hyphae 固定二进制升级记录（2026-10-28，v0.27.2）

## 结果

- Agent24 基线：`4a2b55dd`（origin/main，#1421）。
- Hyphae 来源：[`v0.27.2`](https://github.com/iDoris-ai/Hyphae/releases/tag/v0.27.2)，提交 `076a5205f0f45539082538c8f10ea3db69b7a8f1`（Hyphae PR #295 合并提交）。旧锁为 v0.27.1 `859d8fb7e79c67beb3c7a5a5d6d58c36baac7d0f`（见 [LOCK-UPGRADE-20261027b](LOCK-UPGRADE-20261027b.md)）。
- Go：`go1.26.4`；构建配方不变。
- `darwin/arm64` CLI SHA-256：`9bfa2e3e3cd68738654cd54112c696f94ecbb9707a211cc80f5aafc92784523f`
- `linux/amd64`（锁里记作 linux-x64）CLI SHA-256：`0152a7e9e146f3f8d7c1de39bf98d2ac4f32e65d17acd680aeb8506ae5c2ab60`
- `hyphae.lock.json` 在 Release 里的 SHA-256：`5795e2702675f61e5db1060d0240c0d05b8a45e9de93435395b9feb189f8c97c`；Release 自带的 lock 内容与本仓 lock 逐值一致。
- `hyphae-relay`（与 v0.27.0/v0.27.1 哈希相同，仅记录，不进 lock）：darwin/arm64 `1d6dfc5f69914953bd9b6145b39d2cf6ffa1291c28aaababad93c7baf8146a1b`；linux/amd64 `c6327e3571bc5544579d051d3a76d375a4289e2109d5417045351dbb252dc000`。
- 平台空缺：`darwin-x64`、`linux-arm64` 本版 Release 没有资产，保持 null。

## 证据

四个二进制均用 `gh release download v0.27.2` 下载到空目录后本机 `shasum -a 256` 实算，并与 Release 的 `SHA256SUMS` 逐值一致。

## v0.27.2 相对 v0.27.1 的变化：退出码 4 → 1（#1346、Hyphae #295）

`groupchat` 的调用方输入类错误由 `other_error`（退出码 4）改为 `user_error`（退出码 1）：group not active（`send`、`history`）、未知 logical id（`status`、`retry`，原 message 为 `sql: no rows in result set`）、invalid group ID、invite not found、identity not found、调用方已离开的群。`history --limit 0` 与负数改报 `user_error`（原按 100 处理）。不变：部分失败的 `send` 仍是 `other_error`/退出码 4 且 `data` 带逐收件人报告；存储/SQL、协议不一致仍为 `other_error`；`retry` 非 failed 状态与 identity mismatch 仍为 `other_error`。

## 对 Agent24 的影响（逐点核对）

Agent24 不按退出码数字判断业务，而是按 envelope 的 `error` 名映射（`error.rs::map_envelope_failure`）；`runner.rs::ExitClass` 已同时认识退出码 1 与 4。

| 调用点 | 结论 |
|---|---|
| `error.rs::map_envelope_failure` | `user_error` → 400 `invalid`；带 `data` 的非 user_error → 502 `partial`。新旧版本都正确，无需改。 |
| `groupchat.rs` send 部分失败 | 仍是 `other_error`/4 + `data` → 502 `partial`，不变。 |
| `groupchat.rs` history `limit` | 本地已限制 1..=200，先于 Hyphae 拦截，不变。 |
| group not active / 未知 logical id / invite·identity not found | 0.27.1：502 `upstream`；0.27.2：400 `invalid`。**行为变化**，且是预期的（调用方错误本就不该是 502）。 |
| `router.rs`、`import.rs`、`npub.rs` 及其假 hyphae 脚本（`exit 4`） | 都是 `send`/`import` 等非 groupchat 命令或「绝不应运行」的哨兵脚本，不受影响。 |

决定：**只认新码，不做 0.27.1 兼容分支**。映射本来按错误名走，不存在需要「同时认 4 和 1」的代码；锁只允许 0.27.2 这一个二进制，所以没有两个版本并存的运行时。

## 改动范围

- lock 值、`binary.rs` 断言、两份 Deployment 文档的锁说明。
- `groupchat.rs`：只改文档注释。
- 测试：`groupchat_routes_stub.rs` 中 `exit_four_without_data_is_502_upstream` 改用真正的内部错误（`sqlite: disk I/O error`），并新增 `exit_one_user_error_group_not_active_is_400_invalid`；`groupchat_real_binary.rs` 中未知 logical id 的预期由 502 改为 400。
- 没有改生产逻辑，没有改工作流。

## 验证

见 PR 描述。未验证：linux-x64 哈希的复现（交给 `hyphae-lock-verify`）。
