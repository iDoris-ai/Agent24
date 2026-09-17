# T7b / ME-3e（拆分 2/3，`advise` 完整 + `gate` 协议骨架；执行验收留给 T7c）—— 模块审批：`_a24/approval/gate` / `_a24/approval/advise` / `_a24/approval/status`

## 与 T7a 的关系，以及这一版的范围收窄

T7a（已合并 PR #199）把「进程外模块能不能拿到能力授予」这条地基修好，用事件回调把 `Grants`/`Offer`/`MethodsFor` 这套机制跑通、测出来。T7b 直接复用这套地基，只新增审批本身需要的部分。

**v1 送评审后（0 Critical / 7 High / 4 Medium / 1 Low）发现范围声明本身有问题，这一版明确收窄**：SPEC ME-3e 的验收要求「批准/拒绝的执行路径符合 §6 第 3 条」「批准 A、执行 B 不可能」——这两条只有在**真的存在一个内核可执行动作**时才可能被测出来（决策记录 4 沿用 T7a 前置分析：本轮内核可执行动作闭集为空，`gate` 对任何调用都直接 `forbidden`，不建审批记录）。所以：

**这一版 T7b 交付的是「`advise` 完整可用 + `gate` 的协议骨架（永远 forbidden，因为闭集为空）」，不是「gate 的执行验收」**。SPEC 那两条关于「批准 A 执行 B」的验收条目本轮**明确 deferred**，留给未来某个交付第一个内核可执行动作的任务（暂记 **T7c**）。这不是回避审批本身该有的严谨性——`advise` 一样要经过用户真实决定、一样要防重放/错配/陈旧决定，只是它的副作用留在模块自己域内，内核不代为执行。

T7b 范围明确不包含：`gate` 的真实执行、内核可执行动作闭集的任何非空内容、`agent24-cli` TUI 扩展、渠道桥（wechat/nostr）改动。

## v4 → v5：从「同步阻塞等答案」改成「异步提交 + 轮询」——用户 2026-09-17 裁决的架构重设计

**这不是又一轮小修补，是交互模型本身的更换**。Codex 连续四轮设计评审（v1→v4，累计 0 Critical / 26 High / 17 Medium / 4 Low，逐条修复记录见文末历史 changelog）里，最难的一类问题——决定 CAS 成功之后交付给等待中的 RPC 调用这一步可能永久卡死、`PendingGuard`/per-row 定时器架构描述反复对不上真实实现、`DELIVERY_GRACE`/`RequesterGone` 这些概念边界始终讲不干净——根子都在同一个假设上：**`advise` 的 `Handler::call()` 要一直阻塞到人做出决定才返回**。v4 评审最后指出这个假设本身有硬伤：

> 现有回调方法的统一超时是 30 秒（`rpc.rs`），原始代理请求的总 deadline 也是 30 秒（`proxy.rs`）。人类审批决定不可能保证在 30 秒内做出——现有的、`run_id` 那一套审批默认超时反而是 300 秒（`server.rs`）。一个同步阻塞的 `advise` 调用活不过 30 秒，`request_is_live`会在原始代理请求早就正常结束之后判定"死了"，而那时用户可能压根还没看到这条审批。

**改成异步提交 + 轮询**之后，这一整类问题**不是被修复，是被消除**：`_a24/approval/advise` 变成一个立刻返回的"提交"调用（跟建一行数据库记录一样快，稳稳落在 30 秒以内），真正的决定通过模块自己发起的第二次调用（`_a24/approval/status`）按自己的节奏查——查询是幂等的读操作，不存在"决定已经产生、但还没交付给谁"这种中间态，因为**压根没有"交付"这个动作**，模块想知道结果随时自己来读。`PendingGuard`、`oneshot` 通道、`DELIVERY_GRACE`、`RequesterGone`、`request_is_live`——这些为了给一次同步阻塞调用兜底而发明出来的机制，在异步模型下全部不需要。

## 现状（均已读源码验证，含订正）

1. **T7a 交付、本轮直接复用但需要显式改写的接口**：
   - `Capability`（`agent24-domain/src/lib.rs:141-156`）加 `Approval` 变体，同时要改 `ALL_CAPABILITIES`（`lib.rs:161-166`）和 `as_str()`（`lib.rs:209-217`），以及 `lib.rs:1239` 一带的穷举防漂移测试——三处 + 一条测试必须同步。
   - `agent24d/src/domain.rs:70` 的 `KERNEL_GRANTS`（进程内）和 `domain.rs:77` 的 `KERNEL_OOP_GRANTS`（进程外）都要加 `Capability::Approval`——两个独立常量，加一个不会带出另一个。
   - `Offer` 的构造（`domain.rs:1201-1213` 一带）现在是 `if granted.has(Events) { events 前缀 } else { none }`，**改成按位累加**：`let mut provides = Vec::new(); if granted.has(Events) { provides.push("_a24/events/") } if granted.has(Approval) { provides.push("_a24/approval/") } Offer { provides }`——否则一个只被授予 `approval`、没被授予 `events` 的模块会拿到空 `Offer`。
   - `MethodsFor` 闭包（`domain.rs:1214-1241`）现在只 `.with("_a24/events/emit", ...)` 一个方法；本轮**三个新方法都要无条件注册**（`gate`/`advise`/`status`，能力门控在 handler 内部做，跟 `events_emit.rs` 完全同一个模式）。
   - `ErrorKind` 闭集（`agent24-os-proto/src/rpc.rs:112-181`）新增变体必须同步进 `ALL`（`141-158`）、`as_str()`（`161-180`）、SPEC 闭集文案（`SPEC-ME3-OUT-OF-PROCESS.md:170`）、以及 `rpc.rs:1920` 一带把 `ErrorKind::ALL` 跟 SPEC 引文精确比对的测试。**这一轮只新增一个变体：`TokenInvalid`**（决策记录 2）——闭集外的动作复用既有的 `Forbidden`（SPEC §6.1 原文写死这个措辞，不留给实现者另创更精确 kind 的空间）。
   - `agent24-cli/src/tui/app.rs:201` 一带对 `EventBody` 做穷举 `match`——本轮新增的 `ModuleApprovalRequired`/`ModuleApprovalResolved`（决策记录 7）会让这个 `match` 编译失败，需要显式加 `EventBody::ModuleApprovalRequired(_) | EventBody::ModuleApprovalResolved { .. } => {}` 这样列出具体变体的分支，不能写成会拆掉穷举防漂移效果的 `_ => {}`。
   - `agent24d/src/domain.rs:107` 一带的 `RESERVED_KERNEL_SEGMENTS` 要加上 `module-approvals`，否则同名模块能在 axum 路由层造成冲突。
   - `agent24d/src/server.rs`（或等价的 `AppState` 定义处）新增 `module_approval_broker: Arc<ModuleApprovalBroker>` 字段、`mod module_approvals;` 声明、路由注册（决策记录 7），以及把这个 broker 一路传给 `mount_all`/`mount_package`（供决策记录 6 的 `PolicyApprovalBackend` 使用）。
   - `ParamsBudget`/`params_budget_of`（`rpc.rs:382-461`，`pub`）**这一条不需要改**——`dispatch()` 层对所有方法通用生效，`gate`/`advise`/`status` 的 params 自动被覆盖。
2. **异步模型下，`Generation`/`drain.rs` 的状态机只在"提交"这一刻起作用**（`drain.rs:216-225` `Inner`）：`admit_callback`（`drain.rs:363-375`）在 `Running` 无条件 `Ok(())`、`Draining` 要求 `request_id` 仍在 `in_flight`——这个既有状态机只需要在 `gate`/`advise` 的**提交**调用里复用一次（决策记录 2），回答"这次提交是不是来自一个仍然合法的连接/请求"。**`_a24/approval/status`（决策记录 4）完全不碰这个状态机**——它是一次独立的、跟原始代理请求毫无关系的查询，问的是"这个 `approval_id` 现在的决定是什么"，不是"那个旧请求还活不活着"，所以也不需要 v1-v4 反复想解决的"存活性重查"这类机制（详见上面"v4→v5"的说明）。
3. **`proxy.rs` 铸 `request_id` 的地方**（`proxy.rs:968`，`state.ids.mint()`）与注入转发头的地方（`proxy.rs:1177-1178`）：本轮在同一处新增铸造+注入 `X-A24-Approval-Token`。`strips_from_request`/`strips_from_response`（`proxy.rs:161-232`）已经覆盖所有 `X-A24-*` 前缀，新令牌自动享受。
4. **现有审批模型完全独立，字段不兼容**：`ApprovalRequest`（`agent24-policy/src/lib.rs:95-111`，`run_id: &str` 必填）、`Approval`（`agent24-protocol/src/types.rs:378-...`，`run_id: String` 必填）、`ApprovalBroker`（`agent24-policy/src/lib.rs:139-154`，这套本身是同步阻塞模型，`pending: Mutex<HashMap<String, oneshot::Sender<Decision>>>`——**本轮明确不照抄这套阻塞机制**，只借用 `agent24d/src/approvals.rs` REST 端点的手动 body/query 解析代码风格）。本轮不改这些，模块审批平行建一套。
5. **`EventBroadcast`/`HubBroadcast` 是 `ApprovalRequester` 要照抄的分层先例**：`trait EventBroadcast` 定义在 `agent24-domain/src/lib.rs:908`，具体实现 `struct HubBroadcast(EventsHub)` 在 `agent24d/src/domain.rs:131-133`（daemon 侧适配器）。`agent24-domain` 不依赖 `agent24-policy`（已读两个 crate 的 `Cargo.toml` 确认），`agent24-domain` **确实**依赖 `agent24-protocol`（`agent24-domain/Cargo.toml:8`）——所以 `ModuleApprovalKind`/`ModuleApprovalDecision` 这类跨 crate 共用的小类型放 `agent24-protocol`，`ApprovalBackend` trait 放 `agent24-domain`，具体实现 `PolicyApprovalBackend` 放 `agent24d`。`EventSink::new`（`agent24-domain/src/lib.rs:960`）接受 `&DomainOsManifest` 而不是裸字符串——`ApprovalRequester` 的构造函数要照抄这一点，不能接受任意 `impl Into<String>`。
6. **`resume.rs::assess_restore`**（`agent24-agent/src/resume.rs:201-262`）：不改，只借用其「从不可篡改源头重新推导、精确比较」原则；本轮 `gate` 闭集为空用不上，`advise` 不涉及内核执行也用不上，留给 T7c。

## 决策记录

### 1. `RequestContext` 不建新的全局表——沿用 T7a 已验证过的简化（不变）

「哪个模块」来自回调连接绑定的 `Arc<Generation>`（闭包捕获的 `name`），「是否仍活跃、属于哪一代」在**提交**调用里复用 `Generation` 自身状态（决策记录 2）。「run/session/source」今天没有任何被代理的请求路径携带，`notify` 这一版**不加进 `ModuleApproval`**（Codex 第 4 轮 Medium 4 指出上一版声明了一个仓库里不存在的 `NotifyTarget` 类型，编不过也生成不了 schema）——没有真实数据源的字段不预先占位，等未来 run/session/source 真的接进代理路径时，`notify` 跟着那个任务一起加，不属于本轮的"声明了但注定用不上"。

### 2. `approval_token`：只在**提交**这一刻核验一次，不再有"第二次核验"的场景

铸造与登记必须在同一次 `admit_request` 调用里完成，不是先 `admit_request(id)` 再第二次加锁补令牌——把 `Generation::admit_request` 的签名从 `admit_request(id: String)` 改成 `admit_request(id: String, token_hash: [u8; 32])`，`Inner.in_flight: HashSet<String>` 相应改成 `HashMap<String, ApprovalToken>`：

```rust
struct ApprovalToken {
    hash: [u8; 32], // sha256(明文令牌)，不存明文
    used: bool,
}
```

**参数**：32 字节 `/dev/urandom`（拿不到熵就硬失败，不退化——跟 `launch::mint_token` 的既有先例一致），十六进制编码成 64 字符字符串放进 `X-A24-Approval-Token` 请求头；哈希用 `sha2::Sha256`（`agent24-os-proto` 需要新增 `sha2` 依赖）；比对复用 `agent24d/src/server.rs:626` 的同款异或折叠常数时间比较（复制一份到 `agent24-os-proto`，两个 crate 不互相依赖）。

**核验+核销是一次不可分割的操作**：

```rust
impl Generation {
    pub fn admit_approval_callback(&self, request_id: &str, token: &str) -> Result<(), CallbackRefused> { ... }
}
```

**错误矩阵，四种 `DrainState` 全部覆盖**：

| 检查顺序 | 条件 | 结果 |
|---|---|---|
| 1 | `DrainState::Starting` | `not_ready` |
| 2 | `DrainState::Revoked` | `revoked` |
| 3 | `Running`/`Draining`，但 `request_id` 不在 `in_flight`（含：从没存在过、已经 `finish()`、属于旧一代） | `token_invalid` |
| 4 | `request_id` 在 `in_flight`，但令牌哈希不对，或 `used == true` | `token_invalid` |
| 5 | 以上全部通过 | `Ok`，原子标记 `used = true` |

第 3、4 折叠成同一个 `token_invalid`，不给未授权探测提供「id 错还是 token 错」的信息。

**这个核验只发生一次，就在提交调用里**——异步模型下没有"决定送达"这一步，也就没有第二次需要核验令牌的场景（这是跟 v1-v4 最大的结构性差异：以前 `advise` 要在收到决定、准备返回时再查一次「这个 `request_id` 是否还活着」，现在完全不需要）。

### 3. `ModuleApproval`：只有一个决定维度，没有"交付"这个概念

```rust
pub struct ModuleApproval {
    pub id: String,          // 提交时铸造的 approval_id——32 字节随机、十六进制，
                              // 是查询这条记录唯一需要的凭据，不派生自 request_id
    pub module: String,      // 来自回调连接身份（闭包捕获），模块不可自报
    pub request_id: String,  // 决策记录 4 的幂等去重键之一（跟 module/kind 一起构成 UNIQUE）
    pub kind: ModuleApprovalKind,   // Gate | Advise（放 agent24-protocol，见决策记录 6）
    pub binding: bool,       // kind == Advise 时恒 false；kind == Gate 本轮恒不可达
    pub action: String,
    pub target: Option<String>,
    pub payload: serde_json::Value,   // 内核收到的真实值，供人工审阅（不代表模块最终
                                       // 真的会按这个 payload 执行——advise 的副作用在
                                       // 模块自己域内，内核只保证"记录的是提交时刻收到
                                       // 的原样"，不保证"模块后续照做"，SPEC §6.1 早已
                                       // 把 advise 定性为知情权而非安全控制，这里如实
                                       // 承认，不夸大这条记录的约束力）
    pub payload_digest: String,       // approval_digest(&payload)（决策记录 6 定义，唯一
                                       // 计算点在 ModuleApprovalBroker::submit 内部；wire
                                       // handler/ApprovalRequester 都只管传 payload 进去）
    pub decision: ModuleApprovalDecision,
    pub created_at: String,
    pub decided_at: Option<String>,
    pub expires_at: String,  // 过期时间，决策记录 5 的周期扫描用这个字段判定 TimedOut
}

pub enum ModuleApprovalKind { Gate, Advise }
pub enum ModuleApprovalDecision { Pending, Approved, Denied, TimedOut }
```

`(module, request_id, kind)` 三列 `UNIQUE`——这既是数据完整性约束，也是决策记录 4 提交幂等去重直接依赖的机制（一次原始代理请求最多产生一条 gate 记录、一条 advise 记录，重试天然落到同一行）。

**决定 CAS，唯一的一次状态转换**：`UPDATE module_approvals SET decision = ?, decided_at = ? WHERE id = ? AND decision = 'pending' AND expires_at >= ?`。受影响行数 0 → 先按 `id` 单独查一次区分原因（Codex 第 5 轮 Medium 1）：查无 → `404`；查到但 `decision != 'pending'` 或已过期 → `409`——这是 REST 层为了给出准确错误码做的一次读，不是 CAS 本身的一部分，`agent24-store/src/repo.rs:445` 一带的现有 CAS 先例就是这么处理 0 行结果的。`now` 取操作发起时刻（构造 SQL 参数那一刻），不是等到拿到数据库锁之后才取——避免长时间排队之后用一个更晚的时间戳去判定一条其实还没过期的记录。**没有第二个维度需要联合约束**——`decision` 单列 `CHECK` 就足够表达全部合法状态。

`approval_id` 本身按秘密处理（虽然它不像 `approval_token` 那样直接控制一次准入判定，但知道它就能查到这条记录的 `action`/`target`/`payload`，属于该保密的信息）：32 字节随机、十六进制，不从 `request_id` 或其它可预测的值派生。

### 4. `gate` / `advise` / `status`：提交立即返回，查询随时可查

```
_a24/approval/gate      ← 提交。内核执行类动作。闭集为空，本轮永远 forbidden（决策记录 6/T7c）
_a24/approval/advise    ← 提交。仅呈现与记录，不阻塞等待。
_a24/approval/status    ← 查询。带上 approval_id，随时可以再查，查多少次都行。
```

**wire params，含 SPEC 强制要求的 `_meta`**（Codex 第 4 轮 High 6 指出 v4 漏了这个字段，配合 `deny_unknown_fields` 会把合法 `_meta` 当未知字段拒绝）：

```rust
#[serde(deny_unknown_fields)]
struct ApprovalSubmitParams {
    action: String,
    target: Option<String>,
    payload: serde_json::Value,
    request_id: String,
    approval_token: String,
    #[serde(default)]
    _meta: Option<serde_json::Map<String, serde_json::Value>>,
}

#[serde(deny_unknown_fields)]
struct ApprovalStatusParams {
    approval_id: String,
    #[serde(default)]
    _meta: Option<serde_json::Map<String, serde_json::Value>>,
}
```

**`gate`/`advise`（提交）处理顺序（Codex 第 5 轮 High 1/3 订正后）**：
1. 能力检查（`granted.has(Approval)`，无 → `forbidden`，不计入任何配额或记录）。
2. `kind == Gate` 时先查闭集（`check_closed_set(&action)`，本轮零变体，必查不到）→ `forbidden`，**在核销令牌之前**（Codex 第 5 轮 High 1 指出：先核销令牌再查闭集，会让一次必然失败的 `gate` 调用白白烧掉这次请求的令牌，导致同一个 `request_id` 之后连 `advise` 都用不了）。这一步用的是跟决策记录 6（`PolicyApprovalBackend`）共享的同一个自由函数，进程内、进程外两条路径都过它。
3. **幂等去重**：按 `(module, request_id, kind)` 查是否已经有一条记录（`module_approvals` 对这三列建 `UNIQUE` 约束）——有就直接把那条记录的 `{approval_id, decision, kind, binding}` 原样返回，**不重新核验令牌、不重新建记录**（Codex 第 5 轮 High 1 指出的核心场景：模块提交成功、但响应在返回途中丢失/连接被取消，模块重试时用的还是同一个 `request_id`——`request_id` 在同一次原始代理请求的生命周期里是不变的，天然就是这次提交的幂等键，不需要另外发明一套重试协议）。
4. 没有已存在的记录 → `admit_approval_callback(request_id, approval_token)`（决策记录 2）——失败按错误矩阵返回，不建记录。
5. 插入 `ModuleApproval{decision: Pending, kind, binding: kind == Advise}`（`payload_digest` 由 `approval_digest()`——决策记录 6——统一计算），推 WS `module-approval.required`。**这一步失败**（存储暂时不可用）→ 令牌已经核销、记录没插入成功：返回 `BackendUnavailable`，**这次令牌确实作废，模块的这次提交需要拿新的一次代理请求重试**（同一个 `request_id`/`token` 不可能再要到第二次机会——这是本设计承认的、按现实比例可接受的残余代价：本地 SQLite 写入失败在正常运行下极罕见，不值得为它设计一套超出这个系统实际需要的两阶段提交协议）。
6. **返回** `{approval_id, decision: "pending", kind, binding}`。`advise` 走到这一步就结束了，`gate` 永远到不了这一步（第 2 步必定先返回 `forbidden`）。

**`status`（查询）处理顺序（Codex 第 5 轮 High 2/3 订正后）**：
1. 能力检查（`granted.has(Approval)`，无 → `forbidden`）——**这一步 v5 漏掉了**（Codex 第 5 轮 High 3）：没有这一步，一个后来被撤销 `approval` 授权的模块仍能凭旧 `approval_id` 查到之前的审批内容。
2. 单条查询 `WHERE id = ? AND module = ?`（Codex 第 5 轮 Medium 5 指出：不要先按 `id` 单独取出整行、再在应用层比较 `module`，直接把 `module` 放进 SQL 条件里，让"跨模块"和"根本不存在"走同一条数据访问路径、产生同一个结果，不给时序或异常信息上的差异留漏洞）——查无（不管是真的不存在，还是属于别的模块）→ `ErrorKind::NotFound`（决策记录 2 新增的第二个变体，见下）。
3. 查到 → 直接返回 `{approval_id, decision, kind, binding}`（`Pending`/`Approved`/`Denied`/`TimedOut` 原样返回，不需要任何存活性检查——`approval_id` 本身就是这次查询的凭据）。

**`ErrorKind` 这一轮实际新增两个，不是一个**（Codex 第 5 轮 High 2 订正决策记录 2 的计数）：`TokenInvalid`（提交阶段）与 `NotFound`（`status` 查无，仓库现有闭集里没有能表达"记录不存在"的变体，`-32601 method_not_found` 是方法层面的，不能借用来表示数据层面的查无）。「现状」第 1 条、SPEC 闭集文案、`rpc.rs` 的 `ALL`/`as_str()`/精确断言测试都要同步两个变体，不是一个。

**这条决策直接回应"批准 A 执行 B 不可能"这类 SPEC 验收条目对 `gate` 的期待**：`gate` 的提交调用第 3 步必定先命中空闭集，`gate` 永远不会建出一条 `Pending` 记录，`status` 也就永远查不到一条 `kind: Gate` 的记录——这跟 v1-v4 的空闭集结论一致，只是这一版不再需要额外解释"gate 为什么不会进入等待路径"，因为异步模型下**没有等待路径**这个概念，`gate` 第 3 步之后要么已经 forbidden、要么理论上会建记录但本轮不会发生。

### 5. 周期性超时扫描——不是 per-row 定时任务，也不是"送达再收尾"

**v3/v4 反复被打回的根源，是围绕"决定了但还没交付"这个中间态设计恢复机制**（`PendingGuard`、`DELIVERY_GRACE`、per-row 定时任务、启动重建定时器……）。异步模型下这个中间态**不存在**——`decision` 只有 `Pending → {Approved|Denied|TimedOut}` 一次转换，没有后续的"交付"步骤要担心失败或卡住。所以这一版的收尾机制退化成一件很朴素的事：

**一个周期任务（比如每 10 秒跑一次，具体间隔留给实现阶段），一条 SQL**：

```sql
UPDATE module_approvals
SET decision = 'timed_out', decided_at = ?
WHERE decision = 'pending' AND expires_at < ?
```

- **不需要 `PendingGuard`、`oneshot`、per-row `tokio::spawn`**——没有内存态需要维护，纯粹是"数据库里有没有过期的 `pending` 行"这一个问题，周期任务自己独立存在，不依赖任何一次具体的 RPC 调用的生命周期。
- **天然容忍单次失败**：这次扫描因为存储暂时不可用而失败，下一次（10 秒后）自动重试，不需要专门设计重试/退避逻辑——这正是"周期扫描"相对"per-row 一次性定时器"的结构性优势（Codex 第 4 轮 High 2 指出 per-row 定时器一旦 panic/被取消就没有第二次机会，周期扫描没有这个问题）。
- **daemon 重启完全不用特殊处理**——没有内存态、没有"孤儿行"这个概念，daemon 重启后周期任务重新启动，下一次照常扫描全表，该过期的行该在下一次扫描里被判定，不需要专门的"启动清扫"逻辑（v3/v4 花了大量篇幅讨论的"启动扫描 vs 周期扫描 vs 重建定时器"，在异步模型下完全不需要区分，因为只有一种机制）。
- **决定 CAS 本身也要查 `expires_at`**（Codex 第 4 轮 High 3 指出 v4 的决定 CAS 没有检查过期，用户可以在周期任务还没跑到这一行之前批准一条已经过期的审批）：`UPDATE ... SET decision = ? WHERE id = ? AND decision = 'pending' AND expires_at >= ?`——跟周期任务的 `expires_at < ?` 互补，同一个注入时钟，不留谁赢的歧义（`>=`/`<` 互斥覆盖全部情况，不存在恰好相等时两边都不查的空隙）。
- **超时扫描要能逐条推事件，不只是拿一个受影响行数**（Codex 第 5 轮 Medium 2）：用 `UPDATE ... RETURNING id`（或等价的先 `SELECT id WHERE ...` 再 `UPDATE`，取决于 `agent24-store` 用的数据库驱动是否支持 `RETURNING`）拿到这一轮被判超时的每个 `id`，逐条推 `module-approval.resolved`；WS 推送是 best-effort（DB 写成功即认定这一行的状态已经确定，事件丢了不影响状态本身，客户端可以随时用 `status`/REST 对账，不需要 outbox 这类更重的机制）。`(decision, expires_at)` 上建一个部分索引（`WHERE decision = 'pending'`），避免全表扫描。
- **生产接线，具体到位置**（Codex 第 5 轮 High 5 指出 v5 只提到"周期任务"，没说它在哪启动、怎么停）：`agent24d/src/server.rs` 已经有 `CancellationToken`/`tokio::spawn` 起后台任务的既有模式（比如 scheduler 的 `tokio::spawn(scheduler.run(...))`，`server.rs:1011` 一带）——`ModuleApprovalBroker` 的超时扫描照这个模式起一个 `tokio::spawn`，拿 `Shutdown`/`CancellationToken` 的一个 child token，`tokio::select!` 在"到下一个扫描间隔"和"token 被取消"之间选择；循环体内部的每一步 `Store` 调用都要 `match`/`if let Err` 记录日志然后 `continue`，**不能用 `?` 或 `.unwrap()` 让单次失败直接终止整个循环**（否则一次瞬时的存储抖动就会让这个任务提前退出、之后再也没有任何东西收尾过期的行，这正是"周期扫描能自动重试"这条论证成立的前提，必须真的这样实现，不能只是这么打算）。

### 6. 进程内 `ApprovalRequester`：接受 manifest，不接受裸字符串；提交/查询都对外暴露

```rust
// agent24-protocol/src/types.rs（新增；ModuleApprovalKind/ModuleApprovalDecision
// 就是决策记录 3 里 ModuleApproval 用的那两个枚举，本节不重新定义）
pub struct ApprovalAnswer {
    pub approval_id: String,
    pub kind: ModuleApprovalKind,          // Codex 第 5 轮 High 4：不带 kind，调用方拿到一个
                                            // Approved 的 Advise 结果和未来 T7c 的 Gate 结果
                                            // 长得一模一样，容易被误当成"内核已经执行"引用
    pub binding: bool,                     // kind == Advise 时恒 false，SPEC §6.1 要求的字段
    pub decision: ModuleApprovalDecision,  // 提交时恒为 Pending；status 查询时是当前真实值
}

pub enum ApprovalRequestError {
    ActionNotInClosedSet, // 只有 Gate 会命中；wire 上映射成既有的 Forbidden（决策记录 4）
    NotFound,             // status 查询一个不存在/不属于自己的 approval_id；
                          // wire 上映射成决策记录 2 新增的 ErrorKind::NotFound
    BackendUnavailable(String), // 存储层失败——REST 边界映射成 503，wire 边界映射成
                                 // agent24-os-proto 现有的 internal error kind，watchdog
                                 // 场景不适用这个变体（决策记录 5 的周期任务没有"调用方"，
                                 // 失败只是等下一次扫描重试）
}

/// 唯一的摘要函数，wire handler 和 `PolicyApprovalBackend` 都只调用它（经由
/// `ModuleApprovalBroker::submit`），不各自实现一份（Codex 第 5 轮 High 6：v5
/// 引用了这个函数却没有定义它）。键先递归排序再序列化，保证同一个 payload
/// 不管 serde_json 内部用不用 preserve-order 的 Map 实现都算出同一个摘要。
pub fn approval_digest(payload: &serde_json::Value) -> String {
    let canonical = sort_object_keys_recursively(payload);
    let bytes = serde_json::to_vec(&canonical).expect("Value serialization cannot fail");
    format!("sha256:{}", hex::encode(Sha256::digest(&bytes)))
}
```

`agent24-protocol` 需要新增 `sha2`/`hex` 依赖（目前都没有）；`agent24-domain` 需要新增 `futures`/`futures-core`（`BoxFuture` 用到，`Cargo.toml` 目前没有这个依赖，Codex 第 5 轮 Medium 3 指出），或者把 trait 签名换成 `Pin<Box<dyn Future<Output = ...> + Send + 'a>>` 手写别名，避免额外依赖——两种都可以，实现阶段选一种就好。

// agent24-domain/src/lib.rs（新增，紧邻 EventBroadcast）
pub trait ApprovalBackend: Send + Sync {
    fn submit<'a>(&'a self, module: &'a str, kind: ModuleApprovalKind, action: String, target: Option<String>, payload: serde_json::Value)
        -> BoxFuture<'a, Result<ApprovalAnswer, ApprovalRequestError>>;
    fn status<'a>(&'a self, module: &'a str, approval_id: &'a str)
        -> BoxFuture<'a, Result<ApprovalAnswer, ApprovalRequestError>>;
}

pub struct ApprovalRequester {
    module: String, // 私有字段，从 manifest 捕获，不接受任意字符串
    backend: Arc<dyn ApprovalBackend>,
}

impl ApprovalRequester {
    /// 照抄 `EventSink::new(manifest, broadcast)`（`agent24-domain/src/lib.rs:960`）
    /// 的形状——接收已验证的 manifest，不是裸 `impl Into<String>`（Codex 第 4 轮
    /// Medium 2 指出上一版的构造函数签名没有兑现"照抄 EventSink::new"这句话）。
    #[must_use]
    pub fn new(manifest: &DomainOsManifest, backend: Arc<dyn ApprovalBackend>) -> Self {
        Self { module: manifest.name().to_owned(), backend }
    }

    pub async fn submit(&self, kind: ModuleApprovalKind, action: impl Into<String>, target: Option<String>, payload: serde_json::Value) -> Result<ApprovalAnswer, ApprovalRequestError> {
        self.backend.submit(&self.module, kind, action.into(), target, payload).await
    }

    pub async fn status(&self, approval_id: &str) -> Result<ApprovalAnswer, ApprovalRequestError> {
        self.backend.status(&self.module, approval_id).await
    }
}
```

```rust
// agent24d/src/domain.rs（新增，紧邻 struct HubBroadcast）
struct PolicyApprovalBackend(Arc<ModuleApprovalBroker>);
impl agent24_domain::ApprovalBackend for PolicyApprovalBackend {
    fn submit<'a>(&'a self, module: &'a str, kind: ModuleApprovalKind, action: String, target: Option<String>, payload: serde_json::Value)
        -> BoxFuture<'a, Result<ApprovalAnswer, ApprovalRequestError>> {
        Box::pin(async move {
            if kind == ModuleApprovalKind::Gate {
                check_closed_set(&action)?; // 跟 wire handler（决策记录 4）共享同一个函数
            }
            self.0.submit(module, kind, action, target, payload).await
        })
    }
    fn status<'a>(&'a self, module: &'a str, approval_id: &'a str) -> BoxFuture<'a, Result<ApprovalAnswer, ApprovalRequestError>> {
        Box::pin(self.0.status(module, approval_id))
    }
}
```

`KernelCtx` 新增 `fn approval(&self) -> Option<&ApprovalRequester>`——带默认实现 `{ None }`（不破坏既有实现者）。`domain.rs` 挂载点：`granted.has(Capability::Approval).then(|| ApprovalRequester::new(&manifest, Arc::new(PolicyApprovalBackend(broker.clone())) as Arc<dyn ApprovalBackend>))`，跟 `event_sink` 并列构造；`MemoryCtx`（`agent24d/src/os_memory.rs:703-711`）加一个 `approval: Option<ApprovalRequester>` 字段并覆盖 `fn approval()`。

### 7. REST + WS：单一决定维度之后，端点和事件都简单了一截

- **持久化**：新 migration，新表 `module_approvals`，字段对应决策记录 3；`decision` 单列 `CHECK`（`'pending'|'approved'|'denied'|'timed_out'`）；`decided_at` 加一条检查约束：`pending` 时为 `NULL`，其余时候非空（这一条约束替代了 v3/v4 想用复合 `(decision, delivery)` CHECK 解决、却始终定义不完整的问题——现在只有一个维度，单列约束足够）。
- **REST**（`agent24d/src/module_approvals.rs`，新文件，`/api/v1/module-approvals`）：`list_module_approvals`（可选 `?decision=` 过滤）、`get_module_approval`、`decide_module_approval`（`decision: approved|denied`，走决策记录 3 的决定 CAS，冲突 `409`，`BackendUnavailable` 映射成 `503`）。
- **WS**：`EventBody` 新增两个变体，事件名用连字符（`agent24-protocol/tests/fixtures_roundtrip.rs` 的 `event_wire_types_are_dotted_not_snake_case` 只禁下划线，连字符不受限）：`ModuleApprovalRequired(Box<ModuleApproval>)`（`#[serde(rename = "module-approval.required")]`，提交建记录时推）、`ModuleApprovalResolved { id, decision }`（`#[serde(rename = "module-approval.resolved")]`，**决定 CAS 或周期超时扫描成功的那一刻就推**——不再有 v3/v4 那种"等交付到终态才推"的额外等待，因为决定本身现在就是终态）。两者都要进 `wire_type()`、`protocol/events.schema.json`（`cargo run -p agent24-protocol --bin export-schema > protocol/events.schema.json`，注意重定向到文件）、`export-schema.rs` 里的 `FORCE_REQUIRED`（`ModuleApproval` 的 `target`/`decided_at` 这类"总在但可为 null"的字段）和 `DATE_TIME_FIELDS`（`created_at`/`decided_at`/`expires_at`）、`protocol/openapi.yaml`、之后 `pnpm gen:api`；`packages/contract-tests/src/schema.test.ts` 里保护 `FORCE_REQUIRED` 的负例测试也要为这些新字段各加一条缺失用例；`agent24-protocol/tests/fixtures_roundtrip.rs` 的 `REQUIRED_EVENT_FIXTURES` 和 `tests/fixtures/events/` 各加两个新 fixture。
- **SPEC 文档的 canonical 措辞要同步改**（Codex 第 4 轮 High 5 指出只改 `docs/agent/tasks.md` 不够）：`docs/specs/SPEC-ME3-OUT-OF-PROCESS.md:188/593/618` 几处仍然写着 `_a24/approval/request`（这是初稿里的方法名，从未真正实现过，本设计一直用的是 `gate`/`advise`/`status`）、仍然把 gate 执行验收算进 ME-3e 且说"不再阻塞"——这些要一并改成指向 T7b（`advise`+骨架）/T7c（gate 执行）的现状；`docs/specs/SPEC-002-protocol.md` 的 REQUEST 类事件表也要把 `module-approval.required` 加进去，不能只删掉"唯一"这个措辞而不更新表格本身。
- **明确不做**：`agent24-cli` TUI 扩展——本轮只保证 REST/WS 足够让一个愿意读私有 API 的客户端（哪怕是 `curl`）完整走一遍审批往返；TUI 的 `EventBody` 穷举 `match` 仍然要加忽略分支才能编译（现状第 1 条）。

## 判据（带正对照）

1. 模块清单未声明 `approval` 能力 → `_a24/approval/gate`/`advise`/`status` 三个方法调用都得到 `forbidden`，不建/不返回记录（Codex 第 5 轮 High 3：`status` 之前漏测了）。**正对照**：声明了的模块三者都能正常工作。
2a. params 带未知字段（不含 `_meta`）→ `-32602`，不建记录。
2b. params 带合法 `_meta`（哪怕里面夹带 `module`/`request_id`/`approval_token` 这类同名 key）→ 正常通过，且这些夹带的值不影响任何判定（Codex 第 5 轮 Low 1：拆成两条，原来一条判据里"未知字段拒绝"和"合法字段通过"写在一起自相矛盾）。
3. `approval_token` 重放：同一个令牌提交第二次 → `token_invalid`。**正对照**：第一次使用成功。
4. `{request_id, approval_token}` 错配（A 的 id 配 B 的 token，或反之）→ `token_invalid`。**正对照**：正确配对时通过。
5. 旧 generation 的令牌（模块重启后用旧一代的 `{request_id, approval_token}`）→ `token_invalid`。
6. 已结束请求的令牌（请求已 `finish()`）→ `token_invalid`。
7. **成对盗用（B 的 `request_id` + B 自己的 `approval_token`，被 A 的 handler 拿走冒用）：本轮明确不挡**，不写一条会通过的测试假装挡住了（照抄 SPEC 原话）。
8. `gate` 收到任何 `action`（闭集为空）→ `forbidden`，不建记录；`status` 永远查不到任何 `kind: Gate` 的记录（因为从未建出过）。进程内路径同样要测：`ApprovalRequester::submit(Gate, ...)` 直接返回 `Err(ActionNotInClosedSet)`，不经过 `ModuleApprovalBroker`。
9. `advise` 提交立即返回 `{approval_id, decision: "pending"}`，耗时应远小于任何既有超时（30 秒的 RPC/代理超时、300 秒的旧审批默认超时都不构成约束）——用一条计时测试断言提交调用是"数据库写入级别"的延迟，不是"等人"的延迟。
10. `status` 完整往返：提交 → WS 推 `module-approval.required` → REST `decide`（approve）→ 再次调用 `status`（可以是任意时间之后，不需要在同一个连接/同一次会话里）→ 返回 `{decision: "approved"}` → WS 推 `module-approval.resolved`。`deny` 同理。
11. `status` 查询一个不属于当前模块的 `approval_id`（另一个模块建的）→ 表现为"查无"，不泄漏"这个 id 其实存在，只是不是你的"这个事实。
12. `status` 查询一个从不存在的 `approval_id` → 同样表现为"查无"，跟判据 11 拿到完全相同的错误形状（不给枚举 id 的人提供区分依据）。
13. 同一条记录两个并发 `decide_module_approval`（一个 approve 一个 deny）→ 恰好一个成功，另一个 `409` 冲突，不是后写覆盖前写。
14. 周期扫描把一条超过 `expires_at` 还停在 `pending` 的记录判定成 `timed_out`——测试用可注入时钟推进时间、手动触发一次扫描，不依赖真实等待；`status` 查询这条记录返回 `timed_out`。
15. 决定 CAS 检查 `expires_at`：一条已经过期、但周期扫描还没来得及跑到的记录，`decide_module_approval` 直接判定冲突（不能在扫描任务和 REST 之间打时间差，让一条已过期的记录被人为批准）。
16. 单次周期扫描因为存储暂时不可用而失败 → 不影响任何内存态（本来就没有内存态），下一次扫描（正常间隔之后）照常重试并成功收尾——不需要退避或重试计数器，周期本身就是重试机制；**扫描循环本身在第一次 `Store` 错误之后必须还活着**（Codex 第 5 轮 High 5）：测试要证明连续多次故意让存储报错之后，循环仍在跑、一旦存储恢复立刻在下一个周期收尾，不是断言"下一次会重试"就完事，而是真的构造"循环还活着"这个事实。
16a. **提交的幂等重试**（Codex 第 5 轮 High 1，本轮最重要的新增判据）：用同一个 `{request_id, approval_token, action, target, payload}` 提交两次（模拟第一次的响应丢失、模块重试）→ 第二次拿到跟第一次完全相同的 `{approval_id, decision, kind, binding}`，**不重新核验令牌、不产生第二条记录**；提交时故意让存储层在插入那一步失败 → 返回 `BackendUnavailable`，且这次提交对应的 `request_id` 上的令牌确实作废（不能再用同一个令牌重试成功——这是本设计承认的残余代价，判据要如实测出"作废"这个事实，不是测出"能重试成功"）。
16b. `gate` 调用先查闭集（必定 forbidden）**不消耗当次请求的令牌**——紧接着用同一个 `{request_id, approval_token}` 提交 `advise` 仍然成功（Codex 第 5 轮 High 1 指出的顺序问题：先核销令牌再查闭集会让这条判据失败）。
17. `Offer`/`MethodsFor`/`MountReport.granted` 三方一致性（复用 T7a 判据 10 的模式，换成 `Capability::Approval`）：被授予的模块三者一致地「有」，未被授予的一致地「无」；只被授予 `Approval`（不被授予 `Events`）的模块，`Offer.provides` 必须包含 `_a24/approval/` 前缀。
18. 进程内 `ApprovalRequester`：未授予 `Approval` 的模块 `KernelCtx::approval()` 返回 `None`；授予了返回 `Some`，`submit`/`status` 各自一次真实往返（含 REST `decide` 触发状态变化，`status` 能读到）。
18a. `ApprovalAnswer.kind`/`binding` 如实反映记录本身（Codex 第 5 轮 High 4）：一条 `Advise` 记录被批准后，`submit`/`status` 返回的 `kind` 恒为 `Advise`、`binding` 恒为 `false`——用一条测试断言调用方能够、且必须靠这两个字段区分"这是不是一个可以当成内核已执行的结果"，不能只看 `decision == Approved` 就断定。
19. 秘密不进入任何可观测通道：`approval_token` 不出现在错误消息、`tracing` 日志、`Debug` 输出、持久化记录、REST/WS 响应体里——覆盖两个明文出现的位置：转发给模块的 HTTP 请求头，以及 `gate`/`advise` params 里的 `approval_token` 字段（含反序列化失败时的错误消息，不能把整个 params 原样打印出来）。`approval_id` 同样不应该出现在跟它无关的日志/错误里（虽然它不控制准入判定，但知道它就能查到审批内容，按需要保密的信息处理）。
20. `payload`/`payload_digest` 只在提交那一刻从当时的调用参数计算一次（内核算摘要，不是模块自报），不接受调用方后续传入的任何"更新"值；REST `get_module_approval` 返回的记录里 `payload` 字段是内核收到的真实值，可供人工审阅，不是只有一个哈希。
21. `RESERVED_KERNEL_SEGMENTS` 真的挡住了一个叫 `module-approvals` 的模块名。
22. schema/codegen 零漂移：CI 已有的「生成物和源码一致」检查（`protocol/events.schema.json`、`pnpm gen:api` 之后 `git status` 干净）加完两个新事件变体和 REST 端点之后依然通过；`FORCE_REQUIRED`/`DATE_TIME_FIELDS` 的负例测试覆盖新字段。

## 不改的东西

- `agent24-policy::ApprovalBroker`/`ApprovalRequest`/`Approval`（run_id 必填、同步阻塞等待的那一整套）——模块审批是平行的新类型/新表，用完全不同的异步提交+轮询模型，不复用它的阻塞等待机制。
- `agent24-agent/src/resume.rs::assess_restore`——不调用，只借用其原则，留给 T7c。
- `agent24-cli` TUI 的 `Approvals` 屏幕——本轮不扩展。
- `packages/wechat-bridge`/Nostr 桥——本轮不碰。
- 「成对盗用 `{request_id, approval_token}`」——如实记录为已知边界，不写伪装通过的测试。
- `Generation`/`drain.rs` 的状态转换逻辑本身（`ready`/`begin_drain`/`revoke`）——只扩展 `in_flight` 的值类型、新增 `admit_approval_callback` 一个方法，不改变现有转换；`status` 查询完全不碰这个状态机。
- `gate` 的真实执行、内核可执行动作闭集的任何非空内容——留给 T7c，随 T7b 合并同步写进 `docs/agent/tasks.md`。

---

## v1 → v4 历史（同步/阻塞模型下的迭代，已被 v5 的架构变更取代，保留作记录）

- **v1 → v2**（Codex 第 1 轮，0 Critical / 7 High / 4 Medium / 1 Low）：范围收窄为「advise 完整 + gate 协议骨架」；纠正「用 `admit_callback` 做存活性重查」这个误用；拆出决定/交付两个维度；`ApprovalRequester` 定为 trait-in-domain 分层；给出 REST/WS 初版设计；补全 T7a 接线要点；判据 13→17 条。
- **v2 → v3**（Codex 第 2 轮，0 Critical / 8 High / 3 Medium / 0 Low）：闭集外动作改回 SPEC 原文的 `Forbidden`；`payload_digest` 改成内核从真实 payload 算，不采信自报；decision/delivery 给出合法组合表和三种 CAS；订正 `PendingGuard` 的真实行为（只清内存，不代持久化）；`ApprovalRequestError` 补上返回类型；REST/WS/schema 同步点补全；判据 17→21 条。
- **v3 → v4**（Codex 第 3 轮，0 Critical / 5 High / 3 Medium / 1 Low）：wire params 补上遗漏的 `approval_token`；合法状态改成逐条列出的表格并承认交付窗口有真实持续时间（`DELIVERY_GRACE`）；周期任务扩展到同时处理"决定了但没交付"的行；`ApprovalRequester::new()` 补公开构造函数；schema 同步点继续补全；判据扩到 23 条。
- **v4 评审**（Codex 第 4 轮，0 Critical / 6 High / 7 Medium / 2 Low，**结论：不能转入实现**）：指出"7 种合法状态"实际有 8 种、数据库约束没真正实现；**核心问题仍未解决**——决定 CAS 成功后，交付给等待中 RPC 调用这一步本身可能因为进程崩溃/任务丢失/存储故障而永久卡住，per-row 定时器架构不能保证"任意中断点最终收敛"；`_meta` 字段被 SPEC 强制要求但 v4 的 exact params 没给；`DELIVERY_GRACE`/`expires_at` 定义前后矛盾；**且发现了比这些都更根本的问题**：同步阻塞的 `advise` 调用活不过现有 30 秒 RPC/代理超时，人类不可能在这么短时间内做出决定——这个发现直接导致用户裁决把整个交互模型换成本文档 v5 的异步提交+轮询，上面列的所有"决定/交付窗口"类问题随架构变更一并消失，不再逐条修复。

---

## v5 → v6 改动（Codex 第 5 轮：0 Critical / 6 High / 6 Medium / 2 Low，全部采纳）

第 5 轮确认异步提交+轮询这个架构方向本身是对的、消除了旧模型的核心问题，剩下的都是局部的实现细节缺口，不再是架构级问题：

- High 1：提交处理顺序改成「能力 → 闭集（gate）→ 幂等去重（`(module,request_id,kind)` 唯一约束）→ 令牌核销 → 插入」，用 `request_id` 天然做幂等键，模块响应丢失后用同一个 `request_id` 重试能拿到同一条记录，不需要另外发明两阶段提交协议；如实承认插入失败时令牌确实作废、这次请求拿不到第二次机会。
- High 2：`ErrorKind` 新增第二个变体 `NotFound`（决策记录 2），`status` 查无用它，不再想着靠 `Forbidden`/`method_not_found` 硬凑。
- High 3：`status` 补上能力检查这一步（v5 遗漏）。
- High 4：`ApprovalAnswer`/`ModuleApproval` 加 `kind`/`binding` 字段，防止 Advise 结果被当成 Gate 结果引用。
- High 5：决策记录 5 补上扫描循环的具体接线（`CancellationToken`、`tokio::select!`、单次 `Store` 错误不终止循环）和 `RETURNING id` 逐条推事件的做法。
- High 6：决策记录 6 补上 `approval_digest()` 的实际定义（键排序 + `sha256:` 前缀）。
- Medium 1-6：REST 0 行结果区分 404/409、`now` 取样时机、`futures`/`sha2`/`hex` 依赖、`export-schema.rs`/OpenAPI ULID 假设等同步点、判据补齐（幂等重试、扫描循环存活、kind/binding 区分）、payload/规范化措辞收紧到"记录的是提交时刻收到的原样"而不是"模块必然照做"。
- Low：判据 2 拆成 2a/2b 消除自相矛盾；`_a24/approval/request` 那两处是仓库里跟本设计无关的通用占位符字符串（不是本设计要用的方法名，本身也没有恢复同步阻塞的含义），不需要跟着改。

**这一版之后不再送 Codex 第 6 轮评审**——五轮下来（原同步模型 4 轮 + 架构重设计后 1 轮），核心机制（令牌核验、幂等提交、单维状态机、周期扫描、trait 分层）已经被逐条核实过，剩下能发现的问题级别已经降到"实现时会被编译器/测试挡住"这一档，继续送审的边际收益不再值得再花一轮 token。转入实现，让 `cargo test`/pre-pr-check 去验证判据是否真的立得住。

---

（v6，设计冻结，进入实现；上一版 v1 的事件回调内容已在 T7a 交付；`gate` 的真实执行留给 T7c）
