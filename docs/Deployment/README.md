# Deployment —— Agent24 多平台发布计划

> 2026-09-30 立项。jason 已同意的顺序：① v0.5.1 macOS + Linux；② v0.6 Windows；③ 远程访问 → 移动端遥控。
> 本轮**只做规划**。Opus 评审一轮（CHANGES，4H/10M 已全部处置）后**冻结**，然后按下面的批次开工。

| 文件 | 内容 |
|---|---|
| [`RESEARCH.md`](RESEARCH.md) | 现状实查（含 sidecar-host 线、daemon 单例锁）、各平台可行性、打包选型、风险 |
| [`TASKS.md`](TASKS.md) | A（现在就能做）/ B（Apple 账号到位后）/ C（依赖其他）三个阶段；每个任务写明依赖、执行者、规模、可证伪的验收；文末附评审处置表 |
| [`HYPHAE-NETWORK-DELIVERY.md`](HYPHAE-NETWORK-DELIVERY.md) | Hyphae 跨仓交付契约：标准包携带客户端，可选本机节点、离线包与服务器发行；当前缺口、双锁信任、安装/回滚、三种引导与验收。实现须等 Hyphae M1 发布门关闭，任务见 TASKS 的 DEP-HN 系列与既有 C8/C9；文档不代表交付完成 |

## 一句话结论

- **macOS 与 Linux 现在就能做**：主要是 CI 和打包工程，外加桌面端与 CLI 的 daemon 复用这一处真实代码改动。
- **Windows 需要一个里程碑**：进程外模块依赖 Unix socket 和 fd 继承。候选路线是 Windows AF_UNIX，再复用 sidecar-host 线的 ProcessKit。
- **iOS / Android 装不了完整版**：只能做遥控端，MVP 先做 PWA，前提是 daemon 支持远程访问。

## 三个阶段的评估

| 阶段 | 任务 | 外部依赖 | 规模 | 风险 | 产出 |
|---|---|---|---|---|---|
| **A 现在就能做** | A1 CI 加 macOS · A3 CLI 发布流水线 · A4 模块多平台包 · A5 daemon 复用 · A6 桌面端 Linux 包 · A7 Windows 设计 · A8 发布 | 无 | 7 个任务（其中 A4 跨两个仓库），约 1–1.5 周 | 中：Linux 的 AppImage 沙箱 / glibc，Intel 没有实机 | **v0.5.1**：CLI 覆盖 macOS arm64/x64 + Linux x64/arm64；桌面端 Linux AppImage + deb；**不发 macOS dmg** |
| **B 账号到位后** | B1 证书 · B2 dmg 签名与公证 · B3 CLI 和模块签名与公证 · B5 发布 | Apple 开发者账号 + jason 配置 Secrets | 4 个任务，账号到位后约 3–5 天 | 中：公证、嵌套二进制签名 | **v0.5.2**：签名版 macOS dmg + CLI，浏览器下载后双击即用 |
| **C 依赖其他** | C0 自动更新 · C1 Windows 签名 · C2/C3 Windows · C4 远程访问设计 · C5/C6 移动端（PWA 优先）· C7 cargo-dist/Homebrew | A7 冻结；Windows 签名资格与费用；移动端方向；sidecar-host 线落地情况 | Windows L（2–3 周）；移动端 L+ | 高：Windows 范围、远程访问的安全性 | **v0.6** Windows；移动端另立里程碑 |

## 工作顺序（Sonnet 同时最多 3 个）

```
第 1 批   A1 CI 加 macOS   ∥   A3 CLI 发布流水线   ∥   A5 daemon 复用
          Opus 同时做：A7 Windows 设计（摸底 check 并入第 2 批的 Sonnet 名额）
第 2 批   A4 Sin90/Cos72 多平台包（依赖 A3 定下的命名）   ∥   A6 桌面端 Linux 包   ∥   A7 摸底
第 3 批   A8 v0.5.1 清单 → 打 tag 发布 → 三处干净机器验收
—— Apple 账号到位 ——
第 4 批   B1（jason 配置 Secrets）→ B2 dmg ∥ B3 CLI/模块 → B5 v0.5.2
—— A7 冻结 + C1 有结论 ——
第 5 批   C2 Windows 实现（分片）→ C3 Windows 发布 v0.6
—— jason 确定移动端方向 ——
第 6 批   C4 远程访问设计 → C5 PWA 设计 → C6 实现
```

## 用 `AGENT24_DATA_DIR` 跑隔离数据目录的实例（MAINT-03）

默认数据根是 `$HOME/.agent24`：`daemon.json`、`logs/`、`agent24.db`、`attach/`、tool workspace 都在它下面。
设置环境变量 `AGENT24_DATA_DIR`（非空的**绝对路径**）后，数据根就是这个目录本身（**不再拼 `.agent24`**）；
空值或相对路径会被忽略，回落到默认。不设置时行为与以前完全一致。daemon、CLI 和桌面端读的是同一口径
（`agent24_protocol::state_file::state_dir()` 与桌面端 `daemonStatePath()`），所以只要它们在同一个环境里启动，就会彼此找到。

> ⚠️ 覆盖目录必须是**专用目录**（新建或空目录）：daemon 启动时会对它执行 `create_dir_all` 并设为 `0700`，
> 然后往里写 `agent24.db`、`workspace/`、`os/`、`logs/`、`attach/` 等。不要指向 `$HOME`、`Documents` 这类已有目录。
> 另：相对路径或空值会被**静默**忽略并回落到 `~/.agent24`，写错不会报错。

典型场景：**预览包与开发环境共存**。开发构建（如 `ab/kernel` 线）可能已把 `~/.agent24/agent24.db` 迁移到更新的 schema，
main 基线的预览包再打开它会报 `migration N was previously applied but is missing`。让预览包用自己的目录即可互不破坏：

```bash
# macOS 预览包：从终端直接启动可执行文件，环境变量才会传进去（Finder/Dock 启动不会带）
AGENT24_DATA_DIR="$HOME/.agent24-preview" /Applications/Agent24.app/Contents/MacOS/Agent24

# 或者只跑 daemon / CLI
AGENT24_DATA_DIR=/tmp/a24-x agent24d serve
AGENT24_DATA_DIR=/tmp/a24-x agent24 daemon status
```

影响与注意：

- 隔离实例**看不到原数据**：会话、记忆、已装模块、`attach` 注册都在新目录里从零开始；原 `~/.agent24` 不会被迁移、复制或删除。
- 还有少数调用点没有跟随这个开关，会继续读写原 `~/.agent24`（因此覆盖目录**不是**完整隔离）：
  桌面端的 Open Design 组件（`apps/desktop/src/main/open-design-component.ts`，`~/.agent24/components`），
  以及 `nostr-bridge` / `wechat-bridge` 的 `daemon.json` 与 token（`packages/*/src/config.ts`）——
  它们会去连原始数据目录里的那个 daemon。
- 两个数据目录各有自己的 daemon 单例锁和 `daemon.json`，可以同时运行（端口由各自 daemon 决定）；
  但 CLI / 桌面端只会连上与自己 `AGENT24_DATA_DIR` 相同的那个 daemon。
- 想回到原数据，去掉这个变量重新启动即可；不需要也不应手动搬动 `agent24.db`。

## 需要 jason 决定或动手的事

1. **Apple 开发者账号**：到位后导出 Developer ID Application 证书和 App Store Connect API key，填进仓库 Secrets（B1，只能由你来做）。
2. **v0.5.1 不发 macOS dmg**（评审后改的建议）：从 macOS 15 起，未签名的 dmg 不能再用「右键 → 打开」放行，得去系统设置里手动放行，甚至要执行 `xattr -cr`，体验太差。所以 dmg 等签名后在 v0.5.2 发；v0.5.1 的 macOS 用户先用 CLI。
3. **Windows 签名**：Azure Trusted Signing 的个人验证据说只开放给美国和加拿大，请你先核实能不能申请；不能的话就改买 OV/EV 证书（C1，要花钱）。
4. **移动端方向**：遥控端要做哪些功能，远程访问走 tailscale、自建 relay 还是 Nostr。定下来之后再启动 C4。
