# Knowledge profile / Off / 撤权状态机契约 v1（CONTRACT-STATE-v1）

> **状态**：规范核心 v1（草案）· **日期**：2026-10-20 · **Issue**：#1013 ①（KB-CT-03），阶段 #1000
> **依据**：`docs/knowledge/CONTRACT-READONLY-v1.md`（下称 READONLY，按 r8），规则写作 `R7.3` 等。本文由完整版 `ab/weknora-03a-state-machine`（#1072 的分支）压缩而来，只写对外行为与不变式，实现机制由实现任务以测试证明。条款沿用完整版编号，空号（如 S1.9、S1.11）即压缩时已删除，不再使用。

## 0. 范围与边界

- **0.1 冻结什么**：Agent24 宿主侧 KnowledgeProvider（下称 Provider）里一个 Knowledge binding 的身份、状态、事件的可观察结果、generation 的值与递增时机，以及事件对在途请求、迟到响应、缓存、后续注入与本地结果的影响。不定类型、存储位置、锁、键名、日志格式、UI 文案、探测间隔与退避数值。服务端行为只引用 READONLY，本文不给服务端新增义务。
- **0.2 不管什么**：

| 内容 | 交给 |
|---|---|
| 请求带 generation、迟到响应丢弃、错误信封、就绪、兼容判定、Provider 层合并 | READONLY（§8 列出接口） |
| 交换票据、UI 会话、消息桥、长期登录态的保存与销毁 | #1013 ②（UI-BRIDGE，§9） |
| 写操作、ingest、job、ContextPack、SOP | #1013 ③（OPS，§9） |
| 系统凭据存储、Provider 实例化、Tool 注册、run 绑定的落地、持久化与崩溃恢复的实现 | KB-HOST-01、KB-C1-04、KB-C1-01 |
| local-managed 的握手与实例标识、进程启停与回收、注销隔离与历史可见性 | KB-LITE-01a、KB-LITE-02、KB-LITE-04 |
| Tool 证据格式、广告文案、自动背景注入 | KB-C1-05、KB-CTX-02 |

- **0.3 变更规则**：沿用 READONLY §0.3。binding 身份、状态集合、事件集合、generation 的递增事件、不变式 I1–I8 属语义，改动升主版本；新增本地 `reason` 记为 v1 修订。

## 术语

| 术语 | 定义 |
|---|---|
| binding | Provider 连接一个服务端身份的单位（身份见 S2.4），有永不复用的 `binding_id`。 |
| 未绑定占位 | profile 没有任何非 `retired` binding 时的 profile 级占位（S2.5）。 |
| 激活 | Knowledge 为 On、profile 已选用且未停用、principal 已绑定、profile 不带待确认（S1.13）。每个 profile 至多一个激活 binding，不同 profile 可同时激活（S4.9）。 |
| 连接阶段 / 可服务 | 连接阶段指 `loading`、`ready`、`degraded`、`unavailable`；可服务指 `ready`、`degraded`。 |
| 准入开放 | binding 可服务、`persist_pending` 为假、没有未解除的待复验（S3.1）、不在作废流程中（S4.5）。 |
| 当前凭据 | binding 最近一次采用、尚未丢弃的短期凭据；每个请求记下它所用的凭据（S3.1 凭据归属）。 |
| 收紧 / 放开 | 收紧指减少可访问性或可交付证据的变化；放开指增加它们的变化（清单见 S1.10）。 |
| 后续注入 | 事件之后进入模型上下文的 Knowledge 内容：新 Tool 结果、自动注入、历史证据在后续模型调用中的重发。 |

## 1. 状态

- **S1.1** 服务状态闭集：`registered` / `loading` / `ready` / `degraded` / `unavailable` / `disabled`。另有终态 `retired`（S1.6）与阻断条件 `persist_pending`（S1.7），都不属于这六个状态。
- **S1.2** 派生按顺序取第一个成立的：Off 标志或 profile 停用标志为真 → `disabled`；未激活（含只有占位、带待确认）→ `registered`；否则取连接阶段。两个标志相互独立，可以同时为真。
- **S1.3** 连接阶段只看能力判定与不带 scope 的就绪响应（R8.2、R9.4）：兼容、`live: true`、search 与 read 都 `ready` → `ready`；恰好一个 `ready` → `degraded`，只准入为 `ready` 的那个操作；不兼容、`live: false`、两者都不 `ready`、不可达或超时 → `unavailable`。`operations.ingest` 不参与；未知就绪 `state` 按 `not_ready`（R6.2）；按 KB 的就绪（R8.4）不改变服务状态。单个请求得到 `unavailable` 或 `timeout` 只触发一次就绪探测，由探测结果决定是否转移。
- **S1.4** `unavailable` 必带 `reason`，取值与恢复方式：
  - `dependency`（不可达、超时、`live: false`、sidecar 不可用、签发方不可达）、`not_ready`（存活但 search、read 都不 `ready`）：按退避自动重试。
  - `incompatible`、`instance_unidentified`、`credential_rejected`、`access_revoked`：不自动重试，跨重启保持；只在手动重试、profile 配置变更、local-managed 的 sidecar 重启或重新握手后再试；`credential_rejected`、`access_revoked` 另可经重新登录再试（同一 principal 回到 `loading`，不同 principal 按 S3.5）。
  - `local_storage`：要进入 `loading` 时 `persist_pending` 为真；`persist_pending` 清除后回到 `loading`。其他 `reason` 在 `persist_pending` 期间与清除后都不变。
- **S1.5** 只有可服务状态发出 search 与 read；`loading`、`unavailable` 只发能力发现与就绪探测（不自动重试的 `reason`、`local_storage` 与 `persist_pending` 为真时什么都不发）；`registered`、`disabled`、`retired` 与未绑定占位不发任何 READONLY 请求。Provider 在**每次 dispatch** 时检查状态并按 §6 本地拒绝，与 Tool 是否注册、是否广告无关。Off 时 Knowledge Tool 仍在广告里，描述按当前状态每轮生成，标明不可用及原因，不主动提示用户打开。
- **S1.6** **`retired`** 是 binding 的终点：身份元组任一项改变、移除 profile、重启时判定身份不符、全局收紧或持久状态损坏（S1.14）都使 binding 进入 `retired`。`retired` 不可再激活、不准入任何请求，只能新建 binding；它的 generation 值由同一 profile 下新 binding 的初值越过（S4.4）。
- **S1.7** **`persist_pending`**：binding 有尚未确认持久化的 generation 变化或放开时为真。它不参与派生，但为真时该 binding 不准入任何请求，本地结果按 §6；确认后自动清除。收紧的持久化失败、单个 run 的绑定持久化失败都不置它。
- **S1.8** 持久状态无法读取、损坏或不完整到无法判定时 fail closed，按 S1.14 的「持久状态损坏」处理；profile 上次已确认的 principal 不可读时按「不同」处理。
- **S1.10** **崩溃安全不变式**。收紧类：Off、停用、取消选用、注销、移除 profile、各类 `retired`、占位规则与证据纪元（S5.6）、删除墓碑、待复验登记、标记待确认、进入不自动重试的 `reason`（含占位记下的失败 `reason`）、复验超时计数。放开类：On、启用、选用、用户确认、宿主级确认、新建 binding（含初值）、首次可服务、解除待复验、撤回删除墓碑、run 绑定与补记、离开不自动重试的 `reason`、清除占位失败 `reason`、复验超时计数清零。`dependency` 退避与各 10 分钟窗口的计数尽力持久化，丢失不算收紧丢失。持久化结果未知（失败、超时、无法确认）一律按未确认处理；同一变化的重试不得产生第二次效果。
  - **I1 没有持久记录就不放开**：放开在持久化确认前不生效；已落盘、确认未返回时崩溃，重启后可按已确认生效；放弃的放开不得在重启后出现。放开挂起时用户做相反动作，视为放弃，并持久化一条抵消它的收紧；被放弃的放开之后到达的确认忽略。**阻止范围**：宿主级收紧未确认时拒绝一切放开；binding 级收紧（含注销、退役）未确认时拒绝同一 profile 下的放开（含新建 binding）；宿主级放开在任一 binding 有未确认收紧时被拒；涉及多个 binding 的放开，任一有未确认收紧即被拒；依赖尚未生效之放开的放开同样被拒。被拒的放开直接丢弃、不排队、不自动重放，发起方得到的本地结果按 §6 的顺序判定（Off 为真时 `knowledge_off`，否则 `local_storage`）；之后按当时状态重新校验。
  - **I2 收紧立即生效、不回退**：收紧在内存立即生效（即 S4.5 的作废生效），持久化失败不阻断、不置 `persist_pending`，界面提示「部分设置尚未保存」。之后任何崩溃，重启要么得到这项收紧，要么进入全局收紧。存储在收紧后完全不可写再崩溃是残余风险，不在保证内。
  - **I3 不确定一律收紧**：重启时不能确定所有已生效的收紧都已持久化，或持久状态损坏，按 S1.14 收紧，不得按较放开的状态恢复。
  - **I4 generation 不撞号**：任何崩溃序列后，非 `retired` binding 的 generation 大于崩溃前用过和回报过的任何值（含未生效的待写值）；新 binding 的初值大于同一 profile 下用过或回报过的任何值。
  - **I5 作废后的证据不重发**：被占位规则覆盖的、属于 `retired` binding 的、`binding_id` 在重启后不存在的、被待复验或删除墓碑暂扣的证据，任何崩溃序列后都不带正文发给模型。
  - **I6 退役不复活**：`retired` binding 任何崩溃序列后都不再激活，它的长期登录态不用于任何 binding；不能确定退役是否已持久化时，所有 binding 按可能已退役处理。
  - **I7 Off、停用、取消选用、注销不恢复成放开**：重启后要么保持，要么全局收紧。
  - **I8 待确认不丢失**：待确认（S1.13）在用户确认持久化之前，任何崩溃序列后仍待确认，不自动取凭据。
- **S1.12** **曾可服务**：binding 第一次进入可服务状态前，先持久化「首次可服务」（放开类），未确认时进入 `unavailable`（`local_storage`）；之后不撤销，无法确定时按已可服务处理。
- **S1.13** **待确认**挂在 profile 上，两种原因：
  - `principal_changed`：principal 变化（S3.5）或占位上登录得到的 principal 与上次已确认的不同时置位（收紧类）。它与新 binding 是否建成、何时建成无关：新 binding 因 I1 被丢弃、之后重试建立的 binding 照样待确认，直到用户确认。确认时若长期登录态的 principal 与 binding 不同，必须重新登录并得到 binding 的 principal 才能确认；得到别的 principal 按 S3.5 处理，这次确认丢弃。确认（放开类）持久化后记下该 principal 为「已确认」；不需要确认而建立的 binding，其 principal 同样记为已确认。
  - `unsaved_settings`：由全局收紧或持久状态损坏设置，取代 `principal_changed`。带它的 profile 下 binding 都是 `retired`；On、启用、选用只改标志；登录或握手不自动建立 binding，只能在宿主级确认已持久化后由用户确认该 profile 并重新登录或握手：解除标志与建立新 binding 同一次原子持久化，principal 与上次已确认的不同时新 binding 的 profile 改带 `principal_changed`。
- **S1.14** **重启后的可观察语义**（先按身份元组与当前配置、长期登录态的 principal 判定 `retired`；带 `principal_changed` 的 profile 改在确认时校验）：
  - **正常恢复**（持久状态完好，且能确定已生效的收紧都已持久化）：按持久状态恢复 Off、停用、选用、注销、`retired`、待确认、已确认 principal、曾可服务、待复验与暂扣、删除墓碑、run 绑定、占位规则与纪元、可见集合、不自动重试的 `reason`、占位失败 `reason`、各窗口与超时计数；非 `retired` binding 的 generation 递增并持久化确认后才准入；不新增占位、不提示。未解除的待复验在取得凭据后重做复验；墓碑未决的 KB 先暂扣再重查。
  - **全局收紧**（持久状态完好但不能确定收紧都已持久化）：Off 并持久化；每个非 `retired` binding 记一条适用全部 KB 的占位规则并退役；每个 profile 标 `unsaved_settings`；所有长期登录态作废（不用于任何 binding，销毁方式归 UI-BRIDGE）；提示「上次有设置未保存，其中可能包含已移除的项」。用户确认提示（放开类）后才能 On。宿主级确认持久化前再次启动：全局收紧的写入都已确认且之后无未确认收紧时，不重复收紧、只再提示一次；否则再次全局收紧。
  - **持久状态损坏**：在全局收紧之上，所有 profile 回到未绑定占位并按未选用、已停用处理，全部历史证据换成占位；之后新 binding 的初值大于损坏前可能用过的任何值。确认持久化后不因同一损坏再次进入此情形。
  - **首次安装**：没有持久状态且宿主能确认是首次安装时默认 Off，不提示、不收紧。会话历史有 Knowledge 证据或存在 Knowledge profile 配置时不算首次安装；无法确认时按持久状态损坏处理。

## 2. profile 与 binding 身份

- **S2.1** 两种 profile 共用本文的状态与事件。local-managed：principal 是本机唯一用户（R1.4），激活时自动绑定；服务端身份是受控握手得到的实例标识；`loading` 另需 sidecar 可用并完成握手；没有注销事件；撤权来源是本实例撤销凭据或轮换本实例秘密。remote：principal 由签发权威认定（R1.3），登录后绑定；服务端身份是端点与租户（或 profile 配置修订号）；`loading` 另需向签发服务取凭据。
- **S2.2** sidecar 状态是输入不是服务状态：进入 `starting` 时 `unavailable` 的 binding 与记有 `dependency` 的占位重试；`ready`、`healthy` 不单独改变服务状态（health ≠ ready，R8.1）；`degraded`、`failed`、`stopping`、`stopped` 使连接阶段的 binding 进入 `unavailable`（`dependency`），不递增。
- **S2.3** Off 不停止 sidecar。停用或移除 local-managed profile 时，先完成作废（S4.5），再按 KB-LITE-04② 的所有权语义停止 sidecar。
- **S2.4** **binding 身份**：remote 为 profile × principal × 端点 × 租户（或配置修订号）；local-managed 为 profile × principal × 实例标识。任一项变化即新 binding，旧 binding `retired`。实例标识只取自 KB-LITE-01a 的受控握手，不取 HTTP 自报值；同一实例重启（标识不变）不算换实例、generation 不变。握手拿不到标识时不新建 binding、不递增，记 `instance_unidentified`。
- **S2.5** **未绑定占位**：状态按 S1.2 派生为 `registered` 或 `disabled`，没有 generation，不准入任何请求。登录（remote）或握手（local-managed）在占位上进行，不是 READONLY 请求；成功后新 binding 连同初值持久化确认，才进入 `loading` 并发 READONLY 请求。失败时占位记下 `reason`（`dependency`、`credential_rejected`、`instance_unidentified`）：`dependency` 在占位被选用、Off 与停用都为假、profile 不带 `unsaved_settings` 时按退避重试；另两者只在手动重试、重新握手或重新登录后再试。登录或握手的结果只在属于当前尝试时生效。
- **S2.6** 同一身份元组在旧 binding `retired` 后再次出现（端点 X → Y → X）时新建 binding、取新的 `binding_id`，不复活旧 binding。
- **S2.7** `retired` binding 的长期登录态不得用于任何其他 binding；是否、如何销毁由 UI-BRIDGE 定义。

## 3. 事件与可观察结果

- **S3.1** **凭据类响应与撤权**（只看 READONLY 信封；按 `code` 与 `reason` 分支，R6.2）：
  - **撤权信号**：`unauthorized/credential_revoked`；签发权威通知撤销或拒绝续签；local-managed 轮换本实例秘密。**范围变窄**：同一 principal 续签后凭据范围移除了任一 KB 或操作（同时有新增也算）；或凭据未变而服务端收窄（S5.10）。二者合称撤权类事件。
  - **撤权类事件的结果**：在连接阶段，作废在途请求并递增 generation（S4.5），按 S5.6 占位。`ready`、`degraded` 回到 `loading` 重签，重签被拒进入 `unavailable`（`access_revoked`）；`loading`、`unavailable` 中的撤权信号进入 `unavailable`（`access_revoked`）并丢弃内存凭据；范围变窄进入新一轮 `loading`，但处于不自动重试的 `reason` 时只作废与占位，`reason` 不变。在 `registered`、`disabled` 中不递增，仍按 S5.6 占位。由已接受响应派生的撤权事件不因前置状态改变而丢弃（S4.6）。
  - **例外**：binding 从未可服务（S1.12），且自建立以来未发生撤权类事件时，`loading` 中签发被拒，或本轮新签发的凭据被以撤权信号拒绝，进入 `unavailable`（`credential_rejected`），不递增、不占位。
  - **窗口上限**：同一 binding 10 分钟内第 3 次撤权类事件仍作废、递增、占位，但一律进入 `unavailable`（`access_revoked`），不再自动重签。
  - **`credential_expired`**：先续签。续签成功且同一 principal、范围相同或更宽，状态不变（`unavailable` 时回到 `loading`），不递增；principal 不同按 S3.5；范围变窄按撤权类事件；续签被拒按撤权信号（满足例外时按例外）；签发方不可达进入 `unavailable`（`dependency`），不递增。
  - **凭据无效类**：`unauthorized` 且 `reason` 为 `credential_invalid`、`credential_missing` 或未知值。连接阶段（`unavailable` 只限 `dependency`、`not_ready`，其他 `reason` 不变）第一次出现时不算撤权：不递增、不作废在途、不占位；立即停止准入，丢弃当前凭据，进入新一轮 `loading` 续签一次。**续签也失败**（续签被拒，或这一轮新凭据仍得凭据无效类）只在这一轮内判定：进入 `unavailable`（`credential_rejected`），按 S5.6 记一条适用全部 KB 的占位规则，不递增，不声称已撤权。签发方明示撤销按撤权信号；不可达进入 `unavailable`（`dependency`），不占位。同一 binding 10 分钟内第 3 次凭据无效类事件直接按续签也失败处理。
  - **凭据归属**：签发与续签的结果属于发起它的那一轮 `loading`（或占位上的那次尝试），轮次已变时到达的凭据不采用、不触发转移。只有用当前凭据发出的请求，其凭据类响应才按上面处理；用已丢弃凭据发出的请求，其 `credential_expired`、凭据无效类与范围信息不触发转移、不占位、不判收窄，结果换成本地 `not_ready`。
  - **旧凭据的撤权信号**：用已丢弃凭据发出的请求带回撤权信号时，Provider 登记待复验（收紧类，范围 Q 为被质疑请求的范围；该请求本身不带 KB 范围时（如签发请求），Q 取它所用凭据的授权范围，Q 不得为空；多条合并取并集）：立即不准入新调用（本地 `not_ready`），历史中该 binding、Q 内 KB 的证据暂扣（含登记后追加的）。随后用当前凭据对 Q 做带 scope 的就绪探测：得撤权信号按撤权类事件处理，暂扣转为占位；通过且覆盖了最新合并的 Q，解除待复验（放开类），恢复准入与暂扣，旧请求的结果换成本地 `not_ready`；得凭据无效类按上条处理，待复验保留。复验未完成（被作废、崩溃、不可达、超时、无凭据）时待复验保留，之后每次取得当前凭据且处于连接阶段时重做；连续 3 次超时后停止自动复验，等手动重试或重新登录。binding `retired` 时仍未解除的，暂扣转为占位。
  - `forbidden`（任何 `reason`）、`not_found`、`stale`、partial 只是请求级结果，不改变状态；它们带出的服务端收窄另按 S5.10。
- **S3.2** 同一 binding 的事件串行处理：前一个作废事件生效并完成中止与清缓存后才处理下一个；持久化有时限，超时按结果未知处理，不让后续事件无限等待。
- **S3.3** **用户与配置事件**（「作废」指 S4.5，均只在连接阶段递增，`registered`、`disabled` 下只改标志）：
  - **Off / On**：宿主级，作用于全部 binding；Off 作废并进入 `disabled`。On 为放开，持久化确认后按 S1.2 派生（profile 仍停用则 `disabled`）。
  - **停用 / 启用 profile**：只作用于该 profile；停用作废、丢弃凭据、进入 `disabled`；启用为放开，按 S1.2 派生。两个标志都为真时本地结果取 `knowledge_off`。
  - **取消选用 / 选用**：取消选用作废并回到 `registered`；选用另一个 profile 不影响已激活的 binding；重新选用时原 binding 未 `retired` 则重新激活同一 binding。`disabled` 中的选用与登录只记下，标志清除后生效。
  - **注销**（仅 remote）：作废并回到 `registered`，销毁短期凭据；长期登录态的清除归 UI-BRIDGE。同一 principal 重新登录重新激活同一 binding，不递增。
  - **移除 profile**：先按停用作废，再 `retired`；询问用户是否删除该 profile 的本地数据目录（S5.4）。
  - **取消一个 run**：只中止该 run 的在途请求，立即移出在途集合，不递增。
  - **M1 记忆开关**：不影响本文任何状态与 generation（§7）。
- **S3.4** **探测结果**：S1.3 的判定在 `loading`、`ready`、`degraded`、`unavailable` 之间转移，不递增；服务不可用与降级不中止在途请求，已发请求按各自 deadline 结束（R3.3、R4.5），结果照常交 Tool 层。同一轮内只应用比已应用者更晚发出的探测结果；属于已结束轮次的结果丢弃。
- **S3.5** **身份变化**（旧 binding 在连接阶段时作废并递增，否则不递增；均按 S5.6 占位）：
  - **换实例**（local-managed 握手得到不同实例标识）：旧 binding `retired`，新建 binding 并按 S1.2 派生。
  - **principal 变化**（续签或登录得到的 principal 与 binding 不同）：旧 binding `retired`，profile 置 `principal_changed`（S1.13），新 binding 待确认、派生为 `registered`（Off 或停用时 `disabled`），不复用旧登录态。
  - **改端点或租户**（remote）：旧 binding `retired`，profile 回到未绑定占位，重新登录后按 S2.5 建立新 binding。
  - 新 binding 在旧 binding 的退役确认持久化之后（或与之同一次原子持久化）才建立；退役未确认时新 binding 丢弃，占位记 `dependency` 按 S2.5 重试，`principal_changed` 照常保留（S1.13）。

## 4. generation

- **S4.1** 每个 binding 一个 generation：正整数，只增不减、不复用；唯一来源是 Provider，Electron 主进程、桥与页面只读取。
- **S4.2** **+1 的事件且只有这些**（都只在 binding 处于连接阶段时）：Off、停用 profile、撤权类事件（S3.1）、取消选用、注销、换实例、principal 变化、改端点或租户、移除 profile；以及 Provider 启动时每个非 `retired` binding 各 +1。
- **S4.3** **不递增**：探测结果与降级、不可用、恢复；S3.1 的例外；凭据无效类与它的续签（含续签也失败）；拿不到实例标识；占位重试；同一实例重启；续签范围相同或更宽；请求级结果；M1 开关；取消单个 run；On、启用、选用、用户确认、同一 principal 重新登录；`registered`、`disabled` 中发生的任何事件（Provider 启动除外）。
- **S4.4** generation 按 binding 持久保存。每次递增先持久化、确认后才生效；未确认时置 `persist_pending`，期间再有递增事件时待写值在已有待写值上 +1，确认后一次生效。新 binding 首个初值为 1，之后大于同一 profile 下用过或回报过的任何值（含 `retired` binding 与待写值）。
- **S4.5** **作废**（R7.4）：生效时同时关闭该 binding 的准入并把全部在途请求标为作废，此后不会有请求带旧值发出；随后中止在途请求（R7.1）、清缓存（S5.11）、丢弃之后到达的响应。生效后持久化或清理出错都不得重新开放准入。被作废的调用得本地 `cancelled`，`current_generation` 为新值，持久化未确认时为待写值。S5.6 的占位规则与作废同时生效。
- **S4.6** **接受与消费**：Provider 只接受 `request_id` 在途、未被作废、generation 等于当前值的响应（R7.3）；R6.1 允许回显为 `null` 的错误信封按发起它的那次 HTTP 交换配对，满足同样条件就照常接受（凭据类响应照常进入 S3.1）。不被接受的响应（含非 `null` 的错值）一律丢弃，得本地 `cancelled`。由响应派生的状态变化与缓存写入，在 generation 已过期、binding 已不在该变化要求的前置状态、或探测不属于当前轮次时丢弃；由已接受响应派生的撤权类事件除外。Tool 结果追加进模型上下文前再复查一次：generation 已过期，或该 binding 正在作废、`persist_pending` 为真、状态为 `disabled`、`registered`、`retired` 时丢弃（Off 换成本地 `knowledge_off`，其余换成 `cancelled`）；`loading`、`degraded`、`unavailable` 不阻止追加。复查与追加之间不得插入作废。
- **S4.7** **run 绑定 binding 集合，不绑定 generation**：run 开始时持久化当时激活的 binding 集合，以及每个已选用 profile 当时指向的候选身份元组（Off 时也记）。之后每次调用使用这些 binding 当时的 generation；集合中的 binding 未激活或已 `retired` 时本地拒绝（§6），不改用其他 binding。因此重新选用、同一 principal 重新登录、Off 后再 On 时旧 run 继续可用。集合为空时，在第一次 dispatch 时若有与某个候选元组完全一致的激活 binding，补记一次（放开类）；被本地拒绝的调用不占用补记。run 绑定或补记未确认持久化时，只有该 run 的 Knowledge 调用得 `local_storage`。
- **S4.8** **调用级作废**：一次 Tool 调用派生的全部子请求（READONLY R3.7 的分组与补齐）绑定该调用准入时的 generation；任一子请求被作废，该 binding 的整个部分得本地 `cancelled`，已接受的部分结果不作为 partial 交出。补齐只在 generation 未变、调用未作废、binding 可服务且用当前凭据时发出。翻页是新调用。涉及多个 binding 时各 binding 的部分独立交付。
- **S4.9** **多个 binding 同时激活**：每个 binding 独立维护状态、generation、轮次、窗口计数、证据纪元与占位规则、`persist_pending`；一个 binding 的事件不影响其他 binding。Off 作用于全部 binding，停用、移除只作用于该 profile。Provider 不跨 binding 合并排名（R3.6、R3.7），结果按 binding 分别交出，证据与引用带 `binding_id`，去重、`score_space` 分组与缓存都以 binding 为界。

## 5. 证据、缓存与数据

- **S5.1** Off 与 On、profile 停用标志都持久保存，重启后不变。Off 是收紧（I2），On 与启用是放开（I1）。
- **S5.2** Off 是宿主级开关，选用或取消选用 profile 都不改变它；只关某个库用停用 profile。新安装默认 Off，用户显式打开后才进入 `loading`。
- **S5.3** Off 不删除任何数据：不删服务端 KB 与文档、不删本地数据目录、不清长期登录态、不停止 local-managed 进程；只丢弃内存里的短期凭据与缓存。
- **S5.4** 停用不删数据。移除 profile 时询问用户是否删除该 profile 的本地数据目录：选择删除则在 sidecar 停止后删除；服务端的 KB 与文档一律不动。历史证据按 S5.6 处理，不因删除与否改变。local-managed 疑似泄露时轮换本实例秘密，按撤权信号处理。
- **S5.5** 已交付给模型或用户的内容追不回，任何事件都不得声称撤回（R7.7）。
- **S5.6** **占位**：每条交付的证据记下 `binding_id`、所属 KB 与准入时该 binding 的证据纪元（持久、单调，与 generation 无关）；一条证据可以记多个所属 KB（如 ContextPack 记下全部来源 KB），下文「覆盖它所属 KB」指覆盖其中任一 KB。撤权类事件、凭据无效类续签也失败、取消选用、注销、停用或移除 profile、换实例、principal 变化、改端点或租户，在任何状态下发生都使纪元 +1 并记一条适用全部 KB 的占位规则；宿主自己发起的删除 KB 成功后纪元 +1，规则只适用该 KB。组装发给模型的历史时，证据属于 `retired` binding、`binding_id` 不在持久状态里、或纪元小于某条覆盖它所属 KB 的规则，就换成不含正文的占位；待复验或删除墓碑暂扣的证据同样不发正文，解除后恢复。Off、On 与单纯重启不改纪元、不产生占位（全局收紧除外）。占位规则持久化失败时，组装历史仍执行内存中的规则。用户可见的历史如何显示由 KB-LITE-04① 定。
- **S5.7** 本地 `cancelled` 只交 Tool 层，不进上下文、缓存或证据；宿主日志记 `request_id`、新旧 generation、事件类型，不记 query、正文与凭据（R10.3）。
- **S5.8** 任何状态都不缓存证据：同一查询再次调用必须重新发请求。
- **S5.9** **桥的接受条件**：桥上的消息只在 generation 等于当前值、准入开放时被接受；作废生效到新值确认持久化之间拒绝所有消息。
- **S5.10** **服务端收窄**（按范围变窄处理）：Provider 按 binding 持久维护「最近一次可见」的 KB 集合：已接受的 search、read 成功响应的 `effective_scope` 与带 scope 的就绪响应列出的 KB 加入集合，判定时与该 binding 交付过、未被占位覆盖的证据所属 KB（多 KB 证据取其全部 KB）取并集。只拿用当前凭据发出的请求，在同一请求范围 Q 内比较：Q 中的某个 KB 在集合里，但成功响应的 `effective_scope` 或带 scope 的就绪响应不含它（`forbidden/empty_scope` 视为 Q 中全部 KB 都不在），即判为被移出。其他 `forbidden` 不参与，不同 Q 的结果不互相比较。判为移出时把这些 KB 移出集合，并按 S3.1 的范围变窄处理（作废、递增、占位、计入窗口）；触发它的那次结果在追加前换成本地 `cancelled`，之后的新调用把服务端的 `forbidden` 原样交给 Tool 层。**宿主自己发起的删除 KB**：先记删除墓碑（收紧类）并移出集合，墓碑存在期间响应中出现该 KB 不加回、省略它不算收窄；删除成功后只占位该 KB，失败则撤回墓碑（放开类）；全过程不递增、不作废、不计入窗口；重启时墓碑未决则暂扣并重查。局限：之后既不查询也不探测的 KB 无法发现被收窄。
- **S5.11** **缓存**（凭据、能力声明、就绪结果、按 KB 的 `score_space`、调用的合并缓冲区）：作废时全部清除，取得新凭据后重建；身份变化后新 binding 重新做能力发现；服务不可用与降级只清就绪结果，回到 `loading` 时重新判定兼容（R9.4）；凭据无效类只丢弃当前短期凭据。缓存不跨进程保存。
- **S5.12** **存在性不可区分**：服务端的删除、移出范围与不存在对 Provider 不可区分（R2.5、R2.6、R8.4），一律按 S5.10 判定；本地结果、占位与宿主日志不得表达某个 KB 或文档是否存在、是否被删除。

## 6. Provider 本地结果

按 binding 各自判定。**新调用**（以及 I1 中被拒的放开）按下表自上而下取第一个成立的情形；都使用 `code: unavailable`。

| 情形 | `reason` | retryable |
|---|---|---|
| Off（含全局收紧、持久状态损坏后） | `knowledge_off` | false |
| `persist_pending` 为真；因未确认的收紧或正在持久化的放开而被拒的放开；run 绑定未确认持久化的 run | `local_storage` | false |
| 仅 profile 停用；`retired`；run 绑定的 binding 已不激活 | `profile_inactive` | false |
| 未绑定占位记有失败 `reason`（S2.5） | 同记录的 `reason` | `dependency` 为 true（profile 带 `unsaved_settings` 时 false），其余 false |
| 其余 `registered`（含待确认、未记失败的占位） | `profile_inactive` | false |
| `loading`；`degraded` 下未 `ready` 的操作；`unavailable`（`not_ready`）；待复验未解除 | `not_ready` | true |
| `unavailable`（`dependency`） | `dependency` | true |
| `unavailable`（`incompatible`、`credential_rejected`、`access_revoked`、`instance_unidentified`） | 同状态的 `reason` | false |

**已发出或已接受的调用**不查此表：被作废或不被接受得 `cancelled`（retryable false，带 `current_generation`）；追加前复查丢弃时 Off 得 `knowledge_off`、其余得 `cancelled`；旧凭据请求的凭据类响应、复验通过的旧请求得 `not_ready`（retryable true）。

- **S6.1** 本地结果使用 READONLY R6.1 的信封，不发网络请求、不上线，不进 #1012 的线上 schema。`request_id` 为本次调用新生成的值，`generation` 为当前值（占位为 `null`）；`cancelled` 与旧凭据请求的 `not_ready` 回显原请求的 `request_id` 与准入时的 generation，新值放 `current_generation`（READONLY §11.4）。本地 `reason` 只挂在 `unavailable` 或 `cancelled` 上，不新增 code，不用 `unauthorized`。本文新增的本地 `reason` 闭集：`knowledge_off`、`profile_inactive`、`incompatible`、`credential_rejected`、`access_revoked`、`instance_unidentified`、`local_storage`。`retryable: false` 的本地结果不得自动重试（R6.6）。

## 7. 与 M1 记忆开关的独立性

- **S7.1** Knowledge Off 与 M1 个人记忆开关互不读取、互不改写、不触发对方的转移或 generation；Knowledge 不复用 M1 的 `memory_settings` 命名空间或键。四种组合都合法，各按自己的开关生效。
- **S7.2** 两者都持久、关闭都不删数据；读不出时 Knowledge 按关（S1.8），M1 按自身规则。Knowledge 证据能否被 M1 写入由 KB-SEC-01b 定。

## 8. 与 READONLY r8 的接口

- 本文消费：信封与本地 `cancelled`（R6.1、§11.4）、`code` 闭集与未知值兜底（R6.2）、重试即新请求（R6.6）、取消与 generation（R7.1–R7.5）、撤权在下一个请求生效与 `effective_scope` 回报（R7.6）、追不回（R7.7）、就绪（R8.1、R8.2、R8.4）、兼容判定（R9.4）、Provider 层合并与 `score_space`（R3.6、R3.7）、deadline（R3.3、R4.5）、越界与不存在不可区分（R2.5、R2.6）、日志不含正文与凭据（R10.3）、principal 来源（R1.3、R1.4）。
- 本文回答 READONLY 交给 #1013 ① 的问题：generation 何时递增、Off 是否持久（R7.4，见 §4、S5.1）；服务端就绪到宿主状态的映射（R8.5，见 S1.3）。
- READONLY 术语表把 generation 写作按「profile × principal」维护、任何作废都 +1；以本文为准：按 binding 维护，取消单个 run 不递增。待 READONLY 下一修订同步。服务端只回显 generation（R7.5，READONLY §12 #6）。

## 9. 交接点（只写接口）

- **UI-BRIDGE（#1013 ②）读取**：binding 与 `binding_id`、服务状态与准入开放、generation 的当前值与递增事件（S4.2）、作废生效的时点（S4.5）、`retired`（S1.6）、全局收紧与待确认（S1.13、S1.14）。**它提供**：凭据的取得、保存与销毁，长期登录态的作废与销毁（S2.7、S1.14），注销时的清除。约定：
  - 桥的消息受 S5.9 约束；不含秘密、不触发服务端请求的握手消息是否例外由 UI-BRIDGE 定义。
  - UI-BRIDGE 规定由 Provider 用当前凭据发出、响应为 READONLY 信封的请求（如签发入口），与 search、read 同等适用 S4.6 的接受、S3.1 的凭据分类与归属；是否作为 S5.10 的输入、以什么作请求范围由 UI-BRIDGE 指定。这类请求用已丢弃凭据带回撤权信号时，待复验的 Q 取该凭据的授权范围（S3.1）。UI 会话兑换与会话探测的响应不是 READONLY 响应，不进入本文的任何判定。签发响应里的 `unavailable/credential_near_expiry` 不交本文，不触发 S1.3 的就绪探测。
  - 收紧事件丢弃凭据之前，UI-BRIDGE 规定的撤销动作可以使用该凭据；这不推迟作废生效，不重新开放准入，也不发出 READONLY 读请求。
  - UI-BRIDGE 不新增 generation 递增事件。
- **OPS（#1013 ③）读取**：服务状态、准入、generation、作废、run 绑定（S4.7）、多 binding（S4.9）、占位（S5.6）、本地结果（§6）。约定：
  - 写请求只在 binding 可服务且准入开放时发出（额外条件由 OPS 定），带 generation，按 S4.6 接受，凭据类响应按 S3.1；写响应是否作为 S5.10 的输入由 OPS 指定。
  - 作废中止在途写请求时，本文只保证给出本地 `cancelled`；写入结果未知的表达与幂等重发由 OPS 定；待决写记录同样委托 OPS（宿主侧记录，发出前持久化，不是本文的持久状态）。
  - OPS 标为请求级的 `unavailable` `reason`（如 `request_in_progress`）不触发 S1.3 的就绪探测。
  - OPS 不新增 generation 递增事件，也不新增本文的持久状态。
- **实现任务**：KB-HOST-01（run 绑定字段、dispatch 时拒绝、追加前复查与上下文组装）、KB-C1-04（generation、作废与持久化）、KB-C1-01（Provider 与崩溃安全实现）、KB-LITE-04（停用与移除时的 sidecar 停止、历史可见性）以测试证明本文规则与 I1–I8。

## 10. 负责人已定的决定

1. Off 是宿主级，作用于全部 binding（#1）。
2. 新安装默认 Off（#2）。
3. Off 不删数据、不清登录态、不停 local-managed 进程，只丢内存凭据与缓存（#3）。
4. 作废事件后历史证据按占位不重发；Off、On、单纯重启不处理已有证据（#4）。
5. 不同 profile 的 binding 可同时激活，每个 profile 至多一个，结果按 binding 分别交出（#5）。
6. generation 持久保存、启动 +1、先持久化后生效、失败 fail closed（#6）。
7. run 绑定 binding 集合而非 generation；空集合按完整候选身份元组补记一次（#7）。
8. 服务不可用、降级不递增，不中止在途请求（#8）。
9. 撤权判定：`credential_revoked` 算撤权；过期先续签；范围不变或变宽不递增；变窄递增；凭据无效类不算撤权、续签一次、再失败进 `credential_rejected` 并占位（#9）。
10. 本地 `reason` 只挂在 `unavailable` 或 `cancelled` 上，不新增 code，不用 `unauthorized`（#10）。
11. 连接阶段只看 search、read，ingest 不参与（#11）。
12. local-managed 没有注销；疑似泄露时轮换秘密按撤权（#12）。
13. 实例标识纳入 binding 身份，只取自受控握手（#13）。
14. Off 时不从广告隐藏 Knowledge Tool，标明不可用及原因，不主动提示打开（#14、#18）。
15. principal 变化要用户显式确认才激活（#17）。
16. 停用不删数据；移除时询问是否删除本地数据目录，服务端数据不动（#19）。
17. 不能确定收紧都已持久化时全局收紧，长期登录态只作废不销毁（#20、#24）。
18. 服务端收窄按撤权处理；宿主自己发起的删除先写墓碑、只占位该 KB（#21）。
19. 撤权类事件 10 分钟内第 3 次不再自动重签（#25）。
20. 规范只冻结对外行为与不变式 I1–I8，实现机制不冻结（#26；2026-10-20 压缩为本核心）。
21. 被拒的放开直接丢弃、不排队；阻止范围按依赖对象（#27）。

## 11. 非规范材料

完整版（含转移表、验收用例、故障注入用例、参考设计、现状证据与评审记录）保留在分支 `ab/weknora-03a-state-machine`（#1072 的分支），由实现任务以测试证明，不约束本契约。
