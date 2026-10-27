# 附录 A：架构与分层（Sonnet 4.6，只读，基线 ab/core 4f4f49ab）

> 原始评审稿，未经删改；主报告中的修正见 CODE-REVIEW-2026-10-27.md §2。

# Agent24 架构与分层评审（origin/ab/core @ 4f4f49ab）

评审范围：rust/apps/*、rust/crates/*、apps/desktop、packages/*、protocol/。只读，未构建，未改仓库。
对照文档：docs/decision.md（ADR-026/031/037）、docs/ARCHITECTURE-LAYERS.md、CORE-2、CORE-5、跟踪文档 ARCHITECTURE-2026-10-26.md。

## 总评

Rust 侧 crate 依赖方向是干净的：22 个 crate 加 4 个 app，没有环，没有下层依赖上层，store 没有反向依赖 decide（只在 dev-dependencies）。数据目录解析（Rust `agent24-protocol::data_dir` 与 TS `@agent24/data-dir` 共用 fixture）也是真正的单一来源。主要问题不在 crate 图，而在三处：

1. agent24d 的组合根过胖：`serve()` 单函数约 1270 行，server.rs 6517 行，已经逼出一批「读自己源码做断言」的结构性测试。
2. 桌面壳与 ADR-037「外壳不得自带能力」不一致：渲染进程仍有一批只存在于 node-daemon 的页面和 IPC，oMLX 管理逻辑在主进程，`backendProxy` 无白名单。
3. 三库（agent24.db、memory.db、documents.db）的失败语义和迁移保护不一致，扩展面（os-proto、sidecar-host、进程内 DomainModule）有大量暂无生产消费者的抽象。

## 依赖图（正常依赖，省略 `agent24-` 前缀，据各 Cargo.toml 实读）

```
L0  protocol(无) · sidecar-host-protocol(无) · os-fd(无) · os-cwd(无) · worker(无,无消费者) · comm(无) · hw(无)
    domain -> protocol
L1  core -> protocol
L2  store -> core, protocol            [dev: decide]
    workspace -> core, protocol, store  (os-cwd 为平台条件依赖，文档称有)
L3  models -> protocol
    memory -> core, models
    tools -> protocol, domain, workspace, store      (文档表里没有 domain、store)
    policy -> core, domain, models, protocol, store, tools   (文档表里没有 domain)
    scheduler -> core, protocol, store
    mcp -> protocol, tools
    decide -> hw
L4  agent -> core, memory, models, protocol, store, tools, workspace  [dev: policy]
L5½ os-proto -> domain, os-fd
    os-packages -> domain, os-proto
    os-sdk -> os-proto
L5  agent24d -> agent, comm, core, decide, hw, domain, os-packages, os-proto, mcp, memory, models,
                policy, scheduler, store, tools, workspace, protocol   [dev: os-sdk]
App agent24-cli -> hw, protocol, mcp, os-packages, store     (直接开 agent24.db，绕过 daemon)
    agent24-documents -> os-sdk   (独立二进制，自有 documents.db)
    agent24-sidecar-host -> sidecar-host-protocol  (无任何消费者)
TS  apps/desktop -> @agent24/node-daemon(workspace:*), data-dir, api-client
    packages: data-dir · api-client · contract-tests · documents-accept · nostr-bridge · wechat-bridge · node-daemon
```

规模（行数）：store 28.8k，os-proto 27.7k，memory 13.8k，comm 10.3k，agent 9.6k，sidecar-host 14.6k，agent24d 44.5k（其中 server.rs 6.5k、domain.rs 5.9k、os_memory.rs 2.9k、os_routes.rs 2.6k、model_callback.rs 2.5k）。

---

## 发现清单

### A-01 | High | 组合根过胖
- 位置：rust/apps/agent24d/src/server.rs:1403（`pub async fn serve(`）至 :2675（`fn validate_auth_startup`）；server.rs:3032 起测试模块；domain.rs（5907 行）。
- 问题：`serve()` 单函数约 1270 行，同时负责：打开 store 与 memory、硬件快照、工作区、令牌、单例锁、模块挂载、调度、审批恢复扫描、停机编排。server.rs 约 3000 行生产码加约 3500 行测试，另有 49 个顶层项。agent24d 共 44k 行，已是仓库里最大的单体。
- 证据：
  - `serve` 起止行 1403 到 2675（据 `grep -n "^pub async fn serve\|^fn validate_auth_startup"`）。
  - 测试里按字符串切 `serve()` 函数体：`let src = include_str!("server.rs"); let start = src.find("pub async fn serve(")... body.find("\n}\n")`（server.rs:3075、:3114、:5664），还有按 `shutdown.request();\n    let _ = stopping.await;` 这类文本定位调用顺序。
  - `production_sources()` 遍历 apps/agent24d/src 与 comm/src 读源码做「路由注册白名单」（server.rs:5094）。
- 影响：行为用测试读源码来钉，说明函数太大，单测够不到。任何重排、改名、rustfmt 变化都可能让这些测试误红或，更糟，变成空洞通过。编译单元大，改动冲突面大，新人无法定位启动时序。
- 建议：把 `serve` 拆成明确阶段的类型：`Bootstrap`（目录、锁、库）、`Mount`（模块）、`Background`（调度、扫描）、`Shutdown`（停机序列）。启动时序用可注入的阶段表或 trait 钉，不再读源码。结构性断言改为对阶段函数的单测。路由与 OpenAPI 对齐门（G-API-3）改为运行时枚举 `Router` 的路由表，而不是读源码。
- 工作量：L
- 置信度：CONFIRMED

### A-02 | High | 文档与代码不一致 / 桌面残留 node-daemon
- 位置：apps/desktop/src/main/ipc/index.ts:112-180（`/api/modules`、`/api/modules/discover`、`/api/modules/install`、`/api/modules/uninstall`）；apps/desktop/src/renderer/App.tsx:13-33、:337；renderer/pages/CodeSandbox.tsx:59/81、ServiceBoxDemo.tsx:18/35、modules/HelloModule.tsx:24、ModulesManager.tsx；apps/desktop/package.json:15、:39；根 package.json:12-13。
- 问题：桌面默认后端已是 Rust（`AGENT24_BACKEND` 非 node 即 rust），但渲染层和主进程 IPC 仍有一整套打到 node-daemon v1 mock 路由的功能：`/api/modules*`、`/api/codebox/*`、`/api/service-box/*`、`/api/svc/*`。Rust 路由表里没有这些路径。
- 证据：
  - `grep -rn '"/api/modules' rust --include='*.rs'` 无命中；Rust 路由全部在 `/api/v1/*`（server.rs:1144-1310 的路由表）。
  - `ModulesList` IPC 调 `proxyToBackend({method:'GET', path:'/api/modules'})`（ipc/index.ts:113-115）。
  - renderer 里非 v1 的 `backendProxy` 路径：`/api/service-box/status`、`/api/codebox/status`、`/api/modules/hello/greet`（grep 结果）。
  - desktop `package.json` 仍把 `@agent24/node-daemon` 作为依赖，`dev:electron` 与根 `build`/`dev` 先构建 node-daemon。
  - ADR-037 决策 1：「`packages/node-daemon` 定位为契约测试用的参考实现，不再进入任何安装包」。
- 影响：默认产品路径上，模块管理页、CodeSandbox、ServiceBox 页点开是 404 或空列表，这是死功能。同时模块管理实际应走 `/api/v1/os`，现状是有两套模块概念（npm capability 模块与 domain OS）并存而没有收口。
- 建议：删除上述页面与 IPC（或明确改接 `/api/v1/os`），把 node-daemon 从 desktop 依赖和根构建链里摘出，只留在 contract-tests 的 CI 链里。
- 工作量：M
- 置信度：CONFIRMED（Rust 无这些路由）；「点开为 404」按 fallback 推断，PLAUSIBLE。

### A-03 | High | 外壳边界 / 授权
- 位置：apps/desktop/src/main/ipc/index.ts:`isBackendProxyRequest`、`proxyToBackend`、`IpcChannels.BackendProxy`；renderer 中 32 处 `backendProxy(` 调用。
- 问题：渲染进程经 `backendProxy` 可向 daemon 发任意 `/` 开头的路径，主进程用宿主持有的 daemon token 转发，没有路径白名单。校验只要求 method 在 GET/POST/PUT/PATCH/DELETE 且 path 以 `/` 起、不以 `//` 起。
- 证据：
  - `requestPath.startsWith('/') && !requestPath.startsWith('//')` 是唯一的路径检查。
  - ADR-037 自己承认：「桌面的 `backendProxy` 会替渲染进程把任意路径带」，并要求「主进程按路径白名单转发，或为页面签发只覆盖所需操作的短期令牌」。
  - daemon 侧授权 `authorize()` 一律用 `Resource::global()`（server.rs:1092），legacy 模式下 token 能调全部路由，包括审批决定、shutdown。
- 影响：渲染进程（含 Creative WebContentsView 里嵌入的第三方页面若能触达 preload）的任何 XSS 都等同于 daemon 全权。这是「daemon 是本体、外壳是客户端」目标下的第一道需要补的缝。
- 建议：短期在主进程加路径+方法白名单（由 OpenAPI 生成），把审批决定、shutdown、capability mint 等提升级路径排除在 backendProxy 之外，只经原生确认框的专用 IPC 触达。中期按 CORE-2 走登记握手，取得日常级令牌。
- 工作量：M
- 置信度：CONFIRMED

### A-04 | Medium | 外壳边界 / 业务逻辑在主进程
- 位置：apps/desktop/src/main/ipc/index.ts:`OMLX_PROBE_CANDIDATES`、`fetchOmlxModels`、`OmlxStart`/`OmlxStop`/`LlmStatus`；renderer App.tsx:176-188、Settings.tsx:50-154。
- 问题：oMLX 的探测、启动、预热、停止全在 Electron 主进程里：硬编码端口候选和 API key，用 `execFile('omlx', ...)` 起进程，用 `pkill -f 'omlx serve'` 杀所有同名进程；渲染层 Settings 与 App 里重复编排「detect→start→models→warmup」。
- 证据：`{ url: 'http://127.0.0.1:8088', apiKey: 'xiaobao8088' }`；`execFile('pkill', ['-f', 'omlx serve'], ...)`；`LlmStatus` 又用同一 key 探测一次。`xiaobao8088` 在桌面源码出现 9 次。ADR-037 决策 1 已把它列为「现存的例外按计划迁回 daemon 或 iDoris」。
- 影响：两份模型发现逻辑（daemon 的 ModelRouter 与桌面 IPC）会分叉；`pkill -f` 会误杀用户自己跑的 oMLX；固定 key 写进源码；Linux/Windows 无对应实现；TUI 做不了同样的事。
- 建议：迁到 daemon 的 `/api/v1/models` 与 iDoris 管理面（ADR-036），桌面只留「打开 oMLX 控制台」之类的壳操作。迁移前至少把 key 与端口收进配置，把 `pkill` 换成只停自己拉起的子进程。
- 工作量：M
- 置信度：CONFIRMED

### A-05 | Medium | 外壳边界 / 大块非展示逻辑
- 位置：apps/desktop/src/main/open-design-component.ts（1180 行）、creative-serve-web.ts（687）、documents-import.ts（258）、agentear-events.ts（204）+ shared/agentearSequencer.ts（281）、backend-manager.ts（620）。
- 问题：Electron 主进程累计约 9.8k 行（含测试），其中有明显的非展示逻辑：组件下载+校验+解包安装（tar、SHA、目录祖先安全检查）、Open Design 子进程监管、PDF/图片分块上传+幂等重试+哈希、AgentEar 事件去重与排序。这些在 TUI 与 CLI 里完全不可用。
- 证据：documents-import.ts 头注释「main hashes the file, starts an upload, sends it as raw chunks of at most 768 KiB and starts the import」，并自带 `RETRY` 错误码集合；agentear-events.ts 解 WS 帧并 `parseAgentEarFrame`。CreativeServeWeb 与 os-proto 的 supervisor、sidecar-host、桌面 SidecarManager 是同一问题的四套监管实现（见 A-09）。
- 影响：能力对齐目标（ADR-037 决策 4：TUI 全功能）无法达成；这些逻辑只能靠 Electron 测试，没有 Rust 侧覆盖；Windows/Linux 外壳要重做。
- 建议：Open Design 组件安装的校验与下载归 daemon（ADR 已有「可选组件按需下载」决定）；文档导入的分块协议归 `agent24-cli`/daemon 一个共享客户端；AgentEar 事件排序放进 daemon 的 WS 层。外壳只转发展示。按 ADR 例外逐项登记，不要继续长。
- 工作量：L
- 置信度：CONFIRMED

### A-06 | High | 数据边界 / 失败语义不一致
- 位置：rust/apps/agent24d/src/server.rs:488-503（`open_memory_base`）与 :1580-1592（`Store::open`）；rust/apps/agent24-documents/src/db.rs:18（`documents.db`）；docs/agent/CORE-5-MIGRATION-GUARD-DESIGN.md。
- 问题：三个 SQLite 库的失败语义不同。主库 `agent24.db` 打不开则 daemon 启动失败（`map_err(std::io::Error::other)?`）；`memory.db` 打不开只 `warn` 后继续，「sessions will not remember and no domain OS will be lent memory」；`documents.db` 由独立二进制自管。CORE-5 设计明确指出现状 V1「主库先迁移，memory 失败会降级继续」，目标改为双库整体回滚；但 CORE-5 文中没有出现 documents.db。
- 证据：
  ```
  Err(err) => { tracing::warn!("memory base unavailable ({err}); sessions will not remember ...
  ```
  `grep -n documents CORE-5-MIGRATION-GUARD-DESIGN.md` 无命中。documents 的迁移由模块进程在自己启动时跑（`sqlx migrate`），不在 daemon 的快照/回滚范围内。
- 影响：升级时 memory 迁移半途失败会让用户在「看起来正常、但无记忆」状态下继续写入，之后无法判断哪些会话没落记忆；documents.db 的不可回退迁移没有前置快照，破坏 ADR-037 的「所有会自动迁移的库都要过这道门」。
- 建议：CORE-5 范围明确写成「daemon 拥有的库 + 它启动的第一方模块库」，要么由 daemon 在拉起 documents 之前统一快照（模块声明 `data_dir` 下的库），要么要求模块在自己迁移前走 daemon 提供的快照回调。落地前，memory 打开失败至少不能在非 ephemeral 模式静默降级，改为 degraded 状态可见于 `/health` 与 `agent24 daemon status`。
- 工作量：M
- 置信度：CONFIRMED（memory 降级与主库不同）；documents 不在 CORE-5 范围内按文档未提及判断，PLAUSIBLE。

### A-07 | Medium | 数据边界 / 跨库事务缝（MD-2b）
- 位置：rust/crates/agent24-memory/src/lib.rs:384-402（`events()`、`artifacts()`）；event.rs:426-428；condenser.rs:98/233/267（三个 `TODO(MD-2b ...)`）；reconcile.rs；crates/agent24-agent/src/retain.rs:234、session_memory.rs:313-380。
- 问题：
  1. memory.db 内部：KV、事件日志、artifact CAS、assertion 共用一个连接池，但没有跨 store 的事务：`EventLog` 与 KV 的写「同一文件，不同事务」。
  2. 跨库：run/审批/审计在 agent24.db，会话记忆与 retain 在 memory.db。run 终态与记忆保存无共同事务，也没有补偿协议。
  3. artifact 有 `db_checksum` 与 `file_checksum` 双谱系，对账（reconcile）存在但本次扫描未见 daemon 内调用方。
- 证据：lib.rs:386-389「Same file, NOT the same transaction ... that cross-store transaction seam is MD-2b work」；`grep -rn "reconcile::" apps crates` 除自身文件外无命中。retain.rs:234 只保证「与它所写的那一条」原子（`agent24_memory::writer`），不涉及 run 状态。
- 影响：崩溃点落在「run 已 Completed、记忆未写」或「记忆已写、run 被孤儿清扫取消」之间时，会出现不可见的不一致；artifact 对账没有触发点，磁盘文件被外部改后 DB 与文件会长期发散。
- 建议：先把这两条缝写成 ADR 级不变量（允许哪个方向的丢失，并给出幂等重放键，例如 run_id 作为 retain 的幂等键），再决定是否需要 outbox。在 memory 内部先补 `KvStore::begin()` 暴露的事务句柄供 events/KV 共用。给 reconcile 一个启动期调用点或显式 CLI 命令。
- 工作量：M
- 置信度：CONFIRMED（缝存在）；「无调用方」为 PLAUSIBLE（只用 grep 判）。

### A-08 | Medium | 职责重叠 / store 成为杂物间
- 位置：rust/crates/agent24-store/src/module_schedules.rs（3596 行）、legacy_recovery.rs（2986）、module_consents.rs（1480）、module_audit_event.rs（1492）、decision_log.rs、terminal_core.rs、allocation_*.rs。
- 问题：L2「权威与持久化」crate 28k 行，已经装进模块调度、模块同意书、模块审计、决策日志、workspace 分配/租约/恢复等领域状态。它声称只做「SQLite 仓储 + 哈希链审计」。decision_log.rs 头注释专门说明为避免依赖 L3 的 decide 而复制了 plain DTO。
- 证据：`wc -l agent24-store/src/*.rs`（总 27769）；Cargo.toml 描述「sqlx SQLite repositories enforcing agent24-core transitions, plus a hash-chained audit log」；迁移 18 个 `.sql` 同在一个目录。
- 影响：每加一个领域就改同一 crate 与同一迁移序列，编译与评审面持续增长；store 成为所有 crate 的公共下游，一次小改触发全量重编；迁移序列合并冲突（ab/memory-p1 已到 0022，main 到 0018）。
- 建议：不拆也要分层：保持 `store` 提供 `Pool`+迁移+审计，把 workspace 分配、module_* 仓储分到 `store::workspace`、`store::module` 子模块或独立 crate（共享同一 `Pool` 与迁移注册）。优先把 `module_schedules` 与 `module_consents` 拆出。
- 工作量：M
- 置信度：CONFIRMED

### A-09 | Medium | 模块系统抽象过多 / 监管逻辑重复
- 位置：rust/crates/agent24-os-proto/src/（supervisor.rs 3124、supervise.rs 1523、launch.rs 1542、proxy.rs 5874、rpc.rs 3344、drain.rs 2171、attach_mux.rs 1688）；rust/apps/agent24-sidecar-host/src/（launch_order.rs 2243、generation_driver.rs 1918、launch.rs 901 等，总 14.6k）；apps/desktop/src/main/sidecar-manager.ts（353）、creative-serve-web.ts。
- 问题：同一件事「拉起子进程、握手、监管、排空、杀进程树」有至少四套实现：os-proto（领域 OS）、sidecar-host（桌面 sidecar 宿主）、桌面 SidecarManager（TS）、桌面 CreativeServeWeb（TS 直接 spawn）。其中 sidecar-host 与 SidecarManager 在产品路径上都没有被使用。
- 证据：ARCHITECTURE-LAYERS.md「agent24-sidecar-host（没有任何 app 依赖它，Desktop 也不调用）」；`grep -n "SidecarManager" main.ts` 无调用；跟踪文档同样标 🟡。os-proto 单 crate 27.7k 行，比 agent24d 以外任何 crate 都大，超过 agent 内核（9.6k）近三倍。launch.rs 57 处与 supervisor.rs 41 处直接涉及 spawn/进程组。
- 影响：同一类安全性质（进程树清理、fd 继承、令牌脱敏）要在多处各自证明与维护；14.6k 行没有消费者的代码是纯成本，且 Windows 路径已有专门 CI（sidecar-windows.yml）在持续运行。
- 建议：裁决二选一：要么让 Open Design 真正走 sidecar-host（并删除 CreativeServeWeb 的直接 spawn），要么把 sidecar-host 与 SidecarManager 冻结到 feature 或独立仓库，不再进主 workspace。长期把进程监管内核抽成一个被 os-proto 与 sidecar-host 共用的小 crate。
- 工作量：L
- 置信度：CONFIRMED

### A-10 | Medium | 抽象无生产消费者
- 位置：rust/apps/agent24d/src/domain.rs:733（`Build::InProcess`）；rust/crates/agent24-domain/src/lib.rs（`DomainModule`、`KernelCtx`）；rust/crates/agent24-worker；rust/crates/agent24-os-cwd（50 行）。
- 问题：进程内 `DomainModule` 挂载缝在生产里没有任何实现，只有测试里的 `FakeModule`、`RootRouteModule`、`OpenModule`。domain.rs 5907 行里相当一部分在服务于这条路径。`agent24-worker` 无消费者；`os-cwd` 是 50 行独立 crate。
- 证据：`grep -rn "impl DomainModule for"` 的命中全部位于 `#[cfg(test)]`（domain.rs:2148、:3345，server.rs:3954）；`Build::InProcess(` 的非测试出现仅 domain.rs:733、:1253；ARCHITECTURE-LAYERS.md 写明「进程内 DomainModule 🟡（当前无生产内置模块）」「worker 🟡（无消费者）」。
- 影响：每个新模块特性要同时兼顾「进程内」与「进程外」两种形态，实际只有一种在跑；`KernelCtx` 这类 trait 面是未被真实调用方压力测试的 API，早晚要改。
- 建议：把 `DomainModule`/InProcess 缝降级为仅测试用的 feature（或直接删，ADR-031 已把方向定为进程外）。`worker` 要么挂到某个里程碑，要么移出 workspace。`os-cwd` 合并进 `workspace`。
- 工作量：M
- 置信度：CONFIRMED

### A-11 | Medium | 测试组织 / 脆弱点
- 位置：rust/apps/agent24d/src/server.rs:3075/3114/5094/5664；rust/apps/agent24d/tests/*_blackbox.rs（CARGO_BIN_EXE）；rust/crates/agent24-comm/tests/{real_binary,import_real_binary,unlock_real_binary,daemon_*}.rs；.github/workflows/ci.yml:84、:131。
- 问题：
  1. 时间：全 rust 目录 344 处 `sleep`，`start_paused` 仅 3 处。
  2. 真实二进制：10 个测试文件依赖 `CARGO_BIN_EXE_*` 或外部二进制；comm 测试依赖 `HYPHAE_TEST_BIN`，未设置时「打印原因并提前 return」，显示为通过。`me4_node_module_blackbox.rs:56` 依赖本机 `node`。
  3. 源码扫描：见 A-01。
  4. CI 用 `cargo test --workspace` 并行执行，网络端口（46 处 `127.0.0.1:0`/`bind`）与时间抢占共存。
- 证据：git log 近期 flaky 修复：8f808c8e（comm_zero_run 在 macOS 偶发）、4eb88a5c/c621036a（disable 与 close 竞态）、d1a80a0b（冷却时长改用暂停时钟）、15d7d4e1（WAL 复制 exists→copy 竞态）、cfbf623c（时间戳随保留期过期）；sleep 集中在 daemon 白盒测试：session_memory/recall.rs 12、server.rs 11、comm/tests/daemon_supervise.rs 11。
- 影响：已见的 flaky 全属于「真实时钟/真实进程/真实文件竞态」三类；`HYPHAE_TEST_BIN` 缺失即空过，CI 里这类测试是否真跑无从得知。
- 建议：新增一个 `Clock` 抽象（或统一 `tokio::time::pause`）并禁止新测试直接 `sleep`；真实二进制测试在 CI 上要求变量必设，缺失即失败，仅本地可 skip；给黑盒测试统一的「等条件」助手（带超时的轮询），替换裸 `sleep`。
- 工作量：M
- 置信度：CONFIRMED（计数与 git log）；「CI 是否空过」PLAUSIBLE（未读 CI 环境变量配置）。

### A-12 | Medium | 数据边界 / CLI 绕过 daemon
- 位置：rust/apps/agent24-cli/Cargo.toml（依赖 `agent24-store`）；rust/apps/agent24-cli/src/offline_db.rs:24；main.rs:2701。
- 问题：CLI 直接 `Store::open(agent24.db)`（`decide export/delete`、consent 失效等离线入口），daemon 的单例锁并不能挡住 CLI 开库。
- 证据：offline_db.rs:24 `let path = data_dir.join("agent24.db");`；CORE-5 V3：「daemon 单例不能拦住 CLI 直接开库」；ARCHITECTURE-LAYERS 表里 CLI 依赖 store 并注明「D0-2：直接读写 agent24.db，不经过 daemon」。
- 影响：迁移快照、停靠态、未来的审计链一致性都要求「同一时刻只有一个写者」；CLI 直写破坏哈希链审计的单写者假设，且让 CLI 绑定了具体的 schema 版本。
- 建议：按 CORE-5 D1，离线入口在拿到单例锁且无停靠标记时才允许；其余全部改为走 daemon 请求。长期把 CLI 对 store 的依赖降为只依赖 `protocol` 的客户端。
- 工作量：M
- 置信度：CONFIRMED

### A-13 | Medium | 授权架构未接线
- 位置：rust/apps/agent24d/src/server.rs:1062-1119（`authorize`）、:1092（`Resource::global()`）；capabilities/operations.rs。
- 问题：capability 令牌库已实现（22 个 `Operation`，TTL，代际），但路由到 Operation 的映射只覆盖少数路由；校验处资源一律 `Resource::global()`，没有按 workspace/session/run 提取资源。默认仍是 `legacy_single_token`。
- 证据：server.rs:1089-1093 `store.validate_bearer(bearer, operation, &crate::capabilities::Resource::global(), ...)`；ADR-037 事实基线：「`server.rs:required_operation` 只区分 models、mint、revoke 三类」（本次未单独找到该函数名，但 `Operation::` 在 server.rs 的非测试引用只有 :848-851 的 creative 铸造路径）；ARCHITECTURE-LAYERS 把「路由级资源授权」标为 📐。
- 影响：开发者令牌、移动端受限作用域（ADR-037 决策 3、6）都依赖这条线；在它落地前，「分权」只在 Creative 白名单这一处有效，其余是形同虚设的基础设施。
- 建议：先做 G-API-1：生成一张 `(method, route template) -> (Operation, ResourceExtractor)` 表，由 OpenAPI 驱动，缺项即编译或启动期失败，同时服务于 A-01 的路由门。
- 工作量：L
- 置信度：PLAUSIBLE（结论来自 ADR 事实基线加我读到的 `authorize`，未穷举所有路由）

### A-14 | Low | 配置与发现 / 重复解析
- 位置：apps/desktop/src/main/backend-manager.ts:144-175（`daemonStatePath`、`readExternalDaemonState`）；packages/documents-accept/src/client.ts:15-20；packages/wechat-bridge/src/config.ts:42；packages/nostr-bridge/src/config.ts:114；rust/crates/agent24-protocol/src/state_file.rs。
- 问题：`daemon.json` 的路径解析已统一到 `@agent24/data-dir`（好），但 JSON 字段解析在 TS 里有至少 4 份独立实现，Rust 一份。字段变更（例如 ADR-037 要加协议版本与迁移水位，`token` 在服务模式下消失）要同步改 5 处。
- 证据：backend-manager 手写 `JSON.parse` 并逐字段 `typeof` 检查（port、token、pid、protocol_version）；其余各包各自拼 `joinLiteral(dataDir(), 'daemon.json')`。
- 影响：低；但与 CORE-2/CORE-4 要改 daemon.json 契约叠加，会成为漏改点。
- 建议：在 `@agent24/data-dir`（或新 `@agent24/daemon-discovery`）里提供 `readDaemonState()`，并加一个与 Rust `state_file` 共用的 fixture 测试，同数据目录的做法。
- 工作量：S
- 置信度：CONFIRMED

### A-15 | Low | 配置与发现 / 硬编码路径字面量
- 位置：rust/crates/agent24-domain/src/lib.rs:568；rust/apps/agent24-cli/src/main.rs:1325；apps/desktop/src/main/open-design-component.ts:8、:853。
- 问题：manifest 默认 `data_dir: ~/.agent24/os/{name}/` 作为字面量散在 domain、os-packages、cli 与大量测试字符串里；CLI 的 `~/.agent24/os.json` 与桌面的 `~/.agent24/components` 也写死。ADR-037 要求「manifest 里 `~/.agent24/os/<name>/` 一类的路径一律按 `<data_dir>/os/<name>/` 解析」。
- 证据：domain/src/lib.rs:568 `format!("~/.agent24/os/{name}/")`；cli/main.rs:1325 `"~/.agent24/os.json"`；统计到 30 余处该字面量出现在 `agent24d`、`os-packages`、`cli` 的源码与测试里。
- 影响：设置 `AGENT24_DATA_DIR` 或 Windows `%LOCALAPPDATA%` 时，这些字面量与实际目录不一致；多数是测试夹具，生产命中点为 domain/lib.rs:568 与 cli:1325。
- 建议：生产路径统一经 `data_dir()`；manifest 里的 `~/.agent24` 前缀在解析点集中替换，一处修。
- 工作量：S
- 置信度：CONFIRMED（出现位置）；对 Windows 的实际影响为 PLAUSIBLE

### A-16 | Low | 分层 / L0 契约 crate 带运行时 fs 逻辑
- 位置：rust/crates/agent24-protocol/src/{data_dir.rs,state_file.rs}。
- 问题：被定位为「v1 wire 类型 L0」的 `agent24-protocol` 同时含数据目录解析、`daemon.json` 读写、单例锁（`try_acquire_singleton`）、`remove_if_owner`。契约 crate 因此依赖 fs 与锁的平台行为，并被 L1/L2 全体依赖。
- 证据：state_file.rs:138-241 的 `state_dir`、`write`、`try_acquire_singleton`、`read_live`、`remove_if_owner`。
- 影响：低；但 Windows 移植（命名管道、ACL）改动会落在 L0，使 wire 类型也被牵连重编。
- 建议：拆出 `agent24-runtime-dirs`（或并入 os-fd 一类的小 crate），protocol 只留 serde 类型。
- 工作量：S
- 置信度：CONFIRMED

### A-17 | Low | CLI 默认行为与 ADR 偏差
- 位置：rust/apps/agent24-cli/src/main.rs:5-6、:673-687（`spawn_daemon(ephemeral)`）、:824。
- 问题：模块头注释仍声明「Standalone: no daemon found → spawn an ephemeral agent24d」。ADR-037 决策 2 要求 TUI/CLI 发现不到 daemon 时提示并启动常驻实例，不再默认 `--ephemeral`。已有 `os_cli_no_ephemeral.rs` 测试，说明 `os` 子命令已改，其余命令是否已改未逐一核。
- 证据：main.rs:5 注释；:677-679 `if ephemeral { args.push("--ephemeral") }`；:687 `kill_on_drop(ephemeral)`；:824「Standalone (ephemeral) only when nothing is there to attach to」。
- 影响：用户在无 daemon 时用 CLI 聊天，数据落在内存库，退出即丢，与「daemon 是本体」冲突。
- 建议：统一在 `connect()` 一处处理，按子命令是否会写数据决定是提示还是拒绝。
- 工作量：S
- 置信度：PLAUSIBLE

### A-18 | Low | 文档与代码不一致
- 位置：docs/ARCHITECTURE-LAYERS.md §1.1 依赖表。
- 问题：依赖表称 tools 依赖「protocol, workspace」，policy 依赖「core, models, protocol, store, tools」，而 Cargo.toml 实际是 tools 另依赖 `domain`、`store`，policy 另依赖 `domain`。表头说「cargo metadata」生成，但已陈旧。
- 证据：agent24-tools/Cargo.toml 依赖列表含 agent24-domain、agent24-store；agent24-policy 含 agent24-domain。
- 影响：低；但「依赖图自检」本是这份文档的价值，陈旧后可信度下降。
- 建议：用脚本（`cargo metadata` + jq）生成该表并进 CI 对比，也可顺带加「下层不得依赖上层」的 allowlist 测试（comm 已有 `comm_dependency_allowlist.rs` 可借鉴）。
- 工作量：S
- 置信度：CONFIRMED

---

## 做得好的地方（不计入发现）

- Rust crate 依赖单向、无环；store 为避免依赖 decide 复制了 DTO，并写明原因。
- 数据目录解析单一来源：Rust `data_dir_from` 与 TS `@agent24/data-dir` 共用 fixture（`AGENT24_DATA_DIR` 优先，macOS/Linux 保持 `~/.agent24`，Windows 用 `%LOCALAPPDATA%`）。
- daemon.json 写入与删除持锁，避免新旧 daemon 的 TOCTOU（state_file.rs:`hold_lock`）。
- comm 有依赖白名单测试，可作为全仓模板。

## 建议的改造顺序（按收益/成本）

1. **A-02 + A-03 一起做（M，收益最高）**：删除桌面里只存在于 node-daemon 的页面与 IPC，摘掉 node-daemon 的构建依赖；同时给 `backendProxy` 加方法+路径白名单并排除提升级路径。前者清除死功能，后者是 ADR-037 里最便宜的安全收益。
2. **A-06（M）**：把 memory 打开失败改为可见的降级状态（或直接失败），并明确 documents.db 是否纳入 CORE-5 的快照范围。必须在任何不可回退迁移（0015-0017 及之后）发布之前做。
3. **A-01（L，但可增量）**：先抽出 `serve()` 的停机与启动阶段，把读源码的测试换成对阶段函数的测试；同时把路由表枚举做成运行时可查（顺带满足 G-API-1/G-API-3，也是 A-13 的前置）。
4. **A-11（M）**：引入统一的时钟/等待条件助手，真实二进制测试在 CI 设为必须；这直接减少已见的 flaky 类别。
5. **A-09 + A-10 的裁决（先决策，后动手）**：决定 sidecar-host 与进程内 DomainModule 的去留。这两项合计约 15k 行以上无生产消费者的代码，裁决本身几乎零成本，却决定后续是否继续投入维护。
