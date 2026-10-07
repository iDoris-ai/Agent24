# 出处清单

> 本目录各文件引用的全部来源。仓库内路径相对仓库根；PR 都在 `iDoris-ai/Agent24`。外部链接的核实状态以原引用文档（主要是 `DECISION-MODELS-EVIDENCE-2026-10-06.md` 与 `DECISION-MODELS.md` §8）为准，本清单不重新核实，例外处单独注明。

## 1. PR

| PR | 标题 | 合并时间（UTC） | 合并提交 | 评审要点 |
|---|---|---|---|---|
| [#680](https://github.com/iDoris-ai/Agent24/pull/680) | fix(m1): M1-T13 —— 放宽「记住」识别（合入 `ab/m1-memory`） | 2026-10-06 13:07 | `c503bf1` | 规则基线所用的 `retain.rs` 版本 |
| [#684](https://github.com/iDoris-ai/Agent24/pull/684) | docs(research): 决策模型全面调研结论 + D0–D3 引入方案 | 2026-10-07 07:20 | `caeff3e` | 抽查三处内部代码引用均属实 |
| [#686](https://github.com/iDoris-ai/Agent24/pull/686) | docs(decide): PLAN-DECIDE 任务拆解 | 2026-10-07 07:28 | `ab8e8d8` | 引用了一份尚不存在的 ADR（非阻塞） |
| [#687](https://github.com/iDoris-ai/Agent24/pull/687) | docs(decide): D0-4 中文评测集 + D0-7 许可证核实 | 2026-10-07 07:44 | `4fe8c26` | 独立移植规则、零偏差复算基线 |
| [#688](https://github.com/iDoris-ai/Agent24/pull/688) | feat(decide): D0-1 agent24-decide crate | 2026-10-07 08:31 | `109cdcc` | M1 calibrated、M2 阈值反序列化、M3 文档；[第 1 轮评审](https://github.com/iDoris-ai/Agent24/pull/688#pullrequestreview-5439484307) |
| [#689](https://github.com/iDoris-ai/Agent24/pull/689) | feat(decide): D0-3 硬件探测 + 分档 + 模型目录 | 2026-10-07 09:17 | `5391c57` | pin 只查非空；LaunchAgent 透传 |
| [#717](https://github.com/iDoris-ai/Agent24/pull/717) | feat(decide): D0-2 决策日志 | 2026-10-07 09:44 | `df2f91e` | CLI 迁移版本错位等 forward-note；jason 确认 180 天 |
| [#718](https://github.com/iDoris-ai/Agent24/pull/718) | feat(decide): D0-5 横评脚本 | 2026-10-07 09:43 | `a912cbb` | 近义模板泄漏；[评审](https://github.com/iDoris-ai/Agent24/pull/718#pullrequestreview-5440415310) |
| [#722](https://github.com/iDoris-ai/Agent24/pull/722) | feat(decide): D0-6 两机实测 + 近似重复检测 | 2026-10-07 11:03 | `df747c7` | 检测器反向验证；16GB 结论 |
| [#726](https://github.com/iDoris-ai/Agent24/pull/726) | feat(decide): 评测集扩成中/英/泰三语 | 截至 2026-10-07 OPEN | — | D0-8，本目录未引用其数字 |

提交：`bb81341`（M1-T13 句末疑问修复）、`889c7f1`（D0-6 两机实测结论）、`e6484a5`（本目录 00/02）。

## 2. 仓库内文件

调研与规划：
- `docs/research/DECISION-MODELS.md`（§1–§9）
- `docs/research/DECISION-MODELS-EVIDENCE-2026-10-06.md`
- `docs/research/DECISION-POINTS-INVENTORY-2026-10-06.md`
- `docs/agent/PLAN-DECIDE.md`
- `docs/decision.md` ADR-033、ADR-034

代码：
- `rust/crates/agent24-decide/src/{lib,types,points,backend,service,threshold,hw,tier,catalog,log}.rs`
- `rust/crates/agent24-decide/decide-models.catalog.json`
- `rust/crates/agent24-store/migrations/0014_decision_log.sql`
- `rust/crates/agent24-store/src/decision_log.rs`
- `rust/crates/agent24-agent/src/retain.rs`（`ab/m1-memory` @ `c503bf1`）

评测：
- `eval/decide/README.md`；`eval/decide/{retain_intent,recall_gate,tool_risk}.jsonl`
- `eval/decide/bench/README.md`；`eval/decide/bench/train_data/`（README + 3 份训练集）
- `eval/decide/bench/src/decide_bench/{metrics,overlap}.py`、`adapters/setfit_bgem3.py`
- `eval/decide/bench/tests/{test_rule_port,test_overlap,test_metrics}.py`
- `eval/decide/bench/results/m1max-64g/2026-10-07.{md,json}`、`2026-10-07-dedup.{md,json}`
- `eval/decide/bench/results/m4-16g/2026-10-07.{md,json}`

## 3. jason 博客（blog.mushroom.cv）

本地源文件：`/Users/jason/Dev/mycelium/blog/src/content/blog/<文件名>.md`；线上地址 `https://blog.mushroom.cv/blog/<文件名>`。`02-ecosystem-survey.md` 依据前 14 篇。

| 发布日期 | 标题 | 链接 |
|---|---|---|
| 2026-09-18 | Jev：TypeSafe AI 的 RLCD 决策模型，输出 Token 免费、最高快 200 倍 | https://blog.mushroom.cv/blog/typesafe-ai-jev-system-one-model-rlcd-decision-ai-enterprise |
| 2026-09-20 | Kev：Jared Palmer 开源本地决策模型，一次前向传播回答多个问题 | https://blog.mushroom.cv/blog/kev-jaredpalmer-local-decision-model-jev-open-source-qwen-lora |
| 2026-09-24 | AgentJev-0.6B：Qwen3 底座的 System-1 决策核 | https://blog.mushroom.cv/blog/agentjev-0-6b-system-one-decision-model-qwen3 |
| 2026-09-24 | JevEmbed：用 Embedding 做决策——Choice / Score / Noul 三合一框架 | https://blog.mushroom.cv/blog/jevembed-embedding-decision-framework-choice-score-noul |
| 2026-09-22 | LLM2Jev：用任意本地 LLM 克隆 Jev 的 /v1/systemone | https://blog.mushroom.cv/blog/llm2jev-local-jev-api-prefill-only-binary-inference |
| 2026-09-22 | KaLM-Jev 本地部署指南 | https://blog.mushroom.cv/blog/kalm-jev-local-judgment-engine-hardware-deploy |
| 2026-09-22 | SemIf 拆解：把 Agent 的每次判断降维为「读 logit」 | https://blog.mushroom.cv/blog/semif-semantic-if-local-jev-decision-engine |
| 2026-10-06 | Laya ANE：把 421M 决策模型烧进 Apple 神经引擎 | https://blog.mushroom.cv/blog/laya-ane-coreml-apple-neural-engine-typed-decisions |
| 2026-10-02 | Cloudflare 开源 Clef 决策模型：兼容 Jev API、9B 版 Mac 可跑，榜首成绩是自报的 | https://blog.mushroom.cv/blog/cloudflare-clef-flash-open-decision-model-jev-api-rl-finetune |
| 2026-09-20 | Bespoke Nimble：一天之内做一个会读概率的 9B 决策模型 | https://blog.mushroom.cv/blog/bespoke-nimble-9b-open-decision-model-logprob-jev-rival |
| 2026-10-03 | StartLux Decision 拆解 | https://blog.mushroom.cv/blog/startlux-decision-typed-decision-model-jev-compatible-games-demo |
| 2026-09-29 | Ollaya：本地运行决策模型的 Ollama | https://blog.mushroom.cv/blog/ollaya-local-decision-model-runtime-laya-jev-teardown |
| 2026-09-25 | mu（μ）：编程 Agent 的判断核，35 个决策点交给小模型 | https://blog.mushroom.cv/blog/mu-coding-agent-judgment-kernel-35-decision-points-hive |
| 2026-09-22 | TypeSafe AI 的 Jev 生态刷屏：所谓「一天冒出 800+」…核实 | https://blog.mushroom.cv/blog/jev-awesome-list-gold-rush-fact-check |
| 2026-09-20 | jev-skill：给 Agent 装上 Jev 决策感知（`DECISION-MODELS.md` §7 的输入之一） | https://blog.mushroom.cv/blog/jev-skill-agent-decision-9-skills-90-scenarios-openrouter |

## 4. 外部：产品、项目与模型卡

出自 `DECISION-MODELS-EVIDENCE-2026-10-06.md`：
- TypeSafe Jev：https://www.datacamp.com/blog/system-one-models-jev 、https://systemonemodels.org/ 、https://aimlapi.com/blog/what-is-jev （ECE 数字为二级来源）、typesafe.ai 首页
- jev-skill：https://github.com/wuyoscar/jev-skill
- Kev：https://github.com/jaredpalmer/kev
- Ollaya：https://github.com/ollaya-dev/ollaya
- Qwen3Guard：https://github.com/QwenLM/Qwen3Guard
- 约束解码：https://www.mindstudio.ai/blog/parallel-constrained-decoding-mlx

出自 `DECISION-MODELS.md` §8.1（许可证原文，2026-10-07 核实）：
- https://huggingface.co/knowledgator/gliclass-multilang-mini/raw/main/README.md （ultra / edge 同路径）
- https://huggingface.co/knowledgator/gliclass-x-base/raw/main/README.md
- https://huggingface.co/MoritzLaurer/mDeBERTa-v3-base-mnli-xnli/raw/main/README.md
- https://huggingface.co/MoritzLaurer/mDeBERTa-v3-base-xnli-multilingual-nli-2mil7/raw/main/README.md
- https://github.com/facebookresearch/XNLI/blob/main/LICENSE （CC-BY-NC-4.0）
- https://huggingface.co/microsoft/mdeberta-v3-base/raw/main/README.md
- https://huggingface.co/IDEA-CCNL/Erlangshen-Roberta-110M-NLI/raw/main/README.md
- https://huggingface.co/BAAI/bge-m3/raw/main/README.md
- https://raw.githubusercontent.com/huggingface/setfit/main/LICENSE
- https://huggingface.co/Qwen/Qwen3Guard-Gen-0.6B/raw/main/LICENSE
- https://huggingface.co/jaredpalmer/kev-0.8b/raw/main/README.md 、https://huggingface.co/jaredpalmer/kev-4b/raw/main/README.md
- https://github.com/jaredpalmer/kev/blob/main/LICENSE
- https://github.com/ollaya-dev/ollaya/blob/main/LICENSE
- https://huggingface.co/convaiinnovations/laya/raw/main/README.md
- https://huggingface.co/Mapika/decider-0.8b/raw/main/README.md

本目录新增（作者 2026-10-07 抓取核对）：
- SetFit 官方博客（Hugging Face + Intel Labs + UKP Lab 合作）：https://huggingface.co/blog/setfit

## 5. 外部：论文

| 论文 | 链接 | 在本目录中的用途 |
|---|---|---|
| Tunstall et al., *Efficient Few-Shot Learning Without Prompts*（SetFit） | https://arxiv.org/abs/2209.11055 | 06 §1 |
| ICLR 2024 LLM 置信度系统比较（verbalized confidence 普遍过度自信） | https://arxiv.org/pdf/2306.13063 | 01 §2 |
| *Rethinking Verbalized Confidence for LLM-as-a-Judge*（post-2025 模型 verbalized 反超 logprob） | https://arxiv.org/pdf/2609.10996 | 01 §2 |
| ConfidenceBench | https://arxiv.org/html/2607.20526 | EVIDENCE §6（弱已核实） |
| *Auditing Cross-Domain Recalibration of LLM Judges* | https://arxiv.org/pdf/2609.27954 | 分布偏移风险 |
| Qwen3Guard 技术报告（Apache-2.0、119 语言） | https://arxiv.org/pdf/2510.14276 | 许可证与多语言证据 |
| GLiClass | https://arxiv.org/html/2508.07662v1 | 候选背景 |
| RouteLLM | https://arxiv.org/html/2406.18665v3 | 不采用（停更） |

## 6. 外部：工程实践参考（EVIDENCE §8）

- 三层级联内容审核案例：https://www.techinterview.org/post/3233474439/
- Youden's J 与代价敏感阈值：https://casrai.org/guides/youdens-j-index-roc-threshold-selection
- 主动学习：https://docs.roboflow.com/deployment/monitoring-and-analytics/active-learning 、https://docs.humansignal.com/guide/active_learning.html
- 人在回路的「毕业」机制：https://galileo.ai/blog/human-in-the-loop-agent-oversight 、https://www.anthropic.com/research/measuring-agent-autonomy
