# Knowledge UI 身份交换与消息桥契约 v1（CONTRACT-UI-BRIDGE-v1）

> **状态**：**草案（Draft）**，待 fork 侧维护者（David）评审（文档 PR，base `ab/weknora`；依赖 STATE 草案的 #1070–#1072，合并顺序在其后）。评审通过前不授权任何实现。
> **版本**：契约 v1（桥报文字段 `v: 1`），文档修订 r8 · **日期**：2026-10-20 · **作者**：David Xu
> **Issue**：#1013 ②（KB-CT-03），阶段 #1000；消费方 KB-C2-01、KB-C2-03、KB-C2-04a、KB-C2-04b，另供 KB-HOST-01、KB-LITE-04① 引用。
> **依据**：任务方案 v2.1（方案全文见 #986 第一条评论）；`docs/knowledge/CONTRACT-READONLY-v1.md`（下称 READONLY，按 r7）；`docs/knowledge/CONTRACT-STATE-v1.md`（下称 STATE 草案，按 r14，PR #1070–#1072；**尚未合入**，本文依赖它的规则都登记在 §10 #12，它定稿时逐条复核）；ADR-004（`docs/open-design-workspace/design/ADR-004-DESKTOP-HOST.md`，"ADR-004:117" 指其第 117 行）。
> 来源标签沿用 READONLY：决策 `[D1]`–`[D8]`、任务条目（如 `[KB-C2-01]`）、READONLY 规则（如 `[R1.7]`）、STATE 草案规则（如 `[S5.9]`）、ADR-004 行号（如 `[ADR-004:117]`）。`[新设]` 表示本文新设计、在以上来源里都找不到依据的规则；与其他标签并列时，表示在该依据之上新增的部分。所有 `[新设]` 都待本 PR 评审（§10 #13）。
> **分片**：分两片提交，一起合入。第一片：§0–§5（身份、票据、session JWT、分区与销毁及其验收）、§9、§10 中除 #14–#19、#21、#22 以外的条目；第二片：§6–§8（桥、消息 schema、拒绝用例）与 §10 #14–#19、#21、#22。单独阅读第一片时，正文引用的桥规则（U6.x、U7.x，`hello`、`state` 消息，关桥）都在第二片。
> **现状证据**：Agent24 的 `file:line` 都按 `ab/weknora` 的 `eb1a58b`，路径相对仓库根目录，汇总在 §9。

## 0. 范围、边界与变更规则

### 0.1 本文冻结什么
KnowledgeView（Electron main 为一个 binding 创建的隔离 WebContentsView，承载 WeKnora 嵌入页）的三件事：宿主如何为它取得 UI 会话（一次性交换票据换 session JWT，由宿主兑换并注入，页面看不到）；页面与宿主之间的受限消息桥；binding 的长期登录态如何销毁（STATE 草案 S2.7 交给本文）。
本文只定语义，不定 HTTP 路径、Go/TS 类型与具体通道机制（KB-C2-04a 验证）。标 [新设] 的数值是工程值，可以按实测调整；放宽时要在 PR 里说明理由。

### 0.2 与相邻任务的边界
| 留给 | 内容 | 本文怎么引用 |
|---|---|---|
| READONLY | 访问凭据、有效范围 E、错误信封与 `code`/`reason`、generation 只回显（R7.5） | 直接引用；需要它补的话见 §10 #25 |
| STATE 草案 | binding 与 `binding_id`、多个 binding 同时激活（S4.9）、generation 的值与递增、撤权处理（T18、T35a）、凭据无效类处理（T42）、可服务状态、准入、桥的接受条件（S5.9）、崩溃安全不变式（S1.10 I1–I8）与重启语义（S1.14） | 引用，不改；本文不新增 generation 递增事件（§10 #12） |
| KB-C2-01（W） | 票据签发、兑换与撤销入口，会话探测入口，session JWT 签发，每个 UI 资源入口的校验，UI 资源路径前缀白名单，CORS 收紧 | 本文给语义与验收（§2、§3、§5） |
| KB-C2-02（W） | 嵌入页、内存 session adapter、首版只读 | adapter 在内存里，但不持有任何秘密（U3.5） |
| KB-C2-03（A） | KnowledgeView 的创建、分区、CSP 细则、导航、弹窗与下载策略、清理 | 本文定分区映射、CSP 下限、注入钩子与销毁步骤（§4） |
| KB-C2-04a / 04b（A） | 无 preload 通道与请求钩子的 PoC；桥产品化 | 本文定 PoC 要验证的项（U4.5、U6.1）与消息 schema（§7） |
| KB-HOST-01（A） | 系统凭据存储、C 放在哪里、谁铸造票据、main↔daemon 私有接口与跨进程作废 | 本文定私有接口的隔离要求、作废协议、登录态的键与销毁步骤（U1.5、U4.8–U4.11） |
| KB-LITE-04① | 远端切换与注销隔离、历史与引用的可见性 | 本文定页面会话与登录态的销毁步骤 |
| KB-C1-05 | 引用 UI、证据包裹 | 桥的引用消息只给定位，正文由宿主经 read 重新取得（U7.2） |

### 0.3 变更规则
沿用 READONLY §0.3。票据与 session JWT 的绑定项、寿命上限、兑换判定、注入条件、页面到宿主的消息类型闭集、拒绝类别都是语义，改动升主版本。宿主按闭集校验桥消息，未知字段即拒绝，所以**桥 schema 的任何改动（包括新增可选字段）都要递增 `v`**；宿主可以同时支持多个版本。

## 术语
| 术语 | 定义 |
|---|---|
| KnowledgeView / 页面 | Electron main 为一个 binding 创建的 WebContentsView（KB-C2-03），及其主 frame 当前已提交的文档（KB-C2-02）。不是 Agent24 自身的 renderer。 |
| 宿主 | Electron main 与 Provider（STATE 草案术语，持有 C 的一侧）的合称；内部分工由 KB-HOST-01 定。 |
| 服务端 | WeKnora fork：local-managed 为本实例，remote 为远端服务。 |
| 访问凭据 C | READONLY 术语表的"凭据"，带 `credential_id` 与到期时间。 |
| 交换票据 | 一次性、短寿命的不透明句柄，只能换一个 session JWT（§2）。 |
| session JWT | 嵌入页调用服务端 UI 接口时由宿主注入的短期凭据，受众与 C 不同（§3）。 |
| app instance | 一次 Desktop 安装；宿主首次启动时生成随机的 `app_instance_id` 并持久保存。 |
| view 实例 | 一个 KnowledgeView 从创建到销毁；每次创建都生成新的 `view_nonce` 与 `view_secret`（都用 CSPRNG；`view_secret` 至少 256 bit，放进请求头时用 base64url 编码）。崩溃重建、切换主体都产生新的 view 实例。 |
| 分区映射 | 宿主维护的表：分区名 ↔ 所属 binding、当前 view 实例（或"已销毁待复用"）（U4.6）。 |
| view 级处理 | 只销毁并重建 view 实例：换新的 `view_nonce` 与 `view_secret`、撤销旧族，**不递增 binding 的 generation**。 |
| view 证明 | 宿主附加在请求上的 `view_secret`（U4.5）。`vph` = H("kb-vph-v1"; `view_secret` 的原始字节)；请求头里放它的 base64url 编码，服务端先解码再按同一公式计算。`vph` 同时是 JWT 的族标识（U3.7）。 |
| 允许 origin | 握手时记下的精确 origin（U4.4）。 |
| 长期登录态 | 宿主无需用户重新登录就能取得新 C 的材料：remote 的刷新令牌或设备授权及其在签发权威一侧的会话；local-managed 见 U4.11。 |
| H(tag; x₁, …, xₙ) | 宿主与服务端按同一公式计算：SHA-256(len(tag) ‖ tag ‖ len(x₁) ‖ x₁ ‖ … ‖ len(xₙ) ‖ xₙ)，len 为 4 字节大端字节长度；输出无填充 base64url。 |

## 1. 角色与信任边界

- **U1.1** [D2/R1.1+新设] 票据与 session JWT 都由服务端签发。宿主即使按 R1.4 持有 local-managed 实例的专属秘密，也不得自己签发它们。
- **U1.2** [D5/R1.7+新设] **页面不可信**：它是第三方前端代码，会渲染知识正文，可能被注入。页面始终看不到 C、长期登录态、票据、session JWT、`view_secret` 与本地路径；它只知道 generation、不透明的 view 实例 ID，以及 `state` 消息里的非秘密信息（U6.2）。
- **U1.3** [D2+新设] **受众隔离**：C 的受众是集成端点，session JWT 的受众是嵌入 UI（`aud: "agent24-knowledge-view"`），服务端两边互不接受。session JWT 的范围是 C 的子集；它的 `ops` 是 KB-C2-01 定义的 UI 操作清单，不含下载原件与 preview（R2.7），不能换票据、签凭据或调用写入、管理接口。UI 操作清单与 C 的数据操作范围是什么关系、C 用于票据签发与撤销是否计入数据操作，都是 READONLY 的修订项（§10 #25，阻塞 KB-C2-01 的实现）；在 #25 定之前，KB-C2-01 不得实现超出 search/read 的 UI 操作。
- **U1.4** [R1.7/KB-HOST-01+新设] C 留在 Provider 一侧（agent24d 或系统凭据存储），不进任何 renderer。票据只经过 Provider（含 agent24d）、Electron main 的内存与两者之间的私有接口；session JWT 只存在于服务端与 Electron main 的内存里。
- **U1.5** [ADR-004:145/KB-HOST-01+新设] Agent24 自身的 renderer 不参与身份交换。main↔daemon 之间铸票、撤销、作废通知用的私有接口，必须走通用 `backendProxy` 到不了的通道或凭据：独立的 IPC，或 daemon 内只接受 main 专属令牌的路由（`backendProxy` 不携带该令牌）；也可以给 `backendProxy` 加路径白名单，把这些路由排除在外（§9 第 3 条、§10 #8）。加固 [新设]：私有通道建立时双向认证，daemon 回显 main 启动时生成的一次性 nonce，main 核对后才接受铸票、作废通知与心跳。

## 2. 一次性交换票据

- **U2.1** [D2/KB-C2-01+新设] **流程**：
  ① 宿主确认该 binding 处于可服务状态且准入开放，并在同一次准入里读取 generation（S4.6、S5.9）；
  ② Provider 用 C 调用服务端的票据签发入口，请求带 `binding_tag`、`partition_tag`、`vph`、`generation`；票据若经 agent24d 交给 main，走 U1.5 的私有接口；
  ③ 服务端按 READONLY §1、§2 校验 C 并计算 E（为空按 R2.3 拒绝）；C 的剩余寿命不足 60 秒时返回 `unavailable/credential_near_expiry`（可重试，新 `reason`，§10 #29），宿主先续 C 再重试，不把它当作服务不可用交给 STATE；签发后返回 `{ ticket, ticket_id, expires_at }`。签发入口的响应是 READONLY 类凭据响应。签票只发生在可服务状态（`ready` / `degraded`），签发请求记下所用 C 的代号（S3.1 的凭据归属）：签发在途期间 C 已被替换的，响应按 S3.1 的凭据归属处理（凭据无效类不触发 T42，撤权信号按"旧凭据的撤权信号"记待复验、不走 T18），这次签发作废，按 U3.6 用新 C 重新签发。用当前 C 发出的签发，其 `unauthorized` 按 S3.1 分流：撤权信号（如 `credential_revoked`）走 T18（递增、作废在途请求、写占位），`degraded` 也一样；凭据无效类（`credential_invalid`、`credential_missing`、未知 `reason`）走 T42（不递增、不占位，丢弃当前 C 并续签一次，续签也失败才按 T05 占位）；`credential_expired` 先续签；`forbidden/empty_scope` 交 STATE 按 S5.10 判定是否收窄（签发请求不带 Q，请求范围 Q 取 C 的范围），本次不建 view（U3.8）。响应到达时 binding 已离开可服务状态的（如刚经 T08 进入 `unavailable`），按 STATE 处理时的状态走（撤权信号走 T35a）；
  ④ Electron main 再检查一次：generation 仍是当前值、binding 仍可服务、view 实例未销毁；不通过就丢弃票据：generation 变了时按 U4.8 做 binding 级销毁，票据与族都撤销（U4.7 ③）；只是不可服务时只撤销这一张票据；
  ⑤ main 用不会自动重发的通道调用兑换入口：Node 的 `http` / `https` 模块（证书校验开启），不走 Chromium 网络栈；受 U4.4 的 origin 与 TLS 规则约束；票据放在请求体里，并附上 view 证明；
  ⑥ 服务端按 U2.6 判定、原子消费，把 session JWT 放在响应体里返回给 main；main 只把它放在内存，按 U4.5 注入。
- **U2.2** [D2+新设] **形式**：服务端生成的不透明随机句柄，熵至少 256 bit，服务端只保存其哈希。票据不是 JWT，绑定信息只在服务端记录里。`ticket_id` 只用于撤销与审计，不能兑换。
- **U2.3** [新设] **寿命与持久化**：默认 30 秒，最长 60 秒，只能兑换一次；签发与校验在同一服务端，不另加时钟容差。服务端的票据消费记录与撤销记录（U3.7）必须持久保存；做不到时，服务端每次重启都轮换 UI 会话密钥（local-managed 由实例秘密派生时，派生输入加一个重启纪元），让重启前签出的 JWT 全部失效。票据记录的持久程度不得高于消费与撤销记录：后者不跨重启保留时，票据记录也不跨重启保留，重启前签出的票据一律兑换不了，已消费的票据不能在重启后再兑换一次。多副本部署要求这些记录放在共享存储，或用粘性路由把同一 view 实例的请求固定到同一副本（§10 #28）。
- **U2.4** [D2/D5+新设] **绑定**（服务端记录）：principal、`tenant_id`、范围（签发时的 E，非空，R2.4）；C 的 `credential_id` 与到期时间；`binding_tag` = H("kb-binding-v1"; `app_instance_id`, `binding_id`)，覆盖 app instance 与主体边界（S2.4）；`partition_tag` = H("kb-partition-v1"; 分区映射里该实例的分区名)；`vph`；`generation`（只记录、不校验，R7.5）。WebContents 的绑定由宿主保证：票据与 JWT 只在宿主一侧，只注入给该 WebContents 主 frame 的请求（U4.5）；`vph` 是服务端可查的纵深防御（§10 #2）。
- **U2.5** [D2/ADR-004:123+新设] **不落地**：票据与 session JWT 不进 URL、任何浏览器存储、cookie 与页面可见的任何位置，也不进宿主日志、服务端访问日志、审计与崩溃报告。签发与兑换的请求和响应都带 `Cache-Control: no-store`。
- **U2.6** [D2+新设] **兑换判定**（服务端，按顺序取第一个不成立的）：票据存在 → 未被消费 → 未过期 → 未被撤销 → `vph` 所在的族未被撤销 → 请求带 view 证明，且对它做 base64url 解码后 H("kb-vph-v1"; 解码所得字节) 等于 `vph` → C 仍有效 → 重新计算的 E 非空。**消费是原子的**：在同一个事务里检查未消费并标记已消费（compare-and-set），之后判定成功与否，票据都已作废。兑换入口单独限速。
- **U2.7** [R6.1+新设] **拒绝**：用 READONLY R6.1 的信封。前六项失败对兑换方一律返回 `unauthorized/credential_invalid`，不可区分（复用 READONLY 已有值，不新增 `ticket_invalid`，不修订 READONLY；§10 #5 已定）。服务端审计用 `internal_reason`（`not_found` / `consumed` / `expired` / `revoked` / `family_revoked` / `proof_mismatch`）区分，记 H(`ticket_id`) 与 `request_id`。C 失效按 READONLY 返回 `credential_expired` / `credential_revoked`；E 为空返回 `forbidden/empty_scope`。**兑换入口与会话探测（U3.4）的响应不属于 READONLY 响应，不进入 STATE 的撤权判定**（U3.8）。
- **U2.8** [D2+新设] **重放与泄露**：`consumed`（已消费的票据再次提交），以及未消费票据上出现 `proof_mismatch`，都视为泄露：服务端按 U3.7 撤销该票据 `vph` 所在的整族 JWT，记安全审计；**不撤销 C**。
- **U2.9** [新设] **单飞**：每个 view 实例同一时刻至多一个兑换在途。兑换结果不明（超时、连接中断）时不重发同一张票据，改铸新票。服务端仍以 compare-and-set 兜底：并发兑换只有一个成功，其余按 `consumed` 处理（U2.8）。
- **U2.10** [新设] **过期**：返回 U2.7 的统一拒绝，不签发 JWT；宿主按 U2.1 改铸新票。
- **U2.11** [D5+新设] **分区被销毁**：宿主先丢弃 `view_secret` 与内存里的票据和 JWT（U4.7），再按 U3.7 撤销本实例的族。族撤销同时作废该 `vph` 下尚未兑换的票据（U2.6）；`view_secret` 已丢弃，这些票据也带不出正确的 view 证明。
- **U2.12** [S4.5/S4.6+新设] **② 与 ④ 之间状态变了**：generation 变了，票据不兑换，view 按 U4.8 做 binding 级销毁，票据与族都撤销；generation 没变但 binding 不再可服务（如 T08），票据不兑换、只撤销这一张，view 保留并暂停注入（U4.8）。
- **U2.13** [R10.1/R10.3+新设] 签发、兑换、拒绝与撤销各记一条审计：H(`ticket_id`)、principal、`credential_id`、`binding_tag`、`outcome`、`internal_reason`。不记票据值、JWT 与 view 证明。

## 3. session JWT

- **U3.1** [D2/R1.2/KB-C2-01+新设] 兑换成功时由服务端签发；签名密钥是服务端的 UI 会话密钥，与签发 C 的密钥分开，不是租户 HMAC（R1.2）；local-managed 下由本实例专属秘密派生（R1.4）。
- **U3.2** [新设] **声明**：`iss`、`aud: "agent24-knowledge-view"`、`sub`、`tenant_id`、`scope.knowledge_base_ids`（兑换时的 E，非空）、`ops`（首版只读 UI 操作，清单由 KB-C2-01 定）、`iat`、`nbf`、`exp`、`jti`、`ticket_id`、`parent_credential_id`、`binding_tag`、`partition_tag`、`cnf: { "vph" }`、`gen`（只记录）。
- **U3.3** [新设] **寿命**：默认 10 分钟，最长 15 分钟，`exp` = min(签发时刻 + ttl, C 的到期时间)。不发刷新令牌。
- **U3.4** [D2/R2.1/KB-C2-01+新设] **校验**：服务端在**每个** UI 资源入口校验签名（算法由服务端固定，拒绝 `none`，不按 JWT header 的 `alg`、`kid`、`jku`、`x5u` 选择算法或密钥）、`aud`、`exp` / `nbf`（时钟容差不超过 30 秒）、`jti` 与族未撤销、C 未过期未撤销、view 证明（base64url 解码后按同一公式）与 `cnf.vph` 相符，并按 R2.1 重新计算 E，资源须在 E 内（越界按 R2.6 不可区分）。失败返回 `unauthorized`，`reason` 区分宿主的三种处理（U3.8）：JWT 过期为 `credential_expired`；**C** 被撤销为 `credential_revoked`；`jti` 或族被撤销，以及签名、`aud`、view 证明缺失或不符，为 `credential_invalid`（复用 READONLY 已有值，不为族撤销另设新值；§10 #20 已定）。SSE、WebSocket 等长连接最多存活到建立时所用 JWT 的 `exp`，到期由服务端关闭；族、`jti` 或 C 被撤销时，服务端关闭该族已建立的 SSE 与 WebSocket，做不到的话长连接至少每 60 秒重新校验一次 [新设]。这是 KB-C2-01 的义务，对应 READONLY R7.6 指出的"在途长请求不因撤权中止"的缺口。错误体与响应头不回显 `Authorization` 与 view 证明头。KB-C2-01 另提供一个只认 session JWT 与 view 证明的会话探测入口，供 U3.8 使用。
- **U3.5** [KB-C2-02+新设] **页面里没有 JWT**：KB-C2-02 的内存 session adapter 在内存里维护会话状态，但不持有任何秘密；嵌入页发请求时不带 `Authorization`，由宿主注入（U4.5）。
- **U3.6** [新设] **续期**：由宿主发起。JWT 用掉约 2/3 寿命时，宿主按 U2.1 重新铸票兑换，换上新 JWT（同一 view 实例，同一族）；每次续期服务端都重新校验 C 与政策。**C 变化时换发** [新设]：宿主的当前 C 被替换（续签，或 T42 丢弃后取得新 C）时，宿主主动按 U2.1 重新铸票、换发 JWT（同一 view 实例，同一族，`parent_credential_id` 指向新 C），换发完成前暂停注入；只因 C 被替换而暂停的（如 binding 一直可服务的例行续签 T27），换发完成后直接继续注入，不重新加载当前文档（与 U4.5 的心跳中断相同），期间同时因不可服务而暂停的，恢复时仍按 U4.5 重新加载；续签本身不要求撤销旧 C，旧 C 是否被签发权威作废不影响这条。换发前因旧 C 失效得到的 401 按 U3.8 自查，但不计入 `revocation_mismatch`。
- **U3.7** [D2+新设] **族与撤销**：族 = 同一 view 实例下签出的全部 JWT，以 `vph` 标识。撤销入口按 `ticket_id` 只撤销这一张票据及它换出的 JWT（若已兑换），按 `vph` 撤销整族。族一旦撤销，同一 `vph` 的兑换与 JWT 一律被拒（U2.6、U3.4）；宿主不会收到通知，经 U3.8 的自查发现并自愈。撤销记录的持久化要求见 U2.3。撤销入口**只接受 Provider 用 C 发起的调用，且 C 的 principal 必须与被撤销对象的 principal 相同**；用 session JWT 调用一律拒绝。view 实例销毁（U4.7）时撤销整族（C 的丢弃时机见 U4.7 ③），撤销时服务端一并关闭该族的长连接（U3.4）；U2.1 ④ 因不可服务未通过时只撤销那一张票据。撤销有时限、在进程内重试，但不阻塞销毁，也不持久化：进程崩溃后残留的 JWT 最多活到它的 `exp`（≤15 分钟），而 `view_secret` 只在 main 内存里、随进程消失，没有它 JWT 用不了。view 证明防不了日志泄露：若某处日志同时泄露 JWT 与 view 证明，在 `exp` 之前两者可以一起被冒用（U2.5、U4.5 禁止记录二者）。
- **U3.8** [S3.1+新设] **UI 路径上的 401 与处置表**：webRequest 读不到响应体，宿主不能直接按 `reason` 分流。页面发起、**经过注入**的请求得到 401 时，宿主自己发请求读出原因：JWT 已到或接近到期时直接按 U3.6 续期，否则带当前 JWT 与 view 证明调用会话探测入口（U3.4）。自查在每个 view 实例上单飞，且至多每 10 秒一次；未经注入的请求（如暂停注入期间、白名单外路径）得到 401 不触发自查。兑换入口与会话探测的响应不进入 STATE 的撤权判定（U2.7）。按下表处置，**都不由本文递增 generation**；表外的结果一律按"超时、5xx 或未知"处理（fail closed）：

| 来源 | 结果 | 处置 |
|---|---|---|
| 探测 | `credential_expired` | 续期（U3.6），不销毁 view |
| 探测 | `credential_revoked` | 不据此判定撤权；由 Provider 用 C 铸一次票（U2.1 ②），签发入口的响应按下面"签发入口"两行处理；签票成功则继续续期。同一 binding 10 分钟内第 3 次出现"探测得 `credential_revoked` 而签票成功"时，进入 `closed`（`code: "revocation_mismatch"`） |
| 探测 | `credential_invalid` | view 级处理，撤销旧族 |
| 探测或兑换 | 超时、5xx 或未知 `reason` | 暂停注入，按指数退避重试（兑换不重发同一张票据，U2.9）；连续 3 次失败进入 `closed`（`code: "probe_failed"`）；重试得到表中其他结果即解除这次暂停并按该行处理，连续失败计数清零（恢复注入时按 U4.5 重新加载当前文档） |
| 探测 | 成功 | 不做任何事 |
| 兑换 | `credential_invalid` | 第一次按 U2.10 改铸新票；连续第二次做 view 级处理 |
| 兑换 | `credential_expired` | 先续 C，再重铸票 |
| 兑换 | `credential_revoked` | 由 Provider 用 C 签票，按"签发入口"两行处理 |
| 兑换 | `forbidden/empty_scope` | 不建 view；已有 view 时销毁且不重建，KnowledgeView 显示"没有可见的知识库"（`closed`，`code: "empty_scope"`） |
| 签发入口 | `unauthorized/credential_expired` | 按 S3.1 先续签 C，成功后重新签发（U2.1 ③） |
| 签发入口 | 其余 `unauthorized` | READONLY 类响应，按 S3.1 交 STATE：撤权信号走 T18（处理时已不可服务则走 T35a），凭据无效类走 T42；C 被替换后按 U3.6 换发（U2.1 ③） |
| 签发入口 | `forbidden/empty_scope` | 交 STATE 按 S5.10 判定；不建 view，已有 view 时销毁且不重建，显示"没有可见的知识库"（`closed`，`code: "empty_scope"`） |
| 签发入口 | `unavailable/credential_near_expiry` | 先续 C 再重试，不交给 STATE（U2.1 ③） |

  同一 binding 10 分钟内由 `credential_invalid` 触发的 view 级处理，第 2 次触发时不再自动重建：KnowledgeView 进入宿主侧状态 `closed`（`code: "session_rejected"`），等用户操作；profile 的服务状态不变。渲染进程崩溃、Off→On、端口变化与通道恢复后的重建都不计入。KB-C2-01 在 UI 响应头里回显 `reason` 是可选义务（§10 #26），v1 不依赖它。
- **U3.9** [R7.5/S5.9] 服务端不校验 `gen`（R7.5）。generation 由宿主执行：桥按 S5.9 拒绝旧 generation 的消息；generation 递增时销毁 view 实例（U4.8）。

## 4. 分区、注入钩子与销毁

- **U4.1** [D5/ADR-004:117+新设] **分区身份**：`agent24-knowledge-v1:<inst>:<bind>:<slot>`，`inst` = H("kb-inst-v1"; `app_instance_id`)，`bind` = `binding_tag`，`slot` 为分区首次创建时用 CSPRNG 生成的 128 bit 值。没有 `persist:` 前缀；不得用 profile 名、用户路径、URL 或明文 principal 生成；不从 `view_nonce` 推导。分区属于哪个 binding、当前对应哪个 view 实例，只按分区映射判断（U4.6）。
- **U4.2** [ADR-004:118/:120/:122/D5+新设] KnowledgeView 必须 `contextIsolation`、`sandbox`、关闭 `nodeIntegration`、不加载 preload（退路见 §10 #14）；`window.open`、跨 origin 导航、`file://`、自定义协议默认拒绝；v1 拒绝下载（R2.7）；**生产构建**禁用 DevTools（`devTools: false`）；不传 `--remote-debugging-port`；Electron fuses 关闭 `RunAsNode`、`EnableNodeCliInspectArguments` 与 `EnableNodeOptionsEnvironmentVariable`。**权限**：分区同时安装 `setPermissionRequestHandler` 与 `setPermissionCheckHandler`，一律拒绝（含 `media`、`geolocation`、`notifications`、`clipboard-read`、`openExternal`、`midi`、`display-capture`、`fileSystem` 等全部取值）；`setDevicePermissionHandler` 一律返回 false，`setDisplayMediaRequestHandler`、`setBluetoothPairingHandler` 一律不授予。**设备选择事件**不经过权限 handler（不调用 `preventDefault` 时 Electron 自动选第一个可用设备，§9 第 7 条）：KnowledgeView 的 `select-bluetooth-device` 与分区 session 的 `select-hid-device`、`select-serial-port`、`select-usb-device` 一律 `preventDefault()` 并以空选择回调取消。**客户端证书选择**同理（不调用 `preventDefault` 时 Electron 自动用证书库里的第一张证书，§9 第 7 条）：KnowledgeView 的 `select-client-certificate` 一律 `preventDefault()` 并调用 `callback()` 不传证书。WebRTC 不受 CSP 约束，Electron 34 也没有整体禁用 WebRTC 的开关：v1 对 KnowledgeView 调用 `webContents.setWebRTCIPHandlingPolicy('disable_non_proxied_udp')`，残余风险见 §10 #30。**CSP 下限**（只装在它的分区上，细则归 KB-C2-03）：`default-src` 只含允许 origin；`script-src`、`style-src`、`img-src`、`font-src`、`media-src` 限于该 origin（`data:` 与其他放宽是否需要，§10 #27）；`connect-src` 限于该 origin 及其 ws/wss 对应；`frame-src 'none'`、`child-src 'none'`、`worker-src 'none'`、`object-src 'none'`、`base-uri 'none'`；`form-action 'none'` 或只含该 origin；`frame-ancestors 'none'`。`worker-src 'none'` 会挡掉合法的 Web Worker（§10 #24）。
- **U4.3** [新设] **禁用 Service Worker**：靠 U4.2 的 `worker-src 'none'`；另监听分区 `serviceWorkers` 的 `registration-completed` 事件，一旦出现就注销并清除该分区的 service worker 存储（`clearStorageData`）。
- **U4.4** [ADR-004:121+新设] **允许 origin 与 TLS**：remote 的允许 origin 取自可信的 profile 配置，只能是 `https`；证书错误一律拒绝，不提供放行入口。`http` 只允许用于 local-managed 握手时记下的 loopback origin（IP 字面量 `127.0.0.1` 或 `[::1]`）。允许 origin 不取自页面或服务端响应。main 的兑换与自查请求、分区内的请求都受此约束。
- **U4.5** [ADR-004:123+新设] **注入**：分区的 `onBeforeSendHeaders` 钩子对**每个**请求先删除 `Authorization` 与 `X-Agent24-View-Proof`（头名大小写不敏感匹配），再只为同时满足以下全部条件的请求加上 `Authorization: Bearer <session JWT>` 与 view 证明头：① `webContentsId` 等于当前 view 实例；② 发出 frame 是主 frame（`details.frame` 为空等判断不了的情况不注入）；③ 资源类型为 `xhr`（含 fetch）或 `webSocket`，或是落在白名单内的主文档导航；④ URL 的 origin 与允许 origin 完全一致，比较时 `http` 对应 `ws`、`https` 对应 `wss`，并且注入前当场核对允许 origin 仍是握手时的值；⑤ 按 WHATWG URL 规范化后的 pathname 落在 KB-C2-01 公布的 UI 资源路径前缀白名单内，且原始路径不含 `%2f`、`%5c`（不区分大小写）；⑥ 注入未被暂停（U4.8），main 收到的最近一次 Provider 心跳不超过 1 秒，local-managed 的 sidecar 仍处于 ready 且握手会话有效。任一条件判断不了就不加（fail closed）。①–⑤ 都成立、只因 ⑥ 不成立而不能注入的请求，由分区的 `onBeforeRequest` 直接取消，不无认证发出 [新设]。只因心跳不新鲜而中断的注入不算 U4.8 的暂停：拿到新心跳后直接继续，不重新加载当前文档。`img` 等子资源不注入。每一跳重定向都重新判定，跳到其他 origin 的那一跳不带这两个头。KB-C2-04a 的 PoC 必须记录主文档导航（`details.frame` 是否为空）、SSE（EventSource）、WebSocket 升级与 download 的资源类型与是否经过钩子；不能归入 ③ 的类型拿不到注入，页面改用 fetch。**加载顺序**：首个 JWT 兑换成功、注入就绪之后，宿主才对 KnowledgeView 执行 `loadURL`；暂停后恢复注入时，宿主重新加载当前文档（只因 C 被替换而暂停的除外，U3.6）。
- **U4.6** [新设] **分区映射与复用**（Electron 34 的 Session 不能销毁，每个分区都留在进程内存里）：同一进程内 KnowledgeView 分区至多 64 个。新建 view 实例时：该 binding 有已销毁实例的分区，就清理后复用；否则总数未满 64 时新建分区；否则（64 个都还活着，或已销毁的分区都属于其他 binding）**拒绝新建**，提示用户重启应用。不跨 binding 复用。销毁与复用时依次调用 `clearStorageData`（全部 storages，含 service worker）、`clearCache`、`clearAuthCache`、`closeAllConnections`；任一步失败，该分区不再复用，本次也不新建 view（fail closed）。代价：内存随分区数增长到上限；复用时的隔离依赖这四步是否清干净（§10 #23）。
- **U4.7** [ADR-004:113/:117+新设] **销毁 view 实例**按顺序：① 关闭该实例的桥；② 停止注入，从内存丢弃 `view_secret`、票据与 JWT；③ 按 U3.7 撤销本实例的族：Off、停用 profile、注销、取消选用时，Provider 先用 C 完成撤族尝试（时限 2 秒 [新设]）再丢弃 C，超时也丢弃；C 已不可用（撤权后或已丢弃）时跳过这一步并记审计，靠 `view_secret` 已丢弃与 U4.8 (b) 兜底；④ 销毁 WebContents；⑤ 按 U4.6 清理分区。①② 同步先行；③–⑤ 出错不得重开桥或恢复注入。
- **U4.8** [S1.5/S3.2/S4.2/S4.5/ADR-004:108/:112+新设] **何时销毁**：
  - **binding 级**：generation 递增（S4.2 的全部事件；§10 #6 已定）。多个 binding 同时激活时（S4.9），一个 binding 的事件只销毁该 binding 的 view；Off 作用于全部 binding，逐个销毁。Off 期间不新建 KnowledgeView，入口与 S1.5 的广告描述一致，标明不可用及原因。**跨进程作废协议**：Provider 执行 STATE 草案 S4.5 第 ① 步时，同步通知 main"停止注入并关桥"（`binding_id`）；main 对该 binding 的全部 view 实例完成 U4.7 的 ①② 后回 ack；Provider 收到 ack 后才继续 S4.5 第 ② 步。ack 超过 2 秒未到时，Provider 照常继续第 ② 步（S3.2 不得无限等待），同时 (a) 用 C 按 `vph` 撤销该 binding 下所有在用的族（Provider 铸票时已知各实例的 `vph`），C 已不可用时跳过 (a) 并记审计；(b) 主动断开与 main 的私有通道并停发心跳，迫使 main 走下面的全局 fail closed。**心跳**：Provider 每 500 毫秒发一次，与写盘不在同一执行线程；main 在私有通道断开或 2 秒收不到心跳时，对全部 binding 的全部 view 实例停止注入、关桥并销毁，不依赖 Provider。通道恢复后按 view 级重建，这次重建不计入 `crash_loop` 与 `session_rejected`。**休眠**：`powerMonitor` 发出 `suspend` 时暂停心跳丢失判定；`resume` 后有 5 秒宽限期，不按心跳丢失做全局销毁，但注入仍要求心跳新鲜（U4.5 ⑥），拿到新心跳之前不注入。**注入窗口上界**（ack 超时路径）：设 Provider 在 t₀ 发出作废通知而 main 没有处理。Provider 最晚在 t₀ + 2 秒停发心跳，main 收到的最后一次心跳最晚是 t₀ + 2 秒发出、t₀ + 2 秒 + d 到达（d 为本机通道传输时延，main 用单调时钟按接收时间判定）；U4.5 ⑥ 要求心跳不超过 1 秒，所以注入最晚在 t₀ + 3 秒 + d 停止。在此之前若 (a) 的撤族已在服务端生效，旧 JWT 已被拒。这组数值见 §10 #31。U4.7 ③–⑤ 随后进行。binding 进入 `retired`（S1.6）与应用退出也按 binding 级销毁。
  - **view 级处理**：渲染进程崩溃（按指数退避重建，不重启健康的 sidecar，ADR-004:112；同一 binding 10 分钟内崩溃超过 3 次进入 `closed`（`code: "crash_loop"`），等用户操作）；主 frame 落到允许 origin 之外；U3.8 的第三种；关桥后用户重新加载（U6.6）；local-managed 重新握手完成后（见下）。
  - **只暂停注入、不销毁**：不可服务的状态（`loading`、`unavailable`，含不递增 generation 的 T08 与 T42）；准入未开放（`persist_pending` 为真，或待复验未解除，S3.1）；当前 C 被替换、尚未换发 JWT（U3.6）。**T42 续签也失败**（`unavailable`/`credential_rejected`，STATE 记占位但不递增 generation；§10 #33 已定）：暂停注入，不关桥、不销毁 view，桥按 U6.4 ⑬ 拒绝（`not_ready`），与其他暂停情形一致；宿主在 view 之上绘制不透明遮罩，写明"不可用，需要重新授权"（由宿主界面绘制，不经桥、不改页面，遮罩期间页面内容不可见、不接收输入）；回到可服务状态且 generation 未变（T38 以同一 principal 重新登录，或 T10 的手动重试、sidecar 重新握手）时撤下遮罩，按 U3.6 换发 JWT 后继续注入并重新加载当前文档，桥照常按 U6.4 接受消息；之后若确认撤权或发生其他递增 generation 的事件（如 T35a、T23），按 binding 级销毁；宿主删除单个 KB（S5.10，不递增 generation）不处理 view。桥按 S5.9 拒绝消息；恢复后继续注入并重新加载当前文档（U4.5）；`degraded` 属于可服务状态（STATE 草案术语表"连接阶段"，第 37 行附近），不暂停。local-managed 的 sidecar 进程退出事件到达时同步暂停注入；sidecar 离开 ready（不论原因）、重启或重新握手时，旧 view 实例立即停止注入。端口变化时先关桥、停止注入，再更新允许 origin，新握手完成后做 view 级处理；这次关桥不计入 U6.6 的 10 分钟 3 次。隐藏与再显示（ADR-004:108）、JWT 过期（续期）既不销毁也不暂停。
- **U4.9** [S2.5/S2.6/S2.7/S1.14+新设] **长期登录态的键**：登录发生在未绑定占位上（S2.5），或在带 `unsaved_settings` 的 profile 上经 T44 重新登录；这时登录态先以临时键 `pending:<login_attempt_id>` 写入系统凭据存储；binding 建立成功（初值持久化确认）后，改以 `binding_id` 为键写入并删除临时条目。登录成功但 binding 建立失败时，立即删除临时条目（remote 同时按 U4.11 ③ 撤销）；宿主每次启动清理所有残留的 `pending:` 条目（remote 先按 U4.11 ③ 写墓碑 `revoke:pending:<login_attempt_id>:<序号>` 并撤销，再删除），并重试 `revoke:` 条目（U4.11）。**注销标记**：U4.11 ① 的"不可用"同时写进键名索引（该键名标为已注销，索引与凭据存储不是同一故障源）；启动时带这个标记的条目一律按失效处理，不用于续签，并重走 U4.11 ③。STATE 记为已注销的 binding（I7、S1.14），其条目一律不用。**同一 binding 重新登录**（T02、T38）以该 `binding_id` 写入新条目时，在登记该键名的同一次原子索引更新里清除该键名的注销标记与"待删"记录（U4.11 ④）；该键名下的旧令牌还没有写进 `revoke:` 墓碑时（U4.11 ④ 墓碑写入失败那一支），先按 U4.11 ③ 把旧令牌写进墓碑，写成功才允许覆盖旧条目；写不成时不覆盖，本次登录按失败处理（新登录态立即撤销、不保存，与 binding 建立失败相同）。已有的 `revoke:` 墓碑照常重试，不因重新登录取消，也不据此判定新条目失效。**全局收紧中的作废**（S1.14；清理时机经 Grok、Opus 盲选一致，§10 #34）：全局收紧或持久状态损坏时，全部 Knowledge 登录态条目在索引里标为作废：不用于任何 binding、不续签，这时不销毁（与 STATE §10 #3、#20、#22 一致）；之后某个 profile 经 T44 重新登录、新 binding 持久化确认后，按 U4.11 销毁该 profile 下的作废条目；profile 被移除时同样销毁。条目属于哪个 profile 按键名索引里记的 profile id 判断，不依赖 STATE 的持久状态（S1.8 损坏时 binding 到 profile 的映射可能已丢失）；所记 profile 已不存在的作废条目，按"profile 被移除"销毁。**键名索引**：宿主维护一份持久的键名索引（只存键名与所属 profile id，不含秘密，原子更新），写入条目之前先登记键名，删除条目成功之后再移除键名；启动清理以索引为准。不一致时 fail closed：索引里有、存储里没有的键名，从索引移除；所选存储可以枚举时，存储里有、索引里没有的 Knowledge 条目一律删除并记审计；索引读不出或损坏时报警，全部 Knowledge 登录态按失效处理（不使用任何条目，要求重新登录），可枚举时删除全部 Knowledge 条目。这也是对 KB-HOST-01 的约束：所选存储必须可枚举，或者维护这份索引（§10 #9）。按 S2.6，同一身份元组以后新建的 binding 拿到新的 `binding_id`，`retired` binding 的登录态按构造就用不上（S2.7）。
- **U4.10** [S2.7/S5.3+新设] **何时销毁登录态**：注销（T20、T31、T32）；binding 因换实例、principal 变化、改端点或租户、移除 profile 进入 `retired`（T21、T23–T25）。**不销毁**（§10 #7 已定）：Off（S5.3）、取消选用（T19）、停用 profile（T14）。全局收紧与持久状态损坏中的退役（S1.14）只作废、暂不销毁，见 U4.9。
- **U4.11** [R7.7+新设] **销毁登录态的步骤**：① 标为不可用，不再续签：内存里立即生效，同时写进键名索引的注销标记（U4.9）；② 按 U4.7 销毁该 binding 的全部 view 实例；③ remote：先把待撤销的刷新令牌或设备授权写进墓碑条目 `revoke:<binding_id>:<序号>`（`pending:` 条目没有 `binding_id`，其墓碑键为 `revoke:pending:<login_attempt_id>:<序号>`；序号单调递增，多次注销互不覆盖；存系统凭据存储，保护等级与登录态相同，键名先登记进 U4.9 的索引），再调用签发权威撤销（有时限），成功后立即删除该墓碑；④ 墓碑写入成功之后才删除以该 `binding_id` 为键的登录态条目；墓碑写入失败时保留登录态条目（仍标为不可用；索引里的注销标记保证重启后也不用于续签），提示用户并稍后重试 ③（期间同一 binding 重新登录，按 U4.9 先补写墓碑再覆盖）；⑤ 记录结果。**重试**：墓碑在每次启动与退避到期时重试，指数退避，下限 1 分钟、上限 1 小时；每次实际发出的撤销调用计 1 次（启动触发的也算），20 次或保留满 7 天先到者为止 [新设]，到期丢弃并记审计。④ 删除失败时持久记"待删"墓碑，每次启动重试；删掉之前同一 binding 再次登录（T02、T38）时按 U4.9 覆盖旧条目，并清除注销标记与"待删"记录。注销标记与墓碑都写入失败时 fail closed：删除该登录态条目，不再能远端撤销，界面写明"服务端登录在过期前可能仍有效"。③ 未确认成功时，界面写明"服务端登录在过期前可能仍有效"。local-managed 没有注销（S5.4）：`retired` 时只删宿主为它保存的凭据材料；实例自己的秘密随实例由 KB-LITE-01a、KB-LITE-04② 管理（§10 #10）。
- **U4.12** [R7.7] 销毁 view 实例或登录态都追不回已经显示或交给模型的内容；界面与审计不得声称可以。

## 5. 验收：票据、session JWT、注入与销毁

除注明外 profile 为 remote，binding 处于 `ready`。KB-C2-01、KB-C2-03、KB-C2-04a、KB-HOST-01 按本表写测试。

| ID | 规则 | 构造 | 期望 |
|---|---|---|---|
| UT-01 | U2.1、U3.5 | 打开 KnowledgeView，在页面脚本里查找票据与 JWT | 页面请求带注入的头并成功；页面的 JS、URL、history、存储、cookie 里都没有票据与 JWT |
| UT-02 | U2.8 | 截获已兑换的票据再次提交 | `unauthorized/credential_invalid`；整族 JWT 被撤销；C 未被撤销；安全审计 `consumed` |
| UT-03 | U2.9 | 兑换请求超时 | 宿主不重发同一张票据，改铸新票；服务端侧并发兑换只有一个成功，其余按 `consumed` 处理，随后该 `vph` 的整族 JWT 被撤销（含成功一方换出的 JWT，U2.8） |
| UT-04 | U2.3、U2.10 | 超过票据寿命后兑换 | 统一拒绝；宿主改铸新票 |
| UT-05 | U2.6、U2.8 | 在分区外用截获的未消费票据兑换，不带或带错 view 证明 | 统一拒绝；`proof_mismatch` 触发族撤销；票据作废 |
| UT-06 | U2.1 ③ | C 剩余寿命 50 秒时请求签发 | `unavailable/credential_near_expiry`；宿主续 C 后重试成功；不交给 STATE |
| UT-07 | U2.11 | 票据签出后、兑换前销毁分区 | 不兑换；票据与族被撤销 |
| UT-08 | U3.4 | 在分区外使用截获的 JWT（无 view 证明） | `credential_invalid` |
| UT-09 | U2.12 | 签发票据后、兑换前发生 Off | 不兑换；票据与族都撤销；view 按 binding 级销毁 |
| UT-10 | U2.12、U4.8 | 签发票据后、兑换前进入 `unavailable`（T08） | 不兑换，只撤销这张票据；view 保留、暂停注入；恢复后重新铸票并继续注入 |
| UT-11 | U3.3、U3.6 | JWT 用掉约 2/3 寿命；另测 C 的到期早于 ttl | 宿主重新铸票兑换并注入新 JWT；后者 `exp` 等于 C 的到期时间 |
| UT-12 | U3.4 | SSE 或 WebSocket 连接跨过 JWT 的 `exp`；连接期间族、`jti` 或 C 被撤销 | 前者服务端在 `exp` 关闭连接；后者服务端关闭该族已建立的流，做不到即时关闭的至多 60 秒内关闭 |
| UT-13 | U3.8、S3.1 | C 被签发权威撤销；页面经过注入的请求得 401；子用例：binding 处于 `degraded` | 探测得 `credential_revoked`；Provider 用 C 铸票，签发入口的撤权信号按 S3.1 走 T18（递增一次、作废在途请求、写占位），view 随之按 binding 级销毁；`degraded` 子用例同样走 T18；本文不另外递增；探测与兑换的响应本身不进入 STATE 判定；另测签发入口得 `credential_invalid`：走 T42，不递增，view 暂停注入、不销毁，取得新 C 后按 U3.6 换发 JWT |
| UT-14 | U3.8 | 页面经过注入的请求得 401，JWT 已过期；另测探测得 `credential_expired` | 宿主续期后继续注入；view 不销毁 |
| UT-15 | U3.8 | 页面经过注入的请求得 401，族已被撤销；10 分钟内再发生一次 | 探测得 `credential_invalid`；第一次 view 级处理，generation 不变；第 2 次触发时不再重建，KnowledgeView `closed`（`session_rejected`），profile 状态不变；期间的崩溃、Off→On、端口变化不计数 |
| UT-16 | U3.8 | 1 秒内 20 个经过注入的请求得 401；另有白名单外路径的请求得 401 | 10 秒内至多一次自查；白名单外的 401 不触发自查 |
| UT-17 | U1.3 | 拿 session JWT 调集成 search 端点；拿 C 调 UI 接口 | 都是 `unauthorized/credential_invalid` |
| UT-18 | U3.4、R2.6 | JWT 访问 E 外的文档；A 收窄到 E 为空 | 前者与不存在相同的 `not_found`；后者 `forbidden/empty_scope` |
| UT-19 | U2.5、U2.13 | 跑完 UT-01–UT-05 后检查宿主日志、服务端访问日志与审计 | 只有 H(`ticket_id`)，没有票据值、JWT、view 证明 |
| UT-20 | U3.4 | 构造各类 401、403、404、500 响应 | 错误体与响应头都不含 `Authorization` 与 view 证明的值 |
| UT-21 | U3.7 | 用另一个 principal 的 C 调撤销入口；用 session JWT 调撤销入口 | 都被拒绝，目标票据与族不受影响 |
| UT-22 | U4.5 | 页面向其他 origin 发请求；子 frame 向允许 origin 发请求；另一个 WebContents 发请求 | 都不带注入的头 |
| UT-23 | U4.5 | 页面自带 `authorization`、`AUTHORIZATION`、`x-agent24-view-proof`、`X-AGENT24-View-Proof` 等头，分别发往允许 origin 的白名单路径与其他 origin | 各种大小写都先被删除；只有前者换成宿主注入的值 |
| UT-24 | U4.5 | 允许 origin 的白名单路径 30x 跳到其他 origin | 后一跳不带这两个头 |
| UT-25 | U4.5 | 页面向允许 origin 上的集成端点（不在 UI 白名单内）发请求；以 `img` 加载白名单路径下的资源 | 都不注入 |
| UT-26 | U4.5 | PoC：主文档导航、EventSource、WebSocket 升级（`wss` 对应 `https` 的允许 origin）、download | 记录各自的资源类型、`details.frame` 是否为空、是否经过钩子；`details.frame` 为空的不注入；WebSocket 按 origin 映射注入；不能归入允许类型的不注入 |
| UT-27 | U4.2、U4.3 | 页面尝试插入 iframe、`object`、`<base>`、向其他 origin 提交表单、启动 Web Worker、注册 Service Worker；测试桩强行触发 `registration-completed` | 都被 CSP 挡住；被触发的注册立即被注销并清除 |
| UT-28 | U4.2 | 生产构建：为 KnowledgeView 打开 DevTools；带 `--remote-debugging-port` 启动；带 `--inspect` 等 Node CLI 参数或设置 `NODE_OPTIONS` 启动 | DevTools 打不开；应用不传远程调试端口；fuses `RunAsNode`、`EnableNodeCliInspectArguments` 与 `EnableNodeOptionsEnvironmentVariable` 为关闭，相关参数与环境变量（含 `ELECTRON_RUN_AS_NODE`）不生效 |
| UT-29 | U4.4 | remote 配置为 `http` origin；服务端证书无效；local-managed 握手 origin 为 `http://127.0.0.1:<port>`；local-managed 配置为非 loopback 的 `http` | 依次：拒绝配置；请求失败且没有放行入口；正常注入；拒绝 |
| UT-30 | U4.5、U4.8 | local-managed 的 sidecar 进程退出；sidecar 离开 ready；sidecar 退出后其端口被别的进程占用 | 退出事件到达后再没有请求被注入，含退出到健康检查发现之间的窗口；离开 ready 时立即停止注入；冒充进程完不成握手，始终不注入 |
| UT-31 | U4.7、U4.8 | 子用例：① Off，main 回 ack；② Off，main 不回 ack；③ 撤权（C 已不可用）后 main 不回 ack | ① Provider 收到 ack 后才写盘，丢弃 C 之前用 C 撤族成功（2 秒内）；② Provider 2 秒后继续写盘，用 C 按 `vph` 撤族并断开私有通道，main 在 3 秒加通道时延内停止注入，旧 JWT 被以 `credential_invalid` 拒绝；③ 跳过撤族并记审计，只靠断开通道与心跳新鲜度，注入同样在上界内停止，旧 JWT 因 C 已撤销被拒 |
| UT-32 | U4.8 | agent24d 崩溃（私有通道断开或心跳超时），随后恢复；另测 Provider 写盘卡住 | main 在 2 秒内对全部 view 停止注入、关桥并销毁；恢复后按 view 级重建，不计入 `crash_loop` 与 `session_rejected`；写盘卡住时心跳照常（不在同一线程） |
| UT-33 | U4.7 | 故障注入：撤销族、销毁 WebContents、清理分区三步各自失败 | 桥保持关闭，注入不恢复 |
| UT-34 | U4.8 | Off 后 On；渲染进程崩溃；10 分钟内第 4 次崩溃；隐藏后再显示；"sidecar 不重启"一项另用 local-managed profile 测前两项 | Off→On 与崩溃都重建新实例、没有旧存储，崩溃按退避；local-managed 下 Off→On 与崩溃重建时 sidecar 都不重启；第 4 次崩溃后 `closed`（`crash_loop`）；隐藏再显示是同一实例 |
| UT-35 | U4.1 | 两个 binding 先后打开 KnowledgeView | 分区不同，存储不共享 |
| UT-36 | U4.6 | 同一 binding 销毁后重建；64 个分区都活着时再建；总数已满 64、已销毁的分区都属于 binding A 时为 binding B 新建；清理中 `clearAuthCache` 失败 | 依次：复用该 binding 的分区，Web 存储、HTTP 缓存、auth 缓存都为空，旧连接已关闭；拒绝并提示重启；拒绝并提示重启；该分区不再复用，本次不新建 view |
| UT-37 | U1.5 | Agent24 renderer 经 `backendProxy` 访问铸票、撤销、作废通知用的私有路由，以及兑换入口与 UI 接口路径 | 都被拒绝；拿不到票据或 JWT |
| UT-38 | U4.10、U4.11 | 注销 | view 实例被销毁；签发权威撤销被调用；凭据存储条目被删；同一 principal 重新登录写入新条目 |
| UT-39 | U4.9、S2.7 | 改端点（T24）后在原端点重新登录 | 新 binding 查不到旧条目 |
| UT-40 | U4.9、U4.11 | 注销时签发权威不可达；同一 binding 先后注销两次；墓碑写入失败，随后重启；墓碑与注销标记都写入失败；凭据存储删除失败后重启；删除失败 → 同一 binding 重新登录 → 重启；墓碑写入失败 → 同一 binding 重新登录（另测此时补写墓碑也失败） | 先写 `revoke:<binding_id>:<序号>` 墓碑、写成功才删登录态条目；按 1 分钟到 1 小时的指数退避重试，每次实际调用计一次，20 次或 7 天后丢弃并记审计，撤销成功即删墓碑；两次注销各有墓碑、互不覆盖；墓碑写入失败时登录态条目保留并提示用户，重启后按索引的注销标记视为失效、不用于续签，并重试撤销；两者都写入失败时删除条目并提示服务端登录可能仍有效；删除失败时留"待删"墓碑、重启后重试；删除失败后重新登录：新条目覆盖旧条目，注销标记与"待删"记录随键名登记一并清除，重启后新登录仍可用、未被撤销、没有被删除，旧令牌的 `revoke:` 墓碑照常重试；墓碑写入失败后重新登录：先把旧令牌写进 `revoke:` 墓碑再覆盖，补写失败时不覆盖、本次登录按失败处理（新登录态撤销、不保存） |
| UT-41 | U4.9 | 登录成功，但 binding 初值写盘失败；随后重启；另测：临时条目写入后进程崩溃，再启动 | 临时条目立即删除（remote 撤销）；重启时没有残留的 `pending:` 条目；崩溃残留的 `pending:` 条目在启动清理时 remote 先写墓碑 `revoke:pending:<login_attempt_id>:<序号>` 并撤销，再删除 |
| UT-42 | U4.10 | Off；取消选用；停用 profile | 都不删登录态条目（§10 #7） |
| UT-43 | U4.8 | binding 从 `ready` 进入 `degraded`；再进入 `unavailable` | `degraded` 时照常注入；`unavailable` 时暂停注入，恢复后继续 |
| UT-44 | U4.2 | 页面调用 `window.open`、跨 origin 导航、打开 `file://` 与自定义协议、触发下载 | 都被拒绝 |
| UT-45 | U2.6、U2.7 | 兑换时 C 已过期或被撤销；兑换时 E 为空 | 前者 `credential_expired` / `credential_revoked`；后者 `forbidden/empty_scope`；都不签发 JWT |
| UT-46 | U2.6、U3.7 | 按 `ticket_id` 撤销后再兑换该票据；族撤销后用同一 `vph` 的新票据兑换；族撤销后继续用旧 JWT | 都被拒（`credential_invalid`，审计分别为 `revoked`、`family_revoked`）；旧 JWT 被拒 |
| UT-47 | U2.6 | 短时间内大量请求兑换入口 | 兑换入口按自己的限额拒绝超出部分，签发、探测与 UI 接口不受影响 |
| UT-48 | U2.1 ⑤ | 兑换请求发出后连接被重置 | 服务端只收到一次兑换；宿主不重发同一张票据，改铸新票 |
| UT-49 | U2.3 | 服务端重启；另测跨重启重放：重启前已兑换的票据在重启后再次提交 | 消费与撤销记录仍然有效；没有持久化的部署在重启后轮换 UI 会话密钥，重启前的 JWT 被拒；重放的票据被统一拒绝（两种部署都一样），不签发 JWT |
| UT-50 | U4.8 | local-managed 的 sidecar 换端口 | 先关桥、停止注入，再更新允许 origin；新握手后重建并注入新 origin；这次关桥不计入 10 分钟 3 次 |
| UT-51 | U4.2 | 页面请求 `media`、`geolocation`、`notifications`、`clipboard-read`、`openExternal`、`midi` 等权限并用 Permissions API 查询；请求 USB、HID、serial、蓝牙设备与屏幕共享；测试桩让 `select-bluetooth-device`、`select-hid-device`、`select-serial-port`、`select-usb-device` 各带一个可用设备触发；测试桩让 KnowledgeView 的 `select-client-certificate` 带一张可用证书触发 | 权限请求与检查都被拒；设备与屏幕共享不授予；四个选择事件都被 `preventDefault` 并以空选择取消，页面拿不到任何设备；`select-client-certificate` 被 `preventDefault`，`callback()` 不传证书，不发出任何客户端证书；KnowledgeView 的 WebRTC IP 策略为 `disable_non_proxied_udp` |
| UT-52 | U4.5 | 白名单路径用 `/../` 跳出前缀；路径含 `%2f`、`%2F`、`%5c` | 前者按规范化后的 pathname 判定，跳出前缀即不注入；后者不注入 |
| UT-53 | U4.5 | 打开 KnowledgeView；暂停注入后恢复 | 首个 JWT 兑换成功、注入就绪之前不执行 `loadURL`；恢复注入时重新加载当前文档 |
| UT-54 | U4.5、U4.8 | 心跳中断 1.2 秒后恢复；系统休眠后唤醒 | 前者中断期间不注入、桥回 `not_ready`，但不全局销毁；后者休眠期间不判心跳丢失，唤醒后 5 秒宽限内不全局销毁，拿到新心跳之前不注入 |
| UT-55 | U3.8 | 探测超时或 5xx 连续 3 次；探测得 503 后重试成功，随后再连续 2 次超时；10 分钟内 3 次"探测得 `credential_revoked` 而签票成功"；兑换得 `credential_expired`；兑换得 `forbidden/empty_scope`；签发入口得 `credential_expired`；签发入口得 `forbidden/empty_scope` | 依次：暂停注入、退避重试后 `closed`（`probe_failed`）；503 时暂停注入，重试成功后解除暂停、恢复注入并重新加载当前文档，计数清零，之后的 2 次超时不进入 `closed`；`closed`（`revocation_mismatch`）；先续 C 再重铸票；不建 view，显示"没有可见的知识库"；先续签 C 再签发；交 STATE 按 S5.10 判定，不建 view，显示"没有可见的知识库" |
| UT-56 | U4.9 | 故障注入：条目已写入但键名未登记索引（可枚举存储）；索引有键名而条目缺失；索引损坏 | 依次：启动时删除该条目并记审计；从索引移除该键名；报警，Knowledge 登录态全部按失效处理，要求重新登录 |
| UT-57 | U3.6 | 续签换得新 C；T42 丢弃当前 C 后取得新 C；换发前旧 C 已被签发权威作废 | 两种情况都在换发前暂停注入，按 U2.1 重新铸票换发 JWT（同一 view 实例、同一族，`parent_credential_id` 为新 C），之后继续注入；binding 一直可服务、只因 C 被替换而暂停时（续签换得新 C），继续注入时不重新加载当前文档；换发前的 401 不计入 `revocation_mismatch` |
| UT-58 | U4.5 | 心跳超过 1 秒期间页面向白名单路径发 fetch；心跳恢复 | 前者被取消，没有无认证请求到达服务端；后者直接继续注入，不重新加载当前文档 |
| UT-59 | U4.9、U4.10 | 测试桩构造 STATE 的全局收紧（S1.14）；随后 profile P 经 T44 重新登录并建立新 binding；另一 profile Q 未确认；另测：持久状态损坏（S1.8）后 P 经 T44 重新登录，以及索引里所记 profile R 已不存在 | 全局收紧时登录态条目全部标为作废、都不用于续签，没有远端撤销调用；P 的新 binding 持久化后，P 下的作废条目按 U4.11 撤销并删除；Q 的作废条目保留、不使用；持久状态损坏时按索引里的 profile id 找到 P 的作废条目并撤销、删除，R 的作废条目按"profile 被移除"撤销并删除 |
| UT-60 | U4.8、S4.9 | profile P、Q 的 binding 同时激活、各开一个 KnowledgeView；P 取消选用；随后 Off | 前者只销毁 P 的 view，Q 的 view 照常注入；后者两个都销毁，之后不新建 KnowledgeView，入口标明不可用及原因 |
| UT-61 | U4.8 | 签发入口得凭据无效类 `unauthorized` 触发 T42（页面请求的 401 经探测，探测结果不进 STATE，U3.8），续签也失败（`credential_rejected`）；遮罩期间页面发合法 `citation`；随后同一 principal 重新登录（T38），之后页面发合法 `citation`；另测：遮罩期间手动重试经 T10 回到可服务；另测：遮罩期间收到撤权信号（T35a） | 暂停注入，桥不关闭、view 不销毁，宿主遮罩显示"不可用，需要重新授权"，页面内容不可见、不接收输入；遮罩期间的 `citation` 按 U6.4 ⑬ 回 `not_ready`；重新登录回到可服务、generation 未变时撤下遮罩，换发 JWT 并重新加载当前文档，之后的 `citation` 得到 `accepted`；T10 子用例同样撤下遮罩并恢复；撤权信号使 generation 递增，view 按 binding 级销毁 |

## 6. 桥通道与宿主判定

- **U6.1** [D5/KB-C2-04a+新设] **通道**：本文不规定机制，KB-C2-04a 先验证（页面里的 `window.postMessage` 到不了 main）。候选通道都必须满足：(a) main 从通道自身的元数据得知发出方的 WebContents、frame 与文档，不依赖消息内容；Electron 34 的 WebFrameMain 没有文档令牌（§9 第 7 条），**文档身份从哪里来是 PoC 项**；(b) 页面经通道只能到达桥的处理函数，通道不代发请求、不读文件、不开 URL；(c) main 能识别发出 frame 是否主 frame——这只用于违规检测，**不是安全边界**：同源子 frame 能操纵主 frame，与主 frame 同权，隔离靠 CSP 的 `frame-src 'none'`、`child-src 'none'` 与 origin 校验；(d) main 在解析之前就知道原始字节长度，并在传输层计数与拒绝；(e) 宿主到页面的消息只送达当前文档的主 frame，子 frame 与旧文档收不到。无 preload 时宿主→页面只能用 `executeJavaScript` 或 `executeJavaScriptInIsolatedWorld`：前者在页面主世界执行；后者在隔离世界执行，但交给页面代码时仍要经过主世界可见的事件。宿主调用两者时 `userGesture` 一律为 `false`。所以宿主→页面的消息对页面内任何脚本可见，也可能被页面脚本伪造给页面自己；它们不含秘密，页面不得据此做安全决策，宿主侧的判定不受影响。(f) main 用 `before-input-event` 加 `input-event` 观察该 view 内的用户输入，只认激活类事件（`InputEvent.type` 为 `mouseDown`、`pointerDown`、`rawKeyDown`、`keyDown`、`gestureTap`、`touchStart`），`mouseMove`、`mouseEnter`、滚轮与滚动手势不算；PoC 只验证这两个事件对 WebContentsView 是否生效（U7.2 用）。做不到 (a)、(b)、(d)、(e) 时桥不开放（§10 #14）。
- **U6.2** [ADR-004:145/S5.9+新设] **方向与类型**：页面→宿主只有 `status`、`citation`、`cancel`、`return_to_chat_intent` 四种（任务原文的状态、引用、取消、回主对话意图）。宿主→页面另设闭集：`hello`（协议版本、generation、不透明的 view 实例 ID）、`state`（profile 状态及 code、主题、语言）、`reply`（对页面消息的应答）。没有交付票据或凭据的消息；宿主→页面同样不得出现 URL、路径与令牌（U7.4）。KnowledgeView 的 `closed` 状态由宿主自己的界面显示（ADR-004:141 的 typed status），不经桥发送。
- **U6.3** [新设] **文档切换与 hello**：主 frame 每提交一个新文档（含重新加载），宿主清空该 view 实例按文档维护的去重表、在途表与待发队列，在 `dom-ready` 之后向新文档发 `hello`。页面还没拿到 generation 时，可以发一条 `generation` 为 0 的 `status` 作为 hello 请求，宿主回一条 `hello`，不计违规；每个文档至多 3 次，之后的按 `stale_generation` 处理。hello 请求在 U6.4 ⑫ 之后、⑬ 之前处理，所以桥未开放时也能拿到 `hello`；这是 S5.9 的例外，`hello` 只含非秘密信息（§10 #12）。旧文档的消息按通道给出的文档身份判为非当前文档（U6.4 ⑤）。
- **U6.4** [S5.9/ADR-004:145+新设] **宿主对每条页面消息的判定**（按顺序，取第一个不成立的）：
  ① 发出方 WebContents 按分区映射是一个存活 view 实例的 KnowledgeView，且该实例的桥未关闭；
  ② 发出方 frame 是主 frame；
  ③ 该 frame 已提交文档的 origin（main 看到的值）等于允许 origin；
  ④ 该 WebContents 的 session 就是分区映射里该实例的分区，且 U4.2、U4.3 的条件成立；
  ⑤ 发出文档是当前文档；
  ⑥ 到达速率未超限（U6.7）：只看传输层元数据，按到达数计，不解析；
  ⑦ 原始长度不超过上限（U6.7）；
  ⑧ 是合法 UTF-8 JSON，没有重复键，嵌套、数组与字符串长度不超过上限（U6.7）；
  ⑨ 任意深度都没有禁止键（U7.4）；
  ⑩ 自由文本字段通过内容检查（U7.4；v1 没有自由文本字段，本步恒成立）；
  ⑪ `v` 存在且受支持（`v` 缺失落到 ⑫）；
  ⑫ 符合 schema（U7.1、U7.2），`direction` 为 `page_to_host`；通过后若是 hello 请求（U6.3），回 `hello` 并结束；
  ⑬ 该 binding 准入开放、处于可服务状态（S5.9 中除 generation 以外的条件；待复验未解除视为准入未开放，S3.1），且 main 收到的最近一次心跳不超过 1 秒（U4.5 ⑥）；
  ⑭ 消息的 `generation` 等于当前值（S5.9 的 generation 条件）；
  ⑮ `id` 不在当前文档的去重表里（U6.7）；
  ⑯ 未超过在途上限（U6.7）。
  ①–⑤ 不成立时**静默丢弃**，不回 `reply`，按发出方计数，日志采样记录（每个发出方每分钟前 10 条，之后每 100 条记 1 条），宿主至多同时跟踪 256 个发出方、超出时淘汰最旧的；其中 ②、③ 的来源在 view 内部，同时记为硬违规（U6.6）。⑥ 超限时每个限速窗口只回一次 `overloaded`，其余静默丢弃并计数。⑦–⑯ 不成立时回 `reply`（`rejected`，`code` 与 `reason` 见 U7.5）。`reply` 的 `in_reply_to` 按 U7.1 只在拿到触发消息的合法 `id` 时填写：⑥、⑦、⑧ 不成立时一律省略（没有解析），⑨–⑫ 不成立而顶层 `id` 缺失或不是 ULID 时同样省略。只有通过 ⑯ 的 `id` 才登记进去重表。任何一步不成立，消息都没有任何效果。
- **U6.5** [D5+新设] sender、frame、origin 与文档身份**只取自传输层元数据**。报文自报任何绑定字段（如 `origin`、`sender`、`frame`、`webcontents`）即视为伪造，按禁止键处理（U7.4）。
- **U6.6** [新设] **违规与关桥**：
  - `overloaded` 是正常的背压信号，本身不算违规。**无视 `retry_after_ms`** 才算：宿主为某个限额（到达速率、在途总数或 intent 名额）发出 `overloaded` 后，只统计在发出时刻加 max(500 毫秒, 该通道的往返时延估计) 之后、`retry_after_ms` 结束之前到达、且受同一限额约束的消息（例如 intent 名额满时，并发的 `status` 不计）；这类消息达到 3 条，记一次软违规。窗口互不重叠：窗口未结束时再发出的 `overloaded` 不开新窗口。
  - **软违规**：⑦、⑧、⑫、⑭、⑮ 不成立；无视 `retry_after_ms`；⑫ 中只因引用 ID 默认禁止规则（U7.1）不成立的不计；⑪ 不成立单独计数，同一 view 实例 10 秒内达到 5 次，记一次软违规并清零。⑬ 不成立只拒绝，不计入软违规，也不计入关桥上限。同一 view 实例 10 秒内 5 次软违规即关桥。
  - **硬违规**：⑨ 不成立（先回 `reply` 的 `forbidden_key`，再关桥），或 ②、③ 的来源在 view 内部（静默后关桥）；一次即关桥，并记审计 `knowledge.bridge_violation`（`binding_tag`、H("kb-view-v1"; `view_nonce`)、判定步骤、`type`，不含消息内容）。
  - 关桥是 view 级处理：按 U4.7 销毁 view 实例并撤销族，**不递增 generation**；KnowledgeView 进入 `closed`（`code: "bridge_closed"`），用户点重新加载才新建 view 实例。同一 binding 10 分钟内第 3 次因违规关桥后不再提供重新加载，直到用户从 Knowledge 设置里重新打开（§10 #17）。因端口变化（U4.8）、通道恢复或 binding 级销毁而关桥的，不计入。
- **U6.7** [新设] **上限**（工程值，取较严，可按实测调整）：页面→宿主到达速率持续 10 条/秒、突发 20 条（⑥）；单条不超过 8 KiB（⑦，传输层拒绝）；嵌套不超过 4 层（根对象算第 1 层，每个对象或数组各算一层）；数组不超过 32 项；单个字符串不超过 2 KiB；处理中最多 4 条（`cancel` 不占名额），其中 `return_to_chat_intent` 最多 1 条（⑯）。宿主→页面单条不超过 32 KiB；按队列发送：`hello` 与 `state` 合计最多 5 条/秒，排队中的 `state` 只保留最新一条；`reply` 不合并、不丢弃，它的数量受 ⑥ 的到达限速约束：每条通过 ⑥ 的消息至多产生两条 `reply`（`accepted` 加 `done` 或 `rejected`），超限的到达每个窗口至多一条 `overloaded`。**去重表**：每个文档至多 4096 项，满了按 FIFO 淘汰；被淘汰的 `id` 再次出现时当新消息处理——内存有界比完美去重重要，页面重发自己的消息也得不到额外权限；作为替代手段，宿主可以拒收时间戳早于最老保留项的 ULID（回 `duplicate_id`）。hello 请求每个文档至多 3 次（U6.3）。
- **U6.8** [R10.3+新设] 宿主日志与审计只记 `type`、`code`、`reason`、`id`、H("kb-view-v1"; `view_nonce`) 与判定步骤，不记消息体、票据、JWT 与 view 证明。

## 7. 消息 schema

- **U7.1** [KB-C2-04b+新设] **信封**：`{ v, direction, type, id, generation, in_reply_to?, payload }`。每一层都不允许多余字段（`additionalProperties: false`），除注明外都必填。

| 字段 | 类型与约束 | 说明 |
|---|---|---|
| `v` | 整数，v1 恒为 `1` | 不受支持时回 `unsupported_protocol_version` |
| `direction` | 枚举 `page_to_host` / `host_to_page` | 页面只能发 `page_to_host` |
| `type` | 页面→宿主：`status` / `citation` / `cancel` / `return_to_chat_intent`；宿主→页面：`hello` / `state` / `reply` | 与 `direction` 不配即 `schema_violation` |
| `id` | ULID，`^[0-9A-HJKMNP-TV-Z]{26}$` | 发出方生成，当前文档内唯一 |
| `generation` | 整数，1 至 2^53−1；只有作为 hello 请求的 `status` 可以为 0（U6.3） | 页面取自最近一条 `hello` |
| `in_reply_to` | ULID | 只能出现在 `reply` 里，其他类型出现即 `schema_violation`。触发消息通过了 U6.4 ⑧、且顶层 `id` 是符合本表 `id` 行格式的 ULID 时必填，取该 `id`；否则省略（⑥ 的 `overloaded`、⑦、⑧，以及 `id` 缺失或不是 ULID 的，U6.4），宿主不编造占位值 |
| `payload` | 对象，按 `type` 取 U7.2、U7.3 之一 | |

  **引用 ID**：`source_revision`、`index_build_id` 是 READONLY R5.2 的不透明标识；`knowledge_base_id`、`knowledge_id`、`chunk_id` 按本文约束。五者都校验 UTF-8 字节长度 1–128、不含控制字符（U+0000–U+001F、U+007F–U+009F），并作为纵深防御默认拒绝含 `/`、`\`、`:`、`..` 或空白的 ID；长度或控制字符不符合时回 `schema_violation`（软违规）；只命中默认禁止规则时同样回 `schema_violation`，但不计违规（服务端若使用含 `:` 等字符的 ID，不致因合法引用关桥，§10 #21）。它们不做 U7.4 的内容检查。宿主只把它们作为不透明值放进 READONLY 的 read 请求，从不当作 URL 或路径解释。这条默认禁止规则在与 READONLY R5.2 同步之前先行采用（§10 #21）。`chunk_revision` 为整数，0 至 2^31−1。引用的修订字段拍平在同一层，`return_to_chat_intent` 的嵌套为 4 层（信封、`payload`、`citations` 数组、引用对象）。**容量验算**：8 条引用、每个 ID 取满 128 字节且不含需要 JSON 转义的字符时，整条消息紧凑编码为 6283 字节，装得进 8 KiB；`\` 已被默认禁止；ID 若含 `"`，转义后最坏 11403 字节，按 `too_large` 拒绝。服务端生成的 ID 不含 `"`，所以 8 条的上限保持不变（§10 #21）。

- **U7.2** [KB-C2-04b/R4.7+新设] **页面→宿主的 payload**：

| `type` | payload | 宿主怎么处理 |
|---|---|---|
| `status` | `state`：`loading` / `ready` / `error`；`error_code`：只在 `error` 时出现，`unauthorized` / `forbidden` / `not_found` / `unavailable` / `internal` | 只用于宿主侧显示，不触发续期、登录、自查或任何 STATE 事件；`generation` 为 0 时是 hello 请求（U6.3） |
| `citation` | `knowledge_base_id`、`knowledge_id`、`chunk_id`、`source_revision`、`index_build_id`、`chunk_revision`，全部必填 | 只用于展示：宿主经 Provider 以当前 generation 做带 `chunk_id` 与 `expected_revision` 的单分块 read（R4.1、R4.7），通过才在引用 UI 里显示（KB-C1-05）；不写入模型上下文，也不写入草稿；read 走桥预算并排队（见下） |
| `cancel` | `target_id`：ULID | 撤回当前文档内一条还在处理或排队的 `citation` 或 `return_to_chat_intent`：被撤回的原消息最终回 `rejected`（`code: "cancelled"`），等待确认的弹窗随之关闭；`cancel` 本身回 `accepted`；目标不存在或已完成时回 `accepted`，不做任何事 |
| `return_to_chat_intent` | `citations`：数组，0 至 8 项，每项同 `citation` 的 payload | 见下 |

  `return_to_chat_intent` [新设]：只在该 KnowledgeView 最近 5 秒内有激活类用户输入（U6.1 (f)）时接受，否则回 `forbidden/user_activation_required`。草稿附件在一个对话里累计至多 16 项，超出回 `invalid_request/attachment_limit`。接受后宿主只高亮"回到对话"入口或弹出确认，**不自动切换**；确认界面列出将附加的引用条数与每条的标题，标题由宿主重新 read 后取得，不用页面给的任何文字；标题按纯文本渲染，去掉控制字符与 bidi 控制字符（U+202A–U+202E、U+2066–U+2069 等），截断到 120 个字符。等待确认最多 60 秒，超时释放名额并回 `rejected`（`forbidden/confirmation_timeout`，不可重试）；用户拒绝回 `rejected`（`forbidden/user_declined`），该 view 实例随后冷却 30 秒，冷却期内的 intent 回 `forbidden/cooldown`。等待确认期间发生文档切换（U6.3）、view 实例销毁（U4.7，含关桥）或 binding 离开可服务状态、准入关闭（同 U6.4 ⑬，心跳新鲜度除外）时，宿主关闭确认弹窗、释放名额、不加附件，回 `rejected`（`unavailable/not_ready`）；前两种情形这条 `reply` 随待发队列清空，不送达。用户确认后才切回主对话，引用逐项按 `citation` 的方式校验后作为草稿附件放进输入区；**只有这条路径产生草稿附件**。只有部分引用通过时，只附加通过的那几项，回 `done`，`code` 取第一项未通过引用的 READONLY 值（如 `stale`）；全部通过时 `done` 不带 `code`；全部未通过时不加附件，同样回带该 `code` 的 `done`。不自动发送、不创建 run、不做任何审批；v1 不带自由文本（§10 #15）。

  **桥预算** [新设]：`citation` 与 intent 触发的 read（intent 的逐条校验 read 与标题 read 都算）走 Provider 为桥单设的低优先级预算，至多 2 个并发，其余按 FIFO 排队，不直接拒绝；不占用主对话知识调用的预算与并发，这是与 KB-C1-04 总预算的接口。U6.4 ⑯ 的"处理中"计数包含排队中的消息，上限 4 不变。排队超过 10 秒仍未开始的，回 `unavailable/overloaded` 并带 `retry_after_ms`。

- **U7.3** [新设] **宿主→页面的 payload**：`hello`：`supported`（整数数组，v1 为 `[1]`）、`view_instance_id`（`^[A-Za-z0-9_-]{22}$`，随机生成，不同于 `view_nonce` 与 `view_secret`，不是秘密）。`state`：`profile_state`（STATE 草案 S1.1 的六个状态之一）、`code`（可选，`^[a-z_]{1,64}$`，取 STATE 草案 §6 的本地 `reason`）、`theme`（`light` / `dark`）、`locale`（BCP 47，`^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8}){0,3}$`）。`reply`：`result`（`accepted` / `rejected` / `done`）、`code`、`reason`、`retry_after_ms`、`supported`（后四项可选）；`done` 表示处理结束，宿主的 read 失败时 `code` 取 READONLY 的值（如 `stale`）。宿主→页面的消息没有自由文本，发出前同样过 U7.4 的禁止键检查，不通过就不发并记日志。
- **U7.4** [ADR-004:145+新设] **禁止键与内容检查**：
  - 禁止键在任意深度生效。比对方式：键名转小写、去掉 `_` 与 `-` 后，与清单项（同样规范化）**精确相等**才算命中，不做子串匹配。清单：`url`、`uri`、`href`、`path`、`file`、`dir`、`cwd`、`command`、`cmd`、`exec`、`executable`、`argv`、`args`、`shell`、`script`、`approval`、`approve`、`decision`、`token`、`jwt`、`ticket`、`secret`、`password`、`authorization`、`cookie`、`origin`、`sender`、`frame`、`webcontents`、`port`、`__proto__`、`constructor`、`prototype`。解析器不得给 `__proto__` 等键特殊语义。
  - CI 检查桥 schema 本身不定义任何禁止键；检查用例里放 `profile_state`、`direction` 作为不应命中的反例。
  - 内容检查只用于自由文本字段：字符串值含 `/`、`\`、`://`，或以 `~`、`.` 开头，视为携带 URL、路径或可执行内容，回 `forbidden_content` 并按硬违规处理。**v1 两个方向都没有自由文本字段，本条不适用**，留给以后的版本。
- **U7.5** [R6.1+新设] **`reply` 的拒绝码**：`code` 沿用 READONLY 的取值，新增的 `reason` 只在桥上使用；不带自由文本，不回显内容。

| 判定 | `code` / `reason` | 附加 | 违规 |
|---|---|---|---|
| U6.4 ⑥ 到达超速 | `unavailable` / `overloaded` | `retry_after_ms`；每窗口一次 | 不计（无视它另计，U6.6） |
| U6.4 ⑦ 超长 | `invalid_request` / `too_large` | | 软 |
| U6.4 ⑧、⑫ 格式或 schema 不符 | `invalid_request` / `schema_violation` | | 软 |
| U6.4 ⑫ 只命中 U7.1 引用 ID 的默认禁止规则 | `invalid_request` / `schema_violation` | | 不计 |
| U6.4 ⑨ 禁止键 | `invalid_request` / `forbidden_key` | | 硬：先回 `reply`，再关桥 |
| U6.4 ⑪ 版本 | `invalid_request` / `unsupported_protocol_version` | `supported` | 单独计数（U6.6） |
| U6.4 ⑬ 未准入、不可服务或心跳不新鲜 | `unavailable` / `not_ready` | | 不计 |
| U6.4 ⑭ generation 不符 | `invalid_request` / `stale_generation` | | 软 |
| U6.4 ⑮ `id` 重复 | `invalid_request` / `duplicate_id` | | 软 |
| U6.4 ⑯ 在途已满 | `unavailable` / `overloaded` | `retry_after_ms` | 不计（无视它另计，U6.6） |
| U7.2 缺少用户输入 | `forbidden` / `user_activation_required` | | 不计 |
| U7.2 附件超限 | `invalid_request` / `attachment_limit` | | 不计 |
| U7.2 确认超时；用户拒绝；冷却期内 | `forbidden` / `confirmation_timeout`；`forbidden` / `user_declined`；`forbidden` / `cooldown` | | 不计 |
| U7.2 等待确认时 binding 离开可服务状态或准入关闭 | `unavailable` / `not_ready` | | 不计 |
| U7.2 桥 read 排队超过 10 秒 | `unavailable` / `overloaded` | `retry_after_ms` | 不计 |
| U7.2 被 `cancel` 撤回的原消息 | `cancelled`（READONLY 错误码闭集里的本地码，不上线到服务端） | | 不计 |

- **U7.6** 示例。引用：
```json
{ "v": 1, "direction": "page_to_host", "type": "citation", "id": "01JB4A0B1C2D3E4F5G6H7J8K9M", "generation": 7,
  "payload": { "knowledge_base_id": "kb-a", "knowledge_id": "doc-1", "chunk_id": "chk-11",
               "source_revision": "src-3", "index_build_id": "ib-5", "chunk_revision": 0 } }
```
应答：
```json
{ "v": 1, "direction": "host_to_page", "type": "reply", "id": "01JB4A0C2D3E4F5G6H7J8K9M0N", "generation": 7,
  "in_reply_to": "01JB4A0B1C2D3E4F5G6H7J8K9M", "payload": { "result": "accepted" } }
```

## 8. 验收：伪造与越权的拒绝用例

KB-C2-04a、KB-C2-04b 按本表写测试。"静默"指不回 `reply`、没有任何效果、按 U6.4 计数并采样记日志。除注明外，消息本身合法，binding 处于 `ready`。会产生软违规的子用例各用一个新 view 实例，或间隔 10 秒以上执行，避免互相累积关桥。

| ID | 规则 | 构造 | 期望 |
|---|---|---|---|
| UB-01 | U6.4 ① | Agent24 自身 renderer 或另一个非 KnowledgeView 的 WebContents 经同一通道发合法消息 | 静默 |
| UB-02 | U6.4 ⑭ | 先打开 binding A 的 KnowledgeView（generation 5），再取消选用 A、改选 binding B（A 的 view 已销毁，B 的 generation 为 9）；B 的页面发 `generation: 5` | `stale_generation`；对 A 没有任何效果 |
| UB-03 | U6.1 (c)、U6.4 ② | 测试桩绕过 CSP 插入同源子 frame（正常情况下 `frame-src 'none'` 已挡住），子 frame 发消息 | 静默；按违规检测记硬违规，关桥，记审计。隔离靠 CSP，不靠这一步 |
| UB-04 | U6.4 ②、U6.6 | 同上，换成跨 origin 子 frame | 同 UB-03 |
| UB-05 | U6.4 ③、U4.8 | 主 frame 落到其他 origin 后发消息 | 静默；硬违规；view 级处理 |
| UB-06 | U6.4 ④ | 测试桩把一个使用其他分区的 WebContents 登记为 view 实例，并从它发消息 | 静默，记日志 |
| UB-07 | U6.5、U6.6、U7.4 | 消息带 `origin`、`Sender`、`frame` 或 `web_contents` 键 | 先回 `forbidden_key`，再关桥；硬违规 |
| UB-08 | U6.3、U6.4 ⑤ | 主 frame 导航到新文档后，旧文档在卸载前发消息 | 静默；不计违规 |
| UB-09 | U6.4 ⑭ | `generation` 为当前值减一或加一 | `stale_generation`（软） |
| UB-10 | U4.8、S5.9 | STATE 草案 S4.5 第 ① 步之后发消息 | 桥已在 ack 之前关闭：静默丢弃，即 S5.9 的"拒绝所有消息" |
| UB-11 | U6.4 ⑬、U6.6 | binding 处于 `unavailable`（不递增 generation 的 T08）；`persist_pending` 为真（靠测试桩注入）；待复验未解除（测试桩注入旧凭据的撤权信号，S3.1）；最近一次心跳超过 1 秒；以上各连发 10 次 | 都是 `unavailable/not_ready`；不计软违规，不关桥 |
| UB-12 | U6.4 ⑮ | 同一 `id` 发两次；另把一条在 ⑫ 被拒的消息修正后用同一 `id` 重发 | 前者第二次 `duplicate_id`，效果只发生一次；后者正常处理（被拒的 `id` 未登记） |
| UB-13 | U6.4 ⑦、U7.1 | 8 KiB + 1 字节的消息；另测：报文开头带合法 `id` | `too_large`，不解析；`reply` 不带 `in_reply_to`（两种都一样） |
| UB-14 | U6.4 ⑧、U7.1 | 子用例：非 UTF-8；非 JSON；重复键；嵌套 5 层（根对象算第 1 层，数组算一层）；数组 33 项；字符串超过 2 KiB | 每个子用例都是 `schema_violation`，`reply` 不带 `in_reply_to` |
| UB-15 | U6.4 ⑫ | `type` 为 `open_url` 或 `approve` | `schema_violation` |
| UB-16 | U6.4 ⑪、U6.6 | `v` 为 2；10 秒内连发 5 次 | 每次 `unsupported_protocol_version`，`supported: [1]`；第 5 次记一次软违规 |
| UB-17 | U6.4 ⑫、U7.1 | 多一个未知字段 `note`；页面发 `direction: "host_to_page"`、`type: "reply"`；缺少 `v`；缺少 `id`；`id` 为 `"x"`（不是 ULID）；`status` 带 `in_reply_to` | 都是 `schema_violation`；缺少 `id` 与 `id` 不是 ULID 两项的 `reply` 不带 `in_reply_to`，其余各项的 `reply` 都带 `in_reply_to` 且等于触发消息的 `id`；CI schema 检查接受不带 `in_reply_to` 的 `reply`，拒绝带 `in_reply_to` 的非 `reply` 消息 |
| UB-18 | U7.1 | `chunk_id` 为 `https://evil.example/x`；`knowledge_id` 为 `../../etc/passwd`；ID 含空白或 `\` | `schema_violation`，不计违规；不发 read |
| UB-19 | U7.1 | 引用 ID 含控制字符；ID 为 43 个三字节字符（129 字节） | `schema_violation` |
| UB-20 | U7.4 | 任意深度出现 `URL`、`file`、`Executable`、`argv`、`shell`、`approval`、`decision` 键 | 先回 `forbidden_key`，再关桥 |
| UB-21 | U7.4 | 带 `token`、`jwt`、`ticket`、`authorization`、`cookie` 或 `password` 键 | 同 UB-20；值不进日志 |
| UB-22 | U7.4 | 带 `__proto__` 或 `constructor` 键 | 同 UB-20；宿主对象原型不受影响 |
| UB-23 | U7.2 | `status.state` 为 `approved`，或带自由文本字段 `message` | `schema_violation` |
| UB-24 | U7.2 | `return_to_chat_intent` 带 9 条引用；`chunk_revision` 为 −1 或 2^31 | `schema_violation` |
| UB-25 | U7.2 | 页面发 `status`（`error`，`error_code: "unauthorized"`） | 只更新显示；没有续期、自查、登录或 STATE 事件 |
| UB-26 | U7.2、R4.4 | 形状合法的 `citation`，修订有效；另一条修订已过期 | 前者在引用 UI 显示，模型上下文与草稿都不变；后者 `done`，`code: "stale"`，不显示 |
| UB-27 | U7.2 | `cancel` 指向另一个 view 实例或旧文档的 `id` | `accepted`，没有任何效果 |
| UB-28 | U6.6、U6.7 | 1 秒内发 50 条，之后按 `retry_after_ms` 等待；收到 `overloaded` 后 500 毫秒内又到达的消息；intent 名额满时并发发 `status` | 超限部分每窗口一次 `overloaded`，其余静默；⑥ 的 `overloaded` 不带 `in_reply_to`（即使触发消息带合法 `id`，U7.1）；这三种都不计违规 |
| UB-29 | U6.6 | 新 view：收到 `overloaded` 后，在宽限期之后、`retry_after_ms` 之内持续发受同一限额约束的消息 | 每个窗口记一次软违规，窗口不重叠；10 秒内 5 次关桥 |
| UB-30 | U6.7、U7.2 | 测试桩让 Provider 的 read 延迟 30 秒；先发 4 条 `citation` 占满在途（2 条执行、2 条排队），再发指向不存在 `id` 的 `cancel`，之后发第 5 条 `citation`；另测：一条 intent 等待确认时再发一条 intent | `cancel` 照常 `accepted`；第 5 条 `citation` `overloaded`；第二条 intent `overloaded` |
| UB-31 | U6.6 | 10 秒内 5 次软违规 | 关桥；generation 不变；族被撤销；KnowledgeView `closed`（`bridge_closed`）；重新加载后新实例可用 |
| UB-32 | U6.6 | 同一 binding 10 分钟内第 3 次关桥 | 不再提供重新加载；从 Knowledge 设置里重新打开后才恢复 |
| UB-33 | U6.4 | 非 KnowledgeView 的 WebContents 1 分钟内发 10000 条；300 个不同来源各发 1 条 | 全部静默；日志按采样记录；跟踪的来源不超过 256 个，内存不随消息数增长 |
| UB-34 | U6.1 (e) | 子 frame 与刚被替换的旧文档监听宿主消息 | 都收不到 `hello`、`state`、`reply` |
| UB-35 | U6.7 | 1 秒内产生 20 次 `state` 更新与 10 条 `reply` | 页面收到的最后一条 `state` 是最新值；10 条 `reply` 全部送达 |
| UB-36 | U7.3 | 故障注入：宿主要发的消息带 `url` 键 | 宿主不发送，记日志；CI 的 schema 检查同样拒绝 |
| UB-37 | U7.2 | 页面在没有用户输入的情况下发合法 `return_to_chat_intent` | `forbidden/user_activation_required`；不切换、不加附件 |
| UB-38 | U7.2 | 同一对话已有 16 项草稿附件后再发带引用的 intent | `invalid_request/attachment_limit` |
| UB-39 | U7.2 | 合法 intent 被接受后用户 60 秒不确认；另一次用户点拒绝，并在 30 秒内再发 intent | 前者 `forbidden/confirmation_timeout` 并释放名额；后者先 `forbidden/user_declined`，冷却期内的 intent 得 `forbidden/cooldown`；都不加附件 |
| UB-40 | U7.2 | 用户刚在 KnowledgeView 里按键或点击后发合法 intent（2 条有效引用） | 宿主只高亮入口或弹出确认，不自动切换；确认界面显示"2 条"与宿主 read 得到的标题；用户确认后两条引用以草稿附件出现；没有发送消息、没有创建 run、没有审批弹窗 |
| UB-41 | U6.1 (f)、U7.2 | 只有 `mouseMove`、`mouseEnter`、滚轮或滚动手势之后发合法 intent | `forbidden/user_activation_required` |
| UB-42 | U6.3 | 页面错过 `hello`，发 `generation: 0` 的 `status`；同一文档发第 4 次 | 前 3 次各得一条 `hello`，不计违规；第 4 次 `stale_generation` |
| UB-43 | U6.7 | 同一文档发满 4096 个不同 `id` 后，重发最早的那个 `id` | 被淘汰的 `id` 当新消息处理（实现了时间戳替代手段时回 `duplicate_id`）；宿主内存不超过上限 |
| UB-44 | U7.2 | 主对话正在进行知识调用时，页面在在途上限内连发 `citation`，测试桩让 read 变慢 | 主对话的知识调用不被延迟；桥的 read 至多 2 个并发，其余按 FIFO 排队；排队超过 10 秒的回 `overloaded` 并带 `retry_after_ms` |
| UB-45 | U7.2 | `cancel` 一条正在排队的 `citation`；`cancel` 一条等待确认的 intent | 原消息都回 `rejected`（`cancelled`）；确认弹窗随之关闭；`cancel` 本身 `accepted` |
| UB-46 | U7.2 | 宿主 read 得到的标题含控制字符、U+202E 等 bidi 控制字符、HTML 标记，或长于 120 字符 | 确认界面按纯文本显示：去掉控制与 bidi 字符，截断到 120 字符，不解释标记 |
| UB-47 | U7.4 | 消息带 `port` 或 `Port` 键 | 先回 `forbidden_key`，再关桥 |
| UB-48 | U7.2 | 合法 intent 等待确认时，子用例：主 frame 重新加载；关桥（view 被销毁）；binding 进入 `unavailable`（T08）；另测：用户确认后 2 条引用中 1 条修订已过期 | 前三者确认弹窗关闭、名额释放、不加附件；`unavailable` 子用例回 `rejected`（`unavailable/not_ready`），前两者不送达任何 `reply`；另测只附加有效的那一条，回 `done`（`code: "stale"`） |

## 9. 现状证据（Agent24 `eb1a58b`）

1. **现有隔离 view（Creative）**：`apps/desktop/src/main/main.ts:113-120` 创建 WebContentsView，`contextIsolation`、`sandbox` 为真，`nodeIntegration` 为假，没有 preload；但分区是持久的 `persist:agent24-creative`（`apps/desktop/src/main/creative-serve-web.ts:64`），与 ADR-004:117 的非持久要求不符。弹窗与导航策略在 `main.ts:122-141`。隐藏只把 bounds 设成 1×1（`main.ts:84-89`），全仓库没有 `clearStorageData` 调用。KnowledgeView 按 D5 新做，不照抄这些代码。
2. **CSP 与请求钩子**：只在 `session.defaultSession` 上装了 `onHeadersReceived` 来设 CSP（`main.ts:256-270`）；Creative 分区既没有 CSP，也没有任何请求钩子。U4.5 的注入钩子是净新增。
3. **IPC 不校验 sender**：Creative 的处理函数忽略 `event`（`main.ts:274-281`）；`registerIpcHandlers` 里的处理函数都只看参数（`apps/desktop/src/main/ipc/index.ts:136-325`），没有 `senderFrame` 或 origin 校验；`DocumentsImportFile` 只用 `event.sender` 找窗口（`ipc/index.ts:298-299`）。通用 `backendProxy` 只允许 GET/POST/PUT/PATCH/DELETE，路径任意（只拒绝以 `//` 开头的，`ipc/index.ts:119-133`），由 main 带上 daemon token 转发（`ipc/index.ts:95`、`:316-325`），并经主窗口 preload 暴露给 renderer（`apps/desktop/src/main/preload.ts:37-38`、`:129`）。所以 U1.5 的私有路由不能只靠 daemon token 保护。主窗口关了 sandbox（`main.ts:166-173`）。
4. **令牌只在 main**：agent24d 的 token 取自 ready 行（`apps/desktop/src/main/backend-manager.ts:344-347`），由 main 放进 `Authorization` 头（`backend-manager.ts:153`、`ipc/index.ts:95`）；sidecar 就绪秘密要求至少 32 字节、无空白（`apps/desktop/src/main/sidecar-manager.ts:121-125`）。本文的票据、session JWT 与 `view_secret` 沿用"秘密只在宿主"的做法。
5. **系统凭据存储先例**：`rust/crates/agent24-comm/src/password_store.rs:259-285` 用 `keyring` 读写删，服务名 `ai.idoris.agent24.comm`（`:33`）；错误映射（`:292-297`）把 `NoEntry` 映射为 `NotFound`，其余错误都映射为 `Unavailable`（`keychain_unavailable`）。Knowledge 的 C 与登录态放在哪里由 KB-HOST-01 定（§10 #8、#9）。
6. **过期回调丢弃的先例**：`CreativeViewRequestFence`（`creative-serve-web.ts:68-89`）用自增计数丢弃过期的显示请求；本文的 generation 来自 STATE 草案，与它无关。
7. **Electron 34 的约束**（`apps/desktop/package.json:49`，`^34.5.8`）：非持久 session 无法销毁（U4.6）；webContents 既有 `before-input-event`，也有 `input-event`（Electron 34.5.8 的 `electron.d.ts` 约第 14385 行与第 15425 行），U6.1 (f) 用两者判断用户输入，`InputEvent.type` 的取值见同文件约第 8201 行，PoC 只验证它们对 WebContentsView 是否生效。Electron 34 的 `WebFrameMain` 只有 `frameTreeNodeId`、`processId`、`routingId`、`origin`、`url` 等属性（约第 17183 行起），没有文档令牌，文档身份来源是 KB-C2-04a 的 PoC 项（U6.1 (a)）；无 preload 时宿主→页面只能用 `executeJavaScript` 或 `executeJavaScriptInIsolatedWorld`（U6.1 (e)）。Session 有 `setPermissionRequestHandler`、`setPermissionCheckHandler`、`setDevicePermissionHandler`（约第 11964–12004 行），WebContents 有 `setWebRTCIPHandlingPolicy`（约第 16768 行），取值以该文件为准（U4.2）。设备选择事件：Session 的 `select-hid-device`、`select-serial-port`、`select-usb-device`（约第 11321、11343、11371 行），WebContents 的 `select-bluetooth-device`（约第 15730 行；第 15715–15729 行的说明写明不调用 `preventDefault` 时自动选第一个可用设备，回调传空字符串即取消）。客户端证书选择：app 的 `select-client-certificate`（约第 777–780 行）与 WebContents 的同名事件（约第 15748 行），不调用 `preventDefault` 时自动用证书库里的第一张证书（U4.2）。Electron 34 内置 Node 20，Node 20 的 `https` 默认只用内置的 Mozilla 根证书，不读系统证书库（以 KB-C2-04a 实测为准，§10 #32）。仓库里还没有 KnowledgeView、桥、票据或 session JWT 的任何代码。

## 10. 待决与需要评审确认的点

| # | 事项 | v1 默认 | 状态 |
|---|---|---|---|
| 1 | 票据默认 30 秒、最长 60 秒；JWT 默认 10 分钟、最长 15 分钟，`exp` 不晚于 C；C 剩余不足 60 秒拒签；2/3 寿命续期；长连接到 `exp` 为止 | 按本文（U2.3、U3.3、U3.4、U3.6） | 已定（盲选一致） |
| 2 | **盲选已定**：view 证明作纵深防御，`vph` 兼作族标识。**r2–r5 双审增补，待评审**：`vph` 按原始字节计算；族撤销时关闭长连接；先撤后丢；防不了日志泄露的说明；CSPRNG 与长度前缀的 H；撤销入口只接受同一 principal 的 Provider；按 `ticket_id` 只撤一张；族撤销后同一 `vph` 一律拒绝；ack 超时按 `vph` 撤族 | 按本文（U2.6、U3.7、U4.5、术语） | 前半已定；后半本 PR 评审 |
| 3 | **UI 路径上的族撤销自动重建 view，10 分钟内第 2 次后 KnowledgeView `closed` 等用户操作**（用户可见）。立场一（v1 默认）：自动重建一次，之后停下，兼顾无感与防循环；立场二：不设上限、每次都自动重建，用户无感，但在泄露或配置错误时会反复重建；立场三：第一次就停下等用户，最保守，打扰最多 | 立场一（U3.8） | 已定（David） |
| 4 | **盲选已定**：由宿主兑换并注入，页面看不到票据与 JWT。**r2–r5 双审增补，待评审**：只对 UI 白名单路径（WHATWG 规范化后匹配，含 `%2f`、`%5c` 不注入）的 `xhr`、`webSocket` 与主文档导航注入；注入前确认心跳新鲜；兑换成功后才 `loadURL`；每个请求先删头、大小写不敏感；逐跳判定；`details.frame` 为空不注入；注入前确认 sidecar ready；401 后由宿主自查（U3.8）；暂停注入只针对不可服务状态；兑换走 Node `https` | 按本文（U2.1、U3.5、U3.8、U4.5） | 前半已定；后半本 PR 评审；KB-C2-04a 验证钩子覆盖 |
| 5 | **票据失败对兑换方统一返回时用哪个 `reason`**。Opus：复用 `credential_invalid`，无须修订 READONLY；Sonnet：新增 `ticket_invalid`，作为兼容新增 | 复用 `credential_invalid`，不修订 READONLY；服务端审计用 `internal_reason` 区分；STATE 需在 S3.1 写明兑换与会话探测的响应除外（#12）（U2.7） | 已定（David） |
| 6 | **generation 每次递增都销毁 view 实例**，包括撤权后重签（T18）：页面会重新加载（对外行为） | 一律销毁（U4.8） | 已定（David）；KB-LITE-04① |
| 7 | **Off、取消选用、停用 profile 时是否销毁长期登录态**（对外行为） | 都不销毁，只在注销与 `retired`（T21、T23–T25）时销毁；全局收紧中的退役只作废（U4.9、U4.10，#34） | 已定（David）；与 STATE 草案 §10 #3 一致 |
| 8 | **宿主内部由谁铸造票据、调用撤销入口，跨进程作废通知怎么传**。C 留在 daemon 或系统凭据存储；票据若经 agent24d 交给 main，要新增 main↔daemon 私有接口（不进 openapi），且必须是 `backendProxy` 到不了的通道或凭据（U1.5）；U4.8 的 ack 与心跳也走这条接口。**涉及内核接口，需知会 jason** | 不定实现；C 不进任何 renderer（U1.4、U1.5、U4.8） | KB-HOST-01 |
| 9 | **长期登录态存在哪里**（Electron `safeStorage` 还是 agent24d 的 `keyring`）。若经 daemon API，**涉及内核接口，需知会 jason**。约束：所选存储必须可枚举，或维护键名索引 | 不定；临时键、`binding_id` 键、`revoke:` 墓碑与键名索引（U4.9、U4.11） | KB-HOST-01 |
| 10 | local-managed 的 binding 进入 `retired` 后，实例自己的秘密是否删除 | 不删，随实例由 KB-LITE-01a、KB-LITE-04② 管理（U4.11） | KB-LITE-01a、KB-LITE-04②；#997 |
| 11 | remote 签发权威撤销刷新令牌的接口与时限；失败后的持久重试队列（先写墓碑 `revoke:<binding_id>:<序号>` 再删登录态，存系统凭据存储，指数退避 1 分钟到 1 小时，至多 20 次或 7 天）[新设] | 按本文，界面如实提示（U4.11） | #992（外部依赖）；队列数值本 PR 评审 |
| 12 | **依赖 STATE 草案 r14（#1070–#1072）的规则**：可服务状态与术语表"连接阶段"（`degraded` 可服务，不暂停注入，U4.8）；S1.10 的不变式 I1–I8（I6 退役不复活、I7 Off 与注销不恢复成放开，对应 U4.9–U4.11）与 S1.14 的重启语义（全局收紧时登录态只作废，U4.9）；S1.13、T44（带 `unsaved_settings` 的 profile 重新登录，U4.9）；S2.4–S2.7；S3.1（撤权信号走 T18，处理时已不可服务走 T35a；凭据无效类走 T42；旧凭据撤权信号的待复验不准入，U6.4 ⑬；**STATE 仍需在 S3.1 写明"UI 兑换与会话探测的响应除外"**，r14 尚未写入；`credential_near_expiry` 不作为服务不可用交给 STATE，U2.1 ③、U3.8）；S3.2（ack 超时不得无限等待）；S4.2；S4.5（U4.7 的 ①② 发生在 S4.5 第 ① 步之内，main 回 ack 后 Provider 才进行第 ② 步，U4.8）、S4.6；S4.9（多个 binding 同时激活，view 按 binding 销毁，U4.8）；S5.9（拆成 U6.4 ⑬⑭ 两步；**STATE 仍需补一条例外**：hello 请求在桥未开放时也能拿到只含非秘密信息的 `hello`，U6.3，r14 尚未写入）；S5.3、T14、T19（取消选用）、T20（**STATE 仍需配合"先撤后丢"**：Off、停用、取消选用、注销时 Provider 先用 C 撤族再丢弃 C，U4.7 ③，r14 尚未写入）；S5.10（签发入口的 `forbidden/empty_scope`）；S1.6、S1.8；T02、T08、T10、T18、T21、T23–T25、T27、T31、T32、T35a、T38、T42。**签发入口的响应交给 STATE，r14 还接不住，STATE 仍欠三处**（U2.1 ③）：① S4.6 的接受判定与"由已接受响应派生"要覆盖签发入口的响应（STATE 的在途请求目前只有 search、read、能力发现、就绪）；② S5.10 判定签发入口的 `forbidden/empty_scope` 时，请求范围 Q 取 C 的范围（签发请求不带 Q）；③ S3.1 的凭据归属与旧凭据的撤权信号同样适用于签发请求（签发记下所用 C 的代号）。READONLY 若升到 r8，R7.3 的 null 回显配对同样适用于签发入口的 `unauthorized`，随 S4.6 一起修。**本文不新增 generation 递增事件**：页面引起的问题只做 view 级处理 | 按 STATE 草案 r14 | STATE 草案定稿时逐条复核 |
| 13 | 所有标 `[新设]` 的规则 | 按本文 | 本 PR 评审 |
| 14 | **无 preload 通道做不到 U6.1 的 (a)、(b)、(d)、(e) 时怎么办**：退到 ADR-004:119 的专用最小 preload，还是推迟桥。前者偏离 D5 与 KB-C2-03 的"无 preload" | 桥不开放，KnowledgeView 只显示、不收发消息 | 已定（David，先按此执行；KB-C2-04a 实测后再议） |
| 15 | **`return_to_chat_intent` 是否允许带自由文本草稿**（产品范围） | v1 不带自由文本（U7.2） | 已定（David）；KB-C1-05 |
| 16 | 四种类型只约束页面→宿主；宿主→页面另设 `hello`、`state`、`reply`；信封 `{ v, direction, type, id, generation, in_reply_to?, payload }` | 按本文（U6.2、U7.1、U7.3） | 已定（盲选一致） |
| 17 | **关桥后需要用户重新加载，10 分钟内第 3 次后不再提供重新加载**（用户可见）。立场一（v1 默认）：交给用户决定何时恢复，防止恶意页面造成重建循环；立场二：冷却一段时间后自动重开，用户无感，但被注入的页面可以周期性地触发违规 | 立场一（U6.6）；端口变化、通道恢复与 binding 级销毁引起的关桥不计入；⑬ 不计入；到达限速、静默计数与日志采样、overloaded 宽限期、去重表等数值按 U6.4、U6.6、U6.7 | 已定（David）；数值本身为工程值，取较严 |
| 18 | **盲选已定**：封闭 schema 加禁止键（任意深度、规范化比对、两边并集）、CI 检查 schema、静默丢弃与硬违规的范围、`reply` 的码。**r2–r5 双审增补，待评审**：清单加 `port`；规范化后精确匹配、CI 反例；硬违规先回 `reply` 再关桥；被拒的 `id` 不登记；`v` 缺失落到 ⑫；hello 请求；去重表 4096 项 FIFO；`direction` 改名（已确认） | 按本文（U6.4–U6.6、U7.4、U7.5） | 前半已定；后半本 PR 评审 |
| 19 | 可选的宿主→页面 `reveal_citation`（让页面定位到某条引用） | v1 不加，以后按兼容新增 | 已定（David） |
| 20 | **UI 路径上族或 `jti` 撤销用哪个 `reason`**：宿主要能把它与 C 撤销区分开（U3.8）。选项：复用 `credential_invalid`（与签名、证明错误同一处理，无须修订 READONLY）；或新增兼容值（如 `session_revoked`） | 复用 `credential_invalid`（U3.4） | 已定（David）：复用 `credential_invalid`，与 #5 一并 |
| 21 | 引用 ID 的字符集要与 READONLY R5.2（不透明标识）同步；同步之前先采用默认禁止规则：拒绝含 `/`、`\`、`:`、`..` 或空白的 ID（`schema_violation`，不计违规；长度或控制字符不符才是软违规，U6.6、U7.5）。容量验算的前提（ID 不含 `"`，8 条上限不降）已获认可 | 默认禁止规则（U7.1） | READONLY 下一修订；#1012 |
| 22 | `return_to_chat_intent` 的用户输入窗口 5 秒（只认激活类事件）、对话内附件上限 16 项、确认等待 60 秒、拒绝后冷却 30 秒、桥预算至多 2 个并发且 FIFO 排队（排队上限 10 秒）、标题纯文本 120 字符 [新设]；`before-input-event` 与 `input-event` 对 WebContentsView 是否生效待 PoC | 按本文（U7.2） | 本 PR 评审；KB-C2-04a |
| 23 | Electron 34 下非持久 session 无法销毁：每进程至多 64 个 KnowledgeView 分区；只在同一 binding 内复用；清理四步任一失败即不复用、不新建；满额时拒绝并提示重启 [新设] | 按本文（U4.6） | 本 PR 评审；KB-C2-03 实测 |
| 24 | CSP 的 `worker-src 'none'` 会挡掉合法的 Web Worker；WeKnora 嵌入页是否依赖 Worker 待实测 | 按本文禁用（U4.2） | KB-C2-03 实测 |
| 25 | **READONLY 需同步修订**：R1 与 R2.7 写明 session JWT 的 `ops`（KB-C2-01 的 UI 操作清单）与 C 的数据操作范围之间的关系，以及"C 可用于 UI 票据的签发与撤销，不计入数据操作范围"（U1.3） | 在 #25 定之前，KB-C2-01 不得实现超出 search/read 的 UI 操作 | READONLY 下一修订；**阻塞 KB-C2-01 的实现** |
| 26 | KB-C2-01 是否在 UI 响应头里回显 `unauthorized` 的 `reason`（可选义务），让宿主省掉自查 | v1 不依赖，按 U3.8 自查 | 已定（David）：可选义务，v1 不依赖；KB-C2-01 |
| 27 | CSP 下限之外的放宽：`img-src`、`font-src` 等是否放行 `data:`，`style-src` 是否需要 `'unsafe-inline'` | 不放行（U4.2） | 待决；KB-C2-03 实测 |
| 28 | 服务端票据消费与撤销记录的持久化；做不到时重启轮换 UI 会话密钥（local-managed 派生加重启纪元）；多副本用共享存储或粘性路由 [新设] | 按本文（U2.3） | 本 PR 评审；KB-C2-01 |
| 29 | 新 `reason` `credential_near_expiry`（`unavailable`，可重试）：C 剩余寿命不足 60 秒时拒签 | 按本文（U2.1 ③） | READONLY 下一修订；#1012 |
| 30 | **WebRTC 的残余风险**：CSP 管不到 WebRTC，Electron 34 没有整体禁用的开关；`disable_non_proxied_udp` 只限制 UDP 与本机地址暴露，被注入的页面仍可能经 TCP 中继向外连接 | v1 设 `disable_non_proxied_udp`（U4.2），接受残余风险 | 本 PR 评审；KB-C2-03 实测 |
| 31 | **宿主侧的恢复与心跳策略**（用户可见）：崩溃 10 分钟 3 次后 `crash_loop`；心跳丢失时对全部 binding 全局销毁；ack 2 秒、心跳 500 毫秒、丢失判定 2 秒、注入新鲜度 1 秒、唤醒宽限 5 秒 | 按本文（U4.5、U4.8） | 已定（David）：按本文，数值可按实测调整 |
| 32 | **Node `https` 的信任库与代理**：Electron 34 内置的 Node 不读系统证书库，企业内网自签 CA 与系统代理不会自动生效 | v1 用 Node `https`，在内置根证书之外加载 profile 配置的 CA 证书包，代理按 profile 配置；是否改用 Chromium 网络栈并在服务端识别重发，由 KB-C2-04a 实测后定 | KB-C2-04a |
| 33 | **T42 续签也失败时 KnowledgeView 怎么处理**：STATE r14 中这种情形记一条适用全部 KB 的占位规则，但不递增 generation（进入 `unavailable`/`credential_rejected`，不自动重试）。盲选时 Grok 主张按 binding 级销毁，Opus 主张只暂停注入 | 只暂停注入，不关桥、不销毁，桥按 U6.4 ⑬ 回 `not_ready`；view 上加不透明遮罩"不可用，需要重新授权"；回到可服务状态且 generation 未变（T38 或 T10）时撤下遮罩继续，桥照常接受消息；确认撤权或 generation 递增时按 binding 级销毁（U4.8）；宿主删除单个 KB 不处理 view | 已定（David） |
| 34 | **全局收紧（STATE S1.14）中被作废的长期登录态何时清理**：全局收紧时只作废、不销毁 | 该 profile 经 T44 重新登录、新 binding 持久化后按 U4.11 销毁；profile 被移除时同样销毁；此前保留不用（U4.9） | 已定（Grok、Opus 盲选一致） |

## 变更记录
- r0（2026-10-17）：首稿。由页面兑换票据并在内存持有 JWT；view 证明；分区与登录态销毁；桥信封含 `view_epoch`；验收 UT-01–UT-25、UB-01–UB-32；待决 #1–#18。影响夹具：否。
- r1（2026-10-17）：按 B 类盲选（Opus 与 Sonnet 各自作答、都没看草稿；依据 David 2026-10-17 授权的夜间规则）修订。改为宿主兑换并经分区钩子注入，页面看不到票据与 JWT；补寿命细节、族、单飞、Service Worker 禁用；票据失败统一为 `credential_invalid`（#5 待 David）；桥信封改为 `{ v, direction, type, id, generation, in_reply_to?, payload }`，宿主→页面为 `hello` / `state` / `reply`；禁止键取并集；软硬违规与关桥；数值取较严。影响夹具：否。
- r2（2026-10-17）：按第 1 轮双审互评修订。页面引起的问题只做 view 级处理，不递增 generation；UI 路径的 `unauthorized` 分三种处理；注入钩子先删头、只对 UI 白名单路径注入、origin 映射、逐跳判定；remote 只允许 https；Electron 34 session 累积的缓解；登录态临时键；`view_secret` 用 CSPRNG；撤销入口限同一 principal；关桥不递增 generation；`overloaded` 不算违规；`cancel` 不占名额；宿主→页面排队合并；`return_to_chat_intent` 要求用户输入、附件上限、不自动切换；引用 ID 只校验长度与控制字符；补 UT 与 UB。§10 #3、#17 改为待 David，新增 #20–#23。影响夹具：否。
- r3（2026-10-17）：按第 2 轮双审互评修订。UI 路径的 401 由宿主自查；跨进程作废协议；分区映射与同 binding 复用；CSP 下限；生产构建禁用 DevTools；崩溃退避；私有路由须 `backendProxy` 到不了；到达限速前移、静默计数与日志采样；`overloaded` 宽限期；`status` 只用于显示，`citation` 只用于展示；ID 改为 128 字节并写出容量验算；补 UT-01–UT-42、UB-01–UB-40。§10 #2、#4、#18 拆分，新增 #24–#27。影响夹具：否。
- r4（2026-10-17）：按第 3 轮双审互评修订。ack 超时撤族并断开私有通道；签发入口的响应按 S3.1 交 STATE，兑换与探测的响应不进入 STATE；持久重试队列；`degraded` 不暂停注入；兑换走 Node `https`；族撤销后同一 `vph` 一律拒绝；服务端记录持久化；文档身份列为 PoC 项；只认激活类输入；hello 请求；去重表上限；补 UT-43–UT-50、UB-41–UB-44，新增 #28、#29。影响夹具：否。
- r5（2026-10-17）：按第 4 轮双审互评修订。第一片：签发入口的撤权信号一律走 T18（`degraded` 也一样），删去 T05 例外；分区权限一律拒绝，WebRTC 设 `disable_non_proxied_udp`；先用 C 撤族再丢弃 C，C 不可用时跳过并靠断开通道兜底；撤销时服务端关闭长连接（做不到则 60 秒重验）；注入与桥都要求心跳不超过 1 秒，写出注入窗口上界（3 秒 + 通道时延）；休眠唤醒宽限；墓碑键 `revoke:<binding_id>:<序号>`，先写墓碑再删登录态，指数退避；U3.8 改为处置表；兑换成功后才 `loadURL`；键名索引；`vph` 按原始字节计算；路径按 WHATWG 规范化匹配；Node `https` 的信任库与代理；DevTools 与 fuses；补 UT-51–UT-56。第二片：⑬ 加心跳新鲜度，`not_ready` 不计违规；hello 请求是 S5.9 的例外；桥 read 在预算内 FIFO 排队，在途计数含排队，排队超过 10 秒才 `overloaded`；`cancel` 使原消息回 `rejected`（`cancelled`）并关闭确认弹窗；标题纯文本、去控制与 bidi 字符、截断 120 字符；引用 ID 默认禁止 `/`、`\`、`:`、`..` 与空白（软违规）；禁止键加 `port`；`userGesture` 为 false；补 UB-45–UB-47，改 UB-11、UB-18、UB-30、UB-44。新增 #30–#32，#9、#11、#12、#17、#21、#22 同步。影响夹具：否。
- r6（2026-10-18）：按第 5 轮裁定与 David 的决定修订，并对齐 STATE 草案 r14。第一片：注销标记写进键名索引，墓碑与标记都写失败时删除条目；设备选择事件（蓝牙、HID、串口、USB）一律 `preventDefault` 并取消，fuses 加 `RunAsNode`；签发入口的 `unauthorized` 按 S3.1 分流（撤权信号走 T18 或 T35a，凭据无效类走 T42），补 `credential_expired`、`forbidden/empty_scope` 的处置；C 变化时换发 JWT；JWT 固定算法；⑥ 不成立时取消请求，心跳恢复不重载；崩溃残留的 `pending:` 先撤销再删；多个 binding 同时激活时按 binding 销毁 view；全局收紧中的登录态只作废，T44 后销毁；"切换离开"改为"取消选用"；私有通道双向认证；补 UT-57–UT-60，改 UT-13、UT-28、UT-36、UT-40–UT-42、UT-51、UT-55。第二片：⑬ 含待复验；引用 ID 默认禁止规则不计违规；`in_reply_to` 在 `reply` 中必填；改 UB-17、UB-18。§10 #3、#5、#6、#7、#14、#15、#17、#19、#20、#26、#31 按 David 的决定改为已定，#12 改按 STATE r14，新增 #33（David 定：T42 续签也失败时暂停注入并加遮罩，不销毁 view）、#34；补 UT-61。影响夹具：否。
- r7 第一片（2026-10-20）：按 PR-Daemon 对 #1076 的评审（与 #1077 B2）修订。M1：同一 binding 重新登录（T02、T38）时，在登记键名的同一次原子索引更新里清除注销标记与"待删"记录；旧令牌尚未写进 `revoke:` 墓碑时先补写再覆盖，补写失败本次登录按失败处理；已有墓碑照常重试、不判新条目失效；U4.9 写明 STATE 记为已注销的 binding 其条目一律不用；UT-40 补"删除失败 → 重新登录 → 重启"与"墓碑写入失败 → 重新登录"。B2（= #1077 B2，取推荐的第一种）：T42 续签也失败那一支删去"关桥"，改为桥按 U6.4 ⑬ 回 `not_ready`；UT-61 补恢复后合法 `citation` 得到 `accepted`，§10 #33 同步。L4：遮罩在回到可服务状态且 generation 未变（T38 或 T10）时撤下。L0：探测或兑换重试得到其他结果即解除暂停、计数清零，UT-55 补 503 后成功。L1：只因 C 被替换而暂停的（如 T27），换发后继续注入不重载，UT-57 同步。L2：`pending:` 条目的墓碑键 `revoke:pending:<login_attempt_id>:<序号>`，UT-41 同步。L3：签发记下所用 C 的代号、在途期间 C 被替换按 S3.1 凭据归属处理，S5.10 的 Q 取 C 的范围，并入 §10 #12 的"STATE 仍欠"。L5：键名索引记所属 profile id，作废条目按它归属，profile 已不存在的按移除销毁，UT-59 补持久状态损坏。L6：票据记录不比消费、撤销记录更持久，UT-49 补跨重启重放。L7：UT-03 写明并发兑换后整族被撤销。N：UT-61 只由签发入口触发 T42；UT-34 的"sidecar 不重启"用 local-managed 测；"宿主删除单个 KB 不处理 view"写进 U4.8；§10 #12 补 T02、T10、T27。影响夹具：否。
- r8 第二片（2026-10-20）：按 PR-Daemon 对 #1077 的评审修订（B2 已在 r7 第一片解决，U6.4 ①、U6.6 不动）。B1：`in_reply_to` 只能出现在 `reply` 里，只有触发消息通过 ⑧ 且顶层 `id` 是合法 ULID 时才必填，否则省略（⑥ 的 `overloaded`、⑦、⑧，以及 `id` 缺失或不是 ULID 的），宿主不编造占位值；U7.1、U6.4 同步；UB-13、UB-14 补"`reply` 不带 `in_reply_to`"，UB-17 补缺少 `id`、`id` 不是 ULID、非 `reply` 带 `in_reply_to` 三项并改 CI schema 检查的期望，UB-28 补 ⑥ 的 `overloaded` 不带 `in_reply_to`。L1：KnowledgeView 的 `select-client-certificate` 一律 `preventDefault()` 并 `callback()` 不传证书（U4.2），§9 第 7 条补事件位置，UT-51 补子用例。L2：等待确认的 intent 遇文档切换、view 销毁（含关桥）、binding 离开可服务状态或准入关闭时关闭弹窗、释放名额、回 `rejected`（`unavailable/not_ready`，前两种不送达）；确认后部分引用通过时只附加通过的项，回 `done` 并带第一项未通过的 `code`；U7.5 补一行，新增 UB-48。L3：UB-11 补"待复验未解除"子用例。L4：UB-30 的 `cancel` 改为指向不存在的 `id`。L5：§10 #21 改为命中默认禁止规则不计违规。Nit：§10 #14 改为"做不到 U6.1 的 (a)、(b)、(d)、(e) 时"；UB-02 的"切换到 binding B"改为"取消选用 A、改选 binding B"。影响夹具：否。
