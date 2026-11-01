# ADR-043：待执行动作与对账（草案）

- 状态：**提议**（2026-11-01，Fable 初稿，待 Codex 挑战与 jason 拍板）
- 上位：[ADR-039](../../../adr/ADR-039-concept-model.md) §2.1（本体负责审批与审计）；ADR-026 硬约束 2（fail-closed）
- 相关：ADR-K1-01 §2.4–2.6（有界调用、结果未知、不自动重试）、ADR-042（工单完成条件）、CORE-5（掉电验收方法 T9）、`docs/design/COMM-HYPHAE.md` §5（消息四层、outbox）
- 依据：[研究初稿](../SUPERAGENT-EVOLUTION-2026-11-01-fable.md) §2.3、§3.1 第 3 条；文章 S1「为什么数据库和 Outbox 比更多 Prompt 更关键」与故障表；原型 `docs/007-architecture.md` Effects / Outbox、`docs/008-cookbook.md` §5（[SOURCES](../SOURCES.md) S1、S2、S6）

## 1. 背景

Agent24 已有三处「先存后做」的零件，没有统一：

| 零件 | 位置 | 覆盖 |
|---|---|---|
| 审批 | `agent24-policy`：store 行权威，内存通道只唤醒 | 工具调用准不准 |
| 调度投递状态机 | `agent24-scheduler::deliveries::apply_outcome` + `FireOutcome`（`agent24d/src/scheduler_deliver.rs`） | 日程触发投给模块 |
| Hyphae outbox | `agent24-comm/src/router.rs:175-177` `/outbox`、`/outbox/{event_id}/retry`、`/outbox/clear` | Nostr 发送；状态在 Hyphae 二进制里，daemon 只转发 |

没有覆盖的：daemon 自己决定要做的外部动作（发消息、开 run、通知用户、写外部文档）在决定和执行之间没有持久记录。进程在「决定了但还没做」时被杀，重启后不知道自己做过什么。

K1-01 §2.6 已定：内核不自动重试模块工具调用，不做传输层去重，`tool_call_id` 不是幂等键；结果可能已派发而无可信终态时报 `result_unknown`。本 ADR 不改这条。

原型的做法：命令在事务内 `enqueue(kind, key, payload)`，提交后才执行；同 kind/key 只一行，重复 enqueue 覆盖 payload；指数退避；`Later` 推迟不计失败；Outbox 在同一事务里写收据并排 `reply` 动作，适配器用消息 id 作幂等键（GitHub 评论里埋 HTML 标记、再发前先查）；发送结果三态 `sent / gone / 抛错重试`；失败三次才在面板报错；同工单同会话的消息保序。文章提醒：这不是跨 SQLite 与外部系统的 exactly-once；多实例与网络分区会带来新竞争条件。

## 2. 决策

### D1 本体有一个待执行动作队列，决定与动作同一事务

- 新增本体内部资源 **待执行动作**（`pending_actions` 表）：`kind`、`key`、`payload`、`order`、`attempts`、`last_error`、`due_at`、`status: waiting | running | done | dropped`。`(kind, key)` 唯一；重复入队覆盖 payload 并立即到期，不改顺序。
- 入队只能在产生该决定的同一 store 事务里；执行只在提交之后（现有 sqlx 事务；对应原型 `afterCommit`）。
- 每种 `kind` 一个处理器、一个并发上限、一张退避表。首批 kind：`channel_send`（通道发送）、`run_launch`（为工单起 run）、`notify`（通知用户）、`deliver_input`（向在跑的 run 投输入）、`schedule_fire`（迁入现有投递状态机）。
- 处理器可返回 `Later(duration)` 表示「条件未满足，不算失败」（例如没有可用模型、组件未就绪）。
- 失败按指数退避重试；连续失败三次才在 `/health` 的 `work` 条目报错（ADR-044）。
- 全局暂停：暂停时不执行任何动作，入站事件照常记录（原型 003 Pause）。

### D2 通道发送：收据 + 幂等键 + 对账

- 每条对外消息先写**收据**（`channel_receipts` 表：`message_id`、`work_item_id`、`channel`、`status: pending | sent | dropped`、`dropped_reason`），与 `channel_send` 动作同一事务。
- 幂等键 = `message_id`（本体生成，稳定）。通道组件（Hyphae、微信桥、将来的邮件）在 ADR-038 D9 `events.channels[]` 里声明是否支持幂等键：
  - 支持：发送时带 `message_id`，重试安全；
  - 不支持：组件必须实现**对账**（再发前按 `message_id` 查已发记录，原型 GitHub 适配器做法），否则 Agent24 对该通道不自动重试，第一次失败即 `result_unknown`。
- 发送结果三态：`sent`、`gone`（目的地永久不可达，收据标 `dropped` 并记原因，不再入队）、错误（重试）。
- 同一工单发往同一目的地的消息保序：后一条等前一条 `sent`，除非前一条已失败三次。
- Hyphae 迁移：daemon 的 `message_id` 映射到 Hyphae 的 `event_id`；Hyphae 自己的 outbox 保留为传输层重试，daemon 的收据是权威。两层各记各的，`/outbox/clear` 不再是用户入口。

### D3 模块工具调用不进本队列

- 模块工具调用（K1-01）仍由 run 内同步发起、不自动重试、`result_unknown` 如实返回。本 ADR 的队列只放**本体自己决定的**外部动作。
- 理由：模块工具的业务幂等由模块按业务键实现（K1-01 §2.6）；本体没有业务键就不能替它重试。
- `result_unknown` 的结果进工单阶段 `person/blocked`（ADR-042 D4），由人决定是否重新授权再发一次新调用。

### D4 不声明跨系统 exactly-once

- 文档、界面、审计都只说「至多一次入队、至少一次尝试、按幂等键或对账去重」。不说「保证只发一次」。
- 离线验收用 CORE-5 T9 的方法：文件操作层故障注入模拟掉电，在每个持久边界重启，断言收据与动作一起回滚或一起存在。

### D5 对外

用户看到：消息的「已发送 / 发送失败 / 无法送达」三态；失败三次后的「需要处理」提示。队列、收据是内部名词。

## 3. 备选方案及否决理由

| 备选 | 否决理由 |
|---|---|
| 把 Hyphae 的 outbox 当权威，daemon 不记收据 | 换通信组件（ADR-038 D8「换品牌只改锁」）时收据跟着丢；而且 Hyphae outbox 不知道工单 |
| 模块工具调用也进队列自动重试 | 违反 K1-01 §2.6；副作用重复的责任在模块业务键，本体替它重试会把「结果未知」伪装成失败再做一次。**交 Codex 挑战**（研究初稿 §6 Q3） |
| 用 `tool_call_id` / `run_id` 作幂等键 | K1-01 §2.6 明文禁止 |
| 队列放内存，靠重启后重放审计日志 | 审计日志是只追加的证据，不是执行状态；而且记忆 P1 §9② 已指出同库哈希链不能当锚 |
| 直接用消息队列中间件 | 本地优先、单用户；SQLite 表够用，原型也只用一个 SQLite 文件 |

## 4. 后果

- 好处：掉电、崩溃、响应丢失有确定恢复路径；换通信组件不丢收据；工单的「就绪 ≠ 完成」有数据来源。
- 代价：两张新表、五个处理器；Hyphae 发送路径改造；通道组件要声明幂等支持。
- 风险：`Later` 与退避参数调不好会让动作饿死或雪崩；需要 `/health` 的最老等待指标（ADR-044）盯着。

## 5. 与其他 ADR 的关系

- 为 ADR-042 D4 提供交付记录；为 ADR-041 提供「策略变更投输入」与「为工单起 run」的动作。
- 不改 K1-01 §2.4–2.6；D3 明确边界。
- ADR-038 D9 `events.channels[]` 新增 `idempotent: bool` 字段（随 ADR-041 的 `events` 块一起）。
- 掉电验收方法沿用 CORE-5 §3 T9。

## 6. 开放问题

- Q1 `deliver_input`（向在跑的 run 投输入）与现有 run 的消息模型怎么接：run 是否允许中途收输入（现在是一次 prompt）。可能要先做 ADR-044 的后台作业再回答。
- Q2 收据保留期与记忆 P1 purge 的关系。
- Q3 `notify` 动作与桌面通知、TUI 提示的去重。
- Q4 通道组件不支持幂等键也不做对账时，是否干脆禁止自动发送、全部人工点发。建议是。
