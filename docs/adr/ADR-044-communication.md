# ADR-044：通信（Hyphae / Nostr）

索引：[架构总入口](../architecture/README.md)・[ADR 索引](README.md)

- 状态：**已接受**（jason 2026-11-05 拍板接受；正文 2026-11-05 经 #1593 评审合入）。§6 待办表里的订正项照常推进，不影响接受
- 编号：ADR-044，ADR 索引是唯一发号处（[架构优化方案](../tracking/2026-11-04/ARCH-OPTIMIZATION-PLAN.md) 的 S0-3 / A5）。`docs/research/deepseek-superagent/SUPERAGENT-EVOLUTION-2026-11-01-codex-astra.md:109,145` 里另有一份叫「ADR-044」的 Codex 稿，按 S0-3 改记为 `DRAFT-C04x`，与本篇无关。
- 登记：S0-3（#1596）已合入，索引里的 ADR-044 一行由 S0-7 PR 补上。
- 上位：ADR-037 决策 1、6（`docs/decision.md:1605-1612`）；[ADR-038](ADR-038-component-contract-v1.md)；[ADR-039](ADR-039-concept-model.md)；[ADR-041](ADR-041-inbound-events-admission-and-work-items.md)
- 吸收：[COMM-HYPHAE](../design/COMM-HYPHAE.md) §0 的已定决策；[F4 Nostr 通道](../specs/F4-nostr-channel.md) 的已定决策；[COMM-FRIENDS](../design/COMM-FRIENDS.md) 的已拍板决策。合入后这三份改称「实施说明」，决策以本篇为准，冲突时以本篇为准。
- 依据：R3 §3.A（分析报告 `docs/architecture/review-2026-11-03/R3-ADR-AND-DOCS.md`，在分支 `docs/arch-review-r3-adr-docs`，尚未合入 main）。本篇引用的 C01–C31、X1–X19 是 R3 的编号。R3 的行号取自它的基线提交 `9865d7a0`；我对 main（`f818b0bf`）抽查了标 **（main 已核）** 的几处，其余行号以 R3 为准，评审时请以 main 为准。
- jason 拍板（2026-11-04）：X1、X7、X11、Q6，见各决策。

## 1. 背景与范围

通信的已定决策散在 COMM-HYPHAE、F4、COMM-FRIENDS、HYPHAE-NETWORK-DELIVERY 和若干 PR 里，没有一份 ADR。后果是：文档之间有三处实质矛盾（X1 随包还是按需、X7 两套 Nostr 身份、X11 入站执行入口），ADR-041 和后续的好友机制也没有上位依据（方案 A6）。本篇把 31 条已做决策收到一处，对三处矛盾给出结论，其余偏差列为订正待办。

**范围**：agent24d 内的 comm 模块、Hyphae 子进程的分发与监管、Nostr 身份与数据归属、凭据与授权等级、入站消息的执行边界、消息状态语义、relay、应用层协议。

**不在范围**：遥控命令的格式（另写 `docs/design/REMOTE-CONTROL-HYPHAE.md`，目前不存在，C29）；T01 消息层协议（对端回执、NIP-17）；iDoris；Sin90；好友机制的具体实现（COMM-FRIENDS，另行设计，见 §11）。

按 ADR-039，通信是一项能力，提供者是组件 Hyphae（能力 id `nostr-dm`、`nostr-group`、`nostr-identity`、`nostr-relay`，见 `docs/capabilities/REGISTRY.md`）。

**两仓分工（C31）**：Hyphae 拥有身份、发现、加密通信、history/outbox、协议和上游发布；Agent24 拥有打包、兼容晋升、凭据保管、安装 API、UI 与进程监管（`docs/Deployment/HYPHAE-NETWORK-DELIVERY.md:9`）。

## 2. 决策

### D1 进程与服务归属（C01、C02、C14）

- 通信服务放在 agent24d 内，模块 `comm`，路由 `/api/v1/comm/*`。`agent24 comm` 和桌面 UI 都只是这套 REST 的客户端（`COMM-HYPHAE.md:12`；落地 `rust/crates/agent24-comm/src/router.rs`、`rust/apps/agent24d/src/comm_routes.rs`、`capabilities/route_policy.rs`）。
- Hyphae daemon 是 agent24d 的子进程，固定传 `--notify=false --auto-reply=false` 并显式传 `--relay`（`daemon.rs:750-751` **（main 已核）**）。
- 退避沿用 `RestartPolicy`（500 ms 起翻倍，5 次后熔断）；孤儿进程按 pid 加启动时间识别；关机并入 SHUT-1b，记录名 `comm.hyphae`。桌面不 spawn Hyphae。
- 停止宽限：Hyphae 守护进程消费锁里的 `stop_grace_ms`（ADR-038 §7.11）。X14 说的「没有接线」是 69c2674e 时的状态，已过期，见 §3 待办。

### D2 二进制来源、分发与信任（C07、C08、C15、C16）——X1 的结论

**X1 结论（jason 2026-11-04）：Hyphae 按需下载，不随安装包发。**

- 不打进 DMG。运行时由桌面通信页「下载并设置」触发，弹原生确认框。安装属于 `ComponentInstall`，提升级（`comm_routes.rs` 的 `/install`，`route_policy.rs`；`apps/desktop/src/main/ipc/index.ts`；#1489、#1496）。
- 因此 `docs/Deployment/HYPHAE-NETWORK-DELIVERY.md` 的「随包交付」「标准包含 hyphae」「client 不依赖首次联网下载」（`:10,25,31`）作废，该文要改（待办 T1）。
- 发布信息只有一个来源：逐字节复制上游 schema 2 的 `ComponentLock`（`rust/crates/agent24-comm/hyphae.lock.json`，当前 `v0.30.1`）。版本不写死，靠 sha256 校验，不靠 `--version`；改锁的 PR 要写明原因。CI：`.github/workflows/hyphae-lock-verify.yml`。
- 信任：macOS 安装时全量验签并检查公证，每次启动做离线 codesign；Linux 只靠 sha256 加可复现构建（ADR-038 D2a、§7.5、§7.11；`hyphae_install.rs`）。
- 二进制查找顺序：`A24_HYPHAE_BIN` → `A24_SPEAKER_BIN`（兼容，打 warn）→ 组件目录（每次按锁重验 hash）→ agent24d 同目录；不查 PATH（`COMM-HYPHAE.md` 路径规则；`comm_routes.rs`）。nostr-bridge 已删除（#1785），查找由 comm 做。旧说法「桥并入 comm 后查找由 comm 统一做，并入之前不扩展桥的查找逻辑」（已被 2026-11-12 回写取代）：入站适配不并入 comm，见 D5。

### D3 数据归属与身份（C03、C21）——X7 的结论

**X7 结论（jason 2026-11-04，2026-11-12 回写）：agent 的 Nostr 身份，以通讯专用目录 `<data>/comm/hyphae-home` 里的钥匙库为准。身份归 comm（已定）。旧的 nostr-bridge 已删除（#1785），不再是身份来源。入站适配不并入 comm，落在 `agent24d` 的 inbound 模块（见 D5）。**

- 专用 HOME `<state_dir>/comm/hyphae-home/`。旧 `~/.hyphae` 只能经 `agent24 comm import --from` 显式导入，用户确认后**复制**（`daemon.rs` 设 HOME 和 cwd；`import.rs`）。
- 默认身份只有一个来源：`identity list` 里 `default:true` 的一项（`daemon.rs:698`、`router.rs:829` **（main 已核）**）。Hyphae 的文件是消息的唯一来源，comm 不另存消息；Agent24 只存关系状态。
- nostr-bridge 过去不覆盖 HOME（`speaker.ts` 里 grep `HOME` 零命中 **（main 已核）**），实际用哪份 HOME 取决于运行环境。决定之后：**身份只认 comm 的钥匙库**。桥已删除（#1785）。旧说法「桥在并入之前不得自己生成或持有另一套身份」（已被 2026-11-12 回写取代）：没有第二套桥身份；入站适配在 `agent24d`，见 D5 与 T2。
- 私钥备份和恢复用 NIP-49 `ncryptsec`；备份文件由 daemon 自己写（`O_EXCL|O_NOFOLLOW`，0600）（`export.rs`，#1535）。这条原先只在 PR 里，现在补进决策。

### D4 凭据与授权等级（C04、C10、C20）

- keystore 口令存系统钥匙串（service `ai.idoris.agent24.comm`，account `hex(sha256(salt))`），只经 `--password-stdin`，不进 argv、env、日志或配置。`A24_COMM_PASSWORD_STORE=memory` 只给测试和联调（`password_store.rs`；#621、#628）。
- 改 keystore 的命令统一经 `KeystoreWriteLock` 串行（`keystore_lock.rs`）。
- comm 读是 Everyday，写和安装是提升级。提升级的 comm 写操作走主进程专用 IPC 并严格校验，加联系人要原生确认（`ipc/index.ts`，#1535）。
- **send 没有在场证明**：这条残余风险明确接受，等 CORE-2d / 7c 再收口（X13）。依据只在 PR 和代码注释（提交 b9d81e55）。

### D5 zero-run、桥与入站执行——X11 的结论

**X11 结论（jason 2026-11-04）：所有外部触发 agent 干活的入口，统一走 ADR-041 的准入关卡。** jason 的理由是「这样权限、安全都好做」。

适用的入口，一个不漏：

| 入口 | 现状 | 决定 |
|---|---|---|
| Nostr 入站消息（Hyphae 收件箱；nostr-bridge 已删除，#1785） | 收件箱由 `agent24d` inbound 轮询写入。旧说法「桥在桥内判名单，F4b 默认冻结」（已被 2026-11-12 回写取代） | 经 ADR-041 D1 写入 `inbound_events`，由 D3 准入决定。适配器在 `agent24d` inbound，不在 comm |
| 语音输入（AgentEar） | ADR-041 已列为第六条路径 | 同上 |
| 远程 @agent 消息 | 尚无实现 | 同上；没有单独的旁路 |
| 微信桥（wechat-bridge） | 开发工具。X11-2g（#1818、#1819）起只做传输，不再 `POST /api/v1/runs`，不再自持名单，也不在微信里回 y/n。旧说法「桥内判 uid 名单」（已被 2026-11-12 回写取代） | **开发工具，不进产品构建，产品构建里不得启动。** 开发态把消息作为 `source = wechat` 的事件交给 ADR-041 的准入，名单与去重由 daemon 持有 |
| 手机遥控（ADR-037 决策 6） | 设计文档不存在（C29） | 遥控只是准入的**一种来源**，配对设备身份由准入表的名单承载；ADR-037 决策 6 里「独立的授权入口」按本条理解为「准入的一个来源」，不是绕过准入的第二条路 |

- **基础通信保持 zero-run**：收发消息、联系人、relay 管理不触发任何 run。结构约束保留：comm 依赖图有白名单，禁止 HTTP 客户端 crate，`CommState` 不含 Store、ModelRouter（`agent24-comm/tests/comm_dependency_allowlist.rs`、`tests/comm_zero_run.rs`；C06）。
- **F4b 冻结**（已被 2026-11-12 回写取代）：旧 nostr-bridge 与 `A24_NOSTR_F4B_INBOUND` 已随 X11 ① 删除（#1785）。入站执行以 ADR-041 准入为准，适配器在 `agent24d` inbound，不在 comm。待办 X5 里的 `inbound.ts` 路径是删除前的记录。
- **好友不等于可以指挥 agent**：好友表不写进 `A24_NOSTR_ALLOWED_NPUBS`，好友协议消息也 zero-run（C24）。能否让 agent 干活只看 ADR-041 的准入表。
- **Q6 (c) 桥的归宿**（2026-11-12 回写：入站适配不进 comm）：
  - nostr-bridge 已删除（X11 ①，#1785）。入站适配在 `agent24d` 的 `inbound` 模块（Hyphae 轮询写 `inbound_events`），**不是** `agent24-comm`。理由：comm 的依赖白名单与 zero-run 约束（本节上文；`comm_dependency_allowlist.rs`、`comm_zero_run.rs`）不允许持有 Store 或起 run。身份仍由 comm 管。
  - wechat-bridge **维持开发工具**，不进产品路径。开发态也不得绕过准入（X11-2g，#1818、#1819）：它不再 `POST /api/v1/runs`。
  - **产品构建不启动桥**（已被 2026-11-12 回写取代）。旧 nostr-bridge 已删除（#1785）。wechat-bridge 维持开发工具，且不再 `POST /api/v1/runs`（T3，#1818、#1819）。旧说法「在并入之前禁止桥使用 legacy 全权 token，代码尚未实现」不再成立：入站适配不并入 comm。按组件发受限令牌仍依赖 ADR-038 C9（未排期）。

### D6 消息状态语义与可靠性（C11、C12、C13、C19）

- 错误是封闭集合并映射 HTTP 状态码；`partial` 原样透传 data；send 不自动重试，也没有「重发」，只能按原 event_id 走 outbox retry（`error.rs`；#627）。
- 消息状态分四层。L3「对端确认」只占位，UI 永远不显示「已送达」。
- daemon 状态拆成 process、relay_probe、catch_up 三项分别给出，互不推导；catch_up 永远不报「完成」（#633）。
- `POST /comm/inbox/pull` 供手动或没有 daemon 的场景拉取。
- 去重键用 Hyphae 的真实 hex `event_id`；入站读 `history inbox --as`（F4，C27；`inbound.ts` 去重）。

### D7 网络与 relay（C17、C18）

- `source != "config"` 时当作未配置，daemon 启动时总是显式传 relay。桌面预填的默认 relay 是 Hyphae 自建的 `wss://relay.aastar.io`（`relayDefaults.ts:2` **（main 已核）**，#1529）；设置 relay 前弹原生确认。
- 「公共 relay 算不算暗中回落」（X12）：现状是桌面路径预填并有确认框，CLI 与 headless 的默认值未定义。这是否算暗中回落、headless 默认用什么，由 T4 决定，本篇不定。
- Windows 在 DEP-A7 之前一律返回 `unsupported_platform`。

### D8 应用层协议与命名空间（C22、C23、C25–C28、C26）

- F4：不 vendor 源码，只驱动二进制；信封三个（say / announce / listen），意图开放枚举；内容格式 `f4/1`。每条 agent msg 加唯一 `d` 标签由上游实现（C26，Agent24 侧无代码）。
- 对外只发布业务能力，不发原子工具（C28）。「默认注册、默认开」与 `profile publish` 的关系见待办 X10：本篇不把公开广播能力写成已定，评审时单独定。
- 群聊已由 #1377 接入（`groupchat.rs`），COMM-HYPHAE 的「群聊是非目标」作废，群聊在范围内。
- 好友协议归 Agent24：明文前缀 `agent24.friend/v1`，走 kind 30078 加 NIP-44；默认不告诉对方被拒；裸 Hyphae 对端回一条消息算同意；已有联系人默认算好友；v1 存本地并纳入备份；不做 NIP-05（C23，`COMM-FRIENDS.md:5,291,295-300`）。**这些是已拍板、未实现的设计决策**（grep `friends.json`、`agent24.friend` 零命中），实现另排（§11）。

### D9 隐私与威胁模型

- relay 能看到 `p` 标签（明文）；NIP-17 gift wrap 等 T01（X17）。
- 发消息的正文目前经 argv 传给 Hyphae（`router.rs:1673` `--content` **（main 已核）**），本机其他进程可能从进程列表看到。Hyphae v0.28.1 起有 `--content-file`，应改用（待办 X8）。
- 屏蔽只在本机生效。同 UID 进程不在防护范围内（与 ADR-038 §7.11 (b) 一致）。

## 3. 订正待办

下面是文档或代码与本篇不一致的地方。**文档类**由后续文档片订正；**代码类**不在 S0-4 内，各自开片。

| # | 来源 | 内容 | 类别 |
|---|---|---|---|
| X2 | R3 X2 | `COMM-HYPHAE.md:418-427`（§8.1）还写 schema 1、含 `null`；实际锁是 schema 2，ADR-038 规定不写 `null` | 文档 |
| X3 | R3 X3 | `COMM-HYPHAE.md:154` 写 `fetch_verified`、`idoris/component-cache/hyphae`、`v0.27.2`；代码是 `download_locked_asset`、`components/hyphae-cache`、锁 `v0.30.1`。`ARCHITECTURE-LAYERS.md:153,317` 还写「借用 iDoris 下载器」（S0-5 一并订正） | 文档 |
| X4 | R3 X4 | `COMM-HYPHAE.md:307-308` 要求传 `--watch-interval 30 --retry-interval 60`；`daemon.rs:745-760` 没传（main 已核：搜不到 `watch-interval`）。要么补参数，要么改文档，评审时选 | 代码或文档 |
| X5 | R3 X5 | `COMM-HYPHAE.md:380` 说拦截在 `InboundBridge.handle` 开头；实际在 `pollOnce`（`inbound.ts:200-245`），`handle` 不看开关 | 文档 |
| X8 | R3 X8 | 正文进 argv，应改用 `--content-file`（`router.rs:1673`） | 代码 |
| X9 | R3 X9 | `COMM-HYPHAE.md:467` 说锁停在 `a4aa606`、G2「没有结构化健康状态」；现在锁是 v0.30.1，且已有 `contract`/`health`（C2）。G1–G9 状态要回写 | 文档 |
| X14 | R3 X14 | R3 说 `stop_grace_ms` 没接线；ADR-038 §7.11 记录 Hyphae 守护进程已消费该字段，X14 的「没有接线」已过期。`COMM-HYPHAE.md:320` 只提环境变量，补上锁字段的说明 | 文档 |
| X15 | R3 X15 | `COMM-FRIENDS.md:182` 要求在 `COMM-HYPHAE.md` §2 补一行「关系状态在 `comm/friends.json`」，还没补 | 文档 |
| X16 | R3 X16 | 各文档引用的 Hyphae 版本基线不一致：`COMM-HYPHAE.md:5`（`a4aa606`）、`COMM-FRIENDS.md:7`（v0.28.1）、ADR-038 r5 稿 `:135`（`44189b8e`，示例值，r5 已加注）。统一为「以 `hyphae.lock.json` 为准」 | 文档 |

由本篇新增的待办（不是 R3 编号）：

| # | 内容 | 类别 |
|---|---|---|
| T1 | `HYPHAE-NETWORK-DELIVERY.md:10,25,31` 的「随包交付」改成按需下载（X1 结论） | 文档 |
| T2 | **已完成（2026-11-12，X11 ②③④）**：入站适配并入 `agent24d` 的 `inbound` 模块，不并入 `agent24-comm`（依赖白名单与 zero-run，见 D5）。X11 ①（2026-11-10，#1785）已删除旧 nostr-bridge 及 F4b；身份仍由 comm 管理。②③④ 的 PR：#1797 方案，#1805 / #1806（2a），#1803（2b），#1810 / #1813 / #1816（2c），#1808 / #1812（2d），#1820（迁移已合，bb03081e）及 #1821（2e 第 2 部分 Hyphae 轮询，已合 a93f5df3；群聊每轮取最新一页、已入库跳过，`inbound_cursor` 表保留暂不使用），#1809 / #1811（2f），#1818 / #1819（2g）。原待办里「并入 comm 后删除桥内名单」已改为 daemon 持有名单与去重 | 代码 |
| T3 | **已完成（2026-11-12，X11 ① 与 2g）**：旧 Nostr 桥已删。wechat-bridge 维持开发工具，启动必须 `A24_DEV_TOOL=1`，且不再 `POST /api/v1/runs`，只把消息交给 daemon 入站准入（#1818、#1819）。产品构建不启动桥。原待办里的「按组件发受限令牌」仍依赖 ADR-038 的 C9（未排期）。F4b 已随旧桥删除，不再是入站执行路径 | 代码 |
| T4 | 定义 CLI 与 headless 场景的默认 relay（X12） | 决策 |
| T5 | ADR-037 决策 6 的措辞与 D5 对齐（遥控是准入的一种来源）；`decision.md` 的订正归 S0-1 | 文档 |

**R3 里的其余未决项，本篇不定**：X6 随 T2 消失；X10（非目标与实际不一致、是否公开广播）、X13（send 在场证明，等 CORE-2d / 7c）、X17（T01 协议）、X18（COMM-6/7 完成证据缺失）、X19（COMM-FRIENDS 开放问题）。

## 4. 与 ADR-041 的边界

**ADR-041 D1 写的「nostr-bridge 与 wechat-bridge 改为只做传输，名单与去重搬进 daemon」，其中 wechat-bridge 的部分以本篇为准**：wechat-bridge 维持开发工具，不进产品路径（Q6 (c)），不是产品里要迁移的适配器。入站适配在 `agent24d` 的 inbound 模块（T2），不在 comm。ADR-041 的开放问题 Q4（桥的名单搬进 daemon 的行为差异）已由 X11-2f / 2g 落地：名单在 daemon，微信桥不再自持名单。

两份 ADR 管不同的东西，不重叠：

| | ADR-044（本篇） | ADR-041 |
|---|---|---|
| 管什么 | 通信这项能力**怎么交付、怎么运行、怎么收发**：Hyphae 的分发与信任、身份与数据、凭据、消息状态、relay、应用层协议 | 入站事件**进入本体之后怎么处理**：`inbound_events` 表、准入表、接受模式、工单 |
| 入站消息 | comm 负责**拿到**消息（拉取、去重、标 `actor` / `human` / `addressed`），不判断谁有权让 agent 干活 | 负责**准入**（名单 × 是否 @ × 接收范围）和之后的一切 |
| 名单与去重 | 不再保留在桥或 comm 里 | 持有（D1：名单与去重在 daemon 的 inbound 模块） |
| 遥控、语音、远程 @agent | 只声明它们走准入（D5） | 定义准入表如何容纳这些来源。手机遥控本里程碑只预留来源名 |
| 状态 | 已接受（#1593、#1601）。X11 ②③④ 已落地，见 T2 | 041a（D1–D4、D10）已接受并已实现；041b（D5–D9 工单）仍是提议 |

具体接口：comm 作为事件源，在 ADR-038 D9 清单里加 ADR-041 D2 的可选块 `events`；comm 保证来源侧职责（持读位置、先过滤自己发的消息、标明 `actor` 与 `human`）；准入判定不在 comm。若 ADR-041 被修改或被拒绝，D5 里「统一走准入」的结论仍然成立，只是准入关卡的实现换一个载体，不回到桥内判定。

## 5. 被否决的方案

出处是 R3 §3.A.3 第 12 节的候选，这里只列有出处的：Hyphae 原生好友状态机、用 kind 3 当好友、NIP-17 现在就上、systemd 凭据、周期探测、日志轮转、`GET /comm/inbox`、靠 `--version` 校验（`COMM-FRIENDS.md:132-135`；`COMM-HYPHAE.md:32,209,493`）。每项的具体理由见出处，本篇不重述。

## 6. 平台

Windows 在 DEP-A7 之前不支持，放开时要补 env 白名单（`COMM-HYPHAE.md:460-461`；`docs/design/WINDOWS-PORT.md:130`）。macOS 钥匙串 ACL 与签名绑定带来的问题，见 `COMM-HYPHAE.md` 钥匙串一节，本篇不展开。

## 7. 后果

- **好处**：三处矛盾有了结论；ADR-041、COMM-FRIENDS、遥控设计有上位依据；「谁能让 agent 干活」只有准入这一个答案，权限和审计只做一套。
- **代价**：入站适配在 `agent24d` 的 inbound 模块，不在 comm（T2）；产品构建不启动 wechat-bridge（T3）；遥控设计要按「准入来源」重写措辞（T5）。
- **风险**：041b 工单仍是提议。041a 准入已经落地（T2）；入站执行以准入为准，不再依赖已删除的 F4b。

## 8. 已拍板（jason，2026-11-04）

1. X1：Hyphae 按需下载，不随安装包发。
2. X7：Nostr 身份以 `<data>/comm/hyphae-home` 的钥匙库为准；nostr-bridge 不再是身份来源。入站适配不并入 comm，在 `agent24d` 的 inbound 模块（2026-11-12 回写，见 D5、T2）。
3. X11：Nostr 入站、语音输入、远程 @agent、手机遥控，一律经 ADR-041 准入。②③④ 已落地（T2）；手机遥控本里程碑只预留来源名。
4. Q6 (c)：nostr 入站适配在 `agent24d` inbound；wechat-bridge 维持开发工具。
5. 预览版停发，主线是组件整合与统一。

## 9. 关联与取代

- 取代：`COMM-HYPHAE.md` §0 的「已定决策」、`F4-nostr-channel.md` 的决策节、`COMM-FRIENDS.md` §8 的已拍板节，三者降为实施说明（文头加注由后续文档片做，本篇不改）。
- 与 ADR-037 决策 1（常驻 daemon）一致。对决策 6（遥控）做**收窄订正**：遥控不是「独立的授权入口」，而是准入的一种来源，待 T5 回写 `decision.md`，回写前以本篇 D5 为准。
- 与 ADR-038 的关系：Hyphae 是该 ADR 的第一个按次调用型加常驻型组件；锁、签名、契约版本、状态机全按它。
- 与 ADR-039 的关系：通信是能力，不是技能；能力不能指定技能。
- 与 ADR-041 的关系：见 §4。
