# Release checklist — v0.6.0-preview.20261028.1（预发布）

> 只管这一次预发布。结构沿用 `docs/RELEASE-CHECKLIST-v0.5.1.md`。四条线（kernel、documenting、
> weknora、memory-p1；第五条 iDoris 线见 #1370，可能不赶版）的内容和迁移见 `CHANGELOG.md` 的 `[0.6.0-preview.20261028.1]`。
> 通用流程（安装器、"mother test"）见 `docs/RELEASE-CHECKLIST.md`。
>
> **这是预发布，不是 0.6.0 正式版。** 清单里的版本号是 `0.6.0`；`-preview.20261028.1` 只出现在
> tag 和发布物文件名里，与 `v0.5.1-preview.20261016.2` 同做法（那次清单版本号一直是 `0.5.1`）。

## 0. 发布物与命名

**tag**：`v0.6.0-preview.20261028.1`（不匹配 `release.yml`、`release-desktop.yml` 的
`v[0-9]+.[0-9]+.[0-9]+` 触发条件，**推 tag 不会自动跑发布流水线**；发布物由人工构建并上传，
与 v0.5.1 预发布相同）。

| 文件 | 说明 |
|---|---|
| `Agent24-0.6.0-preview.20261028.1-arm64.dmg` | macOS arm64 桌面包（核心包；Design 等组件按需下载） |
| `agent24-0.6.0-preview.20261028.1-macos-arm64.tar.gz` | CLI：解包后顶层一个目录，里面恰好 `agent24`、`agent24d` |
| `SHA256SUMS.20261028.1` | 以上文件的校验值 |

Linux、Intel mac 不在本次预发布范围。dmg 文件名以实际构建输出为准，构建后回填本表。

## 1. 前置条件（任一不满足就不往下走）

- [ ] 四条线都已合入 main，顺序为 kernel #1280 → memory-p1 #1341 → documenting → weknora。
      `gh pr list -R iDoris-ai/Agent24 --state merged --base main --limit 20` 能看到这四个 release PR。
- [ ] `ab/idoris` 已进 main（第五线，排在最后；截止 2026-10-28 12:00 Asia/Bangkok，见 #1370）。**赶不上则本版不含 iDoris，
      配对改下一个 preview**，不等；此时第 4 节的 iDoris 用例组（ID-1…ID-8）、第 5.1 节的 iDoris 项和 Release Notes 的 iDoris 项一并划掉。
- [ ] 本分支（`chore/release-0.6.0-preview-prep`）已吸收 main，**合并后重新核对版本号**：
      ```bash
      git grep -n '^version = "0.5.1"' -- '*Cargo.toml'; git grep -n '"version": "0.5.1"' -- '*package.json'
      ```
      必须无输出。四条线里新增的 crate（`agent24-knowledge`、`agent24-documents-client` 等）按各自分支的
      写法会带着 `0.5.1` 进来，要在吸收 main 之后补升到 `0.6.0`，再 `cd rust && cargo update -w`。
- [ ] 迁移目录连续：store `0001–0029`、memory `0001–0020`、documents `0001–0008`，且
      `agent24d/knowledge_migrations/0001_log.sql` 存在。
      ```bash
      ls rust/crates/agent24-store/migrations | tail -3   # 0027–0029
      ls rust/crates/agent24-memory/migrations | tail -2  # 0019–0020
      ls rust/apps/agent24-documents/migrations | tail -2 # 0007–0008
      ```
- [ ] main 上 CI 全绿：`gh run list -R iDoris-ai/Agent24 --branch main --limit 10` 对 HEAD 的 run 都是
      `completed / success`。
- [ ] 本地门禁：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、
      `cargo test --workspace --no-fail-fast`、`pnpm install --frozen-lockfile`（lockfile 不应改动）、
      `pnpm typecheck`、`pnpm lint`、`pnpm gen:api` 无漂移。
- [ ] 本 release 准备 PR 经 PR-Daemon APPROVE，且 HEAD == 被批准的 SHA
      （`git rev-parse HEAD`）；工作区干净；tag 不存在
      （`git rev-parse -q --verify refs/tags/v0.6.0-preview.20261028.1` 必须失败）。
- [ ] 用例验收（第 4 节）已通过，结果贴进发布 PR 正文。

## 2. 构建

在已吸收 main、HEAD 等于已审批 SHA 的干净 checkout 里构建，记下 `git rev-parse HEAD`
写进 Release Notes（"构建来源"）。

**桌面 dmg（macOS arm64）**

```bash
pnpm install --frozen-lockfile
pnpm --filter @agent24/desktop build:mac     # build:rust → node-daemon → build → electron-builder --mac
ls apps/desktop/release/*.dmg
```

拿到的 dmg 重命名为 `Agent24-0.6.0-preview.20261028.1-arm64.dmg`（`package.json` 的 version 是
`0.6.0`，预发布后缀靠文件名带）。

**CLI（macOS arm64）**

```bash
cd rust && cargo build --release -p agent24-cli -p agent24d
NAME=agent24-0.6.0-preview.20261028.1-macos-arm64
mkdir -p /tmp/a24-pre/$NAME && cp target/release/agent24 target/release/agent24d /tmp/a24-pre/$NAME/
( cd /tmp/a24-pre && tar -czf $NAME.tar.gz $NAME && tar -tzf $NAME.tar.gz | sort )
```

`tar -tzf` 的清单必须是「一个顶层目录 + `agent24` + `agent24d`」，不多不少。

**校验值**

```bash
shasum -a 256 -- *.dmg *.tar.gz > SHA256SUMS.20261028.1 && shasum -a 256 -c SHA256SUMS.20261028.1
```

要多平台 CLI 产物时，可在 main 上手动触发 `release.yml`（`workflow_dispatch`，`dry_run=true` 只上传
artifact、不建 Release）作为交叉验证，但本次预发布的发布物以人工构建的 arm64 为准。

## 3. 签名与公证（已裁决：签名 + 公证，走仓库内流水线）

jason 2026-10-27 拍板：预览版也必须 Developer ID 签名并公证。流水线由 DEP-B2/B3 补齐（PR #1353，分支
`ci/dep-b-sign-notarize`，本机 arm64 已真跑通过：dmg 公证 submission `dbf61f9f-dee9-4356-81d5-92d7e6a677f9`
Accepted；CLI zip 公证 `015302f0-9e66-4adf-836f-eaa2c92f2f93` Accepted）。**#1353 必须先合入 main，本节才能执行。**

本机前置（已核实存在）：
- 签名身份 `Developer ID Application: Huifeng Jiao (HYAP444FN9)`（`security find-identity -v -p codesigning`）
- notarytool keychain profile `apple-notary-HYAP444FN9`（`xcrun notarytool history --keychain-profile apple-notary-HYAP444FN9` 能列出历史）

执行（细节见 `docs/Deployment/MACOS-SIGNING.md`）：
```bash
# 桌面 dmg：build:rust(release) → build → electron-builder --mac arm64 → notarytool → staple → 验证
apps/desktop/scripts/release-mac.sh            # AGENT24_SIGN_IDENTITY / AGENT24_NOTARY_PROFILE 可覆盖默认
# CLI：签名 agent24 / agent24d → zip → 公证 → SHA256SUMS
scripts/sign-cli-mac.sh
# 独立验证（挂载只读副本 → 复制 → 卸载 → codesign/spctl/lipo）
scripts/verify-mac-signing.sh <dmg>
```

- [ ] `verify-mac-signing.sh` 输出 `accepted` 且 `source=Notarized Developer ID`；`codesign -dv` 可见内嵌 `backend/agent24d` 已签且 Hardened Runtime；`lipo -archs` 为 arm64。
- [ ] dmg 与 CLI 的 submission id、`spctl`/`codesign` 实测输出贴进 Release Notes，不照抄上一版。
- [ ] 已知限制写进 Release Notes：CLI 裸二进制不能 staple，离线机器首次运行需联网核验；Intel（x64）未实测、本次不发；GitHub Actions 侧签名仍需 DEP-B1 的 Secrets（本次在 jason 本机出包）。

## 4. 用例验收

按 #1293 合入后的规则，release PR 的合并门槛是 PR-Daemon APPROVE + 完整用例验收（本规则在 #1293 合入
前以 jason 同意为准）。用例清单、命令和预期结果在：

**`docs/release/ACCEPTANCE-v0.6.0-preview.md`**（由另一位写手负责，本清单只引用路径）。

- [ ] 该文件已存在于待发布的 HEAD 上，且逐条跑过：全新数据目录、真实 daemon、通过 CLI 按用户操作顺序走。
- [ ] 跑过的用例、命令和结果贴进发布 PR 正文；缺口（用户看不到的改动、纯界面展示）逐项写明。
- [ ] **升级路径**至少跑一次：用 `v0.5.1-preview.20261016.2` 留下的真实库（schema 17）复制到干净 HOME 启动，
      不设 `AGENT24_DATA_DIR`，`/api/v1/health` 返回 200，迁移走到 store `0029` / memory `0020`。
- [ ] 验收机若跑过 `ab/kernel` 或 `ab/memory-p1` 的开发版，先清库或用 `AGENT24_DATA_DIR` 隔开（见 CHANGELOG
      「升级注意」）。

## 5. 打 tag 与 GitHub 预发布

只有第 1–4 节全部勾完、第 3 节签名验证通过之后才执行。

```bash
# 1) tag 打在已审批 SHA 上
git fetch origin && git checkout main && git pull --ff-only
test "$(git rev-parse HEAD)" = "<已审批 SHA>" || { echo "HEAD 不是已审批 SHA"; exit 1; }
git tag -a v0.6.0-preview.20261028.1 -m "Agent24 v0.6.0-preview.20261028.1"
git push origin v0.6.0-preview.20261028.1

# 2) 建预发布（--prerelease，不是 --latest）
gh release create v0.6.0-preview.20261028.1 \
  -R iDoris-ai/Agent24 --prerelease --verify-tag \
  --title "Agent24 v0.6.0-preview.20261028.1 · ARM64 预览" \
  --notes-file <release-notes.md> \
  Agent24-0.6.0-preview.20261028.1-arm64.dmg \
  agent24-0.6.0-preview.20261028.1-macos-arm64.tar.gz \
  SHA256SUMS.20261028.1

# 3) 发布后核对
gh release view v0.6.0-preview.20261028.1 -R iDoris-ai/Agent24 --json isPrerelease,assets -q '.isPrerelease, (.assets[].name)'
mkdir -p /tmp/a24-pre-verify && gh release download v0.6.0-preview.20261028.1 -R iDoris-ai/Agent24 -D /tmp/a24-pre-verify
( cd /tmp/a24-pre-verify && shasum -a 256 -c SHA256SUMS.20261028.1 )
```

**Release Notes 必写项**（格式参考 `v0.5.1-preview.20261016.2` 的说明）：

- 构建来源（main 的 SHA）和构建号 `20261028.1`；资产表和 SHA-256。
- 签名与公证状态，严格按第 3 节实测结果写。
- 四条线各自的要点，以及 **kernel 的行为变化**（授予 POST 默认关、MCP/ACP 只看自成一体的 run、无制品身份的
  旧授予按 Stale 拒绝、已知局限 #1290）。
- **数据库升级不可回退**：迁移无 `.down.sql`，旧二进制遇到已记录的新迁移会拒开；升级前自行复制
  `agent24.db` / 各记忆库。开发机若跑过 memory-p1 改号前的版本，需要重置本地库。
- **iDoris：temperature 透传行为变化（见 #1370）**：调用方设置了 `temperature` 时也会发给 iDoris 网关，默认不设就不发。同时写明本期不做项（共同 launcher / Admin token FD 交接、自动安装模型、`idoris-any`、流式），不得宣称。
- 未验收项：不要把没跑的东西写成已通过。

### 5.1 发布后的跨仓库动作（联合发布）

- [ ] **AgentEar 联合发布（issue #1347）**：宿主侧 vendored 契约须已重钉到 AgentEar v0.28.0（`0b6409ba`，PR `chore/agentear-vendor-v0.28.0`）并随本版进 main。发布后在 #1347 回帖：本版 tag、main 提交 SHA、dmg 与 CLI 包的 sha256。在此之前不把任何版本称为联合发布的宿主侧。
- [ ] **iDoris 联合发布（issue #1370）**：发布后在 #1370 回帖 tag、main 提交 SHA、dmg 与 CLI 包的 sha256；Release Notes 引用 iDoris v0.3.1 的 tar sha256（`idoris-v0.3.1-aarch64-apple-darwin.tar.gz`，以 #1370 回填为准）；双方 Release 说明互相链接。
- [ ] **Hyphae（issue #1346）**：若 #1352（锁升到 v0.27.0）随本版进 main，发布后通知 Hyphae 更新 `.github/joint-ci-baseline.json`（先例 Hyphae #281），并核对其交接文档里标「待确认」的三项（Agent24 实际锁、内部文件布局、daemon 生命周期）。
- [ ] **Brood**：`orgs/auraai/INTERFACES.md` 里 Hyphae 版本（现写 v0.25.0）与 Agent24 版本行更新为本版。
- [ ] `docs/PROGRESS-2026-10-26.md`（跟踪文档分支）补发布记录。

## 6. 回滚

预发布出问题时撤回发布物（人工操作，不自动化）：

```bash
gh release delete v0.6.0-preview.20261028.1 -R iDoris-ai/Agent24 --yes
git push --delete origin v0.6.0-preview.20261028.1
git tag -d v0.6.0-preview.20261028.1
```

- 不做「发新 patch 覆盖」；需要重发就改构建号（`20261028.2`）按本清单重新走一遍，不复用已撤回的 tag。
- 回滚只影响能下载到的发布物，**不回退已经升级过的本地库**：迁移不可回退，旧二进制会拒绝打开升级过的库。
  已升级的用户要还原就得用升级前自己复制的 `agent24.db` 备份；这一点写进撤回公告，不要指望回滚本身解决。
- 版本号和 CHANGELOG 不回退（`0.6.0` 是下一个正式版本的预备值，下一次预发布仍用它）。
