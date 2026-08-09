# SIN90 — 内置 Personal-OS 领域模型(接口草案)

> 设计草案 v0.1(2026-08-09)。配套:[集成约定](../SIN90-PET0-INTEGRATION.md)。
> 分层遵循 ADR-026:领域 crate 保持纯净(只依赖 protocol + thiserror),
> 持久化扩展 `agent24-store`,API 扩展 `agent24d`。**本文件是接口契约草案,
> 非最终实现;签名可在 SPIKE-00 中调整。**

---

## 1. 三块交付物与归属

| 交付 | 落点 | 纯度 |
|---|---|---|
| **领域**:实体类型 + 状态机 + Proposal 校验 | 新 crate `agent24-sin90` | 纯(只依赖 protocol + thiserror,**不碰 sqlx/tokio/vendor**) |
| **持久化**:迁移 + repo + 对账 SQL | 扩展 `agent24-store`(同库不同表) | sqlx |
| **API**:`/api/v1/sin90/*` + WS 事件 | 扩展 `agent24d`(新 `sin90.rs` 路由模块) | axum |

分层理由:与现有 `agent24-core`(纯状态机)/`agent24-store`(sqlx)/`agent24d`(axum)完全同构。换持久化或 API 不动领域一行。

---

## 2. crate `agent24-sin90` 布局

```
rust/crates/agent24-sin90/
├── Cargo.toml          # deps: agent24-protocol, thiserror  —— 仅此
└── src/
    ├── lib.rs
    ├── types.rs        # 实体 + 状态枚举(snake_case wire shape)
    ├── transitions.rs  # 纯状态机函数(镜像 agent24-core::transitions)
    ├── proposal.rs     # Sin90Proposal + 确定性校验
    └── attention.rs    # 对账输入/输出形状(SQL 在 store 侧)
```

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

## 3. 持久化(扩展 `agent24-store`)

### 3.1 迁移 `migrations/0005_sin90.sql`(草案)

同一个 SQLite 文件、同一迁移框架,新增表:

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

## 4. API 面(扩展 `agent24d`,新 `sin90.rs`)

沿用现有约定:`/api/v1/*`、bearer 门(除 health)、路径直接抠 `{id}`、WS 复用 `/api/v1/events`。

### 4.1 路由表

```rust
// server.rs 里挂进现有 Router::new()……
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

---

## 6. Cargo 接线

```toml
# rust/Cargo.toml [workspace].members 增:
"crates/agent24-sin90",

# agent24-store/Cargo.toml deps 增:
agent24-sin90 = { path = "../agent24-sin90" }

# agent24-sin90/Cargo.toml —— 保持纯净
[dependencies]
agent24-protocol = { path = "../agent24-protocol" }
thiserror = "2"
serde = { workspace = true }
serde_json = { workspace = true }
```

---

## 7. SPIKE-00 落地清单(据此起跑)

- [ ] `agent24-sin90`:`Direction/Task/ScheduleBlock` 类型 + `check_*_transition` + `validate`(纯,带单测)
- [ ] `agent24-store`:`0005_sin90.sql` 最小子集 + `Sin90Repo`(`create_direction`/`apply_proposal`/`transition_block`/`attention`)
- [ ] `agent24d`:`sin90.rs` 挂 `POST /directions`、`POST /proposals`、`GET /attention`
- [ ] 判定:塞入一串 block.completed 事件,`GET /api/v1/sin90/attention?window=week` 返回「Coding 18h / Business 2h」,全部来自事件回放
- [ ] Pet0 侧:一次性壳建 1 个 Direction + 渲染对账 → 绿灯即坐实
```
