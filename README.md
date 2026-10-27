# Agent24

> 面向 24/7 AI agent 的 **shell-agnostic 框架**——Rust 内核 + daemon 提供统一的"个人 AI 助手"承载能力，外壳与核心解耦：自带 Electron 参考外壳，任何外壳都能挂上来（如 Tauri 的 Pet0）；支持可插拔能力模块、本地 & API 多模型适配、分层记忆、跨 agent 通信。

## 定位

**Agent24 是框架，不是应用。** 我们提供：

- **外壳无关（shell-agnostic）**：Rust 内核 + daemon 与前端解耦，壳只经 HTTP/WS 协议连接——自带 Electron 参考外壳，Tauri 外壳（如 Pet0 桌宠）等同样可挂载
- **多端**：桌面已落地（Electron 参考壳，macOS / Windows / Linux 分发）；移动（iOS / Android）与 Web 规划中，同属 shell-agnostic——移动端计划各提供 Tauri 与 Expo / React Native 瘦壳示例（见 [ADR-027](docs/decision.md)）。daemon 与模型可不在端上（如跑在你的 Mac），移动 / Web 端做瘦壳、只经 HTTP/WS 协议远程消费。**注**：`agent24d` 今天固定绑 `127.0.0.1` 且没有 bind-address 参数，跨设备访问需要自备隧道/反代与相应的认证方案——「远程消费」是目标态
- 后台 daemon + 用户交互一致性
- 标准化能力模块接口：进程外领域 OS 有对外 Rust SDK `agent24-os-sdk`（0.1.0，原型阶段，接口可能变）；能力模块今天仍是 `packages/node-daemon` 的内部 `CapabilityModule`（TS，未对外发布）
- AI 适配层（三级路由 + `ModelProvider` 缝）。**今天生产启动只走 `ModelRouter::from_env()`，构造 oMLX 与 Ollama 两个槽**；tier 按 URL 判定——非 loopback 的 `OMLX_URL` 会被重标为 `Remote`，好让 LocalOnly 的任务拒绝它而不是悄悄外泄。没有独立的 remote / lora 注册入口，没有 Claude provider，也没有运行时的设置页切换。iDoris 接入排在 P4 门后
- 分层记忆：今天有 L0 KV、canonical session（阈值触发的摘要压缩），以及 M-D 落下的事件日志 / 断言账本 / artifact / condenser / 巩固等**库原语**。**L3 ATIF 轨迹、SkillBank、自进化框架尚未实现**（代码中无此符号），是目标态
- 通过 **Hyphae 菌丝网络**（Nostr）与其他 agent 通信

**应用方**（如小黑书、博客、社区工具等）从本框架 fork，搭载具体场景的能力模块。

> **仓库位置**：本框架仓库现位于 `iDoris-ai/Agent24`（组织由早期文档中的 `AuraAIHQ` 迁至 `iDoris-ai`；历史 rename 背景见 [ADR-015](docs/decision.md)）。

---

## 架构（Rust Core + Polyglot，见 [ADR-026](docs/ADR-026-rust-core-polyglot.md)）

内核是 Rust daemon `agent24d`——唯一核心运行时，也是桌面端默认后端（`AGENT24_BACKEND=rust`，v0.1.0 起）。所有外壳只经 **v1 REST + WebSocket** 协议接入，互不感知实现。

```
┌────────────────────────────────────────────────────────────────────┐
│  外壳（shell-agnostic，只经 v1 REST/WS 接入）                          │
│  Electron+React 桌面（默认/参考壳）· Tauri（如 Pet0）·                 │
│  移动 iOS/Android + Web（规划，瘦壳）· TUI（ratatui）· CLI             │
└───────────────────────────────┬────────────────────────────────────┘
                  HTTP REST + WebSocket（bearer token，动态端口）
┌───────────────────────────────▼────────────────────────────────────┐
│  Agent24 Core = Rust daemon  agent24d                                │
│  /api/v1: sessions · runs · events(WS) · approvals · schedules ·     │
│           models · chat · usage · tools · tool-overrides · os ·      │
│           attached …（进程外领域 OS 经 os-proto 代理挂进 /<name>/*）  │
│  crates: core · agent(Loop) · models(网关+三级路由) · scheduler ·    │
│          store · memory · policy · tools · mcp · protocol ·          │
│          os-proto · os-packages（进程外领域 OS 挂载/代理/回调）       │
└──────────┬───────────────────────────────────────┬─────────────────┘
     契约：protocol/openapi.yaml + events.schema     │ REST
     （两条生成链：REST 手写 openapi.yaml、WS 事件由 Rust 类型导出；两者共同生成 packages/api-client 的 TS 类型，CI 各有一道漂移门）
┌──────────▼──────────────────┐        ┌───────────▼──────────────────┐
│ TS 能力模块 / 协议参考实现    │        │ Python ML Worker（规划）       │
│ packages/node-daemon         │        │ Embedding · Whisper ·          │
│ （v1 协议 mock/参考实现，     │        │ 图像 · LoRA 训练                │
│  CapabilityModule 承载）     │        │ （agent24-ml-worker）          │
└──────────────────────────────┘        └────────────────────────────────┘
```

进程外领域 OS 模块（独立进程，如 Sin90）用 Rust SDK `agent24-os-sdk`（events/memory/approval/scheduler/model 五个客户端）+ `agent24-os-fd`（继承监听 socket 的 fd 传递）接入，不在上图任何一个框里跑——它们连的是内核经 `os-proto` 暴露的握手/回调通道，不是内核进程本身。

> **为什么不是 Node/Python 主后端**（[ADR-026](docs/ADR-026-rust-core-polyglot.md)，取代 ADR-023 的「M3 切 Python FastAPI」）：新内核能力（Agent Loop / 调度器 / 记忆 / 工作流 / 权限）从第一行起写在 Rust，不在 Node 或 Python 主后端里先写一遍。`packages/node-daemon` 保留为 v1 协议的 **mock/参考实现**（`AGENT24_BACKEND=node` 可切），保障协议演进期日常开发不阻塞；Python **仅**用于 ML Worker（不承担会话/权限/持久化/审计）。

### 核心组件

> 状态图例：✅ 已落地（有测试）· 🟡 部分 · 🔲 未建成。职责列只写已落地能力，目标态见 ADR/ROADMAP。

| 组件 | 路径 | 状态 | 职责 |
|------|------|------|------|
| **agent24d**（Rust daemon） | `rust/apps/agent24d` | ✅ | v1 REST+WS 核心运行时；桌面默认后端 |
| **agent24-cli / TUI** | `rust/apps/agent24-cli` | ✅ CLI · ✅ TUI 最小版 · 🔲 chat | Attached/Standalone；TUI（ratatui）runs/事件流/审批队列，headless 运维 |
| **agent24-core** | `rust/crates/agent24-core` | ✅ | 稳定领域模型（Session/Run/Task/ToolCall/Approval/Event/Usage…），零框架依赖 |
| **agent24-agent** | `rust/crates/agent24-agent` | ✅ | Agent Loop：上下文 → 调模型 → 解析 ToolCall → 权限 → 执行 → 续 |
| **agent24-models** | `rust/crates/agent24-models` | ✅ 网关/路由 · 🔲 LoRA | Model Gateway + 三级路由（本地小模型 / 远程 API / 自训领域 LoRA）。**LoRA 训练侧未建**（`agent24-worker` 自己标着 later），三级路由的 LoRA 那一级今天没有 provider 填 |
| **agent24-scheduler** | `rust/crates/agent24-scheduler` | ✅ | cron 式日常工作流调度器 |
| **agent24-store / memory / policy** | `rust/crates/agent24-{store,memory,policy}` | ✅ | 持久化 / 分层记忆 / 权限审批 |
| **agent24-os-proto / os-packages** | `rust/crates/agent24-os-{proto,packages}` | ✅ | 进程外领域 OS：握手/帧协议、挂载、回调通道、包安装（`agent24 os install/uninstall`）。Sin90 不再编译进内核（T11），是进程外包，仓库 `iDoris-ai/Sin90` |
| **agent24-os-sdk / os-fd** | `rust/crates/agent24-os-{sdk,fd}` | 🟡 原型 0.1.0 | 进程外模块 Rust SDK：events/memory/approval/scheduler/model 五个客户端 + fired；`os-fd` 负责继承监听 socket 的 fd 传递。接口可能变 |
| **附着式模块（ADR-032/A3）** | `rust/crates/agent24-os-proto`（类型）+ `rust/apps/agent24d`（注册/生命周期/反向命令） | ✅ P0–P2 | 用户自行启动的模块（如 AgentEar）经 `agent24 os attach` 注册 token 后连上内核，握手拿 offer set，反向命令走同一条连接；P3（流式）之后再做 |
| **桌面「语音」面板** | `apps/desktop/src/renderer` | ✅ P2 | 展示附着模块（AgentEar）的实时 transcript，`speak`/`stop_playback` 按钮，转写只在内存不落盘 |
| **api-client**（生成物） | `packages/api-client` | ✅ | openapi + events schema → **生成的 TS 协议类型**（只有 type,没有 HTTP/WS client;CI 校验零漂移） |
| **node-daemon**（参考实现） | `packages/node-daemon` | ✅ | v1 协议 mock/参考；TS CapabilityModule 承载 |
| **desktop**（Electron 壳） | `apps/desktop` | ✅ | spawn agent24d + 端口/token/托盘/preload；React UI |
| **agent24-worker → Python ML Worker** | `rust/crates/agent24-worker` | ✅ 契约/客户端 · 🔲 Python 侧 | Rust 侧 wire 契约 + HTTP 客户端（embed/transcribe/health）；Python 服务 `agent24-ml-worker`（Embedding/Whisper/图像/LoRA）规划 |

### 两套扩展机制：领域 OS vs 能力模块

Agent24 有两条互不替代的扩展路径。一句话：**领域 OS 是「换主板」，能力模块是「插一张卡」。**

```
外壳（Pet0 / desktop / 微信 / Nostr / TUI / MCP client）
   │  只经 v1 REST + WebSocket，互不感知实现
   ▼
内核 agent24d（Rust，唯一核心运行时）
   core · agent(Loop) · models · scheduler · store · memory · policy · protocol · mcp
   │                                      │
   │ ① 领域 OS（M-E，重）                  │ ② 能力模块 / 插件（M2 · ADR-016，轻）
   ▼                                      ▼
DomainModule（Rust trait）             CapabilityModule（TS）
 = 这台 agent 是「什么产品」             = 这台 agent「多会一件事」
 sin90 / cos72…                        ping / summarize / codebox…
```

|  | 领域 OS（`DomainModule`） | 能力模块（`CapabilityModule`） |
|---|---|---|
| 回答的问题 | 这台 agent 是**什么产品** | 这台 agent **多会一件事** |
| 数量 | 可同时挂多个（`mount_all` 遍历整个 catalogue；`os.json` 逐模块记 enabled，未列出的模块服从全局 `default`）；**catalogue 由 `agent24 os install <dir>` 安装的进程外包组成，不再随内核编译**（T11 之后 Sin90 也已迁出，仓库 `iDoris-ai/Sin90`） | 可装多个 |
| 语言 / 宿主 | Rust，挂进 `agent24d` | TS，跑在 `packages/node-daemon` |
| 自带存储 | ✅ 内核分配一个**独立数据目录**并在 `open_store(dir)` 里交给模块；**数据库与迁移由模块自己实现**（Sin90 的选择是独立 SQLite `sin90.db`） | ❌ 框架不分配；模块是普通 npm 包，自己建库没人拦（无沙箱、permissions 不参与强制） |
| 记忆分区 | ✅ 共享记忆底座里的私有分区 `(org, os:<name>)`——**经内核交出的 `ScopedMemory` 句柄**访问时模块间不可互读 | ❌ |
| 路由命名空间 | `/api/v1/<name>/*`（由清单 name 派生，模块挂不到自己命名空间外；`RESERVED_KERNEL_SEGMENTS` 拦下撞内核顶级段的名字——同步靠一个扫源码字面量的测试，是检查不是结构保证） | `/api/capabilities/<id>`（惯例） |
| 事件 | `EventBody::Module{module, kind, payload}` | — |
| 隔离 | 进程内（ME-3 后可进程外）；**非沙箱** | **本体也在 node-daemon 进程内，非沙箱**；仅 CodeBox 的代码执行与声明了 `container` 的服务负载走 **BoxLite 微 VM**（Hypervisor.framework / KVM） |
| 分发 | `agent24 os install/uninstall <dir>` 安装/卸载进程外包（原子安装、包根即执行边界；**下次 daemon 启动生效**）；`agent24 os enable`（下次启动生效）/`disable`（进程外模块**热停**：立即拒绝新请求、drain 至多 30s 后停止；仅编译进内核的模块才要等下次启动） | npm registry + 市场浏览 + 安装同意摘要 |
| 清单 | `domain-os.yml` → `DomainOsManifest` | [`protocol/module.schema.json`](protocol/module.schema.json) → `ModuleManifest` |

> 两者的权限词表今天是两套（`module.schema.json` 明确记着「词表统一推迟到 M-E」），这笔债未还。

**隔离是两层，不是二选一**：领域数据的隔离靠**模块自管的独立存储**（内核只分配目录；Sin90 用独立的 `sin90.db`，内核的 `agent24.db` 不认识这些表）；而共享**记忆底座**是所有模块共用的，那里的归属靠 **`(组织, 空间)` 所有权维度**（[ADR-030](docs/decision.md)）——模块拿到的 `ScopedMemory` 句柄被钉死在自己的分区上，键对 org 与 space 都做长度前缀编码。

> **这是句柄的性质，不是沙箱。** 领域 OS 自 T11/ME-3 起是**独立进程**（不再编译进 daemon），但与内核**同 UID、无沙箱**——进程边界挡不住它绕开句柄直接打开内核那个 sqlite 文件，**「经句柄不可跨分区读取」这条保证就不再成立**（`ScopedMemory` 的文档自己就这么写）。独立 `sin90.db`、`(org, space)` 的键编码这些事实不受影响。「读不到用户自己的记忆」还有一个前提——用户 id 不以 `v2\0` 开头；今天成立是因为 daemon 的用户 id 是常量 `local`。
> 句柄本身也很窄：只有 `remember` / `recall` / `recent` 三个方法，**不交出** `EventLog`、`KvStore` 或连接池，也没有 AssertionLedger / ArtifactStore。

### 领域 OS 开发（Rust `DomainModule`，[ADR-029](docs/decision.md)）

> 下面是**接口摘录**，不是可直接编译的示例（省略了 import）。`DomainModule`（编译进内核，in-process）今天只剩测试替身实现（`rust/apps/agent24d/src/domain.rs`），**没有生产级实现** —— Sin90 已随 T11 迁出内核，成为进程外包（仓库 [`iDoris-ai/Sin90`](https://github.com/iDoris-ai/Sin90)）。新领域 OS 应该走进程外路径：用 Rust SDK `agent24-os-sdk`（`rust/crates/agent24-os-sdk`，含 `examples/minimal`）实现，经 `agent24 os install <dir>` 安装，不必再实现这个 in-process trait。

```rust
// rust/crates/agent24-domain/src/lib.rs —— 单向：模块用内核，内核不认识模块
#[async_trait::async_trait]
pub trait DomainModule: Send + Sync {
    fn manifest(&self) -> &DomainOsManifest;              // 清单是模块的唯一身份：
                                                          // 故意没有 name() / event_module()，
                                                          // 免得 trait 方法与已校验的清单不一致
    async fn open_store(&self, dir: &Path) -> Result<()>; // 自己的 DB + 自己的迁移
    fn routes(&self, ctx: Arc<dyn KernelCtx>) -> axum::Router;
                                                          // 相对自己的命名空间（/directions，
                                                          // 不是 /api/v1/sin90/directions）
}

pub trait KernelCtx: Send + Sync {
    fn events(&self) -> Option<&EventSink>;               // None = 没授予 Capability::Events
    fn memory(&self) -> Option<&dyn ScopedMemory>;        // None = 没授予 Capability::Memory
                                                          // 故意不收 scope / grants 参数：
                                                          // 那是调用方可以挪动的边界
    // models() / scheduler() / policy() 在这个 in-process trait 上尚不存在 ——
    // 调度回调（_a24/scheduler/*）与推理回调（_a24/model/complete）
    // 只对进程外模块开放（经 agent24-os-sdk 的 Scheduler/Model 客户端），
    // 不经这个 in-process KernelCtx；in-process 路径本身也已没有生产实现（见上）
}
```

> `KERNEL_GRANTS = {Events, Memory}` 是内核**最多愿意给**的（`rust/apps/agent24d/src/domain.rs`）。实际拿到多少还要看：模块自己在清单里请求了什么（`Grants::granting` 取 `requested ∩ willing`，**多要无益，不要也不会白给**），以及 memory 分区登记是否成功（失败则不给）。

### 能力模块开发（TS CapabilityModule，由 `node-daemon` 承载）

> 今天**没有对外发布的 SDK**：`CapabilityModule` 只是 `packages/node-daemon` 的内部接口，那个包是 private、只有 `main: dist/server.js`、不导出类型入口——**第三方模块 import 不到它**，只能按下面的结构实现（仓内模块按自己所在位置相对引用 `base`——放在 `src/capabilities/` 下就是 `./base`）。
> 加载器 `require()` 包的入口后要求**包根直接带有 `manifest` 与 `register`**，所以别只 `export const myModule`：那样导出的是 `{ myModule }`，会被拒绝。

```ts
// 仓内（此文件放在 packages/node-daemon/src/capabilities/ 下）：
//   import type { CapabilityModule } from './base'
// 仓外：没有可 import 的类型，按结构实现即可
const myModule = {
  manifest: {                       // 清单是必需的；没有顶层 id 字段
    id: 'my-capability',
    version: '0.1.0',
    name: 'My Capability',
    description: '…',
    type: 'headless',               // ui | headless | hybrid
    permissions: [],
  },
  register(router, ctx) {
    // handler 收 RouteContext（params/query/body）并 return 结果，不是 (req, res)
    router.get('/api/capabilities/my-capability', (rctx) => ({ ok: true }))
    // ctx.llm 是注入的 LLM Gateway
  },
}

module.exports = myModule   // 包根必须直接是 { manifest, register }
```

> `/api/capabilities/<id>` 是**惯例不是强制**——router 接受任意 path（CodeBox 就挂在 `/api/codebox/*`）。

### LLM 运行时

> ⚠️ 下表是**目标形态**。今天 daemon 启动时按环境变量（`OMLX_URL` / `OMLX_API_KEY` / `DEFAULT_MODEL`）只构造 oMLX + Ollama 两个槽，tier 按 URL 判定（非 loopback 的 `OMLX_URL` → `Remote`）；**没有独立的 remote / lora 注册入口，也没有运行时的设置页切换。**

| 运行时 | 端点 | 说明 |
|--------|------|------|
| **oMLX**（默认） | `localhost:8088/v1` | Apple Silicon 原生，最低延迟 |
| Ollama | `localhost:11434` | 跨平台，模型丰富 |
| LM Studio | `localhost:1234/v1` | 图形界面管理 |
| Remote API | 自定义 | OpenAI 兼容接口 |

---

## CLI 快速开始（Rust daemon，M-B 起）

```bash
# 构建
cd rust && cargo build -p agent24d -p agent24-cli

# 常驻模式：启动 daemon（~/.agent24/daemon.json 供发现）
./target/debug/agent24 daemon start
./target/debug/agent24 daemon status     # running · pid … · backend rust
./target/debug/agent24 models            # 需本地 oMLX(8088)/Ollama(11434)
./target/debug/agent24 chat "你好"        # attached：连上已运行的 daemon
./target/debug/agent24 daemon stop

# 无 daemon 时直接 chat：自动拉起临时 daemon，用完即走
./target/debug/agent24 chat "hi"
```

端到端冒烟：`scripts/cli-smoke.sh`。Electron 壳切换 Rust 后端：`AGENT24_BACKEND=rust pnpm dev`。

## 二次开发者接口（现在就有的）

**真源在哪，说清楚**

> - **REST**：`protocol/openapi.yaml` 是**手写**的真源。
> - **WS 事件**：真源是 Rust 类型 `agent24-protocol`，`events.schema.json` 由 `cargo run -p agent24-protocol --bin export-schema` **生成**。
> - **CI 有两道零漂移门**：① Rust event 类型 → `events.schema.json`（`export-schema` 输出与文件 diff）；② `protocol/` → `packages/api-client` 生成物。另有 openapi 的 redocly lint。
> - 它们**都不**校验 daemon 的实际路由与 yaml 一致——今天 `/api/v1/os`、`/api/v1/os/{name}` 就在 daemon 上而不在 yaml 里。

| 契约 | 位置 | 说明 |
|---|---|---|
| v1 REST | [`protocol/openapi.yaml`](protocol/openapi.yaml) | health · chat · models · usage · sessions · runs · approvals · schedules · shutdown · tools · standing-grants · tool-overrides · sin90 的 7 条（**未随 T11 更新**：这 7 条只在装了进程外 Sin90 包时经内核代理出现，且 `/api/v1/os*`、`/api/v1/attached*`、`/commands/*`、`usage?module=` 这些新端点也还没收进 openapi.yaml，是已知的规格漂移） |
| WS 事件 | [`protocol/events.schema.json`](protocol/events.schema.json) | 含 `ModuleEventPayload{module, kind, payload}` —— 领域模块触达事件流的唯一一条缝 |
| 插件清单 | [`protocol/module.schema.json`](protocol/module.schema.json) | `ModuleManifest` |
| 生成物 | `packages/api-client` · `packages/contract-tests` | 生成的 TS 协议**类型**（不含运行时 client）；契约测试任何实现都能拿去跑 |

**运维 / 集成面**

```bash
agent24 daemon start|status|stop      # 进程管理（~/.agent24/daemon.json 供发现）
agent24 service install|uninstall|status   # macOS LaunchAgent：登录自启 + 自愈（24/7）
agent24 tui                           # runs / 事件流 / 审批队列
agent24 os list|enable|disable        # 领域 OS 处置（enable 下次启动生效；
                                      # disable 对进程外模块热停，立即生效）
agent24 os install|uninstall <dir>    # 安装/卸载进程外领域 OS 包（下次启动生效）
agent24 os attach add|revoke          # 附着式模块（ADR-032/A3，如 AgentEar）注册/撤销
agent24 mcp                           # 把 agent24d 自己变成 MCP server —— 外部 agent
                                      # 可以把任务跑在你的 agent24 上，风险动作仍在本机审批
```

渠道：微信桥（`packages/wechat-bridge`）、Nostr 桥（`packages/nostr-bridge`，NIP-44 加密，驱动 agent-speaker 二进制，含入站活性探针）。
约定（**是约定，不是运行时强制**）：能力模块应经注入的 LLM Gateway 调模型而不直连外部 API（[ADR-019](docs/decision.md)）。今天 node-daemon 不阻止模块自己 `fetch` 出去——模块是普通 npm 包，`require()` 进来后顶层代码就在宿主进程里跑；清单里的 permissions 目前只做结构校验与安装同意摘要，**不参与 dispatch 时的权限强制**。

---

## 里程碑进度

> 图例：✅ 完成 · 🟡 部分 · 🔲 未开工。**权威状态源是 [`docs/agent/tasks.md`](docs/agent/tasks.md)**（当前在做什么、做到哪一步）；`docs/specs/TASKS.md` 是 M-A~P4 这条更早的里程碑线的历史记录，两者不冲突时互为补充，冲突时以 `docs/agent/tasks.md` 为准。

| 线 | 内容 | 状态 |
|---|---|---|
| **M-A** 契约冻结 | openapi / events / module schema · contract-tests · api-client 生成管道 | ✅ |
| **M-B** Rust 内核 | agent24d · CLI · core / agent / models / scheduler / store / policy | ✅ |
| **M-C** 发布 | v0.1.0 → v0.2.0 → v0.3.0 → **v0.4.0**（当前） | ✅ |
| **M-H** 人机边界 | 审批门 · payload 哈希 · durable resume · plan mode · 安装同意摘要 · Fake 渠道 harness | ✅ |
| **M-D** 记忆重做 | 权威+投影 · 真双时相 · 治理写门 · Condenser · 巩固循环 · FTS/向量缝——**crate 层的库原语与迁移已落地，daemon 尚未端到端消费**（巩固只有调用方驱动的 `run_once` 无后台循环；新 condenser 未接管现有 session 路径；`OmlxEmbedder` 只有 seam 无实现）。逐行状态与判据见 [`SPEC-MD-ME.md`](docs/specs/SPEC-MD-ME.md) §0 上方那张表（**本文不复制**）。今天**没有任何一行是 ✅**：MD-2 🟡（`EventLog` 接了、`ArtifactStore` 没接），其余全 🟢 | 🟡 |
| **M-F** 渠道 | F3 微信 ✅ · F4 Nostr ✅ 桥侧代码与契约完成（含入站活性探针；依赖外部 `agent-speaker` daemon，加密 keystore 无法 headless 解锁——挂账在上游）· F1b 托盘常驻 ✅ · **F5 7×24 泡测 🔲** | 🟡 |
| **M-E → ME-3** 进程外领域 OS | `DomainModule`+`KernelCtx`（in-process，已无生产实现）✅ · 配置注册表 + `os` CLI ✅ · **进程外 Provider（挂载/代理/回调通道/包安装）✅ 已交付**（ADR-031，T11 已把 Sin90 迁出内核） | ✅ |
| **ME-4** 内核回调面 + SDK + AgentEar 附着 | 调度回调 `_a24/scheduler/*` ✅ · 推理回调 `_a24/model/complete` ✅ · SDK 原型 `agent24-os-sdk` 0.1.0 ✅ · ADR-032/A3 AgentEar 附着 P0–P2 ✅（插队线，见 `tasks.md`）· Cos72 骨架/第二个进程外领域 OS 🔲（`PLAN-ME4-OS-CAPABILITIES.md` ME4-5.3.x，v0.5.0 范围） | 🟡 |
| **P4** 生态 / 分发 | 模块市场后端 ✅（npm 发现 + 浏览过滤）· 跨用户分发 / 模块签名 / 跨设备记忆同步 🔲 | 🟡 |

**当前唯一的物理阻塞**：F5 —— 需要 Mac mini + 微信扫码 + Nostr identity 连跑 7 天。启动 F5 所需的仓内代码已具备（这是一句限定，不是「全仓无待办」）。
**下一步**：v0.4.0 发布 → ME4-5.2.0/5.2.1（Sin90 迁到 SDK）→ Cos72 最小闭环（mytask）→ v0.5.0（ME-3 完整 + 回调 + SDK + Sin90/Cos72 独立可装）。详见 [`docs/agent/PLAN-ME4-OS-CAPABILITIES.md`](docs/agent/PLAN-ME4-OS-CAPABILITIES.md)。

**里程碑门**：进入 P4（跨用户分发、模块签名 + 信任根）与发布 tag/Release 需用户确认，不擅自跨。

---

## 文档

- [工作站规划](docs/WORKSTATION_PLAN.md) — oMLX API 调研、64GB Mac 模型清单、能力 TODO
- [决策日志](docs/decision.md) — ADR-001 ~ ADR-030
- [实现蓝图](docs/specs/SPEC-MD-ME.md) — M-D 记忆 + M-E 领域 OS · [任务队列](docs/specs/TASKS.md) — loop 的工作来源（**M-D 的状态源是 `SPEC-MD-ME.md` §3，不是这里**）

## 参考实现

`vendor/xiaoheishu` 是 [MushroomDAO/Xiaoheishu](https://github.com/MushroomDAO/Xiaoheishu) 作为参考引入的 submodule，提供成熟的 Electron + Vite + React 基础。框架演进后，小黑书等应用将从本仓库 fork，只维护自身能力模块。

## License

This project is licensed under the [Apache License, Version 2.0](LICENSE).  
Copyright 2024-present MushroomDAO Contributors.  
See [NOTICE](./NOTICE) · [TRADEMARK.md](./TRADEMARK.md) · [LICENSE-zh.md](./LICENSE-zh.md) · [TRADEMARK-zh.md](./TRADEMARK-zh.md) for details.
