# Agent24 多平台发布：任务拆分（v2，已吸收 Opus 评审）

> 背景与选型见 [`RESEARCH.md`](RESEARCH.md)，执行顺序见 [`README.md`](README.md)，评审处置见文末。
> 分工：**Opus 5.5** 负责规划、设计、验收（设计评审最多 1 轮即冻结）；**Sonnet** 负责写代码，最多 3 个并行；**PR-Daemon** 负责评审。
> 沿用 Agent24 惯例：
> - 一个 task 对应一个分支、一个 PR；只修真 bug；
> - 不动 `pnpm-lock.yaml`（除非任务明确要求）；绝不 `git add -A`；
> - **不改 sidecar-host / Open Design 线的 PR 与设计**；
> - 标「用户」的动作只能由 jason 本人做。
>
> 状态：`BACKLOG` · `READY` · `IN_PROGRESS` · `BLOCKED` · `DONE`

## 阶段 A：现在就能做（无外部依赖）→ v0.5.1

v0.5.1 的范围：CLI 覆盖 macOS arm64/x64、Linux x64/arm64；桌面端只发 Linux（AppImage + deb）。
**v0.5.1 当时的计划是 macOS dmg 留到 v0.5.2 签名后再发——这条已被 Owner 2026-10-05 的裁决推翻**：
未签名不是不发 dmg 的理由，见下面 DEP-A9/DEP-B2 和阶段 B 的说明。

| ID | 任务 | 依赖 | 执行 | 规模 | 状态 |
|---|---|---|---|---|---|
| DEP-A1 | CI 增加 macOS：Rust 的 fmt/clippy/test 扩成 `ubuntu-latest` + `macos-latest` 矩阵；不与 sidecar 线的 `sidecar-windows.yml` 重复 | — | Sonnet | S | `DONE`（#607） |
| DEP-A3 | CLI 发布流水线：手写 `.github/workflows/release.yml`，**只在 `v[0-9]+.[0-9]+.[0-9]+` tag 上触发**；4 个目标在各自的原生 runner 上构建（Linux 用 `ubuntu-22.04` 与 arm runner，Intel mac 用现行 Intel runner 标签）；包的布局和命名沿用 v0.5.0：`agent24-<ver>-<os>-<arch>.tar.gz`，内含 `agent24` 与 `agent24d`；产出 `SHA256SUMS`；**由本流水线创建 Release**。cargo-dist 已推迟，见 C7 | — | Sonnet | M | `DONE`（#608、#609、#619） |
| DEP-A4 | Sin90 / Cos72 多平台包（跨仓库，2 个 PR）：`package.sh` 去掉只允许 `Darwin arm64` 的限制，加 `--target`；各自新增 tag 触发的发布 CI，布局不变 | A3（命名） | Sonnet ×2 | M | `DONE`（Sin90 #76 + #77 锁文件；Cos72 #11 + #12 tag 门禁） |
| DEP-A5 | CLI daemon 与桌面端 sidecar 复用：桌面端启动时先读 `daemon.json` 并检查 health，有 daemon 就直接连；托盘的停止/重启只作用于桌面端自己拉起的 daemon；最小改动，不与 sidecar-host 设计分叉 | — | Sonnet | M | `DONE`（#602） |
| DEP-A6 | 桌面端 Linux 构建：新增 `release-desktop.yml`，在 `ubuntu-22.04` 上出 AppImage + deb，**把附件追加到 A3 创建的 Release 上**（不自行创建 Release）；冒烟测试用 xvfb 加 `--appimage-extract-and-run`，并检查 sidecar 的 `LD_LIBRARY_PATH` 污染；同时补 deb 目标配置 | — | Sonnet | M | `DONE`（#617） |
| DEP-A7 | Windows 移植设计：`docs/design/WINDOWS-PORT.md`。候选方案对比：Windows AF_UNIX + 句柄继承 / 命名管道 / 回环 + token；复用 sidecar-host ProcessKit；给出所有 Unix 专有代码的处置清单（20 个文件 + proxy.rs + `state_dir` 的 HOME + `command-fds`/`close_fds` + `same_device`）；分阶段计划。摸底用 **`windows-latest` 上 `workflow_dispatch` 触发的 `cargo check --workspace --keep-going`** | — | Opus 设计 + Sonnet 摸底 | M | `DONE`（#610、#611） |
| DEP-A8 | v0.5.1 清单与发布：`docs/RELEASE-CHECKLIST-v0.5.1.md` 冻结命名；打 tag 触发 A3/A6；干净机器验收 | A1, A3–A6 | Opus 清单 + Sonnet | M | `DONE`（#624；tag `v0.5.1`@4fb5a89，Sin90 `v0.5.1`@030ae09，Cos72 `v0.1.1`@51187da；验收见 `docs/RELEASE-CHECKLIST-v0.5.1.md` 末节） |
| DEP-A9 | Owner 裁决（2026-10-05）：Open Design 改为按需下载组件，不再打进安装包（曾把 AppImage 从 146MB 顶到 934MB、deb 从 101MB 顶到 622MB）。`pack-open-design-component.mjs` 产出 tarball + 校验 manifest；`open-design-component.ts` 是运行时安装器（下载/校验 sha256/解压/校验 contract/原子落地到 `~/.agent24/components/open-design/`）；`CreativeServeWeb` 消费已安装目录，未安装时报 `needs-download`；Design 页首次点开才提示下载 | A6 | Sonnet | M | 见 PR（本次） |
| DEP-A10 | macOS 版 Open Design 按需组件：调研推翻了最初「需要新写 mac 专用资源脚本」的判断——`tools-pack dev mac build --to app`（映射到 electron-builder 的 `dir` target）产出的 `Contents/Resources` 下已经天然是 `app/ open-design/ open-design-web-standalone/` 这套和 Linux 完全一致的布局（`afterPack` 钩子把 Next standalone web 拷进 `open-design-web-standalone`，跟 win 走同一套；`asar:false` 让 `app/` 保持可读写松散目录）。本机对**真实构建产物**跑过 `stage:open-design` / `rebuild:open-design-native` / `pack:open-design` 三个脚本，全部不改代码直接复用成功，产出真实 darwin-arm64 tarball（约 277MB）。本机 arm64 **真机闭环**（本地 HTTPS 服务器 serve 真 tarball + 真实打包的 Agent24.app，不是 mock）额外挖出两个单元测试测不出来的真 bug，已修：① `assertSafeTarMembers` 的 `tar -tzf` 列表在真实 `open-design-web-standalone` 体量下超过 Node 默认 1MB stdout 缓冲区；② fork 自己的 `agent24-headless.cjs` 有一条「`resourceRoot` 必须在 `runtimeExecutable` 推出的安全目录下面」的校验，按需下载装到 app 包外面直接撞上。**原先的修法（在 app 自己的 resourcesPath 下放一个指向真实装好目录的符号链接）被 Opus 复审判定为阻塞问题并推翻**：AppImage 只读挂载下是 EROFS，deb `/opt` 安装是 EACCES，签名 mac .app 会被破坏，多开有竞态——见 DEP-A11 | A9 | Sonnet | M | `DONE`（见 PR #671，真机闭环已验证；符号链接修法见 DEP-A11 更新） |
| DEP-A11 | Opus 复审 #670/#671（REQUEST_CHANGES）收口：**C1（阻塞）**真正修法在 fork 侧——`iDoris-ai/open-design-agent24` 新分支 `agent24/explicit-resource-safe-base`（PR [#7](https://github.com/iDoris-ai/open-design-agent24/pull/7)，未合并），`agent24-headless.cjs` 配置新增显式 `resourceSafeBase` 字段、协议升到 v2（absolute + realpath 自洽 + resourceRoot 在其下 + POSIX uid/mode 校验）；Agent24 侧删除 `ensureOnDemandComponentLink`，直接用 `fs.realpathSync(componentStatus.dir)` 作为 `resourceSafeBase`，组件目录改 0700。**pin 暂不改**（等 fork PR 合并后统一升级），但代码+测试已就位。**H1** tar 加固测试改手写 ustar 头（不再依赖 `tar -s`，两端行为不一致有silent-pass 风险），并断言归档里真的有 `..` 成员。**H2** `assertSafeTarMembers` 的 `once(child,'exit')` 在排空 stdout 的 for-await 循环之后才注册，真实压测（系统高负载）下 tar 常在循环读完前就已退出，监听器注册晚了永久等不到——改成 spawn 后立刻注册。**H3** mac x64 签名顺序颠倒（先 `--mac dmg` 后签名，签的是没打进 dmg 的那份副本）：改成 `--mac dir` → `codesign --force --deep -s -` → `electron-builder --prepackaged <app> --mac dmg`，冒烟测试加 `codesign --verify --deep --strict`。**M1** `tar -tzf` 改 spawn+readline 流式处理，不再用固定 maxBuffer。**M2** 新增 CI 闭环冒烟（`ci-on-demand-smoke.mjs`）：真实走 `OpenDesignComponentInstaller` 装进真实默认目录 `~/.agent24/components/open-design/`，再用真实打包的 Electron 二进制（`ELECTRON_RUN_AS_NODE=1`）跑通到 `/api/ready`——用本地 fixture launcher 代替 fork 真实的 `agent24-headless.cjs`（因为官方 pin 还停在协议 v1，测的是 Agent24 自己这侧的装载/spawn/协议解析/轮询，不是 fork 的协议实现，后者由 fork PR 自己的测试覆盖）。这个真机闭环额外挖出一个真 bug：`resourceRoot` 用未 realpath 的 `resourcesPath` 拼，`resourceSafeBase` 却是 realpath 过的，装在经过符号链接的路径下（本机复现于 macOS `/var/folders`）时两者不一致导致必然被拒——已修（两者统一锚定到同一个已解析的 base），补了回归测试。**L1** 「installer 里不能有 .tar.gz」检查去掉 `-maxdepth 1`。**L2** mac 打包 tarball 加 `COPYFILE_DISABLE=1` + `--no-mac-metadata --no-acls --no-xattrs`，避免 AppleDouble 元数据污染体积与确定性 | A9, A10 | Sonnet | M | `DONE`（#670 head `0380bf7`、#671 已合并 #670；fork PR #7 待合并——见 DEP-A12 的二次复审） |
| DEP-A12 | fork PR #7（`agent24/explicit-resource-safe-base`）二次复审：Opus 对上一轮的 resourceSafeBase 实现又发了 REQUEST_CHANGES（H1/H2/M1-M5 全新一轮，不是 DEP-A11 那轮的重复）——resourceRoot===base 的边界漏洞、resourceRoot 从未 realpath、祖先链只查了 base 自己没查全链、win32 完全不校验、dataRoot/runtimeRoot 和资源树重叠——全部修了；修完用 `codex exec` 连续挑战三轮（本机 codex-cli 0.160.0，ChatGPT 登录）：第一轮挑出 3 个 High（sticky 位连文件和 base 自身都豁免了、验证用的路径和真正 spawn 用的路径是两份独立计算互相脱节、TOCTOU 快照只查 base 自己且只比对 inode）+2 个 Medium，全修；第二轮又挑出 1 个 High（TOCTOU 复查只查端点不查链上中间目录、只比 dev/ino 不比 uid/mode）+3 个 Medium（`startsWith("..")` 误伤 `..bundle` 这种合法目录名、`realpathOfNearestExistingAncestor` 把任何 realpath 报错都当成"还没创建"连悬空符号链接都放过、M3 交叠检查只查了 3 个原始字段没查派生路径），全修；第三轮确认无新 High/Critical，2 个 Medium 完全解决、2 个 Medium 部分解决后记成模块顶部的已知限制（sidecars.ts 的 openLog 没有逐个校验它实际开文件的子目录；这个文件和 daemon-paths.ts 对同一路径可能各自归一化出不同字符串）——这两条要修都得动这个文件以外的代码，不在这次范围内，诚实记录而不是装作关严。fork 侧 apps/packaged 全量测试 365/365 通过。Agent24 侧（#670）配套改动：安装器把 `~/.agent24/components` 及其 `open-design` 子目录显式 0700（不依赖 mkdirSync 的 mode 选项或宿主 umask）；装完整棵组件树 chmod 成只读（目录 0500/文件 0444），重装前有专门的恢复可写步骤；装的时候把 4 个会被执行的入口文件（`agent24-headless.cjs`+3 个 entry）sha256 记进 marker，`creative-serve-web.ts` 在 spawn 前重新算一遍校验，并且自己也做一遍祖先链 ownership/mode 检查（"自检不是边界"——fork 自己的检查跑在已经 spawn 出去的进程里，严格晚于宿主决定要不要 spawn 它）。apps/desktop 全量测试 367/367 通过。**pin 仍然不改**，等 fork PR #7 真正合并后再统一升级 | A11 | Sonnet | M | `DONE`（fork PR #7 head `ed60d6d237`，未合并；#670 head `7e66710`、#671 已合并，head `baf04bd`，CI 跑着） |

原来的 DEP-A2（Linux 黑盒）**已删除**：`a3_2b` / `a3_3` 黑盒没有标 `#[ignore]`，现有 ubuntu CI 每次都在跑，重做一遍证伪不了任何东西。真正的新内容是「Sin90/Cos72 真实二进制在 Linux 上挂载」，已并入 A4 和 A8 的验收。

**验收标准（每条都能执行，且能证伪）**
- **A1**：`macos-latest` job 为绿。反向验证：临时加一条 `#[cfg(target_os="macos")] assert!(false)` 的测试，该 job 会变红，确认后删掉。
- **A3**：在 fork 或测试仓库推送测试 tag，产出 4 个 tar.gz 和 `SHA256SUMS`，`shasum -c` 通过。每个包 `tar -tzf` 恰好是 `agent24`、`agent24d`，并且：
  - `file` 显示的架构与包名一致；
  - 在对应 runner 上解压后执行 `agent24 daemon start`，`/health` 返回 200；
  - 推送 `agent24-os-sdk-v9.9.9` 形式的 tag **不会**触发流水线（反向验证）。
- **A4**：每个目标的包恰好含 `domain-os.yml` + `bin/<name>`，`file` 显示的架构正确；在 Linux runner 上 `agent24 os install` 之后 `os list` 显示 `[mounted]`。
- **A5**：先 `agent24 daemon start` 再启动桌面端，`pgrep -c agent24d == 1`，桌面端显示「外部 daemon」；点托盘「停止」后，CLI 起的 daemon 仍存活。反过来先启动桌面端、再执行 `agent24 daemon start`，会提示「已在运行」并退出 0，不报错。
- **A6**：Release 上多出 AppImage 和 deb。xvfb 冒烟时 sidecar 的 `/health` 返回 200，且 sidecar 进程环境里没有 AppImage 注入的 `LD_LIBRARY_PATH`（或者已确认无害并记录原因）。deb 能在 `ubuntu-22.04` 上 `dpkg -i` 安装并启动。
- **A7**：设计文档经 1 轮评审后冻结；Windows runner 上的 check 错误按 crate 分类写进文档。
- **A8**：清单中的断言全部通过。干净机器验收覆盖三处，都只用 Release 资产：Mac mini（arm64）；一台 Linux（x64，Ubuntu 22.04 或 Debian 12）；Intel mac 在 Intel runner 上做冒烟测试，没有条件就在 Release Notes 里标「x64 未实机验证」。Release Notes 写明：
  - 最低 glibc / 发行版；
  - AppImage 需要 libfuse2 或 `--appimage-extract-and-run` / `--no-sandbox`；
  - `agent24 service` 仅支持 macOS。

## 阶段 B：Apple 开发者账号到位后 → v0.5.2（签名 + 公证）

> **Owner 裁决（2026-10-05）**：没有 Apple 开发者账号不是不发 dmg 的理由。DEP-B2 拆成两半——
> **未签名 dmg 现在就发**（见下面「DEP-B2a（已实现）」，走 `release-desktop.yml` 新增的
> `build-macos` job，`CSC_IDENTITY_AUTO_DISCOVERY=false` + ad-hoc `codesign --force --deep
> --sign -`，首次打开走右键「打开」或 `xattr -dr com.apple.quarantine`）；**只有 notarytool
> 公证**这一步仍然卡账号（见 DEP-B2b）。

| ID | 任务 | 依赖 | 执行 | 规模 | 状态 |
|---|---|---|---|---|---|
| DEP-B1 | 证书与密钥：导出 Developer ID Application 证书（p12）和 App Store Connect API key，存入仓库 Secrets | 账号 | **用户** | S | `BLOCKED`（账号申请中） |
| DEP-B2a | 未签名 macOS dmg：`release-desktop.yml` 新增 `build-macos` job（`macos-14` arm64 + `macos-15-intel` x64 矩阵），`electron-builder --mac dmg` + `CSC_IDENTITY_AUTO_DISCOVERY=false`，ad-hoc 签名让 app 能在本机架构启动；冒烟挂载 dmg 校验 `codesign -dv`、跑内置 `agent24d`、`/health`、断言包内没有 `open-design` 树（Open Design 在 mac 上还是按需组件，见 A10）。**H3（DEP-A11）纠正**：原顺序先 `--mac dmg` 后签名——签的是没打进 dmg 的那份副本，dmg 里实际装的是签名前的拷贝。改成 `--mac dir` → `codesign --force --deep -s -` → `electron-builder --prepackaged <app> --mac dmg`，冒烟测试加 `codesign --verify --deep --strict` 确认挂载出来的 app 真的通过签名校验 | A5, A9 | Sonnet | M | `DONE`（本次 PR，本机 arm64 实测：144MB dmg，启动+health 200；签名顺序修复见 DEP-A11） |
| DEP-B2b | notarytool 公证 + staple：开启 hardened runtime + entitlements；`mac.binaries` 显式列出 `agent24d` | B1, B2a | Sonnet | M | `BLOCKED`（账号） |
| DEP-B3 | CLI 和模块二进制签名 + 公证：`agent24`/`agent24d`、`bin/sin90`、`bin/cos72` 用 codesign 签名，打成 zip 送公证，接入 A3/A4 的流水线 | B1, A3, A4 | Sonnet | M | `BLOCKED` |
| DEP-B5 | v0.5.2 发布：**公证**的 dmg 和 CLI；用**浏览器下载**（带 quarantine 标记）做干净机器验收 | B2b, B3 | Opus + Sonnet | S | `BLOCKED` |

**验收标准**
- **B2a**（已验证，本次 PR）：本机 arm64 `electron-builder --mac dmg` + ad-hoc 签名；`codesign -dv`
  显示 `Signature=adhoc`；挂载 dmg 后打开 app，内置 `agent24d` 的 `/api/v1/health` 返回
  200；`Contents/Resources` 下没有 `open-design`/`open-design-web-standalone`。CI 侧 x64
  （`macos-15-intel`）跑法相同，尚未在真实 CI 上跑过（见 PR 风险说明）。
- **B2b**：`spctl -a -vv Agent24.app` 输出 `accepted` 且 `source=Notarized Developer ID`；`codesign --verify --deep --strict` 通过；`codesign -dv` 能看到嵌套的 `agent24d` 已签名，且开启了 hardened runtime；`lipo -archs` 与 dmg 的架构一致。
- **B3**：用 Safari 下载 → Finder 双击解压 → 在终端执行 `agent24 --version`，不出现 Gatekeeper 拦截；daemon 能拉起模块并挂载。另用 `tar` 解压的方式再测一遍，记录 quarantine 行为。
- **B5**：在干净机器上浏览器下载 dmg，双击后首次打开，不出现「无法验证开发者」或「已损坏」。

## 阶段 C：依赖其他条件（设计、账号、决策、较大的里程碑）

| ID | 任务 | 依赖 | 执行 | 规模 | 状态 |
|---|---|---|---|---|---|
| DEP-B4→C0 | 自动更新：electron-updater，需要 mac `zip` 目标；用 fork 或 prerelease 通道验证，不污染正式更新通道 | B2b | Sonnet | M | `BLOCKED` |
| DEP-C1 | Windows 签名方式：jason 核实能否申请 Azure Trusted Signing（Artifact Signing），不能就改买 OV/EV 证书 | 用户决策与费用 | **用户** | S | `BLOCKED` |
| DEP-C2 | Windows 移植实现：按 A7 冻结的设计分片实现；CI 加 `windows-latest` | A7 冻结；sidecar-host 线的落地情况 | Sonnet（分片） | L | `BLOCKED` |
| DEP-C3 | Windows 发布：release.yml 加 `x86_64-pc-windows-msvc`；桌面端出 NSIS 安装包并签名 → v0.6 | C1, C2 | Sonnet | M | `BLOCKED` |
| DEP-C4 | daemon 远程访问设计：`docs/design/REMOTE-ACCESS.md`，内容包括配对、令牌、TLS/tailscale/Nostr relay 选型、威胁模型，以及与现有 loopback 鉴权的关系 | 用户确定移动端方向 | Opus 设计 | M | `BACKLOG` |
| DEP-C5 | 移动端设计：**MVP 是 PWA**（由 daemon 或桌面端提供，经 tailscale 访问）；原生外壳（Capacitor 或 RN）排在之后，renderer 需要把 `window.agent24.*` 抽象成可替换的传输层 | C4 | Opus 设计 | M | `BACKLOG` |
| DEP-C6 | 移动端实现（先 PWA；上架应用商店另行立项） | C5 | Sonnet | L | `BACKLOG` |
| DEP-C7 | 重新评估 cargo-dist、`curl \| sh` 安装脚本、Homebrew tap（tap 仓库和 PAT 需要用户动手） | 有需求时 | Opus | S | `BACKLOG` |
| DEP-C8 | Hyphae lock 升级：把 `rust/crates/agent24-comm/hyphae.lock.json` 升到含 daemon 锁（`.hyphae/daemon.lock`）的 Hyphae 版本（#104 之后），通过 Hyphae lock verify（来源：`docs/design/COMM-HYPHAE.md` §9 G3） | — | Sonnet | S | `BACKLOG` |
| DEP-C9 | 发布包加入 `hyphae` 二进制（macOS / Linux），打包后在干净机器上验证 `agent24 comm` 可用（来源：`COMM-HYPHAE.md` §9 R4） | C8 | Sonnet | S | `BACKLOG` |

**验收标准**
- **C2**：`windows-latest` 上 Rust 全量测试通过；在 Windows 上 Sin90 能挂载，并能收到调度回调。
- **C3**：安装包带有效的 Authenticode 签名，`signtool verify /pa` 通过；在干净的 Windows 机器上安装后，模块显示 `mounted`。SmartScreen 信誉需要时间积累，不作为验收条件。
- **C4/C5**：设计经 1 轮评审后冻结，jason 确认功能范围。

## 不在本计划内
- Mac App Store 上架：沙盒机制与拉起模块冲突。
- 在手机上本地运行 daemon 或模型。
- Linux 包的 GPG 签名、Flatpak/Snap、Linux 服务注册（systemd），等有需求再做。

## 附：Opus 评审处置（2026-09-30，1 轮，结论 CHANGES → 已全部处置并冻结）

| 发现 | 处置 |
|---|---|
| H1 未提及 sidecar-host / Open Design 线 | RESEARCH §1.7 新增；A5 限定最小改动、不分叉；A7 把它列为复用基础；A1 不与 sidecar CI 重复 |
| H2 cargo-dist 的产物数量、export-schema、tag 误触发、Homebrew 需要用户动手 | v0.5.1 改为手写矩阵（A3），沿用现有布局；加 tag 过滤和反向验证；cargo-dist/Homebrew 移到 C7 |
| H3 共存是确定冲突（单例锁），A6→A5 是人为串行 | RESEARCH §1.6 改写；A5 改为实现复用并给出可证伪的验收；删除 A6→A5 依赖 |
| H4 未签名 dmg 的 Sequoia 路径、x64 包装进 arm64 sidecar、curl 绕过 quarantine | v0.5.1 不发 dmg；B2 要求原生 runner + `lipo`；B3/B5 改用浏览器下载验收 |
| M1 Unix 清单漏项；Windows AF_UNIX 路线 | RESEARCH §1.4 和 §2 已补；A7 列入候选 |
| M2 在 macOS 上 cargo check Windows 目标跑不通 | A7 改在 `windows-latest` 上跑 |
| M3 A2 与现有 CI 重复 | 删除 A2，内容并入 A4/A8 |
| M4 glibc、AppImage 的 fuse/sandbox、LD_LIBRARY_PATH、service 在 Linux 上的行为 | RESEARCH 风险表已补；A3/A6/A8 的验收和 Release Notes 已覆盖 |
| M5 两条流水线抢着创建 Release | A3 负责创建，A6 只追加附件 |
| M6 Intel 没有实机 / 没有 runner | A3 用 Intel runner；A8 做不到实机就明确标注 |
| M7 A4 规模低估 | 改为 M，注明跨仓库 2 个 PR |
| M8 自动更新需要 zip、测试会污染通道、B5 被卡住 | B4 移到 C0，用 prerelease 通道；v0.5.2 只做签名 |
| M9 SmartScreen 判据无法证伪；Trusted Signing 的申请资格 | C3 改用 `signtool verify`；C1 请 jason 核实资格 |
| M10 移动端先做商店 App 过重 | C5 的 MVP 改为 PWA |
| Low：`macos-13` 已退役、Android 限制的理由、renderer 复用程度、quarantine 行为不确定 | 已在 RESEARCH 相应位置修正或标注「不确定，实测定」 |
