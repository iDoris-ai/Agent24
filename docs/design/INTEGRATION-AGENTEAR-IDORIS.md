# AgentEar × Agent24 × iDoris 接入规划（ADR-032 草案）

> **状态：提议中 —— §8 已记录 jason 2026-09-26 的 4 项拍板；推荐的接入方式因此从 A1 改为 A3（附着式模块）；iDoris 部分仍待 idoris 会话核对**。决策摘要登记在 [`docs/decision.md`](../decision.md) ADR-032。
> 来源：2026-09-26 Opus 只读调研 + AgentEar 会话（agentear-59）一手回复。


> 2026-09-26｜只读调研。基线：Agent24 `main@2bce2d6`、AgentEar `main@7485bbe`（v0.20.0）、iDoris 工作树 `fix/tasks-ledger-restore`（它的 `main` 上没有代码）。AgentEar 一手回复见附录 A（agentear-59 回复摘要）。iDoris 会话的回复还没到，本文里 iDoris 的部分都是按仓库推断的。

## 1. 结论

**方向是对的。** 三者的分工和各仓库已经写下的边界一致：
- AgentEar 管「听和说」（ADR-0008）；
- iDoris 管推理准入（`ecosystem-boundaries.md` §4.1）；
- Agent24 管执行、确认、记忆（ADR-028/029）。

语音模型留在本机，这点三方也早有共识。iDoris 的 §4.2 已经推翻了「ASR/TTS 走 iDoris」这个选项。

**有 4 处需要修正。**

1. **「Agent24 在中间逐轮转发」这种形态不可取。** AgentEar 已经自己编排会话（ADR-0007 §4.4 选 A，`src/session.rs`），打断靠的是进程内的播放状态。建议让 Agent24 做**能力提供方和执行方**，不做每一轮的中继：AgentEar 通过回调向 Agent24 要推理和记忆，把「动作提案」交给 Agent24 去确认、执行。
2. **流式输出两头都缺，这是头号缺口。** 推理回调 `_a24/model/complete` 明确不做流式（ME4-S2 §0、SPEC-ME3 §9），反向代理也拒绝 SSE/WS（SPEC-ME3 §9）。不补流式的话，AgentEar 换成走 Agent24 以后，首字起播中位数会从 2.80s 退回 4.86s（`AgentEar/docs/benchmarks-talk.md:146-147`）。
3. **混合模式下，LocalOnly 会在 iDoris 这一跳失守。** Agent24 只要看到端点是回环地址，就把它标成 `Local`。可 iDoris 跑在 127.0.0.1，却会把请求转发到外部 API。ME4-S2 已经把这种情况明确列为「不在保证范围内」（R13，见 SPEC-ME3 §3 的 ME-4b 注）。
4. **iDoris 还不是一个能部署的服务。** 代码全在未合并的栈式 PR #9–#37 上（`docs/agent/tasks.md:7`）。`@idoris/router` 只导出 `startRouter()`，没有 bin 或守护进程入口（`packages/router/package.json`），只绑定 127.0.0.1（`server.ts:47`），也没有鉴权。这和它自己写的「Mac mini + Tailscale」部署形态对不上。

## 2. 三方现状

### AgentEar（Rust 单二进制菜单栏 app，外加两个 Python/MLX 边车）
- **热键**：CGEventTap 监听右 Command，推键式（按一下开始、再按一下停）（`src/hotkey.rs`）。
- **ASR**：SenseVoiceSmall q8 子进程（`src/asr.rs`）；泰语走 whisper.cpp；可选 Qwen3-ASR。全部离线。
- **LLM**：OpenAI-compat `POST /v1/chat/completions`，SSE 流式，指向 `talk_llm_url`（默认 127.0.0.1:8794，MiniCPM5-2B）。请求里**没有 `model` 字段、没有鉴权头，只发单轮**（`src/talk.rs:202-245`）。
- **TTS**：VoxCPM2 边车 127.0.0.1:8765，`POST /speak`。按句流水线：出一句、合成一句、播一句。
- **对外接口**：只有 CLI。冻结的契约是 `--match-command --json` → `agentear.proposal/1`（ADR-0008 §3），只提出、永不执行。**没有 HTTP、IPC 或事件通道**（Cargo.toml 里没有任何 server 依赖）。
- **数据**：落盘在 `~/.agentear/`（raw/routes/kb/index），没有记忆子系统，这与 agentear-73 对齐过的边界一致。
- **ADR-0008 §5 的 6 个问题**：全部未拍板（`docs/agent/tasks.md:726`）。

### iDoris（TS monorepo）
- **已实现（未合并）**：
  - `/health`、`/v1/models`、`/capabilities`、`/v1/chat/completions`（`packages/router/src/server.ts:120-153`），其中 chat 支持 SSE 透传（`proxy.ts:98`）；
  - 控制面 header `X-iDoris-Privacy/Intent/Complexity/…`，缺省 privacy 为 `local_only`，fail-closed 返回 503（`handoff-agent24.md` R2/R3）；
  - 审计、预算、租户；
  - oMLX 适配器、硬件推荐（24GB 机器推荐 `ornith-1.0-9b@q6_k`）。
- **缺口**：没有守护进程入口；没有鉴权；只绑回环；台账 `progress.md` 还停在 09-07 的「无代码」状态，已经过期。
- **与 AgentEar 的关系**：iDoris 已经把 AgentEar 列为「第二个消费者，改一个 URL 就能接」（`ecosystem-boundaries.md:157-160`）。

### Agent24（Rust `agent24d`，只监听 127.0.0.1，bearer token 鉴权）
- **进程外（OOP）模块机制**（SPEC-ME3）：
  - 内核拉起子进程，入站用 fd 3 的 Unix socket 做反向代理，挂在 `/api/v1/<ns>/*` 下；
  - 出站是 JSON-RPC 回调，offer set 目前是 `{Events, Approval, Memory, Scheduler, Models}`；
  - **回调连接断开，模块就必须退出**（SPEC-ME3 §3「允许的连接数」）；
  - 不做流式代理，不跨机。
- **推理回调**（#505 已合入 main）：
  - 隐私只由 manifest 字段 `model_access` 决定，缺省 `local_only`；
  - 每个模块并发 2，令牌桶 30 次/分钟，超时 120s；
  - 不流式，也不支持 tools（ME4-S2 §0、§5.2）。
- **`agent24-models`**：`from_env()` 只接了 oMLX 和 Ollama 两个槽（`router.rs:258-298`）。`LocalOnly` 只走 `Local`/`Lora` 两层，`Local` 必须是回环地址、不走代理、不跟随重定向（`loopback_only`）。**`IDORIS_URL` 没有接线。**
- **REST `/api/v1/chat`**：写死 `TaskProfile::default()`，也就是 `Privacy::Any`，而且不流式（`agent24d/src/routes.rs:111`）。
- **审批**：`_a24/approval/gate` 里「内核可执行动作」是**空集**，只有 `advise` 能用（SPEC-ME3 §3 方法表、§6.1）。所以 AgentEar 提案里的 `http_post`、`open_url` 目前没有由内核执行的路径。
- **记忆**：长期记忆归 M-D（ADR-028），AgentEar 不自建记忆子系统（已确认）；模块只能用 `_a24/memory/private/*`。

## 3. 推荐架构

```
 麦克风 ─► [AgentEar 模块 · 用户本机]────────────────────────────────────┐
           热键/采集/VAD/ASR/会话状态机/打断/TTS/播放（全在本进程和本机边车）│
             │① _a24/model/complete(流式,待补)  ② _a24/events/emit       │
             │③ _a24/approval/advise|gate        ④ _a24/memory/private   │
             ▼   JSON-RPC over Unix socket（A24_CALLBACK_SOCK）          │
 ┌──────────────── agent24d（协调者 / 执行者）─────────────────┐  ⑤ 反代 │
 │ ModelRouter: privacy 由 manifest 定 → tier_order            │ POST /api/v1/agentear/speak|stop
 │  ├ Local: oMLX 127.0.0.1:8088 (loopback_only)               │◄───────┘
 │  ├ Local: "idoris-local"  = IDORIS_URL + 强制 X-iDoris-Privacy: local_only
 │  └ Remote:"idoris-any"    = IDORIS_URL + X-iDoris-Privacy: any
 │ 审批/执行/回执/历史 · M-D 记忆 · WS /api/v1/events → 外壳(Electron/Pet0)
 └────────────┬───────────────────────────────────────────────┘
              │ OpenAI-compat REST + X-iDoris-* header（SSE）
              ▼
 [iDoris 网关] ─ local_only → oMLX/本机 Qwen 2B/7B/27B（fail-closed 503）
              └ any/complex → 外部 API（审计·预算·凭证在 iDoris）
```

**每条边：**
- **①** 模块调推理，协议是 OOP 回调 JSON-RPC。
- **②** 模块向外壳推事件（transcript/proposal），经 WS 到外壳，协议是 OOP 回调加 WS。
- **③** 模块提交提案，交给 Agent24 确认和执行。
- **④** 私有记忆，用来存多轮上下文。
- **⑤** 外壳或内核让 AgentEar 播报、停播，走 REST 反向代理。
- **Agent24 → iDoris**：OpenAI-compat REST。

**谁调谁：** 每一轮都是 AgentEar 主动发起；Agent24 只在 ⑤ 这一条边上反向调 AgentEar。

**两种模式：**
- **纯本地**：manifest 写 `model_access: local_only`。路由器只走 `Local` 层（oMLX，或者用 idoris-local）。iDoris 不在线时直接走 oMLX，不影响可用性。
- **混合**：manifest 写 `remote_allowed`，由 AgentEar 按每轮传 `complexity`（simple 本地优先，complex 远端优先），选哪家 provider 由内核定。结果里的 `tier` 字段告诉模块这一轮有没有出本机。

**隐私边界，四层都要执行：**
1. 音频和原始转写只留在 AgentEar 本机；
2. Agent24 按 manifest 定 `Privacy`（ME4-S2 §2.2）；
3. Agent24 调 iDoris 时**必须带** `X-iDoris-Privacy: local_only`，这样才能堵上 R13；
4. iDoris 的 `dispatch` 在没有本地候选时 fail-closed 返回 503。

另外，iDoris 要在响应里返回实际落点（例如 `x-idoris-served-locality`），让 Agent24 的事后绊线能核对（ME4-S2 §2.2 的最后一行）。

## 4. 集成方式比较

### A. AgentEar 怎么接 Agent24

| | A1 OOP 模块（推荐） | A2 外部客户端调 REST |
|---|---|---|
| 隐私 | 由 manifest 强制 LocalOnly | `/api/v1/chat` 固定是 `Any`，要改内核 |
| 能力 | 事件、审批、记忆、推理全都有 | 只有 chat、runs、approvals |
| 生命周期 | 内核拉起；连接断开模块就退出 | AgentEar 独立，但依赖宿主在线 |
| 风险 | TCC 归属、开机自启冲突、两种运行形态（见 §7） | 要自管 token 发现，隐私要另外补 |

**推荐 A1。** AgentEar 自己也倾向 A1（附录 A §4）。前提是 AgentEar 保留「独立运行」模式：它检测到有 `A24_CALLBACK_SOCK` 就进入模块模式，否则维持现有行为。

### B. iDoris 怎么接 Agent24

| | B1 作为 `agent24-models` 的 OpenAI-compat provider（推荐） | B2 作为 OOP 模块 |
|---|---|---|
| 依据 | iDoris 的 R1 需求、Agent24 汇总 §9 I1，纯加法 | 模块不能对内核「提供」推理，方向反了 |
| 隐私 | 路由器和 iDoris 双重门禁 | 绕开 ModelRouter，是第二套策略 |
| 流式 | iDoris 已经支持 SSE | 反向代理拒绝 SSE |

**推荐 B1**，并且拆成两个逻辑 provider：idoris-local 标 `Local`，idoris-any 标 `Remote`。

## 5. 分阶段落地

| 阶段 | 内容 | 验收判据 | 与 ME-4 的衔接 |
|---|---|---|---|
| **P0 拍板** | 回答 ADR-0008 §5 的六问（建议见 §7）；定事件和命令 schema `agentear.event/1` | 两边仓库各落一份 ADR | 不占 ME-4 资源，可以马上做 |
| **P1 零代码直连** | 把 AgentEar 的 `talk_llm_url` 指向 iDoris（本机拉起 `startRouter`） | `--ask` 在 local_only 下 200；把本地后端关掉后返回 503、出站计数为 0 | 前提是 iDoris 合并 PR 并补出守护进程入口；与 Agent24 无关 |
| **P2 AgentEar 做成模块（非流式）** | manifest 声明 `models`/`events`/`approval`/`memory`、`model_access: local_only`；fd 3 起 HTTP；emit transcript/proposal；`/speak` `/stop` | 黑盒测试：挂载 → 按键 → 事件到 WS → 推理 `tier=local` → 播报；kill agent24d 以后 AgentEar 退出 | 排在 ME4-M5 的 SDK（`agent24-os-sdk`）之后，作为 SDK 的**第三个真实调用方**；也可以直接照 WIRE 文档实现，顺便检验 T14 |
| **P3 流式推理** | 新增 `_a24/model/stream`，或用 notification 增量推送 | 首字起播中位数 ≤ 3.0s（今天直连是 2.80s），n≥10 | **排在 v0.5.0 之后**；这需要新设计（SPEC-ME3 §10-2） |
| **P4 iDoris provider** | `IDORIS_URL` 双槽 + header 映射 + 落点校验 | LocalOnly + iDoris 只有外部 provider → Unavailable，外部桩计数 0（带正对照） | P4 门后的「iDoris 主 AI 接入」需要用户拍板 |
| **P5 提案执行** | gate 闭集加 `open_url`/`http_post`（凭证在 Agent24）；外壳做确认 UI 和回执 | 念出来的 prompt 与实际执行的内容同源（ADR-0008 §3 硬约束 1），拒绝后零执行 | 属于 T7c，还没排期 |

## 6. 可直接转发的诉求

**→ AgentEar**
1. 做「模块模式」：从 fd 3 接入站 HTTP（可以用 SDK 的 `listener_from_env`）；**读到回调 EOF 就退出**；独立模式保持不变。
2. 定义事件：`transcript{text, lang, content_hash}`、`proposal`（直接复用 `agentear.proposal/1`）、`turn{phase}`；定义命令：`POST /speak{text, lang, voice?}` 和 `POST /stop`。全部带 schema 版本。
3. LLM 调用抽成可替换的 transport：独立模式走 HTTP；模块模式走 `_a24/model/complete`，并带上 `complexity`。
4. `sidecar::probe` 现在只认 `mlx-dspark` 这个身份标识（`src/sidecar.rs:631`），接 iDoris 时要加 iDoris 的身份标识。另外请求里要补上 `model` 字段。
5. 继续不做记忆和执行。多轮上下文放进 `_a24/memory/private`，或者等 Agent24 的会话回调。

**→ iDoris**
1. 先合并主干，再提供守护进程入口：`idoris serve --port`，加 launchd 配置和 bearer 鉴权。如果要跨机访问（Tailscale），需要显式的绑定开关，并且必须开鉴权。
2. 响应头返回实际落点，例如 `x-idoris-served-locality: loopback|external`，以及 `x-idoris-provider-tier`。
3. SSE 透传要覆盖 local_only 这条路径；在元数据里保证 `stream: true` 时的首包延迟不高于直连 oMLX。
4. `/health` 返回可识别的服务身份，供 AgentEar 和 Agent24 校验对端。
5. 更新 `progress.md`，避免下游按过期的「无代码」状态做判断。

**→ Agent24**
1. 回答 ADR-0008 §5 的六问，并在 Agent24 仓库落一份 AgentEar 集成 ADR。目前 Agent24 仓库里搜不到任何 AgentEar 相关文档。
2. `_a24/model` 支持流式，设计时要一并考虑背压、取消、计量。
3. `ModelRouter` 加 `IDORIS_URL` 双槽和 `X-iDoris-*` 映射，关掉 R13 的缺口。
4. gate 闭集的首批条目：`open_url`、`http_post`（凭证、回执、留档都在 Agent24）；外壳订阅 `agentear` 事件并提供确认 UI。
5. 考虑增加 `_a24/session/*` 或 `_a24/agent/turn`，让语音对话进入 Agent24 的会话和记忆，而不是由模块自己拼上下文。
6. 生命周期要支持**附着到已运行的 GUI app**，或者由 app 包启动。这一条取决于 §7 里 TCC 的 spike 结论。

## 7. 风险与待拍板

| # | 问题 | 建议或现状 |
|---|---|---|
| 1 | **延迟预算** | 目标首字 ≤3s，瓶颈在 TTS（约 2.4s，附录 A §5）。每加一跳进程间通信大约是毫秒级，真正的风险是没有流式。**待拍板**：P2 是否接受非流式先上线。 |
| 2 | **热键和麦克风归属** | 建议都归 AgentEar。外壳不装 CGEventTap，打断语义保持不变。 |
| 3 | **TCC** | **待确认，需要 spike**：agent24d 拉起的子进程，麦克风和辅助功能权限算在谁头上（macOS 按「负责进程」归属）。如果算到 agent24d，用户就要重新授权，而且权限会挂在 Agent24 上。 |
| 4 | **自启冲突** | AgentEar 自带 LaunchAgent（`src/launch_agent.rs`），Agent24 也要拉起它，可能出现双实例。需要约定：模块模式下关闭 AgentEar 自己的自启。 |
| 5 | **流式输出** | 同 §1 第 2 条，需要排期拍板。 |
| 6 | **凭证与计费** | 外部 API 的 key 和预算归 iDoris；执行类凭证（Notion、邮件）归 Agent24；AgentEar 不持有任何凭证。按模块的用量看 `GET /api/v1/usage?module=`，按租户的用量看 iDoris 的账本，两边口径要对齐（**待确认**）。 |
| 7 | **离线降级** | iDoris 不在线 → 走 Agent24 直连 oMLX；Agent24 不在线 → AgentEar 回到独立模式，用自己的边车。**待拍板**：独立模式要不要保留 MiniCPM 边车（占 1–2GB）。 |
| 8 | **Mac mini 部署** | 按 Agent24 的定义，非回环一律算 `Remote`，所以 LocalOnly 永远不会发到 Mac mini 上的 iDoris。**待拍板**：LAN/Tailscale 上的节点算不算「本地」。目前 ME4-S2 明确判为 Remote。 |
| 9 | **配置归属** | 建议 AgentEar 的模型、音色、`commands.json` 仍以它自己的配置为唯一真相，Agent24 只读展示。回执留档归 Agent24。 |


## 8. jason 拍板（2026-09-26）与修订

| # | 问题 | 拍板 | 影响 |
|---|---|---|---|
| D1 | P2 能否先上非流式 | **可以先上非流式** | P2 不等 P3；流式 `_a24/model` 仍排在 v0.5.0 之后 |
| D2 | LAN / Tailscale 上的节点（如 Mac mini）算不算本地 | **算远程** | 维持 ME4-S2 的判定：非回环一律 `Remote` |
| D3 | AgentEar 独立模式是否保留自带小模型 | **保留**（例如 2B）。能独立运行，粗陋一点可以接受；需要更强能力时接 iDoris。**全部可配置** | AgentEar 的 LLM transport 做成三档可选：自带边车 / 直连 iDoris（独立模式）/ 经 Agent24 `_a24/model`（附着模式） |
| D4 | 麦克风、辅助功能等 TCC 权限归谁 | **留在 AgentEar**。它是外置的，去掉它不影响 Agent24 原有体验（手动打字照常可用） | 见下面的 A3 |

**「本地 / 远程」的含义（回答 jason 的追问）**：在 Agent24 里，这对概念划的是**隐私边界**，与模型能力无关。它只回答一个问题：这次请求的内容会不会离开当前这台电脑。端点在回环地址（127.0.0.1）上，且不走代理、不跟随重定向，就算 `Local`；其余都算 `Remote`，包括用户自己的 Mac mini。模型的强弱由另一个维度决定：每次请求带的 `complexity`（simple / complex）决定在允许的层里优先选哪一层。两个维度互相独立：`Privacy` 决定请求**能去哪**，`complexity` 决定在能去的地方**选谁**。以后如果要让自家内网节点获得中间档的信任，需要新增一个级别（例如「受信内网」），另行决策，本次不做。

**AgentEar 用哪个模型的分层（回答 jason 的追问）**
- 语音模型（ASR、TTS）**由 AgentEar 自己管**：首字延迟的瓶颈在 TTS，音频也不能离开本机。iDoris 在 §4.2 里已经排除了承接语音的方案。
- 大模型分层：独立模式用自带的小模型；需要更强能力时，独立模式下直连 iDoris（AgentEar 已是 OpenAI-compat，改一个 URL 即可），附着到 Agent24 时经 `_a24/model`，由 Agent24 统一管隐私和用量。三种方式都通过配置切换。

### A3 附着式模块（因 D4 替换 §4 推荐的 A1）

由 Agent24 拉起 AgentEar（A1）时，macOS 的 TCC 按「负责进程」归属，麦克风和辅助功能权限**很可能**会算到 agent24d 头上，这与 D4 冲突。改为：

| | A1 由内核拉起 | A2 外部 REST 客户端 | **A3 附着式模块（新推荐）** |
|---|---|---|---|
| TCC 归属 | 可能归 agent24d（待 spike 验证） | AgentEar ✅ | AgentEar ✅ |
| 能用到的能力 | 全部 | 只有 chat 等少数几项，且 Privacy 固定为 `Any` | 全部，和 A1 相同 |
| 独立性 | 依赖内核生命周期 | 独立 | 独立：Agent24 不在时自动回到独立模式 |
| Agent24 需要做的改动 | 无 | 按模块的 token 和隐私设置 | **新增「附着」生命周期**：模块自行启动，用注册 token 连上 Agent24 的回调端点，握手之后拿到和 OOP 模块相同的 offer set。manifest、授权、`model_access` 规则不变 |

**A3 在 Agent24 侧要先设计的内容**（作为 P2 的前置，单独出一份设计文档，走对抗评审后冻结）：
- 注册 token 怎么发放、怎么吊销；
- 回调端点用 Unix socket 还是 loopback HTTP，以及怎么鉴权；
- 附着模块的 generation、draining、重连语义（和内核拉起的模块相比，「回调断开就退出」这条规则要改）；
- 反代入站（`/api/v1/agentear/*`）怎么路由到一个不是内核拉起的进程；
- 同一个模块不能同时以「拉起」和「附着」两种方式存在。

**P0 新增**：用一个 spike 实测 A1 下 TCC 的实际归属。spike 需要在图形界面里点授权弹窗，要 jason 手动配合。如果实测 TCC 归属 AgentEar 自己，A1 仍然可用，A3 就降为可选项。

---

## 附录 A：AgentEar 会话（agentear-59）一手回复摘要


## 1. 已实现的链路
- **形态**：Rust 单二进制守护进程（菜单栏 app，有设置窗口，开机自启 src/launch_agent.rs），另有两个独立的本地 Python/MLX 边车（HTTP，127.0.0.1）。
- **热键**：CGEventTap（src/hotkey.rs）。
  - 右 Command 单击 = 输入法模式，双击 = 对话模式。
  - 推键式：按一下开始，再按一下停；按键会打断正在播放的内容。这是 jason 拍板的 V1 终态。
- **VAD**：只有 ASR 内的 FSMN-VAD 负责切段，不做自动起停。
- **ASR**：
  - 默认 SenseVoiceSmall q8，经 llama-funasr-sensevoice 子进程（src/asr.rs:140）。
  - 泰语用 whisper.cpp + Thonburian。
  - 可选 Qwen3-ASR（speech_swift）。
  - 全本地、离线。
- **对话链路**：ASR 文本 → OpenAI-compat `/v1/chat/completions`，SSE 流式（src/talk.rs:181/:231）→ 默认 127.0.0.1:8794 上的 mlx_lm.server 跑 MiniCPM5-2B-4bit。改 `talk_llm_url` 可以接任何 OpenAI-compat 端点。回答按句子流水线：出一句、合成一句、播一句。
- **TTS**：VoxCPM2（MLX），HTTP 边车 127.0.0.1:8765，接口 `POST /speak {text, lang: zh|en|th, voice?, style?, tone?}` → wav。播放用 afplay，兜底用 `say`。
- **M2 理解层**：术语纠错和打标签（Ornith-9B），默认关闭。

## 2. 对外接口
- 没有 HTTP、WS、IPC 事件通道。只有 CLI：`--transcribe`、`--ask`、`--say`、`--talk-turn`、`--diagnose`。
- 冻结契约 `agentear --match-command "<文本>" --json` → `agentear.proposal/1`（src/main.rs:609/:639；字段表和两条硬约束见 ADR-0008 §3）：
  - 只提出，永不执行；
  - `needs_confirm` 表示「会不会把内容送出本机」；
  - prompt 与执行内容同源。
  - 假宿主：scripts/agent24-standin.py。
- 两个边车的 HTTP 是内部依赖，没有冻结。
- **落盘位置**：~/.agentear/
  - raw/audio（L0，不可重建）
  - derived/transcripts（纯文本）
  - routes/YYYY-MM/<sha256>.json（src/route.rs:60），字段：content_hash, created_at, label, label_source, confidence, secondary[], text, delivery{state, attempts, last_error, location}
  - kb/**/*.md（可从 routes 重放）
  - derived/index.sqlite（FTS5）
  - 转写没有时间戳，也没有分段。

## 3. ADR-0008 §5 六个问题：全部未回答
1. 事件通道：a stdout JSONL / b 本地 HTTP / c WS。倾向 a 起步、b 长期。
2. 热键归属：两个 app 都装 CGEventTap 会冲突，必须只有一方录音。
3. 麦克风与播放归属：打断依赖进程内的播放状态。
4. 模型、音色、commands.json 的配置归属：接管还是只读。
5. TCC：麦克风和辅助功能权限按 app 授予。采集留在 AgentEar，Agent24 就不需要麦克风权限。
6. 回执留档：AgentEar 不落库，倾向归宿主。

## 4. AgentEar 的倾向（待 jason 拍板）
- AgentEar 作为 Agent24 的 OOP 模块：Agent24 拉起并托管它，通过 manifest + JSON-RPC 回调通道收事件。
- AgentEar 继续持有热键、麦克风、播放：改动最小，打断语义不变，TCC 权限不用迁移。
- 向宿主推 `transcript` / `proposal` 事件；接收 `speak` / `stop_playback` 命令。
- 单独运行（不挂在宿主下）时保持现有行为。
- 不倾向让 AgentEar 作为外部客户端调 Agent24 REST，因为那样 AgentEar 会依赖宿主是否在线。

## 5. 对 Agent24 的诉求和约束
**诉求**
1. 确定的事件和命令契约，带 schema 版本。
2. 确认 UI、执行、回执的展示和留档，由宿主负责。
3. `_a24/model/complete` 若能提供 OpenAI-compat 语义或流式输出，AgentEar 就可以去掉 MiniCPM 边车，省下 1–2GB 常驻内存。但首字延迟必须靠流式：非流式首字中位数 4.86s，流式 2.80s（docs/benchmarks-talk.md）。
4. 用私有记忆接对话上下文：现在每轮之间不记忆，2B 模型也撑不住多轮。

**约束**
- TCC 权限按 app 授予。
- 热键全局只能有一方持有。
- 打断依赖进程内的播放状态。
- 内存：ASR ≤2GiB（高资源档 ≤4GiB），V1 整体 ≤10GB。
- 隐私：语音和转写不出本机，不能静默走云。
- 延迟：首字起播中位数 2.8s，瓶颈在 TTS 合成（约 2.4s）。

## 6. 离线程度
**已做到**：ASR、LLM、TTS 全本地运行。首次需要联网下载模型，之后可以离线。

**还缺**
- AEC：没有回声消除，所以只能推键式。
- TTS 不能真流式。
- 泰语 LLM 不稳。
- 方言 TTS 还没验收。
- 录音中途崩溃会丢整段。
- 没有离线首装流程。
