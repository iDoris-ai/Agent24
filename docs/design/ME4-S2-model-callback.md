# ME4-S2 —— 推理回调 `_a24/model/*`（ME4-4.1.1 设计）

> **草稿 v1，待评审**（2026-09-23）。尚未经过任何一轮对抗评审。
> 评审方：按 `docs/agent/PLAN-ME4-OS-CAPABILITIES.md` §一 第 3 条——Codex 额度 2026-09-29 19:28 前耗尽，
> 期间由**全新上下文的 Opus 子代理**做对抗评审（Critical/High/Medium/Low + file:line），评审记录在此处逐轮追加，
> 并在 `docs/agent/followups.md` 的 `ME4-CODEX-DEBT` 追加一行，额度恢复后补 Codex 一轮。
>
> | 轮次 | 评审方 | 结论 | C / H / M / L |
> |---|---|---|---|
> | —（v1 未送审） | — | — | — |
>
> **与 ME4-1.1.1（调度回调设计，另一个 worktree 并行写）的 SPEC 改动需要合并**：两份设计都改
> `docs/specs/SPEC-ME3-OUT-OF-PROCESS.md` 的 §3 offer set 那一句、§3 方法表、§8 交付表（都在 ME-3g 行之后加一行）、
> §9「不做 `Models` / `Scheduler` / `Policy`」那一条。本分支对 SPEC **只加 model 相关的行/段**，不动 scheduler 的内容；
> 后合并的一方按「两边的增量都保留」解决文本冲突（冲突点清单见 §10.3）。
>
> 设计里每一段 Rust 签名/片段都在 scratch crate 里 `cargo check` / `cargo test` 过，命令与输出见附录 A。

## 版本改动记录

| 版本 | 日期 | 改动 |
|---|---|---|
| v1 | 2026-09-23 | 初稿。裁决 S2-1（`model_access` 字段）、S2-4（选 **(a) 按方法超时 + 统一桥接取消**）、S2-2（`max_tokens: Option<NonZeroU32>` / `model_id: Option<String>`）、S2-5（并发 2/全局 4、令牌桶 30/0.5s、`module_model_usage` 表与 `GET /api/v1/usage?module=`）、S2-6（新增 `unavailable` 一个 kind）、S2-3（`_a24/model/complete` 的确切 JSON 形状；不做 `_a24/model/list`）。同分支改 SPEC-ME3 的 Models 相关内容 |

---

## 0. 这份文档解决什么、不解决什么

**解决**（PLAN §二 S2 第 1–7 条，每条都是下限，本文只做得更严）：

| S2 条 | 本文位置 | 一句话结论 |
|---|---|---|
| S2-1 隐私 | §2 决策 M1 | manifest 新字段 `model_access: local_only \| remote_allowed`，缺省 `local_only`；隐私**只**来自挂载时的 manifest，params 里没有任何能改它的字段（`deny_unknown_fields` 让 `privacy`/`model`/`tools` 都是 `-32602`） |
| S2-4 超时与取消 | §3 决策 M2 | 选 **(a)**：`Handler` 加一个默认方法 `call_timeout()`，`_a24/model/complete` 声明 120s，连接级 30s 对其它方法不变；`$/cancelRequest` / 连接关闭 / 代次撤销 / 方法超时 / 所绑请求结束五条路**最终都是 drop handler future**，handler 里持有 provider `CancellationToken` 的 `DropGuard`，于是统一到达 provider |
| S2-2 契约扩展 | §4.1 决策 M3 | `CompletionRequest.max_tokens: Option<NonZeroU32>`（`None` 不发字段，`/api/v1/chat` 字节不变）、`CompletionResponse.model_id: Option<String>`（取 OpenAI `response.model`，不回填）、`ModelRouter::complete_served` 返回 tier |
| S2-3 方法最小集 | §4.2–§4.4 决策 M4 | 只有 `_a24/model/complete`，确切 params/result 见 §4.2/§4.3；不开 tools；**不做** `_a24/model/list` |
| S2-5 限流 + 计量 | §5 决策 M5、§6 决策 M7 | 每模块并发 2、全 daemon 4、每模块令牌桶（挂载级，跨 generation 不重置）；新表 `module_model_usage`，失败/取消都计、被挡在路由器之前的不计、`cost_usd` 恒 `null` |
| S2-6 错误 | §7 决策 M6 | 闭集加一个 `unavailable`（`data.retryable` 区分「稍后可能好」与「provider 拒了」），provider 名/URL/原文一律不出内核 |
| S2-7 测试 | §8 判据 | 进程内用 Rust 桩 + 手建 `ModelRouter`；黑盒用 Python 桩当 `OMLX_URL`，python3 缺失即失败；另留 `#[ignore]` 真 oMLX 冒烟 |

**不解决**（写进 SPEC §9，见 §11 残余风险）：tools / function calling；流式输出；模块自选模型；`_a24/model/list`；
远端 provider 的配置入口（今天唯一的「远端」来源是非回环的 `OMLX_URL`，§1 第 6 条）；价目表与费用计算；按日远端 token 预算；
embeddings。**进程内**模块的模型句柄（`KernelCtx` 没有，本文不加——`Models` 只进进程外授予表）。

---

## 1. 现状（2026-09-23 在 `73a9592` 上逐条重新核对，行号即该提交）

1. **模型契约**：`CompletionRequest { messages, model, tools, response_format }`，**没有 `max_tokens`**
   （`rust/crates/agent24-models/src/lib.rs:93-102`）；`CompletionResponse { message, usage }`，**没有实际模型 id**（`lib.rs:137-140`）。
   OpenAI 请求体只有 `model/messages/stream`（`lib.rs:493-497`）+ 可选 `tools`/`response_format`（`lib.rs:498-517`）；
   响应解析 `OaChatResponse { choices, usage }` **丢了 `model` 字段**（`lib.rs:318-321`）；`usage.cost_usd` 恒 `0.0`（`lib.rs:546,552`）。
   chat 超时 120s（`lib.rs:206`），connect 2s；响应体上限 8 MiB。
2. **错误**：`ModelError::{Unavailable(String), Provider(String), Cancelled}`（`lib.rs:143-152`）。`friendly_http_error`（`lib.rs:393`）把
   429/5xx 归 `Unavailable`、其余 4xx 归 `Provider`，**消息里带 provider 名与 provider 原文**（`format!("{provider}: {cause} (HTTP {code}: {d})")`）；
   `classify`（`lib.rs:473`）把 connect/timeout 归 `Unavailable`，其余 reqwest 错误原文进 `Provider`。——**原样外传就是泄露**。
3. **路由**：`ModelRouter::complete(TaskProfile{privacy, complexity}, req, cancel) -> (provider_name, resp)`（`router.rs:284-321`），
   **返回的是 provider 名，不是 tier、不是模型 id**。`Privacy::LocalOnly` 的 `tier_order` 只含 `Local/Lora`（`router.rs:78-87`），
   路由为空即 `Unavailable("no local provider available for a local-only task")`——这是 LocalOnly 的**唯一强制点**，本文复用、不另造。
4. **provider 的取消**：`OpenAiCompatProvider::complete` 在 `send()`、读响应体的每个 chunk 处都 `select!` 了 `cancel.cancelled()`
   （`lib.rs:518-529`、`read_json_capped`）；**另外**，丢掉 `complete` 的 future 本身就会让 hyper 关掉这条 HTTP/1 连接——
   这一点不是读代码能确定的，已在 scratch 里用真实 TCP 桩实测（附录 A `wire_tests::dropping_the_provider_future_closes_the_upstream_connection`）。
5. **/chat 与用量**：`post_chat` 用 `TaskProfile::default()`（`Privacy::Any + Simple`，`routes.rs:110`）；用量是**一个全局内存计数器**
   `UsageCounters`（`routes.rs:28-50`），`GET /api/v1/usage` 直接返回它（`routes.rs:52-54`，`server.rs:667`），重启清零，不分模块。
6. **daemon 里的 provider 集**：`ModelRouter::from_env()`（`router.rs:205-231`，`server.rs:909`）**恒为** `omlx`（`OMLX_URL`）+ `ollama`（写死 `127.0.0.1:11434`）两个；
   `env_local_tier` 把非回环 URL 标成 `Remote`（`router.rs:151-163`）。**daemon 今天没有任何「配一个远端 provider」的入口**——
   唯一会出现 `Tier::Remote` 的方式是把 `OMLX_URL` 指到非回环地址。所以 `remote_allowed` 今天的实际效果很小；它是为将来的远端配置留的授权位。
7. **回调通道的超时**：`CALL_TIMEOUT = 30s`（`agent24-os-proto/src/rpc.rs:82`），在 `Conn::on_frame` 的 `Dispatch::Call` 分支里
   **对每个方法无条件**套 `tokio::time::timeout(self.limits.call_timeout, handler.call(params))`（`rpc.rs:1264-1269`）；
   `Conn::finished` 的超时文案读的也是 `self.limits.call_timeout`（`rpc.rs:1284-1293`），`by_task` 只存 id（`rpc.rs:1228`）。
   `Handler` trait 只有 `check_params` / `call`（`rpc.rs:326-352`）。生产连接一律 `rpc::Limits::default()`（`supervisor.rs:946-954`）。
8. **回调通道的取消**：`$/cancelRequest` → `handle.abort()`（`rpc.rs:1239-1245`）；连接结束（含 `serve_until` 的 stop = `generation.revoked()`，
   `supervisor.rs:946-954`）→ `conn.handlers.shutdown().await`（`rpc.rs:1187`）。**三条路都是 drop handler future**，没有一条会碰到 handler 自己持有的 token。
9. **生命周期绑定**：`Generation::admit_callback_bound(request_id)`（`drain.rs:487-510`）一次加锁给出准入 + 生命周期；
   `Running` 下**未知的 `request_id` 返回 `Ok(None)`**（`drain.rs:501`）——memory handler 拿到 `None` 照样无绑定执行（`memory_callback.rs:127-136`）。
   `bind_to_lifecycle(None, work)` 就是 `work.await`，**不提供任何取消**（`drain.rs:750-783`）；有生命周期时 `select!` 剩余预算与 `ended()`。
   被代理请求的预算是代理总时限 `UPSTREAM_DEADLINE = 30s`（`proxy.rs:116`，经 `admit_request` 的 `budget`，`drain.rs:416-447`）。
10. **授予与挂载**：`KERNEL_OOP_GRANTS = [Events, Approval, Memory]`（`agentd domain.rs:93-94`）；`mount_package`（`domain.rs:1275`）
    算一次 `Grants::granting`（`domain.rs:1364`）、按「真的持有」拼 `provides`（`domain.rs:1413-1423`）、在 `MethodsFor` 闭包里无条件注册方法
    （`domain.rs:1424-1537`）；**挂载级**状态（memory 的 limiter/admission）在闭包**外**建、闭包内只 clone（`domain.rs:1424-1453`、`os_memory.rs:597-615`），
    events 的 limiter 在闭包**内**每代重建（`domain.rs:1454-1462`）。`mount_package` 今天拿不到 `ModelRouter`（参数表 `domain.rs:1275-1286`）。
11. **manifest**：`RawManifest` 顶层 `deny_unknown_fields`（`agent24-domain/src/lib.rs:355-395`），capability 以字符串收再 `Capability::parse`
    （`lib.rs:741-744`），`Capability::Models` 早就在（`lib.rs:148`）。`requires_models` 只用于挂载时的资源检查（`agentd domain.rs:440-459,1359`），与授予无关。
12. **错误闭集**：`ErrorKind::ALL` 17 个（`rpc.rs:155-173`）；测试 `the_error_kinds_are_exactly_specs_closed_set`（`rpc.rs:1937-1953`）
    用一份**SPEC 句子的拷贝**钉住集合——加 kind 必须同时改 `ALL`、`as_str`、这份拷贝与 SPEC §3 原句。
13. **params 预算**：`dispatch()` 对所有方法的 params 先做 5 000 节点 / 32 层 / 256 KiB 字符串字节的预算（`rpc.rs:404-412`），
    所以一次 `complete` 的 prompt 总量天然 ≤ 256 KiB，不需要再另立字节上限。
14. **FU-70**（`docs/agent/followups.md:91`）：approval 回调没接 `bind_to_lifecycle`。本文的 handler 必须用 `admit_callback_bound` + `bind_to_lifecycle`，不复制它。
15. **store**：`agent24-store` 的迁移到 `0006`（`migrations/`），池 5 连接（`agent24-store/src/lib.rs:59`）。ME4-1.2.1（调度）会占下一个号。

---

## 2. 决策 M1（S2-1）：隐私由 manifest 决定，调用只能表达 complexity

### 2.1 manifest 字段

- 字段名 **`model_access`**，取值 **`local_only` | `remote_allowed`**，**缺省 `local_only`**（YAML 写 `~`/`null` 同缺省）。
- 解析位置：`agent24-domain` 的 `RawManifest` 加 `#[serde(default)] model_access: Option<String>`，与 `kernel_capabilities` 同一理由收**字符串**再映射
  （错误要能引用作者写错的原字；serde 的 variant 错误做不到）。校验在 `DomainOsManifest::from_yaml` 里 capability 映射之后：
  - 非法取值 → `DomainError::Manifest("model_access: \"remote\" is not one of local_only, remote_allowed")`；
  - **出现该字段但 `kernel_capabilities` 没请求 `models` → 拒**（与 `impl_kind`/`spawn` 双向一致同一个理由：否则 manifest 读起来像「这个模块会外发」，实际它一个模型都够不着）；
  - 字段名拼错（`model_acess`）由既有的顶层 `deny_unknown_fields` 拒。
- `MANIFEST_SCHEMA_VERSION` 不变（字段可选、缺省最严）。老 daemon + 带 `model_access` 的新 manifest 在**解析期**失败（门 6 的既有语义）；新 daemon + 老 manifest = `local_only`。

```rust
// agent24-domain/src/lib.rs（scratch 已 check + test）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAccess {
    #[default]
    LocalOnly,
    RemoteAllowed,
}
impl ModelAccess {
    pub const ALL: &'static [ModelAccess] = &[ModelAccess::LocalOnly, ModelAccess::RemoteAllowed];
    pub fn as_str(self) -> &'static str { /* "local_only" | "remote_allowed" */ }
    pub fn parse(s: &str) -> std::result::Result<Self, String>;
}
// RawManifest:        #[serde(default)] model_access: Option<String>,
// DomainOsManifest:   model_access: ModelAccess,
impl DomainOsManifest {
    pub fn model_access(&self) -> ModelAccess;
}
```

### 2.2 强制在哪一层

| 层 | 做什么 | 为什么是这里 |
|---|---|---|
| wire（`ModelCompleteParams`，§4.2） | `deny_unknown_fields`，**没有** `privacy`/`model`/`tools`/`provider`/`tier` 字段；`_meta` 宽容但**永不被读** | 能改隐私的字段根本不存在，不是「存在但被忽略」 |
| 挂载（`ModelGrant::new`，§5.1） | `privacy = match manifest.model_access() { RemoteAllowed => Privacy::Any, LocalOnly => Privacy::LocalOnly }`，一次算好存进挂载级 grant | 隐私的**唯一来源**；handler 从 grant 取，不从 params 取 |
| 路由（`ModelRouter::route` / `tier_order`，既有） | `LocalOnly` 的层序只有 `Local/Lora`；空路由即 `Unavailable`，**不碰远端 provider** | 既有、已测的唯一强制点（§1 第 3 条）；本文不另造第二个会漂移的判断 |
| 事后绊线（handler，§4.4） | `privacy == LocalOnly && !served.tier.is_local()` → 记 `error` 日志、结果**不返回**、回 `-32603` | **只是检测，不是防护**——字节已经发出去了；它把「路由器不变量被破坏」从静默变成响亮 |

`remote_allowed` 的调用以 `Privacy::Any` 路由，模块只能给 `complexity: simple | complex`（缺省 `simple`）：`Simple` 本地优先、`Complex` 远端优先（`router.rs:78-87`）。
选哪个 provider、用哪个模型，**全由内核**。

### 2.3 授予与挂载接线

- `KERNEL_OOP_GRANTS` 加 `Capability::Models`；**进程内 `KERNEL_GRANTS` 不加**（`KernelCtx` 没有模型句柄——授予没有句柄的能力就是撒谎，SPEC §3）。
- `mount_all`/`mount_package` 多一个参数 `models: Option<&ModelCallbackDeps>`（daemon 级依赖，§5.1）。`ModelGrant` 在 `MethodsFor` **闭包外**建一次：

```rust
// scratch check/src/mount_sketch.rs（已 check）
pub fn model_grant(name: &str, manifest: &DomainOsManifest, granted: &Grants,
                   deps: Option<&ModelCallbackDeps>) -> Option<ModelGrant> {
    match (granted.has(Capability::Models), deps) {
        (true, Some(deps)) => Some(ModelGrant::new(name.to_owned(), manifest.model_access(), deps.clone())),
        _ => None, // 没真的持有 → 不报 granted、不进 provides（invariant #134，同 memory）
    }
}
// provides：if grant.is_some() { provides.push("_a24/model/".to_owned()) }
// granted_names：filter(|c| c != "models" || grant.is_some())
// MethodsFor 闭包内，无条件注册：
methods.with("_a24/model/complete",
    Arc::new(ModelCompleteHandler { generation: generation.clone(), grant: grant.clone() }))
```

- **offer set 阶梯**（SPEC §8「生产的 offer set 只包含当前已有 handler 的能力」）：把 `Models` 加进 `KERNEL_OOP_GRANTS`、`provides` 加 `_a24/model/`、
  注册 handler **必须在同一个 PR 落地**。PLAN §三 现在把「`KERNEL_OOP_GRANTS` 加 `Models`、`provides` 加 `_a24/model/`」放在 ME4-4.2.1、handler 放在 4.2.2b——
  **照原切法会出现「已授予但方法 not found」的中间态**。本文要求改切法（§10.2），开工前先改 PLAN §三 与台账。
- `MountReport` 对 `models` 的报告同 memory：只在真的持有 grant 时出现在 `granted`。`model_access` 是否在 `/api/v1/os` 显示见 §11 残余风险 R5。

---

## 3. 决策 M2（S2-4）：选 (a) —— 按方法的超时元数据 + 统一桥接取消

### 3.1 为什么是 (a) 不是 (b)

(b) 作业化（`start` → `poll`/`cancel`）要新增：作业表（内存还是持久化？daemon 重启后作业怎么办）、作业 id 的所有权与跨 generation 可见性、
孤儿作业回收、`poll` 本身的限流、结果保留时长与大小上限——每一项都是一个新状态机，而 S2 的真实消费者（Sin90 M5 的 classify/summarize/propose）
是**单次、秒级到几十秒**的补全。(a) 只动 `rpc.rs` 里三处（§3.2），复用既有的 drop 语义与 `bind_to_lifecycle`，不引入任何新状态。
代价如实写：(a) 下一次调用最长占着一个回调槽 120s（每连接 64 槽，§5 又把模型调用压到每模块 2 个），且**结果不能在连接断开后取回**——
Sin90 若需要「提交后离开、稍后取结果」，那是它自己 outbox 的事（它本来就有），不是内核的。

**不调大全局 `CALL_TIMEOUT`**（S2-4 明文禁止）：连接级 30s 对 events/approval/memory 一字不变。

### 3.2 `rpc.rs` 要改的，精确到函数（scratch 里改过真实 `rpc.rs` 的拷贝，既有 51 个 rpc 测试 + 新增 2 个全绿）

1. **`trait Handler`**（`rpc.rs:326`）加一个**带默认实现**的方法——既有 handler 与测试 handler 零改动：

```rust
/// This method's own budget, replacing `Limits::call_timeout` for its calls.
/// `None` (the default) keeps the connection-level value. Clamped to
/// `MAX_METHOD_CALL_TIMEOUT`. Asked once per call, before the task is spawned.
fn call_timeout(&self) -> Option<Duration> {
    None
}
```

2. **新常量** `pub const MAX_METHOD_CALL_TIMEOUT: Duration = Duration::from_secs(300);`（紧跟 `CALL_TIMEOUT`，`rpc.rs:82` 之后）。
   任何方法都不能把一次调用变成实际无界。
3. **新私有纯函数**（为了不等 300s 就能单测夹紧）：

```rust
fn effective_call_timeout(declared: Option<Duration>, default: Duration) -> Duration {
    declared.map_or(default, |t| t.min(MAX_METHOD_CALL_TIMEOUT))
}
```

4. **`Conn::on_frame`** 的 `Dispatch::Call` 分支（`rpc.rs:1264`）：
   `let timeout = self.limits.call_timeout;` → `let timeout = effective_call_timeout(handler.call_timeout(), self.limits.call_timeout);`，
   并 `self.by_task.insert(handle.id(), (id.clone(), timeout));`。
5. **`struct Conn`** 的 `by_task: HashMap<tokio::task::Id, String>`（`rpc.rs:1228`）→ `HashMap<tokio::task::Id, (String, Duration)>`。
6. **`Conn::finished`**（`rpc.rs:1278-1313`）：超时文案改读该任务自己的 `timeout`，不再读 `self.limits.call_timeout`
   （SPEC §2.1「上限与时限的文案：一律报当时实际生效的那个值」）。

`Limits` 结构体**不加字段**（加字段会让所有 `Limits { .. }` 字面量改动；上限用常量足够）。`dispatch()`、`Dispatch`、`serve`/`serve_until` 签名都不变。

### 3.3 五条取消路径如何到达 provider 的 `CancellationToken`

handler 里（§4.4 完整代码）：

```rust
let cancel = grant.deps.shutdown.child_token();      // 父 = daemon 停机 token
let _cancel_on_drop = cancel.clone().drop_guard();   // 本 future 被 drop → cancel()
bind_to_lifecycle(lifecycle, grant.deps.router.complete_served(profile, &request, &cancel)).await
```

| 触发 | 机制（既有，行号见 §1） | 到达 provider 的方式 | 模块收到 |
|---|---|---|---|
| `$/cancelRequest` | `handle.abort()` → 任务被取消 → handler future drop | drop 带走 provider future（hyper 关连接）+ `DropGuard` 取消 token | `cancelled`（由 `serve` 生成） |
| 回调连接关闭 | `serve` 退出 → `handlers.shutdown().await` | 同上 | 无响应（连接没了，SPEC §3） |
| 代次撤销 | `serve_until` 的 stop = `generation.revoked()` → 同上 | 同上 | 无响应 |
| 方法超时 120s | `tokio::time::timeout` 到点 → drop 内层 future | 同上 | `timeout`，文案报 `120000ms` |
| 所绑请求结束 / 预算耗尽 | `bind_to_lifecycle` 的 `select!` 另一臂胜出 → drop `work`（即 router future） | drop 带走 provider future；handler 随后返回，`DropGuard` 取消 token | `timeout`（`RequestEnded` / `BudgetExhausted` 两条文案，同 memory） |
| daemon 停机 | 父 token 取消 → provider 在 `select!` 处返回 `ModelError::Cancelled` | token 直达 | `cancelled`（「the daemon is shutting down」） |

**如实写 token 与 drop 的分工**：前五条里真正打断 HTTP 请求的是 **drop**（已实测，§1 第 4 条）；`DropGuard` 保证的是「凡是从这个 token 派生、活在 future 之外的东西也会停」——
今天 provider 不派生任何东西，所以它是防御性的，将来 provider 若加流式/后台读，不需要再改 handler。只有「daemon 停机」一条是**只靠 token** 到达的。

### 3.4 有没有 `request_id`，生命周期怎么绑

| 调用形态 | 准入（`admit_callback_bound`，同 memory 的表） | 约束它的东西 |
|---|---|---|
| 带 `request_id`，该请求**在途** | Running / Draining 都放行 | 方法超时 120s、**该请求剩余预算**（被代理请求 ≤ 30s；调度 fired 投递的请求 ≤ ME4-S1 定的投递超时，建议 10s）、请求结束、`$/cancelRequest`、连接、撤销、停机 |
| 带 `request_id`，该 id **不在途**（从未存在或已结束） | **本文比 memory 更严**：Running 下也**拒**，`timeout`「request_id is not (or no longer) in flight; send no request_id for background work」；Draining 下照既有表回 `draining` | ——（不会跑） |
| **不带** `request_id`（后台任务、定时任务） | Running 放行；Starting → `not_ready`，Draining → `draining`，Revoked → `revoked` | 方法超时 120s、`$/cancelRequest`、回调连接关闭、**代次撤销**（在途的无绑定调用在 drain 结束、代次被撤销的那一刻被 drop）、daemon 停机 |

为什么「不在途的 id」要拒而不是降级：memory 那样降级（§1 第 9 条），一个模块在请求刚结束时带着它的 id 发起的补全，会**静默**变成一个能跑 120s 的无绑定调用——
模块以为「请求没了它就停」，实际不停，而且 Running 时测不出来。拒掉之后，想要后台语义的模块必须**显式**不带 `request_id`。

**对调用方的直接后果（写进 SPEC 方法表与 wire 文档）**：在一次被代理请求里内联推理，受 30s 代理总时限约束；在 scheduler fired 投递里内联推理，受投递超时约束。
推理可能超过这些预算的模块，应当先应答请求/fired、再以**不带 `request_id`** 的后台调用去做（Sin90 的 outbox 本来就是这个形状）。

---

## 4. 决策 M3 + M4（S2-2 / S2-3）：模型契约扩展与方法形状

### 4.1 `agent24-models` 的扩展（ME4-4.2.2a；scratch 已改真实 crate 的拷贝，既有 28 个测试全绿 + 新增 2 个 wire 测试）

```rust
pub struct CompletionRequest {
    pub messages: Vec<Msg>,
    pub model: Option<String>,
    pub tools: Vec<ToolSpec>,
    pub response_format: Option<ResponseFormat>,
    /// None = 不发该字段（/api/v1/chat 字节不变）。下限 ≥1 是类型事实；
    /// 上限是调用方的策略（模块调用 ≤ 4096，§4.2），本 crate 原样转发、从不悄悄夹紧。
    pub max_tokens: Option<std::num::NonZeroU32>,
}
pub struct CompletionResponse {
    pub message: Msg,
    pub usage: Usage,
    /// provider 报告的实际模型 id（OpenAI `response.model`）；没报就是 None——
    /// 不用请求里的名字、也不用 provider 名回填，那等于把没人观察到的事当事实写。
    pub model_id: Option<String>,
}
// OaChatResponse 加 #[serde(default)] model: Option<String>；空串视同 None。
// complete(): if let Some(n) = req.max_tokens { body["max_tokens"] = Value::from(n.get()); }

// router.rs
#[derive(Debug, Clone)]
pub struct Served { pub provider: String, pub tier: Tier, pub response: CompletionResponse }
impl ModelRouter {
    pub async fn complete_served(&self, profile: TaskProfile, req: &CompletionRequest,
                                 cancel: &CancellationToken) -> Result<Served, ModelError>;
    /// 签名与行为不变，改为 complete_served 的投影。
    pub async fn complete(&self, profile: TaskProfile, req: &CompletionRequest,
                          cancel: &CancellationToken) -> Result<(String, CompletionResponse), ModelError> {
        self.complete_served(profile, req, cancel).await.map(|s| (s.provider, s.response))
    }
}
```

- 类型选择：`max_tokens` 用 `NonZeroU32`——0 在 OpenAI 语义里无意义，让它**不可表示**；`u32` 足够（没有 provider 接受 >2^32）。
  `model_id` 用 `Option<String>`——很多 OpenAI 兼容服务确实省略它，`String` 就得造一个假值。
- **对 `/api/v1/chat` 零变化**的论证：`post_chat` 构造 `max_tokens: None`（请求体不出现该键）；`model_id` 只是多解析一个 `#[serde(default)]` 字段，
  `ChatResponse` 不含它；`complete` 的签名、路由顺序、冷却、错误全部经 `complete_served` 原样透出。判据 J4 用「/chat 发给桩的请求体键集合」前后逐一相等来钉。
- 工作区内要补字段的结构体字面量：`CompletionRequest` 7 处、`CompletionResponse` 8 处（`agent24-agent`、`agent24-policy`、`agent24-models`、`agentd routes.rs`，含测试桩；`grep -rn 'CompletionRe\(quest\|sponse\) {$'` 去掉定义与返回类型行），机械改动。
  **外部仓库**（Sin90 若仍以 git 依赖构造这两个结构体）需同步——见 §11 R7。

### 4.2 `_a24/model/complete` 的 params（确切形状）

```json
{
  "messages": [ { "role": "system" | "user" | "assistant", "content": "…" } ],   // 1..=64 条
  "response_format": { "type": "json_schema",
                       "json_schema": { "name": "…", "schema": { … }, "strict": true } },  // 可选
  "max_tokens": 512,               // 可选，1..=4096，缺省 1024
  "complexity": "simple" | "complex",   // 可选，缺省 simple；local_only 模块下无效果
  "request_id": "req_…",           // 可选，§3.4
  "_meta": { … }                   // 可选，宽容，永不被读
}
```

```rust
// scratch check/src/model_callback.rs（已 check + test）
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelCompleteParams {
    messages: Vec<WireMessage>,
    #[serde(default)] response_format: Option<WireResponseFormat>,
    #[serde(default)] max_tokens: Option<u32>,
    #[serde(default)] complexity: Option<WireComplexity>,
    #[serde(default)] request_id: Option<String>,
    #[serde(default)] _meta: Option<Map<String, Value>>,
}
#[derive(Debug, Deserialize)] #[serde(deny_unknown_fields)]
struct WireMessage { role: WireRole, content: String }
#[derive(Debug, Clone, Copy, Deserialize)] #[serde(rename_all = "snake_case")]
enum WireRole { System, User, Assistant }                 // "tool" → -32602
#[derive(Debug, Clone, Copy, Deserialize)] #[serde(rename_all = "snake_case")]
enum WireComplexity { Simple, Complex }
#[derive(Debug, Deserialize)] #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WireResponseFormat { JsonSchema { json_schema: WireJsonSchema } }   // "json_object" → -32602
#[derive(Debug, Deserialize)] #[serde(deny_unknown_fields)]
struct WireJsonSchema { name: String, schema: Map<String, Value>, #[serde(default)] strict: bool }
```

- 语义校验（`validate`，在 `check_params` 里跑，违反即 `-32602`、handler 不执行）：消息 1..=64 条；`max_tokens` 1..=4096，文案带范围
  （「max_tokens must be between 1 and 4096」——不能过度脱敏，同 T8.5c-W-wire 判据 10）；`json_schema.name` 非空；`schema` 必须是对象（类型保证）。
- **不存在的字段**：`privacy`、`model`、`tools`、`tool_calls`、`provider`、`tier`、`temperature`… 一律 `-32602`。`temperature` 等采样参数本轮不开（最小集），
  将来加是**加字段**，老模块不受影响。
- 字节上限不另立：`dispatch()` 的 256 KiB 字符串预算已先于解析生效（§1 第 13 条）。
- `into_request`：`model: None`、`tools: vec![]`、`max_tokens: NonZeroU32::new(max_tokens.unwrap_or(1024))`。

### 4.3 result（确切形状）

```json
{ "text": "…", "model_id": "Qwen3-8B-4bit" | null, "tier": "local" | "remote",
  "usage": { "prompt_tokens": 12, "completion_tokens": 34 } }
```

`text` = 助手消息的 `content`（没有则 `""`；不开 tools，所以不返回 `tool_calls`）。`tier` 是本文在 PLAN 最小集上**加**的一个字段：
`remote_allowed` 的模块据此知道这次数据是否离开了设备（它可以记进自己的审计），且它只暴露层级，不暴露 provider 名/URL。
`Lora` 计为 `local`。`usage.total_tokens` 不给（冗余）。

### 4.4 handler（完整，scratch 已 check + test）

```rust
pub struct ModelCompleteHandler {
    pub generation: Arc<Generation>,
    pub grant: Option<ModelGrant>,          // None → forbidden；方法仍无条件注册
}

impl Handler for ModelCompleteHandler {
    fn check_params(&self, params: &Value) -> Result<(), String> {
        ModelCompleteParams::parse(params.clone()).map(|_| ())
    }
    fn call_timeout(&self) -> Option<Duration> { Some(MODEL_CALL_TIMEOUT) }   // 120s
    fn call(&self, params: Value) -> CallFuture {
        let parsed = ModelCompleteParams::parse(params);
        let grant = self.grant.clone();
        let generation = self.generation.clone();
        Box::pin(async move {
            let parsed = parsed.map_err(|e| RpcError::internal(format!(
                "params valid at check_params but not at call(): {e}")))?;
            let Some(grant) = grant else { return Err(forbidden()) };
            let (request, complexity, request_id) = parsed.into_request();
            // 一次加锁：准入 + 所绑请求的生命周期（W4b）。不复制 FU-70。
            let lifecycle = generation.admit_callback_bound(request_id.as_deref())
                .map_err(refused_error)?;
            if request_id.is_some() && lifecycle.is_none() {          // §3.4，比 memory 严
                return Err(RpcError::application(ErrorKind::Timeout,
                    "request_id is not (or no longer) in flight; send no request_id for background work"));
            }
            // §5：先占并发（不排队 → busy），再扣令牌——busy 不花令牌。
            let Ok(_module_permit) = grant.module_admission.clone().try_acquire_owned() else { return Err(busy()) };
            let Ok(_global_permit) = grant.deps.global_admission.clone().try_acquire_owned() else { return Err(busy()) };
            if !grant.limiter.try_acquire() {
                return Err(RpcError::application(ErrorKind::RateLimited, "model call rate limit reached"));
            }
            let profile = TaskProfile { privacy: grant.privacy, complexity };   // 隐私只来自 grant
            let cancel = grant.deps.shutdown.child_token();
            let _cancel_on_drop = cancel.clone().drop_guard();                  // §3.3
            let ticket = UsageTicket::new(grant.deps.usage.clone(), grant.module.clone());  // §6
            let served = match bind_to_lifecycle(lifecycle,
                    grant.deps.router.complete_served(profile, &request, &cancel)).await {
                Err(lt) => return Err(lifecycle_error(lt)),                     // ticket drop → Cancelled
                Ok(Err(e)) => {
                    ticket.finish(match e { ModelError::Cancelled => UsageOutcome::Cancelled,
                                            _ => UsageOutcome::Failed });
                    return Err(map_model_error(&grant.module, &e));              // §7
                }
                Ok(Ok(served)) => served,
            };
            if grant.privacy == Privacy::LocalOnly && !served.tier.is_local() { // §2.2 绊线
                tracing::error!(module = %grant.module, provider = %served.provider,
                    "LocalOnly model call was served by a non-local tier — router invariant broken");
                ticket.finish(UsageOutcome::Failed);
                return Err(RpcError::internal("the kernel routed this call incorrectly; the result is withheld"));
            }
            // …ticket.finish(Ok{served_by, tokens})；序列化 §4.3 的 result
        })
    }
}
```

顺序的理由：`forbidden` 先于准入（同 memory）；准入先于占用资源（Draining 的后台调用不应占槽）；
并发先于令牌（被 `busy` 挡住的调用不该花掉速率额度）；`UsageTicket` 在确定要进路由器时才建（被挡在前面的调用不计量，§6.2）。

### 4.5 不做 `_a24/model/list`

模块选不了模型（§4.2 没有 `model` 字段），列表对它没有行动价值；列表会把 daemon 的 provider 配置（有没有远端、哪些模型）暴露给模块，
而 `local_only` 模块本不该知道远端存在；挂载时的「要求的模型在不在」已经有 `requires_models` + `check_resources`（§1 第 11 条）。
将来需要时再加是**加方法**，`_a24/model/` 前缀已占好。

---

## 5. 决策 M5（S2-5 前半）：并发与令牌桶

### 5.1 三层对象

```rust
/// daemon 级：serve() 里建一次，传给 mount_all。
#[derive(Clone)]
pub struct ModelCallbackDeps {
    pub router: Arc<ModelRouter>,            // 就是 AppState.router 那一个（共享健康/冷却）
    pub usage: UsageRecorder,                // §6.3
    pub shutdown: CancellationToken,         // daemon 停机 token 的 child
    pub global_admission: Arc<Semaphore>,    // MODEL_MAX_IN_FLIGHT_GLOBAL
}
/// 挂载级：mount_package 里、MethodsFor 闭包外建一次；闭包内只 clone。
#[derive(Clone)]
pub struct ModelGrant {
    pub module: String,
    pub privacy: Privacy,
    pub limiter: Arc<RateLimiter>,           // agentd events_emit::RateLimiter，复用
    pub module_admission: Arc<Semaphore>,    // MODEL_MAX_IN_FLIGHT_PER_MODULE
    pub deps: ModelCallbackDeps,
}
```

### 5.2 数值（⚖️ = 选的，不是推出来的）

| 常量 | 值 | 理由 |
|---|---|---|
| `MODEL_MAX_IN_FLIGHT_PER_MODULE` ⚖️ | 2 | PLAN 建议值。本地推理基本串行，更多并发只是在 oMLX 里排队、吃掉各自的 120s |
| `MODEL_MAX_IN_FLIGHT_GLOBAL` ⚖️ | 4 | 所有模块合计。**不含** `/api/v1/chat` 与 agent loop——内核自己的用量不被模块挤占，但也因此不能保证 oMLX 不过载（R3） |
| `MODEL_RATE_CAPACITY` / `MODEL_RATE_REFILL_PER_SEC` ⚖️ | 30 / 0.5 | 突发 30 次、持续 30 次/分钟。按次计，不按 token 计（token 在调用前未知） |
| `MODEL_CALL_TIMEOUT` | 120s | = provider 自己的 `chat_timeout`（`lib.rs:206`）；再长 provider 先放弃 |
| `MODEL_MAX_TOKENS_CEILING` / 缺省 ⚖️ | 4096 / 1024 | 4096 token 的文本远低于 1 MiB 帧上限；缺省给个上限，免得没写 `max_tokens` 的模块把 120s 跑满 |
| `MODEL_MAX_MESSAGES` ⚖️ | 64 | 字节已由 dispatch 预算兜住，条数防病态的「一万条空消息」 |

- 满了**不排队**：`try_acquire_owned` 失败即 `busy`（同回调通道的并发语义，队列是对端控制的内存）。
- **跨重启**：令牌桶与每模块信号量在挂载级，**跨 restart generation 不重置**（SPEC §5「限流桶不能被崩溃重置：以稳定的 `(module, …)` 为键、跨连接跨 restart generation 生效」）；
  **daemon 重启会重置**——它们只在内存里，模块触发不了 daemon 重启（同 UID 的敌意进程能，但那在 §0 之外）。
- **与 events 的相反选择，理由**：`_a24/events/emit` 的桶**故意每代重建**（`domain.rs:1454-1458`，Codex round 3 Medium 2：不让重启的模块继承上一代花掉的额度）；
  memory 与本文照 SPEC §5 放在挂载级。模型调用是**贵的**（本地 GPU 时间；远端是钱），若每代重建，一个模块靠自崩溃循环就能每次拿满 30 次突发——
  这正是 SPEC §5 那一条要防的。代价是一个刚重启的模块可能继承空桶，最多等 2s 回填一次。
- permit 是 handler future 里的局部变量：任何一条取消路径 drop 掉 future，permit 就还回去，不会泄漏。

---

## 6. 决策 M7（S2-5 后半）：按模块持久化用量

### 6.1 schema（`agent24-store` 新迁移；号取 ME4-1.2.1 之后的下一个，scratch 里暂用 `0008`）

```sql
CREATE TABLE module_model_usage (
    module            TEXT    NOT NULL,
    day               TEXT    NOT NULL CHECK (length(day) = 10),   -- UTC 'YYYY-MM-DD'，记录时刻
    served_by         TEXT    NOT NULL CHECK (served_by IN ('local', 'remote', 'none')),
    calls_ok          INTEGER NOT NULL DEFAULT 0 CHECK (calls_ok >= 0),
    calls_failed      INTEGER NOT NULL DEFAULT 0 CHECK (calls_failed >= 0),
    calls_cancelled   INTEGER NOT NULL DEFAULT 0 CHECK (calls_cancelled >= 0),
    prompt_tokens     INTEGER NOT NULL DEFAULT 0 CHECK (prompt_tokens >= 0),
    completion_tokens INTEGER NOT NULL DEFAULT 0 CHECK (completion_tokens >= 0),
    CHECK (served_by <> 'none' OR (calls_ok = 0 AND prompt_tokens = 0 AND completion_tokens = 0)),
    PRIMARY KEY (module, day, served_by)
) WITHOUT ROWID;
```

- **聚合表，不是逐次流水**：流水无界增长（30 次/分钟/模块 × 一年），聚合每模块每天至多 3 行；需要的查询（按模块、按天、按层）全能回答。
- 写：**一条语句** `INSERT … ON CONFLICT (module, day, served_by) DO UPDATE SET x = x + excluded.x`，token 和用 `min(…, i64::MAX)` 饱和——
  SQLite 串行化写者，读改写在同一语句里，并发记录不会丢增量（scratch 测试见附录 A）。
- 读：`module_model_usage(module, since_day)` 按天明细；`module_model_usage_totals(module)` 一次 `GROUP BY served_by` 的全期合计。

```rust
// agent24-store/src/module_model_usage.rs（scratch 已 check + test）
pub enum ServedBy { Local, Remote, None }
pub enum UsageOutcome { Ok { served_by: ServedBy, prompt_tokens: u64, completion_tokens: u64 }, Failed, Cancelled }
pub struct ModelUsageRow { pub day: String, pub served_by: String, pub calls_ok: u64, pub calls_failed: u64,
                           pub calls_cancelled: u64, pub prompt_tokens: u64, pub completion_tokens: u64 }
impl Store {
    pub async fn record_module_model_usage(&self, module: &str, day: &str, outcome: UsageOutcome) -> Result<()>;
    pub async fn module_model_usage(&self, module: &str, since_day: &str) -> Result<Vec<ModelUsageRow>>;
    pub async fn module_model_usage_totals(&self, module: &str) -> Result<Vec<ModelUsageRow>>;
}
```

### 6.2 计量口径

| 调用结局 | 计不计 | 记在哪 |
|---|---|---|
| 成功 | 计 | `served_by = local/remote`，`calls_ok += 1`，token = provider 回报值（没回报就是 0） |
| 路由器返回 `Unavailable` / `Provider` / LocalOnly 绊线 | 计 | `none` 行 `calls_failed += 1`，token 0 |
| 进了路由器后被取消（`$/cancelRequest`、连接断、撤销、方法超时、所绑请求结束、停机） | 计 | `none` 行 `calls_cancelled += 1`，token 0 |
| 被挡在路由器之前（`-32602`、`forbidden`、`not_ready`/`draining`/`revoked`、`request_id` 不在途、`busy`、`rate_limited`） | **不计** | ——它们没有用到任何模型 |

「失败与取消也计」的理由：远端 provider 可能已经为一次失败/被取消的请求计费；次数是能知道的，token 不能——所以计次数、token 记 0、**不猜**。
「挡在前面的不计」：这张表回答「用了多少模型」，不回答「被拒了多少次」（后者是日志与限流的事）。

### 6.3 什么时候写 —— 调用方从不等磁盘

- `UsageTicket` 在确定进路由器时建；`finish(outcome)` 记一笔；**被 drop 而没 finish**（任何取消路径）→ `Drop` 里记 `Cancelled`。
  所以「每次进了路由器的调用恰好一笔」由类型结构保证。
- 记一笔 = 往 `UsageRecorder` 的有界 `mpsc`（容量 1024 ⚖️）`try_send`，**不 await、不 spawn**——满了就丢这一笔、计入 `dropped` 计数并 `warn`。
  `Drop` 里做的只有这个同步 `try_send`，所以不违反 `Handler` 契约「调用的全部工作必须在返回的 future 里」（SPEC ME-3c 表）：记账不是调用的工作，
  且它不产生任何 handler 之外的任务。
- daemon 里**一个**写者任务（`UsageRecorder::spawn(store, shutdown)`）逐条写库；写失败 `error` 日志，不重试。
  停机：停机 token 触发后，再用至多 500ms ⚖️ 把队列里已有的写掉，超时丢弃（R4）。

```rust
#[derive(Clone)]
pub struct UsageRecorder { tx: mpsc::Sender<UsageRecord>, dropped: Arc<AtomicU64> }
impl UsageRecorder {
    pub fn spawn(store: Store, shutdown: CancellationToken) -> (Self, tokio::task::JoinHandle<()>);
    fn record(&self, module: &str, outcome: UsageOutcome);   // try_send；满 → dropped += 1
    pub fn dropped(&self) -> u64;
}
struct UsageTicket { recorder: UsageRecorder, module: String, done: bool }   // Drop 未 finish → Cancelled
```

### 6.4 费用字段怎么填

**不填 0**。今天 `Usage.cost_usd` 恒 `0.0`（§1 第 1 条），provider 不回报费用、内核没有价目表——一个恒为 0 的「费用」在远端调用上是**错误的事实陈述**。
所以表里**没有费用列**，`GET /api/v1/usage?module=` 的 `cost_usd` 恒为 **`null`**（含义：未知，不是免费）。将来有价目表时加一列 + 迁移，`null` 变成真数。

### 6.5 `GET /api/v1/usage?module=<name>`

- 不带 `module`：**原样**返回 `/chat` 的全局内存计数器（`routes.rs:52-54` 的形状与语义一字不变；未知 query 参数照旧忽略，所以 `UsageQuery` 不 `deny_unknown_fields`）。
  模块调用**不**加进这个全局计数器（它的含义是「本次启动以来 `/api/v1/chat` 的用量」，改它就是改既有行为）。
- 带 `module`：名字过 `agent24_domain::is_valid_module_name`，不合法 → `400 invalid_request`；合法但从没调用过（或已卸载）→ 200 + 全 0。query 串本身解析失败 → `400`（用 `Result<Query<_>, QueryRejection>` 接住，不落到 axum 的纯文本 400）。
- 响应形状：

```json
{
  "module": "sin90",
  "totals":    { "calls_ok": 3, "calls_failed": 1, "calls_cancelled": 0,
                 "prompt_tokens": 120, "completion_tokens": 80, "total_tokens": 200 },
  "by_served": { "local": { …同上六个字段… }, "none": { … }, "remote": { … } },
  "daily":     [ { "day": "2026-09-23", "served_by": "local", "calls_ok": 3, …六个字段… } ],
  "cost_usd":  null
}
```

`totals`/`by_served` 为全期；`by_served` 三个键恒在；`daily` 为含今天在内最近 30 个 UTC 日（⚖️）、新到旧、只含非空行（至多 90 行）。
鉴权同所有内核路由（bearer，`server.rs` 的 auth layer 之内）。

---

## 7. 决策 M6（S2-6）：`ModelError` → 错误闭集

**闭集加一个 kind：`unavailable`**（`ErrorKind::Unavailable`，`ALL` 17 → 18）。现有 kind 里没有能诚实表达「内核够不着一个它允许你用的模型」的：
`busy` 是「本连接/本模块并发满了」，`timeout` 是「时间到了」，`-32603` 在 SPEC 里专指**内核自己的缺陷**（ME-3c 表「handler panic」行）——provider 拒绝不是内核缺陷。

| 来源 | wire | 文案（**不含** provider 名/URL/原文） | `data` |
|---|---|---|---|
| `ModelError::Unavailable(_)`（含 LocalOnly 空路由、429、5xx、连不上、provider 超时） | `-32000` `unavailable` | 「no model this module may use is available right now」 | `retryable: true` |
| `ModelError::Provider(_)`（401/403/404/400/413/422、坏 JSON、超大响应、无 choices） | `-32000` `unavailable` | 「the model backend refused this request」 | `retryable: false` |
| `ModelError::Cancelled`（只可能来自 daemon 停机） | `-32000` `cancelled` | 「the daemon is shutting down」 | — |
| `LifecycleTimeout::{BudgetExhausted, RequestEnded}` | `-32000` `timeout` | 同 memory 的两条（`os_memory_page.rs:218-231`） | — |
| `request_id` 不在途（§3.4） | `-32000` `timeout` | 「request_id is not (or no longer) in flight; …」 | — |
| 方法超时 | `-32000` `timeout`（`serve` 生成） | 「the kernel gave up after 120000ms; …」 | — |
| `$/cancelRequest` | `-32000` `cancelled`（`serve` 生成） | 既有 | — |
| 未授予 | `forbidden` | 「this module was not granted model access」 | — |
| 准入拒绝 | `not_ready` / `draining` / `revoked` | `refused_error`（`events_emit.rs:273`） | — |
| 并发满 / 令牌空 | `busy` / `rate_limited` | 静态 | — |
| params | `-32602` | serde / `validate` 的原文（含合法范围） | — |
| LocalOnly 绊线 / 结果不可序列化 / call 时参数反解失败 | `-32603` | 静态 | — |

- `retryable` 是 `data` 里 `kind` 旁的**附加**字段（`RpcError::with_data`，`rpc.rs:264`），不是新 kind；Provider 细节（哪个 key 错、哪个模型不存在）
  对**模块**没有可行动的价值、对**运维**有——所以 `tracing::warn!(module, detail)` 进 daemon 日志，wire 上一个字都不出（同 `map_memory_error` 的默认拒绝）。
- 对 `local_only` 模块，「只配了远端」与「本地全挂」给**同一句**：模块不该能从错误里推断 daemon 有没有远端。
- 闭集扩展的同步改动（ME4-4.2.2b，一个 PR 内）：`ErrorKind` 加变体 + `ALL` + `as_str`；`rpc.rs` 测试 `the_error_kinds_are_exactly_specs_closed_set` 里的 SPEC 句子拷贝；
  SPEC §3「错误形状」那一句（本分支已改，改后的原句见 SPEC 第 170 行附近）。scratch 已按改后的句子跑过该测试（附录 A）。

---

## 8. 判据（每条带正对照；`cargo test <过滤>` 一律先 `-- --list` 断言非空，PLAN §一 第 5 条）

> 编号 J1–J15。「变异」= `docs/agent/mutate.sh` 的改回方式，改回后该判据必须变红。

**J1 manifest（ME4-4.2.1，`cargo test -p agent24-domain model_access`）**：缺省 → `LocalOnly`；`kernel_capabilities: [models]` + `model_access: remote_allowed` → `RemoteAllowed`；
显式 `local_only` → `LocalOnly`（正对照：合法值被接受）；`model_access: remote` → 错误且消息同时含 `"remote"` 与 `remote_allowed`；`[events]` + `remote_allowed` → 错误且消息含 `models`；
`model_acess:` 拼错 → 错误。变异：删掉「未请求 models」那条检查 → 第 5 条变红。（scratch `manifest_tests` 已跑通。）

**J2 授予与注册（ME4-4.2.2b，`-p agent24d model_grant`）**：manifest 未请求 `models` 的模块调 `_a24/model/complete` → `forbidden`（**不是** `-32601`：方法无条件注册）；
其 `InitializeResult.offer.provides` **不含** `_a24/model/`；请求了 `models` 的模块 → `provides` 含 `_a24/model/`、`MountReport.granted` 含 `models`（正对照）。
`deps = None` 的挂载（测试构造）→ 即使请求了也不出现在 `granted`/`provides`（invariant #134）。

**J3 LocalOnly 负对照（S2-1 明文，`-p agent24d model_callback`）**：手建 `ModelRouter::new(vec![(远端桩, Tier::Remote)])`，`local_only` 模块调用 →
`unavailable`、`retryable: true`、**远端桩收到的请求数 = 0**、消息不含桩的名字；**正对照**：同一路由器、`remote_allowed` 模块 → 成功、`tier: "remote"`、桩计数 1。
同一 `local_only` 模块在 params 里放 `privacy: "any"` / `model: "x"` / `tools: []` → `-32602` 且桩计数 0；`_meta: {privacy: "any", tier: "remote"}` → 仍 `unavailable`、桩计数 0。
变异：`ModelGrant::new` 里把 `LocalOnly` 映射成 `Privacy::Any` → 桩计数变 1，红。（scratch `local_only_never_reaches_a_remote_provider_and_remote_allowed_does` 已跑通。）

**J4 契约（ME4-4.2.2a，`-p agent24-models max_tokens`、`-p agent24-models model_id` 两条命令）**：真 TCP 桩记录请求体——`max_tokens: Some(77)` → 体里 `"max_tokens": 77`；
**正对照** `None` → 体里没有这个键。桩回 `"model": "stub-actual-7b"` → `model_id == Some("stub-actual-7b")`，且 ≠ provider 名 `omlx`、≠ 请求里的名字；
桩不回 `model` → `None`。**/chat 零变化**：经真实 `post_chat` 打桩，请求体的键集合 == `{model, messages, stream}`（与改动前同一断言）。（scratch `wire_tests` 已跑通前两部分。）

**J5 按方法超时（ME4-4.2.2b 的 proto 部分，`-p agent24-os-proto per_method_budget`）**：`Limits.call_timeout = 100ms`；声明 800ms 的 handler 睡 300ms → 成功；
**正对照**：同一 handler 不声明 → `timeout` 且文案含 `100ms`；声明 400ms、睡 5s → `timeout` 且文案含 `400ms`（报实际生效值）。
`effective_call_timeout` 单测：`None → default`、`120s → 120s`、`1h → 300s`（夹紧）。既有 rpc 测试全绿（回归）。变异：`on_frame` 改回读 `self.limits.call_timeout` → 第一条变红。
（scratch 已跑通：rpc 53 个测试全绿。）

**J6 字面 30s（PLAN 4.2.2b 验收原文，`-p agent24d model_callback_outlives_call_timeout`）**：真 `serve()` + `Limits::default()` + 进程内桩 provider 睡 31s → 成功。
正对照由 J5 的缩放版承担（同一关系、不同量级）；另加变异：`ModelCompleteHandler::call_timeout` 返回 `None` → 30s 处 `timeout`，红。（这条测试单独花 ~31s，只此一条。）

**J7 取消到达 provider（ME4-4.2.2b，`-p agent24d model_callback_cancel`）**：provider = 指向 Rust TCP 桩的真 `OpenAiCompatProvider`，桩收完请求后不回，记录「对端是否在 N 秒内关闭连接」。
经真 `serve()`（duplex）：
- (a) `$/cancelRequest` → 模块收到 `cancelled`；桩 1s 内观察到关闭。
- (b) 模块一侧关闭回调连接 → 无响应；桩 1s 内观察到关闭。
- (c) 调用带一个 `admit_request` 得到的在途 id，对该 `InFlight` 调 `finish()` → `timeout`（「already ended」）；桩 1s 内观察到关闭。
- (d) 取消 `ModelCallbackDeps.shutdown` → `cancelled`（「shutting down」）；桩 1s 内观察到关闭。
- **正对照**：同一搭建、不触发任何一条，桩在 2s 后应答 → 调用成功、桩没有观察到提前关闭。
- 每条之后 `module_model_usage` 的 `none` 行 `calls_cancelled` 各 +1（(d) 经 `ModelError::Cancelled` 分支）。
代次撤销与 (b) 同一机制（`serve_until` 的 stop → `handlers.shutdown()`），既有 `when_serve_returns_every_handler_future_is_already_dropped` 已钉住，本文不重复；黑盒 J14 用 `os disable` 真实走一遍。
（scratch 已跑通前提：drop 真 provider future → TCP 桩观察到关闭；drop handler future → provider token 被取消且记一笔 `cancelled`。）

**J8 生命周期绑定（`-p agent24d model_callback_lifecycle`）**：Running、不带 `request_id` → 执行（正对照）；带一个从未存在的 id → `timeout` 且桩计数 0；
带一个刚 `finish()` 的 id → 同上。Draining：不带 id → `draining`；带在途 id → 执行；Revoked → `revoked`。
变异：删掉 §3.4 的 `request_id.is_some() && lifecycle.is_none()` 检查 → 第二条变成「执行」，红。（scratch `an_unknown_request_id_is_refused_not_run_unbound` 已跑通前两条。）

**J9 并发（`-p agent24d model_callback_busy`）**：桩挂起；同一模块 2 个调用在途，第 3 个 → `busy`。**`busy` 不花令牌**：令牌桶用冻结时钟、容量 3——
c1、c2 挂起（剩 1），c3 `busy`，abort c1，c4 → **成功**；变异：把「扣令牌」挪到「占并发」之前 → c4 变 `rate_limited`，红。
**全局**：模块 A、B 各 2 个在途，模块 C 的第 1 个 → `busy`；正对照：只有 A 的 2 个在途时 C 的第 1 个 → 执行。

**J10 令牌桶与跨代（`-p agent24d model_callback_rate`）**：冻结时钟，第 31 次 → `rate_limited`；时钟前进 2s → 下一次成功（正对照）。
**跨 restart generation**：对同一次挂载的 `MethodsFor` 闭包调用两次（两代），第一代耗尽令牌后第二代的第一个调用 → `rate_limited`；
正对照：另一个模块的第一个调用成功。变异：把 `ModelGrant` 的构造挪进闭包 → 第二代成功，红。

**J11 计量（ME4-4.2.3，`-p agent24d usage_by_module`）**：两个模块各成功一次 → 各自 `calls_ok = 1`、互不串；关闭并以同一数据文件重开 daemon（或 `Store`）→ 仍在（**重启不清零**）；
一次 `unavailable` → `none.calls_failed = 1`；一次被取消 → `none.calls_cancelled = 1`；一次 `forbidden`、一次 `busy`、一次 `rate_limited` → **不**增加任何计数（正对照：同批里成功的那次增加了）。
store 层：50 个并发 `record_module_model_usage` → 合计恰为 50；token 和饱和到 `i64::MAX` 不回绕；`served_by = 'none'` 带 token 的行被 CHECK 拒。
测试等待写者落盘用轮询（≤ 5s）而不是 sleep。（scratch store 两个测试已跑通。）

**J12 用量 API（`-p agent24d usage_by_module_api`）**：不带 `module` → 与改动前**逐字节**相同的 JSON（golden：`{"prompt_tokens":…,"completion_tokens":…,"total_tokens":…,"cost_usd":…}`）；
`?module=../x` → 400；`?module=never_called` → 200、全 0、`by_served` 三键齐全、`cost_usd: null`；`?module=a&module=b` → 400 `invalid_request` JSON（不是 axum 纯文本）。

**J13 错误映射（`-p agent24d model_callback_errors` + `-p agent24-os-proto the_error_kinds_are_exactly_specs_closed_set`）**：桩名 `stub-SECRET`、URL 含 `secret-host`：
`Unavailable` → `unavailable` + `retryable: true`，`message` 与整个 `error` JSON 都不含 `SECRET`/`secret-host`；桩回 401 → `unavailable` + `retryable: false`，同样不含。
闭集测试的 SPEC 拷贝含 `unavailable`；变异：从拷贝里删掉 `unavailable` → 红（证明量具有效）。

**J14 黑盒（ME4-4.3.1，`--test me4_model_blackbox`，连跑 10 次）**：Python `http.server` 桩当 `OMLX_URL`（python3 缺失即失败）；仓外 Python 模块 manifest 请求 `models`：
挂载 → 模块在启动后不带 `request_id` 调一次 → 探针文件记下 `text`/`model_id`（= 桩回的 id）/`tier: local`；`GET /api/v1/usage?module=` 计数 1；重启 daemon 后仍 1；
桩对第二次调用挂起，期间 `agent24 os disable` → 桩观察到连接关闭（撤销路径的真实版）；未请求 `models` 的第二个模块 → `forbidden`（正对照）。
**黑盒不做 LocalOnly 负对照**：`from_env` 恒带一个写死 `127.0.0.1:11434` 的 ollama（§1 第 6 条），开发机上若真跑着 ollama，「全挂即报错」的断言会不确定；负对照由 J3 进程内承担。另留 `#[ignore]` 的真 oMLX 冒烟。

**J15 LocalOnly 绊线**：绊线在正常路由下不可达（`tier_order` 保证），所以不能直接测「它会触发」；用变异测：把 `tier_order` 的 `LocalOnly` 分支加上 `Tier::Remote` →
J3 的「桩计数 0」变红、**且**模块收到 `-32603`（绊线生效，结果未返回）。

---

## 9. 自审

1. **(a) 是否真的「不调大全局 `CALL_TIMEOUT`」**：是。改的是「谁决定这次调用的预算」，默认值与所有既有方法不变；`MAX_METHOD_CALL_TIMEOUT` 保证没有方法能无界。
   `Handler` 加的是带默认实现的方法，源码兼容（scratch 里 os-proto 既有的全部测试 handler 零改动编过）。
2. **「统一桥接」有没有夸大**：§3.3 如实写了五条路里四条靠 drop、一条靠 token；`DropGuard` 的价值是防御性的。之所以仍然接 token，是因为 provider 契约
   （ADR-026 §6.5「取消是一等公民」）要求每个调用都有 token，且 daemon 停机只能经 token 到达。
3. **LocalOnly 是不是单点**：强制点仍是 `tier_order` 一处（刻意——两个会漂移的判断比一个更糟）；其上的 wire 层让「改隐私」不可表达，其下的绊线让破坏可见。
   真正的前提是 `Tier` 标签诚实：`from_env` 对非回环 URL 降级为 `Remote`（`router.rs:151-163`），手建路由器的调用方须遵守 `ModelRouter::new` 的隐私契约注释。
4. **比 memory 更严的一处（`request_id` 不在途即拒）会不会误伤**：会拒掉「请求刚结束时发出的补全」——这正是要拒的；想要后台语义就不带 id。
   不改 memory 的既有行为（那是另一个已冻结设计的范围）。
5. **计量不阻塞**：`try_send` + 单写者；丢记录可观测（`dropped` 计数 + warn），不静默。`Drop` 里只做同步 `try_send`，不 spawn。
6. **没有复制 FU-70**：handler 用 `admit_callback_bound` + `bind_to_lifecycle`，与 memory 同一形状。
7. **offer set 阶梯**：发现 PLAN 原切法会造出「已授予但方法 not found」，已在 §2.3/§10.2 要求改切法，不是默默照做。
8. **片段都编过**：附录 A；scratch 用的是真实 crate 的拷贝打补丁，不是凭空的影子类型（`agent24d` 是 binary crate，无法作 path 依赖，
   所以 handler 所需的 `RateLimiter`/`refused_error` 按原形状复制了一份，挂载接线写成纯函数 `mount_sketch.rs`）。

---

## 10. 接口清单与切法

### 10.1 接口清单

| crate / 文件 | 新增或改动 | 任务 |
|---|---|---|
| `agent24-domain/src/lib.rs` | `pub enum ModelAccess { LocalOnly, RemoteAllowed }` + `ALL`/`as_str`/`parse`；`RawManifest.model_access: Option<String>`；`DomainOsManifest::model_access()`；解析期两条校验 | 4.2.1 |
| `agent24-models/src/lib.rs` | `CompletionRequest.max_tokens: Option<NonZeroU32>`；`CompletionResponse.model_id: Option<String>`；`OaChatResponse.model`；请求体条件写 `max_tokens` | 4.2.2a |
| `agent24-models/src/router.rs` | `pub struct Served { provider, tier, response }`；`ModelRouter::complete_served`；`complete` 改为其投影 | 4.2.2a |
| 工作区其它 crate | 15 处结构体字面量补 `max_tokens: None` / `model_id: None` | 4.2.2a |
| `agent24-os-proto/src/rpc.rs` | `Handler::call_timeout`（默认 `None`）；`MAX_METHOD_CALL_TIMEOUT`；`effective_call_timeout`；`Conn.by_task` 带时长；`on_frame`/`finished`；`ErrorKind::Unavailable`（`ALL` 18）；闭集测试的 SPEC 拷贝 | 4.2.2-0（新，见 10.2） |
| `agentd/src/model_callback.rs`（新，`main.rs` 加 `mod model_callback;`） | 常量（§5.2）、`ModelCompleteParams` 等 wire 类型、`ModelCallbackDeps`、`ModelGrant`、`ModelCompleteHandler`、`map_model_error`、`UsageRecorder`/`UsageTicket` | 4.2.2b（记账部分 4.2.3） |
| `agentd/src/domain.rs` | `KERNEL_OOP_GRANTS` 加 `Models`；`mount_all`/`mount_package` 加 `models: Option<&ModelCallbackDeps>`；`ModelGrant` 闭包外构造；`provides`/`granted_names`；注册 `_a24/model/complete` | 4.2.2b |
| `agentd/src/server.rs` | `serve()` 里建 `ModelCallbackDeps`（`router` 复用 `AppState.router`、`UsageRecorder::spawn(store, shutdown)`、全局 `Semaphore(4)`）并传给 `mount_all` | 4.2.2b / 4.2.3 |
| `agent24-store` | 迁移 `00NN_module_model_usage.sql`；`module_model_usage.rs`：`ServedBy`、`UsageOutcome`、`ModelUsageRow`、三个 `Store` 方法 | 4.2.3 |
| `agentd/src/routes.rs` | `get_usage` 加 `Result<Query<UsageQuery>, QueryRejection>`；`ModuleUsageResponse`/`UsageCounts`/`DailyUsage` | 4.2.3 |
| `docs/specs/SPEC-ME3-OUT-OF-PROCESS.md` | §3 offer set + ME-4b 注、错误闭集句、「超时」行、方法表 `_a24/model/complete` 行；§5「模型回调」段；§8 ME-4b 行与 3c 表超时行注；§9 | **本分支已改** |

### 10.2 对 PLAN §三 切法的修改要求（开工前先改 PLAN 与台账）

1. **ME4-4.2.1 收窄为「manifest 字段」**：只做 `agent24-domain` 的 `ModelAccess` 与校验（验收 `-p agent24-domain model_access`）。
   **`KERNEL_OOP_GRANTS` 加 `Models`、`provides` 加 `_a24/model/`、`model_grant` 判据移到 4.2.2b**——否则 4.2.1 合入到 4.2.2b 合入之间，生产 offer set 里有 `Models` 却没有 handler（SPEC §8 的阶梯规则）。
   这样 4.2.1 也**不再改 `mount_package`**，不必排在 ME4-1.5.1 之后，可与 M1 并行。
2. **新增 ME4-4.2.2-0「回调通道按方法超时 + `unavailable` kind」**（只动 `agent24-os-proto/src/rpc.rs` 与 SPEC 闭集句的测试拷贝，约 60 行 + 测试），可与 4.2.2a 并行；4.2.2b 依赖它。
   它与 ME4-1.x 的交集只有「都可能给 `ErrorKind` 加变体」——谁后合谁 rebase 闭集句。
3. 4.2.2b 仍依赖 ME4-1.5.1（改 `mount_package`）。4.2.3 依赖 4.2.2b，迁移号取调度迁移之后的下一个。

### 10.3 与 ME4-1.1.1 的 SPEC 合并点

本分支对 `SPEC-ME3-OUT-OF-PROCESS.md` 的改动（`git diff` 可见）与调度设计预计冲突的位置：
§3 第 143 行 offer set 句（两边都改「`Models`/`Scheduler`/`Policy` 都不在本轮」）；§3 方法表末尾（两边都在 `_a24/approval/status` 行后加行）；
§3 错误闭集句（若调度也扩 kind）；§8 交付表（两边都在 ME-3g 行后加行）；§9「不做 `Models` / `Scheduler` / `Policy`」一条（本分支改成「不做 `Scheduler` / `Policy`」，调度改成去掉 `Scheduler`——合并后应为「不做 `Policy`」）。
**解决原则：两边的增量都保留，句子取两边删减的交集。**

---

## 11. 已接受残余风险

| # | 风险 | 为什么接受 / 谁接 |
|---|---|---|
| R1 | 同步调用最长占一个回调槽 120s；推理结果在连接断开后取不回 | (a) 的固有代价（§3.1）；每模块 2 并发把它压住；需要「提交后离开」的模块用自己的 outbox |
| R2 | 被代理请求内联推理受 30s 代理总时限、fired 内联推理受投递超时约束 | 由 §3.4 的使用指引（先应答、再后台）承担，写进 SPEC 方法表与 T14 wire 文档 |
| R3 | 全局 4 并发不含 `/api/v1/chat` 与 agent loop，本地 oMLX 仍可能过载、模块调用在 oMLX 里排到超时 | 内核自己的用量不该被模块挤占；过载表现为模块侧 `timeout`/`unavailable`，不影响 daemon |
| R4 | 用量记录在队列满或停机时可能丢；取消的远端调用费用未知 | `dropped` 计数 + warn 可观测；费用本来未知（§6.4） |
| R5 | `model_access` 不在 `/api/v1/os` / `agent24 os list` 里显示，用户要看 manifest 才知道模块能不能外发 | 今天远端只能经非回环 `OMLX_URL` 出现（§1 第 6 条），暴露面很小；**followup**：`MountReport` 加 `model_access`（ME4→next，Medium） |
| R6 | 没有按日远端 token 预算，`remote_allowed` 模块在令牌桶内可持续外发 | 需要价目表与远端配置入口，本轮都不做；remote 需 manifest 显式声明（用户安装即知情） |
| R7 | `CompletionRequest`/`CompletionResponse` 加字段对**外部**直接构造它们的 crate 是源码不兼容改动 | 工作区内 15 处机械修；外部（若 Sin90 仍 git 依赖 `agent24-models`）在其 M5 前同步——4.2.2a 的 PR body 点名 |
| R8 | `ModelRouter` 的健康/冷却由模块调用与 `/chat` 共享：模块调用遇到真实不可用会让 provider 进冷却，`/chat` 随之跳过它 | 冷却只由**真实**的 `Unavailable` 触发，反映的是 provider 的真实状态，不是模块能伪造的信号 |
| R9 | daemon 重启重置令牌桶 | 模块无法触发 daemon 重启（§0 同 UID 敌意进程除外） |

---

## 附录 A：scratch crate 与 `cargo check` / `cargo test` 记录

位置：`/private/tmp/claude-502/-Users-jason-Dev-auraai-Agent24/977deb42-1aba-448f-95e7-5bae2dee6fd4/scratchpad/me4s2-check/`。

构成：把 worktree（`73a9592`）的 `agent24-models`、`agent24-domain`、`agent24-os-proto`、`agent24-store` **整份拷贝**进来（包名改 `me4s2-*`、lib 名不变，
`agent24-protocol`/`agent24-core` 仍 path 依赖 worktree），按本文 §2.1 / §3.2 / §4.1 / §6.1 / §7 打补丁；再加一个 `me4s2-check` crate 放
`model_callback.rs`（§4.2–§7 的 handler、wire 类型、grant、记账）、`usage_route.rs`（§6.5）、`mount_sketch.rs`（§2.3）、以及测试。

```sh
S=/private/tmp/claude-502/-Users-jason-Dev-auraai-Agent24/977deb42-1aba-448f-95e7-5bae2dee6fd4/scratchpad/me4s2-check
cd $S
CARGO_TARGET_DIR=target cargo check --workspace --all-targets
CARGO_TARGET_DIR=target cargo clippy -p me4s2-check --all-targets          # 无 warning
CARGO_TARGET_DIR=target cargo test -p me4s2-check                            # handler / params / manifest / wire
CARGO_TARGET_DIR=target cargo test -p me4s2-os-proto --lib rpc::             # 既有 rpc 测试 + 按方法超时
CARGO_TARGET_DIR=target cargo test -p me4s2-store module_model_usage
CARGO_TARGET_DIR=target cargo test -p me4s2-models                           # 既有 28 个测试（/chat 路径回归）
CARGO_TARGET_DIR=target cargo test -p me4s2-domain                           # 既有 47 个测试
```

工具链：本机 `cargo +1.98.0` 不可用（Homebrew rustc 1.95.0），scratch 用默认工具链；实现 PR 仍按 PLAN §一 第 8 条用 `+1.98.0` 跑全局前置。

输出（2026-09-23）：

```
$ cargo check --workspace --all-targets
    Checking me4s2-models v0.3.0 (…/me4s2-check/models)
    Checking me4s2-domain v0.3.0 (…/me4s2-check/domain)
    Checking me4s2-check v0.0.0 (…/me4s2-check/check)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.98s

$ cargo test -p me4s2-check
test model_callback::tests::no_grant_is_forbidden ... ok
test model_callback::tests::params_shape ... ok
test manifest_tests::model_access_default_explicit_and_rejections ... ok
test model_callback::tests::an_unknown_request_id_is_refused_not_run_unbound ... ok
test model_callback::tests::local_only_never_reaches_a_remote_provider_and_remote_allowed_does ... ok
test model_callback::tests::the_third_concurrent_call_is_busy ... ok
test model_callback::tests::dropping_the_call_future_cancels_the_provider_token_and_counts_cancelled ... ok
test wire_tests::max_tokens_is_forwarded_only_when_set_and_model_id_is_parsed ... ok
test wire_tests::dropping_the_provider_future_closes_the_upstream_connection ... ok
test result: ok. 9 passed; 0 failed

$ cargo test -p me4s2-os-proto --lib rpc::
test rpc::tests::a_declared_budget_is_clamped_and_an_absent_one_is_the_default ... ok
test rpc::tests::per_method_budget_outlives_the_connection_budget ... ok
test rpc::tests::the_error_kinds_are_exactly_specs_closed_set ... ok
test result: ok. 53 passed; 0 failed

$ cargo test -p me4s2-store module_model_usage
test module_model_usage::tests::the_none_row_cannot_carry_tokens ... ok
test module_model_usage::tests::per_module_rows_accumulate_and_stay_separate ... ok
test result: ok. 2 passed; 0 failed

$ cargo test -p me4s2-models   → test result: ok. 28 passed; 0 failed
$ cargo test -p me4s2-domain   → test result: ok. 47 passed; 0 failed
```

scratch 与真实代码的差异（如实）：`agentd` 是 binary crate，`RateLimiter` 与 `refused_error` 按原形状复制进 `check/src/events_emit.rs`；
`mount_package` 的新增部分写成纯函数（`mount_sketch.rs`），没有在真实 `mount_package` 上编译；`usage_route.rs` 用 `AppStateLite` 代替 `AppState`。
