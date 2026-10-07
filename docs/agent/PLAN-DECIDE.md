# PLAN-DECIDE — 决策服务 `agent24-decide` 任务拆解

> 立于 2026-10-07。jason 已同意 [`../research/DECISION-MODELS.md`](../research/DECISION-MODELS.md) §7 的方案与 D0–D3 分段，并追加两条要求：
> 1. **按用户电脑自动适配**，不同硬件拿到不同的决策模型组合；
> 2. **从第一天起积累数据**，为将来训练用户自己的决策模型（个人版 Jev）打基础。
>
> 本文只做任务拆解与验收定义；方案的「为什么」见调研台账 §7，不在这里重复。执行状态以 [`tasks.md`](tasks.md) 的「DECIDE 台账」为准。

## 0. 不变的约束（来自调研台账 §7，实施时不得放宽）

- A 类判断（WriteGate、来源标记、owner/active 过滤、会话导入、Authorizer、capability 令牌、会话视图）**永远不进决策服务**。决策服务的输出只能触发「建议」或「更严格」的动作。
- 后端不可用时返回 `unavailable`，**不静默降级**；LLM 模拟出的结果标 `backend=llm_simulation`，不冒充校准概率。
- 模型与运行时是**按需下载组件**（pin 版本 + sha256，同 Open Design 组件机制），不进安装包。
- 接口归 Agent24，后端长期迁往 iDoris 网关；迁移时只换实现。

## 1. 新增要求一：按硬件自动适配

### 1.1 机制

```
HardwareProbe ──► HardwareProfile ──► TierPolicy ──► 每个决策点的后端组合
 (RAM/芯片/GPU/      (确定性代码)      (查模型目录)     (用户可覆盖)
  磁盘/OS/电源)
```

- **探测是确定性代码，不是模型**：内存总量与可用量、CPU 架构与核数、加速器（Apple Metal / CUDA / 无）、磁盘余量、OS、是否电池供电。
- **模型目录** `decide-models.catalog.json`：每个候选写明 HF 仓库 + 固定 revision + sha256、许可证、下载体积、常驻内存、各档实测 P95 延迟、适用决策点、所需运行时（进程内 ort/candle 或 oMLX/Ollaya）。目录里的延迟和内存数字**只能来自 D0 实测**，不抄模型卡。
- **分档（草案，D0 实测后定稿）**：

| 档位 | 典型机器 | 快速层（每条消息都跑） | 深度层（复杂判断，可选） |
|---|---|---|---|
| **T0 仅规则** | 未同意下载 / 离线 / 磁盘不足 / < 8GB | 规则 | 无 |
| **T1 轻量** | 8–16GB，无独显或省电模式 | int8 小 encoder（CPU） | 无 |
| **T2 标准** | 16–32GB Apple Silicon（如 Mac mini M4 24GB） | encoder + Qwen3Guard-0.6B | Kev-0.8B 类（oMLX/Metal，按需加载） |
| **T3 充裕** | ≥ 32GB（如 M1 Max 64GB） | 同 T2 | Kev-4B 类；具备本地微调能力（见 §2.4） |

- **用户可见、可覆盖**：设置页显示「本机档位 + 每个决策点用的模型 + 为什么」，用户可降档或关闭；硬件变化（换机、内存压力持续告警）时重新推荐，但**不自动升档下载**，要用户确认。
- **与 iDoris 的关系**：iDoris 规划里的 `HardwareAwareModelRecommender`（iDoris/docs 07）先在这里落地一个只服务决策模型的版本，接口按可迁移设计，后续由 iDoris 网关接管全模型推荐。

## 2. 新增要求二：积累数据，为个人决策模型打基础

### 2.1 决策日志（D0 就定 schema，D1 起正式记录）

每次决策写一条，字段从一开始就按「可直接导出训练」设计：

| 字段 | 说明 |
|---|---|
| `decision_id` / `ts` / `schema_version` | 稳定标识 |
| `point` | 决策点 ID（`retain.intent`、`recall.gate`、`guardian.risk`…，对应盘点编号） |
| `input` | 用户原文（本地保存）+ 结构化上下文特征（不存整段对话） |
| `question` | choice/noul/score 题面与候选标签 |
| `layers[]` | 每层的 backend、模型 id+revision、标签、概率、耗时 |
| `final` | 最终动作：执行 / 弃权 / 反问 / 交人 |
| `hw_tier` | 当时的硬件档位 |
| `outcome` | 事后标签（见 2.2），可为空，可多次追加 |

### 2.2 标签从哪来（不打扰用户）

| 信号 | 标签含义 | 质量 |
|---|---|---|
| 用户回答了澄清问题（「你是想让我记住吗？」→ 是/否） | 强标签 | 高 |
| 用户撤回刚写入的记忆 / 说「不对」「我没让你记」 | 判错 | 高 |
| 审批被拒 / 被批 | 风险判断的对错 | 中 |
| 写入后被正常召回且未被纠正 | 弱正例 | 低（只做辅助） |

### 2.3 隐私与主权（硬约束）

- 日志**只存本地**（agent24 store），永不上传；不进遥测。
- 用户可查看、导出（JSONL）、整体或按条删除；与记忆「清除权」ADR 同一套语义。
- 保留期默认有界（具体天数 D0 定），超期只保留去掉原文的统计。
- 联邦学习（只传 LoRA、同 base，见 iDoris 规划）是更后面的事，**必须单独征得同意**，本计划不实现。

### 2.4 个人模型训练（D4，数据够了才开工）

- 门槛：某决策点累计 ≥ N 条带高/中质量标签、且正负两类都有（N 由 D1 数据量定，初估 200）。
- 方法：在 T3（或用户自愿的 T2）机器上做 SetFit 头 / encoder LoRA（MLX），不动 base。
- **晋升门**：个人模型必须同时满足：通用中文评测集不退化、危险操作漏判仍为 0、用户留出集上优于全局模型。不达标就不启用，继续用全局模型。可一键回退。

## 3. 任务拆解

分工惯例：Opus 写规格、评测集和验收；Sonnet 写代码（最多 3 并发）；PR-Daemon 评审。

### D0 — 接口 + 评测台 + 硬件适配 + 日志 schema（约 1 周）

| ID | 内容 | 负责 | 依赖 | 验收 |
|---|---|---|---|---|
| D0-1 | `agent24-decide` crate（L3）：`DecisionRequest` / `Question{choice,noul,score}` / `Decision{label,p,backend,abstain}`、`DecisionBackend` trait、`RuleBackend` 适配器、`Unavailable` 语义；简短 ADR 说明分层位置 | Sonnet | — | 单测覆盖三种题型、不可用路径、`llm_simulation` 标记；`cargo test -p agent24-decide` 绿 |
| D0-2 | 决策日志：store 迁移 + 写入接口 + `agent24 decide export --jsonl` + 删除接口 | Sonnet | D0-1 | schema 按 §2.1；导出可往返；删除后不可再导出 |
| D0-3 | `HardwareProbe` + `TierPolicy` + 目录 schema；`GET /api/v1/decide/profile`（openapi 手写 + 生成客户端） | Sonnet | D0-1 | macOS / Linux 探测有测试（注入假数据）；无网/拒绝下载时稳定落 T0 |
| D0-4 | 三份中文评测集：记住意图约 100 句、召回门控约 100 句、工具风险约 60 例；含问句陷阱、A-不-A 问句、口语变体、少量英文 | Opus | — | 每条有期望标签与误判代价等级；评测集进仓库 `eval/decide/` |
| D0-5 | 横评脚本：规则 / GLiClass-multilang / mDeBERTa-xnli / Erlangshen-NLI / SetFit(bge-m3) / Qwen3Guard-0.6B / Kev-0.8B | Sonnet | D0-4 | 输出准确率、ECE、按代价加权误判率、P95 延迟、RSS、下载体积 |
| D0-6 | 两台机器实测（笔记本 M1 Max 64GB、Mac mini M4 24GB），产出分档表并回填目录 | Sonnet 跑，Opus 定稿 | D0-5 | 结果追加到调研台账 §8；§1.1 分档表定稿 |
| D0-7 | 候选许可证逐个核实 | Opus | — | 结论进调研台账；不可商用的剔除 |

D0-1 / D0-3 / D0-4 / D0-7 可并行。规则基线在 M1 合入 main 前从 `ab/m1-memory` 的 `retain.rs` 取。

**D0 出口（jason 拍板）**：每个决策点选定各档位的胜出模型，或结论为「这一点暂时只用规则」。

### D1 — 记住意图 + 召回门控上线（M1 合入 main 之后）

- 两个决策点接入决策服务，规则保留为地板，阈值三段；
- 启动时按硬件档位装配，首次使用时提示按需下载；
- 决策日志正式开始记录，撤回 / 澄清回答回流为标签；
- 验收：误写入率 ≤ 规则版；记住召回显著提升；无关问题误注入 = 0；CPU 上 P95 < 100ms；T0 档行为与今天的规则版逐字节一致。

### D2 — Guardian 风险 + 入口路由（ID-1）

验收：危险操作漏判 = 0；`always_review` 硬清单不可被模型覆盖；TaskProfile 字段与 iDoris 对齐。

### D3 — 入站分类、PII、语音句末、纠错 / 偏好识别；后端迁往 iDoris

各自评测集达标。

### D4 — 个人决策模型

按 §2.4；数据量不够就不开工。

## 4. 不做什么

- 不训练通用大模型；不上传任何用户数据；
- 不让决策服务承担 A 类判断；
- D0 不改任何现有调用点的行为（只加 crate、日志、探测和评测）。
