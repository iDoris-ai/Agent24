# D0-5 横评脚本

对 [`docs/agent/PLAN-DECIDE.md`](../../../docs/agent/PLAN-DECIDE.md) D0-5：在
[`../`](../) 三份中文评测集（`retain_intent` / `recall_gate` / `tool_risk`，D0-4）
上横评候选决策模型，产出准确率、宏 F1、ECE、按 `cost_level` 加权的误判率、
P50/P95 延迟、峰值 RSS 增量、下载体积；`retain_intent` 额外给误写入率与记住召回。

## 安装

```bash
cd eval/decide/bench
uv sync
```

Apple Silicon 可选装 `mlx` extra（目前仅 `kev-0.8b` 的服务端用到，见下）：

```bash
uv sync --extra mlx
```

## 跑全部

```bash
uv run bench --all --machine m1max-64g
```

- `--candidates rule,gliclass-multilang` 只跑指定候选（逗号分隔，名字见 `decide_bench.adapters.build_registry`）。
- `--sets retain_intent,recall_gate` 只跑指定评测集。
- 不指定 `--machine` 时用 `decide_bench.hardware.default_machine_id()` 猜一个（芯片名+内存），建议显式传。

结果写到 `results/<machine-id>/<today>.json`（含逐条预测，供复核）与同名
`.md`（汇总表）。单个候选加载失败（缺依赖、下载失败、显存/内存不足等）记为
`unavailable` 并继续跑其余候选，不终止整轮。

## 候选

| name | 模型 | 适用点 | 说明 |
|---|---|---|---|
| `rule` | `retain.rs::explicit_remember` 的 Python 移植 | `retain_intent` | 地板基线；移植版先过 `retain.rs` 自带 30 条测试表（`tests/test_rule_port.py`），再拿来跑评测集 |
| `gliclass-multilang` | `knowledgator/gliclass-multilang-mini` | 全部三个 | DECISION-MODELS.md §8.1 生产候选，零样本多标签分类 |
| `mdeberta-xnli-control` | `MoritzLaurer/mDeBERTa-v3-base-mnli-xnli` | 全部三个 | **仅对照**，§8.1 因训练数据 XNLI 为 CC-BY-NC-4.0 已剔出生产候选 |
| `erlangshen-nli` | `IDEA-CCNL/Erlangshen-Roberta-110M-NLI` | 全部三个 | 中文 NLI 零样本分类 |
| `setfit-bge-m3` | `BAAI/bge-m3` + SetFit 分类头 | 全部三个 | 少样本训练，训练集见 `train_data/`（手写，与评测集严格不重叠，`setfit_bgem3.py` 的 `_load_train_rows` 启动时会断言不重叠） |
| `qwen3guard-0.6b` | `Qwen/Qwen3Guard-Gen-0.6B` | 仅 `tool_risk` | 内容安全分类器，三档（Safe/Controversial/Unsafe）映射到 low/medium/must_review；因此**永远不会输出 `high`**（映射本身的已知天花板，见 `qwen3guard.py` 顶部注释）。`recall_gate`/`retain_intent` 判定标 not-applicable，不是 unavailable |
| `kev-0.8b` | `jaredpalmer/kev-0.8b`（经其自带 `/v1/systemone` 服务，Apple Silicon 上该服务内部走 MLX） | 全部三个 | **英文模型**（模型卡明示 "Languages: English"），我们的评测集是中文——结果预期是真实但偏低的分数，不是 bug。需要单独起它的服务，见下 |

## Kev-0.8B 需要单独起服务

本仓库不重新实现 Kev 的 pointer-head 推理，而是直接打它自己的 TypeSafe
兼容 API（`/v1/systemone`），这正是 PLAN-DECIDE 要求的 oMLX 路径（Kev 在
Apple Silicon 上通过这条服务命令自动走 MLX）：

```bash
git clone https://github.com/jaredpalmer/kev.git && cd kev
uv sync --extra serve   # 需要 Python >=3.12，与本 bench 的 3.11 环境分开
uv run --extra serve python -m kev.serve --run jaredpalmer/kev-0.8b --port 8008
```

默认 `KEV_BASE_URL=http://127.0.0.1:8008`；没有这个服务在跑，`kev-0.8b`
候选会在 `load()` 里探测失败并标记 `unavailable`（给出上面这段命令作为
reason），不会让整轮横评失败。

## 训练/评测分离（SetFit）

`train_data/*.jsonl` 是手写的少样本训练数据，`setfit_bgem3.py` 加载时用
文本精确匹配断言它与对应的 `eval/decide/*.jsonl` 没有交集；一旦有人不小
心把评测句子抄进训练集，启动就会 `AssertionError` 而不是悄悄把分数刷高。

### 近似重复检测（D0-6，补 PR #718 复审遗留项）

逐字精确匹配只挡得住原样复制。PR #718 复审（clestons）另外用
`difflib.SequenceMatcher` 跑了一遍训练集/评测集，发现有换模板词的近义
句混进了训练集（例如训练集「上海明天天气」对评测集「北京明天天气」，
相似度 0.94；三份评测集里命中率 2–6%）——这类重叠逐字检查查不出来，
会让 SetFit 的分数虚高。

`decide_bench/overlap.py` 补了这道检查，同时跑两个指标（详见模块
docstring 为什么两个都做，不是二选一）：

- 字符 3-gram Jaccard 相似度（零额外依赖、确定性）
- `difflib.SequenceMatcher.ratio()`（与复审方法一致，标准库自带）

两者任一 ≥ 阈值（默认 0.7，取自复审实测的量级）就算命中。跑法：

```bash
uv run bench --check-overlap                       # 阈值 0.7，跑三份评测集
uv run bench --check-overlap --overlap-threshold 0.6  # 调阈值
uv run bench --check-overlap --sets tool_risk       # 只查一个点
```

有命中时退出码为 1，并打印每一对 `(eval_id, train_text)` 及两个相似度分
数，方便定位。本仓库当前状态下三份 `train_data/*.jsonl` 对各自评测集在
阈值 0.7 下为 **0 命中**（2026-10-07 修过一批模板撞词的训练样本，见
`results/m1max-64g/2026-10-07-dedup.md`）；不是静默改数据——改哪些、为
什么改、改了以后分数怎么变，都在那份文档里。

`setfit_bgem3.py` 的 `load()` 在训练前也会跑一次这个检查：如果（将来）
又有命中，不会让候选失败，而是把命中清单塞进该候选结果的
`overlap_warning` 字段，`render.py` 会在横评报告里单独起一节列出来——
确认过某次结果干净，不代表以后改了训练集还干净，这道检查就是防止那种
情况悄悄发生。

## D0-8：同系列多尺寸补测（2026-10-07）

PLAN-DECIDE D0-8 要求按「同一系列、多尺寸」补测，覆盖中/英/泰三语，并给
每个候选许可证结论。完整结论见
[`../../../docs/research/DECISION-MODELS.md`](../../../docs/research/DECISION-MODELS.md)
§10；本节只记代码层面的东西。

### 按语言分指标

当前 `eval/decide/*.jsonl` 还没有显式 `lang` 字段（三语评测集在
`ab/decide-01`，尚未合入）。`decide_bench.lang.resolve_lang` 优先读
`item.lang`，没有就用 `infer_lang` 按规则推断（含泰文字符→th，含 CJK→
zh，否则 en）。`render.py` 在每个候选每个点下面加一张「按语言」子表，
只要本次报告里有任何一条是推断出来的，就在表前面加一行说明——不是悄悄
把推断结果当成真实标签用。`ab/decide-01` 合入后重跑即可自动改用显式
标签，不需要改这边的代码。

### 新增候选（`adapters/embed_head.py` / `causal_logit.py` / `kev.py`）

| name | 系列 | 尺寸 | 训练方式 |
|---|---|---|---|
| `qwen3-embed-0.6b` / `-4b` / `-8b` | Qwen3-Embedding | 0.6B/4B/8B | embedding + LogisticRegression（见下） |
| `e5-small` / `-base` / `-large` / `-large-instruct` | multilingual-e5 | small/base/large/large-instruct | 同上，带 `query: ` 前缀（模型卡要求） |
| `minilm-multilingual` | paraphrase-multilingual-MiniLM-L12-v2 | 118M | 同上，8GB 档极小对照 |
| `kalm-embed-v2.5-reference` | KaLM-Embedding v2.5 | mini-instruct | 同上，**仅对照，许可证未按 §10 核实到可商用结论** |
| `qwen3-llm-0.6b` / `-1.7b` / `-4b` / `-8b` | Qwen3（因果 LLM） | 0.6B–8B | 零训练，读首 token logit（见下），MLX |
| `kev-4b` | Kev | 4B | 同 `kev-0.8b`，经其自带 `/v1/systemone` 服务 |

**`adapters/embed_head.py` 不是 `setfit` 库**：它是 sentence-transformers
`.encode()` + `sklearn.LogisticRegression`，训练数据仍是
`train_data/*.jsonl`（通过新拆出的 `decide_bench/train_data.py`，
`setfit_bgem3.py` 也改成引用这一份，行为不变）。这是故意的简化——
`setfit` 库的对比学习训练器不是每个 backbone 都能直接套（e5 需要
query/passage 前缀、Qwen3 causal 底座的 `.encode()` 支持不一致），而这个
配方在全部新增 backbone 上都能跑。**跟 `setfit-bge-m3` 比较时要记得这一
点**：差异里既有 backbone 的差异，也有头训练方式的差异，不是纯粹的
backbone ablation。

**`adapters/causal_logit.py`**：对 Qwen3 系列因果 LLM，一次前向读「A/B/
C/…」几个候选字母 token 的 logit，softmax 限定在候选集合内（不是全词表
的 argmax token），零训练。优先 MLX（`mlx_lm`，见 `uv sync --extra mlx`）
——0.6B/8B 复用 `~/.omlx/models/` 里已下载好的 4bit 权重（不产生新下
载，报告里 `download_size_mb=0`），1.7B/4B 新下载
`mlx-community` 的预量化权重。不套 chat 模板，纯续写式 prompt——这些是
base 模型，套用某个模型专属的对话模板会让"零训练"的说法掺进针对单一
模型调过的 prompt 工程，削弱"同系列尺寸连贯"比较的可信度。

**`kev-4b`**：和 `kev-0.8b` 同一套适配器（`KevCandidate` 已泛化为接受
`model_id`/`revision`/端口），同样需要手动起服务，换个端口避免冲突：

```bash
git clone https://github.com/jaredpalmer/kev.git && cd kev
uv python pin 3.12 && uv sync --extra serve
uv run --extra serve python -m kev.serve --run jaredpalmer/kev-4b --port 8009
KEV_BASE_URL=http://127.0.0.1:8009 uv run bench --candidates kev-4b ...
```

### ONNX 导出（`scripts/export_onnx.py`）

把横评里胜出的小体积 embedding+头导出 ONNX（动态 int8 量化），用
onnxruntime 重新测推理 RSS/延迟——这是 Rust `ort` 集成路线的第一步。脚本
**不在本项目 `pyproject.toml` 的依赖里**：`optimum[onnxruntime]` 需要
`transformers<4.47`，与本项目基础依赖 `gliclass`（需要
`transformers>=5.0`）冲突，锁不到一个公共环境。脚本自己起一个临时环境：

```bash
uv run --isolated --with 'optimum[onnxruntime]>=1.20,<2.2' --with 'transformers<4.47' \
    --with torch --with onnx --with scikit-learn --with psutil \
    python scripts/export_onnx.py --model-id intfloat/multilingual-e5-base \
    --revision d128750597153bb5987e10b1c3493a34e5a4502a --query-prefix "query: " \
    --out-dir /tmp/onnx-e5-base
```

脚本输出 `onnx_export_report.json`，并在 `int8/` 保存分类头和逐条 Python
参考结果。模型、tokenizer 和头文件留在输出目录，不进仓库。

Rust D1-1 spike 默认不需要模型资产，集成测试会跳过。配置 `int8/` 路径后
对 221 条 `recall_gate` 样本逐条对照：标签一致率 ≥99%，最大类别概率绝对误差
≤0.02；标签分歧仅允许在双方概率都处于 0.5 ±0.02 的边界带。下列命令测量
Rust 单独进程 RSS 与 P50/P95：

```bash
export CARGO_TARGET_DIR=$HOME/Dev/auraai/Agent24/rust/target
export agent24_onnx_spike_dir=/tmp/onnx-e5-base/int8
cd rust
cargo test -p agent24-decide --features onnx-spike --test onnx_spike
cargo run -p agent24-decide --features onnx-spike --bin onnx_spike_bench
```
