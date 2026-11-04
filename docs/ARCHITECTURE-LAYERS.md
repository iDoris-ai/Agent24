# Agent24 架构分层总览

索引：[架构总入口](architecture/README.md)・[ADR 索引](adr/README.md)

> 初稿写于 2026-10-03（main `122fff7`）。**2026-11-05 重新生成：对应 main `f818b0bf`**（架构优化方案第 0 期 S0-5，[方案 §4.1](tracking/2026-11-04/ARCH-OPTIMIZATION-PLAN.md)）。依赖表由 `scripts/gen-layer-deps.py` 从 `cargo metadata` 生成；其余文字按 R1 / R3 两份梳理报告（`docs/architecture/review-2026-11-03/`，在分支 `docs/arch-review-r1-module-map`、`docs/arch-review-r3-adr-docs` 上）逐条订正，冲突处以 main 代码为准。上一版是 2026-11-01（`bb434b61`）。
> 本文是**导航性总览**：解释各层是什么、彼此什么关系、将来扩展会互相影响到哪里。
> 每一层的权威细节在文中链接的 ADR / spec / 设计文档里；两者冲突时，以那些文档和**代码**为准，并回来修正本文。

**状态标记**（全文统一）：

- ✅ **已接线**：在默认产品路径上运行；
- 🟡 **已实现、未接入产品路径**：代码和测试在 main 上，但 Desktop / CLI 的默认路径没有启用；
- 📐 **仅设计**：只有 ADR / 设计文档，没有实现。

---

## 1. 一张图看全貌

```
┌──────────────────────── L7 外壳（入口）────────────────────────────┐
│ apps/desktop（Electron main）                                       │
│   BackendManager ✅ 拉起 daemon：Unix 默认 capability 模式，Windows 用 legacy │
│   CreativeServeWeb ✅ 选启动方式 + Creative WebContentsView            │
│   SidecarManager ✅ 托管 Open Design（POSIX 独立进程组；C8）           │
│ agent24-cli（含 `agent24 acp` ✅ ACP→REST/WS 映射桥）                  │
│ packages/：api-client（生成客户端）· contract-tests · nostr/wechat 桥 │
│            · node-daemon（开发用旧 API mock，只覆盖 v1 的 4/124 个 op） │
└──────────────┬───────────────────────────────────────────────────────┘
               │ REST / WS + bearer（capability 令牌；legacy 单 token 仍可用）
┌──────────────▼──────────── L6 扩展面（四类"外挂"）────────────────────┐
│ ① 进程内 DomainModule 🟡（挂载缝已实现；当前无生产内置模块）           │
│ ② 进程外领域 OS ✅（ME-3/ME-4/K1：Sin90 / Cos72 / Documents）          │
│ ③ 附着模块 ✅（A3：AgentEar，CORE-2 登记）                             │
│ ④ Sidecar 集成（Open Design）：现行 ✅ SidecarManager 托管 + ACP 桥；   │
│    目标形态 🟡 + agent24-sidecar-host + CreativeRuntime                │
│ （另：MCP server 的工具 ✅ → 包装成普通 Tool 走同一条审批流水线）       │
└──────────────┬───────────────────────────────────────────────────────┘
┌──────────────▼──────── L5 组合根：agent24d ────────────────────────────┐
│ v1 API、领域 OS 挂载与监管、附着注册表、审批/调度/推理/记忆回调        │
│ capability 令牌 ✅（Desktop 默认；一方外壳 enroll/elevate；默认拒绝）   │
│   路由级资源授权 📐 未实现；CLI/--auth-mode 缺省仍是 legacy 单 token    │
│ 组件托管：iDoris 托管、Hyphae 按需安装、Knowledge 宿主接口、模块工具 K1 │
│ 统一组件状态 /api/v1/components（ADR-038 C3）                          │
└──────────────┬───────────────────────────────────────────────────────┘
┌──────────────▼──────── L4½ 领域 OS 运行时（L5 与 L6 共用）──────────┐
│ os-proto（启动/握手/监管/受约束代理/分帧）· os-fd（继承 fd 接管）      │
│ os-packages（包发现与原子安装）· os-sdk（给模块作者的 SDK）            │
└──────────────┬───────────────────────────────────────────────────────┘
┌──────────────▼──────── L4 运行时内核：agent24-agent ───────────────────┐
│ run 生命周期 + agent loop                                               │
└──────────────┬───────────────────────────────────────────────────────┘
┌──────────────▼──────── L3 内核能力服务 ────────────────────────────────┐
│ models（网关 + TaskProfile 路由） tools（工具流水线）                   │
│ policy（fail-closed 审批 + 可选 Guardian） memory（M-D）                │
│ scheduler · mcp · comm（Hyphae，例外：自带 REST 面） · hw · decide 🟡    │
│ components（通用组件机制）· idoris（建在 components 之上）             │
│ knowledge · worker 🟡（ML Worker 契约，无消费者）· documents-client      │
└──────────────┬───────────────────────────────────────────────────────┘
┌──────────────▼──────── L2 权威与持久化 ────────────────────────────────┐
│ store：SQLite + 哈希链审计 + workspace 注册表/租约/分配/保留/恢复       │
│ workspace：WorkspaceService / WorkspaceHandle（描述符钉住的根）          │
│   └ os-cwd：post-fork fchdir，把子进程 cwd 钉在 workspace 内             │
│ 平台原语：os-proc（进程身份，CORE-2）· os-sqlite（SQLite 在线备份）     │
└──────────────┬───────────────────────────────────────────────────────┘
┌──────────────▼──────── L1 纯领域核心：agent24-core ────────────────────┐
│ 零框架依赖的状态机（ADR-026）                                           │
└──────────────┬───────────────────────────────────────────────────────┘
┌──────────────▼──────── L0 契约层 ──────────────────────────────────────┐
│ agent24-protocol（v1 wire 类型）· agent24-domain（DomainModule /        │
│ KernelCtx / Capability / 模块工具声明）· sidecar-host-protocol · protocol/│
└──────────────────────────────────────────────────────────────────────┘
```

### 1.1 真实依赖图（`cargo metadata`，省略 `agent24-` 前缀）

下表由脚本生成，不手改。重新生成：

```
python3 scripts/gen-layer-deps.py --write   # 重写两个标记之间的内容
python3 scripts/gen-layer-deps.py --check   # 与 cargo metadata 不一致时退出码为 1
```

数据来源是 `cargo metadata --no-deps --format-version 1 --manifest-path rust/Cargo.toml`（2026-11-05，main `f818b0bf`）。"正常依赖"不含 dev-dependencies，"仅 dev 依赖"是只出现在 dev-dependencies 里的内部依赖。「层」一列是人工归层，写在脚本的 `LAYER` 表里，新增 crate 没归层脚本会报错。

<!-- BEGIN GENERATED: layer-deps (scripts/gen-layer-deps.py) -->
| crate | 层 | 正常依赖 | 仅 dev 依赖 |
|---|---|---|---|
| domain | L0 | protocol | — |
| protocol | L0 | （无） | knowledge |
| sidecar-host-protocol | L0 | （无） | — |
| core | L1 | protocol | — |
| os-cwd | L2 | （无） | — |
| store | L2 | core, protocol | decide |
| workspace | L2 | core, os-cwd, protocol, store | — |
| os-proc | L2（平台原语） | （无） | — |
| os-sqlite | L2（平台原语） | （无） | — |
| components | L3 | （无） | — |
| decide | L3 | hw | — |
| hw | L3 | （无） | — |
| idoris | L3 | components, hw | — |
| knowledge | L3 | （无） | — |
| mcp | L3 | domain, protocol, tools | — |
| memory | L3 | core, models, store | — |
| models | L3 | domain, protocol | — |
| policy | L3 | core, domain, models, protocol, store, tools | — |
| scheduler | L3 | core, protocol, store | — |
| tools | L3 | domain, protocol, store, workspace | — |
| worker | L3 | protocol | — |
| comm | L3（例外：自带 REST 面） | （无） | components |
| documents-client | L3（客户端库） | （无） | — |
| agent | L4 | core, domain, memory, models, protocol, scheduler, store, tools, workspace | policy |
| os-fd | L4½ | （无） | — |
| os-packages | L4½ | domain, os-proto | — |
| os-proto | L4½ | domain, os-fd | — |
| os-sdk | L4½ | os-proto | — |
| **agent24d** | L5 | agent, comm, components, core, decide, domain, hw, idoris, knowledge, mcp, memory, models, os-packages, os-proc, os-proto, os-sqlite, policy, protocol, scheduler, store, tools, workspace | os-sdk |
| documents | L6 | os-sdk | domain, os-proto |
| sidecar-host | L6 | sidecar-host-protocol | — |
| cli | L7 | documents-client, hw, mcp, os-packages, protocol, store | — |

共 32 个 crate（`cargo metadata` 的 workspace member 数）。
<!-- END GENERATED: layer-deps -->

读表时的几点说明（手写，依赖表变了要回来核对）：

- 依赖方向一律向下，正常依赖里没有环，也没有下层依赖上层（R1 §3.2 用 DFS 核过）。dev 依赖里有两条反向，只影响测试：`protocol` → `knowledge`、`store` → `decide`。
- `idoris` 依赖 `components`，不是反过来：`ComponentLock` 和通用下载器在 `components`，iDoris 的 `IdorisLock` 是它之上的视图（ADR-038 C1，#1539；原先 11-01 版写成「`components` 借用 `idoris`」，已不成立，`fetch_verified` 在全仓已不存在）。
- `agent24d` 是内核运行时的组合根，没有 crate 反向依赖它。`comm` 编译进 `agent24d`，且不依赖其它内部 crate。
- `worker` 依赖 `protocol`，但没有任何 crate 依赖它。`decide` 只被 `agent24d` 的 `GET /api/v1/decide/profile` 消费。
- `agent24-cli` 直接链接 `store`（`decide export`/`delete` 直接读写 `agent24.db`，不经过 daemon，见 ADR-034），也直接链接 `os-packages`、`mcp`、`hw`。
- `sidecar-host` 只依赖自己的协议 crate，没有任何 app 依赖它，Desktop 也不调用。
- `os-proc`、`os-sqlite` 是操作系统原语，不是领域 OS；`os-` 前缀有两种意思（见 §2 L4½ 的说明和 Q7 的结论：不改名，文档里写清）。
- `documents-client` 是 L6 组件 documents 的客户端库，只被 CLI 使用，暂放 L3。

---

## 2. 逐层说明

### L0 契约层

| crate / 目录 | 是什么 | 权威文档 |
|---|---|---|
| `protocol/openapi.yaml` | REST 契约，**手写的权威来源**，由它生成 TS 客户端（`packages/api-client`）；CI 只做 lint | [SPEC-002-protocol](specs/SPEC-002-protocol.md) |
| `protocol/events.schema.json` | 事件契约，**由 Rust `Event` 类型导出**（`agent24-protocol` 的 `export-schema`），CI 做零漂移比对，再生成 TS | 同上 |
| `protocol/module.schema.json` | 领域 OS manifest 契约，手写权威文件 | [SPEC-ME3-OUT-OF-PROCESS](specs/SPEC-ME3-OUT-OF-PROCESS.md) |
| `rust/crates/agent24-protocol` | v1 wire 类型，经 fixture 测试与 `protocol/` 互锁；workspace 的 `kind` 等枚举在 wire 上是**开放字符串**（前向兼容） | 同上 |
| `rust/crates/agent24-domain` | 内核↔领域 OS 契约：`DomainModule`、`KernelCtx`、`Capability`、`DomainOsManifest`、`ScopedMemory`，以及 K1 的模块工具声明（`tool` 模块，[ADR-K1-01](adr/ADR-K1-01-module-tool-contract.md)） | [decision.md ADR-029](decision.md) |
| `rust/crates/agent24-sidecar-host-protocol` | 桌面 sidecar 宿主的私有协议（有界 NDJSON、Launch 解码、token 校验与脱敏） | [G8-BOUNDED-LAUNCH-DECODER](open-design-workspace/design/G8-BOUNDED-LAUNCH-DECODER.md) |

> OpenAPI 改为由 Rust 生成（B4）仍是计划，不是现状。

### L1 纯领域核心 — `agent24-core`

零框架依赖的状态机（run、审批等的合法迁移）。store 的普通逐行迁移（如 `transition_run`）调用 core 状态机校验；批量恢复 / 超时类路径（孤儿 run 清扫、审批超时 / 中止）以带条件的 SQL 直接编码允许的迁移边。见 [ADR-026](ADR-026-rust-core-polyglot.md)。

### L2 权威与持久化 — `agent24-store`、`agent24-workspace`、`agent24-os-cwd`（另有平台原语 `os-proc`、`os-sqlite`）

- **store**：sqlx + SQLite；哈希链审计日志；以及 M10 带进来的 workspace 权威数据：
  - 注册表与租约；
  - 分配：intent → reservation → materialization → commitment → registration；
  - 保留（retention）；
  - legacy recovery holds；
  - run 的 workspace 准入与终止；
  - 决策日志（D0-2，[ADR-034](decision.md)）：`decision_log` + `decision_outcome`，plain DTO（不依赖 `agent24-decide`），写入/导出/三种删除/保留期 scrub，无生产调用方。
- **workspace**：`WorkspaceService`（`rust/crates/agent24-workspace/src/service.rs`）把持久化的根目录证据和**用文件描述符钉住的真实目录**组合起来，签发进程内的 `WorkspaceHandle`。**当前仅 Unix**：Unix 上非 ephemeral 的 daemon 启动时组合它；非 Unix（Windows）上 daemon 不组合 `WorkspaceService`（crate 的非 Unix 实现直接返回 `UnsupportedPlatform`；产品 API 因 service 未组合返回 `503 workspace_unavailable`），workspace-bound run 不可用，只有 legacy 路径。
  - **workspace-bound run**（带 `workspace_id`）：`fs_read` / `fs_write` 的路径经 `WorkspaceHandle` 在钉住的根内解析；`shell_exec` **只**把子进程 cwd 钉在 workspace 根并照常走审批，**不是 OS 沙箱**，命令本身仍可访问根目录之外（ADR-002）。每次需要时重新校验 run / 租约 / workspace 事实。
  - **legacy run**（不带 `workspace_id`，今天的默认情况）：工具上下文是 `ToolContext::legacy`，文件工具退回到工具自身配置的 allowlist 根目录，shell 退回到配置的 `workdir`。
- **os-cwd**（Unix-only）：只在 fork 之后、exec 之前做 `fchdir(2)`，让子进程 cwd 就是被钉住的目录。
- **os-proc**（平台原语）：内核侧的进程身份原语（CORE-2 D5），agent24d 的 enroll 端点用它校验对端代码签名身份（macOS）。无内部依赖，只被 agent24d 使用。
- **os-sqlite**（平台原语）：SQLite 在线备份，直接用 sqlx 已持有的连接句柄。唯一的生产调用方是 CORE-5 的 `migration_guard/snapshot.rs`，随 `ENABLED=false` 一起休眠。

`os-proc`、`os-sqlite` 的 `os` 是「操作系统」，与 L4½ 的 `os-proto` / `os-sdk` / `os-packages` 里的 `os`（「领域 OS」）不是一回事。Q7 已拍板不改名，只在这里写清（R1 §6.1）。

权威：[ADR-002 Workspace 契约](open-design-workspace/design/ADR-002-WORKSPACE-CONTRACT.md)、[ADR-006 Legacy recovery holds](open-design-workspace/design/ADR-006-LEGACY-RECOVERY-HOLDS.md)、[G1/G2 scratch service plan](open-design-workspace/design/G1-G2-SCRATCH-SERVICE-PLAN.md)。

> workspace v1 的取值：受信的 scratch 构造器只产生 `kind = orchestrator_scratch`、`writeback_policy = external`、`concurrency_policy = serial`，TTL 默认 24h、最长 7 天。wire 类型为前向兼容保留开放字符串；存储层另有内部的 `legacy_compat` 种类。

### L3 内核能力服务

| crate | 职责 | 关键约束 / 状态 |
|---|---|---|
| `agent24-models` | 模型网关：`ModelProvider` trait + OpenAI 兼容适配 + 有序注册表；`ModelRouter`（`router.rs`，M-D/D2）按 `TaskProfile`（`Privacy::{Any, LocalOnly}` × `Complexity::{Simple, Complex}`）在 `Tier::{Local, Remote, Lora}` 间选 provider，带健康/冷却 | `LocalOnly` 对远端 fail-closed（本地全挂也报错，不外发）。`ModelRouter::from_env` 默认构造 oMLX 与 Ollama provider；设置了 `IDORIS_URL` 时改为只走 iDoris 网关（`router.rs`）。**主对话、子 agent、REST 聊天默认传 `TaskProfile::default()`**；Guardian、模块推理回调以及来源策略为 `LocalOnly` 的 run（`merge_source_policy`）会设置 `LocalOnly`，任务画像生成 📐（ID-1）。Agent24 内没有出境脱敏代码（已定由 iDoris 出口网关负责）。**iDoris provider ✅**：ADR-032 P4 已接线——`idoris.rs` 要求 `IDORIS_URL` 是回环 HTTP(S) 源，请求带 `X-iDoris-Privacy` 头；推理回调流式（P3）未核实 |
| `agent24-tools` | 工具 trait + 注册表 + 内建工具（`http_fetch`、`fs_read`、`fs_write`、`shell_exec`） | 固定流水线：normalize → 能力白名单 → 审批门 → 超时执行；文件系统权限见 L2 的 workspace-bound / legacy 两条路径 |
| `agent24-policy` | 审批 broker + 工具门；可选的 **Guardian**（`guardian.rs`，M-D/D3）：在人工审批前用 LocalOnly 本地模型给工具调用评风险 | fail-closed：超时即拒绝，取消即 aborted，store 行为权威，重复决策 409。Guardian **默认关闭**（`A24_GUARDIAN=1` 才开）：只有明确解析出 `low` 才自动批准，其余一律交给人；`exec` 默认始终要人审 |
| `agent24-memory` | M-D 记忆：D1 的 `KvStore` + `CanonicalSession`，以及 artifact / assertion / condenser / consolidator / event / knowledge / retriever / vector / writer 等高层模块 | agent loop **已消费** D1 的 `KvStore` / `CanonicalSession`，并在每次 run 前用 `retriever().search_any` 做跨会话 recall（`agent24-agent/src/session_memory.rs`，受个人记忆暂停开关和预算约束）；event 层被领域 OS 的 memory 回调使用；consolidator 等其余高层是否已被 agent loop 消费未核实 |
| `agent24-scheduler` | cron / every / at 调度 | 先推进 `next_run_at` 再触发；跳过漏掉的 tick；连续失败自动禁用；ME-4 起可把触发投递给模块 |
| `agent24-mcp` | 外部 MCP server 适配 | 外部工具包装成普通 `Tool`，与内建工具走同一条审批流水线 |
| `agent24-comm`（**显式例外**） | Hyphae（Nostr）CLI 调用、keystore 串行化、密码存储、daemon 监管、NIP-49 备份恢复、群聊，**以及自带的 axum REST 路由** | 锁里的 sha256 加 macOS 签名或公证校验（D2a），拒绝原因 `signature_rejected` / `hash_mismatch`；密码只走 stdin（[COMM-HYPHAE](design/COMM-HYPHAE.md)）。**例外**：L3 里只有它自带对外 REST 面（`comm/src/router.rs`；axum 依赖见 `comm/Cargo.toml`），路由前缀 `/api/v1/comm` 却由 agent24d 的 `comm_routes.rs` 挂载；别的 L3 crate 都只给 trait / 类型，路由留在 L5。这是已知例外，不要照着新增；Q6 已拍板旧 nostr-bridge 以后并入 comm（R1 §3.3） |
| `agent24-worker` 🟡 | Python ML Worker（embedding / whisper）的 Rust 侧 wire 契约 + HTTP 客户端 | workspace 内没有任何 crate 依赖它 |
| `agent24-documents-client` | L6 组件 documents 的 HTTP 客户端库 | 无内部依赖，只被 `agent24-cli` 使用；归 L3 是本文的暂定放法（R1 §3.1） |
| `agent24-idoris` ✅ | iDoris 托管：`idoris.lock.json`（schema 2，是 `ComponentLock` 之上的视图）、钥匙串、监督进程（supervisor）、oMLX 探测、运行期契约检查（`check_contract`） | 依赖 `agent24-components` 和 `agent24-hw`；下载、安装、验签用 components（`download.rs` / `install.rs` 只剩薄封装）；agent24d 的 `idoris_host` / `managed_idoris` 组合它并把网关接进 `ModelRouter` |
| `agent24-components` ✅ | 通用组件机制（ADR-038 C1 / C3）：schema-2 `ComponentLock`、通用下载器（`download_locked_asset` / `download_pinned`）、不可变安装器、macOS 签名或公证校验（`SignatureVerifier`，Install / AtRest 两种模式）、组件状态机 `ComponentState` | **没有内部依赖**，被 `idoris` 和 agent24d 的 `hyphae_install.rs` 使用（C1，#1539）；Hyphae 的按需安装走它。`fetch_verified` 已不存在 |
| `agent24-knowledge` ✅ | Knowledge v1 wire 类型、HTTP 传输、Mock、Provider 与工具调用层 | agent24d 提供宿主接口（`knowledge_host`，仅 ProductHost 可用）、后端（`knowledge_backend`）和 `knowledge_search` / `knowledge_read` 工具；仅绑定了 Knowledge 的 run 才会拿到这些工具；crate 内 Provider 尚未接线（见 `knowledge_backend.rs` 注释） |
| `agent24-hw` ✅ | 硬件探测与内存账（HW-1，[ADR-036](decision.md)，设计 [HW-TIERS](design/HW-TIERS-capability-hardware-matrix.md)）：`HardwareProfile` v2（芯片、统一内存、GPU wired 上限、独显、**全部挂载卷**含外接盘、模型目录所在盘余量、电池）+ `HardwareProbe`/`SystemProbe`/`FakeHardwareProbe` + `HwLevel::L1..L6` + `usable_bytes` 预算（`A24_HW_BUDGET_CAP_GB` 可压低以模拟小内存机） | 只依赖 serde/serde_json/sysinfo，不依赖任何其它内部 crate；外部命令（sysctl/sw_vers/pmset/nvidia-smi）全带 2s 超时、失败为 `None`；agent24d 启动时写 `~/.agent24/hardware.json`（`hardware.rs`），`agent24 hw probe` 打印；**runtimes 未探测、能力梯/解析器未做**；HTTP 侧只有经 decide 暴露的 `GET /api/v1/decide/profile` |
| `agent24-decide` 🟡 | 决策服务（D0-1，[ADR-033](decision.md)）：`DecisionRequest`/`Question`/`Decision` 等类型 + `DecisionBackend` trait + `RuleBackend` + 级联 `DecisionService`；接口形状照 Jev `/v1/systemone`（choice/noul/score + 校准概率）。D0-2（[ADR-034](decision.md)）追加 `log` 模块：决策日志的 `DecisionLog` trait + DTO（`LogEntry`/`OutcomeEntry`/`ExportFilter`/`DeleteSelector`） | 只依赖 `agent24-hw`（硬件探测与分档，ADR-036）；**agent24d 只用它服务 `GET /api/v1/decide/profile`（`decide_routes.rs`），agent loop 里没有调用点**；`DecisionLog` 无实现，等 D1 接线（记住意图 + 召回门控）时才有人实现 `DecisionLog`（预期落在 `agent24d`，ADR-034）。CLI 的 `decide export`/`delete` 直接读写 `agent24.db` |

### L4 运行时内核 — `agent24-agent`

run 管理器与 agent loop。正常依赖：core / memory / models / protocol / store / tools / workspace。审批门由 agent24d 组合 `agent24-policy` 后注入，`agent24-agent` 自身只在测试里依赖 policy。

**取消**：模型调用、工具执行、审批等待、session 锁和有界的记忆写入这类长等待，都显式与 `CancellationToken` 竞速；少量短的持久化 await（起止状态迁移、写消息）不竞速，只在边界前后检查取消。工具调用全部落库、事件化，被拒绝的全部审计。

### L5 组合根 — `rust/apps/agent24d`

内核运行时的组合根：

- **对外 API**：v1 REST + WS（runs、sessions、approvals、comm、os、attached…）。workspace 目前**没有**对外 REST 路由（产品构造、host lease、resolve 都未开放），只能经内部 run 准入路径使用。
- **鉴权模式**：
  - `legacy_single_token`（`agent24d serve` 的缺省值）✅：生成一个全权 bearer，写进 `~/.agent24/daemon.json`。Windows 上的 Desktop 和 Node mock daemon 走这条路径。
  - `capabilities` ✅（Unix 上 Desktop 的默认：`backend-manager.ts` 的 `selectedAuthMode`，以 `--auth-mode capabilities --host-bootstrap-stdio` 拉起）：`daemon.json` 不含凭据，`ProductHost` 只经私有 ready pipe 交给宿主父进程。已实现：令牌库、`CreativeRuntime` 的 mint / 吊销（claims 含 workspace / attachment / principal）、TTL 与代际、**默认拒绝**的全量路由映射（`src/capabilities/route_policy.rs` 的 `required_operation`）。
  - **CORE-2 一方外壳登记** ✅（[CORE-2-ENROLL-DESIGN](agent/CORE-2-ENROLL-DESIGN.md)、[CORE-2-AGENTEAR-SLICE](agent/CORE-2-AGENTEAR-SLICE.md)）：`--enroll-endpoint` 开启 `<data_dir>/enroll.sock`（`enroll_endpoint.rs`），校验对端代码签名身份（`agent24-os-proc`，macOS）后签发 `FirstPartyShell` 的日常令牌（`first_party.rs`，TTL 1 小时）；提升级操作需 `elevate`，由独立的在场确认（`presence.rs`）换取 60 秒、单次使用的 grant。CLI 侧见 `agent24-cli/src/enroll.rs`。
  - **尚未实现**（📐，见 [ADR-005](open-design-workspace/design/ADR-005-CAPABILITY-AUTHORITY.md)）：从请求中提取 workspace / session / run 资源并校验、durable 的 session / run 归属、事件过滤与每次发送前复验、broker / handoff。`CreativeRuntime` 令牌仍只放行 `GET /api/v1/models`（`creative_route_permitted`），新映射的路由对它保持关闭。Desktop 的 `backendProxy` 已有路由白名单（`backend-proxy.ts` 的 `ALLOWED_PROXY_ROUTES`），提升级操作走专用 IPC，不经代理；是否满足 [PLAN-OD-NEXT](agent/PLAN-OD-NEXT.md) F11.0 的全部验收未核实。`agent24 acp` 在 capability 模式下代码路径存在（CLI 的 `Endpoint` 持有 enroll 会话），真机未核实。
- **领域 OS 挂载**（`src/domain.rs`）：进程内模块的授权上限 `KERNEL_GRANTS` = Events / Memory / Approval；进程外模块 `KERNEL_OOP_GRANTS` 再多 Scheduler / Models。
- **回调面**：审批、调度、推理、记忆回调（ME-4），见 [PLAN-ME4-OS-CAPABILITIES](agent/PLAN-ME4-OS-CAPABILITIES.md)。
- **模块工具（K1）** ✅：`module_tool_runtime.rs` 把进程外模块声明的工具以 `ModuleToolRuntime` 接进 `agent24-tools` 的注册表，授权、撤销通知、结果上限（64 KiB）见 [ADR-K1-01](adr/ADR-K1-01-module-tool-contract.md)；Documents 是第一个消费方。
- **组件托管**：`idoris_host` / `managed_idoris`（iDoris 安装、监督、网关发布）、`hyphae_install`（Hyphae 按需安装，用 `agent24-components` 的 `download_locked_asset`）、`knowledge_host` / `knowledge_backend` / `knowledge_tools`（Knowledge 宿主接口与工具）、`hardware` / `decide_routes`。
- **统一组件状态**：`component_routes.rs`（`GET /api/v1/components`、`POST /api/v1/components/{id}/report`，ADR-038 C3a，#1545）聚合 iDoris、Hyphae、attached、Documents、Knowledge 的状态；Open Design 的状态由桌面 main 上报（C3b，#1549）。docked 诊断 router（`docked_router.rs`，CORE-5f）只在停靠态提供，且在 `ENABLED=false` 之后。
- **迁移协调器（CORE-5）** 🟡：`migration_coordinator.rs:20` 是 `ENABLED = false`，`run` 没有调用方；`main.rs:114` 有编译期断言 `assert!(!migration_coordinator::ENABLED)` 把它钉死。5f 的停靠类型、router 和启动接线已合入 main（`server.rs` 的 `docked_startup::pre_start`，#1528、#1540），但都在 `ENABLED` 后面，`pre_start` 因 guard 为 false 直接跳过。`migration_guard/` 是快照 / 回滚等配套。剩 5g / 5h / 5i（见 [CORE-5 设计](agent/CORE-5-MIGRATION-GUARD-DESIGN.md)）。
- **附着注册表**：A3 模块的监听 socket、握手、代际簿记、反向命令。

**agent24d 的体量与瘦身（Q3 选 (B)，2026-11-04 拍板）**

agent24d 非测试代码约 26,300 行，`server.rs` 一个文件 8,734 行（3,813 行之前是生产代码，`serve()` 一个函数就有约 1,530 行），分组见 [R1 §4](architecture/review-2026-11-03/R1-MODULE-MAP.md)（该文件在分支 `docs/arch-review-r1-module-map` 上）和 [方案 §5 Q3](tracking/2026-11-04/ARCH-OPTIMIZATION-PLAN.md)。结论：

- **下沉三块**，每块都是业务逻辑，不是接线：
  - iDoris 托管（`idoris_host`、`provision`、`managed_idoris`，约 2,640 行）→ `agent24-idoris`；
  - Knowledge 宿主（`knowledge_host` / `backend` / `store` / `tools`，约 1,340 行）→ `agent24-knowledge`；
  - attach 子系统（`attached`、`attach_registry`、`attach_listener`、`attach_commands` 等，约 1,520 行，整体迁，不单拆注册表）→ 新建 `agent24-attach`。
- **鉴权留在 agent24d 核心，不抽 `agent24-auth`**（方案原 S2-5 已取消）。R1 §4 指出这块（约 4,900 行，含 `server.rs:1025-1418` 的鉴权中间件）依赖 `os-proc` 和 `crate::capabilities`，handler 与中间件接线必须留在 agent24d。
- 三块下沉后**仍然编译进同一个 agent24d 进程**，只是代码归属变了，不增加进程，也不改对外 API。HTTP handler 留在 agent24d。
- 每期先移动、后修行为，同一个 PR 不混；前置是本文（新边界先写进文档）。迁移落地之前，上文各 crate 的依赖以当前 main 为准；下沉后依赖表用脚本重新生成。
- `comm_routes` 中的 `CommDaemonSlot` 与 `hyphae_install` 是否下沉另议，不在 Q3 范围内。

### L4½ 领域 OS 运行时 — `os-proto`、`os-fd`、`os-packages`、`os-sdk`

> 原编号 L5½（2026-11-05 改为 L4½）。这组 crate 只依赖 `domain` 和本组内部，**不依赖 L5**，被 L5（agent24d）和 L6（documents）同时依赖，所以是 L4 与 L5 之间共用的下层。旧编号让人以为它在 L5 之上，是误导（R1 §3.1）。

| crate | 职责 |
|---|---|
| `agent24-os-proto` | 内核↔进程外模块协议：版本协商、分帧、握手、启动（trampoline，经 `command-fds` 做 fd 继承）、监管（熔断 / 重启 / 停止记录）、受约束的入站代理 |
| `agent24-os-fd` | 把内核继承下来的监听 fd 安全地接管成 socket |
| `agent24-os-packages` | 领域 OS 包的发现与原子安装（`agent24 os install`） |
| `agent24-os-sdk` | 给模块作者的 Rust SDK（`Module` / `ModuleBuilder` 与五个 typed client）；daemon 只在测试中依赖它 |

权威：[SPEC-ME3-OUT-OF-PROCESS](specs/SPEC-ME3-OUT-OF-PROCESS.md)、[WIRE-OOP-MODULE](specs/WIRE-OOP-MODULE.md)、[ME4-S3 os-sdk](design/ME4-S3-os-sdk.md)。

### L6 扩展面 — 四类"外挂"

> 按 ADR-039（已接受）对外只有「本体 / 能力 / 技能」三个概念；下面四类外挂在实现上都是「组件」（能力的安装包，契约见 [ADR-038](adr/ADR-038-component-contract-v1.md)，C1–C8 已全部完成），区别只在运行形态（进程内 / 进程外 / 附着 / sidecar）。本文暂时保留现有四类分类，便于对照代码。

| 类型 | 例子 | 谁启动进程 | 信任假设 | 通道 | 权威文档 |
|---|---|---|---|---|---|
| ① 进程内 DomainModule 🟡 | 当前无（生产 catalogue 为空，没有生产代码构造 `Build::InProcess`；挂载缝已实现并测试） | 编进 daemon | **可信代码**；Rust 可见性不是沙箱 | 进程内 trait | [ADR-029](decision.md) |
| ② 进程外领域 OS ✅ | Sin90、Cos72、Documents（`apps/agent24-documents`，经 K1 模块工具暴露） | daemon 拉起 / 监管 / 停止 | 同 UID、合作但可能有 bug；**不防**敌意本地二进制 | UDS + 一次性回调 socket + 握手 token | [SPEC-ME3](specs/SPEC-ME3-OUT-OF-PROCESS.md)、[ME4-S3 os-sdk](design/ME4-S3-os-sdk.md) |
| ③ 附着模块 ✅ | AgentEar（经 CORE-2 登记） | 用户 / launchd / 自己 | 同②，但 token 长期有效、断连不杀 | 常驻 attach socket 上的 JSON-RPC | [A3-ATTACHED-MODULE](design/A3-ATTACHED-MODULE.md)、ADR-032 |
| ④ Sidecar 集成 | Open Design | 见下表 | 经 pin / hash 校验的 bundled sidecar 属于**本地应用 TCB**；capability 是应用层最小权限，**不是**同 UID 的 OS 沙箱，敌意 sidecar 不在当前威胁模型内（ADR-005） | 见下表 | [ADR-001](open-design-workspace/design/ADR-001-INTEGRATION-BOUNDARIES.md)、[ADR-003](open-design-workspace/design/ADR-003-ACP-BRIDGE.md)、[ADR-004](open-design-workspace/design/ADR-004-DESKTOP-HOST.md)、[ADR-005](open-design-workspace/design/ADR-005-CAPABILITY-AUTHORITY.md) |

**④ Open Design 的现行路径与目标形态**

| 方面 | 现行产品路径 ✅ | 目标形态（ADR-004/005）🟡 |
|---|---|---|
| 进程托管（Q1 = (b)，#1580、#1584） | **打包模式**：`CreativeServeWeb` 选启动方式和下载前置，进程交给 `SidecarManager`（TS）托管（C8，#1580；Open Design 当前 pin 0.26.1，#1544）：launcher 以 `detached` 拉起，POSIX 独立进程组；就绪、健康检查（`GET /api/ready`，连续 3 次失败报 `degraded`，不自动重启）、停止（TERM 只发 launcher，15 s 宽限后对整个进程组 SIGKILL）、崩溃后整组回收都由 manager 负责；崩溃后懒重启（下次打开或点重启）。**checkout 模式**仍由 `CreativeServeWeb` 直接拉起 `node … od.mjs`。**Windows 暂不支持**（launcher 返回 `unsupported platform`，Job Object 留给 sidecar-host）。端点不鉴权（`auth: 'none'`）。已知缺口：OD daemon 自己 detach 的 agent 子进程在强杀时可能残留；Electron 被 SIGKILL 时进程组会留下 | 在此基础上接 `agent24-sidecar-host`（Rust，Windows Job Object / 父进程死亡监视），并补自动有界重启（ADR-004 §6） |
| 分发（2026-10-05 裁决） | **不再打进安装包**（曾把 AppImage 从 146MB 顶到 934MB、deb 从 101MB 顶到 622MB）；电子打包只带一个小 manifest，首次点开 Design 页时由 `open-design-component.ts` 按需下载、校验 sha256、解压、再启动 | 同左 |
| 界面 | Creative 页是 `WebContentsView`，使用**全局持久**的 session 分区 `persist:agent24-creative`，带同源导航拦截和请求代际栅栏 | 按 app 实例 + workspace 派生的**非持久**分区，detach / 切换时清除 cookie、localStorage、cache、service worker（ADR-004） |
| 运行时协议 | Open Design 每个会话拉起 `agent24 acp`，把 ACP 会话/提示映射成 REST/WS run，执行仍在 agent24d；桌面把包内 CLI 经 `AGENT24_BIN` 交给 OD（#1555） | 同左 |
| 凭据 | `agent24 acp` 用 CLI 的 `Endpoint` 令牌：legacy 模式是 daemon.json 里的全权 bearer，capability 模式是 enroll 得到的一方日常令牌（`acp.rs`），都不是受限的 `CreativeRuntime` 令牌 | daemon 以 capability 模式运行，Open Design / ACP 只拿 `CreativeRuntime` 受限令牌；需先实现路由级资源授权、durable 归属、事件过滤、broker / handoff |
| workspace | `agent24 acp` 建 session / run 时 `workspace_id` 为 `None`，走 legacy 文件权限 | 每个 Open Design 项目绑定一个 scratch workspace，run 是 workspace-bound |

也就是说，**M10 落地了现行集成路径和 capability / workspace / sidecar 的基础组件，并通过了发布 gate；但 ADR-001/005 承诺的"第三方只拿受限令牌、只在自己的 workspace 里干活"在今天的产品路径上还没有生效**。这是 Open Design 下一阶段的首要工作。

另有 **MCP** ✅：外部 MCP server 的工具进入与内建工具相同的审批流水线，是"工具级"扩展，不是"模块级"扩展。

### L7 外壳

- **apps/desktop（Electron）**：唯一的桌面 main 进程。`BackendManager` 托管 agent24d（Unix 默认 capability 模式）；`SidecarManager` 托管 Open Design 的 headless 进程（C8），`CreativeServeWeb` 负责选启动方式、下载前置和对外状态；组件页读 `/api/v1/components`，OD 状态由 main 上报（C3b）；`agent24-sidecar-host` 尚未接线（Q1 = (b) 后冻结）。
- **agent24-cli**：v1 协议客户端（capability 模式下先 `enroll`，需要时 `elevate`）；`agent24 acp` 是 ACP-over-stdio 映射桥，不持有执行逻辑。
- **packages/**：
  - `api-client`：由 OpenAPI 生成的 TS 客户端；
  - `contract-tests`：契约测试；
  - `nostr-bridge` / `wechat-bridge`：通信桥（nostr-bridge 不再是 agent 的 Nostr 身份来源，身份以 `<data>/comm/hyphae-home` 的钥匙库为准，以后并入 comm，Q12 X7 / Q6）；
  - `data-dir`：平台数据目录解析（ADR-037，镜像 `agent24-protocol` 的 `data_dir.rs`）；`documents-accept`：Documenting 验收 harness；
  - `node-daemon`：**开发用的旧 API mock**，只实现 124 个 v1 op 中的 4 个（`GET /health`、`/models`、`/usage`、`POST /chat`，`v1/routes.ts:60-89`），其余返回 404，另带一组非 v1 的旧接口；打包构建强制使用 Rust 后端（`backend-manager.ts:173-176`）。**不是客户端**，也不能当 v1 参考实现（R1 §7.4）。

---

## 3. 几个容易混淆的概念

### 3.1 "能力"有两层意思

1. **内核借给模块的能力** ✅：`agent24-domain::Capability`（Events、Models、Scheduler、Policy、Memory、Approval）。模块拿到的是**句柄**，没授权就没有句柄；刻意不提供"先查授权再取句柄"的旁路。
2. **外部客户端能调哪些 API** ✅：agent24d 的 capability token（`Audience::{ProductHost, FirstPartyShell, CreativeRuntime}`），只在 `--auth-mode capabilities` 下生效（Unix 上 Desktop 默认启用）；路由级资源授权尚未实现（见 L5）；legacy 模式下只有一个全权 bearer。

前者管"模块能用哪些内核服务"，后者管"外部进程能调哪些路由"。

### 3.2 内核、OS、Workspace 的关系

- **内核**（agent24d + L1–L4）是 run / 模型 / 工具 / 审批 / 审计的权威。
- **领域 OS**（Sin90、Cos72）是"住在内核上的应用"：有自己的存储和路由，通过句柄消费内核能力。
- **Workspace** 是 **workspace-bound run** 的文件系统权限来源（当前仅 Unix）：对外只暴露不透明的 `workspace_id`，真实根目录、租约、TTL、清理只在内核内部。不带 `workspace_id` 的 legacy run 仍使用工具 allowlist / `workdir`。目标是新的第三方集成都走 workspace-bound 路径。
- Open Design 走的是**第 ④ 类 sidecar**，不是领域 OS。ADR-001 明确：若将来改成领域 OS 挂载，必须新开 ADR。

### 3.3 "Workspace"这个词的另一个用法

[SPEC-ORG-SPACE](specs/SPEC-ORG-SPACE.md) 里的 Workspace 指"此刻哪些空间在作用域内"的**记忆授权**概念（设计中，未实现），与本文 L2 的文件系统 workspace 不是一回事。

---

## 4. 安全与权限边界（由外到内）

1. **进程边界** ✅：②③④ 都在独立进程里；②③ 的威胁模型明确不防敌意的同 UID 本地二进制；④ 的 bundled sidecar 被当作本地应用 TCB，更强隔离需要 OS 沙箱。
2. **凭据边界**：
   - ✅ capability 模式（Unix 上 Desktop 默认）：全权 `ProductHost` 不落盘；一方外壳经 enroll 拿短期日常令牌，提升级操作另需 `elevate` + 在场确认。
   - ✅ legacy 模式（`serve` 缺省、Windows）：只有一个全权 bearer，存在 `daemon.json`（Unix 下目录 `0700`、文件 `0600`，只隔离其他用户，不隔离同用户 sidecar；非 Unix 当前未显式设置等价 ACL）。
   - 📐 目标：第三方只拿限定 workspace、短 TTL、可吊销的令牌（令牌基础设施已实现，路由级资源授权未实现）。
3. **审批边界** ✅：所有工具（内建、MCP、模块触发）走同一条 fail-closed 审批流水线；第三方的 "permission response" 不是授权凭据。
4. **文件系统边界**：
   - 🟡 workspace-bound run（仅 Unix；运行时准入、handle、工具接线已实现，但默认 Desktop / ACP 路径还不可达）：`fs_read` / `fs_write` 的路径在描述符钉住的根内解析；`shell_exec` 只用 `fchdir` 钉住 cwd，不是 OS 沙箱；
   - legacy run：靠工具 allowlist 与 `workdir`。
5. **unsafe 边界** ✅：工作区默认 `unsafe_code = "forbid"`（`rust/Cargo.toml:49`），只有四个 crate 在各自 `Cargo.toml` 里降为 `deny` 并开了窄例外：
   - `os-cwd`：post-fork 的 `fchdir`（1 个 `unsafe` 块）；
   - `os-fd`：接管继承 fd 的所有权（2 个块）；
   - `os-proc`：进程身份原语（5 处 allow、8 个块）；
   - `os-sqlite`：SQLite 在线备份的原始句柄调用（1 个块）。

   `os-proto` 的 fork/exec fd 处理交给 `command-fds` crate，自己保持 forbid。前两处经过人工审计；`os-proc`、`os-sqlite` 是否经过同等审计未核实（R1 §8）。
6. **组件制品边界** ✅：组件发布物按锁定的 sha256 加 macOS Developer ID 签名或公证校验（D2a，`components/src/sign.rs`，Install / AtRest 两种模式），用于 iDoris 和 Hyphae。它**不是**领域 OS 签名（ME-6）。Q15 已拍板：OS 包（Sin90、Cos72 等）也要有文件清单和哈希，哈希不对就拒绝加载，不做「先只记录」的过渡；这一条尚未实现（见 §7）。
7. **法律层**：[docs/laws/](laws/README.md)，包括上下文准入（CONTEXT）、记忆隔离（MEMORY）、审批授权（APPROVAL）。改动涉及哪条法律，要在 PR 模板里写明。

---

## 5. 未来扩展会互相影响到哪里

| 扩展 | 会波及 | 建议 |
|---|---|---|
| capability 路由级授权 + workspace 绑定（Open Design 下一阶段） | agent24d：请求资源提取、session / run durable 归属、事件过滤与复验、broker / handoff；`agent24 acp` 取令牌与传 `workspace_id`；CLI / TUI 兼容（legacy 与 capability 互斥）；Creative 分区与生命周期；Windows 上的 workspace 支持 | 这是把 ADR-001/005 从"组件就绪"变成"产品生效"的关键一步（启动参数已在 Desktop 默认打开，缺的是资源级授权）；先于接入任何新的第三方工具 |
| 给 `Capability` 加新能力 | 契约层、`KERNEL_*_GRANTS`、os-sdk 客户端、协议版本协商、所有模块类型 | 影响面最大；先写 ADR，再按 ME-4 的切法逐片落地 |
| 新 workspace 类型 / 支持写回源码 | tools、policy（写回需审批）、store 结构、ADR-001/002 | 先定写回审批与并发策略 |
| 接第二个 sidecar 工具（例如另一个 Apache-2.0 创作工具） | 复用 L6④ 全套；但 `CreativeRuntime`、`creative_attachment_id`、`CreativeServeWeb` 是 Creative 专名 | 先完成上面第一行，再把专名泛化成按工具区分的 audience / attachment / 托管器 |
| M1 记忆产品化 | `ScopedMemory`、领域 OS 记忆分区、agent loop、`laws/MEMORY.md` | recall 已进 agent loop；其余高层（consolidator 等）先核实消费情况，再谈跨 OS 共享 |
| ② 与 ④ 两套扩展模型长期并存 | 生命周期、授权、协议各一套 | 中长期考虑共用一套令牌 + workspace 权限模型 |
| iDoris provider 扩展（P3 流式等） | `ModelRouter::from_env`、`idoris.rs`、隐私头 `X-iDoris-Privacy` | 保持 `LocalOnly` 对远端 fail-closed |
| 领域 OS 签名（ME-6） | os-packages 安装、启动校验 | 当前威胁模型明确不防敌意本地二进制，签名也解决不了这一点 |

---

## 6. 第三方开源工具接入的可复用路径（来自 M10 经验）

1. **边界**：照 [ADR-001](open-design-workspace/design/ADR-001-INTEGRATION-BOUNDARIES.md) 写一份。Agent24 是唯一 AI 控制面，第三方只是工作区，不拿工具权限。
2. **源码**：fork + pin，本仓库只放契约、宿主、桥、pin、集成测试；Apache-2.0 记得保留 NOTICE。
3. **进程**：用 `SidecarManager`（C8 起 Open Design 已走它）：每个工具只写自己的 launcher、health 和 tree controller（参考 `open-design-sidecar.ts`）。目标再接 `agent24-sidecar-host`（Windows 与父进程死亡监视）。
4. **运行时协议**：能说 ACP 就复用 `agent24 acp`；否则写一个同样"只做映射、不持有权限"的桥。
5. **权限**：一个 workspace + 一个限定在该 workspace 的短期令牌。前提是 capability 的路由级资源授权已实现、并已在产品路径启用（见 §5 第一行）。
6. **验收与落地**：精确 SHA 的 E2E（M10 用的 harness 保存在 PR #654 的 `refs/pull/654/head`）；锁定 main、打 tag、合并预演、合并后再跑 gate（[M10 落地指南](design/OPEN-DESIGN-M10-MAIN-LANDING-GUIDE.md)，现作为 M10 的历史审计记录保留）。

**M10 的教训**：
- integration 分支漂得太久：最后 633 个提交、约 200 个叠加 PR；
- 设计文档没随第一片落地，差点在清理 PR 时丢失；
- "组件落地"与"产品路径生效"没有分开验收，导致 capability 基础设施 / SidecarManager 当时落地了却没接线（SidecarManager 已在 C8 接线，capability 路由级授权仍未做完），capability 的路由级授权也还没做完。

下次按切片尽早落 main，ADR 随第一片进 main，并把"产品路径接线"列为单独的验收项。

---

## 7. 尚未完成、不要误以为已经有的

- 📐 capability 的路由级资源授权（workspace / session / run 资源提取与校验）、durable 归属、事件过滤与复验、broker / handoff；`CreativeRuntime` 仍只能读模型目录。
- 🟡 capability 模式在 Windows 和 `serve` 缺省路径上不是默认；`agent24 acp` 在 capability 模式下代码路径存在（CLI `Endpoint` 带 enroll 会话），真机未核实。
- 📐 Creative 的按 workspace 非持久 session 分区（现为全局持久分区）。
- 🟡 workspace-bound run 仅 Unix；Windows 上只有 legacy 路径。
- 📐 workspace 的产品构造、host lease、续期 / 释放、resolve 等对外路由未开放（`WorkspaceService` 的产品构造按代码注释仍关闭）。
- 🟡 进程内 DomainModule 挂载缝已实现，但当前没有生产内置模块。
- 🟡 `agent24-sidecar-host`（Rust，Windows Job Object / 父进程死亡监视）未接入 Desktop，没有任何 Rust 消费者，Q1 = (b) 后冻结。Open Design 已由 `SidecarManager`（TS）托管（C8，#1580、#1584）；Windows 暂不支持。
- 🟡 `agent24 acp` 不传 `workspace_id`，Open Design 的 run 不是 workspace-bound。
- 🟡 **CORE-5 迁移协调器未接线**：`migration_coordinator::ENABLED = false`（`:20`），`main.rs:114` 的编译期断言保证没人悄悄打开；`run` 没有调用方。5f（停靠类型、router、启动接线）已合入 main，但都在 `ENABLED` 后面；剩 5g 外壳识别、5h 安装器回退、5i 授权 retry。
- 🟡 **ADR-038 收尾**：C1–C8 已全部完成（Open Design 按 Q1 = (b) 由 `SidecarManager` 托管）。仍未做的是运行期契约检查（Agent24 侧没有向 Hyphae 做 `hyphae-cli` 契约握手，只有 iDoris 做 `check_contract`）；Hyphae 锁只有 darwin-arm64 和 linux-x64 两个平台。
- 🟡 `agent24-decide`：只有契约、规则后端和 profile 路由；`DecisionLog` 无实现，agent loop 无调用点（D1）。
- 🟡 `agent24-hw`：runtimes 未探测，能力梯 / 解析器未做。
- 🟡 `agent24-knowledge` 的 Provider 未接线，agent24d 当前用自己的后端直连；Knowledge 工具只给绑定了 Knowledge 的 run。
- 🟡 `agent24-worker` 只有 Rust 侧 wire / client / mock，且无消费者；Python worker 服务端（D4b）尚未实现。
- 📐 推理回调流式（iDoris P3）。
- 📐 任务画像生成（主路径默认 `TaskProfile`，仅来源策略等会置 `LocalOnly`）；出境脱敏（归 iDoris 出口网关）。
- 📐 Web3（AAStar）身份与结算：Agent24 中没有接线。
- 📐 OpenAPI 由 Rust 生成（B4）。
- workspace 只有 scratch 一种受信构造；写回、其它类型、并发策略未定。
- 📐 领域 OS 签名（ME-6）。组件签名（D2a）已有，但不覆盖领域 OS 包。
- 📐 OS 包完整性（Q15）：OS 包的文件清单和哈希校验、哈希不对即拒绝加载，尚未实现。
- 📐 agent24d 瘦身（Q3 (B)）：iDoris 托管下沉 `agent24-idoris`、Knowledge 宿主下沉 `agent24-knowledge`、attach 新建 `agent24-attach`，尚未动手；鉴权留核心。
- 📐 通讯入口统一准入（Q12 X11）：Nostr 入站、语音输入、远程 @agent、手机遥控等外部触发 agent 的入口，要统一走 ADR-041 的准入关卡，尚未实现。
- `docs/laws/` 目前只有上下文、记忆、审批三部法条。
- Review 遗留：[#661](https://github.com/iDoris-ai/Agent24/issues/661)（Creative view）、[#663](https://github.com/iDoris-ai/Agent24/issues/663)（M10 叠加 PR 遗留）。

## 相关

- **各结构组件的里程碑路线与依赖**：[agent/COMPONENT-ROADMAP.md](agent/COMPONENT-ROADMAP.md)

- 内核架构与不可动摇的边界：[agent/architecture.md](agent/architecture.md)
- 执行状态权威：[agent/tasks.md](agent/tasks.md)
- 全部 ADR：[decision.md](decision.md)
- Open Design 集成设计全集：[open-design-workspace/](open-design-workspace/README.md)
