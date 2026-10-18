# P02：运行输入快照的私有来源契约

状态：**待外部 exact-head APPROVE；本文选定方案，但未授权实现或上线。**
本轮核查基线：`86808d30f683262fcf7bc92d6d666fbede5850f6`，分支 `ab/m1-memory-hardening-06-provenance-contract`；原稿基线为 `ab125de262827bd0f1c6680b427d9c241ccff785`。
本文仅处理 P02 完整 marker 用户输入被误删及其冷恢复来源边界；不扩大到 M3、全量 Hindsight、NoLoss、Workspace 权限或自动回放。
依据：[冻结 M1 计划](M1-PLAN-v2.md) 的 M1-T07.1、[OpenAPI](../../protocol/openapi.yaml) 的 `putMemorySettings`；用户已批准 Unknown 旧运行阻止继续、保留数据、提示显式新建。
交叉输入：`/tmp/a24-memory-hardening/resume/P02-{MINIMAL-PLAN,ASTRA-CROSS-REVIEW}.md`、`P02-PROVENANCE-CONTRACT-DRAFT.md`、`/tmp/a24-memory-hardening/p02-contract/CONSUMERS.{json,md}`；本轮按同目录 `CONTRACT-REVIEW.{json,md}` 的 R1–R3 修订，尚待独立复核。
协调器告知 H01 #910 已合入方向分支（`86808d30f683262fcf7bc92d6d666fbede5850f6`）；本工作区不改 base，不追加 H01，不开 release PR。

## 1. 已核实边界

下表路径以仓库根为起点；函数名是核查定位依据，描述的是当前行为。

| 路径 / 函数 | 当前事实与契约接缝 |
| --- | --- |
| [agent lib](../../rust/crates/agent24-agent/src/lib.rs) `execute` | 真正 recall 返回值被插入 snapshot；notice、prior context、prompt 同批落库，成功后才调用 `run_loop`。 |
| 同上 `request_messages_for` / `prune_stale_recall_message` | 每轮按首条 user + 完整 markers 猜来源；active IDs 为空时能删除用户原文。 |
| 同上 `thread_to_messages` / `resume_run` / `drive_resume` | thread 转 Msg 丢 seq；冷恢复先结算批准工具，后调用 provider；同进程等待不走 `drive_resume`。 |
| [store repo](../../rust/crates/agent24-store/src/repo.rs) `append_run_messages_tx` / `list_run_messages` | 批写为 `BEGIN IMMEDIATE`；每行分配 `MAX(seq)+1`，返回 unit；`RunMessage` 保有 `(run_id,seq)`，未有来源分类。 |
| [0004 migration](../../rust/crates/agent24-store/migrations/0004_run_messages.sql) | `(run_id,seq)` 是主键；普通 role/content/tool 字段不能证明宿主来源。 |
| [session memory](../../rust/crates/agent24-agent/src/session_memory.rs) `recall` / `owner` / `active_ids` | recall 给真实 `(Msg, ids)`；owner 是宿主配置；资格查询按该 owner，不能证明某条消息是谁插入。 |
| [daemon server](../../rust/apps/agent24d/src/server.rs) `session_memory`；[os memory](../../rust/apps/agent24d/src/os_memory.rs) `ensure_personal_recorded` / `partition_key` | personal owner 经 catalog 计算后传 `with_owner`；不是 Run 的 session/workspace，也不是请求字段。 |
| [session log](../../rust/crates/agent24-memory/src/session_log.rs) `append_turn`；session memory `context` | 原始 turn 有 owner/session/Origin；没有 run 消息实例绑定，context 还丢事件 seq，不能补证旧 snapshot。 |
| [events hub](../../rust/apps/agent24d/src/events.rs) `EventsHub` | `memory.recalled` 是 broadcast，无持久 replay；事件缺席不等于 NoRecall。 |
| [approval route](../../rust/apps/agent24d/src/approvals.rs) `decide_approval`；[broker](../../rust/crates/agent24-policy/src/lib.rs) `resolve` / `apply_decision` | 当前先 resolve + wake，再 resume；wake 后可以创建免问授权并执行工具，不能只在后续 provider 检查。 |
| agent lib `restore_pending_approvals`；[resume](../../rust/crates/agent24-agent/src/resume.rs) `assess_restore` | 当前 TTL 在 thread/tool/payload 后检查审批 `created_at < cutoff`（resume:201–258；lib:999–1049），Unknown 保留前须先独立判断真实过期。 |
| server:1617–1667；store repo:440–449,721–760；[workspace orphan](../../rust/crates/agent24-store/src/run_workspace_orphan.rs):171–219 | 当前先 deadline timeout/recovery、再 restore、再两个 orphan sweep；Unknown 不能因来源错误走通用 abort。 |
| [protocol types](../../rust/crates/agent24-protocol/src/types.rs):431–434；OpenAPI:2736–2740；[runs route](../../rust/apps/agent24d/src/runs.rs):215–225 | `Run.error` 仅 failed；GET/list 当前直接返回存储 Run，无 continuation 诊断。 |
| [ACP](../../rust/apps/agent24-cli/src/acp.rs) `run_prompt`:162–245 | 订阅 WS 后创建 run，仅 WS 终态结束请求，无 REST 对账；TUI:282–290,376–388 已对账，desktop Runs:24–41 已每 3 秒轮询。 |

外部 source03 probe 已记录真实 provider spy 收到 `messages=[]` 的 RED；本工作区没有该新增测试，未重跑或声称新测试通过。

## 2. 选定持久表示（以下全部是待批准提案）

新增 **agent24-store** 私有表 `run_input_provenance`，一 run 一行，O(1) 元数据；不用 mem_events、公共 Run/Msg、tool 字段或新消息 UUID。
在当前最后一个 store 迁移 `0014_decision_log.sql` 后新增 `0015_run_input_provenance.sql`；与 memory crate 的迁移序列无关。
若同步后 0015 已占用，停止派存储实现并重新审查具体迁移号；不覆盖已有迁移或擅改冻结契约。

```sql
CREATE TABLE run_input_provenance (
  run_id TEXT PRIMARY KEY NOT NULL REFERENCES runs(id),
  version INTEGER NOT NULL CHECK (version = 1),
  state TEXT NOT NULL CHECK (state IN ('no_recall','host_recall')),
  snapshot_len INTEGER NOT NULL CHECK (snapshot_len >= 0),
  host_seq INTEGER,
  host_owner TEXT,
  CHECK ((state = 'no_recall' AND host_seq IS NULL AND host_owner IS NULL)
    OR (state = 'host_recall' AND host_seq IS NOT NULL AND host_seq >= 0
      AND host_seq < snapshot_len AND host_owner IS NOT NULL
      AND length(trim(host_owner)) > 0))
);
```

`Unknown(reason)` 不写成数据库默认；无行就是 Unknown。迁移只建空表，不给旧 run 添 NoRecall，不 mass backfill，不重写 run/messages/ledger。
`KnownNoRecall(v1,run_id,snapshot_len)` 明确表示宿主实际插入零条 recall；即使用户/历史内容完全仿形，也不转为 HostRecall。
`KnownHostRecall(v1,run_id,snapshot_len,host_seq,host_owner)` 最多指向一个实际插入实例；owner 保存此次 `SessionMemory::owner()` 的完整私有分区 key。
不建指向 host message 的级联删除，不加公共 owner 字段；run 外键沿用 [Store::open/open_memory](../../rust/crates/agent24-store/src/lib.rs) 的 `foreign_keys(true)`。
初始 snapshot 从空 thread 写入，实际 seq 连续为 `0..snapshot_len` 的半开区间；该边界用于核对完整性/重试，**不是** host 身份判据。
所有字段无默认值；未知版本、非法类型/状态/组合、缺行、引用越界/缺失、错误 role/工具字段、host 块无法按现有 renderer 格式解析、读取错误皆解码为 Unknown。
host 正文不重复存储，ids 不另存；只有先证明实例后，才解析它的现有 recall 格式。此契约不是抵御本地数据库任意篡改的完整性系统。

## 3. 原子写入、实例绑定与精确重试

新增专用私有 store 写入入口；普通 `append_run_message(s)_tx` 继续服务 tail，不允许它替初始快照补来源。
宿主在 `execute` 收到 `Some((recalled,ids))` 时给**这个对象**附一次内部插入标签；禁止从内容、role、数组第零条、当前 active IDs 或 raw prompt 等值关系补标签。
单个 `BEGIN IMMEDIATE` 内：读 run/来源/初始 thread，逐行 `INSERT ... RETURNING seq`，收集真实分配的 seq；把标签对应的返回 seq 写入来源行，核验，最后 commit。
只有空 thread + 无来源 + 此次 fresh snapshot 创建动作可首次写入；非空旧 thread 无来源必须拒绝，绝不“认领”为新快照。
消息角色、nullable content、工具数组及 tool_call_id 无损保存；序列化失败不得用 `[]` 伪造成功；全体消息与来源行同事务成功或全体回滚。
空 snapshot 也必须提交 `KnownNoRecall(snapshot_len=0)`，不能沿用当前 empty batch no-op；实际 `execute` 仍有 prompt。
memory 未配置、关闭、budget=0、不足以装一条、零命中、全部裁掉、recall 错误后按已有策略继续：仅在实际没有插入 host 对象时写 NoRecall；暂停 notice 不是 recall。
retry：来源所有字段及初始区间逐条消息（含原首次 `created_at`）完全一致才返回原结果；重用首次写入参数/时间，不追加第二份；合法 tail 不参加初始快照等值比较。
任一字段/行/长度冲突、只有部分消息或只有来源、旧无来源 thread 均返回内部 `SnapshotConflict`；**禁止 INSERT OR IGNORE / REPLACE** 掩盖不一致，不修改已有行。
commit 成败不明须读取确认上述全值；无法确认即停止，不按“未写入”重跑 recall/context 或工具。commit 未确认前零 provider / 零工具。
首次普通持久化失败仍使用现有 `memory_snapshot_persist_failed` fail-closed 路径；Unknown 来源拒绝继续不借用它制造终态。

## 4. 统一私有 loader 与 provider 视图

fresh 在 commit 后与 cold 一样使用新私有 loader，在同一 SQLite 读事务读 provenance + 全 thread；返回保留 `(run_id,seq,Msg)` 的内部 snapshot 与 Known/Unknown 判定。
初始区间必须完整、连续；tail 的现有工具配对/恢复校验仍保留。普通历史读取 `list_run_messages` 不强制来源解码，终态 prior-context 清理仍按既有 `execute` 范围执行。
`thread_to_messages` 不能先丢 seq 再查同文定位；内部映射随 append 维护，追加 assistant/tool 不更改 host ref，normalize 不参与实例定位。
每轮 `request_messages_for` 先验证 private Known 及其映射；只复制 host_seq 指向的消息，沿用 `active_ids` → `prune_stale_recall_message` → `normalize_for_provider`。
HostRecall owner 必须与当前宿主授权配置的 personal partition 完全一致；缺 memory、owner mismatch/无授权为 Unknown，禁止转到外 owner ledger 查询或回退默认 owner。
资格读取沿用 [AssertionStore::beliefs_as_of](../../rust/crates/agent24-memory/src/assertion.rs) 的 owner、qualified、双时态及 retract/supersede；其读取失败只排除**已证明的 host 实例**的 request copy。
NoRecall 不扫描 markers、不为用户文本查 active IDs；所有 user/history/assistant/tool/notice 字节与相对顺序保留，最后允许既有 normalization 的 system 合并/user 连接。
ledger 读取失败与来源读取失败不同：前者有已证明实例可排除，后者 Unknown 不发送整份 request。原始 input/thread/history/ledger 均不原地裁剪。
Known snapshot 审批恢复不重新 recall/context，不重查 pause 改写旧 snapshot/notice；Unknown gate 是独立原因，保留 `putMemorySettings` 的既有豁免。

## 5. Unknown 的批准行为与工具边界

仅未完成 run 的 continuation 被本门拒绝；已完成/失败/取消 run 的原始历史与普通 GET/list 继续可读。Unknown 不发 provider，也不执行下一项已批准工具。
先从 `assess_restore` 提取最小共享纯谓词 `approval_resume_expired(created_at,cutoff) = created_at < cutoff`；输入是审批时间而非 run 时间，沿用 `iso8601_before(RESUME_TTL)` 的 72 小时与固定 UTC 比较，等于 cutoff 不过期。
启动保护分类、`restore_pending_approvals` 与 `drive_resume` 在 Unknown 分支前共用此谓词；真实过期 pending 沿原 TTL `abort_one_approval` → orphan 路径，drive 沿原取消路径，绝不进入保护集合；无需读来源/thread/payload 才证明过期。
未过期 Unknown 保留 run/approval/thread，不调用通用 `assess_restore/abort_one_approval`，不发可执行 `approval.required`；仅 Known 再走原 thread/tool/payload/Workspace authority 校验，权限规则不放宽。
审批 route 在读取授权范围内的 approval/run、检查既有 resolved/invalid-decision 规则后，**在 `broker.resolve` 的 CAS/wake 之前**作来源预检；Unknown 返回错误，approval 保持原状。
`resume_run` / `drive_resume` 在转 Running、RunStarted、plan unlock、`settle_resumed` 授权副作用或 `execute_preapproved` 之前检查同一 private snapshot；Unknown 原样返回，不 finish_failed/cancelled。
同进程等待、fan-out 后续 call 与每轮 provider 均必须使用同一已加载来源映射；批准唤醒后的 `apply_decision` 授权/实际 tool execute 前也须通过门，不能只靠 cold 入口。
具体接线：daemon 给 broker 装配一个私有 continuation-check（共享 loader + 宿主 owner）；`resolve/apply_decision/verdict_from_row/BrokerGate::settle_resumed` 在接受/应用批准前检查，覆盖 waiter、CAS 输家与免问授权回放。
私有 typed blocked stop 沿 broker → tools → agent 返回；`run_tool_call/settle_parked_call/run_plan_proposal` 直接停续，不落入现有 Denied、AbortRun、finish_* 或工具结果路径；工具实际执行前再次检查，不能用批准当来源证明。
若中途来源不可确认，停止下一副作用，不制造 denied tool result、回写 ledger 或重放已执行工具；已有副作用不撤销、不补跑，已有批准不解锁新权限。
启动保留现有 deadline timeout/recovery 顺序；在 restore/orphan 前读取完整 unfinished 候选及适用审批时间，先排除真实过期，再由 loader 得到私有 `protected_run_ids`，包含未过期 pending、已决定及无审批的 Unknown。完整候选/时间读取失败须传播启动错误、停止破坏性 sweep；单项来源读取失败只判 Unknown。
`repo.rs::sweep_orphan_runs` 的 UPDATE 排除该集合；[workspace orphan](../../rust/crates/agent24-store/src/run_workspace_orphan.rs) `sweep_workspace_orphan_runs` 在现有每 run terminal composition 前同样跳过这些 ID，尾部 remaining 断言也排除；不改 lease/root/权限算法，不引入 Workspace paused 功能。
现有 `timeout_expired_approvals` 的 `expires_at < now`、`recover_timed_out_approval_runs` 候选与既有周期照常运行，实际过期不受保护；不改 deadline/TTL、不为已决定或无审批 run 发明 run-age TTL、不加 timer 服务。用户显式 cancel 仍可用，不把来源失败翻译成 payload/TTL 失败。
新 run 只由用户显式操作创建，默认新会话/不带旧 session，不自动复制 Unknown recall、旧 thread、tool state、summary/history 块。
若用户明确继续某 session，只能读取独立 owner/session 绑定的 SessionLog 事件及其既有 Origin/summary 规则；没有独立可信来源就不带该 session，不能以“在旧 run 出现过”为证据。原始用户请求经用户重新提交可成为新输入，不自动迁移未知宿主内容。
NoRecall/HostRecall 只决定来源剪裁资格，绝不授予 memory 指令权威、工具权限或改变 plan/Workspace 授权。

## 6. 选定公共错误与消费者（待 exact-head 审查）

新增私有 typed `SnapshotProvenanceUnknown`（内部 reason 分类）与 `SnapshotConflict`；来源错误对外统一开放字符串码 **`run_provenance_unknown`**，不公开 Unknown/Known、owner、seq、ids、SQL 或 foreign run 存在性。
授权检查与原有 not-found/resolved precedence 不变；来源 gate 只检查调用者本已可读的 run/approval，不能为外部 run ID 作存在性探测。
审批 POST 拒绝为 **409**，用已有 [ErrorEnvelope/ErrorBody](../../rust/crates/agent24-protocol/src/types.rs) 和 [error_response_with_hint](../../rust/crates/agent24-domain/src/http.rs)。
固定 message：`无法安全继续此运行，请显式新建运行；原始输入和历史仍可查看。`；hint：`重新提交原始请求；不要复制未知召回或旧工具状态，已有副作用不会自动重放。`；details 省略。
来源 read error 也使用相同码/泛化正文，不漏底层错误；内部日志记录原因。实际 run 主行不可读仍走原有 storage error，而不是伪造可读历史。
保留 **`Run.error` 仅 failed 的原语义**；GET/list 对已被实际 continuation gate 拒绝的未完成 run，新增可选响应字段 **`continuation_error: ErrorBody`**，正常/终态省略，既有 error 原样返回。它只投影诊断，不持久化、不证明来源，不增加 RunStatus/endpoint/WS 事件。
RunManager 私有 `HashMap<run_id,ErrorBody>` 仅登记真实 gate 拒绝；GET/list 在授权过滤后的可读 Run 副本上查表，不调用来源 loader、不因无来源行造 hold、不修改库。fresh snapshot 未提交且尚无门拒绝时，GET 必须不含诊断；不需要 fresh 回执或 snapshot-pending 豁免。
进程重启从空诊断表开始，在接收请求前按第 5 节完整候选、真实过期优先及来源/当前宿主 owner 检查重建拒绝诊断；不序列化旧诊断、不从标记文本猜来源。候选读取失败中止启动，不能带不完整保护集合继续 sweep。
诊断在真实 gate 再次验证成功时清除，但 GET 不触发验证/自动 resume；终态（含用户 cancel、真实 expiry）或 run 删除时清除，投影同时以已读 run 的非终态作兜底。只绑定 run_id，不跨 run/session 继承，任务停续仍保留以供查询；无通用 hold/status 服务。
来源判断继续每次检查 private snapshot 与当前宿主 owner；诊断既不授予批准也不触发 ledger 查询。HTTP 保留已有 root/授权/404 precedence，只为本已可读的 run 投影，正文/hint/details 不泄露分区 owner、文件根、来源内容或 foreign ID；GET 不暴露内部 reason。
公开 wire 是可选响应字段的加法变化；旧客户端可忽略它但不能因此声称已观察 hold，WS-only 客户端必须用现有 GET/list REST 对账。仅 409 无法覆盖无 POST 的启动/异步拒绝，故不采用。
不发伪造 `run.failed`/`run.cancelled`；来源 gate 失败与第一次快照持久失败分开。Luna 建议 fresh Unknown 直接 failed 在此不采纳。

| 已有消费者 / 函数 | 必须同步的最小变化 |
| --- | --- |
| daemon `runs.rs::get_run/list_runs`、`approvals.rs::decide_approval` / `runs.rs::map_agent_error` | 非终态 continuation_error 只读投影、批准前预检与 typed 错误映射；不能继续“记录 resume 错误后返回成功”。 |
| [desktop api](../../apps/desktop/src/renderer/pages/agent/api.ts) `decideApproval/errorMessage` | 当前吞全部 409；只吞 `approval_already_resolved`，其余显示 message/hint，保留 provenance 提示到该 run 被替换/用户关闭。 |
| [Approvals](../../apps/desktop/src/renderer/pages/Approvals.tsx) / [Runs](../../apps/desktop/src/renderer/pages/Runs.tsx) | 轮询 continuation_error 并显示 message/hint 与显式新建指引，阻止本地继续按钮；轮询成功不抹 per-run 提示，取消仍可用，列表/原文保留，不自动复制或创建。原 failed error 展示保持。 |
| [TUI](../../rust/apps/agent24-cli/src/tui/mod.rs) `Conn::list_runs/decide/perform`；[ui](../../rust/apps/agent24-cli/src/tui/ui.rs) `draw_events` | 当前吞全部 409，且 perform 忽略 Err；按 code 区分并展示 Err，现有 REST reconcile 解码响应 transport，渲染 continuation_error/hint 与原 failed error，不等伪造 run.failed。 |
| ACP `acp.rs::run_prompt`:203–245；`session/prompt` 分支:113–115 | WS 等待中并行每 3 秒低频 GET `/api/v1/runs/{id}`，单请求 3 秒超时、至多一个请求、跳过错过的 tick；REST 不阻塞 delta/终态。看到 continuation_error 返回 Err，沿现有 `AGENT_ERROR` 回复当前请求并带显式新建指引；不 cancel/failed daemon run、不自动新建/重放。旧 daemon 缺字段照常等待。 |

## 7. wire / codegen 的明确影响

持久 `Run`/公共 `Msg/RunCreate/RunInput` 不加来源或诊断字段；选定 Rust `agent24-protocol/src/types.rs::RunResponse` 薄响应 transport：flatten 原 `Run`，另有 `Option<ErrorBody>` continuation_error，`serde(default, skip_serializing_if = "Option::is_none")`；POST/create/cancel 返回原 Run，数据库 row decoder 不变。
OpenAPI 新增 `RunResponse`（原 Run + 不在 required 的 continuation_error: ErrorBody），仅 GET/list 响应引用它；补 `decideApproval` 409 例子/开放错误码及 GET/list 说明，保留 Run.error 描述。该可选加法是明确的缺席字段例外，不把现有必填 nullable 字段一并改成可省略。
Rust TUI/ACP GET/list 解码 `RunResponse`；desktop `api.ts` 手写 Run 补同名可选字段并用含 hint 的现有 ErrorBody 形状，409 同步展示 hint；生成类型不会自动修手写类型。旧响应缺字段须成功解码，旧 Run 客户端忽略额外字段，严格校验客户端须升级；缺席绝不证明 Known。
若同步 Rust `ErrorBody.code` 注释（统一开放码列表），[export-schema](../../rust/crates/agent24-protocol/src/bin/export-schema.rs) 会机械更新 `protocol/events.schema.json` 描述；不新增 WS 通知。
实现阶段在 rust/ 用共享 target 跑 `cargo run -p agent24-protocol --bin export-schema > ../protocol/events.schema.json`；根目录跑 `pnpm lint:openapi`、`pnpm gen:api`，核对 `packages/api-client/src/{openapi,events}.d.ts` 的 REST 可选字段/注释变化；REST-only RunResponse 不进入 Event schema，再次生成应无 diff。
私有 store schema 本身无 API 生成影响；不要为 source 改公共 RunCreate/RunInput/Msg 序列化，也不手改生成文件。本次文档工作不跑这些生成/构建命令。

## 8. 切片、验收与冻结门

| 切片 | 生产行预算 / 依赖 | 合入前独立验收 |
| --- | --- | --- |
| S1 私有迁移、typed decoder、原子 writer/loader | ≤180；依赖本文 exact-head APPROVE | 旧库零回填、empty NoRecall、坏版本/引用/读取；联合事务前后故障、精确 retry/conflict 与 reopened DB。单独合入只提供未启用私有能力。 |
| S2a fresh/provider 实例映射 | ≤120；S1 | source03 完整 marker RED→GREEN；host/prompt/history 同文只删真正 host；每轮 spy 有实际调用；active/撤回/资格/时态/ledger error；所有原文保真。 |
| S2b1 restore/启动保留/纯 expiry；S2b2 broker/工具安全门 | 每片 ≤120；S1 + S2a，b2 接 b1 | in-process/cold/plan/fan-out；未过期 Unknown 工具/provider 均 0且行不变；审批 cutoff 前/等于/后、deadline 及 workspace remaining 对照；Known/NoRecall control 实际调用，pause 豁免。 |
| S3a typed HTTP/诊断投影/响应 transport/protocol；S3b desktop；S3c TUI；S3d ACP | a/b/c 每片 ≤100，新增 d ≤80；S2b，消费者分别提交 | 409 不吞；fresh 提交暂停 GET 不误 hold；同库重启 Unknown 可见；诊断清理、读取不写库、error failed-only、字段缺席/旧客户端兼容、root/owner 无泄漏；显式新 run 不复制 Unknown；机械 codegen/客户端门禁。 |
| S3d ACP 的独立验收 | 计入上述 ≤80，不默增其他切片；依赖 S3a | WS 持续连接且无终态，注入真实异步 gate 拒绝；健康 GET 最迟既有在途请求 ≤3 秒 + 下一 tick ≤3 秒 + 新请求 ≤3 秒内报当前 ACP 错误（≤9 秒）；daemon 状态/history/approval/副作用不变；正常 delta/终态、旧 daemon 缺字段与 transient GET 超时对照，超时不伪造 hold。 |

本任务最新口径：每 task 默认 ≤300 **生产 churn**、测试/文档排除，一般 ≤500；上述更小切片预算是估算，超出先拆，不沿用已撤回的 E10 总行数口径，不以估算证明实现完整性。
切片分别过对应 Rust/TS/protocol 门禁；验收测试先作负控制，provider/tool spy 是主 oracle，不用 marker helper 证明来源，不以“从不调用”假通过。
Luna 矩阵 a–h 保留；同一来源路径负控制覆盖无来源/partial/duplicate/empty 与 cold reopened DB，同文 host/prompt/history 只裁 host；Known 恢复和 NoRecall 均须 provider 实际调用，工具 control 实际执行；原审批/ledger/原文不变。R1–R3 新验收均未运行。
S2a/S2b 不能提前启用 Unknown 拒绝而 cold 工具仍先执行；全部 gate 与 S3 HTTP/desktop/TUI/ACP 一起启用，不发布静默 hold 中间产品；先合未启用私有能力，再最终启用，不默增预算。
所有存储/授权副作用/公共协议切片必须 PR-Daemon exact-head APPROVE 后合入 `ab/m1-memory-hardening`；先由协调器整合消费者审计并发布本文文档 PR，再冻结其批准 head 后派实现。
本轮仅修订文档；协调器提交修订 SHA 作独立 R1–R3 复核后再走外部文档 PR 门。需审新增响应字段、真实 expiry 优先与 ACP 对账，已批准 Unknown 政策不重问；本地检查不等于外部 APPROVE/运行验收，不授权实现或上线。
