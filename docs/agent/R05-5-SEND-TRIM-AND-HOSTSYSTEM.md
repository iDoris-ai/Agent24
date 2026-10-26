# R05-5：发送副本按来源 kind 整形，与 HostSystem 来源

基线是 `origin/ab/memory-p1` 的 tip `b9e4eb5f059e1d406e562cfbd4ec368ef5b5fc1e`，即 PR #1205 的 merge。本稿是契约，不写代码。评审对象是分支 `ab/memory-p1-49-r05-5-send-trim` 的精确 head。clestons 对该 head 的 APPROVE 只接受本稿条目。本稿不勾 PLAN，不改 R05-1 到 R05-4 的原文。

本稿改动两个文件：新增本稿，修订 `docs/adr/ADR-MEMORY-RECALL-MESSAGE-ORIGINS.md`。不改 Rust、migrations、OpenAPI、协议。

## 1. 现状（已核对事实）

`rust/crates/agent24-agent/src/lib.rs::request_messages_for` 仍只按位置 0 的消息判定宿主 recall。判据是该消息 role 为 user，正文以 `RECALL_PREFIX` 开头，且以 `RECALL_END_MARKER` 结尾。它不读 `run_message_origins` 的 kind。命中后，它用 `memory.active_ids()` 的结果交给同文件的 `prune_stale_recall_message`，按行裁掉已失效的 id；全部失效则从发送副本移除整条；`active_ids()` 失败则移除整条并记 warn。

因此用户自己输入的、恰好同形的整段文字，仍会被当成 recall 裁剪或丢弃。这是 P02 探针报告里的原始缺陷。#1197 和 #1205 没有修它。#1197 只加了拒发门 `persisted_origins_allow_send`。#1205 只加了 provider 返回 role 的门。这两道门不改变放行之后的整形。R05-4 第 2 节已写明这一点。

另有三条已核对的事实，本稿后文要用：

- `request_messages_for` 的入参是 `&[Msg]` 和 `run_id`，没有每条消息的 seq。`run_loop` 每轮迭代都在它之前调用 `persisted_origins_allow_send`，该函数按 `list_run_messages` 的 seq 逐条调 `read_run_message_origin`。
- `execute` 里宿主 recall 只在快照的第一行写入：先 `rows.push(originated(&recalled, MessageOrigin::HostRecall { .. }))`，再是暂停通知，再是 prior context，最后是本轮 prompt。`recall_ref` 由 `canonical_recall_ref` 写成字典序紧凑 JSON 数组。运行中新写的行只有 Assistant 与 ToolResult（`persist_message`，migration 0022）。所以当前生产写点只会在位置 0 写 HostRecall。
- 暂停通知在 `execute` 里以 `SnapshotRunMessage::PauseNotice` 写入，`append_snapshot_run_messages` 对这个变体只插 `run_messages`，不插来源行。读出来是 Missing，被 `origin_allows_send` 拒发。`chat_memory_prelude` 里另有一处 `Msg::system(retain::PAUSED_WRITE_NOTICE)`，它只进 prelude，不经 `run_loop`，不落 `run_messages`。

## 2. H03-b 发送副本语义

本节填 R05-1 第 4 节的列「provider 可以收到什么」「必须裁掉什么」「snapshot 是否保持」。发送前已有 `persisted_origins_allow_send` 门。本节只描述该门放行之后的整形。门的语义不变：Missing、Unreadable、读错误、`HostRecall { recall_ref: None }` 仍整次拒发。

规则：

1. 是否宿主 recall，只看该 seq 已落盘来源的 kind 是否为 HostRecall。不再看文本标记。
2. `RECALL_PREFIX` 与 `RECALL_END_MARKER` 只用于在已证明的 HostRecall 正文里定位要裁的行。
3. HostRecall 不限位置 0，按 seq 逐条处理。
4. 其余 kind（UserPrompt、SessionHistory、Assistant、ToolResult，以及第 3 节的 HostSystem）逐字发送，不裁剪，不移除。
5. 裁剪只作用于发送副本。库里的消息正文、来源行、snapshot 都不改。
6. 整形完成后才调用 `normalize_for_provider`（与 ADR 第 3 节第 6 条一致）。

| 行 | provider 可以收到什么 | 必须裁掉什么 | snapshot 是否保持 |
| --- | --- | --- | --- |
| HostRecall（Present，recall_ref 非 NULL） | 正文中 id 仍在当前有效集合内的行，连同头和结束标记。 | 沿用 `prune_stale_recall_message` 的逐行规则，裁掉 id 已失效的行。全部失效则整条从发送副本移除。读取有效 id 集合失败则整条移除并记 warn（失败关闭）。 | 保持。库里正文与来源行不变。 |
| UserPrompt | 逐字发送。 | 无。永不裁剪，永不移除，即使正文与 recall 块完全同形。 | 保持。 |
| SessionHistory | 逐字发送，role 为 user、assistant、tool、system 都一样。 | 无。永不裁剪，永不移除，即使正文与 recall 块完全同形。 | 保持。 |
| 同文三来源 | 同一段正文分成 HostRecall、UserPrompt、SessionHistory 三行时，三行各按自己的 kind 处理：HostRecall 行按上面规则裁，另两行逐字到达。digest 相同不影响处理。 | 只有 HostRecall 那一行可能被裁。 | 保持，三行都不变。 |
| 冷恢复 | 与同进程相同。每次发送都从库读消息与来源（`list_run_messages` 加 `read_run_message_origin`），不依赖进程内缓存。进程重启后经 `resume_run`、`drive_resume`、`run_loop` 得到与同进程相同的发送副本。 | 同上，按行里的 kind 裁，不按进程内状态。 | 保持。`drive_resume` 的 `thread_to_messages` 只是重放，不改库。 |

本节明确不在本片：

- G1。把 `recall_ref` 解析成 id 数组并对照账本做成员证明。本片裁剪仍以正文行里的 id 对照当前有效集合。`recall_ref` 在本片只用来满足拒发门，不参与裁剪。
- G2、G4、G5。
- K1 与来源同事务。生产路径仍是 `append_snapshot_run_messages`（消息与来源同事务）之后独立 `tag_run_source`。
- 把消息 seq 传进 `request_messages_for` 的具体做法。4b 必须让每条消息的 kind 来自库里该 seq 的来源行，不得由内存下标推断。内存列表与 seq 是否逐位对齐，由 4b 自己核对并写测试，本稿不断言。

### 先验测试（red 到 green）

测试写在 `agent24-agent` 里，用 provider spy 捕获 `CompletionRequest.messages`。

| 测试名（建议） | 断言要点 |
| --- | --- |
| `send_copy_keeps_user_prompt_shaped_like_recall` | UserPrompt 正文是完整同形的 recall 块（含已撤回 id）。provider spy 收到的这一条与库里正文逐字节相等。当前代码在 memory 存在且它位于位置 0 时会裁它，所以此测先红。 |
| `send_copy_keeps_session_history_shaped_like_recall` | SessionHistory 行正文同形。逐字到达 provider。 |
| `send_copy_trims_revoked_ids_from_real_host_recall` | 真 HostRecall 中被撤回 id 的行被裁，有效 id 的行保留。 |
| `send_copy_drops_host_recall_when_all_ids_revoked` | 全部撤回后该条不在 provider 请求里。 |
| `send_copy_drops_host_recall_when_active_ids_unreadable` | `active_ids()` 失败：HostRecall 整条消失，同一请求里的 UserPrompt 仍在，且有 warn。 |
| `send_copy_three_same_text_origins_each_by_kind` | 同文三行：只有 HostRecall 行被裁，另两行逐字到达。 |
| `send_copy_trim_leaves_snapshot_bytes_unchanged` | 裁剪前后读 `list_run_messages` 与 `read_run_message_origin`：正文、kind、digest、recall_ref 逐字节不变。 |
| `resume_after_restart_sends_same_copy_as_live` | 新建一个 manager 模拟 daemon 重启，走 `resume_run`。provider spy 收到的发送副本与同进程审批唤醒路径逐条相等。 |
| `send_copy_handles_host_recall_not_at_position_zero` | 位置非 0 的 HostRecall 也被处理。当前写点只在位置 0 写 HostRecall，测试用直接构造的来源行覆盖（经 `append_originated_run_messages` 写入）。 |

## 3. HostSystem 来源

产品负责人 jason 于 2026-10-26 拍板。

1. 推翻 R05-2 D7d 中「暂停通知没有现成 kind，保持无来源行，读取继续是 Missing」这一条，以及 R05-4 第 1 节表里「暂停通知是有类型的无来源行」的说法。D7d 其余文字不再适用于暂停通知；R05-2 原文不改。
2. 新增来源 kind `HostSystem`，用于宿主生成的系统性通知。首个且唯一的写点是记忆暂停时 `execute` 写入的暂停通知。`recall_ref` 为 SQL NULL。非 NULL 在读取时不得变成 Present。digest 规则不变，仍绑定 schema_version、role、content、tool_calls、tool_call_id。
3. 发送语义：Present 的 HostSystem 逐字发送，不裁剪，不是 HostRecall。
4. 存储：需要一个新迁移。编号是下一个，即 `0023`（`rust/crates/agent24-store/migrations/` 现有最大编号是 `0022_run_message_origin_kinds.sql`）。做法照 0022：建 `run_message_origins_next`，CHECK 加入 `HostSystem`，复制旧行，删旧表，改名。不改 0021、0022。不回填既有无来源的暂停通知行。
5. 旧 run 没有来源行：继续停发，不回填。这是 jason 2026-10-26 的确认。已存在的无来源暂停通知行仍读成 Missing，相关旧 run 仍按 #1197 以 `source_unproven` 拒发。
6. 写点变化（属 4a）：暂停通知改走 `Originated`，带 `MessageOrigin::HostSystem`。`SnapshotRunMessage::PauseNotice` 变体在 4a 里是删除还是保留，由 4a 决定，但生产路径不得再写出无来源的暂停通知。`chat_memory_prelude` 的那一处不落库，不在本节范围。
7. 用户可见效果：记忆暂停后，用户说「记住 X」，新 run 不再以 `source_unproven` 失败。暂停通知照常进入发送副本。

不变的边界：模型、tool、客户端 body 不得指定 HostSystem。REST 的 role=system 或自报 source 不是宿主构造证明。HostSystem 不授予工具权限，也不扩大 K1 路由。

测试要点：

| 测试名（建议） | 断言要点 |
| --- | --- |
| `paused_remember_run_completes_and_provider_gets_notice` | 记忆暂停加「记住 X」的新 run 到 Completed。provider spy 收到暂停通知，正文逐字。该 run 的暂停通知行读成 Present 的 HostSystem。 |
| `legacy_unsourced_pause_notice_still_refused` | 手工插入一个无来源行的暂停通知（模拟旧 run）。run 仍以 `source_unproven` 失败，provider 未被调用，不回填。 |
| `origin_kind_check_rejects_unknown_kind` | 直接 SQL 插入未知 kind 被 CHECK 拒绝；`HostSystem` 被接受。 |
| `host_system_with_recall_ref_is_unreadable` | HostSystem 带非 NULL `recall_ref` 读成 Unreadable。 |

## 4. 下一片

批准后才派。各自独立 PR。

| 片 | 内容 |
| --- | --- |
| 4a | HostSystem：迁移 `0023`，`MessageOrigin::HostSystem` 的读写，暂停通知写点切换，第 3 节的测试。 |
| 4b | H03-b：`request_messages_for` 按 kind 整形，第 2 节的测试。 |

4a 与 4b 谁先都行，各自独立。4b 不依赖 4a：HostSystem 之前，暂停通知行仍是 Missing，被拒发门挡在整形之前；4b 对 kind 的 match 里，HostSystem 在 4a 合并后才有该变体，4b 先合时不引用它。

## 5. 本片边界

- 只新增本稿，并按需修订 ADR（状态行、基线、已落地边界、决策记录）。
- 不勾 PLAN 的 R02、R05 或 H03。
- 不改 R05-1、R05-2、R05-3、R05-4 原文。
- 不改 Rust、migrations、OpenAPI、协议。
- 不实现 G1、G2、G4、G5、K1 或冷恢复矩阵。
- 不启动 purge，不做 P1-02c，不改 #794，不开 H04，不开 release。
- 本稿不代外审打勾。
