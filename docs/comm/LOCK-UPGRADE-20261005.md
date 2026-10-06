# Hyphae 固定二进制升级记录（2026-10-05）

## 结果

- Agent24 基线：`54cec44a7410532e8a864ea90860f2d442c06ba2`（本地 `main`）。
- Hyphae 来源：本地 `agent-speaker` 仓库的干净 Git 源码提交 `671c584f9e9eb807a15968e2aa42fd7507e178b8`。
- Go：`go1.26.4`；固定构建配方保持原样：
  `GOTOOLCHAIN=go1.26.4 CGO_ENABLED=0 GOOS=<os> GOARCH=<arch> go build -trimpath -buildvcs=false -ldflags='-buildid=' -o hyphae ./cmd/hyphae`
- 两次独立 `darwin/arm64` CLI 构建都得到 SHA-256：
  `d1171421e91ae62c40374bd00049cd51dd6ac1135b6cd7b31908968b9158df60`
- `linux/amd64` CLI 构建 SHA-256：
  `ccdf1a603b7f4105bf8d522c68f0e68a9d380d967d4b5d272a70739aeda94806`
- `darwin/arm64` `hyphae-relay` SHA-256：
  `a012d86e549cbeb564d5a5932c54f9b3511c2434203846537096420c89f36aef`。该值仅作记录，不进入 lock；它与此前联合调试记录的 relay 哈希一致。

## 构建与证据

构建均在本机完成，没有使用其他机器或远端 Codex。构建源通过 `git archive <SHA>` 从 `/Users/jason/Dev/auraai/agent-speaker` 提取，因此工作目录中未跟踪的 `.codex/`、`.loopx/` 不参与构建。每次构建使用单独临时目录，运行固定配方后直接对产物执行 `shasum -a 256`。Go 环境报告为 `go version go1.26.4 darwin/arm64`。未记录 HOME、凭据或用户数据；临时产物路径已省略。

Agent24 的 `.github/workflows/hyphae-lock-verify.yml` 仍从 lock 读取来源 SHA、配方和 Linux 哈希，并据此检出、构建、比对。现在工作流也校验来源是完整 40 位 commit SHA、Linux 哈希是 64 位 SHA-256；从 lock 读取精确 Go 版本、检查配方中的 `GOTOOLCHAIN` 与该版本一致，并将该版本传给 setup-go。这样检出目标和 Go 版本都由 lock 的精确值控制。

## 验证结果与待处理

- `actionlint .github/workflows/hyphae-lock-verify.yml`：通过。
- `jq` 对 lock 中新来源 SHA、Go 版本及两个二进制哈希进行断言：通过。
- 全套命令 `HYPHAE_TEST_BIN=/tmp/hyphae-darwin-arm64-run1.KhZFmh/hyphae cargo test -p agent24-comm`：exit 0；148 项单元测试通过。实际调用锁定 CLI 的集成项为 `real_binary` 1 项、`import_real_binary` 1 项、`keystore_write_lock` 1 项、`router_lifecycle` 2 项，共 5 项。`daemon_status_probe` 的 5 项是 status/relay probe 测试，`daemon_supervise` 的 21 项使用 fake-script CLI；这 26 项不是对新 Hyphae CLI 的联调。4 项测试按原配置 ignored。该次完整运行日志未保留。
- 本轮目标重跑命令：`HYPHAE_TEST_BIN=/tmp/hyphae-darwin-arm64-run1.KhZFmh/hyphae cargo test -p agent24-comm --test real_binary -- --nocapture`：exit 0，`real_hyphae_binary_envelopes_round_trip` 1 passed。原始输出保存在工作树外 `/tmp/agent24-lock-upgrade-20261005-real-binary.log`，仅含构建/测试结果与本地临时路径，无凭据或用户数据。

为配合 lock 升级，已将 `binary::tests::embedded_lock_parses_and_resolves_baseline_platform` 中的来源 SHA 与 macOS 哈希断言更新为新值；未改动生产逻辑。

`tests/real_binary.rs` 按二进制哈希保留两种兼容分支：reference hash `bc30dcf7…` 断言非法 npub 的 `contact add` 为 `OtherError/other_error`（exit 4）；当前 embedded lock 的 darwin-arm64 hash `d1171421…` 断言为 `UserError/user_error`（exit 1）。生产路由未改。新 lock 分支由上面的本轮目标命令实际验证通过。旧 reference binary 不在已记录的本机路径 `/Users/jason/.local/share/hyphae-pr-monitor/state/validation-artifacts/cli-integration-a4aa606/macos-arm64/hyphae`，因此本轮未能执行 reference 分支；要验证该分支，Agent24 需恢复该本地验收产物或提供同 hash 的本地 artifact。

固定到新来源后发现的兼容差异也影响文档：`docs/design/COMM-HYPHAE.md` 的 G6 和 `docs/comm/JOINT-ROUND1.md` 仍记录旧行为。建议 Agent24 后续同步当前与 reference 两种行为，避免历史 G6 被误作新 lock 的契约。

本次没有为绕过失败而改写或伪造任何哈希。其余锁、工作流与测试文件均在约定范围内。
