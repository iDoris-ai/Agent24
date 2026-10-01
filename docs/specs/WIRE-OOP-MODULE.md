# WIRE-OOP-MODULE — 进程外领域 OS 模块协议参考（ME4-5.4.1 / T14）

> **目标读者**：一个从未读过本仓源码的第三方开发者，只凭本文档，用任意语言写出一个能被
> `agent24d` 挂载、代理、回调的进程外领域 OS 模块。
>
> **判据**（`docs/agent/PLAN-ME4-OS-CAPABILITIES.md` §五 T14 原文）：**「不看 SDK 源码能不能写出来」**——
> `examples/node-module/` 是按本文档从零写的 Node.js 参考实现（不 `require`/`import` 本仓任何 Rust crate，
> 只用 Node 标准库），`rust/apps/agent24d/tests/me4_node_module_blackbox.rs` 是它的黑盒验收。
>
> **基线**：Agent24 `main@ad8cddb`（2026-09-29）。文中 `file:line` 均按此提交核对；实现演进后行号会漂移，
> 但字段名、常量值、错误闭集是协议契约，不应无预警变化——变化时按 §1.7「manifest schema 版本」协商。
>
> **权威来源与本文的关系**：[`SPEC-ME3-OUT-OF-PROCESS.md`](SPEC-ME3-OUT-OF-PROCESS.md) 是设计与取舍的
> 权威记录（威胁模型、每条决定"为什么不是另一个答案"）。本文只做**协议参考**：把同一套事实
> 按"实现者要按什么顺序知道什么"重新组织，并给每个字段配真实 JSON 例子。两者冲突时以代码事实为准
> （本文的每条字段/常量都在真实源码里核对过，不是转抄 SPEC 的中文叙述）。
> [`docs/design/A3-ATTACHED-MODULE.md`](../design/A3-ATTACHED-MODULE.md) 描述的是**另一种**进程外模块
> （用户自己启动、向已运行的 daemon 注册的"附着式模块"，如 AgentEar）——握手帧、方法、错误闭集与本文
> 完全共用，只有"谁启动进程 / token 从哪来 / 连接能不能断了重连"三点不同，本文不重复，需要时参见该文档 §2 的对照表。
>
> **本文档覆盖 ME-3（`{events, memory, approval}`）+ ME-4a（`scheduler`）+ ME-4b（`models`）全部已交付的回调方法**。
> 本仓 `examples/node-module/` 参考实现出于"原型、最小可用"的范围控制，只声明并使用 `events` 与
> `memory.private`；`scheduler`/`model`/`approval` 三族按本文档同等详细程度记录，但**未**在参考实现或
> 黑盒测试里演示往返（followups.md FU-104 记录了这条范围收窄）。

## 目录

1. 打包与 manifest（`domain-os.yml`）
2. 启动环境
3. 回调连接与 `initialize` 握手
4. 帧格式与 RPC
5. 回调方法（逐一：params / result / 错误 / 真实 JSON 例子）
6. 内核 → 模块：HTTP 代理与 fired 投递
7. 退出与断连语义
8. 错误闭集表（附录）

---

## 1. 打包与 manifest（`domain-os.yml`）

### 1.1 包目录形状

一个模块是磁盘上的一个目录（内核发现 = 从磁盘读，见 SPEC §4）：

```
<package-dir>/
  domain-os.yml     # 唯一固定文件名，MANIFEST_FILE
  <spawn.command 与 spawn.args 指向的其余文件>
```

内核在 `spawn.command` 指向的可执行文件之外，还要求整棵包目录树（本身与每个文件/目录）属主与
daemon 用户一致、组/其他不可写（符号链接只查属主）——这是启动前的完整性检查，不是本文档的重点，
见 SPEC-ME3 §1「启动前还要校验整棵包目录树的属主」。`agent24 os install <目录>` 会用 `0700` 建包根目录
并去掉组/其他写权限。

### 1.2 `domain-os.yml` 字段表

解析分两步（`RawManifest`，`rust/crates/agent24-domain/src/lib.rs:406-461`，顶层
`#[serde(deny_unknown_fields)]`）：

**第一步——信封（宽容，不 `deny_unknown_fields`）**，只读三个字段用于版本门（`lib.rs:593-689`）：

| 字段 | 类型 | 必填 | 缺省 |
|---|---|---|---|
| `name` | string | 否（缺失时报错信息里用 `"<unnamed>"`） | — |
| `manifest_version` | u32 | 否 | `1`（`MANIFEST_SCHEMA_VERSION`，`lib.rs:400`） |
| `min_daemon_protocol` | u32 | 否 | 无门槛 |

`manifest_version` 高于本 daemon 支持的 → 解析期拒绝（`DomainError::ManifestUnsupported`），
早于握手。今天只接受 `1`。`min_daemon_protocol` 高于本 daemon 的回调协议上限同理拒绝。

**第二步——严格结构**（`RawManifest`，字段与校验）：

| YAML key | 类型 | 必填 | 缺省 | 校验 |
|---|---|---|---|---|
| `name` | string | ✅ | — | `[a-z0-9][a-z0-9_-]*`，≤64 字节（`MAX_NAME_BYTES`），不能是 Windows 保留设备名（`CON`/`NUL`/…）（`lib.rs:494-538`） |
| `version` | string | ✅ | — | trim 后非空（`lib.rs:776-778`） |
| `route_namespace` | string | ✅ | — | 必须**恰好等于** `/api/v1/<name>`（`lib.rs:547,779-785`） |
| `event_module` | string | ✅ | — | 必须**恰好等于** `name`（`lib.rs:786-792`） |
| `data_dir` | string | ✅ | — | 必须**恰好等于** `~/.agent24/os/<name>/`（`lib.rs:552,793-803`） |
| `kernel_capabilities` | `[string]` | 否 | `[]` | 每项经 `Capability::parse` 校验，闭集 `events, models, scheduler, policy, memory, approval`（`lib.rs:144-171`）；未知值报错并点名 |
| `requires_models` | `[string]` | 否 | `[]` | 信息性，本文不展开 |
| `requires_apis` | `[string]` | 否 | `[]` | 同上 |
| `requires_deps` | `[string]` | 否 | `[]` | 同上 |
| `model_access` | string | 否 | 缺省视为 `local_only` | 值闭集 `local_only` \| `remote_allowed`；**声明了此字段但 `kernel_capabilities` 不含 `models`** → 解析期拒绝（`lib.rs:818-830`） |
| `ui_entry` | string | 否 | `None` | 信息性 |
| `impl_kind` | enum（snake_case） | ✅ | — | `in_process_crate` \| `out_of_process_provider` \| `attached_process`（`lib.rs:274-290`） |
| `spawn` | `{command: string, args: [string]=[]}` | 见下 | `None` | 自身也 `deny_unknown_fields`（`lib.rs:316-328`）；`command`/`args` 纯词法校验：非空、拒绝绝对路径、拒绝 `..`（`lib.rs:370-392`） |
| `host_commands` | `[string]` | 否 | `[]` | **只在 `impl_kind: attached_process` 下合法**（`lib.rs:881-887`）；每项 1–32 字符 `[a-z0-9_]`。进程外模块（本文主题）不用这个字段 |
| `manifest_version` | u32 | 否 | — | 复述字段，仅为了让第二步的 `deny_unknown_fields` 不拒绝它（`lib.rs:447-457`） |
| `min_daemon_protocol` | u32 | 否 | — | 同上（`lib.rs:458-460`） |

**`impl_kind` × `spawn` 双向校验**（`lib.rs:845-872`，测试 `lib.rs:2328-2342`）：

| `impl_kind` | `spawn` | 结果 |
|---|---|---|
| `out_of_process_provider` | 缺失 | ❌ 拒绝："no `spawn` command is declared" |
| `out_of_process_provider` | 存在 | ✅ |
| `in_process_crate` | 存在 | ❌ 拒绝："`spawn` is declared but impl_kind is in_process_crate" |
| `in_process_crate` | 缺失 | ✅ |
| `attached_process` | 存在 | ❌ 拒绝（附着模块不由内核 spawn） |

文档大小上限：`MAX_YAML_BYTES = 64 * 1024`（64 KiB），解析前检查（`lib.rs:544`）。

### 1.3 最小示例（进程外，本文的主题形状）

```yaml
name: node-ref
version: "0.1.0"
route_namespace: /api/v1/node-ref
event_module: node-ref
data_dir: ~/.agent24/os/node-ref/
kernel_capabilities: [events, memory]
impl_kind: out_of_process_provider
spawn:
  command: node
  args: ["index.js"]
```

（`requires_models`/`requires_apis`/`requires_deps` 省略即 `[]`，与显式写 `[]` 等价；这正是
`examples/node-module/domain-os.yml` 的内容。）

### 1.4 全字段示例（进程外模块合法使用的每一个字段）

```yaml
name: sin90
version: "0.2.1"
route_namespace: /api/v1/sin90
event_module: sin90
data_dir: ~/.agent24/os/sin90/
requires_models: ["sin90/direction-v1"]
requires_apis: ["nostr"]
requires_deps: ["some-pkg>=1.0"]
kernel_capabilities: [events, models, scheduler, policy, memory, approval]
model_access: remote_allowed
ui_entry: ui/index.html
impl_kind: out_of_process_provider
spawn:
  command: node
  args: ["server.js", "--port", "0"]
manifest_version: 1
min_daemon_protocol: 1
```

（`host_commands` 有意省略——只对 `attached_process` 合法，这里会被拒绝。`kernel_capabilities`
里列出 `policy` 只是展示闭集，内核今天不对进程外模块授予 `Policy`，见 SPEC-ME3 §9。）

### 1.5 `manifest_digest`

```
manifest_digest(bytes) = "sha256:" + lowercase_hex(SHA256(bytes))
```

`rust/crates/agent24-os-proto/src/manifest.rs:56-65`，测试锚定 `manifest_digest(b"abc") ==
"sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"`（`manifest.rs:73-79`）。

内核在**发现包时**对磁盘上 `domain-os.yml` 的**原始字节**（UTF-8 解码、YAML 解析之前）算一次摘要
（`agent24-os-packages/src/discovery.rs:206,213`）。模块必须在 `initialize` 里报告**同一份字节**的摘要——
最简单的做法是模块启动时重新读一遍同一个文件并计算（见 `examples/node-module/index.js` 的
`manifestDigest()`），**不要**手写字符串常量：manifest 文件哪怕只改一个空格，摘要就会变。

### 1.6 模块侧的"精简读法"

模块构建自己的 `initialize` 请求只需要 manifest 里的三个字段：`name`、`route_namespace`、
`kernel_capabilities`（宽容读法，不校验其余字段——内核挂载时已经校验过一遍，模块没必要重新校验），
对应 `rust/crates/agent24-os-proto/src/manifest.rs` 的 `ManifestFacts`/`facts_from_yaml`。

---

## 2. 启动环境

内核 fork 之后、exec 模块程序之前，把下列环境变量交给子进程（`rust/crates/agent24-os-proto/src/launch.rs:28-50`）：

| 变量 | 内容 | 备注 |
|---|---|---|
| `A24_LISTEN_FD` | 固定字符串 `"3"` | 入站 HTTP 监听 fd 的编号，见 §2.2 |
| `A24_CALLBACK_SOCK` | 文件系统路径（字符串） | 回调 socket 的路径，模块自己 `connect()`，见 §3 |
| `A24_HANDSHAKE_TOKEN` | 64 个十六进制字符（32 字节，`/dev/urandom` 现铸） | 每次 spawn 轮换，只走环境变量，从不进 argv（`ps` 可见），`launch.rs:1033-1064` 有专门测试证明这一点 |
| `A24_DATA_DIR` | 文件系统路径（字符串） | 模块自己的数据目录，等于 manifest 里 `data_dir` 展开 `~` 之后的路径 |

**恰好这四个 `A24_*` 变量**，不多不少（`launch.rs:1119-1129` 的测试断言）。

**其余环境变量**：先 `env_clear()`，再只透传一个白名单——`INHERITED_ENV = ["PATH", "HOME",
"LANG", "TZ", "TMPDIR", "USER"]`（`launch.rs:50`）外加任何 `LC_*` 前缀的变量（`launch.rs:52-55`）。
daemon 自己的凭据（provider API key、云令牌等）**不会**出现在子进程环境里。

**工作目录**：spawn 时 `.current_dir(<包目录>)`（`launch.rs:560`）——`spawn.command`/`spawn.args`
里的相对路径（如本参考实现的 `node index.js`）以及模块运行时打开的相对路径（`domain-os.yml`、
静态资源）都相对包目录解析。

**跳板（trampoline）**：内核经一个跳板进程 exec 到最终模块程序，跳板会额外设置 `A24_TRAMPOLINE=1`，
但在最终 `exec()` 之前会 `env_remove` 掉它（`launch.rs:317,427,567`）——**模块程序本身永远看不到
`A24_TRAMPOLINE`**，无需处理。

### 2.2 fd 3：入站 HTTP 监听 socket

- **已经是一个 Unix 域、`SOCK_STREAM`、处于 `listen()` 状态的 socket**——内核已经 `bind()` 并
  `listen()` 过了；模块**只需要 `accept()`**，不调用 `bind()`/`listen()`。
- 内核在子进程拿到 fd 之后立即关闭自己那份拷贝（`launch.rs:459-460,581-582`）——模块一死，新连接
  立即被拒绝（不会排队进一个没人 accept 的 backlog）。
- **Node 的接管方式**：`http.createServer(handler).listen({ fd: 3 })`——Node 的 `net`/`http` 服务器
  原生支持"接管一个已存在、已绑定的 fd"这个用法，不需要额外的 `FD_CLOEXEC`/非阻塞设置（那是 Rust
  SDK 为了配合 tokio reactor 才做的内部处理，`agent24-os-fd/src/lib.rs:67-70`，Node runtime 自己管理）。
- 本仓 Rust 侧的 `take_inherited_listener()` 只能调用一次（进程级原子标记）——这是**该 Rust crate 自己
  的防误用护栏**，不是内核对协议的要求；用其他语言实现时没有等价物需要复刻，读一次 `fd=3` 即可。

### 2.3 `A24_CALLBACK_SOCK`：出站回调 socket

一个**文件系统路径字符串**（不是继承的 fd）——模块自己 `connect()` 到这个路径，建立回调通道。
详见 §3。

---

## 3. 回调连接与 `initialize` 握手

### 3.1 连接与"每代恰好一条"（D1）

内核对这一代（一次进程运行）的回调 socket **只 accept 一次**；接到后监听立即关闭、路径删除。
**这条连接是这一代的生命线**：它断开，这一代就结束——**同一代不允许重连**。模块读到这条连接的
EOF 必须退出（见 §7），不能尝试重新 `connect()`。

（`rust/crates/agent24-os-proto/src/endpoint.rs:11-23` 的 `CallbackListener::accept_one`
doc 明确引用"用户裁决 D1"。）

### 3.2 首帧必须是 `initialize`

握手请求（`InitializeRequest`，`rust/crates/agent24-os-proto/src/initialize.rs:82-94`）：

```json
{
  "jsonrpc": "2.0",
  "id": "1",
  "method": "initialize",
  "params": {
    "protocol_versions": {"min": 1, "max": 1},
    "module": "node-ref",
    "manifest_digest": "sha256:...",
    "auth_token": "<A24_HANDSHAKE_TOKEN 的值>",
    "capabilities": ["events", "memory"]
  }
}
```

`params`（`InitializeParams`，`initialize.rs:57-79`，**`deny_unknown_fields`，重复 JSON key 被拒**）：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `protocol_versions` | `{min: u32, max: u32}` | **实质必填**——缺失按不兼容处理，不当作"假定 v1" | 模块支持的协议版本区间 |
| `module` | string | ✅ | 必须等于内核 spawn 这个进程时用的 manifest 里的 `name`；不符 → `manifest_mismatch` |
| `manifest_digest` | string | ✅ | 必须等于内核对同一份 manifest 算出的摘要；不符 → `manifest_mismatch` |
| `auth_token` | string | ✅ | 必须等于 `A24_HANDSHAKE_TOKEN`；不符 → `auth_failed` |
| `capabilities` | `[string]` | 否（缺省 `[]`） | 模块请求的能力；内核只会**给交集**（manifest 请求 ∩ 内核愿意给），见 §5 开头的 offer 语义 |

**今天的协议版本区间只有 `1..=1`**——`protocol_versions: {"min": 1, "max": 1}` 是唯一能协商成功的值。

成功响应（`InitializeResult`，`initialize.rs:150-157`）：

```json
{"jsonrpc": "2.0", "id": "1", "result": {"protocol_version": 1, "offer": {"provides": ["_a24/events/", "_a24/memory/private"]}}}
```

- `protocol_version`：协商出的**唯一**版本（`min(模块.max, 内核.max)`，取交集里的那个值；内核绝不会
  回一个模块没声明支持的版本）。
- `offer.provides`：本连接**被授权调用**的方法族前缀列表（不等于"内核有没有这个 handler"——见
  `initialize.rs:96-121` 的 doc：一个模块没被授予某能力时，即使 handler 存在，`offer` 里也不会出现
  对应前缀）。匹配方式是**无界前缀匹配**（`str::starts_with`），例如 `"_a24/memory/private"` 会匹配
  `"_a24/memory/private/remember"`。这是握手时的**提示**，不是授权检查本身——真正的检查是每个方法
  `Handler::call()` 内部各自的 `Grants` 校验。

### 3.3 握手失败：错误码 + 断连

| 情形 | JSON-RPC code | `error.data.kind` | 断连？ |
|---|---|---|---|
| 首帧不是合法 JSON | `-32700` | 无 | ✅ 断连，**不回响应行**（`endpoint.rs` 短路：先于 `initialize::accept` 就失败） |
| 首帧不是 `initialize`（方法名错、缺字段导致解析失败、非对象等） | `-32600` | 无 | ✅ 断连 |
| `params` 解析失败（缺字段、未知字段、重复 key，如重复 `auth_token`） | `-32602` | 无 | ✅ 断连 |
| `auth_token` 不匹配 | `-32000` | `auth_failed` | ✅ 断连（消息**不**描述 token 长度/内容，避免成为 oracle） |
| `module`/`manifest_digest` 与内核记录的不符 | `-32000` | `manifest_mismatch` | ✅ 断连 |
| 版本区间无交集，或模块未声明区间 | `-32000` | `version_mismatch`，`data` 里带 `module`/`kernel` 两个区间 `{min,max}` | ✅ 断连 |

**校验顺序是先形状后秘密**：一个帧既不是合法 `initialize` 又带错 token，回的是 `-32600`，
不会先检查 token（`initialize.rs:791` 测试 `the_shape_is_checked_before_the_token`）。
**校验内容顺序**：`module`/`manifest_digest` 先于 `auth_token`（内核校验的是自己 spawn 的进程报的
digest 是不是自己给的那份，这一步不泄露秘密，可以放在 token 之前）。

**握手期任何失败都断连，且不给同一代重试的机会**（D1）——要重连，只能等内核重启出一个新的代
（新进程、新 token、新握手）。内核在断连前会**先回一行错误**（带模块发来的 id；id 不可用时为
`null`）。**这与握手成功之后的错误处理不同**——握手成功之后同一连接上的普通调用失败只失败该次调用，
连接继续（见 §4）。

**大小限制**：首帧同样受 §4 的单帧上限约束（1 MiB）——超限时**直接断连、不回任何响应**（无法解析
就无法知道该回什么 id）。

---

## 4. 帧格式与 RPC

### 4.1 NDJSON framing

一行一个 JSON 值，`\n` 结尾（不含 `\n` 本身）。`MAX_FRAME_BYTES = 1024 * 1024`（1 MiB，
`rust/crates/agent24-os-proto/src/frame.rs:84`）。

- **超限行**：读到超过上限时立即拒绝——**握手期与握手后行为一致**：都是断连、不回任何响应行
  （超长行意味着流已经不同步，没有可信的 id 可以回；见 SPEC §8「超长行：直接断连，不先回一条错误」）。
- 一行没有 `\n` 就遇到 EOF：视为 `Eof`，不是一个完整帧。

### 4.2 请求信封

```
{"jsonrpc": "2.0", "id": "<string>", "method": "<string>", "params": {...}}
```

**只允许这四个顶层成员**；出现任何其他成员 → `-32600`（`rust/crates/agent24-os-proto/src/rpc.rs:596-604`）。

| 规则 | 结果 |
|---|---|
| `id` 不是字符串 | `-32600`，响应 `id: null` |
| `id` 超过 256 字节（`MAX_ID_BYTES`） | `-32600`，响应 `id: null`（即使它是字符串也不回显） |
| 缺 `id` | 视为 **notification**，永不回复（除非 `method == "$/cancelRequest"`，见 §4.5） |
| 顶层出现重复 key（如两个 `"id"`） | `-32600`，响应 `id: null`（哪个是"真的"本身就是歧义，不猜） |
| `params` 内部（任意深度）出现重复 key | `-32602`，`id` 正常回显 |
| 顶层重复与 params 内部重复同时出现 | 顶层的判定赢（`-32600`，`id: null`） |
| **这个 `id` 当前仍在途（同连接未结束）** | `-32600`，响应 `id: null` **且** `error.data.duplicate_id = "<原 id>"`——**这条检查排在所有其它信封校验之前** |
| 一个已完成的 `id` 被复用 | 允许，正常处理为新请求 |

顶层信封字段的语义错误一律是 `-32600`，**不是** `-32602`（`params` 本身解析失败才是 `-32602`）。

### 4.3 并发与并发上限

- 允许并发在途，**响应可乱序**——调用方按 `id` 自己配对。
- 每连接**同时在途上限 64**（`MAX_IN_FLIGHT_PER_CONNECTION`，`rpc.rs:60-73`）。超出**当场拒绝**，
  不排队：`-32000` + `data.kind: "busy"`，消息形如 `"64 calls are already in flight on this connection"`。

### 4.4 超时

- 连接级默认：**`CALL_TIMEOUT = 30s`**（`rpc.rs:75-82`）。
- 方法可声明自己的上限（不得超过 `MAX_METHOD_CALL_TIMEOUT = 300s`）；今天唯一声明了非默认超时的方法
  是 `_a24/model/complete = 120s`（§5.5）。
- 到点 → `-32000` + `data.kind: "timeout"`，**不重试**（回调可能已有副作用）。
- 另有一条**写响应超时** `WRITE_TIMEOUT = 10s`——对端（模块）如果不读走内核写回来的响应，整条连接
  会被内核判定为对端不读、直接断开，防止模块拖死内核的写循环。这条是内核→模块方向的自我保护，
  模块侧只需正常读取自己 socket 上的数据即可，无需特别处理。

### 4.5 取消：`$/cancelRequest`

```json
{"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": "h"}}
```

- 这是一条 **notification**（**没有顶层 `id` 字段**）——带 `id` 发送会被拒（`-32600`，
  `"$/cancelRequest is a notification; send it without an id"`）。
- `params` 只允许 `{id, _meta}` 两个成员；出现其它成员或该 notification 本身格式不对 → 静默丢弃
  （notification 从不回复，格式错误也一样）。
- 被取消的目标请求**仍然会收到一个响应**——`-32000` + `data.kind: "cancelled"`，消息
  `"cancelled by $/cancelRequest; any side effect already committed stays"`。
- 若目标 `id` 早已完成/不存在/属于另一代，取消本身是 notification，无从得知也无需处理——静默无效果。
- 取消是**尽力而为**：已经提交的副作用不回滚。

### 4.6 响应信封

成功：
```json
{"jsonrpc": "2.0", "id": "<string>", "result": <value>}
```
错误：
```json
{"jsonrpc": "2.0", "id": "<string|null>", "error": {"code": <i32>, "message": "<string>", "data": {"kind": "<string>", ...}?}}
```
`data` 字段只在有内容时出现（`kind` 不为空且/或有其它诊断字段时），不会出现空对象。

### 4.7 标准 JSON-RPC 协议码

```
-32700  Parse error       （帧不是合法 JSON；握手期与握手后均使用）
-32600  Invalid Request   （信封不合法；见 §4.2）
-32601  Method not found  （方法名内核完全不认识——闭集之外，见下）
-32602  Invalid params    （params 解析失败；固定不进 handler，不触发任何业务副作用）
-32603  Internal error    （内核自身缺陷/存储故障；消息不泄露内部路径/SQL/token）
-32000  Application error （业务失败，永远配一个 error.data.kind，见 §8 闭集）
```

**`-32601`（未知方法）与 `-32000 forbidden`（已知方法但未获得能力授权）是两回事**，必须能区分：
前者说"这个 daemon 根本没有这个方法"，后者说"这个方法存在，但你没有被授权调用它"。一个模块靠这个
区分「协议版本太旧、这个方法族还不存在」与「我的 manifest 忘了申请这个能力」。

---

## 5. 回调方法

每个方法的 `params` 结构体在 Rust 侧都是 `#[serde(deny_unknown_fields)]`（安全边界，见
SPEC-ME3 §3「方法参数对象一律 deny_unknown_fields」）——塞入任何协议未定义的字段（包括
`lease`/`scope`/`org`/`space` 这类看似合理的字段）一律 `-32602`，**不会被静默忽略**。唯一容忍未知
内容的位置是显式声明的 `_meta` 字段（`params._meta`，一个内部宽容的对象），且 **`_meta` 从不参与
授权与分区判断**——往 `_meta` 里塞 `org`/`space`/`lease` 没有任何效果（各方法小节的测试已验证）。

### 5.1 `_a24/events/emit`

进程内对应：`EventSink`。所需能力：`events`。

**params**（`EventsEmitParams`，`rust/apps/agent24d/src/events_emit.rs:249-256`）：

| 字段 | 类型 | 必填 |
|---|---|---|
| `kind` | string | ✅ |
| `payload` | JSON object | ✅ |
| `request_id` | string | 否（缺省不发送，不是 `null`） |

**没有 `module` 字段**——事件的归属模块永远来自这条连接自己的身份（握手时确定），协议里根本不存在
一个可以让模块"冒充别的模块发事件"的字段。

**result**：`{}`（空对象）。

**真实例子**（`rust/apps/agent24d/tests/me3f_blackbox.rs:162`，模块侧真实调用）：
```json
// → request
{"jsonrpc":"2.0","id":"7","method":"_a24/events/emit",
 "params":{"kind":"task.transitioned","payload":{"probe":"t9"}}}
// ← response
{"jsonrpc":"2.0","id":"7","result":{}}
```

**限流**：token bucket，容量 20、每秒回填 5（per-generation）。**负载上限**：≤256 节点 / ≤8192 字节
字符串（自己的遍历计数，不是序列化字节数）。

**错误**：`forbidden`（无 `events` 授权）、`not_ready`/`draining`/`revoked`（生命周期）、
`rate_limited`、`payload_too_large`、`-32602`（`kind` 不是合法字符串等）、`timeout`（绑定的
`request_id` 生命周期到期）、`internal`（内核自身接线问题）。

### 5.2 `_a24/memory/private/{remember,recall,recent}`

进程内对应：`ScopedMemory::{remember,recall,recent}`。所需能力：`memory`。**只认连接身份**——
这三个方法**拒绝**任何 `lease`/`scope`/`org`/`space` 字段（`deny_unknown_fields` 结构性保证，
`rust/apps/agent24d/src/memory_callback.rs:34-68`），不存在"跨空间"的用法；后台任务（不绑定任何
在途请求）一样可以调用这三个方法，写自己的私有分区。

#### `remember`

**params**：

| 字段 | 类型 | 必填 | 限制 |
|---|---|---|---|
| `kind` | string | ✅ | 1–128 字节 |
| `body` | JSON object | ✅ | 序列化后 ≤ 65536 字节（64 KiB） |
| `request_id` | string | 否 | — |

**result**（`Remembered`，`rust/crates/agent24-domain/src/memory.rs:94-97`）：`{"id": "<字符串>", "at": "<ISO-8601>"}`。
`id` 形如 `"osmem:<ULID>"`，内核铸造。

**真实例子**（`me3f_blackbox.rs:116`，请求为真实 fixture；响应形状经断言 `me3f_blackbox.rs:513-517` 确认）：
```json
// → request
{"jsonrpc":"2.0","id":"1","method":"_a24/memory/private/remember",
 "params":{"kind":"t9-note","body":{"text":"t9-blackbox"}}}
// ← response
{"jsonrpc":"2.0","id":"1","result":{"id":"osmem:01J...","at":"2026-09-29T12:00:00.000Z"}}
```

#### `recall`

**params**：

| 字段 | 类型 | 必填 | 限制 |
|---|---|---|---|
| `query` | string | ✅ | 归一化为 `trim().lowercase()`；空字符串匹配全部 |
| `page_size` | 非负整数 | ✅ | 1..=50（`MEMORY_MAX_PAGE_SIZE`），无隐式缺省，**每次必须显式提供** |
| `cursor` | string | 否 | 上次响应里的 `cursor`（不透明字符串，见下） |
| `request_id` | string | 否 | — |

**result**（`RecallPage`）：`{"items": [{"id","kind","body","at"}, ...], "cursor": "<字符串或 null>"}`。
`cursor: null` 表示没有更多数据。

**真实例子**（`me3f_blackbox.rs:117`，响应形状经断言 `:518-529` 确认）：
```json
// → request
{"jsonrpc":"2.0","id":"2","method":"_a24/memory/private/recall",
 "params":{"query":"t9-note","page_size":10}}
// ← response
{"jsonrpc":"2.0","id":"2","result":{
  "items":[{"id":"osmem:01J...","kind":"t9-note","body":{"text":"t9-blackbox"},"at":"2026-09-29T12:00:00.000Z"}],
  "cursor":null}}
```

#### `recent`

**params**：同 `recall` 减去 `query`（`{page_size, cursor?, request_id?}`）。**result**：同
`RecallPage` 形状。（构造示例，仓内测试目录未见到字面 fixture）：
```json
// → request
{"jsonrpc":"2.0","id":"3","method":"_a24/memory/private/recent","params":{"page_size":20}}
// ← response
{"jsonrpc":"2.0","id":"3","result":{"items":[{"id":"osmem:01J...","kind":"note","body":{"text":"..."},"at":"2026-09-29T12:00:00.000Z"}],"cursor":"eyJ2MjoxMjozZjIxYT..."}}
```

**cursor 机制**：不透明 token。明文形状 `"v2:<seq:i64>:<fp:016x>"`——`seq` 是上次解析到的内部序号，
`fp` 是对 `方法标签 + 归一化 query` 做的 64-bit FNV-1a 指纹；wire 上再做 base64url-no-pad 编码。
**模块必须把它当纯粹的不透明字符串对待**——把一个 `recall` 的 cursor 传给 `recent`（反之亦然）会因
指纹不匹配被拒（`-32602`）。

**限额**：每次调用扫描行数上限 2000；单页响应字节预算 512 KiB（超预算的记录不放进这一页，游标
提前停在它之前，不是"先拼完再截断"）。分区（按 `owner`）默认配额：200000 行 / 256 MiB。

**限流**：三个方法共享一个 token bucket；每调用扣费——`remember`=1，`recall`=2000（按最坏情形扫描
预算），`recent`=`page_size`。

**错误**（remember/recall/recent 共通）：`-32602`（字段超限、`page_size` 越界、cursor 不可解码/跨
方法误用）、`forbidden`（无 `memory` 授权）、`not_ready`/`draining`/`revoked`、`rate_limited`、
`timeout`（排队等待共享准入时所绑请求已结束或预算耗尽）、`quota_exceeded`（仅 `remember` 的写入触发）、
`internal`（存储故障，消息不含内部标识符）。

### 5.3 `_a24/approval/{gate,advise,status}`

进程内对应：`ApprovalRequester`。所需能力：`approval`。

**关键概念（§6.1 的收窄）**：`gate` = 副作用**归内核所有**（写非私有分区、注册 schedule 等）的
"真门"——批准后由内核自己执行；`advise` = 副作用**归模块自己域内**（模块自己的 DB、模块自己的外部
调用）——只呈现给用户看、记录用户的答复，**模块可以无视这个答复**，这不是安全控制，只是知情权。

**`gate` 内核可执行动作闭集（今天只有一项）**：`"schedule_callback"`——`target` 必须是可解析的
RFC3339 时间戳（会被规范化为秒精度，内核据此在未来某个时刻回调模块，配合 §5.4 的调度机制）。
任何其它 `action` 一律 `forbidden`。

**params**（gate 与 advise 共用 `ApprovalSubmitParams`，`rust/apps/agent24d/src/approval_callback.rs:32-43`）：

| 字段 | 类型 | 必填 |
|---|---|---|
| `action` | string | ✅ |
| `target` | string | 否（`schedule_callback` 要求它是合法时间戳） |
| `payload` | JSON value | ✅ |
| `request_id` | string | ✅——**必须**是模块从自己收到的代理请求头 `X-A24-Request-Id` 读出来的值（见 §6.2），不是自造 |
| `approval_token` | string | ✅——同上，从 `X-A24-Approval-Token` 读出来的值 |

**result**（`ApprovalAnswer`）：
```
{"approval_id": "<字符串>", "kind": "gate"|"advise", "binding": <bool>, "decision": "pending"|"approved"|"denied"|"timed_out", "executed_at": "<ISO-8601>|null"}
```
`binding` 恒等于 `kind == "gate"`（`gate` 提交成功即 `binding: true`；`advise` 恒为 `false`）。

**真实例子**（`me3f_blackbox.rs:146-161`，请求为真实 fixture；负例证明拒绝不消耗真 token）：
```json
// → 错 token（负对照）
{"jsonrpc":"2.0","id":"2","method":"_a24/approval/gate",
 "params":{"action":"schedule_callback","target":"2099-01-01T00:00:00Z","payload":{},
           "request_id":"<X-A24-Request-Id 头的值>","approval_token":"definitely-not-the-real-token"}}
// ← response
{"jsonrpc":"2.0","id":"2","error":{"code":-32000,
 "message":"request_id/approval_token did not admit this submission","data":{"kind":"token_invalid"}}}

// → 同一 request_id，真 token
{"jsonrpc":"2.0","id":"3","method":"_a24/approval/gate",
 "params":{"action":"schedule_callback","target":"2099-01-01T00:00:00Z","payload":{},
           "request_id":"<同上>","approval_token":"<X-A24-Approval-Token 头的值>"}}
// ← response
{"jsonrpc":"2.0","id":"3","result":{"approval_id":"<hex>","kind":"gate","binding":true,
 "decision":"pending","executed_at":null}}
```

`advise` 示例（`rust/crates/agent24-os-sdk/src/clients/approval.rs:141-179`）：
```json
// → request
{"jsonrpc":"2.0","id":"5","method":"_a24/approval/advise",
 "params":{"action":"award_points","payload":{"award_id":"a1"},
           "request_id":"req-1","approval_token":"secret-1"}}
// ← response（立即返回，不等人工决定）
{"jsonrpc":"2.0","id":"5","result":{"approval_id":"appr-1","kind":"advise","binding":false,
 "decision":"pending","executed_at":null}}
```

`status`（**params 只需** `{"approval_id": "<字符串>"}`，不查生命周期/在途状态，是独立只读查询）：
```json
{"jsonrpc":"2.0","id":"8","method":"_a24/approval/status","params":{"approval_id":"appr-2"}}
// ←
{"jsonrpc":"2.0","id":"8","result":{"approval_id":"appr-2","kind":"gate","binding":true,
 "decision":"approved","executed_at":"2026-01-01T00:00:00Z"}}
```
跨模块或不存在的 `approval_id` 一律 `not_found`（故意不区分，防止枚举探测别的模块）。

**错误**：`forbidden`（无 `approval` 授权，或 `gate` 的 `action` 不在闭集内）、`-32602`（`gate` 的
`target` 缺失/不可解析）、`token_invalid`（`request_id`/`approval_token` 配对失败——过期、跨连接、
跨代、错误、已用过，统一一个 kind，不给攻击者区分是哪一半错了）、`not_ready`/`revoked`、
`not_found`（仅 `status`）、`internal`。

### 5.4 `_a24/scheduler/{upsert,delete,list}`

进程内无对应物（ME-4a 新增）。所需能力：`scheduler`。

#### `upsert`

**params**（`SchedulerUpsertParams`，`rust/apps/agent24d/src/scheduler_callback.rs:107-125`）：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `key` | string | ✅ | `[a-z0-9._-]{1,128}`，字节精确，不折叠大小写 |
| `spec` | tagged union | ✅ | 见下 |
| `enabled` | bool | 否 | 缺省 `true`——**是目标状态语义，不是"保持原样"** |
| `label` | string | 否 | 缺省回落到 `key`；1..=128 字符，禁控制字符与 Unicode 双向控制字符（Trojan-Source 防护） |
| `request_id` | string | 否 | — |

`spec`（内部按 `"type"` 打标签，`deny_unknown_fields`）：
```json
{"type": "cron", "expr": "0 9 * * *", "tz": "Asia/Shanghai"}   // tz 可选
{"type": "every", "secs": 3600}                                  // secs 下限 60
{"type": "at", "ts": "2030-01-01T00:00:00Z"}
```
cron 规则：恰好 5 段（无秒段，即便 REST 路径接受 6 段这里也不接受）；星期字段只接受 `*` 或
`SUN`..`SAT` 英文缩写（大小写不敏感）或它们的范围，**拒绝数字**（POSIX 与 cron 引擎对 0/7 的含义
不一致）；日字段与星期字段不能同时受限（POSIX 是 OR、内核引擎是 AND）；`expr` ≤128 字节，`tz` ≤64
字节且必须是精确大小写的合法 IANA 时区名；日历上不可能出现的组合（如 2 月 30 日）在 upsert 时就被
拒绝（`at` 类型例外——已过期的一次性时间戳仍合法，配合幂等对账）。

**result**：`{"outcome": "created"|"updated"|"unchanged", "schedule": <ModuleScheduleState>}`。

`ModuleScheduleState`：
```
{key, spec, enabled, label, user_suspended, system_disabled_reason,
 next_run_at, last_fire: {tick: <LastFire|null>, run_now: <LastFire|null>}}
```
`LastFire = {fire_id, scheduled_for, status, last_error}`，`status` ∈
`pending|deferred|delivered|failed|expired`。

**真实例子**（`rust/apps/agent24d/tests/me4_scheduler_blackbox.rs:159`；result 形状取自 SDK 单测
`agent24-os-sdk/src/clients/scheduler.rs:180-207`）：
```json
// → request
{"jsonrpc":"2.0","id":"1","method":"_a24/scheduler/upsert",
 "params":{"key":"k1","spec":{"type":"every","secs":3600},"enabled":true}}
// ← response
{"jsonrpc":"2.0","id":"1","result":{"outcome":"created","schedule":{
  "key":"k1","spec":{"type":"every","secs":3600},"enabled":true,"label":"k1",
  "user_suspended":false,"system_disabled_reason":null,
  "next_run_at":"2026-01-01T01:00:00Z","last_fire":{"tick":null,"run_now":null}}}}
```

**配额/限流**：每模块 256 个 key（`MODULE_SCHEDULE_QUOTA`），第 257 个新 key → `quota_exceeded`
（已有 key 仍可正常 upsert）。token bucket 容量 300、每秒回填 1——足够一次性把 256 个 key 全量对账
一遍，紧接着的第二次全量对账需要退避重试。

**错误**：`forbidden`（无 `scheduler` 授权）、`-32602`（key/spec/label 校验失败）、`quota_exceeded`、
`rate_limited`、`timeout`（带 `request_id` 但该请求已不在途，`retryable: false`）、`internal`。

#### `delete`

**params**：`{"key": "<字符串>", "request_id"?, "_meta"?}`（key 用同一套正则校验，格式错也是 `-32602`）。
**result**：`{"outcome": "deleted"|"absent"}`。只作用于**自己**的 key——删别的模块的 key 一律
`absent`，与"从未存在"不可区分（不泄露别的模块有没有这个 key）。

#### `list`

**params**：`{"request_id"?, "_meta"?}`。**result**：`{"schedules": [<ModuleScheduleState>, ...]}`，
按 `key` 排序，最多 256 条（= 配额），不分页——供模块一次性对账整个期望状态。

### 5.5 `_a24/model/complete`

进程内无对应物（`KernelCtx` 没有模型句柄；ME-4b 新增）。所需能力：`models`。manifest 字段
`model_access` 决定隐私路由（§1.2）——由**调用方在 manifest 里静态声明**，不是每次调用可选的字段。

**params**（`ModelCompleteParams`，`rust/apps/agent24d/src/model_callback.rs:67-81`，
`deny_unknown_fields`）：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `messages` | `[{role: "system"\|"user"\|"assistant", content: string}]` | ✅ | 1..=64 条 |
| `response_format` | `{"type":"json_schema","json_schema":{name,schema,strict?}}` | 否 | — |
| `max_tokens` | u32 | 否 | 1..=4096，缺省 1024 |
| `complexity` | `"simple"\|"complex"` | 否 | 仅是偏好，最终选哪个 provider 由内核决定 |
| `request_id` | string | 否 | — |

**没有任何字段能改隐私、选具体模型或开 tools**——`privacy`/`model`/`tools`/`provider` 都不是这个
结构体的字段，出现即 `-32602 "unknown field"`。`_meta` 里塞类似字段解析虽能通过但**完全无效**。

**result**（`ModelCompleteResult`）：
```
{"text": "...", "model_id": "<字符串>|null", "tier": "local"|"remote",
 "usage": {"prompt_tokens": <u32>, "completion_tokens": <u32>}}
```

**真实例子**（SDK 单测 `agent24-os-sdk/src/clients/model.rs:165-195`）：
```json
// → request
{"jsonrpc":"2.0","id":"9","method":"_a24/model/complete",
 "params":{"messages":[{"role":"user","content":"hi"}]}}
// ← response
{"jsonrpc":"2.0","id":"9","result":{"text":"hello back","model_id":"stub-7b","tier":"local",
 "usage":{"prompt_tokens":3,"completion_tokens":2}}}
```

**`unavailable` 错误的 `data` 闭集**：
```
{"retryable": <bool>, "cause": "no_provider"|"request_rejected"|"backend_config"|"response_too_large"}
```
只有 `no_provider` 的 `retryable` 为 `true`；其余三个恒为 `false`。

**方法专属超时**：120 秒（比连接级默认的 30 秒长，`Handler::call_timeout()` 声明值）。

**并发/限流**：每模块同时在途 ≤2，全 daemon 所有模块合计 ≤4——超出当场 `busy`，不排队。token
bucket 容量 30、每秒回填 0.5（约每分钟 30 次）。

**错误**：`forbidden`（无 `models` 授权）、`-32602`（出现未声明字段，或字段值越界）、`busy`、
`rate_limited`、`timeout`（带 `request_id` 但已不在途）、`unavailable`（见上）、`cancelled`
（daemon 停机时的模型调用取消根被触发）。

---

## 6. 内核 → 模块：HTTP 代理与 fired 投递

### 6.1 客户端请求 → 模块（受约束代理，不是原样转发）

客户端经 `Authorization: Bearer` 鉴权后，内核把 `/api/v1/<ns>/*` 的请求代理给模块。**转发的路径是
完整路径，含 `/api/v1/<ns>` 前缀，不剥离**——模块自己的路由必须匹配这个完整路径（不像进程内模块
那样被 `nest` 剥掉前缀）。

**内核从客户端请求里剥掉、模块永远看不到**：
- `Authorization`、`Cookie`、`Host`（换成合成值，见下）、`Expect`、`Forwarded`、`X-Real-IP`；
- hop-by-hop 头族：`Connection`/`Keep-Alive`/`TE`/`Trailer`/`Transfer-Encoding`/`Upgrade`/
  `Proxy-Authenticate`/`Proxy-Authorization`/`Proxy-Connection`/`Content-Length`；
- **前缀匹配**：任何 `x-a24-*`（客户端伪造的内核头）、任何 `x-forwarded-*`（整个变体族，含
  `X-Forwarded-Ssl`/`-Port`/`-Scheme` 这类不那么显眼的）。

**内核注入、模块只能读**：
- `X-A24-Request-Id`：非秘密相关性 ID，可进日志；
- `X-A24-Approval-Token`：秘密、不可猜、单次使用、绑定该请求——模块要提交 `_a24/approval/gate`/
  `advise` 时，把这个头的值原样作为 `approval_token` 参数传回去（同时把 `X-A24-Request-Id` 的值作为
  `request_id` 参数传回去，见 §5.3）。

`Host` 头被替换成固定合成值 `agent24-module.invalid`（一个永不可解析的 RFC 2606 保留域名，与实际
监听地址无关，防止泄露内核内部状态）。

**响应侧同样受约束**——从模块的响应剥掉：`Set-Cookie`、`WWW-Authenticate`、`Authorization`、
`Authentication-Info`、`Proxy-Authentication-Info`、`Refresh`、hop-by-hop 头族，以及任何
`x-a24-*`（模块试图回显一个内核秘密头会被拦下）。`Location` 头**不是**直接剥掉，而是校验：必须
恰好一个值，且解析后必须落在本模块自己的命名空间内——校验失败（越界、重复、带 scheme 的绝对 URL、
含 `%2e`/反斜杠/控制字符等）时**整个响应变成 502**，不是"把 Location 删掉再放行"。

### 6.2 保留路径 `/api/v1/<ns>/_a24/...`

任何客户端（不是内核自己）请求这个前缀下的路径，一律 **404**（规范化后判定，能挡住编码变体如
`%5fa24`、`..;`、`//` 等绕过尝试；无法明确规范化的路径 → **400 `invalid_request_path`**，fail-closed）。
这个判定发生在请求被计入在途、发给模块之前——一个被拒的保留路径请求，模块进程连一个字节都看不到。

### 6.3 限制与超时

| 项 | 值 |
|---|---|
| 请求体/响应体上限 | 1 MiB（`MAX_BODY_BYTES`） |
| 总时限（从读客户端 body 起算） | 30 秒 |
| 首字节时限 | 10 秒（与总时限取较小者生效） |
| 每模块并发上限 | 64 个在途代理请求，超出当场 `503 module_overloaded`，不排队 |

### 6.4 错误状态码 / `error.code` 对照

响应体形状：`{"error": {"code": "<字符串>", "message": "<字符串>", "hint"?, "details"?}}`（`hint`/
`details` 缺省时整个字段不出现，不是 `null`）。

| 情形 | HTTP 状态 | `error.code` |
|---|---|---|
| 上游不可达/地址解析失败 | 502 | `upstream_unavailable` |
| 响应中途断开 | 502 | `upstream_unavailable` |
| 响应超过 1 MiB | 502 | `upstream_response_too_large` |
| 模块响应是 `101`/`text/event-stream`（流式，本轮不支持） | 502 | `upstream_streaming_unsupported` |
| `Location` 越命名空间/重复 | 502 | `upstream_location_rejected` |
| 客户端请求保留路径 | 404 | `not_found` |
| 请求路径无法规范化 | 400 | `invalid_request_path` |
| 客户端 body 未收完 / 模块未开始应答 / 模块未答完 | 504 | `upstream_timeout` |
| 并发上限 64 已满 | 503 | `module_overloaded` |
| 模块尚未就绪（握手未完成） | 503 | `module_not_ready` |
| 模块正在 drain | 503 | `module_draining` |
| 模块正在/已经停止 | 503 | `module_stopping`（熔断/停止失败/被杀等子情形各有更细的 code，如 `circuit_breaker_tripped`/`stop_failed`/`module_panicked`/`module_killed`） |
| 停机期间被放弃的在途请求 | 503 | `request_abandoned` |

**真实例子**（`rust/apps/agent24d/tests/a3_3_host_commands_blackbox.rs:583-584`）：
```json
// 模块尚未挂载/未就绪
// HTTP 503
{"error":{"code":"module_not_ready","message":"..."}}
```

### 6.5 内核主动发起：fired 投递（`_a24/scheduler/*` 的配套）

调度到点时，内核经**回调通道所在的同一条上游连接**（不是走客户端代理路径）向模块发送：

```
POST <route_namespace>/_a24/scheduler/fired
```

**body**（`FiredBody`，故意不是 `deny_unknown_fields`——内核以后加字段，旧模块仍能宽松解析）：
```json
{"key": "<模块自己的 key>", "trigger": "tick"|"run_now", "scheduled_for": "<ISO-8601>", "fired_at": "<ISO-8601>"}
```

**注入的头**：`X-A24-Schedule-Key`（= `key`）、`X-A24-Fire-Id`、`X-A24-Request-Id`、
`X-A24-Approval-Token`（后两者供该次投递期间的回调绑定使用）。

**`fire_id` 的确定性派生**：
```
"fire_" + hex(SHA256("agent24-fire-v2\0" || trigger || "\0" || schedule_id || "\0" || scheduled_for)[..16])
```
只由 `(trigger, schedule, scheduled_for)` 决定——**同一次 fire 的每次重试、崩溃后的续投都是同一个
`fire_id`、同一个 body**。**模块必须按 `fire_id` 去重**，收到重复 `fire_id` 时不要重复执行副作用
（例如不要为同一次 fire 重复提交审批）。

**时限与重试**：端到端每次尝试 10 秒；收到 2xx 响应头即算送达（响应体最多读 64 KiB 后丢弃）；
同一次 fire 最多尝试 3 次（含首次），失败间隔 5 秒、15 秒；模块不可用（未就绪/正在 drain/未安装）
时到点算**延迟**，不计入失败次数；只有模块 Running 且请求字节可能已经离开内核后收到非 2xx/超时/
连接错误才算一次真实失败尝试；连续 5 次失败会让内核置 `system_disabled_reason`（模块下次 upsert
或用户手动恢复都能清除）。超过 24 小时未送达的非终态投递行视为过期。

**承诺的粒度**：是"每个 schedule、每个触发来源（`tick`/`run_now` 分开）的**最新一次** fire 至少送达
一次"，不是"每个 slot 都送达"——较早的 fire 可能被同来源的更新取代、被模块改 spec/关闭覆盖，或超过
24 小时过期而不送达。`_a24/scheduler/list` 的 `last_fire.tick`/`last_fire.run_now` 可以查到每个来源
最近一次的去向。

**这条投递不占用 §6.3 的"每模块 64 并发代理请求"名额**——它走的是独立的投递泵（每模块并发上限 4、
全局 16），与客户端触发的代理请求分开计数。

---

## 7. 退出与断连语义

### 7.1 EOF 在回调 socket 上

`A24_CALLBACK_SOCK` 连接断开（EOF、读错误、或任何传输失败）意味着**这一代结束**（D1，§3.1）。
**模块必须停止服务并退出**，不能尝试重连——同一代不允许第二次 `connect()` 成功（内核那一侧的监听
在 accept 第一条连接后就已关闭）。继续运行意味着通过一条永远不会再传回任何内核调用结果的连接去
回答 HTTP 请求——没有意义,而且会与内核随后启动的新一代竞争同一个数据目录。

### 7.2 退出码

内核**不关心具体的退出码数值**——它只看进程是否已经不在了（`supervise::Exit{code, signal}` 只是
记录 `ExitStatus` 报告的内容，没有任何分支依据数值本身做决定）。Rust SDK/Sin90 的约定是退出码
`70`（"不是普通的 0 退出"，日志里容易识别），但这是**调用方约定**，不是协议要求。本参考实现在
EOF 时以 `0` 退出（正常、预期的关闭，不是崩溃）——两种做法内核都能正确处理；重要的是**及时退出**，
不是退出码的具体数值。

### 7.3 SIGTERM / SIGKILL

守护进程停止一个模块时：先 SIGTERM，等待一段宽限期（daemon 整体停机时默认 `A24_MODULE_STOP_GRACE_MS
= 500ms`，可调 100–5000ms；单模块级停止另有默认 3 秒的宽限），宽限期满后 SIGKILL 整个进程组。
daemon 整体停机时还有一段更早的 drain 期（默认 `A24_MODULE_DRAIN_MS = 800ms`，可调至 10 秒）——
这段时间内代理层不再准入新请求，但回调通道仍然服务在途请求的回调。**模块收到 SIGTERM 后应尽快
flush 掉能 flush 的状态并退出**——默认参数下从 SIGTERM 到 SIGKILL 大约有几百毫秒到几秒的窗口，
具体取决于部署方的调参。

---

## 8. 错误闭集表（附录）

`error.data.kind` 是一个**闭集**，今天恰好 18 个值（`ErrorKind::ALL`，
`rust/crates/agent24-os-proto/src/rpc.rs:166-186`）。JSON-RPC code 全部是 `-32000`（协议层的
`-32601`/`-32602` 等不带 `kind`）。

| kind | 典型触发场景 |
|---|---|
| `forbidden` | 方法存在但连接未被授予对应能力；`gate` 的 `action` 不在内核可执行闭集内 |
| `busy` | 连接在途请求数已达 64（§4.3）；或 `_a24/model/complete` 撞上并发上限（§5.5） |
| `cancelled` | 目标请求被 `$/cancelRequest` 取消（§4.5） |
| `timeout` | 调用超过其生效超时预算；或所绑 `request_id` 已不在途/预算耗尽 |
| `quota_exceeded` | 记忆分区行/字节配额超限（§5.2）；scheduler 每模块 256 key 配额超限（§5.4） |
| `invalid_lease` | 闭集保留位——请求租约（`X-A24-Request-Lease`）机制本轮未签发（SPEC-ME3 §3），今天不会被触发 |
| `unknown_capability` | manifest 请求了一个内核不认识的 capability 字符串 |
| `version_mismatch` | 握手时协议版本区间无交集，或模块未声明区间（§3.3） |
| `auth_failed` | 握手 `auth_token` 不匹配（§3.3，仅握手期） |
| `manifest_mismatch` | 握手 `module`/`manifest_digest` 与内核记录不符（§3.3，仅握手期） |
| `not_ready` | 回调在这一代的 `initialize` 完成之前到达 |
| `draining` | 生成代正在 drain，且回调没有携带一个仍在途的 `request_id` |
| `revoked` | 这一代已被撤销 |
| `rate_limited` | 对应方法的 token bucket 耗尽 |
| `payload_too_large` | `params` 超过通用预算（节点数 >5000、深度 >32、字符串字节 >262144），先于任何方法专属校验触发 |
| `token_invalid` | `_a24/approval/gate`/`advise` 的 `{request_id, approval_token}` 配对失败（错误、过期、跨连接、跨代、重放） |
| `not_found` | `_a24/approval/status` 查询一个不存在或属于别的模块的 `approval_id` |
| `unavailable` | `_a24/model/complete` 找不到可用 provider 或被拒绝（`data` 另带 `retryable`/`cause`，§5.5） |

---

## 附：与本文档配套的产出物

- 参考实现：`examples/node-module/`（`index.js` + `domain-os.yml` + `README.md`）——纯 Node.js 标准库，
  未引用本仓任何 Rust crate。
- 黑盒验收：`rust/apps/agent24d/tests/me4_node_module_blackbox.rs`——真起 `agent24d`、安装这个 Node
  模块、重启、经内核代理访问其路由、事件出现在 WS、`remember` 后能 `recall`。本机无 `node` 时该测试
  显式 `eprintln!` 说明原因并跳过（不是静默通过）。
