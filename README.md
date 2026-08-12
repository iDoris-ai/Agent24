# Agent24

> 面向 24/7 AI agent 的 **shell-agnostic 框架**——Rust 内核 + daemon 提供统一的"个人 AI 助手"承载能力，外壳与核心解耦：自带 Electron 参考外壳，任何外壳都能挂上来（如 Tauri 的 Pet0）；支持可插拔能力模块、本地 & API 多模型适配、分层记忆、跨 agent 通信。

## 定位

**Agent24 是框架，不是应用。** 我们提供：

- **外壳无关（shell-agnostic）**：Rust 内核 + daemon 与前端解耦，壳只经 HTTP/WS 协议连接——自带 Electron 参考外壳，Tauri 外壳（如 Pet0 桌宠）等同样可挂载
- **多端**：桌面已落地（Electron 参考壳，macOS / Windows / Linux 分发）；移动（iOS / Android）与 Web 规划中，同属 shell-agnostic——移动端计划各提供 Tauri 与 Expo / React Native 瘦壳示例（见 [ADR-027](docs/decision.md)）。daemon 与模型可不在端上（如跑在你的 Mac），移动 / Web 端做瘦壳、只经 HTTP/WS 协议远程消费
- 后台 daemon + 用户交互一致性
- 标准化能力模块接口（`@auraaihq/sdk` `defineModule`）
- AI 适配层（本地 & API 多模型：本地小模型 + Claude / OpenAI / iDoris 等 API，可切换）
- 分层记忆（L0 KV → L3 ATIF 轨迹 + SkillBank）+ 自进化框架
- 通过 **Hyphae 菌丝网络**（Nostr）与其他 agent 通信

**应用方**（如小黑书、博客、社区工具等）从本框架 fork，搭载具体场景的能力模块。

> **重命名计划**：M3 末 `AuraAIHQ/Agent24` → `AuraAIHQ/Agent24`（旧 Agent24 仓库届时归档，名字空出来，详见 [ADR-015](docs/decision.md)）。

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
│           models · chat · usage · tools · tool-overrides · sin90 …   │
│  crates: core · agent(Loop) · models(网关+三级路由) · scheduler ·    │
│          store · memory · policy · tools · mcp · protocol · sin90    │
└──────────┬───────────────────────────────────────┬─────────────────┘
     契约：protocol/openapi.yaml + events.schema     │ REST
     （单一来源 → packages/api-client TS SDK 自动生成，CI 校验漂移）
┌──────────▼──────────────────┐        ┌───────────▼──────────────────┐
│ TS 能力模块 / 协议参考实现    │        │ Python ML Worker（规划）       │
│ packages/node-daemon         │        │ Embedding · Whisper ·          │
│ （v1 协议 mock/参考实现，     │        │ 图像 · LoRA 训练                │
│  CapabilityModule 承载）     │        │ （agent24-ml-worker）          │
└──────────────────────────────┘        └────────────────────────────────┘
```

> **为什么不是 Node/Python 主后端**（[ADR-026](docs/ADR-026-rust-core-polyglot.md)，取代 ADR-023 的「M3 切 Python FastAPI」）：新内核能力（Agent Loop / 调度器 / 记忆 / 工作流 / 权限）从第一行起写在 Rust，不在 Node 或 Python 主后端里先写一遍。`packages/node-daemon` 保留为 v1 协议的 **mock/参考实现**（`AGENT24_BACKEND=node` 可切），保障协议演进期日常开发不阻塞；Python **仅**用于 ML Worker（不承担会话/权限/持久化/审计）。

### 核心组件

> 状态图例：✅ 已落地（有测试）· 🟡 部分 · 🔲 未建成。职责列只写已落地能力，目标态见 ADR/ROADMAP。

| 组件 | 路径 | 状态 | 职责 |
|------|------|------|------|
| **agent24d**（Rust daemon） | `rust/apps/agent24d` | ✅ | v1 REST+WS 核心运行时；桌面默认后端 |
| **agent24-cli / TUI** | `rust/apps/agent24-cli` | ✅ CLI · ✅ TUI 最小版 · 🔲 chat | Attached/Standalone；TUI（ratatui）runs/事件流/审批队列，headless 运维 |
| **agent24-core** | `rust/crates/agent24-core` | ✅ | 稳定领域模型（Session/Run/Task/ToolCall/Approval/Event/Usage…），零框架依赖 |
| **agent24-agent** | `rust/crates/agent24-agent` | ✅ | Agent Loop：上下文 → 调模型 → 解析 ToolCall → 权限 → 执行 → 续 |
| **agent24-models** | `rust/crates/agent24-models` | ✅ | Model Gateway + 三级路由（本地小模型 / 远程 API / 自训领域 LoRA；敏感任务强制本地或 LoRA，数据不出设备） |
| **agent24-scheduler** | `rust/crates/agent24-scheduler` | ✅ | cron 式日常工作流调度器 |
| **agent24-store / memory / policy** | `rust/crates/agent24-{store,memory,policy}` | ✅ | 持久化 / 分层记忆 / 权限审批 |
| **agent24-sin90 (+store)** | `rust/crates/agent24-sin90*` | ✅ | 内置 Personal-OS 领域模块（独立 `sin90.db`） |
| **api-client**（生成物） | `packages/api-client` | ✅ | openapi + events schema → TS SDK（CI 校验零漂移） |
| **node-daemon**（参考实现） | `packages/node-daemon` | ✅ | v1 协议 mock/参考；TS CapabilityModule 承载 |
| **desktop**（Electron 壳） | `apps/desktop` | ✅ | spawn agent24d + 端口/token/托盘/preload；React UI |
| **agent24-worker → Python ML Worker** | `rust/crates/agent24-worker` | ✅ 契约/客户端 · 🔲 Python 侧 | Rust 侧 wire 契约 + HTTP 客户端（embed/transcribe/health）；Python 服务 `agent24-ml-worker`（Embedding/Whisper/图像/LoRA）规划 |

### 能力模块开发（TS CapabilityModule，由 `node-daemon` 承载）

```ts
// 实现 CapabilityModule 接口
export const myModule: CapabilityModule = {
  id: 'my-capability',
  register(router, ctx) {
    router.get('/api/capabilities/my-capability', (req, res) => {
      // ctx.llm 可调用 LLM Gateway
      res.end(JSON.stringify({ ok: true }))
    })
  },
}
```

### LLM 运行时（可在设置页切换）

| 运行时 | 端点 | 说明 |
|--------|------|------|
| **oMLX**（默认） | `localhost:8000/v1` | Apple Silicon 原生，最低延迟 |
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

## 文档

- [工作站规划](docs/WORKSTATION_PLAN.md) — oMLX API 调研、64GB Mac 模型清单、能力 TODO
- [决策日志](docs/decision.md) — ADR-001 ~ ADR-027

## 参考实现

`vendor/xiaoheishu` 是 [MushroomDAO/Xiaoheishu](https://github.com/MushroomDAO/Xiaoheishu) 作为参考引入的 submodule，提供成熟的 Electron + Vite + React 基础。框架演进后，小黑书等应用将从本仓库 fork，只维护自身能力模块。

## License

This project is licensed under the [Apache License, Version 2.0](LICENSE).  
Copyright 2024-present MushroomDAO Contributors.  
See [NOTICE](./NOTICE) · [TRADEMARK.md](./TRADEMARK.md) · [LICENSE-zh.md](./LICENSE-zh.md) · [TRADEMARK-zh.md](./TRADEMARK-zh.md) for details.
