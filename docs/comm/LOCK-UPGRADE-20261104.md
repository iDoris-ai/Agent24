# Hyphae 固定二进制升级记录（2026-11-04，v0.30.1）

## 结果

- Hyphae 来源：[`v0.30.1`](https://github.com/iDoris-ai/Hyphae/releases/tag/v0.30.1)，提交 `068e2d2e5a657bbf87204b3892451e8a280ac3a7`。旧锁为 v0.30.0 `0e0d4ea4`（见 [LOCK-UPGRADE-20261102](LOCK-UPGRADE-20261102.md)）。
- 锁：`hyphae.lock.json` 与 Release 资产逐字节一致（`cmp` 通过，SHA-256 `0a3d77d3f50582b4690946000c5e4f7da185403f14e3f2da282a6abeb990bf6f`，与 Release 的 `SHA256SUMS` 一致），schema 2 不变。
- 契约（`contract` / `provides`）不变，只换了版本、提交、资产 sha 和配方里的 `-X main.version`。
- 二进制变化：v0.30.0 → v0.30.1 共 14 个提交。除依赖 `modernc.org/sqlite` 从 1.61.0 升到 1.61.1 外，其余都是 joint-ci 脚本和发布文档改动。

| 资产 | 锁里的 sha256 | 签名 |
|---|---|---|
| `hyphae-darwin-arm64` | `7a079c5d43d6179dda0e46f05d0c31d991c0b7ada42a41b862d508a09e67b172` | Developer ID `HYAP444FN9`，`spctl --type install` → accepted，Notarized |
| `hyphae-linux-amd64`（键 `linux-x64`） | `096d9664fb4ab7ebf2bb11204bb78e076cf45b58c140e8a753e5d247f008b838` | 无（平台无系统签名） |

## 证据

- 资产从 Release 下载到空目录后用 `shasum -a 256` 实算，与锁逐值一致。
- 本地 `cargo test -p agent24-comm`、`agent24d` 的 hyphae/comm/components 用例通过。两条计时用例在机器高负载（load 96）下各失败一次，单独重跑后通过。
- 复现比对交给 CI 的 hyphae reproduce 两个任务。
