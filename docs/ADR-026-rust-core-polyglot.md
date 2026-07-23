# ADR-026: Agent24 采用 Rust Core + Polyglot Worker 架构（草案）

**状态**: 🟡 提议（草案，待 jason 确认后转 ✅ 采纳）
**日期**: 2026-07-23
**作者**: jason / Claude Code
**取代**: ADR-023（后端切 Python FastAPI）→ 标记 Superseded；ADR-024 部分 Superseded
**修订**: ADR-016（消费 @auraaihq/* 包）→ 范围收缩，见 §6

---

## 背景

两条外部输入触发本次重新评估：

1. **Rust Agent 框架生态评估**：Rig（MIT，模型适配 SDK）、Goose（Apache-2.0，完整通用 Agent）、VT Code / Codex CLI（Coding Agent 参考）、zerostack（GPL-3.0）、pi_agent_rust（0.1.x，License 带 Rider）。
2. **针对本仓库的架构评估**：结论为「Rust 内核 + TS 外壳融合」优于「继续全 TS」和「全量切 Rust」。

同时重申产品定位（不变）：

> **Agent24 是 24/7 常驻的个人/社区/组织/城市日常工作流 Agent，不是 Coding Agent。**
> 核心资产：调度器、工作流、分层记忆、渠道接入（微信/Nostr）、权限审计、AgentStore/PGL 承载。
> 模型供给是组合式：本地 3B/7B 小模型完成基础动作 + 复杂任务调外部 API + 自训 LoRA。

### 现状盘点（关键事实）

当前仓库已验证的：Electron 壳 + BackendManager（fork/健康检查/自动重启）+ Node daemon :8765 + LLM Gateway（oMLX→Ollama fallback）+ CapabilityModule 注册/安装 + BoxLite 服务容器代理。

**尚不存在的（全部 planned）**：Agent Loop、任务分解/状态机、调度器、分层记忆、上下文压缩、权限审批、工作流引擎、多 Agent、自进化。

> **核心洞察：Agent24 目前有"容器"，没有"agent"。本 ADR 决定的不是"重写现有内核用什么语言"，而是"尚未编写的内核从第一行起写在哪里"。**

---

## 决策

### 三层架构（终态）

```
Agent24 Desktop  = Electron + React 产品壳（保留，不重写）
Agent24 Core     = Rust Agent 内核（agent24d daemon + agent24-cli）
Agent24 Workers  = Python ML Worker（MLX serving / LoRA 训练 / ComfyUI）
                 + Node Host（现有 TS CapabilityModule 兼容层）
```

### 九条核心决议

1. **Rust 是 Agent24 唯一的核心运行时**。新的 Agent Loop、Memory、Workflow、Scheduler、Permission、CLI 从第一行起写在 Rust，不再沉淀进 Node 或 Python 主后端。
2. `agent24d` 提供 **REST + WebSocket**（不用 N-API 作第一版连接方式）：命令/资源走 REST，流式事件走 WS。
3. `agent24-cli` 与 daemon 共享 Rust core，支持 Attached（连已运行 daemon）和 Standalone 两种模式。
4. **Electron + React 继续作为正式桌面外壳**，Main 进程职责收缩为：spawn agent24d、管理动态端口+token、托盘/窗口/Keychain/自动更新/preload 桥。
5. TypeScript 模块通过 **Node Host（JSON-RPC/MCP）** 接入内核，不由 Rust 直接加载 npm 模块；UI Module 继续用 React。
6. **Python 仅用于 ML Worker**（MLX 加载、Embedding、Whisper、图像生成、LoRA 训练），不承担会话状态/权限/任务持久化/CLI/审计。
7. 所有跨语言接口由 **OpenAPI / JSON Schema** 单一来源定义，TS SDK 自动生成（openapi-typescript/orval），CI 校验生成物一致性。
8. **内核不依赖任何 Agent Framework**：Rig 仅作为 `ModelProvider` trait 的一个 adapter 实现（Rig API 不稳定，官方自认仍有 breaking changes）；不 fork Goose（太重）；不引入 zerostack 代码（GPL-3.0）。
9. Tauri 迁移**不设为当前目标**，但 REST+WS 边界天然为其留门。
10. **不做任何"临时 TS 版"内核能力**（调度器、Agent Loop 等一律不在 Node 里先写一遍）；过渡期由「现 Node daemon 改造成 v1 协议的 mock/参考实现」保障日常开发不阻塞，Rust 内核就绪后按路由逐步替换。
11. **提供 TUI**（`agent24 tui`，ratatui 实现）：定位是 headless 部署（Mac mini / 社区 Linux 服务器 / 城市节点）经 SSH 的运维与审批界面，**只做协议薄客户端**（chat + runs 监控 + approval + 日志），严格走 v1 REST/WS，不复制桌面端功能。它同时是协议完备性的试金石——TUI 能纯靠 API 做出来，协议才算闭环。

### 显式否决

- ❌ ADR-023 的「M3 切 Python FastAPI 主后端」——避免 Node→Python→Rust 双迁移。
- ❌ 全量 Rust 重写（重写 React UI / Electron 生命周期 / Playwright 生态无收益）。
- ❌ 先在 TS 里写 Agent Loop 再迁 Rust——这是双迁移的变体。
- ❌ Fork Goose / VT Code / Codex——产品是日常工作流 agent，不需要代码理解/沙箱编辑内核；仅作架构参考（Codex 学权限审批，Goose 学 MCP 扩展体系，Pi 学极简 Loop）。

---

## 1. Rust 内核结构

```
rust/
├── crates/
│   ├── agent24-core/       # 稳定领域模型：Session/Message/Run/Task/Step/
│   │                       # ToolCall/Approval/MemoryRecord/Event/Usage
│   │                       # 零外部框架依赖（不依赖 Rig/Axum/Electron/厂商 SDK）
│   ├── agent24-agent/      # Agent Loop：加载上下文→调模型→解析 ToolCall→
│   │                       # 权限判断→执行→追加结果→继续/完成
│   ├── agent24-models/     # Model Gateway + 三层路由（见 §4）
│   ├── agent24-scheduler/  # ★ cron 式日常工作流调度器（本产品的灵魂）
│   ├── agent24-workflow/   # Step 状态机、checkpoint/resume（承接 ADR-024 设计）
│   ├── agent24-memory/     # L0 KV → 分层记忆、上下文压缩
│   ├── agent24-policy/     # 权限（fs/shell/net/mcp）、用户审批、审计、成本限额
│   ├── agent24-tools/      # 内置工具执行
│   ├── agent24-mcp/        # rmcp（官方 MCP Rust SDK）client/server
│   ├── agent24-protocol/   # serde 类型 + schemars → OpenAPI 生成源
│   └── agent24-store/      # SQLite (sqlx)
└── apps/
    ├── agent24d/           # daemon: serve/doctor/migrate/models
    └── agent24-cli/        # agent24 chat / run / tools / sessions / daemon
                            # + `agent24 tui`（ratatui，headless 运维/审批薄客户端）
```

技术选型：tokio + axum + sqlx + serde + schemars + utoipa + rmcp（+ ratatui for TUI）。

### 参考仓库（本地只读，git-ignored，不作 submodule 提交）

克隆于 `vendor/reference/`（已加入 .gitignore），供开发时对照借鉴，**GPL 仓库只读思路、禁止复制代码**：

| 仓库 | License | 借鉴什么 |
|---|---|---|
| 0xPlaygrounds/rig | MIT | ModelProvider adapter 实现、多厂商接口 |
| aaif-goose/goose | Apache-2.0 | MCP 扩展体系、通用 agent 执行架构 |
| vinhnx/vtcode | MIT/Apache-2.0 | Rust crate 模块化拆分、沙箱 |
| openai/codex | Apache-2.0 | 权限审批模型、流式事件、TUI 架构 |
| gi-dellav/zerostack | **GPL-3.0 ⚠️** | 极简 Loop / 上下文压缩 / 会话恢复（只读思路） |
| Dicklesworthstone/pi_agent_rust | MIT+Rider ⚠️ | Pi 式结构化并发、任务生命周期（只读思路） |
| earendil-works/pi | MIT | 极简 Agent Harness 设计 |
| RightNow-AI/openfang | Apache-2.0 | "Agent OS" 整体架构（18k★，与本产品定位最近） |
| sigoden/aichat | Apache-2.0 | CLI/REPL/TUI 交互设计、RAG 集成 |
| tensorzero/tensorzero | Apache-2.0 | LLM Gateway + 可观测性 + 实验路由（对标 agent24-models） |
| memvid/memvid | Apache-2.0 | 单文件记忆层设计（对标 agent24-memory） |
| EricLBuehler/mistral.rs | MIT | Rust 原生本地推理（未来可选：内嵌小模型，减少对 oMLX 外部进程依赖） |
| liquidos-ai/AutoAgents | Apache-2.0 | Rust 多 agent 协作框架 |

### ModelProvider trait（防框架锁定）

```rust
trait ModelProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionStream, ModelError>;
}
// 实现：OpenAICompatProvider（oMLX/Ollama/LM Studio）、RemoteApiProvider、
//       RigAdapter（可选）、MockProvider（测试）
```

## 2. Daemon API（v1）

> 示意性草图，非权威——端点与事件的权威定义以 `docs/specs/SPEC-002-protocol.md` 与 `protocol/` 为准。

```
GET    /health
POST   /api/v1/sessions
POST   /api/v1/runs                  # 发起一次 agent 执行
POST   /api/v1/runs/{id}/cancel
POST   /api/v1/approvals/{id}        # 用户审批放行
GET    /api/v1/tasks/{id}
GET    /api/v1/schedules             # ★ 日常工作流 CRUD
POST   /api/v1/schedules
GET    /api/v1/models
GET    /api/v1/tools
WS     /api/v1/events                # run.started / model.delta / tool.requested /
                                     # approval.required / run.completed / run.failed
```

### 安全模型

- 只绑定 `127.0.0.1`，**动态端口**（`--port 0`），废弃固定 8765。
- 启动时 stdout 输出 `{"type":"ready","port":N,"token":"..."}`，Electron 捕获；每次启动重新生成 token；所有请求带 `Authorization: Bearer`。
- production 禁止无 token API；校验 Origin；后期 macOS/Linux 走 Unix Domain Socket、Windows 走 Named Pipe。
- BackendManager 从 `fork(server.js)` 改为 `spawn(agent24d, ['serve','--port','0'])`，保留健康检查/自动重启逻辑。

## 3. 模块体系拆分

现有 `CapabilityModule.register(router, ctx)` 是进程内回调，无法跨语言。拆为两类：

- **UI Module**（TS/React 继续）：manifest.json + dist/ui.js，只做页面/表单/可视化，调 Agent24 API。
- **Capability Provider**（跨进程协议）：Rust / Node / Python / MCP Server / Container / Remote，统一暴露 `list_tools / call_tool / list_resources / read_resource / health / shutdown`。以 MCP 为基础，外加 Agent24 Manifest 描述权限、UI 入口、模型需求、容器配置、**PGL 签约信息（pgl.yml）**。

**兼容路径（不一次性重写现有模块）**：

```
agent24d (Rust) ──JSON-RPC/MCP──► agent24-node-host ──► 现有 CapabilityModule
                                                      ├─► npm 模块 / Playwright
                                                      └─► Node 专属能力
```

### 与 AgentStore/PGL 的对接（本 repo 特殊定位）

Capability Provider 的 manifest 层预留 `pgl` 字段：签约状态、分账规则（Supplier/Wrapper/Seller）、接入方式。`agent24-policy` 的审计流为未来「使用付费 skill → SuperPaymaster v5 分账触发」和「行为 → 声誉 SBT」提供事件源。「妈妈测试」自动化挂在 Provider 健康检查 + eval 扩展上。

## 4. 模型供给：三层组合（用户核心需求）

```
L1  本地小模型（3B/7B，oMLX 常驻，Qwen3-4B/8B-4bit 级别）
    职责：意图分类、路由决策、摘要、格式化、工具参数填充、日常固定流程步骤
    目标：≥80% 日常调用命中本地，零 token 成本，<1s 首响
L2  远程 API（Claude / OpenAI / DeepSeek，OpenAI-compat 统一接入）
    职责：复杂规划、长上下文、多步推理
    触发：L1 路由判定升级 / 规则强制 / 用户显式指定
L3  领域 LoRA（自训，按工作流域微调）
    训练：Python ML Worker；服务：oMLX 挂 adapter，走 L1 同一调用路径
硬约束（继承 ADR-017）：敏感任务强制 L1/L3，数据不出设备
```

路由表实现于 `agent24-models`，policy 可配置（per-workflow、per-module、per-sensitivity）。

## 5. Python ML Worker 定位

```
agent24d ──► OpenAI-compat HTTP ──► oMLX（serving，现状已如此）
         ──► HTTP ──► ComfyUI
         ──► Worker RPC ──► agent24-ml-worker（Embedding/Whisper/LoRA 训练）
```

现有 LLM Gateway 已把 oMLX 当 OpenAI-compat HTTP 服务调用，证明**无需为调 MLX 而把主后端迁 Python**——ADR-023 的核心动机不成立。

## 6. ADR-016 范围收缩（止损重复投资）

| ADR-016 项 | 与 Rust 内核关系 | 修订 |
|---|---|---|
| `@auraaihq/sdk`（类型） | 冗余——类型真源改为 `protocol/` schema；TS 客户端由 OpenAPI 自动生成（仓库内构建产物，不发布 npm） | 🛑 摒弃：冻结现状不再消费；模块作者改用官方 MCP TS SDK |
| `@auraaihq/idoris`（TS AI 网关） | 冲突——智能路由是内核职责 | ⚠️ 降级为薄 adapter/客户端，不做路由 |
| `@auraaihq/core`（ModuleLoader/Registry） | 冲突——模块生命周期归内核 | 🛑 停止加码；现有代码转为 node-host 基础 |
| `@auraaihq/memory` | memory 终态在 Rust | ⚠️ 仅保留 L0 KV 供 TS 侧使用 |
| `@auraaihq/boxlite-client` | 不冲突 | ✅ 继续 |
| PR7-PR12（Agent24 换用 packages） | 部分失去意义 | 逐项重估，避免"TS 建完 Rust 再建" |

## 6.5 设计硬约束（来自参考仓库研读，详见 `docs/reference-notes/`）

基于 codex-rs（`reference-notes/codex.md`）与 openfang（`reference-notes/openfang.md`）的深度研读，以下写入 M-B/M-C 的**非协商项**：

1. **Cancellation 一等公民**（openfang 最大教训：199K 行没有 CancellationToken，事后补不上）：Agent Loop 从第一行接 `tokio_util::sync::CancellationToken`，每轮迭代、每个 tool、`ModelProvider::stream` 都 `select!` 它。
2. **审批 = 请求-响应，流式 = 单向通知**（codex 协议核心）：WS 上 delta/begin/end 类事件无需回包；`approval.required` 带 id，client 必须回 `POST /api/v1/approvals/{id}`。服务端用 `HashMap<approval_id, oneshot::Sender<Decision>>` per-turn 表挂起执行，**fail-closed**（channel 断开/turn 取消 = Abort），turn 结束统一 drop 所有 sender。
3. **可用决策集数据驱动**：`approval.required` 事件携带 `available_decisions` 数组（approve / approve_for_session / approve_and_remember / deny / abort），UI 只渲染下发列表，不硬编码。
4. **审批与沙箱是两道正交闸门**，编排顺序固定：审批 → 选沙箱 → 尝试 → 失败按策略升级重试（二次审批带 reason，session 内不重复问）。
5. **Guardian 模式对接 L1 小模型**（codex `GuardianAssessment` × 我们的三层模型路由）：24/7 无人值守时用本地 3B 小模型做"自动审批员"，产出 `{risk_level, rationale, status}` 结构化审计记录，低风险自动放行、高风险升级给人。
6. **Tool 用 registry trait，禁止巨型 match**：`trait Tool { definition(); call(ctx,input) }` 注册进 map，MCP/skill/内置工具同接口；dispatch 前流水线固定为 normalize → capability 校验 → approval 门 → 执行。
7. **Cron 调度器**：用 `cron` crate + chrono-tz；到期**立即 pre-advance next_run** 防重复；`MissedTickBehavior::Skip`；连续失败 N 次自动禁用；JSON 原子写持久化。**四类概念命名隔离**：挂钟 cron / per-agent 自主循环 / 事件 trigger / 资源配额，各自独立命名（openfang 把配额器叫 scheduler 是反例）。
8. **WS 协议用强类型 serde tag enum**，禁止手解析 `serde_json::Value`（openfang ws.rs 反例）。
9. **crate 解耦手法**：`agent24-agent`（runtime）不依赖上层 kernel，通过 `KernelHandle` 风格 trait 反向注入（openfang 验证过的模式），保持 `agent24-core` 零框架依赖。
10. **记忆层借鉴**：canonical cross-channel session（超阈值 LLM 摘要压缩而非粗暴截断）+ Merkle 链审计表；存储坚持 sqlx 原生 async（openfang 的 rusqlite+spawn_blocking 是将就）。
11. **审计遥测脱敏**：决策全文落会话录制（可回放）；遥测只记 opaque 决策串 + 命令 hash + 耗时，不含原始内容。

## 7. 实施里程碑

| 里程碑 | 内容 | 完成判据（DoD） |
|---|---|---|
| **M-A 契约冻结 + 仓库重构**（~3-4周） | 本 ADR 定稿；ADR-023 标 Superseded；ADR-016 按 §6 收缩（sdk 摒弃）；建 `protocol/`（openapi.json + events.schema.json + module.schema.json）；仓库重构为目标 monorepo 布局（`apps/desktop` + `packages/node-daemon` + `rust/` 占位）；**Node daemon 加 `/api/v1/*` + WS 事件薄适配层，成为 agent24d 协议的 mock/参考实现**；contract tests 对 mock 全绿 | UI 全部改走 v1 协议；日常开发在 mock 上无阻塞继续 |
| **M-B Rust 最小 daemon**（~4-6周） | Cargo workspace；agent24d 实现 health/chat（先做 oMLX 透传）/events；动态端口+token；BackendManager 加 `AGENT24_BACKEND=node\|rust` 开关；CLI 骨架 | 同一套 contract tests 双后端全绿；Electron UI 零改动可切换 |
| **M-C Agent Loop + 调度** | Session/Run/Task 状态机；ToolCall；取消/恢复；**cron 式工作流调度器**；基础权限审批+审计；SQLite 持久化；`agent24 tui` 最小版（runs 监控 + 审批） | `agent24 run "每天8点抓RSS摘要推送"` 无人值守长期运行；SSH 登录后可用 TUI 审批 |
| **M-D 记忆 + 模型分层** | Memory L0-L1、上下文压缩；三层路由落地；Python Worker 接入（serving 先行，LoRA 训练后置） | 日常流程 ≥80% 调用命中本地小模型 |
| **M-E 模块生态桥接** | node-host 兼容层；UI Module 规范；MCP Provider 接入；PGL manifest 解析钩子 | 现有 5 个示例模块经 node-host 跑通；1 个外部 MCP server 可用 |
| **M-F 24/7 化 + 渠道** | headless 自启、托盘、崩溃自愈；微信（iDoris-SDK）/ Nostr（agent-speaker）渠道；社区部署形态 | 一台 Mac mini 连续 7 天无人干预运行，微信可唤起 |

## 8. 风险与缓解

| 风险 | 缓解 |
|---|---|
| 单人带宽：ADR-016 迁移 + Rust 内核并发推进超载 | M-A 先止损收缩 ADR-016；Rust 侧每里程碑保持最小可交付 |
| Rust 学习曲线拖慢迭代 | M-B 刻意最小化（3 个端点）；核心复杂度后置到 M-C |
| Rig breaking changes | 仅 adapter 层依赖，core 零依赖 Rig |
| 双后端并存期行为漂移 | 同一 contract tests 双跑；漂移即 CI 红 |
| zerostack GPL-3.0 单向不兼容：引入代码将迫使 agent24d 二进制（静态链接=合并作品）整体转 GPL，并传导至下游 fork 应用方与商业定制，破坏 Apache+商业双生模型 | 只读借鉴思路（思路不受版权保护），禁止复制代码 |
| 现有模块生态断裂 | node-host 兼容层保证旧模块零改动可用 |

## 参考

- Rig: github.com/0xPlaygrounds/rig（MIT）
- MCP Rust SDK: github.com/modelcontextprotocol/rust-sdk（rmcp）
- Goose / VT Code / Codex CLI / Pi：仅架构参考
- 本仓库：ADR-016（`docs/ADR-016-consume-auraai-packages.md`）、ADR-017/019/022/023/024（`docs/decision.md`）
- Brood: `protocol/PGL/CONTEXT.md`（AgentStore 承载要求）
