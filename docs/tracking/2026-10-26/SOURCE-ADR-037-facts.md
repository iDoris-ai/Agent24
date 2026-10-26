# Agent24 产品形态与进程模型：代码现状事实清单

基线：main @ 3a433bb0（2026-10-26）。只读调查，未构建、未运行。
路径缩写：`R` = `rust`，`D` = `apps/desktop`，`DOC` = `docs`。

---

## 1. daemon 怎么启动、生命周期归谁管

### 1.1 桌面端 BackendManager 拉起 agent24d
- 文件 `D/src/main/backend-manager.ts`，类 `BackendManager`，模块级变量 `currentEndpoint`。
- 默认后端是 Rust：`selectedBackend()` 只有 `AGENT24_BACKEND=node` 才选 node（`backend-manager.ts:selectedBackend`）。
- 拉起命令：`defaultSpawnProcess` → `spawn(bin, ['serve','--port','0'], {stdio:['ignore','pipe','pipe'], env:{...process.env}})`。**不传 `--auth-mode`，即 legacy 单 token**；没有 `--ephemeral`，即非临时、带单例锁和发现文件。
- 二进制位置 `resolveRustBinary`：`AGENT24D_BIN` 优先；开发态 `rust/target/debug/agent24d`；打包态 `process.resourcesPath/backend/agent24d(.exe)`。
- 端口与 token 的传递：端口 0，由 OS 分配。BackendManager 逐行读子进程 stdout，取第一行 `{"type":"ready","port":…,"token":…}`（SPEC-002 §4），存进 `currentEndpoint`（`wireChild`）。渲染进程拿不到 token，只能拿端口（`IpcChannels.BackendEndpoint` 只返回 `{port}`，`D/src/main/ipc/index.ts:290`）。
- 复用外部 daemon（DEP-A5）：`start()` 先 `tryReuseExternal()`，读 `daemon.json`（`daemonStatePath()`：`AGENT24_DATA_DIR` 绝对路径优先，否则 `$HOME/.agent24/daemon.json`），带 token 探 `GET /api/v1/health`，健康就复用，不再 spawn。注释写明原因：非临时 agent24d 持有终身单例锁，第二个会失败。
- 退出处理：
  - `app.on('will-quit')` → `backendManager.stop()`（`main.ts:384-396`）。
  - `stop()`：自己 spawn 的子进程发 `SIGTERM`（`killChild`）；复用的外部 daemon 绝不杀，只断开（`disconnectExternal`）。
  - 健康检查每 5s 一次，连续 3 次失败就杀掉重启，重启前再探一次外部（`tick`，常量 `HEALTH_INTERVAL_MS/MAX_HEALTH_FAILURES`）。
  - 托盘菜单有启动/停止/重启 daemon（`main.ts:341-363`）；用户手动停止后不会自动拉起（`userStopped`）。
- 结论：**桌面端自己 spawn 的 daemon 是桌面的子进程，桌面退出即被 SIGTERM；只有先于桌面存在的外部 daemon 才会在桌面退出后继续活着。** 桌面端代码里没有安装或触发 LaunchAgent 的逻辑（grep `LaunchAgent|service install|openAtLogin` 在 `D/src` 无命中）。

### 1.2 agent24d 的 `serve` 子命令
- `R/apps/agent24d/src/main.rs`，唯一子命令 `Serve`，参数：
  - `--port <u16>`，默认 0；
  - `--ephemeral`，跳过单例锁和发现文件（CLI 独立模式用）；
  - `--auth-mode legacy-single-token|capabilities`，默认 legacy；
  - `--host-bootstrap-stdio`（hidden），capability 模式必需，要求 stdin/stdout 是宿主私有管道。
- `server.rs:validate_auth_startup` 校验组合。Capability 模式的 `host_bootstrap` 只在 `#[cfg(unix)]` 有实现，非 Unix 是返回 `Unsupported` 的桩（`main.rs` 里 `mod host_bootstrap`）。
- 绑定：`server.rs:1894` `TcpListener::bind(("127.0.0.1", port))`，写死回环，注释 "never a public bind (SPEC-001 §9)"。
- 信号：unix 下处理 SIGTERM + SIGINT（`server.rs:1361-1371`）；另有鉴权 `POST /api/v1/shutdown` 和 `GET /api/v1/shutdown`（报告），见 `server.rs:733/745`。

### 1.3 有没有常驻模式
- 有，但只有 macOS LaunchAgent，且是 CLI 手动安装：`R/apps/agent24-cli/src/service.rs`。
  - `LABEL = "ai.auraai.agent24"`；plist 路径 `~/Library/LaunchAgents/ai.auraai.agent24.plist`；日志 `state_dir/logs`。
  - `render_plist`：`ProgramArguments = [agent24d, serve, --port, 0]`，`RunAtLoad=true`，`KeepAlive{SuccessfulExit:false}`，`ThrottleInterval=10`，`ProcessType=Background`，可注入环境变量（`capture_env`）。
  - `install`/`uninstall`/`status` 用 `launchctl bootstrap/bootout/print gui/<uid>`。
  - CLI 子命令 `agent24 service install|uninstall|status`（`main.rs:cmd_service`）。
- **没有 systemd unit、没有 Windows service/任务计划**（grep `systemd`、`schtasks` 于 `R` 无实现；`service.rs` 自述 "macOS LaunchAgent"）。
- `agent24 daemon start|stop|status`（`main.rs:cmd_daemon`）：非临时 spawn 后 `drop(child)` 脱离，代码注释写 "production autostart is F1's launchd/systemd job; this is the dev/manual path"。
- 桌面端 `BackendManager` 能复用 LaunchAgent 起的 daemon（走 daemon.json 发现），但不负责安装它。

### 1.4 发现文件
- `~/.agent24/daemon.json`（`AGENT24_DATA_DIR` 可整体改根）。定义 `R/crates/agent24-protocol/src/state_file.rs`：`DaemonState {port, token(legacy 才有), pid, version, generation, auth_mode}`。
- 写入时机：ready 行之前（`server.rs:2441-2467`）；优雅退出删除，且 `remove_if_owner(pid)` 在锁下只删自己的。
- 权限：Unix 下目录 0700、文件 0600（`state_file.rs:179,194`），非 Unix 没有对应设置（`ARCHITECTURE-LAYERS.md:236` 也说明）。
- capability 模式的 daemon.json **不含 token**，只含端口、pid、generation。
- 读取方 `read_live()` 用 pid 存活做陈旧判断。
- CLI 遇到 capability 模式的 daemon 会直接报 `host authority unavailable`（`main.rs:connect`、`attach_only`）。

### 1.5 两个 daemon 打开同一数据目录会怎样
- 有锁：`state_file.rs:try_acquire_singleton()` 对 `<state_dir>/daemon.singleton.lock` 做 `fs2::try_lock_exclusive`。非临时 daemon 在 `server.rs:1430` 取锁，失败直接返回错误 "another agent24d is already running (singleton lock held)"。持有到进程结束。
- `--ephemeral` 实例跳过锁，且用内存库（`Store::open_memory()`，`server.rs:1493` 附近），所以不会共享 SQLite。
- 数据目录由 `AGENT24_DATA_DIR` 隔离（MAINT-03，`state_file.rs:state_dir`）。
- SQLite：`R/crates/agent24-store/src/lib.rs:157-165` `Store::open`：sqlx，`journal_mode=WAL`、`busy_timeout=5s`、`foreign_keys=true`、连接池最大 5。`module_schedules.rs:1131` 另有一处 WAL 连接。
- 结论：同数据目录下第二个非临时 daemon 在拿到锁之前就失败，不会触达 SQLite；WAL 本身只是单进程内多连接并发，不是为多 daemon 设计。是否有其他进程（模块、CLI）直接开该 db：CLI 是 REST 客户端，未见直接开库（未穷尽核实，标「未能确认」）。
- Windows 上 `fs2` 的锁是跨平台的，但 daemon.json 权限、`host_bootstrap` 等见第 5 节。

---

## 2. 鉴权

### 2.1 legacy 单 token
- 生成：`server.rs:1225` `generate_token()`：32 字节 `rand::rng()`，hex 编码（64 字符）。每次启动重新生成（`server.rs:1479`）。
- 存放：写入 `~/.agent24/daemon.json` 的 `token` 字段，同时出现在 stdout ready 行（`server.rs:2483-2512`）。内存里是 `AppState.token`。
- 校验：`server.rs:949` `auth` 中间件，`constant_time_eq` 比较 `Authorization: Bearer`；仅 `GET /api/v1/health` 免认证（`server.rs:950`）。模块路由在 auth 层之前合并，所以也受保护（`build_router_with_modules`，回归测试 `module_routes_are_behind_kernel_auth`）。
- 这是一个全权 token：能调 run、approval、grant、shutdown 等所有路由（ADR-005 的"问题"一节）。
- 桌面端与 CLI 当前都走这条路径（`ARCHITECTURE-LAYERS.md:157`）。

### 2.2 capability 令牌基础设施（代码都在 `R/apps/agent24d/src/capabilities/`）
- 文件：`mint.rs`（145 行）、`revoke.rs`、`store.rs`、`token.rs`、`types.rs`、`validate.rs`、`watch.rs`、`operations.rs`，`tests/` 目录。
- Audience 两种：`ProductHost`、`CreativeRuntime`（`types.rs:Audience`）。
- Claims：`capability_id, audience, workspace_id, attachment_id, principal_id, daemon_generation, host_generation, sidecar_generation, created_at, expires_at, allowed_operations`（`types.rs:CapabilityClaims`）。
- Mint：`mint.rs:mint_product_host`（TTL 可到 `u64::MAX`，daemon 启动时调用一次，`server.rs:1481`）、`mint_creative` / `mint_creative_authorized`；Creative TTL 上限 `MAX_CREATIVE_TTL_SECONDS = 3600`。
- 吊销：`revoke.rs` `revoke(token)`、`revoke_by_id`、`revoke_by_generation`、`revoke_by_daemon/host/sidecar_generation`、`epoch()`。
- 代际：daemon_generation 在启动时生成（`server.rs:1477`）并写入 daemon.json 和 ready；校验遇到旧代际返回 `StaleGeneration`。
- 校验 API：`validate_bearer / validate_action / revalidate(_bearer)`；错误枚举 `CapabilityError`（`Unauthorized, Expired, Revoked, StaleGeneration, OperationDenied, ResourceDenied, HostRequired, InvalidTtl`）。
- 操作枚举 `Operation` 共 22 个（`operations.rs`）：其中 creative 白名单 10 个（`ModelsRead, WorkspaceResolve, SessionCreate/Read/Transcript, RunCreate/Read/Cancel, EventsRead, ApprovalStatusRead`），其余为仅宿主（`ModelsAdmin, WorkspaceCreate/Release, HostLease, ApprovalDecision, GrantManage, OverrideManage, ScheduleManage, ModuleAdmin, CapabilityMint/Revoke, Shutdown`）。
- 启用方式：`--auth-mode capabilities --host-bootstrap-stdio`，ProductHost 令牌只经 ready 管道交给宿主父进程，不落盘（`server.rs:2483-2526`）。**桌面端与 `agent24 acp` 都没有使用该模式**（`ARCHITECTURE-LAYERS.md:158`）。

### 2.3 哪些路由真的检查了 capability（capability 模式下）
- `server.rs:1016` `required_operation(method, path)` 只有 4 个分支：
  - `GET /api/v1/models` → `ModelsRead`；
  - `POST /api/v1/capabilities/creative` → `CapabilityMint`；
  - `POST /api/v1/capabilities/*/revoke` → `CapabilityRevoke`；
  - **其余一律 `ModelsAdmin`（仅宿主）**。
- 中间件校验时资源固定传 `Resource::global()`（`server.rs:991`），没有从请求里提取 workspace/session/run。
- 所以 capability 模式下，非宿主令牌（CreativeRuntime）能访问的受认证路由只有 `GET /api/v1/models`（`ARCHITECTURE-LAYERS.md:158` 同样结论）。
- 未见 `required_operation` 以外的路由逐个检查 capability 的代码（在 `agent24d/src` 内 grep `validate_action` 未发现业务路由使用；未穷尽，标「未能确认」是否有个别路由内再校验）。
- 注意 `R/apps/agent24d/src/authz.rs` 与此无关：它是模块私有空间的 `Authorizer`（`ModulePrivateOnly`），`ActiveScope` 是占位。

### 2.4 「路由级资源授权」现状（对照 `ARCHITECTURE-LAYERS.md`）
- `ARCHITECTURE-LAYERS.md:43,157-158,236-237,287` 标 📐（仅设计）的内容：workspace/session/run 资源提取与校验、durable 的 session/run 归属、事件过滤与每次发送前复验、broker/handoff；另外 `:288` Creative 按 workspace 的非持久 session 分区；`:290` workspace 对外路由未开放。
- 代码对照：与上面 2.3 一致，`required_operation` + `Resource::global()` 就是全部；`Resource` 类型（`types.rs:Resource`，含 workspace/attachment/principal/session/run 构造器）已定义但中间件未用。
- 文档里另一条：桌面 `backendProxy` 接受渲染进程任意路径并带 token，高权限接口隔离未做（`ARCHITECTURE-LAYERS.md:158`，PLAN-OD-NEXT F11.0）。代码印证：`D/src/main/ipc/index.ts:316` `BackendProxy` 只校验 "valid method and absolute path"，之后 `proxyToBackend(req)` 附 token。
- 设计依据：`DOC/open-design-workspace/design/ADR-005-CAPABILITY-AUTHORITY.md`（Accepted 2026-09-19）。

### 2.5 监听地址、CORS、Origin
- 只绑 `127.0.0.1`（`server.rs:1894`）。没有 bind-address 配置项。
- **没有 CORS**：`agent24d` 内无 `CorsLayer`、`tower-http` 依赖、`access-control` 字样（grep 无命中）。
- **Origin 校验只做在 WS**：`R/apps/agent24d/src/events.rs:68` `ws_events`，请求带 `Origin` 头（含 `Origin: null`）就 403，防浏览器 CSRF（SPEC-002 §4 / `SPEC-002-protocol.md:187`）。REST 路由没有 Origin 校验，也没有 Host 头校验（DNS rebinding 防护，grep `header::HOST` 无命中）；REST 靠 Bearer 头挡浏览器简单请求。
- ADR-027「遗留」一节也写：要远程消费需 bind-address 配置路径（现硬编码 127.0.0.1）+ 远程认证/配对，浏览器 Web 壳还要处理 Origin/CSRF 拒绝。

---

## 3. TUI / CLI 现状

### 3.1 `agent24` 子命令（`R/apps/agent24-cli/src/main.rs`，`enum Command`）
`chat`、`models`、`service`（install/uninstall/status）、`daemon`（start/status/stop）、`tui`、`os`（list/enable/disable/install/uninstall/…）、`comm`（identity/contact/relay/import/unlock/daemon/send/history/pull/outbox/status）、`mcp`（stdio MCP server）、`acp`（stdio ACP 桥）、`decide`（export/delete，不走 daemon）、`hw`（probe，不走 daemon）。另有 `attach`（附着模块 add/list/remove，在 `os` 或顶层的子动作里，见 `main.rs:198-250`）。

### 3.2 TUI 视图（`R/apps/agent24-cli/src/tui/`）
- `mod.rs`（538 行）：连接与 WS/REST 对账；`app.rs`（874 行）：状态机，`Focus::{Runs, Approvals}` 两个焦点；`ui.rs`（246 行）：面板 Runs、事件流、Approvals 列表、"Approval required" 弹窗。
- 用到的 REST：`GET /api/v1/runs`、`GET /api/v1/approvals?status=pending`、`POST /api/v1/approvals/{id}`、`POST /api/v1/runs/{id}/cancel`、WS `/api/v1/events`（`tui/mod.rs:66-144`）。
- 文档定位：`tui/mod.rs` 头注释："a thin ops client … operator surface, not a second agent runtime"（ADR-026 决策 #11）。**没有聊天输入、没有 schedule/memory/os/comm/documents 视图**；`app.rs:278` 注释承认 `/api/v1/module-approvals` 未渲染。

### 3.3 `agent24 acp` 桥
- `R/apps/agent24-cli/src/acp.rs`（320 行）："Minimal ACP-over-stdio bridge for the Open Design prototype"：stdin/stdout 换行分隔 JSON-RPC，把 ACP session/prompt 映射到 daemon 的 REST + WS run 协议；执行仍在 agent24d（`acp.rs` 头注释）。用 `connect()` 得到的 `Endpoint`。设计见 ADR-003。

### 3.4 openapi 覆盖统计
- 方法：`protocol/openapi.yaml` 解析，统计 paths 下 get/post/put/delete/patch 的 operationId；再按路径前缀（取 `{` 之前部分）在 `rust/apps/agent24-cli/src/*.rs`（含 `tui/`）里做子串匹配，判断有无入口。**粗略，子串匹配可能高估，不核对请求方法。**
- 总数：**75 个 operationId**（`grep -c operationId` 也是 75）。
- CLI 或 TUI 有对应入口：约 **26 个**。分布：health、chat、models、decide/profile、os（list/update/stop/commands）、attached（4 个）、sessions（3 个）、runs（4 个，其中 TUI 用到 4 个）、approvals（TUI）、module-approvals、shutdown（2 个）。
- 无入口：约 **49 个**。缺口类别：
  - documents（14 个：capabilities、list/get、uploads、chunks、imports、jobs、render page、read text、find、extractions）；
  - schedules（8 个：CRUD + run_now/suspend/resume）；
  - sin90（9 个：directions、schedule-blocks、proposals、attention）；
  - memory（5 个：assertions、settings）；
  - tool-overrides / standing-grants / tools（6 个）；
  - module-consents（4 个）；
  - timings / usage（3 个）。
- openapi **不含**但 daemon 实有的路由：comm（`/api/v1/comm/*`，CLI 有对应）、capabilities mint/revoke、WS `/api/v1/events`、`/hardware`、`/decide/*` 的其余部分等（`server.rs` 路由约 85 条，openapi 75 个操作，两者未逐条比对）。
- openapi `servers` 仍列 `127.0.0.1:8765` 的 node mock 与动态端口 agent24d（`openapi.yaml:30-34`）。

### 3.5 TUI 自己起 daemon 还是连已有的
- 两者都有：`main.rs:cmd_tui` → `connect()`：先 `state_file::read_live()` + 健康探测，命中则 attached；否则 `spawn_daemon(true)` 起 **临时 daemon**（`--ephemeral`，`kill_on_drop`，退出时 `finish()` 杀掉，内存库，不共享数据）。
- 例外：发现文件是 capability 模式时，CLI 直接报错 `host authority unavailable`。
- 所有 CLI 请求用 `client()`：`no_proxy()`、`redirect(Policy::none())`、仅回环（FU-74）。

---

## 4. 桌面端

### 4.1 三个模块
- **BackendManager**（`backend-manager.ts`，476 行）：见 1.1，监管 agent24d（或 legacy node mock）的子进程、健康检查、复用外部 daemon、托盘状态。
- **CreativeServeWeb**（`creative-serve-web.ts`，687 行）：拉起 Open Design（Creative）的 web/headless 服务并给出 `origin`，状态 `stopped|starting|ready|failed|needs-download`；优先用显式 checkout，其次打包的 `prebundled/agent24-headless.cjs`，其次按需下载的组件（`~/.agent24/components/open-design/`，`open-design-component.ts`）。`main.ts:236` 创建，`will-quit` 时 stop。**这是当前在用的 sidecar 路径**，Open Design 通过 `agent24 acp` 访问 daemon。
- **SidecarManager**（`sidecar-manager.ts`，353 行）：带 ownership/generation/进程树控制的 sidecar 生命周期管理器，配合 `agent24-sidecar-host`（Rust）。**在 `main.ts` 里没有被引用**，只有自身和 `sidecar-manager.test.ts`（grep 结果）；`ARCHITECTURE-LAYERS.md:32-33` 也把它标 🟡 目标形态，现行是 CreativeServeWeb。

### 4.2 渲染进程怎么调 daemon
- 渲染进程**不直接 fetch**。`window.agent24.backendProxy({method,path,body})`（`preload.ts` 经 `contextBridge` 暴露）→ 主进程 `ipcMain.handle(IpcChannels.BackendProxy)` → `proxyToBackend`，由主进程带 token 发到 `127.0.0.1:<port>`（`D/src/main/ipc/index.ts:316`；`D/src/renderer/pages/agent/api.ts` 头注释 "never a direct fetch"）。
- `@agent24/api-client`（`packages/api-client/src/{index,openapi.d.ts,events.d.ts}`）是 openapi 生成的**类型定义包**，被 `D/src/renderer/pages/agent/api.ts`、`documents/*`、`shared/ipc-types.ts`、`main/documents-import.ts` 引用；它不负责传输。
- 文档类请求另有 `DocumentsRequest` 专用 IPC（`ipc/index.ts:294-297`）。
- 另有若干直接的 oMLX IPC（`OmlxDetect/Models/Start/Warmup/Stop`，`ipc/index.ts:138-190`），不经过 daemon。

### 4.3 electron-builder 配置（`D/package.json` 的 `build` 字段）
- 通用：`appId=ai.auraai.agent24`，`productName=Agent24`；`extraResources` 把 `packages/node-daemon/dist` → `backend/`（**所有平台都打进 node mock**），`.open-design-component` → 根。
- mac：`target: dmg`，`extraResources: rust/target/release/agent24d → backend/agent24d`。**只放 agent24d，不含 `agent24` CLI**（`build:rust` 虽编了 agent24-cli，但未进 extraResources）。没有 `hardenedRuntime/entitlements/notarize/identity` 配置。
- win：`target: nsis`，`agent24d.exe → backend/agent24d.exe`。
- linux：`AppImage` + `deb`，`agent24d → backend/agent24d`，`executableName=agent24-desktop`。
- CI：`.github/workflows/release-desktop.yml` 在 `ubuntu-22.04` 上 `electron-builder --linux AppImage deb`（`:252`）；没有 mac/win 桌面构建 job。`release.yml` 的 CLI/daemon 矩阵：`aarch64/x86_64-apple-darwin`、`x86_64/aarch64-unknown-linux-gnu`，**无 Windows**。`windows-check.yml` 是 `windows-latest` 上 `workflow_dispatch` 的 `cargo check` 摸底。

### 4.4 DMG 签名状态
- 本机仅有 `D/release/Agent24-0.1.0-arm64.dmg`；`codesign -dv` 输出 "code object is not signed at all"。
- 配置层面没有 identity/notarize；`.github/workflows` 中 grep `codesign|CSC_|notar|identity` 无命中。
- 文档：`DOC/Deployment/TASKS.md` DEP-B1/B2/B3/B5 全部 `BLOCKED`（等 Apple 开发者账号）；`DOC/Deployment/README.md:78` 说 v0.5.1 不发 macOS dmg。**与记忆里「10-05 未签名 DMG 也正常发布」的裁决不一致，TASKS.md 文字尚未更新，见第 8 节。**
- 现有 dmg 只有 arm64。

---

## 5. 跨平台要替换的部分

### 5.1 os-fd / os-cwd
- `agent24-os-fd`（`R/crates/agent24-os-fd/src/lib.rs`，290 行）：把内核继承的 listen fd 变成 `std::os::unix::net::UnixListener`。依赖 `rustix 1.1`（`fs`,`net`）；用 `BorrowedFd::borrow_raw` / `OwnedFd::from_raw_fd`（整个工作区仅有的 `unsafe` 之一）；`is_listening` 有 `cfg(target_vendor="apple")` 与非 Apple 两个分支。纯 Unix，无 `cfg(windows)`。
- `agent24-os-cwd`（`R/crates/agent24-os-cwd/src/lib.rs`，50 行）：`#![cfg(unix)]`，`install_current_dir_fd` 用 `CommandExt::pre_exec` + `rustix::process::fchdir`。整个 crate 在 Windows 上不存在。
- 同类 Unix 依赖：`agent24-workspace`（`[target.'cfg(unix)'.dependencies] rustix fs`，`service.rs` 33 处、`allocation_root.rs` 16 处、`root.rs` 13 处 cfg）、`agent24-os-proto`、`agent24-os-packages`、`agent24d`（`rustix`、`libc`、`attach_listener.rs` 的 UDS）、`agent24-comm`（`rustix flock`）。
- cfg 统计：在 `R/` 下 grep `cfg(unix)|cfg(not(unix))|cfg(windows)|cfg(target_os` 命中最多的是 `agent24-workspace/src/service.rs`(33)、`allocation_root.rs`(16)、`agent24-sidecar-host/src/target.rs`(15)、`root.rs`(13)、`os-packages/install.rs`(12)；含 `cfg(windows)`/`cfg(not(unix))` 的共 61 处（grep 粗计）。`agent24-sidecar-host/Cargo.toml` 有 `[target.'cfg(windows)'.dependencies]`（sidecar-host 线已有 Windows 分支）；`agent24d` 的 `host_bootstrap` 有 `cfg(not(unix))` 桩。
- 设计文档：`DOC/design/WINDOWS-PORT.md`（v2 冻结，DEP-A7）：模块传输选命名管道（DACL + pid→SID），wire 帧不变；Windows 没有 fd 3、没有跳板；A3 附着 socket 与鉴权推迟；逐文件处置表 §6 列了 13 个生产文件 + 遗漏项。Windows 实现 DEP-C2/C3 状态 `BLOCKED`。
- agent24d 的主 HTTP 监听是 TCP 回环，本身不依赖 Unix；`state_file.rs` 的目录/文件权限设置是 `#[cfg(unix)]`，非 Unix 无 ACL。

### 5.2 oMLX 耦合、llama.cpp、Ollama
- 模型网关 `R/crates/agent24-models/src/{lib.rs,router.rs}`：`ModelProvider` trait + OpenAI 兼容适配；`router.rs::from_env` **默认链 oMLX（8088）→ Ollama（11434）**，环境变量 `OMLX_URL / OMLX_API_KEY / DEFAULT_MODEL`（`lib.rs:964-965`）。
- oMLX 特定耦合：`chat_template_kwargs:{enable_thinking:false}` 扩展只对名为 `omlx` 且判定为本地回环的 provider 发送（`lib.rs:242-293`）；Ollama 有单独的 `reasoning_effort` 分支（ME4-S2c）。
- **没有 llama.cpp 后端**（grep `llama.cpp|llama-server` 于 `R` 无命中）。Ollama 后端存在（OpenAI 兼容接口）。LM Studio 仅在注释里被提及为「可兼容」。
- 桌面端有 oMLX 专用管理 IPC（detect/start/warmup/stop，`ipc/index.ts:138-190`）；仓库根 `omlx.sh`；记忆里模型目录 `~/.omlx/models/`。
- oMLX 是 Apple Silicon MLX 运行时（ADR-022「LLM 运行时默认 MLX」）；ADR-021 写的「Ollama 运行时下载」是更早的假设。
- `ARCHITECTURE-LAYERS.md:134`：`ModelRouter::from_env` 今天只构造 oMLX 与 Ollama provider。

### 5.3 钥匙串 / 密钥存储
- 只有 `agent24-comm` 用：`keyring = 3.6, default-features=false`（`R/crates/agent24-comm/Cargo.toml:36-40`），注释写明 Windows 不在范围（COMM-HYPHAE §9 R2），所以没开 `windows-native` feature。服务名 `ai.idoris.agent24.comm`，account = `hex(sha256(keystore.salt))`（`COMM-HYPHAE.md` §2；`password_store.rs` 有 `KeyringPasswordStore` 与 `MemoryPasswordStore`）。
- Agent24 自己的 daemon token 不进钥匙串，是明文 `daemon.json`（0600）。
- Hyphae 自己的 keystore 是 `<state_dir>/comm/hyphae-home/.hyphae/keystore.json`（可加密）。

### 5.4 硬件探测（ADR-036）
- ADR-036 在 `DOC/decision.md:1466`（✅ 采纳，2026-10-15）：硬件探测与整机内存账归 Agent24（`agent24-hw`），模型选型归 iDoris；档位数字 1–6；云端补位默认关；设计文档 `DOC/design/HW-TIERS-capability-hardware-matrix.md`。
- 代码 `R/crates/agent24-hw/src/{probe,parse,disks,legacy_disk,budget,lib}.rs`，依赖 `sysinfo 0.39`。`probe.rs` 调外部命令 `sw_vers`、`sysctl machdep.cpu.brand_string`、`sysctl iogpu.wired_limit_mb`、`pmset -g batt`、`nvidia-smi`（均有 2s 超时，失败为 None）。`agent24d/src/hardware.rs` 启动时写 `~/.agent24/hardware.json`；`agent24 hw probe` 打印。
- 现状（`ARCHITECTURE-LAYERS.md:142`）：🟡，**runtimes 未探测、无 HTTP 路由（HW-3）、能力梯/解析器未做**；ADR-036 要求的 `ModelRouter` 云端开关强制（HW-4）也未做（见同文件 :134 `TaskProfile::default()`）。
- 探测的 macOS 专有路径较多（sw_vers/sysctl/pmset），Linux/Windows 分支大部分是 `None`（未逐行核实，标「未能确认」覆盖度）。

### 5.5 其他平台专有代码
- `agent24-cli/src/service.rs`：整个是 launchd；`#[cfg(unix)]` 一处（`:250`）。
- `agent24-cli` 依赖 `rustix` 的 `pty`/`termios`。
- `agent24-os-fd` 里 `target_vendor="apple"` 分支（见 5.1）。
- `agent24-sidecar-host`：`posix.rs` 与 `[target.'cfg(unix)']` / `cfg(windows)` 两套依赖，`target.rs` 15 处 cfg。
- 桌面端 `release.yml` 之外的 `agent24-documents`（pdfkit、`blob.rs` cfg）也有平台条件（未细读）。

---

## 6. 移动端和远程

### 6.1 Hyphae（agent24-comm）能提供什么
- 结构（`DOC/design/COMM-HYPHAE.md` §2，`R/crates/agent24-comm/src/*`）：agent24-comm 是**对 Hyphae Go 二进制的包装层**。`HyphaeRunner` 单次命令（带 sha256 锁文件校验的二进制副本、清洗环境、密码走 stdin）；`HyphaeDaemonSupervisor` 监管长驻 `hyphae daemon --notify=false`；专用 HOME。
- REST（`router.rs:160-175`，挂在 agent24d 的 `/api/v1/comm/*`）：`identity`（列/建/设默认）、`unlock`、`contact`、`relay`（含 probe）、`import`（从 ~/.hyphae 导入）、`daemon`（status/start/stop）、`send`、`history`、`inbox/pull`、`outbox`（列/retry/clear）。CLI 的 `agent24 comm …` 是同一套 REST 的客户端。
- 消息收发：`send` 发送、`history` 读历史（Hyphae 的 `messages.db`）、`inbox/pull` 拉取入站。目前没有「入站消息触发动作」的遥控路由；入站在 agent24d 内如何变成 run（F4 契约，`DOC/specs/F4-nostr-channel.md`）属另一条线（`packages/nostr-bridge`）。
- 加密：Nostr 协议，NIP-44（`F4-nostr-channel.md:26`，`README.md:240`），事件 kind 30078 + NIP-44 + zstd content 约定（`F4-nostr-channel.md:221`）；传输经 relay。密码保护 keystore（钥匙串存密码）。agent24-comm 的代码里没有直接做加密，加密发生在 Hyphae 二进制内。
- 平台：Hyphae 二进制按平台锁定（`current_platform()`），COMM-HYPHAE 声明 Windows 不在范围；移动端是否有 Hyphae 客户端：**未能确认**（仓库内没有移动端代码）。
- Hyphae 锁定版本 v0.26.3（HEAD 提交 `4a48e352 chore(comm): lock Hyphae v0.26.3`），`DOC/comm/*` 为联调与验收记录。

### 6.2 有没有「远程控制 daemon」的现有设计
- **没有成文设计**。`DOC/Deployment/TASKS.md` DEP-C4 `docs/design/REMOTE-ACCESS.md` 状态 `BACKLOG`，文件不存在（`ls docs/design | grep -i remote` 无）。
- 相关已有文字：
  - `DOC/Deployment/README.md:3,16,24,80` / `RESEARCH.md:97,136-139`：移动端只能做遥控端，MVP 倾向 PWA + tailscale，或 tailscale/自建 relay/Nostr relay 三选一待定；前提是 daemon 支持远程访问（配对、令牌、TLS）。
  - `DOC/decision.md` ADR-027：移动端瘦壳经 HTTP/WS 直连 daemon，遗留项：bind-address 配置、远程认证/配对、Origin/CSRF。
  - `DOC/Deployment/HYPHAE-NETWORK-DELIVERY.md` / `HYPHAE-HEALTH-ROLLBACK-CONTRACT.md`：Hyphae 网络交付与回滚契约，与「遥控」无关。
- 代码侧：没有任何远程监听、配对、TLS 的实现。

---

## 7. packages/node-daemon

- 是什么：`@agent24/node-daemon@0.5.1`，私有包，描述 "Agent24 backend daemon (Node) — M-A+: mock/reference implementation of the v1 protocol"（`packages/node-daemon/package.json`）。入口 `dist/server.js`；源码有 `server.ts`、`v1/`、`capability-registry.ts`、`llm-gateway.ts`、`module-*.ts`、`boxlite-*.ts`；依赖 `@boxlite-ai/boxlite`。固定端口 8765 的历史设计来自 ADR-020。
- 使用方：
  - `D/src/main/backend-manager.ts`：`AGENT24_BACKEND=node` 时 `resolveNodeEntry`（开发态 `require.resolve('@agent24/node-daemon')`，打包态 `resourcesPath/backend/server.js`）；
  - `D/package.json` 的 dev/build:* 脚本先 `pnpm --filter @agent24/node-daemon build`，且 `extraResources` 把 `dist` 打进**所有平台**安装包（即使默认 Rust）；`dev-prepare-backend.mjs` 在 node 模式跳过 cargo；
  - 根 `package.json` 的 dev/build/typecheck/test/lint/format 都包含它；
  - `.github/workflows/ci.yml:179-183`：contract-tests 在 CI 里对 node mock 跑一遍（`node packages/node-daemon/dist/server.js`），另有 `Contract (agent24d)` job 对 `agent24d serve --port 0` 跑（`ci.yml:205-260`）；`release-desktop.yml:146` 也先构建它；
  - `packages/contract-tests`（"run against any Agent24 daemon (node mock or agent24d) via A24_BASE_URL"）：通过 `A24_BASE_URL` 连任一 daemon，不直接 import node-daemon；
  - `apps/desktop/src/shared/ipc-types.ts:150,267` 注明与 `node-daemon/src/manifest.ts`、`module-discovery.ts` 是镜像副本。
- 状态：ADR-026 称其为 v1 协议参考实现；`backend-manager.ts` 头注释称「legacy mock，显式 opt-in」。

---

## 8. 相关文档清单与冲突点

方向基线（jason 2026-10-26）：agent24d 是本体；每用户一个常驻 daemon 供所有外壳共用；开发者 API 先只监听回环 + 分权 token；TUI 能力对齐；先 Mac 后 Win/Linux；移动端只做遥控经 Hyphae。

### 8.1 `DOC/` 顶层与 ADR
| 文档 | 一句话 | 与方向的冲突或需更新 |
|---|---|---|
| `DOC/ADR-026-rust-core-polyglot.md`（✅，2026-08-21） | Rust 核心 daemon + 多语言 worker；协议边界 HTTP/WS；含「TUI 是 operator surface」(#11) | 需补：daemon 常驻/单例为产品默认；TUI 要对齐能力，与 #11「不是第二个 runtime」不矛盾，但「只是运维面」的措辞要放宽 |
| `DOC/decision.md` ADR-020（✅） | Node http 后端，端口 8765，Electron `fork()` 管理 | **过时**：已被 ADR-026 取代，Rust 是默认；文件未标 Superseded（需核实，只看了标题和正文） |
| 同上 ADR-021 | 一站式零 CLI 安装，Ollama 运行时下载 | 部分过时（ADR-022 改 MLX）；「全程不触碰终端」与 TUI 能力对齐无冲突 |
| 同上 ADR-022 | 默认 MLX（oMLX），UI 可切 | **与跨平台冲突**：oMLX 仅 Apple Silicon；Win/Linux 需要另一个默认运行时（现仅 Ollama 兜底） |
| 同上 ADR-018 / ADR-027 | 移动端：Tauri → 改为 shell-agnostic 双壳（Tauri + Expo），瘦壳经 **HTTP/WS 直连 daemon** | **与「移动端只做遥控、走 Hyphae」冲突**：ADR-027 的直连与 bind-address 遗留项需被替换或标明范围 |
| 同上 ADR-029/031/032 | 内核↔领域 OS 边界；进程外模块协议（UDS + fd 3）；AgentEar/iDoris 接入 | 进程模型事实来源；Windows 路径见 WINDOWS-PORT |
| 同上 ADR-035 | iDoris Admin session token 经匿名继承 FD/HANDLE 一次性交付 | 与「每用户一个常驻 daemon + 多客户端」的 token 交付方式相关，需要对照 |
| 同上 ADR-036 | 硬件探测归 Agent24，选型归 iDoris | 与方向一致；实现 HW-3/HW-4 未完成 |
| `DOC/ARCHITECTURE-LAYERS.md` | 分层与 ✅/🟡/📐 状态；鉴权两种模式、sidecar 目标形态、遗留清单 | 最重要的现状基线；方向若引入「分权 token 给多种客户端」，需把 📐 路由级资源授权升格；「Desktop 与 acp 都未使用 capability 模式」要更新 |
| `DOC/ROADMAP.md`、`DOC/PLAN.md`、`DOC/WORKSTATION_PLAN.md` | 里程碑与计划 | 只在 grep 命中 ADR 引用，**未读全**，标「未能确认」 |

### 8.2 `DOC/Deployment/`
| 文档 | 一句话 | 冲突或需更新 |
|---|---|---|
| `README.md` | 多平台发布三阶段：A 发 CLI+Linux 桌面 → B 签名后发 mac dmg → C Windows、远程访问、移动端 PWA | 「移动端 PWA + tailscale」与新方向「经 Hyphae 遥控」不同；「v0.5.1 不发 dmg」与 10-05 裁决不符，需同步 |
| `TASKS.md` | 任务表：A5 DEP 复用外部 daemon（DONE）、A7 Windows 设计（DONE）、B* 签名（BLOCKED）、C1–C9、HN0–HN6 | DEP-C4 `REMOTE-ACCESS.md` 仍 BACKLOG，和新方向（Hyphae 遥控）对不上；B* 的 BLOCKED 状态需与 10-05 裁决对齐 |
| `RESEARCH.md` | 各平台可行性调研（含 iOS/Android 只能遥控） | 移动端传输选型待更新 |
| `HYPHAE-NETWORK-DELIVERY.md`、`HYPHAE-HEALTH-ROLLBACK-CONTRACT.md` | Hyphae 二进制交付、健康与回滚契约 | 与方向一致，可作为「Hyphae 连接」的运维基线 |

### 8.3 设计与规格
| 文档 | 一句话 | 冲突或需更新 |
|---|---|---|
| `DOC/design/WINDOWS-PORT.md`（v2 冻结） | Windows 移植：命名管道传输、逐文件处置表 | 与「先 Mac 后 Win」一致；设计 Windows 上 daemon 常驻（服务/任务）没有覆盖，需补 |
| `DOC/design/COMM-HYPHAE.md`（r2 冻结） | Hyphae 接入：comm 模块、REST、专用 HOME、keyring | 声明 Windows 不在范围；是遥控的底座 |
| `DOC/design/A3-ATTACHED-MODULE.md`（v2 冻结） | 附着式进程模块（AgentEar），常驻 attach socket | 「附着模块」与「每用户常驻 daemon」互补；socket 鉴权是 Unix 向 |
| `DOC/design/HW-TIERS-capability-hardware-matrix.md` | 能力×硬件分档 | 一致 |
| `DOC/design/HYPHAE-CLI-INTEGRATION.md`、`INTEGRATION-AGENTEAR-IDORIS.md`、`SHUT-shutdown-observability.md`、`ME4-S*` | Hyphae 提案、AgentEar/iDoris 集成、关机可观测、OS SDK | 背景；SHUT 与 daemon 生命周期强相关 |
| `DOC/specs/SPEC-001-engineering.md`、`SPEC-002-protocol.md` | 工程规范（§9 回环）与协议（ready 行、WS Origin 拒绝、token 每次启动重新生成） | 「token 每次启动重新生成」「stdout ready 行」假定宿主是父进程；新方向多客户端共用常驻 daemon 时需要补充发现/授权路径 |
| `DOC/specs/SPEC-ME3-OUT-OF-PROCESS.md`、`WIRE-OOP-MODULE.md` | 进程外模块 | Windows 小节待 W6 |
| `DOC/specs/F4-nostr-channel.md` | Nostr 通信渠道契约（NIP-44、kind 30078） | 遥控消息格式的候选基础 |
| `DOC/open-design-workspace/design/ADR-001..006` | Open Design 集成：边界、workspace 契约、ACP 桥、桌面宿主、capability 隔离、遗留恢复 | ADR-004 规定「Electron main 是桌面宿主唯一 owner」（进程所有权），与「daemon 独立常驻、桌面是客户端」要对照；ADR-005 是 capability 模型来源 |
| `DOC/agent/PLAN-OD-NEXT.md` | Open Design 下一步，含 F11.0 backendProxy 隔离 | 与分权 token 直接相关 |
| `DOC/adr/ADR-K1-01..04` | K1 内核：模块工具契约、会话数据流与安全模式、enable 授权与信任吊销、审计与共享授权 | 授权模型相关；未细读，标「未能确认」是否涉及客户端分权 |

### 8.4 代码内的事实对应关系（便于 ADR 引用）
- 「每用户一个常驻 daemon」：已有单例锁（`state_file.rs:try_acquire_singleton`）+ 发现文件（`daemon.json`）+ LaunchAgent（`service.rs`）+ 桌面复用外部（`backend-manager.ts:tryReuseExternal`）；缺：桌面/TUI 不安装或启动常驻服务、非 mac 无服务化。
- 「分权 token」：已有 capability 存储（22 个 Operation、TTL、吊销、代际）；缺：路由级策略（`required_operation` 只认 models）、资源提取、非 ProductHost 令牌的发放路径（mint 路由只有 creative）、客户端发现与 token 交付（capability 模式下 daemon.json 无 token，且只能由 stdio 父进程拿到宿主令牌）。

---

## 未能确认的清单
1. 是否存在其他进程直接打开 `agent24.db`（CLI 为 REST 客户端，模块经回调，未发现，但未穷尽）。
2. `required_operation` 之外是否有个别业务路由内再做 capability 校验（grep 未发现）。
3. agent24-hw 在 Linux/Windows 上的探测覆盖度。
4. 移动端是否已有任何 Hyphae 客户端实现（仓库内没有）。
5. `ROADMAP.md`/`PLAN.md`/`WORKSTATION_PLAN.md`、`ADR-K1-*` 的具体内容未读全。
6. ADR-020 是否在 decision.md 里已标 Superseded（只看了章节）。
