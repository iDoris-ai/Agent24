# Jev 类决策模型：评测、训练与实际使用（研究资料目录）

> 用途：供 jason 写公开文章。本目录只**汇编**仓库与本机已有材料，不新增实验；每个数字都标了出处（仓库相对路径或 PR 号），推断明确写「推断」。
> 截至 2026-10-07：D0（接口、日志、硬件分档、评测集、横评、两机实测）已全部合入 main；D0-8（三语评测集 PR #726、同系列模型补测）进行中；D1（接线上线）未开始。

## 目录

| 文件 | 内容 |
|---|---|
| [`00-timeline-and-decisions.md`](00-timeline-and-decisions.md) | 时间线、jason 拍板的决定、过程教训（主会话撰写） |
| [`01-why-decision-models.md`](01-why-decision-models.md) | 问题定义：M1 规则误判原句、生成式 LLM 当判断器的问题、19 个判断点三分类、A 类为何必须是确定性代码 |
| [`02-ecosystem-survey.md`](02-ecosystem-survey.md) | 博客 14 篇的生态调研：候选一览、16GB/8GB 路线、要警惕的点（主会话撰写） |
| [`03-architecture.md`](03-architecture.md) | `agent24-decide` 设计：三种题型、级联、三段阈值、不静默降级、`llm_simulation`、A 类不变量、ADR-033/034 |
| [`04-evaluation-design.md`](04-evaluation-design.md) | 自建中文评测集：标签、`cost_level`、问句陷阱、规则基线复现、指标、泄漏检测的故事 |
| [`05-results.md`](05-results.md) | 两台机器的横评结果表、去重前后对比、16GB 上 SetFit 不可用、结论与局限 |
| [`06-training-and-deployment.md`](06-training-and-deployment.md) | SetFit、训练/推理分离、按需下载与 pin、T0–T3 与五档新要求、Rust 集成路线、许可证 |
| [`07-data-flywheel-and-personal-model.md`](07-data-flywheel-and-personal-model.md) | 决策日志 schema、标签来源、隐私硬约束、个人模型晋升门（D4） |
| [`08-code-review-findings.md`](08-code-review-findings.md) | PR 评审抓到的真实缺陷：问题、危害、修法、锁住的测试 |
| [`sources.md`](sources.md) | 全部 PR、提交、文件、博客、外部链接与论文 |

## 一页结论

1. **规则有天花板**：修完 M1-T13 后，规则在 101 句中文记住意图评测集上仍漏识别 15/33、误识别 8/68（`eval/decide/README.md`，#687 评审独立复算零偏差）。中文口语、方言、句式位置穷举不完。
2. **生成式 LLM 不适合当判断器**：本地 Qwen3-8B 在 system 已写「记忆已暂停」时仍回答「我记住了」；口头置信度普遍过度自信（`DECISION-MODELS.md` §7.1，arXiv 2306.13063）。
3. **只把「理解语言」的判断交给模型**：19 个判断点中，WriteGate、来源标记、owner 过滤、Authorizer 等 A 类永远是确定性代码；模型输出只能让动作更严，不能放宽（`DECISION-MODELS.md` §7.1、ADR-033 第 4 条）。
4. **Jev 只借接口形态**：Jev 闭源，我们只用它的 choice / noul / score + 概率的接口词汇（`types.rs`）。
5. **中文没有现成可信的基准**，必须自建：14 篇生态拆解里几乎没有中文分类准确率（`02-ecosystem-survey.md`）。
6. **少样本 SetFit(bge-m3) 在三个决策点都领先**：去重后准确率 0.762 / 0.850 / 0.469，零样本候选在中文意图上普遍不可用（`results/m1max-64g/2026-10-07{,-dedup}.md`）。
7. **但还不够上线**：SetFit 误写入率 0.162 仍高于规则的 0.118，tool_risk 最高只有 0.469——必须「规则地板 + 模型 + 阈值弃权带 + `always_review` 硬清单」组合使用（`05-results.md` §8）。
8. **训练集泄漏要查近似重复**：逐字不重叠不够，近义模板（「北京 / 上海明天天气」）占评测集 2–6%；补上 0.7 阈值检测、去重后 SetFit 领先仍成立（#718 评审、#722）。
9. **硬件假设要实测，训练与推理要分开**：「24GB 的 Mac mini」实测 16GB，SetFit 加载时训练在其上 30 分钟跑不完、swap 几乎用满（`DECISION-MODELS.md` §9.2）。
10. **契约代码最需要评审**：零调用点的 D0 代码里抓到 4 个阻塞缺陷——校准标记可伪造、阈值反序列化后方向放宽、pin 只查非空、文档描述死代码路径（`08-code-review-findings.md`）。

## 文章提纲建议

### 角度 A：为什么 agent 需要一个 System-1 决策层

1. 开场：一句「你记住我吗」被当成记住指令；另一句「你记住，我对花生过敏」没被认出（01 §1）。
2. 三条路都试过：规则（召回 55%）、提示词让 LLM 判断（无视指令、过度自信）、专用决策模型（输出可阈值化的概率）（01 §1–2）。
3. 什么叫 System-1：不生成文字，只回答 choice / noul / score；Jev 的接口与生态（02、03 §2）。
4. 不是所有判断都该交给概率：19 个判断点三分类，A 类为何必须是确定性代码（01 §3–4）。
5. 怎么接：规则地板 → 快速模型 → 深度模型；三段阈值；不可用就说不可用，不静默降级（03 §1、§3、§4）。
6. 让它可信的细节：`llm_simulation` 不得冒充校准概率，以及评审是怎么发现它能被伪造的（08 §1）。
7. 收尾：决策日志与个人模型——用户的每次纠正都是标签（07）。

### 角度 B：中文 / 多语言决策模型的空白与自建评测

1. 生态全景：两周内冒出十几个 Jev 平替，几乎没有中文分类数据；自报成绩在复测中缩水；许可证不明是常态（02）。
2. 自建评测集怎么设计：六类记忆意图、`cost_level` 按判错后果、问句陷阱 / A-不-A / 方言 / 来源敏感的工具风险对照（04 §2）。
3. 规则基线如何做到可复现：逐行移植 + 原测试表自验 + 评审独立复算（04 §3）。
4. 横评结果：七个候选、三个决策点、两台机器（05）。
5. 泄漏的故事：逐字检查 → 评审发现近义模板 → 0.7 阈值双指标检测 → 反向验证检测器 → 去重后重跑（04 §5）。
6. 坦白局限：样本小、单次运行、泰文还在补、ECE 偏高（05 §9）。
7. 呼吁：把中文（及泰文等）决策评测集当公共物品开放（**这是写作建议，非已有决定**）。

### 角度 C：从 64GB 到 8GB——本地决策模型的分档

1. 同一个模型在两台 Mac 上的命运：64GB 上 292s 跑完，16GB 上 30 分钟 swap 到底（05 §6）。
2. 硬件探测是确定性代码，不是模型；T0–T3 的判定规则、用户只能降档、不同意下载就落 T0（06 §4.1）。
3. 训练与推理分离：用户机器只推理（06 §2）。
4. 按需下载与供应链：40 位 commit pin + sha256，以及「`revision: main` 也算 pin」这个评审抓到的坑（06 §3、08 §4）。
5. 分档建议与 jason 的五档要求（8/16/24/32/64GB+、同系列、中英泰）（06 §4.2–4.3）。
6. 进程内运行时怎么选：`ort` vs `candle` / `tract` 的取舍（06 §5）。
7. 未完成：24GB 档、同系列补测、16GB 只推理的实测（见下「待补项」）。

## 待补项（D0-8 等结果出来后要补的位置）

| 待补内容 | 来源（预期） | 要补的位置 |
|---|---|---|
| 中英泰三语评测集的规模、标签分布、泰文样例 | PR #726（`ab/decide-01-trilingual-eval`） | `04-evaluation-design.md` §2、§7；本 README 结论 5 |
| 三语评测集上的横评数字（尤其泰文） | D0-8 横评结果目录 | `05-results.md` 新增一节；§9 局限中「泰文暂缺」一条 |
| 同系列、尺寸连贯的模型补测结果（8/16/24/32/64GB+ 各一档） | `ab/decide-02`（截至本文未见 PR） | `06-training-and-deployment.md` §4.3；`05-results.md`；角度 C 提纲第 5 点 |
| 真 24GB 机器上 SetFit 能否稳定运行 | `DECISION-MODELS.md` §9.2 要求的复测 | `05-results.md` §6；`06-training-and-deployment.md` §4.2 |
| 训练产物在 16GB 机器上**只推理**的内存与 P95 | 尚未立项 | `06-training-and-deployment.md` §2 |
| kev 服务进程本身的常驻内存 | `DECISION-MODELS.md` §9.4 | `05-results.md` §3 |
| jason 对 §9.5 分档建议的拍板 | jason | `06-training-and-deployment.md` §4.2；`00-timeline-and-decisions.md` |
| `ort` vs `candle`/`tract` 的选择与 Rust 内实测 | D1 | `06-training-and-deployment.md` §5 |
| Erlangshen 训练数据许可（CMNLI 是否由 MNLI/XNLI 翻译） | `DECISION-MODELS.md` §8.2 | `06-training-and-deployment.md` §6 |
| D1 上线后的真实误写入率 / 召回 / 日志量 | D1 | `05-results.md`、`07-data-flywheel-and-personal-model.md` |
