# Knowledge 对 K1 的依赖与私有数据禁用门（KB-SEC-01a）

> 任务：KB-SEC-01a（#1088）· 阶段 #1001（WS2）· 内核依赖跟踪 #735 · 下游验收 KB-SEC-01b（#1089）
> 依据：本 worktree（基于 `ab/weknora` `eb1a58bb`）的代码；文中 `文件:行` 都按这个提交，Rust 路径相对 `rust/`。issue 正文按 2026-10-18 的快照。
> 本文只定契约与安全前提，不写实现、错误码映射和函数签名。

## 1. 目的与范围

Knowledge 检索到的证据（search 的 hit、read 的正文）进入任何模型调用之前，都必须带上 K1 的来源标签，并受 K1 隐私策略约束。本文登记 Knowledge 依赖的每项 K1 能力，以及它的现状、缺口 owner、交付状态，并规定 K1 未交付期间的私有数据禁用门（§3）。
范围只到 Agent24 一侧。WeKnora 服务端的处理出口（例如 search 时计算 query embedding，契约 R3.11，`docs/knowledge/CONTRACT-READONLY-v1.md:100`）由 KB-SVC-POLICY-01 负责，不在 K1 的控制范围内。

## 2. 依赖清单

状态取值：**已有**＝代码已实现并在本 worktree 的历史里；**部分**＝有可用部件，但缺 Knowledge 所需的接线；**缺失**＝没有对应实现。

| 编号 | Knowledge 需要什么 | K1 现状与出处 | 缺口 owner | 交付状态与跟踪 | 依赖它的 Knowledge 任务 |
|---|---|---|---|---|---|
| K1-D1 | 证据要有一个确定的来源类别，并带上修订摘要 | **部分**。`SourceKind` 只有 `UserInput`、`SelectedMaterial` 两类（`crates/agent24-store/src/source_policy.rs:56-64`）。`SelectedMaterial` 已建模，但注释写明"No Agent24 entry point populates this yet"（`:59-62`）。构造函数 `SourceRef::selected_material` 带 `revision_digest`（`:131-145`），生产代码里没有调用点，只在本文件的测试里出现（`:343`、`:356`）。Knowledge 证据是沿用 `SelectedMaterial`，还是新增类别，尚未决定 | 类别选择：David（K1 接续，#735 2026-10-11 评论）提出；改内核枚举需要 jason 确认（#1087 冲突分类 c） | 未开工；#1087 PR c；#735 | KB-C1-05 c |
| K1-D2 | 证据作为工具结果进入 run 时，由宿主写入来源标签，并关联到对应消息 | **缺失**。run 入口只给用户输入写标签：`SourceRef::user_input`，在 `crates/agent24-agent/src/lib.rs:1547-1560`，写入失败时 run 以 `source_policy_unavailable` 结束。工具结果在 run 中途进入上下文，目前没有写标签的接缝。存储侧已经具备：`Store::tag_run_source`，按 `(run_id, seq, source_id)` 写入（`source_policy.rs:265-290`）；表结构见 `crates/agent24-store/migrations/0016_run_source_tags.sql:18-26`。标签只能由宿主构造，模型和工具都不能写（`source_policy.rs:82-86`） | David（K1）；改 agent24-agent 的已有接口需要 jason 确认（#990、#1087） | 未开工；#1087 PR c；#735 | KB-C1-05 c、KB-SEC-01b |
| K1-D3 | run 里只要有证据，模型调用就按最严来源收窄；LocalOnly 时绝不路由到远端 | **已有（仅模型路径，6b.2）**。折叠规则是"最严者胜"，没有标签按 LocalOnly 处理（`source_policy.rs:186-212`）；`merge_source_policy` 在 `crates/agent24-agent/src/lib.rs:49-57`，`task_profile_for_run` 在 `:482-495`，读取失败也按 LocalOnly。各调用点：run loop（`lib.rs:1726`，审批续接和重启恢复也走这里，见 `source_policy.rs:15-17`）、REST chat（`apps/agent24d/src/routes.rs:569`）、explorer 子调用（`crates/agent24-agent/src/subagent.rs:218-231`）、会话压缩固定按 LocalOnly（`lib.rs:239-240`）。路由层对 LocalOnly 只排本地档位（`crates/agent24-models/src/router.rs:86-91`），没有本地 provider 时直接报错，不回退到远端（`router.rs:432-435`）。按 `source_policy.rs:115-125` 与 `:199-212` 推断：用户输入的标签恒为 LocalOnly，所以当前每个 run 都是 LocalOnly | — | 已经随 #1022 发布到 main（见 K1-D8） | KB-C1-05、KB-SEC-01b |
| K1-D4 | 证据不能经非模型出口外传：HTTP、MCP、其他模块、Exec、记忆、后台任务、委派 | **缺失**。`source_policy.rs:28-29` 写明 HTTP/MCP/module/Exec 的出口门推迟到 6b.3；ADR-K1-02 §6 第 3、4 片（`docs/adr/ADR-K1-02-session-dataflow-security-modes.md:106-107`）分别是 6b.3 统一出站判定、6b.4 Exec/记忆/后台，第 5 片 6b.5 是逐出口矩阵。现在只有模块工具通告这一处，用 `has_remote_tier()` 作过渡门（`apps/agent24d/src/server.rs:1615-1619`；`router.rs:204-207`） | David（K1，#735） | 6b.3–6b.5 未见提交；#735 | KB-SEC-01b（全路径出口验收）、KB-C1-05 |
| K1-D5 | 获准云处理：把单份资料按"资料 × 用途 × 目的地"授权为 `CloudAuthorized` | **缺失（已建模，未授予）**。`SourceMode::CloudAuthorized` 只建了模；本切片的写路径从不产生这个模式（`source_policy.rs:66-80`），`authorization_ref` 恒为 `None`（`:102-104`），`policy_version` 恒为 `0`，表示还没接上政策权威（`:97-101`）。授权粒度由 jason 在 2026-10-07 拍板（#735 评论第 2 点） | David（K1）；可授权哪些目的地待 #994 | 未排期；#735、#994 | KB-SVC-POLICY-01、KB-SEC-01b（获准云正负例）；v1 不需要 |
| K1-D6 | 审计能回答"哪条证据进入了哪次模型调用"（只记 id，不记正文） | **部分**。标签按 `(run_id, seq)` 关联到消息（`0016_run_source_tags.sql:18-26`），可以回答"哪条证据进入了哪个 run 的哪条消息"。模型调用耗时记录 `NewCallTiming` 没有 run_id 和来源字段（`crates/agent24-store/src/model_call_timings.rs:17-32`），无法回答"进入了哪次模型调用"。K1-7.2 的审计只覆盖模块工具（`apps/agent24d/src/module_tool_runtime.rs`），Knowledge 的 Tool 走内置 Tool trait，不是模块工具，是否在覆盖范围内：待核实 | 待定（K1-7 / ADR-K1-04 还是 Knowledge 自己做，尚未分配） | 未开工；#735 第 4 项 | KB-C1-05、KB-SEC-01b（证据链）、#1001 退出验收 |
| K1-D7 | 跨重启、恢复、续轮时，证据的标签不丢失，也不被放宽 | **已有（仅标签存储）**。标签已落盘，重启后可读回（测试见 `source_policy.rs:469-501`）。schema 未知或损坏的行读回时降为 LocalOnly（`:215-252`）。对话文本不能放宽已持久化的政策（`:31`） | — | 随 #1022 发布 | KB-SEC-01b（恢复、续轮路径） |
| K1-D8 | K1 经 main 发布（#1089 的前置条件） | **部分**。main 上已有 release 提交 `56264ef9`（#1022，2026-10-16），提交标题为"迁移 0015–0017 + consent/工具调用/审计"，本 worktree 的历史已包含它。6b.3–6b.5 不在 main，`ab/kernel` 上也没有对应提交（K1-D4）。#735 截至 2026-10-18 仍为 OPEN | David（K1，#735） | 部分发布；#1022、#735 | KB-SEC-01b、§3 禁用门 |
| K1-D9 | Knowledge 自己的传输（Agent24 → WeKnora）发出的 query 和 scope 也要经过出站判定 | **缺失**。`agent24-knowledge` crate 还不存在（KB-C1-02a，#1083）；出站判定 API 属于 6b.3，同样未交付（K1-D4）。WeKnora 部署在内网时算不算"本地"，待 #994 | David（Knowledge 传输侧）＋ David（K1 6b.3）；口径待 #994 | 未开工；#1083、#1084、#994 | KB-C1-02b、KB-C1-04、KB-SEC-01b |

## 3. 私有数据禁用门（安全前提）

本节是规范性条款，"必须""不得"都是强制要求。

- **G1 门的状态。** 下列条件全部满足之前，门处于**关闭**状态：
  - (a) K1-D2、K1-D4 所需的 K1 能力已经经 main 发布；
  - (b) KB-SEC-01b（#1089）的全路径出口验收通过，LocalOnly 下远端调用计数为零；
  - (c) #994 已有决定，并且已经回写到 §4。

  开门要做一次本文修订，链接上述证据，由 David 复述确认；涉及内核接口时还需要 jason 确认。这道门不是运行时开关，不提供用户或配置层面的绕过手段。
- **G2 关门期间允许的资料。** 门关闭时，Knowledge 只能接入 Mock 数据和**批准样本**（测试或样本资料），不得接入任何私有资料或真实用户资料。不能证明属于批准样本的资料，一律按私有资料处理。
- **G3 "本地"的口径。** 在 #994 作出决定前，内网服务器、局域网服务、同一组织的远端主机都**不算**设备本地（#1089）。
- **G4 失败即拒绝。** 门关闭时，如果 KnowledgeProvider 发现资料不属于批准样本、标签写入失败，或无法确认所需的 K1 能力是否就绪，它**必须拒绝**这次检索或注入（fail closed）。不得静默降级，包括：去掉标签后照常注入、改走远端模型、改走其他出口、只告警不拒绝。拒绝不能被解释为同意。
- **G5 先打标签，后注入。** 每条证据必须先由宿主写入来源标签（K1-D2），才能进入模型上下文；标签写入失败时，证据不得注入。这与用户输入的现有做法一致（`crates/agent24-agent/src/lib.rs:1549-1559`）。
- **G6 不得放宽。** 在 K1-D5 交付并且 #994 作出决定之前，证据标签的模式只能是 LocalOnly。模型输出、工具结果、证据正文都不能放宽这个模式（`source_policy.rs:31`、`:82-86`）。证据正文是数据，不是指令（#1087）。
- **G7 与 #1001 的关系。** 这道门是 #1001 退出验收中"真实数据验收"一项的前提，与 KB-C1-03、KB-C1-01、KB-OBS-01、KB-SVC-POLICY-01、WeKnora 基线升级并列，任何一项都不能替代这道门。用 Mock 和批准样本做的 #1001 验收不受这道门阻塞。
- **G8 过渡限制不撤。** 门关闭期间，K1-D4 里的 `has_remote_tier()` 过渡门不得撤除（#735 jason 2026-10-07 第 5 点；ADR-K1-02 §6 第 5 片）。

## 4. 待决与 owner 未定

| # | 事项 | 影响 | 依赖的决策 | owner |
|---|---|---|---|---|
| 1 | LocalOnly 是否严格限于设备本地；内网远端适用什么政策 | G3、K1-D9、KB-SEC-01b 的验收口径 | #994 | jason（或用户）决定，David 复述 |
| 2 | 测试资料和正式资料分别允许经过哪些模型、解析、存储、追踪（Langfuse）服务；费用上限由谁承担 | K1-D5、K1-D9、KB-SVC-POLICY-01；契约 §12 第 3 项（`CONTRACT-READONLY-v1.md:353`） | #994 | 同上 |
| 3 | Knowledge 证据沿用 `SelectedMaterial`，还是新增 `SourceKind` 类别 | K1-D1 | ADR-K1-02 的范围；#1087 PR c | David 提出，jason 确认 |
| 4 | "证据 → 模型调用"这一级审计归 K1-7 还是 Knowledge | K1-D6 | ADR-K1-04 | 待定 |
| 5 | #1089 里的"K1 经 main 发布"，是否指 6b.3–6b.5 也已在 main（本文 G1(a) 按"是"处理） | K1-D8、G1 | #1089、#735 | David 提出，jason 确认 |
| 6 | 批准样本清单由谁维护，Provider 怎么识别批准样本 | G2、G4 | KB-C1-04（#1086）；可能涉及 #992 | 待定 |
| 7 | 6b.3–6b.5 的排期（#735 截至 2026-10-18 仍为 OPEN） | K1-D4、K1-D8 | #735 | David（K1） |

## 5. 变更记录

- r0（2026-10-18）：首稿。
