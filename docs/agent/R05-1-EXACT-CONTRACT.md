# R05-1：来源契约精确版本

本稿不代外审打勾。R05 仍是 DRAFT。本稿不开 PR。本 PR 指随后以分支 `ab/memory-p1-40-r05-exact` 的精确 head 打开的评审。clestons 对该 head 给出 APPROVE 之前，不得把 `docs/adr/ADR-MEMORY-RECALL-MESSAGE-ORIGINS.md` 改成已批准，也不得派 H03 消费者。

基线是 `origin/ab/memory-p1` 的 `c9a19532675ceee98a7fe1bfbb9f503c000fad92`，即合并 #1168 的提交。协调者把 D1 到 D4 当作存储原子的实现策略。那不是外审批准。#1162 合并的是 R05-0 外审包，那次 APPROVE 不是本稿的批准。#1168 只落地存储原子，也不批准本稿。

本稿只新增这一份文件。不改 PLAN 勾选，不改 ADR 状态，不改 Rust、migrations、OpenAPI 或协议。

## 1. 过期句子

`docs/agent/R05-0-SOURCE-CONTRACT-REVIEW-PACKET.md` 第 4 节写下一片才是存储原子。该句已过期。存储原子已按协调者后续命令落地。实现 head 是 `54bafa8010ba3d163df4c967188ddeb07888a60f`。#1168 的 merge 是 `c9a19532675ceee98a7fe1bfbb9f503c000fad92`。表是 `run_message_origins`。迁移文件是 `rust/crates/agent24-store/migrations/0021_run_message_origins.sql`。本稿不重开 D5，也不再加迁移。

#1152 同秒审计 ID 已合并。merge 是 `7dd6f08f2e979e2e4e54caa23e4c3834cd97fc5d`。`docs/agent/PLAN-MEMORY-P1.md` 里「修复未完成」是过期文字。本稿不改那份计划。

#1166 的标题是 daemon 启动恢复回归。#1167 的标题是 HTTP pause 不改已有 run snapshot。两片都已合并。本稿不再写这些测试。

ADR 第 2 节仍写表名和迁移号待批准。该句相对 #1168 已过期。本稿不编辑 ADR。PLAN 里 store 停在 `0018` 的句子同样过期。已落地的迁移文件是 `0021`。

## 2. 请外审按条接受、收窄或拒绝

沿用 D1 到 D4，不放宽。外审可以收窄 D1。建议与 ADR 第 4 节一致，把 D1 到 D4 当作兼容策略。下表外审列保持待外审。本稿不代外审打勾。

| ID | 精确文本 | 不接受则不能做 | 外审 |
| --- | --- | --- | --- |
| D1 | 只收紧缺映射、坏来源，或无法证明来源闭包的 run。已有可信来源的审批恢复不得一律停发。 | 不得把已有可信来源的审批恢复改成一律停发。 | 待外审 |
| D2 | 历史、原文和 snapshot 保留。只停自动恢复和自动发送，并给出新建会话回执。不得删历史。回执未实现，属 H03，本 PR 不写代码。 | 不得删历史来换安全。 | 待外审 |
| D3 | 不自动复制旧 recall 或待执行工具。旧审批 grant 不进新 run。本 PR 不移动审批 grant。 | 不得把隔离实现成静默重放。 | 待外审 |
| D4 | 禁止用 marker、位置 0、role 或同文回填来源。禁止把旧 snapshot 标成 NoRecall。无损升级旧 run 必须另有可信写点凭证或迁移证明。本稿不提供该证明。 | 不得因迁移成本退回文本推断。 | 待外审 |

D5 不重开。表、迁移号，以及与 K1 `run_source_tags` 正交，都已经落地。外审若要换表或再加迁移，应拒绝本稿并另开一片。本 PR 不写该代码。

D6 仍是批准后的发送验收，不是本稿的测试结果。来源矩阵里标成未实现的格子都还没绿。不得用本稿宣称 P02 红证已绿。

## 3. 已落地的读取语义

下列事实只描述存储读写。它们不是发送路径，也不是 H03。

读取类型是 `MessageOriginLookup`。`Present` 只在 schema、kind 和 digest 都通过时返回。缺行不是 `Present`。

| 事实 | 结果 |
| --- | --- |
| kind 只有 HostRecall、UserPrompt、SessionHistory | 存储已落地。`0021` 的 CHECK 只允许这三种。 |
| 缺行 | 存储已落地。`read_run_message_origin` 返回 `Missing`。不是 NoRecall。该读不写 `run_source_tags`。 |
| digest 不符 | 存储已落地。返回 `Unreadable`，不是 `Present`。 |
| schema_version 不是 1，或 kind 坏 | 存储已落地。返回 `Unreadable`，不是 `Present`。 |
| 空字符串 recall_ref | 存储已落地。`append_originated_run_messages` 拒绝写入。读到空字符串则是 `Unreadable`。 |
| UserPrompt 或 SessionHistory 带 recall_ref | 存储已落地。写入接口不带 recall_ref。行上若带了，读取是 `Unreadable`。 |
| HostRecall 的 recall_ref 为 SQL NULL | 存储已落地。NULL 可以写入并读成 HostRecall。这是读取事实，不是放宽 D4，也不是批准缺引用的 HostRecall 可以发送。空字符串仍然拒绝。发送侧如何对待 NULL 引用未实现，属 H03，本 PR 不写代码。 |
| 无 backfill | 存储已落地。`0021` 不给旧 run 插来源行。 |
| 旧 append | 存储已落地。`append_run_message` 与 `append_run_messages_tx` 只写 `run_messages`，不默认来源。 |
| 新写入 | 存储已落地。`append_originated_run_messages` 在一个 `BEGIN IMMEDIATE` 里写消息和来源。失败则整批 rollback。这次写入不插入 K1 标签。 |

digest 绑定 schema_version、role、content、tool_calls 和 tool_call_id。content 为 null 不等于空字符串。存储层不做 trim，也不做 Unicode 归一化。digest 不符就是 `Unreadable`。

## 4. 来源矩阵

每一格只用两个标记之一。「存储已落地」只表示 `run_message_origins` 的读写。「未实现，属 H03，本 PR 不写代码」表示发送、裁剪、回执或冷恢复消费者。冷恢复不靠进程内缓存这一条标成未实现。

| 行 | 存储里有什么 | provider 可以收到什么 | 必须裁掉什么 | snapshot 是否保持 |
| --- | --- | --- | --- | --- |
| HostRecall | 存储已落地。一行 kind 为 HostRecall。来源插入不改该 seq 的消息正文。 | 未实现，属 H03，本 PR 不写代码。 | 未实现，属 H03，本 PR 不写代码。 | 未实现，属 H03，本 PR 不写代码。 |
| UserPrompt | 存储已落地。一行 kind 为 UserPrompt，recall_ref 为 SQL NULL。来源插入不改消息正文。 | 未实现，属 H03，本 PR 不写代码。 | 未实现，属 H03，本 PR 不写代码。 | 未实现，属 H03，本 PR 不写代码。 |
| SessionHistory | 存储已落地。一行 kind 为 SessionHistory，recall_ref 为 SQL NULL。来源插入不改消息正文。 | 未实现，属 H03，本 PR 不写代码。 | 未实现，属 H03，本 PR 不写代码。 | 未实现，属 H03，本 PR 不写代码。 |
| 同文三来源 | 存储已落地。相同正文可以分成三行，kind 分别是 HostRecall、UserPrompt 和 SessionHistory。digest 按实例字段计算，不按正文是否相同合并来源。 | 未实现，属 H03，本 PR 不写代码。 | 未实现，属 H03，本 PR 不写代码。 | 未实现，属 H03，本 PR 不写代码。 |
| 缺行 | 存储已落地。结果是 Missing，不是 NoRecall，不写 `run_source_tags`。 | 未实现，属 H03，本 PR 不写代码。 | 未实现，属 H03，本 PR 不写代码。 | 存储已落地。缺行读取不删消息，也不把旧 snapshot 标成 NoRecall。 |
| 坏 digest | 存储已落地。结果是 Unreadable，不是 Present。读取不改消息正文。 | 未实现，属 H03，本 PR 不写代码。 | 未实现，属 H03，本 PR 不写代码。 | 存储已落地。坏 digest 的读取不改 snapshot 正文。 |
| 冷恢复 | 存储已落地。来源行在库里。 | 未实现，属 H03，本 PR 不写代码。冷恢复不靠进程内缓存。 | 未实现，属 H03，本 PR 不写代码。 | 未实现，属 H03，本 PR 不写代码。冷恢复不靠进程内缓存。 |

## 5. 发送屏障

本 PR 不开 H04。下表只记下已观察边界和仍未证明的项。

| 项 | 结论 |
| --- | --- |
| P03 已观察的撤回、pause、fallback、qualification、no-session | 不是 H04 违约。已构造请求按现行边界继续发送。 |
| 首次发送前的线性化 | 仍未证明。本 PR 不开 H04。 |
| #1166 与 #1167 | 已合并。本稿不重写 daemon 启动恢复，也不重写一次 HTTP pause 不改 snapshot。 |

## 6. 批准之后才允许的下一片

clestons 在本 PR 精确 head 上 APPROVE 之后，下一片代码才是 H03 的第一个消费者。本 PR 不实现它。

在那一片之前，不得改 `request_messages_for`，不得改发送路径，不得写新建会话回执，不得移动审批 grant。

adr-s9 仍开。它问 `docs/adr/ADR-MEMORY-P1-LIFECYCLE-AND-PURGE.md` 第 9 节有没有接受或修订记录。没有该记录时，adr-s9 仍挡住 P1-02c、purge 和 #794。本稿不关闭 adr-s9。本稿的 APPROVE 也不代替该记录。

## 7. 本片边界

- 只新增本稿。
- 不把 R05 从 DRAFT 改成已批准。
- 不勾 PLAN 的 R02、R05 或 H03。
- 不改 migrations、Rust、协议或 OpenAPI。
- 不重开 D5，不加迁移。
- 不实现 H03，不开 H04。
- 不启动 purge，不做 P1-02c，不碰 #794。
- 本稿不代外审打勾。
