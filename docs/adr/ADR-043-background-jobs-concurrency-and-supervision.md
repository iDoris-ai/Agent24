# ADR-043：后台作业、并发与监督回路

- 状态：**提议**（2026-11-01，三步评审终稿，待 jason 拍板）
- 上位：[ADR-039](ADR-039-concept-model.md) §2.1（本体负责调度）、§3 规则 3（系统不得静默修改技能）；ADR-026 硬约束 2（fail-closed）
- 相关：ADR-K1-01 §2.4（`pending` 作业句柄契约）；ADR-K1-02 §2.2（后台传播继承来源）；`agent24-agent/src/self_wake.rs`；`agent24-policy/src/guardian.rs`；CORE-5 5f（`/health` 契约）；`docs/design/SHUT-shutdown-observability.md`；[ADR-040](ADR-040-skill-contract-v1.md) D5（提案收件箱）；[ADR-041](ADR-041-inbound-events-admission-and-work-items.md)；[ADR-042](ADR-042-durable-actions-and-reconciliation.md)
- 依据：[演进终稿](../research/deepseek-superagent/SUPERAGENT-EVOLUTION-2026-11-01.md) §2 裁决 12、14；文章 S1「上下文、后台执行与 Watchdog 如何落地」；原型 `docs/003-execution.md` Background work / Concurrency；原文 §4.1 全双工、§6 Watchdog；[SOURCES](../research/deepseek-superagent/SOURCES.md) S1、S2、S14

## 1. 背景

**后台作业。** `shell_exec` 是同步工具，预算 30 秒（`agent24-tools/src/local.rs:508`）：run 在工具返回前不处理任何输入。K1-01 §2.4 为模块工具定义了类型化 `pending` 作业句柄（只对声明异步语义的操作），未实现（`module_tool_runtime.rs` 里的 `pending` 是内部标志）。`self_wake` 证明了「完成后作为新事件回到会话」可行。原型 003：前台 shell 5 秒后转后台作业，每轮后台工作最多 30 分钟，超时杀作业并报告，作业完成唤醒 Executor；文章强调「已启动」只说明接单，不代表成功，最终状态要重新进入事件流。

**并发。** Agent24 没有并发槽概念；HW-TIERS 只管模型内存。原型：worker slot 限制并发，另给人的消息一个额外槽，新工作不能阻止人继续已有工单。

**监督。** 现有监督都是进程级或模块级：桌面每 5 秒探 `/health`，连续 3 次失败杀掉重启（ADR-037 事实）；scheduler 连续失败自动禁用；os-proto 监管熔断模块；`/health` 恒 ok（CORE-5 5f 要改）。没有东西看「工作有没有实质进展」。原文 §6 的 Watchdog 是主系统外的独立 Agent Session，能在线改提示词和触发规则；文章 S1 建议先监控确定性指标，需要模型判断偏航时再让独立审查器读这些证据，并对重启、停工、改策略设权限与次数上限。Guardian 已是同形态零件：LocalOnly 小模型给工具调用评风险，只认明确的 `low`，默认关。

**评测。** 现有 `eval/decide/` 三个 jsonl、`agent24-memory` 的 eval、`packages/documents-accept`，都是专项；没有覆盖「事件 → 决定 → 动作 → 交付」的统一评测。

## 2. 决策

### D1 后台作业：真实 id、取消、完成事件

- 新增本体内部资源**后台作业**（`jobs` 表）：`id`、`run_id`、`work_item_id`（可空）、`kind: shell | module_pending`、`command_summary`（脱敏）、`started_at`、`deadline_at`、`status: running | completed | killed | failed`、`exit`、`output_ref`（输出存文件，表里只存引用与大小）。
- `shell_exec` 增加 `background_after_ms`（默认 5 000）：超过即返回 `{ started: true, job_id }`，工具调用算完成；输出先截断进上下文，全文按 `job_id` 分页读。
- 新工具 `job_wait(job_id, max_ms ≤ 5000)`、`job_output(job_id, page)`、`job_kill(job_id)`。`job_kill` 是 Write 类，走审批。`job_wait` 每 run 计数，防止变相同步等待（原型用 `noWait` 守卫拒绝 `sleep`）。
- 作业完成、被杀或失败时写出站事件 `job.completed | job.killed | job.failed`，并通过 ADR-042 的 `deliver_input` 动作唤醒原 run（run 已结束时唤醒工单的下一个 run）。「已启动」不等于成功。
- 每轮后台作业上限 30 分钟（可配）；到期杀作业，「被杀的作业与最后输出」进工单输入。daemon 关机按 SHUT 排空：等 10 秒，再杀。
- 模块工具的 `pending` 句柄（K1-01 §2.4）映射为 `kind: module_pending` 的作业；查询仍是独立的模块工具调用，不自动重试。
- 后台作业继承父 run 的来源标签与策略引用（K1-02 §2.2），每次实际执行前重验撤销 / 到期；输出进模型前按 K1-02 §2.1.1 打标签（`shell_exec` 结果 `Unknown`）。

### D2 并发：有界，人优先

- v1 并发槽固定：**1 个工单 run 槽 + 1 个人直接对话的保留槽**。工单的 `run_launch` 等普通槽；用户直接发起的对话 run 可用保留槽，不被工单排队挡住。
- 每个工单有预算（本日模型调用、token、外部写入次数），超过即 `person/blocked`，不自动续。
- 同一资源（工作区路径、文档 id）同时只允许一个工单的 run 持有写权；冲突的 `run_launch` 返回 `Later`，不静默覆盖。
- 放宽并发槽需要 S6 的证据：真实完成率与人工介入时间，不是「开了多少 run」。

### D3 监督第一层：确定性指标进 `/health`

- `/health` 增加 `work` 条目（随 CORE-5 5f 的 health 契约一起改 OpenAPI，走 G-API-3）：`oldest_waiting_s`（待执行动作最老等待）、`failing_actions`（连续失败 ≥3）、`running_jobs`、`stalled_work_items`（`working` 超过阈值且无新产物）、`budget`（本日用量对上限）。
- 每个指标有阈值；越界只改 `status: degraded` 并在待处理列表给一条「系统需要你看一眼」，不自动重启、不自动取消。
- 「可验证产物」定义为：收据到 `sent`、文件写入成功、作业完成。工单的 `last_artifact_at` 由这些事件更新。模型自报进展、日志更新都不算产物。

### D4 监督第二层：独立审查器，权限有界

- 审查器是本体内的定时任务（不是能力、不是技能），Guardian 形态：输入只有 D3 的指标与工单的结构化事实（阶段、attempt 数、最近产物、最近三条输入摘要），用 LocalOnly 模型判「是否偏航」。
- 能做的动作**闭集**：`pause_work_item`、`retry_action`、`notify_user`。每个工单每日每种动作最多 3 次，超过只能 `notify_user`。
- `retry_action` 只能作用于 `status=waiting`、且上次失败是**明确错误**的动作（不是 `result_unknown`）。动作处于 `result_unknown`，或所在通道既不支持幂等键也不做对账时，审查器只能 `notify_user`，由人决定。这与 ADR-041 D8、ADR-042 D2/D3「结果未知不自动重试」一致，审查器不是绕过这条规则的第二条路径。
- **不能**：改技能、改提示词、改准入表、改接受模式、撤回或吊销授权依据、解除人工暂停、激活技能候选、杀 run 以外的进程。它要改什么，只能产出提案进 ADR-040 D5 的 `proposed` 收件箱。
- 每次判断与动作记审计（K1-04 字段）；模型输出不可解析视为「无判断」，不动作。
- 默认关闭（同 Guardian）；D3 必须先上。审查器的误报率是 S6 的度量项。

### D5 评测：分层，覆盖准入到验收

- 评测分六层，各有固定用例与基线：准入（误接单 / 漏接单）、执行（完成率、attempt 数）、安全（越范围动作、审批拒绝数）、恢复（掉电 / 重启 / 响应丢失注入后的状态一致）、交付（收据 `sent` 率、验收通过率）、人类介入成本（确认与纠错次数、时间）。
- 离线场景集是每阶段的交付物（终稿 §7）；合成消息与替身工具的结果只能证明调度与恢复行为，不写成「真实能力提升」。
- 候选技能、审查器都不得修改评测用例或评分器（ADR-040 D5）。
- 训练与监督分离：训练归 iDoris；Agent24 只做经同意的数据导出，默认无上送。

### D6 不做的

- 不做主系统外的独立 Watchdog Session：Agent24 是单用户本地 daemon，没有第二个进程可信地拿到权限；进程级存活由服务管理器管（ADR-037 决策 2）。
- 不让审查器「在线修正主系统行为」：违反 ADR-039 规则 3。
- 不做多工作者并行（远期，见终稿 §7）。

### D7 对外

用户看到：工具调用的「后台运行中」、作业输出、「系统需要你看一眼」、工单的预算用尽提示。作业表、审查器、并发槽都是内部名词。

## 3. 备选方案及否决理由

| 备选 | 否决理由 |
|---|---|
| `shell_exec` 保持同步，只提高超时 | 会话被长命令阻塞就不能接收新输入；工单「等人」阶段会被一个 `sleep` 卡死 |
| 作业完成直接回写 run 上下文（不经事件与动作队列） | run 可能已结束；经 ADR-042 队列才能在掉电后不丢完成通知 |
| 不限并发，由硬件分档兜底 | HW-TIERS 只管模型内存；两个工单同时写一个工作区会静默覆盖 |
| 只监控进程健康 | 发现不了空转（Codex） |
| 审查器用云模型 | 它读的是工单事实与输入摘要，可能含 `Restricted` 来源；K1-02 fail-closed |
| 审查器可以改准入表、关闭失控的主动技能 | 「收紧」也是对用户配置的静默修改；原文 §6 是团队场景下维护者授权的，个人分身没有这个维护者 |
| 给监督者完整权限自修 | 第二个高权限失控点（Codex） |
| 审查器的 `retry_action` 不限动作状态 | 可能对 `result_unknown` 的 `channel_send` 重投，把可能已经发出的消息再发一遍，等于模型绕过人重复外发（PR-Daemon #1530 M-1） |
| 用桌面的 5 秒健康检查扩展成监督 | 桌面是外壳（ADR-037 决策 1），退出后 daemon 还在 |
| 只测模型基准 | 不能证明产品交付可靠（Codex） |

## 4. 后果

- 好处：会话全双工；长任务不再阻塞审批与输入；一项工作阻塞不拖死人直接对话；偏航能被发现；审查器有硬上限；进步有可比较的度量。
- 代价：`shell_exec` 语义变化要同步桌面 / TUI 展示与 OpenAPI；三个新工具；`/health` 契约变更进 G-API-3；`jobs` 表前置 CORE-5 5f–5g。
- 风险：模型滥用 `job_wait`（靠上限与计数）；`stalled_work_items` 阈值没有现成数字（Q3）；审查器误报让用户厌烦（默认关、先度量）。

## 5. 与其他 ADR 的关系

- 实现 K1-01 §2.4 `pending` 句柄的落点。
- 完成事件经 ADR-042 `deliver_input` 投递；指标来自 ADR-042 的队列与 ADR-041 的工单阶段。
- 审查器的提案走 ADR-040 D5 收件箱。
- `/health` 字段随 CORE-5 5f 一起改，不另起契约。
- 建议 ADR-039 §2.1 本体职责加「后台作业与监督回路」（终稿 §6）。

## 6. 开放问题

- Q1 `background_after_ms` 默认 5 秒是否适合本地模型推理场景；`shell_exec` 与模型调用可能要分别设。
- Q2 作业输出文件的保留期与 purge。
- Q3 `stalled_work_items` 阈值；建议先从 `observe` 期间的工单记录统计分布再定。
- Q4 审查器「偏航」评测用例从哪来：建议先收集 `observe` 期间的工单记录做离线集。
- Q5 预算的初值按 HW-TIERS 档位给默认，还是统一后由用户改。

## 7. 评审记录

- 来源：Fable 初稿 `adr-drafts/ADR-044-background-jobs-and-supervision.md`（编号改为 043），吸收 Codex 草案「ADR-044 有界并行、独立监督与交付评测」。
- Codex 改了什么：加并发、预算与资源写入冲突控制归本体（D2）；要求监督者无权突破暂停、自动激活技能候选（D4）；要求评测覆盖准入、执行、安全、恢复、交付、人类介入成本，并有可比较基线与故障注入（D5）；要求训练与监督分离、个人数据不默认上传（D5）；并发增益要用真实完成率与人类介入时间证明（D2）。
- 裁判改了什么：裁决 12 把并发槽定为 v1「1 + 1 保留槽」并采原型「人的消息优先」；加每工单预算与同资源写权（D2）；加后台作业执行前重验撤销 / 到期（D1，K1-02 §2.2）；`job_wait` 加计数（D1）；D3 明确「模型自报进展不算产物」；D5 写明合成结果不写成真实能力；核实 `shell_exec` 预算 30 秒与 `module_tool_runtime.rs` 的 `pending` 只是内部标志（§1）。
