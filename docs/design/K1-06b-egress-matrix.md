# K1-6b.5 逐出口矩阵

> **依据**：[ADR-K1-02](../adr/ADR-K1-02-session-dataflow-security-modes.md) §2.4（五类出口）、§2.5（撤除 `has_remote` 过渡门的条件）、§5 第 1–11 条、§6 第 5 条；跟踪 [#735](https://github.com/iDoris-ai/Agent24/issues/735)。
> **状态**：6b.5a 建表；6b.5b 已补模型类。表里写的是**现有**测试；「缺」列出后续切片（6b.5b–5e）要补的测试，每补一片就在这里勾掉。
> **盘点基准**：K1-6b.4 栈顶（#1204）。测试按 `文件` + 函数名引用，不写行号。

## 0. 先说清楚的事实

1. **生产里没有获准云处理。**
   - 所有 `SourceRef` 构造器写死 `mode: LocalOnly`、`policy_version: 0`、`authorization_ref: None`（`agent24-store/src/source_policy.rs`）。
   - 生产代码不调用 `EgressResource::cloud_authorized`、`ToolContext::with_egress_policy` 或 `ToolRegistry::with_egress_gate`。
   - 模型侧唯一的 gate 是 `deny_all`；工具侧生产的 `ToolRegistry` 根本不装 gate，受限 run 按下面第 4 条的「没装 gate → 拒绝」处理。
   - 所以 (c) 列只能在 domain、router、http_fetch、MCP 层用测试 gate 做。产品路径上，每个消费方要有一条「模式未支持 → unavailable，零外发」的测试，不能记成云授权正例已通过。
2. **生产里远端模型对所有资料都不可达，Ordinary 也一样。**
   - 模型调用都走 `complete`/`complete_served`，模型发现走 `models_detailed`，底下都是 `deny_all` 加空资源（`agent24-models/src/router.rs`）。模型发现有两个调用方：`GET /api/v1/models`（经 `models`），以及启动时的 `ModelCatalog::probe`（`agent24d/src/server.rs`，有模块声明模型时才清点）。
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
| **Router** | `router.rs` `local_only_never_falls_back_to_remote`、`local_only_with_cooled_down_local_still_never_uses_remote`、`local_only_uses_lora_as_a_local_tier`、`local_only_never_reaches_remote_even_when_the_gate_grants_it`（6b.5e 前置，放行 gate 加云授权） | `router.rs` `remote_provider_is_not_called_without_a_host_egress_gate`、`denied_remote_falls_back_to_healthy_local_provider`、`tier_parse_defaults_unknown_to_remote`、`a_remote_provider_that_cannot_name_its_destination_is_never_called`（6b.5b） | 正例：`router.rs` `complex_task_prefers_remote`（放行 gate）。负例只在 domain 层：`egress.rs` `cloud_grant_must_match_resource_purpose_destination_and_generation`（没有换 `resource_id`、`authorization_ref`、`policy_version`）、`an_authorized_request_is_rechecked_against_the_live_gate_after_revocation`；router 层：`an_exact_grant_lets_the_remote_provider_serve_only_that_request`（6b.5b，精确匹配 gate：正例，以及资源、修订、授权引用、政策版本、代次、目的地、用途逐项不匹配的负例） | `router.rs` `simple_task_prefers_local`、`tier_order_respects_privacy_and_complexity` |
| **run_loop**（`RunManager::execute`） | `lib.rs` `resumed_thread_cannot_rebuild_remote_permission_from_default_profile`（远端桩，唯一一条） | `lib.rs` `source_tag_write_failure_stops_before_any_model_call`、`a_recall_tag_that_cannot_be_written_stops_the_run_before_the_model`、`a_session_floor_that_cannot_be_written_stops_the_run_before_the_model`、`a_schedule_origin_that_cannot_be_read_stops_the_run_before_the_model`、`a_run_whose_own_sources_cannot_be_read_stops_before_the_model`（都用本地 provider）；`a_run_whose_sources_cannot_be_read_is_routed_local_only`（6b.5b，远端桩） | 不适用：只调 `router.complete` | `lib.rs` `run_completes_with_full_event_sequence`、`ordinary_sources_of_any_kind_do_not_restrict_tool_egress` |
| **REST /api/v1/chat** | `routes.rs` `post_chat_never_reaches_a_remote_only_provider`（6b.5b） | `session_memory/chat.rs` `a_chat_turn_whose_session_floor_cannot_be_written_fails_before_the_model`；`lib.rs` `a_chat_prelude_whose_floor_cannot_be_written_fails` | 不适用 | `routes.rs` `post_chat_records_a_timing_row_on_success`；`chat.rs` `explicit_remember_over_chat_stores_a_qualified_assertion` |
| **子 agent（explore）** | `subagent.rs` `delegated_call_inherits_persisted_local_only_policy` | `subagent.rs` `a_delegated_call_whose_parent_policy_cannot_be_read_stays_local_only`、`a_delegated_call_without_a_policy_store_stays_local_only`（6b.5b） | 不适用 | `subagent.rs` `a_direct_answer_returns_immediately`、`it_reads_a_file_then_answers` |
| **会话压缩** | `session_memory_tests.rs` `local_only_run_compaction_never_falls_back_to_remote` | 不适用：写死 LocalOnly，不读政策 | 不适用 | `session_memory/mod.rs` `successful_compaction_replays_every_original_message_after_daemon_restart` |
| **模块模型回调** `_a24/model/complete` | `model_callback.rs` `remote_provider_stays_closed_without_a_host_egress_policy`；`me4_model_blackbox.rs` `model_complete_blackbox_round_trip`（远端桩零请求） | **计划/代码差异**：回调不读父 run 的来源（见 §6 第 2 条），所以没有可失败的政策读取 | 无接口可表达 | `model_callback.rs` `a_successful_call_broadcasts_exactly_one_model_call_event_with_no_content` |
| **Guardian**（默认关闭，`A24_GUARDIAN`） | `guardian.rs` `the_model_assessor_never_sends_a_payload_to_a_remote_model`（6b.5b，接真实 router 和远端桩） | `guardian.rs` `assessor_error_fails_closed_to_escalation`（只测 stub） | 不适用 | `guardian.rs` `low_risk_auto_approves` |
| **模型发现** | `router.rs` `model_discovery_never_lists_a_remote_provider_through_the_default_gate`（6b.5b，`models()` 计数为 0） | `a_remote_provider_that_cannot_name_its_destination_is_never_called`（6b.5b，同时测 `models_detailed_with_egress`）；gate 拒绝见 (a) | `router.rs` `model_discovery_needs_a_grant_for_the_discovery_purpose`（6b.5b，精确匹配 gate：正例，以及用途、资源、修订、授权引用、政策版本、代次、目的地逐项不匹配的负例） | `routes.rs` `get_models_includes_the_daemons_default_model_field` |

**(a) 列证明的是什么**（#1209 PR-Daemon 的 Gate，2026-10-25 实测）：把 `TaskProfile::tier_order` 里 LocalOnly 对远端的排除去掉（LocalOnly 也排上 Remote），结果如下。
- 仍然通过：6b.5b 之前的 (a) 测试，即 router 的三条、`delegated_call_inherits_persisted_local_only_policy`、`local_only_run_compaction_never_falls_back_to_remote`、`resumed_thread_cannot_rebuild_remote_permission_from_default_profile`。生产 gate 是 `deny_all`，远端本来就被拒，所以这些测试只证明「远端零调用」，不证明 LocalOnly 排除在起作用。
- 变红：6b.5b 断言提示里含 `local-only` 的五条（`a_run_whose_sources_cannot_be_read_is_routed_local_only`、子 agent 两条、`the_model_assessor_never_sends_a_payload_to_a_remote_model`、`post_chat_never_reaches_a_remote_only_provider`），加上原有的 `tier_order_respects_privacy_and_complexity`。这段提示只出现在「LocalOnly 且没有本地 provider」那个分支，排除一去掉，就变成 gate 拒绝的提示。
- **撤门（6b.5e）的前置，已补**：`local_only_never_reaches_remote_even_when_the_gate_grants_it` 用放行一切的 gate 加有效的云授权资源，LocalOnly 任务在只有远端、本地失败、本地冷却三种情况下远端都零调用；同样的 gate 和授权下 Any 任务能到远端（对照）。这里 LocalOnly 排除是唯一挡住远端的一层。把排除去掉后，这条和 `tier_order_respects_privacy_and_complexity` 变红，上面三条老的 (a) 测试仍绿。

## 3. 网络类

| 消费方 | (a) | (b) | (c) | (d) |
|---|---|---|---|---|
| **http_fetch** | `net.rs` `a_local_only_source_is_refused_before_any_request`（IPv4、IPv6 `[::1]`、域名三种主机都经 `call()`，零请求、零 DNS）、`every_resource_of_the_run_is_checked_not_only_the_first`；`domain/egress.rs` `the_local_only_flag_alone_refuses_an_otherwise_granted_resource`（6b.5c-1，#1220） | `net.rs` `denied_fetch_does_not_invoke_dns_resolver`、`a_fetch_whose_source_policy_is_unavailable_is_refused_before_any_request`、`a_granted_run_whose_policy_then_became_unavailable_is_refused`、`a_restricted_run_without_an_egress_gate_is_refused_before_any_request`（6b.5c-1） | `net.rs` `an_exact_grant_fetches_only_what_it_names`（精确目的地正例，以及资料、修订、授权引用、政策版本、代次、目的地、用途逐项不匹配的负例）、`an_incompletely_granted_resource_is_refused_even_by_a_permissive_gate`（6b.5c-1）。**缺**（Low）：HTTPS 主机、HTTPS 请求用 HTTP 来源的授权（本机桩没有 TLS） | `net.rs` `ordinary_fetch_does_not_require_egress_policy_before_dns`、`an_ordinary_fetch_of_an_ip_literal_needs_no_egress_policy`（6b.5c-1，IP 字面量、无 gate、端到端取回）；`tools/src/lib.rs` `ordinary_http_fetch_is_advertised_without_egress_policy` |
| **MCP client 工具** | `agent24-mcp/src/lib.rs` `a_restricted_or_unknown_source_never_reaches_the_mcp_server`（放行 gate 加 LocalOnly 资源，6b.5c-1） | 同一条测试的「政策不可用」「没装 gate」两种情况（6b.5c-1） | `an_exact_grant_reaches_only_the_mcp_tool_it_names`（测试 gate，见下方注；6b.5c-1）。**缺**（Low）：目的地常量和桩的真实名字相同，区分不出写死 | `an_ordinary_run_calls_an_mcp_tool_without_egress_policy`（6b.5c-1） |
| **MCP server、ACP 桥（peer 视图）** | `agent24d/src/peer_runs.rs` `a_run_with_any_source_that_is_not_ordinary_is_withheld_from_peers`、`a_run_that_called_tools_is_relayed_only_when_each_call_has_its_own_ordinary_tag`（6b.5f-3）、`a_run_that_recalled_a_memory_is_withheld`、`a_run_started_by_a_kernel_created_schedule_is_withheld`、`a_session_run_is_withheld_while_the_daemon_keeps_session_memory`（6b.5c-2a）；两座桥只读 peer 视图的测试在 6b.5c-2b | `a_run_whose_sources_cannot_be_read_is_withheld_from_peers`、`a_run_that_has_not_ended_carries_no_content_and_tightens_when_a_source_lands`（6b.5c-2a）、`a_run_whose_tool_calls_or_schedule_origin_cannot_be_read_is_withheld`（6b.5f-3） | 不适用：peer 视图不接受云授权，非 Ordinary 一律扣下 | `an_ordinary_run_that_has_ended_is_relayed_in_full`（6b.5c-2a） |

另有 SSRF 测试：`net.rs` 里的 `public_ip_classification`、`private_targets_are_denied`、`redirects_are_returned_not_followed`。

> 注：MCP 的门在代码里存在（`agent24-mcp/src/lib.rs`，用途 `McpTool`，目的地 `mcp:{server}:{tool}`）。但传输是 stdio 子进程，目的地只是一个标签，不是子进程实际的网络端点。模块工具同理。放行 gate 加 CloudAuthorized 会让调用通过，可子进程的网络其实圈不住（§2.4 第 3 条）。所以 (c) 只在测试 gate 上做，并在这里写明它不证明子进程不外发。

## 4. 模块与进程类

| 消费方 | (a) | (b) | (c) | (d) |
|---|---|---|---|---|
| **模块工具** | `module_tool_runtime.rs` `restricted_module_tool_is_denied_when_egress_gate_denies` | `a_module_tool_is_refused_when_the_source_policy_is_unavailable`（政策不可用、没装 gate 两种情况，运行时零调用；6b.5d） | `an_exact_grant_reaches_only_the_module_tool_it_names`（测试 gate，目的地是 `模块.操作` 这个标签，同 §3 的注；正例，以及资料、修订、授权引用、政策版本、代次、目的地、用途逐项不匹配的负例；6b.5d） | `module_tool_runtime.rs` `granted_fake_module_is_advertised_and_callable` |
| **shell_exec** | `local.rs` `shell_exec_fails_closed_when_child_network_use_cannot_be_bounded`、`local_only_shell_exec_is_denied_even_by_permissive_egress_gate`（6b.5d 起改用 `touch`，断言子进程没跑） | `local.rs` `shell_exec_is_denied_when_source_policy_lookup_failed`（同样断言零副作用）；`lib.rs` `restricted_or_unknown_sources_of_any_kind_restrict_tool_egress`、`a_run_with_no_readable_sources_restricts_tool_egress`（经 `tool_context_for`：没有标签、标签读失败；6b.5d） | `local.rs` `cloud_authorized_shell_exec_is_denied_because_its_destination_is_unknown`（授权齐全的 CloudAuthorized 加放行 gate 仍被拒，子进程没跑；6b.5d）；`domain/egress.rs` `a_permissive_gate_cannot_allow_what_the_invariants_refuse`（非远端、空目的地、没有资源） | `local.rs` `shell_exec_runs_for_ordinary_run_without_egress_policy`、`ordinary_shell_exec_runs_after_approval_without_egress_gate`；`lib.rs` `approved_shell_exec_actually_executes` |
| **模块进程自身出网** | 没有网络沙箱。**已知限制**：只靠调用前的出口门（受限 run 一律拒） | — | — | — |

`has_remote` 过渡门（`module_tool_runtime.rs` 的 `blocked_by_remote_tier_guard`）现有测试：
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
| 恢复停放的 run | (a)：直接调 `run_loop` 的那一条；`lib.rs` `a_restricted_run_resumed_after_a_restart_still_cannot_spawn`（带 Restricted 来源、停在 shell_exec 审批上的 run，经 `restore_pending_approvals`、批准、`resume_run` 恢复后子进程没跑；只有 Ordinary 来源的对照会跑；6b.5d）。恢复时不重新合并下限（#1204 G4，David 定） |
| 模型 override | 只换 model id：`ModelRouter::route` 只看 tier 顺序和健康状况，不读 `req.model`。`router.rs` `a_model_override_never_changes_the_routed_tier`（override 指向哪个 provider 都不改 profile 选的 tier；LocalOnly 不会到远端；6b.5d） |

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
   **David 2026-10-25 定：视为出口，按分类过滤**（Claude、Codex、Fable 三方都选这一项）。在 6b.5c-2 实现，要求如下。其中「Ordinary」怎么算，以第 4 条的现行规则（只放行自成一体的 run）为准：
   - 内核权威分类为 Ordinary 的 run，照常返回文本。
   - Restricted、Unknown 或分类读不出来时，只返回状态和固定提示；`agent24_list_runs` 不给这些 run 的 prompt 和输出。
   - 错误消息同样过滤。
   - 分类可能在 run 中途因召回而收紧，所以 ACP 的流式增量要等分类确定后再放行，或者每段增量按当时的分类判定，不能先流出去再检查。
   - 内核要把 run 的分类暴露给桥（一个字段或一个按分类过滤的端点）。
   - 测试要覆盖：分类中途变化、分类缺失、受限内容不外泄。
4. **运行中途的工具结果没有来源标签**（6b.5c-2 第 1 轮 Codex 评审发现，2026-10-25）：
   - ADR §2.1 要求「模块/MCP/HTTP/Exec 输出都成为带标签的数据块」，追溯不到来源时视作敏感。可 `RunManager::execute` 只在第一次调模型之前打标签（用户输入、召回、schedule 来源、会话下限），`run_tool_call` 的结果直接进模型上下文，不打标签。
   - 所以一个 run 即使用模块工具读了受限文档、用 fs_read 读了文件，它的标签仍然全是 Ordinary。工具读到的内容还会经下面几条路进入后来的 run，而那些 run 自己没有工具调用，标签同样全是 Ordinary：
     - 会话记忆：会话下限在调模型之前、只按标签合并，后续轮从会话上下文读到上一轮的回答。`/chat` 带 session_id 时是临时 run，不进 runs 表、不打标签，但它召回的内容和回答也写进会话记忆。
     - 长期记忆：`run_assertion_source_ref` 只看标签，工具内容记成 Ordinary 的断言，在别的会话里被召回。
     - self-wake：schedule 记下的来源分类只看创建它的 run 的标签（`self_wake.rs`），唤醒 prompt 里可以带着工具内容；创建它的 run 没有会话时，被唤醒的 run 也没有。
   - 受影响的有：工具出口门（`tool_context_for` 只看标签）、会话下限、记忆分类、MCP server 和 ACP 桥的 peer 视图。
   - **David 的决定**（2026-10-25 至 10-26，三次）：
     - 第一次：peer 视图里，本 run 调过工具就扣下。Claude、Codex、Fable 都选这一项。
     - 第二次：第 2 轮评审发现跨 run 传播，改成按会话算。Claude、Fable 选这一项；Codex 选在源头打 Unknown 标签，并自认那会改变目标。
     - 第三次（现行）：第 3 轮评审又找到 `/chat`、无会话的 self-wake、同会话并发三条路径，按路径推断补不完。改成**只放行自成一体的 run**：标签全是 Ordinary、没调过工具、没召回过记忆、不是内核建的 schedule 触发、没有会话上下文（不带会话，或内核没开会话记忆）。其余一律扣下。6b.5c-2a 实现内核的 peer 视图（`/api/v1/peer/runs`），6b.5c-2b 把 MCP server 和 ACP 桥接上它。三方都选这一项，Codex、Fable 都指出它扩大了代价，David 确认接受。ACP 桥每次 prompt 都建会话，所以开了会话记忆时，ACP 上的 run 全部只发固定提示。
     - 会话下限、记忆分类和工具出口门这次都不改：在源头打 Unknown，会让调过任何工具的 run 及其会话后续轮的 shell_exec、http_fetch 等全被拒。
     - 给工具结果打标签另开一片（§7 的 6b.5f），同时修工具出口门、会话下限、记忆分类和 self-wake 的来源分类；那一片落地后再放宽 peer 视图的规则。已经记成 Ordinary 的旧断言和 schedule 不做兼容：David 已定不做兼容、清库。
   - **David 2026-10-26 对 6b.5f 的两项决定**：
     - 分类按工具定：fs_read、explorer 的输出记 Ordinary，因为来源能追溯到工作区根目录里的具体路径；http_fetch 取回的公网内容记 Ordinary；shell_exec、MCP、模块工具的输出记 Unknown，因为来源追溯不到；fs_write、self_wake 返回回执，不含读到的资料。Claude、Fable 选这一项；Codex 选「本地结果都算普通」，那要修订 ADR §2.1。David 接受的代价：跑过 shell_exec 以后，本 run 和同会话后面的轮次不能再用 shell_exec、http_fetch、MCP、模块工具；工作区文件经 fs_read→http_fetch 外发这条链仍开着，和现在一样。
     - 同源回送不拦：模块工具的输出记 Unknown，标签上由内核记下来源模块。发往模块 X 的调用不计来自 X 自己的 Unknown，因为 X 本来就持有这些内容。ADR 目前没有这条例外：它沿用 §2.1.1 第 3、5 条「按动作实际携带的数据判定、由可信宿主收窄」的方向，但第 5 条要证明的是载荷，这里证明的只是接收方，所以 6b.5f-1 落地时要同步补进 ADR §2.1.1。其他来源的 Unknown 和 Restricted 照拦；External 操作不享受这条例外；读不到来源时照拒。同源靠注册表里的绑定来认，不看模块自报的 `output_privacy`。来源模块要带着身份传到同会话后面的轮次。三方都选这一项。这样 Documenting 的对话入口可以在同一个模块内连续调用。
   - **David 2026-10-27 对 6b.5f-3 的两项决定**（证据裁决，记录在本机 `~/.claude/decisions/`）：
     - peer 视图放宽（选 C，三方 R1 收敛）：去掉「没调过工具」，改成每条 tool_calls 行都要有一条 `source_id` 以 `:{这条行的 id}` 结尾的 tool_result 标签，否则扣下；对上的标签照常参与「全部 Ordinary」的判断。被拒的调用不豁免：用户拒绝审批时写的理由、http_fetch 的 SSRF 拒绝里解析出的地址都会进模型，却没有标签。代价：有过被拒调用的 run、plan 模式的 run（propose_plan 不打结果标签）仍扣下；开着会话记忆时带会话的 run 照旧全扣。
     - 存量缺口回填（选 B，裁判多数）：迁移 0028 给升级时已有的、带会话却没有 session_floor 标签的 run 补一条 Unknown 的 session_floor 标签；peer 视图的判定不为此改动。代价：开发库里不开会话记忆时合法的历史会话 run 也被永久扣下。
5. **Hyphae `/comm/send`**：只能由用户经 REST 直接调用，没有 Tool 包装，也不带 run 资料。视为用户直接操作，不在 agent 出口矩阵内。

## 7. 后续切片

| 片 | 补什么 |
|---|---|
| 6b.5b 模型类 | **已补**（见上表标 6b.5b 的测试）。没有补的：模块模型回调的 (b)，要等 §6 第 2 条接上来源继承以后才有可测的政策读取 |
| 6b.5c 网络类 | http_fetch 的 LocalOnly、政策不可用、精确目的地正例和逐项不匹配负例，以及 Ordinary 下 IP 字面量分支和端到端取回；MCP client 的 (a)(b)(d)，以及测试 gate 上的 (c)；MCP server 和 ACP 桥按分类过滤（§6 第 3、4 条）。6b.5c-1（#1220）已补 http_fetch 与 MCP client 的四列；peer 视图拆成 6b.5c-2a（内核视图）、6b.5c-2b（两座桥） |
| 6b.5d 模块、进程、生命周期 | 模块工具 (b)(c)；shell_exec 零副作用、CloudAuthorized 仍被拒、`tool_context_for` 两个分支；恢复时受限来源的出口测试；模型 override 测试 |
| 6b.5f 工具结果打标签 | 按 §6 第 4 条 David 10-26 的两项决定。5f-1：run 内打标签（结果进模型之前，写失败就结束 run）和模块的同源回送。5f-2：来源模块跨轮次传到会话后面的轮次。5f-3：放宽 6b.5c-2 的 peer 视图规则，改成按标签逐条对账（§6 第 4 条 10-27 的决定），并用 0028 回填存量缺口 |
| 6b.5e 撤 `has_remote` 过渡门 | 前提：本表全绿、§2 末尾那条 LocalOnly 排除测试已补、§6 第 2 条已接、6b.5f 已合入、K1-6a 已验收；附 #708 macOS 证据。**撤门要 David 确认** |
