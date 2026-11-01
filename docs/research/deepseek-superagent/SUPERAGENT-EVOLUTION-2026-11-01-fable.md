# Agent24 向 Super Agent 框架演进：初稿（Fable，2026-11-01）

> 三步评审的第一步：Fable 初稿 → Codex（gpt-6-astra）独立评估 → Fable 裁判整合。
> 对应 main `09cdad8f`（本工作树基线）。只做静态核查（grep / 读文件），不构建、不运行。
> 来源编号 S1–S20 见 [SOURCES.md](SOURCES.md)。名词按 ADR-039：对外只说 Agent24、能力、技能；对内用组件、运行形态、工具、资源。
> 本文把「入站事件」「工单」「授权依据」「待执行动作」「后台作业」当作本体内部的**资源种类**，与现有的 run、session、schedule 同级，不是新的顶层概念，也不对用户暴露。这是本文的一个假设，列为开放问题 Q1 交 Codex 挑战。

## 0. 结论

1. **DeepSeek「Super Agent」的核心不是多开 Worker，是四样东西：事件有稳定 id、授权依据独立于会话、决定先落库再执行、完成条件分开判。** 这四样 Agent24 一样都没有完整的（§2）。它们和 Agent24 本地优先、daemon 是本体的产品形态不冲突，应该学。
2. **双层 Router/Worker 和云端 Agent Loop 不该照搬。** 前者是团队规模的优化（原文自己的数据：直通省 37% token，但不提高完成质量；S14 §7.1），个人数字分身的事件量用确定性准入加订阅就够；后者与 ADR-037「daemon 是本体」和「不提取用户数据」直接冲突（§3.2）。
3. **Agent24 已有的东西比原型强的是安全底座**：fail-closed 审批、Guardian 本地风险门、capability token、哈希链审计、K1 数据流。原型的 Worker 以 `danger-full-access` 跑（S5）。演进时这些不能退让，Super Agent 的「主动」必须压在审批之下。
4. **自进化从技能开始，用原型的两条机制**：版本带基线修订、默认更新时显示差异（S2 `docs/004-prompts.md` Versions）；主动行为先 `observe` 再 `auto`（S4 Accept mode）。这正好填 ADR-039 规则 3 和 ADR-040 的空。
5. **和 ADR-039 / ADR-038 的关系是补充，不是修订**：ADR-039 §2.1 的本体职责要加「事件接入与准入、工单、后台作业、监督回路」；ADR-038 D9 清单要加可选的「事件源」声明。起草了 5 份 ADR 草案（ADR-040、041、042、043、044），见 `adr-drafts/`。

## 1. 来源有三层，地位不同

| 层 | 来源 | 可信度 | 用法 |
|---|---|---|---|
| 原文设计 | DeepSeek《Super-agent 设计探索》，经转载 S14 的二手摘要 | 中：WebFetch 模型摘要，非逐字；原文未取到 | 只引架构思想与实验数字 |
| 公开原型 | `dsh-superagent-prototype` S2，docs/000–008 已读完，源码只看目录树 | 高（文档）/ 中（行为靠 007 描述） | 引具体机制与反例 |
| 博客建议 | 文章 S1 新增的工程建议与离线练习 S15 | 高（作者明确区分了三层） | 引工程落地路径与验收指标 |

原型 README 自述：研究代码，与内部版本及文章描述不一致，部分设想未实现。本文凡引原型机制，都注明是「原型已实现」还是「文章建议」。

## 2. Super Agent 框架要点，逐项对照 Agent24

判定标准：**有** = 在默认产品路径上存在同等机制；**部分有** = 有零件或在某条线上有，没统一；**没有** = 代码里找不到，或只有设计。每条给依据。

### 2.1 分层

| 要点 | 框架说法 | Agent24 现状 | 判定 |
|---|---|---|---|
| Router / Worker 双层 | 常驻 Router 只路由不执行，Worker 隔离执行，向 Router 汇报（S14 §2；原型 main Session / Executor Session，S2 003） | 单个 agent loop（`rust/crates/agent24-agent/src/lib.rs` `RunManager`）；唯一的子 agent 是只读、深度 1 的 `explore`（`subagent.rs` 文件头：只有 `fs_read`，无递归） | 没有 |
| Engine 是唯一写入口，端口化 | Engine 同步、每命令一事务、端口在事务内 enqueue、适配器提交后执行（S2 007 Engine） | store 是权威（`agent24-store`，sqlx 事务）；审批 broker「store 行为权威，内存通道只唤醒」（`agent24-policy/src/lib.rs` 头注释）；但没有一个「领域对象唯一写者」的 Engine，run / schedule / approval 各自写 | 部分有 |
| 组合根无领域规则 | Runtime 只组装（S2 007 Runtime） | `agent24d` 是组合根（ARCHITECTURE-LAYERS L5），同时含大量路由与业务（`server.rs` 2000+ 行） | 部分有 |

### 2.2 组件（领域对象）

| 对象 | 框架定义（S3） | Agent24 对应 | 判定 |
|---|---|---|---|
| Source | 事件流；持读位置和 bot 身份；先丢自己的消息和范围外事件 | 四个事件来源各走各的：scheduler（`ScheduleAction::AgentRun`，`agent24-protocol/src/types.rs:875`）、`self_wake`（`agent24-agent/src/self_wake.rs`）、Hyphae 收件箱（`agent24-comm/src/router.rs:174` `/inbox/pull`）、nostr-bridge 轮询后起 run（`packages/nostr-bridge/src/main.ts:7-9`）。没有统一的来源接口与读位置 | 部分有 |
| Trigger | 不可变事件，稳定 id；接受两次只建一个 Ticket | 没有入站事件记录；`protocol/events.schema.json` 的 `run.*`、`tool.*`、`schedule.*` 是**出站**事件流 | 没有 |
| Thread | 同一讨论的事件序列；每 Thread 同时只跑一个 Eval | session 是对话容器，不绑定外部讨论串；Hyphae 群聊有 `groupchat.rs` 但未与 run 关联（待核实） | 没有 |
| Policy / Eval | 版本化规则 + 各来源白名单；两层：确定性 gate → 模型研判 | 对 API 调用方：capability token（`Operation` 22 种，ADR-037 决策 3）；对模块工具：K1-03 逐工具授权；对入站消息：nostr-bridge 自称「authorized peer message」，规则未核实。没有策略版本号，没有「观察模式」 | 部分有 |
| Mandate | 不可变授权依据：策略版本、范围、强度、发起者身份；范围扩大 = 新 Ticket | capability token claims（workspace / attachment / principal、TTL、代际）；K1-03 §1 许可绑定模块、工具、范围、用途、目的地、策略版本、到期。这些是「谁可以调什么」，不是「这项工作凭什么开始、范围多大」 | 部分有 |
| Ticket / Assignment | 工作单元独立于执行会话；一张工单多次派工 | run 是唯一的工作单元（`RunStatus` queued→running⇄awaiting_approval→completed/failed/cancelled，`types.rs:411`）；一个 run 一次执行，没有跨 run 的工单 | 没有 |
| Subscription | Ticket ↔ Thread 多对多；扇出不加权限 | 没有 | 没有 |

### 2.3 运行循环

| 要点 | 框架 | Agent24 | 判定 |
|---|---|---|---|
| 两秒一轮的 pass + 持久 effects 队列 | S2 007 Passes / Effects：kind/key 一行、指数退避、`Later` 推迟 | scheduler `tick()` 每轮重读 store（`self_wake.rs` 头注释）；模块投递有状态机 `agent24_scheduler::deliveries::apply_outcome` + `FireOutcome`（`agent24d/src/scheduler_deliver.rs`）。只覆盖调度投递，不覆盖通用动作 | 部分有 |
| 先存后做 | 决定和待执行动作同一事务，提交后再调外部（S1 §Outbox） | 审批：store 行权威；记忆：`remember_idempotence` 测试；模块工具：K1-01 §2.6 不自动重试、`result_unknown`。但「发消息」「开 run」这类外部动作没有统一的先落库队列 | 部分有 |
| Outbox + 幂等发送 | 消息 id 作幂等键；GitHub 用 HTML 标记对账（S1、S6 §5） | Hyphae 有 outbox（`/outbox`、`/outbox/{event_id}/retry`、`/outbox/clear`），由 Hyphae 二进制维护，Agent24 只转发；daemon 自己没有发送收据 | 部分有 |
| 完成条件分开判 | ready ≠ PR 合并；分别等待交付与验收（S1 故障表第 5 行） | run `completed` 即结束；没有「交付后等验收」状态 | 没有 |
| 重启恢复 | 运行中的 Assignment 带恢复说明重启，先核对再重复外部动作（S2 003 Restart） | H3 durable resume：审批停靠，重启时重新验证（`agent24-agent/src/resume.rs:202`）；shutdown 排空（SHUT 设计） | 部分有 |
| 暂停处理 | 停派工与投递，Source 继续记录（S2 003 Pause） | 没有全局暂停；个人记忆有暂停开关 | 没有 |

### 2.4 记忆与上下文

| 要点 | 框架 | Agent24 | 判定 |
|---|---|---|---|
| Inline / Spill / 链式召回 | 对象有稳定 id，窗口内留引用，按需逐跳读（S14 §3.3） | 召回有预算（`session_memory.rs`，可进化性评估）；工具结果上限 64 KiB（K1-01）；`explore` 只回最终文本。没有「按 id 分页读资源」的工具 | 部分有 |
| 有预算的读接口 | 文本页 4000/8000，列表 20/50（S2 007） | 没有统一读接口 | 没有 |
| 长程记忆 | 原文列为开放问题 8.1 | M-D 记忆、生命周期 ADR、P1 线进行中 | Agent24 更深，但方向不同 |

### 2.5 工具与技能

| 要点 | 框架 | Agent24 | 判定 |
|---|---|---|---|
| 角色工具集由宿主裁剪 | 主 Session 14 个应用工具、Executor 6 个，Executor 无「问人 / 改配置」工具（S2 007 模型面） | 工具流水线 normalize → 白名单 → 审批门 → 超时（ARCHITECTURE-LAYERS L3）；plan 模式只通告 Read 类工具（`RunMode::Plan`） | 有 |
| 技能（SKILL.md） | 原型带 `skills/dsh-superagent-{github,feishu}/SKILL.md`，团队可加 skill（S2 004） | 没有加载器（#1037 OPEN，`rust/` 不区分大小写检索 `skill` 命中 0）；`SPEC-SKILL-DISTRIBUTION.md` 设计定稿未实现 | 没有 |
| 提示词版本化 | 每次保存不可变版本，带基线修订、前驱、来源；默认更新显示差异（S2 004 Versions） | 没有（可进化性评估问题 5） | 没有 |
| 设置版本与冲突拒绝 | `PUT settings/<block>` 带 `revision`，409 过期（S5 管理 API） | 没有核实到同类机制（待核实） | 待核实 |

### 2.6 多 agent

| 要点 | 框架 | Agent24 | 判定 |
|---|---|---|---|
| Worker 之间不直接通信，经订阅看到彼此 | S14 §2.2、§3.2 | 能力之间不直接调用（ADR-039 规则 1）；没有多个并行工作者 | 没有 |
| Worker 内可再建 subagent | S14 §2.2；原型 Executor 用 Harness `standard` 预设的 subagent | `explore` 只读子 run，深度 1 | 部分有 |
| 并发槽与人的消息优先 | worker slot + 一个额外槽给人（S2 003 Concurrency） | 没有并发槽概念；硬件分档（HW-TIERS）管模型内存，不管 run 并发 | 没有 |

### 2.7 自进化

| 要点 | 框架 | Agent24 | 判定 |
|---|---|---|---|
| Watchdog 在线改提示词、改触发规则 | 原文 §6（文章建议，原型未实现） | 无。ADR-039 规则 3 禁止静默改技能 | 没有，且有约束 |
| 主动行为 off / observe / auto | 原型 002：默认 observe，记录「会接受」但不做 | 没有 | 没有 |
| 模型训练 | 原文 8.1 提到针对 Router 的专项训练 | 归 iDoris；Agent24 只做经同意的数据导出（评估建议 6） | 不在 Agent24 |

### 2.8 评测

| 要点 | 框架 | Agent24 | 判定 |
|---|---|---|---|
| 消融实验与指标 | p50/p95 完成时间、token、Router 决定数（S14 §7） | `eval/decide/` 三个 jsonl（recall_gate、retain_intent、tool_risk），未接进改动流程 | 部分有 |
| 验收指标 | 误接单、漏接单、越范围动作、人工介入次数、成本（S1 分阶段） | 没有 | 没有 |
| 离线练习 | 用真实 Engine + 替身跑 7 个场景（S15） | 审批 / 恢复有测试；没有「事件→决定→动作」的端到端离线场景 | 部分有 |

### 2.9 安全

| 要点 | 框架 | Agent24 | 判定 |
|---|---|---|---|
| 三层控制：来源身份 / 策略 / 工具边界 | S1 授权节 | capability token（路由级）；K1-03 逐工具授权；审批门 fail-closed（`agent24-policy` 头注释）；Guardian LocalOnly 风险门（`guardian.rs`）；K1-02 三维度（分类 / 处理模式 / 授权，§2.1.1） | 有，且更细 |
| 撤销落实到工具边界 | 文章建议 | K1-03 §3：撤销串行化，在途调用取消，每个外发边界前复验；记忆 P1 §9④ fence 后不再授予 | 有 |
| 执行隔离 | 原型 `danger-full-access`，不是沙箱（S5） | workspace-bound run 钉 cwd，不是 OS 沙箱（ARCHITECTURE-LAYERS §4）；审批覆盖所有工具 | 不同路线：审批而非账号隔离 |
| 外部文本只是数据 | 004 优先级第 5 级 | K1-01 §2.3：模块输出是不可信工具输出；`docs/laws/CONTEXT.md` | 有 |
| 凭据不进上下文 | 原文 8.4 开放问题 | 秘密只走 stdin（ADR-038 D4）；setup 剔除含 KEY/TOKEN 的环境变量是原型做法，Agent24 待核实 | 部分有 |
| 审计 | 原型：Decision 带 reason 码；无哈希链 | 哈希链 `audit_log`（`agent24-store/src/audit.rs:361 append_audit`） | 有 |

### 2.10 工程实践

| 要点 | 框架 | Agent24 | 判定 |
|---|---|---|---|
| 健康面板一写者、只在人需介入时报错 | HealthBoard，连续三次失败才报（S2 007 Effects） | `/health` 恒 ok（CORE-5 V4）；CORE-5 5f 要加 `migration_failed` | 部分有 |
| 面板只显示需要人的 | 005 三个问题 | 审批列表、TUI Runs/Approvals | 部分有 |
| 单实例锁 + 状态目录 | Runtime 锁目录 | `daemon.singleton.lock`、`daemon.json`（ADR-037 事实 §1.1） | 有 |
| 升级前备份、新版本库旧版本打不开 | 006 Upgrade | CORE-5 快照 + 整体回滚设计，`ENABLED=false` | 部分有（设计更强，未接线） |
| 旧记录不删、无限增长 | 007 Persistence 自述 | 记忆 P1 生命周期与 purge ADR | Agent24 更前 |
| 后台作业：5 秒转后台、30 分钟上限、真实作业 id | 003 Background work | `shell_exec` 同步；K1-01 §2.4 定义了 `pending` 作业句柄的**契约**，未见实现 | 没有 |
| 不等待的 Router：拒绝 `sleep`、长超时 | 007 Presets | 不适用（无 Router） | — |

## 3. 该学的，不适合的

### 3.1 该学（按价值排序）

1. **工作单元与授权依据分离于会话（Ticket + Mandate）。** 理由：Agent24 的 run 是一次执行；用户在微信 / Nostr 里说「帮我把合同整理好发给同事」，中间要等人确认、要等对方回复、要跨三个 run。今天没有东西把这串 run 连成一件事，也没有东西记录「这件事凭什么开始、能做到哪」。原型的 Mandate 不可变、范围扩大要新授权（S3），和 K1-03「扩展部分须重新同意」是同一条规则，放到工作单元上即可。→ ADR-042。
2. **入站事件有稳定 id、先去重、确定性准入先于模型判断。** 理由：Agent24 已有四条入站路径各走各的（§2.2 Source 行）；接 AgentEar 语音、微信后会是六条。原型 cookbook 的「Trigger id 经 `key` 派生，重读安全」（S6 §3）是最便宜的可靠性。准入表（范围 × 是否在名单 × 是否 @）直接对应「个人数字分身」要回答的问题：谁能让我的分身干活。→ ADR-041。
3. **先落库后执行的待执行动作，通道发送带幂等键。** 理由：Hyphae outbox 已经在做一半（§2.3）。统一后，scheduler 投递状态机、Hyphae 发送、将来的微信发送走同一个队列，CORE-5 的掉电验收方法可以复用。原型的坦白要保留：不声明跨系统 exactly-once（S1）。→ ADR-043。
4. **后台作业有真实 id、取消、完成事件。** 理由：K1-01 §2.4 已写了 `pending` 句柄契约；`self_wake` 已证明「完成后作为新事件进入会话」可行（它用一次性 schedule 实现）。缺的是 `shell_exec` 的后台化与 30 分钟上限。→ ADR-044。
5. **监督回路先用确定性指标，再用独立审查器，审查器权限有界。** 理由：Guardian 已是「LocalOnly 小模型给风险打分、只认明确 low」的样板（`guardian.rs`）；同样的形态可做进度审查：输入是队列最老等待、重试次数、最近可验证产物、预算（S1 Watchdog 节），输出只能是「暂停 / 重试 / 通知人」，不能改提示词。→ ADR-044。
6. **技能版本带基线修订、默认更新显示差异、主动技能先 observe。** 理由：直接满足 ADR-039 规则 3「有版本、通过评测、经过审批、可回滚」；原型的 `off / observe / auto` 是最便宜的「自进化不越权」做法。→ ADR-040。
7. **完成条件分开：就绪 ≠ 完成。** 理由：Agent24 的交付物常在外部（发出去的消息、提交的文档）；「发了」和「对方收到并确认」要分开记。→ 并入 ADR-042。
8. **验收指标五项**：误接单、漏接单、越范围动作、人工介入次数、成本（S1 分阶段第 4 步）。理由：这是能离线算的指标；Agent24 现在只有 decide 的三个 jsonl。→ 路线图阶段 1 起每阶段都报。
9. **有预算、带稳定引用的读接口。** 理由：记忆版本化（评估建议 4）要给对象稳定 id；有了 id，模型按 id 分页读就是原型 `orchestrator_read` 的形状。→ 并入记忆版本化那条线，本文不另起 ADR。
10. **离线练习作为验收形式。** 理由：原型七个场景（S15）全是「决定 → 落库 → 重开 → 不重复」类断言，Agent24 的 CORE-5 T9 掉电重放是同一类。每个阶段交付一份这样的离线场景集。

### 3.2 不适合（写明理由）

| 不学的 | 理由 | 来源 |
|---|---|---|
| **云端 Agent Loop + 端侧轻 daemon**（原文 §5） | 与 ADR-037 决策 1「agent24d 是产品本体」和 Mycelium「不提取用户数据」直接冲突；原文 8.4 自己指出云端被入侵会波及所有端侧设备、端侧凭据进入云端上下文有泄露风险。Agent24 的跨设备只做远程控制（ADR-037 决策 6，移动端不直连）。**可借的只有一点**：状态持久化到可重建，进程可重启（H3 已有） | S14 §5、§8.4；ADR-037 |
| **常驻 LLM Router Session 做批处理**（原文 §2.1、§7.2） | 为团队级事件速率（每分钟 1–40 条 Trigger）设计；个人分身事件稀疏，常驻模型会话在本地硬件上占内存（HW-TIERS），而且原文自证直通机制不提高质量和速度，只省 Router token（S14 §7.1）。Agent24 用确定性准入 + 订阅直达，语义研判只对主动候选按需起 run | S14 §7.1 |
| **多 Worker 并行执行** | 本地硬件一次只跑得了一个像样的模型（HW-TIERS 分档）；原文自己提醒「只看开了多少 Worker 容易把高并发当成高产出」（S1）。并发先限于「一个执行 run + 若干等待中的工单」 | S1 分阶段节 |
| **Agent 专用 GitHub 账号 / 飞书 bot 身份** | 原型是团队的自动成员；Agent24 是用户本人的数字分身，身份是用户自己的（将来 AirAccount）。发出去的话要以用户名义，所以审批而不是账号隔离 | S5 Accounts；PROFILE.md |
| **Worker 以 `danger-full-access` 无审批执行** | 违反 ADR-026 硬约束 2（fail-closed）；Agent24 的「主动」只能走审批 / Guardian。原型自己也说这不是安全边界 | S5 Permissions |
| **Watchdog 在线改提示词、改触发规则**（原文 §6） | 违反 ADR-039 规则 3「系统不得静默修改技能」。只允许审查器产出**提案**进审批 | S14 §6 |
| **换到 DeepSeek Harness / Cordis 插件框架** | Agent24 是 Rust daemon，组件契约已由 ADR-038 定；Cordis 是 TS 元框架，API 自述不稳定（S10）。**可借的思想**：注册可逆、依赖就绪才启动（S12），ADR-038 D5 状态机已覆盖 | S10、S12 |
| **面板作为 Harness Web 插件** | Agent24 有桌面 / TUI，能力中心两栏（ADR-039 §6） | — |
| **英文提示词由团队编辑** | Agent24 三语（中英泰）；技能的说明与步骤要三语（ADR-038 D9 已这样要求显示名） | — |
| **投机执行（原文 §3.4）** | 用户的本地算力和电池是成本；而且「静默复现并修复」类行为要用到写工具，必过审批。先不做；若做，只做读类投机（备好草稿等确认），且进 observe 模式 | S14 §3.4 |

## 4. 与 ADR-039 / ADR-038 的关系

### 4.1 互相印证

| ADR-039 / 038 条款 | 原型对应 | 说明 |
|---|---|---|
| ADR-039 规则 2「本体不依赖任何能力就能启动」 | 000「概念少、设置少」；Source 是插件，核心不依赖具体 Source | 一致 |
| ADR-039 规则 1「能力可以向本体发事件或提请求，由本体按触发规则、经审批决定是否运行技能」 | Source → Trigger → Gate → Eval → Ticket | 这句话就是事件接入与准入的定义；ADR-041 把它展开 |
| ADR-039 规则 3「能力稳定可靠，技能越用越好」 | 004 优先级：工具与宿主强制的事实 > 提示词文本；提示词可编辑、有版本 | 一致：能力（代码）是硬边界，技能（文本）可进化 |
| ADR-038 D5 生命周期状态机、D6 错误三元组 | 007 HealthBoard 一写者；`UserError` 闭集码；`CliError.code` | 一致 |
| ADR-038 D9 `provides` / `requires` | SourceKind 声明 `health`、`environment.variables`、`policy` | 一致，但原型多一项「事件源」声明 |
| K1-01 §2.6「不自动重试、不把 tool_call_id 当幂等键」 | 原型对 Channel 发送**用** message id 作幂等键 | 不矛盾：模块工具调用与通道发送是两类动作，ADR-043 分开规定 |
| K1-03 §1「扩展部分须重新同意」 | Mandate 不可变，范围更宽 = 新 Ticket | 同一规则，作用对象不同 |

### 4.2 要补充的（不改已接受条款）

1. **ADR-039 §2.1 本体职责列表**现在是「思考、记忆、审批与审计、调度、权限令牌、能力管理」。要补四项：**事件接入与准入、工单与授权依据、后台作业、监督回路**。这是补充不是修订：它们都不是能力也不是技能，按 §5 第三问归本体。建议在 ADR-041 被接受时顺手改 §2.1 一句话。
2. **ADR-039 §4 内部名词表**要加一行「资源」的子类举例：工作区、文件、会话、**入站事件、工单、待执行动作、后台作业**。不新增顶层名词。
3. **ADR-038 D9 清单**加一个可选块 `events`：组件作为事件源时声明 `{ source_kind, id_rule, cursor, channels[] }`；没有的组件不写。这让 Hyphae、AgentEar、微信桥在 ADR-041 下用同一个入口，不改 v1 的 C1–C8 顺序。建议写进 ADR-038 r4 的「后续」节，不阻塞 r3。
4. **ADR-040（技能契约）**除 ADR-039 §6 列的清单格式、依赖声明、版本、评测、审批、回滚外，要加：基线修订与「默认更新」差异、主动技能的 `off / observe / auto`、三来源优先级（沿用 SPEC-SKILL-DISTRIBUTION）。本文直接起草了 ADR-040 草案。
5. **ADR-037 决策 2「每用户一个常驻 daemon」**是 Super Agent「持久」特征的前提。原型用 Harness profile + tmux + 小时检查实现常驻（S5）；Agent24 用 LaunchAgent / systemd --user / 计划任务。不冲突，但 ADR-041 的事件接入要求 daemon 不在时事件不丢：来源要持读位置（原型 Source 规则），这条写进 ADR-041。

### 4.3 需要注意的冲突

- **K1-02 §2.1.1 第 4 条「缺少证据不是放行」** 与原型「订阅扇出不检查发送者」看似冲突。不冲突：扇出的是**上下文**，K1-02 管的是**出口**；订阅来的消息按来源打 `Unknown` 标签，照 K1-02 走。ADR-042 明写。
- **ADR-039 规则 1「能力不能指定技能」** 与原型「Source 的 Policy 部分决定准入」：准入规则属于本体（策略），不属于来源组件；组件只提供身份与范围事实。ADR-041 明写。

## 5. 演进路线

对齐已排事项：下一版预览发布后开工 ADR-038 C1–C8；ADR-040 待写；CORE-5 5f–5i；记忆版本化；自进化从技能开始；训练归 iDoris。本文的阶段**不插队**，都排在预览发布之后，与 C1–C8 并行或其后。每阶段先做离线场景集，再接真实来源。

| 阶段 | 目标 | 前置 | 交付物 | 验收 |
|---|---|---|---|---|
| **P0 文档与拍板**（现在 → 预览发布） | ADR-040–044 评审到可接受 | 本文三步评审 | 5 份 ADR 终稿；ADR-039 §2.1 / §4 补充一句；ADR-038 r4 后续节 | jason 拍板；Codex 挑战点关闭 |
| **P1 事件接入与准入**（预览发布后，与 C1 并行） | 四条入站路径走同一入口；事件有稳定 id、去重、来源白名单；`observe` 模式 | ADR-041；Hyphae relay 发版闸（只影响真实来源接入，不影响离线） | store 新表 `inbound_events`（id、source、thread_key、actor、addressed、at、disposition）；`InboundSource` trait 与 scheduler / self_wake / Hyphae 收件箱 / nostr-bridge 四个适配；准入表（按来源：名单内 × 是否 @）；`observe` 只记录决定 | 离线场景：重开库后同 id 为 duplicate；名单外发起者不进模型；observe 下零外部动作。指标：误接单 / 漏接单可从 `disposition` 统计 |
| **P2 工单与授权依据**（P1 后，与 C2–C3 并行） | 一件事跨多个 run；授权依据不可变；订阅只加上下文；就绪 ≠ 完成 | ADR-042；P1 | store `work_items`、`mandates`、`subscriptions`；run 带 `work_item_id`（可空，旧 run 不变）；`GET /api/v1/work-items`（CORE-2 `Operation` 登记）；桌面 / TUI「待处理」只列 `person/*` 阶段 | 离线场景：待派工重开后继续原工单不另建；订阅跟进上下文直达、授权依据不变；就绪与完成分开；策略变更通知工单不吊销。指标：越范围动作次数 |
| **P3 待执行动作与对账**（P2 后） | 决定与动作同一事务；通道发送带幂等键；结果未知可见 | ADR-043；CORE-5 5f（health 契约）合入 | store `pending_actions`（kind、key、payload、attempts、due）；Hyphae 发送改走它，幂等键 = 动作 id 映射 Hyphae `event_id`；scheduler 投递状态机迁入；`result_unknown` 进工单阶段 | 离线场景：收据与动作一起回滚；两次尝试一个逻辑结果；中断 / 重启 / 响应丢失三类注入。指标：重试与外部写入次数 |
| **P4 后台作业与监督回路**（P3 后） | `shell_exec` 后台化；作业 id / 取消 / 完成事件；确定性指标进 `/health`；审查器只能暂停 / 重试 / 通知 | ADR-044；K1-01 §2.4 `pending` 句柄 | `jobs` 表与 `job.started/completed/killed` 事件；30 分钟上限；`/health` 加 `work`（最老等待、重试、最近产物、预算）；LocalOnly 进度审查器（Guardian 形态）；审查器动作计数上限 | 离线场景：作业完成唤醒原 run；超时杀作业并记录；审查器越权动作被拒。指标：人工介入次数、成本 |
| **P5 技能自进化**（可与 P2 并行，依赖 ADR-040、#1037） | 技能可装、可版本、可评测、可审批、可回滚；主动技能 observe → auto | ADR-040；ADR-038 D9 能力 id 登记表 | Skill 加载器（三来源）；清单校验；评测跑 `eval/skills/<id>/*.jsonl`；安装 / 升级走审批；「默认更新」差异展示；第一个真实技能（建议：周报，零外部写） | 离线场景：无版本拒装；评测不过拒升；回滚恢复上一版本；observe 下零外部动作。指标：技能评测通过率 |
| **P6 真实来源接入与度量**（P1–P5 后） | 接真实 Hyphae / 微信 / AgentEar 事件；按五项指标评估 | Hyphae relay 上线；ADR-038 C2 | 脱敏事件样本回放工具；指标面板 | 按 S1 分阶段第 1–4 步逐步放开；每步报五项指标 |
| **远期，不排期** | 多工作者并行；语义 Router；跨月记忆；训练数据导出 | 硬件分档与证据 | — | 只有 P6 指标证明单工作者是瓶颈时再议；训练归 iDoris |

**不动的东西**：审批 fail-closed、capability token、K1 数据流、记忆 P1 线、CORE-5 线。每阶段 PR 按 ≤300 行分片、PR-Daemon 评审（现有约束）。

## 6. 交给 Codex 最该挑战的三个点

1. **「工单」是不是新概念？**本文把它当本体内部资源，与 run / session 同级。反方：ADR-039 §5 说「不允许为新东西发明新的顶层名词」，工单会不会漏到用户界面变成第四个词？替代方案：扩展 session 加授权依据字段，不加新对象（原型否决过「Ticket 跟随 Session」，但那是团队场景）。
2. **个人分身到底要不要 Router 层的语义研判？**本文说确定性准入 + 订阅就够，语义研判只对主动候选按需起 run。反方：微信群里「谁在跟我的分身说话、说的算不算请求」本身就是语义问题，纯白名单会漏接单。
3. **先落库后执行的队列要不要覆盖模块工具调用？**本文按 K1-01 §2.6 把模块工具调用排除在自动重试之外，只对通道发送用幂等键。反方：这会让「工单」里的大部分外部动作仍然没有恢复路径，ADR-043 的价值被 K1-01 削掉一半。

## 7. 未核实

- nostr-bridge 的「authorized peer」判定规则（`packages/nostr-bridge/src/inbound.ts` 只看了导出名）。
- Hyphae `groupchat.rs` 是否已有讨论串概念可映射 Thread。
- Agent24 设置是否有修订号与冲突拒绝（§2.5 最后一行）。
- setup 时是否剔除含 KEY / TOKEN 的环境变量。
- 原型源码 `store.ts` / `effects.ts` / `github-channel.ts` 的实际行为（只读了 007 的描述）。
- DeepSeek 原文逐字内容（只有转载的模型摘要）。
- 原文实验数字（p50 12.4 分钟等）未复核。
