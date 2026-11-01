# ADR-041：入站事件、准入与工单

- 状态：**提议**（2026-11-01，三步评审终稿，待 jason 拍板）
- 上位：[ADR-039](ADR-039-concept-model.md) §2.1（本体职责）、§3 规则 1（能力向本体发事件，由本体按触发规则、经审批决定）、§4「资源」、§5 第三问；ADR-037 决策 2（常驻 daemon）
- 相关：[ADR-038](ADR-038-component-contract-v1.md) D9（组件清单）；ADR-K1-02 §2.1.1（分类 / 处理模式 / 授权三维度）；ADR-K1-03 §1、§3（逐工具授权、撤销）；ADR-MEMORY-P1 §9④（在途与外部副本）；[ADR-040](ADR-040-skill-contract-v1.md) D6（主动技能）；[ADR-042](ADR-042-durable-actions-and-reconciliation.md)（交付记录）
- 依据：[演进终稿](../research/deepseek-superagent/SUPERAGENT-EVOLUTION-2026-11-01.md) §2 裁决 1、2、4、10、11；原型 `docs/001-domain.md`、`002-authorization.md`、`003-execution.md`（固定提交 `6337fc8`，裁判全文读过）；[SOURCES](../research/deepseek-superagent/SOURCES.md) S3、S4、S6

## 1. 背景

Agent24 今天有**五条**入站路径，各走各的：

| 路径 | 代码 | 稳定 id | 准入 |
|---|---|---|---|
| 日程触发 | `agent24-scheduler`，`ScheduleAction::AgentRun`（`agent24-protocol/src/types.rs:875`） | `FireId::derive`（`fire.rs:25`），投递状态机 | 用户自建，无需准入 |
| 自唤醒 | `agent24-agent/src/self_wake.rs`（一次性 schedule） | 同上 | 同上，上限 32 个 |
| Hyphae 收件箱 | `agent24-comm/src/router.rs:174` `POST /inbox/pull` | Hyphae 的 `event_id`，daemon 只转发 | 无 |
| Nostr 桥 | `packages/nostr-bridge/src/inbound.ts` | 内存 `Set` 去重（上限 10 000，重启丢失） | npub 名单，fail-closed；桥内判定 |
| 微信桥 | `packages/wechat-bridge/src/bridge.ts` | 桥内会话文件；无事件 id | uid 名单，fail-closed；每用户串行；微信里回 y/n 解决审批 |

接 AgentEar（语音）后是六条。没有统一记录就没法回答：这条消息处理过没有；谁能让我的分身干活；`observe` 模式下它会做什么；一件事做到哪了。

工作单元只有 run：`queued → running ⇄ awaiting_approval → completed | failed | cancelled`（`types.rs:411`），一次执行。`plan_resume` 只恢复 `AwaitingApproval`（`resume.rs:87`）。用户的一件事常跨多个 run（等人确认、等对方回复），今天没有东西把它们连成一件事，也没有东西记录「凭什么开始、范围多大、谁要求的」。`Session` 是按渠道的对话容器（`types.rs:376`），一个会话可起多件事、一件事可跨渠道，所以不能靠扩展 session 解决。

原型的做法（001–003）：Source 持读位置、丢自己的消息；Trigger 不可变、id 由原生 id 派生、接受两次只建一个工单；准入先走确定性 gate（名单、是否 @、接收范围）决定 strength，`inferred` 候选才需模型研判，且主动接单至少要匹配一条启用的规则；接受模式 `off / observe / auto`；Ticket 独立于 Session，Mandate 不可变、范围更宽 = 新 Ticket；Subscription 扇出不检查发送者、不加权限；ready 报告 ≠ PR 合并。原型否决了「Ticket 跟随 Session」「订阅过准入」「跨 Source 映射身份」「策略变更取消运行中 Ticket」。

## 2. 决策

### D1 本体有一个入站事件入口，所有来源都走它

- 新增本体内部资源**入站事件**（`inbound_events` 表）：`id`（`<source>:<native_id>[:<edit_version>]`）、`source`（组件 id 或 `scheduler` / `self`）、`thread_key`（来源定义的讨论串键，可空；Hyphae 群聊用 `group_id`）、`actor`（来源原生身份，可空）、`human: bool`、`addressed: none | mention | direct`、`at`、`text`（上限默认 4 000 字）、`meta`（来源自有 JSON，不可信）、`disposition`。
- 同 `id` 再次写入返回 `duplicate`，不建第二条记录，不触发第二次处理。编辑过的消息要当新事件时，来源在 id 里带版本（原型 008 §3）。
- 五条现有路径改为适配器：scheduler 投递与 self_wake 写入 `source = scheduler | self`；Hyphae 收件箱由 daemon 主动拉取写入；nostr-bridge 与 wechat-bridge 改为只做传输，名单与去重搬进 daemon（迁入前桥的内存去重不算满足本条）。
- 入站事件不是出站事件流（`protocol/events.schema.json` 的 `run.* / tool.* / schedule.* / approval.*`）。分表、分名；入站内容不进 WS `/events` 广播。

### D2 来源的职责：身份、读位置、范围、讨论串

- 作为事件源的组件在 ADR-038 D9 清单里加可选块 `events: { id_rule, cursor: bool, threads: bool, channels: [...] }`（ADR-038 建议修订 1）。
- 来源负责：持读位置（daemon 不在时不丢、重启不重放）；标明 `actor` 与 `human`；标明 `addressed`；先过滤自己发出的消息；把范围外事件标 `filtered` 写入（仍记录，便于解释），不建讨论串。
- 来源**不负责**准入判定。准入规则属于本体策略，组件只提供事实（ADR-039 规则 1：能力不能指定技能）。
- 身份以来源原生 id 记（npub、微信 id、AgentEar 的本机用户）；**不跨来源映射身份**。

### D3 准入：确定性门先于任何模型判断

- 每个来源有一张准入表，按**接收范围 × 发起者是否在名单 × 是否 @ 分身**决定三种结果：`explicit`（明确请求）、`inferred`（候选）、`ignore`。默认：名单内且 @ 或私信 → `explicit`；名单内未 @ → `inferred`；名单外 → `ignore`。bot / CI 最多 `inferred`。
- **身份、名单、是否 @、接收范围只由确定性代码判；模型从不判断谁有权让分身干活。**
- `explicit` 事件直接进 D5 的工单创建；`ignore` 只记 `disposition`。
- `inferred` 事件「算不算请求、值不值得做」是语义问题，需要模型研判，但**只在至少有一个启用的主动技能（ADR-040 D6，`observe` 或 `auto`）时才起模型**；没有主动技能时记 `ignore(no_rule)`，在 `observe` 记录里可见，便于用户发现漏接单。
- 研判输入是外部文本，按 K1-02 §2.1.1 打 `Unknown` 标签，研判只能用 LocalOnly 模型（Guardian 同形态）；研判结果只能是「匹配哪条主动技能」，不能产生权限。

### D4 接受模式与策略版本

- 本体有一个全局接受模式 `off | observe | auto`，默认 `observe`；主动技能各自再有一档（ADR-040 D6），取两者更严者。`explicit` 请求在任何模式下都接单。
- 准入表 + 名单 + 接受模式 + 启用的主动技能集合构成**策略版本**（哈希）。每条接单记录（D6）记它接受时的策略版本。
- 策略变更：正在研判的候选作废并按新版本重排；受影响的工单收到一条「策略已变」输入，不自动取消。撤销写权限走 K1-03 §3 的撤销，不走策略。

### D5 工单：跨 run 的工作单元，本体内部资源

- 新增本体内部资源**工单**（`work_items` 表）：`id`、`title`、`origin_event_id`、`mandate_id`、`stage`、`outcome`、`ended_at`、`ended_by`、`session_id`（可空）。
- run 增加可空字段 `work_item_id`。旧 run 与用户直接发起的对话 run 不带工单，**行为零改动**。
- 一张工单可以有多个 run；同一时刻最多一个 run 在跑。每次 run 记 `attempt` 序号，**旧 attempt 的回报不能覆盖新 attempt 的状态**。
- 阶段按「它在等什么」判定，第一行优先：`person/answer`（有待人回答的问题）→ `person/review`（已交付等验收）→ `person/blocked`（失败、`result_unknown` 或受阻，且之后无人消息）→ `working/queued`（无 run 或 run 排队）→ `working`（run 在跑）→ `idle`（等下一个事件）。用户界面「待处理」只列 `person/*`。
- 工单的上下文在 run 之间传递只用「工单摘要 + 稳定 id 引用」，全文按 id 分页读（Inline / Spill），不共用整段会话。

### D6 接单记录（授权依据）：不可变，范围扩大要新工单；**不是许可**

- 新增本体内部资源**授权依据**（`mandates` 表）：`id`、`policy_version`、`strength: explicit | inferred`、`scope`（给人和模型看的描述；结构化部分只记 `matched_skills[]` 与来源接收方式）、`actor`、`source`、`basis: addressed | followup | rule`、`created_at`、`withdrawn_at`、`revoked_at`。
- 创建后不可修改。run 判断请求超出 `scope` 时交回准入（D3），需要更大范围就建新工单、新记录；旧工单结束。
- 三种变化：创建（`explicit` 事件或主动技能 `auto` 匹配）、撤回（用户主动停止）、吊销（恢复前重验策略不成立：发起者已不在名单、匹配的主动技能已关、接受模式退回 `off`）。
- **授权依据不参与工具门。** 它只回答「这件事凭什么开始、范围描述、谁要求的、当时的策略版本」，用于恢复前重验与「超范围 → 新工单」判定。run 里每次工具调用照旧只过审批门与 K1-03 许可；工单、订阅、策略版本、模型生成的计划都不是授权凭证（ADR-039 建议修订 3）。

### D7 订阅：只加上下文，不加权限

- 工单可以订阅若干讨论串（`subscriptions` 表：`work_item_id`、`thread_key`、`kind: own | context`）。订阅讨论串上的新事件直达工单作为输入，不经准入、不经研判，不改变授权依据。
- 用户在工单自己的讨论串上的后续消息，由工单的 run 决定是否在范围内；超范围的交回准入。`context` 类订阅不能唤醒已结束的工单；用户停止的工单什么也收不到。
- 订阅来的事件文本按 K1-02 §2.1.1 打来源标签，出口照旧 fail-closed。「不检查发送者」只指不做准入，不指可信。

### D8 完成条件分开：就绪 ≠ 交付 ≠ 验收

- run 报 `completed` 不结束工单。有外部交付物（发出去的消息、提交的文档、创建的日程）时，等交付记录到 `sent / confirmed`（ADR-042 D2）再进 `person/review`；用户确认或超过保留期自动结束。
- 没有外部交付物的工单，run 完成即结束。
- `result_unknown` 的动作（ADR-042）把工单置于 `person/blocked`，不自动重试。

### D9 停止、撤销、暂停与在途

- 用户停止工单：撤回授权依据；取消正在跑的 run（现有 `CancellationToken`）；按 K1-03 §3 与记忆 P1 §9④：fence 之后不再授予新的发送或工具许可，在途动作尽力取消，已出去的列为「失控副本」如实显示，不承诺撤回。停止后的工单不被任何后续事件自动唤醒。
- 全局暂停：停止派工、投递与研判，入站事件照常记录；恢复时先重验授权依据再继续（ADR-042 D1）。
- 新事件、订阅、旧审批、记忆恢复、技能回滚都**不能恢复已撤销的权限或已删除的数据**。

### D10 数据流与对外

- 入站事件的 `text` 与 `meta` 是外部文本，按 K1-02 打来源标签：Hyphae / 微信 / Nostr 来源 `Unknown`；本机 AgentEar 转写待定（开放问题）。它们只是输入，永不改变权限（`docs/laws/CONTEXT.md`）。
- 入站事件进模型上下文前经 Inline / Spill：默认只给 id、前 50 字、相邻事件 id；全文按 id 分页读。
- 用户只看到：「来源」设置页里的名单与「谁能让我做事」三档；「待处理 / 进行中 / 已完成」三组与「停止」按钮；`observe` 期间的「观察」分组。`inbound_events`、准入表、策略版本、工单、授权依据、订阅都是内部名词。
- CORE-2：`GET /api/v1/work-items` 登记新 `Operation`；停止是提升级。

## 3. 备选方案及否决理由

| 备选 | 否决理由 |
|---|---|
| 维持各来源自己判定（现状） | 六条路径六套规则；无法统计误接单 / 漏接单；无法做 `observe` |
| 把准入放进来源组件 | 违反 ADR-039 规则 1；组件可能是第三方，准入必须在本体 |
| 跨来源映射「同一个人」 | 原型明确否决；需要身份系统，归 AirAccount 线 |
| 所有事件先过模型 | 成本与隐私；原型消融显示 Router 决定数是成本主因 |
| 复用出站事件表存入站事件 | 语义不同；出站表是广播流，混用会让 WS `/events` 泄露入站内容 |
| 扩展 session 加授权字段，不加工单 | session 是渠道对话容器；原型 001 否决「Ticket state that follows the Session」理由相同 |
| 用 run 的 `parent_run_id` 串起来 | 没有「凭什么开始」，回答不了「等人」阶段 |
| 授权依据带可执行范围，作为工具门上限 | 形成第二套权限权威（Codex）；「为什么被拒」无法解释；K1-03 已是唯一许可来源 |
| 订阅事件也过准入 | 原型否决；会把名单外的人的跟进全丢掉，分身就不会「听」了 |
| 完成 = run completed | 发出去的消息对方没收到时「已完成」是谎言 |
| 照搬 Ticket / Mandate / Assignment 为产品概念 | 违反 ADR-039 §4、§5 |

## 4. 后果

- 好处：误接单 / 漏接单可度量；脱敏回放成为可能；两个桥变薄；AgentEar / 新通道只写适配器；一件事有连续身份；「凭什么做」可审计；就绪与完成分开。
- 代价：四张新表、新 trait、五处改造、准入表 UI、「待处理」视图；工单与事件进记忆 P1 purge 范围。**所有新表都在 CORE-5 5f–5g 接线之后才进 main**。
- 风险：`observe` 默认会让用户觉得「分身不干活」，界面要解释并给一键切 `auto`（提升级）；对直接聊天的用户是额外一层，要保证无工单 run 路径零改动。

## 5. 与其他 ADR 的关系

- 展开 ADR-039 规则 1 的「触发规则」；建议 ADR-039 §2.1、§4、§3 的三处补充见终稿 §6。
- ADR-038 D9 的 `events` 块见终稿 §6（r4「后续」，不阻塞 C1–C8）。
- `inferred` 候选的匹配对象是 ADR-040 D6 的主动技能。
- 交付记录、`result_unknown`、「策略已变」输入与「为工单起 run」动作来自 ADR-042。
- 撤销沿用 K1-03 §3 与记忆 P1 §9④。

## 6. 开放问题

- Q1 AgentEar 转写算 `addressed: direct` 还是单独一类；来源标签是否 `Unknown`。建议 `direct`、actor = 本机用户、标签交 K1-02 定。
- Q2 入站事件与工单保留期。建议与审计一致 180 天，受记忆 P1 purge 覆盖。
- Q3 已结束工单何时允许重开：原型允许用户在原讨论串的消息直接恢复。建议 v1 允许，但先重验授权依据。
- Q4 nostr-bridge / wechat-bridge 的名单搬进 daemon 时的行为差异；桥的内存去重在迁入前是否先落本地文件。
- Q5 工单是否暴露给组件（领域 OS 想知道「这是哪件事」）。建议 v1 不暴露，只在 K1-01 可信上下文里带 `work_item_id` 作关联。

## 7. 评审记录

- 来源：Fable 初稿 `adr-drafts/ADR-041-inbound-events-and-admission.md` 与 `adr-drafts/ADR-042-work-items-and-mandates.md`，按 Codex 的划分合并为一份（事件到工单是一条生命周期）。
- Codex 改了什么：警告授权依据不能成第二套权限权威 → D6 改为「接单记录，不参与工具门」；加「旧 attempt 回报不能覆盖新 attempt」（D5）；加「来源身份不跨平台合并」（D2）；加「人工停止不被普通后续事件解除」（D9）；加「新事件、订阅、旧审批、记忆备份、技能回滚不能恢复已撤销权限或已删数据」（D9）；点名 wechat-bridge。
- 裁判改了什么：入站路径改为五条并核实 nostr-bridge 去重在内存（§1）；D3 补「身份与权限只由确定性代码判」与「研判只能 LocalOnly」的边界，补 `ignore(no_rule)` 记录；D5 加 attempt 序号与 Inline / Spill 的上下文传递规则；D6 的 `scope` 从「允许能力列表 + 目的地列表」降为描述；D9 加全局暂停恢复前重验；新表前置 CORE-5 5f–5g 写进后果；备选表加「照搬 Ticket / Mandate 为产品概念」与「授权依据作工具门上限」两行。
