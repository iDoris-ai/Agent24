# A3 —— 附着式进程模块（Attached Process Module）设计

> **状态：草案 v1，待对抗评审后冻结**（2026-09-27，agent24-13）。本文是 AgentEar 嵌入的 **P0 契约冻结的 Agent24 一侧**，冻结后 P2 联调才解除阻塞。
> 关联：ADR-032、[`INTEGRATION-AGENTEAR-IDORIS.md`](INTEGRATION-AGENTEAR-IDORIS.md) §8「A3」；AgentEar `docs/agent24-embedding.md`、AgentEar `contracts/`（已合入 AgentEar `main@522f9eb`，见 §7.2）；[`SPEC-ME3-OUT-OF-PROCESS.md`](../specs/SPEC-ME3-OUT-OF-PROCESS.md)（A1）、[`ME4-S2`](ME4-S2-model-callback.md)、[`ME4-S3`](ME4-S3-os-sdk.md)。
> 基线：Agent24 `main@f504ae0`；文中行号、常量均按此提交核对。Rust 签名已在 scratch crate（依赖真实 `agent24-os-proto`/`agent24-domain`）`cargo check` 通过，见附录 A。

## 1. 目标与范围

**目标**：让一个**由用户自己启动**的进程（首个用户：AgentEar，macOS 语音前端，自带麦克风/热键/TCC/ASR/TTS）向**已运行**的 `agent24d` 注册为模块，拿到与 A1 相同的回调能力（本期只用 `events`、`models`），并在**同一条连接**上接收宿主下发的命令（`speak`/`stop_playback`）。

**范围只到 P2 单轮端到端**：真机「说一句 → Agent24 收到 transcript 事件并在桌面端可见 → AgentEar 调 `_a24/model/complete`（`local_only`，零远端流量）→ AgentEar 播报；宿主也能用 `speak` 主动让它播一句」。

| 阶段 | 本文 | 说明 |
|---|---|---|
| P0 契约冻结 | ✅ 全部 | 注册/token/握手/生命周期/反向命令/事件接入的 wire 规格 |
| P1 并行骨架 | ✅ 判据与切法 | Agent24 侧用**按本文 wire 规格手写的 Python 假 AgentEar**做黑盒测试 |
| P2 单轮端到端 | ✅ 最小范围（§7.3） | 非流式（D1），单轮（不接记忆） |
| P3 提案闭环 | ❌ 只列为后续 | `proposal`/`confirm_reply` 在 P2 只**展示**，不执行、不留档；gate 可执行集合仍为空（SPEC-ME3 §6.1） |
| P4 流式 | ❌ 只列为后续 | `_a24/model/complete` 仍非流式（ME4-S2 §0） |

**明确不做**：跨机附着；TCP 端口（loopback ≠ 授权，§9 iDoris 同理）；把附着模块的 HTTP 入站挂到 `/api/v1/<ns>/*`；多轮记忆（`memory` capability 不授予，按 embedding.md §3 单轮处理）；Rust 模块侧的 `connect_attached` 与 SDK 包装（§2 末尾说明为何推迟）。

## 2. 与 A1 的差异

A1 = 内核拉起（SPEC-ME3）。A3 与 A1 **只差「谁连谁、token 怎么来、连接断了算什么」**；帧、握手字段、方法、错误闭集、offer 计算、`Methods` 构造（`domain.rs` `build_methods_for`）全部复用。

| 维度 | A1 内核拉起 | **A3 附着** |
|---|---|---|
| 谁启动进程 | `agent24d`（`launch.rs`，fd 3 + 4 个环境变量） | 用户 / launchd / AgentEar 自己；内核不 spawn、不 kill |
| manifest 从哪来 | 包目录 `domain-os.yml`，启动时读 | **注册时**随 REST 请求提交，内核存原文 + digest（§3）；`impl_kind: attached_process`，**不许有** `spawn` |
| 回调 socket | `~/.agent24/run/<pid>/<n>.sock`，0700，**每代只 accept 一次**、accept 后删路径（`endpoint.rs` `accept_one`） | **一个常驻监听** `~/.agent24/attach/agent24d.sock`，目录 0700、节点 0600，可多次 accept；每条连接各自握手 |
| token | 每代现铸、经 `A24_HANDSHAKE_TOKEN` 传入，一次性 | 注册时铸造、**长期有效直到撤销/轮换**；内核只存 sha256；AgentEar 存 macOS Keychain |
| 握手 | `initialize`（`initialize.rs`） | **同一帧、同一字段**：`auth_token` 装附着 token；无新增字段 |
| 内核 → 模块 | HTTP over 模块 UDS（`<n>.l` 作为 fd 3，`kernel_call.rs`/`proxy.rs`） | **同一条回调连接上的 JSON-RPC 请求**（§6）；模块不监听任何 socket |
| 入站 REST 反代 `/api/v1/<ns>/*` | 有 | **无**（命令走 §6 的固定内核路由） |
| 连接断开 | D1：连接即生命线，断 = 这一代结束，进程被停 | 断 = 这一代结束（generation 撤销），**进程不受影响**，可用同一 token 重连成新一代 |
| 重启/熔断 | `RestartPolicy` + 熔断 | 无（内核不拉起）；重连退避由模块自己做（§5.4） |
| 单实例 | 每模块一个 supervisor | 每模块同时最多一代；第二条连接握手被拒 `busy`（§5.3） |
| 与另一形态互斥 | — | **按模块名互斥**：同名已安装包 / 编入模块 → 注册 409；已注册附着名 → 同名包挂载 `Refused` |

**为什么反向命令走同一连接，而不是模块另开 `inbound.sock` 让内核用 HTTP 连它**（AgentEar 会话提出的备选，已评估）：

| | H：模块在 `~/.agent24/attached/<m>/inbound.sock` 起 HTTP，内核连它 | **J：同一连接双向 JSON-RPC（选）** |
|---|---|---|
| 复用 | 可复用 `kernel_call::send_kernel_request`；但 `/api/v1/<ns>/*` 反代是启动时 `mount_all` 建好的 axum 路由，运行期注册的模块挂不上，照样要新写一条固定内核路由 | `rpc::serve_until` **一行不改**，外面包一层帧分拣（§6.2，新文件约 200 行） |
| 模块侧成本 | AgentEar 今天**没有** tokio/hyper/任何 server 依赖（`Cargo.toml`），要新起一个 HTTP/1.1 服务并照 SPEC-ME3 §2 处理 `X-A24-*` 头 | 已有的 NDJSON 读循环里多认一种帧（带 `method` 的）并回一行 |
| 身份绑定 | 两条通道要证明是同一进程：`LOCAL_PEERPID` 比对，有 pid 复用窗口；同 UID 进程可抢先 bind 该路径 | 只有一条已认证连接，无第二个信任锚 |
| 生命周期 | 两个 socket 各自断、各自清理（残留节点、目录权限） | 一条连接，断即全断 |
| 失败面 | 多一个文件系统节点由模块持有 | 无 |

结论：J 的总改动更小、少一个信任锚、AgentEar 不引入 HTTP 服务。代价是 A1 与 A3 的「内核→模块」路径不同（A1 仍是 HTTP，理由是 A1 模块本来就有入站 HTTP）；这是有意的分叉，不回改 A1。

**Rust 模块侧 `connect_attached` 与 SDK 包装推迟**（偏离 agent24-13 最初偏好 ③，理由）：唯一的 A3 用户 AgentEar 按 B4 **不依赖** Agent24 crate；Agent24 自己的黑盒测试用**只读本文 wire 规格写成的 Python 假模块**，这恰好同时验证「wire 规格可独立实现」。出现第一个 Rust 附着模块时再加（`module.rs` 私有 `handshake(stream, env, hello)` 只从 `ModuleEnv` 取 token 一项，届时改为传参、再加一个按路径拨号的构造函数即可，是小改动）。

## 3. 注册、token 发放与撤销

### 3.1 manifest（附着形态）

```yaml
name: agentear
version: "0.24.0"
route_namespace: /api/v1/agentear   # 仍按既有规则由 name 推导并校验；A3 下不挂路由，只占名
event_module: agentear
data_dir: ~/.agent24/os/agentear/   # 同上，只校验不使用（AgentEar 用自己的 ~/.agentear/）
impl_kind: attached_process          # 新变体；与 spawn 互斥（有 spawn → 拒绝）
kernel_capabilities: [events, models]
model_access: local_only             # 可省略，缺省即 local_only（ME4-S2 §2.1）
host_commands: [speak, stop_playback] # 新字段，可选；内核只转发此处列出的命令名，名字语法 [a-z0-9_]{1,32}
```

`agent24-domain` 改动：`ImplKind::AttachedProcess`、`RawManifest.host_commands: Vec<String>`（`deny_unknown_fields` 下旧 daemon 会拒绝新 manifest，这是期望行为）。`host_commands` 只允许出现在 `attached_process` 上（A1 的反向通道是 HTTP，不需要）。

### 3.2 REST 端点（全部在既有 bearer 鉴权之后）

REST 只监听 `127.0.0.1` 且要求 bearer（`server.rs:664` `auth`，token 在 `~/.agent24/daemon.json`）。「已认证 CLI」即持有该 bearer 的 `agent24` CLI；**附着 token 永远不能用来调 REST**，两者不是一套凭据。

| 方法 & 路径 | 作用 | 返回 |
|---|---|---|
| `POST /api/v1/os/attached` body `{"manifest": "<domain-os.yml 原文>"}` | 校验 manifest → 名字互斥检查 → 铸 token（`launch::mint_token`，32 字节）→ 存记录 → **明文只在本响应出现一次** | `201 {"name","manifest_digest","token","socket_path","token_id"}`；已存在同名附着记录 → **轮换**：新 token 生效、旧 token 立即失效（撤销现连接，§3.4），返回 `200` 同形；同名已装包/编入模块 → `409 name_taken`；manifest 非法 → `400 invalid_manifest` |
| `DELETE /api/v1/os/attached/{name}` | 撤销：删记录 + 撤销现有一代并断开 | `204`；不存在 `404` |
| `GET /api/v1/os`（既有） | 列表里附着模块多一类 `kind: "attached"`，带 `attach_status`（§5.1）、`generation`、`manifest_digest`、`token_id` | **从不**含 token 或其哈希 |
| `POST /api/v1/os/{name}/commands/{command}` | 反向命令，见 §6 | — |

CLI：`agent24 os attach add <manifest.yml> [--json]`、`agent24 os attach revoke <name>`；`agent24 os list` 展示附着状态。`os enable/disable` 对附着模块同样生效（disable = 排空并断开，之后握手被拒 `forbidden`）。

### 3.3 存储

`~/.agent24/attached.json`，写法与 `os.json` 相同（`os_config.rs`：`.lock` 文件锁 + 临时文件 + rename），文件 0600：

```json
{"version":1,"modules":{"agentear":{
  "manifest_yaml":"<原文>","manifest_digest":"sha256:<hex>",
  "token_sha256":"<hex>","token_id":"tok_<8 hex>","created_at":"<RFC3339>"}}}
```

- 只存 `sha256(token)`；比对时对**提交的 token 做 sha256 后常量时间比较**。`token_id` 是随机短 id，用于日志/展示区分轮换，不可反推 token。
- 判据 C1 断言：文件里 grep 明文 token = 0 次，grep 其 sha256 = 1 次（正对照）。

### 3.4 撤销与轮换的效果

撤销或轮换 = 同一把锁内：①记录删除/替换并落盘 ②若有现役一代：`Generation` 撤销（`drain.rs` `revoke()`，在途回调得到 `revoked` 或随连接关闭而取消，ME4-S2 §3.3 的取消路径照旧）③关闭连接。此后旧 token 握手一律 `auth_failed`。`Generation::revoke` 是 `pub(crate)`，所以附着生命周期的代际管理放在 proto 新文件 `attach.rs`，agent24d 只调用它。

## 4. 连接与握手 wire 规格（独立可实现）

本节是 AgentEar 实现的唯一依据；与 A1 共用的条文在 SPEC-ME3 §3 有出处，此处**完整重述**，以本节为准。

### 4.1 传输

- Unix 域流 socket，路径 = 注册响应里的 `socket_path`（当前为 `~/.agent24/attach/agent24d.sock`，展开后的绝对路径）。模块应保存该路径，不要硬编码。
- 内核只接受**同 UID** 对端（`peer_cred`），否则直接关闭。模块不监听任何 socket、任何端口。
- daemon 不在时 `connect` 失败（`ENOENT`/`ECONNREFUSED`）= 宿主离线，按 §5.4 处理。

### 4.2 帧

- 一帧 = 一个 UTF-8 JSON **对象**，以 `\n` 结尾；负载（不含 `\n`）≤ **1 MiB**（`frame.rs:84` `MAX_FRAME_BYTES = 1048576`）。超长 → 内核断开连接；最后一行没有 `\n` 不算帧。
- **不支持 batch**（数组 → `-32600`）。`jsonrpc` 必须是 `"2.0"`。请求对象只允许 `jsonrpc`/`id`/`method`/`params` 四个成员；`params` 必须是对象（可省略）。
- `id` 是**字符串**、≤256 字节（`rpc.rs` `MAX_ID_BYTES`），同一方向在途 id 不得重复。两个方向的 id 空间**互相独立**；区分帧的唯一依据：**有 `method` = 请求/通知；无 `method` 且有 `result` 或 `error` = 对某个请求的响应**。
- 响应不保证顺序，按 id 匹配。可选字段不发时**省略**，不要发 `null`。**解析对方的响应时忽略未知字段**；内核解析请求 `params` 时**拒绝**未知字段（`-32602`）。
- 取消：通知 `{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":"<在途 id>"}}`，只能取消自己发出的请求。

### 4.3 握手

连接后模块发的**第一帧**必须是 `initialize`（只能发一次；之后再发 → 该行 `-32600`，连接继续）。握手期（从 accept 起 **5 s** 内，含内核写完应答）任何失败 → 内核写一行错误后断开。

```json
{"jsonrpc":"2.0","id":"1","method":"initialize","params":{
  "protocol_versions":{"min":1,"max":1},
  "module":"agentear",
  "manifest_digest":"sha256:<注册时同一份 domain-os.yml 原始字节的 sha256 小写 hex>",
  "auth_token":"<注册返回的 token>",
  "capabilities":["events","models"]}}
```

- 字段与 A1 **完全相同**（`initialize.rs` `InitializeParams`，`deny_unknown_fields`、拒重复键）：`protocol_versions` 缺失视为不兼容；`capabilities` 可省略（仅作提示，实际授予以 manifest + 策略为准）。**A3 没有专有字段**。
- 成功：`{"jsonrpc":"2.0","id":"1","result":{"protocol_version":1,"offer":{"provides":["_a24/events/","_a24/model/"]}}}`。`provides` 是前缀列表，未列出即未授权（调用会得 `forbidden`）。模块侧**宽松解析**（忽略未知字段，同 `module.rs` `InitializeReply`）；`protocol_version` 必须落在自己声明的区间内；应答之后在读缓冲里不应有残余字节。
- 内核校验顺序（固定，测试断言）：JSON/信封形状 → 按 `module` 查注册记录（**查不到 → `auth_failed`**，不暴露「是否注册过」）→ `manifest_digest` → token 哈希 → 版本 → 模块是否被 disable → 是否已有现役一代。

| 失败 | `error.code` | `error.data.kind` |
|---|---|---|
| 首帧不是合法 JSON | `-32700` | — |
| 不是 `initialize` 请求 / 信封非法 | `-32600` | — |
| `params` 解析失败（缺字段、未知字段、重复键） | `-32602` | — |
| 未注册的 `module`，或 token 不对 | `-32000` | `auth_failed` |
| digest 或 module 与注册记录不符 | `-32000` | `manifest_mismatch` |
| 版本区间无交集或未声明 | `-32000` | `version_mismatch`（`data` 另含 `module`/`kernel` 两个区间） |
| 已注册但被 `os disable` | `-32000` | `forbidden` **（A3 新增用法）** |
| 同名模块已有现役一代 | `-32000` | `busy` **（A3 新增用法）** |

后两行只是给 `HandshakeError` 加两个变体，`kind` 取自既有 `ErrorKind`，**18 值的 wire 闭集不变**，AgentEar `agentear.event/1` 的 `error.code` 枚举无需改动。

### 4.4 握手之后：方法表（本期）

| 方向 | 方法 | params → result | 限制（来源） |
|---|---|---|---|
| 模块→内核 | `_a24/events/emit` | `{kind, payload, request_id?}` → `{}` | `kind` 为点分小写 ASCII `[a-z0-9_-]+(\.[a-z0-9_-]+)+`、≤96 字节；`payload` 对象 ≤256 节点且字符串（含键）≤8 KiB；令牌桶 20 突发 / 5 每秒（`events_emit.rs`） |
| 模块→内核 | `_a24/model/complete` | `{messages:[{role:system\|user\|assistant, content}], complexity?: simple\|complex, max_tokens?: 1..=4096（缺省 1024）, response_format?, request_id?, _meta?}` → `{text, model_id, tier: local\|remote, usage:{prompt_tokens, completion_tokens}}` | messages ≤64；超时 120 s；每模块并发 2、全局 4；令牌桶 30 突发 / 0.5 每秒（`model_callback.rs:35-49`）。**模块侧响应等待须 > 120 s**（建议 130 s），否则内核的 `timeout` 会被自己的超时抢先 |
| 内核→模块 | `_a24/command/invoke` | `{name, body}` → 任意 JSON 对象 | 见 §6；内核等待 5 s |

错误：`-32601` 方法不存在；`-32602` 参数非法；`-32603` 内部错误；`-32000` + `data.kind` ∈ `ErrorKind::ALL`（`rpc.rs:167`，18 个：`forbidden busy cancelled timeout quota_exceeded invalid_lease unknown_capability version_mismatch auth_failed manifest_mismatch not_ready draining revoked rate_limited payload_too_large token_invalid not_found unavailable`），可带 `data.retryable: bool`。`unavailable` 另带 `data.cause` ∈ `no_provider | request_rejected | backend_config | response_too_large`。`request_not_in_flight`/`connection_lost`/`not_sent` 是 SDK 的 `ClientError`，**不是** wire 值。

## 5. 生命周期

### 5.1 状态机（每个附着注册一份，内存态；只有注册记录落盘）

```
            add（REST）                    握手成功（gen = N+1）
(无记录) ───────────► Detached ──────────────────────────► Attached(N+1)
   ▲                    ▲  ▲                                 │   │
   │ revoke             │  └──── 连接断开 / 写失败 / 超长帧 ──┘   │ disable / daemon 退出
   │（任意状态）         │         （撤销该代，立即）             ▼
   └────────────────────┴──────────────────────────────── Draining(N) ──(≤10 s 或在途清零)──► Detached / Disabled
```

- `attach_status` ∈ `detached | attached | draining | disabled`，经 `GET /api/v1/os` 暴露。不复用 supervisor 的 `Status`（那是进程视角：Starting/Backoff/GaveUp…，附着没有这些）。
- **generation**：每次握手成功新建一个 `Generation`（无 upstream，`Generation::starting()` → ready），编号在该模块内单调递增（内存计数；daemon 重启后从 1 重新计，编号只用于日志与判据，不是凭据）。所有回调的准入照旧走 `admit_callback`：旧代被撤销后，它在途与新发的回调一律 `revoked`（ME-3b-5 语义原样复用）。`Methods` 由 `build_methods_for` 按代构造：`events` 令牌桶每代新建，`models` 的 `ModelGrant`（含限速与健康表）**按注册存续期**只建一次——与 A1「按挂载存续期」一致，重连不能刷新模型配额。

### 5.2 断连、drain、撤销

- **连接断开**（EOF、读写失败、超长帧、写超时 10 s）：立即撤销该代（不 drain——对端已经没了），在途 `model/complete` 随之取消，状态回 `Detached`。内核不做任何「等它回来」。
- **disable / daemon 优雅退出**：`begin_drain(10 s)` → 新回调无 `request_id` 的得 `draining`；在途调用清零或 10 s 到 → 撤销、关闭连接。模块看到的只是 EOF。
- **revoke / 轮换**：立即撤销（§3.4），不 drain。

### 5.3 单实例与互斥

- 同一模块同时最多一代：现役代存在时新连接握手得 `busy`（**先到者保留**；待拍板 Q3）。AgentEar 自身也有单实例锁，这一条是第二道。
- 与 spawn 互斥：注册时同名已安装包或编入模块 → `409`；`mount_all` 遇到与附着注册同名的包 → `MountOutcome::Refused("registered as an attached module")`。名字是唯一身份（与 A1 相同的推导规则），因此路由命名空间、事件模块名天然不会冲突。

### 5.4 daemon 重启与模块侧重连

- daemon 重启：注册记录从 `attached.json` 读回，token 依旧有效，监听 socket 在同一路径重建（启动时若路径已是 socket 节点且连不上 → 视为残留，unlink 后 bind；连得上 → 另一 daemon 在跑，附着监听降级并记录错误）。所有模块回到 `Detached`，等它们重连。
- 模块侧（AgentEar 实现约束）：EOF 后按 B5 策略切换本地行为；后台以 1 s 起、翻倍、上限 30 s 的退避重连；重连成功是**新会话**：新的 `session_id`、`seq` 从 1 重新开始（`agentear.event/1` 已如此定义）。收到 `auth_failed`/`manifest_mismatch` 时**停止重连**并在设置里提示「需要重新配对」——那是凭据问题，重试没有意义。
- **B5（已决，jason 2026-09-27）**：断连后，若独立模式的推理配置为本机边车 → 自动回独立模式；否则停听并提示；设置可改。任何情况下不得把附着时受 `local_only` 约束的请求改发远端。

## 6. 反向命令（`speak` / `stop_playback`）

### 6.1 路由

```
外壳/CLI ──POST /api/v1/os/agentear/commands/speak（bearer）──► agent24d
   内核：①附着注册存在？(否 404) ②command ∈ manifest.host_commands？(否 403，零帧下发)
        ③body 是 JSON 对象且 ≤64 KiB？(否 400) ④现役一代且 Ready？(否 503 module_not_ready)
        ⑤在该代连接上发 {"jsonrpc":"2.0","id":"k<n>","method":"_a24/command/invoke",
                          "params":{"name":"speak","body":<请求体原样>}}，等待 5 s
AgentEar ──{"jsonrpc":"2.0","id":"k<n>","result":{"accepted":true}}──► 内核 ──200 {"result":{…}}──► 外壳
```

- 内核**不解析** `body` 的业务 schema（`agentear.command/1` 归 AgentEar）；`name` 与 `body.type` 的一致性由 AgentEar 校验，不一致按非法命令回 `-32602`。
- 模块对 `_a24/command/invoke` 的应答只表示**已受理**（入队）；播放结果经 `speech`/`error` 事件回报（带同一 `command_id`），与 AgentEar 契约一致。模块对未知 `name`、schema 不合法、未知版本 → `-32602`；老版本不认识该方法 → `-32601`。
- REST 映射：`result` → `200 {"result":…}`；模块 JSON-RPC 错误 → `502 {"error":{"code":"module_error","message","rpc_code","kind"?}}`；5 s 无应答 → `504 timeout`（**结果未知**）；已写出后连接断开 → `502 connection_lost`（结果未知）；未写出 → `503 module_not_ready`（确定未送达）。
- 内核→模块调用每连接在途上限 8（命令是用户动作级别的频率，8 足够；超出 → REST `429 busy`，零帧下发）。

### 6.2 实现落点：`serve_attached`（proto 新文件 `attach_mux.rs`）

`rpc::serve_until` 已被多轮评审固化，**不改**。附着连接在它外面包一层：一个读任务按 §4.2 的规则分拣帧——有 `method` 的原样喂给 `serve_until`；无 `method`、有字符串 `id` 且命中待决内核调用的，交给对应等待者；其余仍喂给 `serve_until`（它照旧回 `-32600`，行为与 A1 一致）。写方向由一个写任务按**整行**合并 `serve_until` 的输出与内核请求。签名（scratch 已 check）：

```rust
pub fn serve_attached<R, W, S>(reader: R, writer: W, methods: Methods, limits: Limits, stop: S)
    -> (KernelCalls, impl Future<Output = Ended> + Send)
where R: AsyncBufRead + Unpin + Send + 'static, W: AsyncWrite + Unpin + Send + 'static,
      S: Future<Output = ()> + Send + 'static;

impl KernelCalls {
    pub async fn call(&self, method: &'static str, params: Value, timeout: Duration)
        -> Result<Value, KernelCallFailed>;          // NotSent | ConnectionLost | Timeout | Rpc(RpcError)
}
```

### 6.3 幂等

- **内核从不自动重发命令**（命令有副作用：说话）。调用方拿到 `504`/`502 connection_lost` 时可**用同一 `command_id` 重试**。
- **AgentEar 负责按 `command_id` 去重**：至少记住本进程最近 256 个 `command_id` 及其首次应答，重复送达直接回首次结果、不再执行（契约已写）。进程重启后缓存丢失属可接受残余：最坏多播一句。
- `stop_playback` 在无播放时也回 `accepted`；它和用户按键打断都清空 `speak` 队列（B7）。

## 7. 事件接入与 P2 宿主展示

### 7.1 发送

AgentEar 用 `_a24/events/emit`，`kind` 固定为 **`agentear.event`**，`payload` = 完整的 `agentear.event/1` 对象（含 `schema`/`event_id`/`session_id`/`seq`/`type`/`payload`）。内核照旧把它包成 WS `type:"module"` 事件（`module:"agentear"`，`kind`，`payload` 原样）广播到 `GET /api/v1/events`；**内核不理解、不校验这份 schema**（与 ME-3e 的「内核只做通用承载」一致）。

- 大小：`transcript.text` 连同其它字符串须在 8 KiB 内（中文约 2600 字），超出得 `payload_too_large`；AgentEar 截断并在文本末尾标注，不拆成多条。
- 重试复用 `event_id` 与 `seq`。**AgentEar 按 `seq` 顺序逐条等待 `emit` 应答再发下一条**（同一 session 内不并发 emit），这样线上顺序即 seq 顺序；宿主的重排只是兜底。

### 7.2 去重与排序（宿主消费端）

内核是通用承载，**`(session_id, seq)` 去重放在理解该 schema 的消费端**——P2 即桌面端主进程的 `agentearSequencer`（纯函数 + 状态，vitest 用 AgentEar fixtures 测）：

- 未知 `schema` 版本 → 丢弃并计数（`fixtures/*/invalid/unknown_version.json`）。
- 同 `(session_id, seq)` 且同 `event_id` → 丢弃（重试）；不同 `event_id` → 拒收后到者并记一条协议违规（`sequences/seq_conflict.json`）。
- 乱序：每 session 缓冲至多 32 条、缺口最多等 2 s，超时跳过缺口并记录；按 seq 交付（`sequences/reorder.json`）。
- **fixtures 引用方式：vendored 副本 + 记录 commit**（最简单：CI 不联网、不跨仓库拉取）。基准 = AgentEar `main@522f9eba0f720c97b9bb0a63d059a82e33029dcb`（3 份 schema + 57 个 fixtures；`error.code` = wire 18 + AgentEar 自有 4；字段拼写 `retryable`）。做法：`git -C <AgentEar> archive 522f9eb contracts/ | tar -x -C apps/desktop/test/fixtures/agentear/`，同目录写 `SOURCE`（完整 sha + 拷贝日期）；vitest 首条断言 `SOURCE` 存在且 sha 为 40 位。升级 = 换 sha 重新拷、同一 PR 里改测试，不跟 `main` 浮动头。Rust 侧假 AgentEar 的事件样例也从这份副本读，不另写。

### 7.3 P2 最小范围（宿主侧）

依 embedding.md §3「模型：AgentEar 调 `_a24/model/complete`」：**P2 的对话由 AgentEar 自己编排——自己调模型、自己播；宿主只提供能力、展示事件、接受 `speak`。** 具体：

| 做 | 不做（后续） |
|---|---|
| 附着注册/握手/生命周期/撤销（§3–§5） | 宿主替 AgentEar 编排对话、把对话挂到 `/api/v1/sessions` |
| `events`、`models` 两个 capability；`local_only` | `memory`（多轮）、`approval`（P3） |
| 桌面端「语音」面板：主进程订阅 WS、经 sequencer 后显示 transcript、turn 相位、speech 状态、error；显示本模块 `GET /api/v1/usage?module=agentear` 的 tier 统计 | transcript **落盘**（P2 只在桌面端内存保留最近 200 条；daemon 不存） |
| 面板上一个「让它说」输入框 → `POST …/commands/speak`；「停止」→ `stop_playback` | proposal 的确认 UI、执行、回执留档（P3）；P2 面板只把 proposal 列出来并标「P3 前不执行」 |
| `agent24 os attach add/revoke`，`os list` 状态 | 回复文本在宿主展示（见待拍板 Q1） |

## 8. 隐私

- `model_access` 缺省 `local_only`，只由注册时的 manifest 决定；握手里的 `capabilities`、调用里的 `complexity` 都**不能**提高隐私级别（ME4-S2 §2）。改成 `remote_allowed` 必须重新注册（digest 变 → 必须用户经 CLI 重新 add）。
- `local_only` 无本地 provider → `unavailable` + `cause: no_provider`，**不回落远端**；AgentEar 不得把它转成独立模式直连远端的请求（B5 只允许「独立模式本身就是本机边车」时回退）。
- 内核日志只记元数据：模块名、generation、方法名、字节数、耗时、错误 kind；**不记** transcript、messages、回复、命令 body、token。`token_id` 可记。
- 事件经 WS 只到本机已认证客户端；P2 不落盘。
- **零远端流量验收**见 C8：路由器里放一个计数远端桩（非回环地址 → `Remote` 层，D2），`local_only` 全流程计数必须为 0，并用 `remote_allowed` 的对照模块证明计数器本身会涨。

## 9. 核心判据

约定：每条 `cargo test <过滤>` 先跑 `-- --list` 断言非空；每条带正对照（标 ⊕）；「假 AgentEar」= `rust/apps/agent24d/tests/fixtures/fake_agentear.py`，**只按本文 §4/§6 写成**（不 import 任何 Agent24 代码）。

| # | 判据 | ⊕ 正对照 |
|---|---|---|
| C1 注册 | `POST /api/v1/os/attached` 返回 token 仅一次；`attached.json` 中 grep 明文 token = 0；`GET /api/v1/os` 不含 token/哈希；再 add 同名 → 旧 token 握手 `auth_failed` | grep `sha256(token)` = 1；新 token 握手成功 |
| C2 握手 | 错 token / 未注册名 → `-32000 auth_failed` 且连接关闭；digest 不符 → `manifest_mismatch`；disable 后 → `forbidden` | 正确 token → `offer.provides` 恰为 `["_a24/events/","_a24/model/"]`（测试 daemon 须配模型依赖：`_a24/model/` 只在 `model_grant` 为 `Some` 时列出，`domain.rs:1739`） |
| C3 撤销 | `DELETE` 后 ≤1 s 假模块读到 EOF；撤销前挂起的 `model/complete` 以连接关闭结束、provider 端取消被观测到；旧 token 重连 `auth_failed` | 撤销前同一 token 可重连 |
| C4 单实例/互斥 | 现役代在时第二条连接 → `busy`；与已装包同名注册 → `409`；同名包挂载 → `Refused` | 第一条断开后重连成功且 generation +1；不同名注册 `201` |
| C5 反向命令 | `POST …/commands/speak` → 假模块收到 `_a24/command/invoke`，`body` 与请求体逐字节等价（规范化 JSON）；未声明命令 → `403` 且假模块收到的内核请求帧数 = 0；未附着 → `503`；假模块不应答 → `504` | 声明的命令帧数 = 1；假模块回 `-32602` → `502 module_error` |
| C6 帧分拣 | 附着连接上模块发「无 method、未知 id」的响应帧 → 被 `serve_until` 以 `-32600` 回（与 A1 同）；A1 连接行为逐项不变（既有 rpc 测试全绿） | 已知 id 的响应帧被路由到等待者 |
| C7 事件 | 假模块 emit 一条 `transcript` → WS 收到 `module` 事件，`payload` 与发送体相等；桌面 vitest：`sequences/{dedupe_retry,seq_conflict,reorder}.json` 与 `event/invalid/unknown_version.json` 的 `expect` 全部满足 | `event/valid/*.json` 全部被接受 |
| C8 隐私 | `local_only` + 本地桩 + 远端计数桩：完整一轮后远端计数 = 0、结果 `tier=local`；无本地 provider → `unavailable/no_provider`、远端计数 = 0；daemon 日志 grep transcript 哨兵串 = 0 | 另注册 `remote_allowed` 的对照模块 + `complexity: complex` → 远端计数 ≥1；哨兵串在 WS 抓包中出现 |
| C9 重启 | daemon 重启后同 token 重连成功，generation 从 1 重新计；残留 socket 节点被清理后监听成功 | 重启前后注册记录 digest 相同 |
| C10 真机 P2（jason 手动） | 对话模式说一句 → 桌面「语音」面板出现该 transcript → AgentEar 播出回复 → `usage?module=agentear` 新增一条 `tier=local` → 远端计数桩 = 0；面板「让它说」能播；按键打断后 `stop_playback` 无报错；退出 Agent24 后 AgentEar 按 B5 行为切换、无残留录音/TTS 子进程 | 同一句在 AgentEar 独立模式下可正常回答（证明链路本身可用） |

## 10. PR 切法（4 片，stacked）

| 片 | 内容 | 判据 | 估计 |
|---|---|---|---|
| **A3-1** proto + domain | `ImplKind::AttachedProcess`、`host_commands`；`initialize::accept_attached` + `HandshakeError::{Busy, Forbidden}`；`attach_mux.rs`（`serve_attached`/`KernelCalls`）；`attach.rs`（代际与撤销） | C2（单元层）、C6 | ~450 行，含测试 |
| **A3-2** agent24d 注册与生命周期 | `attached.json`、REST add/revoke/list、CLI、常驻监听、状态机、互斥、drain、重启恢复；复用 `build_methods_for`；Python 假 AgentEar | C1–C4、C8、C9 | ~600 行 |
| **A3-3** 反向命令 | `POST /api/v1/os/{name}/commands/{command}`、REST 错误映射、在途上限 | C5 | ~250 行 |
| **A3-4** 桌面端 | 主进程 WS 订阅、`agentearSequencer` + fixtures（钉 sha）、「语音」面板、speak/stop 按钮 | C7；C10 在此片后由 jason 真机跑 | ~400 行 |

A3-2 若超过 300 行门槛可再按「存储 + REST」「监听 + 生命周期」拆，但仍在 4 片以内合并审（不再细拆）。开工前先更新 PLAN §三 与 `tasks.md` 台账。

## 11. 仍需 jason 拍板

| # | 问题 | 选项 | 推荐 |
|---|---|---|---|
| Q1 | P2 桌面端要不要显示**模型回复文本**？`agentear.event/1` 目前没有 `reply` 类型，宿主看不到回复 | a) P2 不显示，只显示 transcript + tier；b) AgentEar 在 `/1` 里加 `reply{text, model_id, tier}` 类型（改 enum，按 AgentEar 版本规则应升 `/2` 或双方同步） | **a**。P2 目标是听说闭环；回复展示与留档一起放 P3 设计，避免为展示先改 schema |
| Q2 | 配对 UX：谁调 `agent24 os attach add` | a) 用户在终端跑，粘贴 token 到 AgentEar 设置；b) AgentEar 设置里「连接 Agent24」按钮代跑本机 `agent24` CLI 拿 token 存 Keychain | **b**。同 UID 下 a 不更安全（SPEC-ME3 §0 不防同 UID），b 少一步手工；失败时退回 a |
| Q3 | 现役代存在时来了第二条合法连接 | a) 先到者保留，新连接 `busy`；b) 新连接顶掉旧的 | **a**。b 让任何拿到 token 的进程能静默抢走麦克风前端的会话；卡死场景用 `revoke` 或重启 AgentEar 解决 |
| Q4 | P2 是否让 daemon 持久化 transcript | a) 不存（桌面内存最近 200 条）；b) 存进 store | **a**。与「日志不记内容」一致；回执留档属 P3（ADR-0008 §5 第 6 问已定归 Agent24，届时一起设计） |

## 12. 后续（不在本文设计）

- P3：`proposal` → 宿主确认 UI（含 `confirm_reply` 事件消费、同源校验）→ gate 可执行集合 → 回执留档。
- P4：流式 `_a24/model/stream`（取消、背压、计量）。
- 多轮：授予 `memory`（`_a24/memory/private/*`），或宿主会话回调。
- Rust 模块侧 `connect_attached` + SDK 包装（出现 Rust 附着模块时）。
- 附着模块的入站 HTTP（若将来有模块需要被外壳直接调 REST）。

## 附录 A：scratch 签名检查

`scratchpad/a3-sig/`：`Cargo.toml` 以 path 依赖本分支 `rust/crates/agent24-os-proto` 与 `agent24-domain`；`src/lib.rs` 含本文全部 Rust 签名——`AttachedExpectation { manifest_digest: String, token_sha256: [u8; 32], kernel_versions: VersionRange, offer: Offer }`、`accept_attached(frame: &[u8], lookup: &dyn Fn(&str) -> Option<AttachedExpectation>) -> Result<(String, Accepted), HandshakeError>`、`KernelCallFailed`、`KernelCalls::call`、`serve_attached`、`COMMAND_METHOD = "_a24/command/invoke"`、`COMMAND_TIMEOUT = 5 s`；`HandshakeError` 的两个新变体以镜像枚举检查。`cargo check`（stable，edition 2024）通过，0 error。
