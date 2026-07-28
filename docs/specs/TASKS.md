# TASKS — 任务队列与状态跟踪

> **这是 loop 的唯一工作来源与状态文件。**
> 状态值：`pending` → `in-progress` → `in-pr(#N)` → `merged` ｜ `blocked(原因)`
> loop 每轮：取「状态 pending 且依赖均为 in-pr/merged」中序号最靠前的任务执行（见 LOOP.md）。
> 每完成一步（提 PR / 收到 merge）由 loop 更新本文件并 commit。
> 最后更新：2026-07-24（M-A ✅ M-B ✅；C1 merged #35；C2 in-pr #36；alpha tag 待用户确认）；#32/#33/#34 stacked 在审）
> 2026-07-24 追加 **M-H（从 OpenWorker 借鉴：人机边界）**，并据此修订 G1/G2 的落地形态。
> 2026-07-26 **发布 v0.2.0**：H1/H2/H4/H12/H9 已合并（人机边界线）。剩 H5/H8 未阻塞待专注做，H3/H10/H11 阻塞于未建的 G1/E3/F3。

## 执行顺序总览（最佳路径）

串行主链：`A0 → A1 → A2 → A3 → A4 → A5 → A6 → B1 → B2 → B3 → B4 → B5 → B6 → [v0.1.0-alpha] → C1 → C2 → C3 → C4 → C5 → C6 → C7 → C8 → [v0.1.0 发布]`
（M-D/M-E/M-F 任务在 v0.1.0 后与用户确认优先级再启动，默认顺序见下。）

---

## 🎯 个人助理优先级（v0.2.1 之后的主排序，2026-07-27 用户确认）

> **目标**：做成「你在微信上聊、7×24 跑你的定时工作流、碰风险动作问你、按 session 记忆」的个人助理，
> 同时把基础框架做扎实到「任何人都能基于它做自己的 agent」。
> **这是 loop 的任务选择依据**：在「pending 且依赖满足」的任务里，优先级高的先做。

**P0 — 无人值守正确性（进行中，最高优先）**
- `[G1+H3]` 3-PR stack：PR-1 消息线程 ✅merged #70 → **PR-2** 异步 parked+payload 哈希 → PR-3 durable resume+重校验。
  没有它，24/7 下需审批的任务在凌晨阻塞到超时**死掉**——个人助理不成立。

**P1 — 能对话的渠道（"个人助理"的定义性一步）**
- H11 Fake 渠道 harness（先行，让 F3 可自动测）→ F3 微信渠道（入站消息→run，审批经微信完成）
  → F5 7×24 泡测（Mac mini 连跑 7 天）→ F4 Nostr 渠道 → F1b 托盘常驻（小项，可穿插）。

**P2 — 让"任何人可基于它做 agent"（框架完成度）**
- E4 agent24d 作为 MCP server 暴露自身工具 → E3 module.schema + 模块市场页
  → H10 模块/persona 安装同意摘要（依赖 E3）→ E5 PGL manifest。

**P3 — 助理更自主/更聪明**
- H5 self-wake → H8 plan mode + propose_plan → G2/G3 审批判据补充/CLI 授权成文
  → L2 语义记忆（M-D 扩展；BGE-M3 embedding+reranker 这套用在这，需先有消费者）。

**P4 — 生态 / 分发（M4 / M5，最后）**
- 跨用户 skill 共享、Nostr 分发 skill、iDoris 主 AI 接入、模块 marketplace、
  模块签名 + AirAccount 信任根、跨设备记忆同步、Tauri mobile。

**里程碑门（需用户确认才越过，不擅自跨）**：进入 P4（M4/M5 产品级生态）前停下确认；发布 tag/Release 由用户执行。

---

## M-A 契约冻结 + 仓库重构

| ID | 任务 | 依赖 | 状态 | PR |
|---|---|---|---|---|
| A0 | 提交现有设计文档 PR | — | merged | #21 |
| A1 | `protocol/` v1 真源：openapi.yaml | A0 | merged | #23 |
| A2 | `protocol/` 事件与模块 schema | A1 | merged | #24 |
| A3 | contract-tests 包（对现 node daemon） | A1 | merged | #25 |
| A4 | 仓库重构为 pnpm workspace 目标布局 | A3 | merged | #26 |
| A5 | node-daemon v1 适配层（mock daemon） | A4 | merged | #27 |
| A6 | api-client 生成管道 + CI 三 job | A5 | merged | #28 |

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
| B1 | Cargo workspace + agent24-protocol | A6 | merged | #29 |
| B2 | agent24d 骨架：health + 握手 + 优雅关闭 | B1 | merged | #30 |
| B3 | ModelProvider trait + chat 透传 | B2 | merged | #31 |
| B4 | WS 事件通道 | B3 | merged | #32 |
| B5 | BackendManager 双后端开关 + contract 双跑 | B4 | merged | #33 |
| B6 | agent24-cli 骨架 | B5 | merged | #34 |

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
- `/api/v1/events`：axum WS + 强类型事件 enum（serde tag）；内部 `broadcast` 总线 → per-连接转发（容量按 codex 经验放大出站缓冲）；拒绝带 Origin 头的升级；chat 触发 run 生命周期事件（对齐 A5 mock 行为）。**生成源切换**：从 agent24-protocol 导出 events.schema.json（schemars）并覆盖 `protocol/`，CI 加零漂移检查（openapi.yaml 的导出切换随 utoipa 引入同步完成）。
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
| C1 | agent24-core 领域模型 + agent24-store | B5 | merged | #35 |
| C2 | Agent Loop v1（runs 端到端） | C1 | merged | #36 |
| C3 | Tool trait + registry + 基础工具 | C2 | merged | #37 |
| C4 | 审批系统 | C3 | merged | #38 |
| C5 | Schedule 调度器 | C2 | merged | #39 |
| C6 | `agent24 tui` 最小版 | C4, C5 | merged | #40 |
| C7 | 桌面端 Runs/Schedules/Approvals UI | C4, C5 | merged | #41 |
| C8 | v0.1.0 发布工程 | C6, C7 | in-pr #42  | |

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
| D1 | agent24-memory：L0 KV + canonical session（超阈值 LLM 摘要压缩） | C8 + 用户确认 | done |
| D2 | ModelRouter 一等公民：TaskProfile → tier(local/remote/lora) 选择，health+cooldown 反馈闭环，隐私标签强制本地 | C8 + 用户确认 | done |
| D3 | Guardian 自动审批员：L1 本地小模型评估 `{risk_level, rationale}`，低风险自动放行+结构化审计，高风险升级人审 | C4, D2 | done |
| D4a | ML worker Rust 侧契约 + 客户端：`agent24-worker`（MlWorker trait + HTTP/JSON 客户端 + mock，embed/transcribe/health 线协议） | D2 | done |
| D4b | Python ML worker serving 实现（embedding/whisper；LoRA 训练后置），对齐 D4a 契约 | D4a | **deferred（等消费者）** |
| D5a | daemon 集成布线（一）：ModelRouter 接管 daemon 全部模型调用（chat/runs），Guardian 按 `A24_GUARDIAN` opt-in 接入 ApprovalBroker（默认关） | D2, D3 | done |
| D5b | 会话记忆（D1 生效）：CanonicalSession 接入 run 生命周期——按 session 载入既往上下文、完成后回写并按阈值压缩 | D5a, D1 | done |
| D5c | HttpMlWorker 挂载 + 消费端（D4a 生效） | D5a, D4a | **deferred（等需要时再做）** |

### M-D 收尾说明（2026-07-24）

**M-D 的目标已达成**：ADR-026 对 M-D 的定义是「Memory **L0-L1**、上下文压缩；三层路由落地」——
L0 KV（D1）、L1 会话压缩（D1+D5b）、三层路由（D2）全部 merged，daemon 也已真正布线（D5a/D5b）。

**D4b / D5c 是按决策延后，不是漏掉的活**，重启 loop 时不要盲目捡起来：

- ADR-026 §5 已论证：**跑 LLM 不需要 Python**（oMLX 走 OpenAI-compat HTTP 即可）。
  Python ML Worker 的价值只在 oMLX chat 接口给不了的三件事：Embedding、Whisper、LoRA 训练。
- 这三件当下**都没有上层消费者**：
  - Embedding → 给 L2 语义检索用，而 **L2 不在 M-D 范围内**（L0-L1 走摘要压缩，不用向量）
  - Whisper → 等 **M-F 渠道**（微信/Nostr）接进来，语音输入才有意义
  - LoRA → L3，最靠后
- 因此 **`/api/v1/embed` 端点也不加**：没有消费者，加了是空端点，还要动 `protocol/openapi.yaml` 触发零漂移门。

**原则：先有消费者，再有提供者。** D4a 已把 wire 契约 + 客户端 + mock 锁死，
将来任一能力真的需要时，Python 侧照契约实现即可接上，无需重新设计。

**下一步应由「想要哪个能力」驱动，而不是按任务编号顺序推**（语音 → M-F；长期语义记忆 → L2；24/7 无人值守 → M-F）。


## M-E 模块生态桥接（v0.3.0）

| ID | 任务 | 依赖 | 状态 |
|---|---|---|---|
| E1 | agent24-mcp：rmcp client（stdio），MCP 工具以 `mcp_{server}_{tool}` 注入 registry | C8 + 用户确认 | **done** #54 + 接线 |
| E2 | ~~node-host：现有 5 个 CapabilityModule 经 JSON-RPC 接入内核~~ | E1 | **descoped**（见下方说明） |
| E3 | module.schema.json 落地 UI Module 规范 + 模块市场页对接 | ~~E2~~ E1 | **merged #74**（安装门禁校验；市场浏览 UI 归 M4） |
| E4 | agent24d 作为 MCP server 暴露自身工具 | E1 | **merged #73**（server 模块 + `agent24 mcp` + e2e client↔server 测试） |
| E5 | PGL manifest（pgl.yml）解析钩子 + AgentStore 元数据展示 | E3 | pending |

### E3 落地范围（2026-07-27）

`module.schema.json`（A2 建）此前**没有任何强制点**——`loadInstalledModule` 只查 manifest 是不是对象。E3 把规范**落地为安装门禁**：

- `packages/node-daemon/src/module-manifest.ts`：`validateManifest(manifest) → string[]`，依 module.schema.json 校验**结构契约**（required 字段齐、类型对、id 匹配 npm pattern、permissions 为字符串数组）；**不**拒绝未知 enum 值/未知字段（schema 自述其 string enum 为开放集、消费者须忽略未知字段——前向兼容）。
- 接入 `loadInstalledModule`：manifest 不合规 → 记录原因 + 拒绝注册（不再让畸形清单跑到运行期才炸）。**这就是 H10「清单严格校验」的基座**。
- **drift-guard 测试**：加载 `protocol/module.schema.json`，断言 validator 的 REQUIRED/id-pattern 与 schema 同步——校验规则不会悄悄偏离权威。33 个 node-daemon 测试全绿 + typecheck 净。

**未纳入 E3（归 M4）**：模块市场**浏览/发现** UI——需要发现服务（Nostr 索引 / npm scope 扫描），那是 M4「模块 Marketplace」的活；且 E4 落地后 MCP 已是主力扩展路径，node-daemon 模块系统权重下降，市场 UI 不宜在此刻重投入。E3 交付「规范落地」这一确定有价值、且 H10 依赖的核心。

### E4 设计（2026-07-27，用户选 P2 后开工）

**关键判断：不裸暴露危险工具，而是暴露"能力"，且经守门。** 让外部 MCP client（Claude Desktop 等）把 agent24d 当一个 MCP 工具用，但绝不给外部一条绕过审批直接跑 `shell_exec` 的路。

- **形态**：`agent24 mcp`（stdio）子命令 = 一个 rmcp **server**，代理到运行中 agent24d 的 v1 HTTP API（守门权威留在 daemon 一处，审批照常经 daemon 的 TUI/桌面/未来微信弹出）。
- **暴露面（curated）**：
  - `agent24_run(prompt, session_id?)` → `POST /api/v1/runs` 轮询到终态返回输出。**走完整 gated loop**——外部要跑危险动作，宿主人在 daemon 侧审批，天然继承 C4/D3/H1-H4。
  - 只读自省：`agent24_list_tools`(GET /tools)、`agent24_list_runs`(GET /runs)。安全，无副作用。
- **不做**：把 registry 里的 `shell_exec`/`fs_write` 逐个当 MCP 工具直接暴露给外部——那等于给第三方一条免审批执行路。要真按「逐个工具」暴露，也必须每次经审批门（宿主阻塞批准），MVP 先不做，只出 `agent24_run` + 只读自省。
- **安全性是白拿的**：一切副作用都发生在 `agent24_run` 触发的 run 内部，经既有 gated dispatch；MCP server 层只是转发 + 只读。
- **rmcp**：现 `agent24-mcp` 只开了 `client` feature；E4 加 `server`（+ stdio transport）feature；SDK 仍限在 adapter crate（ADR-026）。
- **验收**：`agent24 mcp` 可被一个标准 MCP client（或单测 harness）发现 `agent24_run`/`agent24_list_*`；`agent24_run` 端到端跑通一个 run 并返回输出；危险动作确实在 daemon 侧触发审批而非被外部静默执行。

### E2 降级说明（2026-07-24，用户确认）

**原计划**：写 node-host 兼容层，让现有 5 个 `CapabilityModule` 经 JSON-RPC 接入内核。
**决定：不做**（用户已确认）。理由是逐个看过这 5 个模块的实质：

| 模块 | 行数 | 实质 |
|---|---|---|
| `example-ping` | 23 | 健康检查演示 —— daemon 早有 `/api/v1/health` |
| `example-summarize` | 41 | 调 LLM 摘要 —— 内核本来就会调 LLM |
| `example-hello-ui` | 47 | UI 模块**范例**，非能力 |
| `example-codebox` | 41 | BoxLite 隔离 Python 执行 ← 唯一真能力 |
| `example-service-box` | 80 | BoxLite 长运行服务容器 ← 同上 |

前三个是纯演示，为它们建一整套 Node 兼容层属于「为迁移而迁移」。

**更重要的是前提变了**：ADR 写「5 个示例模块跑通」这条验收时 MCP 还不是选项；
E1/E1b 落地后内核已能接整个 MCP 生态（文件系统、git、搜索、数据库……），
获取真实能力的成本远低于救活 5 个 demo。

**BoxLite 沙箱执行（codebox/service-box）若要保留，应包成 MCP server**，
而不是另建 node-host 通道 —— 那样它自动继承已验证的审批门、命名空间、超时预算与取消传播，
不必为它单独再实现一遍这些安全性质。

**E3 的依赖因此从 E2 改为 E1。**

### M-E 开工前已确定的设计判断（2026-07-24，M-D 完成后补记）

1. **E1 有真实消费者，不是空布线。** 消费者就是现成的工具注册表（C3）——接上 MCP server 后
   agent 立刻多出可调用的真工具。与 D4b/D5c「客户端连了个不存在的服务端」性质完全不同，
   所以 E1 不适用那条「先有消费者再有提供者」的延后理由。
2. **`agent24_tools::Tool` 与 MCP 是 1:1 的**：`info() / parameters() / call()`
   ↔ MCP 的 `name / inputSchema / tools/call`。桥接不需要改造既有工具体系。
3. **安全性是白拿的，这也是选型理由**：MCP 工具经 `ToolRegistry` 注册后自动走
   **C4 审批门 + D3 Guardian**——外部 server 的工具无法绕过审批。
   这是「桥接进注册表」优于「另开一条调用路径」的主要论据，实现时不要走捷径绕开 registry。
4. **建议顺序 E1 → E4 →（E2/E3/E5）**：E1 之后先用一个现成第三方 MCP server 打通端到端
   （ADR 验收「1 个外部 MCP server 可用」即可达成），不必等 node-host；
   E2 的 node-host 价值在于救活既有 5 个模块，属于迁移工作，可后置。
5. **E1 实现注意**：JSON-RPC 2.0 over stdio 的难点在**请求/响应关联、子进程生命周期、
   取消传播**（内核到处是 CancellationToken，MCP 调用必须可取消）。
   优先评估直接用 `rmcp` crate 而非手写协议栈。

## M-F 24/7 化 + 渠道（v0.4.0）

| ID | 任务 | 依赖 | 状态 |
|---|---|---|---|
| F1a | headless 开机自启：`agent24 service install/uninstall/status`（macOS LaunchAgent） | M-D | **done** #51 |
| F1b | 托盘常驻（菜单栏状态/启停） | F1a | pending |
| F2 | 崩溃自愈 | F1a | **done** #51（见下方设计变更） |
| F3 | 微信渠道（**WeChat iLink 官方 Bot API**）：入站消息 → run，审批经微信完成 | C8 + 用户确认 | **in-pr**（`packages/wechat-bridge`，见下） |
| F4 | Nostr 渠道（agent-speaker，NIP-44） | F3 | pending |
| F5 | 7×24 稳定性验证：Mac mini 连续 7 天，日程照跑，无人工干预 | F2 | pending（F1a/F2 已就绪，可开始跑） |

### F3 落地：WeChat iLink 官方 Bot API（2026-07-28，用户指路 `~/Dev/tools/heinu1`）

**技术栈修正**：早前以为走公众号 MP API，实际用户已用 **WeChat iLink 官方 Bot API**（`https://ilinkai.weixin.qq.com`，`weixin.qq.com` 域，非第三方）跑通——参考实现 heinu1（`bot/src/ilink/`）。核心是**扫码登录**拿 `bot_token`，再走长轮询收发消息。**没有 npm SDK**——协议是一层薄 HTTP（headers `AuthorizationType: ilink_bot_token` + `Bearer <token>` + `X-WECHAT-UIN`）。

- **QR 登录**：`GET /ilink/bot/get_bot_qrcode?bot_type=3` → 展示二维码 → 轮询 `get_qrcode_status` → `confirmed` 拿 `bot_token`+`baseurl`，持久化（用户**扫一次**）。
- **收**：长轮询 `POST /ilink/bot/getupdates`（cursor `get_updates_buf`）→ `msgs[]`。
- **发**：`POST /ilink/bot/sendmessage`（`item_list[{type TEXT, text_item}]`，超长切 1800）。

**落地形态（用户确认：Node 包移植）**：新建 `packages/wechat-bridge`（TS），把 heinu1 的 `ilink/`（auth/client/monitor/sender/types）搬过来，接线到 **agent24d v1 HTTP API**（而非 spawn claude）：
- 每个微信用户 ↔ 一个 agent24 session（`daemon.json` 自动发现 daemon）；入站消息 → `POST /runs` → 轮询 → `sendmessage` 回结果。
- **审批经微信 + durable resume 组合**：run park 在审批上 → 发「需要批准：<summary> 回复 y/n」→ 用户回 y → `POST /approvals/{id}` → **daemon `resume_run` 续跑**（H3）→ 再 await → 回最终输出。F3 与 H3 天然契合。
- 测试：`parseDecision`/`splitText`/`messageText` 纯逻辑（7 测）+ typecheck 绿。**实机**需用户扫码验证（凭据链）。
- **后置**：图片/语音/文件（当前只文字）、会话映射持久化已做（`wechat-sessions.json`）、launchd 常驻（同 heinu1）。

### F1a/F2 设计变更（2026-07-24，#51）

**原计划**：F2「看门狗与崩溃自愈（heartbeat FSM，参考 openfang 3 次 cooldown 模式）」——即自己写一个守护进程。
**实际做法**：不自造守护，改由 **launchd** 承担，原计划的 heartbeat FSM **不再需要**：

| 需求 | launchd 键 |
|---|---|
| 开机自启 | `RunAtLoad` |
| 崩溃自愈 | `KeepAlive{SuccessfulExit:false}` |
| 退避（替代 3 次 cooldown） | `ThrottleInterval` |
| 崩溃留证 | `StandardOut/ErrorPath` |

**理由**：自造的守护进程自己也会挂；launchd 由操作系统拉起，不会。
且 `SuccessfulExit:false` 天然区分「崩溃」与「用户主动 `agent24 daemon stop`」——
朴素的「总是重启」守护会把用户的主动停止也复活回来，这是最容易做错的一点。

**已实机验证**（非仅单测）：`kill -9` 约 2 秒自愈（`runs=2`）；`daemon stop` 退出码 0 且 20 秒后不复活。

**踩到的坑（F5 验证时注意）**：launchd 不传登录 shell 的任何环境变量
（`launchctl getenv PATH` 为空，只有 `/usr/bin:/bin:/usr/sbin:/sbin`）。
因此 `shell_exec` 调 `node`/`git`/`python3` 会「手动起能用、后台自启失败」。
#51 已在安装时**快照** PATH + daemon 读取的 7 个变量写入 `EnvironmentVariables`（plist 权限 0600，因可能含 `OMLX_API_KEY`）。
**这是快照——改了环境变量需重新 `service install` 刷新。**

---

## M-G 从 MediaBot 借鉴（跨仓库）

> 来源：`github.com/iDoris-ai/MediaBot`（本地 `~/Dev/tools/MediaBot`）。
> 两个仓库是同一作者的「左右两侧」——Agent24 **自建运行时**（本地/小模型优先），
> MediaBot **搭 Claude Code 运行时跑媒体运营负载**。两边独立收敛到同一批决策
> （审批门、SQLite、daemon+scheduler、契约优先、自接 MCP），因此差异处特别值得互相借鉴。

| ID | 任务 | 依赖 | 状态 |
|---|---|---|---|
| G1 | **异步审批队列**：审批可离线批复，批准后再执行；含 payload 完整性校验 | F1a, C4 | pending |
| G2 | 审批判据补充「对外/不可撤回」维度（现按工具种类分级） | C4 | pending |
| G3 | CLI wrapper 集成策略（包二进制而非 vendor 源码）写入 SPEC-001 | — | **in-pr**（SPEC-001 §10：进程边界=授权边界，包二进制不 vendor 源码，与 §9/cargo-deny 互补） |

### G1 为什么重要（M-F 之后必然撞上）

**现状**：Agent24 的审批是**同步阻塞**的——`ApprovalBroker::request` 在工具分发时被 await，
超时 fail-closed 拒绝。这在人坐在电脑前时是对的。

**F1a（24/7 无人值守）落地后就不对了**：凌晨 3 点触发的定时任务若需审批，
同步模型下只能阻塞到超时然后**失败关闭——任务直接死掉**，而不是等人醒来批复。
对无人值守 agent 这是错误语义。

**MediaBot 的做法**：审批进队列 → 人次日批复 → daemon 再执行。
配套的正确性属性是 **payload 完整性哈希**（`ApprovalIntegrityError`）：

> *payload changed after it was approved — refusing to execute and re-queueing for review*

同步模型不需要这个哈希（入参始终在内存里，批的就是执行的）；
**异步模型不做就会出现「批的是 A、执行的是 B」**。两者是配套关系，不能只搬前半截。

实现时注意：现有同步路径不应删除——交互式场景（TUI/桌面端有人在看）同步阻塞体验更好。
应是**两种模式并存**，由「是否有人在线」或调用方声明决定。

> **「怎么并存」已由 OpenWorker 给出更好的答案**（见 `docs/reference-notes/openworker.md` §4）：
> 不要两条代码路径，而是**一条 parked 记录 + 一个 `visibility` 字段**（inline / inbox）。
> 同一个可等待、可持久化、可从任何界面 first-responder-wins 应答的条目，只是出现的位置不同。
> 另外 G1 还缺一半：**重启后的遗留审批应当 durable resume（接着问），而不是全部 aborted**——见 H3。

### G2 判据差异

Agent24 按**工具种类**分级（`exec` / `fs_write` / `network` / `module`）。
MediaBot 按「**对外的、挂用户名字的、发出去撤不回来**」分级，并据此让监控层永远只读、
看到热点也不自动发帖。后者是更本质的判据——它解释了*为什么*某个动作危险，
而不只是*哪个工具*危险。二者应叠加而非替换。

> **落地形态见 OpenWorker `RiskClass`**（`read/write_local/exec/external`，见 openworker.md §1）：
> 分级的价值不在于分得细，而在于**不同级享有不同的豁免路径**——只有 `external` 能拿常驻定向授权，
> `exec` 永远问。H1 按这个形态实现，G2 即随之满足。

### G3 授权策略

MediaBot `src/core/cli-adapter.ts` 的论证值得成文：

> *invoking a program does not create a derivative work, so this route stays
> licence-clean even for upstreams whose source we could not copy*

即**包二进制而不 vendor 源码**，可以合法集成我们无法复制其源码的上游（如 GPL 工具）。
Agent24 现有 `vendor/reference/` 已注明「zerostack 是 GPL 只读思路禁止复制代码」，
但没有正面写出「那该怎么合法集成」——G3 补上这条。

---

## M-H 从 OpenWorker 借鉴（人机边界）

> 来源：`github.com/andrewyng/openworker`（MIT，本地只读克隆 `vendor/reference/openworker/`）。
> 研读笔记：**`docs/reference-notes/openworker.md`**——开工前必读，本节只列任务不重复论证。
> 产品同构（本地优先 + 审批门 + MCP + 定时 + 渠道 + 桌面壳），实现异构（Python 单体 vs Rust 内核）。
> **借鉴的不是它的架构，而是它把「人机边界」想得比我们细的那几处。**
>
> **本节经 codex 对抗式评审后重写（2026-07-25）**。三处实质修正，已在下方各任务里落实：
> ① 原 H6「补两条调度策略」**删除**——两条我们都已有（`agent24-scheduler/src/lib.rs:8` 的 skip-missed
> doc contract + `agent24-agent/src/lib.rs:442` 的双层 spawn 监督执行）；
> ② 原 H3 严重低估成本——**我们没有持久化消息线程**（store 只有
> sessions/runs/tool_calls/approvals/schedules/audit_log，见 `migrations/0001_initial.sql`），
> 而 OpenWorker 的 durable resume 恰恰是靠消息线程重建的，且 H3 **必须**与 G1 的 payload 哈希捆绑；
> ③ 原 H1 写成「取代 `requires_approval: bool`」过于轻率——那是**公开协议字段**
> （`protocol/openapi.yaml:971`），改动触发 `pnpm gen:api` + CI 零漂移门，必须做成**加法迁移**。

| ID | 任务 | 依赖 | 状态 |
|---|---|---|---|
| H1 | **`risk_class` 加法迁移**：`read/write_local/exec/external` 作为新协议字段落地，`requires_approval` 改为由它派生；零行为变更 | C4 | merged #63 |
| H2 | **用户本地风险 override**：glob 规则调整单个工具的 risk_class；**模块/persona 不得写入**；与 Guardian 的优先级明确 | H1, E1 | merged #61 |
| H4 | **external 定向常驻授权**：`tool → 确切目标`，挂在 schedule 记录上；**并对 external 工具停用宽泛的 `approve_for_session`** | H1, C5 | merged #62 |
| H3 | **异步审批 + durable resume**（与 G1 合并执行）：消息线程持久化 → payload 完整性哈希 → 重启后复原而非全 abort → 陈旧性重校验 | G1, F1a, H1 | **merged**（PR-1 #70 + PR-2 #72；P0 完成） |
| H5 | **self-wake**：`sleep_for` / `sleep_until` / `wake_on(job)` / `wake_on_event`，复用 scheduler tick 的 extra_tick 位；含关停取消契约 | C5 | pending（未阻塞；需专门的 wake 表 + tick 集成，H4 量级） |
| H8 | **plan mode + `propose_plan`**：只读门禁下 explore → 提交计划 → 人批准 → 才退出只读 | C4 | pending（下一个 P3 大件；设计见下） |
| H9 | **只读 explorer subagent**：独立上下文、只读工具集、禁递归 | C3 | merged #66 |
| H10 | **模块/persona 安装同意摘要**：清单严格校验 + 安装后默认 disabled pending consent + 安装绝不写 override | H2, E3 | **in-pr**（见下） |
| H11 | **协议级 Fake 渠道 harness**：FakeWeChat / FakeNostr，让渠道审批与 inbox 可自动测 | F3 | pending |
| H12 | **provider 错误人话翻译**：额度/权限/模型不存在类错误落成可读文案 | — | merged #65 |
| H7 | 工具并发三分法（授权串行 → 只读并发 → 写/exec 串行） | H1 | **deferred**（收益不确定，代价高，见下） |
| ~~H6~~ | ~~调度器补 catch-up + spawn 不 await~~ | — | **删除（已存在）** |

### 执行顺序（codex 评审后确定）

`H1 → H2 → H4 → [G1+H3] → H5 → H10/H11 →（H8/H9/H12 择机）`，H7 最后且可能永不做。

前三条是一条线：**H1 提供判据 → H2 用判据放宽 → H4 用判据收窄**。
`G1+H3` 是 M-F 前必须做完的那一块（否则 24/7 语义是错的），但它最贵，放在判据成型之后。

### G1+H3 交付形态：收敛为 2 个 PR（2026-07-27 修订）

原计划拆 3 个 stacked PR。实现中确认：**PR-1（消息线程）能干净独立，但剩下三件（visibility/park、payload 完整性、durable resume）本质是同一个行为**——TASKS.md 自己写「四件事捆绑，缺一不可」。硬拆 PR-2/PR-3 会各自变成没有消费者的空布线（payload 哈希的消费者就是 resume）。故收敛为 2 个 PR。

| PR | 切片 | 内容 | 状态 |
|---|---|---|---|
| PR-1 | 消息线程持久化 | 新 `run_messages` 表 + repo；agent loop 落库 user/assistant/tool。**复原挂起点的地基**。不动协议。 | **merged #70**（clestons v4 APPROVE） |
| PR-2 | 完整 durable resume 行为 | `feat/h3b-durable-resume`：resume 分析器 → 陈旧性重校验 → 启动复原而非全 abort → resolve 触发 run 从持久化线程续跑。 | **in-pr #72**（21 新测，28 套全绿） |

**resume 架构决定**：采用 **OpenWorker 式完整复原**——parked 审批重现 inbox，人应答后从持久化线程重建 run 继续跑（而非只让审批 durable、run 不续跑的过渡版）。
**触发模型改为 lazy（实现中确认更简单且正确）**：不在重启时为每个 pending 审批 spawn 一个跨关停存活的 waiter（那会在优雅关停时误 abort、丢 durability）。改为：重启只**重新广播** pending 审批（`assess_restore` 通过的）+ 排除 orphan-sweep；真正的续跑由**人应答时**（resolve）触发 `resume_run`——那时决策已在行上，无需 await。故不需要 `reattach`，用 `settle_resumed`（读已决行、重放授权副作用）即可。

**PR-2 内部进度（breadcrumbs）**：
- [x] `resume::plan_resume(status, thread) -> ResumePlan`（RunStatus 区分「取消 vs 死在审批」；半应答轮；8 单测）
- [x] `resume::assess_restore(...)`：工具仍在 + payload 未变（拒「批 A 跑 B」）+ TTL（`iso8601_before` cutoff）；7 单测
- [x] `ToolRegistry::execute_preapproved`：跳过门的执行路径（续跑时决策已在手，不得二次问）；2 单测（含仍强制 whitelist）
- [x] `ApprovalBroker::settle_resumed` + `GrantCtx::for_resume`：无 waiter 时读已决行、重放 session/target 授权副作用；2 单测
- [x] `execute` → `run_loop` 抽取（run/cancel 按值，loop 主体逐字不变；45 旧测全过证行为保持）
- [x] agent manager：`resume_run(run_id, approval_id)` + `drive_resume` —— 从线程重建 `messages`、`assess_restore` 复校、结算挂起 call（approved→`execute_preapproved` 跑**批准的 payload**、denied→喂 reason、abort→cancel）、半应答轮的剩余 call 走 `run_tool_call`、继续 `run_loop`；supervised spawn + cancel token + 幂等（非 awaiting/已有 task 即 no-op）。2 端到端集成测试（approve 跑工具并完成 / deny 不执行仍完成）。**注**：结算走 store-only 读决策，restored 审批的 `approve_for_session/target` 不重铸授权（fail-closed 下次再问；已在代码注释标 KNOWN LIMITATION）
- [x] `RunManager::restore_pending_approvals`：启动逐条 `assess_restore`，Restore 则重广播 `approval.required` 保留 pending、Abort 则落 aborted（兜底）；返回 (restored, aborted)。agent 测：restorable 保留+重广播、run 已 Completed 的 aborted
- [x] `sweep_orphan_runs` 排除「awaiting_approval 且有 pending 审批」（SQL `NOT IN (SELECT run_id ... status='pending')`）；store 测：parked 幸免、stranded/running 取消
- [x] `server.rs`：早期 abort-all 移除，state 建好后 `restore_pending_approvals` → 再 `sweep_orphan_runs`（顺序保证）；`decide_approval` resolve 成功后无条件调 `resume_run`（幂等，sync 情形因 run 在 cancels 而 no-op）
- **C4 验收修订**：旧「遗留 pending 全 aborted」不再是服务器行为（改为 restore-or-abort）。`abort_lingering_approvals` store 原语保留（仍有单测），仅不再被 daemon 调用。
- [ ] （PR-3 遗留，非阻塞）append 路径幂等：resume 重入不得重复 append（clestons #70 note 2）——当前 resume 只 append 新 tool result，不重放已存消息，故本 PR 无重复；正式幂等契约留 PR-3
- [ ] （PR-3 遗留，非阻塞）best-effort 契约测试：注入 `append_run_message` 失败断言 run 仍 completed
- [x] 端到端（crate 级）：seed 崩溃态 → 应答 → 复原 → 工具执行 → 完成（approve/deny 两例）。真·跨进程 kill 的 daemon 级 e2e 留手册化验证（F5 泡测顺带）

`inline`（TUI/桌面有人在看）保留现有同步阻塞路径；两种模式共用一条 parked 记录。

### H1 加法迁移（本轮执行）

**不是**把 `requires_approval: bool` 换成枚举——那是破坏性契约改动。做法：

1. `agent24-protocol`：新增 `RiskClass{read, write_local, exec, external}`（serde snake_case + schemars），
   `ToolInfo` 加 `risk_class` 字段。
2. `protocol/openapi.yaml`：`ToolInfo` 加 `risk_class`（**不进 `required`**——老客户端不得因此失败），
   跑 `pnpm gen:api` 让 api-client 同步，CI 零漂移门必须绿。
3. `agent24-tools`：每个内置工具**声明** risk_class；`requires_approval` 改为 `risk_class != Read` **派生**，
   消除「两处手工同步」的漂移可能（这是 OpenWorker `risk.py:1` 重构掉 `WRITE_TOOLS` 名集合的同一个理由）。
4. 映射（**刻意保持零行为变更**）：`fs_read → read`、`http_fetch → read`、`fs_write → write_local`、
   `shell_exec → exec`、MCP 工具 → `external`。派生出的 `requires_approval` 与当前逐字相同。
5. `http_fetch = read` 的取舍要写进代码注释：GET 无副作用，**它的危险是外泄而不是改动**——
   外泄属于污点（taint）问题，不属于风险级（见 `openworker.md` §1 与 `openfang.md` §8 的 taint 原语）。
   把网络读塞进 `external` 会让 H4 的定向授权对它生效，那是错的语义。

**验收**：`cargo test` 全绿；`GET /api/v1/tools` 每个工具带 risk_class；
新增单测断言「每个工具的 `requires_approval` == (risk_class != read)」（派生不可被绕过）；
`pnpm gen:api` 无漂移；contract-tests 双后端绿。

### H2 重写（原文「没有它 E1 等于不可用」过满）

**事实修正**：D3 Guardian 已能对 gated call 做本地小模型评估并自动放行低风险
（`agent24-policy/src/lib.rs:166`、`guardian.rs:142`），所以 MCP 并非「必须靠 H2 才可用」。

**但 Guardian 不是 H2 的替代品**，三点差异决定了两者都要：

| | D3 Guardian | H2 override |
|---|---|---|
| 默认 | **关**（`A24_GUARDIAN=1` 才开，`agent24d/src/server.rs:79`——刻意不默认信任模型） | 用户显式写下才生效 |
| 性质 | 模型判断，**非确定性**，每次调用都要跑一次 | 声明式规则，确定性，可审计 |
| 可解释 | `{risk_level, rationale}` 一次性 | 「是我允许的」，可回看可撤销 |

**优先级必须写死**：用户 override 先于 Guardian（用户的显式声明高于模型的推断）；
override 只能把风险**调整**到用户选定的级，不能凭空放行一次具体调用——
放行仍走同一条 `risk_class → 门禁` 路径。

**硬约束（写进 E3/E5/H10 之前必须先立）**：override store 是 **user-local，永远不由模块清单/persona 写入**
（上游把这条标为 inviolable，`overrides.py:1`）。模块可以*声明*它想要什么工具，
但只有用户决定信任到什么程度。否则模块市场等于让第三方自带豁免。

### H4 收窄（比原文更进一步）

现状比原笔记写得更值得改：`approve_for_session` 命中后**直接 `Verdict::Approved`**
（`agent24-policy/src/lib.rs:151`），**Guardian 也不再被咨询**（`:170`）。
`MAX_GRANTS` 溢出清空（`:420`）只防集合无界增长，**完全不限制一条已有 grant 的参数范围**。

H4 = 对 external 风险的工具提供**更窄的选项**并**停用宽泛选项**：
- 常驻规则形态 `tool → 确切目标`，资格三重收窄：① 仅 `external` ② 工具必须声明 target 参数
  ③ 调用必须真的填了 target；`exec`/`write_local` 永远问。
- 规则挂在 schedule 记录上（撤销 per-automation，删任务带走规则）。
- 授权 fail-closed：只有写操作成为授权；读权限只在同意卡片上**披露**、不存储。
- **`approve_for_session` 对 external 工具不再作为可选项呈现**——否则窄选项旁边永远摆着个宽选项，
  用户会点宽的那个。

### G1+H3 合并（M-F 前必须完成，最贵的一块）

原 H3 只写了「重启后接着问」，漏了三件事，codex 指出后补上。**四件事捆绑交付，缺一不可**：

1. **消息线程持久化**——我们目前只把成功的 exchange 追加进 session memory
   （`agent24-agent/src/lib.rs:319`），没有 assistant/tool 消息表。
   不先有它，就没有 OpenWorker 那种「从未回答的 trailing tool_calls 重建挂起点」的便宜做法。
2. **payload 完整性哈希**（G1 已论证）——异步模型下不做就会「批的是 A、执行的是 B」。
3. **复原取代 abort**——改 `agent24d/src/server.rs:388` 的启动清扫，
   并**同步修订 C4 的验收条目**（现在写的是「遗留 pending 审批全部标记 aborted」）。
4. **陈旧性重校验**——哈希只保证 payload 未变，**不保证世界未变**：工具可能已卸载、
   MCP server 已下线、目标资源已删除、schedule 已改。复原时必须重校验工具仍存在、
   并给审批项 TTL 与「这是 N 小时前排队的」提示。无法复原的仍标 aborted（兜底不能丢）。

### H5 self-wake（成本比原文写的高）

不只是四个工具：`ScheduleAction` 目前只有 `agent_run`（`agent24-protocol/src/types.rs:254`），
H5 要新增 wake 表 + **「向既有 session 投递后台消息」这个语义本身**。
scheduler 的 tick 已存在（`agent24-scheduler/src/lib.rs:202`），按上游的 `extra_tick` 位挂进去即可，
并继承关停契约：**停调度器时把 spawn 出去的 run 一并取消，挂起的 run 不得比调度器活得久**。

### H8 设计（2026-07-28，开工前锁定，值得专注一轮）

单 PR 可做但涉及协议 + loop + 新工具 + 审批四处，是 P3 里的大件。锁定方案：

- **触发**：`RunCreate.mode`（`"plan" | "normal"`，默认 normal；**加法协议** → 触发 `pnpm gen:api` 零漂移门）。client 请求进入 plan mode，记进 `RunInput.mode` 持久化。
- **只读强制（关键）**：agent loop 读 `run.input.mode`；plan 期**只 advertise `Read` 类工具 + `propose_plan`**——模型看不见 write/exec/external，就调不了（同 H9 explorer 的「结构性只读」，靠不 advertise 而非 dispatch 期拒）。
- **`propose_plan{plan}` 工具**：提交计划 → 经 broker 建「批准此计划」审批；**批准 → loop 内存 `plan_mode=false` → 后续 advertise 全量工具（退出只读）**；拒绝 → run 结束。mode flip 是 loop 内存态。
- **与 H3 durable resume 的耦合点**：plan-mode run 若 park 在计划审批上再崩溃重启，resume 必须知道它**曾在 plan mode**（否则复原后错误地拿到全量工具）——`RunInput.mode` 持久化 + resume 据此恢复 `plan_mode`。实现时务必接上，别让 resume 绕过只读门。
- **验收**：plan-mode run 只看得到 read+propose_plan（断言 adverts）；propose_plan 触发审批；批准后同一 run 能用写工具、拒绝则结束；gen:api 无漂移；与 durable resume 的只读恢复有测试。

### H10 落地（2026-07-27，紧接 E3）

建立在 E3 校验之上，三条 inviolable 全部到位：

1. **清单严格校验** —— E3 的 `validateManifest` 已在 `loadInstalledModule` 做安装门禁（畸形清单不注册）。
2. **安装后默认 disabled pending consent** —— 此前 `module-state` 默认 **enabled**。**关键修正（clestons #75 抓到）**：`registerCommunityModule` 会无条件 `startService`（执行容器 `startCmd` = 任意代码）+ `ensureModels`，路由级 `isEnabled` 门只挡 HTTP 访问、挡不住注册期的容器启动——所以带 `container` 的模块一装就跑了 startCmd。**修法**：先 `setEnabled(id,false)` **再** register；`startModuleServices`（models+container）抽出来，register/registerAll 都 **gate 在 `isEnabled`**；enable-toggle 才真正启动服务、disable 停容器（对称生命周期）；`setEnabled` 改返回布尔，install 持久化失败即回滚（fail-closed，堵住「持久化失败+重启→默认 enabled」的 fail-open）。e2e 测试断言 disabled 模块**不**启容器、enable 才启。内置模块默认 enabled 不受影响。
3. **安装绝不写 override** —— node-daemon 模块安装**没有任何路径**能触到 daemon 的 risk-override 存储（H2 的 inviolable 规则），架构上天然成立；已在 install handler 注释写明。模块只能*声明*想要什么权限，是否信任由用户决定。

**知情同意**：install 响应新增 `pendingConsent: true` + `consent`（`consentSummary`：id/name/type/permissions）；桌面 `ModulesManager` 卡片现在展示每个模块**声明的权限 chips**，用户在启用（=同意）前能看到它要什么。

**测试**：`consentSummary`（含缺 name 回退 id、过滤非字符串权限）+ E3 校验共 36 个 node-daemon 测试；桌面 85 测试 + 双 tsconfig typecheck 全绿。

### H7 为何降级

原文说它「几乎零风险」是错的。审批、状态转换与执行在 `ToolRegistry::dispatch()` 里是耦合的
（`agent24-tools/src/lib.rs:248`），`run_tool_call` 还会把 run 切到 `awaiting_approval`
（`agent24-agent/src/lib.rs:759`）。要并发就得先拆 `authorize()` / `execute()` 两阶段，
还要保证 tool result 按原调用顺序回填。收益（几个独立只读调用并行）在本地模型延迟面前不显著。
**H1 之后再评估，没有明确的慢场景就不做。**

### 明确不借鉴

- **13.4K 行手写连接器**（Slack/Gmail/GCal/GitHub/HubSpot 各自 OAuth + 地址簿 + 发送者归属）。
  注意它是**在已支持 MCP 的前提下**还手写了这些——说明 MCP 给不了那种产品级体验。
  这印证 E2 降级的判断，同时警告 M-F：**渠道成本大头在账号与寻址，不在协议**。我们只要一个微信 + 一个 Nostr。
- **前缀式 ProviderRouter**（`provider:model` 分发，不看成本/健康/隐私）——D2 的 ModelRouter 比它成熟，不要回退。
- **JSON 整文件重写当存储**——我们 sqlx/SQLite 已更对，H1–H12 一律建表。
- **3505 行的 manager.py God object**——与 openfang 的 kernel 同病。
