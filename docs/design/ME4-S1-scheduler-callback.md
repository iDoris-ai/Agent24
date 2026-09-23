# ME4-S1 —— 调度回调 `_a24/scheduler/*` + fired 投递（ME4-1.1.1）

> **草稿 v1，待评审**（2026-09-23）。尚未经过任何一轮对抗评审。
> 评审方：Codex 额度 2026-09-29 19:28 前耗尽 → 按 `PLAN-ME4-OS-CAPABILITIES.md` §一 第 3 条，由**全新上下文的 Opus 子代理**做对抗评审，并在 `followups.md` 记 `ME4-CODEX-DEBT`，额度恢复后补审。
> 冻结条件（ME4-1.1.1 验收）：末轮 approve 且无 Critical/High；本文所有 Rust 片段在 scratch crate 里 `cargo check`/`clippy -D warnings`/`test` 通过（附录 A）；SPEC-ME3 §3 方法表出现 `_a24/scheduler/*` 行。
>
> | 轮次 | 版本 | 结论 | Critical | High | Medium | Low |
> |---|---|---|---|---|---|---|
> | — | v1 | 待送审 | — | — | — | — |
>
> 输入：`docs/agent/PLAN-ME4-OS-CAPABILITIES.md` §〇/§一/§二 S1（**硬约束是下限**，本文只收紧、不放松）/§三 ME4-1.1.1…1.5.1/§五；`docs/specs/SPEC-ME3-OUT-OF-PROCESS.md` §0 §2 §3 §5 §8 §9。
> 格式照 `docs/design/T8.5c-W-wire.md`。

## 版本改动记录

| 版本 | 改了什么 | 为什么 |
|---|---|---|
| v1（草稿） | 初稿。另吸收一条来自 Sin90 T3.1.1 对抗评审、已核实的补充输入：cron 0.15 的星期字段不是 POSIX（1..=7 且 1=周日）→ §6.3 裁决模块路径的星期字段只收 `*` 与英文缩写 | 统筹者 2026-09-23 转达；scratch crate 用真实 `next_fire` 复现：`0 7 * * 1-5` 从周六起算，下一次落在**周日**（附录 A 测试 `mon_fri_first_fire_is_a_monday` 的负对照） |

---

## 0. 这份文档解决什么、不解决什么

**解决**（PLAN S1 全部十条 + 留给设计的裁决点）：

1. `schedules` 的所有权与身份列、revision 规则、迁移 0007（S1-1）。
2. 模块投递不进 `ScheduleAction`；REST 对模块行的护栏；`user_suspended` 的 REST 入口（S1-2、S1-5）。
3. `ScheduleInvocation` / `FireOutcome` 的确切签名与 `RunTrigger` 迁移（S1-3）。
4. 幂等 upsert 的 SQL 与 outcome 判定、tick 回写的 revision CAS、`delete`/`list` 的形状（S1-4）。
5. 「不可用 ≠ 失败」的完整分类；scheduler 循环挪到 `mount_all` 之后；**hot-disable / uninstall 时 schedules 与未完成投递的去留**（S1-6，裁决见 §9）。
6. 投递器：按 owner 找 `Generation` 的只读 accessor、`admit_request` → `dispatch()` → 发送 → `finish()` 的完整流程、超时、结果分类（S1-7）。
7. 至少一次 + 扛崩溃：`schedule_deliveries` 的**完整状态机与迁移表**、确定性 `fire_id`、同次到点的重试次数与退避上限、过期策略、重启恢复流程、`run_now` 对模块行的语义（S1-8）。
8. 保留路径的规范化规则（percent-decoding、编码斜杠、`//`、`.`/`..`、大小写、非法序列）（S1-9）。
9. 准入、配额、令牌桶、`deny_unknown_fields`、生命周期绑定；ErrorKind 闭集是否扩展（S1-10，裁决：**不扩展**，§10.2）。
10. SPEC-ME3 的同步改写（§3 offer set 与方法表、§2.1 保留路径、§8 ME-4a 行、§9 删掉 Scheduler）。

**不解决**：

- `_a24/model/*`（ME4-S2 另一份文档）。
- 模块能读写**别的模块**或**用户**的 schedule —— 不存在这种方法，owner 永远是连接身份。
- 用户 / agent 路径（REST `POST /schedules`、self-wake）的 cron 星期字段歧义 —— 本轮**不改**，登记 followup（§6.3 取舍）。
- `Scheduler::update` 的 read-modify-write 在**两个并发 PATCH** 之间的丢失更新（既有问题，与本设计的 tick CAS 正交，§14 残余 R6）。
- 同 UID 的敌意进程直连模块 socket 伪造 fired（SPEC §0，本轮威胁模型外；§7 只保证「经内核 HTTP 代理进来的外部客户端伪造不了」）。

---

## 1. 现状（2026-09-23 在分支 `docs/me4-1.1.1-scheduler-design` 上逐条重读代码，行号为本次核对结果）

**scheduler crate**（`rust/crates/agent24-scheduler/src/lib.rs`）

- `RunTrigger::trigger(&self, action: &ScheduleAction, schedule_id: &str) -> Result<String, String>`（`lib.rs:53-56`）；成功值被当作 `run_id` 写进 `ScheduleFiredPayload`（`lib.rs:285-288`）。
- `create`（`lib.rs:101-125`）铸 `sch_<ulid>`，经 `upsert_schedule` 落盘；`update`（`lib.rs:140-179`）是 get → 改 → `upsert_schedule` 的 read-modify-write，改 `spec` 或切 `enabled` 时重算 `next_run_at` 并清零 `consecutive_failures`。
- `run_now`（`lib.rs:189-196`）直接 `trigger`，不看 `enabled`、不动 `next_run_at`、不走失败计数；错误映射成 `ScheduleError::Invalid`（REST 400）。
- `tick`（`lib.rs:202-236`）走 `list_schedules_lenient`，逐行 `fire`，单行出错只记日志。
- `fire`（`lib.rs:251-304`）：**先**用 `next_fire(spec, now)`（skip-missed）算出下一次，`update_schedule_runtime` 持久化（`lib.rs:269-276`，行已删则放弃），**再** `trigger`（`lib.rs:279`）。失败只 `consecutive_failures += 1`，不重试这一次；到 `MAX_CONSECUTIVE_FAILURES = 5`（`agent24-core/src/transitions.rs:98`）写 `enabled=false` 并发 `schedule.disabled`（`lib.rs:290-301`）。**两次持久化之间崩溃 = 这一次到点永久丢失**（pre-advance 已落、trigger 没发生）。
- `run(clock, tick_interval, cancel)`（`lib.rs:317-338`）：`clock.sleep(tick)` → `tick(now)`，可注入 `Clock`。

**`next_fire.rs`**

- `normalize_cron`（`next_fire.rs:39-48`）接受 5 段（补秒 `0`）**和 6 段（带秒段）**—— 6 段 cron 可以每秒触发；`every` 有 60s 下限（`next_fire.rs:13-14`）。
- 星期字段直接交给 `cron 0.15`：该 crate 的星期取值是 1..=7、**1=周日**，POSIX 是 0..=6、0=周日。scratch crate 实测：从周六 2026-09-26 12:00Z 起，`0 7 * * 1-5` 的下一次是**周日** 2026-09-27 07:00Z；`0 7 * * MON-FRI` 是周一 2026-09-28 07:00Z（附录 A）。

**store**（`rust/crates/agent24-store/`）

- `schedules(id PK, name, enabled, spec, action, delivery, last_run_at, next_run_at, consecutive_failures)`（`migrations/0001_initial.sql:57-67`），无 owner、无幂等键、无 revision。最新迁移是 `0006_module_approval_executed_at.sql`，本设计新增 **0007**。
- `upsert_schedule`（`repo.rs:493-517`）：`INSERT … ON CONFLICT(id) DO UPDATE SET` **覆盖包括 runtime 列在内的全部列**。调用方：`Scheduler::create/update`、`self_wake.rs:204-206`。
- `update_schedule_runtime`（`repo.rs:525-539`）：`UPDATE … SET enabled, last_run_at, next_run_at, consecutive_failures WHERE id = ?`，**无 CAS** —— 与一次并发 PATCH（改 spec）交错时，tick 会用旧 spec 算出的 `next_run_at` 覆盖新值。
- `delete_schedule`（`repo.rs:596-614`）`BEGIN IMMEDIATE`，连带删 `standing_grants`。
- 连接池 `max_connections(5)`、`busy_timeout 5s`、`foreign_keys(true)`（`agent24-store/src/lib.rs:56-59`）—— **外键已开**，`ON DELETE CASCADE` 可用。

**protocol**（`rust/crates/agent24-protocol/src/types.rs`）

- `ScheduleSpec`（`types.rs:649-661`，内部 tag，**无 `deny_unknown_fields`**）、`ScheduleAction::AgentRun`（`types.rs:664-676`，唯一变体）、`Schedule`（`types.rs:686-699`，`action: ScheduleAction` 必填）、`ScheduleCreate`（`types.rs:701-710`，**无 `deny_unknown_fields`**）、`ScheduleUpdate`（`types.rs:719-731`）。
- `ScheduleFiredPayload { schedule_id, run_id }`（`events.rs:207-211`）。事件 wire 名在 `events.rs:105-106`。`protocol/openapi.yaml`（`Schedule` 在 1559 行附近）、`protocol/events.schema.json`、`protocol/fixtures/events/schedule.*.json`、`packages/api-client` 都引用这些类型。

**daemon**（`rust/apps/agent24d/src/`）

- `schedules.rs`：`create_schedule` 直接 `serde_json::from_slice::<ScheduleCreate>`（`schedules.rs:42`）、`update_schedule` 解析 `ScheduleUpdate`（`schedules.rs:78`）后 `scheduler.update`（`schedules.rs:95`），**不做任何 owner/action 检查**；`run_now` 回 `202 {"run_id"}`（`schedules.rs:108-117`）。
- `server.rs`：`RunManagerTrigger`（`server.rs:298-327`）；`Scheduler::new`（`server.rs:504-510`，在 `AppState::new` 里，早于挂载）；**tick 循环先起**（`server.rs:1023-1037`），**`mount_all` 在后**（`server.rs:1250`）。
- `domain.rs`：`KERNEL_GRANTS = [Events, Memory, Approval]`（`domain.rs:74-75`）；`KERNEL_OOP_GRANTS = [Events, Approval, Memory]`（`domain.rs:93-94`）；`Supervisors` 注册表（`domain.rs:660-840`）：`running: Option<Vec<Supervised>>`（shutdown 后 `None`），`disable()` 把模块从 `running` 里 `swap_remove`（`domain.rs:774-816`），`Supervised { name, handle, current: Arc<Current> }`（`domain.rs:845-851`）；`mount_package`（`domain.rs:1275-1286`）**拿不到 scheduler**；`provides` 按授予逐项 push（`domain.rs:1413-1423`）；记忆的限流器在 `MethodsFor` 闭包**外**建、跨 generation 复用（`domain.rs:1438`），events 的限流器在闭包**内**、每代重建（`domain.rs:1459`）；`proxy::mount(app, &namespace, current)`（`domain.rs:1611`）。namespace 由 `DomainOsManifest::declared_namespace(name) = "/api/v1/{name}"`（`agent24-domain/src/lib.rs:478-480`）决定。
- `os_routes.rs`：hot-disable = `Supervisors::disable(name, DISABLE_DRAIN=30s, …)`（`os_routes.rs:300,332-342`）；`agent24 os uninstall` 由 **CLI 直接删包目录**（`agent24-cli/src/main.rs:529-548`，daemon 可以不在线），之后**尽力**`POST /api/v1/os/{name}/stop`（`main.rs:572-610`）—— **daemon 观察不到一次原子的「卸载」**。
- `memory_callback.rs:127,175`：`admit_callback_bound(request_id)` 拿 lifecycle 再交给带 `bind_to_lifecycle` 的存储调用；`events_emit.rs:390-393` 仍是两步 `request_lifecycle`；`approval_callback.rs` 完全没有 `bind_to_lifecycle`（FU-70，本设计不得复制）。
- `self_wake.rs:171-178`：按 `name == "self-wake" && enabled && next_run_at.is_some()` 数待醒数量（上限 32）—— **不看 owner**。模块若把 label 起成 `self-wake`，会把 agent 的 self-wake 配额占满（本设计要堵，§8.5）。

**os-proto**（`rust/crates/agent24-os-proto/src/`）

- `Generation::admit_request(id, token_hash, now, budget) -> Result<InFlight, RequestRefused>`（`drain.rs:416-448`）：只有 `Running` 准入，id 重复 → `DuplicateId`；在途表同时登记审批 token 与 lifecycle。
- `admit_callback_bound(request_id)`（`drain.rs:487-510`）：`Running` 下未知 id 仍 `Ok(None)`，`Draining` 下只放行在途 id。**只写一个 `X-A24-Request-Id` 头不构成在途请求**。
- `InFlight::dispatch()`（`drain.rs:945-952`，已 revoke 则 `false`）、`InFlight::revoked()`（`drain.rs:958-963`）、`InFlight::finish() -> Result<(), Abandoned>`（`drain.rs:975-1000`，提交点）、`InFlight::upstream()`（`drain.rs:922-924`）、`Drop` 自动出表（`drain.rs:1002`）。`InFlight::generation()` 是 `pub(crate)`。
- `bind_to_lifecycle(Option<RequestLifecycle>, work)`（`drain.rs:750-786`）；`Current::get()`（`drain.rs:886-893`）。
- `proxy.rs`：`proxy()`（`proxy.rs:989-1064`）先铸 id 与审批 token、`admit_request`（`proxy.rs:1015`）再 `forward`；请求头先 `sanitize_request_headers`（按前缀剥全部 `x-a24-*`，`proxy.rs:170-209`）再注入 `x-a24-request-id`/`x-a24-approval-token`（`proxy.rs:1224-1236`）；上游路径取 `OriginalUri` 原样（`proxy.rs:1249-1255`）；发送前 `dispatch()`（`proxy.rs:1305`）。`exchange`（`proxy.rs:758`）、`Upstream`、`mint_approval_token`（`proxy.rs:921-927`）、`RequestIds`（`proxy.rs:870-891`，形如 `<8hex>-<n>`）都是私有的。`normalise_dot_segments`（`proxy.rs:414-426`）与 `location_within`（`proxy.rs:348`）是响应侧 `Location` 的规范化先例。**请求路径今天不做任何保留前缀判断**。
- `rpc.rs`：`CALL_TIMEOUT = 30s`（`rpc.rs:82`）、`CANCEL_METHOD = "$/cancelRequest"`（`rpc.rs:95`）、`ErrorKind` 闭集 17 个（`rpc.rs:113-173`），`the_error_kinds_are_exactly_specs_closed_set` 用一段写死的 SPEC 原文钉住闭集（`rpc.rs:1936-1955`）。`MAX_FRAME_BYTES = 1 MiB`（`frame.rs:84`）。

**SPEC-ME3**：§3 开头写「本轮 offer set 是 `{Memory, Events, Approval}`——`Models`/`Scheduler`/`Policy` 都不在本轮（§9）」；方法表无 scheduler 行；§8 无 ME-4 行；§9「不做 `Models`/`Scheduler`/`Policy` 能力的回调」。

---

## 2. 决策 D1：存储 —— 迁移 0007 与 revision 规则

### 2.1 迁移（`rust/crates/agent24-store/migrations/0007_module_schedules.sql`，全文已在 SQLite 上执行验证）

```sql
ALTER TABLE schedules ADD COLUMN owner_module TEXT;
ALTER TABLE schedules ADD COLUMN module_key TEXT
    CHECK ((owner_module IS NULL) = (module_key IS NULL));
ALTER TABLE schedules ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE schedules ADD COLUMN user_suspended INTEGER NOT NULL DEFAULT 0
    CHECK (user_suspended IN (0, 1) AND (user_suspended = 0 OR owner_module IS NOT NULL));
ALTER TABLE schedules ADD COLUMN system_disabled_reason TEXT
    CHECK (system_disabled_reason IS NULL OR owner_module IS NOT NULL);

CREATE UNIQUE INDEX idx_schedules_module_key
    ON schedules (owner_module, module_key)
    WHERE owner_module IS NOT NULL;

CREATE TABLE schedule_deliveries (
    fire_id         TEXT PRIMARY KEY,
    schedule_id     TEXT NOT NULL REFERENCES schedules (id) ON DELETE CASCADE,
    owner_module    TEXT NOT NULL,
    module_key      TEXT NOT NULL,
    scheduled_for   TEXT NOT NULL,
    fired_at        TEXT NOT NULL,
    fire_trigger    TEXT NOT NULL CHECK (fire_trigger IN ('tick', 'run_now')),
    status          TEXT NOT NULL
        CHECK (status IN ('pending', 'deferred', 'delivered', 'failed', 'expired')),
    attempts        INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TEXT,
    expires_at      TEXT NOT NULL,
    last_error      TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    CHECK ((status IN ('pending', 'deferred')) = (next_attempt_at IS NOT NULL))
);
CREATE INDEX idx_schedule_deliveries_due
    ON schedule_deliveries (next_attempt_at)
    WHERE status IN ('pending', 'deferred');
CREATE INDEX idx_schedule_deliveries_schedule
    ON schedule_deliveries (schedule_id);
```

比 S1-1 更严的三处，各有理由：

- `user_suspended` / `system_disabled_reason` **只允许出现在模块行**（CHECK）。用户行已经有 `enabled` 这一个开关；让用户行也能 `user_suspended=1` 会长出第二个「暂停」语义，REST 的 PATCH `enabled` 与 suspend 互相打架。
- `schedule_deliveries` 比 S1-8 的列多 `owner_module/module_key/fired_at/fire_trigger/expires_at/created_at/updated_at`：`fired_at` 让每次重试的 body **逐字节相同**（从行里读，不是每次重取当下时间）；`expires_at` 是过期策略（§4.5）；`owner_module` 让投递器不必 join `schedules` 就能按 owner 分组。状态集比 S1-8 多一个 **`expired`**（终态，§4.3）。
- 列名用 `fire_trigger` 而不是 `trigger`：`TRIGGER` 是 SQLite 关键字。

模块行的 `action` 列（`NOT NULL`）写死为哨兵 `{"type":"module_delivery"}`（`MODULE_ACTION_SENTINEL`）。它**不是**一个 `ScheduleAction` 变体，行的含义只由 `owner_module` 决定；副作用是**降级安全**：旧二进制的宽松 tick 列表（`repo.rs:544-559`）把这种行当「不可读」跳过，不会把模块行当 AgentRun 触发。

旧行：五列全部走默认值（`NULL/NULL/0/0/NULL`），不改任何已有值 —— scratch 测试 `migration_keeps_old_rows_and_checks_hold` 先插一行 0001 形状的旧数据再跑 0007，断言原值不变。

### 2.2 revision 规则（一句话）

**凡是改变「这一行该在何时、是否触发」的写，`revision += 1`；tick 的 pre-advance（`last_run_at/next_run_at`）与失败计数器不 bump。**

逐个写者：

| 写者 | 改什么 | revision |
|---|---|---|
| 模块 `upsert`（`Created`） | 插入 | 置 1 |
| 模块 `upsert`（`Updated`） | spec/enabled/label，清 `system_disabled_reason` | +1 |
| 模块 `upsert`（`Unchanged`） | 不写 | 不变 |
| REST suspend / resume | `user_suspended`、`next_run_at` | +1 |
| 内核 system-disable（连续失败 / next_fire 出错） | `system_disabled_reason`、`next_run_at=NULL` | +1 |
| REST PATCH（用户行）、`create`、self-wake 经 `upsert_schedule` | 全列 | 更新 +1，插入 0 |
| tick pre-advance | `last_run_at`、`next_run_at` | **不变**（它是 CAS 的读者） |
| 投递结果 / AgentRun 结果 | `consecutive_failures` | 不变 |

`upsert_schedule` 的 SQL 同时加一道结构性护栏：`ON CONFLICT(id) DO UPDATE SET …, revision = schedules.revision + 1 WHERE schedules.owner_module IS NULL` —— 即使某个调用方忘了 §8 的检查，这条语句也**改不到模块行**（scratch 测试 `rest_upsert_cannot_touch_a_module_row`，带用户行正对照）。

### 2.3 tick 的 runtime 回写（revision CAS，S1-4）

```sql
UPDATE schedules SET last_run_at = ?, next_run_at = ?
 WHERE id = ? AND revision = ? AND next_run_at = ?
   AND enabled = 1 AND user_suspended = 0 AND system_disabled_reason IS NULL
```

参数依次：`now`、`advanced`（`next_fire(spec_seen, now)`）、`id`、tick 读到的 `revision`、tick 读到的 `next_run_at`（到点的那个 slot）。`rows_affected == 0` = **CAS 输**（行已删 / 配置变了 / 已不可触发 / 同一 slot 已被推进过）→ 整个事务回滚，本 tick 跳过这一行，**不 trigger、不写投递行**。于是「tick 用旧 spec 算出的 `next_run_at` 覆盖新 spec」在结构上不可能（scratch 测试 `tick_cas_loses_to_a_newer_spec`）。

**这条 CAS 对用户行（AgentRun）同样生效**。可观察差异只在并发窗口里：以前「tick 与 PATCH 交错」会让 tick 把旧值写回，现在 tick 让路。非竞态路径的行为逐字不变（判据 C2.7 回归）。AgentRun 的失败计数 / 失败禁用写也改成带 `AND revision = ?` 的版本（用户在两次之间编辑过 = 新的开始，旧计数作废，与 `update()` 清零计数的既有语义一致）。

---

## 3. 决策 D2：触发接口（S1-3）

### 3.1 签名（scratch `src/invocation.rs`，编译通过）

```rust
pub struct ModuleScheduleKey {
    pub owner_module: String,
    pub module_key: String,
}

pub enum FireTrigger { Tick, RunNow }

/// 两臂互斥：AgentRun 没有 fire_id（它没有投递行），模块投递没有 ScheduleAction（S1-2）。
pub enum InvocationTarget {
    AgentRun(ScheduleAction),
    Module { owner: ModuleScheduleKey, fire_id: FireId },
}

pub struct ScheduleInvocation {
    pub schedule_id: String,
    pub scheduled_for: DateTime<Utc>,
    pub fired_at: DateTime<Utc>,
    pub trigger: FireTrigger,
    pub target: InvocationTarget,
}

pub enum DeferReason { MountPending, NotRunning, NotReady, Draining, Stopping, NeverSent, KernelTransient }

pub enum FireOutcome {
    AgentRun { run_id: String },
    ModuleDelivered { fire_id: FireId },
    Deferred { reason: DeferReason },
    Failed { reason: String },
}

#[async_trait]
pub trait RunTrigger: Send + Sync {
    async fn trigger(&self, invocation: &ScheduleInvocation) -> FireOutcome;
}

pub enum RunNowOutcome { Run { run_id: String }, Fire { fire_id: FireId } }
```

与 PLAN S1-3 的字段表对照：`schedule_id / owner_module / module_key / scheduled_for / fired_at / fire_id` 全部在，只是 `owner_module/module_key/fire_id` 收进 `InvocationTarget::Module`。**这是收紧**：PLAN 的平铺写法要用三个 `Option` 表达，允许「有 owner 没 key」「AgentRun 带 fire_id」这类非法组合；枚举让它们不可表示，与迁移的 CHECK 同构。`trigger` 的返回值**不是 `Result`**：每一种失败都必须被分类成 `FireOutcome`，不存在一个可以 `?` 掉的错误。

### 3.2 谁调用 trigger

- **AgentRun 行**：仍在 tick 里内联调用（与今天相同位置、相同时序），先 CAS pre-advance（§2.3），再 `trigger`。`FireOutcome::AgentRun{run_id}` → 今天的成功分支（发 `schedule.fired{schedule_id, run_id}`，按 revision CAS 清零计数）；`Failed{reason}` → 今天的失败分支；其它变体对 AgentRun 目标是内核 bug，按失败处理（`agent_run_result`，scratch 已编译）。
- **模块行**：tick **不调用 trigger**。tick 在同一事务里 pre-advance + 写投递行（§4.2），然后唤醒投递泵（§5.4）；由泵对每个待投行调用 `trigger`，拿 `FireOutcome` 驱动状态机。这样一次慢投递（最长 10s）**不阻塞 tick**，而 trigger 仍是 AgentRun 与模块共用的一个接口。

### 3.3 `RunTrigger` 迁移（既有 AgentRun 路径行为不变）

`server.rs:298-327` 的 `RunManagerTrigger` 改名 `KernelTrigger`，持有 `runs` 与 `ModuleDeliverer`（§5）。AgentRun 臂是原函数体原样包一层：`Ok(run_id) → AgentRun{run_id}`、`Err(e) → Failed{reason: e}`（scratch `AgentRunAdapter` 编译了这层包装）。`Scheduler::run_now` 返回 `RunNowOutcome`：AgentRun 行与今天完全相同（`trigger` 失败仍映射 `ScheduleError::Invalid` → 400，成功 → `202 {"run_id"}`）；模块行见 §4.7。

agent24-scheduler 自己的测试替身 `RecordingTrigger`（`lib.rs:351-390`）同步改成新签名 —— 它是实现 PR 的机械改动，不改任何断言。

---

## 4. 决策 D3：投递语义 —— 至少一次、扛崩溃（S1-8）

### 4.1 语义一句话

**每一个被记录的 fire，要么以同一个 `fire_id` 被模块 2xx 确认至少一次，要么以一个可查询的终态（`failed` / `expired`）结束；「被记录」与「pre-advance」是同一个事务。**模块按 `fire_id` 去重。

### 4.2 确定性 `fire_id` 与记录事务

```rust
/// fire_ + 32 hex = SHA-256("agent24-fire-v1\0" || schedule_id || "\0" || fmt_iso(scheduled_for)) 的前 16 字节
pub fn derive(schedule_id: &str, scheduled_for: DateTime<Utc>) -> FireId
```

- 只依赖 `(schedule_id, scheduled_for)`，不依赖 attempt、tick 时刻、generation —— 同一 slot 的每次重试、崩溃后的每次续投都是同一个 id。`scheduled_for` 先 `fmt_iso`（秒精度 `Z`），一个 slot 只有一种拼写。
- **不直接拼 `schedule_id`**：`schedule_id` 是内核内部 id，模块不需要知道；模块删 key 再重建会得到新 `schedule_id`，于是新行的 fire 不会与旧行的 fire 撞 id（这是对的：那是一条新 schedule）。
- 不是秘密。

tick 对模块行：在**一个** `BEGIN IMMEDIATE` 事务里（scratch `advance_and_record_fire`）：
1. §2.3 的 CAS pre-advance；输了 → 回滚，什么都不写。
2. **取代**同一 schedule 更早的未完成投递：`UPDATE schedule_deliveries SET status='expired', next_attempt_at=NULL, last_error='superseded' WHERE schedule_id=? AND status IN ('pending','deferred') AND fire_id <> ?`。
3. `INSERT … status='pending', attempts=0, next_attempt_at=now, expires_at=fired_at+24h ON CONFLICT(fire_id) DO NOTHING`。

崩溃分析：1–3 要么全落要么全不落。全不落 → `next_run_at` 没动，下一次 tick 读到同一个到点 slot，算出**同一个 fire_id**。全落 → 行在表里，重启后泵续投（§4.6）。**「pre-advance 之后、trigger 之前崩溃丢一次」这个窗口不复存在。**

取代（第 2 步）的理由：它是既有 skip-missed 哲学（`lib.rs:6-10`：daemon 停机期间错过的 slot 不回放，只触发一次）在投递层的同一句话 —— 模块长期不可用时，**同一 schedule 最多只有一条未完成投递**，醒来收到的是最新的那一次，而不是一串积压；也让未完成投递行数有界（≤ 模块行数 ≤ 256/模块）。

### 4.3 状态机（完整迁移表）

状态：`pending`、`deferred`（非终态）；`delivered`、`failed`、`expired`（终态，任何事件都不再改变它们）。

| # | 从 | 事件 | 到 | attempts | next_attempt_at | 对 schedule 的副作用 | 事件 |
|---|---|---|---|---|---|---|---|
| T1 | （无） | tick / run_now 记录 fire | pending | 0 | now | tick：CAS pre-advance（同事务） | — |
| T2 | pending / deferred | 投递 `ModuleDelivered`（2xx） | delivered | +1 | NULL | `consecutive_failures=0` | `schedule.delivered` |
| T3 | pending | `Deferred{reason}` | deferred | 不变 | now（由泵的 owner 跳过缓存挡住，§5.4） | 无 | — |
| T4 | deferred | `Deferred{reason}` | deferred | 不变 | 不变 | 无 | —（**不写库**） |
| T5 | pending / deferred | `Failed`，且 attempts+1 < 3 | pending | +1 | now + [5s, 15s][attempts] | 无 | — |
| T6 | pending / deferred | `Failed`，且 attempts+1 = 3 | failed | 3 | NULL | `consecutive_failures += 1`；到 5 → `system_disabled_reason='consecutive_failures'`、`next_run_at=NULL`、revision+1 | 首次越线发 `schedule.disabled` |
| T7 | pending / deferred | `expires_at <= now`（清扫） | expired | 不变 | NULL | 无 | — |
| T8 | pending / deferred | 同 schedule 记录了新 fire | expired（`superseded`） | 不变 | NULL | 无 | — |
| T9 | pending / deferred | 模块 upsert 改了 spec 或把 `enabled` 关掉；用户 suspend | expired（`superseded_by_upsert` / `suspended`） | 不变 | NULL | 同事务 | — |
| T10 | 任意 | schedule 行被删（模块 delete / REST DELETE） | （行消失，FK 级联） | — | — | — | — |
| T11 | delivered / failed / expired | 最后更新超过 24h（GC） | （行删除） | — | — | — | — |

- T2/T5/T6 由泵按 `(fire_id, status ∈ {pending,deferred}, attempts = 读到的值)` **CAS** 落库，与该 schedule 的计数器同一事务（scratch `apply_delivery_outcome`）。CAS 输（被 T7/T8/T9/T10 抢先）→ 结果丢弃；即便那次其实已 2xx，也不补发事件 —— 模块已经收到了，丢的只是一条通知。
- 状态机本身是纯函数 `apply_outcome(from, attempts, &FireOutcome, now) -> Result<Applied, NotApplied>`（scratch `src/delivery.rs`，含单测：三次失败只计一次失败；一百次延迟不写库也不计数；终态不动）。

### 4.4 重试次数与退避上限（S1-8 裁决）

- **每个 fire 最多 3 次「已发出」的尝试**（`MAX_SENT_ATTEMPTS = 3`，含第一次）；间隔 **5s、15s**；每次尝试端到端上限 **10s**（`DELIVERY_TIMEOUT`，从 `admit_request` 到读完响应）。
- 最坏总时长 = 10 + 5 + 10 + 15 + 10 = **50s < 60s**，而模块行的最短周期是 60s（`every ≥ 60`、cron 只收 5 段，§6.3）—— 所以「总时长 ≤ 下一次到点」对模块行**恒成立**（scratch 断言 `worst_case_retry_span_is_under_the_minimum_period`）。就算某种组合让下一次到点先来了，T8 取代也保证同一 schedule 不会并存两条未完成投递。
- **`Deferred` 不是尝试**：不增加 `attempts`，不计失败，不消耗退避。只有「模块 Running 且请求真的发出去了（或连它活着的 generation 都连不上）」才是尝试。
- 只有第 3 次已发出的尝试也失败，才 `failed` 并计**一次** `consecutive_failures`（不是三次）。5 次 `failed` 的 fire（不是 5 次尝试）→ system-disable。

### 4.5 过期策略（S1-6/S1-8 裁决）

- `expires_at = fired_at + 24h`（`DELIVERY_TTL`）。**从 `fired_at` 起算，不从 `scheduled_for`**：daemon 停机三天后启动，skip-missed 触发的那一次 `scheduled_for` 是三天前，若从它起算会立刻过期 —— 那等于把 skip-missed 的「触发一次」吞掉。
- 过期清扫与 GC 每 60s 一次（`SWEEP_INTERVAL`），不是每轮泵 —— 两条都是写语句，没必要每秒抢写锁。
- 过期是终态、**不计失败**：过期意味着模块在 24h 内一直没处于可投状态（或被新 slot 取代），那是「不可用」，不是「投递失败」。

### 4.6 重启恢复流程

1. 迁移 → `Store::open`。
2. `AppState::new` 建 `Scheduler`（带 `KernelTrigger`，其 `ModuleDeliverer.supervisors` 是空的 `OnceLock`）。
3. `serve` → `mount_all`（模块起进程，generation 处于 `Starting`）。
4. **`mount_all` 返回后**：`deliverer.supervisors.set(host.supervisors.clone())` → 起 tick 循环 → 起投递泵（S1-6：「scheduler 循环在 `mount_all` 完成之后才启动」，现 `server.rs:1023-1037` 整段挪到 `server.rs:1250` 之后）。
5. 泵第一轮：`pending/deferred` 行按 `scheduled_for` 取出（**沿用行里的 `fire_id`、`fired_at`、`attempts`**）；模块多半还在握手 → `Deferred(NotReady)`（T3，不计数）；握手完成、generation `Running` → 投递 → `delivered`（T2）。崩溃前已 `delivered` 的行是终态，**不重投**。
6. tick 第一轮：停机期间到点的行按 skip-missed 各记录一次 fire；若该 schedule 还有崩溃前留下的未完成行，按 T8 被新 slot 取代。
7. 崩溃打断的那次尝试（请求已发、结果未落库）不计入 `attempts`，重启后以同一 `fire_id` 再投 —— 这正是「至少一次」要付的重复，模块去重。

### 4.7 `run_now` 对模块行（S1-8 裁决）

- `Scheduler::run_now(id)`：先在一个 `BEGIN IMMEDIATE` 里判是否模块行（scratch `record_run_now_fire`）。是 → `scheduled_for = fired_at = now`（秒精度）、`fire_trigger='run_now'`、`fire_id = derive(schedule_id, now)`，同事务取代旧的未完成投递（T8）并插入 `pending`；**不动 `next_run_at`**（与今天的 run_now 一致）；唤醒泵；返回 `RunNowOutcome::Fire{fire_id}` → REST `202 {"fire_id": …}`。
- **与 tick 完全相同的投递 / 延迟 / 失败语义**：它就是状态机里的一行，走同一个泵、同一张迁移表，失败同样计 `consecutive_failures`（S1-8：「run_now 对模块行走与 tick 相同的失败/延迟语义」）。
- 与今天一样**不看** `enabled`；同理也不看 `user_suspended` / `system_disabled_reason` —— run_now 是用户显式点的，意思就是「现在试一次」（它是用户在 system-disable 之后验证模块是否修好的唯一手段）。
- 同一秒内连点两次 → 同一个 `fire_id`，`INSERT … DO NOTHING`，第二次返回同一个 id：幂等，不是错误。
- 是用户行 → 今天的路径原样执行。

---

## 5. 决策 D4：投递器（S1-7）

### 5.1 按 owner 找当前 Generation 的只读 accessor

加在 `agent24d/src/domain.rs` 的 `Supervisors` 上（scratch `src/accessor.rs` 用字段一致的镜像结构编译了同一个方法体）：

```rust
impl Supervisors {
    /// 模块 `name` 此刻在 running 列表里才返回它的代理槽；从未启动 / os.json 禁用 /
    /// hot-disable（`disable()` 已 swap_remove）/ shutdown 关表之后一律 None。
    /// 只读：克隆一个 Arc，拿与 `statuses()` 同一把短锁。
    #[must_use]
    pub fn running_slot(&self, name: &str) -> Option<Arc<Current>> {
        self.lock()
            .running
            .as_ref()?
            .iter()
            .find(|s| s.name == name)
            .map(|s| Arc::clone(&s.current))
    }
}
```

「在列表里」只说明有 supervisor 持有它，不说明 ready：`Starting/Draining/Revoked` 交给 `current.get().admit_request(..)` 判，与代理同一个准入。

`ModuleDeliverer { supervisors: OnceLock<Arc<Supervisors>>, ids: KernelRequestIds }`：未 set → `Deferred(MountPending)`。

### 5.2 内核主动请求：新增 `agent24-os-proto/src/kernel_call.rs`

投递不走 loopback HTTP + bearer，而是直接用代理的 UDS 上游（S1-7）。`exchange`/`Upstream`/`mint_approval_token`/`sha256` 是 `proxy.rs`/`drain.rs` 的私有件，所以新代码放进 **os-proto 内部**（这些函数改成 `pub(crate)`），对外只暴露：

```rust
pub struct KernelRequest { pub path: String, pub extra_headers: Vec<(HeaderName, HeaderValue)>, pub body: Bytes }
pub struct KernelLimits { pub total: Duration, pub max_response_bytes: usize }
pub struct KernelResponse { pub status: StatusCode, pub body: Bytes }
pub enum KernelCallError { Refused(RequestRefused), EntropyUnavailable, NotDispatched, Abandoned(Abandoned),
                           NotSent(String), MaybeSent(String), Timeout, ResponseTooLarge, NoUpstream }
pub struct KernelRequestIds { /* "sch-<8hex>-<n>" */ }

pub async fn send_kernel_request(
    generation: &Arc<Generation>,
    ids: &KernelRequestIds,
    request: KernelRequest,
    limits: KernelLimits,
) -> Result<KernelResponse, KernelCallError>;

pub const SCHEDULE_KEY_HEADER: &str = "x-a24-schedule-key";
pub const FIRE_ID_HEADER: &str = "x-a24-fire-id";
```

流程（scratch 里用真实的公开 `Generation`/`InFlight` API 完整实现并编译，只有物理发送用一个同形桩代替私有 `exchange`）：

1. 铸 id（`sch-<8hex>-<n>`，与代理的 `<8hex>-<n>` 形状不相交，不会撞 `DuplicateId`）与审批 token（`/dev/urandom`，无降级；失败 → `EntropyUnavailable`，什么都没准入）。
2. `generation.admit_request(id, sha256(token), Instant::now(), limits.total)` → 持有 `InFlight`。**budget = 10s**：绑在这个 request id 上的回调，其 lifecycle 截止时间就是投递截止时间。
3. 构造请求：`POST <path>`，头 = 调用方给的 `x-a24-schedule-key`/`x-a24-fire-id` + `x-a24-request-id` + `x-a24-approval-token` + `Host: agent24-module.invalid` + `Content-Type: application/json`。审批 token 也注入 —— fired handler 与任何被代理请求一样，可以在处理期间提交一次 `_a24/approval/advise`。
4. **最后一步才是 `in_flight.dispatch()`**；`false`（准入之后被 revoke）→ `NotDispatched`，**一个字节都不发**（S1-7）。
5. `exchange(…, force_fresh = true, None, Some(&|| in_flight.dispatch()))`：**每次新连接、不入池**（内核请求不幂等，不给复用连接留任何半关闭窗口），连接之后发送之前再查一次 `dispatch()`（沿用 FU-64 的 `send_guard`）。
6. 整个交换与 `in_flight.revoked()` 赛跑（同 `proxy()`），外层 `timeout(limits.total)`；响应 body 读上限 64 KiB（读完即弃）。
7. **`in_flight.finish()` 是提交点且优先于传输结果**：`Err(Abandoned)` → `KernelCallError::Abandoned`，不管传输层说了什么。

InFlight 的生命周期因此是：`admit_request` 起 → 响应读完或超时或被 revoke 止，**始终由 `send_kernel_request` 的栈帧持有**；任何 early return / panic / 被取消都经 `Drop` 出表（`drain.rs:1002`）。**投递进行中，模块 fired handler 用 `X-A24-Request-Id` 发的回调在 Draining 期间仍能通过 `admit_callback_bound`；投递结束的那一刻起同一个 id 就不能**（判据 C4.6）。hot-disable 的 drain 宽限是 30s（`os_routes.rs:300`）> 10s 投递上限，所以正常情况下投递在宽限内自然结束。

### 5.3 路径与 body；结果分类

- 路径：`DomainOsManifest::declared_namespace(owner) + "/_a24/scheduler/fired"`，即 `/api/v1/<ns>/_a24/scheduler/fired`。
- body（`FiredBody`，scratch 已编译）：`{"key", "scheduled_for", "fired_at"}`，三个值都从投递行读 —— **每次重试逐字节相同**。
- 头：`X-A24-Schedule-Key: <key>`、`X-A24-Fire-Id: <fire_id>`、`X-A24-Request-Id`、`X-A24-Approval-Token`。

分类（scratch `classify`，编译通过）—— 规则一句话：**没发出去 ⇒ `Deferred`；发出去了（或它活着的 generation 连不上）且不是 2xx ⇒ `Failed`**：

| 传输结果 | FireOutcome |
|---|---|
| 2xx | `ModuleDelivered{fire_id}` |
| 非 2xx | `Failed` |
| `running_slot` 为 None | `Deferred(NotRunning)` |
| `OnceLock` 未 set | `Deferred(MountPending)` |
| `Refused(NotReady / Draining / Stopping)` | `Deferred(NotReady / Draining / Stopping)` |
| `Refused(DuplicateId)`、`EntropyUnavailable` | `Deferred(KernelTransient)` |
| `NotDispatched`、`Abandoned{dispatched:false}` | `Deferred(NeverSent)` |
| `Abandoned{dispatched:true}` | **`Failed`**（见下） |
| `NotSent`（generation 未被 revoke 却连不上）、`MaybeSent`、`Timeout`、`ResponseTooLarge` | `Failed` |

`Abandoned{dispatched:true}`（请求已发出，随后 generation 被 revoke）算一次**已发出的失败尝试**，而不是延迟：
- 至少一次要求重投（模块可能执行了也可能没有）—— 它照样会被重投（T5）；
- 但若算延迟，一个**每收到这次 fired 就崩溃**的模块会无限循环「投递 → 崩溃 → 重启 → 再投」，直到熔断，然后 daemon 重启后再来一轮。算作尝试后，第 3 次就 `failed`，毒丸被截断。
- 代价：hot-disable 恰好落在一次 fired 处理超过宽限（30s）时，会记一次失败尝试（不是一次 `consecutive_failures`，需要三次）。可接受，列入 §14 R4。

### 5.4 投递泵（`agent24-scheduler/src/deliveries.rs`）

- 独立于 tick 的任务（同一个 `CancellationToken`、可注入的 `Clock`）；每 1s 一轮（`PUMP_INTERVAL`）+ tick/run_now 记录 fire 后 `Notify` 立即唤醒。
- 取行（scratch 已在 SQLite 上执行）：
  ```sql
  SELECT fire_id, schedule_id, owner_module, module_key, scheduled_for, fired_at, fire_trigger, status, attempts
  FROM (SELECT *, ROW_NUMBER() OVER (PARTITION BY owner_module ORDER BY scheduled_for, fire_id) AS rn
        FROM schedule_deliveries
        WHERE status IN ('pending','deferred') AND next_attempt_at <= ?1 AND expires_at > ?1
          AND owner_module NOT IN (SELECT value FROM json_each(?2)))
  WHERE rn <= ?3 ORDER BY scheduled_for, fire_id LIMIT ?4
  ```
  `?2` = **owner 跳过缓存**（JSON 数组）：某 owner 本轮回了 `Deferred`，2s 内（`DEFER_RECHECK`）不再取它的行。于是一个死掉的模块每 2s 只花**一次探测、零次写库**（T4 不写），也占不住查询窗口；`?3 = 4` 每 owner 至多 4 行，一个 owner 的积压挤不掉别人（scratch `due_query_is_fair_skips_cached_owners_and_spec_change_retires_fires`）。
- 并发：每 owner 至多 4 个在途尝试、全局 16 个（`JoinSet` + 两个计数）；**同一 schedule 同时至多一个在途尝试**（内存里的 in-flight schedule 集合，取到的行若其 schedule 已在途则本轮跳过）。
- 每个尝试：从行构造 `ScheduleInvocation`（`fired_at`/`scheduled_for` 取行里的值）→ `trigger.trigger(&inv).await` → `apply_outcome` → `apply_delivery_outcome`（CAS）→ 成功则发事件。
- 取消：`cancel` 触发 → 停止取行，`JoinSet` 析构中止在途尝试 → `InFlight` 经 `Drop` 出表，行仍是 `pending`/`deferred`，下次启动续投（§4.6）。
- **超时不阻塞 tick**：tick 从不 await 投递（§3.2），泵自己的尝试也是 spawn 的；一次卡满 10s 的投递只占一个在途名额。

### 5.5 事件

新增 `EventBody::ScheduleDelivered(ScheduleDeliveredPayload)`，wire 名 `schedule.delivered`，payload `{schedule_id, module, key, fire_id, scheduled_for}`，在 T2 落库之后发。**不冒充 `run_id`**：`schedule.fired{schedule_id, run_id}` 只给 AgentRun 行。模块行达到失败上限时复用既有的 `schedule.disabled{schedule_id, reason:"consecutive_failures"}`。

---

## 6. 决策 D5：回调 handler `_a24/scheduler/{upsert,delete,list}`（S1-4、S1-10）

### 6.1 参数与返回（scratch `src/params.rs`，含 `deny_unknown_fields` 各层的单测）

```rust
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModuleSpec { Cron { expr: String, #[serde(default)] tz: Option<String> }, Every { secs: u32 }, At { ts: String } }

#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct SchedulerUpsertParams {
    pub key: String,
    pub spec: ModuleSpec,
    #[serde(default = "default_true")] pub enabled: bool,   // 期望状态语义：缺省 = true，不是「保持」
    #[serde(default)] pub label: Option<String>,             // 缺省 = key
    #[serde(default)] pub request_id: Option<String>,
    #[serde(default)] pub _meta: Option<Map<String, Value>>,
}
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct SchedulerDeleteParams { pub key: String, request_id: Option<String>, _meta: Option<Map<String, Value>> }
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct SchedulerListParams { request_id: Option<String>, _meta: Option<Map<String, Value>> }

#[derive(Serialize)]
pub struct ModuleScheduleState {
    pub key: String, pub spec: ScheduleSpec, pub enabled: bool, pub label: String,
    pub user_suspended: bool, pub system_disabled_reason: Option<String>, pub next_run_at: Option<String>,
}
// upsert → {"outcome": "created"|"updated"|"unchanged", "schedule": ModuleScheduleState}
// delete → {"outcome": "deleted"|"absent"}
// list   → {"schedules": [ModuleScheduleState, …]}   按 key 排序，≤ 256 条，不分页
```

- 模块侧的 spec **不是** `agent24_protocol::ScheduleSpec` 本身：后者在变体内部不拒未知键（`types.rs:649`），改它会改 REST 契约。`ModuleSpec` 在每一层都严格，再转换成 `ScheduleSpec` 存库。scratch 断言 `{"type":"every","secs":60,"x":1}` 与 `{"type":"cron",…,"zone":"UTC"}` 都是解析失败。
- params 里**没有** owner 字段；顶层带 `owner_module` 是解析失败；`_meta` 里的任何东西（含 `owner_module`/`org`）都不被读（S1-10）。
- `list` 返回**完整期望状态**（S1-4）：`spec/enabled/label` 是模块自己能比对的期望，`user_suspended/system_disabled_reason` 告诉它内核为什么没按期望触发，`next_run_at` 供诊断。**从不返回内核的 `schedule_id`**。256 条 × 每条 < 1 KiB（key ≤128、label ≤128、cron ≤128、tz ≤64、ts ≤64）远低于 1 MiB 帧上限，所以不分页。

### 6.2 upsert 的 SQL 与 outcome 判定（S1-4；scratch `upsert_module_schedule` 全文在 SQLite 上执行）

调用方先在事务外算好 `next_if_recomputed = next_fire(spec, now)`（纯函数），然后一个 `BEGIN IMMEDIATE` 事务：

1. `SELECT … FROM schedules WHERE owner_module = ?owner AND module_key = ?key`。
2. **不存在**：`SELECT COUNT(*) FROM schedules WHERE owner_module = ?owner`，`>= 256` → `QuotaExceeded`（事务回滚，什么都没写 —— 配额在写锁内计数，两个并发 upsert 不可能一起越线）；否则 `INSERT`（新 `sch_<ulid>`、`action = 哨兵`、`revision = 1`、`next_run_at = enabled ? next_if_recomputed : NULL`）→ **`created`**。
3. **存在**：比较 `spec`（反序列化后 `ScheduleSpec` 的 `PartialEq`，不比 JSON 字符串）、`enabled`、`label`，以及 `system_disabled_reason`：
   - 三者都相同且 `system_disabled_reason IS NULL` → **`unchanged`**，不写、不 bump。
   - 否则 `UPDATE … SET name, enabled, spec, next_run_at = ?, consecutive_failures = CASE WHEN ?recompute THEN 0 ELSE consecutive_failures END, system_disabled_reason = NULL, revision = revision + 1 WHERE id = ? AND revision = ?` → **`updated`**。其中 `recompute = spec 变 ∨ enabled 变 ∨ 原先被 system-disable`；`recompute` 时 `next_run_at = (enabled ∧ ¬user_suspended) ? next_if_recomputed : NULL`，否则保留原值（只改 label 不重算）。
   - spec 变了或 `enabled` 被关掉 → 同事务把该 schedule 的未完成投递置 `expired`（T9）。
   - **`user_suspended` 永远不被 upsert 读来决定写什么以外的东西，更不被清除**（S1-5）。
4. 同事务读回 `ModuleScheduleState`，提交。

两连接 + barrier 并发 upsert 同一 key → 一行、一个 `created` 一个 `unchanged`（scratch `concurrent_upserts_on_two_connections_make_one_row`；正对照：不同 key 两行）。

三种「不启用」（S1-5）在这里各有唯一的写者：`enabled` 只由模块 upsert 写；`user_suspended` 只由 REST suspend/resume 写；`system_disabled_reason` 由内核置、由模块的**任意一次**upsert 清（即便期望状态没变 —— 此时 outcome 是 `updated` 而非 `unchanged`，scratch `three_failed_deliveries_x5_system_disable_and_upsert_clears`）。

`delete{key}`：`DELETE FROM schedules WHERE owner_module = ?owner AND module_key = ?key`；`rows_affected == 0` → `{outcome: "absent"}`，不是错误。投递行经外键级联删除。A 删 B 的 key：WHERE 子句带 A 的 owner，B 的行不可达 → `absent` 且 B 的行还在。

### 6.3 key / label / spec 校验（`check_params` 内完成，失败 = `-32602`）

- key：`[a-z0-9._-]{1,128}`，逐字节，不折叠大小写、不 trim（S1-10）。
- label：1..=128 个字符、无控制字符；缺省 = key。
- spec：先过共享的 `next_fire::validate`（S1-10），**再加三条模块专属收紧**：
  1. **cron 只收 5 段**：6 段带秒段，可以每秒触发；模块行保持与 `every` 相同的 60s 下限，也是 §4.4「重试总时长 < 最短周期」的前提。
  2. **星期字段只收 `*` 与英文缩写**：`*`，或逗号列表，每项是 `SUN|MON|TUE|WED|THU|FRI|SAT` 或 `名-名`（ASCII 不区分大小写）。**任何数字、`/`、`?`、`L`、`#` 一律拒**。理由：`cron 0.15` 的星期 1..=7 且 1=周日，POSIX 是 0..=6 且 0=周日 —— `1-5` 对前者是周日到周四、对后者是周一到周五，内核无从判断模块想的是哪一个；名字在两种语义下都没有歧义。错误文案直接说原因（scratch `validate_dow`）。
  3. 字符串上限：cron expr ≤ 128 字节、tz ≤ 64、`at.ts` ≤ 64。
- **取舍（用户 / agent 路径不收紧）**：REST `POST /schedules` 与 self-wake 本轮**不改**校验 —— 收紧它们会让今天能创建的行（含 6 段 cron、数字星期）变成 400，是一次对现有用户的行为变更，且 self-wake 只产 `At`。登记 followup：`FU-ME4-DOW`（REST 路径的数字星期歧义：要么同样只收名字，要么在 `normalize_cron` 里把 POSIX 数字翻译成 cron crate 的数字，二选一需要用户裁决，因为两者对已存行的含义不同）。Sin90 侧同步采用同一条规则（统筹者已转达）。

### 6.4 准入、配额、令牌桶、生命周期（S1-10）

`call()` 内的固定顺序（照 `memory_callback.rs`，不复制 FU-70）：

1. `check_params` 已过（结构 + §6.3 的语义校验）。
2. `granted.has(Capability::Scheduler)` 否 → `forbidden`（方法**恒注册**，`rpc.rs` 模块文档的原则）。
3. `generation.admit_callback_bound(params.request_id)` → `not_ready` / `draining` / `revoked`（`events_emit::refused_error`）；**一次加锁**同时拿到 lifecycle。
4. 令牌桶 `try_acquire()` 否 → `rate_limited`。桶**每挂载一个、在 `MethodsFor` 闭包外建**（照 `domain.rs:1438` 的记忆限流器，不照 events 的每代重建），容量 64、每秒回填 1 —— 跨模块重启不重置（S1-10）。三个方法各耗 1 个。
5. `bind_to_lifecycle(lifecycle, store_op).await`：`BudgetExhausted` / `RequestEnded` → `timeout`（同 events 的文案）。存储事务被取消 = 回滚；已提交而响应丢失 → 模块重试 → upsert 幂等（`unchanged`）。
6. 结果 / 错误映射：`QuotaExceeded` → `quota_exceeded`；存储错误 → `-32603` 且消息固定为 `"storage error"`（不带 SQL、路径、owner —— SPEC §3「error.data 不得出现内核内部信息」）。

配额 256 行/模块，计数在写锁内（§6.2）；删除释放名额；已存在 key 的 upsert 在满额时照常成功。

### 6.5 接线

- `KERNEL_OOP_GRANTS` 加 `Capability::Scheduler`；**`KERNEL_GRANTS`（进程内）不加** —— 进程内没有 scheduler 句柄，授予一个没有句柄的能力是撒谎（`domain.rs:62-66` 自己写下的规矩）。`KERNEL_OOP_GRANTS` 的文档注释「Narrower than KERNEL_GRANTS」同步改写（本仓 T11 后不再编译进程内模块，这一维「更宽」是有句柄支撑的）。
- `mount_all` / `mount_package` 多一个参数 `scheduler: &Arc<Scheduler>`；`provides` 在 `granted.has(Scheduler)` 时 push `"_a24/scheduler/"`；`Methods` 恒注册三个方法（handler 结构体持 `generation, module, granted, scheduler, limiter`）。
- **offer set 阶梯**：`Scheduler` 进 `KERNEL_OOP_GRANTS` 与 `provides` 的改动落在 ME4-1.4.1（handler 同 PR），不早于它 —— SPEC §8「生产 offer set 只包含已有 handler 的能力」。

---

## 7. 决策 D6：保留路径 `/api/v1/<ns>/_a24/…`（S1-9）

### 7.1 放在哪

`proxy()`（`proxy.rs:989`）的**最前面**，铸 id、`admit_request` 之前：命中 → 404，在途表里什么都没登记，上游零请求。判定输入是 `OriginalUri::path()`（原始、未解码、不含 query）与本命名空间 `/api/v1/<ns>`（axum 的 `nest` 已经按字节精确匹配过它）。**转发给模块的仍是原始路径**（今天的行为不变）；规范化只用于判定。

### 7.2 规则（scratch `src/reserved_path.rs` 是参考实现，带变体矩阵与正对照单测）

1. 路径必须以 namespace 开头，且余下部分为空或以 `/` 开头；否则**拒**（不可达，fail-closed）。
2. 余下部分按原始 `/` 切段。对每段：
   - 原始段里出现 `\` → **拒**。
   - **第 1 轮严格解码**：`%` 后不是两个十六进制数字 → **拒**（非法序列）。
   - **第 2–4 轮宽松解码**（只解合法的 `%XX`，其余原样），直到不动点；4 轮后仍在变 → **拒**（病态嵌套）。这一步让 `%255f` → `%5f` → `_` 这类「每经一层框架剥一层」的编码也被看穿。
   - **每一轮的中间结果**都必须是合法 UTF-8，且不含 `/`、`\`、NUL 与任何控制字符 → 否则**拒**。于是编码斜杠（`%2F`、`%2f`、`%252F`）、编码反斜杠、超长 UTF-8（`%C0%AF`）都在任何深度被拒。
3. 用**解码后的最终形式**处理点段：`""`（即 `//` 与末尾 `/`）与 `.` 丢弃；`..` 弹栈，栈空时弹 → **拒**（越出命名空间根，同 `normalise_dot_segments` 的「拒而不夹」）。`%2e%2e`、`.%2E` 都按 `..` 处理。
4. 余下的**第一段**：截到第一个 `;`（路径参数）、`?` 或 `#`（被解码出来的分隔符，防止粗心模块再解析一次）为止，**ASCII 不区分大小写**等于 `_a24` → **保留**。
5. 保留与拒都回同一个 `404 not_found`，同一个 body，不转发。其余照常转发原始路径。

大小写的裁决：**`_a24` 段不区分 ASCII 大小写**。内核判不了模块用的路由器是否大小写敏感（ASP.NET、部分 Node 路由默认不敏感），fail-closed。非 ASCII 的「看起来像」（全角 `＿ａ２４`、Unicode 兼容等价）**不处理**，列入残余 R2。

### 7.3 代价（如实写）

规则 2 是**对所有模块路径**生效的，不只是 `_a24` 开头的：带编码斜杠、非法 `%`、编码控制字符的请求从此 404。这是 S1-9「拒绝编码斜杠与非法序列」要的 fail-closed —— 一个会解码路径的模块能把 `x%2F..%2F_a24/scheduler/fired` 解释成 fired 路由，内核在转发之前无法知道它会不会这样做。合法地需要在路径段里放字面 `/`（`%2F`）的模块改用 query 参数。与 `location_within` 对 `%2e` 的取舍同形（SPEC §2.1）。

### 7.4 它保证什么、不保证什么（S1-9）

保证：**经内核 HTTP 代理进入的外部客户端无法把请求送到 `/api/v1/<ns>/_a24/…`**，所以无法伪造 fired。**不保证**「fired 只可能来自内核」：同 UID 进程可以直连模块的 UDS（SPEC §0）。模块若想把 fired 与伪造区分开，本轮能依赖的只有这一条；`X-A24-Fire-Id` 不是凭据。

---

## 8. 决策 D7：REST 护栏（S1-2、S1-5）

### 8.1 视图字段

`agent24_protocol::Schedule` 新增：`owner: Option<ScheduleOwner{module, key}>`、`user_suspended: bool`、`system_disabled_reason: Option<String>`；`action` 改为 `Option<ScheduleAction>` —— **恰在 `owner` 为 `Some` 时为 `null`**。用户行的 JSON 只多三个字段（`owner:null, user_suspended:false, system_disabled_reason:null`），`action` 的值不变。`ScheduleAction` 本身**不加变体**：REST 反序列化得到的动作永远只可能是 `AgentRun`，模块投递从结构上进不来（S1-2）。`ScheduleCreate` 不加 `deny_unknown_fields`（加了会让今天带多余字段的客户端变成 400）；它没有 owner 字段，多余的 `owner_module` 键被丢弃也到不了存储。

### 8.2 每个端点对模块行

| 端点 | 模块行 | 用户行 |
|---|---|---|
| `POST /schedules` | 不可能创建（无 owner 字段、`ScheduleAction` 只有 `AgentRun`） | 不变 |
| `GET /schedules`、`GET /schedules/{id}` | 返回，带 `owner` 等字段 | 不变（多三个字段） |
| `PATCH /schedules/{id}` | **任何字段都拒**：`409 module_owned_schedule`，hint 指向 suspend/resume/DELETE；判断在 `Scheduler::update` 里（不只在 handler），SQL 层 `WHERE schedules.owner_module IS NULL` 再兜一道 | 不变 |
| `DELETE /schedules/{id}` | 允许（级联删投递行）。**注意**：模块下次对账会重新 upsert 回来；想让它别再触发，用 suspend | 不变 |
| `POST /schedules/{id}/run_now` | `202 {"fire_id"}`（§4.7） | `202 {"run_id"}`，不变 |
| `POST /schedules/{id}/suspend`（新） | `200 Schedule`，幂等；`user_suspended=1`、`next_run_at=NULL`、revision+1，同事务把未完成投递置 `expired`（T9） | `409 not_a_module_schedule`（用户行用 `PATCH enabled`） |
| `POST /schedules/{id}/resume`（新） | `200 Schedule`，幂等；`user_suspended=0`，`enabled ∧ ¬system_disabled` 时 `next_run_at = next_fire(spec, now)`（skip-missed）、计数清零、revision+1 | `409 not_a_module_schedule` |

「对模块行不可改的字段」完整列表：`name/label`、`enabled`、`spec`、`action`、`delivery`、`owner_module`、`module_key`。用户对模块行能做的只有：suspend、resume、delete、run_now。

suspend/resume 的 SQL（scratch `SET_USER_SUSPENDED_SQL`，已执行；`WHERE … AND owner_module IS NOT NULL`，对用户行影响 0 行）：

```sql
UPDATE schedules
SET user_suspended = ?1,
    next_run_at = CASE WHEN ?1 = 0 AND enabled = 1 AND system_disabled_reason IS NULL THEN ?2 ELSE NULL END,
    consecutive_failures = CASE WHEN ?1 = 0 THEN 0 ELSE consecutive_failures END,
    revision = revision + 1
WHERE id = ?3 AND owner_module IS NOT NULL
```

### 8.3 `ScheduleError` 新变体

`ModuleOwned(id)` → 409 `module_owned_schedule`；`NotModuleOwned(id)` → 409 `not_a_module_schedule`；`QuotaExceeded(u32)`（只经 RPC 出现）。既有三个变体与映射不变（`schedules.rs:16-35`）。

### 8.4 openapi / 事件 schema

`protocol/openapi.yaml`：`Schedule` 加三个字段、`action` 可空；两个新端点；`run_now` 的 202 响应改为 `oneOf {run_id} | {fire_id}`。`protocol/events.schema.json` + `protocol/fixtures/events/schedule.delivered.json` + `export-schema.rs` 的 `FORCE_REQUIRED`；`packages/api-client` 重新生成（`pnpm gen:api` 后 `git status` 干净）。

### 8.5 self-wake 不再被模块行干扰

`self_wake.rs:171-178` 的计数条件加 `s.owner.is_none()`。否则模块把 32 个 label 起成 `self-wake` 就能让 agent 的 self-wake 工具永久 `Denied`。

---

## 9. 决策 D8：模块 hot-disable / uninstall 时 schedules 与未完成投递的去留（S1-6 裁决）

**裁决：两者都不写库 —— schedules 与未完成投递原样保留；可用性在每次投递时从活的 `Supervisors`/`Generation` 状态现读；不可用一律 `Deferred`，不计失败；未完成投递由 T8 取代与 24h TTL 收敛。**

逐个情形：

| 情形 | 内核观察到什么 | 到点时 | 投递 |
|---|---|---|---|
| hot-disable（`PATCH enabled:false` 或 `POST /stop`） | `Supervisors::disable` 把它移出 `running`（`domain.rs:787`），generation 进 Draining → Revoked | tick 照常 pre-advance 并记录 fire（行没变） | `running_slot` = None → `Deferred(NotRunning)`；在途的那次若已发出且被 revoke → §5.3 |
| os.json 禁用后重启 | 从未启动，不在 `running` | 同上 | `Deferred(NotRunning)` |
| uninstall（CLI 删目录，daemon 可能不在线） | 在线时：`POST /stop` ≈ hot-disable；下次启动：包不存在，不挂载 | 同上 | `Deferred(NotRunning)` |
| 崩溃退避 / 熔断 `GaveUp` / `PackageChanged` | 仍在 `running`，槽里是 revoked 或占位 generation | 同上 | `admit_request` → `Deferred(Stopping/NotReady)` |
| 重新 enable + 重启 / 重新安装 | 挂载 → `Running` | 同上 | 未过期的 `deferred` 行被投出（同一 `fire_id`）；模块启动对账 upsert |

**为什么不在 disable 时「原子暂停」、不在 uninstall 时「原子删除」**：

1. **uninstall 根本没有一个 daemon 能参与的原子时刻**：CLI 在 daemon 不在线时也能删包（`main.rs:529-548`），daemon 可能下次启动才发现。任何「卸载即删 schedules」的方案都只能在**启动时**按「目录里没有这个包」推断 —— 而包目录读失败、manifest 暂时损坏、用户正在替换包，都会被误读成「已卸载」，于是一次瞬时的发现失败就会**删掉用户的 `user_suspended` 与模块的全部期望状态**。不删是唯一不会因误判丢数据的选择。
2. **disable 是可逆的**：写库暂停就要在 enable 时写库恢复，两次写之间的任何失败都会留下「模块回来了、schedules 还是暂停着」或反之的状态；而「暂停」本身会与模块的 `enabled`、用户的 `user_suspended` 形成第四种不启用状态。运行时现读没有这个问题 —— **没有需要原子化的写，也就没有非原子的窗口**。
3. **S1-6 的底线（不在模块不在时累计失败）由分类保证，不由写库保证**：§5.3 的表里，模块不在 `running`、不 Running、未发出，全部是 `Deferred`，T3/T4 不碰 `attempts` 与 `consecutive_failures`。

**有界性**：模块不在时，tick 每到一个 slot 仍写一次投递行（与 pre-advance 同事务），但 T8 保证每个 schedule 至多一条未完成行，TTL 保证 24h 后过期，GC 保证终态行 24h 后删除；泵对不可用 owner 每 2s 一次探测、零写（§5.4）。上限：模块数 × 256。

**孤儿行的清理入口**：用户可以 REST `DELETE` 模块行（不会被已卸载的模块重新创建）。`GET /schedules` 里这些行 `owner.module` 指向一个 `agent24 os list` 里不存在的模块，足以识别。自动 GC 孤儿 schedules 登记为 followup（需要一个比「本次启动没发现这个包」更可靠的卸载信号）。

**同名重装**：新装的包若与旧模块同名，会继承旧的 schedules —— 与内核的身份模型一致（记忆分区同样按名字 `os:<name>` 归属），不是本设计新增的性质（§14 R3）。

---

## 10. SPEC-ME3 改写与 ErrorKind 裁决

### 10.1 本 PR 对 `docs/specs/SPEC-ME3-OUT-OF-PROCESS.md` 的改动

1. **§3 开头**：offer set 改写为「ME-3 结束时是 `{Memory, Events, Approval}`；**ME-4a（ME4-M1）加入 `Scheduler`**；`Models`（ME-4b，另立设计）与 `Policy` 仍不在」，并注明 `Scheduler` 只进进程外 grant、不进进程内 `KERNEL_GRANTS` 的理由。
2. **§3 方法表**加三行（每行以 `` | `_a24/scheduler/…` `` 开头，满足 ME4-1.1.1 的 grep 验收）：`upsert`、`delete`、`list`；表后加一段「内核 → 模块：fired 投递」说明路径、头、body、至少一次与 `fire_id` 去重、`Deferred` 不计失败。
3. **§2 注入清单**加 `X-A24-Schedule-Key` / `X-A24-Fire-Id`（只出现在内核主动发起的 fired 请求上）；**§2.1 表**加两行：「保留路径 `/api/v1/<ns>/_a24/`」与「内核主动请求」。
4. **§8 交付表**加 **ME-4a** 行；offer set 阶梯表加 `ME-4a | {Events, Approval, Memory, Scheduler}` 一行。
5. **§9**：「不做 `Models`/`Scheduler`/`Policy`」改为「不做 `Models`（ME-4b 另立）/`Policy`」，`Scheduler` 由 ME-4a 交付并指回本文。

### 10.2 ErrorKind 闭集：**不扩展**

逐个失败对应到既有 kind / 协议码：未授予 → `forbidden`；配额 → `quota_exceeded`；令牌桶 → `rate_limited`；握手未完成 / draining / revoked → `not_ready` / `draining` / `revoked`；绑定请求结束或预算耗尽 → `timeout`；key/spec/label 非法、未知字段 → `-32602`；存储错误 → `-32603`（脱敏）。**没有一种失败需要新 kind**：`delete` 不存在的 key 是 `{outcome:"absent"}` 成功，不是 `not_found`；`user_suspended` 下的 upsert 是成功（状态照写、只是不触发）。所以 `ErrorKind::ALL`、SPEC §3 的闭集句、`rpc.rs:1936-1955` 钉住的文本**都不动**（判据 C5.9 断言这一点）。

---

## 11. 判据（编号对应实现 task；每条带正对照；全部 `cargo test <过滤>` 先跑 `-- --list` 断言非空）

### C1 —— ME4-1.2.1 存储层（`cargo +1.98.0 test -p agent24-store module_schedule`）

- **C1.1** 同 owner 同 key 两次 upsert → 一行，outcome `created` 然后 `unchanged`；改 spec 再 upsert → `updated`。正对照：不同 key → 两行。变异：删唯一索引 → 「一行」断言在并发用例（C1.2）里变红。
- **C1.2** 两连接 + barrier 并发 upsert 同 key → 一行、outcome 恰为 `{created, unchanged}`（期望同则 unchanged）。变异：`BEGIN IMMEDIATE` 改 `BEGIN` → 出现两个 `created` 或唯一冲突错误。
- **C1.3** tick CAS：读到 revision r 后模块改 spec（r+1）→ `advance_and_record_fire` 返回 `Lost`，`next_run_at` 是新 spec 的值，投递表零行。正对照：无并发改动时 `Advanced` 且写入一行 `pending`。变异：去掉 `AND revision = ?` → 变红。
- **C1.4** CHECK：半空 owner/key 插入被拒；用户行 `user_suspended=1` 被拒；用户行 `system_disabled_reason` 非空被拒。正对照：完整模块行插入成功。
- **C1.5** 迁移：用在 agent24-store 测试模块新增的同形 helper（照 `agent24-memory/src/lib.rs:1097` 的 `pool_migrated_up_to`，经真实 migrator 截断到 0006）建旧库、插一行旧 AgentRun 行 → 迁移到 0007 → 原值逐列不变、`revision=0`、owner 为 NULL。
- **C1.6** 配额：第 257 个新 key → `QuotaExceeded` 且行数仍 256；已存在 key 在满额时 upsert → `updated`。
- **C1.7** 同事务：`advance_and_record_fire` 的记录步骤注入失败（测试钩子让 INSERT 报错）→ `next_run_at` 未推进。正对照：无注入时两者都落。
- **C1.8** `upsert_schedule` 对模块行 `rows_affected == 0`、`name/action` 不变；对用户行更新且 revision+1。
- **C1.9** 取代与级联：同 schedule 第二次记录 fire → 第一条 `expired('superseded')`；删 schedule → 投递行全无。
- **C1.10** 泵取行查询：owner A 10 条、B 2 条 → A 4 条、B 2 条；A 在跳过缓存里 → 只剩 B。正对照：缓存为空时 A 出现。

### C2 —— ME4-1.2.2 触发接口与 REST 护栏（`cargo +1.98.0 test -p agent24d schedules_rest_guard`）

- **C2.1** REST `POST /schedules` 带 `{"action":{"type":"module_delivery"}}` → 400；带 `owner_module` 字段 → 201 但行的 `owner` 为 null（正对照：它被当作普通用户行）。
- **C2.2** PATCH 模块行的 `action` / `spec` / `enabled` / `name` / `delivery` 各一次 → 全部 409 `module_owned_schedule`，行不变。正对照：PATCH 用户行 → 200。
- **C2.3** 用户 suspend 模块行 → 模块 upsert `enabled=true`（spec 也变）→ 仍 `user_suspended=true`、`next_run_at=null`。正对照：resume 后下一次 upsert/tick 生效，`next_run_at` 非空。
- **C2.4** suspend/resume 用户行 → 409 `not_a_module_schedule`。
- **C2.5** `run_now` 模块行 → `202 {"fire_id": "fire_…"}`，`next_run_at` 不变，投递表多一行 `fire_trigger='run_now'`。正对照：用户行 → `202 {"run_id"}` 与今天相同。
- **C2.6** 同一秒两次 `run_now` 模块行 → 同一 `fire_id`、一行。
- **C2.7** 回归：既有 `agent24-scheduler` 全部测试与 `schedules.rs` 既有 REST 测试不改断言全绿（只改测试替身的 trait 签名）。
- **C2.8** self-wake：插入 32 个 label=`self-wake` 的模块行后，self-wake 工具仍可创建。变异：去掉 `owner.is_none()` 过滤 → `Denied`。

### C3 —— ME4-1.3.2 保留路径（`cargo +1.98.0 test -p agent24-os-proto reserved_path`）

- **C3.1** 变体矩阵全部 404 且 mock 上游**零请求**：`_a24`、`_A24`、`//_a24`、`./_a24`、`x/../_a24`、`x/%2e%2e/_a24`、`x/.%2E/_a24`、`%5fa24`、`%5Fa24`、`%5F%61%32%34`、`%255fa24`、`%25255fa24`、`_a24;x=1`、`_a24%3Fq`、`_a24%23f`、`_a24`（无尾段）、`_a24/`、query 变体 `…/_a24/scheduler/fired?x=1`。
- **C3.2** 拒绝矩阵（也是 404、零请求）：`x%2F..%2F_a24`、`x%2f..%2f_a24`、`x%252F..%252F_a24`、`x%5C..%5C_a24`、原始 `\`、`_a24%`、`_a24%zz`、`%C0%AF_a24`、`%00_a24`、`../<ns>/_a24`、`%2e%2e/admin`。
- **C3.3** 正对照照常转发（上游收到**原始**路径）：`/api/v1/<ns>`、`/api/v1/<ns>/`、`/anything`、`/a24x`、`/_a24x/y`、`/_a25/…`、`/x/_a24/…`（非首段）、`/routines/%E4%BD%A0%E5%A5%BD`、`/a%20b`。
- **C3.4** 变异：把判定换成对原始路径的 `starts_with("/_a24")` → C3.1 的编码变体用例变红；去掉「每轮中间结果查 `/`」→ `%252F` 用例变红。

### C4 —— ME4-1.3.1 fired 投递器（`cargo +1.98.0 test -p agent24d scheduler_deliver`）

- **C4.1** mock 上游（真实 `Generation::serving_at` + UDS）收到 `POST /api/v1/<ns>/_a24/scheduler/fired`，头含 `x-a24-fire-id`、`x-a24-schedule-key`、`x-a24-request-id`、`x-a24-approval-token`，body 为 `{key, scheduled_for, fired_at}`。
- **C4.2** 上游 500 → 第 2、3 次尝试的 `x-a24-fire-id` 与 body **逐字节相同**；正对照：下一个 slot 的 `fire_id` 不同。
- **C4.3** 上游 500 × 3 → 行 `failed`、`attempts=3`、`consecutive_failures` **+1**（不是 +3）；正对照：第 2 次返回 200 → `delivered`、计数归零。
- **C4.4** 模块 `Starting` / `Draining` / 未安装（不在 `running`）/ `running_slot` 前（`OnceLock` 未 set）时到点 → 行 `deferred`、`attempts=0`、`consecutive_failures` 不变、上游零请求；之后模块 `Running` → `delivered` 且 `fire_id` 不变。
- **C4.5** 超时：上游不回 → 10s 后 `Failed`（计一次尝试）；同期另一个 schedule 的 tick 与投递照常进行（tick 不被阻塞）。
- **C4.6** 投递持有的 request id：上游 handler 阻塞期间让 generation 进 Draining → handler 用该 id 调 `admit_callback_bound` 通过；随机 id 不通过；投递结束后同一 id 不通过。
- **C4.7** `dispatch()` 失败：`admit_request` 之后、发送之前 revoke（测试钩子）→ `NotDispatched` → `Deferred(NeverSent)`，上游**零请求**。变异：去掉 `dispatch()` 检查 → 上游收到请求，变红。
- **C4.8** 崩溃恢复：写好投递行后、投递前「杀掉」（测试钩子让泵在取行后 panic / 直接重建 `Scheduler` 与泵）→ 重启后该行以**同一 `fire_id`** 被投递。正对照：已 `delivered` 的行重启后**不**重投。
- **C4.9** `Abandoned{dispatched:true}`（发出后 revoke）→ `Failed` 计一次尝试；连续三次 → `failed`。正对照：`Abandoned{dispatched:false}` → `Deferred`。
- **C4.10** 死模块零写：owner 不可用期间跑 N 轮泵，`schedule_deliveries` 的 `updated_at` 不变（T4 不写库）。
- **C4.11** scheduler 循环在 `mount_all` 之后启动：`server.rs` 结构测试（照 `domain.rs:2934` 读 `include_str!("server.rs")` 的先例）断言 tick spawn 的文本位置在 `mount_all(` 之后。正对照：把两段对调 → 变红。

### C5 —— ME4-1.4.1 回调 handler（`cargo +1.98.0 test -p agent24d scheduler_callback`）

- **C5.1** 未声明 `scheduler` 能力 → `forbidden`；且 `Offer.provides` 不含 `_a24/scheduler/`。正对照：声明后 `provides` 含它且调用成功。
- **C5.2** A 删 B 的 key → `{outcome:"absent"}`，B 的行还在；A `list` 看不到 B 的行。
- **C5.3** 第 257 个 key → `quota_exceeded`。
- **C5.4** 非法 cron（`nope`、6 段、`0 7 * * 1-5`、`0 7 * * */2`）、非法 key（`A`、`a/b`、129 字节）、`every.secs=59` → `-32602`。正对照：`0 7 * * MON-FRI` 成功，且从固定起点（周六 2026-09-26T12:00Z）算出的 `next_run_at` 是周一 2026-09-28T07:00Z。
- **C5.5** `_meta: {owner_module: "B"}` → 写入的仍是调用方自己的行；顶层 `owner_module` → `-32602`；spec 内未知键 → `-32602`。
- **C5.6** `list` 返回完整期望状态：含 `user_suspended` 与 `system_disabled_reason`（用 REST suspend 与强制 system-disable 构造），不含 `schedule_id`。
- **C5.7** 令牌桶：连打 65 次 → 第 65 次 `rate_limited`；模块重启（新 generation）后桶**不**回满。正对照：等回填后成功。
- **C5.8** Draining 期间：不带 `request_id` 的 upsert → `draining`；带在途 fired 投递 id 的 upsert → 成功。
- **C5.9** ErrorKind 闭集不变：`ErrorKind::ALL.len() == 17` 且 `rpc.rs` 的 SPEC 文本测试不改仍绿（本设计不扩展闭集的结构性确认）。

### C6 —— ME4-1.5.1 黑盒（`cargo +1.98.0 test -p agent24d --test me4_scheduler_blackbox`，连跑 10 次）

照 PLAN 原文，本设计只补两点：Python 模块的 fired handler 必须回 2xx 并把 `x-a24-fire-id` / `scheduled_for` 追加写入探针文件；「重启 daemon」场景额外断言重启期间（模块握手完成前）到点的 fire 在握手后以同一 `fire_id` 到达，且 `consecutive_failures == 0`。客户端伪造 fired 用 C3.1 的至少 5 个编码变体，探针文件无新增。

---

## 12. 自审

1. **PLAN 硬约束逐条对照**：S1-1 列与 CHECK、部分唯一索引（§2.1，更严）；S1-2 `ScheduleAction` 不加变体、REST 护栏（§8）；S1-3 签名（§3，枚举比平铺 Option 更严）；S1-4 `BEGIN IMMEDIATE` + revision CAS + `absent` + 完整期望状态（§2.3、§6.2）；S1-5 三种不启用各有唯一写者（§6.2）；S1-6 不可用 ≠ 失败 + 循环后移 + 去留裁决（§5.3、§4.6、§9）；S1-7 accessor + admit + dispatch + 头 + 10s（§5）；S1-8 同事务、确定性 id、续投、3 次 5s/15s、过期、run_now（§4）；S1-9 规范化 + 措辞收窄（§7）；S1-10 grant、offer、配额 256、令牌桶跨重启、`deny_unknown_fields`、`_meta`、`admit_callback_bound`、闭集（§6.4、§10.2）。没有一条被放松。
2. **§五 前几轮的坑**：R1-1（发送前 `dispatch()`）写进 `send_kernel_request` 本体并有变异判据 C4.7；R1-9（pre-advance 与投递之间崩溃）由 §4.2 同事务关闭并有 C1.7/C4.8；v1 Critical 1（只写头不构成在途）由真实 `admit_request` + C4.6 的 Draining 正负对照覆盖；v1 Critical 2（先于 mount 起循环）由 §4.6 + C4.11；`$/cancelRequest` 名字本设计未涉及；「`cargo test` 过滤零匹配」由 §11 抬头的 `--list` 约定覆盖。
3. **自己找到的、PLAN 没点名的缺口**：self-wake 计数被模块 label 污染（§8.5）；6 段 cron 让模块行每秒触发（§6.3）；cron 星期数字歧义（统筹者转达，已用真实 `next_fire` 复现）；`ScheduleSpec` 变体内不拒未知键（§6.1）；死模块的延迟行每轮写库、并占满查询窗口（§5.4 的跳过缓存 + 每 owner 窗口）；`Abandoned{dispatched:true}` 若算延迟会成毒丸循环（§5.3）；upsert 改 spec 后旧 slot 的未完成投递仍会被投（T9）；TTL 从 `scheduled_for` 起算会吞掉 skip-missed（§4.5）；`trigger` 是 SQLite 关键字（§2.1）。
4. **可能被质疑的取舍**，理由已写在正文：模块行不 CAS 失败计数器（§2.2）；run_now 不看 suspend（§4.7）；保留路径对所有模块路径拒编码斜杠（§7.3）；uninstall 不删 schedules（§9）。
5. **编译验证覆盖面**：§2.1 的迁移、§2.2/§2.3/§4.2/§4.3/§5.4/§6.2/§8.2 的全部 SQL 在 SQLite 上执行并有断言；§3.1、§4.2、§5.1、§5.2、§5.3、§6.1、§6.3、§7.2、§8.3 的 Rust 全部 `cargo check` + `clippy -D warnings` + 单测（附录 A）。**未编译的**只有「改动既有函数」类描述（例如 `mount_package` 多一个参数、`Scheduler::update` 里加一行 owner 判断）—— 它们是对现有签名的增量，不引入新类型。

---

## 13. 交给实现的接口清单（按 task）

**ME4-1.2.1（agent24-store）**
- `migrations/0007_module_schedules.sql`（§2.1 全文）。
- `repo.rs`：`upsert_schedule` 改为 §2.2 的 SQL；新增 `list_schedules_for_tick() -> Vec<ScheduleRecord{schedule, revision}>`、`upsert_module_schedule`、`delete_module_schedule`、`list_module_schedules`、`count_module_schedules`、`advance_and_record_fire`、`record_run_now_fire`、`apply_delivery_outcome`、`due_deliveries`、`expire_deliveries`、`gc_deliveries`、`set_user_suspended`（suspend 时同事务 `EXPIRE_OUTSTANDING_SQL`）、AgentRun 用的 `update_schedule_runtime_cas`；`row_to_schedule` 读新列并按 `owner_module` 决定 `action`。
- 类型：`ModuleScheduleDesired`、`ModuleScheduleState`、`UpsertOutcome`、`Advance`、`NewFire`、`StoreError::QuotaExceeded`。
- 测试 helper：同形 `pool_migrated_up_to`。

**ME4-1.2.2（agent24-protocol / agent24-scheduler / agent24d REST / agent24-agent）**
- protocol：`Schedule.{owner, user_suspended, system_disabled_reason}`、`action: Option<ScheduleAction>`、`ScheduleOwner`、`EventBody::ScheduleDelivered(ScheduleDeliveredPayload)`（`schedule.delivered`）；openapi / events.schema / fixtures / api-client 同步。
- scheduler：`FireId`、`ModuleScheduleKey`、`FireTrigger`、`InvocationTarget`、`ScheduleInvocation`、`DeferReason`、`FireOutcome`、新 `RunTrigger`、`RunNowOutcome`；`fire()` 改为 CAS 版本；模块行的 tick 分支（记录 fire + `Notify`）；`update()` 的 owner 检查；`suspend/resume`；`ScheduleError::{ModuleOwned, NotModuleOwned, QuotaExceeded}`。
- agent24d：`RunManagerTrigger` → `KernelTrigger`（模块臂先返回 `Deferred(MountPending)`，1.3.1 接上真实投递器）；`schedules.rs` 两个新路由与 409 映射、`run_now` 双响应。
- agent24-agent：`self_wake.rs` 计数加 `owner.is_none()`。

**ME4-1.3.1（agent24-os-proto / agent24-scheduler / agent24d）**
- os-proto：`kernel_call.rs`（§5.2 全部公开项）；`proxy.rs` 的 `exchange`/`Upstream`/`mint_approval_token` 改 `pub(crate)`；`SCHEDULE_KEY_HEADER`/`FIRE_ID_HEADER`。
- scheduler：`deliveries.rs`（`DeliveryPump`、`apply_outcome`、§4.4/§4.5/§5.4 的常量）。
- agent24d：`Supervisors::running_slot`；`scheduler_deliver.rs`（`ModuleDeliverer`、`classify`、`FiredBody`）；`server.rs` 把 tick spawn 挪到 `mount_all` 之后、set `OnceLock`、spawn 泵。

**ME4-1.3.2（agent24-os-proto）**
- `proxy.rs`：`judge(namespace, raw_path) -> PathVerdict`（§7.2，参考实现即 scratch `reserved_path.rs`），在 `proxy()` 最前面调用；404 统一 body。

**ME4-1.4.1（agent24d）**
- `scheduler_callback.rs`：三个 handler、§6.1 参数类型、`validate_key/validate_label/validate_module_spec/validate_dow`、`SCHEDULER_RATE_*`、`MODULE_SCHEDULE_QUOTA`。
- `domain.rs`：`KERNEL_OOP_GRANTS += Scheduler`（及注释）、`mount_all/mount_package` 的 `scheduler` 参数、`provides.push("_a24/scheduler/")`、闭包外建令牌桶、恒注册三个方法。
- `docs/agent/me3-status.sh` 的 `4a 调度回调` 探针在 1.5.1 落。

**ME4-1.5.1**：`tests/me4_scheduler_blackbox.rs`（§11 C6）。

**followups 新登记**：`FU-ME4-DOW`（REST/self-wake 路径的星期字段歧义，需用户裁决方向）；`FU-ME4-ORPHAN`（已卸载模块的 schedules 自动清理，需要可靠的卸载信号）；`ME4-CODEX-DEBT` 追加本设计。

---

## 14. 已接受的残余风险（不阻塞冻结）

- **R1 同 UID 直连伪造 fired**：§7.4。模块若需要更强的来源证明，要等 SPEC §0 之外的隔离立项。
- **R2 非 ASCII 形似的保留段**：`＿ａ２４` 等不做 Unicode 规范化判断；假设模块框架不会把全角字符折叠成 ASCII 再路由。
- **R3 同名重装继承 schedules**：与记忆分区的身份模型一致（§9）。
- **R4 hot-disable 撞上超过宽限的 fired 处理**：记一次失败尝试（§5.3）；需要连续三次才 `failed`，五个 `failed` 才禁用。
- **R5 至少一次的重复**：崩溃 / 响应丢失 / 超时后重投同一 `fire_id`；模块必须按 `fire_id` 去重，且 fired handler 应当快速 2xx（10s 上限）、把长工作放到后台。
- **R6 `Scheduler::update` 两个并发 PATCH 丢失更新**：既有，与本设计正交；tick 那一侧的覆盖已由 revision CAS 关掉。
- **R7 降级**：回滚到不认识 0007 的旧二进制，严格的 `list_schedules`（REST `GET /schedules`）会因模块行的哨兵 action 解析失败而 500；宽松的 tick 列表会跳过它们（不会误触发）。本仓不支持降级，如实记录。
- **R8 路径拒绝面扩大**：所有模块路径里的 `%2F`/非法 `%`/编码控制字符从此 404（§7.3）。

---

## 附录 A：scratch crate 与编译验证

位置：`/private/tmp/claude-502/-Users-jason-Dev-auraai-Agent24/977deb42-1aba-448f-95e7-5bae2dee6fd4/scratchpad/me4s1-check/`（会话临时目录；path 依赖本 worktree 的 `agent24-protocol`、`agent24-scheduler`、`agent24-os-proto`、`agent24-domain`，`Cargo.lock` 复制自 `rust/Cargo.lock`，lints 与 workspace 相同：`unwrap_used`/`expect_used = deny`）。

| 模块 | 对应章节 | 内容 |
|---|---|---|
| `fire.rs` | §4.2 | `FireId::derive` + 测试（同 slot 同 id、次秒折叠、下一 slot / 别的 schedule 不同） |
| `invocation.rs` | §3 | 全部触发接口类型、`RunTrigger`、`AgentRunAdapter`、`agent_run_result` |
| `delivery.rs` | §4.3–4.5、§5.4 | 常量、`apply_outcome` 迁移表 + 测试（三次失败只计一次、延迟不写不计、终态不动、重试总时长 < 60s） |
| `store_sql.rs` | §2、§4、§6.2、§8.2 | 迁移 0007 与全部 SQL，在 WAL 文件库上执行：迁移保旧行 + CHECK、outcome、双连接并发一行、配额在事务内、tick CAS 输给新 spec、记录/取代/级联、3×5 失败 → system-disable → upsert 清除、suspend 不被 upsert 清除 + resume 正对照、`upsert_schedule` 改不到模块行、取行查询公平性与跳过缓存、改 spec 退役旧投递 |
| `params.rs` | §6.1、§6.3 | 参数类型、各层 `deny_unknown_fields`、key 规则、5 段 cron、星期字段只收名字（`0 7 * * 1-5` 被拒；`MON-FRI` 从周六起第一次落在周一；负对照：数字 `1-5` 在引擎里落在周日） |
| `reserved_path.rs` | §7 | `judge` 参考实现 + C3.1–C3.3 的矩阵 |
| `kernel_call.rs` | §5.2–5.3 | `send_kernel_request` 完整流程（真实 `Generation::admit_request` / `InFlight::dispatch/revoked/finish/upstream`，仅物理发送为桩）、`classify`、`FiredBody` |
| `accessor.rs` | §5.1 | `Supervisors::running_slot`（字段一致的镜像结构） |
| `rest.rs` | §8 | `ScheduleError` 新变体、视图字段、`run_now` 响应体 |

命令与结果（2026-09-23，本机 rustup 工具链 1.98.0）：

```bash
cd /private/tmp/claude-502/-Users-jason-Dev-auraai-Agent24/977deb42-1aba-448f-95e7-5bae2dee6fd4/scratchpad/me4s1-check
rustup run 1.98.0 cargo check --all-targets                  # Finished `dev` profile
rustup run 1.98.0 cargo clippy --all-targets -- -D warnings  # Finished `dev` profile（零警告）
rustup run 1.98.0 cargo test                                 # test result: ok. 22 passed; 0 failed
```

（`cargo +1.98.0` 在本机 Homebrew cargo 上不可用，等价写法是 `rustup run 1.98.0 cargo`。）
