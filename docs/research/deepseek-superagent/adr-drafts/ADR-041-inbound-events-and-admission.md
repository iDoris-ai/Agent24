# ADR-041：入站事件与准入（草案）

- 状态：**提议**（2026-11-01，Fable 初稿，待 Codex 挑战与 jason 拍板）
- 上位：[ADR-039](../../../adr/ADR-039-concept-model.md) 规则 1（能力向本体发事件，由本体按触发规则、经审批决定）；[ADR-037](../../../decision.md) 决策 2（常驻 daemon）
- 相关：ADR-038 D9（组件清单）、ADR-K1-02 §2.1.1（分类 / 处理模式 / 授权三维度）、ADR-040（主动技能）、ADR-042（工单）
- 依据：[研究初稿](../SUPERAGENT-EVOLUTION-2026-11-01-fable.md) §2.2、§3.1 第 2 条；原型 `docs/001-domain.md` Source / Trigger、`docs/002-authorization.md` Gate / Admission、`docs/008-cookbook.md` §3（[SOURCES](../SOURCES.md) S3、S4、S6）

## 1. 背景

Agent24 今天有四条入站路径，各走各的：

| 路径 | 代码 | 有无稳定 id | 有无准入 |
|---|---|---|---|
| 日程触发 | `agent24-scheduler`，`ScheduleAction::AgentRun`（`agent24-protocol/src/types.rs:875`） | 有 `FireId`（投递状态机） | 用户自建，无需准入 |
| 自唤醒 | `agent24-agent/src/self_wake.rs`（一次性 schedule） | 同上 | 同上，上限 32 个 |
| Hyphae 收件箱 | `agent24-comm/src/router.rs:174` `POST /inbox/pull` | Hyphae 的 `event_id`，Agent24 只转发 | 无 |
| Nostr 桥 | `packages/nostr-bridge/src/main.ts:7-9`：轮询收件箱，把「authorized peer message」变成受审批的 run | 桥自己记，daemon 不记 | 桥内判定，规则未核实 |

接 AgentEar（语音）、微信桥之后会是六条。没有统一记录就没法回答：这条消息处理过没有；谁能让我的分身干活；observe 模式下它会做什么。

原型的做法：Source 持读位置与自己的身份，先丢自己的消息和范围外事件；Trigger 是不可变事件，id 由来源的原生 id 派生（`key(kind, native_id)`），重读安全，接受两次只建一个工作单元；准入先走确定性 gate（谁在名单、是否 @ 智能体、接收范围），再决定要不要模型研判；bot 只能产生 inferred 候选；接受模式 `off / observe / auto`。「人」不是领域概念，白名单是来源的策略部分，不同来源的身份不互相映射。

## 2. 决策

### D1 本体有一个入站事件入口，所有来源都走它

- 新增本体内部资源 **入站事件**（`inbound_events` 表）：`id`（`<source>:<native_id>[:<edit_version>]`）、`source`（组件 id 或 `scheduler` / `self`）、`thread_key`（来源定义的讨论串键，可空）、`actor`（来源原生身份，可空）、`human: bool`、`addressed: none | mention | direct`、`at`、`text`（有上限，默认 4 000 字）、`meta`（来源自有 JSON，不可信）、`disposition`。
- 同 `id` 再次写入返回 `duplicate`，不建第二条记录，不触发第二次处理。编辑过的消息要当新事件时，来源在 id 里带版本（原型 cookbook §3）。
- 四条现有路径改为适配器：scheduler 投递与 self_wake 写入 `source = scheduler|self`；Hyphae 收件箱由 daemon 主动拉取写入；nostr-bridge 改为只做传输，判定搬进 daemon。
- 入站事件不是出站事件流（`protocol/events.schema.json` 的 `run.*` 等）。两者分表、分名。

### D2 来源的职责：身份、读位置、范围、讨论串

- 作为事件源的组件在 ADR-038 D9 清单里加可选块 `events: { id_rule, cursor: true|false, threads: true|false, channels: [...] }`。
- 来源负责：持读位置（daemon 不在时不丢、重启不重放）；标明 `actor` 与 `human`；标明是否 `addressed`；先过滤自己发出的消息；把范围外事件标 `filtered` 写入（仍记录，便于解释），不建讨论串。
- 来源**不负责**准入判定。准入规则属于本体策略，组件只提供事实（ADR-039 规则 1：能力不能指定技能）。

### D3 准入：确定性门先于任何模型判断

- 每个来源有一张准入表，按**接收范围 × 发起者是否在名单 × 是否 @ 分身**决定三种结果：`explicit`（明确请求）、`inferred`（候选）、`ignore`。默认：名单内且 @ → explicit；名单内未 @ → inferred；名单外 → ignore。bot / CI 最多 inferred。
- 名单以来源原生身份记（Nostr npub、微信 id、AgentEar 的本机用户）；不跨来源映射身份。
- `explicit` 事件直接进 ADR-042 的工单创建；`inferred` 事件进主动技能的匹配（ADR-040 D5）；`ignore` 只记录 disposition。
- 模型研判只发生在 `inferred` 且存在 `observe` / `auto` 模式的主动技能时；没有主动技能就不起模型。

### D4 接受模式与策略版本

- 本体有一个全局接受模式 `off | observe | auto`，默认 `observe`；主动技能各自再有一档（ADR-040 D5），取两者更严者。
- 准入表 + 名单 + 接受模式 + 启用的主动技能集合构成**策略版本**（哈希）。每个工单的授权依据记录它接受时的策略版本（ADR-042）。
- 策略变更：正在研判的候选作废并按新版本重排；受影响的工单收到一条「策略已变」输入，不自动取消（原型 002 Policy version；与 K1-03 §3「撤销要落实到边界」不矛盾：取消写权限走审批撤销，不走策略）。

### D5 与数据流的关系

- 入站事件的 `text` 与 `meta` 是外部文本，按 K1-02 §2.1.1 打来源标签：Hyphae / 微信来源 `Unknown`，本机 AgentEar 转写待定（开放问题）。它们只是输入，永不改变权限（原型 002 开头；`docs/laws/CONTEXT.md`）。
- 入站事件进模型上下文前经 Inline / Spill 规则：默认只给 id、前 50 字、相邻事件 id；全文按 id 分页读。

### D6 对外

用户只看到「来源」设置页里的名单与「谁能让我做事」的三档，以及「待处理」列表。`inbound_events`、准入表、策略版本都是内部名词。

## 3. 备选方案及否决理由

| 备选 | 否决理由 |
|---|---|
| 维持各来源自己判定（现状） | 六条路径六套规则；无法统计误接单 / 漏接单；无法做 observe |
| 把准入放进来源组件（像原型把 Policy part 放在 SourceKind 里） | 违反 ADR-039 规则 1「能力不能指定技能」；Agent24 的组件可能是第三方，准入必须在本体 |
| 跨来源映射「同一个人」 | 原型明确否决（001 Rejected concepts）；需要身份系统，归 AirAccount 线 |
| 所有事件先过模型再决定 | 成本与隐私：本地模型每条消息起一次；原型的消融显示 Router 决定数是成本主因 |
| 复用出站事件表存入站事件 | 语义不同（出站是本体做了什么，入站是外界说了什么）；出站表是广播流，混用会让 WS `/events` 泄露入站内容（ADR-037 G-API-4） |

## 4. 后果

- 好处：误接单 / 漏接单可度量；重放脱敏样本成为可能；nostr-bridge 变薄；AgentEar / 微信接入只写适配器。
- 代价：新表、新 trait、三处改造；准入表 UI。
- 风险：`observe` 默认会让用户觉得「分身不干活」，界面要解释并给一键切 `auto`（提升级）。

## 5. 与其他 ADR 的关系

- 展开 ADR-039 规则 1 的「触发规则」；建议 ADR-039 §2.1 本体职责加「事件接入与准入」。
- 为 ADR-038 D9 增加可选 `events` 块（建议进 r4「后续」节，不阻塞 C1–C8）。
- ADR-042 消费 `explicit` 事件；ADR-040 D5 消费 `inferred` 事件。
- 策略变更不等于撤销，撤销沿用 K1-03 §3。

## 6. 开放问题

- Q1 AgentEar 转写（本机用户说话）算 `addressed: direct` 还是单独一类。建议 direct，且 actor = 本机用户。
- Q2 入站事件保留期。建议与审计一致 180 天，受记忆 P1 purge 覆盖。
- Q3 `observe` 期间的「会运行」记录放哪里给用户看。建议待处理列表的「观察」分组。
- Q4 nostr-bridge 的现有判定规则搬进 daemon 时是否有行为差异（未核实 `inbound.ts`）。
