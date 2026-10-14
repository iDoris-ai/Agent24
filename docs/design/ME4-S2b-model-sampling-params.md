# ME4-S2b —— 模块推理回调开放 `temperature`（修订 ME4-S2 §4.2）

> 状态：**v2.1，已定（David，2026-10-15）**，§9 的三个问题按建议定。改的是内核与模块之间的接口，jason 在 #954 知会；他有意见可以在 `ab/documenting` → `main` 的发布 PR 之前提。起草：Documenting 线（DOC-1-09 第 9 步，2026-10-15；David 同意开放温度参数）。
> 设计审查：Fable 5.1 代审（codex 本机未装），第 1 轮 REQUEST_CHANGES（1 H / 6 M / 4 L），第 2 轮 REQUEST_CHANGES（1 M / 4 L），第 3 轮 APPROVE，都已采纳，见文末「改动记录」。
> 修订对象：`docs/design/ME4-S2-model-callback.md` §4.2（params 的确切形状，以及「`temperature` 等采样参数本轮不开（最小集），将来加是**加字段**」那一条）；
> `docs/specs/WIRE-OOP-MODULE.md` §5.5；`docs/design/ME4-S3-os-sdk.md` 里 `CompleteRequest` 的形状。
> 与 ME4-S2a（#948）语义上互不依赖，但会改到同一批代码，见 §6。本文只是设计，代码按 §6 另开 PR。

## 0. 问题

模块推理回调 `_a24/model/complete` 不能指定采样温度。Ollama 的 OpenAI 兼容层在请求不带 `temperature` 时会**自己写入 1.0**，而且请求里的值优先于模型文件（Modelfile）里的 `PARAMETER`。
本机的 Ollama 0.40.4 日志证实了这一点：53 次请求里，只有 3 次显式带了 `temperature: 0` 的用的是 0，其余都是 1.0，包括用一个声明了 `PARAMETER temperature 0` 的派生模型名跑的那次。
所以今天**没有任何办法**让文档模块的抽取以温度 0 运行。

文档模块的抽取要求模型逐字照抄原文、写对块编号，不需要随机性，按概率抽签反而让结果不稳定、更容易出错。

**实测**（2026-10-14/15，Apple M4，Ollama 0.40.4，`qwen2.5:7b`；证据目录 `docs/documenting/evidence/2026-10-14-s01-mac-mini-m4/`）：把 s01-07 的同一个抽取请求直接发给 Ollama 重放，

- 不带 `temperature`（即 1.0）3 次：写错的引文是 5/17、4/19、17/35，能落地的值是 12/17、14/18、9/19；
- 请求体带 `temperature: 0` 三次：结果完全相同，写错的引文 3/19，能落地的值 15/18。

8 份样本在温度 0 下的全量结果，用一个只在本机做实验、不提交的构建测量（内核发给 Ollama 的请求写死 `temperature: 0`），结果补在 PR 正文里。

## 1. 范围

**本轮只开放 `temperature`**：数，`0 ≤ t ≤ 2`（OpenAI 的取值范围），抽取传 0。

**暂不开放**（2026-10-15 逐个看过）：

| 参数 | 原因 |
|---|---|
| `seed` | 温度为 0 时用处不大；而且后端是否接受没有验证（oMLX 未核）。后端若以 400/422 拒绝，内核记为 `RequestRejected`，抽取器会把它当成「输入太长」去拆窗（`extract/run.rs:139-144`），直到失败，错得很隐蔽。等有需要且在 oMLX 上验证过再加 |
| `top_p` / `top_k` / `min_p` | 温度为 0 时不起作用 |
| `frequency_penalty` / `presence_penalty` / `repeat_penalty` | 会惩罚照抄原文，对抽取**有害**；内核不发（J6 钉住） |
| `n` | 多个答案，耗时成倍增加 |
| `logprobs` | 以后复核流程需要时再加 |
| 上下文长度 | OpenAI 兼容接口传不进去，是后端的部署设置 |
| 选择模型 | 牵涉隐私和路由，另议 |

## 2. 线上形状

`_a24/model/complete` 的 params 加一个可选字段，其余不变：

```json
{ "messages": [ … ], "response_format": { … }, "max_tokens": 4096,
  "temperature": 0,          // 可选，新增
  "complexity": "simple", "request_id": "req_…", "_meta": { … } }
```

- `ModelCompleteParams`（`model_callback.rs:69`）加 `#[serde(default)] temperature: Option<f64>`。仍是 `deny_unknown_fields`，其他采样参数照旧回 `-32602`。
- `validate`（`model_callback.rs:121` 起）加一条范围检查，越界即 `-32602`，文案「temperature must be between 0 and 2」。
- **能到达 `validate` 的只有范围错误**：
  - 字符串等类型错误由 serde 拒绝，文案是 serde 的（`invalid type: string …`）；
  - 超出 f64 范围的字面量（如 `1e309`）在 `dispatch` 解析整帧时就失败，回 `-32700`、`id: null`（`agent24-os-proto/src/rpc.rs:540-542`）；
  - JSON 里写不出 NaN 和无穷大。
- 整数 `0`、`1` 能解析成 f64，照收。`-0.0` 满足 `0 ≤ t`，原样转发，后端按 0 处理。
- 字段缺省时，行为与今天逐字节相同。

## 3. 内核

- **`CompletionRequest`**（`agent24-models/src/lib.rs:93`，派生 `Debug, Clone, PartialEq`，没有 `Eq`）加 `temperature: Option<f64>`。全程用 f64：f32 会把 0.7 写成 `0.699999988079071`，后端收到的就不是模块给的值了。
- **`OpenAiCompatProvider`**：只在 `Some` 时写进请求体，和 `max_tokens` 的「只在有值时发」同一条规则（`lib.rs:607-609`）。`None` 时请求体逐字节不变，其他调用方不受影响。
- **`into_request`**（`model_callback.rs:148`）：原样带进 `CompletionRequest`。
- **结构体字面量**：`CompletionRequest` 没有 `Default`，加字段要改全部 17 处构造点（实现时更正：S2a 合并后显式写出全部字段的是 13 处，另有几处用 `..req()` 继承，不用改；见第 1 片的 PR）：
  - `agent24-agent/src/lib.rs:219`、`:1663`，`agent24-agent/src/subagent.rs:187`；
  - `agent24-models/src/lib.rs:914`、`:927`、`:1026`、`:1030`、`:1079`，`agent24-models/src/router.rs:578`、`:920`、`:1008`；
  - `agent24-policy/src/guardian.rs:223`；
  - `agent24d/src/routes.rs:532`，`agent24d/src/model_callback.rs:163`、`:1285`。
- **用量记录**（`module_model_usage`）：不记温度，它是请求参数，不是用量。
- **重复惩罚**：内核不发任何惩罚参数。这是今天的行为，写成判据钉住（J6）。

## 4. 版本兼容：一个新模块装在老内核上

老内核的 `deny_unknown_fields` 会把带 `temperature` 的调用回 `-32602`。今天这种错配**不是边角情况**：文档模块没有第一方安装，开发时用 `agent24 os install` 手装（`rust/apps/agent24-documents/README.md:7`、`:28`）；`install` 只校验 manifest 的形状（`agent24-os-packages/src/install.rs:89-99`）；装好的包会留在 packages 目录里，跨内核升降级一直在。两种做法，**已选 (B)**（Q1）：

- **(A) 只加字段，不升协议版本**：
  - 老内核回 `-32602`，SDK 报 `ClientError::InvalidParams`（`agent24-os-sdk/src/error.rs:166-167`）。今天抽取器会落到兜底分支（`extract/run.rs:149`），以 `engine_unavailable` 失败，`message` 是「the local model: invalid params: unknown field `temperature` …」；
  - 文档模块要在 `run.rs:126-150` 加一个 `InvalidParams` 分支。`InvalidParams` 也是模块自身 bug 的出口（如空的 `json_schema.name`、超过 64 条消息，`model_callback.rs:122-138`），所以只有原文里含 ``unknown field `temperature` `` 时才提示「内核版本过旧，请升级 Agent24」，其余保留原文；
  - 不悄悄退回不带温度的调用：那样得到的结果和带温度的不同，`extractor_version` 却相同；
  - **问题只在每个 job 运行时才暴露**，开发时会反复撞到。
- **(B) 升协议版本，由 manifest 把关**：`DAEMON_PROTOCOL_VERSION` 1 → 2，版本号的含义就是「这个内核收 `temperature`」。
  - **不按连接区分**：`temperature` 是加法，v2 内核对任何连接都收，无害。所以不需要把协商到的版本存进连接、传给 handler（今天两边都没有这条管线：`ModelCompleteHandler` 只有 `generation` 和 `grant`，`model_callback.rs:718-723`；SDK 也没有暴露协商版本）。
  - 文档模块的 manifest 写 `min_daemon_protocol: 2`。老版本在**安装时**就拒绝这个包：`install.rs:95` 调用的正是同一个 `DomainOsManifest::from_yaml`（`agent24-domain/src/lib.rs:557`）。已经装好的，老内核在**挂载时**以 `ManifestUnsupported` 拒绝（`lib.rs:706-714`）。不会等到 job 运行才失败；
  - 代价：
    - 内核今天只接受 `min = max = DAEMON_PROTOCOL_VERSION`（`agent24-os-proto/src/version.rs:198-202`，测试 `:399-407` 钉死），要改成接受 `[1, 2]`；
    - SDK 的 `PROTOCOL_MAX`（`agent24-os-sdk/src/module.rs:24-25`）改为 2；
    - 握手测试跟着改，manifest 加一行。

我原先倾向 (A)；把 (B) 简化成上面的样子以后，**改为倾向 (B)**：代价只是版本范围加一，换来的是错配在安装时就暴露。这对「手装模块」这个现状正合适。David 按此选了 (B)。

## 5. SDK 与文档模块

- **SDK**：`CompleteRequest`（`agent24-os-sdk/src/clients/model.rs:56`，没有派生）加 `temperature: Option<f64>`。
  - SDK 手工拼 params（`model.rs:108-136`，`set_opt` 定义在 `clients/mod.rs:114`），`None` 时不写出，老调用方的请求不变；
  - **非有限值**（NaN、无穷大）在本地就报 `ClientError::InvalidParams`，不发出去。否则 serde_json 会把它写成 `null`，内核解析成 `None`，请求就**不带温度**地发出去了；
  - 公开结构体加字段，对直接写结构体字面量的外部 crate（Sin90）是源码不兼容，CHANGELOG 写明（同 ME4-S2 R7）。
- **文档模块**：每次抽取调用传 `temperature: 0`（`extract/run.rs:120-125`）。
  - `extractor_version` 加一：采样参数变了，同样的请求会得到不同的结果（ADR-DOC-02 §3.2「抽取器版本」）；
  - `ab/documenting-129-ground-by-quote`（改法 A，引文按原文定位）已经把版本从 2 改成 3，两者都碰 `extract.rs:18` 和 ADR §3.2 的版本条目。**合并时取下一个没用过的号，后合并的一方重新编号**，ADR 按同样的格式加一条「N：抽取以温度 0 调用模型（日期）」；
  - ADR-DOC-02 §3.2 补一句「抽取以温度 0 调用模型」。

## 6. 切法（每个 ≤ 300 行，不计测试；base 为 `ab/documenting`）

1. **`agent24-models` + `agent24d`**：§2、§3，J1–J6。
2. **`agent24-os-sdk`**：§5 的 SDK 部分与 CHANGELOG，J7、J10。
3. **（仅选 (B) 时）协议版本**：`agent24-os-proto` 的版本范围、`agent24-domain` 的常量、SDK 的 `PROTOCOL_MAX`、握手测试，排在第 4 片之前。
4. **`agent24-documents`**：传温度、`InvalidParams` 分支（选 (A) 时）或 manifest 的 `min_daemon_protocol: 2`（选 (B) 时）、`extractor_version`、ADR，J8；附 J9 的实测记录，以及 §0 的日志观察。

**与 ME4-S2a（#948）的冲突**：两者第 1 片都改 `agent24-models` + `agent24d`，都在 `CompletionRequest` 加字段、都改 `into_request`，都要改上面那 17 处构造点。语义上不冲突：两者的 `None` 都要求请求体逐字节不变。后合并的一方 rebase。
J1 只断言 `temperature` 这个键的有无和值，不断言整个请求体相等，因为 S2a 的 `progress: Some` 会多出 `stream: true`。

## 7. 判据（每条带正对照）

| # | 判据 | 正对照 |
|---|---|---|
| J1 | `temperature: 0.7` 的调用：发给后端的请求体里 `temperature` 是 0.7（按数值比较）；`0` 也按数值比较（`Value::from(0.0)` 序列化为 `0.0`） | 不带它的调用：请求体里没有这个键，与今天相同 |
| J2 | `temperature` 为 -0.1、2.1 → `-32602`，文案带范围，handler 不执行；`"0"` → `-32602`（serde 文案）；`1e309` → 整帧 `-32700` | 0、`-0.0`、0.7、2、整数 `1` 通过 |
| J3 | `seed`、`top_p`、`frequency_penalty` 仍回 `-32602` | — |
| J4 | 模块回调之外的 5 个调用方（`/api/v1/chat`、Guardian、会话摘要器、agent loop、explorer 子代理）的请求体里没有 `temperature` | — |
| J5 | `CompletionRequest` 在 `temperature` 不同时 `!=`（`PartialEq` 照样工作） | 相同时 `==` |
| J6 | 任何调用发给后端的请求体里都没有 `frequency_penalty` / `presence_penalty` / `repeat_penalty` | — |
| J7 | SDK：`None` 时 params 里没有 `temperature`；`Some(0.0)` 时写出 0 | — |
| J8 | 文档模块：抽取的每次调用都带 `temperature: 0`。选 (A) 时：内核回 ``unknown field `temperature` `` 的 `-32602`，job 以 `engine_unavailable` 失败，`message` 提示升级；选 (B) 时：manifest 要求 v2，v1 内核在安装时以 `InstallError::Source`（带 `ManifestUnsupported` 文案，`install.rs:95`）、在挂载时以 `DomainError::ManifestUnsupported`（`agent24-domain/src/lib.rs:709-713`）拒绝 | (A)：别的 `-32602`（如空的 schema 名）保留原文、不提示升级 |
| J9 | 实测（记进 PR）：同一台 M4、`qwen2.5:7b`，经内核调用（不用派生模型），同一请求重放 3 次结果相同。核实温度的手段与 §0 相同：Ollama 默认日志级别下，每次请求打印的采样参数行 `temp = …` 为 `0.000`（这是观察手段，不是自动判据） | §0 不带温度的三次重放 |
| J10 | SDK：`temperature` 为 NaN 或无穷大 → 本地 `InvalidParams`，不发出；SDK 与真实 handler 联调（同 `model_callback.rs:2340` 的写法），`temperature` 以数值落到 params | 有限值照常发出 |

变异：去掉范围检查、`None` 时也写出 `temperature`、SDK 不拦非有限值、文档模块不传温度，各自至少一条测试变红。

## 8. 残余风险

| # | 风险 | 为什么接受 |
|---|---|---|
| R-a | 温度 0 也不保证不同硬件、不同后端版本之间逐字节一致（llama.cpp 的批处理、浮点顺序） | 同一台机器上实测三次一致；验收证据本来就记录机器、后端和模型 |
| R-b | 温度 0 时，个别模型会陷入重复输出 | `max_tokens` 上限兜底；抽取器对截断的回答有拆半重问的规则 |
| R-c | 选 (A) 时，新模块装在老内核上，要到 job 运行才失败 | 报错写明要升级；或者选 (B)，在挂载时就拒绝 |
| R-d | oMLX 对 `temperature` 的处理没有验证 | OpenAI 兼容接口的标准字段；实现时若手上没有 oMLX，请 jason 在 oMLX 上试一次 |
| R-e | 远端的推理类端点（OpenAI o 系列、gpt-5 等）会以 400 拒绝不等于 1 的 `temperature`。内核会把它记成 `RequestRejected`，抽取器就当成「输入太长」去拆窗，和 §1 推迟 `seed` 的理由相同（PR-Daemon 在 #971） | 文档模块是 `model_access: local_only`，碰不到远端。以后有不是 `local_only` 的模块传温度时，要先让内核区分这种 400，或者对这类 provider 不发温度 |

## 9. 决定（David，2026-10-15）

- **Q1** 版本兼容选 (B)：协议版本 1 → 2，不按连接区分，文档模块的 manifest 写 `min_daemon_protocol: 2`，在安装和挂载时把关（§4）。所以 §6 的第 3 片要做，文档模块不加 `InvalidParams` 分支。
- **Q2** 本轮只开 `temperature`，`seed` 暂缓（§1）。
- **Q3** `temperature` 的范围 0–2。

## 改动记录

**v1 → v2**（Fable 第 1 轮，REQUEST_CHANGES，1 H / 6 M / 4 L，全部采纳）：
- H1：Ollama 的 OpenAI 兼容层在请求不带温度时写入 1.0，并优先于 Modelfile → v1 §0 用派生模型名做的「温度 0 全量」其实是 1.0，那组数字删除；本机日志核实（§0）；全量结果改用写死温度的实验构建重测。
- M1：wire 用 f64、结构体用 f32 会让 0.7 变成 0.69999… → 全程 f64。
- M2：`1e309` 在解析整帧时就回 `-32700`；SDK 把非有限值写成 `null` 会悄悄丢掉温度 → J2 改正，SDK 本地拦截（J10）。
- M3：`extractor_version` 与改法 A 的先后 → §5 写明取下一个号、后合并方重编号。
- M4：「模块包与内核同一次发布」今天不成立 → §4 按实际重写，补上 (A) 需要的 `InvalidParams` 分支和 (B) 在挂载时就拒绝的好处与 SDK 侧的代价。
- M5：与 S2a 改到同一批代码 → §3 列出 17 处构造点，§6 写明 rebase 和 J1 的断言方式。
- M6：后端拒绝 `seed` 会被抽取器误判成「太长」去拆窗 → 本轮不开 `seed`（§1）。
- L1 J2 的文案按 serde 与 `validate` 分开写，补正对照；L2 写成「OpenAI 兼容层缺省写入 1.0」；L3 修正行号，J4 覆盖全部 5 个其他调用方；L4 补 SDK 与真实 handler 的联调（J10）、按数值比较（J1）。

**v2 → v2.1**（Fable 第 2 轮，REQUEST_CHANGES，1 M / 4 L，全部采纳）：
- M1：(B) 里「按连接区分 v1、v2」两边都没有现成管线 → 改为不按连接区分，版本号只表示「内核收 `temperature`」，由 manifest 的 `min_daemon_protocol` 在安装和挂载时把关；倾向随之改为 (B)（§4）。
- L1：`InvalidParams` 也可能是模块自己的错 → 只在原文含 ``unknown field `temperature` `` 时提示升级（§4、J8）。
- L2：(B) 的协议版本改动单独成片，排在文档模块之前（§6）。
- L3：§0 的日志观察写进第 4 片的 PR 正文（§6）。
- L4：J9 写明温度是怎么核实的。
