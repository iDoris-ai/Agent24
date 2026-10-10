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

v0.5.1 的范围：CLI 覆盖 macOS arm64/x64、Linux x64/arm64；桌面端只发 Linux（AppImage + deb）。**macOS dmg 不在本版发布**，因为未签名的 dmg 在 Sequoia 上打开很麻烦，留到 v0.5.2 签名后再发。

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

## 阶段 B：Apple 开发者账号到位后 → v0.5.2（只做签名）

| ID | 任务 | 依赖 | 执行 | 规模 | 状态 |
|---|---|---|---|---|---|
| DEP-B1 | 证书与密钥：导出 Developer ID Application 证书（p12）和 App Store Connect API key，存入仓库 Secrets | 账号 | **用户** | S | `BLOCKED`（账号申请中） |
| DEP-B2 | macOS 桌面端 dmg：在 arm64 和 Intel runner 上分别构建；开启 hardened runtime + entitlements；在 `mac.binaries` 里显式列出 `agent24d`；notarytool 公证 + staple | B1, A5 | Sonnet | M | `BLOCKED` |
| DEP-B3 | CLI 和模块二进制签名 + 公证：`agent24`/`agent24d`、`bin/sin90`、`bin/cos72` 用 codesign 签名，打成 zip 送公证，接入 A3/A4 的流水线 | B1, A3, A4 | Sonnet | M | `BLOCKED` |
| DEP-B5 | v0.5.2 发布：签名的 dmg 和 CLI；用**浏览器下载**（带 quarantine 标记）做干净机器验收 | B2, B3 | Opus + Sonnet | S | `BLOCKED` |

**验收标准**
- **B2**：`spctl -a -vv Agent24.app` 输出 `accepted` 且 `source=Notarized Developer ID`；`codesign --verify --deep --strict` 通过；`codesign -dv` 能看到嵌套的 `agent24d` 已签名，且开启了 hardened runtime；`lipo -archs` 与 dmg 的架构一致。
- **B3**：用 Safari 下载 → Finder 双击解压 → 在终端执行 `agent24 --version`，不出现 Gatekeeper 拦截；daemon 能拉起模块并挂载。另用 `tar` 解压的方式再测一遍，记录 quarantine 行为。
- **B5**：在干净机器上浏览器下载 dmg，双击后首次打开，不出现「无法验证开发者」或「已损坏」。

## 阶段 C：依赖其他条件（设计、账号、决策、较大的里程碑）

| ID | 任务 | 依赖 | 执行 | 规模 | 状态 |
|---|---|---|---|---|---|
| DEP-B4→C0 | 自动更新：electron-updater，需要 mac `zip` 目标；用 fork 或 prerelease 通道验证，不污染正式更新通道 | B2 | Sonnet | M | `BLOCKED` |
| DEP-C1 | Windows 签名方式：jason 核实能否申请 Azure Trusted Signing（Artifact Signing），不能就改买 OV/EV 证书 | 用户决策与费用 | **用户** | S | `BLOCKED` |
| DEP-C2 | Windows 移植实现：按 A7 冻结的设计分片实现；CI 加 `windows-latest` | A7 冻结；sidecar-host 线的落地情况 | Sonnet（分片） | L | `BLOCKED` |
| DEP-C3 | Windows 发布：release.yml 加 `x86_64-pc-windows-msvc`；桌面端出 NSIS 安装包并签名 → v0.6 | C1, C2 | Sonnet | M | `BLOCKED` |
| DEP-C4 | daemon 远程访问设计：`docs/design/REMOTE-ACCESS.md`，内容包括配对、令牌、TLS/tailscale/Nostr relay 选型、威胁模型，以及与现有 loopback 鉴权的关系 | 用户确定移动端方向 | Opus 设计 | M | `BACKLOG` |
| DEP-C5 | 移动端设计：**MVP 是 PWA**（由 daemon 或桌面端提供，经 tailscale 访问）；原生外壳（Capacitor 或 RN）排在之后，renderer 需要把 `window.agent24.*` 抽象成可替换的传输层 | C4 | Opus 设计 | M | `BACKLOG` |
| DEP-C6 | 移动端实现（先 PWA；上架应用商店另行立项） | C5 | Sonnet | L | `BACKLOG` |
| DEP-C7 | 重新评估 cargo-dist、`curl \| sh` 安装脚本、Homebrew tap（tap 仓库和 PAT 需要用户动手） | 有需求时 | Opus | S | `BACKLOG` |
| DEP-C8 | Hyphae lock 兼容晋升（保留原 ID）：原 daemon.lock / #104 要求继续验收；按下方网络交付里程碑核对当前锁与候选，补四平台 client 锁、独立 relay 锁与精确组合验证（来源：`COMM-HYPHAE.md` §9 G3） | HN0、HN1；Hyphae M1 发布门 | Sonnet | M（扩展验收） | `BACKLOG` |
| DEP-C9 | 标准包携带 `hyphae`（保留原 ID）：CLI/桌面 macOS / Linux 四平台内置精确 client，不含 relay；更新包布局断言并验收 `agent24 comm`（来源：`COMM-HYPHAE.md` §9 R4） | C8、HN2 | Sonnet | M（四平台） | `BACKLOG` |

**验收标准**
- **C2**：`windows-latest` 上 Rust 全量测试通过；在 Windows 上 Sin90 能挂载，并能收到调度回调。
- **C3**：安装包带有效的 Authenticode 签名，`signtool verify /pa` 通过；在干净的 Windows 机器上安装后，模块显示 `mounted`。SmartScreen 信誉需要时间积累，不作为验收条件。
- **C4/C5**：设计经 1 轮评审后冻结，jason 确认功能范围。

<a id="hyphae-delivery"></a>
### Hyphae 网络交付：DEP-HN（2026-10-10 新增，尚未实现）

规范见 [HYPHAE-NETWORK-DELIVERY.md](HYPHAE-NETWORK-DELIVERY.md)。这是 M1 之后的有界交付里程碑，不是 Hyphae M2；现在只允许文档/契约冻结，所有实现必须等待 Hyphae 当前 M1 发布门关闭。范围仅标准 client、可选本机 relay、离线包、服务器发行与三入口引导；远程执行、移动端、Windows、自动公网配置不在本轮。

**C8/C9 对账**：原编号、BACKLOG 与来源保留，原始 S 规模因四平台/双锁验收扩大为 M；不另建“升级 client lock”或“标准包内置 client”的重复任务。基线生产锁已是 `671c584f…`，不能沿用设计文档“仍为 a4aa606”的历史描述，也不能仅凭 SHA 变化认定 C8 完成；须证明候选包含 #104 能力并通过实际占用/导入反例。A3/A8 等 DONE 记录保持历史原样，C9 负责把未来包布局从两程序更新为三程序。

下表 HN 编号均为本交付台账 ID；Hyphae 行由上游建立对应 issue/PR 并双向映射，不替代其原生里程碑编号。可按 ≤300 行 task PR 再细分，依赖和出口不变。当前仅 HN0 为 READY，其余均 BACKLOG 且受 M1 门阻塞。#881 与 Hyphae 配套契约 PR 合并只建立基线；HN0 仍保持 READY，直到互链 follow-up PR 冻结健康阈值和 schema/回滚矩阵，才可标 DONE 并解锁 HN1。

| ID | 归属仓库 / 负责人 | 依赖 | 交付及可观察验收 | 估算人日 |
|---|---|---|---|---|
| DEP-HN0 | Agent24 + Hyphae / 双方维护者 | —（只写文档） | 两仓互链契约 PR 合并；冻结 SKU、pin/签名根策略、健康探测时限、schema/回滚矩阵与责任；没有根不能声称验签 | 0.5–1 |
| DEP-HN1 | Hyphae / 上游发布负责人 | HN0、M1 发布门 | tag 发布及四平台 client/relay 分包、两类 manifest/lock；固定版本安全安装器；Linux 双架构容器/持久卷指南；实际下载验证来源、完整 hash 与真实 relay smoke，坏资产拒绝 | 4–6 |
| DEP-C8 | Agent24 / comm 维护者 | HN1 | lock-promotion PR 固定双方 SHA；四平台按配方重建匹配下载资产；client/relay 组合、daemon.lock 占用、身份/联系人/收发/离线/重启/zero-run 通过；不兼容附 Hyphae issue/PR | 1–2 |
| DEP-HN2 | Agent24 / 安装器维护者 | C8 | client 与 relay 共用安全安装语义；staging/fsync/不覆盖原子提交、现有副本重验；断电模拟、满盘、篡改/并发/归档逃逸均不执行半成品、不覆盖异常目标 | 2–3 |
| DEP-C9 | Agent24 / 发布负责人 | C8、HN2 | CLI 与桌面包内 client 匹配锁，relay 不在标准包；四平台 Release 下载 smoke；无源码/Go/PATH 外部 hyphae 的干净环境能使用通信 | 1–2 |
| DEP-HN3 | Agent24 / daemon 管理层维护者 | HN2 | 可选下载、离线导入、plan/apply/组件 API 与持久状态机；同意绑定精确资产；取消/重启恢复/幂等成立；无 token、缺同意、坏 pin、latest 均拒绝；不放松 comm 依赖门 | 3–4 |
| DEP-HN4 | Agent24 / 生命周期维护者 | HN3 | relay 专用数据、loopback 与显式首启/autostart；组合更新/健康回滚/备份恢复；注入启动失败恢复获授权旧组合；旧 schema 不可读则阻断，PID 复用不误杀 | 3–4 |
| DEP-HN5 | Agent24 / UI 与 CLI 维护者 | HN4、C9 | 三个非术语入口及状态/确认落地到既有 COMM-6/7 通信界面，不复制消息实现；拒绝下载不监听，加入网络不拉 relay，服务器入口仅指引；ACK/进程存活不冒充送达/健康 | 3–4 |
| DEP-HN6 | Agent24 + Hyphae / 发布与验收负责人 | HN1、C9、HN4、HN5 | 组装离线包；真实 Release 的标准/可选/离线路径、服务器持久卷更新/回退、干净 macOS/Linux 验收与负例全部归档；最终外部评审与双方发布出口通过 | 2–4 |

依赖 DAG：`HN0 → [M1 发布门] → HN1 → C8 → HN2 → {C9, HN3 → HN4} → HN5 → HN6`；HN6 还直接依赖 HN1/C9/HN4。COMM-6/7 如尚未具备接入界面，HN5 必须与其负责人补齐所需通信入口，不以本表替 COMM 记完成；平台签名/公证仍依赖既有 DEP-B 门，不把未签名包写成已签名。

验收必须落实契约 §7 的负例矩阵与干净机器矩阵：四平台安装/CLI smoke，至少 macOS arm64 笔记本与 Linux x64 干净 VM 的 CLI/桌面全流程；用隔离身份，禁止在 Mac mini 执行。篡改、未晋升组合、取消、磁盘满、并发、健康失败、备份恢复、无同意扩展监听及 zero-run 正反对照均有证据。计划/文档通过不能代替这些实现门禁。

**工期估算，不是承诺**：合计约 **20–30 人日**，另预留外部评审/发布等待约 2–3 工作日；假设两仓各一名可用实现者、必要 UI/平台支持和签名条件就绪，按依赖推进预计 **15–20 个工作日**（可并行部分不等于启动额外方向）。文档冻结现在进行；若 M1 在 **2026-10-16** 前正式关闭且 HN0 同时冻结，最早 **2026-10-19** 开工，条件目标 **2026-11-06 至 2026-11-13** 完成，按周一至周五、不计当地假日估算。M1、上游可信资产、COMM UI、平台签名或评审延后则顺延，不能为日期缩减验收。

**关闭条件**：两仓所需 PR 已合并并互链、实际发布资产已构建、标准/可选/离线和服务器路径通过、升级及回滚演练完成、干净机器证据归档；本里程碑每一个 PR 都必须取得 PR-Daemon 对精确最终 head 的 APPROVE，required CI/本地门禁全绿。LAN/公网暴露、TLS、访问控制以及安全/权限/协议/存储变更还须完成适用的独立安全设计评审；release 由人发起审查并经 jason 验收，不因 PR 合并自动发布。HN0 单独完成只表示契约冻结，不能把 DEP-HN、M1 或 M2 标 DONE。

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
