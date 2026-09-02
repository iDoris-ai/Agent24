# Agent24 设计与任务：iDoris 集成 + 入口路由模型

> 文档类型：设计考量 + 工作任务（供 Agent24 自己的 P4 规划采纳）
> 日期：2026-07-30 ｜ 作者：iDoris 侧规划输入（@jhfnetboy）
> 关联：iDoris 仓库的设计文档 `docs/`（01–09，尚未合并到该仓库 main，故此处不给链接）；本项属 Agent24 **P4 门后"iDoris 主 AI 接入"**，排期需 Agent24 侧用户拍板。
>
> **定位**：iDoris 是 Agent24 之外的独立"个人 AI 网关"服务（对外一个 OpenAI-compat 本地 URL）。Agent24 是前端用户交互入口（Electron + Rust 内核），经进程边界(REST)消费 iDoris。本文列 Agent24 侧要考虑的设计 + 工作任务。**均为加法迁移，零回归。**

---

## 一、架构定位（左 agent / 右 model）

```
用户输入 ──▶ [左: Agent 侧]                         [右: Model 侧]
             入口路由模型(0.1B) ──决策解决方案──▶  纯 agent / 搜索 / 发邮件 /
             (Semantic Router)                       引入模型(哪个/多模型链路)
                    │                                        │
                    └────────── ModelRouter ────────────────┘
                         (tier/隐私/health + IDORIS_URL provider)
                                    │ REST(OpenAI-compat)
                                    ▼
                            iDoris 统一服务(三能力)
```
- **入口路由模型在左(agent/入口)**：按用户输入动态决定"走什么解决方案"。
- **它本身也是一个 model**：纳入 provider 池（右），由 ModelRouter 当作一个小 provider 调用。
- **ModelRouter 是枢纽**：既消费入口路由的决策，也经 `IDORIS_URL` 调 iDoris。

---

## 二、设计考量（来自 iDoris 需求 R1–R6，加法）

| # | 设计点 | 要点 | 验收 |
|---|---|---|---|
| D1 | **`IDORIS_URL` provider** | `ModelRouter::from_env()` 加可选 `IDORIS_URL`/`IDORIS_API_KEY`，把 iDoris 当 OpenAI-compat provider；未配置行为不变 | 配了走 iDoris；不配与今日一致；gen:api 无漂移 |
| D2 | **控制面 header** | 把 `TaskProfile{privacy,complexity,intent,capabilities,fallback}` 映射为 `X-iDoris-*` header（不塞 prompt）| iDoris 端能读并据此路由；无 header 有 fail-closed 默认 |
| D3 | **LocalOnly 贯穿** | LocalOnly 任务经 iDoris 也必须只落本地、无本地则报错 | LocalOnly+无本地 → Unavailable，绝不 external |
| D4 | **跨平台后端(不绑 oMLX)** | capability③ 后端按 OS 选：macOS→oMLX、Win/Linux→vLLM/llama.cpp/Ollama；均 OpenAI-compat+LoadPolicy | 同一 Agent24 二进制三平台都能起本地模型层 |
| D5 | **复用审批门** | 经 iDoris 的危险动作照走 C4/D3/审计，不新开绕过路径 | iDoris 路径危险动作在 daemon 侧弹审批 |

---

## 三、入口路由模型（本次新增的核心工作）

**目标**：一个 ~0.1B 入口小模型，按用户输入动态路由到**解决方案**：纯 agent / 搜索 / 发邮件 / 是否引入模型 / 引哪个 / 多模型链路。**基于成熟开源，不自造。**

**开源选型（已调研）**：
| 方案 | 规模 | 适配 | 采用建议 |
|---|---|---|---|
| **[Semantic Router](https://github.com/aurelio-labs/semantic-router)**（aurelio-labs）/ [vLLM Semantic Router](https://github.com/vllm-project/semantic-router) | 嵌入级(~0.1–0.3B，**无需 LLM 调用**) | 定义"路线"=解决方案，按语义相似度即时路由 | **首选**——最贴合"0.1B 入口、快、路线=方案" |
| [Arch-Router-1.5B](https://huggingface.co/katanemo/Arch-Router-1.5B)（katanemo）| 1.5B | 生成式，Domain+Action 偏好路由 | 需更细语义时备选 |
| [LLMRouter](https://github.com/ulab-uiuc/LLMRouter)（ulab-uiuc）| 库 | 16 种路由算法（KNN/SVM/BERT…）| 自建路由器时参考 |
| RouteLLM | — | 仅强/弱模型二选 | 不够（只做模型选择）|

**设计要点**：
- 入口路由是 Agent24 的**前端用户交互入口职责**（用户输入第一站）。
- 路线（routes）声明式配置：`agent-only` / `web-search` / `send-email` / `local-model:<id>` / `chain:<a→b>` …（可扩展）。
- 与 D2 控制面一致：路由决策 → 产出 TaskProfile/意图 → 交 ModelRouter 执行。
- 入口小模型（嵌入模型）也登记为 provider 池的一个 model（左用其决策、右作 provider）。

**验收**：给若干代表性输入（"帮我查天气"→搜索；"给张三发邮件"→email；"推理这道题"→本地大模型；"总结这篇+配图"→多模型链路），入口路由器分类正确并产出对应解决方案标签。

---

## 四、动态硬件推荐模块（低优先，留 TODO）

- **是什么**：按机器 RAM/芯片/OS + 模型目录，推荐常驻/临时/量化组合（算法与标准见 iDoris 仓库 `docs/07-模型量化内存评估与动态推荐.md`；该文档尚未合并到 iDoris main，合并后再补链接）。
- **定位**：**非核心、不着急**（用户 2026-07-30 明确）。macOS 上 oMLX 的 memory-guard 已能兜底；跨平台再补。
- **动作**：**留 TODO**，不在本轮排期。真要做时作为 ModelRouter 的 `HardwareProfile+recommend()` 扩展或独立 util。

---

## 五、建议工作任务（供 Agent24 TASKS.md 采纳，P4 门后）

> 顺序：入口路由(用户入口，最有价值) → IDORIS_URL 接线 → 控制面/隐私 → 跨平台后端 → 硬件推荐(TODO)。

| ID | 任务 | 依赖 | 优先 |
|---|---|---|---|
| ID-1 | 入口路由模型集成（Semantic Router，路线=解决方案，产出意图/TaskProfile）| — | 高 |
| ID-2 | `IDORIS_URL` provider 接入 ModelRouter（D1，加法）| — | 高 |
| ID-3 | 控制面 `X-iDoris-*` header + LocalOnly 贯穿（D2/D3）| ID-2 | 中 |
| ID-4 | 跨平台后端选择（D4；macOS=oMLX，Win/Linux=vLLM/llama.cpp）| ID-2 | 中 |
| ID-5 | 经 iDoris 危险动作复用审批门验证（D5）| ID-2 | 中 |
| ID-6 | 动态硬件推荐模块 | ID-4 | **TODO(低)** |

**门约束**：本组属 Agent24 **P4 门后**（"iDoris 主 AI 接入"），且 iDoris 服务本体尚在建（U0 已验证可行）。**排期与开工由 Agent24 侧用户拍板**；本文只提供设计与任务候选。
