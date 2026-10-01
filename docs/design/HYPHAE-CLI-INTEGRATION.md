# Agent24 × Hyphae：基础通信接线提案

状态：跨仓协作提案，待 Agent24 确认；不代表已实现接口，也不冻结 T01 协议。记录时间：2026-09-30。

## 2026-10-01：Hyphae 第一轮独立复测与接口确认

下文 2026-09-30 的源码/构建快照保留为历史记录。COMM-0 与 Agent24 第一轮记录分别见 [#612](https://github.com/iDoris-ai/Agent24/pull/612) 和 [JOINT-ROUND1](../comm/JOINT-ROUND1.md)；新实现状态按各自 PR 和实际 main 判断，不沿用历史“没有通信子命令”的结论。

本轮固定 Hyphae source `a4aa606eb81d5c040d94c51cdf94553e646d8674`，采用生产 [hyphae.lock.json](../../rust/crates/agent24-comm/hyphae.lock.json) 的 Go 1.26.4、CGO_ENABLED=0、`-trimpath -buildvcs=false -ldflags='-buildid='` 配方。macOS arm64 CLI SHA-256 为 `f53c29b31d8ca5eb0124ced246bcff6610f048f18bc8dcc2de27f685dad8b221`；relay 为 `a012d86e549cbeb564d5a5932c54f9b3511c2434203846537096420c89f36aef`。它们与 Agent24 第一轮制品一致；旧 Go 1.27.1 的 `bc30dcf7…` 不用于本轮。

Hyphae 用两个临时 HOME、合成加密身份及本地 relay 独立运行一次，exit 0。实际断言覆盖：双方加密身份、互加联系人、显式 relay、拉取前本地 history 为空、A→B 与 B→A 的事件 ID/明文/加密标记对应、断线入队、重启 relay 后原 event_id 重试、outbox 清空和重复拉取后历史恰一条。错误断言为错口令 exit 3/auth_error、非法 contact 公钥 exit 4/other_error、发送给不存在联系人 exit 1/user_error；最后一项不冒充 Agent24 的“非法 npub 发送”测试。所有 CLI 调用均为独立进程；本轮没有启动 Agent24 的生产入口，也不验收 Agent24 daemon 重启、模型/模块计数或基础 UI。

原始阶段日志 SHA-256 为 `2ce0224d333a543774af7b8b05dbebcfd7a4066c2db6db725762f29ef1327d20`，精确无口令命令 SHA-256 为 `ea17291b21f53675ab30432bf31c5546910afdfb89c23db2610c94918b24eac9`。本地证据保存于 Hyphae `build/agent-handoff/20261001/real-a4-go1264/round1-unblocked/`；该目录是本机生成物，不是公开下载地址。可复现制品构建工具正在 [Hyphae #102](https://github.com/iDoris-ai/Hyphae/pull/102) 评审，固定 Release 及直接下载 URL 尚待真实 CI/下载验收后交付。

确认 `history inbox` 只读本地数据库：新消息须先经 `agent inbox --as ID --password-stdin` 拉取，或由已解锁的托管 daemon 直接查询、解密及保存后才可见；`--decrypt` 不必显式传 true。daemon 不启动 inbox 子进程；默认 watch interval 为 30s，启动先扫描再按间隔扫描，不保证错误/取消/分页未完成时在 30s 内补齐。进程存活、一次扫描完成和永久同步分别判断。

第二轮按 [JOINT-ROUND2](../comm/JOINT-ROUND2.md) 的候选证据继续：COMM-4a [#626](https://github.com/iDoris-ai/Agent24/pull/626) 在 `ba30f104…` 已获新的外部批准、CI 通过；COMM-3 [#627](https://github.com/iDoris-ai/Agent24/pull/627) 仍是依赖草稿。前置合入后，由 Agent24 在最终实际 main 提供托管/收发、持久凭据重启、125 条积压与重启零新增、run/model/module 三类计数及同量具有效正对照。候选组合的 0→0 记录不能代替这些出口；T21/T22 UI、T01-E 和四仓闭环仍未验收。

## 源码与验收快照（2026-09-30）

本文记录两仓在该日的源码和验收快照，不将其定义为首个或永久版本锁；COMM-0 设计评审需由双方确认首个集成锁清单，随后每个实现 PR 都回填双方 commit SHA、所用平台 binary SHA-256 和验收结果。

- Agent24 main 快照：[`bf3322cf`](https://github.com/iDoris-ai/Agent24/commit/bf3322cfc7674c1c57bd20eb159efdb2a42eb28a)。该版本 Rust CLI [main.rs](https://github.com/iDoris-ai/Agent24/blob/bf3322cfc7674c1c57bd20eb159efdb2a42eb28a/rust/apps/agent24-cli/src/main.rs) 仍没有身份、联系人、relay、消息或 outbox 通信子命令。Nostr bridge 仍通过子进程调用旧命名的 `agent-speaker`，`A24_SPEAKER_BIN` 默认值仍为 `agent-speaker`；见 [config.ts](https://github.com/iDoris-ai/Agent24/blob/bf3322cfc7674c1c57bd20eb159efdb2a42eb28a/packages/nostr-bridge/src/config.ts) 和 [speaker.ts](https://github.com/iDoris-ai/Agent24/blob/bf3322cfc7674c1c57bd20eb159efdb2a42eb28a/packages/nostr-bridge/src/speaker.ts)。
- Hyphae CLI 联调候选快照：[`a4aa606e`](https://github.com/iDoris-ai/Hyphae/commit/a4aa606eb81d5c040d94c51cdf94553e646d8674)，Go 模块最低版本 1.26；本机 macOS arm64 以 Go 1.27.1 构建的验收 binary SHA-256 为 `bc30dcf7bcf8b5c1865a3e995518c2bdab224bd8d6a3a064c4d7fc780de5e2b7`，本机验收路径 `/Users/jason/.local/share/hyphae-pr-monitor/state/validation-artifacts/cli-integration-a4aa606/macos-arm64/hyphae`，其 `manifest.json` 记录来源、平台和 hash。该文件仅供这台机器联调，未安装到生产环境，也不是 release；Agent24 不应硬编码此路径，其他平台须分别构建并校验 hash。此候选可用于启动 CLI 联调，不表示双方已确认正式版本锁，也不冻结 T01 协议。
- `a4aa606e` 的默认 Go tests、integration tests、vet 和 build 均已通过；CI [36738033201](https://github.com/iDoris-ai/Hyphae/actions/runs/36738033201) 在 Linux 与 macOS 成功。该快照的真实 CLI + relay 测试导入 125 条有效积压事件，首次产生 125 个新消息效果；daemon 重启后产生 0 个新效果。此证据验收的是 Hyphae 候选，不代替 Agent24 跨仓接线验收。
- 加密 `profile publish --password-stdin` 已在 Hyphae [`6bd9e437`](https://github.com/iDoris-ai/Hyphae/commit/6bd9e4376bc1e76a47132d2a97fdd8faceaa1091) 合并（[#90](https://github.com/iDoris-ai/Hyphae/pull/90)）。任何使用此能力的 Agent24 实现 PR 都应锁定包含该提交的 Hyphae 版本并单独验收。

## 接线原则

1. Agent24 建一个共享通信服务，让 CLI、bridge 和 UI 共用身份、配置、outbox 与 history；UI 不复制 Nostr、加密或重试实现。
2. 参考架构提案：通信服务放在 `agent24d`，提供 `/api/v1/comm/*` REST；`agent24 comm ...` CLI 和桌面 UI 共用该 REST。由 `agent24d` 作为子进程监管 Hyphae daemon。COMM-0 评审后再冻结架构。当前旧 bridge 使用 `agent-speaker` / `A24_SPEAKER_BIN`；迁移时改用 Hyphae 命名并保留旧变量兼容。调用以参数数组执行固定 binary 路径，不拼 shell 字符串或依赖 PATH 偶然同名程序；通过 SHA-256 校验 binary。
3. Agent24 拟使用专用 HOME：`<state_dir>/comm/hyphae-home/`；旧 `~/.hyphae` 身份只经用户明确确认的显式 import 复制。生产密码由平台凭据库保管（macOS 钥匙串及其他平台对应凭据存储），只经独立 stdin 管道传 `--password-stdin` 并及时关闭；最多 4096 字节，只移除末尾一组 LF/CRLF，保留密码空格，不放 argv、日志或持久配置。所有验收始终用临时 HOME 与合成身份，不访问生产身份。
4. JSON 成功从 stdout 读单一 `{"ok":true,"data":...}`；错误从 stderr 读 envelope 并保留退出码。即使发送非零退出，也解析错误 envelope 内的 `data`，因为事件可能已经入队或被 relay 接受。
5. 状态分层显示：本地保存/待发、relay 接受、对端确认、执行结果不是同一状态。没有对端回执时不显示“已送达”；普通入站或旧消息不能因此触发 Agent run。

## Hyphae 命令接口快照

以下是 Hyphae `a4aa606e` 的参数现状，属于 Hyphae 侧已有能力，不是 Agent24 当前命令。Agent24 外层命令名和 REST 路由需经 COMM-0 评审。

| 能力 | Hyphae 参数与结果 |
|---|---|
| 身份 | `identity list --json`、`identity create --nickname NAME [--default] --json`、`identity use --nickname NAME --json`；JSON 只输出公开字段。只有创建/追加加密身份时使用 `--password-stdin`；list/use 不需要也不加此 flag。 |
| 联系人 | `contact list --json`、`contact add --nickname NAME --npub NPUB [--role ROLE] --json`；不需要也不加密码 flag。 |
| Relay | `relay list --json`、重复 `relay set --relay URL --json`（完整替换）、`relay info [URL] --timeout 5 --json`。`connected=true` 只说明一次 WebSocket 握手，不说明持续在线、已订阅或已送达。 |
| 发送 | `agent msg --from ID --to CONTACT_OR_NPUB --content TEXT --json`；加密库追加 `--password-stdin`。结果包含 `event_id`、`published_to`、`queued_for_retry`、`history_stored` 等字段。`ok:true`/exit 0 可仅表示可靠入队，检查 `published_to > 0` 才能判 relay 已接受；没有对端回执时不判送达。 |
| 收件/历史 | `agent inbox --as ID --limit N --json` 是有上限的单次 relay 查询，加密库解密补收时追加 `--password-stdin`；`history inbox --as ID --limit N --json` 只读本地持久历史，无密码 flag。新消息经 inbox 或托管 daemon 补收后才可见；一次 inbox 不是完整同步证明。 |
| 待发队列 | `storage outbox list --json`、`storage outbox retry --id EVENT_ID --json`、`storage outbox clear --failed --yes --json`。clear 删除本地待发项，不撤回 relay 已接受事件；UI 清理前展示范围并确认。 |
| Daemon | `daemon --identity ID --password-stdin --notify=false --auto-reply=false --relay URL --json` 是长驻进程；`--notify` 默认 `true`，Agent24 监管时须显式设为 `false`。`--json` 在 a4 快照仅用于结构化启动错误，运行期间 stdout/stderr 输出日志，没有 JSON 消息流或健康状态 API。`hyphae --version` 输出 `hyphae version dev`，不能作为版本校验；用 binary SHA-256 校验。Agent24 管理进程和日志；消息状态从 history/outbox 查询。第一轮配方不注入版本 ldflags，以保持生产 lock 摘要。 |

源码入口：[身份/联系人](https://github.com/iDoris-ai/Hyphae/blob/a4aa606eb81d5c040d94c51cdf94553e646d8674/internal/identity/commands.go)、[relay](https://github.com/iDoris-ai/Hyphae/blob/a4aa606eb81d5c040d94c51cdf94553e646d8674/internal/nostr/relay.go)、[消息](https://github.com/iDoris-ai/Hyphae/blob/a4aa606eb81d5c040d94c51cdf94553e646d8674/internal/messaging/agent.go)、[history](https://github.com/iDoris-ai/Hyphae/blob/a4aa606eb81d5c040d94c51cdf94553e646d8674/internal/messaging/commands.go)、[outbox](https://github.com/iDoris-ai/Hyphae/blob/a4aa606eb81d5c040d94c51cdf94553e646d8674/internal/messaging/outbox_commands.go)、[daemon](https://github.com/iDoris-ai/Hyphae/blob/a4aa606eb81d5c040d94c51cdf94553e646d8674/internal/daemon/daemon.go)。

发送或重试失败也可能带部分结果。需检查 `event_id`、`published_to`、`queued_for_retry`、`history_stored`、`superseded`、`queue_state_unknown`。relay 已接受而本地记账失败时，按原 `event_id` 核对，不创建新事件重发。重试用原签名 event ID；并发冲突或队列未知时先重新读取 outbox。

Relay 配置和默认身份在命令/daemon 启动时读取。修改后由统一通信服务重启对应 daemon；不假设运行进程自动重配，也不改写旧待发事件记录的 relay。进程存活、relay 探测成功、历史补收完成分别表达。

**Headless 注册：**`profile publish --password-stdin` 已在 `6bd9e437` 合并。需要此能力的实现 PR 必须锁定包含它的 Hyphae SHA/binary，并对加密注册和密码 stdin 做独立验收。

## 分阶段交付建议

先单独评审并冻结 Agent24 COMM-0 架构与首个双方锁清单。下表 T20-A/T20-B/T21/T22 是 Hyphae 侧工作编号；Agent24 的 `COMM-*` 对应编号待 COMM-0 确认，不在本文猜测具体编号。每个实现 PR 与本文都应双向标注两套编号。以下外层命令仅为候选名；快照中的 Rust CLI 尚无这些通信命令。

| Hyphae 编号 | Agent24 编号 | 交付 | 通过条件 |
|---|---|---|---|
| T20-A | COMM-*（待 COMM-0 确认） | 固定 binary/config 与输出 envelope；身份、联系人、relay 管理。候选 `agent24 comm identity|contact|relay ...`。 | 实际子进程调用；公开 JSON 可解析；密码仅经 stdin；专用 HOME；配置可被后续命令读取；binary 缺失或 SHA-256 校验失败时拒绝启动；`--version` 的 `dev` 输出不能替代 hash 校验。 |
| T20-B | COMM-*（待 COMM-0 确认） | 发信、history、outbox、daemon 生命周期。候选 `agent24 comm send|inbox|history|outbox ...`。 | 本地真实 relay；断线重试使用同一 event ID/签名；保留部分错误结果；同 HOME 重启后历史稳定且无重复新消息效果；daemon 可停止。 |
| T21 | COMM-*（待 COMM-0 确认） | 身份、联系人、relay 管理 UI。 | 复用 T20 通信服务、配置与状态；未连接时不伪装为健康。 |
| T22 | COMM-*（待 COMM-0 确认） | 消息、history、待发及重试 UI。 | 区分本地入队、relay 接受和对端确认；队列未知/并发冲突先核对，不创建新事件重发。 |

## 必须独立验收的边界

- 每个 T20/T21/T22 PR 固定 Agent24/Hyphae commit、binary 版本/hash、退出码和测试结果。Hyphae 验收用临时 HOME、合成身份、本地 relay；不得访问用户生产密钥、relay 数据或全局 `~/.hyphae`。
- 首轮验收覆盖身份/contact/relay 管理、加密发送及 partial error、断线后按原 event ID 重试、history/outbox 查询、专用 HOME 下 daemon 启停，以及 125 条离线积压和重启后零重复新消息效果。Agent24 侧用 spy/counter 证明普通入站/query/receipt 不启动 run，并证明 F4b 已关闭且单一消费路径生效；Hyphae 收件去重不等于 Agent24 执行去重。
- Agent24 现有 F4b 白名单入站 gated-run 路径由 Agent24 决定冻结：不再扩展并默认不启用；入站执行统一由 T01-E 收口后的高层授权路径承接，避免同一 `event_id` 被两条路径消费。本文记录的是方案决定，当前源码是否禁用及 single-consumer 保证仍需 Agent24 实现 PR 与 spy/counter 证据验证。基础通信保持 zero-run：普通入站、旧 kind 30078 消息、query 和 response 不进入模型、模块或 run。T01-E 收口后再实施高层授权与持久化 request/run，然后进行四仓联调验收；不把这些功能混入 T20/T21/T22。
- iDoris 正式 provider/预算、Sin90 授权与 manifest 生命周期、AgentEar 语音各自独立推进，不属于本提案范围，也不由本提案记完成。

## 待 Agent24 确认

1. 通过 COMM-0 单独评审冻结 REST/进程/HOME/凭据架构、首个双方版本锁清单，并确定 COMM-* 与 T20-A → T20-B → T21 → T22 的双向编号映射和外层命令名。
2. 明确 Hyphae binary 的构建、跨平台 hash、安装/更新责任、应用专用 HOME、平台凭据库和 `agent-speaker` / `A24_SPEAKER_BIN` 兼容迁移。
3. 确认部分成功 envelope、daemon 生命周期及 headless 注册的产品呈现；含 `profile publish --password-stdin` 的实现版本须独立验收。
4. 每个实现 PR 回填双方 SHA、集成测试、CLI/UI 验收结果；分别记录 T01-E、iDoris provider、Sin90 授权和四仓状态。
