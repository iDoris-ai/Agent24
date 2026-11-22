# Hyphae 固定二进制升级记录（2026-11-21，v0.33.0）

## 结果

- Hyphae 来源：[`v0.33.0`](https://github.com/iDoris-ai/Hyphae/releases/tag/v0.33.0)，提交 `9566d636b1f19429029f3ce8139251e1f2a3163b`。旧锁为 v0.30.1 `068e2d2e`（见 [LOCK-UPGRADE-20261104](LOCK-UPGRADE-20261104.md)）。
- 锁：`hyphae.lock.json` 与 Release 资产逐字节一致（SHA-256 `087a72b19ff745550532eabe1f1d489af0d65a245c1e0936fe74dcf8bfdc1a3a`，与 Release 的 `SHA256SUMS` 一致），schema 2 不变。
- 契约：`contract.tested` 1.0 → 1.3（只新增命令，1.0–1.2 的调用不变）；`provides` 新增 `nostr-friend`。
- 本次为接入的能力：`contact rename` / `contact remove`（Agent24 接口和界面另开 PR）。

| 资产 | 锁里的 sha256 | 签名 |
|---|---|---|
| `hyphae-darwin-arm64` | `1e94690da8cc4852a00b9ee7ea8aa2eb94a31cfb7be98d7ef0a2a5b769129140` | Developer ID `HYAP444FN9`，`spctl --type install` → accepted，Notarized |
| `hyphae-linux-amd64`（键 `linux-x64`） | `04d8cf358e714c8c521666e91a86e190667a16b443f0f3c4d75b4e8b380e2033` | 无（平台无系统签名） |

## 证据

- 资产从 Release 下载到空目录后用 `shasum -a 256` 实算，与锁和 `SHA256SUMS` 逐值一致；`spctl --assess --type install` 在本机通过。
- 本地 `cargo test -p agent24d hyphae` 51 通过；`cargo test -p agent24-comm` 在机器高负载（load 55–62）下有几条计时用例失败，单独重跑通过。
- 复现比对交给 CI 的 hyphae reproduce 两个任务。
