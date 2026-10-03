# Agent24 架构分层总览

> 写于 2026-10-03，对应 main `122fff7`（M10 Open Design 集成落地 #660、设计文档补落 #662 之后）。
> 本文是**导航性总览**：解释各层是什么、彼此什么关系、将来扩展会互相影响到哪里。
> 每一层的权威细节在文中链接的 ADR / spec / 设计文档里；两者冲突时，以那些文档和代码为准，并回来修正本文。

---

## 1. 一张图看全貌

```
┌──────────────────────── L7 外壳（入口）────────────────────────┐
│ apps/desktop（Electron main：BackendManager / SidecarManager / │
│   Creative WebContentsView）  agent24-cli（含 `agent24 acp`）  │
│ packages/*（api-client、nostr-bridge、wechat-bridge 等 TS 侧）  │
└──────────────┬───────────────────────────────┬─────────────────┘
               │ REST / WS + 凭据（bearer 或 capability token）│ spawn / 托管
┌──────────────▼──────────── L6 扩展面（四类"外挂"）──────────────┐
│ ① 进程内 DomainModule（可信代码，编进 daemon）                   │
│ ② 进程外领域 OS（ME-3：Sin90 个人 / Cos72 社区；os-proto+os-sdk）│
│ ③ 附着模块（A3：AgentEar；自己启动，长期 token）                 │
│ ④ Sidecar 集成（Open Design：sidecar-host + ACP 桥 + 受限令牌） │
│ （另：MCP server 的工具 → 包装成普通 Tool 走同一条流水线）       │
└──────────────┬──────────────────────────────────────────────────┘
┌──────────────▼──────── L5 组合根 / 授权中心：agent24d ──────────┐
│ 路由、capability 签发/吊销（ProductHost / CreativeRuntime）、     │
│ 领域 OS 挂载与监管、附着注册表、审批/调度/推理/记忆回调           │
└──────────────┬──────────────────────────────────────────────────┘
┌──────────────▼──────── L4 运行时内核：agent24-agent ────────────┐
│ run 生命周期 + agent loop（一等公民的取消、事件、持久化）         │
└──────────────┬──────────────────────────────────────────────────┘
┌──────────────▼──────── L3 内核能力服务 ─────────────────────────┐
│ models（模型网关） tools（工具流水线） policy（fail-closed 审批）│
│ memory（M-D） scheduler（调度） mcp（外部工具） comm（Hyphae）   │
└──────────────┬──────────────────────────────────────────────────┘
┌──────────────▼──────── L2 权威与持久化 ─────────────────────────┐
│ store：SQLite + 哈希链审计 + workspace 注册表/租约/分配/保留/恢复 │
│ workspace：WorkspaceService / WorkspaceHandle（描述符钉住的根）  │
│   └ os-cwd：fchdir 把子进程 cwd 钉在 workspace 内                 │
└──────────────┬──────────────────────────────────────────────────┘
┌──────────────▼──────── L1 纯领域核心：agent24-core ──────────────┐
│ 零框架依赖的状态机（ADR-026）                                     │
└──────────────┬──────────────────────────────────────────────────┘
┌──────────────▼──────── L0 契约层（被所有人依赖，自己不依赖上层）─┐
│ agent24-protocol（v1 wire 类型，锁定到 protocol/ 契约）           │
│ agent24-domain（DomainModule / KernelCtx / Capability）           │
│ agent24-os-proto（内核↔领域 OS）  agent24-sidecar-host-protocol    │
└──────────────────────────────────────────────────────────────────┘
```

**依赖方向严格向下。** 契约层不依赖任何上层；`agent24d` 是唯一把几乎所有 crate 组合在一起的地方（组合根）。

---

## 2. 逐层说明

### L0 契约层

| crate / 目录 | 是什么 | 权威文档 |
|---|---|---|
| `protocol/`（openapi.yaml、events.schema.json、module.schema.json、fixtures） | 机器可读的 v1 对外契约 | [SPEC-002-protocol](specs/SPEC-002-protocol.md) |
| `rust/crates/agent24-protocol` | v1 wire 类型，经 fixture 测试锁定到 `protocol/`；含 workspace 等公开类型 | 同上 |
| `rust/crates/agent24-domain` | 内核↔领域 OS 的契约：`DomainModule`、`KernelCtx`、`Capability`、`DomainOsManifest`、`ScopedMemory` | [decision.md ADR-029](decision.md) |
| `rust/crates/agent24-os-proto` | 进程外模块协议：版本协商、分帧、握手、启动、监管、受约束代理 | [SPEC-ME3-OUT-OF-PROCESS](specs/SPEC-ME3-OUT-OF-PROCESS.md)、[WIRE-OOP-MODULE](specs/WIRE-OOP-MODULE.md) |
| `rust/crates/agent24-sidecar-host-protocol` | 桌面 sidecar 宿主的私有协议（有界 NDJSON、Launch 解码、token 校验与脱敏） | [G8-BOUNDED-LAUNCH-DECODER](open-design-workspace/design/G8-BOUNDED-LAUNCH-DECODER.md) |

要点：`agent24-domain` 刻意**只有类型和 trait**，使依赖箭头只能是「内核 → 契约 ← 领域 OS」。

### L1 纯领域核心 — `agent24-core`

零框架依赖的状态机（run、审批等的合法迁移）。store 落库时强制走这些迁移，保证"数据库里不会出现状态机不允许的状态"。见 [ADR-026](ADR-026-rust-core-polyglot.md)。

### L2 权威与持久化 — `agent24-store`、`agent24-workspace`、`agent24-os-cwd`

- **store**：sqlx + SQLite；哈希链审计日志；以及 M10 带进来的大量 workspace 权威数据——注册表、租约、分配（intent → reservation → materialization → commitment → registration）、保留（retention）、legacy recovery holds、run 的 workspace 准入/终止。
- **workspace**：`WorkspaceService`（`rust/crates/agent24-workspace/src/service.rs`）把持久化的根目录证据和**用文件描述符钉住的真实目录**组合起来，签发进程内的 `WorkspaceHandle`。工具的 `fs_read` / `fs_write` / `shell_exec` 都经它解析路径，并在每次需要时重新校验 run / 租约 / workspace 事实，而不是把一次 pin 当永久权限。
- **os-cwd**：唯一的 unsafe 边界，只在 fork 之后、exec 之前做 `fchdir(2)`，让子进程的 cwd 就是那个被钉住的目录。

权威：[ADR-002 Workspace 契约](open-design-workspace/design/ADR-002-WORKSPACE-CONTRACT.md)、[ADR-006 Legacy recovery holds](open-design-workspace/design/ADR-006-LEGACY-RECOVERY-HOLDS.md)、[G1/G2 scratch service plan](open-design-workspace/design/G1-G2-SCRATCH-SERVICE-PLAN.md)。

> 现状：v1 只有 `kind = orchestrator_scratch`、`writeback_policy = external`、`concurrency_policy = serial`、TTL 默认 24h / 最长 7 天。`WorkspaceService` 面向产品的构造按代码注释仍是关闭的，要等宿主准入那一片落地后才开放。

### L3 内核能力服务

| crate | 职责 | 关键约束 |
|---|---|---|
| `agent24-models` | 模型网关：`ModelProvider` trait + OpenAI 兼容适配 + 有序注册表 | 路由/健康逻辑在 trait 之上，不进 provider（ADR-026） |
| `agent24-tools` | 工具 trait + 注册表 + 内建工具 | 固定流水线：normalize → 能力白名单 → 审批门 → 超时执行；fs/shell 经 workspace |
| `agent24-policy` | 审批 broker + 工具门 | fail-closed：超时即拒绝，取消即 aborted，store 行为权威，重复决策 409 |
| `agent24-memory` | M-D 记忆：L0 KV + CanonicalSession | 按文档自陈，agent loop 尚未真正消费 M-D 记忆层（M1 待恢复） |
| `agent24-scheduler` | cron/every/at 调度 | 先推进 next_run_at 再触发；跳过漏掉的 tick；连续失败自动禁用 |
| `agent24-mcp` | 外部 MCP server 适配 | 外部工具包装成普通 `Tool`，**不允许**绕过审批的旁路 |
| `agent24-comm` | Hyphae（Nostr）CLI 调用、keystore、密码存储、REST 路由 | 二进制哈希校验、密码只走 stdin（[COMM-HYPHAE](design/COMM-HYPHAE.md)） |

### L4 运行时内核 — `agent24-agent`

run 管理器与 agent loop。每个 run 都有派生自 daemon 关停令牌的取消令牌，所有 await 点可取消；工具调用全部落库、全部事件化，被拒绝的全部审计。依赖 core / models / tools / policy / memory / store / workspace。

### L5 组合根 / 授权中心 — `rust/apps/agent24d`

唯一的进程级组合根，也是**唯一的 authority**：

- **对外 API**：v1 REST + WS（runs、sessions、approvals、workspaces、comm、os、attached…）。
- **能力令牌**（`src/capabilities/`）：`Audience::ProductHost`（全权，只在可信父进程内存里）与 `Audience::CreativeRuntime`（只限一个 workspace / 附着 / 会话、短 TTL、可吊销）。见 [ADR-005](open-design-workspace/design/ADR-005-CAPABILITY-AUTHORITY.md)。
- **领域 OS 挂载**（`src/domain.rs`）：进程内模块的授权上限 `KERNEL_GRANTS` = Events / Memory / Approval；进程外模块 `KERNEL_OOP_GRANTS` 再多 Scheduler / Models。
- **回调面**：审批、调度、推理、记忆回调（ME-4），见 [PLAN-ME4-OS-CAPABILITIES](agent/PLAN-ME4-OS-CAPABILITIES.md)。
- **附着注册表**：A3 模块的监听 socket、握手、代际簿记、反向命令。

### L6 扩展面 — 四类"外挂"

| 类型 | 例子 | 谁启动进程 | 信任假设 | 通道 | 权威文档 |
|---|---|---|---|---|---|
| ① 进程内 DomainModule | （内置领域） | 编进 daemon | **可信代码**；Rust 可见性不是沙箱 | 进程内 trait | [ADR-029](decision.md) |
| ② 进程外领域 OS | Sin90、Cos72 | daemon spawn / 监管 / 停止 | 同 UID、合作但可能有 bug；**不防**敌意本地二进制 | UDS + 一次性回调 socket + 握手 token | [SPEC-ME3](specs/SPEC-ME3-OUT-OF-PROCESS.md)、[ME4-S3 os-sdk](design/ME4-S3-os-sdk.md) |
| ③ 附着模块 | AgentEar | 用户 / launchd / 自己 | 同②，但 token 长期有效、断连不杀 | 常驻 attach socket 上的 JSON-RPC | [A3-ATTACHED-MODULE](design/A3-ATTACHED-MODULE.md)、ADR-032 |
| ④ Sidecar 集成 | Open Design | Electron main（SidecarManager）+ Rust sidecar-host | pin/hash 校验的 bundled sidecar 列入应用 TCB，但**拿不到全权凭据** | ACP（`agent24 acp`）→ REST/WS；受限 capability | [ADR-001](open-design-workspace/design/ADR-001-INTEGRATION-BOUNDARIES.md)、[ADR-003](open-design-workspace/design/ADR-003-ACP-BRIDGE.md)、[ADR-004](open-design-workspace/design/ADR-004-DESKTOP-HOST.md) |

另有 **MCP**：外部 MCP server 的工具进入与内建工具相同的审批流水线，是"工具级"扩展，不是"模块级"扩展。

### L7 外壳

- **apps/desktop（Electron）**：唯一的桌面 main 进程。`BackendManager` 托管 agent24d；`SidecarManager`（通用 sidecar 规格：就绪、健康、重启、关停）托管 Open Design；Creative 页面是隔离 session 的 `WebContentsView`，带同源导航拦截。
- **agent24-cli**：v1 协议客户端；`agent24 acp` 是 Open Design 每个会话拉起的 ACP-over-stdio 桥，只把 ACP 会话/提示映射到现有 run 协议，执行仍在 agent24d。
- **packages/**：TS 侧客户端与桥（api-client、nostr-bridge、wechat-bridge、node-daemon、contract-tests）。

---

## 3. 几个容易混淆的概念

### 3.1 "能力"有两层意思

1. **内核借给模块的能力**：`agent24-domain::Capability`（Events、Models、Scheduler、Policy、Memory、Approval）。模块拿到的是**句柄**，没授权就没有句柄；刻意不提供"先查授权再取句柄"的旁路。
2. **外部客户端能调哪些 API**：agent24d 的 capability token（`Audience`）。比如 Open Design 只拿到 `CreativeRuntime`，只能操作自己那个 workspace 里自己创建的 session / run。

前者管"模块能用哪些内核服务"，后者管"外部进程能调哪些路由"。

### 3.2 内核、OS、Workspace 的关系

- **内核**（agent24d + L1–L4）是 run / 模型 / 工具 / 审批 / 审计 / workspace 的**唯一权威**。
- **领域 OS**（Sin90、Cos72）是"住在内核上的应用"：有自己的存储和路由，通过句柄消费内核能力。
- **Workspace** 是**文件系统权限的唯一来源**：对外只暴露不透明的 `workspace_id`；真实根目录、租约、TTL、清理只在内核内部；工具和第三方都只能在被钉住的根里干活。
- Open Design 走的是**第 ④ 类 sidecar**，不是领域 OS。ADR-001 明确：若将来改成领域 OS 挂载，必须新开 ADR。

### 3.3 "Workspace"这个词的另一个用法

[SPEC-ORG-SPACE](specs/SPEC-ORG-SPACE.md) 里的 Workspace 指"此刻哪些空间在作用域内"的**记忆授权**概念（设计中，未实现），与本文 L2 的文件系统 workspace 不是一回事。

---

## 4. 安全与权限边界（从外到内）

1. **进程边界**：真正不可信的代码只能在独立进程里（②③④），且凭据按受众分层。
2. **凭据边界**：全权凭据（ProductHost）不落盘、不进 discovery 文件；第三方只拿限定 workspace、短 TTL、可吊销的令牌。
3. **审批边界**：所有工具（内建、MCP、模块触发）走同一条 fail-closed 审批流水线；第三方的 "permission response" 不是授权凭据。
4. **文件系统边界**：路径都在描述符钉住的 workspace 根内解析，子进程 cwd 用 `fchdir` 钉住。
5. **法律层**：[docs/laws/](laws/README.md) —— 上下文准入（CONTEXT）、记忆隔离（MEMORY）、审批授权（APPROVAL）。改动涉及哪条法律，要在 PR 模板里写明。

---

## 5. 未来扩展会互相影响到哪里

| 扩展 | 会波及 | 建议 |
|---|---|---|
| 给 `Capability` 加新能力 | 契约层、`KERNEL_*_GRANTS`、os-sdk 客户端、协议版本协商、所有模块类型 | 影响面最大；先写 ADR，再按 ME-4 的切法逐片落地 |
| 新 workspace 类型 / 支持写回源码 | tools、policy（写回需审批）、store 结构、ADR-001/002 | 先定写回审批与并发策略，再开放 `WorkspaceService` 产品面 |
| 接第二个 sidecar 工具（例如另一个 Apache-2.0 创作工具） | 复用 L6④ 全套；但 `CreativeRuntime`、`creative_attachment_id`、`CreativeCapabilityBroker` 是 Creative 专名 | 接之前先泛化成按工具区分的 audience / attachment，否则每个工具复制一套 |
| M1 记忆产品化 | `ScopedMemory`、领域 OS 记忆分区、agent loop、`laws/MEMORY.md` | agent loop 先真正消费 M-D 记忆层，再谈跨 OS 共享 |
| ② 与 ④ 两套扩展模型长期并存 | 生命周期、授权、协议各一套 | 中长期考虑共用一套令牌 + workspace 权限模型 |
| 领域 OS 签名（ME-6，未实现） | os-packages 安装、启动校验 | 当前威胁模型明确不防敌意本地二进制，签名也解决不了这一点 |

---

## 6. 第三方开源工具接入的可复用路径（来自 M10 经验）

1. **边界**：照 [ADR-001](open-design-workspace/design/ADR-001-INTEGRATION-BOUNDARIES.md) 写一份——Agent24 是唯一 AI 控制面，第三方只是工作区，不拿工具权限。
2. **源码**：fork + pin，本仓库只放契约、宿主、桥、pin、集成测试；Apache-2.0 记得保留 NOTICE。
3. **进程**：`SidecarManager`（TS）+ `agent24-sidecar-host`（Rust，Windows Job Object / POSIX 进程组）托管与回收。
4. **运行时协议**：能说 ACP 就复用 `agent24 acp`；否则写一个同样"只做映射、不持有权限"的桥。
5. **权限**：一个 workspace + 一个限定在该 workspace 的短期令牌。
6. **验收与落地**：精确 SHA 的 E2E（参考 `validation/m10-m4-exact` 分支）；锁定 main、打 tag、合并预演、合并后再跑 gate（[M10 落地指南](design/OPEN-DESIGN-M10-MAIN-LANDING-GUIDE.md)）。

**M10 的教训**：integration 分支漂太久（最后 633 个提交、约 200 个叠加 PR）；设计文档没随第一片落地，差点在清理 PR 时丢失。下次按切片尽早落 main，ADR 随第一片进 main。

---

## 7. 尚未完成、不要误以为已经有的

- `WorkspaceService` 产品面未开放；workspace 只有 scratch 一种类型。
- M-D 记忆层尚未被 agent loop 真正消费；M1 暂停中。
- 领域 OS 签名（ME-6）未实现。
- `docs/laws/` 目前只有上下文、记忆、审批三部法条。
- Review 遗留项：[#661](https://github.com/iDoris-ai/Agent24/issues/661)（Creative view）、[#663](https://github.com/iDoris-ai/Agent24/issues/663)（M10 叠加 PR 遗留）。

## 相关

- 内核架构与不可动摇的边界：[agent/architecture.md](agent/architecture.md)
- 执行状态权威：[agent/tasks.md](agent/tasks.md)
- 全部 ADR：[decision.md](decision.md)
- Open Design 集成设计全集：[open-design-workspace/](open-design-workspace/README.md)
