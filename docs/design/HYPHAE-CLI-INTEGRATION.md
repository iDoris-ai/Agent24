# Agent24 × Hyphae：基础通信接线提案

状态：跨仓协作提案，待 Agent24 确认；不代表已实现接口，也不冻结 T01 协议。记录时间：2026-09-30。

## 固定依据

- Agent24 main：[`4c236bcc`](https://github.com/iDoris-ai/Agent24/commit/4c236bcc162b52d2a4d82f258295034f08682336)。Rust CLI [main.rs](https://github.com/iDoris-ai/Agent24/blob/4c236bcc162b52d2a4d82f258295034f08682336/rust/apps/agent24-cli/src/main.rs) 目前没有统一身份、联系人、relay、消息或 outbox 通信子命令。Nostr bridge 仍经子进程调用 CLI，`A24_SPEAKER_BIN` 默认 `agent-speaker`，配置与调用见 [config.ts](https://github.com/iDoris-ai/Agent24/blob/4c236bcc162b52d2a4d82f258295034f08682336/packages/nostr-bridge/src/config.ts) 和 [speaker.ts](https://github.com/iDoris-ai/Agent24/blob/4c236bcc162b52d2a4d82f258295034f08682336/packages/nostr-bridge/src/speaker.ts)。#594 触及桌面 UI，但未增加上述通信管理接线；本提案仅记录身份/联系人/relay/消息/历史/待发管理尚未接通，不评述模型页或其他 UI。
- Hyphae 已验收 main：[`1948aadc`](https://github.com/iDoris-ai/Hyphae/commit/1948aadc551e360176711f9c50172ed6edccd253)。Go 模块声明最低版本 1.26；本机 macOS arm64 二进制以 Go 1.27.1 构建，SHA-256 `a7bb4a83b5d6be0a939a4cd92a853a2672f97012c48d704a9a3a718b9e6d806b`。它未安装到生产环境；其他平台要分别构建并记录 hash。
- 固定 main 的默认测试、integration、vet、build 和 smoke 已通过；CI [36729070274](https://github.com/iDoris-ai/Hyphae/actions/runs/36729070274) 通过。真实 CLI + 本地 relay 测试覆盖 125 条离线积压及同库 daemon 重启后零重复新消息效果。以固定源码、测试和 CI 为行为证据；Hyphae 的待审文档不是冻结协议。

## 接线原则

1. Agent24 建一个共享通信服务，让 CLI、bridge 和 UI 共用身份、配置、outbox 与 history；UI 不复制 Nostr、加密或重试实现。
2. 第一阶段复用 Hyphae CLI，以参数数组启动固定路径的二进制，不拼 shell 字符串。兼容 `A24_SPEAKER_BIN`，新增设置应固定 binary 路径和版本，不能依赖 PATH 中偶然同名的程序。
3. 产品运行时使用 Agent24 持有并显式指定的应用专用 HOME，不默认读取登录用户的全局 `~/.hyphae`。迁移既有用户身份须单独取得明确授权，不自动复制或复用。所有验收始终用临时 HOME 与合成身份，不访问生产身份。密码只通过独立 stdin 管道传 `--password-stdin` 并及时关闭；最多 4096 字节，只移除末尾一组 LF/CRLF，保留密码空格，不放 argv、日志或持久配置。
4. JSON 成功从 stdout 读单一 `{"ok":true,"data":...}`；错误从 stderr 读 envelope 并保留退出码。即使发送非零退出，也解析错误 envelope 内的 `data`，因为事件可能已经入队或被 relay 接受。
5. 状态分层显示：本地保存/待发、relay 接受、对端确认、执行结果不是同一状态。没有对端回执时不显示“已送达”；普通入站或旧消息不能因此触发 Agent run。

## Hyphae 已有命令接口

以下是固定 main 的参数现状，属于 Hyphae 侧已有能力；不是 Agent24 现有命令。Agent24 的统一外层命令名需先讨论确认。

| 能力 | Hyphae 参数与结果 |
|---|---|
| 身份 | `identity list --json`、`identity create --nickname NAME [--default] --json`、`identity use --nickname NAME --json`；JSON 只输出公开字段。只有创建/追加加密身份时使用 `--password-stdin`；list/use 不需要也不加此 flag。 |
| 联系人 | `contact list --json`、`contact add --nickname NAME --npub NPUB [--role ROLE] --json`；不需要也不加密码 flag。 |
| Relay | `relay list --json`、重复 `relay set --relay URL --json`（完整替换）、`relay info [URL] --timeout 5 --json`。`connected=true` 只说明一次 WebSocket 握手，不说明持续在线、已订阅或已送达。 |
| 发送 | `agent msg --from ID --to NPUB --content TEXT --json`；加密库追加 `--password-stdin`。结果包含 `event_id`、`published_to`、`queued_for_retry`、`history_stored` 等字段。 |
| 收件/历史 | `agent inbox --as ID --limit N --json` 是有上限的单次 relay 查询；`history inbox --as ID --limit N --json` 读本地持久历史。一次 inbox 不是完整同步证明。 |
| 待发队列 | `storage outbox list --json`、`storage outbox retry --id EVENT_ID --json`、`storage outbox clear --failed --yes --json`。clear 删除本地待发项，不撤回 relay 已接受事件；UI 清理前展示范围并确认。 |
| Daemon | `daemon --identity ID --password-stdin --json` 是长驻进程；`--json` 仅用于结构化启动错误，运行期间 stdout/stderr 输出日志，没有 JSON 消息流或健康状态 API。Agent24 管理进程和日志；消息状态从 history/outbox 查询。 |

源码入口：[身份/联系人](https://github.com/iDoris-ai/Hyphae/blob/1948aadc551e360176711f9c50172ed6edccd253/internal/identity/commands.go)、[relay](https://github.com/iDoris-ai/Hyphae/blob/1948aadc551e360176711f9c50172ed6edccd253/internal/nostr/relay.go)、[消息](https://github.com/iDoris-ai/Hyphae/blob/1948aadc551e360176711f9c50172ed6edccd253/internal/messaging/agent.go)、[history](https://github.com/iDoris-ai/Hyphae/blob/1948aadc551e360176711f9c50172ed6edccd253/internal/messaging/commands.go)、[outbox](https://github.com/iDoris-ai/Hyphae/blob/1948aadc551e360176711f9c50172ed6edccd253/internal/messaging/outbox_commands.go)、[daemon](https://github.com/iDoris-ai/Hyphae/blob/1948aadc551e360176711f9c50172ed6edccd253/internal/daemon/daemon.go)。

发送或重试失败也可能带部分结果。需检查 `event_id`、`published_to`、`queued_for_retry`、`history_stored`、`superseded`、`queue_state_unknown`。relay 已接受而本地记账失败时，按原 `event_id` 核对，不创建新事件重发。重试用原签名 event ID；并发冲突或队列未知时先重新读取 outbox。

Relay 配置和默认身份在命令/daemon 启动时读取。修改后由统一通信服务重启对应 daemon；不假设运行进程自动重配，也不改写旧待发事件记录的 relay。进程存活、relay 探测成功、历史补收完成分别表达。

**Headless 注册限制：**Hyphae 固定验收 SHA `1948aadc` 和其 binary 中，`profile publish` 没有 `--password-stdin`。后续 PR [#90](https://github.com/iDoris-ai/Hyphae/pull/90) 已合并，但这不改变旧 SHA/binary 的能力。Agent24 若需要加密身份非交互注册，应固定包含 #90 的新 Hyphae SHA/binary 并单独验收后再承诺；不能为绕过旧版缺口创建未加密生产身份。

## 分阶段交付建议

下列 Agent24 命令只是候选名，待该仓确认后再冻结；目前 Rust CLI 没有这些通信命令。

| 阶段 | 交付 | 通过条件 |
|---|---|---|
| T20-A | 固定 binary/config 与输出 envelope；身份、联系人、relay 管理。候选 `agent24 comm identity|contact|relay ...`。 | 实际子进程调用；公开 JSON 可解析；密码仅经 stdin；专用 HOME；配置可被后续命令读取；binary 缺失/版本不符有诊断。 |
| T20-B | 发信、history、outbox、daemon 生命周期。候选 `agent24 comm send|inbox|history|outbox ...`。 | 本地真实 relay；断线重试使用同一 event ID/签名；保留部分错误结果；同 HOME 重启后历史稳定且无重复新消息效果；daemon 可停止。 |
| T21 | 身份、联系人、relay 管理 UI。 | 复用 T20 通信服务、配置与状态；未连接时不伪装为健康。 |
| T22 | 消息、history、待发及重试 UI。 | 区分本地入队、relay 接受和对端确认；队列未知/并发冲突先核对，不创建新事件重发。 |

## 必须独立验收的边界

- 每个 T20/T21/T22 PR 固定 Agent24/Hyphae commit、binary 版本/hash、退出码和测试结果。Hyphae 验收用临时 HOME、合成身份、本地 relay；不得访问用户生产密钥、relay 数据或全局 `~/.hyphae`。
- T20-B 至少复验真实 CLI + relay 断线重试原 event ID，以及 125 条离线积压、daemon 重启后行数不变/新消息效果为零。Agent24 侧另外用 spy/counter 证明 plain/query/F4/receipt 入站不启动 run；Hyphae 收件去重不等于 Agent24 执行去重。
- 基础通信保持 zero-run：普通入站、旧 kind 30078 消息、query 和 response 不进入模型、模块或 run。T01-E 收口后再实施高层授权与持久化 request/run，然后进行四仓联调验收；不把这些功能混入 T20/T21/T22。
- iDoris 正式 provider/预算、Sin90 授权与 manifest 生命周期、AgentEar 语音各自独立推进，不属于本提案范围，也不由本提案记完成。

## 待 Agent24 确认

1. 确认或调整 T20-A → T20-B → T21 → T22 拆分及外层命令名。
2. 明确固定 Hyphae binary 的安装/更新责任、应用专用 HOME 布局和现有 `agent-speaker` bridge 的兼容迁移。
3. 确认部分成功 envelope、daemon 生命周期及 headless 注册限制的产品呈现。
4. 每个实现 PR 回填双方 SHA、集成测试、CLI/UI 验收结果；分别记录 T01-E、iDoris provider、Sin90 授权和四仓状态。
