# R05-3：D7f 存储 kind 决定

本稿不代外审打勾。本稿不关闭 R05-1 的 D7。它只给 `persist_message` 新写入的行建议存储 kind。评审对象是分支 `ab/memory-p1-43-d7f-kinds` 的精确 head。clestons 可以按下面的 ID 逐条接受、收窄或拒绝。这份文件不批准自己。

基线是 `origin/ab/memory-p1` 的 `631ff507728273a5f3d84d11b3c0f6b7fcb6ec10`，即 PR #1181 的 merge。PR #1181 的 head `d86b6386aa9b825b1a65e2ad9cda4216ea622089` 已由 clestons APPROVE。那次 APPROVE 落地的是第一次快照写点。本稿不重做 D7a 到 D7d，不改 `append_snapshot_run_messages`。

`docs/agent/R05-2-D7-KIND-DECISION.md` 写明，运行中经 `persist_message` 写入的 assistant 与 tool 行不是 HostRecall、UserPrompt 或 SessionHistory，也不是永久无来源。存储 kind 留给以后的迁移片。在该片被接受之前不派 H03。本稿只提出 CHECK 字面量。迁移留到本稿被接受之后的下一片。

本稿只新增这一份文件。不改 PLAN，不改 ADR，不改 R05-1，不改 R05-2，不改 Rust，不改 migrations。

## 1. 建议决定

按宿主的哪一次写入来分类。不按客户端自报，不按模型自报，也不按 tool 自报。相同的 role 不能推出相同的 kind。快照里 role 为 assistant 或 tool 的副本仍是 D7c 的 SessionHistory。下表外审列保持待外审。本稿不代外审打勾。

| ID | 精确文本 | 不接受则不能做 | 外审 |
| --- | --- | --- | --- |
| D7f1 | `persist_message` 新写入、且 role 为 assistant 的完成消息是 Assistant。这包括 `run_loop` 在执行工具前落盘的 assistant `tool_calls` 行，也包括最终回答。建议的 CHECK 字面量是 `Assistant`。`recall_ref` 必须是 SQL NULL。 | 不得标成 HostRecall、UserPrompt、SessionHistory、ToolResult 或 HostSystem。不得把 D7c 的副本改成 Assistant。 | 待外审 |
| D7f2 | `persist_message` 经 `Msg::tool_result` 新写入的行是 ToolResult。这包括跳过、拒绝、计划批准、计划拒绝、冷恢复结算和工具执行结果。建议的 CHECK 字面量是 `ToolResult`。`recall_ref` 必须是 SQL NULL。这些行的 role 是 tool。 | 不得标成 HostRecall、UserPrompt、SessionHistory、Assistant 或 HostSystem。不得把 D7c 的副本改成 ToolResult。 | 待外审 |
| D7f3 | 本基线的九处生产调用没有把 `Msg::system` 或 `Msg::user` 传给 `persist_message`。完成消息的 role 仍可能来自供应商 JSON。本稿不为 system、user 或其他 role 再设 kind。 | 不得把这种行标成 Assistant、ToolResult、HostRecall、UserPrompt、SessionHistory 或 HostSystem。不得把这种行声明为永久无来源。不得在本片为此加迁移。 | 待外审 |
| D7f4 | 暂停通知不是 `persist_message` 的行。它仍按 D7d 保持无来源，读取继续是 Missing。 | 不得在本建议里加入 HostSystem。 | 待外审 |
| D7f5 | 客户端、模型或 tool 不得自选 kind。kind 不是 `Msg` 的字段，也不是供应商 JSON 的字段。只有宿主写点可以写入 Assistant 或 ToolResult。 | 不得按 REST 的 role、自报 source、模型正文或 tool 正文选择 kind。 | 待外审 |

以后的迁移片若同时保留已有三种 kind，建议的列约束是 `CHECK (kind IN ('HostRecall', 'UserPrompt', 'SessionHistory', 'Assistant', 'ToolResult'))`。`rust/crates/agent24-store/migrations/0021_run_message_origins.sql` 今天的 CHECK 只有前三个。本稿不改这个文件。HostSystem 不进入这条建议 CHECK。

`docs/adr/ADR-MEMORY-RECALL-MESSAGE-ORIGINS.md` 仍是 DRAFT。它的表里出现过 Assistant、ToolResult 和 HostSystem。存储里没有这三个名字。本稿只把前两个当作建议字面量引用。这不把该 ADR 写成已经落地。

Assistant 与 ToolResult 的 `recall_ref` 只接受 SQL NULL，规则与 UserPrompt、SessionHistory 相同。非 NULL 在读取时不得变成 Present。`tool_call_id` 留在 `run_messages`，不写入 `recall_ref`。

## 2. 实际写入的 role

调用点来自基线 `rust/crates/agent24-agent/src/lib.rs` 的 `persist_message`。该函数只调用 `append_run_message`，不写 `run_message_origins`。这些行今天的来源读取都是 Missing。

| 调用 | 写入什么 | role |
| --- | --- | --- |
| `run_loop`，`tool_calls` 为空 | `res.message` | 供应商消息的 role。宿主在落盘前不改它 |
| `run_loop`，工具轮开始前 | `res.message` | 同上。这一行是冷恢复要重放的 assistant `tool_calls` 行 |
| `run_loop`，超过每轮工具上限 | `Msg::tool_result` | tool |
| `run_loop`，计划批准 | `Msg::tool_result` | tool |
| `run_loop`，计划拒绝 | `Msg::tool_result` | tool |
| `run_loop`，计划模式拒绝写工具 | `Msg::tool_result` | tool |
| `run_loop`，工具执行返回 | `Msg::tool_result` | tool |
| `drive_resume`，计划拒绝 | `Msg::tool_result` | tool |
| `drive_resume`，回答停住的调用 | `Msg::tool_result` | tool |

`Msg::tool_result` 把 role 设为 tool。`Msg::assistant` 把 role 设为 assistant。上面九处没有 `Msg::system`，也没有 `Msg::user`。`drive_resume` 不再次写入已经落盘的 assistant `tool_calls` 行。它只追加 tool 行。活着的审批唤醒不走 `drive_resume`。

`run_loop` 的 `CompletionRequest` 把 `progress` 设为 None，所以走非流式回答。`OpenAiCompatProvider` 经 `From<OaMessage> for Msg` 把 JSON 的 role 原样抄入。流式分支会把 role 写成 assistant，但这条循环不走流式。本仓库里构造完成消息的测试与桩使用 `Msg::assistant`。生产循环本身不检查 role。

`snapshot_origin_tests.rs` 的 `persist_message_does_not_write_run_message_origins` 又调用了一次 `persist_message`，参数是 `Msg::assistant`。那是测试。它不增加生产 role。它证明这次调用不写来源行。

暂停通知由 `execute` 放进 `SnapshotRunMessage::PauseNotice`，role 为 system。它不经过 `persist_message`。

## 3. 批准前后

clestons 在本文件的精确 head 上 APPROVE 之前，不加迁移，不改 `persist_message`，不派 H03。这个 APPROVE 不关闭 R05-1 的 D7，也不改发送路径。

APPROVE 之后的下一片才是迁移和写点。写点只覆盖 `persist_message` 新写入、且被 D7f1 或 D7f2 接受的行。它不改第一次快照，不把暂停通知送进 `append_originated_run_messages`，不改 `request_messages_for`。那一片仍不改发送路径。H03 仍要等该迁移片被接受。本稿的 APPROVE 不派出 H03。

D7f3 的行没有被本稿配 kind。写点片不得用 role 字符串给它挑一个 kind。

adr-s9 仍只挡住 P1-02c 和 purge。没有 `docs/adr/ADR-MEMORY-P1-LIFECYCLE-AND-PURGE.md` 第 9 节的接受或修订记录时，本稿不关闭 adr-s9。本稿的 APPROVE 也不代替该记录。本片不改 #794。

## 4. 本片边界

本片只保持下面这些边界。

- 只新增本稿。
- 不把 R05 从 DRAFT 改成已批准。
- 不关闭 R05-1 的 D7。
- 不勾 PLAN 的 R02、R05 或 H03。
- 不改 R05-1，不改 R05-2，不改 ADR 状态，不改 migrations、Rust、协议或 OpenAPI。
- 不加迁移，不改 `persist_message`，不改发送路径。
- 不重做 D7a 到 D7d 的快照写点。
- 不实现 H03，不开 H04。
- 不启动 purge，不做 P1-02c，不改 #794。
- 本稿不代外审打勾。
