# SIN90 — 内置 Personal-OS 领域模型(接口草案)

> 设计草案 v0.2(2026-08-09)。配套:[集成约定](../SIN90-PET0-INTEGRATION.md)。
> **架构定调(v0.2 修正)**:Sin90 是内核之上的**可加载模块**,**自带独立
> DB `sin90.db`**,依赖单向(Sin90 → 内核,内核绝不反向依赖 Sin90)。持久化
> **不再塞进 `agent24-store`**——Sin90 自带 store。**本文件是接口契约草案,
> 非最终实现;签名可在 SPIKE-00 中调整。**

---

## 0. 边界与依赖方向(v0.2 定调)

```
内核(通用,agent24.db)  core/store/models/policy/scheduler/memory/mcp/agent/protocol
      ▲ 单向依赖:Sin90 用内核,内核不认识 Sin90
模块(Sin90,sin90.db)   agent24-sin90(纯域)+ 自带 store + 挂进 agent24d 的 sin90 路由
      ▲ HTTP/WS
壳(Pet0)
```

两种交互,**不一刀切走 API**:

| 交互 | 走什么 | 理由 |
|---|---|---|
| 壳 ↔ Sin90 | HTTP/WS(`/api/v1/sin90/*`) | 跨进程,本就该 API |
| Sin90 ↔ 内核(调模型/注册调度/发事件/审批) | **进程内 ctx 句柄**(模块 `register(router, ctx)`) | 热路径(意图分类 p95<500ms)不该白加 HTTP 跳 |
| 内核 ↔ Sin90 | **无**(单向) | 内核发通用事件,Sin90 按需订阅;内核绝不 call 进 Sin90 |

**为什么模块+独立 DB,而非独立进程纯 API**:桌宠是本地单机,拿不到独立进程的好处(异地/异语言/热插拔都用不上),却要付多养一个 daemon + 内核模型调用跨 HTTP 的税。模块机制拿到"互不影响"(独立 DB、独立迁移、独立演进),不付这税。

**Proposal 原子性不受独立 DB 影响**:一个 `Sin90Proposal` 只改 Sin90 自己的表,"校验+落库+追加事件"是 `sin90.db` 内一个事务;内核审批只是上游放行信号,无需跨库两阶段提交。

---

## 1. 三块交付物与归属

| 交付 | 落点 | 纯度 |
|---|---|---|
| **领域**:实体类型 + 状态机 + Proposal 校验 | 新 crate `agent24-sin90` | 纯(只依赖 protocol + thiserror,**不碰 sqlx/tokio/vendor**) |
| **持久化**:迁移 + repo + 对账 SQL | `agent24-sin90` 自带 store,**独立 `sin90.db`**(非 agent24-store) | sqlx |
| **API**:`/api/v1/sin90/*` + WS 事件 | `agent24d` 挂 sin90 模块路由(`register(router, ctx)`) | axum |

分层理由:内核 `agent24-store` 保持只认内核表;Sin90 自带 store 对着独立 DB,与内核物理隔离,单向依赖。换壳/换 API 不动领域一行,内核加载别的垂直时**根本不带 Sin90 的表**。

---

## 2. crate 布局(两个 crate:纯域 + 自带 store)

```
rust/crates/agent24-sin90/            # 纯域 —— 不碰 sqlx/tokio/vendor
├── Cargo.toml        # deps: agent24-protocol, thiserror  —— 仅此
└── src/
    ├── lib.rs
    ├── types.rs      # 实体 + 状态枚举(snake_case wire shape)
    ├── transitions.rs# 纯状态机函数(镜像 agent24-core::transitions)
    ├── proposal.rs   # Sin90Proposal + 确定性校验
    └── attention.rs  # 对账输入/输出形状(SQL 在 store 侧)

rust/crates/agent24-sin90-store/      # 自带持久化 —— 对着独立 sin90.db
├── Cargo.toml        # deps: agent24-sin90, sqlx  (不进 agent24-store)
├── migrations/
│   └── 0001_sin90.sql
└── src/lib.rs        # Sin90Repo:BEGIN IMMEDIATE + 事件回放
```

内核 `agent24-store` 保持不变、不认识 Sin90;Sin90 的 store 是独立 crate 对着独立 DB。

### 2.1 `types.rs` — 实体与状态

状态枚举全部 `#[serde(rename_all = "snake_case")]`,与 store 的 TEXT 列、wire JSON 对齐(沿用 protocol 惯例)。

```rust
// 标识符沿用 core::util::ulid()
pub type DirectionId = String;
pub type TaskId = String;
// … RhythmId / WeekId / ScheduleBlockId / ReviewId

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectionStatus { Draft, Active, Paused, Achieved, Abandoned }

#[serde(rename_all = "snake_case")]
pub enum TaskStatus { Backlog, Planned, InProgress, Done, Dropped, CarriedOver }

#[serde(rename_all = "snake_case")]
pub enum WeekStatus { Planning, Active, Reviewing, Closed }

#[serde(rename_all = "snake_case")]
pub enum ScheduleBlockStatus { Planned, Started, Completed, Skipped }

#[serde(rename_all = "snake_case")]
pub enum RhythmStatus { Active, Adjusted, Retired }

#[serde(rename_all = "snake_case")]
pub enum ReviewStatus { Draft, Finalized }

#[serde(rename_all = "snake_case")]
pub enum ReviewKind { Daily, Weekly, Rhythm }

pub struct Direction {
    pub id: DirectionId,
    pub title: String,
    pub status: DirectionStatus,
    pub target_window: String,      // "2026-08" | "2026-Q3"
    pub created_at: String,         // ISO8601
    pub updated_at: String,
}

pub struct Task {
    pub id: TaskId,
    pub direction_id: Option<DirectionId>,
    pub week_id: Option<WeekId>,
    pub title: String,
    pub status: TaskStatus,
    pub kind: TaskKind,             // deep_work | admin | …(Local Brain 分类结果)
    pub energy: Energy,             // high | mid | low
    pub est_minutes: Option<u32>,
    pub carried_from: Option<TaskId>, // carried_over 时留链
    pub created_at: String,
    pub updated_at: String,
}
// Rhythm / Week / ScheduleBlock / Review 同构,略。
```

### 2.2 `transitions.rs` — 纯状态机(镜像 core)

**每个实体一对函数**,与 `agent24-core::transitions` 完全同款签名:

```rust
#[derive(thiserror::Error, Debug)]
pub enum TransitionError {
    #[error("illegal {entity} transition: {from:?} -> {to:?}")]
    Illegal { entity: &'static str, from: String, to: String },
}

pub fn direction_transition_allowed(from: DirectionStatus, to: DirectionStatus) -> bool;
pub fn check_direction_transition(from: DirectionStatus, to: DirectionStatus)
    -> Result<(), TransitionError>;

pub fn task_transition_allowed(from: TaskStatus, to: TaskStatus) -> bool;
pub fn check_task_transition(from: TaskStatus, to: TaskStatus)
    -> Result<(), TransitionError>;

// week / schedule_block / rhythm / review 同款。
pub fn direction_is_terminal(s: DirectionStatus) -> bool; // achieved|abandoned
pub fn task_is_terminal(s: TaskStatus) -> bool;           // done|dropped
```

合法迁移表(草案):

```
Direction : draft→active→{achieved,abandoned,paused} ; paused→active
Task      : backlog→planned→in_progress→{done,dropped,carried_over}
            planned→dropped ; backlog→dropped
Week      : planning→active→reviewing→closed
Block     : planned→started→{completed,skipped} ; planned→skipped
Rhythm    : active→adjusted→retired ; active→retired
Review    : draft→finalized
```

### 2.3 `proposal.rs` — AI 不写库的门

**AI(Local/Executive)产出的一切都是 `Sin90Proposal`;落库前必过 `validate`——复用 store 事务 + 本校验,不新造审批机制(与 `agent24-policy` 同哲学:fail-closed)。**

```rust
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Sin90Op {
    CreateDirection { title: String, target_window: String },
    TransitionTask  { task_id: TaskId, to: TaskStatus },
    CreateTasks     { week_id: WeekId, tasks: Vec<NewTask> },
    ReorderTasks    { week_id: WeekId, order: Vec<TaskId> },
    AdjustRhythm    { rhythm_id: RhythmId, new_alloc: Vec<Alloc> },
    // …
}

pub struct Sin90Proposal {
    pub id: String,
    pub source: ProposalSource,      // local_brain | executive(codex) | rule
    pub ops: Vec<Sin90Op>,           // 一个 proposal 可含多个原子 op
    pub rationale: Option<String>,   // 给用户看的解释
}

/// 纯校验:schema 合法性 + 每个 op 目标状态迁移是否合法。
/// 不触库——库内当前状态由 store 侧在事务里读出后再调本函数。
pub fn validate(p: &Sin90Proposal, ctx: &ValidationCtx) -> Result<(), ProposalError>;
```

落库流程(在 store 侧,`BEGIN IMMEDIATE` 内):

```
读当前状态(写锁) → sin90::proposal::validate() → 逐 op 事务写入 → 追加 sin90_events → COMMIT
                                     │Err
                                     └→ 整个 proposal 回滚,返回精确错误(绝不半写)
```

---

## 3. 持久化(`agent24-sin90-store`,独立 `sin90.db`)

### 3.1 迁移 `agent24-sin90-store/migrations/0001_sin90.sql`(草案)

独立 SQLite 文件 `sin90.db`、独立迁移线;复用 sqlx migrate 框架但**与内核 agent24.db 物理隔离**:

```sql
-- Sin90 domain (Personal-OS). Statuses are TEXT matching snake_case wire enums.
CREATE TABLE sin90_directions (
    id            TEXT PRIMARY KEY,
    title         TEXT NOT NULL,
    status        TEXT NOT NULL,
    target_window TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE INDEX idx_sin90_dir_status ON sin90_directions(status);

CREATE TABLE sin90_tasks (
    id           TEXT PRIMARY KEY,
    direction_id TEXT REFERENCES sin90_directions(id),
    week_id      TEXT REFERENCES sin90_weeks(id),
    title        TEXT NOT NULL,
    status       TEXT NOT NULL,
    kind         TEXT NOT NULL,
    energy       TEXT NOT NULL,
    est_minutes  INTEGER,
    carried_from TEXT REFERENCES sin90_tasks(id),
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE INDEX idx_sin90_task_status ON sin90_tasks(status);
CREATE INDEX idx_sin90_task_week   ON sin90_tasks(week_id);

-- sin90_weeks / sin90_rhythms / sin90_schedule_blocks / sin90_reviews 同构,略。

-- 唯一事实来源:append-only 事件流。所有状态变更必写一条。
CREATE TABLE sin90_events (
    id         TEXT PRIMARY KEY,
    entity     TEXT NOT NULL,          -- direction|task|week|block|rhythm|review
    entity_id  TEXT NOT NULL,
    kind       TEXT NOT NULL,          -- created|transitioned|reordered|…
    from_state TEXT,                   -- 迁移事件才有
    to_state   TEXT,
    payload    TEXT NOT NULL,          -- JSON 明细(用于回放)
    at         TEXT NOT NULL
);
CREATE INDEX idx_sin90_events_entity ON sin90_events(entity, entity_id);
CREATE INDEX idx_sin90_events_at     ON sin90_events(at);

-- 路由记账(三级脑):每次 AI 决策一行。
CREATE TABLE sin90_ai_calls (
    id            TEXT PRIMARY KEY,
    task_kind     TEXT NOT NULL,        -- intent_classify|weekly_plan|…
    engine        TEXT NOT NULL,        -- reflex|local|executive
    fallback_from TEXT,                 -- 非空 = 本该降级却打到了更贵的脑
    latency_ms    INTEGER,
    ok            INTEGER NOT NULL,
    at            TEXT NOT NULL
);

-- 对账物化视图:按天×方向的计划vs实际分钟数,由 sin90_events 回放增量更新。
CREATE TABLE sin90_attention_daily (
    day          TEXT NOT NULL,
    direction_id TEXT NOT NULL,
    planned_min  INTEGER NOT NULL DEFAULT 0,
    actual_min   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, direction_id)
);
```

### 3.2 repo trait(草案)

```rust
// agent24-store 内新增,风格同现有 repo(sqlx::query, BEGIN IMMEDIATE)。
pub trait Sin90Repo {
    async fn create_direction(&self, d: NewDirection) -> Result<Direction, StoreError>;
    /// 读当前状态(写锁)→ check_task_transition → UPDATE → 追加 sin90_events,全在一个事务。
    async fn transition_task(&self, id: &TaskId, to: TaskStatus) -> Result<Task, StoreError>;
    /// 一个 proposal 的所有 op 落在同一事务;任一 op 校验失败 → 整体回滚。
    async fn apply_proposal(&self, p: &Sin90Proposal) -> Result<AppliedProposal, StoreError>;
    /// SPIKE-00 判定面:纯从 sin90_events 回放算注意力分配。
    async fn attention(&self, window: Window) -> Result<AttentionReport, StoreError>;
    // list_* / get_* …
}
```

**SPIKE-00 对账 SQL 的形状**(核心):

```sql
-- 「本周 Coding 18h」= 回放本周所有 block.completed 事件,按 direction 归并 actual_min。
SELECT d.title, SUM(json_extract(e.payload,'$.minutes')) AS actual_min
FROM sin90_events e
JOIN sin90_schedule_blocks b ON b.id = e.entity_id
JOIN sin90_directions d      ON d.id = b.direction_id
WHERE e.entity='block' AND e.kind='transitioned' AND e.to_state='completed'
  AND e.at >= :week_start AND e.at < :week_end
GROUP BY d.id;
```

---

## 4. API 面(sin90 模块挂进 `agent24d`)

沿用现有约定:`/api/v1/*`、bearer 门(除 health)、路径直接抠 `{id}`、WS 复用 `/api/v1/events`。路由由 sin90 模块的 `register(router, ctx)` 挂载,`ctx` 携带内核服务句柄(ModelRouter / Scheduler / policy / EventSink / 自身 Sin90Repo)。

### 4.1 路由表

```rust
// sin90 模块 register(router, ctx) 里挂进 agent24d 的 Router……
.route("/api/v1/sin90/directions",        get(sin90::list_directions).post(sin90::create_direction))
.route("/api/v1/sin90/directions/{id}",   get(sin90::get_direction).patch(sin90::transition_direction))
.route("/api/v1/sin90/tasks",             get(sin90::list_tasks).post(sin90::create_task))
.route("/api/v1/sin90/tasks/{id}",        get(sin90::get_task).patch(sin90::transition_task))
.route("/api/v1/sin90/weeks",             get(sin90::list_weeks).post(sin90::create_week))
.route("/api/v1/sin90/weeks/{id}",        get(sin90::get_week).patch(sin90::transition_week))
.route("/api/v1/sin90/schedule-blocks",   get(sin90::list_blocks).post(sin90::create_block))
.route("/api/v1/sin90/schedule-blocks/{id}", get(sin90::get_block).patch(sin90::transition_block))
.route("/api/v1/sin90/rhythms",           get(sin90::list_rhythms).post(sin90::create_rhythm))
.route("/api/v1/sin90/reviews",           get(sin90::list_reviews).post(sin90::create_review))
.route("/api/v1/sin90/reviews/{id}",      get(sin90::get_review).patch(sin90::transition_review))
// Proposal 门
.route("/api/v1/sin90/proposals",             post(sin90::submit_proposal))
.route("/api/v1/sin90/proposals/{id}/accept", post(sin90::accept_proposal))
.route("/api/v1/sin90/proposals/{id}/reject", post(sin90::reject_proposal))
// 对账(SPIKE-00)
.route("/api/v1/sin90/attention",         get(sin90::attention))
```

- `PATCH /{entity}/{id}` body = `{ "to": "<status>", ...fields }`,handler 调 `Sin90Repo::transition_*`,状态机非法 → `409 Conflict`(与现有 approvals 的 409 语义一致)。
- `POST /proposals` 提交后进 pending;`accept` 才触发 `apply_proposal` 落库。可配置为「low-risk op 自动 accept」——复用 `agent24-policy` 的 standing-grants。

### 4.2 WS 事件(复用 `/api/v1/events`)

新增 `sin90.*` 事件类型(与现有 `run.*`/`schedule.*` 同一条流):

```
sin90.direction.created     sin90.task.transitioned
sin90.week.closed           sin90.proposal.pending
sin90.attention.updated     sin90.nudge.due
```

壳订阅这些做无刷新更新(桌宠反应、气泡、对账面板)。

### 4.3 typed client

`@agent24/api-client` 增 `sin90` 命名空间(`client.sin90.directions.create(...)` 等),供壳消费,避免壳手拼 URL。

---

## 5. 三级路由归属(与领域解耦)

Router 本身是 `agent24-models` 之上的 policy 层,不进 `agent24-sin90`:

```
Sin90 需要一次 AI 决策(如 task 分类 / weekly-plan)
        │
        ▼   记一行 sin90_ai_calls
确定性够吗? ──是──→ Reflex(壳内 FSM/Rule,不过 API)
        │否
        ▼
常规语义? ──是──→ Local(agent24-models: oMLX/GGUF provider,受约束 JSON)
        │否
        ▼
需推理/规划? ─是─→ Executive(agent24-models: OpenAI 兼容 provider,Codex)
```

输出统一回到 §2.3 的 `Sin90Proposal`,再过 Proposal 门。

### 5.1 模型层三个替换轴(隔离原则,硬约束)

**Sin90 永不直连任何具体模型/运行时/端点;它只依赖 `ctx.model` 抽象(内核 `ModelRouter`)。** 下面三个轴各自独立可换,换任一轴不动 Sin90 一行、不动业务接口:

| 轴 | 换什么 | 换的位置 | Sin90 感知? |
|---|---|---|---|
| **运行时** | oMLX ↔ Ollama ↔ llama.cpp ↔ 云 API | `agent24-models` provider 实现 | 否 |
| **模型权重** | Qwen3-0.6B ↔ 1.7B ↔ 换厂商模型 | provider 配置(model id) | 否 |
| **provider 应用** | 现成 OpenAI-兼容 ↔ 更好的第三方 provider crate | `agent24-models` 有序注册表 | 否 |

约束落法:
- Sin90 调模型只经 `ctx.model.route(task_kind, prompt, schema) -> Sin90Proposal 素材`;**输入是任务类别 + 受约束 schema,输出是结构化结果**,概不出现 endpoint / api-key / model-id / vendor SDK 类型。
- 路由/重试/降级/健康反馈在 `ModelRouter`(trait 之上),provider 内只做"一次调用"(ADR-026 §6.5)。
- 有更好的模型 → 改配置换权重;有更好的 provider → 注册表里替换一项;换本地运行时 → 换 provider 实现。三者都不触碰 Sin90 与其 API。

---

## 6. Cargo 接线(依赖单向:Sin90 → 内核)

```toml
# rust/Cargo.toml [workspace].members 增:
"crates/agent24-sin90",
"crates/agent24-sin90-store",

# agent24-sin90/Cargo.toml —— 纯域,保持纯净
[dependencies]
agent24-protocol = { path = "../agent24-protocol" }
thiserror = "2"
serde = { workspace = true }
serde_json = { workspace = true }

# agent24-sin90-store/Cargo.toml —— 自带 store,对着独立 sin90.db
[dependencies]
agent24-sin90 = { path = "../agent24-sin90" }
sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio","sqlite","migrate","macros"] }

# agent24d/Cargo.toml deps 增(内核宿主挂模块):
agent24-sin90       = { path = "../../crates/agent24-sin90" }
agent24-sin90-store = { path = "../../crates/agent24-sin90-store" }
```

**关键**:`agent24-store`(内核持久化)**不新增对 sin90 的依赖**——依赖箭头只从 sin90 指向内核,内核不反向认识 sin90。

---

## 7. SPIKE-00 落地清单(据此起跑)

- [ ] `agent24-sin90`:`Direction/Task/ScheduleBlock` 类型 + `check_*_transition` + `validate`(纯,带单测)
- [ ] `agent24-sin90-store`:`0001_sin90.sql` 最小子集(独立 `sin90.db`)+ `Sin90Repo`(`create_direction`/`apply_proposal`/`transition_block`/`attention`)
- [ ] `agent24d`:sin90 模块挂 `POST /directions`、`POST /proposals`、`GET /attention`
- [ ] Local 脑冒烟:`omlx` 拉 `mlx-community/Qwen3-0.6B` → 经 agent24-models OpenAI provider(`response_format: json_schema`)做一次意图分类,量 p95(SPIKE-03)
- [ ] 判定:塞入一串 block.completed 事件,`GET /api/v1/sin90/attention?window=week` 返回「Coding 18h / Business 2h」,全部来自事件回放
- [ ] Pet0 侧:一次性壳建 1 个 Direction + 渲染对账 → 绿灯即坐实
