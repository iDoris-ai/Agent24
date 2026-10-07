# 本地校准决策模型调研（TypeSafe Jev / Ollaya 生态 + 成熟替代方案）

> 调研时间：2026-10-06。本文聚焦"用校准过的决策模型（choice/yes-no/score）替代 prompt 或正则规则"这一命题的证据。
> 标注规则：每条事实性陈述后标 **已核实（链接）** 或 **未核实**。

---

## 1. TypeSafe Jev / "System One" API

- Jev 是 TypeSafe AI 于 **2026-09-15** 发布的首个公开 "System One Model"：不生成文本，而是对输入 state + 一组"类型化问题"返回**校准概率**。已核实（[DataCamp](https://www.datacamp.com/blog/system-one-models-jev)、[systemonemodels.org](https://systemonemodels.org/)）。
- 决策类型三种，精确术语：**Choice**（多选分类）、**Noul**（是/否概率，不是常见的"yes/no"拼写）、**Score**（打分/评分，带概率分布）。已核实（[Kev README](https://github.com/jaredpalmer/kev) 实际请求示例中出现 `"type": "choice"` / `"type": "noul"` / `"type": "score"`）。
- 校准方法论：TypeSafe 称为 **RLCD（Reinforcement Learning for Calibrated Decisions）**，与 RLHF（人类偏好）、RLVR（程序可验证）并列，目标是让输出概率"认知上诚实"。已核实（[DataCamp](https://www.datacamp.com/blog/system-one-models-jev)、[aimlapi 博客](https://aimlapi.com/blog/what-is-jev)）。
- 校准表现（ECE，按任务）：content moderation 0.032（好）、intent routing 0.096（尚可）、answer rating 0.284（差）——说明"校准"并非万能，任务越主观校准越差。已核实（WebSearch 聚合结果，源自 [aimlapi](https://aimlapi.com/blog/what-is-jev) 等博客二次转述；未直接抓到 TypeSafe 官方技术报告原文，**建议视为二级来源，标记弱已核实**）。
- API 端点：`POST https://api.typesafe.ai/v1/systemone`，模型路由名 `jev-latest`。已核实（WebSearch 聚合 + Kev README 中引用的 `TypeSafeClient(base_url=..., model="kev-latest")` 兼容协议佐证）。
- 定价/可用性：官网给出 **$42/十亿 input tokens**，宣传"比 Claude Fable 5.1 便宜 238 倍、快 193.6 倍、整体成本低 244.6 倍"；状态为 **early access**（早期访问，非正式 GA，也非纯 waitlist——可注册直接用）。已核实（WebFetch typesafe.ai 首页）。systemonemodels.org 给出更细的跨厂商价格对比：Jev 一类商业 API 多在 $0.03–0.24/MTok 区间，也有免费项（如 Mercury Decide $0/MTok）。已核实（WebFetch systemonemodels.org）。两处定价数字口径不同（官网 $42/B ≈ $0.042/MTok，与第三方汇总的 $0.03–0.04/MTok 量级基本吻合）。
- **权重是否开放**：Jev 本身是**纯托管 API，不开放权重**（"shipped as a closed managed API"）。已核实（systemonemodels.org）。但同一生态里有多个开源"平替"模型可下载权重，见下文 Kev/Ollaya 部分。
- 定位延伸阅读：有独立第三方站点 systemonemodels.org 明确声明"与 TypeSafe AI 无关联"，起到中立对比作用，可信度较高。已核实。

## 2. jev-skill（github.com/wuyoscar/jev-skill）

- 仓库定位：面向 coding agent 的 **Jev 使用案例/工作流/skill 合集**（"An awesome collection of Jev use cases, workflows, and agent skills"）。已核实（`gh api repos/wuyoscar/jev-skill`）。
- 规模与成熟度：**575 stars，44 forks，MIT 许可**，创建于 2026-09-20，最后 push 2026-09-30（约一周内活跃，随后趋缓）。已核实（gh api 元数据）。
- Skills 列表（`skills/` 目录下 5 个）：`jev`、`jev-act`、`jev-documents`、`jev-eval`、`jev-triage`。已核实（`gh api repos/wuyoscar/jev-skill/contents/skills`）。README 自述 "5 skills, 108 scenarios, 66 projects/resources, 14 recorded input/output pairs"。已核实（README 原文）。
- **no-silent-fallback 规则**：确认存在，原文核心句：
  > "A silent fallback is a hidden bug. When required data is missing or stale, the correct response is a loud, informative error — not a '---' placeholder, a skipped section, an NA fill, or a conditional block that quietly omits output."
  以及："if removing the fallback would cause a visible failure, the fallback is hiding a real problem. Remove the fallback and let the failure be visible." 对允许的条件渲染场景，要求"可见警告 + 明确标注被跳过的部分"（如 "B4: Skipped — race data not available"），而不是静默跳过。已核实（WebSearch 命中该仓库文档内容片段，原文疑似位于 `docs/` 或 skill 内 SKILL.md；未直接 WebFetch 到该具体文件，**规则存在性已核实，但确切文件路径未核实**）。
- 对 Agent24 的意义：这条规则直接支持 owner 的直觉——当决策 API/模型不可用或置信度不足时，系统应该**报错或转人工**，而不是静默退化成猜测性 prompt 输出。

## 3. Kev（github.com/jaredpalmer/kev）

- **确认为本地可训练/可运行的"Jev 平替"决策模型家族**，不是单一 LoRA，而是 **4 个尺寸**：0.8B / 4B / 9B / 27B。已核实（README、`gh api`）。
- 许可证：**Apache-2.0**（仓库整体许可证字段确认，且 README 注明权重基座 Qwen3.5/Qwen3.8 同为 Apache-2.0）。已核实。
- 基座模型：0.8B/4B/9B 基于 **Qwen3.5-Base**（小参数版本上是 LoRA + 冻结基座的小适配器），27B 基于 **Qwen3.8-27B post-trained**，是**全量微调**（不是 LoRA），故 27B 体积达 **51GB 完整权重**（HF Hub 托管，GitHub Release 放不下，只有 0.8B/4B/9B 的 checksum 在 Release 里）。已核实（README 原文表格 + "Kev 1.0" 版本说明）。
- 体积/规模要点：0.8B/4B/9B 是"小适配器 + 冻结基座"架构（LoRA 风格），**真实下载增量很小**（适配器本身，不含基座），27B 是 51GB 全量权重。已核实。
- 成熟度：**8565 stars，563 forks**，持续活跃，最后 push 就在 2026-10-06（调研当天）。已核实（gh api）。
- 校准质量实测（Brier score，越低越好）：Kev-27B 在 New Sources 上 0.225/0.156（dev/test），接近 Jev 的 0.211；Kev-4B/9B 在 0.269–0.289 区间。已核实（README 内公开评测表，但为作者自评，非第三方独立复现，**建议标记为"厂商自评，弱已核实"**）。
- 中文支持：README **未明确提及**中文语言支持（Qwen 系列基座本身对中文友好是业界常识，但 Kev 自身的中文专项评测**未见公开数据**）。未核实。

## 4. Ollaya（github.com/ollaya-dev/ollaya）

- 定位："像 Ollama 跑 LLM 一样跑开源决策模型"，本地 daemon + CLI，协议**完全兼容 TypeSafe 的 `/v1/systemone`**（换一个 base_url 环境变量即可把 Jev 客户端指向本地）。已核实（README、`gh api`）。
- 技术栈：**Rust** 编写，Apache-2.0，由 Mert Cobanov 维护。已核实。
- 成熟度：**1216 stars，70 forks**，创建于 2026-09-23，最后 push 就在 2026-10-06（当天），活跃度很高但历史很短（不到 3 周）——**需警惕"新锐小项目"风险，生产可用性有限**。已核实（gh api）。
- 运行时架构：对 **非 GGUF 模型**（如 laya/decider/nli/gliclass/von/decima 等编码器/小解码器）用 **ONNX Runtime**（CPU + CUDA）；对**作者自己发布 GGUF 的模型**（`winnow`、`jevk5`、`jeb`）直接跑 **llama.cpp**，支持 CPU / Vulkan / CUDA / **Apple Silicon 上的 Metal**。已核实（README "Fast and exact" 一节）。
- **关键澄清：Ollaya 自己只发布约 3MB 的小 ONNX 计算图，不托管权重本体**——真实权重文件（通常是 `model.safetensors`）仍从原作者 HuggingFace 仓库拉取，按 commit 锁定并 sha256 校验。即"ONNX 图大小"和"真实权重大小"是两个数字，前者极小，后者才是真实下载体量。已核实（README "Weights come from their authors" 原文）。

### 逐模型核查（README 原文 + 外部交叉验证）

| 模型 | 来源/作者 | 许可证 | 基座 | 规模 | 中文支持 | 延迟（README 自报，RTX 4090/CPU） | 成熟度 |
|---|---|---|---|---|---|---|---|
| `laya` | Convai Innovations | 未注明具体许可（Ollaya 整体 Apache-2.0，各模型"保留自己许可证"） | `laya:en` = ModernBERT-large 421M；`laya:multilingual` = mmBERT-base 322M | 路由器，按语言分发 | **multilingual 版明确支持 100+ 语言**，README 未逐一列出但 mmBERT 系基座本身覆盖中文（已核实 mmBERT 多语言训练语料含中文为业界常识，Ollaya 文档未单独验证中文效果，**部分未核实**） | en 版：GPU 上 8–10ms/5 问 | 新项目子模块，无独立 GitHub repo，**未核实**独立成熟度 |
| `decider`/`:4b`/`:0.8b`/`:2b-vision` | Mapika | 未单独注明 | Qwen3.5 解码器 | 2B(默认)/4B/0.8B，另有 2B 视觉版 | 继承 Qwen3.5 的多语言能力（含中文），**未见专项评测**，未核实 | 4B: 0.680 准确率；2B: 0.591（README 自评） | 未核实独立项目成熟度 |
| `kev`/`:0.8b`/`:9b` | Jared Palmer（见上） | Apache-2.0（Kev 本体） | Qwen3.5（LoRA + pointer head） | 4B 默认/0.8B/9B | 同 Kev 本体，未核实 | kev:4b 0.669，kev:9b 0.722（README 自评） | 见上 Kev 条目：8565 stars，已核实 |
| `decision` | vLLM Semantic Router 项目贡献者 | 未单独注明 | Qwen3.5-0.8B + endpoint head | 0.8B，16k token | 未核实 | 未给出单独延迟 | 未核实 |
| `qwen3guard` | **Qwen 团队官方**（Alibaba） | Apache-2.0（技术报告原文确认，见下方第 3 部分安全模型小节） | Qwen3Guard-Gen-0.6B | 0.6B（Ollaya 内置版） | **官方确认支持 119 种语言及方言，含中英文**，已核实（见下方 Qwen3Guard 条目） | 未在此表给出，但 Qwen3Guard 官方自评"低延迟" | Qwen3Guard 本体：518 stars（GitHub），2025-09 发布，有正式 arXiv 技术报告，**成熟度最高** |
| `nli`/`:modernbert-large` | Moritz Laurer | `nli:deberta-v3-large` = MIT；`nli:modernbert-large` = Apache-2.0 | DeBERTa-v3-large / ModernBERT-large | 分别对应两种基座规模 | DeBERTa-v3-large 版即 mDeBERTa 体系，**明确支持中文**（见下方第3部分 NLI 小节），已核实 | 编码器类，README 汇总"7–35ms GPU / 0.15–2.4s CPU" | 见下方 MoritzLaurer 模型条目，HF 下载量大，社区成熟度高 |
| `gliclass` | Knowledgator | Apache-2.0 | DeBERTa-v3-large | base/large/multilang 等多档 | **有专门 `gliclass-multilang-mini`，原生训练 20 种语言含中文**，已核实（HF 模型卡 + WebSearch） | 同上编码器区间 | GitHub 553 stars，持续更新到 2026-10-03，已核实 |
| `von` | Victor Hugo Panisa | 单独许可（README 未细标，疑似随 Ollaya Apache-2.0） | ModernBERT-large | 8k token 上下文 | 未核实 | 编码器区间 | 未核实，无法定位独立 GitHub 仓库 |
| `winnow`/`:e4b` | EldanRing | 基座 Gemma 4（Google DeepMind，Gemma Terms of Use，非纯开源许可） | Gemma 4 微调 | 12B（本体）/E4B（轻量版） | 未核实 | **README 重点数据**：winnow:e4b 在 RTX 4090 上 5 问 89ms，typed-decisions 准确率 0.722（对比 Jev 0.738，几乎打平） | README 标为"Recommended"（推荐默认模型），但无法核实独立仓库 star 数 |
| `jevk5` | alibiserikbay | 未单独注明 | Qwen3.5-4B 微调，作者发布 GGUF | 4B，最多 16 个选项 | 继承 Qwen3.5，未核实中文专项 | 走 llama.cpp，未给单独数字 | 未核实 |
| `clm` | Contrastive-LM | 未单独注明 | Qwen3-8B 编码器 + 投影头 | 8B | 未核实 | typed-decisions 仅 0.357（README 自评，明显偏低） | 未核实 |

- **中文支持总体结论**：Ollaya 模型矩阵里**明确、有据可查的中文支持**集中在三类：① `laya:multilingual`（mmBERT-base，宣称 100+ 语言）；② `gliclass` 的 `multilang` 变体（官方训练语料含中文，20 语言）；③ `qwen3guard`（Qwen 官方技术报告明确 119 语言含中文，这是**证据最硬**的一条）；④ `nli:deberta-v3-large`（继承 mDeBERTa/XNLI 体系，覆盖中文）。其余多数基于 Qwen3.5 系解码器的模型（decider/kev/jevk5/jeb）**理论上**继承 Qwen 系列的中文能力，但 Ollaya/各作者文档**都没有专门给出中文基准分数**，这是整个生态目前最大的证据缺口。
- **Ollama 的竞品动态**：README 提到"Ollama 0.35 也开始原生支持 decision models（Nimble 和 Tev1），通过同样的 TypeSafe 协议"，说明这个品类正在被主流运行时（Ollama）吸收，不是孤立实验。已核实（README 原文）。
- **Ollaya 自己的基准方法论**：在自建 GPU/CPU 上跑分，公开原始数据于 ollaya.dev/results，并做"与作者原始代码逐题对齐（parity check）"，这点方法论比较严谨，但**数据本身是厂商自产**，没有第三方复现。弱已核实。
- **补充：README 中仅两个模型给出了明确的 ECE（Expected Calibration Error）数值**，对"校准优于 prompt"这一核心命题是最直接的量化证据，单独列出（已核实，来自 README "Models" 表原文）：
  - `jeeves`（PostHog，Qwen3.5-9B LoRA merge）：typed-decisions 准确率 0.680，**ECE 0.031**，RTX 4090 上 5 问 838ms。
  - `clef`（Cloudflare，Qwen3.5-9B 全量后训练+联合 schema head）：typed-decisions 准确率 0.703，**ECE 0.020（且无需拟合温度）**，RTX 4090 上 5 问 532ms。
  - 两者对比：`clef` 在准确率更高、延迟更低的同时 ECE 更小，说明"全量后训练 + 联合 schema head"这种架构可能比"LoRA + pointer head"在校准上更占优，但样本量仅 2 个模型，**不足以下架构性结论，仅作为具体数据点记录**。

---

## 5. 成熟本地决策/分类替代方案（含中文能力与许可证）

### GLiClass / GLiNER 家族
- GLiClass（Knowledgator）：zero-shot 序列分类，双向 encoder（BERT 类），base v1.0 约 **0.2B 参数**，有 small/base/large/edge/多语言档位；最新 v3 用 LoRA 做逻辑推理任务，速度比 cross-encoder 快 8–10 倍。已核实（HF 模型卡 + arXiv [2508.07662](https://arxiv.org/html/2508.07662v1)）。
- **`gliclass-multilang-mini` 原生训练 20 种语言（含中文、西语、阿语、印地语等）**，标签和文本可以是不同语言。已核实（HF 模型卡 + WebSearch 聚合）。
- GitHub（knowledgator/GLiClass）：**553 stars，Apache-2.0，最后 push 2026-10-03**，持续维护。已核实（gh api）。
- GLiNER（urchade/GLiNER，GLiClass 的姊妹 NER 项目）：**4049 stars，Apache-2.0，最后 push 2026-10-05**，是这个方向里最成熟、最多引用的项目。已核实（gh api）。

### Zero-shot NLI
- **mDeBERTa-v3-base-mnli-xnli**（MoritzLaurer）：在 XNLI（15 语言）+ 英文 MNLI 上微调，可对 **100 种语言**做 NLI/zero-shot 分类，**明确含中文**，且对未见过语言也有跨语言迁移能力。已核实（HF 模型卡聚合）。
- 另一版本 `mDeBERTa-v3-base-xnli-multilingual-nli-2mil7` 训练数据扩展到 27 种语言、270 万句对。已核实。
- bge-reranker 作为"相关性/entailment 判据"使用是社区常见 trick（本质是复用 reranker 的 cross-encoder 打分做二分类），**本次未找到权威基准论文直接验证这一具体用法的校准质量**，未核实。
- **Erlangshen 中文 NLI 系列**（IDEA-CCNL / Fengshenbang-LM）：`Erlangshen-Roberta-330M-NLI` 基于 Chinese RoBERTa-wwm-ext-large，在 4 个中文 NLI 数据集（合计 101万+ 样本）上微调，CMNLI 82.25%、OCNLI 79.82%、SNLI 88%。是**专门为中文设计**的 NLI 方案，而非"多语言模型里带中文"。已核实（HF 模型卡聚合）。

### SetFit（少样本分类）
- huggingface + Intel Labs 联合出品，**无需 prompt/verbalizer**，两阶段：对比学习微调 sentence-transformer → 在其 embedding 上拟合逻辑回归头。**每类仅需 8 个标注样本**即可获得较高准确率；支持多标签分类和多语言（取决于底层 sentence-transformer 选型，若选多语言 embedding 模型即获得中文能力）。已核实（WebSearch 聚合，源自官方文档与 Argilla 教程）。GitHub（huggingface/setfit）：**2833 stars，Apache-2.0，最后 push 2026-10-06**，持续维护。已核实（gh api）。
- 对 Agent24 场景的意义：如果愿意标注几十条"是否记住指令"的正负样本，SetFit 是**成本最低的定制分类器路线**，且可选中文多语言 sentence-transformer 底座。

### Embedding + 逻辑回归头
- **BAAI/bge-m3**：MIT 许可，569M 参数，模型体积 ~2.27GB，支持 100+ 语言（含中、日、韩、阿拉伯语等），同时具备稠密/稀疏/多向量三种检索模式，最大 8192 token。已核实（HF 模型卡聚合）。是在此基座上接逻辑回归/MLP 头做分类的常见选型，中文能力有直接证据。
- Alibaba-NLP/gte-multilingual-base：本次**未单独核实**其具体参数量/许可证细节（时间所限），但该系列同样主打多语言含中文，与 bge-m3 定位相近——**此条目标记未核实，建议后续补查**。

### Semantic Router（aurelio-labs）
- 开源 Python 库，核心思路：**不用 LLM 实时分类，而是预编码每个 intent 的示例短语，运行时按最近邻（向量相似度）路由**，速度远快于调用 LLM。已核实（GitHub README 聚合）。
- 支持完全本地化：`HuggingFaceEncoder` + 本地 LLM（如 LlamaCppLLM）；文档示例里出现过中文路由短语（"枭起青壤 迪丽热巴陈哲远"等），且官方建议搭配 `intfloat/multilingual-e5-base` 这种明确支持中文的多语言 embedding 模型来获得中文能力。已核实（WebSearch 聚合 + GitHub README）。
- GitHub：**3938 stars，MIT 许可，最后 push 2026-09-29**，成熟且活跃。已核实（gh api）。
- 架构相关性：这正是"embedding 近邻 + 阈值"路线的代表性成熟项目，可以直接类比用于 Agent24 的"是否需要记忆召回"等路由判断。

### RouteLLM（lm-sys）
- 定位是 LLM 强弱模型路由（省成本），不是"是否执行某动作"的决策分类器，但其训练方法（基于偏好数据学习路由器，matrix factorization router 被推荐为默认）对"如何训练一个轻量路由分类器"有参考价值。已核实（GitHub + arXiv [2406.18665](https://arxiv.org/html/2406.18665v3)）。
- GitHub：**5571 stars，Apache-2.0**，但**最后 push 是 2024-08-10**，已经两年多没更新——**项目基本停滞**，这点需要明确标注。已核实（gh api，`pushed_at: 2024-08-10`，而当前日期 2026-10-06，说明已停更逾两年）。

### Guard/安全风险分类模型
- **Qwen3Guard**（Qwen 团队官方）：2025-09-17 发布的多语言安全护栏模型系列，有 Generative（safe/controversial/unsafe 三分类 + 细分类别）和 Stream（流式 token 级分类头，用于生成过程中实时监控）两种变体，各 0.6B/4B/8B 三档，**官方明确支持 119 种语言和方言，在中英文及多语言基准上都是 SOTA**。这是本次调研中**中文支持证据最硬**的一个模型。已核实（[GitHub QwenLM/Qwen3Guard](https://github.com/QwenLM/Qwen3Guard) + [arXiv 2510.14276 技术报告](https://arxiv.org/pdf/2510.14276)，本人直接 WebFetch 了摘要复核）。GitHub 元数据：**518 stars，最后 push 2025-10-21**。许可证：GitHub `license` API 字段为空、仓库根目录直接 `curl` LICENSE 返回 404，但**技术报告摘要原文明确写"released under the Apache 2.0 license"**，已核实（以技术报告原文为准；建议接入前仍去实际权重/模型卡二次确认，不要只信 GitHub 元数据字段）。
- **Llama Guard 4**（Meta）：从 Llama 4 Scout 裁剪蒸馏而来的 12B 密集模型，专精内容安全分类，许可证是 **Llama 4 Community License Agreement**（非标准 OSI 开源协议，有使用条款限制）。官方评测列出的"非英语支持语言"为法、德、印地、意、葡、西、泰 **7 种，不含中文**（Llama Guard 3 的评测范围），Llama Guard 4 是否专门测过中文**未见明确数据，未核实**。已核实许可证与基座信息（HF 模型卡）。
- **ShieldGemma**（Google）：基于 Gemma，4B 参数，开放权重，支持自定义政策微调。中文支持**未核实**（搜索结果未给出具体语言列表）。
- 结论：**如果 Agent24 需要"风险分级"这一具体决策类型，Qwen3Guard 是目前唯一有官方中文实测数据支撑、体积也最小（0.6B 起）的现成选项**，优先级应高于 Llama Guard / ShieldGemma。

### 小 LLM + 约束解码 + logprob 作为校准分类器
- 做法：用 Qwen3-0.6B/1.7B 通过 llama.cpp 的 **GBNF 语法**或 MLX 的语法约束解码，强制输出限定在候选标签集合内，再读取被选 token 的 logprob 作为置信度信号。已核实（llama.cpp 侧：GBNF 是成熟功能，可构造"只允许若干标签之一"的语法规则，生成裸标签文本便于 logprob 还原；需要编译时开启 logprob 支持）。MLX 侧：2026 年已有"并行约束解码"工作（基于 mlx-lm + EBNF 语法），在 M4 Max 上对 1.5B 量化模型实现 5.6–7.0 倍于自回归解码的加速，**但这是生成 JSON schema 的工作，不是专门针对"读取 logprob 做校准置信度"的论文**，两者机制相关但验证目标不同，需要谨慎区分。已核实（[MindStudio 博客](https://www.mindstudio.ai/blog/parallel-constrained-decoding-mlx)）。
- 性能数据（MLX，Apple Silicon）：搜索结果中数字口径混杂（如"Qwen3-0.6B embedding 吞吐 44K tok/s"是 embedding 场景，不是生成场景；"M4 Max 上 Qwen3-14B 38 tok/s"是生成场景），**没有找到 Qwen3-0.6B/1.7B 在纯 CPU/ANE 上做"5 道分类题各几十 ms"这类直接可比的基准**，这部分需要 Agent24 自己实测，未核实。
- 关键结论（间接证据）：Ollaya README 给出的"编码器类模型（laya/nli/gliclass/von/qwen3guard/decima）在 GPU 上 7–35ms、CPU 上 0.15–2.4s；解码器类模型（decider/kev/jevk5 等）在 GPU 上 0.1–0.9s、CPU 上 1.3–25s"——**这组自家实测数据本身就直接证明：同等任务下专用小编码器比"小 LLM 当分类器"快一个数量级以上（CPU 上差 10–100 倍）**，这是本次调研里对"校准模型 vs 小 LLM-as-classifier"最具体、最可信的性能对比来源。已核实（Ollaya README "Speed" 小节原文数字）。

---

## 6. 校准证据（论文/基准，2024–2026）

- **Verbalized confidence（LLM 自己说"我有 80% 把握"）vs logprob-based confidence**：论文 *"Rethinking Verbalized Confidence for LLM-as-a-Judge: A Compatibility Shift on Post-2025 Proprietary Models"*（作者 Yu-Chung Hsiao，[arXiv 2609.10996](https://arxiv.org/pdf/2609.10996)，本人直接 WebFetch 复核）核心结论：**在 SummEval/AggreFact/HelpSteer2 等基准上，post-2025 闭源模型（如 GPT-5/o1 类）用 verbalized confidence 的效果反超 logprob 方案**，"偏好 logprob"这条业界常识不再对新模型成立。已核实（arXiv 摘要原文）。**需要纠正一点**：摘要本身**没有**明确把原因归为"logprob 访问受限/不稳定"——这是对"为什么会反转"的合理推测，但论文给出的是经验结果（在这些基准上 verbalized 更好），不是对访问限制的因果论证，**这一归因未核实，仅标为推测**。结论本身（verbalized 在新模型上更优）已核实，但别把"为什么"也当成论文证实的事实。
- **ConfidenceBench**（[arXiv 2607.20526](https://arxiv.org/html/2607.20526)）：用 Brier score 评测 15 个前沿 LLM 的 verbalized confidence，属于本话题较新的标准化基准。已核实（标题+摘要聚合，**未深入读取具体数值结论，弱已核实**）。
- **ICLR 2024 早期工作**（[arXiv 2306.13063](https://arxiv.org/pdf/2306.13063)）：在 5 类数据集、5 个常用 LLM 上系统比较校准/失败预测方法，结论是**LLM 做 verbalized confidence 时普遍"过度自信"（overconfident）**。已核实（摘要聚合）。
- **失败模式证据**：
  - 分布偏移（distribution shift）：*"Auditing Cross-Domain Recalibration of LLM Judges"*（[arXiv 2609.27954](https://arxiv.org/pdf/2609.27954)）明确指出跨域时准确率/校准会同时下滑，且"准确率差距"本身不足以证明已重新校准。已核实（标题+摘要）。
  - 对抗性措辞：fact-checking 领域工作显示，把陈述改写成更口语化的表达会系统性影响分类结果（"preserve factual content while shifting surface form toward informal language"导致判断变化）；另有研究指出"训练出来的 MLP 分类器在对抗性改写下准确率骤降，而 training-free 方法更鲁棒"。已核实（WebSearch 聚合多篇 arXiv 摘要）。
  - **中文口语/短文本场景专项证据**：本次**未找到**直接针对"中文口语+短指令"的校准失效专项论文或基准，这是一个明确的证据空白——Agent24 如果要验证"记住没有/你帮我这种短句会不会被误判"，**很可能需要自建中文评测集**，不能依赖现成论文结论。未核实（明确标注为空白，而非遗漏）。

---

## 7. Rust 中嵌入小模型的运行时选型

| 运行时 | CoreML/Metal 支持 | 成熟度 | 冷启动/内存 | 二进制体积影响 |
|---|---|---|---|---|
| **ort**（ONNX Runtime 绑定） | 支持 CoreML EP（Apple Silicon），`v2.0.0-rc.13`（2026-07-28）起把 EP 结构体放到 Cargo feature 后面编译期裁剪 | 成熟，社区广泛使用；已核实（docs.rs + WebSearch） | 内置完整 ONNX Runtime C++ 库，启动需加载共享库，中大型模型冷启动在百毫秒级（具体数字因模型而异，**本次未找到 Agent24 场景下的精确基准，未核实**） | 会打包较大的 ONNX Runtime 动态库，体积明显大于纯 Rust 方案 |
| **candle**（huggingface） | 原生支持 Metal（Apple GPU），**纯 Rust 无 Python 依赖** | 很成熟：**21142 stars，Apache-2.0，最后 push 2026-10-05**，已核实（gh api） | 编译为单一二进制，**启动在毫秒级**；示例体积：Whisper tiny on M2 Air 二进制 22MB，LLaMA2-7B q4k 48MB，Phi-2 2.7B q4k 38MB | 体积小，适合 serverless/嵌入式场景 |
| **tract**（sonos） | 支持跑在 Apple GPU 上（文档提及"from embedded ARM CPUs to NVIDIA/Apple GPUs"），**纯 Rust，无外部 C++ 依赖** | 生产级：Sonos 自家唤醒词/流式语音识别在用；**3080 stars**，license 字段 GitHub API 显示 NOASSERTION（需去 LICENSE 文件核实具体协议文本，**许可证细节未完全核实**），最后 push 就在今天 2026-10-06 | 可静态编译进单一小二进制，**没有外部共享库依赖**，冷启动应优于 ort | **包大小 33.7 KiB 级别**（crate 本身），远小于绑定完整 ONNX Runtime 的 ort；是三者中"纯嵌入式"属性最强的 |
| **llama.cpp**（通过 Rust 绑定或直接起子进程）/ **MLX** | llama.cpp 原生支持 Metal；MLX 是 Apple 官方框架，**以 Python/Swift 为主，没有成熟的原生 Rust 绑定**，Rust 守护进程若要用 MLX 基本只能 shell 出一个 Python/CLI 子进程 | llama.cpp 生态成熟；MLX 社区活跃但面向 Python/Swift，Rust 集成是弱项 | 子进程方案会引入**进程间通信开销 + Python 解释器冷启动**（通常几百毫秒级），不适合"sub-100ms 短文本分类"这种硬指标场景 | 如果把 llama.cpp 编译进 Rust 二进制（如 llama-cpp-rs），体积取决于量化模型大小，通常远大于 candle/tract 跑的小编码器方案 |

- **针对 Agent24（Rust daemon + Electron，需要 sub-100ms 短文本分类）的结论**：优先级建议 **tract 或 candle > ort > llama.cpp 子进程 > MLX 子进程**。前两者都是纯 Rust、无外部运行时依赖、冷启动快、体积小，适合跑 laya/gliclass/nli 这类小型 encoder 分类器；ort 功能更全（CoreML EP 调度更智能）但体积和依赖更重；凡是需要"调用 MLX"的路线，都意味着要在 Rust daemon 里管理一个外部 Python/MLX 子进程，这与"daemon 轻量、sub-100ms"的目标有直接冲突，**除非把分类任务完全下沉到已有的 oMLX 常驻进程里复用，而不是让 Rust daemon 自己起新进程**。

---

## 8. 混合系统（规则 + 分类器 + 弃权）最佳实践

- **三层级联模式是内容审核/风控行业的标准做法**，有据可查：规则层做确定性高精度拦截（已知违规模式），分类器处理中间地带并输出违规概率，低/中置信度路由给人工复核，复核结果再回流成训练数据。已核实（WebSearch 聚合多篇内容审核系统设计文章，包括 [techinterview.org 系统设计案例](https://www.techinterview.org/post/3233474439/)）。
- **阈值并非按类别统一设置**：例如 CSAM 类别用低阈值（先下架后复核），政治言论类用高阈值（先复核后处理）——即**阈值选择是业务成本驱动的，不是统一的 0.5 或统一的 Youden's J**。已核实。
- **阈值选择方法论**：Youden's J（敏感度+特异度−1）是 ROC 上默认的等成本切点，但**它假设假阳性和假阴性代价相等，这个假设在大多数真实业务里不成立**；当代价或先验不对称时应改用成本加权阈值、F-beta 优化，或在类别不均衡时改看 PR 曲线而不是 ROC 曲线。已核实（WebSearch 聚合，含 [CASRAI 方法说明](https://casrai.org/guides/youdens-j-index-roc-threshold-selection)）。对 Agent24 的"是否要记住/是否要调用高风险工具"这类决策，误判代价显然不对称（漏记一条指令 vs 误删数据的代价天差地别），**不能直接套用默认 0.5 阈值**。
- **持续评测集（golden set）工具链**：LLM-ops 领域已有成熟分工——DeepEval 走 pytest 风格、可接入 CI 挡住回归发布；Braintrust 主打"数据集版本管理 + 实验对比 + 从生产日志回灌样本"，是golden set 管理最成熟的；LangSmith 则是"全链路 trace → 数据集 → 评测"一体化。已核实（WebSearch 聚合多篇 2026 年工具对比文章）。Agent24 若要给"记住判断/记忆召回判断"建立持续回归集，Braintrust 的数据集管理模式最贴近需求。
- **主动学习（active learning）日志闭环**：成熟做法是在推理管道里记录每条预测的置信度，低置信度样本自动进复核队列，人工标注后批量回灌重新训练；触发重训练的时机可以是"达到标注批量阈值"或"检测到 drift"。已核实（WebSearch 聚合，含 [Roboflow Active Learning 文档](https://docs.roboflow.com/deployment/monitoring-and-analytics/active-learning)、[Label Studio 文档](https://docs.humansignal.com/guide/active_learning.html)）。这与 Agent24 现有"PR 复审/判新"等人工回路在方法论上是同构的。
- **side-effecting/不可逆操作的人机确认**：2025–2026 年行业共识是"同步审批（synchronous approval）"——金额超阈值、账户变更、删除数据等不可逆操作必须暂停等人工签字；更细的治理原则是"按单个动作的可逆性/影响面/敏感度决定要不要进 human-in-the-loop"，且允许 agent 通过持续低错误率（常见基准线 ~5% 错误率以下）**逐个动作"毕业"**，从"in the loop"过渡到"on the loop"（抽查而非逐次审批）。已核实（WebSearch 聚合，含 [Galileo 博客](https://galileo.ai/blog/human-in-the-loop-agent-oversight)、[Anthropic "Measuring AI agent autonomy in practice"](https://www.anthropic.com/research/measuring-agent-autonomy)）。
- **2026 年新出现的反向风险**：有工作指出"保留人工确认"这个经典防线在 2026 年被攻破过——根因不是"有没有人在场"，而是**人工看到的确认文案本身可能是被攻击者操纵过的 LLM 生成文本或页面可控标签**，即确认环节呈现的信息源不可信，会让"人工确认"形同虚设。已核实（WebSearch 聚合摘要，具体来源文章标题未逐一核实，**建议视为需要后续深挖的强信号，弱已核实**）。对 Agent24 而言，这提示：如果将来做"risk of a tool call"这类确认 UI，确认文案的生成链路本身也要纳入信任边界设计，不能假设"有确认框=安全"。

---

## 9. 综合对比表

| 候选 | 类型 | 中文 | 许可证 | 体积 | 本地延迟（CPU，5问/单题量级） | 校准 | 成熟度 | 适合 Agent24 的场景 |
|---|---|---|---|---|---|---|---|---|
| **TypeSafe Jev（托管API）** | 托管决策 API | 未见专项中文评测 | 闭源，API-only | 不适用 | 需联网，非本地 | 厂商自评 ECE 0.03–0.28（因任务而异） | 新但有独立第三方网站背书 | 不符合 Agent24"local-first"定位，仅作对标基准 |
| **Kev（0.8B/4B/9B/27B）** | 本地决策 LoRA/全量模型 | 未核实专项中文分数（继承Qwen） | Apache-2.0 | 0.8B 最小，27B 达 51GB 全量 | README 自评 0.1–0.9s/GPU，CPU 慢一个数量级 | 厂商自评 Brier 0.156–0.481，27B 接近 Jev | 8565 ★，当天仍在更新，生态最活跃 | 若要"开箱即用的 Jev 平替+可自训练"，Kev-4B/0.8B 是第一候选，但需自补中文评测 |
| **Ollaya + laya/gliclass/qwen3guard 等** | 本地决策模型运行时（聚合多模型） | **三个子模型有硬证据**（qwen3guard/gliclass-multilang/nli deberta） | 整体 Apache-2.0，各子模型各自许可 | ONNX图仅3MB，真实权重另算（百MB–GB级不等） | 编码器类 GPU 7–35ms / CPU 0.15–2.4s（README自评） | 多数子模型未独立核实，仅 qwen3guard/winnow 等有数据点 | 仅3周历史，1216★，**生产可用性需谨慎** | 适合"快速试跑多种现成小分类器"的实验阶段，不建议直接上生产 |
| **GLiClass（multilang-mini等）** | zero-shot 分类 encoder | **明确支持20语言含中文** | Apache-2.0 | 0.2B 起，small/base/large多档 | 编码器级，通常几十ms（CPU） | 未见独立校准基准，多为准确率指标 | 553★，持续更新 | **风险分级/标签分类**的稳妥选择，有中文证据 |
| **mDeBERTa-v3-xnli / Erlangshen中文NLI** | zero-shot NLI | **mDeBERTa覆盖100语言含中文；Erlangshen专为中文** | MIT / Apache-2.0（各版本不同，需逐一核实） | mDeBERTa-base级（几百MB） | 编码器级，CPU可用 | 未见统一ECE数据，但准确率有公开基准（CMNLI 82%等） | 社区下载量大，成熟稳定 | **"是否相关/是否需要召回"这类entailment式判断**的首选，尤其Erlangshen对中文更贴合 |
| **SetFit 自训练分类器** | 少样本微调框架 | 取决于底层embedding选型（可选中文/多语言） | Apache-2.0 | 取决于底层 sentence-transformer（通常百MB级） | 训练后推理为embedding+LR，CPU上应是毫秒级 | 需自评，无统一基准 | 2833★，持续维护 | **成本最低的定制化路线**：几十条标注样本训"是否是记住指令"分类器 |
| **bge-m3 + 逻辑回归头** | embedding+分类头 | **100+语言含中文，MIT** | MIT | 569M，~2.27GB | embedding提取后分类头极快，瓶颈在embedding前向 | 需自评 | 社区成熟，FlagEmbedding官方维护 | 需要**语义检索+分类共用底座**时的统一方案 |
| **Semantic Router** | embedding近邻路由 | 搭配中文多语言embedding可用，有中文示例 | MIT | 取决于embedding模型 | 近邻查找极快，瓶颈同样在embedding | 本质无"校准概率"输出，是相似度阈值而非概率 | 3938★，活跃 | **意图路由/工具选择**场景，可平替当前的关键词规则 |
| **Qwen3Guard** | 安全/风险分类 | **官方119语言，含中英文SOTA实测** | Apache-2.0（技术报告确认） | 0.6B/4B/8B三档，0.6B最小 | 官方自评低延迟，具体数字未核实 | 有正式arXiv技术报告支撑 | 518★，官方维护，2025-09发布 | **"tool call风险等级"判断的最佳现成选项**，中文证据最硬 |
| **RouteLLM** | 强弱模型路由 | 未核实 | Apache-2.0 | 依赖所路由的模型 | 不适用（路由到云端模型） | 有专门训练方法但**已停更2年+** | 5571★但停滞 | 不建议采用，架构思路可参考，代码本体过时 |
| **小LLM(Qwen3-0.6B/1.7B)+约束解码+logprob** | 通用LLM充当分类器 | 继承Qwen中文能力 | Apache-2.0（Qwen3系列） | 0.6B/1.7B，量化后几百MB | **明显慢于专用encoder**（Ollaya数据显示解码器类比编码器类慢一个数量级以上） | logprob校准质量**行业正在从"偏好logprob"转向"偏好verbalized"**，无定论 | 工具链（llama.cpp GBNF/MLX约束解码）成熟 | 仅在"没有现成专用分类器覆盖的新颗粒度任务"时作为兜底，不应作为默认路线 |
| **ort / candle / tract（Rust运行时）** | 推理运行时 | 不涉及 | 均为Apache-2.0级开源 | tract最小(33.7KiB级)，candle次之，ort最重 | candle/tract冷启动毫秒级，ort因绑定完整ONNXRuntime略重 | 不涉及 | candle 21142★/tract 3080★，均生产级 | **把上述任一小encoder模型嵌进Rust daemon的底层执行引擎**，候选顺序 tract/candle > ort |

---

## 10. 推荐组合（基于以上证据）

对 Agent24 当前"记住判断 / 记忆召回判断 / 工具调用风险 / 隐私分级路由"这几类具体决策，**不建议直接接入 TypeSafe Jev（闭源托管，不符合 local-first）**，也**不建议直接用 Kev-27B 这种 51GB 全量权重**（体积与 Agent24"本地 oMLX/Ollama on Apple Silicon"的轻量化前提冲突）。基于证据强度，推荐一个**三层组合**：

1. **规则层（高精度地板）保持不变**：现有正则/关键词规则继续作为"一眼就能判定"的高置信场景的拦截层，这是内容审核行业验证过的标准第一层，不建议因为引入模型就整体推翻。
2. **决策层换成"小型专用分类器组合"而非通用 prompt**：
   - "是否是记住指令 / 是否需要记忆召回"这类**语义相关性判断**，优先用 **mDeBERTa-v3-xnli 或 Erlangshen 中文 NLI**（中文证据扎实，entailment 式打分天然适合"这句话算不算 X"的判断），或用 **SetFit** 在几十条标注样本上自训练一个专属头（成本最低，可完全离线迭代）。
   - "工具调用风险等级/隐私分级"这类**安全分类**，优先用 **Qwen3Guard-0.6B**（官方中文实测数据最硬，体积最小，且有正式技术报告背书），而不是重新发明一个 prompt-based 风险判断。
   - "意图路由/任务分发"可以借鉴 **Semantic Router 的"embedding近邻+阈值"架构**（哪怕不直接用这个 Python 库，这个设计模式本身就值得照搬到 Rust daemon 里）。
3. **Rust 运行时用 tract 或 candle 嵌入这些小 encoder 模型**，而不是为了"校准"去起一个 MLX/llama.cpp 子进程跑小 LLM——Ollaya 自己的实测数据已经证明专用 encoder 比"小 LLM 当分类器"快一个数量级以上，这条证据直接支持 owner 的原始直觉："校准过的小分类器优于用 LLM/prompt 现场判断"，但**要落地到中文场景，必须补一步自建中文评测集**（本次调研明确发现这是全行业的证据空白，没有现成论文可以照搬）。
4. **弃权带与阈值不要用统一的 0.5 或默认 Youden's J**：按具体决策的误判代价不对称性分别设阈值（例如"误记住一条无关指令"代价远低于"该记住的没记住"，阈值应该偏宽松；"工具风险误判为安全"代价远高于"误判为危险转人工"，阈值应该偏严格），并把低置信度/被弃权的样本**接入现有的 PR 复审/人工回路做主动学习闭环**，定期回灌更新规则和分类器——这是 Agent24 现有工作流（人工复核 PR/评审）天然契合的扩展点，不需要新建基础设施。
5. 任何"side-effecting 工具调用"的最终确认，仍应保留人工确认环节，但**要警惕 2026 年已有的"确认文案可被操纵"风险**——确认 UI 展示的内容本身要走可信渲染路径，不能直接信任 LLM 生成的描述文本。

### 证据缺口（诚实列出，供下一步调研）
- Jev 的 RLCD 论文原文、ECE 具体数值表格：**未直接读到 TypeSafe 官方技术报告**，目前引用的都是二级博客转述。
- jev-skill 的 no-silent-fallback 规则**确切文件路径**未核实。
- Kev/Ollaya 各子模型的**中文专项基准**几乎全部缺失，这是本报告最大的"未核实"集中区。
- 小 LLM（Qwen3-0.6B/1.7B）在 **Apple Silicon CPU/ANE 上针对"几十 token 短文本分类"任务**的直接延迟基准未找到，需要 Agent24 自己拿真实场景数据实测。
- Qwen3Guard 许可证已通过技术报告原文确认为 Apache-2.0（2026-10-06 复核补充），但 GitHub 仓库本身没有可解析的 LICENSE 文件，接入前仍建议以实际权重/模型卡页面为准做二次确认。
