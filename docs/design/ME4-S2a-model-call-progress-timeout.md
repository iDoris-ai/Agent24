# ME4-S2a —— 模块推理回调的时限按进展算（修订 ME4-S2 决策 M2 的数值）

> 状态：**v2.2，已定（David，2026-10-15）**，§7 的四个问题按建议定。改的是内核与模块之间的接口，jason 在 #948 知会；他有意见可以在 `ab/documenting` → `main` 的发布 PR 之前提。起草：Documenting 线（DOC-1-09 第 9 步真实验收时发现，2026-10-14）。
> 设计审查：Fable 5.1 代审（codex 本机未装）。第 1 轮 REQUEST_CHANGES（3 H / 5 M / 6 L），第 2 轮 APPROVE（0 H / 2 M / 7 L），都已采纳，见文末「改动记录」。
>
> **修订对象**：
> - `docs/design/ME4-S2-model-callback.md`：§1 第 122 行、§3.1 第 324 行、§3.2 第 2 条（`MAX_METHOD_CALL_TIMEOUT`）、§3.3 表「方法超时 120s」一行、§3.4 表、§5.2 `MODEL_CALL_TIMEOUT` 一行、§8 第 864 / 905 行、§11 R1 / R11；
> - `docs/specs/WIRE-OOP-MODULE.md`：第 337–338 行（§4.4）、「方法专属超时：120 秒」；
> - `docs/design/ME4-S3-os-sdk.md`：第 237 行；
> - 代码注释：`rpc.rs:84-88`、`:367-377`，`model_callback.rs:33-36`，`clients/model.rs:1-5`、`:22-24`。
>
> 决策 M2 选的 (a)「按方法的超时元数据 + 统一桥接取消」**不变**，变的是那个数从哪里来。本文只是设计，代码按 §5 的切法另开 PR。

## 0. 问题

**现象**（2026-10-14，Mac16,10 / Apple M4 / 16 GB，Ollama 0.40.4，`qwen2.5:7b` Q4_K_M，上下文 16384，代码 `b8ce9ca`）：
DOC-1-09 验收 harness 用本机真实内核跑 S01 的 8 份样本，用时 25 分钟。Ollama 日志里有 4 次 `POST /v1/chat/completions` 记为 500，
**每次都恰好 `2m0s`**，紧接着是 `srv stop: cancel task`。也就是说，是调用方在 120 秒时断开，不是模型出错。
其中 3 次由抽取器重试后跑完（RPC 计时先到，模块收到 `timeout`，`extract/run.rs:141` 重试一次）；`s01-08` 那次重试也超时，`ACC-EXTRACT` 以 `engine_unavailable` 失败。

**原因**：抽取每次调用的 `max_tokens` 是 4096（ADR-DOC-02 §3.2，`extract/run.rs:29`），这台机器上 7B 模型约 21 token/s，
写满要 195 秒，再加处理提示词的时间。而一次调用的时限，在三层都是固定的 120 秒左右：

| 层 | 常量 | 值 | 位置 |
|---|---|---|---|
| provider | `chat_timeout`（整次请求，`reqwest` 的 `.timeout`） | 120s | `agent24-models/src/lib.rs:251`、`:625` |
| 内核 RPC | `MODEL_CALL_TIMEOUT`（`Handler::call_timeout`），夹在 `MAX_METHOD_CALL_TIMEOUT` 之下 | 120s / 夹紧 300s | `agent24d/src/model_callback.rs:36`、`:731`；`agent24-os-proto/src/rpc.rs:88` |
| 模块 SDK | `MODEL_RESPONSE_TIMEOUT` | 125s | `agent24-os-sdk/src/clients/model.rs:25`、`:139` |

**为什么不是把数调大**：任何固定的总时长都只是挪动边界，更慢的机器、更大的模型（14B 约慢一半）、更长的输出，总会再撞上。
另一方面，ME4-S2 §3.2 的原则要守住：**任何方法都不能把一次调用变成实际无界**。

## 1. 目标与范围

- **目标**：模型还在出字，就不打断；卡住了，尽快发现；每次调用仍有一个**由这次调用本身推出的**截止时间，而且无论路由器试几个 provider，都不超过它。
- **只改模块推理回调 `_a24/model/complete`**。它是唯一显式开启「按进展计时」的调用方。`complete()` 的其他调用方一律不变（仍是非流式、`chat_timeout` 120s）：

| 调用方 | 位置 | 带 tools | `max_tokens` | 本文之后 |
|---|---|---|---|---|
| 模块推理回调 | `model_callback.rs:149` | 否 | 总有值（缺省 1024，上限 4096） | **开启**按进展计时 |
| `/api/v1/chat` | `routes.rs:536-537` | 否（`vec![]`） | `None` | 不变 |
| Guardian 风险评估 | `agent24-policy/src/guardian.rs:226` | 否 | `None` | 不变 |
| 会话摘要器 | `agent24-agent/src/lib.rs:221` | 否 | `None` | 不变 |
| agent loop | `agent24-agent/src/lib.rs:1667` | 是 | `None` | 不变 |
| explorer 子代理 | `agent24-agent/src/subagent.rs:191` | 是 | `None` | 不变 |

- **不做**：不做作业化（ME4-S2 §3.1 的 (b)）；不改并发与限流的数值（每模块 2、全局 4、令牌桶）；其他调用方要不要也开启，另行决定（§7 Q4）。

## 2. 决策

### 2.1 数从哪里来：放进 `agent24-os-proto`

下面这些是内核和模块之间的接口数值，放在两边都依赖的 `agent24-os-proto` 里，内核和 SDK 用同一份。
`MODEL_MAX_TOKENS_CEILING` 从 `model_callback.rs:46` 移过来（值不变，4096）：

```rust
pub const MODEL_MAX_TOKENS_CEILING: u32 = 4096;
pub const MODEL_FIRST_CHUNK_TIMEOUT: Duration = Duration::from_secs(300);   // ⚖️
pub const MODEL_STALL_TIMEOUT: Duration = Duration::from_secs(60);          // ⚖️
pub const MODEL_PER_TOKEN_BUDGET: Duration = Duration::from_secs(1);        // ⚖️
/// The deadline of one call: first chunk plus every token at the slowest pace allowed.
pub const fn model_call_budget(max_tokens: u32) -> Duration {
    MODEL_FIRST_CHUNK_TIMEOUT.saturating_add(MODEL_PER_TOKEN_BUDGET.saturating_mul(max_tokens))
}
pub const MODEL_ROUTING_MARGIN: Duration = Duration::from_secs(60);         // ⚖️
/// The router's longest cooldown; a module waits this long after `no_provider` (§2.6).
pub const MODEL_ROUTER_MAX_COOLDOWN: Duration = Duration::from_secs(60);    // ⚖️
/// The RPC backstop: strictly after any call's deadline.
pub const MODEL_CALL_BACKSTOP: Duration = model_call_budget(MODEL_MAX_TOKENS_CEILING).saturating_add(MODEL_ROUTING_MARGIN);
/// What the SDK waits for a reply: the backstop and 5s, as 125 = 120 + 5 today.
pub const MODEL_RESPONSE_TIMEOUT: Duration = MODEL_CALL_BACKSTOP.saturating_add(Duration::from_secs(5));
```

### 2.2 一次调用只有一个截止时间（v2 H2）

- `CompletionRequest`（`agent24-models/src/lib.rs:93`）加一个字段 `progress: Option<Progress>`，缺省 `None`，`None` 时行为与今天逐字节相同：

```rust
pub struct Progress {
    /// Absolute: set once, after admission and before routing. Every provider the router tries shares it.
    pub deadline: Instant,
    pub first_chunk: Duration,
    pub stall: Duration,
}
```

- 只有 `ModelCompleteHandler` 设它：调用通过准入之后、交给路由器之前，算一次 `deadline = now + model_call_budget(max_tokens)`。
  这时 `max_tokens` 一定有值：超过上限的请求已被 `validate` 拒绝（`model_callback.rs:127-133`），没给的已补上缺省值 1024（`:149`、`:182`）。
- `Instant` 的类型与 `router.rs:18` 统一用一种（实现时定，`tokio::time::Instant` 便于 `sleep_until`）。
- **剩余时间不够就不再换 provider**：前一个 provider 失败后，若离截止时间不足 `MODEL_STALL_TIMEOUT`，路由器不再尝试下一个，直接返回 `Unavailable`。否则下一个 provider 刚加载完模型就会被切断，白占一次。
- 路由器（`router.rs:428-448`）把同一个请求原样交给每个 provider，**不重算**。第一个 provider 在首段卡住，第二个 provider 拿到的是剩下的时间，不会多出一个 300 秒。
- RPC 的计时在派出的任务里、handler 的 future 第一次被 poll 之前就建立（`rpc.rs:1310-1312`），比算截止时间早，而 `MODEL_CALL_BACKSTOP` 又比任何调用的截止时间多出 `MODEL_ROUTING_MARGIN`。所以正常情况下，provider 的计时总是先到，模块看到的是 `no_provider`；RPC 兜底只在内核自身卡住时才会触发。

### 2.3 provider：`progress` 有值时流式接收

`OpenAiCompatProvider::complete`，`req.progress` 为 `Some` 时：

- **请求**：请求体加 `"stream": true` 与 `"stream_options": {"include_usage": true}`；其余字段不变。不给 reqwest 设 `.timeout()`，所有时限由下面的计时管。
- **首段计时**：从调用 `send()` 到收到第一个 `data:` 事件，≤ `min(first_chunk, deadline)`。这一段包括建立连接、在后端排队、加载模型、处理提示词。
  Ollama 在第一个 token 时才发应答头，所以等应答头也算在这一段里。
- **间隔计时**：任意两个相邻的 `data:` 事件之间，≤ `min(stall, deadline)`。
- **截止时间**：一到 `deadline`，不论在哪个阶段都停。
- **只有 `data:` 事件推动计时**。注释行（`: keep-alive`）、`event:` / `id:` 行、空行都不重置首段和间隔计时；有截止时间兜着，不会被无限拖住。
- **SSE 的解析**：同一事件里的多行 `data:` 按 SSE 规范用换行连接。`choices` 为空数组是合法的：`include_usage` 时最后一个事件只带 `usage`。
  `delta.content` 可以缺省或为 `null`，例如只带 `role` 的第一个事件。`model` 取第一个非空值，`usage` 取最后一个。
- **结束**：收到 `data: [DONE]`，或者收到过带 `finish_reason` 的事件之后流正常结束，算完成。
  流在这两者之前就断了，算后端中途崩溃，记为 `ModelError::Unavailable`，不当作完整答案（今天截断的应答体是 `Provider` 错误，`lib.rs:557-558`）。
- **`finish_reason`**：只看有没有（非 `null`），用来判断上一条「结束」；不看它的值，和今天一样。今天 `OaChoice` 只有 `message`（`lib.rs:383-386`），截断由调用方自己发现（抽取器解析失败后拆成两半重问）。
- **流中途的错误事件**：`{"error": …}` 按 500 处理，记为 `Unavailable`。Ollama 的 OpenAI 兼容层在 200 之后不一定发错误事件，所以「断流」才是主要信号（实现时在 0.40.4 上实测，记进 PR）。
- **出错**：三个计时任一到点都是 `ModelError::Unavailable`，文案写明是哪一个，例如 `ollama: no output for 60s`。和今天超时一样，路由器照常冷却、尝试下一个 provider。
- **字节上限不变**：原始流字节和拼出的内容都受 `MAX_CHAT_RESPONSE_BYTES`（8 MiB）约束，超出即 `Provider` 错误。
- **取消不变**：读循环在 `select!` 里同时等 `cancel`（同 `read_json_capped`）；ME4-S2 §3.3 表的五条 drop 路径照样会打断 HTTP 连接。
  同时就绪时，取消先于计时，计时先于读到的数据：被取消的调用总是 `Cancelled`，一直就绪的注释行也越不过截止时间（#960 之后补）。
- **不带 tools**：流式接收不解析 `delta.tool_calls`，所以 `progress` 与非空 `tools` 同时出现时，请求发出之前就报 `Provider` 错误，而不是悄悄丢掉工具调用（#960 之后补）。`_a24/model/complete` 本来就不带 tools。
- **非 2xx**：照旧 `read_error_body` + `friendly_http_error`。
- **后端无视 `stream: true`**，回 `application/json`：按非流式应答读，时限取截止时间。**限制**：这种后端要等整段答案生成完才发应答头，而等应答头算在首段里，所以生成超过 `first_chunk`（300s）的调用仍会在首段到点。我们接受这一点：这类后端就按「第一个事件就是整段答案」来对待，建议换用支持流式的后端（v2 M3）。

**已验证**：Ollama 0.40.4 在 `stream: true` 下同时遵守 `response_format: json_schema`（strict），逐 token 发 `chat.completion.chunk`，每个事件都带 `model`。
**未验证**：oMLX（§6 R-c）。

### 2.4 内核与 RPC

- `ModelCompleteHandler`：入口设 `progress`（§2.2）。`call_timeout()` 返回 `MODEL_CALL_BACKSTOP`。
- `rpc.rs` 的夹紧值 `MAX_METHOD_CALL_TIMEOUT`（300s）要容得下兜底值，否则 `effective_call_timeout`（`rpc.rs:1256`）会悄悄把兜底值夹到截止时间之前，§0 的问题就又回来了。**已选 (A)**（Q2）：
  - **(A，推荐)** 把 `MAX_METHOD_CALL_TIMEOUT` 提到 `MODEL_CALL_BACKSTOP`。今天只有 `_a24/model/complete` 声明了方法级时限，其他方法仍用连接级 30s；
    这个常量的含义改写成「声明了方法级时限的方法，最多能拿到的值」，并加编译期 `assert!(MAX_METHOD_CALL_TIMEOUT >= MODEL_CALL_BACKSTOP)`（同 `module.rs:268` 的写法）。
  - (B) 保留 300s 作为通用夹紧，另加一个只对模型方法放行的常量。代码多一处分支，但「方法级时限 ≤ 300s」这条承诺字面不变。
- **带 `request_id` 的调用不变**（ME4-S2 §3.4）：`bind_to_lifecycle` 的剩余预算（被代理请求 ≤ 30s）仍先到，见 `drain.rs:815-836`。
- **daemon 停机不变**：`modules_cut_off` 是一个绝对时刻（`server.rs:260-268`），不会等在途调用。

### 2.5 模块 SDK

`MODEL_RESPONSE_TIMEOUT` 改为从 `agent24-os-proto` 导入（§2.1，= 兜底值 + 5s）；`agent24_os_sdk::MODEL_RESPONSE_TIMEOUT` 与 `clients::MODEL_RESPONSE_TIMEOUT` 两个重导出路径（`os-sdk/src/lib.rs:37`、`clients/mod.rs:21`）保留。它是公开常量，值变了要在 SDK 的 CHANGELOG 里写明：行为上只是等得更久，不是不兼容。

### 2.6 文档模块（v2 M1）

今天一次瞬时卡顿（模型重新加载、别的进程占了 GPU）会让 RPC 计时先到，模块收到 `timeout` 并重试一次。
改完以后，卡顿在 provider 层就成了 `unavailable` / `no_provider`（`retryable: true`）。`run.rs:154` 收到它会立即让 job 失败，那一次重试也就没了。所以：

- 对 `Unavailable { cause: NoProvider, retryable: true }`，抽取器等 60s 后重试**一次**。60s 是路由器冷却的上限（`router.rs:270`，`route()` 用严格的 `now < until`，`:359`），等过这么久，冷却一定已经结束。
  （实现时修订，David 定：这只在模块只有一个在途调用时成立。模块里另一个调用在等待期间失败，会让 provider 重新冷却。所以改为每次等 60s 后重试，同一次调用累计最多等 3 分钟。见文末改动记录。）
  这个上限今天只是 `with_defaults` 里的一个数，`new()` 能接受到 24h：实现时由 `agent24-models` 导出一个具名常量（如 `MAX_COOLDOWN`），`with_defaults` 和抽取器都用它，J14 断言两者相等；（实现时的做法：文档模块不依赖 `agent24-models`，所以抽取器用的是 `agent24-os-proto` 的 `MODEL_ROUTER_MAX_COOLDOWN`，经 SDK 的 `MODEL_NO_PROVIDER_WAIT` 导出；`agent24-models` 的 `DEFAULT_MAX_COOLDOWN` 给 `with_defaults` 用；agent24d 的测试断言两者相等，即 J14。）
- 等待期间和等忙时一样先问 `go_on()`（`run.rs:132-133`），被取消的 job 不会白等 60s；
- 代价：根本没有可用 provider（比如 Ollama 没在运行）时，回的也是同一个 `no_provider`（`router.rs:417-424`），所以这种失败要多等 60s 才以 `engine_unavailable` 结束（实现时修订：累计最多等 3 分钟，所以是多等 180s）；
- `Timeout` 的重试规则不变（现在只剩内核自身卡住这一种来源）；
- `run.rs:9` 的注释同步改写。

## 3. 数值（⚖️ = 选的，不是推出来的）

| 常量 | 值 | 理由 |
|---|---|---|
| `MODEL_FIRST_CHUNK_TIMEOUT` ⚖️ | 300s | 冷启动加载模型（14B 从外接盘读约 30–60s）+ 在串行后端里排队 + 处理约 16k token 的提示词（慢机器上要几分钟） |
| `MODEL_STALL_TIMEOUT` ⚖️ | 60s | 出字之后，相邻两个 token 间隔一分钟就是卡住了；本机 7B 的间隔约 0.05s |
| `MODEL_PER_TOKEN_BUDGET` ⚖️ | 1s | 即「至少 1 token/s」。本机 7B 约 21 token/s，14B 约一半 |
| 一次调用的截止时间 | 300 + `max_tokens` × 1s | `max_tokens` 1024（缺省）→ 约 22 分钟；4096 → 约 73 分钟 |
| `MODEL_ROUTING_MARGIN` ⚖️ | 60s | handler 入口之前的排队、准入；保证兜底值严格晚于截止时间 |
| `MODEL_CALL_BACKSTOP` | 300 + 4096 + 60 = 4456s | 约 74 分钟 |
| `MODEL_RESPONSE_TIMEOUT`（SDK） | 4461s | 兜底值 + 5s |
| `MODEL_ROUTER_MAX_COOLDOWN` ⚖️ | 60s | 路由器冷却 provider 的最长时间；SDK 以 `MODEL_NO_PROVIDER_WAIT` 导出，模块遇到 `no_provider` 后等这么久再试（§2.6） |
| 抽取器每次调用的 `no_provider` 等待上限 ⚖️ | 180s | 3 次冷却上限：模块里另一个调用失败会让 provider 重新冷却（§2.6 的实现时修订） |

## 4. 判据（每条带正对照；`cargo test <过滤>` 先 `-- --list` 断言非空）

provider 测试用本地 TCP 桩发 SSE，`Progress` 里的三个时长缩到百毫秒级：

| # | 判据 | 正对照 |
|---|---|---|
| J1 | 桩每 100ms 发一个 token、共 2s：成功，内容是各段拼接，`model` 取自第一个事件，`usage` 取自最后一个（`choices: []` 的那个） | — （变异：换回固定 300ms 总时限 → 变红） |
| J2 | 桩发一个 token 后静默：在间隔时限后 `Unavailable`，文案写明「no output」 | 每 100ms 一个 token 的桩照常成功 |
| J3 | 桩接受连接、一个事件都不发：在首段时限后 `Unavailable` | 立刻发事件的桩不超时 |
| J4 | 桩每 150ms 发一个 token（短于间隔时限），但超过截止时间：`Unavailable` | 截止时间足够时成功 |
| J5 | 桩只发 `: keep-alive` 注释行：在首段时限后 `Unavailable`（注释不推动计时） | 同样频率的 `data:` 事件推动计时 |
| J6 | 桩发了内容、没有 `[DONE]` 也没有 `finish_reason` 就断流：`Unavailable`；`data:` 分成两行的事件按换行连接 | 带 `[DONE]` 的同一条流成功 |
| J7 | 请求体：`progress` 为 `Some` 时有 `stream: true` 与 `stream_options`、没有 reqwest 总时限；为 `None` 时与今天逐字节相同 | 同一请求在两种取值下各断言一次 |
| J8 | 流的字节超过上限 → `Provider` 错误 | 刚好在上限内成功 |
| J9 | 流进行中取消 → 立即 `Cancelled` | 不取消则跑完 |
| J10 | 后端回 `application/json` → 按非流式读成功 | 后端回 `text/event-stream` 后挂住 → 间隔计时到点（证明走的是流式） |
| J11 | 路由器两个 provider：第一个在首段卡住，第二个仍受同一个截止时间约束，总耗时 ≤ 截止时间，模块看到 `no_provider` | 每个 provider 各自重算截止时间的变异 → 总耗时超过截止时间，变红 |
| J12 | 内核：handler 的 `call_timeout()` 等于 `MODEL_CALL_BACKSTOP`，夹紧后不变；`rpc.rs:1721-1724` 的夹紧断言随 Q2 改写 | 没有声明的方法仍是连接级 30s |
| J13 | 编译期 `assert!`：`MODEL_RESPONSE_TIMEOUT > MODEL_CALL_BACKSTOP > model_call_budget(MODEL_MAX_TOKENS_CEILING)`（`agent24-os-proto`）；选 (A) 时另有 `MAX_METHOD_CALL_TIMEOUT >= MODEL_CALL_BACKSTOP` | — |
| J14 | 文档模块：`no_provider` 等 60s 后重试，累计最多等 3 分钟（实现时修订，原为一次），预算用完仍失败才让 job 失败；等待中取消，立即结束；没有任何 provider 时 3 分钟后失败 | 第一次就成功则不等 |
| J16 | 路由器：前一个 provider 失败时离截止时间不足 `MODEL_STALL_TIMEOUT`，不再尝试下一个 | 剩余充足时照常尝试 |
| J15 | 实测（记进 PR）：同一台 M4、`qwen2.5:7b`，DOC-1-09 验收跑 8 份，Ollama 日志里没有 `2m0s` 的 500，`s01-08` 抽取完成 | 本文 §0 的那次运行 |

变异：去掉任一个计时、让注释行推动计时、把断流当成完成、每个 provider 重算截止时间、`progress` 为 `None` 也走流式，各自至少一条测试变红。

## 5. 切法（每个 ≤ 300 行，不计测试；base 为 `ab/documenting`，因为 Documenting 的验收在等它）

1. **`agent24-os-proto` + `agent24-models`**：§2.1 的常量、`Progress` 字段、流式读与三个计时（§2.3），J1–J11、J13。
   这时还没有任何调用方设 `progress`，所以这个 PR 合进去，运行行为不变。
   `CompletionRequest` 加字段，对外部直接写结构体字面量的 crate 是源码不兼容（ME4-S2 R7，Sin90），PR 正文点名。J16 也在这个 PR。
   **实际拆分（2026-10-15）**：超出规模，拆成 4 个 PR：常量（#956）、`progress` 字段（#957）、`answer()` 重构（#958）、流式接收与路由器。
2. **`agent24d` + `agent24-os-proto` 的 `rpc.rs` + `agent24-os-sdk`**：handler 开启 `progress`、兜底值与夹紧（按 Q2 的选择）、SDK 常量（§2.4–2.5），J12。
   同一个 PR 改文首「修订对象」里列出的文档和注释，以及 `model_callback.rs:46`、`:128-131`、`:1010-1011`、`:1549`（`MODEL_MAX_TOKENS_CEILING` 搬走与断言）。从这个 PR 起行为才真正改变。
3. **`agent24-documents`**：`no_provider` 等冷却上限后重试（§2.6；实现时修订为同一次调用累计最多等 3 分钟）与注释，J14；附 J15 的实测记录。

## 6. 残余风险

| # | 风险 | 为什么接受 / 谁接 |
|---|---|---|
| R-a | 其他 `complete()` 调用方（§1 的表）仍是固定 120s、非流式 | 它们的输出通常很短；要不要开启，另行决定（Q4） |
| R-b | 串行后端（Ollama 默认 `NUM_PARALLEL=1`）里排队的调用，排队时间算在首段里：前面有一个长调用时，后面的可能在 300s 时以 `no_provider` 失败 | 和今天一样（今天是 120s）；全局 4 个在途的上限不变；文档模块一个 job 一次只发一个调用 |
| R-c | oMLX 对 `stream: true` + `json_schema` + `stream_options` 的支持，本文没有验证 | 实现时若手上没有 oMLX，请 jason 在 oMLX 上跑一次 J1 的手工版；不支持流式时走 §2.3 最后一条的回退，但会受那里写的限制 |
| R-d | 一次调用占着回调槽和模型准入名额的时间，最长从 120s 变为约 74 分钟（ME4-S2 R1）。出字之后卡住，60s 内就会被发现；但首段最长要等 300s，而一个只有 1 token/s 的后端会一直占到截止时间 | 本地推理本来就是串行的；每模块 2、全局 4 的上限不变 |
| R-e | 结果在连接断开后仍取不回（ME4-S2 R1 后半） | 不变 |
| R-f | 不支持流式的后端，生成超过 300s 的调用仍会失败（§2.3 最后一条） | 本机已知的后端（Ollama）支持流式；oMLX 见 R-c |

## 7. 决定（David，2026-10-15）

- **Q1** 三个数：按建议，首段 300s、间隔 60s、每 token 1s。
- **Q2** 夹紧值：(A)，把 `MAX_METHOD_CALL_TIMEOUT` 提到 `MODEL_CALL_BACKSTOP`，并加编译期断言（§2.4，J13）。
- **Q3** 约 74 分钟的兜底值算守住了「任何方法都不能把一次调用变成实际无界」：它有界，而且是从 `max_tokens` 的上限推出来的。
- **Q4** §1 表里的其他调用方这次不开启，等有调用方撞上再议（R-a）。

## 改动记录

**v1 → v2**（Fable 第 1 轮，REQUEST_CHANGES，3 H / 5 M / 6 L，全部采纳）：
- H1：按「不带 tools」来区分会把 `/api/v1/chat`、Guardian、会话摘要器也一起改成流式 → 改为由模块回调通过 `progress` 显式开启，§1 列出全部调用方。
- H2：路由器按序尝试时，每个 provider 都重算时限，总时长会超过 RPC 兜底值 → 一次调用只有一个绝对截止时间，由 handler 算一次（§2.2），J11。
- H3：provider 看不到内核的缺省 `max_tokens` → 只有总带着 `max_tokens` 的模块回调开启（§1 表）。
- M1：流式之后卡顿变成 `no_provider`，文档模块会失去唯一的一次重试 → §2.6，J14。
- M2：SSE 的细节没写清：空的 `choices`、缺省的 `content`、断流、注释行、多行 `data:`、`finish_reason` → §2.3 逐条写明，J5、J6。
- M3：无视 `stream` 的后端走回退时仍受首段计时限制 → 写明这个限制（§2.3、R-f）。
- M4：单独合进第 1 个 PR 时，对模块调用没有作用，对其他调用方反而放大了时限 → 开启方式改为显式，第 1 个 PR 合进去不改变运行行为；§5 列出要改的断言。
- M5：缺判据 → J5、J6、J10（可观察的正对照）、J11、J14。
- L1 改写 R-d；L2 补全「修订对象」；L3 常量放进 `agent24-os-proto`，J13 用编译期断言；L4 不再给 reqwest 设总时限，计时参数随请求传入，不加构造器参数；L5 写明带 `request_id` 的调用和停机不变；L6 J1 的对照改标为变异。

**v2 → v2.1**（Fable 第 2 轮，APPROVE，0 H / 2 M / 7 L，全部采纳）：
- M1：60s 的重试等待要先问 `go_on()`；没有任何 provider 时也会多等 60s → §2.6，J14。
- M2：选 (A) 时用编译期断言防止夹紧值又落到兜底值之下 → §2.4，J13。
- L1 `const` 里改用 `saturating_add` / `saturating_mul`；L2 截止时间统一写成「准入之后、路由之前」，`max_tokens` 超限是被拒绝而不是夹紧；L3 `finish_reason` 只看有没有；
  L4 PR 1 正文点名 R7；L5 SDK 的两个重导出路径保留；L6 剩余时间不足 `MODEL_STALL_TIMEOUT` 时不再换 provider（J16）；L7 补全 PR 2 要改的行。

**v2.1 → v2.2**（PR-Daemon 在 #948 的两条非阻塞项）：冷却上限改为由 `agent24-models` 导出的具名常量，抽取器的等待与之相同（§2.6）；`model_callback.rs:181` → `:182`。

**v2.2 → 已定**：§7 四个问题由 David 按建议定（2026-10-15）；jason 在 #948 知会。

**实现时的修订**（Grok 在第 6 片的评审，David 定）：只重试一次时，模块里两个 job 并发，另一个 job 的失败会把冷却往后推，等满 60s 的重试仍可能拿到 `no_provider`，job 提前失败。改为每次等 60s、同一次调用累计最多 3 分钟（§2.6、J14）。

**实现时的更正**（Fable 在第 6 片的评审）：冷却上限的常量放在 `agent24-os-proto`（`MODEL_ROUTER_MAX_COOLDOWN`），不是 `agent24-models`，因为文档模块只依赖 SDK；§2.6 与 §3 的常量表已补。
