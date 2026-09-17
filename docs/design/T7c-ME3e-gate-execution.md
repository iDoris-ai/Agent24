# T7c / ME-3e（拆分 3/3）—— `gate` 的第一个内核可执行动作：`schedule_callback`

## 与 T7a/T7b 的关系

T7a（已合并 PR #199）交付了能力授予接线 + 事件回调。T7b（PR #201，实现完成待评审）交付了 `gate`/`advise`/`status` 三个方法的协议骨架——`gate` 本轮闭集为空，任何 `action` 都直接 `forbidden`。T7c 要往这个闭集里加**第一个**变体，直接回应 SPEC ME-3e 验收栏里被 T7a/T7b 明确 deferred 的两条：「批准/拒绝的执行路径符合 §6 第 3 条」「批准 A、执行 B 不可能」。

## v1 → v2：不碰 `agent24-scheduler`，直接扩展 T7b 已经交付的周期扫描——这不是退而求其次，是发现了更简单的正确设计

**v1 的方案**（`gate` 批准后调用 `Scheduler::create` 建一条 `ScheduleAction::ModuleCallback` 的 schedule，到点由 `RunTrigger` 触发）送 Codex 第 1 轮评审，结果 **0 Critical**——不对，**4 Critical / 5 High / 3 Medium**，核心问题是：

- `module_approvals` 和 `schedules` 之间没有持久关联，两次分开的写入（建审批记录、建/enable schedule）中间崩溃会留孤儿状态。
- 「批准 A、执行 B 不可能」没有真正发生：到点触发时没有任何一步重新比对 payload_digest，而且 `Schedule.spec`/`action` 本身是可以被 PATCH 改掉的（`agent24-scheduler` 的既有 REST 就允许），"批准时刻不可变"这个前提根本不成立。
- `Scheduler::fire()` 的 pre-advance 设计（触发前先把 `next_run_at` 清空，防止崩溃后重复触发）对**一次性**动作是致命的：daemon 在"清空 next_run_at"和"标记这条审批已执行"之间崩溃，这个回调永远不会再触发——`agent24-scheduler` 的"连续失败自动 disable"救不了一次性动作，因为它根本不会进入失败计数，只是安静地消失。
- `ScheduleAction` 是一个所有 schedule 共用的公开枚举，加一个 `ModuleCallback` 变体之后，操作员的 REST（`POST/PATCH /schedules`、`run_now`）天然也能构造/触发它，没有天然的边界挡住误用。

**这一版发现：T7c 根本不需要 `agent24-scheduler` 这套引擎**。SPEC §6 表格把"注册 cron/schedule"列成一个例子，但 T7c 要做的最小版本——"模块说一个未来的时间点，到了那个点告诉它一声"——**T7b 已经交付的周期扫描（决策记录 5，判定审批提交是否超时用的那个）本身就是一个"到点做点什么"的机制**，只是目前只用来判定 `Pending → TimedOut`。把它推广成"到点也可以把 `Approved` 的 `schedule_callback` 判定成已执行"，是往一个已经写好、测过、扛过一轮设计评审+一轮代码评审的机制上加一条查询，不是接一整套面向 agent run 设计、有自己的 REST 面、有自己的 PATCH 语义的独立引擎。

这个简化让 v1 的 4 个 Critical 全部**不复存在**，不是被修复：

- 没有单独的 `schedule` 行，就没有"两张表之间的关联"这个问题——一条 `ModuleApproval` 本身既是审批记录，也是"到点提醒"这件事的唯一状态源。
- 没有可以被 PATCH 的独立 `spec`/`action`，"批准 A 执行 B"在结构上不可能发生——**能被执行的只有这一行本身在提交时刻写死的 `action`/`target`/`payload`**，没有第二个可以偏离的副本。
- 没有 pre-advance 这道工序——判定"到点了"和"标记已执行"是同一次 `UPDATE ... WHERE executed_at IS NULL` CAS，跟决定 CAS、超时 CAS 用的是同一套已经验证过的模式（design doc T7b 决策记录 3/5），单次崩溃最坏情况是"这一轮扫描没扫到，下一轮扫到"，不存在"永久错过"这个结局。
- 没有碰任何公开的 `Schedule`/`ScheduleAction` 类型，操作员的 schedule REST 面完全不受影响，不存在"被公开 API 意外构造/触发"的问题。

## 决策：`gate` 的 `schedule_callback` 动作——一次性、RFC3339 时间戳，不做 cron/every

**范围明确收窄到一次性**（Codex 第 1 轮 Critical 3 建议，采纳）：`target` 字段是一个 RFC3339 时间戳字符串（不是 cron/every 规格）。「模块想要一个会重复触发的回调」不在本轮范围内——SPEC 表格给的是"cron/schedule"这个大类的例子，不代表第一个交付必须支持全部形态。

```rust
// agent24-protocol/src/types.rs（在 T7b 已经交付的 ModuleApproval 上加字段，不新建表）
pub struct ModuleApproval {
    // …T7b 已有的字段（id/module/request_id/kind/binding/action/target/
    // payload/payload_digest/decision/created_at/decided_at/expires_at）不变…
    pub executed_at: Option<String>, // 新增。None = 还没到点（或者是 advise/未批准的 gate，
                                      // 这两种情况永远是 None）；有值 = 到点那一刻的时间戳
}
```

`ApprovalAnswer`（`submit`/`status` 的响应类型，T7b 已交付，目前只有 `approval_id`/`kind`/`binding`/`decision` 四个字段）同样加 `executed_at: Option<String>`——按仓库既有惯例，wire 上始终携带这个字段，未执行时是 `null`，不是"看情况才出现"。**这个字段目前是由一个手写的 `to_answer` 函数逐个搬字段（`agent24d/src/module_approval_broker.rs:81`），不是结构体自动映射**（Codex 第 2 轮 M4 订正 v2 的错误说法）——加 `executed_at` 必须同时改这个函数，否则编译能过但字段永远是 `None`。`export-schema.rs::FORCE_REQUIRED`/`DATE_TIME_FIELDS` 也要同步，另外 `ApprovalAnswer` 目前不在事件 schema exporter 覆盖的类型范围内（那个只管 WS `EventBody`），REST 的 OpenAPI 类型和生成的 TS 客户端要单独更新，不能假设跟 WS schema 一起自动完成。

**时间规范化——不是"校验一下就行"，必须落到一个具体的、跟扫描用的时钟同格式的字符串**（Codex 第 2 轮 High 1，v2 的核心遗漏）：`target` 允许调用方传任意合法 RFC3339（带任意时区偏移、任意小数位数），但**持久化之前必须重新格式化成跟 `agent24d/src/module_approval_broker.rs::now_iso` 完全一样的规范形式**——`YYYY-MM-DDTHH:MM:SSZ`（UTC、整秒、`Z` 后缀，没有小数部分；`now_iso` 本身就是这个格式，复用它调用的同一个 `agent24_core::util::iso8601_from_epoch_secs`）。**次秒精度直接截断（向下取整到秒），不做四舍五入**——这是一个需要如实写下来的行为，不是实现阶段可以随便选的细节：模块传 `12:00:00.9Z`，规范化成 `12:00:00Z`，跟扫描用同一个函数生成的"现在"字符串按字典序比较，结果才等价于按真实时间比较。不接受 leap second（`:60`）——解析失败按"target 不合法"处理，跟其它解析失败一样。

```rust
/// 校验 + 规范化 target，返回要持久化的字符串（不是裸 `Result<()>`）。
/// wire handler 和进程内 `ApprovalRequester` 必须调用同一个函数，不能一个
/// 校验一个不校验——否则两条路径存进去的格式可能不一致，扫描的字符串比较
/// 就不再等价于时间比较（Codex 第 2 轮 High 1 指出的风险）。
fn canonicalize_schedule_target(target: &str) -> Result<String, TargetError> { ... }
```

**闭集匹配——校验函数要同时看 action 和 target，返回值要能被两条路径复用**（Codex 第 2 轮 High 2 订正）：T7b 交付的 `check_closed_set(_action: &str) -> Result<(), ApprovalRequestError>`（`module_approval_broker.rs:37`）签名里 `action` 参数带下划线前缀、整个函数体从不读它——因为 T7b 的闭集本来就是空的，参数存在只是为了不改调用方签名。T7c 要把签名改成：

```rust
pub fn validate_gate_action(action: &str, target: Option<&str>) -> Result<CanonicalGateAction, ApprovalRequestError> { ... }

pub struct CanonicalGateAction {
    pub action: String,          // 原样透传（本轮只有一个合法值，"schedule_callback"）
    pub target: String,          // canonicalize_schedule_target 的结果——不可省略
}
```

`action == "schedule_callback"` 时：`target` 必须存在且能被 `canonicalize_schedule_target` 规范化，否则返回一个新的 `ApprovalRequestError::InvalidTarget` 变体（区别于"不在闭集内"——这是"在闭集内、但参数不对"，wire 上映射成 `-32602`，不是 `forbidden`，跟 SPEC §6.1 的"闭集外→forbidden"是两回事）；其它任何 `action` 字符串仍然是"不在闭集内"，不做 trim、不做大小写不敏感匹配。**wire handler 和进程内 `PolicyApprovalBackend`（`agent24d/src/domain.rs:168` 一带的调用点）必须调用同一个 `validate_gate_action`**，成功后把 `CanonicalGateAction.target` 而不是调用方传入的原始字符串存进 `ModuleApproval`——这一步顺序仍然在决策记录 2 的令牌核验之前（跟 T7b 的既有顺序一致，不消耗令牌）。

## 决策：批准即生效，执行只是"到点打勾"，不存在"注册"和"执行"两个分开的内核动作

`gate` 批准（`decide_module_approval` 走 T7b 已有的决定 CAS，`decision: Pending → Approved`）这一步本身**就是**"内核同意在 `target` 时刻做这件事"——不需要额外一步"把批准落实成一个可执行对象"（v1 里"建 schedule"这一步不再存在）。到点之后要做的唯一一件事，是把这一行的 `executed_at` 填上，仅此而已——这就是"执行"的全部内容。

**`binding` 必须真正跟着 `kind` 走，不能延用 T7b 的硬编码**（Codex 第 2 轮 High 3）：T7b 的 `insert()`（`module_approval_broker.rs:164`）目前把 `binding` 写死成字面量 `false`，注释原话解释是"因为 `Gate` 永远在到达这一行之前先命中空闭集返回，这一行运行时 `kind` 只可能是 `Advise`"——这个前提在 T7c 之后不再成立（`schedule_callback` 是第一个真的会走到 `insert()` 的 Gate 动作）。改成 `binding: kind == ModuleApprovalKind::Gate`（`Advise` 恒 `false`，`Gate` 恒 `true`，不再是写死的字面量）。

**扩展 T7b 的周期扫描**（`module_approval_broker.rs::scan_loop`/`scan_once`，判据 14/16 已覆盖它的容错性）：新增一条查询，**跟既有的超时判定各自独立 `match`，互不影响、不共享事务**（Codex 第 2 轮 Medium 2）——`scan_once` 里两条 `UPDATE` 分别处理各自的 `Result`，任何一条失败都只记日志、不 `?`、不提前 `return`，不影响另一条继续执行：

```sql
-- 复用现有的部分索引（module_approval_broker 已有 pending+expires_at 索引）
UPDATE module_approvals
SET executed_at = ?
WHERE kind = 'gate' AND decision = 'approved' AND executed_at IS NULL
  AND action = 'schedule_callback' AND target <= ?
```

`target` 存的是 `canonicalize_schedule_target` 规范化之后的字符串（上一节已经保证格式跟 `now_iso` 完全一致），按字符串比较等价于按时间比较，不需要在 SQL 里做任何日期函数转换。

**新增一条部分索引**（Codex 第 2 轮 Medium 3，避免这条新查询随审批记录增多退化成全表扫描）：

```sql
CREATE INDEX idx_module_approvals_pending_schedule
ON module_approvals (target)
WHERE kind = 'gate' AND decision = 'approved' AND action = 'schedule_callback' AND executed_at IS NULL;
```

**这一步不需要 `RETURNING`/逐条推事件**（跟 T7b 的超时扫描不同）：T7c 本轮不新增 WS 事件（模块本来就该用 `status` 轮询，`executed_at` 从 `null` 变成有值这件事本身不需要单独广播——如果未来真的需要推送，那是另一个决定，本轮不做）。

**新 migration，不改已发布的 0005**（Codex 第 2 轮 Medium 1）：`executed_at` 是数据库列，`rust/crates/agent24-store/migrations/0005_module_approvals.sql`（T7b 已交付、假设已经在某些环境跑过）不能被追加改动——新开一个 `0006_module_approval_executed_at.sql`：`ALTER TABLE module_approvals ADD COLUMN executed_at TEXT;` + 上面那条部分索引。`row_to_module_approval`（读出一行时的映射函数）、INSERT 语句、以及 T7b 现有测试里"手工重放 migration 建表"的地方（`module_approval_broker.rs:525` 一带）都要跟着改成重放到 `0006`。

## 决策：`status`/进程内 `ApprovalRequester` 原样返回这个新字段，不新增方法

T7b 的 `status`（纯读、按 `module + approval_id` 隔离、幂等）不需要任何逻辑改动——`executed_at` 只是 `ModuleApproval` 多出来的一个字段，`status` 已经会把整行数据映射进 `ApprovalAnswer`，只需要把这个新字段也带上。进程内 `ApprovalRequester::submit`/`status`（T7b 已交付）同样不需要改签名，`ApprovalAnswer` 多一个字段，两条路径自动一致。

## 判据

1. `gate` 提交 `action: "schedule_callback"`、合法 RFC3339 `target` → 建 `ModuleApproval{decision: Pending, executed_at: None}`。**正对照**：`target` 不合法 → `-32602`，不建记录，不消耗令牌。
2. 人工批准 → `decision: Approved`；`status` 在到点之前持续返回 `executed_at: null`。
3. 到点后下一次周期扫描 → `executed_at` 被填上；`status` 能查到。**正对照**：还没到点的记录不受影响。
4. 拒绝的审批 → `decision: Denied`，永远不会被扫描判定为已执行（扫描 SQL 的 `decision = 'approved'` 条件天然排除）。
5. 已经超时的审批（`decision: TimedOut`，T7b 已有机制）同样永远不会被判定为已执行。
6. 「批准 A、执行 B 不可能」：这一行的 `action`/`target`/`payload`/`payload_digest` 从提交那一刻起没有任何 API 能修改它们（T7b 没有为 `ModuleApproval`设计任何 PATCH——决定 CAS 只碰 `decision`/`decided_at`，本轮的扫描 CAS 只碰 `executed_at`）——用一条测试确认：不存在任何调用路径能在批准之后改变这一行的 `action`/`target`/`payload`。
7. 闭集只有这一个变体：任何其它 `action` 字符串仍然 `forbidden`，且不区分大小写/trim（`"Schedule_Callback"`、`" schedule_callback "` 都仍然 `forbidden`）。
8. 并发：同一个 `approval_id` 被扫描到两次（模拟并发跑两次扫描）→ `executed_at` 只被设置一次（CAS 的 `WHERE executed_at IS NULL` 保证），不会出现"值被覆盖成更晚的时间戳"这种情况。
9. `target` 已经过去（模块提交时给了一个过去的时间戳）→ 提交本身仍然成功（不因为"已经过期"就拒绝提交），批准后下一次扫描立刻判定已执行——这是刻意的行为，不是 bug，跟"活人来不及在 30 秒内批准"是同一个精神：不因为时间点已过就假装这件事没发生过。
10. `advise`/未批准的 `gate` 记录，`executed_at` 永远是 `None`——只有 `kind: Gate` 且 `decision: Approved` 且命中闭集的记录才会被这条扫描碰到。
11. `binding`：`gate` 提交（走到 `insert()` 这一步，也就是命中闭集之后）返回的记录 `binding == true`；`advise` 提交 `binding == false`（Codex 第 2 轮 High 3）。
12. `target` 为 `None`（`gate`/`action: "schedule_callback"` 但没带 `target`）→ `-32602`，`ApprovalRequestError::InvalidTarget`，不建记录。
13. 时间规范化边界：`Z`、`+00:00`、`+08:00`、带 0/3/6/9 位小数的同一个真实时刻，规范化之后必须是完全相同的存储字符串；`target == 当前 now_iso()`（精确相等，不多不少）必须被判定为"已到"（`<=` 不是 `<`）。
14. 扫描的两条查询互不阻断：让"超时判定"那条 SQL 故意失败，"到点执行"那条 SQL 仍然正常跑完并生效，反过来也一样（Codex 第 2 轮 Medium 2）。
15. daemon 重启后，一条 `target` 早已过去、但因为重启前从未被扫描到的 `Approved` 记录，在重启后的第一次扫描里就被判定执行——不需要专门的"启动清扫"，普通那条周期查询本身就覆盖这个场景（因为它是无状态的全表条件查询，不依赖任何内存态）。
16. wire 提交和进程内 `ApprovalRequester::submit` 提交同一个不合法/需要规范化的 `target`，两条路径的校验结果和最终存储的字符串完全一致（证明 `validate_gate_action`/`canonicalize_schedule_target` 确实被两条路径共用，不是各自实现了一份）。
17. 升级场景：一条 T7b 时代（`0005` migration 之后、`0006` migration 之前）就存在的 `advise` 记录，跑完 `0006` migration 后 `executed_at` 是 `NULL`，不受影响。
18. REST `get_module_approval`、WS（如果记录本身推送过 `module-approval.required`，其 payload 结构不含 `executed_at`——这个字段只在事后查询时出现，不在提交时的事件里）、wire `status`、进程内 `status` 四个读取路径返回的 `executed_at` 值互相一致。
19. `protocol/openapi.yaml`、`packages/api-client` 生成类型、`export-schema.rs` 的 `FORCE_REQUIRED`/`DATE_TIME_FIELDS` 都同步了新字段——CI 的 codegen 漂移检查（`pnpm gen:api` 后 `git status` 干净）通过。
20. `agent24-scheduler`/`ScheduleAction` 完全没有被改动——现有的 schedule REST（`POST/PATCH /schedules`、`run_now`）无法构造或触发 `schedule_callback` 这个概念，两套机制在代码层面没有交集。

## 不改的东西

- `agent24-scheduler`/`ScheduleAction`/`Schedule` 的公开 REST 面——整个不碰，T7c 不依赖这套引擎。
- T7b 的 `module_approvals` 表结构、决定 CAS、超时扫描的既有查询——只加一个字段、加一条新查询，不改现有的。
- `status`/`ApprovalRequester` 的方法签名——不变，只是响应里多一个字段。
- 任何形式的 cron/every/重复触发——留给更远的未来任务，本轮明确只做一次性。
- WS 推送——本轮不为"到点执行"这件事新增事件，`status` 轮询已经够用。

---

## v2 → v3 改动（Codex 第 2 轮：0 Critical / 3 High / 4 Medium，全部采纳）

第 2 轮确认 v1 的 4 个 Critical 确实随架构变更消失，没有等价替代问题出现——本轮修的都是"这个新架构下的具体细节"，不是又一次推翻方向：

- High 1：时间规范化不再是"校验一下就行"，新增 `canonicalize_schedule_target`，规定精确的目标格式（跟 `now_iso` 完全一致：UTC、整秒、`Z` 后缀）、次秒精度截断（不四舍五入）、拒绝 leap second——这是保证扫描 SQL 的字符串比较真的等价于时间比较的前提，之前只说"统一 UTC 和精度"但没有落到一个具体函数和具体格式。
- High 2：`check_closed_set` 升级成 `validate_gate_action(action, target)`，进程内、进程外共用同一个函数（新增 `ApprovalRequestError::InvalidTarget`），杜绝"两条路径校验不一致导致存进去的格式不一样"这个风险。
- High 3：`binding` 从 T7b 遗留的硬编码 `false` 改成 `kind == Gate` 的真实推导——不然真的 Gate 记录也会返回 `binding: false`，直接违反 gate/advise 的类型区分契约。
- Medium 1：新增独立 migration（`0006_module_approval_executed_at.sql`），不追加改动已发布的 `0005`。
- Medium 2：扫描的两条查询显式独立 `match`、互不阻断，写进设计文本，不再只是"应该这样做"的默认假设。
- Medium 3：新增部分索引，避免这条新查询在记录变多后退化成全表扫描。
- Medium 4：判据从 10 条扩到 20 条，补齐 binding、target 为空、时间边界、扫描互不阻断、daemon 重启、跨路径一致性、升级场景、codegen 同步、schedule REST 隔离——原来"status 自动映射新字段"的错误说法也一并订正（`to_answer` 是手写的，必须显式改）。

---

**本轮（v3）不再送 Codex 第 3 轮评审**：第 2 轮已经确认架构方向成立、v1 的全部 Critical 消失，剩下的 3 个 High 和 4 个 Medium 全部是"具体怎么实现"层面的问题，这一版已经逐条给出了具体函数签名、具体 SQL、具体 migration 文件名——已经不是需要再一轮对抗性评审才能收敛的架构不确定性，交给实现阶段的编译器和判据对应的测试去验证。等 T7b（PR #201）先合并，再基于它的最终代码实现 T7c。

（v3，设计冻结，等 T7b 合并后进入实现）
