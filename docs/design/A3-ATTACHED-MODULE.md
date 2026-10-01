# A3 —— 附着式进程模块（Attached Process Module）设计

> **状态：v2 冻结**（2026-09-27，agent24-13）。v2 = jason 拍板（§11）+ Tier-2 对抗评审修订（4H/7M 全部落入）+ AgentEar 会话（agentear-59）对齐六点。按用户控制范围要求：**设计只评审一轮，v2 即冻结，不再复审**；实现阶段每个 PR 各评审一轮。本文是 AgentEar 嵌入的 **P0 契约冻结的 Agent24 一侧**，冻结即解除 P2 联调阻塞。
> **实现**：A3-1 #527 / A3-2a #526 / A3-2b #529 / A3-3 #532 / A3-4 #534 已合并（2026-09-27），随 v0.4.0 发布；AgentEar 侧 v0.25.1 真 agentd E2E 5 次里 4 次通过（第 5 次的首轮失败很可能已由发版前修复 #543 的 C1 解决，见 `docs/agent/followups.md` FU-99，待复验）。
> 关联：ADR-032、[`INTEGRATION-AGENTEAR-IDORIS.md`](INTEGRATION-AGENTEAR-IDORIS.md) §8「A3」；AgentEar `docs/agent24-embedding.md`、AgentEar `contracts/`（已合入 AgentEar `main@522f9eb`，见 §7.2）；[`SPEC-ME3-OUT-OF-PROCESS.md`](../specs/SPEC-ME3-OUT-OF-PROCESS.md)（A1）、[`ME4-S2`](ME4-S2-model-callback.md)、[`ME4-S3`](ME4-S3-os-sdk.md)。
> 基线：Agent24 `main@f504ae0`；文中行号、常量均按此提交核对。Rust 签名已在 scratch crate（依赖真实 `agent24-os-proto`/`agent24-domain`）`cargo check` 通过，见附录 A。

## 版本记录

| 版本 | 日期 | 内容 |
|---|---|---|
| v1 | 2026-09-27 | 草案，送对抗评审 |
| **v2（冻结）** | 2026-09-27 | **jason 拍板**（Q1–Q4，§11）；**评审修订**：H1 附着代三态 upstream（§5.1）、H2 纯校验 + 同锁提交（§5.2）、H3 迟到响应丢弃（§6.2）、H4 digest 变 = 新注册（§3.4）、M1 disable/退出立即撤销（§5.3）、M2 运行期依赖与关机顺序（§5.5）、M3 token 先于 digest + 哑哈希 + 不回显（§4.3）、M4 放宽隐私需宿主确认（§3.5）、M5 wire 规格补全（§4.5、§5.6、§6.1）、M6 INTEGRATION §6 引用、M7 REST 路径改为 `/api/v1/attached`（§3.2）；**AgentEar 对齐**：manifest version 语义与自动轮换（§3.1、§5.6）、无 models 能力不回落（§4.3）、取消与令牌桶核查（§4.4）、事件限流处置（§4.4）、CLI 契约（§3.6）、builtin 提案只展示永不执行（§7.1，AgentEar PR #96）、`command_id` 去重下限 256（§6.3） |

## 1. 目标与范围

**目标**：让一个**由用户自己启动**的进程（首个用户：AgentEar，macOS 语音前端，自带麦克风/热键/TCC/ASR/TTS）向**已运行**的 `agent24d` 注册为模块，拿到与 A1 相同的回调能力（本期只用 `events`、`models`），并在**同一条连接**上接收宿主下发的命令（`speak`/`stop_playback`）。

**范围只到 P2 单轮端到端**：真机「说一句 → Agent24 收到 transcript 事件并在桌面端实时可见 → AgentEar 调 `_a24/model/complete`（`local_only`，零远端流量）→ AgentEar 播报；宿主也能用 `speak` 主动让它播一句」。

| 阶段 | 本文 | 说明 |
|---|---|---|
| P0 契约冻结 | ✅ 全部 | 注册/token/握手/生命周期/反向命令/事件接入的 wire 规格 |
| P1 并行骨架 | ✅ 判据与切法 | Agent24 侧用**按本文 wire 规格手写的 Python 假 AgentEar**做黑盒测试 |
| P2 单轮端到端 | ✅ 最小范围（§7.3） | 非流式（D1），单轮（不接记忆） |
| P3 提案闭环 | ❌ 只列为后续 | `proposal`/`confirm_reply` 在 P2 只**展示**，不执行、不留档；gate 可执行集合仍为空（SPEC-ME3 §6.1）；回复文本展示也在 P3（Q1） |
| P4 流式 | ❌ 只列为后续 | `_a24/model/complete` 仍非流式（ME4-S2 §0） |

**明确不做**：跨机附着；TCP 端口（loopback ≠ 授权，§9 iDoris 同理）；把附着模块的 HTTP 入站挂到 `/api/v1/<ns>/*`；多轮记忆（`memory` capability 不授予，按 embedding.md §3 单轮处理）；**Agent24 持久化 transcript**（Q4：归属方是 AgentEar）；Rust 模块侧的 `connect_attached` 与 SDK 包装（§2 末尾说明为何推迟）。

## 2. 与 A1 的差异

A1 = 内核拉起（SPEC-ME3）。A3 与 A1 **只差「谁连谁、token 怎么来、连接断了算什么」**；帧、握手字段、方法、错误闭集、offer 计算、`Methods` 构造（`domain.rs` `build_methods_for`）全部复用。

| 维度 | A1 内核拉起 | **A3 附着** |
|---|---|---|
| 谁启动进程 | `agent24d`（`launch.rs`，fd 3 + 4 个环境变量） | 用户 / launchd / AgentEar 自己；内核不 spawn、不 kill |
| manifest 从哪来 | 包目录 `domain-os.yml`，启动时读 | **注册时**随 REST 请求提交，内核存原文 + digest（§3）；`impl_kind: attached_process`，**不许有** `spawn` |
| 回调 socket | `~/.agent24/run/<pid>/<n>.sock`，0700，**每代只 accept 一次**、accept 后删路径（`endpoint.rs` `accept_one`） | **一个常驻监听** `~/.agent24/attach/agent24d.sock`，目录 0700、节点 0600，可多次 accept；每条连接各自握手 |
| token | 每代现铸、经 `A24_HANDSHAKE_TOKEN` 传入，一次性 | 注册时铸造、**长期有效直到撤销/轮换**；内核只存 sha256；AgentEar 存 macOS Keychain |
| 握手 | `initialize`（`initialize.rs` `accept`） | **同一帧、同一字段**：`auth_token` 装附着 token；无新增字段。内核侧校验函数另写 `accept_attached`（顺序不同，§4.3） |
| 内核 → 模块 | HTTP over 模块 UDS（`<n>.l` 作为 fd 3，`kernel_call.rs`/`proxy.rs`） | **同一条回调连接上的 JSON-RPC 请求**（§6）；模块不监听任何 socket |
| 入站 REST 反代 `/api/v1/<ns>/*` | 有 | **无**（命令走 §6 的固定内核路由）；附着代在类型层就进不了 proxy（§5.1） |
| 连接断开 | D1：连接即生命线，断 = 这一代结束，进程被停 | 断 = 这一代结束（generation 撤销），**进程不受影响**，可用同一 token 重连成新一代 |
| 停止 | 两阶段 drain（SPEC-ME3 §4） | **无 drain**：disable / 撤销 / 轮换 / daemon 退出一律**立即撤销**（§5.3） |
| 重启/熔断 | `RestartPolicy` + 熔断 | 无（内核不拉起）；重连退避由模块自己做（§5.6） |
| 单实例 | 每模块一个 supervisor | 每模块同时最多一代；后到者握手被拒 `busy`（Q3，§5.4） |
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
version: "3"                         # 附着 manifest 自身修订号（见下），不是 app 版本
route_namespace: /api/v1/agentear   # 仍按既有规则由 name 推导并校验；A3 下不挂路由，只占名
event_module: agentear
data_dir: ~/.agent24/os/agentear/   # 同上，只校验不使用（AgentEar 用自己的 ~/.agentear/）
impl_kind: attached_process          # 新变体；与 spawn 互斥（有 spawn → 拒绝）
kernel_capabilities: [events, models]
model_access: local_only             # 可省略，缺省即 local_only（ME4-S2 §2.1）
host_commands: [speak, stop_playback] # 新字段，可选；内核只转发此处列出的命令名，名字语法 [a-z0-9_]{1,32}
```

- `agent24-domain` 改动：`ImplKind::AttachedProcess`、`RawManifest.host_commands: Vec<String>`（`deny_unknown_fields` 下旧 daemon 会拒绝新 manifest，这是期望行为）。`host_commands` 只允许出现在 `attached_process` 上（A1 的反向通道是 HTTP，不需要）。
- **`version` 的语义（AgentEar 对齐 1）**：是**这份附着 manifest 自身的修订号**，不要求等于 AgentEar app 版本；只在能力、命令、隐私声明变化时改。理由：digest 取自 manifest 原始字节，`version` 跟 app 版本走会让每次升级 app 都改 digest、触发一次重新配对（§5.6）。内核对 `version` 只做既有的语法校验，不比较大小。
- 名字 `attached` 被保留（§3.2 M7），任何形态都不能用。

### 3.2 REST 端点（全部在既有 bearer 鉴权之后）

REST 只监听 `127.0.0.1` 且要求 bearer（`server.rs:664` `auth`，token 在 `~/.agent24/daemon.json`）。「已认证 CLI」即持有该 bearer 的 `agent24` CLI；**附着 token 永远不能用来调 REST**，两者不是一套凭据。

| 方法 & 路径 | 作用 | 返回 |
|---|---|---|
| `POST /api/v1/attached` body `{"manifest": "<domain-os.yml 原文>", "allow_relax"?: true}` | 校验 manifest → 名字互斥检查 → 隐私放宽检查（§3.5）→ 铸 token（`launch::mint_token`，32 字节）→ 存记录 → **明文只在本响应出现一次** | 新注册 `201 {"name","manifest_digest","token","socket_path","token_id"}`；已存在同名附着记录 → 按 §3.4 轮换或重新注册，返回 `200` 同形；同名已装包/编入模块 → `409 name_taken`；manifest 非法 → `400 invalid_manifest`；放宽未确认 → `403 relax_requires_confirmation` |
| `DELETE /api/v1/attached/{name}` | 撤销：删记录 + 撤销现有一代并断开 | `204`；不存在 `404` |
| `GET /api/v1/os`（既有） | 列表里附着模块多一类 `kind: "attached"`，带 `attach_status`（§5.1）、`generation`、`manifest_digest`、`token_id` | **从不**含 token 或其哈希 |
| `PATCH /api/v1/os/{name}`（既有 enable/disable） | 对附着模块同样生效；disable = **立即撤销**并断开，之后握手被拒 `forbidden`（§5.3） | 同既有 |
| `POST /api/v1/os/{name}/commands/{command}` | 反向命令，见 §6 | — |

**M7：为什么注册端点不放在 `/api/v1/os/attached`**。v1 的路径会被 axum 的静态段优先规则遮蔽名为 `attached` 的模块：`PATCH /api/v1/os/attached` 命中静态路由得 `405`（进不了 `/{name}`），`POST /api/v1/os/attached/stop` 被 `DELETE /api/v1/os/attached/{name}` 截走。两种修法里**改路径到 `/api/v1/attached` 最简单**：只需在既有 `RESERVED_KERNEL_SEGMENTS`（`domain.rs:181`，校验点 `domain.rs:1084`）加一项 `"attached"`，既有的 `reserved_segments_match_the_kernel_routes_exactly` 测试会**自动**把它与路由表钉在一起（漂移任一方向即红）；另一条路（保留 `/os/attached` 再单开一张「os 子路径保留名」表）要新增一种保留机制和它自己的漂移测试。副作用相同：模块名 `attached` 不可用。

### 3.3 存储

`~/.agent24/attached.json`，写法与 `os.json` 相同（`os_config.rs`：`.lock` 文件锁 + 临时文件 + rename），文件 0600：

```json
{"version":1,"modules":{"agentear":{
  "manifest_yaml":"<原文>","manifest_digest":"sha256:<hex>",
  "token_sha256":"<hex>","token_id":"tok_<8 hex>","created_at":"<RFC3339>"}}}
```

- 只存 `sha256(token)`；比对时对**提交的 token 做 sha256 后常量时间比较**。`token_id` 是随机短 id，每次铸 token 都换新，用于日志/展示区分轮换，并作为 §5.2 提交步骤的复核键；不可反推 token。
- 判据 C1 断言：文件里 grep 明文 token = 0 次，grep 其 sha256 = 1 次（正对照）。

### 3.4 再次 add 同名：token 轮换 vs 重新注册（H4）

对已有记录再 `POST /api/v1/attached`，按新旧 `manifest_digest` 是否相同分两种，都在注册表同一把锁内完成（①改记录并落盘 ②若有现役一代：立即撤销 ③关闭连接），此后旧 token 握手一律 `auth_failed`：

| | digest 不变 = **token-only 轮换** | digest 变 = **重新注册** |
|---|---|---|
| token / token_id | 新铸 | 新铸 |
| `ModelGrant`（含限速桶、健康表） | **沿用**（重连不能刷新模型配额，与 A1「按挂载存续期」一致） | **丢弃并按新 manifest 重建** |
| `Grants`、offer、`EventSink` | 沿用 | **丢弃并重建** |
| 隐私放宽检查（§3.5） | 不适用（manifest 未变） | 适用 |

重新注册必须重建全部派生物，否则会出现「新 manifest 已是 `local_only`，但沿用的 `ModelGrant.privacy` 仍是 `RemoteAllowed`」这类过期授权——正是 C8 新加对照要抓的。实现上派生物挂在注册表 entry 上、与记录同生共死，不单独缓存。

`DELETE` 同理：同一把锁内删记录并落盘、撤销现役代、丢弃 grant 等派生物。`Generation::revoke` 是 `pub(crate)`，所以代际管理放在 proto 新文件 `attach.rs`（§5.2 的 `AttachSlot`），agent24d 只调用它。

### 3.5 隐私放宽需要宿主侧显式确认（M4）

Q2=b 让 AgentEar 代跑 CLI 配对；这不能变成「模块自己给自己放宽隐私」的通道。定义**放宽** = 首次注册即 `model_access: remote_allowed`，或相对现有记录：`model_access` 由 `local_only` 变 `remote_allowed`、`kernel_capabilities` 新增任一项。

- REST：放宽的请求必须带 `"allow_relax": true`，否则 `403 relax_requires_confirmation`（记录不变，现役代不受影响）。
- CLI：`agent24 os attach add` 只有同时满足 ①带 `--allow-remote` ②stdin 是 TTY 且用户在提示下输入 `yes` 时才发 `allow_relax: true`；非 TTY（AgentEar 代跑）一律不发，于是放宽必失败，退出码非 0，`--json` 下 stdout 为 `{"error":{"code":"relax_requires_confirmation",…}}`。
- 桌面端可在自己的确认对话框之后调 REST 带 `allow_relax`（P2 不做这个 UI，放宽只走终端）。
- **只有不放宽的注册（含全部 `local_only` 且能力不增的注册）允许 AgentEar 静默配对。**
- 边界（照实写）：同 UID 进程读得到 `daemon.json` 的 bearer，可以直接调 REST 带 `allow_relax`——SPEC-ME3 §0 本就不防同 UID。本条防的是**自动化流程无人确认地放宽**（包括 §5.6 的自动轮换），不是防恶意同 UID 进程。

### 3.6 CLI 契约（AgentEar 对齐 6）

- **定位**：AgentEar 找 `agent24` 可执行文件的顺序 = AgentEar 设置里可覆盖的路径 → `~/.agent24/bin/agent24` → `PATH`。
- **兼容检查**：`agent24 --version` 输出 semver，要求 ≥ **`<A3_MIN_VERSION>`**（引入 `os attach` 的版本；占位，A3-2 合并时填入并同步 AgentEar）。低于此版本 → AgentEar 提示升级 Agent24，不尝试配对。
- **`agent24 os attach add <manifest.yml> --json`**：成功时 stdout 恰为一个 JSON 对象，与 §3.2 的 `201`/`200` 响应体**同形**：`{"name","manifest_digest","token","socket_path","token_id"}`（新注册与轮换同形；AgentEar 不需区分）。失败时退出码非 0，stdout `{"error":{"code","message"}}`，`code` ∈ `name_taken | invalid_manifest | relax_requires_confirmation | daemon_unavailable | …`（未知 code 按一般失败处理）。
- `agent24 os attach revoke <name> [--json]`；`agent24 os list` 展示附着状态。
- A3-2 为 `add --json` 的成功与 `relax_requires_confirmation` 两种输出各加一份 CLI 快照测试（字段集合与类型钉死，token 值打码）。

## 4. 连接与握手 wire 规格（独立可实现）

本节与 §5.6、§6 是 AgentEar 实现的唯一依据；与 A1 共用的条文在 SPEC-ME3 §3 有出处，此处**完整重述**，以本文为准。

### 4.1 传输

- Unix 域流 socket，路径 = 注册响应里的 `socket_path`（当前为 `~/.agent24/attach/agent24d.sock`，展开后的绝对路径）。模块应保存该路径，不要硬编码。
- 内核只接受**同 UID** 对端（`peer_cred`），否则直接关闭。模块不监听任何 socket、任何端口。
- daemon 不在时 `connect` 失败（`ENOENT`/`ECONNREFUSED`）= 宿主离线，按 §5.6 处理。

### 4.2 帧

- 一帧 = 一个 UTF-8 JSON **对象**，以 `\n` 结尾；负载（不含 `\n`）≤ **1 MiB**（`frame.rs:84` `MAX_FRAME_BYTES = 1048576`）。超长 → 内核断开连接；最后一行没有 `\n` 不算帧。
- **不支持 batch**（数组 → `-32600`）。`jsonrpc` 必须是 `"2.0"`。请求对象只允许 `jsonrpc`/`id`/`method`/`params` 四个成员；`params` 必须是对象（可省略）。
- `id` 是**字符串**、≤256 字节（`rpc.rs` `MAX_ID_BYTES`），同一方向在途 id 不得重复。两个方向的 id 空间**互相独立**；区分帧的唯一依据：**有 `method` = 请求/通知；无 `method` = 对某个请求的响应**。
- **内核对模块发来的「响应」**：只认 `id` 命中内核待决调用的；**其余一律静默丢弃并计数，不回任何帧**（包括迟到的、`id` 未知的、`id` 非字符串的）——见 §6.2。
- 响应不保证顺序，按 id 匹配。可选字段不发时**省略**，不要发 `null`。**解析对方的响应时忽略未知字段**；内核解析请求 `params` 时**拒绝**未知字段（`-32602`）。
- 取消：通知 `{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":"<在途 id>"}}`，只能取消自己发出的请求。效果见 §4.4。

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
- **没拿到 `_a24/model/`（AgentEar 对齐 2）**：宿主未授予模型（manifest 没声明、或 daemon 无模型依赖）。AgentEar **不得回落到任何可能出本机的推理路径**；若其独立推理档是本机边车（回环地址），视同 `local_only` 可用、可继续用它，并 emit 一条 `error{code: "forbidden", message: "宿主未授予模型"}` 事件告知宿主。
- 内核校验分两段（H2、M3）：

**① 纯校验 `accept_attached`**（无锁、无副作用，只产生一个「声称」）。顺序固定、测试断言：JSON/信封形状 → `params` → 按 `module` 查注册记录 → **token 哈希**（查不到记录时与一个固定哑哈希做同样的常量时间比较，再回 `auth_failed`，使「未注册」与「token 错」在结果和耗时上都不可区分）→ `manifest_digest` → 版本。token 先于 digest：不持有 token 的一方拿不到任何关于 digest 的信息（v1 顺序下，digest 错回 `manifest_mismatch`、digest 对才回 `auth_failed`，构成注册/digest 预言机）。返回 `{module, token_id, accepted}`。

**② 提交 `AttachRegistry::commit`**（注册表**同一把锁**内一步完成）：按 `module` 复核记录仍在且 `token_id` 与 ① 通过的相同（未被撤销/轮换）→ 未被 disable → 该模块无现役代 → `Generation::attached()` 装入 slot → `ready()`。任一不符分别回 `auth_failed` / `forbidden` / `busy`，连接关闭。撤销、轮换、disable 也在这把锁里进行，所以「握手通过的一刻记录已被删」不会装入新代：要么提交先拿到锁（随后撤销会撤掉它），要么撤销先拿到锁（提交复核失败）。

| 失败 | `error.code` | `error.data.kind` | 模块该怎么办（§5.6） |
|---|---|---|---|
| 首帧不是合法 JSON | `-32700` | — | 实现缺陷，停止重连并上报 |
| 不是 `initialize` 请求 / 信封非法 | `-32600` | — | 同上 |
| `params` 解析失败（缺字段、未知字段、重复键） | `-32602` | — | 同上 |
| 未注册的 `module`，或 token 不对，或提交时发现已撤销/轮换 | `-32000` | `auth_failed` | 停止重连，提示重新配对 |
| digest 与注册记录不符 | `-32000` | `manifest_mismatch`（**message 固定，不回显注册的 digest**） | 触发自动轮换（§5.6），失败则停止 |
| 版本区间无交集或未声明 | `-32000` | `version_mismatch`（`data` 另含 `module`/`kernel` 两个区间） | 停止重连，提示升级 |
| 已注册但被 `os disable` | `-32000` | `forbidden` **（A3 新增用法）** | 停止重连，提示「已在 Agent24 停用」 |
| 同名模块已有现役一代 | `-32000` | `busy` **（A3 新增用法）** | 继续按退避重连 |

`HandshakeError` 加三个变体 `Busy`、`Forbidden`、`AttachedDigestMismatch`（后者 kind 仍是 `manifest_mismatch`，只是消息不带 expected；A1 的 `ManifestMismatch{expected, got}` 不动——A1 的 digest 是内核自己算后交给子进程的，不是秘密）。`kind` 全部取自既有 `ErrorKind`，**18 值的 wire 闭集不变**，AgentEar `agentear.event/1` 的 `error.code` 枚举无需改动。

### 4.4 握手之后：方法表（本期）

| 方向 | 方法 | params → result | 限制（来源） |
|---|---|---|---|
| 模块→内核 | `_a24/events/emit` | `{kind, payload}` → `{}` | `kind` 为点分小写 ASCII `[a-z0-9_-]+(\.[a-z0-9_-]+)+`、≤96 字节；`payload` 对象 ≤256 节点且字符串（含键）≤8 KiB；令牌桶 20 突发 / 5 每秒（`events_emit.rs`），超限 `rate_limited` |
| 模块→内核 | `_a24/model/complete` | `{messages:[{role:system\|user\|assistant, content}], complexity?: simple\|complex, max_tokens?: 1..=4096（缺省 1024）, response_format?, _meta?}` → `{text, model_id, tier: local\|remote, usage:{prompt_tokens, completion_tokens}}` | messages ≤64；内核超时 120 s；每模块并发 2、全局 4；令牌桶 30 突发 / 0.5 每秒（`model_callback.rs:35-49`） |
| 内核→模块 | `_a24/command/invoke` | `{name, body}` → JSON **对象** | 见 §6；内核等待 5 s |

**`request_id` 在 A3 下无意义，模块应省略**（M1）：附着代没有入站代理请求，也就没有在途的 `request_id`。带了的后果：`model/complete` 得 `-32000 timeout`（`retryable: false`，「request_id is not in flight」，`model_callback.rs:641-649`）；`events/emit` 被接受但该字段只是原样透传到 WS，没有任何生命周期语义。

**超时与取消（AgentEar 对齐 3，按代码核查）**：模块若要比内核的 120 s 更早放弃（AgentEar 语音超时默认 60 s），**应发 `$/cancelRequest`**，而不是单方面丢弃等待。内核的实际行为（`main@f504ae0`）：

1. `rpc.rs:1279-1285`：对该 id 的处理任务 `abort()`；任务结束后 `rpc.rs:1326-1333` 回一帧 `{"id":<原 id>,"error":{"code":-32000,"message":"cancelled by $/cancelRequest; …","data":{"kind":"cancelled"}}}`（无 `retryable`）。若处理已先完成，模块收到的是正常结果——**模块应把「发出取消后收到的那一帧」一律当作该调用的终态**，结果照常丢弃即可。
2. 处理 future 被丢弃 → `_cancel_on_drop`（`model_callback.rs:669`）取消 provider 的 `CancellationToken`，本机推理被中止；并发名额（`admission`）随之释放；`UsageTicket` 未 finish 即被丢弃 → 记一条 `UsageOutcome::Cancelled`（`model_callback.rs:263-289`），`GET /api/v1/usage` 可见。
3. **令牌桶：取消的调用计入、不返还**。`try_acquire` 在调用 provider 之前（`model_callback.rs:657`），取消路径上没有 `refund`。**这不会让连续超时耗尽桶**：每模块并发上限 2，故 60 s 超时每分钟至多消耗 2 个令牌，而补充速率是 0.5/s = 30/分钟。能耗尽桶的只有「每分钟发起并取消 30 次以上」，那本身就是应当限流的负载（每次都已启动一次推理）。**结论：不列「取消返还」为实现项**；C8 附一条判据把这个算术钉住（见 §9）。

**事件限流的模块侧处置（AgentEar 对齐 4）**：`emit` 超限得 `-32000 rate_limited`，该事件**确定未广播**。AgentEar 的处置：`turn` / `speech` 等状态事件 → 丢弃并计数；`transcript` / `proposal` / `confirm_reply` → 退避重试且**复用同一 `event_id` 与 `seq`**。被丢弃的状态事件若已占用 `seq`，消费端会看到缺口，由 §7.2 的 2 s 缺口超时兜底；AgentEar 也可在丢弃时回收该 `seq` 给下一条（因为被拒的那条从未广播，消费端不会判 `seq_conflict`）。

错误：`-32601` 方法不存在；`-32602` 参数非法；`-32603` 内部错误；`-32000` + `data.kind` ∈ `ErrorKind::ALL`（`rpc.rs:167`，18 个：`forbidden busy cancelled timeout quota_exceeded invalid_lease unknown_capability version_mismatch auth_failed manifest_mismatch not_ready draining revoked rate_limited payload_too_large token_invalid not_found unavailable`），可带 `data.retryable: bool`。`unavailable` 另带 `data.cause` ∈ `no_provider | request_rejected | backend_config | response_too_large`。`request_not_in_flight`/`connection_lost`/`not_sent` 是 SDK 的 `ClientError`，**不是** wire 值。

### 4.5 通知（M5）

- **P2 内核不向模块发送任何通知**，包括 `$/cancelRequest`：内核的命令调用 5 s 超时后只是放弃等待（REST 回 `504`），不通知模块取消；模块迟到的应答按 §4.2 被丢弃。
- 模块**必须忽略**收到的任何未知通知（带 `method` 无 `id`），不回帧、不断开——为将来内核增加通知留余地。
- 模块发给内核的通知只有 `$/cancelRequest` 有意义；其它通知按 `serve_until` 既有规则忽略。

## 5. 生命周期

### 5.1 状态机与附着代（H1）

```
            add（REST）                    握手提交成功（gen = N+1）
(无记录) ───────────► Detached ──────────────────────────► Attached(N+1)
   ▲                    ▲  ▲                                 │
   │ revoke             │  └── 连接断开 / 写失败 / 超长帧 ─────┤
   │（任意状态）         │      轮换 / 重新注册（立即撤销）     │
   └────────────────────┴────── disable / daemon 退出（立即撤销）──► Disabled / （进程结束）
```

- `attach_status` ∈ `detached | attached | disabled`，经 `GET /api/v1/os` 暴露（v1 的 `draining` 删除，见 §5.3）。不复用 supervisor 的 `Status`（那是进程视角：Starting/Backoff/GaveUp…，附着没有这些）。
- **附着代是 `Generation` 的第三种形态**。v1 写的「无 upstream 的 `Generation::starting()` → ready」**不成立**：`drain.rs:379` `ready()` 在 `upstream.is_none()` 时返回 `false`，`starting()` 是永不 Running 的占位代。v2 改为：`Generation` 的私有字段 `upstream: Option<PathBuf>` 改成三态 `Upstream::{Process(PathBuf), Attached, Placeholder}`；新增构造函数 `Generation::attached()`；`ready()` **只拒 `Placeholder`**。公开的 `upstream() -> Option<&Path>` 签名不变，`Attached` 与 `Placeholder` 都返回 `None`。
- **附着代不可能进入 proxy**（三道）：①附着模块从不挂 `/api/v1/<ns>/*` 路由（`mount_all` 不认 `attached_process`，§5.4）；②`admit_request` 对 `Attached` 代恒拒 `RequestRefused::NotReady`（新增一行判断 + 单元测试）；③即使被错误放行，`proxy.rs:1476` 与 `kernel_call.rs:274` 在 `upstream()` 为 `None` 时已拒绝（502 / `NoUpstream`）。回调准入 `admit_callback*` 对 `Attached` 与 `Process` 代行为相同。
- **generation 编号**：每次提交成功 +1，在该模块内单调递增（内存计数；daemon 重启后从 1 重新计，编号只用于日志与判据，不是凭据）。所有回调的准入照旧走 `admit_callback`：旧代被撤销后，它在途与新发的回调一律 `revoked`（ME-3b-5 语义原样复用）。
- `Methods` 由 `build_methods_for` 按代构造：`events` 令牌桶每代新建；`models` 的 `ModelGrant`（含限速与健康表）挂在注册表 entry 上，**按注册存续期**只建一次，重连不刷新配额；重新注册时重建（§3.4）。

### 5.2 握手提交与撤销的原子性（H2）

注册表（agent24d）= 一把 `Mutex`，保护 `HashMap<name, Entry { record, slot: AttachSlot, grant, … }>`。proto 新文件 `attach.rs` 提供 `AttachSlot`，其 `install`/`revoke`/`release` 都取 `&mut self`——**调用方的注册表锁是唯一的同步手段**，所以「复核记录 + 装入代」在类型上就不能被拆成两步。在这把锁里发生的操作：握手提交（§4.3 ②）、`DELETE`、add 轮换/重新注册、disable/enable、连接结束时的 `release`（只清掉仍是自己那一代的 slot）、关机 `revoke_all`。

握手侧的昂贵部分（解析、sha256、常量时间比较）全部在 ① 纯校验里、锁外完成，锁内只有几次哈希表查找与一次 slot 更新。

### 5.3 断连与停止：一律立即撤销，无 drain（M1）

- **连接断开**（EOF、读写失败、超长帧、写超时 10 s）：立即撤销该代（对端已经没了），在途 `model/complete` 随之取消，状态回 `Detached`。内核不做任何「等它回来」。
- **disable、revoke、轮换、重新注册、daemon 退出**：一律**立即撤销**当前代并关闭连接，在途回调随连接关闭而取消（ME4-S2 §3.3 的取消路径照旧，provider 端观测到取消、usage 记 `Cancelled`）。模块看到的只是 EOF（disable 后重连再得 `forbidden`）。
- 为什么不 drain：A1 的 drain 是为了让**内核转发给模块的在途入站请求**有机会完成并带着回调；附着代没有入站请求（§4.4），唯一可能在途的是模块自己发起的 `model/complete`，那是后台工作，按 `Draining` 表本就会被拒——v1 的 10 s drain 只是空转，徒增退出时长。

### 5.4 单实例与互斥（Q3=a）

- 同一模块同时最多一代：现役代存在时，新连接在提交步骤得 `busy`（**先到者保留**，jason 拍板 Q3=a）。卡死场景（旧连接未断但进程无响应）用 `agent24 os attach revoke` 或重启 AgentEar 解决。AgentEar 自身也有单实例锁，这一条是第二道。
- 与 spawn 互斥：注册时同名已安装包或编入模块 → `409`；`mount_all` 遇到与附着注册同名的包 → `MountOutcome::Refused("registered as an attached module")`。名字是唯一身份（与 A1 相同的推导规则），因此路由命名空间、事件模块名天然不会冲突。

### 5.5 运行期依赖与关机顺序（M2）

- **依赖**：`build_methods_for` 需要 `CallbackDeps`（`domain.rs:142`：scheduler、`models: Option<ModelCallbackDeps>`）。今天它只在 `serve()` 启动的挂载阶段被消费。A3-2 在 `AppState` 保留一份 `CallbackDeps` 克隆，交给注册表，运行期握手提交时用它构造 `Methods` 与 `ModelGrant`。`ModelCallbackDeps.cancel_root` 是 `spawn_cancel_root(shutdown.modules_cut_off())`（`server.rs:1343`）的同一棵树，附着模块的模型调用因此也在 `modules_cut_off` 时被取消。
- **关机顺序**：注册表的「`revoke_all` + 丢弃全部 grant 与 deps 克隆 + 关闭监听 socket」并入 `stopping` 任务（`server.rs:1209`），与 supervisors 的 `close()` 同一阶段，**先于** `stop_usage_writer`（`server.rs:1503` 及正常退出路径）。这样：①取消产生的 `UsageOutcome::Cancelled` 在 usage writer 停止前入队、被落盘；②usage writer 的最后一个 `Arc` 不会被注册表里残留的 deps 克隆拖住。

### 5.6 daemon 重启与模块侧重连（M5 合并表）

- daemon 重启：注册记录从 `attached.json` 读回，token 依旧有效，监听 socket 在同一路径重建（启动时若路径已是 socket 节点且连不上 → 视为残留，unlink 后 bind；连得上 → 另一 daemon 在跑，附着监听降级并记录错误）。所有模块回到 `Detached`，等它们重连。
- 模块侧（AgentEar 实现约束）：EOF 后按 B5 切换本地行为；后台以 1 s 起、翻倍、上限 30 s 的退避重连；重连成功是**新会话**：新的 `session_id`、`seq` 从 1 重新开始（`agentear.event/1` 已如此定义）。

**完整处置表**（模块在握手或会话中收到/遇到下列情况）：

| 情况 | 模块处置 |
|---|---|
| `connect` 失败（`ENOENT`/`ECONNREFUSED`）、EOF、读写错误、握手 5 s 超时 | 退避重连（1 s → 30 s） |
| `busy` | 退避重连（另一实例在线；它断开后本实例可接上） |
| `manifest_mismatch` | **自动轮换一次**：若本地 manifest digest 与上次注册记下的不同，代跑 `agent24 os attach add <manifest> --json`（§3.6），成功则用新 token 重连；若新 manifest 属于放宽（§3.5），CLI 必然失败于 `relax_requires_confirmation` → 停止重连，设置里提示「需要在 Agent24 侧确认权限变更」；若 digest 与上次相同却仍不符，或轮换失败 → 停止重连，提示重新配对 |
| `auth_failed` | 停止重连，提示重新配对（凭据问题，重试无意义） |
| `version_mismatch` | 停止重连，提示升级 AgentEar 或 Agent24 |
| `forbidden`（握手时） | 停止重连，提示「已在 Agent24 停用」，设置里给手动重连按钮 |
| `-32700`/`-32600`/`-32602`（握手时） | 实现缺陷：停止重连并记录 |
| 会话中调用得 `revoked` | 视同即将 EOF，等 EOF 后按第一行 |

- **AgentEar 的自动轮换判定**（AgentEar 对齐 1）：AgentEar 保存上次成功注册时的 `manifest_digest`；启动或收到 `manifest_mismatch` 时比较本地 manifest 的 digest，不同即代跑 `attach add`。内核不为此新增 error kind。
- **B5（已决，jason 2026-09-27）**：断连后，若独立模式的推理配置为本机边车 → 自动回独立模式；否则停听并提示；设置可改。任何情况下不得把附着时受 `local_only` 约束的请求改发远端。

## 6. 反向命令（`speak` / `stop_playback`）

### 6.1 路由与 REST 映射

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
- **REST 映射（M5 补全）**：

| 模块应答 / 情况 | REST |
|---|---|
| `result` 是 JSON 对象 | `200 {"result":…}` |
| 合法的 JSON-RPC `error`（`code` 整数、`message` 字符串） | `502 {"error":{"code":"module_error","message","rpc_code","kind"?}}` |
| `error` 形状非法、既无 `result` 也无 `error`、两者都有、`result` 不是对象 | `502 {"error":{"code":"module_error","message":"malformed response"}}`（`rpc_code` 缺省） |
| 5 s 无应答 | `504 timeout`（**结果未知**；内核不发 `$/cancelRequest`，§4.5） |
| 已写出后连接断开 | `502 connection_lost`（结果未知） |
| 未写出（连接已断/代已撤销） | `503 module_not_ready`（确定未送达） |
| 在途内核调用已达 8 | `429 busy`（零帧下发） |

### 6.2 实现落点：`serve_attached`（proto 新文件 `attach_mux.rs`）

`rpc::serve_until` 已被多轮评审固化，**不改**。附着连接在它外面包一层：一个读任务按下列规则分拣帧——

| 帧 | 去向 |
|---|---|
| 不是 JSON 对象（解析失败、数组、标量） | 交给 `serve_until`（照旧回 `-32700`/`-32600`，与 A1 一致） |
| 对象且有 `method` | 交给 `serve_until` |
| 对象、无 `method`、`id` 为字符串且命中待决内核调用 | 交给对应等待者（按 §6.1 表判定形状） |
| 对象、无 `method`、其余一切（迟到、未知 id、非字符串 id） | **丢弃并计数（`stray_responses`），永不转给 `serve_until`，不产生任何回写帧**（H3） |

v1 把「未命中的响应」喂回 `serve_until`，后者会以 `-32600` 回写一帧，这一帧带着内核自己的 id 空间里的 id 落进模块的 id 空间，模块可能误当作对它自己某个在途请求的应答。v2 直接丢弃。写方向由一个写任务按**整行**合并 `serve_until` 的输出与内核请求。签名（scratch 已 check）：

```rust
pub fn serve_attached<R, W, S>(reader: R, writer: W, methods: Methods, limits: Limits, stop: S)
    -> (KernelCalls, impl Future<Output = Ended> + Send)
where R: AsyncBufRead + Unpin + Send + 'static, W: AsyncWrite + Unpin + Send + 'static,
      S: Future<Output = ()> + Send + 'static;

impl KernelCalls {
    pub async fn call(&self, method: &'static str, params: Value, timeout: Duration)
        -> Result<serde_json::Map<String, Value>, KernelCallFailed>;
        // NotSent | ConnectionLost | Timeout | Rpc(RpcError) | Malformed(String)
    pub fn stray_responses(&self) -> u64;
}
```

`stop` 由 agent24d 传入 `generation.revoked()`（`drain.rs:669`），撤销即停止服务并关闭连接。

### 6.3 幂等

- **内核从不自动重发命令**（命令有副作用：说话）。调用方拿到 `504`/`502 connection_lost` 时可**用同一 `command_id` 重试**。
- **AgentEar 负责按 `command_id` 去重**：至少记住本进程最近 256 个 `command_id` 及其首次应答（下限；AgentEar 实现为 512，兼容），重复送达直接回首次结果、不再执行（契约已写）。进程重启后缓存丢失属可接受残余：最坏多播一句。
- `stop_playback` 在无播放时也回 `accepted`；它和用户按键打断都清空 `speak` 队列（B7）。

## 7. 事件接入与 P2 宿主展示

### 7.1 发送

AgentEar 用 `_a24/events/emit`，`kind` 固定为 **`agentear.event`**，`payload` = 完整的 `agentear.event/1` 对象（含 `schema`/`event_id`/`session_id`/`seq`/`type`/`payload`）。内核照旧把它包成 WS `type:"module"` 事件（`module:"agentear"`，`kind`，`payload` 原样）广播到 `GET /api/v1/events`；**内核不理解、不校验这份 schema**（与 ME-3e 的「内核只做通用承载」一致）。

- 大小：`transcript.text` 连同其它字符串须在 8 KiB 内（中文约 2600 字），超出得 `payload_too_large`；AgentEar 截断并在文本末尾标注，不拆成多条。
- 重试复用 `event_id` 与 `seq`。**AgentEar 按 `seq` 顺序逐条等待 `emit` 应答再发下一条**（同一 session 内不并发 emit），这样线上顺序即 seq 顺序；宿主的重排只是兜底。限流处置见 §4.4。
- AgentEar 附着模式下把 `proposal` 的 `commands_path` 相对化（AgentEar 对齐 5，已知悉，Agent24 侧无需改动：P2 只展示，不解析路径）。
- **`action.type = builtin` 的 proposal**（切语系/语气/模式、记一条；AgentEar PR #96 已按 v1 草稿实现）：附着模式下 AgentEar 照常 emit，但**已在 AgentEar 本地执行过**。宿主对 builtin 提案**只展示、永不执行**——P2 面板显示为「已在 AgentEar 本地执行」；P3 的执行门（gate 可执行集合、确认 UI）也必须**排除 builtin**，否则同一动作会被执行两次。

### 7.2 去重与排序（宿主消费端）

内核是通用承载，**`(session_id, seq)` 去重放在理解该 schema 的消费端**——P2 即桌面端主进程的 `agentearSequencer`（纯函数 + 状态，vitest 用 AgentEar fixtures 测）：

- 未知 `schema` 版本 → 丢弃并计数（`fixtures/*/invalid/unknown_version.json`）。
- 同 `(session_id, seq)` 且同 `event_id` → 丢弃（重试）；不同 `event_id` → 拒收后到者并记一条协议违规（`sequences/seq_conflict.json`）。
- 乱序：每 session 缓冲至多 32 条、缺口最多等 2 s，超时跳过缺口并记录；按 seq 交付（`sequences/reorder.json`）。
- **fixtures 引用方式：vendored 副本 + 记录 commit**（最简单：CI 不联网、不跨仓库拉取）。基准 = AgentEar `main@522f9eba0f720c97b9bb0a63d059a82e33029dcb`（3 份 schema + 57 个 fixtures；`error.code` = wire 18 + AgentEar 自有 4；字段拼写 `retryable`）。做法：`git -C <AgentEar> archive 522f9eb contracts/ | tar -x -C apps/desktop/test/fixtures/agentear/`，同目录写 `SOURCE`（完整 sha + 拷贝日期）；vitest 首条断言 `SOURCE` 存在且 sha 为 40 位。升级 = 换 sha 重新拷、同一 PR 里改测试，不跟 `main` 浮动头。Rust 侧假 AgentEar 的事件样例也从这份副本读，不另写。

### 7.3 P2 最小范围（宿主侧）

依 embedding.md §3「模型：AgentEar 调 `_a24/model/complete`」：**P2 的对话由 AgentEar 自己编排——自己调模型、自己播；宿主只提供能力、实时展示事件、接受 `speak`。**

**转写记录的归属方是 AgentEar**（Q4=a，jason 拍板）：AgentEar 默认已保存转写；Agent24 **只在桌面面板实时显示**，daemon 与桌面端都**不写数据库、不写日志、不写任何文件**。桌面端为显示在内存里保留最近 200 条，关窗或重启即丢。

| 做 | 不做（后续） |
|---|---|
| 附着注册/握手/生命周期/撤销（§3–§5） | 宿主替 AgentEar 编排对话、把对话挂到 `/api/v1/sessions` |
| `events`、`models` 两个 capability；`local_only` | `memory`（多轮）、`approval`（P3） |
| 桌面端「语音」面板：主进程订阅 WS、经 sequencer 后实时显示 transcript、turn 相位、speech 状态、error；显示本模块 `GET /api/v1/usage?module=agentear` 的 tier 统计 | transcript 任何形式的持久化（Q4：归 AgentEar） |
| 面板上一个「让它说」输入框 → `POST …/commands/speak`；「停止」→ `stop_playback` | proposal 的确认 UI、执行、回执留档（P3）；P2 面板只把 proposal 列出来并标「P3 前不执行」，builtin 提案标「已在 AgentEar 本地执行」（§7.1，永不由宿主执行） |
| `agent24 os attach add/revoke`，`os list` 状态 | **模型回复文本在宿主展示**（Q1=a，放 P3，届时与回执留档一起设计，不为展示先改 `agentear.event/1`） |

## 8. 隐私

- `model_access` 缺省 `local_only`，只由注册时的 manifest 决定；握手里的 `capabilities`、调用里的 `complexity` 都**不能**提高隐私级别（ME4-S2 §2）。改成 `remote_allowed` 必须重新注册，且须宿主侧显式确认（§3.5）；重新注册会重建 `ModelGrant`（§3.4），反向（`remote_allowed` → `local_only`）立即生效。
- `local_only` 无本地 provider → `unavailable` + `cause: no_provider`，**不回落远端**；AgentEar 不得把它转成独立模式直连远端的请求（B5 只允许「独立模式本身就是本机边车」时回退）。未被授予 `models` 时同理（§4.3）。
- 内核日志只记元数据：模块名、generation、方法名、字节数、耗时、错误 kind；**不记** transcript、messages、回复、命令 body、token。`token_id` 可记。
- 事件经 WS 只到本机已认证客户端；Agent24 全链路不落盘（Q4）。
- **零远端流量验收**见 C8：路由器里放一个计数远端桩（非回环地址 → `Remote` 层，D2），`local_only` 全流程计数必须为 0，并用 `remote_allowed` 的对照模块证明计数器本身会涨。

## 9. 核心判据

约定：每条 `cargo test <过滤>` 先跑 `-- --list` 断言非空；每条带正对照（标 ⊕）；「假 AgentEar」= `rust/apps/agent24d/tests/fixtures/fake_agentear.py`，**只按本文 §4/§5.6/§6 写成**（不 import 任何 Agent24 代码）。并发判据（标 ∥）用固定种子、N ≥ 200 次交错。

| # | 判据 | ⊕ 正对照 |
|---|---|---|
| C1 注册 | `POST /api/v1/attached` 返回 token 仅一次；`attached.json` 中 grep 明文 token = 0；`GET /api/v1/os` 不含 token/哈希；同 manifest 再 add → 旧 token 握手 `auth_failed`、`token_id` 变、`ModelGrant` 为同一实例（`Arc::ptr_eq`）；非 TTY `add` 一份 `remote_allowed` manifest → `403 relax_requires_confirmation` 且记录不变；模块名 `attached` → `400 invalid_manifest` | grep `sha256(token)` = 1；新 token 握手成功；带 `allow_relax` 的同一请求 `201` |
| C2 握手 | 错 token / 未注册名 → `-32000 auth_failed` 且连接关闭，两者的应答字节除 id 外相同；digest 不符（token 正确）→ `manifest_mismatch` 且应答中不含注册的 digest；digest 不符且 token 错 → `auth_failed`（token 先于 digest）；disable 后 → `forbidden`；`admit_request` 对 `Attached` 代 → `NotReady` | 正确 token → `offer.provides` 恰为 `["_a24/events/","_a24/model/"]`（测试 daemon 须配模型依赖：`_a24/model/` 只在 `model_grant` 为 `Some` 时列出，`domain.rs:1739`），**且握手后第一次 `_a24/events/emit` 返回 `{}`、WS 收到该事件**（证明附着代确实进入 Running，H1） |
| C3 撤销 | `DELETE` 后 ≤1 s 假模块读到 EOF；撤销前挂起的 `model/complete` 以连接关闭结束、provider 端取消被观测到；旧 token 重连 `auth_failed`。∥ 握手与 `DELETE` 交错 N 次：每次 `DELETE` 返回后，该模块已认证连接数恒为 0、slot 为空 | 撤销前同一 token 可重连；无 `DELETE` 的对照组 N 次握手全部成功 |
| C4 单实例/互斥 | 现役代在时第二条连接 → `busy`；∥ 两条连接并发握手 N 次：每次恰好一条成功、另一条 `busy`；与已装包同名注册 → `409`；同名包挂载 → `Refused` | 第一条断开后重连成功且 generation +1；不同名注册 `201` |
| C5 反向命令 | `POST …/commands/speak` → 假模块收到 `_a24/command/invoke`，`body` 与请求体逐字节等价（规范化 JSON）；未声明命令 → `403` 且假模块收到的内核请求帧数 = 0；未附着 → `503`；假模块不应答 → `504`；假模块回 `{"id":…}`（无 result/error）/ `result: 1` / `error: "x"` → 各 `502 module_error` | 声明的命令帧数 = 1；假模块回 `-32602` → `502 module_error` 且 `rpc_code = -32602`；回 `{"accepted":true}` → `200` |
| C6 帧分拣 | 附着连接上模块发「无 method、未知 id」「迟到（内核已 504 之后）」「非字符串 id」三种响应帧 → **内核写出的帧数不变（0 新增）**、`stray_responses` 各 +1；A1 连接行为逐项不变（既有 rpc 测试全绿） | 已知 id 的响应帧被路由到等待者；无 `method` 的非对象帧仍得 `-32600`（证明分拣没吞掉协议错误） |
| C7 事件 | builtin proposal fixture 经 sequencer 后标记为「本地已执行」且不进入任何执行路径；假模块 emit 一条 `transcript` → WS 收到 `module` 事件，`payload` 与发送体相等；桌面 vitest：`sequences/{dedupe_retry,seq_conflict,reorder}.json` 与 `event/invalid/unknown_version.json` 的 `expect` 全部满足；桌面端与 daemon 数据目录在一轮后无新增含 transcript 哨兵串的文件 | `event/valid/*.json` 全部被接受；哨兵串出现在 WS 抓包中 |
| C8 隐私 | `local_only` + 本地桩 + 远端计数桩：完整一轮后远端计数 = 0、结果 `tier=local`；无本地 provider → `unavailable/no_provider`、远端计数 = 0；daemon 日志 grep transcript 哨兵串 = 0；**先以 `remote_allowed`（带 `allow_relax`）注册、再以 `local_only` 重新注册 → 重连后 `complexity: complex` 调用远端计数 = 0**（H4）；**取消算术**：注入时钟，按每模块并发 2、每 60 s 各取消一次，连续模拟 30 分钟 → 无一次 `rate_limited` | `remote_allowed` 注册期间同一调用 → 远端计数 ≥1；取消算术对照：同一时钟下 1 s 内发起并取消 31 次 → 第 31 次 `rate_limited`（证明桶确实计入取消） |
| C9 重启/关机 | daemon 重启后同 token 重连成功，generation 从 1 重新计；残留 socket 节点被清理后监听成功；**daemon 优雅退出时有一条在途 `model/complete`（provider 挂起）→ usage 表中有该模块一条 `cancelled` 行，且进程退出时长 ≤ `deadlines().modules`**（M2） | 重启前后注册记录 digest 相同；无在途调用时退出不新增 usage 行 |
| C10 真机 P2（jason 手动） | 对话模式说一句 → 桌面「语音」面板实时出现该 transcript → AgentEar 播出回复 → `usage?module=agentear` 新增一条 `tier=local` → 远端计数桩 = 0；面板「让它说」能播；按键打断后 `stop_playback` 无报错；AgentEar 设置里一键配对成功且 token 在 Keychain；退出 Agent24 后 AgentEar 按 B5 行为切换、无残留录音/TTS 子进程；Agent24 侧重启后面板为空（未持久化） | 同一句在 AgentEar 独立模式下可正常回答（证明链路本身可用） |

## 10. PR 切法（4 片，stacked）

| 片 | 内容 | 判据 | 估计 |
|---|---|---|---|
| **A3-1** proto + domain | `ImplKind::AttachedProcess`、`host_commands`；`Generation` 三态 upstream + `attached()` + `ready()`/`admit_request` 调整；`initialize::accept_attached`（token 先于 digest、哑哈希、不回显）+ `HandshakeError::{Busy, Forbidden, AttachedDigestMismatch}`；`attach.rs`（`AttachSlot`）；`attach_mux.rs`（`serve_attached`/`KernelCalls`，丢弃未命中响应、`Malformed`） | C2（单元层，含 `admit_request` 拒 Attached）、C6 | ~500 行，含测试 |
| **A3-2** agent24d 注册与生命周期 | `attached.json`；注册表（同锁提交、轮换 vs 重新注册、`revoke_all`）；`POST/DELETE /api/v1/attached` + `allow_relax`；保留名 `attached`；`AppState` 持 `CallbackDeps` 克隆；常驻监听、立即撤销、重启恢复；`stopping` 内关机顺序；CLI `os attach add/revoke`、`--allow-remote` + TTY 确认、`--json` 快照测试、`--version` 门槛值；Python 假 AgentEar | C1–C4、C8、C9 | ~650 行 |
| **A3-3** 反向命令 | `POST /api/v1/os/{name}/commands/{command}`、§6.1 REST 映射全表、在途上限 | C5 | ~250 行 |
| **A3-4** 桌面端 | 主进程 WS 订阅、`agentearSequencer` + fixtures（钉 sha）、「语音」面板（实时、仅内存、无回复文本）、speak/stop 按钮 | C7；C10 在此片后由 jason 真机跑 | ~400 行 |

A3-2 超过 300 行门槛，按层拆为 **A3-2a「存储 + 注册表 + REST + CLI」**（C1、C8 的隐私放宽部分）与 **A3-2b「监听 + 握手提交 + 生命周期 + 关机」**（C2–C4、C8 其余、C9），仍在 4 片计划内（总计 5 个 PR）。开工前先更新 PLAN §三 与 `tasks.md` 台账。每个 PR 各评审一轮（有 High 才复审）。

## 11. 已拍板（jason，2026-09-27）

| # | 问题 | 决定 | 落点 |
|---|---|---|---|
| Q1 | P2 桌面端要不要显示**模型回复文本** | **a）P2 不显示**，只显示 transcript + tier；回复展示与留档一起放 P3，不为展示先改 `agentear.event/1` | §7.3 |
| Q2 | 配对 UX：谁调 `agent24 os attach add` | **b）AgentEar 设置里一键配对**：代跑本机 `agent24` CLI，token 存储；失败时退回终端手动；放宽隐私不能静默（M4）。**更新（v0.4.0）**：token 存储位置从 macOS Keychain 改为 `~/.agentear/agent24/token`（0600），需要 AgentEar ≥ v0.25.2，推荐 v0.26.1——升级后不再弹钥匙串授权提示；同 UID 威胁模型下（模块与内核同 UID、无沙箱，见 §0）文件权限 0600 与 Keychain 提供等价的机密性 | §3.5、§3.6、§5.6 |
| Q3 | 现役代存在时来了第二条合法连接 | **a）先到者保留，后到者 `busy`** | §4.3、§5.4 |
| Q4 | 是否让 Agent24 持久化 transcript | **a）不持久化**。转写记录的归属方是 AgentEar（它默认已保存）；Agent24 只在桌面面板实时显示，不写数据库、不写日志 | §7.3、§8、C7、C10 |

## 12. 后续（不在本文设计）

- P3：`proposal` → 宿主确认 UI（含 `confirm_reply` 事件消费、同源校验）→ gate 可执行集合（**排除 `action.type = builtin`**，§7.1）→ 回执留档；回复文本展示（Q1）。
- P4：流式 `_a24/model/stream`（取消、背压、计量）。
- 多轮：授予 `memory`（`_a24/memory/private/*`），或宿主会话回调。
- Rust 模块侧 `connect_attached` + SDK 包装（出现 Rust 附着模块时）。
- 附着模块的入站 HTTP（若将来有模块需要被外壳直接调 REST）。
- 桌面端的隐私放宽确认对话框（P2 只走终端 `--allow-remote`）。

## 附录 A：scratch 签名检查

`scratchpad/a3-sig/`：`Cargo.toml` 以 path 依赖本分支 `rust/crates/agent24-os-proto` 与 `agent24-domain`；`src/lib.rs` 含本文全部 Rust 签名。A3 **新增到既有 crate** 的类型以镜像形式检查（设计 PR 不改真实 crate），它们依赖的一律是真实类型：

- §5.1：`enum Upstream { Process(PathBuf), Attached, Placeholder }`；`Generation::attached() -> Arc<Generation>`；`upstream(&self) -> Option<&Path>`（签名不变）。
- §4.3：`AttachedExpectation { manifest_digest: String, token_sha256: [u8; 32], token_id: String, kernel_versions: VersionRange, offer: Offer }`；`AttachedAccepted { module: String, token_id: String, accepted: Accepted }`；`accept_attached(frame: &[u8], lookup: &dyn Fn(&str) -> Option<AttachedExpectation>) -> Result<AttachedAccepted, HandshakeError>`；`HandshakeError` 新变体 `Busy`/`Forbidden`/`AttachedDigestMismatch`（镜像枚举）。
- §5.2：`AttachSlot::{install(&mut self) -> Result<(u64, Arc<Generation>), SlotBusy>, revoke(&mut self) -> bool, release(&mut self, &Arc<Generation>)}`；`AttachRegistry::{commit(&self, &AttachedAccepted) -> Result<(u64, Arc<Generation>, Methods), HandshakeError>, revoke_all(&self)}`。
- §6：`KernelCallFailed::{NotSent, ConnectionLost, Timeout, Rpc(RpcError), Malformed(String)}`；`KernelCalls::call(..) -> Result<Map<String, Value>, KernelCallFailed>`、`stray_responses() -> u64`；`serve_attached`；`COMMAND_METHOD = "_a24/command/invoke"`、`COMMAND_TIMEOUT = 5 s`、`MAX_KERNEL_CALLS_IN_FLIGHT = 8`；`stop` 取真实 `Generation::revoked()`。
- §3.6：`AttachAddResponse { name, manifest_digest, token, socket_path, token_id }`（`Serialize`，REST 响应体与 CLI `--json` 输出共用）。

`cargo check`（stable，edition 2024）通过，0 error、0 warning。
