# 05 横评结果（D0-5 / D0-6，2026-10-07）

> 本文的数字**照抄**以下结果文件，不做重新计算：
> - `eval/decide/bench/results/m1max-64g/2026-10-07.md`（笔记本，去重前，全部 7 个候选；PR #718）
> - `eval/decide/bench/results/m1max-64g/2026-10-07-dedup.md` 与 `.json`（笔记本，去重后，仅 setfit-bge-m3；PR #722）
> - `eval/decide/bench/results/m4-16g/2026-10-07.md`（Mac mini，去重后训练集，SetFit 未跑完；PR #722）
> - 两机对照与解读：`docs/research/DECISION-MODELS.md` §9
>
> 全部是 **2026-10-07 单次运行**。代价加权权重 high=5 / medium=2 / low=1。

## 1. 机器

| 结果目录 | 机器 | Python |
|---|---|---|
| `m1max-64g` | Apple M1 Max，64GB | 3.11.15 |
| `m4-16g` | Apple M4，16GB（Mac mini；原先假设是 24GB，`sysctl hw.memsize` 实测为 16GB，见 `DECISION-MODELS.md` §9.0） | 3.11.17 |

## 2. 候选

| 名字 | 模型 | 方式 | 适用点 |
|---|---|---|---|
| rule | `retain.rs::explicit_remember` 的 Python 移植 | 规则 | 仅 retain_intent |
| gliclass-multilang | `knowledgator/gliclass-multilang-mini` | 零样本 | 三点 |
| mdeberta-xnli-control | `MoritzLaurer/mDeBERTa-v3-base-mnli-xnli` | 零样本 NLI，**仅对照**（训练数据 XNLI 为 CC-BY-NC-4.0，已剔出生产候选） | 三点 |
| erlangshen-nli | `IDEA-CCNL/Erlangshen-Roberta-110M-NLI` | 零样本 NLI | 三点 |
| setfit-bge-m3 | `BAAI/bge-m3` + SetFit 分类头 | 少样本训练 | 三点 |
| qwen3guard-0.6b | `Qwen/Qwen3Guard-Gen-0.6B` | 安全分类，三档映射到 low/medium/must_review，**永远不会输出 `high`** | 仅 tool_risk |
| kev-0.8b | `jaredpalmer/kev-0.8b` | 经其 `/v1/systemone` 服务（MLX）；**英文模型** | 三点 |

出处：`eval/decide/bench/README.md`「候选」表。

## 3. 笔记本 m1max-64g（去重前，全部候选）

出处：`eval/decide/bench/results/m1max-64g/2026-10-07.md`。

### retain_intent（n=101）

| 候选 | 准确率 | 宏F1 | ECE | 代价加权误判率 | P50(ms) | P95(ms) | 误写入率 | 记住召回 |
|---|---|---|---|---|---|---|---|---|
| erlangshen-nli | 0.089 | 0.027 | 0.834 | 0.931 | 352.9 | 546.3 | 0.000 | 0.000 |
| gliclass-multilang | 0.416 | 0.227 | 0.165 | 0.584 | 1946.9 | 5464.1 | 0.809 | 1.000 |
| kev-0.8b | 0.564 | 0.532 | 0.163 | 0.448 | 69.5 | 94.1 | 0.485 | 0.939 |
| mdeberta-xnli-control | 0.188 | 0.153 | 0.403 | 0.821 | 240.5 | 353.9 | 0.000 | 0.182 |
| rule | 0.317 | 0.152 | — | 0.734 | 0.0 | 0.0 | 0.118 | 0.545 |
| setfit-bge-m3 | 0.762 | 0.780 | 0.254 | 0.234 | 31.4 | 189.2 | 0.162 | 0.788 |

### recall_gate（n=100）

| 候选 | 准确率 | 宏F1 | ECE | 代价加权误判率 | P50(ms) | P95(ms) |
|---|---|---|---|---|---|---|
| erlangshen-nli | 0.430 | 0.301 | 0.445 | 0.497 | 62.2 | 106.8 |
| gliclass-multilang | 0.460 | 0.438 | 0.422 | 0.557 | 1323.7 | 1741.8 |
| kev-0.8b | 0.520 | 0.491 | 0.053 | 0.448 | 55.8 | 83.3 |
| mdeberta-xnli-control | 0.650 | 0.551 | 0.180 | 0.437 | 81.7 | 143.2 |
| setfit-bge-m3 | 0.870 | 0.869 | 0.301 | 0.153 | 28.2 | 41.0 |

### tool_risk（n=64）

| 候选 | 准确率 | 宏F1 | ECE | 代价加权误判率 | P50(ms) | P95(ms) |
|---|---|---|---|---|---|---|
| erlangshen-nli | 0.328 | 0.237 | 0.081 | 0.581 | 125.9 | 301.4 |
| gliclass-multilang | 0.203 | 0.084 | 0.158 | 0.943 | 1774.2 | 2191.4 |
| kev-0.8b | 0.406 | 0.364 | 0.109 | 0.629 | 71.8 | 88.8 |
| mdeberta-xnli-control | 0.297 | 0.192 | 0.192 | 0.729 | 177.6 | 286.9 |
| qwen3guard-0.6b | 0.297 | 0.248 | — | 0.838 | 293.6 | 436.5 |
| setfit-bge-m3 | 0.469 | 0.441 | 0.107 | 0.515 | 32.2 | 63.7 |

### 资源与下载

| 候选 | revision | 加载(s) | 峰值RSS增量(MB) | 下载体积(MB) |
|---|---|---|---|---|
| erlangshen-nli | 864d25be… | 13.2 | 557.2 | 780.5 |
| gliclass-multilang | 0bd888b6… | 13.1 | 1003.6 | 560.9 |
| kev-0.8b | 9a45d25e（服务端） | 0.7 | 13.9 | 62.5 |
| mdeberta-xnli-control | 8adb042d… | 8.6 | 1150.9 | 551.5 |
| qwen3guard-0.6b | fada3b2f… | 8.0 | 1178.9 | 1448.8 |
| rule | retain.rs@c503bf1 | 0.0 | 0.1 | 0.0 |
| setfit-bge-m3 | 5617a9f6… | 197.2 | 4217.3 | 4352.9 |

完整 40 位 revision 见原结果文件。注意：kev-0.8b 的 13.9MB 只是 bench 客户端自己的内存，**不含** `kev.serve` 服务进程（`DECISION-MODELS.md` §9.4）；setfit-bge-m3 的加载耗时包含在加载时训练三个头（PR #718 body）。

## 4. 去重前后：SetFit 分数变化

出处：`eval/decide/bench/results/m1max-64g/2026-10-07-dedup.md`（P50/P95 来自同名 `.json`）。去重只改训练集 17 条，评测集不动（过程见 `04-evaluation-design.md` §5）。

| 决策点 | 指标 | 去重前 | 去重后 | 变化 |
|---|---|---|---|---|
| retain_intent | 准确率 | 0.762 | 0.762 | 0 |
| retain_intent | 宏F1 | 0.780 | 0.772 | −0.008 |
| retain_intent | ECE | 0.254 | 0.259 | +0.005 |
| retain_intent | 代价加权误判率 | 0.234 | 0.234 | 0 |
| retain_intent | 误写入率 | 0.162 | 0.162 | 0 |
| retain_intent | 记住召回 | 0.788 | 0.788 | 0 |
| recall_gate | 准确率 | 0.870 | 0.850 | −0.020 |
| recall_gate | 宏F1 | 0.869 | 0.850 | −0.019 |
| recall_gate | ECE | 0.301 | 0.226 | −0.075 |
| recall_gate | 代价加权误判率 | 0.153 | 0.169 | +0.016 |
| tool_risk | 准确率 | 0.469 | 0.469 | 0 |
| tool_risk | 宏F1 | 0.441 | 0.424 | −0.017 |
| tool_risk | ECE | 0.107 | 0.154 | +0.047 |
| tool_risk | 代价加权误判率 | 0.515 | 0.507 | −0.008 |
| — | 加载耗时(s) | 197.2 | 292.1 | +94.9（结果文件注明：本机当时负载波动） |
| — | 峰值RSS增量(MB) | 4217.3 | 4269.5 | +52.2 |

去重后 P95 延迟（`.json`）：retain_intent 84.7ms、recall_gate 62.7ms、tool_risk 90.5ms。

结果文件的解读：准确率基本不变（最大降 2 个点）；ECE 变化方向不一致，样本小、10 桶噪声大，不应读出「去重让校准变好/变差」；**去重前的分数不是泄漏撑起来的**，D1 选型引用去重后数字即可。

## 5. Mac mini m4-16g

出处：`eval/decide/bench/results/m4-16g/2026-10-07.md`。

### retain_intent（n=101）

| 候选 | 准确率 | 宏F1 | ECE | 代价加权误判率 | P50(ms) | P95(ms) | 误写入率 | 记住召回 |
|---|---|---|---|---|---|---|---|---|
| erlangshen-nli | 0.089 | 0.027 | 0.834 | 0.931 | 56.0 | 61.8 | 0.000 | 0.000 |
| gliclass-multilang | 0.416 | 0.227 | 0.165 | 0.584 | 315.1 | 334.2 | 0.809 | 1.000 |
| kev-0.8b | 0.564 | 0.532 | 0.164 | 0.448 | 64.7 | 71.3 | 0.485 | 0.939 |
| mdeberta-xnli-control | 0.198 | 0.160 | 0.404 | 0.815 | 79.4 | 90.5 | 0.000 | 0.212 |
| rule | 0.317 | 0.152 | — | 0.734 | 0.0 | 0.0 | 0.118 | 0.545 |

### recall_gate（n=100）

| 候选 | 准确率 | 宏F1 | ECE | 代价加权误判率 | P50(ms) | P95(ms) |
|---|---|---|---|---|---|---|
| erlangshen-nli | 0.430 | 0.301 | 0.445 | 0.497 | 19.6 | 22.8 |
| gliclass-multilang | 0.460 | 0.438 | 0.422 | 0.557 | 228.8 | 247.7 |
| kev-0.8b | 0.510 | 0.477 | 0.072 | 0.454 | 40.5 | 41.8 |
| mdeberta-xnli-control | 0.650 | 0.551 | 0.179 | 0.437 | 28.0 | 36.7 |

### tool_risk（n=64）

| 候选 | 准确率 | 宏F1 | ECE | 代价加权误判率 | P50(ms) | P95(ms) |
|---|---|---|---|---|---|---|
| erlangshen-nli | 0.328 | 0.237 | 0.081 | 0.581 | 41.7 | 84.4 |
| gliclass-multilang | 0.203 | 0.084 | 0.158 | 0.943 | 347.3 | 438.0 |
| kev-0.8b | 0.406 | 0.358 | 0.109 | 0.633 | 73.4 | 78.5 |
| mdeberta-xnli-control | 0.312 | 0.203 | 0.175 | 0.707 | 63.2 | 195.4 |
| qwen3guard-0.6b | 0.297 | 0.253 | — | 0.838 | 350.6 | 412.2 |

### 资源与下载

| 候选 | 加载(s) | 峰值RSS增量(MB) | 下载体积(MB) |
|---|---|---|---|
| erlangshen-nli | 71.0 | 891.8 | 390.3 |
| gliclass-multilang | 43.2 | 1029.5 | 560.9 |
| kev-0.8b | 1.4 | 14.0 | 62.5 |
| mdeberta-xnli-control | 50.1 | 1127.5 | 551.5 |
| qwen3guard-0.6b | 70.6 | 2338.9 | 1448.8 |
| rule | 0.0 | 0.1 | 0.0 |

**Unavailable：setfit-bge-m3 —— `subprocess timed out after 1800.0s`。**

## 6. 为什么 16GB 上 SetFit 不可用

出处：`DECISION-MODELS.md` §9.2、§9.6；PR #722。

- `uv run bench --all` 跑到 setfit-bge-m3（列表最后一个）时，30 分钟子进程超时，被判 `UNAVAILABLE`。
- 超时期间采样：`sysctl vm.swapusage` 显示 swap 已用 5181.19M（6GB swap 几乎用满）；`vm_stat` 的 `Pages free` 只有 4006 页（约 64MB）。
- 对照：同一候选在 64GB 笔记本上去重后 292s 跑完，峰值 RSS 增量 4269.5MB。
- 结论原文：「不是稍微慢一点」，是**重度 swap thrashing**；16GB 下 SetFit(bge-m3)「实质上不可用，不是能用但慢」。
- 编排正确：超时后子进程被回收，没有孤儿进程，`bench --all` 继续跑完其余候选——「单个候选失败不终止全局」在硬超时这种最坏情况下也成立。
- 注意范围：这次测的是「加载时训练 + 推理」（bench 的 SetFit 候选在 `load()` 里训练三个头，见 `setfit_bgem3.py`）。训练和推理分离之后，16GB 上**只做推理**是否可行，这次没有单独测（**推断**：推理内存应显著低于训练，但需实测，见 `06-training-and-deployment.md`）。
- **24GB 是否可行仍是未知数**：`DECISION-MODELS.md` §9.2 明确说不能用 16GB 的结果倒推 24GB，D1 定档前要在真 24GB 机器上复测。

## 7. 其他现象（未定论）

出处：`DECISION-MODELS.md` §9.4，均标「未验证成因」。

- GLiClass 在 M1 Max 上比 M4 慢 6–16 倍（P95：5464 vs 334ms 等）。可能原因：`gliclass` 库默认尝试 MPS，小 batch 下同步开销大；或者顺序跑时的热节流 / 前序候选留下的系统压力。都未验证。
- qwen3guard-0.6b 的峰值 RSS 增量在 16GB 机器上是 64GB 机器的两倍（1178.9 → 2338.9MB）。更可能是内存压力下分配器行为影响了一次性 delta 读数，建议改成周期采样取 max。
- PR #718 记录了一次真实的瞬时失败：SetFit 第一次与其他候选并跑、本机 load average 一度 57–79 时，训练 tool_risk 头报 `Input X contains NaN`；单独重跑成功，未再复现，原文记在 json 的 `notes` 字段。
- `erlangshen-nli` / `mdeberta-xnli-control` 共用一个中文 hypothesis 模板（`这句话属于：{}`），没有分别调模板；erlangshen 全线垫底可能部分来自这一点（结果 md 文件头的注）。
- PR #718 body 观察：英文模型 kev-0.8b 在中文三项上都排进前二；gliclass 在 retain_intent 上召回 1.000 但误写入 0.809，几乎什么都判成 `remember`（argmax 零样本在类别语义相近时的典型失败）。

## 8. 结论

1. **SetFit(bge-m3) 在三个决策点上都领先**：去重后 0.762 / 0.850 / 0.469，对次优候选 0.564（kev-0.8b）/ 0.650（mdeberta-xnli-control，仅对照）/ 0.406（kev-0.8b）。
2. **零样本候选在中文意图上普遍不可用**：retain_intent 上 GLiClass 0.416、mDeBERTa 0.188、Erlangshen 0.089，后两者还不如规则（0.317）。
3. **规则的强项是误写入低**（0.118），SetFit 去重后误写入 0.162，**仍高于规则**——按 D1 验收「误写入率 ≤ 规则版」，单独用 SetFit 达不到；这正是「规则兜底 + 模型 + 阈值弃权带」要解决的问题（**推断**：需要在 D1 用阈值把低置信的「记住」转为反问，再测一次误写入率）。
4. **tool_risk 全员不及格**：最高 0.469（SetFit），`DECISION-MODELS.md` §9.5 提到零样本候选在这一点上普遍低于或接近 4 类随机基线 0.25。危险操作不能交给这些模型，`always_review` 硬清单必须保留。
5. **16GB 机器上，训练 SetFit 不可行**；分档建议见 `DECISION-MODELS.md` §9.5（全部标「待 jason 拍板」）与 `06-training-and-deployment.md`。

## 9. 局限（写文章时必须交代）

- **样本量小**：64–101 条/份，单类最少 9 条（retain_intent 的 `correct`）。几个百分点的差距不显著。
- **单次运行**：没有多次重复、没有置信区间、没有多个随机种子；SetFit 训练只用 `num_epochs=1, batch_size=16, num_iterations=5`（`setfit_bgem3.py`）。
- **评测集与训练集由同一方编写**：即便去了近似重复，分布仍可能比真实用户输入更接近（**推断**）。
- **泰文暂缺**：jason 2026-10-07 要求中英泰三语，三语评测集在 PR #726（截至本文 OPEN）；本文所有数字都是中文为主 + 约 15% 英文。
- **同系列模型未测**：jason 要求 8/16/24/32/64GB+ 用同一系列、尺寸连贯的模型，本轮候选是不同家族，D0-8 同系列补测尚未产出结果。
- **24GB 档未测**；kev 服务进程内存未测；GLiClass 延迟、qwen3guard 内存两处异常未查明。
- **ECE 偏高**：SetFit 去重后 ECE 为 0.259 / 0.226 / 0.154（三个点），说明它报出的概率与实际准确率有明显偏差（对照：Jev 自报 intent routing ECE 0.096，二级来源，见 EVIDENCE §1），直接用于三段阈值前需要校准（**推断**：如温度缩放，本轮未做）。
