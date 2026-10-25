# K1-6b.5 逐出口矩阵

> **依据**：[ADR-K1-02](../adr/ADR-K1-02-session-dataflow-security-modes.md) §2.4（五类出口）、§2.5（撤除 `has_remote` 过渡门的条件）、§5 第 1–11 条、§6 第 5 条；跟踪 [#735](https://github.com/iDoris-ai/Agent24/issues/735)。
> **状态**：6b.5a 建表。表里写的是**现有**测试；「缺」列出后续切片（6b.5b–5e）要补的测试，每补一片就在这里勾掉。
> **盘点基准**：K1-6b.4 栈顶（#1204）。测试按 `文件` + 函数名引用，不写行号。

## 0. 先说清楚的事实

1. **生产里没有获准云处理。**
   - 所有 `SourceRef` 构造器写死 `mode: LocalOnly`、`policy_version: 0`、`authorization_ref: None`（`agent24-store/src/source_policy.rs`）。
   - 生产代码不调用 `EgressResource::cloud_authorized`、`ToolContext::with_egress_policy` 或 `ToolRegistry::with_egress_gate`，唯一的 gate 是 `deny_all`。
   - 所以 (c) 列只能在 domain、router、http_fetch、MCP 层用测试 gate 做。产品路径上，每个消费方要有一条「模式未支持 → unavailable，零外发」的测试，不能记成云授权正例已通过。
2. **生产里远端模型对所有资料都不可达，Ordinary 也一样。**
   - 模型调用都走 `complete`/`complete_served`，模型发现走 `models_detailed`，底下都是 `deny_all` 加空资源（`agent24-models/src/router.rs`）。
   - `complete_with_egress` 只有 router 测试在调用。
3. **domain 先于 gate 拒绝**（`agent24-domain/src/egress.rs` `EgressRequest::authorize`）。以下情况都在问 gate 之前就拒绝：
   - 请求不是远端；
   - 目的地未知或为空；
   - 资源为空；
   - 任何资源是 LocalOnly；
   - 资源缺 revision、`authorization_ref`，或 `policy_version` 为 0。
4. **工具侧**：`ToolContext::authorize_egress_if_restricted`（`agent24-tools/src/lib.rs`）按三步判定：
   - 政策不可用 → 拒绝；
   - 没有非 Ordinary 资源 → 放行；
   - 其余交给 gate，没装 gate → 拒绝。
   `tool_context_for`（`agent24-agent/src/lib.rs`）把 run 的非 Ordinary 来源转成资源；快照为空或读失败时，标记为政策不可用。

## 1. 列

| 列 | 含义 |
|---|---|
| (a) | LocalOnly 或受限、未知来源的负例：远端桩零请求，或工具在产生副作用前被拒 |
| (b) | 门故障负例：政策读取失败、gate 出错、tier 或目的地未知时，一律失败即关闭 |
| (c) | 云授权精确三元组：正例，以及逐项不匹配的负例（资料、用途、目的地、授权代次） |
| (d) | Ordinary 不回归（§5 第 9 条） |

## 2. 模型类

| 消费方 | (a) | (b) | (c) | (d) |
|---|---|---|---|---|
| **Router** | `router.rs` `local_only_never_falls_back_to_remote`、`local_only_with_cooled_down_local_still_never_uses_remote`、`local_only_uses_lora_as_a_local_tier` | `router.rs` `remote_provider_is_not_called_without_a_host_egress_gate`、`denied_remote_falls_back_to_healthy_local_provider`、`tier_parse_defaults_unknown_to_remote`；**缺**：provider 不报目的地（`outbound_destination()` 为 `None`）时零请求 | 正例：`router.rs` `complex_task_prefers_remote`（放行 gate）。负例只在 domain 层：`egress.rs` `cloud_grant_must_match_resource_purpose_destination_and_generation`（没有换 `resource_id`、`authorization_ref`、`policy_version`）、`an_authorized_request_is_rechecked_against_the_live_gate_after_revocation`；**缺**：router 层用精确匹配 gate 的逐项负例 | `router.rs` `simple_task_prefers_local`、`tier_order_respects_privacy_and_complexity` |
| **run_loop**（`RunManager::execute`） | `lib.rs` `resumed_thread_cannot_rebuild_remote_permission_from_default_profile`（远端桩，唯一一条） | `lib.rs` `source_tag_write_failure_stops_before_any_model_call`、`a_recall_tag_that_cannot_be_written_stops_the_run_before_the_model`、`a_session_floor_that_cannot_be_written_stops_the_run_before_the_model`、`a_schedule_origin_that_cannot_be_read_stops_the_run_before_the_model`、`a_run_whose_own_sources_cannot_be_read_stops_before_the_model`（都用本地 provider）；**缺**：`task_profile_for_run` 读失败分支配远端桩（无会话、无记忆的 run） | 不适用：只调 `router.complete` | `lib.rs` `run_completes_with_full_event_sequence`、`ordinary_sources_of_any_kind_do_not_restrict_tool_egress` |
| **REST /api/v1/chat** | **缺**：远端桩零请求（现有 routes 测试都是本地 router） | `session_memory/chat.rs` `a_chat_turn_whose_session_floor_cannot_be_written_fails_before_the_model`；`lib.rs` `a_chat_prelude_whose_floor_cannot_be_written_fails` | 不适用 | `routes.rs` `post_chat_records_a_timing_row_on_success`；`chat.rs` `explicit_remember_over_chat_stores_a_qualified_assertion` |
| **子 agent（explore）** | `subagent.rs` `delegated_call_inherits_persisted_local_only_policy` | **缺**：父 run 快照读失败、`store: None` 两个分支 | 不适用 | `subagent.rs` `a_direct_answer_returns_immediately`、`it_reads_a_file_then_answers` |
| **会话压缩** | `session_memory_tests.rs` `local_only_run_compaction_never_falls_back_to_remote` | 不适用：写死 LocalOnly，不读政策 | 不适用 | `session_memory/mod.rs` `successful_compaction_replays_every_original_message_after_daemon_restart` |
| **模块模型回调** `_a24/model/complete` | `model_callback.rs` `remote_provider_stays_closed_without_a_host_egress_policy`；`me4_model_blackbox.rs` `model_complete_blackbox_round_trip`（远端桩零请求） | **计划/代码差异**：回调不读父 run 的来源（见 §6 第 2 条），所以没有可失败的政策读取 | 无接口可表达 | `model_callback.rs` `a_successful_call_broadcasts_exactly_one_model_call_event_with_no_content` |
| **Guardian**（默认关闭，`A24_GUARDIAN`） | **缺**：`ModelRiskAssessor` 接 router 加远端桩（现有测试都是 stub assessor） | `guardian.rs` `assessor_error_fails_closed_to_escalation`（只测 stub） | 不适用 | `guardian.rs` `low_risk_auto_approves` |
| **模型发现** | **缺**：远端桩的 `models()` 计数为 0（`a_partial_sweep_is_incomplete_even_though_it_returned_models` 分不清「被拒」和「调了但失败」） | **缺**：目的地未知、gate 出错 | **缺**：`ModelDiscovery` 用途 | `routes.rs` `get_models_includes_the_daemons_default_model_field` |

## 3. 网络类

| 消费方 | (a) | (b) | (c) | (d) |
|---|---|---|---|---|
| **http_fetch** | **缺**：LocalOnly 资源 → 拒绝、DNS 零次 | `net.rs` `denied_fetch_does_not_invoke_dns_resolver`；**缺**：`with_egress_policy_unavailable()` | 只有部分正例：`net.rs` `fetches_a_fixture_body` 用的 gate 无条件放行，不看三元组；`lib.rs` `model_fetches_a_url_through_http_fetch` 的 `FixtureEgressGate` 精确比对资源、用途和代次，但目的地只比前缀 `http://127.0.0.1:`（任何端口都放行）。**缺**：精确目的地的正例，以及逐项不匹配的负例 | 部分：`net.rs` `ordinary_fetch_does_not_require_egress_policy_before_dns`（只测域名主机的 DNS 前一步）；`tools/src/lib.rs` `ordinary_http_fetch_is_advertised_without_egress_policy`（只测通告）。**缺**：IP 字面量主机分支在 Ordinary、无 gate 时放行；Ordinary 下端到端取回成功 |
| **MCP client 工具** | **缺** | **缺** | **缺**（见下方注） | **缺** |

另有 SSRF 测试：`net.rs` 里的 `public_ip_classification`、`private_targets_are_denied`、`redirects_are_returned_not_followed`。

> 注：MCP 的门在代码里存在（`agent24-mcp/src/lib.rs`，用途 `McpTool`，目的地 `mcp:{server}:{tool}`）。但传输是 stdio 子进程，目的地只是一个标签，不是子进程实际的网络端点。模块工具同理。放行 gate 加 CloudAuthorized 会让调用通过，可子进程的网络其实圈不住（§2.4 第 3 条）。所以 (c) 只在测试 gate 上做，并在这里写明它不证明子进程不外发。

## 4. 模块与进程类

| 消费方 | (a) | (b) | (c) | (d) |
|---|---|---|---|---|
| **模块工具** | `module_tool_runtime.rs` `restricted_module_tool_is_denied_when_egress_gate_denies` | **缺**：政策不可用 → 拒绝，运行时零调用 | **缺** | `module_tool_runtime.rs` `granted_fake_module_is_advertised_and_callable` |
| **shell_exec** | `local.rs` `shell_exec_fails_closed_when_child_network_use_cannot_be_bounded`、`local_only_shell_exec_is_denied_even_by_permissive_egress_gate`；**缺**：零副作用断言（两条都用 `/bin/echo`） | `local.rs` `shell_exec_is_denied_when_source_policy_lookup_failed`；`lib.rs` `restricted_or_unknown_sources_of_any_kind_restrict_tool_egress`；**缺**：经 `tool_context_for` 的快照为空、读失败两个分支 | **缺**：CloudAuthorized 加放行 gate 仍被拒（目的地未知） | `local.rs` `shell_exec_runs_for_ordinary_run_without_egress_policy`、`ordinary_shell_exec_runs_after_approval_without_egress_gate`；`lib.rs` `approved_shell_exec_actually_executes` |
| **模块进程自身出网** | 没有网络沙箱。**已知限制**：只靠调用前的出口门（受限 run 一律拒） | — | — | — |

`has_remote` 过渡门（`module_tool_runtime.rs`）现有测试：
- `a_withheld_tool_called_by_name_is_refused_and_audited`
- `the_guard_refuses_before_authorization_is_looked_up`
- `a_remote_tier_added_while_running_refuses_the_next_call`
- `the_production_view_reads_the_tier_on_every_check`（tier 读不出来按远端计）

撤门见 6b.5e。

## 5. 存储外流与生命周期

| 项 | 结论 |
|---|---|
| 记忆 | 本机 SQLite，不出境。写入分类由 6b.4 覆盖：`lib.rs` 的 `a_remembered_fact_carries_the_strictest_source_of_its_run`、`a_run_whose_sources_cannot_be_read_writes_unknown_memory`。模块只能读写自己的 `_a24/memory/private/*` |
| 日志、计时、用量 | 本机，没有网络客户端 |
| 导出 | 只有 `/api/v1/module-consents/export`，交给本机 REST 调用方的是授权元数据（`server.rs` `module_consent_export_contains_metadata_only_and_revoke_broadcasts`） |
| 模块投递 | 走模块的本机代次通道，载荷不带会话内容 |
| 续轮（会话下限，6b.4c） | (a)：`lib.rs` `a_restricted_earlier_turn_restricts_the_later_turns_of_its_session`、`prior_session_context_without_a_recorded_floor_is_unknown`（都断言工具出口被拒）<br>分类传播（不算 (a)，只断言分类）：`the_chat_prelude_classifies_client_history_by_the_session_floor`<br>(b)：见 run_loop 一行<br>(d)：`an_ordinary_session_stays_ordinary_across_turns`<br>残余风险：会话已有下限时，客户端搬来的历史按这条下限计（#1203 G1，David 接受） |
| self-wake（6b.4d） | (a)：`lib.rs` `a_self_wake_run_carries_the_classification_recorded_at_creation`（断言工具出口被拒）<br>分类传播（不算 (a)）：`lib.rs` `a_restricted_self_wake_raises_its_session_floor_for_later_turns`（只断言写入记忆的分类）；`self_wake.rs` `a_wake_records_the_strictest_source_of_the_run_that_created_it`（只断言记下的分类）<br>(b)：`a_schedule_origin_that_cannot_be_read_stops_the_run_before_the_model`、`a_wake_whose_sources_cannot_be_recorded_is_not_created`<br>(d)：`a_user_schedule_run_has_no_schedule_origin` |
| 恢复停放的 run | (a) 只有直接调 `run_loop` 的那一条。**缺**：带受限来源的 run 经 `drive_resume`/`restore_pending_approvals` 恢复后，shell_exec 被拒、零 spawn。恢复时不重新合并下限（#1204 G4，David 定） |
| 模型 override | 只换 model id：`ModelRouter::route` 只看 tier 顺序和健康状况，不读 `req.model`。**缺**：钉住「override 不改路由 tier」的测试 |

## 6. 不在矩阵内、或计划与代码不一致的

1. **ADR 列出、但目前没有生产代码的消费方**：
   - 邮件和 webhook 投递：`DeliveryTarget` 只存不发；
   - 远端 embedding：只有本地 `HashEmbedder`；
   - 遥测、崩溃报告、同步；
   - 流式请求：`"stream": false` 写死；
   - `SelectedMaterial` 入口（ID-2 未实现）。
   这些不写成「已覆盖」。接入时必须同时补齐本表的四列。
2. **模块模型回调不继承父 run 的来源集合**（§2.2：「模块 callback … 继承父操作的来源集合」）。`model_callback.rs` 不读 run 的政策，隐私只看 manifest 里的 `ModelAccess`。目前远端被 `deny_all` 拦住，所以没有泄露。但撤 `has_remote` 过渡门、或者接入获准云处理之前，必须先接上来源继承。
3. **agent24d 作为 stdio MCP server，以及 ACP 桥**（`agent24-mcp/src/server.rs`、`agent24-cli/src/acp.rs`）：
   - `agent24_run` 把 run 的最终输出交给外部 MCP 对端；
   - `agent24_list_runs` 返回所有 run 的 prompt 和输出；
   - 不按来源分类过滤。
   对端可能把内容转发给它自己的云模型。§2.4 第 2 条点名了「MCP client/server 工具」。
   **David 2026-10-25 定：视为出口，按分类过滤**（Claude、Codex、Fable 三方都选这一项）。在 6b.5c 实现，要求如下：
   - 内核权威分类为 Ordinary 的 run，照常返回文本。
   - Restricted、Unknown 或分类读不出来时，只返回状态和固定提示；`agent24_list_runs` 不给这些 run 的 prompt 和输出。
   - 错误消息同样过滤。
   - 分类可能在 run 中途因召回而收紧，所以 ACP 的流式增量要等分类确定后再放行，或者每段增量按当时的分类判定，不能先流出去再检查。
   - 内核要把 run 的分类暴露给桥（一个字段或一个按分类过滤的端点）。
   - 测试要覆盖：分类中途变化、分类缺失、受限内容不外泄。
4. **Hyphae `/comm/send`**：只能由用户经 REST 直接调用，没有 Tool 包装，也不带 run 资料。视为用户直接操作，不在 agent 出口矩阵内。

## 7. 后续切片

| 片 | 补什么 |
|---|---|
| 6b.5b 模型类 | REST chat 远端桩 (a)；Guardian 接 router 加远端桩 (a)；子 agent 两个读失败分支 (b)；provider 目的地未知 (b)；模型发现 (a)(b)(c)；router 层三元组逐项负例 (c)；`task_profile_for_run` 读失败加远端桩 (b) |
| 6b.5c 网络类 | http_fetch 的 LocalOnly、政策不可用、精确目的地正例和逐项不匹配负例，以及 Ordinary 下 IP 字面量分支和端到端取回；MCP client 的 (a)(b)(d)，以及测试 gate 上的 (c)；MCP server 和 ACP 桥按分类过滤（§6 第 3 条，可能要拆成单独一片） |
| 6b.5d 模块、进程、生命周期 | 模块工具 (b)(c)；shell_exec 零副作用、CloudAuthorized 仍被拒、`tool_context_for` 两个分支；恢复时受限来源的出口测试；模型 override 测试 |
| 6b.5e 撤 `has_remote` 过渡门 | 前提：本表全绿、§6 第 2 条已接、K1-6a 已验收；附 #708 macOS 证据。**撤门要 David 确认** |
