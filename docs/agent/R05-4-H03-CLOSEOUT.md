# R05-4：H03 已合并格子收口

本稿不代外审打勾。R05 仍是 DRAFT。本稿不把 `docs/adr/ADR-MEMORY-RECALL-MESSAGE-ORIGINS.md` 改成已批准。评审对象是分支 `ab/memory-p1-47-h03-closeout` 的精确 head。clestons 对该 head 的 APPROVE 只接受本页记录。该 APPROVE 不接受 ADR，也不派出 G1。

基线是 `origin/ab/memory-p1` 的 `b9e4eb5f059e1d406e562cfbd4ec368ef5b5fc1e`，即 PR #1205 的 merge。2026-10-26 用 `git fetch --no-prune origin ab/memory-p1` 核对过这个 tip。本稿不改 `docs/agent/R05-1-EXACT-CONTRACT.md` 第 4 节的原文。下面只记下哪些格子已经有合并提交。

本稿只新增这一份文件。不改 PLAN 勾选，不改 ADR 状态行，不改 R05-1、R05-2、R05-3，不改 Rust、migrations、OpenAPI 或协议。

## 1. 已合并提交

下表是这条基线上已核对的合并。H02 同秒审计碰撞的 merge 是 #1152 `7dd6f08f2e979e2e4e54caa23e4c3834cd97fc5d`。计划正文若仍写它未完成，那是过期句子。本稿不重做 H02，也不改那份计划。

| PR | merge | head | 本稿用来锚定什么 |
| --- | --- | --- | --- |
| #1166 | `2fabb2282d6a7faebf734c7dc5ff2c6b81e2f805` | 测试片 | daemon 启动恢复测试。不关闭第 2 节的 H03 矩阵。 |
| #1167 | `5ad99157bc8c290162fb0a315ffe00a18377bd26` | 测试片 | HTTP pause 不改已有 snapshot。不关闭第 2 节的 H03 矩阵。 |
| #1168 | `c9a19532675ceee98a7fe1bfbb9f503c000fad92` | `54bafa8010ba3d163df4c967188ddeb07888a60f` | 来源表 migration `0021`，消息与来源同一事务，读取时核对 digest。不回填旧行。 |
| #1170 | `4fbc3728080298bfe2b0c942293c296eff7fa63b` | `860d9ad4c6f7ba555108ceba8db774a82f187f91` | `docs/agent/R05-1-EXACT-CONTRACT.md`。精确契约。不实现 H03。 |
| #1173 | `9ba7d5ee69880cfc6b8ff555f60148cd59f8146b` | `21f6e0bd5bb9869e4048f2ba19ba1465f17286e8` | `docs/agent/R05-2-D7-KIND-DECISION.md`。D7a–D7f。暂停通知是有类型的无来源行。 |
| #1181 | `631ff507728273a5f3d84d11b3c0f6b7fcb6ec10` | `d86b6386aa9b825b1a65e2ad9cda4216ea622089` | 第一次快照与来源同一事务。只写快照。 |
| #1185 | `2207cdc6bf16c937aa9f70c70c9d17fe24c21359` | `201c29fe8ecade668d737c8b548dbbf8a471bcd3` | `docs/agent/R05-3-D7F-KIND-DECISION.md`。CHECK 增加 Assistant、ToolResult。`recall_ref` 为 SQL NULL。 |
| #1195 | `b680cb0a5d2eba90266e5e9f885bfbd40e761938` | `e2652062d99224e1c69f1bf8b441cb5b2229c2c0` | migration `0022` 重建 CHECK。`persist_message` 写入 Assistant 与 ToolResult。不改 `0021`。不回填。 |
| #1197 | `9df7d670005b6e8e58b73c470d2268368860a817` | `c25507b0d77441470f22f3796d797ba7734f974e` | 来源无法证明时不发送。恢复路径在执行停住的工具之前核对来源。2026-10-24T21:04:49Z 合并。 |
| #1205 | `b9e4eb5f059e1d406e562cfbd4ec368ef5b5fc1e` | `1cfb520111311fa736376534fee7b740bddf3689` | 非 assistant 回合在落库和派发工具之前以 `provider_error` 拒绝。2026-10-25T03:35:18Z 合并。 |

#1197 的批准是评审 5640020302，对象是 head `c25507b0d77441470f22f3796d797ba7734f974e`，时间 2026-10-24T21:03:52Z。编码提交 `1cf3ccd92ece7ce8c11e2062f2702cd59af2c925` 是该 head 的祖先：快照写入 HostRecall 时，`recall_ref` 是全部 assertion id 的字典序紧凑 JSON 数组。一个 id 是 `["id"]`，不是裸字符串。已有 SQL NULL 不回填。

#1205 的批准是评审 5641360546，对象是 head `1cfb520111311fa736376534fee7b740bddf3689`，时间 2026-10-25T03:34:27Z。clestons 写明 G3 已满足。`bde02855bd50b0a9cb7f0c5f3a2fb810134154bf` 只守了带工具调用的分支，该提交也是这次 merge 的祖先。行为以 `1cfb5201` 为准。

## 2. R05-1 第 4 节现在对应哪次合并

列名仍是：存储里有什么、provider 可以收到什么、必须裁掉什么、snapshot 是否保持。本稿不改那些格子的原文。

### 存储列

HostRecall、UserPrompt、SessionHistory、同文三来源、缺行、坏 digest、冷恢复的存储列，在 R05-1 里已经是「存储已落地」。#1168 落地读写。#1181 落地第一次快照写点。#1195 落地 Assistant 与 ToolResult 的新写入。这些后续合并没有把存储列改回未落地。

Assistant 与 ToolResult 只在 `recall_ref` 为 SQL NULL 时读成 Present。其他情况 `origin_from_row` 返回空，读取是 Unreadable。HostRecall 的空字符串在读取时同样是 Unreadable。NULL 的 HostRecall 仍读成 Present。

K1 那一行的存储列仍是未实现。见第 3 节。

### 缺行、坏 digest、NULL HostRecall：整次不发送

这三格的「provider 可以收到什么」有合并提交，就是 #1197。

`persisted_origins_allow_send` 按已落盘 seq 读来源。读错误、`Missing`、`Unreadable`、`HostRecall { recall_ref: None }` 都使这次发送不发生。`run_loop` 在调用 provider 之前因此 `finish_failed`。code 是 `source_unproven`。文案是「无法证明本线程已落盘消息的来源，已停止发送。请新建会话。不要自动复制旧 recall 或待执行工具。」历史保留。审批 grant 不移动。snapshot 写点不改。没有 memory 模块时也不跳过这道门。

`drive_resume` 在 `assess_restore` 和工作区权威检查之后、结算停住的工具、派发和 `RunStarted` 之前做同一核对。`AwaitingApproval` 不能直接进入 `Failed`，所以拒绝时先进入 `Running`，不发出 `RunStarted`，再以同一 code 和文案 `finish_failed`。旧线程没有来源行时，批准和拒绝都不调 provider，也不执行 shell。工作区权威失败仍在来源门之前取消。活着的 `ApprovalBroker` 唤醒代码没有另加一道门；它进入的 `run_loop` 在每次 provider 请求前核对。

旧 run 没有来源行时仍是 Missing。没有 backfill。这种线程不自动发送。

这三格的「必须裁掉什么」没有按条从请求里摘掉之后继续发送其余行。已落地的消费者是整次拒发。`request_messages_for` 仍按位置 0，加上 recall 前缀和结束标记，决定要不要改写或丢掉开头那一条。它不读 kind，也不决定这次能不能发送。

缺行和坏 digest 的 snapshot 列在 R05-1 里已经是「存储已落地」：读取不删消息，也不把旧 snapshot 标成 NoRecall。#1197 的拒发同样不删消息，也不改 snapshot 正文。

### HostRecall、UserPrompt、SessionHistory、同文三来源

Present，并且不是 `HostRecall { recall_ref: None }` 的行，不因 #1197 的门停发。这道门不解析 `recall_ref`，也不对照账本。因此这四行的「provider 可以收到什么」和「必须裁掉什么」还没有合并提交。允许发送之后，请求正文仍由 `request_messages_for` 按位置和标记整形。同文三行的 kind 仍只来自写点。digest 不覆盖 kind。

这四行的快照写点是 #1181。#1197 的拒发不删这些 snapshot。这不关闭冷恢复那一行。

`1cf3ccd9` 只规定 HostRecall 写入时的 JSON 形状。它不证明数组里的 id 都在账本上。那是 G1。

### 冷恢复

冷恢复一行的「provider 可以收到什么」「必须裁掉什么」「snapshot 是否保持」仍没有合并提交。#1197 的 `drive_resume` 就是进程重启之后、从库里读回线程的持久恢复入口（`resume_run` 的文档注释，`agent24d` 的 `approvals.rs`）。它在这个入口上落地了拒发那一半：来源无法证明时不调 provider，也不执行停住的工具。来源可以证明时的那一半还没有：按来源决定发什么、裁什么，snapshot 是否保持，以及对应的矩阵测试。

### G3 在矩阵之外

#1205 不填写第 4 节的来源格子。它落地的是另一件事：`run_loop` 在累计 usage 之后、最终答复和工具派发分叉之前检查 role。不是 `assistant` 就 `finish_failed`。code 是已有的 `provider_error`。文案是「模型返回的角色不是 assistant，已停止。不会执行这一轮的工具。」这次返回发生在落库、`ModelDelta`、`remember_exchange` 和工具派发之前。role 不被改写成 assistant。

## 3. 明确推迟

下面每条都另开一片，而且只在自己的触发出现时开。本稿不实现其中任何一条。合入本稿之后才决定要不要单开 G1。G2、G4、G5、K1、冷恢复、purge、P1-02c、#794 和 release 不跟那一片捆在一起。

- **G1**。触发：有一片把契约写成，在把 `recall_ref` 当作成员证明之前，把它解析成字典序紧凑 JSON 数组，并对照账本核对每个 id；解析失败或未知 id 视为 Unreadable。#1197 评审 5640020302 将此列为后续门。
- **G2**。触发：有一片会调用 provider 的工具或子代理，或有一处派发绕过 `run_loop` 与 `drive_resume` 的来源门。该片必须先核对来源，或只能在核对之后到达。
- **G4**。触发：有一片给 Explorer 加上 `fs_read` 以外的工具或网络，或持久化 Explorer 转录，或让它的回合进入带来源的落盘行，或新增另一个由 provider 驱动的工具循环。该片必须在转录入栈和派发之前拒绝 `role != "assistant"`。只改当前只读的 `fs_read` 不构成这个触发。#1205 评审 5641360546。
- **G5**。触发：有一片依赖「被拒绝的回合不发事件、不写记忆」。该片必须在已收集的 events 上断言没有 `model.delta` 和 `run.completed`，并加一个带会话的用例断言没有记忆写入。
- **K1**。触发：有一片把契约写成消息、来源映射和适用的 K1 标签同一事务。R05-1 写的是 `append_run_messages_tx` 之后再独立 `tag_run_source` 的窗口，它不是 H03 发送消费者。#1181 之后生产路径是 `append_snapshot_run_messages`（消息与来源同一事务）之后再独立 `tag_run_source`。K1 标签仍不在那个事务里。发送门的后续片不能把 K1 写成已实现。
- **冷恢复**。触发：有一片把契约写成进程重启之后、不靠进程内缓存，按已落盘来源决定 provider 可以收到什么、必须裁掉什么、snapshot 是否保持。`drive_resume` 只落地了拒发那一半，不关闭这三格。

## 4. ADR、计划与仍开着的项

`docs/adr/ADR-MEMORY-RECALL-MESSAGE-ORIGINS.md` 开头仍是「DRAFT / 待独立设计评审，2026-10-23。」本稿不改这一行。批准本稿不把 ADR 改成 accepted。

`docs/agent/PLAN-MEMORY-P1.md` 第 4 节的 R02/R05 与 R06-H03 仍是 `[ ]`。本稿不勾这些格子。已经合并的代码也不等于那一行可以勾掉。

adr-s9 仍开。没有生命周期 ADR 第 9 节的接受或修订记录。它仍只挡住 P1-02c 和 purge。本稿不关闭 adr-s9。#794 仍按 PLAN 第 6 节单独核。本稿不改 #794。

不开 H04。不开 LoopX。不开 `ab/memory-p1` 到 `main` 的 release。那个 release 由人开、人审、真机验收。

两处已知 Low 保持原样。暂停失败的文案仍含「请新建会话」。记忆还暂停着时，新会话会以同一 code 失败。落盘失败且这句还会进入下一次模型请求时，run 以已有 code `internal` 结束，不再推进内存向量。没有单次 append 失败注入测试。现有 store 测试钩子不能只让一次 append 失败。本稿不加这个钩子，也不改文案。

`/api/v1/chat` 的 `chat_memory_prelude` 不走 `run_loop`。记忆暂停时它仍告诉模型「记忆已暂停」。本稿不改这条路径。

## 5. 本片边界

- 只新增本稿。
- 不把 R05 从 DRAFT 改成已批准。
- 不勾 PLAN 的 R02、R05 或 H03。
- 不改 R05-1、R05-2、R05-3，不改 ADR 状态行。
- 不改 migrations、Rust、协议或 OpenAPI。
- 不实现 G1、G2、G4、G5、K1 或冷恢复。
- 不启动 purge，不做 P1-02c，不改 #794，不开 H04，不开 release。
- 不挂心跳。
- 本稿不代外审打勾。
