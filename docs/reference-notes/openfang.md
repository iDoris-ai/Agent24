# 研读笔记：OpenFang — "Agent Operating System" 整体架构

> 来源：`vendor/reference/openfang/`（RightNow-AI/openfang v0.6.9，Apache-2.0 OR MIT，本地只读克隆）
> 日期：2026-07-23 | 用途：Agent24 M-B/M-C 设计输入（ADR-026）
> 所有 `path:line` 相对 `vendor/reference/openfang/`。

## 0. 一句话定位 + 规模

- "The Agent Operating System"——不是 chatbot 框架，是**运行自主 agent 的操作系统**。核心卖点正是 Agent24 想要的：**24/7 常驻、按 schedule 自主跑、构建知识图谱、监控目标、结果推到 dashboard**。全系统编译成**单个 ~32MB 二进制**（`openfang start` 起 daemon，dashboard 在 `:4200`）。
- 规模：14 crates，约 **199K 行 Rust**（含测试）。runtime 51.7K、channels 33K（48 个 IM 平台）、cli 30.9K（含 ratatui TUI）、kernel 20.5K、api 20.4K、types 13.8K、skills 5K、memory 4.5K。
- 技术栈对照 Agent24：tokio(full) + axum 0.8 + **rusqlite（不是 sqlx）** + **rmcp 1.2** + async-trait + governor（限流）+ ratatui + wasmtime（基本未用）+ ed25519-dalek（skill 签名）。**与 Agent24 计划最大差异：存储用同步 rusqlite + `spawn_blocking` 假 async，而非 sqlx。**

## 1. Crate 依赖图

```
openfang-types      ← 零依赖叶子（所有公共类型/schema）
openfang-memory     ← types            （rusqlite 存储 substrate）
openfang-wire       ← types            （OFP 点对点网络）
openfang-channels   ← types            （48 个 IM 平台驱动）
openfang-hands      ← types            （特化长驻子 agent 注册表）
openfang-skills     ← types            （skill 加载/marketplace/签名）
openfang-migrate/extensions ← types
openfang-runtime    ← types, memory, skills
                      ★ 通过 KernelHandle trait 反向调用 kernel，避免循环依赖
openfang-kernel     ← types,memory,runtime,skills,hands,extensions,wire,channels
                      （God-object OpenFangKernel）
openfang-api        ← kernel + 全部（axum）
openfang-cli        ← kernel,api,...（main + TUI）
openfang-desktop    ← kernel,api,types（Tauri 托盘）
```

关键解耦：**runtime 不依赖 kernel**，而是定义 `KernelHandle` trait（`runtime/src/kernel_handle.rs`，270 行），kernel 实现并注入。Agent24 保持 `agent24-core` 零框架依赖可直接照搬这个手法。

## 2. Agent Loop（`runtime/src/agent_loop.rs`，5493 行）

- **形态：不是可恢复的 step 函数，是两个巨型 async 函数**——`run_agent_loop`（`:293-1146`）非流式、`run_agent_loop_streaming`（`:1520-2364`）流式孪生版（多 `stream_tx: mpsc::Sender<StreamEvent>`）。**22 个参数**全按参数注入不挂 struct。
- 返回 `AgentLoopResult{response, total_usage, iterations, cost_usd, silent, directives}`（`:249`）。
- 常量（`:35-90`）：`MAX_ITERATIONS=50`（可被 manifest 覆盖）、`MAX_RETRIES=3`、`MAX_CONTINUATIONS=5`、`DEFAULT_CONTEXT_WINDOW=200_000`。
- 迭代体（`:506-1124`）每轮：① overflow 4 阶段恢复 → ② 压缩超大 tool result → ③ 构 request → ④ 心跳 → ⑤ `call_with_retry`（含 provider cooldown 熔断）→ ⑥ 文本型 tool-call 回收（Groq/Llama 正文吐 `<function=…>`）→ ⑦ `match stop_reason`：EndTurn（解析 directives / NO_REPLY 早退 / **phantom-action 检测** / 存 session / 带 embedding 记忆）/ ToolUse（dedup → LoopGuard 熔断 → BeforeToolCall hook → **timeout 包裹 execute_tool** → AfterToolCall → 动态截断 → tool result 作 User 消息回填）/ MaxTokens（追加 "Please continue"）。
- **⚠️ 该避开：没有 CancellationToken**。全文件 grep 不到 cancel/abort。取消是协作式：stream 消费端断开只 log 然后**继续跑**（`:1734`）。硬停只来自 per-tool timeout、LoopGuard 熔断、MAX_ITERATIONS、cooldown 拒绝。**Agent24 若要支持"用户中途打断跑了 5 分钟的任务"，必须一开始就把 CancellationToken 织进 loop——这是 openfang 最大结构性缺陷。**
- Context 压缩分层：循环前 history 截断 → 每轮 `recover_from_overflow` → `apply_context_guard` + 动态截断 → 完整 LLM 摘要 `compact_session`（kernel 外部触发）。

## 3. Model Provider 抽象（重点对照 Agent24 `ModelProvider`）

### LLM driver trait 逐字（`runtime/src/llm_driver.rs:146-172`）

```rust
#[async_trait]
pub trait LlmDriver: Send + Sync {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError>;
    async fn stream(&self, request: CompletionRequest,
        tx: tokio::sync::mpsc::Sender<StreamEvent>,
    ) -> Result<CompletionResponse, LlmError> {
        // 默认实现: 退化为 complete() + 一次性 emit 全部
        let response = self.complete(request).await?;
        let text = response.text();
        if !text.is_empty() { let _ = tx.send(StreamEvent::TextDelta { text }).await; }
        let _ = tx.send(StreamEvent::ContentComplete {
            stop_reason: response.stop_reason, usage: response.usage }).await;
        Ok(response)
    }
}
```

**建议 Agent24 照抄的点**：trait 极小（只 complete + defaulted stream）；所有编排（retry/cooldown/fallback/routing/压缩/tool loop）全压在 trait 之上的 loop 里，driver 只见 request→response；`&self`+`Send+Sync`，全 `Arc<dyn>`；流式用 push 模型（owned mpsc Sender）同时返回聚合；`system` 提为独立字段，`thinking` 一等字段。**⚠️ 签名没 cancellation token——Agent24 应加。**
`LlmError`（`:12`）区分 `RateLimited{retry_after_ms}` / `Overloaded{retry_after_ms}`（自带重试延迟）/ Billing / Auth / ModelNotFound。

### Driver 注册/分发：手写 if/else 工厂，无 enum 无 registry（⚠️ 可改进）

`create_driver(config)`（`drivers/mod.rs:333`）一条线性 `if provider=="anthropic"{}else if "gemini"{}…`（`:337-597`），default 落到 `provider_defaults` 大 match，复用 `OpenAIDriver` 服务所有 OpenAI 兼容后端（groq/openrouter/deepseek/ollama/vllm/lmstudio…）。扩展只能改代码。Agent24 用 registry map 更干净。

### 模型路由：两套独立机制

- (a) **复杂度启发式路由**（`routing.rs`）：`TaskComplexity{Simple,Medium,Complex}`，`score()` 按输入 token / tool 数 / 代码标记 / 对话深度加权 → 映射 `{simple,medium,complex}_model`。纯输入形状启发式，不看成本/健康。对应 Agent24 想要的"本地小模型 vs 远程分层"，但 **openfang 默认没启用它**——loop 实际直接用 `manifest.model`。
- (b) **per-agent 静态配置 + fallback 链**（实际路径）：`ModelNotFound` 时按 `manifest.fallback_models` 逐个换 driver（仅 ModelNotFound 触发，限流不触发）。另有 driver 级 `FallbackDriver` 对任意错误降级。
- (c) **健康/熔断**：`provider_health.rs` 只探活（60s TTL 缓存）+ `is_local_provider`（ollama/vllm/lmstudio）。真熔断在 `auth_cooldown::ProviderCooldown`：调用前 `check(provider)` → Reject/AllowProbe/Allow，billing 错误加重熔断。

> **启示**：openfang 把"本地/远程分层"做成未接线的启发式路由器，实际靠 per-agent 手配 + fallback 兜底。Agent24 应把路由决策做成一等公民（`ModelRouter::select(task_profile)->ProviderChoice`），并把 cooldown/health 反馈喂进路由，别学它两套机制割裂。

## 4. 调度器 / Cron / 常驻 daemon

### Cron：用真 `cron` crate v0.16 + chrono-tz（非手写）

`CronScheduler`（`kernel/src/cron.rs:75`）：`jobs: DashMap<CronJobId,JobMeta>` + `persist_path`（`<home>/cron_jobs.json`）。

- 持久化：JSON 原子写（写 `.tmp` 再 rename）。
- **tick loop**（`kernel.rs:4657-4704`）：`interval` **每 15 秒**，`MissedTickBehavior::Skip`（长任务不补发）。每 tick 查 shutdown → `due_jobs()` → `cron_run_job()` → 每 20 tick 持久化。
- **防重复**：`due_jobs()` 返回到期 job 时**立即 pre-advance `next_run`**。
- 时区：5/6 字段表达式转 cron crate 的 7 字段；有 `tz` 按 `chrono_tz::Tz` 算本地再转 UTC 存；非法退 UTC/`now+1h`。
- 失败：连续 5 次自动禁用。**重启存活**：`reassign_agent_jobs` 在 agent 重启拿新 UUID 时重映射 job。

### ⚠️ 四类"定时/触发"要分清

| 子系统 | 文件 | 触发源 | 触发什么 |
|---|---|---|---|
| Cron scheduler | cron.rs | 挂钟（At/Every/Cron）15s tick | SystemEvent/AgentTurn/WorkflowRun 打到指定 agent |
| Agent ScheduleMode 自循环 | background.rs | per-agent 间隔自我 prompt | 注入 `[AUTONOMOUS TICK]`（只认 `"every <N>[smhd]"` 手写非真 cron） |
| Triggers | triggers.rs | EventBus 匹配事件 | 给订阅 agent 发模板 prompt |
| Auto-reply | auto_reply.rs | 入站 channel 消息 | 跑 agent turn 回复原 channel |

**命名坑**：`scheduler.rs` 的 `AgentScheduler` **不是时间调度器，是资源/配额追踪器**（`max_llm_tokens_per_hour` 滚动窗口）。Agent24 命名要避开。

### Cron job schema（`types/src/scheduler.rs:214`，可直接借鉴）

`CronJob{id, agent_id, name, enabled, schedule: At{at}|Every{every_secs:60..=86400}|Cron{expr,tz}, action: SystemEvent|AgentTurn{message,model_override,timeout}|WorkflowRun, delivery, delivery_targets: Vec<Channel|Webhook|LocalFile|Email>, last_run, next_run}`。`validate()` 强制 `MAX_JOBS_PER_AGENT=50`。扇出投递用 `futures::join_all` 并发 best-effort。

### Daemon 生命周期

- **God-object** `OpenFangKernel`（`kernel.rs:60-183`）所有子系统作自有字段。`self_handle: OnceLock<Weak<OpenFangKernel>>` Arc 化后回填弱自引用。
- Boot：`start_background_agents`（`:4390`）依次 spawn：交错启 agent 循环（500ms 错开防限流）、heartbeat、OFP peer、本地 provider 探活、usage 清理（24h）、memory consolidation、MCP、扩展健康、workflow 加载、cron tick（15s）、A2A 发现、WhatsApp 网关。每个循环顶部查 `supervisor.is_shutting_down()`。
- Shutdown 信号：`Supervisor`（`supervisor.rs:10`）用 **`tokio::sync::watch::Sender<bool>`** 一对多广播。
- 优雅关闭：`graceful_shutdown.rs` `ShutdownCoordinator` **10 阶段有序状态机**（Running→Draining→…→ClosingDatabase→Complete），`AtomicBool::swap` 幂等，默认 drain30s/agent60s/total120s。
- 崩溃恢复 FSM（heartbeat）：30s tick，`inactive>interval*2` 判无响应 → Crashed → 复位 Running（最多 3 次 cooldown60s）→ 耗尽 Terminated。
- Event bus（`event_bus.rs`）：**tokio broadcast pub/sub**（全局 cap1024 + per-agent cap256）+ 1000 环形历史，全 fire-and-forget。

## 5. 记忆层（`crates/openfang-memory/`）

- 后端 rusqlite（bundled）单 `Arc<Mutex<Connection>>` 全局共享，WAL；**async 是假的**（SQL 跑 spawn_blocking）。可选 HTTP 后端（PG+pgvector）仅 semantic 走。
- 六 store 组成 `MemorySubstrate`：Structured(KV) / Semantic(向量) / Knowledge(实体图) / Session(per-channel + **canonical 跨渠道**) / Consolidation / Usage。
- **向量搜索有但朴素**：embedding 存 BLOB，recall 取 max(limit*10,100) 候选 **Rust 里暴力 cosine 重排**，无 ANN。⚠️ Agent24 上规模需真 ANN（hnsw/usearch）或 pgvector。
- Consolidation：单 SQL 把 7 天未访问记忆 confidence 衰减，**尚无合并**（Phase 1）。
- schema v8，表含 canonical_sessions、**audit_entries（prev_hash+hash Merkle 链）**。
- **⭐ 最值得抄**：canonical cross-channel session（每 agent 一条持久 session，超阈值把旧消息压成 `compacted_summary`，支持 LLM 摘要替代粗暴截断，`session.rs:322-437`）；审计 Merkle 链防篡改。

## 6. Skills / MCP

- **Skill**：带 `skill.toml` 或 `SKILL.md`（YAML frontmatter+markdown，加载时自动转）。`SkillRuntime`：Python/Wasm（未实现）/Node/Shell/Builtin/**PromptOnly（默认主流）**——无代码只把 markdown 注入 system prompt。
- 加载时跑 prompt-injection 扫描并 **BLOCK** 危险 skill。代码型执行 spawn 子进程走 stdin/stdout，**关键安全：`env_clear()` 后只回填 PATH/HOME**，第三方读不到宿主 API key。
- 供应链：**Ed25519 签名**（`types/manifest_signing.rs`）+ 安装器 `require_signed` + `allowed_signer_keys` pinning + SHA256 checksum + 启发式扫描。
- **MCP 完全依赖官方 rmcp 1.2**：client（`runtime/src/mcp.rs`）`McpTransport{Stdio,Sse,Http(Streamable)}`，**工具命名空间 `mcp_{server}_{tool}`**，安全：stdio env_clear+白名单、HTTP `check_ssrf` 挡 169.254.169.254。server（`mcp_server.rs`）把自己 tool 暴露给外部 MCP client，挂 `POST /mcp`。
- **内置工具 dispatch（`tool_runner.rs`，5014 行）：一个巨型 `match tool_name`**（⚠️ 非 registry）。dispatch 前流水线：normalize 别名 → **capability 校验** → **approval 门** → match。约 65 个内置工具。`other=>` 兜底两级：先 MCP 再 skill 工具。**⚠️ tool 定义与 dispatch match 是两份手工同步的平行清单**——Agent24 用 registry trait 可消除耦合。

## 7. HTTP/WS API（`crates/openfang-api/`）

- axum 0.8，`server.rs` 返回 `(Router, Arc<AppState>)`。路由注册在 server.rs，handler 在 routes.rs（12975 行）。
- 中间件（外→内）：auth（Bearer/X-API-Key，subtle 常量时间比较）→ gcra_rate_limit（governor GCRA 按 IP 500/min）→ security_headers → logging → Compression → Trace → CORS（显式 origin）。
- 路由几百条：agents 全生命周期（含 `/message/stream` SSE、`/ws`）、approvals、skills/clawhub、hands、channels、triggers/schedules/cron/workflows、config/models/providers、memory/kv、usage/budget/audit/metrics、a2a（含 `/.well-known/agent.json`）、`POST /mcp`、**OpenAI 兼容 `/v1/chat/completions` + `/v1/models`**（model 映射到 agent id）、webchat SPA。
- **WS 协议**（`ws.rs`，2028 行）：JSON `"type"` 判别，**手解析 Value 非 Rust enum**（⚠️ 该避开，Agent24 用强类型 serde-tag enum）。S→C：typing/text_delta(debounce)/response(带 token 统计)/error/silent_complete/canvas。升级 3 种鉴权：Bearer/`?token=`/cookie。

## 8. 权限与安全：四层独立（非统一系统）

1. **Capability**（`types/capability.rs` + `kernel/capabilities.rs`）：`Capability` 枚举（FileRead/Write(glob)、ToolInvoke/ToolAll、LlmMaxTokens、AgentSpawn、ShellExec…），支持 glob 与数值 `>=`。**`validate_capability_inheritance(parent,child)` 防子 agent 提权**——Agent24 做 subagent 可抄。`CapabilityManager` = `DashMap<AgentId,Vec<Capability>>`，创建时授予、设计不可变。
2. **Approval**（human-in-the-loop）：require-list 工具触发时创建 `ApprovalRequest` **阻塞 agent 等 oneshot**。`request_approval` = insert + `timeout(timeout,rx)`；`/api/approvals/{id}/approve` 走 resolve 送 decision。默认 `require_approval=["shell_exec"]`。策略热重载。
3. **Tool policy**（glob allow/deny，`runtime/tool_policy.rs`）：**deny-wins**，有 allow 则必须命中其一。**深度感知**：subagent(depth>0) 剥离 admin 工具，叶子 agent 剥离 spawn/kill。
4. **Taint 追踪**（`types/taint.rs`）：格子式信息流控制，`TaintLabel{ExternalNetwork,UserInput,Pii,Secret,UntrustedAgent}`。**只是 types 层原语，sink 处强制尚未全接线**。

Config：`types/config.rs` ~161KB 根是 `KernelConfig`，TOML，重度 `#[serde(default)]` + `default_*` fn + 显式 `Default`（三处都要加否则编译失败），支持 `include=[]` 深合并 + 热重载。

## 9. 对照 Agent24 计划架构

| 维度 | openfang | Agent24 计划 | 评价 |
|---|---|---|---|
| 存储 | rusqlite 同步+spawn_blocking | **sqlx** | Agent24 更对（原生 async+编译期检查） |
| core 解耦 | KernelHandle trait 反调 | agent24-core 零依赖 | 一致，借鉴 KernelHandle |
| Provider trait | LlmDriver 极小 | ModelProvider | 一致理念，**建议加 cancellation** |
| Provider 注册 | 手写 if/else | registry | Agent24 map 更好 |
| MCP | rmcp client+server | rmcp | 一致 |
| agent loop | 巨型 async fn 无 cancel | — | **必须织入 CancellationToken** |
| tool dispatch | 巨型 match + 平行定义 | — | **用 Tool trait + registry** |
| WS 协议 | 手解析 Value | — | **用强类型 serde-tag enum** |
| 模型分层 | 启发式 router 默认未启用 | 本地+远程分层 | **做成一等公民并喂 health/cooldown** |
| 向量记忆 | 暴力 cosine 无 ANN | — | 上规模需真 ANN |
| kernel | God-object 9415 行 | — | 谨慎，建议按子系统拆 trait |

## 10. 对 Agent24 M-B/M-C 的 5 条建议

1. **Cancellation 一等公民（M-B 必做，openfang 最大教训）**：agent loop 从第一行接 `tokio_util::sync::CancellationToken`，每轮迭代和每个 tool 都 `select!` 它；`ModelProvider::stream` 也接 cancel。openfang 事后补不上。
2. **Cron 抄成熟做法、命名避坑**：用 `cron` crate + chrono-tz；job 存并发 map + 原子 rename；`interval` + `MissedTickBehavior::Skip`；**到期立即 pre-advance next_run 防重复**；连续失败 N 次自动禁用；重启用 reassign 而非 orphan。**把"挂钟 cron / per-agent 自主循环 / 事件 trigger / 资源配额"四概念命名清晰隔离**（openfang 的 scheduler 实为配额器是反例）。
3. **ModelProvider trait 保持极小 + 路由/熔断上移，但把分层路由真正接线**：trait 只留 complete + defaulted stream（+cancel）；把 `ModelRouter::select(TaskProfile)->ProviderChoice` 做成实际生效组件（本地 3B/7B 优先，失败/超载降级远程），并把 health+cooldown 反馈喂回路由——别学 openfang 留成没人调的启发式类。
4. **Tool 用 registry trait 别用巨型 match**：`trait Tool{fn definition()->ToolDefinition; async fn call(ctx,input)->ToolResult}` 注册进 `HashMap<String,Arc<dyn Tool>>`，MCP/skill 工具同接口动态注入，消除"定义+dispatch 两份手工同步"耦合。dispatch 前流水线（normalize→capability→approval→执行）值得照搬。
5. **记忆层借鉴两亮点 + 权限两点**：canonical cross-channel session（超阈值 LLM 摘要压缩）和 Merkle 审计链；用 sqlx 原生 async，向量上规模换真 ANN。权限上 **capability 继承校验（防提权）** 和 **tool-policy 深度剥离 admin 工具** 在多 agent/subagent 场景很实用，M-C 做多 agent 时接入。

## 关键文件锚点

| 主题 | 位置 |
|---|---|
| LLM trait | `runtime/src/llm_driver.rs:146` |
| agent loop | `runtime/src/agent_loop.rs:293` |
| driver 工厂 | `runtime/src/drivers/mod.rs:333` |
| cron | `kernel/src/cron.rs:75` + tick `kernel.rs:4657` |
| cron schema | `types/src/scheduler.rs:214` |
| KernelHandle | `runtime/src/kernel_handle.rs` |
| memory substrate | `memory/src/substrate.rs:30` |
| canonical session | `memory/src/session.rs:322` |
| MCP client | `runtime/src/mcp.rs:73` |
| tool dispatch | `runtime/src/tool_runner.rs:109` |
| axum server | `api/src/server.rs:40` |
| approval | `kernel/src/approval.rs:18` |
| capability 继承 | `types/src/capability.rs:171` |
| 优雅关闭 | `runtime/src/graceful_shutdown.rs` |
