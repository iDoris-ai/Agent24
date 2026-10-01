# Release checklist — v0.5.1（DEP-A8，专用清单，冻结版）

> 本清单只管 v0.5.1 这一次发布，逐项精确到文件名。对应 Deployment 计划
> `docs/Deployment/TASKS.md` 的 DEP-A8；上游依赖 DEP-A1（#607）、DEP-A3（#608/#609/#619）、
> DEP-A5（#602）、DEP-A6（#617）已全部合入 main。构建与打包方式沿用
> `.github/workflows/release.yml`（DEP-A3）与 `.github/workflows/release-desktop.yml`
> （DEP-A6）两条流水线，本清单不引入新脚本，只验证它们的产出。
>
> v0.5.0 的历史清单见 `docs/RELEASE-CHECKLIST-v0.5.0.md`；更早的通用发布流程
> （安装器、"mother test"）见 `docs/RELEASE-CHECKLIST.md`（v0.1.0 版，仍适用于桌面 dmg，
> 但 v0.5.1 本版不发布 dmg，见下）。

## 0. 发布物清单（精确到文件名）

**tag**：`v0.5.1`

### CLI（4 个目标，`release.yml` / DEP-A3 产出）

- `agent24-0.5.1-macos-arm64.tar.gz`
- `agent24-0.5.1-macos-x64.tar.gz`
- `agent24-0.5.1-linux-x64.tar.gz`
- `agent24-0.5.1-linux-arm64.tar.gz`
- `SHA256SUMS`（对上面 4 个 tar.gz 的校验值，`release.yml` 的 `release` job 生成）

每个 tar.gz 解包后顶层一个目录，目录内**恰好**两项：

```
agent24-0.5.1-<os>-<arch>/
agent24-0.5.1-<os>-<arch>/agent24
agent24-0.5.1-<os>-<arch>/agent24d
```

### 桌面端（Linux only，`release-desktop.yml` / DEP-A6 产出，追加到同一个 Release）

命名已在 CI 实测确认（`gh run view` 读取 #617 对应 run 的日志，`electron-builder` 的
`building target=... file=release/...` 行；apps/desktop/package.json 的 `build.linux`
未设 `artifactName`，AppImage 走 electron-builder 默认 `${productName}-${version}.${ext}`；
`build.deb.artifactName` 显式写成 `agent24-desktop_${version}_${arch}.${ext}`）：

- `Agent24-0.5.1.AppImage`
- `agent24-desktop_0.5.1_amd64.deb`
- `SHA256SUMS-desktop`（对上面两个文件的校验值，`release-desktop.yml` 的 `publish` job
  单独生成，**不与** `SHA256SUMS` 合并——M5 处置，两条流水线不抢着创建/覆盖同一个文件）

**agent24-os-sdk / agent24-os-fd**：仍是 `0.1.0`，版本线独立于 Agent24 主版本，本次不发新
tag、不产出新发布物。

## 1. 机械断言：Release 中不得出现任何 macOS 桌面包（.dmg / .zip）

来源：#599 评审建议 —— 未签名的 dmg 在 Sequoia 上打开很麻烦，v0.5.1 不对外发布，留到
v0.5.2 签名后再发（见 `docs/Deployment/TASKS.md` 阶段 A 开篇说明、阶段 B DEP-B2/B5）。

打 tag、两条流水线都跑完、Release 资产齐全之后，执行：

```bash
gh release view v0.5.1 --json assets -q '.assets[].name' | grep -E '\.(dmg|zip)$' && exit 1
echo "OK: 未发现 dmg/zip"
```

grep 命中任何一行（即存在 `.dmg` 或 `.zip`）就是断言失败，必须停下排查（最可能是
`apps/desktop/package.json` 的 `build.mac.target` 被误触发，或有人手动上传了 dmg）。
`exit 1` 保证脚本化调用这条命令时能感知失败，而不是静默通过。

## 2. 发布命令前断言（逐条执行，任一失败即停）

- [ ] **tag 不存在**：`git rev-parse -q --verify refs/tags/v0.5.1` 必须失败。
- [ ] **工作区干净**：`git status --porcelain` 空输出。
- [ ] **HEAD == 已审批 SHA**：`git rev-parse HEAD` 的输出必须等于本次发布 PR 被批准
      （APPROVED）时评审所见的那个合并提交 SHA。不匹配就停下，不得对着一个评审没看过的
      commit 打 tag。
- [ ] **两条流水线都绿**：打 tag 推送后，`gh run list --workflow=release.yml` 与
      `gh run list --workflow=release-desktop.yml` 对应 `v0.5.1` 的 run 都是
      `completed` / `success`。`release-desktop.yml` 的 `publish` job 会轮询等待
      `release.yml` 先创建 Release（最多 30 分钟），两者都绿之后再继续下一步。
- [ ] **`shasum -c` 两份校验文件都通过**：
      ```bash
      gh release download v0.5.1 -D /tmp/a24-v0.5.1-verify
      cd /tmp/a24-v0.5.1-verify
      shasum -a 256 -c SHA256SUMS
      shasum -a 256 -c SHA256SUMS-desktop
      ```
      两条命令的每一行都必须是 `OK`。
- [ ] **每个 CLI 包里恰好是 `agent24` 和 `agent24d`**：
      ```bash
      for f in agent24-0.5.1-*.tar.gz; do
        echo "== $f =="
        tar -tzf "$f" | sort
      done
      ```
      每个包的清单必须是「一个顶层目录 + 目录下 `agent24` + `agent24d`」，不多不少。
- [ ] **本节「1. 机械断言」的 dmg/zip 检查已跑过且通过**。

## 3. 干净机验收（DEP-A8 验收，只用 Release 资产，不从源码构建、不用本地 checkout）

### 3.1 Mac mini（arm64）

`ssh jason@100.107.243.106`（tailscale）：

1. **浏览器下载一次**（会带 quarantine 标记）：按 Release Notes 里给出的步骤，从
   `github.com/iDoris-ai/Agent24/releases/tag/v0.5.1` 页面用浏览器下载
   `agent24-0.5.1-macos-arm64.tar.gz` + `SHA256SUMS`，校验、解压、按 Release Notes 的
   Gatekeeper 处理步骤（如需要）运行 `agent24 daemon start`，确认 `/health` 返回 200。
   记录是否触发 Gatekeeper 拦截、触发后如何处理。
2. **curl 下载一次**：同一版本改用 `curl -L` 下载，同样校验 + 解压 + 运行，对比两种下载
   方式的 quarantine 行为差异（curl 下载的文件通常不带 `com.apple.quarantine` 属性，
   `xattr -p com.apple.quarantine <file>` 可核实）。
3. 两种方式的真实命令输出都记录进发布 PR / Release notes。

### 3.2 Linux x64（Ubuntu 22.04 或 Debian 12）

1. **CLI 包**：下载 `agent24-0.5.1-linux-x64.tar.gz` + `SHA256SUMS`，`shasum -a 256 -c`
   通过后解压，`./agent24 daemon start`，确认 `~/.agent24/daemon.json` 里的 `port` 对应
   `curl http://127.0.0.1:<port>/api/v1/health` 返回 200 且 `version` 为 `0.5.1`。
2. **deb 包**：下载 `agent24-desktop_0.5.1_amd64.deb` + `SHA256SUMS-desktop`，校验通过后
   `sudo dpkg -i agent24-desktop_0.5.1_amd64.deb`（缺依赖用 `sudo apt-get install -f`
   补齐），确认 `dpkg -L agent24-desktop` 列出的文件里含 `agent24d`，桌面图标可正常启动
   （若无图形环境，至少验证 `dpkg -L` 内容与 CI 冒烟一致）。
3. **AppImage**（可选在同一台机器上做，图形环境要求见 Release Notes）：`chmod +x
   Agent24-0.5.1.AppImage`，按 Release Notes 的 libfuse2 / `--appimage-extract-and-run`
   指引运行，确认 sidecar `/health` 返回 200。

### 3.3 Intel mac（x64）

只靠 CI runner（`macos-15-intel`）做的自检冒烟为准（`release.yml` 的
「Self-check — daemon start / health / stop」job），**不做干净机实机验收**——
Release Notes 里必须明确标注「x64（Intel）未实机验证，仅 CI runner 冒烟通过」。

## 4. Release Notes 必须写明的内容

- **最低 glibc / 发行版版本**：Linux 两个目标分别在 `ubuntu-22.04`（x64）与
  `ubuntu-22.04-arm`（arm64）上构建，glibc 基线对应 Ubuntu 22.04（约 2.35）；更老的发行版
  （如 Ubuntu 20.04、Debian 10）未验证，可能因 glibc 版本过低无法运行。
- **AppImage 依赖**：需要系统安装 `libfuse2`（多数现代发行版默认不再预装）；没有 FUSE
  时改用 `./Agent24-0.5.1.AppImage --appimage-extract-and-run`；沙盒受限环境（如容器内）
  可能还需要加 `--no-sandbox`。
- **`agent24 service` 仅支持 macOS**：Linux/其他平台上该子命令不可用，守护进程管理走
  `agent24 daemon start/stop` 或发行版自己的 systemd（本版未提供 systemd unit，见
  `docs/Deployment/TASKS.md`「不在本计划内」一节）。
- **macOS 桌面端本版未发布**：v0.5.1 不提供 `.dmg` / `.zip` 桌面安装包（未签名的 dmg 在
  Sequoia 上体验差，见上「1. 机械断言」）；macOS 用户目前只能用 CLI 包，或自行从源码构建
  桌面端。签名后的 macOS 桌面端计划随 v0.5.2 发布（`docs/Deployment/TASKS.md` 阶段 B）。
- **F4b 入站执行默认冻结的迁移说明**（COMM-5a，#613/#618，详见 CHANGELOG `[0.5.1]`）：
  升级后 Nostr 入站消息不再自动触发 `agent24d` run，即使配置了
  `A24_NOSTR_ALLOWED_NPUBS`；需要临时恢复旧行为可设置 `A24_NOSTR_F4B_INBOUND=1`，但这是
  过渡开关，后续入站执行将改走 T01-E 的高层授权。

## 5. Sin90 / Cos72 —— jason 2026-10-01 已定：同步发多平台包

DEP-A4 已完成：Sin90 #76、Cos72 #11（`package.sh --target` + tag 触发的四平台 release workflow），发布条件 tag 门禁跟进见 Cos72 #12；Sin90 锁文件跟进 #77。jason 决定随 v0.5.1 同步升级模块版本：

| 仓库 | 版本 | 版本号 PR | tag | 产物 |
|---|---|---|---|---|
| Sin90（iDoris-ai/Sin90） | 0.5.0 → **0.5.1** | 待 #77 合并后提 | `v0.5.1` | `sin90-0.5.1-{macos,linux}-{arm64,x64}.tar.gz` + `SHA256SUMS` |
| Cos72（MushroomDAO/Cos72） | 0.1.0 → **0.1.1** | #13 | `v0.1.1` | `cos72-0.1.1-{macos,linux}-{arm64,x64}.tar.gz` + `SHA256SUMS` |

- [ ] 两个仓库的版本号 PR 合并后各自打 tag；各仓库 release workflow 全绿
- [ ] 每个模块包 `tar -tzf` 恰为 `<name>/domain-os.yml` 与 `<name>/bin/<name>`；`shasum -a 256 -c SHA256SUMS` 通过
- [ ] 干净机验收时，CLI 装好后用同一平台的模块包 `agent24 os install <目录>`，`agent24 os list` 两者 `[mounted]`

本 PR（Agent24 v0.5.1 发布 PR）不修改 Sin90 / Cos72 仓库。

## 6. 回滚

发布后如需撤回：**删除 GitHub Release 与对应 tag**（人工操作，不自动化）：

```bash
gh release delete v0.5.1 -R iDoris-ai/Agent24 --yes
git push --delete origin v0.5.1
```

不做「发新 patch 覆盖」——回滚就是真的把发布物撤回，需要重发时按本清单重新走一遍。若已有
用户下载了 v0.5.1 的 CLI 包并执行过 `agent24 os install`，注意 F4b 行为变化（见第 4
节）不会随回滚自动撤销——回滚只影响能下载到的制品，不影响已经升级过的本地状态；需要让
用户知晓的，走 Release Notes 的更新说明，而不是依赖回滚。

## 7. 发布记录（2026-10-01）

- **tag**：Agent24 `v0.5.1` @ `4fb5a89`（#624 合并提交）；Sin90 `v0.5.1` @ `030ae09`（#78）；Cos72 `v0.1.1` @ `51187da`（#13）。四条 release workflow 全绿（Agent24 CLI run 36822473922、desktop 36822473867；Sin90 36822490692；Cos72 36822504118）。
- **断言**：Agent24 Release 资产恰为 4 个 CLI 包 + `Agent24-0.5.1.AppImage` + `agent24-desktop_0.5.1_amd64.deb` + `SHA256SUMS` + `SHA256SUMS-desktop`；**无 `.dmg`/`.zip`**；两份 `shasum -c` 全 OK；每个 CLI 包恰为 `agent24`/`agent24d`。Sin90/Cos72 各 4 包 + `SHA256SUMS`，`shasum -c` OK，包内恰为 `domain-os.yml` + `bin/<name>`。
- **注意**：#621（COMM-1b）早于 #624 合入 main，故 tag 包含 COMM-1b 而原 CHANGELOG 未列；已在 Release Notes 与本次 CHANGELOG 补记。
- **干净机验收**：
  - Mac mini（arm64）**curl 下载**：三包 `shasum -c` OK → `os install` ×2 → `daemon start` → `os list`：`sin90 0.5.1 [mounted]`、`cos72 0.1.1 [mounted]`；`/api/v1/health` → `{"version":"0.5.1"}`。✅
  - Mac mini **模拟浏览器下载**（手动加 `com.apple.quarantine`）：未签名二进制被 Gatekeeper 拒绝（`spctl` rejected，首次启动挂起等待 GUI 确认）——符合预期。按 Release Notes 执行 `xattr -dr com.apple.quarantine` 后，在**无 GUI 的 ssh 会话**中启动仍挂起（推测是首次拦截的系统对话框仍待确认），**该路径在无头环境下未能完成验证**，需在有 GUI 的 Mac 上人工复核一次。v0.5.2 签名公证后此问题消失。⚠️
  - Linux x64（`ubuntu:22.04` 容器，amd64）：CLI + Sin90 + Cos72 `shasum -c` OK → 非 root 用户 `os install` ×2 → `daemon start` → 两者 `[mounted]`，health `0.5.1` ✅；deb `apt-get install ./agent24-desktop_0.5.1_amd64.deb` 成功，含 `/opt/Agent24/resources/backend/agent24d` ✅。
  - Intel macOS：仅 CI runner 冒烟（daemon 起、health 200），Release Notes 已标注「x64 未实机验证」。
