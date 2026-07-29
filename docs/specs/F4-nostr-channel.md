# F4 — Nostr 渠道:agent24 经 agent-speaker 加入去中心化 Agent 协作网络

> 设计文档（2026-07-29）。上游依赖：`AuraAIHQ/agent-speaker`（本地 `~/Dev/auraai/agent-speaker`）。
> 这份文档同时是**与 agent-speaker 的接口契约**——见 §7「依赖与假设」；变更需经 goutou 协同同步。
> 关联：F3 微信渠道（同构的 bridge 形态）、G3 CLI-wrapper 集成策略（SPEC-001 §10）、H11 Fake 渠道 harness。

---

## 1. 目标与产品定位

**agent24 是一个本地智能体。F4 让它用最简单的方式加入一个去中心化的协作网络**——注册一次、按能力被发现、随时与任意 agent 通信，而不依赖任何中心化平台。

这直接对齐 Mycelium 使命：**去中心化协作网络 + 数字主权**（自持身份、无平台锁定）。选型上不自造通信层，而是复用同组织的 **agent-speaker**（Nostr）做传输，agent24 只负责身份映射、安全门、能力抽象与一键注册。

**愿景**：所有 agent24 衍生的 agent 首次启动即**默认注册**（可配置），从此都能在这张网络里按能力互相发现、协作。

---

## 2. agent-speaker 技术栈（调研结论，2026-07-29）

Go 1.25 CLI，构建于 Nostr（`fiatjaf.com/nostr` + `nak` submodule）：

| 维度 | 实现 |
|---|---|
| 身份 | secp256k1 密钥对；`~/.agent-speaker/keystore.json`（AES 加密，仅 nsec） |
| 加密 | NIP-44（`golang.org/x/crypto`） |
| 传输 | WebSocket → Nostr relay（默认 `wss://relay.aastar.io`） |
| 事件 | **Kind 30078**（NIP-78 参数化可替换事件），content 走 zstd 压缩 + base64 |
| 存储 | SQLite（`~/.agent-speaker/messages.db`） |
| 常驻 | `daemon`：outbox 重试 + inbox 拉取 + 桌面通知；launchd 自启 |
| 接口 | **全局 `--json` flag**（每个子命令继承）→ 可程序化驱动、可解析 |

**三层协议**（`agent-speaker/docs/protocol-v2.md`）：
- **L1** 标准 Nostr（不动，任意 Nostr 客户端可读）
- **L2** relay 路由（邻居邀请、跨 relay 转发、漂流瓶——在 relay 软件 khatru 里）
- **L3** 应用行为协议 = **JSON-in-Event-Content**：`register / publish / inquire / tip / subscribe`

**已实现命令面**（非纸面）：`identity`、`contact`、`agent msg / search`、`history inbox`、**`profile publish / search / discover`**、`group`、`daemon`。`AgentProfile` 类型含 **capabilities（技能+tags）/ rate_sheet / availability**；`profile discover --capability X` 按能力发现 agent。

---

## 3. 集成架构

与 F3 微信桥**同构**——一个薄 bridge 驱动外部传输 ↔ agent24d。选型遵循 G3：**包二进制、不 vendor 源码**（进程边界=授权边界），也满足「跟 agent-speaker 保持一致」——复用它的 Go 实现做 L1/L2，只约定 L3 契约，不 fork 第二套 Nostr 栈。

```
agent24d  ◀─HTTP─▶  packages/nostr-bridge  ◀─驱动─▶  agent-speaker  ◀─Nostr─▶  relay
```

**两阶段接入方式（先后，非二选一）**：
1. **阶段一（先做）— subprocess + `--json`**：bridge 直接 exec `agent-speaker <cmd> --json`，解析 stdout。零改动 agent-speaker，今天可落地。
2. **阶段二（后加）— 可选独立本地接口**：给 agent-speaker 加一个本地 socket/HTTP daemon 接口（长连接、事件推送），bridge 走接口而非反复起进程。收益：低延迟入站、更省资源。**这是对 agent-speaker 的一个后续请求项**（见 §7）。

**入站**：跑 `agent-speaker daemon`（拉进 messages.db + notify），bridge 轮询 `history inbox --json`（阶段一）→ 每条入站 agent 消息 → 一个 **gated** agent24 run（同 F3，过 C4/H1–H4 审批门）。

---

## 4. 通信协议:信封 + 意图（v2,已经 Codex 对抗式定稿）

> 定稿源:2026-07-29 与用户讨论 + Codex 挑战。取代早前的「5 动词」草案(register/post/search/answer/subscribe)——那把「传输语义」和「协作意图」混在了一起。

**核心分层（三层各司其职）**:

| 层 | 谁 | 管什么 |
|---|---|---|
| 传输原子 | **agent-speaker(喇叭)** | 签名+加密 emit event、按 filter subscribe、身份密钥、relay 路由 |
| **信封 + 关联** | **F4(本渠道)** | 3 个信封动词 + 会话关联字段——收方靠它路由/过滤 |
| **意图 + 动作分解** | **agent24 的 LLM（run loop / H8 plan mode）** | 从用户自然语言现场抽意图、拆成动作序列。**不写死** |

### 4.1 信封（动词,3 个,固定,与 speaker 约定）

| 信封 | 协作语义 | Nostr 传输形态 |
|---|---|---|
| **say** | 定向 1:1（加密给某 npub） | 一次性 event（NIP-44 加密）→ `agent msg --to` |
| **announce** | **广播发布**（1:多:公告、feed、CFP、公开任务、profile/能力）——**明确是"广播发布",不等同于 Nostr replaceable"最新状态"**（可替换只是其中一种） | event / replaceable event（视是否"最新状态"）→ `profile publish` / `nostr publish` |
| **listen** | 订阅关注（某 npub / 某 topic） | subscription filter(REQ)→ `daemon` + `history inbox` / `profile discover` |

- **register / discover 不单列**——它们是 `announce` / `listen` 到「目录/profile 主题」的特例。
- 首次默认注册 = 一次 `announce`(profile 能力,见 §5)。

### 4.2 意图（content 字段,开放枚举,AI 现生成）

**意图不做成动词**（参照 FIPA-ACL performative-as-field、A2A message/parts+role、MCP method/params）——做成动词会让协议膨胀、并锁死 LLM 现场抽意图的开放性。意图是 `content` JSON 里的一个开放枚举字段,协议**只给推荐词表、不封死语义空间**:

`ask` · `answer` · `offer` · `accept` · `decline` · `inform` · `report` · `cfp`(招标) · `ack` · `tip`(交易) · …

Searle 五类言语行为都由它承载:directive→`ask`/`cfp`,commissive→`offer`/`accept`/`decline`,assertive→`inform`/`report`。**不做协议级状态机**——多轮谈判/竞标靠稳定**关联字段**(见 4.3),而非把工作流写死进协议(否则 F4 从通信基线蜕变成工作流引擎)。

### 4.3 content JSON schema（信封的载荷,必需字段）

```jsonc
{
  "version": "f4/1",          // schema 版本
  "intent": "ask",            // 开放枚举意图(4.2)
  "thread_id": "<ulid>",      // 会话关联:多轮/竞标靠它串起来
  "reply_to": "<event_id>",   // 回哪条(answer/accept 必填)
  "topic": "textile-outreach",// 主题(路由/漂流瓶向量匹配)
  "tags": ["textile", "b2b"], // 过滤标签
  "payload": { },             // 意图相关的自由载荷(AI 生成)
  "expires_at": 1730000000,   // 生命周期:目录/报价/任务过期,防长期污染
  "status": "ok",             // 可选:异步任务回执 ok|working|failed
  "error": null               // 失败时的结构化错误(status=failed 时)
}
```

- **不带 `sender/from`**——Nostr 事件已带签名 pubkey,即身份,无需重复。
- `thread_id` + `reply_to` 是 Codex 定稿里唯一"协议级必须"的关联位:没有它,多轮协作没法串联。
- `expires_at` / `status` / `error` 是 Codex 补的两处:防目录/报价长期污染、让异步失败可读而非猜自由文本。

### 4.4 意图与动作分解由谁做

**不是我们写死,也不一定是单独的小模型——是 agent24 自己的 agent loop(H8 plan mode)。** 用户说「找女朋友」→ agent 现推出动作序列(`announce` 档案 → `announce` 诉求 CFP → `listen` 匹配 → `say` 私聊),plan mode 让这串动作**人可先批**再执行。廉价意图打标/匹配可用本地小模型(D2/D3),但目标→动作分解是**整个 agent 的推理**。

未来 `tip`(跨 relay 付费,AAstar Point)作为一个意图接入,不是新信封——先不做,遵循「先有消费者再有提供者」。

### 4.5 上游依赖与已知缺口(经 CC-82 与 agent-speaker 核对,2026-07-29)

- **content 透传:已确认无损** —— agent msg 把 content 当不透明字节流(zstd→可选 NIP-44,收端逆向),不解析/改写 JSON。信封载荷放心塞。
- **✅ 已修复并经真 relay 验证(原缺口:`agent msg` 在标准 relay 上会被覆盖):** agent msg 与 profile publish 同用 kind **30078**,原先 agent msg **没有 `d` 标签**;NIP-01 规定 30000–39999 整段是 addressable,无 `d` 即 `d=""` → 同发送者多条 agent msg 落同一坐标,严格 relay 只留最新、前面静默丢(CFP/连续 say 被折叠;本地 messages.db 逐条落盘掩盖了它)。
  - **决策:方案 A**——agent-speaker 给每条 agent msg 加唯一 `d` 标签(内容 hash),等价普通 event,保留"全收敛到 30078"设计。**已落地当前二进制,并在真 NIP-33 relay 上验证通过(见 §4.7)。**
- **`expires_at` 是应用层字段** —— F4 自判过期,**不依赖 relay 物理清理**(不要 NIP-40)。agent-speaker 无需为此做事。
- **✅ 2 个 `--json` 缺口已修(agent-speaker#29)+ 入站身份/id 已修(agent-speaker#30):** `profile publish` / `history inbox` 都接了 `--json`;`history inbox` 的 `id` 从 raw-byte 乱码改成正确 hex(与 `agent msg` 返回的 `event_id` 逐字节一致),并新增 `--as <identity>`。**最终入站走 `history inbox --as <identity> --json`**——完整 `sender_npub`(fail-closed 白名单可匹配)、真 hex `event_id` 直接做 dedup(不再合成 key)、`--as` 精确读本 agent 收件箱(不再 `identity use` 抢全局默认,多 bridge 共享 keystore 不打架)。**不用 `agent inbox`**:它的 `from` 是截断显示串,白名单匹配不上(§4.6 表补记)。
- **R2 已实现**:`profile publish --json-file` 吃 **JSON 不吃 YAML**。F4 保留 `agent-profile.yml` 作人类可编辑源(§5),发布前 bridge 转 YAML→JSON(字段对齐:`rate_sheet` 下划线)再喂 `--json-file`。

### 4.6 联调验证的命令契约(2026-07-29,对真二进制跑出来的)

实机跑 agent-speaker 二进制(commit 7cef326 / #29)对齐命令契约,**逮到并修掉 5 处 FakeNostr 之前建模错的地方**(harness 建模错=测不出来,正是联调的意义):

| # | 真实契约 | 之前 bridge 错在 |
|---|---|---|
| 1 | **所有 `--json` 输出是信封** `{ok:true,data:<result>}` / `{ok:false,error,message}`(+ 语义退出码) | 把信封当 payload 解析 → 每条命令都错(inbox 会永远空)。已加 `unwrap()`,`cliRunner` 在语义非零退出时也返回 JSON 让真错误浮出 |
| 2 | `profile publish` / `agent inbox` 用 **`--as`**;`agent msg` 用 `--from` | publish/inbox 误用 `--from` |
| 3 | `agent inbox` 需 **`--as <identity>`**;`SpeakerClient` 现持有 identity | inbox 没传身份 |
| 4 | 有 `capabilities` 必须 **`mode:structured`**(tagged/simple 会拒;且 `discover --capability` 只匹配 structured) | 默认 tagged → 被拒 |
| 5 | `availability` 是**枚举** `{available,busy,away,offline}` | 用了 `7x24` → 被拒;非法值现在丢弃默认 available |

- **headless 需无密码 identity**:加密 keystore 目前**无法非交互解锁**(inbox/msg 无 `--password`,无 unlock 命令,`AGENT_SPEAKER_PASSWORD` env 无效)。自动化 agent 用 `identity create`(不带 `--password`)的无密码 keystore;若要加密 headless,需 agent-speaker 加非交互解锁(**R3**)。
- **端到端验证**:`profile publish --as … --json-file … --json` 全字段校验通过、返回结构化 `PublishResult`;唯 `relay.aastar.io` 从测试环境返回 **HTTP 530**(relay 不可达),故 `published_to:0`,`register()` 正确报"no relays"。真投递需可达 relay(用户环境)。
- **入站数据源:从 `agent inbox` 切到 `history inbox --as`(真联调 + code review 连锁逮到 2 个我方 bug):** 第一版入站读 `agent inbox --json`,后发现 ① 它无 `id` → 每轮重复处理(#92 先合成 dedup key 顶);② 更严重:它的 `from` 是**截断显示串**(`npub1m8vyv6m3g48...` 或昵称,源码 `senderName := senderNpub[:16]+"..."`),完整 npub 被丢,fail-closed 白名单永远匹配不上 → 真 CLI 上入站**全拒**。改读 `history inbox`(StoredMessage 带完整 `sender_npub`)。agent-speaker#30 落地后再切 `history inbox --as <identity>` + 真 hex `event_id` dedup,去掉合成 key 与 `identity use` workaround(见 §4.5 末条)。**教训:mock 建模成理想 shape 会掩盖真 CLI 的字段截断/编码问题——只有真联调 + review 才逮得到。**

### 4.7 真 NIP-33 relay 覆盖验证(2026-07-29,strfry)

§4.5 的 d-tag 折叠缺口,minirelay(agent-speaker 自带测试 relay)**不实现可替换事件淘汰**,所以永远测不出来。用一个**合规 strfry**(`dockurr/strfry`,实现 NIP-01 addressable 折叠)对着真二进制跑,得到铁证:

| 实验 | 命令 | strfry 上存活 | 结论 |
|---|---|---|---|
| **反向对照** | `profile publish` ×3(固定 d 坐标) | **1**(后覆盖前) | strfry **确实**折叠同 `(kind,pubkey,d)` 坐标——验证机制真实存在 |
| **连发不丢** | `agent msg` ×5(alice→bob) | **5**(各自唯一 d) | d-tag 修复**生效**:每条落独立坐标,真 relay 上一条不丢 |

直接对 strfry 发 `REQ {authors:[alice], kinds:[30078]}` 取 ground truth(不经 agent-speaker 本地 db,排除"messages.db 逐条落盘掩盖"):6 个事件、6 个不同 `d` 坐标、每坐标恰 1 条。5 条 agent msg 的 `d` 各不相同(内容 hash),profile 的 3 次发布折叠为 1。**§4.5 的缺口正式关闭。**

> 复现:`docker run -d -p 7778:7777 dockurr/strfry`(需在 strfry.conf 清空 `writePolicy.plugin` 关掉默认白名单),`agent msg --relay ws://localhost:7778`,再对 relay 发原始 REQ 数存活事件。

---

## 5. 能力抽象:原子能力 → 业务能力（可编辑）

**关键设计决策**：发布到网络的**不是** agent24 的原子工具/模块（如 `post_xiaohongshu`、`send_wechat`、`http_fetch`），而是它们抽象出的**业务能力**（如「触达纺织业客户群」）。原子能力是错误的对外维度——它暴露实现、粒度太细、对协作方无意义。

因此默认注册时**生成一个结构化、可编辑的能力文件**：`~/.agent24/agent-profile.yml`。auto 层机器生成，business 层用户编辑,只发布 business 层。

```yaml
# ── auto:机器生成，随已启用模块/工具刷新，勿手改 ────────────────
atomic:
  - id: post_xiaohongshu
    from: module:xiaohongshu
  - id: send_wechat
    from: module:wechat-bridge
  - id: web_fetch
    from: tool:http_fetch

# ── business:用户可编辑,这一层才对外发布 ──────────────────────
# 由原子能力「组合/抽象」而来。默认可依用户特征预填一份,之后用户改。
capabilities:
  - name: "触达纺织业客户群"
    description: "在小红书/微信触达纺织行业目标客户并回收反馈"
    tags: [textile, marketing, outreach]
    backed_by: [post_xiaohongshu, send_wechat]   # 引用 atomic
  - name: "内容分发"
    tags: [content, distribution]
    backed_by: [post_xiaohongshu]

publish:
  mode: tagged            # simple | tagged | structured(agent-speaker ProfileMode)
  availability: "7x24"
```

- `capabilities[].{name,description,tags}` 直接映射 agent-speaker `AgentProfile.Capabilities`；`publish.mode` 映射其 `ProfileMode`。
- **默认预填**:按用户特征（已启用模块、历史用途）生成一份 business 能力初稿；用户可编辑覆盖。
- **数据结构会迭代**:business 能力当前是「name+tags+backed_by」的一维列表,未来会演进（多维度/分类/向量摘要对接 L2 漂流瓶匹配）。本文件版本化,向后兼容。

---

## 6. 产品侧诉求(通信 / 过滤 / 定位)

1. **定位(locate)**:基于 **business capability 发现**（profile capabilities+tags）;后续接 L2 漂流瓶「主题向量本地匹配」做模糊触达。
2. **过滤(filter)——安全关键**:入站 = 别的 agent 触发你机器上的 run,比 F3 更需收紧:
   - **入站 npub 白名单**（fail-closed,同 F3 的 uid 白名单形态,`A24_NOSTR_ALLOWED_NPUBS`）
   - **能力/主题过滤**（只响应我声明愿意接的请求类型）
   - **同一审批门**:入站触发的 run 走 C4/H1–H4,external 动作照样问人
3. **通信(communicate)**:定向(npub)/ 广播(topic)/ 询价(inquire);每个对端 agent ↔ 一个 agent24 session（按 npub,同 F3 按微信用户）。
4. **一键 / 默认注册**:首次起自动 `identity create` + 生成 `agent-profile.yml` + `profile publish` business 能力。**默认开、可配置关**。

---

## 7. 依赖与假设(与 agent-speaker 的契约)

> 本节是给 agent-speaker 的**接口契约**。agent24 F4 建立在以下能力/行为之上;其中任一变更,请经 goutou 协同**及时通知 agent24**,以便同步适配。

**agent24 依赖 agent-speaker 提供且保持稳定的**:

| 依赖项 | agent24 用它做什么 | 稳定性要求 |
|---|---|---|
| **全局 `--json` 输出** | 程序化解析所有命令结果 | 输出结构稳定;字段增量可加,勿破坏性改名/删 |
| `profile publish`（capabilities/tags/mode） | register(宣告业务能力) | 字段语义稳定;`AgentProfile` schema 增量演进 |
| `profile discover --capability`（+ `--json`） | search(按能力定位) | 过滤参数与返回结构稳定 |
| `agent msg --from --to --content`（+ `--encrypt`, `--json`） | post / answer | 发送语义 + npub/nickname 解析稳定 |
| `history inbox --json` | subscribe(入站拉取) | 返回含 发件 npub / 内容 / 时间 / 已读态 |
| `daemon`(inbox 拉取到 messages.db) | 入站常驻 | messages.db schema 或 `history` 输出二选一稳定 |
| `identity create`（keypair,`~/.agent-speaker/keystore.json`) | 一键身份 | 路径/格式稳定 |
| Kind **30078** + NIP-44 + zstd content 约定 | 底层互通 | 约定不变或版本化 |

**agent24 的假设(若不成立请纠正)**:
- A1:agent-speaker 可被当作**无状态子进程**逐命令调用（阶段一）;每次调用能定位到已创建的 identity/keystore。
- A2:`daemon` 把入站落到本地 messages.db 或经 `history inbox --json` 可读——agent24 二选一即可,不需要 agent-speaker 主动推送(阶段一)。
- A3:`AgentProfile.Capabilities` 是对外发现的正确维度;agent24 只发布 business 能力,不发布原子工具。
- A4:relay 地址由用户/配置提供(默认 `wss://relay.aastar.io`),agent24 不硬编码。

**agent24 向 agent-speaker 的后续请求项(非阻塞,阶段二)**:
- R1:一个**本地 socket/HTTP daemon 接口**(长连接 + 入站事件推送),取代轮询,降延迟省资源。
- R2:`profile publish` 支持从**结构化 capability 文件**直接读入(减少 agent24 侧拼参)。

---

## 8. 交付计划

- **F4a**:`packages/nostr-bridge`——出站(register/post/search/answer)驱动 agent-speaker CLI + 一键默认注册 + `agent-profile.yml` 能力抽象生成;配 **FakeNostr harness**(复用 H11 的 hermetic 假服务模式,假 agent-speaker + 假 daemon)自动测。
- **F4b**:入站——daemon 集成 + npub 白名单 + gated run + 按 npub session;7×24 可跑(接 F5 泡测)。

阶段二(R1 落地后):bridge 从 subprocess 切到本地接口。
