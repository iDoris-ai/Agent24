# 决策模型（Decision Models）调研台账

> 立于 2026-10-06（jason 拍板）。本文是**持续调研台账**：记录「决策模型」方向的候选、评估结论与跟进事项；每次调研或 Spike 后追加一节，不删历史。
> 执行状态见 [`../agent/tasks.md`](../agent/tasks.md)「DM-SPIKE」。
> 任务拆解（含硬件自动适配、数据积累与个人模型）见 [`../agent/PLAN-DECIDE.md`](../agent/PLAN-DECIDE.md)。

## 1. 是什么，为什么关心

决策模型（如 Jev/TypeSafe、Laya、GLiClass、NLI 类）**不生成文字，只输出带置信度的分类/打分**：给一句话 + 结构化问题，直接返回「标签 + 概率」。本地 CPU 推理为毫秒级（据报道 Laya 中文约 8ms，比云端快约 30 倍），可离线、零调用成本。

Agent24 的直接动机：M1 真机验收中，规则式 `explicit_remember`（`agent24-agent/src/retain.rs`）先后出现
- 漏识别：「你记住，我对花生过敏。」未被识别（只认开头「记住/请记住」）；
- 误识别：「你记住我吗」「你记住他的名字了吗」被当成记住指令（M1-T13 / #680 修复中）。

中文表达穷举不完，意图判断本质是分类问题，适合决策模型。

## 2. 可用的场景（按优先级）

1. **记忆意图分类**：记住 / 忘掉 / 询问记忆 / 更正 / 偏好设置（「以后用中文回复我」）/ 无关。覆盖 M1 retain，并为 P1（更正、偏好、清除）与 P2（候选抽取，原计划用本地 LLM）提供更便宜的实现。
2. **召回门控**：「这句话需要用到记忆吗」，替代 / 补强 M1-T07.2 的停用词表，从根上降低误注入。
3. **Agent 前置判断**：在调用昂贵 LLM 前做路由、风险与紧急度判断（与 C2 ID-1 任务画像相关）。

## 3. 设计原则（已确定）

- **规则兜底 + 模型判断的混合**，不直接替换规则。抽象为 `IntentClassifier` trait：规则实现 + 决策模型实现。
- **写记忆有副作用，偏精确**：规则明确命中 → 直接执行；规则未命中 → 问决策模型，**高置信才写入并发「已记住」回执**；低置信不写或反问用户。
- **本地优先、可选组件**：模型与运行时作为按需下载组件（同 iDoris Design 组件机制），不进安装包。
- **长期归属 iDoris 网关**（模型能力）；Spike 阶段先在 Agent24 内经 trait 接入，迁移时只换实现。

## 4. 候选与已知信息

| 候选 | 来源 | 说明 / 待核实 |
|---|---|---|
| Ollaya（运行时） | github.com/ollaya-dev/ollaya，Apache-2.0 | 「决策模型界的 Ollama」：`ollaya serve/pull/run`，兼容 TypeSafe `/v1/systemone` API；2026-09 底开源，约 409 星，**非官方、早期** |
| laya | Convai Innovations | 多语言，报道中文约 8ms；许可证待核 |
| gliclass | — | 零样本分类，适合意图类目可变的场景；许可证待核 |
| decider / kev / decision / jevk5 | Mapika 等 | 通用决策 / TypeSafe 兼容；许可证待核 |
| qwen3guard | 基于 Qwen3 | guard 类（安全判断）候选 |
| nli 系列 | 多家 | 自然语言推理，可做「这句是否表达了要记住的意图」蕴含判断 |
| 直接在 Rust 内加载 ONNX（`ort` crate） | — | 免额外守护进程的替代方案，Spike 后视结果再定 |

**风险**：项目早期、API 可能变；各模型许可证不同，商用前逐个核实；「约 3MB」只是 ONNX 图，权重另从 HuggingFace 拉取（pin commit + sha256），实际体积需实测；中文意图分类效果未在我们的数据上验证。

来源：blog.mushroom.cv「Ollaya：本地运行决策模型的 Ollama」（2026-09-29）。

## 5. DM-SPIKE（M1 合进 main 后第一个任务）

- **设计（Opus）**：`IntentClassifier` trait 与类目定义；约 100 句中英文意图评测集（含「你记住我吗」等问句陷阱、口语变体、英文），每类给期望标签。
- **执行（Sonnet）**：本地用 Ollaya 跑 laya / gliclass / decider 等，与规则版在同一评测集上对比。
- **指标与门槛**：**误写入率**（非记住句被判为记住）≤ 规则版；记住类召回明显高于规则版；延迟与组件体积可接受；许可证允许。全部满足才接入（按需下载组件），否则只保留在评测台做对照。
- **产出**：本台账追加「Spike 结果」一节（数字 + 结论 + 是否接入）。

## 6. 持续跟进（每次调研追加）

- [ ] 跟踪 Ollaya 版本与 `/v1/systemone` 协议稳定性
- [x] 逐个核实候选模型许可证（见 §8，2026-10-07）
- [ ] 评估召回门控场景（§2-2）
- [ ] 与 iDoris 网关规划对齐（模型能力归属）

---

## 7. 2026-10-06 全面调研结论（Opus 综合）

> 证据：[`DECISION-MODELS-EVIDENCE-2026-10-06.md`](DECISION-MODELS-EVIDENCE-2026-10-06.md)（外部调研，逐条标注已核实/未核实）、[`DECISION-POINTS-INVENTORY-2026-10-06.md`](DECISION-POINTS-INVENTORY-2026-10-06.md)（Agent24 内 19 个判断点盘点）。输入还包括 blog.mushroom.cv 的 Ollaya 与 jev-skill 两篇拆解。

### 7.1 结论：有帮助，但只对「理解语言的判断」有帮助

**对 Agent24 有明确正收益**，理由有三：
1. **比 prompt 判断更可靠**：用生成式 LLM 做分类，结果随措辞漂移，且自报置信度普遍过度自信（ICLR 2024 2306.13063）。本轮真机验收也看到了：本地 Qwen3-8B 无视 system 里的「记忆已暂停」提示，回了「我记住了」。专门训练的决策模型输出的是概率，可设阈值、可评测、可回归。
2. **比规则更能泛化**：中文口语说法穷举不完，#680 一修再修（「你记住我吗」「好不好/行不行」），这正是规则的天花板。
3. **符合本地优先**：专用 encoder 分类器在本地是毫秒到亚秒级（Ollaya 实测：encoder 类在 CPU 上 0.15–2.4s，GPU 上 7–35ms；decoder 类「小 LLM 当分类器」在 CPU 上 1.3–25s），不出网、无调用成本。

**但不是所有判断都该交给概率模型。** 19 个判断点分三类：

| 类别 | 判断点（编号见盘点） | 处理 |
|---|---|---|
| **A. 安全/一致性不变量：必须保持确定性代码** | #2 WriteGate 信任策略、#5/#15 来源标记、#6 召回的 owner/active 过滤、#8 会话导入、#10 Authorizer、#16 capability 令牌、#11 会话视图 | **不引入模型**。概率模型可以建议，但不能越过这些门 |
| **B. 语言理解类判断：适合决策模型** | #1/#9 记住意图、#19 召回门控（这句话需要用记忆吗）、#4 Guardian 工具风险、#13 入口路由 / TaskProfile、#17 入站消息分类、#18 PII 识别、#14 语音句子边界与意图 | **规则兜底 + 决策模型判断 + 弃权带** |
| **C. 暂不需要** | #7 摘要触发（计数即可）、#12 审批类型 | 维持现状 |

另外还有几个**尚未存在、但引入决策模型后自然出现**的场景：用户反馈与纠错识别（「不对，我说的是…」→ 更正记忆）、偏好指令识别（「以后用中文回复」→ P1 程序性偏好）、「是否要反问澄清」、Agent 循环卡住检测（jev-skill 示例：`stuck=true, P=0.88`）、上下文是否需要压缩。

### 7.2 关键判断（与直觉不同的地方）

- **Jev 本身不能用**：闭源托管 API，不开放权重，与本地优先冲突。只把它的**接口形态**（choice / noul / score + 校准概率）作为内部标准。
- **「小 LLM + 约束解码 + logprob」不是默认路线**：比专用 encoder 慢一个数量级，而且 2025 年后的研究显示 logprob 校准不再稳定占优。只在没有现成专用分类器时兜底使用。
- **中文没有现成可信的基准**，这是全行业空白。凡是进入 Agent24 的决策模型，都必须先过**我们自建的中文评测集**，不能信任模型卡上的英文数字。
- **Ollaya 适合做实验台，不适合直接上生产**：不到 3 周历史；多数子模型许可证未单独注明。中文证据最硬的是 Qwen3Guard（官方 119 语言，Apache-2.0）、GLiClass-multilang（20 语言含中文）、mDeBERTa-xnli / Erlangshen（中文 NLI）。
- **Kev（Apache-2.0，Qwen3.5 底座，0.8B–27B）是最接近 Jev 的开源平替**，生态最活跃（8.5k★）。但它是 decoder 类，CPU 上慢，适合放在 oMLX/Metal 上做「多问题、有上下文」的复杂决策，不适合每条消息都跑。

### 7.3 建议架构：决策服务（Decision Service）

```
调用点（retain / recall / guardian / router / inbound…）
   │  DecisionRequest{ state, questions:[choice|noul|score], side_effect_class }
   ▼
agent24-decide（新 crate，接口照 TypeSafe /v1/systemone 形状）
   1. 规则层（高精度地板）：命中即定，记录 backend=rule
   2. 快速模型层（进程内，candle/ort 跑小 encoder）：
        GLiClass-multilang / 中文 NLI / SetFit(bge-m3) / Qwen3Guard-0.6B
   3. 深度模型层（可选，经 oMLX/Ollaya，Metal）：Kev-0.8B/4B 类，处理多问题、长上下文
   4. 阈值三段：执行 / 弃权（不做或反问）/ 交人（审批）；阈值按误判代价逐个决策点设
   5. 不允许静默降级（借鉴 jev-skill）：后端不可用就返回 unavailable，调用方走规则或交人；
      用 LLM 模拟的结果必须标 backend=llm_simulation，不得冒充校准概率
   6. 全量决策日志：输入摘要、各层输出与概率、最终动作；用户撤回 / 审批拒绝 / 「不对」作为标签回流
```

- **不变量优先**：A 类判断永远在决策服务之外，用确定性代码执行；决策服务的输出只能触发「建议」或「更严格」的动作，不能放宽权限。
- **副作用要可见、可撤销**：写记忆、执行工具这类决策，配合系统回执（M1-T14 的 `memory_receipt` 就是这个模式）和撤回入口。用户的纠正自动成为训练和评测标签。
- **组件化**：模型与运行时都作为按需下载组件（同 iDoris Design 组件机制），不进安装包。
- **归属**：接口在 Agent24（`agent24-decide`）；长期后端迁到 iDoris 网关（模型能力统一调度），迁移时只换实现。

### 7.4 分阶段落地

| 阶段 | 内容 | 验收门槛 |
|---|---|---|
| **D0 = DM-SPIKE**（M1 合进 main 后，约 1 周） | `agent24-decide` 接口与决策日志；评测台；三份中文评测集（记住意图约 100 句、召回门控约 100 句、工具风险约 60 例，都要含问句陷阱和口语变体）；在本机 Apple Silicon 上横评：规则 / GLiClass-multilang / mDeBERTa-xnli / Erlangshen-NLI / SetFit(bge-m3) / Qwen3Guard-0.6B / Kev-0.8B（经 Ollaya） | 每个候选给出准确率、ECE、按代价加权的误判率、P95 延迟、内存、下载体积、许可证结论 |
| **D1**（第一批上线） | 记住意图 + 召回门控换上 D0 的胜出者（进程内 encoder），规则保留为地板；阈值三段 | 误写入率 ≤ 规则版；记住召回率显著提升；无关问题误注入 = 0；P95 < 100ms（CPU） |
| **D2** | Guardian 工具风险改为 Qwen3Guard 或决策模型 + `always_review` 硬清单；入口路由 / TaskProfile（ID-1）用决策服务生成 | 危险操作漏判 = 0（评测集）；与 iDoris 字段对齐 |
| **D3** | 入站消息分类（Hyphae）、PII 识别（GLiNER 类 NER，不是分类器）、语音句末判断、纠错 / 偏好识别（支撑 P1）；后端迁往 iDoris | 各自评测集达标 |

### 7.5 风险与对策

- **中文效果未知** → D0 先评测后接入，未达标就不上线。
- **生态早期、API 不稳**（Ollaya、Kev 都很新）→ 只依赖我们自己的接口；Ollaya 仅作实验台，生产走进程内运行时。
- **许可证分散** → 每个模型接入前逐一核对，结论记进本台账。
- **概率模型被提示注入或对抗改写** → 它只能「建议更严」，不能放宽 A 类不变量；确认 UI 的文案走可信渲染。
- **漂移** → 决策日志 + 用户纠正回流 + 评测集持续回归（并入 CI）。

---

## 8. D0-7 许可证核实（2026-10-07）

> 方法：逐个抓取 HuggingFace 模型卡 raw README 的 YAML `license:` 字段、HF API `cardData.license`、仓库 LICENSE 原文（HF 或 GitHub）；底座模型按 config / 卡片追到上一层再核一次。不采信搜索摘要和第三方文章。核实日期均为 2026-10-07。
> 「原文已核实」= 读到了 YAML license 字段或 LICENSE 文件；只读到二手描述的标「未核实」。

### 8.1 结论表

| 候选 | 确切 repo | 许可证 | 可商用 | 出处 | 原文已核实 | 结论 |
|---|---|---|---|---|---|---|
| GLiClass-multilang | `knowledgator/gliclass-multilang-mini` / `-ultra` / `-edge` | Apache-2.0 | 是 | https://huggingface.co/knowledgator/gliclass-multilang-mini/raw/main/README.md（ultra/edge 同路径） | 是（YAML） | **保留**。底座：mini=`microsoft/mdeberta-v3-base`（MIT）、ultra=`google/mt5-xl`（Apache-2.0）、edge=`jhu-clsp/mmBERT-small`（MIT） |
| GLiClass（旧多语言） | `knowledgator/gliclass-x-base` | Apache-2.0 | 是 | https://huggingface.co/knowledgator/gliclass-x-base/raw/main/README.md | 是（YAML） | **保留**。卡片原文：trained on synthetic and licensed data that allow commercial use |
| mDeBERTa-xnli | `MoritzLaurer/mDeBERTa-v3-base-mnli-xnli` | MIT（权重） | **存疑** | https://huggingface.co/MoritzLaurer/mDeBERTa-v3-base-mnli-xnli/raw/main/README.md | 是（YAML） | **剔除出生产候选**，D0-5 可保留作对照。训练用了 XNLI，XNLI LICENSE 原文为 CC-BY-NC-4.0（https://github.com/facebookresearch/XNLI/blob/main/LICENSE） |
| mDeBERTa-xnli 多语言变体 | `MoritzLaurer/mDeBERTa-v3-base-xnli-multilingual-nli-2mil7` | MIT（权重） | **存疑** | https://huggingface.co/MoritzLaurer/mDeBERTa-v3-base-xnli-multilingual-nli-2mil7/raw/main/README.md | 是（YAML） | **剔除出生产候选**，同上。训练数据另含 `facebook/anli`（YAML：cc-by-nc-4.0） |
| mDeBERTa 底座 | `microsoft/mdeberta-v3-base` | MIT | 是 | https://huggingface.co/microsoft/mdeberta-v3-base/raw/main/README.md | 是（YAML） | 底座本身无问题 |
| Erlangshen NLI | `IDEA-CCNL/Erlangshen-Roberta-110M-NLI` / `-330M-NLI` / `Erlangshen-MegatronBert-1.3B-NLI` | Apache-2.0 | 是（权重） | https://huggingface.co/IDEA-CCNL/Erlangshen-Roberta-110M-NLI/raw/main/README.md（其余同路径）；Fengshenbang-LM LICENSE 亦为 Apache-2.0 | 是（YAML） | **保留，待补数据许可**。底座 `hfl/chinese-roberta-wwm-ext(-large)` Apache-2.0；微调数据（CMNLI/OCNLI 等 4 个中文 NLI 集）许可证未核实，见 8.2 |
| bge-m3（SetFit 底座） | `BAAI/bge-m3` | MIT | 是 | https://huggingface.co/BAAI/bge-m3/raw/main/README.md | 是（仅 YAML） | **保留**。仓库无 LICENSE 文件，以 YAML 为准 |
| SetFit 库 | `huggingface/setfit` | Apache-2.0 | 是 | https://raw.githubusercontent.com/huggingface/setfit/main/LICENSE | 是（LICENSE） | **保留** |
| Qwen3Guard-0.6B | `Qwen/Qwen3Guard-Gen-0.6B`、`Qwen/Qwen3Guard-Stream-0.6B` | Apache-2.0 | 是 | https://huggingface.co/Qwen/Qwen3Guard-Gen-0.6B/raw/main/LICENSE（Stream 同路径） | 是（YAML + LICENSE） | **保留**。底座 `Qwen/Qwen3-0.6B` 为 Apache-2.0，不是 Qwen 自定义许可 |
| Kev-0.8B | `jaredpalmer/kev-0.8b` | Apache-2.0 | 是 | https://huggingface.co/jaredpalmer/kev-0.8b/raw/main/README.md | 是（YAML） | **保留**。底座 `Qwen/Qwen3.5-0.8B-Base` Apache-2.0；Kev 1.0 发布于 2026-09-24 |
| Kev-4B | `jaredpalmer/kev-4b` | Apache-2.0 | 是 | https://huggingface.co/jaredpalmer/kev-4b/raw/main/README.md | 是（YAML） | **保留，待补数据许可**。卡片称各数据源许可证记在 suite manifest，未读；卡片明确未使用 Jev 输出 |
| Kev 代码 | `github.com/jaredpalmer/kev` | Apache-2.0 | 是 | https://github.com/jaredpalmer/kev/blob/main/LICENSE | 是（LICENSE） | **保留** |
| Ollaya 运行时 | `github.com/ollaya-dev/ollaya` | Apache-2.0 | 是 | https://github.com/ollaya-dev/ollaya/blob/main/LICENSE（Cargo.toml 同为 Apache-2.0） | 是（LICENSE） | **保留（仅实验台）**。README 原文：Each model keeps its own license；随附 llama.cpp 为 MIT |
| laya | `convaiinnovations/laya`（Ollaya 注册表 pin 在 commit `aa8c91ca`，含 en / multilingual / typed-decisions） | Apache-2.0 | 是 | https://huggingface.co/convaiinnovations/laya/raw/main/README.md | 是（YAML） | **保留，待补**。en 底座 ModernBERT-large（Apache-2.0）、multilingual 底座 mmBERT-base（MIT）；typed-decisions 用 teacher 分布蒸馏，teacher 未披露 |
| decider | `Mapika/decider-0.8b` / `-2b` / `-2b-vision` / `-4b` | Apache-2.0 | 是 | https://huggingface.co/Mapika/decider-0.8b/raw/main/README.md（其余同路径） | 是（YAML） | **保留，待补数据许可**。底座 Qwen3.5-*-Base（Apache-2.0）；2b v11 部分数据由 Qwen3.6-27B 生成 |

**小结**：所有候选的**权重许可**都是 Apache-2.0 或 MIT，没有 NC 或自定义许可。唯一实质性商用风险在**训练数据**：两个 mDeBERTa-xnli 模型用了 CC-BY-NC-4.0 的 XNLI（2mil7 还有 ANLI）。用 NC 数据训练出的权重能否商用尚无定论；Agent24 有商业实体分发（HyperCapital），按保守原则剔出生产候选，只在 D0-5 留作对照。中文 NLI 路线由 Erlangshen 承担，但它的训练数据也要补核（见下）。

### 8.2 未核实项

1. **Erlangshen NLI 的训练数据许可**（CMNLI、OCNLI 等 4 个中文 NLI 集）未核原文。注意：CMNLI 一般被描述为由 MNLI/XNLI 翻译而来——**本条未核实**；若属实，Erlangshen 与 mDeBERTa-xnli 是同一类数据风险，需在 D1 选型前核清。
2. GLiClass-multilang 的训练集 `BioMike/formal-logic-reasoning-gliclass-2k`、`knowledgator/gliclass-v3-logic-dataset` 许可证未核。
3. `MoritzLaurer/multilingual-NLI-26lang-2mil7` 的 YAML 没有 license 字段（已剔除，不影响结论）。
4. Kev-4B 各数据源许可证（在 suite manifest 中）未读。
5. laya typed-decisions 的 teacher 模型未披露，无法判断是否有闭源服务条款的连带限制。
6. Ollaya 注册表里的 `jevk5`、`arbiter` 不在本次范围；README 写明 arbiter 底座为 Gemma 3 4B IT（Gemma Terms of Use，非 Apache-2.0），如要用需单独评估。

### 8.3 对 D0-5 横评的影响

- 横评名单不变；mDeBERTa-xnli 结果只作对照，不参与「胜出模型」评选。
- D1 选型前补核 8.2 第 1、2 条（中文 NLI 与 GLiClass 是最可能胜出的 encoder 路线）。
- 接入任何模型时 pin HF commit + sha256（§4 风险条已定），并在组件清单里带上许可证字段。

## 9. D0-5/D0-6 横评结论（2026-10-07）

> D0-6：在两台真机上实测 D0-5 横评，并补训练/评测近似重复检测。完整数
> 据见 `eval/decide/bench/results/{m1max-64g,m4-16g}/`；本节只给结论与
> 建议。**本节的分档建议都标了「待 jason 拍板」，不是定论**，也不改
> [`PLAN-DECIDE.md`](../agent/PLAN-DECIDE.md) §1.1 的分档草案表本身。

### 9.0 先更正一处假设：Mac mini 实际是 M4 16GB，不是 24GB

PLAN-DECIDE §1.1 分档草案表把「Mac mini M4 24GB」列为 T2 档的典型机
器。2026-10-07 实测这台 Mac mini（`sysctl hw.memsize` 核实）是 **M4
16GB**，比假设少 8GB。本节结果目录因此叫 `results/m4-16g/`，不是
`m4-24g/`。这不是小差异——下面 9.2 的核心发现（SetFit 在这台机器上跑
不完）直接由这 16GB 决定，换成真 24GB 机器结论可能不同，**D1 定档前
需要单独找一台真 24GB 的 M4/M-系列机器复测**，不能拿 16GB 的结果代表
T2 档整体。

### 9.1 两机对照表

两边都是 2026-10-07 单次运行；SetFit 一列用的是去重后的数字（见
9.3），其余候选去重前后分数不变（训练集未涉及它们）。`rule` 候选只覆
盖 `retain_intent`（移植自 `retain.rs`）。延迟单位 ms，RSS 单位 MB。

#### retain_intent（n=101）

| 候选 | m1max-64g 准确率 | m1max P95 | m1max 峰值RSS | m4-16g 准确率 | m4-16g P95 | m4-16g 峰值RSS |
|---|---|---|---|---|---|---|
| rule | 0.317 | 0.0 | 0.1 | 0.317 | 0.0 | 0.1 |
| kev-0.8b | 0.564 | 94.1 | 13.9 | 0.564 | 71.3 | 14.0 |
| gliclass-multilang | 0.416 | 5464.1 | 1003.6 | 0.416 | 334.2 | 1029.5 |
| mdeberta-xnli-control | 0.188 | 353.9 | 1150.9 | 0.198 | 90.5 | 1127.5 |
| erlangshen-nli | 0.089 | 546.3 | 557.2 | 0.089 | 61.8 | 891.8 |
| **setfit-bge-m3（去重后）** | **0.762** | 84.7 | 4269.5 | **UNAVAILABLE**（见 9.2） | — | — |

#### recall_gate（n=100）

| 候选 | m1max-64g 准确率 | m1max P95 | m4-16g 准确率 | m4-16g P95 |
|---|---|---|---|---|
| mdeberta-xnli-control | 0.650 | 143.2 | 0.650 | 36.7 |
| kev-0.8b | 0.520 | 83.3 | 0.510 | 41.8 |
| gliclass-multilang | 0.460 | 1741.8 | 0.460 | 247.7 |
| erlangshen-nli | 0.430 | 106.8 | 0.430 | 22.8 |
| **setfit-bge-m3（去重后）** | **0.850** | 62.7 | **UNAVAILABLE** | — |

#### tool_risk（n=64）

| 候选 | m1max-64g 准确率 | m1max P95 | m4-16g 准确率 | m4-16g P95 |
|---|---|---|---|---|
| kev-0.8b | 0.406 | 88.8 | 0.406 | 78.5 |
| erlangshen-nli | 0.328 | 301.4 | 0.328 | 84.4 |
| mdeberta-xnli-control | 0.297 | 286.9 | 0.312 | 195.4 |
| qwen3guard-0.6b | 0.297 | 436.5 | 0.297 | 412.2 |
| gliclass-multilang | 0.203 | 2191.4 | 0.203 | 438.0 |
| **setfit-bge-m3（去重后）** | **0.469** | 90.5 | **UNAVAILABLE** | — |

### 9.2 关键发现：SetFit(bge-m3) 在 16GB 机器上跑不完——24GB 下能否常驻仍待验证

这是 D0-6 任务里点名要查的问题，实测结果是**否定的，而且不是勉强失
败，是严重 swap thrashing**：

- M4 16GB 上 `uv run bench --all` 跑到 `setfit-bge-m3`（全跑列表里的最
  后一个）时，30 分钟超时（`--_run_one` 子进程的默认超时）被判定
  `UNAVAILABLE`，原因记录为 `subprocess timed out after 1800.0s`。
- 超时期间抓取：`sysctl vm.swapusage` 显示 `used = 5181.19M`（6GB swap
  几乎用满），`vm_stat` 的 `Pages free` 只有 4006 页（约 64MB）。对照
  M1 Max 64GB 上同一候选去重后只需 292s、峰值 RSS 增量 4269.5MB 就能
  跑完三份评测集。
- 这不是"稍微慢一点"——30 分钟跑不完一个在 64GB 机器上 5 分钟搞定的任
  务，差了至少 6 倍，和重度 swap 换页的数量级吻合。**16GB 内存下，
  SetFit(bge-m3) 实质上不可用**，不是"能用但慢"。
- 超时后编排进程正确回收了子进程（未留下孤儿进程），`bench --all` 继
  续跑完剩下的候选并写出报告——"单个候选失败不终止全局"在这个最坏情
  况（硬超时而非异常）下也成立。
- **本节标题里的问题还没有真正答案**：PLAN-DECIDE 假设的 T2 典型机器
  是 24GB，这次实测机器是 16GB（见 9.0）。16GB 下跑不完，不能倒推
  24GB 下也跑不完或能跑完——24GB 多出的 8GB 刚好接近 bge-m3 那 4.2GB
  峰值 RSS 的两倍，有没有富余取决于系统本身占用和是否有其他进程常驻。
  **D1 定档前必须在真 24GB 机器上复测这一项**，不能用这份 16GB 数据
  下结论。

### 9.3 去重前后：SetFit 分数变化

完整数据与解读见 [`eval/decide/bench/results/m1max-64g/2026-10-07-dedup.md`](../../eval/decide/bench/results/m1max-64g/2026-10-07-dedup.md)；摘要：

| 决策点 | 去重前准确率 | 去重后准确率 | 变化 |
|---|---|---|---|
| retain_intent | 0.762 | 0.762 | 0 |
| recall_gate | 0.870 | 0.850 | −0.020 |
| tool_risk | 0.469 | 0.469 | 0 |

三点准确率基本不变（最大降幅 2 个点），印证了 PR #718 复审自己的判
断：训练集里那批近义模板泄漏规模太小（每份评测集 2–6%），解释不了
SetFit 相对其余候选的大幅领先。D1 选型引用去重后的数字即可，不需要
因为这件事怀疑横评的相对排名结论。

### 9.4 其他值得注意的现象（未定论，列出来是为了不被忽略）

- **GLiClass-multilang 在两台机器上的延迟差了一个数量级**（P95：
  retain_intent 5464ms vs 334ms，recall_gate 1742ms vs 248ms，
  tool_risk 2191ms vs 438ms；M1 Max 比 M4 慢 6–16 倍）。两台机器 CPU
  单核性能差距不该有这么大（M4 对 M1 Max 常见跑分大约快 20–30%），更
  可能的解释是 `gliclass` 库在 Apple Silicon 上默认尝试走 MPS（Metal）
  后端，单条小 batch 推理时 host↔device 同步开销可能远超 CPU 直接算；
  M1 Max 那次是跟其余 6 个候选在同一个 `--all` 顺序跑完的，也可能有热
  节流或前序候选留下的系统压力。**这两种解释都未验证**，D1 选型前如
  果要用 GLiClass 路线，需要单独测一下强制 CPU-only 和去掉顺序跑的变
  量再比这个数字，不要直接拿这次的 P95 定论。
- **qwen3guard-0.6b 的峰值 RSS 增量在 16GB 机器上是 64GB 机器的两倍**
  （1178.9MB → 2338.9MB，`tool_risk` 唯一需要加载的候选）。同一个模
  型同一个 revision，内存占用不该翻倍；更可能是内存压力下分配器行为
  / 页面碎片化影响了 `resource.getrusage` 的峰值读数，而不是模型真的
  吃了两倍内存。本次没有专门隔离验证，**后续如果要精确测峰值内存，
  建议换成更细粒度的采样（周期性读 RSS 取 max）而不是进程退出前后一
  次性的 delta**。
- **kev-0.8b 走外部服务，bench 报的 14MB 峰值 RSS 只是 bench 客户端自
  己的内存**，不包含 `kev.serve` 服务进程的真实占用——这次没有在服务
  进程活跃推理时单独采样它的 RSS。T1/T2 如果考虑用 Kev 类模型，需要
  补测服务进程本身在小内存机器上的常驻内存，不能只看这份报告里的
  14MB。

### 9.5 分档建议（待 jason 拍板）

以下建议基于本次实测，**不是最终决定**，也不改 PLAN-DECIDE §1.1 的表：

- **T0（仅规则）**：不受影响，两台机器上 `rule` 候选都是 0 延迟 / 0
  内存。维持现状。
- **T1（8–16GB，无独显或省电模式）**：实测范围内，纯零样本候选
  （GLiClass/mDeBERTa/Erlangshen）在三个决策点上的准确率都偏低
  （GLiClass 最高也只有 0.46，`tool_risk` 上普遍低于或接近随机猜测
  的 4 类基线 0.25），SetFit 又在 16GB 上跑不完——**建议这一档暂时维
  持「仅规则」，不急着塞一个零样本模型进去**，等 D1/D2 有更好的小模
  型候选（或确认 24GB 机器上 SetFit 的真实表现后再评估下调到 T1 是否
  可行）。如果用户所在机器有 Kev 外部服务可用（T1 机器通常没有余量
  常驻额外进程），`kev-0.8b` 在三点上都明显优于零样本候选（0.564 /
  0.510-0.520 / 0.406），但它是英文模型，中文表现是「真实但偏低」，
  且需要补测服务进程本身的内存占用（见 9.4）——只有在确认这两点后才
  该考虑。
- **T2（16–32GB Apple Silicon，如 Mac mini M4）**：这次实测的 16GB
  机型上 SetFit(bge-m3) 不可用；T2 的下限（16GB）和上限（32GB，更接
  近 24GB 假设）行为可能完全不同（见 9.0/9.2）。建议：**T2 内部按内
  存细分**——16–20GB 左右维持 T1 的组合（规则 + 可选 Kev），≥24GB 且
  复测确认 SetFit 能稳定跑完、常驻内存可接受后，才用 SetFit(bge-m3)
  作为深度层。这一条尤其需要 jason 拍板，因为它意味着 T2 不是一个统
  一的组合，需要再切一刀。
- **T3（≥32GB，如 M1 Max 64GB）**：SetFit(bge-m3) 去重后三点准确率
  0.762/0.850/0.469，明显领先其余候选（§9.1），峰值 RSS 增量
  4.2–4.3GB、加载 200–290s，在 64GB 机器上都有余量。建议 T3 用
  SetFit(bge-m3) 作为三个决策点的统一模型；加载耗时较高（近 5 分钟），
  若做成「首次使用时按需加载」要在 UI 上提示用户等待，不要让它看起来
  像卡死。

### 9.6 Unavailable 项汇总

- **setfit-bge-m3 @ m4-16g**：`subprocess timed out after 1800.0s`，
  实测为重度 swap thrashing（swap 用到 5.18/6GB），非崩溃，详见 9.2。
- 其余候选在两台机器上均可用，包括 `kev-0.8b`（按 README 指引临时起
  了 `kev.serve` 服务，验收后已停止，未改动用户机器上任何既有模型或
  服务）。

## 10. D0-8 同系列选型（2026-10-07）

> 任务见 [`PLAN-DECIDE.md`](../agent/PLAN-DECIDE.md) D0-8。本节只在**本机
> M1 Max 64GB 笔记本**上实测（硬约束：禁止 ssh/Mac mini）；完整原始结果
> 见 `eval/decide/bench/results/m1max-64g-d0-8*/`。**五档里除「64GB+」外
> 的内存可用性都是按本机实测 RSS 推算，不是在对应档位机器上实测**——
> 跟 §9.0 的教训一样（16GB 和假设的 24GB 行为不同），这里的 8/16/24/32
> 档结论同样需要后续真机复核才能定档。

### 10.0 范围与方法

- **三语评测集**：评测集与训练集已从 `ab/decide-01`（PR #726）合入
  `ab/decide`，每条都有显式 `lang`（`zh`/`en`/`th`/`mixed`），`retain_intent`
  226 条 / `recall_gate` 221 条 / `tool_risk` 144 条，`eval/decide/bench
  --check-overlap` 在阈值 0.7 下仍为 0 命中。**泰文条目由模型撰写，母语
  者复核尚未完成**（见 `eval/decide/TH_REVIEW.md`）——本节所有泰文分数
  只作参考，不作为选型硬门槛，和 README 的既有声明一致。
- **render.py 的 bug 修复**：#726 评审指出 `tool_risk` 的渲染前缀固定
  用中文模板（「工具调用：…，参数：…」），把英文/泰文的 `args`/`context`
  糊在一个中文壳子里，不只是读起来别扭，还会在语言推断时制造假信号。
  本节的所有数字已经用修过的语言中立模板（`tool: … args: … context:
  …`）跑出，`decide_bench/render.py` 的改动见本 PR diff。`qwen3guard-0.6b`
  自己另起一套中文 prompt（不经过 `render_item_text`），不在这次修复范
  围内，三语下可能有相同问题，未修，分数照旧标出。
- **训练/推理分离**：`embed_head.py` 家族（`e5-*`/`qwen3-embed-*`/
  `minilm-multilingual`）的"训练"只是在已加载的 backbone 上 `encode()`
  几十到两百条 `train_data/` 句子再拟合一个 `sklearn.LogisticRegression`
  头，训练阶段和推理阶段的 RSS 增量基本相等（见 10.3）——这不是测量误
  差，是因为两者复用同一个常驻 backbone，没有本质不同的"训练态"。只有
  `setfit-bge-m3`（SetFit 库真正的对比学习训练器）训练阶段有意义的更高
  峰值。五档判定只看**推理**成本，训练只在本机做一次、落盘保存头。
- **预算假设**：「快速层」（每条消息都跑）用 **≤ 推理 RSS 占物理内存
  10%**；「深度层」（可选、按需加载，PLAN-DECIDE §7.3 的第 3 层）放宽到
  **≤ 20%**，因为它不是每条消息都跑、且用户可见「正在加载深度模型」的
  等待态。这两个数字是本节自己定的分析假设，不是 jason 已拍板的数字。

### 10.1 D0-8 新增候选许可证结论表

> 方法同 §8：读 HF `cardData.license`（YAML frontmatter，经
> `huggingface_hub.model_info` 解析，等同于直接读 README 的 YAML 字段）、
> 必要时读原始 README/LICENSE 全文；核实日期 2026-10-07。

| 候选 | 确切 repo | 许可证 | 可商用 | 出处 | 原文已核实 | 结论 |
|---|---|---|---|---|---|---|
| Qwen3-Embedding-0.6B/4B/8B | `Qwen/Qwen3-Embedding-{0.6B,4B,8B}` | Apache-2.0 | 是 | https://huggingface.co/Qwen/Qwen3-Embedding-0.6B/raw/main/README.md（4B/8B 同路径） | 是（YAML） | **保留**，三档同系列、同许可证 |
| multilingual-e5 small/base/large/large-instruct | `intfloat/multilingual-e5-{small,base,large,large-instruct}` | MIT | 是 | https://huggingface.co/intfloat/multilingual-e5-base/raw/main/README.md（其余同路径） | 是（YAML） | **保留**。训练数据含 mC4/CC 等网页语料，卡片未声明 NC 限制；未逐条核对每个子数据集许可证（见 10.1.1） |
| paraphrase-multilingual-MiniLM-L12-v2 | `sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2` | Apache-2.0 | 是 | https://huggingface.co/sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2/raw/main/README.md | 是（YAML） | **保留**，8GB 档极小体积对照 |
| Qwen3（因果 LLM 底座）0.6B/1.7B/4B/8B | `Qwen/Qwen3-{0.6B,1.7B,4B,8B}` | Apache-2.0 | 是 | https://huggingface.co/Qwen/Qwen3-0.6B/raw/main/README.md（其余同路径） | 是（YAML） | **保留**。本仓库实际加载的是 `mlx-community` 的预量化版（见下），权重来自同一组 Apache-2.0 base |
| mlx-community 预量化包 | `mlx-community/Qwen3-{1.7B,4B}-4bit` | Apache-2.0 | 是 | https://huggingface.co/mlx-community/Qwen3-4B-4bit/raw/main/README.md（1.7B 同路径） | 是（YAML） | **保留**，纯量化重打包，不改变底座许可证 |
| Kev-4B | `jaredpalmer/kev-4b` | Apache-2.0 | 是 | https://huggingface.co/jaredpalmer/kev-4b/raw/main/README.md | 是（YAML） | **保留，待补数据许可**（同 §8.1 Kev-4B 行）。`provenance.json` 显示训练数据为 `evals/round10/skills/train.jsonl`，具体许可证仍记在未读的 suite manifest 里；base `Qwen/Qwen3.5-4B-Base`（Apache-2.0，revision `1001bb4d826a52d1f399e183466143f4da7b741b`） |
| KaLM-embedding-multilingual-mini-instruct-v2.5 | `KaLM-Embedding/KaLM-embedding-multilingual-mini-instruct-v2.5` | Apache-2.0 | 是（权重层面） | https://huggingface.co/KaLM-Embedding/KaLM-embedding-multilingual-mini-instruct-v2.5/raw/main/README.md | 是（YAML） | **仅对照，不进生产候选**——README 未列全部训练数据来源明细，本次时间预算内未逐一核对是否含 NC 语料；按保守原则不推荐。**且本次实测环境不兼容**：`transformers==5.19.0` 加载其自定义建模代码报 `'Qwen2Config' object has no attribute 'rope_theta'`，`unavailable`，不是许可证问题，是环境/版本问题 |

**任务里点名的 AgentJev-0.6B / KaLM-Jev Nano**：在 HuggingFace 上搜索未
找到与这两个名字严格对应的官方仓库（搜到的同名/近名条目要么是第三方
未注明出处的重新上传如 `lujihong/agentjev-0.6b-int8-onnx`，要么是不同
项目），**本次未下载、未测**——不确定官方 repo 是哪个、许可证是什么，
比瞎猜一个下载更不负责任。如果 jason 能提供准确的 HF repo id，下一轮
可以补测。

**小结**：所有新增候选的权重许可证同样全部是 Apache-2.0 或 MIT，没有
NC/自定义许可；延续 §8.1 的结论——真正的风险点在训练数据，不是权重许
可证本身。

#### 10.1.1 未核实项（延续 §8.2 编号）

7. multilingual-e5 系列的具体训练数据集清单（mC4 等）每个子集的许可证
   未逐一核对。
8. Kev-4B 的 suite manifest（各数据源许可证）仍未读，同 §8.2 第 4 条对
   Kev-4B 本身的结论。
9. KaLM-embedding-v2.5 的训练数据来源未逐一核对，这是它被列为「仅对
   照」而非生产候选的主要原因（不是许可证证据上有负面发现，而是没有
   查够，按保守原则处理）；且环境不兼容，本次实际上也没能跑出准确率
   数字可供参考。

### 10.2 横评结果（三语合计，详细按语言分表见各 `results/*/*.md`）

全部数字 2026-10-07 单次运行，本机 M1 Max 64GB；`setfit-bge-m3` 与
`embed_head.py` 家族训练用的是当前（合入 #726 后）的 `train_data/`，比
§9 的数字基于更大的训练/评测集，**与 §9 的旧数字不可直接比较**。

#### retain_intent（n=226：zh 86 / en 66 / th 62 / mixed 12）

| 候选 | 系列 | 准确率 | 宏F1 | 代价加权误判率 | P50(ms) | P95(ms) |
|---|---|---|---|---|---|---|
| rule | — | 0.230 | 0.120 | 0.800 | 0.0 | 0.0 |
| erlangshen-nli | NLI 零样本 | 0.124 | 0.037 | 0.899 | 279 | 581 |
| mdeberta-xnli-control | NLI 零样本（对照） | 0.190 | 0.147 | 0.833 | 336 | 1689 |
| gliclass-multilang | GLiClass | 0.367 | 0.276 | 0.642 | 2485 | 5067 |
| qwen3-llm-0.6b | D. Qwen3 LLM 读 logit | 0.319 | 0.269 | 0.718 | 51 | 75 |
| qwen3-llm-1.7b | D. Qwen3 LLM 读 logit | 0.416 | 0.304 | 0.495 | 162 | 214 |
| qwen3-llm-4b | D. Qwen3 LLM 读 logit | 0.642 | 0.629 | 0.307 | 222 | 440 |
| qwen3-llm-8b | D. Qwen3 LLM 读 logit | 0.752 | 0.720 | 0.216 | 376 | 389 |
| kev-0.8b | E. Kev | 0.496 | 0.493 | 0.508 | 163 | 261 |
| kev-4b | E. Kev | 0.690 | 0.663 | 0.292 | 504 | 959 |
| minilm-multilingual | C. 8GB 对照 | 0.743 | 0.743 | 0.212 | 17 | 31 |
| e5-small | B. e5 | 0.770 | 0.780 | 0.196 | 21 | 36 |
| e5-base | B. e5 | 0.832 | 0.840 | 0.150 | 23 | 38 |
| e5-large | B. e5 | 0.836 | 0.844 | 0.137 | 36 | 56 |
| e5-large-instruct | B. e5 | **0.858** | **0.865** | **0.127** | 33 | 58 |
| qwen3-embed-0.6b | A. Qwen3-Embedding | 0.823 | 0.831 | 0.154 | 50 | 282 |
| qwen3-embed-4b | A. Qwen3-Embedding | 0.832 | 0.836 | 0.152 | 92 | 167 |
| qwen3-embed-8b | A. Qwen3-Embedding | 0.841 | 0.853 | 0.140 | 125 | 190 |
| setfit-bge-m3 | C. 对照基线 | 0.832 | 0.845 | 0.165 | 38 | 69 |

#### recall_gate（n=221：zh 87 / en 65 / th 62 / mixed 7）

| 候选 | 系列 | 准确率 | 宏F1 | 代价加权误判率 | P50(ms) | P95(ms) |
|---|---|---|---|---|---|---|
| erlangshen-nli | NLI 零样本 | 0.443 | 0.307 | 0.495 | 68 | 144 |
| mdeberta-xnli-control | NLI 零样本（对照） | 0.611 | 0.503 | 0.466 | 95 | 146 |
| gliclass-multilang | GLiClass | 0.538 | 0.518 | 0.452 | 1895 | 3084 |
| qwen3-llm-0.6b | D | 0.557 | 0.358 | 0.505 | 63 | 76 |
| qwen3-llm-1.7b | D | 0.557 | 0.358 | 0.505 | 161 | 190 |
| qwen3-llm-4b | D | 0.796 | 0.792 | 0.247 | 161 | 168 |
| qwen3-llm-8b | D | 0.787 | 0.775 | 0.244 | 283 | 299 |
| kev-0.8b | E | 0.484 | 0.455 | 0.521 | 108 | 172 |
| kev-4b | E | 0.679 | 0.608 | 0.365 | 223 | 413 |
| minilm-multilingual | C | 0.787 | 0.787 | 0.251 | 17 | 26 |
| e5-small | B | 0.805 | 0.805 | 0.251 | 23 | 47 |
| e5-base | B | **0.869** | **0.868** | **0.158** | 21 | 35 |
| e5-large | B | 0.828 | 0.828 | 0.210 | 34 | 54 |
| e5-large-instruct | B | 0.837 | 0.835 | 0.215 | 32 | 51 |
| qwen3-embed-0.6b | A | 0.801 | 0.800 | 0.224 | 48 | 80 |
| qwen3-embed-4b | A | 0.824 | 0.823 | 0.217 | 81 | 122 |
| qwen3-embed-8b | A | 0.864 | 0.864 | 0.185 | 139 | 202 |
| setfit-bge-m3 | C | 0.837 | 0.837 | 0.194 | 34 | 54 |

#### tool_risk（n=144：zh 64 / en 40 / th 40）

| 候选 | 系列 | 准确率 | 宏F1 | 代价加权误判率 | P50(ms) | P95(ms) |
|---|---|---|---|---|---|---|
| erlangshen-nli | NLI 零样本 | 0.299 | 0.226 | 0.585 | 152 | 233 |
| mdeberta-xnli-control | NLI 零样本（对照） | 0.319 | 0.215 | 0.573 | 239 | 1553 |
| gliclass-multilang | GLiClass | 0.201 | 0.096 | 0.933 | 3194 | 5980 |
| qwen3guard-0.6b | 内容安全分类器 | 0.292 | 0.241 | 0.838 | 315 | 488 |
| qwen3-llm-0.6b | D | 0.319 | 0.217 | 0.708 | 84 | 103 |
| qwen3-llm-1.7b | D | 0.278 | 0.175 | 0.793 | 200 | 252 |
| qwen3-llm-4b | D | 0.306 | 0.246 | 0.724 | 262 | 363 |
| qwen3-llm-8b | D | 0.528 | 0.545 | 0.452 | 460 | 557 |
| kev-0.8b | E | 0.431 | 0.401 | 0.618 | 224 | 277 |
| kev-4b | E | **0.701** | **0.695** | **0.421** | 293 | 624 |
| minilm-multilingual | C | 0.528 | 0.479 | 0.476 | 18 | 28 |
| e5-small | B | 0.472 | 0.444 | 0.524 | 27 | 46 |
| e5-base | B | 0.535 | 0.518 | 0.438 | 28 | 56 |
| e5-large | B | 0.535 | 0.484 | 0.495 | 46 | 89 |
| e5-large-instruct | B | 0.549 | 0.538 | 0.497 | 36 | 61 |
| qwen3-embed-0.6b | A | 0.660 | 0.640 | 0.421 | 58 | 116 |
| qwen3-embed-4b | A | **0.688** | **0.681** | 0.391 | 129 | 221 |
| qwen3-embed-8b | A | 0.653 | 0.644 | 0.454 | 202 | 375 |
| setfit-bge-m3 | C | 0.542 | 0.520 | 0.389 | 33 | 57 |

**跨点小结**：

- **Qwen3-Embedding 和 e5 两个 embedding 家族都明显优于三个零样本路线
  （GLiClass/mDeBERTa/Erlangshen）和四档 Qwen3 因果 LLM 的小尺寸**——
  这和 §7.1 的结论一致（专用分类器优于用小 LLM 当分类器），在三语、更
  大评测集上依然成立。
- **同系列尺寸连贯、规模收益递减**：Qwen3-Embedding 0.6B→4B→8B 在
  `retain_intent`（0.823→0.832→0.841）和 `recall_gate`
  （0.801→0.824→0.864）上单调提升，但增量越来越小；`tool_risk` 上 4B
  反而略高于 8B（0.688 vs 0.653），不是严格单调。e5 small→base→large
  也类似：base 之后继续加大不再明显提升（large 甚至在 `recall_gate` 上
  不如 base），**"越大越好"在这两个决策点上过了 base/0.6B 这一档边际收
  益就很小**，这是选型表没有无脑选最大模型的主要依据。
- **Qwen3 因果 LLM 读 logit 的尺寸曲线最陡**：0.6B 几乎不可用
  （`retain_intent` 0.319，接近 6 类随机猜测的 0.167 但也没高多少），
  8B 才追上embedding 路线的下限（0.752/0.787/0.528）。这条路线如果要
  用，**不能用小尺寸**，和 embedding 家族"小尺寸也能用"的特性相反。
- **Kev 同样尺寸越大越好，且在 `tool_risk` 上是本次横评的最高分**
  （kev-4b 0.701），但延迟也最高（P95 624ms，是同点最快候选的 20 多
  倍）——适合 PLAN-DECIDE §7.3 的"深度层"定位（按需加载、处理复杂判
  断），不适合每条消息都跑的快速层。kev 的 RSS 数字（13–14MB）只是
  bench 客户端自己的内存，不是 `kev.serve` 外部进程的真实占用，README
  里已经说明这个口径限制，这次同样没有单独采样服务进程。
- **setfit-bge-m3 不再像 §9 那样明显领先**：在这份更大、三语的评测集
  上，它的三点分数（0.832/0.837/0.542）和 e5-base／qwen3-embed-8b 基本
  同一水平，不再是单一碾压选项——§9 的数据是在小得多（且几乎全中文）
  的旧评测集上测的，**跨语言泛化之后，差距消失了**。这是本次最值得写
  进结论的发现：如果 jason 之前倾向"T3 就用 SetFit"，这次数据不再强支
  持"非它不可"，e5-base 体积小一百倍、延迟低一个量级，三语平均分接
  近，是更均衡的选择。
- **按语言看，三个零样本路线和 `rule` 基线在非中文上明显更弱**（如
  `rule` 在 `retain_intent` 上 zh 0.326 / en 0.167 / th 0.177，完整分
  表见 `results/m1max-64g-d0-8/2026-10-07.md`），这正是 D0-4 评测集扩成
  三语要解决的问题；embedding 家族的语言差距小得多（e5-large-instruct
  在 `retain_intent` 上 zh 0.849 / en 0.818 / th **0.887**，泰文反而最
  高——但泰文条目未经母语者复核，这个"反而最高"可能是机器生成的泰文
  题面本身更规整、更贴近训练分布，不能直接读成"泰文能力真的最强"）。

### 10.3 训练/推理资源分离

| 候选 | 加载耗时(s) | 加载阶段RSS增量(MB) | 整次运行峰值RSS增量(MB) | 下载体积(MB) |
|---|---|---|---|---|
| setfit-bge-m3 | 454 | 3932 | 3932 | 4353 |
| e5-small | 24 | 1049 | 1056 | 470 |
| e5-base | 14 | 1046 | 1052 | 1082 |
| e5-large | 17 | 1048 | 1054 | 2157 |
| e5-large-instruct | 16 | 1041 | 1047 | 1089 |
| minilm-multilingual | 14 | 1197 | 1220 | 458 |
| qwen3-embed-0.6b | 18 | 764 | 827 | 1152 |
| qwen3-embed-4b | 34 | 769 | 822 | 7686 |
| qwen3-embed-8b | 58 | 625 | 625 | 14449 |

`加载阶段RSS增量`（训练阶段，对会训练头的候选）和`整次运行峰值`
（含推理）几乎相等，印证了 10.0 的预期：这个头训练配方（`encode()` +
`LogisticRegression`）不会产生比推理更高的内存峰值。唯一数量级不同的
是 `setfit-bge-m3`（真训练器，3.9GB），它的训练耗时（454s，约 7.5 分
钟）也比其余候选（14–58s）高一个量级——这部分峰值内存和耗时发生在
**开发机训练一次、落盘保存头**的阶段，用户机器只做推理，不会复现这
454s/3.9GB。

**已知不稳定项（如实记录，不挑一个数字假装精确）**：`qwen3-embed-4b`／
`qwen3-embed-8b` 在两次独立跑测里的峰值 RSS 差出一个数量级——更早一次
单独跑这两个候选时记到 6784MB／8566MB，上表这次（和其余 7 个候选一起
跑、系统刚载入过更多模型之后）只记到 822MB／625MB。这和 §9.4 记录的
`qwen3guard-0.6b` RSS 在两台机器上翻倍是同一类问题：当前 runner.py 用
进程退出前后的一次性 delta（`resource.getrusage().ru_maxrss`），在内
存压力、分配器行为不同的情况下不可靠。**本节五档表里 Qwen3-Embedding
的内存数字采用两次里较高的那个（更保守）**，但真实数字需要用
`export_onnx.py` 那种周期采样方法重新测，不能当作定论。

### 10.4 ONNX 导出（Rust `ort` 集成路线的第一步）

用 `scripts/export_onnx.py` 把 `e5-base`（10.2 里`recall_gate`最高分、
综合表现均衡、体积小的候选）导出 ONNX 并做动态 int8 量化：

| 项 | fp32 ONNX | int8 ONNX（动态量化） |
|---|---|---|
| 文件体积 | 1080 MB | **287 MB**（≈ 3.8x 压缩） |
| retain_intent 准确率 | — | 0.819（PyTorch 原始：0.832，量化掉了 1.3 个点） |
| recall_gate 准确率 | — | 0.864（PyTorch 原始：0.869） |
| tool_risk 准确率 | — | 0.514（PyTorch 原始：0.535） |
| P50/P95 延迟（onnxruntime，CPU） | — | 15ms / 39ms（retain_intent，三点里最慢的 tool_risk 是 37/82ms） |
| 导出+量化+推理总耗时 | — | 29.7s |
| 本次测到的峰值 RSS | — | **10.7GB——不是生产数字，见下** |

**int8 量化基本不掉分**（三点平均降 0.6–2.1 个点），延迟比 PyTorch 原
生更快（15ms vs e5-base 的 23ms P50）。**峰值 RSS 10.7GB 这个数字不能
直接拿来做五档判断**：`export_onnx.py` 的这次运行把 PyTorch 导出（需要
同时装 `torch`+`transformers` 来源模型）、量化、onnxruntime 推理全部
算在同一个进程的生命周期内周期采样，10.7GB 主要是**一次性导出步骤**
的开销（`optimum` 用 PyTorch 追踪模型图），不是 Rust `ort` 只加载那
287MB int8 文件做推理时的真实占用——后者预期接近"int8 文件体积 +
onnxruntime 运行时开销"，量级应该在几百 MB，不是 10GB，但**本次没有单
独测这个数字**（需要把导出步骤和推理步骤分成两个进程才能干净地测）。
这是留给下一轮的工作，不在这次报告里编一个数字出来。

### 10.5 五档推荐表（8 / 16 / 24 / 32 / 64GB+，待 jason 拍板）

> 不改 `PLAN-DECIDE.md` §1.1 的分档草案表本身。预算假设见 10.0：快速层
> ≤10% 物理内存、深度层 ≤20%。**8/16/24/32 档的内存可用性都是从本机
> 64GB 实测 RSS 推算，不是在对应内存的机器上实测**，上线前必须在真机
> 复核——这条和 §9.0/§9.2 的教训完全一样，这次没有条件在笔记本上模拟
> 更小内存的机器，只能把同一句话再说一遍。

| 档位 | 快速层推荐 | 推理RSS | 10%预算 | 是否超预算 | 依据 |
|---|---|---|---|---|---|
| **8GB** | `e5-small`（B 系列最小档） | ~1.05GB | 0.8GB | **超约 30%** | 三语平均分（0.770/0.805/0.472）明显优于零样本路线和 rule，体积最小的两个候选（minilm 1.22GB、e5-small 1.05GB）里 e5-small 分数更高；但确实超过严格 10% 预算——8GB 机器的该预算本身就很紧，比 §9.2 SetFit 在 16GB 上 thrashing 的那种"4GB 比 2GB 预算"情况宽松一些（这里是 1.05 比 0.8，超约 30% 而非几倍），**风险比 §9.2 低但不是零**，如果 jason 认为 8GB 档必须严格卡 10%，备选是 `rule` only（T0 语义，0 内存但§10.2 显示跨语言准确率只有 0.12–0.23） |
| **16GB** | `e5-base`（同系列下一档） | ~1.05GB | 1.6GB | 不超 | 同系列从 small 到 base 是真实的下一个尺寸台阶（120M→278M 参数），三点分数全面提升（尤其 `recall_gate` 0.805→0.869），RSS 几乎不变（e5 系列的 RSS 主要是 transformers/torch 运行时本身的开销，不随这个尺寸区间线性增长）——这正是"小档质量够、本身就该往上走一档"而不是"质量不行所以混搭"的例子 |
| **24GB** | `e5-large-instruct`（同系列、instruct 变体） | ~1.05GB | 2.4GB | 不超 | 10.2 显示 `large-instruct` 在三点上全面不输甚至优于 `large`（同样 560M 参数，指令微调变体），RSS 同样几乎不变；这一档把"同系列"坐实到参数量意义上的最大 e5 尺寸，往上 e5 没有更大的型号了 |
| **32GB** | `qwen3-embed-4b`（切换到 A 系列，理由见下） | ~0.8–6.8GB（不稳定，见10.3，保守按6.8GB） | 3.2GB | **按保守数字超约 2 倍** | e5 系列到 large 已经没有更大尺寸，继续用同一个模型在 32GB 档"浪费"了多出来的 8GB——`qwen3-embed-4b` 在 `tool_risk` 上有真实提升（0.549→0.688），`retain_intent`/`recall_gate` 持平或小升，是用这档多余预算换真实收益的合理选择，**但 10.3 的 RSS 测量不稳定问题在这里最致命**：若保守数字（6.8GB）是真的，这已经远超严格 10% 预算，只是用深度层 20% 的假设（6.4GB）也勉强不够；若实际更接近乐观数字（0.8GB）则完全在预算内。**这一档的结论最依赖后续用周期采样法重测**，现在只能给一个范围 |
| **64GB+** | `qwen3-embed-8b`（同系列下一档，深度层备选 `kev-4b`） | ~0.6–8.6GB（同样不稳定，保守按8.6GB） | 6.4GB（10%）／12.8GB（20%深度层） | 按保守数字不超深度层预算 | 延续 32GB 档的系列（A），8B 三点分数（0.841/0.864/0.653）是 embedding 路线里最高或接近最高；64GB 机器哪怕按保守的 8.6GB 算也只占 13.4%，落在深度层 20% 预算内。可选再加一层 `kev-4b`（PLAN-DECIDE §7.3 的"深度层"定位）处理复杂 `tool_risk` 判断——它的 0.701 是全表最高分，代价是 P95 624ms，不适合快速层但适合按需调用。`setfit-bge-m3` 仍是可选对照（三点分数相近，体积更小但训练耗时高），不作为默认推荐，理由见 10.2 的"不再明显领先"小结 |

**为什么不是单一系列贯穿全部五档**：multilingual-e5 系列只有三个真实
参数量台阶（small 120M / base 278M / large 560M，`large-instruct` 是同
尺寸的变体不是第四个台阶），自然覆盖 8/16/24GB 三档；32GB/64GB+ 这两
档的预算足够装下更大的模型，继续用 560M 的 e5-large 不是"质量不够"，
是"没有更大的同系列模型可用、白白浪费预算"，所以换到 Qwen3-Embedding
系列（0.6B 这档没有用上，因为 e5-large 在 24GB 档的表现已经持平或优于
它）接上 4B/8B 两档。**这是"先穷举同系列能到哪、到头了再换系列接上"
的做法，不是每档各挑一个不相关的模型**。

### 10.6 Unavailable 项汇总

- **`kalm-embed-v2.5-reference`**：`'Qwen2Config' object has no attribute
  'rope_theta'`——这台机器装的 `transformers==5.19.0` 与该模型自带的
  `trust_remote_code` 建模代码不兼容，不是许可证或下载问题；见 10.1。
- **AgentJev-0.6B / KaLM-Jev Nano**：未下载、未测——找不到确定的官方
  HF repo，见 10.1。
- 其余全部候选（A/B/C/D/E 五类共 20 个新增 + 7 个 D0-5 既有候选）本次
  在 64GB 笔记本上都可用，完整 `unavailable` 字段见各
  `results/m1max-64g-d0-8*/2026-10-07.json`。

### 10.7 本次下载与清理（PR body 另有完整台账）

新下载合计约 42GB（HF 缓存从会话开始的 37GB 增长到跑完时的 79GB），在
60GB 预算内；`kev-4b` 的 base（`Qwen/Qwen3.5-4B-Base`，约 9.3GB）和
`qwen3-embed-8b`（约 14.4GB）是其中最大的两块。跑完后**没有删除任何本
次下载的模型**——磁盘总可用约 93GB，且这些
模型本身就是下一轮复测（尤其 10.3 提到的 RSS 重测）会复用的对象，删
了还要重下没有意义；如果 jason 要收紧磁盘，可以安全删除
`~/.cache/huggingface/hub/models--Qwen--Qwen3-Embedding-8B`（14.4GB，
本次横评里不是五档推荐的强制依赖，e5 系列已覆盖大部分场景）和
`~/.cache/huggingface/hub/models--Qwen--Qwen3.5-4B-Base`（9.3GB，只被
`kev-4b` 用到）。`kev.serve` 的两个临时服务（0.8b/4b）已在跑完后停
止，未改动用户机器上任何既有模型或服务，和 D0-5/D0-6 的既有约定一致。
