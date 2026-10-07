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
