# ADR-042：待执行动作与对账

索引：[架构总入口](../architecture/README.md)・[ADR 索引](README.md)

- 状态：**提议**（2026-11-01，三步评审终稿，待 jason 拍板）
- 上位：[ADR-039](ADR-039-concept-model.md) §2.1（本体负责审批与审计、调度）；ADR-026 硬约束 2（fail-closed）
- 相关：ADR-K1-01 §2.4–2.6（有界调用、结果未知、不自动重试）——**本 ADR 是 §2.6 预留的「另开 ADR」，范围只到 D3**；[ADR-038](ADR-038-component-contract-v1.md) D6、D9；[ADR-041](ADR-041-inbound-events-admission-and-work-items.md) D8（完成条件）；CORE-5 §3 T9（掉电验收方法）；`docs/design/COMM-HYPHAE.md` §5（消息四层、outbox）
- 依据：[演进终稿](../research/deepseek-superagent/SUPERAGENT-EVOLUTION-2026-11-01.md) §2 裁决 5；文章 S1「为什么数据库和 Outbox 比更多 Prompt 更关键」与故障表；原型 `docs/007-architecture.md` Effects / Outbox、`docs/008-cookbook.md` §5、`docs/003-execution.md` Stored work / Channel failure；[SOURCES](../research/deepseek-superagent/SOURCES.md) S1、S2、S6

## 1. 背景

Agent24 已有三处「先存后做」的零件，没有统一：

| 零件 | 位置 | 覆盖 |
|---|---|---|
| 审批 | `agent24-policy`：store 行权威，内存通道只唤醒 | 工具调用准不准 |
| 调度投递状态机 | `agent24-scheduler::deliveries` + `FireOutcome`（`agent24d/src/scheduler_deliver.rs`） | 日程触发投给模块 |
| Hyphae outbox | `agent24-comm/src/router.rs:175-177` `/outbox`、`/outbox/{event_id}/retry`、`/outbox/clear`；状态在 Hyphae 二进制里，daemon 只转发 | Nostr 发送 |

没有覆盖的：daemon 自己决定要做的外部动作（发消息、开 run、通知用户、向 run 投输入）在决定和执行之间没有持久记录。进程在「决定了但还没做」时被杀，重启后不知道自己做过什么；发送成功但响应丢失时，重启后不知道该不该再发。

K1-01 §2.6 已定：内核不自动重试模块工具调用，不做传输层去重，`tool_call_id` 不是幂等键；结果可能已派发而无可信终态时报 `result_unknown`；将来要传输层去重必须另开 ADR。今天 Hyphae 发送走的是 daemon 内部 REST（`router.rs:172` `/send` → `runner.rs`），不是模块工具调用，所以「通道发送」与「模块工具调用」在代码上本来就是两条路。

原型的做法：命令在事务内 `enqueue(kind, key, payload)`，提交后才执行；同 kind/key 只一行；指数退避；`Later` 推迟不计失败；Outbox 在同一事务里写收据，适配器用消息 id 作幂等键（GitHub 评论里埋 HTML 标记、再发前先查）；发送结果三态 `sent / gone / 抛错重试`；失败三次才在面板报错；同工单同会话的消息保序。文章提醒：这不是跨 SQLite 与外部系统的 exactly-once。

## 2. 决策

### D1 本体有一个待执行动作队列，决定与动作同一事务

- 新增本体内部资源**待执行动作**（`pending_actions` 表）：`id`（本体生成，稳定）、`kind`、`key`、`payload`、`order`、`attempts`、`last_error`、`due_at`、`generation`（执行代际）、`mandate_id`（可空）、`status: waiting | running | done | dropped`。`(kind, key)` 唯一；重复入队覆盖 payload 并立即到期，不改顺序。
- 入队只能在产生该决定的同一 store 事务里；执行只在提交之后（sqlx 事务；对应原型 `afterCommit`）。
- 区分**业务动作身份**（`id`，跨尝试不变）与**每次调用身份**（`attempt`，每次尝试一个）。
- 每种 `kind` 一个处理器、一个并发上限、一张退避表。首批 kind：`channel_send`、`run_launch`（为工单起 run）、`notify`（通知用户）、`deliver_input`（向在跑的 run 投输入）、`schedule_fire`（迁入现有投递状态机，语义不变）。
- 处理器可返回 `Later(duration)` 表示「条件未满足，不算失败」（没有可用模型、组件未就绪、全局暂停）。
- **每次执行前复验**：`generation` 未变、`mandate_id` 未撤回或吊销、K1-02 来源策略与 K1-03 授权仍有效。任一不成立 → `dropped(reason)`，不执行。
- 失败按指数退避重试；连续失败三次才在 `/health.work` 报错（ADR-043 D3）。
- 全局暂停：暂停时不执行任何动作，入站事件照常记录。

### D2 通道发送：收据 + 幂等键 + 对账

- 每条对外消息先写**收据**（`channel_receipts` 表：`message_id`、`work_item_id`、`channel`、`target`、`payload_digest`、`status: pending | sent | dropped`、`dropped_reason`），与 `channel_send` 动作同一事务。
- 幂等键 = `message_id`（本体生成）。通道组件在 ADR-038 D9 `events.channels[]` 里声明 `idempotency_key: bool`：
  - 支持：发送时带 `message_id`，恢复重投安全；
  - 不支持：组件必须实现**对账**（再发前按 `message_id` 查已发记录，原型 GitHub 适配器做法）；两者都没有的通道，Agent24 不自动重投，第一次失败即 `result_unknown`，由人决定。
- 发送结果三态：`sent`、`gone`（目的地永久不可达，收据标 `dropped` 并记原因，不再入队）、错误（重试）。
- 同一工单发往同一目的地的消息保序：后一条等前一条 `sent`，除非前一条已失败三次。其他工单的消息不等。
- 同键不同载荷：拒绝入队并报错，不覆盖（`payload_digest` 比对）。
- Hyphae 迁移：daemon 的 `message_id` 映射到 Hyphae 的 `event_id`；Hyphae 自己的 outbox 保留为传输层重试，daemon 的收据是权威。`/outbox/clear` 不再是用户入口。

### D3 模块工具调用：默认不进队列；一条窄例外

- 模块工具调用（K1-01）仍由 run 内同步发起、不自动重试、`result_unknown` 如实返回。本队列只放本体自己决定的外部动作。
- **窄例外（K1-01 §2.6 预留的「另开 ADR」）**：组件在 ADR-038 D9 清单里对某个工具标 `idempotency_key: true` 时，本体可以把对它的调用作为 `kind: module_call` 动作入队，用动作 `id` 作该工具的幂等键，允许恢复重投。条件：① 清单明确声明；② 每次重投前按 D1 复验授权；③ 组件对同键不同载荷必须拒绝；④ 组件可返回 `replayed: true`。不满足任一条的工具不得入队。
- 作用域：只覆盖本体按 D1 入队的动作；run 内模型直接发起的工具调用不受影响，仍按 K1-01。持久期与收据同（开放问题 Q2）。
- v1 没有实际的模块工具满足声明条件（Hyphae 发送不是模块工具），本条只定边界。
- `result_unknown` 的结果进工单阶段 `person/blocked`（ADR-041 D8），由人决定是否重新授权再发一次**新**调用。

### D4 不声明跨系统 exactly-once

- 文档、界面、审计都只说「至多一次入队、至少一次尝试、按幂等键或对账去重」。不说「保证只发一次」。
- 取消不代表已发生的副作用被回滚；界面如实显示「已发出、未确认」。
- 离线验收用 CORE-5 T9 的方法：文件操作层故障注入模拟掉电，在每个持久边界重启，断言收据与动作一起回滚或一起存在。

### D5 对外

用户看到：消息的「已发送 / 发送失败 / 无法送达 / 已发出未确认」；失败三次后的「需要处理」提示。队列、收据是内部名词。

## 3. 备选方案及否决理由

| 备选 | 否决理由 |
|---|---|
| 把 Hyphae 的 outbox 当权威，daemon 不记收据 | 换通信组件（ADR-038 D8）时收据跟着丢；Hyphae outbox 不知道工单 |
| 所有失败指数退避重试 | 可能重复副作用（Codex） |
| 永不恢复任何动作 | 容易丢交付（Codex） |
| 模块工具调用全部进队列自动重试 | 违反 K1-01 §2.6；本体没有业务键就不能替模块重试，会把「结果未知」伪装成失败再做一次 |
| 用 `tool_call_id` / `run_id` 作幂等键 | K1-01 §2.6 明文禁止 |
| 队列放内存，靠重启后重放审计日志 | 审计日志是只追加的证据，不是执行状态；记忆 P1 §9② 已指出同库哈希链不能当锚 |
| 直接用消息队列中间件 | 本地优先、单用户；SQLite 表够用 |
| 宣称全局 exactly-once | 跨外部服务没有共同事务 |

## 4. 后果

- 好处：掉电、崩溃、响应丢失有确定恢复路径；换通信组件不丢收据；工单的「就绪 ≠ 完成」有数据来源；K1-01 §2.6 的「另开 ADR」有了边界清楚的落点。
- 代价：两张新表、五个处理器；Hyphae 发送路径改造；通道组件要声明幂等支持；每次执行前的复验增加一次 store 读。新表前置 CORE-5 5f–5g。
- 风险：`Later` 与退避参数调不好会让动作饿死或雪崩，要靠 `/health.work` 的最老等待指标；D3 窄例外若被扩大解释会架空 K1-01，所以条件写成闭集。

## 5. 与其他 ADR 的关系

- 为 ADR-041 D8 提供交付记录，为 D4 提供「策略已变」输入、为 D5 提供 `run_launch`。
- 不改 K1-01 §2.4–2.6；D3 是其预留例外的唯一实例。
- ADR-038 D9 `events.channels[].idempotency_key` 与 `provides[].tools[].idempotency_key` 见终稿 §6 建议修订。
- 掉电验收方法沿用 CORE-5 §3 T9；ADR-043 的完成事件经 `deliver_input` 投递。

## 6. 开放问题

- Q1 `deliver_input` 要求 run 中途可收输入；现在 run 是一次 prompt。可能先做 ADR-043 的作业再回答。
- Q2 收据与幂等键保留期，与记忆 P1 purge 的关系；外部平台查询延迟时对账等多久。
- Q3 `notify` 与桌面通知、TUI 提示的去重。
- Q4 通道既不支持幂等键也不做对账时，是否禁止自动发送、全部人工点发。建议是。
- Q5 多实例（同一用户两台机器）不在 v1；单例锁已排除本机并发。

## 7. 评审记录

- 来源：Fable 初稿 `adr-drafts/ADR-043-durable-actions-and-reconciliation.md`（编号改为 042）。
- Codex 改了什么：要求区分业务动作身份与每次调用身份（D1）；要求绑定目标、载荷摘要、执行代际、授权引用并在重试前重新校验权限（D1、D2）；要求「只为明确声明且验证过幂等 / 查询契约的动作建窄例外，明确对既有不重试规则的例外范围」（D3）；加同键不同载荷的处理（D2）；加「取消不代表副作用回滚」（D4）。
- 裁判改了什么：裁决 5 把 Fable 的「模块工具一律不进队列」改成「默认不进 + 闭集条件的窄例外」，并声明本 ADR 就是 K1-01 §2.6 预留的 ADR；核实 Hyphae 发送是 daemon 内部 REST 而非模块工具，写进背景；加执行前复验的四项条件；加多实例不在 v1（Q5）。
