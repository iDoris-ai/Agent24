# Hyphae 固定二进制升级记录（2026-11-02，v0.30.0，首个签名版 / schema 2）

## 结果

- Agent24 基线：`01f49e3a`（origin/main，#1549）。
- Hyphae 来源：[`v0.30.0`](https://github.com/iDoris-ai/Hyphae/releases/tag/v0.30.0)，提交 `0e0d4ea46d78675ed3f5c9e02ddc146398860edb`。旧锁为 v0.29.0 `560d2bd103d0b7e87c8bd038a0230503924d86ab`（schema 1）。
- 锁：`hyphae.lock.json` 与 Release 资产逐字节一致（`cmp` 通过，SHA-256 `55c4d493dacf60ecbfc4f42571aa5f140ab80c30dad4013acf6340eab8e2e1cf`），**schema 2**（ADR-038 D2 / §7.4）。`hyphae.release.json` 删除，release 位置与资产名改从锁读。
- Go：`go1.26.4`；配方新增 `-X main.version=v0.30.0`（ADR-038 §7.2）。

| 资产 | 锁里的 sha256 | 签名 | 复现目标 `reproducible.unsigned_sha256` |
|---|---|---|---|
| `hyphae-darwin-arm64` | `a9fbaef99b60f330b834c5eff58ed8e2af1a9ea902ae8f481c43ac2ffd3eb9a4`（签名后） | Developer ID `HYAP444FN9`，已公证，裸 Mach-O 无 staple | `865531f83fa76b4692fd47f73d352434d98f081271d3acf0d3081e9b327b863c` |
| `hyphae-linux-amd64`（键 `linux-x64`） | `b30fd252b04b19666a111bae0ffa5ab954ccca4a8166e033982a9ae3acd48607` | 无（平台无系统签名） | 同左 |

## 证据

- 资产从 Release 下载到空目录后 `shasum -a 256` 实算，与 `SHA256SUMS`、锁逐值一致。
- `spctl --assess --type install -vv hyphae-darwin-arm64` → `accepted`、`source=Notarized Developer ID`、Team `HYAP444FN9`。
- 本机按锁里的配方在 `0e0d4ea4` 复现：linux/amd64 `b30fd252…`、darwin/arm64 `865531f8…`，与锁一致。

## Agent24 侧改动（ADR-038 C1b）

- 锁用 `agent24-components::ComponentLock` 严格解析；非 schema 2 一律拒绝。`agent24-comm` 只嵌入（不能依赖带 reqwest 的 components，COMM-5b 边界），agent24d 解析。
- 安装（`VerifyMode::Install`）：sha256 + codesign 团队/Hardened Runtime + `spctl --type install` 公证；启动（`VerifyMode::AtRest`）：sha256 + 离线 codesign，不联网。Linux 只 sha256（显式 `SignatureCheck::UnsignedPlatform`）。
- `/api/v1/components` 的 Hyphae `contract` / `provides` 改从锁读，删除代码常量。
- CI：复现比对 linux→`assets.linux-x64.sha256`（且须等于 `unsigned_sha256.linux-x64`），darwin→`unsigned_sha256.darwin-arm64`；macOS 测试任务改用下载并校验过的签名资产。
