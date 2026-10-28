# 附录 B：代码结构与进程间通信（Sonnet 4.6，只读，基线 ab/core 4f4f49ab）

> 原始评审稿，未经删改；主报告中的修正见 CODE-REVIEW-2026-10-27.md §2。

# Agent24 评审：代码结构与进程间通信

基线：只读 worktree `/Users/jason/Dev/auraai/Agent24-review`，`origin/ab/core` @ `4f4f49ab`。范围：`rust/apps/*`、`rust/crates/*`、`apps/desktop`、`packages/*`、`protocol/`。只读审查，未构建、未运行，所有结论来自源码与 CI 配置阅读。我自己做了几个小脚本统计，口径写在对应条目里。

## 总评

进程外模块这一侧（os-proto JSON-RPC、sidecar-host NDJSON、attach socket）的工程质量高：帧长、并发、超时、取消、重启熔断、错误 `kind` 闭集都有明文常量和大量测试。生产路径的 `unwrap/expect/panic` 基本被 workspace lint（`unwrap_used = "deny"`）挡住，我的统计里生产段几乎为零。

问题集中在三处。一是壳与 daemon 之间的 HTTP/WS 与桌面 IPC：契约门只有路径级，缺方法级和 comm 路由；renderer 可以借主进程的全权 token 访问任意路径；REST 没有 Host/Origin 校验，WS 事件向所有连接广播。二是客户端重复：同一套 daemon 发现、鉴权、HTTP 调用在 Rust 与 TS 里至少手写了十几份，api-client 只有类型没有运行时。三是结构：`serve()` 单函数约 1270 行，36 个文件超过 1500 行（含测试），两套互不共享的"源码文本扫描"路由门。多数问题已被 ADR-037 / CORE-2 登记为后续片；本报告标出的是"设计已知、代码里仍然敞开"的部分，以及设计文档没覆盖到的缺口（C-02 的 comm 与方法级漂移、C-04、C-05、C-09、C-11）。

## 通信通道表

| # | 通道 | 协议 / 代码 | 鉴权 | 版本协商 | 超时 / 重试 / 背压 | 错误传播 | 契约测试 |
|---|---|---|---|---|---|---|---|
| 1 | daemon HTTP REST（壳/CLI/桥 ↔ agent24d） | HTTP/1.1 JSON，axum，`server.rs:1144`；路径 `/api/v1/*` | legacy：单 bearer，`constant_time_eq`（`server.rs:967`）；capabilities：路由表映射 Operation（`capabilities/route_policy.rs`），等级只是元数据不参与拒绝；无 Host/Origin 校验；仅 `GET /health` 免鉴权 | `GET /health` 带 `protocol_version`（`protocol/types.rs:30`）；仅桌面复用外部 daemon 时校验（`backend-manager.ts:323-337`）；TS 桥、documents-accept、MCP、ACP 不校验 | 服务端无统一请求超时；各客户端自定（见 C-01、C-04、C-09） | `{error:{code,message,hint?,details?}}`，code 为开放字符串（`types.rs:264`）；HTTP 状态由各 handler 手填 | 有：`packages/contract-tests`（同一套打 node 与 rust 两个后端，CI `contract` 与 `contract-rust`）；断言手写，没有按 openapi 响应 schema 自动校验 |
| 2 | daemon WS `/api/v1/events` | WS 文本帧，`{v,seq,ts,type,...}`，`events.rs:62` | 与 REST 同一 bearer；拒绝带 `Origin` 头的升级；不按主体或资源过滤（G-API-4 未做） | 帧内 `v:1`；无握手协商 | `broadcast` 容量 4096（`events.rs:25`）；慢客户端发送超时 10s 即断；`Lagged` 即断；无重放，`seq` 按连接重置；服务端不发 ping | 断开后客户端靠 REST 对账 | 有：`events.schema.json` 由 Rust 类型生成（CI 对比导出结果），fixtures 用 ajv 校验（`schema.test.ts`） |
| 3 | ready 行 / 启动引导（桌面 → daemon） | 子进程 stdout 一行 JSON（legacy，含 token）；capabilities 模式走私有管道 `--host-bootstrap-stdio`（Unix 专有，`host_bootstrap.rs`） | legacy token 经 stdout 与 `daemon.json`（0600，目录 0700，`state_file.rs:170-185`）；capabilities 令牌不落盘 | `daemon.json.protocol_version` | 桌面 `STARTUP` 等待靠 health 轮询；`tick` 带 `tickInFlight` 防重入；连续失败阈值后重启 | 退出码与 stderr 只进桌面日志 | 有：`capability_startup.rs`、`backend-manager.test.ts`、`protocol-compatibility.json` 共享 fixture |
| 4 | 本机登记握手 `enroll.sock` | UDS，一行 JSON 帧（设计） | peer_cred + 身份校验 + 在场证明（设计） | 独立协议版本（设计） | 帧 16 KiB、5s、人工配对 5min（设计） | 设计 | 无：CORE-2b/2c 尚未实现，仅有设计文档 `CORE-2-ENROLL-DESIGN.md` |
| 5 | attach socket（A3 附着模块，如 AgentEar） | UDS 0600 + 目录 0700，`attach_listener.rs`；`os-proto/attach_mux.rs`、`attach.rs` | 注册 token + `peer_cred` 同 uid（`attach_listener.rs:171-176`） | `initialize` 握手含版本与能力 | 握手 deadline；MUX 多路；反向命令走同一连接 | `HandshakeError` 闭集（含 `ShuttingDown`） | 有：`a3_2b_attach_blackbox.rs`（1660 行）、`a3_3_host_commands_blackbox.rs` |
| 6 | OOP 模块出站回调（模块 → 内核） | JSON-RPC 2.0 over UDS，NDJSON，`os-proto/rpc.rs`、`frame.rs` | 每次启动新 token，握手校验 | `os-proto/version.rs`；`initialize` 协商能力 | 帧 `MAX_FRAME_BYTES`；每连接在途 64；调用 30s（模型 `MAX_METHOD_CALL_TIMEOUT` 单独）；写 10s；params 节点/深度预算 | 协议错误用 JSON-RPC 码；应用错误固定 `-32000` + 闭集 `error.data.kind`（`rpc.rs:111-140`） | 有：`rpc.rs` 内约 80 个测试、`me4_*_blackbox.rs`、`me3f_blackbox.rs` |
| 7 | OOP 模块入站（内核反向代理 `/api/v1/<ns>/*`） | HTTP 反代，`os-proto/proxy.rs` | 内核剥离 `X-A24-*`（按前缀）与 `Authorization/Cookie`（手写名单，`server.rs:940-950` 有风险说明） | 无 | 总 30s、首包 10s（`proxy.rs:116-124`）；每模块在途 64；不支持流式 | 模块 5xx 与 `module_panicked/killed` 等码 | 有：`proxy.rs` 内测试、`daemon_modules.rs` |
| 8 | 模块进程监管 | `os-proto/supervise.rs`、`supervisor.rs`；进程组信号 | 同 UID，无沙箱（ADR-031 威胁模型已声明） | manifest 版本 | 启动 10s；退避 500ms 起倍增；熔断；稳定运行 60s 才清零；停止宽限 3s | 崩溃记入 `StopRecord`，`shutdown` 摘要落盘 | 有，覆盖面广 |
| 9 | sidecar-host ↔ helper | NDJSON，`agent24-sidecar-host-protocol`，`PROTOCOL_VERSION: u8 = 1`，控制帧 64 KiB、ready 帧 16 KiB | 同进程族、管道继承 | `version` 字段每条消息携带 | 自定义 worker + 超时 | 解码失败分 `InvalidJson/InvalidMessage` | 有；Windows 另有 `sidecar-windows.yml` |
| 10 | 桌面 IPC（renderer ↔ main） | Electron `contextBridge` + `ipcRenderer.invoke`，约 29 个 handler（`main/ipc/index.ts`），通道名在 `shared/ipc-types.ts` | 无 sender 校验；主窗口 `sandbox:false`（`main.ts:173`）；documents 按"操作"白名单，`backendProxy` 按"任意路径" | 无 | `backendProxy` 无超时无体积上限（见 C-01）；oMLX 探测 2s | 形状不一：代理失败返回 `{error:string}`，daemon 错误是 `{error:{code,message}}` | `ipc-types` 与 api-client 部分对齐（documents）；其余手写镜像（见 C-08）；有 `*.test.ts` 但无跨进程契约测试 |
| 11 | 桌面 main → daemon WS | `ws` 库，`agentear-events.ts`、`model-call-log.ts` | 带 bearer 头 | 无 | 指数退避重连，上限 30s；断线期间事件丢失（代码注释接受） | 静默重连 | 有单测 |
| 12 | Nostr / Hyphae（agent24-comm） | 每次调用起一个 Hyphae CLI 子进程；口令走 stdin；stdout/stderr 信封 `{ok,data}` / `{ok:false,error,message}`；`OUTPUT_CAP` | 二进制按内嵌 `hyphae.lock.json` 哈希校验；环境变量白名单 | lock 文件固定版本 | 单次子进程；`keystore_lock` 串行化写；守护 supervisor 带 `restart_policy` | `CommError` 闭集 | 有：`comm_blackbox.rs`、`comm_mount_smoke.rs`；`/api/v1/comm/*` 不在 OpenAPI 中（C-02） |
| 13 | nostr-bridge / wechat-bridge（TS） | `fetch` 打 daemon REST，轮询 `GET /runs/{id}` 等结果（间隔 1.5s，总 600s） | 读 `daemon.json` 的全权 token，或环境变量 `A24_BASE_URL/A24_TOKEN` | 不看 `protocol_version` | `fetch` 无超时 / AbortSignal | 抛异常 | 有单测与 `e2e.test.ts`；与 daemon 无契约测试 |
| 14 | MCP | 出：`agent24-mcp` 通过 `rmcp` 拉起外部 MCP server（stdio）并把工具并入 `ToolRegistry`（过审批门）；入：`agent24-mcp/server.rs` 把 daemon 的 3 个安全工具暴露为 stdio MCP server | 入站侧用 daemon bearer | rmcp 协商 | 入站侧 `reqwest::Client::new()` 无任何超时（C-04）；出站调用支持取消 | `McpError` | 有单测 |
| 15 | ACP | `agent24-cli/src/acp.rs`，stdio JSON-RPC 2.0 原型，把 ACP session/prompt 映射到 REST+WS | 复用 CLI 的 bearer | 无 initialize 协商（"prototype"） | 无 | 自带 `-32000` 等 | 仅文件内测试；ADR-031 明确缝 A 的 ACP 未立项 |
| 16 | CLI / TUI → daemon | `reqwest`，`agent24-cli/src/main.rs:573` 的 `client()`：`no_proxy` + 不跟随重定向 + 2s 连接超时（FU-74） | 读 `daemon.json` token；capabilities 模式直接报"host authority unavailable" | 无 | 连接超时 2s | 字符串错误 | CI `tui-coverage` 文本匹配 OpenAPI 操作（C-02 说明其近似性） |
| 17 | Documenting OS（`agent24-documents` 独立二进制） | HTTP，OpenAPI 里 `/documents/*` | 桌面 `httpSender` 带 token | 无 | 分块上传、重试按上传偏移 | `DocumentsErrorCode` 独立闭集（`openapi.yaml:3622`） | 有：7 个 `openapi-documents-*.test.ts`，覆盖面最好 |

---

## 发现清单

### C-01 [High] 桌面边界：`backendProxy` 让 renderer 借全权 token 访问 daemon 任意路径，且无超时、无大小上限

- 类别：进程边界 / 安全 / 桌面
- 位置：`apps/desktop/src/main/ipc/index.ts:74-117`（`proxyToBackend`）、`:119-134`（`isBackendProxyRequest`）、`:316-325`；`apps/desktop/src/main/preload.ts` 暴露 `backendProxy`
- 问题：校验只要求方法在 5 个值内、路径以 `/` 开头且不以 `//` 开头。主进程之后给请求带上 `Authorization: Bearer <daemon token>` 并转发。
- 证据：renderer 能发 `POST /api/v1/approvals/{id}`（自己批准自己触发的高风险操作）、`POST /api/v1/shutdown`、`PUT /api/v1/tool-overrides/*`、`POST /api/v1/capabilities/creative`。`http.request` 没有 `timeout` 选项，也没有 `outReq.setTimeout`，daemon 接受连接但不回应时 Promise 永不 resolve。响应体 `chunks.push` 没有上限。失败分支返回 `data: { error: string }`，与 daemon 的 `{error:{code,message}}` 形状不同。
- 影响：任何一个 renderer XSS（Markdown 渲染、模块 HTML、Creative 嵌入）直接等于 daemon 全权。ADR-037 G-API 和 CORE-2 V5 已标注，但 CORE-2 的实现片（2d/2f）在 capability 模式下才生效，当前桌面默认走 legacy，这个口子在产品默认路径上是敞开的。
- 建议：短期在主进程做"方法+路径模板"白名单（可直接复用 `route_policy.rs` 的 POLICIES 表生成一份 TS 常量），把审批决定、shutdown、tool-overrides、capabilities、attached 写操作从 renderer 路径中剔除，改为专用 IPC（弹原生确认框）。同时加请求超时（建议 30s，与 daemon 反代对齐）、响应体上限、统一错误形状。长期随 CORE-2 改成短期窄令牌。
- 工作量：M
- 置信度：CONFIRMED

### C-02 [High] 契约漂移：OpenAPI↔路由门只比路径，不比方法、不含 comm，且 openapi 是手写的

- 类别：契约一致性 / CI 缺口
- 位置：`rust/apps/agent24d/src/server.rs:3293-3328`（豁免常量）、`:3330-3440`（`route_openapi_mismatches`）；`protocol/openapi.yaml`；`scripts/tui-coverage.mjs`；`.github/workflows/ci.yml:158-173`
- 问题：
  1. 门的路由来源是对 `build_router_with_modules` 源码文本做 `.route(` 字符串扫描，只取路径，丢掉方法。
  2. `/api/v1/comm/*`（`agent24-comm/src/router.rs:160-169` 的 10 条路由）被 `EXTRA_RESERVED_NOT_IN_BUILD_ROUTER` 标成"已知盲区"，OpenAPI 里完全没有。
  3. `/capabilities/creative`、`/capabilities/{id}/revoke`、`/events` 在豁免表里。
  4. `MODULE_SURFACE_PATHS` 里的 `/sin90/*`（7 条）和 `/documents/*`（14 条）只验证"OpenAPI 有这些路径"，不验证对应模块真的提供。
  5. 请求/响应 schema 没有静态门：`openapi.yaml` 是手写的（`export-schema.rs` 注释写明"openapi generation 切到 utoipa 之前"），与 `agent24-protocol` 的 Rust 类型之间只靠契约测试里手写断言。
- 证据：我把路由模板的方法集合与 OpenAPI 对比，得到一个确认的方法级漂移：`get(crate::approvals::get_approval)` 在 `server.rs:1216` 注册了 `GET /api/v1/approvals/{id}`，OpenAPI 的 `/approvals/{id}`（`openapi.yaml:813`）只有 `post`。路径门因为路径存在而通过。`protocol/openapi.yaml` 共 61 个路径、75 个操作，其中 comm 为 0。
- 影响：ADR-037 G-API-3 要求"OpenAPI 覆盖全部对外路由，CI 检查代码路由与 OpenAPI 一致"。现状满足不了这句话，而 openapi.d.ts 又从同一份 yaml 生成，所以漂移会一路传到 TS 类型。`tui-coverage` 以 OpenAPI 为分母，comm 和漂移路由同样不在分母里，TUI "能力对齐"的统计偏乐观。
- 建议：
  1. 复用 `registered_routes_match_the_operation_catalog`（`server.rs:5301`）已经在用的 `parsed_routes`（带方法，且把 comm 也解析了）来驱动 OpenAPI 门，路径+方法双向比较。
  2. 补写 `/comm/*`、`GET /approvals/{id}` 的 OpenAPI。
  3. 响应 schema 方面，先挑 `Run/Approval/Schedule/ErrorBody` 做 `schemars` 导出对比，或在 `contract-rust` 里用 ajv 按 openapi 的响应 schema 校验每个已调用的响应。
- 工作量：M
- 置信度：CONFIRMED

### C-03 [High] REST 无 Host/Origin 校验；WS 事件对所有连接广播且不复验令牌

- 类别：通信安全 / 授权
- 位置：`rust/apps/agent24d/src/server.rs:951-1000`（auth 中间件）；`rust/apps/agent24d/src/events.rs:62-123`
- 问题：REST 中间件只检查 bearer，不看 `Host`（DNS rebinding）也不拒绝浏览器 `Origin`；只有 WS 升级拒绝 `Origin`。`client_loop` 对每个订阅者发送全部事件（含 `model.delta` 文本、审批内容、记忆召回），升级后不再复验令牌，吊销不会断开已建立连接。
- 证据：全仓 `grep` 没有任何 `header::HOST` 校验；`events.rs` 的 `EventsHub::broadcast` 对全部订阅者一视同仁；capability 模式里 `Resource::global()` 是唯一资源（`server.rs:1092`），`required_operation` 的资源提取没有做。
- 影响：legacy 模式下 token 是 32 字节随机值，DNS rebinding 需要先拿到 token，风险有限；真正的缺口是 capability 模式一旦成为默认、并对开发者令牌开放，低权限令牌可以读到全部会话的事件。ADR-037 已列为 G-API-2/4，但代码里没有占位的拒绝逻辑。
- 建议：先加 `Host` 白名单中间件（`127.0.0.1:{port}`、`localhost:{port}`、`[::1]:{port}`，约 20 行）和对浏览器 `Origin` 的统一拒绝（桌面固定来源例外）。WS 在 `EventBody` 上增加"所属 session/run"访问器，订阅时记下令牌主体，发送前过滤；`CapabilityStore` 已有 `watch.rs` 的吊销通知，可以直接驱动断连。
- 工作量：M
- 置信度：CONFIRMED（缺口）；PLAUSIBLE（实际利用难度）

### C-04 [Medium] MCP 入站服务的 HTTP 客户端没有套用 FU-74 加固，且没有任何超时

- 类别：通信可靠性 / 安全 / 重复
- 位置：`rust/crates/agent24-mcp/src/server.rs:57-65`（`reqwest::Client::new()`），调用点 `:125`、`:150`、`:174`
- 问题：CLI（`agent24-cli/src/main.rs:573`）、worker（`agent24-worker/src/lib.rs:197`）、models 都明确写了 `no_proxy()` + `redirect::Policy::none()` + 连接超时，并用 `#[expect(clippy::expect_used)]` 记录 FU-74 的理由："否则会跟随重定向或读环境代理，把 bearer 交给冒充者"。MCP 服务这一处用默认客户端：读 `HTTP_PROXY`、跟随重定向、没有任何超时。
- 影响：`agent24_run` 在 daemon 无响应时会永久挂起，外部 MCP 客户端只能自行超时；设置了 `HTTP_PROXY` 的机器上 loopback 请求带着 daemon bearer 发给代理（取决于 `NO_PROXY`）。
- 建议：抽一个 `agent24_protocol::client::loopback_client()`（或放在 `agent24-core`），把"FU-74 客户端"集中在一处，CLI/TUI/worker/mcp/models 统一使用，并为 `agent24_run` 单独设总时限。
- 工作量：S
- 置信度：CONFIRMED

### C-05 [Medium] 桌面主进程 IPC 接受 renderer 提供的 URL 并带凭据外发；oMLX 管理仍在外壳，且源码硬编码密钥

- 类别：进程边界 / 安全 / 与 ADR-037 决策 1 不一致
- 位置：`apps/desktop/src/main/ipc/index.ts:31-37`（`OMLX_PROBE_CANDIDATES`、`'xiaobao8088'`）、`:40-66`（`fetchOmlxModels`）、`:147-152`（`OmlxModels`）、`:156-162`（`OmlxStart`）、`:164-`（`OmlxWarmup`）、`:262`（`LlmStatus`）
- 问题：
  1. `OmlxModels`、`OmlxWarmup` 的 `url`、`apiKey` 完全来自 renderer，主进程对任意主机发请求并附 `Authorization: Bearer <key>`；没有"只允许 loopback"校验；`port: parsed.port || 80` 忽略 https。
  2. `OmlxStart` 用 `execFile('omlx', ['serve','--port',String(port),'--api-key',apiKey])`，密钥进入 argv，同机其他用户可通过 `ps` 看到；`port` 未校验为整数。
  3. 默认探测密钥 `xiaobao8088` 硬编码在源码，并在 `LlmStatus` 里对 `127.0.0.1:8088` 发送。
  4. ADR-037 决策 1 要求这类运行时管理迁回 daemon/iDoris，桌面不得自带能力实现。
- 建议：短期校验 `url` 必须解析为 loopback 且协议为 http(s)；`port` 校验整数范围；密钥改用环境变量传给子进程；去掉硬编码默认值，让用户配置或由 iDoris 提供。长期迁出。
- 工作量：S（加固）/ L（迁出）
- 置信度：CONFIRMED

### C-06 [Medium] 主窗口 `sandbox:false`，IPC 无 sender 校验，主窗口无导航守卫

- 类别：桌面 / preload 暴露面
- 位置：`apps/desktop/src/main/main.ts:167-174`；`apps/desktop/src/main/ipc/index.ts` 全部 `ipcMain.handle`；`apps/desktop/src/main/preload.ts`
- 问题：
  1. 注释写明禁用 sandbox 是因为 preload 用了 `require('../shared/ipc-types')`，"打包后再恢复"，目前仍是关闭状态。
  2. 所有 `ipcMain.handle` 都不检查 `event.senderFrame.url`，任何被加载进该窗口的 frame（包括 iframe）都能调用。
  3. 只有 Creative 的 `WebContentsView` 配了 `setWindowOpenHandler`/`will-navigate`（`main.ts:122-131`），主窗口没有。
  4. preload 暴露约 30 个方法，其中 `backendProxy`（C-01）、`omlx*`、`modulesInstall`（安装 npm 包）这类高权限操作与 `getAppVersion` 这类无害方法同等对待。
- 建议：preload 改成打包产物（esbuild/vite 单文件）后启用 `sandbox:true`；给 `registerIpcHandlers` 包一层 `guard(handler)` 校验 `senderFrame` 是主窗口的主 frame 且 URL 是应用自己的；主窗口加 `will-navigate` 与 `setWindowOpenHandler` 默认拒绝；高权限方法分组放在 `agent24.admin.*` 下，便于审计。
- 工作量：M
- 置信度：CONFIRMED

### C-07 [Medium] 错误模型：HTTP code 是开放字符串，三处清单互不一致，各层映射口径不同

- 类别：错误模型
- 位置：`rust/crates/agent24-protocol/src/types.rs:264-281`（`ErrorBody`）；`protocol/openapi.yaml:3586-3592`；`rust/crates/agent24-domain/src/http.rs:35`（`error_response(status, &str, &str)`）；`rust/crates/agent24-os-proto/src/rpc.rs:111-140`
- 问题：
  1. 实际出现的 code 我 grep 到至少 25 个（`invalid_request` 39 处、`internal` 39、`not_found` 33、`unauthorized` 14、`forbidden` 14、`calls_failed` 10、`module_unavailable` 6、`confirm_required` 4、`registry_invalid` 4、`write_conflict` 4、`permission_denied` 3、`host_authority_unavailable` 2、`payload_too_large` 2 等）。
  2. OpenAPI 的 `ErrorBody.code` 描述只列 14 个，缺 `forbidden`、`host_authority_unavailable`、`payload_too_large`、`module_unavailable`、`confirm_required`、`write_conflict` 等；Rust 注释里的清单又是另一份（有 `payload_too_large`，没有 `module_owned_schedule`）。
  3. 状态码与 code 的对应由 handler 手填，没有 `enum ApiErrorCode` 把两者绑定。
  4. 另一端：JSON-RPC 通道有闭集 `ErrorKind`，Documents OS 有闭集 `DocumentsErrorCode`，comm 有 `CommError` 闭集——说明团队已经知道怎么做，只是内核主 REST 没做。
  5. 桌面侧再增加一种形状（C-01 的 `{error:string}`），renderer 要同时处理三种。
- 建议：在 `agent24-protocol` 里定义 `ApiErrorCode`（`#[non_exhaustive]` 枚举 + `as_str()` + 默认状态码），`error_response` 改收枚举；OpenAPI 的 `code` 改为引用同一枚举导出的 `enum`；加一条测试扫描源码字面量 code 必须出现在枚举里。桌面代理统一返回 `{error:{code,message}}`。
- 工作量：M
- 置信度：CONFIRMED

### C-08 [Medium] 客户端侧重复：api-client 只有类型，HTTP 调用与 daemon 发现在多处手写

- 类别：代码结构 / 重复逻辑
- 位置：
  - daemon.json 发现与解析：`apps/desktop/src/main/backend-manager.ts:143-170`、`packages/nostr-bridge/src/config.ts:112-136`、`packages/wechat-bridge/src/config.ts:39-81`、`packages/documents-accept/src/client.ts:12-32`、Rust 侧 `agent24-protocol/src/state_file.rs`、`agent24-cli/src/main.rs`
  - TS 手写 daemon 客户端：`packages/nostr-bridge/src/agent24.ts`（86 行）与 `packages/wechat-bridge/src/agent24.ts`（109 行）近乎同一份；`apps/desktop/src/renderer/pages/agent/api.ts`（15 处 `backendProxy`）、`comm/api.ts`、`voice/api.ts`
  - 类型镜像：`renderer/pages/agent/api.ts:15` 手写 `Run`、`ScheduleSpec`；`shared/ipc-types.ts:148-152` 的 MIRROR NOTICE（"与 node-daemon 保持同步，A6 统一"）
  - Rust reqwest 构造点：CLI 4 处（`main.rs:573,3714`、`tui/mod.rs:48,487`）、worker 2 处、models、tools/net、mcp
  - bearer 拼接：`agent24-cli/src/acp.rs:267`、`tui/mod.rs:184` 等
- 问题：`packages/api-client` 的 `index.ts` 只导出 `paths/components/operations` 类型，没有运行时（fetch 封装、错误解码、协议版本检查、发现）。每个消费者各写一套，所以 C-04（加固漏一处）、bridges 不看 `protocol_version`（C-09）这类问题会反复出现。
- 建议：在 `packages/api-client` 增加一个很薄的运行时 `createClient({discover, fetch})`（`openapi-fetch` 可以直接吃 `paths` 类型），内含 daemon.json 发现、`protocol_version` 校验、超时、统一错误解码；两个桥与桌面 renderer 迁到它。Rust 侧在 `agent24-protocol` 或新 `agent24-client` crate 放同一个加固客户端与 `Endpoint` 发现（C-04）。
- 工作量：L
- 置信度：CONFIRMED

### C-09 [Medium] Nostr / WeChat 桥：外部来源输入经全权 daemon token 创建 run；不看协议版本与鉴权模式；无超时

- 类别：通信 / 鉴权 / 可靠性
- 位置：`packages/nostr-bridge/src/config.ts:123-136`、`packages/wechat-bridge/src/config.ts:68-82`、`packages/nostr-bridge/src/agent24.ts:28-60`、`packages/wechat-bridge/src/agent24.ts`
- 问题：
  1. 桥读取 `daemon.json` 里的全权 token，或环境变量 `A24_BASE_URL/A24_TOKEN`；后者对 base 没有 loopback 限制，token 会被发往任意主机。
  2. `daemon.json` 在 capability 模式下没有 token：桥会静默发出空 token 请求，一直收到 401；`documents-accept/client.ts:29-30` 对此有明确报错，桥没有。
  3. 不检查 `protocol_version`。
  4. 所有 `fetch` 没有 `AbortSignal`；`awaitRun` 以 1.5s 轮询最长 600s，不使用 WS。
  5. 入站微信/Nostr 消息驱动 run，攻击面来自网络，却持有 daemon 全权，而不是 ADR-037 决策 3 里的"开发者应用"级令牌。
- 建议：随 C-08 的共享客户端一起迁移；桥使用 `agent24 token create --scope run:create,run:read` 签发的窄令牌（CORE-2 之后），在此之前至少限制 `A24_BASE_URL` 为 loopback、加超时。
- 工作量：M
- 置信度：CONFIRMED（代码）；PLAUSIBLE（影响取决于入站过滤，COMM-HYPHAE §7 的 zero-run 约束在 comm 一侧，不覆盖这两个桥）

### C-10 [Medium] 结构：超大文件与超长函数，`serve()` 单函数约 1270 行

- 类别：代码结构
- 位置：
  - `rust/apps/agent24d/src/server.rs:1403-2672`（`pub async fn serve`，约 1270 行）；`rust/apps/agent24d/src/domain.rs:1007`（`mount_all`，约 660 行）、`:1667`（`mount_package`，约 294 行）
  - >1500 行的文件共 36 个（Rust 35 个，TS 手写 2 个 + 生成物 `openapi.d.ts` 5022 行）。含测试的前几名：`server.rs` 6517、`domain.rs` 5907、`os-proto/proxy.rs` 5874、`agent24-agent/src/lib.rs` 5683、`agent24-cli/src/main.rs` 3765、`store/module_schedules.rs` 3596。扣除内联测试后的生产代码：`server.rs` 约 3030、`domain.rs` 约 1940、`cli/main.rs` 约 2590、`proxy.rs` 约 1940、`agent24-agent/src/lib.rs` 全文件未拆出测试模块（5683 行，需要进一步确认测试占比）
- 问题：`serve()` 同时负责启动校验、单例锁、迁移、模块挂载、scheduler/pump 的 spawn、attach listener、关停编排、ready 输出。启动顺序依赖散落在 100 多处注释里（"必须在 X 之前"），一旦拆分错位就会破坏 ready 前置条件。`agent24-cli/src/main.rs` 的 3765 行把所有子命令、HTTP 客户端、TUI 入口放在一个文件。
- 建议：按启动阶段拆成 `StartupPhase` 结构（`acquire_lock → migrate → build_state → mount_modules → spawn_background → commit_ready → serve → drain`），每阶段返回显式句柄；`serve` 只剩编排。CLI 按子命令拆模块（`query.rs` 已经开了头）。
- 工作量：L
- 置信度：CONFIRMED

### C-11 [Medium] 后台任务没有 JoinHandle 管理，关停不等待 scheduler / pump / run 执行任务

- 类别：并发与生命周期
- 位置：`rust/apps/agent24d/src/server.rs:2341`（tick）、`:2354`（pump）、`:1372,1442,1470,1504`；`attach_listener.rs:143`；`rust/crates/agent24-agent/src/lib.rs:878-895,1158`（run 执行）；`rust/crates/agent24-os-proto` 内约 48 处裸 `tokio::spawn`
- 问题：统计口径是对 `tokio::spawn|task::spawn|.spawn(` 的粗略计数：agent24d 66 次，仅 2 处使用 `JoinSet`，0 处 `TaskTracker`；os-proto 135 次，4 处 `JoinSet`。scheduler tick 与 delivery pump 的句柄直接丢弃；关停时 `serve` 等待的是模块 supervisor（`stopping`）和 usage writer，不等待它们。run 执行任务的 panic 有监督（`agent24-agent/src/lib.rs:878` 的内外两层 spawn，做得好），但 daemon 关停时不取消也不等待在途 run，靠下次启动的孤儿清扫（`server.rs:1549,1593,1838` 附近）补终态。
- 影响：HTTP 窗口 1.5s 后强制退出（`lifecycle::HTTP_GRACE`），在途 run 在进程内被直接截断，用户在重启前看到 run 仍为 `running`，WS 也没有终止事件。`DeliveryPump` 的代码注释明确选择"abort 而非 await，行留 pending"，这一处是有意设计，但 scheduler tick 与 run 没有同等的说明。`server.rs:129` 的注释还写着"run-level cancel joins in C2"，疑似已过期。
- 建议：用一个 `TaskTracker`（tokio-util）持有 daemon 级后台任务，关停序列里 `close()` + 带 deadline 的 `wait()`；对在途 run 在关停开始时统一 `cancel()` 并等待落终态（最长到 `deadlines().modules`）。attach listener 的每连接任务放进同一个 tracker。
- 工作量：M
- 置信度：PLAUSIBLE（需要运行验证在途 run 在关停时的实际表现）

### C-12 [Medium] 路由策略表与 OpenAPI 门是两套互相独立的源码文本扫描；表外路由的缺省值偏宽

- 类别：契约一致性 / 结构
- 位置：`rust/apps/agent24d/src/server.rs:3330-3380`（OpenAPI 门的扫描）、`:4717-4800`（`route_interior`、`parsed_routes`）、`:5301-5330`（`registered_routes_match_the_operation_catalog`）；`capabilities/route_policy.rs:176-206`（`decide`）
- 问题：
  1. 两套扫描器用不同的实现解析同一段源码（C-02）。如果有人用 helper 函数或循环注册路由，两者都会漏，且 axum 路由不可内省。
  2. `decide()` 对"已匹配模板但不在 POLICIES 表里"的请求返回 `module_decision()`（`ModuleRoute`、`Everyday` 级）。也就是说，新增内核路由而忘记加表项时，capability 模式的缺省是"按模块路由放行"而不是拒绝。`registered_routes_match_the_operation_catalog` 是唯一的保险，但它依赖文本扫描。
  3. 等级（Everyday/Elevated）目前只是元数据，不参与拒绝（`server.rs` 注释"Levels are metadata: they are not a deny input (2b)"），这是设计的中间状态，需要 2b/2d 落地后才有意义。
- 建议：把路由定义收敛到单一数据表（`const ROUTES: &[RouteDef { method, path, handler, operation, level, openapi_op }]`），由它生成 axum Router、`POLICIES`、OpenAPI 门和 tui-coverage 的分母。至少把 `module_decision()` 的缺省改为：路径不在已挂载的模块命名空间内就拒绝。
- 工作量：M
- 置信度：CONFIRMED（两套扫描器、缺省值）；PLAUSIBLE（表项遗漏的实际概率，现有测试覆盖了当前路由）

### C-13 [Medium] 事件流设计：renderer 全部轮询，WS 无重放、无恢复点、无服务端心跳

- 类别：通信 / 背压
- 位置：`rust/apps/agent24d/src/events.rs:25,84-123`；`apps/desktop/src/renderer/pages/Approvals.tsx:68`（2s）、`Runs.tsx:43`（3s）、`Schedules.tsx:61`（5s）、`App.tsx:172`（5s）；`apps/desktop/src/main/agentear-events.ts`
- 问题：
  1. renderer 完全不订阅 WS，审批与 run 状态靠 2–5 秒轮询；WS 只被主进程用于 AgentEar 与 `model.call`。
  2. `Event.seq` 每连接从 0 开始，没有全局序号，也没有 `since` 参数，客户端断线后无法判断漏了什么（代码注释接受"v1 无重放"）。
  3. 服务端只在客户端关闭时退出，不主动 ping，半开连接依赖 10s 发送超时才发现，而空闲连接没有发送动作，不会触发超时。
  4. `Lagged` 直接断开，客户端必须全量 REST 对账，但没有"断线原因"帧。
- 影响：审批响应延迟最长 2s 且持续占用 daemon；TUI 和移动遥控（ADR-037 决策 6）需要可靠的审批通知，现在的通道难以支撑。
- 建议：事件加全局单调 `event_id`，WS 支持 `?since=`（环形缓冲保留最近 N 条，4096 容量已经有了）；服务端每 15–30s 发 ping；`Lagged` 前发一帧 `{type:"resync"}`。renderer 的 Approvals/Runs 改为订阅 + 低频兜底轮询。
- 工作量：M
- 置信度：CONFIRMED

### C-14 [Medium] 平台抽象缺位：Unix 域套接字、fd 继承、`peer_cred` 直接写进核心通道，Windows 只有"摸底"CI

- 类别：代码结构 / 可移植性
- 位置：`rust/crates/agent24-os-proto/Cargo.toml`（`rustix`、`command-fds`、`close_fds`）；`os-proto/src/*`（UDS）；`agent24d/src/attach_listener.rs:171`、`host_bootstrap.rs`；`.github/workflows/windows-check.yml`（"永不因编译错误失败"）
- 问题：ADR-037 决策 5 把 Windows 列为目标平台，设计文档 `WINDOWS-PORT.md` 要求命名管道替代。当前核心通道（#3、#4、#5、#6、#7）没有传输抽象层，`cfg(unix)` 只出现在边缘文件（`agent24-workspace`、`os-packages`、`lifecycle.rs`），说明核心 crate 要么整体不可在 Windows 编译，要么靠 feature 之外的隐式假设。CI 里没有任何一项会在 Windows 回归时变红。
- 建议：在 `os-proto` 引入 `trait Transport`（`accept` 带对端身份，`connect`），UDS 实现先行，命名管道实现随 Windows 片落地；CORE-2 的 `enroll.sock` 设计直接按这个 trait 写，避免再长一份 Unix 专有代码。把 `windows-check` 里已经能过的 crate 子集升级为必过项，防止倒退。
- 工作量：L
- 置信度：PLAUSIBLE（我没有在 Windows 上验证编译状态）

### C-15 [Low] attach listener：accept 失败无退避、未限制握手前并发连接、绑定失败静默降级

- 类别：并发与生命周期
- 位置：`rust/apps/agent24d/src/attach_listener.rs:134-157`、`:122-135`
- 问题：`accept()` 出错（如 `EMFILE`）只打 `warn` 然后立刻重试，形成忙循环刷日志。每个连接 `tokio::spawn` 一个任务，握手前没有并发上限；同 UID 的任意进程可以反复建连占满 fd。`bind` 失败只打 error 并结束任务，daemon 继续运行，AgentEar 等附着模块用户看不到原因（`/api/v1/os` 或 `/health` 里没有对应状态）。
- 建议：accept 错误按 `EMFILE/ENFILE` 退避 50–500ms；握手阶段用 `Semaphore`（如 32）限流；监听启动状态写进 `/health` 或 `/api/v1/attached` 的诊断字段。
- 工作量：S
- 置信度：CONFIRMED

### C-16 [Low] 版本协商分散：一个 u32 管 REST/WS，另有 3 套独立版本，部分客户端不检查

- 类别：版本协商
- 位置：`agent24-protocol/src/version.rs:11`（`PROTOCOL_VERSION: u32 = 1`）、`os-proto/src/version.rs`、`agent24-sidecar-host-protocol/src/lib.rs:20`（`u8`）、`apps/desktop/src/main/backend-manager.ts:337`
- 问题：
  1. REST+WS 共用一个整数，没有"向后兼容的次版本"概念，任何新增字段/路由要么不升（客户端无从得知能力）要么升了就全线不兼容。
  2. 桌面只在复用外部 daemon 时校验（`tryReuseExternal`）；自己 spawn 的 daemon 不校验，升级后新旧二进制混装（安装器替换二进制、旧桌面进程仍在）时无保护。
  3. TS 桥、MCP、ACP、documents-accept 均不校验。
  4. ADR-037 的"协议版本+迁移水位"里迁移水位部分 `/health` 尚未返回。
- 建议：`/health` 增加 `capabilities: [..]` 特性列表（客户端按特性探测而非比版本号），并在 C-08 的共享客户端里统一校验；spawn 路径也比对一次。
- 工作量：S
- 置信度：CONFIRMED

### C-17 [Low] `#[allow(...)]` 与 `dead_code` 的堆积集中在 sidecar-host 与 store 恢复代码

- 类别：代码结构
- 位置：`rust/apps/agent24-sidecar-host/src/lib.rs`（21 处 `dead_code`）、`rust/crates/agent24-store/src/legacy_recovery.rs`（17 处 `dead_code`）、`store/src/audit.rs`（9）、`apps/agent24d/src/authz.rs`（6）、`decide_routes.rs`（5）；`#[allow(clippy::too_many_arguments)]` 18 处
- 统计：全仓 `#[allow(...)]` 中，`unwrap_used/expect_used` 类共约 300 处，其中 `legacy_recovery.rs` 单文件 56 处，经抽查全部位于 `#[cfg(test)]` 之内，不是生产路径问题。`dead_code` 共 105 处（含测试），有 `reason` 的只有 2 处，如 `"exposed for a future A3-2b handshake verifier"`（`attach` 模块）。
- 建议：给所有 `dead_code` 要求 `reason =`（lint `allow_attributes_without_reason = "warn"`），每季度清一遍"未来某片会用"的占位。`too_many_arguments` 的 18 处多数是启动参数，改成参数结构体。
- 工作量：S
- 置信度：CONFIRMED

### C-18 [Low] 生产路径 `unwrap/expect/panic`：整体受控，残留点在 test-util 与 sidecar-host

- 类别：错误模型
- 统计口径：我写脚本逐文件统计 `rust/apps` 与 `rust/crates` 下非 `tests/` 目录、排除 `#[cfg(test)] mod` 之后的 `.unwrap()`、`.expect(`、`panic!(`、`unreachable!/todo!/unimplemented!`，排除行内注释。结果 `.unwrap()` 70、`.expect(` 103、`panic!` 27、其余 15，分布在 21 个文件。workspace 级 `unwrap_used = "deny"`、`expect_used = "deny"` 生效，所以这些命中基本都是被 `#[cfg(any(test, feature))]` 或 `cfg_attr` 排除的测试工具代码，脚本无法精确识别。
- 命中最多的文件：`apps/agent24-sidecar-host/src/launch.rs`（`expect` 63，如 `:288` 的 `reap()` 测试辅助）、`native_generation.rs`（30）、`first_launch_dispatch.rs`（19）、`os-proto/src/module.rs:955-959`（`SystemTime ... .unwrap()` 的临时目录辅助，位于 `fake_kernel` 测试夹具）、`os-sdk/src/module.rs` 与 `os-sdk/src/clients/*`（`test-util` feature 的 fake kernel）。
- 生产路径上有意保留的 `expect` 只见到两类：序列化固定形状结构体（`store/module_consents.rs:183-188`、`protocol/types.rs:790-795`，都带 `#[allow(..., reason=...)]`）；以及构造 loopback 加固客户端失败即 fail-closed（`cli/main.rs:573`、`tui/mod.rs:48-53`、`worker/lib.rs:197`），理由合理。
- 风险高点（需要人工确认是否确实只编进测试）：`os-sdk` 的 `test-util` feature 导出的 `fake_kernel` 等 `pub` 函数含 `expect`，若第三方模块在非测试构建里误开此 feature 会带入 panic 路径。
- 建议：给 `test-util` 的 `pub` 模块加 `#[doc(hidden)]` 并在 crate 文档里写明仅限 dev-dependencies。
- 工作量：S
- 置信度：PLAUSIBLE（脚本口径有限，未运行 clippy 验证）

### C-19 [Low] feature flag 用法克制，但 `test-util` / `test-lock-override` 属于"生产可见的测试开关"

- 类别：代码结构
- 位置：`os-proto/Cargo.toml:68-73`、`os-sdk/Cargo.toml:24-27`、`agent24-comm/Cargo.toml:82-87`
- 问题：全仓只有 3 个 feature，都是测试开关。`test-lock-override` 允许用本地构建的假 Hyphae 二进制替换内嵌 `hyphae.lock.json` 的期望哈希，这是对"二进制必须匹配锁文件"这条安全约束的旁路；注释说明在 `cfg(test)` 下自动开启，集成测试需要 `--features`。
- 影响：若发布构建（`release.yml`）误带 `--all-features`，旁路会进入产物。
- 建议：在 `release.yml` 与 `release-desktop.yml` 加一步 `cargo tree -e features -p agent24d | grep -E 'test-lock-override|test-util'` 必须为空；或把旁路改成运行时环境变量并要求 debug_assertions。
- 工作量：S
- 置信度：PLAUSIBLE

### C-20 [Low] Node daemon 作为参考实现仍与 Rust daemon 共用一套契约测试，但 `current.test.ts` 已是遗留面

- 类别：契约 / 结构
- 位置：`packages/contract-tests/src/current.test.ts`（147 行，断言 `/api/llm/usage` 等旧 `/api/*` 路径）、`packages/node-daemon`、`.github/workflows/ci.yml:176-213`（`contract` 作业起 node daemon）
- 问题：ADR-037 把 node-daemon 降为"契约测试参考实现"，桌面打包已排除（`backend-manager.ts:56-78` 的 `selectedBackend` 在 packaged 下忽略 node）。契约测试仍有一个作业（`contract`）只测 node，另一个 `contract-rust` 测 Rust。两个后端的契约面在 OpenAPI 扩张（os、documents、memory、comm）之后已经不一致，node 只实现旧面。继续维护它的成本会随着 C-02 的补全而上升。
- 建议：明确 node-daemon 的支持范围（只做 health/chat/runs 的最小子集），把 `openapi-os`、`documents`、`memory` 等测试标注为 rust-only，避免"node 也必须绿"拖住契约演进；或者按 ADR-037 的方向直接废弃。
- 工作量：S
- 置信度：PLAUSIBLE

### C-21 [Low] 桌面 renderer 状态管理分散：每页自建轮询与加载状态

- 类别：桌面 / 状态管理
- 位置：`renderer/App.tsx:172`、`pages/Runs.tsx:43`、`Approvals.tsx:68`、`Schedules.tsx:61,69`、`Settings.tsx:142`、`ServiceBoxDemo.tsx:23`；全仓未见共享的 store/query 层
- 问题：每个页面自己 `setInterval` + `useState` 拉 REST，缓存不共享，同一份数据（如 runs）在 Home、Runs、Chat 里各取各的；错误处理各写一遍，与 C-07 的错误形状不统一叠加。
- 建议：引入一个轻量 query 层（TanStack Query 或自写 `useResource(key, fetcher, {events})`），与 C-13 的 WS 订阅配合做失效；`backendProxy` 之上统一超时与错误解码。
- 工作量：M
- 置信度：CONFIRMED

### C-22 [Low] 反向代理不支持流式，已知缺口但会限制模块路由形态

- 类别：通信 / 性能
- 位置：`os-proto/src/proxy.rs:116-141`；ADR-032 "已知缺口"
- 问题：入站反代是整体缓冲，总期限 30s；推理回调也非流式（首字 4.86s vs 直连 2.80s）。OpenAPI 的 Documents OS 绕开了这点（分块上传），但任何模块想提供 SSE/WS 路由都做不到，而 ADR-031 把"HTTP 状态/头/流式语义"当作不用 MCP 的理由之一。
- 建议：先在 SPEC 里明确"流式路由 = 附着模式 + 独立端口"或给出时间表，避免模块作者自行绕过反代。
- 工作量：L
- 置信度：CONFIRMED（已在 ADR-032 记录）

---

## 严重度汇总

- High：3（C-01、C-02、C-03）
- Medium：11（C-04 至 C-14）
- Low：8（C-15 至 C-22）

## 建议改造顺序（Top 5）

1. **先收口桌面边界（C-01 + C-06 + C-05）。** 白名单代理、超时、统一错误形状；`senderFrame` 校验；oMLX 的 URL 与密钥处理。这三项成本低、直接降低最大的横向风险，不依赖 CORE-2。
2. **补齐契约门（C-02 + C-12）。** 先让 OpenAPI 门带上方法并包含 comm，补 `GET /approvals/{id}` 与 comm 的 OpenAPI；随后把路由收敛成单一数据表，驱动 Router、POLICIES、OpenAPI 门、tui-coverage。这决定了 G-API-3 能否真的成立，也是后面所有分权工作的分母。
3. **Host/Origin 与事件授权（C-03 + C-13）。** Host 白名单中间件很小，可以马上做；事件的主体过滤、`since` 重放和 ping 随 CORE-2 的令牌片一起做，并让 renderer 改用订阅。
4. **抽共享客户端（C-08 + C-04 + C-09 + C-16）。** TS 侧在 `packages/api-client` 加运行时，Rust 侧集中 FU-74 客户端；桥、MCP、CLI、renderer 统一迁移，顺带获得协议版本检查、超时与统一错误解码。
5. **统一错误码与后台任务生命周期（C-07 + C-11），再拆 `serve()`（C-10）。** `ApiErrorCode` 枚举让 OpenAPI、Rust、桌面三处同源；`TaskTracker` 管理后台任务并在关停时取消在途 run；这两项做完后再按启动阶段拆 `serve()`，风险最低。

Windows 传输抽象（C-14）应在 CORE-2b/2c 动工前定下 trait，避免再增加一份 Unix 专有代码。
