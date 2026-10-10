# Hyphae 网络交付契约（Agent24 侧）

状态：2026-10-10 契约提案；合并后作为实现与验收约束，**文档合并不代表产品交付**。Agent24 实查基线为 `984d0f7c`；Hyphae 只读实查为 `ff93ce8f4514e12b647a7aee3999a4d409f3341c`。本文的“必须”均是交付要求，不是实现声明。

## 1. 依据、范围与责任

- Agent24 规范：[COMM-HYPHAE](../design/COMM-HYPHAE.md)、[CLI 接线提案及历史验收](../design/HYPHAE-CLI-INTEGRATION.md)；实现任务唯一台账为 [Deployment TASKS](TASKS.md#hyphae-delivery)。历史快照、已完成状态不回写。
- Hyphae 规范：[发布、安装与更新路径（稳定地址）](https://github.com/iDoris-ai/Hyphae/blob/main/docs/agent/hyphae-agent24-distribution-install-update.md)，由 [Hyphae #172](https://github.com/iDoris-ai/Hyphae/pull/172) 合并；本地对应跨仓路径 `/Users/jason/Dev/iDoris/Hyphae/docs/agent/hyphae-agent24-distribution-install-update.md`，不要求读者具有该路径。
- Hyphae 拥有身份、发现、加密通信、history/outbox、client/relay 协议与上游发布；Agent24 拥有包装、兼容晋升、凭据保管、安装 API、UI/CLI 与子进程监管。
- Hyphae client 是标准 Agent24 **随包交付的进程外 sidecar**，由唯一的 `agent24d` 管理，不把 Go 链接进 Rust。relay 是可选服务端，不在每台机器自动运行。
- 通信保持 **zero-run**；不包含远程执行、任务委派、M2 行为协议、移动端远控或 Agent24 daemon 的远程开放。服务器入口只提供部署指引与精确资产，不让 Agent24 SSH 到服务器执行命令。
- 本文扩展 #172 的可选本机节点与离线包范围，也扩展 COMM-HYPHAE §8.1“relay 只供联调、不进 lock”的旧前提；必须通过双方关联 PR 同步确认，不能仅靠 Agent24 单方宣布跨仓冻结。

## 2. 用户入口与包装 SKU（目标态）

首次进入“通信”必须提供以下三个选择；普通用户不必理解 relay 一词，也不必手动搜索 GitHub。高级诊断可以显示技术名称。

| 用户选择 | Agent24 必须执行的流程 | 默认边界 |
|---|---|---|
| 加入已有网络 | 输入管理员提供的网络地址/邀请信息，验证地址，创建或显式导入身份，展示连接对象，再由用户启动通信 | 只运行随包客户端；不下载或启动本机节点；不暗中回落到公共网络 |
| 在这台电脑上创建私人网络 | 展示“下载网络节点组件”的来源、精确版本、大小、磁盘占用、数据路径与监听范围；明确同意后下载、校验、原子安装；另行明确“启动本机网络” | 默认仅 `127.0.0.1`（若启用 IPv6 则仅 `::1`）；告诉用户“目前仅此电脑可连接”，不把 loopback 说成已经可供团队访问 |
| 在服务器上部署团队／公共节点 | 提供固定版本服务端包/容器、资源与持久卷要求、安全配置及升级回滚指引；部署完成后回到“加入已有网络” | 由运营者执行部署并选择团队访问控制或公共服务；不上传用户私钥，不自动操作服务器 |

| SKU | 内容与责任 | 支持目标 |
|---|---|---|
| 标准 Agent24 | Agent24 发布 `agent24 + agent24d + hyphae`，包含许可证/来源记录；CLI 与桌面均可发现配套客户端 | macOS arm64/x64、Linux x64/arm64 |
| 可选本机节点组件 | Hyphae 发布独立 `hyphae-relay` 包；Agent24 下载自己已晋升的精确资产 | 同上；没有该平台可信资产就禁用该选项 |
| 可选离线／自托管包 | Agent24 组装同一兼容组合的 client + relay、两份 lock/manifest 和认证材料；可与标准包一起预置 | 同上；离线导入也必须同意安装与启动，不能跳过校验 |
| 服务器／容器发行 | Hyphae 发布 relay 服务端包与按 digest 固定的容器；运营者管理服务及专用数据卷 | Linux x64/arm64；容器、服务指南是待交付物 |

标准包不含 relay；离线包有 relay 不表示自动启用。标准包中的 client 不依赖首次联网下载，可选组件失败也不应阻断“加入已有网络”。不得默认开启公共监听、UPnP、改防火墙、注册域名或绕过 TLS 校验；跨机器连接默认 `wss://`，loopback 可用 `ws://`。扩大监听到局域网/公网必须先具备访问控制与 TLS 方案，再单独确认地址、端口、可访问人群；达不到条件则拒绝。

## 3. 当前事实与缺口

| 已核对来源 | 当前事实 | 本里程碑补齐 |
|---|---|---|
| [生产 client lock](../../rust/crates/agent24-comm/hyphae.lock.json) | source `671c584f9e9eb807a15968e2aa42fd7507e178b8`，Go `go1.26.4`；只有 darwin-arm64/linux-x64 hash，darwin-x64/linux-arm64 为 null | 验证新候选及四平台；不能用旧设计文档的 `a4aa606` 代替当前锁 |
| [VerifiedBinary](../../rust/crates/agent24-comm/src/binary.rs) | 读取候选并按完整 SHA-256 校验；`O_EXCL`、0500 **直接写最终文件**；已存在副本重新 hash，不一致拒绝；中断可能留下被拒绝的半成品 | 同文件系统 staging、fsync、禁止覆盖目标的原子提交；保留精确 hash 拒绝 |
| [comm_routes](../../rust/apps/agent24d/src/comm_routes.rs) | `A24_HYPHAE_BIN` → 旧 `A24_SPEAKER_BIN` → agent24d 同目录 hyphae；已有专用 HOME、钥匙串选择、daemon 监管与显式首启后 autostart | relay 组件安装/监管、完整引导与健康回滚尚未交付；外部路径仍须匹配 embedded lock |
| [Hyphae artifact builder](https://github.com/iDoris-ai/Hyphae/blob/main/scripts/build_release_artifacts.py) | 当前仅 macOS arm64、Linux amd64；tar.gz 混装 client、relay、LICENSE、NOTICE；输出候选，不自动发布 | tag 发布流水线、四平台、client/relay 分包、独立 relay lock、容器与离线交付 |
| [Hyphae install.sh](https://github.com/iDoris-ai/Hyphae/blob/main/install.sh) | 默认 latest，下载后未验证 SHA256SUMS，不能作为 Agent24 安装入口 | 固定版本与可信来源校验；禁止直接复用现脚本 |
| [Open Design 可选安装器](../../apps/desktop/src/main/open-design-component.ts) | 已有内置 manifest、同意下载入口模式、进度/取消、hash、HTTPS 跳转限制、staging 与 rename | 复用产品模式；不是 Hyphae 已实现，也不能照抄其删除旧目录再 rename 的替换语义 |

当前不存在可宣称完成的完整通信 UI、自动 relay 下载、健康回滚、四平台正式发布或 client/relay 拆包。Hyphae 已有 relay 可执行程序及 loopback 绑定参数，不等于 Agent24 已管理它。COMM-HYPHAE 中进程存活、单次探测、补收进度必须继续分开；不得把存活三秒或握手成功包装成端到端健康。

## 4. 两类锁、信任与兼容晋升

1. client 沿用 `rust/crates/agent24-comm/hyphae.lock.json`；relay 新增独立的 `hyphae-relay.lock.json`（目标路径：同 crate 目录，当前不存在）。两者独立记录身份和 hash，不能让 client hash 为 relay 背书；同一版本号也不证明兼容。
2. Hyphae 分别提供 client/relay artifact manifest；Agent24 内置经评审晋升的组件 manifest，绑定 Agent24 版本、client lock 摘要、relay lock 摘要、已验证协议及数据 schema、允许的当前组合与回滚组合。运行时不从网络覆盖 embedded lock，也不接受版本范围或 `latest`。
3. 每类资产必须记录：schema、component、精确 version/tag、source SHA、Go toolchain、完整构建配方、OS/arch、最低系统版本、精确下载 URL、包名/大小/完整 SHA-256、解包后二进制完整 SHA-256、许可证、迁移/回滚约束及来源认证材料。明确 `linux-x64` lock key 对应 Go/资产的 `amd64`，不能靠文件名猜平台。
4. 本里程碑采用**独立固定 hash**作为必须实现的认证路径：预期摘要来自经过 Agent24 评审、随可信 Agent24 发行交付的内置 manifest，不能从正在下载的同一 Release 临时取值作为信任根。信任根是用户已验证的 Agent24 发行及其内置清单；晋升 PR 必须记录 Hyphae 仓库/source、固定配方重建和发布资产逐字节匹配的来源证据。
5. 若资产声明使用发布签名/证明，必须额外验证；公钥指纹或签发者/仓库/workflow identity 必须事先固化在 Agent24 信任配置中，轮换另走评审发布，不能与资产一起接受一个新根。具体签名机制与 root identity 在 DEP-HN0 双仓冻结时填入；尚未建立根时只允许上述独立 pin 路径，不得显示“签名已验证”。macOS 平台签名/公证另按 DEP-B 系列执行，不与 SHA 校验混称。
6. 同包 `SHA256SUMS` 仅证明传输/包内完整性，**不是发布者认证**；网络 HTTPS、文件权限、`--version`、本地 marker 也不能替代完整 pin 校验。标准包、在线可选包、离线包和容器均适用同一来源约束。
7. 未知平台、null hash、错误 component/schema/架构、不匹配组合、坏签名、hash 不符或不可信下载跳转一律 fail-closed；保留原可用版本并给出原因，不启动候选、不降级到 PATH/global 安装或公共节点。当前 `binary_rejected` 行为必须保留。

## 5. 安装、数据与生命周期（目标态）

| 对象 | 位置与规则 |
|---|---|
| client 副本 | `<state_dir>/comm/bin/hyphae-<sha16>`；0500，完整 hash 校验，短 hash 只用于命名 |
| client 数据 | `<state_dir>/comm/hyphae-home/.hyphae/`；专用 HOME/cwd，目录 0700、秘密文件 0600；身份/history/outbox 不随安装覆盖 |
| relay 副本 | `<state_dir>/components/hyphae-relay/<platform>/<full-sha256>/hyphae-relay`（目标）；不可变版本目录，0500 |
| relay 数据/配置 | `<state_dir>/comm/relay/<node-id>/`（目标）；独立 0700 数据目录、0600 配置/日志；明确传入 data-dir，禁止与 client HOME 或独立 Hyphae 共写 |
| 安装事务与回滚记录 | `<state_dir>/comm/components/`（目标）；记录 operation id、版本组合、授权范围、原版本、备份及阶段；不含口令/私钥；服务器使用独立持久卷，不把数据烘进镜像 |

Agent24 必须在同文件系统内创建仅当前用户可写的 staging，限制下载大小/解压总量，逐跳验证 HTTPS 与允许的资产主机，先验包 hash 再安全解包；拒绝绝对路径、`..`、逃逸链接、设备文件及非预期成员。验证解包后二进制与平台后 fsync 文件，以**不覆盖已有目标**的原子提交发布，再 fsync 父目录；已有目标必须重验，相同才复用，异常则拒绝并提供显式修复。安装事务持跨进程锁，崩溃恢复只清理自己未提交的 staging，不删除仍被其他事务使用的文件。

安装成功只到“已安装，未启动”。`agent24d` 统一监管 client 与已获授权的本机 relay；UI 不 spawn，不直接读写 Hyphae 数据。client 继续显式传 `--notify=false --auto-reply=false` 和用户配置的网络地址，不依赖上游公共地址默认值。首次启动须明确确认，成功后才保存各自 autostart；手动停止清除对应 autostart。关机、崩溃、PID 复用、并发启停沿用身份可验证的进程组监管，有界退避后可见失败，不能误杀外部服务或私自接管已有端口。

更新只接受 Agent24 发布所晋升的精确组合：检查兼容/磁盘空间 → 展示停机与迁移影响并确认 → 预下载/验证 → 停止受影响写入者 → 一致性备份 → 原子安装/切换 → 启动 → 有界健康检查。健康必须包含版本/hash、预期绑定地址、数据可读与真实合成消息往返，测试身份与正文不得污染用户会话；将外网不可达与本地启动故障分开报告。具体时间界限和探测能力由 DEP-HN0 冻结，不能无限等待。

启动或健康失败后先停候选，再恢复仍被当前兼容 manifest 明确允许的旧组合及必要的数据备份，并再次检查；回滚也失败则停止并保留诊断，不循环切换。当前单一 embedded client lock **没有**自动接受旧 hash 的能力：DEP-HN4 必须显式实现受信任的回滚组合授权，不能临时绕开 `VerifiedBinary`；不可读旧 schema 时不能只换旧 binary。不可逆迁移必须先验证可恢复备份并取得额外确认，否则阻断升级。至少保留一个已知健康组合及其受保护备份，用户确认稳定后才清理。

用户私钥始终留在本地加密 keystore；口令由 OS 凭据库保管，仅经 stdin 传子进程，禁止进入 argv、环境、配置、日志、诊断或部署指引。邀请凭据同样脱敏。导入旧 HOME 必须确认、复制并遵守占用锁，不修改原目录。历史 M7 记录的正文 argv/日志限制不能据此宣称已解决；本里程碑不得把秘密塞进正文作为变通。

## 6. Agent24 API、状态机与确认

下述为**新增目标接口**，不是可调用的现有接口；由 Agent24 维护并在实现 PR 同步 API 定义。CLI/桌面共用已有 token 鉴权的 `/api/v1/comm/*`，不新增匿名管理口。组件下载的 HTTP 适配器放在 agent24d 管理层，不能为下载放松 COMM-5b 的 `agent24-comm` 依赖白名单或接入 run 能力。

| 目标 API（均在 `/api/v1/comm` 下） | 必须具备的语义 |
|---|---|
| `GET /network`、`POST /network/plan` | 返回三选一、当前配置与只读计划；plan 包括平台、精确组合、下载大小、来源、绑定/数据路径、影响和 plan id；不下载、不监听 |
| `POST /network/apply` | 提交 plan id、期望 generation 与逐项 consent；过期计划返回 `conflict`，缺同意返回 `confirm_required`；幂等 operation id 防重复下载/启动 |
| `GET /components/hyphae-relay`、`POST /components/hyphae-relay/{install,cancel,update,rollback,uninstall}` | 返回事务进度/错误/可恢复动作；仅允许内置兼容清单指定资产，调用者不能注入任意 URL/命令；离线文件也进同一验证管道 |
| `GET /network/node`、`POST /network/node/{start,stop}` | 独立呈现本机节点状态、实际监听、autostart 与最近探测时间；服务器入口不把外部节点假称为本机受管进程 |

以下枚举为目标闭集；组件状态与运行状态分离，不用“在线”一个词覆盖全部阶段。

| 维度 | 精确状态与迁移 |
|---|---|
| 引导 | `unconfigured → planned → awaiting_consent → applying → configured`；取消回 `unconfigured`（已有配置则保留）；错误为 `failed`，显示失败阶段和重试入口 |
| 组件事务 | `not_installed → awaiting_consent → downloading → verifying → installing → installed`；离线省略 downloading；取消只在提交前生效并回原稳定态；失败为 `failed`，未知平台/无兼容资产为 `unavailable`，不可信候选为 `rejected` |
| 更新/回滚 | `installed → awaiting_consent → downloading → verifying → installing → checking → installed`；失败进入 `rolling_back → installed` 或 `failed`；同时报告 active/previous/candidate，失败状态不抹掉仍可用的 active |
| 本机节点 | `stopped → starting → running → stopping → stopped`；临时故障 `backoff → starting`，超限 `gave_up`；凭据/配置阻断为 `locked`；安装完仍为 stopped |
| 既有 client | 保留 `stopped/starting/running/backoff/gave_up/locked`，入口拒绝继续为 `binary_rejected`；`relay_probe` 与 `catch_up=unknown/incomplete` 独立展示，不发明 complete |

每个事务状态都必须包含 operation id、stage、可取消性、错误码及脱敏原因；重启后从日志与实际文件/进程恢复状态，不能把内存中的 downloading 当成事实。重复 apply 不产生第二个节点；并发修改返回 conflict；错误保留原 event ID 和 partial data。

确认必须由后端执行，UI 隐藏按钮不是权限检查：下载同意绑定资产 hash/大小；首次启动同意绑定监听范围；扩大暴露、改变网络、迁移/回滚、停止正在使用的节点均展示具体影响后确认。升级计划须明确包含健康失败时自动恢复的旧组合与备份范围，用户同意后该范围内的自动回滚不重复弹窗；范围外的恢复须另行确认。卸载默认只删组件、保留数据；删除身份/历史/节点数据须独立二次确认并说明不可恢复及备份；清空 outbox 继续说明“仅删本地记录，不撤回已发事件”。不得用一个总勾选替代所有高风险同意，也不得在更新后扩大既有授权范围。

## 7. 跨仓变更与交付验收

Hyphae 先发布候选（精确 tag/source、client/relay 资产、manifest、来源证据、兼容与迁移说明），Agent24 再开 **lock-promotion PR**。该 PR 固定两侧 SHA/完整 binary hash，按配方重建并与实际 Release 下载匹配，跑身份/联系人/网络配置、加密双向发送、history/outbox、离线恢复、daemon 重启及 zero-run。Hyphae 发布不等于 Agent24 接受，Agent24 不追随 `latest`。

发现不兼容时，Agent24 保留旧锁并在 Hyphae 建关联 issue/PR，提供双方版本/SHA、平台、预期与实际、可复现命令及脱敏证据；修复候选重新走晋升。协议、manifest、包布局、信任根、迁移或 UX 责任边界变化必须有**两仓互链 PR**，双方评审合并后才冻结；不得单方静默改变另一侧契约。本次 Agent24 文档 PR 正文须链接 #172 与后续 Hyphae 补充契约 PR，补充 PR 反链本 PR。

干净机器验收只消费真实 Release 资产，不依赖源码目录、Go、PATH 中已有 hyphae 或生产身份。在笔记本/获准的隔离 VM 和 CI 进行，禁止使用 Mac mini。所有支持的 macOS/Linux 四平台跑安装与 CLI smoke；至少 macOS arm64 笔记本和 Linux x64 干净 VM 完成 CLI/桌面全流程，其余平台准确注明执行环境，不把 runner smoke 冒充真机 UI 验收。

| 验收组 | 可观察结果（标准／可选／离线路径均需覆盖适用项） |
|---|---|
| 首装与默认安全 | 标准包含正确 client、不含 relay；加入已有网络不启动本机监听；首次打开无下载/监听；拒绝同意无副作用；本机节点明确确认后只监听 loopback；离线断网仍可验证安装并启动 |
| 实际通信 | 两套合成身份经真实节点双向加密收发；断网入队再恢复使用同 event ID、恰一次；125 条积压后重启零新增；runs/model/module 三类计数均 Δ=0，同量具正对照有效；UI 不把 ACK 写成送达 |
| 升级与回滚 | client/relay 分别更新和组合更新；版本/数据不混写；健康失败恢复旧授权组合；不可逆 schema 无备份被阻断；恢复后身份、历史、待发一致；服务器持久卷升级/回退同样有证据 |
| 负例与恢复 | 错平台/null lock、篡改一字节/已装副本、同包伪 checksum、坏签名或未知根、未晋升版本/旧清单重放、恶意归档/跳转全部拒绝；下载取消/断网/磁盘满/提交中断/并发安装不留下可执行半成品，不破坏 active |
| 权限与生命周期 | 无 token/无同意/过期 plan 被拒；端口冲突不接管外部进程；agent24d 重启/崩溃不留失管节点；停机/卸载不删数据；argv/日志/报告无秘密；TLS 错误不放行；扩大暴露无授权不监听 |

验收归档须含两仓 PR 与外部评审链接、合并 SHA、release/tag/资产 URL、完整摘要、平台与系统版本、命令/退出码、CI run、逐项结果、脱敏 UI/监听证据和升级回滚记录。每个实现 task 先给出失败反面对照，再提供修复后结果；文档断言不替代真实运行。

**完成定义**：双方所需契约及实现 PR 均合并、真实 release 资产构建发布、标准/可选/离线/服务器路径通过、升级和健康回滚演练通过、干净机器验收归档。**本里程碑每一个 PR** 都必须取得 PR-Daemon 对精确最终 head 的 APPROVE，required CI 与本地门禁通过；LAN/公网暴露、TLS、访问控制以及安全/权限/协议/存储变更还须完成适用的独立安全设计评审。Release 由人发起审查并经 jason 验收，不因 PR 合并自动发布。任何一项缺失都不能关闭交付里程碑。

## 8. 时间与未冻结项

现在只做文档/契约冻结；实现必须等待 [Hyphae 当前 M1 发布门](https://github.com/iDoris-ai/Hyphae/blob/main/docs/agent/m1-release-plan-20261007.md) 正式关闭（聊天、群聊、离线恢复和最终发布证据），本契约既不关闭 M1，也不启动 M2。任务 DAG、逐项人日与条件日期见 [TASKS](TASKS.md#hyphae-delivery)。

待 DEP-HN0 双仓确认：上游补充可选 relay/离线包契约、签名机制及 root identity（若启用）、健康探测及超时、数据 schema 兼容表和正式候选 tag。#881 与 Hyphae 配套契约 PR 合并只建立基线，**不会自动关闭 DEP-HN0**；双方还须用互链 follow-up PR 冻结健康阈值和 schema/回滚矩阵，DEP-HN1 在此之前保持 BLOCKED。独立 pin、loopback、双锁、zero-run 与 M1 前置门不因这些待定项放松；缺少可验证候选或恢复能力时，对应实现任务保持 BLOCKED。
