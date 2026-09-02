# SIN90 — 内置 Personal-OS 领域模型(接口草案)

> 设计草案 v0.3(2026-08-09)。配套:[集成约定](../SIN90-PET0-INTEGRATION.md)。
> **架构定调**:Sin90 是内核之上的**可加载模块**,**自带独立 DB `sin90.db`**,
> 依赖单向(Sin90 → 内核,内核绝不反向依赖 Sin90)。持久化**不塞进
> `agent24-store`**——Sin90 自带 store。**本文件是接口契约草案,非最终实现;
> 签名可在 SPIKE-00 中调整。** v0.3 依据 Codex 架构自审收口了跨库原子性、
> 回放正确性、协议事件三条硬伤(改动记录见 §9)。

---

## 0. 边界与依赖方向

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
| Sin90 ↔ 内核(调模型/注册调度/发事件/查授权) | **`Sin90KernelCtx` trait**(进程内为第一个 adapter) | 热路径(意图分类 p95<500ms)不该白加 HTTP 跳 |
| 内核 ↔ Sin90 | **无**(单向) | 内核发通用/模块事件,Sin90 按需订阅;内核绝不 call 进 Sin90 |

**ctx 是 trait,不是内联句柄(Codex #2 修正)**:边界定义为 `Sin90KernelCtx` trait(`model()` / `scheduler()` / `events()` / `authz()`),进程内实现只是**第一个 adapter**;将来 headless server 版要把 Sin90 拆独立进程时,加一个 RPC adapter 即可,业务代码不动。选"进程内"是**部署决定**,不是语义边界——语义边界永远是这个 trait。

**为什么先做进程内 adapter,而非独立进程纯 API**:桌宠本地单机,拿不到独立进程的好处却要付多养 daemon + 内核模型调用跨 HTTP 的税。模块 + trait 边界拿到"互不影响",又保留将来拆进程的口子。

### 0.1 跨库一致性:Sin90 自持 proposal/审批,内核只读放行(Codex #1/#Critical 修正)

**原"内核审批只是上游放行信号"的说法有洞:若 accept 时内核 approval 行与 sin90 apply 分属两库,一边成功一边失败会审计/实际不一致。收口如下:**

- **proposal 的权威状态、审批决定、apply 结果全部落在 `sin90.db`**(见 §2.3 的 `sin90_proposals`)。内核 `agent24-policy` 只被**只读**查询:"这类 op 是否命中 standing-grant 可自动放行?"——读,不写,不需与 apply 原子。
- 因此 accept 一个 proposal = **单库单事务**:`sin90.db` 内 `pending→applied` + 写领域表 + 追加事件,一起提交。**不存在跨库两阶段提交。**
- 内核侧真正需要写的副作用(如 Rhythm proposal 要在内核 `scheduler` 注册 cron)**不进 proposal 事务**,而是 **apply 提交后触发的幂等对账 job**(outbox 模式,§0.2)。

### 0.2 内核副作用 = apply 后的幂等对账(Codex #High 修正)

凡是"落库 Sin90 状态 → 顺带在内核注册/更新点什么"(Rhythm→cron schedule、nudge→定时器)的场景:

- proposal 事务**只写 Sin90 表**并记一条 `sin90_outbox`(desired kernel state,带确定性 key)。
- 一个 reconciler 读 outbox,对内核做**幂等 upsert**(按 key,重复执行等价一次),成功后标记 outbox 行 done。
- 崩溃重启后 reconciler 重放未完成 outbox → 最终一致;内核 schedule 永远由 Sin90 状态**派生**,不会与之分叉。

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
├── Cargo.toml        # deps: serde, serde_json, thiserror —— 无任何 agent24 依赖
└── src/
    ├── lib.rs
    ├── types.rs      # 实体 + 状态枚举(snake_case wire shape)
    ├── transitions.rs# 纯状态机函数(镜像 agent24-core::transitions)
    ├── proposal.rs   # Sin90Proposal + 确定性校验
    └── attention.rs  # 对账输入/输出形状(SQL 在 store 侧)

rust/crates/agent24-sin90-store/      # 自带持久化 —— 对着独立 sin90.db
├── Cargo.toml        # deps: agent24-sin90, agent24-core(仅 ulid/now), sqlx (不进 agent24-store)
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
// 已实现的形状:每实体一个变体,字段是强类型枚举(不是 String) —— 误用在编译期就挡住
#[derive(thiserror::Error, Debug)]
pub enum TransitionError {
    Direction { from: DirectionStatus, to: DirectionStatus },
    Task { from: TaskStatus, to: TaskStatus },
    // … Week / Block / Rhythm / Review / Proposal 同形
}

pub fn direction_transition_allowed(from: DirectionStatus, to: DirectionStatus) -> bool;
pub fn check_direction_transition(from: DirectionStatus, to: DirectionStatus)
    -> Result<(), TransitionError>;

pub fn task_transition_allowed(from: TaskStatus, to: TaskStatus) -> bool;
pub fn check_task_transition(from: TaskStatus, to: TaskStatus)
    -> Result<(), TransitionError>;

// week / schedule_block / rhythm / review 同款。
pub fn direction_is_terminal(s: DirectionStatus) -> bool; // achieved|abandoned
pub fn task_is_terminal(s: TaskStatus) -> bool;           // done|dropped|carried_over
// 注:carried_over 是终态——原 task 就此关闭,carry-over 原子地新建一个下周 task
// 并回填其 carried_from(见 §3.1 UNIQUE 约束 + §3.2 原子操作)。Codex #Medium 修正:
// 终态集合与迁移表一致,都含 carried_over。
```

合法迁移表(与已合并的 #99 实现同步):

```
Direction : draft→active→{achieved,abandoned,paused} ; paused→active
            draft→abandoned ; paused→abandoned      (直接放弃,不必穿过没待过的态)
Task      : backlog→planned→in_progress→{done,dropped,carried_over}
            planned→dropped ; backlog→dropped ; planned→carried_over
            (carried_over 是终态;可从 planned 或 in_progress 结转)
Week      : planning→active→reviewing→closed
Block     : planned→started→{completed,skipped} ; planned→skipped
Rhythm    : active→adjusted→retired ; active→retired ; adjusted→adjusted (可反复调)
Review    : draft→finalized
Proposal  : pending→applying→applied ; {pending,applying}→rejected
            (applying→pending 是 apply 失败时的 DB 回滚,非显式边)
```

关系型不变量**不进状态机也不进纯校验器**(ValidationCtx 是逐实体的),由 store 在 apply 写锁下强制:任务只在其**周 open(planning|active)**时可变、carry-over 的 `to_week` 不得等于源任务当前周(否则原任务作废且同周产生可无限自我复制的重复行)。alloc 的 direction 查重是**代码校验**(allocations 是 `sin90_rhythms.allocations` 的 JSON,非 DB UNIQUE 约束)。

### 2.3 `proposal.rs` — AI 不写库的门(持久 + 幂等)

**AI(Local/Executive)产出的一切都是 `Sin90Proposal`;它先持久化进 pending,accept 时经 `validate` 校验后在 `sin90.db` 单事务落库。审批门在 Sin90 自持,内核 policy 仅被只读查询(§0.1)。**

```rust
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Sin90Op {
    CreateDirection { title: String, target_window: String },
    TransitionTask  { task_id: TaskId, to: TaskStatus },
    CreateTasks     { week_id: WeekId, tasks: Vec<NewTask> },
    ReorderTasks    { week_id: WeekId, order: Vec<TaskId> },
    AdjustRhythm    { rhythm_id: RhythmId, new_alloc: Vec<Alloc> },
    CarryOverTask   { task_id: TaskId, to_week: WeekId }, // 原子:关闭原 task + 建新 task 回填 carried_from
    // …
}

#[serde(rename_all = "snake_case")]
pub enum ProposalStatus { Pending, Applying, Applied, Rejected } // 持久,见 sin90_proposals

pub struct Sin90Proposal {
    pub id: String,                  // 客户端可复用同 id 重试 → 幂等
    pub status: ProposalStatus,
    pub source: ProposalSource,      // local_brain | executive(codex) | rule
    pub ops: Vec<Sin90Op>,           // 一个 proposal 可含多个原子 op
    pub rationale: Option<String>,
}

/// 纯校验:schema 合法性 + 每个 op 目标状态迁移是否合法。
/// 不触库——库内当前状态由 store 侧在事务里读出后再调本函数。
pub fn validate(p: &Sin90Proposal, ctx: &ValidationCtx) -> Result<(), ProposalError>;
```

**apply 流程(单库单事务 + CAS 幂等,Codex #1/#High/#并发 修正)**:

```
BEGIN IMMEDIATE (sin90.db):
  CAS: UPDATE sin90_proposals SET status='applying' WHERE id=? AND status='pending'
       └ 影响 0 行 → 该 proposal 非 pending(已 apply/正在 apply)→ 直接返回既有结果,不重复 apply
  读每个 op 目标实体当前状态(同事务写锁) → sin90::proposal::validate()
  逐 op 写领域表 + 每次变更追加一条 sin90_events(payload 自包含,见 §3.1)
  需要内核副作用的 op → 只写 sin90_outbox(desired kernel state),不在此调内核(§0.2)
  UPDATE sin90_proposals SET status='applied'
COMMIT
   │任一步 Err → 整体回滚(proposal 退回 pending),返回精确错误,绝不半写
apply 提交后:reconciler 消费 sin90_outbox → 对内核幂等 upsert(崩溃可重放)
```

**审批(accept)**:handler 先只读问 `ctx.authz()`(内核 standing-grants)判断该 proposal 是否可自动放行;人工 proposal 则等用户 accept。放行判定与 apply **不需要原子**——它只决定"要不要调 apply",apply 本身是上面那个幂等事务。

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
-- Codex #High 修正:一个原 task 最多被 carry-over 一次,防并发/重试重复生成子任务。
CREATE UNIQUE INDEX idx_sin90_task_carried ON sin90_tasks(carried_from) WHERE carried_from IS NOT NULL;

-- sin90_weeks / sin90_rhythms / sin90_schedule_blocks / sin90_reviews 同构,略。

-- 唯一事实来源:append-only 事件流。所有状态变更必写一条。
-- Codex #High 修正:seq 单调自增 = 全序;payload 必须自包含(回放不 join 可变表)。
CREATE TABLE sin90_events (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,  -- 单调全序,回放/水位线的锚
    id         TEXT NOT NULL UNIQUE,               -- ulid,对外稳定标识
    entity     TEXT NOT NULL,          -- direction|task|week|block|rhythm|review
    entity_id  TEXT NOT NULL,
    kind       TEXT NOT NULL,          -- created|transitioned|reordered|…
    from_state TEXT,
    to_state   TEXT,
    -- 自包含快照:回放所需的一切都在这(direction_id/title 快照、minutes、
    -- block_id、occurred_at、schema_ver)。事后改 direction 标题/删 block 不影响历史回放。
    payload    TEXT NOT NULL,
    schema_ver INTEGER NOT NULL DEFAULT 1,
    at         TEXT NOT NULL           -- 事件发生时刻(occurred_at),回放按 seq 而非 at 排序
);
CREATE INDEX idx_sin90_events_entity ON sin90_events(entity, entity_id);

-- Proposal 门(持久):pending→applying→applied|rejected,支撑重启恢复 + CAS 幂等。
CREATE TABLE sin90_proposals (
    id         TEXT PRIMARY KEY,       -- 客户端可复用同 id 重试
    status     TEXT NOT NULL,          -- pending|applying|applied|rejected
    source     TEXT NOT NULL,          -- local_brain|executive|rule
    ops        TEXT NOT NULL,          -- JSON: Vec<Sin90Op>
    rationale  TEXT,
    result     TEXT,                   -- applied 后的 AppliedProposal JSON(重试幂等返回)
    created_at TEXT NOT NULL,
    decided_at TEXT
);
CREATE INDEX idx_sin90_proposals_status ON sin90_proposals(status);

-- 内核副作用 outbox(§0.2):apply 事务内只写这里,reconciler 幂等落到内核。
CREATE TABLE sin90_outbox (
    id           TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,        -- register_schedule|update_schedule|…
    dedup_key    TEXT NOT NULL,        -- 确定性 key,内核侧据此 upsert(幂等)
    desired      TEXT NOT NULL,        -- JSON: 期望的内核状态
    status       TEXT NOT NULL,        -- pending|done
    created_at   TEXT NOT NULL,
    done_at      TEXT
);
CREATE INDEX idx_sin90_outbox_status ON sin90_outbox(status);

-- 路由记账(三级脑):每次 AI 决策一行。
CREATE TABLE sin90_ai_calls (
    id            TEXT PRIMARY KEY,
    task_kind     TEXT NOT NULL,
    engine        TEXT NOT NULL,        -- reflex|local|executive
    fallback_from TEXT,                 -- 非空 = 本该降级却打到了更贵的脑
    latency_ms    INTEGER,
    ok            INTEGER NOT NULL,
    at            TEXT NOT NULL
);

-- 对账物化视图:纯派生自 sin90_events,带水位线保证"增量 == 全量回放"。
-- Codex #High 修正:applied_event_seq 记已消费到的 seq;重建 = 清空 + 从 seq 0 回放。
CREATE TABLE sin90_attention_daily (
    day               TEXT NOT NULL,
    direction_id      TEXT NOT NULL,
    -- 只落 actual:planned 从 rhythm 的 allocations 现算,不物化
    -- (否则 rhythm 一改,这张表里的 planned 就是过期副本)
    actual_min        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, direction_id)
);
CREATE TABLE sin90_attention_watermark (
    only_row          INTEGER PRIMARY KEY CHECK (only_row = 1),  -- 单行由 DB 保证,不靠约定
    applied_event_seq INTEGER NOT NULL
);
```

### 3.2 repo trait(草案)

```rust
// agent24-sin90-store 内,风格同现有 repo(sqlx::query, BEGIN IMMEDIATE)。
pub trait Sin90Repo {
    async fn create_direction(&self, d: NewDirection) -> Result<Direction, StoreError>;
    /// 读当前状态(写锁)→ check_task_transition → UPDATE → 追加 sin90_events,全在一个事务。
    async fn transition_task(&self, id: &TaskId, to: TaskStatus) -> Result<Task, StoreError>;
    /// §2.3 apply:CAS 幂等 + 单事务落库 + outbox;重复 id 返回既有结果。
    async fn apply_proposal(&self, p: &Sin90Proposal) -> Result<AppliedProposal, StoreError>;
    /// SPIKE-00:纯从 sin90_events 回放(不 join 可变表)。
    async fn attention(&self, window: Window) -> Result<AttentionReport, StoreError>;
    /// 物化视图增量推进 + 确定性重建(见下)。
    async fn attention_apply_new_events(&self) -> Result<(), StoreError>;
    async fn attention_rebuild(&self) -> Result<(), StoreError>; // 清空 + 从 seq 0 全量回放
    // list_* / get_* …
}
```

**SPIKE-00 对账 SQL(核心,只读事件、不 join 可变表 —— Codex #High 修正)**:

```sql
-- 「本周 Coding 18h」= 回放本周所有 block.completed 事件。
-- direction_id / minutes 全取自事件 payload 快照,绝不 join 当前的 blocks/directions,
-- 这样事后改标题、删 block、迁移都不会篡改历史回放结果。
SELECT json_extract(e.payload,'$.direction_id')       AS direction_id,
       json_extract(e.payload,'$.direction_title')     AS direction_title, -- 快照标题
       SUM(json_extract(e.payload,'$.minutes'))         AS actual_min
FROM sin90_events e
WHERE e.entity='block' AND e.kind='transitioned' AND e.to_state='completed'
  AND json_extract(e.payload,'$.occurred_at') >= :week_start
  AND json_extract(e.payload,'$.occurred_at') <  :week_end
GROUP BY direction_id;
```

**物化视图正确性协议**:`attention_apply_new_events()` 从 `sin90_attention_watermark.applied_event_seq+1` 起按 `seq` 升序消费新事件、累加到 `sin90_attention_daily`、推进水位线,全在一个事务;`attention_rebuild()` 清空后从 seq 0 全量回放。**测试硬断言:任意事件序列下,增量结果 == 全量重建结果**(乱序写入不影响,因回放按 `seq` 全序而非按 `at`)。

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

- **这些是命令(command),不是事件**(Codex #Medium 修正):壳发的 `POST`/`PATCH` 都要经状态机校验、可被拒(409)。壳**从不直接写"既成事实"**;事实只由核在校验后产生,并以 `sin90.*` 事件(§4.2)回推给壳。"壳只发命令、只订阅事件"才是准确表述。
- `PATCH /{entity}/{id}` body = `{ "to": "<status>", ...fields }`,handler 调 `Sin90Repo::transition_*`,状态机非法 → `409 Conflict`(与现有 approvals 的 409 语义一致)。
- `POST /proposals` 持久化进 `pending`(重启可恢复);`accept` 触发 §2.3 的幂等 apply。**同 `proposal_id` 重试安全**:CAS 命中非 pending → 返回既有 `result`,不重复 apply。可配置「low-risk op 自动 accept」——**只读**查 `agent24-policy` standing-grants(§0.1,不与 apply 原子)。

### 4.2 WS 事件:通用模块事件信封(Codex #High/#3危 修正)

**冲突**:现有 `agent24-protocol::EventBody` 是**强类型枚举**;若直接加 `sin90.direction.created`,要么让 protocol 认识 Sin90(破坏"内核不认识 Sin90"),要么无处安放。

**收口**:给内核协议加**一个通用模块事件信封**——内核只知"模块能发不透明事件",不知其语义,单向依赖不破:

```rust
// agent24-protocol::EventBody 新增一个变体(内核一次性通用能力,非 Sin90 专属):
Module { module: String, kind: String, payload: serde_json::Value }
// 例:{ module:"sin90", kind:"task.transitioned", payload:{...} }
```

- Sin90 经 `ctx.events()` 发 `Module{module:"sin90", ...}`,与 `run.*`/`schedule.*` 同一条 `/api/v1/events` WS 流。
- 壳按 `module=="sin90"` 过滤、按 `kind` 分发(`task.transitioned`/`proposal.pending`/`attention.updated`/`nudge.due`…)做无刷新更新。
- WS 鉴权/Origin 拒绝规则**沿用现有 events 通道**(SPEC-002),Sin90 不另立(Codex #Low)。
- 这个信封变体也随 ADR-026 的 JSON Schema 单一事实源发布(见 §8 遗留项)。

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
- Sin90 调模型经 `ctx.model()`,底层是现有 `ModelRouter::complete(TaskProfile, CompletionRequest, CancellationToken)`(Codex #High 修正:原稿写的 `route(task_kind,prompt,schema)` **不存在**,已对齐真实签名)。Sin90 侧永不出现 endpoint / api-key / model-id / vendor SDK 类型。
- **受约束 JSON 输出**(§8):当时 `CompletionRequest` 没有 `response_format`,两个收口方案里选了 (a)——给它加可选 `response_format: JsonSchema`,由 OpenAI-兼容 provider 透传。**已实现并合并(PR #102)**,不再是待办;(b)「Sin90 侧自己校验 + 重试」相应作废。
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
agent24-core  = { path = "../agent24-core" }   # 仅 util:ulid() / now_iso8601(),仍是单向
sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio","sqlite","migrate","macros"] }

# agent24d/Cargo.toml deps 增(内核宿主挂模块):
agent24-sin90       = { path = "../../crates/agent24-sin90" }
agent24-sin90-store = { path = "../../crates/agent24-sin90-store" }
```

**关键**:`agent24-store`(内核持久化)**不新增对 sin90 的依赖**——依赖箭头只从 sin90 指向内核,内核不反向认识 sin90。

---

## 7. SPIKE-00 落地清单(据此起跑)

- [ ] `agent24-sin90`:`Direction/Task/ScheduleBlock` 类型 + `check_*_transition`(终态含 carried_over)+ `validate`(纯,带单测)
- [ ] `agent24-sin90-store`:`0001_sin90.sql`(含 `sin90_proposals`/`sin90_outbox`/事件 `seq`/attention 水位线/`UNIQUE(carried_from)`)+ `Sin90Repo`(`apply_proposal` 走 CAS 幂等单事务、`attention` 纯回放、`attention_rebuild`)
- [ ] **一致性测试(判定门槛)**:`apply_proposal` 同 id 重试幂等;增量 attention == 全量 rebuild;事件 payload 自包含(改标题/删 block 不改历史回放)
- [ ] `agent24d`:sin90 模块挂 `POST /directions`、`POST /proposals`、`POST /proposals/{id}/accept`、`GET /attention`
- [ ] Local 脑冒烟:`omlx` 拉 `mlx-community/Qwen3-0.6B` → 经 agent24-models provider 做一次意图分类,量 p95(SPIKE-03)
- [ ] 判定:塞入一串 block.completed 事件,`GET /api/v1/sin90/attention?window=week` 返回「Coding 18h / Business 2h」,全部来自事件回放
- [ ] Pet0 侧:一次性壳建 1 个 Direction + 渲染对账 → 绿灯即坐实

---

## 8. 内核侧遗留工作(SPIKE-00 前须与内核对齐)

这几项**动的是内核,不是 Sin90**:

1. ✅ **`agent24-protocol`**:通用 `EventBody::Module{module,kind,payload}` 变体 + JSON Schema(§4.2)。**已做 → PR #101**(schema/api-client 已重生成)。
2. ✅ **`agent24-models`**:`CompletionRequest` 加可选 `response_format`(`ResponseFormat::JsonSchema{name,schema,strict}`),OpenAI-兼容 adapter 透传(§5.1)。**已做 → PR #102**。**1e 冒烟验证**:oMLX 服务 `Qwen3-0.6B-4bit`,`response_format: json_schema` 强制合法 JSON,p95 **0.271s**(<500ms 目标)——无需新增 GGUF provider。
3. ⏳ **模块宿主**:`agent24d` 定义 `Sin90KernelCtx` 的进程内 adapter(`model()`/`scheduler()`/`events()`/`authz()`)+ 挂载 `/api/v1/sin90/*` 路由(§0)。**1d,进行中**。
4. ⏳ **ADR-026 单一事实源**:Sin90 的 API/事件随内核一样发布 OpenAPI/JSON Schema。

## 9. v0.3 改动记录(Codex 架构自审收口)

| # | 级别 | 问题 | 收口 |
|---|---|---|---|
| 1 | Critical | 审批与 apply 跨库不原子 | proposal/审批/apply 全落 sin90.db 单事务;内核 policy 只读放行(§0.1) |
| 2 | High | 内核副作用(scheduler)也跨库写 | apply 只写 `sin90_outbox`,reconciler 幂等落内核(§0.2) |
| 3 | High | pending proposal 无持久表 | 新增 `sin90_proposals`(§3.1),重启可恢复 |
| 4 | High | ctx 内联硬编码部署拓扑 | 边界改为 `Sin90KernelCtx` trait,进程内为第一 adapter(§0) |
| 5 | High | attention "纯回放"实为 join 可变表 | 事件 payload 自包含快照,回放不 join(§3.1/§3.2) |
| 6 | High | 物化视图无正确性协议 | 事件 `seq` 全序 + `applied_event_seq` 水位线 + 确定性 rebuild + 断言测试(§3) |
| 7 | High | 无 proposal 幂等,双 accept 重复 apply | `pending→applying` CAS(§2.3) |
| 8 | High | carried_over 链无约束 | `UNIQUE(carried_from)` + 原子 `CarryOverTask` op(§2.3/§3.1) |
| 9 | High | `ctx.model.route(...)` API 不存在 | 对齐真实 `ModelRouter::complete`;response_format 列内核待办(§5.1/§8) |
| 10 | High | `sin90.*` 强类型事件 vs 单向依赖冲突 | 通用 `EventBody::Module` 信封(§4.2/§8) |
| 11 | Medium | task 终态自相矛盾 | 终态集合含 carried_over(§2.2) |
| 12 | Medium | 命令 vs 事件语义混用 | 明确壳发命令(可拒)、只订阅事件(§4.1) |
| 13 | Medium | 镜像文档说用 agent24-store,与本稿矛盾 | 同步镜像:改为 agent24-sin90 模块自带 store |

未采纳(记录理由):sin90_events 全量 hash-chain(Codex #Medium)——单用户本地场景暂以 `seq` 全序 + append-only 达到可回放/可对账,防篡改留作 M3 隐私加固项,不进 SPIKE-00。
