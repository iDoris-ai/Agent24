# ADR-044：后台作业与监督回路（草案）

- 状态：**提议**（2026-11-01，Fable 初稿，待 Codex 挑战与 jason 拍板）
- 上位：[ADR-039](../../../adr/ADR-039-concept-model.md) §2.1（本体负责调度）、规则 3（系统不得静默修改技能）；ADR-026 硬约束 2（fail-closed）
- 相关：ADR-K1-01 §2.4（`pending` 作业句柄契约）、ADR-K1-02 §2.2（后台传播继承来源）、`agent24-agent/src/self_wake.rs`（H5）、`agent24-policy/src/guardian.rs`（M-D/D3）、CORE-5 5f（`/health` 契约）、`docs/design/SHUT-shutdown-observability.md`
- 依据：[研究初稿](../SUPERAGENT-EVOLUTION-2026-11-01-fable.md) §2.3、§2.10、§3.1 第 4、5 条；文章 S1「上下文、后台执行与 Watchdog 如何落地」；原型 `docs/003-execution.md` Background work；原文 §4.1 全双工、§6 Watchdog（[SOURCES](../SOURCES.md) S1、S2、S14）

## 1. 背景

**后台作业。** `shell_exec` 是同步工具：run 在工具返回前不处理任何输入。K1-01 §2.4 为模块工具定义了类型化 `pending` 作业句柄（只对声明异步语义的操作），未见实现。`self_wake` 证明了「完成后作为新事件回到会话」可行，它用一次性 schedule 做到（`self_wake.rs` 头注释）。原文 §4.1：把耗时命令改成 `start()` / `stop()`，模型立即收到「已启动」，后台完成后结果作为新事件投递；原型 003：前台 shell 5 秒后转后台作业，每轮后台工作最多 30 分钟，超时杀作业并报告，作业完成唤醒 Executor。文章强调：「已启动」只说明接单，不代表成功；最终状态要重新进入事件流。

**监督。** 现有的监督都是进程级或模块级：桌面每 5 秒探 `/health`，连续 3 次失败杀掉重启（ADR-037 事实 §1.1）；scheduler 连续失败自动禁用；os-proto 监管熔断模块；`/health` 恒 ok（CORE-5 V4，5f 要改）。没有东西看「工作有没有实质进展」。原文 §6 的 Watchdog 是主系统外的独立 Agent Session，能在线改提示词和触发规则；文章 S1 建议先监控确定性指标（队列最老等待、重试次数、最近可验证产物、预算、阶段），需要模型判断偏航时再让独立审查器读这些证据，并对重启、停工、改策略设权限与次数上限。Agent24 已有同形态的零件：Guardian 用 LocalOnly 小模型给工具调用评风险，只认明确的 `low`（`guardian.rs`）。

## 2. 决策

### D1 后台作业：真实 id、取消、完成事件

- 新增本体内部资源 **后台作业**（`jobs` 表）：`id`、`run_id`、`work_item_id`（可空）、`kind: shell | module_pending`、`command_summary`（脱敏）、`started_at`、`deadline_at`、`status: running | completed | killed | failed`、`exit`、`output_ref`（输出存文件，表里只存引用与大小）。
- `shell_exec` 增加 `background_after_ms`（默认 5 000）：超过即返回 `{ started: true, job_id }`，工具调用算完成；输出先截断进上下文，全文按 `job_id` 分页读。
- 新工具 `job_wait(job_id, max_ms ≤ 5000)`、`job_output(job_id, page)`、`job_kill(job_id)`。`job_kill` 是 Write 类，走审批。
- 作业完成、被杀或失败时写出站事件 `job.completed | job.killed | job.failed`，并通过 ADR-043 的 `deliver_input` 动作唤醒原 run（run 已结束时唤醒工单的下一个 run）。
- 每轮后台作业上限 30 分钟（可配）；到期杀作业，记录「被杀的作业与最后输出」进入工单输入。daemon 关机时按 SHUT 排空：等 10 秒，再杀。
- 模块工具的 `pending` 句柄（K1-01 §2.4）映射为 `kind: module_pending` 的作业；查询仍是独立的模块工具调用，不自动重试。
- 后台作业继承父 run 的来源标签（K1-02 §2.2），输出进模型前按 K1-02 §2.1.1 第 6 条打标签（`shell_exec` 结果 `Unknown`）。

### D2 监督回路第一层：确定性指标进 `/health`

- `/health` 增加 `work` 条目（随 CORE-5 5f 的 health 契约一起改 OpenAPI）：`oldest_waiting_s`（待执行动作最老等待）、`failing_actions`（连续失败 ≥3 的动作数）、`running_jobs`、`stalled_work_items`（`working` 阶段超过阈值且无新产物的工单数）、`budget`（本日模型调用与 token 用量对上限）。
- 每个指标有阈值；越界只改 `status: degraded` 并在待处理列表给出一条「系统需要你看一眼」，不自动重启、不自动取消。
- 「可验证产物」定义为：收据到 `sent`、文件写入成功、作业完成。工单的 `last_artifact_at` 由这些事件更新。

### D3 监督回路第二层：独立审查器，权限有界

- 审查器是本体内的定时任务（不是能力、不是技能），按 Guardian 形态实现：输入只有 D2 的指标与工单的结构化事实（阶段、重试次数、最近产物、最近三条输入的摘要），用 LocalOnly 模型判「是否偏航」。
- 审查器能做的动作**闭集**：`pause_work_item`、`retry_action`、`notify_user`。每个工单每日每种动作最多 3 次，超过只能 `notify_user`。
- 审查器**不能**：改技能、改提示词、改准入表、改接受模式、撤销授权依据、杀 run 以外的进程。它要改什么，只能产出提案进 ADR-040 D4 的 `proposed` 收件箱。
- 审查器的每次判断与动作记审计（K1-04 字段），模型输出不可解析时视为「无判断」，不动作。
- 默认关闭（同 Guardian 的 `A24_GUARDIAN=1`）；D2 必须先上。

### D4 不做的

- 不做主系统外的独立 Watchdog Session（原文 §6 形态）：Agent24 是单用户本地 daemon，没有第二个进程可信地拿到权限；进程级存活由服务管理器管（ADR-037 决策 2）。
- 不让审查器「在线修正主系统行为」：违反 ADR-039 规则 3。

### D5 对外

用户看到：工具调用的「后台运行中」、作业输出、「系统需要你看一眼」。作业表、审查器都是内部名词。

## 3. 备选方案及否决理由

| 备选 | 否决理由 |
|---|---|
| `shell_exec` 保持同步，只提高超时 | 原文 §4.1 与原型 003 的核心：会话被长命令阻塞就不能接收新输入；工单的「等人」阶段会被一个 `sleep` 卡死 |
| 作业完成直接回写到 run 的上下文（不经事件与动作队列） | run 可能已结束；经 ADR-043 队列才能在掉电后不丢完成通知 |
| 审查器用云模型 | 它读的是工单事实与输入摘要，可能含 `Restricted` 来源；K1-02 fail-closed |
| 审查器可以改准入表、关闭失控的主动技能 | 看似安全（只收紧），但「收紧」也是对用户配置的静默修改；原文 §6 的做法是团队场景下维护者授权的，个人分身没有这个维护者。改为只能提案 |
| 用桌面的 5 秒健康检查扩展成监督 | 桌面是外壳（ADR-037 决策 1），外壳不带能力；而且桌面退出后 daemon 还在 |

## 4. 后果

- 好处：会话全双工；长任务不再阻塞审批与输入；偏航能被发现；审查器有硬上限。
- 代价：`shell_exec` 语义变化要同步桌面 / TUI 展示与 OpenAPI；三个新工具；`/health` 契约变更进 G-API-3。
- 风险：模型可能滥用 `job_wait` 变相同步等待；原型用 `noWait` 守卫拒绝 `sleep` 与长超时，Agent24 对应做法是 `job_wait` 上限 5 秒且计数。

## 5. 与其他 ADR 的关系

- 实现 K1-01 §2.4 的 `pending` 句柄落点。
- 完成事件经 ADR-043 的 `deliver_input` 动作投递；指标来自 ADR-043 的队列与 ADR-042 的工单阶段。
- 审查器的提案走 ADR-040 D4 的收件箱。
- `/health` 字段随 CORE-5 5f 一起改，不另起契约。
- 建议 ADR-039 §2.1 本体职责加「后台作业与监督回路」。

## 6. 开放问题

- Q1 `background_after_ms` 默认 5 秒是否适合本地模型推理场景（模型本身就慢）。可能要对 `shell_exec` 与模型调用分别设。
- Q2 作业输出文件的保留期与 purge。
- Q3 `stalled_work_items` 的阈值怎么定（原型没有给数字，文章只给了指标名）。
- Q4 审查器的「偏航」评测用例从哪来：建议先收集 `observe` 期间的工单记录做离线集。
