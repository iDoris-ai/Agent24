# 02 Jev 类决策模型生态调研（2026-10-07）

> 依据：jason 博客（blog.mushroom.cv，本地源码目录 `mycelium/blog/src/content/blog/`）中 14 篇相关拆解文章的全文，以及本地搜索接口 `127.0.0.1:8888/api/search` 的补充查询。
> 各篇的完整文件名、标题与线上地址（`https://blog.mushroom.cv/blog/<文件名去掉 .md>`）见 [`sources.md`](sources.md) §3。
> 每条结论都标了出处文件名。**文章里没有的数字一律不写**；成绩注明来源：「自报」= 项目方自己公布，「实测」= 文章作者或社区复测。

## 一句话结论

除了两处例外，没有任何一个 Jev 类候选给出过**中文分类 / 意图识别的准确率**：
- KaLM-Jev 有一个含中文的 MIRACL 检索分数，但评的是检索，不是分类；
- Ollaya 给出了 Laya「中文约 8ms」的延迟，但只是延迟，不是准确率。

训练和评测数据几乎都是英文（Banking77、AG News、MNLI、BoolQ、SST-5、WANLI 等）。**任何候选进入产品之前，都必须先过我们自建的中英泰评测集。**

## 候选一览

| 候选 | 出品方 | 规模 | 运行时 | 许可证（文章怎么说） | 成绩（自报 / 实测） | 本地权重 | 16GB | 8GB | 能否从 Rust 调用 | 出处 |
|---|---|---|---|---|---|---|---|---|---|---|
| TypeSafe **Jev**（原版） | TypeSafe AI | 未披露 | 云端 API | 商业闭源 | 自报快 193.6 倍 / 便宜 444.6 倍；Every.to 独立测试约 25 倍 / 580 倍；工作流准确率 67.8%，对手 74.1%（自己披露） | 否 | — | — | 只能走 HTTP | typesafe-ai-jev-system-one-model-rlcd-decision-ai-enterprise.md |
| **Kev** | Jared Palmer | 0.5B / 0.6B / 4B / 8B（Qwen 底座） | CUDA / Apple Silicon | Apache-2.0 | kev-4b 分布外 0.790，Jev 0.857（自评，坦诚披露）；对选项顺序敏感 7.4% | 是 | 小尺寸可以 | 0.5B / 0.6B 应可行 | 走 REST | kev-jaredpalmer-…md |
| **AgentJev-0.6B** | aimeigaoshou | Qwen3-0.6B + 决策头 | HF Transformers | 「尚不明确」 | Typed Decisions 2000：79.25%（自报，英文） | 是 | 可以 | 应可行 | 走 HTTP | agentjev-0-6b-…md |
| **JevEmbed** | 哈工大深圳 HITsz-TMG | 可换 embedding 底座（KaLM-v2.5 / Qwen3-Embedding-0.6B/4B/8B / mE5-large） | CPU / CUDA | 文章未注明 | LoRA 后：KaLM 30.24% → 76.68%，Qwen3-Emb-0.6B 30.73% → 84.06%（自报，Open-Jev 子集） | 是 | 可以 | 可以 | 走 HTTP | jevembed-…md |
| **LLM2Jev** | Yinsongxu | 任意 HF 因果 LLM | SGLang（只支持 Linux+CUDA）/ Transformers | Apache-2.0 | **自己没有准确率基准**；staged 模式长上下文加速 4.9 倍（实测） | 取决于所带模型 | 可以 | 可以 | Python；读 logit 的核心逻辑可以用 Rust 重写 | llm2jev-…md |
| **KaLM-Jev Nano** | KaLM 团队（HIT-TMG） | 0.27B | CUDA / CPU | **未声明** | MIRACL 18 语言（含中文）62.08 nDCG@10（自报，检索任务） | 是 | 可以 | 可以（2GB 起） | 走 REST | kalm-jev-…md |
| **SemIf** | TheoLeeCJ | 任意 LLM（示例 4B / 27B） | CUDA / MLX / llama.cpp / WebGPU | MIT | 4B 0.813，27B 0.958（实测，144 条人工标注，英文为主） | 是 | 可以 | 可以（GGUF） | 部分可以 | semif-…md |
| **Laya / Laya-ANE** | Convai + 社区 ANE 转换 | 421M | ANE（CoreML）/ MLX | Apache-2.0（转换件） | ANE 78ms vs MLX 97ms（实测，M4 24GB）；**跨项目对比中质量分仅 6.04，被标注「质量低」** | 是 | 未测 | 未测 | 否（CoreML） | laya-ane-…md，cloudflare-clef-…md |
| **Cloudflare Clef / Clef-flash** | Cloudflare | 27.36B / 9.41B | PyTorch / MLX | Apache-2.0 | 自报 DI 61.21 / 57.07；社区 MLX 抽样复测约 57.0 / 54.7，领先几乎消失 | 是 | flash 的 4bit 版约 5.3GB 可以 | 否 | 否 | cloudflare-clef-…md |
| **Bespoke Nimble-9B** | Bespoke Labs | 9B LoRA | MLX / CUDA | 没有标准 OSI 许可证 | 90.1%，Jev 93.2%（自报，324 条） | 是 | 否 | 否 | 走 HTTP | bespoke-nimble-…md |
| **StartLux Decision** | StartLuxLabs | 0.8B–27B，以及 35B MoE | GGUF + llama.cpp | Apache-2.0 | 27B DI 63.88（自报，未上公榜） | 是 | 小尺寸可以 | 0.8B / 2B 可以 | 部分可以 | startlux-…md |
| **Ollaya**（运行时） | ollaya-dev | ONNX 图约 3MB（权重另下） | ONNX Runtime | Apache-2.0；**子模型许可证要逐个核** | Laya 本地约 8ms，Jev 云端 236–276ms（实测） | 是 | 可以 | 可以 | **可以：ONNX 能用 `ort` 直接加载** | ollaya-…md |
| mu（框架） | Qybaihe | 不是模型 | TypeScript | MIT | 不适用 | — | — | — | — | mu-…md |

## 对 16GB / 8GB 机器的三条路线

1. **小底座句向量 + 分类头**：JevEmbed 路线，本质上就是小底座的 SetFit。和我们已测的最佳方案同构，能导出 ONNX，与 Rust 的 `ort` 集成最顺。
2. **零训练、读 logit**：LLM2Jev 路线。直接用本地已有的小 LLM（如 Qwen3-0.6B / 1.7B）只做一次前向，读出「是 / 否」的概率。适合兜底，也适合 8GB 机器。
3. **现成小决策模型**：KaLM-Jev Nano、AgentJev-0.6B。**许可证都没声明**，只能作参考，不能进生产。

明确**不推荐**的：
- **Laya-ANE**：质量低，而且 CoreML 无法接入 Rust；
- **Nimble-9B 和 Clef 27B**：16GB 跑不动，领先成绩也没有被复现。

## 要警惕的点

1. **自报成绩在复测中大幅缩水**：如 Clef、StartLux、Nimble。
2. **许可证不明是常态**：KaLM-Jev、AgentJev、Nimble、JevEmbed 都是。
3. **项目都极新**：LLM2Jev 3 天、KaLM-Jev 刚开源、StartLux 51 star、Ollaya 不到 3 周。
4. **没有中文证据**：这是整个生态的空白。
5. **生态叙事要打折**：「一天冒出 800+ 个集成」的说法，实际是有人聚合了 32 份已有清单。805 条里有 172 条没有许可证，26 条连 Jev API 都没调用（jev-awesome-list-gold-rush-fact-check.md）。
6. **Jev 本身也不是 SOTA**：它自己披露的工作流准确率落后对手（67.8% vs 74.1%）。
7. **Kev 的坦诚披露值得作为评估诚信的参照**：它写明了分布外准确率落后 6–7 个百分点、对选项顺序敏感 7.4%、逻辑规则推理的 3 个 seed 里只有 1 个达标。
