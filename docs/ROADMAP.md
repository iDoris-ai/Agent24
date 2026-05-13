# Roadmap

> 主里程碑列表见 [PLAN.md 第六节](PLAN.md#六roadmap里程碑)。本文档维护当前进度。
> 所有重大决策见 [decision.md](decision.md)。
> 最后更新：2026-05-12

---

## M0（Bootstrap）— ✅ 完成

- [x] 创建 `AuraAIHQ/Agent24-Desktop` 仓库
- [x] 写产品计划 `docs/PLAN.md`、决策日志 `docs/decision.md`（ADR-001~025）
- [x] 注册 npm scope `@auraaihq`，模块分类与命名规则确认

---

## M1（Desktop 内核 + 第一批模块）— ✅ 完成（已合并 main，2026-05-12）

**核心架构**
- [x] Electron + React + TypeScript 骨架（Vite HMR，CSP 合规）
- [x] IPC bridge：`contextBridge` 暴露 `window.agent24`，typed preload
- [x] 后端 daemon：Node.js HTTP 服务，`127.0.0.1:8765`，零外部依赖
- [x] `BackendManager`：主进程生命周期管理，健康检测 + 自动重启（连续 3 次失败）

**能力模块框架**
- [x] `CapabilityModule` 接口（`base.ts`）：`manifest` + `register(router, ctx)`
- [x] `capability-registry.ts`：静态 MODULES 注册表，`registerAll()` 批量注册
- [x] `LLMGateway`：oMLX（端口 8088）→ Ollama（端口 11434）优先级路由 + failover
- [x] 3 个内置参考模块：`ping`（headless）、`example-summarize`（headless+LLM）、`example-hello`（ui）

**UI**
- [x] 动态 sidebar：从 daemon manifest 自动注入 `ui`/`hybrid` 模块导航项（5 秒轮询）
- [x] Chat、Workbench、Models、Settings 页面骨架
- [x] oMLX 开机自启动检测（`omlxDetect` → `omlxStart` → 轮询）

**质量保障**
- [x] 单元测试覆盖（87%+ 行覆盖率），Vitest + @testing-library/react
- [x] TypeScript 全量 typecheck（renderer + electron 两个 tsconfig）
- [x] PR#8 通过 David review 并合并

---

## M2（系统托盘 + 模块启停 + 管理 UI）— ✅ 完成（已合并 main，2026-05-12）

**系统托盘（PR#9）**
- [x] macOS 系统托盘图标：关闭窗口后 daemon 保持运行
- [x] `isQuitting` flag + `before-quit` 事件：Cmd+Q / 菜单 Quit 正确退出
- [x] `showOrCreateWindow()`：托盘点击、Dock 点击、`activate` 共用同一逻辑，防 destroyed-window crash
- [x] 托盘菜单：「显示 Agent24」「退出」两个菜单项

**模块启停（PR#10）**
- [x] `~/.agent24/module-state.json` 持久化 enable/disable 状态（重启保留）
- [x] `module-state.ts`：`loadState()` 带结构验证（防止损坏 JSON 导致崩溃）
- [x] 后端 API：`POST /api/modules/:id/enable|disable`，带 URI decode 防护 + 404 校验
- [x] IPC handlers：`ModulesEnable`、`ModulesDisable`，带 try/catch 返回 `{ ok: false }`

**模块管理 UI（PR#11/12）**
- [x] `ModulesManager.tsx`：列出所有模块（内置 + 社区），enable/disable toggle
- [x] `loadSeq` ref 防止 stale async 竞态
- [x] 错误 state 显示：toggle 失败时 UI 内联报错

---

## M3（社区模块安装器 + BoxLite 沙箱 + oMLX 模型管理）— 🚧 进行中（PR#13 open）

### 已完成（当前 feat/m3-module-installer 分支）

**社区模块 npm 安装器**
- [x] `src/backend/module-installer.ts`：`installModule()` / `uninstallModule()` / `loadInstalledModule()` / `discoverInstalledModules()`
- [x] 包名合法性校验（`isValidPackageName`，防命令注入）
- [x] 安装失败自动回滚（load 失败 → 自动 npm uninstall，系统状态保持一致）
- [x] 社区模块持久化：安装的模块写入 `~/.agent24/modules/`，重启后自动加载
- [x] `capability-registry.ts` 扩展：`_communityModules[]`、`loadCommunityModules()`、`registerCommunityModule()`、`unregisterCommunityModule()`
- [x] 后端 API：`POST /api/modules/install`（含回滚）、`POST /api/modules/uninstall`
- [x] IPC 类型：`ModulesInstall`、`ModulesUninstall`，`ModuleInstallResult`、`ModuleUninstallResult`
- [x] `ModulesManager.tsx` 新增 npm 安装面板（包名输入 + 安装按钮 + 反馈消息）

**LLM 模型声明（oMLX model management）**
- [x] `ModuleManifest.models?: string[]`：模块可声明所需 LLM 模型
- [x] `LLMGateway.ensureModel(id)` / `ensureModels(ids[])`：注册时自动加载/下载模型（non-blocking）
- [x] oMLX 管理 API 封装：`omlxListModels()` / `omlxLoadModel()` / `omlxDownloadModel()` / `omlxPollDownload()`
- [x] 后端 API：`GET /api/llm/models`（返回 oMLX 实时模型状态列表）
- [x] Models 页面：实时 oMLX 模型状态（加载/未加载状态点）+ 静态推荐目录

**BoxLite Python 沙箱**
- [x] 安装 `@boxlite-ai/boxlite` + `@boxlite-ai/boxlite-darwin-arm64` 原生绑定
- [x] `src/backend/boxlite-host.ts`：懒加载单例，原生绑定不可用时优雅降级（CI 安全）
- [x] `src/backend/capabilities/example-codebox.ts`：hybrid 模块，`POST /api/codebox/run`（每次独立容器）+ `GET /api/codebox/status`
- [x] `src/renderer/pages/CodeSandbox.tsx`：Python 代码编辑器 + 运行按钮 + 输出面板 + BoxLite 可用性状态
- [x] 全部 67 个测试通过，TypeScript typecheck 通过

### 已全部完成（2026-05-12 确认）

- [x] Chat 页面接入真实 LLM（`POST /api/llm/chat`，oMLX → Ollama failover，用户已验证可用）
- [x] IPC bridge：`modules:install` / `modules:uninstall` handlers 在 `ipc/index.ts` + preload 均已实现
- [ ] Workbench 页面功能实现（当前为占位骨架，M4 规划）
- [ ] PR#13 David review → merge

---

## M4（10-12 周）— 自进化 + 共享 + Marketplace

- [ ] 跨用户 skill 共享（用户自愿，匿名 trajectory）
- [ ] Nostr 分发 skill 更新
- [ ] iDoris 主 AI 接入（替换 placeholder）
- [ ] **模块 Marketplace（PLAN 七.6 阶段 4）**
  - [ ] 模块发现服务（Nostr 索引或 npm scope 扫描）
  - [ ] Desktop UI: marketplace 浏览面板（搜索 + 过滤）
  - [ ] 一键 install 流程
  - [ ] 信任分层显示：官方 / 社区 / 第三方 + 权限申请确认

---

## M5（后续）

- [ ] **模块签名 + AirAccount 信任根**（ADR-016 阶段 3）
  - [ ] 模块发布需 sigstore 签名
  - [ ] Marketplace 显示信任级别
  - [ ] 用户可设置"只信任 AirAccount X 签发的模块"
- [ ] 跨设备记忆同步
- [ ] 个人 ↔ 组织 ↔ 公共 三级 agent 网络
- [ ] Tauri 2.0 mobile 端（ADR-018）
- [ ] 拆分时机评估（哪些子目录适合拆出 monorepo）

---

## 拆分决策门槛（参考 ADR-007）

某子目录满足以下任一条件时考虑拆出 monorepo：
- 独立 maintainer 团队
- release cycle 严重不一致（差 10x 以上）
- License 必须不同
- 包数量/issue 量拖累 mono 构建

**底线**：M5 之前不拆，先验证 mono 够不够用。
