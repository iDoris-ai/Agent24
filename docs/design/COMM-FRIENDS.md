# COMM-FRIENDS：通信好友机制（Hyphae / Nostr）

索引：[架构总入口](../architecture/README.md) · [ADR 索引](../adr/README.md)

状态：**设计稿 r2，已拍板（2026-11-02，jason：§8 六条全部按推荐）** · 只写设计，不含实现；实现排在 ADR-038 C1–C3 之后（见 §10）。r2 = Sonnet 评审（2 High、9 Medium、6 Low）处置：Q3 收窄到裸 Hyphae 对端、状态机补全、自动转移的写入规则、去重改为 event_id seen-set、CF-1/2/3 边界。
上位：[ADR-039 概念模型](../adr/ADR-039-concept-model.md) §5 末句「不允许为新东西发明新的顶层名词」：好友、陌生人、屏蔽都是「通信」这项能力里联系人的**状态**，不是新概念）；[ADR-041 入站事件与准入](../adr/ADR-041-inbound-events-admission-and-work-items.md)（好友 / 陌生人属于**通道准入**，不是执行授权）
基线：Agent24 `63a480ae`（含 [#1535](https://github.com/iDoris-ai/Agent24/pull/1535) 通信页可用性 + NIP-49 备份）；Hyphae **v0.28.1**（`44189b8e`）。现状设计见 [COMM-HYPHAE](COMM-HYPHAE.md)、[HYPHAE-CLI-INTEGRATION](HYPHAE-CLI-INTEGRATION.md)、[F4 Nostr 通道](../specs/F4-nostr-channel.md)。
路线图：本文 §10（`docs/agent/roadmap.md` 已标注「本文档描述的不是当前在做的事」，不往里写；组件视图见 [COMPONENT-ROADMAP §C8](../agent/COMPONENT-ROADMAP.md)）。

## 0. 一句话

把「拿到 npub 再加联系人」改成「加某某为好友」：申请就是第一条消息，对方同意后互为好友；好友消息进主列表，非好友消息进「陌生人消息」，屏蔽才静默丢弃。**协议归 Agent24，传输归 Hyphae**：好友申请、同意、撤回都是带类型的 NIP-44 加密消息，走 Hyphae 现有的 kind 30078，Hyphae 不需要改协议，只需要三处小配合（§9）。

## 1. 现状（2026-11-02）

| 事项 | 现状 | 出处 |
|---|---|---|
| 联系人 | Hyphae `contact add / list`，只有这两个子命令，**没有 remove / update**；存在 `keystore.json` 的 `Contacts`（按昵称为键），字段 `nickname / npub / added_at / role(human\|agent)` | Hyphae `internal/identity/commands.go:310-425`，`pkg/types` `Contact` |
| 联系人的语义 | 纯通讯录：只是把昵称映射到 npub，**不需要对方同意**；发消息可以填联系人名，也可以直接填任意 npub | #1535 正文；`Communication.tsx:402-410`；Hyphae `identity.ResolveRecipient` |
| 收件 | daemon 把**任何人**发给我的消息都解密落库，联系人只用于显示昵称；没有名单过滤 | Hyphae `internal/daemon/daemon.go:856`；`history inbox` 返回 `StoredMessage{id, sender_npub, recipient_npub, content, plaintext, created_at, is_encrypted}` |
| 消息格式 | kind 30078，标签 `p=<收件人 hex>`、`c=agent`、`v=v1`、`z=zstd`、`enc=nip44`、`d=agent-message:<token>`；content = zstd(NIP-44(明文)) | Hyphae `internal/messaging/agent_event.go`，[kind-30078 wire contract](https://github.com/iDoris-ai/Hyphae/blob/v0.28.1/docs/agent/kind-30078-wire-contract.md) |
| 带类型消息的先例 | 群聊：明文以保留前缀 `hyphae.group/v1\n{json}` 开头，daemon 按 `type`（invite / accept / decline / activate / cancel / message）分发，**不进普通历史**；其余明文一律当普通聊天 | Hyphae `internal/messaging/verified_agent.go:12`、`group_dispatch.go`、`internal/daemon/inbox.go:22` |
| 地址 | 页面显示我的 npub 加复制按钮，提示「把这个地址发给同事」；没有二维码、邀请链接、NIP-05 | #1535 |
| 备份 | NIP-49 `ncryptsec` 只备份私钥；联系人不在备份里 | #1535 |
| 收件箱 | 一个平铺列表，每条可「回复」，发件人不在联系人里可「加为联系人」 | `Communication.tsx:561-575` |
| agent 自动执行 | nostr-bridge 的 `A24_NOSTR_ALLOWED_NPUBS` 白名单，fail-closed；F4b 默认冻结（`A24_NOSTR_F4B_INBOUND=1` 才分派 run）；comm 模块 zero-run | `packages/nostr-bridge/src/config.ts:57-62`、`inbound.ts:52-60`；COMM-HYPHAE §7 |
| Hyphae 没有的 | NIP-05、NIP-51 列表、屏蔽 / 静音、联系人删除、`history inbox` 游标（群聊 history 有 `--after-id`，单聊没有） | 对 v0.28.1 源码 grep `nip05 / nip51 / mute / contact remove` 零命中 |

## 2. 参考：主流产品的好友、陌生人消息、删除与屏蔽

依据标注：有公开帮助文档的给链接；没有的标「据产品行为」（按 2026 年的产品实际表现记录，没有找到官方逐条说明）。

| 产品 | 建立关系 | 非好友 / 陌生人能否发消息，怎么展示 | 删除 | 屏蔽 | 依据 |
|---|---|---|---|---|---|
| **微信** | **双向**：发申请（带验证语），对方通过后互为好友；申请期间申请方只能发那一条验证语 | 不能。非好友发消息会收到「开启了朋友验证，你还不是他（她）朋友」 | **单向**：A 删 B 后，B 仍在自己列表里看到 A，但发消息被拒；B 再申请，A 若没删 B 则自动通过。删除不通知对方 | 拉黑后对方发的消息显示「消息已发出，但被对方拒收了」；不通知 | 据产品行为 |
| **Telegram** | **单向**：加联系人不需要对方同意（按手机号 / 用户名）。联系人只是通讯录 | 能。任何拿到用户名的人都能私信；第一条消息上方出现「加为联系人 / 屏蔽 / 举报」条。Premium 用户可设「新私聊来自非联系人 → 归档并静音」或「只允许联系人和 Premium 用户私信」 | 单向，聊天记录保留，对方不知情，仍能给你发消息 | 服务端拦截：被屏蔽者的消息**不送达**，不被通知，看不到你的在线状态和头像 | [Telegram FAQ](https://telegram.org/faq)；隐私设置细节据产品行为 |
| **LINE** | **单向加好友**（ID、二维码、手机号、邀请链接）。对方那边出现在「可能认识的人」；对方也加你才是双向 | 默认能：非好友消息进同一列表，顶部提示「此用户不是你的好友」并给「加好友 / 封锁 / 举报」。隐私里有「拒绝接收非好友消息」开关，开了就**收不到** | 从列表隐藏或删除，对方不知情；对方消息按非好友处理 | 封锁后对方消息不送达，对方看到的仍是「已发送」，不被通知 | [LINE 帮助中心](https://help.line.me/)；具体文案据产品行为 |
| **Signal** | 按手机号 / 用户名；没有「好友」，有「消息请求」 | 第一次来自非联系人的消息显示为**消息请求**：看得到内容，但在你「接受」前对方看不到你的资料、已读、正在输入；可「接受 / 删除 / 屏蔽」 | 没有删好友；可删除会话或移除联系人，对方不知情 | 被屏蔽者消息和来电不送达，不被通知 | [Signal 支持：Message Requests](https://support.signal.org/hc/en-us/articles/360007459591)、[Block](https://support.signal.org/hc/en-us/articles/360007320391) |
| **WhatsApp** | 按手机号；没有申请流程 | 能。陌生号码第一条消息带「屏蔽 / 举报 / 加为联系人」条；近年加了「静音陌生来电」和垃圾信息限制 | 从手机通讯录删，单向，会话保留 | 被屏蔽者消息停在单勾不送达，看不到你的最后上线 / 头像 / 状态；不被通知 | [WhatsApp FAQ：Block and unblock](https://faq.whatsapp.com/) |
| **Slack** | 没有好友，按**工作区成员**和频道成员；跨组织用 Slack Connect，发起方邀请、对方组织**接受**后才能互发 | 同工作区任何人都能私信；外部人要先经 Slack Connect 邀请 / 接受，管理员可审批 | 退出频道 / 工作区；没有「删好友」 | **没有屏蔽个人的功能**（只能静音、举报、找管理员） | [Slack Help：Slack Connect](https://slack.com/help/articles/1500001422062)；无屏蔽据产品行为 |
| **Nostr 客户端**（Damus、Amethyst 等） | kind 3 关注列表，**公开、单向** | 私信按「是否关注对方」分两个标签：Messages / **Requests**（或 Known / Unknown）；陌生人消息不通知 | 取消关注即可 | NIP-51 kind 10000 静音列表（可有 NIP-44 加密的私有段），**只在客户端生效**，发送方不受影响 | [NIP-51](https://github.com/nostr-protocol/nips/blob/master/51.md)、[NIP-17](https://github.com/nostr-protocol/nips/blob/master/17.md)；标签分法据产品行为 |

**提炼出来的共识**，也是本文方案的来源：

1. **双向好友**只有微信这一家，其他都是「单向联系人 + 陌生人消息分流」。jason 要的心智是微信式的「申请 → 同意 → 互为好友」，但传输层是 Nostr，更像后四家：**谁都能给你发，过滤只能在接收端做**。所以方案是「微信的关系模型 + Signal / Nostr 的消息请求分流」。
2. **删除和屏蔽在所有产品里都是两件事**：删除是「我不把你当联系人了」，单向、不通知，对方消息降级为陌生人消息；屏蔽才是「我不想再看到你」，不通知，消息不送达或不展示。
3. **被屏蔽的人从不被告知**，所有产品一致。Nostr 没有服务端拦截，所以我们的屏蔽只能是客户端静默丢弃，对方看到的仍是「已发出」——这和 LINE、WhatsApp 对被屏蔽者的呈现是一样的。
4. **陌生人消息不通知**（Signal、Nostr 客户端、Telegram Premium 归档），且提供「只收好友消息」的开关（LINE、Telegram Premium）。

## 3. Agent24 的方案

### 3.1 名词与边界（遵守 ADR-039）

- 对外只用**联系人**这一个名词，它有几种**状态**：好友、申请中、陌生人、已删除、已屏蔽。「加好友」「陌生人消息」「屏蔽」是界面文案，不是新的顶层概念。
- 好友关系是**每一端自己的本地记录**。Nostr 上没有「关系」这个对象，所谓「互为好友」只是双方各自把对方记成了好友。所以「对方把我删了」我永远不知道，这和微信一样。
- **好友 ≠ 可以指挥我的 agent**（§3.9）。
- 好友协议的全部处理是**确定性代码**，不经模型；它扩展而不是打破 COMM-HYPHAE §7 的 zero-run 判据。

### 3.2 状态机

每个对端 npub 一条记录，状态如下。没有记录 = 陌生人。「已删除」在转移上和陌生人完全一样，只多保留 `name` 和 `removed_at` 便于识别。

```
  陌生人 / 已删除 ──发出申请──▶ 申请中(我发出)
  陌生人 / 已删除 ──收到 request──▶ 申请中(我收到)

  申请中(我发出) ──收到 accept(id 匹配)──▶ 好友
  申请中(我发出) ──收到 request（两边同时申请）──▶ 好友（并回 accept）
  申请中(我发出) ──裸 Hyphae 对端的普通回复(Q3，见 §3.4)──▶ 好友（并补发 accept）
  申请中(我发出) ──收到 decline(id 匹配)──▶ 申请中(我发出)，记 peer_declined_at，界面显示「对方拒绝」
  申请中(我发出) ──撤回──▶ 陌生人
  申请中(我发出) ──30 天未回应（扫描时惰性判定）──▶ 陌生人

  申请中(我收到) ──我同意 / 我点「加为好友」（两者同一个动作）──▶ 好友（并发 accept）
  申请中(我收到) ──我拒绝──▶ 陌生人，记 declined_at
  申请中(我收到) ──收到 withdraw(id 匹配)──▶ 陌生人
  申请中(我收到) ──30 天未处理（扫描时惰性判定）──▶ 陌生人

  好友 ──删除──▶ 已删除
  任何状态 ──屏蔽──▶ 已屏蔽（申请中的记录静默丢弃，不发任何消息）
  已屏蔽 ──解除屏蔽──▶ 陌生人（原本是好友也要重新申请）
```

| 状态 | 含义 | 本地记录 |
|---|---|---|
| 陌生人 | 没有关系 | 无记录，或只有 `declined_at` / `last_request_at` / `speaks_protocol` |
| 申请中（我发出） | 我发了 `request`，等对方 | `request_id`、`sent_at`、可选 `peer_declined_at` |
| 申请中（我收到） | 对方发了 `request`，等我 | `request_id`、`note`、`name`、`received_at` |
| 好友 | 双方（据我所知）互为好友 | `since`、`name`、Hyphae 联系人昵称 |
| 已删除 | 我删了对方；对方可能还把我当好友 | `removed_at`，保留 `name` 便于识别 |
| 已屏蔽 | 不看、不收、不回 | `blocked_at` |

每条记录另有一个布尔 `speaks_protocol`：只要从这个 npub 收到过**任何一条**合规的 `agent24.friend/*` 消息就置 true，永不清零（删除、屏蔽、解除都不清）。它只用来判断 Q3 适不适用（§3.4）。

**自动转移时写什么、发什么**（评审 M1）：

| 转移 | 写 Hyphae `contact add` | 发 `accept` | 需要再次确认吗 |
|---|---|---|---|
| 用户点「同意」/「加为好友」（申请中·我收到 → 好友） | 是 | 是，`id` = 对方的 request id | 是：主进程原生确认（写 keystore） |
| 收到 `accept`（申请中·我发出 → 好友） | 是 | 否 | 否 |
| 两边同时申请（申请中·我发出 + 收到 request → 好友） | 是 | 是，`id` = 对方的 request id | 否 |
| Q3 裸对端普通回复（申请中·我发出 → 好友） | 是 | 是，`id` = **我自己**的 request id（对方没发过 request，没有别的 id 可引用） | 否 |

规则：用户点「加为好友」时过的那一次主进程原生确认，就是对上面三种**无需再确认**的后续动作的授权——它们都只是「我申请的这段关系成立了」的结果，`contact add` 写的也是用户当时亲手填的那个 npub。**两端都在变成好友的那一刻各自执行 `contact add`**，所以互为好友后双方 keystore 里都有对方。`contact add` 失败（keystore 锁住、昵称撞名已按 §4 规则规避）时状态照常变好友，记一条待补写，下次扫描重试；发不出 `accept` 时按 COMM-HYPHAE §5.3 入 outbox，不回滚状态。

### 3.3 消息协议：带类型的 NIP-44 加密消息

**原则**：复用 Hyphae 现有的 kind 30078 消息（标签 `c=agent / v=v1 / enc=nip44 / z=zstd`），**协议信息放在加密后的明文里**，relay 和旁观者只能看到「A 给 B 发了一条 30078」，看不到这是申请还是聊天。做法照抄 Hyphae 群聊的保留前缀机制（`hyphae.group/v1\n{json}`），只是换一个 Agent24 自己的命名空间：

```
agent24.friend/v1
{"type":"request","id":"3f9c…(16 hex)","name":"Jason","note":"我是 iDoris 的 jason，加个好友","ts":1762041600}
```

| `type` | 发送时机 | 字段 | 收到后的处理 |
|---|---|---|---|
| `request` | 用户点「加为好友」 | `id`（16 hex，随机）、`name`（≤ 64 字）、`note`（≤ 200 字，可空）、`ts` | 陌生人 / 已删除 → 申请中（我收到），但 `declined_at` 在 7 天内的静默丢弃（§3.8）；申请中（我发出）→ 好友并回 `accept`；好友 → 回 `accept`（对方可能重装了）；已屏蔽 → 丢弃；申请中（我收到）→ 覆盖旧申请（同一 npub 只保留一条） |
| `accept` | 用户点「同意」；或 §3.2 表里的两种自动补发 | `id`（对应的 request id；Q3 补发时是**我自己**的 request id）、`name`、`ts` | 申请中（我发出）且 `id` 匹配 → 好友；其他状态或 `id` 不匹配 → 忽略并记日志（防止凭空「被同意」） |
| `decline` | **可选**，用户选「拒绝并告知」（Q2） | `id`、`ts` | 申请中（我发出）且 `id` 匹配 → 仍是申请中（我发出），记 `peer_declined_at`，界面显示「对方拒绝」，7 天内不能重发；其他 → 忽略 |
| `withdraw` | 用户撤回申请 | `id`、`ts` | 申请中（我收到）且 `id` 匹配 → 陌生人，静默移除；其他 → 忽略 |

任何一种类型只要解析合规，就把该 npub 的 `speaks_protocol` 置 true（即使随后因状态不符被忽略）。

规则：
- 没有单独的 `card` 类型：profile 卡片就是 `request` / `accept` 里的 `name`（将来可加 `avatar_sha256` 之类字段，v1 不做）。**不用 Hyphae `profile publish`**，那是公开发到 relay 的，谁都能读；好友之间的名片应该走加密消息。
- 整条明文上限 4 KiB，`name` / `note` 过 #1535 的昵称规则（拒绝 Cc / Cf 控制字符，保留 ZWNJ / ZWJ），JSON 禁止重复键、禁止未知顶层字段以外的解析歧义；任何一处不合规 → 当作**普通文本**进陌生人消息（不丢，但不改状态），这样格式错误不会变成拒绝服务。
- 发送用 `agent msg --content-file`（v0.28.1 已有，同时解决 COMM-HYPHAE G9「正文进 argv」），不用 `--content`。
- 事件 id 就是去重键。Agent24 在 `friends.json` 里持久化一个有上限的已处理 `event_id` 集合（LRU，2 000 条），**不用 `created_at` 当游标**（它是发送方自报的时间，可伪造、可乱序；同秒多条也分不开）。同一申请重放不会二次改状态。扫描方式见 §6。
- **与 Hyphae 的共存**：`agent24.` 前缀不以 `hyphae.` 开头，Hyphae 按 `AgentRouteDirect` 当普通聊天落库，`history inbox` 原样返回，**Agent24 的 `GET /comm/history` 把这些行过滤掉并交给状态机**。Hyphae 不需要新增字段或子命令就能跑通；要它配合的只有：承诺不占用 `agent24.` 命名空间并写进 wire contract，TUI 显示时把这类行折叠成一句人话（§9 HY-F3）。
- **对端不是 Agent24**（裸 Hyphae CLI / TUI 用户）：在 HY-F3 把这类行折叠之前，他看到的就是上面那两行原文（前缀 + JSON），`note` 是人话，能看懂；他直接回一句话 → 按 Q3 当作同意。我们补发的 `accept` 他同样会以原文看到一次，这是 HY-F3 之前接受的体验代价。

备选（否决）：
- **Hyphae 原生前缀 `hyphae.friend/v1` + Hyphae 状态机 + `contact request/accept` 子命令**。好处是 TUI 也能用；代价是 Hyphae 要做一套状态机、存储、JSON 契约（L），Agent24 还是要再包一层。好友是产品层的事，Hyphae 的定位是传输（HYPHAE-CLI-INTEGRATION「接线原则」1）。v2 若 TUI 也要好友再议。
- **用 Nostr kind 3 关注列表当好友**：公开、单向，和「申请 / 同意」的心智不符，而且会把社交图暴露在 relay 上。否决。
- **换 NIP-17 gift wrap 隐藏 `p` 标签**：能把「谁给谁发」也藏起来，但 Hyphae 整个消息层要重写，属于 T01 的范围，不在本文。

### 3.4 每种状态下收到普通消息怎么处理

| 对端状态 | 普通文本消息 | 通知 | 能否回复 |
|---|---|---|---|
| 好友 | 进主列表（按对端分会话） | 通知 | 能 |
| 申请中（我收到） | 挂在申请卡片下作为「附言」，同意后并入会话 | 和申请一起通知一次 | 同意后才能回 |
| 申请中（我发出），对端 `speaks_protocol=false`（裸 Hyphae 用户） | **按 Q3**：视为对方同意 → 变好友，`contact add`，并补发一条 `accept`（`id` 为我的 request id）让对方同步 | 通知 | 能 |
| 申请中（我发出），对端 `speaks_protocol=true`（是 Agent24，或曾发过任何协议消息，包括 `decline`） | **Q3 不适用**：状态不变，仍是申请中；消息进陌生人消息列表。Agent24 对端只有发 `accept` 才算同意 | 不通知 | 能回（回复不改变关系） |
| 陌生人 / 已删除 | 进**陌生人消息**列表：按对端折叠，显示名字（若曾是好友）或 npub 短码 + 预览；每个对端最多保留最近 20 条，列表最多 200 个对端，超出按时间淘汰 | **不通知**，只在入口显示数量 | 能回（回复不改变关系；界面提示「对方不是好友」并给「加为好友」） |
| 已屏蔽 | **静默丢弃**：不进任何列表、不计数、不通知 | 无 | 不显示，自然不能回 |

「只接收好友消息」开关（默认关）：开了以后陌生人消息也静默丢弃，但**好友申请仍然收**（否则永远加不了新朋友）；入口显示「已过滤 N 条」。

Q3 的边界（评审 H2）：Q3 只补「对端是裸 Hyphae 用户、发不出 `accept`」这一个缺口。一旦对端发过任何一条 `agent24.friend/*`，它就被认定为会说协议，之后只认 `accept`。已知的不对称：Agent24 对端**静默**拒绝了我（没发 `decline`，所以我这边 `speaks_protocol` 仍是 false），之后又主动给我发普通消息——我这边会按 Q3 变好友并补发 `accept`，对方的 Agent24 收到这条 `accept` 时 `id` 不匹配会忽略，对方那边我仍是陌生人。这是可接受的：对方主动发消息本身就表示愿意联系，而对方那边的关系不会被我单方面改变。

### 3.5 删除与屏蔽的语义

| | 删除好友 | 屏蔽 |
|---|---|---|
| 对方知道吗 | 不知道（所有参考产品一致） | 不知道 |
| 对方还能发吗 | 能发。Nostr 无法从源头阻止 | 能发，但我这边静默丢弃 |
| 我还看得到吗 | 看得到，在陌生人消息里 | 看不到 |
| 我还能回吗 | 能，回复不恢复好友 | 不能 |
| 对方还能再申请吗 | 能，正常走申请 | 申请也丢弃 |
| 已有会话 | 保留，可从陌生人消息里打开 | 隐藏；解除屏蔽后恢复可见 |
| 对 Hyphae 联系人的影响 | 调 `contact remove`（Hyphae 新增，§9 HY-F1）；在那之前只改 Agent24 状态，keystore 里的条目留着不显示 | 同左 |

回答 jason 的问题：**删好友不等于不想再看到他的消息**，那是屏蔽。删好友只是「他不再是我的人」，他的消息降级到陌生人列表，想回还能回。两件事分开，用户才有「先删掉看看、烦了再屏蔽」的余地，这也是微信以外所有产品的做法。

Nostr 的限制要写在界面里：屏蔽只在**这台设备的 Agent24** 生效；用 Hyphae TUI 或另一台设备打开同一个身份，照样能看到被屏蔽者的消息（除非 §3.6 的列表同步做了）。将来 Hyphae 自建 relay（khatru）可以加「按收件人的拒收名单」做服务端拦截，那是 v2 的 relay 需求，本文只登记。

### 3.6 多设备、重装、恢复备份后的好友关系

现状：Hyphae 联系人在 `keystore.json`，#1535 的备份只有私钥，恢复后联系人为空；两台设备各自一份 keystore，互不同步。

三个候选：

| 方案 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| A 只存本地 | `<state_dir>/comm/friends.json`（0600，原子写） | 最简单；不暴露任何元数据 | 重装 / 换机全丢 |
| B 本地 + 纳入备份 | A 的基础上，导出时把好友表和 Hyphae 联系人打成一个用同一备份口令加密的 `.agent24-contacts` 文件，与 `ncryptsec` 并列 | 重装可恢复；零网络依赖；复用 #1535 的口令窗口和写文件流程 | 不自动同步；两台设备要手动导入 |
| C NIP-51 加密列表 | 发布 kind 30000 follow set（`d=agent24-friends`），条目放在 NIP-44 加密到自己的 `content` 里；重装后用私钥从 relay 拉回 | 只要有私钥就能恢复，多设备自然同步 | relay 能看到「这个 npub 有一个列表、何时更新、大小多少」；依赖 relay 保留；Hyphae 没有 NIP-51，要新增发布 / 拉取（M）；冲突合并要定规则 |

**推荐 v1 做 B，C 列为 v2**（Q5）。理由：#1535 刚把备份流程做扎实，加一个文件是 S 级；C 要 Hyphae 新协议和冲突合并，而现在连第二台设备的场景都没验证过。

状态存储说明：COMM-HYPHAE §2 定了「comm 不另存消息，Hyphae 的文件是唯一来源」。好友表不是消息，是关系状态，Agent24 自己存不违反这条；但要在 COMM-HYPHAE §2 补一行「关系状态在 `comm/friends.json`」。ADR-041 落地后，已处理 `event_id` 集合并入 `inbound_events` 的去重，好友表仍独立（它是准入表的「名单」数据来源之一，见 §3.9）。

### 3.7 地址发现：第一次怎么拿到对方

这是 jason 初步思路里点出的真正难点。好友申请解决「拿到地址之后」的事，拿到地址本身有三条路，v1 做前两条：

| 方式 | 做法 | 规模 |
|---|---|---|
| **邀请链接** | `nostr:nprofile1…`（NIP-19 nprofile = npub + relay 提示，比裸 npub 多带 relay，对方不用猜 relay）；页面「分享我的地址」给出链接和纯 npub 两种。「加为好友」输入框接受：`npub1…`、`nprofile1…`、`nostr:` URI、十六进制 pubkey | S |
| **二维码** | 同一个 nprofile 编成二维码，桌面端显示；扫码端 v1 只有「从剪贴板 / 图片文件读二维码」（桌面没有摄像头流程），手机端留给遥控版 | S |
| **NIP-05 用户名** | `name@domain` → `GET https://domain/.well-known/nostr.json?name=name` → pubkey。两点要定：(1) 解析放哪：`agent24-comm` 的依赖 allowlist**禁止任何 HTTP 客户端 crate**（COMM-HYPHAE §7 M8，`tests/comm_dependency_allowlist.rs`），所以要么放 agent24d 其他模块，要么让 Hyphae 加 `contact resolve --nip05`；推荐后者，Hyphae 本来就联网。(2) 要不要 iDoris 给用户发 `name@idoris.ai`：这是产品问题（Q6） | S（Hyphae 做）/ M（Agent24 自己做） |

验证规则：输入解析出的 pubkey 必须是合法的 32 字节；nprofile 的 relay 提示只用于**可选地**追加到发送 relay 列表，并且要经过现有 `PUT /comm/relay` 的 ws/wss 校验，不能绕过。

### 3.8 防骚扰

Nostr 密钥免费、无限，所以限频对「换一个 key 再发」无效；防线是**陌生人永远不通知 + 列表有上限**，限频只是辅助。

- 发出申请：同一对端 24 小时内最多 1 条；收到过对方 `decline` 的（`peer_declined_at`），7 天内不能再发——**这条 7 天规则只在真的收到 `decline` 时生效**，对方静默不回只受 24 小时规则约束；全局每小时最多 20 条（防被攻破的 renderer 拿本机当发件机，呼应 #1535 残余风险 1）。
- 收到申请：我拒绝过的对端（`declined_at`），7 天内它再发的 `request` **静默丢弃**（不进待处理、不进陌生人消息、不通知），7 天后再发的照常显示；同一对端只保留一条（新的覆盖旧的）；待处理申请最多 100 条，超过的进陌生人消息列表尾部。
- 陌生人消息：§3.4 的 20 条 / 200 个对端上限；预览只取前 80 字，去控制字符。
- `request` 的 `note` 展示时当作不可信文本：去控制字符、不渲染链接、不自动预览。
- 申请和同意在桌面端都经**主进程原生确认框**（和 #1535 的加联系人一致），因为同意会写 keystore（`contact add`）。
- 日志只记对端 npub 短码和类型，不记 `note`。

### 3.9 隐私：已读回执，以及对方能看到什么

| 项 | 结论 |
|---|---|
| 已读回执 | **没有**。Hyphae 没有回执字段（COMM-HYPHAE §5.1 L3 预留，G4），本文不加。界面永远不显示「已读」「已送达」 |
| 在线状态 / 正在输入 | 没有，也不做 |
| 对方（申请方）能看到的 | 我的 npub（他本来就有）；同意后我的 `name`。其他什么都没有 |
| 对方能推断的 | 我回了 `accept` 的时间（事件的 `created_at`）；如果我不回，他**分不清**是拒绝、没看到还是屏蔽（这是有意的，和微信一样） |
| relay 和旁观者能看到的 | kind 30078 的 `p` 标签是**明文的收件人 pubkey**，所以 relay 知道「A 给 B 发了几条、何时发」，看不到内容和类型。这是 Hyphae 现有消息层的性质，不是本文引入的；要藏社交图得等 T01 换 NIP-17 gift wrap。要写进用户文档 |
| 名片 | 只走加密消息（§3.3），不用公开的 `profile publish` |

### 3.10 与 agent 自动执行白名单的边界

硬边界：**好友不等于可以指挥我的 agent。** 具体：

1. 好友表**不写入** `A24_NOSTR_ALLOWED_NPUBS`，也不被 nostr-bridge 读取。F4b 路径维持冻结，一切不变。
2. 好友协议消息（`agent24.friend/*`）由 comm 模块确定性处理，**永不进入 run、模型或模块**；COMM-HYPHAE §7 的 T2 zero-run 测试加一类样本「好友申请 / 同意」，断言 runs Δ=0、stub=0。
3. 等 [ADR-041](../adr/ADR-041-inbound-events-admission-and-work-items.md) 落地：准入表的「发起者是否在名单」里，**名单 ≠ 好友表**。每个好友有一个独立开关「允许让我的分身做事」，默认关；只有开了的好友才进名单。陌生人 → `ignore`；已屏蔽 → 入站事件直接 `filtered`，不建记录。这条映射在 ADR-041 的 D3 里是「默认」一行，本文给它一个数据来源，不改 ADR-041 的规则。
4. 回复陌生人消息是用户手点的发送，不是 agent 行为；agent 代发（将来的技能）仍走审批门。

## 4. 现有「联系人」怎么迁移

| 选项 | 结果 |
|---|---|
| **a. 已有联系人默认算好友**（推荐，Q4） | 它们是用户手填 npub 加的，而且已经在聊；标成陌生人会把正在进行的会话藏进折叠列表。状态记 `since = added_at`，来源记 `migrated_from_contact` |
| b. 算「单向联系人」，消息进主列表但带标记「未确认」 | 多一个状态，界面要多一种提示，而对方那边本来也只是本地记录。不值得 |
| c. 全部算陌生人，让用户逐个重新申请 | 对现在唯一的真实用户（jason 和同事）是倒退 |

迁移只做一次：首次启动好友功能时，把 `contact list` 的每一项写成好友记录；之后 Hyphae 联系人表退化为**内部的名字簿**（发消息时解析昵称、`history` 显示名用），界面不再直接展示它。Agent24 写 Hyphae 联系人时用 `<name>-<npub 前 6 位>` 作昵称避免撞名（Hyphae 以昵称为键）。

## 5. REST 与 CLI（草案，实现时按 CORE-2 登记 Operation）

| 方法 + 路径 | 请求 | 说明 |
|---|---|---|
| `GET /comm/friends?state=` | — | 列表，`state` 可选 `friend \| requested_out \| requested_in \| removed \| blocked` |
| `POST /comm/friends/request` | `{to, note?}`，`to` 接受 npub / nprofile / nostr: URI / nip05 | 发 `request`，写申请中（我发出）。对端已在申请中（我收到）时**等同 `accept`**（界面上的「加为好友」和「同意」是同一个动作）。桌面经主进程确认 |
| `POST /comm/friends/{npub}/accept` | — | 发 `accept`，`contact add`，写好友。桌面经主进程确认 |
| `POST /comm/friends/{npub}/decline` | `{notify?: bool}` | 默认不发消息；`notify=true` 发 `decline` |
| `POST /comm/friends/{npub}/withdraw` | — | 发 `withdraw` |
| `POST /comm/friends/{npub}/remove` | `{confirm:true}` | 已删除；有 HY-F1 时顺带 `contact remove` |
| `POST /comm/friends/{npub}/block` · `/unblock` | `{confirm:true}` | 已屏蔽 / 解除 |
| `PATCH /comm/friends/{npub}` | `{name?, allow_agent?}` | 改显示名；`allow_agent` 在 ADR-041 之前只存不用 |
| `GET /comm/history?bucket=friends\|strangers` | 现有路由加参数，默认 `friends` | 协议消息永远被过滤掉；`blocked` 不提供读取 |
| `POST /comm/address/resolve` | `{input}` | 解析为 `{npub, relays[], source}`；NIP-05 按 §3.7 的落点 |
| `GET/PUT /comm/friends/settings` | `{strangers: "list" \| "drop"}` | 「只接收好友消息」开关 |

CLI：`agent24 comm friend list|add|accept|decline|withdraw|remove|block|unblock`，`--json` 原样输出 envelope，风格沿用 `agent24 comm contact`。

错误闭集沿用 COMM-HYPHAE §4，新增 `rate_limited`（429）和 `blocked`（409，对已屏蔽者发申请）。

## 6. 入站扫描怎么跑

- 由 comm 模块在 daemon `running` 时每 30 s（和 Hyphae `--watch-interval` 同步）执行 `history inbox --limit 200`，**每次都拿最新 200 条**，逐条查 `friends.json` 里持久化的已处理 `event_id` 集合（有上限的 LRU，2 000 条）：没见过的交给状态机并记入集合；见过的跳过。处理顺序按 `created_at` 升序只是为了让「先 request 后 withdraw」这类成对消息按发送顺序落地，`created_at` **不做游标、不做过滤条件**。没有 daemon 时由 `POST /comm/inbox/pull` 后顺带扫一次。
- 同一次扫描里也要惰性判定超时：申请中的记录 `sent_at` / `received_at` 超过 30 天的转为陌生人（§3.2）；不另起定时器。
- `history inbox` 没有游标是实打实的缺口：一次拉 200 条足够日常，但两次扫描之间积压超过 200 条就可能漏掉协议消息。所以 §9 请 Hyphae 加 `--after-id`（群聊 history 已有同款）。在那之前，扫描发现「第 200 条（最旧的那条）也没见过」时记一条 `catch_up.incomplete` 风格的告警并提示用户刷新，不静默。
- ADR-041 D1 落地后，这个扫描就是「Hyphae 收件箱由 daemon 主动拉取写入 `inbound_events`」那条适配器，seen-set 并入 `inbound_events` 的 `id` 去重，好友协议消息的 `disposition` 记为 `protocol`，不进准入。

## 7. 测试判据

| 判据 | 正对照 |
|---|---|
| 两套临时 HOME + 本地 relay：A 申请 → B 同意 → 两边 `GET /comm/friends` 都是 `friend`，B 的 keystore 里多了 A 的联系人 | 不发 `accept` 时 A 仍是 `requested_out` |
| A 删除 B 后，B 发消息出现在 A 的 `bucket=strangers`，`bucket=friends` 里没有 | 删除前同一条消息在 `friends` 里 |
| A 屏蔽 B 后，B 的消息和申请在 A 的任何 bucket 里都不出现，`messages.db` 里仍然有（证明是客户端过滤） | 解除屏蔽后再发一条能出现 |
| 两边同时申请 → 两边都直接变好友，各收到一条 `accept`，两边 keystore 里各多一条联系人 | — |
| `accept` 的 `id` 不匹配 → 状态不变 | 匹配的 `id` → 变好友 |
| **Q3 适用**：A 申请裸 Hyphae 用户 B（B 从未发过协议消息），B 用 `hyphae agent msg` 回一句普通文本 → A 变 `friend`，A 的 keystore 多了 B，B 收到一条 `accept` 且其 `id` 等于 A 的 request id | B 不回 → A 仍是 `requested_out` |
| **Q3 不适用**：A 申请 Agent24 用户 B，B 发了 `decline`（带告知），之后 B 又发普通文本 → A 仍是 `requested_out`（带 `peer_declined_at`），那条文本在 A 的 `bucket=strangers` | 同场景 B 发 `accept` → A 变 `friend` |
| B 曾发过任何协议消息（例如一条被忽略的 `withdraw`）后再发普通文本 → A 仍是 `requested_out` | — |
| A 删除 B 后再对 B 发申请 → A 是 `requested_out`，记录保留原 `name` | — |
| A 删除 B 后收到 B 的 `request` → A 是 `requested_in` | — |
| A 在 `requested_in` 时点「加为好友」→ 等同「同意」：变 `friend`、`contact add`、发 `accept` | — |
| A 在 `requested_in` 或 `requested_out` 时屏蔽 B → 直接 `blocked`，不发任何消息，pending 记录消失 | 解除屏蔽后 B 是陌生人，需要重新申请 |
| A 拒绝 B（不告知）后 B 在 7 天内再发 `request` → A 这边什么都不出现；第 8 天再发 → 出现在待处理 | — |
| A 收到 B 的 `decline` 后 7 天内对 B 发申请 → `rate_limited`；没收到 `decline` 只受 24 小时规则限制 | — |
| 30 天超时：把 `sent_at` 改成 31 天前，跑一次扫描 → `requested_out` 变陌生人；29 天 → 不变 | — |
| 畸形 JSON（重复键、超 4 KiB、控制字符）→ 当普通文本进陌生人消息，状态不变 | 合规 JSON → 状态变化 |
| 重放同一 `event_id` 的 `request` 十次 → 只有一条申请；重启 daemon 后再重放 → 仍只有一条（seen-set 已持久化） | 不同 `event_id`、相同内容的 `request` → 覆盖为新申请（同一 npub 只留一条） |
| seen-set 塞满 2 000 条后再来 1 条 → 最旧的被淘汰，新的被记住 | — |
| zero-run：发 20 条申请 / 同意 / 陌生人消息，runs Δ=0、模型 stub=0、`A24_NOSTR_ALLOWED_NPUBS` 文件和环境不变 | F4b 开 flag 的正对照 runs Δ=1（沿用 COMM-5b T3） |
| 备份：导出含好友表的联系人包 → 全新 HOME 恢复 → 好友列表一致 | 不导入时为空 |

## 8. 需要 jason 拍板的问题

> **已拍板（2026-11-02）**：jason 选择「全部按推荐」。Q1 协议归 Agent24；Q2 默认不告知拒绝；Q3 对方回消息即视为同意；Q4 已有联系人默认算好友；Q5 v1 存本地并纳入加密备份，NIP-51 留到 v2；Q6 v1 只做链接和二维码，NIP-05 / `@idoris.ai` 单独立项。

| # | 问题 | 推荐 | 理由 |
|---|---|---|---|
| Q1 | 协议归谁：Agent24 自己的 `agent24.friend/v1` 明文前缀（Hyphae 当普通消息落库，Agent24 解析），还是让 Hyphae 做原生 `hyphae.friend` 状态机和子命令 | **Agent24 自己做**，Hyphae 只做 §9 的三处小配合 | 好友是产品层概念，Hyphae 定位是传输；Hyphae 原生要 L 级改动，还得等它发版锁 hash |
| Q2 | 拒绝申请要不要告诉对方 | **默认不告知**，提供「拒绝并告知」为可选 | 微信、Telegram、LINE、Signal 都不告知；不告知让申请方分不清拒绝 / 没看 / 屏蔽，是保护被申请方 |
| Q3 | 我发出申请后，对方没回 `accept` 但直接回了一句话，算不算同意 | **算，但只对从未发过任何 `agent24.friend/*` 消息的对端（裸 Hyphae 用户）**；自动变好友并补发 `accept`。Agent24 对端只认 `accept` | 对方可能是裸 Hyphae TUI 用户，发不出 `accept`；jason 的原话「申请其实就是第一条消息，对方回了就是同意」。收窄是为了不让「静默拒绝后随口说一句」变成好友（评审 H2） |
| Q4 | 已有联系人怎么算 | **默认算好友**，记来源 `migrated_from_contact` | 它们是手填加的、正在聊的；标陌生人是倒退 |
| Q5 | 好友关系存哪 | **v1 本地 + 纳入备份**（和 `ncryptsec` 并列一个加密的联系人包）；NIP-51 加密列表列 v2 | 复用 #1535 的备份流程是 S 级；NIP-51 要 Hyphae 新协议和多设备冲突规则，现在没有多设备场景 |
| Q6 | NIP-05 用户名要不要做，以及 iDoris 要不要给用户发 `name@idoris.ai` | **v1 不做解析，只做链接和二维码**；`@idoris.ai` 作为 iDoris 的产品项单独立项 | NIP-05 解析在 Agent24 里受 comm crate 禁 HTTP 的约束，放 Hyphae 更顺；但先验证好友流程本身，再谈域名身份 |

## 9. 需要 Hyphae 配合的事

都不是阻塞项，v1 没有它们也能跑；按价值排序。

| 编号 | 内容 | 规模 | 为什么 |
|---|---|---|---|
| HY-F1 | `contact remove --nickname N`（可顺带 `contact update`） | S | 删好友 / 屏蔽后 keystore 里的条目没法清；现在只能留着不显示 |
| HY-F2 | `history inbox --after-id <id>`（和 `groupchat history` 同款游标） | S | 现在每 30 s 全量拉 200 条靠 seen-set 去重；有游标后只拉增量，而且两次扫描之间积压超过 200 条也不会漏 |
| HY-F3 | wire contract 写明 `agent24.` 明文前缀归 Agent24，Hyphae 不解析、不占用；TUI 显示这类行时折叠成「[Agent24 好友申请] <note>」 | S（文档 + TUI 一处） | 裸 TUI 用户不至于看到一坨 JSON；两仓命名空间不撞 |
| HY-F4（v2） | `contact resolve --nip05 name@domain` | S | Agent24 comm crate 禁 HTTP 客户端 |
| HY-F5（v2） | NIP-51 follow set 的发布 / 拉取（NIP-44 加密 content） | M | 多设备同步好友表 |
| HY-F6（v2） | 自建 relay 的按收件人拒收名单 | M | 把屏蔽从客户端推到 relay，省流量、跨设备生效 |

## 10. 工作量与路线图

**结论：工作量大**——Agent24 8 片（CF-1、2、3 纯 daemon；CF-4、6 daemon + 桌面；CF-5a、5b 桌面；CF-7 测试）+ Hyphae 3 片（v1），超过 4 片，且涉及双仓契约。按 COMM-HYPHAE §8 的习惯，每片约 300 行一个 PR；S ≈ 1–2 天，M ≈ 3–5 天，L ≈ 一周以上。开发派 Sonnet / Codex，Opus 只做验收。

| 编号 | 仓 | 交付 | 规模 | 依赖 | 可证伪验收 |
|---|---|---|---|---|---|
| CF-1 | Agent24 daemon | 好友表（`comm/friends.json`，原子写，含 seen-set 与 `speaks_protocol`）、状态机（§3.2 全部转移，作为纯函数）、**不涉及发送的**路由与 CLI（`GET /comm/friends`、`remove`、`block` / `unblock`、`PATCH`、`settings`）、变好友时的 `contact add`、迁移（§4）、限频计数 | M | 无 | 状态机表驱动用例覆盖 §3.2 每一条转移和 §3.8 的两条 7 天规则；并发 10 次「变好友」只写一条联系人；迁移后 `contact list` 每项对应一条 `friend`；本片**不发任何 Nostr 消息**（假二进制断言 `agent msg` 调用数为 0） |
| CF-2 | Agent24 daemon | 协议编解码（§3.3）、`--content-file` 发送、入站扫描与 seen-set 去重（§6）、`GET /comm/history` 过滤协议行；**需要发送的路由**（`request`、`accept`、`decline`、`withdraw`）随本片落地 | M | CF-1；HY-F3 文档（非阻塞） | §7 第 1、4–11、13–18 行；畸形样本 20 个全部当普通文本 |
| CF-3 | Agent24 daemon | `bucket=` 分流、陌生人上限与淘汰、屏蔽静默丢弃、「只接收好友消息」开关 | S | CF-2 | §7 第 2、3、12 行；200 个对端 + 1 时最旧的被淘汰 |
| CF-4 | Agent24 daemon + 桌面 | 地址解析（npub / nprofile / nostr: / hex）、分享链接、二维码生成与从图片读取 | S | 无 | 四种输入解析到同一 pubkey；nprofile 的 relay 提示经 ws/wss 校验；坏二维码报 `invalid` |
| CF-5a | 桌面 | 好友列表、「加为好友」流程（主进程确认）、收到的申请卡片（同意 / 拒绝）、发出的申请（撤回） | M | CF-1、CF-2、CF-4 | 页面测试：五种状态各有对应视图；没有「已读」「已送达」文案 |
| CF-5b | 桌面 | 陌生人消息入口与折叠列表、屏蔽列表与解除、设置开关、联系人表从界面移除 | S | CF-3、CF-5a | 陌生人消息不触发系统通知；屏蔽者在任何视图不出现 |
| CF-6 | Agent24 daemon + 桌面 | 备份：导出 / 恢复加密联系人包，复用 #1535 的口令窗口和写文件流程 | S | CF-1 | §7 最后一行；包里没有私钥 |
| CF-7 | Agent24 | 测试：zero-run 样本扩展、两 HOME + 本地 relay 联调、文档（COMM-HYPHAE §2 补一行、用户文档写明 `p` 标签可见与屏蔽仅本机） | M | CF-2、CF-3 | §7 zero-run 行，正对照 runs Δ=1 |
| HY-F1 / F2 / F3 | Hyphae | 见 §9 | S × 3 | 无 | 各自 `--json` 契约测试；lock 升级走 DEP-C8 流程 |

依赖链：

```
CF-1 → CF-2 → CF-3 → CF-5b
CF-1 → CF-2 → CF-5a ← CF-4
CF-3 + CF-5a → CF-5b
CF-1 → CF-6
CF-2 + CF-3 → CF-7
HY-F3（文档）→ CF-2 对齐命名空间；HY-F1 / HY-F2 到位后各自在 CF-1 / CF-2 里补接线（不阻塞）
```

排序建议（§8 已拍板）：CF-1、CF-4 并行（互不依赖）→ CF-2 → CF-3 → CF-5a → CF-5b → CF-7 → CF-6。Hyphae 三片同步提，赶上下一次 lock 升级就接，赶不上也不等。整条线排在 ADR-038 C1–C3 之后。

与其他线的关系：不依赖 ADR-041 落地（它还是提议，且新表要等 CORE-5 5f–5g）；ADR-041 落地时按 §3.10 第 3 条和 §6 最后一条并入，不返工状态机。不依赖 T01 / NIP-17。

## 11. 开放问题（不需要现在拍板）

- 群聊里的好友语义：Hyphae 群聊邀请现在是「创建者拉人」，非好友能否拉我进群、群成员消息按好友还是按群判——留给群聊 UI 立项时定。
- 手机遥控版的扫码加好友：等移动端线。
- 好友表要不要进记忆 P1 的 purge 范围：它是配置不是记忆，建议不进。
- `allow_agent` 开关的审批级别（CORE-2 Elevated？）：等 ADR-041 时和「谁能让我做事」三档一起定。
