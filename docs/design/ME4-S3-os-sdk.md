# ME4-S3 —— `agent24-os-sdk` 设计（ME4-5.1.1：从两个调用方提取）

> **状态：草案 v0（2026-09-26），未送评审，未冻结。** 按 PLAN-ME4 §一 第 2 条，本文要先经对抗评审（Codex；额度耗尽期间为全新上下文 Opus 子代理）到 APPROVE 才冻结，冻结前不写 SDK 代码。
> 文件名按 PLAN-ME4 §三 ME4-5.1.1 的原文取 `ME4-S3-os-sdk.md`（S3 = PLAN §二 的「S3 `agent24-os-sdk`」；`S5` 是 T14 wire 文档那一节，不用这个编号，免得撞名）。
>
> **盘点基线**：
> - Agent24 `origin/main` @ `309d525`（本 worktree `docs/me4-5.1.1-sdk-design`）。
> - Sin90 `origin/main` @ `135ddb7`（`T5.5.1 M5 real-mount acceptance`，即 ME4-M4b 门的最后一个 task）。下文 `Sin90 path:line` 一律指这个提交。
> - Cos72：`MushroomDAO/Cos72`（本机 `~/Dev/mycelium/Cos72`）目前只有 LICENSE/README/CLAUDE.md，**没有代码**；Cos72 的需求只能从 PLAN §二 S4、ME4-5.3.x、`docs/agent/roadmap.md` M4/M5、`docs/decision.md` ADR-004/029 推。Agent24 的旧分支 `feat/me4-cos72-skeleton` 是进程内骨架（PLAN-OOP §73：「3f 之后重做成进程外样例；现在不合」），对进程外 SDK 没有参考价值，不采用。
>
> 文中每一段 Rust 签名都来自 scratch crate `scratchpad/sdk-sketch/`（proto 桩 + SDK 草图 + example + 3 个测试），已在 `1.98.0` 上 `cargo check / clippy -D warnings / test / fmt --check` 全过，命令与输出见附录 A。正文里的片段是**节选**（函数体以 `{ … }` 省略），完整可编译文本在 scratch 里，附录 B 列出其中最承重的两份原文。

## 版本改动记录

| 版本 | 日期 | 改动 |
|---|---|---|
| v0 | 2026-09-26 | 初稿：盘点 + 边界 + API 草图（已编译）+ 迁移 + 判据 + 切法 + 开放问题 |

---

## 0. 这份文档解决什么、不解决什么

**解决**：
1. 在 Sin90 手写 adapter（`src/adapter_agent24/`，9593 行，含测试）里逐项区分「任何进程外模块都要写的」和「Sin90 特有的」，并推演 Cos72 需要哪几项（§1）。
2. 定 SDK 边界：crate 名与依赖、transport/握手放哪、五种类型化客户端、fired 注册点、错误映射、超时/重试归属、版本与兼容、非 Rust 模块如何对齐 wire（§2）。
3. 公共 API 草图，已编译（§3），以及 proto 为此要补的公开 API（§4）。
4. Sin90 迁到 SDK 的步骤和「零行为变化」的验收办法（§5，ME4-5.2.1 / Sin90 TS.1.1）。
5. 判据与 PR 切法（§6、§7，对应 ME4-5.1.2a/b/c）。
6. 需要用户拍板的开放问题（§8）。

**不解决**：
- 内核回调面本身（调度、推理、记忆、审批的内核实现已由 ME4-S1/S2 与 ME-3e 冻结；SDK 按 PLAN「不带来任何新能力」）。
- T14 wire 文档正文（ME4-5.4.1，规范 S5）；本文只规定 SDK 与 wire 文档的对齐方式（§2.9）。
- Cos72 的业务设计（ME4-5.3.1 起在 Cos72 仓库做）；本文只把 Cos72 对 SDK 的需求推到够用，并把一处与内核闭集冲突的计划措辞交给用户（§8 Q3）。

---

## 1. 盘点：Sin90 adapter 逐项归类，外加 Cos72 的需求推演

### 1.1 归类规则

- **进 SDK**：两个调用方都要，或 Cos72 明确要（用户规则）。Sin90 已有实现、且实现里不含 Sin90 领域类型的，按原语义搬。
- **进 proto**：凡是碰 socket 字节、帧、握手线格式、fd 的（PLAN S3-1：「SDK 不做任何 socket 字节 I/O 与帧解析……若 proto 缺少 SDK 需要的公开 API，在 proto 里补」）。
- **留在 Sin90**：带 Sin90 领域语义、Sin90 自己的存储、或 Sin90 自己的分层约定的。

「Cos72 要不要」一列的依据：PLAN S4（「每个动作发事件；任务完成摘要写 `_a24/memory/private/remember`；发积分经内核审批」）、ME4-5.3.4（「与 Sin90 同时挂载时互相读不到对方的记忆与 schedules」）、D6（「用到事件 + 记忆 + 审批」）。

### 1.2 盘点表

| # | 项 | Sin90 位置 | 归属 | Cos72 要不要 | 说明 |
|---|---|---|---|---|---|
| 1 | 四个 spawn 环境变量读取 | `mod.rs:144-165`（`SpawnEnv::from_env`） | **proto** | 要 | 常量 proto 早有：`launch.rs:30-37`（`ENV_LISTEN_FD` 等）。Sin90 手抄了字符串 |
| 2 | manifest 摘要 `sha256:<hex>` | `mod.rs:169-173` | **proto** | 要 | 握手字段，和内核侧同一算法，放一处 |
| 3 | `initialize` 请求/响应、协议范围、握手后缓冲区不许有残字节 | `mod.rs:66-74`（范围 1..=1000）、`mod.rs:326-402` | **proto** | 要 | proto 已有 `InitializeParams`/`InitializeResult`/`Offer`（`initialize.rs:59-160`），但只有**内核侧**的 `accept`，没有模块侧的发起方。Sin90 手拼 `json!` 且自己 `serde_json::from_slice` 解析响应（`mod.rs:365-386`）——PLAN-OOP T13 判据原话「SDK 里出现 `serde_json::from_slice` 直接解析协议帧 = 缝切错了」 |
| 4 | `INITIALIZE_CAPABILITIES` 手写常量 + 与 manifest 对齐的测试 | `mod.rs:84`、`mod.rs:640-663` | **SDK（改为从 manifest 派生）** | 要 | 手抄一份再用测试钉住，是「两份真相」。SDK 从 manifest 文本里读 `name`/`route_namespace`/`kernel_capabilities`，这个常量与测试整个消失（§2.3） |
| 5 | 帧读写（1 MiB 上限、`\n` 分隔、EOF 半帧 = Closed） | `frame.rs:34-97` | **proto（已有，删 Sin90 这份）** | 要 | proto 有 `frame::MAX_FRAME_BYTES`（`frame.rs:84`）与 `rpc::read_frame_async`（`rpc.rs:886`）。Sin90 这份是重复实现 |
| 6 | 多路复用 transport：单写任务、读任务按 id 分发、64 在途上限、drop 即发 `$/cancelRequest`、写超时 10s、响应兜底 35s、连接死 → `FatalHook` 恰好一次、NotSent/ConnectionLost 二分 | `transport.rs:63-631`（非测试约 325 行）；测试 `transport.rs:686-1368` 共 16 条 | **proto** | 要 | 这是 PLAN S3-2 点名的「T3.2.0 多路复用 transport」。它**持有 socket**，按 S3-1 结构判据只能进 proto。常量与 proto 同值：64 = `rpc::MAX_IN_FLIGHT_PER_CONNECTION`（`rpc.rs:73`），35s > `rpc::CALL_TIMEOUT` 30s（`rpc.rs:82`），10s = `rpc::WRITE_TIMEOUT`（`rpc.rs:93`），`$/cancelRequest` = `rpc::CANCEL_METHOD`（`rpc.rs:101`）。Sin90 注释自己承认「只是数值巧合，没有强制对称」（`transport.rs:26-29`）——搬进 proto 后改成直接引用 proto 常量 |
| 7 | `RpcErrorInfo`（code / data.kind / message / raw） | `transport.rs:119-153` | **proto** | 要 | 解析 JSON-RPC error 对象属于协议层 |
| 8 | `KernelClients`：持连接 + 握手时固定的 `Offer`，`provides(prefix)` | `mod.rs:210-322` | **SDK（`Module`）** | 要 | 无重连、`Offer` 一代不变，这两条语义原样保留 |
| 9 | 闭集错误 `ClientError` + `is_permanent`/`is_retryable` + 两路映射（传输错误、内核 `kind`） | `clients/error.rs:73-397` | **SDK** | 要 | 唯一的 Sin90 耦合：`Unavailable.cause` 用的是 `crate::ai::UnavailableCause`（`error.rs:217-229`、`378-397`）。SDK 自带同名四值闭集，Sin90 在自己的 `ModelFailure` 映射里转换 |
| 10 | 「omit, don't null」可选字段约定 | `clients/mod.rs:74-78` | **SDK** | 要 | 内核 params 全是 `deny_unknown_fields` + `#[serde(default)]`；发 `null` 与缺省语义不同的地方（如 scheduler `enabled`）会出错 |
| 11 | `EventSink`：同步 `emit`、256 容量队列、4 个固定 worker、32 子配额、5s 等位、满即丢并按 2 的幂打日志 | `mod.rs:86-122`、`mod.rs:425-503` | **SDK** | 要（每个动作发事件） | 这套数字是两轮评审（N-M1/N-M2/M5）磨出来的，任何模块自己再写一遍大概率回到「每次 emit spawn 一个任务」的旧缺陷。SDK 同时给「可等待的 `emit`」与「即发即弃的 sink」 |
| 12 | `SchedulerClient`（upsert/delete/list + 类型） | `clients/scheduler.rs:35-243` | **SDK** | 见 §8 Q4 | Cos72 的 S4 闭环不用调度；但 ME4-5.3.4 要验证「互相读不到对方的 schedules」，Cos72 若不申请 `scheduler` 就只能从内核 REST 侧验。第二个真实调用方是 Agent24 黑盒 Python 模块（`me4_scheduler_blackbox.rs:75-240`）与 T14 Node 模块 |
| 13 | `MemoryClient`（remember/recall/recent） | `clients/memory.rs:30-149` | **SDK** | 要（摘要进记忆） | |
| 14 | `remember` 不幂等 → 先 `recall` 翻页找 dedup 标记再写 | `reconciler.rs:255-386`（`RECALL_PRECHECK_MAX_PAGES = 10`） | **待定（§8 Q5）** | 要（摘要写入同样会遇到「超时后重试写出两条」） | 算法依赖内核 `recall` 的实际语义（子串匹配、每次最多扫 2000 行、`cursor` 表示「窗口扫完了」而非「还有匹配」，`reconciler.rs:100-120`），很容易写错；但它是在绕内核缺一个幂等键 |
| 15 | `ApprovalClient`（gate/advise/status，`ApprovalToken` 脱敏） | `clients/approval.rs:38-209` | **SDK** | 要 | Sin90 **没有业务调用方**，只有 test-hooks 路由（`kernel_roundtrip.rs:227-254`）。Cos72 是它第一个真实用户。注意 gate 的动作闭集今天只有 `schedule_callback`（`agent24-protocol/src/types.rs:421-431, 575-590`），见 §8 Q3 |
| 16 | 从被代理请求头取 `X-A24-Request-Id` / `X-A24-Approval-Token` | `kernel_roundtrip.rs:227-254`；Python 黑盒 `me4_scheduler_blackbox.rs:201-221` | **SDK（`RequestContext` 提取器）** | 要（审批必须带这两样） | 头名 proto 已有：`proxy.rs:87`、`proxy.rs:96` |
| 17 | `ModelClient` 的 wire 部分（`_a24/model/complete` 参数/结果、125s 响应期限） | `clients/model.rs:44-156` | **SDK（wire 形状）** | 不要 | 见 §8 Q4。Sin90 自己的 `Role` 没有 `Assistant`、`usage` 用 `u32`，内核是 `System/User/Assistant` 与 `u64`（`agentd model_callback.rs:91-96, 189-193`）。SDK 按内核 wire 定义 |
| 18 | `ModelPort`/`ModelCaller` 适配、`ClientError → ModelFailure` 映射、`SemaphoredModelCaller` 每模块并发 2 | `clients/model.rs:159-215`、`mod.rs:567-579` | **留 Sin90** | — | Sin90 AI 引擎梯的端口，领域语义 |
| 19 | `wire_kernel_clients`：没给能力时降级为 `NullEventSink`、能力前缀表 | `mod.rs:175-204`、`mod.rs:544-581` | **留 Sin90**（降级策略）；「按 `Offer` 返回 `Option`」的机制进 SDK | 各自写 | 各模块的「缺能力怎么降级」不同 |
| 20 | `listener_from_fd`（`unsafe { from_raw_fd }`） | `mod.rs:600-604` | **proto**（需要一个 `unsafe` 决定，§8 Q2） | 要 | 工作区 `unsafe_code = "forbid"`（`rust/Cargo.toml:33`），proto 继承 |
| 21 | fired 保留路由：`X-A24-Fire-Id` 必填、body `deny_unknown_fields`、定宽 ISO-8601 校验、任何结果都回 2xx | `http/mod.rs:190`、`http/mod.rs:320-338`、`http/mod.rs:1347-1359`、`http/mod.rs:1376-1478` | 解析/校验进 **SDK**（`FiredDelivery` 提取器 + `with_fired` 注册点）；「按 `fire_id` 去重写库、镜像事件」**留 Sin90** | 仅当 Cos72 申请 scheduler | Sin90 的分层（`lib.rs:3`：`core ← store ← http ← adapter_agent24`，`http` 不许依赖 Agent24 类型）决定了 Sin90 在 5.2.1 **不**改用 SDK 提取器（§5.2）。内核侧 `FiredBody` 是 `agentd scheduler_deliver.rs:178` 的私有借用类型，SDK 与内核共用一个需要把它挪进 proto |
| 22 | 主流程：读 env → 握手 → 建客户端 → 开 store → nest 到 `/api/v1/sin90` → `axum::serve` | `main.rs:118-262` | 骨架进 **SDK**（`Module::builder(..).connect()` / `serve(router)`）；中间 Sin90 的 store、actor keys、reconciler、test-hooks 合并留 Sin90 | 要 | `route_namespace` 从 manifest 读（Sin90 手写了 `"/api/v1/sin90"`，`main.rs:259`） |
| 23 | 连接死 → `exit(70)` | `main.rs:138-145` | **SDK 默认值**，可覆盖 | 要 | §2.6 |
| 24 | outbox 泵、全量对账、退避 1s×2 上限 5min、other-bucket 20 次耗尽、按错误分类决定「继续 / 停本批 / 整个泵停」 | `reconciler.rs:136-1180` | 存储与泵**留 Sin90**；「错误 → 该怎么办」的纯分类进 **SDK**（`ClientError::retry_class`） | 要分类（摘要进记忆走 outbox） | Sin90 的分类表在 `reconciler.rs:465-546`（文档）与 `692-728`（代码）。SDK 只给分类，不给泵；Sin90 在 5.2.1 **保留自己的 match**（零行为变化，§5.2） |
| 25 | Actor keys（人 / 自动化两把钥匙） | `http/actor.rs` | **留 Sin90** | 各自 | 领域的「AI 不直接写」门 |
| 26 | test-hooks 调试路由、假内核测试夹具 | `kernel_roundtrip.rs`、`reconciler_debug.rs`、`clients/test_support.rs` | 调试路由**留 Sin90**；假内核夹具进 **proto**（`test-util` feature） | 要夹具 | SDK 自己的测试也不能开 socket（clippy 判据对 `--all-targets` 生效），只能用 proto 给的内存假内核 |

**Cos72 需要的集合**（推演结论）：1–11、13、15、16、20、22、23、24（分类部分），外加 14（形式待定）。调度（12、21）取决于 §8 Q4 / ME4-5.3.4 的验法；推理（17）不要。

### 1.3 这份盘点揭出的三件计划里没写到的事

1. **proto 今天没有任何模块侧 API。** PLAN S3-1 写「proto 提供两个入口 `Client::connect_from_env()` 和 `listener_from_env()`」——这两个入口**不存在**（`rg connect_from_env|listener_from_env rust/` 为空）。proto 22,713 行全是内核侧：`accept`（握手判定）、`rpc::serve`（服务模块的调用）、launch/supervise/proxy。所以 5.1.2a 的大头是「把 Sin90 的 transport 搬进 proto 当模块侧客户端」，不是写 SDK。
2. **`listener_from_env` 需要 `unsafe`，而工作区禁了。** 把一个 fd 号变成 socket 只有 `FromRawFd::from_raw_fd`，是 `unsafe`；`rust/Cargo.toml:33` 是 `unsafe_code = "forbid"`（forbid 不能被 `#[allow]` 局部放开）。ME3-SUP 的先例是「用小 crate 代替自己写 unsafe」（`agent24-os-proto/Cargo.toml` 注释里的 command-fds / close_fds 决定 D3）。这要用户拍板（§8 Q2）。
3. **PLAN S4 写「发积分经内核审批（`_a24/approval/gate`）」，但 gate 今天只接受 `schedule_callback` 一个动作**，其余一律 `forbidden`（`types.rs:421-431`）。按现状 Cos72 只能用 `advise` + 轮询 `status`（§8 Q3）。这影响 SDK 要不要多一个「审批裁决回调」注册点。

---

## 2. SDK 边界

### 2.1 crate 名、位置、依赖

- 名字 `agent24-os-sdk`，位置 `rust/crates/agent24-os-sdk`（PLAN S3-1）。工作区成员，继承 `[lints] workspace = true`（因此 `unsafe_code = "forbid"` 自动生效）。
- **普通依赖里 `agent24-*` 只有 `agent24-os-proto` 一个**（判据 J2）。其余：`axum 0.8`（`default-features = false, features = ["json","tokio","http1"]`，与 proto 同主版本）、`serde`、`serde_json`、`thiserror`、`tokio`（`sync`,`time`,`rt`,`macros`）、`tracing`。
- SDK 需要 manifest 的三个字段。proto 本来就依赖 `agent24-domain`（`agent24-os-proto/Cargo.toml:10`），所以由 proto 新增一个 `manifest` 模块做薄转出（§4.4），SDK 不直接依赖 `agent24-domain`。
- **依赖重量的代价**：proto 今天连带 `command-fds`、`close_fds = "=0.3.2"`（精确钉版）、`rustix`、`hyper`/`hyper-util`、`agent24-domain`（→ `serde_yaml`、`agent24-protocol` → `schemars`）。Sin90/Cos72 以 git 依赖引入 SDK 时这些都要编译。要不要给 proto 拆 `kernel`/`module` feature 交用户定（§8 Q8）。

### 2.2 结构性判据：SDK 从不持有 socket（PLAN S3-1，已在 scratch 验证）

`rust/crates/agent24-os-sdk/clippy.toml`：

```toml
disallowed-types = [
  { path = "tokio::net::UnixStream", reason = "S3-1: the SDK never owns a socket; use agent24_os_proto::module" },
  { path = "tokio::net::UnixListener", reason = "S3-1: the SDK never owns a socket; use agent24_os_proto::module" },
  { path = "std::os::unix::net::UnixStream", reason = "S3-1" },
  { path = "std::os::unix::net::UnixListener", reason = "S3-1" },
]
disallowed-methods = [
  { path = "std::os::fd::FromRawFd::from_raw_fd", reason = "S3-1: fd adoption lives in agent24-os-proto" },
  { path = "serde_json::from_slice", reason = "PLAN T13: parsing protocol frames in the SDK means the seam is cut wrong" },
]
```

scratch 里实测三件事（附录 A）：
- `Module::serve` 里 `let listener = listener_from_env(&self.env)?; axum::serve(listener, app)` —— 类型靠推断，源码不写 `UnixListener`，**clippy 不报**（PLAN 的假设成立）。
- 正对照 `--features positive-control` 放进一个 `UnixStream::connect` 与一个 `serde_json::from_slice` → **clippy 报 2 条 disallowed 错误，退出 101**。
- `from_raw_fd` 这条在 SDK 里本来就因 `forbid(unsafe_code)` 编译不过；为确认 clippy 路径写对了，在一个不带 forbid 的独立小 crate 里用同一份 `clippy.toml` 验证 → 报 `use of a disallowed method std::os::fd::FromRawFd::from_raw_fd`。

`serde_json::from_slice` 是本文比 PLAN 多加的一条：PLAN-OOP T13 的原判据就是它。SDK 需要解析的只有 fired 的 HTTP body，改用 `axum::Json` 提取器即可（§3.6），不需要 `from_slice`。

### 2.3 transport + 握手：全在 proto，SDK 只调

proto 新增 `agent24_os_proto::module`（§4），承担 §1.2 的第 1、2、3、5、6、7、20 项。SDK 的 `ModuleBuilder::connect()` 只做三件事：
1. `ModuleEnv::from_env()`；
2. 从 manifest 文本读出 `name` / `route_namespace` / `kernel_capabilities`，组 `Hello`（**不再手抄能力名**，§1.2 第 4 项）；
3. `Connection::connect_from_env(&env, &hello, on_fatal)`。

「无重连、`Offer` 一代不变」（Sin90 `mod.rs:22-30`、`transport.rs:11-22`；内核侧 D1 一代只接受一条回调连接）原样成为 SDK 的公开契约，写在 `Module` 的文档里。

### 2.4 五种类型化客户端（逐一确认）

PLAN S3-2 与 ME4-5.1.2b 原文列的五种是 **Events / Memory / Approval / Scheduler / Model**。逐一对到内核方法与 Sin90 现有实现：

| 客户端 | `Offer` 前缀 | 方法（内核定义位置） | Sin90 现有 | 第二个调用方 |
|---|---|---|---|---|
| `EventsClient` | `_a24/events/` | `emit{kind, payload, request_id?}`（`agentd events_emit.rs:251-256`） | `KernelEventSink`（`mod.rs:425-503`），不发 `request_id` | Cos72（S4）、Python 黑盒 |
| `MemoryClient` | `_a24/memory/private/` | `remember/recall/recent`（`memory_callback.rs:36-67`） | `clients/memory.rs` | Cos72（S4）、Python 黑盒 |
| `ApprovalClient` | `_a24/approval/` | `gate/advise/status`（`approval_callback.rs:34-64`） | `clients/approval.rs`（无业务调用方） | Cos72（S4） |
| `SchedulerClient` | `_a24/scheduler/` | `upsert/delete/list`（`scheduler_callback.rs:75-143`） | `clients/scheduler.rs` | Python 黑盒、T14 Node；Cos72 视 §8 Q4 |
| `ModelClient` | `_a24/model/` | `complete`（`model_callback.rs:68-116, 181-193`） | `clients/model.rs:44-156` | 暂无（§8 Q4） |

共同规则（从 Sin90 原样继承）：
- 构造函数 `new(&Arc<Connection>) -> Option<Self>`：`Offer` 不覆盖本前缀就返回 `None`，**没有「照样调、调了必败」的路径**（Sin90 `clients/mod.rs:5-11`，architecture.md 不可破边界 #7）。
- 可选字段缺省即不发（「omit, don't null」）。
- 响应类型**不加** `deny_unknown_fields`（内核加字段不能打坏没重编的模块）；请求形状必须与内核 `deny_unknown_fields` 的 params 逐字段一致，由 agentd 侧的 wire 对等测试钉住（判据 J7）。
- `request_id` 参数类型是 `Option<&RequestId>`，`RequestId` **只能**从被代理请求的头里提取（没有公开构造函数）——模块不可能「编」一个 id 去绑别人的请求生命周期。

### 2.5 fired 注册点

- 常量 `FIRED_PATH = "/_a24/scheduler/fired"`（相对模块自己的 `route_namespace`；内核 POST 的完整路径是 `/api/v1/<ns>/_a24/scheduler/fired`，ME4-S1 §5.3）。
- 提取器 `FiredDelivery { fire_id, schedule_key, body: FiredBody, ctx: RequestContext }`：`X-A24-Fire-Id` 缺失、body 有未知字段、`scheduled_for`/`fired_at` 不是定宽 ISO-8601 → `FiredRejection`（默认渲染为 400）。非 2xx 会被内核算作一次失败尝试并重投（ME4-S1 §5.3），这正是想要的。
- 注册点 `with_fired(router, handler)`：把 handler 挂到 `FIRED_PATH`。
- 模块想用自己的错误体形状：用 `Result<FiredDelivery, FiredRejection>` 作提取器自己渲染（scratch 测试 `caller_can_render_rejection_in_its_own_shape` 已验）。
- 语义提醒写进文档注释（不是 SDK 能强制的）：**先按 `fire_id` 去重，再做副作用或提交审批**；`ctx` 里的 request id / 审批 token 只在本次投递内有效（ME4-S1 v2 M10）。
- `FiredBody` 从 agentd 的私有借用类型（`scheduler_deliver.rs:178`）挪到 `agent24_os_proto::kernel_call::FiredBody`（拥有型，`Serialize + Deserialize + deny_unknown_fields`），内核序列化、SDK 反序列化用**同一个类型**（判据 J10b）。

### 2.6 错误类型映射

- `ClientError` 闭集按 Sin90 `clients/error.rs` 原样搬：18 个内核 `kind` 中除握手专用的 `auth_failed`/`manifest_mismatch` 和本来就落 `Other` 的 `invalid_lease`/`unknown_capability`/`version_mismatch` 外各有专属变体；`-32602` → `InvalidParams`；`timeout` + `data.retryable == false` → `RequestNotInFlight`；`unavailable` 必须同时有合法 `cause` 与布尔 `retryable`，否则落 `Other`（Sin90 L4 的「不猜」原则）。
- 两处合并保留（Sin90 L-6）：本端 64 在途满 与 内核 `busy` → `Busy`；本端帧超限 与 内核 `payload_too_large` → `PayloadTooLarge`。
- `UnavailableCause` 改为 SDK 自有的四值闭集（去掉对 Sin90 `crate::ai` 的耦合）。
- **新增** `ClientError::retry_class() -> RetryClass`（纯分类，SDK 自己从不重试）：`Revoked → GenerationOver`；`ConnectionLost | Cancelled → OutcomeUnknown`；`RateLimited | Busy | NotReady | Draining → BackoffPauseBatch`；其余按 `is_permanent`/`is_retryable` → `Permanent`/`Backoff`，剩下 `Unclassified`。这是把 Sin90 `reconciler.rs:692-728` 的表抽成与存储无关的形式，给 Cos72 的 outbox 用。**注意**：Sin90 今天把 `Cancelled` 放进「其它 → 退避 + other-bucket 计数」（`reconciler.rs:745-752` 的 `Err(e)` 兜底分支；它的注释列举 `timeout`/`not_sent`/`request_not_in_flight`/`other`，没有 `cancelled`，但 `Cancelled` 实际落在这里），与 `retry_class` 的 `OutcomeUnknown` 不同；所以 5.2.1 里 Sin90 **不改用** `retry_class`（零行为变化），是否对齐留作 Sin90 后续 task。
- 要不要给 `ClientError` 加 `#[non_exhaustive]` 交用户定（§8 Q6）；草图按推荐**不加**。

### 2.7 超时、重试、并发：谁负责

| 机制 | 归属 | 数值 / 规则 | 依据 |
|---|---|---|---|
| 写超时（含 flush） | proto `Connection` | 10s = `rpc::WRITE_TIMEOUT` | Sin90 `transport.rs:87` |
| 单次调用响应兜底 | proto `Connection`，按调用可覆盖（`CallOptions.response_timeout`） | 默认 35s（> `rpc::CALL_TIMEOUT` 30s，让内核的具体错误先到）；编译期断言 `35 > 30` | Sin90 `transport.rs:89-94` |
| 推理调用响应期限 | SDK `ModelClient` | 125s（> 内核 `MODEL_CALL_TIMEOUT` 120s，`model_callback.rs:35`） | ME4-S2 J10a；Sin90 `model.rs:55` |
| 在途上限 | proto `Connection` | 64，第 65 个立即 `Busy`（默认不排队）；`CallOptions.slot_wait` 可选有界等待 | Sin90 `transport.rs:24-34` |
| 取消 | proto `Connection` | 丢弃调用 future 即 best-effort 发 `$/cancelRequest`（`params: {id}`，无顶层 id）；迟到响应静默丢弃 | Sin90 `transport.rs:50-61`。PLAN 说的「call/notify/cancel」里的 cancel 就是这个 drop 语义，不另开显式 `cancel` 方法 |
| 事件队列（容量 / worker / 子配额 / 等位） | SDK `EventSink`，`EventSinkConfig` 可调 | 默认 256 / 4 / 32 / 5s（与 Sin90 相同） | Sin90 `mod.rs:86-122` |
| **重试** | **调用方** | SDK 与 proto 永不自动重试。`NotSent` 表示没上线；`ConnectionLost`/`Timeout`/`Cancelled` 表示结果未知 | Sin90 `transport.rs:36-48`、`error.rs:262-279` |
| 退避曲线、outbox、耗尽阈值、幂等 | **调用方** | SDK 只给 `retry_class()` | §2.6 |
| 推理的每模块并发上限 | **调用方**（内核另有自己的 2 并发） | Sin90 `SemaphoredModelCaller` | ME4-S2 §5 |
| 连接死后怎么办 | SDK 提供 `FatalHook` 注入；**默认** `warn!` + `std::process::exit(70)` | 与 Sin90 `main.rs:138-145` 相同；supervisor 起新一代 | 库里默认退出进程是否可接受见 §8 Q10 |

### 2.8 版本与兼容

- **SDK 自身**：SemVer 从 `0.1.0` 起（PLAN S3-3）。发布方式：Agent24 仓库打 tag `agent24-os-sdk-v0.1.0`，外部仓库 `agent24-os-sdk = { git = "https://github.com/iDoris-ai/Agent24", tag = "agent24-os-sdk-v0.1.0" }`。0.x 期间 minor 升级即可破坏兼容；新增错误 `kind` 属于破坏性变化（§8 Q6）。
- **工具链**：Agent24 是 edition 2024、CI 钉 `1.98.0`，agentd 已用 let-chains（`model_callback.rs:126-127`、`133-134`），需要 Rust ≥ 1.88。Sin90 CI 用 `dtolnay/rust-toolchain@stable`（`.github/workflows/ci.yml:24-25`），当前 stable 满足。SDK `Cargo.toml` 写 `rust-version = "1.88"` 并在 SDK 的 CI 里用 1.88 跑一次 `cargo check -p agent24-os-sdk`（防止无意中抬高 MSRV 打坏下游），是否值得多这一条 CI 作业由评审定。
- **wire 协议版本**：SDK 在 `initialize` 里声明的范围。Sin90 今天是 `1..=1000`（`mod.rs:73-74`），内核 `negotiate` 取 `min(module.max, kernel.max)`（`version.rs:175-189`），内核是 v1（`agent24-domain lib.rs:397`）→ 今天两种写法结果都是 1。但 `1..=1000` 意味着将来 v2 内核会和只懂 v1 的 SDK「协商成功」地讲 v2。推荐 SDK 只声明它实际实现的范围 `1..=1`（§8 Q7）；草图里常量暂按 Sin90 现状写 `1000`，冻结时按裁决改。
- **内核加字段**：响应类型宽松解析，不破坏旧模块。**内核加必填参数**：属于 wire 破坏，SPEC 与 SDK 同 PR 改，SDK minor 升。**内核加错误 kind**：落 `Other` 直到 SDK 升级（deny by default），不 panic。

### 2.9 非 Rust 模块（T14 Node.js）如何对齐 wire

- **唯一真相是 wire，不是 SDK。** T14 的判据是「只拿 `docs/specs/WIRE-OOP-MODULE.md` 的子代理写得出 Node 模块」（PLAN S5）。所以 SDK 的每一条行为约定都必须能在 wire 文档里找到出处，反过来 SDK 不许有 wire 文档之外的「隐藏协议」。
- 为此本设计要求：SDK 源码里所有协议常量（方法名、头名、路径、超时关系）要么引用 proto 常量，要么在 wire 文档里有同名条目。ME4-5.4.1 的 PR 附一张「SDK 常量 → wire 文档章节」对照表（判据 J15），子代理卡住的点补文档不补代码。
- Rust 与 Node 在三处容易跑偏，wire 文档必须写明（本文先列出，供 5.4.1 用）：① 握手后缓冲区不许有残字节（Sin90 `mod.rs:388-400`）；② 调用 id 是字符串、响应不保证顺序、按 id 匹配；③ 可选字段缺省不发、不要发 `null`。
- Python 黑盒模块（`me4_scheduler_blackbox.rs:75-240`）是 PLAN 点名的第二个提取来源。它证明了「不借 SDK、只按 wire 也能做出 `initialize` → upsert → fired → 绑定 request_id 的 remember」，同时暴露了一个 SDK 必须替开发者挡住的坑：它的 `rpc()` 是串行的一问一答，读到的下一行默认就是自己的响应——一旦并发就错。这是 transport 必须按 id 分发的实证。

---

## 3. 公共 API 草图（scratch 已编译，节选）

> 全部来自 `scratchpad/sdk-sketch/sdk/`。proto 部分是桩（只有签名有效），见 §4 与附录 B.1。

### 3.1 crate 根导出

```rust
pub use clients::{
    ApprovalClient, EventSink, EventSinkConfig, EventsClient, MemoryClient, ModelClient,
    SchedulerClient,
};
pub use context::{ApprovalToken, RequestContext, RequestId};
pub use error::{ClientError, RetryClass, UnavailableCause};
pub use fired::{FIRED_PATH, FiredDelivery, FiredRejection, with_fired};
pub use module::{Module, ModuleBuilder, SdkError};
pub use agent24_os_proto::kernel_call::{FireTrigger, FiredBody};
pub use agent24_os_proto::module::FatalHook;
```

### 3.2 `Module`（全文见附录 B.2）

```rust
pub struct ModuleBuilder { /* manifest_yaml: &'static str, on_fatal: Option<FatalHook> */ }

impl ModuleBuilder {
    /// Default: log + `std::process::exit(70)` (Sin90's behaviour).
    pub fn on_connection_lost(mut self, hook: FatalHook) -> Self { … }
    /// Read env, parse manifest (name / namespace / kernel_capabilities come
    /// from it — never hand-copied), dial + `initialize`.
    pub async fn connect(self) -> Result<Module, SdkError> { … }
}

impl Module {
    /// `manifest_yaml` must be the exact bytes the kernel digests
    /// (typically `include_str!("../domain-os.yml")`).
    pub fn builder(manifest_yaml: &'static str) -> ModuleBuilder { … }
    pub fn data_dir(&self) -> &Path { … }
    pub fn offer(&self) -> &Offer { … }
    pub fn is_alive(&self) -> bool { … }
    pub fn events(&self) -> Option<EventsClient> { … }
    pub fn memory(&self) -> Option<MemoryClient> { … }
    pub fn approval(&self) -> Option<ApprovalClient> { … }
    pub fn scheduler(&self) -> Option<SchedulerClient> { … }
    pub fn model(&self) -> Option<ModelClient> { … }
    /// Nest `router` under the manifest's `route_namespace` and serve it on
    /// the kernel-bound listener (`A24_LISTEN_FD`).
    pub async fn serve(self, router: axum::Router) -> Result<(), SdkError> { … }
}

pub enum SdkError { Env(EnvError), Manifest(String), Connect(ConnectError), Io(std::io::Error) }
```

`manifest_yaml: &'static str` 而不是 `&[u8]`：manifest 本来就是 UTF-8 YAML，且 Sin90 的 `MANIFEST` 已是 `include_str!` 的结果（`main.rs:45`）。Sin90 按 feature 切换两份 manifest（`remote-allowed-manifest`）的做法不受影响。

### 3.3 请求上下文

```rust
/// `X-A24-Request-Id` of the proxied request being handled.
pub struct RequestId(String);              // no public constructor
impl RequestId { pub fn as_str(&self) -> &str { … } }

/// `X-A24-Approval-Token`: secret, redacted in Debug.
pub struct ApprovalToken(String);          // no public constructor

/// Kernel-injected per-request context. Extractor never fails.
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    pub request_id: Option<RequestId>,
    pub approval_token: Option<ApprovalToken>,
}
impl RequestContext { pub fn from_headers(h: &axum::http::HeaderMap) -> Self { … } }
impl<S: Send + Sync> FromRequestParts<S> for RequestContext { type Rejection = std::convert::Infallible; … }
```

### 3.4 错误

```rust
pub enum UnavailableCause { NoProvider, RequestRejected, BackendConfig, ResponseTooLarge }

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ClientError {
    Forbidden(String), RateLimited(String), Busy(String), QuotaExceeded(String),
    InvalidParams(String), Timeout(String), RequestNotInFlight(String), NotReady(String),
    Draining(String), Revoked(String), TokenInvalid(String), PayloadTooLarge(String),
    NotFound(String), ConnectionLost, NotSent(String),
    Unavailable { retryable: bool, cause: UnavailableCause },
    Cancelled, Other(String),
}

pub enum RetryClass { Permanent, Backoff, BackoffPauseBatch, OutcomeUnknown, GenerationOver, Unclassified }

impl ClientError {
    pub fn is_permanent(&self) -> bool { … }
    pub fn is_retryable(&self) -> bool { … }
    pub fn retry_class(&self) -> RetryClass { … }
}
```

（上面为可读性把变体压成一行；scratch 里每个变体带 `#[error(..)]`。）

### 3.5 五个客户端

```rust
impl EventsClient {
    pub fn new(conn: &Arc<Connection>) -> Option<Self> { … }
    pub async fn emit(&self, kind: &str, payload: Map<String, Value>,
                      request_id: Option<&RequestId>) -> Result<(), ClientError> { … }
    pub fn spawn_sink(&self, cfg: EventSinkConfig) -> EventSink { … }
}
pub struct EventSinkConfig { pub queue_capacity: usize, pub workers: usize,
                             pub sub_quota: usize, pub slot_wait: Duration }  // Default = 256/4/32/5s
impl EventSink { pub fn emit(&self, kind: &str, payload: Map<String, Value>) { … }
                 pub fn dropped(&self) -> u64 { … } }

impl MemoryClient {
    pub fn new(conn: &Arc<Connection>) -> Option<Self> { … }
    /// NOT idempotent on the wire: every success mints a new id.
    pub async fn remember(&self, kind: &str, body: Map<String, Value>,
                          request_id: Option<&RequestId>) -> Result<Remembered, ClientError> { … }
    pub async fn recall(&self, query: &str, page_size: usize, cursor: Option<&str>,
                        request_id: Option<&RequestId>) -> Result<RecallPage, ClientError> { … }
    pub async fn recent(&self, page_size: usize, cursor: Option<&str>,
                        request_id: Option<&RequestId>) -> Result<RecallPage, ClientError> { … }
}

pub struct ApprovalSubmit<'a> {
    pub action: &'a str, pub target: Option<&'a str>, pub payload: Value,
    pub request_id: &'a RequestId, pub approval_token: &'a ApprovalToken,
}
impl ApprovalClient {
    pub fn new(conn: &Arc<Connection>) -> Option<Self> { … }
    pub async fn gate(&self, s: &ApprovalSubmit<'_>) -> Result<ApprovalAnswer, ClientError> { … }
    pub async fn advise(&self, s: &ApprovalSubmit<'_>) -> Result<ApprovalAnswer, ClientError> { … }
    pub async fn status(&self, approval_id: &str) -> Result<ApprovalAnswer, ClientError> { … }
}

pub struct UpsertRequest<'a> { pub key: &'a str, pub spec: &'a ScheduleSpec,
                               pub enabled: bool, pub label: Option<&'a str> }
impl SchedulerClient {
    pub fn new(conn: &Arc<Connection>) -> Option<Self> { … }
    pub async fn upsert(&self, r: &UpsertRequest<'_>,
                        request_id: Option<&RequestId>) -> Result<UpsertResult, ClientError> { … }
    pub async fn delete(&self, key: &str,
                        request_id: Option<&RequestId>) -> Result<DeleteResult, ClientError> { … }
    pub async fn list(&self, request_id: Option<&RequestId>) -> Result<ListResult, ClientError> { … }
}

pub const MODEL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(125);
pub struct CompleteRequest { pub messages: Vec<ModelMessage>, pub response_format: Option<JsonSchemaFormat>,
                             pub max_tokens: Option<u32>, pub complexity: Option<Complexity> }
impl ModelClient {
    pub fn new(conn: &Arc<Connection>) -> Option<Self> { … }
    pub async fn complete(&self, req: &CompleteRequest,
                          request_id: Option<&RequestId>) -> Result<CompleteResult, ClientError> { … }
}
```

数据类型（`ScheduleSpec`/`ScheduleState`/`LastFire(s)`/`UpsertOutcome`/`DeleteOutcome`/`Remembered`/`Recollection`/`RecallPage`/`ApprovalAnswer`/`ApprovalKind`/`ApprovalDecision`/`ModelMessage`/`ModelRole{System,User,Assistant}`/`Complexity`/`JsonSchemaFormat`/`ServedTier`/`Usage{u64,u64}`/`CompleteResult`）字段与 Sin90 现有类型一一对应，只改了三处：`ModuleSpec → ScheduleSpec`、`ModuleScheduleState → ScheduleState`（去掉「Module」前缀，SDK 里一切都是模块的）；`ModelRole` 补 `Assistant`、`Usage` 用 `u64`（按内核 wire）。

与 Sin90 现签名的差别（迁移时要改的调用点）：`request_id: Option<&str>` → `Option<&RequestId>`；`SchedulerClient::upsert` 五个位置参数 → `UpsertRequest` 结构体；`ApprovalClient::gate/advise` 五个参数 → `ApprovalSubmit`。

### 3.6 fired（全文见附录 B.2）

```rust
pub const FIRED_PATH: &str = "/_a24/scheduler/fired";

pub struct FiredDelivery {
    pub fire_id: String,
    pub schedule_key: Option<String>,
    pub body: FiredBody,          // agent24_os_proto::kernel_call::FiredBody
    pub ctx: RequestContext,
}
pub enum FiredRejection { MissingFireId, BadBody(String), BadTimestamp }
impl IntoResponse for FiredRejection { … }                 // 400 {"error":{"code":"invalid_request",...}}
impl<S: Send + Sync> FromRequest<S> for FiredDelivery { type Rejection = FiredRejection; … }

pub fn with_fired<S, H, T>(router: Router<S>, handler: H) -> Router<S>
where S: Clone + Send + Sync + 'static, H: Handler<T, S>, T: 'static { … }
```

### 3.7 `examples/minimal.rs`（scratch 里编译通过）

```rust
const MANIFEST: &str = "name: minimal\nroute_namespace: /api/v1/minimal\nkernel_capabilities: [events, scheduler]\n";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let module = Module::builder(MANIFEST).connect().await?;
    let sink = module.events().map(|e| e.spawn_sink(Default::default()));
    let fired_sink = sink.clone();

    let app = Router::new().route(
        "/hello",
        get(move |ctx: RequestContext| async move {
            if let Some(s) = &sink {
                s.emit("hello.seen", Map::new());
            }
            axum::Json(json!({ "bound": ctx.request_id.is_some() }))
        }),
    );
    let app = with_fired(app, move |d: FiredDelivery| async move {
        // dedupe by d.fire_id in your own store first, then act
        if let Some(s) = &fired_sink {
            let mut m = Map::new();
            m.insert("key".into(), json!(d.body.key));
            s.emit("minimal.fired", m);
        }
        axum::Json(json!({ "status": "recorded" }))
    });
    module.serve(app).await?;
    Ok(())
}
```

（真实 example 的 manifest 走 `include_str!("minimal/domain-os.yml")`，并补 `impl_kind`/`spawn` 等安装必需字段；上面为了 scratch 自包含而内联。）

---

## 4. proto 要补的公开 API（5.1.2a 的主体）

全部放在新模块 `agent24_os_proto::module`（模块侧），不碰既有内核侧代码路径。签名见附录 B.1（scratch 桩，已编译）。

### 4.1 环境与握手

- `ModuleEnv::from_env() -> Result<ModuleEnv, EnvError>`，字段私有，`handshake_token` 在 `Debug` 里脱敏；读的是 `launch::ENV_*` 常量。
- `Hello<'a> { module, manifest_bytes, capabilities, protocol_min, protocol_max }`。
- `Connection::connect_from_env(&ModuleEnv, &Hello, FatalHook) -> Result<Connection, ConnectError>`：拨 `A24_CALLBACK_SOCK`，发 `InitializeRequest`（用既有 `InitializeParams` 序列化，不再手拼 `json!`），读响应用 `rpc::read_frame_async`，解析用既有 `InitializeResult`，校验 id 相同与缓冲区无残字节，然后 spawn mux 任务。
- 把 `manifest_digest` 放成 `pub fn` 并让内核侧的 `Expectation` 构造也用它（一处算法）。

### 4.2 多路复用连接

- `Connection::{offer, is_alive, call, notify}`；`CallOptions { response_timeout, slot_wait }`，默认 35s / 不等位。
- `CallError { NotSent, ConnectionLost, Busy, FrameTooLarge, Timeout, IdCollision, Rpc(RpcErrorInfo) }`，与 Sin90 `TransportError` 一一对应；`RpcErrorInfo { code, kind, message, data }`（Sin90 的 `raw` 改成只留 `data`，SDK 需要的 `retryable`/`cause` 都在 `data` 里）。
- 实现 = Sin90 `transport.rs:63-631` 的移植（单写任务、读任务、pending 表、`declare_dead` 恰好一次、`CallGuard` drop 发取消），常量改为引用 `rpc::*`；Sin90 的 16 条 transport 测试一并移植为 proto 测试（判据 J8）。
- `notify` 是本文相对 Sin90 新增的：JSON-RPC 通知（无 id、无响应）。今天没有模块→内核的通知方法，`$/cancelRequest` 由 `Connection` 内部发。保留 `notify` 是照 PLAN S3-1「只暴露类型化的 call/notify/cancel」；若评审认为无用户就不该有，可删。

### 4.3 监听 fd

- `listener_from_env(&ModuleEnv) -> std::io::Result<tokio::net::UnixListener>`：唯一一处把 fd 号变成 socket 的地方。**实现需要 `unsafe`**，方案待 §8 Q2 裁决。

### 4.4 其它补齐

- `kernel_call::FiredBody`（拥有型）+ `FireTrigger`，agentd `scheduler_deliver.rs` 改用它（§2.5）。
- `manifest` 模块：`pub use agent24_domain::DomainOsManifest` 或一个只含三字段的 `ManifestFacts` + 解析函数（scratch 用后者做桩）。
- `module::testing`（`test-util` feature）：内存假内核——给一个 `Connection` 与一个能按脚本应答、能记录收到的 `(method, params)` 的对端。SDK、Sin90、Cos72 的单测都用它，替代 Sin90 的 `clients/test_support.rs` 与 `mod.rs:617-632`。它在 proto 里用 `UnixStream::pair()`，不受 SDK 的 clippy 约束。

---

## 5. Sin90 迁移到 SDK（ME4-5.2.1 / Sin90 TS.1.1）

### 5.1 步骤

**前置 PR（Sin90 TS.1.0，建议新增，迁移前合）：线协议金样。** 在迁移前的 Sin90 上，用现有的假内核夹具（`clients/test_support.rs`）给每条会打内核的业务路径录一份「发出去的 `(method, params)` 序列」写成 golden 文件：
- reconciler：upsert（cron+tz）、delete、`list`、`memory.remember` 前的 recall 翻页 + remember；
- model：classify/summarize/propose 各一次 `complete`；
- events：一个 Routine 变更触发的 `emit`；
- 握手：`initialize` 的 params（去掉 `auth_token`）。
比较时忽略 JSON-RPC `id` 值与对象键序。

**迁移 PR（TS.1.1）**，按文件：

| Sin90 文件 | 动作 |
|---|---|
| `Cargo.toml` | 加 `agent24-os-sdk = { git = "...Agent24", tag = "agent24-os-sdk-v0.1.0" }`；dev 依赖加 proto 的 `test-util`。`sha2`/`hex` 若只剩 `manifest_digest` 在用则删 |
| `adapter_agent24/frame.rs` | **删** |
| `adapter_agent24/transport.rs` | **删**（测试已移入 proto，J8） |
| `adapter_agent24/clients/{error,memory,scheduler,approval,mod,test_support}.rs` | **删**，改为 `pub use agent24_os_sdk::{...}`（`clients` 模块保留为一行转出，减少调用点改动） |
| `adapter_agent24/clients/model.rs` | 只留 `ModelPort`/`ModelCaller` 实现与 `ClientError → ModelFailure` 映射（约 60 行 + 其测试）；`ModelRequest → CompleteRequest` 转换在这里写；`UnavailableCause` 在这里从 SDK 的转成 `crate::ai` 的 |
| `adapter_agent24/mod.rs` | 删 `SpawnEnv`/`manifest_digest`/`connect_and_initialize`/`KernelClients`/`KernelEventSink`/`listener_from_fd`/`INITIALIZE_CAPABILITIES` 及其测试；留 `wire_kernel_clients`（改吃 `&Module`），`KernelEventSink` 改为包一层 SDK `EventSink` 实现 Sin90 的 `http::EventSink` trait |
| `adapter_agent24/reconciler.rs` | 只改类型名与三处调用签名（`upsert` 用 `UpsertRequest`、`request_id` 传 `None`）；**分类 match 原样保留**，不改用 `retry_class` |
| `adapter_agent24/kernel_roundtrip.rs` | 头读取改用 `RequestContext`（test-hooks 路由） |
| `main.rs` | `SpawnEnv + KernelClients::handshake + listener_from_fd + nest("/api/v1/sin90")` 换成 `Module::builder(MANIFEST).on_connection_lost(同一个 exit(70) 钩子).connect()` + `module.serve(router)`；test-hooks 的两个路由照旧 merge |
| `http/mod.rs` 的 fired handler | **不动**（`http` 不许依赖 Agent24 类型，`lib.rs:3`）。可选：把 `"x-a24-fire-id"` 字面量换成 SDK 转出的 proto 常量——但这会让 `http` 依赖 SDK，违反 Sin90 自己的分层，**不做** |

### 5.2 零行为变化的验收办法

1. **真实挂载黑盒不变全绿**：`AGENT24_CHECKOUT=../Agent24 cargo test --test agent24_mount_blackbox -- --ignored --test-threads=1`，先 `-- --list --ignored` 断言恰好 5 条（`sin90_mounts_under_a_real_agent24_daemon`、`kernel_clients_roundtrip`、`routine_m3_real_mount_acceptance`、`t441_finalized_review_summary_is_recallable_from_kernel_memory`、`t551_ai_v1_m5_real_mount_acceptance`，`tests/agent24_mount_blackbox.rs:908/1175/1419/1861/2158`），跑之前 `../Agent24` ff 到含 SDK tag 的 main。
2. **线协议金样逐条相等**：TS.1.0 录的 golden 在迁移后重跑逐条相等（用 proto `test-util` 假内核录）。唯一允许的差异在 §8 Q7 裁决后写死（若改成 `1..=1`，`initialize.params.protocol_versions.max` 从 1000 变 1——这是**有意的**变化，在 PR body 里单列，不算回归）。变异：在迁移后的 Sin90 里把 `upsert` 的 `label` 改成发 `null` → golden 测试变红。
3. **常量同值**：一条 Sin90 单测断言 `EventSinkConfig::default()` 等于 `256/4/32/5s`、`MODEL_RESPONSE_TIMEOUT == 125s`；连接死时退出码仍是 70（`main.rs` 的钩子原样传入）。
4. **HTTP 面不变**：Sin90 `cargo test` 全绿（包含 `http/tests.rs` 的 fired 400 用例，因为 fired handler 没动）。
5. **净删代码**：`git diff --stat origin/main -- src/adapter_agent24` 删除行 > 新增行（TS.1.1 原验收）。按 §1.2 粗估：删 `frame.rs`（147）、`transport.rs`（1368）、`clients/` 下除 model 外的 6 个文件（约 2136）、`mod.rs` 约 700 行、`model.rs` 约 250 行，新增 < 200 行。
6. **结构判据**：Sin90 `src/` 里不再出现 `UnixStream`、`from_raw_fd`、`read_frame`、`"initialize"`（用 grep，正对照：在迁移前的树上同一 grep 非空）。
7. **全局前置**：`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`，外加 `--features test-hooks` 与 `--features remote-allowed-manifest` 两个变体（Sin90 CI 已有）。

---

## 6. 判据清单（J-S*）

规则同 PLAN §一 第 5 条：`cargo test <过滤>` 一律先 `-- --list` 断言匹配数 > 0；每条带正对照或变异。

| # | 判据 | 怎么验（机械） | 正对照 / 变异 |
|---|---|---|---|
| J-S1 | SDK 不持有 socket、不解析帧 | `cargo +1.98.0 clippy -p agent24-os-sdk --all-targets -- -D warnings` 绿 | `--features positive-control`（内含 `UnixStream::connect` 与 `serde_json::from_slice`）必须失败且输出含 ≥ 2 条 `use of a disallowed`；CI 里用 `! cargo clippy ... --features positive-control` 与 `grep -c` 两步写死。scratch 已验：2 条、退出 101 |
| J-S2 | 普通依赖里 `agent24-*` 只有 proto | `cargo tree -p agent24-os-sdk -e normal --depth 1 --prefix none \| grep '^agent24-'` 输出恰为 `agent24-os-proto` 一行 | 在 SDK `Cargo.toml` 临时加 `agent24-domain` → 输出两行 → 脚本红 |
| J-S3 | SDK 无 `unsafe` | 继承工作区 `forbid(unsafe_code)`；`cargo metadata` 断言 SDK 的 `lints.workspace == true` | 在 SDK 写 `unsafe {}` → 编译失败 |
| J-S4 | 错误映射覆盖内核闭集 | 测试遍历 `agent24_os_proto::rpc::ErrorKind::ALL`（18 个），每个 kind 映射到文档表里写死的变体；`-32602`、`timeout+retryable:false`、`unavailable` 缺 cause/缺 retryable/非布尔 retryable 各一例 | 删掉 `"quota_exceeded"` 分支 → 该例红；proto 新增第 19 个 kind 而 SDK 表没更新 → 遍历测试红 |
| J-S5 | 前缀门控 | 5 个客户端 × {`Offer` 含前缀 → `Some`，不含 → `None`} 共 10 例，用 proto `test-util` 假内核 | 把某个 `new` 改成无条件 `Some` → 对应例红 |
| J-S6 | 缺省不发 | 每个带可选字段的方法，`None` 时假内核收到的 params **没有**该键 | 把 `set_opt` 改成写 `Value::Null` → 红 |
| J-S7 | 与内核 wire 对等 | agentd 测试（dev 依赖 SDK）：每个方法把 SDK 构造的 params 反序列化进内核的 `deny_unknown_fields` params 类型必须成功；内核构造的 result 序列化后 SDK 结果类型反序列化必须成功 | 把 SDK `UpsertRequest` 的 `label` 键改名 → agentd 侧反序列化失败 → 红 |
| J-S8 | transport 语义 | Sin90 `transport.rs:686-1368` 的 16 条测试移植到 proto（第 65 个在途 `Busy`、drop 发**逐字节**正确的 cancel 帧、正常完成不发 cancel、迟到响应被丢且连接继续可用、断连 → 在途 `ConnectionLost` 且钩子恰好一次、关闭后调用 `NotSent`、超大帧在写任务前被拒、恰好 1 MiB 可过、响应超时 → `Timeout`+cancel、写超时 → 钩子一次、非 JSON 帧/超大入帧触发钩子、`slot_wait` 成功与超时） | 各测试原有的变异说明随迁移保留；新增：把 `CallGuard::drop` 里的 cancel 删掉 → cancel 帧测试红 |
| J-S9 | 超时关系 | proto 编译期断言 `DEFAULT_RESPONSE_TIMEOUT > rpc::CALL_TIMEOUT`；agentd 测试断言 `agent24_os_sdk::clients::MODEL_RESPONSE_TIMEOUT > model_callback::MODEL_CALL_TIMEOUT` | 把 125 改成 120 → agentd 测试红 |
| J-S10 | fired 提取器 | (a) SDK 测试：合法请求 200；缺 `X-A24-Fire-Id` 400；body 多一个字段 400；时间戳非定宽 400；`Result<FiredDelivery, FiredRejection>` 可自定义形状（scratch 已有 3 个测试全过）。(b) `FiredBody` 只有一份定义：`rg 'struct FiredBody' rust/` 恰 1 处且在 proto | (a) 去掉 `deny_unknown_fields` → 多字段例红；(b) 在 agentd 恢复私有 `FiredBody` → rg 计数 2 → 红 |
| J-S11 | 握手内容来自 manifest | 假内核捕获 `initialize` 的 `module` 与 `capabilities`，等于 manifest 的 `name` 与 `kernel_capabilities` | 在 SDK 里硬编码 `capabilities` → 用一份不同能力的 manifest 跑 → 红 |
| J-S12 | 握手后残字节 | 假内核在 `initialize` 响应后同一次写入里多塞一行 → `connect_from_env` 返回 `ConnectError::Protocol` | 删掉该检查 → 红 |
| J-S13 | example 挂载冒烟 | `rust/apps/agent24d/tests/me4_sdk_minimal_blackbox.rs`：构建 `examples/minimal`、`os install`、`os list` 为 `mounted`、经代理 GET `/api/v1/minimal/hello` 得 200 且 `bound: true`、事件流看到 `hello.seen`、一次 `run_now` 后看到 `minimal.fired`；`--list` 断言 1 条 | 把 example 的 `route_namespace` 改错 → 代理 404 → 红 |
| J-S14 | 发布 | `git ls-remote --tags origin agent24-os-sdk-v0.1.0` 非空；探针 `4c SDK` 为 ● | — |
| J-S15 | SDK ↔ wire 文档对齐（ME4-5.4.1 执行） | PR 附「SDK 源码中出现的每个协议常量 / 方法名 / 头名 → wire 文档章节」表；脚本抽取 SDK 与 proto `module` 里所有 `"_a24/`、`"x-a24-`、`"$/` 字面量，逐个在 `WIRE-OOP-MODULE.md` 里 grep 命中 | 在 SDK 里新增一个未入文档的方法名 → 脚本红 |
| J-S16 | Sin90 零行为变化 | §5.2 的 1–7 条 | 见 §5.2 |
| J-S17 | Cos72 只经 SDK | Cos72 仓库放同一份 `clippy.toml`（J-S1 的四个类型 + `from_raw_fd`），CI `clippy -D warnings` | 在 Cos72 写 `UnixStream::connect` → 红 |

---

## 7. 切法（先改 PLAN §三 与 `tasks.md` 台账再开工）

PLAN 里的 5.1.2a/b/c 三个 PR 装不下：盘点后「非测试代码」大约是 proto 模块侧 ~550 行 + SDK ~800 行。按「共享基础设施 / 核心逻辑 / 接入层」拆成 8 个 stacked PR，每个 ≤ 300 行非测试代码：

| 新编号 | 内容 | 仓库 | 估算（非测试） | 依赖 |
|---|---|---|---|---|
| ME4-5.1.2a1 | proto `module`：`ModuleEnv`、`Hello`、`manifest_digest` 公开、`connect_from_env` 的握手部分（先返回一个只能 `offer()` 的连接）、`manifest` 转出、`FiredBody` 挪进 `kernel_call` 并让 agentd 改用；J-S11、J-S12 | Agent24 | ~180 | 5.1.1 冻结 + §8 Q2 裁决 |
| ME4-5.1.2a2 | proto `Connection` 多路复用（移植 Sin90 transport）+ `test-util` 假内核；J-S8、J-S9 前半 | Agent24 | ~300（若超，拆 a2-core：读写任务/分发/在途上限 与 a2-cancel：`CallGuard`/超时/`slot_wait`/`declare_dead`） | a1 |
| ME4-5.1.2a3 | `listener_from_env`（按 Q2 方案）+ SDK crate 骨架（`Cargo.toml`、`clippy.toml`、`positive-control`、`Module`/`ModuleBuilder`/`SdkError`/`serve`）+ CI 步骤；J-S1、J-S2、J-S3 | Agent24 | ~180 | a2 |
| ME4-5.1.2b1 | `ClientError`/`RetryClass`/映射、`RequestContext`、客户端公共 `Core`；J-S4 | Agent24 | ~270 | a3 |
| ME4-5.1.2b2 | Events（含 sink）/ Memory / Approval；J-S5、J-S6 的对应部分 | Agent24 | ~250 | b1 |
| ME4-5.1.2b3 | Scheduler / Model + agentd 侧 wire 对等测试；J-S5/J-S6 其余、J-S7、J-S9 后半 | Agent24 | ~210 | b2 |
| ME4-5.1.2c1 | fired 提取器与 `with_fired`；J-S10 | Agent24 | ~100 | b3 |
| ME4-5.1.2c2 | `examples/minimal` + 挂载冒烟 + 探针 `4c` + tag `agent24-os-sdk-v0.1.0`；J-S13、J-S14 | Agent24 | ~60 + 测试 | c1 |
| Sin90 TS.1.0（新增） | 线协议金样录制（迁移前） | Sin90 | 测试为主 | — |
| ME4-5.2.1 / Sin90 TS.1.1 | 迁移；J-S16 | Sin90 | 净删 | c2、TS.1.0 |

说明：
- 若 §8 Q2 选「独立小 crate」，a3 里多一个 `rust/crates/agent24-os-fd`（约 20 行），仍在 300 行内。
- 若 §8 Q5 选「SDK 带 `remember` 去重助手」，放在 b2 之后单独一个 PR（约 120 行 + 测试），不塞进 b2。
- 若 §8 Q8 选「给 proto 拆 feature」，那是 a1 之前的一个独立重构 PR，改动面大（22k 行的 crate 的 `#[cfg]` 划分），不建议放在本轮。

---

## 8. 待用户拍板的开放问题

每个问题列选项与推荐；推荐只是推荐，未经确认不按它写代码。

**Q1（范围）——「只有两个调用方都要的才进 SDK」与 PLAN「五种客户端」怎么取舍？** 见 Q4，这是 Q4 的总括。

**Q2（架构/安全）——`listener_from_env` 的 `unsafe` 放哪？** 工作区 `unsafe_code = "forbid"`，一个 fd 号变 socket 只能 `unsafe { from_raw_fd }`。
- (a) 新建极小 crate `agent24-os-fd`（~20 行，该 crate 单独 `unsafe_code = "deny"` + 一处带 SAFETY 说明的 `#[allow]`），proto 依赖它；proto 与 SDK 继续 `forbid`。
- (b) proto 的 lint 从 `forbid` 降为 `deny`，在 `listener_from_env` 一处 `#[allow(unsafe_code)]`。
- (c) 不用 fd：内核改为给模块一个监听 socket 路径（`A24_LISTEN_PATH`），模块自己 bind——改内核 launch 契约、SPEC、Python 黑盒、以及「fd 3 由内核持有」的既有安全设计，代价大。
- (d) 依赖第三方 crate 代劳——已知的 `listenfd` 要求 systemd 风格 `LISTEN_FDS`/`LISTEN_PID` 环境变量，内核不设，需要改 launch。
- **推荐 (a)**：unsafe 被隔离在一个一眼能审完的 crate 里，不给 proto 这个 22k 行的内核协议 crate 开口子；与 ME3-SUP D3「用小 crate 替代自己写 unsafe」的精神一致（只是这次小 crate 是自己的）。

**Q3（产品/架构）——Cos72「发积分经内核审批」用 gate 还是 advise？** PLAN S4 写 `_a24/approval/gate`，但 gate 只接受内核能执行的闭集（今天只有 `schedule_callback`），发积分是 Cos72 自己库里的动作，内核执行不了。
- (a) 用 **advise** + 模块轮询 `status(approval_id)`，批准后 Cos72 自己入账；PLAN S4 措辞改成 advise。审批的约束力来自 Cos72 自己守规矩（SPEC §6.1：advise 是「知识，不是安全控制」）——但积分账本的真相本来就只在 Cos72 库里，内核无论如何都执行不了它。
- (b) 扩 gate 闭集：新增一种「内核批准后回调模块」的可执行动作（形态类似 fired：内核 POST `/api/v1/<ns>/_a24/approval/decided`），SDK 再多一个注册点。这是新的内核能力 + 新的保留路由 + SPEC 改动，要走一轮设计评审。
- **推荐 (a)**，(b) 记入下一轮。若选 (a)，SDK 是否提供轮询助手 `ApprovalClient::wait_decided(id, interval, deadline)` 顺带定（推荐不提供，Cos72 在自己的 outbox/泵里轮询，免得 SDK 替调用方决定轮询节奏）。

**Q4（范围）——Scheduler / Model / fired 进不进 SDK v0.1.0？** 按「两个真实调用方」规则：Events/Memory/Approval 两边都要；Scheduler 与 fired 的第二个调用方是 Python 黑盒与未来的 Node 模块，Cos72 只有在 ME4-5.3.4 想从模块侧验「读不到 Sin90 的 schedules」时才需要；Model 只有 Sin90。
- (a) 五个客户端 + fired 全进（与 PLAN 一致）。理由：Scheduler/Model 的 wire 已被 ME4-S1/S2 冻结，SDK 这一层只是机械翻译，形状出错的风险低；T14 的 Rust 对照需要它们。
- (b) Events/Memory/Approval + Scheduler + fired 进；Model 留在 Sin90，直到第二个用户出现（PLAN §三 与 5.1.2b 的验收要改）。
- (c) 只进三件（Events/Memory/Approval），其余留 Sin90。
- **推荐 (a)**。Cos72 是否申请 `scheduler` 由 5.3.1 定；若不申请，5.3.4 的 schedules 隔离从内核 REST 侧验。

**Q5（架构）——`remember` 的幂等问题谁解？** Sin90 用「写前先 recall 翻最多 10 页找标记」绕开（`reconciler.rs:255-386`），Cos72 的摘要写入会遇到同一个问题。
- (a) SDK 提供 `MemoryClient::remember_once(kind, body, dedup_marker, ...)`，把 Sin90 的翻页算法原样搬进来（含「翻不完就报错而不是当作不存在」）。
- (b) 内核给 `remember` 加可选幂等键（如 `dedup_key`，同模块同键返回已有 id）——新的 wire 字段 + 存储唯一索引 + SPEC 改动，是本轮计划外的内核 task。
- (c) 各模块自己写。
- **推荐**：本轮 (a)，并登记 (b) 为下一轮 followup；(b) 落地后 `remember_once` 改走内核幂等键，签名不变。

**Q6（兼容策略）——`ClientError` 要不要 `#[non_exhaustive]`？**
- (a) 不加：新增 kind = SDK 0.x minor 升级（0.x 下 minor 本就可破坏）；下游可以写无通配的穷举 match，漏处理新变体时**编译失败**——Sin90 `model.rs:166-188` 正是故意这样写的。
- (b) 加：SDK 加变体不破坏下游编译，但下游必须写 `_ =>`，失去编译期提醒。
- **推荐 (a)**（草图已按此）。

**Q7（兼容策略）——SDK 在 `initialize` 声明的协议范围：`1..=1000`（Sin90 现状）还是 `1..=1`？** 内核取 `min(模块上限, 内核上限)`：声明 1000 时，将来的 v2 内核会跟只懂 v1 的模块「成功」协商成 v2。今天内核是 v1，两种写法行为相同。
- **推荐 `1..=1`**（SDK 只声明它实现了的），新协议版本随 SDK 升级一起放开。Sin90 迁移后这是握手 params 里唯一有意的变化（§5.2 第 2 条单列）。

**Q8（工程成本）——proto 要不要拆 `kernel` / `module` feature？** 不拆：Sin90/Cos72 编译时带上 `command-fds`、`close_fds`（精确钉版）、`rustix`、`hyper`、`agent24-domain`（`serde_yaml`、`schemars`）。拆：proto 22k 行要按 feature 划 `#[cfg]`，本轮工作量明显变大。
- **推荐本轮不拆**，在 5.2.1 的 PR 里记录 Sin90 冷编译时间变化，登记 followup；若 Cos72 或第三方反馈成本不可接受再拆。

**Q9（治理）——`agent24-os-sdk` 的 tag 由谁打、`0.1.x` 补丁怎么发？** PLAN 只写了 `agent24-os-sdk-v0.1.0`。建议：SDK tag 与 Agent24 版本 tag 独立，只在 SDK 或 proto `module` 有改动时打；每个 SDK tag 在 `CHANGELOG.md` 的独立小节列出对应的内核最低版本。v0.5.0 发布清单（ME4-6.0.1）要把这个 tag 列进去。需要用户确认「SDK 与 daemon 分开打 tag」这件事本身。

**Q10（架构）——库默认 `exit(70)` 可以吗？** 连接断了这一代就结束（无重连），Sin90 的做法是立刻退出让 supervisor 重启。
- (a) SDK 默认同 Sin90（`warn!` + `exit(70)`），可用 `on_connection_lost` 覆盖。
- (b) SDK 默认只记日志，让 `Module::serve` 返回错误、由 `main` 决定——代价是 serve 要同时盯连接存活，在途 HTTP 请求会被中断，且退出码变了。
- **推荐 (a)**：零配置就是正确行为，覆盖口留给测试。

**Q11（架构）——wire DTO 放两份（内核私有 + SDK）还是挪进 proto 共用？** 内核的 params 类型都在 agentd 里且大多私有（`memory_callback.rs:36-67` 等）。
- (a) SDK 自己一份（请求严格、响应宽松），agentd 加对等测试钉住（J-S7）。
- (b) 把请求/响应类型挪进 proto，内核与 SDK 共用——要改已冻结的 S1/S2/ME-3e 各 handler 的类型出处，改动面大、要再过评审。
- **推荐 (a)**，唯一例外是 `FiredBody`（它原本就只是一个借用序列化结构，挪进 proto 成本极小，且方向是内核→模块，两边共用一份最省事）。

---

## 9. 自审（送评审前自己先挑的毛病）

1. **本文的 proto `Connection` 只是桩。** 移植 Sin90 transport 时最容易丢的是 `declare_dead` 与 `closed` 标志在同一把锁下插入 pending 的竞态防护（Sin90 `transport.rs:179-218`）。J-S8 移植了 Sin90 的并发测试，但评审应专门看 a2 的这一段。
2. **`RequestId` 无公开构造函数**会让「后台工作显式不绑定」只能传 `None`——这是故意的；但 Sin90 的 `kernel_roundtrip` 测试路由与 Cos72 的测试需要构造一个。给 `test-util` feature 开一个 `RequestId::for_test`，还是让测试总是走真实代理？倾向前者，未写进草图。
3. **`EventSink` 的丢弃计数只在溢出时加**；Sin90 的「按 2 的幂打日志」没搬进草图（草图只计数）。实现时补上，属机械移植。
4. **`Module::serve` 返回即意味着 HTTP 服务停了**，但连接可能仍活着；反之连接死了由 `FatalHook` 负责退出。两者不联动是 Sin90 现状，保留。
5. **fired 提取器的时间戳校验**与内核的 `fmt_iso` 输出耦合：内核若改成带毫秒的格式，所有模块的 fired 会被 400 并反复重投直到 `failed`。这条耦合应写进 wire 文档并在 agentd 侧加一条「`fmt_iso` 输出满足 SDK 校验」的测试（归入 J-S7）。
6. **J-S15 的字面量抽取**会漏掉拼接出来的方法名（`format!("{}{suffix}", prefix)`）。实现时让每个客户端把完整方法名列成 `const` 数组，脚本从数组取。

---

## 10. 待登记的 followups（本分支不改 `followups.md`；由统筹在规划分支登记编号）

- 内核 `remember` 幂等键（§8 Q5 (b)）。
- 审批裁决回推（§8 Q3 (b)）。
- proto `kernel`/`module` feature 拆分（§8 Q8）。
- Sin90 的 reconciler 是否改用 `retry_class`（`Cancelled` 分类差异，§2.6）。
- 本文评审若仍是 Tier 2（Opus 子代理），记 `ME4-CODEX-DEBT`，额度恢复后补 Codex 一轮。

---

## 附录 A：scratch crate 与命令记录

位置：`scratchpad/sdk-sketch/`（工作区：`proto/` = proto 模块侧 API 桩，`sdk/` = SDK 草图 + `examples/minimal.rs` + `tests/sketch.rs` + `clippy.toml`；`pcfd/` = 独立小 crate，只为验证 `from_raw_fd` 的 clippy 路径）。工作区 lint 与 Agent24 相同：`unsafe_code = "forbid"`、`unwrap_used`/`expect_used = "deny"`，edition 2024。

```
$ rustup run 1.98.0 cargo check --workspace --all-targets --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s)

$ rustup run 1.98.0 cargo clippy --offline --workspace --all-targets -- -D warnings
    Checking agent24-os-proto v0.3.0 (.../sdk-sketch/proto)
    Checking agent24-os-sdk v0.1.0 (.../sdk-sketch/sdk)
    Finished `dev` profile [unoptimized + debuginfo] target(s)

$ rustup run 1.98.0 cargo test --offline -p agent24-os-sdk
test retry_classes ... ok
test caller_can_render_rejection_in_its_own_shape ... ok
test fired_ok_and_rejections ... ok
test result: ok. 3 passed; 0 failed

$ rustup run 1.98.0 cargo fmt --all --check      # 无输出，退出 0

# J-S1 正对照
$ rustup run 1.98.0 cargo clippy --offline -p agent24-os-sdk --features positive-control -- -D warnings
error: use of a disallowed type `tokio::net::UnixStream`
 --> sdk/src/positive_control.rs:4:13
error: use of a disallowed method `serde_json::from_slice`
 --> sdk/src/positive_control.rs:5:13
error: could not compile `agent24-os-sdk` (lib) due to 2 previous errors      # exit 101

# from_raw_fd 路径（独立 crate，无 forbid，同一份 clippy.toml）
$ cd pcfd && rustup run 1.98.0 cargo clippy --offline -- -D warnings
error: use of a disallowed method `std::os::fd::FromRawFd::from_raw_fd`
 --> src/lib.rs:3:14
```

scratch 的非空行（不含注释）计数，供 §7 估算：SDK `error.rs` 165、`module.rs` 114、`clients/scheduler.rs` 113、`clients/events.rs` 109、`clients/model.rs` 93、`fired.rs` 89、`clients/approval.rs` 73、`clients/memory.rs` 68、`clients/mod.rs` 60、`context.rs` 47、`lib.rs` 17；proto 桩 189（真实实现会因 mux 增加约 300）。

## 附录 B：两份承重文本（签名与 scratch 逐字一致；B.1 的函数体以 `{ … }` 省略，B.2 为完整函数）

### B.1 proto `module` 桩（签名即 5.1.2a 的目标 API）

```rust
/// NEW: the module-side half of the protocol (ME4-S3 §4.1).
pub mod module {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::Value;

    use crate::initialize::Offer;

    /// Runs exactly once when the connection is judged dead (no reconnect:
    /// one callback connection per generation, D1).
    pub type FatalHook = Arc<dyn Fn() + Send + Sync>;

    #[derive(Debug)]
    pub enum EnvError {
        Missing(&'static str),
        BadFd(&'static str),
    }

    /// The four spawn variables. `handshake_token` is private and redacted.
    pub struct ModuleEnv {
        data_dir: PathBuf,
        callback_sock: PathBuf,
        handshake_token: String,
        listen_fd: i32,
    }

    impl ModuleEnv {
        pub fn from_env() -> Result<Self, EnvError> { … }
        #[must_use]
        pub fn data_dir(&self) -> &Path { … }
    }

    /// What the module says in `initialize`, beyond the env-provided token.
    #[derive(Debug, Clone)]
    pub struct Hello<'a> {
        pub module: &'a str,
        pub manifest_bytes: &'a [u8],
        pub capabilities: &'a [&'a str],
        pub protocol_min: u32,
        pub protocol_max: u32,
    }

    #[derive(Debug)]
    pub enum ConnectError {
        Io(std::io::Error),
        Refused { code: i64, kind: Option<String>, message: String },
        Protocol(String),
    }

    /// Kernel application error, parsed once (`code`, `data.kind`, raw `data`).
    #[derive(Debug, Clone, PartialEq)]
    pub struct RpcErrorInfo {
        pub code: i64,
        pub kind: Option<String>,
        pub message: String,
        pub data: Option<Value>,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub enum CallError {
        NotSent,
        ConnectionLost,
        Busy,
        FrameTooLarge,
        Timeout,
        IdCollision,
        Rpc(RpcErrorInfo),
    }

    #[derive(Debug, Clone, Copy)]
    pub struct CallOptions {
        pub response_timeout: Duration,
        /// `None` = fail fast with `Busy` when all slots are taken.
        pub slot_wait: Option<Duration>,
    }

    impl Default for CallOptions {
        fn default() -> Self {
            Self { response_timeout: Duration::from_secs(35), slot_wait: None }
        }
    }

    pub struct Connection { /* offer + mux internals */ }

    impl Connection {
        pub async fn connect_from_env(
            env: &ModuleEnv,
            hello: &Hello<'_>,
            on_fatal: FatalHook,
        ) -> Result<Self, ConnectError> { … }
        #[must_use]
        pub fn offer(&self) -> &Offer { … }
        #[must_use]
        pub fn is_alive(&self) -> bool { … }
        /// Dropping the returned future before it resolves sends
        /// `$/cancelRequest` for its id (the plan's "cancel").
        pub async fn call(
            &self,
            method: &str,
            params: Value,
            opts: CallOptions,
        ) -> Result<Value, CallError> { … }
        /// JSON-RPC notification (no id, no response).
        pub async fn notify(&self, method: &str, params: Value) -> Result<(), CallError> { … }
    }

    /// Adopt `A24_LISTEN_FD` as a listener axum can serve on. The ONE place
    /// an fd number becomes a socket (needs the unsafe decision, §8 Q2).
    pub fn listener_from_env(env: &ModuleEnv) -> std::io::Result<tokio::net::UnixListener> { … }
}
```

（scratch 里 `{ … }` 处是占位实现；`Debug for ModuleEnv` 的脱敏实现从略。）

### B.2 SDK `fired.rs` 的提取器与注册点

```rust
impl<S: Send + Sync> FromRequest<S> for FiredDelivery {
    type Rejection = FiredRejection;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let h = req.headers();
        let fire_id = h
            .get(FIRE_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(FiredRejection::MissingFireId)?;
        let schedule_key = h
            .get(SCHEDULE_KEY_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let ctx = RequestContext::from_headers(h);
        let Json(body) = Json::<FiredBody>::from_request(req, state)
            .await
            .map_err(|e| FiredRejection::BadBody(e.body_text()))?;
        if !is_fixed_iso8601(&body.scheduled_for) || !is_fixed_iso8601(&body.fired_at) {
            return Err(FiredRejection::BadTimestamp);
        }
        Ok(Self { fire_id, schedule_key, body, ctx })
    }
}

/// The fired registration point: mounts `handler` at [`FIRED_PATH`].
pub fn with_fired<S, H, T>(router: Router<S>, handler: H) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    H: Handler<T, S>,
    T: 'static,
{
    router.route(FIRED_PATH, post(handler))
}
```
