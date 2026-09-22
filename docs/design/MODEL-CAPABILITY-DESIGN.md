# 模型能力设计 —— Agent24 × iDoris 协作草案

> **状态：DRAFT v0.2（纳入 iDoris 维护者评审），待双方拍板。** 作者：Agent24-dsh（DeepSeek Harness）。
> 日期：2026-09-20。v0.1 的四份侦察来源见 §10；v0.2 由 iDoris 侧对抗式评审批次（subagent 0c541820）修正，处置见 §11。
> 两仓各一份：iDoris `docs/17-模型能力设计-Agent24协作草案.md`、Agent24 `docs/design/MODEL-CAPABILITY-DESIGN.md`。
> **用户拍板（2026-09-20）**：D1=A、D2=A、D3=A、D4=A，D5/D6/D7 与权威模型见 §8/§8.1。**本文件以 iDoris 仓为主**；Agent24 是需求方，iDoris 是能力提供方。
> ⚠️ **必须区分「未实现」与「未设计」**：iDoris 代码为零（未实现），但它对 locality/tenancy/许可/审计**已有设计**；不得把「未实现」写成「没设计」。

---

## 0. 结论摘要

1. **Agent24 只问「角色」，iDoris 解析成「后端 + 模型 + 工件」。** 任何具体运行时（MLX/oMLX/llama.cpp/ONNX）都不得渗透进 Agent24 的 agent 层。
2. **复用已有契约，不重造**：iDoris 已有 `ProviderDescriptor` / `LoadPolicy` / `RoutingPolicy` / 控制面 header / `GET /capabilities`；本协作只做**加法**。
3. **locality 不是新契约**：iDoris 的 `local_only` 门禁已在设计中（组件卡校验器 + 运行期 fail-closed + 逐响应 `X-iDoris-Provider`）；缺口只是响应面直接回传 locality 与 `/capabilities` 增列。**fail-closed 仅对 `local_only` 生效**。
4. **按后端选预量化工件**（MLX/GGUF/ONNX 不可通约）；注册表**以角色为键**，制品标 `license/digest/min_ram_gb`，且**签名/许可归独立的模型制品层**，不得旁路。
5. **Windows 与长上下文是同一件事**：把 **GGUF/llama.cpp 做成一等公民**；MLX 在 Windows 无官方支持。
6. **顺序**：iDoris 先做 T1.1.1 骨架 → T1.1.2 契约 zod → T1.1.3 组件卡校验器 → T1.5.1 TenantContext；**跨进程纵切排第二**，且必须含 tenant/402/audit 最小用例。

---

## 1. 事实基线

### 1.1 iDoris（`/Users/jason/Dev/auraai/iDoris`，branch `preview`）
- 定位：AI 推理**准入**层；`deploy_mode: personal | tenant`，一套契约两种形态。
- 三能力：①本地订阅中转（封 `claude -p`/`codex exec`）②外部 API ③本地模型。
  - ⚠️ **① 有安全前提**：带工具会**绕过 Agent24 审批门**，必须沙箱化（T1.4.1）；tenant 模式下启动即拒绝注册。
- macOS 默认运行时 = **oMLX**（`jundot/omlx`）。
- **已有设计（非代码）**：组件卡 + 强制策略字段（`privacy_class/allowed_egress/fallback_policy/fail_closed`）；校验器反例测试（T1.1.3：`local_only⇒fail_closed`、`tier=local+locality=remote` 非法）；运行期 fail-closed（T1.3.3）；`ProviderDescriptor`；`LoadPolicy/ModelLease`；`RoutingPolicy`；`X-iDoris-*`；`GET /capabilities`；`TenantContext` + 402 + 审计；`allowed_egress` + 出网启动断言（T1.3.6）。
- 对 Agent24 的正式需求 **R1–R6**（`docs/09`）。
- 生态边界：**Agent24 做事 · iDoris 准入 · Hyphae 传话 · agentEar 听与说（含 ASR/TTS）**。

### 1.2 Agent24
- `ModelRouter::from_env()` 只造 2 个槽（oMLX + Ollama），都是 `OpenAiCompatProvider`；`Tier` 是**构造时的静态标签**（`env_local_tier` 按 host loopback 判）。
- `TaskProfile{privacy, complexity}` 两字段；`ModelProvider` = `name/complete/models`；**非流式**；无 remote/LoRA provider、无运行期切换。
- ML Worker 契约 `/v1/embed|transcribe|health`；**apps 0 消费者**，Python 侧不存在；与 memory 的 `Embedder` 无适配器。
- 24/7 用 LaunchAgent（macOS 专属）。

### 1.3 跨平台运行时（来源见 §10）
- MLX：Apple 一等；Linux CUDA/CPU 二等；**Windows 仅实验性 PR/CI**。
- Windows 可选：llama.cpp/GGUF、Ollama、LM Studio、vLLM（无原生 Win）、ONNX Runtime GenAI/Foundry Local、MLC-LLM。
- 格式不可通约：MLX safetensors / GGUF / ONNX。最低公共分母 = **OpenAI 兼容 HTTP**。

---

## 2. 分工边界

| 关注点 | Agent24 | iDoris |
|---|---|---|
| 这次任务要什么 | 入口/解决方案路由、`TaskProfile`、多模型链路 | 保留 `X-iDoris-Intent` 缺省兜底 |
| 用哪个角色/模型 | — | 角色目录、选型、量化、设备/后端 |
| 能不能调 | — | 准入、预算、租户、隐私判定、出口 |
| 记什么账 | — | 用量、成本、审计 |
| 执行 | Agent Loop、工具、审批门（C4/D3）、会话/记忆 | — |
| 不做 | 模型选型、量化、下载、计费 | 业务语义、前端、租户身份签发、agent 执行 |

- **R4 归属**：`docs/09 R4` 明写「归属由 Agent24 定」，`tasks F2.1` 后转移——本文件**引用**，不静默改写。
- **agentEar**：ASR/TTS 运行时归 agentEar，不是 iDoris 端点。
- `docs/08` 图2 的旧说法应由 iDoris 回写，**不是未决事项**。

---

## 3. 契约（在既有形状上补齐）

### L1 请求面
`POST /v1/chat/completions`（SSE、tool calls）、`GET /v1/models`（**角色名 + 模型名双暴露**，并标注哪个是稳定契约）、`POST /v1/embeddings`（归 iDoris）；rerank = **iDoris 能力**；ASR/TTS = **agentEar**。

### L2 控制面
- 请求头：`X-iDoris-Privacy/Intent/Complexity/Capabilities/Fallback/Tenant/Request-Id`。
- 响应头：`X-iDoris-Provider/Role/Model/Reason/Cost-Minor/Degraded` + **新增 `X-iDoris-Locality`**（加法）。
- `GET /capabilities`：角色目录 + `resident/estimated_memory_gb/ctx_limit/queue_depth/admission_status` + **新增 locality 列**。

### L3 必须补齐（v0.1 缺）
`deploy_mode`；`X-iDoris-Tenant` 必填、缺失 400 `tenant_missing`、**且 header 自述 tenant 跨组件不可伪造**（生态 B2）；`402 budget_exceeded`；`503 local_only_unavailable`；`TenantContext{tenant_id, budget{limit,spent,scope}, billing_timezone, quota}`；审计四类 reason + 内容黑名单；`/idoris/tenants/{id}/usage|budget|audit`；`X-iDoris-Request-Id` 幂等；`allowed_egress` + 出网启动断言；`extensions/_degradation`；`CredentialProvider`/MITM；错误体/取消语义。

---

## 4. 隐私落点（修正版）

**iDoris 侧**：`local_only` 门禁**已在设计中**——组件卡校验器拒绝 `tier=local + locality=remote`；运行期 `local_only` 请求在无本地时 503、出站计数为 0；逐响应 `X-iDoris-Provider`。

**Agent24 侧的真实缺口**：`Tier` 是静态标签（`env_local_tier` 只按 loopback），**它无法表达「iDoris 内部是否真的落了本地」**。

**修正后的最小改动（加法）**：
1. iDoris：响应头加 **`X-iDoris-Locality`**；`/capabilities` 增 locality 列。
2. Agent24：`IDORIS_URL` 的 `LocalOnly` 资格**不再靠 URL 推断**，而是**逐响应校验 `X-iDoris-Locality`**；**仅 `LocalOnly`** 在缺失/不含 local 时 fail-closed（其余流量不得因此全挂，遵守加法纪律 + 版本协商）。
3. **不用 `GET /capabilities` 预检替代 Router 内的 privacy-first gate**（否则重开已解的预算/隐私洞）。

---

## 5. 工件与动态选型（修正版）

- 按后端选预量化工件（MLX/GGUF/ONNX 不可通约）——同意。
- 注册表**以角色为键**，条目含 `backend/quant/revision/path + license + digest + min_ram_gb`；**不得**重复 `ProviderDescriptor`/组件卡/`LoadPolicy`。
- **签名/许可/版本固定归独立的模型制品层**（「未验签权重不得载入」）；注册表不得旁路 F2.1 许可门与 FU-11 许可红线。
- 舰队拆分（oMLX 聊天/工具；llama.cpp 长上下文）与 `usable=min(R×pct, R−reserve)` 公式（24GB→15.8、64GB→42.2）与 iDoris docs/13 §3.1/§3.3 逐字一致。
- 角色枚举**待统一**（`idoris-fast/daily/deep/vision` vs `fast/core/deep`）；角色名稳定、模型名可换。

---

## 6. Mac 现在 / Windows 将来

- 原则：`omlx`/MLX 字样只在 adapter 层；Agent24 的 provider 列表改**数据驱动**；后端选择/下载/量化归 iDoris。
- Windows：Phase A（客户端，成本≈0）+ **Phase B 与 GGUF 一等公民一起做**（长上下文必需，不要绑 Windows）；Windows 默认 llama.cpp/Ollama，不押 MLX；DPAPI/WFP/Job Objects 在 iDoris 侧。
- Agent24 即时要求：把「常驻服务管理」（LaunchAgent）抽成 seam。

---

## 7. 顺序（修正版）

1. **iDoris 先**：T1.1.1 骨架 → T1.1.2 五契约 zod → T1.1.3 组件卡校验器（反例测试是唯一凭证）→ **T1.5.1 TenantContext（最高优先，下游 iDoris-website 停工等它）**。
2. **跨进程纵切排第二**（不是第一交付）：一个角色 + 一个后端 + chat/completions + **落点校验**，且必须含 tenant header / 402 / audit reason 三条最小用例 + conformance 测试。

---

## 8. 待拍板决策（v0.2）

| # | 决策 | 结论 | 状态 |
|---|---|---|---|
| D0 | 权威源 | **iDoris 为主**（能力提供方）；真源 = **JSON Schema**，Markdown 是产物 + 零漂移门；Agent24 是需求方 | ✅ 已定 |
| D1 | 编排边界 | Agent24 任务→角色/链路（业务层信息最全）；iDoris 角色→模型/后端 | ✅ 已定（A） |
| D2 | 隐私落点 | 加法：`X-iDoris-Locality` + `/capabilities` 列；**仅 local_only fail-closed** | ✅ 已定（A） |
| D3 | 入口路由 | **Agent24 全权**；iDoris **不做意图推断**，但 header 缺失时用**静态缺省兜底**（不拒绝） | ✅ 已定（B） |
| D4 | ML worker | embedding 归 iDoris `/v1/embeddings`；rerank 归 iDoris；ASR/TTS 归 agentEar | ✅ 已定（A） |
| D5 | Windows | 先做 **Windows 客户端**；接口/协议必须预留「将来长出本地推理模块」的能力；本地推理延后 | ✅ 已定 |
| D6 | iDoris 语言 | **接口优先、语言无关**；可先 TS、再 Rust，甚至两种并存 | ✅ 已定方向 |
| D7 | oMLX 端口 | **8088 是自定义端口**（避开被占的 8000），未来沿用；契约默认 8088 | ✅ 已定 |

### 8.1 决策记录（用户口述要点，2026-09-20）

- **D1 / D3**：「业务层实际上 Agent 了解的信息最全，所以他做这个 router 比较合适。iDoris 只提供能力。」→ iDoris **不做意图推断**，但缺 `X-iDoris-Intent` 时**必须带一个静态缺省**（不拒绝请求）；显式 header 永远优先，**privacy 绝不自推**。
- **D5**：「iDoris 提供的就是模型能力，背后 Windows 还是 Mac 无所谓……Agent24 本身是内核，可以有多种发行版。」→ 结构上不构成问题；**工程顺序 = 先 Windows 客户端，本地推理延后**；当前只需在接口/协议上确保将来能长出该模块（模型与 runtime 进化后 Windows 端可能自然出现）。
- **D6**：「接口一定是语言无关的。先定好接口，再定语言；甚至先 TS 再 Rust、两种并存最好。」→ 先 JSON Schema 契约，实现语言后置。**本文档倾向**：先 TS（iDoris 首批任务本就是 pnpm + zod；重负载在模型服务器 oMLX/llama.cpp，路由/准入层是 I/O 型），Rust 作为同契约的后续实现。
- **D7**：「8088 是为了避开被占用的 8000，我自定义的端口；未来也用它。」→ 契约默认 8088（上游 oMLX 默认 8000）。
- **权威模型**：「以 iDoris 仓库为主，所有变化体现在 iDoris；Agent24 是需求方，提供需求与接口支持；iDoris 是能力提供方。」

---

## 9. 已更正的过期结论
- ~~「预算 vs 隐私顺序未决」~~ → **已解**：隐私判定 → 预算闸门 → 意图匹配（contract-tenancy §4）。
- ~~「iDoris 落点不可知」~~ → **已有设计**，缺的是响应面暴露。
- ~~「编排归谁未决」~~ → iDoris 已收敛，遗留只是 docs/08 图2 的旧措辞。
- 仍需处理：角色枚举三套并存；R6 的 🔴（能力①沙箱 T1.4.1）；`X-iDoris-Tenant` 不可伪造声明。

---

## 10. 来源
（同 v0.1：MLX / oMLX / llama.cpp / Ollama / LM Studio / vLLM / ONNX Runtime GenAI / MLC-LLM 官方仓库与文档；iDoris 仓 docs/00·01·02·06·09·13·14·15·16、agent/ecosystem-boundaries·contract-tenancy·tasks·progress；Agent24 仓 agent24-models·agent24-worker·SPEC-MD-ME·ADR-026）

---

## 11. 评审处置（iDoris 维护者评审 → 本文档）

| 评审项 | 处置 |
|---|---|
| S1 落点威胁模型错 | 已改：§0.3/§4，改为加法 + 区分「未实现/未设计」 |
| S2 fail-closed 越界 | 已改：§4 限定 `local_only` |
| S3 预算/隐私已解 | 已改：§9 |
| S4 契约既缺又重造 | 已改：§3 补齐；§5 注册表改角色为键 + 制品层 |
| S5 分工对账 | 已改：§2（R4 引用、agentEar 边界、docs/08 回写） |
| S6 R1–R6 未逐条 | 已补：R1/R2/R3/R6 的义务见 §3/§4/§2；R6 的 🔴 见 §1.1 |
| S7 角色枚举不一致 | 已记：§5「待统一」 |
| S8 顺序错 | 已改：§7 |
| 契约真源 JSON Schema | 已改：D0 |
