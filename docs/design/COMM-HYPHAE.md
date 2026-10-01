# COMM-0：Hyphae 基础通信接入 Agent24

状态：设计稿，待评审冻结 · 2026-10-01 · 不含实现
依据：Hyphae 提案 [#601 `67ddbce`](https://github.com/iDoris-ai/Agent24/pull/601) `docs/design/HYPHAE-CLI-INTEGRATION.md`；已定决策见 §0。
基线：Agent24 `bf3322c`（含 DEP-A5 #602）；Hyphae `a4aa606eb81d5c040d94c51cdf94553e646d8674`，macOS arm64 二进制 sha256 `bc30dcf7bcf8b5c1865a3e995518c2bdab224bd8d6a3a064c4d7fc780de5e2b7`（`--version` 输出 `hyphae version dev`，不作校验依据）。
命名：本文统一称 **Hyphae**（原名 agent-speaker）。Hyphae 侧编号 T20-A / T20-B / T21 / T22，Agent24 侧编号 COMM-*，对应关系见 §8。

## 0. 已定决策（jason 2026-09-30，本文不再讨论）

| # | 决策 |
|---|---|
| D1 | 通信服务放在 agent24d 内：新模块 `comm`，路由 `/api/v1/comm/*`。`agent24 comm ...` 是这套 REST 的客户端，桌面 UI 也走它 |
| D2 | Hyphae daemon 由 agent24d 作为子进程监管，固定传 `--notify=false` |
| D3 | 使用专用 HOME `<state_dir>/comm/hyphae-home/`；旧 `~/.hyphae` 只能经 `agent24 comm import --from` 显式导入，用户确认后**复制** |
| D4 | keystore 口令存系统钥匙串，只经 `--password-stdin` 传递，不进 argv、日志或配置 |
| D5 | F4b 冻结：白名单入站触发 run 的功能默认关闭、不再扩展；COMM 基础通信 zero-run |
| D6 | Hyphae 版本不写死，用 lock 清单 `comm/hyphae.lock.json`；靠 sha256 校验，不靠 `--version` |
| D7 | 任务编号用 COMM-*，与 T20-A / T20-B / T21 / T22 对照映射 |
| D8 | 保留 `agent-speaker` / `A24_SPEAKER_BIN` 兼容，新增 `A24_HYPHAE_BIN` |

## 1. 范围与非目标

**范围**：身份、联系人、relay 管理；收发消息、本地历史、待发队列（outbox）；Hyphae daemon 的监管；旧 HOME 导入；CLI 与桌面 UI。

**非目标**：
- 高层授权执行，即入站消息触发 run、request/run 持久化、能力授权。这些属于 T01-E 收口之后的工作。
- 对端回执协议。T01 冻结前，消息状态的第三层「对端确认」只预留位置，不实现（见 §5.1）。
- `profile publish` 无头注册。Hyphae `6bd9e437` 已在基线之内，但它需要单独验收，不放进 COMM-1..7。
- 群聊（`group`）、TUI、`--auto-reply`。
- 扩展 nostr-bridge（F4/F4b）：只做冻结和命名兼容（COMM-5）。
- Windows（依赖 DEP-A7，见 §9）。

## 2. 架构

```
 agent24 comm …（CLI）   桌面 UI（T21/T22）
          └──────── HTTP + token ────────┘
                         ▼
 agent24d ─ /api/v1/comm/* ─ comm::router(CommState)   ← 状态类型不是 AppState
                         │
              ┌──────────┴────────────────────┐
              ▼                               ▼
   HyphaeRunner（单次命令）          HyphaeDaemon（长驻子进程）
   spawn → stdin 写口令后关闭 →      spawn hyphae daemon … --notify=false
   读 envelope → 退出                 --auto-reply=false --relay …；
              │                       用 RestartPolicy 退避；日志写入文件
              └──── 同一个 VerifiedBinary，同一个 HOME ─────┘
                         ▼
 <state_dir>/comm/                       （state_dir = $HOME/.agent24）
 ├─ hyphae-home/            ← 子进程的 HOME（0700）
 │   └─ .hyphae/            ← Hyphae 自己的目录：keystore.json(0600) relays.json messages.db
 ├─ config.json             ← comm 自有配置：active_identity、daemon.autostart（不含任何秘密）
 ├─ hyphae-daemon.pid       ← pid、pgid、generation、bin_sha256（用于清理孤儿进程）
 └─ logs/hyphae-daemon.log  ← 按大小轮转，5 MB × 3
 口令：macOS Keychain / Linux Secret Service，service="ai.idoris.agent24.comm"，account=<keystore 指纹>
```

依据：
- 实测 Hyphae 数据目录是 `os.UserHomeDir()/.hyphae`（`internal/identity/keystore.go:90`、`internal/relayconfig/config.go:30`），没有 `HYPHAE_HOME` 之类的变量。所以「专用 HOME」= 子进程的 `HOME=<state_dir>/comm/hyphae-home`，Hyphae 实际数据落在其下的 `.hyphae/`。
- comm 本身**不另存消息**。消息、outbox、去重都以 Hyphae 的 `messages.db` 为唯一来源，comm 运行期只经 CLI 读写，不直接打开这个 SQLite；唯一例外是 import 时用 `VACUUM INTO` 对源库取只读快照。
- comm 的核心逻辑放在新 crate `rust/crates/agent24-comm`，agent24d 只挂载路由（`src/comm_routes.rs`）。这样「comm 不依赖 run 路径」可以交给 Cargo 依赖图来强制（§7）。

## 3. HyphaeRunner 接口（已编译验证）

以下签名已在临时 crate 里通过 `cargo check --offline` 和 `cargo clippy --offline`，环境为 edition 2024，lint 与本仓 workspace 一致，并沿用本仓 `Cargo.lock`。`cargo test --offline` 用真实二进制和临时 HOME 跑通了 `identity list`、`identity create --password-stdin` 和 hash 不匹配被拒三个场景。验证完临时 crate 已删除。实现 PR 可以调整函数体，签名有变动须在 PR 中说明理由。

```rust
pub const PASSWORD_MAX: usize = 4096;            // 与 Hyphae maxPasswordStdinBytes 一致
pub const OUTPUT_CAP: usize = 4 * 1024 * 1024;   // stdout/stderr 各自上限，超出报 OutputTooLarge

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sha256Digest(pub [u8; 32]);
impl Sha256Digest { pub fn from_hex(s: &str) -> Result<Self, BinaryError>; }

/// 通过 hash 校验的二进制；唯一构造途径是 verify。
pub struct VerifiedBinary { /* path: PathBuf, sha256: Sha256Digest */ }
impl VerifiedBinary {
    pub async fn verify(path: &Path, expected: Sha256Digest) -> Result<Self, BinaryError>;
    pub fn path(&self) -> &Path;
    pub fn sha256(&self) -> Sha256Digest;
}
pub enum BinaryError { NotAbsolute(PathBuf), Missing(PathBuf),
    HashMismatch { expected: String, actual: String }, BadLockHash, NoLockForPlatform(String) }

/// 口令：drop 时清零（Zeroizing<Vec<u8>>），不实现 Debug；长度须在 1..=4096。
pub struct Password(/* Zeroizing<Vec<u8>> */);
impl Password { pub fn new(bytes: Vec<u8>) -> Result<Self, RunnerError>; }

/// env_clear 之后按白名单补回；HOME 一律强制设为 hyphae-home，不在白名单里。
pub const ENV_ALLOW: &[&str] = &["PATH","LANG","LC_ALL","TZ","TMPDIR","SSL_CERT_FILE","SSL_CERT_DIR"];

pub struct Invocation {
    pub args: Vec<OsString>,          // 不含 --json / --password-stdin，由 runner 追加
    pub password: Option<Password>,   // Some 时追加 --password-stdin，写入 stdin 后立即关闭
    pub timeout: Option<Duration>,
}

pub enum ExitClass { Ok=0, UserError=1, NetworkError=2, AuthError=3, OtherError=4, WriteConflict=5 }

pub enum Envelope {
    Ok { data: serde_json::Value },
    /// exit 1..=5 时取自 stderr；data 会保留（部分成功，如 send/retry）
    Failed { exit: ExitClass, error: String, message: String, data: Option<serde_json::Value> },
}
pub enum RunnerError { PasswordLength(usize), Spawn(io::Error), Timeout(Duration), Signaled,
    UnknownExit(i32), BadEnvelope { exit: i32, reason: &'static str }, OutputTooLarge, Io(io::Error) }

/// 纯函数：exit 0 时 stdout 必须是 {ok:true}；exit 1..=5 时 stderr 必须是 {ok:false,error,...}；
/// ok 字段与退出码矛盾、或不是单个 JSON 对象，都报 BadEnvelope。
pub fn parse_envelope(code: i32, stdout: &[u8], stderr: &[u8]) -> Result<Envelope, RunnerError>;

pub struct HyphaeRunner { /* bin: VerifiedBinary, home: PathBuf, default_timeout: Duration */ }
impl HyphaeRunner {
    pub fn new(bin: VerifiedBinary, home: PathBuf, default_timeout: Duration) -> Self;
    /// 绝对路径 + 参数数组；env_clear + ENV_ALLOW + HOME；无口令时 stdin 为 null；
    /// process_group(0)；kill_on_drop(true)
    pub fn command(&self, inv: &Invocation) -> tokio::process::Command;
    pub async fn run(&self, inv: Invocation) -> Result<Envelope, RunnerError>;
}
```

规则：
- **路径**：解析顺序为 `A24_HYPHAE_BIN` → `A24_SPEAKER_BIN`（兼容，使用时打 warn）→ 与 agent24d 同目录的 `hyphae`。不查 PATH。相对路径直接拒绝。
- **hash**：期望值来自编译期 `include_str!("comm/hyphae.lock.json")` 中当前平台那一项，运行时改不了。缺项报 `NoLockForPlatform`，不一致报 `HashMismatch`，comm 进入 `binary_rejected`，其他路由照常。没有开发用的跳过开关。
- **TOCTOU**：校验通过后把二进制复制到 `<state_dir>/comm/bin/hyphae-<sha256 前 16 位>`（0500），复制完再算一次 hash；之后只执行这份副本。
- **超时**：默认 15 s。`send` / `outbox retry` 为 `5 s × relay 数 + 10 s`。`relay info` 为 `--timeout` 加 2 s。超时后 drop 子进程（kill_on_drop），实现 PR 还要对进程组 `killpg`。注意：send 超时时事件**可能已经入队或已被 relay 接受**，所以 REST 返回 `timeout`，并提示按 outbox 和 history 核对，不自动重试（§5.3）。
- **口令**：只经 stdin 写入后关闭。Hyphae 去掉末尾一组 LF/CRLF，保留其余空格。runner 原样写入口令字节，不追加换行。
- **日志**：只记录 argv 中的子命令名和退出码。`--content` 的值、口令、stdout 正文都不进日志。

## 4. REST 路由

Agent24 现有鉴权（token）照常适用。响应格式与 Hyphae 一致：`{ok:true,data}`，或 `{ok:false,error,message,data?}`。下文写 Hyphae 命令时省略 runner 统一追加的 `--json`。⚿ 表示带 `--password-stdin`，口令从钥匙串取。

**错误闭集**（`error` 字段只会是下列值之一）：

| error | HTTP | 来源 |
|---|---|---|
| `binary_rejected` | 503 | BinaryError |
| `unsupported_platform` | 501 | 当前平台不在 COMM 支持范围（Windows，见 §9 R2） |
| `locked` | 423 | 拿不到口令：钥匙串不可用或无条目，或 Hyphae 返回 `auth_error` |
| `not_configured` | 409 | 没有身份；relay 来源不是 `config`（§9 R1） |
| `invalid` | 400 | 请求校验失败；Hyphae `user_error` |
| `not_found` | 404 | 身份、联系人或 outbox 条目不存在（Hyphae `user_error` 的子情形，由 comm 在调用前查证） |
| `confirm_required` | 400 | 破坏性或导入操作缺少 `confirm:true` |
| `conflict` | 409 | Hyphae `write_conflict`（退出码 5）；daemon 状态切换正在进行 |
| `network` | 502 | Hyphae `network_error`，且不带 data |
| `partial` | 502 | 任意非零退出但带 data（send/retry）：**原样透传 data** |
| `timeout` | 504 | RunnerError::Timeout |
| `upstream` | 502 | Hyphae `other_error` 且不带 data；BadEnvelope；Signaled；UnknownExit；OutputTooLarge |

| 方法 + 路径 | 请求 | data | Hyphae 命令 |
|---|---|---|---|
| `GET /comm/identity` | — | `[{nickname,npub,default,encrypted}]` | `identity list` |
| `POST /comm/identity` | `{nickname, default?}` | 新身份 | `identity create --nickname N [--default]` ⚿；首个身份创建时 comm 生成 32 字节随机口令，先写入钥匙串再调用 |
| `POST /comm/identity/default` | `{nickname}` | 同上 | `identity use --nickname N`；daemon 在运行时随后重启 |
| `GET /comm/contact` | — | `[{nickname,npub,role}]` | `contact list` |
| `POST /comm/contact` | `{nickname,npub,role?}` | 新联系人 | `contact add --nickname --npub [--role]`（npub 先在 comm 侧校验，见 §9 G6） |
| `GET /comm/relay` | — | `{relays,source,configured}`，`configured = source=="config"` | `relay list` |
| `PUT /comm/relay` | `{relays:[ws/wss…]}`，1..=8 个 | 同上 | 多个 `relay set --relay U…`（整体替换）；daemon 在运行时重启 |
| `POST /comm/relay/probe` | `{url?}` | `{url,connected}`，并写入 daemon 状态的 `relay_probe` | `relay info [U] --timeout 5` |
| `POST /comm/send` | `{to, content, from?, encrypt=true}` | 见 §5.1 | `agent msg --from F --to T --content C [--encrypt=false]` ⚿ |
| `GET /comm/inbox?as=&limit=` | limit 1..=200 | `{messages, complete:false}` | `agent inbox --as A --limit N --decrypt` ⚿（有上限的单次 relay 查询，不代表补收完成） |
| `GET /comm/history?as=&limit=` | — | 收件历史 | `history inbox --as A --limit N` |
| `GET /comm/history?with=&limit=` | — | 与某联系人的会话 | `history conversation --with W --limit N` |
| `GET /comm/outbox?failed_only=` | — | `[{id,status,retry_count,max_retries,relays,stuck,…}]` | `storage outbox list [--failed-only]` |
| `POST /comm/outbox/{event_id}/retry` | — | `{event_id,attempted,sent,queued,…}` | `storage outbox retry --id E`：沿用原签名事件，**不生成新事件** |
| `POST /comm/outbox/clear` | `{confirm:true, min_failures?}` | 清理的条目 | `storage outbox clear --failed --yes [--min-failures]`：只删本地待发记录，不撤回 relay 已接受的事件 |
| `GET /comm/daemon` | — | §5.2 三项状态 | 无（由监管器提供） |
| `POST /comm/daemon/start` | — | 同上 | `daemon …`（§6） |
| `POST /comm/daemon/stop` | — | 同上 | 对进程组发 SIGTERM |
| `POST /comm/import` | `{from, confirm:true, password?, dry_run?}` | `{identities, contacts, messages}` | 不调 Hyphae 写命令；复制完成后在临时 HOME 里跑 `identity list` 验证 |
| `POST /comm/unlock` | `{password}` | `{unlocked:true}` | 仅当钥匙串不可用（§6.4 L2）；口令只留在内存 |

路由补充规则：
- `from` / `as` 缺省时取 `config.json.active_identity`。身份未加密的 keystore（`encrypted:false`）一律视为 `not_configured`：Agent24 不使用明文 keystore。
- **import**：`from` 必须是一个存在的 `.hyphae` 目录（例如 `~/.hyphae`），目标 HOME 必须为空，否则返回 `conflict`。流程如下：
  1. 先复制到 `hyphae-home.staging/`：`keystore.json`、`relays.json`；`messages.db` 用 SQLite `VACUUM INTO` 取一致快照，以免复制到 WAL 写了一半的状态。
  2. 用 runner 在 staging HOME 里跑 `identity list`，要求全部 `encrypted:true`。
  3. 如果提供了口令，用 staging 起一次 daemon 来验证口令（`auth_error` 会在 1 s 内以退出码 3 结束），验证通过才写入钥匙串。
  4. rename 成正式目录。
  5. 源目录只读、不修改。`dry_run` 只报告会导入的数量。
  6. CLI `agent24 comm import --from` 需要交互确认，或显式加 `--yes`。
- CLI 子命令与 REST 一一对应：`agent24 comm identity list|create|use`、`contact list|add`、`relay list|set|probe`、`send`、`inbox`、`history`、`outbox list|retry|clear`、`daemon status|start|stop`、`import`、`unlock`。风格沿用 `agent24 os` 的 clap 子命令写法。CLI 加 `--json` 时原样输出 REST 的 envelope。

## 5. 状态模型

### 5.1 消息四层

| 层 | 含义 | 依据（Hyphae 字段） | 基础通信 |
|---|---|---|---|
| L1 本地待发 | 已签名，已写入 history 或 outbox | `history_stored` 或 `queued_for_retry`，且 `published_to==0`；outbox `status=pending` | 有 |
| L2 relay 已接受 | ≥1 个 relay 返回 OK | `published_to>0`；或 outbox 条目变为 `sent` | 有 |
| L3 对端确认 | 对端发来回执 | **基线里没有回执字段**（`StoredMessage` 不带 ack），依赖 T01 回执契约 | 预留；UI 一律显示「未确认」，不显示「已送达」 |
| L4 执行结果 | 对端执行完成 | T01-E | 不在本范围 |

- `send` 返回 `ok:true` **不等于** relay 已接受。实测 relay 全部不可达时，Hyphae 返回退出码 0、`published_to:0`、`queued_for_retry:true`。comm 以 `published_to` 决定层级，UI 文案为「已入队，等待 relay」。
- `connected=true`（relay info）只说明一次 WebSocket 握手成功，不代表在线、已订阅或已送达。

### 5.2 daemon 三项状态（`GET /comm/daemon`）

```jsonc
{ "process":   {"state":"stopped|starting|running|backoff|gave_up|locked", "pid":..,"generation":..,
                "consecutive_failures":..,"reason":"no_identity|no_relay|keychain_unavailable|password_rejected|binary_rejected"},
  "relay_probe": {"url":"..","connected":true,"at_ms":..,"error":null},   // 运行期间每 60 s 跑一次 relay info，也可手动触发
  "catch_up":  {"state":"unknown|incomplete","last_incomplete_at_ms":..} }
```

- `process` 由监管器维护（§6），是唯一可靠的一项。
- `catch_up`：基线 daemon **没有结构化进度**，只会在日志里打印 `⚠️ Inbox scan incomplete: …`。COMM 只能匹配这一行得到 `incomplete`，其余时候一律显示 `unknown`，**永不显示「已补收完成」**，直到 Hyphae 提供结构化信号（§9 G2）。
- 三项分开呈现：进程活着不代表 relay 通，relay 通也不代表补收完成。

### 5.3 部分成功的呈现

| 情形（send/retry 的 data） | UI 文案 | 允许的操作 |
|---|---|---|
| `published_to>0` 且（`!history_stored` 或 `queue_state_unknown` 或带 `audit_error`） | **「已发出，本地记账异常」**，并显示 event_id | 「按 event_id 核对」：查 outbox 和会话历史 |
| `published_to==0` 且 `queued_for_retry` | 「已入队，等待 relay」 | outbox 重试（原 event_id） |
| `published_to==0` 且 `queue_state_unknown` | 「状态未知」 | 先重读 outbox，再决定是否重试原 event_id |
| `superseded` | 「已被同 ID 的更新条目取代」 | 重读 outbox |
| `timeout`（没有 data） | 「结果未知，可能已发出」 | 按会话历史和 outbox 核对 |

所有情形都**不提供「重发」**，即不提供用新 event_id 再发一次的入口。唯一的重试是 `outbox/{event_id}/retry`。CLI 和 UI 对 `POST /comm/send` 都不做自动重试。

## 6. Daemon 监管

### 6.1 启动

前置条件按顺序检查，任一不满足就进入 `locked`，带上 reason，不进入退避：
1. 二进制已通过校验；
2. `active_identity` 存在且 `encrypted`；
3. `relay list` 的 `source=="config"` 且不为空；
4. 能拿到口令。

启动命令：

```
hyphae daemon --identity <id> --password-stdin --notify=false --auto-reply=false \
  --relay <r1> [--relay <r2>…] --watch-interval 30 --retry-interval 60 --json
```

- `--auto-reply=false` 也显式传：防御 Hyphae 默认值变化。
- relay 显式传入：不依赖 Hyphae 在未配置时回落到默认 relay。

就绪判定：Hyphae 没有就绪信号。进程存活满 3 s 记为 `running`。在此之前退出的情况：
- stderr 有 `auth_error`：记为 `locked{password_rejected}`，不重试；
- 有 `user_error`：记为 `gave_up`，不重试；
- 其他情况：进入退避。

单实例：
- 每个 state_dir 只有一个 agent24d（`try_acquire_singleton`），comm 内部用 Mutex 保证只有一个 daemon generation。
- 基线 Hyphae 本身**没有**同一 HOME 下的 daemon 互斥。专用 HOME 不对外公开，这是本设计依赖的前提。

孤儿清理：agent24d 启动时读 `hyphae-daemon.pid`。如果该 pgid 仍然存活，且其可执行文件就是 `comm/bin/hyphae-<sha>`，先对它 `killpg`（先 TERM 再 KILL），然后才启动新的 daemon。

### 6.2 停止与 agent24d 关机
- `stop`：对进程组发 SIGTERM。Hyphae 用 `signal.NotifyContext` 处理 SIGTERM，实测退出码 0。等待宽限期（复用 `A24_MODULE_STOP_GRACE_MS`，默认 500 ms，下限 100 ms）后发 SIGKILL，再按 `REAP_TIMEOUT` 回收。
- agent24d 关机：comm 的停止并入 SHUT-1b 的模块停止阶段，结果记入 `last-shutdown.json`，在 `records[]` 里加一项，`name` 为 `"comm.hyphae"`。
- agent24d 崩溃：由下次启动时的孤儿清理兜底。
- `daemon.autostart`：用户第一次手动 `start` 成功后置为 true，此后 agent24d 启动时自动拉起 daemon；手动 `stop` 后置回 false。

### 6.3 配置变更与崩溃退避
- 修改 relay 或默认身份后，如果 daemon 在运行：stop 再 start。这次重启**不计入**失败次数。Hyphae 旧 outbox 条目保留各自的 relay，不受新配置影响。
- 崩溃退避直接复用 `agent24_os_proto::supervise::RestartPolicy`：这是纯状态机，500 ms 起步、每次翻倍，连续 5 次熔断进入 `gave_up`，正常运行满 60 s 清零计数。熔断后只有 `POST /comm/daemon/start` 能恢复。
- 日志：stdout 和 stderr 合并写入 `logs/hyphae-daemon.log`。口令只走 stdin 且已关闭，argv 里没有秘密。

### 6.4 口令存放与降级
- **L0 正常**：macOS 用 Keychain（Security.framework），Linux 用 Secret Service（D-Bus）。实现选 `keyring` crate，这是新增依赖，需在 COMM-1 评审。**不得调用 `security` 命令行**，因为 `-w` 参数会让口令出现在 argv 里。
- **L1 systemd 凭据**（Linux 无头场景，需显式启用）：读取 `$CREDENTIALS_DIRECTORY/agent24-comm-password`，即 systemd `LoadCredential=`。文件须属于当前 uid，权限 ≤0400。
- **L2 仅内存**：钥匙串不可用时，由用户经 `agent24 comm unlock`（从 TTY 或 stdin 读取）把口令交给 agent24d，只保存在 `Zeroizing` 内存里。agent24d 重启后回到 `locked`。
- **不提供**：明文口令文件，以及把口令放进环境变量或 `config.json`。
- 降级不会自动发生：L0 失败时状态为 `locked{keychain_unavailable}`，由 UI 或 CLI 提示改用 L1 或 L2。

### 6.5 与 Deployment A5 的关系
- A5 已合并（#602）：桌面端会复用已在运行的 agent24d，托盘只停止自己拉起的那一个。因此**持有 Hyphae daemon 的始终是那个唯一的 agent24d**。
- 桌面端不直接 spawn Hyphae，也不读 `hyphae-home`，只调 `/api/v1/comm/*`。
- 托盘停止外部 agent24d 时不会碰它，Hyphae 也照常运行；停止桌面端自己拉起的 agent24d 时，Hyphae 随之按 §6.2 停止。
- 打包（A3/A6）：后续发布包会在 `agent24`、`agent24d` 旁边放 `hyphae`，hash 记入 lock 清单，这需要在 A3 的打包布局里另行加一项（§9 R4）。

## 7. Zero-run 判据与测试

**判据**：comm 处理入站消息时，runs 表新增数为 0，run、模型、模块调用次数也为 0。覆盖以下六类入站：

| 类别 | 测试样本来源 |
|---|---|
| plain | 任意纯文本 |
| 旧 kind 30078 | F4 `f4/1` envelope，`intent:"ask"`。注意 Hyphae 自己发的普通消息也是 kind 30078（`AgentKind=30078`），所以**不能按 kind 分流**，必须靠结构保证 |
| query | Hyphae `tests/contracts/testdata/public-query-fixtures.json` 中 `type=query` 的样例 |
| response | 同一文件中 `type=response` 的样例 |
| receipt | F4 `intent:"ack"` 加 `status:"working"`（T01 尚无回执候选，先用这个作代表） |
| canary | nostr-bridge FU-32 自发的 canary 正文 |

**结构约束（编译期 + 测试）**：
1. `agent24-comm` 的 `[dependencies]` 不得包含 `agent24-agent`、`agent24-models`、`agent24-store`、`agent24-scheduler`、`agent24-tools`、`agent24-mcp`；允许 `agent24-os-proto`，只用于 `RestartPolicy`。测试 `comm_deps_exclude_run_path` 解析 `Cargo.toml` 的 `[dependencies]` 表做断言（`toml` 目前不在 Cargo.lock 中，作为 dev-dependency 引入，或用 `cargo metadata` 的输出）。**正对照**：同一个检查函数作用于 `apps/agent24d/Cargo.toml` 时必须命中 `agent24-agent`，否则视为检查本身失效。
2. `comm::router(CommState) -> Router`：`CommState` 只包含 `Arc<HyphaeRunner>` 和监管器句柄，不包含 `Store`、`ModelRouter`、`ToolRegistry`、`AppState`。这一点由类型系统保证（已在 §3 的验证 crate 中用 axum 0.8 编译通过）。agent24d 在 `server.rs` 里用 `merge` 挂载。
3. comm 没有入站推送或订阅回调：入站只能经 `GET history/inbox` 被动读取，读取结果原样返回，不解析 envelope、不分派。

**行为测试**（COMM-5，`apps/agent24d/tests/comm_zero_run.rs`）：
- 量具：
  - 测试用 `AppState` 注入一个计数的 mock 模型 provider，记录 `model_calls`；
  - `SELECT COUNT(*) FROM runs`；
  - `module_status` 代理调用计数。
- 用假的 Hyphae（测试脚本，其 hash 交给 `VerifiedBinary::verify`，不经过 lock）在 `history inbox`、`agent inbox`、`history conversation` 中返回上述六类样本。假 daemon 每 100 ms 打印一行日志。
- 步骤：启动 daemon，轮询这三个读接口各 20 次，再 stop。
- 断言：`runs Δ == 0`，`model_calls == 0`，模块调用 `== 0`。
- **正对照**：同一测试进程、同一套量具，用 plain 样本走 F4b 实际使用的 HTTP 路径（`POST /api/v1/sessions` 加 `POST /api/v1/runs`，即 nostr-bridge `Agent24Client.runToCompletion` 发出的请求），断言 `runs Δ == 1` 且 `model_calls ≥ 1`。若正对照为 0，整个测试判失败。

**F4b 冻结**（COMM-5，TS）：
- 现状：只要配置了 `A24_NOSTR_ALLOWED_NPUBS` 就会启动 inbound 循环，**并非默认关闭**。
- 改为：必须同时设置 `A24_NOSTR_F4B_INBOUND=1` 才启动，否则打印一次「F4b 已冻结」后跳过 inbound 循环。
- 测试：`inbound.test.ts` 用 spy 替换 `runToCompletion`。默认配置下发送已授权的消息，调用次数为 0；设置 flag 后为 1，作为正对照。
- 单一消费者：COMM 不消费入站事件。F4b 开启时它是唯一的执行消费者，所以同一个 `event_id` 不会被两条路径各执行一次。

## 8. 任务表

规模：S ≤ 300 行，M ≤ 800 行，L 为更大。每个 PR 都要在描述里写明「COMM-n ↔ Hyphae Tnn」，并回填 §8.1。

| 编号 | Hyphae | 交付 | 依赖 | 规模 | 可证伪验收 |
|---|---|---|---|---|---|
| COMM-1 | T20-A | `agent24-comm` crate：lock 清单解析、`VerifiedBinary`、`HyphaeRunner`、`parse_envelope`、`PasswordStore` trait（keyring / systemd / memory）、HOME 布局（0700）、二进制副本 | COMM-0 冻结 | M | 改动二进制 1 个字节，`verify` 必报 HashMismatch；用相对路径必报 NotAbsolute；退出码 0–5 加 7、ok 字段与退出码矛盾、两段 JSON，各有 1 个表驱动用例；部分成功的 data 能取到 event_id；子进程环境里没有父进程的哨兵变量 `A24_SENTINEL`；只读 stdin 的回显脚本收到的字节数与口令一致，且 argv 中不含口令；4097 字节口令被拒 |
| COMM-2 | T20-A | 路由与 CLI：identity、contact、relay、import；首个身份生成口令并写入钥匙串；错误闭集 | COMM-1 | M | 真实二进制 + 临时 HOME：create→list→use→contact add→relay set→list，完整走通；`relay list` 在 `source=default` 时 send 返回 `not_configured`；import 不带 confirm 返回 `confirm_required`；import 后源目录的 mtime 和 hash 不变；明文 keystore 导入被拒；**全程不碰真实的 `~/.hyphae`**（测试中设 `HOME=` 临时目录，并断言该路径从未被打开） |
| COMM-3 | T20-B | send、inbox、history、outbox（list/retry/clear）路由与 CLI；§5.1 分层与 §5.3 呈现字段 | COMM-2 | M | relay 不可达时 send 返回 ok，层级为 L1，且 `published_to==0`；对 retry 返回的 event_id 与原 id 断言相等；用注入的 data（`published_to:1,history_stored:false`）得到 `partial`，文案为「已发出，本地记账异常」，接口列表中不存在重发入口；clear 不带 confirm 被拒 |
| COMM-4 | T20-B | `HyphaeDaemon` 监管：启动前置条件、就绪判定、RestartPolicy、配置变更重启、孤儿清理、SHUT-1b 接入、三项状态、relay probe、日志轮转 | COMM-1, COMM-2 | M | 用 kill -9 杀掉 Hyphae，按 500→1000 ms 重启，第 5 次熔断进入 gave_up；口令错误时进入 `locked{password_rejected}`，且不重试；修改 relay 后 generation 加 1，失败计数不变；agent24d 被 SIGKILL 后重启，旧的 pgid 被清理，`pgrep -f hyphae-<sha>` 结果为 1；正常关机后 `last-shutdown.json` 的 `records[]` 中有 `comm.hyphae` |
| COMM-5 | T20-B | §7 zero-run 测试与依赖约束；F4b 默认关闭（`A24_NOSTR_F4B_INBOUND`）；nostr-bridge 读取 `A24_HYPHAE_BIN`，优先于 `A24_SPEAKER_BIN` | COMM-3, COMM-4 | S | 六类入站时 runs Δ=0、model_calls=0；正对照 Δ=1。依赖检查对 agent24d 的 `Cargo.toml` 命中。TS：默认 spy=0，开 flag 后 spy=1。只设 `A24_SPEAKER_BIN` 时 bridge 行为不变 |
| COMM-6 | T21 | 桌面 UI：身份、联系人、relay 页面，daemon 三项状态面板，锁定、导入向导 | COMM-2, COMM-4 | M | relay 未配置或 daemon 为 `locked`、`gave_up` 时，面板不显示绿色；`catch_up` 不会出现「完成」；UI 不直接 spawn 进程，也不读 hyphae-home（打包后用 `lsof`/`fs_usage` 抽查） |
| COMM-7 | T22 | 桌面 UI：会话、历史、outbox、重试；§5.3 文案；**双仓联调验收**（§8.2） | COMM-3, COMM-6 | M | 状态文案逐条对应 §5.1 和 §5.3；没有「已送达」字样；重试按钮只出现在 outbox 条目上；§8.2 全部步骤通过 |

依赖链：COMM-1 → COMM-2 → {COMM-3, COMM-4} → COMM-5；COMM-6 依赖 COMM-2 和 COMM-4；COMM-7 依赖 COMM-3 和 COMM-6。COMM-1 与 Hyphae T20-A 收尾可以并行。

### 8.1 lock 清单与回填

```jsonc
// comm/hyphae.lock.json （仓库根下；agent24d 以 include_str! 内嵌）
{ "schema": 1,
  "hyphae": { "repo": "iDoris-ai/Hyphae", "sha": "a4aa606eb81d5c040d94c51cdf94553e646d8674" },
  "binaries": {
    "darwin-arm64": { "sha256": "bc30dcf7bcf8b5c1865a3e995518c2bdab224bd8d6a3a064c4d7fc780de5e2b7", "go": "1.27.1" },
    "darwin-x64": null, "linux-x64": null, "linux-arm64": null },
  "verified": [ { "agent24_sha": "<回填>", "pr": "#<回填>", "platform": "darwin-arm64", "date": "<回填>", "result": "<回填>" } ] }
```

- 每个 COMM PR 在 PR 描述的「锁」一栏写明：Agent24 head SHA、Hyphae SHA、用到的各平台 sha256、Go 版本，以及测试和联调结果。
- 如果 PR 改变了 Hyphae 的期望版本，必须同时修改 lock 文件并说明原因。
- `null` 表示该平台尚未验证：comm 在该平台报 `binary_rejected{NoLockForPlatform}`。
- Hyphae 侧在 T20-A/B 的 PR 中用同一格式回填 Agent24 SHA，实现双向标注。

### 8.2 联调验收步骤（COMM-7 交付，COMM-3/4 先跑一部分）

环境：
- 两套临时 HOME：A 由 agent24d 管理；B 是裸 Hyphae CLI。
- 一个本地 relay：任选符合 NIP-01 的实现（例如 Hyphae T18 工具或 khatru 样例），监听 `ws://127.0.0.1:<port>`，记录实现名称和版本。
- 全程不碰真实的 `~/.hyphae`、`~/.agent24`。

步骤：
1. **校验**：`shasum -a 256` 与 lock 一致；A 侧 `GET /comm/daemon` 不是 `binary_rejected`。
2. **配置**：A 用 `agent24 comm identity create`，B 用 `hyphae identity create --password-stdin`；双方互加联系人；双方 `relay set` 到本地 relay；A 执行 `daemon start`，B 执行 `hyphae daemon --notify=false`。
3. **双向**：A→B，B 的 `history inbox` 收到，且 event_id 一致；B→A，A 的 `GET /comm/history` 收到；A 侧层级为 L2，界面没有「已送达」。
4. **断线重试**：停掉 relay。A 发送，得到 `published_to==0`、`queued_for_retry`，记下 event_id E。恢复 relay，等待一次 retry 周期，或执行 `outbox/E/retry`。B 的历史里 **id 为 E 的消息恰好 1 条**，且没有其他新 id；A 的 outbox 中 E 变为 `sent`。
5. **重启无重复**：kill -TERM agent24d 后重启。A 的 history 条数不变；B 的入站条数不变。
6. **积压**：停掉 A 的 daemon，B 连发 125 条，启动 A，A 收件数加 125；再重启一次，加 0。
7. **zero-run**：第 2 步前和第 6 步后，各取一次 runs 计数（sqlite）和 `GET /api/v1/usage`，两者相等。另外跑一次 COMM-5 的正对照，确认量具有效。
8. **口令不外泄**：联调期间用 `ps -axo args` 采样，结果中不含口令；`grep -r <口令>` 扫 `<state_dir>`（含日志）和 config，均为 0 命中。
9. **记录**：两边 SHA、二进制 hash、relay 实现与版本、操作系统、每一步的结果，写入 PR 描述和 lock 清单的 `verified[]`。

## 9. 风险与待定项

| # | 项 | 决定或处理 |
|---|---|---|
| R1 | **relay 默认值**：官方 relay 会随体系上线，暂时留空。但 Hyphae 在没有配置时会**自动回落**到 `wss://relay.aastar.io`（实测 `relay list` 返回 `source:"default"`；daemon 和 send 都会用它） | comm 把 `source!="config"` 视为未配置：send、daemon 返回 `not_configured`；daemon 启动时总是显式传 `--relay`。官方 relay 上线后作为 Agent24 侧的默认值写入，不依赖 Hyphae 的回落 |
| R2 | Windows 监管 | Hyphae 在 Windows 上取 `USERPROFILE` 而非 `HOME`；`process_group`、`killpg` 只在 Unix 可用。COMM 在 Windows 报 `unsupported_platform`，等 DEP-A7 冻结、C2 实现后另开 COMM-W |
| R3 | macOS 钥匙串 ACL 与代码签名绑定：未签名的 agent24d 升级后可能重新弹出授权 | 在 DEP-B3 签名之前接受这一点；UI 在 `locked{keychain_unavailable}` 时给出说明 |
| R4 | 打包：A3 的发布包只含 `agent24` 和 `agent24d` | 加入 `hyphae` 的工作另起 DEP 任务（需改动 A3 的布局断言）；在此之前用 `A24_HYPHAE_BIN` 指定 |
| R5 | send 超时或进程被杀时，事件是否已发出无法确定 | 返回 `timeout`；按 §5.3 核对；不自动重试 |
| G1 | Hyphae 没有只读的口令验证命令；`identity change-password` 在 JSON 模式下不接受 stdin（实测退出码 3） | 暂时用 daemon 启动来验证口令（§4 import）。请 Hyphae 提供 `keystore verify --password-stdin`，以及支持 stdin 的改口令命令 |
| G2 | daemon 没有结构化的健康和补收进度 | 暂时显示 `catch_up: unknown`，靠日志匹配得到 `incomplete`。请 Hyphae 提供 JSON-lines 状态或状态文件 |
| G3 | Hyphae 没有同一 HOME 下的 daemon 互斥 | 依赖 agent24d 单例和专用 HOME。请 Hyphae 在 daemon 上加 flock |
| G4 | history 不带对端回执 | L3 等 T01 回执契约冻结后再做 |
| G5 | `--version` 输出 `dev` | 以 sha256 为准（D6）。建议 Hyphae 的 release 用 ldflags 注入版本 |
| G6 | 非法 npub 在 Hyphae 里归为 `other_error`（退出码 4），不是 `user_error`（实测） | comm 在调用前先校验 npub（bech32），返回 `invalid`，不依赖 Hyphae 的分类 |
