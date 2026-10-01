# COMM-0：Hyphae 基础通信接入 Agent24

状态：**r2 冻结**（1 轮 Opus 对抗评审 CHANGES 5 High 已处置，见 §10）· 2026-10-01 · 不含实现
依据：Hyphae 提案 [#601 `67ddbce`](https://github.com/iDoris-ai/Agent24/pull/601) `docs/design/HYPHAE-CLI-INTEGRATION.md`；已定决策见 §0。
基线：Agent24 `bf3322c`（含 DEP-A5 #602）；Hyphae `a4aa606eb81d5c040d94c51cdf94553e646d8674`。提案里的验收二进制（sha256 `bc30dcf7…5e2b7`，Go 1.27.1）**没有固定构建配方**，因此不进 lock；lock 的 hash 改由 §8.1 的配方复现。`--version` 输出 `hyphae version dev`，不作校验依据。
命名：本文统一称 **Hyphae**（原名 agent-speaker）。Hyphae 侧编号 T20-A / T20-B / T21 / T22，Agent24 侧编号 COMM-*，对应关系见 §8。

## 0. 已定决策（jason 2026-09-30，本文不再讨论）

| # | 决策 |
|---|---|
| D1 | 通信服务放在 agent24d 内：新模块 `comm`，路由 `/api/v1/comm/*`。`agent24 comm ...` 是这套 REST 的客户端，桌面 UI 也走它 |
| D2 | Hyphae daemon 由 agent24d 作为子进程监管，固定传 `--notify=false` |
| D3 | 使用专用 HOME `<state_dir>/comm/hyphae-home/`；旧 `~/.hyphae` 只能经 `agent24 comm import --from` 显式导入，用户确认后**复制** |
| D4 | keystore 口令存系统钥匙串，只经 `--password-stdin` 传递，不进 argv、日志或配置 |
| D5 | F4b 冻结：白名单入站触发 run 的功能默认关闭、不再扩展；COMM 基础通信 zero-run |
| D6 | Hyphae 版本不写死，用 lock 清单记录；靠 sha256 校验，不靠 `--version` |
| D7 | 任务编号用 COMM-*，与 T20-A / T20-B / T21 / T22 对照映射 |
| D8 | 保留 `agent-speaker` / `A24_SPEAKER_BIN` 兼容，新增 `A24_HYPHAE_BIN` |

## 1. 范围与非目标

**范围**：身份、联系人、relay 管理；发送消息、本地历史、待发队列（outbox）；Hyphae daemon 的监管；旧 HOME 导入；CLI 与桌面 UI；F4b 冻结。

**非目标**：
- 高层授权执行，即入站消息触发 run、request/run 持久化、能力授权。这些属于 T01-E 收口之后的工作。
- 对端回执。T01 冻结前，消息状态的 L3「对端确认」只预留位置（§5.1）。
- `profile publish` 无头注册（`6bd9e437` 已在基线内，但需要单独验收）。
- 群聊、TUI、`--auto-reply`。
- relay 单次查询（`agent inbox`）、按联系人查会话（G7）。

**原型阶段砍掉**（M12）：systemd 凭据、周期性 relay 探测、日志轮转。
**Windows**：依赖 DEP-A7，见 §9。

## 2. 架构

```
 agent24 comm …（CLI）   桌面 UI（T21/T22）
          └──────── HTTP + token ────────┘
                         ▼
 agent24d（bin） ── merge(comm::router(CommState))      ← 状态类型不是 AppState
                         │   crate: rust/crates/agent24-comm（lib，可单测）
              ┌──────────┴────────────────────┐
              ▼                               ▼
   HyphaeRunner（单次命令）          HyphaeDaemon（长驻子进程）
   读命令：直接执行                   hyphae daemon --notify=false --auto-reply=false --relay …
   改 keystore：持 KeystoreWriteLock  监管策略 RestartPolicy；日志在启动时截断
              └──── 同一个 VerifiedBinary（副本）、同一个 HOME、cwd 为 HOME ────┘
                         ▼
 <state_dir>/comm/                         （state_dir = $HOME/.agent24）
 ├─ bin/hyphae-<sha16>       ← 校验通过后以 O_EXCL 写出的副本（0500）
 ├─ hyphae-home/             ← 子进程的 HOME 与 cwd（0700）
 │   └─ .hyphae/
 │       ├─ keystore.json    ← 身份与联系人（0600，可加密），由 Hyphae 读-改-rename，无锁（G8）
 │       ├─ relays.json
 │       ├─ outbox.json      ← 待发队列（H2：不在 messages.db 中）
 │       ├─ outbox.json.lock ← Hyphae 每次操作 outbox 时 flock，操作完即释放
 │       └─ messages.db（含 -wal、-shm） ← 历史、入站去重、审计日志
 ├─ config.json              ← 只记 daemon.autostart；默认身份不在这里存（M5）
 ├─ hyphae-daemon.pid        ← {pid, pgid, start_time, generation, bin_sha256}
 └─ logs/hyphae-daemon.log   ← 0600；每次启动 daemon 时超过 5 MB 就截断
 钥匙串：service="ai.idoris.agent24.comm"，account = hex(sha256(keystore.salt))
```

依据：
- Hyphae 数据目录是 `os.UserHomeDir()/.hyphae`（`internal/identity/keystore.go:90`），没有 `HYPHAE_HOME` 这类变量，所以只能通过覆盖 `HOME` 指定专用目录。
- outbox 路径见 `internal/messaging/outbox.go:26`，`.lock` 见 `outbox_lock_unix.go:14`。
- comm **不另存消息**：Hyphae 的文件是唯一来源。运行期 comm 只经 CLI 读写，不直接打开这些文件；import 是唯一例外，只做文件级复制。
- 默认身份只有一个来源：`identity list` 中 `default:true` 的那一项。

## 3. HyphaeRunner 接口（r2 已编译验证）

以下签名已在临时 crate 中通过三项检查，环境与本仓一致（edition 2024，workspace lint，本仓 `Cargo.lock`，依赖 axum 0.8 / tower 0.5 / base64 0.22），验证完临时 crate 已删除：
- `cargo check --offline`；
- `cargo clippy --offline --all-features --all-targets`：0 告警；
- `cargo test --offline --features test-lock-override`：用假二进制，经 `tower::ServiceExt::oneshot` 调用 router，断言子进程拿到的 HOME。

```rust
pub struct Sha256Digest(pub [u8; 32]);
impl Sha256Digest { pub fn from_hex(s: &str) -> Result<Self, BinaryError>; pub fn short(&self) -> String; }

/// crate 内 `hyphae.lock.json`，用 include_str! 编进二进制
#[derive(serde::Deserialize)]
pub struct HyphaeLock { pub schema: u32, pub source_sha: String, pub go: String, pub recipe: String,
                        pub binaries: BTreeMap<String, Option<String>> }
impl HyphaeLock {
    pub fn embedded() -> Result<Self, BinaryError>;
    pub fn expected_for(&self, platform: &str) -> Result<Sha256Digest, BinaryError>;
    #[cfg(feature = "test-lock-override")]   // 只在测试 feature 下编译；生产无法覆盖
    pub fn override_for_test(platform: &str, sha: Sha256Digest) -> Self;
}

pub struct VerifiedBinary { /* path（副本）, sha256 */ }
impl VerifiedBinary {
    /// 把 source 一次性读入内存，对这份字节算 hash；一致才用 O_EXCL 写到
    /// install_dir/hyphae-<sha16>（0500）。副本已存在时重新算 hash，一致才复用。
    pub async fn install(source: &Path, expected: Sha256Digest, install_dir: &Path) -> Result<Self, BinaryError>;
    pub fn path(&self) -> &Path;  pub fn sha256(&self) -> Sha256Digest;
}
pub enum BinaryError { NotAbsolute(PathBuf), Missing(PathBuf), HashMismatch { expected: String, actual: String },
                       BadLockHash, NoLockForPlatform(String), Io(io::Error) }

pub struct Password(/* Zeroizing<Vec<u8>> */);       // 长度 1..=4096，不实现 Debug
impl Password { pub fn new(bytes: Vec<u8>) -> Result<Self, RunnerError>;
                pub fn generate(random32: [u8; 32]) -> Self; }   // base64url、无填充，43 字节文本

pub enum Account { Pending(String), Salt(String) }   // Salt = hex(sha256(keystore.salt))
impl Account { pub fn from_salt(salt_b64: &str) -> Self; }
#[async_trait] pub trait PasswordStore: Send + Sync {
    async fn get(&self, a: &Account) -> Result<Password, StoreError>;
    async fn put(&self, a: &Account, pw: &Password) -> Result<(), StoreError>;    // 已有条目时替换
    async fn rename(&self, from: &Account, to: &Account) -> Result<(), StoreError>;
    async fn delete(&self, a: &Account) -> Result<(), StoreError>;
}
pub enum StoreError { Unavailable(String), NotFound }

pub const ENV_ALLOW: &[&str] = &["PATH","LANG","LC_ALL","TZ","TMPDIR","SSL_CERT_FILE","SSL_CERT_DIR",
    "HTTPS_PROXY","https_proxy","ALL_PROXY","all_proxy","NO_PROXY","no_proxy"];

pub struct Invocation { pub args: Vec<OsString>, pub password: Option<Password>, pub timeout: Option<Duration> }
pub enum ExitClass { Ok=0, UserError=1, NetworkError=2, AuthError=3, OtherError=4, WriteConflict=5 }
pub enum Envelope { Ok { data: Value },
                    Failed { exit: ExitClass, error: String, message: String, data: Option<Value> } } // 部分成功时保留 data
pub enum RunnerError { PasswordLength(usize), Spawn(io::Error), Timeout(Duration), Signaled, UnknownExit(i32),
                       BadEnvelope { exit: i32, reason: &'static str }, OutputTooLarge, Io(io::Error) }
pub fn parse_envelope(code: i32, stdout: &[u8], stderr: &[u8]) -> Result<Envelope, RunnerError>;

#[derive(Clone, Default)] pub struct KeystoreWriteLock(/* Arc<tokio::sync::Mutex<()>> */);
impl KeystoreWriteLock { pub async fn acquire(&self) -> KeystoreGuard<'_>; }

pub struct HyphaeRunner { /* bin, home, default_timeout, keystore_lock */ }
impl HyphaeRunner {
    pub fn new(bin: VerifiedBinary, home: PathBuf, default_timeout: Duration) -> Self;
    pub fn keystore_lock(&self) -> &KeystoreWriteLock;    // import 在整个流程中持有它
    /// 绝对路径 + 参数数组；env_clear 后补回 ENV_ALLOW；HOME 与 cwd 都设为 home；
    /// 无口令时 stdin 为 null；process_group(0)；kill_on_drop
    pub fn command(&self, inv: &Invocation) -> tokio::process::Command;
    pub async fn run(&self, inv: Invocation) -> Result<Envelope, RunnerError>;                 // 只读命令
    pub async fn run_keystore_write(&self, inv: Invocation) -> Result<Envelope, RunnerError>;  // 持锁执行
}

pub enum EarlyExit { PasswordRejected, Misconfigured, Retryable }
pub fn classify_early_exit(code: Option<i32>) -> EarlyExit;   // 只看退出码：3 / 1 / 其他
#[derive(Serialize, Deserialize)]
pub struct DaemonPidFile { pub pid: u32, pub pgid: u32, pub start_time: u64, pub generation: u64, pub bin_sha256: String }
#[derive(Clone)] pub struct CommState { pub runner: Arc<HyphaeRunner>, /* 监管器句柄 */ }
pub fn router(state: CommState) -> axum::Router;
```

规则：
- **路径**：解析顺序为 `A24_HYPHAE_BIN` → `A24_SPEAKER_BIN`（兼容，打 warn）→ agent24d 同目录下的 `hyphae`。不查 PATH，相对路径直接拒绝。
- **hash**：hash 取自 lock 中当前平台那一项，平台缺项或不一致时 comm 进入 `binary_rejected`，其他路由照常工作。只有 `install` 产出的副本会被执行。
- **改 keystore 的命令必须走 `run_keystore_write`**（H3）：identity create、identity use、contact add、import。Hyphae `SaveKeyStore` 是读-改-rename，中间不加锁，并发写会丢失新身份的私钥。这把锁只能防住 comm 内部的并发；daemon 不写 keystore；外部进程本来就不应碰专用 HOME（G8）。
- **超时**：默认 15 s；send 与 `outbox retry` 为 `5 s × relay 数 + 10 s`；`relay info` 为 `--timeout` 加 2 s。超时后对进程组 `killpg`。send 超时时结果未知，按 §5.3 处理，不自动重试。
- **口令**：口令统一是 base64url 文本：自动生成的口令本身就是；导入或手输的口令原样保存为 UTF-8 字节。只经 stdin 传入，写完立即关闭。
- **日志与正文**（M7）：
  - runner 日志只记子命令名和退出码。
  - daemon 日志文件为 0600，**其中会有入站正文的明文**（Hyphae 会打印收到的消息），这一点要写进用户文档。
  - **已知限制**：`agent msg --content` 把正文放在 argv 里，同一台机器上的其他用户可以通过 `ps` 看到。在 Hyphae 提供 `--content-file` 之前（G9）只能接受。

## 4. REST 路由

Agent24 现有的 token 鉴权照常适用。响应格式与 Hyphae 一致：`{ok:true,data}`，或 `{ok:false,error,message,data?}`。下表 Hyphae 命令中省略 runner 统一追加的 `--json`。⚿ 表示带 `--password-stdin`，🔒 表示持有 KeystoreWriteLock。

**错误闭集**：

| error | HTTP | 来源 |
|---|---|---|
| `binary_rejected` | 503 | BinaryError |
| `unsupported_platform` | 501 | Windows（§9 R2） |
| `locked` | 423 | 拿不到口令；Hyphae 返回 `auth_error` |
| `not_configured` | 409 | 没有默认身份；relay 来源不是 `config`（R1）；keystore 未加密 |
| `invalid` | 400 | 请求校验失败；Hyphae `user_error` |
| `not_found` | 404 | 身份、联系人或 outbox 条目不存在（comm 在调用前查证） |
| `confirm_required` | 400 | 破坏性操作或导入缺少 `confirm:true` |
| `conflict` | 409 | Hyphae `write_conflict`；daemon 状态切换正在进行；导入源正被占用；目标 HOME 非空 |
| `network` | 502 | `network_error` 且不带 data |
| `partial` | 502 | 非零退出且带 data：**原样透传 data** |
| `timeout` | 504 | RunnerError::Timeout |
| `upstream` | 502 | `other_error` 且不带 data；BadEnvelope；被信号终止；未知退出码；输出超限 |

| 方法 + 路径 | 请求 | data | Hyphae 命令 |
|---|---|---|---|
| `GET /comm/identity` | — | `[{nickname,npub,default,encrypted}]` | `identity list` |
| `POST /comm/identity` | `{nickname, default?}` | 新身份 | `identity create --nickname N [--default]` ⚿🔒（首个身份流程见 §6.4） |
| `POST /comm/identity/default` | `{nickname}` | 同上 | `identity use --nickname N` 🔒；daemon 在运行时随后重启 |
| `GET /comm/contact` | — | `[{nickname,npub,role}]` | `contact list` |
| `POST /comm/contact` | `{nickname,npub,role?}` | 新联系人 | `contact add --nickname --npub [--role]` 🔒（npub 先在 comm 侧做 bech32 校验，G6） |
| `GET /comm/relay` | — | `{relays,source,configured}` | `relay list` |
| `PUT /comm/relay` | `{relays:[ws/wss…]}`，1..=8 个 | 同上 | **一次调用**：`relay set --relay U1 --relay U2 …`（整体替换）；daemon 在运行时重启 |
| `POST /comm/relay/probe` | `{url?}` | `{url,connected}`，写入 daemon 状态的 `relay_probe` | `relay info [U] --timeout 5`，只在手动触发时执行 |
| `POST /comm/send` | `{to, content, from?, encrypt=true}` | §5.1 | `agent msg --from F --to T --content C [--encrypt=false]` ⚿ |
| `GET /comm/history?as=&limit=` | limit 1..=200 | 收件历史 | `history inbox --as A --limit N` |
| `GET /comm/outbox?failed_only=` | — | outbox 条目 | `storage outbox list [--failed-only]` |
| `POST /comm/outbox/{event_id}/retry` | — | `{event_id,attempted,sent,queued,…}` | `storage outbox retry --id E`：沿用原签名事件 |
| `POST /comm/outbox/clear` | `{confirm:true, min_failures?}` | 清掉的条目 | `storage outbox clear --failed --yes [--min-failures]`：只删本地记录，不撤回 relay 已接受的事件 |
| `GET /comm/daemon` | — | §5.2 | 无 |
| `POST /comm/daemon/start` · `/stop` | — | 同上 | §6 |
| `POST /comm/import` | `{from, confirm:true, password?, dry_run?}` | `{identities,contacts,outbox,db_files}` | §4.1 |
| `POST /comm/unlock` | `{password, remember?}` | `{unlocked, remembered}` | 不调 Hyphae；`remember=true` 时写入或替换钥匙串条目（M4） |

- `from` / `as` 缺省时，取 `identity list` 中 `default:true` 的那一项。
- **已删除的路由**：`GET /comm/inbox`（M12）、`GET /comm/history?with=`。删后者是因为 `history conversation` 在基线版本不输出 JSON（H1、G7）。
- CLI 与 REST 一一对应：
  - `agent24 comm identity list|create|use`
  - `agent24 comm contact list|add`
  - `agent24 comm relay list|set|probe`
  - `agent24 comm send` / `history`
  - `agent24 comm outbox list|retry|clear`
  - `agent24 comm daemon status|start|stop`
  - `agent24 comm import` / `unlock [--remember]`
  - 风格沿用 `agent24 os`。加 `--json` 时原样输出 REST 的 envelope。

### 4.1 import（M1/M2，Low）

整个流程持有 KeystoreWriteLock，任何一步失败都删除 staging 目录，源目录从头到尾不修改。

1. **源路径**：
   - 先 `canonicalize(from)`；
   - 用 lstat 检查源目录及 `keystore.json`、`relays.json`、`outbox.json`、`messages.db{,-wal,-shm}`：不是符号链接、属主 uid 等于当前 uid、是普通文件或目录，否则返回 `invalid`；
   - 目标 `hyphae-home/.hyphae` 必须不存在或为空，否则返回 `conflict`。
2. **占用探测**：
   - 对 `outbox.json.lock` 尝试非阻塞的 `flock(LOCK_EX|LOCK_NB)`：失败返回 `conflict{source_in_use}`；成功则**一直持有到复制结束**，保证复制期间 outbox 不被改写。
   - 不足之处：Hyphae 只在每次 outbox 操作期间持有这把锁，所以拿到锁并不能证明源目录上没有 daemon 在跑。补充三道防线：
     - **（PR #635 R4 新增）** 同时对 `daemon.lock`（存在时）尝试非阻塞 `flock`——这把锁贯穿 daemon 整个进程生命周期，能抓到一个空闲但仍在跑的 daemon；文件不存在就跳过（兼容 Hyphae #104 之前的旧 HOME，见 G3）；
     - CLI 确认文案要求用户确认「已停止使用该目录的 hyphae」；
     - 复制前后比较 6 个文件的 (size, mtime)，有变化就重试一次（重试前先清空 staging 重建，不清空会让第一趟复制留下的过期文件随 rename 混进提交结果），仍有变化就返回 `conflict{source_changed}`。
   - 根治办法见 G3：daemon 在整个生命周期内持有一把锁——Hyphae #104 已实现，但本 crate 的 lock 还没跟进。
3. **复制**：上述 6 个文件按原样复制到 `hyphae-home.staging/.hyphae/`，文件不存在就跳过（`outbox.json` 在首次入队时才会生成），复制后的文件设为 0600。**不引入 rusqlite**：WAL 回放交给 Hyphae 在 staging HOME 中首次打开数据库时自行完成。
4. **校验**（runner 的 HOME 指向 staging）：
   - `identity list`：必须全部 `encrypted:true`。如有明文 keystore，返回 `not_configured`，提示语为：「源 keystore 未加密。请先在源目录用终端执行 `HOME=<源 HOME> hyphae identity change-password` 设置口令，再重新导入」（M3）。
   - `storage outbox list`：条目数必须等于源 `outbox.json` 的条目数（直接解析 JSON 计数）。
   - `history inbox --limit 1`：能正常返回，说明数据库可以打开。
5. **口令校验**（只在本地，不起 daemon）：
   - 只把 `keystore.json` 复制到一个一次性 HOME（`<state_dir>/comm/verify-<rand>/`）；
   - 在其中执行 `identity create --nickname __verify --password-stdin`：退出码 0 表示口令正确，3 表示错误（实测正确口令 rc=0，错误口令 rc=3 `incorrect password`）；
   - 执行完删除这个一次性 HOME。
   - 口令正确才写入钥匙串，account 用 `from_salt`。
6. **落地**：把 staging rename 成 `hyphae-home`。`dry_run` 只执行步骤 1–4 并报告数量。CLI 需要交互确认，或显式加 `--yes`。

## 5. 状态模型

### 5.1 消息四层

| 层 | 含义 | 依据（Hyphae 字段） | 基础通信 |
|---|---|---|---|
| L1 本地待发 | 已签名，已写入 history 或 outbox | `history_stored` 或 `queued_for_retry`，且 `published_to==0`；outbox `pending` | 有 |
| L2 relay 已接受 | ≥1 个 relay 返回 OK | `published_to>0`，或 outbox 条目变为 `sent` | 有 |
| L3 对端确认 | 对端回执 | 基线中**没有**回执字段（`StoredMessage` 不带 ack）（G4） | 预留；UI 显示「未确认」，从不显示「已送达」 |
| L4 执行结果 | 对端执行完成 | T01-E | 不在本范围 |

relay 全部不可达时，send **仍返回 ok:true 和退出码 0**，同时 `published_to:0`、`queued_for_retry:true`（实测）。因此层级由 `published_to` 判定，不看 `ok`。

### 5.2 daemon 三项状态（`GET /comm/daemon`）

```jsonc
{ "process":     {"state":"stopped|starting|running|backoff|gave_up|locked","pid":..,"generation":..,
                  "consecutive_failures":..,"reason":"no_identity|no_relay|keychain_unavailable|password_rejected|binary_rejected|misconfigured"},
  "relay_probe": {"url":"..","connected":true,"at_ms":..,"error":null},   // 只来自最近一次手动 probe，可能为 null
  "catch_up":    {"state":"unknown|incomplete","last_incomplete_at_ms":..} }
```

- `relay_probe` 的 `connected` 只表示一次 WebSocket 握手成功。
- `catch_up`：基线 daemon 没有结构化进度，只能从日志中的 `Inbox scan incomplete` 得到 `incomplete`，其余时候一律为 `unknown`。**永远不会显示「已补收完成」**（G2）。
- 三项分开呈现，互不推导。

### 5.3 部分成功的呈现

| 情形（send/retry 返回的 data） | UI 文案 | 允许的操作 |
|---|---|---|
| `published_to>0`，且 `!history_stored`、`queue_state_unknown`、`audit_error` 三者至少有一个成立 | **「已发出，本地记账异常」**，并显示 event_id | 按 event_id 查 outbox 与收件历史 |
| `published_to==0` 且 `queued_for_retry` | 「已入队，等待 relay」 | `outbox/{event_id}/retry` |
| `published_to==0` 且 `queue_state_unknown` | 「状态未知」 | 先重新读取 outbox |
| `superseded` | 「已被同 ID 的较新条目取代」 | 重新读取 outbox |
| `timeout`（没有 data） | 「结果未知，可能已发出」 | 查 outbox |

任何情形都**不提供「重发」**（即生成新的 event_id 再发一次）。唯一的重试入口是原 event_id 的 outbox retry。对 `POST /comm/send`，CLI 和 UI 都不自动重试。

## 6. Daemon 监管

### 6.1 启动（COMM-4a）

前置条件按顺序检查，任一不满足就进入 `locked{reason}`，不进入退避：
1. 二进制已校验；
2. 存在 `default:true` 的身份，且 keystore 已加密；
3. `relay list` 的 `source=="config"` 且列表非空；
4. 拿得到口令。

启动命令：

```
hyphae daemon --identity <default> --password-stdin --notify=false --auto-reply=false \
  --relay <r1> [--relay <r2>…] --watch-interval 30 --retry-interval 60 --json
```

- `--auto-reply=false` 也显式传，防御 Hyphae 默认值变化。relay 显式传入，不依赖 Hyphae 在未配置时回落到默认 relay（R1）。
- **就绪判定**：存活满 3 s 记为 `running`。在此之前退出时，**只按退出码分类**，不解析 stderr：
  - 退出码 3：`locked{password_rejected}`，不重试；
  - 退出码 1：`gave_up{misconfigured}`，不重试；
  - 其他：进入退避。
- **单实例**：每个 state_dir 只有一个 agent24d（`try_acquire_singleton`），comm 内部再用 Mutex 保证同一时间只有一个 daemon generation。Hyphae 自身没有按 HOME 互斥（G3）。
- **孤儿识别**：spawn 时把 `DaemonPidFile` 写入 `hyphae-daemon.pid`，其中记录进程启动时间：macOS 取 `proc_pidinfo` 的 `pbi_start_tvsec`，Linux 取 `/proc/<pid>/stat` 第 22 字段。agent24d 启动时，只有当 pid 存活**且**启动时间与文件记录一致，才认定为孤儿，对其 `killpg`（先 TERM 后 KILL）。这样不会误杀复用了同一 pid 的无关进程。

### 6.2 停止与 agent24d 关机（COMM-4a）
- **stop**：对进程组发 SIGTERM（Hyphae 以 `signal.NotifyContext` 处理，实测退出码 0），宽限 `A24_MODULE_STOP_GRACE_MS` 后发 SIGKILL，再按 `REAP_TIMEOUT` 回收。
- **关机**：comm 的停止并入 SHUT-1b 的模块停止阶段，在 `last-shutdown.json` 的 `records[]` 里记一条，`name` 为 `comm.hyphae`。
- **崩溃**：agent24d 崩溃后，由下次启动时的孤儿清理兜底。
- **autostart**：第一次手动 start 成功后，`daemon.autostart` 置为 true；手动 stop 后置为 false。

### 6.3 配置变更、退避与日志（COMM-4a）
- **配置变更**：relay 或默认身份变更后，stop 再 start。这次重启不计入失败次数。已入队的 outbox 条目沿用各自原来的 relay。
- **退避**：复用 `agent24_os_proto::supervise::RestartPolicy`：500 ms 起步、每次翻倍，连续 5 次失败熔断为 `gave_up`，正常运行满 60 s 后清零。熔断后只能手动 start 恢复。
- **日志**：stdout 和 stderr 写入 `logs/hyphae-daemon.log`，文件权限 0600。每次 spawn 前，超过 5 MB 就截断为空（M12，不做轮转）。

### 6.4 口令与钥匙串（COMM-1b）
- **L0**：macOS 用 Keychain，Linux 用 Secret Service。实现用 `keyring` crate（**新依赖**），禁止调用 `security` 命令行，因为 `-w` 参数会进 argv。
- **account**：取 `hex(sha256(keystore.salt))`。
- **首个身份**：此时 keystore 还没有 salt，流程为：
  1. 用 `Password::generate` 生成口令；
  2. 以 `Account::Pending(<uuid>)` 写入钥匙串；
  3. 执行 `identity create` ⚿🔒；
  4. 读出新的 salt，`rename(Pending → Salt)`。
  - create 失败时删除 Pending 条目；
  - agent24d 启动时，残留的 Pending 条目只记日志、不使用。
- **L2 仅内存**：钥匙串不可用时状态为 `locked{keychain_unavailable}`。用户执行 `agent24 comm unlock` 交出口令，口令只保存在 `Zeroizing` 内存中；agent24d 重启后回到 `locked`。加 `--remember` 时写入或替换钥匙串条目，用于钥匙串恢复之后，或修改过口令之后。
- **不提供**：systemd 凭据（M12 砍掉）、明文口令文件、通过环境变量或 config 传口令。降级不会自动发生。
- **显式降级开关**：`A24_COMM_PASSWORD_STORE` 环境变量可选 `keyring`（默认，未设置时也是这个）或 `memory`（改用纯内存的 `MemoryPasswordStore`，口令不持久、daemon 重启即丢，仅用于隔离环境下的测试/联调，启动时打 warn）；其他值不会被悄悄当作 `keyring` 或 `memory`，daemon 照常启动但 comm 路由一律答 `not_configured`，data 里带上具体原因。

### 6.5 与 Deployment A5 的关系
- A5（#602）之后，桌面端会复用已在运行的 agent24d，所以**持有 Hyphae daemon 的始终是唯一那一个 agent24d**。
- 桌面端不 spawn Hyphae，也不读 hyphae-home。
- 托盘停止外部 agent24d 时不动它，Hyphae 也照常运行；停止桌面端自己拉起的 agent24d 时，Hyphae 随之按 §6.2 停止。

## 7. Zero-run 判据与测试

**判据**：comm 运行期间 runs 表新增数为 0，模型后端收到的请求数为 0，模块调用次数为 0。覆盖六类入站：

| 类别 | 样本来源 |
|---|---|
| plain | 纯文本 |
| 旧 kind 30078 | F4 `f4/1` envelope，`intent:"ask"`。注意 Hyphae 普通消息本身就是 kind 30078，**不能按 kind 分流** |
| query / response | Hyphae `tests/contracts/testdata/public-query-fixtures.json` |
| receipt | F4 `intent:"ack"` 加 `status:"working"`（T01 尚无回执候选） |
| canary | nostr-bridge FU-32 的 canary 正文 |

**结构约束**（M8，COMM-5b）：
- 测试 `comm_dependency_allowlist` 执行 `cargo metadata --format-version 1`，遍历 `agent24-comm` 的**完整依赖图**（normal 依赖，含传递依赖），断言：
  - 名字以 `agent24-` 开头的包只能是 `agent24-os-proto`、`agent24-domain`、`agent24-protocol`；
  - 不包含任何 HTTP 客户端：`reqwest`、`ureq`、`isahc`、`surf`、`attohttpc`，以及带 `client` feature 的 `hyper` / `hyper-util`。这是为了防止有人走 HTTP 回环去调 `/api/v1/runs`。
- **正对照**：同一个检查函数作用于 `agent24d` 包时，必须命中 `agent24-agent`。
- 类型约束：`CommState` 不包含 `Store`、`ModelRouter`、`ToolRegistry`、`AppState`。
- comm 没有入站回调，入站内容只能经 `GET /comm/history` 被动读取并原样返回。

**测试入口**（H4）：agent24d 没有 lib target，只能当黑盒测试。因此：

| 测试 | 位置 | 做法 |
|---|---|---|
| T1 runner/router 单测 | `agent24-comm`，`--features test-lock-override` | 假二进制（shell 脚本）配合 `HyphaeLock::override_for_test`；用 `tower::ServiceExt::oneshot` 在进程内调 router。覆盖：envelope 0–5、部分成功、超时、口令只经 stdin。**HOME 断言**：假二进制把 `$HOME` 和 `pwd` 回显出来，断言二者都等于 hyphae-home（r2 已验证可行） |
| T2 zero-run（进程内） | `agent24-comm` | 假二进制在 `history inbox` / `storage outbox list` 中返回六类样本；假 daemon 每 100 ms 打一行日志。**计数后端**：照 `me4_model_blackbox.rs` 的做法，起一个计数 HTTP stub（Python `http.server`，找不到 python3 时测试直接失败，不跳过），并把 `OLLAMA_URL`、`OPENAI_BASE_URL`、`A24_BASE_URL` 都指向它。启动 daemon、各读接口轮询 20 次、stop 之后，断言 stub 请求数为 0 |
| T3 正对照与真实二进制黑盒 | `apps/agent24d/tests/comm_blackbox.rs` | 运行真实 agent24d，模型后端用同一个计数 stub。(a) 对 F4b 实际使用的 HTTP 路径（`POST /api/v1/sessions` 加 `POST /api/v1/runs`）发一条 plain 消息，断言 stub 请求数 ≥1，且 `GET /api/v1/runs` 条数加 1；**正对照为 0 则整个测试判失败**。(b) `A24_HYPHAE_BIN` 指向 CI 按 lock 构建出的真实 linux-x64 二进制，再起一个 `hyphae-relay`、一个对端 Hyphae：对端向 A 发送六类样本，A 收齐后断言 runs 条数加 0、stub 请求数为 0 |

**F4b 冻结**（H5，COMM-5a）：
- **只拦截分派这一步**：在 `InboundBridge.handle` 开头检查 `A24_NOSTR_F4B_INBOUND=1`，未设置就直接返回，不调用 `runToCompletion`。`pollOnce` 的轮询和 `liveness.observe` 照常运行，否则 FU-32 的 canary 永远无法被确认。
- **迁移提示**：启动时如果配置了 `A24_NOSTR_ALLOWED_NPUBS` 却没设 flag，打印一条醒目 warn：「F4b 入站执行已冻结（默认关闭）。已授权的 N 个对端消息将只被读取、不执行；如需旧行为请设置 A24_NOSTR_F4B_INBOUND=1」。
- **测试**：`inbound.test.ts` 用 spy 替换 `runToCompletion`：
  - 默认配置下，已授权消息使 spy 计数为 0，canary 仍被 `observe` 确认；
  - 设置 flag 后，spy 计数为 1（正对照）。
- **文档改动列为 COMM-5a 的实现内容**：`CHANGELOG.md`、`docs/SOAK-F5.md`（soak 若要验证入站执行，需加 flag）、`docs/specs/F4-nostr-channel.md`（§F4b 标注冻结）。
- **单一消费者**：COMM 不消费入站。F4b 开启时它是唯一的执行消费者。

## 8. 任务表

每个 PR 目标约 300 行，并在描述中写明「COMM-n ↔ Hyphae Tnn」和 §8.1 的锁信息。

| 编号 | Hyphae | 交付 | 依赖 | 规模 | 可证伪验收 |
|---|---|---|---|---|---|
| COMM-5a | T20-B（前置） | F4b 冻结：只拦截 `handle` 的分派；迁移 warn；CHANGELOG、SOAK-F5、F4 spec；nostr-bridge 支持 `A24_HYPHAE_BIN`，优先级高于 `A24_SPEAKER_BIN` | 无，**最先做** | S | 默认 spy=0，开 flag 后 spy=1；默认配置下 canary 仍从 `pending` 变为 `confirmed`；配了 ALLOWED_NPUBS 但无 flag 时，启动日志中出现该 warn；只设 `A24_SPEAKER_BIN` 时行为不变 |
| COMM-1a | T20-A | `agent24-comm` crate：lock 与配方、`VerifiedBinary::install`、runner、`parse_envelope`、T1 | COMM-0 冻结 | M | 改动二进制 1 个字节必报 HashMismatch；用相对路径报 NotAbsolute；副本已存在且被篡改时报 HashMismatch；退出码 0–5、7、ok 字段与退出码矛盾、两段 JSON，各一个表驱动用例；部分成功能取出 event_id；子进程环境中看不到哨兵变量；4097 字节口令被拒 |
| COMM-1b | T20-A | `PasswordStore`：keyring 与内存两种实现、Account 改名流程、base64url 编码；`KeystoreWriteLock`（H3，COMM-1a 未做，归并到此）；CI job：按 lock 构建 linux-x64 并比对 hash（同样归并到此） | COMM-1a | S | 首个身份走完 Pending→Salt，钥匙串中只剩 Salt 条目；create 失败后 Pending 条目被删；模拟钥匙串不可用时进入 `locked{keychain_unavailable}`；`unlock --remember` 能替换已有条目；不加锁并发 10 次 `identity create` 会丢身份（先证），加锁后 10 个都在（后修）；CI 构建出的 hash 与 lock 不一致时 job 失败 |
| COMM-2a | T20-A | 路由与 CLI：identity、contact、relay；错误闭集 | COMM-1b | M | 用真实二进制和临时 HOME 走通 create→list→use→contact add→relay set→list；`source=default` 时 send 返回 `not_configured`；**并发 10 个 `POST /comm/identity` 后，`identity list` 中 10 个身份都在**，且每个都能被 `daemon --identity` 解锁（H3） |
| COMM-2b | T20-A | import（§4.1） | COMM-2a | M | 不带 confirm 返回 `confirm_required`；源目录 6 个文件的 hash 和 mtime 在导入前后不变；**导入前后 outbox 条目数一致**（H2）；身份数、联系人数一致；源目录是符号链接或属主不对时被拒；测试中先持有 `outbox.json.lock` 再导入，返回 `conflict{source_in_use}`；明文 keystore 被拒，提示语包含 `change-password`；口令错误时钥匙串中没有新条目 |
| COMM-3 | T20-B | send、history、outbox 的路由与 CLI；§5.1 分层与 §5.3 呈现字段 | COMM-2a | M | relay 不可达时 send 返回 ok，层级为 L1，`published_to==0`；retry 返回的 event_id 与原 event_id 相等；注入 data `{published_to:1,history_stored:false}` 得到 `partial`，接口中没有重发入口；clear 不带 confirm 被拒 |
| COMM-4a | T20-B | daemon 监管：启动前置条件、按退出码分类、RestartPolicy、配置变更重启、pid 文件加启动时间的孤儿清理、接入 SHUT-1b、日志 0600 与启动时截断 | COMM-2a | M | kill -9 后按 500 ms、1000 ms 重启，第 5 次熔断；退出码 3 进入 `locked{password_rejected}` 且不重试；修改 relay 后 generation 加 1、失败计数不变；agent24d 被 SIGKILL 后重启，旧的 pgid 被清理；把 pid 文件里的 start_time 改成别的值后，不会误杀；关机后 `records[]` 中有 `comm.hyphae` |
| COMM-4b | T20-B | 三项状态、手动 probe、catch_up 日志匹配 | COMM-4a | S | relay 停掉时 probe 结果为 `connected:false`；日志中出现 incomplete 行后，状态变为 `incomplete`；任何输入都不会产生 `complete` 状态 |
| COMM-5b | T20-B | §7 结构约束、T2、T3 | COMM-3, COMM-4a | M | 依赖 allowlist 测试能挡住加了 `reqwest` 的分支（PR 中贴出该分支的失败截图），并对 agent24d 命中 `agent24-agent`；T2 中 stub=0；T3 正对照 runs Δ=1，真实二进制场景 runs Δ=0、stub=0 |
| COMM-6 | T21 | UI：身份、联系人、relay、daemon 状态、锁定状态、导入向导 | COMM-2b, COMM-4b | M | 在 `locked`、`gave_up` 状态或 relay 未配置时，界面不显示绿色；不会出现「补收完成」；UI 不 spawn 进程 |
| COMM-7 | T22 | UI：收件历史、outbox、重试；§5.3 文案；**双仓联调**（§8.2） | COMM-3, COMM-6 | M | 界面中没有「已送达」；重试按钮只出现在 outbox 条目上；§8.2 全部步骤通过 |

依赖链：

```
COMM-5a（独立）
COMM-1a → 1b → 2a → { 2b, 3, 4a → 4b }
COMM-3 + COMM-4a → 5b
COMM-2b + COMM-4b → 6
COMM-3 + COMM-6 → 7
```

### 8.1 lock 清单与回填（H4，M9，M11）

```jsonc
// rust/crates/agent24-comm/hyphae.lock.json （include_str! 内嵌）
{ "schema": 1,
  "source_sha": "a4aa606eb81d5c040d94c51cdf94553e646d8674",
  "go": "go1.26.4",
  "recipe": "GOTOOLCHAIN=go1.26.4 CGO_ENABLED=0 GOOS=<os> GOARCH=<arch> go build -trimpath -buildvcs=false -ldflags='-buildid=' -o hyphae ./cmd/hyphae",
  "binaries": {
    "darwin-arm64": "f53c29b31d8ca5eb0124ced246bcff6610f048f18bc8dcc2de27f685dad8b221",
    "linux-x64":    "042f6200f43c095cfcec16b31a136467a39b08c810819ab3c875df5cfe0164f6",
    "darwin-x64": null, "linux-arm64": null } }
```

- **候选 hash 的来源**：上面两个值是本机 go1.26.4 按配方构建所得。darwin-arm64 构建两次，hash 一致；linux-x64 在 COMM-1b 里又按配方本机重新构建一次（同一台机器，go1.26.4，`hyphae-src` 工作区锁定在 `source_sha`），两次结果与 lock 中记录的值逐字节一致。CI job（`.github/workflows/hyphae-lock-verify.yml`，COMM-1b 新增）会在 CI 环境里重复这个构建并比对 hash；由于本次改动不经 push/PR 落地，这个 job 本身**尚未在 GitHub Actions 里实际跑过**——如实记录：本地复现已确认，CI 复现待这次改动被推送开 PR 后首次运行确认。
- **null 的含义**：表示该平台尚未验证，comm 在该平台报 `binary_rejected`。
- **`hyphae-relay`**：联调用的 relay 用同一配方构建 `./cmd/hyphae-relay`，hash 只记在 PR 描述里，不进 lock，因为运行期不会用到它。
- **验证记录不进 lock**：每个 COMM PR 的描述里写明 Agent24 head SHA、Hyphae SHA、各平台 sha256、Go 版本，以及测试和联调结果。
- **修改 lock**：改动 lock 的 PR 必须说明原因。
- **双向标注**：Hyphae 侧 T20/T21/T22 的 PR 用同样格式回填 Agent24 SHA。

### 8.2 联调验收步骤（COMM-7；COMM-3/4a 先跑一部分）

**环境**：
- 两套临时 HOME：A 由 agent24d 管理，B 为裸 Hyphae CLI；
- 本地 relay：按配方构建 `hyphae-relay`，执行 `hyphae-relay --port <p> --data-dir <tmp>`；
- 全程不碰真实的 `~/.hyphae`、`~/.agent24`。

**步骤**：
1. **hash**：二进制 hash 与 lock 一致；A 侧状态不是 `binary_rejected`。
2. **配置**：A 执行 `agent24 comm identity create`，B 执行 `hyphae identity create --password-stdin`；双方互加联系人、`relay set`；A 执行 `daemon start`，B 执行 `hyphae daemon --notify=false`。
3. **双向**：A 发给 B，B 的 `history inbox` 中 event_id 一致；B 发给 A，A 的 `GET /comm/history` 能看到；A 侧层级为 L2；界面没有「已送达」。
4. **断线重试**：停掉 relay。A 发送，得到 `published_to==0` 和 event_id E。恢复 relay 并完成 retry 后，B 的历史中 id=E 的消息**恰好 1 条**，且没有多出其他 id；A 的 outbox 中 E 为 `sent`。
5. **重启无重复**：重启 agent24d 后，A、B 两侧历史条数都不变。
6. **积压**：A 的 daemon 停止期间，B 发送 125 条；A 启动后历史加 125，再重启一次加 0。
7. **zero-run**：第 2 步之前和第 6 步之后，runs 条数和计数 stub 的请求数都不变；同一次运行中，T3 的正对照能数到 1。
8. **秘密不外泄**：联调期间采样 `ps -axo args`，其中不含口令；`grep -r <口令>` 扫描 `<state_dir>`，0 命中。正文出现在 argv 和日志中属于已知限制（M7），不算失败。
9. **记录**：两侧 SHA、两个二进制的 hash、操作系统、每一步的结果，写入 PR 描述。

## 9. 风险与待定项

| # | 项 | 处理 |
|---|---|---|
| R1 | relay 默认值暂时留空，但 Hyphae 在未配置时会自动回落到 `wss://relay.aastar.io`（实测 `source:"default"`） | `source!="config"` 视为未配置；启动 daemon 时总是显式传 `--relay`。官方 relay 上线后由 Agent24 自己写入默认值 |
| R2 | Windows | Hyphae 在 Windows 上读 `USERPROFILE`；`process_group` / `killpg` 只在 Unix 可用；**#549 的教训：`env_clear` 会让 Windows PowerShell 子进程挂起**，Windows 版的白名单必须补回 `SystemRoot`、`ComSpec`、`PATHEXT`、`USERPROFILE` 等变量，并单独验证。在 DEP-A7 冻结之前，Windows 返回 `unsupported_platform` |
| R3 | macOS 钥匙串 ACL 与代码签名绑定，未签名的 agent24d 升级后可能再次弹窗 | 在 DEP-B3 之前接受这一点；UI 给出说明 |
| R4 | A3 发布包只含 `agent24` 和 `agent24d` | 另起 DEP 任务把 `hyphae` 加进发布包（需修改 A3 的布局断言）；在此之前用 `A24_HYPHAE_BIN` 指定 |
| R5 | send 超时或进程被杀时，是否已发出无法确定 | 返回 `timeout`；按 §5.3 核对；不自动重试 |
| R6 | 构建配方的 hash 依赖 Go 的补丁版本 | 配方固定 `GOTOOLCHAIN`；升级 Go 视为修改 lock |
| G1 | 没有只读的口令验证命令；`change-password` 在 JSON 模式下不接受 stdin | 暂用 `identity create __verify`，放在一次性 HOME 中执行（§4.1）。请 Hyphae 提供 `keystore verify --password-stdin` |
| G2 | daemon 没有结构化的健康和补收进度 | 暂时显示 `unknown`/`incomplete`。请 Hyphae 提供 JSON-lines 状态 |
| G3 | daemon 没有按 HOME 的生命周期锁 | **部分解决（PR #635 R4，2026-10-01）**：Hyphae [#104](https://github.com/iDoris-ai/Hyphae/pull/104) 已合并进 `main`，daemon 运行期间持有 `.hyphae/daemon.lock`；import 的 §4.1 步骤 2 已改为同时探测这把锁（非阻塞 flock，文件不存在就跳过，兼容旧版）。但本 crate 的 `hyphae.lock.json` 仍锁定在 `#104` 之前的 `a4aa606`，所以**对一个正在运行但空闲的旧版 Hyphae 守护进程仍检测不到**——这是待办：升级 lock 到 #104 之后的版本 |
| G4 | history 不带对端回执 | L3 等 T01 |
| G5 | `--version` 输出 `dev` | 以 sha256 为准；建议 Hyphae release 用 ldflags 注入版本号 |
| G6 | 非法 npub 返回 `other_error`（退出码 4） | comm 在调用前自行校验 |
| G7 | `history conversation` 不支持 `--json`（H1）；另外 `storage info --json` 在数据库还没创建时也只输出文本 | 已删除 `?with=` 路由。请 Hyphae 为这两个命令补上 JSON envelope |
| G8 | `SaveKeyStore` 是读-改-rename，中间无锁，并发写会丢失新身份的私钥（H3） | comm 用 KeystoreWriteLock 串行化。请 Hyphae 在 keystore 的读-改-写全程持有 flock |
| G9 | 正文只能通过 `--content` 经 argv 传入（M7） | 列为已知限制。请 Hyphae 提供 `--content-file` 或 `--content-stdin` |

## 10. 评审处置表（COMM-0 r1 → r2）

| 条目 | 评审意见 | 处置 |
|---|---|---|
| H1 | `history conversation` 没有 JSON 输出 | 删除 `GET /comm/history?with=`；新增 G7 |
| H2 | outbox 在 `outbox.json` 和 `.lock` 中，不在 db 里 | 更正 §2；import 复制 `outbox.json`；COMM-2b 验收加入「导入前后 outbox 条目数一致」 |
| H3 | keystore 读-改-rename 无锁，会丢私钥 | 新增 `KeystoreWriteLock` 和 `run_keystore_write`，用于 create、use、contact add、import；COMM-2a 加入 10 路并发验收；新增 G8 |
| H4 | 测试方案无法落地 | lock 加入配方与 Go 版本，并附本机复现的候选 hash；CI 按 lock 构建 linux-x64；T1/T2 下沉到 crate 内（oneshot 加 `test-lock-override` feature）；T3 走 agent24d 黑盒，计数 stub 照 me4 的做法 |
| H5 | F4b 冻结会让 canary 无法确认 | 只拦截 `handle` 的分派，保留轮询和 `observe`；加迁移 warn；CHANGELOG、SOAK-F5、F4 spec 列入 COMM-5a |
| M1/M2 | 口令校验方式、源库一致性 | 改为在一次性 HOME 中跑 `identity create __verify`（已实测）；复制期间持有 `outbox.json.lock` 的 flock，前后比对 (size, mtime)；6 个文件原样复制；不引入 rusqlite，WAL 由 Hyphae 自行回放；补充说明这把 flock 的局限，并在 G3 中提出生命周期锁 |
| M3 | 明文 keystore 被拒后无路可走 | 错误提示给出 `change-password` 的操作步骤 |
| M4 | 钥匙串 account、编码、记住口令 | account 取 sha256(salt)，首个身份先用 Pending 再改名；口令为 base64url；`unlock --remember` |
| M5 | 默认身份有两个来源 | 删除 `active_identity`，以 `default:true` 为准 |
| M6 | 代理变量；Windows 上的 env_clear | ENV_ALLOW 加入代理变量的大小写两种写法；R2 写入 #549 的教训 |
| M7 | 日志和 argv 中的正文 | 日志设为 0600 并写明会有明文；argv 中的正文列为已知限制；新增 G9 |
| M8 | 依赖约束太弱 | 改为对 `cargo metadata` 完整依赖图做 allowlist，并禁止 HTTP 客户端类 crate；正对照保留 |
| M9 | 二进制安装存在 TOCTOU | `install(source, expected, install_dir)`：只读一次进内存，算完 hash 后以 O_EXCL 写副本；BinaryError 增加 Io；lock 移入 crate 目录 |
| M11 | lock 里不该放验证记录 | 删除 `verified[]`，验证记录只写在 PR 描述里 |
| M12 | 拆分与范围 | 新增 COMM-5a（S，最先做）；1 拆为 1a/1b，2 拆为 2a/2b，4 拆为 4a/4b；砍掉 systemd 凭据、周期 probe、日志轮转、`GET /comm/inbox`；每个 PR 目标约 300 行；保留独立的 agent24-comm crate |
| Low | 若干细节 | 提前退出只按退出码分类；孤儿识别用 pid 加启动时间；`relay set` 改为一次调用；runner 的 cwd 设为 hyphae-home；import 的 `from` 先 canonicalize，拒绝符号链接，并校验 uid；「从未打开该路径」的断言改为回显 HOME 的假二进制 |
