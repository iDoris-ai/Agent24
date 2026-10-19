# Knowledge profile / Off / 撤权状态机契约 v1（CONTRACT-STATE-v1）

> **状态**：**草案（Draft）**，待 fork 侧维护者（David）评审（文档 PR，base `ab/weknora`）。评审通过前不授权任何实现。
> **版本**：契约 v1，文档修订 r17 · **日期**：2026-10-20 · **作者**：David Xu
> **Issue**：#1013 ①（KB-CT-03），阶段 #1000；消费方 KB-C1-04、KB-LITE-04、KB-HOST-01。
> **依据**：任务方案 v2.1（方案全文见 #986 第一条评论）与 `docs/knowledge/CONTRACT-READONLY-v1.md`（下称 READONLY，按 r8）。来源标签沿用 READONLY：决策 `[D1]`–`[D8]`、任务条目（如 `[KB-C1-04]`）、READONLY 规则（如 `[R7.4]`）。
> `[新设]` 表示本文新设计、在决策、方案与 READONLY 里都找不到依据的规则；与其他标签并列时，表示在该依据之上新增的部分。所有 `[新设]` 都待本 PR 评审（§10 #15）。
> **分片**：本文分三片提交。第一片：§0–§3（状态、profile、binding 身份、事件与转移）、§9 现状证据、§10 待决；第二片：§4–§8（generation 作废流程、事件处理、本地结果、M1 独立、测试清单）；第三片：附录 A（非规范性参考设计）。第一片里指向 §4–§8 与附录 A 的引用在后两片落地。**三片作为一组合入，不单独关闭任务①。** 附录 A 不是规范，实现可以替换；规范只有正文。
> **现状证据**：Agent24 的 `file:line` 都按 `ab/weknora` 的 `eb1a58b`，路径相对仓库根目录，汇总在 §9。

## 0. 范围、边界与变更规则

### 0.1 本文冻结什么
Agent24 宿主侧（KnowledgeProvider 及其 profile 管理，下称"Provider"）一个 Knowledge binding 的身份、状态、事件、转移、generation 递增规则，以及每个事件对在途请求、迟到响应、缓存、后续注入的处理。
本文只定语义，不定 Rust/TS 类型、存储位置、UI 文案、探测间隔与退避数值。服务端（WeKnora）的行为只引用 READONLY，本文不给服务端新增义务。

### 0.2 与相邻任务的边界
| 留给 | 内容 | 本文怎么引用 |
|---|---|---|
| READONLY | 请求带 generation 与迟到响应丢弃（R7）、Provider 层合并（R3.7）、就绪（R8）、兼容判定（R9.4）、错误信封（R6） | 直接引用，不重复 |
| #1013 ② | 交换票据、session JWT、消息桥；Provider 如何取得、保存与销毁凭据；桥消息如何携带 generation | 本文只定 generation 的值与递增时机；桥的接受条件见 S5.9；`retired` binding 的登录态见 S2.7 |
| #1013 ③ | ingest、job、ContextPack、SOP | v1 只读；`operations.ingest` 不参与状态判定（S1.3） |
| KB-HOST-01 | 系统凭据存储、Provider 实例化、Tool 注册、run 绑定的实现 | 本文给语义与验收用例（§8） |
| KB-LITE-01a、KB-LITE-02、KB-LITE-04② | local-managed 的握手与实例标识、进程启动、停止、回收 | sidecar 状态的映射见 S2.2，实例标识见 S2.4，停用 profile 时按其所有权语义停进程（S2.3） |
| KB-LITE-04① | 远端切换与注销隔离、页面会话销毁、历史与引用的可见性 | 本文定 generation 与缓存；历史证据是否重发见 S5.6 |
| KB-C1-05、KB-CTX-02 | Tool 证据格式、广告、自动背景注入 | 本文只定各状态能否产生新注入（§5）与 dispatch 时的拒绝（S1.5） |

### 0.3 变更规则
沿用 READONLY §0.3。binding 身份、状态集合、事件集合、转移、generation 的递增事件都是语义：改动升主版本（`CONTRACT-STATE-v2.md`）。新增 Provider 本地 `reason`（§6）算 v1 修订。草案期规则与 READONLY 相同。

## 术语
| 术语 | 定义 |
|---|---|
| binding | Provider 连接一个服务端身份的单位，身份见 S2.4（profile、principal 与服务端身份）。每个 binding 有一个永不复用的 `binding_id`；READONLY 术语表的 generation 即按 binding 维护（定义的同步见 §10 #23）。 |
| 未绑定占位 | profile 还没有任何非 `retired` 的 binding 时的 profile 级占位（S2.5）。 |
| 激活 | binding 被选为使用中：Knowledge 为 On，所属 profile 已被用户选用且未停用，principal 已绑定（已登录；principal 变化后还须用户确认，S1.13、T36）。不同 profile 的 binding 可以同时激活，每个 profile 同一时刻至多一个激活的 binding（§10 #5，S4.9）。 |
| 连接阶段 | `loading`、`ready`、`degraded`、`unavailable` 四个状态。可服务指其中的 `ready` 与 `degraded`。 |
| loading 轮次 | 每次进入 `loading` 开始新的一轮，带一个尝试令牌（S1.9）。 |
| 曾可服务 | binding 级持久标志：该 binding 是否进入过 `ready` 或 `degraded`（S1.12）。只有进入过可服务状态的 binding 才可能交付过证据。 |
| 在途请求 | 已准入（已读取 generation）而还没完成的 search、read、能力发现或就绪请求。 |
| 迟到响应 | 请求被标记作废或已移出在途集合之后才到达的响应（R7.3、S4.6）。 |
| 缓存 | Provider 为 binding 保存的非证据数据：内存里的短期凭据、能力声明（R9）、就绪结果与按 KB 的 `score_space`（R8.4），以及一次 Tool 调用的合并缓冲区（S4.8）。v1 不缓存证据（KB-C1-04），本文任何规则都不引入证据缓存。 |
| 后续注入 | 事件之后由 Knowledge 进入模型上下文的内容：新的 Tool 结果；自动背景注入（KB-CTX-02，v1 未交付）；会话历史中已有证据在后续模型调用里的重发。 |
| 本地结果 | Provider 不发网络请求、直接交给 Tool 层的结果（§6）。 |

## 1. 状态

- **S1.1** [R8.5+新设] binding 的服务状态是闭集：`registered` / `loading` / `ready` / `degraded` / `unavailable` / `disabled`。另有终态 `retired`（S1.6）与阻断条件 `persist_pending`（S1.7），都不属于这六个状态。

| 状态 | 定义 | 进入（§3） | 离开（§3） | 新调用 |
|---|---|---|---|---|
| `registered` | profile 已登记，binding 未激活：profile 未被选用、principal 未绑定（未登录、已注销）、带待确认标志（S1.13），或只有未绑定占位 | T01、T13、T16、T19、T20、T22（占位）、T23、T24、T26、T36（profile 未选用时）、T43、T44（新 binding 待确认时） | T02、T12、T15、T36、T40、T41、T44、终结（T23–T25） | 本地拒绝 `profile_inactive`（占位记有失败 `reason` 时用它，§6） |
| `loading` | 已激活，正在建立：local-managed 完成握手并取得实例标识（KB-LITE-01a）；remote 取得凭据（#1013②）；能力发现与兼容判定（R9.4）；不带 scope 的就绪探测（R8.2） | T02、T10、T13、T16、T18、T21、T34、T35b、T36、T38、T40、T41、T42、T44；启动时已激活（T26；带不自动重试 `reason` 的除外） | T03、T04、T05、T11、T14、T19–T25、T33、T35a、T35b、T42 | 本地拒绝 `not_ready` |
| `ready` | 兼容；凭据有效；`live: true`；`operations.search` 与 `operations.read` 都是 `ready`；进入时清除 `revoked_since_ready`（S1.11） | T03、T07 | T06、T08、T11、T14、T18–T20、T23–T25、T33、T42 | 准入 |
| `degraded` | 兼容；凭据有效；`live: true`；search 与 read 恰好一个是 `ready` | T04、T06 | T07、T09、T11、T14、T18–T20、T23–T25、T33、T42 | 只准入为 `ready` 的操作，另一个本地拒绝 `not_ready` |
| `unavailable` | 已激活但不能服务，带 `reason`（S1.4） | T05、T08、T09、T18、T35b 与 T42（窗口内第 3 次）、T22、T33、T35a；启动时保持不自动重试的 `reason`（T26）；`persist_pending` 时进入连接阶段（S1.7） | T10、T11、T14、T19、T20、T23–T25、T34、T35b、T38、T42（只从 `dependency`、`not_ready`） | 本地拒绝，按 §6 |
| `disabled` | Off 标志为真（S5.1），或该 profile 的停用标志为真（S5.4），或重启时进入全局收紧、持久状态损坏（S1.8、S1.14） | T01、T11、T12、T14、T15、T17、T23、T24、T37、T43、T44；启动时标志已持久（T26） | 两个标志都清除：T13、T16；T44；终结（T23–T25） | Off：`knowledge_off`；仅 profile 停用：`profile_inactive` |

- **S1.2** [新设] **派生优先级**：服务状态按以下顺序取第一个成立的：Off 标志或 profile 停用标志为真 → `disabled`；未激活 → `registered`；否则按连接阶段取 `loading` / `ready` / `degraded` / `unavailable`。两个标志相互独立，可以同时为真。§3 的转移表是这条规则的展开，两者不一致视为文档错误，按 §0.3 修订。`persist_pending` 不参与派生（S1.7）。持久状态损坏时按 S1.14 处理（S1.8）。
- **S1.3** [R8.2/R8.4+新设] 连接阶段只看就绪响应的 `live` 与 `operations.search`、`operations.read`：`live: false`，或 search 与 read 都不是 `ready`，为 `unavailable`（S1.4）。`operations.ingest`（只读 v1 为 `disabled`）不参与判定。按 KB 的就绪（R8.4）不改变服务状态，由 Tool 结果按 KB 报告。未知的就绪 `state` 按 `not_ready`（R6.2）。
- **S1.4** [R9.4+新设] `unavailable` 的 `reason`：
  - 按退避自动重试：`dependency`（不可达、探测超时、`live: false`、sidecar 不可用，或取凭据、续签时签发方不可达）；`not_ready`（存活，但 search 与 read 都不是 `ready`）。
  - 不自动重试，只在手动重试、profile 配置变更或 sidecar 重新握手后再试（T10），其中 `credential_rejected`、`access_revoked` 另可在重新登录后再试（T38）：`incompatible`（R9.4 不兼容）；`credential_rejected`（T05：属于 S3.1 例外的签发被拒，或本轮新签发的凭据被以撤权信号拒绝且属于该例外；凭据无效类响应的续签也失败，S3.1、T42，此时占位）；`access_revoked`（`loading` 或 `unavailable` 中出现撤权信号，包括不属于 S3.1 例外的重签被拒，T35a；或撤权类事件在窗口内达到上限，T18、T35b）；`instance_unidentified`（握手拿不到实例标识，S2.4）。
  - `local_storage`：由 T33 进入，或由 S1.7 规定的、要进入 `loading` 时遇到 `persist_pending` 的转移进入；`persist_pending` 清除后回到 `loading`（T34）。其他 `reason` 的 `unavailable` 在 `persist_pending` 期间保持原 `reason`，清除后也不变。
- **S1.5** [D4+新设] 只有可服务状态发出 search 与 read。`loading`、`unavailable` 只发能力发现与就绪探测，例外：`unavailable` 的 `reason` 为 `local_storage`，或 `persist_pending` 为真时，不发任何请求；`incompatible`、`credential_rejected`、`access_revoked`、`instance_unidentified` 不自动重试。`registered`、`disabled`、`retired` 与未绑定占位不发任何 READONLY 请求，包括就绪探测。Provider 在 **dispatch 时每次**检查状态并按 §6 本地拒绝；这条是硬性要求，与 Tool 是否注册、是否向模型广告无关。**广告**（§10 #14、#18 已定）：Off 时 Knowledge Tool 仍在广告里，不隐藏；描述标明"当前不可用"及原因（Knowledge 已关闭），按当前状态每轮生成（`info()` 每轮读取，§9 第 3 条），不改注册表、不改内核接口；不主动提示用户打开。其他不可服务情形的描述由 KB-C1-05 定。
- **S1.6** [新设] **终态 `retired`**：binding 的生命周期终点。进入：换实例后的旧 binding（T21）、principal 变化后的旧 binding（T23）、改端点或租户后的旧 binding（T24）、移除 profile（T25）、持久状态损坏，或重启时的全局收紧（S1.14，界面上以待确认呈现）。`retired` 的 binding 不可再激活，只能新建 binding；不准入任何请求；绑定它的 run 一律本地拒绝，`reason` 按 §6 的顺序（S4.7）。它的最终 generation 继续保存，供同一 profile 下新 binding 取初值（S4.4）。
- **S1.7** [新设] **阻断条件 `persist_pending`**：该 binding 有尚未确认持久化的 generation 变化或放开类变化（S1.10）时为真（持久化失败、超时、结果未知，或在重试）；收紧类变化的持久化失败不置它，只影响单个 run 的持久化（run 绑定与补记，S4.7）失败也不置它。它不参与 S1.2 的派生，对外仍可显示 `disabled` 或 `registered`；但只要它为真，该 binding 不准入任何请求，不论派生状态是什么；本地结果按 §6 的顺序（Off 时 `knowledge_off`，否则 `local_storage`）。所有进入 `loading` 的转移都先检查它：为真时改为进入 `unavailable`（`local_storage`）。Provider 在后台重试，确认后清除（T34）。
- **S1.8** [新设] **持久状态损坏时 fail closed**：持久状态无法读取、损坏或不完整到无法判定时，按 S1.14 的"持久状态损坏"处理，不再区分单个 binding 的损坏。该 profile 上次已确认的 principal 无法确定时按"不同"处理（T43）。M1 不同：写入门遇到损坏内容按开，召回路径报错、不召回（§9 第 1 条）。
- **S1.9** [新设] **loading 轮次**：每次进入 `loading` 生成一个新的尝试令牌；本轮发出的凭据请求、握手与探测都带它。由这些结果派生的转移（T03–T05、T21、T22）只在令牌等于当前轮时应用；离开 `loading` 时，本轮在途的探测立即标记作废并中止。上一轮的结果因此不能推动新的一轮。占位上的登录与握手（S2.5、T40、T41）同样带尝试令牌，只有当前令牌的结果才能建立 binding。同一轮内的就绪探测带递增序号，只应用序号大于已应用者的结果。
- **S1.10** [新设] **崩溃安全不变式**（规范；David 第 9 轮后定：正文只冻结对外行为与本条不变式，实现方式不冻结，非规范性参考设计见附录 A，以 §8 的故障注入用例 FI 验收）。变化分两类。**收紧类**：Off、停用 profile、取消选用、注销（解除登录、销毁短期凭据，长期登录态按 #1013② 清除）、移除 profile、`retired`（含换实例、principal 变化、改端点或租户中旧 binding 的退役）、撤权类事件与其他 S5.6 触发事件的占位规则与证据纪元、宿主删除 KB 的墓碑与占位（S5.10）、待复验登记（S3.1）、标记待确认（S1.13、S1.14）、进入不自动重试的 `unavailable` reason（`incompatible`、`credential_rejected`、`access_revoked`、`instance_unidentified`）、占位上记下的不自动重试的失败 `reason`（`credential_rejected`、`instance_unidentified`）、复验的超时计数。`dependency` 与撤权窗口、凭据无效窗口的计数尽力持久化：持久化失败不算收紧丢失，不触发全局收紧。**放开类**：On、启用 profile、选用 profile、用户确认（S1.13）、新建或重新绑定 binding（含 generation 初值与待确认标志）、首次可服务（S1.12）、解除待复验（S3.1）、撤回删除墓碑（S5.10）、run 的绑定与补记（S4.7）、宿主级确认（S1.14 的提示）、离开不自动重试的 reason（T10 手动重试、T38 重新登录、配置变更或重新握手），以及不自动重试的占位失败 `reason`（`credential_rejected`、`instance_unidentified`）的清除与复验超时计数清零。最近一次可见集合（S5.10）的增补同样要持久化。不变式：
  - **I1 没有持久记录就不放开**：放开类变化在持久化确认之前不生效；持久化已经落盘、确认尚未返回时崩溃，重启后可以按已确认生效；持久化结果未知而放弃时，持久状态不得比内存更放开，重启后不得出现内存里从未生效、也未曾落盘的放开。**挂起期间的相反动作**：放开已提交、尚未确认时，用户做出相反的动作（如 On 挂起时点 Off），视为放弃这次放开，并持久化一条抵消它的收紧（收紧类；即使内存里是空操作也要持久化）；被放弃的放开，之后返回的确认一律忽略。**阻止范围**（第 10 轮盲选）：一个放开，只要它生效所依赖的对象上有尚未确认持久化的收紧，就被拒：宿主级收紧未确认时拒绝所有放开；binding 级收紧（含注销、退役）未确认时拒绝同一 profile 下的放开，包括给该 profile 新建 binding，按 profile 判定（T21、T23 在同一步里退役旧 binding 又新建 binding 时，新 binding 等旧 binding 的退役确认持久化之后才持久化，也可以与退役同一次原子持久化；退役没有确认持久化时新 binding 丢弃，profile 回到未绑定占位并记下 `dependency`，按 T40 退避重试，每次重试按当时的状态重新校验）；宿主级放开（如 On）在任一 binding 有未确认收紧时被拒；涉及多个 binding 的放开，任一涉及的 binding 有未确认收紧即被拒。依赖一个尚未生效的放开（新 binding 尚未持久化、宿主级确认尚未给出或尚未持久化）的放开同样被拒：所依赖的放开正在持久化时本地结果为 `local_storage`，宿主级确认尚未给出时 Off 仍为真，本地结果为 `knowledge_off`。依赖宿主级确认的放开只有 On 与 T44。**被拒的放开直接丢弃**（第 10 轮盲选）：不排队、不自动重放，本地结果按上面两种原因给出（因未确认的收紧被拒为 `local_storage`）交给发起方；用户发起的，在所依赖的收紧或放开确认持久化之后界面可以提示重试，重试按当时的状态重新校验；系统发起的（run 绑定、解除待复验等）由发起方按自己的逻辑重试。
  - **I2 收紧立即生效、不回退**：收紧类变化在内存立即生效（作废事件即 S4.5 ①），持久化失败不阻断收紧、不置 `persist_pending`，界面提示"部分设置尚未保存"。之后任何时刻崩溃，重启后要么得到这项收紧，要么进入 S1.14 的全局收紧。保证的边界：持久化介质在收紧之后完全不可写、随后崩溃，重启时可能两者都得不到，这是残余风险，FI 不注入这种故障（FI 只注入单次持久化失败与崩溃）。崩溃之后，只要能确定所有已生效的收紧都已持久化，就按正常恢复，属于 §10 #4 所说的"单纯重启"。
  - **I3 不确定一律收紧**：重启时只要不能确定所有已生效的收紧都已持久化，或持久状态损坏，就按 S1.14 收紧，不得按较放开的状态恢复。
  - **I4 generation 不撞号**：任何崩溃序列之后，每个非 `retired` binding 的 generation 大于崩溃前用过和对外回报过（含 S4.5 的待写值）的任何值；新 binding 的初值大于同一 profile 下用过或回报过的任何值（含 `retired` binding 的值）。`retired` 的 binding 不再准入，它用过和回报过的值由同一 profile 下新 binding 的初值越过（S4.4），不再给它递增。
  - **I5 作废后的证据不重发**：被 S5.6 规则覆盖的证据、属于 `retired` binding 的证据、`binding_id` 在重启后的持久状态里不存在的证据、待复验暂扣的证据，以及删除墓碑未决时暂扣的证据，在任何崩溃序列后都不带正文发给模型。
  - **I6 退役不复活**：`retired` 的 binding 在任何崩溃序列后都不再激活，它的长期登录态不用于任何 binding（S2.7）。不能确定退役是否已持久化时（全局收紧），所有 binding 都按可能已退役处理（S1.14）。
  - **I7 Off、停用、取消选用与注销不恢复成放开**：四者生效之后，任何崩溃序列重启，要么保持，要么进入全局收紧；全局收紧后长期登录态作废，不恢复使用（S1.14）。
  - **I8 待确认不丢失**：带 `principal_changed` 的 binding 与带 `unsaved_settings` 的 profile（S1.13），在用户确认持久化之前，任何崩溃序列后重启都仍待确认，不自动取凭据。全局收紧时，原带 `principal_changed` 的 binding 退役，待确认转到所属 profile 的 `unsaved_settings`。

- **S1.11** [新设] **`revoked_since_ready`**：binding 级标志，只在内存。撤权类事件（T18、T35a、T35b）置位；该 binding 进入 `ready` 时清除（T03、T07）。T38 以同一 principal 重新登录不清除；换 principal 时随 T23 新建的 binding 不带这个标志。签发被拒是否走 T05 见 S3.1 的例外，例外还要求该 binding 从未可服务（S1.12），因此不只依赖这个只在内存的标志。
- **S1.12** [新设] **曾可服务**：binding 第一次进入 `ready` 或 `degraded`（T03、T04）前，"首次可服务"（放开类）持久化确认后才进入；未确认时按 S1.7 进入 `unavailable`（`local_storage`，T33），T34 后回到 `loading`。之后不撤销；无法确定时按"已可服务"处理。用途：S3.1 的例外只适用于从未可服务的 binding。重启、Off 再 On、T08→T10 之后，`loading` 中第一次出现的撤权信号只要该 binding 曾可服务，就走 T35a 并占位，不走 T05。
- **S1.13** [新设] **待确认标志**：分两种。**`principal_changed`**：挂在 binding 上。T23、T43 新建的 binding 带这个标志，它与新 binding（含 generation 初值）是同一次原子持久化；未经用户确认（T36）不算激活，派生为 `registered`。用户确认是放开类，确认持久化后才生效，并记下这个已确认的 principal；新 binding 尚未持久化时，用户确认按 I1 被拒，新 binding 持久化之后用户再确认。不需要确认而建立的 binding，其 principal 同样记为"已确认"。是否待确认，以该 binding 最近一次"标记待确认"与"确认"中较晚的那次为准（§10 #17）。**`unsaved_settings`**：挂在 profile 上，由 S1.14 的全局收紧与持久状态损坏设置（收紧类，持久保存），只由 T44 解除；它覆盖只有未绑定占位的 profile，以及 binding 已因身份不符退役的 profile。带这个标志的 profile 下所有 binding 都是 `retired`，登录与握手只能经 T44（T02、T36、T40、T41、T43 不适用；T13、T16、T37 只改标志，不进入 `loading`、不建立 binding）；在带标志的 profile 上，单纯登录或握手不算确认：Provider 先请用户确认该 profile，用户确认后，这次登录或握手作为 T44 的一部分，用户不确认则不建立 binding。依赖方按两种原因给出不同提示。
- **S1.14** [新设] **重启后的可观察语义**（规范，依赖方可以依赖；§10 #4、#17、#20、#24 已定）。Provider 启动时先做正常恢复里的 `retired` 判定，再分四种情形。**正常恢复**：持久状态完好，且能确定所有已生效的收紧都已持久化。每个 binding 按持久状态恢复：Off、停用、选用、已注销、`retired`、待确认及其原因、已确认的 principal、首次可服务、待复验与暂扣、删除墓碑、run 绑定、占位规则与证据纪元、最近一次可见集合，以及不自动重试的 `unavailable` reason（`incompatible`、`credential_rejected`、`access_revoked`、`instance_unidentified`，重启后保持该 reason，不自动取凭据）、占位上记下的失败 `reason`、撤权窗口与凭据无效窗口的计数、复验的超时计数；generation 按 S4.4 递增；不新增占位，不提示（§10 #4）。`retired` 判定：binding 的身份元组与 profile 当前配置不符（端点、租户、配置修订号，或握手得到的实例标识已变），或所属 profile 已不存在，或存在长期登录态且其 principal 与 binding 不同（没有登录态，如已注销或 local-managed，不算不同；带 `principal_changed` 的 binding 除外，它在 T36 确认时校验），都判为 `retired`。待复验未解除的 binding 继续不准入、暂扣，取得凭据后重做复验；删除墓碑没有成功或撤回结果的 KB，先暂扣该 KB 的证据，再重查或重试删除。**全局收紧**（I3；第 10 轮后经 Grok、Opus 盲选补全）：持久状态完好，但不能确定所有已生效的收紧都已持久化。Knowledge 为 Off 并持久化；每个非 `retired` 的 binding 记一条适用全部 KB 的占位规则（纪元高于历史中该 binding 证据出现过的最大纪元），并判为 `retired`；每个 profile 都标记 `unsaved_settings`（S1.13），界面上以待确认呈现；所有长期登录态作废：在用户重新登录或握手之前不得用于任何 binding，但不由本文销毁（销毁方式按 #1013②，§10 #22；第 11 轮后 David 定）；带 `unsaved_settings` 的 profile 上，T40、T41 暂停，登录与握手只能经 T44；向用户提示"上次有设置未保存，其中可能包含已移除的项"（宿主级）。用户确认这条提示（放开类）之后才能 On；逐个 profile 的确认要重新登录（remote）或重新握手（local-managed），建立新的 binding（T44），身份按常规规则校验；绑定旧 binding 的 run 失去 Knowledge。宿主级确认持久化之前再次启动：只有全局收紧的全部写入都已确认、且之后没有尚未确认的收紧时，才按已写下的状态恢复，不再追加新的全局收紧、不再抬高纪元，只再提示一次；否则照常按 I3 再次全局收紧；确认持久化之后不再提示，也不因同一次不确定再次全局收紧。哪一项细粒度收紧丢失无法得知，因此退化为全局收紧。**持久状态损坏**（S1.8）：除全局收紧的各项外，所有 binding 判为 `retired`，profile 回到未绑定占位并标记 `unsaved_settings`；所有 profile 按未选用、已停用处理，由用户在确认时重新设定；全部历史证据换成占位（I5）；之后新 binding 的初值大于损坏前可能用过的任何值（S4.4）。确认持久化之后，下一次启动不得因同一损坏再次进入此情形。**首次安装**：没有任何持久状态，且宿主能确认是首次安装：按 §10 #2 默认 Off，不提示、不收紧。会话历史里有任何 Knowledge 证据，或存在任何 Knowledge profile 配置时，不得视为首次安装；不能确认是首次安装而持久状态缺失时，按持久状态损坏处理。

## 2. profile 与 binding 身份

- **S2.1** [D2/R1.3/R1.4] 两种 profile 共用 §1 的状态与 §3 的转移，差别只在下表：

| | local-managed | remote |
|---|---|---|
| principal | 本机唯一用户，激活时自动绑定（R1.4） | 签发权威认定（R1.3），登录后绑定 |
| 服务端身份 | 实例标识，只取自受控握手（S2.4） | 端点与租户（或 profile 配置修订号） |
| `loading` 的额外步骤 | 等待 sidecar 可用并完成握手（KB-LITE-01a、KB-LITE-02） | 向签发服务取凭据（#992 决定前只用 Mock 与单用户样本） |
| `dependency` 的常见来源 | sidecar 未运行、启动失败、健康检查连续失败 | 网络不可达、签发服务不可达 |
| 注销 | 没有这个事件（S5.4） | 适用 |
| 撤权来源 | 本实例撤销凭据；怀疑凭据泄露时轮换本实例秘密（S5.4） | 签发权威撤销或拒绝续签 |
| 激活前提 | R1.4 的前提成立前只用于受控样本 | R1.3 |

- **S2.2** [R8.1+新设] local-managed 的 sidecar 状态（`SidecarState`，见 §9）是服务状态的输入，不是服务状态本身：sidecar 进入 `starting` 时，`unavailable` 的 binding 经 T10 回到 `loading`；`ready`、`healthy` 不单独改变服务状态（health ≠ ready，R8.1），由握手与 Knowledge 就绪探测决定；`degraded`、`failed`、`stopping`、`stopped` → `unavailable`（`dependency`）：`loading` 中经 T05，`ready`、`degraded` 中经 T08、T09。
- **S2.3** [KB-LITE-04+新设] Off 不停止 sidecar（§10 #3、S5.3）。停用或移除 local-managed profile 时，先按 T14 进入 `disabled`、丢弃凭据、递增 generation（只在连接阶段），再按 KB-LITE-04② 的所有权语义停止 sidecar；hide ≠ disable。
- **S2.4** [D2+新设] **binding 身份**：remote 为 profile × principal × 端点 × 租户（或 profile 配置修订号）；local-managed 为 profile × principal × 实例标识。任何一项变了就是新 binding，旧 binding 进入 `retired`（T21、T23、T24）。local-managed 的实例标识只取自 KB-LITE-01a 的受控握手，不取 HTTP 响应自报的值；同一实例重启（标识不变）不算换实例，generation 不变。握手拿不到标识时按 T22 处理：不新建 binding，不递增；`instance_unidentified` 按收紧类持久化（S1.10）。换实例只能靠 sidecar 状态变化或握手发现；新实例若仍接受旧凭据（例如复制了数据目录），归 KB-LITE-01a、KB-LITE-02 处理，不在本文。
- **S2.5** [新设] **未绑定占位与 binding 建立**：profile 登记后、首个 binding 建立前，以及所有 binding 都已 `retired` 后（例如改端点或租户，T24），profile 只有一个未绑定占位：状态取 `registered`（Off 或停用时取 `disabled`），没有 generation，不准入任何请求。登录（remote）与握手（local-managed）在占位上进行；成功后新 binding 连同 generation 初值（S4.4）持久化，确认后按 T02 进入 `loading`（所属 profile 带 `unsaved_settings` 时不在此建立，按 S1.13、T44），才发 READONLY 请求（能力发现、就绪探测）。失败时占位保持 `registered`，并记下失败 `reason`（`credential_rejected`、`instance_unidentified`、`dependency`）供本地结果使用；被选中、且 Off 与停用标志都为假的占位，记有 `dependency` 时按退避自动重试，local-managed 在 sidecar 进入 `starting` 时也重试（T40，与 T10、S2.2 一致）；记有 `instance_unidentified` 或 `credential_rejected` 时，只在手动重试、sidecar 重新握手或重新登录后再试（T41）。Off 或停用期间暂停重试，不发任何请求。登录与握手本身不是 READONLY 请求，不带 generation。还有非 `retired` 的 binding 时，登录得到的 principal 与该 binding 的 principal 不同按 T23（该 binding 带 `principal_changed` 时按 T36 的规则处理）；只剩占位时，登录得到的 principal 与该 profile 上次已确认的 principal（S1.13）不同按 T43，新建的 binding 带待确认标志（S1.13），停在 `registered` 等用户确认（T36）。上次已确认的 principal 不可读按"不同"处理（S1.8）。
- **S2.6** [新设] **持久键**：generation 等 binding 级持久状态以 `binding_id` 为键。身份元组（S2.4）随 binding 一起持久化，用来查找"该身份下非 `retired` 的 binding"。同一身份元组在旧 binding `retired` 之后再次出现（例如端点 X → Y → X），新建 binding，取新的 `binding_id`；generation 初值仍按 S4.4 取同一 profile 下的最大值 + 1。
- **S2.7** [新设] `retired` binding 的长期登录态不得用于任何其他 binding（包括同一身份元组后来新建的 binding）。是否销毁、如何销毁由 #1013② 定义（§10 #22）。

## 3. 事件与转移

- **S3.1** [D2+新设] 事件定义：
  - **Off / On**：用户关闭或打开 Knowledge，改的是宿主级 Off 标志（S5.1、S5.2）。**停用 / 启用 profile**：改的是该 profile 的停用标志，持久保存（S1.10）。**移除 profile**：先停用再终结（T25）。
  - **撤权类事件**分两种。**撤权信号**：响应为 `unauthorized` 且 `reason` 是 `credential_revoked`；签发权威通知撤销，或拒绝续签（凭据无效类响应触发的那一次续签被拒按下面的凭据无效类处理，除非签发方明示已撤销）；不属于下面例外的重签被拒（S1.11、S1.12）；local-managed 轮换本实例秘密。**范围变窄**：同一 principal 续签后凭据有效，但范围更窄（新范围既有新增又有移除时，只要移除了任一 KB 或操作，就算变窄）；或凭据没变、服务端收窄了授权（S5.10，§10 #21 已定按撤权处理）。**任何连接阶段**出现撤权类事件，不论凭据是新签发的还是沿用的，都按 S4.5 作废在途请求、递增 generation、触发 S5.6 的占位，并置 `revoked_since_ready`。去向：`ready`、`degraded` 走 T18（回到 `loading` 重签）；`loading`、`unavailable` 中的撤权信号走 T35a（`access_revoked`），范围变窄走 T35b（回到 `loading`：续签变窄用新凭据，服务端收窄重新取凭据）。由已接受响应派生的撤权事件不受 S4.6 前置状态检查的约束。`credential_expired` 先续签：续签成功按 T27，被拒算撤权，签发方不可达按 T08、T09 进入 `unavailable`（`dependency`），不递增；`loading`、`unavailable` 中同样先续签：成功、且同一 principal、范围相同或更宽，则在 `loading` 中继续（`unavailable` 时按 T10 回到 `loading`），principal 变化按 T23，范围变窄按 T35b，被拒按撤权（T35a，例外条件下按 T05），签发方不可达按 T05 进入或保持 `unavailable`（`dependency`）。`forbidden`（任何 `reason`）只是请求级错误（T28），本身不算撤权；它带出的服务端收窄按 S5.10 判定。
  - **凭据无效类响应**（§10 #9 已定）：响应为 `unauthorized` 且 `reason` 是 `credential_invalid`、`credential_missing` 或未知值。第一次不算撤权：不递增、不作废在途请求、不占位、不置 `revoked_since_ready`；Provider 立即停止准入新调用，丢弃当前短期凭据，进入 `loading` 的新一轮并续签一次（T42；`unavailable` 只在 `dependency`、`not_ready` 时这样做，其他 `reason` 保持不变）。续签成功按正常的 `loading` 判定。**续签也失败**（签发方拒绝这次续签，或续签得到的凭据仍收到凭据无效类响应）：按 T05 进入 `unavailable`（`credential_rejected`），并按 S5.6 记一条适用全部 KB 的占位规则；不递增，也不声称已撤权。"续签也失败"只在 T42 开启的那一轮 `loading` 内判定；该轮进入可服务状态后，再收到凭据无效类响应重新按 T42（计入下面的上限）。签发方明示已撤销（如以 `credential_revoked` 拒绝）时按撤权处理（T35a）；签发方不可达按 T05 进入 `unavailable`（`dependency`），不占位。**凭据归属** [新设]：请求记下所用短期凭据的代号；只有用当前凭据发出的请求，其凭据无效类响应才触发上述处理；用已丢弃的旧凭据发出的请求，只把结果换成本地 `not_ready` 交给 Tool 层（§6），不触发转移。**旧凭据的撤权信号** [新设]（David 第 7 轮后定；未完成时的收尾经 Grok、Opus 盲选）：用已丢弃的旧凭据发出的请求带回撤权信号时，不立即按撤权处理。Provider 记下待复验（收紧类：binding、被质疑请求的 Q 与版本号；同一 binding 的多条合并，Q 取并集，版本号 +1），在内存立即生效：该 binding 不准入新调用（本地 `not_ready`），历史中该 binding、Q 内 KB 的证据正文暂扣，包括登记之后追加的证据（S5.6）。然后用当前凭据发一次复验探测（带 scope 的就绪探测，范围覆盖 Q）：复验得到撤权信号，按撤权处理（T18、T35a），暂扣变为占位；复验通过、且这次复验覆盖的版本号仍是当前版本，解除待复验（放开类），持久化确认后恢复准入、解除暂扣，旧请求的结果换成本地 `not_ready`；版本号已变则按新的 Q 再复验；复验得到凭据无效类响应，按本段凭据无效类处理，待复验保留。复验没能完成（被 S4.5 中止、复验期间崩溃、不可达或超时、当前没有可用凭据）时待复验保留，不准入、暂扣继续；之后该 binding 每次取得当前凭据并处于连接阶段时重做复验，超时按退避重试，连续 3 次超时后停止自动重试，等手动重试或重新登录。binding 终结（`retired`）时待复验仍未解除，按撤权收尾：暂扣的证据换成占位。服务端收窄不适用复验；它只比较用当前凭据发出的请求（S5.10）。**上限** [新设]：同一 binding 10 分钟内第 3 次凭据无效类事件不再续签，直接按"续签也失败"处理。本类响应不适用下面的例外。
  - **唯一例外**：`revoked_since_ready` 为假、且该 binding 从未可服务（S1.12）时，`loading` 中签发方拒绝签发，或本轮新签发的凭据被服务端以撤权信号拒绝：走 T05，记为 `credential_rejected`，不递增、不占位。**窗口上限** [新设]：同一 binding 10 分钟内第 3 次撤权类事件（T18、T35a、T35b 都计入）仍按 S4.5 处理，但去向一律改为 `unavailable`（`access_revoked`），不再自动重签，避免活锁；这条优先于 T35b 在不自动重试 `reason` 下保持原 `reason` 的规则。
  - **principal 变化**：续签或登录得到的 principal 与 binding 不同（T23）。**改端点或租户**：remote profile 的这两项配置被修改（T24）。**换实例**：local-managed 握手得到不同的实例标识（T21）。
  - **取消选用**：用户把一个 profile 移出使用中，它的 binding 离开（T19）。选用另一个 profile 不影响已激活的 binding（S4.9）。重新选用原 profile 时，原 binding 未终结就按 T02 重新激活。
  - **注销**：remote binding 的 principal 登出：解除登录，销毁短期凭据，长期登录状态按 #1013② 清除（T20）。
  - **服务不可用 / 降级 / 恢复**：就绪探测的结果（S1.3、S1.4）。单个请求得到 `unavailable` 或 `timeout` 只触发一次就绪探测，由探测结果决定是否转移，单次失败不直接改状态。
- **S3.2** [新设] 同一 binding 的事件串行处理：一个作废事件的 S4.5 第 ①–④ 步全部完成后，才处理下一个事件。第 ② 步持久化有时限，超时按结果未知处理、确认前不生效（S4.4），不会让后续事件无限等待。

| ID | 前置 | 事件 | 后置 | generation | 依据 |
|---|---|---|---|---|---|
| T01 | — | 登记 profile | 未绑定占位（S2.5），按 S1.2 派生：Off 时 `disabled`（`knowledge_off`），否则 `registered` | 无 | [新设] |
| T02 | `registered`（所属 profile 不带 `unsaved_settings`；binding 不带 `principal_changed`） | 激活：选用该 profile，或登录（登录得到的 principal 等于该 binding 的 principal；只有占位时与上次已确认的相同或首次）；占位在 binding 建立后进入 | `loading`（`persist_pending` 时 `unavailable`/`local_storage`） | 不变（新建时写初值） | [KB-HOST-01] |
| T03 | `loading` | 兼容，`live: true`，search 与 read 都 `ready` | `ready`（首次先持久化"首次可服务"，S1.12） | 不变 | [R8.2/R9.4] |
| T04 | `loading` | 兼容，`live: true`，恰好一个 `ready` | `degraded`（同 T03） | 不变 | [新设] |
| T05 | `loading` | 不兼容、不可达、`live: false`、两者都不 `ready`、sidecar 故障、签发方不可达；S3.1 例外中的签发被拒或新凭据被以撤权信号拒绝；凭据无效类响应的续签也失败（S3.1，此时按 S5.6 占位） | `unavailable`（S1.4；凭据被拒为 `credential_rejected`） | 不变 | [R9.4+新设] |
| T06 | `ready` | 降级 | `degraded` | 不变 | [新设] |
| T07 | `degraded` | 恢复 | `ready` | 不变 | [新设] |
| T08 | `ready` | 服务不可用（含续签时签发方不可达、sidecar 不可用） | `unavailable` | 不变 | [新设] |
| T09 | `degraded` | 同 T08 | `unavailable` | 不变 | [新设] |
| T10 | `unavailable` | 重试：`dependency` / `not_ready` 按退避自动；sidecar 进入 `starting`（S2.2）；不自动重试的 reason 在手动重试、配置变更或 sidecar 重新握手后（重新登录见 T38）；回到 `loading` 后沿用的凭据收到撤权信号按 T35a、收到凭据无效类响应按 T42 | `loading`（`persist_pending` 时按 S1.7 进入 `unavailable`/`local_storage`） | 不变 | [新设] |
| T11 | 连接阶段 | Off | `disabled` | +1 | [KB-C1-04] |
| T12 | `registered` | Off | `disabled` | 不变 | [新设] |
| T13 | `disabled` | On | 按 S1.2 派生（profile 仍停用则 `disabled`；进入 `loading` 前查 `persist_pending`）；所属 profile 带 `unsaved_settings` 时只改标志，占位按 S1.2 派生为 `registered` 或 `disabled`，不进入 `loading`、不建立 binding（只能经 T44） | 不变 | [KB-C1-04] |
| T14 | 连接阶段 | 停用 profile | `disabled` | +1；丢弃凭据 | [新设] |
| T15 | `registered` | 停用 profile | `disabled` | 不变 | [新设] |
| T16 | `disabled` | 启用 profile | 按 S1.2 派生（仍 Off 则 `disabled`；进入 `loading` 前查 `persist_pending`）；所属 profile 带 `unsaved_settings` 时只改标志，占位按 S1.2 派生为 `registered` 或 `disabled`，不进入 `loading`、不建立 binding（只能经 T44） | 不变 | [新设] |
| T17 | `disabled` | 已停用时 Off，或已 Off 时停用 profile | `disabled`（两个标志都为真，本地结果取 `knowledge_off`） | 不变 | [新设] |
| T18 | `ready` / `degraded` | 撤权类事件（S3.1） | `loading`（重签；重签被拒按 T35a）；窗口内第 3 次改为 `unavailable`（`access_revoked`） | +1 | [D2/R7.6+新设] |
| T19 | 连接阶段 | 取消选用 | `registered` | +1 | [KB-LITE-04①] |
| T20 | 连接阶段 | 注销（仅 remote） | `registered` | +1 | [KB-LITE-04①] |
| T21 | `loading` | 换实例 | 旧 binding `retired`；新 binding 建立（等退役确认持久化，S1.10 I1），按 S1.2 派生 | 旧 +1；新 binding 按 S4.4 | [新设] |
| T22 | `loading`，或占位上的握手 | 握手拿不到实例标识 | 已有 binding：`unavailable`（`instance_unidentified`）；占位：保持 `registered` 并记下该 `reason` | 不变；`instance_unidentified` 按收紧类持久化，不写 generation | [新设] |
| T23 | 任一非 `retired` | principal 变化 | 旧 binding `retired`；新 binding 建立（等退役确认持久化，S1.10 I1），按 S1.2 派生（带待确认标志，未经用户确认不算激活，S1.13、T36） | 旧 binding 在连接阶段时 +1；新 binding 按 S4.4 | [D2+新设] |
| T24 | 任一非 `retired` | 改端点或租户（remote） | 旧 binding `retired`；profile 回到未绑定占位（按 S1.2 派生），不复用旧登录态，重新登录后按 S2.5 建立新 binding | 同 T23 | [新设] |
| T25 | 任一非 `retired` | 移除 profile | 连接阶段先按 T14 作废；之后 `retired` | 只在连接阶段 +1 | [新设] |
| T26 | 进程启动 | Provider 启动 | 按 S1.14 的恢复语义派生（不自动重试的 `reason` 保持） | 每个非 `retired` 的 binding 递增；已退役的 binding 不递增，其值由新 binding 的初值越过（I4、S4.4） | [KB-C1-04+新设] |
| T27 | `ready` / `degraded` | 续签：同一 principal，范围相同或更宽 | 不变 | 不变 | [新设] |
| T28 | `ready` / `degraded` | 单个请求得到 `forbidden`（任何 `reason`）、`not_found`、`stale`、partial（带出的服务端收窄另按 S5.10 走 T18、T35b） | 不变 | 不变 | [R2.3/R3.4/R6/R7.6] |
| T29 | 任一 | M1 记忆开关切换 | 不变 | 不变 | [KB-C1-04] |
| T30 | `ready` / `degraded` | 用户取消一个 run | 不变 | 不变；该 run 的在途请求立即移出在途集合并中止 | [R7.1+新设] |
| T31 | `disabled` | 撤权、取消选用、注销 | `disabled` | 不变（无在途请求） | [新设] |
| T32 | `registered` | 撤权、取消选用、注销 | `registered` | 不变 | [新设] |
| T33 | 连接阶段 | 目标为 `loading` 的递增事件（T18、T26，以及目标为 `loading` 的 T35b；不含窗口内第 3 次）持久化失败或未确认；或"首次可服务"未确认持久化（S1.12） | `unavailable`（`local_storage`），`persist_pending` 为真 | 新值不生效 | [新设] |
| T34 | `persist_pending` 为真 | 后台重试，该 binding 未确认的 generation 与放开类变化全部确认持久化（S1.10） | 清除 `persist_pending`；只有 `unavailable`（`local_storage`）回到 `loading`，其他状态与 `reason` 不变 | 写入的值生效 | [新设] |
| T35a | `loading` / `unavailable` | 撤权信号（S3.1；T05 的例外除外），包括 `unavailable` 期间已接受响应带回的撤权信号 | `unavailable`（`access_revoked`）；丢弃内存凭据 | +1（按 S4.5 作废在途请求） | [D2+新设] |
| T35b | `loading` / `unavailable` | 范围变窄（S3.1，含服务端收窄 S5.10） | 进入 `loading` 的新一轮（续签变窄用新凭据，服务端收窄重新取凭据）；窗口内第 3 次改为 `unavailable`（`access_revoked`）；`unavailable` 的 `reason` 为 `credential_rejected`、`access_revoked`、`incompatible`、`instance_unidentified` 时只作废与占位，`reason` 不变，不重新取凭据 | +1（按 S4.5 作废在途请求） | [D2+新设] |
| T36 | `registered` 或 `disabled`，带待确认标志，原因 `principal_changed`（S1.13）（所属 profile 不带 `unsaved_settings`；带标志时只能经 T44） | 用户确认 | 存在长期登录态而其 principal 与 binding 不同时，必须重新登录并得到与 binding 相同的 principal，确认才能落盘；得到别的 principal 按 T23 处理，这次确认丢弃；确认持久化后（S1.13）按 S1.2 派生（`disabled` 时仍为 `disabled`） | 不变 | [新设] |
| T37 | `disabled` | 登录或选用该 profile | `disabled`，记下登录与选用；Off 与停用都清除后按 T13、T16 激活；所属 profile 带 `unsaved_settings` 时只记下选用，登录按 S1.13 引导进 T44，不建立 binding | 不变 | [新设] |
| T38 | `unavailable`（`credential_rejected` / `access_revoked`） | 重新登录 | 同一 principal：`loading`（`revoked_since_ready` 不清除）；不同 principal：T23 | 同一 principal：不变；不同 principal：同 T23（旧 binding +1，新 binding 按 S4.4） | [新设] |
| T39 | `persist_pending` 为真 | 又一个需要递增的事件 | 按该事件派生 | 待写值在已有待写值上 +1，写成功后一次生效（S4.4） | [新设] |
| T40 | 被选中的未绑定占位，记有 `dependency`，Off 与停用标志都为假，且所属 profile 不带 `unsaved_settings` | 退避到时，或 sidecar 进入 `starting` | 重新握手或取凭据；成功按 S2.5 建立 binding，按 S1.2 派生；失败保持并记下 `reason` | 无 | [新设] |
| T41 | 被选中的未绑定占位，记有 `instance_unidentified` 或 `credential_rejected`，Off 与停用标志都为假，且所属 profile 不带 `unsaved_settings` | 手动重试、sidecar 重新握手，或重新登录 | 同 T40 | 无 | [新设] |
| T42 | 连接阶段（`unavailable` 只限 `dependency`、`not_ready`），且不在 T42 开启、尚未进入可服务状态的那一轮 `loading` 中 | 用当前凭据发出的请求收到凭据无效类响应（S3.1） | `loading` 的新一轮：丢弃短期凭据、续签一次；续签也失败按 T05（`credential_rejected`）并占位；窗口内第 3 次直接按续签也失败处理 | 不变；不作废在途请求；第一次不占位 | [新设] |
| T43 | 未绑定占位（所属 profile 不带 `unsaved_settings`；带标志时只能经 T44） | 登录得到与该 profile 上次已确认的不同的 principal（或上次已确认的不可读，S1.8） | 新 binding 建立，带待确认标志，`registered`（Off 或停用时 `disabled`） | 新 binding 按 S4.4 | [新设] |
| T44 | 带 `unsaved_settings` 的 profile（S1.13），宿主级确认已持久化 | 用户确认该 profile，并重新登录或重新握手 | 解除 `unsaved_settings` 与建立新的 binding（新 `binding_id`、初值）是同一次原子持久化，按 S1.2 派生；principal 与上次已确认的不同时，新 binding 带 `principal_changed`（同 T43 的效果），`unsaved_settings` 照常解除 | 新 binding 按 S4.4 | [新设] |

T31、T32 里的注销仍按 #1013② 清除长期登录状态；`disabled`、`registered` 下的取消选用只改变哪些 profile 被选用。目标状态不是 `loading` 的递增事件（T11、T14、T19、T20、T23–T25、T35a，不自动重试 `reason` 下的 T35b，以及窗口内第 3 次撤权类事件）持久化失败时，状态照常派生，同时 `persist_pending` 为真（S1.7）；只有目标为 `loading` 的递增事件走 T33。

## 4. generation

- **S4.1** [R7.2+新设] 每个 binding 一个 generation：正整数，只增不减，不复用。唯一来源是 Provider（KB-C1-04）；Electron 主进程、桥与页面只读取（#1013②）。
- **S4.2** [KB-C1-04+新设] **+1 的事件，且只有这些**：Off（T11）、停用 profile（T14）、撤权（T18）、取消选用（T19）、注销（T20）、换实例的旧 binding（T21）、principal 变化、改端点或租户、移除 profile 时处于连接阶段的旧 binding（T23–T25；含 T38 换 principal 那一支）、Provider 启动（T26）、`loading` 或 `unavailable` 中的撤权类事件（T35a、T35b）。
- **S4.3** [新设] **不递增**：降级、不可用与恢复（T03–T10）；S3.1 例外中的签发被拒（T05）；凭据无效类响应与它触发的续签（T42）；拿不到实例标识（T22）；未绑定占位的重试（T40、T41）；同一实例重启（S2.4）；续签范围相同或更宽（T27）；单个请求的错误与 partial（T28）；M1 开关（T29）；`registered` 或 `disabled` 上的 Off、停用、叠加、移除、principal 变化、改端点与登录（T12、T15、T17、T23–T25、T31、T32、T37：没有在途请求）；On、启用 profile、用户确认与同一 principal 重新登录（T13、T16、T36，T38 同一 principal 那一支：不论派生到哪个状态都不递增，因为进入 `disabled` 或 `registered` 时已经递增或当时没有在途请求；T38 换 principal 那一支按 T23，旧 binding +1，见 S4.2）。取消单个 run（T30）也不递增：它只中止这个 run 的请求，不是 binding 级的作废。理由：generation 作废的是授权与信任上下文；服务暂时不可用不改变已发请求的授权，结果仍然可用。
- **S4.4** [新设] **持久**：generation 按 binding 持久保存（S2.6）。新 binding 的初值大于同一 profile 下用过或回报过的任何值（含 `retired` 的 binding 与内存中的待写值），首个为 1。**每次递增都先持久化、确认后才生效**；Provider 启动时把每个非 `retired` 的 binding 递增，持久化确认后才准入，新值满足 I4（参考做法见附录 A.4）。持久化有时限，超时按结果未知处理，确认前不生效。任何一次持久化失败（初值、递增、启动）都置 `persist_pending`（S1.7）：不准入任何请求，后台重试，确认后清除（T34）。`persist_pending` 期间又发生需要递增的事件时，待写值在已有待写值上再 +1（如 N+1 → N+2），确认后一次生效（T39）。持久状态损坏后新建的 binding，初值大于损坏前可能用过的任何值（S1.14）；验收时以测试构造的损坏前最大值判定（ST-26e）。理由：先持久化后生效，崩溃后才不会与崩溃前用过的值撞号；审计（R10.1）与将来的服务端校验（R7.5、READONLY §12 #6）需要跨重启单调。
- **S4.5** [R7.4+新设] **作废流程**：① 在同一个临界区内关闭准入，并把全部在途请求标记为"已作废"；准入检查与读取 generation 在同一把锁下，因此不会有请求拿着旧值在关闭之后发出；② 持久化新值，确认后作为当前值，失败或超时按 S4.4；③ 中止全部在途请求（R7.1）；④ 按 §5 清缓存（含合并缓冲区）；⑤ ① 之后到达的响应都按 S4.6 丢弃（已标记作废，不论 ② 是否成功）。① 完成即视为事件生效；②–④ 出错不得重新开放准入。下一个事件在 ①–④ 完成后才处理（S3.2）。本地 `cancelled` 的 `current_generation` 取本次递增的新值；② 失败时取待写值（尚未生效）：在途请求已经相对它作废，回报旧值会让调用方以为旧值仍然有效。所有 S5.6 触发事件都在 ① 的同一临界区内记下占位规则（收紧类，S1.10），先于新 generation 持久化。
- **S4.6** [R7.2/R7.3/R7.5+新设] **接受与消费**：每个请求携带准入时的 generation。Provider 只接受同时满足三条的响应：`request_id` 在途；未被标记作废；generation 等于当前值（R7.3）。**`null` 回显**：READONLY R6.1 允许回显为 `null` 的错误信封（如解析请求体之前就失败的 `unauthorized`）不按回显配对，按发起它的那次 HTTP 交换配对（R7.3）：该交换仍在途、未被标记作废、准入时的 generation 等于当前值，就照常接受（撤权信号、凭据无效类响应照常驱动 T18、T35a、T42 等）；否则丢弃，给本地 `cancelled`（回显该交换的 `request_id` 与准入时的 generation，S6.1）。非 `null` 的回显值不符，照旧拒绝。判定、把请求标记为已完成（移出在途集合）、交给 Tool 层（或写缓存、派生状态事件）三件事在同一个临界区内完成。由响应派生的状态事件（如探测结果驱动 T03）与缓存写入都携带 generation；应用时 generation 已过期，或 binding 的状态已不是转移表要求的前置状态（例如 T35a 之后迟到的探测结果不能再驱动 T03），或探测不属于当前 loading 轮次（S1.9），就丢弃，不写入。例外：由已接受响应派生的撤权事件（S3.1）不受前置状态检查约束，必须处理。**追加前复查**：Tool 结果被接受之后、追加进模型上下文之前再检查一次，复查与追加在同一个临界区内完成（宿主组装上下文时持有该 binding 的准入锁，跨 binding 时按 `binding_id` 顺序取锁，KB-HOST-01）。丢弃的条件只有两类：generation 已过期；或该 binding 准入已关闭，即 S4.5 ① 已开始、`persist_pending` 为真，或状态为 `disabled`、`registered`、`retired`。`loading`、`degraded`、`unavailable` 不算准入关闭（与 ST-08a 一致：服务不可用不作废已接受的结果）。被丢弃时，Off 换成本地 `knowledge_off`，其他情况换成本地 `cancelled`。Off 之后因此不会再有新证据进入上下文。被中止的请求（S4.5、T30）立即移出在途集合，之后到达的响应因此被丢弃。服务端只回显（R7.5）。
- **S4.7** [KB-C1-04+新设] **run 绑定 binding，不绑定 generation**（§10 #7 已定）。run 开始时持久化一条 run 绑定（放开类，KB-HOST-01 为 RunCreate 补字段）：当时激活的 binding 集合，以及当时每个已选用 profile 的配置所指向的候选身份元组（profile × principal × 端点/租户，local-managed 为实例标识；Off 时也记）。之后每次调用用这些 binding 各自当时的 generation。集合中的 binding 未激活或已 `retired` 时，对它本地拒绝，`reason` 按 §6 的顺序；**不改用其他 binding**（不读全局活动 profile，不跟随改过的端点，也不把 run 开始后才激活的 binding 加进来）。因此重新选用原 profile、同一 principal 重新登录、Off 后再 On 时，旧 run 会继续调用。**空集合补记** [新设]（David 第 7 轮后定；身份键经 Grok、Opus 盲选收紧为完整元组）：集合为空时，在第一次 dispatch 时存在与某个候选元组完全一致的激活 binding 的调用时补记，只补记一次，补记（放开类）持久化确认后生效；本地拒绝的调用不占用补记；开始时没有候选元组，或当前配置与记下的候选元组不同，这个 run 不补记。run 绑定或补记持久化失败时，只有这个 run 的 Knowledge 调用得本地 `local_storage`，不置 binding 的 `persist_pending`（S1.7）。
- **S4.8** [R3.7+新设] **调用级作废**：一次 Tool 调用可能派生多个子请求（R3.7(b) 按组并行、R3.7(a) 重试补齐），它们都绑定这次调用准入时的 generation。任一子请求被作废（S4.5 ①）时，整次调用返回本地 `cancelled`，已接受的部分结果全部丢弃，不作为 partial 交出。重试补齐只在 generation 未变、调用未被作废、binding 处于可服务状态且使用当前凭据时发出。合并缓冲区算作缓存，在 ④ 清除。按 `next_cursor` 翻页是新的一次调用，不受此条约束，按自己准入时的 generation 判定。一次调用涉及多个 binding 时，按 binding 分成独立的部分（S4.9）：某个 binding 的子请求被作废，只让这一部分变为本地 `cancelled`（带该 binding 的值），其他 binding 的部分照常交付；Off 作废全部 binding，各部分按 §6 中已发出调用的规则：在途的得 `cancelled`，已接受未追加的得 `knowledge_off`。
- **S4.9** [新设] **多个 binding 同时激活**（§10 #5 已定）：不同 profile 的 binding 可以同时激活，每个 profile 至多一个。每个 binding 独立维护状态、generation、loading 轮次、`revoked_since_ready`、撤权窗口计数、证据纪元与占位规则、`persist_pending`；一个 binding 的事件不改变其他 binding 的 generation，也不作废它们的在途请求。Off 是宿主级（S5.2），一次作用于全部 binding，每个处于连接阶段的 binding 各按 T11 递增；停用、移除 profile 只作用于该 profile 的 binding。**跨 binding 的结果**：不同 binding 来自不同服务端，KB、文档与分块标识可能重复，`score` 与 `rank` 也不可比较（R3.6、R3.7）。Provider 不跨 binding 合并排名：每个 binding 内按 R3.7 合并，按 binding 分别交给 Tool 层（各自的结果、partial 或本地结果），由 KB-C1-05 呈现；证据与引用都带 `binding_id`，去重、`score_space` 分组与缓存都以 binding 为界。

## 5. 事件处理细则

| 事件 | 在途请求 | 迟到响应 | 缓存 | 后续注入 | 新调用 |
|---|---|---|---|---|---|
| Off | ① 标记作废，③ 中止（R7.1）；每个调用得本地 `cancelled`，带 `current_generation`（READONLY R6 表 `cancelled` 行、§11.4），派生多个子请求的调用整体 `cancelled`（S4.8） | 丢弃（S4.6） | 全部清除，含内存凭据与合并缓冲区 | 不再产生新 Tool 结果与自动注入；历史中的已有证据不处理（S5.6） | `knowledge_off`，模型仍调用工具时也如此（S1.5） |
| 停用、移除 profile | 同 Off | 同 Off | 同 Off | 同取消选用 | `profile_inactive` |
| 撤权类事件（S3.1） | 同 Off | 同 Off | 全部清除；取得新凭据后重建 | 回到可服务前不产生；之后只用新 generation 的结果；历史证据按 S5.6 | `loading` 期间 `not_ready`；`access_revoked` 时按 §6 |
| 凭据无效类响应（T42，S3.1） | 不作废、不中止，各自完成；用已丢弃的旧凭据发出的请求收到凭据无效类响应，只得本地 `not_ready` | 不适用：未作废，到达的响应按 S4.6 接受 | 丢弃当前短期凭据，续签一次 | 第一次不占位；续签也失败（含窗口内第 3 次）时按 S5.6 占位 | 停止准入，`loading` 期间 `not_ready`；续签也失败后 `credential_rejected` |
| 取消选用、注销 | 同 Off | 同 Off | 全部清除（含 `score_space` 分组）；注销另销毁短期凭据，长期登录态按 #1013② 清除（收紧类，S1.10） | 旧 binding 不再产生；绑定它的 run 不改道（S4.7）；历史证据按 S5.6 | `profile_inactive` |
| 换实例、principal 变化、改端点或租户 | 同 Off | 同 Off | 旧 binding 全部清除；新 binding 重新做能力发现；旧登录态不复用（S2.7） | 旧 binding 终结，绑定它的 run 失去 Knowledge；历史证据按 S5.6 | 旧 binding `profile_inactive` |
| 服务不可用、降级 | 不主动中止，各自按 deadline 结束（R3.3、R4.5），结果与错误照常交 Tool 层 | 不适用：未作废，到达的响应按 S4.6 接受 | 只清就绪结果；能力声明在回到 `loading` 时重新判定兼容（R9.4） | 不受影响 | 按 S1.4；`degraded` 只准入为 `ready` 的操作 |
| 宿主发起的删除 KB（S5.10） | 不作废、不中止 | 不适用：未作废，按 S4.6 接受；墓碑存在期间响应里出现该 KB 不把它加回可见集合，省略它也不算收窄 | 记下墓碑时把该 KB 移出最近一次可见集合；删除失败撤回墓碑 | 删除成功后按 S5.6 只占位该 KB；重启时墓碑仍未决的，按 S1.14 暂扣该 KB 的证据并重查 | 不改变状态，照常准入；不递增 |
| Provider 启动 | 随上个进程结束，不存在 | 不存在（连接已断） | 不跨进程保存；凭据按 #1013② 从系统凭据存储重新取得（不自动重试的 `reason` 与 S1.14 全局收紧后的 profile 除外） | 按派生状态；单纯重启不把历史证据换成占位（S5.6；S1.14 全局收紧除外） | 按派生状态 |

- **S5.1** [KB-C1-04+新设] **Off 持久**：Off 与 On 持久保存，重启后不变（T26）。Off 是收紧类：先在内存生效（S4.5 第 ① 步），同时持久化；持久化失败时保持内存中的 Off、后台重试，并在界面提示；期间重启按 S1.14 以 Off 恢复并等用户确认（I2、I7，§10 #20 已定）。On 是放开类：持久化确认后才生效，存在未确认的收紧时也不生效（I1），否则保持 `disabled`。profile 停用标志同理。持久状态损坏时按 S1.8。
- **S5.2** [新设]（§10 #1、#2 已定）：Off 是宿主级开关，作用于全部 binding，选用或取消选用 profile 都不改变 Off；只关某个库用停用 profile（S5.4）；新安装默认 Off，用户显式打开后才进入 `loading`。
- **S5.3** [新设]（§10 #3 已定）：Off 不删除任何数据：不删服务端 KB 与文档，不删本地数据目录，不清长期登录状态，不停止 local-managed 进程（S2.3）；只丢弃内存里的短期凭据与缓存。Off 持久化失败后重启进入全局收紧（S1.14）时，长期登录态作废、不复用，但不清除，这是全局收紧的效果，不是 Off 本身的效果。
- **S5.4** [R1.4+新设] local-managed 没有注销事件（principal 固定，R1.4）。停用路径有两条：Off（S5.1–S5.3）；停用或移除 profile（T14、T25、S2.3）。怀疑凭据泄露时轮换本实例秘密，按撤权处理（T18、T35a）。停用不删数据。移除 profile 时询问用户是否一并删除该 profile 的本地数据目录（§10 #19 已定）：选择删除时，在 sidecar 按 KB-LITE-04② 停止之后删除；服务端的 KB 与文档一律不动；不选择则保留。历史中的证据按 S5.6 换成占位，不因删除与否而改变。
- **S5.5** [R7.7] 已经交付给模型或用户的内容追不回；任何事件都不得声称撤回，界面与审计按 R7.7。
- **S5.6** [新设]（§10 #4 已定）：**占位水位**。每条交付的证据记录 `binding_id`、所属 KB 与该请求准入时该 binding 的证据纪元；证据纪元是每个 binding 一个持久单调计数，不同于 generation。下列触发事件在任何状态下发生（包括 `disabled`、`registered`），都把该 binding 的纪元 +1 并记下一条适用全部 KB 的占位规则：撤权类事件（含服务端收窄，S5.10）、凭据无效类响应的续签也失败（S3.1）、取消选用、注销、停用或移除 profile、换实例、principal 变化、改端点或租户。组装发给模型的历史时，证据属于 `retired` 的 binding，或纪元小于某条规则的纪元且 KB 在该规则范围内，就换成不含正文的占位；用户可见的历史如何显示由 KB-LITE-04① 定。宿主自己发起的删除 KB 成功后，同样把纪元 +1，记一条只适用被删除 KB 的规则（S5.10）。`binding_id` 在重启后的持久状态里不存在的证据，一律换成占位（I5）。**暂扣**：待复验未解除时（S3.1），该 binding、待复验范围内 KB 的证据（包括登记之后追加的）在组装时同样不发正文，但不改纪元，解除后恢复；binding 终结时转为占位。Off、On 与 Provider 启动不是触发事件，不改纪元，不产生占位（S1.14 的全局收紧除外）。规则与纪元是收紧类变化（S1.10）：先在内存生效，同时持久化；持久化失败不阻断准入，组装历史时照样执行内存里的规则；重启时按 I5、S1.14。
- **S5.7** [R7.3/R10.3] 本地 `cancelled` 只交 Tool 层，不进上下文、不进缓存、不作为证据。宿主日志记 `request_id`、旧与新 generation、事件类型，不记 query、正文与凭据。
- **S5.8** [KB-C1-04] 任何状态都不得引入证据缓存：同一查询再次调用必须重新发请求。
- **S5.9** [#1013②+新设] 桥只接受同时满足以下条件的消息：generation 等于当前值；该 binding 准入开放（`persist_pending` 为假）；处于可服务状态。S4.5 第 ① 步到第 ② 步完成之间拒绝所有消息。细则与用例在 ②；KnowledgeView 页面会话的销毁按 KB-LITE-04①、KB-C2-03。
- **S5.10** [R7.6+新设] **服务端收窄**（§10 #21 已定：按撤权处理）。凭据没变，但服务端收窄了授权 A。Provider 按 binding 维护"最近一次可见"的 KB 集合：被接受的 search、read 成功响应的 `effective_scope`，以及就绪探测列出的 KB，都加入集合；判定只看成功响应的 `effective_scope`、带 scope 的就绪探测，以及 `forbidden/empty_scope`，其他 `forbidden`（`policy_denied`、`operation_not_allowed`）不参与；某个 KB 在请求范围 Q 里，响应的 `effective_scope` 却不含它（`forbidden/empty_scope` 视为 Q 里的全部 KB 都不在），而它在集合里，就视为被移出；带 scope 的就绪探测（R8.4）同样按它省略或列出的 KB 更新并比较；不拿不同 Q 的结果互相比较；只拿用当前凭据发出的请求做比较（凭据归属同 S3.1），用已丢弃的旧凭据发出的请求，其响应不判定移出（结果仍按 S4.6 接受或丢弃）。判定移出时，在同一临界区把这些 KB 移出集合，并按范围变窄处理（S3.1）：`ready`、`degraded` 走 T18，`loading`、`unavailable` 走 T35b（不自动重试的 `reason` 下不重新取凭据）；作废在途请求、递增 generation、按 S5.6 占位、计入窗口上限。它由已接受响应派生，不受 S4.6 前置状态检查约束。触发收窄的那个响应经追加前复查换成本地 `cancelled`（S4.6），之后的新调用照常把服务端的 `forbidden` 交给 Tool 层。**持久** [新设]（§10 #21）：集合按 binding 持久保存，增补也要持久化；判定时始终与该 binding 交付过、且尚未被占位规则覆盖的证据所属的 KB 取并集。**删除** [新设]（David 第 7 轮后定）：宿主自己发起删除 KB（经 #1013③ 的接口或宿主界面）之前，先记下该 KB 的删除墓碑（收紧类），立即把它移出集合；墓碑存在期间，响应里出现该 KB 不把它加回集合，省略它也不算收窄。删除成功后按 S5.6 把纪元 +1，记一条只适用该 KB 的占位规则；删除失败时撤回墓碑（放开类）。重启时墓碑没有成功或撤回结果的，按 S1.14 暂扣并重查。这一过程不递增 generation、不作废在途请求、不计入窗口上限。别处的删除与收窄不可区分（R2.5、R8.4），仍按收窄处理。局限：之后既不查询、也不探测的 KB 无法发现被收窄；别处删除可能被当成收窄并计入窗口（§10 #21）。

## 6. Provider 本地结果

按 binding 各自判定（S4.9）。**新调用**按下表自上而下取第一个成立的情形。**已发出或已接受的调用**不查这张表：在途请求或调用被作废（S4.5、S4.8）一律得 `cancelled`（retryable false，带 `current_generation`，READONLY R6 表 `cancelled` 行、§11.4；持久化失败时为待写值，S4.5）；已接受、未追加的结果被追加前复查丢弃时，Off 得 `knowledge_off`，其他得 `cancelled`（S4.6）；用旧凭据发出的请求收到凭据无效类响应，或旧凭据撤权信号复验通过时，得 `not_ready`（retryable true，回显原请求的 `request_id` 与准入时的 `generation`，S3.1）。

| 情形 | code | reason | retryable |
|---|---|---|---|
| Off（含重启时的全局收紧与持久状态损坏，S1.14） | `unavailable` | `knowledge_off` | false |
| `persist_pending` 为真（S1.7）；因未确认的收紧、或正在持久化的放开而被拒的放开（I1）；run 绑定未持久化的 run（S4.7） | `unavailable` | `local_storage` | false |
| 仅 profile 停用、`retired`（含 S1.14 全局收紧中退役的 binding），或 run 绑定的 binding 已不在激活状态 | `unavailable` | `profile_inactive` | false |
| 未绑定占位记有失败 `reason`（S2.5） | `unavailable` | 同记录的 `reason` | `dependency` 为 true（所属 profile 带 `unsaved_settings` 时为 false），其余 false |
| 其余 `registered`（含待确认标志、未记失败的占位） | `unavailable` | `profile_inactive` | false |
| `loading`；`degraded` 下未 `ready` 的操作；`unavailable`（`not_ready`）；待复验未解除（S3.1） | `unavailable` | `not_ready` | true |
| `unavailable`（`dependency`） | `unavailable` | `dependency` | true |
| `unavailable`（`incompatible`，R9.4） | `unavailable` | `incompatible` | false |
| `unavailable`（`credential_rejected`、`access_revoked`、`instance_unidentified`） | `unavailable` | 同状态的 `reason` | false |

- **S6.1** [R6.1+新设] 本地结果使用 READONLY R6.1 的信封：`request_id` 是 Provider 为本次调用生成的值，`generation` 是该 binding 的当前值（未绑定占位为 `null`）。**例外**：`cancelled` 回显被丢弃请求的 `request_id` 与它准入时的 `generation`，新值放在 `current_generation`（READONLY §11.4、#1012 的 local-cancelled 夹具）；追加前复查（S4.6）换成的 `cancelled`、§6 中旧凭据请求得到的 `not_ready` 同样回显原请求的值。本地结果不发网络请求，不上线，#1012 的线上 schema 不收录。本地 `reason` 只挂在 READONLY 闭集里的 `unavailable`，或已标"不上线"的 `cancelled` 上：**不新增 code，也不用 `unauthorized`**（它会触发续签）。本文新增的本地 `reason`：`knowledge_off`、`profile_inactive`、`incompatible`（对应 R9.4）、`credential_rejected`、`access_revoked`、`instance_unidentified`、`local_storage`。`retryable: false` 的本地结果不得自动重试（R6.6）。

## 7. 与 M1 记忆开关的独立性

- **S7.1** [KB-C1-04] Knowledge Off 与 M1 个人记忆开关互相独立：互不读取、互不改写，也不触发对方的转移或 generation（T29）。Knowledge Off 不得复用 M1 的 `memory_settings` 命名空间或键（§9 第 1 条）。
- **S7.2** [KB-C1-04] 四种组合都合法：记忆开、知识关时，记忆召回与写入照常，Knowledge 本地拒绝；记忆关、知识开时，Knowledge 照常，记忆不召回、不写入；两开、两关各自按自己的开关。
- **S7.3** [新设] 两个开关语义对齐、实现不共享：都持久；默认值各自定（M1 默认开，Knowledge 见 S5.2）；关闭都不删除已有数据；读不出时 Knowledge 按关（S1.8）；M1 写入门遇到损坏内容按开，召回路径报错、不召回。M1 没有 generation，关闭记忆不中止 Knowledge 的在途请求。
- **S7.4** Knowledge 证据能否被 M1 写入（retain）属于出口问题，由 KB-SEC-01b 定，不在本文。

## 8. 验收：测试用例清单

前置里的"在途 n"指该 binding 有 n 个在途请求；除注明外 profile 为 remote。每个转移至少一例；KB-C1-04、KB-HOST-01、KB-LITE-04 按本表写测试。"随 #7"表示期望值来自 §10 #7 的决定。

| ID | 转移 | 前置 | 事件 | 期望后置 | 期望副作用 |
|---|---|---|---|---|---|
| ST-01a | T01 | On | 登记 remote profile | 未绑定占位，`registered` | 没有 generation；新调用得 `profile_inactive`；不发任何请求 |
| ST-01b | T01 | Off | 登记 profile | 未绑定占位，`disabled` | 新调用得 `knowledge_off` |
| ST-02a | T02、S2.5 | 未绑定占位 | 首次登录 | `loading` | 建立 binding，写初值 1 成功后才发能力发现与就绪探测 |
| ST-02b | T02、S1.7 | 未绑定占位 | 首次登录，新建记录持久化失败 | `unavailable`（`local_storage`） | `persist_pending`；不发任何请求 |
| ST-03 | T03 | `loading`，`revoked_since_ready` 为真 | 兼容，search、read 都 `ready` | `ready` | search、read 都准入；`revoked_since_ready` 清除 |
| ST-04 | T04 | `loading` | 只有 search `ready` | `degraded` | read 得本地 `not_ready`，不发网络请求 |
| ST-05a | T05 | `loading` | 能力声明缺 `agent24_knowledge` | `unavailable`（`incompatible`） | 不自动重试；不回退上游接口（R9.4） |
| ST-05b | T05 | `loading` | 就绪探测超时 | `unavailable`（`dependency`） | 按退避进入 ST-10a |
| ST-05c | T05 | `loading` | 首次激活时签发方拒绝签发 | `unavailable`（`credential_rejected`） | 不自动重试；本地结果不用 `unauthorized` |
| ST-05d | T05 | `loading` | `live: true`，search、read 都 `not_ready`，ingest `ready` | `unavailable`（`not_ready`） | ingest 不参与判定 |
| ST-05e | T05 | `loading`，凭据刚签发，`revoked_since_ready` 为假，从未可服务 | 探测得 `unauthorized`/`credential_revoked` | `unavailable`（`credential_rejected`） | generation 不变；不占位；不自动重试 |
| ST-05g | S3.1、S1.12 | 重启后 `loading`，该 binding 曾可服务；`revoked_since_ready` 随重启丢失 | 新签发的凭据被以 `unauthorized`/`credential_revoked` 拒绝 | `unavailable`（`access_revoked`） | 走 T35a 而不是 T05：+1，写占位 |
| ST-05h | T42 | `loading`，本轮新签发的凭据（非 T42 续签得来），曾可服务 | 探测得 `credential_invalid` | `loading` 的新一轮 | 不递增；续签一次；续签后仍得 `credential_invalid` 按 ST-18i |
| ST-05f | T18→T35a | `ready` | 撤权后重签成功，探测仍得 `unauthorized` | `unavailable`（`access_revoked`） | T18、T35a 各 +1，各写一次占位 |
| ST-06 | T06 | `ready` | 探测：read `not_ready` | `degraded` | generation 不变 |
| ST-07 | T07 | `degraded` | 探测：都 `ready` | `ready` | generation 不变 |
| ST-08a | T08 | `ready`，在途 1 | 探测不可达 | `unavailable`（`dependency`） | 在途不中止，deadline 内到达的响应被接受；generation 不变 |
| ST-08b | T08 | `ready` | 凭据过期，续签时签发方不可达 | `unavailable`（`dependency`） | generation 不变；不算撤权 |
| ST-09 | T09 | `degraded` | 探测 `live: false` | `unavailable`（`dependency`） | 新调用 fail fast，不发网络请求 |
| ST-10a | T10 | `unavailable`（`dependency`） | 退避到时 | `loading` | 重新探测 |
| ST-10b | T10 | `unavailable`（`incompatible`） | 退避到时；再手动重试 | 先不变，手动后 `loading` | 手动前不发任何请求 |
| ST-10c | T10、S2.4 | local-managed，`unavailable`（`dependency`） | sidecar 进入 `starting`，握手得到相同实例标识 | `loading` | 同一 binding；generation 不变 |
| ST-11a | T11 | `ready`，在途 2 | Off | `disabled` | +1；两个在途被中止，各得本地 `cancelled`（`current_generation` 为新值）；凭据与缓存清空；Off 已持久 |
| ST-11c | T11、S4.5 | `ready`，在途 1；持久值 N | Off，持久化失败 | `disabled`，`persist_pending` | 在途得 `cancelled`，`current_generation` 为待写值 N+1，N+1 尚未生效；新调用得 `knowledge_off`（§6 中 Off 先于 `local_storage`） |
| ST-11b | T11 | `loading`，探测在途 | Off | `disabled` | +1；探测的迟到结果不改状态、不写缓存 |
| ST-12 | T12 | `registered` | Off | `disabled` | generation 不变 |
| ST-13a | T13 | `disabled`（Off），已选中 | On | `loading` | On 先持久再生效 |
| ST-13b | T11、S1.7 | `ready` | Off，generation 持久化失败；随后 On | `unavailable`（`local_storage`） | 不重新准入；后台重试成功后才进入 `loading` |
| ST-14a | T14 | local-managed，`ready`，在途 1 | 停用 profile | `disabled` | +1；在途被中止；凭据丢弃；之后按 KB-LITE-04② 停 sidecar；数据不删 |
| ST-14b | T14 | `degraded` | 停用 profile | `disabled` | +1 |
| ST-15 | T15 | `registered` | 停用 profile | `disabled` | generation 不变 |
| ST-16 | T16 | `disabled`（仅停用），已选中 | 启用 profile | `loading` | 启用先持久再生效 |
| ST-17a | T17 | `disabled`（仅停用） | Off；再启用 profile | 两步后都是 `disabled` | 不递增；本地结果为 `knowledge_off` |
| ST-17b | T17 | `disabled`（仅 Off） | 停用 profile；再 On | 两步后都是 `disabled` | 不递增；On 之后本地结果为 `profile_inactive` |
| ST-18a | T18 | `ready`，在途 2 | 一个请求返回 `unauthorized`/`credential_revoked` | `loading` | +1；另一个在途被中止；缓存清空；重新取凭据 |
| ST-18b | T18 | `degraded`，在途 1 | 续签：同一 principal，范围变窄 | `loading` | +1；在途被中止 |
| ST-18c | T18→T35a | `ready` | 撤权，签发方拒绝重签 | `unavailable`（`access_revoked`） | 不走 T05；T18、T35a 各 +1，各写一次占位；不自动重试 |
| ST-18d | T18 | local-managed，`ready` | 轮换本实例秘密 | `loading` | +1 |
| ST-18e | T42 | `ready`，在途 1 | 响应为 `unauthorized`，`reason` 为 `credential_invalid`、`credential_missing` 或未知值 | `loading` | 不递增；在途不中止；不占位；不置 `revoked_since_ready`；丢弃短期凭据，续签一次 |
| ST-18i | T42→T05 | 接 ST-18e | 续签被拒；或续签后的凭据仍得 `credential_invalid` | `unavailable`（`credential_rejected`） | 不递增；全部历史证据换成占位；不声称已撤权；不自动重试 |
| ST-18j | T42→T35a | 接 ST-18e | 签发方以 `credential_revoked` 拒绝续签 | `unavailable`（`access_revoked`） | 按撤权 +1，写占位 |
| ST-18k | S3.1 | `ready`，用凭据 c1 在途 2 | 第 1 个得 `credential_invalid`，T42 续签得到 c2；第 2 个（c1 发出）随后也得 `credential_invalid` | `loading`（不变） | 第 2 个只得本地 `not_ready`，不触发 T05 |
| ST-18l | S3.1 | `ready` | 10 分钟内第 3 次凭据无效类事件 | `unavailable`（`credential_rejected`） | 不再续签；写占位；不递增 |
| ST-18m | T42 | 接 ST-18e，续签成功、回到 `ready`；30 分钟后 | 又收到 `credential_invalid` | `loading`（T42） | 不按"续签也失败"；计入 10 分钟窗口 |
| ST-18n | S3.1 | `ready`，c1 上在途 search；T42 续签得 c2 后回到 `ready` | c1 的 search 带回 `credential_revoked` | 状态不变；记下待复验 | 不准入新调用；Q 内证据暂扣；用 c2 复验：通过则解除记录写入后恢复，旧结果得 `not_ready`；被拒按 T18 +1，暂扣变占位 |
| ST-18o | S3.1 | 接 ST-18n，复验在途 | Off | `disabled` | 待复验保留；Q 内证据仍暂扣；On 后取得凭据时重做复验 |
| ST-18q | S3.1、S1.14 | 接 ST-18n，复验在途 | 进程崩溃后重启（正常恢复） | 连接阶段，待复验保留 | 新调用得 `not_ready`；Q 内证据仍暂扣；取得凭据后重做复验 |
| ST-18r | S3.1 | 接 ST-18n，按 Q={kb-a}（版本 1）复验在途 | 另一个旧凭据请求带回撤权信号，Q 并为 {kb-a, kb-b}（版本 2）；随后版本 1 的复验通过 | 待复验保留 | 不解除；按版本 2 的 Q 再复验 |
| ST-18p | S3.1 | 接 ST-18n | 复验连续 3 次超时；之后 binding 因改端点 `retired` | 先保持待复验、停止自动重试；`retired` 后 | 暂扣的证据换成占位 |
| ST-18f | T18 | `ready` | `credential_expired`，续签被拒 | `loading` | 按撤权 +1 |
| ST-18g | T18 | `ready` | 10 分钟内第 3 次撤权类事件（T18、T35a、T35b 都计入） | `unavailable`（`access_revoked`） | +1；写占位；不再自动重签 |
| ST-18h | T18 | `ready` | 续签：同一 principal，新增 kb-c、移除 kb-b | `loading` | 按变窄处理：+1；作废在途；写占位 |
| ST-19a | T19 | A、B 都 `ready`；run R 开始时两者都激活，R 对 A 在途 1 | 取消选用 A | A `registered`，B 不变 | A +1，B 不变；R 对 A 的在途被中止，迟到响应被丢弃；A 的缓存（含 `score_space`）清空；R 之后对 A 得 `profile_inactive`，对 B 照常 |
| ST-19b | T02 | 接 ST-19a | 重新选用 A | A `loading`→`ready` | R 继续调用 A，使用新 generation（随 #7） |
| ST-19c | T19、S1.7 | A `ready` | 取消选用，generation 持久化失败 | A `registered`，`persist_pending` | 绑定 A 的 run 的新调用得 `local_storage`（§6 中 `persist_pending` 先于 `profile_inactive`）；再激活 A 得 `unavailable`（`local_storage`），不准入 |
| ST-19d | T19 | A `unavailable`（`dependency`），B `ready` | 取消选用 A | A `registered` | A +1；B 不受影响 |
| ST-20a | T20 | `ready`；run R，在途 1 | 注销 | `registered` | +1；迟到响应被丢弃；缓存清空；凭据销毁 |
| ST-20b | T02 | 接 ST-20a | 同一 principal 重新登录 | 同一 binding `loading` | R 继续调用（随 #7） |
| ST-20c | T20 | `loading` | 注销 | `registered` | +1 |
| ST-21 | T21 | local-managed，`loading`，就绪探测在途 | 握手得到不同实例标识 | 旧 binding `retired`；新 binding `loading` | 旧 +1，探测的迟到响应被丢弃；缓存清空；新初值大于旧值；绑定旧 binding 的 run 得 `profile_inactive` |
| ST-22a | T22 | local-managed 已有 binding，`loading` | 握手拿不到实例标识（只有 HTTP 自报值） | `unavailable`（`instance_unidentified`） | 不新建 binding、不递增；该 reason 按收紧类持久化，重启后保持；不自动重试 |
| ST-22b | T22 | local-managed 未绑定占位 | 首次握手拿不到实例标识 | 占位 `registered` | 本地结果 `instance_unidentified`；没有 generation |
| ST-23a | T23 | `degraded`；run R | 续签得到另一 principal | 旧 `retired`；新 binding `registered` | 旧 +1，在途被中止；R 得 `profile_inactive`；新 binding 不复用旧登录态，等用户确认 |
| ST-23b | T23 | `registered`（已注销） | 用另一 principal 登录 | 旧 `retired`；新 binding `registered` | 不递增；等用户确认 |
| ST-23d | S1.13 | 接 ST-23a，用户尚未确认 | Provider 重启 | 新 binding `registered`（待确认） | 按待确认（原因 `principal_changed`）恢复；不自动取凭据，不发请求 |
| ST-23c | T23 | `disabled`（Off） | principal 变化 | 旧 `retired`；新 binding `disabled` | 不递增 |
| ST-24a | T24 | `degraded`；run R 绑定旧端点 | 修改 profile 端点 | 旧 `retired`；profile 回到未绑定占位 `registered` | 旧 +1；R 之后的调用得 `profile_inactive`，不发往新端点；不复用旧登录态，需要重新登录 |
| ST-24d | T24、S2.5 | 接 ST-24a | 重新登录 | 新 binding `loading` | 新的 `binding_id`；初值大于旧值；写初值后才发探测 |
| ST-24e | T43、S1.13 | 接 ST-24a | 用另一个 principal 登录 | 新 binding `registered`（待确认） | 初值与待确认标志同一次持久化；用户确认后才 `loading` |
| ST-24f | S1.13、T43 | 接 ST-24e，用户未确认 V | 再改端点（T24），然后以 V 登录 | 新 binding `registered`（待确认） | 上次已确认的 principal 仍是原来的，V 未确认过，不走 T02 |
| ST-24b | T24 | `registered` | 修改租户 | 旧 `retired`；未绑定占位 `registered` | 不递增 |
| ST-24c | T24、S2.6 | 端点 X 的 binding b1 | 改为 Y 并登录（b2），再改回 X 并登录 | 第三个 binding b3 | b3 是新的 `binding_id`，不复活 b1；初值大于 b1、b2 用过的所有值 |
| ST-25a | T25 | local-managed，`loading` | 移除 profile | `retired` | 先按 T14 作废并停 sidecar（+1）；数据不删 |
| ST-25b | T25 | `registered` 或 `disabled` | 移除 profile | `retired` | 不递增；数据不删 |
| ST-26a | T26 | 持久状态为 Off；generation 7 | Provider 启动（正常恢复） | `disabled` | 新值大于 7，持久化确认后才处理请求；不发任何网络请求 |
| ST-26b | T26 | On，已激活；generation 3 | Provider 启动（正常恢复） | `loading` | generation 大于 3；不新增占位；不提示 |
| ST-26c | T26 | On，已激活 | Provider 启动，持久化失败 | `unavailable`（`local_storage`） | 不准入任何请求 |
| ST-26d | T26 | `registered`；generation 5 | Provider 启动（正常恢复） | `registered` | generation 大于 5；不发请求 |
| ST-26e | T26、S1.8 | 持久状态损坏；测试构造损坏前用过的最大 generation M（如 1000） | Provider 启动 | 全部 binding `retired`，profile 回到未绑定占位，`disabled` | 全部历史证据换成占位；提示用户；不发请求；用户确认并重新登录后，新 binding 的初值大于 M |
| ST-26f | T26、S1.14 | On，已激活；三个 binding 分别为 `unavailable`（`access_revoked`）、`unavailable`（`credential_rejected`）、`unavailable`（`incompatible`），均已持久化 | Provider 启动（正常恢复） | 各自保持原 `reason` 的 `unavailable` | generation 递增；不从系统凭据存储自动取凭据，不发任何请求；新调用得同一 `reason`；只在 T10 的手动重试、配置变更（前两者另可经 T38 重新登录）后再试 |
| ST-26g | T26、S1.14 | 占位 P 记有 `credential_rejected`；binding A 在 10 分钟窗口内已有 2 次撤权类事件；binding B 的待复验已连续 2 次超时；以上均已持久化 | Provider 启动（正常恢复），仍在 A 的 10 分钟窗口内；之后 A 回到 `ready` 后再遇撤权类事件，B 取得凭据后的复验再超时一次 | P 的新调用仍得 `credential_rejected`；A 进入 `unavailable`（`access_revoked`）；B 停止自动复验 | 计数与占位 `reason` 跨重启保持，不因重启清零；P 不自动重试 |
| ST-27a | T27 | `ready`，在途 1 | 续签：同一 principal，范围不变 | `ready` | 不递增；在途不中止 |
| ST-27b | T27 | `ready`，在途 1 | 续签：同一 principal，范围变宽 | `ready` | 不递增；在途不中止 |
| ST-28a | T28 | `ready` | 单个请求得 `forbidden/policy_denied`；或 `forbidden/empty_scope`，但 Q 中的 KB 都不在最近一次可见集合 | `ready` | 不递增；不算撤权 |
| ST-28b | S5.10→T18 | `ready`，在途 1；kb-a、kb-b 都在最近一次可见集合，历史含两者的证据 | Q = {kb-a, kb-b}，`effective_scope` 只含 kb-a | `loading` | 按撤权：+1；在途被中止；kb-a、kb-b 的旧证据都换成占位；kb-b 移出集合；计入窗口上限 |
| ST-28c | S5.10 | `ready`；kb-a、kb-b 都在集合里 | 先以 Q = {kb-a} 得到 kb-a；再以 Q = {kb-b} 得 `forbidden/empty_scope` | 第一步后 `ready`，第二步后 `loading` | 第一步不算收窄（不比较不同 Q）；第二步 kb-b 被移出，按 T18 +1；这次调用经追加前复查得本地 `cancelled`，之后以 Q = {kb-b} 的新调用得 `forbidden` |
| ST-28d | S5.10→T35b | `unavailable`（`dependency`），read 在途 1；kb-b 在集合里 | 带 scope `{kb-b}` 的就绪探测省略 kb-b | `loading`（新一轮，重新取凭据） | 不因前置状态被丢弃；+1；read 被作废；写占位 |
| ST-28f | S5.10 | `ready`；kb-a 在集合里 | Q 含 kb-a 的 search 得 `forbidden/operation_not_allowed`（或 `policy_denied`） | `ready` | 不算收窄；不递增 |
| ST-28g | S5.10 | `ready`，Q 含 kb-b 的 search 在途；kb-b 在集合里，历史含 kb-a、kb-b 的证据 | 宿主删除 kb-b：先记下墓碑，删除成功；随后在途 search 的响应仍含 kb-b | `ready` | 不递增；在途不作废；kb-b 不被加回集合；纪元 +1，只 kb-b 的旧证据换成占位；不计入窗口 |
| ST-28h | S5.10 | 接 ST-28g 的墓碑阶段 | 删除失败 | `ready` | 撤回墓碑；kb-b 可重新加入集合；不占位 |
| ST-28e | S5.10 | 交付过 kb-b 的证据且未被占位覆盖；重启后 `ready` | Q = {kb-b}，得 `forbidden/empty_scope` | `loading` | kb-b 按未占位证据算在集合里，按收窄处理：+1，写占位 |
| ST-29 | T29 | `ready` | 关闭 M1 记忆 | `ready` | 不递增；Knowledge 调用照常 |
| ST-30 | T30 | `ready`；run 1、run 2 各在途 1 | 取消 run 1 | `ready` | run 1 的请求立即移出在途集合并得 `cancelled`，之后到达的响应被丢弃；run 2 照常；不递增 |
| ST-31 | T31 | `disabled` | 撤权；取消选用；注销 | `disabled` | 都不递增；注销清除长期登录；On 后按 S1.2 派生 |
| ST-32 | T32 | `registered` | 撤权；注销 | `registered` | 不发请求；不递增 |
| ST-33 | T33 | `ready`，在途 1 | 撤权，持久化失败 | `unavailable`（`local_storage`） | 在途仍被中止；不准入 |
| ST-33b | T35a、T34 | `unavailable`（`dependency`） | 撤权信号，generation 持久化失败；之后后台持久化成功 | 先 `unavailable`（`access_revoked`）并 `persist_pending`；T34 后仍为 `access_revoked` | 持久化失败期间本地结果为 `local_storage`；T34 不带回 `loading` |
| ST-33c | T18、T34 | `ready` | 窗口内第 3 次撤权，generation 持久化失败；之后持久化成功 | 同 ST-33b | 同 ST-33b |
| ST-34 | T34 | 接 ST-33 | 后台持久化成功 | `loading` | 清除 `persist_pending`；用持久化的值发探测 |
| ST-35a | T35a | `loading`，探测在途 | 签发权威通知撤销 | `unavailable`（`access_revoked`） | +1；探测被作废；写占位；内存凭据丢弃；不自动重试 |
| ST-35b | S2.2 | local-managed，`ready` | sidecar 变为 `degraded` | `unavailable`（`dependency`） | 不递增 |
| ST-35c | T35a、S4.6 | `unavailable`（`dependency`），read 在途 2 | 一个 read 的响应带回 `unauthorized`/`credential_revoked` | `unavailable`（`access_revoked`） | 撤权事件不因前置状态被丢弃；+1；另一个 read 被作废；写占位 |
| ST-35d | T10→T35a | `unavailable`（`dependency`），沿用旧凭据 | 回到 `loading`，探测得 `credential_revoked` | `unavailable`（`access_revoked`） | 按撤权 +1，不走 T05 |
| ST-35f | S3.1 | `unavailable`（`dependency`），曾可服务，沿用旧凭据经 T10 回到 `loading` | 探测得 `credential_expired`，续签成功（同一 principal、范围不变） | `loading` | 不递增；续签被拒时按 T35a（从未可服务时按 T05） |
| ST-35g | T35b | `unavailable`（`credential_rejected`），read 在途 1 | read 的响应显示 kb-b 被移出 | `unavailable`（`credential_rejected`） | +1；read 被作废；写占位；不重新取凭据 |
| ST-35e | T35b | `unavailable`（`dependency`），read 在途 1 | 续签：同一 principal，范围变窄 | `loading` | +1；read 被作废；写占位；不进入 `access_revoked` |
| ST-36 | T36 | 接 ST-23a | 用户确认新 principal | 新 binding `loading` | 不递增；确认持久化后才生效 |
| ST-37 | T37 | `disabled`（Off） | 登录并选用该 profile；再 On | 先 `disabled`，On 后 `loading` | 不递增 |
| ST-38 | T38 | `unavailable`（`credential_rejected`） | 同一 principal 重新登录 | `loading` | 不递增 |
| ST-38b | T38 | `unavailable`（`access_revoked`） | 同一 principal 重新登录 | `loading` | 不递增；`revoked_since_ready` 仍为真，新凭据再被拒走 T35a |
| ST-38c | T38→T23 | `unavailable`（`credential_rejected`），generation N | 用另一 principal 重新登录 | 旧 `retired`；新 binding `registered` | 旧 binding +1（N+1，`unavailable` 属连接阶段，同 T23）；新 binding 初值按 S4.4 大于 N+1；新 binding 不带 `revoked_since_ready`，等用户确认 |
| ST-39 | T39 | `persist_pending`，待写 N+1 | Off | `disabled` | 待写值变为 N+2；写成功后 N+2 一次生效，N+1 从未生效 |
| ST-40 | T40 | local-managed 未绑定占位，被选中，sidecar 未启动 | 首次激活；之后 sidecar 进入 `starting` | 先占位 `registered`（`dependency`），重试成功后 `loading` | 建立 binding，写初值后才发探测 |
| ST-40b | T40、S2.5 | 同上，但 Off | sidecar 进入 `starting`；退避到时 | 占位 `disabled` | 不重试，不发任何请求 |
| ST-41 | S1.9 | `loading` 第 1 轮，探测在途 | 经 T05、T10 进入第 2 轮后，第 1 轮的探测结果到达 | 第 2 轮的 `loading` | 旧轮结果被丢弃，不能驱动 T03 |
| ST-42 | S1.10、S1.13 | `ready` | principal 变化：旧 binding 的 `retired` 已持久化，新 binding（初值、待确认标志）持久化失败 | 旧 binding `retired`；新 binding `persist_pending`、待确认 | 新 binding 不准入；新 binding 持久化之前用户确认被拒并提示；持久化之后用户再确认，按 T36 |
| ST-43 | T41 | local-managed 未绑定占位，记有 `instance_unidentified` | sidecar 重新握手，得到实例标识 | `loading` | 建立 binding；写初值后才发探测 |
| ST-44 | S4.8、S4.9 | A、B 都 `ready`；一次 search 同时发往 A、B | B 的响应为 `unauthorized`/`credential_revoked` | A `ready`，B `loading` | B +1，A 不变；A 的部分照常交付，B 的部分为本地 `cancelled`（带 B 的值）；结果按 binding 分别交出，不跨 binding 排名 |
| ST-45 | T11、S4.9 | A、B 都 `ready`，各在途 1 | Off | 都 `disabled` | 各 +1；两部分都在途，都得 `cancelled` |
| ST-47 | S1.12、T33 | 新 binding `loading`，从未可服务 | 兼容且 search、read 都 `ready`，首次可服务记录持久化失败 | `unavailable`（`local_storage`），`persist_pending` | 不准入；T34 后回到 `loading` |
| ST-48a | T44 | 全局收紧后，P 带 `unsaved_settings`，宿主级确认已持久化，On | 用户确认 P 并以同一 principal 重新登录 | 新 binding `loading` | 新的 `binding_id`；初值满足 I4；解除 `unsaved_settings`；绑定旧 binding 的 run 不改道 |
| ST-48b | T44 | 同 ST-48a，但仍 Off | 用户确认 P 并重新登录 | 新 binding `disabled` | 解除 `unsaved_settings`；On 后进入 `loading` |
| ST-48c | T44 | 同 ST-48a | 用另一个 principal 重新登录 | 新 binding `registered`（`principal_changed`） | `unsaved_settings` 已解除；等用户确认 |
| ST-48d | S1.14、I1 | 全局收紧后，宿主级确认尚未给出 | 用户点 On | 仍 `disabled` | On 被拒，本地结果 `knowledge_off`，界面提示先确认 |
| ST-48e | S1.13、S2.5 | 全局收紧后 P 只有未绑定占位，带 `unsaved_settings` | 用户以上次已确认的 principal 登录；或 sidecar 自动重新握手 | 不经 T02 | 登录被引导进 T44 的确认，用户确认后才建立 binding；自动握手（T40、T41）暂停 |
| ST-48f | S1.14、I1 | 全局收紧后，宿主级确认已给出、持久化尚未确认 | 用户点 On | 仍 `disabled` | On 被拒，本地结果 `local_storage` |
| ST-48g | T13、T37、S1.13 | 全局收紧后 P 的占位为 `disabled`，带 `unsaved_settings`，宿主级确认已持久化 | 用户点 On；或选用 P；或登录（含另一 principal） | On 后 P 的占位为 `registered`（带 `unsaved_settings`） | On 照常生效；选用只记下；登录引导进 T44；不进入 `loading`、不建立 binding |
| ST-49 | I1 | Off；On 已提交、尚未确认 | 用户点 Off，之后 On 的确认才返回 | `disabled` | 晚到的确认被忽略；准入保持关闭 |
| ST-49b | I1 | Off 已生效、持久化尚未确认（宿主级收紧）；P 未选用，Q 的新 binding 待确认（`principal_changed`） | 用户选用 P；用户确认 Q 的新 binding（T36） | P 仍未选用；Q 的新 binding 仍待确认 | 两项放开都被拒并直接丢弃，不排队，发起方得 `local_storage`；Off 确认持久化后界面可提示重试 |
| ST-49c | I1、S2.5 | P 的 binding b1 因改端点（T24）退役，退役尚未确认持久化；P 回到未绑定占位 | 用户以 P 登录；或 b1 注销（T20）未确认时同一 principal 重新登录 | 不建立新 binding（或不重新激活） | 放开被拒并丢弃，得 `local_storage`；b1 的收紧确认持久化后重试才按 S2.5、T02 建立或激活 |
| ST-49d | I1、S4.7 | A、B 都激活；A 的撤权占位规则尚未确认持久化，B 没有未确认的收紧 | 新 run R 开始，激活集合为 {A, B} | A、B 状态不变 | R 的 run 绑定涉及 A，被拒；R 对 A、B 的 Knowledge 调用都得 `local_storage`；不只绑定 B |
| ST-36b | T36 | 接 ST-23a，长期登录态的 principal 仍是 U | 用户确认 | 先要求重新登录 | 得到 V：确认落盘，按 S1.2 派生；得到 U：按 T23，这次确认丢弃；不用 U 的登录态为 V 取凭据 |

| ID | 规则 | 场景与期望 |
|---|---|---|
| EV-01 | S4.6、S5.7 | Off、取消选用、注销、换实例四种场景下，① 之后到达的响应都被丢弃：不进上下文、不进缓存、不作为证据；Tool 得 `cancelled`（READONLY §11.4） |
| EV-02 | S4.5 | Off 与新调用高并发交错：没有任何请求以旧 generation 在 ① 之后发出 |
| EV-03 | S4.5、S4.6 | 在 ① 与 ② 之间（持久化期间）到达的响应被丢弃，持久化成功或失败都一样 |
| EV-04 | S3.2、S4.5 | ① 之后紧接着 On：On 在 ①–④ 完成后才处理；之后的请求只带新 generation；持久化失败时不准入 |
| EV-05 | S5.1 | Off 后重启：`disabled`，不发任何网络请求（含就绪探测） |
| EV-06 | S5.1、S1.14 | Off 持久化失败：内存保持 Off，后台重试并提示；此期间重启按全局收紧恢复（Off、全部历史证据占位、binding 退役、各 profile 待确认 `unsaved_settings` 且需重新登录、提示，§10 #20）；On 未确认持久化：保持 `disabled` |
| EV-07 | S5.3 | Off→On 后同一 KB 仍可检索；没有调用任何删除接口；local-managed 进程未被停止 |
| EV-08 | S5.6 | 撤权（含服务端收窄）、取消选用、注销、停用 profile、换实例、principal 变化、改端点或租户之后，此前获取的证据在后续模型调用中只剩占位、不含正文；这些事件发生在 `disabled` 或 `registered` 状态下（不递增）时同样占位；Off、On 与单纯重启后历史不变 |
| EV-09 | S5.8 | 相同查询连续两次调用都发出网络请求 |
| EV-10 | §5 | 取消选用、注销、换实例后，能力声明、就绪结果、`score_space` 分组、合并缓冲区、内存凭据都已清空 |
| EV-11 | S7.1、S7.2 | 四种组合各自生效；切换 Knowledge Off 不改 `memory_settings` 行，切换 M1 不改 Knowledge 的持久设置 |
| EV-12 | S4.1、S4.4 | 任意事件序列后 generation 不减；在"持久化之后、生效之前"注入崩溃，重启后的值大于崩溃前用过的任何值 |
| EV-13 | S5.7 | 本地 `cancelled` 的宿主日志含 `request_id`、新旧 generation、事件类型，不含 query、正文、凭据 |
| EV-14 | S6.1 | `retryable: false` 的本地结果不被 Tool 层自动重试；本地结果从不使用 `unauthorized` |
| EV-15 | S1.5 | Off 时工具仍在广告里，描述标明当前不可用及原因；模型调用它：dispatch 得本地 `knowledge_off`，不发网络请求；On 后描述恢复 |
| EV-16 | S4.8 | 一次 search 按两组并行（R3.7(b)）：第 1 组响应已接受，第 2 组在途时 Off；整次调用返回 `cancelled`，不交付任何证据，也不作为 partial；之后不发重试补齐；翻页调用另按自己的 generation 判定 |
| EV-17 | S4.6 | ④ 清缓存之后，迟到的探测结果不得把能力声明、就绪结果写回缓存，也不得驱动 T03 |
| EV-18 | S3.2、S4.4 | 持久化卡住超过时限：按失败处理，进入 `persist_pending`；排在后面的事件照常处理，不被无限阻塞 |
| EV-19 | S5.9 | ① 与 ② 之间桥拒绝所有消息；② 之后只接受当前 generation、准入开放且可服务时的消息 |
| EV-20 | S1.10、S1.14、S5.6 | 占位规则持久化失败期间，组装历史仍执行内存里的规则；持久化前进程重启，按全局收紧把全部历史证据换成占位，旧证据不会恢复重发 |
| EV-21 | S4.6 | 一次 search 的结果已被接受，在追加进模型上下文之前发生 Off：结果被丢弃，换成本地 `knowledge_off`；Off 之后上下文里不出现新证据 |
| EV-22 | S4.6 | binding 为 `unavailable`（`dependency`）时，已接受的结果通过追加前复查照常进入上下文；`disabled`、`registered`、`retired`、`persist_pending` 或作废进行中时被丢弃 |
| EV-23 | S6.1 | 本地 `cancelled` 的 `generation` 是被丢弃请求的值，`current_generation` 是新值，与 READONLY §11.4、#1012 的 local-cancelled 夹具一致 |
| EV-24 | S1.9 | 同一轮内后发的探测先到、先发的后到：只应用后发的结果；占位上旧令牌的登录或握手结果不能建立 binding |
| EV-25 | S5.10 | 本会话中 search 的 `effective_scope` 新出现 kb-c 并交付证据；之后同一 Q 的响应不含 kb-c：判定收窄并占位 |
| EV-26 | S4.4、S1.14 | generation 持久化失败期间回报过待写值 N+2，随后崩溃：重启后的初值大于 N+2 |
| EV-27 | S4.8 | 补齐前 binding 经 T42 进入 `loading`：不发补齐请求 |
| EV-28 | S4.7 | run 在 Off 期间开始（候选元组 P × U × 端点 X）：Off 期间的调用被本地拒绝，不占用补记；On 之后第一次调用补记 P × U × X 的 binding；若期间换成 P × V，或改为端点 Y，都不补记，得 `profile_inactive` |
| EV-33 | S4.6、S5.6 | `ready`，HTTP 交换 X1、X2 在途；X1 得到 `request_id`、`generation` 都为 `null` 的 `unauthorized`/`credential_revoked`（R6.1）：按交换配对接受，驱动 T18（+1、作废在途、按 S5.6 写占位）；X2 随后也带回 `null` 回显的 `unauthorized`/`credential_revoked`：已作废，被丢弃，得本地 `cancelled`（回显 X2 的 `request_id` 与准入时的值），不再触发撤权、不再 +1；回显为非 `null` 的错值照旧被拒 |
| EV-34 | S5.10、S3.1、T27 | `ready`；search X 用凭据 c1（范围 {kb-a}）准入，Q = {kb-a, kb-b}；续签得 c2（同一 principal，范围 {kb-a, kb-b}，T27 不递增），c2 发出的请求先把 kb-b 加进最近一次可见集合；X 随后带着 `effective_scope` = {kb-a} 到达：不判收窄，不递增、不占位、不计入窗口，X 的结果照常接受；之后用 c2 发出、同一 Q 的响应省略 kb-b 时才按收窄处理 |

**故障注入验收**（S1.10 不变式；"崩溃点"之后立即重启；收紧类用例的"写入之前"指持久化还没开始，已落盘未确认的情形见 FI-2c；注入的故障只有单次持久化失败与崩溃，不含存储整体不可写。参考设计的附加用例见附录 A.6，不属于规范）

| ID | 不变式 | 前置 | 崩溃点 | 重启后应处于的状态 |
|---|---|---|---|---|
| FI-1a | I1 | Off，用户点 On | On 写入之前 | `disabled`，不提示（正常恢复） |
| FI-1e | I1 | Off，用户点 On | On 已落盘、确认尚未返回 | On 生效，或 `disabled`；两者都合格 |
| FI-1b | I1 | On 持久化结果未知，Provider 放弃这次 On，界面显示 Off | 放弃之后 | `disabled`，与界面一致 |
| FI-1c | I1、I7 | 停用 P 已生效、持久化未确认；用户启用 P，被拒并提示 | 被拒之后 | P 不是启用：保持停用，或全局收紧；那次启用不会在重启后自动生效 |
| FI-1f | I1 | Off；binding A 的撤权占位未确认；用户 On | 被拒之后 | 不是 On（宿主级放开被任一 binding 的未确认收紧阻止） |
| FI-1g | I1 | 选用、解除待复验、撤回墓碑、首次可服务、run 绑定或补记，各自在写入之前 | 此时 | 对应放开都没有生效 |
| FI-1h | I1、I7 | Off，用户点 On，On 已提交未确认；用户再点 Off | Off 的抵消写入之后 | `disabled`；不会按挂起的 On 打开 |
| FI-1i | I1、I3、I7 | On 已落盘未确认；用户点 Off 之后 On 的确认才返回 | Off 的抵消写入之前 | 全局收紧 |
| FI-1d | I1、I8 | T23：新 binding（待确认）写入之前 | 此时 | 新 binding 不存在；不按新身份取凭据 |
| FI-2a | I2、I7 | `ready`，在途 1，历史有证据 | Off 生效后、写入之前 | 全局收紧：全部 `disabled`，历史证据占位，binding `retired`、各 profile 带 `unsaved_settings`，提示 |
| FI-2b | I2、I5 | `ready`，历史有证据 | 撤权（T18）生效后、占位规则写入之前 | 全局收紧；该 binding 的旧证据不带正文 |
| FI-2c | I2 | FI-2a、2b、6a、6b、6d、6e、7a–7d 中任一收紧已落盘、确认尚未返回 | 此时 | 这项收紧被保持（正常恢复，Off 不改历史证据），或全局收紧；两者都合格 |
| FI-3a | I3 | 持久状态中间损坏 | 启动时 | 持久状态损坏情形：全部 `retired`，profile 回到未绑定占位并带 `unsaved_settings`，所有 profile 按未选用、已停用处理，证据占位，Off，提示 |
| FI-3b | I3 | 持久状态缺失，且不能确认是首次安装 | 启动时 | 同 FI-3a |
| FI-3c | I3 | 首次安装 | 启动时 | 默认 Off，不提示，不收紧 |
| FI-3d | I3 | 接 FI-2a，用户已确认提示 | 再次正常重启 | 正常恢复，不再全局收紧 |
| FI-3e | I3 | 接 FI-3a，用户确认提示且确认已持久化 | 再次正常重启 | 正常恢复，不因同一损坏再次进入持久状态损坏情形，不再提示 |
| FI-3f | I3 | 持久状态缺失，但会话历史里有 Knowledge 证据；或存在 Knowledge profile 配置 | 启动时 | 不视为首次安装，同 FI-3a |
| FI-4a | I4 | generation 持久值 N；N+1 持久化失败，`cancelled` 回报过 N+1，T39 后回报过 N+2 | 此时 | generation 大于 N+2 |
| FI-4b | I4 | 同 FI-4a | 用户正常退出后重启 | generation 大于 N+2 |
| FI-4c | I4 | 同 FI-4a 的 `persist_pending` 期间 T23 新建 binding | 此时 | 新 binding 的初值大于回报过的 N+2 |
| FI-5a | I5 | 撤权的占位规则已持久化 | 任意之后的时刻 | 旧证据为占位 |
| FI-5b | I5 | 历史含某 binding 的证据，该 binding 不在重启后的持久状态里 | 启动时 | 这些证据为占位 |
| FI-5c | I5 | 待复验已登记并已持久化，复验在途 | 此时 | 待复验保留，Q 内证据暂扣，不准入 |
| FI-5d | I5 | 宿主删除 kb-b：墓碑已记、服务端已删除、删除成功的结果尚未处理 | 此时 | kb-b 的证据暂扣，重查删除结果 |
| FI-6a | I3、I6 | 改端点（T24），b1 的 `retired` 写入之前 | 此时 | 全局收紧（Off、证据占位、待确认 `unsaved_settings`、提示）；b1 因身份不符 `retired`；不复用 b1 的登录态，确认后也不复活 |
| FI-6b | I3、I6 | T23 换为 V；旧 binding 的 `retired` 写入之前，长期登录态已是 V | 此时 | 全局收紧；旧 binding `retired`；长期登录态作废，不自动取凭据 |
| FI-6d | I3、I6 | 移除 profile P（T25），写入之前 | 此时 | 全局收紧；P 的 binding `retired`、P 带 `unsaved_settings`；长期登录态作废；确认时需重新登录并建立新 binding |
| FI-6e | I3、I6 | local-managed 换实例（T21），旧 binding 的 `retired` 写入之前 | 此时 | 全局收紧；旧 binding `retired`；用户确认该 profile 后，重新握手得到新实例标识，经 T44 建立新 binding |
| FI-6f | I6 | profile 配置修订号已变，或 profile 已不存在；其余状态已持久化 | 正常重启 | 对应 binding `retired`；正常恢复，不提示 |
| FI-6c | I6、I7 | 已注销（T20），正常退出 | 重启 | binding `registered`，不退役；若 run 跨重启存活（KB-HOST-01），同一 principal 重新登录后它继续（随 #7） |
| FI-6g | I6 | binding 的 principal 为 U、不带 `principal_changed`，长期登录态的 principal 已是 V；其余状态已持久化。对照：已注销（没有长期登录态）的 binding | 正常重启 | 前者 `retired`，不用 V 的登录态为它取凭据；对照的 binding 不退役；正常恢复，不提示 |
| FI-7c | I7 | 注销已生效（占位已持久化），长期登录态清除写入之前 | 此时 | 全局收紧；长期登录态作废，binding 不自动激活 |
| FI-7d | I7 | local-managed P 只有未绑定占位；停用 P 生效后、写入之前 | 此时 | 全局收紧；确认 P 之前不自动握手（T40 暂停） |
| FI-7a | I7 | 停用 P 生效后、写入之前 | 此时 | 全局收紧；P 带 `unsaved_settings`，确认前不激活 |
| FI-7b | I7 | 取消选用 A：占位规则已持久化，取消选用写入之前 | 此时 | 全局收紧，A 不会按"已选用"进入 `loading` |
| FI-8a | I8 | 接 ST-23a，用户未确认 | 正常重启 | 新 binding 仍待确认 |
| FI-8b | I8 | 全局收紧后用户确认了宿主级提示，未逐个确认 profile | 再次重启 | 各 profile 仍待确认（`unsaved_settings`） |
| FI-8d | I8 | 全局收紧后，宿主级确认写入之前 | 此时 | 已写下的收紧照常恢复，不再追加全局收紧、不抬高纪元，再提示一次（前提：全局收紧的写入都已确认、之后没有未确认的收紧） |
| FI-8e | I2、I3 | 全局收紧后、宿主级确认之前，用户移除 profile P | 移除写入之前 | 再次全局收紧；P 的 binding 不复活 |
| FI-8f | I1 | 用户确认（T36）或宿主级确认已落盘、确认尚未返回 | 此时 | 按已确认生效，或仍待确认；两者都合格 |
| FI-8g | I2、I3 | 全局收紧自身的 Off 或 `unsaved_settings` 写入之前 | 此时 | 再次启动时再次全局收紧并提示 |
| FI-8h | I6、I8 | ST-36b 中，T36 要求的重新登录完成之前 | 此时 | 仍待确认（`principal_changed`）；不用旧登录态取凭据 |
| FI-8c | I8 | 用户确认（T36）写入之前 | 此时 | 仍待确认 |
| FI-8i | I8、I3 | 接 ST-23a，P 的新 binding 带 `principal_changed`、用户未确认；随后 Off 生效后、写入之前 | 此时 | 全局收紧：该 binding `retired`，待确认转到 P 的 `unsaved_settings`；只能经 T44 确认并重新登录，不经 T36 |

## 9. 现状证据（Agent24 `eb1a58b`）

1. **M1 开关**：按 owner 存在 kv 的 `memory_settings` 命名空间（`rust/crates/agent24-memory/src/lib.rs:250-258`），默认开（`:402-407`），关闭"不是清除"（`:409-422`）；召回入口的门（`rust/crates/agent24-agent/src/session_memory.rs:151-156`），写入门与写入在同一个 `BEGIN IMMEDIATE` 事务里重查开关，避免"查到开、提交前被关"的竞态（`rust/crates/agent24-memory/src/writer.rs:273-291`，迁移说明见 `rust/crates/agent24-agent/src/retain.rs:229-237`）；该处读到无法解析的内容时按"开"处理（`writer.rs:289-291` 的 `.ok()` 与 `is_none_or`），召回入口经 `memory_enabled` 的 `?` 报错（`session_memory.rs:154`），run 记 warn 后不召回（`rust/crates/agent24-agent/src/lib.rs:1431-1434`）。Knowledge 的持久状态损坏一律 fail closed（S1.8、S1.14）。REST 为 `GET/PUT /api/v1/memory/settings`（`rust/apps/agent24d/src/memory_routes.rs:232-290`，`protocol/openapi.yaml:1501`）。这种"检查与动作在同一临界区"的做法是 S4.5、S4.6 的先例。
2. **注入**：run 开始时只调用 `memory.recall`（`rust/crates/agent24-agent/src/lib.rs:1426-1443`），另有显式"记住"时检查记忆开关（`:1455`）；没有 Knowledge 或通用 context hook。v1 的 Knowledge 注入只经 Tool（D4），自动注入属 KB-CTX-02。
3. **Tool 注册与广告**：`ToolRegistry`（`rust/crates/agent24-tools/src/lib.rs:1063-1074`）在 daemon 启动时建好（`rust/apps/agent24d/src/server.rs:1544`），`RunManager` 持有不可变的 `Arc<ToolRegistry>`（`rust/crates/agent24-agent/src/lib.rs:462`）。但广告有按轮动态过滤的钩子：`Tool::advertisable()`（`agent24-tools/src/lib.rs:392-394`，默认 `true`）由 `live_adverts()`（`:1271-1287`）每轮调用，`RunManager::tool_specs_for`（`agent24-agent/src/lib.rs:2123-2127`）走这条路径，`ModuleTool` 已用它按模块就绪与同意过滤（`agent24-tools/src/lib.rs:651-660`）。所以隐藏广告不必改注册表；`advertisable()` 没有 run 参数，若要按 run 区分（例如 Off 只对部分 run 生效）需改签名，属内核接口，要知会 jason。审批门只在 `requires_approval` 时调用（`:1382`），Read 级 Knowledge 工具不过门，所以拒绝必须在 Provider 每次 dispatch 时做（S1.5）。
4. **run 绑定**：`RunCreate` 只有 `session_id`、`workspace_id`、`prompt`、`model_override`、`mode`（`rust/crates/agent24-protocol/src/types.rs:443-457`），没有 profile 与 principal；模型调用的 task profile 从 `TaskProfile::default()` 起算（`rust/crates/agent24-agent/src/lib.rs:1726`）。S4.7 需要 KB-HOST-01 补字段。
5. **取消**：`cancel_run` 按 run 取消（`rust/crates/agent24-agent/src/lib.rs:900`），没有按 binding 作废的机制；T30 沿用它，S4.5 是新增。
6. **generation 先例**：`SidecarManager` 用自增 token 丢弃过期回调（`apps/desktop/src/main/sidecar-manager.ts:156`、`:177`、`:214`），状态集见 `apps/desktop/src/main/sidecar-contract.ts:56`；模块 attach 的 generation 只增、重启即新一代（`rust/crates/agent24-os-proto/src/attach.rs:34-38`、`:79-91`；`rust/crates/agent24-os-proto/src/drain.rs:1-20`）。本文的 generation 是另一个计数器，不复用它们。
7. 仓库里还没有 Knowledge Provider、Knowledge profile 存储或 Off 开关；`rust/crates/agent24-memory/src/knowledge.rs` 的 `KnowledgeBase` 是 MD-7 指令层，与本文无关。

## 10. 待决与需要评审确认的点

第一片不单独合入，也不单独关闭任务①；三片作为一组合入。

| # | 事项 | 结论（未定时为 v1 默认） | 链接 |
|---|---|---|---|
| 1 | **Off 的粒度**：宿主级、按 profile，还是按 binding（产品范围） | 宿主级，作用于全部 binding（S5.2） | 已定（David） |
| 2 | **新安装时 Knowledge 默认开或关**（产品行为） | 默认 Off，显式打开（S5.2） | 已定（David） |
| 3 | **Off 是否删除数据、清登录、停止 local-managed 进程**（对外行为） | 都不做，只丢内存凭据与缓存（S5.3、S2.3）；Off 持久化失败后重启进入全局收紧时，长期登录态作废、不复用，但不清除（S1.14） | 已定（David）；KB-LITE-04② |
| 4 | **作废事件后历史中的旧证据是否随后续模型调用重发**；Off 之前已进入上下文的证据怎么处理（对外行为）。Off 之后新到的结果已由 S4.6 的追加前复查挡住 | 按占位水位不重发（S5.6）；Off、On 与单纯重启不处理已有证据 | 已定（David）；KB-LITE-04①、#997 |
| 5 | **同一时刻可否激活多个 binding**（产品范围） | 可以：不同 profile 的 binding 同时激活，每个 profile 至多一个；结果按 binding 分别交出（S4.9） | 已定（David；原默认"至多一个"） |
| 6 | generation 持久保存、启动 +1、先写盘后生效、写盘失败 fail closed：**盲选一致**。之后评审增补 [新设]：以 `binding_id` 为键、写盘超时视为失败、`persist_pending` 期间累加（第 2 轮）；只有阻断准入的记录置 `persist_pending`、读不出时跳跃取初值（第 3 轮）；第 10 轮起正文只冻结不变式 I4，做法见附录 A（S4.4、S1.7、S1.10、S2.6） | 按本文 | 盲选部分已定；增补部分本 PR 评审 |
| 7 | **run 绑定 binding 还是 generation；Off 再 On 后，Off 之前开始的 run 能否继续用 Knowledge**（原 #16 并入）。盲选时 Opus 主张绑定 binding，Sonnet 主张绑定 generation | 绑定 run 开始时激活的 binding 集合（S4.7）；Off 再 On、重新选用、同一 principal 重新登录之后旧 run 可以继续；开始时集合为空的 run，在第一次有符合条件 binding 的调用时补记一次：本地拒绝的调用不占用补记，只补记与 run 开始时记下的候选身份元组（profile × principal × 端点/租户或实例标识）完全一致的 binding（David 第 7 轮后定，第 8 轮盲选收紧为完整元组）[新设] | 已定（David） |
| 8 | 服务不可用、降级不递增 generation，在途请求不主动中止（S4.3、§5） | 按本文 | 已定（盲选一致） |
| 9 | 撤权判定（S3.1）。**盲选一致**：`credential_revoked` 算；过期先续签；范围不变或变宽不递增；变窄递增并作废在途；换 principal 终结旧 binding。之后评审增补 [新设]：`credential_missing` 与未知 `reason` 按 `credential_invalid`、`forbidden` 不算（第 1 轮）；`loading` 中本轮新签发凭据被拒且之前无撤权走 T05（第 2 轮）；任何连接阶段的撤权都递增并占位、窗口上限（第 3 轮，#25）；`revoked_since_ready`、续签变窄统一回到 `loading`、增删并存按变窄（第 4 轮）。**`credential_invalid` 已定（David）**：不算撤权，`credential_missing` 与未知 `reason` 同样处理；立即停止准入、续签一次（T42）；续签也失败时进 `unavailable`（`credential_rejected`）并占位、不宣称撤权（第 6 轮后 David 定）。增补 [新设]：例外还要求 binding 从未可服务（S1.12，第 5 轮）；凭据归属与 10 分钟 3 次上限（第 6 轮）；"续签也失败"限定在 T42 开启的那一轮；旧凭据带回的撤权信号先用当前凭据复验，未完成时保留持久的待复验记录、不准入、暂扣证据（David 第 7 轮后定，第 8 轮盲选） | 按本文 | 已定（David）；增补部分本 PR 评审；KB-C1-03 |
| 10 | 本地 `reason` 只挂在 `unavailable` 或 `cancelled` 上，不新增 code，不用 `unauthorized`，各自写明 retryable：**盲选一致**。之后评审增补的 reason [新设]：`credential_rejected`、`instance_unidentified`、`local_storage`（第 1 轮）（S6.1） | 按本文；READONLY 下一修订只加引用 | 盲选部分已定；与 READONLY 下一修订（r8）协调 |
| 11 | 状态只看 search、read；`live: false` 或两者都不 `ready` 为 `unavailable`；ingest 不参与（S1.3） | 按本文 | 已定（盲选一致） |
| 12 | local-managed 没有注销；停用走 Off 或停用、移除 profile；疑似泄露时轮换秘密按撤权（S5.4、S2.3） | 按本文 | 已定（盲选一致） |
| 13 | 实例标识纳入 binding、只取自受控握手、换实例终结旧 binding：**盲选一致**。之后评审修订 [新设]：拿不到标识为 `instance_unidentified`、不建 binding（第 1、2 轮）；换实例的发现前提（第 3 轮）（S2.4、T21、T22） | 按本文 | 盲选部分已定；KB-LITE-01a、KB-LITE-02 |
| 14 | **Off 时是否从模型广告中隐藏 Knowledge 工具**（产品选择）。dispatch 时本地拒绝 `knowledge_off` 是硬性要求（S1.5） | 不隐藏：仍在广告里，描述标明不可用及原因，按状态每轮生成（S1.5，§9 第 3 条） | 已定（David）；KB-C1-05 |
| 15 | 所有标 `[新设]` 的规则 | 按本文 | 本 PR 评审 |
| 16 | （已并入 #7） | — | — |
| 17 | **principal 变化时静默切换还是要用户显式确认**（与 #9 相关） | 旧 binding 终结，新 binding 带持久的待确认标志，等用户确认才激活（T23、S1.13、T36） | 已定（David）；#992 |
| 18 | **Off 时模型是否感知 Knowledge 的存在、是否提示用户打开**（与 #14 相关） | 随 #14：保留广告并标明不可用及原因；调用得 `knowledge_off`；不主动提示 | 已定（David） |
| 19 | **停用、移除 profile 或重装后，数据与引用怎么保留**（与 #12、#13 相关） | 停用不删；移除时询问用户是否删除该 profile 的本地数据目录，服务端数据不动；历史引用按 S5.6 占位（S5.4） | 已定（David）；#997 |
| 20 | **Off 持久化失败期间重启，Knowledge 会按旧的持久值 On 恢复**（S5.1） | 不能确定收紧都已持久化时全局收紧：Off、全部历史证据占位、待确认、提示；补充部分：binding 退役、profile 标记 `unsaved_settings`、长期登录态作废不复用（不销毁，与 #22 一致）、逐个确认需重新登录或握手并建立新 binding（T44）；身份或 principal 不符直接 `retired`（S1.14，不变式 I2、I3、I7；参考做法见附录 A） | 已定（David）；补充部分：已定（Grok/Opus 交叉确认），第 11 轮后 David 定为不销毁 |
| 21 | **授权 A 在服务端被收窄（`forbidden/empty_scope`，或 `effective_scope` 不含最近一次可见的 KB），但凭据没有变**（R7.6）。局限：之后既不查询、也不探测的 KB 无法发现被收窄；别处删除 KB 与收窄不可区分（R2.5、R8.4），也按收窄处理，可能计入窗口上限 | 按撤权处理（范围变窄，T18、T35b）：递增、作废在途、全部 KB 占位、计入窗口；最近一次可见集合持久化，并与未占位证据所属 KB 取并集；宿主自己发起的删除先写墓碑、成功后只占位该 KB，不走撤权（S5.10，David 第 7 轮后定） | 已定（David） |
| 22 | **`retired` binding 的长期登录态如何销毁**：本文只要求不得用于其他 binding（S2.7） | 不复用；销毁方式待 ② 定义 | 与 #1013② 的接口 |
| 23 | **READONLY 术语表的 generation 定义**（"profile × principal"、"任何作废都 +1"）需同步为"按 binding 维护，取消单个 run（T30）不递增" | 以本文为准 | READONLY 下一修订 |
| 24 | **不阻断准入的持久记录（占位规则、Off 与停用标志、移除 profile 与改端点租户 T24 的 `retired` 标记）写成功前进程重启会丢失**（S1.10，与 #20 同类）。T24 的 `retired` 标记丢失后，重启时旧 binding 会复活，有复用旧登录态的风险 | 收紧类内存先生效、同时持久化，后台重试；重启语义按 S1.14 与不变式 I2、I3（同 #20） | 已定（David） |
| 25 | **撤权循环上限**：同一 binding 10 分钟内第 3 次撤权类事件（T18、T35a、T35b 都计入）时不再自动重签 | 3 次 / 10 分钟 [新设]；服务端收窄也计入 | 已定（David） |
| 26 | **崩溃安全的规范范围**：正文只冻结对外行为与不变式 I1–I8（S1.10），重启后的可观察语义写在正文（S1.14）；日志、凭证、序号等机制放附录 A，不是规范 | 按本文 | 已定（David，第 9 轮后） |
| 27 | **被拒的放开与阻止范围**：被拒的放开直接丢弃、不排队；放开所依赖的对象（宿主、profile、涉及的 binding）上有未确认的收紧就阻止，binding 级按 profile 判定；依赖尚未生效的放开同样被拒（第 12 轮评审增补）（S1.10 I1） | 按本文 | 已定（第 10 轮 Grok、Opus 盲选一致） |

## 附录 A 非规范性参考设计（交 Provider 实现任务 KB-C1，以故障注入测试验证）

> **本附录不是规范，实现可以替换。** 规范只有正文：对外行为、§1 的不变式 I1–I8 与 S1.14 的可观察语义。任何满足它们、并通过 §8 FI 用例的实现都合格。本附录记录第 8、9 轮评审得出的一种做法，供 KB-C1 参考。

- **A.1 预写日志**：影响准入或证据交付的持久变化都写进同一份只追加的日志。每条记录带单调序号，整条原子写入并带校验，按序号重放；同一序号的重试是幂等的。"确认写入"指记录已 fsync。追加超时视为结果未知：先 fsync，再按序号读回；读回仍无法判定按失败处理。放弃的序号必须用"作废序号"记录或抵消记录占住，不留空位；T39 的新待写值取新序号。同一序号上既有"作废序号"记录、又有晚到落盘的原记录时，取更严格的一条：原记录是放开类则忽略它（I1，被放弃放开的晚到确认忽略），是收紧类则保留它（I2）。同一 binding 的记录按事件顺序追加，作废事件的收紧记录排在它的 generation 记录之前。
- **A.2 放开凭证**：宿主另有一个凭证文件，内容是创建时日志的最新序号。收紧时，内存生效后、追加收紧记录前，删除凭证并对所在目录 fsync；删除失败时保持收紧并拒绝一切放开（I1）。**已知局限（尚未解决）**：删除凭证排在内存生效之后，一次崩溃即可丢失收紧——凭证删除完成前崩溃（不需要任何故障），或删除失败后、收紧记录落盘前崩溃（单次故障加崩溃），重启时凭证有效、日志里没有收紧记录，会按旧状态正常恢复，过不了 FI-2a、2b、6a、6b、6d、6e、7a、7d；"拒绝一切放开"只保护仍在运行的进程，只重试删除也不够。参考设计不承诺这个窗口，留给 KB-C1 的实现解决。实现必须满足：表示"收紧可能未持久化"的持久标记早于收紧在内存生效（或者标记落盘前收紧不算生效），且单次持久化失败打不掉它（I1–I3，FI-2a 等用例）；同时 FI-1a 要求 On 写入之前崩溃按正常恢复，不能退成每次崩溃都全局收紧。`dependency` 与撤权窗口、凭据无效窗口的计数是尽力持久化（S1.10），这类记录不走删除凭证的路径，写失败也不影响凭证。凭证只有宿主级的一个：任何收紧未确认时拒绝所有放开，范围比 I1 的"binding 级按 profile 判定"更宽；这是有意偏离，只影响可用性，不影响安全。所有未确认的收紧记录确认写入、且日志之外的收紧写入（如注销时按 #1013② 清除凭据存储里的长期登录态，FI-7c）也已完成之后，才重建凭证；重建与删除在同一把宿主级锁里，重建前在锁内复查"没有未确认的收紧"，以写临时文件再 rename 落盘。放开记录只有在凭证存在时才追加并生效。启动时，凭证缺失、读不出、凭证序号大于日志最大序号，或日志里有序号大于凭证序号的收紧记录，都判为"不能确定收紧都已持久化"（S1.14 全局收紧）；凭证序号之后的放开记录属于正常情况。
- **A.3 重放**：按序号重放得出 S1.14 正常恢复列出的全部状态，包括选用与取消选用、最近一次可见集合、待确认（取同一 binding 序号最大的"标记待确认"或"确认"）、待复验及其版本号、删除墓碑与 run 绑定。尾部不完整的记录：任何恢复写入（含全局收紧与持久状态损坏情形的写入）之前，先把文件截到最后一条完整记录的边界，不论凭证是否有效，避免新记录接在残缺字节之后、下次启动误判为中间损坏（FI-3d、FI-8d）。中间损坏或序号断档按持久状态损坏处理；用户确认后，以收紧后的状态写成新日志的基线（快照写入新文件），旧日志归档，避免每次启动重复收紧。首次安装以宿主的首次安装标识判定，此时直接创建凭证。全局收紧时按 S1.14 作废（不复用）长期登录态，把各 binding 记为 `retired`、各 profile 记为 `unsaved_settings`。
- **A.4 generation**：Provider 每次启动追加运行开始记录；只有在没有未确认的 generation 记录时才追加正常关闭记录。启动时取日志中的最大值 + 1；最后一条不是正常关闭记录时取最大值 + 2^16。新 binding 的初值把内存里的待写值也算进最大值。持久状态损坏后新建的 binding 取损坏前日志里能读出的最大值 + 2^32，一个值也读不出时取 2^32。**已知局限**：+2^16 假定一次运行内未确认的增量（含 T39 的累加）少于 2^16，+2^32 假定损坏前用过的值不超过能读出的最大值 + 2^32；超出时参考设计不保证 I4，由 KB-C1 的实现给出上界。
- **A.5 抵消记录**：放弃一次放开时追加的抵消记录按类型对应：On ↔ Off，启用 ↔ 停用，选用 ↔ 取消选用，确认 ↔ 标记待确认，解除待复验 ↔ 重新登记待复验（版本号 +1），撤回墓碑 ↔ 删除墓碑，run 绑定或补记 ↔ run 解绑，新建 binding ↔ `retired`，宿主级确认 ↔ 重新标记宿主级提示待确认，离开不自动重试的 reason ↔ 重新进入该 reason，清除占位失败 `reason` ↔ 重新记下该 `reason`，复验超时计数清零 ↔ 恢复清零前的计数；首次可服务偏向更严格，不需要抵消。
- **A.6 参考设计的附加用例**（不属于规范）：停用记录写失败期间用户启用，凭证尚未重建，启用不生效并提示；停用写入、凭证重建后启用才生效，重放得到启用。generation N+1 追加超时、读回未落盘时以新序号写 N+2，正常关闭后重启初值 N+3。On 的记录追加超时：读回已落盘则生效并通知界面，未落盘而放弃时追加 Off 抵消。删除凭证失败时拒绝一切放开。凭证有效、尾部有不完整记录：截断后正常恢复。凭证已删、追加收紧记录时崩溃留下不完整记录：先截断再写全局收紧，下次启动不进入持久状态损坏。

## 变更记录
- r0（2026-10-17）：首稿。影响夹具：否（本文的本地结果不上线，#1012 不收录）。
- r1（2026-10-17）：按 B 类 #6–#14 的盲选（Opus 与 Sonnet 各自独立作答、都没看草稿；David 2026-10-17 授权的夜间规则）修订。两边一致的直接写入；分歧的 #7、#9 中的 `credential_invalid`、#14 登记待 David；新增待决 #16–#19；改正广告现状。影响夹具：否。
- r2（2026-10-17）：按第 1 轮双审互评（Opus 代 Codex、Sonnet 代 Grok）修订：binding 身份加入端点与租户、`binding_id`；终态 `retired`、阻断条件 `persist_pending`；Off 与 profile 停用分成两个标志；`instance_unidentified`、`credential_rejected`；凭据错误分类；登录映射；作废时标记在途请求、三条接受条件；Off 持久失败的处理；转移表重编号为 T01–T34；新增待决 #20。影响夹具：否。
- r3（2026-10-17）：按第 2 轮双审互评修订：未绑定占位、`binding_id` 作持久键、`retired` 登录态不复用、持久设置读不出 fail closed、T35–T39、调用级作废、接受与消费同一临界区、证据纪元水位、服务端收窄、桥的接受条件；新增待决 #21–#23。影响夹具：否。
- r4（2026-10-17）：按第 3 轮双审互评修订：任何连接阶段的撤权都递增并占位；撤权窗口上限；loading 轮次令牌；持久记录分类（S1.10）；generation 读不出时 binding 进入 `retired`；T24 回到未绑定占位；占位自动重试（T40）；调用级、接受与占位规则的若干补充；§10 按来源分开标注，新增 #24、#25。影响夹具：否。
- r5 第一片（2026-10-17）：按第 4 轮双审互评修订：新增 `revoked_since_ready`（S1.11），撤权后重签被拒一律走 T35a；T35 拆成 T35a（撤权信号）与 T35b（续签变窄，回到 `loading`），增删并存按变窄；窗口上限计入 T35；T34 只带回 `local_storage`；S1.8 扩到 `retired` 标记、证据纪元与占位规则；T40 受 Off 与停用约束、后置按 S1.2 派生，新增 T41；§1 表进入与离开补齐；§9 补 M1 召回路径的出处；§10 #4、#9、#21、#24、#25 同步。影响夹具：否。
- r5 第二片（2026-10-17）：S4.6 增加追加前复查；S4.7 措辞；S4.4 无可读值时初值 2^32；S5.10 带 scope 的就绪探测参与比较；S7.3 改述 M1；测试补 ST-18h、ST-28d、ST-33b/c、ST-35e、ST-38b/c、ST-40b、ST-43、EV-21，改 ST-03、ST-05e/f、ST-13b、ST-18c/g、ST-19c、ST-26e、ST-35a/c/d。影响夹具：否。
- r6 第一片（2026-10-17）：按第 5 轮双审互评与 David 2026-10-17 的逐条决定修订：新增曾可服务标志（S1.12），S3.1 的例外只适用从未可服务的 binding；待确认标志持久化（S1.13）；未落盘标记作启动兜底（S1.14，#20、#24）；凭据无效类响应不算撤权，新增 T42（#9）；服务端收窄按撤权处理，并入范围变窄（#21）；"切换离开"改为"取消选用"，允许多个 binding 同时激活（#5）；Off 时广告标明不可用（#14）；占位上的登录与握手带令牌、探测带序号；§9 retain.rs 行号；§10 #1–#5、#7、#9、#14、#17–#21、#24、#25 改为已定。影响夹具：否。
- r6 第二片（2026-10-17）：S4.6 追加前复查限定准入关闭的情形，与追加同一临界区；S4.7 绑定激活集合；S4.8 按 binding 分部分作废；新增 S4.9；S5.1–S5.6、S5.10 随 #1–#4、#19–#21 改写；§6 按 binding 判定，"仅停用"排在占位失败之前；S6.1 `cancelled` 回显原请求的 generation；测试补 ST-05g、ST-18i、ST-23d、ST-28e、ST-44–ST-47、EV-22–EV-24，改 ST-05e、ST-18e、ST-19a–d、ST-28b–d、ST-31、ST-36、ST-37、ST-42、EV-01、EV-06、EV-08、EV-10、EV-15、EV-20。影响夹具：否。
- r7 第一片（2026-10-17）：按第 6 轮双审互评与 David 第 6 轮后的决定修订：凭据无效类响应续签也失败时占位（#9），凭据归属、10 分钟 3 次上限；S1.4、T05、S3.1 例外只针对撤权信号；T35a 限定撤权信号；T35b 在不自动重试的 `reason` 下不重新取凭据；`credential_expired` 在 `loading`、`unavailable` 中的处理；S1.14 改为待写记录清单，启动时重放、先写盘再删条目、身份与配置不符直接 `retired`；T36 可在 `disabled` 下确认；S1.13 确认与置位的先后；占位上换 principal 带待确认；§1 表补 T42、T24；§10 #7、#9、#20 同步。影响夹具：否。
- r7 第二片（2026-10-17）：S4.4 回报过的待写值不倒退；S4.6 跨 binding 取锁顺序；S4.7 空集合的 run 首次调用时补记 [新设]；S4.8 补齐只在可服务状态、用当前凭据；Off 时多 binding 调用按 §6；S5.1 删去旧提示；S5.6 补占位触发；S5.10 成功响应加入可见集合、触发响应换成 `cancelled`；§6 拆分新调用与已发出调用；测试补 ST-05h、ST-18j–l、ST-24e、ST-35f/g、ST-46b–d、EV-25–EV-28，改 ST-05e、ST-18i、ST-28a、ST-28c、ST-45、ST-46。影响夹具：否。
- r8 第一片（2026-10-17）：按第 7 轮双审互评与 David 第 7 轮后的决定修订：S1.10 单调写入、清除撤销置位重试；S1.14 恢复时纪元取历史最大纪元 + 1、generation 条目只用于取最大值且 T34 后删除；S1.4 `local_storage` 的进入；S1.8、S2.5 与新增 T43（占位上换 principal）；S3.1 "续签也失败"限于 T42 那一轮、旧凭据撤权信号先复验、`credential_expired` 续签成功时的 principal 与范围、窗口规则优先；T33 与表后说明的 T35b；§1 表补 T42、T43；§10 #7、#9、#21 同步。影响夹具：否。
- r8 第二片（2026-10-17）：S4.4 待写值一律登记；S4.7 Off 时按 §6、空集合补记只限同一身份且本地拒绝不占用；S5.6、S5.10 宿主发起的删除只占位该 KB，收窄判定只看成功响应与 `empty_scope`；§6 旧凭据请求的 `not_ready` 回显与复验期间的新调用；测试补 ST-18m/n、ST-28f/g、ST-46e/f、EV-29、EV-30，改 ST-24e、ST-35f、EV-28。影响夹具：否。
- r9 第一片（2026-10-17）：按第 8 轮双审互评与第 8 轮后的设计决定（Grok、Opus 盲选）修订：崩溃安全改为单一预写日志（S1.10），收紧类内存立即生效并写日志、放开类日志确认后才生效、generation 先写后用；放开凭证与重放、不确定一律收紧（S1.14），取代未落盘清单与各独立持久标志；S1.7、S1.8、S1.12、S1.13 改写为日志记录；上次已确认的 principal（S2.5、T02、T43）；旧凭据撤权信号的待复验记录与暂扣（S3.1）；T10 在 `persist_pending` 下按 S1.7；T26 未正常关闭时 +2^16；§10 #7、#9、#20、#21、#24 同步。影响夹具：否。
- r9 第二片（2026-10-17）：S4.4、S4.5 改为日志；所有 S5.6 触发事件在 ① 记下占位规则；S4.7 run 绑定记录与完整身份元组的补记；S5.1、S5.6 暂扣与删除时纪元 +1；S5.10 删除墓碑、可见集合重建、引用改为 R2.5、R8.4；§6 待复验；测试补 ST-18o/p、ST-24f、ST-28h、ST-46g/h、EV-31、EV-32，改 ST-02b、ST-11c、ST-13b、ST-18n、ST-19c、ST-23d、ST-26a–e、ST-28e、ST-28g、ST-33 系列、ST-36、ST-42、ST-46–46f、ST-47、EV-03、EV-04、EV-06、EV-12、EV-18、EV-20、EV-28–EV-30。影响夹具：否。
- r10 第一片（2026-10-17）：按 David 第 9 轮后的决定（收窄契约范围）与第 9 轮双审互评修订：S1.10 改为崩溃安全不变式 I1–I8，收紧类与放开类清单补上选用与取消选用、标记待确认；S1.14 改为重启后的可观察语义（正常恢复、全局收紧、持久状态损坏、首次安装），含 principal 校验收窄、待复验与删除墓碑的恢复、不重复收紧；S1.7、S1.8、S1.12、S1.13 去掉机制描述，待确认以最近一次为准；S2.6 身份元组随 binding 持久化；S3.1 待复验带版本号；§10 #6、#10、#20、#24 同步，新增 #26。影响夹具：否。
- r10 第二片（2026-10-17）：S4.4、S4.5、S4.7 改为规范表述，run 绑定持久化失败只影响该 run；S5.1、S5.6（未知 binding_id 的证据占位、暂扣覆盖登记后证据）、S5.10（可见集合按 §10 #21 持久、删除墓碑的恢复）；§6 `local_storage` 情形；新增故障注入用例 FI-1a–FI-8c，删去依赖机制的 ST-46 系列与 EV-29–EV-32（移入附录 A.6），改 ST-18o（拆出 ST-18q）、新增 ST-18r，改 ST-26a/b/d/e。影响夹具：否。
- r10 第三片（2026-10-17）：新增附录 A 非规范性参考设计（预写日志、放开凭证、重放、generation、抵消记录与附加用例），收进第 9 轮评审对机制的修法（凭证判据与锁、序号与超时、fsync、截断、新基线、正常关闭条件）。影响夹具：否。
- r11 第一片（2026-10-18）：按第 10 轮双审互评、第 10 轮的盲选（被拒的放开丢弃、阻止范围按依赖对象）与第 10 轮后的盲选（全局收紧补全）修订：收紧类补注销、移除 profile 与各类退役，放开类补宿主级确认；I1 允许已落盘未确认的放开生效并写明阻止范围与丢弃；I2 写明保证边界与"单纯重启"；I5 补删除墓碑暂扣；I6 补全局收紧的退路；I7 补注销；待确认带原因；S1.14 先判 `retired`、principal 判定去掉豁免、保留不自动重试 reason 与各计数、全局收紧时 binding 内部退役并销毁长期登录态、T40 暂停、宿主级确认前后的表现、损坏时选用与停用按收紧、首次安装的否定判据；新增 T44；§10 #20 同步，新增 #27。影响夹具：否。
- r11 第二片（2026-10-18）：S4.4 损坏后初值的验收口径；S5.10 可见集合无条件取并集；§5 注销销毁长期登录态；§6 Off 排在 `persist_pending` 之前、`retired` 行同步；ST-23d、ST-42 去掉机制用语，ST-26e 给出可判定的下界，EV-06 同步；FI 改 1a、1c、1d、5c、6a、6b、6c、8b，新增 1e–1g、6d–6f、7c、7d、8d，并写明注入的故障类型。影响夹具：否。
- r11 第三片（2026-10-18）：附录 A.3 补全局收紧时销毁长期登录态。影响夹具：否。
- r12 第一片（2026-10-18）：按第 11 轮双审互评与 David 第 11 轮后的决定修订：宿主级确认前再次启动的"不再追加"加上前提；`unsaved_settings` 改挂在 profile 上，全局收紧时 binding 一律 `retired`，占位上的登录也经 T44；T41、T02 加前置；挂起期间的相反动作视为放弃并持久化抵消；不自动重试 reason、窗口与超时计数列为收紧类，T22、S2.4 改为持久化；`principal_changed` 恢复 principal 判定的豁免，改在 T36 校验；全局收紧时长期登录态只作废、不销毁（与 #22 一致，David 定），#3 补全局收紧的情形，#20 的补充部分标为 Grok/Opus 交叉确认；S1.13 确认在新 binding 持久化前被拒；§1 状态表补 T44、T26。影响夹具：否。
- r12 第二片（2026-10-18）：S4.7 候选元组按当前配置比较；S5.3 补全局收紧情形；§5 启动行补例外；§6 `retired` 行同步；ST-22a、ST-42 改写，新增 ST-48a–e（T44 等）；FI 收紧类崩溃点改为"写入之前"，新增 FI-1h、FI-2c、FI-8e、FI-8f，改 FI-5d、6b、6d、6e、7c、8d。影响夹具：否。
- r12 第三片（2026-10-18）：附录 A.3 改为作废（不复用）长期登录态。影响夹具：否。
- r13 第一片（2026-10-18）：按第 12 轮（定向）双审互评修订：T13、T16、T36、T37、T43 加"所属 profile 不带 `unsaved_settings`"前置，T40、T41 改用同一说法；T36 的 principal 校验改为确认落盘的前提，不同则按 T23 丢弃确认；T44 原子持久化并写清与 T43 的关系；I4 写明 `retired` binding 的值由新 binding 初值越过；I8 覆盖 profile 级待确认；I1 补"依赖未生效放开同样被拒"与"被放弃放开的晚到确认忽略"；`dependency` 与窗口计数改为尽力持久化，离开不自动重试 reason 归入放开类；S1.13 写明单纯登录不算确认；§1 表补 T44；§10 #20 同步。影响夹具：否。
- r13 第二片（2026-10-18）：§6 占位 `dependency` 在 `unsaved_settings` 下 retryable 为 false；ST-48c/d/e 改写，新增 ST-48f、ST-48g、ST-36b；EV-06、FI-2a、FI-7a 随 profile 级待确认改写；新增 FI-1i、FI-8g、FI-8h。影响夹具：否。
- r14 第一片（2026-10-18）：按第 13 轮（定向）双审互评修订：T13、T16、T37 不再整行排除带 `unsaved_settings` 的 profile，只挡进入 `loading` 与建立 binding；T02 加"binding 不带 `principal_changed`、登录 principal 等于 binding 的 principal"；S2.5 改为与 binding 的 principal 比较，并写明带 `unsaved_settings` 时不在此建立；T44 要求宿主级确认已持久化、措辞改为"同 T43 的效果"；I1 写明依赖宿主级确认的放开与本地结果、重试时机；放开类只保留不自动重试占位 reason 的清除与复验超时计数清零；I8 补全局收紧情形；§1 表补 T44；§10 #27 同步。影响夹具：否。
- r14 第二片（2026-10-18）：§6 `local_storage` 行同步；ST-48c、ST-48g 改写，新增 ST-49；FI-1i 只接受全局收紧。影响夹具：否。
- r15 第一片（2026-10-20）：按 PR-Daemon 对 #1070、#1071 的评审修订：依据的 READONLY 改为 r8；#1070 B1：S2.3 改为 Off 不停止 sidecar（§10 #3、S5.3）；L1：S1.4 的重新登录（T38）只适用 `credential_rejected`、`access_revoked`，`incompatible`、`instance_unidentified` 只经 T10；L2：T32 补取消选用，表后说明同步；L3：I1 写明 T21、T23 的新 binding 等退役确认持久化（或同一次原子持久化），未确认时新 binding 丢弃、占位记 `dependency` 按 T40 重试，T21、T23 后置同步；L4：§1 `ready` 行与 T03 补 `live: true`，T04 同样补上；L5：§1 `registered` 进入列补 T36；N1：§10 开头改为三片；#1071 B1（本片部分）：T38 的 generation 列分支，同一 principal 不变，换 principal 同 T23（旧 binding +1）。影响夹具：否。
- r16 第二片（2026-10-20）：按 PR-Daemon 对 #1071 的评审修订：B1：S4.3 的不递增只限 T38 同一 principal 那一支，换 principal 那一支按 T23（旧 binding +1），S4.2 同步；ST-38c 改为旧 binding +1。B2：S4.6 吸收 READONLY r8 R6.1、R7.3 的 `null` 回显配对（按 HTTP 交换配对，在途、未作废、准入 generation 等于当前值才接受，否则丢弃并给本地 `cancelled`；非 `null` 的错值照旧拒绝），新增 EV-33（EV-29–EV-32 已用于附录 A.6）。M1：S5.10 只拿用当前凭据发出的请求做比较（凭据归属同 S3.1），S3.1 末句同步，新增 EV-34。L1：S1.6、S1.7、S4.7 的本地结果改为按 §6 的顺序，ST-11c、ST-19c 写明新调用的 `reason`。L2：§5 表补凭据无效类响应（T42）与宿主发起的删除 KB 两行。L3：新增 ST-26f、ST-26g（正常恢复下不自动重试的 reason、占位 reason 与各计数跨重启保持）、ST-49b–d（I1 阻止范围）、FI-3e、FI-3f（损坏确认后不再进入、首次安装的否定判据）、FI-6g（正常恢复下 principal 不同即 `retired`）、FI-8i（I8 第二句）；FI-3a 补断言 profile 按未选用、已停用处理。Nit：FI-1f 前置写明 Off；ST-05g/05h、ST-24e/24f、ST-48e/48f 按 ID 排序。影响夹具：否。
- r17 第三片（2026-10-20）：按 PR-Daemon 对 #1072 的评审修订附录 A（非规范，不新增规范要求）：B1：A.2 写明"删除凭证排在内存生效之后、一次崩溃即可丢失收紧"的窗口尚未解决，参考设计不承诺，留给 KB-C1，并写明实现须满足的不变式（持久标记早于内存生效、单次失败打不掉，同时满足 FI-1a）；M1：A.3 任何恢复写入之前先截到完整记录边界，不论凭证是否有效，A.6 补对应用例；L1：A.2 重建凭证还要等日志外的收紧写入（#1013② 长期登录态清除，FI-7c）；L2：A.1 同一序号上作废记录与晚到原记录取更严格者，A.5 补宿主级确认、离开不自动重试的 reason、占位失败 `reason` 清除、复验超时计数清零的抵消；L3：A.4 写明 +2^32 的基数与 +2^16、+2^32 的上界假设（已知局限）；L4：A.2 写明单一宿主级凭证是有意偏离，只影响可用性；Nit：页首日期改为 2026-10-20，A.2 写明尽力持久化的记录不走删除凭证的路径。影响夹具：否。
