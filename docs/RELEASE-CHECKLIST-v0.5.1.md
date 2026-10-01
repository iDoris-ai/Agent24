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

## 5. Sin90 / Cos72 —— 待 jason 决定，本 PR 不改这两个仓库

Deployment 计划的 DEP-A4（Sin90 / Cos72 多平台包）目前仍是 `BACKLOG`，**尚未实现**：两个
仓库的 `scripts/package.sh` 还没有去掉 `Darwin arm64` 限制、也没有新增 tag 触发的发布 CI。
是否要在 v0.5.1 这次一并升级模块版本号，留给 jason 决定，列出两个选项：

- **选项 A（推荐，若 DEP-A4 未就绪）**：本次只发布 Agent24 v0.5.1（CLI + 桌面 Linux），
  Sin90 / Cos72 版本号与发布节奏保持不变，等 DEP-A4 落地后再一起发多平台包。
- **选项 B**：同步升级 Sin90 → `0.5.1`、Cos72 → `0.1.1`，但要先完成 DEP-A4（`package.sh`
  加 `--target`、各自新增发布 CI），否则这两个仓库仍然只能产出 `Darwin arm64` 单一目标，
  版本号升级不带来实际的多平台能力。

无论选哪个选项，**本 PR（Agent24 v0.5.1 发布 PR）不修改 Sin90 / Cos72 仓库**。

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
