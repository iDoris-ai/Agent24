# ADR-016: Agent24 迁移至消费 @auraaihq/* 包

**状态**: 进行中（migration 分支活跃）
**日期**: 2026-05-15
**作者**: jason / jhfnetboy

---

## 背景

Agent24 自 M1 起在 `src/backend/` 内部自行实现了以下能力：

| 内部文件 | 实际职责 |
|---------|---------|
| `llm-gateway.ts` | AI 提供商路由（oMLX fallback Ollama） |
| `capability-registry.ts` | 模块注册、发现、启用/禁用 |
| `module-installer.ts` | npm install + 动态加载 |
| `module-state.ts` | 模块启停状态持久化（手写 JSON） |
| `server.ts` | HTTP 路由框架 |
| `src/shared/ipc-types.ts` | ModuleManifest 类型定义 |

与此同时，`auraai-packages` monorepo（发布 `@auraaihq/*` 包）正在构建相同的能力层，但 Agent24 对其零依赖。这形成了**职责重叠**，且每次升级需要在两个地方分别维护。

---

## 决策

**Agent24 改为消费 `@auraaihq/*` 包**，自身只保留 Electron 框架层（主进程、IPC、BoxLite 容器代理、后端子进程管理）。

---

## 目标架构

```
Agent24（Electron 框架壳）
  ├── Electron 主进程 / IPC
  ├── BoxLite 容器代理
  └── 后端子进程管理
          ↓ import
@auraaihq/core      — ModuleLoader, ModuleRegistry, runModule()
@auraaihq/idoris    — AI 网关（adapter + 智能路由 + 隐私层）
@auraaihq/memory    — L0 SQLite K-V 存储
@auraaihq/sdk       — 类型定义（ModuleManifest, Module, Intent, Result...）
```

---

## 统一调用约定

所有能力模块统一使用 `Module` 接口（`@auraaihq/sdk`）：

```typescript
// Agent24 内核调用（完整生命周期管理）
await kernel.invoke('publish-twitter', { kind: 'publish', payload: { content: '...' } })

// 第三方 / 独立调用（不需要内核）
import { runModule } from '@auraaihq/core'
import twitterModule from '@auraaihq/publish-twitter'
const result = await runModule(twitterModule, intent, { ai: myAI, memory: myStore })
```

**不再提供"直接 import 纯函数"路径**，统一走 `Module.invoke()` + `runModule()`，未来可无缝升级为 MCP Server。

---

## iDoris AI 网关层

`@auraaihq/idoris` 是 AI 调用的统一入口，替代原 `llm-gateway.ts`：

```
能力模块（pdf-ocr, publish-*, pptx-gen）
        ↓ ctx.ai.complete()
@auraaihq/idoris
  ├── Adapter 协议（接口定义）
  ├── 内置 adapter: oMLX, Ollama, Claude API, OpenAI...
  ├── 智能路由：本地优先 → 远程 fallback
  ├── 隐私层：PII 脱敏、元数据剥离（M2 实装，M1 预留接口）
  └── 用量统计
        ↓
BoxLite（本地模型容器）  /  远程 HTTP API
```

---

## 迁移路径（18 个 PR，三阶段）

### 阶段 1A：auraai-packages 补足实现（串行）

| PR | 内容 | 状态 |
|----|------|------|
| PR1 | `@auraaihq/sdk` 扩充 ModuleManifest（type/navItem/models/container） | ✅ 完成 (#12) |
| PR2 | `@auraaihq/idoris` 实装（adapter + 路由 + 隐私接口预留） | 🔄 进行中 |
| PR3 | `@auraaihq/core` 实装（ModuleLoader + ModuleRegistry + runModule） | ⏳ 待开始 |
| PR4 | `@auraaihq/boxlite-client`（通用 BoxLite 客户端抽出） | ⏳ 待开始 |

### 阶段 1B：packages 现有插件对齐（可并行）

| PR | 内容 | 状态 |
|----|------|------|
| PR5 | `publish-twitter` 对齐 Module 接口 | ⏳ 待开始 |
| PR6 | `publish-telegram` 对齐 Module 接口 | ⏳ 待开始 |

### 阶段 1C：Agent24 改用 packages（串行）

每个 PR 完成后须 100% 集成测试通过再进行下一步。

| PR | 内容 | 替换目标 |
|----|------|---------|
| PR7 | 接入 `@auraaihq/idoris` | `src/backend/llm-gateway.ts` |
| PR8 | 接入 `@auraaihq/core` | `capability-registry.ts` + `module-installer.ts` |
| PR9 | 接入 `@auraaihq/memory` | `module-state.ts` |
| PR10 | 接入 `@auraaihq/boxlite-client` | `boxlite-service.ts` |
| PR11 | 集成 publish-twitter/telegram 示例模块 | — |
| PR12 | 清理所有已迁移的重复代码 | — |

### 阶段 2：第三方调用路径（`runModule` 验证）
### 阶段 3：MCP Server 暴露（`@auraaihq/mcp-bridge`）

---

## 类型对齐说明

`@auraaihq/sdk` 的 `ModuleManifest` 是 Agent24 的 `src/shared/ipc-types.ts::ModuleManifest` 的超集。
阶段 1C 完成后，Agent24 应直接 `import { ModuleManifest } from '@auraaihq/sdk'` 并删除本地定义。

**Permission 类型差异**（待后续 PR 对齐）：

| `@auraaihq/sdk` Permission | Agent24 Permission |
|---------------------------|-------------------|
| `'fs:read'`, `'fs:write'` | `'filesystem'` |
| `'net'` | `'network'` |
| `'ai'` | `'llm'` |
| `'memory:read'`, `'memory:write'` | `'memory'` |
| — | `'wechat'`, `'nostr'` |

---

## 分支策略

- `auraai-packages`: `migration/packages-extraction` → 各 PRn 子分支 → 合并回 migration → 最终合并 main
- `Agent24`: `migration/consume-packages` → 各 PRn 子分支 → 合并回 migration → 最终合并 main
- 历史快照 tag: `pre-migration-2026-05-15`（两个 repo 均已打）

---

## 参考

- auraai-packages PR #12: feat(sdk) ModuleManifest 扩充
- `src/shared/ipc-types.ts`: Agent24 当前的 ModuleManifest 定义
- Brood: `orgs/auraai/INTERFACES.md` — iDoris.ai 对外接口契约
