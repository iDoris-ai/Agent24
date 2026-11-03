# 架构梳理 R2：运行单元概念盘点（按 ADR-039 / ADR-038 归类）

索引：[架构总入口](../README.md)・[ADR 索引](../../adr/README.md)

| 项 | 内容 |
|---|---|
| HEAD | `9865d7a0303cf73e2766e2f20a174159dc1bd975`（main，「Merge pull request #1555 from iDoris-ai/feat/od-pin-odx3」，2026-11-03 10:32 +0700） |
| 基线说明 | 分析基于 `9865d7a0`；推送前分支已 rebase 到 `c212e591`（#1557）。中间的 #1556（OD 子进程改用 Electron Helper 可执行文件）和 #1557（导出私钥对话框）只改 `apps/desktop/src/main/{creative-serve-web,main,ipc/index,backup-password-window}.ts`，不影响本文结论；但这几个文件里引用的行号以 `9865d7a0` 为准，在新 HEAD 上会有偏移 |
| 生成方式 | 只读静态分析：`grep` / 读源码 / `git log`。**没有构建、没有起进程、没有跑握手**。所有现状描述以代码为准；文档与代码冲突时写明冲突 |
| 数据来源（文档） | `CLAUDE.md`、`AGENTS.md`、`docs/architecture/README.md`、`docs/adr/README.md`、`docs/ARCHITECTURE-LAYERS.md`、ADR-038（草案 r3 + §7 r4 补充）、ADR-039（已接受 r2）、ADR-040～043（均提议）、`docs/architecture/EVOLVABILITY-ASSESSMENT-2026-11-01.md`、`docs/agent/COMPONENT-ROADMAP.md`、`docs/design/COMM-HYPHAE.md`、`docs/design/INTEGRATION-AGENTEAR-IDORIS.md`、`docs/design/A3-ATTACHED-MODULE.md`、`docs/design/OPEN-DESIGN-IDORIS-EDITION.md`、`docs/documenting/README.md`、`docs/knowledge/POSITIONING-AND-DIFFS.md` |
| 数据来源（代码） | `rust/apps/agent24d/src/**`（尤其 `component_routes.rs`、`server.rs`、`capabilities/*`、`domain.rs`、`attached*.rs`、`idoris_host*`、`hyphae_install.rs`、`comm_routes.rs`、`knowledge_*`、`mcp.rs`、`lifecycle.rs`）、`rust/crates/agent24-{components,idoris,comm,models,domain,os-proto,os-packages,mcp,knowledge,store,policy,protocol}`、`rust/apps/agent24-cli`、`rust/apps/agent24-documents`、`rust/apps/agent24-sidecar-host`、`apps/desktop/src/main/**`、`apps/desktop/scripts/*open-design*`、`packages/*` |
| 证据约定 | 路径相对仓库根；`d/` = `rust/apps/agent24d/src/`；`file:line` 为 HEAD 上的行号。读不到的写「未核实」 |

---

## 0. 摘要

1. **概念归类基本对得上 ADR-039**：本体（agent24d 内的子系统）+ 九类能力单元 + 四种外壳。**技能一项都没有实现**（`rust/`、`apps/`、`packages/` 里 `skill` 零命中，ADR-040 仍是「提议」）。
2. **ADR-038 落地到「组件状态汇总」这一层**：C1 / C1b / C3a / C3b 已合（#1539、#1551、#1552、#1545、#1549），`GET /api/v1/components` 在线。但它只覆盖 iDoris、Hyphae、attach 模块、Documents、Knowledge、Open Design 六类；**Sin90/Cos72 等其他 OS 包、MCP、进程内模块不在列表里**；C2（Agent24 侧契约检查与 health）、C4–C7 没有专门代码，C8 只做了上报。
3. **同类事物三四套机制**：下载器 2 套在用、1 套空缺（OS 包靠手工复制目录）、1 套计划中（DOC-03 的「通用按需组件安装器」）；签名校验只覆盖 iDoris/Hyphae；停止宽限有 4 个来源；同意（consent）有 5 种形态。
4. **状态与契约有双重来源**：Hyphae 的 `/api/v1/comm/daemon` 和 `/api/v1/components` 映射规则不同、版本来源不同；iDoris 契约版本有 3 处检查且阈值不一致（锁要求 ≥1.1，模型网关只要求 ≥1.0.1）。
5. **几处归类与 ADR 文字不符**：ADR-038 §1 说 Documents 由桌面拉起 CLI，代码里它是 daemon 监管的进程外 OS 包；Knowledge 是能力，但它的工具以本体 `builtin` 身份注册、不走 K1 逐工具同意；`Form` 枚举没有「进程内」和「OS 包」两种形态；通信（Hyphae）没有 ADR。

---

## 1. 运行单元清单

### 1.1 归类与运行形态

「ADR-039 归类」按 ADR-039 §2 / §5 的三问。「D5 形态」写 ADR-038 D5 的六种：resident / sidecar / cli / attach / remote / mcp；代码里的实际取值来自 `agent24_components::Form`（`rust/crates/agent24-components/src/state.rs:32-45`，变体 `Resident, Sidecar, Cli, Attach, Remote, Mcp`）。

| # | 运行单元 | ADR-039 归类 | D5 形态（文档 → 代码） | 代码位置 |
|---|---|---|---|---|
| U1 | agent24d 内部子系统（agent 循环 / runs、模型路由、记忆、调度、审批与审计、K1 同意、capability token、decide、硬件、workspace、事件、用量 / 计时、迁移协调器、生命周期、组件状态汇总、MCP host、OS 包 host、attach registry） | **本体** | 不适用（本体） | 见 §1.3 |
| U2 | iDoris（本地模型服务） | 能力（服务能力；最小可用配置里的模型来源） | resident → `Form::Resident`（`d/component_routes.rs:191`） | `rust/crates/agent24-idoris`、`d/idoris_host*`、`d/idoris_routes.rs` |
| U3 | Hyphae（通信） | 能力（服务能力） | 实际是**两种形态叠加**：常驻 `hyphae daemon` + 按次调用 CLI（`HyphaeRunner`）→ 代码只登记为 `Form::Resident`（`d/component_routes.rs:237`） | `rust/crates/agent24-comm`、`d/hyphae_install.rs`、`d/comm_routes.rs` |
| U4 | AgentEar（语音） | 能力（服务能力） | attach → `Form::Attach`（`d/component_routes.rs:297`） | `d/attach_listener.rs`、`d/attach_registry.rs`、`d/attached.rs` |
| U5 | Documents（`agent24-documents` 进程外 OS 包） | 能力 | 文档：ADR-038 §1 写「桌面 main 进程拉起 `agent24-documents` CLI」；**代码**：daemon 监管的进程外 OS 包（`impl_kind: out_of_process_provider`），登记为 `Form::Resident`（`d/component_routes.rs:305`） | `rust/apps/agent24-documents`、`rust/crates/agent24-documents-client`、`d/domain.rs` |
| U6 | Sin90 / Cos72 等外部领域 OS 包 | 组件（领域包）；包内每项各自归为能力或技能（ADR-039 §2.2） | 进程外 OS 包；ADR-038 D4 归到「JSON-RPC attach / 领域 OS」绑定。**`Form` 枚举里没有对应变体**，也不在 `/components` | 外仓 `iDoris-ai/Sin90`、`MushroomDAO/Cos72`（`d/domain.rs:11-13`）；宿主 `rust/crates/agent24-os-*` |
| U7 | Knowledge（WeKnora，远端） | 能力（服务能力） | remote → `Form::Remote`（`d/component_routes.rs:341`） | `rust/crates/agent24-knowledge`、`d/knowledge_*` |
| U8 | Open Design（headless sidecar） | 能力（完整系统） | sidecar → `Form::Sidecar`（`d/component_routes.rs:358`），**由桌面托管，不由 daemon 托管** | `apps/desktop/src/main/{open-design-component,creative-serve-web,open-design-report}.ts` |
| U9 | MCP server（用户配置） | 能力（只有工具） | mcp → `Form::Mcp` 已定义，**未被使用**（`/components` 不列 MCP） | `rust/crates/agent24-mcp`、`d/mcp.rs` |
| U10 | 进程内 DomainModule | 组件（进程内形态） | ADR-038 D4 列「进程内」；**`Form` 无此变体**；无生产实例（`d/domain.rs:734-740`「No production code constructs this variant」） | `rust/crates/agent24-domain/src/lib.rs:1402` |
| U11 | 文档引擎 SuperDoc headless / LibreOffice | 能力（原子能力） | 计划按需下载（ADR-DOC-03）；**代码里没有下载器、manifest 或安装目录（未找到）** | 仅设计 |
| U12 | 桌面（Electron） | 外壳（客户端，ADR-037） | 不适用；兼任 Open Design 和 Knowledge binding 的宿主 | `apps/desktop` |
| U13 | TUI | 外壳 | 不适用 | `rust/apps/agent24-cli/src/tui/` |
| U14 | CLI（`agent24`） | 外壳 | 不适用 | `rust/apps/agent24-cli` |
| U15 | `agent24 acp` 桥（供 Open Design 调用） | 外壳（专供某项能力的客户端） | 不适用；stdio ACP → REST/WS（`rust/apps/agent24-cli/src/acp.rs:1-5`） | `rust/apps/agent24-cli/src/acp.rs` |
| U16 | `packages/nostr-bridge`、`packages/wechat-bridge` | 按 ADR-039 应是「通信 / 渠道」能力；现实现为**持有 daemon 全权 token 的外部客户端** | 不在任何形态里；开发态 `tsx src/main.ts`（`packages/nostr-bridge/package.json:8`） | `packages/*-bridge` |
| U17 | `agent24-sidecar-host`（Rust） | 本体的托管工具（计划中） | 只有 CI 用（`.github/workflows/sidecar-windows.yml`），桌面不打包、不引用；大量 `#[allow(dead_code)]`（`rust/apps/agent24-sidecar-host/src/lib.rs:9-30`） | `rust/apps/agent24-sidecar-host` |
| U18 | 技能 | 技能 | 不适用 | **未实现**：`grep -rli skill rust apps packages` 零命中；ADR-040 状态「提议」（`docs/adr/ADR-040-skill-contract-v1.md:5`）；`SPEC-SKILL-DISTRIBUTION.md` 头写「设计定稿，未实现」 |

不算运行单元、只列出来避免遗漏：`packages/node-daemon`（开发用 mock daemon，「仅契约测试与开发参考，不进安装包」，`AGENT24_BACKEND=node` 时桌面才 fork）；`packages/documents-accept`（验收 harness）；`packages/data-dir`（库）；`agent24-os-fd/proc/cwd/sqlite`（宿主辅助 crate）；`agent24-mcp/src/server.rs` 把 agent24d 暴露成 MCP server（反方向，属本体对外接口）。

### 1.2 能力单元的九项属性

列含义：**分发** = 安装与分发、锁或签名；**契约** = 契约定义位置；**状态** = 谁持有、存在哪；**生命周期** = 谁拉起、谁停；**权限** = token / audience、K1 授权落点；**API** = agent24d 对外前缀；**/components** = 接入 `GET /api/v1/components` 的程度。

#### U2 iDoris

| 属性 | 现状与证据 |
|---|---|
| 分发 | agent24d 下载：`download_locked_asset(IdorisLock::embedded(), …)`（`d/idoris_host/provision.rs:101-116`），主机白名单 `RELEASE_HOSTS`（`agent24-components/src/download.rs:29-33`），上限 256 MiB（`:23`）。锁 `rust/crates/agent24-idoris/idoris.lock.json`，`"schema": 2`，`"version": "v0.4.0"`，`signing {macos_team_id: HYAP444FN9, notarized: true}`。签名：安装走 `VerifyMode::Install`（codesign + 团队 ID + Hardened Runtime + spctl），启动 / 恢复走 `VerifyMode::AtRest`（`d/idoris_host.rs:448,455`；`agent24-components/src/sign.rs:22-25,70`）。release 落在 `~/.agent24/components/idoris/<version>/`，缓存在 `…/idoris/cache/`（`server.rs:1930-1932` + `agent24-idoris` `managed_root`） |
| 契约 | 锁：`"contract": { "name": "idoris-http", "major": 1, "min_minor": 1, "tested": "1.1.0" }`（`idoris.lock.json:9`）。检查有三处：① `IdorisLock::check_contract`（`agent24-idoris/src/lib.rs:236`）；② supervisor `validate_health` 读 `/health`，要求 `version==lock.tag` 并调 ①（`supervisor.rs:946-966`）；③ 模型网关 `contract_version_accepted` 要求 `1.x.y` 且 `(minor,patch) >= (0,1)`（`agent24-models/src/idoris.rs:480-492`，调用点 `:244`）。**③ 比锁宽**，见 §2 I-6 |
| 状态 | `idoris_host::State {NotInstalled, NotConfigured, Stopped, Starting, Running, Failed, InstanceConflict}`（`d/idoris_host.rs:120-128`），内存持有在 `IdorisHost.inner`（`:545-562`），由 `installation.json` 推导；`installation.json`、`operations.json`、`data/`、`instances/` 在 `~/Library/Application Support/Agent24/idoris/`（ADR-038 §7.9 有意不搬）；另有 `transactions/.operation.lock`（`provision.rs:278-285`）；连接配置 `idoris-connection.json`（`server.rs:2134`） |
| 生命周期 | daemon 管：`Supervisor` 拉起 `idoris serve --admin-token-stdin`（`agent24-idoris/src/supervisor.rs:874`），停止 SIGTERM → `Timings.terminate = 17s` → SIGKILL（`supervisor.rs:158,697-728`）。**锁里没有 `stop_grace_ms`，全局 15s 宽限也不用** |
| 权限 | daemon→iDoris：每次启动新铸 Admin token，经 **stdin** 交付（`supervisor.rs:874,910-942`）。注意 ADR-035 写的是「匿名 FD/HANDLE」；IL-13 已把它改为 daemon 自持，桌面 FD 路径已删（`apps/desktop/src/main/idoris-admin.test.ts:5-8`）。数据面：`Bearer <data key>`，key 存钥匙串（`agent24-models/src/idoris.rs:140`）。客户端路由：status 走 `IdorisRead`/Everyday，install/connect/start/stop/cancel 走 `IdorisLifecycle`/Elevated（`d/capabilities/route_policy.rs:69-76`），Elevated 走 CORE-2 在场证明（`d/capabilities/first_party.rs:488-497`）。**K1：不涉及**（iDoris 不暴露模块工具）；connect 另有自己的 `ConnectConsent`（`provision.rs:19-24`） |
| API | `/api/v1/idoris/{status,install,connect,omlx/candidates,start,stop,operations/{id},operations/{id}/cancel}`（`d/server.rs:1458-1478`） |
| /components | **已接入**：`idoris_item` 读同一个 `host.status()`，`provides` 来自锁（`d/component_routes.rs:189-225`）。旧路由 `/idoris/status` 仍在，桌面 `IdorisPanel` 每 1 秒轮询它（`apps/desktop/src/renderer/components/idoris/IdorisPanel.tsx:99`） |

#### U3 Hyphae

| 属性 | 现状与证据 |
|---|---|
| 分发 | daemon 下载：`POST /api/v1/comm/install` → `hyphae_install::install`（`d/comm_routes.rs:584-592`，`d/hyphae_install.rs:176-240`），同一个 `download_locked_asset` 和主机白名单。锁 `rust/crates/agent24-comm/hyphae.lock.json`：`"schema": 2`（:61）、`v0.30.0`、`stop_grace_ms: 15000`（:70）、team `HYAP444FN9`、notarized。安装 `VerifyMode::Install`；每次构建 comm 状态时 `VerifyMode::AtRest`（`d/comm_routes.rs:357-366`）；Linux 只有 sha256（`UnsignedPlatform`，`d/hyphae_install.rs:131-155`）。release 在 `~/.agent24/components/hyphae/<version>/hyphae`（`hyphae_install.rs:84-90`），缓存在 `components/hyphae-cache`（`:200-203`）；运行副本另在 `~/.agent24/comm/bin/` |
| 契约 | 锁：`contract {name: hyphae-cli, major: 1, min_minor: 0, tested: "1.0"}`（`hyphae.lock.json:25-30`）。**运行时从未检查**：`accepts_contract` 只出现在锁校验和测试里（`d/hyphae_install.rs:304`），`agent24-comm/src` 里没有调用 `hyphae contract` 或 `hyphae health`。设计文档：`docs/design/COMM-HYPHAE.md`（r2 冻结）、`HYPHAE-CLI-INTEGRATION.md`；**没有 ADR** |
| 状态 | `DaemonStatus.state: &'static str`，注释列出 `stopped/running/backoff/locked/gave_up/not_configured`（`agent24-comm/src/daemon.rs:779-785`），代码另设 `"starting"`（`daemon.rs:1416`）。由 `HyphaeDaemonSupervisor` 持有、登记在 `CommDaemonSlot`（`d/comm_routes.rs:68,199,214`）。持久化：`comm/daemon-autostart.json`、`comm/hyphae-daemon.pid`；业务数据（keystore、outbox、messages.db）在 Hyphae 自己的 HOME `comm/hyphae-home/.hyphae/`（COMM-HYPHAE §2） |
| 生命周期 | daemon 监管：`build_ready_state` 回收孤儿、起 supervisor、按 autostart 启动（`d/comm_routes.rs:375-418`）；拉起 `hyphae daemon --identity … --password-stdin --json`（`daemon.rs:720-742`）。宽限用全局 `lifecycle::Params::stop_grace`（`A24_COMPONENT_STOP_GRACE_MS`，默认 15 000，`d/lifecycle.rs:51,56`）。**锁里的 `stop_grace_ms` 没被读取**（ADR-038 §7.11 已承认），两者只是碰巧都等于 15 000 |
| 权限 | daemon 与 Hyphae 之间没有 token；keystore 口令从钥匙串取，经 `--password-stdin` 传（`agent24-comm/src/runner.rs:479-481`）。客户端路由：`/comm/install` 走 `ComponentInstall`/Elevated（`route_policy.rs:142`），读走 `CommRead`/Everyday，写走 `CommWrite`/Elevated；`identity/export`、`identity/import` 仅 host（`d/server.rs:1041-1047`）。comm 的 Elevated 写**不在** first-party 在场证明白名单里（`d/capabilities/first_party.rs:482-497` 只列 attached、delegation、iDoris），桌面以 ProductHost 身份发，靠桌面自己的原生确认框兜底（`apps/desktop/src/main/backend-proxy.ts:114-120`）。**K1：不涉及** |
| API | `/api/v1/comm/*`（`agent24-comm/src/router.rs:169-187` + `groupchat::routes()`，`d/comm_routes.rs:629-636` 加 `/install`；合并于 `d/server.rs:3160-3161`） |
| /components | **已接入，但与旧路由不一致**：`hyphae_item`（`d/component_routes.rs:227-270`），见 §2 I-5 |

#### U4 AgentEar（attach）

| 属性 | 现状与证据 |
|---|---|
| 分发 | **Agent24 不下载、不锁、不验签**（ADR-038 D2a 写明这是 v1 接受的缺口）。用户或 AgentEar 自己拉起进程，再 `POST /api/v1/attached` 注册；记录 manifest 原文、`manifest_digest`、token 的 sha256，存在 `~/.agent24/attached.json`（`d/attached.rs:100-121,186-188`） |
| 契约 | 同一个 `DomainOsManifest`，要求 `impl_kind == attached_process`（`d/attached.rs:64-70`）；握手 `accept_attached`：先比 token 哈希，再比 digest，最后 `version::negotiate`（`agent24-os-proto/src/initialize.rs:441-475`）。设计 `docs/design/A3-ATTACHED-MODULE.md` |
| 状态 | 磁盘：`attached.json`（含 `disabled`、`revoked` 墓碑，`d/attached.rs:167-178`）。内存：`AttachRegistry` 每模块一个 `AttachSlot`/`Generation`（`d/attach_registry.rs:295`），`snapshot()` 给 `(name, disabled, Option<DrainState>)`（`:708`） |
| 生命周期 | **自启**，daemon 不拉起也不停；daemon 关停时 `registry.revoke_all()`（`d/server.rs:1815,2659`）；不发 SIGTERM，只受 `CALL_CANCEL_GRACE = 2s`（`d/lifecycle.rs:24-28`） |
| 权限 | socket 目录 0700、属主 euid、对端 uid==euid（`d/attach_listener.rs:45-70,164-172`）；token 常量时间比 sha256（`initialize.rs:449-461`）；授权 `ATTACH_OOP_GRANTS = [Events, Models]`（`d/attach_registry.rs:155`）∩ `Grants::granting`。注册和管理走 `ModuleAdmin`/Elevated（`route_policy.rs:116-119`）；隐私放宽要 `allow_relax`（A3 §3.5）。**K1：没有为 attach 模块注册工具**（工具只为 `Build::Package` 注册，`d/server.rs:2236`），所以逐工具同意不适用 |
| API | `/api/v1/attached`（POST/GET）、`/api/v1/attached/{name}`（DELETE/PATCH）、`/api/v1/os/{name}/commands/{command}`（`d/server.rs:1609-1616,1640-1642`）；另在 `GET /api/v1/os` 里以 `kind: "attached"` 出现 |
| /components | **通用接入**：`attach_items`（`d/component_routes.rs:286-300`），`provides` 为空。C4 没有 AgentEar 专项代码 |

#### U5 Documents / U6 Sin90、Cos72（进程外 OS 包）

| 属性 | 现状与证据 |
|---|---|
| 分发 | `agent24 os install <dir>` 在本地直接复制目录（staging + 原子 rename），**不经 daemon**（`rust/apps/agent24-cli/src/main.rs:1460-1478`，`agent24-os-packages/src/install.rs:113,154-217`）；包目录 `<state_dir>/packages`，要求 0700 且属主是当前 uid（`agent24-os-packages/src/lib.rs:23,96,158-197`）。完整性**只有 manifest 字节的 sha256**（`agent24-os-proto/src/manifest.rs:56`），且只覆盖 `domain-os.yml`：「an executable/script/asset changed behind an unchanged manifest passes this」（`rust/crates/agent24-os-packages/src/discovery.rs:261-266`）；**没有锁、没有签名**。Documents 不随安装包分发，要手工安装（`rust/apps/agent24-documents/README.md:7`，界面文案「Install it by hand」`Documents.i18n.ts:8`）。Sin90/Cos72 tarball 的 `SHA256SUMS` 只在发布清单里人工 `shasum -c`（`docs/RELEASE-CHECKLIST-v0.5.1.md:153-158`） |
| 契约 | `DomainOsManifest`（`agent24-domain/src/lib.rs:449-527`，`deny_unknown_fields`，`MANIFEST_SCHEMA_VERSION = 1`、`DAEMON_PROTOCOL_VERSION = 4`，`:418,430`）；握手 `InitializeParams` + `version::negotiate`（`agent24-os-proto/src/initialize.rs:59-74`，`version.rs:175,208`）；wire 规格 `docs/specs/WIRE-OOP-MODULE.md`；Documents 业务契约 ADR-DOC-02。manifest **没有 `provides` 字段**（加了会被 `deny_unknown_fields` 拒绝） |
| 状态 | 内存：`MountOutcome {Mounted, Disabled, Degraded, Refused}`（`d/domain.rs:503-521`）→ `AppState.os_reports`；supervisor `Status` 10 个变体（`agent24-os-proto/src/supervisor.rs:107-160`）→ `AppState.module_status`；`DrainState`（`drain.rs:121`）。磁盘：`~/.agent24/os.json`（启用标记，`d/os_config.rs:130-131`）、`os-enable-marks.json`；模块数据在 `~/.agent24/os/<name>/` |
| 生命周期 | daemon 启动时发现（`d/server.rs:2225` `with_discovered`），`Supervisors` 拉起、监管（`d/domain.rs:779,1916-1924`）；宽限 `GRACE_DEFAULT_MS = 15_000`、`GRACE_MAX_MS = 30_000`、`CALL_CANCEL_GRACE = 2s`（`d/lifecycle.rs:28,56,58`；`agent24-protocol/src/shutdown_bounds.rs:10`） |
| 权限 | 每次拉起一次性握手 token（`agent24-os-proto/src/launch.rs:34,277`）；内核回调授权 `KERNEL_OOP_GRANTS` ∩ `Grants::granting`（`d/domain.rs:116-122,1803`）。**K1 授权落点**：同意写入 `consent_routes::decide` → `grant_module_consent_scoped`/`deny_module_consent`（`d/consent_routes.rs:547,616-632`），存在 `agent24.db` 的 `module_consents` 表（`agent24-store/migrations/0015_module_consents.sql:23`）；每次调用由 `StoreConsentGate::authorize` 执行（`agent24-policy/src/consent_gate.rs:397-398`，`d/module_tool_runtime.rs:389-396` 组装）；写入还受开关 `A24_MODULE_TOOL_GRANTS` 控制（`d/server.rs:1901-1906`）。启用时推进 epoch，禁用时作废同意（`d/os_routes.rs:496,535`） |
| API | `/api/v1/os`、`/api/v1/os/{name}`、`…/delegation`、`…/stop`（`route_policy.rs:120-123`）；模块自有命名空间 `/api/v1/<name>/…` → `Operation::ModuleRoute`/Everyday（`route_policy.rs:237-241`）；同意相关 `/api/v1/module-consents*`（`d/server.rs:1514-1531`） |
| /components | **只有 Documents**：`documents_item` 按 `r.name == "documents"` 查（`d/component_routes.rs:304-306`），`provides` 为空。**Sin90、Cos72 和其他已装 OS 包不出现** |

#### U7 Knowledge（WeKnora，remote）

| 属性 | 现状与证据 |
|---|---|
| 分发 | 不下载（ADR-038 D1 免锁）；个人版的 Lite 部署设想由 SidecarManager 托管（`POSITIONING-AND-DIFFS.md`「两种部署」节），**代码里没有实现**（SidecarManager 只有测试在用） |
| 契约 | crate `agent24-knowledge`：`ProtocolVersion` 常量 1（`agent24-knowledge/src/common.rs:13-25`）、`/system/capabilities` 模型（`capabilities.rs:9-40`）、Provider 状态机和 probe（`provider/binding.rs:344`）。**daemon 没接 Provider**：`d/knowledge_backend.rs:6`「The crate's Provider … is not wired yet」。业务契约 `docs/knowledge/CONTRACT-*.md` |
| 状态 | binding 和凭据 C 只在内存（`d/knowledge_host.rs:1-5,26-30`）；`knowledge.db` 模块写好了但被标 `#[allow(dead_code)]`、没有接线（`d/knowledge_store.rs:27-28`，`d/main.rs:49`） |
| 生命周期 | daemon 不启停远端；binding 由**桌面 `KnowledgeHost` 推送**（`apps/desktop/src/main/knowledge-host.ts:18`，每 4 秒同步，`main.ts:433-434`） |
| 权限 | binding 和凭据路由走 `KnowledgeHost`/Elevated，只有 ProductHost 能调（`route_policy.rs:126-129`）。**工具 `knowledge_search`/`knowledge_read` 以 `"builtin"` 来源、`RiskClass::Read` 注册**（`d/knowledge_tools.rs:31-32,67,88`），只提供给指名了 binding 的 run；**不走 K1 模块同意** |
| API | 没有 `/api/v1/knowledge*`；只有 `/api/v1/host/knowledge/bindings/{binding_id}`（PUT/DELETE）和 `…/credential`（`d/server.rs:1498-1506`） |
| /components | **部分接入**：`knowledge_item` 只看 `any_active()`，有 binding 就报 `ready`，附 `probed: false`（`d/component_routes.rs:335-355`）；没有探测，也就永远不会是 `degraded` 或 `failed` |

#### U8 Open Design（sidecar，桌面托管）

| 属性 | 现状与证据 |
|---|---|
| 分发 | **桌面 TS 下载器**：`open-design-component.ts`（安装包只带一个 manifest；白名单与 Rust 那套相同但另写一份，`:90`；逐跳重验 HTTPS 和 host，`:38-40`；sha256，`:239-247`；断点续传，`:1148-1175`；装好后只读，`:359-377`；每次启动前重算 4 个入口文件哈希，`:81-86,269-282`）。目录 `~/.agent24/components/open-design/<odSha7>-<platform>-<arch>/`（`:190-201`）。**没有签名校验**（只有 sha256）。pin 在多个文件各写一份：`pack-open-design-component.mjs:36-37`（`0.26.1` / `f5beffcb…`）、`prepare-open-design-linux-resources.mjs:13`、`stage-open-design-resources.mjs:5`、运行时 `creative-serve-web.ts:70` |
| 契约 | 桌面 ↔ headless：`AGENT24_HEADLESS_PROTOCOL = 2`（`creative-serve-web.ts:73,146-185`）+ `/api/ready`；OD ↔ Agent24：ACP 桥 `agent24.open-design.v1`（ADR-OD-003），实现在 `rust/apps/agent24-cli/src/acp.rs`；状态枚举 `SidecarState`（`apps/desktop/src/main/sidecar-contract.ts:56`） |
| 状态 | 安装状态 `OpenDesignComponentState` 和运行状态 `headlessPhase: SidecarState` 都在 Electron main（`creative-serve-web.ts:245,267`）；daemon 只在内存里保存桌面上报的状态，TTL 90 秒（`d/component_routes.rs:35`） |
| 生命周期 | 桌面 main：`showCreativeView` 按需 `service.start()`（`main.ts:129`），`will-quit` 时停（`main.ts:496`）；停止发 SIGTERM，headless 超时 `HEADLESS_STOP_TIMEOUT_MS = 60_000`，**不强杀**（`creative-serve-web.ts:69,563,634-641`） |
| 权限 | OD 回调 Agent24 走 `agent24 acp` → `connect()`：legacy 模式下是 `daemon.json` 全权 token，capabilities 模式下是 enroll 得到的 `FirstPartyShell`（`rust/apps/agent24-cli/src/main.rs:959-971`，`d/capabilities/first_party.rs:340`）。ADR-OD-005 定的 `CreativeRuntime` audience 和 `/api/v1/capabilities/creative` 铸币路由都在（`d/capabilities/types.rs:17`，`d/server.rs:1487-1494`），但**桌面没有调用方**（只有拒绝测试 `backend-proxy.test.ts:61`），OD-M11 暂停（`d/server.rs:4113`）。上报路由仅限 host（`d/server.rs:1036-1055`）。**K1：不涉及** |
| API | daemon 侧只有 `POST /api/v1/components/open-design/report`；OD 自己的 HTTP 在 `127.0.0.1:<port>`，由桌面 `WebContentsView` 加载 |
| /components | **上报接入**（C3b/C8 部分）：`open-design-report.ts` 每 30 秒心跳（`:11,200-208`），只在 capabilities 模式上报（legacy 模式下 report 路由返回 403，`d/server.rs:1068-1073`）。所以 **legacy 模式下 `/components` 里 OD 永远是 `absent`、`not_reported`** |

#### U9 MCP server / U10 进程内 DomainModule

| 属性 | MCP | 进程内 DomainModule |
|---|---|---|
| 分发 | 用户的 `~/.agent24/mcp.json`（`d/mcp.rs:1-8,83-84`）；无锁、无摘要、无签名 | 与本体同仓编译；无实例 |
| 契约 | MCP 规范；rmcp stdio 握手（`agent24-mcp/src/lib.rs:127-143`），不做 Agent24 版本协商 | `trait DomainModule`（`agent24-domain/src/lib.rs:1402`） |
| 状态 | `AppState.mcp_servers` 只是句柄列表（`d/server.rs:152`），**没有状态枚举** | — |
| 生命周期 | daemon 启动时挂载一次（`d/server.rs:2344-2351`），连接超时 6 秒、总预算 10 秒（`d/mcp.rs:31,43`）；**没有重连、启停接口** | — |
| 权限 | 所有 MCP 工具都是 `RiskClass::External`，每次调用走审批（`agent24-mcp/src/lib.rs:279`）；环境变量白名单（`:103-110`）；**不走 K1 逐工具同意** | `KERNEL_GRANTS = [Events, Memory, Approval]`（`d/domain.rs:75-76`） |
| API | 无 `/api/v1/mcp*`；工具在 `/api/v1/tools` 里出现（`d/server.rs:1513`） | — |
| /components | **未接入**（`Form::Mcp` 已定义但没人用） | **未接入** |
| 工具名 | `mcp_{server}_{tool}`（`agent24-mcp/src/lib.rs:180-187`） | `<module>.<operation>`（`agent24-domain/src/tool.rs:435-441`） |

#### U12–U16 外壳与客户端

| 单元 | 生命周期 | 身份 / token | 协议检查 | 读组件状态 |
|---|---|---|---|---|
| 桌面 | `BackendManager` 先复用已有 daemon，没有就自己拉起：capabilities 模式 `agent24d serve --port 0 --auth-mode capabilities --host-bootstrap-stdio`，stdin EOF 即关停；legacy 模式 `serve --port 0`（`apps/desktop/src/main/backend-manager.ts:381-396,503-545`）；非 Windows 的 Rust 后端选 capabilities，其余 legacy（`:135-137`） | capabilities 模式下 `ProductHost`（`product_host_token`），legacy 模式下全权 token（`:415-423`） | `protocolCompatibility`，`CLIENT_PROTOCOL_VERSION = 1`（`:520`；`protocol-compatibility.ts:10`） | 是：`useComponentList` 每 5 秒拉 `/api/v1/components`（`renderer/components/useComponentList.ts:4`、`componentApi.ts:81`）；**同时**本地模型页拉 `/idoris/status`，通信页拉 `/comm/*` |
| CLI / TUI | 找已有 daemon，没有就起临时 daemon（`rust/apps/agent24-cli/src/main.rs:986-1015`） | `AuthMode` 默认 `LegacySingleToken`（`agent24-protocol/src/state_file.rs:16-23`；`agent24d serve --auth-mode` 默认值 `rust/apps/agent24d/src/main.rs:155`）；capabilities 模式经 `enroll.sock` 得到 `FirstPartyShell`（`main.rs:959-971`；`d/enroll_endpoint.rs:13,22-23`，macOS 验团队 ID） | `protocol_compatibility`，要求完全相等（`agent24-protocol/src/version.rs:11,18-26`） | `agent24 components`（`main.rs:75-78,1324-1345`） |
| `agent24 acp` | 由 Open Design 拉起（`main.rs:120-122`） | 同 CLI 的 `connect()` | 同 CLI | 否 |
| nostr / wechat bridge | 开发态手动起（`tsx src/main.ts`） | 读 `daemon.json` 或 `A24_TOKEN`，**只能用 legacy 全权 token**（`packages/nostr-bridge/src/config.ts:112-134`、`packages/wechat-bridge/src/config.ts:39-42`） | 未核实 | 否 |

### 1.3 本体子系统（U1 展开）

只列与组件边界相关的部分。完整路由表在 `d/server.rs:1451-1664`，策略表在 `d/capabilities/route_policy.rs:62-173`。

| 子系统 | 位置 | API 前缀 | 状态存放 | 守护 Operation |
|---|---|---|---|---|
| runs / sessions / chat | `agent24-agent`，`d/runs.rs`、`routes.rs` | `/api/v1/{sessions,runs,chat}` | `agent24.db` | Session*/Run*/ChatCreate |
| 模型路由 | `agent24-models`（`ModelRouter::from_env`，`d/server.rs:2128`） | `/api/v1/models` | env + `idoris-connection.json` | ModelsRead |
| 记忆 | `agent24-memory`，`d/memory_routes.rs` | `/api/v1/memory/*` | `memory.db` | Memory* |
| 调度 | `agent24-scheduler` | `/api/v1/schedules*` | `agent24.db` | Schedule* |
| 审批 / 审计 / 模块审批 | `agent24-policy`，`d/approvals.rs`、`module_approvals.rs` | `/api/v1/approvals*`、`/api/v1/module-approvals*` | `agent24.db` | ApprovalStatusRead/ApprovalDecision |
| K1 同意 | `agent24-store/src/module_consents.rs`，`d/consent_routes.rs` | `/api/v1/module-consents*` | `agent24.db` `module_consents` | ConfigRead/ConsentRevoke/ApprovalDecision |
| capability token | `d/capabilities/*`，`d/enroll_*` | `/api/v1/capabilities/*` | **只在内存**（`capabilities/store.rs`） | CapabilityMint/Revoke |
| 组件状态汇总 | `d/component_routes.rs` | `/api/v1/components*` | 只在内存（上报 TTL 90 秒） | ComponentsRead/ComponentsReport |
| OS / attach 宿主 | `d/domain.rs`、`os_routes.rs`、`attached*.rs` | `/api/v1/os*`、`/api/v1/attached*` | `os.json`、`attached.json`、`os-enable-marks.json` | ModuleRead/ModuleAdmin/ModuleRoute |
| MCP host | `d/mcp.rs` | 无 | `mcp.json` | — |
| 生命周期 / 关停 | `d/lifecycle.rs` | `/api/v1/shutdown` | `last-shutdown.json` | Shutdown |
| 迁移协调器 | `d/migration_coordinator.rs` | 诊断路由 | `ENABLED = false`（`:20`） | DiagnosticRead |
| 硬件 | `agent24-hw`，`d/hardware.rs` | 无（HW-3） | `hardware.json` | — |

`Audience` 只有三个变体：`ProductHost`、`FirstPartyShell`、`CreativeRuntime`（`d/capabilities/types.rs:14-18`）。**没有按组件区分的 audience**，ADR-038 D7「令牌由 CORE-2 按组件发放」在客户端这一侧还没有对应的东西。组件进程拿到的都是各自机制的私有凭据：OS 包是握手 token，attach 是注册 token，iDoris 是 Admin token，Hyphae 和 MCP 没有凭据。

---

## 2. 不一致清单

严重度：**高** = 伤用户数据或安全判断；**中** = 同类事物多套机制，或文档误导设计；**低** = 命名或文档滞后。

### 2.1 同一类事物，多种机制

| # | 问题 | 证据 | 严重度 |
|---|---|---|---|
| I-1 | **下载器 2 套在用、1 套空缺、1 套计划中**：Rust `agent24-components::download_locked_asset`（iDoris、Hyphae）；TS `open-design-component.ts`（OD，另写一份同样的白名单，加断点续传）；OS 包没有下载器（`agent24 os install` 只复制本地目录）；ADR-DOC-03 计划为 SuperDoc/LibreOffice 做「通用的按需组件安装器」（`docs/documenting/adr/ADR-DOC-03-docx-engine-and-conversion.md:198` 列为 DOC-1 第 2 片） | `agent24-components/src/download.rs:29-33`；`open-design-component.ts:90`；`agent24-os-packages/src/install.rs:113` | 中 |
| I-2 | **完整性和签名四档并存**：iDoris/Hyphae 是 sha256 + codesign + Hardened Runtime + spctl；OD 只有 sha256 + 入口文件重哈希；OS 包（Documents、Sin90、Cos72）只对 `domain-os.yml` 做摘要，**可执行文件 `bin/<name>` 既不哈希也不验签**；AgentEar、MCP 完全没有。ADR-038 D1 的免锁条款只豁免「同仓构建 / 自装 / 远端」，**Sin90、Cos72 是外仓发布的二进制，不在任何一条豁免里**，却也没有锁 | `agent24-components/src/sign.rs:22-25`；`agent24-os-packages/src/discovery.rs:261-266`；ADR-038:84 | **高** |
| I-3 | **停止宽限 4 个来源**：全局 `A24_COMPONENT_STOP_GRACE_MS`（默认 15 秒，`d/lifecycle.rs:51,56`）；iDoris 自己的 `Timings.terminate = 17s`（`agent24-idoris/src/supervisor.rs:158`）；OD headless 60 秒且不强杀（`creative-serve-web.ts:69`）；锁里的 `stop_grace_ms` 字段（Hyphae 写了 15000）**没人读** | 同左；ADR-038 §7.11 自述「暂未接线」 | 中 |
| I-4 | **状态上报三种通道**：daemon 拉（iDoris、Hyphae、OS 包、attach）；桌面推（OD，HTTP 心跳 + TTL）；桌面推 binding、daemon 推断（Knowledge）。桌面推送**只在 capabilities 模式有效**，legacy 模式下 OD 永远 `absent`。daemon 的 `--auth-mode` 默认是 legacy（`rust/apps/agent24d/src/main.rs:155`） | `open-design-report.ts:99-105`；`d/server.rs:1068-1073` | 中 |
| I-5 | **同意 / 确认 5 种形态**：K1 逐工具同意（OS 包，`module_consents`）；attach 的 `allow_relax`（A3 §3.5）；iDoris 的 `ConnectConsent`（`provision.rs:19-24`）+ `{"consent":true}` 安装；Hyphae 安装走桌面原生确认（`apps/desktop/src/main/ipc/index.ts:631-638`），不走 daemon 在场证明；MCP 只有每次调用审批，没有启用同意。ADR-038 D7 要求「首次启用按 ADR-K1-03 §1、§4」，目前只有 OS 包做到 | 同左；`d/capabilities/first_party.rs:482-497` | 中 |
| I-6 | **工具注册三种身份**：OS 包是 `<module>.<operation>` 且走 K1；MCP 是 `mcp_{server}_{tool}`、`RiskClass::External`；Knowledge 是 `knowledge_search`/`knowledge_read`，**来源 `"builtin"`**。按 ADR-039 Knowledge 是能力、不是本体，它的工具却以本体身份出现，绕开了 ADR-K1-01 的命名和 K1-03 的逐工具同意 | `agent24-domain/src/tool.rs:435-441`；`agent24-mcp/src/lib.rs:180-187,279`；`d/knowledge_tools.rs:31-32,88` | 中 |
| I-7 | **OD 版本 pin 写了 4 份**：`pack-open-design-component.mjs:36-37`、`prepare-open-design-linux-resources.mjs:13`、`stage-open-design-resources.mjs:5`、运行时 `creative-serve-web.ts:70`。iDoris/Hyphae 的版本只在锁里 | 同左 | 低 |

### 2.2 归类、形态与 ADR 不符

| # | 问题 | 证据 | 严重度 |
|---|---|---|---|
| I-8 | **Documents 的形态写错**：ADR-038 §1 表写「桌面 main 进程拉起 `agent24-documents` CLI（`documents-cli.ts`）……同仓库构建，不需要锁」。代码里 `documents-cli.ts` 拉起的是 `agent24 documents …` 外壳 CLI（`apps/desktop/src/main/documents-cli.ts:1-6,32-37`），`agent24-documents` 是 daemon 监管的进程外 OS 包，而且**不随安装包分发、要手工装**（README:7）。「同仓构建所以免锁」的前提（随本体一起分发）不成立 | 同左；`d/component_routes.rs:305` | 中 |
| I-9 | **`Form` 枚举和 D4/D5 对不上**：D4 列了「进程内」和「JSON-RPC attach / 领域 OS」两种绑定，`Form` 只有 `Resident/Sidecar/Cli/Attach/Remote/Mcp`。OS 包（Documents）被记成 `Resident`，但 D5 的 resident 子集是「全集」，含 `absent/installing/installed`，而 OS 包没有 daemon 管的安装流程；Sin90/Cos72 和进程内模块没有可用的 Form | `agent24-components/src/state.rs:32-45,75-86` | 中 |
| I-10 | **Hyphae 被记成纯 Resident**：实际同时是常驻 daemon 和按次 CLI。D5 的「按次 CLI」子集和 C2 的 `contract`/`health` 都是针对 CLI 形态写的，现在一个 Form 装不下两种 | `d/component_routes.rs:237`；ADR-038 D5 | 低 |
| I-11 | **Open Design 的 sidecar 由桌面托管**：符合 ADR-OD-004「Electron main 是 sidecar 生命周期唯一 owner」，但 ADR-038 D4 写「目标是 `SidecarManager`」。`SidecarManager`（TS）和 `agent24-sidecar-host`（Rust）两套托管器**都没有进入产品路径**，Knowledge Lite 的设想也依赖它 | `apps/desktop/src/main/sidecar-manager.ts:151`（无非测试引用）；`rust/apps/agent24-sidecar-host/src/lib.rs:9-30` | 中 |
| I-12 | **渠道桥是「持全权 token 的客户端」**：`nostr-bridge`、`wechat-bridge` 按 ADR-039 应是通信 / 渠道能力（受 D7 `requires` 授权），现在只能用 legacy 单 token，等于拿到全部 daemon 权限。ADR-038 D7 写明「组件不得持有 legacy 全权 token」 | `packages/nostr-bridge/src/config.ts:112-134`；ADR-038 D7 | **高**（目前限开发态） |
| I-13 | **ADR-035 与实现不符**：ADR-035 定的是「可信 launcher 通过匿名 FD/HANDLE 一次性交付」，实现是 daemon 自己铸 token 走 stdin，桌面 FD 路径已删（IL-13）。ADR 本身没有标被替代 | `agent24-idoris/src/supervisor.rs:874`；`idoris-admin.test.ts:5-8`；`docs/adr/README.md` 仍标「已接受」 | 低 |

### 2.3 还没有契约文档的单元

| # | 单元 | 现状 | 严重度 |
|---|---|---|---|
| I-14 | **通信（Hyphae / Nostr）没有 ADR** | `docs/architecture/README.md`「暂无独立 ADR」；决策散在 `COMM-HYPHAE.md` §0、ADR-037 决定 6、ADR-038 §7；COMM-FRIENDS 已拍板但只是设计稿 | 中 |
| I-15 | **能力 id 登记表不存在** | `docs/capabilities/` 目录不存在；ADR-038 D9 和 ADR-040 D1 都依赖它（ADR-040：C7 之前技能只能 `requires: []`） | 中 |
| I-16 | **iDoris 能力列表来源缺失** | ADR-038 D4：「iDoris 没有 `/capabilities` 端点，能力列表待 iDoris 补」；目前 `provides` 只来自锁（期望），没有「实际」可比，D9 的实际⊂期望规则无从执行 | 低 |
| I-17 | **sidecar 托管契约** | `sidecar-contract.ts` 的 `SidecarState` 有 `healthy`，D5 没有；两套托管器都没上线，契约只在 TS 类型里 | 低 |
| I-18 | **技能** | ADR-040 提议、`SPEC-SKILL-DISTRIBUTION.md` 未实现、无代码 | —（已知） |

### 2.4 状态或权限的双重来源

| # | 问题 | 证据 | 严重度 |
|---|---|---|---|
| I-19 | **Hyphae 两个状态路由会说不同的话**：`/components` 的 `from_hyphae` 只认 `running/stopped/not_configured`，**`starting`、`backoff`、`locked` 一律映射成 `failed`**（`d/component_routes.rs:177-185`）。D5 有 `starting`；`locked`（口令未解锁）也不是崩溃。版本：`/components` 只要有 supervisor 就报 `lock.version`，而 supervisor 可能跑的是 `A24_HYPHAE_BIN` 或旁路二进制（`d/comm_routes.rs:252-268`）；`/comm/daemon` 的 `installed_version` 每次重算哈希。没装时：`/components` 报 `absent`，`/comm/daemon` 报 `process.state: "not_configured"` | 同左；`d/comm_routes.rs:731-757` | 中 |
| I-20 | **iDoris 契约阈值不一致**：锁 `min_minor: 1`（≥1.1）；supervisor 用锁判断；模型网关 `contract_version_accepted` 只要求 ≥1.0.1。用 `IDORIS_URL` 指向非托管 iDoris 时（`agent24-models/src/router.rs:359-363`），1.0.x 的 iDoris 会被网关接受，而托管路径会拒绝 | `idoris.lock.json:9`；`agent24-models/src/idoris.rs:480-492` | 中 |
| I-21 | **iDoris 模型来源两条配置路**：托管的 `IdorisHost` 发布网关（`d/idoris_host.rs:1`「gateway publication」），环境变量 `IDORIS_URL`/`IDORIS_API_KEY` 也能直接建网关。两者同时存在时谁优先：**未核实** | `agent24-models/src/router.rs:130,252-271,359-363` | 低 |
| I-22 | **iDoris 的状态分两个根**：release 在 `~/.agent24/components/idoris/`，状态文件在 `~/Library/Application Support/Agent24/idoris/`。ADR-038 §7.9 有意如此，但「组件的东西在哪」从此要看两处，gc 也要排除两处 | ADR-038 §7.9 | 低 |
| I-23 | **组件状态在桌面上有两套读法**：能力中心读 `/components`，本地模型页每 1 秒读 `/idoris/status`，通信页读 `/comm/*`。ADR-038 D5 要求「三个页面都从 `/components` 读，旧路由保留为别名、内部读同一份」，目前只做到 iDoris 读同一个对象；Hyphae 两边逻辑不同（I-19） | `IdorisPanel.tsx:99`；`useCapabilityStatus.ts:17,22` | 中 |
| I-24 | **Knowledge 的 `ready` 不可信**：有 binding 就报 `ready`，没有任何探测；远端挂了也显示 `ready` | `d/component_routes.rs:335-355` | 中 |
| I-25 | **K1 同意的生效依赖一个默认关闭的环境变量**：`A24_MODULE_TOOL_GRANTS` 不开时 `decide` 返回 `grants_closed`；同时 `os install` 先撤销同意再装。状态在 `agent24.db`，启用标记在 `os.json`，代际在 `os-enable-marks.json`，三处合起来才是「这个模块能不能被调用」 | `d/consent_routes.rs:444,552-558`；`d/os_config.rs:121` | 低 |

---

## 3. 完成度：ADR-038 C1–C8 与 COMPONENT-ROADMAP

### 3.1 ADR-038 切片

| 片 | 内容（ADR-038 §4） | 状态 | 证据 |
|---|---|---|---|
| C1 | `ComponentLock` schema 2、通用下载器、D2a 签名、依赖反转、iDoris 锁升 schema 2、迁到新托管根 | **已落地** | #1539（`7e9bc3b1`；提交 `0d009848` C1a、`aa0f09f4` iDoris 锁 schema 2）；`agent24-components/src/{lock,download,sign,install}.rs`；`agent24-idoris/Cargo.toml` 依赖 `agent24-components`；`fetch_verified` 已删（grep 零命中） |
| C1b | Hyphae 签名、schema 2 锁、Agent24 对 Hyphae 启用 D2a | **已落地**（Agent24 侧） | #1551（`af4b1603`）、#1552（`e8b58246`，拒绝原因信号）；`hyphae.lock.json:61` `"schema": 2` |
| C2 | Hyphae `contract`/`health` 子命令；Agent24 写死契约期望并做握手检查 | **部分**：Hyphae 侧子命令已随 v0.29.0/v0.30.0 发布（#1546 `612f0f0f` 升锁）；**Agent24 侧没有调用 `contract`/`health`**，Hyphae 状态仍靠进程存活判断，没有 `degraded` | `d/hyphae_install.rs:304`（只在测试里）；`d/component_routes.rs:173-176` |
| C3（C3a） | `GET /api/v1/components`、report 路由、统一状态机、宽限统一 | **已落地** | #1545（`77790807`）；`d/server.rs:1453-1457`；`route_policy.rs:67-68`；`agent24-components/src/state.rs`；`GRACE_DEFAULT_MS = 15_000`、`GRACE_MAX_MS = 30_000`、`A24_COMPONENT_STOP_GRACE_MS` 及旧名别名（`d/lifecycle.rs:51-58,87-92`）。**未完成部分**：「能力中心、通信页、本地模型页改读这个接口」只做了能力中心（I-23） |
| C3b | 桌面上报 + 30 秒心跳 | **已落地** | #1549（`01f49e3a`）；`open-design-report.ts:11-12,200-208` |
| C4 | AgentEar：在 `/components` 登记（attach 子集），补 §3 用例 2、3、4 | **只有通用覆盖**：`attach_items` 列出所有 attach 模块；没有 AgentEar 专项代码，§3 用例是否补齐**未核实** | `d/component_routes.rs:286-300` |
| C5 | Knowledge：`/system/capabilities` 映射到 D3/D5 | **未开始**：daemon 没接 Provider、没有探测 | `d/knowledge_backend.rs:6`；`d/component_routes.rs:335-336` |
| C6 | Documents：补 D9 清单 | **未开始**：`documents_item` 只有状态，`provides` 为空 | `d/component_routes.rs:304-333` |
| C7 | 领域 OS、进程内模块、MCP 的 D9 清单和状态接入；建 `REGISTRY.md` | **未开始**：Sin90/Cos72、MCP 不在 `/components`；`docs/capabilities/` 不存在 | `ls docs/capabilities` →「No such file or directory」 |
| C8 | OD：桌面上报（已随 C3b）；随 `SidecarManager` 迁到 sidecar 绑定 | **部分**：上报完成，迁移未做 | 见 I-11 |

`git log --oneline -600` 里没有任何提交或合并标题提到 C4–C8。

### 3.2 COMPONENT-ROADMAP（立于 2026-10-03）对照

| 路线图条目 | 路线图写的 | 代码现状 | 结论 |
|---|---|---|---|
| C2 模型网关「iDoris 未接」 | 🟡 | `router.rs:359-363` 已读 `IDORIS_URL`，`IdorisHost` 托管并发布网关 | **路线图过时** |
| C7 OD「按需下载组件 ✅」 | ✅ | `open-design-component.ts` 在线 | 一致 |
| C7「目标形态 🟡/📐」，OD-M11 暂停 | ⏸️ | `CreativeRuntime` 有路由无调用方；`SidecarManager` 未用 | 一致 |
| C8 DEP-C8「hyphae.lock 升级到含 daemon 锁」 | 波次 A/B | 锁已是 schema 2 v0.30.0 签名版（#1551） | **已完成，路线图未更新** |
| C8 COMM-FRIENDS | 排在 ADR-038 C1–C3 之后 | C1–C3a/b 已合，COMM-FRIENDS 无实现（`friends.json`、`/comm/friends` 零命中） | 前置已满足，可以排期 |
| C9 AgentEar「P0–P2 ✅」 | ✅ | attach 链路在线 | 一致 |
| C5 A-6 ME-6 模块签名 | 远期 | OS 包无签名（I-2） | 一致，但 I-2 说明它不只是「远期」 |
| Documents、Knowledge、组件契约 | **路线图里没有这几行** | — | 路线图缺项：结构组件表 C1–C12 里没有 Documents、Knowledge，也没有 ADR-038 组件契约这条线 |

---

## 4. 统一化建议候选

按优先级排序。**只列候选，不做决定**；每条给收益、风险和第一步。

| 优先级 | 候选 | 收益 | 风险 / 代价 | 第一步（最小可验证） |
|---|---|---|---|---|
| P1 | **OS 包补完整性：锁或包摘要覆盖可执行文件**（对应 I-2） | 堵住「外仓二进制无任何校验」这个最实的安全缺口；Sin90/Cos72 与 iDoris/Hyphae 同一信任模型 | 改 `agent24 os install` 和启动校验，影响现有已装包（需要迁移或首次补算）；第三方发布者信任根仍未定（ADR-038 D2a 放到 K1-03/ME-6） | manifest digest 扩为「包内全部文件的清单摘要」，启动时重算；先只记录、不拒绝，跑一个版本后再改为拒绝 |
| P2 | **统一状态上报：`/components` 成为唯一状态源**（I-19、I-23、I-24、C4–C7） | 能力中心、通信页、本地模型页说同一句话；技能依赖解析（ADR-040 D2）需要它 | 要改 Hyphae 映射（`starting`、`locked` 的语义）、给 Knowledge 加探测（C5）、把 OS 包和 MCP 也列进来；旧路由兼容期内仍是两套 | 让 `/comm/daemon` 和 `/idoris/status` 内部改读 `component_routes` 的同一个 `Item`；补「两条路由同一状态」的对照测试 |
| P3 | **统一契约声明：给 OS 包 / MCP / Knowledge 补 D9 清单，建 `REGISTRY.md`**（I-6、I-15、C6、C7） | 技能（ADR-040）解锁 `requires` 非空；能力中心能按能力展示；Knowledge 工具归位到组件身份 | `DomainOsManifest` 是 `deny_unknown_fields`，加 `provides` 需要升 `MANIFEST_SCHEMA_VERSION`，旧 daemon 会拒新 manifest；外仓（Sin90、Cos72、AgentEar）要跟着改 | 先建 `docs/capabilities/REGISTRY.md` 和 CI 校验（只校验已有的 iDoris/Hyphae 锁）；Documents 作为第一个 OS 包补 `provides` |
| P4 | **统一 Form 与形态定义**（I-9、I-10） | `Form` 和 D4 一一对应，OS 包不再冒充 Resident，状态子集正确 | 改 `agent24-components::Form` 是对外 wire（`/components` 的 `form` 字段），要进 v1 契约测试 | 增加 `Package`（进程外 OS 包）和 `InProcess` 两个变体并定状态子集；Hyphae 是否拆成两项另议 |
| P5 | **统一组件安装器：Rust 下载器成为唯一实现**（I-1、I-7） | DOC-03 的 SuperDoc/LibreOffice 不用再写第四套；OD 的断点续传可以回流给 iDoris/Hyphae；pin 只在锁里 | OD 下载迁进 daemon 是 ADR-038 明确排除在 v1 外的事；桌面离线 / legacy 模式下 daemon 未起时 OD 无法安装；断点续传要移植 | 先让 DOC-03 的引擎走 `agent24-components`（新单元，不迁旧的）；OD 的 pin 收成一份 `open-design.lock.json` 供 4 处脚本读取 |
| P6 | **统一停止宽限：锁里的 `stop_grace_ms` 按组件生效**（I-3） | 一个字段说清每个组件的落盘时间；iDoris 17 秒、OD 60 秒这类特例显式化 | ADR-038 §7.11 已说明：关停总预算按全局值算，单组件更长的宽限会被截断，要先改总预算模型 | 先在 `/components` 主机视图里报出每个组件实际使用的宽限值，暴露差异 |
| P7 | **统一同意入口：首次启用都走 ADR-K1-03**（I-5、I-25） | 用户看到同一种确认；D7「requires ∩ 宿主 ∩ 用户」三者有统一落点 | iDoris、Hyphae 不暴露模块工具，K1 的「逐工具」语义不直接适用，需要定义「服务能力的启用同意」；可能需要修订 K1-03 | 列一张「每类组件的同意落点表」进 ADR-038 修订稿，先统一审计字段（K1-04），不改交互 |
| P8 | **按组件发 token，去掉渠道桥的 legacy 全权 token**（I-12、D7） | 符合 ADR-038 D7；为 ADR-041 入站统一准入做准备 | 依赖 CORE-2 按组件 audience（现在只有 3 个 audience）；渠道桥目前只是开发态，收益要等它进产品才兑现 | 先在 ADR 里把 nostr/wechat bridge 归类为「渠道能力」并登记，产品化前禁止 legacy token |
| P9 | **通信写 ADR**（I-14） | Hyphae 的形态、锁、契约、状态、F4b 冻结、COMM-FRIENDS 的上位决策集中到一处 | 纯文档成本；可能和 ADR-041（入站）重叠，要划清边界 | 从 `COMM-HYPHAE.md` §0 和 ADR-038 §7 抽出已定决策，写成 ADR 草案 |
| P10 | **文档纠偏**（I-8、I-13、§3.2） | 避免按错文档做设计 | 低 | 改 ADR-038 §1 的 Documents 行；ADR-035 标注被 IL-13 部分取代；COMPONENT-ROADMAP 补 Documents、Knowledge、ADR-038 三行，更新 C2、C8 |

---

## 5. 需要 jason 拍板的问题

1. **Sin90 / Cos72 这类外仓 OS 包要不要立刻补完整性校验**（P1）？还是维持「发布清单人工 `shasum -c`」到 ME-6？
2. **`/components` 是否定为唯一状态源**，旧的 `/idoris/status`、`/comm/daemon` 在多少个版本后只做别名（P2）？Hyphae 的 `locked`（口令未解锁）应映射成什么状态？
3. **Knowledge 的工具要不要从 `builtin` 改为组件工具**（`knowledge.search`），走 K1 逐工具同意（I-6）？这会让知识检索多一次首次启用确认。
4. **`Form` 是否新增 `Package` / `InProcess`**，Hyphae 是否拆成「daemon」和「CLI」两项（P4）？
5. **Open Design 的下载是否列入 v2 迁进 daemon**，还是长期保留桌面 TS 下载器、只统一锁格式（P5）？
6. **服务能力（iDoris、Hyphae）的「首次启用同意」用什么形式**：沿用各自的确认框，还是修订 ADR-K1-03 统一（P7）？
7. **通信是否单独写 ADR**，以及它和 ADR-041（入站事件）的边界（P9）？

---

## 6. 未核实项

- 所有运行时行为（握手、签名、心跳、探测）只读了代码，没有起进程验证。
- `IDORIS_URL` 环境变量与托管 `IdorisHost` 同时存在时的优先级（I-21）。
- C4 的 §3 用例 2、3、4 是否已在测试里覆盖。
- Open Design fork 是否确实通过 `AGENT24_BIN` 调 `agent24 acp`（fork 侧代码不在本仓）。
- `nostr-bridge` / `wechat-bridge` 是否做 `protocol_compatibility` 检查。
- `decision_log` 表在 agent24d 侧是否有写入方。
- AgentEar 自己的安装器是否做制品校验（ADR-038 D2a 说「由 AgentEar 负责」，不在本仓）。
