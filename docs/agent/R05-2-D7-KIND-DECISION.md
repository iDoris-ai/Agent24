# R05-2：D7 来源种类决定

本稿不代外审打勾。本稿不关闭 R05-1 的 D7。它只给当前快照批次提出可按条接受的建议，并写明运行中的 assistant 与 tool 行还要另片决定存储 kind。评审对象是分支 `ab/memory-p1-41-d7-kinds` 的精确 head。clestons 可以按下面的 ID 逐条接受、收窄或拒绝。这份文件不批准自己。

基线是 `origin/ab/memory-p1` 的 `4fbc3728080298bfe2b0c942293c296eff7fa63b`，即 PR #1170 的 merge。PR #1170 的 head `860d9ad4c6f7ba555108ceba8db774a82f187f91` 已由 clestons APPROVE。`docs/agent/R05-1-EXACT-CONTRACT.md` 第 6 节写明，那次 APPROVE 不是派 H03 的充分条件。本稿不改 R05-1。

本稿只新增这一份文件。不改 PLAN，不改 ADR，不改 Rust，不改 migrations，不改 OpenAPI。

## 1. 建议决定

按谁构造了这一行来分类。不按客户端自报，也不按模型自报。相同的 role 不能推出相同的 kind。下表每一行有自己的 ID，外审可以只接受其中几条。外审列保持待外审。本稿不代外审打勾。因为 D7f 还要等以后的迁移片，本稿整体不关闭 R05-1 的 D7。

| ID | 精确文本 | 不接受则不能做 | 外审 |
| --- | --- | --- | --- |
| D7a | 宿主插入、且带非空 assertion id 集合的 recall 块是 HostRecall。一个块携带这次召回放进正文的全部 id。SQL NULL 的 HostRecall 发送时仍必须拒发。 | 不得把该块标成 UserPrompt。不得放行 SQL NULL 的 HostRecall。 | 待外审 |
| D7b | 本轮用户正文是 UserPrompt。 | 不得把 prior context 里的历史 user 行标成 UserPrompt。 | 待外审 |
| D7c | 宿主从自己的 session log 复制进本次快照的 prior context 是 SessionHistory。行内 role 可以仍是 user、assistant 或 tool。宿主加在这段前面的 system 摘要也是 SessionHistory。kind 是宿主的副本分类，不是模型选的 kind。这不包括本轮运行中经 persist_message 新写入的 assistant 或 tool 行。 | 不得把这些副本标成 HostRecall、UserPrompt，或尚未落地的 Assistant、ToolResult。不得用 D7c 覆盖 D7f。 | 待外审 |
| D7d | 暂停时宿主插入的 system 通知没有现成 kind。它保持无来源行，读取继续是 Missing。该行保持无来源之后，按 R05-1 的 D1 与 D2，这次恢复会被机械地停发，并给出新建会话回执；不会从 role 或通知正文推断 kind。这是既定后果，不是留给实现去补的洞。 | 不得标成 HostRecall、UserPrompt 或 SessionHistory。以后要 HostSystem 必须另开迁移片。 | 待外审 |
| D7e | REST 的 role 等于 system、客户端自报 source、模型或 tool 指定 kind，都不是宿主构造证明。 | 不得据此写入 HostRecall、UserPrompt 或 SessionHistory。 | 待外审 |
| D7f | 本轮运行中宿主经 persist_message 写入的 assistant 与 tool 行，包括冷恢复要重放的 assistant tool_calls 行，不是 D7c 的 prior context。不得把它们标成 HostRecall、UserPrompt 或 SessionHistory，也不得把它们声明为永久无来源。永久无来源会让带 tool call 的冷恢复在写点切换之后仍被 D1 排除，和 D1「不得一律停发」冲突。决定是以后另片增加存储 kind。本稿不选 CHECK 字符串，不加迁移。写点切换不得给这些行写 kind，也不得把它们送进 `append_originated_run_messages`。H03 在该迁移片被外审接受之前不得派出。因此本稿不关闭 R05-1 的 D7。 | 不得在本片实现该迁移，不得改发送路径。 | 待外审 |

非空 assertion id 集合是 HostRecall 的构造条件。`session_memory` 的召回只在收集到的 `ids` Vec 非空时返回一个块；一个块携带预算内放进正文的全部 id，上界是这次召回的 top-k。检查的是这个 Vec 是否为空，不是单个 `hit.assertion.id` 非空。现有字段仍是 `recall_ref`。本稿不规定这个集合怎么写进该字段，以后的实现也不得先把 `recall_ref` 写成单个 id。`rust/crates/agent24-store/migrations/0021_run_message_origins.sql` 的 CHECK 只有 HostRecall、UserPrompt、SessionHistory。ADR 里的 HostSystem、Assistant、ToolResult 不是本片的存储 kind。空字符串 `recall_ref` 的既有拒绝不变。本稿不改读取。

## 2. 当前生产快照

下面的顺序来自该基线的 `rust/crates/agent24-agent/src/lib.rs`。同一次 `append_run_messages_tx` 只写 `run_messages`，不写 `run_message_origins`。这些行今天的来源读取都是 Missing。本稿不改这个文件。

1. 可选 recall。`memory.recall` 只在 assertion id 集合非空时返回块。该块的 role 是 user。这个集合是预算循环里收集的 `hit.assertion.id`，不是单独一个 id。
2. 可选暂停通知。本轮提示是显式记住，且个人记忆处于暂停时，宿主插入 role 为 system 的 `retain::PAUSED_WRITE_NOTICE`。正文是 `记忆已暂停：本条不会被记住，请如实告知用户`。
3. session prior context。`session_context` 读取宿主自己的 session log。尾部可以含 user、assistant 和 tool。有摘要时，宿主把一条 system 放在这段前面，正文以 `Summary of earlier conversation:` 开头。这一段是 D7c，不是本轮运行稍后新写入的 assistant 或 tool 行。
4. 本轮 user prompt。role 是 user，正文是 `run.input.prompt`。

这次调用之后，生产代码仍用另一次 `tag_run_source` 给本轮 user prompt 打 K1 标签。那一次不是来源写入。本稿不把两次调用收成同一事务。

同一次快照之外，运行循环经 `persist_message` 调用 `append_run_message`，写入 assistant 与 tool 行。其中包含工具执行前落盘的 assistant `tool_calls` 行。审批冷恢复重放这些已落盘的行；线程里没有这个未完成的 assistant tool_calls 悬挂点时，冷恢复会中止。活着的审批唤醒不走 `drive_resume`。这些行属于 D7f。本稿不改这些函数。

## 3. 批准前后

clestons 在本文件的精确 head 上 APPROVE 之前，不切换写点，也不派 H03。这个 APPROVE 也不关闭 R05-1 的 D7，也不派 H03。它只允许下一片处理本次快照。

APPROVE 之后的下一片不是把整批快照改交给 `append_originated_run_messages`。`OriginatedRunMessage.origin` 是必填的 `MessageOrigin`，暂停通知按 D7d 必须保持无来源行，所以它进不了这个函数。下一片要在同一次 `BEGIN IMMEDIATE` 里允许恰好这一类行不写 `run_message_origins`。这一类只能由推入该行时设置的类型化标记识别。它不是任意行的 `Option` 或 None，也不是按 role 或通知正文做匹配。快照里其余已在 D7a、D7b、D7c 分类的行仍必须带 kind。这句话不覆盖 D7f 的运行中 assistant 与 tool 行，那些行不进入这一片。读出来的暂停通知继续是 Missing。不得把暂停通知标成已有三种 kind，不得在这一片加 HostSystem 迁移，也不得把它拆出本次快照。那一片仍不改发送路径。SQL NULL 的 HostRecall 拒发仍留给以后的 H03。

在该 APPROVE 之前，不得改 `request_messages_for`，不得改发送路径，不得写新建会话回执，不得移动审批 grant。

adr-s9 仍只挡住 P1-02c 和 purge。没有 `docs/adr/ADR-MEMORY-P1-LIFECYCLE-AND-PURGE.md` 第 9 节的接受或修订记录时，本稿不关闭 adr-s9。本稿的 APPROVE 也不代替该记录。本片不改 #794。

## 4. 本片边界

- 只新增本稿。
- 不把 R05 从 DRAFT 改成已批准。
- 不关闭 R05-1 的 D7。
- 不勾 PLAN 的 R02、R05 或 H03。
- 不改 R05-1，不改 ADR 状态，不改 migrations、Rust、协议或 OpenAPI。
- 不加迁移，不新增 HostSystem，不给 D7f 选定 CHECK 字符串。
- 不切换 `append_originated_run_messages`，不实现 H03，不开 H04。
- 不改 `request_messages_for`，不改发送路径，不改读取。
- 不启动 purge，不做 P1-02c，不改 #794。
- 本稿不代外审打勾。
