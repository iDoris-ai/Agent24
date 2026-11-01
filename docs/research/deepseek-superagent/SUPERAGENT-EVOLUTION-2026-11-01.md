# Agent24 向 Super Agent 框架演进（最终版，2026-11-01）

> 对应 main `09cdad8f`。三步评审的最终版：Fable 初稿 → Codex gpt-6-astra 独立评估（未读初稿）→ Fable 裁判对照代码与原型整合。
> 输入：[Fable 初稿](SUPERAGENT-EVOLUTION-2026-11-01-fable.md)（含 [adr-drafts/](adr-drafts/) 五份草案）、[Codex astra 评估](SUPERAGENT-EVOLUTION-2026-11-01-codex-astra.md)。来源编号见 [SOURCES.md](SOURCES.md)。
> 裁判只做静态核查（grep / 读源码），另用 `gh api` 取回原型固定提交 `6337fc8` 的 `docs/001-domain.md`、`002-authorization.md`、`003-execution.md` 全文逐条对照。不构建、不运行。
> 名词按 [ADR-039](../../adr/ADR-039-concept-model.md)：对外只说 Agent24、能力、技能；下文的入站事件、工单、授权依据、待执行动作、后台作业都是本体内部资源，不对用户暴露。
> **最终 ADR 见 `docs/adr/ADR-040`–`ADR-043`**；`adr-drafts/` 保留为初稿原始记录。

## 1. 结论

1. **该学的是四样机制，两方一致**：入站事件有稳定 id、确定性准入先于模型；工作身份独立于 run 和 session；决定与待执行动作同一事务、外部结果对账；就绪 ≠ 交付 ≠ 验收。四样 Agent24 都只有零件（§3）。它们全是本体内部实现，不新增对外名词，不与「daemon 是本体」冲突。
2. **不学的也两方一致**：云端 Agent Loop 当权威、常驻 LLM Router 会话、默认全权限执行、监督者在线改提示词或准入规则、投机执行。Agent24 的审批 fail-closed、capability token、K1 数据流、哈希链审计比原型强，演进时不能退让。
3. **顺序采 Codex：先保护记忆与恢复，再扩自主性。** CORE-5 5a–5e 和 5f-1 已合但 `ENABLED=false`、未接线；本文每个阶段都要新增 store 迁移，在 5f–5g 把生产迁移保护接上之前，任何新表都不进 main。只读设计、trait、离线场景集可以并行（§7 S0）。Fable 的阶段切分保留，前置改掉。
4. **自进化从技能开始，服从 ADR-039 规则 3**：评测是门槛，不是「以后再加」（Codex 指出上午评估建议 5 与已接受的 ADR-039 矛盾，裁定服从 ADR-039）。训练归 iDoris，Agent24 只做经同意的数据导出。记忆版本化归记忆线，不进本套 ADR。
5. **ADR 收敛为四份**：ADR-040 技能契约、ADR-041 入站事件、准入与工单、ADR-042 待执行动作与对账、ADR-043 后台作业、并发与监督。ADR-039 与 ADR-038 需要小修订，条款列在 §6，等 jason 拍板，本次不改原文。

## 2. 分歧裁决表

| # | 项目 | Fable | Codex | 裁定 | 依据 |
|---|---|---|---|---|---|
| 1 | 工单是本体内部资源还是扩展 session；是否违反 ADR-039 §5 | 内部资源，与 run / session 同级；自请挑战 | 「用户要完成的工作」与 run 分离是最重要的新边界；但不得照搬 Ticket / Mandate 为产品概念 | **两方实质一致：内部资源，不扩展 session，不违反 §5。** ADR-039 §5 第三问「都不是 → 属于本体，单独写 ADR」正是这条路；§4 对内名词表的「资源」一行需补举例（§6）。界面只出现「待处理 / 进行中 / 已完成」，不出现「工单」 | `Session` 是按渠道的对话容器（`agent24-protocol/src/types.rs:376`，`channel: desktop\|cli\|tui\|schedule\|wechat\|nostr`），一个会话可起多件事、一件事可跨渠道；原型 001「Rejected concepts」明确否决「Ticket state that follows the DSH Session」 |
| 2 | 授权依据会不会变成第二套权限权威 | `scope` 含允许的能力 id 与外发目的地，「K1-03 只能更严」 | 警告「易制造第二套权限权威」；「模型生成的计划、事件订阅与技能版本都不是授权凭证」 | **采 Codex。授权依据是接单记录，不是许可**：它只回答「这件事凭什么开始、范围描述、发起者、策略版本」，用于恢复前重验与「超范围 → 新工单」判定；工具调用照旧只过审批与 K1-03，授权依据不参与工具门。Fable 草案里 `scope` 的「允许能力列表 / 目的地列表」降级为给模型和人看的描述 | K1-03 §1「授权是宿主签发的、逐工具且有边界的许可」；两套上限叠加会让「为什么被拒」无法解释 |
| 3 | 优先级：先记忆与恢复，还是演进线与 C1–C8 并行 | P0–P6 排在预览发布后，与 C1–C8 并行；只有 P3 前置 5f | 先完成 CORE-5 5f–5i，再扩自主性 | **采 Codex 的顺序，保留 Fable 的切分。** 每个阶段都加 store 表；迁移保护未接线时每次发版 memory 打不开仍无记忆启动，新表越多风险越大。裁定：5f–5g 是**所有新增持久表的硬前置**；5h–5i 不阻塞。离线部分（trait、适配器、场景集）可与 C1 并行 | `migration_coordinator.rs:20` `ENABLED=false`；`server.rs:521` `open_memory_base` 失败返回 `None`；`migration_guard/` 已有 5a–5e、5f-1（git log：`70f1e9a1` 5f-1 类型契约「未接路由」） |
| 4 | 群聊里「谁在跟分身说话」能否只靠确定性规则 | 确定性准入 + 订阅直达够用；模型只对 `inferred` 且有主动技能时研判 | 程序先准入，模型后研判；模型只判「值不值得做、怎么做」，身份与权限留在确定性代码 | **两方一致，补一条边界。** 身份、名单、是否 @、接收范围只由确定性代码判；「名单内未 @ 的话算不算请求」属于语义，需要模型，但只在至少有一个启用的主动技能时才起模型，否则记 `ignore` 并在 `observe` 记录里可见。研判输入是 `Unknown` 来源文本，按 K1-02 必须 LocalOnly（Guardian 形态） | 原型 002：gate 决定 strength，`inferred` 候选「normally needs a full assessment」，「A proactive accept requires at least one matching rule」；K1-02 §2.1.1 |
| 5 | 「先落库再执行」队列是否覆盖模块工具调用 | 不覆盖，只放本体自己的动作；通道发送用幂等键 | 只为明确支持幂等或对账的操作建窄例外，不能用公共重试队列架空 K1-01 | **两方可合并。** 队列只放本体自己决定的动作（`channel_send`、`run_launch`、`notify`、`deliver_input`、`schedule_fire`）。模块工具调用默认不进队列（K1-01 §2.6）。**ADR-042 就是 K1-01 §2.6 预留的「另开 ADR」**，但只开一条窄例外：组件在清单里对某个操作声明 `idempotency_key` 支持时，本体生成的动作 id 作为该键，允许恢复重投；每次重投前复验授权；未声明的操作一律 `result_unknown` → 工单 `person/blocked`。今天 Hyphae 发送走的是 daemon 内部 REST（`agent24-comm/src/router.rs:172` `/send`），不是模块工具，所以 v1 没有实际的模块工具进队列 | K1-01 §2.6「将来如需传输层去重，必须另开 ADR 定义作用域…」；原型 008 cookbook 通道用 `message.id` 作幂等键 |
| 6 | Router / Worker 双层与云端 Agent Loop 要不要学 | 都不学；可借的只有「状态可重建、进程可重启」 | 「面向人的协调与具体执行分开」值得学，「计算可分离但不必在云端」；云端权威不学 | **两方一致：不学常驻 LLM Router，不学云端权威。** 角色分离在 Agent24 里已经是工具集裁剪（plan 模式、只读 `explore` 子代理），不另起 Router 会话。「计算分离」只允许模型提供者在远端（现有模型路由），状态、授权、用户控制权留本地。多 Worker 并行见第 12 项 | S14 §7.1 直通只省 Router token 不提高质量；ADR-037 决策 1、6；`subagent.rs` 只有 `fs_read`、深度 1 |
| 7 | ADR 数量与划分 | 五份：040 技能、041 事件准入、042 工单授权、043 动作对账、044 作业监督 | 五份：040 技能、041 事件准入+持续工作、042 动作回执、043 记忆版本化、044 并行监督评测 | **四份。** 041 合并 Fable 的 041+042（Codex 的划分：事件到工单是一条生命周期）；042 = 动作与对账；043 = 作业、并发与监督，吸收 Codex 的评测要求 | 任务要求「少而精」 |
| 8 | 记忆版本化要不要单独立 ADR | 并入记忆版本化那条线，不另起 | 立 ADR-043 | **不立。** 它不是 Super Agent 特有问题，上午评估建议 4 与记忆 P1 线已覆盖。Codex 的三条要求（稳定引用与分页预算、恢复不得复活已删内容、召回入口注入 `Retriever`）写进 §7 S3 的验收，交记忆线 | 可进化性评估建议 3、4；ADR-MEMORY-P1 §6 |
| 9 | 技能评测门槛 | 草案 D4：进入「当前」指针前评测必须通过 | 指出上午评估「评测门槛以后再加」与 ADR-039 规则 3 矛盾 | **服从 ADR-039。** v1 评测用例可以只有 `exact / contains`，但「有评测且通过」是激活前置，不是可选 | ADR-039 §3 规则 3 已接受 |
| 10 | 现有入站路径数 | 四条（scheduler、self_wake、Hyphae 收件箱、nostr-bridge） | 点名 `packages/wechat-bridge/src/bridge.ts` | **五条，Fable 漏了微信桥。** 它已有 uid 名单、每用户串行队列、微信里回 y/n 解决审批 | `packages/wechat-bridge/src/{main,bridge}.ts` |
| 11 | 订阅来的事件要不要过准入 | 不过准入，但打 K1-02 来源标签 | 「订阅只增加上下文，不扩大授权；外部消息不是权限指令」 | **两方一致。** 不过准入、不加权限、按来源打 `Unknown` 标签走出口门 | 原型 001 否决「Admission checks on Subscriptions」；K1-02 §2.1.1 第 4 条 |
| 12 | 多工作者并行 | 远期，等 P6 指标证明单工作者是瓶颈 | P4「受控并行」：有界并发、每项预算、同资源写入冲突控制，在 P2 稳定后 | **采 Codex 的形态，采 Fable 的时机。** v1 并发槽固定为「1 个工单 run + 1 个人直接对话的保留槽」；放宽要真实完成率与人工介入时间的证据 | 原型 003 Concurrency「one extra slot for a person's message」；HW-TIERS |
| 13 | ADR-038 D8/D9「可替换」的充分条件 | 未提 | 同 id、契约名、major 不足以证明语义相同；要运行时绑定记录与兼容用例 | **成立，但超出本题。** 列入 §6 ADR-038 建议修订，不进 ADR-040–043 | ADR-038 §3 第 7 条只要求 mock 第二实现 |
| 14 | 验收指标 | 五项：误接单、漏接单、越范围动作、人工介入次数、成本 | 分层：准入、执行、安全、恢复、交付、人类介入成本；合成消息不能证明真实能力 | **合并。** 五项是离线可算的，分层是覆盖面；每阶段报五项，S6 起加分层 | S1 分阶段第 4 步 |

## 3. 对照现状（裁判核实后）

| 要点 | 现状 | 判定 |
|---|---|---|
| 入站事件稳定 id、去重、准入 | 五条路径各自为政。nostr-bridge：npub 名单 fail-closed，`event_id` 去重是**内存** `Set`（上限 10 000，重启即丢，`inbound.ts`）；wechat-bridge：uid 名单、每用户串行；scheduler：`FireId::derive` 稳定（`fire.rs:25`）；Hyphae 收件箱只转发；self_wake 是一次性 schedule | 部分有 |
| 工作身份独立于 run | run 是唯一工作单元；`plan_resume` 只认 `AwaitingApproval`（`resume.rs:87`），`Running / Queued` 由孤儿 sweep 收尾；Session 是渠道对话容器 | 没有 |
| 先落库后执行、对账 | 审批 store 行权威；scheduler 投递有状态机；Hyphae outbox 在 Hyphae 二进制里；daemon 自己的「发消息 / 开 run / 通知」无持久意图 | 部分有 |
| 就绪 ≠ 完成 | run `completed` 即结束，无交付与验收状态 | 没有 |
| 后台作业 | `shell_exec` 同步、预算 30 s（`local.rs:508`）；K1-01 §2.4 `pending` 句柄只有契约，`module_tool_runtime.rs` 里的 `pending` 是内部标志不是作业句柄 | 没有 |
| 监督 | 桌面 5 s 探 `/health`、3 次失败重启（ADR-037 事实）；Guardian 评工具风险、默认关；`/health` 恒 ok | 部分有 |
| 技能 | `rust/` 下 `skill` 零命中；#1037 OPEN；`docs/capabilities/REGISTRY.md` 尚不存在（C7 建表） | 没有 |
| 设置版本与冲突拒绝 | 未找到（Fable 标待核实，裁判检索无结果） | 没有 |
| 凭据不进子进程 | 工具子进程 `env_clear()` + 白名单（`env_whitelist.rs`）；Hyphae runner 同样 `env_clear()`（`runner.rs:359`）。比原型「剔除含 KEY/TOKEN 的变量」强 | 有 |
| 记忆升级保护 | 5a–5e、5f-1 已合，`ENABLED=false`，未接线 | 部分有，未接生产 |
| 安全底座 | fail-closed 审批（`agent24-policy` 头注释，ADR-026 硬约束 2）、哈希链审计、capability token；`acp.rs:157,198` 仍 `workspace_id: None` | 有，覆盖需逐路径验收 |
| 桌面与 daemon | `tryReuseExternal` 先复用；仍保留 spawn 路径；`will-quit` 调 `backendManager.stop()`（`main.ts:467`） | 服务模式存在，默认常驻未完成 |

## 4. 该学和不该学的

**学（按价值）**：① 工作身份与接单记录独立于 run / session；② 事件稳定 id + 确定性准入 + `off / observe / auto`；③ 决定与动作同事务、通道发送幂等键、对账、不声明 exactly-once；④ 就绪 / 交付 / 验收分开；⑤ 后台作业真实 id、取消、完成事件回到事件流；⑥ 监督先用确定性指标，审查器动作闭集、次数有上限；⑦ 技能版本带基线、默认更新显示差异；⑧ 有预算、带稳定引用的读接口（归记忆线）；⑨ 离线场景集作验收形式；⑩ 用有效交付与人工介入成本衡量进步。

**不学**：云端权威与端侧轻 daemon；常驻 LLM Router 与批处理；默认多 Worker 并行；agent 专用账号身份（Agent24 以用户本人名义行事，靠审批不靠账号隔离）；`danger-full-access`；监督者在线改提示词 / 规则；投机执行；换到 Harness / Cordis；「所有东西都插件化」（审批、记忆、调度归本体）。

## 5. 对两份初稿的事实更正

| 方 | 错误或不准确 | 更正 |
|---|---|---|
| Fable | 入站路径「四条」，把微信桥列为未来 | 已有五条，`packages/wechat-bridge/` 在产品路径上 |
| Fable | nostr-bridge 判定规则「未核实」 | 已核实：npub 名单 fail-closed；去重在内存，重启丢失 |
| Fable | 「设置版本与冲突拒绝 待核实」 | 检索无结果，判「没有」 |
| Fable | 「setup 剔除 KEY/TOKEN 待核实」 | Agent24 子进程 `env_clear()` + 白名单，比原型强 |
| Fable | `resume.rs:202`、`server.rs` 「2000+ 行」 | `plan_resume` 在 `resume.rs:87`；`server.rs` 8 435 行 |
| Fable | ADR-040 草案 D1 要求 `requires` 引用 `REGISTRY.md` 已登记 id，未标明该文件尚不存在 | 终稿 ADR-040 写明依赖 ADR-038 C7 |
| Codex | 「`server.rs` 部分授权仍用全局资源」 | 未给出行号，裁判未核实，标待核实 |
| Codex | 路径 `agent24-cli/src/acp.rs`（自称相对 `rust/`） | 实际在 `rust/apps/agent24-cli/src/acp.rs`，行号 157、198 正确 |
| Codex | 立 ADR-043 记忆版本化 | 不是事实错误，是范围问题；归记忆线（裁决 8） |
| 两方 | 把 CORE-5 写成「5f–5i 未做」 | 更准确：5a–5e、5f-1 已合但未接线，剩 5f-2 起到 5i |

## 6. 与 ADR-039、ADR-038 的关系（建议修订，等拍板，本次不改原文）

**ADR-039（已接受）——三处补充，不改规则**：
1. §2.1 本体职责句改为：「…负责思考（agent 循环、模型路由）、记忆、审批与审计、调度、权限令牌、能力管理，**以及事件接入与准入、工单与接单记录、待执行动作、后台作业与监督回路**。」
2. §4 内部名词表「资源」一行的含义改为：「本体管理的工作区、文件、会话，**以及入站事件、工单、授权依据、待执行动作、后台作业**（ADR-041–043）。」
3. §3 规则 1 末尾加一句（Codex 建议）：「**模型生成的计划、事件订阅、工单与技能版本都不是授权凭证**；执行权限只来自审批与 capability token（ADR-K1-03）。」

**ADR-038（草案 r3）——进 r4「后续」节，不阻塞 C1–C8**：
1. D9 清单加可选块 `events: { id_rule, cursor: bool, threads: bool, channels: [{ id, idempotency_key: bool }] }`，组件作为事件源或发送通道时声明；没有就不写（ADR-041 D2、ADR-042 D2）。
2. D9 `provides[].tools[]` 允许逐工具标 `idempotency_key: true`，作为 ADR-042 D3 窄例外的声明入口。
3. D8 补「可替换」的充分条件（Codex 裁决 13）：同能力 id + 契约名 + major 之外，要有运行时绑定记录（一次 run 固定选中的提供者与契约）和覆盖参数、输出、副作用、隐私、取消的兼容用例；执行中不得静默换提供者。
4. D5 补一句：桌面经 `report` 上报的状态是观察值，`ready` 不等于已授权，不授予工具许可。

**ADR-K1-01 §2.6**：不改；ADR-042 作为它预留的「另开 ADR」，范围只到 D3 窄例外。

## 7. 分阶段路线

与已排事项对齐：下一版预览发布后开工 ADR-038 C1–C8；CORE-5 剩余片；记忆 P1 / 版本化；训练归 iDoris。每阶段 PR ≤300 行、PR-Daemon 评审（现有约束）；**每阶段先交离线场景集，再接真实来源**；每阶段报五项指标（误接单、漏接单、越范围动作、人工介入次数、成本）。

| 阶段 | 目标 | 前置 | 交付物 | 验收 |
|---|---|---|---|---|
| **S0 保护与拍板**（现在 → 预览发布后） | 生产迁移保护接线；四份 ADR 可接受 | CORE-5 5a–5e、5f-1 已合；本文 | CORE-5 **5f-2、5g**（停靠路由、外壳识别、`ENABLED=true`）；5h、5i 按原排期；ADR-040–043 拍板；§6 修订条款拍板 | CORE-5 T4/T5/T6/T13/T14；双库失败注入不再无记忆启动。**此后才允许新增 store 迁移** |
| **S1 入站事件与准入** | 五条路径走同一入口；稳定 id、去重、名单、`observe` | ADR-041；S0（持久表）；离线部分可与 C1 并行 | `InboundSource` trait 与五个适配器（scheduler / self_wake / Hyphae 收件箱 / nostr-bridge / wechat-bridge 先变薄）；`inbound_events` 表；准入表（按来源）；全局接受模式；`observe` 记录 | 离线场景：重开库后同 id 为 `duplicate`；名单外不进模型；`observe` 零外部动作；无主动技能时 `inferred` 不起模型。指标：误接单 / 漏接单可由 `disposition` 统计 |
| **S2 工单与接单记录** | 一件事跨多个 run；就绪 ≠ 完成；订阅只加上下文 | ADR-041 D5–D9；S1；C3（`GET /api/v1/components` 的 Operation 登记样板） | `work_items`、`mandates`、`subscriptions`；run 加可空 `work_item_id`；`GET /api/v1/work-items`（新 `Operation`）；桌面 / TUI「待处理」 | 离线场景：待派工重开后继续原工单；订阅直达且接单记录不变；run 成功不自动验收；旧 run 回报不能覆盖新尝试；策略变更投输入不吊销；无工单的 run 路径零改动 |
| **S3 待执行动作与对账** | 决定与动作同事务；通道幂等键；`result_unknown` 可见 | ADR-042；S2；C2（Hyphae `contract` / `health`，relay 上线后）；记忆线：稳定引用与分页预算、`Retriever` 注入（Codex 043 要求，归记忆线） | `pending_actions`、`channel_receipts`；Hyphae 发送改走队列（`message_id` ↔ `event_id`）；scheduler 投递状态机迁入；处理器 `Later` | 离线场景：收据与动作一起回滚；两次尝试一个逻辑结果；中断 / 重启 / 响应丢失三类注入（CORE-5 T9 方法）；重投前复验授权；未声明幂等的通道第一次失败即 `result_unknown`。指标：重试与外部写入次数 |
| **S4 后台作业、并发与监督** | `shell_exec` 后台化；并发槽 1+1；确定性指标进 `/health`；审查器闭集 | ADR-043；S3；CORE-5 5f 的 health 契约 | `jobs` 表与 `job.*` 事件；`job_wait / job_output / job_kill`；30 分钟上限；`/health.work`；LocalOnly 审查器（默认关） | 离线场景：作业完成唤醒原 run 或工单下一 run；超时杀作业并记录；审查器越权动作被拒、每日次数上限生效；暂停后不自动恢复；一项工作阻塞不拖死人直接对话。指标：人工介入次数、成本 |
| **S5 技能契约落地**（可与 S2 起并行） | 技能可装、版本、评测、审批、回滚；主动技能 `observe → auto` | ADR-040；#1037；C7（`REGISTRY.md`）；S1（主动技能的事件来源） | 加载器（三来源）；清单校验；版本表；评测跑器（`exact / contains`）；安装 / 激活走审批；「默认更新」差异；第一个真实技能（周报，零外部写） | 无版本拒装；评测不过拒激活；回滚恢复上一版且不恢复已撤销权限；旧 run 固定旧版本；`observe` 零外部动作；组件附带技能独立记录与审批 |
| **S6 真实来源与度量** | 接真实 Hyphae / 微信 / AgentEar 事件；分层评测 | S1–S5；Hyphae relay 上线；C2、C4 | 脱敏事件回放工具；指标面板；按 S1 五步逐级放开（先回放只记录，再读 + 草稿，再逐种写操作对账，再真实模型评估，再放开来源与并发） | 每步报五项指标 + 分层（准入 / 执行 / 安全 / 恢复 / 交付 / 人工成本）；合成消息结果不写成真实能力 |
| **远期，不排期** | 多工作者并行；语义 Router；跨月记忆；训练数据导出 | S6 指标证明单工作者是瓶颈 | — | 训练归 iDoris；Agent24 默认无数据上送 |

**不动的东西**：审批 fail-closed、capability token、K1 数据流、记忆 P1 线、CORE-5 线、ADR-038 C1–C8 顺序。

## 8. 开放问题

1. 工单、入站事件、收据、作业输出的保留期与记忆 P1 purge 的粒度（P1 §9① 首版只做全域）。
2. AgentEar 转写（本机用户说话）的 `addressed` 与来源标签：建议 `direct`、actor = 本机用户、标签待 K1-02 定。
3. `deliver_input`（向在跑的 run 投输入）要求 run 中途可收输入；现在 run 是一次 prompt。可能先做 ADR-043 的作业再回答。
4. `background_after_ms` 对本地模型推理是否合适（模型本身慢）；`shell_exec` 与模型调用可能要分别设。
5. Codex「`server.rs` 部分授权仍用全局资源」待核实行号。
6. nostr-bridge 的内存去重在 S1 迁入 daemon 后才持久；迁入前是否先落 bridge 本地文件，由 Hyphae relay 发版闸一起定。
7. 原型 `store.ts / effects.ts` 的实际行为与 DeepSeek 原文逐字内容仍未读（两方同）。
