# 06 训练与部署：SetFit、按需下载、硬件分档、Rust 集成

> 出处：`docs/agent/PLAN-DECIDE.md` §0、§1、§2.4；`docs/research/DECISION-MODELS.md` §8、§9.5；`DECISION-MODELS-EVIDENCE-2026-10-06.md` §5、§7；`rust/crates/agent24-decide/src/{hw,tier,catalog}.rs`；`eval/decide/bench/src/decide_bench/adapters/setfit_bgem3.py`；PR #689、#722。外部来源单独注明。

## 1. SetFit 是什么

- **出品**：Hugging Face 与 Intel Labs、UKP Lab 合作推出。Hugging Face 官方博客原文：「Together with our research partners at Intel Labs and the UKP Lab, Hugging Face is excited to introduce SetFit」（https://huggingface.co/blog/setfit ，本目录作者 2026-10-07 抓取核对）。论文：Tunstall 等，*Efficient Few-Shot Learning Without Prompts*，arXiv 2209.11055。仓库内 EVIDENCE §5 只写了「huggingface + Intel Labs 联合出品」，UKP 来自上述博客。
- **方法**：两阶段，不需要 prompt / verbalizer——①用少量文本对做对比学习，微调 sentence-transformer；②在其 embedding 上拟合逻辑回归分类头（EVIDENCE §5）。博客称每类 8 个标注样本即可在若干数据集上取得有竞争力的结果（同上博客）。
- **许可证**：`huggingface/setfit` 代码 Apache-2.0（`DECISION-MODELS.md` §8.1，LICENSE 原文已核实）。
- **底座 bge-m3**：`BAAI/bge-m3`，来自 BAAI（北京智源人工智能研究院），MIT（`DECISION-MODELS.md` §8.1，以 HF YAML 为准，仓库无 LICENSE 文件）；569M 参数、约 2.27GB、支持 100+ 语言含中文、最长 8192 token（EVIDENCE §5，HF 模型卡聚合）。
- **本项目的用法**：每个决策点单独训练一个头；训练参数 `num_epochs=1, batch_size=16, num_iterations=5`（`setfit_bgem3.py`）；训练集 60 / 30 / 30 条手写样本（见 `04-evaluation-design.md` §6）。

为什么是它赢（结合 `05-results.md`）：三个决策点上去重后都领先。**推断**：零样本 NLI / GLiClass 依赖「标签名的语义」，而「记住 / 询问记忆 / 更正 / 偏好」在字面上高度相近（都含「记」），少样本训练能直接学到决策边界。这一解释没有做消融验证。

## 2. 训练与推理分离

- PLAN-DECIDE §2.4：个人模型的训练在 T3（或用户自愿的 T2）机器上做，「SetFit 头 / encoder LoRA（MLX），不动 base」。
- `00-timeline-and-decisions.md` 教训 6：**用户机器只做推理**，训练在开发机上做一次，再分发产物。
- 证据：Mac mini 16GB 上「加载时训练」的 SetFit 30 分钟没跑完、swap 几乎用满（`05-results.md` §6）。横评脚本把训练放在 `load()` 里是为了评测方便，不是产品形态。
- **尚未做的事**：把训练好的 SetFit 产物（微调后的 bge-m3 + 分类头）导出、在 16GB 机器上**只测推理**的内存与延迟。这一项决定 SetFit 能否下放到 T1/T2，目前没有数据。

## 3. 按需下载组件：pin(40hex) + sha256

- 原则（PLAN-DECIDE §0）：模型与运行时是**按需下载组件**（pin 版本 + sha256，同 Open Design 组件机制），**不进安装包**。
- 模型目录 `rust/crates/agent24-decide/decide-models.catalog.json`：每条写明 HF 仓库、固定 revision、sha256、许可证、下载体积、常驻内存、运行时、适用决策点、适用档位、实测数据（`catalog.rs` 的 `CatalogEntry`）。**目录目前刻意为空**：延迟和内存数字「只能来自 D0 实测，不抄模型卡」（`catalog.rs` 模块文档、PLAN-DECIDE §1.1）。
- 校验（`catalog.rs::ModelCatalog::load_str`）：
  - `revision` 必须是 **40 位小写 hex** 的 commit SHA——分支名（`main`）、tag、短 SHA、大写 hex 一律拒绝；
  - `sha256` 必须是 **64 位小写 hex**；
  - 任一条不合格，**整个目录加载失败**，不会「大部分 pin 了、悄悄跳过一条」。
  - 这条校验是 PR #689 评审抓出来的：原实现只查非空，`revision: "main"`、`sha256: "deadbeef"` 都能过（见 `08-code-review-findings.md` 第 4 条）。
- 为什么要 pin：Ollaya 的做法一样——它只发约 3MB 的 ONNX 图，真实权重从原作者 HF 仓库按 commit 锁定并 sha256 校验（EVIDENCE §4）。
- 下载需要用户同意：`DownloadConsent` 默认未同意（`A24_DECIDE_DOWNLOAD_CONSENT` 环境变量），未同意时稳定落 T0（`tier.rs`）。该变量曾漏进 LaunchAgent 透传清单，被 CI 测试抓到（`08-code-review-findings.md` 第 5 条）。
- 与 jason 的「可选组件按需下载」偏好一致：大组件不进安装包，点菜单时提示下载 + 校验后加载（PLAN-DECIDE §0 引用 Open Design 组件机制）。
- 许可证要进组件清单：`DECISION-MODELS.md` §8.3「接入任何模型时 pin HF commit + sha256，并在组件清单里带上许可证字段」。

## 4. 硬件分档

### 4.1 现行代码里的 T0–T3（草案）

`tier.rs` 常量与 PLAN-DECIDE §1.1：

| 档位 | 代码判定 | PLAN 草案组合（快速层 / 深度层） |
|---|---|---|
| **T0 仅规则** | 未同意下载；或可用磁盘 < 2GB；或总内存 < 8GB | 规则 / 无 |
| **T1 轻量** | 8GB ≤ 内存 < 16GB；或无加速器；或确认在电池供电（均封顶 T1） | int8 小 encoder（CPU）/ 无 |
| **T2 标准** | 16GB ≤ 内存 < 32GB，且有加速器 | encoder + Qwen3Guard-0.6B / Kev-0.8B 类（oMLX/Metal，按需） |
| **T3 充裕** | 内存 ≥ 32GB，且有加速器 | 同 T2 / Kev-4B 类；具备本地微调能力 |

规则细节（`tier.rs`）：
- `TierPolicy::decide` 是纯函数（无 I/O、无时钟），边界值都有单测（如 `mem_exactly_16gb_with_accelerator_is_t2`、`no_accelerator_caps_at_t1_even_with_64gb`）。
- 用户覆盖**只能降档，不能升档**；硬件变化时重新推荐，但不自动升档下载（PLAN-DECIDE §1.1）。
- `on_battery == None`（探测不出来）不封顶；`SystemProbe` 目前永远报 `None`，因为 `sysinfo` 不提供电源状态——代码文档写明「Not implemented, not pretended to be」（`hw.rs`）。
- 每个决定附带人类可读的 `reasons[]`，供设置页显示「本机档位 + 为什么」。

### 4.2 D0-6 实测后的分档建议（待 jason 拍板）

出处：`DECISION-MODELS.md` §9.5，**原文标注「不是最终决定」，也不改 PLAN-DECIDE §1.1 的表**：

- T0：不变。
- T1（8–16GB）：零样本候选准确率偏低、SetFit 在 16GB 上跑不完 → **暂时维持「仅规则」**；kev-0.8b 明显优于零样本候选，但它是英文模型且服务进程内存未测。
- T2：16GB 机型上 SetFit 不可用 → 建议 **T2 内部按内存再切一刀**：16–20GB 维持 T1 组合；≥24GB 且复测确认 SetFit 能稳定跑完后，才用 SetFit 作深度层。
- T3（≥32GB）：SetFit(bge-m3) 作为三个决策点的统一模型；加载近 5 分钟，首次使用要在 UI 上提示等待。

### 4.3 jason 2026-10-07 的新要求：8 / 16 / 24 / 32 / 64GB+ 五档、同系列连贯

出处：`00-timeline-and-decisions.md`「jason 2026-10-07 追加的要求」。

- 为 **8 / 16 / 24 / 32 / 64GB+** 提供不同规格，**模型要成系列、尺寸连贯**；
- 最低支持**中文、英文、泰文**；
- 许可证给结论；最好有开源基础，不强求从头训练；
- 由此派出 D0-8：三语评测集（`ab/decide-01`，PR #726）与同系列模型补测（`ab/decide-02`，截至本文未见 PR）。

与现行代码的差距（本目录作者对照得出）：`tier.rs` 只有四档且以 16/32GB 为界，没有 24GB 与 64GB 两个边界；本轮横评的候选来自不同家族（BGE、DeBERTa、RoBERTa、Qwen），不构成「同系列」。生态里能提供连贯尺寸的候选，见 `02-ecosystem-survey.md`（如 Kev 0.5B/0.6B/4B/8B——注意 EVIDENCE §3 依 Kev README 记作 0.8B/4B/9B/27B，两处尺寸不一致，待核；StartLux 0.8B–27B、JevEmbed 可换 Qwen3-Embedding-0.6B/4B/8B 底座）——**这些在我们的中文评测集上都还没测**。

## 5. Rust 集成路线：ONNX / `ort` 及其他

现状：横评全部在 Python 里跑；Rust 侧只有契约，没有任何模型推理代码（`backend.rs`：只有 `RuleBackend`）。

已有材料中的路线：

| 来源 | 说法 |
|---|---|
| `DECISION-MODELS.md` §7.3 | 快速模型层「进程内，candle/ort 跑小 encoder」；深度层经 oMLX/Ollaya |
| `catalog.rs` 的 `Runtime` 枚举 | `Ort` / `Omlx` / `Ollaya` 三种 |
| EVIDENCE §7 | `ort`（ONNX Runtime 绑定）成熟，支持 CoreML EP，但要打包较大的 ONNX Runtime 动态库；`candle` 纯 Rust、原生 Metal；`tract` 纯 Rust、体积最小。该文结论：**tract 或 candle > ort > llama.cpp 子进程 > MLX 子进程**；凡是要调 MLX 的路线都意味着在 Rust daemon 里管一个 Python 子进程，与 sub-100ms 目标冲突 |
| `02-ecosystem-survey.md` | Ollaya 走 ONNX Runtime，「ONNX 能用 `ort` 直接加载」；JevEmbed 路线「能导出 ONNX，与 Rust 的 `ort` 集成最顺」 |

两处材料对 `ort` 与 `tract/candle` 的排序不一致：EVIDENCE §7 从体积与冷启动角度更偏纯 Rust 运行时，生态调研从「现成 ONNX 图可直接复用」角度偏 `ort`。**最终选哪个没有拍板**，也没有做过 Rust 内的实测。

**推断（未验证）的落地步骤**：①开发机上训练 SetFit，导出 sentence-transformer 为 ONNX + 逻辑回归头权重；②发布为按需下载组件（pin + sha256 + 许可证）；③Rust 侧实现一个 `Encoder` 类 `DecisionBackend`，用选定运行时加载；④在 T1/T2 机器上实测推理内存与 P95，回填 `decide-models.catalog.json` 的 `measured` 字段。D1 验收要求 CPU 上 P95 < 100ms（PLAN-DECIDE D1）。

## 6. 许可证结论（摘要）

出处：`DECISION-MODELS.md` §8.1（2026-10-07 逐个读 HF YAML / LICENSE 原文）。

- 所有候选的**权重**许可都是 Apache-2.0 或 MIT。
- 唯一实质风险在**训练数据**：两个 mDeBERTa-xnli 模型用了 CC-BY-NC-4.0 的 XNLI（2mil7 另含 ANLI），按保守原则剔出生产候选，只留作对照。
- 待补核：Erlangshen 的中文 NLI 训练数据（CMNLI 一般被描述为由 MNLI/XNLI 翻译而来——**未核实**）、GLiClass 两个训练集、Kev-4B 数据源、laya teacher 模型（§8.2）。
- SetFit + bge-m3 这条胜出路线：代码 Apache-2.0、底座 MIT；训练数据是我们自己手写的，没有第三方数据许可问题（本目录作者据 `train_data/README.md`「全部手写」得出）。
