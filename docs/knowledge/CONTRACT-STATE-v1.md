# Knowledge profile / Off / 撤权状态机契约 v1（CONTRACT-STATE-v1）

> **状态**：**草案（Draft）**，待 fork 侧维护者（David）评审（文档 PR，base `ab/weknora`）。评审通过前不授权任何实现。
> **版本**：契约 v1，文档修订 r14 · **日期**：2026-10-17 · **作者**：David Xu
> **Issue**：#1013 ①（KB-CT-03），阶段 #1000；消费方 KB-C1-04、KB-LITE-04、KB-HOST-01。
> **依据**：任务方案 v2.1（方案全文见 #986 第一条评论）与 `docs/knowledge/CONTRACT-READONLY-v1.md`（下称 READONLY，按 r7）。来源标签沿用 READONLY：决策 `[D1]`–`[D8]`、任务条目（如 `[KB-C1-04]`）、READONLY 规则（如 `[R7.4]`）。
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
| `registered` | profile 已登记，binding 未激活：profile 未被选用、principal 未绑定（未登录、已注销）、带待确认标志（S1.13），或只有未绑定占位 | T01、T13、T16、T19、T20、T22（占位）、T23、T24、T26、T43、T44（新 binding 待确认时） | T02、T12、T15、T36、T40、T41、T44、终结（T23–T25） | 本地拒绝 `profile_inactive`（占位记有失败 `reason` 时用它，§6） |
| `loading` | 已激活，正在建立：local-managed 完成握手并取得实例标识（KB-LITE-01a）；remote 取得凭据（#1013②）；能力发现与兼容判定（R9.4）；不带 scope 的就绪探测（R8.2） | T02、T10、T13、T16、T18、T21、T34、T35b、T36、T38、T40、T41、T42、T44；启动时已激活（T26；带不自动重试 `reason` 的除外） | T03、T04、T05、T11、T14、T19–T25、T33、T35a、T35b、T42 | 本地拒绝 `not_ready` |
| `ready` | 兼容；凭据有效；`operations.search` 与 `operations.read` 都是 `ready`；进入时清除 `revoked_since_ready`（S1.11） | T03、T07 | T06、T08、T11、T14、T18–T20、T23–T25、T33、T42 | 准入 |
| `degraded` | 兼容；凭据有效；`live: true`；search 与 read 恰好一个是 `ready` | T04、T06 | T07、T09、T11、T14、T18–T20、T23–T25、T33、T42 | 只准入为 `ready` 的操作，另一个本地拒绝 `not_ready` |
| `unavailable` | 已激活但不能服务，带 `reason`（S1.4） | T05、T08、T09、T18、T35b 与 T42（窗口内第 3 次）、T22、T33、T35a；启动时保持不自动重试的 `reason`（T26）；`persist_pending` 时进入连接阶段（S1.7） | T10、T11、T14、T19、T20、T23–T25、T34、T35b、T38、T42（只从 `dependency`、`not_ready`） | 本地拒绝，按 §6 |
| `disabled` | Off 标志为真（S5.1），或该 profile 的停用标志为真（S5.4），或重启时进入全局收紧、持久状态损坏（S1.8、S1.14） | T01、T11、T12、T14、T15、T17、T23、T24、T37、T43、T44；启动时标志已持久（T26） | 两个标志都清除：T13、T16；T44；终结（T23–T25） | Off：`knowledge_off`；仅 profile 停用：`profile_inactive` |

- **S1.2** [新设] **派生优先级**：服务状态按以下顺序取第一个成立的：Off 标志或 profile 停用标志为真 → `disabled`；未激活 → `registered`；否则按连接阶段取 `loading` / `ready` / `degraded` / `unavailable`。两个标志相互独立，可以同时为真。§3 的转移表是这条规则的展开，两者不一致视为文档错误，按 §0.3 修订。`persist_pending` 不参与派生（S1.7）。持久状态损坏时按 S1.14 处理（S1.8）。
- **S1.3** [R8.2/R8.4+新设] 连接阶段只看就绪响应的 `live` 与 `operations.search`、`operations.read`：`live: false`，或 search 与 read 都不是 `ready`，为 `unavailable`（S1.4）。`operations.ingest`（只读 v1 为 `disabled`）不参与判定。按 KB 的就绪（R8.4）不改变服务状态，由 Tool 结果按 KB 报告。未知的就绪 `state` 按 `not_ready`（R6.2）。
- **S1.4** [R9.4+新设] `unavailable` 的 `reason`：
  - 按退避自动重试：`dependency`（不可达、探测超时、`live: false`、sidecar 不可用，或取凭据、续签时签发方不可达）；`not_ready`（存活，但 search 与 read 都不是 `ready`）。
  - 不自动重试，只在手动重试、重新登录（T38）、profile 配置变更或 sidecar 重新握手后再试：`incompatible`（R9.4 不兼容）；`credential_rejected`（T05：属于 S3.1 例外的签发被拒，或本轮新签发的凭据被以撤权信号拒绝且属于该例外；凭据无效类响应的续签也失败，S3.1、T42，此时占位）；`access_revoked`（`loading` 或 `unavailable` 中出现撤权信号，包括不属于 S3.1 例外的重签被拒，T35a；或撤权类事件在窗口内达到上限，T18、T35b）；`instance_unidentified`（握手拿不到实例标识，S2.4）。
  - `local_storage`：由 T33 进入，或由 S1.7 规定的、要进入 `loading` 时遇到 `persist_pending` 的转移进入；`persist_pending` 清除后回到 `loading`（T34）。其他 `reason` 的 `unavailable` 在 `persist_pending` 期间保持原 `reason`，清除后也不变。
- **S1.5** [D4+新设] 只有可服务状态发出 search 与 read。`loading`、`unavailable` 只发能力发现与就绪探测，例外：`unavailable` 的 `reason` 为 `local_storage`，或 `persist_pending` 为真时，不发任何请求；`incompatible`、`credential_rejected`、`access_revoked`、`instance_unidentified` 不自动重试。`registered`、`disabled`、`retired` 与未绑定占位不发任何 READONLY 请求，包括就绪探测。Provider 在 **dispatch 时每次**检查状态并按 §6 本地拒绝；这条是硬性要求，与 Tool 是否注册、是否向模型广告无关。**广告**（§10 #14、#18 已定）：Off 时 Knowledge Tool 仍在广告里，不隐藏；描述标明"当前不可用"及原因（Knowledge 已关闭），按当前状态每轮生成（`info()` 每轮读取，§9 第 3 条），不改注册表、不改内核接口；不主动提示用户打开。其他不可服务情形的描述由 KB-C1-05 定。
- **S1.6** [新设] **终态 `retired`**：binding 的生命周期终点。进入：换实例后的旧 binding（T21）、principal 变化后的旧 binding（T23）、改端点或租户后的旧 binding（T24）、移除 profile（T25）、持久状态损坏，或重启时的全局收紧（S1.14，界面上以待确认呈现）。`retired` 的 binding 不可再激活，只能新建 binding；不准入任何请求；绑定它的 run 一律本地拒绝 `profile_inactive`（S4.7）。它的最终 generation 继续保存，供同一 profile 下新 binding 取初值（S4.4）。
- **S1.7** [新设] **阻断条件 `persist_pending`**：该 binding 有尚未确认持久化的 generation 变化或放开类变化（S1.10）时为真（持久化失败、超时、结果未知，或在重试）；收紧类变化的持久化失败不置它，只影响单个 run 的持久化（run 绑定与补记，S4.7）失败也不置它。它不参与 S1.2 的派生，对外仍可显示 `disabled` 或 `registered`；但只要它为真，该 binding 不准入任何请求，本地结果与准入都以它为准（§6 `local_storage`）。所有进入 `loading` 的转移都先检查它：为真时改为进入 `unavailable`（`local_storage`）。Provider 在后台重试，确认后清除（T34）。
- **S1.8** [新设] **持久状态损坏时 fail closed**：持久状态无法读取、损坏或不完整到无法判定时，按 S1.14 的"持久状态损坏"处理，不再区分单个 binding 的损坏。该 profile 上次已确认的 principal 无法确定时按"不同"处理（T43）。M1 不同：写入门遇到损坏内容按开，召回路径报错、不召回（§9 第 1 条）。
- **S1.9** [新设] **loading 轮次**：每次进入 `loading` 生成一个新的尝试令牌；本轮发出的凭据请求、握手与探测都带它。由这些结果派生的转移（T03–T05、T21、T22）只在令牌等于当前轮时应用；离开 `loading` 时，本轮在途的探测立即标记作废并中止。上一轮的结果因此不能推动新的一轮。占位上的登录与握手（S2.5、T40、T41）同样带尝试令牌，只有当前令牌的结果才能建立 binding。同一轮内的就绪探测带递增序号，只应用序号大于已应用者的结果。
- **S1.10** [新设] **崩溃安全不变式**（规范；David 第 9 轮后定：正文只冻结对外行为与本条不变式，实现方式不冻结，非规范性参考设计见附录 A，以 §8 的故障注入用例 FI 验收）。变化分两类。**收紧类**：Off、停用 profile、取消选用、注销（解除登录、销毁短期凭据，长期登录态按 #1013② 清除）、移除 profile、`retired`（含换实例、principal 变化、改端点或租户中旧 binding 的退役）、撤权类事件与其他 S5.6 触发事件的占位规则与证据纪元、宿主删除 KB 的墓碑与占位（S5.10）、待复验登记（S3.1）、标记待确认（S1.13、S1.14）、进入不自动重试的 `unavailable` reason（`incompatible`、`credential_rejected`、`access_revoked`、`instance_unidentified`）、占位上记下的不自动重试的失败 `reason`（`credential_rejected`、`instance_unidentified`）、复验的超时计数。`dependency` 与撤权窗口、凭据无效窗口的计数尽力持久化：持久化失败不算收紧丢失，不触发全局收紧。**放开类**：On、启用 profile、选用 profile、用户确认（S1.13）、新建或重新绑定 binding（含 generation 初值与待确认标志）、首次可服务（S1.12）、解除待复验（S3.1）、撤回删除墓碑（S5.10）、run 的绑定与补记（S4.7）、宿主级确认（S1.14 的提示）、离开不自动重试的 reason（T10 手动重试、T38 重新登录、配置变更或重新握手），以及不自动重试的占位失败 `reason`（`credential_rejected`、`instance_unidentified`）的清除与复验超时计数清零。最近一次可见集合（S5.10）的增补同样要持久化。不变式：
  - **I1 没有持久记录就不放开**：放开类变化在持久化确认之前不生效；持久化已经落盘、确认尚未返回时崩溃，重启后可以按已确认生效；持久化结果未知而放弃时，持久状态不得比内存更放开，重启后不得出现内存里从未生效、也未曾落盘的放开。**挂起期间的相反动作**：放开已提交、尚未确认时，用户做出相反的动作（如 On 挂起时点 Off），视为放弃这次放开，并持久化一条抵消它的收紧（收紧类；即使内存里是空操作也要持久化）；被放弃的放开，之后返回的确认一律忽略。**阻止范围**（第 10 轮盲选）：一个放开，只要它生效所依赖的对象上有尚未确认持久化的收紧，就被拒：宿主级收紧未确认时拒绝所有放开；binding 级收紧（含注销、退役）未确认时拒绝同一 profile 下的放开，包括给该 profile 新建 binding，按 profile 判定；宿主级放开（如 On）在任一 binding 有未确认收紧时被拒；涉及多个 binding 的放开，任一涉及的 binding 有未确认收紧即被拒。依赖一个尚未生效的放开（新 binding 尚未持久化、宿主级确认尚未给出或尚未持久化）的放开同样被拒：所依赖的放开正在持久化时本地结果为 `local_storage`，宿主级确认尚未给出时 Off 仍为真，本地结果为 `knowledge_off`。依赖宿主级确认的放开只有 On 与 T44。**被拒的放开直接丢弃**（第 10 轮盲选）：不排队、不自动重放，本地结果按上面两种原因给出（因未确认的收紧被拒为 `local_storage`）交给发起方；用户发起的，在所依赖的收紧或放开确认持久化之后界面可以提示重试，重试按当时的状态重新校验；系统发起的（run 绑定、解除待复验等）由发起方按自己的逻辑重试。
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
- **S2.3** [KB-LITE-04+新设] Off 不规定 sidecar 是否停止（§10 #3）。停用或移除 local-managed profile 时，先按 T14 进入 `disabled`、丢弃凭据、递增 generation（只在连接阶段），再按 KB-LITE-04② 的所有权语义停止 sidecar；hide ≠ disable。
- **S2.4** [D2+新设] **binding 身份**：remote 为 profile × principal × 端点 × 租户（或 profile 配置修订号）；local-managed 为 profile × principal × 实例标识。任何一项变了就是新 binding，旧 binding 进入 `retired`（T21、T23、T24）。local-managed 的实例标识只取自 KB-LITE-01a 的受控握手，不取 HTTP 响应自报的值；同一实例重启（标识不变）不算换实例，generation 不变。握手拿不到标识时按 T22 处理：不新建 binding，不递增；`instance_unidentified` 按收紧类持久化（S1.10）。换实例只能靠 sidecar 状态变化或握手发现；新实例若仍接受旧凭据（例如复制了数据目录），归 KB-LITE-01a、KB-LITE-02 处理，不在本文。
- **S2.5** [新设] **未绑定占位与 binding 建立**：profile 登记后、首个 binding 建立前，以及所有 binding 都已 `retired` 后（例如改端点或租户，T24），profile 只有一个未绑定占位：状态取 `registered`（Off 或停用时取 `disabled`），没有 generation，不准入任何请求。登录（remote）与握手（local-managed）在占位上进行；成功后新 binding 连同 generation 初值（S4.4）持久化，确认后按 T02 进入 `loading`（所属 profile 带 `unsaved_settings` 时不在此建立，按 S1.13、T44），才发 READONLY 请求（能力发现、就绪探测）。失败时占位保持 `registered`，并记下失败 `reason`（`credential_rejected`、`instance_unidentified`、`dependency`）供本地结果使用；被选中、且 Off 与停用标志都为假的占位，记有 `dependency` 时按退避自动重试，local-managed 在 sidecar 进入 `starting` 时也重试（T40，与 T10、S2.2 一致）；记有 `instance_unidentified` 或 `credential_rejected` 时，只在手动重试、sidecar 重新握手或重新登录后再试（T41）。Off 或停用期间暂停重试，不发任何请求。登录与握手本身不是 READONLY 请求，不带 generation。还有非 `retired` 的 binding 时，登录得到的 principal 与该 binding 的 principal 不同按 T23（该 binding 带 `principal_changed` 时按 T36 的规则处理）；只剩占位时，登录得到的 principal 与该 profile 上次已确认的 principal（S1.13）不同按 T43，新建的 binding 带待确认标志（S1.13），停在 `registered` 等用户确认（T36）。上次已确认的 principal 不可读按"不同"处理（S1.8）。
- **S2.6** [新设] **持久键**：generation 等 binding 级持久状态以 `binding_id` 为键。身份元组（S2.4）随 binding 一起持久化，用来查找"该身份下非 `retired` 的 binding"。同一身份元组在旧 binding `retired` 之后再次出现（例如端点 X → Y → X），新建 binding，取新的 `binding_id`；generation 初值仍按 S4.4 取同一 profile 下的最大值 + 1。
- **S2.7** [新设] `retired` binding 的长期登录态不得用于任何其他 binding（包括同一身份元组后来新建的 binding）。是否销毁、如何销毁由 #1013② 定义（§10 #22）。

## 3. 事件与转移

- **S3.1** [D2+新设] 事件定义：
  - **Off / On**：用户关闭或打开 Knowledge，改的是宿主级 Off 标志（S5.1、S5.2）。**停用 / 启用 profile**：改的是该 profile 的停用标志，持久保存（S1.10）。**移除 profile**：先停用再终结（T25）。
  - **撤权类事件**分两种。**撤权信号**：响应为 `unauthorized` 且 `reason` 是 `credential_revoked`；签发权威通知撤销，或拒绝续签（凭据无效类响应触发的那一次续签被拒按下面的凭据无效类处理，除非签发方明示已撤销）；不属于下面例外的重签被拒（S1.11、S1.12）；local-managed 轮换本实例秘密。**范围变窄**：同一 principal 续签后凭据有效，但范围更窄（新范围既有新增又有移除时，只要移除了任一 KB 或操作，就算变窄）；或凭据没变、服务端收窄了授权（S5.10，§10 #21 已定按撤权处理）。**任何连接阶段**出现撤权类事件，不论凭据是新签发的还是沿用的，都按 S4.5 作废在途请求、递增 generation、触发 S5.6 的占位，并置 `revoked_since_ready`。去向：`ready`、`degraded` 走 T18（回到 `loading` 重签）；`loading`、`unavailable` 中的撤权信号走 T35a（`access_revoked`），范围变窄走 T35b（回到 `loading`：续签变窄用新凭据，服务端收窄重新取凭据）。由已接受响应派生的撤权事件不受 S4.6 前置状态检查的约束。`credential_expired` 先续签：续签成功按 T27，被拒算撤权，签发方不可达按 T08、T09 进入 `unavailable`（`dependency`），不递增；`loading`、`unavailable` 中同样先续签：成功、且同一 principal、范围相同或更宽，则在 `loading` 中继续（`unavailable` 时按 T10 回到 `loading`），principal 变化按 T23，范围变窄按 T35b，被拒按撤权（T35a，例外条件下按 T05），签发方不可达按 T05 进入或保持 `unavailable`（`dependency`）。`forbidden`（任何 `reason`）只是请求级错误（T28），本身不算撤权；它带出的服务端收窄按 S5.10 判定。
  - **凭据无效类响应**（§10 #9 已定）：响应为 `unauthorized` 且 `reason` 是 `credential_invalid`、`credential_missing` 或未知值。第一次不算撤权：不递增、不作废在途请求、不占位、不置 `revoked_since_ready`；Provider 立即停止准入新调用，丢弃当前短期凭据，进入 `loading` 的新一轮并续签一次（T42；`unavailable` 只在 `dependency`、`not_ready` 时这样做，其他 `reason` 保持不变）。续签成功按正常的 `loading` 判定。**续签也失败**（签发方拒绝这次续签，或续签得到的凭据仍收到凭据无效类响应）：按 T05 进入 `unavailable`（`credential_rejected`），并按 S5.6 记一条适用全部 KB 的占位规则；不递增，也不声称已撤权。"续签也失败"只在 T42 开启的那一轮 `loading` 内判定；该轮进入可服务状态后，再收到凭据无效类响应重新按 T42（计入下面的上限）。签发方明示已撤销（如以 `credential_revoked` 拒绝）时按撤权处理（T35a）；签发方不可达按 T05 进入 `unavailable`（`dependency`），不占位。**凭据归属** [新设]：请求记下所用短期凭据的代号；只有用当前凭据发出的请求，其凭据无效类响应才触发上述处理；用已丢弃的旧凭据发出的请求，只把结果换成本地 `not_ready` 交给 Tool 层（§6），不触发转移。**旧凭据的撤权信号** [新设]（David 第 7 轮后定；未完成时的收尾经 Grok、Opus 盲选）：用已丢弃的旧凭据发出的请求带回撤权信号时，不立即按撤权处理。Provider 记下待复验（收紧类：binding、被质疑请求的 Q 与版本号；同一 binding 的多条合并，Q 取并集，版本号 +1），在内存立即生效：该 binding 不准入新调用（本地 `not_ready`），历史中该 binding、Q 内 KB 的证据正文暂扣，包括登记之后追加的证据（S5.6）。然后用当前凭据发一次复验探测（带 scope 的就绪探测，范围覆盖 Q）：复验得到撤权信号，按撤权处理（T18、T35a），暂扣变为占位；复验通过、且这次复验覆盖的版本号仍是当前版本，解除待复验（放开类），持久化确认后恢复准入、解除暂扣，旧请求的结果换成本地 `not_ready`；版本号已变则按新的 Q 再复验；复验得到凭据无效类响应，按本段凭据无效类处理，待复验保留。复验没能完成（被 S4.5 中止、复验期间崩溃、不可达或超时、当前没有可用凭据）时待复验保留，不准入、暂扣继续；之后该 binding 每次取得当前凭据并处于连接阶段时重做复验，超时按退避重试，连续 3 次超时后停止自动重试，等手动重试或重新登录。binding 终结（`retired`）时待复验仍未解除，按撤权收尾：暂扣的证据换成占位。服务端收窄与所用凭据无关，不适用复验（S5.10）。**上限** [新设]：同一 binding 10 分钟内第 3 次凭据无效类事件不再续签，直接按"续签也失败"处理。本类响应不适用下面的例外。
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
| T03 | `loading` | 兼容，search 与 read 都 `ready` | `ready`（首次先持久化"首次可服务"，S1.12） | 不变 | [R8.2/R9.4] |
| T04 | `loading` | 兼容，恰好一个 `ready` | `degraded`（同 T03） | 不变 | [新设] |
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
| T21 | `loading` | 换实例 | 旧 binding `retired`；新 binding 建立，按 S1.2 派生 | 旧 +1；新 binding 按 S4.4 | [新设] |
| T22 | `loading`，或占位上的握手 | 握手拿不到实例标识 | 已有 binding：`unavailable`（`instance_unidentified`）；占位：保持 `registered` 并记下该 `reason` | 不变；`instance_unidentified` 按收紧类持久化，不写 generation | [新设] |
| T23 | 任一非 `retired` | principal 变化 | 旧 binding `retired`；新 binding 建立，按 S1.2 派生（带待确认标志，未经用户确认不算激活，S1.13、T36） | 旧 binding 在连接阶段时 +1；新 binding 按 S4.4 | [D2+新设] |
| T24 | 任一非 `retired` | 改端点或租户（remote） | 旧 binding `retired`；profile 回到未绑定占位（按 S1.2 派生），不复用旧登录态，重新登录后按 S2.5 建立新 binding | 同 T23 | [新设] |
| T25 | 任一非 `retired` | 移除 profile | 连接阶段先按 T14 作废；之后 `retired` | 只在连接阶段 +1 | [新设] |
| T26 | 进程启动 | Provider 启动 | 按 S1.14 的恢复语义派生（不自动重试的 `reason` 保持） | 每个非 `retired` 的 binding 递增；已退役的 binding 不递增，其值由新 binding 的初值越过（I4、S4.4） | [KB-C1-04+新设] |
| T27 | `ready` / `degraded` | 续签：同一 principal，范围相同或更宽 | 不变 | 不变 | [新设] |
| T28 | `ready` / `degraded` | 单个请求得到 `forbidden`（任何 `reason`）、`not_found`、`stale`、partial（带出的服务端收窄另按 S5.10 走 T18、T35b） | 不变 | 不变 | [R2.3/R3.4/R6/R7.6] |
| T29 | 任一 | M1 记忆开关切换 | 不变 | 不变 | [KB-C1-04] |
| T30 | `ready` / `degraded` | 用户取消一个 run | 不变 | 不变；该 run 的在途请求立即移出在途集合并中止 | [R7.1+新设] |
| T31 | `disabled` | 撤权、取消选用、注销 | `disabled` | 不变（无在途请求） | [新设] |
| T32 | `registered` | 撤权、注销 | `registered` | 不变 | [新设] |
| T33 | 连接阶段 | 目标为 `loading` 的递增事件（T18、T26，以及目标为 `loading` 的 T35b；不含窗口内第 3 次）持久化失败或未确认；或"首次可服务"未确认持久化（S1.12） | `unavailable`（`local_storage`），`persist_pending` 为真 | 新值不生效 | [新设] |
| T34 | `persist_pending` 为真 | 后台重试，该 binding 未确认的 generation 与放开类变化全部确认持久化（S1.10） | 清除 `persist_pending`；只有 `unavailable`（`local_storage`）回到 `loading`，其他状态与 `reason` 不变 | 写入的值生效 | [新设] |
| T35a | `loading` / `unavailable` | 撤权信号（S3.1；T05 的例外除外），包括 `unavailable` 期间已接受响应带回的撤权信号 | `unavailable`（`access_revoked`）；丢弃内存凭据 | +1（按 S4.5 作废在途请求） | [D2+新设] |
| T35b | `loading` / `unavailable` | 范围变窄（S3.1，含服务端收窄 S5.10） | 进入 `loading` 的新一轮（续签变窄用新凭据，服务端收窄重新取凭据）；窗口内第 3 次改为 `unavailable`（`access_revoked`）；`unavailable` 的 `reason` 为 `credential_rejected`、`access_revoked`、`incompatible`、`instance_unidentified` 时只作废与占位，`reason` 不变，不重新取凭据 | +1（按 S4.5 作废在途请求） | [D2+新设] |
| T36 | `registered` 或 `disabled`，带待确认标志，原因 `principal_changed`（S1.13）（所属 profile 不带 `unsaved_settings`；带标志时只能经 T44） | 用户确认 | 存在长期登录态而其 principal 与 binding 不同时，必须重新登录并得到与 binding 相同的 principal，确认才能落盘；得到别的 principal 按 T23 处理，这次确认丢弃；确认持久化后（S1.13）按 S1.2 派生（`disabled` 时仍为 `disabled`） | 不变 | [新设] |
| T37 | `disabled` | 登录或选用该 profile | `disabled`，记下登录与选用；Off 与停用都清除后按 T13、T16 激活；所属 profile 带 `unsaved_settings` 时只记下选用，登录按 S1.13 引导进 T44，不建立 binding | 不变 | [新设] |
| T38 | `unavailable`（`credential_rejected` / `access_revoked`） | 重新登录 | 同一 principal：`loading`（`revoked_since_ready` 不清除）；不同 principal：T23 | 不变 | [新设] |
| T39 | `persist_pending` 为真 | 又一个需要递增的事件 | 按该事件派生 | 待写值在已有待写值上 +1，写成功后一次生效（S4.4） | [新设] |
| T40 | 被选中的未绑定占位，记有 `dependency`，Off 与停用标志都为假，且所属 profile 不带 `unsaved_settings` | 退避到时，或 sidecar 进入 `starting` | 重新握手或取凭据；成功按 S2.5 建立 binding，按 S1.2 派生；失败保持并记下 `reason` | 无 | [新设] |
| T41 | 被选中的未绑定占位，记有 `instance_unidentified` 或 `credential_rejected`，Off 与停用标志都为假，且所属 profile 不带 `unsaved_settings` | 手动重试、sidecar 重新握手，或重新登录 | 同 T40 | 无 | [新设] |
| T42 | 连接阶段（`unavailable` 只限 `dependency`、`not_ready`），且不在 T42 开启、尚未进入可服务状态的那一轮 `loading` 中 | 用当前凭据发出的请求收到凭据无效类响应（S3.1） | `loading` 的新一轮：丢弃短期凭据、续签一次；续签也失败按 T05（`credential_rejected`）并占位；窗口内第 3 次直接按续签也失败处理 | 不变；不作废在途请求；第一次不占位 | [新设] |
| T43 | 未绑定占位（所属 profile 不带 `unsaved_settings`；带标志时只能经 T44） | 登录得到与该 profile 上次已确认的不同的 principal（或上次已确认的不可读，S1.8） | 新 binding 建立，带待确认标志，`registered`（Off 或停用时 `disabled`） | 新 binding 按 S4.4 | [新设] |
| T44 | 带 `unsaved_settings` 的 profile（S1.13），宿主级确认已持久化 | 用户确认该 profile，并重新登录或重新握手 | 解除 `unsaved_settings` 与建立新的 binding（新 `binding_id`、初值）是同一次原子持久化，按 S1.2 派生；principal 与上次已确认的不同时，新 binding 带 `principal_changed`（同 T43 的效果），`unsaved_settings` 照常解除 | 新 binding 按 S4.4 | [新设] |

T31、T32 里的注销仍按 #1013② 清除长期登录状态；`disabled` 下的取消选用只改变哪些 profile 被选用。目标状态不是 `loading` 的递增事件（T11、T14、T19、T20、T23–T25、T35a，不自动重试 `reason` 下的 T35b，以及窗口内第 3 次撤权类事件）持久化失败时，状态照常派生，同时 `persist_pending` 为真（S1.7）；只有目标为 `loading` 的递增事件走 T33。

## 9. 现状证据（Agent24 `eb1a58b`）

1. **M1 开关**：按 owner 存在 kv 的 `memory_settings` 命名空间（`rust/crates/agent24-memory/src/lib.rs:250-258`），默认开（`:402-407`），关闭"不是清除"（`:409-422`）；召回入口的门（`rust/crates/agent24-agent/src/session_memory.rs:151-156`），写入门与写入在同一个 `BEGIN IMMEDIATE` 事务里重查开关，避免"查到开、提交前被关"的竞态（`rust/crates/agent24-memory/src/writer.rs:273-291`，迁移说明见 `rust/crates/agent24-agent/src/retain.rs:229-237`）；该处读到无法解析的内容时按"开"处理（`writer.rs:289-291` 的 `.ok()` 与 `is_none_or`），召回入口经 `memory_enabled` 的 `?` 报错（`session_memory.rs:154`），run 记 warn 后不召回（`rust/crates/agent24-agent/src/lib.rs:1431-1434`）。Knowledge 的持久状态损坏一律 fail closed（S1.8、S1.14）。REST 为 `GET/PUT /api/v1/memory/settings`（`rust/apps/agent24d/src/memory_routes.rs:232-290`，`protocol/openapi.yaml:1501`）。这种"检查与动作在同一临界区"的做法是 S4.5、S4.6 的先例。
2. **注入**：run 开始时只调用 `memory.recall`（`rust/crates/agent24-agent/src/lib.rs:1426-1443`），另有显式"记住"时检查记忆开关（`:1455`）；没有 Knowledge 或通用 context hook。v1 的 Knowledge 注入只经 Tool（D4），自动注入属 KB-CTX-02。
3. **Tool 注册与广告**：`ToolRegistry`（`rust/crates/agent24-tools/src/lib.rs:1063-1074`）在 daemon 启动时建好（`rust/apps/agent24d/src/server.rs:1544`），`RunManager` 持有不可变的 `Arc<ToolRegistry>`（`rust/crates/agent24-agent/src/lib.rs:462`）。但广告有按轮动态过滤的钩子：`Tool::advertisable()`（`agent24-tools/src/lib.rs:392-394`，默认 `true`）由 `live_adverts()`（`:1271-1287`）每轮调用，`RunManager::tool_specs_for`（`agent24-agent/src/lib.rs:2123-2127`）走这条路径，`ModuleTool` 已用它按模块就绪与同意过滤（`agent24-tools/src/lib.rs:651-660`）。所以隐藏广告不必改注册表；`advertisable()` 没有 run 参数，若要按 run 区分（例如 Off 只对部分 run 生效）需改签名，属内核接口，要知会 jason。审批门只在 `requires_approval` 时调用（`:1382`），Read 级 Knowledge 工具不过门，所以拒绝必须在 Provider 每次 dispatch 时做（S1.5）。
4. **run 绑定**：`RunCreate` 只有 `session_id`、`workspace_id`、`prompt`、`model_override`、`mode`（`rust/crates/agent24-protocol/src/types.rs:443-457`），没有 profile 与 principal；模型调用的 task profile 从 `TaskProfile::default()` 起算（`rust/crates/agent24-agent/src/lib.rs:1726`）。S4.7 需要 KB-HOST-01 补字段。
5. **取消**：`cancel_run` 按 run 取消（`rust/crates/agent24-agent/src/lib.rs:900`），没有按 binding 作废的机制；T30 沿用它，S4.5 是新增。
6. **generation 先例**：`SidecarManager` 用自增 token 丢弃过期回调（`apps/desktop/src/main/sidecar-manager.ts:156`、`:177`、`:214`），状态集见 `apps/desktop/src/main/sidecar-contract.ts:56`；模块 attach 的 generation 只增、重启即新一代（`rust/crates/agent24-os-proto/src/attach.rs:34-38`、`:79-91`；`rust/crates/agent24-os-proto/src/drain.rs:1-20`）。本文的 generation 是另一个计数器，不复用它们。
7. 仓库里还没有 Knowledge Provider、Knowledge profile 存储或 Off 开关；`rust/crates/agent24-memory/src/knowledge.rs` 的 `KnowledgeBase` 是 MD-7 指令层，与本文无关。

## 10. 待决与需要评审确认的点

第一片不单独合入，也不单独关闭任务①；两片作为一组合入。

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

## 变更记录
- r0（2026-10-17）：首稿。影响夹具：否（本文的本地结果不上线，#1012 不收录）。
- r1（2026-10-17）：按 B 类 #6–#14 的盲选（Opus 与 Sonnet 各自独立作答、都没看草稿；David 2026-10-17 授权的夜间规则）修订。两边一致的直接写入；分歧的 #7、#9 中的 `credential_invalid`、#14 登记待 David；新增待决 #16–#19；改正广告现状。影响夹具：否。
- r2（2026-10-17）：按第 1 轮双审互评（Opus 代 Codex、Sonnet 代 Grok）修订：binding 身份加入端点与租户、`binding_id`；终态 `retired`、阻断条件 `persist_pending`；Off 与 profile 停用分成两个标志；`instance_unidentified`、`credential_rejected`；凭据错误分类；登录映射；作废时标记在途请求、三条接受条件；Off 持久失败的处理；转移表重编号为 T01–T34；新增待决 #20。影响夹具：否。
- r3（2026-10-17）：按第 2 轮双审互评修订：未绑定占位、`binding_id` 作持久键、`retired` 登录态不复用、持久设置读不出 fail closed、T35–T39、调用级作废、接受与消费同一临界区、证据纪元水位、服务端收窄、桥的接受条件；新增待决 #21–#23。影响夹具：否。
- r4（2026-10-17）：按第 3 轮双审互评修订：任何连接阶段的撤权都递增并占位；撤权窗口上限；loading 轮次令牌；持久记录分类（S1.10）；generation 读不出时 binding 进入 `retired`；T24 回到未绑定占位；占位自动重试（T40）；调用级、接受与占位规则的若干补充；§10 按来源分开标注，新增 #24、#25。影响夹具：否。
- r5 第一片（2026-10-17）：按第 4 轮双审互评修订：新增 `revoked_since_ready`（S1.11），撤权后重签被拒一律走 T35a；T35 拆成 T35a（撤权信号）与 T35b（续签变窄，回到 `loading`），增删并存按变窄；窗口上限计入 T35；T34 只带回 `local_storage`；S1.8 扩到 `retired` 标记、证据纪元与占位规则；T40 受 Off 与停用约束、后置按 S1.2 派生，新增 T41；§1 表进入与离开补齐；§9 补 M1 召回路径的出处；§10 #4、#9、#21、#24、#25 同步。影响夹具：否。
- r6 第一片（2026-10-17）：按第 5 轮双审互评与 David 2026-10-17 的逐条决定修订：新增曾可服务标志（S1.12），S3.1 的例外只适用从未可服务的 binding；待确认标志持久化（S1.13）；未落盘标记作启动兜底（S1.14，#20、#24）；凭据无效类响应不算撤权，新增 T42（#9）；服务端收窄按撤权处理，并入范围变窄（#21）；"切换离开"改为"取消选用"，允许多个 binding 同时激活（#5）；Off 时广告标明不可用（#14）；占位上的登录与握手带令牌、探测带序号；§9 retain.rs 行号；§10 #1–#5、#7、#9、#14、#17–#21、#24、#25 改为已定。影响夹具：否。
- r7 第一片（2026-10-17）：按第 6 轮双审互评与 David 第 6 轮后的决定修订：凭据无效类响应续签也失败时占位（#9），凭据归属、10 分钟 3 次上限；S1.4、T05、S3.1 例外只针对撤权信号；T35a 限定撤权信号；T35b 在不自动重试的 `reason` 下不重新取凭据；`credential_expired` 在 `loading`、`unavailable` 中的处理；S1.14 改为待写记录清单，启动时重放、先写盘再删条目、身份与配置不符直接 `retired`；T36 可在 `disabled` 下确认；S1.13 确认与置位的先后；占位上换 principal 带待确认；§1 表补 T42、T24；§10 #7、#9、#20 同步。影响夹具：否。
- r8 第一片（2026-10-17）：按第 7 轮双审互评与 David 第 7 轮后的决定修订：S1.10 单调写入、清除撤销置位重试；S1.14 恢复时纪元取历史最大纪元 + 1、generation 条目只用于取最大值且 T34 后删除；S1.4 `local_storage` 的进入；S1.8、S2.5 与新增 T43（占位上换 principal）；S3.1 "续签也失败"限于 T42 那一轮、旧凭据撤权信号先复验、`credential_expired` 续签成功时的 principal 与范围、窗口规则优先；T33 与表后说明的 T35b；§1 表补 T42、T43；§10 #7、#9、#21 同步。影响夹具：否。
- r9 第一片（2026-10-17）：按第 8 轮双审互评与第 8 轮后的设计决定（Grok、Opus 盲选）修订：崩溃安全改为单一预写日志（S1.10），收紧类内存立即生效并写日志、放开类日志确认后才生效、generation 先写后用；放开凭证与重放、不确定一律收紧（S1.14），取代未落盘清单与各独立持久标志；S1.7、S1.8、S1.12、S1.13 改写为日志记录；上次已确认的 principal（S2.5、T02、T43）；旧凭据撤权信号的待复验记录与暂扣（S3.1）；T10 在 `persist_pending` 下按 S1.7；T26 未正常关闭时 +2^16；§10 #7、#9、#20、#21、#24 同步。影响夹具：否。
- r10 第一片（2026-10-17）：按 David 第 9 轮后的决定（收窄契约范围）与第 9 轮双审互评修订：S1.10 改为崩溃安全不变式 I1–I8，收紧类与放开类清单补上选用与取消选用、标记待确认；S1.14 改为重启后的可观察语义（正常恢复、全局收紧、持久状态损坏、首次安装），含 principal 校验收窄、待复验与删除墓碑的恢复、不重复收紧；S1.7、S1.8、S1.12、S1.13 去掉机制描述，待确认以最近一次为准；S2.6 身份元组随 binding 持久化；S3.1 待复验带版本号；§10 #6、#10、#20、#24 同步，新增 #26。影响夹具：否。
- r11 第一片（2026-10-18）：按第 10 轮双审互评、第 10 轮的盲选（被拒的放开丢弃、阻止范围按依赖对象）与第 10 轮后的盲选（全局收紧补全）修订：收紧类补注销、移除 profile 与各类退役，放开类补宿主级确认；I1 允许已落盘未确认的放开生效并写明阻止范围与丢弃；I2 写明保证边界与"单纯重启"；I5 补删除墓碑暂扣；I6 补全局收紧的退路；I7 补注销；待确认带原因；S1.14 先判 `retired`、principal 判定去掉豁免、保留不自动重试 reason 与各计数、全局收紧时 binding 内部退役并销毁长期登录态、T40 暂停、宿主级确认前后的表现、损坏时选用与停用按收紧、首次安装的否定判据；新增 T44；§10 #20 同步，新增 #27。影响夹具：否。
- r12 第一片（2026-10-18）：按第 11 轮双审互评与 David 第 11 轮后的决定修订：宿主级确认前再次启动的"不再追加"加上前提；`unsaved_settings` 改挂在 profile 上，全局收紧时 binding 一律 `retired`，占位上的登录也经 T44；T41、T02 加前置；挂起期间的相反动作视为放弃并持久化抵消；不自动重试 reason、窗口与超时计数列为收紧类，T22、S2.4 改为持久化；`principal_changed` 恢复 principal 判定的豁免，改在 T36 校验；全局收紧时长期登录态只作废、不销毁（与 #22 一致，David 定），#3 补全局收紧的情形，#20 的补充部分标为 Grok/Opus 交叉确认；S1.13 确认在新 binding 持久化前被拒；§1 状态表补 T44、T26。影响夹具：否。
- r13 第一片（2026-10-18）：按第 12 轮（定向）双审互评修订：T13、T16、T36、T37、T43 加"所属 profile 不带 `unsaved_settings`"前置，T40、T41 改用同一说法；T36 的 principal 校验改为确认落盘的前提，不同则按 T23 丢弃确认；T44 原子持久化并写清与 T43 的关系；I4 写明 `retired` binding 的值由新 binding 初值越过；I8 覆盖 profile 级待确认；I1 补"依赖未生效放开同样被拒"与"被放弃放开的晚到确认忽略"；`dependency` 与窗口计数改为尽力持久化，离开不自动重试 reason 归入放开类；S1.13 写明单纯登录不算确认；§1 表补 T44；§10 #20 同步。影响夹具：否。
- r14 第一片（2026-10-18）：按第 13 轮（定向）双审互评修订：T13、T16、T37 不再整行排除带 `unsaved_settings` 的 profile，只挡进入 `loading` 与建立 binding；T02 加"binding 不带 `principal_changed`、登录 principal 等于 binding 的 principal"；S2.5 改为与 binding 的 principal 比较，并写明带 `unsaved_settings` 时不在此建立；T44 要求宿主级确认已持久化、措辞改为"同 T43 的效果"；I1 写明依赖宿主级确认的放开与本地结果、重试时机；放开类只保留不自动重试占位 reason 的清除与复验超时计数清零；I8 补全局收紧情形；§1 表补 T44；§10 #27 同步。影响夹具：否。
