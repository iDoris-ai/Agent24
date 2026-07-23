# TASKS — 任务队列与状态跟踪

> **这是 loop 的唯一工作来源与状态文件。**
> 状态值：`pending` → `in-progress` → `in-pr(#N)` → `merged` ｜ `blocked(原因)`
> loop 每轮：取「状态 pending 且依赖均为 in-pr/merged」中序号最靠前的任务执行（见 LOOP.md）。
> 每完成一步（提 PR / 收到 merge）由 loop 更新本文件并 commit。
> 最后更新：2026-07-23（A0-A3 全部 merge；A4 in-pr #26）

## 执行顺序总览（最佳路径）

串行主链：`A0 → A1 → A2 → A3 → A4 → A5 → A6 → B1 → B2 → B3 → B4 → B5 → B6 → [v0.1.0-alpha] → C1 → C2 → C3 → C4 → C5 → C6 → C7 → C8 → [v0.1.0 发布]`
（M-D/M-E/M-F 任务在 v0.1.0 后与用户确认优先级再启动，默认顺序见下。）

---

## M-A 契约冻结 + 仓库重构

| ID | 任务 | 依赖 | 状态 | PR |
|---|---|---|---|---|
| A0 | 提交现有设计文档 PR | — | merged | #21 |
| A1 | `protocol/` v1 真源：openapi.yaml | A0 | merged | #23 |
| A2 | `protocol/` 事件与模块 schema | A1 | merged | #24 |
| A3 | contract-tests 包（对现 node daemon） | A1 | merged | #25 |
| A4 | 仓库重构为 pnpm workspace 目标布局 | A3 | in-pr | #26 |
| A5 | node-daemon v1 适配层（mock daemon） | A4 | pending | |
| A6 | api-client 生成管道 + CI 三 job | A5 | pending | |

### A0 提交现有设计文档 PR
- 范围：仅 stage 本次架构工作新增文件——`docs/ADR-026-rust-core-polyglot.md`、`docs/reference-notes/{codex,openfang}.md`、`docs/specs/*`、`.gitignore`（vendor/reference 条目）。**不得**卷入工作区里其他历史修改（README、TRADEMARK*、docs/PLAN 等）。
- 分支从 `main` 切：`docs/adr-026-specs`。
- 验收：PR 仅含上述文件；`docs/decision.md` 追加一行 ADR-023 标注 `Superseded by ADR-026` 的修订（允许该文件这一处修改）。

### A1 `protocol/` v1 真源：openapi.yaml
- 按 SPEC-002 §2 全部端点（含 M-C 的）写出 OpenAPI 3.1：paths + components/schemas（Session/Run/ToolCall/Approval/Decision/Schedule/Usage/Model/Error）。M-C 端点标 `x-milestone: M-C`。
- 验收：`npx @redocly/cli lint protocol/openapi.yaml` 无 error；schema 与 SPEC-002 §1 逐字段一致；含 §5 错误格式组件。

### A2 事件与模块 schema
- `protocol/events.schema.json`：信封 + 全部事件类型（SPEC-002 §3），notification/request 分组标注；`protocol/module.schema.json`：现有 ModuleManifest 的 JSON Schema 化（含 container/models/navItem，预留 `pgl` 对象字段）。
- 验收：两文件过 ajv 校验（含每类事件至少一个合法示例 fixture，fixtures 放 `protocol/fixtures/`）。

### A3 contract-tests 包
- 新建 `packages/contract-tests/`（vitest，独立 package.json，读 `A24_BASE_URL` env 默认 `http://127.0.0.1:8765`——不可用 `BASE_URL`，Vite 内置变量会被 vitest 注入覆盖）。覆盖现 node daemon 已有能力在 v1 出现前的现状端点（/health、/api/llm/chat、/api/llm/models、/api/llm/usage）各正/错例，并为 v1 端点建好按 milestone 跳过的测试骨架（`describe.todo`）。
- 验收：本地起 daemon 后 `pnpm --dir packages/contract-tests test` 全绿（`-F` workspace 过滤待 A4 落地后启用）；不依赖 Electron。

### A4 仓库重构为 pnpm workspace
- `src/main+renderer+shared` → `apps/desktop/src/…`；`src/backend` → `packages/node-daemon/`；根建 `pnpm-workspace.yaml`；各 tsconfig/vite/vitest/eslint 路径、electron-builder 配置、CI 同步更新；`rust/` 建空目录占位（README 说明）。
- 验收：`pnpm dev` 起得来且 Chat 页可用；`pnpm typecheck && pnpm test && pnpm lint` 全绿；CI 绿；git 历史用 `git mv` 保留（diff 显示 rename）。

### A5 node-daemon v1 适配层（mock daemon）
- 在 node-daemon 加 `/api/v1/health`、`/api/v1/chat`、`/api/v1/models`、`/api/v1/usage`（内部复用现有 gateway），加 WS `/api/v1/events`（先只广播 `run.started/model.delta/run.completed`，由 /api/v1/chat 触发模拟 run 生命周期）；实现 SPEC-002 §4 的 ready 行（token 空串）；renderer 的 Chat 页改走 v1。
- 验收：contract-tests 中 v1 已实现端点转正并全绿；旧路由不受影响。

### A6 api-client 生成管道 + CI 三 job
- `packages/api-client/`：openapi-typescript 生成脚本（`pnpm gen:api`）；`protocol/events.schema.json` → 事件 TS 类型（json-schema-to-typescript）；CI 扩为 SPEC-001 §7 三 job（rust job 先 `if: false` 占位），加生成漂移检查。
- 验收：CI 三 job 定义齐全并绿；手改 api-client 会被 CI 抓住（drift check 演示于 PR 描述）。

---

## M-B Rust 最小 daemon（完成后 tag `v0.1.0-alpha`）

| ID | 任务 | 依赖 | 状态 | PR |
|---|---|---|---|---|
| B1 | Cargo workspace + agent24-protocol | A6 | pending | |
| B2 | agent24d 骨架：health + 握手 + 优雅关闭 | B1 | pending | |
| B3 | ModelProvider trait + chat 透传 | B2 | pending | |
| B4 | WS 事件通道 | B3 | pending | |
| B5 | BackendManager 双后端开关 + contract 双跑 | B4 | pending | |
| B6 | agent24-cli 骨架 | B5 | pending | |

### B1 Cargo workspace + agent24-protocol
- `rust/` workspace（edition 2024，`forbid(unsafe_code)`，workspace lints：clippy deny warnings + unwrap_used）；`agent24-protocol` crate：SPEC-002 §1/§3 全部类型（serde snake_case + schemars）；`cargo deny` 配置（禁 GPL/AGPL）。
- 验收：`cargo test -p agent24-protocol` 含 serde 往返测试（用 `protocol/fixtures/` 同一批 fixture 断言与 JSON Schema 一致；显式断言事件 `type` 为点分名如 `run.started`，非 `run_started`）；CI rust job 启用并绿。

### B2 agent24d 骨架
- `rust/apps/agent24d`：axum；`serve --port 0`；`GET /api/v1/health` 返回 `backend:"rust"`；stdout ready 行（真 token，32B 随机）；Bearer 校验中间件；`CancellationToken` 贯穿 + SIGTERM/SIGINT 优雅关闭（有序：停接新请求 → 等在飞 → 退出，超时强杀自身任务）。
- 验收：contract-tests 以动态 port+token 对 agent24d 跑 health 用例绿；kill -TERM 下 2s 内干净退出无 panic。

### B3 ModelProvider trait + chat 透传
- `agent24-models`：`trait ModelProvider { async fn complete(req, cancel) -> …; async fn stream(req, tx, cancel) -> … (defaulted) }`；`OpenAICompatProvider`（oMLX/Ollama 均适用，reqwest）；**registry map** 注册（禁 if/else 工厂）；`/api/v1/chat`、`/api/v1/models`、`/api/v1/usage` 落地（用量内存累计）。
- 验收：对 agent24d 跑 contract-tests 的 chat/models/usage 全绿（需本机 oMLX 或 Ollama；CI 用 mock provider feature 跑单测）；cancel 传入后请求确实中断（单测用挂起 mock 验证）。

### B4 WS 事件通道
- `/api/v1/events`：axum WS + 强类型事件 enum（serde tag）；内部 `broadcast` 总线 → per-连接转发（容量按 codex 经验放大出站缓冲）；拒绝带 Origin 头的升级；chat 触发 run 生命周期事件（对齐 A5 mock 行为）。
- 验收：contract-tests 事件用例双后端语义一致；断连不影响 run 继续。

### B5 BackendManager 双后端开关 + contract 双跑
- `apps/desktop` BackendManager：`AGENT24_BACKEND=node|rust`（默认 node）；rust 路径改 `spawn` 二进制 + 解析 ready 行 + 传递 port/token 给 IPC 代理层；健康检查/自动重启对两种后端统一。
- 验收：Electron UI 零改动下两种后端均可用 Chat；CI contract job 矩阵双跑全绿。

### B6 agent24-cli 骨架
- `agent24 daemon start|status|stop`、`agent24 chat "<msg>"`（attached：发现/连接已运行 daemon；standalone：临时拉起）；`agent24 models`。
- 验收：CLI 端到端 smoke 测试脚本；`--help` 完整；README 快速开始更新。
- **完成后**：请求用户确认 → tag `v0.1.0-alpha`。

---

## M-C Agent Loop + 调度 + 审批 + TUI（完成后发布 `v0.1.0`）

| ID | 任务 | 依赖 | 状态 | PR |
|---|---|---|---|---|
| C1 | agent24-core 领域模型 + agent24-store | B5 | pending | |
| C2 | Agent Loop v1（runs 端到端） | C1 | pending | |
| C3 | Tool trait + registry + 基础工具 | C2 | pending | |
| C4 | 审批系统 | C3 | pending | |
| C5 | Schedule 调度器 | C2 | pending | |
| C6 | `agent24 tui` 最小版 | C4, C5 | pending | |
| C7 | 桌面端 Runs/Schedules/Approvals UI | C4, C5 | pending | |
| C8 | v0.1.0 发布工程 | C6, C7 | pending | |

### C1 agent24-core + agent24-store
- core：Run/Session/ToolCall/Approval/Schedule 状态机（纯逻辑，穷举非法转移返回错误）；store：sqlx SQLite migrations（全部实体表 + audit 表含 prev_hash 链）、repo 层。
- 验收：状态机单测穷举 SPEC-002 §1.2 全部转移（合法/非法）；store 测试用内存/临时库；`.sqlx` 提交 CI offline 绿。

### C2 Agent Loop v1
- `agent24-agent`：`POST /api/v1/runs` → 队列 → loop（构上下文 → provider 调用 → tool call 解析 → 迭代，MAX_ITERATIONS 保护）；**CancellationToken 织入每个等待点**；全生命周期发 WS 事件；run/toolcall 落库；`/runs/{id}/cancel` 生效于任意状态。
- 验收：contract-tests runs 用例（创建/查询/取消/事件序列）绿；取消一个正在流式输出的 run，1s 内收到 `run.cancelled` 且 provider 请求已中断。

### C3 Tool trait + registry + 基础工具
- `agent24-tools`：`trait Tool { fn definition(&self) -> ToolDefinition; async fn call(&self, ctx, input, cancel) -> ToolResult }` + registry；内置：`http_fetch`（SSRF 防护：拒内网/元数据地址）、`fs_read`/`fs_write`（路径白名单）、`shell_exec`（argv 数组执行不走 shell 字符串）；dispatch 流水线 normalize → capability 校验（先做白名单版）→ approval 门（C4 合入前为 **fail-closed stub**：策略标记需审批的工具（shell_exec、fs_write）一律 auto-deny 并记审计，即这两个工具在 C4 前不可用；仅 http_fetch/fs_read 可自动执行）→ 执行（timeout 包裹）。
- 验收：每工具正/错例单测；`GET /api/v1/tools` 列出全部；LLM 实际能在 run 中调用 http_fetch 完成一个抓取任务（集成测试打本地 fixture server）。

### C4 审批系统
- `agent24-policy`：审批策略配置（默认 `shell_exec`、`fs_write` 需审批）；`HashMap<approval_id, oneshot::Sender<Decision>>`（Drop=abort，Decision Default=deny）；`approval.required` 事件 + `POST /approvals/{id}`（409 语义）+ `expires_at` 超时；`approval.resolved` 广播；审计双写（详情落库、日志脱敏）。
- 验收：contract-tests 审批用例：挂起→批准→继续 / 拒绝→模型收到 reason / abort→run cancelled / 超时→timed_out / 重复决议 409 / **daemon 被 kill 后重启，遗留 pending 审批全部标记 aborted**。

### C5 Schedule 调度器
- `agent24-scheduler`：`cron` crate + chrono-tz；tick interval + Skip；到期 pre-advance；持久化经 store；连续失败 5 次自动禁用 + `schedule.disabled` 事件；CRUD + `run_now` 端点；daemon 重启恢复 next_run。
- 验收：mock clock 单测（禁真实 sleep）：cron/every/at 三型触发正确、DST 时区用例、pre-advance 防重复、失败禁用；contract-tests CRUD 绿；端到端：`every 60s` schedule 触发的 run 完成并发全事件。

### C6 `agent24 tui` 最小版
- ratatui；三面板：runs 列表（实时状态）/ 当前 run 事件流 / **审批队列**（渲染 `available_decisions`，Esc=取消不落决策，任何选择发显式决策）；WS 断线自动重连 + REST 对账。
- 验收：SSH 场景手册化验证脚本（起 daemon → 触发需审批 run → TUI 批准 → run 完成）；对照 codex 两条硬契约（显式决策、Esc 语义）。

### C7 桌面端 Runs/Schedules/Approvals UI
- apps/desktop 新增三页（走 api-client + WS）：Runs（列表+详情+取消）、Schedules（CRUD 表单，cron 表达式即时预览下次触发时间）、Approvals（待办 + 系统通知弹审批）；侧边栏入口。
- 验收：现有 UI 测试标准（覆盖率阈值）；桌面通知在 `approval.required` 时弹出并可直达决策。

### C8 v0.1.0 发布工程
- electron-builder 打包内嵌 agent24d 二进制（extraResources，按平台）；`AGENT24_BACKEND` 默认切 rust；CLI 二进制随 GitHub Release 附件发布；CHANGELOG.md；版本号统一 0.1.0；发布 checklist 文档（含「妈妈测试」5 项自查表）。
- 验收：本机产出 dmg 安装后全流程可用（chat + 创建 schedule + 审批）；**发布动作本身（tag/release/上传）列 checklist 交用户执行**。

---

## M-D 记忆 + 模型三层路由（v0.2.0）——启动前与用户确认

| ID | 任务 | 依赖 | 状态 |
|---|---|---|---|
| D1 | agent24-memory：L0 KV + canonical session（超阈值 LLM 摘要压缩） | C8 + 用户确认 | pending |
| D2 | ModelRouter 一等公民：TaskProfile → tier(local/remote/lora) 选择，health+cooldown 反馈闭环，隐私标签强制本地 | C8 + 用户确认 | pending |
| D3 | Guardian 自动审批员：L1 本地小模型评估 `{risk_level, rationale}`，低风险自动放行+结构化审计，高风险升级人审 | C4, D2 | pending |
| D4 | Python ML worker 接入（embedding/whisper 先行；LoRA 训练后置） | D2 | pending |

## M-E 模块生态桥接（v0.3.0）

| ID | 任务 | 依赖 | 状态 |
|---|---|---|---|
| E1 | agent24-mcp：rmcp client（stdio/http），MCP 工具以 `mcp_{server}_{tool}` 注入 registry | C8 + 用户确认 | pending |
| E2 | node-host：现有 5 个 CapabilityModule 经 JSON-RPC 接入内核 | E1 | pending |
| E3 | module.schema.json 落地 UI Module 规范 + 模块市场页对接 | E2 | pending |
| E4 | agent24d 作为 MCP server 暴露自身工具 | E1 | pending |
| E5 | PGL manifest（pgl.yml）解析钩子 + AgentStore 元数据展示 | E3 | pending |

## M-F 24/7 化 + 渠道（v0.4.0）

| ID | 任务 | 依赖 | 状态 |
|---|---|---|---|
| F1 | headless 开机自启（launchd/systemd 模板）+ 托盘常驻 | C8 + 用户确认 | pending |
| F2 | 看门狗与崩溃自愈（heartbeat FSM，参考 openfang 3 次 cooldown 模式） | F1 | pending |
| F3 | 微信渠道（iDoris-SDK / @agent-wechat）：入站消息 → run，审批可经微信完成 | C8 + 用户确认 | pending |
| F4 | Nostr 渠道（agent-speaker，NIP-44） | F3 | pending |
| F5 | 7×24 稳定性验证：Mac mini 连续 7 天，日程照跑，无人工干预 | F2 | pending |
