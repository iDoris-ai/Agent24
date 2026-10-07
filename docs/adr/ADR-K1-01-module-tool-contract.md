# ADR-K1-01：通用模块工具契约

> **状态**：Proposed（K1-1；本 ADR 仅提出契约，不授权实现；涉及工具授权与执行协议，PR 须等待 PR-Daemon APPROVE 后方可合入 `ab/kernel`）
>
> **日期**：2026-10-07 · **方向**：Kernel K1 · **任务**：#735 / K1-1
>
> **依据**：[#735](https://github.com/iDoris-ai/Agent24/issues/735)；[PLAN-KERNEL-K1 §2(1)、§4 K1-1](../agent/PLAN-KERNEL-K1.md)；`origin/ab/documenting` 上已采纳的 [ADR-DOC-01 D4/D8/D9](https://github.com/iDoris-ai/Agent24/blob/ab/documenting/docs/documenting/adr/ADR-DOC-01-placement-and-integration.md) 与 [ADR-DOC-02 §4/§6/§7/Q7](https://github.com/iDoris-ai/Agent24/blob/ab/documenting/docs/documenting/adr/ADR-DOC-02-operation-contract.md)；2026-10-07 #735 评论中的授权、隐私、审计与幂等裁决。
>
> 本文行号按 K1-1 起草时的 `ab/kernel` 工作树核对；它们描述**现状**，不是未来实现承诺。

## 0. 范围与边界

本 ADR 定义 Agent24 内核如何把进程外模块声明的工具安全地注册、发现、授权、调用并返回结果。它规定：声明与宿主授权的交集、稳定身份、可信调用上下文、有限且类型化的结果、超时与取消、幂等边界，以及模块不可用时的行为。

Documenting 是第一个消费方，也是通用契约的需求来源之一；它的文档存储、版本、编辑器、PDF/导出、领域操作及业务幂等键仍由 Documenting ADR 和 #701/#702/#703 定义。内核不得加入 `document` 领域路由、存储或业务规则。

本 ADR 不定义授权 UI、信任根/签名验证、隐私标记传播的完整状态机、审计哈希链字段或业务审计界面；分别由 [ADR-K1-03](../ADR-K1-03-enable-authorization-trust-revocation.md)、[ADR-K1-02](ADR-K1-02-session-dataflow-security-modes.md)、计划中的 ADR-K1-04 承接。本 ADR 对其依赖作出兼容约束。此提案不表示对应机制已存在。

## 1. 现状与差距

| 主题 | 已有事实 | 对 K1 工具契约的差距 |
|---|---|---|
| Manifest 与授权 | `DomainOsManifest` 有 `kernel_capabilities`、`model_access`、`host_commands`，没有模块 Agent 工具清单（`rust/crates/agent24-domain/src/lib.rs:480-492`）。Capability 是按内核可提供的能力集取交集（`rust/apps/agent24d/src/domain.rs:62-76,116-122`）。 | 需要新增工具声明和逐工具宿主授权；模块自报的风险/隐私属性不能授予权限。现有 `host_commands` 是反向桌面命令，不是 agent 工具授权。 |
| agent24d callback | `KERNEL_OOP_GRANTS` 仅列 Events/Approval/Memory/Scheduler/Models，`CallbackDeps` 承载内核句柄（`rust/apps/agent24d/src/domain.rs:116-150`）；`mount_package` 的 callback registry 注册事件、调度、模型等宿主方法（`:1438-1490,1538,1610-1644`）。事件 callback 在 handler 内按 capability 授权（`rust/apps/agent24d/src/events_emit.rs:5-9`）。 | 这些是模块调用内核的反向 callback，不是模块贡献 Agent 工具的注册/调用协议；不能把现有 capability grant 当作逐工具授权。 |
| 身份与命名 | OS `name` 是路由命名空间、事件模块名和数据目录的单一来源，其他值必须精确匹配（`rust/crates/agent24-domain/src/lib.rs:470-472,780-803,920-928`）。 | 为工具定义内核生成/校验的稳定全名，避免模块间冲突；工具名不是授权本身。 |
| 工具注册与发现 | `ToolRegistry` 持有静态工具 map 和 allowlist（`rust/crates/agent24-tools/src/lib.rs:305-316`）；`mount_all` 前构建 builtin/MCP registry（`rust/apps/agent24d/src/server.rs:1523-1558`），`adverts()` 只过滤已 allowlist 且可执行的工具（`:466-481`）。 | 当前没有模块向 agent loop 注册工具、根据模块实时生命周期或 `/capabilities` 过滤模块工具的接缝。 |
| 工具调用上下文 | 内建工具的 `ToolContext` 已由宿主持有 `run_id`、`session_id`、`tool_call_id` 与 workspace authority（`rust/crates/agent24-tools/src/lib.rs:35-44,52-114`）。 | 进程外模块工具协议尚未把这些可信关联及获准资源范围作为宿主注入上下文；不得接受模型或模块自报身份来扩权。 |
| 内核到模块传输 | `send_kernel_request` 经当前模块 Generation 发 POST 并受 `KernelLimits` 总时限/响应限额约束（`rust/crates/agent24-os-proto/src/kernel_call.rs:79-94,235-276`）。它读完限额内响应体后只保留 `status`、丢弃 body（`:364-388`）。生产 scheduler 投递用 10 秒、64 KiB 且丢弃响应体（`rust/apps/agent24d/src/scheduler_deliver.rs:27-34`）。 | 这是 scheduler callback，不是工具结果通道。要新增独立工具调用结果解析，不能改变现有 fired 投递语义。 |
| 错误、超时、取消 | `ToolError` 目前是 `Invalid/Denied/Failed/Timeout/Cancelled/AbortRun`，缺少保留模块错误码的结构化结果（`rust/crates/agent24-tools/src/lib.rs:152-170`）。registry 以 timeout 和 `CancellationToken` 竞速（`:669-683`）；`Cancelled` 在 agent loop 可能取消整个 run（ADR-DOC-02 §1 对应 `agent24-agent/src/lib.rs:1347,1986`）。 | 模块调用错误码须结构化透传；模块级取消不得误映射成会取消整个 run 的错误。超时/断连时需表达“结果可能未知”。 |
| 幂等与重试 | scheduler 的 `kernel_call` 注释明确内部 kernel request 不具幂等性（`rust/crates/agent24-os-proto/src/kernel_call.rs:217-231`）；公开代理只对无 body 的 GET/HEAD/OPTIONS 自动重试（`rust/crates/agent24-os-proto/src/proxy.rs:1546-1556`）。 | 工具有副作用时不得由内核盲目自动重试；`run_id` / `tool_call_id` 仅用于可信关联与审计，不是业务幂等键，也不承诺传输层去重。 |
| 生命周期 | Generation、drain 和 revoke 已为现有进程外 callback 提供在途请求/停止接缝（`rust/crates/agent24-os-proto/src/drain.rs`、`kernel_call.rs:217-276`）。 | 工具可发现性、调用许可必须同步反映 disable、退避、熔断、停机及操作级 unavailable；当前没有模块工具生命周期过滤。 |

上表只陈述内核现状。manifest capability 交集、Generation 生命周期和内建工具审批不能被误读为模块工具已获授权或已实现。

## 2. 决策

### 2.1 工具声明与有效许可

模块 manifest 可以声明一个有限工具清单；每项至少有模块内操作名、描述、输入 schema、请求风险等级、`timeout_ms`、`inline_wait_ms` 和输出隐私需求。实现可在 K1-5 确认 wire 细节，但字段语义须满足本节。

内核以 `module_id + operation` 形成稳定工具身份（展示为 `<module_id>.<operation>`）。模块 ID 来自已验证 manifest；操作名经内核校验，不允许任意覆盖模块命名空间。schema 只用于输入校验/模型描述，不是授权语义。

**可调用权限是以下集合的交集**：manifest 声明、当前宿主策略允许、当前用户/组织/来源授权允许、模块及操作实时可用。任一项缺失、过期、撤销、未知或无法校验，均拒绝调用。声明、签名、第一方身份、安装状态、risk request、`output_privacy` 都不能单独放行。

由宿主定实际风险与资源动作。第三方或手动安装模块默认按高风险处理；未经确认的操作不得因自报 `Read` 或 `local_only` 降权。用户可收紧风险。第一方模块（包括 Documenting）按已拍板方向默认安装即启用，但首次使用前须确认一次清楚列出的权限范围；这不提高模块风险，也不代替运行时资源/隐私门。第三方及手动安装按高风险默认。升级若扩大工具清单、资源范围或其他权限，扩展部分须重新同意，未同意前不得调用。

云处理的产品承诺**可选，默认关闭**。敏感资料默认 `LocalOnly`；云授权须明确限定“资料 × 用途 × 目的地”，有期限且可撤销。组织或来源的更严限制优先，普通便利开关不得覆盖。获准云请求在该授权和 K1-02 执行门通过前不可发送。实现首发是否提供云档位由后续决定；未支持时报告 unavailable。

“存在任意远端模型档位就不通告文档工具”保留为过渡保护，只在 K1 隐私执行 K1-6 完成并验收后撤除；K1-6 拆为 K1-6a 启用权限确认/撤销（最先实现，依赖 ADR-K1-03）和 K1-6b 隐私标记传播/LocalOnly 出口门。此安排不授权在保护门完成前开放远端处理。

### 2.2 发现与生命周期

对模型通告工具前，宿主须在同一个有效状态视图中检查：已注册、已启用、权限有效、模块运行且未退避/熔断/停止、该操作能力可用、风险策略允许通告。动态状态变化时，后续通告应更新；已排队但尚未派发的调用仍须重新检查所有门。

停用、撤权、熔断、draining/stopping、模块崩溃、操作在 `/capabilities` 中不可用/未知，都必须使该操作不可通告和不可调用。若状态在通告后变化，调用时返回结构化 `module_unavailable`（可附受限原因），不能派发给模块。恢复可用需经过宿主生命周期状态更新，不接受模块自述“我已恢复”作为授权。

模块操作名只在模块命名空间内唯一。重复/冲突注册、无效 schema、未知风险枚举或不兼容必需字段导致该工具拒绝注册，不能覆盖既有工具或使其权限扩大。具体版本协商与滚动升级留给 K1-5。

### 2.3 可信上下文与资源范围

每次派发由宿主生成并绑定 `run_id`、`session_id`（可空）、唯一 `tool_call_id`、模块/工具身份、调用期限、当前授权版本/决策引用，以及本次已授权的资源范围和数据处理政策引用。模块只能把这些字段当作上下文信息；其回传或修改不改变宿主归属。

上下文通过宿主控制的内部通道注入（具体 header/body/wire 由 K1-5 定稿）。客户端不得访问内部工具路由或伪造 `X-A24-*` 字段。模型提供的工具参数只作为不可信输入；资源 ID 必须按本次宿主授权重新验证，不能由 `tool_call_id`、模块名或“同一 session”推导额外访问权。

数据政策和来源标记由内核维护并随结果/后续调用继续受 ADR-K1-02 约束。模块输出不得自行声明可信、清除来源 taint、放宽 LocalOnly，或为其网络出口授权。结果仅作为不可信工具输出进入 agent 上下文，不能升级为系统指令或授权依据。

### 2.4 有界调用、结果与错误

每次调用有内核强制的总期限、最大请求/响应字节数和并发/资源配额。模块请求值只能收紧限制，不能扩大宿主上限。响应超限时拒绝其结果并停止读取；不得将截断 JSON 当成成功结果。空结果、非法 JSON、schema 不符、未知 envelope 均是协议错误。

工具成功返回**类型化结果**：至少区分 `completed` 与经 schema 声明的 `pending`/job handle（只适用于该工具明确支持的异步语义），保留受限 JSON payload 及可选 `replayed` 标记。当前 `kernel_call` 仅返回 HTTP status，不能满足该契约。HTTP 2xx 不自动代表业务成功，必须解析合法成功 envelope；非 2xx 也须解析闭集/可版本化的结构化错误 envelope。

模块工具声明的 `timeout_ms` 是单次调用硬上限；宿主配置可更短，不可更长。`inline_wait_ms` 必须为非负且严格小于有效 `timeout_ms`，用于规定同步等待操作结果的最长时间。只有 manifest 明确声明异步结果语义的操作，超过 inline wait 才可返回类型化 `pending` 句柄供后续独立调用查询；不得把未确认的副作用结果伪装成 pending 成功。

稳定错误码至少覆盖：`invalid_input`、`permission_denied`、`module_unavailable`、`operation_unavailable`、`rate_limited`、`timeout`、`cancelled`、`response_too_large`、`invalid_result`、`module_error`、`result_unknown`。错误携带 code、是否可重试提示及受限 details；不得依靠自然语言 message 做分支。模块可提供名称空间内的细分 code，但未知 code 映射为通用 `module_error` 且保留原始受限 code。任何错误或 details 不得包含正文、凭据、完整提示、未授权资源内容。

工具级 `cancelled` 只表示该 tool call 被取消，不应被映射为会取消整个 run 的 `ToolError::Cancelled` 语义；只有宿主的 run-cancel 决策可取消 run。取消信号通过宿主控制的取消 token/控制通道或关闭对应连接传播，具体 wire 由 K1-5 选定；必须关联到精确 `tool_call_id`，不能取消同模块其他调用。

### 2.5 超时、取消与结果未知

run 被取消、用户撤权、模块停用/熔断或调用超时时，宿主停止等待并向对应 tool call 发出协作式取消；若模块不响应，宿主可关闭其请求/连接并按进程生命周期策略处置。取消通知不是事务回滚承诺；模块可能已产生副作用。

只要请求可能已派发而没有可信终态响应，结果必须表达 `result_unknown`，不得伪报失败后替用户自动再执行。用户或 agent 后续可以在重新授权并执行模块业务幂等规则后显式发起新调用；新调用有新 `tool_call_id`。

### 2.6 幂等边界与重试

内核**不自动重试**模块工具调用，不做传输层去重，也不把 `tool_call_id` / `run_id` 当幂等键。它们只用于可信调用关联和审计。业务幂等由模块按业务语义与业务键实现；重新授权仍须在每次请求上检查，包括业务键命中时。相同业务键配不同有效载荷的拒绝规则由模块业务契约定义。模块若返回重放结果，可用 `replayed: true` 显示该事实。

将来如需传输层去重，必须另开 ADR 定义作用域、同一调用 ID 不同载荷时的拒绝规则、持久期及其与业务键的映射；本 ADR 不授权此能力。重试提示仅供决策参考，不是内核重试指令。

### 2.7 审计与数据最小化

工具调用成功、失败和拒绝均应可用 `run_id + tool_call_id + module_id + operation` 关联审计；记录结果码、时间、授权决定引用及必要的尺寸/时长元数据。不记录正文、凭据或完整提示。审计留存 180 天，用户可查看、导出；删除边界与 ADR-K1-04/决策日志保持一致。模块自有日志不替代内核审计。

本 ADR 规定审计事件所需关联字段，不定义哈希链实现和查看/导出接口，也不因此宣称当前已有每次模块工具执行的哈希链记录。

## 3. 威胁模型

保护对象是用户资料、其他模块/会话/资源、宿主授权状态、agent run 完整性和本机可用性。模块可能恶意、被攻陷、写错 manifest、被替换/升级，或在调用中崩溃；模型参数、模块返回值、模块自报风险/来源/处理模式均不可信。进程外 OS 是执行边界，但本 ADR 不声称其能抵御同用户进程通过宿主操作系统权限直接读写用户文件。

主要威胁及控制：

- **越权工具或资源调用**：声明与宿主许可取交集；每次调用复验权限和资源范围；身份及上下文由宿主注入。
- **跨模块冒名/工具名冲突**：模块名由 manifest 校验，工具名按命名空间限定，注册冲突 fail closed。
- **停用/撤权竞态**：通告过滤之外，派发前再检查；在途调用取消，状态未知时不得重试。
- **资料外传与假 LocalOnly**：敏感来源默认 LocalOnly；模块声明不是出口授权；K1-02 执行门负责所有出站路径，更严组织/来源限制优先。
- **拒绝服务/资源耗尽**：请求/响应字节、总时限、并发数及速率受宿主上限约束；超限不可作为成功结果。
- **副作用重复**：不自动重试、不以追踪 ID 去重；业务幂等由模块契约负责并向用户显示结果未知。
- **提示词注入/伪造结果**：工具结果视为不可信证据；错误 details 限长且不泄露敏感内容。

本 ADR 不把模块签名、包来源或第一方身份视作“无副作用”证明，也不把 OS 进程隔离等同于文件系统或网络沙箱。

## 4. 兼容性、部署与迁移

1. 这是新增工具能力；旧 manifest 未声明 `tools` 时语义保持“没有模块 Agent 工具”，不能推断为允许全部工具。
2. 现有 `_a24/scheduler/fired`、models、memory、approval、events callback 保持各自协议和重试语义。工具结果 body 的支持必须采用独立调用语义/类型，不改变 scheduler `KernelResponse { status }`。
3. 旧版/不支持新协议的模块不能被通告新工具；协议能力未知、升级验证未完成或迁移状态不明时 fail closed，不以兼容模式放宽授权。
4. 工具注册/发现与调用必须可用后才对 agent 公布；过渡期远端模型存在时对 Documenting 不通告的限制，按 §2.1 仅在 K1-6 完成和验收后撤除。
5. Task PR 必须保持高风险门槛：CI、本地门禁、逐类 `pre-pr-check.sh` 回应及 PR-Daemon APPROVE；本 ADR 的 Proposed 状态不构成 K1-5/K1-6 实现授权。

## 5. 反例测试清单（后续实现必须先写并证明修复前失败）

以下是待实现的验收测试，不是本 ADR PR 已运行的代码测试：

1. manifest 声明工具但宿主未授权、已撤权、已过期或升级扩大权限未重新同意 → 不通告且调用被拒；未经确认的第三方自报 `Read`/`local_only` 不降风险。
2. 两模块注册同名操作、非法操作名/schema、未知 risk enum → 拒绝注册，不覆盖已注册工具或扩大权限。
3. 模型伪造 `run_id`、`session_id`、`tool_call_id`、授权资源或 `X-A24-*`，模块响应回传修改这些值 → 宿主归属与权限不变；其他用户/资源不可访问。
4. 模块停用、熔断、退避、draining、崩溃，或操作 capabilities 未知/不可用 → 过滤工具；通告后的竞态调用返回 `module_unavailable`，不再派发。
5. 模块返回超限 body、非法 JSON、未知 envelope、缺字段/错误类型，或在 2xx 后中断 body → 不得作为成功结果交给模型；有限上限在实际读取中生效。
6. 模块返回业务错误码 → agent 可按结构化 code 处理；未知 code 安全归一化；模块 tool cancellation 不误取消整个 run。
7. timeout、run cancel、撤权、stop 与模块执行竞态 → 取消只作用于精确 tool call；不声称事务回滚；若可能已派发且无终态，返回 `result_unknown`。
8. 执行会改变外部状态的工具时模拟连接断开/响应丢失 → 内核调用次数不自动增加；不得把 `tool_call_id` 当业务键；显式再次调用仍逐次复核权限。
9. 一个允许资源的授权尝试访问未授权资源、改用另一个 session 或复制 tool-call ID → 拒绝；同一模块另一个并行调用不受取消影响。
10. 敏感资料在 LocalOnly、拒绝、授权过期/撤销、更严格来源或组织策略下尝试远端工具 → 出口门阻断；远端模型已配置不得改变结果；获准云请求只能对应明确的资料×用途×目的地授权。
11. 并发达到限额、工具超过 timeout/inline_wait 或返回 pending 但未声明异步语义 → 有界失败；只有符合已声明异步契约的结果可返回 pending/job handle。
12. 审计成功、失败、拒绝均有关联元数据且不含正文、凭据或完整提示；保留策略 180 天，用户查看/导出结果可复核。

实现 PR 应为适用项先提交测试，并记录反面对照（当前实现/测试在修复前失败的证据）；ADR-only PR 不伪称这些行为测试已通过。

## 6. 法律映射

| 法律 | 本 ADR 的约束 | 现状与实现要求 |
|---|---|---|
| [APPROVAL.md L-APPR-1..3](../laws/APPROVAL.md) | manifest/安装源不能写宿主授权；风险调整不等同调用批准；宿主授权优先，工具声明不能越权。 | 现有风险 override 与门禁分离（`agent24-tools/src/lib.rs:400-447`）；模块工具专属声明/授权交集尚未实现。 |
| [APPROVAL.md L-APPR-4..7](../laws/APPROVAL.md) | 高风险不能取得宽泛授权；逐工具资源范围、期限和撤销要受统一门控；读权限不能因模块自报而长期放大。 | 当前已有工具授权门的一些规则，但未覆盖动态模块工具；按 ADR-K1-03 补权限确认/撤销机制。 |
| [MEMORY.md L-MEM-1..3](../laws/MEMORY.md) | 模块不得取得 raw store 或自报 owner/scope；宿主提供有限句柄与资源作用域。 | 现有 scoped memory 体现宿主注入原则；通用工具上下文与资源验证需复用这一边界，不暴露数据库/存储句柄。 |
| [CONTEXT.md L-CTX-2、L-CTX-4](../laws/CONTEXT.md) | 工具输出及其来源可点名并受大小限制；运行记录不能作为任务完成证据。 | 新工具结果需携带宿主来源关联并有字节上限；该约束不替代 K1-02 的持久隐私标记传播。 |
| `docs/laws/README.md` 的法律变更流程 | 若实现发现缺少长期隐私/出站不变量，应另提 `PRIVACY.md` 或获批合并位置，并单独闭环。 | 本 ADR 不直接修改 laws，也不将政策规则写成已经有机制。 |

## 7. 明确不纳入本 ADR 的内容

- Documenting 文档、revision、编辑、PDF、导出或业务操作定义；
- 以 `document_id`、`change_set_sha256` 或其他 Documenting 键作为内核通用幂等规则；
- 内核模块间任意调用、模块任意声明网络出口、或工具返回内容自动成为可信指令；
- 仅靠模块签名、风险自报、`output_privacy`、安装默认值或普通审批放行数据外传；
- 本 ADR 通过即宣称模块工具、LocalOnly 出口门、启用 UI、审计哈希链已经实现或验收。

## 8. 后果

通用模块工具将成为需显式宿主授权、可发现性过滤和有界执行的新能力；不会复用“工具名碰巧属于 module 类别”或静态 registry 作为权限证明。实现将涉及 manifest、os-proto、agent24d/domain、agent24-tools/agent loop 与授权/取消/审计接缝，须按 K1 计划拆片并逐项验收。旧调用与业务操作保持原权威来源，Documenting 只作为消费者。
