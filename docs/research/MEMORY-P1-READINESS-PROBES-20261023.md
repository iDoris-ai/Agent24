# 记忆 P1 开工探针：2026-10-23

## 结论与边界

> **最新补充优先读 §8.1 与 §8.5**：同进程/冷恢复 marker history 已补得红证；真实 HTTP 的撤回、pause、fallback、资格变化和无 session 共 5 项已运行并由协调者复跑通过。前文“未覆盖 HTTP/审批 marker”是首轮记录，不再表示这些列明场景尚未测试。同秒 reassert 固定时钟后续已取得红证，见 §9；没有据这些观察改变已发布契约。

首轮及 §8 在 **`f16e65a95f9732f4e00cb288f967e40e0978eb7b`** 实际执行，未改生产代码；§9 是后续独立集成基线上的私有时钟接缝，不能把“生产代码零修改”外推至 §9。H02、P02 均取得新的确定性红证；P01 的新 run / 同进程审批 / 独立进程退出后冷恢复符合已发布 pause 契约；P03 得到真实组件发送时点观察，但尚不能核销为 H04 产品违约。

| 项目 | 本轮结果 | 可以据此做什么 |
| --- | --- | --- |
| H02 新 evidence 重记后再次 remember | RED：真实 WriteGate 返回 Conflict；真实 chat 记忆入口返回 Failed | 独立修复幂等审计匹配，保留所有审计拒绝控制 |
| P02 完整 marker 用户 prompt / 普通历史保真 | RED：provider 实际收到空 messages，或缺少该历史 user turn；原始持久内容保留 | 进入来源方案评审；不能把历史草案当作获批迁移契约 |
| P01 新 run pause / 同进程审批 / 冷恢复 | GREEN：新 run 无 recall；旧审批继续原 recall；真实撤回仅裁 request copy | 核销这里列明的当前契约场景，不改 pause 为撤回 |
| P03 chat prelude 后与 fallback 前变更 | OBSERVED：撤回或 pause 后，已经构造的请求仍含原 recall，实际送达 spy | 继续 HTTP 场景与承诺边界确认；不宣布已验证整个 P03 或已证明 H04 |

M1、M2 保持已发布；H01 #1024 已交付。初始探针 worker 未重做旧交付，未改生产/迁移/OpenAPI/依赖/计划，未提交推送、派子 agent、连接 Mini 或使用真实用户 DB / 大模型。后续协调者另有文档 PR 与 §9 私有接缝任务，须按各自边界读取，不把初始 worker 的零改动声明扩成整条主线状态。

## 基线、输入与环境

- 隔离任务分支：`ab/memory-p1-20-readiness-probes`；首次 `git rev-parse HEAD` 为上述精确 SHA，`git status --short` 为空。后续报告与夹具尚须分片入库，不能认为裸 main 已包含全部探针。
- 本地 `origin/main` 同为该 SHA；本地 `main` 为 `a2afe2205c56db8ba6a2c600b42f453aca734e2e`，本轮不切换、不修改、不 fetch，代码事实以指定基线为准，不声称实时远端状态。
- 已读 `docs/agent/PLAN-M1-MEMORY-HARDENING.md`、`docs/agent/PLAN-M1-SNAPSHOT-PROVENANCE.md`，及用户给定的 2026-10-22《02-记忆余项证据核销》建议稿；计划的归档和未批准标注优先于正文旧待办。
- 仅笔记本本地测试。所有 Cargo 命令先设置共享 `CARGO_TARGET_DIR=$HOME/Dev/auraai/Agent24/rust/target`、`ulimit -n 4096`，未创建工作区 target，Cargo lane 串行使用。
- 数据均为内存库或 `tempfile` 独立文件库；审批工具仅执行 `/bin/echo p1-approved-tool`。provider 为测试 spy，无真实模型/网络调用。
- 完整 stdout/stderr 在 `/tmp/a24-p1-probes/*.log`，退出码在同名 `.exit`，Cargo argv 在同名 `.command`。使用 `> log 2>&1; result=$?` 保存原退出码，无 grep 管道掩盖退出码。临时日志可能被系统清理，关键原始输出节录于本文；完整夹具与日志另由协调者保存带哈希的本线归档。源码尚未全部合入集成分支，复跑须取得对应固定夹具，不能只检出裸 main。

## 新增源码与命令台账

- Memory 探针原始路径：`rust/crates/agent24-memory/tests/p1_readiness.rs`（任务夹具，待分片入库；指纹见下文）。
- Agent 探针原始路径：`rust/crates/agent24-agent/tests/p1_readiness.rs`（任务夹具，待拆分；不提供尚未入库的死链接）。
- 以下所有探针共用上述精确基线；编号是本报告命令引用，不是新任务 ID。

在工作区 `rust/` 执行，每次先运行：

```sh
export CARGO_TARGET_DIR=$HOME/Dev/auraai/Agent24/rust/target
ulimit -n 4096
```

| 命令 | 完整 argv | 日志 / exit | 实际结果 |
| --- | --- | --- | --- |
| C1 | `cargo test -p agent24-memory --test p1_readiness -- --nocapture` | `h02-controls-v2.log` / 0 | 2 passed，1 ignored |
| C2 | `cargo test -p agent24-memory --test p1_readiness h02_new_evidence_reassert_then_retry_must_succeed -- --ignored --exact --nocapture` | `h02-red.log` / 101 | 0 passed，1 failed，2 filtered |
| C3 | `cargo test -p agent24-agent --test p1_readiness -- --nocapture` | `agent-controls.log` / 0 | 当时 5 passed，3 ignored（尚未加入 history 红测） |
| C4 | `cargo test -p agent24-agent --test p1_readiness p02_complete_marker -- --ignored --nocapture` | `p02-red.log` / 101 | 0 passed，2 failed，7 filtered |
| C5 | `cargo test -p agent24-agent --test p1_readiness h02_chat_remember_product_path_retry -- --ignored --exact --nocapture` | `h02-product-red.log` / 101 | 0 passed，1 failed，8 filtered |
| C6 | `cargo test -p agent24-memory -p agent24-agent --test p1_readiness -- --nocapture` | `final-default.log` / 0 | 最终 Agent 5 passed / 4 ignored；Memory 2 passed / 1 ignored |
| C7 | `cargo clippy -p agent24-memory -p agent24-agent --test p1_readiness -- -D warnings` | `clippy.log` / 0 | 两个探针 target 及依赖检查通过 |
| C8 | `cargo fmt --all --check` | `fmt-check.log` / 0 | 全 workspace 格式检查通过 |

所有日志上表省略的前缀均为 `/tmp/a24-p1-probes/`。C6 合计 **7 passed / 0 failed / 5 ignored**；ignored 中 4 个是明确 opt-in 的已知红色契约测试，1 个为仅供父测试调用的冷恢复子进程夹具。不要对整个 target 无筛选地运行 `--ignored`，子夹具要求父测试设置临时路径。红测未删掉或反转期望；默认 CI 不执行已知失败契约探针。本轮未跑全 workspace tests / 全 workspace clippy，不能把定向门禁称为完整提交门禁；没有提交操作。

首次 C1 夹具版本记录为 `h02-controls.log` / exit 101：1 passed、1 failed、1 ignored，失败于测试试图 UPDATE 不可变 `mem_events.payload`，报 `SqliteError 1811: mem_events.payload is immutable`。这是**本轮夹具错误，不是 H02 红证**。修订后在隔离测试库中读取原审计、DELETE 该行，再用真实 `EventStore::append` 插入同 ID 的伪造事件；没有删除不可变触发器、没有改生产。C1 v2 / C6 验证五个反例均被拒绝。

## H02：新 evidence 重记的幂等审计

路径：memory 探针的 `h02_new_evidence_reassert_then_retry_must_succeed`（C2），agent 探针 `h02_chat_remember_product_path_retry`（C5）。

最小反例顺序（每一步 await 完成，无 sleep/秒边界依赖）：

1. 真 WriteGate `propose` 同一 assertion ID，evidence=`ev1`，成功。
2. 真 `KvStore::forget`，返回 Forgotten。
3. 同 ID、同内容、evidence=`ev2` 重记，返回 Committed。
4. 同内容再次 remember（新事件对应 evidence=`ev3`），期待幂等成功，实际 Conflict。
5. 在最终失败断言前读取 DB：1 条 active belief，evidence 仍为 `ev2`；审计数量不变，含首次 commit、retracted、reasserted。

C2 原始输出：

```text
H02 retry=Err(Conflict("assertion id \"p1-fact\" already exists without matching current content, evidence, or commit audit")); audit_kinds=["mem.write_decision", "assertion.retracted", "assertion.reasserted"]; active=1; evidence=["ev2"]
```

C5 用 `RunManager::chat_remember_turn` 连通真实 `SessionMemory::remember` → append_turn → `retain::persist` → WriteGate。三次 `remember espresso` 使用真实新 user event ID，前两次 Saved，中间真实 forget，第三次实际 `Failed`（期待 Saved），原始输出 `H02 real chat_remember_turn retry=Failed; event_count=9`。这证明现行聊天记忆产品组件可达，不依赖伪造 evidence 触发；尚非 HTTP receipt 序列化端到端验证。

正/反例（C1/C6）：

- 原始 commit 未撤回时，同内容不同 evidence 重试仍 Committed，不增加审计。
- u2 对 u1 同 ID 写入 Conflict，forget 为 NotFound，u2 recall 为空。
- 缺审计、伪 payload、伪 owner、伪 origin source、伪 kind：均 Conflict；原 belief 保留。伪造行是测试故障注入，不声称普通产品 API 可改审计。

对应源码事实：`writer.rs::commit_with_audit` 当前 active 分支用当前 assertion evidence 重建期望 `mem.write_decision` ID；reassert 分支改为 ev2，却仅写 `assertion.reasserted`，故首次 ev1 commit 审计不匹配。这是本轮红证支持的归因，不是 H01 重复撤回 ID 问题。

未覆盖：**同秒、相同 evidence 多次 reassert 的 ID 碰撞**。writer 直接读 `now_iso8601()`（秒精度），当前公开 WriteGate 没有注入时钟入口；本轮没有用“恰好跑得快”冒充确定性时钟证明。下一步应单独批准测试专用时钟接缝，再固定时间验证多次转换；不能靠本轮结论宣布该独立风险成立或关闭。

## P02：完整 marker 的用户 prompt 与普通历史

路径：agent 探针 `p02_complete_marker_prompt_must_reach_provider`、`p02_complete_marker_history_must_reach_provider`（C4）；对照 `p02_prefix_only_and_active_id_controls`（C3/C6）。

无宿主 recall、无 prior history 的新 run，真实 `start_run` → 持久 snapshot → `request_messages_for` → 真 ModelRouter → spy：

- 仅完整 `RECALL_PREFIX + RECALL_END_MARKER`、无条目：spy 实际被调用一次，收到 `messages=[]`。
- 完整 marker 中含 `unknown-id` 条目：同样实际调用一次、空 messages。
- 原始 `run_messages[0]` 两例均逐字保留 prompt，且 source tag 为 `seq=0, kind=UserInput, mode=LocalOnly`。**已有 UserInput tag 并未阻止发送副本按 marker 误裁**。
- 对照：仅 prefix 不带 end marker，逐字到 provider；active ID 的完整形状 prompt 在 `recall_budget=0`、无真实 host 注入下保留。该对照是配置级 no-host 场景，不额外宣称有公开 budget UI。

普通历史反例通过真实 `chat_remember_turn` 把完整 marker user turn 和普通 assistant answer 写入 SessionLog，再创建有 session 的新 run。实际收包：

```text
P02 normal history provider=[[Msg { role: "assistant", content: Some("historic answer"), tool_calls: [], tool_call_id: None }, Msg { role: "user", content: Some("follow up"), tool_calls: [], tool_call_id: None }]]; original raw events preserved
```

断言要求历史 user marker 文本到达 provider，实际失败；同时逐事件检查原 `mem_events` 的 ID/body 仍在。不是原文物理删除，而是 provider 视图误裁。真实 host recall 的撤回对照由 P01 同进程审批测试覆盖：撤回后不发送，snapshot prefix 不改；因此不能通过关闭所有撤回重校验来修 P02。

实际产品可达性：任意合法 prompt 可进入新 run；有 session 的正常历史也由真实生产组件构造。未调用 HTTP route，不断言前端展示或线上 provider 效果。未覆盖完整 marker 的同进程/冷审批恢复、host/prompt/history 完全同文三者同场、ledger 故读失败/时态资格变化；下一步在下面来源评审冻结后，扩展现有真实审批夹具，以 provider 收包和 immutable snapshot 为 oracle。这些缺口不削弱已运行 fresh/history 红证，也不算已验收未来来源修复。

## P01：pause 与审批恢复契约

依据当前 OpenAPI `putMemorySettings`（`protocol/openapi.yaml:1525` 起）：设置影响新开始 run 的新写入/跨 session recall；已有 assertion 保留；审批恢复重放自身已持久化 messages，不按当前 pause 重决定。`SessionMemory::active_ids` 查询 belief 资格，不把 pause 当 retract。

路径与执行：agent 探针三项，C3/C6 均 exit 0。

| 测试 | 顺序 / 断言 / 正反例 | 可达性与限制 |
| --- | --- | --- |
| `p01_new_run_pause_keeps_raw_and_blocks_recall_and_write` | 同一库先 remember espresso；enabled 新 run spy 见 host recall；pause 后新 run spy 不见 recall、仍见 pause notice 和 prompt；`chat_remember_turn` 返回 SkippedPaused，原始事件增加、原 assertion 仍 1 条 | 真 RunManager / SessionMemory / WriteGate；未跑 PUT HTTP handler；未测 pause 与正在提交事务的竞争 |
| `p01_in_process_approval_pause_and_retract_controls` | spy 首轮发 shell_exec；真 ApprovalBroker/BrokerGate 进入 pending 后才变更；pause 分支批准后第二次 spy 仍见 recall；retract 分支第二次 spy 无 recall；两分支都见实际工具输出 `p1-approved-tool`，各 2 次 provider 调用；原 snapshot 各行 content/seq 不变 | 真审批 waiter 和工具执行；覆盖当前普通 shell 审批，未覆盖 plan/fan-out |
| `p01_cold_reopened_approval_pause` | 子测试进程创建真 run/pending approval、首 provider 已调用、pause 持久化后直接 exit(0)；父进程重开两个 SQLite 文件，确认 enabled=false，restore=(1,0)，新 broker approve，再 resume_run；1 次实际 provider 调用含原 recall 和已执行工具输出，原 snapshot content/seq 保留 | 真进程边界与新 DB pools，无内存 waiter 残留；不是仅 drop manager；未启动 daemon 的完整启动/orphan sweep，也不覆盖跨版本升级 |

冷恢复子进程的全部捕获 stdout/stderr 均并入父测试日志。子进程是夹具，主动 exit 前完成所有持久化和断言；不以缺失的 libtest 完结行充作通过，父测试检查进程退出码、重开状态、实际恢复与收包。

原始关键输出：

```text
P01 enabled/new-paused provider calls=2; notice/prompt/raw retained; existing assertion active; new write skipped
P01 same-process retract=false: provider calls=2; tool executed; snapshot prefix unchanged; recall_kept=true
P01 same-process retract=true: provider calls=2; tool executed; snapshot prefix unchanged; recall_kept=false
P01 cold reopened: restore=(1,0); provider=1; tool executed; paused original recall/snapshot preserved
```

上述通过不是未来 Unknown 来源方案的批准，也不要求 pause 清空 snapshot/history。若需 daemon 启动全链验收，下一步复用这个文件库夹具，调用 daemon startup 的 restore 与 orphan sweep 后再经 approvals route 驱动，另列断言，不将组件级冷恢复扩大成已验证全启动。

## P03：chat / fallback 的发送时点

路径：agent 探针 `p03_chat_prelude_and_fallback_send_observation`（C3/C6，exit 0）。这个测试是**现状观察**，其绿色不表示隐私/撤回契约验收通过。

复用 `RunManager::chat_memory_prelude`、`normalize_for_provider`、`task_profile_for_run` 和真 `ModelRouter::complete_served`。没有复制 recall 过滤 helper。两种确定性接缝：

1. prelude 已返回 → await 真 forget 或 set_memory_enabled(false) → 构造完成的请求进入 router → spy。
2. 首 provider 已收到请求 → 该 provider 内 await 真 forget 或 pause → 返回 Unavailable → router fallback → 第二个 spy。

均为 await 先后关系，无概率 sleep；第二种明确在首 provider 开始接收之后，不声称能追回已发送内容。每种接缝分别跑撤回和 pause，共 4 场景。直接发送分支 spy=1；fallback 分支首 provider=1、最终 spy=1。每次最终 spy messages 与原 request 完全相等并含 recall ID；撤回场景另断言当前 ledger recall 已为空。

```text
P03 pause=false fallback=false: mutation-before-send confirmed; final provider kept recall; first_calls=0 final_calls=1
P03 pause=false fallback=true: mutation-before-send confirmed; final provider kept recall; first_calls=1 final_calls=1
P03 pause=true fallback=false: mutation-before-send confirmed; final provider kept recall; first_calls=0 final_calls=1
P03 pause=true fallback=true: mutation-before-send confirmed; final provider kept recall; first_calls=1 final_calls=1
```

当前消费者映射：`agent24d/src/routes.rs:523` 调 prelude，之后 normalize/构建 request，`:566` 调 complete_served；router 每次 provider.complete 使用同一 `&req`。因此组件观察与真实 route 组合相符，但**本轮没有经过真实 post_chat HTTP handler**，不能写成 REST 端到端反例。

尚不能完整核销的精确缺口及下一步：

- route 中没有公开测试 barrier 接缝，当前独立 integration target 只能调用公开 RunManager/router；本轮未装配 daemon AppState/router。下一步加 daemon 独立测试模块，装配现有 test AppState 和 provider Notify barrier，经真实 POST chat 及 DELETE/settings 请求验证时点，不改生产、不跨网络持 SQLite 锁。
- qualification 失效、no-session chat 的 byte-for-byte 控制未执行。下一步在隔离 file DB 中先注入 qualified=false，再记录两 provider 收包；无 session 请求单独断言完全不经记忆 prelude。
- 当前 DELETE OpenAPI（`:1478` 起）承诺阻止新的 session recall、保留原 conversation；没有在此处定义每个 fallback provider 都必须重新授权。本轮是 recall 已构造的运行中请求。需先冻结“首次发送前已撤回”和“fallback 前撤回”的现行承诺范围，再判 H04；若要求新 live-check 时点，应独立设计，不把 pause 恢复例外顺手废掉。

## 0016 与当前来源链的事实映射

本轮只记录事实，不选迁移表、不增 schema、不实现 Unknown/NoRecall 状态机。

| 层 | 基线事实 | 与 P02 的关系 |
| --- | --- | --- |
| store 迁移 | `0015_module_consents.sql` 已占用；`0016_run_source_tags.sql` 存在；当前序列到 0018 | 归档草案的 0015_run_input_provenance 不能照搬 |
| 0016 表 | `(run_id, seq, source_id)` 主键；run_id FK 指向 runs；列含 schema_version/tag_json/created_at；seq 以约定关联 run_messages，不是独立消息 FK | 不是一 run 一条、不可变的 host snapshot 元数据表 |
| SourceRef | 当前 kind 为 UserInput / SelectedMaterial，mode 为 LocalOnly / CloudAuthorized；user_input 构造器给 LocalOnly、policy_version=0、无 revision_digest/authorization_ref；SelectedMaterial 当前无产品入口 | 尚无 HostRecall 类型/owner/NoRecall 显式状态；不能推断已有完整 host 来源契约 |
| 写入 | RunManager 先原子 append snapshot，再独立调用 `tag_run_source` 标记末条真实 prompt 的 seq；tag API 的冲突语义为 upsert，可替换 | 消息与 tag 不是同一个事务；本轮不擅自要求改变现行 tag 契约 |
| 读取消费者 | `run_policy_snapshot` 折叠 tags；缺失/无法解码收紧 LocalOnly；agent run_loop、chat、subagent、summary 相关模型路由使用 source policy | 这是处理位置/路由限制；不等于可识别哪一实例由 recall 注入 |
| recall 来源 | `SessionMemory::recall` 从 owner-qualified ledger 检索，renderer 输出 id/time marker；首次 snapshot 后发 MemoryRecalled IDs | MemoryRecalled 是首次注入事件，非每次实际发送审计；request_messages_for 没读 SourceKind/seq 来剪裁 |
| raw → assertion | SessionLog append_turn 保留 user/assistant Origin；`remember` 用 user event ID 给 retain evidence；retain 校验 UserSaid 并以 owner+object 定 ID；WriteGate 同事务写 decision | H02 的 evidence 是真实新事件链；P02 的 user source tag 和 raw Origin 已存在却未用于 host 实例判断 |

0016 SQL 文件中“尚无消费者”的初始注释属于创建时切片说明；当前 `source_policy.rs` 已明确列出 K1-6b.2 路由消费者，不能只读旧 SQL 注释就称表未启用。当前 `request_messages_for` 仍判断第一条 user 的完整 prefix/end，再依据 active IDs 重建或删掉第一条；P02 直接观察到 UserInput 标签与误裁同时存在。

下一步来源评审必须明确：复用 0016 的字段/更新语义是否足以表达 host 实例、与现有路由消费者如何兼容、初始快照原子性及旧 run 兼容如何处理。本文不预设“新表必须创建”或“已有 tag 一定足够”，不把旧稿的 continuation_error/wire/Unknown 政策当作本轮批准。

## 历史证据隔离与交付状态

- 2026-10-12 hardening 计划引用的 H01/R1 `/tmp/a24-memory-hardening/...` 红测、provenance 计划引用的外部 source03 provider spy RED，均是**历史他方证据**，本轮未复用其测试结果计数。
- 2026-10-22 台账明确是静态复核、无新 Rust 运行。本报告 H02、P02 的 `.log/.exit` 与新增源码是 **2026-10-23 指定基线的新运行**。
- 所有红色结果允许作为探针证据，未作生产修复；默认测试通过仅证明探针控制可运行，不代表 H02/P02 已修。
- 尚欠 P03 HTTP/qualification/no-session 完整覆盖及发送承诺冻结；P02 恢复/来源修复矩阵和同秒 reassert 时钟探针均逐项保留未验证。上述限制不被“开工核查完成”措辞隐藏。

## 最终源码与原始日志指纹

SHA-256 用于核对本轮产物；测试均未提交。

| 文件 | SHA-256 |
| --- | --- |
| `rust/crates/agent24-memory/tests/p1_readiness.rs` | `259f8826d906d359897d88202c5b0ee0acced2ac12fe30a91c9f400cea5bd3fa` |
| `rust/crates/agent24-agent/tests/p1_readiness.rs` | `487182495e1e2e293fffd7a065be82757ffcfcb4cce3114b2fe328fa5fe4ab0f` |
| `/tmp/a24-p1-probes/h02-red.log` | `8a35ac8cc7107ddbefc01d54e619d7c4206e7dcbff43c92b378e430b3ceb7be2` |
| `/tmp/a24-p1-probes/h02-product-red.log` | `68a486ecc4ecf5e5f2ed9dbd375e687fdb987c27bda7603ffac2db42dcb4e957` |
| `/tmp/a24-p1-probes/p02-red.log` | `3b36fe1a0f17517b02b88934f4dc3fbb65921661469b2b1fceda66c76ba8c2e9` |
| `/tmp/a24-p1-probes/final-default.log` | `01e88d658c29879616f7a1eb0b7b28ceef456ed0332f8bc307dbe9d5fba142b4` |

## 8. 2026-10-23 补充核查：审批保真、HTTP 边界与同秒审计 ID

本节为新增证据，不替换前文观察、红证或既有命令结果。基线仍为 `f16e65a95f9732f4e00cb288f967e40e0978eb7b`；实现只新增/扩展 `agent24-agent/tests/p1_readiness.rs` 中的 ignored contract probes 和一个安全的 child fixture。没有改生产、迁移、OpenAPI、依赖或批准法律。新 ignored 探针须按下表 exact 命令运行；不要无筛选执行所有 ignored 测试。`p02_cold_marker_fixture_child` 未标 ignored，在没有 `A24_P1_FIXTURE_DIR` 时立即 no-op，因此不会成为 `--ignored` 的独立子进程误运行对象。

### 8.1 P02：审批与真正冷重启后的 marker 原文

新增 `p02_complete_marker_prompt_survives_same_process_approval` 和 `p02_complete_marker_prompt_survives_cold_approval_restart`。两者均以真实 `RunManager`、`SessionMemory`、`ApprovalBroker`/`BrokerGate`、`shell_exec` 和 provider spy 运行；审批产生于 provider 首次返回真实 tool call 之后。冷场景由子测试进程创建 pending approval 后 `process::exit(0)`，父进程重开两个独立 SQLite 文件、restore approval 并 resume，保证不是仅 drop manager 的假重启。源码还保存了完整 marker 的历史 user turn 和普通 assistant answer，检查历史 marker 与用户 prompt 的逐字内容，而不只检查来源标签。

| 命令 | 原始 exit / 日志 | 证据与判定 |
|---|---|---|
| `cargo test -p agent24-agent --test p1_readiness p02_complete_marker_prompt_survives_same_process_approval -- --ignored --exact --nocapture` | `101`，`/tmp/a24-p1-probes-extra/p02-approval-inprocess.log` | 原始 snapshot 中 marker prompt 和历史 marker 都完整；审批后真实 provider 收到 marker prompt 原文，但没有收到 marker history，断言 RED。不是 snapshot 丢失。 |
| `cargo test -p agent24-agent --test p1_readiness p02_complete_marker_prompt_survives_cold_approval_restart -- --ignored --exact --nocapture` | `101`，`/tmp/a24-p1-probes-extra/p02-approval-cold.log` | 子进程实际退出且父进程重开文件库、审批、resume 成功；provider 收到 follow-up 和普通 assistant history，但完整 marker history 不在 provider 输入中，断言 RED。原 run snapshot 前缀逐行不变。 |
| `cargo test -p agent24-agent --test p1_readiness -- --nocapture`（最终源码） | `0`，`/tmp/a24-p1-probes-extra/default.log` | Agent target 6 passed / 6 ignored；其中新增两个 RED opt-in，child fixture 正常默认运行时 no-op。Memory target 2 passed / 1 ignored。 |
| `cargo clippy -p agent24-memory -p agent24-agent --test p1_readiness -- -D warnings` | `0`，`/tmp/a24-p1-probes-extra/clippy.log` | 两个 readiness integration target 通过定向 clippy。 |
| `cargo fmt --all --check` | `0`，`/tmp/a24-p1-probes-extra/fmt-check.log` | 全 workspace 格式检查通过。 |

日志记录最终源码 SHA-256：`rust/crates/agent24-agent/tests/p1_readiness.rs` 为补充后实算值（见本节收束指纹）。现有 `p02_complete_marker_prompt_must_reach_provider`、`p02_complete_marker_history_must_reach_provider` 未删除、未反转期望；这两条新测进一步证明审批恢复路径也未解决 provider-view 的裁剪问题。现有历史报告关于“durable raw 保留、provider view 丢失”的区分继续成立。

### 8.2 P03：HTTP route、qualification 与撤回承诺边界

本次没有把组件级 `p03_chat_prelude_and_fallback_send_observation` 升格为 HTTP 证据，也没有宣称 H04 已证实为 bug。其仍只通过公开 `RunManager::chat_memory_prelude`/`ModelRouter::complete_served` 与确定性 provider mutation hook，观察 prelude 后撤回/pause 和首 provider 返回 `Unavailable` 后撤回/pause 共 4 个组合；最终 provider 都收到已构造的原 messages。此前命令 C3/C6 及其原始日志仍是唯一运行证据，本轮未伪造新的 HTTP 运行日志。

这份 checkout 的 `rust/apps/agent24d/Cargo.toml` 只有 binary target、没有 `[lib]`；`src/main.rs` 把 `routes`、`server` 声明成 binary 内部模块。独立 integration test target 无法导入私有 `AppState`/`routes::post_chat`，而唯一可直接装配完整 provider/AppState 并调用 router 的现有 hooks 是 `routes.rs` 内 `#[cfg(test)] mod tests` 的私有 fixtures（该处已有 `state_with_stub()` 与真实 `build_router_with_modules()`），但本 task 限定只能改两份 `p1_readiness.rs`、报告或新增独立 HTTP test file。Memory 的公开接口允许 `forget`、pause 和新增 assertion；`Assertion::qualified` 可在新 assertion 插入前设 false，但没有公开 API 在 prelude 后把已有 qualified assertion 改为 unqualified，`KvStore` pool 与 `AssertionLedger::insert_tx` 也均为 crate-private。故“资格在 prelude 后失效”无法从当前公开 hooks 确定性构造；不能用删除 assertion冒充 qualification 变化。

**最小下一片方案**：在 daemon 内添加独立 `#[cfg(test)]` readiness module 文件并由 `routes.rs` 测试模块引用，复用 `state_with_stub` 的 AppState/真实 Router，使用 `Notify` barrier provider 在 post_chat 已完成 prelude 后暂停；同一 test 通过现有 DELETE/settings HTTP 路由执行撤回、pause，再放行首 provider 或让其返回 `Unavailable`，第二 provider spy 读取实际 request。附 no-session `/api/v1/chat` 对照，断言 provider body 原样只有客户端 messages。qualification 场景需经 crate 内 test-only SQL fixture 在 barrier 等待期间把隔离 assertion 置 `qualified=0`，不添加生产/HTTP 合同。测试结果只能作为发送时点观察；当前 DELETE 契约是阻止新的 session recall 并保留原 conversation，并未定义 fallback 对已构造请求重新授权，因此要等承诺冻结后才能把发送旧 recall 判为契约违规。该观察不能撤回已送出的第一 provider 内容，也不应在网络 await 期间持 SQLite 写锁。

### 8.3 H02：同秒 reassert 审计 ID

本轮没有用“循环跑得快”当作确定性证明，也没有修改生产时间逻辑。源码证据：`agent24-memory/src/writer.rs::commit_with_audit` 在事务中直接取 `now_iso8601()`（秒精度），随后用 `checksum(format!("{now}|{:?}", c.evidence))` 生成 `reassert-{assertion_id}-...`；相同 assertion/evidence/秒值会得到相同 audit event ID，第二次 append 可能撞唯一键。公开 `KvStore::write_gate()` 返回的 `WriteGate::new`/pool 为 crate-private，现有 API 没有可注入时钟；组件探针只检查不同新 evidence 的重记路径，不能覆盖该碰撞。

**为何独立设计**：必须固定 reassert 两次拿到的完全相同秒值，同时保留真实 SQLite 事务、事件唯一键和公开行为；纳秒 sleep、循环或“通常小于一秒”都不能作为控制。最小测试专用接缝补丁建议：在 `writer.rs` 的单元测试作用域新增私有 `commit_with_audit_at(candidate, qualified, verdict, now: &str)`，让现有生产 `commit_with_audit` 仅负责取 `now_iso8601()` 并委托；只在 `#[cfg(test)]` 模块直接调用 `*_at`，无需公开 API/feature/依赖或写入时钟状态。固定 `now` 执行 commit → forget → reassert(evidence=ev2) → forget → 同 assertion/evidence 再 reassert，断言第一、第二 reassert 的 ID 相同且第二次事务按现状失败；同时保留不同 evidence/不同时间可区分的控制。这个 seam 涉及 writer 私有实现，因此应作为独立 memory-crate 单元测试补丁审阅，不应把未改生产的本任务伪报成已完成时钟注入。

### 8.4 本轮门禁与来源指纹

最终运行日志目录：`/tmp/a24-p1-probes-extra/`，包括新增 RED 原始命令、stdout/stderr、原 exit、默认定向测试、clippy 与 fmt。定向默认测试合计 8 passed / 0 failed / 7 ignored（Agent 6/6、Memory 2/1）；这不是 workspace 门禁或 readiness 全绿。没有跑全 workspace test/clippy，没有提交、推送、开 PR，也未使用 Mini、真实模型或真实 DB。

- `rust/crates/agent24-agent/tests/p1_readiness.rs` 最终 SHA-256：`af80ddb1869eec8fb74b916835cf691f5e2ab26ab73a31d01aef65216f89a439`。
- `rust/crates/agent24-memory/tests/p1_readiness.rs` 未修改；最终 SHA-256：`259f8826d906d359897d88202c5b0ee0acced2ac12fe30a91c9f400cea5bd3fa`。
- 本报告的最终 SHA-256 在交付消息中记录，避免自引用哈希。

### 8.5 P03：真实 HTTP router 探针补齐

本片仅增加 daemon 测试接缝和独立 `#[cfg(test)]` 模块，没有改生产逻辑、public API、schema、依赖或两份既有 `p1_readiness.rs`。新探针由 `routes.rs` 的测试模块引用，内部自行装配 `AppState` 并调用 `build_router_with_modules` 的完整 Axum router；provider 使用 `Notify` barrier，因此变更确实发生在 `/api/v1/chat` 完成 prelude、首次 provider 已收到请求之后。

覆盖结果：

- HTTP DELETE 撤回后释放 barrier：首 provider 收到原已构造 recall。
- HTTP PUT `{"enabled":false}` pause 后释放 barrier：首 provider 收到原已构造 recall。
- 首个 Local provider 在 barrier 释放后返回 `Unavailable`，第二个 Local spy 经 router fallback 收到同一 recall；撤回通过 HTTP DELETE 在两 provider 之间完成。Local tier 是必要条件，因为 `/chat` 的默认任务隐私级别会屏蔽 Remote provider。
- file-backed 临时 SQLite DB 中，独立 `SqliteConnection` 在 barrier 等待期间把 assertion 的 `qualified` 从 1 改为 0；无 SQL 写锁跨越 provider await，最终 spy 仍收到已构造 recall，随后独立查询确认该行确为 `qualified=0`。
- 无 `session_id` 的对照只向 provider 发送单条原始客户端 user message，没有 memory prelude。

这些结果是发送时点观察，和 §8.2 的 OpenAPI 核对一致：DELETE 定义为阻止新的 session recall，pause 定义为影响新启动的 run；两者均未承诺撤回一个已开始 HTTP chat 内已构造的 request，也没有据此宣称 H04 违约。qualification 变更亦只观察现有 request 的快照，不新增产品承诺。没有使用真实模型、用户库或 Mini。

执行日志位于 `/tmp/a24-p1-http-probes/`。下列命令均在 `rust/`，使用共享 `CARGO_TARGET_DIR=$HOME/Dev/auraai/Agent24/rust/target`，测试/检查前 `ulimit -n 4096`：

| 命令 | 结果 | 日志 |
|---|---|---|
| worker 原始定向 HTTP 测试（原报告 argv 与多 target 日志不完全一致，不作为精确复跑命令） | daemon 内 5 passed、0 failed；其它 target 为 0 tests | `http-test.log` |
| 协调者独立复跑：`cargo test -p agent24d --bin agent24d p1_http_readiness -- --nocapture` | exit 0；5 passed、0 failed，仅 binary target | `coordinator-rerun.log` / `coordinator-rerun.exit` |
| `cargo clippy -p agent24d --bin agent24d --tests -- -D warnings` | exit 0 | `clippy.log` |
| `cargo fmt --all --check` | exit 0 | `fmt-check.log` |

本次源码指纹：

- `rust/apps/agent24d/src/routes.rs`: `a80382f789bacc84d5f2725bd639ab1458a60567a4c01db71b83b5bcf84cf934`
- `rust/apps/agent24d/src/routes/tests/p1_http_readiness.rs`: `99f43651019dda94cfa645ba1bb7787c22e6c421e434c256a2e60f36da859f17`

本片未运行 workspace test/clippy、没有提交、推送或开 PR；定向结果不代表 workspace 门禁全绿。此前已存在的两份 `p1_readiness.rs` 和本报告前文均保留。

## 9. H02 固定时钟同秒探针：真实 RED（后续补证）

基线改为已同步集成 `8535154fdc4aeb1aaffb415066824224a0124d6a`；该基线的生产 Rust 树与最初 `f16e65a9` 相同。独立任务 `ab/memory-p1-25-clock-probe` 只改 `writer.rs`，diff 164+/1-；私有 wrapper 传入原 `now_iso8601`，仅替换原锁内、读取 existing 之后的取时调用，未重写 Assertion/MemEvent 构造时刻或 ID 分配算法。其余变化是 `cfg(test)` 合成夹具。

- 固定 T=`2999-01-01T00:00:00Z`：commit(ev1) → forget → reassert(ev2) → forget → reassert(ev2)。第二次重申真实返回 SQLite 2067：`UNIQUE constraint failed: mem_events.id`。没有靠 sleep 或运行速度碰撞。
- RED 测试期待第二次成功及两条不同 reassert ID，故失败；默认 `ignored` 仅用于记录已知未修复契约，不是生产通过凭证。
- 三项 GREEN：失败转换后事件数量/owner 用量不增且断言仍撤回；仅变更第二次时间或 evidence 时成功；合法原 commit 的 active retry 不新增转换。
- 回滚观察当前利用同秒碰撞触发失败；将来修复 ID 时应改为独立故障注入控制，不能保留“碰撞必须失败”作为最终产品要求。

| 命令（`rust/`、共享 target、`ulimit -n 4096`） | 实测结果 |
|---|---|
| `cargo test -p agent24-memory --lib writer::tests::h02_same_second_same_evidence_two_transitions_must_succeed -- --ignored --exact` | worker 与协调者独立复跑均 exit 101；明确为上述 SQLite ID 碰撞 |
| `cargo test -p agent24-memory --lib h02_` | worker 与协调者均 exit 0；3 passed / 1 ignored |
| `cargo test -p agent24-memory --lib` | worker exit 0；247 passed / 1 ignored |
| `cargo clippy -p agent24-memory --all-targets -- -D warnings` | worker exit 0 |
| `cargo fmt --all --check` | worker exit 0 |

源码 SHA-256：`656490806adf1c630d330d01ca3b0d6fbd70390caa980512c8a318feb15b4011`（`writer.rs`）。命令与日志原目录 `/tmp/a24-p1-clock-probe/`，协调者另存带哈希归档；未运行本片 workspace 全测试、未提交/推送。结论只把“同秒风险待复现”升级为确定红证；H02 审计匹配与同秒 ID 缺陷均未修复，不批准新生命周期或 purge。
