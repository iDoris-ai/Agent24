# Agent24 多平台发布：调研与可行性分析

> 日期：2026-09-30 · 基线：`main@4c236bc`（v0.5.0 已发布 + #594）
> 作者：规划（Opus 5.5）· 执行：Sonnet 子代理（≤3 并发）· 评审：PR-Daemon
> 本文只陈述事实与判断；任务拆分见 [`TASKS.md`](TASKS.md)，顺序见 [`README.md`](README.md)。

## 1. 现状（全部来自仓库实查）

### 1.1 已发布的东西

| Release | 附件 |
|---|---|
| v0.1.0 | `agent24-v0.1.0-macos-arm64`、`agent24d-v0.1.0-macos-arm64`（裸二进制） |
| v0.2.0 / v0.2.1 / v0.3.0 | 无附件 |
| v0.4.0 / v0.5.0 | `agent24-<ver>-macos-arm64.tar.gz` + `SHA256SUMS`（`agent24` + `agent24d`） |
| Sin90 v0.5.0 / Cos72 v0.1.0 | `<name>-<ver>-macos-arm64.tar.gz` + `SHA256SUMS`（`domain-os.yml` + `bin/<name>`） |

- **桌面端（Electron）从未发布过。** 所有发布都是在本机手工构建、手工 `gh release create`，没有 CI 发布流水线。
- 只有 macOS arm64 一个平台。

### 1.2 桌面端打包配置（已有，但从未跑进发布）

`apps/desktop/package.json`：
- 构建命令：`build:mac` / `build:win` / `build:linux` 都是 `build:rust → node-daemon build → vite build → electron-builder --<os>`。
- 打包目标：mac 出 `dmg`，win 出 `nsis`，linux 出 `AppImage`。
- **daemon 已作为 sidecar 打进 App**：`extraResources` 把 `rust/target/release/agent24d(.exe)` 放进 `Resources/backend/`。桌面端启动时由 `src/main/backend-manager.ts` 执行 `spawn(agent24d, ['serve','--port','0'])`，环境变量原样继承。
- **没有**签名、公证、自动更新（electron-updater）的配置；linux 目标只有 AppImage（没有 deb）；公共 `extraResources` 还会带上 `packages/node-daemon/dist`（node 后备后端，硬依赖 `@boxlite-ai/boxlite-darwin-arm64`）。
- 上一次成功打出 dmg 是 2026-07-24（v0.1.0 时期），之后再没跑过。

结论：**装了桌面端，就同时装好了 daemon**；缺的只是 `agent24` 命令行。

### 1.3 CI

`.github/workflows/ci.yml` 中 4 个 job 全部是 `runs-on: ubuntu-latest`：
- Linux x64 一直在持续编译和测试，这是现成的 Linux 可行性证据；
- macOS 只在开发者本机验证过；
- Windows **从未编译过**。

### 1.4 Unix 专有代码

Rust 中直接使用 `std::os::unix` 的非测试文件共 **20 个**：

| 位置 | 文件 | 用途 |
|---|---|---|
| `agent24-os-proto` | `launch.rs` `supervise.rs` `module.rs` `endpoint.rs` `supervisor.rs` | 进程外模块：UDS 回调 socket、fd 3（`A24_LISTEN_FD`）继承、进程组 |
| `agent24-os-fd` | `lib.rs` | 模块侧取继承的 listener（唯一 `unsafe`） |
| `agent24-os-packages` | `install.rs` `discovery.rs` `lib.rs` | 权限位、可执行位 |
| `agent24d` | `attach_listener.rs` `attached.rs` `server.rs` `lifecycle.rs` `domain.rs` `os_config.rs` `os_routes.rs` | A3 附着 socket、0600 权限、peer_cred 鉴权、信号 |
| 其他 | `agent24-cli/service.rs`、`agent24-protocol/state_file.rs`、`agent24-memory/reconcile.rs`、`agent24-tools/local.rs` | launchd 服务、状态文件权限、shell_exec |

另外还有：`os-proto/src/proxy.rs` 的 `tokio::net::UnixStream`；`state_file::state_dir()` 只读 `HOME`，Windows 上没有这个变量；`agent24-os-proto` 无条件依赖只能在 Unix 上用的 `command-fds`、`close_fds`。

`cfg(not(unix))` 分支只有 5 处，而且并不都是权限降级：
- `server.rs:966`：信号处理；
- `state_file.rs:244`：检查 pid 是否存活；
- `install.rs:416`：`same_device` 返回 None，会让 Windows 上的 `os install` 直接被拒。

**进程外模块机制（ADR-031）和 A3 附着（ADR-032）没有任何 Windows 实现。**

### 1.5 本地模型

- `agent24-models` 默认的本地调用链是 **oMLX(8088) → Ollama(11434)**，都走同一个 `OpenAiCompatProvider`，也兼容 LM Studio 和远端 API。
- oMLX 只能跑在 Apple Silicon 上。
- Linux、Windows、Intel Mac 走 Ollama 这条路，**代码层面已经支持**，但没有在这些平台上实测过。

### 1.6 已知未验证点

- **CLI daemon 与桌面 sidecar 共存（确定冲突，不是可能冲突）**：非 ephemeral 模式的 daemon 有单例锁（`server.rs:1000-1014`），第二个起来的会报 `another agent24d is already running` 然后退出。
  - 先起 CLI 再起桌面端：sidecar 起不来，BackendManager 进入重启循环。
  - 先起桌面端再起 CLI：`daemon start` 失败。
  - 要解决必须**实现复用**：桌面端读 `daemon.json` 连接已有的 daemon，且托盘「停止/重启」不能杀掉不是它启动的 daemon。

### 1.7 正在进行的 sidecar-host 线（Open Design，尚未合入 main）

约 40 个开放 PR（#291、#517–#554 等）正在做 `rust/apps/agent24-sidecar-host`：由 Rust 负责桌面 sidecar 的生命周期边界，用 ProcessKit 实现 Windows Job Object 和 pipes，#553 已经在 `windows-latest` 上编译并测试。它和本计划有三处重叠：
- 桌面端 daemon 生命周期（A5）；
- 安装包里的二进制组成（A6）；
- Windows 的进程与管道机制（A7、C2）。

**本计划不改它的设计**：A5 只做最小改动（复用检测），A7 把它列为 Windows 的复用基础。那条线属于其他线的 PR，这里不动。

## 2. 各平台可行性

| 平台 | 判断 | 理由 | 主要工作 | 工作量 |
|---|---|---|---|---|
| macOS arm64 | ✅ 现在（dmg 等签名） | 已发布 CLI；dmg 配置现成 | CI 化；签名与公证（等开发者账号） | 小 |
| macOS x64（Intel） | ✅ 现在 | Rust 交叉编译即可；模型走 Ollama | 加构建目标；在 Intel 上冒烟测试 | 小 |
| Linux x64 / arm64 | ✅ 很快 | CI 已在 Linux 上全量测试；Unix API 全部可用 | CLI 包 + AppImage/deb；sidecar 在 Linux 上的实测；模块黑盒测试在 Linux 上跑 | 小～中 |
| Windows x64 | ⚠️ 需要一个里程碑 | 模块传输依赖 UDS 与 fd 继承，Rust std 在 Windows 上没有 `UnixStream`。可选路线：Windows 10 1803 起支持的 AF_UNIX（`uds_windows` 等 crate）加 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` 句柄继承，可能让改动小很多；另外可以复用 sidecar-host 线的 ProcessKit | 设计「模块传输跨平台抽象」（命名管道，或回环 + token）；SDK / shell_exec / 路径 / 服务注册适配；签名 | 中～大 |
| iOS / Android | ❌ 不能装完整版 | 系统禁止常驻后台 daemon、禁止拉起子进程（模块机制跑不起来），内存也跑不了 35B 模型 | 只能做**遥控 App**：连接家里的 Mac/Mac mini 上的 daemon，前提是 daemon 支持远程访问（配对、TLS/tailscale） | 大，要先做设计 |
| Mac App Store | ❌ 不走 | App Sandbox 不允许拉起任意子进程或模块 | 走 Developer ID 直接分发 | — |

## 3. 打包最佳实践（选型）

### 3.1 CLI / daemon / 模块：v0.5.1 手写 Actions 矩阵（cargo-dist 已评估，推迟）

cargo-dist（axodotdev `dist`）的情况：
- 由 `dist init` 生成 `.github/workflows/release.yml`：推送 tag 时，在各 OS 的 runner 上矩阵构建，自动产出归档（`.tar.xz`/`.zip`）+ 校验和 + `dist-manifest.json`，并创建 GitHub Release。
- 同时生成安装器：shell（`curl | sh`）、PowerShell、Homebrew tap，Windows 另有 msi。
- **与本仓库不契合的地方（评审发现）**：
  - `agent24` 和 `agent24d` 分属两个 package，dist 会拆成 2×4 个归档，用户要装两次。而 CLI 要在同目录或 PATH 里找到 `agent24d`，拆开后容易装不全。
  - `agent24-protocol` 里的 `export-schema` 二进制会被当成发布物。
  - 仓库里已有 `agent24-os-sdk-v0.1.0`、`v0.4.0-m4` 这类 tag，dist 默认会被它们误触发。
  - Homebrew tap 需要单独建仓库并配 PAT，这得用户来做。
- 目标 triple：`aarch64-apple-darwin`、`x86_64-apple-darwin`、`x86_64-unknown-linux-gnu`、`aarch64-unknown-linux-gnu`；Windows 之后再加 `x86_64-pc-windows-msvc`。
- PR 上只跑 `dist plan`（`pr-run-mode = "plan"`），不发布。
- **与现状的差异**：归档命名会从 `agent24-0.5.0-macos-arm64.tar.gz` 变成 cargo-dist 的 `agent24-cli-aarch64-apple-darwin.tar.xz` 这类格式，需要在清单里重新冻结命名。Sin90 / Cos72 的 `os install <目录>` 不受影响，因为只要解压出来的目录布局不变就行。
- **决定（评审后）**：v0.5.1 **先手写 GitHub Actions 发布矩阵**，保持现有「一个 tar 包里放 `agent24` + `agent24d`」的布局和命名，大约 30 行 YAML，只由 `v*.*.*` tag 触发。cargo-dist、安装脚本、Homebrew 以后需要时再评估（放进 C 阶段）。这样最符合控制范围的原则。

### 3.2 桌面端：electron-builder（已在用）
- 各 OS 在各自的 runner 上构建（跨平台打包不可靠）：macOS 用 `macos-latest`（arm64），Intel 用现存的 Intel runner（`macos-13` 已退役，开工时查现行标签）；Linux 用 `ubuntu-22.04`（glibc 基线更低）出 AppImage + deb；Windows 用 `windows-latest` 出 NSIS。
- **架构陷阱**：`extraResources` 写死了 `rust/target/release/agent24d`。如果在 arm64 runner 上用 `--x64` 打包，x64 包里会装进 arm64 的 sidecar，而且不报错。所以每个架构都要在对应 runner 上构建，并用 `lipo -archs`/`file` 校验。
- **macOS 签名与公证**：需要 Developer ID Application 证书 + App Store Connect API key（notarytool），打开 hardened runtime，配 entitlements（Electron 需要 `allow-jit` 等）。**被打进 App 的 `agent24d` 也要单独签名并带 hardened runtime**，否则公证会被拒。最后 staple。
- **Windows 签名**：候选是 Azure Trusted Signing（可能已改名 Artifact Signing；个人身份验证据说只对美国和加拿大开放，**需要 jason 核实能否申请**）或传统 OV/EV 证书。无论哪种都**不能保证** SmartScreen 首发就不拦，信誉要慢慢积累。
- **Linux**：不需要签名；可选发布 SHA256 与 GPG 签名。
- **自动更新**：electron-updater 读 GitHub Release（`latest-mac.yml` 等）。macOS 上**必须已签名**，并且要额外出 `zip` 目标，因为 Squirrel.Mac 用的是 zip。
- **未签名 dmg 的用户路径**：macOS 15 Sequoia 起，右键 → 打开**已经不能**绕过 Gatekeeper，必须去「系统设置 → 隐私与安全性 → 仍要打开」，常见提示还是「已损坏」，得执行 `xattr -cr`。**所以 v0.5.1 不发 dmg**，等签名后在 v0.5.2 发。

### 3.3 下载的二进制与 Gatekeeper
浏览器下载的文件会被打上 quarantine 标记，未签名的二进制第一次运行会被 Gatekeeper 拦下。影响范围：
- CLI 的 `agent24` / `agent24d`；
- Sin90 / Cos72 的 `bin/<name>`：daemon 拉起模块时同样会被拦。

用 `curl` 下载不会打这个标记，这也是这次 Mac mini 验收没遇到问题的原因。所以**开发者账号到位后，CLI 和模块二进制也要签名并公证**（zip 形式公证），不只是 dmg。裸二进制公证后没法 staple，第一次运行要联网校验。

另外，daemon 执行一个带 quarantine 标记的模块时，Gatekeeper 具体怎么处理、用 `tar` 解压会不会继承 quarantine，**目前还不确定**，B3 会用实测来定。

### 3.4 移动端（远期）
- **MVP 先做响应式 Web/PWA**：远程访问（C4）做好后，由 daemon 或桌面端提供，手机通过 tailscale 访问，不需要应用商店，先验证真实需求。
- 之后再做原生外壳：优先 **Capacitor**，React 组件能复用；但 renderer 大量依赖 `window.agent24.*` 这个 preload IPC（光 `backendProxy` 就有 29 处），需要加一层传输抽象。其次是 React Native / Expo。
- Android 的真实限制是 W^X：API 29 起不能执行下载到数据目录里的模块二进制。它本身允许前台服务，也能执行 APK 内自带的原生二进制，所以这条不影响「先做遥控」的结论。
- 前置依赖：daemon 远程访问设计（配对、令牌、TLS，或走 tailscale / Nostr relay），以及 Apple / Google 开发者账号与商店审核。

## 4. 风险

| 风险 | 缓解 |
|---|---|
| Linux glibc 基线：在 ubuntu-24.04 上构建的二进制无法在 22.04 / Debian 12 上运行 | 在 `ubuntu-22.04` 上构建，Release Notes 注明最低发行版 |
| AppImage 在 Ubuntu 24.04 上因 AppArmor 限制 userns 导致沙箱崩溃；22.04 以上默认没有 libfuse2 | 冒烟测试使用 `--appimage-extract-and-run`，Release Notes 写明 `--no-sandbox` 或安装 libfuse2 的方法；deb 包不受影响 |
| AppRun 注入的 `LD_LIBRARY_PATH` 被 `agent24d` 继承，再传给模块和 shell_exec | A6 冒烟时检查；发现污染就在 spawn 前清掉 |
| `agent24 service install` 在 Linux 上直接调用 `launchctl` | 本期不支持 Linux 服务注册，CLI 给出明确报错，Release Notes 注明 |
| macOS x64 没有实机验证 | 用 Intel runner 做冒烟测试；做不到就在 Release 中标注「x64 未实机验证」 |
| Linux 上模块二进制挂载失败 | A 阶段用真实的 Release 包在 Linux 上做干净机器验收（socket 路径上限 103 按 macOS 取值，Linux 是 108，不构成问题；现有黑盒测试已经在 ubuntu CI 上跑） |
| CLI daemon 与桌面 sidecar 冲突（单例锁，确定会失败） | A5 实现复用：读 `daemon.json` 并检查 health；不杀非自己启动的 daemon；不与 sidecar-host 设计分叉 |
| macOS 公证因嵌套二进制未签名被拒 | 用 `mac.binaries` 显式列出 `agent24d`；公证前先用 `codesign --verify --deep --strict` 自检 |
| Windows 移植范围失控 | 先交设计文档，冻结后再开工；本轮不承诺 Windows 功能完整 |
