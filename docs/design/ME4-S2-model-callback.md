# ME4-S2 —— 推理回调 `_a24/model/*`（ME4-4.1.1 设计）

> **草稿 v2，待第 2 轮评审**（2026-09-24）。v1 经第 1 轮对抗评审 **REQUEST_CHANGES（0 Critical / 2 High / 7 Medium / 10 Low）**，
> 19 条逐条核对后全部采纳，处理见下方「v1 → v2 改动记录」。
> 评审方：按 `docs/agent/PLAN-ME4-OS-CAPABILITIES.md` §一 第 3 条——Codex 额度 2026-09-29 19:28 前耗尽，
> 期间由**全新上下文的 Opus 子代理**做对抗评审（Critical/High/Medium/Low + file:line），评审记录在此处逐轮追加，
> 并在 `docs/agent/followups.md` 的 `ME4-CODEX-DEBT` 追加一行，额度恢复后补 Codex 一轮。
>
> | 轮次 | 评审方 | 结论 | C / H / M / L |
> |---|---|---|---|
> | 第 1 轮（v1） | 全新上下文 Opus 子代理（Tier 2，Codex 额度耗尽） | REQUEST_CHANGES | 0 / 2 / 7 / 10 |
> | 第 2 轮（v2） | 待送审 | — | — |
>
> **与 ME4-1.1.1（调度回调设计 `docs/design/ME4-S1-scheduler-callback.md`，另一个 worktree 并行写、同在评审中）的改动需要合并**——
> 不只是 SPEC 文本，还有 `KERNEL_OOP_GRANTS`、`mount_all`/`mount_package` 参数表、`provides` 块、`MethodsFor` 闭包体、`serve()` 接线、
> `ErrorKind::ALL` 长度（它的判据 C5.9 钉住 17，本文把它变成 18）、迁移号（它用 0007，本文用 0008）等。完整清单与解决原则见 §10.3。
> 本分支对 SPEC **只加 model 相关的行/段**，不动 scheduler 的内容。
>
> 设计里每一段 Rust 签名/片段都在 scratch crate 里 `cargo check` / `cargo test` 过，命令与输出见附录 A。

## 版本改动记录

| 版本 | 日期 | 改动 |
|---|---|---|
| v2 | 2026-09-24 | 第 1 轮评审 19 条全部采纳（见下表）。要点：H1 Tier 标签改用 HTTP 客户端同一解析器判定（现有代码缺陷，登记 FU-71）；H2 每个模块走独立健康表的路由器；M1 取消根与用量写者挂到 `modules_cut_off()`；M2 全局并发加公平规则；M3 结果文本 ≤ 512 KiB；M4 `UsageSink` + 4.2.2b 预拆 b1/b2 + §10.2 完整切法；M5 `OLLAMA_URL` 可配、黑盒做 LocalOnly 负对照；M6/M7 SPEC 措辞与合并清单；L1–L10 |
| v1 | 2026-09-23 | 初稿。裁决 S2-1（`model_access` 字段）、S2-4（选 **(a) 按方法超时 + 统一桥接取消**）、S2-2（`max_tokens: Option<NonZeroU32>` / `model_id: Option<String>`）、S2-5（并发 2/全局 4、令牌桶 30/0.5s、`module_model_usage` 表与 `GET /api/v1/usage?module=`）、S2-6（新增 `unavailable` 一个 kind）、S2-3（`_a24/model/complete` 的确切 JSON 形状；不做 `_a24/model/list`）。同分支改 SPEC-ME3 的 Models 相关内容 |

## v1 → v2 改动记录（第 1 轮：REQUEST_CHANGES，0 C / 2 H / 7 M / 10 L，全部采纳）

| # | 级别 | 问题（已对照代码/实测核实） | v2 的处理（位置） |
|---|---|---|---|
| H1 | High | LocalOnly 依赖的 `Tier` 标签可被绕过：`router.rs` 手写的 `url_host`/`is_loopback_host` 与 reqwest（WHATWG）解析不一致，`OMLX_URL=http://evil.example\@127.0.0.1:8088` 被判 `Local`，reqwest 实际连 `evil.example`（评审 scratch `urlcheck/` 实测；本文 scratch 复现） | §2.3 新增：`env_local_tier` 改为 `reqwest::Url::parse` 后按 host 判定（IPv4/IPv6 `is_loopback()`、域名只认 `localhost`、解析失败/无 host/非 http(s) 一律 `Remote`），且**对基址与适配器实际请求的两个 URL 都判**；放进 4.2.2a。判据 J16（变体矩阵 + 正对照）。SPEC §3 ME-4b 注措辞收窄为「以 Tier 标签为准；`from_env` 用与 HTTP 客户端同一解析器判定回环」。这是**现有代码的缺陷**，影响 LocalOnly 的所有使用方（Guardian、会话摘要器），登记 **FU-71**（§12） |
| H2 | High | 模块可诱发 5xx/429 让共享 `ModelRouter` 的冷却表把 provider 冷却，打挂 `/api/v1/chat`、Guardian（`server.rs:341-356`）、会话摘要器（`server.rs:418-430`） | §5.1：每个 `ModelGrant` 持有 `deps.router.with_separate_health()`——共享 provider、**独立**健康表；模块之间也互不影响。判据 J17（桩对模块调用失败后内核路由器仍打到它；负对照：共享路由器时被跳过）。R8 删除 |
| M1 | Medium | 用量写者与停机竞态；provider token 的父是 daemon 停机 token，停机一开始就中止推理，与模块 drain 语义不一致 | §3.3/§6.3：取消根 `cancel_root` 在 `Shutdown::modules_cut_off()` 触发（停机开始到 cut-off 之间照 Draining 规则：在途继续、新的后台调用 `draining`）；写者在**所有 sender 被 drop**（grant 随 supervisor 拆除、`serve` 把 deps 按值交给 `mount_all`）时写完退出，硬上限 `Shutdown::deadlines().modules`（= cut-off + `CONFIRM` 200ms，不新增停机预算）。J7(d) 改为取消取消根并断言记录落进 sink |
| M2 | Medium | 全局 4 = 2×2，两个模块能永久占满 | §5.3：`ModelAdmission` 一把锁判两个上限，「第二个及以后的并发不得占最后一个空闲全局槽」——两个模块永远占不满 4 个，活跃模块数 < 4 时新模块的首个调用总有槽。判据 J9 钉住；≥ 4 个同时活跃模块的饥饿写进 R3 |
| M3 | Medium | provider 可回 ≤ 8 MiB，> 1 MiB 帧上限 → 变成 `-32603` | §4.3：结果 `text` ≤ 512 KiB，构造结果**之前**检查；超出 → `unavailable` + `retryable:false` + `cause:"response_too_large"`，计 `FailedAfterServe`（token 保留在所属层行）。判据 J18（桩回 2 MiB） |
| M4 | Medium | 切法：4.2.2b 同时含 handler、接线、落库，过大且 J7 依赖存储 | §6.3 引入 `UsageSink` trait：4.2.2b1 用 `MemoryUsageSink`，J7 对 sink 断言；落库（`UsageRecorder`）与其断言移到 4.2.3。4.2.2b 预拆 b1（`model_callback.rs`，**方法不注册**）/ b2（授予、`provides`、注册、`serve` 接线——offer set 阶梯只由 b2 跨越）。§10.2 写成「交给 PLAN 的切法」完整清单 |
| M5 | Medium | 黑盒可以做 LocalOnly 负对照 | §8 J14：`from_env` 的 ollama 地址改为可配 `OLLAMA_URL`（缺省不变），用同一规则判层；黑盒 `OMLX_URL=http://127.0.0.1:<p1>`（本地桩）、`OLLAMA_URL=http://0.0.0.0:<p2>`（非回环 → `Remote`，实际连回本机的远端桩）。负对照：本地桩回 503 时 `local_only` 模块 → `unavailable` 且远端桩计数 0；正对照：`remote_allowed` + `complex` → `tier:"remote"`、远端桩计数 1 |
| M6 | Medium | SPEC diff 自相矛盾：§3 首句仍说本轮集合不含 Models，§9 又说 Models 已提供 | SPEC §3 首句与 §9 改为「ME-3 为 `{Memory, Events, Approval}`；ME-4b 起为 `{Memory, Events, Approval, Models}`」 |
| M7 | Medium | §10.3 冲突清单只列 SPEC 文本，漏了代码层 | §10.3 补全：`KERNEL_OOP_GRANTS`、`mount_all`/`mount_package` 参数表与全部调用点和测试、`provides` 块、`MethodsFor` 闭包体、`serve()` 构造 deps、`ErrorKind::ALL` 长度与闭集测试的 SPEC 拷贝（ME4-S1 C5.9 钉 17）、`Handler` trait、迁移号（「后合并方 rebase 时取 max+1」）、`me3-status.sh` 探针、`followups.md` 编号 |
| L1 | Low | §2.2 高估绊线 | 如实写：绊线读的是同一个 `Tier` 标签，只能抓 `tier_order` 回归，抓不到打错标签（那由 H1 的判定与 J16 管） |
| L2 | Low | §5.2 超时文案方向错 | 改正：RPC 计时 120s 与 provider 自己的 120s 同时起算、RPC 的先到点；路由器按序尝试多个 provider 时，第一个慢 provider 可以吃完整个预算，后面的轮不到 |
| L3 | Low | `retryable` 一个布尔不够 | `data.cause` 闭集 ∈ {`no_provider`, `request_rejected`, `backend_config`, `response_too_large`}；为此 `ModelError` 加 `Rejected { status, message }`（4xx 非 429；`Display` 与 `Provider` 同文，`/chat` 与 agent loop 输出不变） |
| L4 | Low | 不在途 id 的 `timeout` 会被模块当成可重试 | 加 `data.retryable: false` |
| L5 | Low | `?module=` 的 `QueryRejection` 会让不带 module 的畸形 query 从 200 变 400 | 改读 `RawQuery`、只看 `module` 键：无 → 原响应（含 `?%zz`）；一个 → 模块用量；多个 → 400。全局 `cost_usd: 0.0` 记 **FU-72** |
| L6 | Low | 令牌桶判据需要注入时钟 | `ModelGrant::with_clock` |
| L7 | Low | `call_timeout` 文档没警示并发 | `Handler::call_timeout` 文档写明「声明 > `CALL_TIMEOUT` 的方法必须自己限并发」 |
| L8 | Low | J1 缺两格 | 补 `[events]` + `model_access: ~` → 接受；`[]` + `model_access: local_only` → 拒 |
| L9 | Low | J11「或 `Store`」含糊 | 删掉，只用「以同一数据文件重开 daemon」 |
| L10 | Low | SPEC §5「每一种最终都是 handler future 被 drop」不准 | 改为：前四种 drop 整个 handler future；所绑请求结束时 drop 的是 `bind_to_lifecycle` 里的内层 work（router future），handler 随后返回 `timeout`；停机经取消根 token 到达 |

---

## 0. 这份文档解决什么、不解决什么

**解决**（PLAN §二 S2 第 1–7 条，每条都是下限，本文只做得更严）：

| S2 条 | 本文位置 | 一句话结论 |
|---|---|---|
| S2-1 隐私 | §2 决策 M1 | manifest 新字段 `model_access: local_only \| remote_allowed`，缺省 `local_only`；隐私**只**来自挂载时的 manifest，params 里没有任何能改它的字段（`deny_unknown_fields` 让 `privacy`/`model`/`tools` 都是 `-32602`） |
| S2-4 超时与取消 | §3 决策 M2 | 选 **(a)**：`Handler` 加一个默认方法 `call_timeout()`，`_a24/model/complete` 声明 120s，连接级 30s 对其它方法不变；`$/cancelRequest` / 连接关闭 / 代次撤销 / 方法超时四条 drop 整个 handler future，所绑请求结束 drop 内层 work，handler 里持有 provider `CancellationToken` 的 `DropGuard`，于是统一到达 provider；daemon 停机经取消根（`modules_cut_off()` 时触发）直达 token |
| S2-2 契约扩展 | §4.1 决策 M3 | `CompletionRequest.max_tokens: Option<NonZeroU32>`（`None` 不发字段，`/api/v1/chat` 字节不变）、`CompletionResponse.model_id: Option<String>`（取 OpenAI `response.model`，不回填）、`ModelRouter::complete_served` 返回 tier |
| S2-3 方法最小集 | §4.2–§4.4 决策 M4 | 只有 `_a24/model/complete`，确切 params/result 见 §4.2/§4.3；不开 tools；**不做** `_a24/model/list` |
| S2-5 限流 + 计量 | §5 决策 M5、§6 决策 M7 | 每模块并发 2、全 daemon 4（带公平规则）、每模块令牌桶（挂载级，跨 generation 不重置）、**每模块独立健康表**；新表 `module_model_usage`，失败/取消都计、被挡在路由器之前的不计、`cost_usd` 恒 `null` |
| S2-6 错误 | §7 决策 M6 | 闭集加一个 `unavailable`，`data.retryable` + `data.cause`（闭集 4 值），provider 名/URL/原文一律不出内核 |
| （评审 H1） | §2.3 | LocalOnly 的前提——`Tier` 标签诚实——今天被手写 URL 解析破坏；改为与 HTTP 客户端同一解析器 |
| S2-7 测试 | §8 判据 | 进程内用 Rust 桩 + 手建 `ModelRouter`；黑盒用 Python 桩当 `OMLX_URL`，python3 缺失即失败；另留 `#[ignore]` 真 oMLX 冒烟 |

**不解决**（写进 SPEC §9，见 §11 残余风险）：tools / function calling；流式输出；模块自选模型；`_a24/model/list`；
远端 provider 的配置入口（今天唯一的「远端」来源是非回环的 `OMLX_URL`，v2 起加上非回环的 `OLLAMA_URL`，§1 第 6 条）；价目表与费用计算；按日远端 token 预算；
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
15. **store**：`agent24-store` 的迁移到 `0006`（`migrations/`），池 5 连接（`agent24-store/src/lib.rs:59`）。ME4-1.2.1（调度，ME4-S1）用 **0007**，本文用 **0008**。
16. **（v2，H1）Tier 标签的判定与实际连接不一致**：`env_local_tier` 用手写的 `url_host`（`router.rs:118-131`，`rsplit_once('@')` 剥 userinfo、按 `/?#` 切 authority）+ `is_loopback_host`（`router.rs:137-146`）；
    reqwest 用 WHATWG URL 解析，`\` 在 http(s) 里等同 `/`、会结束 authority。于是 `http://evil.example\@127.0.0.1:8088`：手写解析得 host `127.0.0.1` → `Local`，
    reqwest 连 `evil.example`。评审在 `scratchpad/urlcheck/` 实测，本文 scratch 的 J16 用 `reqwest::Url::parse(h1).host_str() == "evil.example"` 复现。
    **这是现有代码的缺陷**：今天所有 LocalOnly 使用方（Guardian `server.rs:341-356`、会话摘要器 `server.rs:418-430`）都受影响，不只模块。
17. **（v2，H2）一个路由器、一张健康表**：`AppState.router`（`server.rs:31`，`server.rs:909` 建一次）同时服务 `/api/v1/chat`、agent loop、Guardian、会话摘要器；
    `record_failure`（`router.rs:257`）在任何调用方遇到 `Unavailable` 时把该 provider 冷却，所有调用方随之跳过它。
18. **（v2，M1）停机的时间点**：`Shutdown::modules_cut_off()`（`server.rs:214-223`）= 停机开始 + 模块 drain + stop grace；
    `Deadlines.modules` = 它 + `CONFIRM` 200ms（`lifecycle.rs:106`），其后才是写停机摘要的 `PERSIST`。模块的在途请求/回调活到 cut-off。

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
| 标签（`env_local_tier`，§2.3，v2 H1） | 每个 provider 的 `Tier` 用与 HTTP 客户端**同一个**解析器判定；判不准一律 `Remote` | 路由层的强制完全依赖标签诚实；标签错了，下一层什么都挡不住 |
| 路由（`ModelRouter::route` / `tier_order`，既有） | `LocalOnly` 的层序只有 `Local/Lora`；空路由即 `Unavailable`，**不碰远端 provider** | 既有、已测的唯一强制点（§1 第 3 条）；本文不另造第二个会漂移的判断 |
| 事后绊线（handler，§4.4） | `privacy == LocalOnly && !served.tier.is_local()` → 记 `error` 日志、结果**不返回**、回 `-32603` | **只是检测，不是防护**——字节已经发出去了。**而且它读的是同一个 `Tier` 标签**（v2 L1）：它只能抓到 `tier_order` 的回归（J15），**抓不到打错的标签**——一个被误标成 `Local` 的远端 provider 在绊线看来就是本地的。标签的正确性只由 §2.3 与 J16 保证 |

`remote_allowed` 的调用以 `Privacy::Any` 路由，模块只能给 `complexity: simple | complex`（缺省 `simple`）：`Simple` 本地优先、`Complex` 远端优先（`router.rs:78-87`）。
选哪个 provider、用哪个模型，**全由内核**。

### 2.3 `Tier` 标签的判定（v2 H1；ME4-4.2.2a，现有代码缺陷 FU-71）

`router.rs` 的 `url_host`/`is_loopback_host`（§1 第 16 条）删除，`env_local_tier` 改为：

```rust
// agent24-models/src/router.rs（scratch 已改真实文件的拷贝并测试）
/// 用 HTTP 客户端自己的解析器（reqwest::Url，WHATWG）判定是否在本机。
fn is_loopback_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else { return false };
    if !matches!(parsed.scheme(), "http" | "https") { return false; }
    let Some(host) = parsed.host_str() else { return false };
    let bare = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    match bare.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.is_loopback(),
        Ok(std::net::IpAddr::V6(v6)) => v6.is_loopback(),
        Err(_) => bare.eq_ignore_ascii_case("localhost"),
    }
}
/// 基址本身，以及适配器真正会请求的两个 URL，都必须是本机，才标 Local。
fn env_local_tier(url: &str) -> Tier {
    let requested = [url.to_owned(), format!("{url}/v1/chat/completions"), format!("{url}/v1/models")];
    if requested.iter().all(|u| is_loopback_url(u)) { Tier::Local } else { /* warn */ Tier::Remote }
}
```

- 为什么判三个 URL：适配器请求的是 `format!("{base}/v1/chat/completions")`（`lib.rs:520-521`），不是基址；只判基址，就又留下「判的串 ≠ 连的串」这同一类缺口。
- 判定是**保守**的：`::ffff:127.0.0.1`（`Ipv6Addr::is_loopback` 为假）、`0.0.0.0`、`localhost.` 都判 `Remote`——它们也许实际连回本机，但把本机标成远端不会泄露，反过来才会。
  J14 的黑盒正是利用这一点用 `0.0.0.0:<port>` 造一个「标签为远端、实际在本机」的桩。
- `OLLAMA_URL`（v2 M5）：`from_env` 里写死的 `http://127.0.0.1:11434` 改为 `std::env::var("OLLAMA_URL")`（缺省值不变），同一规则判层。
- SPEC §3 的表述收窄为「**以 `Tier` 标签为准**；`from_env` 用与 HTTP 客户端同一解析器判定回环，判不准即 `Remote`」——不再写成「LocalOnly 绝不外发」这种比机制强的话。

### 2.4 授予与挂载接线

- `KERNEL_OOP_GRANTS` 加 `Capability::Models`；**进程内 `KERNEL_GRANTS` 不加**（`KernelCtx` 没有模型句柄——授予没有句柄的能力就是撒谎，SPEC §3）。
- `mount_all`/`mount_package` 多一个参数 `models: Option<ModelCallbackDeps>`（daemon 级依赖，§5.1；**按值**传入，`mount_all` 返回时它随之 drop，于是用量通道的 sender 只剩 grant 里的那些，§6.3）。`ModelGrant` 在 `MethodsFor` **闭包外**建一次：

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
  注册 handler **必须在同一个 PR 落地**——本文把它定为 **ME4-4.2.2b2**；4.2.2b1 只交付 `model_callback.rs`（handler 写好、测好，但**不注册、不授予**）。
  PLAN §三 原切法把授予放在 4.2.1、handler 放在 4.2.2b，会出现「已授予但方法 not found」的中间态；完整的切法修改见 §10.2，开工前先改 PLAN §三 与台账。
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

### 3.3 六条取消路径如何到达 provider 的 `CancellationToken`

handler 里（§4.4 完整代码）：

```rust
let cancel = grant.deps.cancel_root.child_token();   // 父 = 取消根：在 modules_cut_off() 触发（v2 M1）
let _cancel_on_drop = cancel.clone().drop_guard();   // 本 future 被 drop → cancel()
bind_to_lifecycle(lifecycle, grant.router.complete_served(profile, &request, &cancel)).await   // grant.router：独立健康表（v2 H2）
```

| 触发 | 机制（既有，行号见 §1） | 到达 provider 的方式 | 模块收到 |
|---|---|---|---|
| `$/cancelRequest` | `handle.abort()` → 任务被取消 → handler future drop | drop 带走 provider future（hyper 关连接）+ `DropGuard` 取消 token | `cancelled`（由 `serve` 生成） |
| 回调连接关闭 | `serve` 退出 → `handlers.shutdown().await` | 同上 | 无响应（连接没了，SPEC §3） |
| 代次撤销 | `serve_until` 的 stop = `generation.revoked()` → 同上 | 同上 | 无响应 |
| 方法超时 120s | `tokio::time::timeout` 到点 → drop 内层 future | 同上 | `timeout`，文案报 `120000ms` |
| 所绑请求结束 / 预算耗尽 | `bind_to_lifecycle` 的 `select!` 另一臂胜出 → drop 的是**内层 `work`（router future）**，不是整个 handler future（v2 L10） | drop 带走 provider future；handler 随后返回，`DropGuard` 取消 token | `timeout`（`RequestEnded` / `BudgetExhausted` 两条文案，同 memory） |
| daemon 停机 | **v2 M1**：停机开始时什么都不做——模块照 drain 规则运行（在途调用继续；generation 进入 Draining 后新的后台调用回 `draining`）；到 `Shutdown::modules_cut_off()`，`serve()` 里的一个任务取消 `cancel_root` → provider 在 `select!` 处返回 `ModelError::Cancelled`（若代次撤销先到，则按撤销那一行） | 取消根 token 直达 | `cancelled`（「the daemon is shutting down」） |

**如实写 token 与 drop 的分工**：前五条里真正打断 HTTP 请求的是 **drop**（前四条 drop 整个 handler future，第五条只 drop 内层 work）（已实测，§1 第 4 条）；`DropGuard` 保证的是「凡是从这个 token 派生、活在 future 之外的东西也会停」——
今天 provider 不派生任何东西，所以它是防御性的，将来 provider 若加流式/后台读，不需要再改 handler。只有「daemon 停机（取消根）」一条是**只靠 token** 到达的。

取消根的接线（`serve()`，scratch `mount_sketch::spawn_cancel_root` 已 check）：

```rust
pub fn spawn_cancel_root(modules_cut_off: impl Future<Output = ()> + Send + 'static) -> CancellationToken {
    let root = CancellationToken::new();
    let fire = root.clone();
    tokio::spawn(async move { modules_cut_off.await; fire.cancel(); });
    root
}
// serve(): let cancel_root = spawn_cancel_root(shutdown.modules_cut_off());
```

为什么不在停机一开始就中止：模块在 drain 期间被允许把在途请求做完，而那些请求里的推理若被 daemon 先一步中止，drain 就名不副实；
cut-off 是「模块本来就会被杀」的时刻，推理与它同生共死。

### 3.4 有没有 `request_id`，生命周期怎么绑

| 调用形态 | 准入（`admit_callback_bound`，同 memory 的表） | 约束它的东西 |
|---|---|---|
| 带 `request_id`，该请求**在途** | Running / Draining 都放行 | 方法超时 120s、**该请求剩余预算**（被代理请求 ≤ 30s；调度 fired 投递的请求 ≤ ME4-S1 定的投递超时，建议 10s）、请求结束、`$/cancelRequest`、连接、撤销、取消根 |
| 带 `request_id`，该 id **不在途**（从未存在或已结束） | **本文比 memory 更严**：Running 下也**拒**，`timeout`「request_id is not (or no longer) in flight; send no request_id for background work」；Draining 下照既有表回 `draining` | ——（不会跑） |
| **不带** `request_id`（后台任务、定时任务） | Running 放行；Starting → `not_ready`，Draining → `draining`，Revoked → `revoked` | 方法超时 120s、`$/cancelRequest`、回调连接关闭、**代次撤销**（在途的无绑定调用在 drain 结束、代次被撤销的那一刻被 drop）、取消根（停机时的 modules cut-off，§3.3） |

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

- **（v2 L3）`ModelError` 加一个变体**，让调用方不读 provider 原文也能区分「请求被拒」与「后端配置/故障」：

```rust
#[error("provider error: {message}")]          // 与 Provider 同一 Display 文本
Rejected { status: u16, message: String },     // friendly_http_error 对 4xx（非 429）返回它
```

  路由语义同 `Provider`（终止，不 fallthrough）。工作区里对 `ModelError` 的穷尽 `match` 只有两处——`agent24-agent/src/lib.rs:1026-1031` 与 `agentd routes.rs:140-152`——
  各加一个与 `Provider` 同样处理的分支，所以 `/api/v1/chat` 与 agent loop 的输出**一字不变**；`agent24-models` 自己的三条断言（401/404/403）改为断言 `Rejected { status }`。
- **（v2 H2）** `ModelRouter::with_separate_health(&self) -> ModelRouter`：同一组 provider（`Arc` 共享、标签相同、冷却参数相同）、**空的**健康表。
- **（v2 H1/M5）** `env_local_tier` 换实现（§2.3）；`OLLAMA_URL` 可配。
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

`text` = 助手消息的 `content`（没有则 `""`；不开 tools，所以不返回 `tool_calls`）。**（v2 M3）`text` ≤ `MODEL_MAX_TEXT_BYTES` = 512 KiB**，
在构造结果**之前**检查：provider 的响应体上限是 8 MiB（`lib.rs` `MAX_CHAT_RESPONSE_BYTES`），回调响应帧上限是 1 MiB，不在这里挡，超大答案会被 `response_line` 换成 `-32603`。
超出 → `unavailable` + `retryable:false` + `cause:"response_too_large"`（文案提示调小 `max_tokens`），计量为 `FailedAfterServe`（token 已花，记在所属层的行上）。
512 KiB 给 JSON 转义留了一倍余量（最坏情况每字节转义成 `\uXXXX` 是 6 倍，那种输出由 `response_line` 兜底为 `-32603`，见 R10）。`tier` 是本文在 PLAN 最小集上**加**的一个字段：
`remote_allowed` 的模块据此知道这次数据是否离开了设备（它可以记进自己的审计），且它只暴露层级，不暴露 provider 名/URL。
`Lora` 计为 `local`。`usage.total_tokens` 不给（冗余）。

### 4.4 handler（完整，scratch 已 check + test；v2 更新）

```rust
pub struct ModelCompleteHandler {
    pub generation: Arc<Generation>,
    pub grant: Option<ModelGrant>,          // None → forbidden；方法仍无条件注册（b2 起）
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
            let lifecycle = generation.admit_callback_bound(request_id.as_deref()).map_err(refused_error)?;
            if request_id.is_some() && lifecycle.is_none() {            // §3.4，比 memory 严
                return Err(RpcError::application(ErrorKind::Timeout,
                    "request_id is not (or no longer) in flight; send no request_id for background work")
                    .with_data("retryable", Value::Bool(false)));     // v2 L4
            }
            // §5：先过公平准入（不排队 → busy），再扣令牌——busy 不花令牌。
            let Some(_admitted) = grant.deps.admission.try_admit(&grant.module) else { return Err(busy()) };
            if !grant.limiter.try_acquire() {
                return Err(RpcError::application(ErrorKind::RateLimited, "model call rate limit reached"));
            }
            let profile = TaskProfile { privacy: grant.privacy, complexity };   // 隐私只来自 grant
            let cancel = grant.deps.cancel_root.child_token();                  // v2 M1
            let _cancel_on_drop = cancel.clone().drop_guard();                  // §3.3
            let ticket = UsageTicket::new(grant.deps.usage.clone(), grant.module.clone());  // §6.3
            let served = match bind_to_lifecycle(lifecycle,
                    grant.router.complete_served(profile, &request, &cancel)).await {   // v2 H2
                Err(lt) => return Err(lifecycle_error(lt)),                     // ticket drop → Cancelled
                Ok(Err(e)) => {
                    ticket.finish(match e { ModelError::Cancelled => UsageOutcome::Cancelled,
                                            _ => UsageOutcome::Failed });
                    return Err(map_model_error(&grant.module, &e));              // §7
                }
                Ok(Ok(served)) => served,
            };
            let u = &served.response.usage;
            let (p, c, s) = (u.prompt_tokens, u.completion_tokens, served_of(served.tier));
            if grant.privacy == Privacy::LocalOnly && !served.tier.is_local() {  // §2.2 绊线（L1：只抓 tier_order 回归）
                tracing::error!(/* module, provider */ "LocalOnly model call was served by a non-local tier");
                ticket.finish(UsageOutcome::FailedAfterServe { served: s, prompt_tokens: p, completion_tokens: c });
                return Err(RpcError::internal("the kernel routed this call incorrectly; the result is withheld"));
            }
            let text = served.response.message.content.clone().unwrap_or_default();
            if text.len() > MODEL_MAX_TEXT_BYTES {                               // v2 M3
                ticket.finish(UsageOutcome::FailedAfterServe { served: s, prompt_tokens: p, completion_tokens: c });
                return Err(unavailable(UnavailableCause::ResponseTooLarge,
                    "the model's answer exceeds the size a callback result may carry; lower max_tokens"));
            }
            ticket.finish(UsageOutcome::Ok { served: s, prompt_tokens: p, completion_tokens: c });
            // …序列化 §4.3 的 result
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

## 5. 决策 M5（S2-5 前半）：并发、公平、令牌桶、健康表

### 5.1 三层对象

```rust
/// daemon 级：serve() 里建一次，按值交给 mount_all，mount_all 返回即 drop（§6.3）。
#[derive(Clone)]
pub struct ModelCallbackDeps {
    pub router: Arc<ModelRouter>,            // AppState.router——内核自己的；模块从不直接用它路由（v2 H2）
    pub usage: Arc<dyn UsageSink>,           // §6.3；b1/b2 为 MemoryUsageSink，4.2.3 起为 UsageRecorder
    pub cancel_root: CancellationToken,      // v2 M1：modules_cut_off() 时取消
    pub admission: Arc<ModelAdmission>,      // v2 M2：每模块上限 + 公平的全局上限
}
/// 挂载级：mount_package 里、MethodsFor 闭包外建一次；闭包内只 clone。
#[derive(Clone)]
pub struct ModelGrant {
    pub module: String,
    pub privacy: Privacy,
    pub router: Arc<ModelRouter>,            // v2 H2：deps.router.with_separate_health()——本模块自己的健康表
    pub limiter: Arc<RateLimiter>,           // agentd events_emit::RateLimiter，复用
    pub deps: ModelCallbackDeps,
}
impl ModelGrant {
    pub fn new(module: String, access: ModelAccess, deps: ModelCallbackDeps) -> Self;
    /// v2 L6：同上，令牌桶用注入的时钟（测试）。
    pub fn with_clock(module: String, access: ModelAccess, deps: ModelCallbackDeps, clock: Arc<dyn Clock>) -> Self;
}
```

**（v2 H2）为什么每个模块一张健康表**：冷却是「这个 provider 刚才对我不可用」的记忆。模块能随意诱发 429/5xx（超长 prompt、畸形 schema、打满本地推理），
共享一张表就等于把内核自己的 `/api/v1/chat`、Guardian、会话摘要器的路由交给模块操纵（§1 第 17 条）。独立表之后：模块引起的冷却只影响它自己；
模块之间也互不影响；**代价**是模块不能从内核已知的冷却里受益（会自己再撞一次不可用的 provider，最多多花一次 connect 超时 2s）。

### 5.2 数值（⚖️ = 选的，不是推出来的）

| 常量 | 值 | 理由 |
|---|---|---|
| `MODEL_MAX_IN_FLIGHT_PER_MODULE` ⚖️ | 2 | PLAN 建议值。本地推理基本串行，更多并发只是在 oMLX 里排队、吃掉各自的 120s |
| `MODEL_MAX_IN_FLIGHT_GLOBAL` ⚖️ | 4 | 所有模块合计，带 §5.3 的公平规则。**不含** `/api/v1/chat` 与 agent loop——内核自己的用量不被模块挤占，但也因此不能保证 oMLX 不过载（R3） |
| `MODEL_RATE_CAPACITY` / `MODEL_RATE_REFILL_PER_SEC` ⚖️ | 30 / 0.5 | 突发 30 次、持续 30 次/分钟。按次计，不按 token 计（token 在调用前未知） |
| `MODEL_CALL_TIMEOUT` | 120s | = provider 自己的 `chat_timeout`（`lib.rs:206`）。**（v2 L2 改正方向）**两者几乎同时起算，**RPC 的计时先到点**（它在 handler 之前就开始计），所以模块看到的总是 `timeout` 而不是 provider 的超时；且路由器按序尝试多个 provider——**第一个慢 provider 可以吃完整个 120s**，后面的 provider 轮不到（R11） |
| `MODEL_MAX_TOKENS_CEILING` / 缺省 ⚖️ | 4096 / 1024 | 4096 token 的文本远低于 512 KiB 的结果上限；缺省给个上限，免得没写 `max_tokens` 的模块把 120s 跑满 |
| `MODEL_MAX_MESSAGES` ⚖️ | 64 | 字节已由 dispatch 预算兜住，条数防病态的「一万条空消息」 |
| `MODEL_MAX_TEXT_BYTES` ⚖️ | 512 KiB | v2 M3，见 §4.3 |

- 满了**不排队**：`try_admit` 失败即 `busy`（同回调通道的并发语义，队列是对端控制的内存）。
- **跨重启**：令牌桶在挂载级，**跨 restart generation 不重置**（SPEC §5「限流桶不能被崩溃重置」）；**daemon 重启会重置**——模块触发不了 daemon 重启。
- **与 events 的相反选择，理由**：`_a24/events/emit` 的桶**故意每代重建**（`domain.rs:1454-1458`，Codex round 3 Medium 2：不让重启的模块继承上一代花掉的额度）；
  memory 与本文照 SPEC §5 放在挂载级。模型调用是**贵的**（本地 GPU 时间；远端是钱），若每代重建，一个模块靠自崩溃循环就能每次拿满 30 次突发——
  这正是 SPEC §5 那一条要防的。代价是一个刚重启的模块可能继承空桶，最多等 2s 回填一次。
- 准入守卫（`AdmissionGuard`）是 handler future 里的局部变量：任何一条取消路径 drop 掉 future，槽位就还回去，不会泄漏。

### 5.3 公平的全局上限（v2 M2）

```rust
/// daemon 级。一把锁同时判两个上限，规则是精确的（不是先看后取的竞态）：
/// - 一个模块的第 1 个在途调用：需要 total < GLOBAL；
/// - 它的第 2 个及以后：需要 mine < PER_MODULE 且 total + 1 < GLOBAL（不得占最后一个空闲槽）。
pub struct ModelAdmission { /* global, per_module, Mutex<(total, HashMap<module, n>)> */ }
impl ModelAdmission {
    pub fn new(global: usize, per_module: usize) -> Arc<Self>;
    pub fn try_admit(self: &Arc<Self>, module: &str) -> Option<AdmissionGuard>;   // guard Drop 归还
}
```

性质：「第 2 个及以后」的调用最多占 `GLOBAL − 1` 个槽，所以**两个模块永远占不满 4 个**；只要同时活跃的模块数 < `GLOBAL`，新模块的第一个调用总能拿到槽。
**不保证**的：≥ 4 个模块同时各有 1 个在途时，第 5 个模块会 `busy`（R3）——那时每个模块都只拿着 1 个槽，已经是这个上限下能做到的最公平。
`MODEL_MAX_IN_FLIGHT_PER_MODULE` 的独立信号量因此并入 `ModelAdmission`（v1 的 `module_admission: Arc<Semaphore>` 与 `global_admission` 删除）。

---

## 6. 决策 M7（S2-5 后半）：按模块持久化用量

### 6.1 schema（`agent24-store/migrations/0008_module_model_usage.sql`；ME4-S1 用 0007。**若合并时已有更大的号，后合并方 rebase 时取 max+1**）

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
  SQLite 串行化写者，读改写在同一语句里，并发记录不会丢增量（scratch 在 WAL 文件库上 50 个并发写，合计恰为 50）。
- 读：`module_model_usage(module, since_day)` 按天明细；`module_model_usage_totals(module)` 一次 `GROUP BY served_by` 的全期合计。
- **（v2 M4）store 不认识「调用怎么结束」**：它只收一个增量，与 agentd 的 `UsageOutcome` 无共享类型，4.2.2b1 与 4.2.3 因此互不依赖。

```rust
// agent24-store/src/module_model_usage.rs（scratch 已 check + test）
pub enum ServedBy { Local, Remote, None }
#[derive(Default)]
pub struct ModelUsageDelta { pub calls_ok: u64, pub calls_failed: u64, pub calls_cancelled: u64,
                             pub prompt_tokens: u64, pub completion_tokens: u64 }
pub struct ModelUsageRow { pub day: String, pub served_by: String, pub calls_ok: u64, pub calls_failed: u64,
                           pub calls_cancelled: u64, pub prompt_tokens: u64, pub completion_tokens: u64 }
impl Store {
    pub async fn record_module_model_usage(&self, module: &str, day: &str,
                                           served_by: ServedBy, delta: ModelUsageDelta) -> Result<()>;
    pub async fn module_model_usage(&self, module: &str, since_day: &str) -> Result<Vec<ModelUsageRow>>;
    pub async fn module_model_usage_totals(&self, module: &str) -> Result<Vec<ModelUsageRow>>;
}
```

### 6.2 计量口径

| 调用结局 | 计不计 | 记在哪 |
|---|---|---|
| 成功 | 计 | `served_by = local/remote`，`calls_ok += 1`，token = provider 回报值（没回报就是 0） |
| **（v2）** provider 答了、内核拒绝转交（结果超过 512 KiB、LocalOnly 绊线） | 计 | **所属层的行**，`calls_failed += 1`，token 照记（已经花了） |
| 路由器返回 `Unavailable` / `Rejected` / `Provider` | 计 | `none` 行 `calls_failed += 1`，token 0 |
| 进了路由器后被取消（`$/cancelRequest`、连接断、撤销、方法超时、所绑请求结束、取消根） | 计 | `none` 行 `calls_cancelled += 1`，token 0 |
| 被挡在路由器之前（`-32602`、`forbidden`、`not_ready`/`draining`/`revoked`、`request_id` 不在途、`busy`、`rate_limited`） | **不计** | ——它们没有用到任何模型 |

「失败与取消也计」的理由：远端 provider 可能已经为一次失败/被取消的请求计费；次数是能知道的，token 不能——所以计次数、token 记 0、**不猜**。
「挡在前面的不计」：这张表回答「用了多少模型」，不回答「被拒了多少次」（后者是日志与限流的事）。

### 6.3 什么时候写 —— `UsageSink`，调用方从不等磁盘（v2 M1 / M4）

```rust
// agentd/src/model_callback.rs —— 4.2.2b1
pub enum Served { Local, Remote }
pub enum UsageOutcome {
    Ok { served: Served, prompt_tokens: u64, completion_tokens: u64 },
    FailedAfterServe { served: Served, prompt_tokens: u64, completion_tokens: u64 },
    Failed,
    Cancelled,
}
/// 同步、非阻塞是契约：会在 Drop 里被调用，绝不 await、绝不 spawn。
pub trait UsageSink: Send + Sync { fn record(&self, module: &str, outcome: UsageOutcome); }
#[derive(Default)] pub struct MemoryUsageSink(/* Mutex<Vec<(String, UsageOutcome)>> */);   // b1/b2 与全部 handler 测试
struct UsageTicket { /* sink, module, done */ }   // finish(outcome) 记一笔；未 finish 就 Drop → Cancelled

// agentd/src/usage_recorder.rs —— 4.2.3
pub struct UsageRecorder { /* tx: mpsc::Sender<UsageRecord>（容量 1024 ⚖️）, dropped: AtomicU64 */ }
impl UsageRecorder {
    pub fn spawn(store: Store, hard_stop: impl Future<Output = ()> + Send + 'static)
        -> (Arc<Self>, tokio::task::JoinHandle<()>);
    pub fn dropped(&self) -> u64;
}
impl UsageSink for UsageRecorder { /* try_send；满 → dropped += 1 + warn */ }
```

- `UsageTicket` 在确定进路由器时建；**每次进了路由器的调用恰好一笔**由类型结构保证。`Drop` 里只有同步的 `record`，不违反 `Handler` 契约「调用的全部工作必须在返回的 future 里」（SPEC ME-3c 表）。
- **写者的生命周期（v2 M1）**：
  - 正常结束 = **通道关闭**：sender 只存在于 `ModelCallbackDeps`（`serve` 按值交给 `mount_all`，返回即 drop）与各 `ModelGrant`（活在 `MethodsFor` 闭包里，闭包随 supervisor 拆除而 drop）。
    最后一个 sender 消失时，队列里已有的记录全部写完，任务返回。
  - 硬上限 = `hard_stop`：`serve` 传入「停机 token 取消后，`sleep_until(shutdown.deadlines().modules)`」，即 cut-off + `CONFIRM`（200ms）。
    到点即关通道、丢弃剩余并 `warn!(lost = n)`。**不新增停机预算**：`deadlines().modules` 本来就在 `persist`/`watchdog` 之前（`lifecycle.rs:104-116`），默认 2s 的退出上限不变。
  - `serve` 在停机序列里等写者的 `JoinHandle`，与等 supervisor 并列，上限同一个 `deadlines().modules`。
- 写失败 `error` 日志，不重试。

### 6.4 费用字段怎么填

**不填 0**。今天 `Usage.cost_usd` 恒 `0.0`（§1 第 1 条），provider 不回报费用、内核没有价目表——一个恒为 0 的「费用」在远端调用上是**错误的事实陈述**。
所以表里**没有费用列**，`GET /api/v1/usage?module=` 的 `cost_usd` 恒为 **`null`**（含义：未知，不是免费）。将来有价目表时加一列 + 迁移，`null` 变成真数。
不带 `module` 的全局响应里那个恒 `0.0` 的 `cost_usd` 同样是错误陈述，但改它就改了 `/api/v1/usage` 的既有输出——登记 **FU-72**，不在本轮改。

### 6.5 `GET /api/v1/usage?module=<name>`

- **（v2 L5）读 `RawQuery`，只看 `module` 键**（`serde_urlencoded` 宽松解析成键值对）：
  - 没有 `module` 键 → **原样**返回 `/chat` 的全局内存计数器（`routes.rs:52-54` 的形状与语义一字不变），**无论 query 其余部分多畸形**（`?%zz`、`?a=1&a=2` 都照旧 200）；
  - 恰好一个 → 模块用量；多个 → `400 invalid_request`（JSON 信封，不是 axum 的纯文本 400）。
  - 模块调用**不**加进全局计数器（它的含义是「本次启动以来 `/api/v1/chat` 的用量」）。
- 名字过 `agent24_domain::is_valid_module_name`，不合法 → `400 invalid_request`；合法但从没调用过（或已卸载）→ 200 + 全 0。
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

**（v2 L3）`unavailable` 的 `data` 是两个固定字段**：`retryable: bool` 与 `cause`，`cause` 是闭集（`UnavailableCause`）：

| `cause` | 来源 | `retryable` | 模块该做什么 |
|---|---|---|---|
| `no_provider` | `ModelError::Unavailable`（LocalOnly 空路由、429、5xx、连不上、provider 超时） | `true` | 稍后重试 / 走规则兜底 |
| `request_rejected` | `ModelError::Rejected { status: 400 \| 408 \| 409 \| 413 \| 422 }` | `false` | 改请求（缩短 prompt、改 schema） |
| `backend_config` | `Rejected { 401 \| 403 \| 404 \| 其余 4xx }`、`ModelError::Provider(_)`（坏 JSON、超大响应体、无 choices） | `false` | 不要重试；告诉用户后端配置有问题 |
| `response_too_large` | 结果 `text` > 512 KiB（§4.3） | `false` | 调小 `max_tokens` |

完整映射：

| 来源 | wire | 文案（**不含** provider 名/URL/原文） | `data` |
|---|---|---|---|
| `ModelError::Unavailable(_)` | `-32000` `unavailable` | 「no model this module may use is available right now」 | `retryable:true, cause:no_provider` |
| `ModelError::Rejected{..}` | `-32000` `unavailable` | 「the model backend refused this request」 | `retryable:false, cause:request_rejected\|backend_config` |
| `ModelError::Provider(_)` | `-32000` `unavailable` | 「the model backend failed this request」 | `retryable:false, cause:backend_config` |
| 结果过大 | `-32000` `unavailable` | 「…exceeds the size a callback result may carry; lower max_tokens」 | `retryable:false, cause:response_too_large` |
| `ModelError::Cancelled`（取消根，即 cut-off） | `-32000` `cancelled` | 「the daemon is shutting down」 | — |
| `LifecycleTimeout::{BudgetExhausted, RequestEnded}` | `-32000` `timeout` | 同 memory 的两条（`os_memory_page.rs:218-231`） | — |
| `request_id` 不在途（§3.4） | `-32000` `timeout` | 「request_id is not (or no longer) in flight; …」 | **`retryable:false`**（v2 L4） |
| 方法超时 | `-32000` `timeout`（`serve` 生成） | 「the kernel gave up after 120000ms; …」 | — |
| `$/cancelRequest` | `-32000` `cancelled`（`serve` 生成） | 既有 | — |
| 未授予 | `forbidden` | 「this module was not granted model access」 | — |
| 准入拒绝 | `not_ready` / `draining` / `revoked` | `refused_error`（`events_emit.rs:273`） | — |
| 并发满 / 令牌空 | `busy` / `rate_limited` | 静态 | — |
| params | `-32602` | serde / `validate` 的原文（含合法范围） | — |
| LocalOnly 绊线 / 结果不可序列化 / call 时参数反解失败 | `-32603` | 静态 | — |

- provider 细节只进 daemon 日志（`tracing::warn!(module, status, detail)`），同 `map_memory_error` 的默认拒绝。
- 对 `local_only` 模块，「只配了远端」与「本地全挂」给**同一句**（都是 `no_provider`）：模块不该能从错误里推断 daemon 有没有远端。
- 闭集扩展的同步改动（ME4-4.2.2-0，一个 PR 内）：`ErrorKind` 加变体 + `ALL` + `as_str`；`rpc.rs` 测试 `the_error_kinds_are_exactly_specs_closed_set` 里的 SPEC 句子拷贝；
  SPEC §3「错误形状」那一句（本分支已改）。scratch 已按改后的句子跑过该测试。**ME4-S1 的判据 C5.9 断言 `ALL.len() == 17`**，谁后合谁把它改成「不因调度而变」（§10.3）。

---
## 8. 判据（每条带正对照；`cargo test <过滤>` 一律先 `-- --list` 断言非空，PLAN §一 第 5 条）

> 编号 J1–J18。「变异」= `docs/agent/mutate.sh` 的改回方式，改回后该判据必须变红。括号里是所属任务（切法见 §10.2）。

**J1 manifest（4.2.1，`cargo test -p agent24-domain model_access`）**：缺省 → `LocalOnly`；`[models]` + `remote_allowed` → `RemoteAllowed`；
显式 `local_only` → `LocalOnly`（正对照）；`model_access: remote` → 错误且消息同时含 `"remote"` 与 `remote_allowed`；`[events]` + `remote_allowed` → 错误且消息含 `models`；
`model_acess:` 拼错 → 错误；**（v2 L8）`[events]` + `model_access: ~` → 接受且 `LocalOnly`；`[]` + `model_access: local_only` → 拒**。
变异：删掉「未请求 models」那条检查 → 第 5 条变红。（scratch `manifest_tests` 已跑通全部格。）

**J2 授予与注册（4.2.2b2，`-p agent24d model_grant`）**：未请求 `models` 的模块调 `_a24/model/complete` → `forbidden`（**不是** `-32601`）；其 `offer.provides` **不含** `_a24/model/`；
请求了 `models` 的模块 → `provides` 含 `_a24/model/`、`MountReport.granted` 含 `models`（正对照）。`models = None` 的挂载 → 即使请求了也不出现在 `granted`/`provides`（invariant #134）。
**4.2.2b1 合入后、b2 合入前**：生产 `Methods` 里**没有** `_a24/model/complete`（`-32601`），`provides` 不含 `_a24/model/`——结构测试断言 b1 的 `domain.rs` 不引用 `ModelCompleteHandler`。

**J3 LocalOnly 负对照（4.2.2b1，`-p agent24d model_callback`）**：手建 `ModelRouter::new(vec![(远端桩, Tier::Remote)])`，`local_only` 模块调用 →
`unavailable`、`retryable:true`、`cause:"no_provider"`、**远端桩收到的请求数 = 0**、消息不含桩名；**正对照**：同一路由器、`remote_allowed` 模块 → 成功、`tier:"remote"`、桩计数 1。
params 里放 `privacy`/`model`/`tools` → `-32602` 且桩计数 0；`_meta: {privacy:"any", tier:"remote"}` → 仍 `unavailable`、桩计数 0。
变异：`ModelGrant` 里把 `LocalOnly` 映射成 `Privacy::Any` → 桩计数变 1。（scratch 已跑通。）

**J4 契约（4.2.2a，`-p agent24-models max_tokens`、`-p agent24-models model_id` 两条命令）**：真 TCP 桩记录请求体——`max_tokens: Some(77)` → 体里 `"max_tokens": 77`；
**正对照** `None` → 体里没有这个键。桩回 `"model": "stub-actual-7b"` → `model_id == Some(..)`，≠ provider 名、≠ 请求名；桩不回 `model` → `None`。
**/chat 零变化**：经真实 `post_chat` 打桩，请求体键集合 == `{model, messages, stream}`；桩回 401 时 `/chat` 的错误 JSON 与改动前逐字节相同（`Rejected` 与 `Provider` 同文）。（scratch `wire_tests` 已跑通前两部分；401/404/403 → `Rejected{status}` 的断言在 models 的既有测试里改写并通过。）

**J5 按方法超时（4.2.2-0，`-p agent24-os-proto per_method_budget`）**：`Limits.call_timeout = 100ms`；声明 800ms 的 handler 睡 300ms → 成功；
**正对照**：不声明 → `timeout` 且文案含 `100ms`；声明 400ms、睡 5s → `timeout` 且文案含 `400ms`。`effective_call_timeout`：`None → default`、`120s → 120s`、`1h → 300s`、`5ms → 5ms`。
既有 rpc 测试全绿（回归）。变异：`on_frame` 改回读 `self.limits.call_timeout` → 第一条变红。（scratch：rpc 53 个测试全绿。）

**J6 字面 30s（4.2.2b1，`-p agent24d model_callback_outlives_call_timeout`）**：真 `serve()` + `Limits::default()` + 进程内桩 provider 睡 31s → 成功。
正对照由 J5 的缩放版承担；变异：`ModelCompleteHandler::call_timeout` 返回 `None` → 30s 处 `timeout`。（单独 ~31s，只此一条。）

**J7 取消到达 provider（4.2.2b1，`-p agent24d model_callback_cancel`；断言对象是 `MemoryUsageSink`，v2 M4）**：provider = 指向 Rust TCP 桩的真 `OpenAiCompatProvider`，桩收完请求后不回，记录「对端是否在 N 秒内关闭连接」。经真 `serve()`（duplex）：
- (a) `$/cancelRequest` → 模块收到 `cancelled`；桩 1s 内观察到关闭；sink 得到一笔 `Cancelled`。
- (b) 模块一侧关闭回调连接 → 无响应；桩 1s 内观察到关闭；sink 一笔 `Cancelled`。
- (c) 带一个 `admit_request` 得到的在途 id，对该 `InFlight` 调 `finish()` → `timeout`（「already ended」）；桩 1s 内观察到关闭；sink 一笔 `Cancelled`。
- (d) **（v2 M1）取消 `deps.cancel_root`**（生产中由 `modules_cut_off()` 触发）→ `cancelled`（「shutting down」）；桩观察到关闭；sink 一笔 `Cancelled`。
- **正对照**：同一搭建、不触发任何一条，桩在 2s 后应答 → 成功、桩没有观察到提前关闭、sink 一笔 `Ok`。
（scratch 已跑通：(d) 用真 provider + TCP 桩 `cancel_root_reaches_the_real_provider_and_answers_cancelled`；drop 路径用 `dropping_the_call_future_cancels_the_provider_token_and_records_cancelled`。）

**J8 生命周期绑定（4.2.2b1，`-p agent24d model_callback_lifecycle`）**：Running、不带 `request_id` → 执行（正对照）；带从未存在的 id → `timeout` + `retryable:false` 且桩计数 0；
带刚 `finish()` 的 id → 同上。Draining：不带 id → `draining`；带在途 id → 执行；Revoked → `revoked`。变异：删掉 §3.4 的检查 → 第二条变成「执行」。（scratch 已跑通前两条。）

**J9 并发与公平（4.2.2b1，`-p agent24d model_callback_busy`）**：
- 同一模块 2 个挂起调用在途，第 3 个 → `busy`。
- **`busy` 不花令牌**：冻结时钟、容量 3——c1、c2 挂起（剩 1），c3 `busy`，abort c1，c4 → **成功**；变异：把「扣令牌」挪到「准入」之前 → c4 变 `rate_limited`。
- **（v2 M2）公平**（`ModelAdmission::new(4, 2)` 单测 + handler 级各一遍）：A 两个、B 一个在途时，B 的第二个 → `busy`，C 的第一个 → 执行；A、B 各尝试两个时合计只成 3 个，C 仍能进。
  正对照：没有公平规则（变异成「只看 total < GLOBAL」）→ A、B 各 2 个占满 4 个，C `busy`，红。（scratch `fair_global_admission`、`busy_spends_no_token` 已跑通。）

**J10 令牌桶与跨代（4.2.2b1/b2，`-p agent24d model_callback_rate`）**：`ModelGrant::with_clock` 冻结时钟，第 31 次 → `rate_limited`；时钟前进 2s → 成功（正对照）。
**跨 restart generation**（b2）：同一次挂载的 `MethodsFor` 闭包调两次（两代），第一代耗尽后第二代首调 → `rate_limited`；正对照：另一个模块首调成功。变异：把 `ModelGrant` 构造挪进闭包 → 第二代成功。

**J11 计量落库（4.2.3，`-p agent24d usage_by_module`）**：两个模块各成功一次 → 各自 `calls_ok = 1`、互不串；**关闭 daemon 并以同一数据文件重开 → 仍在**（重启不清零；v2 L9 删去「或 Store」）；
一次 `unavailable` → `none.calls_failed = 1`；一次被取消 → `none.calls_cancelled = 1`；一次结果过大 → **所属层行** `calls_failed = 1` 且 token 非 0；
一次 `forbidden`、一次 `busy`、一次 `rate_limited` → **不**增加任何计数（正对照：同批成功的那次增加了）。
store 层：WAL 文件库上 50 个并发记录 → 合计恰为 50；token 饱和到 `i64::MAX`；`none` 行带 token 被 CHECK 拒。
写者：所有 sender drop 后写完队列并退出（正对照：还有 sender 时只有 `hard_stop` 能让它退出）。等待落盘用轮询（≤ 5s）。（scratch store 3 个 + recorder 2 个测试已跑通。）

**J12 用量 API（4.2.3，`-p agent24d usage_by_module_api`）**：不带 `module` → 与改动前**逐字节**相同；**（v2 L5）`?%zz`、`?a=1&a=2` → 同样逐字节相同的 200**；
`?module=../x` → 400；`?module=never_called` → 200、全 0、`by_served` 三键齐全、`cost_usd: null`；`?module=a&module=b` → 400 `invalid_request` JSON。（scratch `module_selector` 单测已跑通前三类。）

**J13 错误映射（4.2.2b1 + 4.2.2-0）**：桩名 `stub-SECRET`、URL 含 `secret-host`：`Unavailable` → `unavailable` + `retryable:true` + `cause:no_provider`，整个 `error` JSON 不含 `SECRET`/`secret-host`；
桩回 400 → `cause:request_rejected`；401 → `cause:backend_config`；两者 `retryable:false`，同样不含。闭集测试的 SPEC 拷贝含 `unavailable`；变异：从拷贝删掉 `unavailable` → 红。

**J14 黑盒（4.3.1，`--test me4_model_blackbox`，连跑 10 次；v2 M5 加 LocalOnly 负对照）**：两个 Python `http.server` 桩（python3 缺失即失败）：
本地桩绑 `127.0.0.1:<p1>`、`OMLX_URL=http://127.0.0.1:<p1>`（`Local`）；远端桩绑 `0.0.0.0:<p2>`、**`OLLAMA_URL=http://0.0.0.0:<p2>`**（§2.3 判 `Remote`，实际连回本机）。
两个仓外 Python 模块：`m_local`（`[models]`，缺省 `local_only`）、`m_remote`（`[models]` + `remote_allowed`）。
- 正常：`m_local` 不带 `request_id` 调一次 → `tier:local`、`model_id` = 本地桩回的 id；远端桩计数 0。
- **负对照**：本地桩切到「回 503」→ `m_local` → `unavailable`/`no_provider`，**远端桩计数仍为 0**。
- **正对照**：`m_remote` + `complexity:"complex"` → `tier:"remote"`、远端桩计数 1。
- `GET /api/v1/usage?module=m_local` 计数与上面一致；重启 daemon 后仍在。
- 本地桩对下一次调用挂起，期间 `agent24 os disable m_local` → 本地桩观察到连接关闭（撤销路径的真实版）。
- 未请求 `models` 的第三个模块 → `forbidden`（正对照）。另留 `#[ignore]` 的真 oMLX 冒烟。

**J15 LocalOnly 绊线（4.2.2b1）**：正常路由下不可达，用变异测：把 `tier_order` 的 `LocalOnly` 分支加上 `Tier::Remote` → J3 的「桩计数 0」变红、**且**模块收到 `-32603`。
（它只证明绊线接上了；打错标签不在它的能力范围内，见 J16。）

**J16 Tier 判定（4.2.2a，`-p agent24-models env_local_tier`；v2 H1）**：变体矩阵——
`Local`（正对照）：`http://127.0.0.1:8088`、`http://localhost:8088`、`http://LOCALHOST:8088`、`https://[::1]:8443`、`http://user:pw@127.0.0.1:8088`、`http://evil.example%2F@127.0.0.1:8088`（`%2F` 留在 userinfo）、`http://0x7f000001:8088`（WHATWG 规范化为 127.0.0.1）、`http://loc<TAB>alhost:8088`（制表符被剥）、`http://127.0.0.1:8088?@evil.example`；
`Remote`：**`http://evil.example\@127.0.0.1:8088`**（H1 原向量）、`http://evil.example:80\@localhost/`、`http://127.0.0.1<TAB>.evil.example:8088`、`http://[::ffff:127.0.0.1]:8088`、`http://0.0.0.0:8088`、`http://localhost.:8088`、`http://192.168.1.50:8088`、`https://inference.example.com`、`not a url`、`file:///tmp/x`、空串。
另断言 `reqwest::Url::parse(H1 向量).host_str() == Some("evil.example")`——证明量具（判定器与 HTTP 客户端）看到的是同一个 host。
变异：换回手写 `url_host` → H1 向量变红。（scratch 已跑通，models 27 个测试全绿。）

**J17 健康表隔离（4.2.2b1，`-p agent24d model_callback_health`；v2 H2）**：内核路由器 = `[a(Local), b(Local)]`；`a` 对模块调用回不可用（500）→ 模块调用经 `b` 成功；
随后 `a` 恢复，内核路由器（`/api/v1/chat` 用的那一个）的下一次调用**打到 `a`**（`a` 计数 +1）。**负对照**：同一序列走**同一个**路由器 → 第二次调用跳过 `a`（`a` 计数不变）。
变异：`ModelGrant` 里用 `deps.router` 而不是 `with_separate_health()` → 红。（scratch `module_failures_do_not_cool_down_the_kernels_router` 已跑通，含负对照。）

**J18 结果大小（4.2.2b1；v2 M3）**：TCP 桩回 2 MiB 的 `content` → `unavailable` + `retryable:false` + `cause:"response_too_large"`，连接不断、下一次调用正常；
sink 一笔 `FailedAfterServe` 且 token 非 0。正对照：恰好 512 KiB → 成功。变异：删掉检查 → 得到 `-32603`（帧上限），红。（scratch 用进程内桩跑通 512 KiB+1 / 512 KiB 两格。）

---

## 9. 自审

1. **(a) 是否真的「不调大全局 `CALL_TIMEOUT`」**：是。改的是「谁决定这次调用的预算」，默认值与所有既有方法不变；`MAX_METHOD_CALL_TIMEOUT` 保证没有方法能无界；
   （v2 L7）文档写明声明长预算的方法必须自己限并发，模型方法照做（§5.3）。
2. **「统一桥接」有没有夸大**：§3.3 如实写了四条靠 drop 整个 handler future、一条 drop 内层 work、一条靠取消根 token。
3. **LocalOnly 是不是单点**：强制点仍是 `tier_order` 一处；**v2 承认它的前提（标签）在 v1 时是坏的**（H1），修法是让判定与连接用同一个解析器，并把判不准的都判成远端。
   绊线不能替代它（L1）。`ModelRouter::new` 手建路由器的调用方仍须自己保证标签诚实——那是既有契约（`router.rs` 的 PRIVACY CONTRACT 注释）。
4. **模块能不能影响别人**：v1 漏了共享健康表（H2）；v2 每模块一张表，并发有公平规则（M2），限流、计量都按模块。剩下的共享资源是 provider 本身（本地 GPU），R3。
5. **比 memory 更严的一处（`request_id` 不在途即拒）**：会拒掉「请求刚结束时发出的补全」——这正是要拒的；现在还明确 `retryable:false`。
6. **计量不阻塞、不丢在正常停机里**：`try_send` + 单写者；正常结束靠通道关闭写完队列，硬上限落在既有的 `deadlines().modules` 上，不延长停机。
7. **没有复制 FU-70**：`admit_callback_bound` + `bind_to_lifecycle`。
8. **offer set 阶梯**：b1 不注册、b2 一次跨越（§2.4/§10.2）。
9. **片段都编过**：附录 A。scratch 是真实 crate 的拷贝打补丁；`agentd` 是 binary crate，handler 所需的 `RateLimiter`/`refused_error`/`Clock` 按原形状复制，
   挂载与 `serve()` 接线写成纯函数（`mount_sketch.rs`：`model_grant`、`provides_model`、`with_model_method`、`spawn_cancel_root`）。

---

## 10. 接口清单、切法与合并

### 10.1 接口清单

| crate / 文件 | 新增或改动 | 任务 |
|---|---|---|
| `agent24-domain/src/lib.rs` | `ModelAccess` + `ALL`/`as_str`/`parse`；`RawManifest.model_access: Option<String>`；`DomainOsManifest::model_access()`；解析期两条校验 | 4.2.1 |
| `agent24-models/src/lib.rs` | `CompletionRequest.max_tokens: Option<NonZeroU32>`；`CompletionResponse.model_id: Option<String>`；`OaChatResponse.model`；条件写 `max_tokens`；**`ModelError::Rejected { status, message }`**，`friendly_http_error` 对 4xx（非 429）返回它 | 4.2.2a |
| `agent24-models/src/router.rs` | `Served { provider, tier, response }`；`complete_served`；`complete` 改为投影；**`with_separate_health`**；**`is_loopback_url` + 新 `env_local_tier`**（删 `url_host`/`is_loopback_host`）；**`OLLAMA_URL`** | 4.2.2a |
| 工作区其它 crate | 15 处结构体字面量补字段；`agent24-agent/src/lib.rs:1026-1031` 与 `agentd routes.rs:140-152` 两处 `ModelError` 穷尽 `match` 加 `Rejected` 分支（与 `Provider` 同处理） | 4.2.2a |
| `agent24-os-proto/src/rpc.rs` | `Handler::call_timeout`（默认 `None`，文档含并发警示）；`MAX_METHOD_CALL_TIMEOUT`；`effective_call_timeout`；`Conn.by_task` 带时长；`on_frame`/`finished`；`ErrorKind::Unavailable`（`ALL` 18）；闭集测试的 SPEC 拷贝 | 4.2.2-0 |
| `agentd/src/model_callback.rs`（新，`main.rs` 加 `mod model_callback;`） | §5.2 常量、wire 类型、`ModelCallbackDeps`、`ModelGrant`（`new`/`with_clock`）、`ModelAdmission`/`AdmissionGuard`、`ModelCompleteHandler`、`UnavailableCause`、`map_model_error`、`Served`/`UsageOutcome`/`UsageSink`/`MemoryUsageSink`/`UsageTicket` | 4.2.2b1 |
| `agentd/src/domain.rs` | `KERNEL_OOP_GRANTS` 加 `Models`；`mount_all`/`mount_package` 加 `models: Option<ModelCallbackDeps>`；闭包外建 `ModelGrant`；`provides`/`granted_names`；注册 `_a24/model/complete` | 4.2.2b2 |
| `agentd/src/server.rs` | `serve()`：`spawn_cancel_root(shutdown.modules_cut_off())`、`ModelAdmission::new(4, 2)`、sink（b2 为 `MemoryUsageSink`，4.2.3 换成 `UsageRecorder::spawn(store, hard_stop)` 并在停机序列里等它）、按值传给 `mount_all` | 4.2.2b2 / 4.2.3 |
| `agent24-store` | 迁移 `0008_module_model_usage.sql`（合并时若已有更大号则 max+1）；`module_model_usage.rs`：`ServedBy`、`ModelUsageDelta`、`ModelUsageRow`、三个 `Store` 方法 | 4.2.3 |
| `agentd/src/usage_recorder.rs`（新） | `UsageRecorder`（`spawn`、`dropped`、`impl UsageSink`） | 4.2.3 |
| `agentd/src/routes.rs` | `get_usage` 改读 `RawQuery` + `module_selector`；`ModuleUsageResponse`/`UsageCounts`/`DailyUsage` | 4.2.3 |
| `docs/specs/SPEC-ME3-OUT-OF-PROCESS.md` | §3 offer set 首句 + ME-4b 注、错误闭集句、「超时」行、方法表行；§5「模型回调」段；§8 ME-4b 行与 3c 表超时行注；§9 | **本分支已改** |
| `docs/agent/followups.md` | FU-71（H1 现有缺陷）、FU-72（全局 `cost_usd: 0.0`）、FU-73（`model_access` 不在 `/api/v1/os`）、`ME4-CODEX-DEBT` 一行 | **本分支已改** |

### 10.2 交给 PLAN 的切法（开工前先改 PLAN §三 与 `tasks.md` 台账；本文不改 PLAN）

| 任务 | 内容 | 依赖 | 验收（判据） | 与原 PLAN 的差异 |
|---|---|---|---|---|
| **ME4-4.2.1** manifest 字段 | 只做 `agent24-domain` 的 `ModelAccess` 与校验 | 4.1.1 冻结 | J1 | **收窄**：原来还含 `KERNEL_OOP_GRANTS`/`provides` 与 `-p agentd model_grant`——挪到 b2。**不再改 `mount_package`，不必排在 ME4-1.5.1 之后**，可与 M1 并行 |
| **ME4-4.2.2-0**（新） 回调通道按方法超时 + `unavailable` | `rpc.rs` 的 §3.2 六处 + `ErrorKind::Unavailable` + 闭集测试拷贝 + SPEC 闭集句 | 4.1.1 冻结 | J5、J13 的闭集部分 | 新增。只动 `agent24-os-proto`，可与 4.2.2a、M1 并行；与 ME4-S1 的交集见 §10.3 |
| **ME4-4.2.2a** 模型契约 + 标签修复 | §4.1 全部 + §2.3（H1）+ `with_separate_health` + `OLLAMA_URL` + `Rejected` | 4.1.1 冻结 | J4、J16（及 models 既有测试改写） | **扩大**：加入 H1（修现有缺陷，关 FU-71）、H2 的路由器侧、`Rejected` |
| **ME4-4.2.2b1**（新拆） handler | `model_callback.rs` 全部（不注册、不授予）；`MemoryUsageSink` | 4.2.1、4.2.2-0、4.2.2a | J3、J6、J7、J8、J9、J10（前半）、J13、J15、J17、J18 | 由原 4.2.2b 拆出；**不改 `domain.rs`**，因此不依赖 ME4-1.5.1 |
| **ME4-4.2.2b2**（新拆） 授予与接线 | `KERNEL_OOP_GRANTS += Models`、`mount_all`/`mount_package` 参数、`provides`、注册、`serve()` 构造 deps（sink 仍为内存） | 4.2.2b1、**ME4-1.5.1**（两条回调都改 `mount_package`） | J2、J10（跨代） | **offer set 阶梯只由它跨越** |
| **ME4-4.2.3** 按模块用量 | 迁移、store 方法、`UsageRecorder`、`serve()` 换 sink 并在停机序列等写者、`/api/v1/usage?module=` | 4.2.2b2 | J11、J12 | 落库断言从 4.2.2b 挪来 |
| **ME4-4.3.1** 黑盒 + 探针 | `me4_model_blackbox.rs`（J14，含 LocalOnly 负对照）、`me3-status.sh` 的 `4b 推理回调` | 4.2.3 | J14 | 黑盒加负对照（v2 M5） |

规模估计（均 ≤ 300 行量级，不含测试）：4.2.1 ~60；4.2.2-0 ~60；4.2.2a ~150；4.2.2b1 ~280；4.2.2b2 ~80；4.2.3 ~250；4.3.1 以测试为主。

### 10.3 与 ME4-1.1.1（调度，`docs/design/ME4-S1-scheduler-callback.md`）的合并点（v2 M7 补全代码层）

**解决原则：两边的增量都保留；删减取交集；后合并方负责 rebase 并跑两边的判据。**

| 位置 | ME4-S1 做什么 | 本文做什么 | 合并后应为 |
|---|---|---|---|
| SPEC §3 offer set 首句 | 加 `Scheduler` | 「ME-3 为 {…}；ME-4b 起为 {Memory, Events, Approval, Models}」 | 两条增量都在：「ME-3 为 `{Memory, Events, Approval}`；ME-4a 起加 `Scheduler`，ME-4b 起加 `Models`」 |
| SPEC §3 方法表末尾 | `_a24/scheduler/*` 行 | `_a24/model/complete` 行 | 两组行都在（两边的 grep 验收各自命中） |
| SPEC §3 错误闭集句 | 不扩展（C5.9） | 加 `unavailable` | 含 `unavailable` 的句子；**ME4-S1 的 C5.9 改为「调度不改变闭集」**（不再断言 `== 17`） |
| SPEC §8 交付表 | ME-3g 后加 ME-4a 行 | ME-3g 后加 ME-4b 行 | 两行，按 4a、4b 顺序 |
| SPEC §9 | 去掉 `Scheduler` | 去掉 `Models`（v2 M6 措辞） | 「不做 `Policy`」+ 两边各自的「不做」补充条 |
| `KERNEL_OOP_GRANTS`（`domain.rs:93-94`）及其文档注释 | `+= Scheduler`，改写「Narrower than KERNEL_GRANTS」注释 | `+= Models` | `[Events, Approval, Memory, Scheduler, Models]`；注释两边的理由都保留 |
| `mount_all`/`mount_package` 参数表（`domain.rs:875`、`1275`）与**全部调用点和测试**（`mount_package` 唯一调用点 `domain.rs:1054`；`mount_all` 在 `server.rs:1250`、`server.rs:1956` 与 `domain.rs` 测试里多处——实现时 `grep -n 'mount_all(' rust/apps/agent24d/src` 逐个补） | `scheduler: &Arc<Scheduler>` | `models: Option<ModelCallbackDeps>` | 两个参数都加；调用点逐个补齐（`#[allow(clippy::too_many_arguments)]` 已在） |
| `provides` 块（`domain.rs:1413-1423`） | push `_a24/scheduler/` | push `_a24/model/`（仅当持有 grant） | 两个 push 都在，互不嵌套 |
| `MethodsFor` 闭包体（`domain.rs:1424-1537`） | 注册三个 scheduler 方法，闭包外建令牌桶 | 注册 `_a24/model/complete`，闭包外建 `ModelGrant` | 两组 `.with(..)` 都在；两个闭包外对象都 clone 进闭包 |
| `server.rs` `serve()` | tick 循环挪到 `mount_all` 之后、`OnceLock`、投递泵 | 构造 `ModelCallbackDeps`（取消根、准入、sink）、4.2.3 起等写者 | 两段都在；本文的构造放在 `mount_all` **之前**（它是参数），调度的启动放在之后 |
| `ErrorKind::ALL` 与闭集测试的 SPEC 拷贝（`rpc.rs:155`、`rpc.rs:1937-1953`） | 不动 | 17 → 18 | 18；拷贝含 `unavailable` |
| `Handler` trait（`rpc.rs:326`） | （若调度也要按方法超时）复用本文的 `call_timeout` | 加 `call_timeout` | 一个默认方法，谁先合谁定义，另一方只 `impl` |
| 迁移号 | 0007 | 0008 | **后合并方 rebase 时迁移号取 max+1**；两者无表冲突 |
| `docs/agent/me3-status.sh` 探针 | `4a 调度回调` | `4b 推理回调` | 两行 |
| `docs/agent/followups.md` | （若新增 FU） | FU-71/72/73 + CODEX-DEBT 一行 | **编号冲突时后合并方顺延编号**，并同步改本文 §12 与各自设计里的引用 |

---

## 11. 已接受残余风险

| # | 风险 | 为什么接受 / 谁接 |
|---|---|---|
| R1 | 同步调用最长占一个回调槽 120s；推理结果在连接断开后取不回 | (a) 的固有代价（§3.1）；每模块 2 并发把它压住；需要「提交后离开」的模块用自己的 outbox |
| R2 | 被代理请求内联推理受 30s 代理总时限、fired 内联推理受投递超时约束 | 由 §3.4 的使用指引（先应答、再后台）承担，写进 SPEC 方法表与 T14 wire 文档 |
| R3 | 全局 4 并发不含 `/api/v1/chat` 与 agent loop，本地 oMLX 仍可能过载；**≥ 4 个模块同时活跃时第 5 个 `busy`**（v2 M2） | 内核自己的用量不该被模块挤占；公平规则已保证两三个模块占不满；更多模块同时推理是本机算力问题，不是分配问题 |
| R4 | 用量记录在队列满、或停机硬上限（cut-off + 200ms）后仍排队时会丢；取消的远端调用费用未知 | `dropped` 计数、`lost` 日志可观测；不为用量延长停机；费用本来未知（§6.4） |
| R5 | `model_access` 不在 `/api/v1/os` / `agent24 os list` 里显示 | 远端来源今天只有非回环的 `OMLX_URL`/`OLLAMA_URL`；**FU-73**（ME4→next，Medium） |
| R6 | 没有按日远端 token 预算，`remote_allowed` 模块在令牌桶内可持续外发 | 需要价目表与远端配置入口，本轮都不做；remote 需 manifest 显式声明 |
| R7 | `CompletionRequest`/`CompletionResponse` 加字段、`ModelError` 加变体，对**外部**直接构造/穷尽匹配它们的 crate 是源码不兼容 | 工作区内已列全（§10.1）；外部（若 Sin90 仍 git 依赖 `agent24-models`）在其 M5 前同步——4.2.2a 的 PR body 点名 |
| R8 | ~~共享健康表~~ | **v2 已消除**（H2，§5.1） |
| R9 | daemon 重启重置令牌桶 | 模块无法触发 daemon 重启（§0 同 UID 敌意进程除外） |
| R10 | 512 KiB 的文本在 JSON 里最坏可膨胀 6 倍（全是需转义的控制字符）而超过 1 MiB 帧 → 仍会是 `-32603` | 只有病态输出才会；兜底仍是既有的 `response_line`（不断连）；要消除需按序列化后字节检查，代价是先序列化一遍 |
| R11 | 按序尝试多个 provider 时，第一个慢 provider 可吃完整个 120s（v2 L2） | 路由器的既有行为；冷却会让它下次被跳过（在本模块自己的健康表里） |
| R12 | 模块不能从内核已知的冷却受益，会自己再撞一次不可用的 provider（v2 H2 的代价） | 最多多一次 connect 超时（2s）；换来的是模块无法操纵内核路由 |

---

## 12. 登记的 followups（本分支已写入 `docs/agent/followups.md`）

- **FU-71**（现有缺陷，High，**由 ME4-4.2.2a 关闭**）：`ModelRouter::from_env` 的 `url_host`/`is_loopback_host` 与 reqwest 的 WHATWG 解析不一致，
  `OMLX_URL=http://evil.example\@127.0.0.1:8088` 被标 `Local`、实际连 `evil.example`——**所有** LocalOnly 使用方（Guardian、会话摘要器，以及本文的模块回调）的隐私保证因此不成立。修法与判据见本文 §2.3 / J16。
- **FU-72**（Low，ME4→next）：`GET /api/v1/usage`（不带 `module`）的 `cost_usd` 恒 `0.0`，对远端调用是错误陈述；应为 `null` 或去掉，需要评估客户端兼容。
- **FU-73**（Medium，ME4→next）：`MountReport`/`agent24 os list` 显示模块的 `model_access`（R5）。
- **ME4-CODEX-DEBT**：本设计 v1 经 Opus 子代理评审（REQUEST_CHANGES 0/2/7/10），v2 待第 2 轮；额度恢复后补 Codex。

---

## 附录 A：scratch crate 与 `cargo check` / `cargo test` 记录

位置：`/private/tmp/claude-502/-Users-jason-Dev-auraai-Agent24/977deb42-1aba-448f-95e7-5bae2dee6fd4/scratchpad/me4s2-check/`。

构成：把 worktree（`73a9592`）的 `agent24-models`、`agent24-domain`、`agent24-os-proto`、`agent24-store` **整份拷贝**进来（包名改 `me4s2-*`、lib 名不变，
`agent24-protocol`/`agent24-core` 仍 path 依赖 worktree），按本文 §2.1 / §2.3 / §3.2 / §4.1 / §6.1 / §7 打补丁；再加一个 `me4s2-check` crate：
`model_callback.rs`（§4.2–§7：handler、wire 类型、grant、准入、错误、`UsageSink`）、`usage_recorder.rs`（§6.3 写者）、`usage_route.rs`（§6.5）、`mount_sketch.rs`（§2.4、§3.3 接线）、以及测试。

```sh
S=/private/tmp/claude-502/-Users-jason-Dev-auraai-Agent24/977deb42-1aba-448f-95e7-5bae2dee6fd4/scratchpad/me4s2-check
cd $S
CARGO_TARGET_DIR=target cargo check --workspace --all-targets
CARGO_TARGET_DIR=target cargo clippy -p me4s2-check --all-targets -- -D warnings
CARGO_TARGET_DIR=target cargo test -p me4s2-check                   # handler / params / manifest / wire / recorder / route
CARGO_TARGET_DIR=target cargo test -p me4s2-os-proto --lib rpc::    # 既有 rpc 测试 + 按方法超时 + 闭集
CARGO_TARGET_DIR=target cargo test -p me4s2-store module_model_usage
CARGO_TARGET_DIR=target cargo test -p me4s2-models                  # 既有测试（/chat 路径回归）+ J16 变体矩阵
CARGO_TARGET_DIR=target cargo test -p me4s2-domain
```

工具链：本机 `cargo +1.98.0` 不可用（Homebrew rustc 1.95.0），scratch 用默认工具链；实现 PR 仍按 PLAN §一 第 8 条用 `+1.98.0` 跑全局前置。

输出（v2，2026-09-24）：

```
$ cargo check --workspace --all-targets
    Checking me4s2-check v0.0.0 (…/me4s2-check/check)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.14s
$ cargo clippy -p me4s2-check --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.55s
$ cargo test -p me4s2-check
test model_callback::tests::fair_global_admission ... ok
test model_callback::tests::no_grant_is_forbidden ... ok
test model_callback::tests::local_only_never_reaches_a_remote_provider_and_remote_allowed_does ... ok
test model_callback::tests::params_shape ... ok
test model_callback::tests::an_unknown_request_id_is_refused_not_run_unbound ... ok
test usage_route::tests::selector ... ok
test model_callback::tests::module_failures_do_not_cool_down_the_kernels_router ... ok
test model_callback::tests::an_oversize_answer_is_a_defined_error_and_its_tokens_are_kept ... ok
test manifest_tests::model_access_default_explicit_and_rejections ... ok
test wire_tests::max_tokens_is_forwarded_only_when_set_and_model_id_is_parsed ... ok
test usage_recorder::tests::writer_drains_and_exits_when_the_channel_closes ... ok
test model_callback::tests::busy_spends_no_token ... ok
test usage_recorder::tests::the_hard_stop_ends_the_writer_even_with_a_live_sender ... ok
test model_callback::tests::cancelling_the_root_reaches_the_provider_and_records_it ... ok
test wire_tests::cancel_root_reaches_the_real_provider_and_answers_cancelled ... ok
test model_callback::tests::dropping_the_call_future_cancels_the_provider_token_and_records_cancelled ... ok
test wire_tests::dropping_the_provider_future_closes_the_upstream_connection ... ok
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
$ cargo test -p me4s2-os-proto --lib rpc::
test rpc::tests::a_declared_budget_is_clamped_and_an_absent_one_is_the_default ... ok
test rpc::tests::the_error_kinds_are_exactly_specs_closed_set ... ok
test rpc::tests::the_handshakes_error_kinds_are_members_of_the_closed_set ... ok
test rpc::tests::per_method_budget_outlives_the_connection_budget ... ok
test result: ok. 53 passed; 0 failed; 0 ignored; 0 measured; 268 filtered out; finished in 1.04s
$ cargo test -p me4s2-store module_model_usage
test module_model_usage::tests::the_none_row_cannot_carry_tokens ... ok
test module_model_usage::tests::per_module_rows_accumulate_and_stay_separate ... ok
test module_model_usage::tests::concurrent_records_lose_no_increment ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 15 filtered out; finished in 0.04s
$ cargo test -p me4s2-models
test tests::status_becomes_a_cause_a_user_can_act_on ... ok
test tests::transient_errors_allow_fallthrough_config_errors_do_not ... ok
test router::tests::env_local_tier_uses_the_http_clients_own_parser ... ok
test result: ok. 27 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.41s
$ cargo test -p me4s2-domain
test result: ok. 47 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

scratch 与真实代码的差异（如实）：`agentd` 是 binary crate，`RateLimiter`/`Clock`/`refused_error` 按原形状复制进 `check/src/events_emit.rs`；
`mount_package` 与 `serve()` 的新增部分写成纯函数（`mount_sketch.rs`），没有在真实函数体里编译；`usage_route.rs` 用 `AppStateLite` 代替 `AppState`；
`agent24-agent` 与 `agentd routes.rs` 那两处 `ModelError` 穷尽 `match` 没有拷贝进 scratch（它们在工作区里，改法是加一个与 `Provider` 相同的分支）。
