# Knowledge UI 身份交换与消息桥契约 v1（CONTRACT-UI-BRIDGE-v1）

> **状态**：规范核心 v1（草案）· **日期**：2026-10-20 · **桥报文** `v: 1`
> **依据**：`docs/knowledge/CONTRACT-READONLY-v1.md`（下称 READONLY，按 r8）；`docs/knowledge/CONTRACT-STATE-v1.md`（下称 STATE，草案，PR #1070–#1072，尚未合入本分支；本文依赖它的规则汇总在 §8.2，STATE 定稿时逐条复核）。
> 由完整版 `ab/weknora-03b-ui-bridge`（#1077 的分支）压缩而来。本文只写对外行为与不变式；实现机制由各实现任务以代码和测试证明。
> 编号：`R*` 为 READONLY 规则，`S*` 为 STATE 规范核心 v1（同批）的条款，`U*` 为本文规则（沿用完整版编号，空号即已移出规范），`决定 #n` 见 §9.1（已定）与 §9.5（待决）。

## 0. 范围与边界

**本文管**：KnowledgeView（Electron main 为一个 binding 创建的隔离 WebContentsView，承载 WeKnora 嵌入页）的三件事：① 宿主如何为它取得 UI 会话（一次性票据换 session JWT，由宿主兑换并注入，页面看不到）；② 页面与宿主之间的受限消息桥；③ view 实例与 binding 长期登录态的销毁（STATE S2.7 交给本文）。

**本文不管**（交给谁）：
- READONLY：访问凭据 C、有效范围 E、错误信封与 `code`/`reason` 闭集、generation 只回显（R7.5）。
- STATE：binding 与 `binding_id`、generation 的值与递增、撤权与凭据无效处理、可服务状态、准入、桥的接受条件（S5.9）、崩溃安全与重启语义。**本文不新增 generation 递增事件**；页面引起的问题只做 view 级处理。
- KB-C2-01（服务端）：票据签发、兑换、撤销与会话探测入口，session JWT 签发与校验，UI 资源路径前缀白名单，CORS；HTTP 路径与类型。
- KB-C2-02：嵌入页与内存 session adapter（不持有任何秘密）。KB-C2-03：KnowledgeView 的创建、分区、CSP 细则、清理。
- KB-C2-04a / 04b：桥通道与注入钩子的 PoC；桥产品化。
- KB-HOST-01：C 与长期登录态存在哪里、谁铸票、main↔daemon 私有接口、跨进程作废与存活信号、登录态的键与撤销重试的持久形式。
- KB-LITE-04①：远端切换与注销隔离、历史可见性。KB-C1-05：引用 UI。KB-C1-04：桥 read 与主对话的总预算。

**变更规则**：沿用 READONLY §0.3。票据与 JWT 的绑定项、寿命上限、兑换判定、注入条件、消息类型闭集、拒绝类别改动升主版本。宿主按闭集校验，所以桥 schema 的任何改动（含新增可选字段）都递增 `v`；宿主可同时支持多个版本。

## 1. 术语

- **页面**：KnowledgeView 主 frame 当前已提交的文档；不是 Agent24 自身的 renderer。**宿主**：Electron main 与 Provider（持有 C 的一侧）的合称。**服务端**：WeKnora fork（local-managed 为本实例，remote 为远端）。
- **view 实例**：一个 KnowledgeView 从创建到销毁；每次创建生成新的 `view_secret`（CSPRNG，≥256 bit）。崩溃重建、切换主体都产生新实例。
- **view 证明**：宿主附在请求上的 `view_secret`。`vph` = H("kb-vph-v1"; `view_secret` 原始字节)，同时是**族**（同一 view 实例签出的全部 JWT）的标识。H(tag; x₁…xₙ) = SHA-256(len(tag)‖tag‖len(x₁)‖x₁‖…)，len 为 4 字节大端，输出无填充 base64url；宿主与服务端按同一公式计算。
- **view 级处理**：只销毁并重建 view 实例、撤销旧族，不递增 generation。**binding 级销毁**：销毁该 binding 的全部 view 实例（U4.7）。
- **允许 origin**：握手时记下的精确 origin（U4.4）。**长期登录态**：无需用户重新登录就能取得新 C 的材料（remote 的刷新令牌或设备授权）。
- **可服务**：STATE 的 `ready`、`degraded`。**当前 C**：Provider 此刻持有、未被替换或丢弃的 C。

## 2. 角色与信任边界

- **U1.1** 票据与 session JWT 只由服务端签发；宿主即使按 R1.4 持有实例秘密也不得自签。
- **U1.2** 页面不可信。页面始终拿不到 C、长期登录态、票据、JWT、`view_secret` 与本地路径；它只知道 generation、不透明的 view 实例 ID 与 `state` 消息里的非秘密信息。
- **U1.3** 受众隔离：C 与 session JWT（`aud: "agent24-knowledge-view"`）在服务端互不接受。JWT 的范围是 C 的子集，`ops` 只含只读 UI 操作，不含下载原件与 preview（R2.7），不能换票据、签凭据或调用写入与管理接口；READONLY 写明 UI 操作与 C 的关系之前（决定 #25），UI 操作不超出 search/read。
- **U1.4** C 不进任何 renderer；票据只在 Provider、main 内存及两者的私有接口里；JWT 只在服务端与 main 内存里。
- **U1.5** Agent24 自身的 renderer 不参与身份交换：铸票、撤销、作废通知所用的私有接口，经 renderer 可用的任何通用代理（如 `backendProxy`）都到不了；该通道双向认证。

## 3. 一次性交换票据

- **U2.1** 流程：① 只在 binding 可服务且准入开放时签票，并在同一次准入里读取 generation（S4.6、S5.9）；② Provider 用当前 C 请求签发，绑定 `binding_tag`、`partition_tag`、`vph`、`generation`；③ 服务端按 READONLY §1、§2 校验 C、计算 E（为空按 R2.3 拒绝），C 剩余寿命不足 60 秒时返回 `unavailable/credential_near_expiry`（可重试），响应按 U2.14 处理；④ main 兑换前确认 generation 未变、binding 可服务、view 实例存活，否则按 U2.12；⑤ main 经不会自动重发的通道兑换（受 U4.4 约束），票据放请求体并附 view 证明；⑥ 服务端判定并原子消费，JWT 只在响应体里交给 main。
- **U2.2** 票据是服务端生成的不透明随机句柄（≥256 bit），不是 JWT；`ticket_id` 只用于撤销与审计，不能兑换。
- **U2.3** 寿命默认 30 秒、最长 60 秒，只能兑换一次。**持久不变式**：已消费的票据此后永远兑换不成功；已撤销的票据与族在其 JWT 的最长寿命内一直被拒；两者都跨服务端重启、跨副本成立。做不到时，服务端必须让重启前签出的票据与 JWT 全部失效。
- **U2.4** 服务端记录的绑定项：principal、`tenant_id`、签发时的 E（非空）、C 的 `credential_id` 与到期时间、`binding_tag` = H("kb-binding-v1"; `app_instance_id`, `binding_id`)、`partition_tag`、`vph`、`generation`（只记录，R7.5）。
- **U2.5** 票据与 JWT 不进 URL、浏览器存储、cookie、页面可见的任何位置，不进任何日志、审计与崩溃报告；签发与兑换的请求和响应都 `Cache-Control: no-store`。
- **U2.6** 兑换判定（按序取第一个不成立的）：票据存在 → 未消费 → 未过期 → 未撤销 → 所在族未撤销 → view 证明与 `vph` 相符 → C 有效 → 重算的 E 非空。消费是原子的：并发兑换至多一个成功；判定之后不论结果，票据都已作废。兑换入口单独限速。
- **U2.7** 拒绝用 R6.1 信封。U2.6 前六项失败对兑换方一律 `unauthorized/credential_invalid`，彼此不可区分（票据是否存在、是否被用过都不泄露）；审计以 `internal_reason`（§9.4）区分。C 失效返回 `credential_expired` / `credential_revoked`；E 为空返回 `forbidden/empty_scope`。**兑换与会话探测的响应不是 READONLY 响应，不进入 STATE 的撤权判定。**
- **U2.8** 已消费的票据再次提交，或未消费票据的 view 证明不符，视为泄露：服务端撤销该 `vph` 整族并记安全审计；不撤销 C。
- **U2.9** **结果未知**：每个 view 实例同时至多一个兑换在途；超时或连接中断时不重发同一张票据，改铸新票。
- **U2.10** 过期票据按 U2.7 统一拒绝；宿主改铸新票。
- **U2.12** 签票后、兑换前：generation 变了或 view 实例已销毁 → 不兑换，票据与族都撤销，按 binding 级销毁；generation 未变但 binding 不再可服务 → 不兑换，只撤这一张票据，view 保留并暂停注入。
- **U2.13** 签发、兑换、拒绝、撤销各记一条审计：H(`ticket_id`)、principal、`credential_id`、`binding_tag`、`outcome`、`internal_reason`；不记票据、JWT 与 view 证明。
- **U2.14** **签发响应的凭据归属**：签发请求记下所用 C；签发入口的响应是 READONLY 类凭据响应，按所用 C 归属处理（STATE S3.1），R7.3 的 `null` 回显配对同样适用：
  - 所用 C 仍是当前 C：`unauthorized` 按 S3.1 分流（撤权信号按撤权类事件处理（S3.1），处理时已不可服务则进入 `unavailable`（`access_revoked`，S3.1、S1.4）；凭据无效类按 S3.1 续签一次；`credential_expired` 先续签再重签）；`forbidden/empty_scope` 交 STATE 按 S5.10 判定，请求范围 Q 取所用 C 的范围，并按 U3.8 关闭 view；`credential_near_expiry` 先续 C 再重试，不交 STATE。
  - 所用 C 已被替换：本次签发作废，改用当前 C 重签（U3.6）。凭据无效类、`credential_expired` 与 `forbidden/empty_scope` 都**不触发任何转移、不占位、不判定收窄、不关闭 view**；撤权信号不按撤权类事件处理，按 S3.1「旧凭据的撤权信号」记待复验，待复验的 Q 取该旧 C 的范围，复验覆盖这一范围。

## 4. session JWT

- **U3.1** 兑换成功时服务端签发；签名用服务端的 UI 会话密钥，与签发 C 的密钥分开，不是租户 HMAC（R1.2）；local-managed 下由本实例专属秘密派生（R1.4）。
- **U3.2** 声明：`iss`、`aud`、`sub`、`tenant_id`、`scope.knowledge_base_ids`（兑换时的 E，非空）、`ops`、`iat`、`nbf`、`exp`、`jti`、`ticket_id`、`parent_credential_id`、`binding_tag`、`partition_tag`、`cnf.vph`、`gen`（只记录）。
- **U3.3** 寿命默认 10 分钟、最长 15 分钟，`exp` 不晚于 C 的到期时间；不发刷新令牌。
- **U3.4** 服务端在**每个** UI 资源入口校验：签名（算法由服务端固定，拒绝 `none`，不按 header 选算法或密钥）、`aud`、`exp`/`nbf`（容差 ≤30 秒）、`jti` 与族未撤销、C 有效、view 证明与 `cnf.vph` 相符，并按 R2.1 重算 E，资源须在 E 内（越界与不存在不可区分，R2.6）。失败为 `unauthorized`：JWT 过期 → `credential_expired`；C 被撤销 → `credential_revoked`；`jti` 或族被撤销、签名、`aud`、view 证明缺失或不符 → `credential_invalid`。长连接（SSE、WebSocket）最多活到所用 JWT 的 `exp`；族、`jti` 或 C 被撤销时服务端关闭该族的长连接，至迟 60 秒。错误体与响应头不回显 `Authorization` 与 view 证明。服务端另提供只认 JWT 与 view 证明的会话探测入口。
- **U3.5** 页面里没有 JWT：嵌入页请求不带 `Authorization`，由宿主注入（U4.5）。
- **U3.6** **续期与换发**由宿主发起，都在同一 view 实例、同一族内按 U2.1 重新铸票兑换，服务端每次重新校验 C 与政策：JWT 用掉约 2/3 寿命时续期；当前 C 被替换（续签，或凭据无效类续签（S3.1）后取得新 C）时换发，换发完成前暂停注入，新 JWT 的 `parent_credential_id` 指向新 C。换发前因旧 C 失效得到的 401 不计入 `revocation_mismatch`。续签不要求撤销旧 C。
- **U3.7** **族与撤销**：撤销入口按 `ticket_id` 只撤那一张票据及其换出的 JWT，按 `vph` 撤整族；族撤销后同一 `vph` 的兑换与 JWT 一律被拒。撤销入口只接受 Provider 用 C 发起的调用，用 session JWT 一律拒绝。**存在性不可区分**：目标不存在、已过期、已撤销或属于其他 principal 时，响应与撤销成功相同，且对他人的对象没有任何效果。**成功的含义**：属于调用方的目标只有在撤销（含其换出的 JWT）持久生效后才返回成功；执行或持久化失败返回错误，超时或断连按结果未知处理，可幂等重试；成功不构成目标存在或归属的证明，宿主按自己的签发记录判断归属（Grok、Codex 讨论一致）。view 实例销毁时撤整族；撤销有时限、不阻塞销毁，失败时残留 JWT 至多活到 `exp`，且 `view_secret` 已丢弃、无法使用。同时泄露 JWT 与 view 证明时，两者在 `exp` 前可被冒用（所以 U2.5 禁止记录二者）。
- **U3.8** **UI 路径上的 401**（都不由本文递增 generation；未列出的结果按"超时或 5xx"处理，fail closed）：
  - 只有**经过注入**的页面请求得到 401 才触发自查；每个 view 实例单飞、至多每 10 秒一次。JWT 已到或接近到期时直接续期，否则调会话探测。
  - 探测 `credential_expired` → 续期，不销毁 view。探测 `credential_revoked` → 不据此判定撤权，由 Provider 用当前 C 签一次票，签发响应按 U2.14；签票成功即续期；同一 binding 10 分钟内第 3 次"探测得 `credential_revoked` 而签票成功" → `closed/revocation_mismatch`。探测 `credential_invalid` → view 级处理。探测成功 → 无动作（若此时正因探测失败暂停，按下一条解除）。
  - 兑换 `credential_invalid` → 第一次改铸新票，连续第二次做 view 级处理；`credential_expired` → 先续 C 再重铸；`credential_revoked` → 由 Provider 用当前 C 签票，按 U2.14；`forbidden/empty_scope` → 见末条。
  - 探测或兑换超时、5xx 或未知 `reason` → 暂停注入并退避重试（兑换不重发同一票据）；连续 3 次 → `closed/probe_failed`；其间任一次得到其他结果，即解除暂停、计数清零，并按该结果处理。
  - 同一 binding 10 分钟内由 `credential_invalid` 触发的第 2 次 view 级处理不再自动重建 → `closed/session_rejected`，等用户操作，profile 服务状态不变；崩溃、Off→On、端口变化与通道恢复引起的重建不计。
  - 当前 C 的签发或兑换得到 `forbidden/empty_scope`：不建 view；已有 view 销毁且不重建，显示"没有可见的知识库"（`closed/empty_scope`）。
- **U3.9** 服务端不校验 `gen`（R7.5）；generation 由宿主执行：桥拒绝旧 generation 的消息（U6.4 ⑭），generation 递增时 binding 级销毁（U4.8）。

## 5. view 实例、注入与销毁

- **U4.1** 每个 view 实例的分区不持久，不跨 binding 共享存储；分区名不由 profile 名、用户路径、URL 或明文 principal 生成。
- **U4.2** **页面能力下限**：无 Node、无 preload、`contextIsolation` 与 `sandbox` 开启；拒绝 `window.open`、跨 origin 导航、`file://`、自定义协议与下载；不授予任何权限、设备选择、屏幕共享，不自动出示任何客户端证书（不论事件来自 app 还是 view）；生产构建无 DevTools、无远程调试、无 Node 调试与 `RunAsNode` 入口。CSP 下限：资源与连接只来自允许 origin（含其 ws/wss），禁止任何子 frame、Worker、`object`、`<base>`，表单只能提交到允许 origin，`frame-ancestors 'none'`。WebRTC 不暴露非代理 UDP，残余风险见决定 #30。
- **U4.3** Service Worker 不得生效：一旦出现注册，立即注销并清除其存储。
- **U4.4** 允许 origin：remote 只能是可信 profile 配置里的 `https`，证书错误一律拒绝且没有放行入口；`http` 只允许 local-managed 握手记下的 loopback（`127.0.0.1` 或 `[::1]`）；允许 origin 不取自页面或服务端响应。main 自己的兑换与探测请求同样受限。
- **U4.5** **注入**：宿主先剥除每个请求自带的 `Authorization` 与 view 证明头（大小写不敏感），再只为同时满足下列条件的请求注入 JWT 与 view 证明：① 来自当前 view 实例的主 frame；② 是 fetch/XHR、WebSocket 或白名单内的主文档导航；③ origin 与允许 origin 完全一致（`http`↔`ws`、`https`↔`wss`），且仍是握手时的值；④ 规范化后的 pathname 在 UI 路径前缀白名单内，原始路径不含 `%2f`、`%5c`；⑤ 注入未暂停、Provider 存活信号新鲜（≤1 秒），local-managed 的 sidecar ready 且握手有效。判断不了即不注入。①–④ 成立、只因 ⑤ 不成立的请求被取消，不无认证发出。子资源不注入；重定向逐跳重判，跨 origin 那一跳不带这两个头。首个 JWT 就绪后宿主才加载页面。**恢复即重载**：注入从暂停恢复时重新加载当前文档；只有当这次暂停的全部原因都是"当前 C 被替换"或"存活信号短暂不新鲜"时才不重载。
- **U4.6** 分区只在同一 binding 内复用；复用前清空存储、缓存、认证缓存与连接，任一步失败即不复用、本次不新建 view。进程内 KnowledgeView 分区至多 64 个，满时拒绝新建并提示重启应用。
- **U4.7** **销毁 view 实例**按序：① 关桥；② 停止注入，丢弃 `view_secret`、票据与 JWT；③ 撤族——Off、停用 profile、注销、取消选用时 Provider 先用 C 尝试撤族（时限 2 秒）再丢弃 C；C 已不可用则跳过并记审计；④ 销毁页面；⑤ 清理分区。①② 先于其余步骤完成；③–⑤ 出错不得重开桥或恢复注入。
- **U4.8** **何时销毁或暂停**：
  - **binding 级**：generation 递增（S4.2 全部事件，决定 #6）；binding 进入 `retired`；应用退出。多个 binding 同时激活时（S4.9）只销毁事件所属 binding 的 view；Off 作用于全部。Off 期间不新建 KnowledgeView，入口标明不可用及原因。
  - **跨进程不变式**：STATE S4.5 作废生效时，main 已对该 binding 停止注入并关桥，Provider 等待 main 确认至多 2 秒；超时则照常继续，并在 C 可用时按 `vph` 撤销该 binding 的全部在用族，且保证 main 在作废通知发出后 3 秒加本机通道时延内停止注入。main 失去 Provider 存活信号（通道断开或 2 秒无信号）时，自行对全部 binding 的全部 view 停止注入、关桥并销毁；恢复后按 view 级重建，不计入 `crash_loop` 与 `session_rejected`。系统休眠期间不判丢失，唤醒后 5 秒内不全局销毁，但拿到新信号前不注入。
  - **view 级处理**：渲染进程崩溃（退避重建，不重启健康的 sidecar；同一 binding 10 分钟内超过 3 次 → `closed/crash_loop`）；主 frame 落到允许 origin 之外；探测得 `credential_invalid`；关桥后用户重新加载；local-managed sidecar 重新握手完成。
  - **只暂停注入**：`loading`、`unavailable`（含不递增的探测转入（S3.4）与凭据无效类续签也失败（S3.1））；准入未开放（`persist_pending` 为真或待复验未解除）；当前 C 被替换尚未换发。`degraded`、隐藏与再显示、JWT 续期都不暂停。
  - **凭据无效类续签也失败**（S3.1，`unavailable/credential_rejected`，决定 #33）：暂停注入，不关桥、不销毁；桥回 `not_ready`；宿主在 view 上绘制不透明遮罩"不可用，需要重新授权"，页面内容不可见、不接收输入。回到可服务且 generation 未变（同一 principal 重新登录，或手动重试，S1.4）时撤遮罩、换发 JWT、按 U4.5 重载；确认撤权或其他递增事件 → binding 级销毁。
  - **sidecar**：进程退出事件到达、离开 ready、重启或重新握手时，旧实例立即停止注入；端口变化时先关桥、停止注入，再更新允许 origin。一次重新握手完成后**只做一次** view 级重建：它同时满足遮罩恢复或端口变化时，这次重建取代遮罩撤除与原实例恢复，不计入任何关桥或重建上限。
  - 宿主删除单个 KB（S5.10，不递增）不处理 view。
- **U4.9** **长期登录态的归属与作废**：
  - 登录态只属于一个 binding；`retired` binding 的登录态不得用于任何其他 binding（含同一身份元组后来新建的，S2.6、S2.7）；STATE 记为已注销的 binding，其登录态一律不用。
  - 登录先于 binding 建立（未绑定占位，或带 `unsaved_settings` 时经确认后重新登录建立，S1.13）时，登录态在 binding 初值持久化确认前是临时的；建立失败立即按 U4.11 销毁；崩溃残留的临时登录态在下次启动时按 U4.11 销毁。
  - 已标为不可用的登录态（U4.11 ①）跨重启仍不可用，不用于续签，直到被销毁或被同一 binding 的重新登录替换。
  - **同一 binding 重新登录**（注销后，S3.3；`credential_rejected`/`access_revoked` 后，S1.4）：旧登录态的撤销材料已持久保存之后，新登录态才能替换旧的并清除不可用标记；保存不了时本次登录按失败处理（新登录态撤销、不保存）。已在进行的旧撤销照常重试，不影响新登录态。
  - **全局收紧或持久状态损坏**（S1.14）：全部 Knowledge 登录态作废——不用、不续签、暂不销毁；该 profile 解除 `unsaved_settings` 并建立的新 binding（S1.13）持久化之后，或该 profile 被移除时，按 U4.11 销毁其作废条目。登录态属于哪个 profile 由宿主独立记录，不依赖 STATE 持久状态；所属 profile 已不存在的按"profile 被移除"处理（决定 #34）。
  - 宿主必须能完整列举自己保存的 Knowledge 登录态；无法确认完整性时，全部按失效处理并要求重新登录。
- **U4.10** 销毁登录态：注销（S3.3）；binding 因换实例、principal 变化、改端点或租户（S3.5）、移除 profile（S3.3）进入 `retired`。**不销毁**：Off（S5.3）、取消选用（S3.3）、停用 profile（S3.3）（决定 #7）。
- **U4.11** **销毁步骤**：① 先标为不可用，内存立即生效并持久记录；② 按 U4.7 销毁该 binding 的全部 view 实例；③ remote：先持久保存撤销材料，再调签发权威撤销（有时限），失败则持久地有限次重试，到期丢弃并记审计；④ 撤销材料保存成功后才删除登录态；不可用标记与撤销材料都无法持久时，删除登录态、放弃远端撤销；⑤ 记录结果。远端撤销未确认成功时，界面写明"服务端登录在过期前可能仍有效"。local-managed 没有注销（S2.1），`retired` 时只删宿主为它保存的凭据材料，实例秘密由 KB-LITE-01a、KB-LITE-04② 管理。
- **U4.12** 销毁 view 实例或登录态都追不回已显示或交给模型的内容（R7.7）；界面与审计不得声称可以。

## 6. 桥通道与宿主判定

- **U6.1** 通道机制由 KB-C2-04a 验证，必须满足：(a) main 从通道元数据（不是消息内容）得知发出方的 WebContents、frame 与文档；(b) 页面经通道只能到达桥的处理函数，通道不代发请求、不读文件、不开 URL；(c) 能识别是否主 frame，仅用于违规检测，不是安全边界（隔离靠 CSP）；(d) 解析前已知原始字节长度并能在传输层拒绝；(e) 宿主→页面的消息只送达当前文档主 frame；(f) 能观察该 view 内的激活类用户输入（按下、点击、触摸，不含移动、悬停、滚动）。做不到 (a)(b)(d)(e) 时桥不开放，KnowledgeView 只显示（决定 #14）。宿主→页面的消息对页面内任何脚本可见、可被页面伪造给自己；它们不含秘密，页面不得据此做安全决策。
- **U6.2** 页面→宿主只有 `status`、`citation`、`cancel`、`return_to_chat_intent`；宿主→页面只有 `hello`、`state`、`reply`。没有交付票据或凭据的消息；宿主→页面不含 URL、路径与令牌。`closed` 状态由宿主界面显示，不经桥。
- **U6.3** 主 frame 每提交一个新文档（含重载），宿主清空该文档的去重表、在途表与待发队列，并向新文档发 `hello`。页面可发 `generation: 0` 的 `status` 作 hello 请求，宿主回 `hello`、不计违规，每个文档至多 3 次，之后按 `stale_generation`。hello 请求在 ⑫ 之后、⑬ 之前处理，所以桥未开放时也能拿到只含非秘密信息的 `hello`（S5.9 的例外，§8.2）。
- **U6.4** 宿主对每条页面消息按序判定，取第一个不成立的：① 发出方是存活 view 实例的 KnowledgeView 且桥未关；② 主 frame；③ 已提交文档的 origin 等于允许 origin；④ session 是该实例的分区，U4.2、U4.3 成立；⑤ 是当前文档；⑥ 到达速率未超限（只看传输层，不解析）；⑦ 原始长度未超限；⑧ 合法 UTF-8 JSON、无重复键、嵌套/数组/字符串未超限；⑨ 任意深度无禁止键；⑩ 自由文本通过内容检查（v1 无自由文本，恒成立）；⑪ `v` 存在且受支持（缺失落到 ⑫）；⑫ 符合 schema 且 `direction` 为 `page_to_host`（hello 请求在此回 `hello` 并结束）；⑬ binding 准入开放、可服务（待复验未解除视为未开放）且存活信号新鲜；⑭ `generation` 等于当前值；⑮ `id` 不在去重表；⑯ 未超在途上限。
  - ①–⑤ 不成立：静默丢弃，不回 `reply`，按发出方计数、采样记日志，跟踪的发出方数量有上限；②③ 的来源在 view 内部时同时记硬违规。⑥ 超限：每个限速窗口只回一次 `overloaded`，其余静默。⑦–⑯ 不成立：回 `reply`（`rejected`，码见 §9.3）。
  - 只有通过 ⑯ 的 `id` 才登记进去重表。任何一步不成立，消息都没有任何效果。
- **U6.5** sender、frame、origin、文档身份只取自传输层元数据；报文自报 `origin`、`sender`、`frame`、`webcontents` 等即视为伪造，按禁止键处理。
- **U6.6** **违规与关桥**：`overloaded` 是背压，本身不算违规；发出 `overloaded` 后、在宽限期（至少 500 毫秒）之后且 `retry_after_ms` 之内又到达 3 条受同一限额约束的消息，记一次软违规（窗口不重叠）。**软违规**：⑦⑧⑫⑭⑮ 不成立（⑫ 只因引用 ID 默认禁止规则不成立的除外）；⑪ 不成立单独计数，10 秒内 5 次记一次软违规。⑬ 不成立只拒绝，不计任何违规。同一 view 实例 10 秒内 5 次软违规即关桥。**硬违规**：⑨ 不成立（先回 `forbidden_key` 再关桥），或 ②③ 来源在 view 内部（静默后关桥）；一次即关桥并记审计 `knowledge.bridge_violation`（不含消息内容）。关桥是 view 级处理，不递增 generation；KnowledgeView 进入 `closed/bridge_closed`，用户点重新加载才新建实例；同一 binding 10 分钟内第 3 次因违规关桥后不再提供重新加载，直到从 Knowledge 设置重新打开（决定 #17）。端口变化、通道恢复、binding 级销毁引起的关桥不计。
- **U6.7** **上限**（工程值，可按实测调整，只能取更严或在 PR 说明理由）：到达速率持续 10 条/秒、突发 20 条；单条 ≤8 KiB；嵌套 ≤4 层（根对象为第 1 层）；数组 ≤32 项；字符串 ≤2 KiB；处理中（含排队）≤4 条，`cancel` 不占名额，其中 `return_to_chat_intent` ≤1 条。宿主→页面单条 ≤32 KiB，`hello`+`state` ≤5 条/秒且排队的 `state` 只保留最新一条；`reply` 不合并不丢弃，每条通过 ⑥ 的消息至多两条 `reply`（`accepted` 加一个终态）。去重表每个文档 ≤4096 项，按 FIFO 淘汰，被淘汰的 `id` 再出现时当新消息处理（或拒收早于最老保留项的 ULID，回 `duplicate_id`）。
- **U6.8** 宿主日志与审计只记 `type`、`code`、`reason`、`id`、H("kb-view-v1"; view 实例标识) 与判定步骤，不记消息体、票据、JWT 与 view 证明。

## 7. 消息 schema

- **U7.1** **信封** `{ v, direction, type, id, generation, in_reply_to?, payload }`，每层都不允许多余字段，除注明外必填。`v`：v1 恒为 1。`direction`：`page_to_host` / `host_to_page`，与 `type` 不配即 `schema_violation`。`id`：ULID（`^[0-9A-HJKMNP-TV-Z]{26}$`），文档内唯一。`generation`：1 至 2^53−1，只有 hello 请求可为 0。`in_reply_to`：只能出现在 `reply` 里；触发消息通过 ⑧ 且顶层 `id` 是合法 ULID 时必填并取该 `id`，否则省略（⑥⑦⑧ 一律省略），宿主不编造占位值。
  - **引用 ID**（`knowledge_base_id`、`knowledge_id`、`chunk_id`、`source_revision`、`index_build_id`）：UTF-8 1–128 字节、不含控制字符，否则 `schema_violation`（软违规）；另默认拒绝含 `/`、`\`、`:`、`..` 或空白的 ID，回 `schema_violation` 但不计违规（与 R5.2 同步前先行采用，决定 #21）。宿主只把它们作为不透明值放进 READONLY 的 read，从不当 URL 或路径解释。`chunk_revision` 为 0 至 2^31−1 的整数。
- **U7.2** **页面→宿主的 payload 与处理**：
  - `status`：`state` ∈ `loading`/`ready`/`error`；`error_code` 只在 `error` 时出现，∈ `unauthorized`/`forbidden`/`not_found`/`unavailable`/`internal`。只用于宿主显示，不触发续期、登录、自查或任何 STATE 事件。
  - `citation`：六个引用字段全部必填。宿主经 Provider 以当前 generation 做带 `chunk_id` 与 `expected_revision` 的单分块 read（R4.1、R4.7），通过才在引用 UI 显示；不进模型上下文、不进草稿。**存在性不可区分**：read 的 Q 只取引用里的 KB，且该 KB 必须在 binding 最近一次可见集合内（S5.10），否则不发 read，直接回 `done`、`code: "not_found"`，与文档不存在相同。read 失败时 `done` 的 `code` 取 READONLY 的值（如 `stale`、`not_found`）。
  - `cancel`：`target_id`（ULID）。目标是本文档内仍在处理、排队或等待确认的 `citation` / intent 时撤回它：原消息的终态为 `rejected`（`code: "cancelled"`），确认弹窗随之关闭；`cancel` 自身回 `accepted`。目标不存在、属于其他文档或已到终态时回 `accepted`，没有任何效果。
  - `return_to_chat_intent`：`citations` 为 0–8 项引用。只在该 view 最近 5 秒内有激活类用户输入时接受，否则 `forbidden/user_activation_required`；对话内草稿附件累计至多 16 项，超出 `invalid_request/attachment_limit`。接受后宿主只高亮"回到对话"或弹出确认，不自动切换；确认界面列出条数与每条标题，标题由宿主重新 read 取得（纯文本，去控制与 bidi 字符，截断到 120 字符），不用页面给的文字。等待确认至多 60 秒，超时 `forbidden/confirmation_timeout`；用户拒绝 `forbidden/user_declined`，此后 30 秒内的 intent 回 `forbidden/cooldown`。等待确认期间文档切换、view 销毁（含关桥）、binding 离开可服务或准入关闭时，关闭弹窗、释放名额、不加附件，回 `unavailable/not_ready`（前两种情形这条 `reply` 随待发队列清空，不送达）。用户确认后才切回主对话，引用逐项按 `citation` 的方式校验，通过的项**一次性**加入草稿附件——**只有这条路径产生草稿附件**。部分通过时只附加通过的项并回 `done`，`code` 取第一个未通过项的 READONLY 值；全部通过时 `done` 不带 `code`；全部未通过时不加附件，回带该 `code` 的 `done`。**撤回优先**：附件加入之前收到 `cancel` 的 intent，不加任何附件，终态只有 `rejected/cancelled`，部分通过规则不适用；附件加入后 intent 已是 `done`，之后的 `cancel` 按"已到终态"处理。不自动发送、不创建 run、不做审批；v1 不带自由文本（决定 #15）。
  - **桥预算**：`citation` 与 intent 触发的 read（含标题 read）走 Provider 为桥单设的低优先级预算，至多 2 个并发、其余 FIFO 排队，不占主对话知识调用的预算与并发（与 KB-C1-04 的接口）；排队超过 10 秒仍未开始回 `unavailable/overloaded` 并带 `retry_after_ms`。这些 read 是普通 READONLY 请求，响应按 STATE 照常处理。
- **U7.3** **宿主→页面的 payload**：`hello`：`supported`（整数数组，v1 为 `[1]`）、`view_instance_id`（`^[A-Za-z0-9_-]{22}$`，随机，不同于 `view_secret`，不是秘密）。`state`：`profile_state`（STATE 的状态之一）、`code`（可选，`^[a-z_]{1,64}$`，取 STATE 的本地 `reason`）、`theme`（`light`/`dark`）、`locale`（BCP 47）。`reply`：`result`（`accepted`/`rejected`/`done`）及可选的 `code`、`reason`、`retry_after_ms`、`supported`。宿主发出前同样做禁止键检查，不通过就不发并记日志。
- **U7.4** **禁止键**在任意深度生效：键名转小写、去掉 `_` 与 `-` 后与清单项精确相等才算命中，不做子串匹配。清单：`url`、`uri`、`href`、`path`、`file`、`dir`、`cwd`、`command`、`cmd`、`exec`、`executable`、`argv`、`args`、`shell`、`script`、`approval`、`approve`、`decision`、`token`、`jwt`、`ticket`、`secret`、`password`、`authorization`、`cookie`、`origin`、`sender`、`frame`、`webcontents`、`port`、`__proto__`、`constructor`、`prototype`。解析器不给 `__proto__` 等键特殊语义。桥 schema 自身不得定义任何禁止键。内容检查（含 `/`、`\`、`://` 或以 `~`、`.` 开头即 `forbidden_content`，硬违规）只用于自由文本字段，v1 两个方向都没有。

## 8. 接口与交接点

### 8.1 与 READONLY r8
- 引用：R1.2、R1.4（UI 会话密钥）；R1.7（C 不进 renderer）；R2.1、R2.3、R2.6（E 的计算、空范围、越界与不存在不可区分）；R2.7（不含下载与 preview）；R4.1、R4.7（引用 read）；R5.2（引用 ID 不透明）；R6.1（错误信封）；R7.3（`null` 回显按 HTTP 交换配对，适用于签发入口）；R7.5（generation 只回显）；R7.6（在途长请求，U3.4 补上 UI 长连接的关闭）；R7.7；R10.3。
- 不修订 READONLY 的：票据失败与族撤销都复用 `credential_invalid`（决定 #5、#20）。
- 需要 READONLY 下一修订的：新 `reason` `unavailable/credential_near_expiry`（可重试，决定 #29）；UI 操作清单与 C 的数据操作范围的关系，以及 C 可用于票据签发与撤销（决定 #25，阻塞 KB-C2-01 实现）；引用 ID 字符集（决定 #21）。

### 8.2 与 STATE（草案）的交接
- **binding 状态**：可服务 = `ready`、`degraded`；`loading`、`unavailable` 只暂停注入（U4.8）；`retired` 与 generation 递增（S4.2）触发 binding 级销毁；多个 binding 各自处理（S4.9）；凭据无效类续签也失败（S3.1）按决定 #33。
- **generation**：签票在同一次准入里读取（S4.6）；作废生效时（S4.5）完成停止注入与关桥（U4.8）；服务端只回显。
- **桥准入 S5.9**：拆为 U6.4 ⑬（准入开放、可服务、存活信号新鲜，待复验未解除视为未开放）与 ⑭（generation）；S4.5 作废生效之后桥已关闭，消息静默丢弃。**STATE 需补**：hello 请求例外（U6.3）。
- **S3.1**：签发入口的响应按凭据归属处理（U2.14）；兑换与会话探测的响应不进入撤权判定（U2.7）。**STATE 需补**：S3.1 写明这两类响应除外；S4.6 的接受判定覆盖签发请求；旧 C 撤权信号的待复验 Q 取该 C 的范围（U2.14）。
- **S5.10**：签发入口的 `forbidden/empty_scope` 以所用 C 的范围为 Q 判定（只看当前 C）；桥 `citation` 依赖"最近一次可见集合"（U7.2）。
- **登录态**：S2.7 的销毁交给本文（U4.9–U4.11）；S1.14 全局收紧只作废；S5.3 Off 不清登录态。**STATE 需配合**：Off、停用、取消选用、注销时 Provider 先用 C 撤族再丢弃 C（U4.7 ③）。

## 9. 已定决定与枚举

### 9.1 已定决定（编号沿用完整版 §10）
- #1 票据 30 秒/最长 60 秒；JWT 10 分钟/最长 15 分钟且不晚于 C；C 剩余不足 60 秒拒签；约 2/3 寿命续期。
- #2 view 证明作纵深防御，`vph` 兼作族标识。
- #3 UI 路径的 `credential_invalid` 自动重建一次，10 分钟内第 2 次 `closed/session_rejected`。
- #4 由宿主兑换并注入，页面看不到票据与 JWT。
- #5 票据失败对兑换方复用 `credential_invalid`，不新增 `ticket_invalid`。
- #6 generation 每次递增都销毁 view 实例（含撤权类事件（S3.1）后重签）。
- #7 Off、取消选用、停用 profile 不销毁长期登录态。
- #14 无 preload 通道做不到 U6.1 (a)(b)(d)(e) 时桥不开放。
- #15 `return_to_chat_intent` v1 不带自由文本。
- #16 页面→宿主四种类型，宿主→页面 `hello`/`state`/`reply`，信封按 U7.1。
- #17 关桥后由用户重新加载；10 分钟内第 3 次后不再提供重新加载。
- #19 v1 不加 `reveal_citation`。
- #20 族与 `jti` 撤销复用 `credential_invalid`。
- #26 服务端在响应头回显 `reason` 是可选义务，v1 不依赖。
- #31 恢复与存活策略按 U4.5、U4.8 的数值，可按实测调整。
- #33 凭据无效类续签也失败（S3.1）：只暂停注入并加遮罩，不关桥、不销毁。
- #34 全局收紧作废的登录态，在该 profile 解除 `unsaved_settings` 重新绑定（S1.13）后或 profile 移除时销毁。

### 9.2 宿主侧 KnowledgeView `closed` 的 `code`（闭集）
`session_rejected`、`revocation_mismatch`、`probe_failed`、`empty_scope`、`crash_loop`、`bridge_closed`。

### 9.3 `reply` 的拒绝码（`code` 沿用 READONLY 闭集，新 `reason` 只在桥上使用）
| 来源 | `code` / `reason` | 违规 |
|---|---|---|
| ⑥ 到达超速；⑯ 在途已满；桥 read 排队超时 | `unavailable` / `overloaded`（带 `retry_after_ms`） | 不计（无视另计，U6.6） |
| ⑦ 超长 | `invalid_request` / `too_large` | 软 |
| ⑧、⑫ 格式或 schema 不符 | `invalid_request` / `schema_violation` | 软（只命中引用 ID 默认禁止规则时不计） |
| ⑨ 禁止键 | `invalid_request` / `forbidden_key` | 硬 |
| ⑪ 版本 | `invalid_request` / `unsupported_protocol_version`（带 `supported`） | 单独计数 |
| ⑬ 未准入、不可服务、存活信号不新鲜；等待确认时离开可服务 | `unavailable` / `not_ready` | 不计 |
| ⑭ generation 不符 | `invalid_request` / `stale_generation` | 软 |
| ⑮ `id` 重复 | `invalid_request` / `duplicate_id` | 软 |
| intent 无用户输入 / 附件超限 | `forbidden` / `user_activation_required`；`invalid_request` / `attachment_limit` | 不计 |
| intent 确认超时 / 用户拒绝 / 冷却期 | `forbidden` / `confirmation_timeout`、`user_declined`、`cooldown` | 不计 |
| 被 `cancel` 撤回 | `cancelled`（READONLY 本地码，不上线） | 不计 |

### 9.4 服务端审计 `internal_reason`（兑换失败，不对外）
`not_found`、`consumed`、`expired`、`revoked`、`family_revoked`、`proof_mismatch`。

### 9.5 仍待决（按 v1 默认执行，不得自行放宽；编号沿用完整版 §10）
- #8、#9 宿主内部谁铸票、私有接口形式、登录态存放（KB-HOST-01；涉及内核接口须知会 jason）。#11 签发权威撤销刷新令牌的接口（#992）。
- #21 引用 ID 字符集与 R5.2 同步（READONLY 下一修订）。#25 UI 操作与 C 数据操作范围的关系（READONLY 下一修订，阻塞 KB-C2-01 实现）。#29 新 `reason` `credential_near_expiry`（READONLY 下一修订）。
- #22–#24、#27、#28、#30、#32 工程值与实测项（intent 窗口与预算、分区上限、`worker-src`、CSP 放宽、服务端记录持久化、WebRTC 残余风险、Node `https` 信任库与代理），由 KB-C2-01/03/04a 实测后定。

## 10. 非规范材料

完整版（含转移表、验收用例、参考设计）保留在分支 `ab/weknora-03b-ui-bridge`（#1077 的分支），由实现任务以测试证明，不约束本契约。
