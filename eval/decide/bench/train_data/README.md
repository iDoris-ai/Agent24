# SetFit(bge-m3) 训练集（仅供 D0-5 少样本训练，不是评测集）

与 `../../../eval/decide/*.jsonl`（三份评测集）**严格分开**：

- 这里的例句全部手写，没有一条取自 `eval/decide/` 的 226/221/144 条评测样本（不同措辞、不同人名/过敏原/工具名，也没有逐条改写）。`uv run bench --check-overlap`（字符 3-gram Jaccard 与 SequenceMatcher，阈值 0.7）必须 0 命中；有命中时改写这里的样本，不动评测集。
- 评测脚本（`decide_bench/adapters/setfit_bgem3.py`）只用这三份文件 fit 一个 SetFit 分类头，评测永远跑在 `eval/decide/*.jsonl` 上；两边互不混用。
- `tool_risk_train.jsonl` 的 `text` 字段用的是跟推理时同一套渲染规则（`decide_bench.render.render_tool_risk_text`），不是原始 `{tool,args}` 字典，因为 SetFit 只能喂文本。
- 每类 8–15 条，覆盖主要场景但不追求穷尽——SetFit 本来就是少样本方法，训练集大小本身也是报告要交代的一项。
- 每行带 `lang`（`zh` / `en` / `th`），**每类每语言至少 8 条**；分布见 `../../README.md`「语言 × 标签分布」（`python3 eval/decide/lang_stats.py` 生成）。
- 英/泰的 `tool_risk` 训练样本沿用中文样本的做法，用**专门的工具名**（`pkg_install`、`cache_clear`、`root_exec` 等）而不是通用的 `shell_exec`：评测集的英/泰条目大多是 `shell_exec`，共用「工具调用：shell_exec，参数：…；上下文：User request: …」前缀会让短样本的 SequenceMatcher 超过 0.7。
- 泰文样本由模型撰写，待母语者复核，清单见 `../../TH_REVIEW.md`。
