# Agent24-Desktop 功能验证手册

> 版本：2026-05-12 | 覆盖里程碑：M1 + M2（已合并 main）、M3（实现中，PR#13 open）
> 运行环境：macOS（Apple Silicon 推荐）、Node.js 20+、pnpm

---

## 前置准备

在执行任何验证步骤前，请确认以下环境就绪：

1. 克隆仓库并安装依赖：
   ```bash
   git clone https://github.com/AuraAIHQ/Agent24-Desktop
   cd Agent24-Desktop
   pnpm install
   ```
2. 编译 Electron 主进程与后端：
   ```bash
   pnpm build:electron   # 或对应 tsc 命令
   ```
3. 启动开发模式（如需 UI 验证）：
   ```bash
   pnpm dev
   ```
4. （可选）如需验证 oMLX 相关功能，需提前安装 `omlx` CLI 并下载至少一个模型。

---

## M1 功能验证清单

### 1. oMLX 开机自启与自动检测

**功能描述**

应用启动时，渲染进程通过 `window.agent24.omlxDetect()` IPC 调用，依次探测
`127.0.0.1:8088`、`127.0.0.1:8000`、`127.0.0.1:8001`、`localhost:8088`
四个候选地址。若探测到运行中的 oMLX 实例则直接使用；若未检测到，则自动调用
`omlxStart(8088, 'xiaobao8088')` 尝试拉起 `omlx serve`，并轮询最多 5 次（每次间隔 2 秒）等待服务就绪。

**涉及文件**

- `src/main/ipc/index.ts`（`OmlxDetect`、`OmlxStart` 处理器）
- `src/renderer/App.tsx`（启动时 `useEffect` 逻辑）
- `src/renderer/pages/Settings.tsx`（`initialize()` 函数）

---

✅ **前置条件**

- oMLX 服务**未运行**（`pkill omlx` 确认）
- 机器已安装 `omlx` CLI（`which omlx` 返回路径）
- `~/.omlx/models/` 下存在至少一个模型文件

📋 **验证步骤**

1. 执行 `pnpm dev` 启动应用
2. 观察顶部导航栏右侧（对话页）的 LLM 标签，初始显示 `Detecting…`
3. 等待约 5–15 秒，观察标签变化
4. 若 oMLX 被自动拉起，打开终端执行 `pgrep -a omlx` 确认进程存在
5. 点击左侧导航栏「设置」，查看「AI Model Service」区域的状态指示灯

✔️ **通过标准**

- 顶栏 LLM 标签最终显示 `<模型名> · oMLX`（如 `Qwen3-8B-4bit · oMLX`）
- 设置页状态指示灯变为绿色，显示 `Connected`，并列出模型名称与 URL（`http://127.0.0.1:8088`）
- 若 oMLX 未预先运行，进程列表中可见 `omlx serve --port 8088 --api-key xiaobao8088`

---

### 2. LLM Gateway（oMLX → Ollama 故障转移）

**功能描述**

后端 daemon 的 `LLMGateway` 实现优先级路由：优先请求 oMLX（端口 8088），若收到
`ECONNREFUSED` 则自动 failover 到 Ollama（端口 11434）。其他错误类型（非连接拒绝）
不触发 failover，直接向调用方抛出异常。

**涉及文件**

- `src/backend/llm-gateway.ts`（`LLMGateway.chat()` 方法）
- `src/backend/server.ts`（`POST /api/llm/chat` 路由）

---

✅ **前置条件**

- 后端 daemon 已运行（可通过 `curl http://127.0.0.1:8765/health` 验证）
- 准备好两种场景测试：a) oMLX 运行中；b) oMLX 停止、Ollama 运行中

📋 **验证步骤**

**场景 A：oMLX 优先**

1. 确认 oMLX 已运行（`curl http://127.0.0.1:8088/v1/models -H 'Authorization: Bearer xiaobao8088'`）
2. 发送 LLM 请求：
   ```bash
   curl -s -X POST http://127.0.0.1:8765/api/llm/chat \
     -H 'Content-Type: application/json' \
     -d '{"messages":[{"role":"user","content":"hi"}]}'
   ```
3. 查看响应和 LLM 使用日志：`curl http://127.0.0.1:8765/api/llm/usage`

**场景 B：oMLX 不可用时 fallover 至 Ollama**

1. 停止 oMLX：`pkill omlx`（等待进程退出）
2. 确认 Ollama 运行中：`curl http://127.0.0.1:11434/api/tags`
3. 重复步骤 2 发送 LLM 请求
4. 查看 usage 日志确认 provider 字段

✔️ **通过标准**

- 场景 A：usage 日志中 `provider` 字段为 `"omlx"`，`message.content` 非空
- 场景 B：请求依然成功返回，usage 日志中 `provider` 字段为 `"ollama"`
- 两种场景下响应体结构均为 `{ "message": { "role": "assistant", "content": "..." } }`

---

### 3. CapabilityModule 接口（UI / Headless / Hybrid 三种类型）

**功能描述**

`CapabilityModule` 接口定义在 `src/backend/capabilities/base.ts`，规定所有能力模块必须实现：
- `manifest`：声明 `id`、`version`、`name`、`type`（`'ui'` | `'headless'` | `'hybrid'`）、`permissions`、可选 `navItem`
- `register(router, ctx)`：向 SimpleRouter 注册 HTTP 路由

`Headless` 模块无 UI，结果通过 API 返回；`UI` 模块声明 `navItem` 后 sidebar 自动注入导航项；`Hybrid` 模块兼具两者。

**涉及文件**

- `src/backend/capabilities/base.ts`（接口定义）
- `src/backend/capability-registry.ts`（模块注册表）
- `src/backend/capabilities/example-ping.ts`（Headless 示例）
- `src/backend/capabilities/example-summarize.ts`（Headless + LLM 权限示例）
- `src/backend/capabilities/example-hello-ui.ts`（UI 模块示例）

---

✅ **前置条件**

- 后端 daemon 已运行（`curl http://127.0.0.1:8765/health` 返回 `{"status":"ok",...}`）

📋 **验证步骤**

1. 获取模块清单：
   ```bash
   curl -s http://127.0.0.1:8765/api/modules | python3 -m json.tool
   ```
2. 确认返回数组包含三个模块：`ping`（headless）、`@auraaihq/example-summarize`（headless）、`@auraaihq/example-hello`（ui）
3. 验证 headless 模块（ping）：
   ```bash
   curl -s http://127.0.0.1:8765/api/capabilities/ping
   ```
4. 验证 UI 模块（hello）的 API 端点：
   ```bash
   curl -s http://127.0.0.1:8765/api/modules/hello/info
   ```
5. 检查 hello 模块的 manifest 包含 `navItem` 字段（路由为 `/modules/hello`）

✔️ **通过标准**

- `/api/modules` 返回包含 3 个模块的数组，每个模块有完整的 `id`、`version`、`name`、`type`、`permissions` 字段
- `ping` 模块响应：`{"status":"ok","moduleId":"ping","ts":<时间戳>}`
- `hello/info` 响应：`{"moduleId":"@auraaihq/example-hello","version":"0.1.0","description":"..."}`
- hello 模块的 `type` 为 `"ui"`，manifest 包含 `navItem: {icon:"👋", label:"Hello", route:"/modules/hello"}`

---

### 4. 动态 Sidebar（从 daemon manifest 注入模块导航）

**功能描述**

`App.tsx` 每 5 秒通过 `window.agent24.modulesList()` 轮询后端 `/api/modules`，将
`type` 为 `'ui'` 或 `'hybrid'` 且声明了 `navItem` 的模块自动注入 sidebar「能力模块」分区。
无需修改任何前端代码，新增模块 manifest 后 UI 自动反映。

**涉及文件**

- `src/renderer/App.tsx`（`moduleNavItems` 过滤逻辑，sidebar 渲染部分）
- `src/main/ipc/index.ts`（`ModulesList` IPC handler，代理到 `/api/modules`）

---

✅ **前置条件**

- 应用已在开发或生产模式下运行
- 后端 daemon 正常运行，`hello-ui` 模块已注册

📋 **验证步骤**

1. 启动应用，观察左侧 sidebar
2. 在固定导航项（对话、工作台、模型、设置）下方，应出现「能力模块」分区标题
3. 分区内应有「👋 Hello」导航按钮
4. 点击「👋 Hello」导航按钮，观察主内容区变化
5. 检查 topbar 标题是否更新为模块名称（`Hello Module`）

✔️ **通过标准**

- sidebar 在「设置」按钮下方出现「能力模块」分区
- 分区内显示「👋 Hello」按钮
- 点击后主内容区渲染 `HelloModule` 组件，显示输入框和「Greet me」按钮
- 若后端未运行，sidebar 显示灰色「➕ 安装模块」占位按钮，而非崩溃

---

### 5. 参考模块：ping / summarize / hello-ui

**功能描述**

三个内置参考模块演示了不同的 `CapabilityModule` 实现模式：

| 模块 | 类型 | 核心演示点 |
|------|------|-----------|
| `ping` | headless | 最简路由注册，无 LLM 依赖 |
| `@auraaihq/example-summarize` | headless | 调用 LLMGateway，`permissions: ['llm']` |
| `@auraaihq/example-hello` | ui | navItem 注入 sidebar + `backendProxy` 跨越 IPC 调用后端 |

---

✅ **前置条件**

- 后端 daemon 已运行
- 验证 hello-ui 时需要 LLM 服务（oMLX 或 Ollama）可用

📋 **验证步骤**

**ping 模块**

```bash
curl -s http://127.0.0.1:8765/api/capabilities/ping
# 预期：{"status":"ok","moduleId":"ping","ts":...}
```

**summarize 模块**（需 LLM 运行）

```bash
curl -s -X POST http://127.0.0.1:8765/api/capabilities/summarize \
  -H 'Content-Type: application/json' \
  -d '{"text":"Electron is a framework for building cross-platform desktop apps with JavaScript."}'
# 预期：{"summary":"..."}
```

**hello-ui 模块（UI 交互）**

1. 在 sidebar 点击「👋 Hello」
2. 在「Your name」输入框输入自己的名字（如 `Jason`）
3. 点击「Greet me」按钮或按 Enter
4. 等待 LLM 响应（按钮显示 `Thinking…`）
5. 点击「Module Info」区域的「Load info」按钮

✔️ **通过标准**

- ping：JSON 响应包含 `"status":"ok"` 和 `"moduleId":"ping"`
- summarize：响应包含非空 `"summary"` 字段，内容与输入文本语义相关
- hello-ui：输入名字后，LLM 返回个性化问候语，显示在输入框下方；「Load info」返回模块 ID 和描述

---

## M2 功能验证清单

### 1. 系统托盘（Tray Icon）— 关闭窗口后 daemon 保持运行

**功能描述**

在 macOS 上，关闭主窗口（点击红色关闭按钮）不会退出应用。`main.ts` 中
`window-all-closed` 事件仅在非 macOS 平台调用 `app.quit()`；macOS 遵循
系统惯例，应用继续在托盘/Dock 中保持运行。后端 `BackendManager` 在
`app.whenReady()` 时启动，在 `app.will-quit` 时停止，因此 daemon 随应用生命
周期而非窗口生命周期运行。

**涉及文件**

- `src/main/main.ts`（`window-all-closed` 事件处理、`BackendManager` 生命周期）
- `src/main/backend-manager.ts`（`start()`、`stop()` 方法）

---

✅ **前置条件**

- 应用以生产模式或开发模式运行（`pnpm dev`）
- macOS 环境

📋 **验证步骤**

1. 启动应用，等待 sidebar 底部后端状态点变为绿色
2. 记录后端 daemon 的进程 PID：
   ```bash
   curl -s http://127.0.0.1:8765/health
   pgrep -a node | grep server.js
   ```
3. 点击主窗口左上角红色关闭按钮关闭窗口
4. 等待 3 秒，再次执行健康检查：
   ```bash
   curl -s http://127.0.0.1:8765/health
   ```
5. 点击 Dock 中的 Agent24 图标重新打开窗口

✔️ **通过标准**

- 关闭窗口后，`curl http://127.0.0.1:8765/health` 依然返回 `{"status":"ok",...}`（daemon 未停止）
- `pgrep node | grep server.js` 仍显示对应进程
- 点击 Dock 图标可重新打开窗口，sidebar 状态点仍为绿色

---

### 2. 模块启用/禁用（Enable/Disable Toggle，状态持久化）

**功能描述**

M2 规划的模块 enable/disable toggle 功能：主进程暴露 `ModuleManagerAPI`
（`list` / `enable` / `disable` / `getDetails`），preload bridge 将其暴露给 renderer，
Renderer 中模块列表面板提供 toggle 开关，状态持久化到
`~/.agent24/module-state.json`。

> **注意**：根据 ROADMAP.md，此功能在当前代码库处于规划/部分实现状态。
> 以下步骤用于验证当前可验证的部分，并标注哪些尚未实现。

**涉及文件**（规划中）

- `src/main/backend-manager.ts`（M2 新增：`ModuleManagerAPI` 接口）
- `src/renderer/pages/`（M2 新增：模块管理 UI 页面）

---

✅ **前置条件**

- 应用运行中
- 后端 daemon 正常，`/api/modules` 返回模块列表

📋 **验证步骤（当前可验证部分）**

1. 通过 IPC 代理调用模块列表（当前通过 `/api/modules` 实现）：
   ```bash
   curl -s http://127.0.0.1:8765/api/modules | python3 -m json.tool
   ```
2. 检查 app UI 是否有「Modules」或「模块管理」导航入口（M2 新增页面）
3. 若导航入口存在，点击进入并查看模块 toggle 状态
4. 切换某个模块的 enable/disable toggle
5. 重启应用，检查 `~/.agent24/module-state.json` 是否保留了状态

✔️ **通过标准**

- `/api/modules` 正确返回所有注册模块（当前基线）
- M2 完成后：toggle off 的模块从 sidebar「能力模块」分区消失
- M2 完成后：重启应用后 toggle 状态与关闭前一致，与 `~/.agent24/module-state.json` 内容匹配

---

### 3. ModulesManager UI 页面

**功能描述**

M2 规划新增独立的模块管理页面，展示所有已注册能力模块的清单，包括：
模块名称、版本、类型（ui/headless/hybrid）、权限列表、enable/disable toggle。
此页面作为固定导航项出现在 sidebar，或通过「工作台」入口访问。

**涉及文件**（规划中）

- `src/renderer/pages/`（M2 新增页面组件）
- `src/renderer/App.tsx`（需在 `BUILTIN_NAV` 添加入口，或通过动态注入）

---

✅ **前置条件**

- 应用运行中
- 后端 daemon 正常

📋 **验证步骤**

1. 检查 sidebar 是否有「模块管理」或类似导航入口（图标建议：🧩 或 📦）
2. 点击进入模块管理页面
3. 确认页面列出所有当前注册的模块（至少 3 个：ping、summarize、hello）
4. 每行应显示：模块名、版本、类型标签、权限 badge、enable/disable toggle
5. 点击「查看详情」或展开某个模块，查看完整 manifest 信息

✔️ **通过标准**

- 模块管理页面正常渲染，无报错
- 列出的模块数量与 `curl http://127.0.0.1:8765/api/modules` 返回数量一致
- 每个模块的类型标签（`ui` / `headless` / `hybrid`）显示正确
- Enable/disable toggle 可操作（M2 完成后状态变化即时反映在 sidebar）

---

### 4. Backend 健康检测与状态显示

**功能描述**

`BackendManager` 每 5 秒对 `http://127.0.0.1:8765/health` 发起健康检查，
连续 3 次失败后自动重启后端进程（`SIGTERM` 旧进程 + `fork` 新进程）。
前端 `App.tsx` 同样每 5 秒调用 `backendProxy({ GET, '/health' })`，将结果
映射到 sidebar 底部的状态指示点（绿色/红色/灰色）。

**涉及文件**

- `src/main/backend-manager.ts`（`tick()` 健康检查 + `spawn()` 自动重启）
- `src/renderer/App.tsx`（`checkBackend` 定时器 + `backendOk` 状态渲染）
- `src/backend/server.ts`（`GET /health` 路由）

---

✅ **前置条件**

- 应用已运行，sidebar 底部状态点为绿色

📋 **验证步骤**

**状态显示验证**

1. 启动应用，确认 sidebar 底部显示「后端服务运行中 :8765」（绿色指示点）
2. 打开 Electron DevTools（`Cmd+Option+I`），查看 Console 是否有 `[backend] listening` 日志
3. 观察 5 秒间隔的健康检查是否正常（网络面板或主进程日志）

**自动重启验证**

1. 获取后端 daemon 进程 PID：
   ```bash
   pgrep -a node | grep server.js
   ```
2. 手动发送 SIGTERM 杀掉 daemon：
   ```bash
   kill -SIGTERM <PID>
   ```
3. 观察 sidebar 状态点：应先变为红色（`后端服务离线`），约 10–20 秒后恢复绿色
4. 查看主进程日志（`/tmp/agent24-main.log` 或 Electron 控制台），确认出现以下日志：
   - `[backend] health check failed (1/3)`
   - `[backend] health check failed (2/3)`
   - `[backend] health check failed (3/3)`
   - `[backend] restarting after consecutive failures`
   - `[backend] spawned pid <新PID>`

✔️ **通过标准**

- 正常状态下：sidebar 底部绿色指示点 + 文字「后端服务运行中 :8765」
- 后端停止后：status 点在下次轮询（最迟 5 秒）内变红，文字变为「后端服务离线」
- 连续 3 次健康检查失败后，`BackendManager` 自动 respawn daemon
- Respawn 后约 10–15 秒内 sidebar 状态点恢复绿色

---

## M3 功能验证清单（实现中，feat/m3-module-installer 分支）

> 以下功能已在 `feat/m3-module-installer` 分支实现，PR#13 open，待 David review 合并到 main。

### 1. 社区模块 npm 安装

**涉及文件**

- `src/backend/module-installer.ts`（`installModule` / `uninstallModule` / `loadInstalledModule` / `discoverInstalledModules`）
- `src/backend/server.ts`（`POST /api/modules/install`、`POST /api/modules/uninstall`）
- `src/renderer/pages/ModulesManager.tsx`（npm 安装面板）

---

✅ **前置条件**

- 后端 daemon 运行中（`curl http://127.0.0.1:8765/health`）
- 互联网可访问 npm registry
- 有一个符合 `CapabilityModule` 接口的 npm 包可供安装测试

📋 **验证步骤**

1. 启动应用，进入「🧩 模块管理」页面
2. 在顶部「安装社区模块」输入框中输入包名（如 `@auraaihq/example-ping` 或自定义包）
3. 点击「安装」按钮，观察安装状态消息
4. 安装完成后：
   ```bash
   curl -s http://127.0.0.1:8765/api/modules | python3 -m json.tool
   # 应包含新模块的 manifest
   ls ~/.agent24/modules/node_modules/
   # 应存在安装的包目录
   ```
5. 若新模块为 `hybrid`/`ui` 类型且有 `navItem`，5 秒内 sidebar 出现新导航项
6. 点击「卸载」按钮，确认模块从列表消失，`~/.agent24/modules/` 对应目录被删除

✔️ **通过标准**

- 安装成功：`/api/modules` 返回新模块，sidebar 即时更新（无需重启）
- 安装失败（包不存在 / 非法包名）：UI 显示错误信息，`~/.agent24/modules/` 无残留
- **安装回滚**：包安装成功但 `loadInstalledModule()` 失败时，自动执行 `uninstallModule()` 回滚，返回 `"Package installed but does not export a valid CapabilityModule — rolled back"`
- 卸载成功：`/api/modules` 不再返回该模块，sidebar 5 秒内消失

---

### 2. oMLX 模型实时状态

**涉及文件**

- `src/backend/llm-gateway.ts`（`omlxListModels()` / `omlxLoadModel()` / `omlxDownloadModel()` / `omlxPollDownload()`）
- `src/backend/server.ts`（`GET /api/llm/models`）
- `src/renderer/pages/Models.tsx`（实时状态展示）

---

📋 **验证步骤**

1. 确认 oMLX 运行中（`curl http://127.0.0.1:8088/admin/api/models -H 'Authorization: Bearer xiaobao8088'`）
2. 进入「🤖 模型管理」页面
3. 页面顶部「oMLX 运行状态」区域应列出所有模型及加载状态
4. 已加载模型显示绿色状态点（`●`），未加载显示灰色
5. oMLX 未运行时，该区域显示「oMLX 未运行 — 启动后此处显示实时模型状态」
6. 直接查询 API：
   ```bash
   curl -s http://127.0.0.1:8765/api/llm/models | python3 -m json.tool
   ```

✔️ **通过标准**

- oMLX 运行时：Models 页面显示模型列表，加载状态与 oMLX 实际状态一致
- oMLX 未运行时：页面正常渲染（静态目录仍显示），无崩溃，无 unhandled promise error
- `GET /api/llm/models` 返回 `OmlxModelEntry[]` 数组，每项含 `id`、`engine`/`status` 字段

---

### 3. BoxLite Python 沙箱

**涉及文件**

- `src/backend/boxlite-host.ts`（懒加载单例，优雅降级）
- `src/backend/capabilities/example-codebox.ts`（hybrid CapabilityModule）
- `src/renderer/pages/CodeSandbox.tsx`（Python 编辑器 UI）

---

✅ **前置条件**

- macOS Apple Silicon（M1/M2/M3）环境
- `@boxlite-ai/boxlite-darwin-arm64` 原生绑定已安装（`pnpm install` 自动完成）
- Docker 或 Hypervisor.framework 可用（BoxLite 依赖）

📋 **验证步骤**

1. 进入「🐍 Python 沙箱」页面（sidebar「能力模块」分区）
2. 观察顶部状态：
   - BoxLite 可用：显示绿色「● BoxLite 就绪」
   - BoxLite 不可用：显示灰色「● BoxLite 不可用」及原因
3. 在代码框中输入 Python 代码：
   ```python
   import sys
   print(f"Python {sys.version}")
   for i in range(5):
       print(f"Line {i}")
   ```
4. 点击「▶ 运行」按钮
5. 等待容器启动（首次约 5-10 秒，后续更快），观察输出区
6. 测试隔离性：运行 `import subprocess; subprocess.run(['rm', '-rf', '/'])` 应被沙箱限制
7. 通过 API 直接测试：
   ```bash
   curl -s -X POST http://127.0.0.1:8765/api/codebox/run \
     -H 'Content-Type: application/json' \
     -d '{"code":"print(2+2)"}'
   ```

✔️ **通过标准**

- BoxLite 可用时：运行结果显示在输出区，内容与 Python 代码预期输出一致
- 每次运行使用独立容器（无跨次状态污染）
- BoxLite 原生绑定不可用（如在 CI 或 x86 环境）：页面正常渲染，运行按钮禁用，显示具体错误信息，不崩溃
- `GET /api/codebox/status` 返回 `{ available: boolean, error?: string }`
- `POST /api/codebox/run { code: "" }`：返回 `400 Bad Request`（code 不能为空）

---

### M3 功能状态汇总

| 功能 | 状态 | 验收方式 |
|------|------|---------|
| 社区模块 npm 安装 | ✅ 已实现 | UI 一键安装，daemon 不重启即生效 |
| 安装失败回滚 | ✅ 已实现 | load 失败自动 uninstall，报告"rolled back" |
| 社区模块持久化加载 | ✅ 已实现 | 重启后 `~/.agent24/modules/` 自动加载 |
| 模块 LLM 模型声明 | ✅ 已实现 | manifest.models → 注册时自动 ensureModel |
| oMLX 模型实时状态 | ✅ 已实现 | GET /api/llm/models + Models 页面 |
| BoxLite Python 沙箱 | ✅ 已实现 | POST /api/codebox/run + CodeSandbox.tsx |
| Chat → 真实 LLM | ✅ 已实现 | POST /api/llm/chat，用户已验证与 Qwen 模型对话正常 |
| IPC bridge install/uninstall | ✅ 已实现 | ipc/index.ts + preload.ts 均已实现 |

---

## 附录：快速诊断命令

```bash
# 检查后端 daemon 健康状态
curl -s http://127.0.0.1:8765/health | python3 -m json.tool

# 列出所有注册模块
curl -s http://127.0.0.1:8765/api/modules | python3 -m json.tool

# 查看 LLM 使用日志
curl -s http://127.0.0.1:8765/api/llm/usage | python3 -m json.tool

# 检查 oMLX 模型列表
curl -s http://127.0.0.1:8088/v1/models \
  -H 'Authorization: Bearer xiaobao8088' | python3 -m json.tool

# 检查 Ollama 模型列表（fallback）
curl -s http://127.0.0.1:11434/api/tags | python3 -m json.tool

# 确认后端进程存在
pgrep -a node | grep server.js

# 查看后端 daemon 的 PID
lsof -i :8765 | grep LISTEN
```
