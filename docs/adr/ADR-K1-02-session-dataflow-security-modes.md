# ADR-K1-02：会话数据流与安全模式

> **状态**：**Accepted**（2026-10-08 jason 确认；PR-Daemon 已 APPROVE。实现按 PLAN-KERNEL-K1 §4 切片进行，每片仍须 PR-Daemon APPROVE）
>
> **日期**：2026-10-07 · **方向**：Kernel K1 · **任务**：#735 / K1-2
>
> **依据**：[#735](https://github.com/iDoris-ai/Agent24/issues/735)；[PLAN-KERNEL-K1 §2(2)、§4 K1-2](../agent/PLAN-KERNEL-K1.md)；C5 A-3、C2 ID-1/ID-3（[COMPONENT-ROADMAP](../agent/COMPONENT-ROADMAP.md)）；Jason 2026-10-07 对 PLAN-KERNEL-K1 §2 五组问题的裁决；[ADR-K1-01](ADR-K1-01-module-tool-contract.md)；[ADR-K1-03](ADR-K1-03-enable-authorization-trust-revocation.md)。
>
> 本文行号按 K1-2 起草时的 `ab/kernel` 工作树核对；它们描述**现状**，不是未来实现承诺。

## 0. 范围与边界

本 ADR 定义会话数据的来源、修订与处理模式如何由宿主建立并跨执行边界传播，以及 LocalOnly 和获准云处理的出口门。它覆盖主对话、恢复、委派、模型路由、HTTP/MCP/模块/Exec、记忆和后台任务。它定义政策契约，不实现代码、不承诺首发提供云模式，也不替代 [ADR-K1-03](ADR-K1-03-enable-authorization-trust-revocation.md) 的启用授权或计划中的 ADR-K1-04 审计字段。

安全模式只有两个有效结果：`LocalOnly` 与 `CloudAuthorized`。未知、缺失、过期、互相矛盾或无法恢复的状态一律按 `LocalOnly` 处理；不设“尽力而为”的中间态。文档和其他敏感来源默认 `LocalOnly`。获准云处理是可选能力、默认关闭；首发是否承诺此能力另行决定。

## 1. 现状与差距

| 主题 | 已有事实（文件:行） | 差距 |
|---|---|---|
| 路由底层 | `TaskProfile` 只有 `privacy × complexity`（[`router.rs`](../../rust/crates/agent24-models/src/router.rs#L74)）；`LocalOnly` 的路由不含远端 provider，无本地 provider 时返回 unavailable（[`router.rs`](../../rust/crates/agent24-models/src/router.rs#L405)）。 | 路由能执行已给定的隐私值，但没有来源、修订、会话政策或授权依据；调用方给错 profile 仍可能外发。 |
| 主对话 / ID-1/3 | agent loop 每轮用 `TaskProfile::default()`（[`lib.rs`](../../rust/crates/agent24-agent/src/lib.rs#L1663)）；REST `/api/v1/chat` 也传默认画像（[`routes.rs`](../../rust/apps/agent24d/src/routes.rs#L565)）。C2 ID-1 尚规划入口生成画像；ID-3 的 iDoris 控制头及 LocalOnly 贯穿依赖 ID-2，均非本 ADR 可宣称已完成。 | 主路径没有从用户输入和关联资料计算政策、将其附加到 run 并在每次模型调用强制收窄的统一接缝。ID-1 是任务/路由画像，不是授权源；不得覆盖更严格来源政策。 |
| 模块模型回调 | 回调从可信 manifest `ModelAccess` 构造 `TaskProfile`；缺省 `local_only`，`remote_allowed` 映射为 `Privacy::Any`（[`model_callback.rs`](../../rust/apps/agent24d/src/model_callback.rs#L489)、[`domain/lib.rs`](../../rust/crates/agent24-domain/src/lib.rs#L225)），LocalOnly 还有 served-tier 断言（[`model_callback.rs`](../../rust/apps/agent24d/src/model_callback.rs#L861)）。 | 这是模块模型调用的粗粒度 manifest 设置，不是逐资料×用途×目的地授权或运行来源标记。它不得为任何会话资料授予云处理。 |
| 会话与重启恢复 | run 消息逐轮持久化；恢复只允许从 `awaiting_approval` 且有未答工具调用的状态恢复（[`resume.rs`](../../rust/crates/agent24-agent/src/resume.rs#L1)、[`resume.rs`](../../rust/crates/agent24-agent/src/resume.rs#L73)）。 | 持久线程未携带可信隐私来源/策略快照；恢复时尚无逐项恢复/撤销/过期重验。策略状态不能依赖易失内存，也不能仅靠 thread 内容复原。 |
| 委派 / 子 agent | subagent 重用默认画像；它没有 `http_fetch`，但注释明确其模型调用仍与主 run 同享默认远端 profile（[`subagent.rs`](../../rust/crates/agent24-agent/src/subagent.rs#L195)）。 | 委派必须继承父 run 的来源集合与最严格模式，不能由新 run 或子 agent 重置。当前无此继承契约。 |
| HTTP / MCP / Exec | `http_fetch` 有 SSRF 与响应大小约束（[`net.rs`](../../rust/crates/agent24-tools/src/net.rs#L1)）；MCP 有独立服务/工具执行（[`server.rs`](../../rust/crates/agent24-mcp/src/server.rs#L1)）；`shell_exec` 运行 argv 子进程且只过滤环境变量（[`local.rs`](../../rust/crates/agent24-tools/src/local.rs#L282)、[`env_whitelist.rs`](../../rust/crates/agent24-tools/src/env_whitelist.rs#L1)）。 | 网络安全与 secret 环境过滤不等同数据出境门。HTTP、MCP、第三方模块、Exec、模型工具结果与间接调用均未统一收来源标记/目的地授权；Exec 无网络沙箱保证。 |
| 记忆 / 上下文 | session memory 将 user/assistant turn 写入日志（[`session_memory.rs`](../../rust/crates/agent24-agent/src/session_memory.rs#L307)）；召回被标注为“记忆数据·非指令”（[`session_memory.rs`](../../rust/crates/agent24-agent/src/session_memory.rs#L25)）。`CONTEXT.md` L-CTX-2 要求来源可点名及内容哈希。 | 记忆记录/召回有来源和指令权威边界，但没有安全模式 taint、授权快照或云处理状态沿写入、摘要、召回传播。 |
| 后台任务 | self-wake 建立延迟 run 并重新载入会话上下文（[`self_wake.rs`](../../rust/crates/agent24-agent/src/self_wake.rs#L115)）；模块 schedule fire 由后台 deliverer 向模块发送（[`scheduler_deliver.rs`](../../rust/apps/agent24d/src/scheduler_deliver.rs#L27)）。 | 定时任务/恢复 run 无持久策略快照与授权重验。创建后台任务不能延长许可有效期或把来源降级。 |
| 临时远端限制 | PLAN-KERNEL-K1 与 ADR-K1-01 记载 #733 D8 的“存在远端档位就不通告文档工具”过渡保护。当前代码搜索未发现 `has_remote()` 实现或对应通告门；远端档位本身在 [`router.rs`](../../rust/crates/agent24-models/src/router.rs#L15) 枚举。 | 本 ADR 不把计划文字当作代码已执行的保护。须定位真实产品门并验证；不满足撤除门槛前，不得以本 ADR 提案删除任何仍存在的限制。 |

上表对代码作现状描述，不主张组件现有的审批、模型路由、SSRF、记忆标注或任务恢复已形成端到端隐私保证。

## 2. 决策

### 2.1 来源标记和政策合并

每个进入 agent 上下文的内容块由可信宿主赋予不可由模型、模块或工具改写的标记：规范来源 ID、来源修订/内容摘要、资料/资源 ID（适用时）、信任级别、用途/任务类别、允许目的地集合、模式、政策版本，以及创建时间/有效期或撤销引用。原始正文不放进策略/审计元数据对象。

用户输入、选中的文件/资料、检索结果、记忆召回、模块/MCP/HTTP/Exec 输出都成为带标签的数据块。派生内容继承所有实际贡献输入的来源集合；模式取所有输入、会话、组织及来源政策中最严格者。无法追溯某段摘要/缓存/工具结果的输入来源时，将其视作敏感 `LocalOnly`。模型生成文本不会因“模型生成”而清除其所依据输入的 taint；工具/模块回传、摘要、引用和剪贴也不得自行降级标签。

会话由宿主汇总消息和被引用资料的标签，生成 run 的有效政策。资料在会话中途加入、来源策略收紧、授权撤销或失效时，后续执行立即重新计算；不把此前 `CloudAuthorized` 许可追溯扩大到新资料。策略只可被显式、有效且范围精确的授权收窄；来源/组织限制优先于用户许可，普通便利开关、模型 override、provider 配置、manifest、签名或工具自报属性不能放行。

### 2.2 跨轮次、恢复、委派与后台传播

- 每个 run 及其消息/派生数据引用持久的来源集合、政策版本和必要的授权引用；落库失败、版本未知或引用丢失时拒绝恢复外发，回退 `LocalOnly`。不必复制正文到策略记录。
- 每轮模型调用及每次工具调用重新从当前权威政策求交集；run 的缓存画像只能收紧当前政策，不能缓存成授权。用户续轮沿用累积来源集合；新输入只会维持或收紧模式。
- 重启恢复时重新读取并校验来源修订、组织/来源政策、用户许可期限与撤销状态。来源无法读取/校验或当前政策服务不可用时可在本地继续（若来源资料本地可得），否则 unavailable；不得转远端或静默丢弃敏感上下文后继续执行副作用。
- 委派、子 agent、工具调用、HTTP/MCP 代理、模块 callback 和 Exec 子进程继承父操作的来源集合、用途、目的地限制、有效期和最严模式。子任务可因删除输入而收窄，不能自行放宽；结果返回父 run 时并入派生来源集合。
- 记忆写入保留来源/模式引用和修订；摘要、压缩、索引、向量化等变换均继承来源。召回注入时重新检查当前政策；LocalOnly 记忆不得送入云模型。后台 schedule、self-wake、重试队列、缓存任务持有创建时政策引用，但每次实际执行仍重新验证撤销/到期/来源变更。无法安全重验则不执行。

### 2.3 LocalOnly 与获准云处理

`LocalOnly` 是出口约束，不只是 provider 偏好。模型路由必须使用宿主计算出的 `TaskProfile.privacy = LocalOnly`；没有本地模型、代理不支持或结果无法证明由允许的本地 tier 服务时，返回 unavailable，不回退远端。模型/provider 的名义 tier 必须可信配置，不能采信响应或调用方自报。

云处理仅可在功能明确启用后使用，默认关闭。显式用户授权绑定**资料 × 用途 × 目的地**，包含可校验的来源/修订或资料范围、用途、精确服务/接收方（不能仅是“云”）、有效期、签发者、当前策略版本与撤销状态。授权每次外发前校验；资料、用途、目的地、修订范围或政策任一不匹配即拒绝。组织/管理员限制、来源自身限制和更严格会话模式优先；授权拒绝、过期、撤销、未知或授权服务不可用均 fail closed。第一方工具首次权限确认与升级扩权重同意遵循 [ADR-K1-03](ADR-K1-03-enable-authorization-trust-revocation.md)；模块工具契约遵循 [ADR-K1-01](ADR-K1-01-module-tool-contract.md)。

### 2.4 LocalOnly fail-closed 出口门清单

出口门覆盖所有可能把正文、摘录、提示、派生结果或可识别资料发出本地信任边界的路径；不以“不是 LLM”或“工具自己负责”豁免：

1. **模型**：所有主对话、REST chat、模块模型 callback、路由器 fallback、重试/流式与 embedding/摘要/分类请求；LocalOnly 只能到本地 tier。C5 A-3a 的远端 LLM 负载必须经 iDoris 出口网关脱敏，但脱敏不能代替 LocalOnly 或授权门。
2. **网络与连接器**：`http_fetch`、HTTP/REST 代理、MCP client/server 工具、邮件/IM/外部 API、模块直接出网、返回 URL 的间接 fetch；C5 A-3b 要求各消费方进程有自己的启动断言/egress guard。目标解析与 SSRF 防护不是资料政策许可。
3. **模块与进程执行**：通用模块工具 callback、第三方插件以及 `shell_exec`/Exec 子进程的 stdin、argv、文件共享、环境、socket 和其后续网络流量。宿主当前无法约束子进程目的地时，对带敏感资料的该动作拒绝；不得把本地启动命令当作“仅本地处理”的证明。
4. **存储外流**：记忆/日志/trace、向量与缓存、后台任务队列、诊断/遥测、崩溃报告、导出或同步服务。任何可能传到远端的派生/持久数据均须继承原标签并过同一授权门；本机持久化是否允许另受存储策略约束。
5. **生命周期和控制面**：续轮、重启恢复、委派、定时/自唤醒、后台重试及模型/目的地 override。每个边界即时复验来源、模式、授权代次和期限；任何一个出口路径未接门、政策未知、门故障或无法观察实际目的地时拒绝并给出 unavailable/permission denied。

工具审批、模块能力、SSRF、环境变量白名单与出站隐私授权是独立门，必须全部通过；其中任何一门允许不意味着其余门允许。

### 2.5 `has_remote()` 临时限制的撤除条件

“存在远端档位即不通告文档工具”只是临时的保守兼容门，不是安全模式定义，也不应永久替代逐 run 政策。该限制**仅在 K1-6a 和 K1-6b 均完成并验收后**撤除：6a 启用时权限确认、撤销已即时有效；6b 来源/修订/模式沿所有 §2.4 路径传播、所有 LocalOnly 出口门 fail closed；并完成计划中的反例测试和 #708 macOS 真实产品路径验收。单个 ADR 获批、代码合入、只完成 6a、仅模型 Router 单测或远端档位未配置，均不满足门槛。撤除前须有可复核证据证明被保护的具体通告路径已由逐 run 执行门覆盖；若当前代码中没有该旧门，应记录为计划/代码差异，不能伪称已删除或依赖它保护。

## 3. 对齐与法律边界

- **C5 A-3**：A-3a 规定远端 LLM 走 iDoris 出口网关脱敏并让 LocalOnly 贯穿主路径；A-3b 要求每个非 LLM 出站消费者进程自带启动断言。脱敏/启动断言是路径控制，不授予某份资料的云处理许可。本 ADR 将资料政策门叠加在它们之上。C5 规划中 A-3 法律冻结与实现分批，不能声称 Agent24 A-3b 当前已完成。
- **C2 ID-1 / ID-3**：ID-1 产出的 TaskProfile/意图只用于任务分类和路由偏好；宿主必须把 K1 来源策略与之合并，模式只能取更严者。ID-3 的 iDoris `X-iDoris-*` 控制头必须由内核从可信政策构造，不从 prompt/模块参数读取；`LocalOnly` 不得由 ID-1 路由或 provider fallback 覆盖。计划文档提示跨仓字段尚需核对，真实 wire 字段由其路线另行确认。
- **[CONTEXT.md L-CTX-1..4](../laws/CONTEXT.md)**：上下文来源受授权根约束、每份来源可点名并记录 canonical path/作用范围/哈希与大小上限、兼容输入不可改写、运行记录不是完成证据。上述要求适用于 K1 标记中的来源引用；本 ADR 不声称当前所有会话/资料入口已满足。
- **[MEMORY.md L-MEM-1..7](../laws/MEMORY.md)**：模块不获原始 store、不能自报 scope/owner，身份与范围由内核注入，不夸大隔离。模式标记同样由内核注入且不可由模块改写；当前 memory partition 不等于会话隐私隔离。
- PLAN-KERNEL-K1 要求长期隐私不变量另走 `docs/laws/PRIVACY.md`（或经批准的合并位置）独立法律 PR。该文件在本次核对中不存在；本 ADR 不代替 C5 A-3 法律冻结，也不修改法律。

## 4. 威胁模型与否决

威胁包括提示词/资料注入、恶意或被攻陷模块/MCP 工具、错误来源修订、恢复竞态、授权撤销竞态、provider/tier 配置错误，以及本地子进程自行联网。模型、模块、工具参数、响应头和会话文本均不可信。OS 进程分离不被视作网络沙箱。

以下任一情形必须拒绝远端处理/副作用，或返回明确 unavailable：来源未知；授权范围不全；模式/策略冲突；政策或撤销状态不能读取；目的地无法精确识别；来源修订变化未复核；恢复/委派未带策略；出口门未接线；执行后无法确认数据仍在允许边界内。用户同意、普通确认框、模块签名或本地模型缺席都不能放宽这些否决条件。

## 5. 后续实现反例与验收

本 PR 只写 ADR；下列是 K1-6a/6b 的待实现反例，不是本 PR 已运行的测试。实现应先在旧行为上证明反例成立，再完成修复并复测。

1. 敏感文档与远端 provider 同时存在 → 对话/子 agent/摘要仅使用本地 tier；无本地模型时 unavailable，远端桩零请求。
2. 把资料拆成摘要、memory recall、tool result、缓存或 embedding 后送出 → 派生项仍继承原来源/修订/LocalOnly，拒绝出口。
3. 用户仅授权资料 A × 用途 U × 目的地 D → 换资料、用途、修订或目的地即拒绝；组织/来源更严政策及普通便利开关覆盖关系正确。
4. run 挂起后重启，或续轮/恢复时授权已撤销、过期、来源变更/不可读 → 重新检查并阻止远端及副作用；不会从 thread 文本或默认 TaskProfile 重建放宽许可。
5. 子 agent、模块、MCP、HTTP、Exec、邮件/IM、定时任务/自唤醒继承 LocalOnly → 任一直接或间接出网均被门拦截；未受宿主控制的子进程网络动作不可执行。
6. policy service、出口 guard、provider tier/目的地识别不可用或未知 → fail closed；不得 fallback 到远端或把失败当作无标签。
7. 有效云授权到期/撤回、政策收紧或 run 中加入敏感资料 → 下一次外发前即拒绝；正在执行的请求在可取消边界停止，已送达内容不声称可收回。
8. K1-6a 与 K1-6b 均通过反例和 #708 产品验收之前，远端档位存在时的文档工具通告限制仍保留（如代码中存在该门）；不得以单测或 ADR 获批提前撤除。

## 6. K1-6b 实现切片建议（每片 ≤300 行）

K1-6a（启用时权限确认/撤销）先于本路线，依赖 ADR-K1-03。以下切片都要独立保持 fail closed，按 `ab/kernel` 为 base，先写反例并证明旧行为失败；不把 K1-6a 授权 UI 混入 6b。

1. **6b.1 来源标记与持久 run 元数据**：定义内核私有 `SourceRef/PolicySnapshot`（来源 ID、修订摘要、模式、策略版本/授权引用）；入口标记用户输入和已选择资料，落盘与消息/派生对象关联。未知/旧 schema 读取为 LocalOnly；测试序列化、升级、恢复及来源缺失。
2. **6b.2 模型画像合并与恢复/委派传播**：将可信来源政策与 ID-1 TaskProfile 合并，在 agent loop、REST、resume、subagent 的每次调用统一构造 profile；更严模式优先。先覆盖默认画像泄漏反例和 provider fallback。
3. **6b.3 统一出站判定 API 与模型/HTTP/MCP/模块接线**：引入宿主持有的策略判定（资源、用途、目的地、来源、授权代次）；先接模型 router、`http_fetch`、MCP 和 ADR-K1-01 模块工具。任一未接入消费者不通告/不可用；出站请求前复验并覆盖撤权竞态。
4. **6b.4 Exec、记忆和后台任务**：把受限策略引用传到 shell/模块子进程、memory write/recall、self-wake、scheduler delivery、缓存/重试任务；没有可证明的网络隔离时阻断敏感数据路径。先测恢复、派生内容 taint、撤销/过期与后台执行。
5. **6b.5 逐出口矩阵与 #708 证据**：以清单驱动每个消费方的 LocalOnly 负例、获准云精确三元组正/负例、门故障负例及 macOS 产品路径验收；只有覆盖矩阵全绿才移除实际存在的 `has_remote()` 过渡门。法律更新另开 PR。

每片 ≤300 行，若切片需要引入共享类型，先落一个仅供下游消费且默认 LocalOnly 的窄契约；不能形成任一可外传的中间版本。所有实现 PR 均为高风险，须 PR-Daemon APPROVE 后合入。
