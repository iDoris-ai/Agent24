# Agent24

> 跨平台 Electron 桌面框架——为 Agent24 生态提供统一的"个人 AI 助手"承载壳，支持可插拔能力模块、多 AI 适配、分层记忆、跨 agent 通信。

## 定位

**Agent24 是框架，不是应用。** 我们提供：

- 跨平台分发（macOS / Windows）
- 后台 daemon + 用户交互一致性
- 标准化能力模块接口（`@auraaihq/sdk` `defineModule`）
- AI 适配层（iDoris 主、Claude / OpenAI / 本地 LLaVA 备）
- 分层记忆（L0 KV → L3 ATIF 轨迹 + SkillBank）+ 自进化框架
- 通过 agent-speaker / Nostr 与其他 agent 通信

**应用方**（如小黑书、博客、社区工具等）从本框架 fork，搭载具体场景的能力模块。

> **重命名计划**：M3 末 `AuraAIHQ/Agent24` → `AuraAIHQ/Agent24`（旧 Agent24 仓库届时归档，名字空出来，详见 [ADR-015](docs/decision.md)）。

---

## 架构（M2 — Backend Daemon）

```
┌─────────────────────────────────────────────────────────────────┐
│              Electron Shell（跨平台分发 + UI 一致性）               │
│                                                                 │
│  Main Process (Node.js)              Renderer (React)          │
│  ┌─────────────────────────────┐    ┌────────────────────────┐  │
│  │  BackendManager             │IPC │  Chat  / Workbench     │  │
│  │   └─ fork() backend daemon  │────┤  Models / Settings     │  │
│  │  registerIpcHandlers()      │    │  backendProxy()        │  │
│  │   └─ BackendProxy IPC       │    └────────────────────────┘  │
│  └─────────────────────────────┘                                 │
│                    │ child_process.fork                          │
│                    ▼                                             │
│  ┌─────────────────────────────┐                                 │
│  │  Backend Daemon :8765       │                                 │
│  │  ├─ /health                 │                                 │
│  │  ├─ /api/llm/chat           │                                 │
│  │  ├─ /api/llm/usage          │                                 │
│  │  └─ CapabilityModule routes │                                 │
│  │                             │                                 │
│  │  LLM Gateway                │                                 │
│  │   ├─ oMLX  :8000 (default)  │                                 │
│  │   ├─ Ollama :11434          │                                 │
│  │   ├─ LM Studio              │                                 │
│  │   └─ Remote OpenAI-compat.  │                                 │
│  └─────────────────────────────┘                                 │
└─────────────────────────────────────────────────────────────────┘
```

> **M1 → M3 演进**：M1 用 `@auraaihq/core` 内核（见 PR #3）；M2 用独立 Node.js daemon + LLM Gateway；M3 将迁移到 Python FastAPI（原生 MLX 绑定）。

### 核心组件

| 组件 | 路径 | 状态 | 职责 |
|------|------|------|------|
| **BackendManager** | `src/main/backend-manager.ts` | ✅ M2 | fork/health-check/auto-restart backend daemon |
| **Backend Daemon** | `src/backend/server.ts` | ✅ M2 | Node.js http 服务，聚合路由，不依赖 Electron |
| **LLM Gateway** | `src/backend/llm-gateway.ts` | ✅ M2 | 统一 LLM 调用、token 统计、运行时切换 |
| **CapabilityModule** | `src/backend/capabilities/` | ✅ M2 | 可插拔能力模块接口 `register(router, ctx)` |
| **IPC 桥** | `src/main/ipc/index.ts` | ✅ M2 | `BackendProxy` IPC 转发 + 参数校验 |
| **Preload** | `src/main/preload.ts` | ✅ M2 | `backendProxy()` 暴露给 Renderer |
| **Onboarding Wizard** | `src/main/onboarding/` | 🔲 M2 planned | 硬件检测 → 模型推荐 → 下载引导 |
| **Python FastAPI backend** | `src/backend_py/` | 🔲 M3 planned | 原生 MLX 绑定，替换 Node.js daemon |
| **MemPalace 记忆模块** | `src/backend/memory/` | 🔲 M3 planned | 分层记忆 L0-L3 + SkillBank |

### 能力模块开发

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

## 文档

- [工作站规划](docs/WORKSTATION_PLAN.md) — oMLX API 调研、64GB Mac 模型清单、能力 TODO
- [决策日志](docs/decision.md) — ADR-001 ~ ADR-025

## 参考实现

`vendor/xiaoheishu` 是 [MushroomDAO/Xiaoheishu](https://github.com/MushroomDAO/Xiaoheishu) 作为参考引入的 submodule，提供成熟的 Electron + Vite + React 基础。框架演进后，小黑书等应用将从本仓库 fork，只维护自身能力模块。

## License

This project is licensed under the [Apache License, Version 2.0](LICENSE).  
Copyright 2024-present MushroomDAO Contributors.  
See [NOTICE](./NOTICE) · [TRADEMARK.md](./TRADEMARK.md) · [LICENSE-zh.md](./LICENSE-zh.md) · [TRADEMARK-zh.md](./TRADEMARK-zh.md) for details.
