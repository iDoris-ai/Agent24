# Sin90 ↔ Pet0 集成约定（权威版）

> 文档类型：跨仓库集成契约（Integration Contract）
> 权威源：本文件（`iDoris-ai/Agent24:docs/SIN90-PET0-INTEGRATION.md`）
> 镜像：`tools/Pet0:docs/AGENT24_INTEGRATION.md`（内容同源，仅视角前置不同）
> 状态：草案 v0.2 · 待双方确认后据此各自开发接口模块
> **对齐**：`specs/SIN90-domain.md` v0.3 · `protocol/events.schema.json`
> 最后更新：2026-09-03
>
> **本文件不复述状态机与事件字段**，只引用权威源。上一轮复审发现的漂移全部集中在
> 「机制描述段」——凡是复述过来的，都比来源先过期；而两边各改一次不会有任何东西报错。
> 所以状态机看 [`specs/SIN90-domain.md`](specs/SIN90-domain.md) §2.2，事件信封看
> `protocol/events.schema.json`，本文件只说**谁跟谁怎么接**。

---

## 0. 一句话

**Pet0 是建在 Agent24 之上的第一个垂直产品(桌面伴侣 + Personal OS);Agent24 提供内置基础领域模型 Sin90 与全部底层机制,Pet0 只做壳、感官与产品语义,通过 `agent24d` 的本地 HTTP/WS 消费 Sin90,绝不重写地基。**

---

## 1. 为什么可行(两边架构 DNA 同源)

Pet0 写进文档的 6 条硬约束,Agent24 的 Rust core 已用代码实现每一条:

| Pet0 原则 | Agent24 现成实现 |
|---|---|
| Database is source of truth, AI is not | 同一套地基(sqlx SQLite + `BEGIN IMMEDIATE` + 迁移矩阵校验),但 Sin90 用**自己的** `sin90.db`,不写 `agent24-store` |
| AI 输出只是 Proposal,落库必过确定性校验 | `agent24-policy`:fail-closed 审批门(**只读**复用其放行判定);事务在模块自己的 `sin90.db` 里 |
| Core 不依赖具体模型,只依赖 IntelligenceProvider | `agent24-core` 按 ADR-026 只依赖 protocol+thiserror;`agent24-models` 是最小 `ModelProvider` trait |
| 每次状态变更都产生事件 | `EventSink` + append-only 审计(hash-chain) |
| Router 每次路由决策都记账 | store 的 hash-chained audit log |
| 换掉 Codex 不改 Core 一行 | `agent24-models`:routing/health 在 trait 之上,provider 可插拔 |

结论:这不是表面相似,是同一套地基约束。Pet0 可以、且应该坐在 Agent24 上。

---

## 2. 边界:壳与核之间是一道本地进程边界

**关键事实:`agent24d`(Rust daemon)在 `127.0.0.1` 上跑 HTTP + WebSocket(bearer token 握手、动态端口)。壳通过本地 HTTP/WS 与它对话——这正是 Pet0 要的「表现层只发 Event、只订阅 State」。**

因此 **Tauri vs Electron 不是冲突**:壳用什么技术与后端完全解耦。

```
┌─────────────────────────────┐
│  Pet0 Shell (Tauri 或复用 Electron) │  表现层 + Reflex(FSM) + 感官
│  透明窗 / 穿透 / 动画 / 气泡 / 语音     │
└───────────────┬─────────────┘
        本地 HTTP + WS (agent24d)      ← 唯一集成面
┌───────────────▼─────────────┐
│  agent24d  (Rust daemon)          │
│  ┌─────────────────────────┐  │
│  │  Sin90 领域(内置)         │  │  Direction/Rhythm/Week/Task/Review/AttentionBudget
│  ├─────────────────────────┤  │
│  │ store · core · policy ·    │  │  事务状态机 / Proposal 门 / 事件日志
│  │ scheduler · memory · models │  │  cron / KV+session / 可配置 provider 注册表
│  │ · mcp                       │  │
│  └─────────────────────────┘  │
│  SQLite(唯一事实来源,本地)        │
└─────────────────────────────┘
```

壳技术选型只看**分发体积 vs 复用现有壳代码**:桌宠 to-C 铺量、体积敏感 → 倾向 **Tauri 壳 + agent24d 后端**;想复用现有 UI/IPC → 复用 `apps/desktop` 的 Electron 壳。**此决定不影响本约定的任何其它条款。**

---

## 3. Sin90 —— Agent24 内置基础领域模型

Sin90 是 Agent24 提供的 **Personal-OS 领域模型**,以**内核之上的可加载模块**形态落地(纯域 crate `agent24-sin90` + 自带 store `agent24-sin90-store`,**独立 DB `sin90.db`**),依赖单向(Sin90→内核,内核绝不反向依赖)。它给 Pet0 的是**领域实体 + 状态机 + 事件 + Proposal 门**;`agent24-core` 现有的状态机是 agent-run 语义,Sin90 补齐 Personal-OS 语义。边界与依赖方向详见 [SIN90-domain.md §0](specs/SIN90-domain.md)。

### 3.1 实体与状态机

命名沿用 core 既有约定:每个实体一对 `<entity>_transition_allowed(from,to) -> bool` + `check_<entity>_transition(from,to) -> Result<(),TransitionError>`,落库前强制校验。

| 实体 | 说明 |
|---|---|
| **Direction** 长期方向 | 月/季度级方向 |
| **Rhythm** 节奏 | 某 Direction 下的目标时间占比(如 Coding 60%) |
| **Week** 周容器 | 一周计划的生命周期 |
| **Task** | carried_over 生成下周新 Task 并留链 |
| **ScheduleBlock** 时间块 | 计划的执行块,实际由事件对账 |
| **Review** 复盘 | daily / weekly / rhythm 三型 |
| **AttentionBudget** 注意力预算 | (非状态机,物化视图)按 Direction 的 planned vs actual,纯事件回放算出 |

> **迁移矩阵在 [`specs/SIN90-domain.md`](specs/SIN90-domain.md) §2.2,以已合并的
> `agent24-sin90/src/transitions.rs` 为准。本文件不复述。**
>
> 上一版这里放了一张简化的主干链表格,而已合并的矩阵比它多 8 条边。其中
> **`adjusted → adjusted` 不是省略,是语义**:照旧表实现,Pet0 会以为一个 rhythm
> 一辈子只能调整一次。这类错误在「复述」里必然复发,所以整表改成引用。

### 3.2 事件与对账(Sin90 的灵魂)

- `sin90_events` append-only。任何 Sin90 状态变更**必须**产生事件;无事件的状态变更视为 bug。
- `sin90_attention_daily` 为物化视图,由事件回放增量更新。
- **SPIKE-00 判定**:`GET /api/v1/sin90/attention?window=week` 必须能纯从事件回放算出「本周 Coding 18h / Business 2h」,不依赖任何对话上下文。

### 3.3 Proposal 门(AI 不写库)

AI(本地脑或 Codex)产出的一切都是 `Sin90Proposal`,经确定性校验后转事务落库并产事件。

**事务在 `sin90.db` 里,不在 `agent24-store`。** proposal 的状态、审批与 apply 全落
模块自己的库、走单个事务;内核的 `agent24-policy` 只被**只读**查询(standing-grant
放行判定),不写审批行、不参与事务。跨库 approval + apply 正是 `SIN90-domain.md` §9
第 1 条(Critical)要消灭的形状 —— 照旧稿的「复用 `agent24-store` 事务」实现,
等于把它复刻回来。

```
AI 输出 → Sin90Proposal → 确定性校验(schema + 状态机) → 事务写入 + 产事件
                              │失败
                              └→ 拒绝 / 降级到 Rule / 问用户
```

### 3.4 Three-Brain 路由归属

| 脑 | 归属 | 说明 |
|---|---|---|
| **Reflex**(桌宠 FSM/规则) | **Pet0 壳** | 走路/点击/拖拽/quiet-hours,零模型,不过网络 |
| **Local**(Qwen3-0.6B 意图分类等) | **Agent24**(`agent24-models` + oMLX provider) | 受约束 JSON 输出,schema 校验,失败降级 |
| **Executive**(Codex 规划/反思) | **Agent24**(OpenAI-兼容 provider,Codex 插进来) | 只在需理解/权衡/规划时调用 |

Agent24 提供三级路由 policy 层 + `sin90_ai_calls` 记账(每次决策记录 engine 与 `fallback_from`)。

---

## 4. 职责划分(据此各自开发)

### 4.1 Agent24 做(我们)

1. **`agent24-sin90` 领域 crate**:§3 的实体 / 状态机 / 事件日志 / Proposal 门(自带 `agent24-sin90-store`,独立 `sin90.db`;只**只读**用内核 policy 的放行判定)。
2. **可配置模型网关**:`agent24-models` provider 注册表——Executive(Codex/OpenAI 兼容)+ Local(GGUF/MLX 经 oMLX)+ 三级路由 policy + `sin90_ai_calls` 审计。
3. **调度**:`agent24-scheduler`(现成)接 Sin90 的 Rhythm / Nudge 触发(cron/every/at,防重放)。
4. **记忆**:`agent24-memory`(现成)作 Sin90 记忆底座(L0 KV + session 压缩)。
5. **MCP 适配**:`agent24-mcp`(现成),供 M3 集成(Calendar/GitHub/…)走同一 dispatch + 审批门。
6. **API 面 + 客户端**:`agent24d` 挂载 Sin90 的路由(§5)、WS 走通用 `module` 信封(§5);`@agent24/api-client` 出 typed client 供壳消费。
7. **契约与版本**:Sin90 API 的 schema 版本化,变更走 ADR。

### 4.2 Pet0 做(Pet0)

1. **桌面壳**:Tauri(或复用 Electron)——透明窗、点击穿透、多显示器、托盘。
2. **桌宠表现 + Reflex**:精灵图/动画/FSM/气泡、`.petpack` 格式(Reflex 脑在此)。
3. **感官/语音链路**:VAD/KWS/STT/TTS(sherpa-onnx),在边缘壳侧。
4. **Nudge 呈现**:触发是 Sin90 scheduler 的 Rule,**措辞与渲染在壳**(quiet hours/focus 状态壳侧遵守)。
5. **Onboarding / first-run**:引导用户经 Sin90 API 建立第一个 Direction。
6. **只经 agent24d 消费 Sin90**:本地 HTTP/WS,**绝不直连 DB、绝不重写 store/event/proposal/scheduler/model-gateway**。
7. **产品语义输入**:Sin90 实体字段与状态机由 Pet0 作为领域专家与 Agent24 共定,**实现权归 Agent24**。

### 4.3 共同边界(不可协商)

- 壳↔核 = agent24d 本地 HTTP + WS;壳**发命令(经校验、可被拒),只订阅事件**——从不直接写既成事实(事实只由核在校验后产生,并以 `module` 信封回推,见 §5)。
- **离线优先是硬指标**:除 Executive 脑外,桌宠/FSM/本地脑/录入/检索/对账全部断网可用。
- AI 不写库,一切经 Proposal。
- 每次状态变更产事件;每次路由决策记账。
- schema 变更必须有迁移,用户 DB 不靠重建。

---

## 5. Sin90 API 面(agent24d 新增,契约草案)

沿用现有 `/api/v1/*` + bearer + WS 约定。全部 `/api/v1/sin90/` 前缀:

```
# 领域实体(GET 列表 / POST 建 / GET 单个 / PATCH 迁移状态)
GET|POST         /api/v1/sin90/directions
GET|PATCH        /api/v1/sin90/directions/{id}      # PATCH body = {to: <status>, ...},走状态机校验
GET|POST         /api/v1/sin90/rhythms
                 # 注意:Rhythm **没有** PATCH /{id}。它的变更不是状态迁移,是重新
                 # 分配占比,必须经 Proposal 门的 `AdjustRhythm{rhythm_id,new_alloc}`
                 # (见 specs/SIN90-domain.md §2.3)。这也是 adjusted → adjusted 那条边
                 # 的用途:同一个 rhythm 可以反复调,每次都留一条 proposal 记录。
GET|POST         /api/v1/sin90/weeks
GET|PATCH        /api/v1/sin90/weeks/{id}
GET|POST         /api/v1/sin90/tasks
GET|PATCH        /api/v1/sin90/tasks/{id}
GET|POST         /api/v1/sin90/schedule-blocks
GET|PATCH        /api/v1/sin90/schedule-blocks/{id}
GET|POST         /api/v1/sin90/reviews
GET|PATCH        /api/v1/sin90/reviews/{id}

# Proposal 门(AI 产出 → 用户确认 → 落库)
POST             /api/v1/sin90/proposals            # 提交一个 Sin90Proposal
POST             /api/v1/sin90/proposals/{id}/accept
POST             /api/v1/sin90/proposals/{id}/reject

# 对账 / 注意力预算(SPIKE-00 判定面)
GET              /api/v1/sin90/attention?window=week # planned vs actual,纯事件回放

# 事件流(复用现有 WS)
GET  /api/v1/events
```

**事件信封是通用的,不是 `sin90.*`。** 领域模块触达 WS 流只有一条缝:`type` 恒为
`"module"`,模块名与模块自己的事件名在 payload 里。客户端按 `payload.module` +
`payload.kind` 分发:

```jsonc
{
  "type": "module",
  "payload": {
    "module": "sin90",                  // 哪个领域 OS
    "kind": "task.transitioned",        // 模块自己的命名空间,内核不解释
    "payload": { /* 模块自定义 */ }
  }
}
```

> 权威定义在 `protocol/events.schema.json` 的 `ModuleEventPayload`(必填
> `module` / `kind` / `payload`)与 `rust/crates/agent24-protocol` 的
> `EventBody::Module`。**本文件不复述字段** —— 复述正是这份契约上一轮漂移的根因。
>
> ⚠️ 早期草案写的是 `sin90.direction.created` 这类**顶层**事件类型。按那个形状实现
> 的客户端**一条也匹配不到**。若你手上的 Pet0 代码在 `type` 上比对 `sin90.*`,
> 那是照旧稿写的,需要改成上面的两级分发。

Reflex(壳内 FSM)不走这些接口;它只在需要落库时(如「用户完成了任务」)发一条 Sin90 **命令**(可被拒)。壳从不写既成事实。

---

## 6. Pet0 需要砍掉的自建

原 Pet0 计划里以下**改为复用 Agent24,不再写第二遍**:

- M1 W1 内核:SQLite 建库/迁移框架/事件回放/事务化 Proposal → 用 `agent24-sin90` 模块(自带 store,独立 `sin90.db`;**不是** `agent24-store`)。
- M2:`IntelligenceProvider` 抽象 / Intelligence Router 基础设施 / MCP 接入 → 用 `agent24-models` + `agent24-mcp`。
- 调度/Nudge 触发底座 → 用 `agent24-scheduler`。

Pet0 保留自有:桌宠 FSM/动画/petpack、语音链路、Nudge UX、以及 Sin90 领域语义的**共定输入**。

---

## 7. 一个哲学差异(划清)

Agent24 的「魂」含 Nostr/联邦/多渠道(`nostr-bridge`/`wechat-bridge`/双模式)——它是多渠道 agent 框架。Pet0 刻意单用户、local-first、可离线、M1/M2 不做多 Agent/云同步。

兼容点:agent24d 本来就是 `127.0.0.1` 本地 + 本地 SQLite,**离线优先天然成立**。Pet0 只用 Agent24 的一个子集,不碰联邦那几个独立 crate 即可,互不拖累。

---

## 8. 第一个联合里程碑:SPIKE-00(验证这桩婚事)

**目标**:用一个 Spike 证明 Pet0 能坐在 Sin90 上。

- **Agent24 交付**:`agent24-sin90` 最小 schema(Direction/Rhythm/Task/ScheduleBlock + `sin90_events`)+ `GET /api/v1/sin90/attention` + `POST /proposals` 全链。
- **判定**:`attention` 能纯从事件回放算出「本周 Coding 18h / Business 2h」,数据全来自事件,不来自对话。
- **Pet0 交付**:一个一次性壳,经 API 建一个 Direction、渲染对账结果。
- **绿 → 进 M1 正式;红 → 先改方案,不带未验证假设往下走。**

后续里程碑对齐 Pet0 的 M1/M2/M3,但内核任务替换为「对接 Sin90 API」。

---

## 9. 归属与流程

- **Sin90 规格权威**:Agent24 `docs/specs/`(实现)+ 本约定(接口面)。
- 本约定两仓库镜像;任何条款变更先改本权威版,再同步 Pet0 镜像,重大变更走 ADR。
- 领域字段/状态机的产品语义:Pet0 提议 → 双方确认 → Agent24 实现。

---

## 10. 待确认(Open Questions)

1. Pet0 壳最终选 Tauri 还是复用 Electron?(不阻塞本约定,但影响分发与复用估算)
2. ~~Local 脑走 oMLX 能否吃 Qwen3-0.6B?~~ **已定(2026-08-09 调研)**:能,且**无需新增 GGUF provider**。oMLX(mlx-lm 底座,OpenAI/Anthropic 兼容)原生支持 `response_format: json_schema` 结构化输出,满足 Local 脑"受约束 JSON"硬约束;Qwen3-0.6B 由 mlx-lm 支持、mlx-community 有量化权重。链路 = `agent24-models` 现成 OpenAI provider → oMLX:8088 → Qwen3-0.6B。动作:`omlx` 拉一次 0.6B 权重 + 量 p95。注意 `enable_thinking=false`(Qwen3 thinking token 会破坏 JSON,与 Pet0 架构一致)。额外:HF cache 已有 `Qwen3-ASR-0.6B` 可喂语音链路 STT。
3. ~~Sin90 同库不同表 vs 独立 DB?~~ **已定**:**独立 DB `sin90.db` + 可加载模块**,比同库更彻底。Sin90 是内核之上的模块,自带 store,依赖单向(Sin90→内核,内核绝不反向依赖)。两种交互不一刀切:**壳↔Sin90 走 HTTP/WS API**;**Sin90↔内核走进程内 ctx 句柄**(模块 `register(router,ctx)`,热路径不加 HTTP 跳)。不做独立进程纯 API——桌宠本地单机拿不到独立进程的好处却要付运维税(但边界用 `Sin90KernelCtx` trait 定义,进程内只是第一个 adapter,将来可加 RPC adapter 拆进程)。**跨库一致性(Codex 自审收口)**:proposal 的状态/审批/apply **全落 sin90.db 单事务**,内核 policy 仅被**只读**查询是否放行;需要写内核的副作用(如注册 cron)经 apply 后的**幂等 outbox 对账**,不跨库两阶段提交。详见 [SIN90-domain.md §0.1/§0.2](specs/SIN90-domain.md)。
4. `.petpack` 的 behaviors.json 沙箱与 Agent24 的模块/审批模型如何对齐?
