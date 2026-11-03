# ADR-042：工单与授权依据（草案）

- 状态：**提议**（2026-11-01，Fable 初稿，待 Codex 挑战与 jason 拍板）
- 上位：[ADR-039](../../../adr/ADR-039-concept-model.md) §2.1（本体负责审批与审计、调度）、§4「资源」
- 相关：ADR-041（入站事件与准入）、ADR-043（待执行动作）、ADR-K1-03 §1、§3（逐工具授权、撤销）、ADR-K1-02 §2.1.1、ADR-MEMORY-P1 §9④（在途与外部副本）
- 依据：[研究初稿](../SUPERAGENT-EVOLUTION-2026-11-01-fable.md) §2.2、§3.1 第 1、7 条；原型 `docs/001-domain.md` Ticket / Mandate / Assignment / Subscription、`docs/003-execution.md` 阶段表与结束条件、`docs/002-authorization.md` Subscriptions（[SOURCES](../SOURCES.md) S3、S4、S2）

## 1. 背景

Agent24 的工作单元是 run：`queued → running ⇄ awaiting_approval → completed | failed | cancelled`（`agent24-protocol/src/types.rs:411`）。一个 run 是一次执行。用户的一件事常常跨多个 run：等人确认、等对方回复、等文档改完；今天没有东西把这串 run 连成一件事，也没有东西记录「这件事凭什么开始、范围多大、谁要求的」。

capability token 和 K1-03 许可回答的是「谁可以调什么」；审批回答的是「这一次工具调用准不准」。都不回答「这项工作是否被授权、到哪为止」。

原型的做法：Ticket 是授权了的工作单元，只有 Eval 能创建；它引用一个不可变的 Mandate（策略版本、范围、强度、发起者身份、来源接收方式）；范围更宽意味着新 Ticket；只有人的 stop 撤回 Mandate，策略不再成立时吊销；Ticket 结束后 Mandate 不结束，人在原讨论串的消息可以在同一 Mandate 下恢复它。Assignment 把 Ticket 绑定到一次执行，一张 Ticket 多次派工。Subscription 让后续事件直达 Ticket 作为上下文，不检查发送者，也不加权限。阶段表的第一行是「需要人」。Worker 报 ready 不等于 PR 合并，完成条件分开判。

## 2. 决策

### D1 工单：跨 run 的工作单元，本体内部资源

- 新增本体内部资源 **工单**（`work_items` 表）：`id`、`title`、`origin_event_id`（ADR-041）、`mandate_id`、`stage`、`outcome`、`ended_at`、`ended_by`、`session_id`（可空）。
- run 增加可空字段 `work_item_id`。旧 run 与用户直接发起的对话 run 不带工单，行为不变。
- 一张工单可以有多个 run（原型 Assignment）；同一时刻最多一个 run 在跑。
- 工单阶段按「它在等什么」判定，第一行优先：`person/answer`（有待人回答的问题）→ `person/review`（已交付等验收）→ `person/blocked`（失败或受阻且之后无人消息）→ `working/queued`（无 run 或 run 排队）→ `working`（run 在跑）→ `idle`（等下一个事件）。用户界面「待处理」只列 `person/*`。

### D2 授权依据：不可变，范围扩大要新授权

- 新增本体内部资源 **授权依据**（`mandates` 表）：`id`、`policy_version`（ADR-041 D4）、`strength: explicit | inferred`、`scope`（文本 + 允许的能力 id 列表 + 允许的外发目的地列表）、`actor`（来源原生身份）、`source`、`basis: addressed | followup | rule`、`matched_skills[]`、`created_at`、`withdrawn_at`、`revoked_at`。
- 创建后不可修改。需要更大范围时建新工单、新授权依据；旧工单结束。
- 三种变化方式：创建（ADR-041 `explicit` 事件或主动技能 `auto` 匹配）、撤回（用户主动停止）、吊销（恢复前重新验证策略不成立：发起者已不在名单、匹配的主动技能已关、接受模式退回 `off`）。
- 授权依据**不代替**工具审批与 K1-03 许可。run 里每次工具调用照常审批；授权依据只是「可以开始、能做到哪」的上限，K1-03 的许可与审批只能更严。

### D3 订阅：只加上下文，不加权限

- 工单可以订阅若干讨论串（`subscriptions` 表：`work_item_id`、`thread_key`、`kind: own | context`）。订阅讨论串上的新事件直达工单作为输入，不经准入、不经研判，也不改变授权依据。
- 用户在工单自己的讨论串上的后续消息，由工单的 run 决定是否在范围内；超范围的交回准入（ADR-041）。`context` 类订阅不能唤醒已结束的工单。
- 订阅来的事件文本按 K1-02 §2.1.1 打来源标签，出口照旧 fail-closed。「不检查发送者」只指不做准入，不指可信。

### D4 完成条件分开：就绪 ≠ 完成

- run 报 `completed` 不结束工单。工单的结束由**交付记录**决定：有外部交付物（发出去的消息、提交的文档、创建的日程）时，等交付记录到 `sent` / `confirmed`（ADR-043）再进 `person/review`；用户确认或超过保留期自动结束。
- 没有外部交付物的工单，run 完成即结束。
- `result_unknown` 的动作（ADR-043）把工单置于 `person/blocked`，不自动重试。

### D5 撤销与在途

- 用户停止工单：撤回授权依据；取消正在跑的 run（现有 `CancellationToken`）；按 K1-03 §3 与记忆 P1 §9④：fence 之后不再授予新的发送或工具许可，在途动作尽力取消，已出去的列为「失控副本」如实显示，不承诺撤回。
- 策略变更不自动取消工单，只投一条输入（ADR-041 D4）。

### D6 对外

用户看到的是「待处理」「进行中」「已完成」三组，加「停止」按钮。工单、授权依据、订阅都不出现在界面文案里。

## 3. 备选方案及否决理由

| 备选 | 否决理由 |
|---|---|
| 扩展 session 加授权字段，不加新对象 | session 是对话容器，用户可以在一个会话里起多件事、一件事跨多个会话（微信群里起，桌面上改）；原型 001 否决「Ticket 跟随 Session」理由相同。**但这是交 Codex 挑战的第一点**（研究初稿 §6 Q1） |
| 用 run 的 `parent_run_id` 串起来 | 串得起来但没有「凭什么开始」；也回答不了「等人」阶段 |
| 授权依据可修改范围 | 原型与 K1-03 §4 一致：扩大范围 = 重新同意；可修改就无法审计「当时凭什么」 |
| 订阅事件也过准入 | 原型 001 否决（Admission checks on Subscriptions）：订阅覆盖整个讨论串，授权由 Mandate 分离；再过一遍准入会把名单外的人的跟进全部丢掉，分身就不会「听」了 |
| 完成 = run completed | 发出去的消息对方没收到、文档提交失败时，用户看到「已完成」是谎言（原型 004「诚实交付」） |

## 4. 后果

- 好处：一件事有连续身份；「凭什么做」可审计；待处理列表有确定定义；就绪与完成分开。
- 代价：三张新表；run 加字段；桌面 / TUI 加「待处理」视图；工单也要进记忆 P1 的 purge 范围。
- 风险：对直接聊天的用户是额外一层；要保证无工单的 run 路径零改动。

## 5. 与其他 ADR 的关系

- 工单由 ADR-041 的 `explicit` 事件或 ADR-040 D5 的主动技能创建。
- 交付记录来自 ADR-043。
- 授权依据的撤回沿用 K1-03 §3 与记忆 P1 §9④ 的 fence 语义。
- 建议 ADR-039 §2.1 本体职责加「工单与授权依据」；§4 资源举例加「工单」。
- CORE-2：`GET /api/v1/work-items` 登记新 `Operation::WorkItemsRead`；停止是提升级。

## 6. 开放问题

- Q1 工单是否要暴露给组件（领域 OS 想知道「这是哪件事」）。建议 v1 不暴露，只在 K1-01 可信上下文里带 `work_item_id` 作关联。
- Q2 一个工单的 run 之间的上下文怎么传：共用 session，还是每次只传工单摘要 + 稳定 id 引用。建议后者（Inline / Spill）。
- Q3 授权依据的 `scope` 用自然语言还是结构化。建议两者都存：结构化给机器判，文本给人看。
- Q4 工单的保留期与 purge 粒度（记忆 P1 §9① 首版只做全域）。
