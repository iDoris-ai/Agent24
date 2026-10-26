# Hyphae 固定二进制升级记录（2026-10-24，v0.26.3）

## 结果

- Agent24 基线：`f16e65a95f9732f4e00cb288f967e40e0978eb7b`（origin/main，#1137）。
- Hyphae 来源：[`v0.26.3`](https://github.com/iDoris-ai/Hyphae/releases/tag/v0.26.3)，提交 `144555ca6b043be77225786ddbf4e6bce5ad3228`（Hyphae PR #280 的合并提交）。旧锁为 `671c584f9e9eb807a15968e2aa42fd7507e178b8`，两者相差 280 个提交，包含 M1 的私聊 exactly-once、离线 outbox 和加密群聊。
- Go：`go1.26.4`；固定构建配方不变：
  `GOTOOLCHAIN=go1.26.4 CGO_ENABLED=0 GOOS=<os> GOARCH=<arch> go build -trimpath -buildvcs=false -ldflags='-buildid=' -o hyphae ./cmd/hyphae`
- `darwin/arm64` CLI SHA-256：`545636015d027fa4614ad48361829ff5a5116683a5553f8e2d08fb65ae14261c`
- `linux/amd64` CLI SHA-256：`d741fb9461a6b16d92d471eb103536295808e4dc22a3874b193e12937ee8b99b`
- `hyphae.lock.json` 在 Release 里的 SHA-256：`007fbfbce8f78c98b9a55faa4f60ba1ec28a2835e1d3510905fdbfc8e51c9cf8`（Hyphae 侧生成，本仓的 lock 内容与之逐值一致）。
- `darwin/arm64` `hyphae-relay` SHA-256：`1d6dfc5f69914953bd9b6145b39d2cf6ffa1291c28aaababad93c7baf8146a1b`；`linux/amd64`：`c6327e3571bc5544579d051d3a76d375a4289e2109d5417045351dbb252dc000`。仅作记录，不进 lock。

## 证据

Hyphae 侧用干净 checkout 在 `144555ca` 上两次独立构建，`SHA256SUMS` 完全相同；Release 的九个 asset 与该文件一致，下载复核通过。完整记录见 Hyphae 仓库 `docs/agent/release-v0.26.3-progress.md`。

## 改动范围

只改 lock 值、`binary.rs` 里 `embedded_lock_parses_and_resolves_baseline_platform` 的断言、`docs/Deployment/HYPHAE-NETWORK-DELIVERY.md` 的当前锁说明。没有改生产逻辑，没有改工作流。

## 兼容性

Hyphae 侧对旧锁与 `144555ca` 各编译一份，逐个比对 Agent24 用到的 15 个子命令的 `--help`，参数集合一致。outbox 输出新增 `route` 字段和 `group_pending`/`group_failed` 状态，Agent24 以宽松方式解析，不受影响。群聊没有接入 Agent24 的 Rust/REST/UI。

## 验证

见 PR 描述中的实际命令与结果。
